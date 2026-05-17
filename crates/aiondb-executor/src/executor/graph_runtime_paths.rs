use std::{collections::HashSet, sync::Arc};

use aiondb_core::{DbResult, RelationId, TupleId, Value};
use aiondb_eval::{build_hash_key, ValueHashKey};
use aiondb_graph::{GraphDirection, GraphStorage};
use aiondb_plan::graph::{CypherNodePattern, CypherRelDirection, CypherRelPattern};
use tracing::debug;

use super::graph_runtime_traversal::RelationshipTraversalSpec;
use super::{ExecutionContext, Executor};
use crate::executor::graph_plans::{
    ensure_graph_result_row_capacity, ensure_graph_workset_capacity, estimate_binding_row_bytes,
    estimate_variable_frontier_entry_bytes, format_cypher_edge_literal, push_graph_binding,
    BindingRow, BoundValue, GraphMatchRuntimeCache, SharedRow, SharedStrings, SharedText,
};

fn value_to_bfs_key(v: &Value) -> Option<ValueHashKey> {
    build_hash_key(v).ok()
}

fn adjacency_neighbor_allowed(
    neighbor_id: &Value,
    allowed_ids: Option<&HashSet<ValueHashKey>>,
) -> bool {
    if neighbor_id.is_null() {
        return false;
    }
    if let Some(allowed_ids) = allowed_ids {
        let Some(neighbor_key) = value_to_bfs_key(neighbor_id) else {
            return false;
        };
        return allowed_ids.contains(&neighbor_key);
    }
    true
}

fn excluded_next_node_id_match(
    excluded_next_node_id_keys: &[ValueHashKey],
    neighbor_id: &Value,
) -> bool {
    let Some(neighbor_key) = value_to_bfs_key(neighbor_id) else {
        return false;
    };
    excluded_next_node_id_keys.contains(&neighbor_key)
}

fn collect_excluded_next_node_id_keys(
    binding: &BindingRow,
    excluded_next_node_id_vars: &[String],
) -> Vec<ValueHashKey> {
    excluded_next_node_id_vars
        .iter()
        .filter_map(|variable| match binding.get(variable) {
            Some(BoundValue::Node { id_value, .. }) => value_to_bfs_key(id_value),
            _ => None,
        })
        .collect()
}

pub(super) struct VariableTraversalFrontierEntry {
    pub node_id: Value,
    pub binding: BindingRow,
    pub path_edges: HashSet<(RelationId, TupleId)>,
    pub path_nodes: Vec<String>,
    pub path_relationships: Vec<String>,
}

pub(super) fn far_end_for_direction(
    current_node_id: &Value,
    direction: CypherRelDirection,
    source_id: &Value,
    target_id: &Value,
) -> Value {
    match direction {
        CypherRelDirection::Outgoing => target_id.clone(),
        CypherRelDirection::Incoming => source_id.clone(),
        CypherRelDirection::Both => {
            if *current_node_id == *source_id {
                target_id.clone()
            } else {
                source_id.clone()
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn record_variable_traversal_step(
    context: &ExecutionContext,
    output: &mut Vec<BindingRow>,
    next_frontier: &mut Vec<VariableTraversalFrontierEntry>,
    entry: &VariableTraversalFrontierEntry,
    spec: &RelationshipTraversalSpec,
    compat_row: &SharedRow,
    raw_row: &SharedRow,
    tuple_id: TupleId,
    far_end: Value,
    edge_key: (RelationId, TupleId),
    depth: usize,
    min_hops: usize,
    max_hops: usize,
    path_variable: Option<&str>,
    path_edge_literal: Option<String>,
    next_node_literal: Option<String>,
) -> DbResult<()> {
    let mut new_binding = entry.binding.clone();
    let mut new_path_nodes = entry.path_nodes.clone();
    let mut new_path_relationships = entry.path_relationships.clone();

    if let Some(ref var) = spec.rel.variable {
        new_binding = new_binding.with_binding(
            var,
            BoundValue::Edge {
                table_id: spec.table_id,
                row: Arc::clone(compat_row),
                raw_row: Arc::clone(raw_row),
                tuple_id,
                rel_type: Arc::clone(&spec.edge_rel_type),
                column_names: Arc::clone(&spec.edge_col_names),
            },
        );
    }

    if let Some(path_variable) = path_variable {
        if let Some(edge_literal) = path_edge_literal {
            new_path_relationships.push(edge_literal);
        }
        if let Some(node_literal) = next_node_literal {
            new_path_nodes.push(node_literal);
        }
        let path_len = new_path_relationships.len();
        new_binding.insert_binding(
            path_variable.to_owned(),
            BoundValue::PathValues {
                nodes: Arc::new(new_path_nodes.clone()),
                relationships: Arc::new(new_path_relationships.clone()),
                directions: Arc::new(vec![spec.rel.direction; path_len]),
            },
        );
    }

    new_binding.push_fresh_shared_binding(
        "__edge_next_node_id__".to_owned(),
        Arc::new(BoundValue::Node {
            table_id: RelationId::new(0),
            row: Arc::new(aiondb_core::Row::new(vec![far_end.clone()])),
            raw_row: Arc::new(aiondb_core::Row::new(vec![far_end.clone()])),
            id_value: Value::Null,
            tuple_id: TupleId::new(0),
            labels: Arc::new(Vec::new()),
            column_names: Arc::new(Vec::new()),
        }),
    );

    if depth >= min_hops {
        ensure_graph_result_row_capacity(context, output.len())?;
        context.track_memory(estimate_binding_row_bytes(&new_binding))?;
        output.push(new_binding.clone());
    }

    if depth < max_hops {
        let mut new_path_edges = entry.path_edges.clone();
        new_path_edges.insert(edge_key);
        context.track_memory(estimate_variable_frontier_entry_bytes(
            &far_end,
            &new_binding,
            new_path_edges.len(),
        ))?;
        ensure_graph_workset_capacity(context, next_frontier.len(), "variable-length frontier")?;
        next_frontier.push(VariableTraversalFrontierEntry {
            node_id: far_end,
            binding: new_binding,
            path_edges: new_path_edges,
            path_nodes: new_path_nodes,
            path_relationships: new_path_relationships,
        });
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) fn build_traversed_edge_binding(
    binding: &BindingRow,
    rel: &CypherRelPattern,
    table_id: RelationId,
    compat_row: SharedRow,
    raw_row: SharedRow,
    tuple_id: TupleId,
    edge_rel_type: &SharedText,
    edge_col_names: &SharedStrings,
    current_node_id: Option<&Value>,
    source_id: &Value,
    target_id: &Value,
) -> BindingRow {
    let mut new_binding = binding.clone();

    if let Some(ref var) = rel.variable {
        new_binding = new_binding.with_binding(
            var,
            BoundValue::Edge {
                table_id,
                row: Arc::clone(&compat_row),
                raw_row: Arc::clone(&raw_row),
                tuple_id,
                rel_type: Arc::clone(edge_rel_type),
                column_names: Arc::clone(edge_col_names),
            },
        );
    }

    let next_node_id = match rel.direction {
        CypherRelDirection::Outgoing => target_id.clone(),
        CypherRelDirection::Incoming => source_id.clone(),
        CypherRelDirection::Both => {
            if current_node_id.is_some_and(|current| *current == *source_id) {
                target_id.clone()
            } else {
                source_id.clone()
            }
        }
    };

    new_binding.push_fresh_shared_binding(
        "__edge_next_node_id__".to_owned(),
        Arc::new(BoundValue::Node {
            table_id: RelationId::new(0),
            row: Arc::new(aiondb_core::Row::new(vec![next_node_id.clone()])),
            raw_row: Arc::new(aiondb_core::Row::new(vec![next_node_id])),
            id_value: Value::Null,
            tuple_id: TupleId::new(0),
            labels: Arc::new(Vec::new()),
            column_names: Arc::new(Vec::new()),
        }),
    );

    new_binding
}

impl Executor {
    /// Dispatch a relationship pattern step: try adjacency index lookup first,
    /// falling back to a full table scan when the storage backend does not
    /// support adjacency indexes. Variable-length patterns (`*min..max`) are
    /// handled via iterative BFS expansion.
    pub(super) fn match_relationship(
        &self,
        context: &ExecutionContext,
        current_node: &CypherNodePattern,
        rel: &CypherRelPattern,
        next_node: Option<&CypherNodePattern>,
        input_bindings: Vec<BindingRow>,
        excluded_next_node_id_vars: &[String],
        path_variable: Option<&str>,
        runtime_cache: &mut GraphMatchRuntimeCache,
    ) -> DbResult<Vec<BindingRow>> {
        let rel_variants = self.relationship_pattern_variants(context, rel)?;
        if rel_variants.is_empty() {
            return Ok(Vec::new());
        }

        if rel.min_hops.is_some() || rel.max_hops.is_some() {
            return self.match_variable_length_relationship(
                context,
                current_node,
                &rel_variants,
                next_node,
                input_bindings,
                path_variable,
            );
        }

        if rel_variants.len() > 1 {
            let mut output = Vec::new();
            for variant in &rel_variants {
                let bindings = self.adjacency_match_relationship(
                    context,
                    current_node,
                    variant,
                    next_node,
                    input_bindings.clone(),
                    excluded_next_node_id_vars,
                    path_variable,
                    runtime_cache,
                )?;
                output.extend(bindings);
            }
            return Ok(output);
        }

        let Some(rel) = rel_variants.first() else {
            return Ok(Vec::new());
        };
        self.adjacency_match_relationship(
            context,
            current_node,
            rel,
            next_node,
            input_bindings,
            excluded_next_node_id_vars,
            path_variable,
            runtime_cache,
        )
    }

    fn relationship_pattern_variants(
        &self,
        context: &ExecutionContext,
        rel: &CypherRelPattern,
    ) -> DbResult<Vec<CypherRelPattern>> {
        let mut variants = Vec::new();
        let mut seen_labels = HashSet::new();

        let mut push_variant =
            |label: Option<String>, table_id: RelationId, base: &CypherRelPattern| {
                let key = (table_id.get(), label.clone().unwrap_or_default());
                if !seen_labels.insert(key) {
                    return;
                }
                variants.push(CypherRelPattern {
                    rel_type: label,
                    rel_type_alternatives: Vec::new(),
                    table_id: Some(table_id),
                    ..base.clone()
                });
            };

        if rel.rel_type.is_none() && rel.rel_type_alternatives.is_empty() && rel.table_id.is_none()
        {
            for label in self.catalog_reader.list_edge_labels(context.txn_id)? {
                push_variant(Some(label.label.clone()), label.table_id, rel);
            }
            return Ok(variants);
        }

        if let Some(ref rel_type) = rel.rel_type {
            if let Some(table_id) = rel.table_id {
                push_variant(Some(rel_type.clone()), table_id, rel);
            } else if let Some(label) = self
                .catalog_reader
                .get_edge_label(context.txn_id, rel_type)?
            {
                push_variant(Some(label.label.clone()), label.table_id, rel);
            }
        } else if let Some(table_id) = rel.table_id {
            push_variant(None, table_id, rel);
        }

        for rel_type in &rel.rel_type_alternatives {
            if let Some(label) = self
                .catalog_reader
                .get_edge_label(context.txn_id, rel_type)?
            {
                push_variant(Some(label.label.clone()), label.table_id, rel);
            }
        }

        Ok(variants)
    }

    pub(super) fn match_variable_length_relationship(
        &self,
        context: &ExecutionContext,
        current_node: &CypherNodePattern,
        rel_variants: &[CypherRelPattern],
        next_node: Option<&CypherNodePattern>,
        input_bindings: Vec<BindingRow>,
        path_variable: Option<&str>,
    ) -> DbResult<Vec<BindingRow>> {
        let Some(rel) = rel_variants.first() else {
            return Ok(Vec::new());
        };

        let min_hops = usize::try_from(rel.min_hops.unwrap_or(1)).unwrap_or(usize::MAX);
        let max_hops = usize::try_from(rel.max_hops.unwrap_or(10)).unwrap_or(usize::MAX);

        let traversal_specs =
            self.relationship_traversal_specs(context, rel_variants, path_variable)?;
        if traversal_specs.is_empty() {
            return Ok(Vec::new());
        }

        let mut output = Vec::new();

        for binding in &input_bindings {
            context.check_deadline()?;

            let Some(start_id) = self.find_current_node_id_for_pattern(binding, Some(current_node))
            else {
                continue;
            };
            let initial_path_nodes = if path_variable.is_some() {
                vec![self.path_node_literal_from_binding_or_fetch(
                    context,
                    current_node,
                    binding,
                    &start_id,
                )?]
            } else {
                Vec::new()
            };
            let mut frontier = vec![VariableTraversalFrontierEntry {
                node_id: start_id.clone(),
                binding: binding.clone(),
                path_edges: HashSet::new(),
                path_nodes: initial_path_nodes,
                path_relationships: Vec::new(),
            }];
            context.track_memory(estimate_variable_frontier_entry_bytes(
                &start_id, binding, 0,
            ))?;

            for depth in 1..=max_hops {
                if frontier.is_empty() {
                    break;
                }
                context.check_deadline()?;

                let mut next_frontier = Vec::new();

                for entry in &frontier {
                    for spec in &traversal_specs {
                        let edge_records = self.collect_adjacent_edges(
                            context,
                            spec.table_id,
                            &entry.node_id,
                            spec.rel.direction,
                            spec.src_col_idx,
                            spec.tgt_col_idx,
                            spec.use_table_adjacency,
                            spec.edge_rls_policies.as_deref(),
                            spec.projected_scan.as_ref(),
                        )?;

                        for edge_record in &edge_records {
                            context.check_deadline()?;

                            let edge_key = (spec.table_id, edge_record.tuple_id);
                            if entry.path_edges.contains(&edge_key) {
                                continue;
                            }

                            let scan_column_names = spec.projected_scan.as_ref().map_or_else(
                                || spec.edge_col_names.as_slice(),
                                |projection| {
                                    projection.scan_column_names(edge_record.native_endpoints)
                                },
                            );

                            if !self.check_property_filters(
                                context,
                                &spec.rel.properties,
                                scan_column_names,
                                edge_record.compat_row.as_ref(),
                                &entry.binding,
                            )? {
                                continue;
                            }

                            let far_end = far_end_for_direction(
                                &entry.node_id,
                                spec.rel.direction,
                                &edge_record.source_id,
                                &edge_record.target_id,
                            );

                            let path_edge_literal = path_variable.map(|_| {
                                format_cypher_edge_literal(
                                    spec.edge_col_names.as_ref(),
                                    edge_record.compat_row.as_ref(),
                                    spec.edge_rel_type.as_ref(),
                                )
                            });
                            let next_node_literal = if path_variable.is_some() {
                                Some(self.fetch_path_node_literal(context, next_node, &far_end)?)
                            } else {
                                None
                            };
                            record_variable_traversal_step(
                                context,
                                &mut output,
                                &mut next_frontier,
                                entry,
                                spec,
                                &edge_record.compat_row,
                                &edge_record.raw_row,
                                edge_record.tuple_id,
                                far_end,
                                edge_key,
                                depth,
                                min_hops,
                                max_hops,
                                path_variable,
                                path_edge_literal,
                                next_node_literal,
                            )?;
                        }
                    }
                }

                frontier = next_frontier;
            }
        }

        Ok(output)
    }

    fn build_neighbor_marker_binding(binding: &BindingRow, next_node_id: Value) -> BindingRow {
        let mut new_binding = binding.clone();
        let marker_row = Arc::new(aiondb_core::Row::new(vec![next_node_id.clone()]));
        new_binding.push_fresh_shared_binding(
            "__edge_next_node_id__".to_owned(),
            Arc::new(BoundValue::Node {
                table_id: RelationId::new(0),
                row: Arc::clone(&marker_row),
                raw_row: marker_row,
                id_value: next_node_id,
                tuple_id: aiondb_core::TupleId::new(0),
                labels: Arc::new(Vec::new()),
                column_names: Arc::new(Vec::new()),
            }),
        );
        new_binding
    }

    pub(super) fn adjacency_match_relationship(
        &self,
        context: &ExecutionContext,
        current_node: &CypherNodePattern,
        rel: &CypherRelPattern,
        next_node: Option<&CypherNodePattern>,
        input_bindings: Vec<BindingRow>,
        excluded_next_node_id_vars: &[String],
        _path_variable: Option<&str>,
        runtime_cache: &mut GraphMatchRuntimeCache,
    ) -> DbResult<Vec<BindingRow>> {
        let Some(table_id) = rel.table_id else {
            return Ok(input_bindings);
        };

        let ((src_col_idx, tgt_col_idx), use_table_adjacency) =
            self.resolve_edge_endpoint_columns_for_rel(context, table_id, rel.rel_type.as_deref())?;
        let edge_rel_type: SharedText = Arc::from(rel.rel_type.as_deref().unwrap_or("").to_owned());
        let edge_table_descriptor = self
            .catalog_reader
            .get_table_by_id(context.txn_id, table_id)?;
        let edge_col_names: SharedStrings = Arc::new(
            edge_table_descriptor
                .as_ref()
                .map(|t| t.columns.iter().map(|c| c.name.clone()).collect::<Vec<_>>())
                .unwrap_or_default(),
        );
        let include_oid_system_column =
            self.compat_include_oid_system_column_for_table_id(context, table_id)?;
        let edge_rls_policies = match edge_table_descriptor.as_ref() {
            Some(table) => self.compile_compat_rls_policies(
                table,
                super::dml_plans::CompatRlsAction::Select,
                context,
            )?,
            None => None,
        };
        let projected_scan = if rel.variable.is_none() && edge_rls_policies.is_none() {
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
        let neighbor_only_adjacency = rel.variable.is_none() && rel.properties.is_empty();
        let next_node_candidate_ids = if neighbor_only_adjacency {
            next_node
                .map(|node| self.collect_static_node_candidate_id_keys(context, node))
                .transpose()?
                .flatten()
        } else {
            None
        };
        if next_node_candidate_ids
            .as_ref()
            .is_some_and(HashSet::is_empty)
        {
            return Ok(Vec::new());
        }

        let mut output = Vec::new();
        let has_interrupts = context.has_execution_interrupts();
        let mut tid_counter: u32 = 0;
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

        for (binding_idx, binding) in input_bindings.iter().enumerate() {
            if has_interrupts && binding_idx.trailing_zeros() >= 9 {
                context.check_deadline()?;
            }
            let excluded_next_node_id_keys =
                collect_excluded_next_node_id_keys(binding, excluded_next_node_id_vars);

            let current_id = self.find_current_node_id_for_pattern(binding, Some(current_node));
            let directions: &[(bool, bool)] = match rel.direction {
                CypherRelDirection::Outgoing => &[(true, false)],
                CypherRelDirection::Incoming => &[(false, true)],
                CypherRelDirection::Both => &[(true, false), (false, true)],
            };

            let mut used_adjacency = false;
            if let (true, Some(node_id)) = (use_table_adjacency, current_id.as_ref()) {
                let mut adj_ok = true;
                for &(is_outgoing, _) in directions {
                    if neighbor_only_adjacency {
                        let cached_neighbors = value_to_bfs_key(node_id)
                            .map(|node_key| (table_id, node_key, is_outgoing))
                            .and_then(|cache_key| {
                                runtime_cache
                                    .adjacency_neighbor_cache
                                    .get(&cache_key)
                                    .cloned()
                            });
                        if let Some(neighbor_ids) = cached_neighbors {
                            used_adjacency = true;
                            for neighbor_id in neighbor_ids.iter().cloned() {
                                if has_interrupts {
                                    tid_counter = tid_counter.wrapping_add(1);
                                    if tid_counter.trailing_zeros() >= 10 {
                                        context.check_deadline()?;
                                    }
                                }
                                if !adjacency_neighbor_allowed(
                                    &neighbor_id,
                                    next_node_candidate_ids.as_ref(),
                                ) {
                                    continue;
                                }
                                if excluded_next_node_id_match(
                                    &excluded_next_node_id_keys,
                                    &neighbor_id,
                                ) {
                                    continue;
                                }
                                let new_binding =
                                    Self::build_neighbor_marker_binding(binding, neighbor_id);
                                push_graph_binding(context, &mut output, new_binding)?;
                            }
                        } else if let Some(node_key) =
                            value_to_bfs_key(node_id).filter(|_| traversal.uses_traversal_store())
                        {
                            let neighbor_ids =
                                Arc::new(self.fast_graph_adjacency_neighbors_cached(
                                    context,
                                    table_id,
                                    node_id,
                                    is_outgoing,
                                )?);
                            runtime_cache.adjacency_neighbor_cache.insert(
                                (table_id, node_key, is_outgoing),
                                Arc::clone(&neighbor_ids),
                            );
                            used_adjacency = true;
                            for neighbor_id in neighbor_ids.iter().cloned() {
                                if has_interrupts {
                                    tid_counter = tid_counter.wrapping_add(1);
                                    if tid_counter.trailing_zeros() >= 10 {
                                        context.check_deadline()?;
                                    }
                                }
                                if !adjacency_neighbor_allowed(
                                    &neighbor_id,
                                    next_node_candidate_ids.as_ref(),
                                ) {
                                    continue;
                                }
                                if excluded_next_node_id_match(
                                    &excluded_next_node_id_keys,
                                    &neighbor_id,
                                ) {
                                    continue;
                                }
                                let new_binding =
                                    Self::build_neighbor_marker_binding(binding, neighbor_id);
                                push_graph_binding(context, &mut output, new_binding)?;
                            }
                        } else {
                            let mut cursor = GraphStorage::neighbor_ids(
                                traversal_store,
                                node_id,
                                if is_outgoing {
                                    GraphDirection::Outgoing
                                } else {
                                    GraphDirection::Incoming
                                },
                            );
                            let mut saw_neighbor = false;
                            used_adjacency = true;
                            while let Some(neighbor_id) = cursor.next_neighbor() {
                                saw_neighbor = true;
                                if has_interrupts {
                                    tid_counter = tid_counter.wrapping_add(1);
                                    if tid_counter.trailing_zeros() >= 10 {
                                        context.check_deadline()?;
                                    }
                                }
                                if !adjacency_neighbor_allowed(
                                    &neighbor_id,
                                    next_node_candidate_ids.as_ref(),
                                ) {
                                    continue;
                                }
                                if excluded_next_node_id_match(
                                    &excluded_next_node_id_keys,
                                    &neighbor_id,
                                ) {
                                    continue;
                                }
                                let new_binding =
                                    Self::build_neighbor_marker_binding(binding, neighbor_id);
                                push_graph_binding(context, &mut output, new_binding)?;
                            }
                            if !saw_neighbor && !traversal.uses_traversal_store() {
                                debug!(
                                    "adjacency neighbor lookup unavailable, falling back to scan"
                                );
                                adj_ok = false;
                                break;
                            }
                        }
                        continue;
                    }

                    let mut tuple_cursor = GraphStorage::edge_ids(
                        traversal_store,
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
                        if has_interrupts {
                            tid_counter = tid_counter.wrapping_add(1);
                            if tid_counter.trailing_zeros() >= 10 {
                                context.check_deadline()?;
                            }
                        }
                        let native_endpoints = GraphStorage::edge_endpoints(traversal_store, tid);
                        let maybe_row = self.storage_dml.fetch_ref(
                            context.txn_id,
                            &context.snapshot,
                            table_id,
                            tid,
                            projected_scan.as_ref().map(|projection| {
                                projection.fetch_projection(native_endpoints.is_some())
                            }),
                        )?;
                        let Some(row) = maybe_row else {
                            continue;
                        };
                        let record = aiondb_storage_api::TupleRecord {
                            tuple_id: tid,
                            heap_position: tid.get(),
                            row,
                        };
                        let compat_row = self.compat_scan_row(
                            &record,
                            include_oid_system_column,
                            Some(table_id),
                        );
                        let scan_column_names = projected_scan
                            .as_ref()
                            .map_or(edge_col_names.as_slice(), |value| {
                                value.scan_column_names(native_endpoints.is_some())
                            });
                        let (source_id, target_id) =
                            if let Some((source_id, target_id)) = native_endpoints {
                                (source_id, target_id)
                            } else {
                                let projected_src_col_idx = projected_scan
                                    .as_ref()
                                    .map_or(src_col_idx, |value| value.src_col_idx);
                                let projected_tgt_col_idx = projected_scan
                                    .as_ref()
                                    .map_or(tgt_col_idx, |value| value.tgt_col_idx);
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

                        if !self.check_adjacency(
                            binding,
                            Some(current_node),
                            rel.direction,
                            &source_id,
                            &target_id,
                        ) {
                            continue;
                        }

                        if !self.check_property_filters(
                            context,
                            &rel.properties,
                            scan_column_names,
                            &compat_row,
                            binding,
                        )? {
                            continue;
                        }

                        let next_node_id = match rel.direction {
                            CypherRelDirection::Outgoing => target_id.clone(),
                            CypherRelDirection::Incoming => source_id.clone(),
                            CypherRelDirection::Both => {
                                if current_id
                                    .as_ref()
                                    .is_some_and(|current| *current == source_id)
                                {
                                    target_id.clone()
                                } else {
                                    source_id.clone()
                                }
                            }
                        };
                        if excluded_next_node_id_match(&excluded_next_node_id_keys, &next_node_id) {
                            continue;
                        }

                        let new_binding = build_traversed_edge_binding(
                            binding,
                            rel,
                            table_id,
                            Arc::new(compat_row),
                            Arc::new(record.row),
                            record.tuple_id,
                            &edge_rel_type,
                            &edge_col_names,
                            current_id.as_ref(),
                            &source_id,
                            &target_id,
                        );
                        push_graph_binding(context, &mut output, new_binding)?;
                    }
                    if !saw_tuple && !traversal.uses_traversal_store() {
                        debug!("adjacency lookup unavailable, falling back to scan");
                        adj_ok = false;
                        break;
                    }
                }
                if !adj_ok {
                    used_adjacency = false;
                }
            }

            if !used_adjacency {
                if !use_table_adjacency {
                    if let Some(node_id) = current_id.as_ref() {
                        if let Some(edge_records) = self.collect_indexed_adjacent_edges(
                            context,
                            table_id,
                            node_id,
                            rel.direction,
                            src_col_idx,
                            tgt_col_idx,
                            include_oid_system_column,
                            projected_scan.as_ref(),
                        )? {
                            for (compat_row, raw_row, tuple_id, source_id, target_id) in
                                edge_records
                            {
                                if has_interrupts {
                                    tid_counter = tid_counter.wrapping_add(1);
                                    if tid_counter.trailing_zeros() >= 10 {
                                        context.check_deadline()?;
                                    }
                                }
                                if !self.check_property_filters(
                                    context,
                                    &rel.properties,
                                    projected_scan
                                        .as_ref()
                                        .map_or(edge_col_names.as_ref(), |value| {
                                            value.column_names.as_ref()
                                        }),
                                    compat_row.as_ref(),
                                    binding,
                                )? {
                                    continue;
                                }

                                let next_node_id = match rel.direction {
                                    CypherRelDirection::Outgoing => target_id.clone(),
                                    CypherRelDirection::Incoming => source_id.clone(),
                                    CypherRelDirection::Both => {
                                        if current_id
                                            .as_ref()
                                            .is_some_and(|current| *current == source_id)
                                        {
                                            target_id.clone()
                                        } else {
                                            source_id.clone()
                                        }
                                    }
                                };
                                if excluded_next_node_id_match(
                                    &excluded_next_node_id_keys,
                                    &next_node_id,
                                ) {
                                    continue;
                                }

                                let new_binding = build_traversed_edge_binding(
                                    binding,
                                    rel,
                                    table_id,
                                    Arc::clone(&compat_row),
                                    Arc::clone(&raw_row),
                                    tuple_id,
                                    &edge_rel_type,
                                    &edge_col_names,
                                    current_id.as_ref(),
                                    &source_id,
                                    &target_id,
                                );
                                push_graph_binding(context, &mut output, new_binding)?;
                            }
                            continue;
                        }
                    }
                }

                let mut stream = self.scan_table_locked(
                    context,
                    table_id,
                    projected_scan
                        .as_ref()
                        .map(|projection| projection.projected_columns.clone()),
                )?;
                let mut scan_counter: u32 = 0;
                while let Some(record) = stream.next()? {
                    if has_interrupts {
                        scan_counter = scan_counter.wrapping_add(1);
                        if scan_counter.trailing_zeros() >= 10 {
                            context.check_deadline()?;
                        }
                    }
                    if !self.compat_rls_allows_existing_row(
                        edge_rls_policies.as_deref(),
                        &record.row,
                        context,
                    )? {
                        continue;
                    }
                    let compat_row =
                        self.compat_scan_row(&record, include_oid_system_column, Some(table_id));
                    let scan_column_names = projected_scan
                        .as_ref()
                        .map_or(edge_col_names.as_ref(), |value| value.column_names.as_ref());
                    let projected_src_col_idx = projected_scan
                        .as_ref()
                        .map_or(src_col_idx, |value| value.src_col_idx);
                    let projected_tgt_col_idx = projected_scan
                        .as_ref()
                        .map_or(tgt_col_idx, |value| value.tgt_col_idx);

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

                    if !self.check_adjacency(
                        binding,
                        Some(current_node),
                        rel.direction,
                        &source_id,
                        &target_id,
                    ) {
                        continue;
                    }

                    if !self.check_property_filters(
                        context,
                        &rel.properties,
                        scan_column_names,
                        &compat_row,
                        binding,
                    )? {
                        continue;
                    }

                    let next_node_id = match rel.direction {
                        CypherRelDirection::Outgoing => target_id.clone(),
                        CypherRelDirection::Incoming => source_id.clone(),
                        CypherRelDirection::Both => {
                            if current_id
                                .as_ref()
                                .is_some_and(|current| *current == source_id)
                            {
                                target_id.clone()
                            } else {
                                source_id.clone()
                            }
                        }
                    };
                    if excluded_next_node_id_match(&excluded_next_node_id_keys, &next_node_id) {
                        continue;
                    }

                    let new_binding = build_traversed_edge_binding(
                        binding,
                        rel,
                        table_id,
                        Arc::new(compat_row),
                        Arc::new(record.row),
                        record.tuple_id,
                        &edge_rel_type,
                        &edge_col_names,
                        current_id.as_ref(),
                        &source_id,
                        &target_id,
                    );
                    push_graph_binding(context, &mut output, new_binding)?;
                }
            }
        }

        Ok(output)
    }
}
