//! Typed identities.

use serde::{Deserialize, Serialize};

/// Stable database identity within the cluster.
///
/// `DatabaseId::DEFAULT` (= `1`) is reserved for the database bootstrapped
/// by the engine at startup. `DatabaseId::CLUSTER` (= `0`) is reserved for
/// cluster-level operations (roles, pg_database itself) - no user
/// transaction runs on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct DatabaseId(u32);

impl DatabaseId {
    pub const CLUSTER: Self = Self(0);
    pub const DEFAULT: Self = Self(1);

    pub const fn new(value: u32) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u32 {
        self.0
    }

    pub const fn is_default(self) -> bool {
        self.0 == Self::DEFAULT.0
    }

    pub const fn is_cluster(self) -> bool {
        self.0 == Self::CLUSTER.0
    }

    pub fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }
}

impl std::fmt::Display for DatabaseId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Identity of a PG-compat tablespace. Reserved for future use; the engine
/// does not yet implement distinct tablespaces.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct TablespaceId(u32);

impl TablespaceId {
    pub const PG_DEFAULT: Self = Self(1663);
    pub const PG_GLOBAL: Self = Self(1664);

    pub const fn new(value: u32) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u32 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_one_cluster_is_zero() {
        assert_eq!(DatabaseId::DEFAULT.get(), 1);
        assert_eq!(DatabaseId::CLUSTER.get(), 0);
        assert!(DatabaseId::DEFAULT.is_default());
        assert!(!DatabaseId::DEFAULT.is_cluster());
        assert!(DatabaseId::CLUSTER.is_cluster());
    }

    #[test]
    fn next_increments() {
        assert_eq!(DatabaseId::DEFAULT.next().get(), 2);
        assert_eq!(DatabaseId::new(u32::MAX).next().get(), u32::MAX);
    }

    #[test]
    fn tablespace_constants_match_pg() {
        assert_eq!(TablespaceId::PG_DEFAULT.get(), 1663);
        assert_eq!(TablespaceId::PG_GLOBAL.get(), 1664);
    }
}
