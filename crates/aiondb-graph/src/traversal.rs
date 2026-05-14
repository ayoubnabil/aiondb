//! Graph traversal specifications.
//!
//! [`TraversalSpec`] describes a graph traversal pattern that the planner
//! can translate into a physical plan.  It supports both outgoing and
//! incoming edge traversals with configurable depth bounds.

use aiondb_core::RelationId;

/// Direction of edge traversal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TraversalDirection {
    /// Follow outgoing edges (source -> target).
    Outgoing,
    /// Follow incoming edges (target -> source).
    Incoming,
    /// Follow edges in either direction.
    Both,
}

/// Specifies a graph traversal pattern.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TraversalSpec {
    /// The starting node label name.
    pub start_label: String,
    /// The edge label to traverse.
    pub edge_label: String,
    /// The target node label name (the label at the other end of the edge).
    pub end_label: String,
    /// The traversal direction.
    pub direction: TraversalDirection,
    /// The minimum traversal depth (number of hops). Defaults to 1.
    pub min_depth: u32,
    /// The maximum traversal depth (number of hops). Defaults to 1.
    /// Use a larger value for multi-hop traversals.
    pub max_depth: u32,
    /// The backing edge table id, if already resolved.
    pub edge_table_id: Option<RelationId>,
}

impl TraversalSpec {
    /// Create a single-hop outgoing traversal.
    #[must_use]
    pub fn single_hop(
        start_label: impl Into<String>,
        edge_label: impl Into<String>,
        end_label: impl Into<String>,
    ) -> Self {
        Self {
            start_label: start_label.into(),
            edge_label: edge_label.into(),
            end_label: end_label.into(),
            direction: TraversalDirection::Outgoing,
            min_depth: 1,
            max_depth: 1,
            edge_table_id: None,
        }
    }

    /// Create a multi-hop outgoing traversal.
    #[must_use]
    pub fn multi_hop(
        start_label: impl Into<String>,
        edge_label: impl Into<String>,
        end_label: impl Into<String>,
        min_depth: u32,
        max_depth: u32,
    ) -> Self {
        Self {
            start_label: start_label.into(),
            edge_label: edge_label.into(),
            end_label: end_label.into(),
            direction: TraversalDirection::Outgoing,
            min_depth,
            max_depth,
            edge_table_id: None,
        }
    }

    /// Set the traversal direction.
    #[must_use]
    pub fn with_direction(mut self, direction: TraversalDirection) -> Self {
        self.direction = direction;
        self
    }

    /// Set the resolved edge table id.
    #[must_use]
    pub fn with_edge_table_id(mut self, table_id: RelationId) -> Self {
        self.edge_table_id = Some(table_id);
        self
    }

    /// Return `true` if this is a single-hop traversal.
    #[must_use]
    pub fn is_single_hop(&self) -> bool {
        self.min_depth == 1 && self.max_depth == 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_hop_defaults() {
        let spec = TraversalSpec::single_hop("person", "knows", "person");
        assert_eq!(spec.start_label, "person");
        assert_eq!(spec.edge_label, "knows");
        assert_eq!(spec.end_label, "person");
        assert_eq!(spec.direction, TraversalDirection::Outgoing);
        assert_eq!(spec.min_depth, 1);
        assert_eq!(spec.max_depth, 1);
        assert!(spec.is_single_hop());
        assert!(spec.edge_table_id.is_none());
    }

    #[test]
    fn multi_hop() {
        let spec = TraversalSpec::multi_hop("person", "knows", "person", 1, 3);
        assert_eq!(spec.min_depth, 1);
        assert_eq!(spec.max_depth, 3);
        assert!(!spec.is_single_hop());
    }

    #[test]
    fn with_direction() {
        let spec =
            TraversalSpec::single_hop("a", "e", "b").with_direction(TraversalDirection::Incoming);
        assert_eq!(spec.direction, TraversalDirection::Incoming);
    }

    #[test]
    fn with_edge_table_id() {
        let spec = TraversalSpec::single_hop("a", "e", "b").with_edge_table_id(RelationId::new(5));
        assert_eq!(spec.edge_table_id, Some(RelationId::new(5)));
    }

    #[test]
    fn clone_eq() {
        let spec = TraversalSpec::single_hop("a", "e", "b");
        assert_eq!(spec, spec.clone());
    }

    #[test]
    fn direction_variants() {
        assert_ne!(TraversalDirection::Outgoing, TraversalDirection::Incoming);
        assert_ne!(TraversalDirection::Incoming, TraversalDirection::Both);
        assert_ne!(TraversalDirection::Outgoing, TraversalDirection::Both);
    }
}
