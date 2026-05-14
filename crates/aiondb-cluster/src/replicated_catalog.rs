//! Replicated catalog descriptor.
//!
//! Cluster-wide table/index metadata replicated via the control
//! plane. Each descriptor carries a version number that the planner
//! consults to invalidate cached plans.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TableDescriptor {
    pub table_id: u64,
    pub name: String,
    pub version: u64,
    pub columns: Vec<ColumnDescriptor>,
    pub indexes: Vec<IndexDescriptor>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ColumnDescriptor {
    pub id: u32,
    pub name: String,
    pub data_type: String,
    pub nullable: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct IndexDescriptor {
    pub id: u32,
    pub name: String,
    pub column_ids: Vec<u32>,
    pub unique: bool,
}

#[derive(Clone, Debug, Default)]
pub struct ReplicatedCatalog {
    inner: Arc<std::sync::RwLock<BTreeMap<u64, TableDescriptor>>>,
}

impl ReplicatedCatalog {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn upsert(&self, mut table: TableDescriptor) -> u64 {
        let mut guard = self.inner.write().unwrap();
        let prev_version = guard.get(&table.table_id).map(|t| t.version).unwrap_or(0);
        table.version = table.version.max(prev_version).saturating_add(1);
        let v = table.version;
        guard.insert(table.table_id, table);
        v
    }

    pub fn get(&self, table_id: u64) -> Option<TableDescriptor> {
        self.inner.read().unwrap().get(&table_id).cloned()
    }

    pub fn drop_table(&self, table_id: u64) -> Option<TableDescriptor> {
        self.inner.write().unwrap().remove(&table_id)
    }

    pub fn list(&self) -> Vec<TableDescriptor> {
        self.inner.read().unwrap().values().cloned().collect()
    }

    pub fn table_count(&self) -> usize {
        self.inner.read().unwrap().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn column(id: u32, name: &str) -> ColumnDescriptor {
        ColumnDescriptor {
            id,
            name: name.into(),
            data_type: "int".into(),
            nullable: false,
        }
    }

    fn descriptor(id: u64, name: &str) -> TableDescriptor {
        TableDescriptor {
            table_id: id,
            name: name.into(),
            version: 0,
            columns: vec![column(1, "id"), column(2, "name")],
            indexes: vec![IndexDescriptor {
                id: 1,
                name: "pk".into(),
                column_ids: vec![1],
                unique: true,
            }],
        }
    }

    #[test]
    fn upsert_advances_version() {
        let c = ReplicatedCatalog::new();
        let v1 = c.upsert(descriptor(7, "users"));
        let v2 = c.upsert(descriptor(7, "users"));
        assert!(v2 > v1);
    }

    #[test]
    fn list_returns_every_table() {
        let c = ReplicatedCatalog::new();
        c.upsert(descriptor(1, "a"));
        c.upsert(descriptor(2, "b"));
        let all = c.list();
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn drop_table_removes_from_catalog() {
        let c = ReplicatedCatalog::new();
        c.upsert(descriptor(1, "a"));
        assert!(c.drop_table(1).is_some());
        assert!(c.get(1).is_none());
    }

    #[test]
    fn version_starts_at_one_for_fresh_table() {
        let c = ReplicatedCatalog::new();
        let v = c.upsert(descriptor(99, "fresh"));
        assert_eq!(v, 1);
    }

    #[test]
    fn version_resilient_to_provided_value() {
        let c = ReplicatedCatalog::new();
        let mut t = descriptor(99, "fresh");
        t.version = 500;
        let v = c.upsert(t);
        assert_eq!(v, 501);
    }
}
