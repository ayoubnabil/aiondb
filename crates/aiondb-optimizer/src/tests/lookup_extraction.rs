use super::*;

// -------------------------------------------------------------------
// extract_index_lookup_value: BinaryEq with column and literal
// -------------------------------------------------------------------

#[test]
fn extract_lookup_binary_eq_column_left_literal_right() {
    let table = make_table_descriptor();
    let filter = TypedExpr::binary_eq(
        TypedExpr::column_ref("id", 0, DataType::Int, false),
        TypedExpr::literal(Value::Int(42), DataType::Int, false),
    );
    let result = extract_index_lookup_value(&filter, &table, ColumnId::new(10));
    assert_eq!(result, Some(Value::Int(42)));
}

// -------------------------------------------------------------------
// extract_index_lookup_value: BinaryEq with literal and column (reversed)
// -------------------------------------------------------------------

#[test]
fn extract_lookup_binary_eq_literal_left_column_right() {
    let table = make_table_descriptor();
    let filter = TypedExpr::binary_eq(
        TypedExpr::literal(Value::Int(99), DataType::Int, false),
        TypedExpr::column_ref("id", 0, DataType::Int, false),
    );
    let result = extract_index_lookup_value(&filter, &table, ColumnId::new(10));
    assert_eq!(result, Some(Value::Int(99)));
}

// -------------------------------------------------------------------
// extract_index_lookup_value: LogicalAnd containing BinaryEq
// -------------------------------------------------------------------

#[test]
fn extract_lookup_logical_and_with_eq_on_left() {
    let table = make_table_descriptor();
    let eq_filter = TypedExpr::binary_eq(
        TypedExpr::column_ref("id", 0, DataType::Int, false),
        TypedExpr::literal(Value::Int(7), DataType::Int, false),
    );
    let other = TypedExpr::binary_gt(
        TypedExpr::column_ref("name", 1, DataType::Text, true),
        TypedExpr::literal(Value::Text("a".to_owned()), DataType::Text, false),
    );
    let filter = TypedExpr::logical_and(eq_filter, other);
    let result = extract_index_lookup_value(&filter, &table, ColumnId::new(10));
    assert_eq!(result, Some(Value::Int(7)));
}

#[test]
fn extract_lookup_logical_and_with_eq_on_right() {
    let table = make_table_descriptor();
    let other = TypedExpr::binary_gt(
        TypedExpr::column_ref("name", 1, DataType::Text, true),
        TypedExpr::literal(Value::Text("a".to_owned()), DataType::Text, false),
    );
    let eq_filter = TypedExpr::binary_eq(
        TypedExpr::column_ref("id", 0, DataType::Int, false),
        TypedExpr::literal(Value::Int(13), DataType::Int, false),
    );
    let filter = TypedExpr::logical_and(other, eq_filter);
    let result = extract_index_lookup_value(&filter, &table, ColumnId::new(10));
    assert_eq!(result, Some(Value::Int(13)));
}

// -------------------------------------------------------------------
// extract_index_lookup_value: column matches index key column
// -------------------------------------------------------------------

#[test]
fn extract_lookup_column_matches_index_key() {
    let table = make_table_descriptor();
    let filter = TypedExpr::binary_eq(
        TypedExpr::column_ref("id", 0, DataType::Int, false),
        TypedExpr::literal(Value::Int(1), DataType::Int, false),
    );
    let result = extract_index_lookup_value(&filter, &table, ColumnId::new(10));
    assert!(result.is_some());
}

// -------------------------------------------------------------------
// extract_index_lookup_value: column does NOT match index key -> None
// -------------------------------------------------------------------

#[test]
fn extract_lookup_column_does_not_match_key() {
    let table = make_table_descriptor();
    let filter = TypedExpr::binary_eq(
        TypedExpr::column_ref("id", 0, DataType::Int, false),
        TypedExpr::literal(Value::Int(1), DataType::Int, false),
    );
    let result = extract_index_lookup_value(&filter, &table, ColumnId::new(999));
    assert_eq!(result, None);
}

// -------------------------------------------------------------------
// extract_index_lookup_value: NULL literal -> None (skipped)
// -------------------------------------------------------------------

#[test]
fn extract_lookup_null_literal_returns_none() {
    let table = make_table_descriptor();
    let filter = TypedExpr::binary_eq(
        TypedExpr::column_ref("id", 0, DataType::Int, false),
        TypedExpr::literal(Value::Null, DataType::Int, true),
    );
    let result = extract_index_lookup_value(&filter, &table, ColumnId::new(10));
    assert_eq!(result, None);
}

// -------------------------------------------------------------------
// extract_index_lookup_value: non-Eq filter -> None
// -------------------------------------------------------------------

#[test]
fn extract_lookup_binary_gt_returns_none() {
    let table = make_table_descriptor();
    let filter = TypedExpr::binary_gt(
        TypedExpr::column_ref("id", 0, DataType::Int, false),
        TypedExpr::literal(Value::Int(5), DataType::Int, false),
    );
    let result = extract_index_lookup_value(&filter, &table, ColumnId::new(10));
    assert_eq!(result, None);
}

#[test]
fn extract_lookup_binary_ne_returns_none() {
    let table = make_table_descriptor();
    let filter = TypedExpr::binary_ne(
        TypedExpr::column_ref("id", 0, DataType::Int, false),
        TypedExpr::literal(Value::Int(5), DataType::Int, false),
    );
    let result = extract_index_lookup_value(&filter, &table, ColumnId::new(10));
    assert_eq!(result, None);
}

#[test]
fn extract_lookup_binary_lt_returns_none() {
    let table = make_table_descriptor();
    let filter = TypedExpr::binary_lt(
        TypedExpr::column_ref("id", 0, DataType::Int, false),
        TypedExpr::literal(Value::Int(5), DataType::Int, false),
    );
    let result = extract_index_lookup_value(&filter, &table, ColumnId::new(10));
    assert_eq!(result, None);
}

#[test]
fn extract_lookup_logical_or_returns_none() {
    let table = make_table_descriptor();
    let left = TypedExpr::binary_eq(
        TypedExpr::column_ref("id", 0, DataType::Int, false),
        TypedExpr::literal(Value::Int(1), DataType::Int, false),
    );
    let right = TypedExpr::binary_eq(
        TypedExpr::column_ref("id", 0, DataType::Int, false),
        TypedExpr::literal(Value::Int(2), DataType::Int, false),
    );
    let filter = TypedExpr::logical_or(left, right);
    let result = extract_index_lookup_value(&filter, &table, ColumnId::new(10));
    assert_eq!(result, None);
}

#[test]
fn extract_lookup_logical_not_returns_none() {
    let table = make_table_descriptor();
    let inner = TypedExpr::binary_eq(
        TypedExpr::column_ref("id", 0, DataType::Int, false),
        TypedExpr::literal(Value::Int(1), DataType::Int, false),
    );
    let filter = TypedExpr::logical_not(inner);
    let result = extract_index_lookup_value(&filter, &table, ColumnId::new(10));
    assert_eq!(result, None);
}

// -------------------------------------------------------------------
// extract_index_lookup_value: literal on both sides -> None
// -------------------------------------------------------------------

#[test]
fn extract_lookup_both_literals_returns_none() {
    let table = make_table_descriptor();
    let filter = TypedExpr::binary_eq(
        TypedExpr::literal(Value::Int(1), DataType::Int, false),
        TypedExpr::literal(Value::Int(1), DataType::Int, false),
    );
    let result = extract_index_lookup_value(&filter, &table, ColumnId::new(10));
    assert_eq!(result, None);
}

// -------------------------------------------------------------------
// extract_index_lookup_value: column ordinal out of bounds -> None
// -------------------------------------------------------------------

#[test]
fn extract_lookup_column_ordinal_out_of_bounds() {
    let table = make_table_descriptor();
    let filter = TypedExpr::binary_eq(
        TypedExpr::column_ref("phantom", 999, DataType::Int, false),
        TypedExpr::literal(Value::Int(1), DataType::Int, false),
    );
    let result = extract_index_lookup_value(&filter, &table, ColumnId::new(10));
    assert_eq!(result, None);
}

// -------------------------------------------------------------------
// extract_index_lookup_value with text value
// -------------------------------------------------------------------

#[test]
fn extract_lookup_text_value() {
    let table = make_table_descriptor();
    let filter = TypedExpr::binary_eq(
        TypedExpr::column_ref("name", 1, DataType::Text, true),
        TypedExpr::literal(Value::Text("alice".to_owned()), DataType::Text, false),
    );
    let result = extract_index_lookup_value(&filter, &table, ColumnId::new(20));
    assert_eq!(result, Some(Value::Text("alice".to_owned())));
}

// -------------------------------------------------------------------
// Nested LogicalAnd -> finds deepest match
// -------------------------------------------------------------------

#[test]
fn extract_lookup_nested_and_finds_match() {
    let table = make_table_descriptor();
    let inner_eq = TypedExpr::binary_eq(
        TypedExpr::column_ref("id", 0, DataType::Int, false),
        TypedExpr::literal(Value::Int(55), DataType::Int, false),
    );
    let other1 = TypedExpr::binary_gt(
        TypedExpr::column_ref("name", 1, DataType::Text, true),
        TypedExpr::literal(Value::Text("z".to_owned()), DataType::Text, false),
    );
    let other2 = TypedExpr::binary_lt(
        TypedExpr::column_ref("name", 1, DataType::Text, true),
        TypedExpr::literal(Value::Text("m".to_owned()), DataType::Text, false),
    );
    let inner_and = TypedExpr::logical_and(inner_eq, other1);
    let outer_and = TypedExpr::logical_and(inner_and, other2);
    let result = extract_index_lookup_value(&outer_and, &table, ColumnId::new(10));
    assert_eq!(result, Some(Value::Int(55)));
}
