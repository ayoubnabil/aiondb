//! Observed-Remove Set CRDT.
//!
//! Allows safe distributed add + remove operations. Each add carries
//! a unique tag; remove only invalidates the tags it has observed.
//! Merging is monotonic so concurrent operations from different
//! replicas converge.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct OrSet<T: Clone + Ord + Eq> {
    inner: Arc<std::sync::Mutex<Inner<T>>>,
}

impl<T: Clone + Ord + Eq> Default for OrSet<T> {
    fn default() -> Self {
        Self {
            inner: Arc::new(std::sync::Mutex::new(Inner::default())),
        }
    }
}

#[derive(Debug)]
struct Inner<T: Clone + Ord + Eq> {
    additions: BTreeMap<T, BTreeSet<u64>>, // element -> set of unique tags
    tombstones: BTreeMap<T, BTreeSet<u64>>, // element -> tags marked removed
    next_tag: u64,
}

impl<T: Clone + Ord + Eq> Default for Inner<T> {
    fn default() -> Self {
        Self {
            additions: BTreeMap::new(),
            tombstones: BTreeMap::new(),
            next_tag: 0,
        }
    }
}

impl<T: Clone + Ord + Eq> OrSet<T> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&self, element: T) -> u64 {
        let mut guard = self.inner.lock().unwrap();
        guard.next_tag = guard.next_tag.saturating_add(1);
        let tag = guard.next_tag;
        guard.additions.entry(element).or_default().insert(tag);
        tag
    }

    pub fn remove(&self, element: &T) {
        let mut guard = self.inner.lock().unwrap();
        let tags: BTreeSet<u64> = guard.additions.get(element).cloned().unwrap_or_default();
        guard
            .tombstones
            .entry(element.clone())
            .or_default()
            .extend(tags);
    }

    pub fn contains(&self, element: &T) -> bool {
        let guard = self.inner.lock().unwrap();
        let adds = guard.additions.get(element).cloned().unwrap_or_default();
        let rems = guard.tombstones.get(element).cloned().unwrap_or_default();
        adds.difference(&rems).next().is_some()
    }

    pub fn elements(&self) -> Vec<T> {
        let guard = self.inner.lock().unwrap();
        guard
            .additions
            .keys()
            .filter(|k| {
                let adds = guard.additions.get(*k).cloned().unwrap_or_default();
                let rems = guard.tombstones.get(*k).cloned().unwrap_or_default();
                adds.difference(&rems).next().is_some()
            })
            .cloned()
            .collect()
    }

    pub fn merge(&self, other: &OrSet<T>) {
        let other_state = other.inner.lock().unwrap();
        let mut guard = self.inner.lock().unwrap();
        for (k, tags) in &other_state.additions {
            guard
                .additions
                .entry(k.clone())
                .or_default()
                .extend(tags.iter().copied());
        }
        for (k, tags) in &other_state.tombstones {
            guard
                .tombstones
                .entry(k.clone())
                .or_default()
                .extend(tags.iter().copied());
        }
        guard.next_tag = guard.next_tag.max(other_state.next_tag);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_then_contains() {
        let s: OrSet<&'static str> = OrSet::new();
        s.add("alice");
        assert!(s.contains(&"alice"));
    }

    #[test]
    fn remove_excludes_existing_element() {
        let s: OrSet<&'static str> = OrSet::new();
        s.add("alice");
        s.remove(&"alice");
        assert!(!s.contains(&"alice"));
    }

    #[test]
    fn add_after_remove_re_adds() {
        let s: OrSet<&'static str> = OrSet::new();
        s.add("alice");
        s.remove(&"alice");
        s.add("alice");
        assert!(s.contains(&"alice"));
    }

    #[test]
    fn merge_unions_state_from_both_replicas() {
        let a: OrSet<&'static str> = OrSet::new();
        a.add("alice");
        let b: OrSet<&'static str> = OrSet::new();
        b.add("bob");
        a.merge(&b);
        let elements: BTreeSet<&str> = a.elements().into_iter().collect();
        assert_eq!(elements.len(), 2);
        assert!(elements.contains("alice"));
        assert!(elements.contains("bob"));
    }

    #[test]
    fn concurrent_add_and_remove_preserves_add() {
        // Replica A adds, Replica B removes without seeing the add yet
        // -> after merging, the add wins (Add-Wins semantics).
        let a: OrSet<&'static str> = OrSet::new();
        a.add("alice");
        let b: OrSet<&'static str> = OrSet::new();
        b.remove(&"alice"); // b never saw the add
        a.merge(&b);
        assert!(
            a.contains(&"alice"),
            "Add-Wins : a's add survives b's blind remove"
        );
    }
}
