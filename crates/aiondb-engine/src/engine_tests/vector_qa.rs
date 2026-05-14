use super::*;

// =====================================================================
// Exactness tests -- verify computed distances match expected values
// =====================================================================

#[test]
fn l2_distance_exact() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    // l2_distance([1,0,0], [0,1,0]) = sqrt((1-0)^2 + (0-1)^2 + (0-0)^2) = sqrt(2)
    // Store both vectors as columns in the same row to avoid aliases.
    let results = engine
        .execute_sql(
            &session,
            "CREATE TABLE vecs (v1 VECTOR(3), v2 VECTOR(3)); \
             INSERT INTO vecs VALUES ('[1.0,0.0,0.0]', '[0.0,1.0,0.0]'); \
             SELECT l2_distance(v1, v2) AS dist FROM vecs",
        )
        .expect("execute");

    match &results[2] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            let dist = match &rows[0].values[0] {
                aiondb_core::Value::Double(d) => *d,
                other => panic!("expected Double, got {other:?}"),
            };
            let expected = 2.0_f64.sqrt();
            assert!(
                (dist - expected).abs() < 1e-10,
                "l2_distance should be sqrt(2) = {expected}, got {dist}"
            );
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn cosine_distance_exact() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    // cosine_distance([1,0,0], [0,1,0]):
    //   dot = 0, norm_a = 1, norm_b = 1, similarity = 0/1 = 0
    //   cosine_distance = 1 - 0 = 1.0  (orthogonal vectors)
    let results = engine
        .execute_sql(
            &session,
            "CREATE TABLE vecs (v1 VECTOR(3), v2 VECTOR(3)); \
             INSERT INTO vecs VALUES ('[1.0,0.0,0.0]', '[0.0,1.0,0.0]'); \
             SELECT cosine_distance(v1, v2) AS dist FROM vecs",
        )
        .expect("execute");

    match &results[2] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            let dist = match &rows[0].values[0] {
                aiondb_core::Value::Double(d) => *d,
                other => panic!("expected Double, got {other:?}"),
            };
            assert!(
                (dist - 1.0).abs() < 1e-10,
                "cosine_distance of orthogonal vectors should be 1.0, got {dist}"
            );
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn cosine_distance_parallel_vectors() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    // cosine_distance([1,2,3], [2,4,6]):
    //   They are parallel (same direction), so similarity = 1, distance = 0.
    let results = engine
        .execute_sql(
            &session,
            "CREATE TABLE vecs (v1 VECTOR(3), v2 VECTOR(3)); \
             INSERT INTO vecs VALUES ('[1.0,2.0,3.0]', '[2.0,4.0,6.0]'); \
             SELECT cosine_distance(v1, v2) AS dist FROM vecs",
        )
        .expect("execute");

    match &results[2] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            let dist = match &rows[0].values[0] {
                aiondb_core::Value::Double(d) => *d,
                other => panic!("expected Double, got {other:?}"),
            };
            assert!(
                dist.abs() < 1e-10,
                "cosine_distance of parallel vectors should be 0.0, got {dist}"
            );
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn inner_product_exact() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    // inner_product([1,2,3], [4,5,6]) = 1*4 + 2*5 + 3*6 = 4 + 10 + 18 = 32
    let results = engine
        .execute_sql(
            &session,
            "CREATE TABLE vecs (v1 VECTOR(3), v2 VECTOR(3)); \
             INSERT INTO vecs VALUES ('[1.0,2.0,3.0]', '[4.0,5.0,6.0]'); \
             SELECT inner_product(v1, v2) AS ip FROM vecs",
        )
        .expect("execute");

    match &results[2] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            let ip = match &rows[0].values[0] {
                aiondb_core::Value::Double(d) => *d,
                other => panic!("expected Double, got {other:?}"),
            };
            assert!(
                (ip - 32.0).abs() < 1e-10,
                "inner_product([1,2,3],[4,5,6]) should be 32.0, got {ip}"
            );
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn inner_product_orthogonal_is_zero() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    // inner_product([1,0], [0,1]) = 0
    let results = engine
        .execute_sql(
            &session,
            "CREATE TABLE vecs (v1 VECTOR(2), v2 VECTOR(2)); \
             INSERT INTO vecs VALUES ('[1.0,0.0]', '[0.0,1.0]'); \
             SELECT inner_product(v1, v2) AS ip FROM vecs",
        )
        .expect("execute");

    match &results[2] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            let ip = match &rows[0].values[0] {
                aiondb_core::Value::Double(d) => *d,
                other => panic!("expected Double, got {other:?}"),
            };
            assert!(
                ip.abs() < 1e-10,
                "inner_product of orthogonal vectors should be 0.0, got {ip}"
            );
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

// =====================================================================
// Dimension validation
// =====================================================================

#[test]
fn vector_wrong_dimensions_rejected() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE items (id INT, embedding VECTOR(3))")
        .expect("create table");

    // Inserting a 2-dimensional vector into a VECTOR(3) column should fail.
    let result = engine.execute_sql(&session, "INSERT INTO items VALUES (1, '[1.0,2.0]')");
    assert!(
        result.is_err(),
        "inserting a 2-dim vector into VECTOR(3) column should fail"
    );
}

#[test]
fn vector_too_many_dimensions_rejected() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE items (id INT, embedding VECTOR(3))")
        .expect("create table");

    // Inserting a 4-dimensional vector into a VECTOR(3) column should fail.
    let result = engine.execute_sql(
        &session,
        "INSERT INTO items VALUES (1, '[1.0,2.0,3.0,4.0]')",
    );
    assert!(
        result.is_err(),
        "inserting a 4-dim vector into VECTOR(3) column should fail"
    );
}

#[test]
fn distance_dimension_mismatch() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    // Two columns with different dimensions
    engine
        .execute_sql(
            &session,
            "CREATE TABLE mixed (v2 VECTOR(2), v3 VECTOR(3)); \
             INSERT INTO mixed VALUES ('[1.0,2.0]', '[1.0,2.0,3.0]')",
        )
        .expect("setup");

    // l2_distance between mismatched dimensions should fail
    let result = engine.execute_sql(&session, "SELECT l2_distance(v2, v3) FROM mixed");
    assert!(
        result.is_err(),
        "l2_distance between different dimension vectors should fail"
    );

    // cosine_distance between mismatched dimensions should also fail
    let result = engine.execute_sql(&session, "SELECT cosine_distance(v2, v3) FROM mixed");
    assert!(
        result.is_err(),
        "cosine_distance between different dimension vectors should fail"
    );

    // inner_product between mismatched dimensions should also fail
    let result = engine.execute_sql(&session, "SELECT inner_product(v2, v3) FROM mixed");
    assert!(
        result.is_err(),
        "inner_product between different dimension vectors should fail"
    );
}

// =====================================================================
// NULL handling
// =====================================================================

#[test]
fn vector_null_distance() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    // l2_distance(NULL, vec) should return NULL.
    // Store a NULL and a non-NULL vector in separate columns of the same row.
    let results = engine
        .execute_sql(
            &session,
            "CREATE TABLE items (v1 VECTOR(3), v2 VECTOR(3)); \
             INSERT INTO items VALUES (NULL, '[1.0,2.0,3.0]'); \
             SELECT l2_distance(v1, v2) AS dist FROM items",
        )
        .expect("execute");

    match &results[2] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(
                rows[0].values[0],
                aiondb_core::Value::Null,
                "l2_distance(NULL, vec) should return NULL"
            );
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn cosine_distance_null() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    // cosine_distance(vec, NULL) should return NULL.
    let results = engine
        .execute_sql(
            &session,
            "CREATE TABLE items (v1 VECTOR(3), v2 VECTOR(3)); \
             INSERT INTO items VALUES ('[1.0,2.0,3.0]', NULL); \
             SELECT cosine_distance(v1, v2) AS dist FROM items",
        )
        .expect("execute");

    match &results[2] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(
                rows[0].values[0],
                aiondb_core::Value::Null,
                "cosine_distance(vec, NULL) should return NULL"
            );
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn inner_product_null() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, embedding VECTOR(2)); \
             INSERT INTO items VALUES (1, NULL); \
             SELECT inner_product(embedding, embedding) AS ip FROM items",
        )
        .expect("execute");

    match &results[2] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(
                rows[0].values[0],
                aiondb_core::Value::Null,
                "inner_product(NULL, NULL) should return NULL"
            );
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

// =====================================================================
// Multiple rows with ORDER BY distance -- exact nearest neighbor
// =====================================================================

#[test]
fn vector_exact_nearest_neighbor() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    // Insert several 3D vectors and a query vector in a separate column.
    // Find the 3 nearest to [1,0,0] by L2 distance.
    //   [1,0,0]    -> dist 0.0
    //   [1,1,0]    -> dist 1.0
    //   [0,0,0]    -> dist 1.0
    //   [2,0,0]    -> dist 1.0
    //   [10,10,10] -> dist sqrt(81+100+100) = sqrt(281)
    let results = engine
        .execute_sql(
            &session,
            "CREATE TABLE vecs (id INT, v VECTOR(3), query VECTOR(3)); \
             INSERT INTO vecs VALUES \
                 (1, '[1.0,0.0,0.0]', '[1.0,0.0,0.0]'), \
                 (2, '[1.0,1.0,0.0]', '[1.0,0.0,0.0]'), \
                 (3, '[0.0,0.0,0.0]', '[1.0,0.0,0.0]'), \
                 (4, '[2.0,0.0,0.0]', '[1.0,0.0,0.0]'), \
                 (5, '[10.0,10.0,10.0]', '[1.0,0.0,0.0]'); \
             SELECT id, l2_distance(v, query) AS dist \
             FROM vecs \
             ORDER BY dist \
             LIMIT 3",
        )
        .expect("execute");

    match results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 3, "expected 3 nearest neighbors");

            // The closest should be id=1 (distance 0)
            assert_eq!(rows[0].values[0], aiondb_core::Value::Int(1));
            let dist0 = match &rows[0].values[1] {
                aiondb_core::Value::Double(d) => *d,
                other => panic!("expected Double, got {other:?}"),
            };
            assert!(
                dist0.abs() < 1e-10,
                "nearest should have distance 0, got {dist0}"
            );

            // The next two should all have distance 1.0 (ids 2, 3, or 4)
            let dist1 = match &rows[1].values[1] {
                aiondb_core::Value::Double(d) => *d,
                other => panic!("expected Double, got {other:?}"),
            };
            let dist2 = match &rows[2].values[1] {
                aiondb_core::Value::Double(d) => *d,
                other => panic!("expected Double, got {other:?}"),
            };
            assert!(
                (dist1 - 1.0).abs() < 1e-10,
                "second nearest should have distance 1.0, got {dist1}"
            );
            assert!(
                (dist2 - 1.0).abs() < 1e-10,
                "third nearest should have distance 1.0, got {dist2}"
            );

            // id=5 (distance ~16.7) should NOT be in the top 3
            for row in rows {
                assert_ne!(
                    row.values[0],
                    aiondb_core::Value::Int(5),
                    "the farthest vector should not be in top 3"
                );
            }
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn vector_order_by_cosine_distance() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    // Vectors at different angles from [1,0]:
    //   [1,0]  -> cosine_distance = 0  (same direction)
    //   [1,1]  -> cosine_distance = 1 - 1/sqrt(2) ~ 0.293
    //   [0,1]  -> cosine_distance = 1  (orthogonal)
    //   [-1,0] -> cosine_distance = 2  (opposite)
    // Store the query vector [1,0] in a column alongside each data vector.
    let results = engine
        .execute_sql(
            &session,
            "CREATE TABLE vecs (id INT, v VECTOR(2), query VECTOR(2)); \
             INSERT INTO vecs VALUES \
                 (1, '[1.0,0.0]', '[1.0,0.0]'), \
                 (2, '[1.0,1.0]', '[1.0,0.0]'), \
                 (3, '[0.0,1.0]', '[1.0,0.0]'), \
                 (4, '[-1.0,0.0]', '[1.0,0.0]'); \
             SELECT id, cosine_distance(v, query) AS dist \
             FROM vecs \
             ORDER BY dist",
        )
        .expect("execute");

    match results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 4);
            // Verify ordering: id=1 (0), id=2 (~0.29), id=3 (1.0), id=4 (2.0)
            assert_eq!(rows[0].values[0], aiondb_core::Value::Int(1));
            assert_eq!(rows[1].values[0], aiondb_core::Value::Int(2));
            assert_eq!(rows[2].values[0], aiondb_core::Value::Int(3));
            assert_eq!(rows[3].values[0], aiondb_core::Value::Int(4));
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

// =====================================================================
// Self-distance identity checks
// =====================================================================

#[test]
fn l2_self_distance_is_zero() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "CREATE TABLE vecs (id INT, v VECTOR(4)); \
             INSERT INTO vecs VALUES (1, '[3.0,4.0,5.0,6.0]'); \
             SELECT l2_distance(v, v) AS dist FROM vecs",
        )
        .expect("execute");

    match &results[2] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(
                rows[0].values[0],
                aiondb_core::Value::Double(0.0),
                "l2 self-distance should be exactly 0.0"
            );
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn cosine_self_distance_is_zero() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "CREATE TABLE vecs (id INT, v VECTOR(4)); \
             INSERT INTO vecs VALUES (1, '[3.0,4.0,5.0,6.0]'); \
             SELECT cosine_distance(v, v) AS dist FROM vecs",
        )
        .expect("execute");

    match &results[2] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            let dist = match &rows[0].values[0] {
                aiondb_core::Value::Double(d) => *d,
                other => panic!("expected Double, got {other:?}"),
            };
            assert!(
                dist.abs() < 1e-10,
                "cosine self-distance should be ~0.0, got {dist}"
            );
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

// =====================================================================
// Drop HNSW index and verify seq scan still works
// =====================================================================

#[test]
fn drop_hnsw_index_then_seq_scan() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE drop_idx_t (id INT, v VECTOR(3), q VECTOR(3)); \
             CREATE INDEX idx_drop ON drop_idx_t USING hnsw (v); \
             INSERT INTO drop_idx_t VALUES \
                 (1, '[1.0,0.0,0.0]', '[1.0,0.0,0.0]'), \
                 (2, '[0.0,1.0,0.0]', '[1.0,0.0,0.0]'), \
                 (3, '[0.0,0.0,1.0]', '[1.0,0.0,0.0]')",
        )
        .expect("setup");

    // Drop the index
    engine
        .execute_sql(&session, "DROP INDEX idx_drop")
        .expect("drop index");

    // Verify sequential scan still works after index is dropped
    let results = engine
        .execute_sql(
            &session,
            "SELECT id, l2_distance(v, q) AS dist FROM drop_idx_t ORDER BY dist LIMIT 2",
        )
        .expect("seq scan after drop");

    match results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 2);
            // id=1 should be closest (distance 0)
            assert_eq!(rows[0].values[0], aiondb_core::Value::Int(1));
            let dist0 = match &rows[0].values[1] {
                aiondb_core::Value::Double(d) => *d,
                other => panic!("expected Double, got {other:?}"),
            };
            assert!(dist0.abs() < 1e-10);
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

// =====================================================================
// Multiple vector inserts and nearest neighbor correctness
// =====================================================================

#[test]
fn vector_nn_correctness_ten_vectors() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    // Insert 10 vectors; query for top 3 nearest to [0,0,0]
    engine
        .execute_sql(
            &session,
            "CREATE TABLE vecs10 (id INT, v VECTOR(2), q VECTOR(2)); \
             INSERT INTO vecs10 VALUES \
                 (1, '[0.1,0.1]', '[0.0,0.0]'), \
                 (2, '[1.0,1.0]', '[0.0,0.0]'), \
                 (3, '[0.2,0.0]', '[0.0,0.0]'), \
                 (4, '[5.0,5.0]', '[0.0,0.0]'), \
                 (5, '[0.0,0.3]', '[0.0,0.0]'), \
                 (6, '[3.0,3.0]', '[0.0,0.0]'), \
                 (7, '[0.5,0.5]', '[0.0,0.0]'), \
                 (8, '[10.0,10.0]', '[0.0,0.0]'), \
                 (9, '[2.0,2.0]', '[0.0,0.0]'), \
                 (10, '[0.0,0.0]', '[0.0,0.0]')",
        )
        .expect("setup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT id, l2_distance(v, q) AS dist FROM vecs10 ORDER BY dist LIMIT 3",
        )
        .expect("nn search");

    match results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 3);
            // id=10 ([0,0]) should be closest (dist 0)
            assert_eq!(rows[0].values[0], aiondb_core::Value::Int(10));
            // Next should be id=1 ([0.1,0.1], dist ~0.141) or id=3 ([0.2,0], dist 0.2)
            let ids: Vec<i32> = rows
                .iter()
                .map(|r| match &r.values[0] {
                    aiondb_core::Value::Int(i) => *i,
                    other => panic!("expected Int, got {other:?}"),
                })
                .collect();
            assert!(ids.contains(&10));
            assert!(ids.contains(&1));
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

// =====================================================================
// Vector column with NULL values alongside non-null
// =====================================================================

#[test]
fn vector_null_values_in_column() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE vnull (id INT, v VECTOR(2)); \
             INSERT INTO vnull VALUES (1, '[1.0,0.0]'), (2, NULL), (3, '[0.0,1.0]')",
        )
        .expect("setup");

    // Select all rows including NULL
    let results = engine
        .execute_sql(&session, "SELECT id, v FROM vnull ORDER BY id")
        .expect("select");

    match results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 3);
            assert_ne!(rows[0].values[1], aiondb_core::Value::Null); // id=1
            assert_eq!(rows[1].values[1], aiondb_core::Value::Null); // id=2
            assert_ne!(rows[2].values[1], aiondb_core::Value::Null); // id=3
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

// =====================================================================
// HNSW index with ef_construction parameter and search
// =====================================================================

#[test]
fn hnsw_ef_construction_and_search() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE hnsw_ef (id INT, v VECTOR(3), q VECTOR(3)); \
             CREATE INDEX idx_ef ON hnsw_ef USING hnsw (v) WITH (ef_construction = 200); \
             INSERT INTO hnsw_ef VALUES \
                 (1, '[1.0,0.0,0.0]', '[1.0,0.0,0.0]'), \
                 (2, '[0.0,1.0,0.0]', '[1.0,0.0,0.0]'), \
                 (3, '[10.0,10.0,10.0]', '[1.0,0.0,0.0]')",
        )
        .expect("setup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT id FROM hnsw_ef ORDER BY l2_distance(v, q) LIMIT 1",
        )
        .expect("search");

    match results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], aiondb_core::Value::Int(1));
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

// =====================================================================
// Vector with various column types alongside
// =====================================================================

#[test]
fn vector_alongside_other_column_types() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    // Keep a query vector in a column here to cover the mixed-schema case,
    // even though string vector literals are now contextualized correctly.
    let results = engine
        .execute_sql(
            &session,
            "CREATE TABLE items (\
                 id INT, name TEXT, embedding VECTOR(2), query VECTOR(2)\
             ); \
             INSERT INTO items VALUES (1, 'widget', '[1.0,0.0]', '[1.0,0.0]'); \
             INSERT INTO items VALUES (2, 'gadget', '[0.0,1.0]', '[1.0,0.0]'); \
             SELECT name, l2_distance(embedding, query) AS dist \
             FROM items \
             ORDER BY dist",
        )
        .expect("execute");

    match results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 2);
            // 'widget' should be closest (distance 0)
            assert_eq!(
                rows[0].values[0],
                aiondb_core::Value::Text("widget".to_owned())
            );
            let dist0 = match &rows[0].values[1] {
                aiondb_core::Value::Double(d) => *d,
                other => panic!("expected Double, got {other:?}"),
            };
            assert!(dist0.abs() < 1e-10);
        }
        other => panic!("expected Query, got {other:?}"),
    }
}
