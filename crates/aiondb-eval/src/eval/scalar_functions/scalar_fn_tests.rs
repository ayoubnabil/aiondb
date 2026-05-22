use crate::eval::scalar_functions::*;
use aiondb_core::VectorValue;

fn eval_generic(name: &str, args: &[Value]) -> DbResult<Value> {
    eval_scalar_function(&ScalarFunction::Generic(name.to_owned()), args)
}

#[test]
fn pgvector_io_functions_parse_and_format_vectors() {
    let vector = eval_generic("vector_in", &[Value::Text("[1.0,0.0,2.5]".to_owned())]).unwrap();
    assert_eq!(
        vector,
        Value::Vector(VectorValue::new(3, vec![1.0, 0.0, 2.5]))
    );
    assert_eq!(
        eval_generic("vector_out", &[vector]).unwrap(),
        Value::Text("[1,0,2.5]".to_owned())
    );
    assert_eq!(
        eval_generic(
            "vector_in",
            &[
                Value::Text("[1.0,0.0,2.5]".to_owned()),
                Value::Int(0),
                Value::Int(3),
            ],
        )
        .unwrap(),
        Value::Vector(VectorValue::new(3, vec![1.0, 0.0, 2.5]))
    );
    assert!(eval_generic(
        "vector_in",
        &[Value::Text("[1.0,0.0,2.5]".to_owned()), Value::Int(0)],
    )
    .is_err());

    let sparse = eval_generic("sparsevec_in", &[Value::Text("{1:1,3:2.5}/4".to_owned())]).unwrap();
    assert_eq!(
        sparse,
        Value::Vector(VectorValue::new(4, vec![1.0, 0.0, 2.5, 0.0]))
    );
    assert_eq!(
        eval_generic("sparsevec_out", &[sparse]).unwrap(),
        Value::Text("{1:1,3:2.5}/4".to_owned())
    );
    assert!(eval_generic(
        "sparsevec_in",
        &[
            Value::Text("{1:1,3:2.5}/4".to_owned()),
            Value::Int(0),
            Value::Int(3),
        ],
    )
    .is_err());
}

#[test]
fn array_to_vector_generic_returns_dense_runtime_vector() {
    let value = eval_generic(
        "array_to_vector",
        &[
            Value::Array(vec![Value::Int(1), Value::Int(0), Value::Double(2.5)]),
            Value::Int(3),
            Value::Boolean(true),
        ],
    )
    .unwrap();
    assert_eq!(
        value,
        Value::Vector(VectorValue::new(3, vec![1.0, 0.0, 2.5]))
    );
}

#[test]
fn vector_to_float4_generic_returns_real_array() {
    let value = eval_generic(
        "vector_to_float4",
        &[
            Value::Vector(VectorValue::new(3, vec![1.0, 0.0, 2.5])),
            Value::Int(3),
            Value::Boolean(true),
        ],
    )
    .unwrap();
    assert_eq!(
        value,
        Value::Array(vec![Value::Real(1.0), Value::Real(0.0), Value::Real(2.5)])
    );
}

#[test]
fn halfvec_to_float4_generic_returns_real_array() {
    let value = eval_generic(
        "pg_catalog.halfvec_to_float4",
        &[
            Value::Vector(VectorValue::new(3, vec![1.0, 0.0, 2.5])),
            Value::Int(3),
            Value::Boolean(true),
        ],
    )
    .unwrap();
    assert_eq!(
        value,
        Value::Array(vec![Value::Real(1.0), Value::Real(0.0), Value::Real(2.5)])
    );
}

#[test]
fn array_to_halfvec_generic_returns_dense_runtime_vector() {
    let value = eval_generic(
        "array_to_halfvec",
        &[
            Value::Array(vec![Value::Int(1), Value::Int(0), Value::Double(2.5)]),
            Value::Int(3),
            Value::Boolean(true),
        ],
    )
    .unwrap();
    assert_eq!(
        value,
        Value::Vector(VectorValue::new(3, vec![1.0, 0.0, 2.5]))
    );
}

#[test]
fn vector_to_halfvec_generic_returns_dense_runtime_vector() {
    let value = eval_generic(
        "vector_to_halfvec",
        &[
            Value::Vector(VectorValue::new(3, vec![1.0, 0.0, 2.5])),
            Value::Int(3),
            Value::Boolean(true),
        ],
    )
    .unwrap();
    assert_eq!(
        value,
        Value::Vector(VectorValue::new(3, vec![1.0, 0.0, 2.5]))
    );
}

#[test]
fn halfvec_to_vector_generic_returns_dense_runtime_vector() {
    let value = eval_generic(
        "halfvec_to_vector",
        &[
            Value::Vector(VectorValue::new(3, vec![1.0, 0.0, 2.5])),
            Value::Int(3),
            Value::Boolean(true),
        ],
    )
    .unwrap();
    assert_eq!(
        value,
        Value::Vector(VectorValue::new(3, vec![1.0, 0.0, 2.5]))
    );
}

#[test]
fn halfvec_to_sparsevec_generic_returns_dense_runtime_vector() {
    let value = eval_generic(
        "halfvec_to_sparsevec",
        &[
            Value::Vector(VectorValue::new(3, vec![1.0, 0.0, 2.5])),
            Value::Int(3),
            Value::Boolean(true),
        ],
    )
    .unwrap();
    assert_eq!(
        value,
        Value::Vector(VectorValue::new(3, vec![1.0, 0.0, 2.5]))
    );
}

#[test]
fn sparsevec_to_vector_generic_returns_dense_runtime_vector() {
    let value = eval_generic(
        "sparsevec_to_vector",
        &[
            Value::Vector(VectorValue::new(3, vec![1.0, 0.0, 2.5])),
            Value::Int(3),
            Value::Boolean(true),
        ],
    )
    .unwrap();
    assert_eq!(
        value,
        Value::Vector(VectorValue::new(3, vec![1.0, 0.0, 2.5]))
    );
}

#[test]
fn sparsevec_to_halfvec_generic_returns_dense_runtime_vector() {
    let value = eval_generic(
        "sparsevec_to_halfvec",
        &[
            Value::Vector(VectorValue::new(3, vec![1.0, 0.0, 2.5])),
            Value::Int(3),
            Value::Boolean(true),
        ],
    )
    .unwrap();
    assert_eq!(
        value,
        Value::Vector(VectorValue::new(3, vec![1.0, 0.0, 2.5]))
    );
}

#[test]
fn binary_quantize_pgvector_cast_signature_returns_bits() {
    let value = eval_scalar_function(
        &ScalarFunction::BinaryQuantize,
        &[
            Value::Vector(VectorValue::new(4, vec![1.0, -2.0, 0.0, 0.1])),
            Value::Int(4),
            Value::Boolean(true),
        ],
    )
    .unwrap();
    assert_eq!(value, Value::Text("1001".to_owned()));
}

#[test]
fn pg_catalog_pgvector_distance_aliases_work_as_generic_functions() {
    let left = Value::Vector(VectorValue::new(2, vec![1.0, 2.0]));
    let right = Value::Vector(VectorValue::new(2, vec![4.0, 6.0]));

    assert_eq!(
        eval_generic("pg_catalog.l2_distance", &[left.clone(), right.clone()]).unwrap(),
        Value::Double(5.0)
    );
    assert_eq!(
        eval_generic("pg_catalog.l1_distance", &[left.clone(), right.clone()]).unwrap(),
        Value::Double(7.0)
    );
    assert_eq!(
        eval_generic("pg_catalog.inner_product", &[left.clone(), right.clone()]).unwrap(),
        Value::Double(16.0)
    );
    assert_eq!(
        eval_generic("pg_catalog.negative_inner_product", &[left, right]).unwrap(),
        Value::Double(-16.0)
    );
    assert_eq!(
        eval_generic(
            "pg_catalog.cosine_distance",
            &[
                Value::Vector(VectorValue::new(2, vec![1.0, 0.0])),
                Value::Vector(VectorValue::new(2, vec![0.0, 1.0])),
            ],
        )
        .unwrap(),
        Value::Double(1.0)
    );
    assert_eq!(
        eval_generic(
            "pg_catalog.hamming_distance",
            &[
                Value::Text("1010".to_owned()),
                Value::Text("1001".to_owned())
            ],
        )
        .unwrap(),
        Value::Double(2.0)
    );
}

#[test]
fn pgvector_operator_impl_functions_match_vector_arithmetic() {
    let left = Value::Vector(VectorValue::new(3, vec![3.0, 4.0, 12.0]));
    let right = Value::Vector(VectorValue::new(3, vec![1.0, 2.0, 3.0]));

    assert_eq!(
        eval_generic("vector_add", &[left.clone(), right.clone()]).unwrap(),
        Value::Vector(VectorValue::new(3, vec![4.0, 6.0, 15.0]))
    );
    assert_eq!(
        eval_generic("pg_catalog.vector_sub", &[left.clone(), right.clone()]).unwrap(),
        Value::Vector(VectorValue::new(3, vec![2.0, 2.0, 9.0]))
    );
    assert_eq!(
        eval_generic("vector_mul", &[left.clone(), right.clone()]).unwrap(),
        Value::Vector(VectorValue::new(3, vec![3.0, 8.0, 36.0]))
    );
    assert_eq!(
        eval_generic(
            "vector_concat",
            &[left, Value::Vector(VectorValue::new(2, vec![1.0, 2.0]))],
        )
        .unwrap(),
        Value::Vector(VectorValue::new(5, vec![3.0, 4.0, 12.0, 1.0, 2.0]))
    );

    assert_eq!(
        eval_generic(
            "halfvec_add",
            &[
                Value::Vector(VectorValue::new(3, vec![1.0, 2.0, 3.0])),
                Value::Vector(VectorValue::new(3, vec![3.0, 2.0, 1.0])),
            ],
        )
        .unwrap(),
        Value::Vector(VectorValue::new(3, vec![4.0, 4.0, 4.0]))
    );
    assert_eq!(
        eval_generic(
            "pg_catalog.halfvec_concat",
            &[
                Value::Vector(VectorValue::new(2, vec![1.0, 2.0])),
                Value::Vector(VectorValue::new(1, vec![3.0])),
            ],
        )
        .unwrap(),
        Value::Vector(VectorValue::new(3, vec![1.0, 2.0, 3.0]))
    );
}

#[test]
fn vector_to_sparsevec_generic_returns_dense_runtime_vector() {
    let value = eval_generic(
        "vector_to_sparsevec",
        &[
            Value::Vector(VectorValue::new(3, vec![1.0, 0.0, 2.5])),
            Value::Int(3),
            Value::Boolean(true),
        ],
    )
    .unwrap();
    assert_eq!(
        value,
        Value::Vector(VectorValue::new(3, vec![1.0, 0.0, 2.5]))
    );
}

#[test]
fn pg_catalog_vector_to_sparsevec_generic_returns_dense_runtime_vector() {
    let value = eval_generic(
        "pg_catalog.vector_to_sparsevec",
        &[
            Value::Vector(VectorValue::new(3, vec![1.0, 0.0, 2.5])),
            Value::Int(3),
            Value::Boolean(true),
        ],
    )
    .unwrap();
    assert_eq!(
        value,
        Value::Vector(VectorValue::new(3, vec![1.0, 0.0, 2.5]))
    );
}

#[test]
fn array_to_sparsevec_generic_returns_dense_runtime_vector() {
    let value = eval_generic(
        "array_to_sparsevec",
        &[
            Value::Array(vec![Value::Int(1), Value::Int(0), Value::Double(2.5)]),
            Value::Int(3),
            Value::Boolean(true),
        ],
    )
    .unwrap();
    assert_eq!(
        value,
        Value::Vector(VectorValue::new(3, vec![1.0, 0.0, 2.5]))
    );
}

#[test]
fn pg_catalog_array_to_sparsevec_generic_returns_dense_runtime_vector() {
    let value = eval_generic(
        "pg_catalog.array_to_sparsevec",
        &[
            Value::Array(vec![Value::Int(1), Value::Int(0), Value::Double(2.5)]),
            Value::Int(3),
            Value::Boolean(true),
        ],
    )
    .unwrap();
    assert_eq!(
        value,
        Value::Vector(VectorValue::new(3, vec![1.0, 0.0, 2.5]))
    );
}

#[test]
fn array_to_sparsevec_rejects_dimension_mismatch() {
    let err = eval_generic(
        "array_to_sparsevec",
        &[
            Value::Array(vec![Value::Int(1), Value::Int(2)]),
            Value::Int(3),
            Value::Boolean(true),
        ],
    )
    .expect_err("dimension mismatch should fail");
    assert!(err.to_string().contains("expected 3 dimensions"), "{err}");
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
