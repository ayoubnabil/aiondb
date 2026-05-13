// Helpers only; consumer test modules were removed during the
// per-area split. Kept for re-use by future split modules; suppress
// dead-code lints in the meantime.
#![allow(dead_code, unused_imports)]

use super::*;

use aiondb_plan::{JoinType, ScalarFunction};

use crate::physical_builder::estimate_plan_rows;

// -------------------------------------------------------------------
// Helper: build a multi-table catalog for join tests
// -------------------------------------------------------------------

/// A catalog that knows about multiple tables, each with a configurable
/// number of columns and optional statistics.
#[derive(Debug)]
pub(super) struct MultiTableCatalog {
    tables: Vec<TableDescriptor>,
    statistics: Vec<Option<TableStatistics>>,
}

impl MultiTableCatalog {
    /// Create a catalog with `n` tables, each having `cols_per_table` columns.
    /// All tables get a row count of `row_count`.
    pub(super) fn new(n: usize, cols_per_table: usize, row_count: u64) -> Self {
        let mut tables = Vec::new();
        let mut statistics = Vec::new();
        for i in 0..n {
            let table_id = RelationId::new((i + 1) as u64);
            let schema_id = SchemaId::new(1);
            let name = QualifiedName::qualified("public", format!("t{}", i + 1));
            let columns: Vec<ColumnDescriptor> = (0..cols_per_table)
                .map(|c| ColumnDescriptor {
                    column_id: ColumnId::new((i * 100 + c + 1) as u64),
                    name: format!("col_{c}"),
                    data_type: DataType::Int,
                    raw_type_name: None,
                    text_type_modifier: None,
                    nullable: false,
                    ordinal_position: c as u32,
                    default_value: None,
                })
                .collect();
            tables.push(TableDescriptor {
                table_id,
                schema_id,
                name,
                columns,
                identity_columns: Vec::new(),
                primary_key: None,
                foreign_keys: Vec::new(),
                check_constraints: Vec::new(),
                shard_config: None,
                owner: None,
            });
            statistics.push(Some(TableStatistics {
                table_id,
                row_count,
                total_bytes: row_count * 64,
                dead_row_count: 0,
                last_updated_by: None,
                column_stats: Vec::new(),
            }));
        }
        Self { tables, statistics }
    }
}

impl CatalogReader for MultiTableCatalog {
    fn get_schema(
        &self,
        _txn: TxnId,
        _name: &QualifiedName,
    ) -> DbResult<Option<aiondb_catalog::SchemaDescriptor>> {
        Ok(None)
    }

    fn get_table(&self, _txn: TxnId, name: &QualifiedName) -> DbResult<Option<TableDescriptor>> {
        Ok(self.tables.iter().find(|t| t.name == *name).cloned())
    }

    fn get_table_by_id(
        &self,
        _txn: TxnId,
        table_id: RelationId,
    ) -> DbResult<Option<TableDescriptor>> {
        Ok(self.tables.iter().find(|t| t.table_id == table_id).cloned())
    }

    fn list_tables(&self, _txn: TxnId, _schema_id: SchemaId) -> DbResult<Vec<TableDescriptor>> {
        Ok(self.tables.clone())
    }

    fn list_indexes(&self, _txn: TxnId, _table_id: RelationId) -> DbResult<Vec<IndexDescriptor>> {
        Ok(Vec::new())
    }

    fn get_index(&self, _txn: TxnId, _index_id: IndexId) -> DbResult<Option<IndexDescriptor>> {
        Ok(None)
    }

    fn get_sequence(
        &self,
        _txn: TxnId,
        _name: &QualifiedName,
    ) -> DbResult<Option<aiondb_catalog::SequenceDescriptor>> {
        Ok(None)
    }

    fn get_statistics(
        &self,
        _txn: TxnId,
        table_id: RelationId,
    ) -> DbResult<Option<TableStatistics>> {
        Ok(self
            .statistics
            .iter()
            .find(|s| s.as_ref().map(|st| st.table_id) == Some(table_id))
            .cloned()
            .flatten())
    }

    fn get_view(
        &self,
        _txn: TxnId,
        _name: &QualifiedName,
    ) -> DbResult<Option<aiondb_catalog::ViewDescriptor>> {
        Ok(None)
    }

    fn list_views(
        &self,
        _txn: TxnId,
        _schema_id: SchemaId,
    ) -> DbResult<Vec<aiondb_catalog::ViewDescriptor>> {
        Ok(Vec::new())
    }
}

// -------------------------------------------------------------------
// Helper: build a two-table NestedLoopJoin with an equi-join condition
// -------------------------------------------------------------------

/// Build a ProjectTable leaf with explicit output columns so the
/// physical plan has a known width.
fn make_scan_leaf(table_id: RelationId, col_names: &[&str]) -> LogicalPlan {
    let outputs = col_names
        .iter()
        .map(|name| make_projection(name, DataType::Int))
        .collect();
    LogicalPlan::ProjectTable {
        table_id,
        outputs,
        filter: None,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
    }
}

fn make_typed_scan_leaf(table_id: RelationId, columns: &[(&str, DataType)]) -> LogicalPlan {
    let outputs = columns
        .iter()
        .enumerate()
        .map(|(ordinal, (name, data_type))| ProjectionExpr {
            field: ResultField {
                name: (*name).to_owned(),
                data_type: data_type.clone(),
                text_type_modifier: None,
                nullable: false,
            },
            expr: TypedExpr::column_ref(*name, ordinal, data_type.clone(), false),
        })
        .collect();
    LogicalPlan::ProjectTable {
        table_id,
        outputs,
        filter: None,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
    }
}

fn make_two_table_equi_join(
    left_table_id: RelationId,
    right_table_id: RelationId,
    left_ordinal: usize,
    right_ordinal: usize,
) -> LogicalPlan {
    // Condition: left.col = right.col (equi-join)
    let condition = TypedExpr::binary_eq(
        TypedExpr::column_ref("col_0", left_ordinal, DataType::Int, false),
        TypedExpr::column_ref("col_0", right_ordinal, DataType::Int, false),
    );
    LogicalPlan::NestedLoopJoin {
        left: Box::new(make_scan_leaf(left_table_id, &["col_0", "col_1"])),
        right: Box::new(make_scan_leaf(right_table_id, &["col_0", "col_1"])),
        join_type: JoinType::Inner,
        condition: Some(condition),
        outputs: vec![make_projection("result", DataType::Int)],
        filter: None,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
    }
}

fn make_hybrid_leaf(function_name: &str, estimated_rows: i32, output_name: &str) -> LogicalPlan {
    LogicalPlan::ProjectSource {
        source: Box::new(LogicalPlan::HybridFunctionScan {
            function_name: function_name.to_owned(),
            args: if function_name.eq_ignore_ascii_case("vector_top_k_ids") {
                vec![
                    TypedExpr::literal(Value::Text("docs".to_owned()), DataType::Text, false),
                    TypedExpr::literal(Value::Text("embedding".to_owned()), DataType::Text, false),
                    TypedExpr::literal(Value::Text("[1.0,0.0]".to_owned()), DataType::Text, false),
                    TypedExpr::literal(Value::Int(estimated_rows), DataType::Int, false),
                ]
            } else {
                vec![
                    TypedExpr::literal(
                        Value::Text("related_doc".to_owned()),
                        DataType::Text,
                        false,
                    ),
                    TypedExpr::literal(Value::Int(42), DataType::Int, false),
                ]
            },
            output_fields: vec![ResultField {
                name: output_name.to_owned(),
                data_type: DataType::Int,
                text_type_modifier: None,
                nullable: false,
            }],
        }),
        outputs: vec![ProjectionExpr {
            field: ResultField {
                name: output_name.to_owned(),
                data_type: DataType::Int,
                text_type_modifier: None,
                nullable: false,
            },
            expr: TypedExpr::column_ref(output_name, 0, DataType::Int, false),
        }],
        filter: None,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
    }
}

fn make_transparent_nested_hybrid_subquery(
    function_name: &str,
    estimated_rows: i32,
    output_name: &str,
) -> LogicalPlan {
    LogicalPlan::ProjectSource {
        source: Box::new(make_hybrid_leaf(function_name, estimated_rows, output_name)),
        outputs: vec![ProjectionExpr {
            field: ResultField {
                name: output_name.to_owned(),
                data_type: DataType::Int,
                text_type_modifier: None,
                nullable: false,
            },
            expr: TypedExpr::column_ref(output_name, 0, DataType::Int, false),
        }],
        filter: None,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
    }
}

fn make_nontransparent_nested_hybrid_subquery(
    function_name: &str,
    estimated_rows: i32,
    output_name: &str,
) -> LogicalPlan {
    LogicalPlan::ProjectSource {
        source: Box::new(make_hybrid_leaf(function_name, estimated_rows, output_name)),
        outputs: vec![ProjectionExpr {
            field: ResultField {
                name: output_name.to_owned(),
                data_type: DataType::Int,
                text_type_modifier: None,
                nullable: false,
            },
            expr: TypedExpr::cast(
                TypedExpr::column_ref(output_name, 0, DataType::Int, false),
                DataType::Int,
            ),
        }],
        filter: None,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
    }
}

fn make_row_expanding_hybrid_subquery() -> LogicalPlan {
    LogicalPlan::ProjectSource {
        source: Box::new(make_hybrid_leaf("vector_top_k_ids", 2, "note_id")),
        outputs: vec![ProjectionExpr {
            field: ResultField {
                name: "task_id".to_owned(),
                data_type: DataType::Int,
                text_type_modifier: None,
                nullable: false,
            },
            expr: TypedExpr::scalar_function(
                ScalarFunction::Generic("graph_neighbors".to_owned()),
                vec![
                    TypedExpr::literal(Value::Text("note_task".to_owned()), DataType::Text, false),
                    TypedExpr::column_ref("note_id", 0, DataType::Int, false),
                ],
                DataType::Int,
                false,
            ),
        }],
        filter: None,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
    }
}

fn assert_column_ordinal(expr: &TypedExpr, expected_ordinal: usize) {
    match &expr.kind {
        TypedExprKind::ColumnRef { ordinal, .. } => {
            assert_eq!(
                *ordinal, expected_ordinal,
                "expected column ordinal {expected_ordinal}, got {ordinal}"
            );
        }
        other => panic!("expected ColumnRef, got {other:?}"),
    }
}

fn assert_binary_eq_left_column_ordinal(expr: &TypedExpr, expected_ordinal: usize) {
    match &expr.kind {
        TypedExprKind::BinaryEq { left, .. } => assert_column_ordinal(left, expected_ordinal),
        other => panic!("expected BinaryEq, got {other:?}"),
    }
}

fn assert_scalar_function_arg_ordinals(
    expr: &TypedExpr,
    expected_func: ScalarFunction,
    expected_ordinals: &[usize],
) {
    match &expr.kind {
        TypedExprKind::ScalarFunction { func, args } => {
            assert_eq!(*func, expected_func, "unexpected scalar function: {func:?}");
            assert_eq!(
                args.len(),
                expected_ordinals.len(),
                "unexpected scalar function arity"
            );
            for (arg, expected_ordinal) in args.iter().zip(expected_ordinals.iter().copied()) {
                assert_column_ordinal(arg, expected_ordinal);
            }
        }
        other => panic!("expected ScalarFunction, got {other:?}"),
    }
}
