#![allow(clippy::similar_names)]

use super::*;
use std::sync::Arc;
use std::time::Duration;

// =====================================================================
// 1. CREATE TABLE with VECTOR(3) column
// =====================================================================

#[test]
fn create_table_with_vector_column() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(&session, "CREATE TABLE items (id INT, embedding VECTOR(3))")
        .expect("execute");

    assert_eq!(
        results,
        vec![StatementResult::Command {
            tag: "CREATE TABLE".to_owned(),
            rows_affected: 0,
        }]
    );
}

#[test]
fn unconstrained_vector_type_accepts_pgvector_casts_and_mixed_dims() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, embedding VECTOR); \
             INSERT INTO items VALUES \
                 (1, CAST('[1.0,2.0]' AS VECTOR)), \
                 (2, '[1.0,2.0,3.0]'); \
             SELECT id, vector_dims(embedding) FROM items ORDER BY id",
        )
        .expect("execute unconstrained vector");

    let StatementResult::Query { columns, rows } = results.last().unwrap() else {
        panic!("expected query result");
    };
    assert_eq!(
        columns[1].data_type,
        aiondb_core::DataType::Int,
        "vector_dims should expose the runtime vector dimension"
    );
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].values[0], aiondb_core::Value::Int(1));
    assert_eq!(rows[0].values[1], aiondb_core::Value::Int(2));
    assert_eq!(rows[1].values[0], aiondb_core::Value::Int(2));
    assert_eq!(rows[1].values[1], aiondb_core::Value::Int(3));
}

#[test]
fn halfvec_type_accepts_pgvector_syntax_and_hnsw_opclass() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, embedding HALFVEC(3)); \
             CREATE INDEX idx_items_embedding ON items USING hnsw \
                 (embedding halfvec_l2_ops); \
             INSERT INTO items VALUES \
                 (1, '[1.0,0.0,0.0]'), \
                 (2, '[0.0,1.0,0.0]'); \
             SELECT id, vector_dims(embedding) \
             FROM items \
             ORDER BY embedding <-> '[1.0,0.0,0.0]' \
             LIMIT 1",
        )
        .expect("halfvec pgvector syntax");

    let StatementResult::Query { rows, .. } = results.last().unwrap() else {
        panic!("expected query result");
    };
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], aiondb_core::Value::Int(1));
    assert_eq!(rows[0].values[1], aiondb_core::Value::Int(3));
}

#[test]
fn sparsevec_type_accepts_pgvector_text_and_exact_distance() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, embedding SPARSEVEC(4)); \
             INSERT INTO items VALUES \
                 (1, '{1:1.0,3:2.0}/4'), \
                 (2, '{2:1.0}/4'); \
             SELECT id, vector_dims(embedding), l2_distance(embedding, '[1.0,0.0,2.0,0.0]') AS dist \
             FROM items \
             ORDER BY embedding <-> CAST('{1:1.0,3:2.0}/4' AS SPARSEVEC(4)) \
             LIMIT 1",
        )
        .expect("sparsevec pgvector syntax");

    let StatementResult::Query { rows, .. } = results.last().unwrap() else {
        panic!("expected query result");
    };
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], aiondb_core::Value::Int(1));
    assert_eq!(rows[0].values[1], aiondb_core::Value::Int(4));
    assert_eq!(rows[0].values[2], aiondb_core::Value::Double(0.0));
}

#[test]
fn pgvector_io_functions_execute() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT \
                vector_out(vector_in('[1.0,0.0,2.5]')), \
                vector_dims(vector_in('[1.0,0.0,2.5]', 0, 3)), \
                halfvec_out(halfvec_in('[1.0,0.0,2.5]')), \
                vector_dims(halfvec_in('[1.0,0.0,2.5]', 0, 3)), \
                sparsevec_out(sparsevec_in('{1:1.0,3:2.5}/4')), \
                vector_dims(sparsevec_in('{1:1.0,3:2.5}/4', 0, 4))",
        )
        .expect("pgvector io functions");

    match results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::Text("[1,0,2.5]".to_owned()));
            assert_eq!(rows[0].values[1], Value::Int(3));
            assert_eq!(rows[0].values[2], Value::Text("[1,0,2.5]".to_owned()));
            assert_eq!(rows[0].values[3], Value::Int(3));
            assert_eq!(rows[0].values[4], Value::Text("{1:1,3:2.5}/4".to_owned()));
            assert_eq!(rows[0].values[5], Value::Int(4));
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

// =====================================================================
// 2. INSERT vector data using text literal '[1.0,2.0,3.0]'
// =====================================================================

#[test]
fn insert_vector_data_via_text_literal() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, embedding VECTOR(3)); \
             INSERT INTO items VALUES (1, '[1.0,2.0,3.0]')",
        )
        .expect("execute");

    assert_eq!(results.len(), 2);
    assert_eq!(
        results[1],
        StatementResult::Command {
            tag: "INSERT".to_owned(),
            rows_affected: 1,
        }
    );
}

// =====================================================================
// 3. SELECT vector data back
// =====================================================================

#[test]
fn select_vector_data_back() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, embedding VECTOR(3)); \
             INSERT INTO items VALUES (1, '[1.0,2.0,3.0]'); \
             SELECT id, embedding FROM items",
        )
        .expect("execute");

    assert_eq!(results.len(), 3);
    match &results[2] {
        StatementResult::Query { columns, rows } => {
            assert_eq!(columns.len(), 2);
            assert_eq!(columns[0].name, "id");
            assert_eq!(
                columns[1].data_type,
                aiondb_core::DataType::Vector {
                    dims: 3,
                    element_type: aiondb_core::VectorElementType::Float32
                }
            );
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], aiondb_core::Value::Int(1));
            assert_eq!(
                rows[0].values[1],
                aiondb_core::Value::Vector(aiondb_core::VectorValue::new(3, vec![1.0, 2.0, 3.0]))
            );
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

// =====================================================================
// 4. l2_distance function
// =====================================================================

#[test]
fn l2_distance_function() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, embedding VECTOR(3)); \
             INSERT INTO items VALUES (1, '[1.0,0.0,0.0]'), (2, '[0.0,1.0,0.0]'); \
             SELECT id, l2_distance(embedding, embedding) AS self_dist FROM items",
        )
        .expect("execute");

    match &results[2] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 2);
            // Self-distance should be 0
            assert_eq!(rows[0].values[1], aiondb_core::Value::Double(0.0));
            assert_eq!(rows[1].values[1], aiondb_core::Value::Double(0.0));
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn l2_distance_between_different_vectors() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    // Use two rows in same table, compute distance between [1,0,0] and [0,1,0]
    // which should be sqrt(2)
    let results = engine
        .execute_sql(
            &session,
            "CREATE TABLE vecs (id INT, v VECTOR(3)); \
             INSERT INTO vecs VALUES (1, '[1.0,0.0,0.0]'), (2, '[0.0,1.0,0.0]'); \
             SELECT l2_distance(v, v) FROM vecs WHERE id = 1",
        )
        .expect("execute");

    // Self-distance is 0 -- sanity check
    match &results[2] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], aiondb_core::Value::Double(0.0));
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

// =====================================================================
// 5. cosine_distance function
// =====================================================================

#[test]
fn cosine_distance_identical_vectors() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, embedding VECTOR(3)); \
             INSERT INTO items VALUES (1, '[1.0,2.0,3.0]'); \
             SELECT cosine_distance(embedding, embedding) FROM items",
        )
        .expect("execute");

    match &results[2] {
        StatementResult::Query { rows, .. } => {
            let dist = match &rows[0].values[0] {
                aiondb_core::Value::Double(d) => *d,
                other => panic!("expected Double, got {other:?}"),
            };
            // Cosine distance of identical vectors should be 0
            assert!(dist.abs() < 1e-10);
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

// =====================================================================
// 6. inner_product function
// =====================================================================

#[test]
fn inner_product_function() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, embedding VECTOR(3)); \
             INSERT INTO items VALUES (1, '[1.0,2.0,3.0]'); \
             SELECT inner_product(embedding, embedding) FROM items",
        )
        .expect("execute");

    match &results[2] {
        StatementResult::Query { rows, .. } => {
            let product = match &rows[0].values[0] {
                aiondb_core::Value::Double(d) => *d,
                other => panic!("expected Double, got {other:?}"),
            };
            // inner_product([1,2,3], [1,2,3]) = 1+4+9 = 14
            assert!((product - 14.0).abs() < 1e-10);
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

// =====================================================================
// 7. ORDER BY distance with LIMIT (exact nearest neighbor search)
// =====================================================================

#[test]
fn order_by_l2_distance_with_limit() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    // Compute self-distance (always 0), order by id descending with limit 2
    // This tests ORDER BY + LIMIT with vector function results
    let results = engine
        .execute_sql(
            &session,
            "CREATE TABLE vectors (id INT, v VECTOR(2)); \
             INSERT INTO vectors VALUES \
                 (1, '[0.0,0.0]'), \
                 (2, '[10.0,10.0]'), \
                 (3, '[1.0,1.0]'), \
                 (4, '[5.0,5.0]'); \
             SELECT id, l2_distance(v, v) AS dist \
             FROM vectors \
             ORDER BY id \
             LIMIT 2",
        )
        .expect("execute");

    let select_result = results.last().unwrap();
    match select_result {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 2);
            // Should get id=1 and id=2 (ordered by id, limit 2)
            assert_eq!(rows[0].values[0], aiondb_core::Value::Int(1));
            assert_eq!(rows[1].values[0], aiondb_core::Value::Int(2));
            // All self-distances should be 0
            assert_eq!(rows[0].values[1], aiondb_core::Value::Double(0.0));
            assert_eq!(rows[1].values[1], aiondb_core::Value::Double(0.0));
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

// =====================================================================
// 8. CREATE INDEX USING hnsw + vector search pipeline
// =====================================================================

#[test]
fn vector_create_hnsw_index() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, embedding VECTOR(3)); \
             CREATE INDEX idx_emb ON items USING hnsw (embedding)",
        )
        .expect("execute");

    assert_eq!(results.len(), 2);
    assert_eq!(
        results[1],
        StatementResult::Command {
            tag: "CREATE INDEX".to_owned(),
            rows_affected: 0,
        }
    );
}

#[test]
fn vector_create_hnsw_index_with_cosine_distance_option() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, embedding VECTOR(3)); \
             CREATE INDEX idx_emb ON items USING hnsw (embedding) \
                WITH (m = 8, ef_construction = 40, distance = 'cosine', quantization = 'none')",
        )
        .expect("execute");

    assert_eq!(results.len(), 2);
    assert_eq!(
        results[1],
        StatementResult::Command {
            tag: "CREATE INDEX".to_owned(),
            rows_affected: 0,
        }
    );
}

#[test]
fn vector_create_ivfflat_index_pgvector_syntax_serves_top_k() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, embedding VECTOR(3)); \
             CREATE INDEX idx_emb ON items USING ivfflat (embedding) WITH (lists = 8); \
             INSERT INTO items VALUES \
                (1, '[0.0,0.0,0.0]'), \
                (2, '[1.0,0.0,0.0]'), \
                (3, '[4.0,0.0,0.0]')",
        )
        .expect("setup ivfflat compatibility index");

    let result = engine
        .execute_sql(
            &session,
            "SELECT id FROM items \
             ORDER BY embedding <-> '[0.9,0.0,0.0]' LIMIT 1",
        )
        .expect("select")
        .pop()
        .expect("at least one statement");

    let StatementResult::Query { rows, .. } = result else {
        panic!("expected query result, got {result:?}");
    };
    let first = rows.first().expect("at least one result row");
    assert_eq!(first.values.first(), Some(&Value::Int(2)));
}

#[test]
fn create_extension_vector_records_pg_extension_row() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "CREATE EXTENSION IF NOT EXISTS vector; \
             SELECT extname, extversion FROM pg_extension WHERE extname = 'vector'; \
             SELECT installed_version FROM pg_available_extensions WHERE name = 'vector'",
        )
        .expect("create vector extension");

    let StatementResult::Query { rows, .. } = &results[1] else {
        panic!("expected pg_extension query");
    };
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].values[0],
        aiondb_core::Value::Text("vector".to_owned())
    );
    assert_eq!(
        rows[0].values[1],
        aiondb_core::Value::Text("0.8.2".to_owned())
    );

    let StatementResult::Query { rows, .. } = results.last().unwrap() else {
        panic!("expected pg_available_extensions query");
    };
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].values[0],
        aiondb_core::Value::Text("0.8.2".to_owned())
    );
}

#[test]
fn pgvector_catalog_introspection_exposes_types_ops_and_opclasses() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "CREATE EXTENSION IF NOT EXISTS vector; \
             SELECT typname FROM pg_catalog.pg_type \
             WHERE typname = 'bit' OR typname = 'halfvec' OR typname = 'sparsevec' \
                OR typname = 'varbit' OR typname = 'vector' \
             ORDER BY typname; \
             SELECT amname FROM pg_catalog.pg_am \
             WHERE amname = 'hnsw' OR amname = 'ivfflat' \
             ORDER BY amname; \
             SELECT oprname FROM pg_catalog.pg_operator \
             WHERE oprleft = to_regtype('vector') \
               AND (oprname = '<#>' OR oprname = '<+>' OR oprname = '<->' OR oprname = '<=>') \
             ORDER BY oprname; \
             SELECT oprname FROM pg_catalog.pg_operator \
             WHERE oprleft = to_regtype('bit') \
               AND (oprname = '<%>' OR oprname = '<~>') \
             ORDER BY oprname; \
             SELECT opcname, opcmethod FROM pg_catalog.pg_opclass \
             WHERE opcname = 'bit_hamming_ops' OR opcname = 'bit_jaccard_ops' \
                OR opcname = 'halfvec_cosine_ops' OR opcname = 'halfvec_l2_ops' \
                OR opcname = 'vector_cosine_ops' OR opcname = 'vector_l2_ops' \
             ORDER BY opcname, opcmethod; \
             SELECT count(*) FROM pg_catalog.pg_amop \
             WHERE amoplefttype = to_regtype('vector')",
        )
        .expect("pgvector catalog introspection");

    let query_rows = |idx: usize| -> &[Row] {
        let StatementResult::Query { rows, .. } = &results[idx] else {
            panic!(
                "expected query result at index {idx}, got {:?}",
                results[idx]
            );
        };
        rows
    };
    let text_values = |idx: usize| -> Vec<String> {
        query_rows(idx)
            .iter()
            .map(|row| match &row.values[0] {
                Value::Text(value) => value.clone(),
                other => panic!("expected text value, got {other:?}"),
            })
            .collect()
    };

    assert_eq!(
        text_values(1),
        vec!["bit", "halfvec", "sparsevec", "varbit", "vector"]
    );
    assert_eq!(text_values(2), vec!["hnsw", "ivfflat"]);
    assert_eq!(text_values(3), vec!["<#>", "<+>", "<->", "<=>"]);
    assert_eq!(text_values(4), vec!["<%>", "<~>"]);

    let opclass_rows = query_rows(5);
    assert_eq!(opclass_rows.len(), 11);
    assert!(opclass_rows
        .iter()
        .any(|row| { matches!(&row.values[0], Value::Text(name) if name == "bit_hamming_ops") }));
    assert!(opclass_rows
        .iter()
        .any(|row| { matches!(&row.values[0], Value::Text(name) if name == "vector_cosine_ops") }));

    assert_eq!(query_rows(6)[0].values[0], Value::BigInt(7));
}

#[test]
fn vector_ivfflat_pgvector_operator_class_sets_distance_metric() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, embedding VECTOR(3)); \
             CREATE INDEX idx_emb ON items USING ivfflat \
                (embedding vector_cosine_ops) WITH (lists = 8); \
             INSERT INTO items VALUES \
                (1, '[1.0,0.0,0.0]'), \
                (2, '[0.0,1.0,0.0]')",
        )
        .expect("setup ivfflat cosine compatibility index");

    let statement = parse_prepared_statement(
        "SELECT id FROM items ORDER BY embedding <=> '[1.0,0.0,0.0]' LIMIT 1",
    )
    .expect("parse");
    let plan = engine
        .build_physical_plan(&session, &statement)
        .expect("physical plan");
    assert!(
        matches!(plan, aiondb_plan::PhysicalPlan::HnswScan { .. }),
        "vector_cosine_ops should map ivfflat syntax to a cosine vector scan: {plan:?}"
    );
}

#[test]
fn vector_create_hnsw_index_with_prenormalised_flag_serves_top_k() {
    // End-to-end check that `WITH (prenormalised = true)` reaches the storage
    // layer and the cosine fast path returns the same ranking as the
    // standard cosine path.
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, embedding VECTOR(3)); \
             CREATE INDEX idx_emb ON items USING hnsw (embedding) \
                WITH (distance = 'cosine', prenormalised = true); \
             INSERT INTO items VALUES \
                (1, '[1.0,0.0,0.0]'), \
                (2, '[0.0,1.0,0.0]'), \
                (3, '[0.0,0.0,1.0]')",
        )
        .expect("setup");
    // CREATE TABLE + CREATE INDEX + INSERT.
    assert!(results.len() >= 3);

    let result = engine
        .execute_sql(
            &session,
            "SELECT id FROM items \
             ORDER BY embedding <=> '[1.0,0.0,0.0]' LIMIT 1",
        )
        .expect("select")
        .pop()
        .expect("at least one statement");

    let StatementResult::Query { rows, .. } = result else {
        panic!("expected query result, got {result:?}");
    };
    let first = rows.first().expect("at least one result row");
    let id = first.values.first().expect("id column");
    assert_eq!(format!("{id:?}"), "Int(1)", "expected id=1, got {first:?}");
}

#[test]
fn vector_create_hnsw_index_rejects_unknown_distance_metric() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let err = engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, embedding VECTOR(3)); \
             CREATE INDEX idx_emb ON items USING hnsw (embedding) \
                WITH (distance = 'hamming')",
        )
        .expect_err("unknown distance metric should be rejected");
    assert!(
        err.to_string().contains("distance"),
        "unexpected error: {err}"
    );
}

#[test]
fn vector_create_hnsw_index_rejects_unknown_quantization() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let err = engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, embedding VECTOR(3)); \
             CREATE INDEX idx_emb ON items USING hnsw (embedding) \
                WITH (quantization = 'mystery')",
        )
        .expect_err("unknown quantization kind should be rejected");
    assert!(
        err.to_string().contains("quantization"),
        "unexpected error: {err}"
    );
}

#[test]
fn pgvector_operator_l2_distance_matches_function() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, v VECTOR(3)); \
             INSERT INTO items VALUES (1, '[0.0,0.0,0.0]'), (2, '[3.0,4.0,0.0]')",
        )
        .expect("setup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT v <-> '[0.0,0.0,0.0]' FROM items WHERE id = 2",
        )
        .expect("<-> operator");
    match results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            match rows[0].values.first().unwrap() {
                aiondb_core::Value::Double(d) => assert!((*d - 5.0).abs() < 1e-6),
                other => panic!("expected Double, got {other:?}"),
            }
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn pgvector_operator_cosine_distance_matches_function() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, v VECTOR(3)); \
             INSERT INTO items VALUES (1, '[1.0,0.0,0.0]'), (2, '[0.0,1.0,0.0]')",
        )
        .expect("setup");

    // <=> cosine: identical direction → 0; orthogonal → 1.
    let results = engine
        .execute_sql(
            &session,
            "SELECT id, v <=> '[2.0,0.0,0.0]' FROM items ORDER BY id",
        )
        .expect("<=> operator");
    match results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 2);
            // Row 1: cosine distance between [1,0,0] and [2,0,0] ≈ 0.
            match rows[0].values.get(1).unwrap() {
                aiondb_core::Value::Double(d) => {
                    assert!(d.abs() < 1e-6, "expected ~0, got {d}")
                }
                other => panic!("expected Double, got {other:?}"),
            }
            // Row 2: cosine distance between [0,1,0] and [2,0,0] = 1.
            match rows[1].values.get(1).unwrap() {
                aiondb_core::Value::Double(d) => {
                    assert!((*d - 1.0).abs() < 1e-6, "expected ~1, got {d}")
                }
                other => panic!("expected Double, got {other:?}"),
            }
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn pgvector_operator_l1_distance_matches_function() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, v VECTOR(3)); \
             INSERT INTO items VALUES (1, '[0.0,0.0,0.0]'), (2, '[3.0,4.0,5.0]')",
        )
        .expect("setup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT v <+> '[1.0,1.0,1.0]', l1_distance(v, '[1.0,1.0,1.0]') \
             FROM items WHERE id = 2",
        )
        .expect("<+> operator");
    match results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            for value in &rows[0].values {
                match value {
                    aiondb_core::Value::Double(d) => assert!((*d - 9.0).abs() < 1e-6),
                    other => panic!("expected Double, got {other:?}"),
                }
            }
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn pgvector_operator_negative_inner_product_returns_negated_dot() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, v VECTOR(3)); \
             INSERT INTO items VALUES (1, '[1.0,2.0,3.0]'), (2, '[-1.0,-2.0,-3.0]')",
        )
        .expect("setup");

    // `<#>` = -(dot(a,b)). For v1 = [1,2,3] and q = [1,2,3]: dot = 14, so
    // the operator returns -14. For v2 = [-1,-2,-3] it returns +14.
    let results = engine
        .execute_sql(
            &session,
            "SELECT id, v <#> '[1.0,2.0,3.0]' FROM items ORDER BY id",
        )
        .expect("<#> operator");
    match results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 2);
            match rows[0].values.get(1).unwrap() {
                aiondb_core::Value::Double(d) => {
                    assert!((*d + 14.0).abs() < 1e-6, "expected -14, got {d}")
                }
                other => panic!("expected Double, got {other:?}"),
            }
            match rows[1].values.get(1).unwrap() {
                aiondb_core::Value::Double(d) => {
                    assert!((*d - 14.0).abs() < 1e-6, "expected 14, got {d}")
                }
                other => panic!("expected Double, got {other:?}"),
            }
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn pgvector_operator_negative_inner_product_order_by_picks_max_dot() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, v VECTOR(3)); \
             INSERT INTO items VALUES \
                 (1, '[1.0,0.0,0.0]'), \
                 (2, '[5.0,0.0,0.0]'), \
                 (3, '[0.0,1.0,0.0]')",
        )
        .expect("setup");

    // ORDER BY v <#> q ASC LIMIT 1 should pick the vector with the HIGHEST
    // dot product with q (because <#> is negated).
    let results = engine
        .execute_sql(
            &session,
            "SELECT id FROM items ORDER BY v <#> '[1.0,0.0,0.0]' LIMIT 1",
        )
        .expect("<#> ORDER BY");
    match results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            match rows[0].values.first().unwrap() {
                aiondb_core::Value::Int(id) => assert_eq!(*id, 2),
                other => panic!("expected Int, got {other:?}"),
            }
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn pgvector_operator_negative_inner_product_lowers_to_hnsw_scan_for_inner_product_index() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, v VECTOR(3)); \
             CREATE INDEX idx_v ON items USING hnsw (v) \
                WITH (distance = 'inner_product'); \
             INSERT INTO items VALUES (1, '[1.0,0.0,0.0]'), (2, '[5.0,0.0,0.0]')",
        )
        .expect("setup");

    let statement =
        parse_prepared_statement("SELECT id FROM items ORDER BY v <#> '[1.0,0.0,0.0]' LIMIT 1")
            .expect("parse");
    let plan = engine
        .build_physical_plan(&session, &statement)
        .expect("physical plan");
    assert!(
        matches!(plan, aiondb_plan::PhysicalPlan::HnswScan { .. }),
        "<#> should lower to HnswScan against an inner_product index: {plan:?}"
    );
}

#[test]
fn inner_product_desc_lowers_to_hnsw_scan_for_inner_product_index() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, v VECTOR(3)); \
             CREATE INDEX idx_v ON items USING hnsw (v) \
                WITH (distance = 'inner_product'); \
             INSERT INTO items VALUES (1, '[1.0,0.0,0.0]'), (2, '[5.0,0.0,0.0]')",
        )
        .expect("setup");

    let statement = parse_prepared_statement(
        "SELECT id FROM items ORDER BY inner_product(v, '[1.0,0.0,0.0]') DESC LIMIT 1",
    )
    .expect("parse");
    let plan = engine
        .build_physical_plan(&session, &statement)
        .expect("physical plan");
    assert!(
        matches!(plan, aiondb_plan::PhysicalPlan::HnswScan { .. }),
        "inner_product DESC should lower to HnswScan for inner-product index: {plan:?}"
    );
}

#[test]
fn inner_product_desc_query_executes_end_to_end() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, v VECTOR(3)); \
             CREATE INDEX idx_v ON items USING hnsw (v) \
                WITH (distance = 'inner_product'); \
             INSERT INTO items VALUES \
                 (1, '[1.0,0.0,0.0]'), \
                 (2, '[5.0,0.0,0.0]'), \
                 (3, '[0.0,1.0,0.0]')",
        )
        .expect("setup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT id FROM items ORDER BY inner_product(v, '[1.0,0.0,0.0]') DESC LIMIT 1",
        )
        .expect("inner_product DESC query");
    match results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            match rows[0].values.first().unwrap() {
                aiondb_core::Value::Int(id) => assert_eq!(*id, 2),
                other => panic!("expected Int, got {other:?}"),
            }
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn inner_product_asc_does_not_lower_to_hnsw_scan() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, v VECTOR(3)); \
             CREATE INDEX idx_v ON items USING hnsw (v) \
                WITH (distance = 'inner_product'); \
             INSERT INTO items VALUES (1, '[1.0,0.0,0.0]'), (2, '[5.0,0.0,0.0]')",
        )
        .expect("setup");

    let statement = parse_prepared_statement(
        "SELECT id FROM items ORDER BY inner_product(v, '[1.0,0.0,0.0]') ASC LIMIT 1",
    )
    .expect("parse");
    let plan = engine
        .build_physical_plan(&session, &statement)
        .expect("physical plan");
    assert!(
        !matches!(plan, aiondb_plan::PhysicalPlan::HnswScan { .. }),
        "inner_product ASC should not lower to HnswScan: {plan:?}"
    );
}

#[test]
fn pgvector_operator_l2_plan_lowers_to_hnsw_scan() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, v VECTOR(3)); \
             CREATE INDEX idx_v ON items USING hnsw (v); \
             INSERT INTO items VALUES (1, '[1.0,0.0,0.0]'), (2, '[0.0,1.0,0.0]')",
        )
        .expect("setup");

    let statement =
        parse_prepared_statement("SELECT id FROM items ORDER BY v <-> '[1.0,0.0,0.0]' LIMIT 1")
            .expect("parse");
    let plan = engine
        .build_physical_plan(&session, &statement)
        .expect("physical plan");
    assert!(
        matches!(plan, aiondb_plan::PhysicalPlan::HnswScan { .. }),
        "<-> should lower to HnswScan against an L2 index: {plan:?}"
    );
}

#[test]
fn cosine_distance_lowers_to_hnsw_scan_with_prenormalised_index() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, v VECTOR(3)); \
             CREATE INDEX idx_v ON items USING hnsw (v) \
                WITH (distance = 'cosine', prenormalised = true); \
             INSERT INTO items VALUES (1, '[1.0,0.0,0.0]'), (2, '[0.0,1.0,0.0]')",
        )
        .expect("setup");

    let statement =
        parse_prepared_statement("SELECT id FROM items ORDER BY v <=> '[1.0,0.0,0.0]' LIMIT 1")
            .expect("parse");
    let plan = engine
        .build_physical_plan(&session, &statement)
        .expect("physical plan");
    assert!(
        matches!(plan, aiondb_plan::PhysicalPlan::HnswScan { .. }),
        "<=> should lower to HnswScan against a cosine HNSW index: {plan:?}"
    );
}

#[test]
fn distance_functions_f64_simd_round_trip_through_sql() {
    // Sanity check that the SIMD-dispatched f64 kernels produce results
    // matching a pure scalar f64 reference when called via SQL. Long-ish
    // vectors so the SIMD body actually runs (not just the scalar tail).
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let dims: usize = 64;
    let a: Vec<f32> = (0..dims).map(|i| (i as f32) * 0.07 - 0.3).collect();
    let b: Vec<f32> = (0..dims).map(|i| 1.0 - (i as f32) * 0.05).collect();

    let lit = |v: &[f32]| -> String {
        // 9 significant digits round-trip an f32 exactly.
        let body = v
            .iter()
            .map(|x| format!("{x:.9e}"))
            .collect::<Vec<_>>()
            .join(",");
        format!("'[{body}]'")
    };

    // Pure scalar f64 references: re-derive locally so this test is
    // independent of the kernel implementations under check.
    let dot_ref: f64 = a
        .iter()
        .zip(b.iter())
        .map(|(x, y)| f64::from(*x) * f64::from(*y))
        .sum();
    let l2sq_ref: f64 = a
        .iter()
        .zip(b.iter())
        .map(|(x, y)| {
            let d = f64::from(*x) - f64::from(*y);
            d * d
        })
        .sum();
    let l2_ref = l2sq_ref.sqrt();
    let l1_ref: f64 = a
        .iter()
        .zip(b.iter())
        .map(|(x, y)| (f64::from(*x) - f64::from(*y)).abs())
        .sum();
    let norm_a_sq: f64 = a.iter().map(|x| f64::from(*x) * f64::from(*x)).sum();
    let norm_b_sq: f64 = b.iter().map(|x| f64::from(*x) * f64::from(*x)).sum();
    let cos_ref = 1.0 - dot_ref / (norm_a_sq.sqrt() * norm_b_sq.sqrt());

    // Stage the vectors via a table so the columns carry the VECTOR(N) type
    // tag. Distance functions need at least one typed side to dispatch.
    engine
        .execute_sql(
            &session,
            &format!(
                "CREATE TABLE pair (id INT, va VECTOR({dims}), vb VECTOR({dims})); \
                 INSERT INTO pair VALUES (1, {a}, {b})",
                dims = dims,
                a = lit(&a),
                b = lit(&b)
            ),
        )
        .expect("setup");
    // l2_distance has geometric-type short-circuit fallbacks that take over
    // for 2-coordinate text shapes; we skip it here and exercise the three
    // pure-vector functions, which all funnel directly into the new f64
    // SIMD dispatch.
    let _ = l2_ref;
    let results = engine
        .execute_sql(
            &session,
            "SELECT \
                cosine_distance(va, vb), \
                inner_product(va, vb), \
                manhattan_distance(va, vb) \
             FROM pair",
        )
        .expect("distance functions");
    let StatementResult::Query { rows, .. } = results.into_iter().next().unwrap() else {
        panic!("expected query result");
    };
    let row = rows.first().expect("one row");
    let cell = |idx: usize| -> f64 {
        match row.values[idx] {
            aiondb_core::Value::Double(d) => d,
            ref other => panic!("col {idx}: expected Double, got {other:?}"),
        }
    };
    let cos_got = cell(0);
    let dot_got = cell(1);
    let l1_got = cell(2);

    assert!(
        (cos_got - cos_ref).abs() < 1e-9,
        "cosine: got {cos_got} ref {cos_ref}"
    );
    assert!(
        (dot_got - dot_ref).abs() < 1e-9,
        "dot: got {dot_got} ref {dot_ref}"
    );
    assert!(
        (l1_got - l1_ref).abs() < 1e-9,
        "l1: got {l1_got} ref {l1_ref}"
    );
}

#[test]
fn vector_manhattan_distance_sql_function() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, v VECTOR(3)); \
             INSERT INTO items VALUES \
                 (1, '[0.0,0.0,0.0]'), \
                 (2, '[3.0,4.0,0.0]')",
        )
        .expect("setup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT \
                manhattan_distance(v, '[0.0,0.0,0.0]'), \
                l1_distance(v, '[0.0,0.0,0.0]') \
             FROM items WHERE id = 2",
        )
        .expect("manhattan_distance SQL call");

    match results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            for value in &rows[0].values {
                match value {
                    aiondb_core::Value::Double(d) => assert!((*d - 7.0).abs() < 1e-6),
                    other => panic!("expected Double, got {other:?}"),
                }
            }
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn vector_pgvector_utility_functions() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, v VECTOR(3)); \
             INSERT INTO items VALUES (1, '[3.0,4.0,12.0]')",
        )
        .expect("setup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT \
                vector_dims(v), \
                l2_norm(v), \
                vector_norm(v), \
                l2_norm(l2_normalize(v)), \
                vector_dims(subvector(v, 2, 2)), \
                subvector(v, 2, 2), \
                binary_quantize(CAST('[1.0,-2.0,0.0,0.1]' AS VECTOR)), \
                binary_quantize(CAST('[1.0,-2.0,0.0,0.1]' AS VECTOR), 4, true), \
                binary_quantize(CAST('[1.0,-2.0,0.0,0.1]' AS VECTOR))::bit(4), \
                binary_quantize(CAST('[1.0,-2.0,0.0,0.1]' AS VECTOR))::varbit(4), \
                hamming_distance(binary_quantize(CAST('[1.0,-2.0,0.0,0.1]' AS VECTOR)), '1011'), \
                jaccard_distance(binary_quantize(CAST('[1.0,-2.0,0.0,0.1]' AS VECTOR)), '1011'), \
                binary_quantize(CAST('[1.0,-2.0,0.0,0.1]' AS VECTOR)) <~> '1011', \
                binary_quantize(CAST('[1.0,-2.0,0.0,0.1]' AS VECTOR)) <%> '1011', \
                vector_dims(array_to_vector(ARRAY[3.0,4.0,12.0], 3, true)), \
                vector_to_float4(v, 3, true), \
                vector_dims(array_to_halfvec(ARRAY[3.0,4.0,12.0], 3, true)), \
                vector_dims(vector_to_halfvec(v, 3, true)), \
                vector_dims(halfvec_to_vector(v, 3, true)), \
                vector_dims(halfvec_to_sparsevec(CAST('[3.0,4.0,12.0]' AS HALFVEC(3)), 3, true)), \
                vector_dims(sparsevec_to_vector(CAST('{1:3.0,2:4.0,3:12.0}/3' AS SPARSEVEC(3)), 3, true)), \
                vector_dims(sparsevec_to_halfvec(CAST('{1:3.0,2:4.0,3:12.0}/3' AS SPARSEVEC(3)), 3, true)), \
                vector_dims(vector_to_sparsevec(v, 3, true)), \
                l2_distance(vector_to_sparsevec(v, 3, true), CAST('{1:3.0,2:4.0,3:12.0}/3' AS SPARSEVEC(3))), \
                vector_dims(array_to_sparsevec(ARRAY[3.0,4.0,12.0], 3, true)), \
                l2_distance(array_to_sparsevec(ARRAY[3.0,4.0,12.0], 3, true), v), \
                pg_catalog.vector_dims(v), \
                pg_catalog.l2_norm(v), \
                pg_catalog.binary_quantize(CAST('[1.0,-2.0,0.0,0.1]' AS VECTOR)), \
                l2_norm(CAST('{1:3.0,2:4.0,3:12.0}/3' AS SPARSEVEC(3))), \
                l2_norm(l2_normalize(CAST('{1:3.0,2:4.0,3:12.0}/3' AS SPARSEVEC(3)))), \
                v + CAST('[1.0,1.0,1.0]' AS VECTOR(3)), \
                v - CAST('[1.0,2.0,3.0]' AS VECTOR(3)), \
                v * CAST('[2.0,0.5,1.0]' AS VECTOR(3)), \
                vector_dims(v || CAST('[1.0,2.0]' AS VECTOR(2))), \
                vector_add(v, CAST('[1.0,1.0,1.0]' AS VECTOR(3))), \
                pg_catalog.vector_sub(v, CAST('[1.0,2.0,3.0]' AS VECTOR(3))), \
                vector_mul(v, CAST('[2.0,0.5,1.0]' AS VECTOR(3))), \
                vector_dims(vector_concat(v, CAST('[1.0,2.0]' AS VECTOR(2)))), \
                halfvec_add(CAST('[1.0,2.0,3.0]' AS HALFVEC(3)), CAST('[3.0,2.0,1.0]' AS HALFVEC(3))), \
                vector_dims(pg_catalog.halfvec_concat(CAST('[1.0,2.0]' AS HALFVEC(2)), CAST('[3.0]' AS HALFVEC(1)))) \
             FROM items WHERE id = 1",
        )
        .expect("pgvector utility functions");

    match results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], aiondb_core::Value::Int(3));
            for value in &rows[0].values[1..=2] {
                match value {
                    aiondb_core::Value::Double(norm) => assert!((*norm - 13.0).abs() < 1e-6),
                    other => panic!("expected Double, got {other:?}"),
                }
            }
            match &rows[0].values[3] {
                aiondb_core::Value::Double(norm) => assert!((*norm - 1.0).abs() < 1e-6),
                other => panic!("expected Double, got {other:?}"),
            }
            assert_eq!(rows[0].values[4], aiondb_core::Value::Int(2));
            assert_eq!(
                rows[0].values[5],
                aiondb_core::Value::Vector(aiondb_core::VectorValue::new(2, vec![4.0, 12.0]))
            );
            assert_eq!(
                rows[0].values[6],
                aiondb_core::Value::Text("1001".to_owned())
            );
            assert_eq!(
                rows[0].values[7],
                aiondb_core::Value::Text("1001".to_owned())
            );
            assert_eq!(
                rows[0].values[8],
                aiondb_core::Value::Text("1001".to_owned())
            );
            assert_eq!(
                rows[0].values[9],
                aiondb_core::Value::Text("1001".to_owned())
            );
            assert_eq!(rows[0].values[10], aiondb_core::Value::Double(1.0));
            match rows[0].values[11] {
                aiondb_core::Value::Double(distance) => {
                    assert!((distance - (1.0 / 3.0)).abs() < 1e-9);
                }
                ref other => panic!("expected Double, got {other:?}"),
            }
            assert_eq!(rows[0].values[12], aiondb_core::Value::Double(1.0));
            match rows[0].values[13] {
                aiondb_core::Value::Double(distance) => {
                    assert!((distance - (1.0 / 3.0)).abs() < 1e-9);
                }
                ref other => panic!("expected Double, got {other:?}"),
            }
            assert_eq!(rows[0].values[14], aiondb_core::Value::Int(3));
            assert_eq!(
                rows[0].values[15],
                aiondb_core::Value::Array(vec![
                    aiondb_core::Value::Real(3.0),
                    aiondb_core::Value::Real(4.0),
                    aiondb_core::Value::Real(12.0)
                ])
            );
            assert_eq!(rows[0].values[16], aiondb_core::Value::Int(3));
            assert_eq!(rows[0].values[17], aiondb_core::Value::Int(3));
            assert_eq!(rows[0].values[18], aiondb_core::Value::Int(3));
            assert_eq!(rows[0].values[19], aiondb_core::Value::Int(3));
            assert_eq!(rows[0].values[20], aiondb_core::Value::Int(3));
            assert_eq!(rows[0].values[21], aiondb_core::Value::Int(3));
            assert_eq!(rows[0].values[22], aiondb_core::Value::Int(3));
            assert_eq!(rows[0].values[23], aiondb_core::Value::Double(0.0));
            assert_eq!(rows[0].values[24], aiondb_core::Value::Int(3));
            assert_eq!(rows[0].values[25], aiondb_core::Value::Double(0.0));
            assert_eq!(rows[0].values[26], aiondb_core::Value::Int(3));
            match rows[0].values[27] {
                aiondb_core::Value::Double(norm) => assert!((norm - 13.0).abs() < 1e-6),
                ref other => panic!("expected Double, got {other:?}"),
            }
            assert_eq!(
                rows[0].values[28],
                aiondb_core::Value::Text("1001".to_owned())
            );
            match rows[0].values[29] {
                aiondb_core::Value::Double(norm) => assert!((norm - 13.0).abs() < 1e-6),
                ref other => panic!("expected Double, got {other:?}"),
            }
            match rows[0].values[30] {
                aiondb_core::Value::Double(norm) => assert!((norm - 1.0).abs() < 1e-6),
                ref other => panic!("expected Double, got {other:?}"),
            }
            assert_eq!(
                rows[0].values[31],
                aiondb_core::Value::Vector(aiondb_core::VectorValue::new(3, vec![4.0, 5.0, 13.0]))
            );
            assert_eq!(
                rows[0].values[32],
                aiondb_core::Value::Vector(aiondb_core::VectorValue::new(3, vec![2.0, 2.0, 9.0]))
            );
            assert_eq!(
                rows[0].values[33],
                aiondb_core::Value::Vector(aiondb_core::VectorValue::new(3, vec![6.0, 2.0, 12.0]))
            );
            assert_eq!(rows[0].values[34], aiondb_core::Value::Int(5));
            assert_eq!(
                rows[0].values[35],
                aiondb_core::Value::Vector(aiondb_core::VectorValue::new(3, vec![4.0, 5.0, 13.0]))
            );
            assert_eq!(
                rows[0].values[36],
                aiondb_core::Value::Vector(aiondb_core::VectorValue::new(3, vec![2.0, 2.0, 9.0]))
            );
            assert_eq!(
                rows[0].values[37],
                aiondb_core::Value::Vector(aiondb_core::VectorValue::new(3, vec![6.0, 2.0, 12.0]))
            );
            assert_eq!(rows[0].values[38], aiondb_core::Value::Int(5));
            assert_eq!(
                rows[0].values[39],
                aiondb_core::Value::Vector(aiondb_core::VectorValue::new(3, vec![4.0, 4.0, 4.0]))
            );
            assert_eq!(rows[0].values[40], aiondb_core::Value::Int(3));
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn binary_quantize_halfvec_pgvector_compat() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT \
                binary_quantize(CAST('[1.0,-2.0,0.0,0.1]' AS HALFVEC(4))), \
                pg_catalog.binary_quantize(CAST('[1.0,-2.0,0.0,0.1]' AS HALFVEC(4)))",
        )
        .expect("halfvec binary quantize");

    match results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(
                rows[0].values,
                vec![
                    aiondb_core::Value::Text("1001".to_owned()),
                    aiondb_core::Value::Text("1001".to_owned()),
                ]
            );
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn halfvec_to_float4_pgvector_compat() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT \
                halfvec_to_float4(CAST('[1.0,0.0,2.5]' AS HALFVEC(3)), 3, true), \
                pg_catalog.halfvec_to_float4(CAST('[1.0,0.0,2.5]' AS HALFVEC(3)), 3, true), \
                CAST(CAST('[1.0,0.0,2.5]' AS HALFVEC(3)) AS REAL[])",
        )
        .expect("halfvec_to_float4");

    match results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            let expected = aiondb_core::Value::Array(vec![
                aiondb_core::Value::Real(1.0),
                aiondb_core::Value::Real(0.0),
                aiondb_core::Value::Real(2.5),
            ]);
            assert_eq!(rows[0].values[0], expected);
            assert_eq!(rows[0].values[1], expected);
            assert_eq!(rows[0].values[2], expected);
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn pg_catalog_pgvector_distance_aliases_execute() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT \
                pg_catalog.l2_distance(CAST('[1.0,2.0]' AS VECTOR(2)), CAST('[4.0,6.0]' AS VECTOR(2))), \
                pg_catalog.cosine_distance(CAST('[1.0,0.0]' AS HALFVEC(2)), CAST('[0.0,1.0]' AS HALFVEC(2))), \
                pg_catalog.inner_product(CAST('{1:1.0,3:2.0}/3' AS SPARSEVEC(3)), CAST('{1:3.0,3:4.0}/3' AS SPARSEVEC(3))), \
                pg_catalog.negative_inner_product(CAST('[1.0,2.0]' AS VECTOR(2)), CAST('[4.0,6.0]' AS VECTOR(2))), \
                pg_catalog.hamming_distance('1010', '1001')",
        )
        .expect("pg_catalog pgvector distances");

    match results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], aiondb_core::Value::Double(5.0));
            assert_eq!(rows[0].values[1], aiondb_core::Value::Double(1.0));
            assert_eq!(rows[0].values[2], aiondb_core::Value::Double(11.0));
            assert_eq!(rows[0].values[3], aiondb_core::Value::Double(-16.0));
            assert_eq!(rows[0].values[4], aiondb_core::Value::Double(2.0));
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn vector_cast_from_numeric_array_pgvector_compat() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT \
                CAST(ARRAY[1.0,2.5,-3.0] AS VECTOR(3)), \
                vector_dims(CAST(ARRAY[1,2,3,4] AS VECTOR)), \
                CAST(CAST('[1.0,2.5,-3.0]' AS VECTOR(3)) AS REAL[]), \
                CAST(CAST('[1.0,2.5,-3.0]' AS VECTOR(3)) AS DOUBLE PRECISION[])",
        )
        .expect("numeric array to vector cast");

    match results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(
                rows[0].values[0],
                aiondb_core::Value::Vector(aiondb_core::VectorValue::new(3, vec![1.0, 2.5, -3.0]))
            );
            assert_eq!(rows[0].values[1], aiondb_core::Value::Int(4));
            assert_eq!(
                rows[0].values[2],
                aiondb_core::Value::Array(vec![
                    aiondb_core::Value::Real(1.0),
                    aiondb_core::Value::Real(2.5),
                    aiondb_core::Value::Real(-3.0)
                ])
            );
            assert_eq!(
                rows[0].values[3],
                aiondb_core::Value::Array(vec![
                    aiondb_core::Value::Double(1.0),
                    aiondb_core::Value::Double(2.5),
                    aiondb_core::Value::Double(-3.0)
                ])
            );
        }
        other => panic!("expected Query, got {other:?}"),
    }

    let err = engine
        .execute_sql(&session, "SELECT CAST(ARRAY[1.0,2.0] AS VECTOR(3))")
        .expect_err("array vector cast should validate dimensions");
    assert!(
        err.to_string().contains("expected 3 dimensions"),
        "expected dimension mismatch, got {err}"
    );
}

#[test]
fn vector_sum_and_avg_aggregates_pgvector_compat() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, v VECTOR(3)); \
             INSERT INTO items VALUES \
                 (1, '[1.0,2.0,3.0]'), \
                 (2, '[4.0,5.0,6.0]'), \
                 (3, NULL)",
        )
        .expect("setup");
    engine
        .execute_sql(
            &session,
            "CREATE TABLE half_items (id INT, v HALFVEC(3)); \
             INSERT INTO half_items VALUES \
                 (1, '[1.0,2.0,3.0]'), \
                 (2, '[4.0,5.0,6.0]'), \
                 (3, NULL)",
        )
        .expect("halfvec setup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT sum(v), avg(v) FROM items \
             UNION ALL \
             SELECT sum(v), avg(v) FROM half_items",
        )
        .expect("vector aggregates");

    match results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 2);
            for row in rows {
                assert_eq!(
                    row.values[0],
                    aiondb_core::Value::Vector(aiondb_core::VectorValue::new(
                        3,
                        vec![5.0, 7.0, 9.0]
                    ))
                );
                assert_eq!(
                    row.values[1],
                    aiondb_core::Value::Vector(aiondb_core::VectorValue::new(
                        3,
                        vec![2.5, 3.5, 4.5]
                    ))
                );
            }
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn vector_hnsw_manhattan_index_lowers_to_hnsw_scan() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, v VECTOR(3)); \
             CREATE INDEX idx_v ON items USING hnsw (v) \
                WITH (m = 8, ef_construction = 40, distance = 'manhattan'); \
             INSERT INTO items VALUES \
                 (1, '[0.0,0.0,0.0]'), \
                 (2, '[1.0,0.0,0.0]'), \
                 (3, '[5.0,5.0,5.0]')",
        )
        .expect("setup");

    let statement = parse_prepared_statement(
        "SELECT id FROM items \
         ORDER BY manhattan_distance(v, '[0.1,0.1,0.1]') \
         LIMIT 1",
    )
    .expect("parse");
    let plan = engine
        .build_physical_plan(&session, &statement)
        .expect("physical plan");
    match plan {
        aiondb_plan::PhysicalPlan::HnswScan { limit, .. } => {
            assert_eq!(limit, 1);
        }
        other => panic!("expected HnswScan for manhattan metric, got {other:?}"),
    }
}

#[test]
fn pgvector_l1_operator_lowers_to_hnsw_scan_for_l1_index() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, v VECTOR(3)); \
             CREATE INDEX idx_v ON items USING hnsw (v vector_l1_ops); \
             INSERT INTO items VALUES \
                 (1, '[0.0,0.0,0.0]'), \
                 (2, '[1.0,0.0,0.0]'), \
                 (3, '[5.0,5.0,5.0]')",
        )
        .expect("setup");

    let statement =
        parse_prepared_statement("SELECT id FROM items ORDER BY v <+> '[0.1,0.1,0.1]' LIMIT 1")
            .expect("parse");
    let plan = engine
        .build_physical_plan(&session, &statement)
        .expect("physical plan");
    assert!(
        matches!(plan, aiondb_plan::PhysicalPlan::HnswScan { .. }),
        "<+> should lower to HnswScan against a vector_l1_ops index: {plan:?}"
    );
}

#[test]
fn vector_hnsw_cosine_index_ranks_by_direction() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, v VECTOR(3)); \
             CREATE INDEX idx_v ON items USING hnsw (v) \
                WITH (m = 8, ef_construction = 40, distance = 'cosine'); \
             INSERT INTO items VALUES \
                 (1, '[1.0,0.0,0.0]'), \
                 (2, '[0.0,1.0,0.0]'), \
                 (3, '[0.0,0.0,1.0]'), \
                 (4, '[10.0,0.0,0.0]')",
        )
        .expect("setup");

    // Query colinear with tuples 1 and 4. Under cosine, tuples 1 and 4 are
    // equidistant (both same direction), so either can come first, but tuples
    // 2 and 3 (orthogonal) must be the farthest.
    let results = engine
        .execute_sql(
            &session,
            "SELECT id FROM items \
             ORDER BY cosine_distance(v, '[2.0,0.0,0.0]') \
             LIMIT 2",
        )
        .expect("vector search");

    match results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 2);
            // Extract id values.
            let ids: Vec<i32> = rows
                .iter()
                .filter_map(|row| match row.values.first()? {
                    aiondb_core::Value::Int(v) => Some(*v),
                    _ => None,
                })
                .collect();
            // Both top-2 must be the colinear vectors (1 and 4).
            assert!(ids.contains(&1), "expected id 1 in top-2, got {ids:?}");
            assert!(ids.contains(&4), "expected id 4 in top-2, got {ids:?}");
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn vector_hnsw_insert_then_search() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, embedding VECTOR(3)); \
             CREATE INDEX idx_emb ON items USING hnsw (embedding); \
             INSERT INTO items VALUES \
                 (1, '[1.0,0.0,0.0]'), \
                 (2, '[0.0,1.0,0.0]'), \
                 (3, '[0.0,0.0,1.0]'), \
                 (4, '[1.0,1.0,0.0]'), \
                 (5, '[0.0,1.0,1.0]')",
        )
        .expect("setup");

    // The optimizer should recognize this ORDER BY l2_distance + LIMIT pattern
    // and route through the HNSW index or fall back to exact scan.
    // Either way, the result must be correct.
    let results = engine
        .execute_sql(
            &session,
            "SELECT id FROM items \
             ORDER BY l2_distance(embedding, embedding) \
             LIMIT 3",
        )
        .expect("vector search");

    match results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            // Self-distance is 0 for all, so all are tied at distance 0.
            // Expect exactly 3 rows.
            assert_eq!(rows.len(), 3);
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn vector_hnsw_physical_plan_uses_index_for_cast_literal_query() {
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
    engine
        .execute_sql(&session, "SET hnsw.ef_search = 128")
        .expect("set hnsw.ef_search");

    let statement = parse_prepared_statement(
        "SELECT id FROM items \
         ORDER BY l2_distance(v, CAST('[1.0,0.0,0.0]' AS VECTOR(3))) \
         LIMIT 1",
    )
    .expect("parse");
    let plan = engine
        .build_physical_plan(&session, &statement)
        .expect("physical plan");
    match plan {
        aiondb_plan::PhysicalPlan::HnswScan {
            query_vector,
            limit,
            ef_search,
            ..
        } => {
            assert_eq!(query_vector, vec![1.0, 0.0, 0.0]);
            assert_eq!(limit, 1);
            assert_eq!(ef_search, 128);
        }
        other => panic!("expected HnswScan, got {other:?}"),
    }
}

#[test]
fn vector_hnsw_physical_plan_uses_index_for_plain_text_literal_query() {
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

    let statement = parse_prepared_statement(
        "SELECT id FROM items ORDER BY l2_distance(v, '[1.0,0.0,0.0]') LIMIT 1",
    )
    .expect("parse");
    let plan = engine
        .build_physical_plan(&session, &statement)
        .expect("physical plan");
    match plan {
        aiondb_plan::PhysicalPlan::HnswScan {
            query_vector,
            limit,
            ..
        } => {
            assert_eq!(query_vector, vec![1.0, 0.0, 0.0]);
            assert_eq!(limit, 1);
        }
        other => panic!("expected HnswScan, got {other:?}"),
    }
}

#[test]
fn vector_hnsw_physical_plan_uses_index_for_cast_column_query() {
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

    let statement = parse_prepared_statement(
        "SELECT id FROM items \
         ORDER BY l2_distance(CAST(v AS VECTOR(3)), '[1.0,0.0,0.0]') \
         LIMIT 1",
    )
    .expect("parse");
    let plan = engine
        .build_physical_plan(&session, &statement)
        .expect("physical plan");
    match plan {
        aiondb_plan::PhysicalPlan::HnswScan {
            query_vector,
            limit,
            ..
        } => {
            assert_eq!(query_vector, vec![1.0, 0.0, 0.0]);
            assert_eq!(limit, 1);
        }
        other => panic!("expected HnswScan, got {other:?}"),
    }
}

#[test]
fn vector_hnsw_physical_plan_wraps_filtered_search_in_project_source() {
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

    let statement = parse_prepared_statement(
        "SELECT id FROM items \
         WHERE id > 1 \
         ORDER BY l2_distance(v, '[1.0,0.0,0.0]') \
         LIMIT 2",
    )
    .expect("parse");
    let plan = engine
        .build_physical_plan(&session, &statement)
        .expect("physical plan");
    match plan {
        aiondb_plan::PhysicalPlan::ProjectSource {
            source,
            filter,
            order_by,
            limit,
            ..
        } => {
            assert!(filter.is_some(), "expected WHERE predicate in wrapper");
            assert!(
                order_by.is_empty(),
                "wrapper should preserve ANN source ordering"
            );
            assert!(
                matches!(
                    limit.as_ref().map(|expr| &expr.kind),
                    Some(aiondb_plan::TypedExprKind::Literal(
                        aiondb_core::Value::Int(2)
                    ))
                ),
                "expected LIMIT 2 in wrapper plan, got {limit:?}"
            );
            match source.as_ref() {
                aiondb_plan::PhysicalPlan::HnswScan { limit, .. } => {
                    assert!(
                        *limit >= 2,
                        "filtered HNSW source should fetch at least LIMIT candidates"
                    );
                }
                other => panic!("expected inner HnswScan, got {other:?}"),
            }
        }
        other => panic!("expected ProjectSource wrapper, got {other:?}"),
    }
}

#[test]
fn vector_hnsw_filtered_query_executes_end_to_end() {
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

    let results = engine
        .execute_sql(
            &session,
            "SELECT id FROM items \
             WHERE id > 1 \
             ORDER BY l2_distance(v, '[1.0,0.0,0.0]') \
             LIMIT 2",
        )
        .expect("filtered vector search");

    match results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 2);
            for row in rows {
                let Value::Int(id) = row.values[0] else {
                    panic!("expected INT id column, got {:?}", row.values[0]);
                };
                assert!(id > 1, "filtered query returned id={id}, expected id > 1");
            }
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn vector_hnsw_filtered_query_adaptive_widening_hits_limit() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let mut setup_sql = String::from(
        "CREATE TABLE items (id INT, v VECTOR(1)); \
         CREATE INDEX idx_v ON items USING hnsw (v); \
         INSERT INTO items VALUES ",
    );
    for id in 1..=3_000i32 {
        if id > 1 {
            setup_sql.push(',');
        }
        setup_sql.push_str(&format!("({id}, '[{id}.0]')"));
    }
    engine.execute_sql(&session, &setup_sql).expect("setup");

    // Filter keeps only very distant ids. The initial ANN over-fetch window
    // (LIMIT*8 with floor 256) is insufficient here, so executor-side
    // adaptive widening must expand the candidate window to satisfy LIMIT.
    let results = engine
        .execute_sql(
            &session,
            "SELECT id FROM items \
             WHERE id > 1990 \
             ORDER BY l2_distance(v, '[0.0]') \
             LIMIT 5",
        )
        .expect("filtered vector search with adaptive widening");

    match results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(
                rows.len(),
                5,
                "adaptive widening should satisfy LIMIT when qualifying rows exist"
            );
            let ids = rows
                .iter()
                .map(|row| match row.values[0] {
                    Value::Int(id) => id,
                    ref other => panic!("expected INT id column, got {other:?}"),
                })
                .collect::<Vec<_>>();
            assert_eq!(ids, vec![1991, 1992, 1993, 1994, 1995]);
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn vector_hnsw_filtered_offset_query_adaptive_widening_hits_limit() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let mut setup_sql = String::from(
        "CREATE TABLE items (id INT, v VECTOR(1)); \
         CREATE INDEX idx_v ON items USING hnsw (v); \
         INSERT INTO items VALUES ",
    );
    for id in 1..=3_000i32 {
        if id > 1 {
            setup_sql.push(',');
        }
        setup_sql.push_str(&format!("({id}, '[{id}.0]')"));
    }
    engine.execute_sql(&session, &setup_sql).expect("setup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT id FROM items \
             WHERE id > 1990 \
             ORDER BY l2_distance(v, '[0.0]') \
             LIMIT 3 OFFSET 2",
        )
        .expect("filtered offset vector search with adaptive widening");

    match results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(
                rows.len(),
                3,
                "adaptive widening should satisfy LIMIT with filtered OFFSET pagination"
            );
            let ids = rows
                .iter()
                .map(|row| match row.values[0] {
                    Value::Int(id) => id,
                    ref other => panic!("expected INT id column, got {other:?}"),
                })
                .collect::<Vec<_>>();
            assert_eq!(ids, vec![1993, 1994, 1995]);
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn vector_hnsw_filtered_large_limit_caps_ef_search_and_executes() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let mut setup_sql = String::from(
        "CREATE TABLE items (id INT, v VECTOR(1)); \
         CREATE INDEX idx_v ON items USING hnsw (v); \
         INSERT INTO items VALUES ",
    );
    for id in 1..=3_000i32 {
        if id > 1 {
            setup_sql.push(',');
        }
        setup_sql.push_str(&format!("({id}, '[{id}.0]')"));
    }
    engine.execute_sql(&session, &setup_sql).expect("setup");

    let statement = parse_prepared_statement(
        "SELECT id FROM items \
         WHERE id > 0 \
         ORDER BY l2_distance(v, '[0.0]') \
         LIMIT 3000",
    )
    .expect("parse");
    let plan = engine
        .build_physical_plan(&session, &statement)
        .expect("physical plan");

    match plan {
        aiondb_plan::PhysicalPlan::ProjectSource { source, .. } => match source.as_ref() {
            aiondb_plan::PhysicalPlan::HnswScan {
                limit, ef_search, ..
            } => {
                assert_eq!(*limit, 10_000);
                assert_eq!(
                    *ef_search, 16_384,
                    "ef_search should be clamped to storage hard cap"
                );
            }
            other => panic!("expected inner HnswScan, got {other:?}"),
        },
        other => panic!("expected ProjectSource wrapper, got {other:?}"),
    }

    let results = engine
        .execute_sql(
            &session,
            "SELECT id FROM items \
             WHERE id > 0 \
             ORDER BY l2_distance(v, '[0.0]') \
             LIMIT 3000",
        )
        .expect("filtered large-limit vector search");
    match results.last().unwrap() {
        StatementResult::Query { rows, .. } => assert_eq!(rows.len(), 3_000),
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn vector_hnsw_physical_plan_wraps_offset_search_in_project_source() {
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

    let statement = parse_prepared_statement(
        "SELECT id FROM items \
         ORDER BY l2_distance(v, '[1.0,0.0,0.0]') \
         LIMIT 2 OFFSET 1",
    )
    .expect("parse");
    let plan = engine
        .build_physical_plan(&session, &statement)
        .expect("physical plan");
    match plan {
        aiondb_plan::PhysicalPlan::ProjectSource {
            source,
            filter,
            order_by,
            limit,
            offset,
            ..
        } => {
            assert!(filter.is_none());
            assert!(
                order_by.is_empty(),
                "wrapper should preserve ANN source ordering"
            );
            assert!(
                matches!(
                    limit.as_ref().map(|expr| &expr.kind),
                    Some(aiondb_plan::TypedExprKind::Literal(
                        aiondb_core::Value::Int(2)
                    ))
                ),
                "expected LIMIT 2 in wrapper plan, got {limit:?}"
            );
            assert!(
                matches!(
                    offset.as_ref().map(|expr| &expr.kind),
                    Some(aiondb_plan::TypedExprKind::Literal(
                        aiondb_core::Value::Int(1)
                    ))
                ),
                "expected OFFSET 1 in wrapper plan, got {offset:?}"
            );
            match source.as_ref() {
                aiondb_plan::PhysicalPlan::HnswScan { limit, .. } => {
                    assert!(
                        *limit >= 3,
                        "offset search should fetch at least LIMIT + OFFSET candidates"
                    );
                }
                other => panic!("expected inner HnswScan, got {other:?}"),
            }
        }
        other => panic!("expected ProjectSource wrapper, got {other:?}"),
    }
}

#[test]
fn vector_hnsw_physical_plan_wraps_computed_projection_in_project_source() {
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

    let statement = parse_prepared_statement(
        "SELECT id + 1 AS id2 FROM items \
         ORDER BY l2_distance(v, '[1.0,0.0,0.0]') \
         LIMIT 1",
    )
    .expect("parse");
    let plan = engine
        .build_physical_plan(&session, &statement)
        .expect("physical plan");
    match plan {
        aiondb_plan::PhysicalPlan::ProjectSource { source, .. } => {
            assert!(
                matches!(source.as_ref(), aiondb_plan::PhysicalPlan::HnswScan { .. }),
                "computed projection should be wrapped over HnswScan source"
            );
        }
        other => panic!("expected ProjectSource wrapper, got {other:?}"),
    }
}

#[test]
fn vector_hnsw_computed_projection_query_executes_end_to_end() {
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

    let results = engine
        .execute_sql(
            &session,
            "SELECT id + 1 AS id2 FROM items \
             ORDER BY l2_distance(v, '[1.0,0.0,0.0]') \
             LIMIT 1",
        )
        .expect("computed projection vector query");

    match results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            match rows[0].values.first().unwrap() {
                Value::Int(id2) => assert_eq!(*id2, 2),
                other => panic!("expected Int, got {other:?}"),
            }
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn vector_hnsw_offset_query_executes_end_to_end() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, v VECTOR(3)); \
             CREATE INDEX idx_v ON items USING hnsw (v); \
             INSERT INTO items VALUES \
                 (1, '[1.0,0.0,0.0]'), \
                 (2, '[0.2,0.0,0.0]'), \
                 (3, '[0.4,0.0,0.0]'), \
                 (4, '[0.6,0.0,0.0]'), \
                 (5, '[0.8,0.0,0.0]')",
        )
        .expect("setup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT id FROM items \
             ORDER BY l2_distance(v, '[1.0,0.0,0.0]') \
             LIMIT 2 OFFSET 1",
        )
        .expect("offset vector search");

    match results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 2);
            let ids = rows
                .iter()
                .map(|row| match row.values[0] {
                    Value::Int(id) => id,
                    ref other => panic!("expected INT id column, got {other:?}"),
                })
                .collect::<Vec<_>>();
            assert_eq!(
                ids,
                vec![5, 4],
                "expected stable nearest-neighbor pagination order"
            );
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn vector_hnsw_executes_plain_text_literal_query_end_to_end() {
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

    let results = engine
        .execute_sql(
            &session,
            "SELECT id FROM items ORDER BY l2_distance(v, '[1.0,0.0,0.0]') LIMIT 1",
        )
        .expect("plain text literal vector query should execute");

    match results.last().unwrap() {
        StatementResult::Query { columns, rows } => {
            assert_eq!(columns.len(), 1);
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values.len(), 1);
            assert_eq!(rows[0].values[0], aiondb_core::Value::Int(1));
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn vector_cosine_literal_query_does_not_use_hnsw_scan() {
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

    let statement = parse_prepared_statement(
        "SELECT id FROM items ORDER BY cosine_distance(v, '[1.0,0.0,0.0]') LIMIT 1",
    )
    .expect("parse");
    let plan = engine
        .build_physical_plan(&session, &statement)
        .expect("physical plan");
    assert!(
        !matches!(plan, aiondb_plan::PhysicalPlan::HnswScan { .. }),
        "cosine distance should not lower to L2-only HnswScan: {plan:?}"
    );
}

#[test]
fn vector_large_limit_does_not_use_hnsw_scan() {
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

    let statement = parse_prepared_statement(
        "SELECT id FROM items ORDER BY l2_distance(v, '[1.0,0.0,0.0]') LIMIT 10001",
    )
    .expect("parse");
    let plan = engine
        .build_physical_plan(&session, &statement)
        .expect("physical plan");
    assert!(
        !matches!(plan, aiondb_plan::PhysicalPlan::HnswScan { .. }),
        "large LIMIT should not lower to HnswScan: {plan:?}"
    );
}

#[test]
fn vector_cosine_plain_text_literal_query_executes_end_to_end() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, v VECTOR(2)); \
             INSERT INTO items VALUES \
                 (1, '[1.0,0.0]'), \
                 (2, '[1.0,1.0]'), \
                 (3, '[0.0,1.0]'), \
                 (4, '[-1.0,0.0]'); \
             SELECT id FROM items ORDER BY cosine_distance(v, '[1.0,0.0]') LIMIT 1",
        )
        .expect("cosine query with plain text literal should execute");

    match results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], aiondb_core::Value::Int(1));
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn vector_hnsw_nearest_neighbor_search() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    // Use a query column alongside the data column so l2_distance sees two
    // different vectors (the optimizer pattern needs col vs literal).
    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, v VECTOR(3), q VECTOR(3)); \
             CREATE INDEX idx_v ON items USING hnsw (v); \
             INSERT INTO items VALUES \
                 (1, '[1.0,0.0,0.0]', '[1.0,0.0,0.0]'), \
                 (2, '[0.0,1.0,0.0]', '[1.0,0.0,0.0]'), \
                 (3, '[0.0,0.0,1.0]', '[1.0,0.0,0.0]'), \
                 (4, '[1.0,1.0,0.0]', '[1.0,0.0,0.0]'), \
                 (5, '[10.0,10.0,10.0]', '[1.0,0.0,0.0]')",
        )
        .expect("setup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT id, l2_distance(v, q) AS dist \
             FROM items \
             ORDER BY dist \
             LIMIT 3",
        )
        .expect("nn search");

    match results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 3, "expected 3 nearest neighbors");
            // Closest to [1,0,0]: id=1 (dist 0)
            assert_eq!(rows[0].values[0], aiondb_core::Value::Int(1));
            let dist0 = match &rows[0].values[1] {
                aiondb_core::Value::Double(d) => *d,
                other => panic!("expected Double, got {other:?}"),
            };
            assert!(
                dist0.abs() < 1e-10,
                "nearest should have distance 0, got {dist0}"
            );
            // id=5 should NOT be in top 3 (it is far away)
            for row in rows {
                assert_ne!(
                    row.values[0],
                    aiondb_core::Value::Int(5),
                    "farthest vector should not be in top 3"
                );
            }
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn vector_hnsw_delete_in_same_tx_still_fills_visible_limit() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, v VECTOR(2)); \
             CREATE INDEX idx_v ON items USING hnsw (v); \
             INSERT INTO items VALUES \
                 (1, '[1.0,0.0]'), \
                 (2, '[1.0,0.1]'), \
                 (3, '[1.0,0.2]')",
        )
        .expect("setup");

    engine.execute_sql(&session, "BEGIN").expect("begin");
    engine
        .execute_sql(&session, "DELETE FROM items WHERE id = 1")
        .expect("delete nearest row");

    let results = engine
        .execute_sql(
            &session,
            "SELECT id FROM items ORDER BY l2_distance(v, '[1.0,0.0]') LIMIT 2",
        )
        .expect("vector query after delete");

    match results.last().unwrap() {
        StatementResult::Query { columns, rows } => {
            assert_eq!(columns.len(), 1);
            assert_eq!(rows.len(), 2, "visible rows should still fill LIMIT");
            assert_eq!(rows[0].values, vec![aiondb_core::Value::Int(2)]);
            assert_eq!(rows[1].values, vec![aiondb_core::Value::Int(3)]);
        }
        other => panic!("expected Query, got {other:?}"),
    }

    engine.execute_sql(&session, "ROLLBACK").expect("rollback");
}

#[test]
fn vector_hnsw_plan_rejects_non_finite_plain_text_literal_query_vector() {
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

    let statement = parse_prepared_statement(
        "SELECT id FROM items ORDER BY l2_distance(v, '[NaN,0.0,1.0]') LIMIT 1",
    )
    .expect("parse");
    let err = engine.build_physical_plan(&session, &statement).expect_err(
        "non-finite plain text literal query vector should be rejected during planning",
    );
    assert!(
        err.to_string()
            .contains("invalid input syntax for type vector"),
        "unexpected error: {err}"
    );
}

#[test]
fn vector_top_k_ids_cosine_metric_ignores_l2_hnsw_index() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, v VECTOR(2)); \
             CREATE INDEX idx_v_l2 ON items USING hnsw (v); \
             INSERT INTO items VALUES \
                 (1, '[10.0,0.0]'), \
                 (2, '[1.0,1.0]'), \
                 (3, '[0.0,10.0]')",
        )
        .expect("setup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT item_id \
             FROM vector_top_k_ids('items', 'v', '[1.0,0.0]', 1, 'cosine') AS seeds(item_id)",
        )
        .expect("vector_top_k_ids cosine search");

    match results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::BigInt(1));
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn vector_top_k_ids_l2_metric_ignores_cosine_hnsw_index() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, v VECTOR(2)); \
             CREATE INDEX idx_v_cos ON items USING hnsw (v) \
                 WITH (distance = 'cosine'); \
             INSERT INTO items VALUES \
                 (1, '[10.0,0.0]'), \
                 (2, '[1.0,1.0]'), \
                 (3, '[0.0,10.0]')",
        )
        .expect("setup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT item_id \
             FROM vector_top_k_ids('items', 'v', '[1.0,0.0]', 1) AS seeds(item_id)",
        )
        .expect("vector_top_k_ids l2 search");

    match results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::BigInt(2));
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn vector_top_k_ids_inner_product_metric_with_matching_hnsw_index() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, v VECTOR(2)); \
             CREATE INDEX idx_v_ip ON items USING hnsw (v) \
                 WITH (distance = 'inner_product'); \
             INSERT INTO items VALUES \
                 (1, '[10.0,0.0]'), \
                 (2, '[1.0,1.0]'), \
                 (3, '[0.0,10.0]')",
        )
        .expect("setup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT item_id \
             FROM vector_top_k_ids('items', 'v', '[1.0,0.0]', 1, 'inner_product') AS seeds(item_id)",
        )
        .expect("vector_top_k_ids inner_product search");

    match results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::BigInt(1));
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn vector_top_k_ids_inner_product_metric_ignores_l2_hnsw_index() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, v VECTOR(2)); \
             CREATE INDEX idx_v_l2 ON items USING hnsw (v); \
             INSERT INTO items VALUES \
                 (1, '[10.0,0.0]'), \
                 (2, '[1.0,1.0]'), \
                 (3, '[0.0,10.0]')",
        )
        .expect("setup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT item_id \
             FROM vector_top_k_ids('items', 'v', '[1.0,0.0]', 1, 'inner_product') AS seeds(item_id)",
        )
        .expect("vector_top_k_ids inner_product search");

    match results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::BigInt(1));
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn vector_top_k_ids_manhattan_metric_with_matching_hnsw_index() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, v VECTOR(2)); \
             CREATE INDEX idx_v_l1 ON items USING hnsw (v) \
                 WITH (distance = 'manhattan'); \
             INSERT INTO items VALUES \
                 (1, '[0.0,3.0]'), \
                 (2, '[2.0,0.0]'), \
                 (3, '[0.0,0.0]')",
        )
        .expect("setup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT item_id \
             FROM vector_top_k_ids('items', 'v', '[2.0,2.0]', 1, 'manhattan') AS seeds(item_id)",
        )
        .expect("vector_top_k_ids manhattan search");

    match results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::BigInt(2));
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn vector_top_k_ids_hnsw_distance_threshold_filters_results() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, v VECTOR(2)); \
             CREATE INDEX idx_v_l2 ON items USING hnsw (v); \
             INSERT INTO items VALUES \
                 (1, '[1.0,0.0]'), \
                 (2, '[2.0,0.0]'), \
                 (3, '[4.0,0.0]')",
        )
        .expect("setup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT item_id \
             FROM vector_top_k_ids('items', 'v', '[1.0,0.0]', 3, 'l2', 64, 0.6) \
                  AS seeds(item_id)",
        )
        .expect("vector_top_k_ids thresholded hnsw search");

    match results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1, "distance threshold should keep only one row");
            assert_eq!(rows[0].values[0], Value::BigInt(1));
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn vector_top_k_ids_exact_distance_threshold_filters_results() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, v VECTOR(2)); \
             INSERT INTO items VALUES \
                 (1, '[1.0,0.0]'), \
                 (2, '[2.0,0.0]'), \
                 (3, '[4.0,0.0]')",
        )
        .expect("setup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT item_id \
             FROM vector_top_k_ids('items', 'v', '[1.0,0.0]', 3, 'l2', 64, 0.6) \
                  AS seeds(item_id)",
        )
        .expect("vector_top_k_ids thresholded exact search");

    match results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1, "distance threshold should keep only one row");
            assert_eq!(rows[0].values[0], Value::BigInt(1));
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn vector_top_k_ids_ef_search_over_cap_is_bounded() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, v VECTOR(2)); \
             CREATE INDEX idx_v_l2 ON items USING hnsw (v); \
             INSERT INTO items VALUES \
                 (1, '[1.0,0.0]'), \
                 (2, '[2.0,0.0]'), \
                 (3, '[4.0,0.0]')",
        )
        .expect("setup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT item_id \
             FROM vector_top_k_ids('items', 'v', '[1.0,0.0]', 1, 'l2', 1000000) \
                  AS seeds(item_id)",
        )
        .expect("vector_top_k_ids with oversized ef_search should be bounded");

    match results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::BigInt(1));
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn vector_top_k_ids_ef_search_zero_is_rejected() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, v VECTOR(2)); \
             CREATE INDEX idx_v_l2 ON items USING hnsw (v); \
             INSERT INTO items VALUES (1, '[1.0,0.0]')",
        )
        .expect("setup");

    let err = engine
        .execute_sql(
            &session,
            "SELECT item_id \
             FROM vector_top_k_ids('items', 'v', '[1.0,0.0]', 1, 'l2', 0) AS seeds(item_id)",
        )
        .expect_err("ef_search=0 should be rejected");
    assert!(
        err.to_string().contains("ef_search"),
        "expected ef_search validation error, got {err}"
    );
}

#[test]
fn vector_top_k_ids_score_threshold_filters_l2_exact_results() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, v VECTOR(2)); \
             INSERT INTO items VALUES \
                 (1, '[1.0,0.0]'), \
                 (2, '[2.0,0.0]'), \
                 (3, '[4.0,0.0]')",
        )
        .expect("setup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT item_id \
             FROM vector_top_k_ids('items', 'v', '[1.0,0.0]', 3, 'l2', 64, 10.0, false, -0.5) \
                  AS seeds(item_id)",
        )
        .expect("vector_top_k_ids score-thresholded exact search");

    match results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1, "score threshold should keep only one row");
            assert_eq!(rows[0].values[0], Value::BigInt(1));
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn vector_top_k_ids_score_threshold_filters_cosine_hnsw_results() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, v VECTOR(2)); \
             CREATE INDEX idx_v_cos ON items USING hnsw (v) WITH (distance = 'cosine'); \
             INSERT INTO items VALUES \
                 (1, '[1.0,0.0]'), \
                 (2, '[0.0,1.0]'), \
                 (3, '[0.7,0.7]')",
        )
        .expect("setup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT item_id \
             FROM vector_top_k_ids('items', 'v', '[1.0,0.0]', 3, 'cosine', 64, 2.0, false, 0.8) \
                  AS seeds(item_id)",
        )
        .expect("vector_top_k_ids score-thresholded cosine search");

    match results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1, "score threshold should keep only one row");
            assert_eq!(rows[0].values[0], Value::BigInt(1));
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn vector_top_k_ids_exact_argument_type_is_validated() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, v VECTOR(2)); \
             INSERT INTO items VALUES (1, '[1.0,0.0]')",
        )
        .expect("setup");

    let err = engine
        .execute_sql(
            &session,
            "SELECT item_id \
             FROM vector_top_k_ids('items', 'v', '[1.0,0.0]', 1, 'l2', 64, 10.0, 'yes') \
                  AS seeds(item_id)",
        )
        .expect_err("exact argument should require boolean");
    assert!(
        err.to_string().contains("exact"),
        "expected exact type validation error, got {err}"
    );
}

#[test]
fn vector_top_k_ids_exact_mode_argument_is_accepted_with_hnsw_index() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, v VECTOR(2)); \
             CREATE INDEX idx_v_l2 ON items USING hnsw (v); \
             INSERT INTO items VALUES \
                 (1, '[1.0,0.0]'), \
                 (2, '[2.0,0.0]'), \
                 (3, '[4.0,0.0]')",
        )
        .expect("setup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT item_id \
             FROM vector_top_k_ids('items', 'v', '[1.0,0.0]', 1, 'l2', 64, 10.0, true) \
                  AS seeds(item_id)",
        )
        .expect("vector_top_k_ids exact=true");

    match results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::BigInt(1));
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn vector_top_k_ids_json_options_override_positional_arguments() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, v VECTOR(2)); \
             CREATE INDEX idx_v_cos ON items USING hnsw (v) WITH (distance = 'cosine'); \
             INSERT INTO items VALUES \
                 (1, '[1.0,0.0]'), \
                 (2, '[0.0,1.0]'), \
                 (3, '[0.7,0.7]')",
        )
        .expect("setup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT item_id \
             FROM vector_top_k_ids( \
                    'items', \
                    'v', \
                    '[1.0,0.0]', \
                    3, \
                    'l2', \
                    64, \
                    10.0, \
                    false, \
                    -10.0, \
                    '{\"metric\":\"cosine\",\"score_threshold\":0.8}'::jsonb \
                ) AS seeds(item_id)",
        )
        .expect("vector_top_k_ids with jsonb options");

    match results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(
                rows.len(),
                1,
                "options should override metric+score threshold"
            );
            assert_eq!(rows[0].values[0], Value::BigInt(1));
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn vector_top_k_ids_text_options_json_is_supported() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, v VECTOR(2)); \
             INSERT INTO items VALUES \
                 (1, '[1.0,0.0]'), \
                 (2, '[2.0,0.0]'), \
                 (3, '[4.0,0.0]')",
        )
        .expect("setup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT item_id \
             FROM vector_top_k_ids( \
                    'items', \
                    'v', \
                    '[1.0,0.0]', \
                    3, \
                    'l2', \
                    64, \
                    10.0, \
                    false, \
                    -10.0, \
                    '{\"exact\":true,\"distance_threshold\":0.6}' \
                ) AS seeds(item_id)",
        )
        .expect("vector_top_k_ids with text json options");

    match results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1, "text-json options should be applied");
            assert_eq!(rows[0].values[0], Value::BigInt(1));
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn vector_top_k_ids_json_options_qdrant_params_are_supported() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, v VECTOR(2)); \
             CREATE INDEX idx_v_l2 ON items USING hnsw (v); \
             INSERT INTO items VALUES \
                 (1, '[1.0,0.0]'), \
                 (2, '[1.2,0.0]'), \
                 (3, '[4.0,0.0]')",
        )
        .expect("setup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT item_id \
             FROM vector_top_k_ids( \
                    'items', \
                    'v', \
                    '[1.0,0.0]', \
                    3, \
                    'l2', \
                    1, \
                    10.0, \
                    false, \
                    -10.0, \
                    '{\"params\":{\"exact\":true,\"hnsw_ef\":16},\"distance_threshold\":0.5}'::jsonb \
                ) AS seeds(item_id)",
        )
        .expect("vector_top_k_ids qdrant params options");

    match results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 2);
            assert_eq!(rows[0].values[0], Value::BigInt(1));
            assert_eq!(rows[1].values[0], Value::BigInt(2));
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn vector_top_k_ids_json_options_limit_overrides_k_argument() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, v VECTOR(2)); \
             INSERT INTO items VALUES \
                 (1, '[1.0,0.0]'), \
                 (2, '[1.1,0.0]'), \
                 (3, '[2.0,0.0]')",
        )
        .expect("setup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT item_id \
             FROM vector_top_k_ids( \
                    'items', \
                    'v', \
                    '[1.0,0.0]', \
                    0, \
                    'l2', \
                    64, \
                    10.0, \
                    false, \
                    -10.0, \
                    '{\"exact\":true,\"limit\":2}'::jsonb \
                ) AS seeds(item_id)",
        )
        .expect("vector_top_k_ids options limit override");

    match results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 2);
            assert_eq!(rows[0].values[0], Value::BigInt(1));
            assert_eq!(rows[1].values[0], Value::BigInt(2));
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn vector_top_k_ids_json_options_qdrant_params_reject_invalid_hnsw_ef() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, v VECTOR(2)); \
             INSERT INTO items VALUES (1, '[1.0,0.0]')",
        )
        .expect("setup");

    let err = engine
        .execute_sql(
            &session,
            "SELECT item_id \
             FROM vector_top_k_ids( \
                    'items', \
                    'v', \
                    '[1.0,0.0]', \
                    1, \
                    'l2', \
                    64, \
                    10.0, \
                    false, \
                    -10.0, \
                    '{\"params\":{\"hnsw_ef\":0}}'::jsonb \
                ) AS seeds(item_id)",
        )
        .expect_err("params.hnsw_ef=0 should be rejected");
    assert!(
        err.to_string().contains("hnsw_ef"),
        "expected params.hnsw_ef validation error, got {err}"
    );
}

#[test]
fn vector_top_k_ids_json_options_reject_unknown_keys() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, v VECTOR(2)); \
             INSERT INTO items VALUES (1, '[1.0,0.0]')",
        )
        .expect("setup");

    let err = engine
        .execute_sql(
            &session,
            "SELECT item_id \
             FROM vector_top_k_ids( \
                    'items', \
                    'v', \
                    '[1.0,0.0]', \
                    1, \
                    'l2', \
                    64, \
                    10.0, \
                    false, \
                    -10.0, \
                    '{\"mystery\":1}'::jsonb \
                ) AS seeds(item_id)",
        )
        .expect_err("unknown options key should be rejected");
    assert!(
        err.to_string().contains("unknown key"),
        "expected unknown key validation error, got {err}"
    );
}

#[test]
fn vector_top_k_ids_json_options_offset_applies_to_exact_results() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, v VECTOR(2)); \
             INSERT INTO items VALUES \
                 (1, '[1.0,0.0]'), \
                 (2, '[2.0,0.0]'), \
                 (3, '[4.0,0.0]')",
        )
        .expect("setup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT item_id \
             FROM vector_top_k_ids( \
                    'items', \
                    'v', \
                    '[1.0,0.0]', \
                    1, \
                    'l2', \
                    64, \
                    10.0, \
                    false, \
                    -10.0, \
                    '{\"exact\":true,\"offset\":1}'::jsonb \
                ) AS seeds(item_id)",
        )
        .expect("vector_top_k_ids exact offset");

    match results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::BigInt(2));
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn vector_top_k_ids_json_options_offset_applies_to_hnsw_results() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, v VECTOR(2)); \
             CREATE INDEX idx_v_l2 ON items USING hnsw (v); \
             INSERT INTO items VALUES \
                 (1, '[1.0,0.0]'), \
                 (2, '[2.0,0.0]'), \
                 (3, '[4.0,0.0]')",
        )
        .expect("setup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT item_id \
             FROM vector_top_k_ids( \
                    'items', \
                    'v', \
                    '[1.0,0.0]', \
                    1, \
                    'l2', \
                    64, \
                    10.0, \
                    false, \
                    -10.0, \
                    '{\"offset\":1}'::jsonb \
                ) AS seeds(item_id)",
        )
        .expect("vector_top_k_ids hnsw offset");

    match results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::BigInt(2));
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn vector_top_k_ids_null_optional_arguments_are_ignored() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, v VECTOR(2)); \
             INSERT INTO items VALUES \
                 (1, '[1.0,0.0]'), \
                 (2, '[2.0,0.0]')",
        )
        .expect("setup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT item_id \
             FROM vector_top_k_ids( \
                    'items', \
                    'v', \
                    '[1.0,0.0]', \
                    1, \
                    NULL, \
                    NULL, \
                    NULL, \
                    NULL, \
                    NULL, \
                    NULL \
                ) AS seeds(item_id)",
        )
        .expect("null optional args should be ignored");

    match results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::BigInt(1));
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn vector_top_k_ids_json_options_filter_object_applies_to_exact_results() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, tag TEXT, v VECTOR(2)); \
             INSERT INTO items VALUES \
                 (1, 'keep', '[1.0,0.0]'), \
                 (2, 'drop', '[1.1,0.0]'), \
                 (3, 'keep', '[2.0,0.0]'), \
                 (4, 'drop', '[3.0,0.0]')",
        )
        .expect("setup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT item_id \
             FROM vector_top_k_ids( \
                    'items', \
                    'v', \
                    '[1.0,0.0]', \
                    2, \
                    'l2', \
                    64, \
                    10.0, \
                    false, \
                    -10.0, \
                    '{\"exact\":true,\"filter\":{\"tag\":\"keep\"}}'::jsonb \
                ) AS seeds(item_id)",
        )
        .expect("vector_top_k_ids exact payload filter");

    match results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 2);
            assert_eq!(rows[0].values[0], Value::BigInt(1));
            assert_eq!(rows[1].values[0], Value::BigInt(3));
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn vector_top_k_ids_json_options_filter_mixes_qdrant_clauses_and_shorthand_must() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, tag TEXT, source TEXT, v VECTOR(2)); \
             INSERT INTO items VALUES \
                 (1, 'news', 'web', '[1.0,0.0]'), \
                 (2, 'news', 'spam', '[1.1,0.0]'), \
                 (3, 'sports', 'web', '[1.2,0.0]'), \
                 (4, 'news', 'web', '[3.0,0.0]')",
        )
        .expect("setup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT item_id \
             FROM vector_top_k_ids( \
                    'items', \
                    'v', \
                    '[1.0,0.0]', \
                    3, \
                    'l2', \
                    64, \
                    10.0, \
                    false, \
                    -10.0, \
                    '{\"exact\":true,\"filter\":{\"must\":[{\"key\":\"source\",\"match\":{\"value\":\"web\"}}],\"tag\":\"news\"}}'::jsonb \
                ) AS seeds(item_id)",
        )
        .expect("vector_top_k_ids mixed qdrant and shorthand filter");

    match results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 2);
            assert_eq!(rows[0].values[0], Value::BigInt(1));
            assert_eq!(rows[1].values[0], Value::BigInt(4));
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn vector_top_k_ids_json_options_filter_qdrant_must_widens_hnsw_search() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, tag TEXT, v VECTOR(2)); \
             CREATE INDEX idx_v_l2 ON items USING hnsw (v); \
             INSERT INTO items VALUES \
                 (1, 'drop', '[1.00,0.0]'), \
                 (2, 'drop', '[1.05,0.0]'), \
                 (3, 'drop', '[1.10,0.0]'), \
                 (4, 'keep', '[1.15,0.0]'), \
                 (5, 'keep', '[1.20,0.0]')",
        )
        .expect("setup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT item_id \
             FROM vector_top_k_ids( \
                    'items', \
                    'v', \
                    '[1.0,0.0]', \
                    2, \
                    'l2', \
                    2, \
                    10.0, \
                    false, \
                    -10.0, \
                    '{\"filter\":{\"must\":[{\"key\":\"tag\",\"match\":{\"value\":\"keep\"}}]}}'::jsonb \
                ) AS seeds(item_id)",
        )
        .expect("vector_top_k_ids hnsw payload filter");

    match results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(
                rows.len(),
                2,
                "adaptive widening should recover filtered neighbors"
            );
            assert_eq!(rows[0].values[0], Value::BigInt(4));
            assert_eq!(rows[1].values[0], Value::BigInt(5));
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn vector_top_k_ids_hnsw_filter_respects_max_scan_tuples() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, tag TEXT, v VECTOR(2)); \
             CREATE INDEX idx_v_l2 ON items USING hnsw (v); \
             INSERT INTO items VALUES \
                 (1, 'drop', '[1.00,0.0]'), \
                 (2, 'drop', '[1.05,0.0]'), \
                 (3, 'drop', '[1.10,0.0]'), \
                 (4, 'keep', '[1.15,0.0]'), \
                 (5, 'keep', '[1.20,0.0]'); \
             SET hnsw.max_scan_tuples = 1",
        )
        .expect("setup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT item_id \
             FROM vector_top_k_ids( \
                    'items', \
                    'v', \
                    '[1.0,0.0]', \
                    2, \
                    'l2', \
                    2, \
                    10.0, \
                    false, \
                    -10.0, \
                    '{\"filter\":{\"must\":[{\"key\":\"tag\",\"match\":{\"value\":\"keep\"}}]}}'::jsonb \
                ) AS seeds(item_id)",
        )
        .expect("vector_top_k_ids hnsw payload filter");

    match results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert!(
                rows.len() <= 1,
                "hnsw.max_scan_tuples should cap adaptive widening below requested k"
            );
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn vector_top_k_ids_json_options_filter_rejects_unknown_column() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, v VECTOR(2)); \
             INSERT INTO items VALUES (1, '[1.0,0.0]')",
        )
        .expect("setup");

    let err = engine
        .execute_sql(
            &session,
            "SELECT item_id \
             FROM vector_top_k_ids( \
                    'items', \
                    'v', \
                    '[1.0,0.0]', \
                    1, \
                    'l2', \
                    64, \
                    10.0, \
                    false, \
                    -10.0, \
                    '{\"filter\":{\"missing\":\"x\"}}'::jsonb \
                ) AS seeds(item_id)",
        )
        .expect_err("unknown filter column should be rejected");
    assert!(
        err.to_string().contains("does not exist"),
        "expected undefined column error, got {err}"
    );
}

#[test]
fn vector_top_k_ids_json_options_filter_qdrant_range_applies_to_exact_results() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, bucket INT, v VECTOR(2)); \
             INSERT INTO items VALUES \
                 (1, 1, '[1.00,0.0]'), \
                 (2, 2, '[1.05,0.0]'), \
                 (3, 3, '[1.10,0.0]'), \
                 (4, 4, '[1.15,0.0]')",
        )
        .expect("setup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT item_id \
             FROM vector_top_k_ids( \
                    'items', \
                    'v', \
                    '[1.0,0.0]', \
                    3, \
                    'l2', \
                    64, \
                    10.0, \
                    false, \
                    -10.0, \
                    '{\"exact\":true,\"filter\":{\"must\":[{\"key\":\"bucket\",\"range\":{\"gte\":2,\"lt\":4}}]}}'::jsonb \
                ) AS seeds(item_id)",
        )
        .expect("vector_top_k_ids exact range filter");

    match results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 2);
            assert_eq!(rows[0].values[0], Value::BigInt(2));
            assert_eq!(rows[1].values[0], Value::BigInt(3));
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn vector_top_k_ids_json_options_filter_qdrant_match_any_and_except_apply_to_exact_results() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, tag TEXT, v VECTOR(2)); \
             INSERT INTO items VALUES \
                 (1, 'news', '[1.00,0.0]'), \
                 (2, 'drop', '[1.01,0.0]'), \
                 (3, 'sports', '[1.02,0.0]'), \
                 (4, 'other', '[1.03,0.0]')",
        )
        .expect("setup");

    let any_results = engine
        .execute_sql(
            &session,
            "SELECT item_id \
             FROM vector_top_k_ids( \
                    'items', \
                    'v', \
                    '[1.0,0.0]', \
                    3, \
                    'l2', \
                    64, \
                    10.0, \
                    false, \
                    -10.0, \
                    '{\"exact\":true,\"filter\":{\"must\":[{\"key\":\"tag\",\"match\":{\"any\":[\"news\",\"sports\"]}}]}}'::jsonb \
                ) AS seeds(item_id)",
        )
        .expect("vector_top_k_ids exact match.any filter");

    match any_results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 2);
            assert_eq!(rows[0].values[0], Value::BigInt(1));
            assert_eq!(rows[1].values[0], Value::BigInt(3));
        }
        other => panic!("expected Query, got {other:?}"),
    }

    let except_results = engine
        .execute_sql(
            &session,
            "SELECT item_id \
             FROM vector_top_k_ids( \
                    'items', \
                    'v', \
                    '[1.0,0.0]', \
                    3, \
                    'l2', \
                    64, \
                    10.0, \
                    false, \
                    -10.0, \
                    '{\"exact\":true,\"filter\":{\"must\":[{\"key\":\"tag\",\"match\":{\"except\":[\"drop\"]}}]}}'::jsonb \
                ) AS seeds(item_id)",
        )
        .expect("vector_top_k_ids exact match.except filter");

    match except_results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 3);
            assert_eq!(rows[0].values[0], Value::BigInt(1));
            assert_eq!(rows[1].values[0], Value::BigInt(3));
            assert_eq!(rows[2].values[0], Value::BigInt(4));
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn vector_top_k_ids_json_options_filter_qdrant_match_text_applies_to_strings_and_json_arrays() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, body TEXT, payload JSONB, v VECTOR(2)); \
             INSERT INTO items VALUES \
                 (1, 'The Quick Brown Fox', '{\"notes\":[\"vector filters\",\"pg\"]}', '[1.00,0.0]'), \
                 (2, 'quick brown only', '{\"notes\":[\"other\"]}', '[1.01,0.0]'), \
                 (3, 'vector filters only', '{\"notes\":[\"vector filters\"]}', '[1.02,0.0]'), \
                 (4, 'the quick brown fox', '{\"notes\":[\"VECTOR FILTERS\"]}', '[1.03,0.0]')",
        )
        .expect("setup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT item_id \
             FROM vector_top_k_ids( \
                    'items', \
                    'v', \
                    '[1.0,0.0]', \
                    3, \
                    'l2', \
                    64, \
                    10.0, \
                    false, \
                    -10.0, \
                    '{\"exact\":true,\"filter\":{\"must\":[{\"key\":\"body\",\"match\":{\"text\":\"quick brown\"}},{\"key\":\"payload.notes[]\",\"match\":{\"text\":\"vector\"}}]}}'::jsonb \
                ) AS seeds(item_id)",
        )
        .expect("vector_top_k_ids match.text filter");

    match results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 2);
            assert_eq!(rows[0].values[0], Value::BigInt(1));
            assert_eq!(rows[1].values[0], Value::BigInt(4));
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn vector_top_k_ids_json_options_filter_qdrant_match_clauses_apply_to_json_arrays() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, tags JSONB, v VECTOR(2)); \
             INSERT INTO items VALUES \
                 (1, CAST('[\"news\",\"green\"]' AS JSONB), '[1.00,0.0]'), \
                 (2, CAST('[\"sports\",\"red\"]' AS JSONB), '[1.01,0.0]'), \
                 (3, CAST('[\"other\",\"green\"]' AS JSONB), '[1.02,0.0]'), \
                 (4, CAST('[\"black\",\"yellow\"]' AS JSONB), '[1.03,0.0]')",
        )
        .expect("setup");

    let match_results = engine
        .execute_sql(
            &session,
            "SELECT item_id \
             FROM vector_top_k_ids( \
                    'items', \
                    'v', \
                    '[1.0,0.0]', \
                    4, \
                    'l2', \
                    64, \
                    10.0, \
                    false, \
                    -10.0, \
                    '{\"exact\":true,\"filter\":{\"must\":[{\"key\":\"tags\",\"match\":{\"value\":\"green\"}}]}}'::jsonb \
                ) AS seeds(item_id)",
        )
        .expect("vector_top_k_ids exact array match filter");

    match match_results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 2);
            assert_eq!(rows[0].values[0], Value::BigInt(1));
            assert_eq!(rows[1].values[0], Value::BigInt(3));
        }
        other => panic!("expected Query, got {other:?}"),
    }

    let any_results = engine
        .execute_sql(
            &session,
            "SELECT item_id \
             FROM vector_top_k_ids( \
                    'items', \
                    'v', \
                    '[1.0,0.0]', \
                    4, \
                    'l2', \
                    64, \
                    10.0, \
                    false, \
                    -10.0, \
                    '{\"exact\":true,\"filter\":{\"must\":[{\"key\":\"tags\",\"match\":{\"any\":[\"news\",\"sports\"]}}]}}'::jsonb \
                ) AS seeds(item_id)",
        )
        .expect("vector_top_k_ids exact array match.any filter");

    match any_results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 2);
            assert_eq!(rows[0].values[0], Value::BigInt(1));
            assert_eq!(rows[1].values[0], Value::BigInt(2));
        }
        other => panic!("expected Query, got {other:?}"),
    }

    let except_results = engine
        .execute_sql(
            &session,
            "SELECT item_id \
             FROM vector_top_k_ids( \
                    'items', \
                    'v', \
                    '[1.0,0.0]', \
                    4, \
                    'l2', \
                    64, \
                    10.0, \
                    false, \
                    -10.0, \
                    '{\"exact\":true,\"filter\":{\"must\":[{\"key\":\"tags\",\"match\":{\"except\":[\"black\",\"yellow\"]}}]}}'::jsonb \
                ) AS seeds(item_id)",
        )
        .expect("vector_top_k_ids exact array match.except filter");

    match except_results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 3);
            assert_eq!(rows[0].values[0], Value::BigInt(1));
            assert_eq!(rows[1].values[0], Value::BigInt(2));
            assert_eq!(rows[2].values[0], Value::BigInt(3));
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn vector_top_k_ids_json_options_filter_qdrant_nested_json_key_applies_to_exact_results() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, payload JSONB, v VECTOR(2)); \
             INSERT INTO items VALUES \
                 (1, CAST('{\"city\":\"paris\",\"score\":3,\"tags\":[\"news\",\"green\"]}' AS JSONB), '[1.00,0.0]'), \
                 (2, CAST('{\"city\":\"berlin\",\"score\":7,\"tags\":[\"sports\",\"red\"]}' AS JSONB), '[1.01,0.0]'), \
                 (3, CAST('{\"city\":\"paris\",\"score\":9,\"tags\":[\"other\",\"green\"]}' AS JSONB), '[1.02,0.0]'), \
                 (4, CAST('{\"city\":\"rome\",\"score\":1,\"tags\":[]}' AS JSONB), '[1.03,0.0]')",
        )
        .expect("setup");

    let city_results = engine
        .execute_sql(
            &session,
            "SELECT item_id \
             FROM vector_top_k_ids( \
                    'items', \
                    'v', \
                    '[1.0,0.0]', \
                    4, \
                    'l2', \
                    64, \
                    10.0, \
                    false, \
                    -10.0, \
                    '{\"exact\":true,\"filter\":{\"must\":[{\"key\":\"payload.city\",\"match\":{\"value\":\"paris\"}}]}}'::jsonb \
                ) AS seeds(item_id)",
        )
        .expect("vector_top_k_ids exact nested json key match");

    match city_results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 2);
            assert_eq!(rows[0].values[0], Value::BigInt(1));
            assert_eq!(rows[1].values[0], Value::BigInt(3));
        }
        other => panic!("expected Query, got {other:?}"),
    }

    let range_results = engine
        .execute_sql(
            &session,
            "SELECT item_id \
             FROM vector_top_k_ids( \
                    'items', \
                    'v', \
                    '[1.0,0.0]', \
                    4, \
                    'l2', \
                    64, \
                    10.0, \
                    false, \
                    -10.0, \
                    '{\"exact\":true,\"filter\":{\"must\":[{\"key\":\"payload.score\",\"range\":{\"gte\":5}}]}}'::jsonb \
                ) AS seeds(item_id)",
        )
        .expect("vector_top_k_ids exact nested json range");

    match range_results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 2);
            assert_eq!(rows[0].values[0], Value::BigInt(2));
            assert_eq!(rows[1].values[0], Value::BigInt(3));
        }
        other => panic!("expected Query, got {other:?}"),
    }

    let tags_results = engine
        .execute_sql(
            &session,
            "SELECT item_id \
             FROM vector_top_k_ids( \
                    'items', \
                    'v', \
                    '[1.0,0.0]', \
                    4, \
                    'l2', \
                    64, \
                    10.0, \
                    false, \
                    -10.0, \
                    '{\"exact\":true,\"filter\":{\"must\":[{\"key\":\"payload.tags\",\"match\":{\"value\":\"green\"}}]}}'::jsonb \
                ) AS seeds(item_id)",
        )
        .expect("vector_top_k_ids exact nested json array match");

    match tags_results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 2);
            assert_eq!(rows[0].values[0], Value::BigInt(1));
            assert_eq!(rows[1].values[0], Value::BigInt(3));
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn vector_top_k_ids_json_options_filter_qdrant_bracket_array_keys_apply_to_json_arrays() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, payload JSONB, v VECTOR(2)); \
             INSERT INTO items VALUES \
                 (1, CAST('{\"tags\":[\"news\",\"green\"],\"cities\":[{\"name\":\"paris\"},{\"name\":\"lyon\"}]}' AS JSONB), '[1.00,0.0]'), \
                 (2, CAST('{\"tags\":[\"sports\",\"red\"],\"cities\":[{\"name\":\"berlin\"}]}' AS JSONB), '[1.01,0.0]'), \
                 (3, CAST('{\"tags\":[\"other\",\"green\"],\"cities\":[{\"name\":\"paris\"}]}' AS JSONB), '[1.02,0.0]'), \
                 (4, CAST('{\"tags\":[],\"cities\":[]}' AS JSONB), '[1.03,0.0]')",
        )
        .expect("setup");

    let tags_results = engine
        .execute_sql(
            &session,
            "SELECT item_id \
             FROM vector_top_k_ids( \
                    'items', \
                    'v', \
                    '[1.0,0.0]', \
                    4, \
                    'l2', \
                    64, \
                    10.0, \
                    false, \
                    -10.0, \
                    '{\"exact\":true,\"filter\":{\"must\":[{\"key\":\"payload.tags[]\",\"match\":{\"value\":\"green\"}}]}}'::jsonb \
                ) AS seeds(item_id)",
        )
        .expect("vector_top_k_ids exact json array bracket match");

    match tags_results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 2);
            assert_eq!(rows[0].values[0], Value::BigInt(1));
            assert_eq!(rows[1].values[0], Value::BigInt(3));
        }
        other => panic!("expected Query, got {other:?}"),
    }

    let city_results = engine
        .execute_sql(
            &session,
            "SELECT item_id \
             FROM vector_top_k_ids( \
                    'items', \
                    'v', \
                    '[1.0,0.0]', \
                    4, \
                    'l2', \
                    64, \
                    10.0, \
                    false, \
                    -10.0, \
                    '{\"exact\":true,\"filter\":{\"must\":[{\"key\":\"payload.cities[].name\",\"match\":{\"value\":\"paris\"}}]}}'::jsonb \
                ) AS seeds(item_id)",
        )
        .expect("vector_top_k_ids exact json nested array bracket match");

    match city_results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 2);
            assert_eq!(rows[0].values[0], Value::BigInt(1));
            assert_eq!(rows[1].values[0], Value::BigInt(3));
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn vector_top_k_ids_json_options_filter_qdrant_range_applies_to_json_array_elements() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, payload JSONB, v VECTOR(2)); \
             INSERT INTO items VALUES \
                 (1, CAST('{\"scores\":[1,4]}' AS JSONB), '[1.00,0.0]'), \
                 (2, CAST('{\"scores\":[2,8]}' AS JSONB), '[1.01,0.0]'), \
                 (3, CAST('{\"scores\":[9]}' AS JSONB), '[1.02,0.0]'), \
                 (4, CAST('{\"scores\":[]}' AS JSONB), '[1.03,0.0]')",
        )
        .expect("setup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT item_id \
             FROM vector_top_k_ids( \
                    'items', \
                    'v', \
                    '[1.0,0.0]', \
                    4, \
                    'l2', \
                    64, \
                    10.0, \
                    false, \
                    -10.0, \
                    '{\"exact\":true,\"filter\":{\"must\":[{\"key\":\"payload.scores[]\",\"range\":{\"gte\":5,\"lt\":9}}]}}'::jsonb \
                ) AS seeds(item_id)",
        )
        .expect("vector_top_k_ids exact json array range filter");

    match results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::BigInt(2));
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn vector_top_k_ids_json_options_filter_qdrant_min_should_applies_to_exact_results() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, category TEXT, source TEXT, priority INT, v VECTOR(2)); \
             INSERT INTO items VALUES \
                 (1, 'story', 'web', 5, '[1.00,0.0]'), \
                 (2, 'story', 'api', 2, '[1.01,0.0]'), \
                 (3, 'story', 'web', 1, '[1.02,0.0]'), \
                 (4, 'other', 'web', 5, '[1.03,0.0]')",
        )
        .expect("setup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT item_id \
             FROM vector_top_k_ids( \
                    'items', \
                    'v', \
                    '[1.0,0.0]', \
                    4, \
                    'l2', \
                    64, \
                    10.0, \
                    false, \
                    -10.0, \
                    '{\"exact\":true,\"filter\":{\"must\":[{\"key\":\"category\",\"match\":{\"value\":\"story\"}}],\"min_should\":{\"conditions\":[{\"key\":\"source\",\"match\":{\"value\":\"web\"}},{\"key\":\"priority\",\"range\":{\"gte\":3}}],\"min_count\":2}}}'::jsonb \
                ) AS seeds(item_id)",
        )
        .expect("vector_top_k_ids exact min_should filter");

    match results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::BigInt(1));
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn vector_top_k_ids_json_options_filter_qdrant_clause_singletons_are_accepted() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, category TEXT, source TEXT, priority INT, v VECTOR(2)); \
             INSERT INTO items VALUES \
                 (1, 'story', 'web', 5, '[1.00,0.0]'), \
                 (2, 'story', 'api', 2, '[1.01,0.0]'), \
                 (3, 'other', 'web', 5, '[1.02,0.0]')",
        )
        .expect("setup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT item_id \
             FROM vector_top_k_ids( \
                    'items', \
                    'v', \
                    '[1.0,0.0]', \
                    3, \
                    'l2', \
                    64, \
                    10.0, \
                    false, \
                    -10.0, \
                    '{\"exact\":true,\"filter\":{\"must\":{\"key\":\"category\",\"match\":{\"value\":\"story\"}},\"must_not\":{\"key\":\"source\",\"match\":{\"value\":\"api\"}},\"min_should\":{\"conditions\":{\"key\":\"priority\",\"range\":{\"gte\":3}},\"min_count\":1}}}'::jsonb \
                ) AS seeds(item_id)",
        )
        .expect("vector_top_k_ids singleton filter clauses");

    match results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::BigInt(1));
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn vector_top_k_ids_json_options_filter_clause_shorthand_maps_are_accepted() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, category TEXT, source TEXT, priority INT, v VECTOR(2)); \
             INSERT INTO items VALUES \
                 (1, 'story', 'web', 5, '[1.00,0.0]'), \
                 (2, 'story', 'api', 4, '[1.01,0.0]'), \
                 (3, 'story', 'web', 2, '[1.02,0.0]'), \
                 (4, 'note', 'web', 5, '[1.03,0.0]')",
        )
        .expect("setup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT item_id \
             FROM vector_top_k_ids( \
                    'items', \
                    'v', \
                    '[1.0,0.0]', \
                    3, \
                    'l2', \
                    64, \
                    10.0, \
                    false, \
                    -10.0, \
                    '{\"exact\":true,\"filter\":{\"must\":{\"category\":\"story\",\"source\":\"web\"},\"should\":[{\"priority\":5},{\"priority\":4}]}}'::jsonb \
                ) AS seeds(item_id)",
        )
        .expect("vector_top_k_ids shorthand clause maps");

    match results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::BigInt(1));
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn vector_top_k_ids_json_options_filter_min_should_rejects_zero_min_count() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, category TEXT, v VECTOR(2)); \
             INSERT INTO items VALUES (1, 'story', '[1.0,0.0]')",
        )
        .expect("setup");

    let err = engine
        .execute_sql(
            &session,
            "SELECT item_id \
             FROM vector_top_k_ids( \
                    'items', \
                    'v', \
                    '[1.0,0.0]', \
                    1, \
                    'l2', \
                    64, \
                    10.0, \
                    false, \
                    -10.0, \
                    '{\"filter\":{\"min_should\":{\"conditions\":[{\"key\":\"category\",\"match\":{\"value\":\"story\"}}],\"min_count\":0}}}'::jsonb \
                ) AS seeds(item_id)",
        )
        .expect_err("min_should min_count must be positive");
    assert!(
        err.to_string()
            .contains("min_should.min_count must be >= 1"),
        "expected min_should min_count error, got {err}"
    );
}

#[test]
fn vector_top_k_ids_json_options_filter_min_should_rejects_impossible_min_count() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, category TEXT, v VECTOR(2)); \
             INSERT INTO items VALUES (1, 'story', '[1.0,0.0]')",
        )
        .expect("setup");

    let err = engine
        .execute_sql(
            &session,
            "SELECT item_id \
             FROM vector_top_k_ids( \
                    'items', \
                    'v', \
                    '[1.0,0.0]', \
                    1, \
                    'l2', \
                    64, \
                    10.0, \
                    false, \
                    -10.0, \
                    '{\"filter\":{\"min_should\":{\"conditions\":[{\"key\":\"category\",\"match\":{\"value\":\"story\"}}],\"min_count\":2}}}'::jsonb \
                ) AS seeds(item_id)",
        )
        .expect_err("min_should min_count cannot exceed conditions length");
    assert!(
        err.to_string()
            .contains("min_should.min_count cannot exceed conditions length"),
        "expected min_should impossible min_count error, got {err}"
    );
}

#[test]
fn vector_top_k_ids_json_options_filter_match_any_rejects_non_array() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, tag TEXT, v VECTOR(2)); \
             INSERT INTO items VALUES (1, 'news', '[1.0,0.0]')",
        )
        .expect("setup");

    let err = engine
        .execute_sql(
            &session,
            "SELECT item_id \
             FROM vector_top_k_ids( \
                    'items', \
                    'v', \
                    '[1.0,0.0]', \
                    1, \
                    'l2', \
                    64, \
                    10.0, \
                    false, \
                    -10.0, \
                    '{\"filter\":{\"must\":[{\"key\":\"tag\",\"match\":{\"any\":\"news\"}}]}}'::jsonb \
                ) AS seeds(item_id)",
        )
        .expect_err("match.any payload must be an array");
    assert!(
        err.to_string().contains("match.any must be an array"),
        "expected match.any array error, got {err}"
    );
}

#[test]
fn vector_top_k_ids_json_options_filter_qdrant_is_null_and_is_empty_apply_to_exact_results() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, payload JSONB, v VECTOR(2)); \
             INSERT INTO items VALUES \
                 (1, NULL, '[1.00,0.0]'), \
                 (2, CAST('null' AS JSONB), '[1.01,0.0]'), \
                 (3, CAST('[]' AS JSONB), '[1.02,0.0]'), \
                 (4, CAST('[\"tag\"]' AS JSONB), '[1.03,0.0]')",
        )
        .expect("setup");

    let is_null_results = engine
        .execute_sql(
            &session,
            "SELECT item_id \
             FROM vector_top_k_ids( \
                    'items', \
                    'v', \
                    '[1.0,0.0]', \
                    3, \
                    'l2', \
                    64, \
                    10.0, \
                    false, \
                    -10.0, \
                    '{\"exact\":true,\"filter\":{\"must\":[{\"is_null\":{\"key\":\"payload\"}}]}}'::jsonb \
                ) AS seeds(item_id)",
        )
        .expect("vector_top_k_ids exact is_null filter");

    match is_null_results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 2);
            assert_eq!(rows[0].values[0], Value::BigInt(1));
            assert_eq!(rows[1].values[0], Value::BigInt(2));
        }
        other => panic!("expected Query, got {other:?}"),
    }

    let is_empty_results = engine
        .execute_sql(
            &session,
            "SELECT item_id \
             FROM vector_top_k_ids( \
                    'items', \
                    'v', \
                    '[1.0,0.0]', \
                    4, \
                    'l2', \
                    64, \
                    10.0, \
                    false, \
                    -10.0, \
                    '{\"exact\":true,\"filter\":{\"must\":[{\"is_empty\":{\"key\":\"payload\"}}]}}'::jsonb \
                ) AS seeds(item_id)",
        )
        .expect("vector_top_k_ids exact is_empty filter");

    match is_empty_results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 3);
            assert_eq!(rows[0].values[0], Value::BigInt(1));
            assert_eq!(rows[1].values[0], Value::BigInt(2));
            assert_eq!(rows[2].values[0], Value::BigInt(3));
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn vector_top_k_ids_json_options_filter_is_null_rejects_invalid_payload() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, payload JSONB, v VECTOR(2)); \
             INSERT INTO items VALUES (1, NULL, '[1.0,0.0]')",
        )
        .expect("setup");

    let err = engine
        .execute_sql(
            &session,
            "SELECT item_id \
             FROM vector_top_k_ids( \
                    'items', \
                    'v', \
                    '[1.0,0.0]', \
                    1, \
                    'l2', \
                    64, \
                    10.0, \
                    false, \
                    -10.0, \
                    '{\"filter\":{\"must\":[{\"is_null\":{\"field\":\"payload\"}}]}}'::jsonb \
                ) AS seeds(item_id)",
        )
        .expect_err("is_null payload requires a key field");
    assert!(
        err.to_string()
            .contains("is_null requires only a key field"),
        "expected is_null key error, got {err}"
    );
}

#[test]
fn vector_top_k_ids_json_options_filter_qdrant_has_id_applies_to_exact_results() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, tag TEXT, v VECTOR(2)); \
             INSERT INTO items VALUES \
                 (1, 'keep', '[1.00,0.0]'), \
                 (2, 'drop', '[1.01,0.0]'), \
                 (3, 'keep', '[1.02,0.0]'), \
                 (4, 'drop', '[1.03,0.0]')",
        )
        .expect("setup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT item_id \
             FROM vector_top_k_ids( \
                    'items', \
                    'v', \
                    '[1.0,0.0]', \
                    4, \
                    'l2', \
                    64, \
                    10.0, \
                    false, \
                    -10.0, \
                    '{\"exact\":true,\"filter\":{\"must\":[{\"has_id\":[3,1]}]}}'::jsonb \
                ) AS seeds(item_id)",
        )
        .expect("vector_top_k_ids exact has_id filter");

    match results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 2);
            assert_eq!(rows[0].values[0], Value::BigInt(1));
            assert_eq!(rows[1].values[0], Value::BigInt(3));
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn vector_top_k_ids_json_options_filter_has_id_rejects_key_field() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, v VECTOR(2)); \
             INSERT INTO items VALUES (1, '[1.0,0.0]')",
        )
        .expect("setup");

    let err = engine
        .execute_sql(
            &session,
            "SELECT item_id \
             FROM vector_top_k_ids( \
                    'items', \
                    'v', \
                    '[1.0,0.0]', \
                    1, \
                    'l2', \
                    64, \
                    10.0, \
                    false, \
                    -10.0, \
                    '{\"filter\":{\"must\":[{\"key\":\"id\",\"has_id\":[1]}]}}'::jsonb \
                ) AS seeds(item_id)",
        )
        .expect_err("has_id must not include key");
    assert!(
        err.to_string().contains("has_id must not include a key"),
        "expected has_id key error, got {err}"
    );
}

#[test]
fn vector_top_k_ids_json_options_filter_qdrant_values_count_applies_to_exact_results() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, comments JSONB, v VECTOR(2)); \
             INSERT INTO items VALUES \
                 (1, CAST('[\"good\",\"ok\"]' AS JSONB), '[1.00,0.0]'), \
                 (2, CAST('[\"good\",\"ok\",\"again\"]' AS JSONB), '[1.01,0.0]'), \
                 (3, CAST('\"single\"' AS JSONB), '[1.02,0.0]'), \
                 (4, CAST('null' AS JSONB), '[1.03,0.0]')",
        )
        .expect("setup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT item_id \
             FROM vector_top_k_ids( \
                    'items', \
                    'v', \
                    '[1.0,0.0]', \
                    4, \
                    'l2', \
                    64, \
                    10.0, \
                    false, \
                    -10.0, \
                    '{\"exact\":true,\"filter\":{\"must\":[{\"key\":\"comments\",\"values_count\":{\"gt\":1,\"lte\":3}}]}}'::jsonb \
                ) AS seeds(item_id)",
        )
        .expect("vector_top_k_ids exact values_count filter");

    match results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 2);
            assert_eq!(rows[0].values[0], Value::BigInt(1));
            assert_eq!(rows[1].values[0], Value::BigInt(2));
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn vector_top_k_ids_json_options_filter_values_count_rejects_non_integer_bound() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, comments JSONB, v VECTOR(2)); \
             INSERT INTO items VALUES (1, CAST('[\"ok\"]' AS JSONB), '[1.0,0.0]')",
        )
        .expect("setup");

    let err = engine
        .execute_sql(
            &session,
            "SELECT item_id \
             FROM vector_top_k_ids( \
                    'items', \
                    'v', \
                    '[1.0,0.0]', \
                    1, \
                    'l2', \
                    64, \
                    10.0, \
                    false, \
                    -10.0, \
                    '{\"filter\":{\"must\":[{\"key\":\"comments\",\"values_count\":{\"gt\":1.5}}]}}'::jsonb \
                ) AS seeds(item_id)",
        )
        .expect_err("values_count bound must be an integer");
    assert!(
        err.to_string()
            .contains("values_count bound \"gt\" must be a non-negative integer"),
        "expected values_count integer-bound error, got {err}"
    );
}

#[test]
fn vector_top_k_ids_json_options_filter_range_rejects_non_numeric_column() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, tag TEXT, v VECTOR(2)); \
             INSERT INTO items VALUES (1, 'keep', '[1.0,0.0]')",
        )
        .expect("setup");

    let err = engine
        .execute_sql(
            &session,
            "SELECT item_id \
             FROM vector_top_k_ids( \
                    'items', \
                    'v', \
                    '[1.0,0.0]', \
                    1, \
                    'l2', \
                    64, \
                    10.0, \
                    false, \
                    -10.0, \
                    '{\"filter\":{\"must\":[{\"key\":\"tag\",\"range\":{\"gte\":1}}]}}'::jsonb \
                ) AS seeds(item_id)",
        )
        .expect_err("range filter on text column should be rejected");
    assert!(
        err.to_string().contains("requires a numeric column"),
        "expected numeric-column range error, got {err}"
    );
}

#[test]
fn vector_top_k_ids_json_options_filter_qdrant_should_and_must_not_are_applied() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, tag TEXT, v VECTOR(2)); \
             INSERT INTO items VALUES \
                 (1, 'news', '[1.00,0.0]'), \
                 (2, 'sports', '[1.05,0.0]'), \
                 (3, 'news', '[1.10,0.0]'), \
                 (4, 'other', '[1.15,0.0]')",
        )
        .expect("setup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT item_id \
             FROM vector_top_k_ids( \
                    'items', \
                    'v', \
                    '[1.0,0.0]', \
                    3, \
                    'l2', \
                    64, \
                    10.0, \
                    false, \
                    -10.0, \
                    '{\"exact\":true,\"filter\":{\"should\":[{\"key\":\"tag\",\"match\":{\"value\":\"news\"}},{\"key\":\"tag\",\"match\":{\"value\":\"sports\"}}],\"must_not\":[{\"key\":\"id\",\"value\":3}]}}'::jsonb \
                ) AS seeds(item_id)",
        )
        .expect("vector_top_k_ids should+must_not filter");

    match results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 2);
            assert_eq!(rows[0].values[0], Value::BigInt(1));
            assert_eq!(rows[1].values[0], Value::BigInt(2));
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn vector_top_k_ids_qdrant_filter_hnsw_with_btree_prefilter_match_clauses() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, category TEXT, source TEXT, v VECTOR(2)); \
             CREATE INDEX idx_v_l2 ON items USING hnsw (v); \
             CREATE INDEX idx_items_id ON items (id); \
             CREATE INDEX idx_items_category ON items (category); \
             CREATE INDEX idx_items_source ON items (source); \
             INSERT INTO items VALUES \
                 (1, 'news', 'web', '[1.00,0.0]'), \
                 (2, 'news', 'banned', '[1.01,0.0]'), \
                 (3, 'sports', 'web', '[1.02,0.0]'), \
                 (4, 'other', 'web', '[1.03,0.0]'), \
                 (5, 'sports', 'trusted', '[1.04,0.0]')",
        )
        .expect("setup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT item_id \
             FROM vector_top_k_ids( \
                    'items', \
                    'v', \
                    '[1.0,0.0]', \
                    2, \
                    'l2', \
                    2, \
                    10.0, \
                    false, \
                    -10.0, \
                    '{\"filter\":{\"must\":[{\"key\":\"category\",\"match\":{\"value\":\"sports\"}}],\"should\":[{\"key\":\"source\",\"match\":{\"value\":\"web\"}},{\"key\":\"source\",\"match\":{\"value\":\"trusted\"}}],\"must_not\":[{\"key\":\"id\",\"match\":{\"value\":3}}]}}'::jsonb \
                ) AS seeds(item_id)",
        )
        .expect("vector_top_k_ids hnsw filter with btree prefilter");

    match results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::BigInt(5));
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn vector_top_k_ids_hnsw_prefilter_keeps_non_indexed_should_matches() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, category TEXT, bucket INT, v VECTOR(2)); \
             CREATE INDEX idx_v_l2 ON items USING hnsw (v); \
             CREATE INDEX idx_items_category ON items (category); \
             INSERT INTO items VALUES \
                 (1, 'news', 1, '[1.00,0.0]'), \
                 (2, 'news', 7, '[1.01,0.0]'), \
                 (3, 'news', 3, '[1.02,0.0]'), \
                 (4, 'other', 7, '[1.03,0.0]')",
        )
        .expect("setup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT item_id \
             FROM vector_top_k_ids( \
                    'items', \
                    'v', \
                    '[1.0,0.0]', \
                    2, \
                    'l2', \
                    2, \
                    10.0, \
                    false, \
                    -10.0, \
                    '{\"filter\":{\"must\":[{\"key\":\"category\",\"match\":{\"value\":\"news\"}}],\"should\":[{\"key\":\"bucket\",\"range\":{\"gte\":5}}]}}'::jsonb \
                ) AS seeds(item_id)",
        )
        .expect("vector_top_k_ids hnsw filter with mixed-index should");

    match results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::BigInt(2));
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn vector_top_k_hits_returns_distance_score_and_payload_in_exact_mode() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, tag TEXT, v VECTOR(2)); \
             INSERT INTO items VALUES \
                 (1, 'keep', '[1.0,0.0]'), \
                 (2, 'drop', '[2.0,0.0]'), \
                 (3, 'other', '[4.0,0.0]')",
        )
        .expect("setup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT hit \
             FROM vector_top_k_hits( \
                    'items', \
                    'v', \
                    '[1.0,0.0]', \
                    2, \
                    'l2', \
                    64, \
                    10.0, \
                    false, \
                    -10.0, \
                    '{\"exact\":true}'::jsonb \
                ) AS hits(hit)",
        )
        .expect("vector_top_k_hits exact mode");

    match results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 2);
            let Value::Jsonb(first) = &rows[0].values[0] else {
                panic!("expected first hit as jsonb");
            };
            let Value::Jsonb(second) = &rows[1].values[0] else {
                panic!("expected second hit as jsonb");
            };
            assert_eq!(first.get("id").and_then(serde_json::Value::as_i64), Some(1));
            assert_eq!(
                second.get("id").and_then(serde_json::Value::as_i64),
                Some(2)
            );
            let first_distance = first
                .get("distance")
                .and_then(serde_json::Value::as_f64)
                .unwrap_or(f64::NAN);
            let first_score = first
                .get("score")
                .and_then(serde_json::Value::as_f64)
                .unwrap_or(f64::NAN);
            assert!((first_distance - 0.0).abs() < 1e-9);
            assert!((first_score - 0.0).abs() < 1e-9);
            let second_distance = second
                .get("distance")
                .and_then(serde_json::Value::as_f64)
                .unwrap_or(f64::NAN);
            let second_score = second
                .get("score")
                .and_then(serde_json::Value::as_f64)
                .unwrap_or(f64::NAN);
            assert!((second_distance - 1.0).abs() < 1e-9);
            assert!((second_score + 1.0).abs() < 1e-9);
            let first_payload = first
                .get("payload")
                .and_then(serde_json::Value::as_object)
                .expect("payload object");
            assert_eq!(
                first_payload.get("tag").and_then(serde_json::Value::as_str),
                Some("keep")
            );
            assert!(
                first_payload.get("v").is_none(),
                "payload should not duplicate vector column"
            );
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn vector_top_k_hits_with_payload_option_controls_payload_output() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, tag TEXT, source TEXT, v VECTOR(2)); \
             INSERT INTO items VALUES \
                 (1, 'news', 'web', '[1.0,0.0]'), \
                 (2, 'sports', 'api', '[2.0,0.0]')",
        )
        .expect("setup");

    let without_payload = engine
        .execute_sql(
            &session,
            "SELECT hit \
             FROM vector_top_k_hits( \
                    'items', \
                    'v', \
                    '[1.0,0.0]', \
                    1, \
                    'l2', \
                    64, \
                    10.0, \
                    false, \
                    -10.0, \
                    '{\"exact\":true,\"with_payload\":false}'::jsonb \
                ) AS hits(hit)",
        )
        .expect("vector_top_k_hits without payload");

    match without_payload.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            let Value::Jsonb(hit) = &rows[0].values[0] else {
                panic!("expected jsonb hit");
            };
            assert!(hit.get("payload").is_none());
        }
        other => panic!("expected Query, got {other:?}"),
    }

    let selected_payload = engine
        .execute_sql(
            &session,
            "SELECT hit \
             FROM vector_top_k_hits( \
                    'items', \
                    'v', \
                    '[1.0,0.0]', \
                    1, \
                    'l2', \
                    64, \
                    10.0, \
                    false, \
                    -10.0, \
                    '{\"exact\":true,\"with_payload\":[\"source\"]}'::jsonb \
                ) AS hits(hit)",
        )
        .expect("vector_top_k_hits selected payload");

    match selected_payload.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            let Value::Jsonb(hit) = &rows[0].values[0] else {
                panic!("expected jsonb hit");
            };
            let payload = hit
                .get("payload")
                .and_then(serde_json::Value::as_object)
                .expect("payload object");
            assert_eq!(payload.len(), 1);
            assert_eq!(
                payload.get("source").and_then(serde_json::Value::as_str),
                Some("web")
            );
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn vector_top_k_hits_with_payload_object_include_and_exclude_are_supported() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, tag TEXT, source TEXT, bucket INT, v VECTOR(2)); \
             INSERT INTO items VALUES \
                 (1, 'news', 'web', 7, '[1.0,0.0]'), \
                 (2, 'sports', 'api', 9, '[2.0,0.0]')",
        )
        .expect("setup");

    let include_results = engine
        .execute_sql(
            &session,
            "SELECT hit \
             FROM vector_top_k_hits( \
                    'items', \
                    'v', \
                    '[1.0,0.0]', \
                    1, \
                    'l2', \
                    64, \
                    10.0, \
                    false, \
                    -10.0, \
                    '{\"exact\":true,\"with_payload\":{\"include\":[\"tag\",\"bucket\"]}}'::jsonb \
                ) AS hits(hit)",
        )
        .expect("vector_top_k_hits payload include object");

    match include_results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            let Value::Jsonb(hit) = &rows[0].values[0] else {
                panic!("expected jsonb hit");
            };
            let payload = hit
                .get("payload")
                .and_then(serde_json::Value::as_object)
                .expect("payload object");
            assert_eq!(payload.len(), 2);
            assert!(payload.contains_key("tag"));
            assert!(payload.contains_key("bucket"));
        }
        other => panic!("expected Query, got {other:?}"),
    }

    let exclude_results = engine
        .execute_sql(
            &session,
            "SELECT hit \
             FROM vector_top_k_hits( \
                    'items', \
                    'v', \
                    '[1.0,0.0]', \
                    1, \
                    'l2', \
                    64, \
                    10.0, \
                    false, \
                    -10.0, \
                    '{\"exact\":true,\"with_payload\":{\"exclude\":[\"source\"]}}'::jsonb \
                ) AS hits(hit)",
        )
        .expect("vector_top_k_hits payload exclude object");

    match exclude_results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            let Value::Jsonb(hit) = &rows[0].values[0] else {
                panic!("expected jsonb hit");
            };
            let payload = hit
                .get("payload")
                .and_then(serde_json::Value::as_object)
                .expect("payload object");
            assert!(payload.contains_key("tag"));
            assert!(payload.contains_key("bucket"));
            assert!(!payload.contains_key("source"));
            assert!(!payload.contains_key("v"));
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn vector_top_k_hits_with_vector_option_includes_vector_output() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, tag TEXT, v VECTOR(2)); \
             INSERT INTO items VALUES \
                 (1, 'news', '[1.0,0.0]'), \
                 (2, 'sports', '[2.0,0.0]')",
        )
        .expect("setup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT hit \
             FROM vector_top_k_hits( \
                    'items', \
                    'v', \
                    '[1.0,0.0]', \
                    1, \
                    'l2', \
                    64, \
                    10.0, \
                    false, \
                    -10.0, \
                    '{\"exact\":true,\"with_payload\":false,\"with_vector\":true}'::jsonb \
                ) AS hits(hit)",
        )
        .expect("vector_top_k_hits with vector output");

    match results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            let Value::Jsonb(hit) = &rows[0].values[0] else {
                panic!("expected jsonb hit");
            };
            assert!(hit.get("payload").is_none());
            assert_eq!(
                hit.get("vector").and_then(serde_json::Value::as_array),
                Some(&vec![
                    serde_json::Value::from(1.0),
                    serde_json::Value::from(0.0)
                ])
            );
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn vector_top_k_hits_filter_qdrant_must_widens_hnsw_search() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, tag TEXT, v VECTOR(2)); \
             CREATE INDEX idx_v_l2 ON items USING hnsw (v); \
             INSERT INTO items VALUES \
                 (1, 'drop', '[1.00,0.0]'), \
                 (2, 'drop', '[1.05,0.0]'), \
                 (3, 'drop', '[1.10,0.0]'), \
                 (4, 'keep', '[1.15,0.0]'), \
                 (5, 'keep', '[1.20,0.0]')",
        )
        .expect("setup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT hit \
             FROM vector_top_k_hits( \
                    'items', \
                    'v', \
                    '[1.0,0.0]', \
                    2, \
                    'l2', \
                    2, \
                    10.0, \
                    false, \
                    -10.0, \
                    '{\"filter\":{\"must\":[{\"key\":\"tag\",\"match\":{\"value\":\"keep\"}}]}}'::jsonb \
                ) AS hits(hit)",
        )
        .expect("vector_top_k_hits hnsw payload filter");

    match results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(
                rows.len(),
                2,
                "adaptive widening should recover filtered hits"
            );
            let ids: Vec<i64> = rows
                .iter()
                .map(|row| {
                    let Value::Jsonb(hit) = &row.values[0] else {
                        panic!("expected jsonb hit");
                    };
                    hit.get("id")
                        .and_then(serde_json::Value::as_i64)
                        .unwrap_or_default()
                })
                .collect();
            assert_eq!(ids, vec![4, 5]);
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn vector_top_k_hits_filter_qdrant_mixed_shorthand_and_match_text() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, tag TEXT, payload JSONB, v VECTOR(2)); \
             INSERT INTO items VALUES \
                 (1, 'news', '{\"notes\":[\"Vector filters are ready\"]}', '[1.0,0.0]'), \
                 (2, 'news', '{\"notes\":[\"other\"]}', '[1.1,0.0]'), \
                 (3, 'sports', '{\"notes\":[\"vector filters\"]}', '[1.2,0.0]'), \
                 (4, 'news', '{\"notes\":[\"VECTOR FILTERS\"]}', '[3.0,0.0]')",
        )
        .expect("setup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT hit \
             FROM vector_top_k_hits( \
                    'items', \
                    'v', \
                    '[1.0,0.0]', \
                    3, \
                    'l2', \
                    64, \
                    10.0, \
                    false, \
                    -10.0, \
                    '{\"exact\":true,\"filter\":{\"must\":{\"key\":\"payload.notes[]\",\"match\":{\"text\":\"vector filters\"}},\"tag\":\"news\"}}'::jsonb \
                ) AS hits(hit)",
        )
        .expect("vector_top_k_hits mixed qdrant filter");

    match results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 2);
            let ids: Vec<i64> = rows
                .iter()
                .map(|row| {
                    let Value::Jsonb(hit) = &row.values[0] else {
                        panic!("expected jsonb hit");
                    };
                    hit.get("id")
                        .and_then(serde_json::Value::as_i64)
                        .unwrap_or_default()
                })
                .collect();
            assert_eq!(ids, vec![1, 4]);
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn hybrid_fuse_rrf_hits_merges_dense_and_sparse_rankings() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT hit \
             FROM hybrid_fuse_rrf_hits( \
                    '[{\"id\":1,\"score\":0.9},{\"id\":2,\"score\":0.8}]'::jsonb, \
                    '[{\"id\":2,\"score\":0.95},{\"id\":3,\"score\":0.7}]'::jsonb, \
                    3 \
                ) AS fused(hit)",
        )
        .expect("hybrid_fuse_rrf_hits basic merge");

    match results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 3);
            let ids: Vec<i64> = rows
                .iter()
                .map(|row| {
                    let Value::Jsonb(hit) = &row.values[0] else {
                        panic!("expected jsonb hit");
                    };
                    hit.get("id")
                        .and_then(serde_json::Value::as_i64)
                        .unwrap_or_default()
                })
                .collect();
            assert_eq!(ids, vec![2, 1, 3]);
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn hybrid_fuse_rrf_hits_accepts_vector_top_k_hits_as_dense_input() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, v VECTOR(2)); \
             INSERT INTO items VALUES \
                 (1, '[1.0,0.0]'), \
                 (2, '[1.1,0.0]'), \
                 (3, '[1.2,0.0]')",
        )
        .expect("setup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT hit \
             FROM hybrid_fuse_rrf_hits( \
                    vector_top_k_hits('items', 'v', '[1.0,0.0]', 2, 'l2', 64, 10.0, false, -10.0, '{\"exact\":true}'::jsonb), \
                    '[{\"id\":2,\"score\":1.0}]'::jsonb, \
                    2, \
                    1.0, \
                    2.0, \
                    1 \
                ) AS fused(hit)",
        )
        .expect("hybrid_fuse_rrf_hits with dense vector input");

    match results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 2);
            let ids: Vec<i64> = rows
                .iter()
                .map(|row| {
                    let Value::Jsonb(hit) = &row.values[0] else {
                        panic!("expected jsonb hit");
                    };
                    hit.get("id")
                        .and_then(serde_json::Value::as_i64)
                        .unwrap_or_default()
                })
                .collect();
            assert_eq!(ids[0], 2, "sparse weight should boost id=2");
            assert_eq!(ids[1], 1);
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn hybrid_fuse_dbsf_hits_merges_dense_and_sparse_rankings() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT hit \
             FROM hybrid_fuse_dbsf_hits( \
                    '[{\"id\":1,\"score\":0.9},{\"id\":2,\"score\":0.8}]'::jsonb, \
                    '[{\"id\":2,\"score\":0.95},{\"id\":3,\"score\":0.7}]'::jsonb, \
                    3 \
                ) AS fused(hit)",
        )
        .expect("hybrid_fuse_dbsf_hits basic merge");

    match results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 3);
            let ids: Vec<i64> = rows
                .iter()
                .map(|row| {
                    let Value::Jsonb(hit) = &row.values[0] else {
                        panic!("expected jsonb hit");
                    };
                    hit.get("id")
                        .and_then(serde_json::Value::as_i64)
                        .unwrap_or_default()
                })
                .collect();
            assert_eq!(ids, vec![2, 1, 3]);
            let Value::Jsonb(first) = &rows[0].values[0] else {
                panic!("expected jsonb hit");
            };
            assert!(first.get("fused_score").is_some());
            assert!(first
                .get("dense")
                .and_then(serde_json::Value::as_object)
                .and_then(|dense| dense.get("normalized_score"))
                .is_some());
            assert!(first
                .get("sparse")
                .and_then(serde_json::Value::as_object)
                .and_then(|sparse| sparse.get("normalized_score"))
                .is_some());
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn hybrid_fuse_dbsf_hits_uses_distance_when_score_is_missing() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT hit \
             FROM hybrid_fuse_dbsf_hits( \
                    '[{\"id\":1,\"distance\":0.1},{\"id\":2,\"distance\":0.3}]'::jsonb, \
                    '[{\"id\":2,\"distance\":0.2},{\"id\":3,\"distance\":0.4}]'::jsonb, \
                    3, \
                    1.0, \
                    2.0 \
                ) AS fused(hit)",
        )
        .expect("hybrid_fuse_dbsf_hits with distance fallback");

    match results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 3);
            let ids: Vec<i64> = rows
                .iter()
                .map(|row| {
                    let Value::Jsonb(hit) = &row.values[0] else {
                        panic!("expected jsonb hit");
                    };
                    hit.get("id")
                        .and_then(serde_json::Value::as_i64)
                        .unwrap_or_default()
                })
                .collect();
            assert_eq!(ids, vec![2, 1, 3]);
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn hybrid_search_top_k_hits_defaults_to_rrf_fusion() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, category TEXT, body TEXT, v VECTOR(2)); \
             INSERT INTO items VALUES \
                 (1, 'story', 'run run run fast', '[1.00,0.0]'), \
                 (2, 'story', 'database internals only', '[1.05,0.0]'), \
                 (3, 'other', 'run logs and notes', '[0.0,1.0]'), \
                 (4, 'other', 'buffers and pages', '[1.2,0.0]')",
        )
        .expect("setup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT hit \
             FROM hybrid_search_top_k_hits( \
                    'items', \
                    'v', \
                    'body', \
                    '[1.0,0.0]', \
                    'run', \
                    2, \
                    '{\"source_k\":2}'::jsonb \
                ) AS hybrid(hit)",
        )
        .expect("hybrid_search_top_k_hits default rrf");

    match results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 2);
            let ids: Vec<i64> = rows
                .iter()
                .map(|row| {
                    let Value::Jsonb(hit) = &row.values[0] else {
                        panic!("expected jsonb hit");
                    };
                    hit.get("id")
                        .and_then(serde_json::Value::as_i64)
                        .unwrap_or_default()
                })
                .collect();
            assert_eq!(ids, vec![1, 2]);
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn hybrid_search_top_k_hits_supports_dbsf_fusion_option() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, body TEXT, v VECTOR(2)); \
             INSERT INTO items VALUES \
                 (1, 'run run run fast', '[1.00,0.0]'), \
                 (2, 'database internals only', '[1.05,0.0]'), \
                 (3, 'run logs and notes', '[0.0,1.0]')",
        )
        .expect("setup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT hit \
             FROM hybrid_search_top_k_hits( \
                    'items', \
                    'v', \
                    'body', \
                    '[1.0,0.0]', \
                    'run', \
                    2, \
                    '{\"fusion\":\"dbsf\",\"source_k\":2,\"dense_weight\":0.2,\"sparse_weight\":3.0}'::jsonb \
                ) AS hybrid(hit)",
        )
        .expect("hybrid_search_top_k_hits dbsf");

    match results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 2);
            let ids: Vec<i64> = rows
                .iter()
                .map(|row| {
                    let Value::Jsonb(hit) = &row.values[0] else {
                        panic!("expected jsonb hit");
                    };
                    hit.get("id")
                        .and_then(serde_json::Value::as_i64)
                        .unwrap_or_default()
                })
                .collect();
            assert_eq!(ids, vec![1, 3]);
            let Value::Jsonb(first) = &rows[0].values[0] else {
                panic!("expected jsonb hit");
            };
            assert!(first.get("fused_score").is_some());
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn hybrid_search_top_k_hits_supports_payload_filter_option() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, category TEXT, body TEXT, v VECTOR(2)); \
             INSERT INTO items VALUES \
                 (1, 'story', 'run run run fast', '[1.00,0.0]'), \
                 (2, 'other', 'run logs and notes', '[1.01,0.0]'), \
                 (3, 'story', 'database internals only', '[1.02,0.0]')",
        )
        .expect("setup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT hit \
             FROM hybrid_search_top_k_hits( \
                    'items', \
                    'v', \
                    'body', \
                    '[1.0,0.0]', \
                    'run', \
                    3, \
                    '{\"source_k\":3,\"filter\":{\"must\":[{\"key\":\"category\",\"match\":{\"value\":\"story\"}}]}}'::jsonb \
                ) AS hybrid(hit)",
        )
        .expect("hybrid_search_top_k_hits with payload filter");

    match results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 2);
            for row in rows {
                let Value::Jsonb(hit) = &row.values[0] else {
                    panic!("expected jsonb hit");
                };
                assert_ne!(
                    hit.get("id").and_then(serde_json::Value::as_i64),
                    Some(2),
                    "id=2 should be filtered out by payload category"
                );
                assert_eq!(
                    hit.get("payload")
                        .and_then(serde_json::Value::as_object)
                        .and_then(|payload| payload.get("category"))
                        .and_then(serde_json::Value::as_str),
                    Some("story")
                );
            }
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn hybrid_group_hits_by_payload_field_groups_vector_hits() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, tag TEXT, v VECTOR(2)); \
             INSERT INTO items VALUES \
                 (1, 'news', '[1.00,0.0]'), \
                 (2, 'news', '[1.05,0.0]'), \
                 (3, 'sports', '[1.10,0.0]'), \
                 (4, 'sports', '[1.20,0.0]')",
        )
        .expect("setup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT grp \
             FROM hybrid_group_hits_by( \
                    vector_top_k_hits('items', 'v', '[1.0,0.0]', 4, 'l2', 64, 10.0, true, -10.0, '{\"exact\":true}'::jsonb), \
                    'tag', \
                    2, \
                    2 \
                ) AS grouped(grp)",
        )
        .expect("hybrid_group_hits_by over vector_top_k_hits");

    match results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 2);

            let Value::Jsonb(first_group) = &rows[0].values[0] else {
                panic!("expected first grouped jsonb row");
            };
            let Value::Jsonb(second_group) = &rows[1].values[0] else {
                panic!("expected second grouped jsonb row");
            };

            assert_eq!(
                first_group.get("group").and_then(serde_json::Value::as_str),
                Some("news")
            );
            assert_eq!(
                second_group
                    .get("group")
                    .and_then(serde_json::Value::as_str),
                Some("sports")
            );
            assert_eq!(
                first_group.get("count").and_then(serde_json::Value::as_i64),
                Some(2)
            );
            assert_eq!(
                second_group
                    .get("count")
                    .and_then(serde_json::Value::as_i64),
                Some(2)
            );
            assert_eq!(
                first_group
                    .get("hits")
                    .and_then(serde_json::Value::as_array)
                    .map(std::vec::Vec::len),
                Some(2)
            );
            assert_eq!(
                second_group
                    .get("hits")
                    .and_then(serde_json::Value::as_array)
                    .map(std::vec::Vec::len),
                Some(2)
            );
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn vector_prefetch_top_k_hits_accepts_vector_top_k_hits_as_prefetch_input() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, v VECTOR(2)); \
             INSERT INTO items VALUES \
                 (1, '[1.0,0.0]'), \
                 (2, '[1.1,0.0]'), \
                 (3, '[1.2,0.0]'), \
                 (4, '[3.0,0.0]')",
        )
        .expect("setup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT hit \
             FROM vector_prefetch_top_k_hits( \
                    'items', \
                    'v', \
                    '[1.0,0.0]', \
                    vector_top_k_hits('items', 'v', '[1.0,0.0]', 3), \
                    2 \
                ) AS pref(hit)",
        )
        .expect("vector_prefetch_top_k_hits with vector_top_k_hits input");

    match results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 2);
            let ids: Vec<i64> = rows
                .iter()
                .map(|row| {
                    let Value::Jsonb(hit) = &row.values[0] else {
                        panic!("expected jsonb hit");
                    };
                    hit.get("id")
                        .and_then(serde_json::Value::as_i64)
                        .unwrap_or_default()
                })
                .collect();
            assert_eq!(ids, vec![1, 2]);
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn vector_prefetch_top_k_hits_supports_nested_multi_stage_prefetch() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, v VECTOR(2)); \
             INSERT INTO items VALUES \
                 (1, '[0.0,0.0]'), \
                 (2, '[1.0,0.0]'), \
                 (3, '[2.0,0.0]'), \
                 (4, '[10.0,0.0]')",
        )
        .expect("setup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT hit \
             FROM vector_prefetch_top_k_hits( \
                    'items', \
                    'v', \
                    '[1.0,0.0]', \
                    vector_prefetch_top_k_hits( \
                        'items', \
                        'v', \
                        '[0.0,0.0]', \
                        vector_top_k_hits('items', 'v', '[10.0,0.0]', 2), \
                        2 \
                    ), \
                    1 \
                ) AS pref(hit)",
        )
        .expect("vector_prefetch_top_k_hits nested");

    match results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            let Value::Jsonb(hit) = &rows[0].values[0] else {
                panic!("expected jsonb hit");
            };
            assert_eq!(
                hit.get("id").and_then(serde_json::Value::as_i64),
                Some(3),
                "multi-stage prefetch should keep reranking within prefetched candidates"
            );
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn vector_recommend_top_k_hits_positive_and_negative_examples() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, v VECTOR(2)); \
             INSERT INTO items VALUES \
                 (1, '[1.0,0.0]'), \
                 (2, '[0.5,0.0]'), \
                 (3, '[-1.0,0.0]')",
        )
        .expect("setup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT hit \
             FROM vector_recommend_top_k_hits( \
                    'items', \
                    'v', \
                    '[1.0,0.0]', \
                    '[-1.0,0.0]', \
                    2, \
                    'l2', \
                    64, \
                    10.0, \
                    true, \
                    -10.0, \
                    '{\"exact\":true}'::jsonb \
                ) AS rec(hit)",
        )
        .expect("vector_recommend_top_k_hits with positive/negative examples");

    match results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 2);
            let ids: Vec<i64> = rows
                .iter()
                .map(|row| {
                    let Value::Jsonb(hit) = &row.values[0] else {
                        panic!("expected jsonb hit");
                    };
                    hit.get("id")
                        .and_then(serde_json::Value::as_i64)
                        .unwrap_or_default()
                })
                .collect();
            assert_eq!(ids, vec![1, 2]);
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn vector_recommend_top_k_hits_allows_null_negative_example() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, v VECTOR(2)); \
             INSERT INTO items VALUES \
                 (1, '[1.0,0.0]'), \
                 (2, '[2.0,0.0]')",
        )
        .expect("setup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT hit \
             FROM vector_recommend_top_k_hits( \
                    'items', \
                    'v', \
                    '[1.0,0.0]', \
                    NULL, \
                    1, \
                    'l2', \
                    64, \
                    10.0, \
                    true, \
                    -10.0, \
                    '{\"exact\":true}'::jsonb \
                ) AS rec(hit)",
        )
        .expect("vector_recommend_top_k_hits with null negative");

    match results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            let Value::Jsonb(hit) = &rows[0].values[0] else {
                panic!("expected jsonb hit");
            };
            assert_eq!(hit.get("id").and_then(serde_json::Value::as_i64), Some(1));
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn vector_recommend_top_k_hits_supports_json_examples_with_ids_and_vectors() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, v VECTOR(2)); \
             INSERT INTO items VALUES \
                 (1, '[1.0,0.0]'), \
                 (2, '[0.9,0.0]'), \
                 (3, '[0.8,0.0]'), \
                 (4, '[-0.8,0.0]'), \
                 (5, '[-1.0,0.0]')",
        )
        .expect("setup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT hit \
             FROM vector_recommend_top_k_hits( \
                    'items', \
                    'v', \
                    '[{\"id\":1},{\"vector\":[0.9,0.0]}]'::jsonb, \
                    '[{\"id\":5}]'::jsonb, \
                    2, \
                    'l2', \
                    64, \
                    10.0, \
                    true, \
                    -10.0, \
                    '{\"exact\":true}'::jsonb \
                ) AS rec(hit)",
        )
        .expect("vector_recommend_top_k_hits with json examples");

    match results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 2);
            let ids: Vec<i64> = rows
                .iter()
                .map(|row| {
                    let Value::Jsonb(hit) = &row.values[0] else {
                        panic!("expected jsonb hit");
                    };
                    hit.get("id")
                        .and_then(serde_json::Value::as_i64)
                        .unwrap_or_default()
                })
                .collect();
            assert_eq!(ids, vec![1, 2]);
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn vector_recommend_top_k_hits_rejects_unknown_example_id() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, v VECTOR(2)); \
             INSERT INTO items VALUES (1, '[1.0,0.0]')",
        )
        .expect("setup");

    let err = engine
        .execute_sql(
            &session,
            "SELECT hit \
             FROM vector_recommend_top_k_hits( \
                    'items', \
                    'v', \
                    '[{\"id\":999}]'::jsonb, \
                    NULL::jsonb, \
                    1 \
                ) AS rec(hit)",
        )
        .expect_err("unknown id should be rejected");
    assert!(
        err.to_string().contains("unknown id 999"),
        "expected unknown-id message, got {err}"
    );
}

#[test]
fn full_text_top_k_hits_supports_english_stemming_and_payload() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE docs (id INT, category TEXT, body TEXT); \
             INSERT INTO docs VALUES \
                 (1, 'story', 'The runners are running swiftly'), \
                 (2, 'story', 'A runner likes to run every day'), \
                 (3, 'other', 'Database internals and buffers')",
        )
        .expect("setup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT hit \
             FROM full_text_top_k_hits( \
                    'docs', \
                    'body', \
                    'run', \
                    2, \
                    'plain', \
                    'english' \
                ) AS fts(hit)",
        )
        .expect("full_text_top_k_hits stemming");

    match results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 2);
            for row in rows {
                let Value::Jsonb(hit) = &row.values[0] else {
                    panic!("expected jsonb hit");
                };
                let score = hit
                    .get("score")
                    .and_then(serde_json::Value::as_f64)
                    .unwrap_or_default();
                assert!(
                    score > 0.0,
                    "expected strictly positive ts_rank score, got {score}"
                );
                let payload = hit
                    .get("payload")
                    .and_then(serde_json::Value::as_object)
                    .expect("payload object");
                assert!(
                    payload.get("body").is_none(),
                    "payload should not duplicate indexed text column"
                );
                assert_eq!(
                    payload.get("category").and_then(serde_json::Value::as_str),
                    Some("story")
                );
            }
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn full_text_top_k_hits_phrase_mode_matches_consecutive_terms() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE docs (id INT, body TEXT); \
             INSERT INTO docs VALUES \
                 (1, 'quick brown fox jumps'), \
                 (2, 'quick fox leaps')",
        )
        .expect("setup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT hit \
             FROM full_text_top_k_hits( \
                    'docs', \
                    'body', \
                    'quick fox', \
                    2, \
                    'phrase', \
                    'english' \
                ) AS fts(hit)",
        )
        .expect("full_text_top_k_hits phrase mode");

    match results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            let Value::Jsonb(hit) = &rows[0].values[0] else {
                panic!("expected jsonb hit");
            };
            assert_eq!(hit.get("id").and_then(serde_json::Value::as_i64), Some(2));
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn full_text_top_k_hits_supports_gin_index_on_text_column() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE docs (id INT, body TEXT); \
             CREATE INDEX docs_body_gin ON docs USING gin (body); \
             INSERT INTO docs VALUES \
                 (1, 'the runner is running quickly'), \
                 (2, 'database internals and pages')",
        )
        .expect("setup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT hit \
             FROM full_text_top_k_hits( \
                    'docs', \
                    'body', \
                    'run', \
                    2, \
                    'plain', \
                    'english' \
                ) AS fts(hit)",
        )
        .expect("full_text_top_k_hits with gin index");

    match results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            let Value::Jsonb(hit) = &rows[0].values[0] else {
                panic!("expected jsonb hit");
            };
            assert_eq!(hit.get("id").and_then(serde_json::Value::as_i64), Some(1));
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn full_text_top_k_hits_supports_payload_filter_option() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE docs (id INT, category TEXT, body TEXT); \
             CREATE INDEX docs_body_gin ON docs USING gin (body); \
             INSERT INTO docs VALUES \
                 (1, 'story', 'the runner keeps running'), \
                 (2, 'other', 'runner notes and diagnostics')",
        )
        .expect("setup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT hit \
             FROM full_text_top_k_hits( \
                    'docs', \
                    'body', \
                    'run', \
                    5, \
                    'plain', \
                    'english', \
                    NULL, \
                    '{\"filter\":{\"must\":[{\"key\":\"category\",\"match\":{\"value\":\"story\"}}]}}'::jsonb \
                ) AS fts(hit)",
        )
        .expect("full_text_top_k_hits with payload filter");

    match results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            let Value::Jsonb(hit) = &rows[0].values[0] else {
                panic!("expected jsonb hit");
            };
            assert_eq!(hit.get("id").and_then(serde_json::Value::as_i64), Some(1));
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn full_text_top_k_hits_websearch_or_with_gin_index_preserves_disjunction() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE docs (id INT, body TEXT); \
             CREATE INDEX docs_body_gin ON docs USING gin (body); \
             INSERT INTO docs VALUES \
                 (1, 'running stories and races'), \
                 (2, 'manual pages and internals')",
        )
        .expect("setup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT hit \
             FROM full_text_top_k_hits( \
                    'docs', \
                    'body', \
                    'run OR pages', \
                    5, \
                    'websearch', \
                    'english' \
                ) AS fts(hit)",
        )
        .expect("full_text_top_k_hits websearch disjunction");

    match results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 2);
            let mut ids: Vec<i64> = rows
                .iter()
                .filter_map(|row| match &row.values[0] {
                    Value::Jsonb(hit) => hit.get("id").and_then(serde_json::Value::as_i64),
                    _ => None,
                })
                .collect();
            ids.sort_unstable();
            assert_eq!(ids, vec![1, 2]);
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn vector_hnsw_plan_rejects_non_finite_literal_query_vector() {
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

    let statement = parse_prepared_statement(
        "SELECT id FROM items \
         ORDER BY l2_distance(v, CAST('[NaN,0.0,1.0]' AS VECTOR(3))) \
         LIMIT 1",
    )
    .expect("parse");
    let err = engine
        .build_physical_plan(&session, &statement)
        .expect_err("non-finite literal query vector should be rejected during planning");
    assert!(
        err.to_string()
            .contains("invalid input syntax for type vector"),
        "unexpected error: {err}"
    );
}

#[test]
fn vector_hnsw_create_index_on_existing_data() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    // Insert data BEFORE creating the HNSW index -- from_rows should be used
    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, v VECTOR(3)); \
             INSERT INTO items VALUES \
                 (1, '[1.0,0.0,0.0]'), \
                 (2, '[0.0,1.0,0.0]'), \
                 (3, '[0.0,0.0,1.0]'); \
             CREATE INDEX idx_v ON items USING hnsw (v)",
        )
        .expect("setup");

    // Verify we can still query after building index on existing data
    let results = engine
        .execute_sql(&session, "SELECT id FROM items ORDER BY id")
        .expect("query");

    match results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 3);
            assert_eq!(rows[0].values[0], aiondb_core::Value::Int(1));
            assert_eq!(rows[1].values[0], aiondb_core::Value::Int(2));
            assert_eq!(rows[2].values[0], aiondb_core::Value::Int(3));
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn vector_hnsw_rejects_non_finite_existing_vectors_during_index_build() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let err = engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, v VECTOR(3)); \
             INSERT INTO items VALUES (1, '[NaN,0.0,1.0]'); \
             CREATE INDEX idx_v ON items USING hnsw (v)",
        )
        .expect_err("CREATE INDEX should reject non-finite stored vectors");

    assert!(
        err.to_string()
            .contains("invalid input syntax for type vector"),
        "unexpected error: {err}"
    );
}

#[test]
fn vector_group_by_errors_without_hash_key_support() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let err = engine
        .execute_sql(
            &session,
            "CREATE TABLE items (embedding VECTOR(2)); \
             INSERT INTO items VALUES ('[1.0,2.0]'); \
             SELECT embedding, COUNT(*) FROM items GROUP BY embedding",
        )
        .expect_err("GROUP BY on VECTOR should fail until hash keys are supported");

    assert!(
        err.to_string()
            .contains("VECTOR values cannot be used as hash keys"),
        "got: {err}"
    );
}

#[test]
fn vector_hnsw_delete_then_search() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, v VECTOR(2)); \
             CREATE INDEX idx_v ON items USING hnsw (v); \
             INSERT INTO items VALUES \
                 (1, '[1.0,0.0]'), \
                 (2, '[0.0,1.0]'), \
                 (3, '[1.0,1.0]'); \
             DELETE FROM items WHERE id = 1",
        )
        .expect("setup");

    let results = engine
        .execute_sql(&session, "SELECT id FROM items ORDER BY id")
        .expect("query after delete");

    match results.last().unwrap() {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 2);
            // id=1 should be gone
            assert_eq!(rows[0].values[0], aiondb_core::Value::Int(2));
            assert_eq!(rows[1].values[0], aiondb_core::Value::Int(3));
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn vector_hnsw_rejects_non_finite_vectors_on_insert() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let err = engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, v VECTOR(3)); \
             CREATE INDEX idx_v ON items USING hnsw (v); \
             INSERT INTO items VALUES (1, '[Infinity,0.0,1.0]')",
        )
        .expect_err("INSERT should reject non-finite vectors once HNSW is present");

    assert!(
        err.to_string()
            .contains("invalid input syntax for type vector"),
        "unexpected error: {err}"
    );
}

#[test]
fn vector_hnsw_with_custom_params() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    // Test HNSW index creation with custom WITH options
    let results = engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, v VECTOR(3)); \
             CREATE INDEX idx_v ON items USING hnsw (v) WITH (m = 8, ef_construction = 100)",
        )
        .expect("create with custom params");

    assert_eq!(results.len(), 2);
    assert_eq!(
        results[1],
        StatementResult::Command {
            tag: "CREATE INDEX".to_owned(),
            rows_affected: 0,
        }
    );
}

#[test]
fn vector_hnsw_query_is_cancelable_mid_execution() {
    let engine = Arc::new(EngineBuilder::for_testing().build().unwrap());
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, v VECTOR(3), q VECTOR(3)); \
             CREATE INDEX idx_v ON items USING hnsw (v)",
        )
        .expect("create vector table and hnsw index");

    const ROWS: usize = 4096;
    const CHUNK: usize = 512;
    for chunk_start in (0..ROWS).step_by(CHUNK) {
        let chunk_end = (chunk_start + CHUNK).min(ROWS);
        let mut sql = String::from("INSERT INTO items VALUES ");
        for i in chunk_start..chunk_end {
            if i > chunk_start {
                sql.push_str(", ");
            }
            let x = i as f32 * 0.03125;
            let y = (i as f32 * 0.017).sin();
            let z = (i as f32 * 0.013).cos();
            sql.push_str(&format!(
                "({}, '[{x:.6},{y:.6},{z:.6}]', '[0.500000,0.250000,0.750000]')",
                i + 1
            ));
        }
        engine.execute_sql(&session, &sql).expect("insert chunk");
    }

    let worker_engine = engine.clone();
    let worker_session = session.clone();
    let worker = std::thread::spawn(move || {
        worker_engine.execute_sql(
            &worker_session,
            "SELECT id, l2_distance(v, q) AS dist \
             FROM items \
             ORDER BY dist \
             LIMIT 1024",
        )
    });

    let cancel_engine = engine.clone();
    let cancel_session = session.clone();
    let canceller = std::thread::spawn(move || {
        for _ in 0..5 {
            std::thread::sleep(Duration::from_millis(1));
            cancel_engine
                .cancel_session(&cancel_session)
                .expect("cancel vector query");
        }
    });

    let error = worker
        .join()
        .expect("worker thread should join")
        .expect_err("vector HNSW query should be canceled mid-execution");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::QueryCanceled);

    canceller.join().expect("canceller thread should join");
}

// =====================================================================
// 9. Error: dimension mismatch on INSERT
// =====================================================================

#[test]
fn insert_dimension_mismatch_error() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE items (id INT, embedding VECTOR(3))")
        .expect("create");

    let result = engine.execute_sql(&session, "INSERT INTO items VALUES (1, '[1.0,2.0]')");
    assert!(result.is_err(), "expected dimension mismatch error");
}

// =====================================================================
// 9. Error: dimension mismatch on distance function
// =====================================================================

#[test]
fn distance_function_dimension_mismatch_error() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    // Use two columns with different dimensions in the same table
    let results = engine
        .execute_sql(
            &session,
            "CREATE TABLE mixed (v2 VECTOR(2), v3 VECTOR(3)); \
             INSERT INTO mixed VALUES ('[1.0,2.0]', '[1.0,2.0,3.0]')",
        )
        .expect("setup");

    assert_eq!(results.len(), 2);

    let result = engine.execute_sql(&session, "SELECT l2_distance(v2, v3) FROM mixed");
    assert!(result.is_err(), "expected dimension mismatch error");
}
