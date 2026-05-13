use super::*;

// =====================================================================
// IS NULL / IS NOT NULL
// =====================================================================

#[test]
fn placeholder_pg_function_errors_explicitly() {
    let expr = TypedExpr::scalar_function(
        aiondb_plan::ScalarFunction::Generic("currval".to_owned()),
        vec![lit_text("seq_name")],
        DataType::BigInt,
        true,
    );
    let error = eval(&expr).expect_err("currval placeholder should error");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::FeatureNotSupported);
}

#[test]
fn current_setting_returns_builtin_default() {
    let expr = TypedExpr::scalar_function(
        aiondb_plan::ScalarFunction::Generic("current_setting".to_owned()),
        vec![lit_text("search_path")],
        DataType::Text,
        true,
    );
    assert_eq!(
        eval(&expr).unwrap(),
        Value::Text("\"$user\", public".to_owned())
    );
}

#[test]
fn pg_backend_pid_returns_process_id() {
    let expr = TypedExpr::scalar_function(
        aiondb_plan::ScalarFunction::Generic("pg_backend_pid".to_owned()),
        vec![],
        DataType::Int,
        true,
    );
    let value = eval(&expr).unwrap();
    assert!(matches!(value, Value::Int(pid) if pid > 0));
}

#[test]
fn to_regtype_resolves_builtin_alias() {
    let expr = TypedExpr::scalar_function(
        aiondb_plan::ScalarFunction::Generic("to_regtype".to_owned()),
        vec![lit_text("int4")],
        DataType::Text,
        false,
    );
    assert_eq!(eval(&expr).unwrap(), Value::Text("integer".to_owned()));
}

#[test]
fn to_regtype_resolves_pgvector_types() {
    for (input, canonical) in [
        ("vector", "vector"),
        ("pg_catalog.vector(3)", "vector"),
        ("vector(3)[]", "vector[]"),
        ("halfvec(4)", "halfvec"),
        ("sparsevec(5)", "sparsevec"),
        ("bit", "bit"),
        ("bit varying", "bit varying"),
        ("varbit(8)[]", "bit varying[]"),
    ] {
        let expr = TypedExpr::scalar_function(
            aiondb_plan::ScalarFunction::Generic("to_regtype".to_owned()),
            vec![lit_text(input)],
            DataType::Text,
            false,
        );
        assert_eq!(eval(&expr).unwrap(), Value::Text(canonical.to_owned()));
    }

    assert_eq!(
        crate::eval::scalar_functions::pg_compat::resolve_regtype_oid("vector"),
        Some(80_001)
    );
    assert_eq!(
        crate::eval::scalar_functions::pg_compat::resolve_regtype_oid("sparsevec(5)"),
        Some(80_005)
    );
    assert_eq!(
        crate::eval::scalar_functions::pg_compat::resolve_regtype_oid("bit varying(8)[]"),
        Some(1_563)
    );
}

#[test]
fn format_type_renders_pgvector_typmods() {
    assert_eq!(
        crate::eval::scalar_functions::pg_compat::pg_format_type(80_001, 3 + 4),
        "vector(3)"
    );
    assert_eq!(
        crate::eval::scalar_functions::pg_compat::pg_format_type(80_003, 4 + 4),
        "halfvec(4)"
    );
    assert_eq!(
        crate::eval::scalar_functions::pg_compat::pg_format_type(80_005, 5 + 4),
        "sparsevec(5)"
    );
    assert_eq!(
        crate::eval::scalar_functions::pg_compat::pg_format_type(80_001, -1),
        "vector"
    );
}

#[test]
fn compat_type_normalization_handles_pgvector_and_bit_typmods() {
    assert_eq!(
        crate::eval::session::normalize_compat_type_name("pg_catalog.vector(1536)[]"),
        "vector[]"
    );
    assert_eq!(
        crate::eval::session::normalize_compat_type_name("halfvec(4)"),
        "halfvec"
    );
    assert_eq!(
        crate::eval::session::normalize_compat_type_name("sparsevec(5)"),
        "sparsevec"
    );
    assert_eq!(
        crate::eval::session::normalize_compat_type_name("bit varying(128)[]"),
        "varbit[]"
    );
    assert_eq!(
        crate::eval::session::compat_display_type_name("varbit(128)"),
        "bit varying"
    );
}

#[test]
fn to_regclass_resolves_builtin_catalog_table() {
    let expr = TypedExpr::scalar_function(
        aiondb_plan::ScalarFunction::Generic("to_regclass".to_owned()),
        vec![lit_text("pg_class")],
        DataType::Text,
        false,
    );
    assert_eq!(
        eval(&expr).unwrap(),
        Value::Text("pg_catalog.pg_class".to_owned())
    );
}

#[test]
fn to_regnamespace_resolves_builtin_schema() {
    let expr = TypedExpr::scalar_function(
        aiondb_plan::ScalarFunction::Generic("to_regnamespace".to_owned()),
        vec![lit_text("PG_CATALOG")],
        DataType::Text,
        false,
    );
    assert_eq!(eval(&expr).unwrap(), Value::Text("pg_catalog".to_owned()));
}

#[test]
fn to_regproc_resolves_visibility_helpers() {
    for function_name in [
        "pg_catalog.pg_type_is_visible",
        "pg_catalog.pg_operator_is_visible",
        "pg_catalog.pg_ts_parser_is_visible",
        "pg_catalog.pg_collation_is_visible",
        "pg_catalog.pg_statistics_obj_is_visible",
    ] {
        let expr = TypedExpr::scalar_function(
            aiondb_plan::ScalarFunction::Generic("to_regproc".to_owned()),
            vec![lit_text(function_name)],
            DataType::Text,
            false,
        );
        assert_eq!(
            eval(&expr).unwrap(),
            Value::Text(function_name.trim_start_matches("pg_catalog.").to_owned())
        );
    }
}

#[test]
fn to_regprocedure_resolves_visibility_helpers() {
    for function_signature in [
        "pg_catalog.pg_type_is_visible(oid)",
        "pg_catalog.pg_operator_is_visible(oid)",
        "pg_catalog.pg_ts_parser_is_visible(oid)",
        "pg_catalog.pg_collation_is_visible(oid)",
        "pg_catalog.pg_statistics_obj_is_visible(oid)",
    ] {
        let expr = TypedExpr::scalar_function(
            aiondb_plan::ScalarFunction::Generic("to_regprocedure".to_owned()),
            vec![lit_text(function_signature)],
            DataType::Text,
            false,
        );
        assert_eq!(
            eval(&expr).unwrap(),
            Value::Text(
                function_signature
                    .trim_start_matches("pg_catalog.")
                    .to_owned()
            )
        );
    }
}

#[test]
fn to_regprocedure_resolves_pgvector_signatures() {
    for (signature, canonical) in [
        (
            "pg_catalog.l2_distance(vector, vector)",
            "l2_distance(vector,vector)",
        ),
        (
            "negative_inner_product(sparsevec, sparsevec)",
            "negative_inner_product(sparsevec,sparsevec)",
        ),
        (
            "subvector(vector, int4, int4)",
            "subvector(vector,integer,integer)",
        ),
        (
            "array_to_vector(integer[], integer, boolean)",
            "array_to_vector(integer[],integer,boolean)",
        ),
        (
            "vector_to_halfvec(vector, integer, boolean)",
            "vector_to_halfvec(vector,integer,boolean)",
        ),
        (
            "array_to_sparsevec(double precision[], integer, boolean)",
            "array_to_sparsevec(double precision[],integer,boolean)",
        ),
        (
            "binary_quantize(vector, integer, boolean)",
            "binary_quantize(vector,integer,boolean)",
        ),
    ] {
        let expr = TypedExpr::scalar_function(
            aiondb_plan::ScalarFunction::Generic("to_regprocedure".to_owned()),
            vec![lit_text(signature)],
            DataType::Text,
            false,
        );
        assert_eq!(eval(&expr).unwrap(), Value::Text(canonical.to_owned()));
    }
}

#[test]
fn to_regoperator_resolves_pgvector_operators() {
    for (signature, canonical) in [
        ("pg_catalog.<->(vector, vector)", "<->(vector,vector)"),
        ("<#>(halfvec,halfvec)", "<#>(halfvec,halfvec)"),
        ("<=>(sparsevec, sparsevec)", "<=>(sparsevec,sparsevec)"),
        ("<~>(bit, bit)", "<~>(bit,bit)"),
    ] {
        let expr = TypedExpr::scalar_function(
            aiondb_plan::ScalarFunction::Generic("to_regoperator".to_owned()),
            vec![lit_text(signature)],
            DataType::Text,
            false,
        );
        assert_eq!(eval(&expr).unwrap(), Value::Text(canonical.to_owned()));
    }
}

#[test]
fn to_regtype_returns_null_for_unknown_name() {
    let expr = TypedExpr::scalar_function(
        aiondb_plan::ScalarFunction::Generic("to_regtype".to_owned()),
        vec![lit_text("missing_type")],
        DataType::Text,
        true,
    );
    assert_eq!(eval(&expr).unwrap(), Value::Null);
}

#[test]
fn regtype_cast_resolves_user_type_array_aliases() {
    let context = crate::eval::session::EvalSessionContext::default().with_compat_user_types(vec![
        crate::eval::session::CompatUserType {
            name: "attmp_array".to_owned(),
            schema_name: None,
            oid: 5000,
            enum_labels: Vec::new(),
            composite_fields: Vec::new(),
        },
        crate::eval::session::CompatUserType {
            name: "_attmp_array".to_owned(),
            schema_name: None,
            oid: 5001,
            enum_labels: Vec::new(),
            composite_fields: Vec::new(),
        },
        crate::eval::session::CompatUserType {
            name: "__attmp_array".to_owned(),
            schema_name: None,
            oid: 5002,
            enum_labels: Vec::new(),
            composite_fields: Vec::new(),
        },
    ]);

    crate::eval::session::with_session_context(context, || {
        assert_eq!(
            crate::eval::scalar_functions::pg_compat::resolve_regtype_oid("attmp_array[]"),
            Some(5001)
        );
        assert_eq!(
            crate::eval::scalar_functions::pg_compat::resolve_regtype_oid("_attmp_array[]"),
            Some(5002)
        );
    });
}

#[test]
fn to_regclass_returns_null_for_unknown_name() {
    let expr = TypedExpr::scalar_function(
        aiondb_plan::ScalarFunction::Generic("to_regclass".to_owned()),
        vec![lit_text("missing_relation")],
        DataType::Text,
        true,
    );
    assert_eq!(eval(&expr).unwrap(), Value::Null);
}

#[test]
fn is_null_on_null_returns_true() {
    let inner = TypedExpr::literal(Value::Null, DataType::Int, true);
    let expr = TypedExpr::is_null(inner, false);
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn is_null_on_non_null_returns_false() {
    let expr = TypedExpr::is_null(lit_int(42), false);
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(false));
}

#[test]
fn is_not_null_on_null_returns_false() {
    let inner = TypedExpr::literal(Value::Null, DataType::Int, true);
    let expr = TypedExpr::is_null(inner, true);
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(false));
}

#[test]
fn is_not_null_on_non_null_returns_true() {
    let expr = TypedExpr::is_null(lit_int(42), true);
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn is_null_on_text_null() {
    let inner = TypedExpr::literal(Value::Null, DataType::Text, true);
    let expr = TypedExpr::is_null(inner, false);
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn is_null_on_text_non_null() {
    let expr = TypedExpr::is_null(lit_text("hello"), false);
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(false));
}

#[test]
fn is_null_never_returns_null() {
    // Even for NULL input, IS NULL returns Boolean, never NULL
    let inner = TypedExpr::literal(Value::Null, DataType::Int, true);
    let expr = TypedExpr::is_null(inner, false);
    let result = eval(&expr).unwrap();
    assert!(matches!(result, Value::Boolean(_)));
}

#[test]
fn pg_input_error_info_char_overflow_reports_string_truncation() {
    let message_expr = TypedExpr::scalar_function(
        aiondb_plan::ScalarFunction::Generic("__aiondb_pg_input_error_info_message".to_owned()),
        vec![lit_text("abcde"), lit_text("char(4)")],
        DataType::Text,
        true,
    );
    assert_eq!(
        eval(&message_expr).unwrap(),
        Value::Text("value too long for type character(4)".to_owned())
    );

    let sqlstate_expr = TypedExpr::scalar_function(
        aiondb_plan::ScalarFunction::Generic("__aiondb_pg_input_error_info_sqlstate".to_owned()),
        vec![lit_text("abcde"), lit_text("char(4)")],
        DataType::Text,
        true,
    );
    assert_eq!(
        eval(&sqlstate_expr).unwrap(),
        Value::Text("22001".to_owned())
    );
}

#[test]
fn char_pad_length_rejects_negative_length() {
    let expr = TypedExpr::scalar_function(
        aiondb_plan::ScalarFunction::Generic("__aiondb_char_pad_length".to_owned()),
        vec![lit_text("abc"), lit_int(-1)],
        DataType::Text,
        false,
    );
    let err = eval(&expr).expect_err("negative char length must be rejected");
    assert!(err.report().message.contains("must be non-negative"));
}

#[test]
fn char_pad_length_rejects_excessive_length() {
    let expr = TypedExpr::scalar_function(
        aiondb_plan::ScalarFunction::Generic("__aiondb_char_pad_length".to_owned()),
        vec![
            lit_text("abc"),
            TypedExpr::literal(Value::BigInt(20_000_000), DataType::BigInt, false),
        ],
        DataType::Text,
        false,
    );
    let err = eval(&expr).expect_err("excessive char length must be rejected");
    assert!(err.report().message.contains("maximum allowed size"));
}

#[test]
fn quoted_pg_char_cast_decodes_ascii_octal_escape() {
    let expr = TypedExpr::scalar_function(
        aiondb_plan::ScalarFunction::Generic("__aiondb_pg_char_cast".to_owned()),
        vec![lit_text("\\101")],
        DataType::Text,
        false,
    );
    assert_eq!(eval(&expr).unwrap(), Value::Text("A".to_owned()));
}

#[test]
fn quoted_pg_char_cast_preserves_high_bit_escape_display() {
    let expr = TypedExpr::scalar_function(
        aiondb_plan::ScalarFunction::Generic("__aiondb_pg_char_cast".to_owned()),
        vec![lit_text("\\377")],
        DataType::Text,
        false,
    );
    assert_eq!(eval(&expr).unwrap(), Value::Text("\\377".to_owned()));
}

#[test]
fn quoted_pg_char_cast_maps_nul_escape_to_empty_text() {
    let expr = TypedExpr::scalar_function(
        aiondb_plan::ScalarFunction::Generic("__aiondb_pg_char_cast".to_owned()),
        vec![lit_text("\\000")],
        DataType::Text,
        false,
    );
    assert_eq!(eval(&expr).unwrap(), Value::Text(String::new()));
}

#[test]
fn getdatabaseencoding_returns_utf8() {
    let expr = TypedExpr::scalar_function(
        aiondb_plan::ScalarFunction::Generic("getdatabaseencoding".to_owned()),
        vec![],
        DataType::Text,
        false,
    );
    assert_eq!(eval(&expr).unwrap(), Value::Text("UTF8".to_owned()));
}

#[test]
fn pg_encoding_to_char_returns_utf8_for_catalog_encoding_id() {
    let expr = TypedExpr::scalar_function(
        aiondb_plan::ScalarFunction::Generic("pg_encoding_to_char".to_owned()),
        vec![lit_int(6)],
        DataType::Text,
        false,
    );
    assert_eq!(eval(&expr).unwrap(), Value::Text("UTF8".to_owned()));
}

#[test]
fn pg_char_to_encoding_returns_utf8_catalog_id() {
    let expr = TypedExpr::scalar_function(
        aiondb_plan::ScalarFunction::Generic("pg_char_to_encoding".to_owned()),
        vec![lit_text("UTF8")],
        DataType::Int,
        false,
    );
    assert_eq!(eval(&expr).unwrap(), Value::Int(6));
}

#[test]
fn pg_proc_is_visible_returns_true_for_non_null_oid() {
    let expr = TypedExpr::scalar_function(
        aiondb_plan::ScalarFunction::Generic("pg_proc_is_visible".to_owned()),
        vec![lit_int(2092)],
        DataType::Boolean,
        false,
    );
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn normalize_and_is_normalized_support_unicode_forms() {
    let normalize_expr = TypedExpr::scalar_function(
        aiondb_plan::ScalarFunction::Generic("normalize".to_owned()),
        vec![lit_text("a\u{0308}\u{24D1}c"), lit_text("NFKC")],
        DataType::Text,
        false,
    );
    assert_eq!(
        eval(&normalize_expr).unwrap(),
        Value::Text("äbc".to_owned())
    );

    let is_normalized_expr = TypedExpr::scalar_function(
        aiondb_plan::ScalarFunction::Generic("is_normalized".to_owned()),
        vec![lit_text("äbc"), lit_text("NFC")],
        DataType::Boolean,
        false,
    );
    assert_eq!(eval(&is_normalized_expr).unwrap(), Value::Boolean(true));
}

#[test]
fn normalize_rejects_invalid_normalization_form() {
    let expr = TypedExpr::scalar_function(
        aiondb_plan::ScalarFunction::Generic("normalize".to_owned()),
        vec![lit_text("abc"), lit_text("def")],
        DataType::Text,
        false,
    );
    let error = eval(&expr).expect_err("invalid form should error");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::SyntaxError);
    assert!(error
        .to_string()
        .contains("invalid normalization form: def"));
}

// =====================================================================
// LIKE
// =====================================================================

#[test]
fn like_exact_match() {
    let expr = TypedExpr::like(lit_text("hello"), lit_text("hello"), false, false);
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn like_exact_no_match() {
    let expr = TypedExpr::like(lit_text("hello"), lit_text("world"), false, false);
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(false));
}

#[test]
fn like_percent_matches_any() {
    let expr = TypedExpr::like(lit_text("hello world"), lit_text("hello%"), false, false);
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn like_percent_at_start() {
    let expr = TypedExpr::like(lit_text("hello world"), lit_text("%world"), false, false);
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn like_percent_both_ends() {
    let expr = TypedExpr::like(lit_text("hello world"), lit_text("%lo wo%"), false, false);
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn like_percent_matches_empty() {
    let expr = TypedExpr::like(lit_text("hello"), lit_text("hello%"), false, false);
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn like_underscore_matches_single_char() {
    let expr = TypedExpr::like(lit_text("hello"), lit_text("hell_"), false, false);
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn like_underscore_no_match_empty() {
    let expr = TypedExpr::like(lit_text("hell"), lit_text("hell_"), false, false);
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(false));
}

#[test]
fn like_empty_pattern_matches_empty_string() {
    let expr = TypedExpr::like(lit_text(""), lit_text(""), false, false);
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn like_empty_pattern_no_match_nonempty() {
    let expr = TypedExpr::like(lit_text("a"), lit_text(""), false, false);
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(false));
}

#[test]
fn like_percent_only_matches_anything() {
    let expr = TypedExpr::like(lit_text("anything"), lit_text("%"), false, false);
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn like_percent_only_matches_empty() {
    let expr = TypedExpr::like(lit_text(""), lit_text("%"), false, false);
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn not_like_match() {
    let expr = TypedExpr::like(lit_text("hello"), lit_text("hello"), true, false);
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(false));
}

#[test]
fn not_like_no_match() {
    let expr = TypedExpr::like(lit_text("hello"), lit_text("world"), true, false);
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn like_null_value_returns_null() {
    let inner = TypedExpr::literal(Value::Null, DataType::Text, true);
    let expr = TypedExpr::like(inner, lit_text("pattern"), false, false);
    assert_eq!(eval(&expr).unwrap(), Value::Null);
}

#[test]
fn like_null_pattern_returns_null() {
    let pattern = TypedExpr::literal(Value::Null, DataType::Text, true);
    let expr = TypedExpr::like(lit_text("hello"), pattern, false, false);
    assert_eq!(eval(&expr).unwrap(), Value::Null);
}

#[test]
fn like_matches_long_text_without_silent_false_negative() {
    let text = format!("{}needle{}", "a".repeat(2_048), "b".repeat(2_048));
    let expr = TypedExpr::like(
        TypedExpr::literal(Value::Text(text), DataType::Text, false),
        lit_text("%needle%"),
        false,
        false,
    );
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

// =====================================================================
// ILIKE (case-insensitive LIKE)
// =====================================================================

#[test]
fn ilike_case_insensitive_match() {
    let expr = TypedExpr::like(lit_text("Hello World"), lit_text("hello%"), false, true);
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn ilike_case_insensitive_no_match() {
    let expr = TypedExpr::like(lit_text("Hello World"), lit_text("xyz%"), false, true);
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(false));
}

#[test]
fn ilike_case_insensitive_exact_match() {
    let expr = TypedExpr::like(lit_text("ALICE"), lit_text("alice"), false, true);
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn ilike_case_insensitive_percent_both_ends() {
    let expr = TypedExpr::like(lit_text("Hello World"), lit_text("%LO WO%"), false, true);
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn ilike_case_insensitive_underscore() {
    let expr = TypedExpr::like(lit_text("Hello"), lit_text("hell_"), false, true);
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn not_ilike_case_insensitive_match() {
    let expr = TypedExpr::like(lit_text("Hello"), lit_text("hello"), true, true);
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(false));
}

#[test]
fn not_ilike_case_insensitive_no_match() {
    let expr = TypedExpr::like(lit_text("Hello"), lit_text("world"), true, true);
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn ilike_null_returns_null() {
    let inner = TypedExpr::literal(Value::Null, DataType::Text, true);
    let expr = TypedExpr::like(inner, lit_text("pattern"), false, true);
    assert_eq!(eval(&expr).unwrap(), Value::Null);
}

// =====================================================================
// IN list
// =====================================================================

#[test]
fn in_list_found() {
    let expr = TypedExpr::in_list(lit_int(2), vec![lit_int(1), lit_int(2), lit_int(3)], false);
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn in_list_not_found() {
    let expr = TypedExpr::in_list(lit_int(5), vec![lit_int(1), lit_int(2), lit_int(3)], false);
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(false));
}

#[test]
fn in_list_negated_found() {
    let expr = TypedExpr::in_list(lit_int(2), vec![lit_int(1), lit_int(2), lit_int(3)], true);
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(false));
}

#[test]
fn in_list_negated_not_found() {
    let expr = TypedExpr::in_list(lit_int(5), vec![lit_int(1), lit_int(2), lit_int(3)], true);
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn in_list_null_value_returns_null() {
    let expr = TypedExpr::in_list(lit_null(), vec![lit_int(1), lit_int(2)], false);
    assert_eq!(eval(&expr).unwrap(), Value::Null);
}

#[test]
fn in_list_null_in_list_not_found_returns_null() {
    let null_item = TypedExpr::literal(Value::Null, DataType::Int, true);
    let expr = TypedExpr::in_list(lit_int(5), vec![lit_int(1), null_item, lit_int(3)], false);
    assert_eq!(eval(&expr).unwrap(), Value::Null);
}

#[test]
fn in_list_null_in_list_found_returns_true() {
    let null_item = TypedExpr::literal(Value::Null, DataType::Int, true);
    let expr = TypedExpr::in_list(lit_int(1), vec![lit_int(1), null_item, lit_int(3)], false);
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn in_list_empty_list() {
    let expr = TypedExpr::in_list(lit_int(1), vec![], false);
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(false));
}

#[test]
fn in_list_text_found() {
    let expr = TypedExpr::in_list(
        lit_text("b"),
        vec![lit_text("a"), lit_text("b"), lit_text("c")],
        false,
    );
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn in_list_text_not_found() {
    let expr = TypedExpr::in_list(
        lit_text("z"),
        vec![lit_text("a"), lit_text("b"), lit_text("c")],
        false,
    );
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(false));
}

// =====================================================================
// BETWEEN
// =====================================================================

#[test]
fn between_in_range() {
    let expr = TypedExpr::between(lit_int(5), lit_int(1), lit_int(10), false);
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn between_at_low_boundary() {
    let expr = TypedExpr::between(lit_int(1), lit_int(1), lit_int(10), false);
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn between_at_high_boundary() {
    let expr = TypedExpr::between(lit_int(10), lit_int(1), lit_int(10), false);
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn between_below_range() {
    let expr = TypedExpr::between(lit_int(0), lit_int(1), lit_int(10), false);
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(false));
}

#[test]
fn between_above_range() {
    let expr = TypedExpr::between(lit_int(11), lit_int(1), lit_int(10), false);
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(false));
}

#[test]
fn not_between_in_range() {
    let expr = TypedExpr::between(lit_int(5), lit_int(1), lit_int(10), true);
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(false));
}

#[test]
fn not_between_out_of_range() {
    let expr = TypedExpr::between(lit_int(0), lit_int(1), lit_int(10), true);
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn between_null_value_returns_null() {
    let expr = TypedExpr::between(lit_null(), lit_int(1), lit_int(10), false);
    assert_eq!(eval(&expr).unwrap(), Value::Null);
}

#[test]
fn between_null_low_returns_null() {
    let expr = TypedExpr::between(lit_int(5), lit_null(), lit_int(10), false);
    assert_eq!(eval(&expr).unwrap(), Value::Null);
}

#[test]
fn between_null_high_returns_null() {
    let expr = TypedExpr::between(lit_int(5), lit_int(1), lit_null(), false);
    assert_eq!(eval(&expr).unwrap(), Value::Null);
}

#[test]
fn between_text_in_range() {
    let expr = TypedExpr::between(lit_text("b"), lit_text("a"), lit_text("c"), false);
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn between_text_out_of_range() {
    let expr = TypedExpr::between(lit_text("z"), lit_text("a"), lit_text("c"), false);
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(false));
}

#[test]
fn between_double_in_range() {
    let expr = TypedExpr::between(lit_double(5.5), lit_double(1.0), lit_double(10.0), false);
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

// =====================================================================
// COALESCE
// =====================================================================

#[test]
fn coalesce_returns_first_non_null() {
    let expr = TypedExpr::coalesce(vec![lit_null(), lit_null(), lit_int(42)], DataType::Int);
    assert_eq!(eval(&expr).unwrap(), Value::Int(42));
}

#[test]
fn coalesce_returns_first_when_non_null() {
    let expr = TypedExpr::coalesce(vec![lit_int(1), lit_int(2)], DataType::Int);
    assert_eq!(eval(&expr).unwrap(), Value::Int(1));
}

#[test]
fn coalesce_all_null_returns_null() {
    let expr = TypedExpr::coalesce(vec![lit_null(), lit_null()], DataType::Int);
    assert_eq!(eval(&expr).unwrap(), Value::Null);
}

#[test]
fn coalesce_single_non_null() {
    let expr = TypedExpr::coalesce(vec![lit_text("hello")], DataType::Text);
    assert_eq!(eval(&expr).unwrap(), Value::Text("hello".into()));
}

#[test]
fn coalesce_with_row() {
    let col_a = TypedExpr::column_ref("a", 0, DataType::Int, true);
    let expr = TypedExpr::coalesce(vec![col_a, lit_int(99)], DataType::Int);
    let row = Row {
        values: vec![Value::Null],
    };
    assert_eq!(eval_row(&expr, &row).unwrap(), Value::Int(99));
}

// =====================================================================
// NULLIF
// =====================================================================

#[test]
fn nullif_returns_null_when_equal() {
    let expr = TypedExpr::nullif(lit_int(1), lit_int(1), DataType::Int);
    assert_eq!(eval(&expr).unwrap(), Value::Null);
}

#[test]
fn nullif_returns_first_when_not_equal() {
    let expr = TypedExpr::nullif(lit_int(1), lit_int(2), DataType::Int);
    assert_eq!(eval(&expr).unwrap(), Value::Int(1));
}

#[test]
fn nullif_with_null_left() {
    let expr = TypedExpr::nullif(lit_null(), lit_int(1), DataType::Int);
    // NULL != 1 is NULL (unknown), so not equal => return left which is NULL
    assert_eq!(eval(&expr).unwrap(), Value::Null);
}

#[test]
fn nullif_with_text() {
    let expr = TypedExpr::nullif(lit_text("a"), lit_text("b"), DataType::Text);
    assert_eq!(eval(&expr).unwrap(), Value::Text("a".into()));
}

#[test]
fn nullif_with_text_equal() {
    let expr = TypedExpr::nullif(lit_text("same"), lit_text("same"), DataType::Text);
    assert_eq!(eval(&expr).unwrap(), Value::Null);
}

// =====================================================================
// CAST
// =====================================================================

#[test]
fn cast_int_to_bigint() {
    let expr = TypedExpr::cast(lit_int(1), DataType::BigInt);
    assert_eq!(eval(&expr).unwrap(), Value::BigInt(1));
}

#[test]
fn cast_int_to_text() {
    let expr = TypedExpr::cast(lit_int(42), DataType::Text);
    assert_eq!(eval(&expr).unwrap(), Value::Text("42".into()));
}

#[test]
fn cast_null_to_int() {
    let expr = TypedExpr::cast(lit_null(), DataType::Int);
    assert_eq!(eval(&expr).unwrap(), Value::Null);
}

#[test]
fn cast_text_to_int() {
    let expr = TypedExpr::cast(lit_text("123"), DataType::Int);
    assert_eq!(eval(&expr).unwrap(), Value::Int(123));
}

#[test]
fn cast_text_to_int_invalid() {
    let expr = TypedExpr::cast(lit_text("abc"), DataType::Int);
    assert!(eval(&expr).is_err());
}

#[test]
fn cast_bigint_to_int() {
    let expr = TypedExpr::cast(lit_bigint(100), DataType::Int);
    assert_eq!(eval(&expr).unwrap(), Value::Int(100));
}

#[test]
fn cast_bigint_to_int_overflow() {
    let expr = TypedExpr::cast(lit_bigint(i64::MAX), DataType::Int);
    assert!(eval(&expr).is_err());
}

#[test]
fn cast_int_to_double() {
    let expr = TypedExpr::cast(lit_int(5), DataType::Double);
    assert_eq!(eval(&expr).unwrap(), Value::Double(5.0));
}

#[test]
fn cast_int_to_real() {
    let expr = TypedExpr::cast(lit_int(5), DataType::Real);
    assert_eq!(eval(&expr).unwrap(), Value::Real(5.0));
}

#[test]
fn cast_double_to_text() {
    let expr = TypedExpr::cast(lit_double(3.14), DataType::Text);
    assert_eq!(eval(&expr).unwrap(), Value::Text("3.14".into()));
}

#[test]
fn cast_bool_to_text() {
    let expr = TypedExpr::cast(lit_bool(true), DataType::Text);
    assert_eq!(eval(&expr).unwrap(), Value::Text("true".into()));
}

#[test]
fn cast_text_to_bool_true() {
    let expr = TypedExpr::cast(lit_text("true"), DataType::Boolean);
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn cast_text_to_bool_false() {
    let expr = TypedExpr::cast(lit_text("false"), DataType::Boolean);
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(false));
}

#[test]
fn cast_identity_int() {
    let expr = TypedExpr::cast(lit_int(42), DataType::Int);
    assert_eq!(eval(&expr).unwrap(), Value::Int(42));
}

#[test]
fn cast_with_row() {
    let col = TypedExpr::column_ref("val", 0, DataType::Int, false);
    let expr = TypedExpr::cast(col, DataType::BigInt);
    let row = Row {
        values: vec![Value::Int(7)],
    };
    assert_eq!(eval_row(&expr, &row).unwrap(), Value::BigInt(7));
}

// =====================================================================
// CASE WHEN
// =====================================================================

#[test]
fn case_when_first_branch_true() {
    // CASE WHEN TRUE THEN 'yes' ELSE 'no' END
    let expr = TypedExpr::case_when(
        vec![lit_bool(true)],
        vec![lit_text("yes")],
        Some(lit_text("no")),
        DataType::Text,
        false,
    );
    assert_eq!(eval(&expr).unwrap(), Value::Text("yes".into()));
}

#[test]
fn case_when_first_branch_false_takes_else() {
    // CASE WHEN FALSE THEN 'yes' ELSE 'no' END
    let expr = TypedExpr::case_when(
        vec![lit_bool(false)],
        vec![lit_text("yes")],
        Some(lit_text("no")),
        DataType::Text,
        false,
    );
    assert_eq!(eval(&expr).unwrap(), Value::Text("no".into()));
}

#[test]
fn case_when_no_else_returns_null() {
    // CASE WHEN FALSE THEN 'yes' END
    let expr = TypedExpr::case_when(
        vec![lit_bool(false)],
        vec![lit_text("yes")],
        None,
        DataType::Text,
        true,
    );
    assert_eq!(eval(&expr).unwrap(), Value::Null);
}

#[test]
fn case_when_multiple_branches_second_matches() {
    // CASE WHEN FALSE THEN 1 WHEN TRUE THEN 2 ELSE 3 END
    let expr = TypedExpr::case_when(
        vec![lit_bool(false), lit_bool(true)],
        vec![lit_int(1), lit_int(2)],
        Some(lit_int(3)),
        DataType::Int,
        false,
    );
    assert_eq!(eval(&expr).unwrap(), Value::Int(2));
}

#[test]
fn case_when_null_condition_skipped() {
    // CASE WHEN NULL THEN 'yes' ELSE 'no' END
    let expr = TypedExpr::case_when(
        vec![lit_null()],
        vec![lit_text("yes")],
        Some(lit_text("no")),
        DataType::Text,
        false,
    );
    assert_eq!(eval(&expr).unwrap(), Value::Text("no".into()));
}

#[test]
fn case_when_with_row() {
    // CASE WHEN col0 > 0 THEN 'positive' ELSE 'non-positive' END
    let col = TypedExpr::column_ref("val", 0, DataType::Int, false);
    let cond = TypedExpr::binary_gt(col, lit_int(0));
    let expr = TypedExpr::case_when(
        vec![cond],
        vec![lit_text("positive")],
        Some(lit_text("non-positive")),
        DataType::Text,
        false,
    );
    let row_pos = Row {
        values: vec![Value::Int(5)],
    };
    assert_eq!(
        eval_row(&expr, &row_pos).unwrap(),
        Value::Text("positive".into())
    );
    let row_neg = Row {
        values: vec![Value::Int(-1)],
    };
    assert_eq!(
        eval_row(&expr, &row_neg).unwrap(),
        Value::Text("non-positive".into())
    );
}

#[test]
fn cast_inside_case_when() {
    // CASE WHEN TRUE THEN CAST(1 AS BIGINT) ELSE CAST(2 AS BIGINT) END
    let expr = TypedExpr::case_when(
        vec![lit_bool(true)],
        vec![TypedExpr::cast(lit_int(1), DataType::BigInt)],
        Some(TypedExpr::cast(lit_int(2), DataType::BigInt)),
        DataType::BigInt,
        false,
    );
    assert_eq!(eval(&expr).unwrap(), Value::BigInt(1));
}

#[test]
fn case_when_inside_cast() {
    // CAST(CASE WHEN TRUE THEN 1 ELSE 2 END AS BIGINT)
    let case_expr = TypedExpr::case_when(
        vec![lit_bool(true)],
        vec![lit_int(1)],
        Some(lit_int(2)),
        DataType::Int,
        false,
    );
    let expr = TypedExpr::cast(case_expr, DataType::BigInt);
    assert_eq!(eval(&expr).unwrap(), Value::BigInt(1));
}
