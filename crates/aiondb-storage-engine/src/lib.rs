#![allow(
    clippy::cast_possible_wrap,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::doc_markdown,
    clippy::elidable_lifetime_names,
    clippy::items_after_statements,
    clippy::collapsible_else_if,
    clippy::manual_let_else,
    clippy::map_unwrap_or,
    clippy::match_same_arms,
    clippy::missing_errors_doc,
    clippy::missing_fields_in_debug,
    clippy::must_use_candidate,
    clippy::needless_pass_by_value,
    clippy::redundant_closure_for_method_calls,
    clippy::redundant_else,
    clippy::semicolon_if_nothing_returned,
    clippy::similar_names,
    clippy::single_match_else,
    clippy::trivially_copy_pass_by_ref,
    clippy::unnecessary_wraps,
    clippy::unused_self,
    clippy::wildcard_imports,
    clippy::too_many_lines,
    clippy::uninlined_format_args,
    clippy::unreadable_literal
)]

mod backend;
mod engine;
mod layout;
mod lsm_sstable;
mod replication;
pub mod storage_compat;
#[cfg(test)]
pub(crate) mod test_support;

pub use aiondb_wal::{WalConfig, WalLsnMode};
pub use backend::{
    DiskBackendConfig, DiskSyncPolicy, LsmBackendConfig, PageEngineBackendConfig, PageSyncPolicy,
    StorageBackendHandle, StorageBackendKind, StorageBackendSpec,
};
pub use engine::{
    disk_heap::{DiskTableStore, DiskTableStoreConfig},
    row_lock::{DmlPrecheck, IntentLockMode, RowLockMode, RowLockTable},
    HnswIndexStats, HnswSearchStats, HnswSearchStatsSummary, InMemoryStorage, RecoveredStatistics,
    RecoveryReport, StorageBufferPoolConfig, StorageEngine, StorageMetrics, StorageOptions,
    WalCommitPolicy,
};
pub use replication::{install_replication_seed, StorageReplicationSeedManifest};
pub use storage_compat::{
    doctor_data_dir, ensure_storage_contract_for_open, upgrade_data_dir, StorageDoctorReport,
};
pub type PageStoreStorage = StorageEngine;
