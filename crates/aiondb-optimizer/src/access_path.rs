use std::{cell::RefCell, cmp::Ordering, collections::HashMap, ops::Bound};

use aiondb_catalog::{
    AccessPathMetadata, IndexDescriptor, IndexKind, TableDescriptor, TableStatistics,
};
use aiondb_core::{ColumnId, DbResult, IndexId, RelationId, TxnId, Value};
use aiondb_plan::{
    PhysicalPlan, ProjectionExpr, ResultField, ScalarFunction, ScanAccessPath, SortExpr, TypedExpr,
    TypedExprKind,
};

use crate::{
    cost::PlanCost, i64_to_f64, u64_to_f64, usize_to_f64, usize_to_i32_saturating, Optimizer,
};

/// Thread-local cache for catalog metadata used during access-path
/// selection.  Avoids repeated catalog lookups when the same table is
/// referenced in multiple correlated subquery invocations within a
/// single statement.
#[derive(Clone)]
struct CachedTableMeta {
    table: TableDescriptor,
    indexes: Vec<IndexDescriptor>,
    stats: Option<TableStatistics>,
}

thread_local! {
    static ACCESS_PATH_META_CACHE: RefCell<HashMap<(TxnId, RelationId), Option<CachedTableMeta>>> =
        RefCell::new(HashMap::new());
}

pub(crate) fn clear_access_path_meta_cache() {
    ACCESS_PATH_META_CACHE.with(|cache| cache.borrow_mut().clear());
}

const DEFAULT_EQUALITY_SELECTIVITY: f64 = 0.01;
const DEFAULT_RANGE_SELECTIVITY: f64 = 0.33;
const BOUNDED_RANGE_SELECTIVITY: f64 = 0.10;
const HIGH_NDISTINCT_MIN_RANGE_SELECTIVITY: f64 = 0.01;
const HIGH_NDISTINCT_RANGE_BUCKET_WIDTH: f64 = 100.0;
const MIN_SELECTIVITY: f64 = 1.0e-6;
const MAX_COSTED_BITMAP_OR_SELECTIVITY: f64 = 0.50;

impl Optimizer {
    /// Like `choose_access_path` but also considers index-only scans when
    /// `projected_column_ids` is provided and all requested columns are
    /// available from a single index.
    pub(crate) fn choose_access_path_with_projection(
        &self,
        txn_id: TxnId,
        table_id: RelationId,
        filter: Option<&TypedExpr>,
        projected_column_ids: Option<&[ColumnId]>,
    ) -> DbResult<ScanAccessPath> {
        let Some(filter) = filter else {
            return Ok(ScanAccessPath::SeqScan);
        };

        // Use the thread-local cache to avoid repeated catalog lookups
        // for the same table within a single statement (common with
        // correlated subqueries).
        let cached =
            ACCESS_PATH_META_CACHE.with(|cache| cache.borrow().get(&(txn_id, table_id)).cloned());
        let meta = match cached {
            Some(Some(meta)) => meta,
            Some(None) => return Ok(ScanAccessPath::SeqScan),
            None => {
                // Single bundled call so backends with a per-state lock
                // (e.g. `aiondb-catalog-store`) acquire the catalog read
                // lock once instead of three times per query.
                let bundle = self
                    .catalog_reader
                    .get_access_path_metadata(txn_id, table_id)?;
                let entry = bundle.map(
                    |AccessPathMetadata {
                         table,
                         indexes,
                         stats,
                     }| CachedTableMeta {
                        table,
                        indexes,
                        stats,
                    },
                );
                ACCESS_PATH_META_CACHE.with(|cache| {
                    cache.borrow_mut().insert((txn_id, table_id), entry.clone());
                });
                match entry {
                    Some(m) => m,
                    None => return Ok(ScanAccessPath::SeqScan),
                }
            }
        };
        let table = &meta.table;
        let indexes = &meta.indexes;

        if let Some(gin_path) = self.try_gin_containment(indexes, table, filter) {
            return Ok(gin_path);
        }

        if let Some(bitmap_path) = self.try_bitmap_or(txn_id, table_id, filter, &meta)? {
            return Ok(bitmap_path);
        }

        // Bitmap OR for `WHERE col IN (lit1, lit2, ...)`. Without this,
        // `IN`-list predicates fall through to a SeqScan even when `col`
        // is indexed - common with ORMs batching lookups.
        if let Some(in_list_path) = self.try_in_list_bitmap_or(filter, &meta)? {
            return Ok(in_list_path);
        }
        if let Some(composite_in_list_path) =
            self.try_composite_prefix_in_list_bitmap_or(filter, &meta)?
        {
            return Ok(composite_in_list_path);
        }

        let mut extracted_by_column: HashMap<ColumnId, Option<ColumnAccessConstraint>> =
            HashMap::new();
        let mut best: Option<(ScanAccessPath, PlanCost)> = None;
        let mut forced_unique_lookup_best: Option<(ScanAccessPath, PlanCost)> = None;
        let prefer_index_for_count_only = projected_column_ids.is_some_and(<[ColumnId]>::is_empty);
        // Collect all usable single-index paths for BitmapAnd consideration.
        let mut and_candidates: Vec<(ScanAccessPath, PlanCost)> = Vec::new();
        for index in indexes {
            if index.kind != IndexKind::BTree || index.key_columns.is_empty() {
                continue;
            }

            // Extract constraints for all contiguous leading key columns.
            // Stop at the first column that has no WHERE constraint.
            let mut eq_values: Vec<Value> = Vec::new();
            let mut trailing_range: Option<ColumnAccessConstraint> = None;

            for key_col in &index.key_columns {
                let extracted = extracted_by_column
                    .entry(key_col.column_id)
                    .or_insert_with(|| {
                        extract_column_access_constraint(filter, &table, key_col.column_id)
                    })
                    .clone();
                match extracted {
                    Some(ColumnAccessConstraint::Eq(val)) => {
                        eq_values.push(val);
                    }
                    Some(range @ ColumnAccessConstraint::Range(_)) => {
                        // Range on the column immediately after the equality prefix.
                        // We only use this if it is the first non-eq column.
                        trailing_range = Some(range);
                        break;
                    }
                    None => break,
                }
            }

            let eq_prefix_len = eq_values.len();
            let has_trailing_range = trailing_range.is_some();
            let access_path = if eq_prefix_len > 0 {
                if let Some(ColumnAccessConstraint::Range(range)) = trailing_range {
                    ScanAccessPath::IndexEqRangeComposite {
                        index_id: index.index_id,
                        eq_values,
                        lower: range.lower,
                        upper: range.upper,
                    }
                } else if index.key_columns.len() > 1 {
                    // Any equality-constrained prefix on a multi-column index is a
                    // composite-prefix lookup, even if the prefix currently covers
                    // only the first key column.
                    ScanAccessPath::IndexEqComposite {
                        index_id: index.index_id,
                        values: eq_values,
                    }
                } else {
                    ScanAccessPath::IndexEq {
                        index_id: index.index_id,
                        value: eq_values.into_iter().next().expect("eq_prefix_len > 0"),
                    }
                }
            } else if let Some(ColumnAccessConstraint::Range(range)) = trailing_range {
                ScanAccessPath::IndexRange {
                    index_id: index.index_id,
                    lower: range.lower,
                    upper: range.upper,
                }
            } else {
                continue;
            };

            let stats = &meta.stats;

            // Per-index correlation can differ between candidates, so the
            // previous shape-only cost cache (`index_eq_cost`,
            // `index_range_cost`) is no longer correct: an `IndexEq` on a
            // clustered PK and an `IndexEq` on a randomly-ordered text
            // column share the same `match` arm but should not share the
            // same cost. Cost the path against this exact index every
            // time; the calculation is a handful of floats.
            let cost = self.estimate_access_cost_with_stats_and_indexes(
                &access_path,
                stats.as_ref(),
                indexes,
                &table,
                filter,
            );

            and_candidates.push((access_path.clone(), cost));

            let is_exact_unique_lookup = index.kind == IndexKind::BTree
                && index.unique
                && !has_trailing_range
                && eq_prefix_len == index.key_columns.len()
                && index_keys_appear_high_distinct(index, meta.stats.as_ref());
            if is_exact_unique_lookup
                && forced_unique_lookup_best
                    .as_ref()
                    .map_or(true, |(_, best_cost)| cost.cheaper_than(*best_cost))
            {
                forced_unique_lookup_best = Some((access_path.clone(), cost));
            }

            if (prefer_index_for_count_only
                && matches!(access_path, ScanAccessPath::IndexEqRangeComposite { .. }))
                || best
                    .as_ref()
                    .map_or(true, |(_, best_cost)| cost.cheaper_than(*best_cost))
            {
                best = Some((access_path, cost));
            }
        }

        // --- Try BitmapAnd when multiple indexes each cover different AND predicates ---
        if and_candidates.len() >= 2 {
            if let Some(bitmap_and) = self.try_bitmap_and(&and_candidates, &meta, table, filter)? {
                let bitmap_cost = bitmap_and.1;
                if best
                    .as_ref()
                    .map_or(true, |(_, c)| bitmap_cost.cheaper_than(*c))
                {
                    best = Some(bitmap_and);
                }
            }
        }

        let mut forced_unique_lookup = false;
        if let Some((path, cost)) = forced_unique_lookup_best {
            best = Some((path, cost));
            forced_unique_lookup = true;
        }

        let Some((mut best_path, best_cost)) = best else {
            return Ok(ScanAccessPath::SeqScan);
        };

        let seq_cost = self.estimate_seq_scan_cost(txn_id, table_id)?;
        if !forced_unique_lookup
            && !prefer_index_for_count_only
            && !best_cost.cheaper_than(seq_cost)
        {
            return Ok(ScanAccessPath::SeqScan);
        }

        // --- Try Index-Only Scan upgrade ---
        if let Some(proj_cols) = projected_column_ids {
            if let Some(ios_path) =
                self.try_index_only_scan(&best_path, indexes, proj_cols, &meta)?
            {
                best_path = ios_path;
            }
        }

        Ok(best_path)
    }

    /// Try to build a BitmapOr plan for a `WHERE col IN (lit1, ..., litN)`
    /// predicate where `col` is indexed.  Each list element becomes an
    /// `IndexEq` lookup; the BitmapOr unions their tuple-id sets and
    /// fetches the heap pages in physical order.
    ///
    /// We only fire when N is small relative to the table, so the
    /// per-element index lookup cost stays well below a full SeqScan
    /// of (row_count) tuples. For very long IN-lists a SeqScan with
    /// in-memory hashset is faster - `bitmap_cost.cheaper_than` keeps
    /// us out of that regime.
    fn try_in_list_bitmap_or(
        &self,
        filter: &TypedExpr,
        meta: &CachedTableMeta,
    ) -> DbResult<Option<ScanAccessPath>> {
        let table = &meta.table;
        let stats = &meta.stats;
        let (row_count, total_bytes) = match stats {
            Some(s) => (s.row_count, s.total_bytes),
            None => (1000, 1000 * 64),
        };
        let seq_cost = PlanCost::seq_scan(row_count, total_bytes);
        let mut best: Option<(ScanAccessPath, PlanCost)> = None;
        for index in &meta.indexes {
            if index.kind != IndexKind::BTree {
                continue;
            }
            let Some(first_key) = index.key_columns.first() else {
                continue;
            };
            let column_id = first_key.column_id;
            let Some(values) = extract_small_in_list_values(filter, table, column_id)
                .or_else(|| extract_small_or_chain_values(filter, table, column_id))
            else {
                continue;
            };

            let mut child_paths = Vec::with_capacity(values.len());
            for value in values {
                let path = Self::build_leading_in_list_child_path(index, table, filter, value);
                if !child_paths.contains(&path) {
                    child_paths.push(path);
                }
            }
            if child_paths.is_empty() {
                continue;
            }
            if child_paths.len() == 1 {
                let path = child_paths.into_iter().next().expect("len checked");
                let path_cost = self.estimate_access_cost_with_stats_and_indexes(
                    &path,
                    stats.as_ref(),
                    &meta.indexes,
                    table,
                    filter,
                );
                if index_appears_selective_for_small_bitmap_or(index, stats.as_ref())
                    || path_cost.cheaper_than(seq_cost)
                {
                    if best
                        .as_ref()
                        .map_or(true, |(_, best_cost)| path_cost.cheaper_than(*best_cost))
                    {
                        best = Some((path, path_cost));
                    }
                }
                continue;
            }

            // For small literal IN-lists on unique/high-distinct columns we pick
            // BitmapOr over SeqScan: the cost model's RANDOM_PAGE_COST loading
            // overpenalises point lookups in a way that can make a 50-element ID
            // list look more expensive than a seq scan. When stats show a
            // low-cardinality leading key, fall through to the cost gate so a
            // predicate like `status IN (...)` can still choose SeqScan.
            const MAX_IN_LIST_BITMAP_OR_LEN: usize = 64;
            let should_force_small_bitmap_or = child_paths.len() <= MAX_IN_LIST_BITMAP_OR_LEN
                && index_appears_selective_for_small_bitmap_or(index, meta.stats.as_ref());
            let child_selectivities: Vec<f64> = child_paths
                .iter()
                .map(|path| {
                    estimate_access_path_selectivity(
                        path,
                        stats.as_ref(),
                        &meta.indexes,
                        table,
                        filter,
                    )
                })
                .collect();
            let combined_selectivity = child_selectivities
                .iter()
                .copied()
                .sum::<f64>()
                .clamp(MIN_SELECTIVITY, 1.0);
            let child_costs: Vec<PlanCost> = child_selectivities
                .iter()
                .map(|selectivity| PlanCost::bitmap_index_probe(row_count, *selectivity))
                .collect();
            let bitmap_cost =
                PlanCost::bitmap_or(&child_costs, row_count, total_bytes, combined_selectivity);
            if !should_force_small_bitmap_or {
                if combined_selectivity >= MAX_COSTED_BITMAP_OR_SELECTIVITY {
                    continue;
                }
                if !bitmap_cost.cheaper_than(seq_cost) {
                    continue;
                }
            }
            let path = ScanAccessPath::BitmapOr { paths: child_paths };
            if best
                .as_ref()
                .map_or(true, |(_, best_cost)| bitmap_cost.cheaper_than(*best_cost))
            {
                best = Some((path, bitmap_cost));
            }
        }
        Ok(best.map(|(path, _)| path))
    }

    /// Try to build a BitmapOr for a composite BTree prefix followed by a
    /// small IN-list, e.g. `(tenant_id, id)` with
    /// `tenant_id = 7 AND id IN (1, 2)`.
    fn try_composite_prefix_in_list_bitmap_or(
        &self,
        filter: &TypedExpr,
        meta: &CachedTableMeta,
    ) -> DbResult<Option<ScanAccessPath>> {
        const MAX_IN_LIST_BITMAP_OR_LEN: usize = 64;
        let table = &meta.table;
        let stats = &meta.stats;
        let (row_count, total_bytes) = match stats {
            Some(s) => (s.row_count, s.total_bytes),
            None => (1000, 1000 * 64),
        };
        let seq_cost = PlanCost::seq_scan(row_count, total_bytes);
        let mut best: Option<(ScanAccessPath, PlanCost)> = None;
        for index in &meta.indexes {
            if index.kind != aiondb_catalog::IndexKind::BTree || index.key_columns.len() < 2 {
                continue;
            }

            let mut eq_values = Vec::new();
            for key_col in &index.key_columns {
                if let Some(value) = extract_index_lookup_value(filter, table, key_col.column_id) {
                    eq_values.push(value);
                    continue;
                }

                if eq_values.is_empty() {
                    break;
                }
                let values = extract_small_in_list_values(filter, table, key_col.column_id)
                    .or_else(|| extract_small_or_chain_values(filter, table, key_col.column_id));
                let Some(in_values) = values else {
                    break;
                };
                let mut unique_in_values = Vec::with_capacity(in_values.len());
                for value in in_values {
                    if !unique_in_values.contains(&value) {
                        unique_in_values.push(value);
                    }
                }
                if unique_in_values.is_empty() || unique_in_values.len() > MAX_IN_LIST_BITMAP_OR_LEN
                {
                    break;
                }

                let mut paths = Vec::with_capacity(unique_in_values.len());
                for value in unique_in_values {
                    paths.push(Self::build_prefix_in_list_child_path(
                        index, table, filter, &eq_values, value,
                    ));
                }
                if paths.is_empty() {
                    break;
                }
                if paths.len() == 1 {
                    let path = paths.into_iter().next().expect("len checked");
                    if let Some(path) =
                        self.single_bitmap_child_if_useful(path, meta, table, filter)
                    {
                        let path_cost = self.estimate_access_cost_with_stats_and_indexes(
                            &path,
                            stats.as_ref(),
                            &meta.indexes,
                            table,
                            filter,
                        );
                        if best
                            .as_ref()
                            .map_or(true, |(_, best_cost)| path_cost.cheaper_than(*best_cost))
                        {
                            best = Some((path, path_cost));
                        }
                    }
                    break;
                }
                let child_selectivities: Vec<f64> = paths
                    .iter()
                    .map(|path| {
                        estimate_access_path_selectivity(
                            path,
                            stats.as_ref(),
                            &meta.indexes,
                            table,
                            filter,
                        )
                    })
                    .collect();
                let child_costs: Vec<PlanCost> = child_selectivities
                    .iter()
                    .map(|selectivity| PlanCost::bitmap_index_probe(row_count, *selectivity))
                    .collect();
                let combined_selectivity = child_selectivities
                    .iter()
                    .copied()
                    .sum::<f64>()
                    .clamp(MIN_SELECTIVITY, 1.0);
                if !index_appears_selective_for_small_bitmap_or(index, meta.stats.as_ref()) {
                    if combined_selectivity >= MAX_COSTED_BITMAP_OR_SELECTIVITY {
                        break;
                    }
                }
                let bitmap_cost =
                    PlanCost::bitmap_or(&child_costs, row_count, total_bytes, combined_selectivity);
                if !index_appears_selective_for_small_bitmap_or(index, meta.stats.as_ref()) {
                    if !bitmap_cost.cheaper_than(seq_cost) {
                        break;
                    }
                }
                let path = ScanAccessPath::BitmapOr { paths };
                if best
                    .as_ref()
                    .map_or(true, |(_, best_cost)| bitmap_cost.cheaper_than(*best_cost))
                {
                    best = Some((path, bitmap_cost));
                }
                break;
            }
        }
        Ok(best.map(|(path, _)| path))
    }

    fn build_prefix_in_list_child_path(
        index: &IndexDescriptor,
        table: &TableDescriptor,
        filter: &TypedExpr,
        eq_prefix_values: &[Value],
        in_value: Value,
    ) -> ScanAccessPath {
        let mut eq_values = eq_prefix_values.to_vec();
        eq_values.push(in_value);
        let mut trailing_range = None;
        for key_col in index.key_columns.iter().skip(eq_values.len()) {
            match extract_column_access_constraint(filter, table, key_col.column_id) {
                Some(ColumnAccessConstraint::Eq(value)) => eq_values.push(value),
                Some(range @ ColumnAccessConstraint::Range(_)) => {
                    trailing_range = Some(range);
                    break;
                }
                None => break,
            }
        }

        if let Some(ColumnAccessConstraint::Range(range)) = trailing_range {
            ScanAccessPath::IndexEqRangeComposite {
                index_id: index.index_id,
                eq_values,
                lower: range.lower,
                upper: range.upper,
            }
        } else {
            ScanAccessPath::IndexEqComposite {
                index_id: index.index_id,
                values: eq_values,
            }
        }
    }

    fn build_leading_in_list_child_path(
        index: &IndexDescriptor,
        table: &TableDescriptor,
        filter: &TypedExpr,
        leading_value: Value,
    ) -> ScanAccessPath {
        if index.key_columns.len() == 1 {
            return ScanAccessPath::IndexEq {
                index_id: index.index_id,
                value: leading_value,
            };
        }

        let mut eq_values = vec![leading_value];
        let mut trailing_range = None;
        for key_col in index.key_columns.iter().skip(1) {
            match extract_column_access_constraint(filter, table, key_col.column_id) {
                Some(ColumnAccessConstraint::Eq(value)) => eq_values.push(value),
                Some(range @ ColumnAccessConstraint::Range(_)) => {
                    trailing_range = Some(range);
                    break;
                }
                None => break,
            }
        }

        if let Some(ColumnAccessConstraint::Range(range)) = trailing_range {
            ScanAccessPath::IndexEqRangeComposite {
                index_id: index.index_id,
                eq_values,
                lower: range.lower,
                upper: range.upper,
            }
        } else {
            ScanAccessPath::IndexEqComposite {
                index_id: index.index_id,
                values: eq_values,
            }
        }
    }

    /// Try to build a BitmapOr plan for a top-level OR predicate where each
    /// disjunct can use a different index.
    fn try_bitmap_or(
        &self,
        _txn_id: TxnId,
        _table_id: RelationId,
        filter: &TypedExpr,
        meta: &CachedTableMeta,
    ) -> DbResult<Option<ScanAccessPath>> {
        let disjuncts = collect_or_disjuncts(filter);
        if disjuncts.len() < 2 {
            return Ok(None);
        }

        let table = &meta.table;
        let indexes = &meta.indexes;
        let stats = &meta.stats;
        let (row_count, total_bytes) = match stats {
            Some(s) => (s.row_count, s.total_bytes),
            None => (1000, 1000 * 64),
        };

        let mut child_paths = Vec::new();
        let mut child_costs = Vec::new();
        let mut combined_selectivity = 0.0;

        for disjunct in &disjuncts {
            // Pick the cheapest usable index path for this disjunct. Multiple
            // composite indexes can match the same predicate, and the first one
            // in catalog order is not necessarily the most selective.
            let mut best_disjunct_path: Option<(ScanAccessPath, PlanCost, f64)> = None;
            for index in indexes {
                if index.kind != IndexKind::BTree {
                    continue;
                }
                if let Some(path) = extract_index_prefix_access_path(disjunct, table, index) {
                    let selectivity = estimate_access_path_selectivity(
                        &path,
                        stats.as_ref(),
                        indexes,
                        table,
                        disjunct,
                    );
                    let cost = PlanCost::bitmap_index_probe(row_count, selectivity);
                    if best_disjunct_path
                        .as_ref()
                        .map_or(true, |(_, best_cost, _)| cost.cheaper_than(*best_cost))
                    {
                        best_disjunct_path = Some((path, cost, selectivity));
                    }
                }
            }
            let Some((path, cost, selectivity)) = best_disjunct_path else {
                // If any disjunct can't use an index, bitmap OR is not viable.
                return Ok(None);
            };
            if !child_paths.contains(&path) {
                combined_selectivity += selectivity;
                child_paths.push(path);
                child_costs.push(cost);
            }
        }

        if child_paths.len() == 1 {
            let path = child_paths.into_iter().next().expect("len checked");
            return Ok(self.single_bitmap_child_if_useful(path, meta, table, filter));
        }

        // Same gate as `try_in_list_bitmap_or`: short OR-chains where every
        // disjunct is a point lookup on the same index are semantically close
        // to an IN-list and should bypass the cost gate. The cost model's
        // RANDOM_PAGE_COST overpenalises N point lookups vs a SeqScan, even
        // though in practice the bitmap path is much faster.
        const MAX_OR_CHAIN_BITMAP_OR_LEN: usize = 64;
        if child_paths.len() <= MAX_OR_CHAIN_BITMAP_OR_LEN
            && child_paths.iter().all(|path| {
                matches!(
                    path,
                    ScanAccessPath::IndexEq { .. } | ScanAccessPath::IndexEqComposite { .. }
                )
            })
            && {
                let first_index = scan_access_path_index_id(&child_paths[0]);
                first_index.is_some()
                    && child_paths
                        .iter()
                        .all(|path| scan_access_path_index_id(path) == first_index)
                    && indexes
                        .iter()
                        .find(|index| Some(index.index_id) == first_index)
                        .is_some_and(|index| {
                            index_appears_selective_for_small_bitmap_or(index, stats.as_ref())
                        })
            }
        {
            return Ok(Some(ScanAccessPath::BitmapOr { paths: child_paths }));
        }

        combined_selectivity = combined_selectivity.clamp(MIN_SELECTIVITY, 1.0);
        if combined_selectivity >= MAX_COSTED_BITMAP_OR_SELECTIVITY {
            return Ok(None);
        }
        let bitmap_cost =
            PlanCost::bitmap_or(&child_costs, row_count, total_bytes, combined_selectivity);
        let seq_cost = PlanCost::seq_scan(row_count, total_bytes);

        if bitmap_cost.cheaper_than(seq_cost) {
            Ok(Some(ScanAccessPath::BitmapOr { paths: child_paths }))
        } else {
            Ok(None)
        }
    }

    fn single_bitmap_child_if_useful(
        &self,
        path: ScanAccessPath,
        meta: &CachedTableMeta,
        table: &TableDescriptor,
        filter: &TypedExpr,
    ) -> Option<ScanAccessPath> {
        let Some(index_id) = scan_access_path_index_id(&path) else {
            return None;
        };
        if meta
            .indexes
            .iter()
            .find(|index| index.index_id == index_id)
            .is_some_and(|index| {
                index_appears_selective_for_small_bitmap_or(index, meta.stats.as_ref())
            })
        {
            return Some(path);
        }
        let (row_count, total_bytes) = match &meta.stats {
            Some(stats) => (stats.row_count, stats.total_bytes),
            None => (1000, 1000 * 64),
        };
        let path_cost = self.estimate_access_cost_with_stats_and_indexes(
            &path,
            meta.stats.as_ref(),
            &meta.indexes,
            table,
            filter,
        );
        if path_cost.cheaper_than(PlanCost::seq_scan(row_count, total_bytes)) {
            Some(path)
        } else {
            None
        }
    }

    /// Try to build a BitmapAnd plan when multiple different indexes each
    /// cover different parts of an AND predicate.
    fn try_bitmap_and(
        &self,
        candidates: &[(ScanAccessPath, PlanCost)],
        meta: &CachedTableMeta,
        table: &TableDescriptor,
        filter: &TypedExpr,
    ) -> DbResult<Option<(ScanAccessPath, PlanCost)>> {
        if candidates.len() < 2 {
            return Ok(None);
        }
        let (row_count, total_bytes) = match &meta.stats {
            Some(s) => (s.row_count, s.total_bytes),
            None => (1000, 1000 * 64),
        };

        struct BitmapAndCandidate {
            path: ScanAccessPath,
            columns: Vec<ColumnId>,
            index_id: IndexId,
            selectivity: f64,
            probe_cost: PlanCost,
        }

        let bitmap_candidates: Vec<BitmapAndCandidate> = candidates
            .iter()
            .filter_map(|(path, _)| {
                let columns = scan_access_path_constrained_columns(path, &meta.indexes)?;
                let index_id = scan_access_path_index_id(path)?;
                let selectivity = estimate_access_path_selectivity(
                    path,
                    meta.stats.as_ref(),
                    &meta.indexes,
                    table,
                    filter,
                );
                Some(BitmapAndCandidate {
                    path: path.clone(),
                    columns,
                    index_id,
                    selectivity,
                    probe_cost: PlanCost::bitmap_index_probe(row_count, selectivity),
                })
            })
            .collect();
        if bitmap_candidates.len() < 2 {
            return Ok(None);
        }

        let seq_cost = PlanCost::seq_scan(row_count, total_bytes);

        let candidates_are_disjoint = |selected: &[usize], candidate_idx: usize| {
            let candidate = &bitmap_candidates[candidate_idx];
            selected.iter().all(|selected_idx| {
                let selected = &bitmap_candidates[*selected_idx];
                selected.index_id != candidate.index_id
                    && !selected
                        .columns
                        .iter()
                        .any(|column| candidate.columns.contains(column))
            })
        };
        let bitmap_cost_for = |selected: &[usize]| {
            let child_costs: Vec<PlanCost> = selected
                .iter()
                .map(|idx| bitmap_candidates[*idx].probe_cost)
                .collect();
            let combined_selectivity = selected
                .iter()
                .map(|idx| bitmap_candidates[*idx].selectivity)
                .product::<f64>()
                .clamp(MIN_SELECTIVITY, 1.0);
            PlanCost::bitmap_and(&child_costs, row_count, total_bytes, combined_selectivity)
        };

        let mut best_subset: Option<(Vec<usize>, PlanCost)> = None;
        for left_idx in 0..bitmap_candidates.len() {
            for right_idx in (left_idx + 1)..bitmap_candidates.len() {
                if !candidates_are_disjoint(&[left_idx], right_idx) {
                    continue;
                }
                let mut selected = vec![left_idx, right_idx];
                let mut selected_cost = bitmap_cost_for(&selected);

                loop {
                    let mut best_extension: Option<(usize, PlanCost)> = None;
                    for candidate_idx in 0..bitmap_candidates.len() {
                        if selected.contains(&candidate_idx)
                            || !candidates_are_disjoint(&selected, candidate_idx)
                        {
                            continue;
                        }
                        let mut extended = selected.clone();
                        extended.push(candidate_idx);
                        let extended_cost = bitmap_cost_for(&extended);
                        if extended_cost.cheaper_than(selected_cost)
                            && best_extension.as_ref().map_or(true, |(_, best_cost)| {
                                extended_cost.cheaper_than(*best_cost)
                            })
                        {
                            best_extension = Some((candidate_idx, extended_cost));
                        }
                    }
                    let Some((candidate_idx, extended_cost)) = best_extension else {
                        break;
                    };
                    selected.push(candidate_idx);
                    selected_cost = extended_cost;
                }

                if best_subset.as_ref().map_or(true, |(_, best_cost)| {
                    selected_cost.cheaper_than(*best_cost)
                }) {
                    best_subset = Some((selected, selected_cost));
                }
            }
        }

        let Some((selected, bitmap_cost)) = best_subset else {
            return Ok(None);
        };
        if !bitmap_cost.cheaper_than(seq_cost) {
            return Ok(None);
        }
        let paths = selected
            .into_iter()
            .map(|idx| bitmap_candidates[idx].path.clone())
            .collect();

        Ok(Some((ScanAccessPath::BitmapAnd { paths }, bitmap_cost)))
    }

    /// Try to upgrade an index scan to an index-only scan when all projected
    /// columns are present in the index (key columns + include columns).
    fn try_index_only_scan(
        &self,
        path: &ScanAccessPath,
        indexes: &[IndexDescriptor],
        projected_column_ids: &[ColumnId],
        _meta: &CachedTableMeta,
    ) -> DbResult<Option<ScanAccessPath>> {
        let index_id = scan_access_path_index_id(path);
        let Some(index_id) = index_id else {
            return Ok(None);
        };
        let Some(index) = indexes.iter().find(|i| i.index_id == index_id) else {
            return Ok(None);
        };
        let constrained_prefix_len = match path {
            ScanAccessPath::IndexEq { .. } | ScanAccessPath::IndexRange { .. } => 1,
            ScanAccessPath::IndexEqComposite { values, .. } => values.len(),
            ScanAccessPath::IndexEqRangeComposite { eq_values, .. } => eq_values.len() + 1,
            _ => return Ok(None),
        };

        // Collect all column IDs available from this index.
        let mut index_col_ids: Vec<ColumnId> =
            index.key_columns.iter().map(|kc| kc.column_id).collect();
        index_col_ids.extend_from_slice(&index.include_columns);

        // For key-column coverage, require contiguous key prefix usage.
        // This avoids index-only upgrades when referenced key columns skip
        // intermediate key positions (e.g. a,c without b on (a,b,c)).
        let projected_key_positions: Vec<usize> = projected_column_ids
            .iter()
            .filter_map(|col| {
                index
                    .key_columns
                    .iter()
                    .position(|key_col| key_col.column_id == *col)
            })
            .collect();
        if let Some(max_pos) = projected_key_positions.iter().copied().max() {
            let contiguous_prefix = (0..=max_pos).all(|pos| {
                index
                    .key_columns
                    .get(pos)
                    .is_some_and(|key_col| projected_column_ids.contains(&key_col.column_id))
            });
            if !contiguous_prefix {
                return Ok(None);
            }
        }

        // Check that every projected column is available from the index.
        let all_covered = projected_column_ids
            .iter()
            .all(|col| index_col_ids.contains(col));
        // Keep plain index paths when projected columns are all inside the
        // already constrained key prefix. We only upgrade to IndexOnlyScan
        // when projections need extra index payload columns.
        let needs_extra_index_payload = projected_column_ids.iter().any(|projected| {
            !index
                .key_columns
                .iter()
                .take(constrained_prefix_len)
                .any(|key_col| key_col.column_id == *projected)
        });

        if all_covered && !projected_column_ids.is_empty() && needs_extra_index_payload {
            Ok(Some(ScanAccessPath::IndexOnlyScan {
                inner: Box::new(path.clone()),
                index_column_ids: index_col_ids,
            }))
        } else {
            Ok(None)
        }
    }

    fn try_gin_containment(
        &self,
        indexes: &[IndexDescriptor],
        table: &TableDescriptor,
        filter: &TypedExpr,
    ) -> Option<ScanAccessPath> {
        let (column_id, pattern) = extract_json_contains(filter, table)
            .or_else(|| extract_text_search_match(filter, table))?;
        let gin_index = indexes.iter().find(|idx| {
            idx.kind == IndexKind::Gin
                && idx.key_columns.len() == 1
                && idx.key_columns[0].column_id == column_id
        })?;

        Some(ScanAccessPath::GinContainment {
            index_id: gin_index.index_id,
            pattern,
        })
    }

    fn estimate_access_cost_with_stats_and_indexes(
        &self,
        access_path: &ScanAccessPath,
        stats: Option<&TableStatistics>,
        indexes: &[IndexDescriptor],
        table: &TableDescriptor,
        filter: &TypedExpr,
    ) -> PlanCost {
        let (row_count, total_bytes) = match stats {
            Some(stats) => (stats.row_count, stats.total_bytes),
            None => (1000, 1000 * 64),
        };
        let leading_correlation =
            |index_id: IndexId| index_leading_column_correlation(indexes, stats, index_id);

        match access_path {
            ScanAccessPath::SeqScan => PlanCost::seq_scan(row_count, total_bytes),
            ScanAccessPath::IndexEq { index_id, value } => {
                let selectivity = estimate_index_equality_selectivity(
                    *index_id,
                    std::slice::from_ref(value),
                    stats,
                    indexes,
                    filter,
                    table,
                );
                let correlation = leading_correlation(*index_id);
                PlanCost::index_eq_with_correlation(
                    row_count,
                    total_bytes,
                    selectivity,
                    correlation,
                )
            }
            ScanAccessPath::IndexEqComposite { index_id, values } => {
                let selectivity = estimate_index_equality_selectivity(
                    *index_id, values, stats, indexes, filter, table,
                );
                let correlation = leading_correlation(*index_id);
                PlanCost::index_eq_with_correlation(
                    row_count,
                    total_bytes,
                    selectivity,
                    correlation,
                )
            }
            ScanAccessPath::IndexRange { index_id, .. } => {
                let selectivity = estimate_range_selectivity(filter, stats, table);
                let correlation = leading_correlation(*index_id);
                PlanCost::index_range_with_correlation(
                    row_count,
                    total_bytes,
                    selectivity,
                    correlation,
                )
            }
            ScanAccessPath::IndexEqRangeComposite {
                index_id,
                eq_values,
                ..
            } => {
                let selectivity = estimate_index_eq_range_selectivity(
                    *index_id, eq_values, stats, indexes, filter, table,
                );
                let correlation = leading_correlation(*index_id);
                PlanCost::index_range_with_correlation(
                    row_count,
                    total_bytes,
                    selectivity,
                    correlation,
                )
            }
            ScanAccessPath::GinContainment { .. } => PlanCost::index_eq_with_selectivity(
                row_count,
                total_bytes,
                DEFAULT_EQUALITY_SELECTIVITY,
            ),
            ScanAccessPath::BitmapOr { .. }
            | ScanAccessPath::BitmapAnd { .. }
            | ScanAccessPath::IndexOnlyScan { .. } => {
                // These are costed at construction time; fallback to seq scan cost.
                PlanCost::seq_scan(row_count, total_bytes)
            }
        }
    }

    fn estimate_seq_scan_cost(&self, txn_id: TxnId, table_id: RelationId) -> DbResult<PlanCost> {
        // Try the thread-local cache first to avoid a catalog round-trip.
        let cached_stats = ACCESS_PATH_META_CACHE.with(|cache| {
            cache
                .borrow()
                .get(&(txn_id, table_id))
                .and_then(|entry| entry.as_ref().map(|m| m.stats.clone()))
        });
        let stats = match cached_stats {
            Some(s) => s,
            None => self.catalog_reader.get_statistics(txn_id, table_id)?,
        };
        let (row_count, total_bytes) = match stats {
            Some(stats) => (stats.row_count, stats.total_bytes),
            None => (1000, 1000 * 64),
        };
        Ok(PlanCost::seq_scan(row_count, total_bytes))
    }

    /// Multi-aggregate MIN/MAX → ProjectOnce of scalar subqueries.
    ///
    /// Mirrors PostgreSQL `planagg.c::optimize_minmax_aggregates`: when
    /// every aggregate in the projection is a MIN or MAX over a B-tree
    /// indexed column (with no per-aggregate FILTER), each becomes a
    /// `ScalarSubquery(ORDER BY col [ASC|DESC] LIMIT 1)`. They share the
    /// same outer scope so each runs as its own O(log N) index probe.
    /// Returns `None` if any aggregate is ineligible.
    pub(crate) fn try_minmax_aggregate_index_scan(
        &self,
        txn_id: TxnId,
        table_id: RelationId,
        aggregates: &[ProjectionExpr],
        filter: Option<&TypedExpr>,
    ) -> DbResult<Option<PhysicalPlan>> {
        if aggregates.is_empty() {
            return Ok(None);
        }
        // Single-aggregate fast path: emit the original ProjectOnce shape
        // so any caller pattern-matching on it (tests, EXPLAIN) keeps the
        // same structure as before this extension.
        if aggregates.len() == 1 {
            return self.try_minmax_index_scan(txn_id, table_id, &aggregates[0], filter);
        }

        let mut output_exprs = Vec::with_capacity(aggregates.len());
        for aggregate in aggregates {
            match self.try_minmax_index_scan(txn_id, table_id, aggregate, filter)? {
                Some(PhysicalPlan::ProjectOnce { mut outputs, .. }) if outputs.len() == 1 => {
                    // Re-wrap with the original aggregate's field
                    // descriptor so the surrounding plan still emits the
                    // same column shape (name, type) the caller expects.
                    let Some(inner) = outputs.pop() else {
                        return Ok(None);
                    };
                    output_exprs.push(ProjectionExpr {
                        field: aggregate.field.clone(),
                        expr: inner.expr,
                    });
                }
                _ => return Ok(None),
            }
        }

        Ok(Some(PhysicalPlan::ProjectOnce {
            outputs: output_exprs,
            filter: None,
            order_by: Vec::new(),
            limit: None,
            offset: None,
            distinct: false,
            distinct_on: Vec::new(),
        }))
    }

    /// MIN/MAX to index scan optimization (PostgreSQL `planagg.c` equivalent).
    ///
    /// When the query is `SELECT MIN(col) FROM t` or `SELECT MAX(col) FROM t`
    /// with no GROUP BY, no HAVING, and `col` has a B-tree index on its first
    /// key column, we replace the full-table aggregate with a scalar subquery:
    ///
    ///   SELECT (SELECT col FROM t ORDER BY col ASC  LIMIT 1) AS min;
    ///   SELECT (SELECT col FROM t ORDER BY col DESC LIMIT 1) AS max;
    ///
    /// The scalar subquery correctly returns NULL on an empty table, matching
    /// the semantics of MIN/MAX over zero rows.  The index provides sorted
    /// output and LIMIT 1 stops after reading a single leaf entry, turning
    /// an O(N) full-scan aggregate into an O(log N) index lookup.
    pub(crate) fn try_minmax_index_scan(
        &self,
        txn_id: TxnId,
        table_id: RelationId,
        aggregate: &ProjectionExpr,
        filter: Option<&TypedExpr>,
    ) -> DbResult<Option<PhysicalPlan>> {
        // 1. Check that the aggregate expression is MIN(col) or MAX(col)
        //    on a simple column reference, with no per-aggregate filter.
        let (is_min, inner_expr) = match &aggregate.expr.kind {
            TypedExprKind::AggMin { expr, filter } if filter.is_none() => (true, expr.as_ref()),
            TypedExprKind::AggMax { expr, filter } if filter.is_none() => (false, expr.as_ref()),
            _ => return Ok(None),
        };

        // Only handle direct column references -- expressions like MIN(a + b)
        // cannot be served by a plain index scan.
        let TypedExprKind::ColumnRef { ordinal, .. } = &inner_expr.kind else {
            return Ok(None);
        };
        let col_ordinal = *ordinal;

        // 2. Resolve the column's ColumnId from the table descriptor.
        let table = match self.catalog_reader.get_table_by_id(txn_id, table_id)? {
            Some(t) => t,
            None => return Ok(None),
        };
        let target_column_id = match table.columns.get(col_ordinal) {
            Some(col) => col.column_id,
            None => return Ok(None),
        };

        // 3. Find a B-tree index whose first key column matches.
        let indexes = self.catalog_reader.list_indexes(txn_id, table_id)?;
        let has_btree = indexes.iter().any(|idx| {
            idx.kind == IndexKind::BTree
                && !idx.key_columns.is_empty()
                && idx.key_columns[0].column_id == target_column_id
        });
        if !has_btree {
            return Ok(None);
        }

        // 4. Build the replacement plan.
        //
        //    We emit:
        //      ProjectOnce {
        //          outputs: [ ScalarSubquery(
        //              LogicalPlan::ProjectTable {
        //                  ORDER BY col ASC/DESC NULLS LAST, LIMIT 1
        //              }
        //          ) ]
        //      }
        //
        //    The ScalarSubquery is re-optimised at execution time by the
        //    executor's `compile_logical_plan`, which will call back into
        //    the optimizer.  At that point the ProjectTable will pick up
        //    the appropriate access path via the normal access-path logic.
        //
        //    Using ScalarSubquery guarantees correct NULL-on-empty-table
        //    semantics without duplicating the executor's aggregate logic.

        let col_expr = inner_expr.clone();
        let descending = !is_min; // ASC for MIN, DESC for MAX

        let subquery_output = ProjectionExpr {
            field: ResultField {
                name: aggregate.field.name.clone(),
                data_type: inner_expr.data_type.clone(),
                text_type_modifier: None,
                nullable: true,
            },
            expr: col_expr,
        };

        let order_by_col = inner_expr.clone();

        let subquery_plan = aiondb_plan::LogicalPlan::ProjectTable {
            table_id,
            outputs: vec![subquery_output],
            filter: filter.cloned(),
            order_by: vec![SortExpr {
                expr: order_by_col,
                descending,
                // NULLS LAST for both ASC and DESC so the first row is
                // always a real (non-NULL) value when one exists.
                nulls_first: Some(false),
            }],
            limit: Some(TypedExpr::literal(
                Value::Int(1),
                aiondb_core::DataType::Int,
                false,
            )),
            offset: None,
            distinct: false,
            distinct_on: Vec::new(),
        };

        // Wrap in a ScalarSubquery expression inside ProjectOnce.
        let scalar_subquery_expr = TypedExpr {
            kind: TypedExprKind::ScalarSubquery {
                plan: Box::new(subquery_plan),
            },
            data_type: aggregate.expr.data_type.clone(),
            nullable: true,
        };

        let plan = PhysicalPlan::ProjectOnce {
            outputs: vec![ProjectionExpr {
                field: aggregate.field.clone(),
                expr: scalar_subquery_expr,
            }],
            filter: None,
            order_by: Vec::new(),
            limit: None,
            offset: None,
            distinct: false,
            distinct_on: Vec::new(),
        };

        Ok(Some(plan))
    }
}

/// Estimate selectivity for an equality predicate using the PostgreSQL
/// MCV + histogram merge formula.  When both MCV list and histogram are
/// available, the histogram covers only the non-MCV, non-NULL population:
///
///   selec = mcv_selec + (1 - null_frac - sum_mcv_freq) * hist_selec
///
fn estimate_equality_selectivity(
    filter: &TypedExpr,
    stats: Option<&TableStatistics>,
    table: &TableDescriptor,
) -> f64 {
    let Some(stats) = stats else {
        return DEFAULT_EQUALITY_SELECTIVITY;
    };
    let Some(column_id) = extract_eq_column_id(filter, table) else {
        return DEFAULT_EQUALITY_SELECTIVITY;
    };
    let Some(col_stats) = stats.column_stats.iter().find(|c| c.column_id == column_id) else {
        return DEFAULT_EQUALITY_SELECTIVITY;
    };

    let value = extract_index_lookup_value(filter, table, column_id);
    estimate_column_equality_selectivity(col_stats, value.as_ref())
}

fn estimate_index_equality_selectivity(
    index_id: IndexId,
    values: &[Value],
    stats: Option<&TableStatistics>,
    indexes: &[IndexDescriptor],
    filter: &TypedExpr,
    table: &TableDescriptor,
) -> f64 {
    let Some(index) = indexes.iter().find(|index| index.index_id == index_id) else {
        return (estimate_equality_selectivity(filter, stats, table)
            * DEFAULT_EQUALITY_SELECTIVITY
                .powi(usize_to_i32_saturating(values.len()).saturating_sub(1)))
        .clamp(MIN_SELECTIVITY, 1.0);
    };
    let selectivity = values
        .iter()
        .zip(&index.key_columns)
        .map(|(value, key_col)| {
            stats
                .and_then(|stats| {
                    stats
                        .column_stats
                        .iter()
                        .find(|col_stats| col_stats.column_id == key_col.column_id)
                })
                .map_or(DEFAULT_EQUALITY_SELECTIVITY, |col_stats| {
                    estimate_column_equality_selectivity(col_stats, Some(value))
                })
        })
        .product::<f64>();
    selectivity.clamp(MIN_SELECTIVITY, 1.0)
}

fn estimate_index_eq_range_selectivity(
    index_id: IndexId,
    eq_values: &[Value],
    stats: Option<&TableStatistics>,
    indexes: &[IndexDescriptor],
    filter: &TypedExpr,
    table: &TableDescriptor,
) -> f64 {
    let range_selectivity = estimate_range_selectivity(filter, stats, table);
    let Some(index) = indexes.iter().find(|index| index.index_id == index_id) else {
        return (estimate_equality_selectivity(filter, stats, table) * range_selectivity)
            .clamp(MIN_SELECTIVITY, 1.0);
    };

    let prefix_selectivity = eq_values
        .iter()
        .zip(&index.key_columns)
        .map(|(value, key_col)| {
            stats
                .and_then(|stats| {
                    stats
                        .column_stats
                        .iter()
                        .find(|col_stats| col_stats.column_id == key_col.column_id)
                })
                .map_or(DEFAULT_EQUALITY_SELECTIVITY, |col_stats| {
                    estimate_column_equality_selectivity(col_stats, Some(value))
                })
        })
        .product::<f64>();

    (prefix_selectivity * range_selectivity).clamp(MIN_SELECTIVITY, 1.0)
}

fn estimate_access_path_selectivity(
    access_path: &ScanAccessPath,
    stats: Option<&TableStatistics>,
    indexes: &[IndexDescriptor],
    table: &TableDescriptor,
    filter: &TypedExpr,
) -> f64 {
    match access_path {
        ScanAccessPath::IndexEq { index_id, value } => estimate_index_equality_selectivity(
            *index_id,
            std::slice::from_ref(value),
            stats,
            indexes,
            filter,
            table,
        ),
        ScanAccessPath::IndexEqComposite { index_id, values } => {
            estimate_index_equality_selectivity(*index_id, values, stats, indexes, filter, table)
        }
        ScanAccessPath::IndexRange { .. } => estimate_range_selectivity(filter, stats, table),
        ScanAccessPath::IndexEqRangeComposite {
            index_id,
            eq_values,
            ..
        } => {
            estimate_index_eq_range_selectivity(*index_id, eq_values, stats, indexes, filter, table)
        }
        ScanAccessPath::GinContainment { .. } => DEFAULT_EQUALITY_SELECTIVITY,
        ScanAccessPath::SeqScan => 1.0,
        ScanAccessPath::BitmapOr { paths } => paths
            .iter()
            .map(|path| estimate_access_path_selectivity(path, stats, indexes, table, filter))
            .sum::<f64>()
            .clamp(MIN_SELECTIVITY, 1.0),
        ScanAccessPath::BitmapAnd { paths } => paths
            .iter()
            .map(|path| estimate_access_path_selectivity(path, stats, indexes, table, filter))
            .product::<f64>()
            .clamp(MIN_SELECTIVITY, 1.0),
        ScanAccessPath::IndexOnlyScan { inner, .. } => {
            estimate_access_path_selectivity(inner, stats, indexes, table, filter)
        }
    }
}

fn index_appears_selective_for_small_bitmap_or(
    index: &IndexDescriptor,
    stats: Option<&TableStatistics>,
) -> bool {
    if index.unique {
        return true;
    }
    let Some(stats) = stats else {
        return true;
    };
    let Some(leading) = index.key_columns.first() else {
        return false;
    };
    let Some(col_stats) = stats
        .column_stats
        .iter()
        .find(|col_stats| col_stats.column_id == leading.column_id)
    else {
        return true;
    };
    let min_distinct = (u64_to_f64(stats.row_count) * 0.5).max(1.0);
    col_stats.ndistinct >= min_distinct
}

fn estimate_column_equality_selectivity(
    col_stats: &aiondb_catalog::ColumnStatistics,
    value: Option<&Value>,
) -> f64 {
    // --- MCV lookup (exact frequency if value is among most-common) ---
    let (mcv_sel, sum_common) = match (&col_stats.mcv, value) {
        (Some(mcv), Some(val)) => {
            if let Some(freq) = mcv.frequency_of(val) {
                // Exact MCV hit - return immediately.
                return freq.clamp(MIN_SELECTIVITY, 1.0);
            }
            (0.0, mcv.sum_frequencies())
        }
        (Some(mcv), None) => (0.0, mcv.sum_frequencies()),
        _ => (0.0, 0.0),
    };

    // Fraction of rows that are neither NULL nor MCV.
    let other_frac = (1.0 - col_stats.null_fraction - sum_common).max(0.0);

    // --- Histogram-based estimate on the non-MCV population ---
    if let (Some(histogram), Some(val)) = (&col_stats.histogram, value) {
        if let Some(hist_sel) = histogram.estimate_equality_selectivity(val) {
            let sel = mcv_sel + other_frac * hist_sel;
            return sel.clamp(MIN_SELECTIVITY, 1.0);
        }
    }

    // --- Fallback: uniform distribution over non-MCV distinct values ---
    let mcv_count = col_stats
        .mcv
        .as_ref()
        .map_or(0.0, |m| usize_to_f64(m.len()));
    let non_mcv_distinct = (col_stats.ndistinct - mcv_count).max(1.0);
    let sel = if other_frac > 0.0 {
        other_frac / non_mcv_distinct
    } else {
        col_stats.equality_selectivity()
    };
    apply_null_fraction(sel, col_stats.null_fraction)
}

fn extract_text_search_match(
    filter: &TypedExpr,
    table: &TableDescriptor,
) -> Option<(ColumnId, serde_json::Value)> {
    match &filter.kind {
        TypedExprKind::ScalarFunction { func, args }
            if matches!(func, ScalarFunction::Generic(name) if name.eq_ignore_ascii_case("ts_match"))
                && args.len() == 2 =>
        {
            let column_id = extract_tsvector_column_id(&args[0], table)?;
            let query = extract_tsquery_literal(&args[1])?;
            let words = extract_and_terms_from_tsquery(&query)?;
            if words.is_empty() {
                return None;
            }
            let mut object = serde_json::Map::with_capacity(words.len());
            for word in words {
                object.insert(word, serde_json::Value::Bool(true));
            }
            Some((column_id, serde_json::Value::Object(object)))
        }
        TypedExprKind::LogicalAnd { left, right } => extract_text_search_match(left, table)
            .or_else(|| extract_text_search_match(right, table)),
        _ => None,
    }
}

fn extract_tsvector_column_id(expr: &TypedExpr, table: &TableDescriptor) -> Option<ColumnId> {
    let TypedExprKind::ScalarFunction { func, args } = &expr.kind else {
        return None;
    };
    if !matches!(func, ScalarFunction::ToTsvector) {
        return None;
    }
    let column_expr = match args.as_slice() {
        [arg] => arg,
        [_, arg] => arg,
        _ => return None,
    };
    let TypedExprKind::ColumnRef { ordinal, .. } = column_expr.kind else {
        return None;
    };
    table.columns.get(ordinal).map(|column| column.column_id)
}

fn extract_tsquery_literal(expr: &TypedExpr) -> Option<String> {
    match &expr.kind {
        TypedExprKind::Literal(Value::Text(query)) => Some(query.clone()),
        TypedExprKind::ScalarFunction {
            func:
                ScalarFunction::ToTsquery
                | ScalarFunction::PlaintoTsquery
                | ScalarFunction::PhrasetoTsquery
                | ScalarFunction::WebsearchToTsquery,
            args,
        } => {
            let query_arg = match args.as_slice() {
                [arg] => arg,
                [_, arg] => arg,
                _ => return None,
            };
            let TypedExprKind::Literal(Value::Text(query)) = &query_arg.kind else {
                return None;
            };
            Some(query.clone())
        }
        _ => None,
    }
}

fn extract_and_terms_from_tsquery(query: &str) -> Option<Vec<String>> {
    if query.contains('|') || query.contains('!') {
        return None;
    }

    let mut words = Vec::new();
    let mut current = String::new();
    for ch in query.chars() {
        if ch.is_alphanumeric() || ch == '_' {
            current.push(ch.to_ascii_lowercase());
        } else if !current.is_empty() {
            push_tsquery_word(&mut words, &mut current);
        }
    }
    if !current.is_empty() {
        push_tsquery_word(&mut words, &mut current);
    }
    words.sort_unstable();
    words.dedup();
    Some(words)
}

fn push_tsquery_word(words: &mut Vec<String>, current: &mut String) {
    if matches!(current.as_str(), "and" | "or" | "not") {
        current.clear();
    } else {
        words.push(std::mem::take(current));
    }
}

/// Estimate selectivity for a range predicate using the PostgreSQL
/// MCV + histogram merge formula and paired-bounds correction.
///
/// For paired bounds (x > low AND x < high), PostgreSQL uses
/// `hisel + losel - 1.0 + nullfrac` instead of `hisel * losel`.
fn estimate_range_selectivity(
    filter: &TypedExpr,
    stats: Option<&TableStatistics>,
    table: &TableDescriptor,
) -> f64 {
    let Some(column_id) = extract_range_column_id(filter, table) else {
        return DEFAULT_RANGE_SELECTIVITY;
    };
    let range = extract_index_range(filter, table, column_id);

    if let Some(stats) = stats {
        if let Some(col_stats) = stats.column_stats.iter().find(|c| c.column_id == column_id) {
            // --- MCV contribution: sum frequencies of MCV values within range ---
            let (mcv_sel, sum_common) = match &col_stats.mcv {
                Some(mcv) => {
                    let mut in_range_freq = 0.0;
                    for (val, freq) in mcv.values.iter().zip(&mcv.frequencies) {
                        if range.as_ref().map_or(true, |r| value_in_range(val, r)) {
                            in_range_freq += freq;
                        }
                    }
                    (in_range_freq, mcv.sum_frequencies())
                }
                None => (0.0, 0.0),
            };

            let other_frac = (1.0 - col_stats.null_fraction - sum_common).max(0.0);

            // --- Histogram-based estimate on non-MCV population ---
            if let Some(ref histogram) = col_stats.histogram {
                if let Some(range) = range.as_ref() {
                    let lower = bound_value(&range.lower);
                    let upper = bound_value(&range.upper);

                    // Try direct range lookup first.
                    if let Some(hist_sel) = histogram.estimate_range_selectivity(lower, upper) {
                        let sel = mcv_sel + other_frac * hist_sel;
                        return sel.clamp(MIN_SELECTIVITY, 1.0);
                    }

                    // Paired-bounds formula: hisel + losel - 1.0 + nullfrac.
                    // More accurate than multiplying independent selectivities.
                    if lower.is_some() && upper.is_some() {
                        let lo_sel = histogram.estimate_range_selectivity(lower, None);
                        let hi_sel = histogram.estimate_range_selectivity(None, upper);
                        if let (Some(lo), Some(hi)) = (lo_sel, hi_sel) {
                            let paired =
                                (hi + lo - 1.0 + col_stats.null_fraction).max(MIN_SELECTIVITY);
                            let sel = mcv_sel + other_frac * paired;
                            return sel.clamp(MIN_SELECTIVITY, 1.0);
                        }
                    }
                }
            }

            // No histogram - use default constants adjusted for nulls.
            let base = range.as_ref().map_or(DEFAULT_RANGE_SELECTIVITY, |r| {
                if has_both_range_bounds(r) {
                    BOUNDED_RANGE_SELECTIVITY
                } else {
                    fallback_open_range_selectivity(col_stats.ndistinct)
                }
            });
            return apply_null_fraction(base, col_stats.null_fraction);
        }
    }

    // No stats at all.
    range.as_ref().map_or(DEFAULT_RANGE_SELECTIVITY, |r| {
        if has_both_range_bounds(r) {
            BOUNDED_RANGE_SELECTIVITY
        } else {
            DEFAULT_RANGE_SELECTIVITY
        }
    })
}

/// Look up the Pearson correlation of an index's leading key column from
/// the table statistics, returning 0.0 (PG's conservative pre-ANALYZE
/// default) when no entry exists. This is the same column whose value
/// order the cost model wants to compare against heap order.
fn index_leading_column_correlation(
    indexes: &[IndexDescriptor],
    stats: Option<&TableStatistics>,
    index_id: IndexId,
) -> f64 {
    let Some(stats) = stats else {
        return 0.0;
    };
    let Some(index) = indexes.iter().find(|idx| idx.index_id == index_id) else {
        return 0.0;
    };
    let Some(leading) = index.key_columns.first() else {
        return 0.0;
    };
    stats
        .column_stats
        .iter()
        .find(|cs| cs.column_id == leading.column_id)
        .map(|cs| cs.correlation)
        .unwrap_or(0.0)
}

/// Fallback selectivity for one-sided ranges when histogram data is unavailable.
///
/// We adapt the estimate using ndistinct for high-cardinality columns:
/// `100 / ndistinct` clamped to [0.01, 0.33]. This avoids massively
/// overestimating predicates like `unique_col < 10` on large unique domains.
fn fallback_open_range_selectivity(ndistinct: f64) -> f64 {
    if !ndistinct.is_finite() || ndistinct <= 0.0 {
        return DEFAULT_RANGE_SELECTIVITY;
    }
    (HIGH_NDISTINCT_RANGE_BUCKET_WIDTH / ndistinct).clamp(
        HIGH_NDISTINCT_MIN_RANGE_SELECTIVITY,
        DEFAULT_RANGE_SELECTIVITY,
    )
}

/// Check whether a value falls within a range constraint.
fn value_in_range(value: &Value, range: &RangeConstraint) -> bool {
    let above_lower = match &range.lower {
        Bound::Unbounded => true,
        Bound::Included(lo) => {
            compare_literal_values(value, lo).is_some_and(|o| o != Ordering::Less)
        }
        Bound::Excluded(lo) => {
            compare_literal_values(value, lo).is_some_and(|o| o == Ordering::Greater)
        }
    };
    let below_upper = match &range.upper {
        Bound::Unbounded => true,
        Bound::Included(hi) => {
            compare_literal_values(value, hi).is_some_and(|o| o != Ordering::Greater)
        }
        Bound::Excluded(hi) => {
            compare_literal_values(value, hi).is_some_and(|o| o == Ordering::Less)
        }
    };
    above_lower && below_upper
}

fn apply_null_fraction(selectivity: f64, null_fraction: f64) -> f64 {
    let live_fraction = 1.0 - null_fraction.clamp(0.0, 1.0);
    (selectivity * live_fraction).clamp(MIN_SELECTIVITY, 1.0)
}

fn extract_eq_column_id(filter: &TypedExpr, table: &TableDescriptor) -> Option<ColumnId> {
    match &filter.kind {
        TypedExprKind::BinaryEq { left, right } => {
            eq_column_id(left, table).or_else(|| eq_column_id(right, table))
        }
        TypedExprKind::LogicalAnd { left, right } => {
            extract_eq_column_id(left, table).or_else(|| extract_eq_column_id(right, table))
        }
        _ => None,
    }
}

fn eq_column_id(expr: &TypedExpr, table: &TableDescriptor) -> Option<ColumnId> {
    if let TypedExprKind::ColumnRef { ordinal, .. } = &expr.kind {
        table.columns.get(*ordinal).map(|column| column.column_id)
    } else {
        None
    }
}

fn extract_range_column_id(filter: &TypedExpr, table: &TableDescriptor) -> Option<ColumnId> {
    match &filter.kind {
        TypedExprKind::LogicalAnd { left, right } => {
            extract_range_column_id(left, table).or_else(|| extract_range_column_id(right, table))
        }
        TypedExprKind::Between {
            expr,
            low,
            high,
            negated,
        } => (!negated)
            .then_some(())
            .and_then(|()| between_column_id(expr, low, high, table)),
        TypedExprKind::BinaryGt { left, right }
        | TypedExprKind::BinaryGe { left, right }
        | TypedExprKind::BinaryLt { left, right }
        | TypedExprKind::BinaryLe { left, right } => {
            range_column_id(left, right, table).or_else(|| range_column_id(right, left, table))
        }
        _ => None,
    }
}

fn range_column_id(
    candidate_column: &TypedExpr,
    candidate_literal: &TypedExpr,
    table: &TableDescriptor,
) -> Option<ColumnId> {
    let TypedExprKind::ColumnRef { ordinal, .. } = &candidate_column.kind else {
        return None;
    };
    extract_constant_value(candidate_literal)
        .and_then(|value| (!matches!(value, Value::Null)).then_some(()))
        .and_then(|()| table.columns.get(*ordinal).map(|column| column.column_id))
}

fn between_column_id(
    candidate_column: &TypedExpr,
    candidate_low: &TypedExpr,
    candidate_high: &TypedExpr,
    table: &TableDescriptor,
) -> Option<ColumnId> {
    let TypedExprKind::ColumnRef { ordinal, .. } = &candidate_column.kind else {
        return None;
    };
    let low = extract_constant_value(candidate_low)?;
    let high = extract_constant_value(candidate_high)?;
    if matches!(low, Value::Null) || matches!(high, Value::Null) {
        return None;
    }
    table.columns.get(*ordinal).map(|column| column.column_id)
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn extract_index_access_path(
    filter: &TypedExpr,
    table: &TableDescriptor,
    index_id: IndexId,
    column_id: ColumnId,
) -> Option<ScanAccessPath> {
    extract_column_access_constraint(filter, table, column_id)
        .map(|extracted| extracted.with_index_id(index_id))
}

pub(crate) fn extract_index_prefix_access_path(
    filter: &TypedExpr,
    table: &TableDescriptor,
    index: &IndexDescriptor,
) -> Option<ScanAccessPath> {
    if index.key_columns.is_empty() {
        return None;
    }

    let mut eq_values = Vec::new();
    let mut trailing_range = None;

    for key_col in &index.key_columns {
        match extract_column_access_constraint(filter, table, key_col.column_id) {
            Some(ColumnAccessConstraint::Eq(value)) => eq_values.push(value),
            Some(range @ ColumnAccessConstraint::Range(_)) => {
                trailing_range = Some(range);
                break;
            }
            None => break,
        }
    }

    if !eq_values.is_empty() {
        if let Some(ColumnAccessConstraint::Range(range)) = trailing_range {
            Some(ScanAccessPath::IndexEqRangeComposite {
                index_id: index.index_id,
                eq_values,
                lower: range.lower,
                upper: range.upper,
            })
        } else if index.key_columns.len() > 1 {
            Some(ScanAccessPath::IndexEqComposite {
                index_id: index.index_id,
                values: eq_values,
            })
        } else {
            Some(ScanAccessPath::IndexEq {
                index_id: index.index_id,
                value: eq_values
                    .into_iter()
                    .next()
                    .expect("eq_values is not empty"),
            })
        }
    } else if let Some(ColumnAccessConstraint::Range(range)) = trailing_range {
        Some(ScanAccessPath::IndexRange {
            index_id: index.index_id,
            lower: range.lower,
            upper: range.upper,
        })
    } else {
        None
    }
}

#[derive(Clone, Debug)]
enum ColumnAccessConstraint {
    Eq(Value),
    Range(RangeConstraint),
}

impl ColumnAccessConstraint {
    fn with_index_id(self, index_id: IndexId) -> ScanAccessPath {
        match self {
            Self::Eq(value) => ScanAccessPath::IndexEq { index_id, value },
            Self::Range(range) => ScanAccessPath::IndexRange {
                index_id,
                lower: range.lower,
                upper: range.upper,
            },
        }
    }
}

fn extract_column_access_constraint(
    filter: &TypedExpr,
    table: &TableDescriptor,
    column_id: ColumnId,
) -> Option<ColumnAccessConstraint> {
    if let Some(value) = extract_index_lookup_value(filter, table, column_id) {
        return Some(ColumnAccessConstraint::Eq(value));
    }

    let range = extract_index_range(filter, table, column_id)?;
    if let Some(value) = range_point_value(&range) {
        return Some(ColumnAccessConstraint::Eq(value));
    }

    if range.is_empty() || range.is_unbounded() {
        None
    } else {
        Some(ColumnAccessConstraint::Range(range))
    }
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn extract_index_lookup_value(
    filter: &TypedExpr,
    table: &TableDescriptor,
    column_id: ColumnId,
) -> Option<Value> {
    extract_index_lookup_value_direct(filter, table, column_id)
}

pub(crate) fn extract_small_in_list_values(
    filter: &TypedExpr,
    table: &TableDescriptor,
    column_id: ColumnId,
) -> Option<Vec<Value>> {
    match &filter.kind {
        TypedExprKind::InList {
            expr,
            list,
            negated,
        } => {
            if *negated || list.is_empty() {
                return None;
            }
            let TypedExprKind::ColumnRef { ordinal, .. } = &expr.kind else {
                return None;
            };
            if !table
                .columns
                .get(*ordinal)
                .is_some_and(|column| column.column_id == column_id)
            {
                return None;
            }
            let mut values = Vec::with_capacity(list.len());
            for element in list {
                let TypedExprKind::Literal(value) = &element.kind else {
                    return None;
                };
                if !matches!(value, Value::Null) && !values.contains(value) {
                    values.push(value.clone());
                }
            }
            Some(values)
        }
        TypedExprKind::LogicalAnd { left, right } => {
            extract_small_in_list_values(left, table, column_id)
                .or_else(|| extract_small_in_list_values(right, table, column_id))
        }
        _ => None,
    }
}

pub(crate) fn extract_small_or_chain_values(
    filter: &TypedExpr,
    table: &TableDescriptor,
    column_id: ColumnId,
) -> Option<Vec<Value>> {
    const MAX_OR_CHAIN_BITMAP_OR_LEN: usize = 64;
    match &filter.kind {
        TypedExprKind::LogicalOr { .. } => {
            let disjuncts = collect_or_disjuncts(filter);
            if disjuncts.len() < 2 {
                return None;
            }
            let mut values = Vec::with_capacity(disjuncts.len());
            for disjunct in disjuncts {
                let value = extract_index_lookup_value(disjunct, table, column_id)?;
                if matches!(value, Value::Null) {
                    return None;
                }
                if !values.contains(&value) {
                    if values.len() >= MAX_OR_CHAIN_BITMAP_OR_LEN {
                        return None;
                    }
                    values.push(value);
                }
            }
            Some(values)
        }
        TypedExprKind::LogicalAnd { left, right } => {
            extract_small_or_chain_values(left, table, column_id)
                .or_else(|| extract_small_or_chain_values(right, table, column_id))
        }
        _ => None,
    }
}

fn extract_index_lookup_value_direct(
    filter: &TypedExpr,
    table: &TableDescriptor,
    column_id: ColumnId,
) -> Option<Value> {
    match &filter.kind {
        TypedExprKind::BinaryEq { left, right } => {
            column_equals_literal(left, right, table, column_id)
                .or_else(|| column_equals_literal(right, left, table, column_id))
        }
        TypedExprKind::LogicalAnd { left, right } => {
            extract_index_lookup_value_direct(left, table, column_id)
                .or_else(|| extract_index_lookup_value_direct(right, table, column_id))
        }
        _ => None,
    }
}

pub(crate) fn extract_index_range(
    filter: &TypedExpr,
    table: &TableDescriptor,
    column_id: ColumnId,
) -> Option<RangeConstraint> {
    match &filter.kind {
        TypedExprKind::LogicalAnd { left, right } => {
            let left = extract_index_range(left, table, column_id);
            let right = extract_index_range(right, table, column_id);
            match (left, right) {
                (Some(left), Some(right)) => Some(left.merge(right)),
                (Some(left), None) => Some(left),
                (None, Some(right)) => Some(right),
                (None, None) => None,
            }
        }
        TypedExprKind::BinaryGt { left, right } => {
            column_range_constraint(left, right, table, column_id, Bound::Excluded, |_| {
                Bound::Unbounded
            })
            .or_else(|| {
                column_range_constraint(
                    right,
                    left,
                    table,
                    column_id,
                    |_| Bound::Unbounded,
                    Bound::Excluded,
                )
            })
        }
        TypedExprKind::BinaryGe { left, right } => {
            column_range_constraint(left, right, table, column_id, Bound::Included, |_| {
                Bound::Unbounded
            })
            .or_else(|| {
                column_range_constraint(
                    right,
                    left,
                    table,
                    column_id,
                    |_| Bound::Unbounded,
                    Bound::Included,
                )
            })
        }
        TypedExprKind::BinaryLt { left, right } => column_range_constraint(
            left,
            right,
            table,
            column_id,
            |_| Bound::Unbounded,
            Bound::Excluded,
        )
        .or_else(|| {
            column_range_constraint(right, left, table, column_id, Bound::Excluded, |_| {
                Bound::Unbounded
            })
        }),
        TypedExprKind::BinaryLe { left, right } => column_range_constraint(
            left,
            right,
            table,
            column_id,
            |_| Bound::Unbounded,
            Bound::Included,
        )
        .or_else(|| {
            column_range_constraint(right, left, table, column_id, Bound::Included, |_| {
                Bound::Unbounded
            })
        }),
        TypedExprKind::Between {
            expr,
            low,
            high,
            negated,
        } => (!negated)
            .then_some(())
            .and_then(|()| column_between_constraint(expr, low, high, table, column_id)),
        // `col LIKE 'prefix%'` becomes a half-open range `[prefix,
        // prefix_succ)` on the indexed column. This unlocks
        // index-range lookups for the very common autocomplete /
        // wildcard-search shape, which previously fell through to a
        // SeqScan + per-row LIKE evaluation.
        TypedExprKind::Like {
            expr,
            pattern,
            negated,
            case_insensitive,
        } if !*negated && !*case_insensitive => {
            column_like_prefix_constraint(expr, pattern, table, column_id)
        }
        _ => None,
    }
}

/// Extract a half-open range `[prefix, prefix_succ)` from a
/// `column LIKE 'literal_prefix%'` predicate. Returns `None` when:
/// - the pattern isn't a literal text value
/// - the pattern uses LIKE wildcards (`%` / `_`) or escapes (`\`)
///   anywhere except for a single trailing `%`
/// - the prefix is empty (no point doing an index range over the
///   whole table)
/// - we can't compute a successor (prefix is the lexicographically
///   maximal text)
fn column_like_prefix_constraint(
    candidate_column: &TypedExpr,
    pattern_expr: &TypedExpr,
    table: &TableDescriptor,
    column_id: ColumnId,
) -> Option<RangeConstraint> {
    let candidate_column = strip_index_cast_wrappers(candidate_column);
    let TypedExprKind::ColumnRef { ordinal, .. } = &candidate_column.kind else {
        return None;
    };
    let column = table.columns.get(*ordinal)?;
    if column.column_id != column_id {
        return None;
    }
    // Pattern must be a literal text value that's `prefix%`, with no
    // wildcards or escape characters inside `prefix`.
    let TypedExprKind::Literal(value) = &pattern_expr.kind else {
        return None;
    };
    let Value::Text(pattern) = value else {
        return None;
    };
    let prefix = like_pattern_to_prefix(pattern)?;
    if prefix.is_empty() {
        return None;
    }
    let upper = like_prefix_successor(&prefix)?;
    Some(RangeConstraint {
        lower: Bound::Included(Value::Text(prefix)),
        upper: Bound::Excluded(Value::Text(upper)),
    })
}

/// Returns `Some(prefix)` when `pattern` is `<prefix>%` with no
/// wildcards or escape characters inside `<prefix>`. Returns `None`
/// for patterns like `%foo`, `f%o`, `foo` (no trailing `%`), or
/// patterns containing `_` / `\` in the prefix.
fn like_pattern_to_prefix(pattern: &str) -> Option<String> {
    if !pattern.ends_with('%') {
        return None;
    }
    let prefix = &pattern[..pattern.len() - '%'.len_utf8()];
    if prefix
        .chars()
        .any(|ch| ch == '%' || ch == '_' || ch == '\\')
    {
        return None;
    }
    Some(prefix.to_owned())
}

/// Compute the lexicographic successor of `prefix` for the upper
/// bound of a half-open range. Increments the last codepoint by
/// one; returns `None` when no valid successor exists (prefix is
/// the maximal text). The PG semantics for `LIKE 'a%'` match
/// `[a, b)` for ASCII; this helper preserves that behaviour for
/// arbitrary UTF-8 by walking back over codepoints that are
/// already at the maximum.
fn like_prefix_successor(prefix: &str) -> Option<String> {
    let mut chars: Vec<char> = prefix.chars().collect();
    while let Some(last) = chars.pop() {
        let next_code = (last as u32).checked_add(1)?;
        // Skip the UTF-16 surrogate range [0xD800, 0xDFFF] which is
        // not a valid scalar value.
        let next_code = if (0xD800..=0xDFFF).contains(&next_code) {
            0xE000
        } else {
            next_code
        };
        if next_code > 0x10_FFFF {
            // This codepoint can't be incremented; carry over by
            // dropping it and incrementing the previous one.
            continue;
        }
        if let Some(next_char) = char::from_u32(next_code) {
            let mut out: String = chars.into_iter().collect();
            out.push(next_char);
            return Some(out);
        }
    }
    None
}

fn column_equals_literal(
    candidate_column: &TypedExpr,
    candidate_literal: &TypedExpr,
    table: &TableDescriptor,
    column_id: ColumnId,
) -> Option<Value> {
    let candidate_column = strip_index_cast_wrappers(candidate_column);
    let TypedExprKind::ColumnRef { ordinal, .. } = &candidate_column.kind else {
        return None;
    };
    let column = table.columns.get(*ordinal)?;
    if column.column_id != column_id {
        return None;
    }

    extract_constant_value(candidate_literal).filter(|value| !matches!(value, Value::Null))
}

fn column_range_constraint(
    candidate_column: &TypedExpr,
    candidate_literal: &TypedExpr,
    table: &TableDescriptor,
    column_id: ColumnId,
    lower_ctor: impl FnOnce(Value) -> Bound<Value>,
    upper_ctor: impl FnOnce(Value) -> Bound<Value>,
) -> Option<RangeConstraint> {
    let candidate_column = strip_index_cast_wrappers(candidate_column);
    let TypedExprKind::ColumnRef { ordinal, .. } = &candidate_column.kind else {
        return None;
    };
    let column = table.columns.get(*ordinal)?;
    if column.column_id != column_id {
        return None;
    }

    let value = extract_constant_value(candidate_literal)?;
    if matches!(value, Value::Null) {
        return None;
    }
    Some(RangeConstraint {
        lower: lower_ctor(value.clone()),
        upper: upper_ctor(value),
    })
}

fn column_between_constraint(
    candidate_column: &TypedExpr,
    candidate_low: &TypedExpr,
    candidate_high: &TypedExpr,
    table: &TableDescriptor,
    column_id: ColumnId,
) -> Option<RangeConstraint> {
    let candidate_column = strip_index_cast_wrappers(candidate_column);
    let TypedExprKind::ColumnRef { ordinal, .. } = &candidate_column.kind else {
        return None;
    };
    let column = table.columns.get(*ordinal)?;
    if column.column_id != column_id {
        return None;
    }

    let low = extract_constant_value(candidate_low)?;
    let high = extract_constant_value(candidate_high)?;
    if matches!(low, Value::Null) || matches!(high, Value::Null) {
        return None;
    }
    Some(RangeConstraint {
        lower: Bound::Included(low),
        upper: Bound::Included(high),
    })
}

#[derive(Clone, Debug)]
pub(crate) struct RangeConstraint {
    pub(crate) lower: Bound<Value>,
    pub(crate) upper: Bound<Value>,
}

fn range_point_value(range: &RangeConstraint) -> Option<Value> {
    let (Bound::Included(lower), Bound::Included(upper)) = (&range.lower, &range.upper) else {
        return None;
    };
    (compare_literal_values(lower, upper) == Some(Ordering::Equal)).then(|| lower.clone())
}

impl RangeConstraint {
    pub(crate) fn is_unbounded(&self) -> bool {
        matches!(self.lower, Bound::Unbounded) && matches!(self.upper, Bound::Unbounded)
    }

    pub(crate) fn is_empty(&self) -> bool {
        let (Some(lower), Some(upper)) = (bound_value(&self.lower), bound_value(&self.upper))
        else {
            return false;
        };

        match compare_literal_values(lower, upper) {
            Some(Ordering::Greater) => true,
            Some(Ordering::Equal) => {
                matches!(self.lower, Bound::Excluded(_)) || matches!(self.upper, Bound::Excluded(_))
            }
            _ => false,
        }
    }

    pub(crate) fn merge(self, other: Self) -> Self {
        Self {
            lower: tighter_lower_bound(self.lower, other.lower),
            upper: tighter_upper_bound(self.upper, other.upper),
        }
    }
}

fn has_both_range_bounds(range: &RangeConstraint) -> bool {
    !matches!(range.lower, Bound::Unbounded) && !matches!(range.upper, Bound::Unbounded)
}

fn tighter_lower_bound(left: Bound<Value>, right: Bound<Value>) -> Bound<Value> {
    match (&left, &right) {
        (Bound::Unbounded, _) => return right,
        (_, Bound::Unbounded) => return left,
        _ => {}
    }

    let (Some(left_value), Some(right_value)) = (bound_value(&left), bound_value(&right)) else {
        return left;
    };

    match compare_literal_values(left_value, right_value) {
        Some(Ordering::Less) => right,
        Some(Ordering::Greater) => left,
        Some(Ordering::Equal) => {
            if matches!(left, Bound::Excluded(_)) {
                left
            } else if matches!(right, Bound::Excluded(_)) {
                right
            } else {
                left
            }
        }
        None => left,
    }
}

fn tighter_upper_bound(left: Bound<Value>, right: Bound<Value>) -> Bound<Value> {
    match (&left, &right) {
        (Bound::Unbounded, _) => return right,
        (_, Bound::Unbounded) => return left,
        _ => {}
    }

    let (Some(left_value), Some(right_value)) = (bound_value(&left), bound_value(&right)) else {
        return left;
    };

    match compare_literal_values(left_value, right_value) {
        Some(Ordering::Less) => left,
        Some(Ordering::Greater) => right,
        Some(Ordering::Equal) => {
            if matches!(left, Bound::Excluded(_)) {
                left
            } else if matches!(right, Bound::Excluded(_)) {
                right
            } else {
                left
            }
        }
        None => left,
    }
}

pub(crate) fn compare_literal_values(left: &Value, right: &Value) -> Option<Ordering> {
    fn numeric_to_f64(value: &Value) -> Option<f64> {
        match value {
            Value::Int(v) => Some(f64::from(*v)),
            Value::BigInt(v) => Some(i64_to_f64(*v)),
            Value::Real(v) => Some(f64::from(*v)),
            Value::Double(v) => Some(*v),
            Value::Numeric(v) => Some(v.to_f64()),
            Value::Money(v) => Some(i64_to_f64(*v)),
            Value::Boolean(v) => Some(if *v { 1.0 } else { 0.0 }),
            _ => None,
        }
    }

    fn compare_array_literal_values(left: &[Value], right: &[Value]) -> Option<Ordering> {
        for (left_value, right_value) in left.iter().zip(right.iter()) {
            let ordering = match (left_value, right_value) {
                (Value::Null, Value::Null) => Ordering::Equal,
                (Value::Null, _) => Ordering::Greater,
                (_, Value::Null) => Ordering::Less,
                _ => compare_literal_values(left_value, right_value)?,
            };
            if ordering != Ordering::Equal {
                return Some(ordering);
            }
        }
        Some(left.len().cmp(&right.len()))
    }

    match (left, right) {
        _ if numeric_to_f64(left).is_some() && numeric_to_f64(right).is_some() => {
            if let (Some(left_num), Some(right_num)) = (numeric_to_f64(left), numeric_to_f64(right))
            {
                Some(left_num.total_cmp(&right_num))
            } else {
                None
            }
        }
        (Value::Int(left), Value::Int(right)) => Some(left.cmp(right)),
        (Value::BigInt(left), Value::BigInt(right)) => Some(left.cmp(right)),
        (Value::Text(left), Value::Text(right)) => Some(left.cmp(right)),
        (Value::Boolean(left), Value::Boolean(right)) => Some(left.cmp(right)),
        (Value::Blob(left), Value::Blob(right)) => Some(left.cmp(right)),
        (Value::Real(left), Value::Real(right)) => left.partial_cmp(right),
        (Value::Double(left), Value::Double(right)) => left.partial_cmp(right),
        (Value::Array(left), Value::Array(right)) => compare_array_literal_values(left, right),
        _ => None,
    }
}

fn strip_index_cast_wrappers(expr: &TypedExpr) -> &TypedExpr {
    let mut current = expr;
    while let TypedExprKind::Cast { expr, .. } = &current.kind {
        current = expr;
    }
    current
}

fn extract_constant_value(expr: &TypedExpr) -> Option<Value> {
    let mut current = expr;
    let mut casts = Vec::new();
    while let TypedExprKind::Cast { expr, target_type } = &current.kind {
        casts.push(target_type);
        current = expr;
    }
    let TypedExprKind::Literal(value) = &current.kind else {
        return None;
    };
    let mut value = value.clone();
    for target_type in casts.into_iter().rev() {
        value = aiondb_eval::coercions::coerce_value(value, target_type).ok()?;
    }
    Some(value)
}

fn bound_value(bound: &Bound<Value>) -> Option<&Value> {
    match bound {
        Bound::Included(value) | Bound::Excluded(value) => Some(value),
        Bound::Unbounded => None,
    }
}

/// Extract the IndexId from a single-index access path.
fn scan_access_path_index_id(path: &ScanAccessPath) -> Option<IndexId> {
    let mut current = path;
    loop {
        match current {
            ScanAccessPath::IndexEq { index_id, .. }
            | ScanAccessPath::IndexEqComposite { index_id, .. }
            | ScanAccessPath::IndexEqRangeComposite { index_id, .. }
            | ScanAccessPath::IndexRange { index_id, .. }
            | ScanAccessPath::GinContainment { index_id, .. } => return Some(*index_id),
            ScanAccessPath::IndexOnlyScan { inner, .. } => current = inner,
            ScanAccessPath::SeqScan
            | ScanAccessPath::BitmapOr { .. }
            | ScanAccessPath::BitmapAnd { .. } => return None,
        }
    }
}

fn scan_access_path_constrained_columns(
    path: &ScanAccessPath,
    indexes: &[IndexDescriptor],
) -> Option<Vec<ColumnId>> {
    let index_id = scan_access_path_index_id(path)?;
    let index = indexes.iter().find(|index| index.index_id == index_id)?;
    let constrained_len = match path {
        ScanAccessPath::IndexEq { .. } | ScanAccessPath::IndexRange { .. } => 1,
        ScanAccessPath::IndexEqComposite { values, .. } => values.len(),
        ScanAccessPath::IndexEqRangeComposite { eq_values, .. } => eq_values.len() + 1,
        ScanAccessPath::IndexOnlyScan { inner, .. } => {
            return scan_access_path_constrained_columns(inner, indexes);
        }
        ScanAccessPath::SeqScan
        | ScanAccessPath::GinContainment { .. }
        | ScanAccessPath::BitmapOr { .. }
        | ScanAccessPath::BitmapAnd { .. } => return None,
    };
    if constrained_len == 0 || constrained_len > index.key_columns.len() {
        return None;
    }
    Some(
        index
            .key_columns
            .iter()
            .take(constrained_len)
            .map(|column| column.column_id)
            .collect(),
    )
}

fn index_keys_appear_high_distinct(
    index: &IndexDescriptor,
    stats: Option<&TableStatistics>,
) -> bool {
    let Some(stats) = stats else {
        return true;
    };
    if stats.row_count <= 1 {
        return true;
    }
    let min_distinct = (u64_to_f64(stats.row_count) * 0.5).max(1.0);
    index.key_columns.iter().all(|key_col| {
        stats
            .column_stats
            .iter()
            .find(|col| col.column_id == key_col.column_id)
            .map_or(true, |col| col.ndistinct >= min_distinct)
    })
}

/// Collect the top-level OR disjuncts from a filter expression.
fn collect_or_disjuncts(filter: &TypedExpr) -> Vec<&TypedExpr> {
    let mut disjuncts = Vec::new();
    let mut stack = vec![filter];
    while let Some(expr) = stack.pop() {
        match &expr.kind {
            TypedExprKind::LogicalOr { left, right } => {
                stack.push(right);
                stack.push(left);
            }
            _ => disjuncts.push(expr),
        }
    }
    disjuncts
}

fn extract_json_contains(
    filter: &TypedExpr,
    table: &TableDescriptor,
) -> Option<(ColumnId, serde_json::Value)> {
    match &filter.kind {
        TypedExprKind::JsonContains { left, right } => {
            let TypedExprKind::ColumnRef { ordinal, .. } = &left.kind else {
                return None;
            };
            let column = table.columns.get(*ordinal)?;
            let TypedExprKind::Literal(Value::Jsonb(pattern)) = &right.kind else {
                return None;
            };
            Some((column.column_id, pattern.clone()))
        }
        TypedExprKind::LogicalAnd { left, right } => {
            extract_json_contains(left, table).or_else(|| extract_json_contains(right, table))
        }
        _ => None,
    }
}
