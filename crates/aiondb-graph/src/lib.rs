//! Node/edge property graph model, label and property descriptors, and
//! traversal planning.
//!
//! This crate centralizes all graph-specific logic:
//!
//! - [`model`] defines the core [`GraphLabelDescriptor`] enum that unifies
//!   node and edge labels.
//! - [`node`] provides [`NodeDescriptor`] with property metadata for graph
//!   nodes.
//! - [`edge`] provides [`EdgeDescriptor`] with source/target and property
//!   metadata for graph edges.
//! - [`traversal`] defines [`TraversalSpec`] for specifying graph traversal
//!   patterns.
//! - [`planner`] provides [`build_graph_plan`] for integrating graph
//!   traversals into the query planner.

#![allow(
    clippy::cast_lossless,
    clippy::cast_precision_loss,
    clippy::items_after_statements,
    clippy::match_wildcard_for_single_variants,
    clippy::missing_errors_doc,
    clippy::must_use_candidate,
    clippy::needless_pass_by_value,
    clippy::similar_names,
    clippy::too_many_lines,
    clippy::unnecessary_wraps
)]

pub mod algorithms;
pub mod edge;
pub mod model;
pub mod node;
pub mod path;
pub mod pattern;
pub mod planner;
pub mod traversal;

// Re-export primary public types.
pub use aiondb_graph_api::{
    GraphDirection, GraphProjection, GraphProjectionAdapter, GraphStats, GraphStorage, GraphViewV2,
    HybridGraphPlan, HybridGraphSource, NeighborCursor, OwnedCursor, ProjectionSnapshot,
    RefreshPolicy, SliceCursor, WeightedNeighbor,
};
pub use edge::EdgeDescriptor;
pub use model::GraphLabelDescriptor;
pub use node::NodeDescriptor;
pub use path::{all_paths, shortest_path};
pub use pattern::{
    match_pattern, Binding, BoundValue, MatchPattern, MatchResult, NodeMatchSpec, PathElement,
    PatternStep, RelMatchSpec, RowProvider,
};
pub use planner::build_graph_plan;
pub use traversal::TraversalSpec;

#[cfg(test)]
mod pattern_tests;
