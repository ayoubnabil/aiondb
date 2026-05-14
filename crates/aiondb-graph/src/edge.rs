//! Edge descriptors for the property graph model.
//!
//! An [`EdgeDescriptor`] enriches the catalog-level [`EdgeLabelDescriptor`]
//! with property metadata, providing everything the graph engine needs to
//! work with graph edges.

use aiondb_catalog::graph::EdgeLabelDescriptor;
use aiondb_core::{DataType, RelationId};

use crate::node::PropertyDescriptor;

/// Describes a graph edge type, including its label, backing table,
/// source/target label references, and exposed properties.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EdgeDescriptor {
    /// The underlying catalog edge label.
    pub label: EdgeLabelDescriptor,
    /// The name of the column containing the source node id.
    pub source_id_column: String,
    /// The name of the column containing the target node id.
    pub target_id_column: String,
    /// The properties exposed on this edge type.
    pub properties: Vec<PropertyDescriptor>,
}

impl EdgeDescriptor {
    /// Create a new edge descriptor with no properties.
    #[must_use]
    pub fn new(
        label: EdgeLabelDescriptor,
        source_id_column: impl Into<String>,
        target_id_column: impl Into<String>,
    ) -> Self {
        Self {
            label,
            source_id_column: source_id_column.into(),
            target_id_column: target_id_column.into(),
            properties: Vec::new(),
        }
    }

    /// The edge label name.
    #[must_use]
    pub fn label_name(&self) -> &str {
        &self.label.label
    }

    /// The backing table's relation id.
    #[must_use]
    pub fn table_id(&self) -> RelationId {
        self.label.table_id
    }

    /// The source node label name.
    #[must_use]
    pub fn source_label(&self) -> &str {
        &self.label.source_label
    }

    /// The target node label name.
    #[must_use]
    pub fn target_label(&self) -> &str {
        &self.label.target_label
    }

    /// Add a property to this descriptor.
    pub fn add_property(&mut self, name: impl Into<String>, data_type: DataType) {
        self.properties.push(PropertyDescriptor {
            name: name.into(),
            data_type,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_edge_label() -> EdgeLabelDescriptor {
        EdgeLabelDescriptor {
            label: "knows".to_owned(),
            table_id: RelationId::new(2),
            source_label: "person".to_owned(),
            target_label: "person".to_owned(),
            endpoints: None,
        }
    }

    #[test]
    fn new_edge_descriptor() {
        let desc = EdgeDescriptor::new(sample_edge_label(), "source_id", "target_id");
        assert_eq!(desc.label_name(), "knows");
        assert_eq!(desc.source_id_column, "source_id");
        assert_eq!(desc.target_id_column, "target_id");
        assert!(desc.properties.is_empty());
    }

    #[test]
    fn source_and_target_labels() {
        let desc = EdgeDescriptor::new(sample_edge_label(), "src", "tgt");
        assert_eq!(desc.source_label(), "person");
        assert_eq!(desc.target_label(), "person");
    }

    #[test]
    fn add_property() {
        let mut desc = EdgeDescriptor::new(sample_edge_label(), "src", "tgt");
        desc.add_property("weight", DataType::Double);
        assert_eq!(desc.properties.len(), 1);
        assert_eq!(desc.properties[0].name, "weight");
    }

    #[test]
    fn table_id() {
        let desc = EdgeDescriptor::new(sample_edge_label(), "src", "tgt");
        assert_eq!(desc.table_id(), RelationId::new(2));
    }

    #[test]
    fn clone_eq() {
        let desc = EdgeDescriptor::new(sample_edge_label(), "src", "tgt");
        assert_eq!(desc, desc.clone());
    }
}
