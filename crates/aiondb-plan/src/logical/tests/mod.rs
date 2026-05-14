use super::*;
use crate::dml::UpdateAssignment;
use crate::shared::{ColumnPlan, IndexColumnPlan, ProjectionExpr};
use crate::TypedExpr;
use aiondb_core::{ColumnId, DataType, IndexId, RelationId, SequenceId, Value};

// === Helpers ===

fn make_field(name: &str, dt: DataType, nullable: bool) -> ResultField {
    ResultField {
        name: name.to_string(),
        data_type: dt,
        text_type_modifier: None,
        nullable,
    }
}

fn make_projection(name: &str, dt: DataType, nullable: bool) -> ProjectionExpr {
    ProjectionExpr {
        field: make_field(name, dt.clone(), nullable),
        expr: TypedExpr::column_ref(name, 0, dt, nullable),
    }
}

mod clone_eq;
mod debug_and_edges;
mod output_fields;
