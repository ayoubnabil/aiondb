//! Cluster-level descriptors.

#![allow(clippy::doc_markdown, clippy::redundant_closure_for_method_calls)]

use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::id::{DatabaseId, TablespaceId};

/// Persisted metadata for a database. Canonical form aligned with
/// PostgreSQL pg_database.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DatabaseDescriptor {
    pub id: DatabaseId,
    pub name: String,
    pub owner: String,
    pub encoding: String,
    pub collate: String,
    pub ctype: String,
    pub tablespace_id: Option<TablespaceId>,
    pub connection_limit: Option<i32>,
    pub is_template: bool,
    pub allow_connections: bool,
    pub created_at: SystemTime,
}

impl DatabaseDescriptor {
    /// Builds the default database descriptor (bootstrap).
    /// Used by in-memory implementations at startup.
    pub fn default_bootstrap(owner: impl Into<String>) -> Self {
        Self {
            id: DatabaseId::DEFAULT,
            name: "default".to_owned(),
            owner: owner.into(),
            encoding: "UTF8".to_owned(),
            collate: "C".to_owned(),
            ctype: "C".to_owned(),
            tablespace_id: Some(TablespaceId::PG_DEFAULT),
            connection_limit: None,
            is_template: false,
            allow_connections: true,
            created_at: SystemTime::UNIX_EPOCH,
        }
    }
}

/// Database creation request submitted to `ClusterCatalog::create_database`.
#[derive(Clone, Debug)]
pub struct CreateDatabaseRequest {
    pub name: String,
    pub owner: String,
    pub template: Option<String>,
    pub encoding: Option<String>,
    pub collate: Option<String>,
    pub ctype: Option<String>,
    pub tablespace_id: Option<TablespaceId>,
    pub connection_limit: Option<i32>,
    pub is_template: bool,
    pub allow_connections: bool,
}

impl CreateDatabaseRequest {
    pub fn simple(name: impl Into<String>, owner: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            owner: owner.into(),
            template: None,
            encoding: None,
            collate: None,
            ctype: None,
            tablespace_id: None,
            connection_limit: None,
            is_template: false,
            allow_connections: true,
        }
    }
}

/// Stable on-disk serialization (future persistence).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PersistedDatabaseDescriptor {
    pub id: u32,
    pub name: String,
    pub owner: String,
    pub encoding: String,
    pub collate: String,
    pub ctype: String,
    pub tablespace_id: Option<u32>,
    pub connection_limit: Option<i32>,
    pub is_template: bool,
    pub allow_connections: bool,
    pub created_at_unix_secs: u64,
}

impl From<&DatabaseDescriptor> for PersistedDatabaseDescriptor {
    fn from(d: &DatabaseDescriptor) -> Self {
        let created_at_unix_secs = d
            .created_at
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        Self {
            id: d.id.get(),
            name: d.name.clone(),
            owner: d.owner.clone(),
            encoding: d.encoding.clone(),
            collate: d.collate.clone(),
            ctype: d.ctype.clone(),
            tablespace_id: d.tablespace_id.map(|t| t.get()),
            connection_limit: d.connection_limit,
            is_template: d.is_template,
            allow_connections: d.allow_connections,
            created_at_unix_secs,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_bootstrap_id_matches_default_constant() {
        let desc = DatabaseDescriptor::default_bootstrap("postgres");
        assert_eq!(desc.id, DatabaseId::DEFAULT);
        assert_eq!(desc.owner, "postgres");
        assert!(desc.allow_connections);
    }

    #[test]
    fn persisted_roundtrip_shape() {
        let desc = DatabaseDescriptor::default_bootstrap("alice");
        let persisted = PersistedDatabaseDescriptor::from(&desc);
        assert_eq!(persisted.id, 1);
        assert_eq!(persisted.name, "default");
        assert_eq!(persisted.tablespace_id, Some(1663));
    }
}
