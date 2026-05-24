//! Cluster-level shared roles (pg_authid / pg_roles).

use serde::{Deserialize, Serialize};

/// Descriptor for a role visible from every database in the cluster.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
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

impl std::fmt::Debug for ClusterRoleDescriptor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClusterRoleDescriptor")
            .field("name", &self.name)
            .field("can_login", &self.can_login)
            .field("is_superuser", &self.is_superuser)
            .field("can_create_db", &self.can_create_db)
            .field("can_create_role", &self.can_create_role)
            .field("inherit_privileges", &self.inherit_privileges)
            .field("connection_limit", &self.connection_limit)
            .field("valid_until_unix_secs", &self.valid_until_unix_secs)
            .field(
                "password_hash",
                &self.password_hash.as_ref().map(|_| "<redacted>"),
            )
            .finish()
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_redacts_password_hash() {
        let mut role = ClusterRoleDescriptor::superuser("admin");
        role.password_hash = Some("SCRAM-SHA-256$secret-verifier".to_owned());

        let debug = format!("{role:?}");

        assert!(!debug.contains("secret-verifier"));
        assert!(debug.contains("redacted"));
    }
}
