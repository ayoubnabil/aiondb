//! Cluster-level shared roles (pg_authid / pg_roles).

use serde::{Deserialize, Serialize};

/// Descriptor for a role visible from every database in the cluster.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClusterRoleDescriptor {
    pub name: String,
    pub can_login: bool,
    pub is_superuser: bool,
    pub can_create_db: bool,
    pub can_create_role: bool,
    pub inherit_privileges: bool,
    pub connection_limit: Option<i32>,
    pub valid_until_unix_secs: Option<u64>,
    pub password_hash: Option<String>,
}

impl ClusterRoleDescriptor {
    pub fn superuser(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            can_login: true,
            is_superuser: true,
            can_create_db: true,
            can_create_role: true,
            inherit_privileges: true,
            connection_limit: None,
            valid_until_unix_secs: None,
            password_hash: None,
        }
    }
}
