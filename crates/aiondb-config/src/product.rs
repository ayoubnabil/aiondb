/// Support level exposed by the public `AionDB` v0.1 product contract.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProductSupportLevel {
    /// Explicitly part of the supported product contract.
    Supported,
    /// Deliberately absent from the supported product contract.
    Unsupported,
    /// Present internally, but not documented or supported for operators.
    InternalOnly,
}

impl ProductSupportLevel {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Supported => "supported",
            Self::Unsupported => "unsupported",
            Self::InternalOnly => "internal_only",
        }
    }
}

/// Product constraints advertised for the public `v0.1` release line.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProductConstraints {
    pub release_line: &'static str,
    pub topology: &'static str,
    pub clustering: ProductSupportLevel,
    pub distributed_execution: ProductSupportLevel,
    pub graph_storage: ProductSupportLevel,
    pub vector_hnsw_storage: ProductSupportLevel,
    pub gpu_acceleration: ProductSupportLevel,
    pub encryption_at_rest: ProductSupportLevel,
    pub backup_restore: ProductSupportLevel,
}

impl ProductConstraints {
    #[must_use]
    pub const fn v0_1() -> Self {
        Self {
            release_line: "0.1",
            topology: "single-node",
            clustering: ProductSupportLevel::Unsupported,
            distributed_execution: ProductSupportLevel::InternalOnly,
            graph_storage: ProductSupportLevel::InternalOnly,
            vector_hnsw_storage: ProductSupportLevel::InternalOnly,
            gpu_acceleration: ProductSupportLevel::InternalOnly,
            encryption_at_rest: ProductSupportLevel::Unsupported,
            backup_restore: ProductSupportLevel::Supported,
        }
    }

    #[must_use]
    pub const fn clustering_summary(self) -> &'static str {
        "Single-node only in v0.1; clustering, shard orchestration, distributed execution and failover are experimental/internal only."
    }

    #[must_use]
    pub const fn experimental_summary(self) -> &'static str {
        "Graph storage, vector HNSW storage, GPU acceleration and distributed modules are not covered by the v0.1 storage-compatibility promise."
    }

    #[must_use]
    pub const fn encryption_at_rest_summary(self) -> &'static str {
        "Persistent AionDB v0.1 data is written unencrypted on disk; no encryption-at-rest key management is provided."
    }

    #[must_use]
    pub const fn backup_restore_summary(self) -> &'static str {
        "Canonical SQL dump/restore is the supported v0.1 safety path; binary online backup and point-in-time recovery are out of scope."
    }

    #[must_use]
    pub const fn startup_warnings(self) -> [&'static str; 4] {
        [
            self.clustering_summary(),
            self.experimental_summary(),
            self.encryption_at_rest_summary(),
            self.backup_restore_summary(),
        ]
    }
}

pub const V0_1_PRODUCT_CONSTRAINTS: ProductConstraints = ProductConstraints::v0_1();

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v0_1_constraints_match_public_scope() {
        let constraints = ProductConstraints::v0_1();
        assert_eq!(constraints.release_line, "0.1");
        assert_eq!(constraints.topology, "single-node");
        assert_eq!(constraints.clustering, ProductSupportLevel::Unsupported);
        assert_eq!(
            constraints.distributed_execution,
            ProductSupportLevel::InternalOnly
        );
        assert_eq!(constraints.graph_storage, ProductSupportLevel::InternalOnly);
        assert_eq!(
            constraints.vector_hnsw_storage,
            ProductSupportLevel::InternalOnly
        );
        assert_eq!(
            constraints.gpu_acceleration,
            ProductSupportLevel::InternalOnly
        );
        assert_eq!(
            constraints.encryption_at_rest,
            ProductSupportLevel::Unsupported
        );
        assert_eq!(constraints.backup_restore, ProductSupportLevel::Supported);
    }

    #[test]
    fn support_level_strings_are_stable() {
        assert_eq!(ProductSupportLevel::Supported.as_str(), "supported");
        assert_eq!(ProductSupportLevel::Unsupported.as_str(), "unsupported");
        assert_eq!(ProductSupportLevel::InternalOnly.as_str(), "internal_only");
    }

    #[test]
    fn startup_warnings_cover_the_three_product_gaps() {
        let warnings = ProductConstraints::v0_1().startup_warnings();
        assert_eq!(warnings.len(), 4);
        assert!(warnings[0].contains("Single-node only"));
        assert!(warnings[1].contains("not covered by the v0.1 storage-compatibility promise"));
        assert!(warnings[2].contains("unencrypted on disk"));
        assert!(warnings[3].contains("Canonical SQL dump/restore"));
    }
}
