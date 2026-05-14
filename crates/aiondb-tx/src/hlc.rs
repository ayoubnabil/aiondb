//! Hybrid Logical Clock (HLC).
//!
//! Spanner-style external consistency requires globally ordered
//! timestamps. CockroachDB skips Spanner's TrueTime hardware by using
//! a **Hybrid Logical Clock** (Kulkarni et al., 2014): each node keeps
//! a clock that combines wall-clock time and a logical counter, and
//! every cross-node message piggybacks the sender's HLC so the
//! receiver can step its clock forward. The result is a monotonic,
//! comparable timestamp that strictly orders any two causally-related
//! events without requiring synchronised clocks.
//!
//! # Invariants
//!
//! - HLC time is monotonically non-decreasing on any single node.
//! - For two events on different nodes connected by a message,
//!   `now(sender) < update(receiver, now(sender))`.
//! - Reads of the same node's clock from different threads are
//!   linearised: each call returns a timestamp strictly greater than
//!   every prior call on that clock.
//!
//! # Clock model
//!
//! An HLC timestamp is `(wall_time, logical)` where:
//! - `wall_time` is microseconds since the Unix epoch.
//! - `logical` is a counter that ticks within the same `wall_time`
//!   when the physical clock has not advanced.
//!
//! Internally stored under a `parking_lot::Mutex` so the `(wall, logical)`
//! pair updates atomically. Mutex round-trips are sub-100ns in the
//! uncontended case, which is the right trade-off given that every
//! call also has to consult the wall clock.
//!
//! # Bounded skew
//!
//! Real-world clocks drift. The HLC guards against unbounded forward
//! jumps by clamping any external timestamp to `now() +
//! max_offset_us`. A peer claiming to be 10 minutes in the future is
//! rejected with [`HlcError::OffsetTooLarge`] so we never poison the
//! local clock with garbage.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;

/// Default maximum allowed clock offset from any peer (500 ms).
pub const DEFAULT_MAX_OFFSET: Duration = Duration::from_millis(500);

/// HLC timestamp encoded as `(wall_time_us, logical)`.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct HlcTimestamp {
    pub wall_time_us: u64,
    pub logical: u32,
}

impl HlcTimestamp {
    pub const ZERO: Self = Self {
        wall_time_us: 0,
        logical: 0,
    };

    pub const fn new(wall_time_us: u64, logical: u32) -> Self {
        Self {
            wall_time_us,
            logical,
        }
    }

    /// Return a timestamp strictly greater than `self` by bumping the
    /// logical counter. Used when the physical clock has not moved
    /// forward but a fresh monotone tick is required.
    fn next(self) -> Self {
        Self {
            wall_time_us: self.wall_time_us,
            logical: self.logical.saturating_add(1),
        }
    }

    pub fn as_micros(self) -> u64 {
        self.wall_time_us
    }
}

impl std::fmt::Display for HlcTimestamp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}", self.wall_time_us, self.logical)
    }
}

/// Errors emitted when a peer's clock claim is rejected.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HlcError {
    /// Peer's wall_time was further ahead than `max_offset_us`. The
    /// local clock was NOT updated. Caller must retry once peer clock
    /// catches up or refuse to talk to the peer.
    OffsetTooLarge {
        peer_wall_time_us: u64,
        local_wall_time_us: u64,
        max_offset_us: u64,
    },
}

impl std::fmt::Display for HlcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HlcError::OffsetTooLarge {
                peer_wall_time_us,
                local_wall_time_us,
                max_offset_us,
            } => write!(
                f,
                "peer clock {peer_wall_time_us}us is more than {max_offset_us}us ahead of local {local_wall_time_us}us"
            ),
        }
    }
}

impl std::error::Error for HlcError {}

/// Source of physical wall-clock time. Pluggable so tests can drive a
/// fake clock without depending on real `SystemTime`.
pub trait WallClock: Send + Sync {
    fn now_us(&self) -> u64;
}

/// Production wall clock backed by `SystemTime::now()`.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemWallClock;

impl WallClock for SystemWallClock {
    fn now_us(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|d| u64::try_from(d.as_micros()).ok())
            .unwrap_or(0)
    }
}

/// Hybrid Logical Clock.
///
/// Cheap to share across threads behind an `Arc`: the state is held
/// under a `parking_lot::Mutex` so reads and updates contend on a
/// single uncontended spin lock in the hot path.
pub struct HybridLogicalClock<C: WallClock = SystemWallClock> {
    state: Mutex<HlcTimestamp>,
    clock: C,
    max_offset_us: u64,
}

impl HybridLogicalClock<SystemWallClock> {
    pub fn new() -> Self {
        Self::with_clock(SystemWallClock, DEFAULT_MAX_OFFSET)
    }
}

impl Default for HybridLogicalClock<SystemWallClock> {
    fn default() -> Self {
        Self::new()
    }
}

impl<C: WallClock> HybridLogicalClock<C> {
    pub fn with_clock(clock: C, max_offset: Duration) -> Self {
        let max_offset_us = u64::try_from(max_offset.as_micros()).unwrap_or(u64::MAX);
        Self {
            state: Mutex::new(HlcTimestamp::ZERO),
            clock,
            max_offset_us,
        }
    }

    /// Maximum allowed peer offset, in microseconds.
    pub fn max_offset_us(&self) -> u64 {
        self.max_offset_us
    }

    /// Read the current HLC state without advancing it. Returns the
    /// last issued timestamp, or [`HlcTimestamp::ZERO`] when the clock
    /// has never been read.
    pub fn peek(&self) -> HlcTimestamp {
        *self.state.lock()
    }

    /// Issue a fresh local timestamp strictly greater than every prior
    /// timestamp on this clock and at least the current wall-clock.
    pub fn now(&self) -> HlcTimestamp {
        let wall = self.clock.now_us();
        let mut state = self.state.lock();
        let next = if wall > state.wall_time_us {
            HlcTimestamp::new(wall, 0)
        } else {
            state.next()
        };
        *state = next;
        next
    }

    /// Step the clock forward to accommodate a peer's timestamp. Caller
    /// receives a local timestamp that strictly succeeds `peer`.
    ///
    /// # Errors
    /// Returns [`HlcError::OffsetTooLarge`] when `peer.wall_time_us`
    /// exceeds `now() + max_offset_us`. The clock is **not** updated in
    /// that case.
    pub fn update(&self, peer: HlcTimestamp) -> Result<HlcTimestamp, HlcError> {
        let wall = self.clock.now_us();
        if peer.wall_time_us > wall.saturating_add(self.max_offset_us) {
            return Err(HlcError::OffsetTooLarge {
                peer_wall_time_us: peer.wall_time_us,
                local_wall_time_us: wall,
                max_offset_us: self.max_offset_us,
            });
        }
        let mut state = self.state.lock();
        let prev = *state;
        let max_wall = wall.max(prev.wall_time_us).max(peer.wall_time_us);
        let next = if max_wall == prev.wall_time_us && max_wall == peer.wall_time_us {
            // Both prev and peer at this wall_time: take max logical + 1.
            HlcTimestamp::new(max_wall, prev.logical.max(peer.logical).saturating_add(1))
        } else if max_wall == prev.wall_time_us {
            HlcTimestamp::new(max_wall, prev.logical.saturating_add(1))
        } else if max_wall == peer.wall_time_us {
            HlcTimestamp::new(max_wall, peer.logical.saturating_add(1))
        } else {
            // Wall clock leapt ahead of both prev and peer.
            HlcTimestamp::new(max_wall, 0)
        };
        *state = next;
        Ok(next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;

    #[derive(Debug, Default)]
    struct FakeClock {
        now: AtomicU64,
    }
    impl FakeClock {
        fn set(&self, v: u64) {
            self.now.store(v, Ordering::SeqCst);
        }
    }
    impl WallClock for FakeClock {
        fn now_us(&self) -> u64 {
            self.now.load(Ordering::SeqCst)
        }
    }

    fn fake_hlc(initial: u64) -> (Arc<FakeClock>, HybridLogicalClock<Arc<FakeClock>>) {
        let clock = Arc::new(FakeClock::default());
        clock.set(initial);
        let hlc = HybridLogicalClock::with_clock(Arc::clone(&clock), Duration::from_millis(500));
        (clock, hlc)
    }

    impl WallClock for Arc<FakeClock> {
        fn now_us(&self) -> u64 {
            self.as_ref().now_us()
        }
    }

    #[test]
    fn now_advances_with_wall_clock() {
        let (clock, hlc) = fake_hlc(100);
        let a = hlc.now();
        assert_eq!(a, HlcTimestamp::new(100, 0));
        clock.set(200);
        let b = hlc.now();
        assert_eq!(b, HlcTimestamp::new(200, 0));
        assert!(b > a);
    }

    #[test]
    fn now_bumps_logical_when_wall_clock_stuck() {
        let (_, hlc) = fake_hlc(100);
        let a = hlc.now();
        let b = hlc.now();
        let c = hlc.now();
        assert_eq!(a.wall_time_us, b.wall_time_us);
        assert!(b > a);
        assert!(c > b);
        assert_eq!(b.logical, 1);
        assert_eq!(c.logical, 2);
    }

    #[test]
    fn update_advances_clock_to_peer_wall_time() {
        let (clock, hlc) = fake_hlc(50);
        // Peer at wall_time 300.
        let updated = hlc.update(HlcTimestamp::new(300, 5)).unwrap();
        assert_eq!(updated.wall_time_us, 300);
        assert_eq!(updated.logical, 6);
        // Local now() must observe the bump.
        clock.set(300);
        let local = hlc.now();
        assert!(local > updated);
    }

    #[test]
    fn update_rejects_peer_beyond_max_offset() {
        let (_, hlc) = fake_hlc(100);
        // Peer 600us ahead of local 100us = 500us offset, plus 1 to
        // exceed the default 500ms = 500_000us offset by way more.
        let far = 100 + 500_000 + 1;
        let err = hlc
            .update(HlcTimestamp::new(far, 0))
            .expect_err("must reject");
        match err {
            HlcError::OffsetTooLarge {
                peer_wall_time_us,
                local_wall_time_us,
                max_offset_us,
            } => {
                assert_eq!(peer_wall_time_us, far);
                assert_eq!(local_wall_time_us, 100);
                assert_eq!(max_offset_us, 500_000);
            }
        }
        // State not updated.
        assert_eq!(hlc.peek(), HlcTimestamp::ZERO);
    }

    #[test]
    fn update_with_equal_walltime_takes_max_logical_plus_one() {
        let (_, hlc) = fake_hlc(100);
        // Prime local state to (100, 3).
        for _ in 0..4 {
            hlc.now();
        }
        assert_eq!(hlc.peek(), HlcTimestamp::new(100, 3));
        // Peer also at wall_time 100, logical 5.
        let updated = hlc.update(HlcTimestamp::new(100, 5)).unwrap();
        assert_eq!(updated, HlcTimestamp::new(100, 6));
    }

    #[test]
    fn update_with_smaller_peer_still_bumps_local() {
        let (clock, hlc) = fake_hlc(500);
        let _ = hlc.now(); // (500, 0)
        clock.set(600);
        // Peer at (400, 9) -- behind local.
        let updated = hlc.update(HlcTimestamp::new(400, 9)).unwrap();
        // Should advance to wall=600, logical bumps over previous.
        assert_eq!(updated.wall_time_us, 600);
    }

    #[test]
    fn concurrent_now_calls_produce_monotone_unique_timestamps() {
        let (_, hlc) = fake_hlc(100);
        let hlc = Arc::new(hlc);
        let mut handles = Vec::new();
        for _ in 0..4 {
            let h = Arc::clone(&hlc);
            handles.push(std::thread::spawn(move || {
                let mut out = Vec::new();
                for _ in 0..1000 {
                    out.push(h.now());
                }
                out
            }));
        }
        let mut all: Vec<HlcTimestamp> = handles
            .into_iter()
            .flat_map(|h| h.join().unwrap())
            .collect();
        all.sort();
        for w in all.windows(2) {
            assert!(
                w[1] > w[0],
                "duplicates / regress: {:?} -> {:?}",
                w[0],
                w[1]
            );
        }
        assert_eq!(all.len(), 4 * 1000);
    }

    #[test]
    fn peek_does_not_advance() {
        let (_, hlc) = fake_hlc(100);
        let _ = hlc.now();
        let p1 = hlc.peek();
        let p2 = hlc.peek();
        assert_eq!(p1, p2);
    }
}
