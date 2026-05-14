//! Distributed savepoint manager.
//!
//! Tracks named savepoints inside an open transaction so the
//! application can roll back a portion of the work. Each savepoint
//! captures the set of intents that exist at that moment. Rolling
//! back removes every intent created after the savepoint.

use std::collections::BTreeMap;
use std::sync::Arc;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Intent {
    pub key: Vec<u8>,
    pub seq: u64,
}

#[derive(Clone, Debug)]
struct SavepointMarker {
    name: String,
    seq_floor: u64,
}

#[derive(Clone, Debug, Default)]
pub struct DistSavepointManager {
    inner: Arc<std::sync::Mutex<SavepointState>>,
}

#[derive(Default, Debug)]
struct SavepointState {
    intents: Vec<Intent>,
    markers: Vec<SavepointMarker>,
    seq: u64,
}

impl DistSavepointManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record_intent(&self, key: Vec<u8>) -> u64 {
        let mut g = self.inner.lock().unwrap();
        g.seq += 1;
        let seq = g.seq;
        g.intents.push(Intent { key, seq });
        seq
    }

    pub fn savepoint(&self, name: impl Into<String>) {
        let mut g = self.inner.lock().unwrap();
        let seq_floor = g.seq;
        g.markers.push(SavepointMarker {
            name: name.into(),
            seq_floor,
        });
    }

    pub fn rollback_to(&self, name: &str) -> Vec<Intent> {
        let mut g = self.inner.lock().unwrap();
        let Some(pos) = g.markers.iter().position(|m| m.name == name) else {
            return Vec::new();
        };
        let floor = g.markers[pos].seq_floor;
        // Drop savepoints strictly after this one.
        g.markers.truncate(pos + 1);
        let dropped: Vec<Intent> = g
            .intents
            .iter()
            .filter(|i| i.seq > floor)
            .cloned()
            .collect();
        g.intents.retain(|i| i.seq <= floor);
        dropped
    }

    pub fn release_savepoint(&self, name: &str) -> bool {
        let mut g = self.inner.lock().unwrap();
        if let Some(pos) = g.markers.iter().position(|m| m.name == name) {
            g.markers.remove(pos);
            true
        } else {
            false
        }
    }

    pub fn intent_count(&self) -> usize {
        self.inner.lock().unwrap().intents.len()
    }

    pub fn savepoints(&self) -> Vec<String> {
        self.inner
            .lock()
            .unwrap()
            .markers
            .iter()
            .map(|m| m.name.clone())
            .collect()
    }

    pub fn snapshot(&self) -> BTreeMap<Vec<u8>, u64> {
        self.inner
            .lock()
            .unwrap()
            .intents
            .iter()
            .map(|i| (i.key.clone(), i.seq))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(s: &str) -> Vec<u8> {
        s.as_bytes().to_vec()
    }

    #[test]
    fn intents_track_sequence() {
        let m = DistSavepointManager::new();
        m.record_intent(k("a"));
        m.record_intent(k("b"));
        assert_eq!(m.intent_count(), 2);
    }

    #[test]
    fn rollback_drops_intents_after_savepoint() {
        let m = DistSavepointManager::new();
        m.record_intent(k("before"));
        m.savepoint("sp1");
        m.record_intent(k("after"));
        let dropped = m.rollback_to("sp1");
        assert_eq!(dropped.len(), 1);
        assert_eq!(dropped[0].key, k("after"));
        assert_eq!(m.intent_count(), 1);
    }

    #[test]
    fn nested_savepoints_truncate() {
        let m = DistSavepointManager::new();
        m.record_intent(k("a"));
        m.savepoint("s1");
        m.record_intent(k("b"));
        m.savepoint("s2");
        m.record_intent(k("c"));
        m.rollback_to("s1");
        assert_eq!(m.savepoints(), vec!["s1".to_string()]);
        assert_eq!(m.intent_count(), 1);
    }

    #[test]
    fn release_drops_marker_only() {
        let m = DistSavepointManager::new();
        m.record_intent(k("a"));
        m.savepoint("s1");
        m.record_intent(k("b"));
        assert!(m.release_savepoint("s1"));
        assert_eq!(m.intent_count(), 2);
        assert!(m.savepoints().is_empty());
    }

    #[test]
    fn unknown_savepoint_rollback_is_noop() {
        let m = DistSavepointManager::new();
        m.record_intent(k("a"));
        let dropped = m.rollback_to("ghost");
        assert!(dropped.is_empty());
        assert_eq!(m.intent_count(), 1);
    }

    #[test]
    fn snapshot_returns_current_state() {
        let m = DistSavepointManager::new();
        m.record_intent(k("a"));
        m.record_intent(k("b"));
        let s = m.snapshot();
        assert_eq!(s.len(), 2);
    }
}
