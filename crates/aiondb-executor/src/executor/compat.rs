use super::*;

impl Executor {
    pub(super) fn compat_scan_row(
        &self,
        record: &TupleRecord,
        include_oid_system_column: bool,
        table_id: Option<RelationId>,
    ) -> Row {
        // Pre-size the destination vec for `len + COMPAT_SYSTEM_COLUMN_COUNT`
        // up-front to avoid the realloc that `Vec::clone()` + `reserve(7)`
        // performs (clone produces a Vec sized exactly to len, and the
        // subsequent system-column appends grow capacity by 7).
        let row_len = record.row.values.len();
        let mut values = Vec::with_capacity(row_len + Self::COMPAT_SYSTEM_COLUMN_COUNT);
        values.extend_from_slice(&record.row.values);
        Self::append_compat_system_columns(
            &mut values,
            record.heap_position,
            include_oid_system_column,
            table_id,
        );
        Row::new(values)
    }

    /// Consuming variant of [`compat_scan_row`] - moves the storage row's
    /// values vector instead of cloning it. Saves a per-row Vec
    /// allocation + per-value clone on hot scan paths (notably the join
    /// child materialiser, where every row would otherwise pay this twice
    /// for the build side).
    pub(super) fn compat_scan_row_consume(
        &self,
        record: TupleRecord,
        include_oid_system_column: bool,
        table_id: Option<RelationId>,
    ) -> Row {
        let TupleRecord {
            heap_position, row, ..
        } = record;
        let mut values = row.values;
        Self::append_compat_system_columns(
            &mut values,
            heap_position,
            include_oid_system_column,
            table_id,
        );
        Row::new(values)
    }

    fn append_compat_system_columns(
        values: &mut Vec<Value>,
        heap_position: u64,
        include_oid_system_column: bool,
        table_id: Option<RelationId>,
    ) {
        values.reserve(Self::COMPAT_SYSTEM_COLUMN_COUNT);
        let tableoid_value = match table_id {
            Some(id) if id.get() > 0 => Value::Int(
                i32::try_from(id.get())
                    .unwrap_or(i32::MAX)
                    .saturating_add(16_384),
            ),
            _ => Value::Null,
        };
        values.push(Value::Tid(Self::compat_tid_from_heap_position(
            heap_position,
        )));
        values.push(tableoid_value);
        values.push(Value::Null);
        values.push(Value::Null);
        values.push(Value::Null);
        values.push(Value::Null);
        if include_oid_system_column {
            values.push(Value::Null);
        }
    }

    pub(super) fn compat_scan_row_for_table_id(
        &self,
        context: &ExecutionContext,
        table_id: RelationId,
        record: &TupleRecord,
    ) -> DbResult<Row> {
        let include_oid_system_column =
            self.compat_include_oid_system_column_for_table_id(context, table_id)?;
        Ok(self.compat_scan_row(record, include_oid_system_column, Some(table_id)))
    }

    pub(super) fn compat_include_oid_system_column_for_table_id(
        &self,
        context: &ExecutionContext,
        table_id: RelationId,
    ) -> DbResult<bool> {
        Ok(!self.relation_has_explicit_oid(context, table_id)?)
    }

    pub(super) fn compat_row_width_for_table_id(
        &self,
        context: &ExecutionContext,
        table_id: RelationId,
    ) -> DbResult<usize> {
        if let Some(cached) = context
            .compat_row_width_cache
            .lock()
            .map_err(|e| DbError::internal(format!("compat row width cache lock poisoned: {e}")))?
            .get(&table_id)
            .copied()
        {
            return Ok(cached);
        }

        let table = self
            .catalog_reader
            .get_table_by_id(context.txn_id, table_id)?
            .or_else(|| {
                aiondb_planner::virtual_synthetic_relation_compat_info(table_id.get()).map(
                    |(width, _)| TableDescriptor {
                        table_id,
                        schema_id: SchemaId::new(0),
                        name: QualifiedName::unqualified("?virtual"),
                        columns: (0..width)
                            .map(|ordinal| ColumnDescriptor {
                                column_id: ColumnId::new(
                                    u64::try_from(ordinal).unwrap_or(u64::MAX).saturating_add(1),
                                ),
                                name: format!("?column{}", ordinal.saturating_add(1)),
                                data_type: DataType::Text,
                                raw_type_name: None,
                                text_type_modifier: None,
                                nullable: true,
                                ordinal_position: u32::try_from(ordinal)
                                    .unwrap_or(u32::MAX)
                                    .saturating_add(1),
                                default_value: None,
                            })
                            .collect(),
                        primary_key: None,
                        foreign_keys: Vec::new(),
                        check_constraints: Vec::new(),
                        shard_config: None,
                        identity_columns: Vec::new(),
                        owner: None,
                    },
                )
            })
            .ok_or_else(|| {
                DbError::internal(format!(
                    "executor expected physical table metadata for relation id {table_id:?}"
                ))
            })?;
        let system_width = if self.relation_has_explicit_oid(context, table_id)? {
            Self::COMPAT_SYSTEM_COLUMN_COUNT.saturating_sub(1)
        } else {
            Self::COMPAT_SYSTEM_COLUMN_COUNT
        };
        let width = table.columns.len().saturating_add(system_width);
        context
            .compat_row_width_cache
            .lock()
            .map_err(|e| DbError::internal(format!("compat row width cache lock poisoned: {e}")))?
            .insert(table_id, width);
        Ok(width)
    }

    pub(super) fn relation_has_explicit_oid(
        &self,
        context: &ExecutionContext,
        table_id: RelationId,
    ) -> DbResult<bool> {
        if let Some(cached) = context
            .relation_has_explicit_oid_cache
            .lock()
            .map_err(|e| DbError::internal(format!("relation oid cache lock poisoned: {e}")))?
            .get(&table_id)
            .copied()
        {
            return Ok(cached);
        }

        if let Some((_, has_explicit_oid)) =
            aiondb_planner::virtual_synthetic_relation_compat_info(table_id.get())
        {
            context
                .relation_has_explicit_oid_cache
                .lock()
                .map_err(|e| DbError::internal(format!("relation oid cache lock poisoned: {e}")))?
                .insert(table_id, has_explicit_oid);
            return Ok(has_explicit_oid);
        }

        let table = self
            .catalog_reader
            .get_table_by_id(context.txn_id, table_id)?
            .ok_or_else(|| {
                DbError::internal(format!(
                    "executor expected physical table metadata for relation id {table_id:?}"
                ))
            })?;
        let has_explicit_oid = table
            .columns
            .iter()
            .any(|column| column.name.eq_ignore_ascii_case("oid"));

        context
            .relation_has_explicit_oid_cache
            .lock()
            .map_err(|e| DbError::internal(format!("relation oid cache lock poisoned: {e}")))?
            .insert(table_id, has_explicit_oid);

        Ok(has_explicit_oid)
    }

    pub(super) fn compat_tid_from_heap_position(heap_position: u64) -> TidValue {
        let zero_based = heap_position.saturating_sub(1);
        let block = u32::try_from(zero_based / Self::COMPAT_TID_PAGE_WIDTH).unwrap_or(u32::MAX);
        let offset = u16::try_from((zero_based % Self::COMPAT_TID_PAGE_WIDTH).saturating_add(1))
            .unwrap_or(u16::MAX);
        TidValue::new(block, offset)
    }
}
