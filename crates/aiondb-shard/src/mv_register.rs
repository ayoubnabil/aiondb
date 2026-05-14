//! Multi-Value Register (MV-Register) CRDT.
//!
//! Stores all concurrent assignments. Each `set` tags the value with
//! a version vector entry from the writing replica. On merge, values
//! whose version vector is strictly dominated by another's are
//! discarded; remaining values are kept side by side and exposed
//! via [`MvRegister::values`].

use std::collections::BTreeMap;

#[derive(Clone, Debug, Eq, PartialEq, Default)]
pub struct VersionVector(BTreeMap<u64, u64>);

impl VersionVector {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn bump(&mut self, node: u64) -> u64 {
        let v = self.0.entry(node).or_insert(0);
        *v += 1;
        *v
    }

    pub fn merge(&mut self, other: &Self) {
        for (n, v) in &other.0 {
            let cur = self.0.entry(*n).or_insert(0);
            if v > cur {
                *cur = *v;
            }
        }
    }

    /// Returns true iff `self` dominates `other` (every entry of
    /// other is <= self's, and at least one is strictly greater).
    pub fn dominates(&self, other: &Self) -> bool {
        let mut strict = false;
        for (n, v) in &other.0 {
            let s = self.0.get(n).copied().unwrap_or(0);
            if s < *v {
                return false;
            }
            if s > *v {
                strict = true;
            }
        }
        // Catch entries that self has but other doesn't.
        for (n, v) in &self.0 {
            if *v > 0 && !other.0.contains_key(n) {
                strict = true;
            }
        }
        strict
    }

    pub fn concurrent(&self, other: &Self) -> bool {
        !self.dominates(other) && !other.dominates(self) && self != other
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MvEntry<V> {
    pub value: V,
    pub vv: VersionVector,
}

#[derive(Clone, Debug, Default)]
pub struct MvRegister<V: Clone + Eq> {
    entries: Vec<MvEntry<V>>,
}

impl<V: Clone + Eq> MvRegister<V> {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    pub fn set(&mut self, node: u64, value: V) {
        let mut vv = self.combined_vv();
        vv.bump(node);
        self.entries.retain(|e| !vv.dominates(&e.vv));
        self.entries.push(MvEntry { value, vv });
    }

    pub fn values(&self) -> Vec<V> {
        self.entries.iter().map(|e| e.value.clone()).collect()
    }

    pub fn merge(&mut self, other: &Self) {
        let mut combined = self.entries.clone();
        combined.extend(other.entries.iter().cloned());
        let mut kept: Vec<MvEntry<V>> = Vec::new();
        'outer: for cand in combined {
            for k in &kept {
                if k.vv.dominates(&cand.vv) {
                    continue 'outer;
                }
            }
            kept.retain(|k| !cand.vv.dominates(&k.vv));
            if !kept
                .iter()
                .any(|k| k.vv == cand.vv && k.value == cand.value)
            {
                kept.push(cand);
            }
        }
        self.entries = kept;
    }

    fn combined_vv(&self) -> VersionVector {
        let mut vv = VersionVector::new();
        for e in &self.entries {
            vv.merge(&e.vv);
        }
        vv
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_set_returns_single_value() {
        let mut r: MvRegister<&'static str> = MvRegister::new();
        r.set(1, "hello");
        assert_eq!(r.values(), vec!["hello"]);
    }

    #[test]
    fn sequential_writes_keep_latest() {
        let mut r: MvRegister<&'static str> = MvRegister::new();
        r.set(1, "a");
        r.set(1, "b");
        assert_eq!(r.values(), vec!["b"]);
    }

    #[test]
    fn concurrent_writes_keep_both() {
        let mut a: MvRegister<&'static str> = MvRegister::new();
        let mut b: MvRegister<&'static str> = MvRegister::new();
        a.set(1, "x");
        b.set(2, "y");
        a.merge(&b);
        let mut vals = a.values();
        vals.sort_unstable();
        assert_eq!(vals, vec!["x", "y"]);
    }

    #[test]
    fn merge_resolves_dominated_value() {
        let mut a: MvRegister<&'static str> = MvRegister::new();
        let mut b: MvRegister<&'static str> = MvRegister::new();
        a.set(1, "v1");
        b.merge(&a);
        b.set(1, "v2");
        a.merge(&b);
        assert_eq!(a.values(), vec!["v2"]);
    }

    #[test]
    fn merge_is_commutative() {
        let mut a: MvRegister<&'static str> = MvRegister::new();
        let mut b: MvRegister<&'static str> = MvRegister::new();
        a.set(1, "x");
        b.set(2, "y");
        let a2 = a.clone();
        let mut b2 = b.clone();
        a.merge(&b);
        b2.merge(&a2);
        let mut va = a.values();
        va.sort_unstable();
        let mut vb = b2.values();
        vb.sort_unstable();
        assert_eq!(va, vb);
    }

    #[test]
    fn version_vector_dominance() {
        let mut a = VersionVector::new();
        let mut b = VersionVector::new();
        a.bump(1);
        b.bump(1);
        b.bump(1);
        assert!(b.dominates(&a));
        assert!(!a.dominates(&b));
    }

    #[test]
    fn version_vector_concurrent() {
        let mut a = VersionVector::new();
        let mut b = VersionVector::new();
        a.bump(1);
        b.bump(2);
        assert!(a.concurrent(&b));
    }
}
