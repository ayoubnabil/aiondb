use super::*;

#[test]
fn analyze_empty_table_returns_command_tag() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "CREATE TABLE empty_tbl (id INT, name TEXT); \
             ANALYZE empty_tbl",
        )
        .expect("execute");

    assert_eq!(
        results,
        vec![
            StatementResult::Command {
                tag: "CREATE TABLE".to_owned(),
                rows_affected: 0,
            },
            StatementResult::Command {
                tag: "ANALYZE".to_owned(),
                rows_affected: 0,
            },
        ]
    );
}

#[test]
fn analyze_after_inserts_updates_statistics() {
    let catalog = Arc::new(aiondb_catalog_store::CatalogStore::default());
    let storage = Arc::new(aiondb_storage_engine::InMemoryStorage::new_without_wal());
    let engine = build_engine_with_store(catalog.clone(), storage);
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE stats_tbl (id INT NOT NULL, label TEXT); \
             INSERT INTO stats_tbl VALUES (1, 'a'); \
             INSERT INTO stats_tbl VALUES (2, 'b'); \
             INSERT INTO stats_tbl VALUES (3, 'c'); \
             ANALYZE stats_tbl",
        )
        .expect("execute");

    let txn_id = engine.current_txn_id(&session).expect("txn id");
    let table = aiondb_catalog::CatalogReader::get_table(
        &*catalog,
        txn_id,
        &aiondb_catalog::QualifiedName::qualified("public", "stats_tbl"),
    )
    .expect("read table")
    .expect("table exists");

    let stats = aiondb_catalog::CatalogReader::get_statistics(&*catalog, txn_id, table.table_id)
        .expect("read stats")
        .expect("stats exist after ANALYZE");

    assert_eq!(stats.row_count, 3);
    assert!(stats.total_bytes > 0, "total_bytes should be positive");
}

#[test]
fn analyze_specific_table_only() {
    let catalog = Arc::new(aiondb_catalog_store::CatalogStore::default());
    let storage = Arc::new(aiondb_storage_engine::InMemoryStorage::new_without_wal());
    let engine = build_engine_with_store(catalog.clone(), storage);
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE tbl_a (id INT); \
             CREATE TABLE tbl_b (id INT); \
             INSERT INTO tbl_a VALUES (1); \
             INSERT INTO tbl_b VALUES (1); \
             INSERT INTO tbl_b VALUES (2); \
             ANALYZE tbl_b",
        )
        .expect("execute");

    let txn_id = engine.current_txn_id(&session).expect("txn id");

    // tbl_a was NOT analyzed.
    let table_a = aiondb_catalog::CatalogReader::get_table(
        &*catalog,
        txn_id,
        &aiondb_catalog::QualifiedName::qualified("public", "tbl_a"),
    )
    .expect("read table_a")
    .expect("table_a exists");

    let stats_a =
        aiondb_catalog::CatalogReader::get_statistics(&*catalog, txn_id, table_a.table_id)
            .expect("read stats_a");
    assert!(stats_a.is_none(), "tbl_a should have no statistics yet");

    // tbl_b WAS analyzed.
    let table_b = aiondb_catalog::CatalogReader::get_table(
        &*catalog,
        txn_id,
        &aiondb_catalog::QualifiedName::qualified("public", "tbl_b"),
    )
    .expect("read table_b")
    .expect("table_b exists");

    let stats_b =
        aiondb_catalog::CatalogReader::get_statistics(&*catalog, txn_id, table_b.table_id)
            .expect("read stats_b")
            .expect("stats_b should exist after ANALYZE");

    assert_eq!(stats_b.row_count, 2);
}

#[test]
fn analyze_nonexistent_table_errors() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(&session, "ANALYZE no_such_table")
        .expect_err("should fail for non-existent table");

    assert_eq!(error.sqlstate(), aiondb_core::SqlState::UndefinedTable,);
}

#[test]
fn analyze_produces_physical_plan_with_analyze_node() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE explain_tbl (id INT)")
        .expect("create table");

    let statement = parse_prepared_statement("ANALYZE explain_tbl").expect("parse");
    let plan = engine
        .build_physical_plan(&session, &statement)
        .expect("physical plan");

    assert!(
        matches!(plan, aiondb_plan::PhysicalPlan::Analyze { .. }),
        "expected Analyze physical plan, got: {plan:?}"
    );
}
