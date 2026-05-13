use super::*;

// === Helpers ===

fn int_lit(v: i32) -> TypedExpr {
    TypedExpr::literal(Value::Int(v), DataType::Int, false)
}

fn text_lit(s: &str) -> TypedExpr {
    TypedExpr::literal(Value::Text(s.to_string()), DataType::Text, false)
}

fn col(name: &str, ordinal: usize) -> TypedExpr {
    TypedExpr::column_ref(name, ordinal, DataType::Int, false)
}

fn bool_col(name: &str, ordinal: usize) -> TypedExpr {
    TypedExpr::column_ref(name, ordinal, DataType::Boolean, false)
}

mod constructors;
mod operators_and_predicates;
mod traits_and_debug;
