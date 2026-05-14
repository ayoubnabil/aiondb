use aiondb_core::{DataType, Row, Value};

use super::*;

// ---------------------------------------------------------------
// Helper to extract text column from a row by index
// ---------------------------------------------------------------

fn text_col(row: &Row, idx: usize) -> &str {
    match &row.values[idx] {
        Value::Text(s) => s.as_str(),
        other => panic!("expected Text, got {other:?}"),
    }
}

fn int_col(row: &Row, idx: usize) -> i32 {
    match &row.values[idx] {
        Value::Int(n) => *n,
        other => panic!("expected Int, got {other:?}"),
    }
}

fn is_null(row: &Row, idx: usize) -> bool {
    matches!(&row.values[idx], Value::Null)
}

// ---------------------------------------------------------------
// information_schema.tables
// ---------------------------------------------------------------

#[test]
fn tables_returns_created_tables() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE users (id INT NOT NULL, name TEXT); \
             CREATE TABLE orders (order_id INT NOT NULL, amount INT)",
        )
        .expect("create tables");

    let results = engine
        .execute_sql(&session, "SELECT * FROM information_schema.tables")
        .expect("query");

    let (columns, rows) = match &results[0] {
        StatementResult::Query { columns, rows } => (columns, rows),
        other => panic!("expected Query, got {other:?}"),
    };

    assert_eq!(columns.len(), 5);
    assert_eq!(columns[0].name, "table_catalog");
    assert_eq!(columns[1].name, "table_schema");
    assert_eq!(columns[2].name, "table_name");
    assert_eq!(columns[3].name, "table_type");
    assert_eq!(columns[4].name, "is_insertable_into");

    // Both tables should appear
    assert_eq!(rows.len(), 2);

    let table_names: Vec<&str> = rows.iter().map(|r| text_col(r, 2)).collect();
    assert!(
        table_names.contains(&"users"),
        "expected 'users' in {table_names:?}"
    );
    assert!(
        table_names.contains(&"orders"),
        "expected 'orders' in {table_names:?}"
    );

    // All rows should have catalog=aiondb, schema=public, type=BASE TABLE
    for row in rows {
        assert_eq!(text_col(row, 0), "default");
        assert_eq!(text_col(row, 1), "public");
        assert_eq!(text_col(row, 3), "BASE TABLE");
    }
}

#[test]
fn tables_include_views_as_views() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE users (id INT NOT NULL, name TEXT); \
             CREATE VIEW active_users AS SELECT id, name FROM users WHERE id > 0",
        )
        .expect("create table and view");

    let results = engine
        .execute_sql(
            &session,
            "SELECT table_name, table_type, is_insertable_into \
             FROM information_schema.tables \
             WHERE table_name = 'active_users'",
        )
        .expect("query");

    let rows = match &results[0] {
        StatementResult::Query { rows, .. } => rows,
        other => panic!("expected Query, got {other:?}"),
    };

    assert_eq!(rows.len(), 1);
    assert_eq!(text_col(&rows[0], 0), "active_users");
    assert_eq!(text_col(&rows[0], 1), "VIEW");
    assert_eq!(text_col(&rows[0], 2), "YES");
}

#[test]
fn information_schema_join_shapes_are_queryable() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE users (id INT NOT NULL, name TEXT)")
        .expect("create table");

    let rows = query_rows(
        &engine,
        &session,
        "SELECT t.table_name, c.column_name \
         FROM information_schema.tables t \
         JOIN information_schema.columns c \
           ON c.table_schema = t.table_schema \
          AND c.table_name = t.table_name \
         WHERE t.table_name = 'users' \
         ORDER BY c.column_name",
    );

    assert_eq!(rows.len(), 2);
    assert_eq!(text_col(&rows[0], 0), "users");
    assert_eq!(text_col(&rows[0], 1), "id");
    assert_eq!(text_col(&rows[1], 0), "users");
    assert_eq!(text_col(&rows[1], 1), "name");
}

#[test]
fn tables_empty_database_returns_no_rows() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(&session, "SELECT * FROM information_schema.tables")
        .expect("query");

    let rows = match &results[0] {
        StatementResult::Query { rows, .. } => rows,
        other => panic!("expected Query, got {other:?}"),
    };
    assert!(rows.is_empty());
}

// ---------------------------------------------------------------
// information_schema.columns
// ---------------------------------------------------------------

#[test]
fn columns_returns_column_metadata() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE products (id INT NOT NULL, name TEXT, price REAL NOT NULL)",
        )
        .expect("create table");

    let results = engine
        .execute_sql(&session, "SELECT * FROM information_schema.columns")
        .expect("query");

    let (columns, rows) = match &results[0] {
        StatementResult::Query { columns, rows } => (columns, rows),
        other => panic!("expected Query, got {other:?}"),
    };

    assert!(columns.len() >= 18);
    assert_eq!(columns[0].name, "table_catalog");
    assert_eq!(columns[3].name, "column_name");
    assert_eq!(columns[4].name, "ordinal_position");
    assert_eq!(columns[4].data_type, DataType::Int);
    assert_eq!(columns[6].name, "is_nullable");
    assert_eq!(columns[7].name, "data_type");
    assert_eq!(columns[14].name, "identity_maximum");
    assert_eq!(columns[16].name, "identity_cycle");
    assert!(columns
        .iter()
        .any(|column| column.name == "character_set_catalog"));

    // 3 columns from the products table
    assert_eq!(rows.len(), 3);

    // Verify id column
    let id_row = rows.iter().find(|r| text_col(r, 3) == "id").expect("id");
    assert_eq!(text_col(id_row, 0), "default");
    assert_eq!(text_col(id_row, 1), "public");
    assert_eq!(text_col(id_row, 2), "products");
    assert_eq!(int_col(id_row, 4), 1);
    assert_eq!(text_col(id_row, 6), "NO"); // NOT NULL
    assert_eq!(text_col(id_row, 7), "integer");

    // Verify name column
    let name_row = rows
        .iter()
        .find(|r| text_col(r, 3) == "name")
        .expect("name");
    assert_eq!(int_col(name_row, 4), 2);
    assert_eq!(text_col(name_row, 6), "YES"); // nullable
    assert_eq!(text_col(name_row, 7), "text");

    // Verify price column
    let price_row = rows
        .iter()
        .find(|r| text_col(r, 3) == "price")
        .expect("price");
    assert_eq!(int_col(price_row, 4), 3);
    assert_eq!(text_col(price_row, 6), "NO"); // NOT NULL
    assert_eq!(text_col(price_row, 7), "real");
}

#[test]
fn columns_multiple_tables() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE t1 (a INT NOT NULL); \
             CREATE TABLE t2 (b TEXT, c BOOLEAN)",
        )
        .expect("create tables");

    let results = engine
        .execute_sql(&session, "SELECT * FROM information_schema.columns")
        .expect("query");

    let rows = match &results[0] {
        StatementResult::Query { rows, .. } => rows,
        other => panic!("expected Query, got {other:?}"),
    };

    // t1 has 1 column, t2 has 2 columns = 3 total
    assert_eq!(rows.len(), 3);

    let t1_rows: Vec<_> = rows.iter().filter(|r| text_col(r, 2) == "t1").collect();
    assert_eq!(t1_rows.len(), 1);
    assert_eq!(text_col(t1_rows[0], 3), "a");

    let t2_names: Vec<&str> = rows
        .iter()
        .filter(|r| text_col(r, 2) == "t2")
        .map(|r| text_col(r, 3))
        .collect();
    assert_eq!(t2_names.len(), 2);
    assert!(t2_names.contains(&"b"));
    assert!(t2_names.contains(&"c"));
}

#[test]
fn columns_default_is_null_when_no_default() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE t (id INT NOT NULL)")
        .expect("create");

    let results = engine
        .execute_sql(&session, "SELECT * FROM information_schema.columns")
        .expect("query");

    let rows = match &results[0] {
        StatementResult::Query { rows, .. } => rows,
        other => panic!("expected Query, got {other:?}"),
    };

    assert_eq!(rows.len(), 1);
    // column_default (index 5) should be NULL
    assert!(is_null(&rows[0], 5));
}

#[test]
fn columns_expose_identity_sequence_metadata() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE things (id INT GENERATED BY DEFAULT AS IDENTITY, name TEXT)",
        )
        .expect("create table");

    let results = engine
        .execute_sql(
            &session,
            "SELECT column_default, is_identity, identity_generation, identity_start, \
                    identity_increment, identity_maximum, identity_minimum, identity_cycle \
             FROM information_schema.columns \
             WHERE table_name = 'things' AND column_name = 'id'",
        )
        .expect("query");

    let rows = match &results[0] {
        StatementResult::Query { rows, .. } => rows,
        other => panic!("expected Query, got {other:?}"),
    };

    assert_eq!(rows.len(), 1);
    assert!(is_null(&rows[0], 0));
    assert_eq!(text_col(&rows[0], 1), "YES");
    assert_eq!(text_col(&rows[0], 2), "BY DEFAULT");
    assert_eq!(text_col(&rows[0], 3), "1");
    assert_eq!(text_col(&rows[0], 4), "1");
    assert_eq!(text_col(&rows[0], 5), "2147483647");
    assert_eq!(text_col(&rows[0], 6), "1");
    assert_eq!(text_col(&rows[0], 7), "NO");
}

#[test]
fn columns_expose_identity_generation_and_sequence_options() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE widgets (id INT GENERATED ALWAYS AS IDENTITY (START WITH 7 INCREMENT BY 5 NO CYCLE), name TEXT)",
        )
        .expect("create table");

    let results = engine
        .execute_sql(
            &session,
            "SELECT is_identity, identity_generation, identity_start, identity_increment, \
                    identity_maximum, identity_minimum, identity_cycle \
             FROM information_schema.columns \
             WHERE table_name = 'widgets' AND column_name = 'id'",
        )
        .expect("query");

    let rows = match &results[0] {
        StatementResult::Query { rows, .. } => rows,
        other => panic!("expected Query, got {other:?}"),
    };

    assert_eq!(rows.len(), 1);
    assert_eq!(text_col(&rows[0], 0), "YES");
    assert_eq!(text_col(&rows[0], 1), "ALWAYS");
    assert_eq!(text_col(&rows[0], 2), "7");
    assert_eq!(text_col(&rows[0], 3), "5");
    assert_eq!(text_col(&rows[0], 4), "2147483647");
    assert_eq!(text_col(&rows[0], 5), "1");
    assert_eq!(text_col(&rows[0], 6), "NO");
}

#[test]
fn serial_columns_reflect_as_sequence_defaults_not_identity() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE serial_widgets (id SERIAL PRIMARY KEY, name TEXT)",
        )
        .expect("create table");

    let results = engine
        .execute_sql(
            &session,
            "SELECT column_default, is_identity, identity_generation \
             FROM information_schema.columns \
             WHERE table_name = 'serial_widgets' AND column_name = 'id'",
        )
        .expect("query");

    let rows = match &results[0] {
        StatementResult::Query { rows, .. } => rows,
        other => panic!("expected Query, got {other:?}"),
    };

    assert_eq!(rows.len(), 1);
    assert_eq!(
        text_col(&rows[0], 0),
        "nextval('serial_widgets_id_seq'::regclass)"
    );
    assert_eq!(text_col(&rows[0], 1), "NO");
    assert!(is_null(&rows[0], 2));
}

#[test]
fn views_returns_view_metadata() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE users (id INT NOT NULL, name TEXT); \
             CREATE VIEW active_users AS SELECT id, name FROM users WHERE id > 0",
        )
        .expect("create table and view");

    let results = engine
        .execute_sql(
            &session,
            "SELECT table_name, is_updatable, is_insertable_into, view_definition \
             FROM information_schema.views \
             WHERE table_name = 'active_users'",
        )
        .expect("query");

    let rows = match &results[0] {
        StatementResult::Query { rows, .. } => rows,
        other => panic!("expected Query, got {other:?}"),
    };

    assert_eq!(rows.len(), 1);
    assert_eq!(text_col(&rows[0], 0), "active_users");
    assert_eq!(text_col(&rows[0], 1), "YES");
    assert_eq!(text_col(&rows[0], 2), "YES");
    assert!(text_col(&rows[0], 3).contains("SELECT id, name FROM "));
    assert!(text_col(&rows[0], 3).contains("users"));
}

#[test]
fn information_schema_order_by_sorts_rows() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE z_table (id INT); \
             CREATE TABLE a_table (id INT)",
        )
        .expect("create tables");

    let results = engine
        .execute_sql(
            &session,
            "SELECT table_name FROM information_schema.tables ORDER BY table_name",
        )
        .expect("query");

    let rows = match &results[0] {
        StatementResult::Query { rows, .. } => rows,
        other => panic!("expected Query, got {other:?}"),
    };

    let names: Vec<&str> = rows.iter().map(|row| text_col(row, 0)).collect();
    assert_eq!(names, vec!["a_table", "z_table"]);
}

#[test]
fn information_schema_order_by_supports_non_projected_columns() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE products (id INT NOT NULL, name TEXT, price REAL NOT NULL)",
        )
        .expect("create table");

    let results = engine
        .execute_sql(
            &session,
            "SELECT column_name \
             FROM information_schema.columns \
             WHERE table_name = 'products' \
             ORDER BY ordinal_position DESC",
        )
        .expect("query");

    let rows = match &results[0] {
        StatementResult::Query { rows, .. } => rows,
        other => panic!("expected Query, got {other:?}"),
    };

    let names: Vec<&str> = rows.iter().map(|row| text_col(row, 0)).collect();
    assert_eq!(names, vec!["price", "name", "id"]);
}

#[test]
fn information_schema_sequences_excludes_owned_internal_sequences() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE users (id BIGINT NOT NULL DEFAULT nextval('users_id_seq')); \
             CREATE SEQUENCE public_seq",
        )
        .expect("create table and sequence");

    let results = engine
        .execute_sql(
            &session,
            "SELECT sequence_name FROM information_schema.sequences ORDER BY sequence_name",
        )
        .expect("query");

    let rows = match &results[0] {
        StatementResult::Query { rows, .. } => rows,
        other => panic!("expected Query, got {other:?}"),
    };

    let names: Vec<&str> = rows.iter().map(|row| text_col(row, 0)).collect();
    assert_eq!(names, vec!["public_seq"]);
}

// ---------------------------------------------------------------
// information_schema.schemata
// ---------------------------------------------------------------

#[test]
fn schemata_includes_public_and_information_schema() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(&session, "SELECT * FROM information_schema.schemata")
        .expect("query");

    let (columns, rows) = match &results[0] {
        StatementResult::Query { columns, rows } => (columns, rows),
        other => panic!("expected Query, got {other:?}"),
    };

    assert_eq!(columns.len(), 3);
    assert_eq!(columns[0].name, "catalog_name");
    assert_eq!(columns[1].name, "schema_name");
    assert_eq!(columns[2].name, "schema_owner");

    let schema_names: Vec<&str> = rows.iter().map(|r| text_col(r, 1)).collect();
    assert!(
        schema_names.contains(&"public"),
        "expected 'public' in {schema_names:?}"
    );
    assert!(
        schema_names.contains(&"information_schema"),
        "expected 'information_schema' in {schema_names:?}"
    );

    // All catalog names should match the active database name.
    for row in rows {
        assert_eq!(text_col(row, 0), "default");
    }
}

// ---------------------------------------------------------------
// Case-insensitive schema name
// ---------------------------------------------------------------

#[test]
fn case_insensitive_information_schema_name() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(&session, "SELECT * FROM INFORMATION_SCHEMA.schemata")
        .expect("query");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert!(!rows.is_empty(), "should return schemata rows");
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

// ---------------------------------------------------------------
// Unsupported information_schema table
// ---------------------------------------------------------------

#[test]
fn unsupported_information_schema_table_returns_error() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let err = engine
        .execute_sql(&session, "SELECT * FROM information_schema.nonexistent")
        .expect_err("should fail");
    assert_eq!(err.sqlstate(), aiondb_core::SqlState::UndefinedTable);
}

#[test]
fn information_schema_views_is_available() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(&session, "SELECT * FROM information_schema.views")
        .expect("views metadata should be queryable");

    match &results[0] {
        StatementResult::Query { columns, .. } => {
            assert_eq!(columns[0].name, "table_catalog");
            assert_eq!(columns[2].name, "table_name");
            assert_eq!(columns[3].name, "view_definition");
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn information_schema_order_by_is_supported() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT * FROM information_schema.tables ORDER BY table_name",
        )
        .expect("ORDER BY should succeed");

    match &results[0] {
        StatementResult::Query { .. } => {}
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn information_schema_count_projection_is_supported() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE count_me (id INT)")
        .expect("create table");

    let results = engine
        .execute_sql(
            &session,
            "SELECT count(*) > 0 AS ok FROM information_schema.tables",
        )
        .expect("COUNT projection should succeed");

    let rows = match &results[0] {
        StatementResult::Query { rows, .. } => rows,
        other => panic!("expected Query, got {other:?}"),
    };
    assert_eq!(rows.len(), 1);
    assert!(matches!(rows[0].values[0], Value::Boolean(true)));
}

#[test]
fn information_schema_numeric_literals_preserve_numeric_type_and_equality() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE numeric_literal_info (id INT)")
        .expect("create table");

    let results = engine
        .execute_sql(
            &session,
            "SELECT 1.5 AS n \
             FROM information_schema.tables \
             WHERE 1.0 = 1.00 AND table_name = 'numeric_literal_info'",
        )
        .expect("numeric literal projection should succeed");

    let (columns, rows) = match &results[0] {
        StatementResult::Query { columns, rows } => (columns, rows),
        other => panic!("expected Query, got {other:?}"),
    };
    assert_eq!(columns.len(), 1);
    assert_eq!(columns[0].data_type, DataType::Numeric);
    assert_eq!(rows.len(), 1);
    assert!(matches!(
        &rows[0].values[0],
        Value::Numeric(value) if value.to_string() == "1.5"
    ));
}

// ---------------------------------------------------------------
// Tables reflect DDL changes
// ---------------------------------------------------------------

#[test]
fn tables_reflect_newly_created_table() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    // Initially empty
    let results = engine
        .execute_sql(&session, "SELECT * FROM information_schema.tables")
        .expect("query");
    let rows = match &results[0] {
        StatementResult::Query { rows, .. } => rows,
        other => panic!("expected Query, got {other:?}"),
    };
    assert!(rows.is_empty());

    // Create a table
    engine
        .execute_sql(&session, "CREATE TABLE new_table (x INT NOT NULL)")
        .expect("create");

    // Now it should appear
    let results = engine
        .execute_sql(&session, "SELECT * FROM information_schema.tables")
        .expect("query");
    let rows = match &results[0] {
        StatementResult::Query { rows, .. } => rows,
        other => panic!("expected Query, got {other:?}"),
    };
    assert_eq!(rows.len(), 1);
    assert_eq!(text_col(&rows[0], 2), "new_table");
}

// ---------------------------------------------------------------
// Data type mapping
// ---------------------------------------------------------------

#[test]
fn columns_data_type_mapping() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE type_test (\
                 a INT NOT NULL, \
                 b BIGINT, \
                 c TEXT, \
                 d BOOLEAN, \
                 e TIMESTAMP, \
                 f DATE\
             )",
        )
        .expect("create");

    let results = engine
        .execute_sql(&session, "SELECT * FROM information_schema.columns")
        .expect("query");

    let rows = match &results[0] {
        StatementResult::Query { rows, .. } => rows,
        other => panic!("expected Query, got {other:?}"),
    };

    assert_eq!(rows.len(), 6);

    let type_map: Vec<(&str, &str)> = rows
        .iter()
        .map(|r| (text_col(r, 3), text_col(r, 7)))
        .collect();

    assert!(type_map.contains(&("a", "integer")));
    assert!(type_map.contains(&("b", "bigint")));
    assert!(type_map.contains(&("c", "text")));
    assert!(type_map.contains(&("d", "boolean")));
    assert!(type_map.contains(&("e", "timestamp without time zone")));
    assert!(type_map.contains(&("f", "date")));
}

#[test]
fn create_foreign_data_wrapper_is_explicitly_unsupported() {
    // AionDB has no FDW runtime; the prior misc-attrs path stored the
    // metadata but no real wrapper was wired. Reject the DDL with
    // `feature_not_supported` so tooling can detect missing support
    // instead of relying on a half-populated `information_schema`.
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let err = engine
        .execute_sql(
            &session,
            "CREATE FOREIGN DATA WRAPPER foo OPTIONS (\"test wrapper\" 'true')",
        )
        .expect_err("CREATE FOREIGN DATA WRAPPER must reject explicitly");
    assert_eq!(err.sqlstate(), aiondb_core::SqlState::FeatureNotSupported);
    assert!(err.report().message.contains("CREATE FOREIGN DATA WRAPPER"));
}
