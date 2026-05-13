//! Database-scoped contracts + default in-memory implementation.
//!
//! The traits below are the stable entry points for the ADR-0014 contract.
//! Existing layers (`aiondb-catalog`, `aiondb-storage-api`) continue to
//! live as they are - the `DatabaseCatalog` / `DatabaseStorage` traits
//! serve as **markers** that the engine uses to route a `DatabaseId` to
//! the correct concrete instance.

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use aiondb_core::{DbError, DbResult, SqlState};

use crate::descriptor::{CreateDatabaseRequest, DatabaseDescriptor};
use crate::id::{DatabaseId, TablespaceId};
use crate::role::ClusterRoleDescriptor;

/// Marker for base-scoped `CatalogReader + CatalogWriter +
/// CatalogTxnParticipant` implementations.
///
/// Phase 1 (ADR-0014): this trait imposes no methods - it exists to
/// materialize the isolation boundary in handle signatures. Later phases
/// will add `database_id()`, `isolation_guard()`, etc.
pub trait DatabaseCatalog: Send + Sync + std::fmt::Debug {
    /// Identity of the database this catalog is scoped to.
    fn database_id(&self) -> DatabaseId;
}

/// Equivalent marker for storage (heap + index + WAL dedicated to a
/// database).
pub trait DatabaseStorage: Send + Sync + std::fmt::Debug {
    fn database_id(&self) -> DatabaseId;
}

/// Handle runtime qu'un moteur garde par base active.
#[derive(Clone)]
pub struct DatabaseHandle {
    pub descriptor: DatabaseDescriptor,
    pub catalog: Arc<dyn DatabaseCatalog>,
    pub storage: Arc<dyn DatabaseStorage>,
}

impl std::fmt::Debug for DatabaseHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DatabaseHandle")
            .field("id", &self.descriptor.id)
            .field("name", &self.descriptor.name)
            .finish()
    }
}

/// Cluster-level catalog: list of databases + shared roles.
///
/// This interface is **the** source of truth for "which databases exist".
/// Callers never manipulate the underlying `pg_database` table directly -
/// they go through this trait.
pub trait ClusterCatalog: Send + Sync + std::fmt::Debug {
    fn list_databases(&self) -> DbResult<Vec<DatabaseDescriptor>>;
    fn get_database_by_id(&self, id: DatabaseId) -> DbResult<Option<DatabaseDescriptor>>;
    fn get_database_by_name(&self, name: &str) -> DbResult<Option<DatabaseDescriptor>>;
    fn create_database(&self, req: CreateDatabaseRequest) -> DbResult<DatabaseDescriptor>;
    fn drop_database(&self, id: DatabaseId) -> DbResult<()>;
    fn rename_database(&self, id: DatabaseId, new_name: String) -> DbResult<()>;

    /// ALTER DATABASE OWNER TO ...
    fn set_database_owner(&self, id: DatabaseId, new_owner: String) -> DbResult<()>;
    /// ALTER DATABASE SET TABLESPACE ... (None = RESET)
    fn set_database_tablespace(
        &self,
        id: DatabaseId,
        tablespace: Option<TablespaceId>,
    ) -> DbResult<()>;
    /// `ALTER DATABASE CONNECTION LIMIT <n>` (`None` = unlimited/-1).
    fn set_database_connection_limit(&self, id: DatabaseId, limit: Option<i32>) -> DbResult<()>;
    /// `ALTER DATABASE ALLOW_CONNECTIONS <bool>`.
    fn set_database_allow_connections(&self, id: DatabaseId, allow: bool) -> DbResult<()>;
    /// `ALTER DATABASE IS_TEMPLATE <bool>`.
    fn set_database_is_template(&self, id: DatabaseId, is_template: bool) -> DbResult<()>;

    // Roles
    fn list_roles(&self) -> DbResult<Vec<ClusterRoleDescriptor>>;
    fn get_role_by_name(&self, name: &str) -> DbResult<Option<ClusterRoleDescriptor>>;
    fn upsert_role(&self, role: ClusterRoleDescriptor) -> DbResult<()>;
    fn drop_role(&self, name: &str) -> DbResult<()>;
}

/// In-memory implementation. No on-disk persistence; serves as:
///
/// 1. Scaffold for Engine during phase 1.
/// 2. Backend for tests.
/// 3. Support for `CREATE DATABASE` before durability - databases are
///    lost on restart until a disk backend is added.
#[derive(Debug, Default)]
pub struct InMemoryClusterCatalog {
    inner: RwLock<ClusterState>,
}

#[derive(Debug, Default)]
struct ClusterState {
    databases: BTreeMap<DatabaseId, DatabaseDescriptor>,
    next_id: Option<DatabaseId>,
    roles: BTreeMap<String, ClusterRoleDescriptor>,
}

impl InMemoryClusterCatalog {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers the default database. Idempotent.
    pub fn bootstrap_default(&self, owner: impl Into<String>) -> DbResult<DatabaseDescriptor> {
        let desc = DatabaseDescriptor::default_bootstrap(owner);
        let mut guard = self.lock_write()?;
        guard
            .databases
            .entry(desc.id)
            .or_insert_with(|| desc.clone());
        guard.next_id = Some(DatabaseId::new(
            guard
                .databases
                .keys()
                .max()
                .map(|id| id.get())
                .unwrap_or(DatabaseId::DEFAULT.get())
                .saturating_add(1),
        ));
        Ok(desc)
    }

    fn lock_read(&self) -> DbResult<std::sync::RwLockReadGuard<'_, ClusterState>> {
        self.inner
            .read()
            .map_err(|_| DbError::internal("cluster catalog poisoned"))
    }

    fn lock_write(&self) -> DbResult<std::sync::RwLockWriteGuard<'_, ClusterState>> {
        self.inner
            .write()
            .map_err(|_| DbError::internal("cluster catalog poisoned"))
    }
}

fn mutate_descriptor<F>(cat: &InMemoryClusterCatalog, id: DatabaseId, apply: F) -> DbResult<()>
where
    F: FnOnce(&mut DatabaseDescriptor) -> DbResult<()>,
{
    let mut guard = cat.lock_write()?;
    let desc = guard.databases.get_mut(&id).ok_or_else(|| {
        DbError::bind_error(
            SqlState::InvalidCatalogName,
            format!("database id {id} does not exist"),
        )
    })?;
    apply(desc)
}

impl ClusterCatalog for InMemoryClusterCatalog {
    fn list_databases(&self) -> DbResult<Vec<DatabaseDescriptor>> {
        Ok(self.lock_read()?.databases.values().cloned().collect())
    }

    fn get_database_by_id(&self, id: DatabaseId) -> DbResult<Option<DatabaseDescriptor>> {
        Ok(self.lock_read()?.databases.get(&id).cloned())
    }

    fn get_database_by_name(&self, name: &str) -> DbResult<Option<DatabaseDescriptor>> {
        let guard = self.lock_read()?;
        Ok(guard
            .databases
            .values()
            .find(|d| d.name.eq_ignore_ascii_case(name))
            .cloned())
    }

    fn create_database(&self, req: CreateDatabaseRequest) -> DbResult<DatabaseDescriptor> {
        let mut guard = self.lock_write()?;
        if guard
            .databases
            .values()
            .any(|d| d.name.eq_ignore_ascii_case(&req.name))
        {
            return Err(DbError::bind_error(
                SqlState::DuplicateObject,
                format!("database \"{}\" already exists", req.name),
            ));
        }
        let id = guard.next_id.unwrap_or(DatabaseId::DEFAULT);
        let desc = DatabaseDescriptor {
            id,
            name: req.name,
            owner: req.owner,
            encoding: req.encoding.unwrap_or_else(|| "UTF8".to_owned()),
            collate: req.collate.unwrap_or_else(|| "C".to_owned()),
            ctype: req.ctype.unwrap_or_else(|| "C".to_owned()),
            tablespace_id: req.tablespace_id,
            connection_limit: req.connection_limit,
            is_template: req.is_template,
            allow_connections: req.allow_connections,
            created_at: std::time::SystemTime::now(),
        };
        guard.databases.insert(id, desc.clone());
        guard.next_id = Some(id.next());
        Ok(desc)
    }

    fn drop_database(&self, id: DatabaseId) -> DbResult<()> {
        let mut guard = self.lock_write()?;
        if id.is_default() {
            return Err(DbError::bind_error(
                SqlState::ObjectNotInPrerequisiteState,
                "cannot drop built-in default database",
            ));
        }
        if guard.databases.remove(&id).is_none() {
            return Err(DbError::bind_error(
                SqlState::InvalidCatalogName,
                format!("database id {id} does not exist"),
            ));
        }
        Ok(())
    }

    fn rename_database(&self, id: DatabaseId, new_name: String) -> DbResult<()> {
        let mut guard = self.lock_write()?;
        if id.is_default() {
            return Err(DbError::bind_error(
                SqlState::ObjectNotInPrerequisiteState,
                "cannot rename built-in default database",
            ));
        }
        if guard
            .databases
            .values()
            .any(|d| d.id != id && d.name.eq_ignore_ascii_case(&new_name))
        {
            return Err(DbError::bind_error(
                SqlState::DuplicateObject,
                format!("database \"{new_name}\" already exists"),
            ));
        }
        let desc = guard.databases.get_mut(&id).ok_or_else(|| {
            DbError::bind_error(
                SqlState::InvalidCatalogName,
                format!("database id {id} does not exist"),
            )
        })?;
        desc.name = new_name;
        Ok(())
    }

    fn set_database_owner(&self, id: DatabaseId, new_owner: String) -> DbResult<()> {
        mutate_descriptor(self, id, |desc| {
            desc.owner = new_owner;
            Ok(())
        })
    }

    fn set_database_tablespace(
        &self,
        id: DatabaseId,
        tablespace: Option<TablespaceId>,
    ) -> DbResult<()> {
        mutate_descriptor(self, id, |desc| {
            desc.tablespace_id = tablespace;
            Ok(())
        })
    }

    fn set_database_connection_limit(&self, id: DatabaseId, limit: Option<i32>) -> DbResult<()> {
        mutate_descriptor(self, id, |desc| {
            desc.connection_limit = limit;
            Ok(())
        })
    }

    fn set_database_allow_connections(&self, id: DatabaseId, allow: bool) -> DbResult<()> {
        mutate_descriptor(self, id, |desc| {
            desc.allow_connections = allow;
            Ok(())
        })
    }

    fn set_database_is_template(&self, id: DatabaseId, is_template: bool) -> DbResult<()> {
        mutate_descriptor(self, id, |desc| {
            desc.is_template = is_template;
            Ok(())
        })
    }

    fn list_roles(&self) -> DbResult<Vec<ClusterRoleDescriptor>> {
        Ok(self.lock_read()?.roles.values().cloned().collect())
    }

    fn get_role_by_name(&self, name: &str) -> DbResult<Option<ClusterRoleDescriptor>> {
        let guard = self.lock_read()?;
        Ok(guard
            .roles
            .values()
            .find(|r| r.name.eq_ignore_ascii_case(name))
            .cloned())
    }

    fn upsert_role(&self, role: ClusterRoleDescriptor) -> DbResult<()> {
        let mut guard = self.lock_write()?;
        guard.roles.insert(role.name.to_ascii_lowercase(), role);
        Ok(())
    }

    fn drop_role(&self, name: &str) -> DbResult<()> {
        let mut guard = self.lock_write()?;
        guard.roles.remove(&name.to_ascii_lowercase());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bootstrap_is_idempotent() {
        let cat = InMemoryClusterCatalog::new();
        let a = cat.bootstrap_default("alice").unwrap();
        let b = cat.bootstrap_default("alice").unwrap();
        assert_eq!(a.id, b.id);
        assert_eq!(cat.list_databases().unwrap().len(), 1);
    }

    #[test]
    fn create_allocates_next_id() {
        let cat = InMemoryClusterCatalog::new();
        cat.bootstrap_default("root").unwrap();
        let desc = cat
            .create_database(CreateDatabaseRequest::simple("analytics", "alice"))
            .unwrap();
        assert_eq!(desc.id.get(), 2);
        assert_eq!(desc.owner, "alice");
    }

    #[test]
    fn create_rejects_duplicate_case_insensitive() {
        let cat = InMemoryClusterCatalog::new();
        cat.bootstrap_default("root").unwrap();
        cat.create_database(CreateDatabaseRequest::simple("Foo", "alice"))
            .unwrap();
        let err = cat
            .create_database(CreateDatabaseRequest::simple("foo", "bob"))
            .unwrap_err();
        match err {
            DbError::Bind(rep) => assert_eq!(rep.sqlstate, SqlState::DuplicateObject),
            _ => panic!("expected bind error"),
        }
    }

    #[test]
    fn drop_rejects_default() {
        let cat = InMemoryClusterCatalog::new();
        cat.bootstrap_default("root").unwrap();
        let err = cat.drop_database(DatabaseId::DEFAULT).unwrap_err();
        match err {
            DbError::Bind(rep) => {
                assert_eq!(rep.sqlstate, SqlState::ObjectNotInPrerequisiteState)
            }
            _ => panic!("expected bind error"),
        }
    }

    #[test]
    fn rename_collision_rejected() {
        let cat = InMemoryClusterCatalog::new();
        cat.bootstrap_default("root").unwrap();
        let db1 = cat
            .create_database(CreateDatabaseRequest::simple("db1", "alice"))
            .unwrap();
        cat.create_database(CreateDatabaseRequest::simple("db2", "alice"))
            .unwrap();
        let err = cat.rename_database(db1.id, "db2".to_owned()).unwrap_err();
        match err {
            DbError::Bind(rep) => assert_eq!(rep.sqlstate, SqlState::DuplicateObject),
            _ => panic!("expected bind error"),
        }
    }

    #[test]
    fn alter_owner_and_connection_limit_persist() {
        let cat = InMemoryClusterCatalog::new();
        cat.bootstrap_default("root").unwrap();
        let db = cat
            .create_database(CreateDatabaseRequest::simple("analytics", "alice"))
            .unwrap();

        cat.set_database_owner(db.id, "bob".to_owned()).unwrap();
        cat.set_database_connection_limit(db.id, Some(42)).unwrap();
        cat.set_database_tablespace(db.id, Some(TablespaceId::PG_DEFAULT))
            .unwrap();
        cat.set_database_allow_connections(db.id, false).unwrap();
        cat.set_database_is_template(db.id, true).unwrap();

        let reloaded = cat.get_database_by_id(db.id).unwrap().unwrap();
        assert_eq!(reloaded.owner, "bob");
        assert_eq!(reloaded.connection_limit, Some(42));
        assert_eq!(reloaded.tablespace_id, Some(TablespaceId::PG_DEFAULT));
        assert!(!reloaded.allow_connections);
        assert!(reloaded.is_template);
    }

    #[test]
    fn alter_unknown_database_rejected() {
        let cat = InMemoryClusterCatalog::new();
        cat.bootstrap_default("root").unwrap();
        let err = cat
            .set_database_owner(DatabaseId::new(9999), "bob".to_owned())
            .unwrap_err();
        match err {
            DbError::Bind(rep) => assert_eq!(rep.sqlstate, SqlState::InvalidCatalogName),
            _ => panic!("expected bind error"),
        }
    }

    #[test]
    fn roles_upsert_and_get() {
        let cat = InMemoryClusterCatalog::new();
        cat.upsert_role(ClusterRoleDescriptor::superuser("postgres"))
            .unwrap();
        let role = cat.get_role_by_name("POSTGRES").unwrap().unwrap();
        assert!(role.is_superuser);
    }

    #[test]
    fn drop_database_then_create_reallocates() {
        let cat = InMemoryClusterCatalog::new();
        cat.bootstrap_default("root").unwrap();
        let first = cat
            .create_database(CreateDatabaseRequest::simple("t", "alice"))
            .unwrap();
        cat.drop_database(first.id).unwrap();
        let second = cat
            .create_database(CreateDatabaseRequest::simple("t", "alice"))
            .unwrap();
        // New id, not recycled.
        assert_ne!(first.id, second.id);
    }
}
