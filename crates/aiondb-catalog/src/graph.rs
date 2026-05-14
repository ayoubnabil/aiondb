//! Graph catalog descriptors for the property graph model.
//!
//! Node and edge labels are metadata markers that associate regular SQL tables
//! with graph semantics.  A node label marks a table as containing graph nodes;
//! an edge label marks a table as containing edges that connect nodes from a
//! source label to a target label.

use aiondb_core::RelationId;
use serde::{Deserialize, Serialize};

/// Describes a node label that is backed by a regular SQL table.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct NodeLabelDescriptor {
    /// The label name (e.g. "person").
    pub label: String,
    /// The backing table's relation id.
    pub table_id: RelationId,
}

/// Column names that store the source and target node identifiers in an edge
/// table.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct EdgeEndpoints {
    /// Column holding the source node id.
    pub source_id_column: String,
    /// Column holding the target node id.
    pub target_id_column: String,
}

/// Describes an edge label backed by a regular SQL table, linking
/// a source node label to a target node label.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct EdgeLabelDescriptor {
    /// The edge label name (e.g. "knows").
    pub label: String,
    /// The backing table's relation id.
    pub table_id: RelationId,
    /// The source node label name.
    pub source_label: String,
    /// The target node label name.
    pub target_label: String,
    /// Optional explicit endpoint column names.
    pub endpoints: Option<EdgeEndpoints>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_label_descriptor_clone_eq() {
        let desc = NodeLabelDescriptor {
            label: "person".to_owned(),
            table_id: RelationId::new(1),
        };
        assert_eq!(desc, desc.clone());
    }

    #[test]
    fn node_label_descriptor_debug() {
        let desc = NodeLabelDescriptor {
            label: "person".to_owned(),
            table_id: RelationId::new(1),
        };
        let dbg = format!("{desc:?}");
        assert!(dbg.contains("person"));
    }

    #[test]
    fn node_label_descriptor_ne_different_label() {
        let a = NodeLabelDescriptor {
            label: "person".to_owned(),
            table_id: RelationId::new(1),
        };
        let b = NodeLabelDescriptor {
            label: "place".to_owned(),
            table_id: RelationId::new(1),
        };
        assert_ne!(a, b);
    }

    #[test]
    fn node_label_descriptor_ne_different_table() {
        let a = NodeLabelDescriptor {
            label: "person".to_owned(),
            table_id: RelationId::new(1),
        };
        let b = NodeLabelDescriptor {
            label: "person".to_owned(),
            table_id: RelationId::new(2),
        };
        assert_ne!(a, b);
    }

    #[test]
    fn edge_label_descriptor_clone_eq() {
        let desc = EdgeLabelDescriptor {
            label: "knows".to_owned(),
            table_id: RelationId::new(2),
            source_label: "person".to_owned(),
            target_label: "person".to_owned(),
            endpoints: None,
        };
        assert_eq!(desc, desc.clone());
    }

    #[test]
    fn edge_label_descriptor_debug() {
        let desc = EdgeLabelDescriptor {
            label: "knows".to_owned(),
            table_id: RelationId::new(2),
            source_label: "person".to_owned(),
            target_label: "person".to_owned(),
            endpoints: None,
        };
        let dbg = format!("{desc:?}");
        assert!(dbg.contains("knows"));
        assert!(dbg.contains("person"));
    }

    #[test]
    fn edge_label_descriptor_ne_different_label() {
        let a = EdgeLabelDescriptor {
            label: "knows".to_owned(),
            table_id: RelationId::new(2),
            source_label: "person".to_owned(),
            target_label: "person".to_owned(),
            endpoints: None,
        };
        let b = EdgeLabelDescriptor {
            label: "likes".to_owned(),
            table_id: RelationId::new(2),
            source_label: "person".to_owned(),
            target_label: "person".to_owned(),
            endpoints: None,
        };
        assert_ne!(a, b);
    }

    #[test]
    fn edge_label_descriptor_ne_different_source() {
        let a = EdgeLabelDescriptor {
            label: "knows".to_owned(),
            table_id: RelationId::new(2),
            source_label: "person".to_owned(),
            target_label: "person".to_owned(),
            endpoints: None,
        };
        let b = EdgeLabelDescriptor {
            label: "knows".to_owned(),
            table_id: RelationId::new(2),
            source_label: "company".to_owned(),
            target_label: "person".to_owned(),
            endpoints: None,
        };
        assert_ne!(a, b);
    }
}
