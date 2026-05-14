//! Distributed barrier.
//!
//! N named participants must all check in before the barrier
//! releases. Useful for coordinated phase changes : schema rollouts,
//! drain phases, version upgrades. Each barrier carries an epoch so
//! a participant restarted mid-phase cannot accidentally release the
//! previous barrier.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BarrierState {
    Open,
    Released,
    Timeout,
}

#[derive(Clone, Debug)]
pub struct BarrierStatus {
    pub epoch: u64,
    pub expected: BTreeSet<String>,
    pub checked_in: BTreeSet<String>,
    pub state: BarrierState,
    pub deadline: Option<Instant>,
}

impl BarrierStatus {
    pub fn missing(&self) -> Vec<String> {
        self.expected
            .difference(&self.checked_in)
            .cloned()
            .collect()
    }
}

#[derive(Clone, Debug, Default)]
pub struct DistributedBarrier {
    inner: Arc<std::sync::Mutex<BTreeMap<String, BarrierStatus>>>,
}

impl DistributedBarrier {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn open(
        &self,
        name: impl Into<String>,
        epoch: u64,
        participants: impl IntoIterator<Item = String>,
        timeout: Option<Duration>,
    ) -> BarrierStatus {
        let mut g = self.inner.lock().unwrap();
        let status = BarrierStatus {
            epoch,
            expected: participants.into_iter().collect(),
            checked_in: BTreeSet::new(),
            state: BarrierState::Open,
            deadline: timeout.map(|t| Instant::now() + t),
        };
        let key = name.into();
        g.insert(key.clone(), status.clone());
        status
    }

    pub fn check_in(&self, name: &str, epoch: u64, participant: &str) -> BarrierState {
        let mut g = self.inner.lock().unwrap();
        let Some(b) = g.get_mut(name) else {
            return BarrierState::Open;
        };
        if b.epoch != epoch {
            return b.state;
        }
        if let Some(d) = b.deadline {
            if Instant::now() > d {
                b.state = BarrierState::Timeout;
                return BarrierState::Timeout;
            }
        }
        b.checked_in.insert(participant.to_string());
        if b.checked_in == b.expected && b.state == BarrierState::Open {
            b.state = BarrierState::Released;
        }
        b.state
    }

    pub fn status(&self, name: &str) -> Option<BarrierStatus> {
        let mut g = self.inner.lock().unwrap();
        let b = g.get_mut(name)?;
        if let Some(d) = b.deadline {
            if b.state == BarrierState::Open && Instant::now() > d {
                b.state = BarrierState::Timeout;
            }
        }
        Some(b.clone())
    }

    pub fn close(&self, name: &str) {
        self.inner.lock().unwrap().remove(name);
    }

    pub fn outstanding(&self) -> Vec<String> {
        self.inner
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, b)| b.state == BarrierState::Open)
            .map(|(k, _)| k.clone())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parts(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn barrier_releases_when_all_check_in() {
        let b = DistributedBarrier::new();
        b.open("upgrade", 1, parts(&["n1", "n2", "n3"]), None);
        assert_eq!(b.check_in("upgrade", 1, "n1"), BarrierState::Open);
        assert_eq!(b.check_in("upgrade", 1, "n2"), BarrierState::Open);
        assert_eq!(b.check_in("upgrade", 1, "n3"), BarrierState::Released);
    }

    #[test]
    fn stale_epoch_ignored() {
        let b = DistributedBarrier::new();
        b.open("phase", 5, parts(&["n1"]), None);
        let s = b.check_in("phase", 4, "n1");
        assert_eq!(s, BarrierState::Open);
        let st = b.status("phase").unwrap();
        assert!(!st.checked_in.contains("n1"));
    }

    #[test]
    fn duplicate_check_in_is_idempotent() {
        let b = DistributedBarrier::new();
        b.open("p", 1, parts(&["n1", "n2"]), None);
        b.check_in("p", 1, "n1");
        b.check_in("p", 1, "n1");
        assert_eq!(b.status("p").unwrap().checked_in.len(), 1);
    }

    #[test]
    fn timeout_moves_to_timeout_state() {
        let b = DistributedBarrier::new();
        b.open("p", 1, parts(&["n1"]), Some(Duration::from_millis(5)));
        std::thread::sleep(Duration::from_millis(15));
        let s = b.status("p").unwrap();
        assert_eq!(s.state, BarrierState::Timeout);
    }

    #[test]
    fn missing_lists_remaining_participants() {
        let b = DistributedBarrier::new();
        b.open("p", 1, parts(&["n1", "n2", "n3"]), None);
        b.check_in("p", 1, "n1");
        let s = b.status("p").unwrap();
        let missing = s.missing();
        assert_eq!(missing.len(), 2);
    }

    #[test]
    fn outstanding_lists_open_barriers() {
        let b = DistributedBarrier::new();
        b.open("a", 1, parts(&["n1"]), None);
        b.open("b", 1, parts(&["n1"]), None);
        b.check_in("a", 1, "n1");
        let out = b.outstanding();
        assert_eq!(out, vec!["b".to_string()]);
    }
}
