use super::*;

// ===================================================================
// INSERT and SELECT with JSONB columns
// ===================================================================

#[test]
fn jsonb_insert_and_select() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE jdoc (id INT NOT NULL, data JSONB)")
        .expect("create table");

    engine
        .execute_sql(
            &session,
            r#"INSERT INTO jdoc VALUES (1, CAST('{"name": "alice", "age": 30}' AS JSONB))"#,
        )
        .expect("insert");

    let results = engine
        .execute_sql(&session, "SELECT data FROM jdoc WHERE id = 1")
        .expect("select");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            let expected: serde_json::Value =
                serde_json::from_str(r#"{"name": "alice", "age": 30}"#).unwrap();
            assert_eq!(rows[0].values[0], Value::Jsonb(expected));
        }
        other => panic!("expected query, got {other:?}"),
    }
}

// ===================================================================
// -> operator (returns JSONB)
// ===================================================================

#[test]
fn jsonb_get_object_key() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE jdoc (id INT NOT NULL, data JSONB)")
        .expect("create table");

    engine
        .execute_sql(
            &session,
            r#"INSERT INTO jdoc VALUES (1, CAST('{"name": "alice", "age": 30}' AS JSONB))"#,
        )
        .expect("insert");

    let results = engine
        .execute_sql(&session, "SELECT data->'name' FROM jdoc WHERE id = 1")
        .expect("select");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(
                rows[0].values[0],
                Value::Jsonb(serde_json::Value::String("alice".to_owned()))
            );
        }
        other => panic!("expected query, got {other:?}"),
    }
}

// ===================================================================
// ->> operator (returns TEXT)
// ===================================================================

#[test]
fn jsonb_get_text_object_key() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE jdoc (id INT NOT NULL, data JSONB)")
        .expect("create table");

    engine
        .execute_sql(
            &session,
            r#"INSERT INTO jdoc VALUES (1, CAST('{"name": "alice", "age": 30}' AS JSONB))"#,
        )
        .expect("insert");

    let results = engine
        .execute_sql(&session, "SELECT data->>'name' FROM jdoc WHERE id = 1")
        .expect("select");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            // ->> returns TEXT, not JSONB
            assert_eq!(rows[0].values[0], Value::Text("alice".to_owned()));
        }
        other => panic!("expected query, got {other:?}"),
    }
}

// ===================================================================
// -> with integer index (array access)
// ===================================================================

#[test]
fn jsonb_get_array_index() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE jdoc (id INT NOT NULL, data JSONB)")
        .expect("create table");

    engine
        .execute_sql(
            &session,
            "INSERT INTO jdoc VALUES (1, CAST('[10, 20, 30]' AS JSONB))",
        )
        .expect("insert");

    let results = engine
        .execute_sql(&session, "SELECT data->0 FROM jdoc WHERE id = 1")
        .expect("select");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(
                rows[0].values[0],
                Value::Jsonb(serde_json::Value::Number(10.into()))
            );
        }
        other => panic!("expected query, got {other:?}"),
    }
}

// ===================================================================
// ->> with integer index (array access returning TEXT)
// ===================================================================

#[test]
fn jsonb_get_text_array_index() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE jdoc (id INT NOT NULL, data JSONB)")
        .expect("create table");

    engine
        .execute_sql(
            &session,
            r#"INSERT INTO jdoc VALUES (1, CAST('["hello", "world"]' AS JSONB))"#,
        )
        .expect("insert");

    let results = engine
        .execute_sql(&session, "SELECT data->>0 FROM jdoc WHERE id = 1")
        .expect("select");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            // ->> returns TEXT (without JSON quotes)
            assert_eq!(rows[0].values[0], Value::Text("hello".to_owned()));
        }
        other => panic!("expected query, got {other:?}"),
    }
}

// ===================================================================
// Missing key returns NULL
// ===================================================================

#[test]
fn jsonb_get_missing_key_returns_null() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE jdoc (id INT NOT NULL, data JSONB)")
        .expect("create table");

    engine
        .execute_sql(
            &session,
            r#"INSERT INTO jdoc VALUES (1, CAST('{"name": "alice"}' AS JSONB))"#,
        )
        .expect("insert");

    let results = engine
        .execute_sql(
            &session,
            "SELECT data->'nonexistent' FROM jdoc WHERE id = 1",
        )
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
// Chained / nested access: data->'a'->'b'
// ===================================================================

#[test]
fn jsonb_get_nested() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE jdoc (id INT NOT NULL, data JSONB)")
        .expect("create table");

    engine
        .execute_sql(
            &session,
            r#"INSERT INTO jdoc VALUES (1, CAST('{"a": {"b": {"c": 42}}}' AS JSONB))"#,
        )
        .expect("insert");

    let results = engine
        .execute_sql(&session, "SELECT data->'a'->'b' FROM jdoc WHERE id = 1")
        .expect("select");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            let expected: serde_json::Value = serde_json::from_str(r#"{"c": 42}"#).unwrap();
            assert_eq!(rows[0].values[0], Value::Jsonb(expected));
        }
        other => panic!("expected query, got {other:?}"),
    }
}

// ===================================================================
// jsonb_typeof function
// ===================================================================

#[test]
fn jsonb_typeof_function() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    // Test with object
    let results = engine
        .execute_sql(&session, r#"SELECT jsonb_typeof(CAST('{"a":1}' AS JSONB))"#)
        .expect("typeof object");
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows[0].values[0], Value::Text("object".to_owned()));
        }
        other => panic!("expected query, got {other:?}"),
    }

    // Test with array
    let results = engine
        .execute_sql(&session, "SELECT jsonb_typeof(CAST('[1,2]' AS JSONB))")
        .expect("typeof array");
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows[0].values[0], Value::Text("array".to_owned()));
        }
        other => panic!("expected query, got {other:?}"),
    }

    // Test with string
    let results = engine
        .execute_sql(&session, r#"SELECT jsonb_typeof(CAST('"hello"' AS JSONB))"#)
        .expect("typeof string");
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows[0].values[0], Value::Text("string".to_owned()));
        }
        other => panic!("expected query, got {other:?}"),
    }

    // Test with number
    let results = engine
        .execute_sql(&session, "SELECT jsonb_typeof(CAST('42' AS JSONB))")
        .expect("typeof number");
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows[0].values[0], Value::Text("number".to_owned()));
        }
        other => panic!("expected query, got {other:?}"),
    }

    // Test with boolean
    let results = engine
        .execute_sql(&session, "SELECT jsonb_typeof(CAST('true' AS JSONB))")
        .expect("typeof boolean");
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows[0].values[0], Value::Text("boolean".to_owned()));
        }
        other => panic!("expected query, got {other:?}"),
    }

    // Test with null
    let results = engine
        .execute_sql(&session, "SELECT jsonb_typeof(CAST('null' AS JSONB))")
        .expect("typeof null");
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows[0].values[0], Value::Text("null".to_owned()));
        }
        other => panic!("expected query, got {other:?}"),
    }
}

// ===================================================================
// jsonb_array_length function
// ===================================================================

#[test]
fn jsonb_array_length_function() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT jsonb_array_length(CAST('[1,2,3]' AS JSONB))",
        )
        .expect("array length");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::Int(3));
        }
        other => panic!("expected query, got {other:?}"),
    }
}

// ===================================================================
// jsonb_build_object function
// ===================================================================

#[test]
fn jsonb_build_object_function() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(&session, "SELECT jsonb_build_object('a', 1, 'b', 2)")
        .expect("build object");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            let expected: serde_json::Value = serde_json::from_str(r#"{"a": 1, "b": 2}"#).unwrap();
            assert_eq!(rows[0].values[0], Value::Jsonb(expected));
        }
        other => panic!("expected query, got {other:?}"),
    }
}

// ===================================================================
// NULL input handling
// ===================================================================

#[test]
fn jsonb_null_input() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE jdoc (id INT NOT NULL, data JSONB)")
        .expect("create table");

    engine
        .execute_sql(&session, "INSERT INTO jdoc VALUES (1, NULL)")
        .expect("insert null");

    // -> on NULL JSONB returns NULL
    let results = engine
        .execute_sql(&session, "SELECT data->'key' FROM jdoc WHERE id = 1")
        .expect("select -> on null");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::Null);
        }
        other => panic!("expected query, got {other:?}"),
    }

    // ->> on NULL JSONB returns NULL
    let results = engine
        .execute_sql(&session, "SELECT data->>'key' FROM jdoc WHERE id = 1")
        .expect("select ->> on null");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::Null);
        }
        other => panic!("expected query, got {other:?}"),
    }
}

// ===================================================================
// Insert and query nested JSON objects
// ===================================================================

#[test]
fn jsonb_nested_object_insert_and_query() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE jnested (id INT NOT NULL, data JSONB)",
        )
        .expect("create table");

    engine
        .execute_sql(
            &session,
            r#"INSERT INTO jnested VALUES (1, CAST('{"user":{"name":"alice","address":{"city":"paris"}}}' AS JSONB))"#,
        )
        .expect("insert");

    // Drill into nested path
    let results = engine
        .execute_sql(
            &session,
            "SELECT data->'user'->'address'->'city' FROM jnested WHERE id = 1",
        )
        .expect("select nested");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(
                rows[0].values[0],
                Value::Jsonb(serde_json::Value::String("paris".to_owned()))
            );
        }
        other => panic!("expected query, got {other:?}"),
    }
}

// ===================================================================
// NULL values in JSONB (SQL NULL vs JSON null)
// ===================================================================

#[test]
fn jsonb_sql_null_vs_json_null() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE jnulls (id INT NOT NULL, data JSONB)",
        )
        .expect("create table");

    // SQL NULL
    engine
        .execute_sql(&session, "INSERT INTO jnulls VALUES (1, NULL)")
        .expect("insert sql null");

    // JSON null value
    engine
        .execute_sql(
            &session,
            "INSERT INTO jnulls VALUES (2, CAST('null' AS JSONB))",
        )
        .expect("insert json null");

    let results = engine
        .execute_sql(&session, "SELECT id, data FROM jnulls ORDER BY id")
        .expect("select");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 2);
            // id=1: SQL NULL
            assert_eq!(rows[0].values[1], Value::Null);
            // id=2: JSON null
            assert_eq!(rows[1].values[1], Value::Jsonb(serde_json::Value::Null));
        }
        other => panic!("expected query, got {other:?}"),
    }
}

// ===================================================================
// JSONB comparison (equality of objects)
// ===================================================================

#[test]
fn jsonb_equality_comparison() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE jcmp (id INT NOT NULL, data JSONB)")
        .expect("create table");

    engine
        .execute_sql(
            &session,
            r#"INSERT INTO jcmp VALUES (1, CAST('{"a":1,"b":2}' AS JSONB)), (2, CAST('{"a":1,"b":2}' AS JSONB)), (3, CAST('{"a":1,"b":3}' AS JSONB))"#,
        )
        .expect("insert");

    let results = engine
        .execute_sql(
            &session,
            r#"SELECT id FROM jcmp WHERE data = CAST('{"a":1,"b":2}' AS JSONB) ORDER BY id"#,
        )
        .expect("select");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 2);
            assert_eq!(rows[0].values[0], Value::Int(1));
            assert_eq!(rows[1].values[0], Value::Int(2));
        }
        other => panic!("expected query, got {other:?}"),
    }
}

// ===================================================================
// jsonb_typeof on different types from table
// ===================================================================

#[test]
fn jsonb_typeof_from_table() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE jtypes (id INT NOT NULL, data JSONB)",
        )
        .expect("create table");

    engine
        .execute_sql(
            &session,
            r#"INSERT INTO jtypes VALUES (1, CAST('{"a":1}' AS JSONB)), (2, CAST('[1,2]' AS JSONB)), (3, CAST('42' AS JSONB))"#,
        )
        .expect("insert");

    let results = engine
        .execute_sql(
            &session,
            "SELECT id, jsonb_typeof(data) FROM jtypes ORDER BY id",
        )
        .expect("select");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 3);
            assert_eq!(rows[0].values[1], Value::Text("object".to_owned()));
            assert_eq!(rows[1].values[1], Value::Text("array".to_owned()));
            assert_eq!(rows[2].values[1], Value::Text("number".to_owned()));
        }
        other => panic!("expected query, got {other:?}"),
    }
}

// ===================================================================
// jsonb_array_length on nested arrays
// ===================================================================

#[test]
fn jsonb_array_length_nested() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    // Array of arrays - length is count of top-level elements
    let results = engine
        .execute_sql(
            &session,
            "SELECT jsonb_array_length(CAST('[[1,2],[3,4],[5]]' AS JSONB))",
        )
        .expect("array length nested");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::Int(3));
        }
        other => panic!("expected query, got {other:?}"),
    }
}

// ===================================================================
// jsonb_strip_nulls removes null values
// ===================================================================

#[test]
fn jsonb_strip_nulls_function() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            r#"SELECT jsonb_strip_nulls(CAST('{"a":1,"b":null,"c":3}' AS JSONB))"#,
        )
        .expect("strip nulls");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            let expected: serde_json::Value = serde_json::from_str(r#"{"a":1,"c":3}"#).unwrap();
            assert_eq!(rows[0].values[0], Value::Jsonb(expected));
        }
        other => panic!("expected query, got {other:?}"),
    }
}

#[test]
fn jsonb_each_select_and_from_support_composite_fields() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let select_results = engine
        .execute_sql(
            &session,
            r#"SELECT (jsonb_each(CAST('{"a,b":"x,y"}' AS JSONB))).key,
                      (jsonb_each(CAST('{"a,b":"x,y"}' AS JSONB))).value"#,
        )
        .expect("SELECT jsonb_each field access");
    match &select_results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::Text("a,b".to_owned()));
            assert_eq!(rows[0].values[1], Value::Jsonb(serde_json::json!("x,y")));
        }
        other => panic!("expected query, got {other:?}"),
    }

    let from_results = engine
        .execute_sql(
            &session,
            r#"SELECT k, v
               FROM jsonb_each(CAST('{"a,b":"x,y","z":1}' AS JSONB)) AS t(k, v)
               ORDER BY k"#,
        )
        .expect("FROM jsonb_each");
    match &from_results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 2);
            assert_eq!(rows[0].values[0], Value::Text("a,b".to_owned()));
            assert_eq!(rows[0].values[1], Value::Jsonb(serde_json::json!("x,y")));
            assert_eq!(rows[1].values[0], Value::Text("z".to_owned()));
            assert_eq!(rows[1].values[1], Value::Jsonb(serde_json::json!(1)));
        }
        other => panic!("expected query, got {other:?}"),
    }
}

#[test]
fn jsonb_each_text_from_returns_plain_text_and_null() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            r#"SELECT k, v
               FROM jsonb_each_text(CAST('{"n":null,"s":"x,y"}' AS JSONB)) AS t(k, v)
               ORDER BY k"#,
        )
        .expect("FROM jsonb_each_text");
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 2);
            assert_eq!(rows[0].values[0], Value::Text("n".to_owned()));
            assert_eq!(rows[0].values[1], Value::Null);
            assert_eq!(rows[1].values[0], Value::Text("s".to_owned()));
            assert_eq!(rows[1].values[1], Value::Text("x,y".to_owned()));
        }
        other => panic!("expected query, got {other:?}"),
    }
}

#[test]
fn jsonb_array_elements_from_returns_jsonb_rows() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            r#"SELECT value
               FROM jsonb_array_elements(CAST('[1, {"k":"v"}, null]' AS JSONB)) AS t(value)"#,
        )
        .expect("FROM jsonb_array_elements");
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 3);
            assert_eq!(rows[0].values[0], Value::Jsonb(serde_json::json!(1)));
            assert_eq!(
                rows[1].values[0],
                Value::Jsonb(serde_json::json!({"k":"v"}))
            );
            assert_eq!(rows[2].values[0], Value::Jsonb(serde_json::Value::Null));
        }
        other => panic!("expected query, got {other:?}"),
    }
}

#[test]
fn jsonb_array_elements_text_from_returns_text_rows() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            r#"SELECT value
               FROM jsonb_array_elements_text(CAST('[1, "a,b", null, {"k":1}]' AS JSONB)) AS t(value)"#,
        )
        .expect("FROM jsonb_array_elements_text");
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 4);
            assert_eq!(rows[0].values[0], Value::Text("1".to_owned()));
            assert_eq!(rows[1].values[0], Value::Text("a,b".to_owned()));
            assert_eq!(rows[2].values[0], Value::Null);
            assert_eq!(rows[3].values[0], Value::Text("{\"k\": 1}".to_owned()));
        }
        other => panic!("expected query, got {other:?}"),
    }
}

// ===================================================================
// JSONB in WHERE clause with -> operator
// ===================================================================

#[test]
fn jsonb_arrow_in_where_clause() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE jfilter (id INT NOT NULL, data JSONB)",
        )
        .expect("create table");

    engine
        .execute_sql(
            &session,
            r#"INSERT INTO jfilter VALUES (1, CAST('{"status":"active","score":10}' AS JSONB)), (2, CAST('{"status":"inactive","score":5}' AS JSONB)), (3, CAST('{"status":"active","score":20}' AS JSONB))"#,
        )
        .expect("insert");

    let results = engine
        .execute_sql(
            &session,
            "SELECT id FROM jfilter WHERE data->>'status' = 'active' ORDER BY id",
        )
        .expect("select with -> in WHERE");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 2);
            assert_eq!(rows[0].values[0], Value::Int(1));
            assert_eq!(rows[1].values[0], Value::Int(3));
        }
        other => panic!("expected query, got {other:?}"),
    }
}

// ===================================================================
// jsonb_build_object with mixed types
// ===================================================================

#[test]
fn jsonb_build_object_mixed_types() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT jsonb_build_object('name', 'alice', 'age', 30, 'active', TRUE)",
        )
        .expect("build object mixed");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            let val = &rows[0].values[0];
            match val {
                Value::Jsonb(obj) => {
                    assert_eq!(obj["name"], serde_json::Value::String("alice".to_owned()));
                    assert_eq!(obj["age"], serde_json::json!(30));
                    assert_eq!(obj["active"], serde_json::Value::Bool(true));
                }
                other => panic!("expected Jsonb, got {other:?}"),
            }
        }
        other => panic!("expected query, got {other:?}"),
    }
}

#[test]
fn jsonb_object_agg_uses_jsonb_build_array_state() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(&session, "SELECT jsonb_object_agg(1, NULL::jsonb)")
        .expect("jsonb_object_agg");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(
                rows[0].values[0],
                Value::Jsonb(serde_json::json!({ "1": null }))
            );
        }
        other => panic!("expected query, got {other:?}"),
    }
}

#[test]
fn jsonb_agg_whole_row_produces_objects() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT jsonb_agg(r) FROM (VALUES (1, 'txt1'), (2, 'txt2')) AS r(x, y)",
        )
        .expect("jsonb_agg whole row");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(
                rows[0].values[0],
                Value::Jsonb(serde_json::json!([
                    {"x": 1, "y": "txt1"},
                    {"x": 2, "y": "txt2"}
                ]))
            );
        }
        other => panic!("expected query, got {other:?}"),
    }
}

#[test]
fn jsonb_agg_order_by_whole_row_respects_order() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT jsonb_agg(r ORDER BY x, y) FROM (VALUES (2, 'txt2'), (1, 'txt1')) AS r(x, y)",
        )
        .expect("jsonb_agg whole row ordered");

    match &results[0] {
        StatementResult::Query { rows, columns, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(columns[0].name, "jsonb_agg");
            assert_eq!(
                rows[0].values[0],
                Value::Jsonb(serde_json::json!([
                    {"x": 1, "y": "txt1"},
                    {"x": 2, "y": "txt2"}
                ]))
            );
        }
        other => panic!("expected query, got {other:?}"),
    }
}

#[test]
fn jsonb_agg_whole_row_nested_row_array_stays_structured() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let sql = "SELECT jsonb_agg(q) \
               FROM ( \
                 SELECT \
                   ('a' || x::text) AS b, \
                   y AS c, \
                   ARRAY[ROW(x, ARRAY[1,2,3]), ROW(y, ARRAY[4,5,6])] AS z \
                 FROM generate_series(1,2) x, generate_series(4,5) y \
               ) q";
    let results = engine
        .execute_sql(&session, sql)
        .expect("jsonb_agg nested row");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            let Value::Jsonb(serde_json::Value::Array(items)) = &rows[0].values[0] else {
                panic!("expected jsonb array result");
            };
            assert_eq!(items.len(), 4);
            for item in items {
                let obj = item.as_object().expect("row object");
                let z = obj.get("z").expect("z field");
                let z_items = z.as_array().expect("z array");
                assert_eq!(z_items.len(), 2);
                for pair in z_items {
                    let pair_obj = pair
                        .as_object()
                        .unwrap_or_else(|| panic!("pair object, got {pair:?}"));
                    assert!(
                        pair_obj.contains_key("f1"),
                        "expected key f1 in nested row object"
                    );
                    assert!(
                        pair_obj.contains_key("f2"),
                        "expected key f2 in nested row object"
                    );
                }
            }
        }
        other => panic!("expected query, got {other:?}"),
    }
}

#[test]
fn jsonb_agg_order_by_nulls_first_respects_null_position() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let sql = "WITH rows(x, y) AS (VALUES (2, 'txt2'), (NULL::int, 'txt0'), (1, 'txt1')) \
               SELECT jsonb_agg(r ORDER BY x NULLS FIRST, y) FROM rows r";
    let results = engine
        .execute_sql(&session, sql)
        .expect("jsonb_agg order by nulls first");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(
                rows[0].values[0],
                Value::Jsonb(serde_json::json!([
                    {"x": null, "y": "txt0"},
                    {"x": 1, "y": "txt1"},
                    {"x": 2, "y": "txt2"}
                ]))
            );
        }
        other => panic!("expected query, got {other:?}"),
    }
}
