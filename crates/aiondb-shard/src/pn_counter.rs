//! Positive-Negative Counter CRDT.
//!
//! Allows both increment and decrement. Backed by two G-Counters
//! (one for positives, one for negatives). The value is `pos - neg`.

use crate::replicated_counter::GCounter;

#[derive(Clone, Debug, Default)]
pub struct PnCounter {
    positives: GCounter,
    negatives: GCounter,
}

impl PnCounter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn increment(&self, replica_id: u64, by: u64) {
        self.positives.increment(replica_id, by);
    }

    pub fn decrement(&self, replica_id: u64, by: u64) {
        self.negatives.increment(replica_id, by);
    }

    pub fn value(&self) -> i128 {
        let p = self.positives.value() as i128;
        let n = self.negatives.value() as i128;
        p - n
    }

    pub fn merge(&self, other: &PnCounter) {
        self.positives.merge(&other.positives);
        self.negatives.merge(&other.negatives);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn increment_and_decrement_combine() {
        let c = PnCounter::new();
        c.increment(1, 10);
        c.decrement(1, 3);
        assert_eq!(c.value(), 7);
    }

    #[test]
    fn merge_combines_both_directions() {
        let a = PnCounter::new();
        a.increment(1, 5);
        let b = PnCounter::new();
        b.decrement(2, 2);
        a.merge(&b);
        assert_eq!(a.value(), 3);
    }

    #[test]
    fn fresh_counter_is_zero() {
        let c = PnCounter::new();
        assert_eq!(c.value(), 0);
    }

    #[test]
    fn negative_value_is_supported() {
        let c = PnCounter::new();
        c.decrement(1, 7);
        assert_eq!(c.value(), -7);
    }

    #[test]
    fn merge_idempotent() {
        let a = PnCounter::new();
        a.increment(1, 5);
        let b = a.clone();
        a.merge(&b);
        a.merge(&b);
        assert_eq!(a.value(), 5);
    }
}
