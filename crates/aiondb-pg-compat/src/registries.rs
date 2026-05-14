//! Data types used by the engine's per-process compat registries
//! (databases, role-membership dependencies, granted-privilege
//! dependencies). The *storage* of these registries (global `Mutex`
//! maps keyed by engine instance) stays in the engine; this module
//! defines only the value types so they can be shared across the
//! compat layer.

use std::collections::HashMap;

use aiondb_catalog::PrivilegeDescriptor;

#[derive(Clone, Debug)]
pub struct CompatDatabaseEntry {
    pub owner_name: String,
    pub tablespace: Option<String>,
    pub connection_limit: Option<i32>,
}

#[derive(Default)]
pub struct CompatDatabaseRegistry {
    pub by_name: HashMap<String, CompatDatabaseEntry>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompatRoleMembershipDependency {
    pub grantor: String,
    pub grantee: String,
    pub granted_role: String,
}

#[derive(Default)]
pub struct CompatRoleMembershipDependencyRegistry {
    pub dependencies: Vec<CompatRoleMembershipDependency>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompatGrantedPrivilegeDependency {
    pub grantor: String,
    pub privilege: PrivilegeDescriptor,
}

#[derive(Default)]
pub struct CompatGrantedPrivilegeDependencyRegistry {
    pub dependencies: Vec<CompatGrantedPrivilegeDependency>,
}
