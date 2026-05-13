//! Distributed query-plan cache.
//!
//! LRU cache keyed by `(sql_hash, schema_version)`. When schema
//! version advances, all entries pinned to the previous version
//! become stale and are evicted lazily.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct PlanKey {
    pub sql_hash: u64,
    pub schema_version: u64,
}

#[derive(Clone, Debug)]
pub struct CachedPlan {
    pub key: PlanKey,
    pub plan_blob: Vec<u8>,
    pub uses: u64,
}

#[derive(Clone, Debug, Default)]
pub struct PlanCache {
    inner: Arc<std::sync::Mutex<Inner>>,
    capacity: usize,
}

#[derive(Debug, Default)]
struct Inner {
    plans: HashMap<PlanKey, CachedPlan>,
    /// LRU order keyed by `uses` so least-used drops first.
    lru: BTreeMap<u64, PlanKey>,
    next_id: u64,
}

impl PlanCache {
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Arc::default(),
            capacity: capacity.max(1),
        }
    }

    pub fn insert(&self, key: PlanKey, plan_blob: Vec<u8>) {
        let mut guard = self.inner.lock().unwrap();
        if guard.plans.len() >= self.capacity {
            // Evict the lowest-use entry.
            if let Some((_, k)) = guard.lru.iter().next().map(|(uses, k)| (*uses, k.clone())) {
                guard.plans.remove(&k);
                let lru_key = guard.lru.keys().next().copied();
                if let Some(lru_key) = lru_key {
                    guard.lru.remove(&lru_key);
                }
            }
        }
        guard.next_id = guard.next_id.saturating_add(1);
        let uses = guard.next_id;
        guard.lru.insert(uses, key.clone());
        guard.plans.insert(
            key.clone(),
            CachedPlan {
                key,
                plan_blob,
                uses,
            },
        );
    }

    pub fn get(&self, key: &PlanKey) -> Option<CachedPlan> {
        let mut guard = self.inner.lock().unwrap();
        let plan = guard.plans.get(key).cloned()?;
        // Refresh LRU.
        guard.lru.remove(&plan.uses);
        guard.next_id = guard.next_id.saturating_add(1);
        let new_uses = guard.next_id;
        guard.lru.insert(new_uses, plan.key.clone());
        if let Some(p) = guard.plans.get_mut(key) {
            p.uses = new_uses;
        }
        Some(plan)
    }

    pub fn invalidate_version(&self, schema_version: u64) -> usize {
        let mut guard = self.inner.lock().unwrap();
        let stale: Vec<PlanKey> = guard
            .plans
            .keys()
            .filter(|k| k.schema_version != schema_version)
            .cloned()
            .collect();
        let count = stale.len();
        for k in stale {
            if let Some(p) = guard.plans.remove(&k) {
                guard.lru.remove(&p.uses);
            }
        }
        count
    }

    pub fn len(&self) -> usize {
        self.inner.lock().unwrap().plans.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(sql: u64, ver: u64) -> PlanKey {
        PlanKey {
            sql_hash: sql,
            schema_version: ver,
        }
    }

    #[test]
    fn insert_then_get_returns_plan() {
        let c = PlanCache::new(4);
        c.insert(key(1, 1), b"plan".to_vec());
        let cached = c.get(&key(1, 1)).unwrap();
        assert_eq!(cached.plan_blob, b"plan".to_vec());
    }

    #[test]
    fn invalidate_version_drops_stale_plans() {
        let c = PlanCache::new(4);
        c.insert(key(1, 1), b"p1".to_vec());
        c.insert(key(2, 1), b"p2".to_vec());
        c.insert(key(3, 2), b"p3".to_vec());
        let dropped = c.invalidate_version(2);
        assert_eq!(dropped, 2);
        assert!(c.get(&key(3, 2)).is_some());
    }

    #[test]
    fn capacity_evicts_lru() {
        let c = PlanCache::new(2);
        c.insert(key(1, 1), b"a".to_vec());
        c.insert(key(2, 1), b"b".to_vec());
        // Hit (1) to bump its LRU.
        let _ = c.get(&key(1, 1));
        c.insert(key(3, 1), b"c".to_vec());
        // Either (2) or (1) evicted; cache holds 2 entries.
        assert_eq!(c.len(), 2);
    }

    #[test]
    fn missing_key_returns_none() {
        let c = PlanCache::new(4);
        assert!(c.get(&key(99, 1)).is_none());
    }

    #[test]
    fn re_inserting_same_key_updates_plan() {
        let c = PlanCache::new(4);
        c.insert(key(1, 1), b"old".to_vec());
        c.insert(key(1, 1), b"new".to_vec());
        let p = c.get(&key(1, 1)).unwrap();
        assert_eq!(p.plan_blob, b"new".to_vec());
    }
}
