use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::Arc,
};

use aiondb_core::{DbError, DbResult, RelationId, Row, SqlState, TupleId, Value};
use aiondb_eval::ValueHashKey;
use aiondb_graph::{
    pattern::AdjacentEdge as GraphAdjacentEdge, shortest_path as graph_shortest_path,
    PathElement as GraphPathElement, RowProvider as GraphRowProvider,
};
use aiondb_plan::graph::{CypherPathFunction, CypherPattern, CypherRelDirection};

use super::graph_runtime_match::{bind_named_path_variable, materialize_named_path_pattern};
use super::{ExecutionContext, Executor};
use crate::executor::graph_plans::GraphMatchRuntimeCache;
use crate::executor::graph_plans::{
    ensure_graph_result_row_capacity, ensure_graph_workset_capacity, estimate_bfs_path_bytes,
    estimate_binding_row_bytes, estimate_shortest_path_queue_entry_bytes, graph_bound_edge_literal,
    graph_bound_node_literal, size_of_u64, usize_to_u32, value_to_bfs_key, BindingRow,
    BoundValue, SharedRow, SharedStrings, SharedText,
};
use crate::executor::helpers::exact_lookup_key_range;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PathSearchMode {
    SingleShortest,
    AllShortest,
}

impl PathSearchMode {
    pub(super) fn from_path_function(func: CypherPathFunction) -> Self {
        match func {
            CypherPathFunction::ShortestPath => Self::SingleShortest,
            CypherPathFunction::AllShortestPaths => Self::AllShortest,
        }
    }

    pub(super) fn allows_multiple_shortest_paths(self) -> bool {
        matches!(self, Self::AllShortest)
    }
}

struct ExecutorGraphRowProvider<'a> {
    executor: &'a Executor,
    context: &'a ExecutionContext,
    edge_endpoint_overrides: HashMap<RelationId, (usize, usize)>,
}

#[derive(Clone)]
struct ShortestPathPredecessor {
    value: Value,
    parent: Option<ValueHashKey>,
    via_edge: Option<TupleId>,
}

impl<'a> GraphRowProvider for ExecutorGraphRowProvider<'a> {
    fn scan_table(&self, table_id: RelationId) -> DbResult<Vec<Row>> {
        let mut rows = Vec::new();
        let mut stream = self
            .executor
            .scan_table_locked(self.context, table_id, None)?;
        while let Some(record) = stream.next()? {
            self.context.check_deadline()?;
            rows.push(self.executor.compat_scan_row_for_table_id(
                self.context,
                table_id,
                &record,
            )?);
        }
        Ok(rows)
    }

    fn column_index(&self, table_id: RelationId, column: &str) -> DbResult<Option<usize>> {
        if let Some((source_idx, target_idx)) = self.edge_endpoint_overrides.get(&table_id) {
            if column.eq_ignore_ascii_case("source_id") {
                return Ok(Some(*source_idx));
            }
            if column.eq_ignore_ascii_case("target_id") {
                return Ok(Some(*target_idx));
            }
        }
        Ok(self
            .executor
            .catalog_reader
            .get_table_by_id(self.context.txn_id, table_id)?
            .and_then(|table| {
                table
                    .columns
                    .iter()
                    .position(|entry| entry.name.eq_ignore_ascii_case(column))
            }))
    }

    fn column_names(&self, table_id: RelationId) -> DbResult<Vec<String>> {
        Ok(self
            .executor
            .catalog_reader
            .get_table_by_id(self.context.txn_id, table_id)?
            .map(|table| {
                table
                    .columns
                    .into_iter()
                    .map(|column| column.name)
                    .collect()
            })
            .unwrap_or_default())
    }

    fn adjacency_lookup_edges(
        &self,
        edge_table_id: RelationId,
        node_id: &Value,
        direction: aiondb_graph::traversal::TraversalDirection,
    ) -> DbResult<Vec<GraphAdjacentEdge>> {
        let directions: &[bool] = match direction {
            aiondb_graph::traversal::TraversalDirection::Outgoing => &[true],
            aiondb_graph::traversal::TraversalDirection::Incoming => &[false],
            aiondb_graph::traversal::TraversalDirection::Both => &[true, false],
        };

        if self.edge_endpoint_overrides.contains_key(&edge_table_id) {
            return self.fallback_adjacency_lookup_edges(edge_table_id, node_id, direction);
        }

        let mut result = Vec::new();
        let mut seen = HashSet::new();

        for &outgoing in directions {
            match self.executor.storage_dml.adjacency_lookup(
                self.context.txn_id,
                &self.context.snapshot,
                edge_table_id,
                node_id,
                outgoing,
            ) {
                Ok(tuple_ids) => {
                    for tuple_id in tuple_ids {
                        if !seen.insert(tuple_id) {
                            continue;
                        }
                        let Some(row) = self.executor.storage_dml.fetch(
                            self.context.txn_id,
                            &self.context.snapshot,
                            edge_table_id,
                            tuple_id,
                            None,
                        )?
                        else {
                            continue;
                        };
                        let record = aiondb_storage_api::TupleRecord {
                            tuple_id,
                            heap_position: tuple_id.get(),
                            row,
                        };
                        result.push(GraphAdjacentEdge {
                            row: self.executor.compat_scan_row_for_table_id(
                                self.context,
                                edge_table_id,
                                &record,
                            )?,
                            tuple_id,
                        });
                    }
                }
                Err(error) if error.sqlstate() == SqlState::FeatureNotSupported => {
                    return self.fallback_adjacency_lookup_edges(edge_table_id, node_id, direction);
                }
                Err(error) => return Err(error),
            }
        }

        Ok(result)
    }
}

impl<'a> ExecutorGraphRowProvider<'a> {
    fn collect_indexed_adjacent_edges(
        &self,
        edge_table_id: RelationId,
        endpoint_idx: usize,
        node_id: &Value,
        seen: &mut HashSet<TupleId>,
        result: &mut Vec<GraphAdjacentEdge>,
    ) -> DbResult<bool> {
        let Some(index_id) = self.executor.find_btree_index_for_column_ordinal(
            self.context,
            edge_table_id,
            endpoint_idx,
        )? else {
            return Ok(false);
        };
        let mut stream = self.executor.scan_index_locked(
            self.context,
            edge_table_id,
            index_id,
            exact_lookup_key_range(node_id),
            None,
        )?;
        while let Some(record) = stream.next()? {
            self.context.check_deadline()?;
            if !seen.insert(record.tuple_id) {
                continue;
            }
            let compat = self.executor.compat_scan_row_for_table_id(
                self.context,
                edge_table_id,
                &record,
            )?;
            if compat.values.get(endpoint_idx) != Some(node_id) {
                continue;
            }
            result.push(GraphAdjacentEdge {
                row: compat,
                tuple_id: record.tuple_id,
            });
        }
        Ok(true)
    }

    fn collect_scanned_adjacent_edges(
        &self,
        edge_table_id: RelationId,
        src_idx: usize,
        tgt_idx: usize,
        node_id: &Value,
        direction: aiondb_graph::traversal::TraversalDirection,
        seen: &mut HashSet<TupleId>,
        result: &mut Vec<GraphAdjacentEdge>,
    ) -> DbResult<()> {
        let mut stream = self
            .executor
            .scan_table_locked(self.context, edge_table_id, None)?;
        while let Some(record) = stream.next()? {
            self.context.check_deadline()?;
            if !seen.insert(record.tuple_id) {
                continue;
            }
            let compat =
                self.executor
                    .compat_scan_row_for_table_id(self.context, edge_table_id, &record)?;
            let src = compat.values.get(src_idx).unwrap_or(&Value::Null);
            let tgt = compat.values.get(tgt_idx).unwrap_or(&Value::Null);
            let matches = match direction {
                aiondb_graph::traversal::TraversalDirection::Outgoing => src == node_id,
                aiondb_graph::traversal::TraversalDirection::Incoming => tgt == node_id,
                aiondb_graph::traversal::TraversalDirection::Both => {
                    src == node_id || tgt == node_id
                }
            };
            if matches {
                result.push(GraphAdjacentEdge {
                    row: compat,
                    tuple_id: record.tuple_id,
                });
            }
        }
        Ok(())
    }

    fn fallback_adjacency_lookup_edges(
        &self,
        edge_table_id: RelationId,
        node_id: &Value,
        direction: aiondb_graph::traversal::TraversalDirection,
    ) -> DbResult<Vec<GraphAdjacentEdge>> {
        let (src_idx, tgt_idx) =
            if let Some(endpoints) = self.edge_endpoint_overrides.get(&edge_table_id).copied() {
                endpoints
            } else {
                let src_idx = self
                    .column_index(edge_table_id, "source_id")?
                    .ok_or_else(|| DbError::internal("edge table missing source_id column"))?;
                let tgt_idx = self
                    .column_index(edge_table_id, "target_id")?
                    .ok_or_else(|| DbError::internal("edge table missing target_id column"))?;
                (src_idx, tgt_idx)
            };

        let mut result = Vec::new();
        let mut seen = HashSet::new();
        match direction {
            aiondb_graph::traversal::TraversalDirection::Outgoing => {
                if !self.collect_indexed_adjacent_edges(
                    edge_table_id,
                    src_idx,
                    node_id,
                    &mut seen,
                    &mut result,
                )? {
                    self.collect_scanned_adjacent_edges(
                        edge_table_id,
                        src_idx,
                        tgt_idx,
                        node_id,
                        direction,
                        &mut seen,
                        &mut result,
                    )?;
                }
            }
            aiondb_graph::traversal::TraversalDirection::Incoming => {
                if !self.collect_indexed_adjacent_edges(
                    edge_table_id,
                    tgt_idx,
                    node_id,
                    &mut seen,
                    &mut result,
                )? {
                    self.collect_scanned_adjacent_edges(
                        edge_table_id,
                        src_idx,
                        tgt_idx,
                        node_id,
                        direction,
                        &mut seen,
                        &mut result,
                    )?;
                }
            }
            aiondb_graph::traversal::TraversalDirection::Both => {
                let used_source_index = self.collect_indexed_adjacent_edges(
                    edge_table_id,
                    src_idx,
                    node_id,
                    &mut seen,
                    &mut result,
                )?;
                let used_target_index = self.collect_indexed_adjacent_edges(
                    edge_table_id,
                    tgt_idx,
                    node_id,
                    &mut seen,
                    &mut result,
                )?;
                if !(used_source_index || used_target_index) {
                    self.collect_scanned_adjacent_edges(
                        edge_table_id,
                        src_idx,
                        tgt_idx,
                        node_id,
                        direction,
                        &mut seen,
                        &mut result,
                    )?;
                }
            }
        }
        Ok(result)
    }
}

impl Executor {
    fn bound_path_hop_count(binding: &BindingRow, path_variable: &str) -> Option<usize> {
        match binding.get(path_variable)? {
            BoundValue::Path { relationships, .. }
            | BoundValue::PathValues { relationships, .. } => Some(relationships.len()),
            _ => None,
        }
    }

    fn materialize_bound_path_value(binding: &BindingRow, path_variable: &str) -> Option<BoundValue> {
        match binding.get(path_variable)? {
            BoundValue::PathValues {
                nodes,
                relationships,
                directions,
            } => Some(BoundValue::PathValues {
                nodes: Arc::clone(nodes),
                relationships: Arc::clone(relationships),
                directions: Arc::clone(directions),
            }),
            BoundValue::Path {
                nodes,
                relationships,
                directions,
            } => Some(BoundValue::PathValues {
                nodes: Arc::new(
                    nodes.iter()
                        .map(|variable| {
                            graph_bound_node_literal(binding, variable)
                                .unwrap_or_else(|| "()".to_owned())
                        })
                        .collect(),
                ),
                relationships: Arc::new(
                    relationships
                        .iter()
                        .map(|variable| {
                            graph_bound_edge_literal(binding, variable)
                                .unwrap_or_else(|| "[]".to_owned())
                        })
                        .collect(),
                ),
                directions: Arc::clone(directions),
            }),
            _ => None,
        }
    }

    fn match_shortest_path_multi_segment(
        &self,
        context: &ExecutionContext,
        pattern: &CypherPattern,
        func: CypherPathFunction,
        input_bindings: Vec<BindingRow>,
    ) -> DbResult<Vec<BindingRow>> {
        if matches!(func, CypherPathFunction::AllShortestPaths) {
            return Err(DbError::feature_not_supported(
                "allShortestPaths multi-segment patterns are not supported yet",
            ));
        }

        if pattern.relationships.iter().any(|rel| rel.table_id.is_none()) {
            return Err(DbError::feature_not_supported(
                "shortestPath requires typed relationship patterns (e.g. [:KNOWS] / [:KNOWS*])",
            ));
        }

        let variable_length_relationships = pattern
            .relationships
            .iter()
            .filter(|rel| rel.min_hops.is_some() || rel.max_hops.is_some())
            .count();
        if variable_length_relationships > 1 {
            return Err(DbError::feature_not_supported(
                "shortestPath/allShortestPaths currently supports at most one variable-length relationship in multi-segment patterns",
            ));
        }

        let internal_path_variable = pattern
            .path_variable
            .clone()
            .unwrap_or_else(|| "__shortest_path_internal__".to_owned());
        let mut internal_pattern = pattern.clone();
        internal_pattern.path_function = None;
        internal_pattern.path_variable = Some(internal_path_variable.clone());
        let internal_pattern = materialize_named_path_pattern(&internal_pattern);

        let mode = PathSearchMode::from_path_function(func);
        let mut runtime_cache = GraphMatchRuntimeCache::default();
        let mut output = Vec::new();

        for binding in input_bindings {
            context.check_deadline()?;
            let matched = self.match_pattern(
                context,
                &internal_pattern,
                vec![binding],
                &[],
                None,
                &mut runtime_cache,
            )?;
            let matched = bind_named_path_variable(&internal_pattern, matched);
            let min_hops = matched
                .iter()
                .filter_map(|binding| Self::bound_path_hop_count(binding, &internal_path_variable))
                .min();
            let Some(min_hops) = min_hops else {
                continue;
            };

            let mut kept = matched
                .into_iter()
                .filter(|binding| {
                    Self::bound_path_hop_count(binding, &internal_path_variable) == Some(min_hops)
                })
                .collect::<Vec<_>>();
            if !mode.allows_multiple_shortest_paths() {
                kept.truncate(1);
            }

            for mut binding in kept {
                if let Some(materialized_path) =
                    Self::materialize_bound_path_value(&binding, &internal_path_variable)
                {
                    binding.insert_binding(internal_path_variable.clone(), materialized_path);
                }
                ensure_graph_result_row_capacity(context, output.len())?;
                context.track_memory(estimate_binding_row_bytes(&binding))?;
                output.push(binding);
            }
        }

        Ok(output)
    }

    fn reconstruct_shortest_path_from_predecessors(
        &self,
        predecessors: &HashMap<ValueHashKey, ShortestPathPredecessor>,
        end_key: ValueHashKey,
    ) -> DbResult<Vec<TupleId>> {
        let mut edges = Vec::new();
        let mut current_key = Some(end_key);
        while let Some(key) = current_key {
            let state = predecessors
                .get(&key)
                .ok_or_else(|| DbError::internal("shortest path predecessor chain missing node"))?;
            if let Some(edge) = state.via_edge {
                edges.push(edge);
            }
            current_key = state.parent.clone();
        }
        edges.reverse();
        Ok(edges)
    }

    pub(super) fn match_shortest_path(
        &self,
        context: &ExecutionContext,
        pattern: &CypherPattern,
        func: CypherPathFunction,
        input_bindings: Vec<crate::executor::graph_plans::BindingRow>,
    ) -> DbResult<Vec<crate::executor::graph_plans::BindingRow>> {
        if pattern.nodes.len() != 2 || pattern.relationships.len() != 1 {
            return self.match_shortest_path_multi_segment(context, pattern, func, input_bindings);
        }
        let start_node_pat = &pattern.nodes[0];
        let end_node_pat = &pattern.nodes[1];
        let rel_pat = &pattern.relationships[0];

        let max_depth = rel_pat.max_hops.unwrap_or(15);

        let Some(edge_table_id) = rel_pat.table_id else {
            return Err(DbError::feature_not_supported(
                "shortestPath requires a typed relationship pattern (e.g. [:KNOWS*])",
            ));
        };

        let edge_table = self
            .catalog_reader
            .get_table_by_id(context.txn_id, edge_table_id)?
            .ok_or_else(|| DbError::internal("edge table not found"))?;
        let edge_col_names: SharedStrings =
            Arc::new(edge_table.columns.iter().map(|c| c.name.clone()).collect());
        let ((source_idx, target_idx), use_table_adjacency) = self
            .resolve_edge_endpoint_columns_for_rel(
                context,
                edge_table_id,
                rel_pat.rel_type.as_deref(),
            )?;

        let mut runtime_cache = GraphMatchRuntimeCache::default();
        let mut shortest_path_node_row_cache: HashMap<(RelationId, String), Arc<Row>> =
            HashMap::new();
        let start_bindings =
            self.match_node(context, start_node_pat, input_bindings, &mut runtime_cache)?;
        let start_and_end_bindings =
            self.match_node(context, end_node_pat, start_bindings, &mut runtime_cache)?;

        let start_var = start_node_pat.variable.as_deref().unwrap_or("__sp_start__");
        let end_var = end_node_pat.variable.as_deref().unwrap_or("__sp_end__");
        let rel_var = rel_pat.variable.as_deref();
        let rel_type_name: SharedText = Arc::from(rel_pat.rel_type.clone().unwrap_or_default());

        let path_variable = pattern.path_variable.clone();
        let path_direction = rel_pat.direction;
        let node_label_by_table: HashMap<RelationId, SharedStrings> = if path_variable.is_some() {
            let mut map = HashMap::new();
            for desc in self.catalog_reader.list_node_labels(context.txn_id)? {
                map.entry(desc.table_id)
                    .or_insert_with(|| Arc::new(vec![desc.label.clone()]));
            }
            map
        } else {
            HashMap::new()
        };
        let mut col_names_by_table: HashMap<RelationId, SharedStrings> = HashMap::new();

        let mut output = Vec::new();

        let mode = PathSearchMode::from_path_function(func);
        let mut edge_endpoint_overrides = HashMap::new();
        if !use_table_adjacency {
            edge_endpoint_overrides.insert(edge_table_id, (source_idx, target_idx));
        }
        let graph_provider = ExecutorGraphRowProvider {
            executor: self,
            context,
            edge_endpoint_overrides,
        };
        let use_storage_backed_shortest_path = matches!(mode, PathSearchMode::SingleShortest);

        for binding in &start_and_end_bindings {
            context.check_deadline()?;

            let start_id = match binding.get(start_var) {
                Some(BoundValue::Node { id_value, .. }) => id_value.clone(),
                _ => continue,
            };
            let end_id = match binding.get(end_var) {
                Some(BoundValue::Node { id_value, .. }) => id_value.clone(),
                _ => continue,
            };

            if start_id == end_id {
                let mut new_binding = binding.clone();
                if let Some(pv) = &path_variable {
                    new_binding.insert_binding(
                        pv.clone(),
                        BoundValue::Path {
                            nodes: Arc::new(vec![start_var.to_owned()]),
                            relationships: Arc::new(Vec::new()),
                            directions: Arc::new(Vec::new()),
                        },
                    );
                }
                ensure_graph_result_row_capacity(context, output.len())?;
                context.track_memory(crate::executor::graph_plans::estimate_binding_row_bytes(
                    &new_binding,
                ))?;
                output.push(new_binding);
                continue;
            }

            let (start_table_id, start_row, start_row_arc) = match binding.get(start_var) {
                Some(BoundValue::Node { table_id, row, .. }) => {
                    (*table_id, row.as_ref(), Arc::clone(row))
                }
                _ => continue,
            };
            let (end_table_id, end_row, end_row_arc) = match binding.get(end_var) {
                Some(BoundValue::Node { table_id, row, .. }) => {
                    (*table_id, row.as_ref(), Arc::clone(row))
                }
                _ => continue,
            };

            if use_storage_backed_shortest_path {
                let Some(path) = graph_shortest_path(
                    start_table_id,
                    start_row,
                    end_table_id,
                    end_row,
                    edge_table_id,
                    &graph_provider,
                    max_depth,
                )?
                else {
                    continue;
                };

                let mut new_binding = binding.clone();
                if let Some(rv) = rel_var {
                    let first_edge_tuple_id = path.iter().find_map(|element| match element {
                        GraphPathElement::Edge { tuple_id, .. } => Some(*tuple_id),
                        GraphPathElement::Node { .. } => None,
                    });
                    if let Some(tuple_id) = first_edge_tuple_id {
                        let Some(raw_row) = self.storage_dml.fetch(
                            context.txn_id,
                            &context.snapshot,
                            edge_table_id,
                            tuple_id,
                            None,
                        )?
                        else {
                            continue;
                        };
                        let record = aiondb_storage_api::TupleRecord {
                            tuple_id,
                            heap_position: tuple_id.get(),
                            row: raw_row,
                        };
                        let compat_row =
                            self.compat_scan_row_for_table_id(context, edge_table_id, &record)?;
                        new_binding.insert_binding(
                            rv.to_owned(),
                            BoundValue::Edge {
                                table_id: edge_table_id,
                                row: Arc::new(compat_row),
                                raw_row: Arc::new(record.row),
                                tuple_id,
                                rel_type: Arc::clone(&rel_type_name),
                                column_names: Arc::clone(&edge_col_names),
                            },
                        );
                    }
                }
                if let Some(pv) = &path_variable {
                    Executor::bind_full_shortest_path(
                        self,
                        context,
                        pv,
                        &path,
                        &rel_type_name,
                        path_direction,
                        start_node_pat.label.as_deref(),
                        &node_label_by_table,
                        &mut col_names_by_table,
                        &mut new_binding,
                    )?;
                }
                ensure_graph_result_row_capacity(context, output.len())?;
                context.track_memory(crate::executor::graph_plans::estimate_binding_row_bytes(
                    &new_binding,
                ))?;
                output.push(new_binding);
            } else {
                let paths = self.search_paths_bfs_adjacency(
                    context,
                    mode,
                    &start_id,
                    &end_id,
                    edge_table_id,
                    target_idx,
                    &graph_provider,
                    max_depth,
                )?;

                if paths.is_empty() {
                    continue;
                }

                for path_edges in &paths {
                    let mut new_binding = binding.clone();

                    if let Some(rv) = rel_var {
                        if let Some(tuple_id) = path_edges.first() {
                            let Some(raw_row) = self.storage_dml.fetch(
                                context.txn_id,
                                &context.snapshot,
                                edge_table_id,
                                *tuple_id,
                                None,
                            )?
                            else {
                                continue;
                            };
                            let record = aiondb_storage_api::TupleRecord {
                                tuple_id: *tuple_id,
                                heap_position: tuple_id.get(),
                                row: raw_row,
                            };
                            let compat_row =
                                self.compat_scan_row_for_table_id(context, edge_table_id, &record)?;
                            new_binding.insert_binding(
                                rv.to_owned(),
                                BoundValue::Edge {
                                    table_id: edge_table_id,
                                    row: Arc::new(compat_row),
                                    raw_row: Arc::new(record.row),
                                    tuple_id: *tuple_id,
                                    rel_type: Arc::clone(&rel_type_name),
                                    column_names: Arc::clone(&edge_col_names),
                                },
                            );
                        }
                    }

                    if let Some(pv) = &path_variable {
                        let path = self.materialize_shortest_path_from_edges(
                            context,
                            path_edges,
                            edge_table_id,
                            target_idx,
                            start_table_id,
                            &start_id,
                            Arc::clone(&start_row_arc),
                            end_table_id,
                            &end_id,
                            Arc::clone(&end_row_arc),
                            &mut shortest_path_node_row_cache,
                        )?;
                        Executor::bind_full_shortest_path(
                            self,
                            context,
                            pv,
                            &path,
                            &rel_type_name,
                            path_direction,
                            start_node_pat.label.as_deref(),
                            &node_label_by_table,
                            &mut col_names_by_table,
                            &mut new_binding,
                        )?;
                    }

                    ensure_graph_result_row_capacity(context, output.len())?;
                    context.track_memory(
                        crate::executor::graph_plans::estimate_binding_row_bytes(&new_binding),
                    )?;
                    output.push(new_binding);
                }
            }
        }

        Ok(output)
    }

    #[allow(clippy::too_many_arguments)]
    fn materialize_shortest_path_from_edges(
        &self,
        context: &ExecutionContext,
        path_edges: &[TupleId],
        edge_table_id: RelationId,
        target_idx: usize,
        start_table_id: RelationId,
        _start_id: &Value,
        start_row: Arc<Row>,
        end_table_id: RelationId,
        end_id: &Value,
        end_row: Arc<Row>,
        node_row_cache: &mut HashMap<(RelationId, String), Arc<Row>>,
    ) -> DbResult<Vec<GraphPathElement>> {
        let mut path = Vec::with_capacity(path_edges.len().saturating_mul(2).saturating_add(1));
        path.push(GraphPathElement::Node {
            table_id: start_table_id,
            row: (*start_row).clone(),
        });

        for (idx, tuple_id) in path_edges.iter().enumerate() {
            let Some(raw_row) = self.storage_dml.fetch(
                context.txn_id,
                &context.snapshot,
                edge_table_id,
                *tuple_id,
                None,
            )?
            else {
                continue;
            };
            let record = aiondb_storage_api::TupleRecord {
                tuple_id: *tuple_id,
                heap_position: tuple_id.get(),
                row: raw_row,
            };
            let edge_row = self.compat_scan_row_for_table_id(context, edge_table_id, &record)?;
            let target_id = edge_row
                .values
                .get(target_idx)
                .cloned()
                .unwrap_or(Value::Null);
            path.push(GraphPathElement::Edge {
                table_id: edge_table_id,
                row: edge_row,
                tuple_id: *tuple_id,
            });

            let is_last = idx + 1 == path_edges.len();
            let node_row = if is_last && target_id == *end_id {
                (*end_row).clone()
            } else {
                let cache_key = (start_table_id, format!("{target_id:?}"));
                if let Some(cached) = node_row_cache.get(&cache_key) {
                    (**cached).clone()
                } else {
                    let fetched = Arc::new(
                        self.fetch_shortest_path_node_row(context, start_table_id, &target_id)?
                            .unwrap_or_else(|| Row {
                                values: vec![target_id.clone()],
                            }),
                    );
                    node_row_cache.insert(cache_key, Arc::clone(&fetched));
                    (*fetched).clone()
                }
            };
            path.push(GraphPathElement::Node {
                table_id: if is_last {
                    end_table_id
                } else {
                    start_table_id
                },
                row: node_row,
            });
        }

        Ok(path)
    }

    fn fetch_shortest_path_node_row(
        &self,
        context: &ExecutionContext,
        table_id: RelationId,
        node_id: &Value,
    ) -> DbResult<Option<Row>> {
        if let Some(index_id) = self.find_first_column_btree_index(context, table_id)? {
            let mut stream = self.scan_index_locked(
                context,
                table_id,
                index_id,
                exact_lookup_key_range(node_id),
                None,
            )?;
            while let Some(record) = stream.next()? {
                context.check_deadline()?;
                let compat = self.compat_scan_row_for_table_id(context, table_id, &record)?;
                if compat.values.first() == Some(node_id) {
                    return Ok(Some(compat));
                }
            }
        }

        let mut stream = self.scan_table_locked(context, table_id, None)?;
        while let Some(record) = stream.next()? {
            context.check_deadline()?;
            let compat = self.compat_scan_row_for_table_id(context, table_id, &record)?;
            if compat.values.first() == Some(node_id) {
                return Ok(Some(compat));
            }
        }
        Ok(None)
    }

    #[allow(clippy::too_many_arguments)]
    fn bind_full_shortest_path(
        &self,
        context: &ExecutionContext,
        path_variable: &str,
        path: &[GraphPathElement],
        rel_type_name: &SharedText,
        direction: CypherRelDirection,
        fallback_label: Option<&str>,
        node_label_by_table: &HashMap<RelationId, SharedStrings>,
        col_names_by_table: &mut HashMap<RelationId, SharedStrings>,
        binding: &mut crate::executor::graph_plans::BindingRow,
    ) -> DbResult<()> {
        let mut node_vars: Vec<String> = Vec::new();
        let mut rel_vars: Vec<String> = Vec::new();
        let mut node_rows_by_table: HashMap<RelationId, HashMap<String, Arc<Row>>> =
            HashMap::new();

        for element in path {
            match element {
                GraphPathElement::Node { table_id, row } => {
                    let column_names = match col_names_by_table.get(table_id) {
                        Some(c) => Arc::clone(c),
                        None => {
                            let table = self
                                .catalog_reader
                                .get_table_by_id(context.txn_id, *table_id)?
                                .ok_or_else(|| {
                                    DbError::internal("node table not found for shortestPath")
                                })?;
                            let names: SharedStrings =
                                Arc::new(table.columns.iter().map(|c| c.name.clone()).collect());
                            col_names_by_table.insert(*table_id, Arc::clone(&names));
                            names
                        }
                    };
                    let labels = node_label_by_table
                        .get(table_id)
                        .map(Arc::clone)
                        .or_else(|| fallback_label.map(|l| Arc::new(vec![l.to_owned()])))
                        .unwrap_or_else(|| Arc::new(Vec::new()));
                    let id_value = row.values.first().cloned().unwrap_or(Value::Null);
                    let var = format!("__sp_{path_variable}_n{}", node_vars.len());
                    let full_row: Arc<Row> = if row.values.len() > 1 {
                        Arc::new(row.clone())
                    } else {
                        let table_cache =
                            node_rows_by_table.entry(*table_id).or_default();
                        let cache_key = format!("{id_value:?}");
                        if let Some(cached) = table_cache.get(&cache_key) {
                            Arc::clone(cached)
                        } else {
                            let fetched = self
                                .fetch_shortest_path_node_row(context, *table_id, &id_value)?
                                .map(Arc::new)
                                .unwrap_or_else(|| Arc::new(row.clone()));
                            table_cache.insert(cache_key, Arc::clone(&fetched));
                            fetched
                        }
                    };
                    let shared_row: SharedRow = full_row;
                    binding.insert_binding(
                        var.clone(),
                        BoundValue::Node {
                            table_id: *table_id,
                            row: Arc::clone(&shared_row),
                            raw_row: shared_row,
                            id_value,
                            tuple_id: aiondb_core::TupleId::default(),
                            labels,
                            column_names,
                        },
                    );
                    node_vars.push(var);
                }
                GraphPathElement::Edge {
                    table_id,
                    row,
                    tuple_id,
                } => {
                    let column_names = match col_names_by_table.get(table_id) {
                        Some(c) => Arc::clone(c),
                        None => {
                            let table = self
                                .catalog_reader
                                .get_table_by_id(context.txn_id, *table_id)?
                                .ok_or_else(|| {
                                    DbError::internal("edge table not found for shortestPath")
                                })?;
                            let names: SharedStrings =
                                Arc::new(table.columns.iter().map(|c| c.name.clone()).collect());
                            col_names_by_table.insert(*table_id, Arc::clone(&names));
                            names
                        }
                    };
                    let var = format!("__sp_{path_variable}_r{}", rel_vars.len());
                    let shared_row: SharedRow = Arc::new(row.clone());
                    binding.insert_binding(
                        var.clone(),
                        BoundValue::Edge {
                            table_id: *table_id,
                            row: Arc::clone(&shared_row),
                            raw_row: shared_row,
                            tuple_id: *tuple_id,
                            rel_type: Arc::clone(rel_type_name),
                            column_names,
                        },
                    );
                    rel_vars.push(var);
                }
            }
        }

        let directions = Arc::new(vec![direction; rel_vars.len()]);
        binding.insert_binding(
            path_variable.to_owned(),
            BoundValue::Path {
                nodes: Arc::new(node_vars),
                relationships: Arc::new(rel_vars),
                directions,
            },
        );
        Ok(())
    }

    fn search_paths_bfs_adjacency(
        &self,
        context: &ExecutionContext,
        mode: PathSearchMode,
        start_id: &Value,
        end_id: &Value,
        edge_table_id: RelationId,
        target_idx: usize,
        provider: &dyn GraphRowProvider,
        max_depth: u32,
    ) -> DbResult<Vec<Vec<TupleId>>> {
        let all = mode.allows_multiple_shortest_paths();
        if !all {
            let Some(start_key) = value_to_bfs_key(start_id) else {
                return self.search_paths_bfs_adjacency_all_paths(
                    context,
                    mode,
                    start_id,
                    end_id,
                    edge_table_id,
                    target_idx,
                    provider,
                    max_depth,
                );
            };

            let mut queue: VecDeque<ValueHashKey> = VecDeque::new();
            let mut predecessors: HashMap<ValueHashKey, ShortestPathPredecessor> = HashMap::new();
            predecessors.insert(
                start_key.clone(),
                ShortestPathPredecessor {
                    value: start_id.clone(),
                    parent: None,
                    via_edge: None,
                },
            );
            context.track_memory(estimate_shortest_path_queue_entry_bytes(start_id, 0, 0))?;
            context.track_memory(
                size_of_u64::<ValueHashKey>()
                    .saturating_mul(2)
                    .saturating_add(32),
            )?;
            ensure_graph_workset_capacity(context, queue.len(), "shortest-path queue")?;
            queue.push_back(start_key);

            for depth in 0..max_depth {
                let frontier_len = queue.len();
                if frontier_len == 0 {
                    break;
                }

                for _ in 0..frontier_len {
                    let Some(current_key) = queue.pop_front() else {
                        break;
                    };
                    let Some(current_state) = predecessors.get(&current_key).cloned() else {
                        continue;
                    };

                    let edges = provider.adjacency_lookup_edges(
                        edge_table_id,
                        &current_state.value,
                        aiondb_graph::traversal::TraversalDirection::Outgoing,
                    )?;
                    for edge in &edges {
                        context.check_deadline()?;
                        let edge_tgt = edge
                            .row
                            .values
                            .get(target_idx)
                            .cloned()
                            .unwrap_or(Value::Null);
                        let Some(edge_tgt_key) = value_to_bfs_key(&edge_tgt) else {
                            return self.search_paths_bfs_adjacency_all_paths(
                                context,
                                mode,
                                start_id,
                                end_id,
                                edge_table_id,
                                target_idx,
                                provider,
                                max_depth,
                            );
                        };
                        if predecessors.contains_key(&edge_tgt_key) {
                            continue;
                        }

                        predecessors.insert(
                            edge_tgt_key.clone(),
                            ShortestPathPredecessor {
                                value: edge_tgt.clone(),
                                parent: Some(current_key.clone()),
                                via_edge: Some(edge.tuple_id),
                            },
                        );
                        context.track_memory(
                            size_of_u64::<ValueHashKey>()
                                .saturating_mul(2)
                                .saturating_add(32),
                        )?;

                        if edge_tgt == *end_id {
                            let path = self.reconstruct_shortest_path_from_predecessors(
                                &predecessors,
                                edge_tgt_key,
                            )?;
                            ensure_graph_result_row_capacity(context, 0)?;
                            context.track_memory(estimate_bfs_path_bytes(path.len()))?;
                            return Ok(vec![path]);
                        }

                        context.track_memory(estimate_shortest_path_queue_entry_bytes(
                            &edge_tgt,
                            usize::try_from(depth.saturating_add(1)).unwrap_or(usize::MAX),
                            0,
                        ))?;
                        ensure_graph_workset_capacity(context, queue.len(), "shortest-path queue")?;
                        queue.push_back(edge_tgt_key);
                    }
                }
            }

            return Ok(Vec::new());
        }

        self.search_paths_bfs_adjacency_all_paths(
            context,
            mode,
            start_id,
            end_id,
            edge_table_id,
            target_idx,
            provider,
            max_depth,
        )
    }

    fn search_paths_bfs_adjacency_all_paths(
        &self,
        context: &ExecutionContext,
        mode: PathSearchMode,
        start_id: &Value,
        end_id: &Value,
        edge_table_id: RelationId,
        target_idx: usize,
        provider: &dyn GraphRowProvider,
        max_depth: u32,
    ) -> DbResult<Vec<Vec<TupleId>>> {
        let all = mode.allows_multiple_shortest_paths();
        let mut queue: VecDeque<(Value, Vec<TupleId>, HashSet<TupleId>)> = VecDeque::new();
        context.track_memory(estimate_shortest_path_queue_entry_bytes(start_id, 0, 0))?;
        ensure_graph_workset_capacity(context, queue.len(), "shortest-path queue")?;
        queue.push_back((start_id.clone(), Vec::new(), HashSet::new()));

        let mut visited: HashMap<ValueHashKey, u32> = HashMap::new();
        if let Some(k) = value_to_bfs_key(start_id) {
            visited.insert(k, 0);
            context.track_memory(
                size_of_u64::<ValueHashKey>()
                    .saturating_add(size_of_u64::<u32>())
                    .saturating_add(16),
            )?;
        }

        let mut found_paths: Vec<Vec<TupleId>> = Vec::new();
        let mut found_depth: Option<u32> = None;

        for depth in 0..max_depth {
            let frontier_len = queue.len();
            if frontier_len == 0 {
                break;
            }

            if let Some(fd) = found_depth {
                if depth > fd {
                    break;
                }
            }

            for _ in 0..frontier_len {
                let Some((current_val, path, path_set)) = queue.pop_front() else {
                    break;
                };

                let edges = provider.adjacency_lookup_edges(
                    edge_table_id,
                    &current_val,
                    aiondb_graph::traversal::TraversalDirection::Outgoing,
                )?;
                for edge in &edges {
                    context.check_deadline()?;
                    let edge_tgt = edge
                        .row
                        .values
                        .get(target_idx)
                        .cloned()
                        .unwrap_or(Value::Null);

                    if path_set.contains(&edge.tuple_id) {
                        continue;
                    }

                    let mut new_path = path.clone();
                    new_path.push(edge.tuple_id);
                    let mut new_path_set = path_set.clone();
                    new_path_set.insert(edge.tuple_id);

                    if edge_tgt == *end_id {
                        let depth = usize_to_u32(new_path.len());
                        match found_depth {
                            None => {
                                found_depth = Some(depth);
                                ensure_graph_result_row_capacity(context, found_paths.len())?;
                                context.track_memory(estimate_bfs_path_bytes(new_path.len()))?;
                                found_paths.push(new_path);
                                if !all {
                                    return Ok(found_paths);
                                }
                            }
                            Some(fd) if depth == fd => {
                                ensure_graph_result_row_capacity(context, found_paths.len())?;
                                context.track_memory(estimate_bfs_path_bytes(new_path.len()))?;
                                found_paths.push(new_path);
                            }
                            _ => {}
                        }
                        continue;
                    }

                    if let Some(k) = value_to_bfs_key(&edge_tgt) {
                        if let Some(&prev_depth) = visited.get(&k) {
                            if !all || usize_to_u32(new_path.len()) > prev_depth {
                                continue;
                            }
                        }
                        if visited.insert(k, usize_to_u32(new_path.len())).is_none() {
                            context.track_memory(
                                size_of_u64::<u64>()
                                    .saturating_add(size_of_u64::<u32>())
                                    .saturating_add(16),
                            )?;
                        }
                    }

                    context.track_memory(estimate_shortest_path_queue_entry_bytes(
                        &edge_tgt,
                        new_path.len(),
                        new_path_set.len(),
                    ))?;
                    ensure_graph_workset_capacity(context, queue.len(), "shortest-path queue")?;
                    queue.push_back((edge_tgt, new_path, new_path_set));
                }
            }
        }

        Ok(found_paths)
    }
}
