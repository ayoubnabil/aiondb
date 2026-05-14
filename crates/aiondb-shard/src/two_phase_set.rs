//! Two-Phase Set CRDT.
//!
//! Simpler than OR-Set : elements can be added then removed but
//! never re-added. Suitable for "deleted user ids" sets or "applied
//! migrations" lists.

use std::collections::BTreeSet;
use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct TwoPhaseSet<T: Clone + Ord + Eq> {
    inner: Arc<std::sync::Mutex<Inner<T>>>,
}

impl<T: Clone + Ord + Eq> Default for TwoPhaseSet<T> {
    fn default() -> Self {
        Self {
            inner: Arc::new(std::sync::Mutex::new(Inner::default())),
        }
    }
}

#[derive(Debug)]
struct Inner<T: Clone + Ord + Eq> {
    additions: BTreeSet<T>,
    removals: BTreeSet<T>,
}

impl<T: Clone + Ord + Eq> Default for Inner<T> {
    fn default() -> Self {
        Self {
            additions: BTreeSet::new(),
            removals: BTreeSet::new(),
        }
    }
}

impl<T: Clone + Ord + Eq> TwoPhaseSet<T> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&self, element: T) {
        self.inner.lock().unwrap().additions.insert(element);
    }

    pub fn remove(&self, element: T) -> bool {
        let mut guard = self.inner.lock().unwrap();
        if guard.additions.contains(&element) {
            guard.removals.insert(element);
            true
        } else {
            false
        }
    }

    pub fn contains(&self, element: &T) -> bool {
        let guard = self.inner.lock().unwrap();
        guard.additions.contains(element) && !guard.removals.contains(element)
    }

    pub fn elements(&self) -> Vec<T> {
        let guard = self.inner.lock().unwrap();
        guard
            .additions
            .iter()
            .filter(|e| !guard.removals.contains(e))
            .cloned()
            .collect()
    }

    pub fn merge(&self, other: &TwoPhaseSet<T>) {
        let other_state = other.inner.lock().unwrap();
        let mut guard = self.inner.lock().unwrap();
        guard
            .additions
            .extend(other_state.additions.iter().cloned());
        guard.removals.extend(other_state.removals.iter().cloned());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_then_contains() {
        let s: TwoPhaseSet<&'static str> = TwoPhaseSet::new();
        s.add("alice");
        assert!(s.contains(&"alice"));
    }

    #[test]
    fn remove_excludes_element() {
        let s: TwoPhaseSet<&'static str> = TwoPhaseSet::new();
        s.add("alice");
        s.remove("alice");
        assert!(!s.contains(&"alice"));
    }

    #[test]
    fn cannot_re_add_after_remove() {
        let s: TwoPhaseSet<&'static str> = TwoPhaseSet::new();
        s.add("alice");
        s.remove("alice");
        s.add("alice"); // re-add ignored by 2P-Set semantics
        assert!(!s.contains(&"alice"));
    }

    #[test]
    fn remove_of_unknown_returns_false() {
        let s: TwoPhaseSet<&'static str> = TwoPhaseSet::new();
        assert!(!s.remove("ghost"));
    }

    #[test]
    fn merge_unions_both_sides() {
        let a: TwoPhaseSet<u32> = TwoPhaseSet::new();
        a.add(1);
        a.add(2);
        let b: TwoPhaseSet<u32> = TwoPhaseSet::new();
        b.add(2);
        b.remove(2);
        a.merge(&b);
        assert!(a.contains(&1));
        assert!(!a.contains(&2));
    }
}
