use crate::{
    access_path::{
        extract_index_lookup_value, extract_index_prefix_access_path, extract_index_range,
        extract_small_in_list_values, extract_small_or_chain_values,
    },
    predicate_pushdown, Optimizer,
};
use aiondb_catalog::{
    CatalogReader, IndexDescriptor, IndexKind, TableDescriptor, TableStatistics,
    VectorDistanceMetric,
};
use aiondb_core::{
    bounded_hnsw_ef_search, ColumnId, DataType, DbError, DbResult, RelationId, TxnId, Value,
    VectorValue, HNSW_MAX_EF_SEARCH, VECTOR_MAX_K,
};
use aiondb_plan::{
    PhysicalPlan, ProjectionExpr, ResultField, ScalarFunction, ScanAccessPath, SortExpr, TypedExpr,
    TypedExprKind,
};

const HNSW_FILTER_OVERSAMPLE_FACTOR: usize = 4;
const HNSW_FILTER_MIN_CANDIDATES: usize = 64;
const SELECTIVE_BITMAP_AND_SELECTIVITY: f64 = 0.02;
const SELECTIVE_BITMAP_OR_SELECTIVITY: f64 = 0.20;
const DEFAULT_BITMAP_AND_EQ_SELECTIVITY: f64 = 0.01;
const DEFAULT_BITMAP_AND_RANGE_SELECTIVITY: f64 = 0.33;
const DEFAULT_BITMAP_AND_BOUNDED_RANGE_SELECTIVITY: f64 = 0.10;
const MAX_BITMAP_OR_LITERAL_COUNT: usize = 64;

/// Map a scalar distance function used in `ORDER BY` to the index metric it
/// requires for a valid HNSW lowering.
///
/// Only true distance-shaped functions (smaller = closer) can be lowered to
/// HnswScan. `inner_product` is a similarity (higher = closer), so it lowers
/// only for `ORDER BY inner_product(...) DESC`. The negated variant
/// (`negative_inner_product` / pgvector `<#>`) lowers for ascending order.
struct VectorOrderSpec {
    metric: VectorDistanceMetric,
    requires_descending: bool,
}

fn distance_function_order_spec(func: &ScalarFunction) -> Option<VectorOrderSpec> {
    match func {
        ScalarFunction::L2Distance => Some(VectorOrderSpec {
            metric: VectorDistanceMetric::L2,
            requires_descending: false,
        }),
        ScalarFunction::CosineDistance => Some(VectorOrderSpec {
            metric: VectorDistanceMetric::Cosine,
            requires_descending: false,
        }),
        ScalarFunction::ManhattanDistance => Some(VectorOrderSpec {
            metric: VectorDistanceMetric::Manhattan,
            requires_descending: false,
        }),
        ScalarFunction::NegativeInnerProduct => Some(VectorOrderSpec {
            metric: VectorDistanceMetric::InnerProduct,
            requires_descending: false,
        }),
        ScalarFunction::InnerProduct => Some(VectorOrderSpec {
            metric: VectorDistanceMetric::InnerProduct,
            requires_descending: true,
        }),
        _ => None,
    }
}

impl Optimizer {
    pub(crate) fn try_hnsw_scan(
        &self,
        txn_id: TxnId,
        table_id: RelationId,
        outputs: &[ProjectionExpr],
        filter: &Option<TypedExpr>,
        order_by: &[SortExpr],
        limit: &Option<TypedExpr>,
        offset: &Option<TypedExpr>,
        distinct: bool,
        hnsw_ef_search_setting: Option<usize>,
    ) -> DbResult<Option<PhysicalPlan>> {
        if distinct {
            return Ok(None);
        }

        let Some(k_usize) = limit.as_ref().and_then(parse_positive_limit) else {
            return Ok(None);
        };
        let Some(offset_rows) = parse_nonnegative_offset(offset) else {
            return Ok(None);
        };

        if order_by.len() != 1 {
            return Ok(None);
        }

        let sort_expr = &order_by[0].expr;
        let (column_id, query_vector, required_metric, requires_descending) =
            match extract_distance_call(sort_expr, self.catalog_reader.as_ref(), txn_id, table_id)?
            {
                Some(triple) => triple,
                None => return Ok(None),
            };
        if order_by[0].descending != requires_descending {
            return Ok(None);
        }

        let indexes = self.catalog_reader.list_indexes(txn_id, table_id)?;
        let hnsw_index = indexes.iter().find(|index| {
            if index.kind != IndexKind::Hnsw {
                return false;
            }
            if index.key_columns.len() != 1 || index.key_columns[0].column_id != column_id {
                return false;
            }
            index.hnsw_distance_metric() == Some(required_metric)
        });
        let Some(hnsw_index) = hnsw_index else {
            return Ok(None);
        };

        let Some(table) = self.catalog_reader.get_table_by_id(txn_id, table_id)? else {
            return Ok(None);
        };

        if let Some(filter) = filter.as_ref() {
            let stats = self.catalog_reader.get_statistics(txn_id, table_id)?;
            if filter_has_selective_sql_lookup(filter, &table, &indexes, column_id, stats.as_ref())
            {
                return Ok(None);
            }
        }

        let requested_rows = k_usize.saturating_add(offset_rows);
        if requested_rows > VECTOR_MAX_K {
            return Ok(None);
        }
        let candidate_limit = hnsw_candidate_limit(requested_rows, filter.is_some());
        let adaptive_ef_search = bounded_hnsw_ef_search(candidate_limit);
        let ef_search = hnsw_ef_search_setting
            .unwrap_or(adaptive_ef_search)
            .max(candidate_limit)
            .max(adaptive_ef_search)
            .min(HNSW_MAX_EF_SEARCH);
        let output_fields: Vec<ResultField> =
            outputs.iter().map(|output| output.field.clone()).collect();
        let direct_projected_ordinals = extract_projected_ordinals(outputs, &table);
        let requires_wrapper =
            filter.is_some() || offset_rows > 0 || direct_projected_ordinals.is_none();

        if !requires_wrapper {
            let Some(projected_ordinals) = direct_projected_ordinals else {
                unreachable!("requires_wrapper=false implies direct projection");
            };
            return Ok(Some(PhysicalPlan::HnswScan {
                table_id,
                index_id: hnsw_index.index_id,
                query_vector,
                limit: candidate_limit,
                ef_search,
                projected_ordinals,
                output_fields,
            }));
        }

        // Wrapper ANN path: run HNSW first (with bounded over-fetch), then
        // evaluate SQL filter/projection/offset in ProjectSource while
        // preserving the source distance ordering.
        let (source_projected_ordinals, source_output_fields) =
            hnsw_wrapper_source_projection(&table, outputs, filter.as_ref());
        let source = PhysicalPlan::HnswScan {
            table_id,
            index_id: hnsw_index.index_id,
            query_vector,
            limit: candidate_limit,
            ef_search,
            projected_ordinals: source_projected_ordinals,
            output_fields: source_output_fields,
        };

        Ok(Some(PhysicalPlan::ProjectSource {
            source: Box::new(source),
            outputs: outputs.to_vec(),
            filter: filter.clone(),
            // Source rows are already distance-ordered by HNSW.
            order_by: Vec::new(),
            limit: limit.clone(),
            offset: offset.clone(),
            distinct: false,
            distinct_on: Vec::new(),
        }))
    }
}

fn filter_has_selective_sql_lookup(
    filter: &TypedExpr,
    table: &TableDescriptor,
    indexes: &[IndexDescriptor],
    vector_column_id: ColumnId,
    stats: Option<&TableStatistics>,
) -> bool {
    if !indexes.iter().any(|index| {
        index.kind == IndexKind::BTree
            && !index.key_columns.is_empty()
            && !index
                .key_columns
                .iter()
                .any(|column| column.column_id == vector_column_id)
    }) {
        return false;
    }

    if filter_has_selective_bitmap_and(filter, table, indexes, vector_column_id, stats) {
        return true;
    }

    indexes.iter().any(|index| {
        if index.kind != IndexKind::BTree
            || index.key_columns.is_empty()
            || index
                .key_columns
                .iter()
                .any(|column| column.column_id == vector_column_id)
        {
            return false;
        }
        let leading_column_id = index.key_columns[0].column_id;
        let exact_unique_lookup = index.unique
            && index.key_columns.iter().all(|column| {
                extract_index_lookup_value(filter, table, column.column_id).is_some()
            });
        if exact_unique_lookup {
            return true;
        }
        let small_in_list = filter_has_small_indexed_in_list(filter, table, leading_column_id);
        if index.unique && index.key_columns.len() == 1 && small_in_list {
            return true;
        }
        let small_or_chain = filter_has_small_indexed_or_chain(filter, table, leading_column_id);
        if index.unique && index.key_columns.len() == 1 && small_or_chain {
            return true;
        }
        if filter_has_selective_bitmap_or(filter, table, index, stats) {
            return true;
        }
        if filter_has_selective_composite_prefix_bitmap_or(filter, table, index, stats) {
            return true;
        }
        if filter_has_selective_composite_disjunct_bitmap_or(filter, table, index, stats) {
            return true;
        }
        if filter_has_selective_composite_prefix_path(filter, table, index, stats) {
            return true;
        }
        if index_leading_key_not_known_low_distinct(index, stats) {
            if filter_has_composite_equality_prefix(filter, table, index) {
                return true;
            }
            if filter_has_composite_bitmap_or(filter, table, index) {
                return true;
            }
            if filter_has_composite_equality_prefix_with_bounded_range(filter, table, index) {
                return true;
            }
            if filter_has_composite_equality_prefix_with_in_list(filter, table, index) {
                return true;
            }
            if filter_has_composite_equality_prefix_with_or_chain(filter, table, index) {
                return true;
            }
        }
        if !column_appears_high_distinct(leading_column_id, stats) {
            return false;
        }
        extract_index_lookup_value(filter, table, leading_column_id).is_some()
            || small_in_list
            || small_or_chain
            || extract_index_range(filter, table, leading_column_id).is_some_and(|range| {
                !range.is_empty()
                    && !matches!(range.lower, std::ops::Bound::Unbounded)
                    && !matches!(range.upper, std::ops::Bound::Unbounded)
            })
    })
}

fn filter_has_selective_bitmap_and(
    filter: &TypedExpr,
    table: &TableDescriptor,
    indexes: &[IndexDescriptor],
    vector_column_id: ColumnId,
    stats: Option<&TableStatistics>,
) -> bool {
    struct BitmapAndCandidate {
        index_id: aiondb_core::IndexId,
        columns: Vec<ColumnId>,
        selectivity: f64,
    }

    let mut candidates = Vec::new();
    for index in indexes {
        if index.kind != IndexKind::BTree
            || index.key_columns.is_empty()
            || index
                .key_columns
                .iter()
                .any(|column| column.column_id == vector_column_id)
        {
            continue;
        }
        let Some(path) = extract_index_prefix_access_path(filter, table, index) else {
            continue;
        };
        let Some(columns) = bitmap_and_candidate_columns(&path, index) else {
            continue;
        };
        let Some(selectivity) = bitmap_and_candidate_selectivity(&path, index, stats) else {
            continue;
        };
        candidates.push(BitmapAndCandidate {
            index_id: index.index_id,
            columns,
            selectivity,
        });
    }

    if candidates.len() < 2 {
        return false;
    }

    let candidates_are_disjoint = |selected: &[usize], candidate_idx: usize| {
        let candidate = &candidates[candidate_idx];
        selected.iter().all(|selected_idx| {
            let selected = &candidates[*selected_idx];
            selected.index_id != candidate.index_id
                && !selected
                    .columns
                    .iter()
                    .any(|column| candidate.columns.contains(column))
        })
    };
    let subset_selectivity = |selected: &[usize]| {
        selected
            .iter()
            .map(|idx| candidates[*idx].selectivity)
            .product::<f64>()
            .clamp(1.0e-6, 1.0)
    };

    let mut best_selectivity: Option<f64> = None;
    for left_idx in 0..candidates.len() {
        for right_idx in (left_idx + 1)..candidates.len() {
            if !candidates_are_disjoint(&[left_idx], right_idx) {
                continue;
            }
            let mut selected = vec![left_idx, right_idx];
            let mut selected_selectivity = subset_selectivity(&selected);

            loop {
                let mut best_extension: Option<(usize, f64)> = None;
                for candidate_idx in 0..candidates.len() {
                    if selected.contains(&candidate_idx)
                        || !candidates_are_disjoint(&selected, candidate_idx)
                    {
                        continue;
                    }
                    let mut extended = selected.clone();
                    extended.push(candidate_idx);
                    let extended_selectivity = subset_selectivity(&extended);
                    if extended_selectivity < selected_selectivity
                        && best_extension
                            .as_ref()
                            .map_or(true, |(_, best)| extended_selectivity < *best)
                    {
                        best_extension = Some((candidate_idx, extended_selectivity));
                    }
                }
                let Some((candidate_idx, extension_selectivity)) = best_extension else {
                    break;
                };
                selected.push(candidate_idx);
                selected_selectivity = extension_selectivity;
            }

            if best_selectivity.map_or(true, |best| selected_selectivity < best) {
                best_selectivity = Some(selected_selectivity);
            }
        }
    }

    best_selectivity.is_some_and(|selectivity| selectivity <= SELECTIVE_BITMAP_AND_SELECTIVITY)
}

fn filter_has_selective_bitmap_or(
    filter: &TypedExpr,
    table: &TableDescriptor,
    index: &IndexDescriptor,
    stats: Option<&TableStatistics>,
) -> bool {
    if index.kind != IndexKind::BTree || index.key_columns.is_empty() {
        return false;
    }
    let column_id = index.key_columns[0].column_id;
    let Some(values) = extract_small_in_list_values(filter, table, column_id)
        .or_else(|| extract_small_or_chain_values(filter, table, column_id))
    else {
        return false;
    };
    let mut unique_values = Vec::with_capacity(values.len());
    for value in values {
        if !unique_values.contains(&value) {
            unique_values.push(value);
        }
    }
    if unique_values.is_empty() || unique_values.len() > MAX_BITMAP_OR_LITERAL_COUNT {
        return false;
    }

    let Some(equality_selectivity) = column_stats_equality_selectivity(stats, column_id) else {
        return false;
    };
    let mut child_selectivity = equality_selectivity;
    for column in index.key_columns.iter().skip(1) {
        if extract_index_lookup_value(filter, table, column.column_id).is_some() {
            let Some(suffix_selectivity) =
                column_stats_equality_selectivity(stats, column.column_id)
            else {
                return false;
            };
            child_selectivity *= suffix_selectivity;
            continue;
        }
        if let Some(range) = extract_index_range(filter, table, column.column_id) {
            if range.is_empty() || range.is_unbounded() {
                return false;
            }
            child_selectivity *=
                column_range_selectivity(stats, column.column_id, &range.lower, &range.upper);
        }
        break;
    }
    let combined_selectivity = (unique_values.len() as f64 * child_selectivity).clamp(0.0, 1.0);
    combined_selectivity <= SELECTIVE_BITMAP_OR_SELECTIVITY
}

fn filter_has_selective_composite_prefix_bitmap_or(
    filter: &TypedExpr,
    table: &TableDescriptor,
    index: &IndexDescriptor,
    stats: Option<&TableStatistics>,
) -> bool {
    if index.kind != IndexKind::BTree || index.key_columns.len() < 2 {
        return false;
    }

    let mut prefix_selectivity = 1.0;
    let mut equality_prefix_len = 0;
    for (key_position, column) in index.key_columns.iter().enumerate() {
        if extract_index_lookup_value(filter, table, column.column_id).is_some() {
            let Some(selectivity) = column_stats_equality_selectivity(stats, column.column_id)
            else {
                return false;
            };
            prefix_selectivity *= selectivity;
            equality_prefix_len += 1;
            continue;
        }

        if equality_prefix_len == 0 {
            return false;
        }
        let Some(values) = extract_small_in_list_values(filter, table, column.column_id)
            .or_else(|| extract_small_or_chain_values(filter, table, column.column_id))
        else {
            return false;
        };
        let mut unique_values = Vec::with_capacity(values.len());
        for value in values {
            if !unique_values.contains(&value) {
                unique_values.push(value);
            }
        }
        if unique_values.is_empty() || unique_values.len() > MAX_BITMAP_OR_LITERAL_COUNT {
            return false;
        }

        let Some(in_selectivity) = column_stats_equality_selectivity(stats, column.column_id)
        else {
            return false;
        };
        let mut child_selectivity = prefix_selectivity * in_selectivity;
        for suffix in index.key_columns.iter().skip(key_position + 1) {
            if extract_index_lookup_value(filter, table, suffix.column_id).is_some() {
                let Some(suffix_selectivity) =
                    column_stats_equality_selectivity(stats, suffix.column_id)
                else {
                    return false;
                };
                child_selectivity *= suffix_selectivity;
                continue;
            }
            if let Some(range) = extract_index_range(filter, table, suffix.column_id) {
                if range.is_empty() || range.is_unbounded() {
                    return false;
                }
                child_selectivity *=
                    column_range_selectivity(stats, suffix.column_id, &range.lower, &range.upper);
            }
            break;
        }
        let combined_selectivity = (unique_values.len() as f64 * child_selectivity).clamp(0.0, 1.0);
        return combined_selectivity <= SELECTIVE_BITMAP_OR_SELECTIVITY;
    }
    false
}

fn filter_has_selective_composite_prefix_path(
    filter: &TypedExpr,
    table: &TableDescriptor,
    index: &IndexDescriptor,
    stats: Option<&TableStatistics>,
) -> bool {
    if index.kind != IndexKind::BTree || index.key_columns.len() < 2 {
        return false;
    }
    let Some(path) = extract_index_prefix_access_path(filter, table, index) else {
        return false;
    };
    match &path {
        ScanAccessPath::IndexEqComposite { values, .. } if values.len() >= 2 => {}
        ScanAccessPath::IndexEqRangeComposite { eq_values, .. } if !eq_values.is_empty() => {}
        _ => return false,
    }
    bitmap_and_candidate_selectivity(&path, index, stats)
        .is_some_and(|selectivity| selectivity <= SELECTIVE_BITMAP_AND_SELECTIVITY)
}

fn bitmap_and_candidate_columns(
    path: &ScanAccessPath,
    index: &IndexDescriptor,
) -> Option<Vec<ColumnId>> {
    let constrained_len = match path {
        ScanAccessPath::IndexEq { .. } | ScanAccessPath::IndexRange { .. } => 1,
        ScanAccessPath::IndexEqComposite { values, .. } => values.len(),
        ScanAccessPath::IndexEqRangeComposite { eq_values, .. } => eq_values.len() + 1,
        _ => return None,
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

fn bitmap_and_candidate_selectivity(
    path: &ScanAccessPath,
    index: &IndexDescriptor,
    stats: Option<&TableStatistics>,
) -> Option<f64> {
    match path {
        ScanAccessPath::IndexEq { value, .. } => {
            let column = index.key_columns.first()?;
            Some(column_equality_selectivity(
                stats,
                column.column_id,
                Some(value),
            ))
        }
        ScanAccessPath::IndexEqComposite { values, .. } => {
            Some(composite_equality_selectivity(stats, index, values))
        }
        ScanAccessPath::IndexRange { lower, upper, .. } => {
            let column = index.key_columns.first()?;
            Some(column_range_selectivity(
                stats,
                column.column_id,
                lower,
                upper,
            ))
        }
        ScanAccessPath::IndexEqRangeComposite {
            eq_values,
            lower,
            upper,
            ..
        } => {
            let range_column = index.key_columns.get(eq_values.len())?;
            Some(
                composite_equality_selectivity(stats, index, eq_values)
                    * column_range_selectivity(stats, range_column.column_id, lower, upper),
            )
        }
        _ => None,
    }
}

fn composite_equality_selectivity(
    stats: Option<&TableStatistics>,
    index: &IndexDescriptor,
    values: &[Value],
) -> f64 {
    values
        .iter()
        .zip(&index.key_columns)
        .map(|(value, column)| column_equality_selectivity(stats, column.column_id, Some(value)))
        .product::<f64>()
        .clamp(1.0e-6, 1.0)
}

fn column_equality_selectivity(
    stats: Option<&TableStatistics>,
    column_id: ColumnId,
    value: Option<&Value>,
) -> f64 {
    if matches!(value, Some(Value::Null)) {
        return 1.0;
    }
    stats
        .and_then(|stats| {
            stats
                .column_stats
                .iter()
                .find(|column| column.column_id == column_id)
        })
        .map(|column| {
            if column.ndistinct.is_finite() && column.ndistinct > 0.0 {
                ((1.0 - column.null_fraction.clamp(0.0, 1.0)) / column.ndistinct).clamp(1.0e-6, 1.0)
            } else {
                DEFAULT_BITMAP_AND_EQ_SELECTIVITY
            }
        })
        .unwrap_or(DEFAULT_BITMAP_AND_EQ_SELECTIVITY)
}

fn column_stats_equality_selectivity(
    stats: Option<&TableStatistics>,
    column_id: ColumnId,
) -> Option<f64> {
    let column = stats?
        .column_stats
        .iter()
        .find(|column| column.column_id == column_id)?;
    if !column.ndistinct.is_finite() || column.ndistinct <= 0.0 {
        return None;
    }
    Some(((1.0 - column.null_fraction.clamp(0.0, 1.0)) / column.ndistinct).clamp(1.0e-6, 1.0))
}

fn range_shape_selectivity(lower: &std::ops::Bound<Value>, upper: &std::ops::Bound<Value>) -> f64 {
    if matches!(lower, std::ops::Bound::Unbounded) || matches!(upper, std::ops::Bound::Unbounded) {
        DEFAULT_BITMAP_AND_RANGE_SELECTIVITY
    } else {
        DEFAULT_BITMAP_AND_BOUNDED_RANGE_SELECTIVITY
    }
}

fn column_range_selectivity(
    stats: Option<&TableStatistics>,
    column_id: ColumnId,
    lower: &std::ops::Bound<Value>,
    upper: &std::ops::Bound<Value>,
) -> f64 {
    let fallback = range_shape_selectivity(lower, upper);
    let Some(column) = stats.and_then(|stats| {
        stats
            .column_stats
            .iter()
            .find(|column| column.column_id == column_id)
    }) else {
        return fallback;
    };
    if !column.ndistinct.is_finite() || column.ndistinct <= 0.0 {
        return fallback;
    }
    let Some(span) = integer_range_span(lower, upper) else {
        return fallback;
    };
    let non_null = 1.0 - column.null_fraction.clamp(0.0, 1.0);
    ((span / column.ndistinct) * non_null).clamp(1.0e-6, fallback)
}

fn integer_range_span(
    lower: &std::ops::Bound<Value>,
    upper: &std::ops::Bound<Value>,
) -> Option<f64> {
    use std::ops::Bound;

    let lower_value = match lower {
        Bound::Included(value) => i128::from(integer_bound_value(value)?),
        Bound::Excluded(value) => i128::from(integer_bound_value(value)?).checked_add(1)?,
        Bound::Unbounded => return None,
    };
    let upper_value = match upper {
        Bound::Included(value) => i128::from(integer_bound_value(value)?),
        Bound::Excluded(value) => i128::from(integer_bound_value(value)?).checked_sub(1)?,
        Bound::Unbounded => return None,
    };
    if upper_value < lower_value {
        return None;
    }
    Some((upper_value - lower_value + 1) as f64)
}

fn integer_bound_value(value: &Value) -> Option<i64> {
    match value {
        Value::Int(value) => Some(i64::from(*value)),
        Value::BigInt(value) => Some(*value),
        _ => None,
    }
}

fn filter_has_composite_equality_prefix(
    filter: &TypedExpr,
    table: &TableDescriptor,
    index: &IndexDescriptor,
) -> bool {
    if index.key_columns.len() < 2 {
        return false;
    }
    index
        .key_columns
        .iter()
        .take_while(|column| extract_index_lookup_value(filter, table, column.column_id).is_some())
        .count()
        >= 2
}

fn filter_has_composite_bitmap_or(
    filter: &TypedExpr,
    table: &TableDescriptor,
    index: &IndexDescriptor,
) -> bool {
    const MAX_OR_CHAIN_BITMAP_OR_LEN: usize = 64;
    if index.key_columns.len() < 2 {
        return false;
    }

    let mut disjuncts = Vec::new();
    collect_or_disjuncts(filter, &mut disjuncts);
    if disjuncts.len() < 2 || disjuncts.len() > MAX_OR_CHAIN_BITMAP_OR_LEN {
        return false;
    }

    disjuncts.iter().all(|disjunct| {
        match extract_index_prefix_access_path(disjunct, table, index) {
            Some(ScanAccessPath::IndexEqComposite { values, .. }) => values.len() >= 2,
            Some(ScanAccessPath::IndexEqRangeComposite { eq_values, .. }) => !eq_values.is_empty(),
            _ => false,
        }
    })
}

fn filter_has_selective_composite_disjunct_bitmap_or(
    filter: &TypedExpr,
    table: &TableDescriptor,
    index: &IndexDescriptor,
    stats: Option<&TableStatistics>,
) -> bool {
    const MAX_OR_CHAIN_BITMAP_OR_LEN: usize = 64;
    if index.kind != IndexKind::BTree || index.key_columns.len() < 2 {
        return false;
    }

    let mut disjuncts = Vec::new();
    collect_or_disjuncts(filter, &mut disjuncts);
    if disjuncts.len() < 2 {
        return false;
    }

    let mut unique_paths: Vec<ScanAccessPath> = Vec::new();
    let mut combined_selectivity = 0.0;
    for disjunct in disjuncts {
        let Some(path) = extract_index_prefix_access_path(disjunct, table, index) else {
            return false;
        };
        if unique_paths.contains(&path) {
            continue;
        }
        if unique_paths.len() >= MAX_OR_CHAIN_BITMAP_OR_LEN {
            return false;
        }
        let Some(selectivity) = strict_composite_bitmap_or_child_selectivity(&path, index, stats)
        else {
            return false;
        };
        unique_paths.push(path);
        combined_selectivity += selectivity;
    }
    if unique_paths.is_empty() {
        return false;
    }

    combined_selectivity.clamp(0.0, 1.0) <= SELECTIVE_BITMAP_OR_SELECTIVITY
}

fn strict_composite_bitmap_or_child_selectivity(
    path: &ScanAccessPath,
    index: &IndexDescriptor,
    stats: Option<&TableStatistics>,
) -> Option<f64> {
    match path {
        ScanAccessPath::IndexEqComposite { values, .. } if values.len() >= 2 => {
            strict_composite_equality_selectivity(stats, index, values)
        }
        ScanAccessPath::IndexEqRangeComposite {
            eq_values,
            lower,
            upper,
            ..
        } if !eq_values.is_empty() => {
            let range_column = index.key_columns.get(eq_values.len())?;
            strict_composite_equality_selectivity(stats, index, eq_values).map(|selectivity| {
                selectivity * column_range_selectivity(stats, range_column.column_id, lower, upper)
            })
        }
        _ => None,
    }
}

fn strict_composite_equality_selectivity(
    stats: Option<&TableStatistics>,
    index: &IndexDescriptor,
    values: &[Value],
) -> Option<f64> {
    values
        .iter()
        .zip(&index.key_columns)
        .map(|(_, column)| column_stats_equality_selectivity(stats, column.column_id))
        .try_fold(1.0, |acc, selectivity| {
            selectivity.map(|selectivity| acc * selectivity)
        })
        .map(|selectivity| selectivity.clamp(1.0e-6, 1.0))
}

fn filter_has_composite_equality_prefix_with_or_chain(
    filter: &TypedExpr,
    table: &TableDescriptor,
    index: &IndexDescriptor,
) -> bool {
    if index.key_columns.len() < 2 {
        return false;
    }
    let mut equality_prefix_len = 0;
    for column in &index.key_columns {
        if extract_index_lookup_value(filter, table, column.column_id).is_some() {
            equality_prefix_len += 1;
            continue;
        }
        return equality_prefix_len > 0
            && extract_small_or_chain_values(filter, table, column.column_id)
                .is_some_and(|values| !values.is_empty() && values.len() <= 64);
    }
    false
}

fn filter_has_composite_equality_prefix_with_in_list(
    filter: &TypedExpr,
    table: &TableDescriptor,
    index: &IndexDescriptor,
) -> bool {
    if index.key_columns.len() < 2 {
        return false;
    }
    let mut equality_prefix_len = 0;
    for column in &index.key_columns {
        if extract_index_lookup_value(filter, table, column.column_id).is_some() {
            equality_prefix_len += 1;
            continue;
        }
        return equality_prefix_len > 0
            && extract_small_in_list_values(filter, table, column.column_id)
                .is_some_and(|values| !values.is_empty() && values.len() <= 64);
    }
    false
}

fn filter_has_composite_equality_prefix_with_bounded_range(
    filter: &TypedExpr,
    table: &TableDescriptor,
    index: &IndexDescriptor,
) -> bool {
    if index.key_columns.len() < 2 {
        return false;
    }
    let mut equality_prefix_len = 0;
    for column in &index.key_columns {
        if extract_index_lookup_value(filter, table, column.column_id).is_some() {
            equality_prefix_len += 1;
            continue;
        }
        return equality_prefix_len > 0
            && extract_index_range(filter, table, column.column_id).is_some_and(|range| {
                !range.is_empty()
                    && !matches!(range.lower, std::ops::Bound::Unbounded)
                    && !matches!(range.upper, std::ops::Bound::Unbounded)
            });
    }
    false
}

fn filter_has_small_indexed_in_list(
    filter: &TypedExpr,
    table: &TableDescriptor,
    column_id: ColumnId,
) -> bool {
    extract_small_in_list_values(filter, table, column_id)
        .is_some_and(|values| !values.is_empty() && values.len() <= MAX_BITMAP_OR_LITERAL_COUNT)
}

fn filter_has_small_indexed_or_chain(
    filter: &TypedExpr,
    table: &TableDescriptor,
    column_id: ColumnId,
) -> bool {
    extract_small_or_chain_values(filter, table, column_id)
        .is_some_and(|values| !values.is_empty() && values.len() <= MAX_BITMAP_OR_LITERAL_COUNT)
}

fn collect_or_disjuncts<'a>(expr: &'a TypedExpr, out: &mut Vec<&'a TypedExpr>) {
    match &expr.kind {
        TypedExprKind::LogicalOr { left, right } => {
            collect_or_disjuncts(left, out);
            collect_or_disjuncts(right, out);
        }
        _ => out.push(expr),
    }
}

fn column_appears_high_distinct(column_id: ColumnId, stats: Option<&TableStatistics>) -> bool {
    let Some(stats) = stats else {
        return false;
    };
    if stats.row_count <= 1 {
        return true;
    }
    let min_distinct = (crate::u64_to_f64(stats.row_count) * 0.5).max(1.0);
    stats
        .column_stats
        .iter()
        .find(|column| column.column_id == column_id)
        .is_some_and(|column| column.ndistinct >= min_distinct)
}

fn index_leading_key_not_known_low_distinct(
    index: &IndexDescriptor,
    stats: Option<&TableStatistics>,
) -> bool {
    if index.unique {
        return true;
    }
    let Some(stats) = stats else {
        return true;
    };
    if stats.row_count <= 1 {
        return true;
    }
    let Some(leading) = index.key_columns.first() else {
        return false;
    };
    let Some(column) = stats
        .column_stats
        .iter()
        .find(|column| column.column_id == leading.column_id)
    else {
        return true;
    };
    let min_distinct = (crate::u64_to_f64(stats.row_count) * 0.5).max(1.0);
    column.ndistinct >= min_distinct
}

fn parse_positive_limit(limit: &TypedExpr) -> Option<usize> {
    match &limit.kind {
        TypedExprKind::Literal(Value::Int(value)) if *value > 0 => {
            usize::try_from(u64::try_from(*value).ok()?).ok()
        }
        TypedExprKind::Literal(Value::BigInt(value)) if *value > 0 => {
            usize::try_from(u64::try_from(*value).ok()?).ok()
        }
        _ => None,
    }
}

fn parse_nonnegative_offset(offset: &Option<TypedExpr>) -> Option<usize> {
    let Some(offset_expr) = offset.as_ref() else {
        return Some(0);
    };
    match &offset_expr.kind {
        TypedExprKind::Literal(Value::Int(value)) if *value >= 0 => {
            usize::try_from(u64::try_from(*value).ok()?).ok()
        }
        TypedExprKind::Literal(Value::BigInt(value)) if *value >= 0 => {
            usize::try_from(u64::try_from(*value).ok()?).ok()
        }
        _ => None,
    }
}

fn hnsw_candidate_limit(limit: usize, has_filter: bool) -> usize {
    if !has_filter {
        return limit;
    }
    limit
        .saturating_mul(HNSW_FILTER_OVERSAMPLE_FACTOR)
        .max(HNSW_FILTER_MIN_CANDIDATES)
        .min(VECTOR_MAX_K)
}

fn extract_distance_call(
    expr: &TypedExpr,
    catalog: &dyn CatalogReader,
    txn_id: TxnId,
    table_id: RelationId,
) -> DbResult<Option<(ColumnId, Vec<f32>, VectorDistanceMetric, bool)>> {
    let TypedExprKind::ScalarFunction { func, args } = &expr.kind else {
        return Ok(None);
    };
    // Only dispatch to HnswScan for vector-order functions with a matching
    // HNSW metric and sort direction.
    let Some(order_spec) = distance_function_order_spec(func) else {
        return Ok(None);
    };
    if args.len() != 2 {
        return Ok(None);
    }

    let Some(table) = catalog.get_table_by_id(txn_id, table_id)? else {
        return Ok(None);
    };

    if let Some((col, vec)) = try_extract_col_and_vector(&args[0], &args[1], &table)? {
        return Ok(Some((
            col,
            vec,
            order_spec.metric,
            order_spec.requires_descending,
        )));
    }
    if let Some((col, vec)) = try_extract_col_and_vector(&args[1], &args[0], &table)? {
        return Ok(Some((
            col,
            vec,
            order_spec.metric,
            order_spec.requires_descending,
        )));
    }
    Ok(None)
}

fn try_extract_col_and_vector(
    col_expr: &TypedExpr,
    vec_expr: &TypedExpr,
    table: &TableDescriptor,
) -> DbResult<Option<(ColumnId, Vec<f32>)>> {
    let Some(ordinal) = extract_vector_column_ordinal(col_expr) else {
        return Ok(None);
    };
    let Some(column) = table.columns.get(ordinal) else {
        return Ok(None);
    };

    let Some(vector) = try_extract_query_vector_literal(vec_expr)? else {
        return Ok(None);
    };
    let DataType::Vector { dims, .. } = column.data_type else {
        return Ok(None);
    };
    if vector.values.iter().any(|value| !value.is_finite()) {
        return Err(DbError::internal(
            "vector search query contains non-finite values",
        ));
    }
    if vector.dims != dims {
        return Err(DbError::internal(format!(
            "vector dimension mismatch: {} vs {}",
            dims, vector.dims
        )));
    }

    Ok(Some((column.column_id, vector.values)))
}

fn extract_vector_column_ordinal(expr: &TypedExpr) -> Option<usize> {
    match &expr.kind {
        TypedExprKind::ColumnRef { ordinal, .. } => Some(*ordinal),
        TypedExprKind::Cast { expr, target_type } => match target_type {
            DataType::Vector { .. } => extract_vector_column_ordinal(expr),
            _ => None,
        },
        _ => None,
    }
}

fn try_extract_query_vector_literal(expr: &TypedExpr) -> DbResult<Option<VectorValue>> {
    match &expr.kind {
        TypedExprKind::Literal(Value::Vector(vector)) => Ok(Some(vector.clone())),
        TypedExprKind::Cast { expr, target_type } => {
            let DataType::Vector { dims, .. } = target_type else {
                return Ok(None);
            };
            let inner_vector = match &expr.kind {
                TypedExprKind::Literal(Value::Text(text)) => {
                    Some(VectorValue::parse(text).ok_or_else(|| {
                        DbError::internal(
                            "vector search query literal could not be parsed as VECTOR",
                        )
                    })?)
                }
                _ => try_extract_query_vector_literal(expr)?,
            };
            let Some(vector) = inner_vector else {
                return Ok(None);
            };
            if vector.dims != *dims {
                return Err(DbError::internal(format!(
                    "vector dimension mismatch: {} vs {}",
                    dims, vector.dims
                )));
            }
            Ok(Some(vector))
        }
        _ => Ok(None),
    }
}

fn extract_projected_ordinals(
    outputs: &[ProjectionExpr],
    table: &TableDescriptor,
) -> Option<Vec<usize>> {
    outputs
        .iter()
        .map(|output| match &output.expr.kind {
            TypedExprKind::ColumnRef { ordinal, .. } if *ordinal < table.columns.len() => {
                Some(*ordinal)
            }
            _ => None,
        })
        .collect()
}

fn full_table_projection(table: &TableDescriptor) -> (Vec<usize>, Vec<ResultField>) {
    let projected_ordinals = (0..table.columns.len()).collect::<Vec<_>>();
    let output_fields = table
        .columns
        .iter()
        .map(|column| ResultField {
            name: column.name.clone(),
            data_type: column.data_type.clone(),
            text_type_modifier: column.text_type_modifier.clone(),
            nullable: column.nullable,
        })
        .collect::<Vec<_>>();
    (projected_ordinals, output_fields)
}

fn hnsw_wrapper_source_projection(
    table: &TableDescriptor,
    outputs: &[ProjectionExpr],
    filter: Option<&TypedExpr>,
) -> (Vec<usize>, Vec<ResultField>) {
    let mut max_ordinal = None;
    for output in outputs {
        collect_max_column_ref_ordinal(&output.expr, &mut max_ordinal);
    }
    if let Some(filter) = filter {
        collect_max_column_ref_ordinal(filter, &mut max_ordinal);
    }
    let Some(max_ordinal) = max_ordinal else {
        return full_table_projection(table);
    };
    let Some(end) = max_ordinal.checked_add(1) else {
        return full_table_projection(table);
    };
    if end > table.columns.len() {
        return full_table_projection(table);
    }
    let projected_ordinals = (0..end).collect::<Vec<_>>();
    let output_fields = table
        .columns
        .iter()
        .take(end)
        .map(|column| ResultField {
            name: column.name.clone(),
            data_type: column.data_type.clone(),
            text_type_modifier: column.text_type_modifier.clone(),
            nullable: column.nullable,
        })
        .collect::<Vec<_>>();
    (projected_ordinals, output_fields)
}

fn collect_max_column_ref_ordinal(expr: &TypedExpr, max_ordinal: &mut Option<usize>) {
    let mut stack = vec![expr];
    while let Some(expr) = stack.pop() {
        match &expr.kind {
            TypedExprKind::ColumnRef { ordinal, .. } => {
                *max_ordinal = Some(max_ordinal.map_or(*ordinal, |current| current.max(*ordinal)));
            }
            _ => {
                predicate_pushdown::for_each_child_expr(expr, &mut |child| {
                    stack.push(child);
                });
            }
        }
    }
}
