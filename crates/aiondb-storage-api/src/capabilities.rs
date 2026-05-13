/// Declares which optional storage features a backend supports.
///
/// The engine queries these capabilities at startup and at plan time
/// to decide which execution paths are available.  Every method has a
/// default returning `false`, so new backends only need to override the
/// capabilities they actually provide.
///
/// # Relationship to default trait methods
///
/// Several methods on [`StorageDML`](crate::StorageDML) and
/// [`StorageTxnParticipant`](crate::StorageTxnParticipant) have default
/// implementations that return
/// [`DbError::feature_not_supported`](aiondb_core::DbError::feature_not_supported)
/// flags declared here correspond to those optional methods:
///
/// | Capability               | Optional methods                                           |
/// |--------------------------|------------------------------------------------------------|
/// | `supports_vector_search` | [`StorageDML::vector_search`](crate::StorageDML::vector_search) |
/// | `supports_gin_search`    | [`StorageDML::gin_containment_search`](crate::StorageDML::gin_containment_search) |
/// | `supports_savepoints`    | [`StorageTxnParticipant::create_savepoint`](crate::StorageTxnParticipant::create_savepoint), [`rollback_to_savepoint`](crate::StorageTxnParticipant::rollback_to_savepoint), [`release_savepoint`](crate::StorageTxnParticipant::release_savepoint) |
/// | `supports_durability`    | WAL-based crash recovery                                   |
/// | `supports_persistent_ordered_indexes` | durable on-disk ordered index pages, not just rebuildable descriptors |
/// | `supports_vacuum`        | [`StorageDML::vacuum_table`](crate::StorageDML::vacuum_table) |
/// | `supports_statistics_logging` | [`StorageDML::log_analyze_stats`](crate::StorageDML::log_analyze_stats) |
pub trait StorageCapabilities: Send + Sync {
    /// Returns `true` if the backend supports HNSW vector similarity search.
    ///
    /// When this returns `false`, calls to
    /// [`StorageDML::vector_search`](crate::StorageDML::vector_search)
    /// will return [`DbError::feature_not_supported`](aiondb_core::DbError::feature_not_supported).
    #[must_use]
    fn supports_vector_search(&self) -> bool {
        false
    }

    /// Returns `true` if the backend supports GIN containment queries
    /// (`col @> pattern`).
    ///
    /// When this returns `false`, calls to
    /// [`StorageDML::gin_containment_search`](crate::StorageDML::gin_containment_search)
    /// will return [`DbError::feature_not_supported`](aiondb_core::DbError::feature_not_supported).
    #[must_use]
    fn supports_gin_search(&self) -> bool {
        false
    }

    /// Returns `true` if the backend supports transactional savepoints
    /// (`SAVEPOINT`, `ROLLBACK TO SAVEPOINT`, `RELEASE SAVEPOINT`).
    ///
    /// When this returns `false`, calls to
    /// [`StorageTxnParticipant::create_savepoint`](crate::StorageTxnParticipant::create_savepoint),
    /// [`rollback_to_savepoint`](crate::StorageTxnParticipant::rollback_to_savepoint),
    /// and [`release_savepoint`](crate::StorageTxnParticipant::release_savepoint)
    /// will return [`DbError::feature_not_supported`](aiondb_core::DbError::feature_not_supported).
    #[must_use]
    fn supports_savepoints(&self) -> bool {
        false
    }

    /// Returns `true` if the backend supports WAL-based durability.
    ///
    /// When this returns `false`, committed data may not survive process
    /// restarts.
    #[must_use]
    fn supports_durability(&self) -> bool {
        false
    }

    /// Returns `true` if ordered secondary indexes are physically persisted as
    /// buffer-managed on-disk index pages.
    ///
    /// This is intentionally separate from [`Self::supports_durability`]. A backend
    /// can persist table rows and index definitions through WAL/snapshots while
    /// still rebuilding ordered index structures into memory during recovery.
    #[must_use]
    fn supports_persistent_ordered_indexes(&self) -> bool {
        false
    }

    /// Returns `true` if the backend supports VACUUM dead-row cleanup.
    ///
    /// When this returns `false`,
    /// [`StorageDML::vacuum_table`](crate::StorageDML::vacuum_table)
    #[must_use]
    fn supports_vacuum(&self) -> bool {
        false
    }

    /// Returns `true` if the backend tracks and persists table statistics
    /// (e.g., via WAL).
    ///
    /// When this returns `false`, calls to
    /// [`StorageDML::log_analyze_stats`](crate::StorageDML::log_analyze_stats)
    #[must_use]
    fn supports_statistics_logging(&self) -> bool {
        false
    }

    /// Returns `true` if the backend supports adjacency index lookups for
    /// graph edge tables.
    ///
    /// When this returns `false`, calls to
    /// [`StorageDML::adjacency_lookup`](crate::StorageDML::adjacency_lookup)
    /// will return [`DbError::feature_not_supported`](aiondb_core::DbError::feature_not_supported).
    #[must_use]
    fn supports_adjacency_lookup(&self) -> bool {
        false
    }
}
