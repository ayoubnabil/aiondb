//! Cypher fast-path: low-level adjacency/count/cache helpers (`impl Executor`).
//!
//! Split out of `graph_plans/graph_fast_paths.rs`. Continuation of
//! `impl Executor`; shared types/helpers reached via `use super::*`.
#![allow(clippy::too_many_lines, clippy::type_complexity)]

use super::*;

impl Executor {
    pub(in crate::executor) fn graph_query_binding_reduction(
        &self,
        context: &ExecutionContext,
        returns: &[ProjectionExpr],
        distinct: bool,
        order_by: &[SortExpr],
        skip: Option<&TypedExpr>,
        limit: Option<&TypedExpr>,
    ) -> DbResult<Option<GraphBindingReduction>> {
        if let Some(reduction) = cypher_query_binding_reduction(returns, distinct, order_by) {
            return Ok(Some(reduction));
        }
        if distinct || order_by.is_empty() || skip.is_some() {
            return Ok(None);
        }
        if returns
            .iter()
            .any(|item| expr_contains_aggregate(&item.expr))
        {
            return Ok(None);
        }
        let Some(limit_expr) = limit else {
            return Ok(None);
        };
        let limit_value = self.evaluate_expr(limit_expr, context)?;
        let limit = match limit_value {
            Value::BigInt(n) if n >= 0 => nonneg_i64_to_usize(n),
            Value::Int(n) if n >= 0 => nonneg_i64_to_usize(i64::from(n)),
            _ => return Ok(None),
        };
        Ok(Some(GraphBindingReduction::TopN {
            order_by: order_by.to_vec(),
            limit,
        }))
    }

    pub(in crate::executor) fn fast_graph_adjacency_neighbors_cached(
        &self,
        context: &ExecutionContext,
        edge_table_id: RelationId,
        node_id: &Value,
        outgoing: bool,
    ) -> DbResult<Vec<Value>> {
        let generation = self.storage_dml.cache_generation();
        let cache_key = generation
            .and_then(|_| build_hash_key(node_id).ok())
            .map(|node_key| GraphAdjacencyNeighborsCacheKey {
                edge_table_id,
                node_key,
                outgoing,
            });

        if let (Some(cache_key), Some(generation)) = (&cache_key, generation) {
            let cached = self
                .graph_adjacency_neighbors_cache
                .read()
                .map_err(|error| {
                    DbError::internal(format!("graph adjacency cache poisoned: {error}"))
                })?
                .get(cache_key)
                .cloned();
            if let Some((cached_generation, values)) = cached {
                if cached_generation == generation {
                    return Ok(values);
                }
            }
        }

        let mut cursor = self.storage_dml.adjacency_neighbor_cursor(
            context.txn_id,
            &context.snapshot,
            edge_table_id,
            node_id,
            outgoing,
        )?;
        let mut values = Vec::with_capacity(graph_prealloc_capacity(cursor.remaining_hint()));
        while let Some(value) = cursor.next_neighbor() {
            context.check_deadline()?;
            ensure_graph_workset_capacity(context, values.len(), "adjacency neighbor cache")?;
            context.track_memory(estimate_value_bytes(&value).saturating_add(32))?;
            values.push(value);
        }

        if let (Some(cache_key), Some(generation)) = (cache_key, generation) {
            let mut cache = self
                .graph_adjacency_neighbors_cache
                .write()
                .map_err(|error| {
                    DbError::internal(format!("graph adjacency cache poisoned: {error}"))
                })?;
            if cache.len() >= 4096 {
                cache.clear();
            }
            cache.insert(cache_key, (generation, values.clone()));
        }

        Ok(values)
    }

    pub(in crate::executor) fn fast_graph_push_adjacency_neighbor_ids(
        &self,
        context: &ExecutionContext,
        edge_table_id: RelationId,
        node_id: &Value,
        outgoing: bool,
        remaining: Option<usize>,
        output: &mut Vec<Value>,
    ) -> DbResult<()> {
        let Some(max_new) = remaining else {
            let values = self.fast_graph_adjacency_neighbors_cached(
                context,
                edge_table_id,
                node_id,
                outgoing,
            )?;
            output.extend(values.into_iter().filter(|value| !value.is_null()));
            return Ok(());
        };
        if max_new == 0 {
            return Ok(());
        }

        let start_len = output.len();
        let generation = self.storage_dml.cache_generation();
        let cache_key = generation
            .and_then(|_| build_hash_key(node_id).ok())
            .map(|node_key| GraphAdjacencyNeighborsCacheKey {
                edge_table_id,
                node_key,
                outgoing,
            });

        if let (Some(cache_key), Some(generation)) = (&cache_key, generation) {
            let cache = self
                .graph_adjacency_neighbors_cache
                .read()
                .map_err(|error| {
                    DbError::internal(format!("graph adjacency cache poisoned: {error}"))
                })?;
            if let Some((cached_generation, values)) = cache.get(cache_key) {
                if *cached_generation == generation {
                    for value in values {
                        if value.is_null() {
                            continue;
                        }
                        output.push(value.clone());
                        if output.len() - start_len >= max_new {
                            break;
                        }
                    }
                    return Ok(());
                }
            }
        }

        let mut cursor = self.storage_dml.adjacency_neighbor_cursor(
            context.txn_id,
            &context.snapshot,
            edge_table_id,
            node_id,
            outgoing,
        )?;
        while let Some(value) = cursor.next_neighbor() {
            context.check_deadline()?;
            if value.is_null() {
                continue;
            }
            ensure_graph_workset_capacity(context, output.len(), "adjacency neighbor traversal")?;
            context.track_memory(estimate_value_bytes(&value).saturating_add(32))?;
            output.push(value);
            if output.len() - start_len >= max_new {
                break;
            }
        }
        Ok(())
    }

    pub(in crate::executor) fn fast_graph_adjacency_neighbor_count(
        &self,
        context: &ExecutionContext,
        edge_table_id: RelationId,
        node_id: &Value,
        outgoing: bool,
    ) -> DbResult<u64> {
        let cursor = self.storage_dml.adjacency_neighbor_cursor(
            context.txn_id,
            &context.snapshot,
            edge_table_id,
            node_id,
            outgoing,
        )?;
        Ok(usize_to_u64(cursor.remaining_hint()))
    }

    pub(in crate::executor) fn fast_graph_add_count_frontier_node(
        context: &ExecutionContext,
        frontier: &mut HashMap<ValueHashKey, (Value, u64)>,
        mut node_id: Value,
        multiplicity: u64,
    ) -> DbResult<()> {
        if node_id.is_null() || multiplicity == 0 {
            return Ok(());
        }
        normalize_int_key(&mut node_id);
        let key = build_hash_key(&node_id)?;
        match frontier.entry(key) {
            std::collections::hash_map::Entry::Occupied(mut entry) => {
                let count = &mut entry.get_mut().1;
                *count = count.saturating_add(multiplicity);
            }
            std::collections::hash_map::Entry::Vacant(entry) => {
                context.track_memory(estimate_value_bytes(&node_id).saturating_add(64))?;
                entry.insert((node_id, multiplicity));
            }
        }
        Ok(())
    }

    pub(in crate::executor) fn fast_graph_count_fixed_outgoing_paths(
        &self,
        context: &ExecutionContext,
        edge_table_id: RelationId,
        start_id: &Value,
        hops: usize,
    ) -> DbResult<u64> {
        if hops == 0 {
            return Ok(0);
        }
        if hops == 1 {
            return self.fast_graph_adjacency_neighbor_count(
                context,
                edge_table_id,
                start_id,
                true,
            );
        }
        if hops == 2 {
            let middle_ids =
                self.fast_graph_adjacency_neighbors_cached(context, edge_table_id, start_id, true)?;
            let mut count = 0u64;
            for mut middle_id in middle_ids {
                context.check_deadline()?;
                if middle_id.is_null() {
                    continue;
                }
                normalize_int_key(&mut middle_id);
                let degree = self.fast_graph_adjacency_neighbor_count(
                    context,
                    edge_table_id,
                    &middle_id,
                    true,
                )?;
                count = count.saturating_add(degree);
            }
            return Ok(count);
        }
        if hops == 3 {
            let first_ids =
                self.fast_graph_adjacency_neighbors_cached(context, edge_table_id, start_id, true)?;
            let mut count = 0u64;
            for mut first_id in first_ids {
                context.check_deadline()?;
                if first_id.is_null() {
                    continue;
                }
                normalize_int_key(&mut first_id);
                let second_ids = self.fast_graph_adjacency_neighbors_cached(
                    context,
                    edge_table_id,
                    &first_id,
                    true,
                )?;
                for mut second_id in second_ids {
                    context.check_deadline()?;
                    if second_id.is_null() {
                        continue;
                    }
                    normalize_int_key(&mut second_id);
                    let degree = self.fast_graph_adjacency_neighbor_count(
                        context,
                        edge_table_id,
                        &second_id,
                        true,
                    )?;
                    count = count.saturating_add(degree);
                }
            }
            return Ok(count);
        }

        let mut frontier = HashMap::new();
        Self::fast_graph_add_count_frontier_node(context, &mut frontier, start_id.clone(), 1)?;
        for _ in 1..hops {
            let mut next = HashMap::new();
            for (node_id, multiplicity) in frontier.into_values() {
                context.check_deadline()?;
                let neighbors = self.fast_graph_adjacency_neighbors_cached(
                    context,
                    edge_table_id,
                    &node_id,
                    true,
                )?;
                for neighbor_id in neighbors {
                    Self::fast_graph_add_count_frontier_node(
                        context,
                        &mut next,
                        neighbor_id,
                        multiplicity,
                    )?;
                }
            }
            if next.is_empty() {
                return Ok(0);
            }
            frontier = next;
        }

        let mut count = 0u64;
        for (node_id, multiplicity) in frontier.into_values() {
            context.check_deadline()?;
            let degree =
                self.fast_graph_adjacency_neighbor_count(context, edge_table_id, &node_id, true)?;
            count = count.saturating_add(multiplicity.saturating_mul(degree));
        }
        Ok(count)
    }

    pub(in crate::executor) fn fast_graph_count_distinct_fixed_outgoing_end_ids(
        &self,
        context: &ExecutionContext,
        edge_table_id: RelationId,
        start_id: &Value,
        hops: usize,
    ) -> DbResult<u64> {
        if hops == 0 {
            return Ok(0);
        }

        let mut frontier = HashMap::new();
        Self::fast_graph_add_count_frontier_node(context, &mut frontier, start_id.clone(), 1)?;
        for _ in 0..hops {
            let mut next = HashMap::new();
            for (node_id, _) in frontier.into_values() {
                context.check_deadline()?;
                let neighbors = self.fast_graph_adjacency_neighbors_cached(
                    context,
                    edge_table_id,
                    &node_id,
                    true,
                )?;
                for neighbor_id in neighbors {
                    Self::fast_graph_add_count_frontier_node(context, &mut next, neighbor_id, 1)?;
                }
            }
            if next.is_empty() {
                return Ok(0);
            }
            frontier = next;
        }

        Ok(usize_to_u64(frontier.len()))
    }

    pub(in crate::executor) fn fast_graph_count_fixed_outgoing_paths_to_allowed_end_ids(
        &self,
        context: &ExecutionContext,
        edge_table_id: RelationId,
        start_id: &Value,
        hops: usize,
        allowed_end_ids: &HashSet<ValueHashKey, join_plans::JoinFxBuildHasher>,
    ) -> DbResult<u64> {
        if hops == 0 {
            return Ok(0);
        }

        let mut frontier = HashMap::new();
        Self::fast_graph_add_count_frontier_node(context, &mut frontier, start_id.clone(), 1)?;
        for _ in 0..hops {
            let mut next = HashMap::new();
            for (node_id, multiplicity) in frontier.into_values() {
                context.check_deadline()?;
                let neighbors = self.fast_graph_adjacency_neighbors_cached(
                    context,
                    edge_table_id,
                    &node_id,
                    true,
                )?;
                for neighbor_id in neighbors {
                    Self::fast_graph_add_count_frontier_node(
                        context,
                        &mut next,
                        neighbor_id,
                        multiplicity,
                    )?;
                }
            }
            if next.is_empty() {
                return Ok(0);
            }
            frontier = next;
        }

        let mut count = 0u64;
        for (node_key, (_, multiplicity)) in frontier {
            if allowed_end_ids.contains(&node_key) {
                count = count.saturating_add(multiplicity);
            }
        }
        Ok(count)
    }

    pub(in crate::executor) fn fast_graph_count_variable_outgoing_paths_unique_edges(
        &self,
        context: &ExecutionContext,
        edge_table_id: RelationId,
        start_id: &Value,
        min_hops: usize,
        max_hops: usize,
    ) -> DbResult<u64> {
        if max_hops == 0 {
            return Ok(u64::from(min_hops == 0));
        }

        let mut count = u64::from(min_hops == 0);
        let mut frontier = vec![(start_id.clone(), Vec::new())];
        context.track_memory(estimate_value_bytes(start_id).saturating_add(64))?;

        for depth in 1..=max_hops {
            if frontier.is_empty() {
                break;
            }
            let mut next = Vec::new();
            for (mut node_id, path_edges) in frontier {
                context.check_deadline()?;
                if node_id.is_null() {
                    continue;
                }
                normalize_int_key(&mut node_id);
                let mut edge_cursor = self.storage_dml.adjacency_edge_cursor(
                    context.txn_id,
                    &context.snapshot,
                    edge_table_id,
                    &node_id,
                    true,
                )?;
                while let Some(edge_tuple_id) = edge_cursor.next_neighbor() {
                    context.check_deadline()?;
                    if path_edges.contains(&edge_tuple_id) {
                        continue;
                    }
                    let Some((_, mut target_id)) = self.storage_dml.adjacency_edge_endpoints(
                        context.txn_id,
                        &context.snapshot,
                        edge_table_id,
                        edge_tuple_id,
                    )?
                    else {
                        continue;
                    };
                    if target_id.is_null() {
                        continue;
                    }
                    normalize_int_key(&mut target_id);
                    if depth >= min_hops {
                        count = count.saturating_add(1);
                    }
                    if depth < max_hops {
                        let mut next_path_edges = path_edges.clone();
                        next_path_edges.push(edge_tuple_id);
                        context.track_memory(
                            estimate_value_bytes(&target_id)
                                .saturating_add(64)
                                .saturating_add(
                                    usize_to_u64(next_path_edges.len())
                                        .saturating_mul(size_of_u64::<aiondb_core::TupleId>()),
                                ),
                        )?;
                        ensure_graph_workset_capacity(
                            context,
                            next.len(),
                            "variable-length count frontier",
                        )?;
                        next.push((target_id, next_path_edges));
                    }
                }
            }
            frontier = next;
        }

        Ok(count)
    }

    pub(in crate::executor) fn fast_graph_collect_fixed_outgoing_endpoint_ids(
        &self,
        context: &ExecutionContext,
        edge_table_id: RelationId,
        start_id: &Value,
        hops: usize,
        ordered: bool,
        limit: Option<usize>,
    ) -> DbResult<Vec<Value>> {
        if hops == 0 {
            return Ok(Vec::new());
        }

        let mut current = vec![start_id.clone()];
        for depth in 1..=hops {
            let mut next = Vec::new();
            let is_last = depth == hops;
            'nodes: for mut node_id in current {
                context.check_deadline()?;
                if node_id.is_null() {
                    continue;
                }
                normalize_int_key(&mut node_id);
                let remaining = if is_last && !ordered {
                    limit.map(|limit| limit.saturating_sub(next.len()))
                } else {
                    None
                };
                self.fast_graph_push_adjacency_neighbor_ids(
                    context,
                    edge_table_id,
                    &node_id,
                    true,
                    remaining,
                    &mut next,
                )?;
                if is_last && !ordered && limit.is_some_and(|limit| next.len() >= limit) {
                    break 'nodes;
                }
            }
            if next.is_empty() {
                return Ok(next);
            }
            current = next;
        }

        if ordered {
            current.sort_by(|left, right| {
                compare_runtime_values(left, right)
                    .ok()
                    .flatten()
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
        }
        if let Some(limit) = limit {
            current.truncate(limit);
        }
        Ok(current)
    }

    pub(in crate::executor) fn fast_graph_id_lookup_cache_get(
        &self,
        edge_table_id: RelationId,
        start_id: &Value,
        hops: u8,
        ordered: bool,
        limit: Option<usize>,
    ) -> DbResult<Option<Vec<Row>>> {
        let Some(generation) = self.storage_dml.cache_generation() else {
            return Ok(None);
        };
        let Ok(start_key) = build_hash_key(start_id) else {
            return Ok(None);
        };
        let cache_key = GraphIdLookupResultCacheKey {
            edge_table_id,
            start_key,
            hops,
            ordered,
            limit,
        };
        let cached = self
            .graph_id_lookup_result_cache
            .read()
            .map_err(|error| DbError::internal(format!("graph id lookup cache poisoned: {error}")))?
            .get(&cache_key)
            .cloned();
        Ok(cached.and_then(|(cached_generation, rows)| {
            (cached_generation == generation).then_some(rows)
        }))
    }

    pub(in crate::executor) fn fast_graph_id_lookup_cache_put(
        &self,
        edge_table_id: RelationId,
        start_id: &Value,
        hops: u8,
        ordered: bool,
        limit: Option<usize>,
        rows: &[Row],
    ) -> DbResult<()> {
        let Some(generation) = self.storage_dml.cache_generation() else {
            return Ok(());
        };
        let Ok(start_key) = build_hash_key(start_id) else {
            return Ok(());
        };
        let cache_key = GraphIdLookupResultCacheKey {
            edge_table_id,
            start_key,
            hops,
            ordered,
            limit,
        };
        let mut cache = self.graph_id_lookup_result_cache.write().map_err(|error| {
            DbError::internal(format!("graph id lookup cache poisoned: {error}"))
        })?;
        if cache.len() >= 4096 {
            cache.clear();
        }
        cache.insert(cache_key, (generation, rows.to_vec()));
        Ok(())
    }

    pub(in crate::executor) fn fast_graph_collect_target_ids_filter_by_ordinal(
        &self,
        context: &ExecutionContext,
        target_table_id: RelationId,
        filter_ordinal: usize,
        comparison: GraphTargetFilterComparison,
        filter_value: &Value,
    ) -> DbResult<Option<Arc<HashSet<ValueHashKey, join_plans::JoinFxBuildHasher>>>> {
        let Some(target_table) = self
            .catalog_reader
            .get_table_by_id(context.txn_id, target_table_id)?
        else {
            return Ok(None);
        };
        let id_ordinal = self
            .find_column_index(&target_table.columns, "id")
            .unwrap_or(0);
        if filter_ordinal >= target_table.columns.len() {
            return Ok(None);
        }
        let mut required_ordinals = vec![id_ordinal];
        if filter_ordinal != id_ordinal {
            required_ordinals.push(filter_ordinal);
        }
        let filter_projected_ordinal = required_ordinals
            .iter()
            .position(|ordinal| *ordinal == filter_ordinal)
            .ok_or_else(|| DbError::internal("failed to map graph filter ordinal"))?;
        let Some(projected_columns) =
            self.table_column_ids_for_ordinals(context, target_table_id, &required_ordinals)?
        else {
            return Ok(None);
        };
        let cache_key = self
            .storage_dml
            .cache_generation()
            .and_then(|_| build_hash_key(filter_value).ok())
            .map(|filter_value| GraphTargetFilterIdsCacheKey {
                target_table_id,
                id_ordinal,
                filter_ordinal,
                comparison,
                filter_value,
            });
        if let (Some(cache_key), Some(generation)) =
            (&cache_key, self.storage_dml.cache_generation())
        {
            let cached = self
                .graph_target_filter_ids_cache
                .read()
                .map_err(|error| {
                    DbError::internal(format!("graph target filter cache poisoned: {error}"))
                })?
                .get(cache_key)
                .cloned();
            if let Some((cached_generation, allowed)) = cached {
                if cached_generation == generation {
                    return Ok(Some(allowed));
                }
            }
        }
        let mut stream = self.resolve_scan_stream(
            context,
            target_table_id,
            &ScanAccessPath::SeqScan,
            Some(projected_columns),
        )?;
        let mut allowed = HashSet::<ValueHashKey, join_plans::JoinFxBuildHasher>::with_hasher(
            join_plans::JoinFxBuildHasher::default(),
        );
        while let Some(record) = stream.next()? {
            context.check_deadline()?;
            let number = record
                .row
                .values
                .get(filter_projected_ordinal)
                .unwrap_or(&Value::Null);
            let Some(ordering) = compare_runtime_values(number, filter_value)? else {
                continue;
            };
            let matched = match comparison {
                GraphTargetFilterComparison::Eq => ordering == Ordering::Equal,
                GraphTargetFilterComparison::Gt => ordering == Ordering::Greater,
            };
            if !matched {
                continue;
            }
            let Some(id_value) = record.row.values.first() else {
                continue;
            };
            if id_value.is_null() {
                continue;
            }
            let mut normalized_id = id_value.clone();
            normalize_int_key(&mut normalized_id);
            let id_key = build_hash_key(&normalized_id)?;
            if allowed.insert(id_key) {
                context.track_memory(estimate_value_bytes(&normalized_id).saturating_add(32))?;
            }
        }
        let allowed = Arc::new(allowed);
        if let (Some(cache_key), Some(generation)) =
            (cache_key, self.storage_dml.cache_generation())
        {
            let mut cache = self
                .graph_target_filter_ids_cache
                .write()
                .map_err(|error| {
                    DbError::internal(format!("graph target filter cache poisoned: {error}"))
                })?;
            if cache.len() >= 256 {
                cache.clear();
            }
            cache.insert(cache_key, (generation, Arc::clone(&allowed)));
        }
        Ok(Some(allowed))
    }

    pub(in crate::executor) fn fast_graph_collect_target_ids_filter(
        &self,
        context: &ExecutionContext,
        target_table_id: RelationId,
        filter_column_name: &str,
        comparison: GraphTargetFilterComparison,
        filter_value: &Value,
    ) -> DbResult<Option<Arc<HashSet<ValueHashKey, join_plans::JoinFxBuildHasher>>>> {
        let Some(target_table) = self
            .catalog_reader
            .get_table_by_id(context.txn_id, target_table_id)?
        else {
            return Ok(None);
        };
        let Some(filter_ordinal) =
            self.find_column_index(&target_table.columns, filter_column_name)
        else {
            return Ok(None);
        };
        self.fast_graph_collect_target_ids_filter_by_ordinal(
            context,
            target_table_id,
            filter_ordinal,
            comparison,
            filter_value,
        )
    }

    pub(in crate::executor) fn fast_graph_collect_target_id_values_filter(
        &self,
        context: &ExecutionContext,
        target_table_id: RelationId,
        filter_column_name: &str,
        comparison: GraphTargetFilterComparison,
        filter_value: &Value,
    ) -> DbResult<Option<Vec<Value>>> {
        let Some(target_table) = self
            .catalog_reader
            .get_table_by_id(context.txn_id, target_table_id)?
        else {
            return Ok(None);
        };
        let id_ordinal = self
            .find_column_index(&target_table.columns, "id")
            .unwrap_or(0);
        let Some(filter_ordinal) =
            self.find_column_index(&target_table.columns, filter_column_name)
        else {
            return Ok(None);
        };
        let mut required_ordinals = vec![id_ordinal];
        if filter_ordinal != id_ordinal {
            required_ordinals.push(filter_ordinal);
        }
        let filter_projected_ordinal = required_ordinals
            .iter()
            .position(|ordinal| *ordinal == filter_ordinal)
            .ok_or_else(|| DbError::internal("failed to map graph filter ordinal"))?;
        let Some(projected_columns) =
            self.table_column_ids_for_ordinals(context, target_table_id, &required_ordinals)?
        else {
            return Ok(None);
        };
        let mut stream = self.resolve_scan_stream(
            context,
            target_table_id,
            &ScanAccessPath::SeqScan,
            Some(projected_columns),
        )?;
        let mut ids = Vec::new();
        while let Some(record) = stream.next()? {
            context.check_deadline()?;
            let candidate = record
                .row
                .values
                .get(filter_projected_ordinal)
                .unwrap_or(&Value::Null);
            let Some(ordering) = compare_runtime_values(candidate, filter_value)? else {
                continue;
            };
            let matched = match comparison {
                GraphTargetFilterComparison::Eq => ordering == Ordering::Equal,
                GraphTargetFilterComparison::Gt => ordering == Ordering::Greater,
            };
            if !matched {
                continue;
            }
            let Some(id_value) = record.row.values.first() else {
                continue;
            };
            if id_value.is_null() {
                continue;
            }
            let mut normalized_id = id_value.clone();
            normalize_int_key(&mut normalized_id);
            ensure_graph_workset_capacity(context, ids.len(), "graph target id filter values")?;
            context.track_memory(estimate_value_bytes(&normalized_id).saturating_add(32))?;
            ids.push(normalized_id);
        }
        Ok(Some(ids))
    }

    pub(in crate::executor) fn hybrid_deep_graph_vector_meta_cached(
        &self,
        context: &ExecutionContext,
        start_table_id: RelationId,
        friend_table_id: RelationId,
        source_table_id: RelationId,
        target_table_id: RelationId,
    ) -> DbResult<Option<HybridDeepGraphVectorMeta>> {
        let cache_key = HybridDeepGraphVectorMetaCacheKey {
            start_table_id,
            friend_table_id,
            source_table_id,
            target_table_id,
        };
        if let Some(generation) = self.storage_dml.cache_generation() {
            if let Some((cached_generation, meta)) = self
                .hybrid_deep_graph_vector_meta_cache
                .read()
                .map_err(|error| {
                    DbError::internal(format!("hybrid graph-vector meta cache poisoned: {error}"))
                })?
                .get(&cache_key)
                .cloned()
            {
                if cached_generation == generation {
                    return Ok(Some(meta));
                }
            }
        }

        let Some(start_columns) = self
            .catalog_reader
            .get_table_by_id(context.txn_id, start_table_id)?
            .map(|table| table.columns)
        else {
            return Ok(None);
        };
        let Some(friend_columns) = self
            .catalog_reader
            .get_table_by_id(context.txn_id, friend_table_id)?
            .map(|table| table.columns)
        else {
            return Ok(None);
        };
        let Some(source_columns) = self
            .catalog_reader
            .get_table_by_id(context.txn_id, source_table_id)?
            .map(|table| table.columns)
        else {
            return Ok(None);
        };
        let Some(target_columns) = self
            .catalog_reader
            .get_table_by_id(context.txn_id, target_table_id)?
            .map(|table| table.columns)
        else {
            return Ok(None);
        };

        let start_id_idx = self.find_column_index(&start_columns, "id").unwrap_or(0);
        let Some(start_tenant_idx) = self.find_column_index(&start_columns, "tenant_id") else {
            return Ok(None);
        };
        let friend_id_idx = self.find_column_index(&friend_columns, "id").unwrap_or(0);
        let Some(friend_tenant_idx) = self.find_column_index(&friend_columns, "tenant_id") else {
            return Ok(None);
        };
        let source_id_idx = self.find_column_index(&source_columns, "id").unwrap_or(0);
        let Some(source_title_idx) = self.find_column_index(&source_columns, "title") else {
            return Ok(None);
        };
        let Some(target_title_idx) = self.find_column_index(&target_columns, "title") else {
            return Ok(None);
        };
        let Some(target_tenant_idx) = self.find_column_index(&target_columns, "tenant_id") else {
            return Ok(None);
        };
        let Some(target_popularity_idx) = self.find_column_index(&target_columns, "popularity")
        else {
            return Ok(None);
        };
        let Some(target_embedding_idx) = self.find_column_index(&target_columns, "embedding")
        else {
            return Ok(None);
        };
        let Some(start_id_index) =
            self.find_first_column_btree_index_for_fast_graph(context, start_table_id)?
        else {
            return Ok(None);
        };
        let Some(friend_id_index) =
            self.find_first_column_btree_index_for_fast_graph(context, friend_table_id)?
        else {
            return Ok(None);
        };
        let Some(source_id_index) =
            self.find_first_column_btree_index_for_fast_graph(context, source_table_id)?
        else {
            return Ok(None);
        };
        let Some(target_id_index) =
            self.find_first_column_btree_index_for_fast_graph(context, target_table_id)?
        else {
            return Ok(None);
        };
        let meta = HybridDeepGraphVectorMeta {
            start_id_idx,
            start_tenant_idx,
            friend_id_idx,
            friend_tenant_idx,
            source_id_idx,
            source_title_idx,
            target_title_idx,
            target_tenant_idx,
            target_popularity_idx,
            target_embedding_idx,
            start_id_index,
            friend_id_index,
            source_id_index,
            target_id_index,
        };
        if let Some(generation) = self.storage_dml.cache_generation() {
            let mut cache = self
                .hybrid_deep_graph_vector_meta_cache
                .write()
                .map_err(|error| {
                    DbError::internal(format!("hybrid graph-vector meta cache poisoned: {error}"))
                })?;
            if cache.len() >= 256 {
                cache.clear();
            }
            cache.insert(cache_key, (generation, meta.clone()));
        }
        Ok(Some(meta))
    }
}
