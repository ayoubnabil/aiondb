#![allow(
    clippy::doc_markdown,
    clippy::items_after_statements,
    clippy::map_unwrap_or,
    clippy::match_same_arms,
    clippy::missing_errors_doc,
    clippy::must_use_candidate,
    clippy::needless_pass_by_value,
    clippy::redundant_closure_for_method_calls,
    clippy::struct_excessive_bools,
    clippy::too_many_lines,
    clippy::unreadable_literal
)]

pub mod env;
pub mod ha;
pub mod loader;
pub mod pgwire;
pub mod product;
pub mod replication;
pub mod runtime;
pub mod security;
pub mod storage;
pub mod sys;

pub use aiondb_shard::ShardingConfig;
pub use ha::HaConfig;
pub use loader::{load_from_env, load_from_file};
pub use pgwire::{PgWireConfig, TlsMode};
pub use product::{ProductConstraints, ProductSupportLevel, V0_1_PRODUCT_CONSTRAINTS};
pub use replication::{ReplicationConfig, ReplicationRole, WriteConcern};
pub use runtime::{
    DistributedConfig, EnginePoolConfig, LimitsConfig, RemoteNodeConfig, RuntimeConfig,
};
pub use security::{SecurityConfig, SecurityProfile};
pub use storage::{StorageBackend, StorageConfig};
pub use sys::total_system_memory;
