//! Range adoption / handoff coordinator.
//!
//! Tracks the state machine of transferring a range from a source
//! replica set to a destination replica set. Each transfer goes
//! through phases :
//!
//! 1. `Pending` - handoff requested.
//! 2. `Snapshotting` - source preparing snapshot.
//! 3. `Streaming` - bytes flowing to destination.
//! 4. `Awaiting` - destination applying tail of log.
//! 5. `Adopted` - destination acked, source can step down.
//!
//! Acks are recorded per replica so a multi-destination handoff
//! is not declared complete until every new voter confirms.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::range_descriptor::{RangeId, ReplicaId};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdoptionPhase {
    Pending,
    Snapshotting,
    Streaming,
    Awaiting,
    Adopted,
    Failed,
}

#[derive(Clone, Debug)]
pub struct AdoptionStatus {
    pub range: RangeId,
    pub source: ReplicaId,
    pub destinations: BTreeMap<ReplicaId, AdoptionPhase>,
    pub started_at: Instant,
    pub finished_at: Option<Instant>,
}

impl AdoptionStatus {
    pub fn overall(&self) -> AdoptionPhase {
        if self.destinations.is_empty() {
            return AdoptionPhase::Pending;
        }
        if self
            .destinations
            .values()
            .any(|p| *p == AdoptionPhase::Failed)
        {
            return AdoptionPhase::Failed;
        }
        if self
            .destinations
            .values()
            .all(|p| *p == AdoptionPhase::Adopted)
        {
            return AdoptionPhase::Adopted;
        }
        *self
            .destinations
            .values()
            .min_by_key(|p| phase_rank(**p))
            .unwrap_or(&AdoptionPhase::Pending)
    }

    pub fn elapsed(&self) -> Duration {
        self.finished_at
            .unwrap_or_else(Instant::now)
            .saturating_duration_since(self.started_at)
    }
}

fn phase_rank(p: AdoptionPhase) -> u8 {
    match p {
        AdoptionPhase::Pending => 0,
        AdoptionPhase::Snapshotting => 1,
        AdoptionPhase::Streaming => 2,
        AdoptionPhase::Awaiting => 3,
        AdoptionPhase::Adopted => 4,
        AdoptionPhase::Failed => 5,
    }
}

#[derive(Clone, Debug, Default)]
pub struct RangeAdoptionCoordinator {
    inner: Arc<std::sync::Mutex<BTreeMap<RangeId, AdoptionStatus>>>,
}

impl RangeAdoptionCoordinator {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn start_handoff(&self, range: RangeId, source: ReplicaId, destinations: Vec<ReplicaId>) {
        let mut guard = self.inner.lock().unwrap();
        let mut dests = BTreeMap::new();
        for d in destinations {
            dests.insert(d, AdoptionPhase::Pending);
        }
        guard.insert(
            range,
            AdoptionStatus {
                range,
                source,
                destinations: dests,
                started_at: Instant::now(),
                finished_at: None,
            },
        );
    }

    pub fn advance(&self, range: RangeId, replica: ReplicaId, phase: AdoptionPhase) -> bool {
        let mut guard = self.inner.lock().unwrap();
        let Some(status) = guard.get_mut(&range) else {
            return false;
        };
        let Some(current) = status.destinations.get_mut(&replica) else {
            return false;
        };
        if phase_rank(phase) < phase_rank(*current) && phase != AdoptionPhase::Failed {
            return false;
        }
        *current = phase;
        if status
            .destinations
            .values()
            .all(|p| *p == AdoptionPhase::Adopted)
            || status
                .destinations
                .values()
                .any(|p| *p == AdoptionPhase::Failed)
        {
            status.finished_at = Some(Instant::now());
        }
        true
    }

    pub fn status(&self, range: RangeId) -> Option<AdoptionStatus> {
        self.inner.lock().unwrap().get(&range).cloned()
    }

    pub fn in_flight(&self) -> Vec<AdoptionStatus> {
        self.inner
            .lock()
            .unwrap()
            .values()
            .filter(|s| s.finished_at.is_none())
            .cloned()
            .collect()
    }

    pub fn purge_completed(&self, older_than: Duration) -> usize {
        let now = Instant::now();
        let mut guard = self.inner.lock().unwrap();
        let before = guard.len();
        guard.retain(|_, s| match s.finished_at {
            Some(t) => now.saturating_duration_since(t) < older_than,
            None => true,
        });
        before - guard.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(n: u64) -> RangeId {
        RangeId::new(n)
    }
    fn rep(n: u64) -> ReplicaId {
        ReplicaId::new(n)
    }

    #[test]
    fn start_and_advance_to_adopted() {
        let c = RangeAdoptionCoordinator::new();
        c.start_handoff(r(1), rep(1), vec![rep(2), rep(3)]);
        assert_eq!(c.status(r(1)).unwrap().overall(), AdoptionPhase::Pending);
        c.advance(r(1), rep(2), AdoptionPhase::Snapshotting);
        c.advance(r(1), rep(2), AdoptionPhase::Streaming);
        c.advance(r(1), rep(2), AdoptionPhase::Adopted);
        assert_ne!(c.status(r(1)).unwrap().overall(), AdoptionPhase::Adopted);
        c.advance(r(1), rep(3), AdoptionPhase::Adopted);
        assert_eq!(c.status(r(1)).unwrap().overall(), AdoptionPhase::Adopted);
    }

    #[test]
    fn failed_destination_marks_overall_failed() {
        let c = RangeAdoptionCoordinator::new();
        c.start_handoff(r(1), rep(1), vec![rep(2), rep(3)]);
        c.advance(r(1), rep(2), AdoptionPhase::Failed);
        assert_eq!(c.status(r(1)).unwrap().overall(), AdoptionPhase::Failed);
    }

    #[test]
    fn cannot_regress_phase() {
        let c = RangeAdoptionCoordinator::new();
        c.start_handoff(r(1), rep(1), vec![rep(2)]);
        c.advance(r(1), rep(2), AdoptionPhase::Streaming);
        assert!(!c.advance(r(1), rep(2), AdoptionPhase::Pending));
        assert_eq!(
            *c.status(r(1)).unwrap().destinations.get(&rep(2)).unwrap(),
            AdoptionPhase::Streaming
        );
    }

    #[test]
    fn in_flight_excludes_finished() {
        let c = RangeAdoptionCoordinator::new();
        c.start_handoff(r(1), rep(1), vec![rep(2)]);
        c.start_handoff(r(2), rep(1), vec![rep(3)]);
        c.advance(r(1), rep(2), AdoptionPhase::Adopted);
        assert_eq!(c.in_flight().len(), 1);
    }

    #[test]
    fn purge_completed_drops_old() {
        let c = RangeAdoptionCoordinator::new();
        c.start_handoff(r(1), rep(1), vec![rep(2)]);
        c.advance(r(1), rep(2), AdoptionPhase::Adopted);
        let removed = c.purge_completed(Duration::from_nanos(0));
        assert_eq!(removed, 1);
        assert!(c.status(r(1)).is_none());
    }

    #[test]
    fn advance_unknown_range_is_noop() {
        let c = RangeAdoptionCoordinator::new();
        assert!(!c.advance(r(99), rep(1), AdoptionPhase::Adopted));
    }
}
