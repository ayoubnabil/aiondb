//! Node descriptors for the property graph model.
//!
//! A [`NodeDescriptor`] enriches the catalog-level [`NodeLabelDescriptor`]
//! with property metadata, providing everything the graph engine needs to
//! work with graph nodes.

use aiondb_catalog::graph::NodeLabelDescriptor;
use aiondb_core::{DataType, RelationId};

/// A property descriptor for a graph node or edge.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PropertyDescriptor {
    /// The property name.
    pub name: String,
    /// The property data type.
    pub data_type: DataType,
}

/// Describes a graph node type, including its label, backing table, and
/// the properties (columns) that are exposed as graph properties.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NodeDescriptor {
    /// The underlying catalog node label.
    pub label: NodeLabelDescriptor,
    /// The name of the column that serves as the node identity key.
    pub id_column: String,
    /// The properties exposed on this node type.
    pub properties: Vec<PropertyDescriptor>,
}

impl NodeDescriptor {
    /// Create a new node descriptor with no properties.
    #[must_use]
    pub fn new(label: NodeLabelDescriptor, id_column: impl Into<String>) -> Self {
        Self {
            label,
            id_column: id_column.into(),
            properties: Vec::new(),
        }
    }

    /// The label name.
    #[must_use]
    pub fn label_name(&self) -> &str {
        &self.label.label
    }

    /// The backing table's relation id.
    #[must_use]
    pub fn table_id(&self) -> RelationId {
        self.label.table_id
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

    fn sample_node_label() -> NodeLabelDescriptor {
        NodeLabelDescriptor {
            label: "person".to_owned(),
            table_id: RelationId::new(1),
        }
    }

    #[test]
    fn new_node_descriptor() {
        let desc = NodeDescriptor::new(sample_node_label(), "id");
        assert_eq!(desc.label_name(), "person");
        assert_eq!(desc.id_column, "id");
        assert!(desc.properties.is_empty());
    }

    #[test]
    fn add_property() {
        let mut desc = NodeDescriptor::new(sample_node_label(), "id");
        desc.add_property("name", DataType::Text);
        desc.add_property("age", DataType::Int);
        assert_eq!(desc.properties.len(), 2);
        assert_eq!(desc.properties[0].name, "name");
        assert_eq!(desc.properties[1].data_type, DataType::Int);
    }

    #[test]
    fn table_id() {
        let desc = NodeDescriptor::new(sample_node_label(), "id");
        assert_eq!(desc.table_id(), RelationId::new(1));
    }

    #[test]
    fn clone_eq() {
        let mut desc = NodeDescriptor::new(sample_node_label(), "id");
        desc.add_property("name", DataType::Text);
        assert_eq!(desc, desc.clone());
    }
}
