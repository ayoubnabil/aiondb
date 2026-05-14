//! Per-tenant catalog isolation.
//!
//! Each tenant sees only the objects (tables, indexes, sequences)
//! created in its namespace. Cross-tenant lookups are rejected.
//!
//! Used by multi-tenant deployments to provide hard isolation
//! without requiring a separate physical database per customer.

use std::collections::BTreeMap;
use std::sync::Arc;

use aiondb_core::{DbError, DbResult};

#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct TenantId(pub u64);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TenantObject {
    pub tenant: TenantId,
    pub object_id: u64,
    pub kind: &'static str,
    pub name: String,
}

#[derive(Clone, Debug, Default)]
pub struct TenantCatalog {
    inner: Arc<std::sync::RwLock<BTreeMap<(TenantId, u64), TenantObject>>>,
}

impl TenantCatalog {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&self, obj: TenantObject) {
        self.inner
            .write()
            .unwrap()
            .insert((obj.tenant, obj.object_id), obj);
    }

    pub fn lookup(&self, tenant: TenantId, object_id: u64) -> DbResult<TenantObject> {
        self.inner
            .read()
            .unwrap()
            .get(&(tenant, object_id))
            .cloned()
            .ok_or_else(|| {
                DbError::internal(format!(
                    "object {object_id} not visible to tenant {:?}",
                    tenant
                ))
            })
    }

    pub fn list_tenant_objects(&self, tenant: TenantId) -> Vec<TenantObject> {
        self.inner
            .read()
            .unwrap()
            .iter()
            .filter(|((t, _), _)| *t == tenant)
            .map(|(_, v)| v.clone())
            .collect()
    }

    pub fn cross_tenant_count(&self) -> usize {
        // Diagnostic : how many tenants have at least one object ?
        let guard = self.inner.read().unwrap();
        let mut tenants = std::collections::BTreeSet::new();
        for ((t, _), _) in guard.iter() {
            tenants.insert(*t);
        }
        tenants.len()
    }

    pub fn drop_object(&self, tenant: TenantId, object_id: u64) -> bool {
        self.inner
            .write()
            .unwrap()
            .remove(&(tenant, object_id))
            .is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obj(tenant: u64, id: u64, name: &str) -> TenantObject {
        TenantObject {
            tenant: TenantId(tenant),
            object_id: id,
            kind: "table",
            name: name.into(),
        }
    }

    #[test]
    fn lookup_within_tenant_succeeds() {
        let c = TenantCatalog::new();
        c.register(obj(1, 7, "users"));
        let o = c.lookup(TenantId(1), 7).unwrap();
        assert_eq!(o.name, "users");
    }

    #[test]
    fn cross_tenant_lookup_fails() {
        let c = TenantCatalog::new();
        c.register(obj(1, 7, "users"));
        assert!(c.lookup(TenantId(2), 7).is_err());
    }

    #[test]
    fn list_returns_only_tenant_objects() {
        let c = TenantCatalog::new();
        c.register(obj(1, 1, "a"));
        c.register(obj(1, 2, "b"));
        c.register(obj(2, 1, "c"));
        let t1 = c.list_tenant_objects(TenantId(1));
        assert_eq!(t1.len(), 2);
        let t2 = c.list_tenant_objects(TenantId(2));
        assert_eq!(t2.len(), 1);
    }

    #[test]
    fn cross_tenant_count_tracks_distinct_tenants() {
        let c = TenantCatalog::new();
        c.register(obj(1, 1, "a"));
        c.register(obj(2, 1, "b"));
        c.register(obj(3, 1, "c"));
        assert_eq!(c.cross_tenant_count(), 3);
    }

    #[test]
    fn drop_object_removes_from_catalog() {
        let c = TenantCatalog::new();
        c.register(obj(1, 1, "a"));
        assert!(c.drop_object(TenantId(1), 1));
        assert!(!c.drop_object(TenantId(1), 1)); // already gone
        assert!(c.lookup(TenantId(1), 1).is_err());
    }
}
