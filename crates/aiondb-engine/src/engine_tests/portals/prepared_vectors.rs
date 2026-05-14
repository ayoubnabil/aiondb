use super::*;

#[test]
fn prepared_vector_rebind_returns_different_nearest_neighbor() {
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
            "vec_rebind".to_owned(),
            "SELECT id FROM items ORDER BY l2_distance(v, $1) LIMIT 1".to_owned(),
        )
        .expect("prepare");
    assert_eq!(
        desc.param_types,
        vec![aiondb_core::DataType::Vector {
            dims: 3,
            element_type: aiondb_core::VectorElementType::Float32
        }]
    );

    // First bind: query near [1,0,0] → expect id=1
    engine
        .bind(
            &session,
            "p_first".to_owned(),
            "vec_rebind".to_owned(),
            vec![aiondb_core::Value::Vector(aiondb_core::VectorValue::new(
                3,
                vec![1.0, 0.0, 0.0],
            ))],
        )
        .expect("bind first");
    let batch1 = engine
        .execute_portal(&session, "p_first", 0)
        .expect("execute first");
    assert_eq!(batch1.rows.len(), 1);
    assert_eq!(batch1.rows[0].values[0], aiondb_core::Value::Int(1));

    // Second bind: query near [0,0,1] → expect id=3
    engine
        .bind(
            &session,
            "p_second".to_owned(),
            "vec_rebind".to_owned(),
            vec![aiondb_core::Value::Vector(aiondb_core::VectorValue::new(
                3,
                vec![0.0, 0.0, 1.0],
            ))],
        )
        .expect("bind second");
    let batch2 = engine
        .execute_portal(&session, "p_second", 0)
        .expect("execute second");
    assert_eq!(batch2.rows.len(), 1);
    assert_eq!(batch2.rows[0].values[0], aiondb_core::Value::Int(3));
}

#[test]
fn prepared_vector_top_k_returns_ordered_results() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, v VECTOR(3)); \
             CREATE INDEX idx_v ON items USING hnsw (v); \
             INSERT INTO items VALUES \
                 (1, '[1.0,0.0,0.0]'), \
                 (2, '[0.9,0.1,0.0]'), \
                 (3, '[0.0,1.0,0.0]'), \
                 (4, '[0.0,0.0,1.0]'), \
                 (5, '[10.0,10.0,10.0]')",
        )
        .expect("setup");

    engine
        .prepare(
            &session,
            "vec_topk".to_owned(),
            "SELECT id FROM items ORDER BY l2_distance(v, $1) LIMIT 3".to_owned(),
        )
        .expect("prepare");
    engine
        .bind(
            &session,
            "pk".to_owned(),
            "vec_topk".to_owned(),
            vec![aiondb_core::Value::Vector(aiondb_core::VectorValue::new(
                3,
                vec![1.0, 0.0, 0.0],
            ))],
        )
        .expect("bind");

    let batch = engine
        .execute_portal(&session, "pk", 0)
        .expect("execute top-k");
    assert_eq!(batch.rows.len(), 3, "expected 3 nearest neighbors");
    // id=1 is exact match (distance 0), must be first
    assert_eq!(batch.rows[0].values[0], aiondb_core::Value::Int(1));
    for row in &batch.rows {
        assert_ne!(row.values[0], aiondb_core::Value::Int(5));
    }
}

#[test]
fn prepared_vector_search_without_hnsw_uses_exact_scan() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    // No HNSW index: must fall back to exact scan
    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, v VECTOR(3)); \
             INSERT INTO items VALUES \
                 (1, '[1.0,0.0,0.0]'), \
                 (2, '[0.0,1.0,0.0]'), \
                 (3, '[0.0,0.0,1.0]')",
        )
        .expect("setup");

    let desc = engine
        .prepare(
            &session,
            "vec_exact".to_owned(),
            "SELECT id FROM items ORDER BY l2_distance(v, $1) LIMIT 1".to_owned(),
        )
        .expect("prepare");
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
            "pe".to_owned(),
            "vec_exact".to_owned(),
            vec![aiondb_core::Value::Vector(aiondb_core::VectorValue::new(
                3,
                vec![0.0, 1.0, 0.0],
            ))],
        )
        .expect("bind");

    let batch = engine
        .execute_portal(&session, "pe", 0)
        .expect("execute exact scan");
    assert_eq!(batch.rows.len(), 1);
    assert_eq!(batch.rows[0].values[0], aiondb_core::Value::Int(2));
}

#[test]
fn prepared_cosine_distance_with_vector_parameter() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, v VECTOR(3)); \
             INSERT INTO items VALUES \
                 (1, '[1.0,0.0,0.0]'), \
                 (2, '[0.0,1.0,0.0]'), \
                 (3, '[1.0,1.0,0.0]')",
        )
        .expect("setup");

    let desc = engine
        .prepare(
            &session,
            "vec_cos".to_owned(),
            "SELECT id FROM items ORDER BY cosine_distance(v, $1) LIMIT 1".to_owned(),
        )
        .expect("prepare cosine");
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
            "pc".to_owned(),
            "vec_cos".to_owned(),
            vec![aiondb_core::Value::Vector(aiondb_core::VectorValue::new(
                3,
                vec![1.0, 0.0, 0.0],
            ))],
        )
        .expect("bind cosine");

    let batch = engine
        .execute_portal(&session, "pc", 0)
        .expect("execute cosine");
    // [1,0,0] has cosine distance 0 to query [1,0,0]
    assert_eq!(batch.rows.len(), 1);
    assert_eq!(batch.rows[0].values[0], aiondb_core::Value::Int(1));
}

#[test]
fn prepared_vector_bind_wrong_dimensions_is_rejected() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, v VECTOR(3)); \
             INSERT INTO items VALUES (1, '[1.0,0.0,0.0]')",
        )
        .expect("setup");

    engine
        .prepare(
            &session,
            "vec_dim".to_owned(),
            "SELECT id FROM items ORDER BY l2_distance(v, $1) LIMIT 1".to_owned(),
        )
        .expect("prepare");

    // Bind a VECTOR(2) to a slot expecting VECTOR(3): reject at bind.
    let err = engine
        .bind(
            &session,
            "pd".to_owned(),
            "vec_dim".to_owned(),
            vec![aiondb_core::Value::Vector(aiondb_core::VectorValue::new(
                2,
                vec![1.0, 0.0],
            ))],
        )
        .expect_err("bind should reject vector dimension mismatch");
    assert_eq!(err.sqlstate(), aiondb_core::SqlState::DatatypeMismatch);
    let msg = err.to_string();
    assert!(
        msg.contains("VECTOR(3)") && msg.contains("VECTOR(2)"),
        "expected vector dimension mismatch message, got: {msg}"
    );
}

#[test]
fn prepared_vector_bind_non_vector_type_is_rejected() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, v VECTOR(3)); \
             INSERT INTO items VALUES (1, '[1.0,0.0,0.0]')",
        )
        .expect("setup");

    engine
        .prepare(
            &session,
            "vec_type".to_owned(),
            "SELECT id FROM items ORDER BY l2_distance(v, $1) LIMIT 1".to_owned(),
        )
        .expect("prepare");

    let err = engine
        .bind(
            &session,
            "pt".to_owned(),
            "vec_type".to_owned(),
            vec![aiondb_core::Value::Text("[1.0,0.0,0.0]".to_owned())],
        )
        .expect_err("bind should reject non-vector value for vector parameter");
    assert_eq!(err.sqlstate(), aiondb_core::SqlState::DatatypeMismatch);
    let msg = err.to_string();
    assert!(
        msg.contains("VECTOR(3)") && msg.contains("TEXT"),
        "expected vector-vs-text mismatch message, got: {msg}"
    );
}

#[test]
fn compat_scroll_cursor_fetch_directions_follow_portal_position() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE cursor_scroll (id INT); \
             INSERT INTO cursor_scroll VALUES (1), (2), (3)",
        )
        .expect("seed");
    engine.execute_sql(&session, "BEGIN").expect("begin");
    engine
        .execute_sql(
            &session,
            "DECLARE c SCROLL CURSOR FOR SELECT id FROM cursor_scroll ORDER BY id",
        )
        .expect("declare");

    let first = engine
        .execute_sql(&session, "FETCH NEXT c")
        .expect("fetch next");
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
        .execute_sql(&session, "FETCH NEXT c")
        .expect("fetch next second");
    assert_eq!(
        second,
        vec![StatementResult::Query {
            columns: vec![ResultColumn {
                name: "id".to_owned(),
                data_type: aiondb_core::DataType::Int,
                text_type_modifier: None,
                nullable: true,
            }],
            rows: vec![aiondb_core::Row::new(vec![aiondb_core::Value::Int(2)])],
        }]
    );

    let prior = engine
        .execute_sql(&session, "FETCH PRIOR c")
        .expect("fetch prior");
    assert_eq!(
        prior,
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

    let first_again = engine
        .execute_sql(&session, "FETCH FIRST c")
        .expect("fetch first");
    assert_eq!(
        first_again,
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

    let last = engine
        .execute_sql(&session, "FETCH LAST c")
        .expect("fetch last");
    assert_eq!(
        last,
        vec![StatementResult::Query {
            columns: vec![ResultColumn {
                name: "id".to_owned(),
                data_type: aiondb_core::DataType::Int,
                text_type_modifier: None,
                nullable: true,
            }],
            rows: vec![aiondb_core::Row::new(vec![aiondb_core::Value::Int(3)])],
        }]
    );

    engine.execute_sql(&session, "CLOSE c").expect("close");
    engine.execute_sql(&session, "COMMIT").expect("commit");
}
