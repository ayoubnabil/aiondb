use super::*;

/// Helper: create engine + session + table with one JSONB row.
fn setup_jsonb(json_literal: &str) -> (Engine, SessionHandle) {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    engine
        .execute_sql(&session, "CREATE TABLE jdoc (id INT NOT NULL, data JSONB)")
        .expect("create table");
    engine
        .execute_sql(
            &session,
            &format!("INSERT INTO jdoc VALUES (1, CAST('{json_literal}' AS JSONB))"),
        )
        .expect("insert");
    (engine, session)
}

/// Extract first row / first column value from a query result.
fn first_val(results: &[StatementResult]) -> &Value {
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert!(!rows.is_empty(), "expected at least one row");
            &rows[0].values[0]
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

// =====================================================================
// #> operator (path get, returns JSONB)
// =====================================================================

#[test]
fn jsonb_path_get_operator() {
    let (engine, s) = setup_jsonb(r#"{"a": {"b": {"c": 42}}}"#);
    let r = engine
        .execute_sql(&s, "SELECT data #> '{a,b}' FROM jdoc WHERE id = 1")
        .expect("path get");
    let expected: serde_json::Value = serde_json::from_str(r#"{"c": 42}"#).unwrap();
    assert_eq!(first_val(&r), &Value::Jsonb(expected));
}

#[test]
fn jsonb_path_get_scalar() {
    let (engine, s) = setup_jsonb(r#"{"a": {"b": 99}}"#);
    let r = engine
        .execute_sql(&s, "SELECT data #> '{a,b}' FROM jdoc WHERE id = 1")
        .expect("path get scalar");
    assert_eq!(
        first_val(&r),
        &Value::Jsonb(serde_json::Value::Number(99.into()))
    );
}

#[test]
fn jsonb_path_get_missing_returns_null() {
    let (engine, s) = setup_jsonb(r#"{"a": 1}"#);
    let r = engine
        .execute_sql(&s, "SELECT data #> '{x,y}' FROM jdoc WHERE id = 1")
        .expect("path get missing");
    assert_eq!(first_val(&r), &Value::Null);
}

// =====================================================================
// #>> operator (path get text, returns TEXT)
// =====================================================================

#[test]
fn jsonb_path_get_text_operator() {
    let (engine, s) = setup_jsonb(r#"{"a": {"b": "hello"}}"#);
    let r = engine
        .execute_sql(&s, "SELECT data #>> '{a,b}' FROM jdoc WHERE id = 1")
        .expect("path get text");
    assert_eq!(first_val(&r), &Value::Text("hello".to_owned()));
}

#[test]
fn jsonb_path_get_text_number() {
    let (engine, s) = setup_jsonb(r#"{"x": 3.14}"#);
    let r = engine
        .execute_sql(&s, "SELECT data #>> '{x}' FROM jdoc WHERE id = 1")
        .expect("path get text number");
    assert_eq!(first_val(&r), &Value::Text("3.14".to_owned()));
}

#[test]
fn jsonb_path_get_text_missing_returns_null() {
    let (engine, s) = setup_jsonb(r#"{"a": 1}"#);
    let r = engine
        .execute_sql(&s, "SELECT data #>> '{no,path}' FROM jdoc WHERE id = 1")
        .expect("path get text missing");
    assert_eq!(first_val(&r), &Value::Null);
}

// =====================================================================
// @> operator (contains)
// =====================================================================

#[test]
fn jsonb_contains_object() {
    let (engine, s) = setup_jsonb(r#"{"a": 1, "b": 2, "c": 3}"#);
    let r = engine
        .execute_sql(
            &s,
            r#"SELECT data @> CAST('{"a": 1}' AS JSONB) FROM jdoc WHERE id = 1"#,
        )
        .expect("contains true");
    assert_eq!(first_val(&r), &Value::Boolean(true));
}

#[test]
fn jsonb_contains_object_false() {
    let (engine, s) = setup_jsonb(r#"{"a": 1, "b": 2}"#);
    let r = engine
        .execute_sql(
            &s,
            r#"SELECT data @> CAST('{"a": 99}' AS JSONB) FROM jdoc WHERE id = 1"#,
        )
        .expect("contains false");
    assert_eq!(first_val(&r), &Value::Boolean(false));
}

#[test]
fn jsonb_contains_nested() {
    let (engine, s) = setup_jsonb(r#"{"a": {"b": 1, "c": 2}}"#);
    let r = engine
        .execute_sql(
            &s,
            r#"SELECT data @> CAST('{"a": {"b": 1}}' AS JSONB) FROM jdoc WHERE id = 1"#,
        )
        .expect("contains nested");
    assert_eq!(first_val(&r), &Value::Boolean(true));
}

#[test]
fn jsonb_contains_array() {
    let (engine, s) = setup_jsonb("[1, 2, 3, 4]");
    let r = engine
        .execute_sql(
            &s,
            "SELECT data @> CAST('[2, 4]' AS JSONB) FROM jdoc WHERE id = 1",
        )
        .expect("contains array");
    assert_eq!(first_val(&r), &Value::Boolean(true));
}

#[test]
fn jsonb_contains_array_false() {
    let (engine, s) = setup_jsonb("[1, 2, 3]");
    let r = engine
        .execute_sql(
            &s,
            "SELECT data @> CAST('[5]' AS JSONB) FROM jdoc WHERE id = 1",
        )
        .expect("contains array false");
    assert_eq!(first_val(&r), &Value::Boolean(false));
}

// =====================================================================
// <@ operator (contained by)
// =====================================================================

#[test]
fn jsonb_contained_by() {
    let (engine, s) = setup_jsonb(r#"{"a": 1}"#);
    let r = engine
        .execute_sql(
            &s,
            r#"SELECT data <@ CAST('{"a": 1, "b": 2}' AS JSONB) FROM jdoc WHERE id = 1"#,
        )
        .expect("contained by true");
    assert_eq!(first_val(&r), &Value::Boolean(true));
}

#[test]
fn jsonb_contained_by_false() {
    let (engine, s) = setup_jsonb(r#"{"a": 1, "x": 9}"#);
    let r = engine
        .execute_sql(
            &s,
            r#"SELECT data <@ CAST('{"a": 1, "b": 2}' AS JSONB) FROM jdoc WHERE id = 1"#,
        )
        .expect("contained by false");
    assert_eq!(first_val(&r), &Value::Boolean(false));
}

// =====================================================================
// ? operator (key exists)
// =====================================================================

#[test]
fn jsonb_key_exists_true() {
    let (engine, s) = setup_jsonb(r#"{"name": "alice", "age": 30}"#);
    let r = engine
        .execute_sql(&s, "SELECT data ? 'name' FROM jdoc WHERE id = 1")
        .expect("key exists true");
    assert_eq!(first_val(&r), &Value::Boolean(true));
}

#[test]
fn jsonb_key_exists_false() {
    let (engine, s) = setup_jsonb(r#"{"name": "alice"}"#);
    let r = engine
        .execute_sql(&s, "SELECT data ? 'email' FROM jdoc WHERE id = 1")
        .expect("key exists false");
    assert_eq!(first_val(&r), &Value::Boolean(false));
}

#[test]
fn jsonb_key_exists_array_element() {
    let (engine, s) = setup_jsonb(r#"["foo", "bar", "baz"]"#);
    let r = engine
        .execute_sql(&s, "SELECT data ? 'bar' FROM jdoc WHERE id = 1")
        .expect("key exists array element");
    assert_eq!(first_val(&r), &Value::Boolean(true));
}

// =====================================================================
// ?| operator (any key exists)
// =====================================================================

#[test]
fn jsonb_any_key_exists_true() {
    let (engine, s) = setup_jsonb(r#"{"a": 1, "b": 2}"#);
    let r = engine
        .execute_sql(&s, "SELECT data ?| ARRAY['b', 'c'] FROM jdoc WHERE id = 1")
        .expect("any key exists true");
    assert_eq!(first_val(&r), &Value::Boolean(true));
}

#[test]
fn jsonb_any_key_exists_false() {
    let (engine, s) = setup_jsonb(r#"{"a": 1, "b": 2}"#);
    let r = engine
        .execute_sql(&s, "SELECT data ?| ARRAY['x', 'y'] FROM jdoc WHERE id = 1")
        .expect("any key exists false");
    assert_eq!(first_val(&r), &Value::Boolean(false));
}

// =====================================================================
// ?& operator (all keys exist)
// =====================================================================

#[test]
fn jsonb_all_keys_exist_true() {
    let (engine, s) = setup_jsonb(r#"{"a": 1, "b": 2, "c": 3}"#);
    let r = engine
        .execute_sql(&s, "SELECT data ?& ARRAY['a', 'c'] FROM jdoc WHERE id = 1")
        .expect("all keys exist true");
    assert_eq!(first_val(&r), &Value::Boolean(true));
}

#[test]
fn jsonb_all_keys_exist_false() {
    let (engine, s) = setup_jsonb(r#"{"a": 1, "b": 2}"#);
    let r = engine
        .execute_sql(&s, "SELECT data ?& ARRAY['a', 'z'] FROM jdoc WHERE id = 1")
        .expect("all keys exist false");
    assert_eq!(first_val(&r), &Value::Boolean(false));
}

// =====================================================================
// jsonb_object_keys function
// =====================================================================

#[test]
fn jsonb_object_keys_function() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    let r = engine
        .execute_sql(
            &s,
            r#"SELECT jsonb_object_keys(CAST('{"b": 2, "a": 1, "c": 3}' AS JSONB))"#,
        )
        .expect("object keys");
    match first_val(&r) {
        Value::Array(keys) => {
            // Keys should be strings; order may vary (serde_json Map is BTreeMap).
            let mut strs: Vec<String> = keys
                .iter()
                .map(|v| match v {
                    Value::Text(s) => s.clone(),
                    _ => panic!("expected text key"),
                })
                .collect();
            strs.sort();
            assert_eq!(strs, vec!["a", "b", "c"]);
        }
        other => panic!("expected Array, got {other:?}"),
    }
}

// =====================================================================
// jsonb_pretty function
// =====================================================================

#[test]
fn jsonb_pretty_function() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    let r = engine
        .execute_sql(&s, r#"SELECT jsonb_pretty(CAST('{"a":1}' AS JSONB))"#)
        .expect("pretty");
    match first_val(&r) {
        Value::Text(t) => {
            assert!(t.contains('\n'), "pretty output should contain newlines");
            assert!(t.contains("\"a\""), "pretty output should contain key");
        }
        other => panic!("expected Text, got {other:?}"),
    }
}

// =====================================================================
// jsonb_extract_path function
// =====================================================================

#[test]
fn jsonb_extract_path_function() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    let r = engine
        .execute_sql(
            &s,
            r#"SELECT jsonb_extract_path(CAST('{"a":{"b":42}}' AS JSONB), 'a', 'b')"#,
        )
        .expect("extract path");
    assert_eq!(
        first_val(&r),
        &Value::Jsonb(serde_json::Value::Number(42.into()))
    );
}

#[test]
fn jsonb_extract_path_missing() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    let r = engine
        .execute_sql(
            &s,
            r#"SELECT jsonb_extract_path(CAST('{"a":1}' AS JSONB), 'x', 'y')"#,
        )
        .expect("extract path missing");
    assert_eq!(first_val(&r), &Value::Null);
}

// =====================================================================
// jsonb_extract_path_text function
// =====================================================================

#[test]
fn jsonb_extract_path_text_function() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    let r = engine
        .execute_sql(
            &s,
            r#"SELECT jsonb_extract_path_text(CAST('{"a":{"b":"hello"}}' AS JSONB), 'a', 'b')"#,
        )
        .expect("extract path text");
    assert_eq!(first_val(&r), &Value::Text("hello".to_owned()));
}

#[test]
fn jsonb_extract_path_text_number() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    let r = engine
        .execute_sql(
            &s,
            r#"SELECT jsonb_extract_path_text(CAST('{"x":123}' AS JSONB), 'x')"#,
        )
        .expect("extract path text number");
    assert_eq!(first_val(&r), &Value::Text("123".to_owned()));
}

// =====================================================================
// jsonb_set function
// =====================================================================

#[test]
fn jsonb_set_existing_key() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    let r = engine
        .execute_sql(
            &s,
            r#"SELECT jsonb_set(CAST('{"a":1,"b":2}' AS JSONB), '{b}', CAST('99' AS JSONB))"#,
        )
        .expect("set existing");
    let expected: serde_json::Value = serde_json::from_str(r#"{"a":1,"b":99}"#).unwrap();
    assert_eq!(first_val(&r), &Value::Jsonb(expected));
}

#[test]
fn jsonb_set_nested_path() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    let r = engine
        .execute_sql(
            &s,
            r#"SELECT jsonb_set(CAST('{"a":{"b":1}}' AS JSONB), '{a,b}', CAST('"new"' AS JSONB))"#,
        )
        .expect("set nested");
    let expected: serde_json::Value = serde_json::from_str(r#"{"a":{"b":"new"}}"#).unwrap();
    assert_eq!(first_val(&r), &Value::Jsonb(expected));
}

#[test]
fn jsonb_set_create_missing_true() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    // Default create_missing = true
    let r = engine
        .execute_sql(
            &s,
            r#"SELECT jsonb_set(CAST('{"a":1}' AS JSONB), '{b}', CAST('2' AS JSONB))"#,
        )
        .expect("set create missing true");
    let expected: serde_json::Value = serde_json::from_str(r#"{"a":1,"b":2}"#).unwrap();
    assert_eq!(first_val(&r), &Value::Jsonb(expected));
}

#[test]
fn jsonb_set_create_missing_false() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    let r = engine
        .execute_sql(
            &s,
            r#"SELECT jsonb_set(CAST('{"a":1}' AS JSONB), '{b}', CAST('2' AS JSONB), FALSE)"#,
        )
        .expect("set create missing false");
    // b does not exist and create_missing is false, so unchanged
    let expected: serde_json::Value = serde_json::from_str(r#"{"a":1}"#).unwrap();
    assert_eq!(first_val(&r), &Value::Jsonb(expected));
}

// =====================================================================
// WHERE clause filtering with JSONB operators
// =====================================================================

#[test]
fn jsonb_contains_in_where_clause() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    engine
        .execute_sql(&s, "CREATE TABLE docs (id INT NOT NULL, data JSONB)")
        .expect("create");
    engine
        .execute_sql(
            &s,
            r#"INSERT INTO docs VALUES (1, CAST('{"type":"a","val":1}' AS JSONB))"#,
        )
        .expect("ins 1");
    engine
        .execute_sql(
            &s,
            r#"INSERT INTO docs VALUES (2, CAST('{"type":"b","val":2}' AS JSONB))"#,
        )
        .expect("ins 2");
    engine
        .execute_sql(
            &s,
            r#"INSERT INTO docs VALUES (3, CAST('{"type":"a","val":3}' AS JSONB))"#,
        )
        .expect("ins 3");

    let r = engine
        .execute_sql(
            &s,
            r#"SELECT id FROM docs WHERE data @> CAST('{"type":"a"}' AS JSONB) ORDER BY id"#,
        )
        .expect("filter by contains");
    match &r[0] {
        StatementResult::Query { rows, .. } => {
            let ids: Vec<&Value> = rows.iter().map(|r| &r.values[0]).collect();
            assert_eq!(ids, vec![&Value::Int(1), &Value::Int(3)]);
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn jsonb_key_exists_in_where_clause() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    engine
        .execute_sql(&s, "CREATE TABLE docs (id INT NOT NULL, data JSONB)")
        .expect("create");
    engine
        .execute_sql(
            &s,
            r#"INSERT INTO docs VALUES (1, CAST('{"email":"a@b.com"}' AS JSONB))"#,
        )
        .expect("ins 1");
    engine
        .execute_sql(
            &s,
            r#"INSERT INTO docs VALUES (2, CAST('{"name":"bob"}' AS JSONB))"#,
        )
        .expect("ins 2");

    let r = engine
        .execute_sql(&s, "SELECT id FROM docs WHERE data ? 'email'")
        .expect("filter by key exists");
    match &r[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::Int(1));
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

// =====================================================================
// NULL propagation for new operators
// =====================================================================

#[test]
fn jsonb_operators_null_propagation() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (s, _) = engine.startup(startup_params()).expect("startup");
    engine
        .execute_sql(&s, "CREATE TABLE jnull (id INT NOT NULL, data JSONB)")
        .expect("create");
    engine
        .execute_sql(&s, "INSERT INTO jnull VALUES (1, NULL)")
        .expect("insert null");

    // #> on NULL -> NULL
    let r = engine
        .execute_sql(&s, "SELECT data #> '{a}' FROM jnull WHERE id = 1")
        .expect("#> null");
    assert_eq!(first_val(&r), &Value::Null);

    // #>> on NULL -> NULL
    let r = engine
        .execute_sql(&s, "SELECT data #>> '{a}' FROM jnull WHERE id = 1")
        .expect("#>> null");
    assert_eq!(first_val(&r), &Value::Null);
}
