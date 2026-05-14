//! Planner integration for graph traversals.
//!
//! Provides [`build_graph_plan`], which validates a [`TraversalSpec`] and
//! produces a [`GraphTraversalPlan`] that the executor can turn into
//! adjacency-index lookups.

use aiondb_core::{DbResult, RelationId};

use crate::traversal::{TraversalDirection, TraversalSpec};

/// A validated graph traversal plan, ready for execution.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GraphTraversalPlan {
    /// The starting node label.
    pub start_label: String,
    /// The edge label to traverse.
    pub edge_label: String,
    /// The target node label.
    pub end_label: String,
    /// The traversal direction.
    pub direction: TraversalDirection,
    /// Minimum hops.
    pub min_depth: u32,
    /// Maximum hops.
    pub max_depth: u32,
    /// The resolved edge table id.
    pub edge_table_id: RelationId,
}

/// Build a graph traversal plan from a [`TraversalSpec`].
///
/// The spec must have its `edge_table_id` resolved before calling this
/// function.
///
/// # Errors
///
/// Returns an error if the edge table id has not been resolved or if the
/// depth bounds are invalid.
pub fn build_graph_plan(spec: &TraversalSpec) -> DbResult<GraphTraversalPlan> {
    let edge_table_id = spec.edge_table_id.ok_or_else(|| {
        aiondb_core::DbError::internal("graph traversal requires a resolved edge table id")
    })?;

    if spec.max_depth < spec.min_depth {
        return Err(aiondb_core::DbError::internal(
            "graph traversal max_depth must be >= min_depth",
        ));
    }

    if spec.max_depth == 0 {
        return Err(aiondb_core::DbError::internal(
            "graph traversal max_depth must be > 0",
        ));
    }

    // Cap max_depth to prevent runaway traversals that exhaust memory/CPU.
    const MAX_ALLOWED_DEPTH: u32 = 1000;
    if spec.max_depth > MAX_ALLOWED_DEPTH {
        return Err(aiondb_core::DbError::bind_error(
            aiondb_core::SqlState::ProgramLimitExceeded,
            format!(
                "graph traversal max_depth ({}) exceeds maximum allowed ({})",
                spec.max_depth, MAX_ALLOWED_DEPTH
            ),
        ));
    }

    Ok(GraphTraversalPlan {
        start_label: spec.start_label.clone(),
        edge_label: spec.edge_label.clone(),
        end_label: spec.end_label.clone(),
        direction: spec.direction,
        min_depth: spec.min_depth,
        max_depth: spec.max_depth,
        edge_table_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolved_spec() -> TraversalSpec {
        TraversalSpec::single_hop("person", "knows", "person")
            .with_edge_table_id(RelationId::new(10))
    }

    #[test]
    fn build_plan_ok() {
        let spec = resolved_spec();
        let plan = build_graph_plan(&spec).unwrap();
        assert_eq!(plan.start_label, "person");
        assert_eq!(plan.edge_label, "knows");
        assert_eq!(plan.edge_table_id, RelationId::new(10));
        assert_eq!(plan.direction, TraversalDirection::Outgoing);
    }

    #[test]
    fn build_plan_unresolved() {
        let spec = TraversalSpec::single_hop("a", "e", "b");
        assert!(build_graph_plan(&spec).is_err());
    }

    #[test]
    fn build_plan_invalid_depth() {
        let mut spec = resolved_spec();
        spec.min_depth = 3;
        spec.max_depth = 1;
        assert!(build_graph_plan(&spec).is_err());
    }

    #[test]
    fn build_plan_zero_max_depth() {
        let mut spec = resolved_spec();
        spec.min_depth = 0;
        spec.max_depth = 0;
        assert!(build_graph_plan(&spec).is_err());
    }

    #[test]
    fn plan_clone_eq() {
        let spec = resolved_spec();
        let plan = build_graph_plan(&spec).unwrap();
        assert_eq!(plan, plan.clone());
    }

    #[test]
    fn plan_debug() {
        let spec = resolved_spec();
        let plan = build_graph_plan(&spec).unwrap();
        let dbg = format!("{plan:?}");
        assert!(dbg.contains("GraphTraversalPlan"));
    }
}
