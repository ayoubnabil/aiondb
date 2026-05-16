#![allow(clippy::missing_errors_doc)]

use std::time::Duration;

use aiondb_core::{ColumnId, DbError, DbResult, IndexId, RelationId, Row, TupleId, TxnId, Value};
use aiondb_graph_api::{GraphStats, NeighborCursor, OwnedCursor};
use aiondb_tx::Snapshot;

use crate::{KeyRange, TupleStream};

/// Storage layer interface for data manipulation (scans, fetches, mutations).
///
/// # Required vs optional methods
///
/// Most methods on this trait are **required** and have no default
/// implementation.  A handful of methods are **optional** and provide
/// sensible defaults (either a
///
/// Optional methods are grouped at the end of this trait and clearly
/// marked.  A backend that wants to advertise support for an optional
/// capability should:
///
/// 1. Override the method(s) in this trait, **and**
/// 2. Return `true` from the corresponding
///    [`StorageCapabilities`](crate::StorageCapabilities) query method.
///
/// See [`StorageCapabilities`](crate::StorageCapabilities) for the full
/// mapping between capability flags and optional methods.
#[allow(clippy::missing_errors_doc)]
pub trait StorageDML: Send + Sync {
    // ─── Required methods ───────────────────────────────────────

    /// Monotonic generation bumped when storage-local read caches are
    /// invalidated. Higher layers can use this to key short-lived result
    /// caches without observing stale rows after writes.
    fn cache_generation(&self) -> Option<u64> {
        None
    }

    /// Load a backend-persisted graph projection cache blob for the given
    /// namespace/key/generation triple.
    ///
    /// The default implementation returns `Ok(None)`, meaning the backend
    /// does not provide durable projection cache storage.
    fn graph_projection_cache_get(
        &self,
        _namespace: &str,
        _cache_key: &str,
        _generation: u64,
    ) -> DbResult<Option<Vec<u8>>> {
        Ok(None)
    }

    /// Persist a graph projection cache blob for the given
    /// namespace/key/generation triple.
    ///
    /// The default implementation is a no-op so non-durable backends can
    /// ignore projection persistence without affecting semantics.
    fn graph_projection_cache_put(
        &self,
        _namespace: &str,
        _cache_key: &str,
        _generation: u64,
        _payload: &[u8],
    ) -> DbResult<()> {
        Ok(())
    }

    /// Apply one WAL entry shipped from a replication primary to this
    /// engine's live state. The argument is the same bytes the primary
    /// produced from `WalEntry::encode` (the wire format produced by
    /// `aiondb-wal::codec::encode_entry`). Decoding is delegated to the
    /// backend so the trait does not have to depend on `aiondb-wal`.
    ///
    /// The default implementation returns `Err(feature_not_supported)`.
    /// Backends that support replication override this. Returning an
    /// error is appropriate when the backend cannot replay incoming WAL
    /// (e.g. an in-memory cache layer).
    fn apply_replicated_wal_entry(&self, _record_bytes: &[u8]) -> DbResult<()> {
        Err(DbError::feature_not_supported(
            "this storage backend does not support applying replicated WAL records",
        ))
    }

    /// Scan all visible rows from a table. **Required.**
    fn scan_table(
        &self,
        txn: TxnId,
        snapshot: &Snapshot,
        table_id: RelationId,
        projected_columns: Option<Vec<ColumnId>>,
    ) -> DbResult<Box<dyn TupleStream>>;

    /// Scan visible rows from a table with a physical row offset and limit.
    ///
    /// The default implementation preserves semantics by truncating
    /// `scan_table`. Backends should override this for OLTP-style
    /// `LIMIT` queries so callers do not have to materialize the full table
    /// when only the first few rows are required.
    fn scan_table_limited(
        &self,
        txn: TxnId,
        snapshot: &Snapshot,
        table_id: RelationId,
        projected_columns: Option<Vec<ColumnId>>,
        offset: u64,
        limit: u64,
    ) -> DbResult<Box<dyn TupleStream>> {
        let mut stream = self.scan_table(txn, snapshot, table_id, projected_columns)?;
        let mut skipped = 0u64;
        let mut records =
            Vec::with_capacity(usize::try_from(limit).unwrap_or(usize::MAX).min(1024));
        while skipped < offset {
            if stream.next()?.is_none() {
                return Ok(Box::new(crate::VecTupleStream::new(records)));
            }
            skipped = skipped.saturating_add(1);
        }
        while u64::try_from(records.len()).unwrap_or(u64::MAX) < limit {
            let Some(record) = stream.next()? else {
                break;
            };
            records.push(record);
        }
        Ok(Box::new(crate::VecTupleStream::new(records)))
    }

    /// Scan one logical shard from a sharded table.
    ///
    /// Backends that do not implement logical table sharding should return
    fn scan_table_shard(
        &self,
        _txn: TxnId,
        _snapshot: &Snapshot,
        _table_id: RelationId,
        _shard_id: u32,
        _projected_columns: Option<Vec<ColumnId>>,
    ) -> DbResult<Box<dyn TupleStream>> {
        Err(DbError::feature_not_supported(
            "storage table shard scan is not supported",
        ))
    }

    /// Scan table rows with an equality filter pushed down to storage:
    /// `filter_column = filter_value`.
    ///
    /// Implementations may use this to avoid materializing non-matching rows.
    /// Default implementation reports unsupported so callers can fall back.
    fn scan_table_eq_filter(
        &self,
        _txn: TxnId,
        _snapshot: &Snapshot,
        _table_id: RelationId,
        _filter_column: ColumnId,
        _filter_value: &Value,
        _projected_columns: Option<Vec<ColumnId>>,
    ) -> DbResult<Box<dyn TupleStream>> {
        Err(DbError::feature_not_supported(
            "storage table equality filter pushdown is not supported",
        ))
    }

    /// Scan table rows with an equality filter, allowing the backend to stop
    /// once `max_matches` matching rows have been materialized when doing so
    /// preserves scan order.
    fn scan_table_eq_filter_limited(
        &self,
        txn: TxnId,
        snapshot: &Snapshot,
        table_id: RelationId,
        filter_column: ColumnId,
        filter_value: &Value,
        projected_columns: Option<Vec<ColumnId>>,
        max_matches: Option<u64>,
    ) -> DbResult<Box<dyn TupleStream>> {
        let _ = max_matches;
        self.scan_table_eq_filter(
            txn,
            snapshot,
            table_id,
            filter_column,
            filter_value,
            projected_columns,
        )
    }

    /// Scan table rows with a comparison-range filter pushed down to
    /// storage: `lower <= filter_column <= upper` with bounds expressed
    /// as `std::ops::Bound`. Either side may be `Unbounded` to express
    /// half-open queries (`col > x`, `col < y`).
    ///
    /// Mirrors PostgreSQL's tight `qualEval` integrated into the heap
    /// scan loop in `src/backend/access/heap/heapam.c`: the comparison
    /// happens once per tuple inside the storage scan, so non-matching
    /// rows skip the full row materialization and the generic
    /// expression-evaluator dispatch the executor would otherwise pay.
    /// The default implementation reports unsupported so callers fall
    /// back to a regular `scan_table` + executor-side filter.
    fn scan_table_range_filter(
        &self,
        _txn: TxnId,
        _snapshot: &Snapshot,
        _table_id: RelationId,
        _filter_column: ColumnId,
        _lower: std::ops::Bound<Value>,
        _upper: std::ops::Bound<Value>,
        _projected_columns: Option<Vec<ColumnId>>,
    ) -> DbResult<Box<dyn TupleStream>> {
        Err(DbError::feature_not_supported(
            "storage table range filter pushdown is not supported",
        ))
    }

    /// Scan table rows with an `IS NULL` / `IS NOT NULL` filter
    /// pushed down to storage.
    ///
    /// PG's planner turns `WHERE col IS NULL` into a special qual that
    /// only loads matching tuples; this method gives storage backends
    /// the same opportunity to skip non-matching rows without paying
    /// the executor's generic evaluator dispatch.  Default
    /// implementation reports unsupported so callers fall back to
    /// `scan_table` + executor-side filter.
    fn scan_table_null_filter(
        &self,
        _txn: TxnId,
        _snapshot: &Snapshot,
        _table_id: RelationId,
        _filter_column: ColumnId,
        _is_not_null: bool,
        _projected_columns: Option<Vec<ColumnId>>,
    ) -> DbResult<Box<dyn TupleStream>> {
        Err(DbError::feature_not_supported(
            "storage table NULL filter pushdown is not supported",
        ))
    }

    /// Scan table rows with multiple AND-combined range filters on
    /// distinct columns pushed down to storage:
    ///
    /// ```sql
    /// WHERE col_a CMP_a literal_a AND col_b CMP_b literal_b AND ...
    /// ```
    ///
    /// Mirrors PostgreSQL's `qpqual` evaluation integrated into the
    /// scan loop: every range bound is checked once per tuple inside
    /// the storage scan, so non-matching rows skip both row
    /// materialization and the executor's generic AND-of-comparison
    /// evaluator. The default implementation reports unsupported so
    /// callers fall back to `scan_table` + executor-side filter.
    fn scan_table_multi_range_filter(
        &self,
        _txn: TxnId,
        _snapshot: &Snapshot,
        _table_id: RelationId,
        _filters: &[(ColumnId, std::ops::Bound<Value>, std::ops::Bound<Value>)],
        _projected_columns: Option<Vec<ColumnId>>,
    ) -> DbResult<Box<dyn TupleStream>> {
        Err(DbError::feature_not_supported(
            "storage table multi-range filter pushdown is not supported",
        ))
    }

    /// Scan table rows with an `IN` filter pushed down to storage.
    fn scan_table_in_filter(
        &self,
        _txn: TxnId,
        _snapshot: &Snapshot,
        _table_id: RelationId,
        _filter_column: ColumnId,
        _filter_values: &[Value],
        _projected_columns: Option<Vec<ColumnId>>,
    ) -> DbResult<Box<dyn TupleStream>> {
        Err(DbError::feature_not_supported(
            "storage table IN filter pushdown is not supported",
        ))
    }

    /// Return a cheap visible-row count when the backend can prove it without
    /// scanning. Callers must fall back to `scan_table` when unsupported.
    fn visible_row_count(
        &self,
        _txn: TxnId,
        _snapshot: &Snapshot,
        _table_id: RelationId,
    ) -> DbResult<u64> {
        Err(DbError::feature_not_supported(
            "storage visible row count is not supported",
        ))
    }

    /// Returns `Some(true)` when the backend can prove via cheap
    /// metadata (e.g. a per-`(ordinal, value)` count map maintained
    /// alongside the heap) that NO visible row of `table_id` matches
    /// the AND-of-bounds described by `column_predicates`. Returns
    /// `Some(false)` when at least one row may match. Returns `None`
    /// when the backend cannot answer authoritatively (concurrent
    /// txns, paged tuples, untrackable column type, …) and the
    /// caller must fall back to a normal scan.
    ///
    /// Callers use this as a constant-time pre-flight before
    /// committing to a full UPDATE/DELETE seq-scan: rejecting the
    /// noop case in microseconds instead of milliseconds.
    fn try_prove_filter_empty(
        &self,
        _txn: TxnId,
        _snapshot: &Snapshot,
        _table_id: RelationId,
        _column_predicates: &[(ColumnId, std::ops::Bound<Value>, std::ops::Bound<Value>)],
    ) -> DbResult<Option<bool>> {
        Ok(None)
    }

    /// Return a cheap visible-row count for `filter_column = filter_value`
    /// when the backend maintains latest-value counts. Callers must fall back
    /// to `scan_table_eq_filter` or `scan_table` when unsupported.
    fn visible_eq_row_count(
        &self,
        _txn: TxnId,
        _snapshot: &Snapshot,
        _table_id: RelationId,
        _filter_column: ColumnId,
        _filter_value: &Value,
    ) -> DbResult<u64> {
        Err(DbError::feature_not_supported(
            "storage visible equality row count is not supported",
        ))
    }

    /// Return a cheap visible-row count for an index key range when the
    /// backend can prove visibility from its latest committed index state.
    fn visible_index_row_count(
        &self,
        _txn: TxnId,
        _snapshot: &Snapshot,
        _index_id: IndexId,
        _key_range: KeyRange,
    ) -> DbResult<u64> {
        Err(DbError::feature_not_supported(
            "storage visible index row count is not supported",
        ))
    }

    /// Return visible tuple ids for an index key range without materializing
    /// heap rows. Backends may support this only for latest snapshots where
    /// the index state is authoritative.
    fn index_candidate_tuple_ids(
        &self,
        _txn: TxnId,
        _snapshot: &Snapshot,
        _index_id: IndexId,
        _key_range: KeyRange,
    ) -> DbResult<Vec<TupleId>> {
        Err(DbError::feature_not_supported(
            "storage index tuple-id candidates are not supported",
        ))
    }

    /// Return grouped visible-row counts for a single-column ordered index.
    ///
    /// The returned values must be in index key order. Callers must fall back
    /// to a regular scan when unsupported.
    fn visible_index_group_counts(
        &self,
        _txn: TxnId,
        _snapshot: &Snapshot,
        _index_id: IndexId,
        _key_range: KeyRange,
    ) -> DbResult<Vec<(Value, u64)>> {
        Err(DbError::feature_not_supported(
            "storage visible index group counts are not supported",
        ))
    }

    /// Return grouped visible-row counts as ready-to-return rows for the
    /// common `GROUP BY key, COUNT(*)` shape.
    fn visible_index_group_count_rows(
        &self,
        txn: TxnId,
        snapshot: &Snapshot,
        index_id: IndexId,
        key_range: KeyRange,
    ) -> DbResult<Vec<Row>> {
        let groups = self.visible_index_group_counts(txn, snapshot, index_id, key_range)?;
        Ok(groups
            .into_iter()
            .map(|(group, count)| {
                Row::new(vec![
                    group,
                    Value::BigInt(i64::try_from(count).unwrap_or(i64::MAX)),
                ])
            })
            .collect())
    }

    /// Return the lowest non-NULL value present in the column
    /// covered by a single-column index. Used by the
    /// `SELECT MIN(col) FROM t` fast path to avoid materialising
    /// every leaf entry through `scan_index`. Returns `Ok(None)`
    /// for empty / all-NULL tables (caller turns into SQL NULL).
    fn index_min_single_column_value(
        &self,
        _txn: TxnId,
        _snapshot: &Snapshot,
        _index_id: IndexId,
    ) -> DbResult<Option<Value>> {
        Err(DbError::feature_not_supported(
            "storage index min-value lookup is not supported",
        ))
    }

    /// Return the highest non-NULL value present in the column
    /// covered by a single-column index. Mirror of
    /// `index_min_single_column_value` for the MAX(col) fast path.
    fn index_max_single_column_value(
        &self,
        _txn: TxnId,
        _snapshot: &Snapshot,
        _index_id: IndexId,
    ) -> DbResult<Option<Value>> {
        Err(DbError::feature_not_supported(
            "storage index max-value lookup is not supported",
        ))
    }

    /// Scan an index within the given key range. **Required.**
    fn scan_index(
        &self,
        txn: TxnId,
        snapshot: &Snapshot,
        index_id: IndexId,
        key_range: KeyRange,
        projected_columns: Option<Vec<ColumnId>>,
    ) -> DbResult<Box<dyn TupleStream>>;

    /// Scan an index and return at most `limit` rows.
    ///
    /// Backends can override this to avoid materializing the full candidate
    /// set before an upper executor LIMIT truncates the stream.
    fn scan_index_limited(
        &self,
        txn: TxnId,
        snapshot: &Snapshot,
        index_id: IndexId,
        key_range: KeyRange,
        projected_columns: Option<Vec<ColumnId>>,
        limit: usize,
    ) -> DbResult<Box<dyn TupleStream>> {
        let mut stream = self.scan_index(txn, snapshot, index_id, key_range, projected_columns)?;
        let mut records = Vec::with_capacity(limit);
        while records.len() < limit {
            let Some(record) = stream.next()? else {
                break;
            };
            records.push(record);
        }
        Ok(Box::new(crate::VecTupleStream::new(records)))
    }

    /// Scan an index in logical key order.
    ///
    /// `descending=false` must match the same order as [`Self::scan_index`].
    /// `descending=true` requests reverse key order.
    ///
    /// Default implementation reports unsupported so callers can fall back.
    fn scan_index_ordered(
        &self,
        _txn: TxnId,
        _snapshot: &Snapshot,
        _index_id: IndexId,
        _key_range: KeyRange,
        _projected_columns: Option<Vec<ColumnId>>,
        _descending: bool,
    ) -> DbResult<Box<dyn TupleStream>> {
        Err(DbError::feature_not_supported(
            "ordered index scan is not supported",
        ))
    }

    /// Scan an index in logical key order, returning at most `limit` rows.
    ///
    /// Backends can override this to avoid materializing the full ordered
    /// stream for Top-N queries. The default implementation preserves
    /// semantics by truncating `scan_index_ordered`.
    fn scan_index_ordered_limited(
        &self,
        txn: TxnId,
        snapshot: &Snapshot,
        index_id: IndexId,
        key_range: KeyRange,
        projected_columns: Option<Vec<ColumnId>>,
        descending: bool,
        limit: usize,
    ) -> DbResult<Box<dyn TupleStream>> {
        let mut stream = self.scan_index_ordered(
            txn,
            snapshot,
            index_id,
            key_range,
            projected_columns,
            descending,
        )?;
        let mut records = Vec::with_capacity(limit);
        while records.len() < limit {
            let Some(record) = stream.next()? else {
                break;
            };
            records.push(record);
        }
        Ok(Box::new(crate::VecTupleStream::new(records)))
    }

    /// Fetch a single row by tuple id. **Required.**
    fn fetch(
        &self,
        txn: TxnId,
        snapshot: &Snapshot,
        table_id: RelationId,
        tuple_id: TupleId,
        projected_columns: Option<Vec<ColumnId>>,
    ) -> DbResult<Option<Row>>;

    /// Fetch a single row by tuple id using a borrowed projection slice.
    ///
    /// Default implementation preserves semantics by cloning the slice into
    /// the owned [`Self::fetch`] shape. Backends should override this when
    /// they can consume borrowed projections directly to avoid per-call
    /// allocation on hot paths.
    fn fetch_ref(
        &self,
        txn: TxnId,
        snapshot: &Snapshot,
        table_id: RelationId,
        tuple_id: TupleId,
        projected_columns: Option<&[ColumnId]>,
    ) -> DbResult<Option<Row>> {
        self.fetch(
            txn,
            snapshot,
            table_id,
            tuple_id,
            projected_columns.map(<[ColumnId]>::to_vec),
        )
    }

    /// Insert a row into a table. **Required.**
    fn insert(&self, txn: TxnId, table_id: RelationId, row: Row) -> DbResult<TupleId>;

    /// Insert multiple rows into a table.
    ///
    /// Default implementation falls back to repeated [`Self::insert`] calls.
    /// Storage engines can override this to batch WAL appends and lock usage.
    fn insert_batch(
        &self,
        txn: TxnId,
        table_id: RelationId,
        rows: Vec<Row>,
    ) -> DbResult<Vec<TupleId>> {
        let mut tuple_ids = Vec::with_capacity(rows.len());
        for row in rows {
            tuple_ids.push(self.insert(txn, table_id, row)?);
        }
        Ok(tuple_ids)
    }

    /// Update a row in a table. **Required.**
    fn update(
        &self,
        txn: TxnId,
        table_id: RelationId,
        tuple_id: TupleId,
        row: Row,
    ) -> DbResult<TupleId>;

    /// Delete a row from a table. **Required.**
    fn delete(&self, txn: TxnId, table_id: RelationId, tuple_id: TupleId) -> DbResult<()>;

    /// Remove dead tuple versions from the given table. **Required.**
    ///
    /// Returns the number of dead versions removed.
    fn vacuum_table(&self, table_id: RelationId) -> DbResult<u64>;

    // ─── Optional methods (have default impls) ──────────────────

    /// Search an HNSW vector index for the k nearest neighbors.
    ///
    /// Returns a `TupleStream` of rows from the underlying table, ordered by
    /// distance to the query vector (closest first).
    ///
    /// When `max_search_duration` is `Some`, the search should abort early if
    /// the given duration is exceeded and return partial results.
    ///
    /// # Optional capability
    ///
    /// The default implementation returns
    /// [`DbError::feature_not_supported`].  Storage backends that support
    /// vector indexes should override this method and return `true` from
    /// [`StorageCapabilities::supports_vector_search`](crate::StorageCapabilities::supports_vector_search).
    fn vector_search(
        &self,
        _txn: TxnId,
        _snapshot: &Snapshot,
        _index_id: IndexId,
        _query: &[f32],
        _k: usize,
        _ef: usize,
        _tuple_id_filter: Option<&(dyn Fn(TupleId) -> bool + Send + Sync)>,
        _max_search_duration: Option<Duration>,
        _interrupt_checker: Option<&(dyn Fn() -> DbResult<()> + Send + Sync)>,
    ) -> DbResult<Box<dyn TupleStream>> {
        Err(DbError::feature_not_supported(
            "vector search is not supported by this storage backend",
        ))
    }

    /// Search a GIN index for tuples whose JSONB column contains the given
    /// pattern (i.e., `col @> pattern`).
    ///
    /// Returns a `TupleStream` of matching rows from the underlying table.
    ///
    /// # Optional capability
    ///
    /// The default implementation returns
    /// [`DbError::feature_not_supported`].  Storage backends that support
    /// GIN indexes should override this method and return `true` from
    /// [`StorageCapabilities::supports_gin_search`](crate::StorageCapabilities::supports_gin_search).
    fn gin_containment_search(
        &self,
        _txn: TxnId,
        _snapshot: &Snapshot,
        _index_id: IndexId,
        _pattern: &serde_json::Value,
    ) -> DbResult<Box<dyn TupleStream>> {
        Err(DbError::feature_not_supported(
            "GIN containment search is not supported by this storage backend",
        ))
    }

    /// Search a GIN index and stop after `visible_limit` visible table rows
    /// when the backend can apply that limit during tuple fetch.
    fn gin_containment_search_limited(
        &self,
        txn: TxnId,
        snapshot: &Snapshot,
        index_id: IndexId,
        pattern: &serde_json::Value,
        _visible_limit: usize,
    ) -> DbResult<Box<dyn TupleStream>> {
        self.gin_containment_search(txn, snapshot, index_id, pattern)
    }

    /// Register an edge table for adjacency index tracking.
    ///
    /// When an edge label is created, call this method with the backing
    /// table's `RelationId` and the column indexes for the source and target
    /// node IDs. Subsequent insert / delete / update operations on this table
    /// will automatically maintain the adjacency index.
    ///
    /// # Optional capability
    ///
    /// maintain adjacency indexes should override this method.
    fn register_edge_table(
        &self,
        _table_id: RelationId,
        _source_col_idx: usize,
        _target_col_idx: usize,
    ) {
    }

    /// Unregister an edge table, removing its adjacency index.
    ///
    /// Called when an edge label is dropped so the storage engine can free
    /// the associated adjacency index resources.
    ///
    /// # Optional capability
    ///
    fn unregister_edge_table(&self, _table_id: RelationId) {}

    /// Log ANALYZE statistics to the WAL for crash recovery.
    ///
    /// # Optional capability
    ///
    /// Backends that persist statistics (e.g., via WAL) should override this
    /// method and return `true` from
    /// [`StorageCapabilities::supports_statistics_logging`](crate::StorageCapabilities::supports_statistics_logging).
    fn log_analyze_stats(
        &self,
        _table_id: RelationId,
        _row_count: u64,
        _total_bytes: u64,
        _dead_row_count: u64,
        _column_stats: Vec<(ColumnId, f64, f64, u32)>,
    ) -> DbResult<()> {
        Ok(())
    }

    /// Look up edge tuple IDs from an adjacency index by node ID and direction.
    ///
    /// When `outgoing` is `true`, returns edge tuple IDs whose **source** node
    /// matches `node_id`.  When `false`, returns edges whose **target** matches.
    ///
    /// # Optional capability
    ///
    /// The default implementation returns
    /// [`DbError::feature_not_supported`].  Storage backends that maintain
    /// adjacency indexes for graph edge tables should override this method
    /// and return `true` from
    /// [`StorageCapabilities::supports_adjacency_lookup`](crate::StorageCapabilities::supports_adjacency_lookup).
    fn adjacency_lookup(
        &self,
        _txn: TxnId,
        _snapshot: &Snapshot,
        _edge_table_id: RelationId,
        _node_id: &Value,
        _outgoing: bool,
    ) -> DbResult<Vec<TupleId>> {
        Err(DbError::feature_not_supported(
            "adjacency lookup is not supported by this storage backend",
        ))
    }

    /// Look up adjacent edge tuple ids through a streaming cursor.
    ///
    /// The default implementation preserves semantics by materializing the
    /// full tuple id vector through [`StorageDML::adjacency_lookup`].
    fn adjacency_edge_cursor(
        &self,
        txn: TxnId,
        snapshot: &Snapshot,
        edge_table_id: RelationId,
        node_id: &Value,
        outgoing: bool,
    ) -> DbResult<Box<dyn NeighborCursor<TupleId> + '_>> {
        let edge_ids = self.adjacency_lookup(txn, snapshot, edge_table_id, node_id, outgoing)?;
        Ok(Box::new(OwnedCursor::new(edge_ids)))
    }

    /// Look up adjacent node IDs directly from the adjacency index.
    ///
    /// This is the value-level companion to [`StorageDML::adjacency_lookup`].
    /// It avoids fetching each edge row when the caller only needs the far
    /// endpoint IDs for a simple graph traversal.
    fn adjacency_neighbors(
        &self,
        _txn: TxnId,
        _snapshot: &Snapshot,
        _edge_table_id: RelationId,
        _node_id: &Value,
        _outgoing: bool,
    ) -> DbResult<Vec<Value>> {
        Err(DbError::feature_not_supported(
            "adjacency neighbor lookup is not supported by this storage backend",
        ))
    }

    /// Look up adjacent node IDs through a streaming cursor.
    ///
    /// The default implementation preserves semantics by materializing the
    /// full neighbor vector through [`StorageDML::adjacency_neighbors`].
    fn adjacency_neighbor_cursor(
        &self,
        txn: TxnId,
        snapshot: &Snapshot,
        edge_table_id: RelationId,
        node_id: &Value,
        outgoing: bool,
    ) -> DbResult<Box<dyn NeighborCursor<Value> + '_>> {
        let neighbors =
            self.adjacency_neighbors(txn, snapshot, edge_table_id, node_id, outgoing)?;
        Ok(Box::new(OwnedCursor::new(neighbors)))
    }

    /// Enumerate edge endpoints directly from the adjacency store.
    ///
    /// Returns `(edge_tuple_id, source_id, target_id)` triples for the edge
    /// table when the backend maintains a native adjacency index.
    fn adjacency_edges(
        &self,
        _txn: TxnId,
        _snapshot: &Snapshot,
        _edge_table_id: RelationId,
    ) -> DbResult<Vec<(TupleId, Value, Value)>> {
        Err(DbError::feature_not_supported(
            "adjacency edge enumeration is not supported by this storage backend",
        ))
    }

    /// Enumerate weighted edge endpoints directly from the adjacency store.
    ///
    /// Returns `(edge_tuple_id, source_id, target_id, weight)` tuples for the
    /// edge table when the backend can combine native adjacency enumeration
    /// with direct access to the requested weight column.
    fn adjacency_weighted_edges(
        &self,
        _txn: TxnId,
        _snapshot: &Snapshot,
        _edge_table_id: RelationId,
        _weight_column: ColumnId,
    ) -> DbResult<Vec<(TupleId, Value, Value, Value)>> {
        Err(DbError::feature_not_supported(
            "adjacency weighted edge enumeration is not supported by this storage backend",
        ))
    }

    /// Look up the `(source_id, target_id)` endpoints for one edge tuple
    /// directly from the native adjacency store.
    fn adjacency_edge_endpoints(
        &self,
        _txn: TxnId,
        _snapshot: &Snapshot,
        _edge_table_id: RelationId,
        _edge_tuple_id: TupleId,
    ) -> DbResult<Option<(Value, Value)>> {
        Err(DbError::feature_not_supported(
            "adjacency edge endpoint lookup is not supported by this storage backend",
        ))
    }

    /// Return whether the storage backend maintains a native adjacency index
    /// for the table, even if that index is currently empty.
    fn adjacency_index_available(&self, _txn: TxnId, _edge_table_id: RelationId) -> bool {
        false
    }

    /// Return planner/runtime stats for a native adjacency index when
    /// available. Backends without adjacency indexes return `None`.
    fn adjacency_index_stats(&self, _txn: TxnId, _edge_table_id: RelationId) -> Option<GraphStats> {
        None
    }

    /// Return whether the adjacency index currently contains any edges for
    /// the table. Backends without adjacency indexes return `false`.
    fn adjacency_index_has_edges(&self, _txn: TxnId, _edge_table_id: RelationId) -> bool {
        false
    }
}
