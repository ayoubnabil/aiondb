use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{SystemTime, UNIX_EPOCH};

use aiondb_core::Row;

use super::*;
use crate::EngineBuilder;

mod acl_enforcement;
mod agg_functions;
mod alter_constraints;
mod analyze;
mod array_ops;
mod arrays;
mod async_notify;
mod auth_audit;
mod authz;
mod backup;
mod cast_matrix;
mod check_constraints;
mod checkpoint;
mod compat_tag_contract;
mod concurrency_isolation;
mod copy;
mod ctes;
mod ctes_additional;
mod ctes_values_aliases;
mod date_functions;
mod ddl_and_expressions;
mod distributed;
mod dml_and_errors;
mod dos_audit;
mod engine_metrics;
mod foreign_keys;
mod functions;
mod graph;
mod graph_campaigns;
mod graph_perf_bench;
mod information_schema;
mod insert_perf_bench;
mod join_perf_bench;
mod jsonb;
mod jsonb_ops;
mod limits_and_validation;
mod lock_table;
mod math_functions;
mod mixed_sql_graph_vector;
mod multi_session;
mod multi_tenant;
mod numeric;
mod ops_drills;
mod perf_budget;
mod pg_catalog;
mod pg_catalog_commands;
mod pg_catalog_runtime_settings;
mod pg_compat;
mod pg_compat_adv;
mod pg_compat_date;
mod pg_compat_horology;
mod pg_compat_interval;
mod pg_compat_joins;
mod pg_compat_timetz;
mod plan_cache;
mod plpgsql;
mod portals;
mod roles;
mod savepoints;
mod schema_ddl;
mod select_perf_bench;
mod sequences_and_defaults;
mod set_operations;
mod soak;
mod sql_basics;
mod stats_ext;
mod stress;
mod stress_load;
mod subqueries;
mod text_functions;
mod time_type;
mod transaction_conflicts;
mod transactions;
mod triggers;
mod unique_enforcement;
mod update_perf_bench;
mod vacuum;
mod vector_campaigns;
mod vector_qa;
mod vectors;
mod views;
mod window_functions;

fn startup_params() -> StartupParams {
    StartupParams {
        database: "default".to_owned(),
        application_name: Some("test".to_owned()),
        options: BTreeMap::new(),
        credential: Credential::Anonymous {
            user: "alice".to_owned(),
        },
        transport: TransportInfo::in_process(),
    }
}

fn engine_builder_base(
    catalog: Arc<aiondb_catalog_store::CatalogStore>,
    storage: Arc<aiondb_storage_engine::InMemoryStorage>,
) -> (EngineBuilder, Arc<aiondb_tx::InMemoryTransactionManager>) {
    let tx_runtime = shared_test_tx_runtime(&catalog, &storage);
    let builder = EngineBuilder::for_testing()
        .with_transaction_manager(tx_runtime.clone())
        .with_snapshot_oracle(tx_runtime.clone())
        .with_serializable_coordinator(tx_runtime.clone())
        .with_catalog_txn(catalog.clone())
        .with_catalog_reader(catalog.clone())
        .with_catalog_writer(catalog.clone())
        .with_sequence_manager(catalog)
        .with_storage_ddl(storage.clone())
        .with_storage_dml(storage.clone())
        .with_storage_txn(storage);
    (builder, tx_runtime)
}

fn build_engine_with_store(
    catalog: Arc<aiondb_catalog_store::CatalogStore>,
    storage: Arc<aiondb_storage_engine::InMemoryStorage>,
) -> Engine {
    let (builder, _) = engine_builder_base(catalog, storage);
    builder.build().unwrap()
}

fn build_engine_with_store_no_locks(
    catalog: Arc<aiondb_catalog_store::CatalogStore>,
    storage: Arc<aiondb_storage_engine::InMemoryStorage>,
) -> Engine {
    let (builder, _) = engine_builder_base(catalog, storage);
    builder
        .with_lock_manager(Arc::new(aiondb_tx::NoopLockManager))
        .build()
        .unwrap()
}

fn shared_test_tx_runtime(
    catalog: &Arc<aiondb_catalog_store::CatalogStore>,
    storage: &Arc<aiondb_storage_engine::InMemoryStorage>,
) -> Arc<aiondb_tx::InMemoryTransactionManager> {
    #[derive(Clone)]
    struct SharedRuntimeEntry {
        catalog: Weak<aiondb_catalog_store::CatalogStore>,
        storage: Weak<aiondb_storage_engine::InMemoryStorage>,
        tx_runtime: Arc<aiondb_tx::InMemoryTransactionManager>,
    }

    static SHARED_RUNTIMES: OnceLock<Mutex<Vec<SharedRuntimeEntry>>> = OnceLock::new();

    let mut runtimes = SHARED_RUNTIMES
        .get_or_init(|| Mutex::new(Vec::new()))
        .lock()
        .expect("shared test tx runtime lock");
    runtimes.retain(|entry| entry.catalog.strong_count() > 0 && entry.storage.strong_count() > 0);
    if let Some(entry) = runtimes.iter().find(|entry| {
        entry.catalog.as_ptr() == Arc::as_ptr(catalog)
            && entry.storage.as_ptr() == Arc::as_ptr(storage)
    }) {
        return entry.tx_runtime.clone();
    }

    let tx_runtime = Arc::new(aiondb_tx::InMemoryTransactionManager::default());
    runtimes.push(SharedRuntimeEntry {
        catalog: Arc::downgrade(catalog),
        storage: Arc::downgrade(storage),
        tx_runtime: tx_runtime.clone(),
    });
    tx_runtime
}

/// Execute SQL and return the rows from the *last* statement result.
fn query_rows(engine: &Engine, session: &SessionHandle, sql: &str) -> Vec<Row> {
    let results = engine.execute_sql(session, sql).expect("query");
    match results.last().expect("at least one result") {
        StatementResult::Query { rows, .. } => rows.clone(),
        other => panic!("expected Query, got {other:?}"),
    }
}

/// Execute a `SELECT COUNT(*) ...` style SQL and return the resulting `i64`.
/// Asserts exactly one row with a `BigInt` cell.
fn query_count(engine: &Engine, session: &SessionHandle, sql: &str) -> i64 {
    let rows = query_rows(engine, session, sql);
    assert_eq!(rows.len(), 1, "COUNT should return exactly one row");
    match &rows[0].values[0] {
        Value::BigInt(n) => *n,
        other => panic!("expected BigInt from COUNT, got {other:?}"),
    }
}

/// Execute SQL whose first statement is a single-row, single-column query,
/// and return that cell's value.
#[allow(dead_code)] // not every test module imports this helper
fn query_single_value(engine: &Engine, session: &SessionHandle, sql: &str) -> Value {
    let results = engine.execute_sql(session, sql).expect("execute");
    match &results[0] {
        StatementResult::Query { rows, .. } => rows[0].values[0].clone(),
        other => panic!("expected query result, got {other:?}"),
    }
}

/// Execute an EXPLAIN-shaped SQL (single Text column per row) and return
/// the rendered plan lines. Panics if any row's first cell isn't `Text`.
#[allow(dead_code)] // not every test module imports this helper
fn explain_lines(engine: &Engine, session: &SessionHandle, sql: &str) -> Vec<String> {
    query_rows(engine, session, sql)
        .into_iter()
        .map(|row| match row.values.first() {
            Some(Value::Text(line)) => line.clone(),
            other => panic!("expected EXPLAIN text row, got {other:?}"),
        })
        .collect()
}

fn access_path_for_query(
    engine: &Engine,
    session: &SessionHandle,
    sql: &str,
) -> aiondb_plan::ScanAccessPath {
    let statement = parse_prepared_statement(sql).expect("parse");
    let plan = engine
        .build_physical_plan(session, &statement)
        .expect("physical plan");
    let aiondb_plan::PhysicalPlan::ProjectTable { access_path, .. } = plan else {
        panic!("expected table scan plan");
    };
    access_path
}

fn unique_relative_backup_path(name: &str) -> std::path::PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time")
        .as_nanos();
    std::path::PathBuf::from(format!("aiondb-{name}-{}-{nanos}.sql", std::process::id()))
}
