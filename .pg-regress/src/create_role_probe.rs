use std::collections::BTreeMap;
use std::sync::Arc;

use aiondb_catalog_store::CatalogStore;
use aiondb_engine::{
    Credential, EngineBuilder, QueryEngine, StartupParams, StatementResult, TransportInfo,
};
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
        .expect("failed to build probe engine");

    let (session, _) = engine
        .startup(StartupParams {
            database: "default".to_owned(),
            application_name: Some("create-role-probe".to_owned()),
            options: BTreeMap::new(),
            credential: Credential::Anonymous {
                user: "alice".to_owned(),
            },
            transport: TransportInfo::in_process(),
        })
        .expect("startup");

    let steps = [
        "CREATE ROLE regress_role_admin CREATEROLE CREATEDB LOGIN",
        "CREATE ROLE regress_tenant NOINHERIT NOLOGIN",
        "CREATE ROLE regress_createrole LOGIN CREATEROLE",
        "GRANT CREATE ON DATABASE default TO regress_tenant",
        "SET SESSION AUTHORIZATION regress_tenant",
        "CREATE TABLE tenant_table (i integer)",
        "CREATE INDEX tenant_idx ON tenant_table(i)",
        "CREATE VIEW tenant_view AS SELECT * FROM pg_catalog.pg_class",
        "REVOKE ALL PRIVILEGES ON tenant_table FROM PUBLIC",
        "SET SESSION AUTHORIZATION regress_createrole",
        "DROP INDEX tenant_idx",
        "ALTER TABLE tenant_table ADD COLUMN t text",
        "DROP TABLE tenant_table",
        "ALTER VIEW tenant_view OWNER TO regress_role_admin",
        "DROP VIEW tenant_view",
        "SELECT current_user, session_user",
        "SHOW search_path",
        "SELECT n.nspname, c.relname, c.relkind \
         FROM pg_catalog.pg_class c \
         JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
         WHERE c.relname IN ('tenant_table', 'tenant_idx', 'tenant_view') \
         ORDER BY c.relname",
        "SET SESSION AUTHORIZATION regress_role_admin",
        "DROP ROLE regress_tenant",
        "SELECT n.nspname, c.relname, c.relkind \
         FROM pg_catalog.pg_class c \
         JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
         WHERE c.relname IN ('tenant_table', 'tenant_idx', 'tenant_view') \
         ORDER BY c.relname",
        "RESET SESSION AUTHORIZATION",
        "SELECT current_user, session_user",
        "SHOW search_path",
        "DROP INDEX tenant_idx",
        "DROP TABLE tenant_table",
        "DROP VIEW tenant_view",
        "DROP ROLE regress_tenant, regress_tenant",
    ];

    for sql in steps {
        println!("\nSQL> {sql}");
        match engine.execute_sql(&session, sql) {
            Ok(results) => {
                for result in results {
                    match result {
                        StatementResult::Command {
                            tag,
                            rows_affected,
                        } => {
                            println!("  OK  {tag} rows={rows_affected}");
                        }
                        StatementResult::Query { columns, rows } => {
                            let header = columns
                                .iter()
                                .map(|column| column.name.clone())
                                .collect::<Vec<_>>()
                                .join(" | ");
                            println!("  QRY columns: {header}");
                            for row in rows {
                                let rendered = row
                                    .values
                                    .into_iter()
                                    .map(|value| format!("{value}"))
                                    .collect::<Vec<_>>()
                                    .join(" | ");
                                println!("  QRY row: {rendered}");
                            }
                        }
                        other => {
                            println!("  OK  {other:?}");
                        }
                    }
                }
            }
            Err(error) => {
                println!("  ERR {error}");
            }
        }
    }
}
