#![allow(
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::doc_markdown,
    clippy::missing_errors_doc,
    clippy::must_use_candidate,
    clippy::redundant_closure_for_method_calls,
    clippy::struct_excessive_bools
)]

pub mod api;
pub mod descriptors;
pub mod graph;
pub mod pg_catalog;
pub mod sequence;
pub mod statistics;

pub use api::{
    AccessPathMetadata, CatalogReader, CatalogTxnParticipant, CatalogWriter, IndexAlteration,
    SequenceAlteration, SequenceManager, TableAlteration,
};
pub use descriptors::{
    CastContextDescriptor, CastDescriptor, CastMethodDescriptor, CatalogPrivilege,
    CatalogShardConfig, CheckConstraint, ColumnDescriptor, CommentDescriptor,
    DomainConstraintDescriptor, DomainDescriptor, ForeignKeyConstraint, FunctionDescriptor,
    FunctionParamDescriptor, FunctionPrivilegeTarget, HnswParams, IdentityColumnDescriptor,
    IndexDescriptor, IndexKeyColumn, IndexKind, PolicyCommandDescriptor, PolicyDescriptor,
    PolicyKindDescriptor, PrivilegeDescriptor, PrivilegeTarget, QualifiedName, RoleDescriptor,
    RuleDescriptor, RuleEventDescriptor, SchemaDescriptor, SortOrder, TableDescriptor,
    TenantDescriptor, TriggerDescriptor, TriggerEventDescriptor, TriggerTimingDescriptor,
    UserTypeDescriptor, UserTypeFieldDescriptor, VectorDistanceMetric, VectorQuantizationKind,
    ViewCheckOption, ViewDescriptor, MAX_CATALOG_HASH_RING_VIRTUAL_NODES, MAX_CATALOG_SHARD_COUNT,
    MAX_CATALOG_VIRTUAL_NODES_PER_SHARD,
};
pub use graph::{EdgeEndpoints, EdgeLabelDescriptor, NodeLabelDescriptor};
pub use sequence::SequenceDescriptor;
pub use statistics::{ColumnStatistics, Histogram, McvList, TableStatistics};
