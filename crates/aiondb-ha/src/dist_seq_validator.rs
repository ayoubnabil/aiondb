//! Distributed sequence validator.
//!
//! Verifies that the local apply stream is strictly monotonic and
//! free of gaps. A violation indicates a bug in the apply loop or
//! a corrupted log. The validator is used in test/staging clusters
//! as a runtime invariant; production builds can disable it via a
//! feature flag.

use std::collections::BTreeMap;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SeqViolation {
    Regression { previous: u64, observed: u64 },
    Gap { previous: u64, observed: u64 },
    Duplicate { index: u64 },
}

#[derive(Clone, Debug, Default)]
pub struct DistSeqValidator {
    inner: Arc<std::sync::Mutex<BTreeMap<u64, u64>>>,
}

impl DistSeqValidator {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn observe(&self, group: u64, index: u64) -> Result<(), SeqViolation> {
        let mut g = self.inner.lock().unwrap();
        let prev = g.get(&group).copied().unwrap_or(0);
        if index == prev {
            return Err(SeqViolation::Duplicate { index });
        }
        if index < prev {
            return Err(SeqViolation::Regression {
                previous: prev,
                observed: index,
            });
        }
        if prev > 0 && index > prev + 1 {
            return Err(SeqViolation::Gap {
                previous: prev,
                observed: index,
            });
        }
        g.insert(group, index);
        Ok(())
    }

    pub fn last_index(&self, group: u64) -> u64 {
        self.inner.lock().unwrap().get(&group).copied().unwrap_or(0)
    }

    pub fn reset_group(&self, group: u64) {
        self.inner.lock().unwrap().remove(&group);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sequential_observations_succeed() {
        let v = DistSeqValidator::new();
        for i in 1..=10 {
            assert!(v.observe(1, i).is_ok());
        }
    }

    #[test]
    fn gap_is_detected() {
        let v = DistSeqValidator::new();
        v.observe(1, 1).unwrap();
        assert_eq!(
            v.observe(1, 5),
            Err(SeqViolation::Gap {
                previous: 1,
                observed: 5,
            })
        );
    }

    #[test]
    fn regression_is_detected() {
        let v = DistSeqValidator::new();
        v.observe(1, 5).unwrap();
        assert_eq!(
            v.observe(1, 3),
            Err(SeqViolation::Regression {
                previous: 5,
                observed: 3,
            })
        );
    }

    #[test]
    fn duplicate_is_detected() {
        let v = DistSeqValidator::new();
        v.observe(1, 5).unwrap();
        assert_eq!(v.observe(1, 5), Err(SeqViolation::Duplicate { index: 5 }));
    }

    #[test]
    fn groups_independent() {
        let v = DistSeqValidator::new();
        v.observe(1, 1).unwrap();
        v.observe(2, 1).unwrap();
        v.observe(1, 2).unwrap();
        v.observe(2, 2).unwrap();
    }

    #[test]
    fn reset_group_clears_state() {
        let v = DistSeqValidator::new();
        v.observe(1, 5).unwrap();
        v.reset_group(1);
        assert!(v.observe(1, 1).is_ok());
    }

    #[test]
    fn last_index_returns_current() {
        let v = DistSeqValidator::new();
        v.observe(1, 1).unwrap();
        v.observe(1, 2).unwrap();
        assert_eq!(v.last_index(1), 2);
    }
}
