use super::*;
use aiondb_core::NumericValue;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

#[test]
fn extract_inner_hash_join_spec_accepts_column_equalities_and_conjunctions() {
    let condition = TypedExpr::logical_and(
        TypedExpr::binary_eq(
            TypedExpr::column_ref("a_id", 0, DataType::Int, false),
            TypedExpr::column_ref("b_id", 2, DataType::Int, false),
        ),
        TypedExpr::binary_eq(
            TypedExpr::column_ref("a_tenant", 1, DataType::Int, false),
            TypedExpr::column_ref("b_tenant", 3, DataType::Int, false),
        ),
    );

    let Some(spec) = extract_inner_hash_join_spec(Some(&condition), 2, 2) else {
        panic!("expected hash join spec for equality conjunction");
    };
    assert_eq!(spec.left_ordinals, vec![0, 1]);
    assert_eq!(spec.right_ordinals, vec![0, 1]);
}

#[test]
fn extract_inner_hash_join_spec_rejects_non_equality_predicates() {
    let condition = TypedExpr::binary_gt(
        TypedExpr::column_ref("a_id", 0, DataType::Int, false),
        TypedExpr::column_ref("b_id", 1, DataType::Int, false),
    );

    assert!(extract_inner_hash_join_spec(Some(&condition), 1, 1).is_none());
}

#[test]
fn extract_inner_hash_join_spec_accepts_exact_numeric_mixed_types() {
    let condition = TypedExpr::binary_eq(
        TypedExpr::column_ref("a_id", 0, DataType::Int, false),
        TypedExpr::column_ref("b_id", 1, DataType::BigInt, false),
    );

    let Some(spec) = extract_inner_hash_join_spec(Some(&condition), 1, 1) else {
        panic!("expected hash join spec for exact numeric mixed types");
    };
    assert_eq!(spec.left_ordinals, vec![0]);
    assert_eq!(spec.right_ordinals, vec![0]);
}

#[test]
fn extract_inner_hash_join_spec_accepts_exact_numeric_cast_column_refs() {
    let condition = TypedExpr::binary_eq(
        TypedExpr::cast(
            TypedExpr::column_ref("a_id", 0, DataType::Int, false),
            DataType::BigInt,
        ),
        TypedExpr::column_ref("b_id", 1, DataType::BigInt, false),
    );

    let Some(spec) = extract_inner_hash_join_spec(Some(&condition), 1, 1) else {
        panic!("expected hash join spec for casted exact numeric column ref");
    };
    assert_eq!(spec.left_ordinals, vec![0]);
    assert_eq!(spec.right_ordinals, vec![0]);
}

#[test]
fn hash_dedup_rows_honors_cancellation_checker() {
    let mut rows: Vec<Row> = (0..128)
        .map(|i| Row::new(vec![Value::Int(i), Value::Int(i % 8)]))
        .collect();

    let checks = Arc::new(AtomicUsize::new(0));
    let cancellation_checker = {
        let checks = checks.clone();
        Arc::new(move || {
            let seen = checks.fetch_add(1, Ordering::Relaxed);
            if seen >= 10 {
                Err(DbError::query_canceled("session canceled"))
            } else {
                Ok(())
            }
        })
    };
    let context = ExecutionContext::default().with_cancellation_checker(cancellation_checker);

    let error = hash_dedup_rows(&mut rows, &context)
        .expect_err("hash_dedup_rows should stop when cancellation fires");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::QueryCanceled);
    assert!(
        checks.load(Ordering::Relaxed) > 10,
        "hash_dedup_rows should poll while deduplicating"
    );
}

#[test]
fn extract_inner_hash_join_spec_rejects_non_hash_safe_mixed_types() {
    let condition = TypedExpr::binary_eq(
        TypedExpr::column_ref("a_value", 0, DataType::Text, false),
        TypedExpr::column_ref("b_value", 1, DataType::Int, false),
    );

    assert!(extract_inner_hash_join_spec(Some(&condition), 1, 1).is_none());
}

#[test]
fn build_join_hash_key_component_canonicalizes_exact_numeric_values() {
    let int_key = build_join_hash_key_component(&Value::Int(1)).unwrap();
    let bigint_key = build_join_hash_key_component(&Value::BigInt(1)).unwrap();
    let numeric_key =
        build_join_hash_key_component(&Value::Numeric(NumericValue::new(100, 2))).unwrap();
    let scaled_numeric_key =
        build_join_hash_key_component(&Value::Numeric(NumericValue::new(1, 0))).unwrap();

    assert_eq!(int_key, bigint_key);
    assert_eq!(bigint_key, numeric_key);
    assert_eq!(numeric_key, scaled_numeric_key);
}
