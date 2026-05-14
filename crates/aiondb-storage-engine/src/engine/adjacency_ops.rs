use super::*;

impl InMemoryStorage {
    /// Register an edge table for adjacency index maintenance.
    ///
    /// After registration, every insert/update/delete on this table will
    /// automatically buffer adjacency changes in the pending transaction
    /// and apply them atomically at commit time.
    pub(crate) fn register_edge_table(
        &self,
        table_id: RelationId,
        source_col_idx: usize,
        target_col_idx: usize,
    ) -> DbResult<()> {
        let mut state = self.write_state()?;
        state.edge_table_registrations.insert(
            table_id,
            EdgeTableRegistration {
                source_col_idx,
                target_col_idx,
            },
        );
        state
            .edge_table_endpoints
            .insert(table_id, (source_col_idx, target_col_idx));
        let mut backfill = Vec::new();
        if let Some(table) = state.tables.get(&table_id) {
            for tuple_id in table.tuple_ids() {
                if let Some(row) = table.load_latest_row(&state.overflow, tuple_id)? {
                    let source_id = row
                        .values
                        .get(source_col_idx)
                        .cloned()
                        .unwrap_or(Value::Null);
                    let target_id = row
                        .values
                        .get(target_col_idx)
                        .cloned()
                        .unwrap_or(Value::Null);
                    backfill.push((source_id, target_id, tuple_id));
                }
            }
        }
        let index = state.adjacency_indexes.entry(table_id).or_default();
        for (source_id, target_id, tuple_id) in backfill {
            index.insert(source_id, target_id, tuple_id);
        }
        Ok(())
    }

    /// Unregister an edge table, removing its adjacency index.
    pub(crate) fn unregister_edge_table(&self, table_id: RelationId) -> DbResult<()> {
        let mut state = self.write_state()?;
        state.edge_table_registrations.remove(&table_id);
        state.edge_table_endpoints.remove(&table_id);
        state.adjacency_indexes.remove(&table_id);
        Ok(())
    }

    /// Return edge tuple IDs for edges originating from `source_id` in
    /// the given edge table, respecting transaction visibility.
    ///
    /// For a transaction with pending changes, the committed index is
    /// overlaid with the pending inserts and removals.
    pub fn adjacency_outgoing(
        &self,
        txn: TxnId,
        table_id: RelationId,
        source_id: &Value,
    ) -> DbResult<Vec<TupleId>> {
        let state = self.read_state()?;
        self.adjacency_lookup_with_pending(&state, txn, table_id, source_id, true)
    }

    /// Return edge tuple IDs for edges arriving at `target_id` in the
    /// given edge table, respecting transaction visibility.
    pub fn adjacency_incoming(
        &self,
        txn: TxnId,
        table_id: RelationId,
        target_id: &Value,
    ) -> DbResult<Vec<TupleId>> {
        let state = self.read_state()?;
        self.adjacency_lookup_with_pending(&state, txn, table_id, target_id, false)
    }

    /// Return all edge tuple IDs incident on `node_id` (either as source
    /// or target) across all registered edge tables, respecting
    /// transaction visibility.
    pub fn adjacency_incident(
        &self,
        txn: TxnId,
        node_id: &Value,
    ) -> DbResult<Vec<(RelationId, TupleId)>> {
        let state = self.read_state()?;
        let mut seen = std::collections::HashSet::new();
        let mut results = Vec::new();
        for &table_id in state.edge_table_registrations.keys() {
            let outgoing =
                self.adjacency_lookup_with_pending(&state, txn, table_id, node_id, true)?;
            let incoming =
                self.adjacency_lookup_with_pending(&state, txn, table_id, node_id, false)?;
            for tid in outgoing {
                if seen.insert((table_id, tid)) {
                    results.push((table_id, tid));
                }
            }
            for tid in incoming {
                if seen.insert((table_id, tid)) {
                    results.push((table_id, tid));
                }
            }
        }
        Ok(results)
    }

    /// Internal: look up adjacency with pending transaction overlay.
    pub(super) fn adjacency_lookup_with_pending(
        &self,
        state: &StorageState,
        txn: TxnId,
        table_id: RelationId,
        node_id: &Value,
        is_outgoing: bool,
    ) -> DbResult<Vec<TupleId>> {
        // Start with committed adjacency.
        let committed = state.adjacency_indexes.get(&table_id);
        let mut result: Vec<TupleId> = match committed {
            Some(index) => {
                if is_outgoing {
                    index.outgoing(node_id).to_vec()
                } else {
                    index.incoming(node_id).to_vec()
                }
            }
            None => Vec::new(),
        };

        // Overlay pending changes from this transaction.
        if !Self::is_autocommit_txn(txn) {
            if let Some(pending) = state.active_txns.get(&txn) {
                for change in &pending.pending_adjacency {
                    if change.table_id != table_id {
                        continue;
                    }
                    let matches = if is_outgoing {
                        adjacency::values_equal(&change.source_id, node_id)
                    } else {
                        adjacency::values_equal(&change.target_id, node_id)
                    };
                    if matches {
                        match change.operation {
                            AdjacencyOp::Insert => {
                                if !result.contains(&change.edge_tuple_id) {
                                    result.push(change.edge_tuple_id);
                                }
                            }
                            AdjacencyOp::Remove => {
                                result.retain(|id| *id != change.edge_tuple_id);
                            }
                        }
                    }
                }
            }
        }

        Ok(result)
    }

    pub(super) fn adjacency_neighbors_with_pending(
        &self,
        state: &StorageState,
        txn: TxnId,
        table_id: RelationId,
        node_id: &Value,
        is_outgoing: bool,
    ) -> DbResult<Vec<Value>> {
        let committed = state.adjacency_indexes.get(&table_id);
        let mut result: Vec<Value> = match committed {
            Some(index) => {
                if is_outgoing {
                    index.outgoing_targets(node_id)
                } else {
                    index.incoming_sources(node_id)
                }
            }
            None => Vec::new(),
        };

        if !Self::is_autocommit_txn(txn) {
            if let Some(pending) = state.active_txns.get(&txn) {
                for change in &pending.pending_adjacency {
                    if change.table_id != table_id {
                        continue;
                    }
                    let matches = if is_outgoing {
                        adjacency::values_equal(&change.source_id, node_id)
                    } else {
                        adjacency::values_equal(&change.target_id, node_id)
                    };
                    if matches {
                        let neighbor = if is_outgoing {
                            change.target_id.clone()
                        } else {
                            change.source_id.clone()
                        };
                        match change.operation {
                            AdjacencyOp::Insert => result.push(neighbor),
                            AdjacencyOp::Remove => {
                                result.retain(|value| !adjacency::values_equal(value, &neighbor));
                            }
                        }
                    }
                }
            }
        }

        Ok(result)
    }

    /// Extract source and target values from a row using the registered
    /// edge table column indices.
    pub(super) fn extract_edge_endpoints(
        state: &StorageState,
        table_id: RelationId,
        row: &Row,
    ) -> Option<(Value, Value)> {
        let reg = state.edge_table_registrations.get(&table_id)?;
        let source = row
            .values
            .get(reg.source_col_idx)
            .cloned()
            .unwrap_or(Value::Null);
        let target = row
            .values
            .get(reg.target_col_idx)
            .cloned()
            .unwrap_or(Value::Null);
        Some((source, target))
    }

    /// Buffer a pending adjacency insert for a transaction.
    pub(super) fn buffer_adjacency_insert(
        pending: &mut PendingTransaction,
        table_id: RelationId,
        source_id: Value,
        target_id: Value,
        edge_tuple_id: TupleId,
    ) {
        Self::buffer_adjacency_change(
            pending,
            table_id,
            source_id,
            target_id,
            edge_tuple_id,
            AdjacencyOp::Insert,
        );
    }

    /// Buffer a pending adjacency removal for a transaction.
    pub(super) fn buffer_adjacency_remove(
        pending: &mut PendingTransaction,
        table_id: RelationId,
        source_id: Value,
        target_id: Value,
        edge_tuple_id: TupleId,
    ) {
        Self::buffer_adjacency_change(
            pending,
            table_id,
            source_id,
            target_id,
            edge_tuple_id,
            AdjacencyOp::Remove,
        );
    }

    fn buffer_adjacency_change(
        pending: &mut PendingTransaction,
        table_id: RelationId,
        source_id: Value,
        target_id: Value,
        edge_tuple_id: TupleId,
        operation: AdjacencyOp,
    ) {
        pending.pending_adjacency.push(PendingAdjacencyChange {
            table_id,
            source_id,
            target_id,
            edge_tuple_id,
            operation,
        });
    }

    /// Apply pending adjacency changes to the committed index.
    pub(super) fn apply_pending_adjacency(
        state: &mut StorageState,
        changes: Vec<PendingAdjacencyChange>,
    ) {
        for change in changes {
            let index = state.adjacency_indexes.entry(change.table_id).or_default();
            match change.operation {
                AdjacencyOp::Insert => {
                    index.insert(change.source_id, change.target_id, change.edge_tuple_id);
                }
                AdjacencyOp::Remove => {
                    index.remove(change.source_id, change.target_id, change.edge_tuple_id);
                }
            }
        }
    }

    /// If `table_id` is a registered edge table, insert the edge into the
    /// adjacency index.
    pub(super) fn adjacency_insert(
        state: &mut StorageState,
        table_id: RelationId,
        tuple_id: TupleId,
        row: &Row,
    ) {
        Self::adjacency_mutate(state, table_id, tuple_id, row, AdjacencyOp::Insert);
    }

    /// If `table_id` is a registered edge table, remove the edge from the
    /// adjacency index.
    pub(super) fn adjacency_remove(
        state: &mut StorageState,
        table_id: RelationId,
        tuple_id: TupleId,
        row: &Row,
    ) {
        Self::adjacency_mutate(state, table_id, tuple_id, row, AdjacencyOp::Remove);
    }

    fn adjacency_mutate(
        state: &mut StorageState,
        table_id: RelationId,
        tuple_id: TupleId,
        row: &Row,
        op: AdjacencyOp,
    ) {
        let Some(&(src_col, tgt_col)) = state.edge_table_endpoints.get(&table_id) else {
            return;
        };
        if src_col >= row.values.len() || tgt_col >= row.values.len() {
            return;
        }
        let source = row.values[src_col].clone();
        let target = row.values[tgt_col].clone();
        if let Some(adj) = state.adjacency_indexes.get_mut(&table_id) {
            match op {
                AdjacencyOp::Insert => adj.insert(source, target, tuple_id),
                AdjacencyOp::Remove => adj.remove(source, target, tuple_id),
            }
        }
    }

    // -------------------------------------------------------------------
    // Pending HNSW index changes (transactional buffering)
    // -------------------------------------------------------------------

    /// Collect committed HNSW index IDs for the given table. This must be
    /// called before borrowing `pending` mutably out of the state.
    pub(super) fn committed_hnsw_index_ids(
        state: &StorageState,
        table_id: RelationId,
    ) -> Vec<IndexId> {
        state
            .hnsw_indexes
            .iter()
            .filter_map(|(index_id, index)| {
                (index.descriptor.table_id == table_id).then_some(*index_id)
            })
            .collect()
    }

    /// Buffer pending HNSW inserts for committed HNSW indexes. The `hnsw_ids`
    /// must be obtained via [`committed_hnsw_index_ids`] before the mutable
    /// borrow of `pending`.
    pub(super) fn push_pending_hnsw_inserts(
        pending: &mut PendingTransaction,
        hnsw_ids: &[IndexId],
        table_id: RelationId,
        tuple_id: TupleId,
        row: &Row,
    ) {
        Self::push_pending_hnsw_changes(pending, hnsw_ids, table_id, tuple_id, row, HnswOp::Insert);
    }

    /// Buffer pending HNSW removals for committed HNSW indexes. The `hnsw_ids`
    /// must be obtained via [`committed_hnsw_index_ids`] before the mutable
    /// borrow of `pending`.
    pub(super) fn push_pending_hnsw_removes(
        pending: &mut PendingTransaction,
        hnsw_ids: &[IndexId],
        table_id: RelationId,
        tuple_id: TupleId,
        row: &Row,
    ) {
        Self::push_pending_hnsw_changes(pending, hnsw_ids, table_id, tuple_id, row, HnswOp::Remove);
    }

    fn push_pending_hnsw_changes(
        pending: &mut PendingTransaction,
        hnsw_ids: &[IndexId],
        table_id: RelationId,
        tuple_id: TupleId,
        row: &Row,
        operation: HnswOp,
    ) {
        for &index_id in hnsw_ids {
            pending.pending_hnsw.push(PendingHnswChange {
                index_id,
                table_id,
                tuple_id,
                row: row.clone(),
                operation,
            });
        }
    }

    /// Apply pending HNSW changes to the committed indexes.
    pub(super) fn apply_pending_hnsw(
        state: &mut StorageState,
        changes: Vec<PendingHnswChange>,
    ) -> DbResult<()> {
        for change in changes {
            let table_descriptor = state
                .tables
                .get(&change.table_id)
                .map(|table| table.descriptor.clone());
            let Some(table_descriptor) = table_descriptor else {
                continue;
            };
            let Some(index) = state.hnsw_indexes.get_mut(&change.index_id) else {
                continue;
            };
            match change.operation {
                HnswOp::Insert => {
                    index.insert_tuple(&table_descriptor, change.tuple_id, &change.row)?;
                }
                HnswOp::Remove => {
                    index.remove_tuple(&table_descriptor, change.tuple_id, &change.row)?;
                }
            }
        }
        Ok(())
    }
}

impl StorageCapabilities for InMemoryStorage {
    fn supports_vector_search(&self) -> bool {
        true
    }

    fn supports_gin_search(&self) -> bool {
        true
    }

    fn supports_savepoints(&self) -> bool {
        true
    }

    fn supports_durability(&self) -> bool {
        self.wal.is_some()
    }

    fn supports_persistent_ordered_indexes(&self) -> bool {
        self.disk_index_pool.is_some()
    }

    fn supports_vacuum(&self) -> bool {
        true
    }

    fn supports_statistics_logging(&self) -> bool {
        self.wal.is_some()
    }
}
