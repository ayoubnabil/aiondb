use super::*;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

// Helper: write a temp config file and return its path.
fn temp_config_file(name: &str, content: &str) -> PathBuf {
    let mut path = std::env::temp_dir();
    static TEST_SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = TEST_SEQ.fetch_add(1, Ordering::Relaxed);
    // Include process/thread + sequence to avoid collisions in parallel tests.
    path.push(format!(
        "aiondb_loader_test_{}_{:?}_{seq}_{name}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::write(&path, content).expect("failed to write temp config");
    path
}

mod file_io_and_mapping;
mod ha_config;
mod new_edge_cases;
mod parse_helpers;
mod validate_config;
