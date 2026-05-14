use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use aiondb_engine::{EngineBuilder, SessionLimits};

use super::*;

fn unique_temp_path(scope: &str, name: &str) -> std::path::PathBuf {
    static TEST_SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = TEST_SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!(
        "aiondb-embedded-{scope}-{name}-{}-{seq}-{nanos}",
        std::process::id()
    ))
}

fn connect() -> Connection<aiondb_engine::Engine> {
    let engine = Arc::new(EngineBuilder::for_testing().build().unwrap());
    let database = Database::new(engine);
    database
        .connect(ConnectOptions {
            application_name: Some("embedded-tests".to_owned()),
            ..ConnectOptions::anonymous("default", "alice")
        })
        .expect("connect")
}

mod basic_queries;
mod dml_and_transactions;
mod limits_and_portals;
mod open_profiles;
mod predicates_and_limits;
mod sql_smoke_and_ddl;
mod transactions_and_prepared;
