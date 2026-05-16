//! Persistent graph projection metadata and catalog contracts.
//!
//! This crate is the landing zone for the future projection engine that will
//! own named graph snapshots, refresh state, and graph-to-ordinal mappings.

#![allow(
    clippy::missing_errors_doc,
    clippy::must_use_candidate,
    clippy::needless_pass_by_value,
    clippy::similar_names,
    clippy::too_many_lines
)]

use std::sync::RwLock;

use aiondb_core::{DbError, DbResult};
use aiondb_graph_api::{GraphStats, ProjectionSnapshot, RefreshPolicy};

mod persistent;
mod persistent_weighted;
mod shared;
pub use persistent::{
    DetailedCatalogEntry, PersistentGraphProjection, PersistentProjectionStore,
    ProjectionCatalogEntry,
};
pub use persistent_weighted::{EdgeAggregation, PersistentWeightedProjection};
pub use shared::SharedProjectionCatalog;

/// Out-degree distribution summary of a projection -- the durable analogue
/// of the shape Neo4j surfaces in `gds.graph.list` (counts, min/max/mean
/// degree, isolated-node count, edge density). Cheap structural stats used
/// for query planning and capacity reporting.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DegreeSummary {
    pub node_count: u32,
    pub edge_count: u64,
    pub min_out_degree: u32,
    pub max_out_degree: u32,
    pub mean_out_degree: f64,
    /// Nodes with out-degree 0.
    pub isolated_out: u32,
    /// `edge_count / (n * (n - 1))` for a directed simple graph; 0 if n < 2.
    pub density: f64,
}

/// Estimate the resident bytes of an **unweighted** compact CSR topology
/// (Neo4j `gds.graph.project.estimate`): offsets `(n + 1) * 4` + targets
/// `e * 4`. Lets the planner size a projection before paying to build it.
#[must_use]
pub fn estimate_csr_bytes(node_count: u32, edge_count: u64) -> u64 {
    (u64::from(node_count) + 1) * 4 + edge_count * 4
}

/// Same, for a **weighted** CSR (adds an `f64` weight per edge).
#[must_use]
pub fn estimate_weighted_csr_bytes(node_count: u32, edge_count: u64) -> u64 {
    estimate_csr_bytes(node_count, edge_count) + edge_count * 8
}

/// Compute the out-degree summary of any [`GraphViewV2`].
#[must_use]
pub fn degree_summary<G>(graph: &G) -> DegreeSummary
where
    G: aiondb_graph_api::GraphViewV2 + ?Sized,
{
    let n = graph.node_count();
    let edges = graph.edge_count();
    let mut min_d = u32::MAX;
    let mut max_d = 0u32;
    let mut isolated = 0u32;
    for v in 0..n {
        let d = graph.degree(v);
        min_d = min_d.min(d);
        max_d = max_d.max(d);
        if d == 0 {
            isolated += 1;
        }
    }
    if n == 0 {
        min_d = 0;
    }
    let mean = if n == 0 {
        0.0
    } else {
        edges as f64 / f64::from(n)
    };
    let density = if n < 2 {
        0.0
    } else {
        edges as f64 / (f64::from(n) * f64::from(n - 1))
    };
    DegreeSummary {
        node_count: n,
        edge_count: edges,
        min_out_degree: min_d,
        max_out_degree: max_d,
        mean_out_degree: mean,
        isolated_out: isolated,
        density,
    }
}

/// How a projection is maintained relative to the relational source of truth.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ProjectionBuildMode {
    Live,
    Async,
    Snapshot,
}

impl From<RefreshPolicy> for ProjectionBuildMode {
    fn from(value: RefreshPolicy) -> Self {
        match value {
            RefreshPolicy::Live => Self::Live,
            RefreshPolicy::Async => Self::Async,
            RefreshPolicy::Snapshot => Self::Snapshot,
        }
    }
}

/// Runtime/catalog visibility for a named projection lifecycle.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ProjectionState {
    Building,
    Ready,
    Stale,
    Failed,
}

/// Stable descriptor for a named graph projection.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct NamedGraphProjectionDescriptor {
    pub name: String,
    pub snapshot: ProjectionSnapshot,
    pub stats: GraphStats,
    pub build_mode: ProjectionBuildMode,
    pub state: ProjectionState,
}

impl NamedGraphProjectionDescriptor {
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        snapshot: ProjectionSnapshot,
        stats: GraphStats,
        state: ProjectionState,
    ) -> Self {
        Self {
            name: name.into(),
            snapshot,
            stats,
            build_mode: ProjectionBuildMode::from(snapshot.refresh_policy),
            state,
        }
    }
}

/// Result of runtime projection discovery.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiscoveredProjection {
    pub descriptor: NamedGraphProjectionDescriptor,
    pub ready: bool,
}

/// Generic runtime record for a discovered projection keyed by an
/// implementation-specific identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeProjectionRecord<K> {
    pub key: K,
    pub discovered: DiscoveredProjection,
}

/// Lightweight runtime catalog view for recently discovered projections.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RuntimeProjectionCatalogView<K> {
    latest: Option<RuntimeProjectionRecord<K>>,
}

impl<K: Eq> RuntimeProjectionCatalogView<K> {
    #[must_use]
    pub fn new() -> Self {
        Self { latest: None }
    }

    pub fn remember(&mut self, record: RuntimeProjectionRecord<K>) {
        self.latest = Some(record);
    }

    #[must_use]
    pub fn latest(&self) -> Option<&RuntimeProjectionRecord<K>> {
        self.latest.as_ref()
    }

    #[must_use]
    pub fn find(&self, key: &K) -> Option<&RuntimeProjectionRecord<K>> {
        self.latest.as_ref().filter(|record| &record.key == key)
    }
}

/// Runtime catalog view specialized for Cypher-native projections.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CypherNativeProjectionCatalog<K> {
    view: RuntimeProjectionCatalogView<K>,
}

impl<K: Eq> CypherNativeProjectionCatalog<K> {
    #[must_use]
    pub fn new() -> Self {
        Self {
            view: RuntimeProjectionCatalogView::new(),
        }
    }

    pub fn remember(&mut self, record: RuntimeProjectionRecord<K>) {
        self.view.remember(record);
    }

    #[must_use]
    pub fn latest_discovered(&self) -> Option<&DiscoveredProjection> {
        self.latest().map(|record| &record.discovered)
    }

    #[must_use]
    pub fn latest(&self) -> Option<&RuntimeProjectionRecord<K>> {
        self.view.latest()
    }

    #[must_use]
    pub fn find(&self, key: &K) -> Option<&RuntimeProjectionRecord<K>> {
        self.view.find(key)
    }

    #[must_use]
    pub fn find_discovered(&self, key: &K) -> Option<&DiscoveredProjection> {
        self.find(key).map(|record| &record.discovered)
    }

    #[must_use]
    pub fn resolve_named_projection(&self, name: &str) -> Option<DiscoveredProjection> {
        self.latest_discovered()
            .filter(|discovered| discovered.descriptor.name == name)
            .cloned()
    }
}

impl<K: Eq + Send + Sync> GraphProjectionCatalog for CypherNativeProjectionCatalog<K> {
    fn list_projections(&self) -> DbResult<Vec<NamedGraphProjectionDescriptor>> {
        Ok(self
            .latest()
            .map(|record| vec![record.discovered.descriptor.clone()])
            .unwrap_or_default())
    }

    fn get_projection(&self, name: &str) -> DbResult<Option<NamedGraphProjectionDescriptor>> {
        Ok(self
            .latest()
            .filter(|record| record.discovered.descriptor.name == name)
            .map(|record| record.discovered.descriptor.clone()))
    }
}

/// Thread-safe runtime registry for Cypher-native projections.
#[derive(Default)]
pub struct CypherNativeProjectionRegistry<K> {
    catalog: RwLock<CypherNativeProjectionCatalog<K>>,
}

impl<K: Eq> CypherNativeProjectionRegistry<K> {
    #[must_use]
    pub fn new() -> Self {
        Self {
            catalog: RwLock::new(CypherNativeProjectionCatalog::new()),
        }
    }

    pub fn read<T>(
        &self,
        reader: impl FnOnce(&CypherNativeProjectionCatalog<K>) -> DbResult<T>,
    ) -> DbResult<T> {
        let guard = self.catalog.read().map_err(|error| {
            DbError::internal(format!(
                "cypher native projection registry poisoned: {error}"
            ))
        })?;
        reader(&guard)
    }

    pub fn write<T>(
        &self,
        writer: impl FnOnce(&mut CypherNativeProjectionCatalog<K>) -> DbResult<T>,
    ) -> DbResult<T> {
        let mut guard = self.catalog.write().map_err(|error| {
            DbError::internal(format!(
                "cypher native projection registry poisoned: {error}"
            ))
        })?;
        writer(&mut guard)
    }

    pub fn list_projections(&self) -> DbResult<Vec<NamedGraphProjectionDescriptor>>
    where
        K: Send + Sync,
    {
        self.read(GraphProjectionCatalog::list_projections)
    }

    pub fn find_discovered(&self, key: &K) -> DbResult<Option<DiscoveredProjection>> {
        self.read(|catalog| Ok(catalog.find_discovered(key).cloned()))
    }

    pub fn resolve_named_projection(&self, name: &str) -> DbResult<Option<DiscoveredProjection>>
    where
        K: Send + Sync,
    {
        self.read(|catalog| Ok(catalog.resolve_named_projection(name)))
    }

    pub fn resolve_named_projection_or_placeholder(
        &self,
        name: &str,
        node_labels: &[(String, u64)],
        edge_labels: &[(String, u64)],
        snapshot: ProjectionSnapshot,
        weighted: bool,
    ) -> DbResult<DiscoveredProjection>
    where
        K: Send + Sync,
    {
        Ok(discovered_projection_or_placeholder(
            self.resolve_named_projection(name)?,
            node_labels,
            edge_labels,
            snapshot,
            weighted,
        ))
    }

    pub fn remember_discovered(&self, key: K, discovered: DiscoveredProjection) -> DbResult<()> {
        self.write(|catalog| {
            catalog.remember(runtime_projection_record(key, discovered));
            Ok(())
        })
    }
}

/// Catalog contract for persistent named graph projections.
pub trait GraphProjectionCatalog: Send + Sync {
    fn list_projections(&self) -> DbResult<Vec<NamedGraphProjectionDescriptor>>;

    fn get_projection(&self, name: &str) -> DbResult<Option<NamedGraphProjectionDescriptor>>;
}

#[must_use]
pub fn ready_projection_descriptor(
    name: impl Into<String>,
    snapshot: ProjectionSnapshot,
    stats: GraphStats,
) -> NamedGraphProjectionDescriptor {
    NamedGraphProjectionDescriptor::new(name, snapshot, stats, ProjectionState::Ready)
}

#[must_use]
pub fn placeholder_projection_descriptor(
    name: impl Into<String>,
    snapshot: ProjectionSnapshot,
    weighted: bool,
) -> NamedGraphProjectionDescriptor {
    NamedGraphProjectionDescriptor::new(
        name,
        snapshot,
        GraphStats {
            node_count: None,
            edge_count: 0,
            source_node_count: None,
            target_node_count: None,
            has_reverse_adjacency: true,
            has_weighted_adjacency: weighted,
            directed: true,
        },
        ProjectionState::Stale,
    )
}

#[must_use]
pub fn discovered_projection(
    descriptor: NamedGraphProjectionDescriptor,
    ready: bool,
) -> DiscoveredProjection {
    DiscoveredProjection { descriptor, ready }
}

#[must_use]
pub fn discovered_projection_or_placeholder(
    resolved: Option<DiscoveredProjection>,
    node_labels: &[(String, u64)],
    edge_labels: &[(String, u64)],
    snapshot: ProjectionSnapshot,
    weighted: bool,
) -> DiscoveredProjection {
    resolved.unwrap_or_else(|| {
        cypher_native_placeholder_projection(node_labels, edge_labels, snapshot, weighted)
    })
}

#[must_use]
pub fn runtime_projection_record<K>(
    key: K,
    discovered: DiscoveredProjection,
) -> RuntimeProjectionRecord<K> {
    RuntimeProjectionRecord { key, discovered }
}

#[must_use]
pub fn cypher_native_projection_name(
    node_labels: &[(String, u64)],
    edge_labels: &[(String, u64)],
) -> String {
    if node_labels.is_empty() && edge_labels.is_empty() {
        return "cypher.native.graph.default".to_owned();
    }
    let node_part = if node_labels.is_empty() {
        "none".to_owned()
    } else {
        node_labels
            .iter()
            .map(|(label, table_id)| format!("{label}#{table_id}"))
            .collect::<Vec<_>>()
            .join("+")
    };
    let edge_part = if edge_labels.is_empty() {
        "none".to_owned()
    } else {
        edge_labels
            .iter()
            .map(|(label, table_id)| format!("{label}#{table_id}"))
            .collect::<Vec<_>>()
            .join("+")
    };
    format!("cypher.native.graph[nodes={node_part};edges={edge_part}]")
}

#[must_use]
pub fn cypher_native_placeholder_projection(
    node_labels: &[(String, u64)],
    edge_labels: &[(String, u64)],
    snapshot: ProjectionSnapshot,
    weighted: bool,
) -> DiscoveredProjection {
    discovered_projection(
        placeholder_projection_descriptor(
            cypher_native_projection_name(node_labels, edge_labels),
            snapshot,
            weighted,
        ),
        false,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptor_build_mode_tracks_snapshot_refresh_policy() {
        let descriptor = NamedGraphProjectionDescriptor::new(
            "people-knows",
            ProjectionSnapshot {
                generation: 7,
                refresh_policy: RefreshPolicy::Async,
                refreshed_at_epoch_millis: Some(1234),
            },
            GraphStats {
                node_count: Some(3),
                edge_count: 2,
                source_node_count: None,
                target_node_count: None,
                has_reverse_adjacency: true,
                has_weighted_adjacency: false,
                directed: true,
            },
            ProjectionState::Ready,
        );

        assert_eq!(descriptor.name, "people-knows");
        assert_eq!(descriptor.build_mode, ProjectionBuildMode::Async);
        assert_eq!(descriptor.state, ProjectionState::Ready);
        assert_eq!(descriptor.stats.edge_count, 2);
    }

    #[test]
    fn cypher_native_projection_name_formats_default_and_labeled_variants() {
        assert_eq!(
            cypher_native_projection_name(&[], &[]),
            "cypher.native.graph.default"
        );
        assert_eq!(
            cypher_native_projection_name(
                &[(String::from("Person"), 1), (String::from("Company"), 2)],
                &[(String::from("KNOWS"), 3)]
            ),
            "cypher.native.graph[nodes=Person#1+Company#2;edges=KNOWS#3]"
        );
    }

    #[test]
    fn placeholder_projection_descriptor_marks_stats_unknown_but_weighted_shape_known() {
        let descriptor = placeholder_projection_descriptor(
            "cypher.native.graph.default",
            ProjectionSnapshot {
                generation: 11,
                refresh_policy: RefreshPolicy::Snapshot,
                refreshed_at_epoch_millis: None,
            },
            true,
        );

        assert_eq!(descriptor.state, ProjectionState::Stale);
        assert_eq!(descriptor.snapshot.generation, 11);
        assert_eq!(descriptor.stats.node_count, None);
        assert!(descriptor.stats.has_weighted_adjacency);
    }

    #[test]
    fn cypher_native_placeholder_projection_marks_discovery_not_ready() {
        let discovered = cypher_native_placeholder_projection(
            &[(String::from("Person"), 1)],
            &[(String::from("KNOWS"), 2)],
            ProjectionSnapshot {
                generation: 9,
                refresh_policy: RefreshPolicy::Snapshot,
                refreshed_at_epoch_millis: None,
            },
            false,
        );

        assert!(!discovered.ready);
        assert_eq!(
            discovered.descriptor.name,
            "cypher.native.graph[nodes=Person#1;edges=KNOWS#2]"
        );
        assert_eq!(discovered.descriptor.state, ProjectionState::Stale);
    }

    #[test]
    fn runtime_projection_record_preserves_key_and_discovery() {
        let discovered = cypher_native_placeholder_projection(
            &[(String::from("Person"), 1)],
            &[],
            ProjectionSnapshot {
                generation: 5,
                refresh_policy: RefreshPolicy::Snapshot,
                refreshed_at_epoch_millis: None,
            },
            false,
        );
        let record = runtime_projection_record(String::from("cache-key"), discovered.clone());

        assert_eq!(record.key, "cache-key");
        assert_eq!(record.discovered, discovered);
    }

    #[test]
    fn runtime_projection_catalog_view_remembers_and_filters_latest_record() {
        let discovered = cypher_native_placeholder_projection(
            &[(String::from("Person"), 1)],
            &[],
            ProjectionSnapshot {
                generation: 5,
                refresh_policy: RefreshPolicy::Snapshot,
                refreshed_at_epoch_millis: None,
            },
            false,
        );
        let record = runtime_projection_record(String::from("cache-key"), discovered.clone());
        let mut view = RuntimeProjectionCatalogView::new();
        view.remember(record.clone());

        assert_eq!(view.latest(), Some(&record));
        assert_eq!(view.find(&String::from("cache-key")), Some(&record));
        assert_eq!(view.find(&String::from("other")), None);
    }

    #[test]
    fn cypher_native_projection_catalog_wraps_runtime_view() {
        let discovered = cypher_native_placeholder_projection(
            &[(String::from("Person"), 1)],
            &[],
            ProjectionSnapshot {
                generation: 12,
                refresh_policy: RefreshPolicy::Snapshot,
                refreshed_at_epoch_millis: None,
            },
            false,
        );
        let record = runtime_projection_record(String::from("graph-key"), discovered);
        let mut catalog = CypherNativeProjectionCatalog::new();
        catalog.remember(record.clone());

        assert_eq!(catalog.latest(), Some(&record));
        assert_eq!(catalog.find(&String::from("graph-key")), Some(&record));
        assert_eq!(catalog.find(&String::from("missing")), None);
    }

    #[test]
    fn cypher_native_projection_catalog_implements_projection_catalog_contract() {
        let discovered = discovered_projection(
            ready_projection_descriptor(
                "cypher.native.graph[nodes=Person#1;edges=KNOWS#2]",
                ProjectionSnapshot {
                    generation: 14,
                    refresh_policy: RefreshPolicy::Snapshot,
                    refreshed_at_epoch_millis: Some(77),
                },
                GraphStats {
                    node_count: Some(2),
                    edge_count: 1,
                    source_node_count: None,
                    target_node_count: None,
                    has_reverse_adjacency: true,
                    has_weighted_adjacency: false,
                    directed: true,
                },
            ),
            true,
        );
        let record = runtime_projection_record(String::from("graph-key"), discovered.clone());
        let mut catalog = CypherNativeProjectionCatalog::new();
        catalog.remember(record);

        let listed = GraphProjectionCatalog::list_projections(&catalog).expect("list projections");
        assert_eq!(listed, vec![discovered.descriptor.clone()]);
        assert_eq!(
            GraphProjectionCatalog::get_projection(
                &catalog,
                "cypher.native.graph[nodes=Person#1;edges=KNOWS#2]"
            )
            .expect("get projection"),
            Some(discovered.descriptor)
        );
        assert_eq!(
            GraphProjectionCatalog::get_projection(&catalog, "missing").expect("missing"),
            None
        );
    }

    #[test]
    fn cypher_native_projection_catalog_resolve_named_projection_preserves_ready_state() {
        let discovered = cypher_native_placeholder_projection(
            &[(String::from("Person"), 1)],
            &[(String::from("KNOWS"), 2)],
            ProjectionSnapshot {
                generation: 18,
                refresh_policy: RefreshPolicy::Snapshot,
                refreshed_at_epoch_millis: None,
            },
            false,
        );
        let mut catalog = CypherNativeProjectionCatalog::new();
        catalog.remember(runtime_projection_record(
            String::from("graph-key"),
            discovered.clone(),
        ));

        let resolved = catalog
            .resolve_named_projection("cypher.native.graph[nodes=Person#1;edges=KNOWS#2]")
            .expect("resolved latest projection");
        assert_eq!(resolved, discovered);
        assert!(!resolved.ready);
    }

    #[test]
    fn cypher_native_projection_registry_reads_and_writes_catalog() {
        let registry = CypherNativeProjectionRegistry::new();
        let discovered = discovered_projection(
            ready_projection_descriptor(
                "cypher.native.graph[nodes=Person#1;edges=KNOWS#2]",
                ProjectionSnapshot {
                    generation: 21,
                    refresh_policy: RefreshPolicy::Snapshot,
                    refreshed_at_epoch_millis: Some(99),
                },
                GraphStats {
                    node_count: Some(2),
                    edge_count: 1,
                    source_node_count: None,
                    target_node_count: None,
                    has_reverse_adjacency: true,
                    has_weighted_adjacency: false,
                    directed: true,
                },
            ),
            true,
        );

        registry
            .write(|catalog| {
                catalog.remember(runtime_projection_record(
                    String::from("graph-key"),
                    discovered.clone(),
                ));
                Ok(())
            })
            .expect("write registry");

        let latest = registry
            .read(|catalog| Ok(catalog.latest().cloned()))
            .expect("read registry")
            .expect("latest projection");
        assert_eq!(latest.discovered, discovered);

        let resolved = registry
            .resolve_named_projection("cypher.native.graph[nodes=Person#1;edges=KNOWS#2]")
            .expect("resolve named projection")
            .expect("named projection");
        assert_eq!(resolved, discovered);
    }
}
