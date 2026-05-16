//! Persistent compact **weighted** graph projection.
//!
//! The unweighted [`super::PersistentGraphProjection`] covers topology-only
//! algorithms; this is its weighted twin. It owns a compact
//! [`WeightedCsrGraph`], persists to a single blob, exposes [`GraphViewV2`]
//! for the generic algorithm suite *and* a `&WeightedCsrGraph` accessor for
//! the weight-aware ones (Bellman-Ford, Dijkstra, delta-stepping, weighted
//! PageRank, Yen, Steiner, …) -- all running with **zero rebuild**.
//!
//! Neo4j re-projects a weighted graph from the store every session; here a
//! weighted named graph is materialised once, durable, and incrementally
//! updatable.

use std::collections::BTreeMap;

use aiondb_core::{DbError, DbResult, Value};
use aiondb_graph::algorithms::WeightedCsrGraph;
use aiondb_graph_api::{
    GraphProjection, GraphStats, GraphViewV2, ProjectionSnapshot, RefreshPolicy,
};

/// How parallel edges (same `src`->`dst`) collapse when a multigraph is
/// projected to a simple graph -- Neo4j GDS relationship aggregation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum EdgeAggregation {
    /// Sum the parallel weights.
    Sum,
    /// Keep the minimum weight.
    Min,
    /// Keep the maximum weight.
    Max,
    /// Weight = number of parallel edges (Neo4j `COUNT`).
    Count,
    /// Keep the first weight encountered (Neo4j `SINGLE`).
    Single,
}

fn encode_key(value: &Value) -> DbResult<Vec<u8>> {
    bincode::serialize(value)
        .map_err(|e| DbError::internal(format!("projection node-id encode failed: {e}")))
}

/// A named, persistent, compact weighted graph projection.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct PersistentWeightedProjection {
    name: String,
    snapshot: ProjectionSnapshot,
    stats: GraphStats,
    graph: WeightedCsrGraph,
    node_ids: Vec<Value>,
    key_index: BTreeMap<Vec<u8>, u32>,
    /// Staged per-node f64 properties (weighted algorithm `.mutate` results,
    /// e.g. Dijkstra distances), ordinal-aligned and persisted.
    #[serde(default)]
    node_props: BTreeMap<String, Vec<f64>>,
    /// Staged per-node integer properties (community/component seeds).
    #[serde(default)]
    node_props_i64: BTreeMap<String, Vec<i64>>,
}

#[inline]
fn usize_for(v: u32) -> usize {
    usize::try_from(v).unwrap_or(usize::MAX)
}

impl PersistentWeightedProjection {
    /// Build from a flat weighted edge list `(src, dst, weight)`.
    #[must_use]
    pub fn from_edges(
        name: impl Into<String>,
        generation: u64,
        node_count: u32,
        edges: &[(u32, u32, f64)],
    ) -> Self {
        Self::from_weighted(
            name,
            generation,
            WeightedCsrGraph::from_edges(node_count, edges),
        )
    }

    /// Wrap an already-built compact weighted CSR graph.
    #[must_use]
    pub fn from_weighted(
        name: impl Into<String>,
        generation: u64,
        graph: WeightedCsrGraph,
    ) -> Self {
        let nodes = u64::from(graph.node_count());
        let stats = GraphStats {
            node_count: Some(nodes),
            edge_count: graph.edge_count(),
            source_node_count: Some(nodes),
            target_node_count: Some(nodes),
            has_reverse_adjacency: true,
            has_weighted_adjacency: true,
            directed: true,
        };
        Self {
            name: name.into(),
            snapshot: ProjectionSnapshot {
                generation,
                refresh_policy: RefreshPolicy::Snapshot,
                refreshed_at_epoch_millis: None,
            },
            stats,
            graph,
            node_ids: Vec::new(),
            key_index: BTreeMap::new(),
            node_props: BTreeMap::new(),
            node_props_i64: BTreeMap::new(),
        }
    }

    /// Attach external node identities (persisted with the topology).
    pub fn with_node_ids(mut self, node_ids: Vec<Value>) -> DbResult<Self> {
        let mut key_index = BTreeMap::new();
        for (ordinal, value) in node_ids.iter().enumerate() {
            let ord = u32::try_from(ordinal).map_err(|_| {
                DbError::internal("projection ordinal exceeds u32 capacity".to_owned())
            })?;
            key_index.entry(encode_key(value)?).or_insert(ord);
        }
        self.node_ids = node_ids;
        self.key_index = key_index;
        Ok(self)
    }

    /// Serialise the whole weighted projection to a durable blob.
    pub fn to_bytes(&self) -> DbResult<Vec<u8>> {
        bincode::serialize(self)
            .map_err(|e| DbError::internal(format!("weighted projection encode failed: {e}")))
    }

    /// Restore a projection produced by [`Self::to_bytes`].
    pub fn from_bytes(bytes: &[u8]) -> DbResult<Self> {
        bincode::deserialize(bytes)
            .map_err(|e| DbError::internal(format!("weighted projection decode failed: {e}")))
    }

    /// Borrow the compact weighted graph for the weight-aware algorithms
    /// (Bellman-Ford / Dijkstra / delta-stepping / weighted PageRank / …)
    /// with no rebuild.
    #[must_use]
    pub fn weighted(&self) -> &WeightedCsrGraph {
        &self.graph
    }

    /// All directed weighted edges, recovered from the compact store.
    #[must_use]
    pub fn edges(&self) -> Vec<(u32, u32, f64)> {
        let n = self.graph.node_count();
        let mut out = Vec::with_capacity(usize::try_from(self.graph.edge_count()).unwrap_or(0));
        for u in 0..n {
            for edge in self.graph.neighbors(u) {
                out.push((u, edge.target, edge.weight));
            }
        }
        out
    }

    /// Incrementally fold in extra weighted edges (delta rebuild + generation
    /// bump, no row-store scan) -- Neo4j has no in-place weighted-projection
    /// update.
    #[must_use]
    pub fn with_added_edges(&self, extra: &[(u32, u32, f64)]) -> Self {
        let mut all = self.edges();
        all.extend_from_slice(extra);
        let mut max_node = self.graph.node_count();
        for &(a, b, _) in extra {
            max_node = max_node.max(a.saturating_add(1)).max(b.saturating_add(1));
        }
        let graph = WeightedCsrGraph::from_edges(max_node, &all);
        let generation = self.snapshot.generation.saturating_add(1);
        let mut next = Self::from_weighted(self.name.clone(), generation, graph);
        next.node_ids = self.node_ids.clone();
        next.key_index = self.key_index.clone();
        next
    }

    /// Incrementally remove directed edges by endpoint pair (weight-agnostic,
    /// multiset: each listed `(src, dst)` cancels one matching edge). Node set
    /// and id mapping unchanged; generation bumped. Neo4j has no in-place
    /// weighted-edge removal.
    #[must_use]
    pub fn with_removed_edges(&self, drop: &[(u32, u32)]) -> Self {
        let mut to_drop: BTreeMap<(u32, u32), u32> = BTreeMap::new();
        for &e in drop {
            *to_drop.entry(e).or_insert(0) += 1;
        }
        let kept: Vec<(u32, u32, f64)> = self
            .edges()
            .into_iter()
            .filter(|&(u, v, _)| {
                if let Some(c) = to_drop.get_mut(&(u, v)) {
                    if *c > 0 {
                        *c -= 1;
                        return false;
                    }
                }
                true
            })
            .collect();
        let graph = WeightedCsrGraph::from_edges(self.graph.node_count(), &kept);
        let generation = self.snapshot.generation.saturating_add(1);
        let mut next = Self::from_weighted(self.name.clone(), generation, graph);
        next.node_ids = self.node_ids.clone();
        next.key_index = self.key_index.clone();
        next
    }

    /// Edge-induced view with a **weight-aware** predicate `pred(src, dst,
    /// weight)`. Every node is kept; a fresh generation marks a new named
    /// graph. The durable analogue of Neo4j's relationship-property filter.
    pub fn filter_edges<P>(&self, name: impl Into<String>, mut pred: P) -> Self
    where
        P: FnMut(u32, u32, f64) -> bool,
    {
        let kept: Vec<(u32, u32, f64)> = self
            .edges()
            .into_iter()
            .filter(|&(u, v, w)| pred(u, v, w))
            .collect();
        let graph = WeightedCsrGraph::from_edges(self.graph.node_count(), &kept);
        let mut next = Self::from_weighted(name, 1, graph);
        next.node_ids = self.node_ids.clone();
        next.key_index = self.key_index.clone();
        next
    }

    /// Node-induced weighted subgraph: keep ordinal `o` iff `keep[o]`,
    /// compactly relabel survivors, retain weighted edges with both endpoints
    /// kept, carry the surviving node-id mapping. Durable (Neo4j's is
    /// transient in-memory).
    #[must_use]
    pub fn subgraph(&self, name: impl Into<String>, keep: &[bool]) -> Self {
        let old_n = self.graph.node_count();
        let mut remap = vec![u32::MAX; usize_for(old_n)];
        let mut kept_ids: Vec<Value> = Vec::new();
        let mut next_id: u32 = 0;
        for old in 0..old_n {
            if keep.get(usize_for(old)).is_some_and(|&b| b) {
                remap[usize_for(old)] = next_id;
                if let Some(v) = self.node_ids.get(usize_for(old)) {
                    kept_ids.push(v.clone());
                }
                next_id += 1;
            }
        }
        let edges: Vec<(u32, u32, f64)> = self
            .edges()
            .into_iter()
            .filter_map(|(u, v, w)| {
                let nu = *remap.get(usize_for(u))?;
                let nv = *remap.get(usize_for(v))?;
                (nu != u32::MAX && nv != u32::MAX).then_some((nu, nv, w))
            })
            .collect();
        let mut next = Self::from_weighted(name, 1, WeightedCsrGraph::from_edges(next_id, &edges));
        if kept_ids.len() == usize_for(next_id) && next_id > 0 {
            next.set_ids(kept_ids);
        }
        next
    }

    /// Undirected weighted view (Neo4j GDS `ORIENTATION: UNDIRECTED`): each
    /// `(u, v, w)` is mirrored to `(v, u, w)` (multigraph semantics -- weights
    /// preserved, no aggregation). Required for weighted Louvain / weighted
    /// similarity; fresh generation, id mapping carried, durable.
    #[must_use]
    pub fn as_undirected(&self, name: impl Into<String>) -> Self {
        let mut edges = self.edges();
        let mirrored: Vec<(u32, u32, f64)> = edges.iter().map(|&(u, v, w)| (v, u, w)).collect();
        edges.extend(mirrored);
        let mut next = Self::from_weighted(
            name,
            1,
            WeightedCsrGraph::from_edges(self.graph.node_count(), &edges),
        );
        next.node_ids = self.node_ids.clone();
        next.key_index = self.key_index.clone();
        next
    }

    /// Collapse parallel edges into a **simple** weighted graph using
    /// `strategy` (Neo4j GDS relationship aggregation). Self-loops are kept
    /// but also aggregated. Fresh generation, id mapping carried, durable --
    /// makes weighted PageRank / shortest-path well-defined on a multigraph.
    #[must_use]
    pub fn aggregate_parallel_edges(
        &self,
        name: impl Into<String>,
        strategy: EdgeAggregation,
    ) -> Self {
        // (src,dst) -> (aggregated weight, count).
        let mut acc: BTreeMap<(u32, u32), (f64, u64)> = BTreeMap::new();
        for (u, v, w) in self.edges() {
            acc.entry((u, v))
                .and_modify(|(cur, cnt)| {
                    *cnt += 1;
                    match strategy {
                        EdgeAggregation::Sum | EdgeAggregation::Count => *cur += w,
                        EdgeAggregation::Min => *cur = cur.min(w),
                        EdgeAggregation::Max => *cur = cur.max(w),
                        EdgeAggregation::Single => {}
                    }
                })
                .or_insert((w, 1));
        }
        let edges: Vec<(u32, u32, f64)> = acc
            .into_iter()
            .map(|((u, v), (w, cnt))| {
                let weight = if strategy == EdgeAggregation::Count {
                    cnt as f64
                } else {
                    w
                };
                (u, v, weight)
            })
            .collect();
        let mut next = Self::from_weighted(
            name,
            1,
            WeightedCsrGraph::from_edges(self.graph.node_count(), &edges),
        );
        next.node_ids = self.node_ids.clone();
        next.key_index = self.key_index.clone();
        next
    }

    /// Transpose (Neo4j GDS `ORIENTATION: REVERSE`): each `(u, v, w)` becomes
    /// `(v, u, w)`, weight preserved. Fresh generation, id mapping carried,
    /// durable.
    #[must_use]
    pub fn as_reversed(&self, name: impl Into<String>) -> Self {
        let edges: Vec<(u32, u32, f64)> = self
            .edges()
            .into_iter()
            .map(|(u, v, w)| (v, u, w))
            .collect();
        let mut next = Self::from_weighted(
            name,
            1,
            WeightedCsrGraph::from_edges(self.graph.node_count(), &edges),
        );
        next.node_ids = self.node_ids.clone();
        next.key_index = self.key_index.clone();
        next
    }

    /// Drop every self-loop `(v, v, _)` (GDS projection-time exclusion).
    /// Fresh generation, id mapping carried, durable.
    #[must_use]
    pub fn without_self_loops(&self, name: impl Into<String>) -> Self {
        let edges: Vec<(u32, u32, f64)> = self
            .edges()
            .into_iter()
            .filter(|&(u, v, _)| u != v)
            .collect();
        let mut next = Self::from_weighted(
            name,
            1,
            WeightedCsrGraph::from_edges(self.graph.node_count(), &edges),
        );
        next.node_ids = self.node_ids.clone();
        next.key_index = self.key_index.clone();
        next
    }

    /// Out-degree distribution summary (Neo4j `gds.graph.list` shape).
    #[must_use]
    pub fn degree_summary(&self) -> crate::DegreeSummary {
        crate::degree_summary(&self.graph)
    }

    /// Estimated resident bytes of the compact weighted CSR topology.
    #[must_use]
    pub fn estimated_topology_bytes(&self) -> u64 {
        crate::estimate_weighted_csr_bytes(self.graph.node_count(), self.graph.edge_count())
    }

    /// Validate internal invariants (durable-catalog integrity check).
    pub fn verify(&self) -> DbResult<()> {
        let n = usize_for(self.graph.node_count());
        if !self.node_ids.is_empty() && self.node_ids.len() != n {
            return Err(DbError::internal(format!(
                "weighted projection '{}': node_ids len {} != node_count {n}",
                self.name,
                self.node_ids.len()
            )));
        }
        for &ord in self.key_index.values() {
            if usize_for(ord) >= n {
                return Err(DbError::internal(format!(
                    "weighted projection '{}': id-index ordinal {ord} out of range {n}",
                    self.name
                )));
            }
        }
        for (key, col) in &self.node_props {
            if col.len() != n {
                return Err(DbError::internal(format!(
                    "weighted projection '{}': f64 property '{key}' len {} != {n}",
                    self.name,
                    col.len()
                )));
            }
        }
        for (key, col) in &self.node_props_i64 {
            if col.len() != n {
                return Err(DbError::internal(format!(
                    "weighted projection '{}': i64 property '{key}' len {} != {n}",
                    self.name,
                    col.len()
                )));
            }
        }
        Ok(())
    }

    /// (Re)build the id mapping from an ordinal-aligned id list.
    fn set_ids(&mut self, node_ids: Vec<Value>) {
        let mut key_index = BTreeMap::new();
        for (ordinal, value) in node_ids.iter().enumerate() {
            if let (Ok(key), Ok(ord)) = (encode_key(value), u32::try_from(ordinal)) {
                key_index.entry(key).or_insert(ord);
            }
        }
        self.node_ids = node_ids;
        self.key_index = key_index;
    }

    /// Stage an ordinal-aligned f64 node property (weighted `.mutate`),
    /// persisted; reused rebuild-free and across restarts.
    pub fn set_node_property(&mut self, key: impl Into<String>, values: Vec<f64>) -> DbResult<()> {
        let n = usize_for(self.graph.node_count());
        if values.len() != n {
            return Err(DbError::internal(format!(
                "node property length {} != node count {n}",
                values.len()
            )));
        }
        self.node_props.insert(key.into(), values);
        Ok(())
    }

    /// Borrow a staged f64 node property.
    #[must_use]
    pub fn node_property(&self, key: &str) -> Option<&[f64]> {
        self.node_props.get(key).map(Vec::as_slice)
    }

    /// Stage an ordinal-aligned integer node property (e.g. community seed).
    pub fn set_node_property_i64(
        &mut self,
        key: impl Into<String>,
        values: Vec<i64>,
    ) -> DbResult<()> {
        let n = usize_for(self.graph.node_count());
        if values.len() != n {
            return Err(DbError::internal(format!(
                "i64 node property length {} != node count {n}",
                values.len()
            )));
        }
        self.node_props_i64.insert(key.into(), values);
        Ok(())
    }

    /// Borrow a staged integer node property.
    #[must_use]
    pub fn node_property_i64(&self, key: &str) -> Option<&[i64]> {
        self.node_props_i64.get(key).map(Vec::as_slice)
    }

    /// Sorted names of all staged properties (f64 then i64 namespaces).
    #[must_use]
    pub fn property_keys(&self) -> Vec<&str> {
        let mut v: Vec<&str> = self
            .node_props
            .keys()
            .chain(self.node_props_i64.keys())
            .map(String::as_str)
            .collect();
        v.sort_unstable();
        v.dedup();
        v
    }
}

impl GraphProjection for PersistentWeightedProjection {
    fn projection_name(&self) -> &str {
        &self.name
    }

    fn snapshot(&self) -> &ProjectionSnapshot {
        &self.snapshot
    }

    fn stats(&self) -> GraphStats {
        self.stats
    }

    fn graph_view(&self) -> &dyn GraphViewV2 {
        &self.graph
    }

    fn node_ordinal(&self, node_id: &Value) -> Option<u32> {
        self.key_index.get(&encode_key(node_id).ok()?).copied()
    }

    fn node_id(&self, ordinal: u32) -> Option<Value> {
        self.node_ids.get(usize::try_from(ordinal).ok()?).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WEDGES: &[(u32, u32, f64)] = &[
        (0, 1, 2.0),
        (1, 2, 3.0),
        (2, 0, 1.0),
        (2, 3, 1.0),
        (3, 1, 4.0),
    ];

    #[test]
    fn exposes_weighted_and_generic_views() {
        let p = PersistentWeightedProjection::from_edges("w", 1, 4, WEDGES);
        assert_eq!(p.graph_view().node_count(), 4);
        assert_eq!(p.weighted().node_count(), 4);
        assert!(p.stats().has_weighted_adjacency);
        // Weighted out-edges of node 0.
        let n0 = p.weighted().neighbors(0);
        assert_eq!(n0.len(), 1);
        assert_eq!(n0[0].target, 1);
        assert!((n0[0].weight - 2.0).abs() < 1e-12);
    }

    #[test]
    fn weighted_algorithms_run_rebuild_free_incl_after_reload() {
        let p = PersistentWeightedProjection::from_edges("w", 1, 4, WEDGES);
        let reloaded = PersistentWeightedProjection::from_bytes(&p.to_bytes().unwrap()).unwrap();
        let fresh = WeightedCsrGraph::from_edges(4, WEDGES);

        // Bellman-Ford (weight-aware) straight on the persisted projection.
        let d_proj = aiondb_graph::algorithms::bellman_ford::bellman_ford(p.weighted(), 0);
        let d_reload = aiondb_graph::algorithms::bellman_ford::bellman_ford(reloaded.weighted(), 0);
        let d_fresh = aiondb_graph::algorithms::bellman_ford::bellman_ford(&fresh, 0);
        assert_eq!(d_proj.distances, d_fresh.distances);
        assert_eq!(d_reload.distances, d_fresh.distances);

        // Weighted PageRank also consumes it directly.
        let pr_proj = aiondb_graph::algorithms::pagerank::weighted_pagerank_default(p.weighted());
        let pr_fresh = aiondb_graph::algorithms::pagerank::weighted_pagerank_default(&fresh);
        assert_eq!(pr_proj.len(), 4);
        for (a, b) in pr_proj.iter().zip(pr_fresh.iter()) {
            assert!((a - b).abs() < 1e-12);
        }
    }

    #[test]
    fn id_mapping_and_incremental_update() {
        let p = PersistentWeightedProjection::from_edges("w", 5, 4, WEDGES)
            .with_node_ids(vec![
                Value::BigInt(7),
                Value::BigInt(8),
                Value::BigInt(9),
                Value::BigInt(10),
            ])
            .unwrap();
        assert_eq!(p.node_ordinal(&Value::BigInt(9)), Some(2));
        assert_eq!(p.node_id(3), Some(Value::BigInt(10)));

        let p2 = p.with_added_edges(&[(0, 3, 0.5)]);
        assert_eq!(p2.snapshot().generation, 6);
        let n0 = p2.weighted().neighbors(0);
        assert!(n0
            .iter()
            .any(|e| e.target == 3 && (e.weight - 0.5).abs() < 1e-12));
        // id mapping carried, still persistable.
        let reloaded = PersistentWeightedProjection::from_bytes(&p2.to_bytes().unwrap()).unwrap();
        assert_eq!(reloaded.node_ordinal(&Value::BigInt(7)), Some(0));
    }

    #[test]
    fn weighted_mutation_and_subgraph_parity() {
        let p = PersistentWeightedProjection::from_edges("w", 3, 4, WEDGES);

        let r = p.with_removed_edges(&[(2, 0), (3, 1)]);
        assert_eq!(r.snapshot().generation, 4);
        assert_eq!(r.weighted().node_count(), 4);
        let mut e: Vec<_> = r.edges();
        e.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
        assert_eq!(e, vec![(0, 1, 2.0), (1, 2, 3.0), (2, 3, 1.0)]);

        // weight-aware edge filter.
        let light = p.filter_edges("light", |_, _, w| w <= 2.0);
        assert_eq!(light.weighted().node_count(), 4);
        assert_eq!(light.edges().len(), 3);
        assert!(light.edges().iter().all(|&(_, _, w)| w <= 2.0));

        // node-induced weighted subgraph keeps weights + remaps ids.
        let sub = p
            .with_node_ids(vec![
                Value::BigInt(1),
                Value::BigInt(2),
                Value::BigInt(3),
                Value::BigInt(4),
            ])
            .unwrap()
            .subgraph("sub", &[true, true, true, false]);
        assert_eq!(sub.weighted().node_count(), 3);
        assert_eq!(sub.edges().len(), 3);
        assert_eq!(sub.node_ordinal(&Value::BigInt(3)), Some(2));
        assert_eq!(sub.node_ordinal(&Value::BigInt(4)), None);
    }

    #[test]
    fn weighted_property_staging_is_durable() {
        let mut p = PersistentWeightedProjection::from_edges("w", 1, 4, WEDGES);
        let dist: Vec<f64> = vec![0.0, 2.0, 5.0, 6.0];
        p.set_node_property("dist", dist.clone()).unwrap();
        p.set_node_property_i64("seed", vec![0, 0, 1, 1]).unwrap();
        assert!(p.set_node_property("bad", vec![0.0; 2]).is_err());
        assert_eq!(p.property_keys(), vec!["dist", "seed"]);

        let back = PersistentWeightedProjection::from_bytes(&p.to_bytes().unwrap()).unwrap();
        assert_eq!(back.node_property("dist"), Some(dist.as_slice()));
        assert_eq!(
            back.node_property_i64("seed"),
            Some([0, 0, 1, 1].as_slice())
        );
    }

    #[test]
    fn weighted_undirected_mirrors_each_edge_with_weight() {
        let p = PersistentWeightedProjection::from_edges("w", 4, 4, WEDGES);
        let u = p.as_undirected("w_undir");
        assert_eq!(u.snapshot().generation, 1);
        assert_eq!(u.weighted().node_count(), 4);
        assert_eq!(u.weighted().edge_count(), WEDGES.len() as u64 * 2);
        // for the original (0,1,2.0) the reverse (1,0,2.0) now exists.
        let n1 = u.weighted().neighbors(1);
        assert!(n1
            .iter()
            .any(|e| e.target == 0 && (e.weight - 2.0).abs() < 1e-12));
    }

    #[test]
    fn relationship_aggregation_collapses_parallel_edges() {
        // multigraph: three 0->1 parallels + one 1->2.
        let multi: &[(u32, u32, f64)] = &[(0, 1, 2.0), (0, 1, 3.0), (0, 1, 1.0), (1, 2, 5.0)];
        let p = PersistentWeightedProjection::from_edges("m", 1, 3, multi);

        let weight_01 = |proj: &PersistentWeightedProjection| {
            proj.weighted()
                .neighbors(0)
                .iter()
                .find(|e| e.target == 1)
                .unwrap()
                .weight
        };

        let sum = p.aggregate_parallel_edges("s", EdgeAggregation::Sum);
        assert_eq!(sum.weighted().edge_count(), 2); // simple now
        assert!((weight_01(&sum) - 6.0).abs() < 1e-12);

        assert!(
            (weight_01(&p.aggregate_parallel_edges("a", EdgeAggregation::Min)) - 1.0).abs() < 1e-12
        );
        assert!(
            (weight_01(&p.aggregate_parallel_edges("b", EdgeAggregation::Max)) - 3.0).abs() < 1e-12
        );
        assert!(
            (weight_01(&p.aggregate_parallel_edges("c", EdgeAggregation::Count)) - 3.0).abs()
                < 1e-12
        );
        assert!(
            (weight_01(&p.aggregate_parallel_edges("d", EdgeAggregation::Single)) - 2.0).abs()
                < 1e-12
        );
    }

    #[test]
    fn weighted_without_self_loops_drops_them() {
        let p = PersistentWeightedProjection::from_edges(
            "w",
            2,
            3,
            &[(0, 0, 1.0), (0, 1, 2.0), (1, 1, 3.0), (1, 2, 4.0)],
        );
        let clean = p.without_self_loops("clean");
        assert_eq!(clean.snapshot().generation, 1);
        assert_eq!(clean.weighted().edge_count(), 2);
        let mut e: Vec<_> = clean.edges();
        e.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
        assert_eq!(e, vec![(0, 1, 2.0), (1, 2, 4.0)]);
    }

    #[test]
    fn weighted_reversed_transposes_and_keeps_weights() {
        let p = PersistentWeightedProjection::from_edges("w", 1, 4, WEDGES);
        let r = p.as_reversed("rev");
        assert_eq!(r.weighted().node_count(), 4);
        assert_eq!(r.weighted().edge_count(), WEDGES.len() as u64);
        // original (0,1,2.0) -> reversed (1,0,2.0).
        let n1 = r.weighted().neighbors(1);
        assert!(n1
            .iter()
            .any(|e| e.target == 0 && (e.weight - 2.0).abs() < 1e-12));
    }

    #[test]
    fn empty_weighted_projection_is_valid() {
        let p = PersistentWeightedProjection::from_edges("e", 0, 0, &[]);
        assert_eq!(p.graph_view().node_count(), 0);
        let r = PersistentWeightedProjection::from_bytes(&p.to_bytes().unwrap()).unwrap();
        assert_eq!(r.weighted().node_count(), 0);
    }
}
