use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
    time::{SystemTime, UNIX_EPOCH},
};

use aiondb_core::{hex_encode, DbError, DbResult, RelationId, Value};
use aiondb_eval::{build_hash_key, ValueHashKey};
use aiondb_graph::algorithms::{
    procedures::{execute_algorithm, AlgorithmConfig, AlgorithmResult},
    CsrGraph, WeightedCsrGraph,
};
use aiondb_graph::{GraphProjection, GraphStats, ProjectionSnapshot, RefreshPolicy};
use aiondb_graph_projection::{
    discovered_projection, discovered_projection_or_placeholder, ready_projection_descriptor,
    CypherNativeProjectionRegistry, DiscoveredProjection,
};
use aiondb_plan::graph::CypherProcedureCall;
use serde::{de::DeserializeOwned, Serialize};
use tracing::warn;

use super::{
    CurrentGraphAlgorithmCatalogSpec, ExecutionContext, Executor, GraphAlgorithmInputCacheEntry,
    GraphAlgorithmInputCacheKey, GraphAlgorithmResolvedEdgeLabel,
    GraphAlgorithmWeightedEdgesCacheKey,
};
use crate::executor::graph_runtime_args::{
    algorithm_config_from_args, weight_column_arg_from_args,
};

const GRAPH_ALGORITHM_INPUT_CACHE_LIMIT: usize = 8;
const GRAPH_ALGORITHM_WEIGHTED_EDGES_CACHE_LIMIT: usize = 16;
const GRAPH_ALGORITHM_INPUT_STORAGE_NAMESPACE: &str = "graph_algorithm_input";
const GRAPH_ALGORITHM_WEIGHTED_STORAGE_NAMESPACE: &str = "graph_algorithm_weighted";
const GRAPH_ALGORITHM_PERSISTED_CACHE_FORMAT_VERSION: u32 = 1;

#[derive(serde::Serialize, serde::Deserialize)]
struct PersistedGraphAlgorithmCacheEnvelope {
    version: u32,
    payload: Vec<u8>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct PersistedGraphAlgorithmInputCacheEntry {
    cache_key: GraphAlgorithmInputCacheKey,
    projection: aiondb_graph_projection::NamedGraphProjectionDescriptor,
    graph: CsrGraph,
    node_ids: Vec<Value>,
    node_table_ids: Vec<RelationId>,
    resolved_edges: Vec<GraphAlgorithmResolvedEdgeLabel>,
}

pub(crate) struct GraphAlgorithmProjectionRuntimeCache {
    input_cache:
        RwLock<HashMap<GraphAlgorithmInputCacheKey, (u64, Arc<GraphAlgorithmInputCacheEntry>)>>,
    projection_registry: CypherNativeProjectionRegistry<GraphAlgorithmInputCacheKey>,
}

impl GraphAlgorithmProjectionRuntimeCache {
    pub(super) fn new() -> Self {
        Self {
            input_cache: RwLock::new(HashMap::new()),
            projection_registry: CypherNativeProjectionRegistry::new(),
        }
    }

    pub(super) fn fresh_projection(
        &self,
        cache_key: &GraphAlgorithmInputCacheKey,
        generation: u64,
    ) -> DbResult<Option<DiscoveredProjection>> {
        Ok(self
            .input_cache
            .read()
            .map_err(|error| {
                DbError::internal(format!("graph algorithm input cache poisoned: {error}"))
            })?
            .get(cache_key)
            .cloned()
            .and_then(|(cached_generation, entry)| {
                (cached_generation == generation)
                    .then(|| discovered_projection(entry.projection.clone(), true))
            }))
    }

    pub(super) fn fresh_input_entry(
        &self,
        cache_key: &GraphAlgorithmInputCacheKey,
        generation: u64,
    ) -> DbResult<Option<Arc<GraphAlgorithmInputCacheEntry>>> {
        Ok(self
            .input_cache
            .read()
            .map_err(|error| {
                DbError::internal(format!("graph algorithm input cache poisoned: {error}"))
            })?
            .get(cache_key)
            .cloned()
            .and_then(|(cached_generation, entry)| {
                (cached_generation == generation).then_some(entry)
            }))
    }

    pub(super) fn remember_input_entry(
        &self,
        cache_key: GraphAlgorithmInputCacheKey,
        generation: u64,
        entry: Arc<GraphAlgorithmInputCacheEntry>,
    ) -> DbResult<()> {
        let mut cache = self.input_cache.write().map_err(|error| {
            DbError::internal(format!("graph algorithm input cache poisoned: {error}"))
        })?;
        if cache.len() >= GRAPH_ALGORITHM_INPUT_CACHE_LIMIT {
            cache.clear();
        }
        cache.insert(cache_key, (generation, entry));
        Ok(())
    }

    pub(super) fn remembered_projection(
        &self,
        cache_key: &GraphAlgorithmInputCacheKey,
    ) -> DbResult<Option<DiscoveredProjection>> {
        self.projection_registry.find_discovered(cache_key)
    }

    pub(super) fn remember_projection(
        &self,
        cache_key: GraphAlgorithmInputCacheKey,
        discovered: DiscoveredProjection,
    ) -> DbResult<()> {
        self.projection_registry
            .remember_discovered(cache_key, discovered)
    }

    pub(super) fn discover_projection(
        &self,
        cache_key: &GraphAlgorithmInputCacheKey,
        generation: u64,
        projection_name: &str,
        node_labels: &[(String, u64)],
        edge_labels: &[(String, u64)],
        snapshot: ProjectionSnapshot,
        weighted: bool,
    ) -> DbResult<DiscoveredProjection> {
        if let Some(discovered) = self.fresh_projection(cache_key, generation)? {
            return Ok(discovered);
        }
        if let Some(discovered) = self.remembered_projection(cache_key)? {
            return Ok(discovered);
        }
        self.resolve_named_projection_or_placeholder(
            projection_name,
            node_labels,
            edge_labels,
            snapshot,
            weighted,
        )
    }

    #[cfg(test)]
    pub(super) fn resolve_named_projection(
        &self,
        projection_name: &str,
    ) -> DbResult<Option<DiscoveredProjection>> {
        self.projection_registry
            .resolve_named_projection(projection_name)
    }

    pub(super) fn resolve_named_projection_or_placeholder(
        &self,
        projection_name: &str,
        node_labels: &[(String, u64)],
        edge_labels: &[(String, u64)],
        snapshot: ProjectionSnapshot,
        weighted: bool,
    ) -> DbResult<DiscoveredProjection> {
        self.projection_registry
            .resolve_named_projection_or_placeholder(
                projection_name,
                node_labels,
                edge_labels,
                snapshot,
                weighted,
            )
    }
}

pub(crate) struct GraphAlgorithmWeightedEdgesRuntimeCache {
    weighted_edges_cache:
        RwLock<HashMap<GraphAlgorithmWeightedEdgesCacheKey, (u64, Arc<WeightedCsrGraph>)>>,
}

impl GraphAlgorithmWeightedEdgesRuntimeCache {
    pub(super) fn new() -> Self {
        Self {
            weighted_edges_cache: RwLock::new(HashMap::new()),
        }
    }

    pub(super) fn fresh_weighted_edges(
        &self,
        cache_key: &GraphAlgorithmWeightedEdgesCacheKey,
        generation: u64,
    ) -> DbResult<Option<Arc<WeightedCsrGraph>>> {
        Ok(self
            .weighted_edges_cache
            .read()
            .map_err(|error| {
                DbError::internal(format!(
                    "graph algorithm weighted edge cache poisoned: {error}"
                ))
            })?
            .get(cache_key)
            .cloned()
            .and_then(|(cached_generation, entry)| {
                (cached_generation == generation).then_some(entry)
            }))
    }

    pub(super) fn remember_weighted_edges(
        &self,
        cache_key: GraphAlgorithmWeightedEdgesCacheKey,
        generation: u64,
        entry: Arc<WeightedCsrGraph>,
    ) -> DbResult<()> {
        let mut cache = self.weighted_edges_cache.write().map_err(|error| {
            DbError::internal(format!(
                "graph algorithm weighted edge cache poisoned: {error}"
            ))
        })?;
        if cache.len() >= GRAPH_ALGORITHM_WEIGHTED_EDGES_CACHE_LIMIT {
            cache.clear();
        }
        cache.insert(cache_key, (generation, entry));
        Ok(())
    }

    pub(super) fn get_or_build_weighted_edges<F>(
        &self,
        cache_key: GraphAlgorithmWeightedEdgesCacheKey,
        generation: u64,
        build: F,
    ) -> DbResult<Arc<WeightedCsrGraph>>
    where
        F: FnOnce() -> DbResult<WeightedCsrGraph>,
    {
        if let Some(entry) = self.fresh_weighted_edges(&cache_key, generation)? {
            return Ok(entry);
        }

        let entry = Arc::new(build()?);
        self.remember_weighted_edges(cache_key, generation, Arc::clone(&entry))?;
        Ok(entry)
    }
}

pub(super) struct GraphAlgorithmRuntimeCaches {
    pub(super) projection_cache: GraphAlgorithmProjectionRuntimeCache,
    pub(super) weighted_edges_cache: GraphAlgorithmWeightedEdgesRuntimeCache,
}

impl GraphAlgorithmRuntimeCaches {
    pub(super) fn new() -> Self {
        Self {
            projection_cache: GraphAlgorithmProjectionRuntimeCache::new(),
            weighted_edges_cache: GraphAlgorithmWeightedEdgesRuntimeCache::new(),
        }
    }
}

impl Executor {
    fn encode_versioned_graph_algorithm_cache_payload<T: Serialize>(
        value: &T,
    ) -> DbResult<Vec<u8>> {
        let payload = bincode::serialize(value).map_err(|error| {
            DbError::internal(format!("graph projection cache encode failed: {error}"))
        })?;
        bincode::serialize(&PersistedGraphAlgorithmCacheEnvelope {
            version: GRAPH_ALGORITHM_PERSISTED_CACHE_FORMAT_VERSION,
            payload,
        })
        .map_err(|error| {
            DbError::internal(format!(
                "graph projection cache envelope encode failed: {error}"
            ))
        })
    }

    fn decode_versioned_graph_algorithm_cache_payload<T: DeserializeOwned>(
        namespace: &str,
        payload: &[u8],
    ) -> Option<T> {
        if let Ok(envelope) = bincode::deserialize::<PersistedGraphAlgorithmCacheEnvelope>(payload)
        {
            if envelope.version != GRAPH_ALGORITHM_PERSISTED_CACHE_FORMAT_VERSION {
                warn!(
                    namespace,
                    found_version = envelope.version,
                    expected_version = GRAPH_ALGORITHM_PERSISTED_CACHE_FORMAT_VERSION,
                    "ignoring unsupported graph projection cache payload version"
                );
                return None;
            }
            return bincode::deserialize::<T>(&envelope.payload).ok();
        }
        bincode::deserialize::<T>(payload).ok()
    }

    fn graph_algorithm_storage_cache_key<T: serde::Serialize>(value: &T) -> DbResult<String> {
        let encoded = bincode::serialize(value).map_err(|error| {
            DbError::internal(format!("graph projection cache key encode: {error}"))
        })?;
        Ok(hex_encode(&encoded))
    }

    fn persisted_graph_algorithm_input_payload_from_entry(
        entry: &GraphAlgorithmInputCacheEntry,
    ) -> DbResult<PersistedGraphAlgorithmInputCacheEntry> {
        let mut node_table_ids = vec![RelationId::new(0); entry.node_ids.len()];
        let mut seen = vec![false; entry.node_ids.len()];
        for ((table_id, _), ordinal) in &entry.node_indexes {
            let index = usize::try_from(*ordinal)
                .map_err(|_| DbError::internal("graph projection node ordinal exceeds usize"))?;
            let Some(slot) = node_table_ids.get_mut(index) else {
                return Err(DbError::internal(
                    "graph projection node ordinal out of bounds while persisting cache",
                ));
            };
            *slot = RelationId::new(*table_id);
            seen[index] = true;
        }
        if seen.iter().any(|present| !present) {
            return Err(DbError::internal(
                "graph projection cache persistence found unmapped node ordinals",
            ));
        }
        Ok(PersistedGraphAlgorithmInputCacheEntry {
            cache_key: entry.cache_key.clone(),
            projection: entry.projection.clone(),
            graph: entry.graph.clone(),
            node_ids: entry.node_ids.clone(),
            node_table_ids,
            resolved_edges: entry.resolved_edges.clone(),
        })
    }

    fn graph_algorithm_input_entry_from_persisted_payload(
        payload: PersistedGraphAlgorithmInputCacheEntry,
    ) -> DbResult<GraphAlgorithmInputCacheEntry> {
        if payload.node_ids.len() != payload.node_table_ids.len() {
            return Err(DbError::internal(
                "graph projection cache payload node id/table id length mismatch",
            ));
        }
        let mut node_indexes = HashMap::<(u64, ValueHashKey), u32>::new();
        let mut node_value_indexes = HashMap::<ValueHashKey, Vec<u32>>::new();
        for (ordinal, (node_id, table_id)) in payload
            .node_ids
            .iter()
            .zip(payload.node_table_ids.iter())
            .enumerate()
        {
            let key = build_hash_key(node_id)?;
            let ordinal = u32::try_from(ordinal).map_err(|_| {
                DbError::program_limit("graph projection cache node ordinal exceeds u32 capacity")
            })?;
            node_indexes.insert((table_id.get(), key.clone()), ordinal);
            node_value_indexes.entry(key).or_default().push(ordinal);
        }
        Ok(GraphAlgorithmInputCacheEntry {
            cache_key: payload.cache_key,
            projection: payload.projection,
            graph: payload.graph,
            node_ids: payload.node_ids,
            node_indexes,
            node_value_indexes,
            resolved_edges: payload.resolved_edges,
        })
    }

    fn load_persisted_graph_algorithm_input_entry(
        &self,
        cache_key: &GraphAlgorithmInputCacheKey,
        generation: u64,
    ) -> DbResult<Option<GraphAlgorithmInputCacheEntry>> {
        let storage_key = Self::graph_algorithm_storage_cache_key(cache_key)?;
        let Some(payload) = self.storage_dml.graph_projection_cache_get(
            GRAPH_ALGORITHM_INPUT_STORAGE_NAMESPACE,
            &storage_key,
            generation,
        )?
        else {
            return Ok(None);
        };
        let Some(decoded) = Self::decode_versioned_graph_algorithm_cache_payload::<
            PersistedGraphAlgorithmInputCacheEntry,
        >(GRAPH_ALGORITHM_INPUT_STORAGE_NAMESPACE, &payload) else {
            return Ok(None);
        };
        if decoded.cache_key != *cache_key {
            return Ok(None);
        }
        Self::graph_algorithm_input_entry_from_persisted_payload(decoded).map(Some)
    }

    fn persist_graph_algorithm_input_entry(
        &self,
        generation: u64,
        entry: &GraphAlgorithmInputCacheEntry,
    ) -> DbResult<()> {
        let storage_key = Self::graph_algorithm_storage_cache_key(&entry.cache_key)?;
        let payload = Self::persisted_graph_algorithm_input_payload_from_entry(entry)?;
        let encoded = Self::encode_versioned_graph_algorithm_cache_payload(&payload)?;
        self.storage_dml.graph_projection_cache_put(
            GRAPH_ALGORITHM_INPUT_STORAGE_NAMESPACE,
            &storage_key,
            generation,
            &encoded,
        )
    }

    fn load_persisted_graph_algorithm_weighted_edges(
        &self,
        cache_key: &GraphAlgorithmWeightedEdgesCacheKey,
        generation: u64,
    ) -> DbResult<Option<Arc<WeightedCsrGraph>>> {
        let storage_key = Self::graph_algorithm_storage_cache_key(cache_key)?;
        let Some(payload) = self.storage_dml.graph_projection_cache_get(
            GRAPH_ALGORITHM_WEIGHTED_STORAGE_NAMESPACE,
            &storage_key,
            generation,
        )?
        else {
            return Ok(None);
        };
        let Some(decoded) = Self::decode_versioned_graph_algorithm_cache_payload::<WeightedCsrGraph>(
            GRAPH_ALGORITHM_WEIGHTED_STORAGE_NAMESPACE,
            &payload,
        ) else {
            return Ok(None);
        };
        Ok(Some(Arc::new(decoded)))
    }

    fn persist_graph_algorithm_weighted_edges(
        &self,
        cache_key: &GraphAlgorithmWeightedEdgesCacheKey,
        generation: u64,
        weighted: &WeightedCsrGraph,
    ) -> DbResult<()> {
        let storage_key = Self::graph_algorithm_storage_cache_key(cache_key)?;
        let encoded = Self::encode_versioned_graph_algorithm_cache_payload(weighted)?;
        self.storage_dml.graph_projection_cache_put(
            GRAPH_ALGORITHM_WEIGHTED_STORAGE_NAMESPACE,
            &storage_key,
            generation,
            &encoded,
        )
    }

    pub(super) fn build_current_graph_algorithm_runtime_entry_for_call(
        &self,
        context: &ExecutionContext,
        call: &CypherProcedureCall,
    ) -> DbResult<(
        Arc<GraphAlgorithmInputCacheEntry>,
        Option<Arc<WeightedCsrGraph>>,
    )> {
        let weight_column = weight_column_arg_from_args(&call.procedure, &call.args)?;
        self.build_current_graph_algorithm_runtime_entry(
            context,
            &call.procedure,
            weight_column.as_deref(),
        )
    }

    pub(super) fn build_current_graph_algorithm_runtime_entry(
        &self,
        context: &ExecutionContext,
        procedure: &str,
        weight_column: Option<&str>,
    ) -> DbResult<(
        Arc<GraphAlgorithmInputCacheEntry>,
        Option<Arc<WeightedCsrGraph>>,
    )> {
        let Some(weight_column) = weight_column else {
            return Ok((
                self.build_current_graph_algorithm_input_entry(context)?,
                None,
            ));
        };
        let spec = self.current_graph_algorithm_catalog_spec(context.txn_id)?;
        let input_cache_key = spec.cache_key.clone();
        let weight_cache_key = GraphAlgorithmWeightedEdgesCacheKey {
            input: input_cache_key.clone(),
            weight_column: weight_column.to_owned(),
        };

        if let Some(generation) = self.storage_dml.cache_generation() {
            if let Some(input) = self
                .graph_algorithm_projection_runtime()
                .fresh_input_entry(&input_cache_key, generation)?
            {
                let weighted = if let Some(weighted) = self
                    .graph_algorithm_weighted_runtime()
                    .fresh_weighted_edges(&weight_cache_key, generation)?
                {
                    weighted
                } else if let Some(weighted) = self
                    .load_persisted_graph_algorithm_weighted_edges(&weight_cache_key, generation)?
                {
                    self.graph_algorithm_weighted_runtime()
                        .remember_weighted_edges(
                            weight_cache_key.clone(),
                            generation,
                            Arc::clone(&weighted),
                        )?;
                    weighted
                } else {
                    let weighted = self
                        .graph_algorithm_weighted_runtime()
                        .get_or_build_weighted_edges(
                            weight_cache_key.clone(),
                            generation,
                            || {
                                self.build_graph_algorithm_weighted_edges_uncached(
                                    context,
                                    &input,
                                    procedure,
                                    weight_column,
                                )
                            },
                        )?;
                    self.persist_graph_algorithm_weighted_edges(
                        &weight_cache_key,
                        generation,
                        weighted.as_ref(),
                    )?;
                    weighted
                };
                return Ok((input, Some(weighted)));
            }

            if let Some(entry) =
                self.load_persisted_graph_algorithm_input_entry(&input_cache_key, generation)?
            {
                let entry = Arc::new(entry);
                self.graph_algorithm_projection_runtime()
                    .remember_input_entry(
                        input_cache_key.clone(),
                        generation,
                        Arc::clone(&entry),
                    )?;
                self.graph_algorithm_projection_runtime()
                    .remember_projection(
                        input_cache_key.clone(),
                        discovered_projection(entry.projection.clone(), true),
                    )?;

                let weighted = if let Some(weighted) = self
                    .load_persisted_graph_algorithm_weighted_edges(&weight_cache_key, generation)?
                {
                    self.graph_algorithm_weighted_runtime()
                        .remember_weighted_edges(
                            weight_cache_key.clone(),
                            generation,
                            Arc::clone(&weighted),
                        )?;
                    weighted
                } else {
                    let weighted = self
                        .graph_algorithm_weighted_runtime()
                        .get_or_build_weighted_edges(
                            weight_cache_key.clone(),
                            generation,
                            || {
                                self.build_graph_algorithm_weighted_edges_uncached(
                                    context,
                                    &entry,
                                    procedure,
                                    weight_column,
                                )
                            },
                        )?;
                    self.persist_graph_algorithm_weighted_edges(
                        &weight_cache_key,
                        generation,
                        weighted.as_ref(),
                    )?;
                    weighted
                };
                return Ok((entry, Some(weighted)));
            }

            let (entry, weighted) = self
                .build_graph_algorithm_input_entry_with_weighted_edges_uncached(
                    context,
                    generation,
                    spec,
                    procedure,
                    weight_column,
                )?;
            let entry = Arc::new(entry);
            let weighted = Arc::new(weighted);
            self.graph_algorithm_projection_runtime()
                .remember_input_entry(input_cache_key.clone(), generation, Arc::clone(&entry))?;
            self.graph_algorithm_weighted_runtime()
                .remember_weighted_edges(
                    weight_cache_key.clone(),
                    generation,
                    Arc::clone(&weighted),
                )?;
            self.persist_graph_algorithm_input_entry(generation, &entry)?;
            self.persist_graph_algorithm_weighted_edges(
                &weight_cache_key,
                generation,
                weighted.as_ref(),
            )?;
            return Ok((entry, Some(weighted)));
        }

        let (entry, weighted) = self
            .build_graph_algorithm_input_entry_with_weighted_edges_uncached(
                context,
                self.storage_dml.cache_generation().unwrap_or(0),
                spec,
                procedure,
                weight_column,
            )?;
        Ok((Arc::new(entry), Some(Arc::new(weighted))))
    }

    pub(super) fn build_current_graph_algorithm_input_entry(
        &self,
        context: &ExecutionContext,
    ) -> DbResult<Arc<GraphAlgorithmInputCacheEntry>> {
        let spec = self.current_graph_algorithm_catalog_spec(context.txn_id)?;

        if let Some(generation) = self.storage_dml.cache_generation() {
            let cache_key = spec.cache_key.clone();
            if let Some(entry) = self
                .graph_algorithm_projection_runtime()
                .fresh_input_entry(&cache_key, generation)?
            {
                return Ok(entry);
            }
            if let Some(entry) =
                self.load_persisted_graph_algorithm_input_entry(&cache_key, generation)?
            {
                let entry = Arc::new(entry);
                self.graph_algorithm_projection_runtime()
                    .remember_input_entry(cache_key.clone(), generation, Arc::clone(&entry))?;
                self.graph_algorithm_projection_runtime()
                    .remember_projection(
                        cache_key,
                        discovered_projection(entry.projection.clone(), true),
                    )?;
                return Ok(entry);
            }
            let entry = Arc::new(
                self.build_graph_algorithm_input_entry_uncached(context, generation, spec)?,
            );
            self.graph_algorithm_projection_runtime()
                .remember_input_entry(cache_key, generation, Arc::clone(&entry))?;
            self.persist_graph_algorithm_input_entry(generation, &entry)?;
            return Ok(entry);
        }

        Ok(Arc::new(self.build_graph_algorithm_input_entry_uncached(
            context,
            self.storage_dml.cache_generation().unwrap_or(0),
            spec,
        )?))
    }

    pub(super) fn build_current_graph_algorithm_weighted_edges(
        &self,
        context: &ExecutionContext,
        input: &GraphAlgorithmInputCacheEntry,
        procedure: &str,
        weight_column: &str,
    ) -> DbResult<Arc<WeightedCsrGraph>> {
        let cache_key = GraphAlgorithmWeightedEdgesCacheKey {
            input: input.cache_key.clone(),
            weight_column: weight_column.to_owned(),
        };

        if let Some(generation) = self.storage_dml.cache_generation() {
            if let Some(weighted) = self
                .graph_algorithm_weighted_runtime()
                .fresh_weighted_edges(&cache_key, generation)?
            {
                return Ok(weighted);
            }
            if let Some(weighted) =
                self.load_persisted_graph_algorithm_weighted_edges(&cache_key, generation)?
            {
                self.graph_algorithm_weighted_runtime()
                    .remember_weighted_edges(
                        cache_key.clone(),
                        generation,
                        Arc::clone(&weighted),
                    )?;
                return Ok(weighted);
            }
            let weighted = self
                .graph_algorithm_weighted_runtime()
                .get_or_build_weighted_edges(cache_key.clone(), generation, || {
                    self.build_graph_algorithm_weighted_edges_uncached(
                        context,
                        input,
                        procedure,
                        weight_column,
                    )
                })?;
            self.persist_graph_algorithm_weighted_edges(&cache_key, generation, weighted.as_ref())?;
            return Ok(weighted);
        }

        Ok(Arc::new(
            self.build_graph_algorithm_weighted_edges_uncached(
                context,
                input,
                procedure,
                weight_column,
            )?,
        ))
    }

    pub(super) fn execute_current_graph_algorithm(
        &self,
        input: &GraphAlgorithmInputCacheEntry,
        procedure: &str,
        config: &AlgorithmConfig,
    ) -> DbResult<Vec<AlgorithmResult>> {
        let projection = input.projection_ref();
        let projection_node_count =
            usize::try_from(projection.graph_view().node_count()).unwrap_or(usize::MAX);
        if projection_node_count != input.node_ids.len() {
            return Err(DbError::internal(format!(
                "graph projection node count mismatch: view reports {}, ids contain {}",
                projection_node_count,
                input.node_ids.len()
            )));
        }
        execute_algorithm(procedure, projection.graph_view(), config).map_err(DbError::internal)
    }

    pub(super) fn prepare_current_graph_algorithm_config_with_prebuilt_weighted_edges(
        &self,
        context: &ExecutionContext,
        call: &CypherProcedureCall,
        input: &GraphAlgorithmInputCacheEntry,
        prebuilt_weighted_edges: Option<Arc<WeightedCsrGraph>>,
    ) -> DbResult<AlgorithmConfig> {
        let mut config = algorithm_config_from_args(
            &call.procedure,
            &call.args,
            &input.node_value_indexes,
            &input.node_ids,
        )?;
        if let Some(weighted_edges) = prebuilt_weighted_edges {
            config.weighted_edges = Some(weighted_edges);
        } else if let Some(weight_column) = config.weight_column.as_deref() {
            config.weighted_edges = Some(self.build_current_graph_algorithm_weighted_edges(
                context,
                input,
                &call.procedure,
                weight_column,
            )?);
        }
        Ok(config)
    }

    pub(super) fn describe_current_cypher_projection(
        &self,
        txn_id: aiondb_core::TxnId,
    ) -> DbResult<DiscoveredProjection> {
        let spec = self.current_graph_algorithm_catalog_spec(txn_id)?;
        let generation = self.storage_dml.cache_generation().unwrap_or(0);
        let snapshot = ProjectionSnapshot {
            generation,
            refresh_policy: RefreshPolicy::Snapshot,
            refreshed_at_epoch_millis: None,
        };
        self.graph_algorithm_projection_runtime()
            .discover_projection(
                &spec.cache_key,
                snapshot.generation,
                spec.projection_name().as_str(),
                &spec.node_label_keys(),
                &spec.edge_label_keys(),
                snapshot,
                false,
            )
    }

    pub(super) fn describe_current_cypher_projection_or_placeholder(
        &self,
        txn_id: aiondb_core::TxnId,
        weighted: bool,
    ) -> DiscoveredProjection {
        self.describe_current_cypher_projection(txn_id)
            .unwrap_or_else(|_| {
                discovered_projection_or_placeholder(
                    None,
                    &[],
                    &[],
                    ProjectionSnapshot {
                        generation: self.storage_dml.cache_generation().unwrap_or(0),
                        refresh_policy: RefreshPolicy::Snapshot,
                        refreshed_at_epoch_millis: None,
                    },
                    weighted,
                )
            })
    }

    fn graph_algorithm_value_to_weight(
        procedure: &str,
        value: &Value,
        column_name: &str,
    ) -> DbResult<f64> {
        let weight = match value {
            Value::Int(value) => f64::from(*value),
            Value::BigInt(value) => value.to_string().parse::<f64>().map_err(|error| {
                DbError::syntax_error(format!(
                    "CALL {procedure} weight column {column_name} is not a valid number: {error}"
                ))
            })?,
            Value::Real(value) => f64::from(*value),
            Value::Double(value) => *value,
            Value::Numeric(value) => value.to_string().parse::<f64>().map_err(|error| {
                DbError::syntax_error(format!(
                    "CALL {procedure} weight column {column_name} is not a valid number: {error}"
                ))
            })?,
            other => {
                return Err(DbError::syntax_error(format!(
                    "CALL {procedure} weight column {column_name} requires a number, got {other:?}"
                )));
            }
        };
        if !weight.is_finite() || weight < 0.0 {
            return Err(DbError::syntax_error(format!(
                "CALL {procedure} weight column {column_name} must contain finite non-negative values"
            )));
        }
        Ok(weight)
    }

    fn intern_graph_algorithm_node(
        node_indexes: &mut HashMap<(u64, ValueHashKey), u32>,
        node_value_indexes: &mut HashMap<ValueHashKey, Vec<u32>>,
        node_ids: &mut Vec<Value>,
        table_id: RelationId,
        node_id: &Value,
    ) -> DbResult<u32> {
        let node_key = build_hash_key(node_id)?;
        let key = (table_id.get(), node_key.clone());
        if let Some(index) = node_indexes.get(&key) {
            return Ok(*index);
        }
        let index = u32::try_from(node_indexes.len()).map_err(|_| {
            DbError::program_limit("Cypher graph procedure input exceeds u32 node id capacity")
        })?;
        node_indexes.insert(key, index);
        node_value_indexes.entry(node_key).or_default().push(index);
        node_ids.push(node_id.clone());
        Ok(index)
    }

    pub(super) fn build_graph_algorithm_input_entry_uncached(
        &self,
        context: &ExecutionContext,
        projection_generation: u64,
        spec: CurrentGraphAlgorithmCatalogSpec,
    ) -> DbResult<GraphAlgorithmInputCacheEntry> {
        let projection_name = spec.projection_name();
        let refreshed_at_epoch_millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|duration| u64::try_from(duration.as_millis()).ok());
        let mut node_indexes = HashMap::<(u64, ValueHashKey), u32>::new();
        let mut node_value_indexes = HashMap::<ValueHashKey, Vec<u32>>::new();
        let mut node_ids = Vec::<Value>::new();
        let mut edges = Vec::<(u32, u32)>::new();
        let resolved_edges =
            self.resolve_algorithm_edge_labels(context, &spec.node_labels, &spec.edge_labels)?;

        for node_label in &spec.node_labels {
            let projected_columns =
                self.table_column_ids_for_ordinals(context, node_label.table_id, &[0])?;
            let mut stream =
                self.scan_table_locked(context, node_label.table_id, projected_columns)?;
            while let Some(record) = stream.next()? {
                context.check_deadline()?;
                let Some(node_id) = record.row.values.first() else {
                    continue;
                };
                Self::intern_graph_algorithm_node(
                    &mut node_indexes,
                    &mut node_value_indexes,
                    &mut node_ids,
                    node_label.table_id,
                    node_id,
                )?;
            }
        }

        for edge_label in &resolved_edges {
            if self
                .storage_dml
                .adjacency_index_available(context.txn_id, edge_label.table_id)
            {
                for (_tuple_id, source_id, target_id) in self.storage_dml.adjacency_edges(
                    context.txn_id,
                    &context.snapshot,
                    edge_label.table_id,
                )? {
                    context.check_deadline()?;
                    let source = Self::intern_graph_algorithm_node(
                        &mut node_indexes,
                        &mut node_value_indexes,
                        &mut node_ids,
                        edge_label.source_table_id,
                        &source_id,
                    )?;
                    let target = Self::intern_graph_algorithm_node(
                        &mut node_indexes,
                        &mut node_value_indexes,
                        &mut node_ids,
                        edge_label.target_table_id,
                        &target_id,
                    )?;
                    edges.push((source, target));
                }
                continue;
            }

            let mut stream = self.scan_table_locked(
                context,
                edge_label.table_id,
                Some(edge_label.projected_columns.clone()),
            )?;
            while let Some(record) = stream.next()? {
                context.check_deadline()?;
                let (Some(source_id), Some(target_id)) =
                    (record.row.values.first(), record.row.values.get(1))
                else {
                    continue;
                };
                let source = Self::intern_graph_algorithm_node(
                    &mut node_indexes,
                    &mut node_value_indexes,
                    &mut node_ids,
                    edge_label.source_table_id,
                    source_id,
                )?;
                let target = Self::intern_graph_algorithm_node(
                    &mut node_indexes,
                    &mut node_value_indexes,
                    &mut node_ids,
                    edge_label.target_table_id,
                    target_id,
                )?;
                edges.push((source, target));
            }
        }

        let graph = CsrGraph::from_edges(
            u32::try_from(node_indexes.len()).map_err(|_| {
                DbError::program_limit("Cypher graph procedure input exceeds u32 node id capacity")
            })?,
            &edges,
        );
        let graph_stats = GraphStats {
            node_count: Some(u64::try_from(node_ids.len()).unwrap_or(u64::MAX)),
            edge_count: u64::try_from(edges.len()).unwrap_or(u64::MAX),
            source_node_count: None,
            target_node_count: None,
            has_reverse_adjacency: true,
            has_weighted_adjacency: false,
            directed: true,
        };
        let projection = ready_projection_descriptor(
            projection_name,
            ProjectionSnapshot {
                generation: projection_generation,
                refresh_policy: RefreshPolicy::Snapshot,
                refreshed_at_epoch_millis,
            },
            graph_stats,
        );
        let entry = GraphAlgorithmInputCacheEntry {
            cache_key: spec.cache_key,
            projection: projection.clone(),
            graph,
            node_ids,
            node_indexes,
            node_value_indexes,
            resolved_edges,
        };
        self.graph_algorithm_projection_runtime()
            .remember_projection(
                entry.cache_key.clone(),
                discovered_projection(projection, true),
            )?;
        Ok(entry)
    }

    pub(super) fn build_graph_algorithm_input_entry_with_weighted_edges_uncached(
        &self,
        context: &ExecutionContext,
        projection_generation: u64,
        spec: CurrentGraphAlgorithmCatalogSpec,
        procedure: &str,
        weight_column: &str,
    ) -> DbResult<(GraphAlgorithmInputCacheEntry, WeightedCsrGraph)> {
        let projection_name = spec.projection_name();
        let refreshed_at_epoch_millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|duration| u64::try_from(duration.as_millis()).ok());
        let mut node_indexes = HashMap::<(u64, ValueHashKey), u32>::new();
        let mut node_value_indexes = HashMap::<ValueHashKey, Vec<u32>>::new();
        let mut node_ids = Vec::<Value>::new();
        let mut edges = Vec::<(u32, u32)>::new();
        let mut weighted = Vec::<(u32, u32, f64)>::new();
        let resolved_edges =
            self.resolve_algorithm_edge_labels(context, &spec.node_labels, &spec.edge_labels)?;
        let weight_column_key = weight_column.to_ascii_lowercase();

        for node_label in &spec.node_labels {
            let projected_columns =
                self.table_column_ids_for_ordinals(context, node_label.table_id, &[0])?;
            let mut stream =
                self.scan_table_locked(context, node_label.table_id, projected_columns)?;
            while let Some(record) = stream.next()? {
                context.check_deadline()?;
                let Some(node_id) = record.row.values.first() else {
                    continue;
                };
                Self::intern_graph_algorithm_node(
                    &mut node_indexes,
                    &mut node_value_indexes,
                    &mut node_ids,
                    node_label.table_id,
                    node_id,
                )?;
            }
        }

        for edge_label in &resolved_edges {
            let weight_idx = *edge_label
                .column_name_indexes
                .get(&weight_column_key)
                .ok_or_else(|| {
                    DbError::syntax_error(format!(
                        "CALL {procedure} weight column {weight_column} does not exist on edge label {}",
                        edge_label.label
                    ))
                })?;
            let weight_column_id = *edge_label
                .table_column_ids
                .get(weight_idx)
                .ok_or_else(|| DbError::internal("edge weight column ordinal out of bounds"))?;
            if self
                .storage_dml
                .adjacency_index_available(context.txn_id, edge_label.table_id)
            {
                for (_tuple_id, source_id, target_id, weight_value) in
                    self.storage_dml.adjacency_weighted_edges(
                        context.txn_id,
                        &context.snapshot,
                        edge_label.table_id,
                        weight_column_id,
                    )?
                {
                    context.check_deadline()?;
                    let source = Self::intern_graph_algorithm_node(
                        &mut node_indexes,
                        &mut node_value_indexes,
                        &mut node_ids,
                        edge_label.source_table_id,
                        &source_id,
                    )?;
                    let target = Self::intern_graph_algorithm_node(
                        &mut node_indexes,
                        &mut node_value_indexes,
                        &mut node_ids,
                        edge_label.target_table_id,
                        &target_id,
                    )?;
                    let weight = Self::graph_algorithm_value_to_weight(
                        procedure,
                        &weight_value,
                        weight_column,
                    )?;
                    edges.push((source, target));
                    weighted.push((source, target, weight));
                }
                continue;
            }

            let mut projected_columns = Vec::with_capacity(3);
            let mut source_value_idx = 0usize;
            let mut target_value_idx = 0usize;
            let mut weight_value_idx = 0usize;
            for (ordinal, slot) in [
                (edge_label.source_col_idx, &mut source_value_idx),
                (edge_label.target_col_idx, &mut target_value_idx),
                (weight_idx, &mut weight_value_idx),
            ] {
                let column_id = *edge_label.table_column_ids.get(ordinal).ok_or_else(|| {
                    DbError::internal("edge projected column ordinal out of bounds")
                })?;
                if let Some(existing_idx) = projected_columns.iter().position(|id| *id == column_id)
                {
                    *slot = existing_idx;
                } else {
                    *slot = projected_columns.len();
                    projected_columns.push(column_id);
                }
            }
            let mut stream =
                self.scan_table_locked(context, edge_label.table_id, Some(projected_columns))?;
            while let Some(record) = stream.next()? {
                context.check_deadline()?;
                let (Some(source_id), Some(target_id), Some(weight_value)) = (
                    record.row.values.get(source_value_idx),
                    record.row.values.get(target_value_idx),
                    record.row.values.get(weight_value_idx),
                ) else {
                    continue;
                };
                let source = Self::intern_graph_algorithm_node(
                    &mut node_indexes,
                    &mut node_value_indexes,
                    &mut node_ids,
                    edge_label.source_table_id,
                    source_id,
                )?;
                let target = Self::intern_graph_algorithm_node(
                    &mut node_indexes,
                    &mut node_value_indexes,
                    &mut node_ids,
                    edge_label.target_table_id,
                    target_id,
                )?;
                let weight =
                    Self::graph_algorithm_value_to_weight(procedure, weight_value, weight_column)?;
                edges.push((source, target));
                weighted.push((source, target, weight));
            }
        }

        let graph = CsrGraph::from_edges(
            u32::try_from(node_indexes.len()).map_err(|_| {
                DbError::program_limit("Cypher graph procedure input exceeds u32 node id capacity")
            })?,
            &edges,
        );
        let graph_stats = GraphStats {
            node_count: Some(u64::try_from(node_ids.len()).unwrap_or(u64::MAX)),
            edge_count: u64::try_from(edges.len()).unwrap_or(u64::MAX),
            source_node_count: None,
            target_node_count: None,
            has_reverse_adjacency: true,
            has_weighted_adjacency: true,
            directed: true,
        };
        let projection = ready_projection_descriptor(
            projection_name,
            ProjectionSnapshot {
                generation: projection_generation,
                refresh_policy: RefreshPolicy::Snapshot,
                refreshed_at_epoch_millis,
            },
            graph_stats,
        );
        let entry = GraphAlgorithmInputCacheEntry {
            cache_key: spec.cache_key,
            projection: projection.clone(),
            graph,
            node_ids,
            node_indexes,
            node_value_indexes,
            resolved_edges,
        };
        self.graph_algorithm_projection_runtime()
            .remember_projection(
                entry.cache_key.clone(),
                discovered_projection(projection, true),
            )?;
        Ok((
            entry,
            WeightedCsrGraph::from_edges(
                u32::try_from(graph_stats.node_count.unwrap_or(u64::MAX)).unwrap_or(u32::MAX),
                &weighted,
            ),
        ))
    }

    pub(super) fn build_graph_algorithm_weighted_edges_uncached(
        &self,
        context: &ExecutionContext,
        input: &GraphAlgorithmInputCacheEntry,
        procedure: &str,
        weight_column: &str,
    ) -> DbResult<WeightedCsrGraph> {
        let projection = input.projection_ref();
        let mut weighted = Vec::<(u32, u32, f64)>::new();
        let weight_column_key = weight_column.to_ascii_lowercase();
        for edge_label in &input.resolved_edges {
            let weight_idx = *edge_label
                .column_name_indexes
                .get(&weight_column_key)
                .ok_or_else(|| {
                    DbError::syntax_error(format!(
                        "CALL {procedure} weight column {weight_column} does not exist on edge label {}",
                        edge_label.label
                    ))
                })?;
            let weight_column_id = *edge_label
                .table_column_ids
                .get(weight_idx)
                .ok_or_else(|| DbError::internal("edge weight column ordinal out of bounds"))?;
            if self
                .storage_dml
                .adjacency_index_available(context.txn_id, edge_label.table_id)
            {
                for (_tuple_id, source_id, target_id, weight_value) in
                    self.storage_dml.adjacency_weighted_edges(
                        context.txn_id,
                        &context.snapshot,
                        edge_label.table_id,
                        weight_column_id,
                    )?
                {
                    context.check_deadline()?;
                    let source_key = (
                        edge_label.source_table_id.get(),
                        build_hash_key(&source_id)?,
                    );
                    let target_key = (
                        edge_label.target_table_id.get(),
                        build_hash_key(&target_id)?,
                    );
                    let (Some(source), Some(target)) = (
                        input.node_indexes.get(&source_key).copied(),
                        input.node_indexes.get(&target_key).copied(),
                    ) else {
                        continue;
                    };
                    let weight = Self::graph_algorithm_value_to_weight(
                        procedure,
                        &weight_value,
                        weight_column,
                    )?;
                    let _ = usize::try_from(source).map_err(|_| {
                        DbError::program_limit(
                            "Cypher graph procedure source index exceeds usize capacity",
                        )
                    })?;
                    weighted.push((source, target, weight));
                }
                continue;
            }

            let mut projected_columns = Vec::with_capacity(3);
            let mut source_value_idx = 0usize;
            let mut target_value_idx = 0usize;
            let mut weight_value_idx = 0usize;
            for (ordinal, slot) in [
                (edge_label.source_col_idx, &mut source_value_idx),
                (edge_label.target_col_idx, &mut target_value_idx),
                (weight_idx, &mut weight_value_idx),
            ] {
                let column_id = *edge_label.table_column_ids.get(ordinal).ok_or_else(|| {
                    DbError::internal("edge projected column ordinal out of bounds")
                })?;
                if let Some(existing_idx) = projected_columns.iter().position(|id| *id == column_id)
                {
                    *slot = existing_idx;
                } else {
                    *slot = projected_columns.len();
                    projected_columns.push(column_id);
                }
            }
            let mut stream =
                self.scan_table_locked(context, edge_label.table_id, Some(projected_columns))?;
            while let Some(record) = stream.next()? {
                context.check_deadline()?;
                let (Some(source_id), Some(target_id), Some(weight_value)) = (
                    record.row.values.get(source_value_idx),
                    record.row.values.get(target_value_idx),
                    record.row.values.get(weight_value_idx),
                ) else {
                    continue;
                };
                let source_key = (edge_label.source_table_id.get(), build_hash_key(source_id)?);
                let target_key = (edge_label.target_table_id.get(), build_hash_key(target_id)?);
                let (Some(source), Some(target)) = (
                    input.node_indexes.get(&source_key).copied(),
                    input.node_indexes.get(&target_key).copied(),
                ) else {
                    continue;
                };
                let weight =
                    Self::graph_algorithm_value_to_weight(procedure, weight_value, weight_column)?;
                let _ = usize::try_from(source).map_err(|_| {
                    DbError::program_limit(
                        "Cypher graph procedure source index exceeds usize capacity",
                    )
                })?;
                weighted.push((source, target, weight));
            }
        }
        Ok(WeightedCsrGraph::from_edges(
            projection.graph_view().node_count(),
            &weighted,
        ))
    }
}
