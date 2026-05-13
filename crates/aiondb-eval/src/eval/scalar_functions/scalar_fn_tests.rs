use crate::eval::scalar_functions::*;

fn eval_generic(name: &str, args: &[Value]) -> DbResult<Value> {
    eval_scalar_function(&ScalarFunction::Generic(name.to_owned()), args)
}

#[test]
fn large_object_oid_allocator_rejects_exhaustion() {
    let mut registry = LargeObjectRegistry {
        next_oid: i32::MAX,
        ..LargeObjectRegistry::default()
    };
    registry.objects.insert(i32::MAX, Vec::new());

    let err = next_available_lo_oid(&registry).expect_err("OID space must be exhausted");
    assert!(err.to_string().contains("OID space exhausted"), "{err}");
}

#[test]
fn large_object_fd_allocator_rejects_exhaustion() {
    let mut session = LargeObjectSessionState {
        next_fd: i32::MAX,
        ..LargeObjectSessionState::default()
    };
    session.fds.insert(
        i32::MAX,
        LargeObjectFdState {
            oid: 1,
            position: 0,
        },
    );

    let err = next_available_lo_fd(&session).expect_err("descriptor space must be exhausted");
    assert!(
        err.to_string().contains("descriptor space exhausted"),
        "{err}"
    );
}

#[test]
fn large_object_write_range_rejects_oversized_result() {
    let err = checked_lo_write_range("lo_put", 0, MAX_COMPAT_LARGE_OBJECT_BYTES + 1)
        .expect_err("oversized large object must fail");
    assert!(err.to_string().contains("exceeds maximum"), "{err}");
}

#[test]
fn geometric_measure_area_handles_box_input() {
    let value = eval_generic("area", &[Value::Text("((0,0),(1,1))".to_owned())]).unwrap();
    assert_eq!(value, Value::Double(0.0));
}

#[test]
fn geometric_predicate_isclosed_handles_open_path() {
    let value = eval_generic("isclosed", &[Value::Text("[(0,0),(1,1)]".to_owned())]).unwrap();
    assert_eq!(value, Value::Boolean(false));
}

#[test]
fn unsupported_geometric_functions_preserve_null_semantics() {
    assert_eq!(
        eval_generic("npoints", &[Value::Null]).unwrap(),
        Value::Null
    );
}

#[test]
fn geometric_passthrough_functions_still_return_text() {
    let value = Value::Text("[(0,0),(1,1)]".to_owned());
    assert_eq!(
        eval_generic("center", std::slice::from_ref(&value)).unwrap(),
        value
    );
    assert_eq!(
        eval_generic("popen", std::slice::from_ref(&value)).unwrap(),
        value
    );
}

#[test]
fn pg_boolean_comparison_helpers_match_postgres_ordering() {
    assert_eq!(
        eval_generic("booleq", &[Value::Boolean(true), Value::Boolean(true)]).unwrap(),
        Value::Boolean(true)
    );
    assert_eq!(
        eval_generic("boolne", &[Value::Boolean(true), Value::Boolean(false)]).unwrap(),
        Value::Boolean(true)
    );
    assert_eq!(
        eval_generic("boollt", &[Value::Boolean(false), Value::Boolean(true)]).unwrap(),
        Value::Boolean(true)
    );
    assert_eq!(
        eval_generic("boolge", &[Value::Boolean(true), Value::Boolean(false)]).unwrap(),
        Value::Boolean(true)
    );
    assert_eq!(
        eval_generic("boolne", &[Value::Null, Value::Boolean(false)]).unwrap(),
        Value::Null
    );
}

#[test]
fn cypher_localtime_is_dispatched() {
    let result = eval_generic(
        "cypher_localtime",
        &[Value::Text("14:30:00+05:00".to_owned())],
    )
    .unwrap();
    assert_eq!(
        result,
        Value::Time(time::Time::from_hms(14, 30, 0).unwrap())
    );
}

#[test]
fn graph_path_length_is_dispatched() {
    let path = Value::Array(vec![Value::Int(1), Value::Int(10), Value::Int(2)]);
    let result = eval_generic("graph_path_length", &[path]).unwrap();
    assert_eq!(result, Value::BigInt(1));
}

#[test]
fn composite_field_temporal_property_is_dispatched() {
    let date = time::Date::from_calendar_date(2026, time::Month::March, 18).unwrap();
    let result = eval_generic(
        "__aiondb_composite_field",
        &[Value::Date(date), Value::Text("year".to_owned())],
    )
    .unwrap();
    assert_eq!(result, Value::BigInt(2026));
}

#[test]
fn multirange_overlap_generic_functions_are_dispatched() {
    let result = eval_generic(
        "multirange_overlaps_range",
        &[
            Value::Text("{[1,4),[8,10)}".to_owned()),
            Value::Text("[3,9)".to_owned()),
        ],
    )
    .unwrap();
    assert_eq!(result, Value::Boolean(true));
}

#[test]
fn multirange_contains_elem_generic_function_is_dispatched() {
    let result = eval_generic(
        "multirange_contains_elem",
        &[Value::Text("{[1,4),[8,10)}".to_owned()), Value::Int(9)],
    )
    .unwrap();
    assert_eq!(result, Value::Boolean(true));
}

#[test]
fn named_range_constructor_accepts_bounds() {
    let result = eval_generic(
        "textrange",
        &[Value::Text("a".to_owned()), Value::Text("c".to_owned())],
    )
    .unwrap();
    assert_eq!(result, Value::Text("[a,c)".to_owned()));
}

#[test]
fn named_range_constructor_quotes_array_bounds() {
    let range = eval_generic(
        "arrayrange",
        &[
            Value::Array(vec![Value::Int(1), Value::Int(2)]),
            Value::Array(vec![Value::Int(2), Value::Int(1)]),
        ],
    )
    .unwrap();
    assert_eq!(range, Value::Text("[\"{1,2}\",\"{2,1}\")".to_owned()));

    let multirange = eval_generic("arraymultirange", &[range]).unwrap();
    assert_eq!(
        multirange,
        Value::Text("{[\"{1,2}\",\"{2,1}\")}".to_owned())
    );
}

#[test]
fn named_range_constructor_rejects_descending_array_bounds() {
    let err = eval_generic(
        "arrayrange",
        &[
            Value::Array(vec![Value::Int(2), Value::Int(1)]),
            Value::Array(vec![Value::Int(1), Value::Int(2)]),
        ],
    )
    .expect_err("descending array bounds must fail");
    assert_eq!(
        err.sqlstate(),
        aiondb_core::SqlState::InvalidTextRepresentation
    );
    assert!(
        err.to_string()
            .contains("range lower bound must be less than or equal to range upper bound"),
        "unexpected error: {err}"
    );
}

#[test]
fn jsonb_agg_finalize_preserves_nulls_and_values() {
    let result = eval_generic(
        "__aiondb_jsonb_agg_finalize",
        &[Value::Array(vec![
            Value::Int(1),
            Value::Null,
            Value::Text("x".to_owned()),
        ])],
    )
    .unwrap();
    assert_eq!(result, Value::Jsonb(serde_json::json!([1, null, "x"])));
}

#[test]
fn jsonb_agg_ordered_finalize_uses_payload_last_element() {
    let result = eval_generic(
        "__aiondb_jsonb_agg_ordered_finalize",
        &[Value::Array(vec![
            Value::Jsonb(serde_json::json!([2, "b"])),
            Value::Jsonb(serde_json::json!([1, "a"])),
        ])],
    )
    .unwrap();
    assert_eq!(result, Value::Jsonb(serde_json::json!(["b", "a"])));
}

#[test]
fn jsonb_object_agg_finalize_builds_object() {
    let result = eval_generic(
        "__aiondb_jsonb_object_agg_finalize",
        &[Value::Array(vec![
            Value::Jsonb(serde_json::json!(["a", 1])),
            Value::Jsonb(serde_json::json!([2, true])),
        ])],
    )
    .unwrap();
    assert_eq!(result, Value::Jsonb(serde_json::json!({"a": 1, "2": true})));
}

#[test]
fn jsonb_object_agg_finalize_rejects_null_key() {
    let err = eval_generic(
        "__aiondb_jsonb_object_agg_finalize",
        &[Value::Array(vec![Value::Jsonb(serde_json::json!([
            null, 1
        ]))])],
    )
    .expect_err("null key must fail");
    assert_eq!(err.sqlstate(), aiondb_core::SqlState::NotNullViolation);
}
