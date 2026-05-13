#![allow(clippy::pedantic)]

use super::*;
use serde_json::json;
use time::{Date, Month};

mod compat_execute;
mod current_of;
mod cursor_lifecycle;
mod prepared_vectors;

fn query_rows_as_text(results: Vec<StatementResult>) -> Vec<Vec<String>> {
    let StatementResult::Query { rows, .. } = &results[0] else {
        panic!("expected query result, got {:?}", results[0]);
    };
    rows.iter()
        .map(|row| row.values.iter().map(ToString::to_string).collect())
        .collect()
}

#[test]
fn executes_prepared_select_portal() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .prepare(
            &session,
            "sel".to_owned(),
            "SELECT 1 AS one, TRUE".to_owned(),
        )
        .expect("prepare");
    engine
        .bind(&session, "p1".to_owned(), "sel".to_owned(), Vec::new())
        .expect("bind");

    let batch = engine
        .execute_portal(&session, "p1", 0)
        .expect("execute portal");
    assert_eq!(
        batch,
        PortalBatch {
            columns: vec![
                ResultColumn {
                    name: "one".to_owned(),
                    data_type: aiondb_core::DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                },
                ResultColumn {
                    name: "?column?".to_owned(),
                    data_type: aiondb_core::DataType::Boolean,
                    text_type_modifier: None,
                    nullable: false,
                },
            ],
            rows: vec![aiondb_core::Row::new(vec![
                aiondb_core::Value::Int(1),
                aiondb_core::Value::Boolean(true),
            ])],
            tag: "SELECT 1".to_owned(),
            rows_affected: 0,
            exhausted: true,
        }
    );
}

#[test]
fn prepares_and_executes_parameterized_select_portal() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE users (id INT, name TEXT); \
             INSERT INTO users VALUES (1, 'alice'), (2, 'bob')",
        )
        .expect("seed");

    let desc = engine
        .prepare(
            &session,
            "sel_param".to_owned(),
            "SELECT id, name FROM users WHERE id = $1".to_owned(),
        )
        .expect("prepare");
    assert_eq!(desc.param_types, vec![aiondb_core::DataType::Int]);

    engine
        .bind(
            &session,
            "p1".to_owned(),
            "sel_param".to_owned(),
            vec![aiondb_core::Value::Int(2)],
        )
        .expect("bind");

    let batch = engine
        .execute_portal(&session, "p1", 0)
        .expect("execute portal");
    assert_eq!(
        batch,
        PortalBatch {
            columns: vec![
                ResultColumn {
                    name: "id".to_owned(),
                    data_type: aiondb_core::DataType::Int,
                    text_type_modifier: None,
                    nullable: true,
                },
                ResultColumn {
                    name: "name".to_owned(),
                    data_type: aiondb_core::DataType::Text,
                    text_type_modifier: None,
                    nullable: true,
                },
            ],
            rows: vec![aiondb_core::Row::new(vec![
                aiondb_core::Value::Int(2),
                aiondb_core::Value::Text("bob".to_owned()),
            ])],
            tag: "SELECT 1".to_owned(),
            rows_affected: 0,
            exhausted: true,
        }
    );
}

#[test]
fn prepared_batch_case_update_after_reinsert_updates_all_rows() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE record (id INT PRIMARY KEY, n INTEGER NOT NULL, s TEXT NOT NULL); \
             INSERT INTO record VALUES (1, 10, 'a'), (2, 20, 'b'); \
             DELETE FROM record WHERE id IN (1, 2); \
             INSERT INTO record VALUES (1, 10, 'a'), (2, 20, 'b')",
        )
        .expect("seed");

    engine
        .prepare(
            &session,
            "batch_upd".to_owned(),
            "UPDATE record SET \
                 n = CASE record.id \
                     WHEN $1::INTEGER THEN $3::INTEGER \
                     WHEN $2::INTEGER THEN $4::INTEGER \
                     ELSE n \
                 END, \
                 s = CASE record.id \
                     WHEN $1::INTEGER THEN $5::TEXT \
                     WHEN $2::INTEGER THEN $6::TEXT \
                     ELSE s \
                 END \
             WHERE record.id IN ($1::INTEGER, $2::INTEGER)"
                .to_owned(),
        )
        .expect("prepare");
    let (batch, _) = engine
        .execute_prepared_statement_with_notices(
            &session,
            "batch_upd".to_owned(),
            vec![
                aiondb_core::Value::Int(1),
                aiondb_core::Value::Int(2),
                aiondb_core::Value::Int(11),
                aiondb_core::Value::Int(22),
                aiondb_core::Value::Text("aa".to_owned()),
                aiondb_core::Value::Text("bb".to_owned()),
            ],
            0,
        )
        .expect("execute prepared");
    assert_eq!(batch.tag, "UPDATE");
    assert_eq!(batch.rows_affected, 2);

    assert_eq!(
        query_rows_as_text(
            engine
                .execute_sql(&session, "SELECT id, n, s FROM record ORDER BY id")
                .expect("select")
        ),
        vec![
            vec!["1".to_owned(), "11".to_owned(), "aa".to_owned()],
            vec!["2".to_owned(), "22".to_owned(), "bb".to_owned()],
        ]
    );
}

#[test]
fn parameterized_portal_rewrites_cached_plan_literals_between_binds() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE users (id INT, name TEXT); \
             CREATE INDEX users_id_idx ON users (id); \
             INSERT INTO users VALUES (1, 'alice'), (2, 'bob')",
        )
        .expect("seed");

    engine
        .prepare(
            &session,
            "sel_param_cached".to_owned(),
            "SELECT id, name FROM users WHERE id = $1".to_owned(),
        )
        .expect("prepare");

    engine
        .bind(
            &session,
            "p_first".to_owned(),
            "sel_param_cached".to_owned(),
            vec![aiondb_core::Value::Int(1)],
        )
        .expect("bind first");
    let first = engine
        .execute_portal(&session, "p_first", 0)
        .expect("execute first");
    assert_eq!(
        first.rows,
        vec![aiondb_core::Row::new(vec![
            aiondb_core::Value::Int(1),
            aiondb_core::Value::Text("alice".to_owned()),
        ])]
    );

    engine
        .bind(
            &session,
            "p_second".to_owned(),
            "sel_param_cached".to_owned(),
            vec![aiondb_core::Value::Int(2)],
        )
        .expect("bind second");
    let second = engine
        .execute_portal(&session, "p_second", 0)
        .expect("execute second");
    assert_eq!(engine.plan_cache_hits(), 1);
    assert_eq!(
        second.rows,
        vec![aiondb_core::Row::new(vec![
            aiondb_core::Value::Int(2),
            aiondb_core::Value::Text("bob".to_owned()),
        ])]
    );
}

#[test]
fn parameterized_update_portal_rewrites_cached_plan_literals_between_binds() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE accounts (id INT PRIMARY KEY, bal INT); \
             CREATE INDEX accounts_id_idx ON accounts (id); \
             INSERT INTO accounts VALUES (1, 10), (2, 20)",
        )
        .expect("seed");

    engine
        .prepare(
            &session,
            "upd_param_cached".to_owned(),
            "UPDATE accounts SET bal = bal + $1 WHERE id = $2".to_owned(),
        )
        .expect("prepare");

    assert_eq!(engine.plan_cache_hits(), 0);

    engine
        .bind(
            &session,
            "p_first".to_owned(),
            "upd_param_cached".to_owned(),
            vec![aiondb_core::Value::Int(5), aiondb_core::Value::Int(1)],
        )
        .expect("bind first");
    let first = engine
        .execute_portal(&session, "p_first", 0)
        .expect("execute first");
    assert!(
        first.tag.starts_with("UPDATE"),
        "unexpected tag: {}",
        first.tag
    );
    assert_eq!(engine.plan_cache_hits(), 0);

    engine
        .bind(
            &session,
            "p_second".to_owned(),
            "upd_param_cached".to_owned(),
            vec![aiondb_core::Value::Int(7), aiondb_core::Value::Int(2)],
        )
        .expect("bind second");
    let second = engine
        .execute_portal(&session, "p_second", 0)
        .expect("execute second");
    assert!(
        second.tag.starts_with("UPDATE"),
        "unexpected tag: {}",
        second.tag
    );
    assert_eq!(engine.plan_cache_hits(), 1);

    let rows = engine
        .execute_sql(&session, "SELECT id, bal FROM accounts ORDER BY id")
        .expect("verify");
    assert_eq!(
        query_rows_as_text(rows),
        vec![
            vec!["1".to_owned(), "15".to_owned()],
            vec!["2".to_owned(), "27".to_owned()],
        ]
    );
}

#[test]
fn prepared_update_inside_explicit_transaction_enrolls_storage_participant() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE accounts (id INT PRIMARY KEY, bal INT); \
             INSERT INTO accounts VALUES (1, 10)",
        )
        .expect("seed");

    engine
        .prepare(
            &session,
            "upd_txn".to_owned(),
            "UPDATE accounts SET bal = bal + $1 WHERE id = $2".to_owned(),
        )
        .expect("prepare");

    engine.execute_sql(&session, "BEGIN").expect("begin");
    engine
        .bind(
            &session,
            "p_txn".to_owned(),
            "upd_txn".to_owned(),
            vec![aiondb_core::Value::Int(5), aiondb_core::Value::Int(1)],
        )
        .expect("bind");
    let batch = engine
        .execute_portal(&session, "p_txn", 0)
        .expect("execute in explicit txn");
    assert!(
        batch.tag.starts_with("UPDATE"),
        "unexpected tag: {}",
        batch.tag
    );
    engine.execute_sql(&session, "COMMIT").expect("commit");

    let rows = engine
        .execute_sql(&session, "SELECT bal FROM accounts WHERE id = 1")
        .expect("verify");
    assert_eq!(query_rows_as_text(rows), vec![vec!["15".to_owned()]]);
}

#[test]
fn parameterized_insert_values_portal_rewrites_cached_plan_literals_between_binds() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE history (tid INT, bid INT, aid INT, delta INT, note TEXT)",
        )
        .expect("seed");

    engine
        .prepare(
            &session,
            "ins_param_cached".to_owned(),
            "INSERT INTO history (tid, bid, aid, delta, note) VALUES ($1, $2, $3, $4, 'ok')"
                .to_owned(),
        )
        .expect("prepare");

    assert_eq!(engine.plan_cache_hits(), 0);

    engine
        .bind(
            &session,
            "p_first".to_owned(),
            "ins_param_cached".to_owned(),
            vec![
                aiondb_core::Value::Int(1),
                aiondb_core::Value::Int(2),
                aiondb_core::Value::Int(3),
                aiondb_core::Value::Int(4),
            ],
        )
        .expect("bind first");
    let first = engine
        .execute_portal(&session, "p_first", 0)
        .expect("execute first");
    assert!(
        first.tag.starts_with("INSERT"),
        "unexpected tag: {}",
        first.tag
    );
    assert_eq!(engine.plan_cache_hits(), 0);

    engine
        .bind(
            &session,
            "p_second".to_owned(),
            "ins_param_cached".to_owned(),
            vec![
                aiondb_core::Value::Int(10),
                aiondb_core::Value::Int(20),
                aiondb_core::Value::Int(30),
                aiondb_core::Value::Int(40),
            ],
        )
        .expect("bind second");
    let second = engine
        .execute_portal(&session, "p_second", 0)
        .expect("execute second");
    assert!(
        second.tag.starts_with("INSERT"),
        "unexpected tag: {}",
        second.tag
    );
    assert_eq!(engine.plan_cache_hits(), 1);

    let rows = engine
        .execute_sql(
            &session,
            "SELECT tid, bid, aid, delta, note FROM history ORDER BY tid",
        )
        .expect("verify");
    assert_eq!(
        query_rows_as_text(rows),
        vec![
            vec![
                "1".to_owned(),
                "2".to_owned(),
                "3".to_owned(),
                "4".to_owned(),
                "ok".to_owned(),
            ],
            vec![
                "10".to_owned(),
                "20".to_owned(),
                "30".to_owned(),
                "40".to_owned(),
                "ok".to_owned(),
            ],
        ]
    );
}

#[test]
fn execute_prepared_statement_respects_updated_bind_values() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE users (id INT, name TEXT); \
             INSERT INTO users VALUES (1, 'alice'), (2, 'bob')",
        )
        .expect("seed");

    engine
        .prepare(
            &session,
            "sel_param_exec".to_owned(),
            "SELECT id, name FROM users WHERE id = $1".to_owned(),
        )
        .expect("prepare");

    let (batch1, notices1) = engine
        .execute_prepared_statement_with_notices(
            &session,
            "sel_param_exec".to_owned(),
            vec![aiondb_core::Value::Int(1)],
            usize::MAX,
        )
        .expect("execute prepared with id=1");
    assert!(
        notices1.is_empty(),
        "unexpected notices for id=1: {notices1:?}"
    );
    assert_eq!(
        batch1.rows,
        vec![aiondb_core::Row::new(vec![
            aiondb_core::Value::Int(1),
            aiondb_core::Value::Text("alice".to_owned()),
        ])]
    );

    let (batch2, notices2) = engine
        .execute_prepared_statement_with_notices(
            &session,
            "sel_param_exec".to_owned(),
            vec![aiondb_core::Value::Int(2)],
            usize::MAX,
        )
        .expect("execute prepared with id=2");
    assert!(
        notices2.is_empty(),
        "unexpected notices for id=2: {notices2:?}"
    );
    assert_eq!(
        batch2.rows,
        vec![aiondb_core::Row::new(vec![
            aiondb_core::Value::Int(2),
            aiondb_core::Value::Text("bob".to_owned()),
        ])]
    );
}

#[test]
fn prepares_and_executes_parameterized_vector_distance_portal() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, v VECTOR(3)); \
             CREATE INDEX idx_v ON items USING hnsw (v); \
             INSERT INTO items VALUES \
                 (1, '[1.0,0.0,0.0]'), \
                 (2, '[0.0,1.0,0.0]'), \
                 (3, '[0.0,0.0,1.0]'), \
                 (4, '[1.0,1.0,0.0]')",
        )
        .expect("setup");

    let desc = engine
        .prepare(
            &session,
            "vec_nn".to_owned(),
            "SELECT id FROM items ORDER BY l2_distance(v, $1) LIMIT 1".to_owned(),
        )
        .expect("prepare vector nearest-neighbor");
    assert_eq!(
        desc.param_types,
        vec![aiondb_core::DataType::Vector {
            dims: 3,
            element_type: aiondb_core::VectorElementType::Float32
        }]
    );

    engine
        .bind(
            &session,
            "vec_nn_portal".to_owned(),
            "vec_nn".to_owned(),
            vec![aiondb_core::Value::Vector(aiondb_core::VectorValue::new(
                3,
                vec![1.0, 0.0, 0.0],
            ))],
        )
        .expect("bind vector parameter");

    let batch = engine
        .execute_portal(&session, "vec_nn_portal", 0)
        .expect("execute prepared vector nearest-neighbor");

    assert_eq!(batch.rows.len(), 1);
    assert!(matches!(
        batch.rows[0].values.first(),
        Some(aiondb_core::Value::Int(1))
    ));
    assert!(matches!(
        batch.columns.first(),
        Some(ResultColumn {
            name,
            data_type: aiondb_core::DataType::Int,
            ..
        }) if name == "id"
    ));
    assert!(batch.exhausted);
}

#[test]
fn prepares_and_executes_parameterized_date_portal() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let desc = engine
        .prepare(
            &session,
            "date_param".to_owned(),
            "SELECT CAST($1 AS DATE) AS d".to_owned(),
        )
        .expect("prepare date portal");
    assert_eq!(desc.param_types, vec![aiondb_core::DataType::Date]);

    let value = Date::from_calendar_date(2024, Month::March, 15).expect("valid date");
    engine
        .bind(
            &session,
            "date_portal".to_owned(),
            "date_param".to_owned(),
            vec![aiondb_core::Value::Date(value)],
        )
        .expect("bind date parameter");

    let batch = engine
        .execute_portal(&session, "date_portal", 0)
        .expect("execute prepared date portal");
    assert_eq!(
        batch.rows,
        vec![aiondb_core::Row::new(vec![aiondb_core::Value::Date(value)])]
    );
}

#[test]
fn prepares_and_executes_parameterized_jsonb_portal() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let desc = engine
        .prepare(
            &session,
            "jsonb_param".to_owned(),
            "SELECT CAST($1 AS JSONB) AS doc".to_owned(),
        )
        .expect("prepare jsonb portal");
    assert_eq!(desc.param_types, vec![aiondb_core::DataType::Jsonb]);

    let value = json!({ "name": "alice", "score": 42 });
    engine
        .bind(
            &session,
            "jsonb_portal".to_owned(),
            "jsonb_param".to_owned(),
            vec![aiondb_core::Value::Jsonb(value.clone())],
        )
        .expect("bind jsonb parameter");

    let batch = engine
        .execute_portal(&session, "jsonb_portal", 0)
        .expect("execute prepared jsonb portal");
    assert_eq!(
        batch.rows,
        vec![aiondb_core::Row::new(vec![aiondb_core::Value::Jsonb(
            value
        )])]
    );
}

#[test]
fn prepares_and_executes_parameterized_int_array_portal() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let desc = engine
        .prepare(
            &session,
            "int_array_param".to_owned(),
            "SELECT CAST($1 AS INT[]) AS xs".to_owned(),
        )
        .expect("prepare array portal");
    assert_eq!(
        desc.param_types,
        vec![aiondb_core::DataType::Array(Box::new(
            aiondb_core::DataType::Int
        ))]
    );

    let value = aiondb_core::Value::Array(vec![
        aiondb_core::Value::Int(1),
        aiondb_core::Value::Int(2),
        aiondb_core::Value::Int(3),
    ]);
    engine
        .bind(
            &session,
            "int_array_portal".to_owned(),
            "int_array_param".to_owned(),
            vec![value.clone()],
        )
        .expect("bind array parameter");

    let batch = engine
        .execute_portal(&session, "int_array_portal", 0)
        .expect("execute prepared array portal");
    assert_eq!(batch.rows, vec![aiondb_core::Row::new(vec![value])]);
}

#[test]
fn prepares_and_executes_parameterized_empty_int_array_portal() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let desc = engine
        .prepare(
            &session,
            "empty_int_array_param".to_owned(),
            "SELECT CAST($1 AS INT[]) AS xs".to_owned(),
        )
        .expect("prepare empty array portal");
    assert_eq!(
        desc.param_types,
        vec![aiondb_core::DataType::Array(Box::new(
            aiondb_core::DataType::Int
        ))]
    );

    let value = aiondb_core::Value::Array(Vec::new());
    engine
        .bind(
            &session,
            "empty_int_array_portal".to_owned(),
            "empty_int_array_param".to_owned(),
            vec![value.clone()],
        )
        .expect("bind empty array parameter");

    let batch = engine
        .execute_portal(&session, "empty_int_array_portal", 0)
        .expect("execute prepared empty array portal");
    assert_eq!(batch.rows, vec![aiondb_core::Row::new(vec![value])]);
}

#[test]
fn prepared_int_array_portal_rejects_wrong_element_type() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .prepare(
            &session,
            "wrong_int_array_param".to_owned(),
            "SELECT CAST($1 AS INT[]) AS xs".to_owned(),
        )
        .expect("prepare wrong-type array portal");

    let err = engine
        .bind(
            &session,
            "wrong_int_array_portal".to_owned(),
            "wrong_int_array_param".to_owned(),
            vec![aiondb_core::Value::Array(vec![aiondb_core::Value::Text(
                "oops".to_owned(),
            )])],
        )
        .expect_err("wrong element type should fail at bind");
    assert_eq!(err.sqlstate(), aiondb_core::SqlState::DatatypeMismatch);
    assert!(err.to_string().contains("expects INT[]"));
}

#[test]
fn prepares_and_executes_parameterized_blob_portal() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let desc = engine
        .prepare(
            &session,
            "blob_param".to_owned(),
            "SELECT CAST($1 AS BLOB) AS payload".to_owned(),
        )
        .expect("prepare blob portal");
    assert_eq!(desc.param_types, vec![aiondb_core::DataType::Blob]);

    let value = aiondb_core::Value::Blob(vec![0xde, 0xad, 0xbe, 0xef]);
    engine
        .bind(
            &session,
            "blob_portal".to_owned(),
            "blob_param".to_owned(),
            vec![value.clone()],
        )
        .expect("bind blob parameter");

    let batch = engine
        .execute_portal(&session, "blob_portal", 0)
        .expect("execute prepared blob portal");
    assert_eq!(batch.rows, vec![aiondb_core::Row::new(vec![value])]);
}

#[test]
fn prepared_explain_vector_distance_uses_hnsw_scan_after_bind() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, v VECTOR(3)); \
             CREATE INDEX idx_v ON items USING hnsw (v); \
             INSERT INTO items VALUES \
                 (1, '[1.0,0.0,0.0]'), \
                 (2, '[0.0,1.0,0.0]'), \
                 (3, '[0.0,0.0,1.0]')",
        )
        .expect("setup");

    let desc = engine
        .prepare(
            &session,
            "vec_nn_explain".to_owned(),
            "EXPLAIN SELECT id FROM items ORDER BY l2_distance(v, $1) LIMIT 1".to_owned(),
        )
        .expect("prepare explain for vector nearest-neighbor");
    assert_eq!(
        desc.param_types,
        vec![aiondb_core::DataType::Vector {
            dims: 3,
            element_type: aiondb_core::VectorElementType::Float32
        }]
    );

    engine
        .bind(
            &session,
            "vec_nn_explain_portal".to_owned(),
            "vec_nn_explain".to_owned(),
            vec![aiondb_core::Value::Vector(aiondb_core::VectorValue::new(
                3,
                vec![1.0, 0.0, 0.0],
            ))],
        )
        .expect("bind vector parameter for explain");

    let batch = engine
        .execute_portal(&session, "vec_nn_explain_portal", 0)
        .expect("execute prepared explain");

    assert!(
        batch.rows.iter().any(|row| {
            matches!(
                row.values.first(),
                Some(aiondb_core::Value::Text(line)) if line.contains("HnswScan")
            )
        }),
        "expected HnswScan in EXPLAIN output, got rows: {:?}",
        batch.rows
    );
}

#[test]
fn executes_parameterized_insert_and_update_portals() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE users (id INT, name TEXT)")
        .expect("create");

    let insert_desc = engine
        .prepare(
            &session,
            "ins".to_owned(),
            "INSERT INTO users VALUES ($1, $2)".to_owned(),
        )
        .expect("prepare insert");
    assert_eq!(
        insert_desc.param_types,
        vec![aiondb_core::DataType::Int, aiondb_core::DataType::Text]
    );
    engine
        .bind(
            &session,
            "ins_portal".to_owned(),
            "ins".to_owned(),
            vec![
                aiondb_core::Value::Int(1),
                aiondb_core::Value::Text("alice".to_owned()),
            ],
        )
        .expect("bind insert");
    let insert_batch = engine
        .execute_portal(&session, "ins_portal", 0)
        .expect("execute insert");
    assert_eq!(insert_batch.tag, "INSERT");
    assert_eq!(insert_batch.rows_affected, 1);

    let update_desc = engine
        .prepare(
            &session,
            "upd".to_owned(),
            "UPDATE users SET name = $1 WHERE id = $2".to_owned(),
        )
        .expect("prepare update");
    assert_eq!(
        update_desc.param_types,
        vec![aiondb_core::DataType::Text, aiondb_core::DataType::Int]
    );
    engine
        .bind(
            &session,
            "upd_portal".to_owned(),
            "upd".to_owned(),
            vec![
                aiondb_core::Value::Text("updated".to_owned()),
                aiondb_core::Value::Int(1),
            ],
        )
        .expect("bind update");
    let update_batch = engine
        .execute_portal(&session, "upd_portal", 0)
        .expect("execute update");
    assert_eq!(update_batch.tag, "UPDATE");
    assert_eq!(update_batch.rows_affected, 1);

    let delete_desc = engine
        .prepare(
            &session,
            "del".to_owned(),
            "DELETE FROM users WHERE id = $1".to_owned(),
        )
        .expect("prepare delete");
    assert_eq!(delete_desc.param_types, vec![aiondb_core::DataType::Int]);
    engine
        .bind(
            &session,
            "del_portal".to_owned(),
            "del".to_owned(),
            vec![aiondb_core::Value::Int(1)],
        )
        .expect("bind delete");
    let delete_batch = engine
        .execute_portal(&session, "del_portal", 0)
        .expect("execute delete");
    assert_eq!(delete_batch.tag, "DELETE");
    assert_eq!(delete_batch.rows_affected, 1);

    let results = engine
        .execute_sql(&session, "SELECT id, name FROM users")
        .expect("select");
    assert_eq!(
        results,
        vec![StatementResult::Query {
            columns: vec![
                ResultColumn {
                    name: "id".to_owned(),
                    data_type: aiondb_core::DataType::Int,
                    text_type_modifier: None,
                    nullable: true,
                },
                ResultColumn {
                    name: "name".to_owned(),
                    data_type: aiondb_core::DataType::Text,
                    text_type_modifier: None,
                    nullable: true,
                },
            ],
            rows: vec![],
        }]
    );
}

#[test]
fn prepared_insert_null_for_not_null_column_is_lenient() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE users (id INT NOT NULL, name TEXT)")
        .expect("create");

    let desc = engine
        .prepare(
            &session,
            "ins_not_null".to_owned(),
            "INSERT INTO users VALUES ($1, $2)".to_owned(),
        )
        .expect("prepare insert");
    assert_eq!(
        desc.param_types,
        vec![aiondb_core::DataType::Int, aiondb_core::DataType::Text]
    );

    engine
        .bind(
            &session,
            "ins_not_null_portal".to_owned(),
            "ins_not_null".to_owned(),
            vec![
                aiondb_core::Value::Null,
                aiondb_core::Value::Text("alice".to_owned()),
            ],
        )
        .expect("bind insert");

    let err = engine
        .execute_portal(&session, "ins_not_null_portal", 0)
        .expect_err("NOT NULL column must reject NULL insert");
    assert_eq!(err.sqlstate(), aiondb_core::SqlState::NotNullViolation);
}

#[test]
fn prepares_and_executes_parameterized_cypher_portal() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE people (id INT PRIMARY KEY, name TEXT); \
             CREATE TABLE knows (source_id INT NOT NULL, target_id INT NOT NULL); \
             CREATE NODE LABEL Person ON people; \
             CREATE EDGE LABEL KNOWS ON knows SOURCE Person TARGET Person; \
             INSERT INTO people VALUES (1, 'alice'), (2, 'bob'); \
             INSERT INTO knows VALUES (1, 2)",
        )
        .expect("seed graph");

    let desc = engine
        .prepare(
            &session,
            "cypher_param".to_owned(),
            "MATCH (p:Person)-[:KNOWS]->(f:Person) WHERE p.id = $1 RETURN f.id LIMIT 20".to_owned(),
        )
        .expect("prepare cypher");
    assert_eq!(desc.param_types, vec![aiondb_core::DataType::Int]);

    engine
        .bind(
            &session,
            "cypher_portal".to_owned(),
            "cypher_param".to_owned(),
            vec![aiondb_core::Value::Int(1)],
        )
        .expect("bind cypher");

    let batch = engine
        .execute_portal(&session, "cypher_portal", 0)
        .expect("execute cypher");
    assert_eq!(
        batch.rows,
        vec![aiondb_core::Row::new(vec![aiondb_core::Value::Int(2)])]
    );
}

#[test]
fn bind_rejects_wrong_parameter_count() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .prepare(
            &session,
            "sel_param".to_owned(),
            "SELECT 1 WHERE $1".to_owned(),
        )
        .expect("prepare");

    let error = engine
        .bind(
            &session,
            "p1".to_owned(),
            "sel_param".to_owned(),
            Vec::new(),
        )
        .expect_err("wrong parameter count");
    assert_eq!(
        error.sqlstate(),
        aiondb_core::SqlState::InvalidParameterValue
    );
}

#[test]
fn prepares_and_executes_parameterized_exists_subquery_portal() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let desc = engine
        .prepare(
            &session,
            "exists_param".to_owned(),
            "SELECT EXISTS (SELECT 1 WHERE $1 = 1) AS matched".to_owned(),
        )
        .expect("prepare exists");
    assert_eq!(desc.param_types, vec![aiondb_core::DataType::Int]);

    engine
        .bind(
            &session,
            "exists_portal".to_owned(),
            "exists_param".to_owned(),
            vec![aiondb_core::Value::Int(1)],
        )
        .expect("bind exists");

    let batch = engine
        .execute_portal(&session, "exists_portal", 0)
        .expect("execute exists");
    assert_eq!(
        batch,
        PortalBatch {
            columns: vec![ResultColumn {
                name: "matched".to_owned(),
                data_type: aiondb_core::DataType::Boolean,
                text_type_modifier: None,
                nullable: false,
            }],
            rows: vec![aiondb_core::Row::new(vec![aiondb_core::Value::Boolean(
                true,
            )])],
            tag: "SELECT 1".to_owned(),
            rows_affected: 0,
            exhausted: true,
        }
    );
}

#[test]
fn prepares_and_executes_parameterized_pg_namespace_exists_portal() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let desc = engine
        .prepare(
            &session,
            "pg_namespace_exists".to_owned(),
            "SELECT EXISTS(SELECT 1 FROM pg_catalog.pg_namespace WHERE nspname = $1) AS schema_exists"
                .to_owned(),
        )
        .expect("prepare pg_namespace exists");
    assert_eq!(desc.param_types, vec![aiondb_core::DataType::Text]);

    engine
        .bind(
            &session,
            "pg_namespace_exists_portal".to_owned(),
            "pg_namespace_exists".to_owned(),
            vec![aiondb_core::Value::Text("public".to_owned())],
        )
        .expect("bind pg_namespace exists");

    let batch = engine
        .execute_portal(&session, "pg_namespace_exists_portal", 0)
        .expect("execute pg_namespace exists");
    assert_eq!(
        batch,
        PortalBatch {
            columns: vec![ResultColumn {
                name: "schema_exists".to_owned(),
                data_type: aiondb_core::DataType::Boolean,
                text_type_modifier: None,
                nullable: false,
            }],
            rows: vec![aiondb_core::Row::new(vec![aiondb_core::Value::Boolean(
                true,
            )])],
            tag: "SELECT 1".to_owned(),
            rows_affected: 0,
            exhausted: true,
        }
    );
}

#[test]
fn execute_portal_respects_max_rows() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE users (id INT); \
             INSERT INTO users VALUES (1), (2), (3)",
        )
        .expect("seed");
    engine
        .prepare(
            &session,
            "sel".to_owned(),
            "SELECT id FROM users".to_owned(),
        )
        .expect("prepare");
    engine
        .bind(&session, "p1".to_owned(), "sel".to_owned(), Vec::new())
        .expect("bind");

    let first = engine
        .execute_portal(&session, "p1", 1)
        .expect("first batch");
    assert_eq!(
        first,
        PortalBatch {
            columns: vec![ResultColumn {
                name: "id".to_owned(),
                data_type: aiondb_core::DataType::Int,
                text_type_modifier: None,
                nullable: true,
            }],
            rows: vec![aiondb_core::Row::new(vec![aiondb_core::Value::Int(1)])],
            tag: "SELECT".to_owned(),
            rows_affected: 0,
            exhausted: false,
        }
    );

    let second = engine
        .execute_portal(&session, "p1", 1)
        .expect("second batch");
    assert_eq!(
        second.rows,
        vec![aiondb_core::Row::new(vec![aiondb_core::Value::Int(2)])]
    );
    assert!(!second.exhausted);

    let third = engine
        .execute_portal(&session, "p1", 1)
        .expect("third batch");
    assert_eq!(
        third,
        PortalBatch {
            columns: vec![ResultColumn {
                name: "id".to_owned(),
                data_type: aiondb_core::DataType::Int,
                text_type_modifier: None,
                nullable: true,
            }],
            rows: vec![aiondb_core::Row::new(vec![aiondb_core::Value::Int(3)])],
            tag: "SELECT 3".to_owned(),
            rows_affected: 0,
            exhausted: true,
        }
    );
}

#[test]
fn suspended_dml_returning_portal_does_not_reexecute_side_effects() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE t_ret_portal (id INT)")
        .expect("create table");
    engine
        .prepare(
            &session,
            "ins_ret".to_owned(),
            "INSERT INTO t_ret_portal VALUES (1), (2) RETURNING id".to_owned(),
        )
        .expect("prepare insert returning");
    engine
        .bind(
            &session,
            "p_ret".to_owned(),
            "ins_ret".to_owned(),
            Vec::new(),
        )
        .expect("bind portal");

    let first = engine
        .execute_portal(&session, "p_ret", 1)
        .expect("first batch");
    assert_eq!(
        first.rows,
        vec![aiondb_core::Row::new(vec![aiondb_core::Value::Int(1)])]
    );
    assert!(!first.exhausted);

    let second = engine
        .execute_portal(&session, "p_ret", 1)
        .expect("second batch");
    assert_eq!(
        second,
        PortalBatch {
            columns: vec![ResultColumn {
                name: "id".to_owned(),
                data_type: aiondb_core::DataType::Int,
                text_type_modifier: None,
                nullable: true,
            }],
            rows: vec![aiondb_core::Row::new(vec![aiondb_core::Value::Int(2)])],
            tag: "INSERT 0 2".to_owned(),
            rows_affected: 0,
            exhausted: true,
        }
    );

    let count = engine
        .execute_sql(&session, "SELECT COUNT(*) AS count FROM t_ret_portal")
        .expect("count rows after portal execution");
    assert!(matches!(
        count.as_slice(),
        [StatementResult::Query { rows, .. }]
            if rows == &[aiondb_core::Row::new(vec![aiondb_core::Value::BigInt(2)])]
    ));
}

#[test]
fn parameter_in_returning_requires_prepare() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE t_param_returning (id INT)")
        .expect("create table");

    let error = engine
        .execute_sql(
            &session,
            "INSERT INTO t_param_returning VALUES (1) RETURNING $1",
        )
        .expect_err("parameterized RETURNING should require prepare");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::UndefinedParameter);
    assert!(
        error
            .report()
            .message
            .contains("parameterized statements must be prepared before execution"),
        "unexpected error: {}",
        error.report().message
    );
}

#[test]
fn prepared_insert_on_conflict_and_returning_bind_params_across_subtrees() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE t_conflict_returning (id INT PRIMARY KEY, val TEXT); \
             INSERT INTO t_conflict_returning VALUES (1, 'seed')",
        )
        .expect("seed table");
    engine
        .prepare(
            &session,
            "ins_conflict_ret".to_owned(),
            "INSERT INTO t_conflict_returning VALUES ($1, $2) \
             ON CONFLICT (id) DO UPDATE SET val = $3 WHERE id = $1 \
             RETURNING $4::TEXT AS marker, val"
                .to_owned(),
        )
        .expect("prepare insert on conflict returning");
    engine
        .bind(
            &session,
            "p_conflict_ret".to_owned(),
            "ins_conflict_ret".to_owned(),
            vec![
                aiondb_core::Value::Int(1),
                aiondb_core::Value::Text("ignored".to_owned()),
                aiondb_core::Value::Text("updated".to_owned()),
                aiondb_core::Value::Text("ok".to_owned()),
            ],
        )
        .expect("bind portal");

    let batch = engine
        .execute_portal(&session, "p_conflict_ret", 0)
        .expect("execute portal");
    assert_eq!(
        batch,
        PortalBatch {
            columns: vec![
                ResultColumn {
                    name: "marker".to_owned(),
                    data_type: aiondb_core::DataType::Text,
                    text_type_modifier: None,
                    nullable: false,
                },
                ResultColumn {
                    name: "val".to_owned(),
                    data_type: aiondb_core::DataType::Text,
                    text_type_modifier: None,
                    nullable: true,
                },
            ],
            rows: vec![aiondb_core::Row::new(vec![
                aiondb_core::Value::Text("ok".to_owned()),
                aiondb_core::Value::Text("updated".to_owned()),
            ])],
            tag: "INSERT 0 1".to_owned(),
            rows_affected: 0,
            exhausted: true,
        }
    );

    let row = engine
        .execute_sql(
            &session,
            "SELECT val FROM t_conflict_returning WHERE id = 1",
        )
        .expect("select updated row");
    assert!(matches!(
        row.as_slice(),
        [StatementResult::Query { rows, .. }]
            if rows == &[aiondb_core::Row::new(vec![aiondb_core::Value::Text(
                "updated".to_owned()
            )])]
    ));
}

#[test]
fn prepared_merge_binds_params_in_on_and_when_clauses() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE merge_target_portal (id INT PRIMARY KEY, val TEXT); \
             CREATE TABLE merge_source_portal (id INT PRIMARY KEY, val TEXT); \
             INSERT INTO merge_target_portal VALUES (1, 'old'); \
             INSERT INTO merge_source_portal VALUES (1, 'seed')",
        )
        .expect("seed merge tables");
    engine
        .prepare(
            &session,
            "merge_stmt".to_owned(),
            "MERGE INTO merge_target_portal AS t \
             USING merge_source_portal AS s \
             ON t.id = s.id AND t.id = $1 \
             WHEN MATCHED AND s.val = $2 THEN UPDATE SET val = $3 \
             WHEN NOT MATCHED THEN INSERT (id, val) VALUES ($4, $5)"
                .to_owned(),
        )
        .expect("prepare merge");
    engine
        .bind(
            &session,
            "p_merge".to_owned(),
            "merge_stmt".to_owned(),
            vec![
                aiondb_core::Value::Int(1),
                aiondb_core::Value::Text("seed".to_owned()),
                aiondb_core::Value::Text("updated".to_owned()),
                aiondb_core::Value::Int(2),
                aiondb_core::Value::Text("inserted".to_owned()),
            ],
        )
        .expect("bind merge params");

    let batch = engine
        .execute_portal(&session, "p_merge", 0)
        .expect("execute merge portal");
    assert!(batch.exhausted);

    let row = engine
        .execute_sql(&session, "SELECT val FROM merge_target_portal WHERE id = 1")
        .expect("select merged row");
    assert!(matches!(
        row.as_slice(),
        [StatementResult::Query { rows, .. }]
            if rows == &[aiondb_core::Row::new(vec![aiondb_core::Value::Text(
                "updated".to_owned()
            )])]
    ));
}

#[test]
fn execute_portal_reports_select_zero_for_empty_results() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .prepare(
            &session,
            "sel_empty".to_owned(),
            "SELECT 1 WHERE FALSE".to_owned(),
        )
        .expect("prepare");
    engine
        .bind(
            &session,
            "p_empty".to_owned(),
            "sel_empty".to_owned(),
            Vec::new(),
        )
        .expect("bind");

    let batch = engine
        .execute_portal(&session, "p_empty", 0)
        .expect("execute empty portal");
    assert_eq!(
        batch,
        PortalBatch {
            columns: vec![ResultColumn {
                name: "?column?".to_owned(),
                data_type: aiondb_core::DataType::Int,
                text_type_modifier: None,
                nullable: false,
            }],
            rows: vec![],
            tag: "SELECT 0".to_owned(),
            rows_affected: 0,
            exhausted: true,
        }
    );
}

#[test]
fn null_comparison_in_where_returns_no_rows() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(&session, "SELECT 1 AS one WHERE NULL = 1")
        .expect("execute");
    assert_eq!(
        results,
        vec![StatementResult::Query {
            columns: vec![ResultColumn {
                name: "one".to_owned(),
                data_type: aiondb_core::DataType::Int,
                text_type_modifier: None,
                nullable: false,
            }],
            rows: Vec::new(),
        }]
    );
}

#[test]
fn execute_sql_rejects_parameterized_statements() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(&session, "SELECT $1")
        .expect_err("parameterized direct execution");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::UndefinedParameter);
}

#[test]
fn execute_sql_supports_compat_cursor_fetch_all() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE cursor_items (id INT); \
             INSERT INTO cursor_items VALUES (1), (2)",
        )
        .expect("seed");
    engine.execute_sql(&session, "BEGIN").expect("begin");
    engine
        .execute_sql(
            &session,
            "DECLARE c CURSOR FOR SELECT id FROM cursor_items ORDER BY id",
        )
        .expect("declare");

    let results = engine
        .execute_sql(&session, "FETCH ALL IN c")
        .expect("fetch all");
    assert_eq!(
        results,
        vec![StatementResult::Query {
            columns: vec![ResultColumn {
                name: "id".to_owned(),
                data_type: aiondb_core::DataType::Int,
                text_type_modifier: None,
                nullable: true,
            }],
            rows: vec![
                aiondb_core::Row::new(vec![aiondb_core::Value::Int(1)]),
                aiondb_core::Row::new(vec![aiondb_core::Value::Int(2)]),
            ],
        }]
    );

    engine.execute_sql(&session, "CLOSE c").expect("close");
    engine.execute_sql(&session, "COMMIT").expect("commit");
}

#[test]
fn exhausted_compat_cursor_fetch_keeps_result_columns() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE cursor_page (id INT); \
             INSERT INTO cursor_page VALUES (1)",
        )
        .expect("seed");
    engine.execute_sql(&session, "BEGIN").expect("begin");
    engine
        .execute_sql(
            &session,
            "DECLARE c CURSOR FOR SELECT id FROM cursor_page ORDER BY id",
        )
        .expect("declare");

    let first = engine
        .execute_sql(&session, "FETCH NEXT IN c")
        .expect("first fetch");
    assert_eq!(
        first,
        vec![StatementResult::Query {
            columns: vec![ResultColumn {
                name: "id".to_owned(),
                data_type: aiondb_core::DataType::Int,
                text_type_modifier: None,
                nullable: true,
            }],
            rows: vec![aiondb_core::Row::new(vec![aiondb_core::Value::Int(1)])],
        }]
    );

    let second = engine
        .execute_sql(&session, "FETCH NEXT IN c")
        .expect("empty fetch");
    assert_eq!(
        second,
        vec![StatementResult::Query {
            columns: vec![ResultColumn {
                name: "id".to_owned(),
                data_type: aiondb_core::DataType::Int,
                text_type_modifier: None,
                nullable: true,
            }],
            rows: Vec::new(),
        }]
    );

    engine.execute_sql(&session, "CLOSE c").expect("close");
    engine.execute_sql(&session, "COMMIT").expect("commit");
}

// =====================================================================
// Prepared vector parameter tests
// =====================================================================
