//! LWW-Map CRDT.
//!
//! Map keyed by `K` whose values are LWW registers. Each write
//! carries a timestamp; on merge, the newer timestamp wins per key.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::lww_register::LwwTimestamp;

#[derive(Clone, Debug)]
pub struct LwwMap<K: Clone + Ord + Eq, V: Clone> {
    inner: Arc<std::sync::Mutex<BTreeMap<K, (LwwTimestamp, V)>>>,
}

impl<K: Clone + Ord + Eq, V: Clone> Default for LwwMap<K, V> {
    fn default() -> Self {
        Self {
            inner: Arc::new(std::sync::Mutex::new(BTreeMap::new())),
        }
    }
}

impl<K: Clone + Ord + Eq, V: Clone> LwwMap<K, V> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn put(&self, key: K, ts: LwwTimestamp, value: V) {
        let mut guard = self.inner.lock().unwrap();
        let should_write = match guard.get(&key) {
            Some((current_ts, _)) => ts > *current_ts,
            None => true,
        };
        if should_write {
            guard.insert(key, (ts, value));
        }
    }

    pub fn get(&self, key: &K) -> Option<(LwwTimestamp, V)> {
        self.inner.lock().unwrap().get(key).cloned()
    }

    pub fn entries(&self) -> Vec<(K, LwwTimestamp, V)> {
        self.inner
            .lock()
            .unwrap()
            .iter()
            .map(|(k, (ts, v))| (k.clone(), *ts, v.clone()))
            .collect()
    }

    pub fn merge(&self, other: &LwwMap<K, V>) {
        let other_state = other.inner.lock().unwrap();
        let mut guard = self.inner.lock().unwrap();
        for (key, (ts, value)) in other_state.iter() {
            let should_write = match guard.get(key) {
                Some((current_ts, _)) => *ts > *current_ts,
                None => true,
            };
            if should_write {
                guard.insert(key.clone(), (*ts, value.clone()));
            }
        }
    }

    pub fn len(&self) -> usize {
        self.inner.lock().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(wall: u64) -> LwwTimestamp {
        LwwTimestamp {
            wall_us: wall,
            logical: 0,
            writer_id: 1,
        }
    }

    #[test]
    fn put_then_get_returns_value() {
        let m: LwwMap<&'static str, u32> = LwwMap::new();
        m.put("a", ts(100), 1);
        let (_, v) = m.get(&"a").unwrap();
        assert_eq!(v, 1);
    }

    #[test]
    fn newer_put_overwrites() {
        let m: LwwMap<&'static str, u32> = LwwMap::new();
        m.put("a", ts(100), 1);
        m.put("a", ts(200), 2);
        assert_eq!(m.get(&"a").unwrap().1, 2);
    }

    #[test]
    fn older_put_ignored() {
        let m: LwwMap<&'static str, u32> = LwwMap::new();
        m.put("a", ts(200), 2);
        m.put("a", ts(100), 1);
        assert_eq!(m.get(&"a").unwrap().1, 2);
    }

    #[test]
    fn merge_takes_newest_per_key() {
        let a: LwwMap<&'static str, u32> = LwwMap::new();
        a.put("k1", ts(100), 1);
        a.put("k2", ts(200), 2);
        let b: LwwMap<&'static str, u32> = LwwMap::new();
        b.put("k1", ts(300), 99);
        b.put("k3", ts(100), 3);
        a.merge(&b);
        assert_eq!(a.get(&"k1").unwrap().1, 99);
        assert_eq!(a.get(&"k2").unwrap().1, 2);
        assert_eq!(a.get(&"k3").unwrap().1, 3);
    }

    #[test]
    fn entries_returns_all_keys() {
        let m: LwwMap<&'static str, u32> = LwwMap::new();
        m.put("a", ts(1), 1);
        m.put("b", ts(2), 2);
        assert_eq!(m.entries().len(), 2);
    }
}
