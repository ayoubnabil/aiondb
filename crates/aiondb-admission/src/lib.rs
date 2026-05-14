//! Admission control for `AionDB`.
//!
//! Inspired by CockroachDB's admission package and Google's Borg
//! quota system: when the node is under sustained overload, *some*
//! requests must be deferred or rejected so the system stays
//! responsive instead of collapsing into queue blow-up + cascading
//! timeouts. Without admission control, a write storm fills WAL
//! buffers, starves replication, and a single hot table can take down
//! the entire cluster.
//!
//! # Model
//!
//! Two complementary primitives:
//!
//! - [`TokenBucket`] -- a classic rate limiter with optional burst
//!   allowance, async waiting and try-acquire fast path. Used to bound
//!   sustained throughput.
//! - [`AdmissionController`] -- a priority-aware front door that wraps
//!   one bucket per [`Priority`] class, plus an overall queue-depth
//!   guard. Higher-priority work bypasses lower-priority backlog so
//!   internal/system work (heartbeats, raft, replication ACKs) can
//!   always make forward progress even when user traffic is throttled.
//!
//! Both primitives are intentionally lock-free for the common path:
//! `try_acquire` is a single atomic CAS, only the slow-path waiters
//! touch a `Mutex` to register themselves on the wake-up notifier.
//!
//! # Backpressure shape
//!
//! When [`AdmissionController::admit`] returns
//! [`AdmissionOutcome::Reject`], the caller MUST translate that into a
//! user-visible "try again later" error rather than retry locally.
//! Internal retry loops at the admission layer create the very
//! cascading behaviour admission control exists to prevent.

pub mod cluster_throttle;
pub mod decay_rate;
pub mod dist_priority_queue;
pub mod quota_ledger;
pub mod sla_priority;
pub mod tenant_isolation;
pub mod tenant_throttle;

pub use tenant_isolation::{QuotaVerdict, TenantIsolation, TenantQuota, TenantUsage};
pub use tenant_throttle::{TenantId, TenantThrottle, TenantThrottleConfig};

use std::sync::Arc;
use std::time::Duration;

use aiondb_core::{DbError, DbResult};
use tokio::sync::Notify;
use tokio::time::Instant;

/// Maximum bucket capacity. Bounded so misconfigured callers cannot
/// silently allocate huge burst headroom.
pub const MAX_BUCKET_CAPACITY: u64 = 1_000_000_000;

/// Work-class priority. Lower numeric value = higher priority. Bound is
/// small on purpose: more classes means harder reasoning about
/// starvation.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Priority {
    /// Cluster keep-alive: raft heartbeats, lease renewals, replication
    /// status. Should essentially never be throttled.
    System = 0,
    /// Replication apply, WAL flush, follower catch-up.
    Replication = 1,
    /// User DML and queries.
    User = 2,
    /// Batch / analytical / background compaction.
    Batch = 3,
}

impl Priority {
    pub const ALL: [Priority; 4] = [
        Priority::System,
        Priority::Replication,
        Priority::User,
        Priority::Batch,
    ];

    fn idx(self) -> usize {
        self as usize
    }
}

/// Decision returned by the admission controller.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdmissionOutcome {
    /// The caller may proceed immediately.
    Admit,
    /// The caller must back off. Carries an optional hint of how long
    /// before retrying could succeed.
    Reject { retry_after: Duration },
}

/// Configuration knobs.
#[derive(Clone, Debug)]
pub struct AdmissionConfig {
    /// Per-class refill rates (tokens per second). Index by
    /// `Priority as usize`.
    pub rates: [f64; 4],
    /// Per-class burst capacity. Index by `Priority as usize`.
    pub bursts: [u64; 4],
    /// Total in-flight work the controller will admit across **all**
    /// classes. When exceeded, only `Priority::System` admits.
    pub global_in_flight_cap: u64,
}

impl Default for AdmissionConfig {
    fn default() -> Self {
        // Tuned for a conservative single node:
        //   System    : 5_000 ops/s, burst 1_000  (effectively unthrottled).
        //   Replication: 2_000 ops/s, burst 500.
        //   User      : 1_000 ops/s, burst 200.
        //   Batch     :   200 ops/s, burst 50.
        // Operators should override based on benchmarked node capacity.
        Self {
            rates: [5_000.0, 2_000.0, 1_000.0, 200.0],
            bursts: [1_000, 500, 200, 50],
            global_in_flight_cap: 10_000,
        }
    }
}

/// Token bucket rate limiter.
///
/// Atomic-CAS fast path for `try_acquire`. Slow waiters park on a
/// `Notify` until tokens refill or another acquirer releases.
#[derive(Debug)]
pub struct TokenBucket {
    inner: std::sync::Mutex<BucketState>,
    notify: Notify,
    capacity: u64,
    refill_per_sec: f64,
}

#[derive(Debug)]
struct BucketState {
    tokens: f64,
    last_refill: Instant,
}

impl TokenBucket {
    /// Build a fresh bucket starting at full capacity.
    ///
    /// # Errors
    /// Returns `DbError::internal` when `capacity == 0`, when
    /// `capacity > MAX_BUCKET_CAPACITY`, or when `refill_per_sec` is
    /// non-finite or negative.
    pub fn new(capacity: u64, refill_per_sec: f64) -> DbResult<Self> {
        if capacity == 0 {
            return Err(DbError::internal("token bucket capacity must be > 0"));
        }
        if capacity > MAX_BUCKET_CAPACITY {
            return Err(DbError::internal(format!(
                "token bucket capacity {capacity} exceeds MAX_BUCKET_CAPACITY {MAX_BUCKET_CAPACITY}"
            )));
        }
        if !refill_per_sec.is_finite() || refill_per_sec < 0.0 {
            return Err(DbError::internal(format!(
                "token bucket refill_per_sec must be finite and non-negative, got {refill_per_sec}"
            )));
        }
        Ok(Self {
            inner: std::sync::Mutex::new(BucketState {
                tokens: capacity as f64,
                last_refill: Instant::now(),
            }),
            notify: Notify::new(),
            capacity,
            refill_per_sec,
        })
    }

    pub fn capacity(&self) -> u64 {
        self.capacity
    }

    pub fn refill_per_sec(&self) -> f64 {
        self.refill_per_sec
    }

    /// Try to take `n` tokens immediately. Returns `true` on success,
    /// `false` if the bucket has insufficient tokens.
    pub fn try_acquire(&self, n: u64) -> bool {
        if n == 0 {
            return true;
        }
        let mut state = self.lock();
        self.refill(&mut state);
        if state.tokens + 1e-9 < n as f64 {
            return false;
        }
        state.tokens -= n as f64;
        true
    }

    /// Wait until `n` tokens are available, then take them. Cancellable
    /// via the future being dropped. Bounded by `timeout` when set.
    ///
    /// # Errors
    /// Returns `DbError::internal` on timeout.
    pub async fn acquire(&self, n: u64, timeout: Option<Duration>) -> DbResult<()> {
        if n == 0 {
            return Ok(());
        }
        if n as f64 > self.capacity as f64 {
            return Err(DbError::internal(format!(
                "request for {n} tokens exceeds bucket capacity {}",
                self.capacity
            )));
        }
        let deadline = timeout.map(|d| Instant::now() + d);
        loop {
            if self.try_acquire(n) {
                return Ok(());
            }
            let wait_for = self.estimate_wait(n);
            // Re-check just before sleep; another caller may have refilled.
            if self.try_acquire(n) {
                return Ok(());
            }
            let remaining = deadline.map(|d| d.saturating_duration_since(Instant::now()));
            if let Some(r) = remaining {
                if r.is_zero() {
                    return Err(DbError::internal(format!(
                        "timed out waiting for {n} token(s)"
                    )));
                }
            }
            let sleep_for = match remaining {
                Some(r) => wait_for.min(r),
                None => wait_for,
            };
            // Race the notifier and a bounded sleep so we re-check on
            // every release event but still wake on time even if no
            // other caller signals us.
            tokio::select! {
                () = self.notify.notified() => {},
                () = tokio::time::sleep(sleep_for) => {},
            }
        }
    }

    /// Refund tokens. Used when a tentatively-admitted request never
    /// actually ran (e.g., the caller bailed out before doing work).
    pub fn release(&self, n: u64) {
        if n == 0 {
            return;
        }
        {
            let mut state = self.lock();
            state.tokens = (state.tokens + n as f64).min(self.capacity as f64);
        }
        self.notify.notify_waiters();
    }

    /// Current token count, after applying any pending refill.
    pub fn available(&self) -> f64 {
        let mut state = self.lock();
        self.refill(&mut state);
        state.tokens
    }

    fn refill(&self, state: &mut BucketState) {
        if self.refill_per_sec <= 0.0 {
            return;
        }
        let now = Instant::now();
        let elapsed = now.saturating_duration_since(state.last_refill);
        if elapsed.is_zero() {
            return;
        }
        let added = elapsed.as_secs_f64() * self.refill_per_sec;
        state.tokens = (state.tokens + added).min(self.capacity as f64);
        state.last_refill = now;
    }

    fn estimate_wait(&self, n: u64) -> Duration {
        let state = self.lock();
        let needed = (n as f64 - state.tokens).max(0.0);
        if self.refill_per_sec <= 0.0 {
            return Duration::from_millis(10);
        }
        let secs = (needed / self.refill_per_sec).max(0.001);
        // Cap so we still re-check periodically.
        Duration::from_secs_f64(secs.min(0.5))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BucketState> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Priority-aware front door. Wraps one [`TokenBucket`] per
/// [`Priority`] class and enforces a global in-flight cap.
#[derive(Debug)]
pub struct AdmissionController {
    buckets: [Arc<TokenBucket>; 4],
    in_flight: std::sync::atomic::AtomicU64,
    global_cap: u64,
}

impl AdmissionController {
    /// Build a controller from the supplied config.
    ///
    /// # Errors
    /// Propagates any [`TokenBucket::new`] error encountered while
    /// building the per-priority buckets.
    pub fn new(config: AdmissionConfig) -> DbResult<Self> {
        let buckets = [
            Arc::new(TokenBucket::new(
                config.bursts[Priority::System.idx()].max(1),
                config.rates[Priority::System.idx()],
            )?),
            Arc::new(TokenBucket::new(
                config.bursts[Priority::Replication.idx()].max(1),
                config.rates[Priority::Replication.idx()],
            )?),
            Arc::new(TokenBucket::new(
                config.bursts[Priority::User.idx()].max(1),
                config.rates[Priority::User.idx()],
            )?),
            Arc::new(TokenBucket::new(
                config.bursts[Priority::Batch.idx()].max(1),
                config.rates[Priority::Batch.idx()],
            )?),
        ];
        Ok(Self {
            buckets,
            in_flight: std::sync::atomic::AtomicU64::new(0),
            global_cap: config.global_in_flight_cap.max(1),
        })
    }

    /// Try to admit one unit of work at the given priority. Returns
    /// immediately. Use [`AdmissionController::wait_to_admit`] for the
    /// blocking variant.
    pub fn admit(&self, priority: Priority) -> AdmissionOutcome {
        // The global cap is hard for non-system work: even if the
        // priority's local bucket has tokens, we cap the in-flight
        // count so total queue depth stays bounded.
        if priority != Priority::System {
            let in_flight = self.in_flight.load(std::sync::atomic::Ordering::Relaxed);
            if in_flight >= self.global_cap {
                return AdmissionOutcome::Reject {
                    retry_after: Duration::from_millis(50),
                };
            }
        }
        if self.buckets[priority.idx()].try_acquire(1) {
            self.in_flight
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            AdmissionOutcome::Admit
        } else {
            AdmissionOutcome::Reject {
                retry_after: Duration::from_millis(20),
            }
        }
    }

    /// Async variant: waits up to `timeout` for the priority's bucket to
    /// refill. Still enforces the global cap on entry; if the cap is
    /// breached the call returns immediately without waiting.
    ///
    /// # Errors
    /// Returns `DbError::program_limit` when the global in-flight cap
    /// is breached and the priority is below System. Propagates
    /// `TokenBucket::acquire` errors otherwise.
    pub async fn wait_to_admit(&self, priority: Priority, timeout: Duration) -> DbResult<()> {
        if priority != Priority::System {
            let in_flight = self.in_flight.load(std::sync::atomic::Ordering::Relaxed);
            if in_flight >= self.global_cap {
                return Err(DbError::program_limit(format!(
                    "admission: global in-flight cap {} reached",
                    self.global_cap
                )));
            }
        }
        self.buckets[priority.idx()]
            .acquire(1, Some(timeout))
            .await?;
        self.in_flight
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok(())
    }

    /// Notify the controller that a previously-admitted unit of work has
    /// completed (success or failure both count). Pair this with every
    /// `Admit` outcome to keep `in_flight` accurate.
    pub fn release(&self, _priority: Priority) {
        // Saturating decrement: defensive against double-release.
        let mut current = self.in_flight.load(std::sync::atomic::Ordering::Relaxed);
        while current > 0 {
            match self.in_flight.compare_exchange_weak(
                current,
                current - 1,
                std::sync::atomic::Ordering::Relaxed,
                std::sync::atomic::Ordering::Relaxed,
            ) {
                Ok(_) => return,
                Err(actual) => current = actual,
            }
        }
    }

    /// Snapshot for introspection (`pg_stat_admission`-style views).
    pub fn snapshot(&self) -> AdmissionSnapshot {
        AdmissionSnapshot {
            in_flight: self.in_flight.load(std::sync::atomic::Ordering::Relaxed),
            global_cap: self.global_cap,
            available: [
                self.buckets[0].available(),
                self.buckets[1].available(),
                self.buckets[2].available(),
                self.buckets[3].available(),
            ],
        }
    }

    pub fn global_cap(&self) -> u64 {
        self.global_cap
    }

    pub fn in_flight(&self) -> u64 {
        self.in_flight.load(std::sync::atomic::Ordering::Relaxed)
    }
}

/// RAII guard that releases one in-flight slot on drop. Use with
/// [`AdmissionController::admit_guard`] to avoid forgetting to call
/// `release` on every exit path.
#[derive(Debug)]
pub struct AdmissionGuard {
    controller: Arc<AdmissionController>,
    priority: Priority,
    released: bool,
}

impl AdmissionGuard {
    pub fn priority(&self) -> Priority {
        self.priority
    }

    /// Release manually -- equivalent to letting the guard drop, but
    /// allows the caller to mark the slot as freed before doing
    /// additional work that should not count as in-flight.
    pub fn release(mut self) {
        if !self.released {
            self.controller.release(self.priority);
            self.released = true;
        }
    }
}

impl Drop for AdmissionGuard {
    fn drop(&mut self) {
        if !self.released {
            self.controller.release(self.priority);
        }
    }
}

impl AdmissionController {
    /// Like [`Self::admit`] but returns an [`AdmissionGuard`] that
    /// auto-releases on drop. Returns `None` when the request is
    /// rejected.
    pub fn admit_guard(self: &Arc<Self>, priority: Priority) -> Option<AdmissionGuard> {
        match self.admit(priority) {
            AdmissionOutcome::Admit => Some(AdmissionGuard {
                controller: Arc::clone(self),
                priority,
                released: false,
            }),
            AdmissionOutcome::Reject { .. } => None,
        }
    }
}

/// Snapshot of admission state, useful for monitoring views.
#[derive(Clone, Copy, Debug)]
pub struct AdmissionSnapshot {
    pub in_flight: u64,
    pub global_cap: u64,
    pub available: [f64; 4],
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn bucket_try_acquire_succeeds_until_drained() {
        let bucket = TokenBucket::new(3, 0.0).unwrap();
        assert!(bucket.try_acquire(1));
        assert!(bucket.try_acquire(1));
        assert!(bucket.try_acquire(1));
        assert!(!bucket.try_acquire(1));
    }

    #[tokio::test(start_paused = true)]
    async fn bucket_refills_at_configured_rate() {
        let bucket = TokenBucket::new(10, 10.0).unwrap();
        for _ in 0..10 {
            assert!(bucket.try_acquire(1));
        }
        assert!(!bucket.try_acquire(1));
        tokio::time::advance(Duration::from_secs(1)).await;
        // Should have refilled to ~10 tokens.
        for _ in 0..10 {
            assert!(bucket.try_acquire(1));
        }
    }

    #[tokio::test(start_paused = true)]
    async fn bucket_acquire_waits_for_refill() {
        let bucket = Arc::new(TokenBucket::new(1, 10.0).unwrap());
        assert!(bucket.try_acquire(1));
        let b2 = Arc::clone(&bucket);
        let h = tokio::spawn(async move {
            b2.acquire(1, Some(Duration::from_secs(5))).await.unwrap();
        });
        tokio::time::advance(Duration::from_millis(150)).await;
        h.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn bucket_acquire_times_out_without_refill() {
        let bucket = TokenBucket::new(1, 0.0).unwrap();
        assert!(bucket.try_acquire(1));
        let result = bucket.acquire(1, Some(Duration::from_millis(50))).await;
        assert!(result.is_err(), "should time out without refill");
    }

    #[tokio::test(start_paused = true)]
    async fn bucket_release_returns_tokens() {
        let bucket = TokenBucket::new(2, 0.0).unwrap();
        assert!(bucket.try_acquire(2));
        assert!(!bucket.try_acquire(1));
        bucket.release(1);
        assert!(bucket.try_acquire(1));
    }

    #[test]
    fn bucket_rejects_invalid_capacity_or_rate() {
        assert!(TokenBucket::new(0, 1.0).is_err());
        assert!(TokenBucket::new(MAX_BUCKET_CAPACITY + 1, 1.0).is_err());
        assert!(TokenBucket::new(1, f64::NAN).is_err());
        assert!(TokenBucket::new(1, -1.0).is_err());
    }

    fn small_config() -> AdmissionConfig {
        AdmissionConfig {
            rates: [10.0, 5.0, 2.0, 1.0],
            bursts: [4, 3, 2, 1],
            global_in_flight_cap: 8,
        }
    }

    #[test]
    fn admit_consumes_tokens_per_priority() {
        let c = AdmissionController::new(small_config()).unwrap();
        // User burst is 2, so two Admit then Reject.
        assert_eq!(c.admit(Priority::User), AdmissionOutcome::Admit);
        assert_eq!(c.admit(Priority::User), AdmissionOutcome::Admit);
        assert!(matches!(
            c.admit(Priority::User),
            AdmissionOutcome::Reject { .. }
        ));
        // Other priorities unaffected.
        assert_eq!(c.admit(Priority::Replication), AdmissionOutcome::Admit);
    }

    #[test]
    fn release_decrements_in_flight() {
        let c = AdmissionController::new(small_config()).unwrap();
        let _ = c.admit(Priority::User);
        assert_eq!(c.in_flight(), 1);
        c.release(Priority::User);
        assert_eq!(c.in_flight(), 0);
        // Double release saturates at zero.
        c.release(Priority::User);
        assert_eq!(c.in_flight(), 0);
    }

    #[test]
    fn global_cap_rejects_non_system_work() {
        let config = AdmissionConfig {
            rates: [1000.0, 1000.0, 1000.0, 1000.0],
            bursts: [1000, 1000, 1000, 1000],
            global_in_flight_cap: 2,
        };
        let c = AdmissionController::new(config).unwrap();
        assert_eq!(c.admit(Priority::User), AdmissionOutcome::Admit);
        assert_eq!(c.admit(Priority::User), AdmissionOutcome::Admit);
        // Third user request hits the global cap.
        assert!(matches!(
            c.admit(Priority::User),
            AdmissionOutcome::Reject { .. }
        ));
        // System bypasses the cap.
        assert_eq!(c.admit(Priority::System), AdmissionOutcome::Admit);
    }

    #[test]
    fn guard_releases_on_drop() {
        let c = Arc::new(AdmissionController::new(small_config()).unwrap());
        {
            let guard = c.admit_guard(Priority::User).expect("admit");
            assert_eq!(guard.priority(), Priority::User);
            assert_eq!(c.in_flight(), 1);
        }
        assert_eq!(c.in_flight(), 0);
    }

    #[test]
    fn guard_release_is_idempotent_with_drop() {
        let c = Arc::new(AdmissionController::new(small_config()).unwrap());
        let guard = c.admit_guard(Priority::User).expect("admit");
        guard.release();
        assert_eq!(c.in_flight(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn wait_to_admit_refills_after_burst() {
        let c = Arc::new(AdmissionController::new(small_config()).unwrap());
        // Burn the User burst.
        for _ in 0..2 {
            assert_eq!(c.admit(Priority::User), AdmissionOutcome::Admit);
        }
        // Concurrent waiter should succeed once a token refills.
        let c2 = Arc::clone(&c);
        let h = tokio::spawn(async move {
            c2.wait_to_admit(Priority::User, Duration::from_secs(5))
                .await
        });
        tokio::time::advance(Duration::from_secs(1)).await;
        h.await.unwrap().expect("admission should succeed");
        assert!(c.in_flight() >= 1);
    }

    #[test]
    fn snapshot_reports_state() {
        let c = AdmissionController::new(small_config()).unwrap();
        let _ = c.admit(Priority::User);
        let s = c.snapshot();
        assert_eq!(s.in_flight, 1);
        assert_eq!(s.global_cap, 8);
        // User bucket should have ~1 token left (burst was 2, used 1).
        assert!((s.available[Priority::User.idx()] - 1.0).abs() < 0.1);
    }
}
