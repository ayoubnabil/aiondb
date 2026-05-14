use std::collections::BTreeMap;
use std::sync::Arc;

use aiondb_catalog_store::CatalogStore;
use aiondb_engine::*;
use aiondb_storage_engine::InMemoryStorage;

fn main() {
    let catalog = Arc::new(CatalogStore::new());
    let storage = Arc::new(InMemoryStorage::new_without_wal());
    let engine = EngineBuilder::for_testing()
        .with_catalog_txn(catalog.clone())
        .with_catalog_reader(catalog.clone())
        .with_catalog_writer(catalog.clone())
        .with_sequence_manager(catalog)
        .with_storage_ddl(storage.clone())
        .with_storage_dml(storage.clone())
        .with_storage_txn(storage)
        .build()
        .expect("failed to build feature-test engine");
    let (session, _) = aiondb_engine::QueryEngine::startup(
        &engine,
        StartupParams {
            database: "default".to_owned(),
            application_name: Some("test".to_owned()),
            options: BTreeMap::new(),
            credential: Credential::Anonymous {
                user: "alice".to_owned(),
            },
            transport: TransportInfo::in_process(),
        },
    )
    .unwrap();

    let tests = vec![
        ("SERIAL type", "CREATE TABLE t_serial(id SERIAL PRIMARY KEY, name TEXT)"),
        ("TEMP TABLE", "CREATE TEMP TABLE t_temp(id INT)"),
        ("bool literal cast", "SELECT bool 't'"),
        ("char(8) CREATE", "CREATE TABLE t_char(f1 char(8))"),
        ("char(8) INSERT", "INSERT INTO t_char VALUES ('test')"),
        ("HAVING setup", "CREATE TABLE t_having(a INT, b INT)"),
        ("HAVING insert", "INSERT INTO t_having VALUES (1, 10), (1, 20), (2, 30)"),
        ("HAVING query", "SELECT a, count(*) FROM t_having GROUP BY a HAVING count(*) > 1"),
        ("repeat()", "SELECT repeat('x', 3)"),
        ("char_length()", "SELECT char_length('hello')"),
        ("DELETE alias setup", "CREATE TABLE t_del(a INT)"),
        ("DELETE alias insert", "INSERT INTO t_del VALUES (1), (2), (3)"),
        ("DELETE with alias", "DELETE FROM t_del AS dt WHERE dt.a > 2"),
        ("DEFAULT in VALUES setup", "CREATE TABLE t_def(a INT, b INT DEFAULT 42)"),
        ("DEFAULT in VALUES", "INSERT INTO t_def (a, b) VALUES (1, DEFAULT)"),
        ("lower()", "SELECT lower('ABC')"),
        ("RETURNING setup", "CREATE TABLE t_ret(id INT, name TEXT)"),
        ("RETURNING insert", "INSERT INTO t_ret VALUES (1, 'a'), (2, 'b')"),
        ("INSERT RETURNING", "INSERT INTO t_ret VALUES (3, 'c') RETURNING *"),
        ("UPDATE RETURNING", "UPDATE t_ret SET name = 'z' WHERE id = 1 RETURNING *"),
        ("DELETE RETURNING", "DELETE FROM t_ret WHERE id = 2 RETURNING *"),
        ("Window ROW_NUMBER", "SELECT id, ROW_NUMBER() OVER (ORDER BY id) FROM t_ret"),
        ("Window COUNT(*)", "SELECT id, COUNT(*) OVER () FROM t_ret"),
        ("Window LAG", "SELECT id, LAG(id) OVER (ORDER BY id) FROM t_ret"),
        ("Window RANK", "SELECT id, RANK() OVER (ORDER BY id) FROM t_ret"),
        ("DISTINCT ON", "SELECT DISTINCT ON (name) name, id FROM t_ret ORDER BY name, id"),
        ("num_nonnulls zero args", "SELECT num_nonnulls()"),
        ("num_nulls zero args", "SELECT num_nulls()"),
        (
            "canonicalize_path helper create",
            "CREATE FUNCTION test_canonicalize_path(text) \
             RETURNS text \
             AS '/tmp/regress.so' \
             LANGUAGE C STRICT IMMUTABLE",
        ),
        (
            "canonicalize_path helper call",
            "SELECT test_canonicalize_path('/./abc/def/')",
        ),
        (
            "pg_log_backend_memory_contexts",
            "SELECT pg_log_backend_memory_contexts(pg_backend_pid())",
        ),
        (
            "pg_log_backend_memory_contexts via pg_stat_activity",
            "SELECT pg_log_backend_memory_contexts(pid) FROM pg_stat_activity WHERE backend_type = 'checkpointer'",
        ),
        (
            "pg_ls_dir in FROM",
            "SELECT count(*) >= 0 FROM pg_ls_dir('.', false, false)",
        ),
        (
            "pg_ls_archive_statusdir in FROM",
            "SELECT count(*) >= 0 FROM pg_ls_archive_statusdir()",
        ),
        (
            "pg_ls_dir in derived subquery",
            "SELECT * FROM (SELECT pg_ls_dir('.') a) a WHERE a = 'base' LIMIT 1",
        ),
        (
            "pg_timezone_names field access",
            "SELECT * FROM (SELECT (pg_timezone_names()).name) ptn WHERE name = 'UTC' LIMIT 1",
        ),
        ("CREATE ROLE regress_log_memory", "CREATE ROLE regress_log_memory"),
        (
            "has_function_privilege before grant",
            "SELECT has_function_privilege('regress_log_memory', 'pg_log_backend_memory_contexts(integer)', 'EXECUTE')",
        ),
        (
            "GRANT EXECUTE built-in function",
            "GRANT EXECUTE ON FUNCTION pg_log_backend_memory_contexts(integer) TO regress_log_memory",
        ),
        (
            "has_function_privilege after grant",
            "SELECT has_function_privilege('regress_log_memory', 'pg_log_backend_memory_contexts(integer)', 'EXECUTE')",
        ),
        ("bool 't' cast", "SELECT bool 't'"),
        ("int4 '42' cast", "SELECT int4 '42'"),
        ("float8 '3.14' cast", "SELECT float8 '3.14'"),
        ("varchar '42' cast", "SELECT varchar 'hello'"),
        ("TEMP TABLE", "CREATE TEMP TABLE t_tmp(x INT)"),
        ("INSERT TEMP", "INSERT INTO t_tmp VALUES (1)"),
        ("SELECT TEMP", "SELECT * FROM t_tmp"),
    ];

    for (name, sql) in &tests {
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            aiondb_engine::QueryEngine::execute_sql(&engine, &session, sql)
        })) {
            Ok(Ok(results)) => {
                let row_count: usize = results
                    .iter()
                    .map(|r| match r {
                        StatementResult::Query { rows, .. } => rows.len(),
                        StatementResult::Command { rows_affected, .. } => {
                            rows_affected.to_owned() as usize
                        }
                        _ => 0,
                    })
                    .sum();
                println!("  OK  {:<30} ({} rows)", name, row_count);
            }
            Ok(Err(e)) => println!("  ERR {:<30} {}", name, e),
            Err(_) => println!("  PAN {:<30} PANIC!", name),
        }
    }
}
