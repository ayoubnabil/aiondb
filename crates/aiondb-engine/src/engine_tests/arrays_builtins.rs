use super::*;

#[test]
fn multiple_rows_with_arrays() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE t (id INT NOT NULL, vals INT[])")
        .expect("create table");

    engine
        .execute_sql(&session, "INSERT INTO t VALUES (1, ARRAY[10, 20])")
        .expect("insert 1");
    engine
        .execute_sql(&session, "INSERT INTO t VALUES (2, ARRAY[30, 40, 50])")
        .expect("insert 2");

    let results = engine
        .execute_sql(&session, "SELECT id, vals FROM t ORDER BY id")
        .expect("select");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 2);
            assert_eq!(
                rows[0].values[1],
                Value::Array(vec![Value::Int(10), Value::Int(20)])
            );
            assert_eq!(
                rows[1].values[1],
                Value::Array(vec![Value::Int(30), Value::Int(40), Value::Int(50)])
            );
        }
        other => panic!("expected query, got {other:?}"),
    }
}

// ===================================================================
// Array column with NULL value (entire array is NULL)
// ===================================================================

#[test]
fn null_array_column() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE t (id INT NOT NULL, vals INT[])")
        .expect("create table");

    engine
        .execute_sql(&session, "INSERT INTO t VALUES (1, NULL)")
        .expect("insert null array");

    let results = engine
        .execute_sql(&session, "SELECT vals FROM t WHERE id = 1")
        .expect("select");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::Null);
        }
        other => panic!("expected query, got {other:?}"),
    }
}

// ===================================================================
// array_length function
// ===================================================================

#[test]
fn array_length_basic() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(&session, "SELECT array_length(ARRAY[1, 2, 3], 1)")
        .expect("array_length");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows[0].values[0], Value::Int(3));
        }
        other => panic!("expected query, got {other:?}"),
    }
}

#[test]
fn array_length_single_arg() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(&session, "SELECT array_length(ARRAY[10, 20], 1)")
        .expect("array_length single arg");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows[0].values[0], Value::Int(2));
        }
        other => panic!("expected query, got {other:?}"),
    }
}

#[test]
fn array_length_null_array() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE t (id INT NOT NULL, vals INT[])")
        .expect("create table");
    engine
        .execute_sql(&session, "INSERT INTO t VALUES (1, NULL)")
        .expect("insert");

    let results = engine
        .execute_sql(&session, "SELECT array_length(vals, 1) FROM t")
        .expect("array_length null");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows[0].values[0], Value::Null);
        }
        other => panic!("expected query, got {other:?}"),
    }
}

// ===================================================================
// array_upper / array_lower functions
// ===================================================================

#[test]
fn array_upper_basic() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(&session, "SELECT array_upper(ARRAY[10, 20, 30], 1)")
        .expect("array_upper");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows[0].values[0], Value::Int(3));
        }
        other => panic!("expected query, got {other:?}"),
    }
}

#[test]
fn array_lower_basic() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(&session, "SELECT array_lower(ARRAY[10, 20, 30], 1)")
        .expect("array_lower");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows[0].values[0], Value::Int(1));
        }
        other => panic!("expected query, got {other:?}"),
    }
}

// ===================================================================
// array_position function
// ===================================================================

#[test]
fn array_position_found() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(&session, "SELECT array_position(ARRAY[10, 20, 30], 20)")
        .expect("array_position found");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows[0].values[0], Value::Int(2));
        }
        other => panic!("expected query, got {other:?}"),
    }
}

#[test]
fn array_position_not_found() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(&session, "SELECT array_position(ARRAY[10, 20, 30], 99)")
        .expect("array_position not found");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows[0].values[0], Value::Null);
        }
        other => panic!("expected query, got {other:?}"),
    }
}

#[test]
fn array_position_null_array() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE t (id INT NOT NULL, vals INT[])")
        .expect("create table");
    engine
        .execute_sql(&session, "INSERT INTO t VALUES (1, NULL)")
        .expect("insert");

    let results = engine
        .execute_sql(&session, "SELECT array_position(vals, 1) FROM t")
        .expect("array_position null array");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows[0].values[0], Value::Null);
        }
        other => panic!("expected query, got {other:?}"),
    }
}

#[test]
fn array_position_text_array() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(&session, "SELECT array_position(ARRAY['a', 'b', 'c'], 'b')")
        .expect("array_position text");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows[0].values[0], Value::Int(2));
        }
        other => panic!("expected query, got {other:?}"),
    }
}

#[test]
fn array_position_respects_start_argument() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(&session, "SELECT array_position(ARRAY[1, 2, 3, 2], 2, 3)")
        .expect("array_position with start");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows[0].values[0], Value::Int(4));
        }
        other => panic!("expected query, got {other:?}"),
    }
}

#[test]
fn array_position_finds_null_element() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT array_position(ARRAY['sun','mon','tue','wed','thu',NULL,'fri','sat'], NULL)",
        )
        .expect("array_position should find null");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows[0].values[0], Value::Int(6));
        }
        other => panic!("expected query, got {other:?}"),
    }
}

#[test]
fn array_position_and_positions_reject_multidimensional_arrays() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let err = engine
        .execute_sql(&session, "SELECT array_position(ARRAY[[1, 2], [3, 4]], 3)")
        .expect_err("multidimensional array_position should fail");
    assert!(
        format!("{err}")
            .contains("searching for elements in multidimensional arrays is not supported"),
        "unexpected error: {err}"
    );

    let err = engine
        .execute_sql(&session, "SELECT array_positions(ARRAY[[1, 2], [3, 4]], 4)")
        .expect_err("multidimensional array_positions should fail");
    assert!(
        format!("{err}")
            .contains("searching for elements in multidimensional arrays is not supported"),
        "unexpected error: {err}"
    );
}

#[test]
fn malformed_text_array_literals_report_pg_details() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    for (sql, literal, detail) in [
        (
            "SELECT '{{1,{2}},{2,3}}'::text[]",
            "{{1,{2}},{2,3}}",
            "Unexpected \"{\" character.",
        ),
        (
            "SELECT '{{},{}}'::text[]",
            "{{},{}}",
            "Unexpected \"}\" character.",
        ),
        (
            "SELECT E'{{1,2},\\\\{2,3}}'::text[]",
            "{{1,2},\\{2,3}}",
            "Unexpected \"\\\" character.",
        ),
        (
            "SELECT '{{\"1 2\" x},{3}}'::text[]",
            "{{\"1 2\" x},{3}}",
            "Unexpected array element.",
        ),
        (
            "SELECT '{}}'::text[]",
            "{}}",
            "Junk after closing right brace.",
        ),
        (
            "SELECT '{ }}'::text[]",
            "{ }}",
            "Junk after closing right brace.",
        ),
    ] {
        let err = engine
            .execute_sql(&session, sql)
            .expect_err("malformed text[] cast should fail");
        let report = err.report();

        assert_eq!(
            err.sqlstate(),
            aiondb_core::SqlState::InvalidTextRepresentation
        );
        assert_eq!(
            report.message,
            format!("malformed array literal: \"{literal}\"")
        );
        assert_eq!(report.client_detail.as_deref(), Some(detail));
        assert!(
            report.position.is_some(),
            "expected source position for {sql}, got {report:?}"
        );
    }
}

#[test]
fn malformed_escape_string_array_literal_points_at_e_prefix() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let sql = "SELECT E'{{1,2},\\\\{2,3}}'::text[]";
    let err = engine
        .execute_sql(&session, sql)
        .expect_err("malformed escape-string array cast should fail");

    assert_eq!(err.report().position, sql.find("E'").map(|index| index + 1));
}

#[test]
fn do_block_array_position_loop_emits_expected_notices() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "DO $$
DECLARE
  o int;
  a int[] := ARRAY[1,2,3,2,3,1,2];
BEGIN
  o := array_position(a, 2);
  WHILE o IS NOT NULL
  LOOP
    RAISE NOTICE '%', o;
    o := array_position(a, 2, o + 1);
  END LOOP;
END
$$ LANGUAGE plpgsql;",
        )
        .expect("do block should succeed");

    assert_eq!(results.len(), 4);
    assert!(matches!(
        &results[0],
        StatementResult::Notice { message } if message == "2"
    ));
    assert!(matches!(
        &results[1],
        StatementResult::Notice { message } if message == "4"
    ));
    assert!(matches!(
        &results[2],
        StatementResult::Notice { message } if message == "7"
    ));
    assert!(matches!(
        &results[3],
        StatementResult::Command { tag, rows_affected } if tag == "DO" && *rows_affected == 0
    ));
}

#[test]
fn do_block_array_assignment_overflow_returns_program_limit() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let err = engine
        .execute_sql(
            &session,
            "DO $$ DECLARE a int[];
BEGIN
  a := '[-2147483648:-2147483647]={1,2}'::int[];
  a[2147483647] := 42;
END $$;",
        )
        .expect_err("overflowing do block array assignment should fail");

    assert_eq!(err.sqlstate(), aiondb_core::SqlState::ProgramLimitExceeded);
    assert!(
        format!("{err}").contains("too many elements"),
        "unexpected error: {err}"
    );
}

#[test]
fn quantified_any_all_coerce_text_array_literals() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT 33 = ANY ('{1,2,3}'), 33 <> ALL ('{1,2,3}'), 33 >= ALL ('{1,2,3}'), 'b' = ANY ('{a,b,c}')",
        )
        .expect("quantified comparisons should succeed");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::Boolean(false));
            assert_eq!(rows[0].values[1], Value::Boolean(true));
            assert_eq!(rows[0].values[2], Value::Boolean(true));
            assert_eq!(rows[0].values[3], Value::Boolean(true));
        }
        other => panic!("expected query, got {other:?}"),
    }
}

#[test]
fn quantified_any_all_match_pg_null_and_empty_array_semantics() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT 33 = ALL ('{1,2,33}'), NULL::INT >= ALL ('{1,2,33}'), NULL::INT >= ALL ('{}'), NULL::INT >= ANY ('{}'), 33 = ANY ('{1,NULL,3}'), 33 = ALL ('{33,NULL,33}')",
        )
        .expect("quantified comparison semantics should succeed");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::Boolean(false));
            assert_eq!(rows[0].values[1], Value::Null);
            assert_eq!(rows[0].values[2], Value::Boolean(true));
            assert_eq!(rows[0].values[3], Value::Boolean(false));
            assert_eq!(rows[0].values[4], Value::Null);
            assert_eq!(rows[0].values[5], Value::Null);
        }
        other => panic!("expected query, got {other:?}"),
    }
}

#[test]
fn quantified_any_all_errors_match_pg_expectations() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let err = engine
        .execute_sql(&session, "SELECT 33 * ANY ('{1,2,3}')")
        .expect_err("non-boolean ANY operator should fail");
    assert!(
        format!("{err}").contains("op ANY/ALL (array) requires operator to yield boolean"),
        "unexpected error: {err}"
    );

    let err = engine
        .execute_sql(&session, "SELECT 33 * ANY (44)")
        .expect_err("non-array ANY operand should fail");
    assert!(
        format!("{err}").contains("op ANY/ALL (array) requires array on right side"),
        "unexpected error: {err}"
    );
}

#[test]
fn quantified_like_and_ilike_array_patterns_match_pg_semantics() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT 'foo' LIKE ANY (ARRAY['%a', '%o']), 'foo' LIKE ALL (ARRAY['f%', '%o']), 'foo' NOT LIKE ANY (ARRAY['%a', '%b']), 'foo' ILIKE ALL (ARRAY['F%', '%O'])",
        )
        .expect("quantified LIKE/ILIKE should succeed");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::Boolean(true));
            assert_eq!(rows[0].values[1], Value::Boolean(true));
            assert_eq!(rows[0].values[2], Value::Boolean(true));
            assert_eq!(rows[0].values[3], Value::Boolean(true));
        }
        other => panic!("expected query, got {other:?}"),
    }
}

// ===================================================================
// array_remove function
// ===================================================================

#[test]
fn array_remove_basic() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(&session, "SELECT array_remove(ARRAY[1, 2, 3, 2], 2)")
        .expect("array_remove");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(
                rows[0].values[0],
                Value::Array(vec![Value::Int(1), Value::Int(3)])
            );
        }
        other => panic!("expected query, got {other:?}"),
    }
}

#[test]
fn array_remove_not_present() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(&session, "SELECT array_remove(ARRAY[1, 2, 3], 99)")
        .expect("array_remove not present");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(
                rows[0].values[0],
                Value::Array(vec![Value::Int(1), Value::Int(2), Value::Int(3)])
            );
        }
        other => panic!("expected query, got {other:?}"),
    }
}

#[test]
fn array_remove_null_array() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE t (id INT NOT NULL, vals INT[])")
        .expect("create table");
    engine
        .execute_sql(&session, "INSERT INTO t VALUES (1, NULL)")
        .expect("insert");

    let results = engine
        .execute_sql(&session, "SELECT array_remove(vals, 1) FROM t")
        .expect("array_remove null");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows[0].values[0], Value::Null);
        }
        other => panic!("expected query, got {other:?}"),
    }
}

#[test]
fn array_remove_accepts_text_array_literal() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(&session, "SELECT array_remove('{1,2,2,3}', 2)")
        .expect("array_remove text literal");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(
                rows[0].values[0],
                Value::Array(vec![Value::Int(1), Value::Int(3)])
            );
        }
        other => panic!("expected query, got {other:?}"),
    }
}

#[test]
fn array_remove_null_target_removes_null_elements() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT array_remove(ARRAY[1, NULL, NULL, 3], NULL)",
        )
        .expect("array_remove null target");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(
                rows[0].values[0],
                Value::Array(vec![Value::Int(1), Value::Int(3)])
            );
        }
        other => panic!("expected query, got {other:?}"),
    }
}

#[test]
fn array_remove_rejects_multidimensional_text_array_literal() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(&session, "SELECT array_remove('{{1,2,2},{1,4,3}}', 2)")
        .expect_err("multidimensional array_remove should fail");
    assert!(
        error
            .report()
            .message
            .contains("removing elements from multidimensional arrays is not supported"),
        "unexpected error: {error:?}"
    );
}

#[test]
fn scalar_function_over_unnest_expands_like_pg() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(&engine, &session, "SELECT abs(unnest(array[1,2,NULL,-3]))");
    assert_eq!(rows.len(), 4);
    assert_eq!(rows[0].values[0], Value::Int(1));
    assert_eq!(rows[1].values[0], Value::Int(2));
    assert_eq!(rows[2].values[0], Value::Null);
    assert_eq!(rows[3].values[0], Value::Int(3));
}

#[test]
fn array_of_composite_field_access_returns_text_field() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "CREATE TYPE comptype AS (f1 INT, f2 TEXT); \
         CREATE TABLE comptable (c1 comptype, c2 comptype[]); \
         INSERT INTO comptable VALUES (row(1,'foo'), ARRAY[row(2,'bar')::comptype, row(3,'baz')::comptype]); \
         SELECT c2[2].f2 FROM comptable",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::Text("baz".to_owned()));
}

#[test]
fn fipshash_over_array_of_composite_field_returns_md5_length() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "CREATE TYPE textandtext AS (c1 TEXT, c2 TEXT); \
         CREATE TABLE dest (f1 textandtext[]); \
         INSERT INTO dest VALUES (ARRAY[row('abc','def')::textandtext]); \
         SELECT length(fipshash((f1[1]).c2)) FROM dest",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::Int(32));
}

#[test]
fn insert_and_update_array_of_composite_field_targets() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let inserted_rows = query_rows(
        &engine,
        &session,
        "CREATE TYPE int8_tbl AS (q1 BIGINT, q2 BIGINT); \
         CREATE TEMP TABLE t1 (f1 int8_tbl[]); \
         INSERT INTO t1 (f1[5].q1) VALUES (42); \
         SELECT f1 FROM t1",
    );
    assert_eq!(inserted_rows.len(), 1);
    assert_eq!(
        inserted_rows[0].values[0],
        Value::Text("[5:5]={\"(42,)\"}".to_owned())
    );

    let updated_rows = query_rows(
        &engine,
        &session,
        "UPDATE t1 SET f1[5].q2 = 43; SELECT f1 FROM t1",
    );
    assert_eq!(updated_rows.len(), 1);
    assert_eq!(
        updated_rows[0].values[0],
        Value::Text("[5:5]={\"(42,43)\"}".to_owned())
    );
}

#[test]
fn width_bucket_accepts_text_array_threshold_literal() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(&engine, &session, "SELECT width_bucket(5, '{}')");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::Int(0));
}

#[test]
fn width_bucket_rejects_multidimensional_threshold_arrays() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(
            &session,
            "SELECT width_bucket(5, ARRAY[ARRAY[1,2], ARRAY[3,4]])",
        )
        .expect_err("multidimensional thresholds should fail");
    assert!(
        error
            .report()
            .message
            .contains("thresholds must be one-dimensional array"),
        "unexpected error: {error:?}"
    );
}

#[test]
fn width_bucket_accepts_timestamptz_threshold_arrays() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "SELECT width_bucket('2024-01-02 00:00:00+00'::timestamptz, ARRAY['2024-01-01 00:00:00+00'::timestamptz, '2024-01-03 00:00:00+00'::timestamptz])",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::Int(1));
}

#[test]
fn width_bucket_rejects_text_operand_with_integer_threshold_array() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(
            &session,
            "SELECT width_bucket('5'::text, ARRAY[3, 4]::integer[])",
        )
        .expect_err("text operand with integer thresholds should fail");
    assert_eq!(
        error.report().message,
        "function width_bucket(text, integer[]) does not exist"
    );
    assert_eq!(
        error.report().client_hint.as_deref(),
        Some(
            "No function matches the given name and argument types. You might need to add explicit type casts."
        )
    );
}

#[test]
fn trim_array_rejects_negative_or_too_large_trim_counts() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    for sql in [
        "SELECT trim_array(ARRAY[1, 2, 3], -1)",
        "SELECT trim_array(ARRAY[1, 2, 3], 10)",
    ] {
        let error = engine
            .execute_sql(&session, sql)
            .expect_err("invalid trim count should fail");
        assert_eq!(
            error.report().message,
            "number of elements to trim must be between 0 and 3"
        );
    }
}

#[test]
fn trim_array_with_explicit_lower_bound_returns_default_bounds() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "SELECT trim_array('[10:16]={1,2,3,4,5,6,7}'::bigint[], 2)",
    );

    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].values[0],
        Value::Array(vec![
            Value::Int(1),
            Value::Int(2),
            Value::Int(3),
            Value::Int(4),
            Value::Int(5),
        ])
    );
}

#[test]
fn array_sample_rejects_negative_or_too_large_sample_sizes() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    for sql in [
        "SELECT array_sample('{1,2,3,4,5,6}'::int[], -1)",
        "SELECT array_sample('{1,2,3,4,5,6}'::int[], 7)",
    ] {
        let error = engine
            .execute_sql(&session, sql)
            .expect_err("invalid sample size should fail");
        assert_eq!(
            error.report().message,
            "sample size must be between 0 and 6"
        );
    }
}

// ===================================================================
// array_cat function
// ===================================================================

#[test]
fn array_cat_basic() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(&session, "SELECT array_cat(ARRAY[1, 2], ARRAY[3, 4])")
        .expect("array_cat");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(
                rows[0].values[0],
                Value::Array(vec![
                    Value::Int(1),
                    Value::Int(2),
                    Value::Int(3),
                    Value::Int(4)
                ])
            );
        }
        other => panic!("expected query, got {other:?}"),
    }
}

#[test]
fn array_cat_wraps_lower_dimensional_left_operand() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT array_cat(ARRAY[1, 2], ARRAY[[3, 4], [5, 6]])",
        )
        .expect("array_cat 1d + 2d");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(
                rows[0].values[0],
                Value::Array(vec![
                    Value::Array(vec![Value::Int(1), Value::Int(2)]),
                    Value::Array(vec![Value::Int(3), Value::Int(4)]),
                    Value::Array(vec![Value::Int(5), Value::Int(6)]),
                ])
            );
        }
        other => panic!("expected query, got {other:?}"),
    }
}

#[test]
fn array_cat_wraps_lower_dimensional_right_operand() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT array_cat(ARRAY[[3, 4], [5, 6]], ARRAY[1, 2])",
        )
        .expect("array_cat 2d + 1d");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(
                rows[0].values[0],
                Value::Array(vec![
                    Value::Array(vec![Value::Int(3), Value::Int(4)]),
                    Value::Array(vec![Value::Int(5), Value::Int(6)]),
                    Value::Array(vec![Value::Int(1), Value::Int(2)]),
                ])
            );
        }
        other => panic!("expected query, got {other:?}"),
    }
}

#[test]
fn array_concat_operator_wraps_lower_dimensional_right_operand() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(&session, "SELECT ARRAY[[1,2],[3,4]] || ARRAY[5,6]")
        .expect("array concat operator");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(
                rows[0].values[0],
                Value::Array(vec![
                    Value::Array(vec![Value::Int(1), Value::Int(2)]),
                    Value::Array(vec![Value::Int(3), Value::Int(4)]),
                    Value::Array(vec![Value::Int(5), Value::Int(6)]),
                ])
            );
        }
        other => panic!("expected query, got {other:?}"),
    }
}

#[test]
fn array_cat_null_left() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE t (id INT NOT NULL, vals INT[])")
        .expect("create table");
    engine
        .execute_sql(&session, "INSERT INTO t VALUES (1, NULL)")
        .expect("insert");

    let results = engine
        .execute_sql(&session, "SELECT array_cat(vals, ARRAY[3, 4]) FROM t")
        .expect("array_cat null left");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(
                rows[0].values[0],
                Value::Array(vec![Value::Int(3), Value::Int(4)])
            );
        }
        other => panic!("expected query, got {other:?}"),
    }
}

#[test]
fn array_cat_both_null() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE t (id INT NOT NULL, a INT[], b INT[])",
        )
        .expect("create table");
    engine
        .execute_sql(&session, "INSERT INTO t VALUES (1, NULL, NULL)")
        .expect("insert");

    let results = engine
        .execute_sql(&session, "SELECT array_cat(a, b) FROM t")
        .expect("array_cat both null");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows[0].values[0], Value::Null);
        }
        other => panic!("expected query, got {other:?}"),
    }
}

// ===================================================================
// array_append function
// ===================================================================

#[test]
fn array_append_basic() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(&session, "SELECT array_append(ARRAY[1, 2], 3)")
        .expect("array_append");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(
                rows[0].values[0],
                Value::Array(vec![Value::Int(1), Value::Int(2), Value::Int(3)])
            );
        }
        other => panic!("expected query, got {other:?}"),
    }
}

#[test]
fn array_append_null_array() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE t (id INT NOT NULL, vals INT[])")
        .expect("create table");
    engine
        .execute_sql(&session, "INSERT INTO t VALUES (1, NULL)")
        .expect("insert");

    let results = engine
        .execute_sql(&session, "SELECT array_append(vals, 42) FROM t")
        .expect("array_append null");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows[0].values[0], Value::Array(vec![Value::Int(42)]));
        }
        other => panic!("expected query, got {other:?}"),
    }
}

// ===================================================================
// array_prepend function
// ===================================================================

#[test]
fn array_prepend_basic() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(&session, "SELECT array_prepend(1, ARRAY[2, 3])")
        .expect("array_prepend");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(
                rows[0].values[0],
                Value::Array(vec![Value::Int(1), Value::Int(2), Value::Int(3)])
            );
        }
        other => panic!("expected query, got {other:?}"),
    }
}

#[test]
fn array_prepend_null_array() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE t (id INT NOT NULL, vals INT[])")
        .expect("create table");
    engine
        .execute_sql(&session, "INSERT INTO t VALUES (1, NULL)")
        .expect("insert");

    let results = engine
        .execute_sql(&session, "SELECT array_prepend(42, vals) FROM t")
        .expect("array_prepend null");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows[0].values[0], Value::Array(vec![Value::Int(42)]));
        }
        other => panic!("expected query, got {other:?}"),
    }
}

// ===================================================================
// array_to_string function
// ===================================================================

#[test]
fn array_to_string_basic() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(&session, "SELECT array_to_string(ARRAY[1, 2, 3], ', ')")
        .expect("array_to_string");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows[0].values[0], Value::Text("1, 2, 3".into()));
        }
        other => panic!("expected query, got {other:?}"),
    }
}

#[test]
fn array_to_string_with_nulls_skipped() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE t (id INT NOT NULL, vals INT[])")
        .expect("create table");
    engine
        .execute_sql(&session, "INSERT INTO t VALUES (1, ARRAY[1, NULL, 3])")
        .expect("insert");

    let results = engine
        .execute_sql(&session, "SELECT array_to_string(vals, '-') FROM t")
        .expect("array_to_string with nulls");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            // NULLs should be skipped when no null_string is provided
            assert_eq!(rows[0].values[0], Value::Text("1-3".into()));
        }
        other => panic!("expected query, got {other:?}"),
    }
}

#[test]
fn array_to_string_with_null_replacement() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE t (id INT NOT NULL, vals INT[])")
        .expect("create table");
    engine
        .execute_sql(&session, "INSERT INTO t VALUES (1, ARRAY[1, NULL, 3])")
        .expect("insert");

    let results = engine
        .execute_sql(&session, "SELECT array_to_string(vals, ', ', 'N/A') FROM t")
        .expect("array_to_string with null replacement");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows[0].values[0], Value::Text("1, N/A, 3".into()));
        }
        other => panic!("expected query, got {other:?}"),
    }
}

#[test]
fn array_to_string_text_array() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT array_to_string(ARRAY['a', 'b', 'c'], '|')",
        )
        .expect("array_to_string text");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows[0].values[0], Value::Text("a|b|c".into()));
        }
        other => panic!("expected query, got {other:?}"),
    }
}
