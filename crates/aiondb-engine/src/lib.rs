pub(crate) mod auth_audit;
pub mod builder;
mod catalog_auth;
pub(crate) mod catalog_authorizer;
pub mod config;
pub mod engine;
mod params;
pub mod prepared;
pub mod session;
#[cfg(test)]
pub(crate) mod test_support;

// Re-export only types that appear in this crate's public API signatures.
// Users should depend on aiondb-core, aiondb-security, aiondb-tx directly
// for the full type surface.
pub use aiondb_auth_audit::{AuthAuditQuery, AuthAuditRecord};
pub use aiondb_cluster::{
    DatabaseId, InMemoryControlPlane, MetadataReader, MetadataWriter, NodeId, NodeMembership,
    NodeReplicationStatus, PlacementEpoch, ReplicaCatchupKey, ReplicaRepairMode, ReplicaRole,
    ReplicationMaintenanceOptions, ReplicationMaintenanceOutcome, ReplicationStatusSnapshot,
    ShardDescriptor, ShardId, ShardPlacement,
};
pub use aiondb_core::{DataType, DbError, DbResult, ErrorReport, RelationId, Row, SqlState, Value};
pub use aiondb_security::{
    AccessRequest, AccessTarget, Action, AllowAllAuthorizer, AuthRateLimiter,
    AuthenticatedIdentity, Authenticator, Authorizer, Credential, FileBackedAuthRateLimiter,
    InMemoryAuthRateLimiter, ScramVerifier, SecretBytes, SecretString, TransportInfo,
    TransportKind,
};
pub use aiondb_tx::IsolationLevel;
pub use builder::EngineBuilder;
pub use config::{session_limits_from_config, EngineConfig};
pub use engine::api::{
    PgWireEngine, QueryExtendedProtocol, QueryReplication, QuerySessionControl, QuerySimpleSql,
    QueryStartup, QueryTransactions, QueryWireCompatibility,
};
pub use engine::metrics::{EngineMetrics, EngineMetricsSnapshot};
pub use engine::replication::{install_replication_seed, EngineReplicationSeedManifest};
pub use engine::{
    Engine, QueryEngine, ReplicationIdentity, SqlStatementWireMetadata, StartupAuthentication,
    StartupParams, WireStateCleanupHint,
};
pub use prepared::{
    PortalBatch, PortalDescription, PreparedStatementDesc, ResultColumn, ResultColumnOrigin,
    StatementResult,
};
pub use session::{SessionHandle, SessionInfo, SessionLimits};
