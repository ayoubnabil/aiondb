//! Lease conflict checker.
//!
//! After a range split, both daughter ranges inherit the parent's
//! key span minus the split point. If the lease bookkeeping is
//! sloppy, a stale parent-lease holder could believe it still owns
//! a key now belonging to a daughter. This checker compares lease
//! intervals against the split history and flags conflicts.

use std::collections::BTreeMap;
use std::sync::Arc;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeaseInterval {
    pub holder: String,
    pub epoch: u64,
    pub start_key: Vec<u8>,
    pub end_key: Vec<u8>,
}

#[derive(Clone, Debug, Default)]
pub struct LeaseConflictChecker {
    inner: Arc<std::sync::Mutex<BTreeMap<u64, LeaseInterval>>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Conflict {
    pub a: LeaseInterval,
    pub b: LeaseInterval,
}

impl LeaseConflictChecker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn install(&self, lease: LeaseInterval) {
        self.inner.lock().unwrap().insert(lease.epoch, lease);
    }

    pub fn remove(&self, epoch: u64) -> bool {
        self.inner.lock().unwrap().remove(&epoch).is_some()
    }

    pub fn check_overlap(&self, candidate: &LeaseInterval) -> Vec<Conflict> {
        let g = self.inner.lock().unwrap();
        g.values()
            .filter(|existing| {
                existing.epoch != candidate.epoch
                    && existing.holder != candidate.holder
                    && intervals_overlap(
                        &existing.start_key,
                        &existing.end_key,
                        &candidate.start_key,
                        &candidate.end_key,
                    )
            })
            .map(|e| Conflict {
                a: e.clone(),
                b: candidate.clone(),
            })
            .collect()
    }

    pub fn list_all(&self) -> Vec<LeaseInterval> {
        self.inner.lock().unwrap().values().cloned().collect()
    }
}

fn intervals_overlap(a_start: &[u8], a_end: &[u8], b_start: &[u8], b_end: &[u8]) -> bool {
    if !a_end.is_empty() && b_start >= a_end {
        return false;
    }
    if !b_end.is_empty() && a_start >= b_end {
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lease(holder: &str, epoch: u64, s: &[u8], e: &[u8]) -> LeaseInterval {
        LeaseInterval {
            holder: holder.into(),
            epoch,
            start_key: s.to_vec(),
            end_key: e.to_vec(),
        }
    }

    #[test]
    fn non_overlapping_intervals_no_conflict() {
        let c = LeaseConflictChecker::new();
        c.install(lease("a", 1, b"a", b"m"));
        let conflicts = c.check_overlap(&lease("b", 2, b"m", b"z"));
        assert!(conflicts.is_empty());
    }

    #[test]
    fn overlapping_intervals_flagged() {
        let c = LeaseConflictChecker::new();
        c.install(lease("a", 1, b"a", b"n"));
        let conflicts = c.check_overlap(&lease("b", 2, b"m", b"z"));
        assert_eq!(conflicts.len(), 1);
    }

    #[test]
    fn same_holder_excluded() {
        let c = LeaseConflictChecker::new();
        c.install(lease("a", 1, b"a", b"n"));
        let conflicts = c.check_overlap(&lease("a", 2, b"a", b"n"));
        assert!(conflicts.is_empty());
    }

    #[test]
    fn empty_end_key_unbounded() {
        let c = LeaseConflictChecker::new();
        c.install(lease("a", 1, b"a", b""));
        let conflicts = c.check_overlap(&lease("b", 2, b"z", b""));
        assert_eq!(conflicts.len(), 1);
    }

    #[test]
    fn remove_drops_lease() {
        let c = LeaseConflictChecker::new();
        c.install(lease("a", 1, b"a", b"n"));
        assert!(c.remove(1));
        let conflicts = c.check_overlap(&lease("b", 2, b"m", b"z"));
        assert!(conflicts.is_empty());
    }

    #[test]
    fn list_all_returns_installed() {
        let c = LeaseConflictChecker::new();
        c.install(lease("a", 1, b"a", b"n"));
        c.install(lease("b", 2, b"n", b"z"));
        assert_eq!(c.list_all().len(), 2);
    }
}
