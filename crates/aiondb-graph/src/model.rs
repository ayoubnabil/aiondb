//! Core graph model types.
//!
//! [`GraphLabelDescriptor`] is the unified enum that wraps both node and edge
//! label descriptors from the catalog, providing a single type for code that
//! needs to work with either kind of label.

use aiondb_catalog::graph::{EdgeLabelDescriptor, NodeLabelDescriptor};

/// A unified descriptor that can represent either a node label or an edge
/// label in the property graph model.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GraphLabelDescriptor {
    /// A node label backed by a table.
    Node(NodeLabelDescriptor),
    /// An edge label backed by a table, connecting source and target node
    /// labels.
    Edge(EdgeLabelDescriptor),
}

impl GraphLabelDescriptor {
    /// Return the label name regardless of whether this is a node or edge
    /// label.
    #[must_use]
    pub fn label_name(&self) -> &str {
        match self {
            Self::Node(n) => &n.label,
            Self::Edge(e) => &e.label,
        }
    }

    /// Return `true` if this is a node label.
    #[must_use]
    pub const fn is_node(&self) -> bool {
        matches!(self, Self::Node(_))
    }

    /// Return `true` if this is an edge label.
    #[must_use]
    pub const fn is_edge(&self) -> bool {
        matches!(self, Self::Edge(_))
    }
}

impl From<NodeLabelDescriptor> for GraphLabelDescriptor {
    fn from(desc: NodeLabelDescriptor) -> Self {
        Self::Node(desc)
    }
}

impl From<EdgeLabelDescriptor> for GraphLabelDescriptor {
    fn from(desc: EdgeLabelDescriptor) -> Self {
        Self::Edge(desc)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aiondb_core::RelationId;

    fn sample_node() -> NodeLabelDescriptor {
        NodeLabelDescriptor {
            label: "person".to_owned(),
            table_id: RelationId::new(1),
        }
    }

    fn sample_edge() -> EdgeLabelDescriptor {
        EdgeLabelDescriptor {
            label: "knows".to_owned(),
            table_id: RelationId::new(2),
            source_label: "person".to_owned(),
            target_label: "person".to_owned(),
            endpoints: None,
        }
    }

    #[test]
    fn node_label_name() {
        let desc = GraphLabelDescriptor::Node(sample_node());
        assert_eq!(desc.label_name(), "person");
    }

    #[test]
    fn edge_label_name() {
        let desc = GraphLabelDescriptor::Edge(sample_edge());
        assert_eq!(desc.label_name(), "knows");
    }

    #[test]
    fn is_node() {
        let desc = GraphLabelDescriptor::Node(sample_node());
        assert!(desc.is_node());
        assert!(!desc.is_edge());
    }

    #[test]
    fn is_edge() {
        let desc = GraphLabelDescriptor::Edge(sample_edge());
        assert!(desc.is_edge());
        assert!(!desc.is_node());
    }

    #[test]
    fn from_node_label() {
        let node = sample_node();
        let desc: GraphLabelDescriptor = node.into();
        assert!(desc.is_node());
    }

    #[test]
    fn from_edge_label() {
        let edge = sample_edge();
        let desc: GraphLabelDescriptor = edge.into();
        assert!(desc.is_edge());
    }

    #[test]
    fn clone_eq() {
        let desc = GraphLabelDescriptor::Node(sample_node());
        assert_eq!(desc, desc.clone());
    }

    #[test]
    fn debug_format() {
        let desc = GraphLabelDescriptor::Node(sample_node());
        let dbg = format!("{desc:?}");
        assert!(dbg.contains("person"));
    }
}
