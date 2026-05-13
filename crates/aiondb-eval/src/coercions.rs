use aiondb_core::{DataType, DbResult, Value};

pub fn coerce_value(value: Value, target: &DataType) -> DbResult<Value> {
    crate::eval::cast::cast_value(value, target)
}

#[cfg(test)]
#[path = "coercions/basic_tests.rs"]
mod basic_tests;

#[cfg(test)]
#[path = "coercions/edge_tests.rs"]
mod edge_tests;
