//! Graph Fabric routing for sharded graph edge tables.
//!
//! Fabric keeps graph-specific placement decisions out of the generic storage
//! wrapper. It answers one question: can an adjacency lookup be served by one
//! physical shard, or must it fan out across every shard?

use aiondb_core::{DbResult, Value};
use aiondb_storage_api::StorageShardConfig;

use crate::placement;

/// Endpoint column ordinals for an edge table.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GraphEdgeEndpoints {
    source_ordinal: usize,
    target_ordinal: usize,
}

impl GraphEdgeEndpoints {
    #[must_use]
    pub const fn new(source_ordinal: usize, target_ordinal: usize) -> Self {
        Self {
            source_ordinal,
            target_ordinal,
        }
    }

    #[must_use]
    pub const fn source_ordinal(self) -> usize {
        self.source_ordinal
    }

    #[must_use]
    pub const fn target_ordinal(self) -> usize {
        self.target_ordinal
    }
}

/// Routing result for a graph adjacency lookup.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GraphShardRoute {
    /// The lookup key is colocated with exactly one shard.
    Single(u32),
    /// The lookup cannot be proven shard-local and must scan all shards.
    FanOut,
}

/// Sharding metadata needed by Fabric to route graph edge adjacency.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GraphShardSpec {
    shard_count: u32,
    shard_key_ordinals: Vec<usize>,
    endpoints: Option<GraphEdgeEndpoints>,
}

impl GraphShardSpec {
    #[must_use]
    pub fn new(config: &StorageShardConfig, shard_key_ordinals: Vec<usize>) -> Self {
        Self {
            shard_count: config.shard_count,
            shard_key_ordinals,
            endpoints: None,
        }
    }

    #[must_use]
    pub fn with_endpoints(mut self, endpoints: GraphEdgeEndpoints) -> Self {
        self.endpoints = Some(endpoints);
        self
    }

    pub fn set_endpoints(&mut self, endpoints: GraphEdgeEndpoints) {
        self.endpoints = Some(endpoints);
    }

    pub fn clear_endpoints(&mut self) {
        self.endpoints = None;
    }

    /// Route an adjacency lookup by endpoint value.
    ///
    /// Outgoing adjacency is shard-local only when the edge table shard key is
    /// exactly the source endpoint. Incoming adjacency is shard-local only when
    /// the shard key is exactly the target endpoint. Multi-column shard keys
    /// need all key values, so they intentionally fall back to fan-out.
    pub fn route_adjacency(&self, node_id: &Value, outgoing: bool) -> DbResult<GraphShardRoute> {
        if self.shard_count == 0 {
            return Ok(GraphShardRoute::FanOut);
        }
        if self.shard_count == 1 {
            return Ok(GraphShardRoute::Single(0));
        }

        let Some(endpoints) = self.endpoints else {
            return Ok(GraphShardRoute::FanOut);
        };
        let endpoint_ordinal = if outgoing {
            endpoints.source_ordinal()
        } else {
            endpoints.target_ordinal()
        };
        if self.shard_key_ordinals.as_slice() != [endpoint_ordinal] {
            return Ok(GraphShardRoute::FanOut);
        }

        placement::values_shard_index([node_id], self.shard_count).map(GraphShardRoute::Single)
    }

    /// Route an exact edge lookup when both endpoints are available.
    ///
    /// This lets Fabric avoid fan-out for composite edge shard keys such as
    /// `(source_id, target_id)`, and still handles source-only or target-only
    /// shard keys with the same hash path as adjacency routing.
    pub fn route_edge(&self, source_id: &Value, target_id: &Value) -> DbResult<GraphShardRoute> {
        if self.shard_count == 0 {
            return Ok(GraphShardRoute::FanOut);
        }
        if self.shard_count == 1 {
            return Ok(GraphShardRoute::Single(0));
        }

        let Some(endpoints) = self.endpoints else {
            return Ok(GraphShardRoute::FanOut);
        };
        match self.shard_key_ordinals.as_slice() {
            [source] if *source == endpoints.source_ordinal() => {
                placement::values_shard_index([source_id], self.shard_count)
                    .map(GraphShardRoute::Single)
            }
            [target] if *target == endpoints.target_ordinal() => {
                placement::values_shard_index([target_id], self.shard_count)
                    .map(GraphShardRoute::Single)
            }
            [source, target]
                if *source == endpoints.source_ordinal()
                    && *target == endpoints.target_ordinal() =>
            {
                placement::values_shard_index([source_id, target_id], self.shard_count)
                    .map(GraphShardRoute::Single)
            }
            _ => Ok(GraphShardRoute::FanOut),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aiondb_core::{ColumnId, Value};
    use aiondb_storage_api::ShardHashFunction;

    fn config(shard_count: u32) -> StorageShardConfig {
        StorageShardConfig {
            shard_key_columns: vec![ColumnId::new(1)],
            shard_count,
            hash_function: ShardHashFunction::Sha256,
            virtual_nodes_per_shard: 128,
        }
    }

    #[test]
    fn outgoing_lookup_routes_when_source_is_shard_key() {
        let spec =
            GraphShardSpec::new(&config(8), vec![0]).with_endpoints(GraphEdgeEndpoints::new(0, 1));

        let route = spec
            .route_adjacency(&Value::Int(42), true)
            .expect("route adjacency");

        match route {
            GraphShardRoute::Single(shard) => assert!(shard < 8),
            GraphShardRoute::FanOut => panic!("expected single-shard route"),
        }
    }

    #[test]
    fn incoming_lookup_routes_when_target_is_shard_key() {
        let spec =
            GraphShardSpec::new(&config(8), vec![1]).with_endpoints(GraphEdgeEndpoints::new(0, 1));

        let route = spec
            .route_adjacency(&Value::Text("node-7".to_owned()), false)
            .expect("route adjacency");

        match route {
            GraphShardRoute::Single(shard) => assert!(shard < 8),
            GraphShardRoute::FanOut => panic!("expected single-shard route"),
        }
    }

    #[test]
    fn mismatched_endpoint_falls_back_to_fanout() {
        let spec =
            GraphShardSpec::new(&config(8), vec![0]).with_endpoints(GraphEdgeEndpoints::new(0, 1));

        assert_eq!(
            spec.route_adjacency(&Value::Int(42), false).unwrap(),
            GraphShardRoute::FanOut
        );
    }

    #[test]
    fn multi_column_shard_key_falls_back_to_fanout() {
        let spec = GraphShardSpec::new(&config(8), vec![0, 1])
            .with_endpoints(GraphEdgeEndpoints::new(0, 1));

        assert_eq!(
            spec.route_adjacency(&Value::Int(42), true).unwrap(),
            GraphShardRoute::FanOut
        );
    }

    #[test]
    fn unregistered_edge_table_falls_back_to_fanout() {
        let spec = GraphShardSpec::new(&config(8), vec![0]);

        assert_eq!(
            spec.route_adjacency(&Value::Int(42), true).unwrap(),
            GraphShardRoute::FanOut
        );
    }

    #[test]
    fn exact_edge_lookup_routes_composite_endpoint_key() {
        let spec = GraphShardSpec::new(&config(8), vec![0, 1])
            .with_endpoints(GraphEdgeEndpoints::new(0, 1));

        let route = spec
            .route_edge(
                &Value::Text("alice".to_owned()),
                &Value::Text("bob".to_owned()),
            )
            .expect("route exact edge");

        match route {
            GraphShardRoute::Single(shard) => assert!(shard < 8),
            GraphShardRoute::FanOut => panic!("expected single-shard route"),
        }
    }

    #[test]
    fn exact_edge_lookup_falls_back_when_key_is_not_endpoint_aligned() {
        let spec = GraphShardSpec::new(&config(8), vec![2, 0])
            .with_endpoints(GraphEdgeEndpoints::new(0, 1));

        assert_eq!(
            spec.route_edge(&Value::Int(1), &Value::Int(2)).unwrap(),
            GraphShardRoute::FanOut
        );
    }
}
