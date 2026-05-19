use std::{collections::HashSet, sync::Arc};

use aiondb_core::{ColumnId, DbResult, IndexId, RelationId, TupleId, Value};
use aiondb_graph::{GraphDirection, GraphStats, GraphStorage, NeighborCursor, OwnedCursor};
use aiondb_plan::graph::{CypherPropertyExpr, CypherRelDirection, CypherRelPattern};

use super::{ExecutionContext, Executor, GraphTraversalRef};
use crate::executor::graph_plans::{ensure_graph_workset_capacity, SharedRow};
use crate::executor::helpers::{estimate_row_bytes, estimate_value_bytes, exact_lookup_key_range};

fn estimate_adjacent_edge_record_bytes(
    compat_row: &aiondb_core::Row,
    raw_row: &aiondb_core::Row,
    source_id: &Value,
    target_id: &Value,
) -> u64 {
    96u64
        .saturating_add(estimate_row_bytes(compat_row))
        .saturating_add(estimate_row_bytes(raw_row))
        .saturating_add(estimate_value_bytes(source_id))
        .saturating_add(estimate_value_bytes(target_id))
}

pub(super) struct RelationshipTraversalSpec {
    pub rel: CypherRelPattern,
    pub table_id: RelationId,
    pub src_col_idx: usize,
    pub tgt_col_idx: usize,
    pub use_table_adjacency: bool,
    pub edge_rel_type: Arc<str>,
    pub edge_col_names: Arc<Vec<String>>,
    pub edge_rls_policies: Option<Vec<super::dml_plans::CompatRlsPolicy>>,
    pub projected_scan: Option<RelationshipScanProjection>,
}

pub(super) struct AdjacentEdgeRecord {
    pub compat_row: SharedRow,
    pub raw_row: SharedRow,
    pub tuple_id: TupleId,
    pub source_id: Value,
    pub target_id: Value,
    pub native_endpoints: bool,
}

pub(super) struct RelationshipScanProjection {
    pub projected_columns: Vec<ColumnId>,
    pub column_names: Arc<Vec<String>>,
    #[allow(dead_code)]
    pub adjacency_projected_columns: Vec<ColumnId>,
    #[allow(dead_code)]
    pub adjacency_column_names: Arc<Vec<String>>,
    pub src_col_idx: usize,
    pub tgt_col_idx: usize,
}

impl RelationshipScanProjection {
    pub(crate) fn fetch_projection(&self, native_endpoints: bool) -> &[ColumnId] {
        if native_endpoints {
            self.adjacency_projected_columns.as_slice()
        } else {
            self.projected_columns.as_slice()
        }
    }

    pub(crate) fn scan_column_names(&self, native_endpoints: bool) -> &[String] {
        if native_endpoints {
            self.adjacency_column_names.as_ref()
        } else {
            self.column_names.as_ref()
        }
    }
}

pub(super) struct NativeGraphTraversalStoreRef<'executor, 'context> {
    executor: &'executor Executor,
    context: &'context ExecutionContext,
    edge_table_id: RelationId,
}

impl GraphStorage for NativeGraphTraversalStoreRef<'_, '_> {
    fn stats(&self) -> GraphStats {
        self.executor
            .storage_dml
            .adjacency_index_stats(self.context.txn_id, self.edge_table_id)
            .unwrap_or(GraphStats {
                node_count: None,
                edge_count: 0,
                source_node_count: None,
                target_node_count: None,
                has_reverse_adjacency: true,
                has_weighted_adjacency: false,
                directed: true,
            })
    }

    fn edge_ids(
        &self,
        node_id: &Value,
        direction: GraphDirection,
    ) -> Box<dyn NeighborCursor<TupleId> + '_> {
        let outgoing = matches!(direction, GraphDirection::Outgoing);
        self.executor
            .storage_dml
            .adjacency_edge_cursor(
                self.context.txn_id,
                &self.context.snapshot,
                self.edge_table_id,
                node_id,
                outgoing,
            )
            .unwrap_or_else(|_| Box::new(OwnedCursor::new(Vec::new())))
    }

    fn neighbor_ids(
        &self,
        node_id: &Value,
        direction: GraphDirection,
    ) -> Box<dyn NeighborCursor<Value> + '_> {
        let outgoing = matches!(direction, GraphDirection::Outgoing);
        self.executor
            .storage_dml
            .adjacency_neighbor_cursor(
                self.context.txn_id,
                &self.context.snapshot,
                self.edge_table_id,
                node_id,
                outgoing,
            )
            .unwrap_or_else(|_| Box::new(OwnedCursor::new(Vec::new())))
    }

    fn edge_endpoints(&self, edge_id: TupleId) -> Option<(Value, Value)> {
        self.executor
            .storage_dml
            .adjacency_edge_endpoints(
                self.context.txn_id,
                &self.context.snapshot,
                self.edge_table_id,
                edge_id,
            )
            .ok()?
    }
}

impl Executor {
    pub(super) fn build_relationship_scan_projection(
        &self,
        context: &ExecutionContext,
        table_id: RelationId,
        src_col_idx: usize,
        tgt_col_idx: usize,
        edge_col_names: &[String],
        properties: &[CypherPropertyExpr],
    ) -> DbResult<Option<RelationshipScanProjection>> {
        let mut ordinals = Vec::new();
        let mut property_ordinals = Vec::new();
        let mut push_ordinal = |ordinal: usize| {
            if let Some(position) = ordinals.iter().position(|entry| *entry == ordinal) {
                position
            } else {
                let position = ordinals.len();
                ordinals.push(ordinal);
                position
            }
        };

        let projected_src_col_idx = push_ordinal(src_col_idx);
        let projected_tgt_col_idx = push_ordinal(tgt_col_idx);
        for property in properties {
            if let Some(column_idx) = edge_col_names
                .iter()
                .position(|name| name.eq_ignore_ascii_case(&property.key))
            {
                push_ordinal(column_idx);
                if !property_ordinals.contains(&column_idx) {
                    property_ordinals.push(column_idx);
                }
            }
        }

        let Some(projected_columns) =
            self.table_column_ids_for_ordinals(context, table_id, &ordinals)?
        else {
            return Ok(None);
        };
        let column_names = Arc::new(
            ordinals
                .iter()
                .map(|ordinal| edge_col_names[*ordinal].clone())
                .collect::<Vec<_>>(),
        );
        let adjacency_ordinals: Vec<usize> = ordinals
            .iter()
            .copied()
            .filter(|ordinal| {
                property_ordinals.contains(ordinal)
                    || (*ordinal != src_col_idx && *ordinal != tgt_col_idx)
            })
            .collect();
        let adjacency_projected_columns = self
            .table_column_ids_for_ordinals(context, table_id, &adjacency_ordinals)?
            .unwrap_or_default();
        let adjacency_column_names = Arc::new(
            adjacency_ordinals
                .iter()
                .map(|ordinal| edge_col_names[*ordinal].clone())
                .collect::<Vec<_>>(),
        );
        Ok(Some(RelationshipScanProjection {
            projected_columns,
            column_names,
            adjacency_projected_columns,
            adjacency_column_names,
            src_col_idx: projected_src_col_idx,
            tgt_col_idx: projected_tgt_col_idx,
        }))
    }

    fn native_graph_traversal_store<'executor, 'context>(
        &'executor self,
        context: &'context ExecutionContext,
        edge_table_id: RelationId,
        src_col_idx: usize,
        tgt_col_idx: usize,
    ) -> DbResult<NativeGraphTraversalStoreRef<'executor, 'context>> {
        let _ = (src_col_idx, tgt_col_idx);
        Ok(NativeGraphTraversalStoreRef {
            executor: self,
            context,
            edge_table_id,
        })
    }

    pub(super) fn native_graph_traversal_ref<'executor, 'context>(
        &'executor self,
        context: &'context ExecutionContext,
        edge_table_id: RelationId,
        src_col_idx: usize,
        tgt_col_idx: usize,
    ) -> DbResult<GraphTraversalRef<NativeGraphTraversalStoreRef<'executor, 'context>>> {
        let storage =
            self.native_graph_traversal_store(context, edge_table_id, src_col_idx, tgt_col_idx)?;
        let traversal_available = self
            .storage_dml
            .adjacency_index_available(context.txn_id, edge_table_id);
        let stats = storage.stats();
        Ok(GraphTraversalRef {
            snapshot: aiondb_graph::ProjectionSnapshot {
                generation: self.storage_dml.cache_generation().unwrap_or(0),
                refresh_policy: aiondb_graph::RefreshPolicy::Live,
                refreshed_at_epoch_millis: None,
            },
            stats,
            plan: aiondb_graph::HybridGraphPlan {
                source: traversal_available
                    .then_some(aiondb_graph::HybridGraphSource::TraversalStore),
                fallback_source: Some(aiondb_graph::HybridGraphSource::RowStore),
                estimated_rows: (stats.edge_count > 0).then_some(stats.edge_count),
                projection_name: None,
                reason: Some(if traversal_available {
                    "native adjacency traversal available for edge table".to_owned()
                } else {
                    "native adjacency traversal unavailable for edge table".to_owned()
                }),
            },
            storage,
        })
    }

    pub(super) fn relationship_traversal_specs(
        &self,
        context: &ExecutionContext,
        rel_variants: &[CypherRelPattern],
        path_variable: Option<&str>,
    ) -> DbResult<Vec<RelationshipTraversalSpec>> {
        let mut specs = Vec::new();

        for rel in rel_variants {
            let Some(table_id) = rel.table_id else {
                continue;
            };
            let ((src_col_idx, tgt_col_idx), use_table_adjacency) = self
                .resolve_edge_endpoint_columns_for_rel(
                    context,
                    table_id,
                    rel.rel_type.as_deref(),
                )?;

            let edge_rel_type: Arc<str> =
                Arc::from(rel.rel_type.as_deref().unwrap_or("").to_owned());
            let edge_table_descriptor = self
                .catalog_reader
                .get_table_by_id(context.txn_id, table_id)?;
            let edge_col_names: Arc<Vec<String>> = Arc::new(
                edge_table_descriptor
                    .as_ref()
                    .map(|t| t.columns.iter().map(|c| c.name.clone()).collect::<Vec<_>>())
                    .unwrap_or_default(),
            );
            let edge_rls_policies = match edge_table_descriptor.as_ref() {
                Some(table) => self.compile_compat_rls_policies(
                    table,
                    super::dml_plans::CompatRlsAction::Select,
                    context,
                )?,
                None => None,
            };
            let projected_scan =
                if rel.variable.is_none() && path_variable.is_none() && edge_rls_policies.is_none()
                {
                    self.build_relationship_scan_projection(
                        context,
                        table_id,
                        src_col_idx,
                        tgt_col_idx,
                        edge_col_names.as_ref(),
                        &rel.properties,
                    )?
                } else {
                    None
                };

            specs.push(RelationshipTraversalSpec {
                rel: rel.clone(),
                table_id,
                src_col_idx,
                tgt_col_idx,
                use_table_adjacency,
                edge_rel_type,
                edge_col_names,
                edge_rls_policies,
                projected_scan,
            });
        }

        Ok(specs)
    }

    pub(super) fn endpoint_indexes_for_direction(
        &self,
        context: &ExecutionContext,
        table_id: RelationId,
        direction: CypherRelDirection,
        src_col_idx: usize,
        tgt_col_idx: usize,
    ) -> DbResult<Option<Vec<IndexId>>> {
        let outgoing = self.find_btree_index_for_column_ordinal(context, table_id, src_col_idx)?;
        let incoming = self.find_btree_index_for_column_ordinal(context, table_id, tgt_col_idx)?;
        match direction {
            CypherRelDirection::Outgoing => Ok(outgoing.map(|index_id| vec![index_id])),
            CypherRelDirection::Incoming => Ok(incoming.map(|index_id| vec![index_id])),
            CypherRelDirection::Both => match (outgoing, incoming) {
                (Some(source_index), Some(target_index)) => {
                    let mut indexes = vec![source_index];
                    if target_index != source_index {
                        indexes.push(target_index);
                    }
                    Ok(Some(indexes))
                }
                _ => Ok(None),
            },
        }
    }

    pub(super) fn collect_indexed_adjacent_edges(
        &self,
        context: &ExecutionContext,
        table_id: RelationId,
        node_id: &Value,
        direction: CypherRelDirection,
        src_col_idx: usize,
        tgt_col_idx: usize,
        include_oid_system_column: bool,
        projection: Option<&RelationshipScanProjection>,
    ) -> DbResult<Option<Vec<(SharedRow, SharedRow, TupleId, Value, Value)>>> {
        let Some(indexes) = self.endpoint_indexes_for_direction(
            context,
            table_id,
            direction,
            src_col_idx,
            tgt_col_idx,
        )?
        else {
            return Ok(None);
        };

        let mut results = Vec::new();
        let mut seen = HashSet::new();
        for index_id in indexes {
            let mut stream = self.scan_index_locked(
                context,
                table_id,
                index_id,
                exact_lookup_key_range(node_id),
                projection.map(|projection| projection.projected_columns.clone()),
            )?;
            while let Some(record) = stream.next()? {
                context.check_deadline()?;
                if !seen.insert(record.tuple_id) {
                    continue;
                }
                let compat_row =
                    self.compat_scan_row(&record, include_oid_system_column, Some(table_id));
                let projected_src_col_idx =
                    projection.map_or(src_col_idx, |value| value.src_col_idx);
                let projected_tgt_col_idx =
                    projection.map_or(tgt_col_idx, |value| value.tgt_col_idx);
                let source_id = compat_row
                    .values
                    .get(projected_src_col_idx)
                    .cloned()
                    .unwrap_or(Value::Null);
                let target_id = compat_row
                    .values
                    .get(projected_tgt_col_idx)
                    .cloned()
                    .unwrap_or(Value::Null);
                let adjacent = match direction {
                    CypherRelDirection::Outgoing => source_id == *node_id,
                    CypherRelDirection::Incoming => target_id == *node_id,
                    CypherRelDirection::Both => source_id == *node_id || target_id == *node_id,
                };
                if !adjacent {
                    continue;
                }
                ensure_graph_workset_capacity(context, results.len(), "adjacent edge candidates")?;
                context.track_memory(estimate_adjacent_edge_record_bytes(
                    &compat_row,
                    &record.row,
                    &source_id,
                    &target_id,
                ))?;
                results.push((
                    Arc::new(compat_row),
                    Arc::new(record.row),
                    record.tuple_id,
                    source_id,
                    target_id,
                ));
            }
        }
        Ok(Some(results))
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn collect_adjacent_edges(
        &self,
        context: &ExecutionContext,
        table_id: RelationId,
        node_id: &Value,
        direction: CypherRelDirection,
        src_col_idx: usize,
        tgt_col_idx: usize,
        use_table_adjacency: bool,
        rls_policies: Option<&[super::dml_plans::CompatRlsPolicy]>,
        projection: Option<&RelationshipScanProjection>,
    ) -> DbResult<Vec<AdjacentEdgeRecord>> {
        let mut results = Vec::new();
        let include_oid_system_column =
            self.compat_include_oid_system_column_for_table_id(context, table_id)?;
        let traversal =
            self.native_graph_traversal_ref(context, table_id, src_col_idx, tgt_col_idx)?;
        let traversal_generation = traversal.snapshot().generation;
        debug_assert_eq!(
            traversal_generation,
            self.storage_dml
                .cache_generation()
                .unwrap_or(traversal_generation)
        );
        let traversal_store = traversal.storage();

        let directions: &[bool] = match direction {
            CypherRelDirection::Outgoing => &[true],
            CypherRelDirection::Incoming => &[false],
            CypherRelDirection::Both => &[true, false],
        };

        let mut used_adjacency = false;
        if use_table_adjacency {
            for &is_outgoing in directions {
                let mut tuple_cursor = traversal_store.edge_ids(
                    node_id,
                    if is_outgoing {
                        GraphDirection::Outgoing
                    } else {
                        GraphDirection::Incoming
                    },
                );
                let mut saw_tuple = false;
                used_adjacency = true;
                while let Some(tid) = tuple_cursor.next_neighbor() {
                    saw_tuple = true;
                    let native_endpoints = traversal_store.edge_endpoints(tid);
                    let has_native_endpoints = native_endpoints.is_some();
                    let maybe_row = self.storage_dml.fetch_ref(
                        context.txn_id,
                        &context.snapshot,
                        table_id,
                        tid,
                        projection.map(|projection| {
                            projection.fetch_projection(native_endpoints.is_some())
                        }),
                    )?;
                    let Some(row) = maybe_row else {
                        continue;
                    };
                    if !self.compat_rls_allows_existing_row(rls_policies, &row, context)? {
                        continue;
                    }
                    let record = aiondb_storage_api::TupleRecord {
                        tuple_id: tid,
                        heap_position: tid.get(),
                        row,
                    };
                    let compat_row =
                        self.compat_scan_row(&record, include_oid_system_column, Some(table_id));
                    let (source_id, target_id) =
                        if let Some((source_id, target_id)) = native_endpoints {
                            (source_id, target_id)
                        } else {
                            let projected_src_col_idx =
                                projection.map_or(src_col_idx, |value| value.src_col_idx);
                            let projected_tgt_col_idx =
                                projection.map_or(tgt_col_idx, |value| value.tgt_col_idx);
                            (
                                compat_row
                                    .values
                                    .get(projected_src_col_idx)
                                    .cloned()
                                    .unwrap_or(Value::Null),
                                compat_row
                                    .values
                                    .get(projected_tgt_col_idx)
                                    .cloned()
                                    .unwrap_or(Value::Null),
                            )
                        };
                    let adjacent = match direction {
                        CypherRelDirection::Outgoing => source_id == *node_id,
                        CypherRelDirection::Incoming => target_id == *node_id,
                        CypherRelDirection::Both => source_id == *node_id || target_id == *node_id,
                    };
                    if !adjacent {
                        continue;
                    }
                    ensure_graph_workset_capacity(
                        context,
                        results.len(),
                        "adjacent edge candidates",
                    )?;
                    context.track_memory(estimate_adjacent_edge_record_bytes(
                        &compat_row,
                        &record.row,
                        &source_id,
                        &target_id,
                    ))?;
                    results.push(AdjacentEdgeRecord {
                        compat_row: Arc::new(compat_row),
                        raw_row: Arc::new(record.row),
                        tuple_id: tid,
                        source_id,
                        target_id,
                        native_endpoints: has_native_endpoints,
                    });
                }
                if !saw_tuple && !traversal.uses_traversal_store() {
                    used_adjacency = false;
                    break;
                }
            }
        }

        if !used_adjacency {
            if let Some(edge_records) = self.collect_indexed_adjacent_edges(
                context,
                table_id,
                node_id,
                direction,
                src_col_idx,
                tgt_col_idx,
                include_oid_system_column,
                projection,
            )? {
                if rls_policies.is_some() {
                    let mut filtered = Vec::new();
                    for record in edge_records {
                        if self.compat_rls_allows_existing_row(
                            rls_policies,
                            record.1.as_ref(),
                            context,
                        )? {
                            filtered.push(AdjacentEdgeRecord {
                                compat_row: record.0,
                                raw_row: record.1,
                                tuple_id: record.2,
                                source_id: record.3,
                                target_id: record.4,
                                native_endpoints: false,
                            });
                        }
                    }
                    return Ok(filtered);
                }
                return Ok(edge_records
                    .into_iter()
                    .map(|(compat_row, raw_row, tuple_id, source_id, target_id)| {
                        AdjacentEdgeRecord {
                            compat_row,
                            raw_row,
                            tuple_id,
                            source_id,
                            target_id,
                            native_endpoints: false,
                        }
                    })
                    .collect());
            }
        }

        if !used_adjacency {
            results.clear();
            let mut stream = self.scan_table_locked(
                context,
                table_id,
                projection.map(|projection| projection.projected_columns.clone()),
            )?;
            while let Some(record) = stream.next()? {
                context.check_deadline()?;
                if !self.compat_rls_allows_existing_row(rls_policies, &record.row, context)? {
                    continue;
                }
                let compat_row =
                    self.compat_scan_row(&record, include_oid_system_column, Some(table_id));
                let projected_src_col_idx =
                    projection.map_or(src_col_idx, |value| value.src_col_idx);
                let projected_tgt_col_idx =
                    projection.map_or(tgt_col_idx, |value| value.tgt_col_idx);
                let source_id = compat_row
                    .values
                    .get(projected_src_col_idx)
                    .cloned()
                    .unwrap_or(Value::Null);
                let target_id = compat_row
                    .values
                    .get(projected_tgt_col_idx)
                    .cloned()
                    .unwrap_or(Value::Null);
                let adjacent = match direction {
                    CypherRelDirection::Outgoing => source_id == *node_id,
                    CypherRelDirection::Incoming => target_id == *node_id,
                    CypherRelDirection::Both => source_id == *node_id || target_id == *node_id,
                };
                if adjacent {
                    ensure_graph_workset_capacity(
                        context,
                        results.len(),
                        "adjacent edge candidates",
                    )?;
                    context.track_memory(estimate_adjacent_edge_record_bytes(
                        &compat_row,
                        &record.row,
                        &source_id,
                        &target_id,
                    ))?;
                    results.push(AdjacentEdgeRecord {
                        compat_row: Arc::new(compat_row),
                        raw_row: Arc::new(record.row),
                        tuple_id: record.tuple_id,
                        source_id,
                        target_id,
                        native_endpoints: false,
                    });
                }
            }
        }

        Ok(results)
    }
}
