use std::{
    collections::{HashMap, HashSet},
    ops::Bound,
    sync::Arc,
    time::Instant,
};

use aiondb_core::{DbResult, RelationId, Value};
use aiondb_eval::{build_hash_key, ValueHashKey};
use aiondb_optimizer::graph_optimizer::{GraphOptimizer, GraphStats};
use aiondb_plan::graph::{
    CypherMatchClause, CypherNodePattern, CypherPattern, CypherPropertyExpr, IndexScanInfo,
};
use aiondb_plan::{ScalarFunction, TypedExpr, TypedExprKind};
use tracing::debug;

use super::{ExecutionContext, Executor};
use crate::executor::graph_plans::{
    build_graph_filter_conjuncts, collect_graph_filter_conjuncts,
    compact_graph_binding_node_payloads, compact_node_bound_value, estimate_binding_row_bytes,
    exact_column_literal_equality, extract_column_literal_range, flip_relationship_direction,
    graph_access_clause_profile_input_key, graph_access_clause_profile_output_key,
    graph_access_clause_runtime_blocker_key, graph_access_clause_runtime_reason_key,
    graph_access_clause_runtime_strategy_key,
    graph_access_pattern_pivot_decision_key,
    graph_access_pattern_pivot_driver_key,
    graph_access_pattern_pivot_reason_key,
    graph_access_pattern_runtime_reason_key,
    graph_access_pattern_runtime_strategy_key,
    graph_access_clause_profile_time_key, graph_access_pattern_profile_time_key,
    graph_access_profile_key, graph_bound_edge_literal, graph_bound_node_literal,
    graph_filter_node_id_inequality_peers, pick_match_pivot_index,
    retain_graph_binding_variables, BindingRow, BoundValue, GraphBindingReduction,
    GraphFilterConjunct, GraphMatchRuntimeCache, SharedStrings, SharedText,
};

const RELATION_SEEDED_NODE_CANDIDATE_THRESHOLD: usize = 8;
const RELATION_SEEDED_NODE_CANDIDATE_SOFT_THRESHOLD: usize = 16;

fn observed_clause_runtime_reason(
    clause: &CypherMatchClause,
    observed_strategy: &str,
) -> &'static str {
    let Some(anchor) = clause.patterns.first().and_then(|pattern| pattern.nodes.first()) else {
        return "empty_clause";
    };
    if observed_strategy == "shared_anchor_star" {
        return "all_patterns_single_hop_same_anchor";
    }
    if clause.optional {
        "optional_clause"
    } else if clause.patterns.len() < 2 {
        "single_pattern_clause"
    } else if clause
        .patterns
        .iter()
        .skip(1)
        .all(|pattern| pattern.nodes.first() == Some(anchor))
    {
        "shared_anchor_non_single_hop_or_rewritten"
    } else {
        "general_multi_pattern_clause"
    }
}

fn observed_clause_runtime_blocker(
    clause: &CypherMatchClause,
    observed_strategy: &str,
) -> Option<&'static str> {
    if observed_strategy == "shared_anchor_star" {
        return None;
    }
    if clause.optional {
        return Some("optional_clause");
    }
    if clause.patterns.len() < 2 {
        return Some("single_pattern_clause");
    }
    let anchor = clause.patterns.first().and_then(|pattern| pattern.nodes.first())?;
    if !clause
        .patterns
        .iter()
        .skip(1)
        .all(|pattern| pattern.nodes.first() == Some(anchor))
    {
        return Some("anchor_not_shared");
    }
    if clause
        .patterns
        .iter()
        .any(|pattern| pattern.path_function.is_some() || pattern.path_variable.is_some())
    {
        return Some("path_binding_or_function_present");
    }
    if clause.patterns.iter().any(|pattern| pattern.nodes.len() != 2) {
        return Some("non_two_node_pattern_present");
    }
    if clause
        .patterns
        .iter()
        .any(|pattern| pattern.relationships.len() != 1)
    {
        return Some("non_single_relationship_pattern_present");
    }
    if clause
        .patterns
        .iter()
        .any(|pattern| pattern.nodes.first() != Some(anchor))
    {
        return Some("pattern_not_single_hop_from_anchor");
    }
    Some("unknown")
}

fn runtime_pivot_node_mode(node: &CypherNodePattern) -> &'static str {
    let has_inline_id = node
        .properties
        .iter()
        .any(|property| property.key.eq_ignore_ascii_case("id"));
    if has_inline_id {
        "id_constrained"
    } else if node.index_scan.is_some() {
        "indexed"
    } else if !node.range_pushdown.is_empty() {
        "range_constrained"
    } else if node.variable.is_some() {
        "label_scan"
    } else {
        "anonymous_scan"
    }
}

fn relation_filter_is_exact(filter: &super::graph_runtime_paths::RelationshipScanFilter) -> bool {
    matches!(
        (&filter.lower, &filter.upper),
        (Bound::Included(lo), Bound::Included(hi)) if lo == hi
    )
}

fn node_candidate_bound(
    start_node_candidate_ids: Option<&HashSet<ValueHashKey>>,
    end_node_candidate_ids: Option<&HashSet<ValueHashKey>>,
) -> Option<usize> {
    [start_node_candidate_ids, end_node_candidate_ids]
        .into_iter()
        .flatten()
        .map(HashSet::len)
        .min()
}

fn column_ndistinct(
    table_stats: &aiondb_catalog::TableStatistics,
    column_id: aiondb_core::ColumnId,
) -> Option<f64> {
    table_stats
        .column_stats
        .iter()
        .find(|stats| stats.column_id == column_id)
        .and_then(|stats| (stats.ndistinct > 0.0).then_some(stats.ndistinct))
}

fn column_ndistinct_by_name(
    table: &aiondb_catalog::TableDescriptor,
    table_stats: &aiondb_catalog::TableStatistics,
    column_name: &str,
) -> Option<f64> {
    let column = table
        .columns
        .iter()
        .find(|column| column.name.eq_ignore_ascii_case(column_name))?;
    column_ndistinct(table_stats, column.column_id)
}

pub(super) fn materialize_named_path_pattern(pattern: &CypherPattern) -> CypherPattern {
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
    Ok(())
}

fn bind_named_multi_segment_variable_length_path(
    pattern: &CypherPattern,
    binding: &BindingRow,
    path_variable: &str,
) -> Option<BoundValue> {
    let BoundValue::PathValues {
        nodes: variable_nodes,
        relationships: variable_relationships,
        directions: variable_directions,
    } = binding.get(path_variable)?
    else {
        return None;
    };

    let variable_length_relationships = pattern
        .relationships
        .iter()
        .filter(|rel| rel.min_hops.is_some() || rel.max_hops.is_some())
        .count();
    if variable_length_relationships != 1
        || (pattern.relationships.len() == 1 && pattern.nodes.len() == 2)
    {
        return None;
    }

    let first_node_var = pattern.nodes.first()?.variable.as_deref()?;
    let mut node_literals = vec![
        graph_bound_node_literal(binding, first_node_var).unwrap_or_else(|| "()".to_owned()),
    ];
    let mut relationship_literals = Vec::new();
    let mut directions = Vec::new();
    let mut consumed_variable_segment = false;

    for (idx, rel) in pattern.relationships.iter().enumerate() {
        let is_variable_length = rel.min_hops.is_some() || rel.max_hops.is_some();
        if is_variable_length {
            if consumed_variable_segment {
                return None;
            }
            relationship_literals.extend(variable_relationships.iter().cloned());
            directions.extend(variable_directions.iter().copied());
            node_literals.extend(variable_nodes.iter().skip(1).cloned());
            consumed_variable_segment = true;
            continue;
        }

        let rel_var = rel.variable.as_deref()?;
        relationship_literals.push(
            graph_bound_edge_literal(binding, rel_var)
                .unwrap_or_else(|| "[]".to_owned()),
        );
        directions.push(rel.direction);

        let next_node_var = pattern.nodes.get(idx + 1)?.variable.as_deref()?;
        node_literals.push(
            graph_bound_node_literal(binding, next_node_var)
                .unwrap_or_else(|| "()".to_owned()),
        );
    }

    if !consumed_variable_segment {
        return None;
    }

    Some(BoundValue::PathValues {
        nodes: Arc::new(node_literals),
        relationships: Arc::new(relationship_literals),
        directions: Arc::new(directions),
    })
}

pub(super) fn bind_named_path_variable(
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
            if let Some(path_value) =
                bind_named_multi_segment_variable_length_path(pattern, binding, path_variable)
            {
                binding.insert_binding(path_variable.clone(), path_value);
            }
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

fn collect_pattern_binding_variables(pattern: &CypherPattern, out: &mut HashSet<String>) {
    if let Some(path_variable) = pattern.path_variable.as_ref() {
        out.insert(path_variable.clone());
    }
    for node in &pattern.nodes {
        if let Some(variable) = node.variable.as_ref() {
            out.insert(variable.clone());
        }
    }
    for rel in &pattern.relationships {
        if let Some(variable) = rel.variable.as_ref() {
            out.insert(variable.clone());
        }
    }
}

fn keep_variables_between_patterns(
    base_variables: &HashSet<String>,
    remaining_patterns: &[CypherPattern],
    filter_conjuncts: &[GraphFilterConjunct<'_>],
    required_output_variables: Option<&HashSet<String>>,
) -> HashSet<String> {
    let mut keep = base_variables.clone();
    for pattern in remaining_patterns {
        collect_pattern_binding_variables(pattern, &mut keep);
    }
    if let Some(required_output_variables) = required_output_variables {
        keep.extend(required_output_variables.iter().cloned());
    }
    for conjunct in filter_conjuncts {
        if let Some(vars) = conjunct.referenced_vars.as_ref() {
            keep.extend(vars.iter().cloned());
        }
    }
    keep
}

fn keep_variables_for_star_branch(
    pattern: &CypherPattern,
    filter_conjuncts: &[GraphFilterConjunct<'_>],
    required_output_variables: Option<&HashSet<String>>,
) -> HashSet<String> {
    let mut branch_vars = HashSet::new();
    collect_pattern_binding_variables(pattern, &mut branch_vars);

    let mut keep = HashSet::new();
    if let Some(required_output_variables) = required_output_variables {
        keep.extend(
            branch_vars
                .iter()
                .filter(|var| required_output_variables.contains(*var))
                .cloned(),
        );
    }
    for conjunct in filter_conjuncts {
        if let Some(vars) = conjunct.referenced_vars.as_ref() {
            keep.extend(
                vars.iter()
                    .filter(|var| branch_vars.contains(*var))
                    .cloned(),
            );
        }
    }
    if keep.is_empty() {
        keep = branch_vars;
    }
    keep
}

fn keep_variables_for_star_seed(
    base_variables: &HashSet<String>,
    anchor: &CypherNodePattern,
    filter_conjuncts: &[GraphFilterConjunct<'_>],
    required_output_variables: Option<&HashSet<String>>,
) -> HashSet<String> {
    let mut keep = base_variables.clone();
    if let Some(anchor_var) = anchor.variable.as_ref() {
        if required_output_variables.is_some_and(|vars| vars.contains(anchor_var))
            || filter_conjuncts.iter().any(|conjunct| {
                conjunct
                    .referenced_vars
                    .as_ref()
                    .is_some_and(|vars| vars.iter().any(|var| var == anchor_var))
            })
        {
            keep.insert(anchor_var.clone());
        }
    }
    keep
}

fn filter_conjuncts_for_star_branch<'a>(
    pattern: &CypherPattern,
    base_variables: &HashSet<String>,
    filter_conjuncts: &[GraphFilterConjunct<'a>],
) -> Vec<GraphFilterConjunct<'a>> {
    let mut branch_allowed_vars = base_variables.clone();
    collect_pattern_binding_variables(pattern, &mut branch_allowed_vars);
    filter_conjuncts
        .iter()
        .filter(|conjunct| {
            conjunct
                .referenced_vars
                .as_ref()
                .is_some_and(|vars| vars.iter().all(|var| branch_allowed_vars.contains(var)))
        })
        .cloned()
        .collect()
}

fn is_single_hop_star_pattern(pattern: &CypherPattern, anchor: &CypherNodePattern) -> bool {
    pattern.path_function.is_none()
        && pattern.path_variable.is_none()
        && pattern.nodes.len() == 2
        && pattern.relationships.len() == 1
        && pattern.nodes.first() == Some(anchor)
}

fn merge_graph_bindings(left: &BindingRow, right: &BindingRow) -> BindingRow {
    let mut merged = left.clone();
    for (name, value) in right.iter() {
        merged.insert_shared_binding(name.clone(), Arc::clone(value));
    }
    merged
}

fn cartesian_merge_graph_bindings(
    lefts: Vec<BindingRow>,
    rights: &[BindingRow],
) -> Vec<BindingRow> {
    if lefts.is_empty() || rights.is_empty() {
        return Vec::new();
    }
    let avg_left = lefts.first().map_or(0, |binding| binding.entries.len());
    let avg_right = rights.first().map_or(0, |binding| binding.entries.len());
    let mut out = Vec::with_capacity(lefts.len().saturating_mul(rights.len()));
    let Some((last_right, rights_prefix)) = rights.split_last() else {
        return out;
    };
    for left in lefts {
        // Clone `left.entries` for every right but the last; on the last right
        // we consume `left.entries` to skip one Vec<(String, Arc<_>)> clone per
        // outer iteration.
        for right in rights_prefix {
            let mut entries = Vec::with_capacity(avg_left.saturating_add(avg_right));
            entries.extend(left.entries.iter().cloned());
            entries.extend(right.entries.iter().cloned());
            out.push(BindingRow { entries });
        }
        let mut entries = Vec::with_capacity(avg_left.saturating_add(avg_right));
        entries.extend(left.entries);
        entries.extend(last_right.entries.iter().cloned());
        out.push(BindingRow { entries });
    }
    out
}

fn direct_graph_distinct_binding_variable(expr: &TypedExpr) -> Option<&str> {
    match &expr.kind {
        TypedExprKind::ColumnRef { name, .. } => {
            let (variable, property) = name.split_once('.')?;
            property.eq_ignore_ascii_case("id").then_some(variable)
        }
        TypedExprKind::ScalarFunction {
            func: ScalarFunction::Generic(function_name),
            args,
        } if function_name.eq_ignore_ascii_case("id") && args.len() == 1 => {
            let TypedExprKind::ColumnRef { name, .. } = &args[0].kind else {
                return None;
            };
            Some(name.as_str())
        }
        _ => None,
    }
}

fn binding_node_id_key(binding: &BindingRow, variable: &str) -> Option<ValueHashKey> {
    match binding.get(variable) {
        Some(BoundValue::Node { id_value, .. }) if !id_value.is_null() => {
            build_hash_key(id_value).ok()
        }
        _ => None,
    }
}

fn dedup_bindings_by_node_id(bindings: Vec<BindingRow>, variable: &str) -> Vec<BindingRow> {
    let mut seen = HashSet::new();
    let mut out = Vec::with_capacity(bindings.len());
    // Take bindings by value so we can move each row into `out` instead of
    // cloning it after the dedup check; the previous &[..] signature forced a
    // BindingRow clone for every kept binding.
    for binding in bindings {
        let Some(key) = binding_node_id_key(&binding, variable) else {
            continue;
        };
        if seen.insert(key) {
            out.push(binding);
        }
    }
    out
}

fn node_id_inequality_pair(expr: &TypedExpr) -> Option<(&str, &str)> {
    let TypedExprKind::BinaryNe { left, right } = &expr.kind else {
        return None;
    };
    let left_var = direct_graph_distinct_binding_variable(left)?;
    let right_var = direct_graph_distinct_binding_variable(right)?;
    Some((left_var, right_var))
}

fn node_has_filter_constraints(node: &CypherNodePattern) -> bool {
    !node.properties.is_empty() || node.index_scan.is_some() || !node.range_pushdown.is_empty()
}

impl Executor {
    pub(in crate::executor) fn cbo_seed_index_for_pattern(
        &self,
        context: &ExecutionContext,
        pattern: &CypherPattern,
    ) -> DbResult<Option<usize>> {
        if pattern.nodes.len() <= 1
            || pattern.path_function.is_some()
            || pattern.path_variable.is_some()
        {
            return Ok(None);
        }
        let mut stats = GraphStats::empty();
        self.collect_graph_stats_for_pattern(context, pattern, &mut stats)?;
        let optimizer = GraphOptimizer::new(stats);
        Ok(optimizer.cbo_seed_index(pattern, &[]).filter(|seed| *seed > 0))
    }

    fn collect_graph_stats_for_pattern(
        &self,
        context: &ExecutionContext,
        pattern: &CypherPattern,
        stats: &mut GraphStats,
    ) -> DbResult<()> {
        for node in &pattern.nodes {
            self.collect_graph_stats_for_node(context, node, stats)?;
        }
        for rel in &pattern.relationships {
            if let Some(rel_type) = rel.rel_type.as_deref() {
                self.collect_graph_stats_for_rel_type(context, rel_type, rel.table_id, stats)?;
            }
            for rel_type in &rel.rel_type_alternatives {
                self.collect_graph_stats_for_rel_type(context, rel_type, None, stats)?;
            }
        }
        Ok(())
    }

    fn collect_graph_stats_for_node(
        &self,
        context: &ExecutionContext,
        node: &CypherNodePattern,
        stats: &mut GraphStats,
    ) -> DbResult<()> {
        let Some(label) = node.label.as_deref() else {
            return Ok(());
        };
        let Some(table_id) = self.node_table_id_for_graph_stats(context, node)? else {
            return Ok(());
        };
        let Some(table_stats) = self.catalog_reader.get_statistics(context.txn_id, table_id)? else {
            return Ok(());
        };
        stats
            .label_cardinality
            .insert(label.to_owned(), table_stats.row_count);
        if let Some(table) = self.catalog_reader.get_table_by_id(context.txn_id, table_id)? {
            for column in &table.columns {
                if let Some(ndistinct) = column_ndistinct(&table_stats, column.column_id) {
                    stats
                        .distinct
                        .insert((label.to_owned(), column.name.clone()), ndistinct);
                }
            }
        }
        Ok(())
    }

    fn collect_graph_stats_for_rel_type(
        &self,
        context: &ExecutionContext,
        rel_type: &str,
        table_id_hint: Option<RelationId>,
        stats: &mut GraphStats,
    ) -> DbResult<()> {
        let descriptor = self.catalog_reader.get_edge_label(context.txn_id, rel_type)?;
        if let Some(descriptor) = &descriptor {
            stats.edge_endpoints.insert(
                rel_type.to_owned(),
                (
                    descriptor.source_label.clone(),
                    descriptor.target_label.clone(),
                ),
            );
        }
        let Some(table_id) = table_id_hint.or_else(|| descriptor.as_ref().map(|d| d.table_id))
        else {
            return Ok(());
        };
        let Some(table_stats) = self.catalog_reader.get_statistics(context.txn_id, table_id)? else {
            return Ok(());
        };
        stats
            .edge_cardinality
            .insert(rel_type.to_owned(), table_stats.row_count);
        let Some(endpoints) = descriptor.as_ref().and_then(|d| d.endpoints.as_ref()) else {
            return Ok(());
        };
        let Some(table) = self.catalog_reader.get_table_by_id(context.txn_id, table_id)? else {
            return Ok(());
        };
        let row_count = table_stats.row_count as f64;
        if let Some(distinct_src) =
            column_ndistinct_by_name(&table, &table_stats, &endpoints.source_id_column)
        {
            stats
                .avg_out_degree
                .insert(rel_type.to_owned(), row_count / distinct_src);
        }
        if let Some(distinct_tgt) =
            column_ndistinct_by_name(&table, &table_stats, &endpoints.target_id_column)
        {
            stats
                .avg_in_degree
                .insert(rel_type.to_owned(), row_count / distinct_tgt);
        }
        Ok(())
    }

    fn node_table_id_for_graph_stats(
        &self,
        context: &ExecutionContext,
        node: &CypherNodePattern,
    ) -> DbResult<Option<RelationId>> {
        if let Some(table_id) = node.table_id {
            return Ok(Some(table_id));
        }
        let Some(label) = node.label.as_deref() else {
            return Ok(None);
        };
        Ok(self
            .catalog_reader
            .get_node_label(context.txn_id, label)?
            .map(|descriptor| descriptor.table_id))
    }

    fn relation_seed_support_score(
        &self,
        context: &ExecutionContext,
        table_id: RelationId,
        pushed_filters: &[super::graph_runtime_paths::RelationshipScanFilter],
    ) -> DbResult<u8> {
        let indexes = self.catalog_reader.list_indexes(context.txn_id, table_id)?;
        let has_indexed_exact = pushed_filters.iter().any(|filter| {
            relation_filter_is_exact(filter)
                && indexes.iter().any(|index| {
                    index.kind == aiondb_catalog::IndexKind::BTree
                        && index
                            .key_columns
                            .first()
                            .is_some_and(|key| key.column_id == filter.column_id)
                })
        });
        if has_indexed_exact {
            return Ok(0);
        }
        let has_indexed_range = pushed_filters.iter().any(|filter| {
            indexes.iter().any(|index| {
                index.kind == aiondb_catalog::IndexKind::BTree
                    && index
                        .key_columns
                        .first()
                        .is_some_and(|key| key.column_id == filter.column_id)
            })
        });
        if has_indexed_range {
            return Ok(1);
        }
        Ok(2)
    }

    fn try_match_pattern_relation_seeded(
        &self,
        context: &ExecutionContext,
        pattern: &CypherPattern,
        input_bindings: &[BindingRow],
        filter_conjuncts: &[GraphFilterConjunct<'_>],
        runtime_cache: &mut GraphMatchRuntimeCache,
    ) -> DbResult<Option<Vec<BindingRow>>> {
        if pattern.path_function.is_some()
            || pattern.path_variable.is_some()
            || pattern.nodes.len() != 2
            || pattern.relationships.len() != 1
        {
            return Ok(None);
        }
        let rel = &pattern.relationships[0];
        if rel.min_hops.is_some()
            || rel.max_hops.is_some()
            || rel.direction == aiondb_plan::graph::CypherRelDirection::Both
        {
            return Ok(None);
        }
        let Some(table_id) = rel.table_id else {
            return Ok(None);
        };
        if input_bindings.iter().any(|binding| {
            pattern
                .nodes
                .iter()
                .filter_map(|node| node.variable.as_deref())
                .any(|var| binding.get(var).is_some())
        }) {
            return Ok(None);
        }

        let pushed_filters =
            self.collect_relationship_scan_filters(context, table_id, rel, filter_conjuncts)?;
        if pushed_filters.is_empty() {
            return Ok(None);
        }

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
        let ((src_col_idx, tgt_col_idx), _) =
            self.resolve_edge_endpoint_columns_for_rel(context, table_id, rel.rel_type.as_deref())?;
        let edge_rel_type: SharedText = Arc::from(rel.rel_type.as_deref().unwrap_or("").to_owned());
        let mut stream = match self.scan_relationship_candidates_with_filters(
            context,
            table_id,
            &pushed_filters,
            None,
        )? {
            Some(stream) => stream,
            None => return Ok(None),
        };

        let start_node = &pattern.nodes[0];
        let end_node = &pattern.nodes[1];
        let start_node_candidate_ids =
            self.collect_static_node_candidate_id_keys(context, start_node)?;
        if start_node_candidate_ids
            .as_ref()
            .is_some_and(HashSet::is_empty)
        {
            return Ok(Some(Vec::new()));
        }
        let end_node_candidate_ids = self.collect_static_node_candidate_id_keys(context, end_node)?;
        if end_node_candidate_ids
            .as_ref()
            .is_some_and(HashSet::is_empty)
        {
            return Ok(Some(Vec::new()));
        }
        let relation_seed_score =
            self.relation_seed_support_score(context, table_id, &pushed_filters)?;
        let smallest_node_candidate_bound = node_candidate_bound(
            start_node_candidate_ids.as_ref(),
            end_node_candidate_ids.as_ref(),
        );
        if smallest_node_candidate_bound.is_some_and(|size| {
            size <= RELATION_SEEDED_NODE_CANDIDATE_THRESHOLD
                || (size <= RELATION_SEEDED_NODE_CANDIDATE_SOFT_THRESHOLD
                    && relation_seed_score >= 1)
        }) {
            return Ok(None);
        }
        let mut output = Vec::new();
        let edge_marker_node = |id_value: Value| -> BoundValue {
            let marker_row = Arc::new(aiondb_core::Row::new(vec![id_value.clone()]));
            BoundValue::Node {
                table_id: RelationId::new(0),
                row: Arc::clone(&marker_row),
                raw_row: marker_row,
                id_value,
                tuple_id: aiondb_core::TupleId::new(0),
                labels: Arc::new(Vec::new()),
                column_names: Arc::new(Vec::new()),
            }
        };

        while let Some(record) = stream.next()? {
            context.check_deadline()?;
            if !self.compat_rls_allows_existing_row(
                edge_rls_policies.as_deref(),
                &record.row,
                context,
            )? {
                continue;
            }

            let compat_row = Arc::new(self.compat_scan_row(
                &record,
                include_oid_system_column,
                Some(table_id),
            ));
            let raw_row = Arc::new(record.row);

            for base_binding in input_bindings {
                if !self.check_property_filters(
                    context,
                    &rel.properties,
                    edge_col_names.as_ref(),
                    compat_row.as_ref(),
                    base_binding,
                )? {
                    continue;
                }

                let source_id = compat_row
                    .values
                    .get(src_col_idx)
                    .cloned()
                    .unwrap_or(Value::Null);
                let target_id = compat_row
                    .values
                    .get(tgt_col_idx)
                    .cloned()
                    .unwrap_or(Value::Null);
                let (start_node_id, end_node_id) = match rel.direction {
                    aiondb_plan::graph::CypherRelDirection::Outgoing => {
                        (source_id, target_id)
                    }
                    aiondb_plan::graph::CypherRelDirection::Incoming => {
                        (target_id, source_id)
                    }
                    aiondb_plan::graph::CypherRelDirection::Both => unreachable!(),
                };
                let Some(start_node_key) = build_hash_key(&start_node_id).ok() else {
                    continue;
                };
                if start_node_candidate_ids
                    .as_ref()
                    .is_some_and(|allowed| !allowed.contains(&start_node_key))
                {
                    continue;
                }
                let Some(end_node_key) = build_hash_key(&end_node_id).ok() else {
                    continue;
                };
                if end_node_candidate_ids
                    .as_ref()
                    .is_some_and(|allowed| !allowed.contains(&end_node_key))
                {
                    continue;
                }

                let mut seeded = base_binding.clone();
                if let Some(var) = rel.variable.as_deref() {
                    seeded.insert_binding(
                        var.to_owned(),
                        BoundValue::Edge {
                            table_id,
                            row: Arc::clone(&compat_row),
                            raw_row: Arc::clone(&raw_row),
                            tuple_id: record.tuple_id,
                            rel_type: Arc::clone(&edge_rel_type),
                            column_names: Arc::clone(&edge_col_names),
                        },
                    );
                }
                seeded.insert_binding(
                    "__edge_next_node_id__".to_owned(),
                    edge_marker_node(start_node_id),
                );
                let mut first = self.match_node(context, start_node, vec![seeded], runtime_cache)?;
                // Move `end_node_id` into the last binding's marker rather
                // than cloning it for every binding.
                let mut iter = first.iter_mut();
                let last = iter.next_back();
                for binding in iter {
                    binding.insert_binding(
                        "__edge_next_node_id__".to_owned(),
                        edge_marker_node(end_node_id.clone()),
                    );
                }
                if let Some(binding) = last {
                    binding.insert_binding(
                        "__edge_next_node_id__".to_owned(),
                        edge_marker_node(end_node_id),
                    );
                }
                let second = self.match_node(context, end_node, first, runtime_cache)?;
                output.extend(second);
            }
        }

        Ok(Some(output))
    }

    fn match_single_hop_star_branch(
        &self,
        context: &ExecutionContext,
        pattern: &CypherPattern,
        input_binding: BindingRow,
        filter_conjuncts: &[GraphFilterConjunct<'_>],
        keep: Option<&HashSet<String>>,
        runtime_cache: &mut GraphMatchRuntimeCache,
    ) -> DbResult<Vec<BindingRow>> {
        let Some(current_node) = pattern.nodes.first() else {
            return Ok(Vec::new());
        };
        let Some(rel) = pattern.relationships.first() else {
            return Ok(vec![input_binding]);
        };
        let Some(next_node) = pattern.nodes.get(1) else {
            return Ok(vec![input_binding]);
        };
        let excluded_next_node_id_vars = next_node
            .variable
            .as_deref()
            .map(|variable| graph_filter_node_id_inequality_peers(filter_conjuncts, variable))
            .unwrap_or_default();

        let mut bindings = self.match_relationship(
            context,
            current_node,
            rel,
            Some(next_node),
            vec![input_binding],
            &excluded_next_node_id_vars,
            None,
            filter_conjuncts,
            runtime_cache,
        )?;
        bindings = self.apply_ready_graph_filter_conjuncts(context, bindings, filter_conjuncts)?;
        for binding in &mut bindings {
            compact_graph_binding_node_payloads(binding);
        }
        if bindings.is_empty() {
            return Ok(bindings);
        }

        bindings = self.match_node(context, next_node, bindings, runtime_cache)?;
        bindings = self.apply_ready_graph_filter_conjuncts(context, bindings, filter_conjuncts)?;
        if let Some(keep) = keep {
            for binding in &mut bindings {
                retain_graph_binding_variables(binding, keep);
                compact_graph_binding_node_payloads(binding);
            }
        } else {
            for binding in &mut bindings {
                compact_graph_binding_node_payloads(binding);
            }
        }
        Ok(bindings)
    }

    fn can_match_single_hop_star_branch_ids_only(
        &self,
        context: &ExecutionContext,
        pattern: &CypherPattern,
        branch_filter_conjuncts: &[GraphFilterConjunct<'_>],
        target_variable: &str,
    ) -> DbResult<bool> {
        let Some(next_node) = pattern.nodes.get(1) else {
            return Ok(false);
        };
        let Some(rel) = pattern.relationships.first() else {
            return Ok(false);
        };
        if next_node.variable.as_deref() != Some(target_variable)
            || node_has_filter_constraints(next_node)
            || rel.variable.is_some()
            || !rel.properties.is_empty()
            || rel.min_hops.is_some()
            || rel.max_hops.is_some()
        {
            return Ok(false);
        }
        if branch_filter_conjuncts.iter().any(|conjunct| {
            conjunct
                .referenced_vars
                .as_ref()
                .is_some_and(|vars| vars.iter().any(|var| var == target_variable))
        }) {
            return Ok(false);
        }
        let Some(rel_table_id) = rel.table_id else {
            return Ok(false);
        };
        let Some(next_label) = next_node.label.as_deref() else {
            return Ok(true);
        };
        let Some(edge_label) = self.edge_label_for_table_id(context, rel_table_id)? else {
            return Ok(false);
        };
        let expected_target_label = match rel.direction {
            aiondb_plan::graph::CypherRelDirection::Outgoing => edge_label.target_label.as_str(),
            aiondb_plan::graph::CypherRelDirection::Incoming => edge_label.source_label.as_str(),
            aiondb_plan::graph::CypherRelDirection::Both => return Ok(false),
        };
        Ok(next_label.eq_ignore_ascii_case(expected_target_label))
    }

    fn match_single_hop_star_branch_ids_only(
        &self,
        context: &ExecutionContext,
        pattern: &CypherPattern,
        input_binding: BindingRow,
        filter_conjuncts: &[GraphFilterConjunct<'_>],
        target_variable: &str,
        runtime_cache: &mut GraphMatchRuntimeCache,
    ) -> DbResult<Vec<BindingRow>> {
        let Some(current_node) = pattern.nodes.first() else {
            return Ok(Vec::new());
        };
        let Some(rel) = pattern.relationships.first() else {
            return Ok(vec![input_binding]);
        };
        let Some(next_node) = pattern.nodes.get(1) else {
            return Ok(vec![input_binding]);
        };
        let excluded_next_node_id_vars = next_node
            .variable
            .as_deref()
            .map(|variable| graph_filter_node_id_inequality_peers(filter_conjuncts, variable))
            .unwrap_or_default();
        let mut base_bindings = self.apply_ready_graph_filter_conjuncts(
            context,
            vec![input_binding],
            filter_conjuncts,
        )?;
        let Some(base_binding) = base_bindings.pop() else {
            return Ok(Vec::new());
        };
        let Some(rel_table_id) = rel.table_id else {
            return Ok(Vec::new());
        };
        let (_, use_table_adjacency) = self.resolve_edge_endpoint_columns_for_rel(
            context,
            rel_table_id,
            rel.rel_type.as_deref(),
        )?;
        if !use_table_adjacency {
            return Ok(Vec::new());
        }
        let Some(current_id) =
            self.find_current_node_id_for_pattern(&base_binding, Some(current_node))
        else {
            return Ok(Vec::new());
        };
        let next_node_candidate_ids =
            self.collect_static_node_candidate_id_keys(context, next_node)?;
        if next_node_candidate_ids
            .as_ref()
            .is_some_and(HashSet::is_empty)
        {
            return Ok(Vec::new());
        }
        let excluded_next_node_id_keys = excluded_next_node_id_vars
            .iter()
            .filter_map(|variable| match base_binding.get(variable) {
                Some(BoundValue::Node { id_value, .. }) => build_hash_key(id_value).ok(),
                _ => None,
            })
            .collect::<HashSet<_>>();

        let directions: &[bool] = match rel.direction {
            aiondb_plan::graph::CypherRelDirection::Outgoing => &[true],
            aiondb_plan::graph::CypherRelDirection::Incoming => &[false],
            aiondb_plan::graph::CypherRelDirection::Both => return Ok(Vec::new()),
        };
        let mut reduced = Vec::new();
        for &is_outgoing in directions {
            let cache_key = build_hash_key(&current_id)
                .ok()
                .map(|node_key| (rel_table_id, node_key, is_outgoing));
            let neighbor_ids = match cache_key {
                Some(key) => {
                    if let Some(neighbor_ids) =
                        runtime_cache.adjacency_neighbor_cache.get(&key)
                    {
                        Arc::clone(neighbor_ids)
                    } else {
                        let neighbor_ids =
                            Arc::new(self.fast_graph_adjacency_neighbors_cached(
                                context,
                                rel_table_id,
                                &current_id,
                                is_outgoing,
                            )?);
                        // Move `key` into the cache instead of cloning it.
                        runtime_cache
                            .adjacency_neighbor_cache
                            .insert(key, Arc::clone(&neighbor_ids));
                        neighbor_ids
                    }
                }
                None => Arc::new(self.fast_graph_adjacency_neighbors_cached(
                    context,
                    rel_table_id,
                    &current_id,
                    is_outgoing,
                )?),
            };
            // Defer the per-neighbor `Value` clone until after the candidate
            // / excluded checks: rejected neighbors no longer pay an upfront
            // clone, only the ones we actually emit do.
            for neighbor_id in neighbor_ids.iter() {
                let Some(neighbor_key) = build_hash_key(neighbor_id).ok() else {
                    continue;
                };
                if next_node_candidate_ids
                    .as_ref()
                    .is_some_and(|allowed| !allowed.contains(&neighbor_key))
                {
                    continue;
                }
                if excluded_next_node_id_keys.contains(&neighbor_key) {
                    continue;
                }
                let mut binding = base_binding.clone();
                binding.push_fresh_shared_binding(
                    target_variable.to_owned(),
                    Arc::new(compact_node_bound_value(
                        RelationId::new(0),
                        neighbor_id.clone(),
                        aiondb_core::TupleId::new(0),
                        Arc::new(Vec::new()),
                        Arc::new(Vec::new()),
                    )),
                );
                reduced.push(binding);
            }
        }
        Ok(reduced)
    }

    fn try_match_shared_anchor_star<'a>(
        &self,
        context: &ExecutionContext,
        patterns: &[CypherPattern],
        clause_label: &'static str,
        clause_index: usize,
        input_binding: &BindingRow,
        filter_conjuncts: &[GraphFilterConjunct<'a>],
        required_output_variables: Option<&HashSet<String>>,
        binding_reduction: Option<&GraphBindingReduction>,
        runtime_cache: &mut GraphMatchRuntimeCache,
    ) -> DbResult<Option<Vec<BindingRow>>> {
        let Some(anchor) = patterns.first().and_then(|pattern| pattern.nodes.first()) else {
            return Ok(None);
        };
        if patterns.len() < 2
            || !patterns
                .iter()
                .all(|pattern| is_single_hop_star_pattern(pattern, anchor))
        {
            return Ok(None);
        }

        let base_variables = input_binding
            .iter()
            .map(|(name, _)| name.clone())
            .collect::<HashSet<_>>();
        let mut anchor_bindings =
            self.match_node(context, anchor, vec![input_binding.clone()], runtime_cache)?;
        anchor_bindings =
            self.apply_ready_graph_filter_conjuncts(context, anchor_bindings, filter_conjuncts)?;
        if anchor_bindings.is_empty() {
            return Ok(Some(Vec::new()));
        }

        let mut output = Vec::new();
        for anchor_binding in anchor_bindings {
            if patterns.len() == 2 {
                if let Some(GraphBindingReduction::GlobalDistinctExpr(expr)) = binding_reduction {
                    if let Some(target_variable) = direct_graph_distinct_binding_variable(expr) {
                        let target_branch_idx = patterns.iter().position(|pattern| {
                            pattern
                                .nodes
                                .get(1)
                                .and_then(|node| node.variable.as_deref())
                                == Some(target_variable)
                        });
                        if let Some(target_branch_idx) = target_branch_idx {
                            let probe_branch_idx = 1usize.saturating_sub(target_branch_idx);
                            if probe_branch_idx < patterns.len()
                                && probe_branch_idx != target_branch_idx
                            {
                                let target_branch_var = patterns[target_branch_idx]
                                    .nodes
                                    .get(1)
                                    .and_then(|node| node.variable.as_deref());
                                let probe_branch_var = patterns[probe_branch_idx]
                                    .nodes
                                    .get(1)
                                    .and_then(|node| node.variable.as_deref());
                                let target_pattern = &patterns[target_branch_idx];
                                let probe_pattern = &patterns[probe_branch_idx];
                                let can_use_id_inequality_semijoin = target_branch_var
                                    .zip(probe_branch_var)
                                    .is_some_and(|(target_var, probe_var)| {
                                        filter_conjuncts.iter().all(|conjunct| {
                                            let Some(vars) = conjunct.referenced_vars.as_ref() else {
                                                return false;
                                            };
                                            if vars.iter().all(|var| base_variables.contains(var)) {
                                                return true;
                                            }
                                            let target_local = vars.iter().all(|var| {
                                                base_variables.contains(var) || var == target_var
                                            });
                                            if target_local {
                                                return true;
                                            }
                                            let probe_local = vars.iter().all(|var| {
                                                base_variables.contains(var) || var == probe_var
                                            });
                                            if probe_local {
                                                return true;
                                            }
                                            matches!(
                                                node_id_inequality_pair(conjunct.expr),
                                                Some((left, right))
                                                    if (left == target_var && right == probe_var)
                                                        || (left == probe_var && right == target_var)
                                            )
                                        })
                                    });
                                if can_use_id_inequality_semijoin {
                                    let probe_var = probe_branch_var.expect("checked above");
                                    let target_var = target_branch_var.expect("checked above");
                                    let target_pattern_started_at = Instant::now();
                                    let target_branch_filter_conjuncts =
                                        filter_conjuncts_for_star_branch(
                                            target_pattern,
                                            &base_variables,
                                            filter_conjuncts,
                                        );
                                    let target_bindings = if self
                                        .can_match_single_hop_star_branch_ids_only(
                                            context,
                                            target_pattern,
                                            &target_branch_filter_conjuncts,
                                            target_var,
                                        )? {
                                        self.match_single_hop_star_branch_ids_only(
                                            context,
                                            target_pattern,
                                            anchor_binding.clone(),
                                            &target_branch_filter_conjuncts,
                                            target_var,
                                            runtime_cache,
                                        )?
                                    } else {
                                        let mut target_keep = HashSet::new();
                                        target_keep.insert(target_var.to_owned());
                                        self.match_single_hop_star_branch(
                                            context,
                                            target_pattern,
                                            anchor_binding.clone(),
                                            &target_branch_filter_conjuncts,
                                            Some(&target_keep),
                                            runtime_cache,
                                        )?
                                    };
                                    let target_bindings =
                                        dedup_bindings_by_node_id(target_bindings, target_var);
                                    context.record_graph_profile_actual_rows(
                                        &graph_access_profile_key(
                                            clause_label,
                                            clause_index,
                                            target_branch_idx,
                                        ),
                                        u64::try_from(target_bindings.len()).unwrap_or(u64::MAX),
                                    )?;
                                    context.record_graph_profile_elapsed_nanos(
                                        &graph_access_pattern_profile_time_key(
                                            clause_label,
                                            clause_index,
                                            target_branch_idx,
                                        ),
                                        u64::try_from(
                                            target_pattern_started_at.elapsed().as_nanos(),
                                        )
                                        .unwrap_or(u64::MAX),
                                    )?;

                                    let probe_pattern_started_at = Instant::now();
                                    let probe_branch_filter_conjuncts =
                                        filter_conjuncts_for_star_branch(
                                            probe_pattern,
                                            &base_variables,
                                            filter_conjuncts,
                                        );
                                    let mut probe_keep = HashSet::new();
                                    probe_keep.insert(probe_var.to_owned());
                                    let probe_bindings = self.match_single_hop_star_branch(
                                        context,
                                        probe_pattern,
                                        anchor_binding.clone(),
                                        &probe_branch_filter_conjuncts,
                                        Some(&probe_keep),
                                        runtime_cache,
                                    )?;
                                    let mut probe_ids = HashSet::new();
                                    for probe_binding in &probe_bindings {
                                        if let Some(key) =
                                            binding_node_id_key(probe_binding, probe_var)
                                        {
                                            probe_ids.insert(key);
                                        }
                                    }
                                    if probe_ids.is_empty() {
                                        context.record_graph_profile_actual_rows(
                                            &graph_access_profile_key(
                                                clause_label,
                                                clause_index,
                                                probe_branch_idx,
                                            ),
                                            0,
                                        )?;
                                        context.record_graph_profile_elapsed_nanos(
                                            &graph_access_pattern_profile_time_key(
                                                clause_label,
                                                clause_index,
                                                probe_branch_idx,
                                            ),
                                            u64::try_from(
                                                probe_pattern_started_at.elapsed().as_nanos(),
                                            )
                                            .unwrap_or(u64::MAX),
                                        )?;
                                        continue;
                                    }
                                    let output_before = output.len();
                                    // Consume `target_bindings` so kept rows
                                    // move into `output` instead of cloning.
                                    for target_binding in target_bindings {
                                        let Some(target_id) =
                                            binding_node_id_key(&target_binding, target_var)
                                        else {
                                            continue;
                                        };
                                        if probe_ids.len() > 1 || !probe_ids.contains(&target_id) {
                                            output.push(target_binding);
                                        }
                                    }
                                    context.record_graph_profile_actual_rows(
                                        &graph_access_profile_key(
                                            clause_label,
                                            clause_index,
                                            probe_branch_idx,
                                        ),
                                        u64::try_from(output.len().saturating_sub(output_before))
                                            .unwrap_or(u64::MAX),
                                    )?;
                                    context.record_graph_profile_elapsed_nanos(
                                        &graph_access_pattern_profile_time_key(
                                            clause_label,
                                            clause_index,
                                            probe_branch_idx,
                                        ),
                                        u64::try_from(
                                            probe_pattern_started_at.elapsed().as_nanos(),
                                        )
                                        .unwrap_or(u64::MAX),
                                    )?;
                                    continue;
                                }
                                let mut branch_bindings = Vec::with_capacity(2);
                                for (pattern_idx, pattern) in patterns.iter().enumerate() {
                                    let pattern_started_at = Instant::now();
                                    let branch_filter_conjuncts = filter_conjuncts_for_star_branch(
                                        pattern,
                                        &base_variables,
                                        filter_conjuncts,
                                    );
                                    let branch_var = pattern
                                        .nodes
                                        .get(1)
                                        .and_then(|node| node.variable.as_ref())
                                        .cloned();
                                    let branch_required_output_variables =
                                        branch_var.as_ref().map(|variable| {
                                            let mut keep = HashSet::new();
                                            keep.insert(variable.clone());
                                            keep
                                        });
                                    let matched_branch =
                                        if let Some(branch_var) = branch_var.as_deref() {
                                            if self.can_match_single_hop_star_branch_ids_only(
                                                context,
                                                pattern,
                                                &branch_filter_conjuncts,
                                                branch_var,
                                            )? {
                                                self.match_single_hop_star_branch_ids_only(
                                                    context,
                                                    pattern,
                                                    anchor_binding.clone(),
                                                    &branch_filter_conjuncts,
                                                    branch_var,
                                                    runtime_cache,
                                                )?
                                            } else {
                                                self.match_single_hop_star_branch(
                                                    context,
                                                    pattern,
                                                    anchor_binding.clone(),
                                                    &branch_filter_conjuncts,
                                                    branch_required_output_variables.as_ref(),
                                                    runtime_cache,
                                                )?
                                            }
                                        } else {
                                            self.match_single_hop_star_branch(
                                                context,
                                                pattern,
                                                anchor_binding.clone(),
                                                &branch_filter_conjuncts,
                                                branch_required_output_variables.as_ref(),
                                                runtime_cache,
                                            )?
                                        };
                                    context.record_graph_profile_actual_rows(
                                        &graph_access_profile_key(
                                            clause_label,
                                            clause_index,
                                            pattern_idx,
                                        ),
                                        u64::try_from(matched_branch.len()).unwrap_or(u64::MAX),
                                    )?;
                                    context.record_graph_profile_elapsed_nanos(
                                        &graph_access_pattern_profile_time_key(
                                            clause_label,
                                            clause_index,
                                            pattern_idx,
                                        ),
                                        u64::try_from(pattern_started_at.elapsed().as_nanos())
                                            .unwrap_or(u64::MAX),
                                    )?;
                                    branch_bindings.push(matched_branch);
                                }
                                let target_bindings = &branch_bindings[target_branch_idx];
                                let probe_bindings = &branch_bindings[probe_branch_idx];
                                let output_before = output.len();
                                for target_binding in target_bindings {
                                    let mut matched = None;
                                    for probe_binding in probe_bindings {
                                        let merged =
                                            merge_graph_bindings(target_binding, probe_binding);
                                        let ready = self.apply_ready_graph_filter_conjuncts(
                                            context,
                                            vec![merged],
                                            filter_conjuncts,
                                        )?;
                                        if let Some(binding) = ready.into_iter().next() {
                                            matched = Some(binding);
                                            break;
                                        }
                                    }
                                    if let Some(binding) = matched {
                                        output.push(binding);
                                    }
                                }
                                context.record_graph_profile_actual_rows(
                                    &graph_access_profile_key(
                                        clause_label,
                                        clause_index,
                                        probe_branch_idx,
                                    ),
                                    u64::try_from(output.len().saturating_sub(output_before))
                                        .unwrap_or(u64::MAX),
                                )?;
                                continue;
                            }
                        }
                    }
                }
            }

            let star_seed_keep = keep_variables_for_star_seed(
                &base_variables,
                anchor,
                filter_conjuncts,
                required_output_variables,
            );
            let mut star_seed = anchor_binding.clone();
            retain_graph_binding_variables(&mut star_seed, &star_seed_keep);
            compact_graph_binding_node_payloads(&mut star_seed);
            let mut combined_bindings = vec![star_seed];
            for (pattern_idx, pattern) in patterns.iter().enumerate() {
                let pattern_started_at = Instant::now();
                let mut branch_allowed_vars = base_variables.clone();
                collect_pattern_binding_variables(pattern, &mut branch_allowed_vars);
                let branch_filter_conjuncts = filter_conjuncts
                    .iter()
                    .filter(|conjunct| {
                        conjunct.referenced_vars.as_ref().is_some_and(|vars| {
                            vars.iter().all(|var| branch_allowed_vars.contains(var))
                        })
                    })
                    .cloned()
                    .collect::<Vec<_>>();
                let branch_keep = keep_variables_for_star_branch(
                    pattern,
                    filter_conjuncts,
                    required_output_variables,
                );
                let branch_var = pattern
                    .nodes
                    .get(1)
                    .and_then(|node| node.variable.as_deref());
                let mut branch_bindings = if branch_var
                    .zip(Some(&branch_keep))
                    .is_some_and(|(branch_var, keep)| keep.len() == 1 && keep.contains(branch_var))
                    && branch_var.is_some_and(|branch_var| {
                        self.can_match_single_hop_star_branch_ids_only(
                            context,
                            pattern,
                            &branch_filter_conjuncts,
                            branch_var,
                        )
                        .unwrap_or(false)
                    }) {
                    self.match_single_hop_star_branch_ids_only(
                        context,
                        pattern,
                        anchor_binding.clone(),
                        &branch_filter_conjuncts,
                        branch_var.expect("checked above"),
                        runtime_cache,
                    )?
                } else {
                    self.match_pattern(
                        context,
                        pattern,
                        vec![anchor_binding.clone()],
                        &branch_filter_conjuncts,
                        Some(&branch_keep),
                        runtime_cache,
                    )?
                };
                for binding in &mut branch_bindings {
                    retain_graph_binding_variables(binding, &branch_keep);
                    compact_graph_binding_node_payloads(binding);
                }
                if branch_bindings.is_empty() {
                    combined_bindings.clear();
                    break;
                }
                let next_combined = if filter_conjuncts.is_empty() {
                    cartesian_merge_graph_bindings(combined_bindings, &branch_bindings)
                } else {
                    let mut next_combined = Vec::new();
                    for combined in combined_bindings.drain(..) {
                        for branch in &branch_bindings {
                            next_combined.push(merge_graph_bindings(&combined, branch));
                        }
                    }
                    self.apply_ready_graph_filter_conjuncts(
                        context,
                        next_combined,
                        filter_conjuncts,
                    )?
                };
                combined_bindings = next_combined;
                context.record_graph_profile_actual_rows(
                    &graph_access_profile_key(clause_label, clause_index, pattern_idx),
                    u64::try_from(combined_bindings.len()).unwrap_or(u64::MAX),
                )?;
                context.record_graph_profile_elapsed_nanos(
                    &graph_access_pattern_profile_time_key(
                        clause_label,
                        clause_index,
                        pattern_idx,
                    ),
                    u64::try_from(pattern_started_at.elapsed().as_nanos()).unwrap_or(u64::MAX),
                )?;
                if combined_bindings.is_empty() {
                    break;
                }
            }
            output.extend(combined_bindings);
        }

        Ok(Some(output))
    }

    fn graph_binding_reduction_key(
        &self,
        context: &ExecutionContext,
        reduction: &GraphBindingReduction,
        binding: &BindingRow,
    ) -> DbResult<Option<(ValueHashKey, u64)>> {
        match reduction {
            GraphBindingReduction::GlobalDistinctExpr(expr) => {
                if let Some(key) = direct_graph_distinct_binding_key(expr, binding) {
                    return Ok(Some((key, 80)));
                }
                let value = self.evaluate_cypher_expr_with_binding(expr, binding, context)?;
                if value.is_null() {
                    return Ok(None);
                }
                let estimated_bytes =
                    crate::executor::estimate_value_bytes(&value).saturating_add(64);
                Ok(Some((build_hash_key(&value)?, estimated_bytes)))
            }
            GraphBindingReduction::TopN { .. } => Ok(None),
        }
    }

    fn graph_binding_reduction_sort_keys(
        &self,
        context: &ExecutionContext,
        reduction: &GraphBindingReduction,
        binding: &BindingRow,
    ) -> DbResult<Option<Vec<Value>>> {
        match reduction {
            GraphBindingReduction::TopN { order_by, .. } => {
                let mut keys = Vec::with_capacity(order_by.len());
                for sort in order_by {
                    keys.push(
                        self.evaluate_cypher_expr_with_binding(&sort.expr, binding, context)?,
                    );
                }
                Ok(Some(keys))
            }
            GraphBindingReduction::GlobalDistinctExpr(_) => Ok(None),
        }
    }

    fn prune_applied_graph_filter_conjuncts<'a>(
        bindings: &[BindingRow],
        pending: &mut Vec<GraphFilterConjunct<'a>>,
    ) {
        let Some(first_binding) = bindings.first() else {
            pending.clear();
            return;
        };
        let bound_names = first_binding
            .iter()
            .map(|(name, _)| name.clone())
            .collect::<HashSet<_>>();
        pending.retain(|conjunct| !conjunct.is_ready_with_names(&bound_names));
    }

    pub(super) fn match_pattern(
        &self,
        context: &ExecutionContext,
        pattern: &CypherPattern,
        bindings: Vec<BindingRow>,
        filter_conjuncts: &[GraphFilterConjunct<'_>],
        required_output_variables: Option<&HashSet<String>>,
        runtime_cache: &mut GraphMatchRuntimeCache,
    ) -> DbResult<Vec<BindingRow>> {
        self.match_pattern_with_strategy(
            context,
            pattern,
            bindings,
            filter_conjuncts,
            required_output_variables,
            runtime_cache,
        )
        .map(|(bindings, _, _, _, _)| bindings)
    }

    pub(super) fn match_pattern_with_strategy(
        &self,
        context: &ExecutionContext,
        pattern: &CypherPattern,
        mut bindings: Vec<BindingRow>,
        filter_conjuncts: &[GraphFilterConjunct<'_>],
        required_output_variables: Option<&HashSet<String>>,
        runtime_cache: &mut GraphMatchRuntimeCache,
    ) -> DbResult<(
        Vec<BindingRow>,
        &'static str,
        &'static str,
        Option<&'static str>,
        Option<usize>,
    )> {
        let mut pending_filter_conjuncts = filter_conjuncts.to_vec();
        for binding in &mut bindings {
            binding.remove("__edge_next_node_id__");
        }

        if let Some(ref func) = pattern.path_function {
            return self
                .match_shortest_path(context, pattern, *func, bindings)
                .map(|bindings| (bindings, "path_function", "path_function_dispatch", None, None));
        }

        if let Some(bindings) = self.try_match_pattern_relation_seeded(
            context,
            pattern,
            &bindings,
            &pending_filter_conjuncts,
            runtime_cache,
        )? {
            return self
                .apply_ready_graph_filter_conjuncts(context, bindings, &pending_filter_conjuncts)
                .map(|bindings| {
                    (
                        bindings,
                        "relation_seeded",
                        "relationship_filter_seed",
                        None,
                        None,
                    )
                });
        }

        let cbo_pivot = self.cbo_seed_index_for_pattern(context, pattern)?;
        let heuristic_pivot = pick_match_pivot_index(pattern);
        if let Some(pivot) = cbo_pivot.or(heuristic_pivot) {
            let pivot_driver = if cbo_pivot.is_some() {
                Some("cbo")
            } else if heuristic_pivot.is_some() {
                Some("heuristic")
            } else {
                None
            };
            return self
                .match_pattern_pivoted(
                    context,
                    pattern,
                    bindings,
                    filter_conjuncts,
                    pivot,
                    required_output_variables,
                    runtime_cache,
                )
                .map(|bindings| {
                    (
                        bindings,
                        "pivoted_node_seed",
                        "pivot_seed",
                        pivot_driver,
                        Some(pivot),
                    )
                });
        }

        for (i, node) in pattern.nodes.iter().enumerate() {
            bindings = self.match_node(context, node, bindings, runtime_cache)?;
            bindings = self.apply_ready_graph_filter_conjuncts(
                context,
                bindings,
                &pending_filter_conjuncts,
            )?;
            Self::prune_applied_graph_filter_conjuncts(&bindings, &mut pending_filter_conjuncts);
            if i < pattern.relationships.len() {
                for binding in &mut bindings {
                    compact_graph_binding_node_payloads(binding);
                }
            }
            if bindings.is_empty() {
                return Ok((bindings, "left_to_right_node_seed", "left_to_right_walk", None, None));
            }
            if i < pattern.relationships.len() {
                let rel = &pattern.relationships[i];
                let next_node = pattern.nodes.get(i + 1);
                let excluded_next_node_id_vars = next_node
                    .and_then(|node| node.variable.as_deref())
                    .map(|variable| {
                        graph_filter_node_id_inequality_peers(filter_conjuncts, variable)
                    })
                    .unwrap_or_default();
                bindings = self.match_relationship(
                    context,
                    node,
                    rel,
                    next_node,
                    bindings,
                    &excluded_next_node_id_vars,
                    pattern.path_variable.as_deref(),
                    &pending_filter_conjuncts,
                    runtime_cache,
                )?;
                bindings = self.apply_ready_graph_filter_conjuncts(
                    context,
                    bindings,
                    &pending_filter_conjuncts,
                )?;
                Self::prune_applied_graph_filter_conjuncts(
                    &bindings,
                    &mut pending_filter_conjuncts,
                );
                for binding in &mut bindings {
                    compact_graph_binding_node_payloads(binding);
                }
                if bindings.is_empty() {
                    return Ok((bindings, "left_to_right_node_seed", "left_to_right_walk", None, None));
                }
            }
        }
        Ok((bindings, "left_to_right_node_seed", "left_to_right_walk", None, None))
    }

    fn match_pattern_pivoted(
        &self,
        context: &ExecutionContext,
        pattern: &CypherPattern,
        bindings: Vec<BindingRow>,
        filter_conjuncts: &[GraphFilterConjunct<'_>],
        pivot: usize,
        _required_output_variables: Option<&HashSet<String>>,
        runtime_cache: &mut GraphMatchRuntimeCache,
    ) -> DbResult<Vec<BindingRow>> {
        let mut bindings =
            self.match_node(context, &pattern.nodes[pivot], bindings, runtime_cache)?;
        bindings = self.apply_ready_graph_filter_conjuncts(context, bindings, filter_conjuncts)?;
        for binding in &mut bindings {
            compact_graph_binding_node_payloads(binding);
        }
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
            let excluded_next_node_id_vars = next_node
                .variable
                .as_deref()
                .map(|variable| graph_filter_node_id_inequality_peers(filter_conjuncts, variable))
                .unwrap_or_default();
            bindings = self.match_relationship(
                context,
                current_node,
                &flipped_rel,
                Some(next_node),
                bindings,
                &excluded_next_node_id_vars,
                None,
                filter_conjuncts,
                runtime_cache,
            )?;
            bindings =
                self.apply_ready_graph_filter_conjuncts(context, bindings, filter_conjuncts)?;
            for binding in &mut bindings {
                compact_graph_binding_node_payloads(binding);
            }
            if bindings.is_empty() {
                return Ok(bindings);
            }
            bindings = self.match_node(context, next_node, bindings, runtime_cache)?;
            bindings =
                self.apply_ready_graph_filter_conjuncts(context, bindings, filter_conjuncts)?;
            for binding in &mut bindings {
                compact_graph_binding_node_payloads(binding);
            }
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
            let excluded_next_node_id_vars = next_node
                .variable
                .as_deref()
                .map(|variable| graph_filter_node_id_inequality_peers(filter_conjuncts, variable))
                .unwrap_or_default();
            bindings = self.match_relationship(
                context,
                current_node,
                original_rel,
                Some(next_node),
                bindings,
                &excluded_next_node_id_vars,
                None,
                filter_conjuncts,
                runtime_cache,
            )?;
            bindings =
                self.apply_ready_graph_filter_conjuncts(context, bindings, filter_conjuncts)?;
            for binding in &mut bindings {
                compact_graph_binding_node_payloads(binding);
            }
            if bindings.is_empty() {
                return Ok(bindings);
            }
            bindings = self.match_node(context, next_node, bindings, runtime_cache)?;
            bindings =
                self.apply_ready_graph_filter_conjuncts(context, bindings, filter_conjuncts)?;
            for binding in &mut bindings {
                compact_graph_binding_node_payloads(binding);
            }
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
        clause_label: &'static str,
        clause_index: usize,
        input_bindings: Vec<BindingRow>,
        required_output_variables: Option<&HashSet<String>>,
        binding_reduction: Option<&GraphBindingReduction>,
    ) -> DbResult<Vec<BindingRow>> {
        let clause_started_at = Instant::now();
        context.record_graph_profile_actual_rows(
            &graph_access_clause_profile_input_key(clause_label, clause_index),
            u64::try_from(input_bindings.len()).unwrap_or(u64::MAX),
        )?;
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
        let mut distinct_seen = match binding_reduction {
            Some(GraphBindingReduction::GlobalDistinctExpr(_)) => Some(HashSet::new()),
            Some(GraphBindingReduction::TopN { .. }) => None,
            None => None,
        };
        let mut runtime_cache = GraphMatchRuntimeCache::default();
        let mut top_bindings = match binding_reduction {
            Some(GraphBindingReduction::TopN { .. }) => Some(Vec::new()),
            Some(GraphBindingReduction::GlobalDistinctExpr(_)) => None,
            None => None,
        };
        let mut observed_clause_runtime_strategy = "pattern_by_pattern";

        for input_binding in &input_bindings {
            context.check_deadline()?;
            let base_variables = input_binding
                .iter()
                .map(|(name, _)| name.clone())
                .collect::<HashSet<_>>();
            let mut pending_filter_conjuncts = filter_conjuncts.clone();
            let (mut current_bindings, used_star_path) = if clause.optional {
                (vec![input_binding.clone()], false)
            } else if let Some(bindings) = self.try_match_shared_anchor_star(
                context,
                patterns,
                clause_label,
                clause_index,
                input_binding,
                &pending_filter_conjuncts,
                required_output_variables,
                binding_reduction,
                &mut runtime_cache,
            )? {
                observed_clause_runtime_strategy = "shared_anchor_star";
                (bindings, true)
            } else {
                (vec![input_binding.clone()], false)
            };

            if !used_star_path {
                for (pattern_idx, pattern) in patterns.iter().enumerate() {
                    let pattern_started_at = Instant::now();
                    validate_named_path_pattern(pattern)?;
                    let materialized_pattern;
                    let pattern = if pattern.path_variable.is_some() {
                        materialized_pattern = materialize_named_path_pattern(pattern);
                        &materialized_pattern
                    } else {
                        pattern
                    };
                    let (
                        matched_bindings,
                        pattern_runtime_strategy,
                        pattern_runtime_reason,
                        pattern_pivot_driver,
                        pattern_pivot_index,
                    ) = self.match_pattern_with_strategy(
                        context,
                        pattern,
                        current_bindings,
                        &pending_filter_conjuncts,
                        required_output_variables,
                        &mut runtime_cache,
                    )?;
                    current_bindings = matched_bindings;
                    current_bindings = bind_named_path_variable(pattern, current_bindings);
                    context.record_graph_profile_actual_rows(
                        &graph_access_profile_key(clause_label, clause_index, pattern_idx),
                        u64::try_from(current_bindings.len()).unwrap_or(u64::MAX),
                    )?;
                    context.record_graph_profile_runtime_text(
                        &graph_access_pattern_runtime_strategy_key(
                            clause_label,
                            clause_index,
                            pattern_idx,
                        ),
                        pattern_runtime_strategy,
                    )?;
                    context.record_graph_profile_runtime_text(
                        &graph_access_pattern_runtime_reason_key(
                            clause_label,
                            clause_index,
                            pattern_idx,
                        ),
                        pattern_runtime_reason,
                    )?;
                    if let Some(pattern_pivot_driver) = pattern_pivot_driver {
                        context.record_graph_profile_runtime_text(
                            &graph_access_pattern_pivot_driver_key(
                                clause_label,
                                clause_index,
                                pattern_idx,
                            ),
                            pattern_pivot_driver,
                        )?;
                        if let Some(pattern_pivot_index) = pattern_pivot_index {
                            context.record_graph_profile_runtime_text(
                                &graph_access_pattern_pivot_reason_key(
                                    clause_label,
                                    clause_index,
                                    pattern_idx,
                                ),
                                &format!(
                                    "pivot_to_node_{}:{}",
                                    pattern_pivot_index,
                                    runtime_pivot_node_mode(&pattern.nodes[pattern_pivot_index])
                                ),
                            )?;
                            context.record_graph_profile_runtime_text(
                                &graph_access_pattern_pivot_decision_key(
                                    clause_label,
                                    clause_index,
                                    pattern_idx,
                                ),
                                &format!("selected_node_{}", pattern_pivot_index),
                            )?;
                        }
                    }
                    context.record_graph_profile_elapsed_nanos(
                        &graph_access_pattern_profile_time_key(
                            clause_label,
                            clause_index,
                            pattern_idx,
                        ),
                        u64::try_from(pattern_started_at.elapsed().as_nanos()).unwrap_or(u64::MAX),
                    )?;
                    Self::prune_applied_graph_filter_conjuncts(
                        &current_bindings,
                        &mut pending_filter_conjuncts,
                    );
                    if pattern_idx + 1 < patterns.len() && pattern.path_variable.is_none() {
                        let keep = keep_variables_between_patterns(
                            &base_variables,
                            &patterns[(pattern_idx + 1)..],
                            &pending_filter_conjuncts,
                            required_output_variables,
                        );
                        for binding in &mut current_bindings {
                            retain_graph_binding_variables(binding, &keep);
                            compact_graph_binding_node_payloads(binding);
                        }
                    }
                    if current_bindings.is_empty() && !clause.optional {
                        break;
                    }
                }
            }

            if let Some(filter) = clause
                .filter
                .as_ref()
                .filter(|_| !pending_filter_conjuncts.is_empty())
            {
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

            if let Some(required_output_variables) = required_output_variables {
                let mut keep = base_variables.clone();
                keep.extend(required_output_variables.iter().cloned());
                for binding in &mut current_bindings {
                    retain_graph_binding_variables(binding, &keep);
                }
            }

            if let Some(reduction) = binding_reduction {
                match reduction {
                    GraphBindingReduction::GlobalDistinctExpr(_) => {
                        let seen = distinct_seen
                            .as_mut()
                            .expect("distinct reduction state initialized");
                        let mut reduced = Vec::with_capacity(current_bindings.len());
                        for binding in current_bindings {
                            context.check_deadline()?;
                            let Some((key, estimated_bytes)) =
                                self.graph_binding_reduction_key(context, reduction, &binding)?
                            else {
                                continue;
                            };
                            if seen.insert(key) {
                                context.track_memory(estimated_bytes)?;
                                reduced.push(binding);
                            }
                        }
                        current_bindings = reduced;
                    }
                    GraphBindingReduction::TopN { order_by, limit } => {
                        let top_rows = top_bindings
                            .as_mut()
                            .expect("topn reduction state initialized");
                        if *limit == 0 {
                            current_bindings.clear();
                            continue;
                        }
                        for binding in current_bindings.drain(..) {
                            context.check_deadline()?;
                            let Some(keys) = self
                                .graph_binding_reduction_sort_keys(context, reduction, &binding)?
                            else {
                                continue;
                            };
                            if top_rows.len() < *limit {
                                top_rows.push((keys, binding));
                                continue;
                            }
                            let mut worst_idx = 0usize;
                            for idx in 1..top_rows.len() {
                                if crate::executor::graph_plans::compare_cypher_sort_keys(
                                    &top_rows[worst_idx].0,
                                    &top_rows[idx].0,
                                    order_by,
                                )? == std::cmp::Ordering::Less
                                {
                                    worst_idx = idx;
                                }
                            }
                            if crate::executor::graph_plans::compare_cypher_sort_keys(
                                &keys,
                                &top_rows[worst_idx].0,
                                order_by,
                            )? == std::cmp::Ordering::Less
                            {
                                top_rows[worst_idx] = (keys, binding);
                            }
                        }
                        current_bindings.clear();
                    }
                }
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

        if let Some(GraphBindingReduction::TopN { order_by, .. }) = binding_reduction {
            let mut top_rows = top_bindings.take().unwrap_or_default();
            top_rows.sort_by(|(a_keys, _), (b_keys, _)| {
                crate::executor::graph_plans::compare_cypher_sort_keys(a_keys, b_keys, order_by)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            let output_rows = top_rows.len();
            context.record_graph_profile_actual_rows(
                &graph_access_clause_profile_output_key(clause_label, clause_index),
                u64::try_from(output_rows).unwrap_or(u64::MAX),
            )?;
            context.record_graph_profile_elapsed_nanos(
                &graph_access_clause_profile_time_key(clause_label, clause_index),
                u64::try_from(clause_started_at.elapsed().as_nanos()).unwrap_or(u64::MAX),
            )?;
            return Ok(top_rows.into_iter().map(|(_, binding)| binding).collect());
        }

        context.record_graph_profile_actual_rows(
            &graph_access_clause_profile_output_key(clause_label, clause_index),
            u64::try_from(result_bindings.len()).unwrap_or(u64::MAX),
        )?;
        context.record_graph_profile_runtime_text(
            &graph_access_clause_runtime_strategy_key(clause_label, clause_index),
            observed_clause_runtime_strategy,
        )?;
        context.record_graph_profile_runtime_text(
            &graph_access_clause_runtime_reason_key(clause_label, clause_index),
            observed_clause_runtime_reason(clause, observed_clause_runtime_strategy),
        )?;
        if let Some(blocker) =
            observed_clause_runtime_blocker(clause, observed_clause_runtime_strategy)
        {
            context.record_graph_profile_runtime_text(
                &graph_access_clause_runtime_blocker_key(clause_label, clause_index),
                blocker,
            )?;
        }
        context.record_graph_profile_elapsed_nanos(
            &graph_access_clause_profile_time_key(clause_label, clause_index),
            u64::try_from(clause_started_at.elapsed().as_nanos()).unwrap_or(u64::MAX),
        )?;
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

fn direct_graph_distinct_binding_key(
    expr: &TypedExpr,
    binding: &BindingRow,
) -> Option<ValueHashKey> {
    match &expr.kind {
        TypedExprKind::ColumnRef { name, .. } => {
            let (variable, property) = name.split_once('.')?;
            if !property.eq_ignore_ascii_case("id") {
                return None;
            }
            match binding.get(variable) {
                Some(BoundValue::Node { id_value, .. }) if !id_value.is_null() => {
                    build_hash_key(id_value).ok()
                }
                _ => None,
            }
        }
        TypedExprKind::ScalarFunction {
            func: ScalarFunction::Generic(function_name),
            args,
        } if function_name.eq_ignore_ascii_case("id") && args.len() == 1 => {
            let TypedExprKind::ColumnRef { name, .. } = &args[0].kind else {
                return None;
            };
            match binding.get(name) {
                Some(BoundValue::Node { id_value, .. }) if !id_value.is_null() => {
                    build_hash_key(id_value).ok()
                }
                _ => None,
            }
        }
        _ => None,
    }
}
