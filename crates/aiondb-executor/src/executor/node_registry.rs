//! Dynamic node registry for distributed fragment execution.
//!
//! Tracks cluster nodes, their health status, and applies circuit breaker
//! logic to avoid dispatching fragments to unhealthy nodes.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::time::{Duration, Instant};

use tracing::warn;

use super::RemoteFragmentHandler;

// ---------------------------------------------------------------------------
// Circuit breaker
// ---------------------------------------------------------------------------

/// State machine for the circuit breaker pattern.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CircuitBreakerState {
    /// Normal operation -- requests pass through.
    Closed,
    /// Failures exceeded the threshold -- requests are rejected until the
    /// reset timeout elapses.
    Open,
    /// The reset timeout has elapsed -- a single probe request is allowed
    /// through to test recovery.
    HalfOpen,
}

impl CircuitBreakerState {
    /// Stable numeric value for gauges: closed=0, half-open=1, open=2.
    pub const fn metric_value(self) -> u64 {
        match self {
            Self::Closed => 0,
            Self::HalfOpen => 1,
            Self::Open => 2,
        }
    }
}

/// Per-node circuit breaker that tracks consecutive failures and temporarily
/// blocks dispatching to a node that appears unhealthy.
pub struct CircuitBreaker {
    state: RwLock<CircuitBreakerState>,
    failure_count: AtomicU32,
    failure_threshold: u32,
    reset_timeout: Duration,
    last_state_change: RwLock<Instant>,
}

impl CircuitBreaker {
    /// Create a new breaker in the `Closed` state.
    pub fn new(failure_threshold: u32, reset_timeout: Duration) -> Self {
        let failure_threshold = normalize_failure_threshold(failure_threshold);
        Self {
            state: RwLock::new(CircuitBreakerState::Closed),
            failure_count: AtomicU32::new(0),
            failure_threshold,
            reset_timeout,
            last_state_change: RwLock::new(Instant::now()),
        }
    }

    /// Returns `true` when the breaker permits a request to pass through.
    ///
    /// * `Closed` -- always allows.
    /// * `Open` -- allows only after the reset timeout has elapsed, in
    ///   which case it transitions to `HalfOpen`.
    /// * `HalfOpen` -- allows (a single probe).
    pub fn allow_request(&self) -> bool {
        let current = *read_lock(&self.state);
        match current {
            CircuitBreakerState::Closed | CircuitBreakerState::HalfOpen => true,
            CircuitBreakerState::Open => {
                let elapsed = read_lock(&self.last_state_change).elapsed();
                if elapsed >= self.reset_timeout {
                    let mut state = write_lock(&self.state);
                    // Re-check after acquiring write lock.
                    if *state == CircuitBreakerState::Open {
                        *state = CircuitBreakerState::HalfOpen;
                        *write_lock(&self.last_state_change) = Instant::now();
                    }
                    true
                } else {
                    false
                }
            }
        }
    }

    /// Record a successful request -- resets the failure counter and
    /// transitions the breaker back to `Closed`.
    pub fn record_success(&self) {
        self.failure_count.store(0, Ordering::Relaxed);
        let mut state = write_lock(&self.state);
        if *state != CircuitBreakerState::Closed {
            *state = CircuitBreakerState::Closed;
            *write_lock(&self.last_state_change) = Instant::now();
        }
    }

    /// Record a failed request.  When the failure count reaches the
    /// threshold the breaker transitions to `Open`.
    pub fn record_failure(&self) {
        let count = self.failure_count.fetch_add(1, Ordering::Relaxed) + 1;
        if count >= self.failure_threshold {
            let mut state = write_lock(&self.state);
            if *state != CircuitBreakerState::Open {
                *state = CircuitBreakerState::Open;
                *write_lock(&self.last_state_change) = Instant::now();
            }
        }
    }

    /// Current state of the breaker (snapshot).
    pub fn state(&self) -> CircuitBreakerState {
        *read_lock(&self.state)
    }

    /// Consecutive failure count currently tracked by this breaker.
    pub fn failure_count(&self) -> u32 {
        self.failure_count.load(Ordering::Relaxed)
    }

    /// Configured consecutive failure threshold.
    pub fn failure_threshold(&self) -> u32 {
        self.failure_threshold
    }

    /// Configured reset timeout.
    pub fn reset_timeout(&self) -> Duration {
        self.reset_timeout
    }

    /// Convenience: `true` when the breaker is in the `Open` state.
    pub fn is_open(&self) -> bool {
        self.state() == CircuitBreakerState::Open
    }
}

// ---------------------------------------------------------------------------
// Node entry
// ---------------------------------------------------------------------------

/// A single registered cluster node together with its health tracking state.
pub struct NodeEntry {
    pub node_id: String,
    pub addr: String,
    pub handler: Arc<RemoteFragmentHandler>,
    circuit_breaker: CircuitBreaker,
    registered_at: Instant,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NodeHealthSnapshot {
    pub node_id: String,
    pub addr: String,
    pub circuit_breaker_state: CircuitBreakerState,
    pub available: bool,
    pub consecutive_failures: u32,
    pub failure_threshold: u32,
    pub reset_timeout_ms: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NodeRegistrySnapshot {
    pub total_nodes: usize,
    pub available_nodes: usize,
    pub open_circuits: usize,
    pub half_open_circuits: usize,
    pub nodes: Vec<NodeHealthSnapshot>,
}

impl NodeEntry {
    /// Create a new node entry with the given circuit breaker configuration.
    pub fn new(
        node_id: String,
        addr: String,
        handler: Arc<RemoteFragmentHandler>,
        failure_threshold: u32,
        reset_timeout: Duration,
    ) -> Self {
        Self {
            node_id,
            addr,
            handler,
            circuit_breaker: CircuitBreaker::new(failure_threshold, reset_timeout),
            registered_at: Instant::now(),
        }
    }

    /// Returns `true` when the node's circuit breaker allows requests.
    pub fn is_available(&self) -> bool {
        self.circuit_breaker.allow_request()
    }

    /// The instant at which this node was registered.
    pub fn registered_at(&self) -> Instant {
        self.registered_at
    }

    /// Expose the inner circuit breaker for direct state queries.
    pub fn circuit_breaker(&self) -> &CircuitBreaker {
        &self.circuit_breaker
    }

    pub fn health_snapshot(&self) -> NodeHealthSnapshot {
        let state = self.circuit_breaker.state();
        NodeHealthSnapshot {
            node_id: self.node_id.clone(),
            addr: self.addr.clone(),
            circuit_breaker_state: state,
            available: state != CircuitBreakerState::Open,
            consecutive_failures: self.circuit_breaker.failure_count(),
            failure_threshold: self.circuit_breaker.failure_threshold(),
            reset_timeout_ms: duration_millis_u64(self.circuit_breaker.reset_timeout()),
        }
    }
}

impl std::fmt::Debug for NodeEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NodeEntry")
            .field("node_id", &self.node_id)
            .field("addr", &self.addr)
            .field("handler", &"<fn>")
            .field("circuit_breaker_state", &self.circuit_breaker.state())
            .field("registered_at", &self.registered_at)
            .finish()
    }
}

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

/// Thread-safe registry of cluster nodes used by the distributed fragment
/// dispatcher.
///
/// Each node carries an independent [`CircuitBreaker`] so that failing nodes
/// are automatically excluded from fragment dispatch until they recover.
pub struct NodeRegistry {
    nodes: RwLock<HashMap<String, Arc<NodeEntry>>>,
    default_failure_threshold: u32,
    default_reset_timeout: Duration,
}

impl NodeRegistry {
    /// Create a registry with default circuit-breaker settings (threshold=5,
    /// reset timeout=30 s).
    pub fn new() -> Self {
        Self {
            nodes: RwLock::new(HashMap::new()),
            default_failure_threshold: 5,
            default_reset_timeout: Duration::from_secs(30),
        }
    }

    /// Create a registry with custom circuit-breaker defaults.
    pub fn with_circuit_breaker_config(threshold: u32, reset_timeout: Duration) -> Self {
        Self {
            nodes: RwLock::new(HashMap::new()),
            default_failure_threshold: normalize_failure_threshold(threshold),
            default_reset_timeout: reset_timeout,
        }
    }

    /// Register (or replace) a node.
    pub fn register(&self, node_id: String, addr: String, handler: Arc<RemoteFragmentHandler>) {
        let entry = Arc::new(NodeEntry::new(
            node_id.clone(),
            addr,
            handler,
            self.default_failure_threshold,
            self.default_reset_timeout,
        ));
        write_lock(&self.nodes).insert(node_id, entry);
    }

    /// Remove a node from the registry.
    pub fn unregister(&self, node_id: &str) {
        write_lock(&self.nodes).remove(node_id);
    }

    /// Look up a node by id.
    pub fn get(&self, node_id: &str) -> Option<Arc<NodeEntry>> {
        read_lock(&self.nodes).get(node_id).cloned()
    }

    /// Return only nodes whose circuit breaker currently allows requests.
    pub fn available_nodes(&self) -> Vec<Arc<NodeEntry>> {
        read_lock(&self.nodes)
            .values()
            .filter(|entry| entry.is_available())
            .cloned()
            .collect()
    }

    /// Return all registered nodes regardless of health status.
    pub fn all_nodes(&self) -> Vec<Arc<NodeEntry>> {
        read_lock(&self.nodes).values().cloned().collect()
    }

    /// Return a non-mutating point-in-time view of remote node health.
    pub fn health_snapshot(&self) -> NodeRegistrySnapshot {
        let mut nodes = read_lock(&self.nodes)
            .values()
            .map(|entry| entry.health_snapshot())
            .collect::<Vec<_>>();
        nodes.sort_by(|left, right| left.node_id.cmp(&right.node_id));
        let available_nodes = nodes.iter().filter(|node| node.available).count();
        let open_circuits = nodes
            .iter()
            .filter(|node| node.circuit_breaker_state == CircuitBreakerState::Open)
            .count();
        let half_open_circuits = nodes
            .iter()
            .filter(|node| node.circuit_breaker_state == CircuitBreakerState::HalfOpen)
            .count();
        NodeRegistrySnapshot {
            total_nodes: nodes.len(),
            available_nodes,
            open_circuits,
            half_open_circuits,
            nodes,
        }
    }

    /// Record a successful dispatch to the given node.
    pub fn record_success(&self, node_id: &str) {
        if let Some(entry) = self.get(node_id) {
            entry.circuit_breaker.record_success();
        }
    }

    /// Record a failed dispatch to the given node.
    pub fn record_failure(&self, node_id: &str) {
        if let Some(entry) = self.get(node_id) {
            entry.circuit_breaker.record_failure();
        }
    }

    /// Total number of registered nodes.
    pub fn node_count(&self) -> usize {
        read_lock(&self.nodes).len()
    }

    /// Number of nodes currently available (circuit breaker allows requests).
    pub fn available_count(&self) -> usize {
        self.available_nodes().len()
    }
}

impl Default for NodeRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for NodeRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let node_ids: Vec<String> = read_lock(&self.nodes).keys().cloned().collect();
        f.debug_struct("NodeRegistry")
            .field("nodes", &node_ids)
            .field("default_failure_threshold", &self.default_failure_threshold)
            .field("default_reset_timeout", &self.default_reset_timeout)
            .finish()
    }
}

// ---------------------------------------------------------------------------
// FragmentDispatcher implementation
// ---------------------------------------------------------------------------

impl super::FragmentDispatcher for NodeRegistry {
    /// Route a fragment to the appropriate node using registry lookups and
    /// circuit breaker health tracking.
    ///
    /// * `Local` and `loopback:*` targets are executed in-process.
    /// * Named remote targets are looked up in the registry; the circuit
    ///   breaker must allow the request and is updated on success/failure.
    fn execute_fragment(
        &self,
        fragment: &super::DistributedFragment,
        executor: &super::Executor,
        context: &super::ExecutionContext,
    ) -> aiondb_core::DbResult<crate::ExecutionResult> {
        match &fragment.target {
            super::FragmentTarget::Local => {
                let fragment_context = context
                    .clone()
                    .with_distributed_current_shard_id(fragment.shard_id);
                executor.execute(&fragment.plan, &fragment_context)
            }
            super::FragmentTarget::Remote(node_id) if node_id.starts_with("loopback:") => {
                let fragment_context = context
                    .clone()
                    .with_distributed_current_shard_id(fragment.shard_id);
                executor.execute(&fragment.plan, &fragment_context)
            }
            super::FragmentTarget::Remote(node_id) => {
                let fragment_context = context
                    .clone()
                    .with_distributed_current_shard_id(fragment.shard_id);
                let entry = self.get(node_id).ok_or_else(|| {
                    aiondb_core::DbError::feature_not_supported(format!(
                        "remote node \"{node_id}\" is not registered in the node registry",
                    ))
                })?;

                if !entry.circuit_breaker.allow_request() {
                    return Err(aiondb_core::DbError::feature_not_supported(format!(
                        "remote node \"{node_id}\" circuit breaker is open (node unavailable)",
                    )));
                }

                match (entry.handler)(fragment, executor, &fragment_context) {
                    Ok(result) => {
                        entry.circuit_breaker.record_success();
                        Ok(result)
                    }
                    Err(error) => {
                        entry.circuit_breaker.record_failure();
                        Err(error)
                    }
                }
            }
        }
    }
}

fn normalize_failure_threshold(threshold: u32) -> u32 {
    if threshold == 0 {
        warn!("circuit breaker failure_threshold=0 is invalid; normalizing to 1");
        1
    } else {
        threshold
    }
}

fn read_lock<T>(lock: &RwLock<T>) -> RwLockReadGuard<'_, T> {
    lock.read().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn write_lock<T>(lock: &RwLock<T>) -> RwLockWriteGuard<'_, T> {
    lock.write()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn duration_millis_u64(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn noop_handler() -> Arc<RemoteFragmentHandler> {
        Arc::new(|_fragment, _executor, _ctx| Ok(crate::ExecutionResult::command("OK")))
    }

    // -- CircuitBreaker unit tests ------------------------------------------

    #[test]
    fn breaker_starts_closed() {
        let cb = CircuitBreaker::new(3, Duration::from_secs(10));
        assert_eq!(cb.state(), CircuitBreakerState::Closed);
        assert!(!cb.is_open());
        assert!(cb.allow_request());
    }

    #[test]
    fn breaker_opens_after_threshold_failures() {
        let cb = CircuitBreaker::new(3, Duration::from_secs(10));
        cb.record_failure();
        cb.record_failure();
        assert_eq!(cb.state(), CircuitBreakerState::Closed);
        cb.record_failure(); // 3rd failure => threshold reached
        assert_eq!(cb.state(), CircuitBreakerState::Open);
        assert!(cb.is_open());
        assert!(!cb.allow_request());
    }

    #[test]
    fn breaker_stays_closed_below_threshold() {
        let cb = CircuitBreaker::new(5, Duration::from_secs(10));
        for _ in 0..4 {
            cb.record_failure();
        }
        assert_eq!(cb.state(), CircuitBreakerState::Closed);
        assert!(cb.allow_request());
    }

    #[test]
    fn breaker_normalizes_zero_threshold() {
        let cb = CircuitBreaker::new(0, Duration::from_secs(10));
        assert_eq!(cb.failure_threshold, 1);
        cb.record_failure();
        assert_eq!(cb.state(), CircuitBreakerState::Open);
    }

    #[test]
    fn breaker_success_resets_counter() {
        let cb = CircuitBreaker::new(3, Duration::from_secs(10));
        cb.record_failure();
        cb.record_failure();
        cb.record_success(); // resets
        cb.record_failure();
        cb.record_failure();
        // Only 2 failures since last reset -- still closed.
        assert_eq!(cb.state(), CircuitBreakerState::Closed);
    }

    #[test]
    fn breaker_transitions_open_to_half_open_after_timeout() {
        let cb = CircuitBreaker::new(1, Duration::from_millis(1));
        cb.record_failure(); // opens immediately
        assert_eq!(cb.state(), CircuitBreakerState::Open);
        // Wait for the reset timeout to elapse.
        std::thread::sleep(Duration::from_millis(5));
        assert!(cb.allow_request()); // should transition to HalfOpen
        assert_eq!(cb.state(), CircuitBreakerState::HalfOpen);
    }

    #[test]
    fn breaker_half_open_to_closed_on_success() {
        let cb = CircuitBreaker::new(1, Duration::from_millis(1));
        cb.record_failure();
        std::thread::sleep(Duration::from_millis(5));
        assert!(cb.allow_request()); // => HalfOpen
        assert_eq!(cb.state(), CircuitBreakerState::HalfOpen);
        cb.record_success();
        assert_eq!(cb.state(), CircuitBreakerState::Closed);
        assert_eq!(cb.failure_count.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn breaker_half_open_to_open_on_failure() {
        let cb = CircuitBreaker::new(1, Duration::from_millis(1));
        cb.record_failure();
        std::thread::sleep(Duration::from_millis(5));
        let _ = cb.allow_request(); // => HalfOpen
        cb.record_failure(); // should re-open
        assert_eq!(cb.state(), CircuitBreakerState::Open);
    }

    // -- NodeEntry tests ----------------------------------------------------

    #[test]
    fn node_entry_delegates_availability() {
        let entry = NodeEntry::new(
            "n1".into(),
            "127.0.0.1:5433".into(),
            noop_handler(),
            2,
            Duration::from_secs(30),
        );
        assert!(entry.is_available());
        entry.circuit_breaker.record_failure();
        entry.circuit_breaker.record_failure();
        assert!(!entry.is_available());
    }

    // -- NodeRegistry tests -------------------------------------------------

    #[test]
    fn registry_register_and_get() {
        let reg = NodeRegistry::new();
        reg.register("n1".into(), "addr1".into(), noop_handler());
        let entry = reg.get("n1").expect("node should exist");
        assert_eq!(entry.node_id, "n1");
        assert_eq!(entry.addr, "addr1");
        assert!(reg.get("n999").is_none());
    }

    #[test]
    fn registry_unregister() {
        let reg = NodeRegistry::new();
        reg.register("n1".into(), "addr1".into(), noop_handler());
        assert_eq!(reg.node_count(), 1);
        reg.unregister("n1");
        assert_eq!(reg.node_count(), 0);
        assert!(reg.get("n1").is_none());
    }

    #[test]
    fn registry_replace_existing_node() {
        let reg = NodeRegistry::new();
        reg.register("n1".into(), "old-addr".into(), noop_handler());
        reg.register("n1".into(), "new-addr".into(), noop_handler());
        assert_eq!(reg.node_count(), 1);
        assert_eq!(reg.get("n1").unwrap().addr, "new-addr");
    }

    #[test]
    fn registry_available_nodes_filters_unhealthy() {
        let reg = NodeRegistry::with_circuit_breaker_config(2, Duration::from_secs(60));
        reg.register("healthy".into(), "a1".into(), noop_handler());
        reg.register("sick".into(), "a2".into(), noop_handler());
        // Trip the breaker on "sick".
        reg.record_failure("sick");
        reg.record_failure("sick");
        let available = reg.available_nodes();
        assert_eq!(available.len(), 1);
        assert_eq!(available[0].node_id, "healthy");
        assert_eq!(reg.available_count(), 1);
        assert_eq!(reg.all_nodes().len(), 2);
    }

    #[test]
    fn registry_health_snapshot_reports_sorted_circuit_state_without_mutating() {
        let reg = NodeRegistry::with_circuit_breaker_config(2, Duration::from_millis(1500));
        reg.register("node-b".into(), "addr-b".into(), noop_handler());
        reg.register("node-a".into(), "addr-a".into(), noop_handler());
        reg.record_failure("node-b");
        reg.record_failure("node-b");

        let snapshot = reg.health_snapshot();

        assert_eq!(snapshot.total_nodes, 2);
        assert_eq!(snapshot.available_nodes, 1);
        assert_eq!(snapshot.open_circuits, 1);
        assert_eq!(snapshot.half_open_circuits, 0);
        assert_eq!(snapshot.nodes[0].node_id, "node-a");
        assert_eq!(snapshot.nodes[1].node_id, "node-b");
        assert_eq!(
            snapshot.nodes[1].circuit_breaker_state,
            CircuitBreakerState::Open
        );
        assert!(!snapshot.nodes[1].available);
        assert_eq!(snapshot.nodes[1].consecutive_failures, 2);
        assert_eq!(snapshot.nodes[1].failure_threshold, 2);
        assert_eq!(snapshot.nodes[1].reset_timeout_ms, 1500);
    }

    #[test]
    fn registry_normalizes_zero_threshold_config() {
        let reg = NodeRegistry::with_circuit_breaker_config(0, Duration::from_secs(60));
        assert_eq!(reg.default_failure_threshold, 1);
        reg.register("n1".into(), "a1".into(), noop_handler());
        reg.record_failure("n1");
        assert_eq!(reg.available_count(), 0);
    }

    #[test]
    fn registry_record_success_resets_breaker() {
        let reg = NodeRegistry::with_circuit_breaker_config(2, Duration::from_secs(60));
        reg.register("n1".into(), "a".into(), noop_handler());
        reg.record_failure("n1");
        reg.record_failure("n1");
        assert_eq!(reg.available_count(), 0);
        // Manually allow the success (simulate half-open probe elsewhere).
        reg.record_success("n1");
        assert_eq!(reg.available_count(), 1);
    }

    #[test]
    fn registry_record_on_unknown_node_is_noop() {
        let reg = NodeRegistry::new();
        // Should not panic.
        reg.record_success("ghost");
        reg.record_failure("ghost");
    }

    #[test]
    fn registry_concurrent_access() {
        let reg = Arc::new(NodeRegistry::with_circuit_breaker_config(
            100,
            Duration::from_secs(60),
        ));
        reg.register("n1".into(), "a".into(), noop_handler());

        let mut handles = Vec::new();
        for _ in 0..8 {
            let r = Arc::clone(&reg);
            handles.push(std::thread::spawn(move || {
                for _ in 0..200 {
                    r.record_failure("n1");
                    let _ = r.available_nodes();
                    let _ = r.get("n1");
                }
            }));
        }
        for h in handles {
            h.join().expect("thread panicked");
        }
        // After 8*200 = 1600 failures with threshold 100, breaker must be open.
        let entry = reg.get("n1").unwrap();
        assert!(entry.circuit_breaker().is_open());
    }

    #[test]
    fn registry_default_config() {
        let reg = NodeRegistry::new();
        assert_eq!(reg.default_failure_threshold, 5);
        assert_eq!(reg.default_reset_timeout, Duration::from_secs(30));
        assert_eq!(reg.node_count(), 0);
        assert_eq!(reg.available_count(), 0);
    }

    // -- FragmentDispatcher implementation tests ------------------------------

    #[test]
    fn dispatcher_rejects_unregistered_remote_node() {
        let reg = NodeRegistry::new();
        // The registry should not find unregistered nodes.
        assert!(
            reg.get("unknown-node").is_none(),
            "node should not be registered"
        );
    }

    #[test]
    fn dispatcher_rejects_node_with_open_breaker() {
        let reg = NodeRegistry::with_circuit_breaker_config(1, Duration::from_secs(60));
        reg.register("n1".into(), "addr".into(), noop_handler());
        reg.record_failure("n1");

        let entry = reg.get("n1").unwrap();
        assert!(
            !entry.circuit_breaker.allow_request(),
            "breaker should be open after 1 failure with threshold=1"
        );
    }

    #[test]
    fn dispatcher_success_resets_breaker_via_dispatch() {
        let reg = NodeRegistry::with_circuit_breaker_config(3, Duration::from_secs(60));
        reg.register("n1".into(), "addr".into(), noop_handler());

        // Accumulate some failures (but not enough to trip).
        reg.record_failure("n1");
        reg.record_failure("n1");
        assert!(reg.get("n1").unwrap().is_available());

        // A success resets the counter.
        reg.record_success("n1");
        reg.record_failure("n1");
        reg.record_failure("n1");
        // Still available: only 2 failures since last reset.
        assert!(reg.get("n1").unwrap().is_available());
    }
}
