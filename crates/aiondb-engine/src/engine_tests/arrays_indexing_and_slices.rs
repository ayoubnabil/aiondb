use super::*;

#[test]
fn update_array_element_assignment() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE t (id INT NOT NULL, vals INT[])")
        .expect("create table");
    engine
        .execute_sql(&session, "INSERT INTO t VALUES (1, ARRAY[10, 20, 30])")
        .expect("insert");
    engine
        .execute_sql(&session, "UPDATE t SET vals[2] = 99 WHERE id = 1")
        .expect("update array element");

    let results = engine
        .execute_sql(&session, "SELECT vals FROM t WHERE id = 1")
        .expect("select");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(
                rows[0].values[0],
                Value::Array(vec![Value::Int(10), Value::Int(99), Value::Int(30)])
            );
        }
        other => panic!("expected query, got {other:?}"),
    }
}

#[test]
fn update_array_element_assignment_extends_array() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE t (id INT NOT NULL, vals INT[])")
        .expect("create table");
    engine
        .execute_sql(&session, "INSERT INTO t VALUES (1, ARRAY[1, 2])")
        .expect("insert");
    engine
        .execute_sql(&session, "UPDATE t SET vals[5] = 50 WHERE id = 1")
        .expect("extend array");

    let results = engine
        .execute_sql(&session, "SELECT vals FROM t WHERE id = 1")
        .expect("select");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(
                rows[0].values[0],
                Value::Array(vec![
                    Value::Int(1),
                    Value::Int(2),
                    Value::Null,
                    Value::Null,
                    Value::Int(50)
                ])
            );
        }
        other => panic!("expected query, got {other:?}"),
    }
}

#[test]
fn update_array_element_assignment_preserves_zero_lower_bound() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE t (id INT NOT NULL, vals FLOAT8[])")
        .expect("create table");
    engine
        .execute_sql(&session, "INSERT INTO t VALUES (1, NULL)")
        .expect("insert");
    engine
        .execute_sql(&session, "UPDATE t SET vals[0] = 1.1 WHERE id = 1")
        .expect("set lower-bound element");
    engine
        .execute_sql(&session, "UPDATE t SET vals[1] = 2.2 WHERE id = 1")
        .expect("extend lower-bound array");

    let results = engine
        .execute_sql(
            &session,
            "SELECT vals, vals[0], array_lower(vals, 1), array_upper(vals, 1), array_dims(vals) FROM t WHERE id = 1",
        )
        .expect("select");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::Text("[0:1]={1.1,2.2}".to_owned()));
            assert_eq!(rows[0].values[1], Value::Double(1.1));
            assert_eq!(rows[0].values[2], Value::Int(0));
            assert_eq!(rows[0].values[3], Value::Int(1));
            assert_eq!(rows[0].values[4], Value::Text("[0:1]".to_owned()));
        }
        other => panic!("expected query, got {other:?}"),
    }
}

#[test]
fn seq_scan_after_array_updates_uses_latest_visible_heap_order() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        r#"
        CREATE TABLE arrtest (a INT[], b INT[][][], c TEXT[]);
        INSERT INTO arrtest (a[1:5], b[1:1][1:2][1:2], c)
            VALUES ('{1,2,3,4,5}', '{{{0,0},{1,2}}}', '{}');
        INSERT INTO arrtest (a, b[1:2][1:2], c)
            VALUES ('{11,12,23}', '{{3,4},{4,5}}', '{"foobar"}');
        INSERT INTO arrtest (a, b[1:2], c)
            VALUES ('{}', '{3,4}', '{foo,bar}');
        UPDATE arrtest
           SET a[1:2] = '{16,25}'
         WHERE NOT a = '{}'::INT[];
        UPDATE arrtest
           SET b[1:1][1:1][1:2] = '{113,117}',
               b[1:1][1:2][2:2] = '{142,147}'
         WHERE array_dims(b) = '[1:1][1:2][1:2]';
        UPDATE arrtest
           SET c[2:2] = '{"new_word"}'
         WHERE array_dims(c) IS NOT NULL;
        SELECT a, c FROM arrtest;
        "#,
    );

    assert_eq!(rows.len(), 3);
    assert_eq!(
        rows[0].values,
        vec![
            Value::Array(vec![
                Value::Int(16),
                Value::Int(25),
                Value::Int(3),
                Value::Int(4),
                Value::Int(5),
            ]),
            Value::Array(Vec::new()),
        ]
    );
    assert_eq!(
        rows[1].values,
        vec![
            Value::Array(Vec::new()),
            Value::Array(vec![
                Value::Text("foo".to_owned()),
                Value::Text("new_word".to_owned()),
            ]),
        ]
    );
    assert_eq!(
        rows[2].values,
        vec![
            Value::Array(vec![Value::Int(16), Value::Int(25), Value::Int(23)]),
            Value::Array(vec![
                Value::Text("foobar".to_owned()),
                Value::Text("new_word".to_owned()),
            ]),
        ]
    );
}

#[test]
fn update_array_slice_assignment() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE t (id INT NOT NULL, vals INT[])")
        .expect("create table");
    engine
        .execute_sql(&session, "INSERT INTO t VALUES (1, ARRAY[10, 20, 30, 40])")
        .expect("insert");
    engine
        .execute_sql(
            &session,
            "UPDATE t SET vals[2:3] = ARRAY[200, 300] WHERE id = 1",
        )
        .expect("update array slice");

    let results = engine
        .execute_sql(&session, "SELECT vals FROM t WHERE id = 1")
        .expect("select");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(
                rows[0].values[0],
                Value::Array(vec![
                    Value::Int(10),
                    Value::Int(200),
                    Value::Int(300),
                    Value::Int(40)
                ])
            );
        }
        other => panic!("expected query, got {other:?}"),
    }
}

#[test]
fn update_array_full_slice_rejects_too_small_source() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE t (id INT NOT NULL, vals INT[])")
        .expect("create table");
    engine
        .execute_sql(
            &session,
            "INSERT INTO t VALUES (1, ARRAY[10, 20, 30, 40, 50])",
        )
        .expect("insert");

    let err = engine
        .execute_sql(
            &session,
            "UPDATE t SET vals[:] = ARRAY[1, 2, 3] WHERE id = 1",
        )
        .expect_err("full slice assignment with too few elements should fail");

    assert!(
        format!("{err}").contains("source array too small"),
        "unexpected error: {err}"
    );
}

#[test]
fn update_multidimensional_array_slice_assignment_composes_same_column() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE t (b INT[][][])")
        .expect("create table");
    engine
        .execute_sql(&session, "INSERT INTO t VALUES ('{{{0,0},{1,2}}}')")
        .expect("insert");
    engine
        .execute_sql(
            &session,
            "UPDATE t SET b[1:1][1:1][1:2] = '{113,117}', b[1:1][1:2][2:2] = '{142,147}'",
        )
        .expect("update multidimensional slice");

    let results = engine
        .execute_sql(&session, "SELECT b FROM t")
        .expect("select");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(
                rows[0].values[0],
                Value::Array(vec![Value::Array(vec![
                    Value::Array(vec![Value::Int(113), Value::Int(142)]),
                    Value::Array(vec![Value::Int(1), Value::Int(147)]),
                ])])
            );
        }
        other => panic!("expected query, got {other:?}"),
    }
}

#[test]
fn update_open_multidimensional_slice_truncates_to_existing_zero_bounds() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE t (b INT[][])")
        .expect("create table");
    engine
        .execute_sql(
            &session,
            "INSERT INTO t VALUES ('[0:2][0:2]={{1,2,3},{4,5,6},{7,8,9}}')",
        )
        .expect("insert lower-bound array");
    engine
        .execute_sql(&session, "UPDATE t SET b[2:][2:] = '{{25,26},{28,29}}'")
        .expect("update open multidimensional slice");

    let results = engine
        .execute_sql(&session, "SELECT b FROM t")
        .expect("select");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(
                rows[0].values[0],
                Value::Text("[0:2][0:2]={{1,2,3},{4,5,6},{7,8,25}}".to_owned())
            );
        }
        other => panic!("expected query, got {other:?}"),
    }
}

#[test]
fn update_array_open_slice_on_null_requires_explicit_bounds() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE t (id INT NOT NULL, vals INT[])")
        .expect("create table");
    engine
        .execute_sql(&session, "INSERT INTO t VALUES (1, NULL)")
        .expect("insert");

    let err = engine
        .execute_sql(
            &session,
            "UPDATE t SET vals[:] = ARRAY[1, 2, 3, 4, 5] WHERE id = 1",
        )
        .expect_err("open slice assignment on null should fail");

    assert!(
        format!("{err}").contains("array slice subscript must provide both boundaries"),
        "unexpected error: {err}"
    );
    assert_eq!(
        err.report().client_detail.as_deref(),
        Some(
            "When assigning to a slice of an empty array value, slice boundaries must be fully specified."
        )
    );
}

#[test]
fn update_array_element_assignment_rejects_huge_index() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE t (id INT NOT NULL, vals INT[])")
        .expect("create table");
    engine
        .execute_sql(&session, "INSERT INTO t VALUES (1, ARRAY[1, 2])")
        .expect("insert");

    let err = engine
        .execute_sql(&session, "UPDATE t SET vals[2147483647] = 42 WHERE id = 1")
        .expect_err("huge subscript should fail");

    assert_eq!(err.sqlstate(), aiondb_core::SqlState::ProgramLimitExceeded);
    assert!(
        format!("{err}").contains("too many elements"),
        "unexpected error: {err}"
    );
}

#[test]
fn select_non_subscriptable_expression_errors() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let err = engine
        .execute_sql(&session, "SELECT (now())[1]")
        .expect_err("subscripting timestamptz should fail");

    assert!(
        format!("{err}")
            .contains("cannot subscript type timestamp with time zone because it does not support subscripting"),
        "unexpected error: {err}"
    );
}

#[test]
fn select_fixed_length_point_slice_errors() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE t (f1 point)")
        .expect("create table");
    engine
        .execute_sql(&session, "INSERT INTO t VALUES ('10.0,10.0')")
        .expect("insert point text");

    let err = engine
        .execute_sql(&session, "SELECT f1[0:1] FROM t")
        .expect_err("fixed-length slices should fail");

    assert!(
        format!("{err}").contains("slices of fixed-length arrays not implemented"),
        "unexpected error: {err}"
    );
}

#[test]
fn update_point_subscript_assignment_matches_normalized_text() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE t (f1 point)")
        .expect("create table");
    engine
        .execute_sql(&session, "INSERT INTO t VALUES ('10.0,10.0')")
        .expect("insert point text");

    let rows = query_rows(
        &engine,
        &session,
        "UPDATE t SET f1[0] = NULL WHERE f1::text = '(10,10)'::point::text RETURNING *",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::Text("(10,10)".to_owned()));

    let rows = query_rows(
        &engine,
        &session,
        "UPDATE t SET f1[0] = -10, f1[1] = -10 WHERE f1::text = '(10,10)'::point::text RETURNING *",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::Text("(-10,-10)".to_owned()));

    let rows = query_rows(
        &engine,
        &session,
        "SELECT f1 FROM t WHERE f1::text = '(-10,-10)'::point::text",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::Text("(-10,-10)".to_owned()));
}

#[test]
fn update_point_subscript_assignment_rejects_out_of_range() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE t (f1 point)")
        .expect("create table");
    engine
        .execute_sql(&session, "INSERT INTO t VALUES ('(-10,-10)')")
        .expect("insert point");

    let err = engine
        .execute_sql(
            &session,
            "UPDATE t SET f1[3] = 10 WHERE f1::text = '(-10,-10)'::point::text RETURNING *",
        )
        .expect_err("out-of-range point subscript should fail");

    assert!(
        format!("{err}").contains("array subscript out of range"),
        "unexpected error: {err}"
    );
}

#[test]
fn temp_point_table_subscript_updates_shadow_public_table() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE public.point_tbl (f1 point)")
        .expect("create public point table");
    engine
        .execute_sql(
            &session,
            "INSERT INTO public.point_tbl VALUES ('10.0,10.0')",
        )
        .expect("insert public point");
    engine
        .execute_sql(
            &session,
            "CREATE TEMP TABLE point_tbl AS SELECT * FROM public.point_tbl",
        )
        .expect("create temp point shadow");
    engine
        .execute_sql(&session, "INSERT INTO point_tbl (f1) VALUES (NULL)")
        .expect("insert null temp point");

    let rows = query_rows(
        &engine,
        &session,
        "UPDATE point_tbl SET f1[0] = -10, f1[1] = -10 WHERE f1::text = '(10,10)'::point::text RETURNING *",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::Text("(-10,-10)".to_owned()));

    let temp_rows = query_rows(
        &engine,
        &session,
        "SELECT f1 FROM point_tbl ORDER BY f1::text NULLS LAST",
    );
    assert_eq!(temp_rows.len(), 2);
    assert_eq!(temp_rows[0].values[0], Value::Text("(-10,-10)".to_owned()));
    assert_eq!(temp_rows[1].values[0], Value::Null);

    let public_rows = query_rows(&engine, &session, "SELECT f1 FROM public.point_tbl");
    assert_eq!(public_rows.len(), 1);
    assert_eq!(
        public_rows[0].values[0],
        Value::Text("10.0,10.0".to_owned())
    );

    let err = engine
        .execute_sql(
            &session,
            "UPDATE point_tbl SET f1[3] = 10 WHERE f1::text = '(-10,-10)'::point::text RETURNING *",
        )
        .expect_err("out-of-range point subscript on temp table should fail");
    assert!(
        format!("{err}").contains("array subscript out of range"),
        "unexpected error: {err}"
    );
}

#[test]
fn full_point_tbl_regress_sequence_errors_on_out_of_range_subscript() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE public.point_tbl (f1 point)")
        .expect("create public point table");
    engine
        .execute_sql(
            &session,
            "INSERT INTO public.point_tbl (f1) VALUES
             ('(0.0,0.0)'),
             ('(-10.0,0.0)'),
             ('(-3.0,4.0)'),
             ('(5.1, 34.5)'),
             ('(-5.0,-12.0)'),
             ('(1e-300,-1e-300)'),
             ('(1e+300,Inf)'),
             ('(Inf,1e+300)'),
             (' ( Nan , NaN ) '),
             ('10.0,10.0')",
        )
        .expect("seed public point table");
    engine
        .execute_sql(
            &session,
            "CREATE TEMP TABLE point_tbl AS SELECT * FROM public.point_tbl",
        )
        .expect("create temp point shadow");
    engine
        .execute_sql(&session, "INSERT INTO point_tbl (f1) VALUES (NULL)")
        .expect("insert null temp point");

    query_rows(
        &engine,
        &session,
        "UPDATE point_tbl SET f1[0] = 10 WHERE f1 IS NULL RETURNING *",
    );
    query_rows(
        &engine,
        &session,
        "INSERT INTO point_tbl(f1[0]) VALUES(0) RETURNING *",
    );
    let rows = query_rows(
        &engine,
        &session,
        "UPDATE point_tbl SET f1[0] = NULL WHERE f1::text = '(10,10)'::point::text RETURNING *",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::Text("(10,10)".to_owned()));
    let rows = query_rows(
        &engine,
        &session,
        "UPDATE point_tbl SET f1[0] = -10, f1[1] = -10 WHERE f1::text = '(10,10)'::point::text RETURNING *",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::Text("(-10,-10)".to_owned()));

    let err = engine
        .execute_sql(
            &session,
            "UPDATE point_tbl SET f1[3] = 10 WHERE f1::text = '(-10,-10)'::point::text RETURNING *",
        )
        .expect_err("out-of-range point subscript should fail after full regress sequence");
    assert!(
        format!("{err}").contains("array subscript out of range"),
        "unexpected error: {err}"
    );
}

#[test]
fn select_with_extra_subscripts_returns_null() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(&session, "SELECT ('{bar,foo}'::text[])[1][1]")
        .expect("nested oversubscript should return null");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::Null);
        }
        other => panic!("expected query, got {other:?}"),
    }
}

#[test]
fn select_array_slice_returns_subarray() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(&session, "SELECT ('{10,20,30,40}'::int[])[2:3]")
        .expect("slice should succeed");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(
                rows[0].values[0],
                Value::Array(vec![Value::Int(20), Value::Int(30)])
            );
        }
        other => panic!("expected query, got {other:?}"),
    }
}

#[test]
fn select_array_open_slices_return_subarray() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let prefix = engine
        .execute_sql(&session, "SELECT ('{10,20,30,40}'::int[])[ :2 ]")
        .expect("prefix slice should succeed");
    let suffix = engine
        .execute_sql(&session, "SELECT ('{10,20,30,40}'::int[])[3:]")
        .expect("suffix slice should succeed");
    let full = engine
        .execute_sql(&session, "SELECT ('{10,20,30,40}'::int[] )[:]")
        .expect("full slice should succeed");

    let StatementResult::Query { rows, .. } = &prefix[0] else {
        panic!("expected query");
    };
    assert_eq!(
        rows[0].values[0],
        Value::Array(vec![Value::Int(10), Value::Int(20)])
    );

    let StatementResult::Query { rows, .. } = &suffix[0] else {
        panic!("expected query");
    };
    assert_eq!(
        rows[0].values[0],
        Value::Array(vec![Value::Int(30), Value::Int(40)])
    );

    let StatementResult::Query { rows, .. } = &full[0] else {
        panic!("expected query");
    };
    assert_eq!(
        rows[0].values[0],
        Value::Array(vec![
            Value::Int(10),
            Value::Int(20),
            Value::Int(30),
            Value::Int(40)
        ])
    );
}

#[test]
fn select_mixed_slice_and_scalar_subscripts_follow_postgres_slice_rules() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT ('{{1,2,3},{4,5,6},{7,8,9}}'::int[])[1:2][2]",
        )
        .expect("mixed slice should succeed");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(
                rows[0].values[0],
                Value::Array(vec![
                    Value::Array(vec![Value::Int(1), Value::Int(2)]),
                    Value::Array(vec![Value::Int(4), Value::Int(5)]),
                ])
            );
        }
        other => panic!("expected query, got {other:?}"),
    }
}

#[test]
fn select_mixed_slice_and_scalar_subscripts_respect_explicit_inner_lower_bounds() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT ('[0:2][0:2]={{1,2,3},{4,5,6},{7,8,9}}'::int[])[1:2][2]",
        )
        .expect("lower-bound mixed slice should succeed");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(
                rows[0].values[0],
                Value::Array(vec![
                    Value::Array(vec![Value::Int(5), Value::Int(6)]),
                    Value::Array(vec![Value::Int(8), Value::Int(9)]),
                ])
            );
        }
        other => panic!("expected query, got {other:?}"),
    }
}

#[test]
fn select_slice_chain_oversubscripting_returns_empty_array() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(&session, "SELECT ('{{3,4},{4,5}}'::int[])[1:1][1:2][1:2]")
        .expect("oversubscripting slice chain should succeed");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::Array(Vec::new()));
        }
        other => panic!("expected query, got {other:?}"),
    }
}

#[test]
fn select_lower_bound_array_literal_uses_logical_indexes() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT ('[0:4]={1,2,3,4,5}'::int[])[0], ('[0:4]={1,2,3,4,5}'::int[])[2:], array_lower('[0:4]={1,2,3,4,5}'::int[], 1), array_upper('[0:4]={1,2,3,4,5}'::int[], 1), array_dims('[0:4]={1,2,3,4,5}'::int[])",
        )
        .expect("select should succeed");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::Int(1));
            assert_eq!(
                rows[0].values[1],
                Value::Array(vec![Value::Int(3), Value::Int(4), Value::Int(5)])
            );
            assert_eq!(rows[0].values[2], Value::Int(0));
            assert_eq!(rows[0].values[3], Value::Int(4));
            assert_eq!(rows[0].values[4], Value::Text("[0:4]".to_owned()));
        }
        other => panic!("expected query, got {other:?}"),
    }
}

#[test]
fn select_multidimensional_lower_bound_literal_normalizes_body_spacing() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT '[0:2][0:2]={{1,2,3}, {4,5,6}, {7,8,9}}'::int[]",
        )
        .expect("select should succeed");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(
                rows[0].values[0],
                Value::Text("[0:2][0:2]={{1,2,3},{4,5,6},{7,8,9}}".to_owned())
            );
        }
        other => panic!("expected query, got {other:?}"),
    }
}

#[test]
fn select_array_positions_respect_lower_bound_indexes() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT array_position('[2:4]={1,2,3}'::int[], 1), array_positions('[2:4]={1,2,3}'::int[], 1)",
        )
        .expect("select should succeed");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::Int(2));
            assert_eq!(rows[0].values[1], Value::Array(vec![Value::Int(2)]));
        }
        other => panic!("expected query, got {other:?}"),
    }
}

#[test]
fn lower_bound_array_shuffle_and_sample_keep_expected_dims() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT array_dims(array_shuffle('[-1:2][2:3]={{1,2},{3,NULL},{5,6},{7,8}}'::int[])), array_dims(array_sample('[-1:2][2:3]={{1,2},{3,NULL},{5,6},{7,8}}'::int[], 3))",
        )
        .expect("select should succeed");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::Text("[-1:2][2:3]".to_owned()));
            assert_eq!(rows[0].values[1], Value::Text("[1:3][2:3]".to_owned()));
        }
        other => panic!("expected query, got {other:?}"),
    }
}

#[test]
fn select_array_slice_with_null_bound_returns_null() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(&session, "SELECT ('{10,20,30,40}'::int[])[1:NULL]")
        .expect("slice should succeed");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::Null);
        }
        other => panic!("expected query, got {other:?}"),
    }
}

#[test]
fn select_with_more_than_six_array_dimensions_errors() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let err = engine
        .execute_sql(&session, "SELECT ('{}'::int[])[1][2][3][4][5][6][7]")
        .expect_err("too many array dimensions should fail");

    assert_eq!(err.sqlstate(), aiondb_core::SqlState::ProgramLimitExceeded);
    assert!(
        format!("{err}").contains("number of array dimensions (7) exceeds the maximum allowed (6)"),
        "unexpected error: {err}"
    );
}

// ===================================================================
// Multiple rows with arrays
// ===================================================================
