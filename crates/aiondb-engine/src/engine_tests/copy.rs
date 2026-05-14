use super::*;

#[test]
fn copy_to_exports_tab_delimited_text() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, name TEXT); \
             INSERT INTO items VALUES (1, 'apple'), (2, 'banana')",
        )
        .expect("setup");

    let results = engine
        .execute_sql(&session, "COPY items TO STDOUT")
        .expect("copy to");

    assert_eq!(results.len(), 1);
    match &results[0] {
        StatementResult::CopyOut { data, column_count } => {
            assert_eq!(*column_count, 2);
            let lines: Vec<&str> = data.lines().collect();
            assert_eq!(lines.len(), 2);
            // Each line is tab-delimited: id<TAB>name
            for line in &lines {
                let fields: Vec<&str> = line.split('\t').collect();
                assert_eq!(fields.len(), 2, "expected 2 columns per line");
            }
            // Verify values are present (order may vary by storage)
            let combined = data.clone();
            assert!(combined.contains("apple"));
            assert!(combined.contains("banana"));
        }
        other => panic!("expected CopyOut, got {other:?}"),
    }
}

#[test]
fn copy_query_to_stdout_uses_returning_rows() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE items (id INT, name TEXT)")
        .expect("create table");

    let error = engine
        .execute_sql(
            &session,
            "COPY (INSERT INTO items VALUES (1, 'apple') RETURNING id, name) TO STDOUT",
        )
        .expect("COPY(query) should succeed");

    assert_eq!(error.len(), 1);
    match &error[0] {
        StatementResult::CopyOut { data, column_count } => {
            assert_eq!(*column_count, 2);
            assert_eq!(data.trim(), "1\tapple");
        }
        other => panic!("expected CopyOut, got {other:?}"),
    }

    let select_results = engine
        .execute_sql(&session, "SELECT id, name FROM items")
        .expect("select");
    match &select_results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], aiondb_core::Value::Int(1));
            assert_eq!(
                rows[0].values[1],
                aiondb_core::Value::Text("apple".to_owned())
            );
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn copy_query_to_stdout_handles_update_returning_rows() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, name TEXT); \
             INSERT INTO items VALUES (1, 'apple')",
        )
        .expect("seed table");

    let results = engine
        .execute_sql(
            &session,
            "COPY (UPDATE items SET name = 'pear' WHERE id = 1 RETURNING id, name) TO STDOUT",
        )
        .expect("COPY(update) should succeed");

    assert_eq!(results.len(), 1);
    match &results[0] {
        StatementResult::CopyOut { data, column_count } => {
            assert_eq!(*column_count, 2);
            assert_eq!(data.trim(), "1\tpear");
        }
        other => panic!("expected CopyOut, got {other:?}"),
    }

    let select_results = engine
        .execute_sql(&session, "SELECT id, name FROM items")
        .expect("select");
    match &select_results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], aiondb_core::Value::Int(1));
            assert_eq!(
                rows[0].values[1],
                aiondb_core::Value::Text("pear".to_owned())
            );
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn copy_query_without_returning_clause_is_rejected() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE items (id INT, name TEXT)")
        .expect("create table");

    let error = engine
        .execute_sql(
            &session,
            "COPY (INSERT INTO items VALUES (1, 'apple')) TO STDOUT",
        )
        .expect_err("COPY(query) without RETURNING should fail");
    assert!(
        format!("{error}").contains("COPY query must have a RETURNING clause"),
        "unexpected error: {error}"
    );
}

#[test]
fn copy_from_imports_tab_delimited_text() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE items (id INT, name TEXT)")
        .expect("create table");

    // Execute COPY FROM to get the CopyIn marker with table_id.
    let results = engine
        .execute_sql(&session, "COPY items FROM STDIN")
        .expect("copy from");

    assert_eq!(results.len(), 1);
    let table_id = match &results[0] {
        StatementResult::CopyIn { table_id, columns } => {
            assert_eq!(columns.len(), 2);
            assert_eq!(columns[0].name, "id");
            assert_eq!(columns[1].name, "name");
            *table_id
        }
        other => panic!("expected CopyIn, got {other:?}"),
    };

    // Supply COPY data through the engine API.
    let data = "1\tapple\n2\tbanana\n3\tcherry\n";
    let result = engine
        .execute_copy_from(&session, table_id, data)
        .expect("copy from data");

    match &result {
        StatementResult::Command { tag, rows_affected } => {
            assert_eq!(tag, "COPY");
            assert_eq!(*rows_affected, 3);
        }
        other => panic!("expected Command, got {other:?}"),
    }

    // Verify the data was inserted.
    let select_results = engine
        .execute_sql(&session, "SELECT id, name FROM items")
        .expect("select");

    assert_eq!(select_results.len(), 1);
    match &select_results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 3);
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn copy_from_accepts_legacy_with_delimiter_as_options() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items_legacy_copy (id INT, name TEXT, note TEXT)",
        )
        .expect("create table");

    super::copy_support::parse_copy_sql_options_for_test(
        "COPY items_legacy_copy FROM STDIN WITH DELIMITER AS ';' NULL AS ''",
        aiondb_parser::CopyDirection::From,
    )
    .expect("direct parse should succeed");

    let results = engine
        .execute_sql(
            &session,
            "COPY items_legacy_copy FROM STDIN WITH DELIMITER AS ';' NULL AS ''",
        )
        .expect("copy from");

    let table_id = match &results[0] {
        StatementResult::CopyIn { table_id, .. } => *table_id,
        other => panic!("expected CopyIn, got {other:?}"),
    };

    engine
        .execute_copy_from(&session, table_id, "1;alice;\n")
        .expect("copy from data");

    let select_results = engine
        .execute_sql(&session, "SELECT id, name, note FROM items_legacy_copy")
        .expect("select");
    match &select_results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], aiondb_core::Value::Int(1));
            assert_eq!(
                rows[0].values[1],
                aiondb_core::Value::Text("alice".to_owned())
            );
            assert_eq!(rows[0].values[2], aiondb_core::Value::Null);
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn copy_from_accepts_date_literal_for_timestamp_column() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE copy_timestamp_date_only (id INT, ts TIMESTAMP)",
        )
        .expect("create table");

    let results = engine
        .execute_sql(&session, "COPY copy_timestamp_date_only FROM STDIN")
        .expect("copy from");

    let table_id = match &results[0] {
        StatementResult::CopyIn { table_id, .. } => *table_id,
        other => panic!("expected CopyIn, got {other:?}"),
    };

    engine
        .execute_copy_from(&session, table_id, "1\t'2022-07-04'\n")
        .expect("copy from data");

    let select_results = engine
        .execute_sql(
            &session,
            "SELECT id, ts::text FROM copy_timestamp_date_only",
        )
        .expect("select");
    match &select_results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], aiondb_core::Value::Int(1));
            assert_eq!(
                rows[0].values[1],
                aiondb_core::Value::Text("2022-07-04 0:00:00.0".to_owned())
            );
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn copy_from_default_marker_uses_column_default() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE copy_default_marker (
                id INT NOT NULL DEFAULT 7,
                text_value TEXT NOT NULL DEFAULT 'test',
                ts_value TIMESTAMP NOT NULL DEFAULT '2022-07-05'
            )",
        )
        .expect("create table");

    let results = engine
        .execute_sql(
            &session,
            "COPY copy_default_marker FROM STDIN WITH (DEFAULT '\\D')",
        )
        .expect("copy from");

    let table_id = match &results[0] {
        StatementResult::CopyIn { table_id, .. } => *table_id,
        other => panic!("expected CopyIn, got {other:?}"),
    };

    engine
        .execute_copy_from(&session, table_id, "1\t\\D\t'2022-07-04'\n")
        .expect("copy from data");

    let select_results = engine
        .execute_sql(
            &session,
            "SELECT id, text_value, ts_value::text FROM copy_default_marker",
        )
        .expect("select");
    match &select_results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], aiondb_core::Value::Int(1));
            assert_eq!(
                rows[0].values[1],
                aiondb_core::Value::Text("test".to_owned())
            );
            assert_eq!(
                rows[0].values[2],
                aiondb_core::Value::Text("2022-07-04 0:00:00.0".to_owned())
            );
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn copy_from_default_marker_errors_when_column_has_no_default() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE copy_default_marker_error (
                id INT PRIMARY KEY,
                text_value TEXT NOT NULL DEFAULT 'test',
                ts_value TIMESTAMP NOT NULL DEFAULT '2022-07-05'
            )",
        )
        .expect("create table");

    let results = engine
        .execute_sql(
            &session,
            "COPY copy_default_marker_error FROM STDIN WITH (DEFAULT '\\D')",
        )
        .expect("copy from");

    let table_id = match &results[0] {
        StatementResult::CopyIn { table_id, .. } => *table_id,
        other => panic!("expected CopyIn, got {other:?}"),
    };

    let error = engine
        .execute_copy_from(&session, table_id, "\\D\tvalue\t'2022-07-04'\n")
        .expect_err("copy from data should fail");
    let error_text = error.to_string();
    assert!(
        error_text.contains("unexpected default marker in COPY data"),
        "unexpected error: {error_text}"
    );
    assert!(
        error_text.contains("unexpected default marker in COPY data"),
        "unexpected error detail: {error_text}"
    );
}

#[test]
fn copy_from_csv_force_not_null_preserves_unquoted_empty_string() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE copy_force_not_null_csv (
                a INT NOT NULL,
                b TEXT NOT NULL,
                c TEXT
            )",
        )
        .expect("create table");

    let results = engine
        .execute_sql(
            &session,
            "COPY copy_force_not_null_csv (a, b, c) FROM STDIN WITH (FORMAT csv, FORCE_NOT_NULL(b), FORCE_NULL(c))",
        )
        .expect("copy from");

    let table_id = match &results[0] {
        StatementResult::CopyIn { table_id, .. } => *table_id,
        other => panic!("expected CopyIn, got {other:?}"),
    };

    engine
        .execute_copy_from(&session, table_id, "1,,\"\"\n")
        .expect("copy from data");

    let select_results = engine
        .execute_sql(&session, "SELECT b, c FROM copy_force_not_null_csv")
        .expect("select");
    match &select_results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], aiondb_core::Value::Text(String::new()));
            assert_eq!(rows[0].values[1], aiondb_core::Value::Null);
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn copy_from_csv_force_null_and_force_not_null_can_coexist() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE copy_force_null_csv (
                a INT NOT NULL,
                b TEXT NOT NULL,
                c TEXT,
                d TEXT
            )",
        )
        .expect("create table");

    let results = engine
        .execute_sql(
            &session,
            "COPY copy_force_null_csv (a, b, c, d) FROM STDIN WITH (FORMAT csv, FORCE_NOT_NULL(c,d), FORCE_NULL(c,d))",
        )
        .expect("copy from");

    let table_id = match &results[0] {
        StatementResult::CopyIn { table_id, .. } => *table_id,
        other => panic!("expected CopyIn, got {other:?}"),
    };

    engine
        .execute_copy_from(&session, table_id, "2,'a',,\"\"\n")
        .expect("copy from data");

    let select_results = engine
        .execute_sql(&session, "SELECT c, d FROM copy_force_null_csv")
        .expect("select");
    match &select_results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], aiondb_core::Value::Text(String::new()));
            assert_eq!(rows[0].values[1], aiondb_core::Value::Null);
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn copy_from_reports_undefined_table_when_target_was_dropped() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE copy_drop_target (id INT, name TEXT)",
        )
        .expect("create table");

    let results = engine
        .execute_sql(&session, "COPY copy_drop_target FROM STDIN")
        .expect("copy from");
    let table_id = match &results[0] {
        StatementResult::CopyIn { table_id, .. } => *table_id,
        other => panic!("expected CopyIn, got {other:?}"),
    };

    engine
        .execute_sql(&session, "DROP TABLE copy_drop_target")
        .expect("drop table");

    let error = engine
        .execute_copy_from(&session, table_id, "1\tapple\n")
        .expect_err("COPY FROM on dropped target should fail cleanly");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::UndefinedTable);
    assert!(
        error
            .report()
            .message
            .contains("COPY target table does not exist"),
        "expected undefined-table copy error, got: {}",
        error.report().message
    );
}

#[test]
fn copy_from_psql_filename_variable_is_rejected() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE array_op_test (seqno INT, i INT[], t TEXT[])",
        )
        .expect("create table");

    let error = engine
        .execute_sql(&session, "COPY array_op_test FROM :'filename'")
        .expect_err("COPY FROM :'filename' should be rejected");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::FeatureNotSupported);
    assert!(
        format!("{error}").contains("COPY FROM psql variable"),
        "unexpected error message: {error}"
    );
}

#[test]
fn copy_from_file_literal_is_rejected() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE copy_guard (id INT, name TEXT)")
        .expect("create table");

    let path = crate::test_support::unique_temp_path("engine-tests-copy", "copy-input")
        .with_extension("tsv");
    std::fs::write(&path, "1\tapple\n2\tbanana\n").expect("write copy input file");
    let sql = format!("COPY copy_guard FROM '{}'", path.display());

    let error = engine
        .execute_sql(&session, &sql)
        .expect_err("COPY FROM file should be rejected");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::FeatureNotSupported);
    assert!(
        format!("{error}").contains("COPY FROM file is not supported"),
        "unexpected error message: {error}"
    );

    let _ = std::fs::remove_file(path);
}

#[test]
fn copy_from_handles_null_values() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE items (id INT, name TEXT)")
        .expect("create table");

    let results = engine
        .execute_sql(&session, "COPY items FROM STDIN")
        .expect("copy from");

    let table_id = match &results[0] {
        StatementResult::CopyIn { table_id, .. } => *table_id,
        other => panic!("expected CopyIn, got {other:?}"),
    };

    // Use \N for NULL values (PostgreSQL COPY text format).
    let data = "1\t\\N\n\\N\tbanana\n";
    let result = engine
        .execute_copy_from(&session, table_id, data)
        .expect("copy from data");

    match &result {
        StatementResult::Command { tag, rows_affected } => {
            assert_eq!(tag, "COPY");
            assert_eq!(*rows_affected, 2);
        }
        other => panic!("expected Command, got {other:?}"),
    }

    // Verify NULL values were inserted.
    let select_results = engine
        .execute_sql(&session, "SELECT id, name FROM items")
        .expect("select");

    match &select_results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 2);
            // First row: id=1, name=NULL
            assert_eq!(rows[0].values[0], aiondb_core::Value::Int(1));
            assert_eq!(rows[0].values[1], aiondb_core::Value::Null);
            // Second row: id=NULL, name='banana'
            assert_eq!(rows[1].values[0], aiondb_core::Value::Null);
            assert_eq!(
                rows[1].values[1],
                aiondb_core::Value::Text("banana".to_owned())
            );
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn copy_from_error_marks_explicit_transaction_failed() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE copy_failed_tx (id INT NOT NULL)")
        .expect("create table");
    engine.execute_sql(&session, "BEGIN").expect("begin");

    let results = engine
        .execute_sql(&session, "COPY copy_failed_tx FROM STDIN")
        .expect("copy from");
    let table_id = match &results[0] {
        StatementResult::CopyIn { table_id, .. } => *table_id,
        other => panic!("expected CopyIn, got {other:?}"),
    };

    let error = engine
        .execute_copy_from(&session, table_id, "1\textra\n")
        .expect_err("COPY FROM should fail on column count mismatch");
    assert!(
        format!("{error}").contains("expected 1 columns, got 2"),
        "unexpected COPY FROM error: {error}"
    );

    let aborted_error = engine
        .execute_sql(&session, "SELECT 1")
        .expect_err("transaction should be aborted after COPY FROM error");
    assert_eq!(
        aborted_error.sqlstate(),
        aiondb_core::SqlState::InFailedSqlTransaction
    );

    engine.execute_sql(&session, "ROLLBACK").expect("rollback");
}

#[test]
fn copy_from_column_count_mismatch_error() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE items (id INT, name TEXT)")
        .expect("create table");

    let results = engine
        .execute_sql(&session, "COPY items FROM STDIN")
        .expect("copy from");

    let table_id = match &results[0] {
        StatementResult::CopyIn { table_id, .. } => *table_id,
        other => panic!("expected CopyIn, got {other:?}"),
    };

    // Wrong number of columns: 3 fields for 2-column table.
    let data = "1\tapple\textra\n";
    let result = engine.execute_copy_from(&session, table_id, data);
    assert!(result.is_err(), "should fail with column count mismatch");
    let err = result.unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("expected 2 columns, got 3"),
        "error should mention column mismatch: {msg}"
    );
}

#[test]
fn copy_from_with_various_data_types() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE typed (a INT, b BIGINT, c BOOLEAN, d REAL, e TEXT)",
        )
        .expect("create table");

    let results = engine
        .execute_sql(&session, "COPY typed FROM STDIN")
        .expect("copy from");

    let table_id = match &results[0] {
        StatementResult::CopyIn { table_id, .. } => *table_id,
        other => panic!("expected CopyIn, got {other:?}"),
    };

    let data = "42\t1000000\ttrue\t3.14\thello world\n\
                -1\t-9999\tfalse\t0.0\t\n";
    let result = engine
        .execute_copy_from(&session, table_id, data)
        .expect("copy from data");

    match &result {
        StatementResult::Command { tag, rows_affected } => {
            assert_eq!(tag, "COPY");
            assert_eq!(*rows_affected, 2);
        }
        other => panic!("expected Command, got {other:?}"),
    }

    let select_results = engine
        .execute_sql(&session, "SELECT a, b, c, d, e FROM typed")
        .expect("select");

    match &select_results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 2);
            // First row
            assert_eq!(rows[0].values[0], aiondb_core::Value::Int(42));
            assert_eq!(rows[0].values[1], aiondb_core::Value::BigInt(1_000_000));
            assert_eq!(rows[0].values[2], aiondb_core::Value::Boolean(true));
            assert_eq!(
                rows[0].values[4],
                aiondb_core::Value::Text("hello world".to_owned())
            );
            // Second row
            assert_eq!(rows[1].values[0], aiondb_core::Value::Int(-1));
            assert_eq!(rows[1].values[2], aiondb_core::Value::Boolean(false));
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn copy_to_escapes_special_characters_in_text() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE docs (id INT, content TEXT); \
             INSERT INTO docs VALUES (1, 'line1\nline2'), (2, 'col1\tcol2'), (3, 'back\\slash')",
        )
        .expect("setup");

    let results = engine
        .execute_sql(&session, "COPY docs TO STDOUT")
        .expect("copy to");

    match &results[0] {
        StatementResult::CopyOut { data, column_count } => {
            assert_eq!(*column_count, 2);
            // The output should have exactly 3 lines (one per row).
            let lines: Vec<&str> = data.lines().collect();
            assert_eq!(lines.len(), 3, "expected 3 rows in output");

            // Each line should have 2 tab-separated fields.
            for line in &lines {
                let fields: Vec<&str> = line.split('\t').collect();
                assert_eq!(fields.len(), 2, "expected 2 columns: {line}");
            }

            // Newlines, tabs, and backslashes in text should be escaped.
            assert!(
                data.contains("\\n"),
                "newline in text should be escaped to \\n"
            );
            assert!(
                data.contains("\\\\"),
                "backslash in text should be escaped to \\\\"
            );
        }
        other => panic!("expected CopyOut, got {other:?}"),
    }
}

#[test]
fn copy_to_outputs_null_as_backslash_n() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, name TEXT); \
             INSERT INTO items VALUES (1, NULL)",
        )
        .expect("setup");

    let results = engine
        .execute_sql(&session, "COPY items TO STDOUT")
        .expect("copy to");

    match &results[0] {
        StatementResult::CopyOut { data, column_count } => {
            assert_eq!(*column_count, 2);
            let lines: Vec<&str> = data.lines().collect();
            assert_eq!(lines.len(), 1);
            let fields: Vec<&str> = lines[0].split('\t').collect();
            assert_eq!(fields.len(), 2);
            assert_eq!(fields[1], "\\N", "NULL should be represented as \\N");
        }
        other => panic!("expected CopyOut, got {other:?}"),
    }
}

#[test]
fn copy_to_exports_blob_as_postgres_hex_text() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE blobs (id INT, payload BLOB); \
             INSERT INTO blobs VALUES (1, CAST('\\xdeadbeef' AS BLOB))",
        )
        .expect("setup");

    let results = engine
        .execute_sql(&session, "COPY blobs TO STDOUT")
        .expect("copy to");

    match &results[0] {
        StatementResult::CopyOut { data, column_count } => {
            assert_eq!(*column_count, 2);
            assert_eq!(data.trim(), "1\t\\xdeadbeef");
        }
        other => panic!("expected CopyOut, got {other:?}"),
    }
}

#[test]
fn copy_roundtrip_preserves_data() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE src (id INT, name TEXT, flag BOOLEAN); \
             INSERT INTO src VALUES (1, 'hello', true), (2, 'world', false), (3, NULL, NULL)",
        )
        .expect("setup source");

    // COPY TO to get the data
    let results = engine
        .execute_sql(&session, "COPY src TO STDOUT")
        .expect("copy to");

    let exported_data = match &results[0] {
        StatementResult::CopyOut { data, .. } => data.clone(),
        other => panic!("expected CopyOut, got {other:?}"),
    };

    // Create destination table and COPY FROM the exported data
    engine
        .execute_sql(
            &session,
            "CREATE TABLE dst (id INT, name TEXT, flag BOOLEAN)",
        )
        .expect("create dst");

    let copy_in = engine
        .execute_sql(&session, "COPY dst FROM STDIN")
        .expect("copy from");

    let table_id = match &copy_in[0] {
        StatementResult::CopyIn { table_id, .. } => *table_id,
        other => panic!("expected CopyIn, got {other:?}"),
    };

    engine
        .execute_copy_from(&session, table_id, &exported_data)
        .expect("import data");

    // Verify the data matches
    let src_rows = engine
        .execute_sql(&session, "SELECT id, name, flag FROM src ORDER BY id")
        .expect("select src");
    let dst_rows = engine
        .execute_sql(&session, "SELECT id, name, flag FROM dst ORDER BY id")
        .expect("select dst");

    assert_eq!(src_rows, dst_rows, "roundtrip data should match");
}

#[path = "copy_compat_aggregate.rs"]
mod copy_compat_aggregate;
