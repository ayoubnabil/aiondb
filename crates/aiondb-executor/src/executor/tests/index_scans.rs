use super::*;

/// A storage mock that can return pre-configured rows for specific index
/// scans, allowing us to exercise the `IndexEq` / `IndexRange` access paths.
struct MockStorageWithIndex {
    /// Rows that a `SeqScan` on any table returns.
    tables: Mutex<std::collections::HashMap<RelationId, Vec<(TupleId, Row)>>>,
    /// Rows that an index scan returns, keyed by `IndexId`.
    /// The caller configures which rows to return via `set_index_rows`.
    index_rows: Mutex<std::collections::HashMap<IndexId, Vec<TupleRecord>>>,
    index_limited_count: Mutex<usize>,
    scan_table_count: Mutex<usize>,
    scan_table_counts: Mutex<std::collections::HashMap<RelationId, usize>>,
    scan_index_count: Mutex<usize>,
    scan_index_counts: Mutex<std::collections::HashMap<IndexId, usize>>,
    scan_index_ordered_count: Mutex<usize>,
    fetch_count: Mutex<usize>,
    next_tuple_id: Mutex<u64>,
}

impl MockStorageWithIndex {
    fn new() -> Self {
        Self {
            tables: Mutex::new(std::collections::HashMap::new()),
            index_rows: Mutex::new(std::collections::HashMap::new()),
            index_limited_count: Mutex::new(0),
            scan_table_count: Mutex::new(0),
            scan_table_counts: Mutex::new(std::collections::HashMap::new()),
            scan_index_count: Mutex::new(0),
            scan_index_counts: Mutex::new(std::collections::HashMap::new()),
            scan_index_ordered_count: Mutex::new(0),
            fetch_count: Mutex::new(0),
            next_tuple_id: Mutex::new(1),
        }
    }

    /// Register rows that will be returned by `scan_index` for the given index.
    fn set_index_rows(&self, index_id: IndexId, rows: Vec<TupleRecord>) {
        self.index_rows.lock().unwrap().insert(index_id, rows);
    }

    fn fetch_count(&self) -> usize {
        *self.fetch_count.lock().unwrap()
    }

    fn scan_table_count(&self) -> usize {
        *self.scan_table_count.lock().unwrap()
    }

    fn scan_table_count_for(&self, table_id: RelationId) -> usize {
        self.scan_table_counts
            .lock()
            .unwrap()
            .get(&table_id)
            .copied()
            .unwrap_or(0)
    }

    fn scan_index_count(&self) -> usize {
        *self.scan_index_count.lock().unwrap()
    }

    fn scan_index_count_for(&self, index_id: IndexId) -> usize {
        self.scan_index_counts
            .lock()
            .unwrap()
            .get(&index_id)
            .copied()
            .unwrap_or(0)
    }

    fn scan_index_ordered_count(&self) -> usize {
        *self.scan_index_ordered_count.lock().unwrap()
    }

    fn index_limited_count(&self) -> usize {
        *self.index_limited_count.lock().unwrap()
    }
}

fn project_mock_row(row: &Row, projected_columns: Option<&[ColumnId]>) -> Row {
    let Some(projected_columns) = projected_columns else {
        return row.clone();
    };
    let values = projected_columns
        .iter()
        .map(|column_id| {
            usize::try_from(column_id.get().saturating_sub(1))
                .ok()
                .and_then(|ordinal| row.values.get(ordinal))
                .cloned()
                .unwrap_or(Value::Null)
        })
        .collect();
    Row::new(values)
}

impl StorageDDL for MockStorageWithIndex {
    fn create_table_storage(&self, _txn: TxnId, table: &TableStorageDescriptor) -> DbResult<()> {
        self.tables
            .lock()
            .unwrap()
            .insert(table.table_id, Vec::new());
        Ok(())
    }

    fn create_index_storage(
        &self,
        _txn: TxnId,
        _index: &aiondb_storage_api::IndexStorageDescriptor,
    ) -> DbResult<()> {
        Ok(())
    }

    fn alter_table_storage(&self, _txn: TxnId, _table: &TableStorageDescriptor) -> DbResult<()> {
        Ok(())
    }

    fn drop_table_storage(&self, _txn: TxnId, table_id: RelationId) -> DbResult<()> {
        self.tables.lock().unwrap().remove(&table_id);
        Ok(())
    }

    fn drop_index_storage(&self, _txn: TxnId, _index_id: IndexId) -> DbResult<()> {
        Ok(())
    }
}

impl StorageDML for MockStorageWithIndex {
    fn scan_table(
        &self,
        _txn: TxnId,
        _snapshot: &Snapshot,
        table_id: RelationId,
        projected_columns: Option<Vec<ColumnId>>,
    ) -> DbResult<Box<dyn TupleStream>> {
        *self.scan_table_count.lock().unwrap() += 1;
        *self
            .scan_table_counts
            .lock()
            .unwrap()
            .entry(table_id)
            .or_default() += 1;
        let tables = self.tables.lock().unwrap();
        let records: Vec<TupleRecord> = tables
            .get(&table_id)
            .unwrap_or(&Vec::new())
            .iter()
            .map(|(tid, row)| TupleRecord {
                tuple_id: *tid,
                heap_position: tid.get(),
                row: project_mock_row(row, projected_columns.as_deref()),
            })
            .collect();
        Ok(Box::new(VecTupleStream::new(records)))
    }

    fn scan_index(
        &self,
        _txn: TxnId,
        _snapshot: &Snapshot,
        index_id: IndexId,
        _key_range: KeyRange,
        projected_columns: Option<Vec<ColumnId>>,
    ) -> DbResult<Box<dyn TupleStream>> {
        *self.scan_index_count.lock().unwrap() += 1;
        *self
            .scan_index_counts
            .lock()
            .unwrap()
            .entry(index_id)
            .or_default() += 1;
        let index_rows = self.index_rows.lock().unwrap();
        let records = index_rows
            .get(&index_id)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|mut record| {
                record.row = project_mock_row(&record.row, projected_columns.as_deref());
                record
            })
            .collect();
        Ok(Box::new(VecTupleStream::new(records)))
    }

    fn fetch(
        &self,
        _txn: TxnId,
        _snapshot: &Snapshot,
        table_id: RelationId,
        tuple_id: TupleId,
        projected_columns: Option<Vec<ColumnId>>,
    ) -> DbResult<Option<Row>> {
        *self.fetch_count.lock().unwrap() += 1;
        let tables = self.tables.lock().unwrap();
        Ok(tables
            .get(&table_id)
            .and_then(|rows| rows.iter().find(|(tid, _)| *tid == tuple_id))
            .map(|(_, row)| project_mock_row(row, projected_columns.as_deref())))
    }

    fn scan_index_limited(
        &self,
        txn: TxnId,
        snapshot: &Snapshot,
        index_id: IndexId,
        key_range: KeyRange,
        projected_columns: Option<Vec<ColumnId>>,
        limit: usize,
    ) -> DbResult<Box<dyn TupleStream>> {
        *self.index_limited_count.lock().unwrap() += 1;
        let mut stream = self.scan_index(txn, snapshot, index_id, key_range, projected_columns)?;
        let mut records = Vec::with_capacity(limit);
        while records.len() < limit {
            let Some(record) = stream.next()? else {
                break;
            };
            records.push(record);
        }
        Ok(Box::new(VecTupleStream::new(records)))
    }

    fn scan_index_ordered(
        &self,
        txn: TxnId,
        snapshot: &Snapshot,
        index_id: IndexId,
        key_range: KeyRange,
        projected_columns: Option<Vec<ColumnId>>,
        descending: bool,
    ) -> DbResult<Box<dyn TupleStream>> {
        *self.scan_index_ordered_count.lock().unwrap() += 1;
        let mut stream = self.scan_index(txn, snapshot, index_id, key_range, projected_columns)?;
        let mut records = Vec::new();
        while let Some(record) = stream.next()? {
            records.push(record);
        }
        if descending {
            records.reverse();
        }
        Ok(Box::new(VecTupleStream::new(records)))
    }

    fn insert(&self, _txn: TxnId, table_id: RelationId, row: Row) -> DbResult<TupleId> {
        let mut next_id = self.next_tuple_id.lock().unwrap();
        let tuple_id = TupleId::new(*next_id);
        *next_id += 1;
        self.tables
            .lock()
            .unwrap()
            .entry(table_id)
            .or_default()
            .push((tuple_id, row));
        Ok(tuple_id)
    }

    fn update(
        &self,
        _txn: TxnId,
        table_id: RelationId,
        tuple_id: TupleId,
        row: Row,
    ) -> DbResult<TupleId> {
        let mut tables = self.tables.lock().unwrap();
        if let Some(rows) = tables.get_mut(&table_id) {
            if let Some(entry) = rows.iter_mut().find(|(tid, _)| *tid == tuple_id) {
                entry.1 = row;
            }
        }
        Ok(tuple_id)
    }

    fn delete(&self, _txn: TxnId, table_id: RelationId, tuple_id: TupleId) -> DbResult<()> {
        let mut tables = self.tables.lock().unwrap();
        if let Some(rows) = tables.get_mut(&table_id) {
            rows.retain(|(tid, _)| *tid != tuple_id);
        }
        Ok(())
    }

    fn vacuum_table(&self, _table_id: RelationId) -> DbResult<u64> {
        Ok(0)
    }
}

impl aiondb_storage_api::StorageTxnParticipant for MockStorageWithIndex {
    fn begin_txn(&self, _txn: TxnId, _isolation: aiondb_tx::IsolationLevel) -> DbResult<()> {
        Ok(())
    }

    fn commit_txn(&self, _txn: TxnId, _commit_ts: u64) -> DbResult<()> {
        Ok(())
    }

    fn rollback_txn(&self, _txn: TxnId) -> DbResult<()> {
        Ok(())
    }

    fn checkpoint(&self) -> DbResult<aiondb_storage_api::CheckpointInfo> {
        Ok(aiondb_storage_api::CheckpointInfo {
            checkpoint_lsn: 0,
            dirty_pages_flushed: 0,
        })
    }

    fn create_savepoint(&self, _txn: TxnId) -> DbResult<u64> {
        Ok(1)
    }

    fn rollback_to_savepoint(&self, _txn: TxnId, _savepoint_id: u64) -> DbResult<()> {
        Ok(())
    }

    fn release_savepoint(&self, _txn: TxnId, _savepoint_id: u64) -> DbResult<()> {
        Ok(())
    }
}

fn make_executor_with_index_storage() -> (Executor, Arc<MockCatalog>, Arc<MockStorageWithIndex>) {
    let catalog = Arc::new(MockCatalog::new());
    let storage = Arc::new(MockStorageWithIndex::new());
    let executor = Executor::new(
        catalog.clone(),
        catalog.clone(),
        catalog.clone(),
        catalog.clone(),
        storage.clone(),
        storage.clone(),
        storage.clone(),
        Arc::new(|logical_plan, _context| {
            Ok(aiondb_optimizer::physical_builder::PhysicalBuilder.build(logical_plan.clone()))
        }),
    );
    (executor, catalog, storage)
}

#[test]
fn index_eq_scan_returns_matching_rows() {
    let (executor, catalog, storage) = make_executor_with_index_storage();
    let ctx = default_context();

    // Create a table so the catalog knows about it.
    let table_id = create_test_table(
        &executor,
        &catalog,
        "t_idx_eq",
        vec![
            ColumnPlan {
                name: "id".to_string(),
                data_type: DataType::Int,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                has_default: false,
            },
            ColumnPlan {
                name: "val".to_string(),
                data_type: DataType::Text,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                has_default: false,
            },
        ],
    );

    // Simulate index data: two rows matching the index key.
    let index_id = IndexId::new(42);
    storage.set_index_rows(
        index_id,
        vec![
            TupleRecord {
                tuple_id: TupleId::new(1),
                heap_position: 1,
                row: Row::new(vec![Value::Int(5), Value::Text("alpha".to_string())]),
            },
            TupleRecord {
                tuple_id: TupleId::new(2),
                heap_position: 2,
                row: Row::new(vec![Value::Int(5), Value::Text("beta".to_string())]),
            },
        ],
    );

    let plan = PhysicalPlan::ProjectTable {
        table_id,
        outputs: vec![
            make_projection_expr(
                "id",
                DataType::Int,
                false,
                TypedExpr::column_ref("id", 0, DataType::Int, false),
            ),
            make_projection_expr(
                "val",
                DataType::Text,
                false,
                TypedExpr::column_ref("val", 1, DataType::Text, false),
            ),
        ],
        filter: None,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
        access_path: ScanAccessPath::IndexEq {
            index_id,
            value: Value::Int(5),
        },
    };

    let result = executor.execute(&plan, &ctx).unwrap();
    match result {
        ExecutionResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 2);
            assert_eq!(rows[0].values[0], Value::Int(5));
            assert_eq!(rows[0].values[1], Value::Text("alpha".to_string()));
            assert_eq!(rows[1].values[0], Value::Int(5));
            assert_eq!(rows[1].values[1], Value::Text("beta".to_string()));
        }
        _ => panic!("expected Query result"),
    }
}

#[test]
fn index_range_scan_returns_rows_in_range() {
    let (executor, catalog, storage) = make_executor_with_index_storage();
    let ctx = default_context();

    let table_id = create_test_table(
        &executor,
        &catalog,
        "t_idx_range",
        vec![ColumnPlan {
            name: "val".to_string(),
            data_type: DataType::Int,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: false,
            has_default: false,
        }],
    );

    let index_id = IndexId::new(43);
    storage.set_index_rows(
        index_id,
        vec![
            TupleRecord {
                tuple_id: TupleId::new(1),
                heap_position: 1,
                row: Row::new(vec![Value::Int(10)]),
            },
            TupleRecord {
                tuple_id: TupleId::new(2),
                heap_position: 2,
                row: Row::new(vec![Value::Int(20)]),
            },
            TupleRecord {
                tuple_id: TupleId::new(3),
                heap_position: 3,
                row: Row::new(vec![Value::Int(30)]),
            },
        ],
    );

    let plan = PhysicalPlan::ProjectTable {
        table_id,
        outputs: vec![make_projection_expr(
            "val",
            DataType::Int,
            false,
            TypedExpr::column_ref("val", 0, DataType::Int, false),
        )],
        filter: None,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
        access_path: ScanAccessPath::IndexRange {
            index_id,
            lower: std::ops::Bound::Included(Value::Int(10)),
            upper: std::ops::Bound::Excluded(Value::Int(30)),
        },
    };

    let result = executor.execute(&plan, &ctx).unwrap();
    match result {
        ExecutionResult::Query { rows, .. } => {
            // The mock returns all pre-configured rows; the key range is
            // trusted to the storage layer. We verify that the executor
            // correctly passes through whatever the storage returns.
            assert_eq!(rows.len(), 3);
            assert_eq!(rows[0].values[0], Value::Int(10));
            assert_eq!(rows[1].values[0], Value::Int(20));
            assert_eq!(rows[2].values[0], Value::Int(30));
        }
        _ => panic!("expected Query result"),
    }
}

#[test]
fn index_eq_scan_no_matching_key_returns_empty() {
    // The default MockStorage::scan_index returns an empty stream for any
    // index ID, simulating the case where no rows match the lookup key.
    let (executor, catalog, _) = make_executor();
    let ctx = default_context();

    let table_id = create_test_table(
        &executor,
        &catalog,
        "t_idx_empty",
        vec![ColumnPlan {
            name: "val".to_string(),
            data_type: DataType::Int,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: false,
            has_default: false,
        }],
    );

    let plan = PhysicalPlan::ProjectTable {
        table_id,
        outputs: vec![make_projection_expr(
            "val",
            DataType::Int,
            false,
            TypedExpr::column_ref("val", 0, DataType::Int, false),
        )],
        filter: None,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
        access_path: ScanAccessPath::IndexEq {
            index_id: IndexId::new(999),
            value: Value::Int(42),
        },
    };

    let result = executor.execute(&plan, &ctx).unwrap();
    match result {
        ExecutionResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 0, "no matching key should return empty result");
        }
        _ => panic!("expected Query result"),
    }
}

#[test]
fn nested_loop_index_join_distinct_hash_dedups_non_adjacent_rows() {
    let (executor, catalog, storage) = make_executor_with_index_storage();
    let ctx = default_context();
    let right_table_id = create_test_table(
        &executor,
        &catalog,
        "t_nlij_distinct",
        vec![ColumnPlan {
            name: "val".to_string(),
            data_type: DataType::Int,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: false,
            has_default: false,
        }],
    );
    let index_id = IndexId::new(77);
    storage.set_index_rows(
        index_id,
        vec![
            TupleRecord {
                tuple_id: TupleId::new(1),
                heap_position: 1,
                row: Row::new(vec![Value::Int(1)]),
            },
            TupleRecord {
                tuple_id: TupleId::new(2),
                heap_position: 2,
                row: Row::new(vec![Value::Int(2)]),
            },
            TupleRecord {
                tuple_id: TupleId::new(3),
                heap_position: 3,
                row: Row::new(vec![Value::Int(1)]),
            },
        ],
    );

    let left = PhysicalPlan::ProjectValues {
        output_fields: vec![ResultField {
            name: "lookup".to_owned(),
            data_type: DataType::Int,
            text_type_modifier: None,
            nullable: false,
        }],
        rows: vec![vec![TypedExpr::literal(
            Value::Int(42),
            DataType::Int,
            false,
        )]],
        order_by: Vec::new(),
        limit: None,
        offset: None,
    };
    let plan = PhysicalPlan::NestedLoopIndexJoin {
        left: Box::new(left),
        right_table_id,
        right_index_id: index_id,
        right_width: 1,
        outer_key_ordinal: 0,
        join_type: aiondb_plan::JoinType::Inner,
        right_filter: None,
        residual: None,
        outputs: vec![make_projection_expr(
            "val",
            DataType::Int,
            false,
            TypedExpr::column_ref("val", 1, DataType::Int, false),
        )],
        filter: None,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: true,
        distinct_on: Vec::new(),
    };

    let result = executor
        .execute(&plan, &ctx)
        .expect("nested loop index join");
    match result {
        ExecutionResult::Query { rows, .. } => {
            assert_eq!(
                rows,
                vec![Row::new(vec![Value::Int(1)]), Row::new(vec![Value::Int(2)])]
            );
        }
        other => panic!("expected query result, got {other:?}"),
    }
}

#[test]
fn nested_loop_index_join_applies_distinct_on_after_sorting() {
    let (executor, catalog, storage) = make_executor_with_index_storage();
    let ctx = default_context();
    let right_table_id = create_test_table(
        &executor,
        &catalog,
        "t_nlij_distinct_on",
        vec![
            ColumnPlan {
                name: "join_key".to_string(),
                data_type: DataType::Int,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                has_default: false,
            },
            ColumnPlan {
                name: "score".to_string(),
                data_type: DataType::Int,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                has_default: false,
            },
        ],
    );
    let index_id = IndexId::new(78);
    storage.set_index_rows(
        index_id,
        vec![
            TupleRecord {
                tuple_id: TupleId::new(1),
                heap_position: 1,
                row: Row::new(vec![Value::Int(1), Value::Int(10)]),
            },
            TupleRecord {
                tuple_id: TupleId::new(2),
                heap_position: 2,
                row: Row::new(vec![Value::Int(2), Value::Int(15)]),
            },
            TupleRecord {
                tuple_id: TupleId::new(3),
                heap_position: 3,
                row: Row::new(vec![Value::Int(1), Value::Int(20)]),
            },
        ],
    );

    let left = PhysicalPlan::ProjectValues {
        output_fields: vec![ResultField {
            name: "lookup".to_owned(),
            data_type: DataType::Int,
            text_type_modifier: None,
            nullable: false,
        }],
        rows: vec![vec![TypedExpr::literal(
            Value::Int(7),
            DataType::Int,
            false,
        )]],
        order_by: Vec::new(),
        limit: None,
        offset: None,
    };
    let plan = PhysicalPlan::NestedLoopIndexJoin {
        left: Box::new(left),
        right_table_id,
        right_index_id: index_id,
        right_width: 2,
        outer_key_ordinal: 0,
        join_type: aiondb_plan::JoinType::Inner,
        right_filter: None,
        residual: None,
        outputs: vec![
            make_projection_expr(
                "join_key",
                DataType::Int,
                false,
                TypedExpr::column_ref("join_key", 1, DataType::Int, false),
            ),
            make_projection_expr(
                "score",
                DataType::Int,
                false,
                TypedExpr::column_ref("score", 2, DataType::Int, false),
            ),
        ],
        filter: None,
        order_by: vec![aiondb_plan::SortExpr {
            expr: TypedExpr::column_ref("score", 2, DataType::Int, false),
            descending: true,
            nulls_first: Some(false),
        }],
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: vec![TypedExpr::column_ref("join_key", 0, DataType::Int, false)],
    };

    let result = executor
        .execute(&plan, &ctx)
        .expect("nested loop index join");
    match result {
        ExecutionResult::Query { rows, .. } => {
            assert_eq!(
                rows,
                vec![
                    Row::new(vec![Value::Int(1), Value::Int(20)]),
                    Row::new(vec![Value::Int(2), Value::Int(15)]),
                ]
            );
        }
        other => panic!("expected query result, got {other:?}"),
    }
}

#[test]
fn nested_loop_index_join_orders_by_non_projected_right_column() {
    let (executor, catalog, storage) = make_executor_with_index_storage();
    let ctx = default_context();
    let right_table_id = create_test_table(
        &executor,
        &catalog,
        "t_nlij_order_by_hidden_right_col",
        vec![
            ColumnPlan {
                name: "join_key".to_string(),
                data_type: DataType::Int,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                has_default: false,
            },
            ColumnPlan {
                name: "score".to_string(),
                data_type: DataType::Int,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                has_default: false,
            },
        ],
    );
    let index_id = IndexId::new(79);
    storage.set_index_rows(
        index_id,
        vec![
            TupleRecord {
                tuple_id: TupleId::new(1),
                heap_position: 1,
                row: Row::new(vec![Value::Int(1), Value::Int(10)]),
            },
            TupleRecord {
                tuple_id: TupleId::new(2),
                heap_position: 2,
                row: Row::new(vec![Value::Int(2), Value::Int(20)]),
            },
        ],
    );

    let plan = PhysicalPlan::NestedLoopIndexJoin {
        left: Box::new(PhysicalPlan::ProjectValues {
            output_fields: vec![ResultField {
                name: "lookup".to_owned(),
                data_type: DataType::Int,
                text_type_modifier: None,
                nullable: false,
            }],
            rows: vec![vec![TypedExpr::literal(
                Value::Int(5),
                DataType::Int,
                false,
            )]],
            order_by: Vec::new(),
            limit: None,
            offset: None,
        }),
        right_table_id,
        right_index_id: index_id,
        right_width: 2,
        outer_key_ordinal: 0,
        join_type: aiondb_plan::JoinType::Inner,
        right_filter: None,
        residual: None,
        outputs: vec![make_projection_expr(
            "join_key",
            DataType::Int,
            false,
            TypedExpr::column_ref("join_key", 1, DataType::Int, false),
        )],
        filter: None,
        order_by: vec![aiondb_plan::SortExpr {
            expr: TypedExpr::column_ref("score", 2, DataType::Int, false),
            descending: true,
            nulls_first: Some(false),
        }],
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
    };

    let result = executor
        .execute(&plan, &ctx)
        .expect("nested loop index join");
    match result {
        ExecutionResult::Query { rows, .. } => {
            assert_eq!(
                rows,
                vec![Row::new(vec![Value::Int(2)]), Row::new(vec![Value::Int(1)])]
            );
        }
        other => panic!("expected query result, got {other:?}"),
    }
}

#[test]
fn index_range_scan_empty_range_returns_empty() {
    let (executor, catalog, _) = make_executor();
    let ctx = default_context();

    let table_id = create_test_table(
        &executor,
        &catalog,
        "t_idx_range_empty",
        vec![ColumnPlan {
            name: "val".to_string(),
            data_type: DataType::Int,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: false,
            has_default: false,
        }],
    );

    let plan = PhysicalPlan::ProjectTable {
        table_id,
        outputs: vec![make_projection_expr(
            "val",
            DataType::Int,
            false,
            TypedExpr::column_ref("val", 0, DataType::Int, false),
        )],
        filter: None,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
        access_path: ScanAccessPath::IndexRange {
            index_id: IndexId::new(999),
            lower: std::ops::Bound::Included(Value::Int(100)),
            upper: std::ops::Bound::Included(Value::Int(200)),
        },
    };

    let result = executor.execute(&plan, &ctx).unwrap();
    match result {
        ExecutionResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 0, "range scan with no data should return empty");
        }
        _ => panic!("expected Query result"),
    }
}

#[test]
fn bitmap_or_scan_unions_rows_and_keeps_heap_order() {
    let (executor, catalog, storage) = make_executor_with_index_storage();
    let ctx = default_context();
    let table_id = create_test_table(
        &executor,
        &catalog,
        "t_bitmap_or",
        vec![ColumnPlan {
            name: "val".to_string(),
            data_type: DataType::Int,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: false,
            has_default: false,
        }],
    );

    storage.set_index_rows(
        IndexId::new(81),
        vec![
            TupleRecord {
                tuple_id: TupleId::new(1),
                heap_position: 1,
                row: Row::new(vec![Value::Int(10)]),
            },
            TupleRecord {
                tuple_id: TupleId::new(3),
                heap_position: 3,
                row: Row::new(vec![Value::Int(30)]),
            },
        ],
    );
    storage.set_index_rows(
        IndexId::new(82),
        vec![
            TupleRecord {
                tuple_id: TupleId::new(2),
                heap_position: 2,
                row: Row::new(vec![Value::Int(20)]),
            },
            TupleRecord {
                tuple_id: TupleId::new(3),
                heap_position: 3,
                row: Row::new(vec![Value::Int(30)]),
            },
        ],
    );

    let plan = PhysicalPlan::ProjectTable {
        table_id,
        outputs: vec![make_projection_expr(
            "val",
            DataType::Int,
            false,
            TypedExpr::column_ref("val", 0, DataType::Int, false),
        )],
        filter: None,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
        access_path: ScanAccessPath::BitmapOr {
            paths: vec![
                ScanAccessPath::IndexEq {
                    index_id: IndexId::new(81),
                    value: Value::Int(1),
                },
                ScanAccessPath::IndexEq {
                    index_id: IndexId::new(82),
                    value: Value::Int(2),
                },
            ],
        },
    };

    let result = executor.execute(&plan, &ctx).expect("bitmap or scan");
    match result {
        ExecutionResult::Query { rows, .. } => {
            assert_eq!(
                rows,
                vec![
                    Row::new(vec![Value::Int(10)]),
                    Row::new(vec![Value::Int(20)]),
                    Row::new(vec![Value::Int(30)]),
                ]
            );
        }
        other => panic!("expected query result, got {other:?}"),
    }
}

#[test]
fn bitmap_or_in_filter_uses_limited_unique_literal_lookups() {
    let (executor, catalog, storage) = make_executor_with_index_storage();
    let ctx = default_context();
    let table_id = create_test_table(
        &executor,
        &catalog,
        "t_bitmap_or_in_fast",
        vec![
            ColumnPlan {
                name: "id".to_string(),
                data_type: DataType::Int,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                has_default: false,
            },
            ColumnPlan {
                name: "val".to_string(),
                data_type: DataType::Int,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                has_default: false,
            },
        ],
    );
    let table = catalog
        .tables
        .lock()
        .unwrap()
        .iter()
        .find(|table| table.table_id == table_id)
        .cloned()
        .expect("table descriptor");
    let id_column_id = table.columns[0].column_id;

    let first_index_id = IndexId::new(125);
    let second_index_id = IndexId::new(126);
    catalog.indexes.lock().unwrap().extend([
        IndexDescriptor {
            index_id: first_index_id,
            schema_id: SchemaId::new(1),
            table_id,
            name: QualifiedName::qualified("public", "idx_t_bitmap_or_in_fast_1"),
            unique: true,
            nulls_not_distinct: false,
            kind: IndexKind::BTree,
            key_columns: vec![IndexKeyColumn {
                column_id: id_column_id,
                sort_order: SortOrder::Ascending,
                nulls_first: false,
            }],
            include_columns: Vec::new(),
            constraint_name: None,
            hnsw_params: None,
        },
        IndexDescriptor {
            index_id: second_index_id,
            schema_id: SchemaId::new(1),
            table_id,
            name: QualifiedName::qualified("public", "idx_t_bitmap_or_in_fast_2"),
            unique: true,
            nulls_not_distinct: false,
            kind: IndexKind::BTree,
            key_columns: vec![IndexKeyColumn {
                column_id: id_column_id,
                sort_order: SortOrder::Ascending,
                nulls_first: false,
            }],
            include_columns: Vec::new(),
            constraint_name: None,
            hnsw_params: None,
        },
    ]);

    storage.set_index_rows(
        first_index_id,
        vec![TupleRecord {
            tuple_id: TupleId::new(1),
            heap_position: 1,
            row: Row::new(vec![Value::Int(1), Value::Int(10)]),
        }],
    );
    storage.set_index_rows(
        second_index_id,
        vec![TupleRecord {
            tuple_id: TupleId::new(2),
            heap_position: 2,
            row: Row::new(vec![Value::Int(2), Value::Int(20)]),
        }],
    );

    let plan = PhysicalPlan::ProjectTable {
        table_id,
        outputs: vec![make_projection_expr(
            "val",
            DataType::Int,
            false,
            TypedExpr::column_ref("val", 1, DataType::Int, false),
        )],
        filter: Some(TypedExpr::in_list(
            TypedExpr::column_ref("id", 0, DataType::Int, false),
            vec![
                TypedExpr::literal(Value::Int(1), DataType::Int, false),
                TypedExpr::literal(Value::Int(2), DataType::Int, false),
            ],
            false,
        )),
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
        access_path: ScanAccessPath::BitmapOr {
            paths: vec![
                ScanAccessPath::IndexEq {
                    index_id: first_index_id,
                    value: Value::Int(1),
                },
                ScanAccessPath::IndexEq {
                    index_id: second_index_id,
                    value: Value::Int(2),
                },
            ],
        },
    };

    let result = executor
        .execute(&plan, &ctx)
        .expect("bitmap or IN fast path");
    match result {
        ExecutionResult::Query { rows, .. } => {
            assert_eq!(
                rows,
                vec![
                    Row::new(vec![Value::Int(10)]),
                    Row::new(vec![Value::Int(20)]),
                ]
            );
        }
        other => panic!("expected query result, got {other:?}"),
    }
    assert_eq!(storage.index_limited_count(), 2);
    assert_eq!(storage.scan_table_count(), 0);
}

#[test]
fn bitmap_and_scan_intersects_rows() {
    let (executor, catalog, storage) = make_executor_with_index_storage();
    let ctx = default_context();
    let table_id = create_test_table(
        &executor,
        &catalog,
        "t_bitmap_and",
        vec![ColumnPlan {
            name: "val".to_string(),
            data_type: DataType::Int,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: false,
            has_default: false,
        }],
    );

    storage.set_index_rows(
        IndexId::new(83),
        vec![
            TupleRecord {
                tuple_id: TupleId::new(1),
                heap_position: 1,
                row: Row::new(vec![Value::Int(10)]),
            },
            TupleRecord {
                tuple_id: TupleId::new(2),
                heap_position: 2,
                row: Row::new(vec![Value::Int(20)]),
            },
        ],
    );
    storage.set_index_rows(
        IndexId::new(84),
        vec![
            TupleRecord {
                tuple_id: TupleId::new(2),
                heap_position: 2,
                row: Row::new(vec![Value::Int(20)]),
            },
            TupleRecord {
                tuple_id: TupleId::new(3),
                heap_position: 3,
                row: Row::new(vec![Value::Int(30)]),
            },
        ],
    );

    let plan = PhysicalPlan::ProjectTable {
        table_id,
        outputs: vec![make_projection_expr(
            "val",
            DataType::Int,
            false,
            TypedExpr::column_ref("val", 0, DataType::Int, false),
        )],
        filter: None,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
        access_path: ScanAccessPath::BitmapAnd {
            paths: vec![
                ScanAccessPath::IndexEq {
                    index_id: IndexId::new(83),
                    value: Value::Int(1),
                },
                ScanAccessPath::IndexEq {
                    index_id: IndexId::new(84),
                    value: Value::Int(2),
                },
            ],
        },
    };

    let result = executor.execute(&plan, &ctx).expect("bitmap and scan");
    match result {
        ExecutionResult::Query { rows, .. } => {
            assert_eq!(rows, vec![Row::new(vec![Value::Int(20)])]);
        }
        other => panic!("expected query result, got {other:?}"),
    }
}

#[test]
fn index_only_scan_supports_filter_without_heap_fetch() {
    let (executor, catalog, storage) = make_executor_with_index_storage();
    let ctx = default_context();
    let table_id = create_test_table(
        &executor,
        &catalog,
        "t_index_only_scan",
        vec![
            ColumnPlan {
                name: "a".to_string(),
                data_type: DataType::Int,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                has_default: false,
            },
            ColumnPlan {
                name: "b".to_string(),
                data_type: DataType::Text,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                has_default: false,
            },
        ],
    );
    let table = catalog
        .tables
        .lock()
        .unwrap()
        .iter()
        .find(|table| table.table_id == table_id)
        .cloned()
        .expect("table descriptor");
    let a_column_id = table.columns[0].column_id;
    let b_column_id = table.columns[1].column_id;

    let index_id = IndexId::new(85);
    storage.set_index_rows(
        index_id,
        vec![
            TupleRecord {
                tuple_id: TupleId::new(1),
                heap_position: 1,
                row: Row::new(vec![Value::Int(1), Value::Text("alpha".to_string())]),
            },
            TupleRecord {
                tuple_id: TupleId::new(2),
                heap_position: 2,
                row: Row::new(vec![Value::Int(2), Value::Text("beta".to_string())]),
            },
        ],
    );

    let plan = PhysicalPlan::ProjectTable {
        table_id,
        outputs: vec![make_projection_expr(
            "b",
            DataType::Text,
            false,
            TypedExpr::column_ref("b", 1, DataType::Text, false),
        )],
        filter: Some(TypedExpr::binary_eq(
            TypedExpr::column_ref("a", 0, DataType::Int, false),
            TypedExpr::literal(Value::Int(1), DataType::Int, false),
        )),
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
        access_path: ScanAccessPath::IndexOnlyScan {
            inner: Box::new(ScanAccessPath::IndexEq {
                index_id,
                value: Value::Int(1),
            }),
            index_column_ids: vec![a_column_id, b_column_id],
        },
    };

    let result = executor.execute(&plan, &ctx).expect("index-only scan");
    match result {
        ExecutionResult::Query { rows, .. } => {
            assert_eq!(rows, vec![Row::new(vec![Value::Text("alpha".to_string())])]);
        }
        other => panic!("expected query result, got {other:?}"),
    }
}

#[test]
fn top_k_filter_order_fetches_only_winners() {
    let (executor, catalog, storage) = make_executor_with_index_storage();
    let ctx = default_context();
    let table_id = create_test_table(
        &executor,
        &catalog,
        "t_top_k_filter_order",
        vec![
            ColumnPlan {
                name: "id".to_string(),
                data_type: DataType::Int,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                has_default: false,
            },
            ColumnPlan {
                name: "user_id".to_string(),
                data_type: DataType::Int,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                has_default: false,
            },
            ColumnPlan {
                name: "title".to_string(),
                data_type: DataType::Text,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                has_default: false,
            },
            ColumnPlan {
                name: "likes".to_string(),
                data_type: DataType::Int,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                has_default: false,
            },
        ],
    );
    let table = catalog
        .tables
        .lock()
        .unwrap()
        .iter()
        .find(|table| table.table_id == table_id)
        .cloned()
        .expect("table descriptor");
    let user_id_column_id = table.columns[1].column_id;
    let index_id = IndexId::new(120);
    catalog.indexes.lock().unwrap().push(IndexDescriptor {
        index_id,
        schema_id: SchemaId::new(1),
        table_id,
        name: QualifiedName::qualified("public", "idx_t_top_k_filter_order_user_id"),
        unique: false,
        nulls_not_distinct: false,
        kind: IndexKind::BTree,
        key_columns: vec![IndexKeyColumn {
            column_id: user_id_column_id,
            sort_order: SortOrder::Ascending,
            nulls_first: false,
        }],
        include_columns: Vec::new(),
        constraint_name: None,
        hnsw_params: None,
    });

    {
        let mut tables = storage.tables.lock().unwrap();
        let rows = tables.entry(table_id).or_default();
        rows.push((
            TupleId::new(1),
            Row::new(vec![
                Value::Int(1),
                Value::Int(7),
                Value::Text("one".to_string()),
                Value::Int(10),
            ]),
        ));
        rows.push((
            TupleId::new(2),
            Row::new(vec![
                Value::Int(2),
                Value::Int(7),
                Value::Text("two".to_string()),
                Value::Int(20),
            ]),
        ));
        rows.push((
            TupleId::new(3),
            Row::new(vec![
                Value::Int(3),
                Value::Int(7),
                Value::Text("three".to_string()),
                Value::Int(30),
            ]),
        ));
    }
    storage.set_index_rows(
        index_id,
        vec![
            TupleRecord {
                tuple_id: TupleId::new(1),
                heap_position: 1,
                row: Row::new(vec![
                    Value::Int(1),
                    Value::Int(7),
                    Value::Text("index-one".to_string()),
                    Value::Int(1010),
                ]),
            },
            TupleRecord {
                tuple_id: TupleId::new(2),
                heap_position: 2,
                row: Row::new(vec![
                    Value::Int(2),
                    Value::Int(7),
                    Value::Text("index-two".to_string()),
                    Value::Int(2020),
                ]),
            },
            TupleRecord {
                tuple_id: TupleId::new(3),
                heap_position: 3,
                row: Row::new(vec![
                    Value::Int(3),
                    Value::Int(7),
                    Value::Text("index-three".to_string()),
                    Value::Int(3030),
                ]),
            },
        ],
    );

    let plan = PhysicalPlan::ProjectTable {
        table_id,
        outputs: vec![
            make_projection_expr(
                "id",
                DataType::Int,
                false,
                TypedExpr::column_ref("id", 0, DataType::Int, false),
            ),
            make_projection_expr(
                "title",
                DataType::Text,
                false,
                TypedExpr::column_ref("title", 2, DataType::Text, false),
            ),
            make_projection_expr(
                "likes",
                DataType::Int,
                false,
                TypedExpr::column_ref("likes", 3, DataType::Int, false),
            ),
        ],
        filter: Some(TypedExpr::binary_eq(
            TypedExpr::column_ref("user_id", 1, DataType::Int, false),
            TypedExpr::literal(Value::Int(7), DataType::Int, false),
        )),
        order_by: vec![SortExpr {
            expr: TypedExpr::column_ref("id", 0, DataType::Int, false),
            descending: true,
            nulls_first: None,
        }],
        limit: Some(TypedExpr::literal(Value::Int(2), DataType::Int, false)),
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
        access_path: ScanAccessPath::IndexEq {
            index_id,
            value: Value::Int(7),
        },
    };

    let result = executor
        .execute(&plan, &ctx)
        .expect("top-k filter order query");
    match result {
        ExecutionResult::Query { rows, .. } => {
            assert_eq!(
                rows,
                vec![
                    Row::new(vec![
                        Value::Int(3),
                        Value::Text("three".to_string()),
                        Value::Int(30),
                    ]),
                    Row::new(vec![
                        Value::Int(2),
                        Value::Text("two".to_string()),
                        Value::Int(20),
                    ]),
                ]
            );
        }
        other => panic!("expected query result, got {other:?}"),
    }
    assert_eq!(storage.fetch_count(), 2);
}

#[test]
fn index_eq_limit_without_order_uses_limited_index_scan() {
    let (executor, catalog, storage) = make_executor_with_index_storage();
    let ctx = default_context();
    let table_id = create_test_table(
        &executor,
        &catalog,
        "t_idx_eq_limit",
        vec![
            ColumnPlan {
                name: "id".to_string(),
                data_type: DataType::Int,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                has_default: false,
            },
            ColumnPlan {
                name: "title".to_string(),
                data_type: DataType::Text,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                has_default: false,
            },
        ],
    );
    let table = catalog
        .tables
        .lock()
        .unwrap()
        .iter()
        .find(|table| table.table_id == table_id)
        .cloned()
        .expect("table descriptor");

    let index_id = IndexId::new(121);
    catalog.indexes.lock().unwrap().push(IndexDescriptor {
        index_id,
        schema_id: SchemaId::new(1),
        table_id,
        name: QualifiedName::qualified("public", "idx_t_idx_eq_limit_id"),
        unique: false,
        nulls_not_distinct: false,
        kind: IndexKind::BTree,
        key_columns: vec![IndexKeyColumn {
            column_id: table.columns[0].column_id,
            sort_order: SortOrder::Ascending,
            nulls_first: false,
        }],
        include_columns: Vec::new(),
        constraint_name: None,
        hnsw_params: None,
    });
    storage.set_index_rows(
        index_id,
        vec![
            TupleRecord {
                tuple_id: TupleId::new(1),
                heap_position: 1,
                row: Row::new(vec![Value::Int(1), Value::Text("index-one".to_string())]),
            },
            TupleRecord {
                tuple_id: TupleId::new(2),
                heap_position: 2,
                row: Row::new(vec![Value::Int(2), Value::Text("index-two".to_string())]),
            },
            TupleRecord {
                tuple_id: TupleId::new(3),
                heap_position: 3,
                row: Row::new(vec![Value::Int(3), Value::Text("index-three".to_string())]),
            },
        ],
    );
    {
        let mut tables = storage.tables.lock().unwrap();
        let rows = tables.entry(table_id).or_default();
        rows.push((
            TupleId::new(1),
            Row::new(vec![Value::Int(1), Value::Text("one".to_string())]),
        ));
        rows.push((
            TupleId::new(2),
            Row::new(vec![Value::Int(2), Value::Text("two".to_string())]),
        ));
        rows.push((
            TupleId::new(3),
            Row::new(vec![Value::Int(3), Value::Text("three".to_string())]),
        ));
    }

    let plan = PhysicalPlan::ProjectTable {
        table_id,
        outputs: vec![
            make_projection_expr(
                "id",
                DataType::Int,
                false,
                TypedExpr::column_ref("id", 0, DataType::Int, false),
            ),
            make_projection_expr(
                "title",
                DataType::Text,
                false,
                TypedExpr::column_ref("title", 1, DataType::Text, false),
            ),
        ],
        filter: None,
        order_by: Vec::new(),
        limit: Some(TypedExpr::literal(Value::Int(2), DataType::Int, false)),
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
        access_path: ScanAccessPath::IndexEq {
            index_id,
            value: Value::Int(7),
        },
    };

    let result = executor.execute(&plan, &ctx).expect("index eq limit query");
    match result {
        ExecutionResult::Query { rows, .. } => {
            assert_eq!(
                rows,
                vec![
                    Row::new(vec![Value::Int(1), Value::Text("one".to_string())]),
                    Row::new(vec![Value::Int(2), Value::Text("two".to_string())]),
                ]
            );
        }
        other => panic!("expected query result, got {other:?}"),
    }
    assert_eq!(storage.index_limited_count(), 1);
    assert_eq!(storage.fetch_count(), 2);
}

#[test]
fn update_with_and_eq_filter_uses_bitmap_index_intersection() {
    let (executor, catalog, storage) = make_executor_with_index_storage();
    let ctx = default_context();
    let table_id = create_test_table(
        &executor,
        &catalog,
        "t_update_bitmap_and",
        vec![
            ColumnPlan {
                name: "tenant_id".to_string(),
                data_type: DataType::Int,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                has_default: false,
            },
            ColumnPlan {
                name: "category_id".to_string(),
                data_type: DataType::Int,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                has_default: false,
            },
            ColumnPlan {
                name: "stock_qty".to_string(),
                data_type: DataType::Int,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                has_default: false,
            },
        ],
    );
    let table = catalog
        .tables
        .lock()
        .unwrap()
        .iter()
        .find(|table| table.table_id == table_id)
        .cloned()
        .expect("table descriptor");

    let tenant_index_id = IndexId::new(122);
    let category_index_id = IndexId::new(123);
    catalog.indexes.lock().unwrap().extend([
        IndexDescriptor {
            index_id: tenant_index_id,
            schema_id: SchemaId::new(1),
            table_id,
            name: QualifiedName::qualified("public", "idx_t_update_bitmap_and_tenant"),
            unique: false,
            nulls_not_distinct: false,
            kind: IndexKind::BTree,
            key_columns: vec![IndexKeyColumn {
                column_id: table.columns[0].column_id,
                sort_order: SortOrder::Ascending,
                nulls_first: false,
            }],
            include_columns: Vec::new(),
            constraint_name: None,
            hnsw_params: None,
        },
        IndexDescriptor {
            index_id: category_index_id,
            schema_id: SchemaId::new(1),
            table_id,
            name: QualifiedName::qualified("public", "idx_t_update_bitmap_and_category"),
            unique: false,
            nulls_not_distinct: false,
            kind: IndexKind::BTree,
            key_columns: vec![IndexKeyColumn {
                column_id: table.columns[1].column_id,
                sort_order: SortOrder::Ascending,
                nulls_first: false,
            }],
            include_columns: Vec::new(),
            constraint_name: None,
            hnsw_params: None,
        },
    ]);

    {
        let mut tables = storage.tables.lock().unwrap();
        let rows = tables.entry(table_id).or_default();
        rows.push((
            TupleId::new(1),
            Row::new(vec![Value::Int(1), Value::Int(10), Value::Int(5)]),
        ));
        rows.push((
            TupleId::new(2),
            Row::new(vec![Value::Int(1), Value::Int(11), Value::Int(4)]),
        ));
        rows.push((
            TupleId::new(3),
            Row::new(vec![Value::Int(1), Value::Int(10), Value::Int(0)]),
        ));
        rows.push((
            TupleId::new(4),
            Row::new(vec![Value::Int(2), Value::Int(10), Value::Int(8)]),
        ));
    }

    storage.set_index_rows(
        tenant_index_id,
        vec![
            TupleRecord {
                tuple_id: TupleId::new(1),
                heap_position: 1,
                row: Row::new(vec![Value::Int(1), Value::Int(10), Value::Int(5)]),
            },
            TupleRecord {
                tuple_id: TupleId::new(2),
                heap_position: 2,
                row: Row::new(vec![Value::Int(1), Value::Int(11), Value::Int(4)]),
            },
            TupleRecord {
                tuple_id: TupleId::new(3),
                heap_position: 3,
                row: Row::new(vec![Value::Int(1), Value::Int(10), Value::Int(0)]),
            },
        ],
    );
    storage.set_index_rows(
        category_index_id,
        vec![
            TupleRecord {
                tuple_id: TupleId::new(1),
                heap_position: 1,
                row: Row::new(vec![Value::Int(1), Value::Int(10), Value::Int(5)]),
            },
            TupleRecord {
                tuple_id: TupleId::new(3),
                heap_position: 3,
                row: Row::new(vec![Value::Int(1), Value::Int(10), Value::Int(0)]),
            },
            TupleRecord {
                tuple_id: TupleId::new(4),
                heap_position: 4,
                row: Row::new(vec![Value::Int(2), Value::Int(10), Value::Int(8)]),
            },
        ],
    );

    let filter = TypedExpr::logical_and(
        TypedExpr::logical_and(
            TypedExpr::binary_eq(
                TypedExpr::column_ref("tenant_id", 0, DataType::Int, false),
                TypedExpr::literal(Value::Int(1), DataType::Int, false),
            ),
            TypedExpr::binary_eq(
                TypedExpr::column_ref("category_id", 1, DataType::Int, false),
                TypedExpr::literal(Value::Int(10), DataType::Int, false),
            ),
        ),
        TypedExpr::binary_gt(
            TypedExpr::column_ref("stock_qty", 2, DataType::Int, false),
            TypedExpr::literal(Value::Int(0), DataType::Int, false),
        ),
    );

    let update_plan = PhysicalPlan::UpdateTable {
        table_id,
        assignments: vec![UpdateAssignment {
            column_ordinal: 2,
            data_type: DataType::Int,
            nullable: false,
            expr: TypedExpr::literal(Value::Int(99), DataType::Int, false),
        }],
        filter: Some(filter),
        returning: Vec::new(),
        from_table_ids: Vec::new(),
    };

    let result = executor
        .execute(&update_plan, &ctx)
        .expect("bitmap-and update");
    assert_eq!(
        result,
        ExecutionResult::Command {
            tag: "UPDATE".to_owned(),
            rows_affected: 1,
        }
    );
    assert_eq!(storage.scan_index_count(), 2);
    assert_eq!(storage.scan_table_count(), 0);

    let rows = storage.tables.lock().unwrap();
    let table_rows = rows.get(&table_id).expect("table rows");
    let by_tid: std::collections::HashMap<_, _> = table_rows.iter().cloned().collect();
    assert_eq!(
        by_tid.get(&TupleId::new(1)).expect("row 1").values,
        vec![Value::Int(1), Value::Int(10), Value::Int(99)]
    );
    assert_eq!(
        by_tid.get(&TupleId::new(2)).expect("row 2").values,
        vec![Value::Int(1), Value::Int(11), Value::Int(4)]
    );
    assert_eq!(
        by_tid.get(&TupleId::new(3)).expect("row 3").values,
        vec![Value::Int(1), Value::Int(10), Value::Int(0)]
    );
    assert_eq!(
        by_tid.get(&TupleId::new(4)).expect("row 4").values,
        vec![Value::Int(2), Value::Int(10), Value::Int(8)]
    );
}

#[test]
fn aggregate_group_by_single_column_uses_ordered_index_stream() {
    let (executor, catalog, storage) = make_executor_with_index_storage();
    let ctx = default_context();
    let table_id = create_test_table(
        &executor,
        &catalog,
        "t_agg_group_stream",
        vec![
            ColumnPlan {
                name: "group_col".to_string(),
                data_type: DataType::Int,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                has_default: false,
            },
            ColumnPlan {
                name: "payload".to_string(),
                data_type: DataType::Int,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                has_default: false,
            },
        ],
    );
    let table = catalog
        .tables
        .lock()
        .unwrap()
        .iter()
        .find(|table| table.table_id == table_id)
        .cloned()
        .expect("table descriptor");

    let index_id = IndexId::new(124);
    catalog.indexes.lock().unwrap().push(IndexDescriptor {
        index_id,
        schema_id: SchemaId::new(1),
        table_id,
        name: QualifiedName::qualified("public", "idx_t_agg_group_stream_group_col"),
        unique: false,
        nulls_not_distinct: false,
        kind: IndexKind::BTree,
        key_columns: vec![IndexKeyColumn {
            column_id: table.columns[0].column_id,
            sort_order: SortOrder::Ascending,
            nulls_first: false,
        }],
        include_columns: Vec::new(),
        constraint_name: None,
        hnsw_params: None,
    });

    {
        let mut tables = storage.tables.lock().unwrap();
        let rows = tables.entry(table_id).or_default();
        rows.push((
            TupleId::new(1),
            Row::new(vec![Value::Int(1), Value::Int(10)]),
        ));
        rows.push((
            TupleId::new(2),
            Row::new(vec![Value::Int(1), Value::Int(20)]),
        ));
        rows.push((
            TupleId::new(3),
            Row::new(vec![Value::Int(2), Value::Int(30)]),
        ));
    }
    storage.set_index_rows(
        index_id,
        vec![
            TupleRecord {
                tuple_id: TupleId::new(1),
                heap_position: 1,
                row: Row::new(vec![Value::Int(1), Value::Int(10)]),
            },
            TupleRecord {
                tuple_id: TupleId::new(2),
                heap_position: 2,
                row: Row::new(vec![Value::Int(1), Value::Int(20)]),
            },
            TupleRecord {
                tuple_id: TupleId::new(3),
                heap_position: 3,
                row: Row::new(vec![Value::Int(2), Value::Int(30)]),
            },
        ],
    );

    let plan = PhysicalPlan::Aggregate {
        table_id,
        group_by: vec![TypedExpr::column_ref("group_col", 0, DataType::Int, false)],
        grouping_sets: Vec::new(),
        aggregates: vec![make_projection_expr(
            "count",
            DataType::BigInt,
            false,
            TypedExpr::agg_count(None),
        )],
        having: None,
        filter: None,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
        access_path: ScanAccessPath::IndexRange {
            index_id,
            lower: std::ops::Bound::Unbounded,
            upper: std::ops::Bound::Unbounded,
        },
    };

    let result = executor.execute(&plan, &ctx).expect("aggregate query");
    match result {
        ExecutionResult::Query { rows, .. } => {
            assert_eq!(
                rows,
                vec![
                    Row::new(vec![Value::BigInt(2)]),
                    Row::new(vec![Value::BigInt(1)])
                ]
            );
        }
        other => panic!("expected query result, got {other:?}"),
    }
    assert_eq!(storage.scan_index_count(), 1);
    assert_eq!(storage.scan_index_ordered_count(), 0);
    assert_eq!(storage.scan_table_count(), 0);
}

// =========================================================================

#[test]
fn shortest_path_fallback_uses_endpoint_index_before_edge_table_scan() {
    use aiondb_catalog::{IndexKeyColumn, IndexKind, SortOrder};
    use aiondb_plan::graph::{
        CypherMatchClause, CypherNodePattern, CypherPathFunction, CypherPattern,
        CypherPropertyExpr, CypherRelDirection, CypherRelPattern,
    };
    use aiondb_plan::graph::CypherQueryPlan;

    let lit_int = |value: i32| TypedExpr::literal(Value::Int(value), DataType::Int, false);

    let (executor, catalog, storage) = make_executor_with_index_storage();
    let ctx = default_context();

    let person_id = create_test_table(
        &executor,
        &catalog,
        "person_shortest_idx",
        vec![
            ColumnPlan {
                name: "id".to_string(),
                data_type: DataType::Int,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                has_default: false,
            },
            ColumnPlan {
                name: "name".to_string(),
                data_type: DataType::Text,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                has_default: false,
            },
        ],
    );
    let knows_id = create_test_table(
        &executor,
        &catalog,
        "knows_shortest_idx",
        vec![
            ColumnPlan {
                name: "source_id".to_string(),
                data_type: DataType::Int,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                has_default: false,
            },
            ColumnPlan {
                name: "target_id".to_string(),
                data_type: DataType::Int,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                has_default: false,
            },
            ColumnPlan {
                name: "weight".to_string(),
                data_type: DataType::Int,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                has_default: false,
            },
        ],
    );

    storage
        .insert(
            ctx.txn_id,
            person_id,
            Row::new(vec![Value::Int(1), Value::Text("A".to_owned())]),
        )
        .expect("insert person 1");
    storage
        .insert(
            ctx.txn_id,
            person_id,
            Row::new(vec![Value::Int(2), Value::Text("B".to_owned())]),
        )
        .expect("insert person 2");
    let edge_tid = storage
        .insert(
            ctx.txn_id,
            knows_id,
            Row::new(vec![Value::Int(1), Value::Int(2), Value::Int(10)]),
        )
        .expect("insert edge");

    let edge_table = catalog
        .tables
        .lock()
        .unwrap()
        .iter()
        .find(|table| table.table_id == knows_id)
        .cloned()
        .expect("edge table descriptor");
    let source_column_id = edge_table.columns[0].column_id;
    let source_index_id = IndexId::new(9901);
    catalog.indexes.lock().unwrap().push(IndexDescriptor {
        index_id: source_index_id,
        schema_id: SchemaId::new(1),
        table_id: knows_id,
        name: QualifiedName::qualified("public", "idx_knows_shortest_idx_source"),
        unique: false,
        nulls_not_distinct: false,
        kind: IndexKind::BTree,
        key_columns: vec![IndexKeyColumn {
            column_id: source_column_id,
            sort_order: SortOrder::Ascending,
            nulls_first: false,
        }],
        include_columns: Vec::new(),
        constraint_name: None,
        hnsw_params: None,
    });
    storage.set_index_rows(
        source_index_id,
        vec![TupleRecord {
            tuple_id: edge_tid,
            heap_position: edge_tid.get(),
            row: Row::new(vec![Value::Int(1), Value::Int(2), Value::Int(10)]),
        }],
    );

    let plan = PhysicalPlan::CypherQuery(Box::new(CypherQueryPlan {
        pipeline: vec![],
        matches: vec![CypherMatchClause {
            optional: false,
            patterns: vec![CypherPattern {
                path_function: Some(CypherPathFunction::ShortestPath),
                path_variable: None,
                nodes: vec![
                    CypherNodePattern {
                        variable: Some("a".to_owned()),
                        label: None,
                        table_id: Some(person_id),
                        properties: vec![CypherPropertyExpr {
                            key: "id".to_owned(),
                            value: lit_int(1),
                        }],
                        index_scan: None,
                        range_pushdown: Vec::new(),
                    },
                    CypherNodePattern {
                        variable: Some("b".to_owned()),
                        label: None,
                        table_id: Some(person_id),
                        properties: vec![CypherPropertyExpr {
                            key: "id".to_owned(),
                            value: lit_int(2),
                        }],
                        index_scan: None,
                        range_pushdown: Vec::new(),
                    },
                ],
                relationships: vec![CypherRelPattern {
                    variable: Some("r".to_owned()),
                    rel_type: None,
                    rel_type_alternatives: Vec::new(),
                    table_id: Some(knows_id),
                    direction: CypherRelDirection::Outgoing,
                    properties: vec![],
                    min_hops: Some(1),
                    max_hops: Some(1),
                    index_scan: None,
                }],
            }],
            filter: None,
        }],
        creates: vec![],
        merges: vec![],
        sets: vec![],
        deletes: vec![],
        returns: vec![ProjectionExpr {
            expr: TypedExpr::column_ref("r.weight", 6, DataType::Int, true),
            field: ResultField {
                name: "r.weight".to_owned(),
                data_type: DataType::Int,
                text_type_modifier: None,
                nullable: true,
            },
        }],
        order_by: vec![],
        skip: None,
        limit: None,
        distinct: false,
        union: None,
    }));

    let result = executor.execute(&plan, &ctx).expect("execute shortest path");
    match result {
        ExecutionResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::Int(10));
        }
        other => panic!("expected query result, got {other:?}"),
    }

    assert_eq!(storage.scan_index_count_for(source_index_id), 1);
    assert_eq!(storage.scan_table_count_for(knows_id), 0);
}
