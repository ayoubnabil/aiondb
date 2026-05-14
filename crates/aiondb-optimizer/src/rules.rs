use aiondb_catalog::{CatalogReader, IndexKind, TableDescriptor, VectorDistanceMetric};
use aiondb_core::{
    bounded_hnsw_ef_search, ColumnId, DataType, DbError, DbResult, RelationId, TxnId, Value,
    VectorValue, HNSW_MAX_EF_SEARCH, VECTOR_MAX_K,
};
use aiondb_plan::{
    PhysicalPlan, ProjectionExpr, ResultField, ScalarFunction, SortExpr, TypedExpr, TypedExprKind,
};

use crate::{predicate_pushdown, Optimizer};

const HNSW_FILTER_OVERSAMPLE_FACTOR: usize = 4;
const HNSW_FILTER_MIN_CANDIDATES: usize = 64;

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

    Ok(Some((column.column_id, vector.values.clone())))
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
