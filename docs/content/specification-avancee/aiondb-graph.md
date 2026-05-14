---
title: aiondb-graph
order: 41
---

# aiondb-graph

Property graph model and graph-algorithm library. Defines node and edge label descriptors, traversal and pattern-match specifications, the planner entry point used by the engine for graph queries, and a set of algorithms that operate on the `GraphView` trait abstraction.

## cargo

```toml
[dependencies]
aiondb-graph = { path = "../aiondb-graph" }
```

## modules

| module | purpose |
|---|---|
| `model` | unified `GraphLabelDescriptor` (node or edge). |
| `node` | `NodeDescriptor` for graph nodes. |
| `edge` | `EdgeDescriptor` for graph edges. |
| `traversal` | `TraversalSpec` and `TraversalDirection`. |
| `pattern` | `MatchPattern`, `RowProvider`, and the pattern matcher. |
| `path` | shortest-path and all-paths algorithms. |
| `planner` | `build_graph_plan` planner entry point. |
| `algorithms` | algorithm library on top of the `GraphView` trait. |

## algorithms

| module | functions |
|---|---|
| `pagerank` | `pagerank`, `pagerank_default`, `PageRankConfig`. |
| `connected_components` | `connected_components`, `count_components`, `strongly_connected_components`. |
| `community` | `louvain`, `louvain_with_config`, `modularity`, `LouvainConfig`. |
| `centrality` | `betweenness_centrality`, `betweenness_centrality_normalized`, `closeness_centrality`. |
| `degree` | `degree_centrality`, `in_degree_centrality`, `out_degree_centrality`, `degree_distribution`. |
| `triangle` | `triangle_count`, `node_triangle_count`, `local_clustering_coefficient`, `global_clustering_coefficient`. |
| `similarity` | `jaccard_similarity`, `overlap_coefficient`, `adamic_adar`, `common_neighbors`, `top_k_similar`. |
| `kcore` | `core_numbers`, `degeneracy`. |
| `procedures` | `list_procedures`, `execute_algorithm`, `AlgorithmConfig`, `ProcedureInfo`. |

## key types

| item | description |
|---|---|
| `GraphLabelDescriptor` | enum wrapping `NodeLabelDescriptor` or `EdgeLabelDescriptor`. |
| `NodeDescriptor` | property metadata for a graph node label. |
| `EdgeDescriptor` | source/target plus property metadata for an edge label. |
| `TraversalSpec`, `TraversalDirection` | direction and depth bounds for traversals. |
| `MatchPattern`, `PatternStep`, `NodeMatchSpec`, `RelMatchSpec` | pattern-match query shape. |
| `Binding`, `BoundValue`, `MatchResult`, `PathElement` | pattern-match output. |
| `RowProvider` | trait used by pattern and path algorithms to fetch rows and adjacency. |
| `match_pattern` | execute a `MatchPattern` against a `RowProvider`. |
| `shortest_path`, `all_paths` | graph path algorithms over `RowProvider`. |
| `build_graph_plan` | planner entry point for graph queries. |
| `algorithms::GraphView`, `algorithms::AdjacencyGraph` | abstract graph trait and a simple in-memory implementation. |

## Fabric sharding

Graph Fabric routing lives in `aiondb-shard::fabric` so graph placement stays separate from generic row storage. `GraphShardSpec::route_adjacency` routes outgoing or incoming adjacency to one shard when the edge shard key is exactly the requested endpoint, and `ShardedStorage` uses that path before calling adjacency indexes on physical shards. The sharded storage registry caches the Fabric spec and updates it from the real `register_edge_table(source, target)` ordinals, so edge tables whose source/target columns are not the first two columns still route correctly. `GraphShardSpec::route_edge` routes exact edge lookups when source and target are known, including composite `(source_id, target_id)` shard keys, and falls back to fan-out when the endpoint mapping cannot prove shard locality.

## example

```rust
use aiondb_graph::algorithms::{pagerank_default, AdjacencyGraph};

let mut graph = AdjacencyGraph::new(4);
graph.add_edge(0, 1);
graph.add_edge(1, 2);
graph.add_edge(2, 0);
graph.add_edge(3, 0);

let scores = pagerank_default(&graph);
assert_eq!(scores.len(), 4);
```
