mod acl_plans;
mod aggregate_accum;
mod aggregate_finalize;
mod aggregate_helpers;
mod aggregate_set;
mod analyze_plan;
mod check_enforcement;
mod command_plans;
mod compat;
mod copy_plans;
pub use copy_plans::{format_copy_text_value, parse_copy_text_value};
pub mod deferred_fk;
mod derived_source_plans;
mod distributed_aggregate;
mod distributed_fragments;
mod distributed_join;
mod distributed_scan;
mod dml_plans;
mod fk_enforcement;
pub mod fragment_metrics;
mod graph_plans;
mod graph_runtime_args;
mod graph_runtime_caches;
mod graph_runtime_match;
mod graph_runtime_nodes;
mod graph_runtime_paths;
mod graph_runtime_shortest_path;
mod graph_runtime_traversal;
pub(crate) mod helpers;
mod join_plans;
mod metadata_helpers;
pub mod node_registry;
mod plpgsql_runtime;
mod projection_plans;
mod special_exprs;
mod trigger_firing;
mod unique_enforcement;
mod user_functions;
mod vacuum_plan;

mod window_eval;

#[cfg(test)]
mod tests;

use std::{
    cell::RefCell,
    cmp::Ordering,
    collections::{BTreeSet, HashMap, HashSet},
    hash::{Hash, Hasher},
    sync::{Arc, Mutex, RwLock},
};

use aiondb_catalog::{
    CatalogReader, CatalogTxnParticipant, CatalogWriter, ColumnDescriptor, EdgeEndpoints,
    EdgeLabelDescriptor, ForeignKeyConstraint, HnswParams, IndexDescriptor, NodeLabelDescriptor,
    QualifiedName, SchemaDescriptor, SequenceDescriptor, SequenceManager, TableAlteration,
    TableDescriptor, TriggerDescriptor, TriggerEventDescriptor, TriggerTimingDescriptor,
    VectorDistanceMetric, VectorQuantizationKind, ViewDescriptor,
};
use aiondb_core::{
    ColumnId, DataType, DbError, DbResult, ErrorReport, IndexId, RelationId, Row, SchemaId,
    SequenceId, SqlState, TidValue, TupleId, TxnId, Value,
};
use aiondb_eval::{
    build_hash_key, coerce_value, compare_runtime_values, eval_pg_ls_dir_with_base_dir,
    eval_pg_read_binary_file_with_base_dir, eval_pg_read_file_with_base_dir, with_session_context,
    ExpressionEvaluator, ValueHashKey,
};
use aiondb_graph::{
    algorithms::CsrGraph, GraphDirection, GraphProjection, GraphStats, GraphStorage, GraphViewV2,
    HybridGraphPlan, HybridGraphSource, NeighborCursor, ProjectionSnapshot,
};
use aiondb_graph_projection::NamedGraphProjectionDescriptor;
use aiondb_plan::{
    InsertOnConflict, JoinType, OnConflictActionPlan, PhysicalPlan, ProjectionExpr, ScalarFunction,
    ScanAccessPath, SetOperationType, SortExpr, TypedExpr, TypedExprKind,
};
use aiondb_schema_bridge::{to_index_storage_descriptor, to_table_storage_descriptor};
use aiondb_storage_api::{
    KeyRange, PartitionFilterStream, StorageDDL, StorageDML, StorageTxnParticipant, TupleRecord,
    TupleStream, VecTupleStream,
};
use aiondb_tx::LockMode;
use tracing::warn;

use crate::{ExecutionContext, ExecutionResult};

pub(super) use aiondb_catalog::{
    CatalogPrivilege, IndexKeyColumn, IndexKind, PrivilegeTarget, SortOrder,
};

use self::distributed_fragments::LocalFragmentDispatcher;
use self::graph_runtime_caches::{
    GraphAlgorithmProjectionRuntimeCache, GraphAlgorithmRuntimeCaches,
    GraphAlgorithmWeightedEdgesRuntimeCache,
};
use self::helpers::*;
use self::metadata_helpers::*;

pub use self::distributed_fragments::{
    assign_distributed_fragment_targets, distributed_fragment_target_for_index,
    format_fragment_target, hash_partition_for_row, DistributedFragment, FragmentDispatcher,
    FragmentPartition, FragmentTarget, RegisteredRemoteFragmentDispatcher, RemoteFragmentHandler,
};

pub type LogicalPlanCompiler =
    dyn Fn(&aiondb_plan::LogicalPlan, &ExecutionContext) -> DbResult<PhysicalPlan> + Send + Sync;

/// Walks the child plan to confirm there is at least one `SeqScan`
/// `ProjectTable` reachable, which is the only operator that responds to
/// `ParallelScanPartition` by emitting a disjoint slice. Without one, fanning
/// out to N workers would duplicate output (operators like `ProjectValues`
/// ignore the partition).
fn plan_contains_partitionable_seq_scan(plan: &PhysicalPlan) -> bool {
    use aiondb_plan::ScanAccessPath;
    let mut stack = vec![plan];
    while let Some(node) = stack.pop() {
        match node {
            PhysicalPlan::ProjectTable {
                access_path: ScanAccessPath::SeqScan,
                ..
            } => return true,
            PhysicalPlan::Gather { child, .. } => stack.push(child),
            PhysicalPlan::ProjectSource { source, .. } => stack.push(source),
            PhysicalPlan::AggregateSource { source, .. } => stack.push(source),
            _ => {}
        }
    }
    false
}

/// Run `child` on `workers` threads, each with a distinct
/// `ParallelScanPartition`. Concatenates the per-worker `Query` row vectors
/// (any non-Query result short-circuits in worker 0's order). Per-thread
/// statement caches are reset implicitly because Executor::execute installs
/// fresh thread-local state on entry. Non-Query workers are not expected for
/// scan plans wrapped in Gather, so the first one wins.
fn execute_gather_parallel(
    executor: &Executor,
    child: &PhysicalPlan,
    context: &ExecutionContext,
    workers: usize,
) -> DbResult<Vec<aiondb_core::Row>> {
    let num_workers = u32::try_from(workers).unwrap_or(u32::MAX);
    let results: Vec<DbResult<Vec<aiondb_core::Row>>> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..workers)
            .map(|worker_id| {
                let mut worker_ctx = context.clone();
                worker_ctx.parallel_scan_partition = Some(crate::ParallelScanPartition {
                    worker_id: worker_id as u32,
                    num_workers,
                });
                scope.spawn(move || match executor.execute(child, &worker_ctx)? {
                    ExecutionResult::Query { rows, .. } => Ok(rows),
                    _ => Err(DbError::internal("Gather child returned non-Query result")),
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| {
                handle
                    .join()
                    .map_err(|_| DbError::internal("Gather worker thread panicked"))
                    .and_then(|inner| inner)
            })
            .collect()
    });
    let mut combined: Vec<aiondb_core::Row> = Vec::new();
    for result in results {
        combined.extend(result?);
    }
    Ok(combined)
}

/// When this context is running inside a Gather worker, wrap the raw scan
/// stream so it only yields tuples assigned to this worker. Outside Gather
/// (the common single-threaded path) it's a no-op.
fn apply_parallel_scan_partition(
    stream: Box<dyn TupleStream>,
    context: &ExecutionContext,
) -> Box<dyn TupleStream> {
    match context.parallel_scan_partition {
        Some(partition) if partition.num_workers > 1 => Box::new(PartitionFilterStream::new(
            stream,
            partition.worker_id,
            partition.num_workers,
        )),
        _ => stream,
    }
}

/// A tuple stream adapter that remaps column ordinals, used for
/// index-only scans where the index column order differs from the
/// requested projection order.
struct RemapTupleStream {
    inner: Box<dyn TupleStream>,
    ordinals: Vec<usize>,
}

impl TupleStream for RemapTupleStream {
    fn next(&mut self) -> DbResult<Option<TupleRecord>> {
        let Some(mut record) = self.inner.next()? else {
            return Ok(None);
        };
        let remapped: Vec<Value> = self
            .ordinals
            .iter()
            .map(|&ord| record.row.values.get(ord).cloned().unwrap_or(Value::Null))
            .collect();
        record.row = Row { values: remapped };
        Ok(Some(record))
    }
}

/// A tuple stream adapter that expands index-only scan tuples back into
/// the base table ordinal layout so filters and ORDER BY can keep using
/// table-column ordinals without a heap fetch.
struct ExpandIndexOnlyTupleStream {
    inner: Box<dyn TupleStream>,
    table_width: usize,
    table_ordinals: Vec<usize>,
}

impl TupleStream for ExpandIndexOnlyTupleStream {
    fn next(&mut self) -> DbResult<Option<TupleRecord>> {
        let Some(mut record) = self.inner.next()? else {
            return Ok(None);
        };
        let mut expanded = vec![Value::Null; self.table_width];
        for (source_ordinal, table_ordinal) in self.table_ordinals.iter().copied().enumerate() {
            if let Some(value) = record.row.values.get(source_ordinal).cloned() {
                expanded[table_ordinal] = value;
            }
        }
        record.row = Row::new(expanded);
        Ok(Some(record))
    }
}

fn annotate_fragment_execution_error(
    error: DbError,
    fragment_index: usize,
    target: &FragmentTarget,
) -> DbError {
    let context = format!(
        "distributed fragment #{fragment_index} target={} failed",
        format_fragment_target(target)
    );
    let merged_detail = match error.report().internal_detail.clone() {
        Some(existing) => format!("{context}; {existing}"),
        None => context,
    };
    error.with_internal_detail(merged_detail)
}

fn format_result_schema_signature(columns: &[aiondb_plan::ResultField]) -> String {
    let mut result = String::new();
    for (i, column) in columns.iter().enumerate() {
        if i > 0 {
            result.push(',');
        }
        use std::fmt::Write;
        let _ = write!(result, "{}:{}", column.name, column.data_type);
    }
    result
}

fn result_schema_compatible(
    expected: &[aiondb_plan::ResultField],
    actual: &[aiondb_plan::ResultField],
) -> bool {
    expected.len() == actual.len()
        && expected
            .iter()
            .zip(actual.iter())
            .all(|(left, right)| left.data_type == right.data_type)
}

fn coerce_rows_to_expected_schema(
    rows: Vec<Row>,
    expected: &[aiondb_plan::ResultField],
) -> DbResult<Vec<Row>> {
    rows.into_iter()
        .enumerate()
        .map(|(row_index, row)| {
            if row.values.len() != expected.len() {
                return Err(DbError::internal(
                    "distributed fragments produced inconsistent row widths",
                )
                .with_internal_detail(format!(
                    "row_index={row_index} expected_row_width={} actual_row_width={}",
                    expected.len(),
                    row.values.len()
                )));
            }

            let values = row
                .values
                .into_iter()
                .zip(expected.iter())
                .enumerate()
                .map(|(column_index, (value, field))| {
                    coerce_value(value, &field.data_type).map_err(|error| {
                        let detail = format!(
                            "column_index={column_index} expected_type={} coercion_error={}",
                            field.data_type,
                            error.report().message
                        );
                        match error.report().internal_detail.clone() {
                            Some(existing) => {
                                error.with_internal_detail(format!("{detail}; {existing}"))
                            }
                            None => error.with_internal_detail(detail),
                        }
                    })
                })
                .collect::<DbResult<Vec<_>>>()?;

            Ok(Row::new(values))
        })
        .collect()
}

fn side_effect_dml_cache_key(plan: &PhysicalPlan) -> Option<u64> {
    if !matches!(
        plan,
        PhysicalPlan::InsertValues { .. }
            | PhysicalPlan::InsertSelect { .. }
            | PhysicalPlan::DeleteFromTable { .. }
            | PhysicalPlan::UpdateTable { .. }
            | PhysicalPlan::MergeTable(_)
    ) {
        return None;
    }

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    format!("{plan:?}").hash(&mut hasher);
    Some(hasher.finish())
}

#[derive(Debug)]
#[allow(clippy::option_option)]
struct InSubqueryCacheEntry {
    values: Vec<Value>,
    hash_index: HashMap<ValueHashKey, Vec<usize>>,
    first_value_type: Option<Option<DataType>>,
    homogeneous_type: bool,
    all_hashable: bool,
    has_null: bool,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum InSubqueryCacheKey {
    Expr(*const TypedExpr),
    Plan(*const aiondb_plan::LogicalPlan),
}

thread_local! {
    static STATEMENT_IN_SUBQUERY_CACHE: RefCell<Option<HashMap<InSubqueryCacheKey, Arc<InSubqueryCacheEntry>>>> =
        const { RefCell::new(None) };
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum ValueSubqueryCacheKey {
    Expr(*const TypedExpr),
}

thread_local! {
    static STATEMENT_VALUE_SUBQUERY_CACHE: RefCell<Option<HashMap<ValueSubqueryCacheKey, Value>>> =
        const { RefCell::new(None) };
}

/// Per-statement memo for *correlated* EXISTS / NOT EXISTS subqueries.
///
/// PG decorrelates most correlated EXISTS into a SemiJoin / AntiJoin, but
/// some shapes (aggregates over the outer relation, joins above the
/// EXISTS, row-level locks, …) keep the per-row sub-plan execution. For
/// those, the EXISTS result still depends only on the values of the
/// outer columns it actually references, so it's safe to memoize on
/// `(expr_ptr, outer_correlation_values)` — equivalent in spirit to PG's
/// `MaterializeNode` over a SubPlan with `extParam` keys.
///
/// Keying on a bincode-serialised tuple of the referenced outer values
/// keeps the key `Hash + Eq` without requiring `Hash` on `Value` (which
/// can hold floats / arrays / jsonb that don't have a stable hash).
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct CorrelatedExistsCacheKey {
    expr_ptr: usize,
    outer_values_serialized: Vec<u8>,
}

thread_local! {
    static STATEMENT_CORRELATED_EXISTS_CACHE: RefCell<Option<HashMap<CorrelatedExistsCacheKey, bool>>> =
        const { RefCell::new(None) };
}

/// Per-statement materialization cache for *correlated* EXISTS / NOT
/// EXISTS subqueries that fit a single equi-correlation pattern, keyed
/// by `*const LogicalPlan` (cast to usize). The semi-join entry is the
/// HashSet of inner-side equi-key values (with a `has_null` flag so we
/// can also serve `NOT EXISTS` semantics correctly), built once on the
/// first probe and re-used for the rest of the outer scan.
///
/// This bypasses the per-row substitute + compile + execute work the
/// generic correlated EXISTS path performs and is the runtime equivalent
/// of PG's planner-level pull-up of EXISTS into a SemiJoin.
pub(crate) struct ExistsSemiJoinEntry {
    pub keys: std::collections::HashSet<ValueHashKey>,
}

thread_local! {
    static STATEMENT_EXISTS_SEMIJOIN_CACHE: RefCell<Option<HashMap<usize, Arc<ExistsSemiJoinEntry>>>> =
        const { RefCell::new(None) };
}

/// Per-statement materialization cache for *correlated scalar
/// aggregate subqueries* of the shape `SELECT agg(col) FROM s WHERE
/// s.k = OUTER.k [AND <const>]`. Decorrelates the per-row aggregate
/// into one `GROUP BY s.k` materialisation; the resulting `(s.k →
/// agg_value)` map answers every probe in O(1) instead of recompiling
/// and re-executing the aggregate per outer row. This is the runtime
/// equivalent of PG's `MaterializeNode` over a SubPlan for the
/// `LATERAL aggregate` shape.
pub(crate) struct ScalarAggregateSemiJoinEntry {
    pub values: HashMap<ValueHashKey, Value>,
}

thread_local! {
    static STATEMENT_SCALAR_AGG_SEMIJOIN_CACHE: RefCell<Option<HashMap<usize, Arc<ScalarAggregateSemiJoinEntry>>>> =
        const { RefCell::new(None) };
}

enum ResolvedRelation {
    Synthetic { oid: i32, display_name: String },
    Table(TableDescriptor),
    View(ViewDescriptor),
    Index(IndexDescriptor),
}

impl ResolvedRelation {
    fn oid(&self) -> i32 {
        match self {
            Self::Synthetic { oid, .. } => *oid,
            Self::Table(table) => relation_id_to_oid(table.table_id),
            Self::View(view) => relation_id_to_oid(view.view_id),
            Self::Index(index) => index_id_to_oid(index.index_id),
        }
    }

    fn display_name(&self) -> String {
        match self {
            Self::Synthetic { display_name, .. } => display_name.clone(),
            Self::Table(table) => format_relation_name(&table.name),
            Self::View(view) => format_relation_name(&view.name),
            Self::Index(index) => format_relation_name(&index.name),
        }
    }

    fn qualified_display_name(&self) -> String {
        match self {
            Self::Synthetic { display_name, .. } => display_name.clone(),
            Self::Table(table) => format_qualified_relation_name(&table.name),
            Self::View(view) => format_qualified_relation_name(&view.name),
            Self::Index(index) => format_qualified_relation_name(&index.name),
        }
    }
}

pub struct Executor {
    evaluator: ExpressionEvaluator,
    catalog_reader: Arc<dyn CatalogReader>,
    catalog_writer: Arc<dyn CatalogWriter>,
    catalog_txn: Arc<dyn CatalogTxnParticipant>,
    sequence_manager: Arc<dyn SequenceManager>,
    storage_ddl: Arc<dyn StorageDDL>,
    storage_dml: Arc<dyn StorageDML>,
    storage_txn: Arc<dyn StorageTxnParticipant>,
    logical_plan_compiler: Arc<LogicalPlanCompiler>,
    fragment_dispatcher: Arc<dyn FragmentDispatcher>,
    expression_index_meta: Mutex<HashMap<IndexId, ExpressionIndexMeta>>,
    table_inheritance_meta: Mutex<TableInheritanceMeta>,
    graph_neighbor_meta: Mutex<HashMap<String, GraphNeighborEdgeMeta>>,
    hnsw_project_source_result_cache:
        RwLock<HashMap<HnswProjectSourceResultCacheKey, (u64, ExecutionResult)>>,
    graph_adjacency_neighbors_cache:
        RwLock<HashMap<GraphAdjacencyNeighborsCacheKey, (u64, Vec<Value>)>>,
    graph_id_lookup_result_cache: RwLock<HashMap<GraphIdLookupResultCacheKey, (u64, Vec<Row>)>>,
    graph_first_col_row_cache: RwLock<HashMap<GraphFirstColRowCacheKey, (u64, Option<Row>)>>,
    graph_edge_filter_limit_rows_cache:
        RwLock<HashMap<GraphEdgeFilterLimitRowsCacheKey, (u64, Vec<Row>)>>,
    graph_target_filter_ids_cache: RwLock<
        HashMap<
            GraphTargetFilterIdsCacheKey,
            (
                u64,
                Arc<HashSet<ValueHashKey, join_plans::JoinFxBuildHasher>>,
            ),
        >,
    >,
    graph_algorithm_runtime_caches: GraphAlgorithmRuntimeCaches,
    hybrid_deep_graph_vector_meta_cache:
        RwLock<HashMap<HybridDeepGraphVectorMetaCacheKey, (u64, HybridDeepGraphVectorMeta)>>,
    join_index_lookup_row_cache: RwLock<HashMap<JoinIndexLookupRowsCacheKey, (u64, Vec<Row>)>>,
    hash_join_build_side_cache:
        RwLock<HashMap<HashJoinBuildSideCacheKey, (u64, Arc<HashJoinBuildSideCacheEntry>)>>,
    project_table_limited_rows_cache:
        RwLock<HashMap<ProjectTableLimitedRowsCacheKey, (u64, Arc<Vec<Row>>)>>,
    project_table_eq_limited_rows_cache:
        RwLock<HashMap<ProjectTableEqLimitedRowsCacheKey, (u64, Arc<Vec<Row>>)>>,
    project_table_range_rows_cache:
        RwLock<HashMap<ProjectTableRangeRowsCacheKey, (u64, Arc<Vec<Row>>, u64)>>,
    project_table_split_rows_cache:
        RwLock<HashMap<ProjectTableSplitRowsCacheKey, (u64, Arc<Vec<Row>>)>>,
    project_table_unique_lookup_rows_cache:
        RwLock<HashMap<ProjectTableUniqueLookupRowsCacheKey, (u64, Arc<Vec<Row>>)>>,
    hash_join_fast_rows_cache: RwLock<HashMap<HashJoinFastRowsCacheKey, (u64, Arc<Vec<Row>>)>>,
    project_table_ordered_rows_cache:
        RwLock<HashMap<ProjectTableOrderedRowsCacheKey, (u64, Arc<Vec<Row>>)>>,
    aggregate_rows_cache: RwLock<HashMap<AggregateRowsCacheKey, (u64, Arc<Vec<Row>>)>>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(super) struct HnswProjectSourceResultCacheKey {
    pub table_id: RelationId,
    pub index_id: IndexId,
    pub query_bits: Vec<u32>,
    pub hnsw_limit: usize,
    pub ef_search: usize,
    pub projected_ordinals: Vec<usize>,
    pub outputs: Vec<HnswProjectSourceOutputCacheKey>,
    pub effective_limit: Option<u64>,
    pub max_result_rows: u64,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(super) enum HnswProjectSourceOutputCacheKey {
    Column(usize),
    L2Distance {
        vector_ordinal: usize,
        query_bits: Vec<u32>,
    },
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(super) struct GraphAdjacencyNeighborsCacheKey {
    pub edge_table_id: RelationId,
    pub node_key: ValueHashKey,
    pub outgoing: bool,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(super) struct GraphIdLookupResultCacheKey {
    pub edge_table_id: RelationId,
    pub start_key: ValueHashKey,
    pub hops: u8,
    pub ordered: bool,
    pub limit: Option<usize>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(super) struct GraphFirstColRowCacheKey {
    pub table_id: RelationId,
    pub index_id: IndexId,
    pub value_key: ValueHashKey,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(super) struct GraphEdgeFilterLimitRowsCacheKey {
    pub edge_table_id: RelationId,
    pub target_col_idx: usize,
    pub weight_col_idx: usize,
    pub filter_value: ValueHashKey,
    pub limit: usize,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) enum GraphTargetFilterComparison {
    Eq,
    Gt,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(super) struct GraphTargetFilterIdsCacheKey {
    pub target_table_id: RelationId,
    pub id_ordinal: usize,
    pub filter_ordinal: usize,
    pub comparison: GraphTargetFilterComparison,
    pub filter_value: ValueHashKey,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq, serde::Serialize, serde::Deserialize)]
pub(super) struct GraphAlgorithmInputCacheKey {
    pub node_labels: Vec<(String, RelationId)>,
    pub edge_labels: Vec<GraphAlgorithmEdgeLabelCacheKey>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq, serde::Serialize, serde::Deserialize)]
pub(super) struct GraphAlgorithmEdgeLabelCacheKey {
    pub label: String,
    pub table_id: RelationId,
    pub source_label: String,
    pub target_label: String,
    pub endpoints: Option<(String, String)>,
}

pub(super) struct GraphAlgorithmInputCacheEntry {
    pub cache_key: GraphAlgorithmInputCacheKey,
    pub projection: NamedGraphProjectionDescriptor,
    pub graph: CsrGraph,
    pub node_ids: Vec<Value>,
    pub node_indexes: HashMap<(u64, ValueHashKey), u32>,
    pub node_value_indexes: HashMap<ValueHashKey, Vec<u32>>,
    pub resolved_edges: Vec<GraphAlgorithmResolvedEdgeLabel>,
}

pub(super) struct GraphAlgorithmProjectionRef<'a> {
    pub projection: &'a NamedGraphProjectionDescriptor,
    pub graph: &'a CsrGraph,
}

impl GraphProjection for GraphAlgorithmProjectionRef<'_> {
    fn projection_name(&self) -> &str {
        &self.projection.name
    }

    fn snapshot(&self) -> &ProjectionSnapshot {
        &self.projection.snapshot
    }

    fn stats(&self) -> GraphStats {
        self.projection.stats
    }

    fn graph_view(&self) -> &dyn GraphViewV2 {
        self.graph
    }
}

impl GraphAlgorithmInputCacheEntry {
    pub(super) fn projection_ref(&self) -> GraphAlgorithmProjectionRef<'_> {
        GraphAlgorithmProjectionRef {
            projection: &self.projection,
            graph: &self.graph,
        }
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(super) struct GraphAlgorithmResolvedEdgeLabel {
    pub label: String,
    pub table_id: RelationId,
    pub source_table_id: RelationId,
    pub target_table_id: RelationId,
    pub source_col_idx: usize,
    pub target_col_idx: usize,
    pub projected_columns: Vec<ColumnId>,
    pub table_column_ids: Vec<ColumnId>,
    pub column_name_indexes: HashMap<String, usize>,
}

pub(super) struct CurrentGraphAlgorithmCatalogSpec {
    pub node_labels: Vec<NodeLabelDescriptor>,
    pub edge_labels: Vec<EdgeLabelDescriptor>,
    pub cache_key: GraphAlgorithmInputCacheKey,
}

impl CurrentGraphAlgorithmCatalogSpec {
    pub(super) fn node_label_keys(&self) -> Vec<(String, u64)> {
        self.cache_key
            .node_labels
            .iter()
            .map(|(label, table_id)| (label.clone(), table_id.get()))
            .collect()
    }

    pub(super) fn edge_label_keys(&self) -> Vec<(String, u64)> {
        self.cache_key
            .edge_labels
            .iter()
            .map(|edge| (edge.label.clone(), edge.table_id.get()))
            .collect()
    }

    pub(super) fn projection_name(&self) -> String {
        aiondb_graph_projection::cypher_native_projection_name(
            &self.node_label_keys(),
            &self.edge_label_keys(),
        )
    }
}

pub(super) struct GraphTraversalRef<S> {
    pub snapshot: ProjectionSnapshot,
    pub stats: GraphStats,
    pub plan: HybridGraphPlan,
    pub storage: S,
}

impl<S: GraphStorage> GraphTraversalRef<S> {
    pub(super) fn snapshot(&self) -> &ProjectionSnapshot {
        &self.snapshot
    }

    pub(super) fn storage(&self) -> &S {
        &self.storage
    }

    pub(super) fn uses_traversal_store(&self) -> bool {
        self.plan.source == Some(HybridGraphSource::TraversalStore)
    }
}

impl<S: GraphStorage> GraphStorage for GraphTraversalRef<S> {
    fn stats(&self) -> GraphStats {
        self.stats
    }

    fn edge_ids(
        &self,
        node_id: &Value,
        direction: GraphDirection,
    ) -> Box<dyn NeighborCursor<TupleId> + '_> {
        self.storage.edge_ids(node_id, direction)
    }

    fn neighbor_ids(
        &self,
        node_id: &Value,
        direction: GraphDirection,
    ) -> Box<dyn NeighborCursor<Value> + '_> {
        self.storage.neighbor_ids(node_id, direction)
    }

    fn edge_endpoints(&self, edge_id: TupleId) -> Option<(Value, Value)> {
        self.storage.edge_endpoints(edge_id)
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq, serde::Serialize, serde::Deserialize)]
pub(super) struct GraphAlgorithmWeightedEdgesCacheKey {
    pub input: GraphAlgorithmInputCacheKey,
    pub weight_column: String,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
#[allow(clippy::struct_field_names)]
pub(super) struct HybridDeepGraphVectorMetaCacheKey {
    pub start_table_id: RelationId,
    pub friend_table_id: RelationId,
    pub source_table_id: RelationId,
    pub target_table_id: RelationId,
}

#[derive(Clone, Debug)]
pub(super) struct HybridDeepGraphVectorMeta {
    pub start_id_idx: usize,
    pub start_tenant_idx: usize,
    pub friend_id_idx: usize,
    pub friend_tenant_idx: usize,
    pub source_id_idx: usize,
    pub source_title_idx: usize,
    pub target_title_idx: usize,
    pub target_tenant_idx: usize,
    pub target_popularity_idx: usize,
    pub target_embedding_idx: usize,
    pub start_id_index: IndexId,
    pub friend_id_index: IndexId,
    pub source_id_index: IndexId,
    pub target_id_index: IndexId,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(super) struct JoinIndexLookupRowsCacheKey {
    pub table_id: RelationId,
    pub index_id: IndexId,
    pub value_key: ValueHashKey,
    pub include_oid_system_column: bool,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(super) struct HashJoinBuildSideCacheKey {
    pub table_id: RelationId,
    pub right_keys: Vec<usize>,
    pub include_oid_system_column: bool,
}

pub(super) struct HashJoinBuildSideCacheEntry {
    pub rows: Vec<Row>,
    pub index: HashMap<join_plans::JoinHashKey, Vec<usize>, join_plans::JoinFxBuildHasher>,
    pub hash_build_ok: bool,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(super) struct ProjectTableLimitedRowsCacheKey {
    pub table_id: RelationId,
    pub projected_columns: Vec<ColumnId>,
    pub offset: u64,
    pub limit: u64,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(super) struct ProjectTableEqLimitedRowsCacheKey {
    pub table_id: RelationId,
    pub filter_column: ColumnId,
    pub filter_value: ValueHashKey,
    pub projected_columns: Vec<ColumnId>,
    pub offset: u64,
    pub limit: u64,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(super) enum ProjectTableRangeBoundCacheKey {
    Unbounded,
    Included(ValueHashKey),
    Excluded(ValueHashKey),
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(super) struct ProjectTableRangeRowsCacheKey {
    pub table_id: RelationId,
    pub index_id: IndexId,
    pub lower: ProjectTableRangeBoundCacheKey,
    pub upper: ProjectTableRangeBoundCacheKey,
    pub projected_columns: Vec<ColumnId>,
    pub offset: u64,
    pub limit: u64,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(super) struct ProjectTableSplitRowsCacheKey {
    pub table_id: RelationId,
    pub id_column: ColumnId,
    pub split_column: ColumnId,
    pub delimiter: String,
    pub offset: u64,
    pub limit: u64,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(super) struct ProjectTableUniqueLookupRowsCacheKey {
    pub table_id: RelationId,
    pub index_id: IndexId,
    pub values: Vec<ValueHashKey>,
    pub projected_columns: Vec<ColumnId>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(super) struct HashJoinFastRowsCacheKey {
    pub left_table_id: RelationId,
    pub right_table_id: RelationId,
    pub left_keys: Vec<usize>,
    pub right_keys: Vec<usize>,
    pub output_ordinals: Vec<usize>,
    pub limit: Option<u64>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(super) struct ProjectTableOrderedRowsCacheKey {
    pub table_id: RelationId,
    pub output_ordinals: Vec<usize>,
    pub filter_key: Option<String>,
    pub order_key: String,
    pub offset: u64,
    pub limit: u64,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(super) struct AggregateRowsCacheKey {
    pub table_id: RelationId,
    pub group_by_key: String,
    pub grouping_sets_key: String,
    pub aggregates_key: String,
    pub having_key: Option<String>,
    pub filter_key: Option<String>,
    pub order_key: String,
    pub limit_key: Option<String>,
    pub offset_key: Option<String>,
    pub distinct: bool,
    pub access_path_key: String,
}

#[derive(Clone)]
pub(super) struct ExpressionIndexMeta {
    pub display_expressions: Vec<String>,
    pub typed_expressions: Vec<TypedExpr>,
    pub expression_only: bool,
}

#[derive(Default)]
pub(super) struct TableInheritanceMeta {
    pub parent_to_children: HashMap<RelationId, BTreeSet<RelationId>>,
    pub child_to_parents: HashMap<RelationId, BTreeSet<RelationId>>,
}

#[derive(Clone)]
/// Outcome of an EvalPlanQual retry: either the predicate still
/// holds against the latest visible tuple (and we have a freshly
/// recomputed new row to apply) or it does not (and the row is
/// silently skipped, mirroring PostgreSQL's READ COMMITTED behaviour).
#[allow(dead_code)]
pub(super) enum EpqOutcome {
    Skip,
    Apply(Row),
}

/// Result of `update_after_table_locked_with_epq`. Distinguishes a
/// successful apply (with the storage-returned tuple id, which may
/// differ from the input when the storage engine relocates the tuple)
/// from a row that the EPQ retry chose to skip.
pub(super) enum EpqUpdateResult {
    Applied(#[allow(dead_code)] aiondb_core::TupleId),
    Skipped,
}

#[derive(Clone)]
pub(super) struct GraphNeighborEdgeMeta {
    pub table_id: RelationId,
    pub source_idx: usize,
    pub target_idx: usize,
    pub source_type: DataType,
    pub target_type: DataType,
    pub use_table_adjacency: bool,
}

impl Executor {
    /// Maximum number of trailing PostgreSQL system columns appended to a
    /// scan row in pg-compat mode: `ctid, tableoid, xmin, cmin, xmax, cmax`
    /// (always) plus a synthetic `oid` (only when the relation does not
    /// already expose an explicit `oid` user column, hence the
    /// `saturating_sub(1)` in `compat_row_width_for_table_id`).
    const COMPAT_SYSTEM_COLUMN_COUNT: usize = 7;
    const COMPAT_TID_PAGE_WIDTH: u64 = 10;
    const GENERATE_SERIES_MAX_ROWS: usize = 150_000;
    const INTERNAL_SAVEPOINT_RECOVERY_MARKER: &str =
        "statement was rolled back to an internal savepoint; outer transaction remains usable";

    fn graph_algorithm_projection_runtime(&self) -> &GraphAlgorithmProjectionRuntimeCache {
        &self.graph_algorithm_runtime_caches.projection_cache
    }

    fn graph_algorithm_weighted_runtime(&self) -> &GraphAlgorithmWeightedEdgesRuntimeCache {
        &self.graph_algorithm_runtime_caches.weighted_edges_cache
    }

    pub(super) fn graph_algorithm_cache_key(
        node_labels: &[NodeLabelDescriptor],
        edge_labels: &[EdgeLabelDescriptor],
    ) -> GraphAlgorithmInputCacheKey {
        let mut node_labels = node_labels
            .iter()
            .map(|label| (label.label.clone(), label.table_id))
            .collect::<Vec<_>>();
        node_labels
            .sort_by(|left, right| left.0.cmp(&right.0).then(left.1.get().cmp(&right.1.get())));

        let mut edge_labels = edge_labels
            .iter()
            .map(|label| GraphAlgorithmEdgeLabelCacheKey {
                label: label.label.clone(),
                table_id: label.table_id,
                source_label: label.source_label.clone(),
                target_label: label.target_label.clone(),
                endpoints: label.endpoints.as_ref().map(|endpoints| {
                    (
                        endpoints.source_id_column.clone(),
                        endpoints.target_id_column.clone(),
                    )
                }),
            })
            .collect::<Vec<_>>();
        edge_labels.sort_by(|left, right| {
            left.label
                .cmp(&right.label)
                .then(left.table_id.get().cmp(&right.table_id.get()))
                .then(left.source_label.cmp(&right.source_label))
                .then(left.target_label.cmp(&right.target_label))
                .then(left.endpoints.cmp(&right.endpoints))
        });

        GraphAlgorithmInputCacheKey {
            node_labels,
            edge_labels,
        }
    }

    pub(super) fn current_graph_algorithm_catalog_spec(
        &self,
        txn_id: TxnId,
    ) -> DbResult<CurrentGraphAlgorithmCatalogSpec> {
        let node_labels = self.catalog_reader.list_node_labels(txn_id)?;
        let edge_labels = self.catalog_reader.list_edge_labels(txn_id)?;
        let cache_key = Self::graph_algorithm_cache_key(&node_labels, &edge_labels);
        Ok(CurrentGraphAlgorithmCatalogSpec {
            node_labels,
            edge_labels,
            cache_key,
        })
    }

    pub(super) fn resolve_algorithm_edge_labels(
        &self,
        context: &ExecutionContext,
        node_labels: &[NodeLabelDescriptor],
        edge_labels: &[EdgeLabelDescriptor],
    ) -> DbResult<Vec<GraphAlgorithmResolvedEdgeLabel>> {
        let node_label_table_ids = node_labels
            .iter()
            .map(|label| (label.label.as_str(), label.table_id))
            .collect::<HashMap<_, _>>();
        edge_labels
            .iter()
            .map(|edge_label| {
                let source_table_id = *node_label_table_ids
                    .get(edge_label.source_label.as_str())
                    .ok_or_else(|| {
                    DbError::internal(format!(
                        "edge label {} references missing source node label {}",
                        edge_label.label, edge_label.source_label
                    ))
                })?;
                let target_table_id = *node_label_table_ids
                    .get(edge_label.target_label.as_str())
                    .ok_or_else(|| {
                    DbError::internal(format!(
                        "edge label {} references missing target node label {}",
                        edge_label.label, edge_label.target_label
                    ))
                })?;
                let table = self
                    .catalog_reader
                    .get_table_by_id(context.txn_id, edge_label.table_id)?
                    .ok_or_else(|| DbError::internal("edge table not found for graph builder"))?;
                let (source_col_idx, target_col_idx) =
                    self.resolve_edge_endpoint_columns_for_label(context, edge_label)?;
                let table_column_ids = table
                    .columns
                    .iter()
                    .map(|column| column.column_id)
                    .collect::<Vec<_>>();
                let column_name_indexes = table
                    .columns
                    .iter()
                    .enumerate()
                    .map(|(index, column)| (column.name.to_ascii_lowercase(), index))
                    .collect::<HashMap<_, _>>();
                let projected_columns = vec![
                    *table_column_ids.get(source_col_idx).ok_or_else(|| {
                        DbError::internal("edge source column ordinal out of bounds")
                    })?,
                    *table_column_ids.get(target_col_idx).ok_or_else(|| {
                        DbError::internal("edge target column ordinal out of bounds")
                    })?,
                ];
                Ok(GraphAlgorithmResolvedEdgeLabel {
                    label: edge_label.label.clone(),
                    table_id: edge_label.table_id,
                    source_table_id,
                    target_table_id,
                    source_col_idx,
                    target_col_idx,
                    projected_columns,
                    table_column_ids,
                    column_name_indexes,
                })
            })
            .collect()
    }

    pub fn new(
        catalog_reader: Arc<dyn CatalogReader>,
        catalog_writer: Arc<dyn CatalogWriter>,
        catalog_txn: Arc<dyn CatalogTxnParticipant>,
        sequence_manager: Arc<dyn SequenceManager>,
        storage_ddl: Arc<dyn StorageDDL>,
        storage_dml: Arc<dyn StorageDML>,
        storage_txn: Arc<dyn StorageTxnParticipant>,
        logical_plan_compiler: Arc<LogicalPlanCompiler>,
    ) -> Self {
        Self {
            evaluator: ExpressionEvaluator,
            catalog_reader,
            catalog_writer,
            catalog_txn,
            sequence_manager,
            storage_ddl,
            storage_dml,
            storage_txn,
            logical_plan_compiler,
            fragment_dispatcher: Arc::new(LocalFragmentDispatcher),
            expression_index_meta: Mutex::new(HashMap::new()),
            table_inheritance_meta: Mutex::new(TableInheritanceMeta::default()),
            graph_neighbor_meta: Mutex::new(HashMap::new()),
            hnsw_project_source_result_cache: RwLock::new(HashMap::new()),
            graph_adjacency_neighbors_cache: RwLock::new(HashMap::new()),
            graph_id_lookup_result_cache: RwLock::new(HashMap::new()),
            graph_first_col_row_cache: RwLock::new(HashMap::new()),
            graph_edge_filter_limit_rows_cache: RwLock::new(HashMap::new()),
            graph_target_filter_ids_cache: RwLock::new(HashMap::new()),
            graph_algorithm_runtime_caches: GraphAlgorithmRuntimeCaches::new(),
            hybrid_deep_graph_vector_meta_cache: RwLock::new(HashMap::new()),
            join_index_lookup_row_cache: RwLock::new(HashMap::new()),
            hash_join_build_side_cache: RwLock::new(HashMap::new()),
            project_table_limited_rows_cache: RwLock::new(HashMap::new()),
            project_table_eq_limited_rows_cache: RwLock::new(HashMap::new()),
            project_table_range_rows_cache: RwLock::new(HashMap::new()),
            project_table_split_rows_cache: RwLock::new(HashMap::new()),
            project_table_unique_lookup_rows_cache: RwLock::new(HashMap::new()),
            hash_join_fast_rows_cache: RwLock::new(HashMap::new()),
            project_table_ordered_rows_cache: RwLock::new(HashMap::new()),
            aggregate_rows_cache: RwLock::new(HashMap::new()),
        }
    }

    pub fn with_fragment_dispatcher(
        mut self,
        fragment_dispatcher: Arc<dyn FragmentDispatcher>,
    ) -> Self {
        self.fragment_dispatcher = fragment_dispatcher;
        self
    }

    pub(super) fn register_expression_index_meta(
        &self,
        index_id: IndexId,
        meta: ExpressionIndexMeta,
    ) {
        if let Ok(mut guard) = self.expression_index_meta.lock() {
            guard.insert(index_id, meta);
        }
    }

    pub(super) fn expression_index_meta(&self, index_id: IndexId) -> Option<ExpressionIndexMeta> {
        self.expression_index_meta
            .lock()
            .ok()
            .and_then(|guard| guard.get(&index_id).cloned())
    }

    pub(super) fn forget_expression_index_meta(&self, index_id: IndexId) {
        if let Ok(mut guard) = self.expression_index_meta.lock() {
            guard.remove(&index_id);
        }
    }

    pub(super) fn clear_graph_neighbor_meta_cache(&self) {
        if let Ok(mut guard) = self.graph_neighbor_meta.lock() {
            guard.clear();
        }
    }

    pub(super) fn register_table_inheritance(
        &self,
        child_table_id: RelationId,
        parent_table_ids: &[RelationId],
    ) {
        if parent_table_ids.is_empty() {
            return;
        }
        if let Ok(mut guard) = self.table_inheritance_meta.lock() {
            {
                let parents = guard.child_to_parents.entry(child_table_id).or_default();
                for parent_table_id in parent_table_ids {
                    parents.insert(*parent_table_id);
                }
            }
            for parent_table_id in parent_table_ids {
                guard
                    .parent_to_children
                    .entry(*parent_table_id)
                    .or_default()
                    .insert(child_table_id);
            }
        }
    }

    pub(super) fn drop_order_for_inheriting_children(
        &self,
        parent_table_id: RelationId,
    ) -> Vec<RelationId> {
        let mut visited = BTreeSet::new();
        let mut order = Vec::new();
        if let Ok(guard) = self.table_inheritance_meta.lock() {
            let mut stack = vec![(parent_table_id, false)];
            while let Some((table_id, exiting)) = stack.pop() {
                if exiting {
                    if table_id != parent_table_id {
                        order.push(table_id);
                    }
                    continue;
                }
                if !visited.insert(table_id) {
                    continue;
                }
                stack.push((table_id, true));
                if let Some(children) = guard.parent_to_children.get(&table_id) {
                    for child in children.iter().rev() {
                        stack.push((*child, false));
                    }
                }
            }
        }
        order
    }

    pub(super) fn unregister_table_inheritance(&self, table_id: RelationId) {
        if let Ok(mut guard) = self.table_inheritance_meta.lock() {
            if let Some(parent_ids) = guard.child_to_parents.remove(&table_id) {
                for parent_id in parent_ids {
                    let mut remove_parent_entry = false;
                    if let Some(children) = guard.parent_to_children.get_mut(&parent_id) {
                        children.remove(&table_id);
                        remove_parent_entry = children.is_empty();
                    }
                    if remove_parent_entry {
                        guard.parent_to_children.remove(&parent_id);
                    }
                }
            }
            if let Some(child_ids) = guard.parent_to_children.remove(&table_id) {
                for child_id in child_ids {
                    let mut remove_child_entry = false;
                    if let Some(parents) = guard.child_to_parents.get_mut(&child_id) {
                        parents.remove(&table_id);
                        remove_child_entry = parents.is_empty();
                    }
                    if remove_child_entry {
                        guard.child_to_parents.remove(&child_id);
                    }
                }
            }
        }
    }

    fn with_internal_rewrite_savepoint<T>(
        &self,
        context: &ExecutionContext,
        operation: &str,
        apply: impl FnOnce() -> DbResult<T>,
    ) -> DbResult<T> {
        if context.txn_id == TxnId::default() {
            return apply();
        }

        let txn_id = context.txn_id;
        let storage_savepoint_id = self.storage_txn.create_savepoint(txn_id)?;
        let catalog_savepoint_id = match self.catalog_txn.create_savepoint(txn_id) {
            Ok(savepoint_id) => savepoint_id,
            Err(error) => {
                let mut cleanup_errors = Vec::new();
                self.record_cleanup_error(
                    operation,
                    "rollback storage savepoint after catalog savepoint creation failure",
                    self.storage_txn
                        .rollback_to_savepoint(txn_id, storage_savepoint_id),
                    &mut cleanup_errors,
                );
                self.record_cleanup_error(
                    operation,
                    "release storage savepoint after catalog savepoint creation failure",
                    self.storage_txn
                        .release_savepoint(txn_id, storage_savepoint_id),
                    &mut cleanup_errors,
                );
                return if cleanup_errors.is_empty() {
                    Err(error)
                } else {
                    Err(error.with_internal_detail(format!(
                        "{operation}: internal savepoint cleanup failed: {}",
                        cleanup_errors.join("; ")
                    )))
                };
            }
        };

        match apply() {
            Ok(value) => {
                let mut cleanup_errors = Vec::new();
                self.record_cleanup_error(
                    operation,
                    "release storage savepoint",
                    self.storage_txn
                        .release_savepoint(txn_id, storage_savepoint_id),
                    &mut cleanup_errors,
                );
                self.record_cleanup_error(
                    operation,
                    "release catalog savepoint",
                    self.catalog_txn
                        .release_savepoint(txn_id, catalog_savepoint_id),
                    &mut cleanup_errors,
                );
                if cleanup_errors.is_empty() {
                    Ok(value)
                } else {
                    Err(DbError::internal(format!(
                        "{operation}: failed to release internal rewrite savepoint"
                    ))
                    .with_internal_detail(cleanup_errors.join("; ")))
                }
            }
            Err(error) => {
                let mut cleanup_errors = Vec::new();
                self.record_cleanup_error(
                    operation,
                    "rollback storage savepoint",
                    self.storage_txn
                        .rollback_to_savepoint(txn_id, storage_savepoint_id),
                    &mut cleanup_errors,
                );
                self.record_cleanup_error(
                    operation,
                    "rollback catalog savepoint",
                    self.catalog_txn
                        .rollback_to_savepoint(txn_id, catalog_savepoint_id),
                    &mut cleanup_errors,
                );
                self.record_cleanup_error(
                    operation,
                    "release storage savepoint after rollback",
                    self.storage_txn
                        .release_savepoint(txn_id, storage_savepoint_id),
                    &mut cleanup_errors,
                );
                self.record_cleanup_error(
                    operation,
                    "release catalog savepoint after rollback",
                    self.catalog_txn
                        .release_savepoint(txn_id, catalog_savepoint_id),
                    &mut cleanup_errors,
                );

                if cleanup_errors.is_empty() {
                    Err(error.with_internal_detail(Self::INTERNAL_SAVEPOINT_RECOVERY_MARKER))
                } else {
                    Err(error.with_internal_detail(format!(
                        "{operation}: internal savepoint cleanup failed: {}",
                        cleanup_errors.join("; ")
                    )))
                }
            }
        }
    }

    fn record_cleanup_error(
        &self,
        operation: &str,
        step: &str,
        result: DbResult<()>,
        errors: &mut Vec<String>,
    ) {
        if let Err(error) = result {
            warn!(operation, step, error = %error, "internal rewrite savepoint cleanup failed");
            errors.push(format!("{step}: {error}"));
        }
    }

    fn compile_logical_plan(
        &self,
        plan: &aiondb_plan::LogicalPlan,
        context: &ExecutionContext,
    ) -> DbResult<PhysicalPlan> {
        (self.logical_plan_compiler)(plan, context)
    }

    fn generate_series_row_limit_error() -> DbError {
        DbError::program_limit(format!(
            "generate_series produces too many rows (limit: {})",
            Self::GENERATE_SERIES_MAX_ROWS
        ))
    }

    fn try_fast_generate_series_hybrid_scan(
        &self,
        function_name: &str,
        args: &[TypedExpr],
        output_fields: &[aiondb_plan::ResultField],
        context: &ExecutionContext,
    ) -> DbResult<Option<ExecutionResult>> {
        let base_function_name = function_name.rsplit('.').next().unwrap_or(function_name);
        if !base_function_name.eq_ignore_ascii_case("generate_series") {
            return Ok(None);
        }
        if args.len() < 2 || args.len() > 3 {
            return Ok(None);
        }

        let mut values = Vec::with_capacity(args.len());
        for arg in args {
            values.push(self.evaluate_expr(arg, context)?);
        }
        if values.iter().any(Value::is_null) {
            return Ok(Some(ExecutionResult::Query {
                columns: output_fields.to_vec(),
                rows: Vec::new(),
            }));
        }

        let has_bigint = values.iter().any(|v| matches!(v, Value::BigInt(_)));
        if has_bigint {
            let to_i64 = |v: &Value| match v {
                Value::Int(n) => Ok(i64::from(*n)),
                Value::BigInt(n) => Ok(*n),
                _ => Err(DbError::internal(
                    "generate_series: unsupported argument type",
                )),
            };
            let start = to_i64(&values[0])?;
            let stop = to_i64(&values[1])?;
            let step = if values.len() == 3 {
                to_i64(&values[2])?
            } else if start <= stop {
                1
            } else {
                -1
            };
            if step == 0 {
                return Err(DbError::internal("step size cannot equal zero"));
            }

            let mut rows = Vec::new();
            let mut current = start;
            if step > 0 {
                while current <= stop {
                    if rows.len() >= Self::GENERATE_SERIES_MAX_ROWS {
                        return Err(Self::generate_series_row_limit_error());
                    }
                    if rows.len().trailing_zeros() >= 10 {
                        context.check_deadline()?;
                    }
                    rows.push(Row::new(vec![Value::BigInt(current)]));
                    current = match current.checked_add(step) {
                        Some(next) => next,
                        None => break,
                    };
                }
            } else {
                while current >= stop {
                    if rows.len() >= Self::GENERATE_SERIES_MAX_ROWS {
                        return Err(Self::generate_series_row_limit_error());
                    }
                    if rows.len().trailing_zeros() >= 10 {
                        context.check_deadline()?;
                    }
                    rows.push(Row::new(vec![Value::BigInt(current)]));
                    current = match current.checked_add(step) {
                        Some(next) => next,
                        None => break,
                    };
                }
            }

            return Ok(Some(ExecutionResult::Query {
                columns: output_fields.to_vec(),
                rows,
            }));
        }

        let to_i32 = |v: &Value| match v {
            Value::Int(n) => Ok(*n),
            _ => Err(DbError::internal(
                "generate_series: unsupported argument type",
            )),
        };
        let start = to_i32(&values[0])?;
        let stop = to_i32(&values[1])?;
        let step = if values.len() == 3 {
            to_i32(&values[2])?
        } else if start <= stop {
            1
        } else {
            -1
        };
        if step == 0 {
            return Err(DbError::internal("step size cannot equal zero"));
        }

        let mut rows = Vec::new();
        let mut current = start;
        if step > 0 {
            while current <= stop {
                if rows.len() >= Self::GENERATE_SERIES_MAX_ROWS {
                    return Err(Self::generate_series_row_limit_error());
                }
                if rows.len().trailing_zeros() >= 10 {
                    context.check_deadline()?;
                }
                rows.push(Row::new(vec![Value::Int(current)]));
                current = match current.checked_add(step) {
                    Some(next) => next,
                    None => break,
                };
            }
        } else {
            while current >= stop {
                if rows.len() >= Self::GENERATE_SERIES_MAX_ROWS {
                    return Err(Self::generate_series_row_limit_error());
                }
                if rows.len().trailing_zeros() >= 10 {
                    context.check_deadline()?;
                }
                rows.push(Row::new(vec![Value::Int(current)]));
                current = match current.checked_add(step) {
                    Some(next) => next,
                    None => break,
                };
            }
        }

        Ok(Some(ExecutionResult::Query {
            columns: output_fields.to_vec(),
            rows,
        }))
    }

    fn try_fast_graph_neighbors_hybrid_scan(
        &self,
        function_name: &str,
        args: &[TypedExpr],
        output_fields: &[aiondb_plan::ResultField],
        context: &ExecutionContext,
    ) -> DbResult<Option<ExecutionResult>> {
        let base_function_name = function_name.rsplit('.').next().unwrap_or(function_name);
        if !base_function_name.eq_ignore_ascii_case("graph_neighbors") {
            return Ok(None);
        }

        let rows = self.resolve_graph_neighbors_rows(args, None, context)?;
        Ok(Some(ExecutionResult::Query {
            columns: output_fields.to_vec(),
            rows,
        }))
    }

    pub fn catalog_reader(&self) -> &dyn CatalogReader {
        self.catalog_reader.as_ref()
    }

    pub(super) fn compile_index_expression(
        &self,
        expression_sql: &str,
        table: &TableDescriptor,
        context: &ExecutionContext,
    ) -> DbResult<TypedExpr> {
        let parsed = aiondb_parser::parse_expression(expression_sql).map_err(|error| {
            DbError::bind_error(
                SqlState::SyntaxError,
                format!("invalid index expression: {error}"),
            )
        })?;
        let search_path_schemas = session_search_path_schemas(context);
        let current_schema = search_path_schemas.first().cloned();
        let current_user = context.current_user_name();
        aiondb_planner::type_check::TypeChecker::new(Arc::clone(&self.catalog_reader))
            .with_session_context(current_user.clone(), current_user, current_schema, None)
            .with_search_path_schemas(search_path_schemas.into())
            .type_check_expression_with_relation(&parsed, table, context.txn_id)
    }

    pub fn evaluate_typed_expr_with_row(&self, expr: &TypedExpr, row: &Row) -> DbResult<Value> {
        self.evaluator.evaluate_with_row(expr, row)
    }

    pub fn validate_sql_function_definition(
        &self,
        language: &str,
        body: &str,
        params: &[(String, DataType)],
        return_type: &DataType,
    ) -> DbResult<()> {
        if language.eq_ignore_ascii_case("sql") {
            // For SQL language, try to validate the body at CREATE time.
            // If validation fails, accept the definition anyway - the
            // function will produce an error when it is actually called.
            // This matches PostgreSQL behaviour for SQL-standard function
            // bodies (BEGIN ATOMIC … END) and complex multi-statement
            // bodies that AionDB cannot yet execute inline.  Accepting
            // them prevents cascading failures in test suites that define
            // helper functions early and reference them later.
            let _ = self.compile_sql_function_body(body, params, return_type, None);
            Ok(())
        } else if language.eq_ignore_ascii_case("plpgsql")
            || language.eq_ignore_ascii_case("c")
            || language.eq_ignore_ascii_case("internal")
        {
            // For plpgsql/C/internal: accept the function definition at
            // CREATE time without validating the body.  The body will be
            // interpreted at CALL time - simple SQL-only plpgsql bodies
            // (e.g. BEGIN ... RETURN expr; END) are handled by extracting
            // the SQL expression; complex control flow will produce a
            // clear error at invocation rather than at definition.
            Ok(())
        } else {
            // Accept any other language as well - the function body is
            // opaque and will be rejected at call time if the language is
            // truly unsupported.  This prevents creation-time failures
            // for extensions that register custom languages.
            Ok(())
        }
    }

    pub fn execute(
        &self,
        plan: &PhysicalPlan,
        context: &ExecutionContext,
    ) -> DbResult<ExecutionResult> {
        let owns_in_subquery_cache = STATEMENT_IN_SUBQUERY_CACHE.with(|cache| {
            let mut cache = cache.borrow_mut();
            if cache.is_some() {
                false
            } else {
                *cache = Some(HashMap::new());
                true
            }
        });
        let owns_value_subquery_cache = STATEMENT_VALUE_SUBQUERY_CACHE.with(|cache| {
            let mut cache = cache.borrow_mut();
            if cache.is_some() {
                false
            } else {
                *cache = Some(HashMap::new());
                true
            }
        });
        let owns_correlated_exists_cache = STATEMENT_CORRELATED_EXISTS_CACHE.with(|cache| {
            let mut cache = cache.borrow_mut();
            if cache.is_some() {
                false
            } else {
                *cache = Some(HashMap::new());
                true
            }
        });
        let owns_exists_semijoin_cache = STATEMENT_EXISTS_SEMIJOIN_CACHE.with(|cache| {
            let mut cache = cache.borrow_mut();
            if cache.is_some() {
                false
            } else {
                *cache = Some(HashMap::new());
                true
            }
        });
        let owns_scalar_agg_semijoin_cache = STATEMENT_SCALAR_AGG_SEMIJOIN_CACHE.with(|cache| {
            let mut cache = cache.borrow_mut();
            if cache.is_some() {
                false
            } else {
                *cache = Some(HashMap::new());
                true
            }
        });
        // Use a Drop guard so the cache is cleaned up even on panic,
        // preventing stale data from leaking into subsequent executions
        // on the same thread.
        struct CacheGuard {
            owns_in_subquery_cache: bool,
            owns_value_subquery_cache: bool,
            owns_correlated_exists_cache: bool,
            owns_exists_semijoin_cache: bool,
            owns_scalar_agg_semijoin_cache: bool,
        }
        impl Drop for CacheGuard {
            fn drop(&mut self) {
                if self.owns_in_subquery_cache {
                    STATEMENT_IN_SUBQUERY_CACHE.with(|cache| {
                        *cache.borrow_mut() = None;
                    });
                }
                if self.owns_value_subquery_cache {
                    STATEMENT_VALUE_SUBQUERY_CACHE.with(|cache| {
                        *cache.borrow_mut() = None;
                    });
                }
                if self.owns_correlated_exists_cache {
                    STATEMENT_CORRELATED_EXISTS_CACHE.with(|cache| {
                        *cache.borrow_mut() = None;
                    });
                }
                if self.owns_exists_semijoin_cache {
                    STATEMENT_EXISTS_SEMIJOIN_CACHE.with(|cache| {
                        *cache.borrow_mut() = None;
                    });
                }
                if self.owns_scalar_agg_semijoin_cache {
                    STATEMENT_SCALAR_AGG_SEMIJOIN_CACHE.with(|cache| {
                        *cache.borrow_mut() = None;
                    });
                }
            }
        }
        let _cache_guard = CacheGuard {
            owns_in_subquery_cache,
            owns_value_subquery_cache,
            owns_correlated_exists_cache,
            owns_exists_semijoin_cache,
            owns_scalar_agg_semijoin_cache,
        };
        let eval_session = eval_session_context(context);
        with_session_context(eval_session, || self.execute_with_session(plan, context))
    }

    pub fn execute_distributed_fragments(
        &self,
        fragments: &[PhysicalPlan],
        context: &ExecutionContext,
    ) -> DbResult<ExecutionResult> {
        let distributed_fragments = fragments
            .iter()
            .cloned()
            .map(DistributedFragment::local)
            .collect::<Vec<_>>();
        self.execute_distributed_fragments_targeted(&distributed_fragments, context)
    }

    pub fn execute_distributed_fragments_targeted(
        &self,
        fragments: &[DistributedFragment],
        context: &ExecutionContext,
    ) -> DbResult<ExecutionResult> {
        context.check_deadline()?;
        if fragments.is_empty() {
            return Ok(ExecutionResult::Query {
                columns: Vec::new(),
                rows: Vec::new(),
            });
        }

        let worker_count = context.parallel_workers_for(fragments.len());
        let results = if worker_count > 1 && fragments.len() > 1 {
            let fragment_dispatcher = Arc::clone(&self.fragment_dispatcher);
            std::thread::scope(|scope| {
                let chunk_size = fragments.len().div_ceil(worker_count);
                let mut handles = Vec::new();
                for (chunk_idx, chunk) in fragments.chunks(chunk_size).enumerate() {
                    let chunk_start = chunk_idx * chunk_size;
                    let worker_context = context.clone();
                    let fragment_dispatcher = Arc::clone(&fragment_dispatcher);
                    handles.push(scope.spawn(move || {
                        let mut chunk_results = Vec::with_capacity(chunk.len());
                        for (offset, fragment) in chunk.iter().enumerate() {
                            worker_context.check_deadline()?;
                            let fragment_index = chunk_start + offset;
                            let fragment_result = fragment_dispatcher
                                .execute_fragment(fragment, self, &worker_context)
                                .map_err(|error| {
                                    annotate_fragment_execution_error(
                                        error,
                                        fragment_index,
                                        &fragment.target,
                                    )
                                })?;
                            chunk_results.push((
                                fragment_index,
                                fragment.target.clone(),
                                fragment.partition.clone(),
                                fragment_result,
                            ));
                        }
                        Ok::<_, DbError>(chunk_results)
                    }));
                }

                let mut ordered_results = Vec::with_capacity(fragments.len());
                for handle in handles {
                    let chunk_results = handle.join().map_err(|_| {
                        DbError::internal("distributed fragment worker thread panicked")
                    })??;
                    ordered_results.extend(chunk_results);
                }
                Ok::<_, DbError>(ordered_results)
            })?
        } else {
            let fragment_dispatcher = Arc::clone(&self.fragment_dispatcher);
            let mut ordered_results = Vec::with_capacity(fragments.len());
            for (fragment_index, fragment) in fragments.iter().enumerate() {
                context.check_deadline()?;
                let fragment_result = fragment_dispatcher
                    .execute_fragment(fragment, self, context)
                    .map_err(|error| {
                        annotate_fragment_execution_error(error, fragment_index, &fragment.target)
                    })?;
                ordered_results.push((
                    fragment_index,
                    fragment.target.clone(),
                    fragment.partition.clone(),
                    fragment_result,
                ));
            }
            ordered_results
        };

        let mut merged_columns: Option<Vec<aiondb_plan::ResultField>> = None;
        let mut merged_rows = Vec::new();
        let mut result_bytes = 0u64;
        let mut merged_row_width: Option<usize> = None;
        for (fragment_index, fragment_target, partition, result) in results {
            context.check_deadline()?;
            let ExecutionResult::Query { columns, rows } = result else {
                return Err(DbError::internal(
                    "distributed fragment did not return a query result",
                )
                .with_internal_detail(format!(
                    "fragment #{} target={} produced a non-query execution result",
                    fragment_index,
                    format_fragment_target(&fragment_target)
                )));
            };
            let mut rows = rows;
            if let Some(expected) = merged_columns.as_ref() {
                if !result_schema_compatible(expected, &columns) {
                    rows = coerce_rows_to_expected_schema(rows, expected).map_err(|error| {
                        DbError::internal(
                            "distributed fragments produced incompatible result schemas",
                        )
                        .with_internal_detail(format!(
                            "fragment #{} target={} expected_schema=[{}] actual_schema=[{}]; {}",
                            fragment_index,
                            format_fragment_target(&fragment_target),
                            format_result_schema_signature(expected),
                            format_result_schema_signature(&columns),
                            error
                                .report()
                                .internal_detail
                                .clone()
                                .unwrap_or_else(|| { error.report().message.clone() })
                        ))
                    })?;
                }
            } else {
                merged_columns = Some(columns);
            }

            // Hash partition filtering is only correct for shared-storage
            // loopback fragments. Real remote nodes are expected to scan their
            let rows: Vec<Row> = match &partition {
                Some(p) if context.distributed_hash_partitioning_enabled() => rows
                    .into_iter()
                    .filter(|row| {
                        distributed_fragments::hash_partition_for_row(row, p.count) == p.index
                    })
                    .collect(),
                Some(_) | None => rows,
            };

            for (row_index, row) in rows.into_iter().enumerate() {
                context.check_deadline()?;
                if let Some(expected_width) = merged_row_width {
                    if row.values.len() != expected_width {
                        return Err(DbError::internal(
                            "distributed fragments produced inconsistent row widths",
                        )
                        .with_internal_detail(format!(
                            "fragment #{} target={} row_index={} expected_row_width={} actual_row_width={}",
                            fragment_index,
                            format_fragment_target(&fragment_target),
                            row_index,
                            expected_width,
                            row.values.len()
                        )));
                    }
                } else {
                    merged_row_width = Some(row.values.len());
                }
                if usize_to_u64(merged_rows.len()) >= context.max_result_rows {
                    return Err(DbError::program_limit(
                        "maximum number of result rows reached",
                    ));
                }
                result_bytes =
                    ensure_result_bytes_fit_and_track_query_row(context, &row, result_bytes)?;
                merged_rows.push(row);
            }
        }

        Ok(ExecutionResult::Query {
            columns: merged_columns.unwrap_or_default(),
            rows: merged_rows,
        })
    }

    fn execute_with_session(
        &self,
        plan: &PhysicalPlan,
        context: &ExecutionContext,
    ) -> DbResult<ExecutionResult> {
        let side_effect_cache_key = side_effect_dml_cache_key(plan);
        if let Some(cache_key) = side_effect_cache_key {
            if let Some(cached) = context
                .side_effect_query_cache
                .lock()
                .map_err(|error| {
                    DbError::internal(format!("side-effect query cache poisoned: {error}"))
                })?
                .get(&cache_key)
                .cloned()
            {
                return Ok(cached);
            }
        }

        let result = match plan {
            PhysicalPlan::ProjectOnce { .. }
            | PhysicalPlan::ProjectTable { .. }
            | PhysicalPlan::LockingProjectTable { .. }
            | PhysicalPlan::ProjectValues { .. } => self.execute_projection_plan(plan, context),
            PhysicalPlan::ProjectSource { .. } => self.execute_project_source_plan(plan, context),
            PhysicalPlan::CreateTable { .. }
            | PhysicalPlan::CreateTableAs { .. }
            | PhysicalPlan::CreateSequence { .. }
            | PhysicalPlan::CreateIndex { .. }
            | PhysicalPlan::DropTable { .. }
            | PhysicalPlan::DropIndex { .. }
            | PhysicalPlan::DropSequence { .. }
            | PhysicalPlan::InsertValues { .. }
            | PhysicalPlan::InsertSelect { .. }
            | PhysicalPlan::DeleteFromTable { .. }
            | PhysicalPlan::UpdateTable { .. }
            | PhysicalPlan::AlterTableAddColumn { .. }
            | PhysicalPlan::AlterTableDropColumn { .. }
            | PhysicalPlan::AlterTableRename { .. }
            | PhysicalPlan::AlterTableRenameColumn { .. }
            | PhysicalPlan::AlterTableSetDefault { .. }
            | PhysicalPlan::AlterTableDropDefault { .. }
            | PhysicalPlan::AlterTableSetNotNull { .. }
            | PhysicalPlan::AlterTableDropNotNull { .. }
            | PhysicalPlan::AlterTableAddConstraint { .. }
            | PhysicalPlan::AlterTableDropConstraint { .. }
            | PhysicalPlan::AlterTableAlterColumnType { .. }
            | PhysicalPlan::CreateView { .. }
            | PhysicalPlan::DropView { .. }
            | PhysicalPlan::CopyFrom { .. }
            | PhysicalPlan::CopyTo { .. }
            | PhysicalPlan::CreateNodeLabel { .. }
            | PhysicalPlan::CreateEdgeLabel { .. }
            | PhysicalPlan::DropNodeLabel { .. }
            | PhysicalPlan::DropEdgeLabel { .. }
            | PhysicalPlan::CreateRole { .. }
            | PhysicalPlan::DropRole { .. }
            | PhysicalPlan::AlterRole { .. }
            | PhysicalPlan::Grant { .. }
            | PhysicalPlan::Revoke { .. }
            | PhysicalPlan::Analyze { .. }
            | PhysicalPlan::Vacuum { .. }
            | PhysicalPlan::Checkpoint
            | PhysicalPlan::Lock { .. }
            | PhysicalPlan::InternalNoOp { .. }
            | PhysicalPlan::PgCompatUtility { .. }
            | PhysicalPlan::Discard { .. }
            | PhysicalPlan::TruncateTable { .. }
            | PhysicalPlan::CreateSchema { .. }
            | PhysicalPlan::DropSchema { .. }
            | PhysicalPlan::PgObjectCommand { .. } => self.execute_command_plan(plan, context),
            PhysicalPlan::CypherQuery(ref cypher_plan) => {
                self.execute_cypher_query(cypher_plan.as_ref(), context)
            }
            PhysicalPlan::MergeTable(_) => self.execute_dml_plan(plan, context),
            PhysicalPlan::RecursiveCte { .. } => Err(DbError::internal(
                "recursive CTEs are materialized at the engine level and should not reach the executor",
            )),
            PhysicalPlan::NestedLoopJoin { .. }
            | PhysicalPlan::NestedLoopIndexJoin { .. }
            | PhysicalPlan::HashJoin { .. }
            | PhysicalPlan::MergeJoin { .. } => self.execute_join_plan(plan, context),
            PhysicalPlan::SeqScan { table_id } => {
                // SeqScan is used as a leaf inside join trees. When executed
                // standalone it returns all rows with all columns.
                context.check_deadline()?;
                match self.scan_table_locked(context, *table_id, None) {
                    Ok(mut stream) => {
                        let include_oid_system_column =
                            self.compat_include_oid_system_column_for_table_id(context, *table_id)?;
                        let mut rows = Vec::new();
                        while let Some(record) = stream.next()? {
                            context.check_deadline()?;
                            // record is owned and unused after this call -
                            // consume it to skip the values-Vec clone in
                            // `compat_scan_row`.
                            rows.push(self.compat_scan_row_consume(
                                record,
                                include_oid_system_column,
                                Some(*table_id),
                            ));
                        }
                        Ok(ExecutionResult::Query {
                            columns: Vec::new(),
                            rows,
                        })
                    }
                    Err(error) => {
                        // Virtual tables (pg_catalog/information_schema) are
                        // normally rewritten to ProjectValues before execution;
                        // tolerate the missing physical storage only for those.
                        // Real user tables must propagate scan errors.
                        if aiondb_planner::is_virtual_synthetic_relation(table_id.get()) {
                            Ok(ExecutionResult::Query {
                                columns: Vec::new(),
                                rows: Vec::new(),
                            })
                        } else {
                            Err(error)
                        }
                    }
                }
            }
            PhysicalPlan::Aggregate { .. }
            | PhysicalPlan::SetOperation { .. }
            | PhysicalPlan::DistributedAppend { .. } => {
                self.execute_aggregate_or_set_plan(plan, context)
            }
            PhysicalPlan::AggregateSource { .. } => {
                self.execute_aggregate_source_plan(plan, context)
            }
            PhysicalPlan::HnswScan {
                table_id,
                index_id,
                query_vector,
                limit,
                ef_search,
                projected_ordinals,
                output_fields,
                ..
            } => {
                context.check_deadline()?;
                // Derive a max search duration from the executor's statement
                // deadline so the HNSW search can abort early on timeout.
                let max_search_duration = context
                    .statement_deadline
                    .and_then(|dl| dl.checked_duration_since(std::time::Instant::now()));
                let mut stream = self.vector_search_locked(
                    context,
                    *table_id,
                    *index_id,
                    query_vector,
                    *limit,
                    *ef_search,
                    None,
                    max_search_duration,
                )?;
                let mut rows = Vec::new();
                while let Some(record) = stream.next()? {
                    context.check_deadline()?;
                    let projected_values = projected_ordinals
                        .iter()
                        .map(|ordinal| {
                            record
                                .row
                                .values
                                .get(*ordinal)
                                .cloned()
                                .unwrap_or(Value::Null)
                        })
                        .collect();
                    rows.push(Row::new(projected_values));
                }
                Ok(ExecutionResult::Query {
                    columns: output_fields.clone(),
                    rows,
                })
            }
            PhysicalPlan::HybridFunctionScan {
                function_name,
                args,
                output_fields,
            } => {
                if let Some(result) = self.try_fast_generate_series_hybrid_scan(
                    function_name.as_str(),
                    args,
                    output_fields,
                    context,
                )? {
                    return Ok(result);
                }
                if let Some(result) = self.try_fast_graph_neighbors_hybrid_scan(
                    function_name.as_str(),
                    args,
                    output_fields,
                    context,
                )? {
                    return Ok(result);
                }
                let Some(output_field) = output_fields.first() else {
                    return Err(DbError::internal(
                        "HybridFunctionScan requires at least one output field",
                    ));
                };
                let expr = TypedExpr::scalar_function(
                    ScalarFunction::Generic(function_name.clone()),
                    args.clone(),
                    output_field.data_type.clone(),
                    output_field.nullable,
                );
                let value = self.evaluate_expr(&expr, context)?;
                let rows = match value {
                    Value::Array(elements) => elements
                        .into_iter()
                        .map(|value| Row::new(vec![value]))
                        .collect(),
                    Value::Null => Vec::new(),
                    value => vec![Row::new(vec![value])],
                };
                Ok(ExecutionResult::Query {
                    columns: output_fields.clone(),
                    rows,
                })
            }
            PhysicalPlan::DistributedScan {
                table_id,
                ref outputs,
                ref filter,
                ref output_fields,
                node_count,
            } => distributed_scan::execute_distributed_scan_plan(
                self,
                *table_id,
                outputs,
                filter,
                output_fields,
                *node_count,
                context,
            ),
            PhysicalPlan::PartialAggregate {
                ref source,
                ref group_by,
                ref aggregates,
                ref output_fields,
            } => distributed_aggregate::execute_partial_aggregate_plan(
                self,
                source,
                group_by,
                aggregates,
                output_fields,
                context,
            ),
            PhysicalPlan::FinalAggregate {
                ref partials,
                ref group_by,
                ref aggregates,
                ref having,
                ref output_fields,
                ref order_by,
                ref limit,
                ref offset,
            } => distributed_aggregate::execute_final_aggregate_plan(
                self,
                partials,
                group_by,
                aggregates,
                having,
                output_fields,
                order_by,
                limit,
                offset,
                context,
            ),
            PhysicalPlan::BroadcastHashJoin {
                ref broadcast,
                ref local,
                ref join_type,
                ref left_keys,
                ref right_keys,
                ref condition,
                ref outputs,
                ref output_fields,
            } => distributed_join::execute_broadcast_hash_join_plan(
                self,
                broadcast,
                local,
                join_type,
                left_keys,
                right_keys,
                condition.as_ref(),
                outputs,
                output_fields,
                context,
            ),
            PhysicalPlan::Gather {
                ref child,
                ref num_workers,
                ref output_fields,
                ..
            } => {
                let num_workers = *num_workers;
                context.check_deadline()?;
                let cap = context.max_parallel_workers_per_query.max(1);
                let requested = if num_workers == 0 { cap } else { num_workers };
                let workers = requested.min(cap).max(1);
                if workers <= 1
                    || context.parallel_scan_partition.is_some()
                    || !plan_contains_partitionable_seq_scan(child)
                {
                    // Either parallelism disabled or we're already nested in a
                    // Gather worker — run child once on this thread.
                    match self.execute(child, context)? {
                        ExecutionResult::Query { rows, .. } => Ok(ExecutionResult::Query {
                            columns: output_fields.clone(),
                            rows,
                        }),
                        other => Ok(other),
                    }
                } else {
                    let columns = output_fields.clone();
                    let rows = execute_gather_parallel(self, child, context, workers)?;
                    Ok(ExecutionResult::Query { columns, rows })
                }
            }
        }?;

        if let Some(cache_key) = side_effect_cache_key {
            context
                .side_effect_query_cache
                .lock()
                .map_err(|error| {
                    DbError::internal(format!("side-effect query cache poisoned: {error}"))
                })?
                .insert(cache_key, result.clone());
        }

        Ok(result)
    }

    fn evaluate_expr(&self, expr: &TypedExpr, context: &ExecutionContext) -> DbResult<Value> {
        self.evaluator
            .evaluate_with_resolver(expr, &|expr| self.resolve_special_expr(expr, None, context))
    }

    fn lock_table(
        &self,
        context: &ExecutionContext,
        table_id: RelationId,
        mode: LockMode,
    ) -> DbResult<()> {
        context.acquire_table_lock(table_id, mode)
    }

    fn scan_table_locked(
        &self,
        context: &ExecutionContext,
        table_id: RelationId,
        projected_columns: Option<Vec<ColumnId>>,
    ) -> DbResult<Box<dyn TupleStream>> {
        self.scan_table_locked_at_snapshot(context, table_id, projected_columns, &context.snapshot)
    }

    fn scan_table_locked_at_snapshot(
        &self,
        context: &ExecutionContext,
        table_id: RelationId,
        projected_columns: Option<Vec<ColumnId>>,
        snapshot: &aiondb_tx::Snapshot,
    ) -> DbResult<Box<dyn TupleStream>> {
        let mode = if context.isolation == aiondb_tx::IsolationLevel::Serializable {
            LockMode::PredicateRead
        } else {
            LockMode::AccessShare
        };
        self.lock_table(context, table_id, mode)?;
        context.record_relation_read(table_id)?;
        let stream = if let Some(shard_id) = context.distributed_current_shard_id {
            self.storage_dml.scan_table_shard(
                context.txn_id,
                snapshot,
                table_id,
                shard_id,
                projected_columns,
            )?
        } else {
            self.storage_dml
                .scan_table(context.txn_id, snapshot, table_id, projected_columns)?
        };
        Ok(apply_parallel_scan_partition(stream, context))
    }

    fn scan_index_locked(
        &self,
        context: &ExecutionContext,
        table_id: RelationId,
        index_id: IndexId,
        key_range: KeyRange,
        projected_columns: Option<Vec<ColumnId>>,
    ) -> DbResult<Box<dyn TupleStream>> {
        self.scan_index_locked_at_snapshot(
            context,
            table_id,
            index_id,
            key_range,
            projected_columns,
            &context.snapshot,
        )
    }

    fn scan_index_locked_at_snapshot(
        &self,
        context: &ExecutionContext,
        table_id: RelationId,
        index_id: IndexId,
        key_range: KeyRange,
        projected_columns: Option<Vec<ColumnId>>,
        snapshot: &aiondb_tx::Snapshot,
    ) -> DbResult<Box<dyn TupleStream>> {
        let mode = if context.isolation == aiondb_tx::IsolationLevel::Serializable {
            LockMode::PredicateRead
        } else {
            LockMode::AccessShare
        };
        self.lock_table(context, table_id, mode)?;
        context.record_relation_read(table_id)?;
        self.storage_dml.scan_index(
            context.txn_id,
            snapshot,
            index_id,
            key_range,
            projected_columns,
        )
    }

    fn scan_index_ordered_locked(
        &self,
        context: &ExecutionContext,
        table_id: RelationId,
        index_id: IndexId,
        key_range: KeyRange,
        projected_columns: Option<Vec<ColumnId>>,
        descending: bool,
    ) -> DbResult<Box<dyn TupleStream>> {
        let mode = if context.isolation == aiondb_tx::IsolationLevel::Serializable {
            LockMode::PredicateRead
        } else {
            LockMode::AccessShare
        };
        self.lock_table(context, table_id, mode)?;
        context.record_relation_read(table_id)?;
        self.storage_dml.scan_index_ordered(
            context.txn_id,
            &context.snapshot,
            index_id,
            key_range,
            projected_columns,
            descending,
        )
    }

    fn table_column_ids_for_ordinals(
        &self,
        context: &ExecutionContext,
        table_id: RelationId,
        ordinals: &[usize],
    ) -> DbResult<Option<Vec<ColumnId>>> {
        let Some(table) = self
            .catalog_reader
            .get_table_by_id(context.txn_id, table_id)?
        else {
            return Ok(None);
        };
        let mut column_ids = Vec::with_capacity(ordinals.len());
        for ordinal in ordinals {
            let Some(column) = table.columns.get(*ordinal) else {
                return Ok(None);
            };
            column_ids.push(column.column_id);
        }
        Ok(Some(column_ids))
    }

    fn gin_containment_search_locked(
        &self,
        context: &ExecutionContext,
        table_id: RelationId,
        index_id: IndexId,
        pattern: &serde_json::Value,
        visible_limit: Option<usize>,
    ) -> DbResult<Box<dyn TupleStream>> {
        let mode = if context.isolation == aiondb_tx::IsolationLevel::Serializable {
            LockMode::PredicateRead
        } else {
            LockMode::AccessShare
        };
        self.lock_table(context, table_id, mode)?;
        context.record_relation_read(table_id)?;
        match visible_limit {
            Some(limit) => self.storage_dml.gin_containment_search_limited(
                context.txn_id,
                &context.snapshot,
                index_id,
                pattern,
                limit,
            ),
            None => self.storage_dml.gin_containment_search(
                context.txn_id,
                &context.snapshot,
                index_id,
                pattern,
            ),
        }
    }

    /// Resolve a `ScanAccessPath` into a tuple stream.  The three index
    /// tolerate a missing physical table (virtual / `pg_catalog` tables) can
    /// wrap the result with `.unwrap_or_else(…)`.
    fn resolve_scan_stream(
        &self,
        context: &ExecutionContext,
        table_id: RelationId,
        access_path: &ScanAccessPath,
        projected_columns: Option<Vec<ColumnId>>,
    ) -> DbResult<Box<dyn TupleStream>> {
        self.resolve_scan_stream_at_snapshot(
            context,
            table_id,
            access_path,
            projected_columns,
            &context.snapshot,
        )
    }

    fn resolve_scan_stream_at_snapshot(
        &self,
        context: &ExecutionContext,
        table_id: RelationId,
        access_path: &ScanAccessPath,
        projected_columns: Option<Vec<ColumnId>>,
        snapshot: &aiondb_tx::Snapshot,
    ) -> DbResult<Box<dyn TupleStream>> {
        match access_path {
            ScanAccessPath::SeqScan => {
                self.scan_table_locked_at_snapshot(context, table_id, projected_columns, snapshot)
            }
            ScanAccessPath::IndexEq { index_id, value } => self.scan_index_locked_at_snapshot(
                context,
                table_id,
                *index_id,
                exact_lookup_key_range(value),
                projected_columns,
                snapshot,
            ),
            ScanAccessPath::IndexEqComposite { index_id, values } => self
                .scan_index_locked_at_snapshot(
                    context,
                    table_id,
                    *index_id,
                    composite_lookup_key_range(values),
                    projected_columns,
                    snapshot,
                ),
            ScanAccessPath::IndexEqRangeComposite {
                index_id,
                eq_values,
                lower,
                upper,
            } => self.scan_index_locked_at_snapshot(
                context,
                table_id,
                *index_id,
                composite_prefix_range_lookup_key_range(eq_values, lower, upper),
                projected_columns,
                snapshot,
            ),
            ScanAccessPath::IndexRange {
                index_id,
                lower,
                upper,
            } => self.scan_index_locked_at_snapshot(
                context,
                table_id,
                *index_id,
                range_lookup_key_range(lower, upper),
                projected_columns,
                snapshot,
            ),
            ScanAccessPath::GinContainment { index_id, pattern } => {
                self.gin_containment_search_locked(context, table_id, *index_id, pattern, None)
            }
            ScanAccessPath::BitmapOr { paths } => {
                self.execute_bitmap_scan(context, table_id, paths, projected_columns, false)
            }
            ScanAccessPath::BitmapAnd { paths } => {
                self.execute_bitmap_scan(context, table_id, paths, projected_columns, true)
            }
            ScanAccessPath::IndexOnlyScan {
                inner,
                index_column_ids,
            } => {
                // Execute the inner index scan and map the index-column order
                // either to the requested projection order or back into base
                // table ordinals when higher layers still need full scan rows
                // for filtering/sorting.
                let inner_stream = self.resolve_scan_stream_at_snapshot(
                    context,
                    table_id,
                    inner,
                    Some(index_column_ids.clone()),
                    snapshot,
                )?;
                if let Some(proj) = &projected_columns {
                    // Build a mapping from requested column IDs to positions
                    // in the index-column order.
                    let ordinal_map: Vec<Option<usize>> = proj
                        .iter()
                        .map(|col_id| index_column_ids.iter().position(|c| c == col_id))
                        .collect();
                    if let Some(ordinals) = ordinal_map.into_iter().collect::<Option<Vec<_>>>() {
                        return Ok(Box::new(RemapTupleStream {
                            inner: inner_stream,
                            ordinals,
                        }));
                    }
                }
                let table = self
                    .catalog_reader
                    .get_table_by_id(context.txn_id, table_id)?
                    .ok_or_else(|| {
                        DbError::internal(format!(
                            "table descriptor {table_id:?} missing for index-only scan"
                        ))
                    })?;
                let table_ordinals: Vec<usize> = index_column_ids
                    .iter()
                    .map(|column_id| {
                        table
                            .columns
                            .iter()
                            .position(|column| column.column_id == *column_id)
                            .ok_or_else(|| {
                                DbError::internal(format!(
                                    "index-only scan column {column_id:?} not found in table {table_id:?}"
                                ))
                            })
                    })
                    .collect::<DbResult<_>>()?;
                Ok(Box::new(ExpandIndexOnlyTupleStream {
                    inner: inner_stream,
                    table_width: table.columns.len(),
                    table_ordinals,
                }))
            }
        }
    }

    /// Attempt to resolve a scan stream that is already ordered by index key.
    ///
    /// Returns `Ok(None)` when the access path cannot preserve a stable index
    /// key order (for example `SeqScan`/bitmap/index-only wrappers).
    fn resolve_scan_stream_ordered(
        &self,
        context: &ExecutionContext,
        table_id: RelationId,
        access_path: &ScanAccessPath,
        projected_columns: Option<Vec<ColumnId>>,
        descending: bool,
    ) -> DbResult<Option<Box<dyn TupleStream>>> {
        let stream = match access_path {
            ScanAccessPath::IndexEq { index_id, value } => Some(self.scan_index_ordered_locked(
                context,
                table_id,
                *index_id,
                exact_lookup_key_range(value),
                projected_columns,
                descending,
            )?),
            ScanAccessPath::IndexEqComposite { index_id, values } => {
                Some(self.scan_index_ordered_locked(
                    context,
                    table_id,
                    *index_id,
                    composite_lookup_key_range(values),
                    projected_columns,
                    descending,
                )?)
            }
            ScanAccessPath::IndexEqRangeComposite {
                index_id,
                eq_values,
                lower,
                upper,
            } => Some(self.scan_index_ordered_locked(
                context,
                table_id,
                *index_id,
                composite_prefix_range_lookup_key_range(eq_values, lower, upper),
                projected_columns,
                descending,
            )?),
            ScanAccessPath::IndexRange {
                index_id,
                lower,
                upper,
            } => Some(self.scan_index_ordered_locked(
                context,
                table_id,
                *index_id,
                range_lookup_key_range(lower, upper),
                projected_columns,
                descending,
            )?),
            _ => None,
        };
        Ok(stream)
    }

    fn resolve_scan_stream_ordered_limited(
        &self,
        context: &ExecutionContext,
        table_id: RelationId,
        access_path: &ScanAccessPath,
        projected_columns: Option<Vec<ColumnId>>,
        descending: bool,
        limit: usize,
    ) -> DbResult<Option<Box<dyn TupleStream>>> {
        let mode = if context.isolation == aiondb_tx::IsolationLevel::Serializable {
            LockMode::PredicateRead
        } else {
            LockMode::AccessShare
        };
        self.lock_table(context, table_id, mode)?;
        context.record_relation_read(table_id)?;
        let stream = match access_path {
            ScanAccessPath::IndexEq { index_id, value } => {
                Some(self.storage_dml.scan_index_ordered_limited(
                    context.txn_id,
                    &context.snapshot,
                    *index_id,
                    exact_lookup_key_range(value),
                    projected_columns,
                    descending,
                    limit,
                )?)
            }
            ScanAccessPath::IndexEqComposite { index_id, values } => {
                Some(self.storage_dml.scan_index_ordered_limited(
                    context.txn_id,
                    &context.snapshot,
                    *index_id,
                    composite_lookup_key_range(values),
                    projected_columns,
                    descending,
                    limit,
                )?)
            }
            ScanAccessPath::IndexEqRangeComposite {
                index_id,
                eq_values,
                lower,
                upper,
            } => Some(self.storage_dml.scan_index_ordered_limited(
                context.txn_id,
                &context.snapshot,
                *index_id,
                composite_prefix_range_lookup_key_range(eq_values, lower, upper),
                projected_columns,
                descending,
                limit,
            )?),
            ScanAccessPath::IndexRange {
                index_id,
                lower,
                upper,
            } => Some(self.storage_dml.scan_index_ordered_limited(
                context.txn_id,
                &context.snapshot,
                *index_id,
                range_lookup_key_range(lower, upper),
                projected_columns,
                descending,
                limit,
            )?),
            _ => None,
        };
        Ok(stream)
    }

    fn resolve_scan_stream_limited(
        &self,
        context: &ExecutionContext,
        table_id: RelationId,
        access_path: &ScanAccessPath,
        projected_columns: Option<Vec<ColumnId>>,
        limit: usize,
    ) -> DbResult<Option<Box<dyn TupleStream>>> {
        let mode = if context.isolation == aiondb_tx::IsolationLevel::Serializable {
            LockMode::PredicateRead
        } else {
            LockMode::AccessShare
        };
        self.lock_table(context, table_id, mode)?;
        context.record_relation_read(table_id)?;
        let stream = match access_path {
            ScanAccessPath::IndexEq { index_id, value } => {
                Some(self.storage_dml.scan_index_limited(
                    context.txn_id,
                    &context.snapshot,
                    *index_id,
                    exact_lookup_key_range(value),
                    projected_columns,
                    limit,
                )?)
            }
            ScanAccessPath::IndexEqComposite { index_id, values } => {
                Some(self.storage_dml.scan_index_limited(
                    context.txn_id,
                    &context.snapshot,
                    *index_id,
                    composite_lookup_key_range(values),
                    projected_columns,
                    limit,
                )?)
            }
            ScanAccessPath::IndexEqRangeComposite {
                index_id,
                eq_values,
                lower,
                upper,
            } => Some(self.storage_dml.scan_index_limited(
                context.txn_id,
                &context.snapshot,
                *index_id,
                composite_prefix_range_lookup_key_range(eq_values, lower, upper),
                projected_columns,
                limit,
            )?),
            ScanAccessPath::IndexRange {
                index_id,
                lower,
                upper,
            } => Some(self.storage_dml.scan_index_limited(
                context.txn_id,
                &context.snapshot,
                *index_id,
                range_lookup_key_range(lower, upper),
                projected_columns,
                limit,
            )?),
            _ => None,
        };
        Ok(stream)
    }

    /// Execute a bitmap scan: collect TupleIds from each child path, merge
    /// them (union for OR, intersection for AND), sort by heap position,
    /// then fetch rows from the heap in physical page order.
    fn execute_bitmap_scan(
        &self,
        context: &ExecutionContext,
        table_id: RelationId,
        child_paths: &[ScanAccessPath],
        mut projected_columns: Option<Vec<ColumnId>>,
        is_and: bool,
    ) -> DbResult<Box<dyn TupleStream>> {
        use std::collections::{BTreeMap, HashSet};

        // FxHash for the bitmap-set membership: TupleIds are internal,
        // monotonically allocated integers — no DoS surface, so we
        // can drop SipHash for the much faster mix that PG uses for
        // its TID sets in `tidbitmap.c`.
        type FxTidSet = HashSet<aiondb_core::TupleId, join_plans::JoinFxBuildHasher>;

        // Collect (tuple_id, heap_position, row) from each child.
        let mut all_sets: Vec<FxTidSet> = Vec::new();
        let mut row_map: BTreeMap<u64, aiondb_storage_api::TupleRecord> = BTreeMap::new();

        let last_child_idx = child_paths.len().saturating_sub(1);
        for (child_idx, child_path) in child_paths.iter().enumerate() {
            let cols = if child_idx < last_child_idx {
                projected_columns.clone()
            } else {
                projected_columns.take()
            };
            let mut stream = self.resolve_scan_stream(context, table_id, child_path, cols)?;
            let mut tid_set: FxTidSet =
                HashSet::with_hasher(join_plans::JoinFxBuildHasher::default());
            while let Some(record) = stream.next()? {
                tid_set.insert(record.tuple_id);
                // Use heap_position as key to sort by physical page order.
                row_map.entry(record.heap_position).or_insert(record);
            }
            all_sets.push(tid_set);
        }

        // Merge: union (OR) or intersection (AND).
        let merged: FxTidSet = if all_sets.is_empty() {
            HashSet::with_hasher(join_plans::JoinFxBuildHasher::default())
        } else if is_and {
            let mut iter = all_sets.into_iter();
            let mut result = iter.next().unwrap_or_default();
            for set in iter {
                result.retain(|tid| set.contains(tid));
            }
            result
        } else {
            let mut result: FxTidSet =
                HashSet::with_hasher(join_plans::JoinFxBuildHasher::default());
            for set in all_sets {
                result.extend(set);
            }
            result
        };

        // Emit rows sorted by heap_position (physical page order).
        let records: Vec<aiondb_storage_api::TupleRecord> = row_map
            .into_values()
            .filter(|r| merged.contains(&r.tuple_id))
            .collect();

        Ok(Box::new(aiondb_storage_api::VecTupleStream::new(records)))
    }

    fn vector_search_locked(
        &self,
        context: &ExecutionContext,
        table_id: RelationId,
        index_id: IndexId,
        query: &[f32],
        k: usize,
        ef: usize,
        tuple_id_filter: Option<&std::collections::HashSet<aiondb_core::TupleId>>,
        max_search_duration: Option<std::time::Duration>,
    ) -> DbResult<Box<dyn TupleStream>> {
        let mode = if context.isolation == aiondb_tx::IsolationLevel::Serializable {
            LockMode::PredicateRead
        } else {
            LockMode::AccessShare
        };
        self.lock_table(context, table_id, mode)?;
        context.record_relation_read(table_id)?;
        let interrupt_checker = || context.check_deadline();
        if let Some(tuple_id_filter) = tuple_id_filter {
            let tuple_filter = |tuple_id| tuple_id_filter.contains(&tuple_id);
            self.storage_dml.vector_search(
                context.txn_id,
                &context.snapshot,
                index_id,
                query,
                k,
                ef,
                Some(&tuple_filter),
                max_search_duration,
                Some(&interrupt_checker),
            )
        } else {
            self.storage_dml.vector_search(
                context.txn_id,
                &context.snapshot,
                index_id,
                query,
                k,
                ef,
                None,
                max_search_duration,
                Some(&interrupt_checker),
            )
        }
    }

    fn insert_locked(
        &self,
        context: &ExecutionContext,
        table_id: RelationId,
        row: Row,
    ) -> DbResult<aiondb_core::TupleId> {
        context.record_relation_write(table_id)?;
        self.lock_table(context, table_id, LockMode::RowExclusive)?;
        let tuple_id = self.storage_dml.insert(context.txn_id, table_id, row)?;
        context.record_tuple_write(table_id, tuple_id)?;
        Ok(tuple_id)
    }

    fn update_locked(
        &self,
        context: &ExecutionContext,
        table_id: RelationId,
        tuple_id: aiondb_core::TupleId,
        expected_row: Option<&Row>,
        row: Row,
    ) -> DbResult<aiondb_core::TupleId> {
        context.record_relation_write(table_id)?;
        self.lock_table(context, table_id, LockMode::RowExclusive)?;
        self.update_after_table_locked(context, table_id, tuple_id, expected_row, row)
    }

    /// Per-row UPDATE step assuming the caller has already acquired
    /// `RowExclusive` on `table_id` and registered the relation-level
    /// write with the serializable coordinator. Used inside the bulk
    /// UPDATE row loop to avoid the per-row mutex round-trip on
    /// `lock_table` and the serializable-coordinator HashMap insert
    /// PostgreSQL similarly takes the table-level lock once at executor
    /// startup and only does per-tuple work in `ExecUpdateAct`.
    pub(super) fn update_after_table_locked(
        &self,
        context: &ExecutionContext,
        table_id: RelationId,
        tuple_id: aiondb_core::TupleId,
        expected_row: Option<&Row>,
        row: Row,
    ) -> DbResult<aiondb_core::TupleId> {
        let was_prelocked =
            context.acquire_tuple_lock_returning_was_held(table_id, tuple_id, LockMode::Update)?;
        if !was_prelocked {
            self.ensure_tuple_matches_expected(context, table_id, tuple_id, expected_row)?;
        }
        let updated_tuple_id = self
            .storage_dml
            .update(context.txn_id, table_id, tuple_id, row)?;
        context.record_tuple_write(table_id, tuple_id)?;
        if updated_tuple_id != tuple_id {
            context.record_tuple_write(table_id, updated_tuple_id)?;
        }
        Ok(updated_tuple_id)
    }

    pub(super) fn update_after_table_locked_with_storage_txn(
        &self,
        context: &ExecutionContext,
        storage_txn_id: TxnId,
        table_id: RelationId,
        tuple_id: aiondb_core::TupleId,
        expected_row: Option<&Row>,
        row: Row,
    ) -> DbResult<aiondb_core::TupleId> {
        let was_prelocked =
            context.acquire_tuple_lock_returning_was_held(table_id, tuple_id, LockMode::Update)?;
        if !was_prelocked {
            self.ensure_tuple_matches_expected(context, table_id, tuple_id, expected_row)?;
        }
        let updated_tuple_id = self
            .storage_dml
            .update(storage_txn_id, table_id, tuple_id, row)?;
        context.record_tuple_write(table_id, tuple_id)?;
        if updated_tuple_id != tuple_id {
            context.record_tuple_write(table_id, updated_tuple_id)?;
        }
        Ok(updated_tuple_id)
    }

    /// EvalPlanQual-aware variant of `update_after_table_locked`.
    ///
    /// PostgreSQL's `ExecUpdate` deals with concurrent updates at
    /// READ COMMITTED by re-fetching the latest committed version of
    /// the conflicting tuple, re-evaluating the WHERE predicate
    /// against it, and -- if the predicate still holds -- recomputing
    /// the projection (target list) against the latest values before
    /// retrying the storage update. This mirror of that behaviour is
    /// thin enough to fit in the AionDB executor: when
    /// `ensure_tuple_matches_expected` would have raised
    /// `SerializationFailure`, we instead invoke `epq_recompute` with
    /// the latest visible tuple and either skip the row (predicate no
    /// longer matches) or apply the recomputed new row.
    ///
    /// Higher isolation levels (Snapshot Isolation / Serializable)
    /// keep the original strict-serialization semantics; only
    /// `IsolationLevel::ReadCommitted` opts into the retry loop. The
    /// caller is the apply-update hot path; the closure captures the
    /// statement-level state needed to recompute the projection
    /// without firing BEFORE triggers a second time (matching
    /// PostgreSQL, which only re-evaluates the target list on the
    /// EPQ retry).
    #[allow(dead_code)]
    pub(super) fn update_after_table_locked_with_epq<F>(
        &self,
        context: &ExecutionContext,
        table_id: RelationId,
        tuple_id: aiondb_core::TupleId,
        expected_row: &Row,
        initial_row: Row,
        mut epq_recompute: F,
    ) -> DbResult<EpqUpdateResult>
    where
        F: FnMut(&Row) -> DbResult<EpqOutcome>,
    {
        let was_prelocked =
            context.acquire_tuple_lock_returning_was_held(table_id, tuple_id, LockMode::Update)?;
        if was_prelocked {
            // We already held the tuple lock, so nothing else could have
            // modified the tuple; the snapshot row matches the latest
            // committed version by construction.
            let updated =
                self.storage_dml
                    .update(context.txn_id, table_id, tuple_id, initial_row)?;
            context.record_tuple_write(table_id, tuple_id)?;
            if updated != tuple_id {
                context.record_tuple_write(table_id, updated)?;
            }
            return Ok(EpqUpdateResult::Applied(updated));
        }

        let latest_snapshot = aiondb_tx::Snapshot::new(
            aiondb_core::TxnId::default(),
            aiondb_core::TxnId::default(),
            Vec::new(),
        );
        let current =
            self.storage_dml
                .fetch(context.txn_id, &latest_snapshot, table_id, tuple_id, None)?;

        if current.as_ref() == Some(expected_row) {
            let updated =
                self.storage_dml
                    .update(context.txn_id, table_id, tuple_id, initial_row)?;
            context.record_tuple_write(table_id, tuple_id)?;
            if updated != tuple_id {
                context.record_tuple_write(table_id, updated)?;
            }
            return Ok(EpqUpdateResult::Applied(updated));
        }

        if context.isolation != aiondb_tx::IsolationLevel::ReadCommitted {
            return Err(DbError::transaction_error(
                SqlState::SerializationFailure,
                format!(
                    "tuple {tuple_id:?} in table {table_id:?} changed concurrently before write"
                ),
            ));
        }

        // EvalPlanQual retry: ask the caller to re-validate the
        // predicate and rebuild the new tuple against the latest
        // committed row.
        let Some(latest) = current else {
            // Tuple was concurrently deleted. PostgreSQL skips the
            // row in this case at READ COMMITTED.
            return Ok(EpqUpdateResult::Skipped);
        };
        match epq_recompute(&latest)? {
            EpqOutcome::Skip => Ok(EpqUpdateResult::Skipped),
            EpqOutcome::Apply(new_row) => {
                let updated =
                    self.storage_dml
                        .update(context.txn_id, table_id, tuple_id, new_row)?;
                context.record_tuple_write(table_id, tuple_id)?;
                if updated != tuple_id {
                    context.record_tuple_write(table_id, updated)?;
                }
                Ok(EpqUpdateResult::Applied(updated))
            }
        }
    }

    fn delete_locked(
        &self,
        context: &ExecutionContext,
        table_id: RelationId,
        tuple_id: aiondb_core::TupleId,
        expected_row: Option<&Row>,
    ) -> DbResult<()> {
        context.record_relation_write(table_id)?;
        self.lock_table(context, table_id, LockMode::RowExclusive)?;
        let was_prelocked = context.holds_tuple_lock(table_id, tuple_id, LockMode::Update)?;
        context.acquire_tuple_lock(table_id, tuple_id, LockMode::Update)?;
        if !was_prelocked {
            self.ensure_tuple_matches_expected(context, table_id, tuple_id, expected_row)?;
        }
        self.storage_dml
            .delete(context.txn_id, table_id, tuple_id)?;
        context.record_tuple_write(table_id, tuple_id)
    }

    fn ensure_tuple_matches_expected(
        &self,
        context: &ExecutionContext,
        table_id: RelationId,
        tuple_id: aiondb_core::TupleId,
        expected_row: Option<&Row>,
    ) -> DbResult<()> {
        let Some(expected_row) = expected_row else {
            return Ok(());
        };
        let latest_snapshot = aiondb_tx::Snapshot::new(
            aiondb_core::TxnId::default(),
            aiondb_core::TxnId::default(),
            Vec::new(),
        );
        let current_row =
            self.storage_dml
                .fetch(context.txn_id, &latest_snapshot, table_id, tuple_id, None)?;

        if current_row.as_ref() == Some(expected_row) {
            return Ok(());
        }

        Err(DbError::transaction_error(
            SqlState::SerializationFailure,
            format!("tuple {tuple_id:?} in table {table_id:?} changed concurrently before write"),
        ))
    }

    fn evaluate_expr_with_row(
        &self,
        expr: &TypedExpr,
        row: &Row,
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        self.evaluate_expr_with_row_prechecked(expr, row, context, true)
    }

    fn evaluate_expr_with_row_prechecked(
        &self,
        expr: &TypedExpr,
        row: &Row,
        context: &ExecutionContext,
        requires_special_resolution: bool,
    ) -> DbResult<Value> {
        if !requires_special_resolution {
            return self.evaluator.evaluate_with_row(expr, row);
        }
        self.evaluator
            .evaluate_with_row_and_resolver(expr, row, &|expr| {
                self.resolve_special_expr(expr, Some(row), context)
            })
    }

    fn evaluate_optional_predicate_prechecked(
        &self,
        predicate: Option<&TypedExpr>,
        row: &Row,
        context: &ExecutionContext,
        requires_special_resolution: bool,
    ) -> DbResult<bool> {
        let Some(predicate) = predicate else {
            return Ok(true);
        };
        match self.evaluate_expr_with_row_prechecked(
            predicate,
            row,
            context,
            requires_special_resolution,
        )? {
            Value::Boolean(value) => Ok(value),
            Value::Null => Ok(false),
            _ => Err(DbError::internal(
                "WHERE expression did not evaluate to BOOLEAN",
            )),
        }
    }

    fn evaluate_order_keys_prechecked(
        &self,
        order_by: &[SortExpr],
        row: &Row,
        context: &ExecutionContext,
        requires_special_resolution: bool,
    ) -> DbResult<Vec<Value>> {
        let mut keys = Vec::with_capacity(order_by.len());
        for sort in order_by {
            keys.push(self.evaluate_expr_with_row_prechecked(
                &sort.expr,
                row,
                context,
                requires_special_resolution,
            )?);
        }
        Ok(keys)
    }

    fn projection_column_ordinals(outputs: &[aiondb_plan::ProjectionExpr]) -> Option<Vec<usize>> {
        let mut ordinals = Vec::with_capacity(outputs.len());
        for output in outputs {
            let TypedExprKind::ColumnRef { ordinal, .. } = &output.expr.kind else {
                return None;
            };
            ordinals.push(*ordinal);
        }
        Some(ordinals)
    }

    fn normalize_projection_ordinals_for_row<'a>(
        ordinals: &'a [usize],
        row_width: usize,
    ) -> Option<std::borrow::Cow<'a, [usize]>> {
        if ordinals.iter().all(|ordinal| *ordinal < row_width) {
            return Some(std::borrow::Cow::Borrowed(ordinals));
        }
        if row_width == 0 {
            return None;
        }
        if ordinals
            .iter()
            .all(|ordinal| *ordinal > 0 && *ordinal <= row_width)
        {
            return Some(std::borrow::Cow::Owned(
                ordinals.iter().map(|ordinal| ordinal - 1).collect(),
            ));
        }
        None
    }

    fn project_outputs_with_precomputed_ordinals(
        &self,
        outputs: &[aiondb_plan::ProjectionExpr],
        direct_column_ordinals: Option<&[usize]>,
        row: &Row,
        context: &ExecutionContext,
    ) -> DbResult<Row> {
        if let Some(ordinals) = direct_column_ordinals {
            let normalized =
                Self::normalize_projection_ordinals_for_row(ordinals, row.values.len());
            let Some(ordinals) = normalized.as_deref() else {
                return self.project_outputs(outputs, row, context);
            };
            match ordinals {
                [ordinal] => {
                    let value = row.values.get(*ordinal).cloned().ok_or_else(|| {
                        DbError::internal(format!(
                            "projection column ordinal {ordinal} out of range (row width {})",
                            row.values.len()
                        ))
                    })?;
                    return Ok(Row::new(vec![value]));
                }
                [first, second] => {
                    let first_value = row.values.get(*first).cloned().ok_or_else(|| {
                        DbError::internal(format!(
                            "projection column ordinal {first} out of range (row width {})",
                            row.values.len()
                        ))
                    })?;
                    let second_value = row.values.get(*second).cloned().ok_or_else(|| {
                        DbError::internal(format!(
                            "projection column ordinal {second} out of range (row width {})",
                            row.values.len()
                        ))
                    })?;
                    return Ok(Row::new(vec![first_value, second_value]));
                }
                _ => {
                    let mut projected = Vec::with_capacity(ordinals.len());
                    for ordinal in ordinals {
                        let value = row.values.get(*ordinal).cloned().ok_or_else(|| {
                            DbError::internal(format!(
                                "projection column ordinal {ordinal} out of range (row width {})",
                                row.values.len()
                            ))
                        })?;
                        projected.push(value);
                    }
                    return Ok(Row::new(projected));
                }
            }
        }
        self.project_outputs(outputs, row, context)
    }

    fn project_outputs(
        &self,
        outputs: &[aiondb_plan::ProjectionExpr],
        row: &Row,
        context: &ExecutionContext,
    ) -> DbResult<Row> {
        let outputs_all_column_refs = outputs
            .iter()
            .all(|output| matches!(output.expr.kind, TypedExprKind::ColumnRef { .. }));
        if outputs_all_column_refs {
            let mut ordinals = Vec::with_capacity(outputs.len());
            for output in outputs {
                let TypedExprKind::ColumnRef { ordinal, .. } = &output.expr.kind else {
                    break;
                };
                ordinals.push(*ordinal);
            }
            if ordinals.len() == outputs.len() {
                if let Some(normalized) =
                    Self::normalize_projection_ordinals_for_row(&ordinals, row.values.len())
                {
                    let mut projected = Vec::with_capacity(outputs.len());
                    for ordinal in normalized.as_ref() {
                        let value = row.values.get(*ordinal).cloned().ok_or_else(|| {
                            DbError::internal(format!(
                                "projection column ordinal {ordinal} out of range (row width {})",
                                row.values.len()
                            ))
                        })?;
                        projected.push(value);
                    }
                    return Ok(Row::new(projected));
                }
            }
        }

        let mut projected = Vec::with_capacity(outputs.len());
        for output in outputs {
            projected.push(self.evaluate_expr_with_row(&output.expr, row, context)?);
        }
        Ok(Row::new(projected))
    }

    fn project_outputs_expanding_srfs(
        &self,
        outputs: &[aiondb_plan::ProjectionExpr],
        direct_column_ordinals: Option<&[usize]>,
        row: &Row,
        context: &ExecutionContext,
    ) -> DbResult<Vec<Row>> {
        let projected = self.project_outputs_with_precomputed_ordinals(
            outputs,
            direct_column_ordinals,
            row,
            context,
        )?;
        let srf_indices: Vec<usize> = projected
            .values
            .iter()
            .enumerate()
            .filter_map(|(index, value)| {
                let expands_scalar_declared_output =
                    !matches!(outputs[index].field.data_type, DataType::Array(_));
                if matches!(value, Value::Array(_))
                    && (self::projection_plans::is_srf_output(&outputs[index].expr)
                        || expands_scalar_declared_output)
                {
                    Some(index)
                } else {
                    None
                }
            })
            .collect();
        if srf_indices.is_empty() {
            return Ok(vec![projected]);
        }
        Ok(self::projection_plans::expand_srf_rows(
            &projected.values,
            &srf_indices,
        ))
    }

    /// Evaluate an expression in the context of a finalized aggregate row.
    ///
    /// Evaluate a HAVING / ORDER BY expression against a finalized aggregate row.
    ///
    /// `agg_row` contains the finalized aggregate values aligned with `aggregates`.
    /// For HAVING / ORDER BY expressions that reference aggregate functions or
    /// group-by columns, we need to resolve them by matching against the output
    /// projection expressions and reading the corresponding value from `agg_row`.
    fn evaluate_having_expr_extended(
        &self,
        expr: &TypedExpr,
        agg_row: &Row,
        aggregates: &[AggregateExprRef<'_>],
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        // Try to find the expression in the output projections first.
        // This handles both aggregate expressions and column references
        // that appear in the SELECT list.
        for (i, proj) in aggregates.iter().enumerate() {
            if exprs_structurally_equal(expr, proj.expr) {
                return agg_row
                    .values
                    .get(i)
                    .cloned()
                    .ok_or_else(|| DbError::internal("aggregate output index out of bounds"));
            }
        }

        // If the expression is a column ref, look it up by matching its name
        // against the output field names.
        if let TypedExprKind::ColumnRef { name, .. } = &expr.kind {
            for (i, proj) in aggregates.iter().enumerate() {
                if proj.field_name.eq_ignore_ascii_case(name) {
                    return agg_row
                        .values
                        .get(i)
                        .cloned()
                        .ok_or_else(|| DbError::internal("aggregate output index out of bounds"));
                }
            }
        }

        // Fall back to evaluating the expression with the aggregate row,
        // using a resolver that intercepts aggregate sub-expressions.
        self.evaluator
            .evaluate_with_row_and_resolver(expr, agg_row, &|sub_expr| {
                // Check if the sub-expression matches any projection output
                for (i, proj) in aggregates.iter().enumerate() {
                    if exprs_structurally_equal(sub_expr, proj.expr) {
                        return Some(agg_row.values.get(i).cloned().ok_or_else(|| {
                            DbError::internal("aggregate output index out of bounds")
                        }));
                    }
                }
                // Let the normal resolver handle NEXTVAL, etc.
                self.resolve_special_expr(sub_expr, Some(agg_row), context)
            })
    }
}
