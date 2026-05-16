use std::{collections::HashMap, sync::Arc};

use aiondb_core::{DbError, DbResult, RelationId, Value};
use aiondb_plan::graph::{CypherMatchClause, CypherPattern, CypherPropertyExpr, IndexScanInfo};
use aiondb_plan::{TypedExpr, TypedExprKind};
use tracing::debug;

use super::{ExecutionContext, Executor};
use crate::executor::graph_plans::{
    build_graph_filter_conjuncts, collect_graph_filter_conjuncts, estimate_binding_row_bytes,
    exact_column_literal_equality, extract_column_literal_range, flip_relationship_direction,
    pick_match_pivot_index, BindingRow, BoundValue, GraphFilterConjunct,
};

fn materialize_named_path_pattern(pattern: &CypherPattern) -> CypherPattern {
    let Some(path_variable) = pattern.path_variable.as_deref() else {
        return pattern.clone();
    };
    let mut materialized = pattern.clone();
    let safe_name = path_variable
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>();

    for (idx, node) in materialized.nodes.iter_mut().enumerate() {
        if node.variable.is_none() {
            node.variable = Some(format!("__path_{safe_name}_node_{idx}"));
        }
    }
    for (idx, rel) in materialized.relationships.iter_mut().enumerate() {
        if rel.variable.is_none() {
            rel.variable = Some(format!("__path_{safe_name}_rel_{idx}"));
        }
    }
    materialized
}

fn validate_named_path_pattern(pattern: &CypherPattern) -> DbResult<()> {
    if pattern.path_variable.is_none() {
        return Ok(());
    }
    if pattern.path_function.is_some() {
        return Ok(());
    }
    let has_variable_length = pattern
        .relationships
        .iter()
        .any(|rel| rel.min_hops.is_some() || rel.max_hops.is_some());
    if has_variable_length && (pattern.relationships.len() != 1 || pattern.nodes.len() != 2) {
        return Err(DbError::feature_not_supported(
            "named multi-segment variable-length path bindings are not supported yet",
        ));
    }
    Ok(())
}

fn bind_named_path_variable(
    pattern: &CypherPattern,
    mut bindings: Vec<BindingRow>,
) -> Vec<BindingRow> {
    let Some(path_variable) = pattern.path_variable.as_ref() else {
        return bindings;
    };
    let nodes = Arc::new(
        pattern
            .nodes
            .iter()
            .filter_map(|node| node.variable.clone())
            .collect::<Vec<_>>(),
    );
    let relationships = Arc::new(
        pattern
            .relationships
            .iter()
            .filter_map(|rel| rel.variable.clone())
            .collect::<Vec<_>>(),
    );
    let directions = Arc::new(
        pattern
            .relationships
            .iter()
            .map(|rel| rel.direction)
            .collect::<Vec<_>>(),
    );
    for binding in &mut bindings {
        if binding.get(path_variable).is_some() {
            continue;
        }
        binding.insert_binding(
            path_variable.clone(),
            BoundValue::Path {
                nodes: Arc::clone(&nodes),
                relationships: Arc::clone(&relationships),
                directions: Arc::clone(&directions),
            },
        );
    }
    bindings
}

impl Executor {
    pub(super) fn match_pattern(
        &self,
        context: &ExecutionContext,
        pattern: &CypherPattern,
        mut bindings: Vec<BindingRow>,
        filter_conjuncts: &[GraphFilterConjunct<'_>],
    ) -> DbResult<Vec<BindingRow>> {
        for binding in &mut bindings {
            binding.remove("__edge_next_node_id__");
        }

        if let Some(ref func) = pattern.path_function {
            return self.match_shortest_path(context, pattern, *func, bindings);
        }

        if let Some(pivot) = pick_match_pivot_index(pattern) {
            return self.match_pattern_pivoted(context, pattern, bindings, filter_conjuncts, pivot);
        }

        for (i, node) in pattern.nodes.iter().enumerate() {
            bindings = self.match_node(context, node, bindings)?;
            bindings =
                self.apply_ready_graph_filter_conjuncts(context, bindings, filter_conjuncts)?;
            if bindings.is_empty() {
                return Ok(bindings);
            }
            if i < pattern.relationships.len() {
                let rel = &pattern.relationships[i];
                let next_node = pattern.nodes.get(i + 1);
                bindings = self.match_relationship(
                    context,
                    node,
                    rel,
                    next_node,
                    bindings,
                    pattern.path_variable.as_deref(),
                )?;
                bindings =
                    self.apply_ready_graph_filter_conjuncts(context, bindings, filter_conjuncts)?;
                if bindings.is_empty() {
                    return Ok(bindings);
                }
            }
        }
        Ok(bindings)
    }

    fn match_pattern_pivoted(
        &self,
        context: &ExecutionContext,
        pattern: &CypherPattern,
        bindings: Vec<BindingRow>,
        filter_conjuncts: &[GraphFilterConjunct<'_>],
        pivot: usize,
    ) -> DbResult<Vec<BindingRow>> {
        let mut bindings = self.match_node(context, &pattern.nodes[pivot], bindings)?;
        bindings = self.apply_ready_graph_filter_conjuncts(context, bindings, filter_conjuncts)?;
        if bindings.is_empty() {
            return Ok(bindings);
        }

        for left_node_idx in (0..pivot).rev() {
            let rel_idx = left_node_idx;
            let original_rel = &pattern.relationships[rel_idx];
            let flipped_rel = flip_relationship_direction(original_rel);
            let current_node = &pattern.nodes[left_node_idx + 1];
            let next_node = &pattern.nodes[left_node_idx];
            for binding in &mut bindings {
                binding.remove("__edge_next_node_id__");
            }
            bindings = self.match_relationship(
                context,
                current_node,
                &flipped_rel,
                Some(next_node),
                bindings,
                None,
            )?;
            bindings =
                self.apply_ready_graph_filter_conjuncts(context, bindings, filter_conjuncts)?;
            if bindings.is_empty() {
                return Ok(bindings);
            }
            bindings = self.match_node(context, next_node, bindings)?;
            bindings =
                self.apply_ready_graph_filter_conjuncts(context, bindings, filter_conjuncts)?;
            if bindings.is_empty() {
                return Ok(bindings);
            }
        }

        for right_node_idx in (pivot + 1)..pattern.nodes.len() {
            let rel_idx = right_node_idx - 1;
            let original_rel = &pattern.relationships[rel_idx];
            let current_node = &pattern.nodes[right_node_idx - 1];
            let next_node = &pattern.nodes[right_node_idx];
            for binding in &mut bindings {
                binding.remove("__edge_next_node_id__");
            }
            bindings = self.match_relationship(
                context,
                current_node,
                original_rel,
                Some(next_node),
                bindings,
                None,
            )?;
            bindings =
                self.apply_ready_graph_filter_conjuncts(context, bindings, filter_conjuncts)?;
            if bindings.is_empty() {
                return Ok(bindings);
            }
            bindings = self.match_node(context, next_node, bindings)?;
            bindings =
                self.apply_ready_graph_filter_conjuncts(context, bindings, filter_conjuncts)?;
            if bindings.is_empty() {
                return Ok(bindings);
            }
        }
        Ok(bindings)
    }

    pub(super) fn apply_ready_graph_filter_conjuncts(
        &self,
        context: &ExecutionContext,
        bindings: Vec<BindingRow>,
        filter_conjuncts: &[GraphFilterConjunct<'_>],
    ) -> DbResult<Vec<BindingRow>> {
        if filter_conjuncts.is_empty() || bindings.is_empty() {
            return Ok(bindings);
        }

        let mut filtered = Vec::with_capacity(bindings.len());
        'binding: for binding in bindings {
            for conjunct in filter_conjuncts {
                if !conjunct.is_ready(&binding) {
                    continue;
                }
                if !self.evaluate_graph_predicate(context, conjunct.expr, &binding)? {
                    continue 'binding;
                }
            }
            filtered.push(binding);
        }
        Ok(filtered)
    }

    pub(in crate::executor) fn execute_cypher_match(
        &self,
        context: &ExecutionContext,
        clause: &CypherMatchClause,
        input_bindings: Vec<BindingRow>,
    ) -> DbResult<Vec<BindingRow>> {
        let col_count_cache: HashMap<RelationId, usize> = if clause.optional {
            let mut cache = HashMap::new();
            for pattern in &clause.patterns {
                for node in &pattern.nodes {
                    if let Some(tid) = node.table_id {
                        if let std::collections::hash_map::Entry::Vacant(entry) = cache.entry(tid) {
                            let count = self
                                .catalog_reader
                                .get_table_by_id(context.txn_id, tid)?
                                .map_or(0, |t| t.columns.len());
                            entry.insert(count);
                        }
                    }
                }
                for rel in &pattern.relationships {
                    if let Some(tid) = rel.table_id {
                        if let std::collections::hash_map::Entry::Vacant(entry) = cache.entry(tid) {
                            let count = self
                                .catalog_reader
                                .get_table_by_id(context.txn_id, tid)?
                                .map_or(0, |t| t.columns.len());
                            entry.insert(count);
                        }
                    }
                }
            }
            cache
        } else {
            HashMap::new()
        };

        let filter_conjuncts = clause
            .filter
            .as_ref()
            .map(build_graph_filter_conjuncts)
            .unwrap_or_default();

        let hinted_patterns;
        let inline_first;
        let patterns: &[CypherPattern] = if let Some(filter) = clause.filter.as_ref() {
            inline_first = clause
                .patterns
                .iter()
                .map(|pattern| self.apply_inline_property_index_hints(context, pattern))
                .collect::<DbResult<Vec<_>>>()?;
            hinted_patterns = inline_first
                .iter()
                .map(|pattern| self.apply_match_filter_index_hints(context, pattern, filter))
                .collect::<DbResult<Vec<_>>>()?;
            &hinted_patterns
        } else {
            inline_first = clause
                .patterns
                .iter()
                .map(|pattern| self.apply_inline_property_index_hints(context, pattern))
                .collect::<DbResult<Vec<_>>>()?;
            &inline_first
        };

        for (pattern_idx, pattern) in patterns.iter().enumerate() {
            let graph_plan = self.describe_cypher_pattern_graph_plan(context, pattern);
            debug!(
                pattern_idx,
                source = ?graph_plan.source,
                fallback_source = ?graph_plan.fallback_source,
                estimated_rows = graph_plan.estimated_rows,
                reason = graph_plan.reason.as_deref().unwrap_or(""),
                "cypher MATCH graph access plan"
            );
        }

        let mut result_bindings = Vec::new();

        for input_binding in &input_bindings {
            context.check_deadline()?;
            let mut current_bindings = vec![input_binding.clone()];

            for pattern in patterns {
                validate_named_path_pattern(pattern)?;
                let materialized_pattern;
                let pattern = if pattern.path_variable.is_some() {
                    materialized_pattern = materialize_named_path_pattern(pattern);
                    &materialized_pattern
                } else {
                    pattern
                };
                current_bindings =
                    self.match_pattern(context, pattern, current_bindings, &filter_conjuncts)?;
                current_bindings = bind_named_path_variable(pattern, current_bindings);
                if current_bindings.is_empty() && !clause.optional {
                    break;
                }
            }

            if let Some(ref filter) = clause.filter {
                let mut filtered = Vec::new();
                for binding in current_bindings {
                    match self.evaluate_graph_predicate(context, filter, &binding) {
                        Ok(true) => filtered.push(binding),
                        Ok(false) => {}
                        Err(e) => return Err(e),
                    }
                }
                current_bindings = filtered;
            }

            if current_bindings.is_empty() && clause.optional {
                let mut null_binding = input_binding.clone();
                for pattern in &clause.patterns {
                    for node in &pattern.nodes {
                        if let Some(ref var) = node.variable {
                            let col_count = node
                                .table_id
                                .and_then(|tid| col_count_cache.get(&tid).copied())
                                .unwrap_or(0);
                            null_binding.insert_binding(
                                var.clone(),
                                BoundValue::Null {
                                    column_count: col_count,
                                },
                            );
                        }
                    }
                    for rel in &pattern.relationships {
                        if let Some(ref var) = rel.variable {
                            let col_count = rel
                                .table_id
                                .and_then(|tid| col_count_cache.get(&tid).copied())
                                .unwrap_or(0);
                            null_binding.insert_binding(
                                var.clone(),
                                BoundValue::Null {
                                    column_count: col_count,
                                },
                            );
                        }
                    }
                }
                crate::executor::graph_plans::ensure_graph_result_row_capacity(
                    context,
                    result_bindings.len(),
                )?;
                context.track_memory(estimate_binding_row_bytes(&null_binding))?;
                result_bindings.push(null_binding);
            } else {
                for binding in current_bindings {
                    crate::executor::graph_plans::ensure_graph_result_row_capacity(
                        context,
                        result_bindings.len(),
                    )?;
                    context.track_memory(estimate_binding_row_bytes(&binding))?;
                    result_bindings.push(binding);
                }
            }
        }

        Ok(result_bindings)
    }

    fn apply_inline_property_index_hints(
        &self,
        context: &ExecutionContext,
        pattern: &CypherPattern,
    ) -> DbResult<CypherPattern> {
        let mut hinted = pattern.clone();
        for node in &mut hinted.nodes {
            if node.index_scan.is_some() {
                continue;
            }
            let Some(table_id) = node.table_id else {
                continue;
            };
            if node.properties.is_empty() {
                continue;
            }
            let Some(table) = self
                .catalog_reader
                .get_table_by_id(context.txn_id, table_id)?
            else {
                continue;
            };
            for prop in &node.properties {
                let scan_value = match &prop.value {
                    TypedExpr {
                        kind: TypedExprKind::Literal(value),
                        ..
                    } if !matches!(value, Value::Null) => value.clone(),
                    _ => continue,
                };
                let Some(column_index) = self.find_column_index(&table.columns, &prop.key) else {
                    continue;
                };
                let Some(index_id) =
                    self.find_btree_index_for_column_ordinal(context, table_id, column_index)?
                else {
                    continue;
                };
                node.index_scan = Some(IndexScanInfo {
                    index_id,
                    column_index,
                    scan_value,
                });
                break;
            }
        }
        Ok(hinted)
    }

    fn apply_match_filter_index_hints(
        &self,
        context: &ExecutionContext,
        pattern: &CypherPattern,
        filter: &TypedExpr,
    ) -> DbResult<CypherPattern> {
        let mut hinted = pattern.clone();
        let mut conjuncts = Vec::new();
        collect_graph_filter_conjuncts(filter, &mut conjuncts);

        for node in &mut hinted.nodes {
            if node.index_scan.is_some() {
                continue;
            }
            let (Some(table_id), Some(variable)) = (node.table_id, node.variable.as_deref()) else {
                continue;
            };
            let Some(table) = self
                .catalog_reader
                .get_table_by_id(context.txn_id, table_id)?
            else {
                continue;
            };
            let mut chosen_index = false;
            for conjunct in &conjuncts {
                let Some((column_ref, scan_value)) = exact_column_literal_equality(conjunct) else {
                    continue;
                };
                let Some(property) = column_ref
                    .strip_prefix(variable)
                    .and_then(|tail| tail.strip_prefix('.'))
                else {
                    continue;
                };
                let Some(column_index) = self.find_column_index(&table.columns, property) else {
                    continue;
                };
                if !chosen_index {
                    if let Some(index_id) =
                        self.find_btree_index_for_column_ordinal(context, table_id, column_index)?
                    {
                        node.index_scan = Some(IndexScanInfo {
                            index_id,
                            column_index,
                            scan_value: scan_value.clone(),
                        });
                        chosen_index = true;
                        continue;
                    }
                }
                let already = node
                    .properties
                    .iter()
                    .any(|p| p.key.eq_ignore_ascii_case(property));
                if !already {
                    let value_type = scan_value
                        .data_type()
                        .unwrap_or(aiondb_core::DataType::Text);
                    node.properties.push(CypherPropertyExpr {
                        key: property.to_owned(),
                        value: TypedExpr {
                            kind: TypedExprKind::Literal(scan_value),
                            data_type: value_type,
                            nullable: true,
                        },
                    });
                }
            }
            for conjunct in &conjuncts {
                let Some((column_ref, lower, upper)) = extract_column_literal_range(conjunct)
                else {
                    continue;
                };
                let Some(property) = column_ref
                    .strip_prefix(variable)
                    .and_then(|tail| tail.strip_prefix('.'))
                else {
                    continue;
                };
                let Some(column_index) = self.find_column_index(&table.columns, property) else {
                    continue;
                };
                let Some(column) = table.columns.get(column_index) else {
                    continue;
                };
                node.range_pushdown
                    .push(aiondb_plan::graph::CypherRangePushdown {
                        column_id: column.column_id,
                        lower,
                        upper,
                    });
            }
        }

        Ok(hinted)
    }
}
