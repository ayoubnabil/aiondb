impl Executor {
    // -----------------------------------------------------------------------
    // Auto-creation helpers for Cypher CREATE
    // -----------------------------------------------------------------------

    /// Infer a SQL `DataType` from a runtime `Value`.
    fn infer_type_from_value(value: &Value) -> DataType {
        match value {
            Value::Int(_) | Value::BigInt(_) => DataType::BigInt,
            Value::Real(_) | Value::Double(_) => DataType::Double,
            // Cypher numeric literals (`5.025648`) come through as Numeric
            // first; mapping to Numeric keeps fidelity, but the Cypher TCK
            // expects unquoted floats, so prefer Double for storage.
            Value::Numeric(_) => DataType::Double,
            Value::Boolean(_) => DataType::Boolean,
            Value::Date(_) => DataType::Date,
            Value::Time(_) => DataType::Time,
            Value::TimeTz(_, _) => DataType::TimeTz,
            Value::Timestamp(_) => DataType::Timestamp,
            Value::TimestampTz(_) => DataType::TimestampTz,
            Value::Interval(_) => DataType::Interval,
            _ => DataType::Text,
        }
    }

    fn graph_object_schema(&self, context: &ExecutionContext) -> String {
        session_search_path_schemas(context)
            .into_iter()
            .find(|schema_name| {
                !schema_name.is_empty()
                    && !schema_name.eq_ignore_ascii_case("pg_catalog")
                    && !schema_name.eq_ignore_ascii_case("information_schema")
                    && self
                        .catalog_reader
                        .get_schema(context.txn_id, &QualifiedName::unqualified(schema_name))
                        .ok()
                        .flatten()
                        .is_some()
            })
            .unwrap_or_else(|| "public".to_owned())
    }

    /// Ensure a node label (and its backing table) exists in the catalog.
    ///
    /// If the label already exists, returns its `table_id`. Otherwise, creates a
    /// backing table with an auto-increment `id` column plus one column per
    /// property key found in `properties`, registers a node label in the catalog,
    /// and returns the new `table_id`.
    fn ensure_node_label(
        &self,
        context: &ExecutionContext,
        label: &str,
        properties: &[CypherPropertyExpr],
        binding: &BindingRow,
    ) -> DbResult<RelationId> {
        if let Some(desc) = self.catalog_reader.get_node_label(context.txn_id, label)? {
            self.ensure_columns_exist(context, desc.table_id, properties, binding)?;
            return Ok(desc.table_id);
        }

        let graph_schema = self.graph_object_schema(context);
        let table_name = label.to_lowercase();
        let qn = QualifiedName::qualified(&graph_schema, &table_name);
        // Use a SHARED node id sequence so ids are globally unique across
        // all node label tables. Without this, MATCH (a)-[r]->(b) where a/b
        // are in different label tables can falsely match because the local
        // (table-relative) id 1 collides between A.id=1 and B.id=1.
        let seq_name = "_graph_node_id_seq".to_owned();
        let seq_qn = QualifiedName::qualified(&graph_schema, &seq_name);
        let seq_default = seq_qn.to_string().replace('\'', "''");

        let mut columns = Vec::new();
        columns.push(ColumnDescriptor {
            column_id: ColumnId::default(),
            name: "id".to_owned(),
            data_type: DataType::BigInt,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: false,
            ordinal_position: 1,
            default_value: Some(format!("nextval('{seq_default}')")),
        });

        let mut seen = std::collections::HashSet::new();
        for prop in properties {
            let col_name = prop.key.to_lowercase();
            // Pre-check via `contains(&str)` so duplicates don't
            // pay the wasted `clone` that `seen.insert(_.clone())`
            // performs even when the entry already exists.
            if col_name == "id" || seen.contains(&col_name) {
                continue;
            }
            seen.insert(col_name.clone());
            let val = self.evaluate_cypher_expr_with_binding(&prop.value, binding, context)?;
            let data_type = Self::infer_type_from_value(&val);
            columns.push(ColumnDescriptor {
                column_id: ColumnId::default(),
                name: col_name,
                data_type,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: true,
                ordinal_position: len_plus_one_to_u32(columns.len()),
                default_value: None,
            });
        }

        let descriptor = TableDescriptor {
            table_id: RelationId::default(),
            schema_id: SchemaId::default(),
            name: qn.clone(),
            columns,
            identity_columns: Vec::new(),
            primary_key: Some(vec![ColumnId::new(1)]),
            foreign_keys: Vec::new(),
            check_constraints: Vec::new(),
            shard_config: None,
            owner: context.current_user_name(),
        };

        let table_id = match self.catalog_writer.create_table(context.txn_id, descriptor) {
            Ok(id) => {
                let table = self
                    .catalog_reader
                    .get_table_by_id(context.txn_id, id)?
                    .ok_or_else(|| {
                        DbError::internal("auto-created node table missing from catalog")
                    })?;
                let storage_descriptor = aiondb_schema_bridge::to_table_storage_descriptor(&table)?;
                self.storage_ddl
                    .create_table_storage(context.txn_id, &storage_descriptor)?;

                // The shared `_graph_node_id_seq` is created the first time
                // any node label table is materialised; subsequent labels
                // reuse the existing sequence. Tolerate the duplicate-create
                // error so multi-label graphs share a globally unique id
                // space.
                let seq_desc = SequenceDescriptor {
                    sequence_id: SequenceId::default(),
                    schema_id: SchemaId::default(),
                    name: seq_qn.clone(),
                    data_type: DataType::BigInt,
                    start_value: 1,
                    increment_by: 1,
                    min_value: 1,
                    max_value: i64::MAX,
                    cache_size: 1,
                    cycle: false,
                    owned_by: None,
                    owner: None,
                };
                if let Err(e) = self
                    .catalog_writer
                    .create_sequence(context.txn_id, seq_desc)
                {
                    if self
                        .catalog_reader
                        .get_sequence(context.txn_id, &seq_qn)?
                        .is_none()
                    {
                        return Err(DbError::internal(format!(
                            "failed to create graph sequence: {e}"
                        )));
                    }
                }

                for col in &table.columns {
                    if col.name == "id" {
                        continue;
                    }
                    self.try_create_graph_index(
                        context,
                        id,
                        &graph_schema,
                        &table_name,
                        &col.name,
                        col.column_id,
                    )?;
                }

                id
            }
            Err(e) => {
                debug!("node table creation returned error (likely already exists): {e}");
                let existing = self
                    .catalog_reader
                    .get_table(context.txn_id, &qn)?
                    .ok_or_else(|| {
                        DbError::internal(format!(
                            "could not create or find backing table for node label '{label}'"
                        ))
                    })?;
                existing.table_id
            }
        };

        let node_label = NodeLabelDescriptor {
            label: label.to_owned(),
            table_id,
        };
        self.catalog_writer
            .create_node_label(context.txn_id, node_label)
            .map_err(|e| {
                DbError::internal(format!("failed to create node label '{label}': {e}"))
            })?;

        Ok(table_id)
    }

    /// Ensure an edge label (and its backing table) exists in the catalog.
    ///
    /// If the label already exists, returns its `table_id`. Otherwise, creates a
    /// backing table with `source_id BIGINT, target_id BIGINT` plus one column
    /// per property key, registers an edge label, and returns the new
    /// `table_id`.
    fn ensure_edge_label(
        &self,
        context: &ExecutionContext,
        rel_type: &str,
        source_label: &str,
        target_label: &str,
        properties: &[CypherPropertyExpr],
        binding: &BindingRow,
    ) -> DbResult<RelationId> {
        if let Some(desc) = self
            .catalog_reader
            .get_edge_label(context.txn_id, rel_type)?
        {
            if desc.endpoints.is_some() {
                return Err(DbError::feature_not_supported(format!(
                    "CREATE relationships is not supported for FK-backed edge label \"{}\"; update the backing table endpoints instead",
                    desc.label
                )));
            }
            self.ensure_columns_exist(context, desc.table_id, properties, binding)?;
            return Ok(desc.table_id);
        }

        let graph_schema = self.graph_object_schema(context);
        let table_name = rel_type.to_lowercase();
        let qn = QualifiedName::qualified(&graph_schema, &table_name);

        let mut columns = Vec::new();
        columns.push(ColumnDescriptor {
            column_id: ColumnId::default(),
            name: "source_id".to_owned(),
            data_type: DataType::BigInt,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: true,
            ordinal_position: 1,
            default_value: None,
        });
        columns.push(ColumnDescriptor {
            column_id: ColumnId::default(),
            name: "target_id".to_owned(),
            data_type: DataType::BigInt,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: true,
            ordinal_position: 2,
            default_value: None,
        });

        let mut seen = std::collections::HashSet::new();
        for prop in properties {
            let col_name = prop.key.to_lowercase();
            if col_name == "source_id"
                || col_name == "target_id"
                || seen.contains(&col_name)
            {
                continue;
            }
            seen.insert(col_name.clone());
            let val = self.evaluate_cypher_expr_with_binding(&prop.value, binding, context)?;
            let data_type = Self::infer_type_from_value(&val);
            columns.push(ColumnDescriptor {
                column_id: ColumnId::default(),
                name: col_name,
                data_type,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: true,
                ordinal_position: len_plus_one_to_u32(columns.len()),
                default_value: None,
            });
        }

        let descriptor = TableDescriptor {
            table_id: RelationId::default(),
            schema_id: SchemaId::default(),
            name: qn.clone(),
            columns,
            identity_columns: Vec::new(),
            primary_key: None,
            foreign_keys: Vec::new(),
            check_constraints: Vec::new(),
            shard_config: None,
            owner: context.current_user_name(),
        };

        let table_id = match self.catalog_writer.create_table(context.txn_id, descriptor) {
            Ok(id) => {
                let table = self
                    .catalog_reader
                    .get_table_by_id(context.txn_id, id)?
                    .ok_or_else(|| {
                        DbError::internal("auto-created edge table missing from catalog")
                    })?;
                let storage_descriptor = aiondb_schema_bridge::to_table_storage_descriptor(&table)?;
                self.storage_ddl
                    .create_table_storage(context.txn_id, &storage_descriptor)?;

                for col in &table.columns {
                    if col.name == "source_id" || col.name == "target_id" {
                        self.try_create_graph_index(
                            context,
                            id,
                            &graph_schema,
                            &table_name,
                            &col.name,
                            col.column_id,
                        )?;
                    }
                }

                id
            }
            Err(e) => {
                debug!("edge table creation returned error (likely already exists): {e}");
                let existing = self
                    .catalog_reader
                    .get_table(context.txn_id, &qn)?
                    .ok_or_else(|| {
                        DbError::internal(format!(
                            "could not create or find backing table for edge label '{rel_type}'"
                        ))
                    })?;
                existing.table_id
            }
        };

        let edge_label = EdgeLabelDescriptor {
            label: rel_type.to_owned(),
            table_id,
            source_label: source_label.to_owned(),
            target_label: target_label.to_owned(),
            endpoints: None,
        };
        self.catalog_writer
            .create_edge_label(context.txn_id, edge_label)
            .map_err(|e| {
                DbError::internal(format!("failed to create edge label '{rel_type}': {e}"))
            })?;

        Ok(table_id)
    }

    /// Ensure that the backing table for a label has columns for all the given
    /// property keys. If a column is missing, add it via ALTER TABLE.
    fn ensure_columns_exist(
        &self,
        context: &ExecutionContext,
        table_id: RelationId,
        properties: &[CypherPropertyExpr],
        binding: &BindingRow,
    ) -> DbResult<()> {
        if properties.is_empty() {
            return Ok(());
        }
        let table = self
            .catalog_reader
            .get_table_by_id(context.txn_id, table_id)?
            .ok_or_else(|| DbError::internal("table not found for column check"))?;
        let mut existing = std::collections::HashSet::new();
        for column in &table.columns {
            existing.insert(column.name.to_lowercase());
        }
        let mut seen = std::collections::HashSet::new();
        let mut pending = Vec::new();
        for prop in properties {
            let col_name = prop.key.to_lowercase();
            if seen.contains(&col_name) {
                continue;
            }
            seen.insert(col_name.clone());
            if existing.contains(&col_name) {
                continue;
            }

            let val = self.evaluate_cypher_expr_with_binding(&prop.value, binding, context)?;
            let data_type = Self::infer_type_from_value(&val);
            existing.insert(col_name.clone());
            pending.push((col_name, data_type));
        }
        if pending.is_empty() {
            return Ok(());
        }
        self.add_implicit_graph_columns(context, table_id, &pending)
    }

    fn add_implicit_graph_columns(
        &self,
        context: &ExecutionContext,
        table_id: RelationId,
        pending_columns: &[(String, DataType)],
    ) -> DbResult<()> {
        if pending_columns.is_empty() {
            return Ok(());
        }
        self.with_internal_rewrite_savepoint(
            context,
            "Cypher implicit graph property rewrite",
            || {
                self.lock_table(context, table_id, LockMode::AccessExclusive)?;

                let table = self
                    .catalog_reader
                    .get_table_by_id(context.txn_id, table_id)?
                    .ok_or_else(|| DbError::internal("table to alter is missing from catalog"))?;
                let mut existing =
                    std::collections::HashSet::with_capacity(table.columns.len());
                for column in &table.columns {
                    existing.insert(column.name.to_lowercase());
                }
                let mut pending = Vec::with_capacity(pending_columns.len());
                for (column_name, data_type) in pending_columns {
                    let normalized = column_name.to_lowercase();
                    // Pre-check via `contains(&str)` so the
                    // already-present case skips the wasted clone the
                    // previous `existing.insert(normalized.clone())`
                    // pattern paid.
                    if !existing.contains(&normalized) {
                        existing.insert(normalized.clone());
                        pending.push((normalized, data_type.clone()));
                    }
                }
                if pending.is_empty() {
                    return Ok(());
                }

                let base_ordinal = table.columns.len();
                for (offset, (column_name, data_type)) in pending.iter().enumerate() {
                    let ordinal = len_plus_one_to_u32(base_ordinal + offset);
                    let col_desc = ColumnDescriptor {
                        column_id: ColumnId::default(),
                        name: column_name.to_owned(),
                        data_type: data_type.clone(),
                        raw_type_name: None,
                        text_type_modifier: None,
                        nullable: true,
                        ordinal_position: ordinal,
                        default_value: None,
                    };
                    self.catalog_writer.alter_table(
                        context.txn_id,
                        table_id,
                        aiondb_catalog::TableAlteration::AddColumn(col_desc),
                    )?;
                }

                let mut stream = self.scan_table_locked(context, table_id, None)?;
                let mut rewrites = Vec::new();
                let nulls_to_append = pending.len();
                while let Some(record) = stream.next()? {
                    context.check_deadline()?;
                    let mut values = record.row.into_values();
                    values.extend(std::iter::repeat_n(Value::Null, nulls_to_append));
                    let row = Row::new(values);
                    context.track_memory(estimate_row_bytes(&row).saturating_add(64))?;
                    rewrites.push((record.tuple_id, row));
                }

                let updated_table = self
                    .catalog_reader
                    .get_table_by_id(context.txn_id, table_id)?
                    .ok_or_else(|| DbError::internal("altered table is missing from catalog"))?;
                let storage_descriptor =
                    aiondb_schema_bridge::to_table_storage_descriptor(&updated_table)?;
                self.storage_ddl
                    .alter_table_storage(context.txn_id, &storage_descriptor)?;

                for (tuple_id, row) in rewrites {
                    context.check_deadline()?;
                    self.update_locked(context, table_id, tuple_id, None, row)?;
                }

                Ok(())
            },
        )
    }

    /// Try to create a `BTree` index on a graph table column.
    ///
    /// ignore the duplicate-name error and continue.
    pub(crate) fn try_create_graph_index(
        &self,
        context: &ExecutionContext,
        table_id: RelationId,
        schema_name: &str,
        table_name: &str,
        column_name: &str,
        column_id: ColumnId,
    ) -> DbResult<()> {
        let index_name = format!("idx_{table_name}_{column_name}");
        let idx_qn = QualifiedName::qualified(schema_name, &index_name);
        let descriptor = IndexDescriptor {
            index_id: IndexId::default(),
            schema_id: SchemaId::default(),
            table_id,
            name: idx_qn,
            unique: false,
            nulls_not_distinct: false,
            kind: IndexKind::BTree,
            key_columns: vec![IndexKeyColumn {
                column_id,
                sort_order: SortOrder::Ascending,
                nulls_first: false,
            }],
            include_columns: Vec::new(),
            constraint_name: None,
            hnsw_params: None,
        };
        match self.catalog_writer.create_index(context.txn_id, descriptor) {
            Ok(index_id) => {
                if let Some(index) = self.catalog_reader.get_index(context.txn_id, index_id)? {
                    let storage_desc = to_index_storage_descriptor(&index)?;
                    self.storage_ddl
                        .create_index_storage(context.txn_id, &storage_desc)?;
                }
            }
            Err(error)
                if matches!(
                    error.sqlstate(),
                    SqlState::DuplicateObject | SqlState::UniqueViolation
                ) =>
            {
                debug!("graph index creation skipped: {error}");
            }
            Err(error) => return Err(error),
        }
        Ok(())
    }

    /// Generate the next id value for a node by looking up its sequence.
    fn generate_node_id(
        &self,
        context: &ExecutionContext,
        table_id: RelationId,
    ) -> DbResult<Value> {
        let table = self
            .catalog_reader
            .get_table_by_id(context.txn_id, table_id)?
            .ok_or_else(|| DbError::internal("table not found for id generation"))?;

        if let Some(col) = table.columns.first() {
            if let Some(ref default_expr) = col.default_value {
                if let Some(seq_name) = extract_nextval_seq(default_expr) {
                    let seq_qn = parse_qualified_name(seq_name);
                    if let Some(seq_desc) =
                        self.catalog_reader.get_sequence(context.txn_id, &seq_qn)?
                    {
                        let value = self
                            .sequence_manager
                            .next_value(context.txn_id, seq_desc.sequence_id)?;
                        return Ok(Value::BigInt(value));
                    }
                }
            }
        }
        Ok(Value::Null)
    }
}
