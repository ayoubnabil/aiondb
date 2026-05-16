//! Real persistent compact graph projection.
//!
//! The rest of this crate is catalog *metadata*; this is the actual store.
//! A [`PersistentGraphProjection`] owns the compact CSR topology, serialises
//! to a single binary blob (so a projection survives process restarts
//! instead of being rebuilt from the row store on every query), and exposes
//! [`GraphViewV2`] directly -- algorithms execute straight on it with **no
//! in-memory adjacency rebuild**.
//!
//! This is the foundation that lets the engine keep large named graphs
//! resident/persisted the way Neo4j's in-memory catalog does, but durable.

use std::collections::BTreeMap;

use aiondb_core::{DbError, DbResult, Value};
use aiondb_graph::algorithms::CsrGraph;
use aiondb_graph_api::{
    GraphProjection, GraphStats, GraphViewV2, ProjectionSnapshot, RefreshPolicy,
};

use crate::persistent_weighted::PersistentWeightedProjection;

/// A named graph projection that owns its compact CSR topology and can be
/// persisted to / restored from a byte blob.
///
/// The optional id mapping lets the engine round-trip results back to the
/// original node identities through a *persisted* projection, instead of
/// re-deriving the ordinal mapping from the row store on every query.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct PersistentGraphProjection {
    name: String,
    snapshot: ProjectionSnapshot,
    stats: GraphStats,
    csr: CsrGraph,
    /// ordinal -> external node id (empty = ordinal-only projection).
    node_ids: Vec<Value>,
    /// bincode(Value) -> ordinal, for `node_ordinal` lookups without
    /// requiring `Value: Eq + Hash`. Persisted alongside the topology.
    key_index: BTreeMap<Vec<u8>, u32>,
    /// Staged per-node f64 properties (algorithm `.mutate` results), keyed by
    /// property name; each vector is ordinal-aligned (`len == node_count`).
    /// Persisted with the topology, so a pipeline's intermediate scores
    /// survive a restart -- Neo4j's mutated properties are in-memory only.
    #[serde(default)]
    node_props: BTreeMap<String, Vec<f64>>,
    /// Staged per-node **integer** properties (community/component seeds,
    /// labels). Persisted, so a seed survives a restart and seeded
    /// community detection runs off the durable projection -- Neo4j's
    /// `seedProperty` must be re-read from the store every session.
    #[serde(default)]
    node_props_i64: BTreeMap<String, Vec<i64>>,
}

fn encode_key(value: &Value) -> DbResult<Vec<u8>> {
    bincode::serialize(value)
        .map_err(|e| DbError::internal(format!("projection node-id encode failed: {e}")))
}

impl PersistentGraphProjection {
    /// Build a projection from a flat edge list. The CSR is materialised once
    /// here; thereafter algorithms reuse it with zero rebuild.
    #[must_use]
    pub fn from_edges(
        name: impl Into<String>,
        generation: u64,
        node_count: u32,
        edges: &[(u32, u32)],
    ) -> Self {
        Self::from_csr(name, generation, CsrGraph::from_edges(node_count, edges))
    }

    /// Wrap an already-built compact CSR graph as a named projection.
    #[must_use]
    pub fn from_csr(name: impl Into<String>, generation: u64, csr: CsrGraph) -> Self {
        let nodes = u64::from(csr.node_count());
        let stats = GraphStats {
            node_count: Some(nodes),
            edge_count: csr.edge_count(),
            source_node_count: Some(nodes),
            target_node_count: Some(nodes),
            has_reverse_adjacency: true,
            has_weighted_adjacency: false,
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
            csr,
            node_ids: Vec::new(),
            key_index: BTreeMap::new(),
            node_props: BTreeMap::new(),
            node_props_i64: BTreeMap::new(),
        }
    }

    /// Build a projection that also remembers the external node identities.
    /// `node_ids[o]` is the original id of ordinal `o`; the reverse index is
    /// computed once and persisted with the topology.
    pub fn from_csr_with_ids(
        name: impl Into<String>,
        generation: u64,
        csr: CsrGraph,
        node_ids: Vec<Value>,
    ) -> DbResult<Self> {
        let mut base = Self::from_csr(name, generation, csr);
        let mut key_index = BTreeMap::new();
        for (ordinal, value) in node_ids.iter().enumerate() {
            let key = encode_key(value)?;
            let ord = u32::try_from(ordinal).map_err(|_| {
                DbError::internal("projection ordinal exceeds u32 capacity".to_owned())
            })?;
            key_index.entry(key).or_insert(ord);
        }
        base.node_ids = node_ids;
        base.key_index = key_index;
        Ok(base)
    }

    /// Edge-list builder that also records external node identities.
    pub fn from_edges_with_ids(
        name: impl Into<String>,
        generation: u64,
        node_ids: Vec<Value>,
        edges: &[(u32, u32)],
    ) -> DbResult<Self> {
        let node_count = u32::try_from(node_ids.len()).map_err(|_| {
            DbError::internal("projection node count exceeds u32 capacity".to_owned())
        })?;
        Self::from_csr_with_ids(
            name,
            generation,
            CsrGraph::from_edges(node_count, edges),
            node_ids,
        )
    }

    /// Serialise the whole projection -- topology included -- to a compact
    /// binary blob suitable for durable storage.
    pub fn to_bytes(&self) -> DbResult<Vec<u8>> {
        bincode::serialize(self)
            .map_err(|e| DbError::internal(format!("projection encode failed: {e}")))
    }

    /// Restore a projection previously produced by [`Self::to_bytes`].
    pub fn from_bytes(bytes: &[u8]) -> DbResult<Self> {
        bincode::deserialize(bytes)
            .map_err(|e| DbError::internal(format!("projection decode failed: {e}")))
    }

    /// Borrow the compact CSR view for direct, rebuild-free algorithm runs.
    #[must_use]
    pub fn view(&self) -> &CsrGraph {
        &self.csr
    }

    /// All directed edges currently in the projection, recovered from the
    /// compact CSR (used for incremental updates).
    #[must_use]
    pub fn edges(&self) -> Vec<(u32, u32)> {
        let n = self.csr.node_count();
        let mut out = Vec::with_capacity(usize::try_from(self.csr.edge_count()).unwrap_or(0));
        for u in 0..n {
            if let Some(neighbors) = GraphViewV2::neighbor_slice(&self.csr, u) {
                for &v in neighbors {
                    out.push((u, v));
                }
            }
        }
        out
    }

    /// Incrementally update the projection with extra directed edges,
    /// returning a new generation.
    ///
    /// Neo4j has no in-place projection update -- changing the graph means
    /// dropping and fully re-projecting from the store. Here the compact
    /// topology is rebuilt from itself plus the delta (no row-store scan),
    /// the generation is bumped, and the id mapping is carried over, so a
    /// persisted named graph can evolve cheaply and stay durable.
    #[must_use]
    pub fn with_added_edges(&self, extra: &[(u32, u32)]) -> Self {
        let mut all = self.edges();
        all.extend_from_slice(extra);
        let mut max_node = self.csr.node_count();
        for &(a, b) in extra {
            max_node = max_node.max(a.saturating_add(1)).max(b.saturating_add(1));
        }
        let csr = CsrGraph::from_edges(max_node, &all);
        let generation = self.snapshot.generation.saturating_add(1);
        let mut next = Self::from_csr(self.name.clone(), generation, csr);
        // Carry the existing id mapping forward (new ordinals, if any, are
        // simply unmapped until the caller supplies ids).
        next.node_ids = self.node_ids.clone();
        next.key_index = self.key_index.clone();
        next
    }

    /// Incrementally grow the projection with **new nodes (carrying their
    /// external ids) and new edges**, preserving a consistent id mapping --
    /// the gap [`Self::with_added_edges`] leaves (it cannot map freshly added
    /// ordinals). New edges must stay within the declared node space
    /// (existing + newly added); reference an undeclared node and you get an
    /// error rather than a silently unmapped vertex. Neo4j cannot grow a
    /// projection at all -- it must drop and fully re-project.
    pub fn with_added_nodes_and_edges(
        &self,
        new_node_ids: &[Value],
        extra_edges: &[(u32, u32)],
    ) -> DbResult<Self> {
        let added = u32::try_from(new_node_ids.len())
            .map_err(|_| DbError::internal("added node count exceeds u32 capacity".to_owned()))?;
        let declared = self
            .csr
            .node_count()
            .checked_add(added)
            .ok_or_else(|| DbError::internal("projection node count overflow".to_owned()))?;
        for &(a, b) in extra_edges {
            if a >= declared || b >= declared {
                return Err(DbError::internal(format!(
                    "edge ({a},{b}) references an undeclared node (declared={declared})"
                )));
            }
        }
        let mut all = self.edges();
        all.extend_from_slice(extra_edges);
        let csr = CsrGraph::from_edges(declared, &all);
        let generation = self.snapshot.generation.saturating_add(1);

        // Identity rules: a mapped base stays fully mapped; an unmapped base
        // may only gain ids if it is currently empty.
        if self.node_ids.is_empty() {
            if new_node_ids.is_empty() {
                let mut next = Self::from_csr(self.name.clone(), generation, csr);
                next.node_ids = self.node_ids.clone();
                next.key_index = self.key_index.clone();
                return Ok(next);
            }
            if self.csr.node_count() != 0 {
                return Err(DbError::internal(
                    "cannot attach ids: base projection is unmapped".to_owned(),
                ));
            }
        }
        let mut ids = self.node_ids.clone();
        ids.extend_from_slice(new_node_ids);
        if ids.len() != usize_for(declared) {
            return Err(DbError::internal(format!(
                "id mapping length {} != declared node count {declared}",
                ids.len()
            )));
        }
        Self::from_csr_with_ids(self.name.clone(), generation, csr, ids)
    }

    /// Incrementally **remove** directed edges (multiset semantics: each
    /// listed `(src, dst)` cancels one matching edge), bumping the
    /// generation. The node set and id mapping are unchanged; unknown edges
    /// are ignored. Completes the incremental-maintenance story --
    /// `with_added_edges`'s inverse -- which Neo4j's projection model lacks
    /// entirely (it can only drop and fully re-project).
    #[must_use]
    pub fn with_removed_edges(&self, drop: &[(u32, u32)]) -> Self {
        let mut to_drop: BTreeMap<(u32, u32), u32> = BTreeMap::new();
        for &e in drop {
            *to_drop.entry(e).or_insert(0) += 1;
        }
        let kept: Vec<(u32, u32)> = self
            .edges()
            .into_iter()
            .filter(|e| {
                if let Some(c) = to_drop.get_mut(e) {
                    if *c > 0 {
                        *c -= 1;
                        return false;
                    }
                }
                true
            })
            .collect();
        let csr = CsrGraph::from_edges(self.csr.node_count(), &kept);
        let generation = self.snapshot.generation.saturating_add(1);
        let mut next = Self::from_csr(self.name.clone(), generation, csr);
        next.node_ids = self.node_ids.clone();
        next.key_index = self.key_index.clone();
        next
    }

    /// Project a **node-induced subgraph**: keep ordinal `o` iff `keep[o]`,
    /// compactly relabel survivors `0..k`, and retain only edges with both
    /// endpoints kept.
    ///
    /// Unlike Neo4j's transient in-memory subgraph projection, the result is
    /// itself a durable named graph: fresh generation, remapped (not just
    /// filtered) topology, and the external node-id mapping is carried over
    /// for exactly the surviving nodes -- so results still round-trip to the
    /// original identities after a reload.
    #[must_use]
    pub fn subgraph(&self, name: impl Into<String>, keep: &[bool]) -> Self {
        let old_n = self.csr.node_count();
        let mut remap = vec![u32::MAX; usize_for(old_n)];
        let mut kept_ids: Vec<Value> = Vec::new();
        let mut next_id: u32 = 0;
        for old in 0..old_n {
            let alive = keep.get(usize_for(old)).is_some_and(|&b| b);
            if alive {
                remap[usize_for(old)] = next_id;
                if let Some(v) = self.node_ids.get(usize_for(old)) {
                    kept_ids.push(v.clone());
                }
                next_id += 1;
            }
        }
        let edges: Vec<(u32, u32)> = self
            .edges()
            .into_iter()
            .filter_map(|(u, v)| {
                let nu = *remap.get(usize_for(u))?;
                let nv = *remap.get(usize_for(v))?;
                (nu != u32::MAX && nv != u32::MAX).then_some((nu, nv))
            })
            .collect();
        let mut next = Self::from_csr(name, 1, CsrGraph::from_edges(next_id, &edges));
        if kept_ids.len() == usize_for(next_id) && next_id > 0 {
            next.set_ids(kept_ids);
        }
        next
    }

    /// Node-induced subgraph from a per-ordinal predicate -- the ergonomic
    /// analogue of Neo4j's `nodeFilter` projection, but durable.
    #[must_use]
    pub fn subgraph_where<P>(&self, name: impl Into<String>, mut keep: P) -> Self
    where
        P: FnMut(u32) -> bool,
    {
        let mask: Vec<bool> = (0..self.csr.node_count()).map(&mut keep).collect();
        self.subgraph(name, &mask)
    }

    /// Subgraph keeping only nodes whose **staged f64 property** `key`
    /// satisfies `pred`. Closes the durable pipeline loop: an algorithm's
    /// `.mutate` result can directly drive a topology filter -- e.g.
    /// `set_node_property("pr", …)` then keep `pr >= t` -- all rebuild-free
    /// and surviving a restart. Nodes without the property are dropped.
    #[must_use]
    pub fn subgraph_by_property<P>(&self, name: impl Into<String>, key: &str, mut pred: P) -> Self
    where
        P: FnMut(f64) -> bool,
    {
        let prop = self.node_props.get(key);
        let mask: Vec<bool> = (0..self.csr.node_count())
            .map(|o| {
                prop.and_then(|v| v.get(usize_for(o)).copied())
                    .is_some_and(&mut pred)
            })
            .collect();
        self.subgraph(name, &mask)
    }

    /// Label-scoped subgraph: keep only nodes whose staged **integer label
    /// column** `label_key` holds one of `allowed` (Neo4j multi-label node
    /// projection / label filter, but durable and rebuild-free). Nodes
    /// without the label column are dropped.
    #[must_use]
    pub fn subgraph_by_label(
        &self,
        name: impl Into<String>,
        label_key: &str,
        allowed: &[i64],
    ) -> Self {
        let set: std::collections::BTreeSet<i64> = allowed.iter().copied().collect();
        let col = self.node_props_i64.get(label_key);
        let mask: Vec<bool> = (0..self.csr.node_count())
            .map(|o| {
                col.and_then(|v| v.get(usize_for(o)).copied())
                    .is_some_and(|lbl| set.contains(&lbl))
            })
            .collect();
        self.subgraph(name, &mask)
    }

    /// Project an **edge-induced** view: every node is kept, but an edge
    /// survives only if `pred(src, dst)` holds. Node identities and node
    /// count are unchanged; a fresh generation marks it a new named graph.
    pub fn filter_edges<P>(&self, name: impl Into<String>, mut pred: P) -> Self
    where
        P: FnMut(u32, u32) -> bool,
    {
        let edges: Vec<(u32, u32)> = self
            .edges()
            .into_iter()
            .filter(|&(u, v)| pred(u, v))
            .collect();
        let mut next = Self::from_csr(name, 1, CsrGraph::from_edges(self.csr.node_count(), &edges));
        next.node_ids = self.node_ids.clone();
        next.key_index = self.key_index.clone();
        next
    }

    /// Project an **undirected** view (Neo4j GDS `ORIENTATION: UNDIRECTED`):
    /// every directed edge `(u, v)` is symmetrised to both `(u, v)` and
    /// `(v, u)`, duplicates collapsed, self-loops kept once. Required for
    /// correct Louvain / triangle-count / connected-components / similarity;
    /// fresh generation, id mapping carried, durable.
    #[must_use]
    pub fn as_undirected(&self, name: impl Into<String>) -> Self {
        let mut sym: std::collections::BTreeSet<(u32, u32)> = std::collections::BTreeSet::new();
        for (u, v) in self.edges() {
            sym.insert((u, v));
            sym.insert((v, u));
        }
        let edges: Vec<(u32, u32)> = sym.into_iter().collect();
        let mut next = Self::from_csr(name, 1, CsrGraph::from_edges(self.csr.node_count(), &edges));
        next.node_ids = self.node_ids.clone();
        next.key_index = self.key_index.clone();
        next
    }

    /// Durable **largest weakly-connected-component** projection: a
    /// self-contained union-find over the (undirected) edge set picks the
    /// biggest component (ties broken by lowest member ordinal for
    /// determinism) and emits it as a compact relabelled named graph. Common
    /// preprocessing that Neo4j needs a WCC run + Cypher filter for; here a
    /// first-class durable op with no external algorithm dependency.
    #[must_use]
    pub fn largest_component(&self, name: impl Into<String>) -> Self {
        let n = usize_for(self.csr.node_count());
        if n == 0 {
            return Self::from_csr(name, 1, CsrGraph::from_edges(0, &[]));
        }
        let mut parent: Vec<u32> = (0..self.csr.node_count()).collect();
        fn find(parent: &mut [u32], mut x: u32) -> u32 {
            while parent[usize_for(x)] != x {
                parent[usize_for(x)] = parent[usize_for(parent[usize_for(x)])];
                x = parent[usize_for(x)];
            }
            x
        }
        for (u, v) in self.edges() {
            let (ru, rv) = (find(&mut parent, u), find(&mut parent, v));
            if ru != rv {
                // attach larger root under smaller -> deterministic roots.
                if ru < rv {
                    parent[usize_for(rv)] = ru;
                } else {
                    parent[usize_for(ru)] = rv;
                }
            }
        }
        let mut size: std::collections::BTreeMap<u32, u32> = std::collections::BTreeMap::new();
        let mut root_of = vec![0u32; n];
        for node in 0..self.csr.node_count() {
            let r = find(&mut parent, node);
            root_of[usize_for(node)] = r;
            *size.entry(r).or_insert(0) += 1;
        }
        // pick max size; tie -> smallest root (BTreeMap iterates ascending).
        let mut best_root = 0u32;
        let mut best_size = 0u32;
        for (&r, &s) in &size {
            if s > best_size {
                best_size = s;
                best_root = r;
            }
        }
        let keep: Vec<bool> = root_of.iter().map(|&r| r == best_root).collect();
        self.subgraph(name, &keep)
    }

    /// Project the **transpose** (Neo4j GDS `ORIENTATION: REVERSE`): every
    /// edge `(u, v)` becomes `(v, u)`. Materialised, durable named graph for
    /// reverse-reachability / reverse personalized PageRank, fresh
    /// generation, id mapping carried.
    #[must_use]
    pub fn as_reversed(&self, name: impl Into<String>) -> Self {
        let edges: Vec<(u32, u32)> = self.edges().into_iter().map(|(u, v)| (v, u)).collect();
        let mut next = Self::from_csr(name, 1, CsrGraph::from_edges(self.csr.node_count(), &edges));
        next.node_ids = self.node_ids.clone();
        next.key_index = self.key_index.clone();
        next
    }

    /// Drop every self-loop `(v, v)` (Neo4j GDS can exclude self-loops at
    /// projection time; PageRank / triangle-count assume none). Fresh
    /// generation, id mapping carried, durable.
    #[must_use]
    pub fn without_self_loops(&self, name: impl Into<String>) -> Self {
        let edges: Vec<(u32, u32)> = self.edges().into_iter().filter(|&(u, v)| u != v).collect();
        let mut next = Self::from_csr(name, 1, CsrGraph::from_edges(self.csr.node_count(), &edges));
        next.node_ids = self.node_ids.clone();
        next.key_index = self.key_index.clone();
        next
    }

    /// Collapse a directed multigraph into a **simple** directed graph
    /// (parallel `(u, v)` duplicates removed; the unweighted analogue of
    /// [`PersistentWeightedProjection::aggregate_parallel_edges`]). Fresh
    /// generation, id mapping carried, durable.
    #[must_use]
    pub fn as_simple(&self, name: impl Into<String>) -> Self {
        let uniq: std::collections::BTreeSet<(u32, u32)> = self.edges().into_iter().collect();
        let edges: Vec<(u32, u32)> = uniq.into_iter().collect();
        let mut next = Self::from_csr(name, 1, CsrGraph::from_edges(self.csr.node_count(), &edges));
        next.node_ids = self.node_ids.clone();
        next.key_index = self.key_index.clone();
        next
    }

    /// Out-degree distribution summary (Neo4j `gds.graph.list` shape).
    #[must_use]
    pub fn degree_summary(&self) -> crate::DegreeSummary {
        crate::degree_summary(&self.csr)
    }

    /// Estimated resident bytes of the compact CSR topology (planner sizing,
    /// Neo4j `gds.graph.project.estimate`).
    #[must_use]
    pub fn estimated_topology_bytes(&self) -> u64 {
        crate::estimate_csr_bytes(self.csr.node_count(), self.csr.edge_count())
    }

    /// Durable **ego-network** projection: the node-induced subgraph of every
    /// node within `radius` out-hops of `center` (BFS-bounded), compactly
    /// relabelled with `center` mapped to ordinal 0 and the id mapping
    /// carried. `radius == 0` yields just the center. Out-of-range center
    /// gives an empty projection. Neo4j needs a Cypher subgraph projection
    /// for this; here it is a first-class durable named graph.
    #[must_use]
    pub fn ego_subgraph(&self, name: impl Into<String>, center: u32, radius: u32) -> Self {
        let n = self.csr.node_count();
        if usize_for(center) >= usize_for(n) {
            return Self::from_csr(name, 1, CsrGraph::from_edges(0, &[]));
        }
        let mut keep = vec![false; usize_for(n)];
        let mut frontier = vec![center];
        keep[usize_for(center)] = true;
        for _ in 0..radius {
            let mut nxt = Vec::new();
            for &node in &frontier {
                if let Some(adj) = GraphViewV2::neighbor_slice(&self.csr, node) {
                    for &v in adj {
                        if !keep[usize_for(v)] {
                            keep[usize_for(v)] = true;
                            nxt.push(v);
                        }
                    }
                }
            }
            if nxt.is_empty() {
                break;
            }
            frontier = nxt;
        }
        // subgraph() relabels survivors in ascending old-ordinal order, so
        // `center` (lowest-or-not) may not be 0; build the remap so center
        // is ordinal 0, then the rest in order.
        self.ego_from_mask(name, center, &keep)
    }

    fn ego_from_mask(&self, name: impl Into<String>, center: u32, keep: &[bool]) -> Self {
        let old_n = self.csr.node_count();
        let mut remap = vec![u32::MAX; usize_for(old_n)];
        let mut kept_ids: Vec<Value> = Vec::new();
        remap[usize_for(center)] = 0;
        if let Some(v) = self.node_ids.get(usize_for(center)) {
            kept_ids.push(v.clone());
        }
        let mut next_id: u32 = 1;
        for old in 0..old_n {
            if old == center || !keep.get(usize_for(old)).is_some_and(|&b| b) {
                continue;
            }
            remap[usize_for(old)] = next_id;
            if let Some(v) = self.node_ids.get(usize_for(old)) {
                kept_ids.push(v.clone());
            }
            next_id += 1;
        }
        let edges: Vec<(u32, u32)> = self
            .edges()
            .into_iter()
            .filter_map(|(u, v)| {
                let nu = *remap.get(usize_for(u))?;
                let nv = *remap.get(usize_for(v))?;
                (nu != u32::MAX && nv != u32::MAX).then_some((nu, nv))
            })
            .collect();
        let mut next = Self::from_csr(name, 1, CsrGraph::from_edges(next_id, &edges));
        if kept_ids.len() == usize_for(next_id) && next_id > 0 {
            next.set_ids(kept_ids);
        }
        next
    }

    /// Validate the projection's internal invariants -- the durable-catalog
    /// integrity check Neo4j has no equivalent of (its catalog is RAM-only).
    /// Checks: id list is either empty or `node_count`-aligned; every
    /// id-index ordinal is in range; each staged property column matches
    /// `node_count`.
    pub fn verify(&self) -> DbResult<()> {
        let n = usize_for(self.csr.node_count());
        if !self.node_ids.is_empty() && self.node_ids.len() != n {
            return Err(DbError::internal(format!(
                "projection '{}': node_ids len {} != node_count {n}",
                self.name,
                self.node_ids.len()
            )));
        }
        for &ord in self.key_index.values() {
            if usize_for(ord) >= n {
                return Err(DbError::internal(format!(
                    "projection '{}': id-index ordinal {ord} out of range {n}",
                    self.name
                )));
            }
        }
        for (key, col) in &self.node_props {
            if col.len() != n {
                return Err(DbError::internal(format!(
                    "projection '{}': f64 property '{key}' len {} != {n}",
                    self.name,
                    col.len()
                )));
            }
        }
        for (key, col) in &self.node_props_i64 {
            if col.len() != n {
                return Err(DbError::internal(format!(
                    "projection '{}': i64 property '{key}' len {} != {n}",
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

    /// Stage an algorithm result as a per-node property (the durable analogue
    /// of Neo4j GDS `algo.mutate`). `values` must be ordinal-aligned, i.e.
    /// `values.len() == node_count`; a later algorithm can then consume it
    /// straight off the persisted projection with **no DB round-trip and no
    /// rebuild**, and -- unlike Neo4j -- it survives a restart.
    pub fn set_node_property(&mut self, key: impl Into<String>, values: Vec<f64>) -> DbResult<()> {
        let n = usize_for(self.csr.node_count());
        if values.len() != n {
            return Err(DbError::internal(format!(
                "node property length {} != node count {n}",
                values.len()
            )));
        }
        self.node_props.insert(key.into(), values);
        Ok(())
    }

    /// Borrow a staged per-node property by name.
    #[must_use]
    pub fn node_property(&self, key: &str) -> Option<&[f64]> {
        self.node_props.get(key).map(Vec::as_slice)
    }

    /// One node's staged property value.
    #[must_use]
    pub fn node_property_value(&self, key: &str, ordinal: u32) -> Option<f64> {
        self.node_props
            .get(key)
            .and_then(|v| v.get(usize_for(ordinal)).copied())
    }

    /// Sorted names of all staged node properties.
    #[must_use]
    pub fn property_keys(&self) -> Vec<&str> {
        let mut v: Vec<&str> = self.node_props.keys().map(String::as_str).collect();
        v.sort_unstable();
        v
    }

    /// Remove a staged property, returning it if present.
    pub fn drop_property(&mut self, key: &str) -> Option<Vec<f64>> {
        self.node_props.remove(key)
    }

    /// Stage an ordinal-aligned **integer** node property (e.g. a community
    /// seed for seeded Louvain/LPA, or a partition/label column). Persisted
    /// with the topology so the seed survives a restart.
    pub fn set_node_property_i64(
        &mut self,
        key: impl Into<String>,
        values: Vec<i64>,
    ) -> DbResult<()> {
        let n = usize_for(self.csr.node_count());
        if values.len() != n {
            return Err(DbError::internal(format!(
                "i64 node property length {} != node count {n}",
                values.len()
            )));
        }
        self.node_props_i64.insert(key.into(), values);
        Ok(())
    }

    /// Borrow a staged integer node property by name.
    #[must_use]
    pub fn node_property_i64(&self, key: &str) -> Option<&[i64]> {
        self.node_props_i64.get(key).map(Vec::as_slice)
    }

    /// One node's staged integer property value.
    #[must_use]
    pub fn node_property_i64_value(&self, key: &str, ordinal: u32) -> Option<i64> {
        self.node_props_i64
            .get(key)
            .and_then(|v| v.get(usize_for(ordinal)).copied())
    }

    /// Sorted names of all staged integer node properties.
    #[must_use]
    pub fn property_keys_i64(&self) -> Vec<&str> {
        let mut v: Vec<&str> = self.node_props_i64.keys().map(String::as_str).collect();
        v.sort_unstable();
        v
    }

    /// Remove a staged integer property, returning it if present.
    pub fn drop_property_i64(&mut self, key: &str) -> Option<Vec<i64>> {
        self.node_props_i64.remove(key)
    }
}

#[inline]
fn usize_for(v: u32) -> usize {
    usize::try_from(v).unwrap_or(usize::MAX)
}

impl GraphProjection for PersistentGraphProjection {
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
        &self.csr
    }

    fn node_ordinal(&self, node_id: &Value) -> Option<u32> {
        let key = encode_key(node_id).ok()?;
        self.key_index.get(&key).copied()
    }

    fn node_id(&self, ordinal: u32) -> Option<Value> {
        self.node_ids.get(usize::try_from(ordinal).ok()?).cloned()
    }
}

/// A durable, named catalog of [`PersistentGraphProjection`]s.
///
/// This is the piece that removes the per-query in-memory rebuild: a
/// projection is materialised **once** via [`Self::get_or_build`], cached by
/// name, and reused (or reloaded from a blob) on every subsequent algorithm
/// call. Unlike Neo4j's purely in-memory graph catalog, the whole store
/// serialises to a single blob and survives restarts.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct PersistentProjectionStore {
    projections: std::collections::HashMap<String, PersistentGraphProjection>,
    weighted: std::collections::HashMap<String, PersistentWeightedProjection>,
}

/// One row of the projection catalog -- the durable analogue of a
/// `gds.graph.list` record (name, shape, generation, on-disk size).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ProjectionCatalogEntry {
    pub name: String,
    pub weighted: bool,
    pub node_count: u64,
    pub edge_count: u64,
    pub generation: u64,
    /// Serialised footprint in bytes (0 if the topology failed to encode).
    pub encoded_bytes: usize,
}

/// A full `gds.graph.list` row: identity + on-disk size + degree
/// distribution + estimated resident topology bytes, in one durable record.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DetailedCatalogEntry {
    pub entry: ProjectionCatalogEntry,
    pub degrees: crate::DegreeSummary,
    /// Estimated resident bytes of the compact (weighted) CSR topology.
    pub estimated_topology_bytes: u64,
}

impl PersistentProjectionStore {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert or replace a projection.
    pub fn upsert(&mut self, projection: PersistentGraphProjection) {
        self.projections
            .insert(projection.projection_name().to_owned(), projection);
    }

    /// Borrow a cached projection by name.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&PersistentGraphProjection> {
        self.projections.get(name)
    }

    #[must_use]
    pub fn contains(&self, name: &str) -> bool {
        self.projections.contains_key(name)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.projections.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.projections.is_empty()
    }

    /// Return the cached projection for `name`, building (and caching) it once
    /// if absent. The closure -- the expensive "rebuild from the row store"
    /// step -- runs **at most once per name**; every later call is a cache
    /// hit, eliminating the per-query rebuild.
    pub fn get_or_build<F>(&mut self, name: &str, build: F) -> DbResult<&PersistentGraphProjection>
    where
        F: FnOnce() -> DbResult<PersistentGraphProjection>,
    {
        if !self.projections.contains_key(name) {
            let projection = build()?;
            self.projections.insert(name.to_owned(), projection);
        }
        self.projections
            .get(name)
            .ok_or_else(|| DbError::internal("projection vanished after insert".to_owned()))
    }

    /// Generation-aware variant: keep the cached projection only while its
    /// snapshot generation is at least `min_generation`; otherwise rebuild.
    ///
    /// This is a real projection *lifecycle*: callers pass the current
    /// source-of-truth generation, and the catalog transparently refreshes a
    /// stale named graph instead of serving outdated topology. Neo4j has no
    /// auto-refresh -- a stale projection must be manually dropped and
    /// recreated.
    pub fn get_or_build_fresh<F>(
        &mut self,
        name: &str,
        min_generation: u64,
        build: F,
    ) -> DbResult<&PersistentGraphProjection>
    where
        F: FnOnce() -> DbResult<PersistentGraphProjection>,
    {
        let stale = match self.projections.get(name) {
            Some(p) => p.snapshot().generation < min_generation,
            None => true,
        };
        if stale {
            self.projections.insert(name.to_owned(), build()?);
        }
        self.projections
            .get(name)
            .ok_or_else(|| DbError::internal("projection vanished after insert".to_owned()))
    }

    /// Insert or replace a weighted projection.
    pub fn upsert_weighted(&mut self, projection: PersistentWeightedProjection) {
        self.weighted
            .insert(projection.projection_name().to_owned(), projection);
    }

    /// Borrow a cached weighted projection by name.
    #[must_use]
    pub fn get_weighted(&self, name: &str) -> Option<&PersistentWeightedProjection> {
        self.weighted.get(name)
    }

    #[must_use]
    pub fn contains_weighted(&self, name: &str) -> bool {
        self.weighted.contains_key(name)
    }

    /// Build-once / cache the weighted projection `name` (rebuild eliminated).
    pub fn get_or_build_weighted<F>(
        &mut self,
        name: &str,
        build: F,
    ) -> DbResult<&PersistentWeightedProjection>
    where
        F: FnOnce() -> DbResult<PersistentWeightedProjection>,
    {
        if !self.weighted.contains_key(name) {
            self.weighted.insert(name.to_owned(), build()?);
        }
        self.weighted
            .get(name)
            .ok_or_else(|| DbError::internal("weighted projection vanished".to_owned()))
    }

    /// Generation-aware build/refresh for weighted projections.
    pub fn get_or_build_weighted_fresh<F>(
        &mut self,
        name: &str,
        min_generation: u64,
        build: F,
    ) -> DbResult<&PersistentWeightedProjection>
    where
        F: FnOnce() -> DbResult<PersistentWeightedProjection>,
    {
        let stale = match self.weighted.get(name) {
            Some(p) => p.snapshot().generation < min_generation,
            None => true,
        };
        if stale {
            self.weighted.insert(name.to_owned(), build()?);
        }
        self.weighted
            .get(name)
            .ok_or_else(|| DbError::internal("weighted projection vanished".to_owned()))
    }

    /// Serialise the entire store (all topologies + id maps) to one blob.
    pub fn to_bytes(&self) -> DbResult<Vec<u8>> {
        bincode::serialize(self)
            .map_err(|e| DbError::internal(format!("projection store encode failed: {e}")))
    }

    /// Reload a store previously produced by [`Self::to_bytes`].
    pub fn from_bytes(bytes: &[u8]) -> DbResult<Self> {
        bincode::deserialize(bytes)
            .map_err(|e| DbError::internal(format!("projection store decode failed: {e}")))
    }

    /// Export **one** cached named graph to its own portable blob (unweighted
    /// preferred, else weighted). Neo4j has no per-graph durable export -- a
    /// catalog graph cannot be saved/shipped independently.
    pub fn export(&self, name: &str) -> DbResult<Vec<u8>> {
        if let Some(p) = self.projections.get(name) {
            p.to_bytes()
        } else if let Some(p) = self.weighted.get(name) {
            p.to_bytes()
        } else {
            Err(DbError::internal(format!(
                "no projection '{name}' to export"
            )))
        }
    }

    /// Import a blob produced by [`PersistentGraphProjection::to_bytes`] as a
    /// cached unweighted named graph (its embedded name is used).
    pub fn import(&mut self, bytes: &[u8]) -> DbResult<&PersistentGraphProjection> {
        let p = PersistentGraphProjection::from_bytes(bytes)?;
        let name = p.projection_name().to_owned();
        self.projections.insert(name.clone(), p);
        self.projections
            .get(&name)
            .ok_or_else(|| DbError::internal("projection vanished after import".to_owned()))
    }

    /// Import a blob produced by [`PersistentWeightedProjection::to_bytes`]
    /// as a cached weighted named graph.
    pub fn import_weighted(&mut self, bytes: &[u8]) -> DbResult<&PersistentWeightedProjection> {
        let p = PersistentWeightedProjection::from_bytes(bytes)?;
        let name = p.projection_name().to_owned();
        self.weighted.insert(name.clone(), p);
        self.weighted
            .get(&name)
            .ok_or_else(|| DbError::internal("weighted projection vanished".to_owned()))
    }

    /// Drop a named unweighted projection; returns it if it existed
    /// (`gds.graph.drop`, but here it also frees durable storage).
    pub fn drop(&mut self, name: &str) -> Option<PersistentGraphProjection> {
        self.projections.remove(name)
    }

    /// Drop a named weighted projection.
    pub fn drop_weighted(&mut self, name: &str) -> Option<PersistentWeightedProjection> {
        self.weighted.remove(name)
    }

    /// Evict every projection (unweighted + weighted).
    pub fn clear(&mut self) {
        self.projections.clear();
        self.weighted.clear();
    }

    /// Sorted names of cached unweighted projections (stable ordering).
    #[must_use]
    pub fn names(&self) -> Vec<&str> {
        let mut v: Vec<&str> = self.projections.keys().map(String::as_str).collect();
        v.sort_unstable();
        v
    }

    /// Sorted names of cached weighted projections.
    #[must_use]
    pub fn weighted_names(&self) -> Vec<&str> {
        let mut v: Vec<&str> = self.weighted.keys().map(String::as_str).collect();
        v.sort_unstable();
        v
    }

    /// Full catalog listing across both maps, sorted by name then by the
    /// weighted flag -- the durable, deterministic equivalent of
    /// `gds.graph.list`. Reports node/edge counts, generation and the exact
    /// serialised footprint of every named graph.
    #[must_use]
    pub fn catalog(&self) -> Vec<ProjectionCatalogEntry> {
        let mut out = Vec::with_capacity(self.projections.len() + self.weighted.len());
        for (name, p) in &self.projections {
            let view = p.view();
            out.push(ProjectionCatalogEntry {
                name: name.clone(),
                weighted: false,
                node_count: u64::from(view.node_count()),
                edge_count: view.edge_count(),
                generation: p.snapshot().generation,
                encoded_bytes: p.to_bytes().map_or(0, |b| b.len()),
            });
        }
        for (name, p) in &self.weighted {
            let g = p.weighted();
            out.push(ProjectionCatalogEntry {
                name: name.clone(),
                weighted: true,
                node_count: u64::from(g.node_count()),
                edge_count: g.edge_count(),
                generation: p.snapshot().generation,
                encoded_bytes: p.to_bytes().map_or(0, |b| b.len()),
            });
        }
        out.sort_unstable_by(|a, b| a.name.cmp(&b.name).then(a.weighted.cmp(&b.weighted)));
        out
    }

    /// Total serialised footprint of the whole catalog in bytes.
    #[must_use]
    pub fn total_encoded_bytes(&self) -> usize {
        self.catalog().iter().map(|e| e.encoded_bytes).sum()
    }

    /// Full detailed catalog: every named graph with its degree distribution
    /// and estimated resident topology size folded in -- the complete
    /// `gds.graph.list` row, durable and deterministic (sorted by name then
    /// weighted flag).
    #[must_use]
    pub fn catalog_detailed(&self) -> Vec<DetailedCatalogEntry> {
        let mut out = Vec::with_capacity(self.projections.len() + self.weighted.len());
        for (name, p) in &self.projections {
            out.push(DetailedCatalogEntry {
                entry: ProjectionCatalogEntry {
                    name: name.clone(),
                    weighted: false,
                    node_count: u64::from(p.view().node_count()),
                    edge_count: p.view().edge_count(),
                    generation: p.snapshot().generation,
                    encoded_bytes: p.to_bytes().map_or(0, |b| b.len()),
                },
                degrees: p.degree_summary(),
                estimated_topology_bytes: p.estimated_topology_bytes(),
            });
        }
        for (name, p) in &self.weighted {
            out.push(DetailedCatalogEntry {
                entry: ProjectionCatalogEntry {
                    name: name.clone(),
                    weighted: true,
                    node_count: u64::from(p.weighted().node_count()),
                    edge_count: p.weighted().edge_count(),
                    generation: p.snapshot().generation,
                    encoded_bytes: p.to_bytes().map_or(0, |b| b.len()),
                },
                degrees: p.degree_summary(),
                estimated_topology_bytes: p.estimated_topology_bytes(),
            });
        }
        out.sort_unstable_by(|a, b| {
            a.entry
                .name
                .cmp(&b.entry.name)
                .then(a.entry.weighted.cmp(&b.entry.weighted))
        });
        out
    }

    /// Verify every cached projection's invariants; returns the first
    /// violation. A durable catalog can be integrity-checked after a reload
    /// -- Neo4j's RAM-only catalog cannot.
    pub fn verify(&self) -> DbResult<()> {
        for p in self.projections.values() {
            p.verify()?;
        }
        for p in self.weighted.values() {
            p.verify()?;
        }
        Ok(())
    }

    /// Incrementally fold edges **into a cached named graph in place**: the
    /// catalog itself is the mutation point. The projection is taken out,
    /// delta-rebuilt (generation bumped, no row-store scan), and re-cached
    /// under the same name. Neo4j cannot mutate a catalog graph -- it must be
    /// dropped and fully re-projected.
    pub fn apply_added_edges(
        &mut self,
        name: &str,
        extra: &[(u32, u32)],
    ) -> DbResult<&PersistentGraphProjection> {
        let cur = self
            .projections
            .remove(name)
            .ok_or_else(|| DbError::internal(format!("no projection '{name}'")))?;
        self.projections
            .insert(name.to_owned(), cur.with_added_edges(extra));
        self.projections
            .get(name)
            .ok_or_else(|| DbError::internal("projection vanished after insert".to_owned()))
    }

    /// Incrementally remove edges from a cached named graph in place.
    pub fn apply_removed_edges(
        &mut self,
        name: &str,
        drop: &[(u32, u32)],
    ) -> DbResult<&PersistentGraphProjection> {
        let cur = self
            .projections
            .remove(name)
            .ok_or_else(|| DbError::internal(format!("no projection '{name}'")))?;
        self.projections
            .insert(name.to_owned(), cur.with_removed_edges(drop));
        self.projections
            .get(name)
            .ok_or_else(|| DbError::internal("projection vanished after insert".to_owned()))
    }

    /// In-place incremental add for a cached **weighted** named graph.
    pub fn apply_added_edges_weighted(
        &mut self,
        name: &str,
        extra: &[(u32, u32, f64)],
    ) -> DbResult<&PersistentWeightedProjection> {
        let cur = self
            .weighted
            .remove(name)
            .ok_or_else(|| DbError::internal(format!("no weighted projection '{name}'")))?;
        self.weighted
            .insert(name.to_owned(), cur.with_added_edges(extra));
        self.weighted
            .get(name)
            .ok_or_else(|| DbError::internal("weighted projection vanished".to_owned()))
    }

    /// In-place incremental edge removal for a cached weighted named graph.
    pub fn apply_removed_edges_weighted(
        &mut self,
        name: &str,
        drop: &[(u32, u32)],
    ) -> DbResult<&PersistentWeightedProjection> {
        let cur = self
            .weighted
            .remove(name)
            .ok_or_else(|| DbError::internal(format!("no weighted projection '{name}'")))?;
        self.weighted
            .insert(name.to_owned(), cur.with_removed_edges(drop));
        self.weighted
            .get(name)
            .ok_or_else(|| DbError::internal("weighted projection vanished".to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EDGES: &[(u32, u32)] = &[(0, 1), (1, 2), (2, 0), (2, 3), (3, 1)];

    #[test]
    fn exposes_compact_view_without_rebuild() {
        let p = PersistentGraphProjection::from_edges("g", 1, 4, EDGES);
        let v = p.graph_view();
        assert_eq!(v.node_count(), 4);
        assert_eq!(v.edge_count(), 5);
        // Zero-copy CSR slices are available directly.
        assert_eq!(v.neighbor_slice(0), Some(&[1u32][..]));
        assert_eq!(v.neighbor_slice(2), Some(&[0u32, 3][..]));
        assert!(v.has_reverse_adjacency());
        assert_eq!(p.projection_name(), "g");
        assert_eq!(p.stats().node_count, Some(4));
        assert_eq!(p.snapshot().generation, 1);
    }

    #[test]
    fn binary_round_trip_preserves_topology() {
        let p = PersistentGraphProjection::from_edges("g", 7, 4, EDGES);
        let bytes = p.to_bytes().expect("encode");
        let restored = PersistentGraphProjection::from_bytes(&bytes).expect("decode");
        assert_eq!(restored.projection_name(), "g");
        assert_eq!(restored.snapshot().generation, 7);
        assert_eq!(restored.graph_view().node_count(), 4);
        for u in 0..4 {
            assert_eq!(
                restored.graph_view().neighbor_slice(u),
                p.graph_view().neighbor_slice(u)
            );
        }
    }

    #[test]
    fn algorithms_run_directly_on_the_projection() {
        // The whole point: an algorithm consumes the projection's view with
        // no executor-side rebuild, and a persisted-then-reloaded projection
        // yields identical results to a freshly built CSR.
        let p = PersistentGraphProjection::from_edges("g", 1, 4, EDGES);
        let reloaded = PersistentGraphProjection::from_bytes(&p.to_bytes().unwrap()).unwrap();
        let reference = CsrGraph::from_edges(4, EDGES);

        let on_projection = aiondb_graph::algorithms::pagerank::pagerank_default(p.graph_view());
        let on_reloaded =
            aiondb_graph::algorithms::pagerank::pagerank_default(reloaded.graph_view());
        let on_fresh_csr = aiondb_graph::algorithms::pagerank::pagerank_default(&reference);

        assert_eq!(on_projection.len(), 4);
        for ((a, b), c) in on_projection
            .iter()
            .zip(on_reloaded.iter())
            .zip(on_fresh_csr.iter())
        {
            assert!((a - b).abs() < 1e-12, "projection vs reloaded");
            assert!((a - c).abs() < 1e-12, "projection vs fresh CSR");
        }
    }

    #[test]
    fn empty_projection_is_valid() {
        let p = PersistentGraphProjection::from_edges("empty", 0, 0, &[]);
        assert_eq!(p.graph_view().node_count(), 0);
        let bytes = p.to_bytes().unwrap();
        assert_eq!(
            PersistentGraphProjection::from_bytes(&bytes)
                .unwrap()
                .graph_view()
                .node_count(),
            0
        );
    }
    #[test]
    fn node_id_mapping_round_trips() {
        let ids = vec![
            Value::BigInt(100),
            Value::BigInt(200),
            Value::Text("c".to_owned()),
            Value::BigInt(400),
        ];
        let p = PersistentGraphProjection::from_edges_with_ids("g", 3, ids.clone(), EDGES)
            .expect("build with ids");
        // ordinal <-> external id both directions.
        for (ord, v) in ids.iter().enumerate() {
            let o = u32::try_from(ord).unwrap();
            assert_eq!(p.node_id(o), Some(v.clone()));
            assert_eq!(p.node_ordinal(v), Some(o));
        }
        assert_eq!(p.node_ordinal(&Value::BigInt(999)), None);
        assert_eq!(p.node_id(99), None);

        // Mapping survives binary persistence.
        let reloaded = PersistentGraphProjection::from_bytes(&p.to_bytes().unwrap()).unwrap();
        assert_eq!(reloaded.node_ordinal(&Value::Text("c".to_owned())), Some(2));
        assert_eq!(reloaded.node_id(3), Some(Value::BigInt(400)));
        // Topology still intact alongside the mapping.
        assert_eq!(
            reloaded.graph_view().neighbor_slice(2),
            Some(&[0u32, 3][..])
        );
    }

    #[test]
    fn ordinal_only_projection_has_no_id_mapping() {
        let p = PersistentGraphProjection::from_edges("g", 1, 4, EDGES);
        assert_eq!(p.node_ordinal(&Value::BigInt(0)), None);
        assert_eq!(p.node_id(0), None);
    }

    #[test]
    fn store_builds_once_then_serves_cache() {
        let mut store = PersistentProjectionStore::new();
        assert!(store.is_empty());
        let mut builds = 0;
        for _ in 0..5 {
            let p = store
                .get_or_build("social", || {
                    builds += 1;
                    Ok(PersistentGraphProjection::from_edges("social", 1, 4, EDGES))
                })
                .expect("get_or_build");
            assert_eq!(p.graph_view().node_count(), 4);
        }
        // The expensive rebuild ran exactly once across 5 queries.
        assert_eq!(builds, 1);
        assert_eq!(store.len(), 1);
        assert!(store.contains("social"));
        assert!(store.get("missing").is_none());
    }

    #[test]
    fn store_persists_all_projections() {
        let mut store = PersistentProjectionStore::new();
        store.upsert(PersistentGraphProjection::from_edges("a", 1, 4, EDGES));
        store.upsert(
            PersistentGraphProjection::from_edges_with_ids(
                "b",
                2,
                vec![
                    Value::BigInt(10),
                    Value::BigInt(20),
                    Value::BigInt(30),
                    Value::BigInt(40),
                ],
                EDGES,
            )
            .unwrap(),
        );
        let reloaded = PersistentProjectionStore::from_bytes(&store.to_bytes().unwrap()).unwrap();
        assert_eq!(reloaded.len(), 2);
        assert_eq!(
            reloaded.get("a").unwrap().graph_view().neighbor_slice(2),
            Some(&[0u32, 3][..])
        );
        // id mapping survives the whole-store round-trip too.
        assert_eq!(
            reloaded.get("b").unwrap().node_ordinal(&Value::BigInt(30)),
            Some(2)
        );
    }

    #[test]
    fn incremental_update_evolves_topology_and_generation() {
        let p = PersistentGraphProjection::from_edges_with_ids(
            "g",
            5,
            vec![
                Value::BigInt(0),
                Value::BigInt(1),
                Value::BigInt(2),
                Value::BigInt(3),
            ],
            EDGES,
        )
        .unwrap();
        // Add an edge 0 -> 3 (new), keep node count.
        let p2 = p.with_added_edges(&[(0, 3)]);
        assert_eq!(p2.snapshot().generation, 6); // bumped
        let n0 = p2.graph_view().neighbor_slice(0).unwrap();
        assert!(n0.contains(&1) && n0.contains(&3));
        assert_eq!(p2.graph_view().node_count(), 4);
        // Id mapping carried over.
        assert_eq!(p2.node_ordinal(&Value::BigInt(2)), Some(2));
        // Original projection is untouched (immutable update).
        assert_eq!(p.graph_view().neighbor_slice(0), Some(&[1u32][..]));
        // Still persistable after the update.
        let reloaded = PersistentGraphProjection::from_bytes(&p2.to_bytes().unwrap()).unwrap();
        assert!(reloaded
            .graph_view()
            .neighbor_slice(0)
            .unwrap()
            .contains(&3));
    }

    #[test]
    fn incremental_update_can_grow_node_count() {
        let p = PersistentGraphProjection::from_edges("g", 1, 3, &[(0, 1), (1, 2)]);
        let p2 = p.with_added_edges(&[(2, 5)]); // references node 5
        assert_eq!(p2.graph_view().node_count(), 6);
        assert!(p2.graph_view().neighbor_slice(2).unwrap().contains(&5));
        assert_eq!(p2.snapshot().generation, 2);
    }

    #[test]
    fn edges_recovers_the_full_edge_set() {
        let p = PersistentGraphProjection::from_edges("g", 1, 4, EDGES);
        let mut got = p.edges();
        got.sort_unstable();
        let mut want = EDGES.to_vec();
        want.sort_unstable();
        assert_eq!(got, want);
    }

    #[test]
    fn stale_projection_rebuilds_only_when_source_advances() {
        let mut store = PersistentProjectionStore::new();
        let mut builds = 0;
        // generation 1 cached.
        store
            .get_or_build_fresh("g", 1, || {
                builds += 1;
                Ok(PersistentGraphProjection::from_edges("g", 1, 4, EDGES))
            })
            .unwrap();
        // still fresh enough -> cache hit, no rebuild.
        store
            .get_or_build_fresh("g", 1, || {
                builds += 1;
                Ok(PersistentGraphProjection::from_edges("g", 1, 4, EDGES))
            })
            .unwrap();
        assert_eq!(builds, 1);
        // source advanced past cached generation -> rebuild + refresh.
        let p = store
            .get_or_build_fresh("g", 2, || {
                builds += 1;
                Ok(PersistentGraphProjection::from_edges("g", 2, 5, EDGES))
            })
            .unwrap();
        assert_eq!(builds, 2);
        assert_eq!(p.snapshot().generation, 2);
        assert_eq!(p.graph_view().node_count(), 5);
    }

    #[test]
    fn store_holds_weighted_projections_build_once_and_persist() {
        let wedges = &[(0u32, 1u32, 2.0f64), (1, 2, 3.0), (2, 0, 1.0)];
        let mut store = PersistentProjectionStore::new();
        let mut builds = 0;
        for _ in 0..3 {
            store
                .get_or_build_weighted("w", || {
                    builds += 1;
                    Ok(PersistentWeightedProjection::from_edges("w", 1, 3, wedges))
                })
                .unwrap();
        }
        assert_eq!(builds, 1);
        assert!(store.contains_weighted("w"));
        assert_eq!(store.get_weighted("w").unwrap().weighted().node_count(), 3);

        // Whole store (unweighted + weighted) round-trips through one blob.
        store.upsert(PersistentGraphProjection::from_edges("u", 1, 4, EDGES));
        let back = PersistentProjectionStore::from_bytes(&store.to_bytes().unwrap()).unwrap();
        assert!(back.contains("u"));
        let w = back.get_weighted("w").unwrap();
        let d = aiondb_graph::algorithms::bellman_ford::bellman_ford(w.weighted(), 0);
        let fresh = aiondb_graph::algorithms::WeightedCsrGraph::from_edges(3, wedges);
        assert_eq!(
            d.distances,
            aiondb_graph::algorithms::bellman_ford::bellman_ford(&fresh, 0).distances
        );
    }

    #[test]
    fn weighted_staleness_refreshes_on_advance() {
        let base = &[(0u32, 1u32, 1.0f64)];
        let mut store = PersistentProjectionStore::new();
        store
            .get_or_build_weighted_fresh("w", 1, || {
                Ok(PersistentWeightedProjection::from_edges("w", 1, 2, base))
            })
            .unwrap();
        let p = store
            .get_or_build_weighted_fresh("w", 3, || {
                Ok(PersistentWeightedProjection::from_edges(
                    "w",
                    3,
                    2,
                    &[(0, 1, 9.0)],
                ))
            })
            .unwrap();
        assert_eq!(p.snapshot().generation, 3);
        assert!((p.weighted().neighbors(0)[0].weight - 9.0).abs() < 1e-12);
    }

    #[test]
    fn catalog_lists_drops_and_clears_both_maps() {
        let mut store = PersistentProjectionStore::new();
        store.upsert(PersistentGraphProjection::from_edges("b", 2, 4, EDGES));
        store.upsert(PersistentGraphProjection::from_edges("a", 1, 3, &[(0, 1)]));
        store.upsert_weighted(PersistentWeightedProjection::from_edges(
            "a",
            7,
            2,
            &[(0, 1, 1.5)],
        ));

        assert_eq!(store.names(), vec!["a", "b"]);
        assert_eq!(store.weighted_names(), vec!["a"]);

        let cat = store.catalog();
        assert_eq!(cat.len(), 3);
        // sorted by name, then unweighted before weighted within a name.
        assert_eq!((cat[0].name.as_str(), cat[0].weighted), ("a", false));
        assert_eq!((cat[1].name.as_str(), cat[1].weighted), ("a", true));
        assert_eq!((cat[2].name.as_str(), cat[2].weighted), ("b", false));
        assert_eq!(cat[2].node_count, 4);
        assert_eq!(cat[2].generation, 2);
        assert!(cat.iter().all(|e| e.encoded_bytes > 0));
        assert_eq!(
            store.total_encoded_bytes(),
            cat.iter().map(|e| e.encoded_bytes).sum::<usize>()
        );

        // drop returns the removed projection and shrinks the catalog.
        assert!(store.drop("b").is_some());
        assert!(store.drop("b").is_none());
        assert!(store.drop_weighted("a").is_some());
        assert_eq!(store.catalog().len(), 1);

        store.clear();
        assert!(store.catalog().is_empty());
        assert!(store.is_empty());
    }

    #[test]
    fn node_induced_subgraph_remaps_topology_and_ids() {
        let p = PersistentGraphProjection::from_edges_with_ids(
            "g",
            9,
            vec![
                Value::BigInt(10),
                Value::BigInt(11),
                Value::BigInt(12),
                Value::BigInt(13),
            ],
            EDGES,
        )
        .unwrap();
        // keep ordinals 0,1,2 ; drop 3.
        let sub = p.subgraph("g_sub", &[true, true, true, false]);
        assert_eq!(sub.projection_name(), "g_sub");
        assert_eq!(sub.snapshot().generation, 1); // fresh named graph
        assert_eq!(sub.view().node_count(), 3);
        // only edges fully inside {0,1,2} survive: (0,1)(1,2)(2,0).
        let mut e = sub.edges();
        e.sort_unstable();
        assert_eq!(e, vec![(0, 1), (1, 2), (2, 0)]);
        // id mapping carried for survivors, remapped to new ordinals.
        assert_eq!(sub.node_id(2), Some(Value::BigInt(12)));
        assert_eq!(sub.node_ordinal(&Value::BigInt(11)), Some(1));
        assert_eq!(sub.node_ordinal(&Value::BigInt(13)), None);
        // durable: survives a blob round-trip.
        let back = PersistentGraphProjection::from_bytes(&sub.to_bytes().unwrap()).unwrap();
        assert_eq!(back.node_ordinal(&Value::BigInt(10)), Some(0));
        assert_eq!(back.view().edge_count(), 3);
    }

    #[test]
    fn empty_subgraph_when_nothing_kept() {
        let p = PersistentGraphProjection::from_edges("g", 1, 4, EDGES);
        let sub = p.subgraph("none", &[false, false, false, false]);
        assert_eq!(sub.view().node_count(), 0);
        assert!(sub.edges().is_empty());
    }

    #[test]
    fn edge_induced_filter_keeps_all_nodes() {
        let p = PersistentGraphProjection::from_edges("g", 4, 4, EDGES);
        // keep only forward edges u < v.
        let f = p.filter_edges("g_fwd", |u, v| u < v);
        assert_eq!(f.view().node_count(), 4); // node set unchanged
        assert_eq!(f.snapshot().generation, 1);
        let mut e = f.edges();
        e.sort_unstable();
        assert_eq!(e, vec![(0, 1), (1, 2), (2, 3)]);
    }

    #[test]
    fn mutate_stages_scores_durably_for_downstream_algos() {
        let mut p = PersistentGraphProjection::from_edges("g", 1, 4, EDGES);
        // run pagerank, then stage it back onto the projection (.mutate).
        let pr = aiondb_graph::algorithms::pagerank::pagerank_default(p.graph_view());
        p.set_node_property("pr", pr.clone()).unwrap();
        assert_eq!(p.property_keys(), vec!["pr"]);
        assert_eq!(p.node_property("pr"), Some(pr.as_slice()));
        assert!((p.node_property_value("pr", 2).unwrap() - pr[2]).abs() < 1e-12);

        // wrong length is rejected.
        assert!(p.set_node_property("bad", vec![0.0; 3]).is_err());

        // survives a blob round-trip (Neo4j's mutated props are RAM-only).
        let back = PersistentGraphProjection::from_bytes(&p.to_bytes().unwrap()).unwrap();
        assert_eq!(back.node_property("pr"), Some(pr.as_slice()));

        // a topology mutation yields a fresh graph with no stale props.
        let sub = p.subgraph("g_sub", &[true, true, true, false]);
        assert!(sub.property_keys().is_empty());

        assert_eq!(p.drop_property("pr").map(|v| v.len()), Some(4));
        assert!(p.property_keys().is_empty());
    }

    #[test]
    fn with_removed_edges_is_the_inverse_of_add() {
        let base = PersistentGraphProjection::from_edges("g", 4, 4, EDGES);
        // drop two real edges + one unknown (ignored).
        let removed = base.with_removed_edges(&[(2, 0), (3, 1), (0, 3)]);
        assert_eq!(removed.snapshot().generation, 5);
        assert_eq!(removed.view().node_count(), 4); // node set unchanged
        let mut got = removed.edges();
        got.sort_unstable();
        assert_eq!(got, vec![(0, 1), (1, 2), (2, 3)]);

        // add-then-remove round-trips the topology exactly.
        let back = base
            .with_added_edges(&[(0, 3)])
            .with_removed_edges(&[(0, 3)]);
        let mut after = back.edges();
        after.sort_unstable();
        let mut want = EDGES.to_vec();
        want.sort_unstable();
        assert_eq!(after, want);

        // multiset: two requested, only one present -> one removed.
        let single = PersistentGraphProjection::from_edges("h", 1, 2, &[(0, 1)]);
        assert!(single
            .with_removed_edges(&[(0, 1), (0, 1)])
            .edges()
            .is_empty());
    }

    #[test]
    fn integer_seed_property_is_durable_and_independent() {
        let mut p = PersistentGraphProjection::from_edges("g", 1, 4, EDGES);
        // a community seed for seeded Louvain/LPA.
        p.set_node_property_i64("seed", vec![0, 0, 1, 1]).unwrap();
        // f64 + i64 columns coexist independently.
        p.set_node_property("pr", vec![0.25; 4]).unwrap();

        assert_eq!(p.property_keys_i64(), vec!["seed"]);
        assert_eq!(p.property_keys(), vec!["pr"]);
        assert_eq!(p.node_property_i64("seed"), Some([0, 0, 1, 1].as_slice()));
        assert_eq!(p.node_property_i64_value("seed", 3), Some(1));

        // length is validated.
        assert!(p.set_node_property_i64("bad", vec![1, 2]).is_err());

        // seed survives a blob round-trip (Neo4j re-reads it every session).
        let back = PersistentGraphProjection::from_bytes(&p.to_bytes().unwrap()).unwrap();
        assert_eq!(
            back.node_property_i64("seed"),
            Some([0, 0, 1, 1].as_slice())
        );
        assert_eq!(back.node_property("pr").map(<[f64]>::len), Some(4));

        assert_eq!(p.drop_property_i64("seed").map(|v| v.len()), Some(4));
        assert!(p.property_keys_i64().is_empty());
    }

    #[test]
    fn durable_pipeline_stage_then_filter_by_property() {
        let mut p = PersistentGraphProjection::from_edges("g", 1, 4, EDGES);
        // .mutate a score, then drive a topology filter straight off it.
        p.set_node_property("pr", vec![0.1, 0.4, 0.3, 0.6]).unwrap();
        let hot = p.subgraph_by_property("hot", "pr", |s| s >= 0.35);
        // survivors: ordinals 1,3 -> relabelled 0,1 ; edge (3,1) -> (1,0).
        assert_eq!(hot.view().node_count(), 2);
        assert_eq!(hot.edges(), vec![(1, 0)]);
        assert_eq!(hot.snapshot().generation, 1);

        // unknown property -> everything dropped.
        assert_eq!(
            p.subgraph_by_property("x", "missing", |_| true)
                .view()
                .node_count(),
            0
        );

        // predicate variant.
        let evens = p.subgraph_where("ev", |o| o % 2 == 0);
        assert_eq!(evens.view().node_count(), 2);
        assert_eq!(evens.edges(), vec![(1, 0)]); // (2,0) remapped
    }

    #[test]
    fn store_mutates_cached_named_graph_in_place() {
        let mut store = PersistentProjectionStore::new();
        store.upsert(PersistentGraphProjection::from_edges("g", 1, 4, EDGES));

        let after = store.apply_added_edges("g", &[(0, 3)]).unwrap();
        assert_eq!(after.snapshot().generation, 2);
        assert!(after.edges().contains(&(0, 3)));
        // still cached under the same name (catalog reflects the new gen).
        assert_eq!(store.catalog()[0].generation, 2);

        store.apply_removed_edges("g", &[(0, 3)]).unwrap();
        let g = store.get("g").unwrap();
        assert!(!g.edges().contains(&(0, 3)));
        assert_eq!(g.snapshot().generation, 3);

        // weighted in-place lifecycle.
        store.upsert_weighted(PersistentWeightedProjection::from_edges(
            "w",
            1,
            3,
            &[(0, 1, 2.0)],
        ));
        let w = store
            .apply_added_edges_weighted("w", &[(1, 2, 5.0)])
            .unwrap();
        assert_eq!(w.snapshot().generation, 2);
        assert_eq!(w.edges().len(), 2);
        store.apply_removed_edges_weighted("w", &[(0, 1)]).unwrap();
        assert_eq!(store.get_weighted("w").unwrap().edges().len(), 1);

        // unknown name -> error, nothing inserted.
        assert!(store.apply_added_edges("nope", &[(0, 1)]).is_err());
        assert!(!store.contains("nope"));
    }

    #[test]
    fn per_graph_export_import_round_trips_through_a_fresh_store() {
        let mut src = PersistentProjectionStore::new();
        src.upsert(
            PersistentGraphProjection::from_edges_with_ids(
                "g",
                7,
                vec![Value::BigInt(10), Value::BigInt(11), Value::BigInt(12)],
                &[(0, 1), (1, 2)],
            )
            .unwrap(),
        );
        src.upsert_weighted(PersistentWeightedProjection::from_edges(
            "w",
            2,
            2,
            &[(0, 1, 4.0)],
        ));
        assert!(src.export("missing").is_err());

        let g_blob = src.export("g").unwrap();
        let w_blob = src.export("w").unwrap();

        // a brand-new store can adopt just that one graph.
        let mut dst = PersistentProjectionStore::new();
        let g = dst.import(&g_blob).unwrap();
        assert_eq!(g.projection_name(), "g");
        assert_eq!(g.snapshot().generation, 7);
        assert_eq!(g.node_ordinal(&Value::BigInt(12)), Some(2));
        assert_eq!(
            dst.import_weighted(&w_blob)
                .unwrap()
                .weighted()
                .node_count(),
            2
        );
        assert_eq!(dst.names(), vec!["g"]);
        assert_eq!(dst.weighted_names(), vec!["w"]);
    }

    #[test]
    fn undirected_projection_symmetrises_for_community_algos() {
        let p = PersistentGraphProjection::from_edges("g", 3, 4, EDGES);
        let u = p.as_undirected("g_undir");
        assert_eq!(u.snapshot().generation, 1);
        assert_eq!(u.view().node_count(), 4);
        // 5 directed edges, none mutually reciprocal -> 10 undirected.
        assert_eq!(u.view().edge_count(), 10);
        // every edge now has its reverse.
        let set: std::collections::BTreeSet<_> = u.edges().into_iter().collect();
        for &(a, b) in EDGES {
            assert!(set.contains(&(a, b)) && set.contains(&(b, a)));
        }
        // adjacency is exactly symmetric: out-degree == in-degree per node.
        for node in 0..4u32 {
            let out = u.view().neighbor_slice(node).map_or(0, <[u32]>::len);
            let inc = u
                .view()
                .reverse_neighbor_slice(node)
                .map_or(0, <[u32]>::len);
            assert_eq!(out, inc);
        }

        // already-symmetric input does not double edges.
        let s = PersistentGraphProjection::from_edges("s", 1, 2, &[(0, 1), (1, 0)]);
        assert_eq!(s.as_undirected("s2").view().edge_count(), 2);
    }

    #[test]
    fn projection_cleaning_self_loops_and_parallels() {
        let dirty = PersistentGraphProjection::from_edges(
            "d",
            1,
            3,
            &[(0, 0), (0, 1), (0, 1), (1, 2), (2, 2)],
        );
        // self-loops gone, parallels still present.
        let nl = dirty.without_self_loops("nl");
        assert_eq!(nl.snapshot().generation, 1);
        let mut e = nl.edges();
        e.sort_unstable();
        assert_eq!(e, vec![(0, 1), (0, 1), (1, 2)]);

        // parallels collapsed, self-loops still present.
        let simple = dirty.as_simple("s");
        let mut e2 = simple.edges();
        e2.sort_unstable();
        assert_eq!(e2, vec![(0, 0), (0, 1), (1, 2), (2, 2)]);

        // chain both -> fully clean simple graph.
        let clean = dirty.without_self_loops("a").as_simple("b");
        let mut e3 = clean.edges();
        e3.sort_unstable();
        assert_eq!(e3, vec![(0, 1), (1, 2)]);
    }

    #[test]
    fn degree_summary_reports_distribution_and_density() {
        // 5 nodes, node 4 isolated. out-deg: 1,1,2,1,0.
        let p = PersistentGraphProjection::from_edges("g", 1, 5, EDGES);
        let s = p.degree_summary();
        assert_eq!(s.node_count, 5);
        assert_eq!(s.edge_count, 5);
        assert_eq!(s.min_out_degree, 0);
        assert_eq!(s.max_out_degree, 2);
        assert_eq!(s.isolated_out, 1);
        assert!((s.mean_out_degree - 1.0).abs() < 1e-12);
        assert!((s.density - 5.0 / 20.0).abs() < 1e-12);

        // empty graph is well-defined.
        let e = PersistentGraphProjection::from_edges("e", 0, 0, &[]).degree_summary();
        assert_eq!((e.node_count, e.min_out_degree, e.isolated_out), (0, 0, 0));
        assert!((e.density - 0.0).abs() < 1e-12);
    }

    #[test]
    fn ego_subgraph_is_bfs_bounded_and_center_is_ordinal_zero() {
        let p = PersistentGraphProjection::from_edges("g", 1, 4, EDGES);

        let r0 = p.ego_subgraph("e0", 0, 0);
        assert_eq!(r0.view().node_count(), 1);
        assert!(r0.edges().is_empty());

        let r1 = p.ego_subgraph("e1", 0, 1); // 0 -> {1}
        assert_eq!(r1.view().node_count(), 2);
        assert_eq!(r1.edges(), vec![(0, 1)]);

        let r2 = p.ego_subgraph("e2", 0, 2); // + 1 -> {2}
        assert_eq!(r2.view().node_count(), 3);
        let mut e = r2.edges();
        e.sort_unstable();
        assert_eq!(e, vec![(0, 1), (1, 2), (2, 0)]);

        // out-of-range center -> empty projection.
        assert_eq!(p.ego_subgraph("x", 9, 3).view().node_count(), 0);
    }

    #[test]
    fn ego_subgraph_maps_center_to_zero_and_carries_ids() {
        let p = PersistentGraphProjection::from_edges_with_ids(
            "g",
            1,
            vec![
                Value::BigInt(100),
                Value::BigInt(101),
                Value::BigInt(102),
                Value::BigInt(103),
            ],
            EDGES,
        )
        .unwrap();
        // center 2 -> neighbors {0,3} at radius 1.
        let ego = p.ego_subgraph("ego", 2, 1);
        assert_eq!(ego.view().node_count(), 3);
        assert_eq!(ego.node_id(0), Some(Value::BigInt(102))); // center == 0
        assert_eq!(ego.node_ordinal(&Value::BigInt(102)), Some(0));
        assert_eq!(ego.node_ordinal(&Value::BigInt(101)), None); // excluded
                                                                 // durable.
        let back = PersistentGraphProjection::from_bytes(&ego.to_bytes().unwrap()).unwrap();
        assert_eq!(back.node_ordinal(&Value::BigInt(103)), Some(2));
    }

    #[test]
    fn topology_memory_estimate_matches_csr_layout() {
        // n=4, e=5 -> offsets (4+1)*4 + targets 5*4 = 40.
        let p = PersistentGraphProjection::from_edges("g", 1, 4, EDGES);
        assert_eq!(p.estimated_topology_bytes(), 40);
        assert_eq!(crate::estimate_csr_bytes(4, 5), 40);
        // weighted adds 8 bytes/edge.
        assert_eq!(crate::estimate_weighted_csr_bytes(4, 5), 80);
    }

    #[test]
    fn label_scoped_subgraph_filters_by_integer_label_column() {
        let mut p = PersistentGraphProjection::from_edges("g", 1, 4, EDGES);
        p.set_node_property_i64("lbl", vec![10, 20, 10, 30])
            .unwrap();

        // single allowed label -> ordinals 0,2 survive (edge (2,0)).
        let a = p.subgraph_by_label("a", "lbl", &[10]);
        assert_eq!(a.view().node_count(), 2);
        assert_eq!(a.edges(), vec![(1, 0)]); // 2->0 remapped

        // multiple labels.
        let b = p.subgraph_by_label("b", "lbl", &[10, 30]);
        assert_eq!(b.view().node_count(), 3);

        // unknown column -> empty.
        assert_eq!(
            p.subgraph_by_label("c", "missing", &[10])
                .view()
                .node_count(),
            0
        );
    }

    #[test]
    fn reversed_projection_transposes_edges() {
        let p = PersistentGraphProjection::from_edges("g", 3, 4, EDGES);
        let r = p.as_reversed("rev");
        assert_eq!(r.snapshot().generation, 1);
        assert_eq!(r.view().node_count(), 4);
        let got: std::collections::BTreeSet<_> = r.edges().into_iter().collect();
        let want: std::collections::BTreeSet<_> = EDGES.iter().map(|&(u, v)| (v, u)).collect();
        assert_eq!(got, want);
        // double transpose == identity topology.
        let back: std::collections::BTreeSet<_> = r.as_reversed("r2").edges().into_iter().collect();
        let orig: std::collections::BTreeSet<_> = EDGES.iter().copied().collect();
        assert_eq!(back, orig);
    }

    #[test]
    fn largest_component_extracts_biggest_wcc() {
        // comp A {0,1,2}, comp B {3,4}, node 5 isolated.
        let p = PersistentGraphProjection::from_edges("g", 1, 6, &[(0, 1), (1, 2), (3, 4)]);
        let lc = p.largest_component("lc");
        assert_eq!(lc.snapshot().generation, 1);
        assert_eq!(lc.view().node_count(), 3);
        let mut e = lc.edges();
        e.sort_unstable();
        assert_eq!(e, vec![(0, 1), (1, 2)]);

        // tie on size -> smallest root wins (deterministic).
        let t = PersistentGraphProjection::from_edges("t", 1, 4, &[(0, 1), (2, 3)]);
        let tlc = t.largest_component("tlc");
        assert_eq!(tlc.view().node_count(), 2);
        assert_eq!(tlc.edges(), vec![(0, 1)]); // component {0,1}

        // empty graph stays empty.
        assert_eq!(
            PersistentGraphProjection::from_edges("e", 1, 0, &[])
                .largest_component("x")
                .view()
                .node_count(),
            0
        );
    }

    #[test]
    fn detailed_catalog_folds_degrees_and_estimate_per_entry() {
        let mut store = PersistentProjectionStore::new();
        store.upsert(PersistentGraphProjection::from_edges("g", 2, 4, EDGES));
        store.upsert_weighted(PersistentWeightedProjection::from_edges(
            "g",
            3,
            2,
            &[(0, 1, 1.0)],
        ));

        let d = store.catalog_detailed();
        assert_eq!(d.len(), 2);
        // sorted: ("g", unweighted) before ("g", weighted).
        assert!(!d[0].entry.weighted && d[1].entry.weighted);

        let u = &d[0];
        assert_eq!(u.entry.node_count, 4);
        assert_eq!(u.degrees.node_count, 4);
        assert_eq!(u.degrees.max_out_degree, 2); // node 2 has 2 out-edges
        assert_eq!(u.estimated_topology_bytes, crate::estimate_csr_bytes(4, 5));

        let w = &d[1];
        assert!(w.entry.weighted);
        assert_eq!(
            w.estimated_topology_bytes,
            crate::estimate_weighted_csr_bytes(2, 1)
        );
    }

    #[test]
    fn integrity_check_passes_for_all_constructed_projections() {
        // ids + both property kinds + a mutation + a derived view + reload.
        let mut p = PersistentGraphProjection::from_edges_with_ids(
            "g",
            1,
            vec![Value::BigInt(1), Value::BigInt(2), Value::BigInt(3)],
            &[(0, 1), (1, 2)],
        )
        .unwrap();
        p.set_node_property("s", vec![0.1, 0.2, 0.3]).unwrap();
        p.set_node_property_i64("l", vec![1, 1, 2]).unwrap();
        p.verify().unwrap();
        p.with_added_edges(&[(2, 0)]).verify().unwrap();
        p.subgraph("sub", &[true, true, false]).verify().unwrap();
        PersistentGraphProjection::from_bytes(&p.to_bytes().unwrap())
            .unwrap()
            .verify()
            .unwrap();

        let mut store = PersistentProjectionStore::new();
        store.upsert(p);
        store.upsert_weighted(
            PersistentWeightedProjection::from_edges("g", 1, 2, &[(0, 1, 1.0)])
                .with_node_ids(vec![Value::BigInt(7), Value::BigInt(8)])
                .unwrap(),
        );
        store.verify().unwrap();
        // survives a full store round-trip and still verifies.
        PersistentProjectionStore::from_bytes(&store.to_bytes().unwrap())
            .unwrap()
            .verify()
            .unwrap();
    }
}
