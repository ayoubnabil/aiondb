use aiondb_core::{ColumnId, IndexId, RelationId, Row, TupleId, TxnId, Value};
use aiondb_storage_api::{IndexStorageDescriptor, TableStorageDescriptor};
use aiondb_tx::IsolationLevel;

use crate::Lsn;

/// Tag space: catalog records use tags 13..=29 and 33..=43.
/// A single WAL record representing one logged operation.
#[derive(Clone, Debug, PartialEq)]
pub enum WalRecord {
    /// Transaction started.
    BeginTxn {
        txn_id: TxnId,
        isolation: IsolationLevel,
    },
    /// Transaction committed with assigned timestamp.
    CommitTxn { txn_id: TxnId, commit_ts: u64 },
    /// Transaction aborted/rolled back.
    AbortTxn { txn_id: TxnId },
    /// Row inserted into a table.
    InsertRow {
        txn_id: TxnId,
        table_id: RelationId,
        tuple_id: TupleId,
        row: Row,
    },
    /// Row deleted from a table.
    DeleteRow {
        txn_id: TxnId,
        table_id: RelationId,
        tuple_id: TupleId,
    },
    /// Row updated (modeled as delete+insert with new `tuple_id`).
    UpdateRow {
        txn_id: TxnId,
        table_id: RelationId,
        old_tuple_id: TupleId,
        new_tuple_id: TupleId,
        row: Row,
    },
    /// Single-row autocommit insert. This is durable as soon as the record is
    /// flushed; recovery replays it immediately without separate
    /// BeginTxn/CommitTxn records.
    AutocommitInsertRow {
        txn_id: TxnId,
        table_id: RelationId,
        tuple_id: TupleId,
        row: Row,
    },
    /// Single-row autocommit delete.
    AutocommitDeleteRow {
        txn_id: TxnId,
        table_id: RelationId,
        tuple_id: TupleId,
    },
    /// Single-row autocommit update.
    AutocommitUpdateRow {
        txn_id: TxnId,
        table_id: RelationId,
        old_tuple_id: TupleId,
        new_tuple_id: TupleId,
        row: Row,
    },
    /// Table storage created.
    CreateTable {
        txn_id: TxnId,
        descriptor: TableStorageDescriptor,
    },
    /// Table storage dropped.
    DropTable { txn_id: TxnId, table_id: RelationId },
    /// Index storage created.
    CreateIndex {
        txn_id: TxnId,
        descriptor: IndexStorageDescriptor,
    },
    /// Index storage dropped.
    DropIndex { txn_id: TxnId, index_id: IndexId },
    /// Table storage altered (full new descriptor).
    AlterTable {
        txn_id: TxnId,
        descriptor: TableStorageDescriptor,
    },
    /// WAL checkpoint marker.
    ///
    /// Despite the field name, the stored LSN currently represents the first
    /// WAL position after the snapshotted prefix.
    Checkpoint { last_committed_lsn: Lsn },
    /// Table statistics update (persisted by ANALYZE).
    UpdateStatistics {
        table_id: RelationId,
        row_count: u64,
        total_bytes: u64,
        dead_row_count: u64,
        column_stats: Vec<(ColumnId, f64, f64, u32)>,
    },
    /// Full page image used for torn-page-safe redo.
    FullPageImage {
        relation_id: RelationId,
        page_number: u64,
        page_data: Vec<u8>,
    },
    /// Multiple full page images for the same relation packed into one record.
    ///
    /// This is currently used to reduce WAL overhead for disk-backed index
    /// persistence without changing the replay semantics of each page image.
    FullPageImageBatch {
        relation_id: RelationId,
        pages: Vec<(u64, Vec<u8>)>,
    },
    /// Fine-grained patch for a single page.
    ///
    /// Each segment replaces a contiguous byte range at `offset` inside the
    /// page. Recovery applies the patches in order onto the previously
    /// checkpointed page image.
    PagePatch {
        relation_id: RelationId,
        page_number: u64,
        segments: Vec<(u16, Vec<u8>)>,
    },
    /// Multiple fine-grained page patches for the same relation.
    PagePatchBatch {
        relation_id: RelationId,
        patches: Vec<(u64, Vec<(u16, Vec<u8>)>)>,
    },
    /// Multiple fixed-width 8-byte replacements for the same relation.
    ///
    /// This specializes the common case where a page mutation only changes one
    /// `u64` field such as a page/header counter or pointer.
    PageSetU64Batch {
        relation_id: RelationId,
        updates: Vec<(u64, u16, u64)>,
    },
    /// Specialized fixed-width DiskBTree metapage update.
    DiskBtreeMetaUpdate {
        relation_id: RelationId,
        root_page: u64,
        height: u32,
        page_count: u64,
        free_list_head: u64,
    },
    /// Specialized fixed-width DiskBTree leaf insert without page split.
    DiskBtreeLeafInsert {
        relation_id: RelationId,
        page_number: u64,
        key: u64,
        value: u64,
    },
    /// Specialized fixed-width DiskBTree leaf delete without page merge/split.
    DiskBtreeLeafDelete {
        relation_id: RelationId,
        page_number: u64,
        key: u64,
        value: u64,
    },
    /// Specialized fixed-width DiskBTree leaf split.
    DiskBtreeLeafSplit {
        relation_id: RelationId,
        left_page: u64,
        right_page: u64,
        old_right_sibling: u64,
        separator: u64,
        left_entries: Vec<(u64, u64)>,
        right_entries: Vec<(u64, u64)>,
    },
    /// Specialized fixed-width DiskBTree internal-page insert without split.
    DiskBtreeInternalInsert {
        relation_id: RelationId,
        page_number: u64,
        separator: u64,
        child_page: u64,
    },
    /// Specialized fixed-width DiskBTree internal split.
    DiskBtreeInternalSplit {
        relation_id: RelationId,
        left_page: u64,
        right_page: u64,
        promoted_separator: u64,
        left_first_child: u64,
        right_first_child: u64,
        left_entries: Vec<(u64, u64)>,
        right_entries: Vec<(u64, u64)>,
    },
    /// Specialized fixed-width DiskBTree new internal root/page initialization.
    DiskBtreeRootGrow {
        relation_id: RelationId,
        page_number: u64,
        first_child: u64,
        separator: u64,
        right_child: u64,
    },
    /// Specialized fixed-width DiskBTree internal delete without split/merge.
    DiskBtreeInternalDelete {
        relation_id: RelationId,
        page_number: u64,
        separator: u64,
        child_page: u64,
    },
    /// Specialized fixed-width DiskBTree leaf redistribution across siblings.
    DiskBtreeLeafRedistribute {
        relation_id: RelationId,
        left_page: u64,
        right_page: u64,
        parent_page: u64,
        parent_slot: u32,
        parent_first_child: u64,
        left_entries: Vec<(u64, u64)>,
        right_entries: Vec<(u64, u64)>,
        right_right_sibling: u64,
        new_separator: u64,
    },
    /// Specialized fixed-width DiskBTree internal redistribution across siblings.
    DiskBtreeInternalRedistribute {
        relation_id: RelationId,
        left_page: u64,
        right_page: u64,
        parent_page: u64,
        parent_slot: u32,
        parent_first_child: u64,
        left_first_child: u64,
        right_first_child: u64,
        left_entries: Vec<(u64, u64)>,
        right_entries: Vec<(u64, u64)>,
        new_separator: u64,
    },
    /// Specialized fixed-width DiskBTree leaf merge with right sibling.
    DiskBtreeLeafMerge {
        relation_id: RelationId,
        left_page: u64,
        right_page: u64,
        parent_page: u64,
        parent_first_child: u64,
        removed_separator: u64,
        left_entries: Vec<(u64, u64)>,
        new_right_sibling: u64,
        next_free_page: u64,
    },
    /// Specialized fixed-width DiskBTree internal merge with right sibling.
    DiskBtreeInternalMerge {
        relation_id: RelationId,
        left_page: u64,
        right_page: u64,
        parent_page: u64,
        parent_first_child: u64,
        removed_separator: u64,
        left_first_child: u64,
        left_entries: Vec<(u64, u64)>,
        next_free_page: u64,
    },
    /// Specialized fixed-width DiskBTree root shrink into a leaf page.
    DiskBtreeRootShrinkLeaf {
        relation_id: RelationId,
        root_page: u64,
        root_entries: Vec<(u64, u64)>,
        right_sibling: u64,
        freed_pages: Vec<(u64, u64)>,
    },
    /// Specialized fixed-width DiskBTree root shrink into an internal page.
    DiskBtreeRootShrinkInternal {
        relation_id: RelationId,
        root_page: u64,
        root_first_child: u64,
        root_entries: Vec<(u64, u64)>,
        freed_pages: Vec<(u64, u64)>,
    },
    /// Specialized fixed-width DiskBTree collapse of an empty internal child.
    DiskBtreeInternalCollapse {
        relation_id: RelationId,
        parent_page: u64,
        parent_slot: u32,
        parent_first_child: u64,
        replacement_child: u64,
        removed_page: u64,
        next_free_page: u64,
    },
    /// Specialized fixed-width DiskBTree promotion of a single child to root.
    DiskBtreeRootPromoteSingleChild {
        relation_id: RelationId,
        new_root_page: u64,
        removed_root_page: u64,
        next_free_page: u64,
    },
    /// Specialized fixed-width DiskBTree root promotion after collapsing an intermediate empty internal child.
    DiskBtreeRootPromoteCollapsedChain {
        relation_id: RelationId,
        new_root_page: u64,
        freed_pages: Vec<(u64, u64)>,
    },
    /// Specialized fixed-width chain of local internal collapses.
    DiskBtreeInternalCollapseChain {
        relation_id: RelationId,
        steps: Vec<(u64, u32, u64, u64, u64, u64)>,
    },
    /// Snapshot-only reference to a row whose bytes live in durable table pages.
    ///
    /// This avoids duplicating every row payload in base snapshots once the
    /// paged table checkpoint has been atomically published at the same LSN.
    PagedRowRef {
        txn_id: TxnId,
        table_id: RelationId,
        tuple_id: TupleId,
    },

    // -- Catalog DDL records (tags 13..=29 and 33..=43) --------------------
    // Descriptors are JSON-serialized to `Vec<u8>` for forward-compatible encoding.
    /// Schema created.
    CatalogCreateSchema {
        txn_id: TxnId,
        descriptor_json: Vec<u8>,
    },
    /// Schema dropped.
    CatalogDropSchema { txn_id: TxnId, schema_id_raw: u64 },
    /// Role created.
    CatalogCreateRole {
        txn_id: TxnId,
        descriptor_json: Vec<u8>,
    },
    /// Role altered.
    CatalogAlterRole {
        txn_id: TxnId,
        descriptor_json: Vec<u8>,
    },
    /// Role dropped.
    CatalogDropRole { txn_id: TxnId, role_name: String },
    /// View created.
    CatalogCreateView {
        txn_id: TxnId,
        descriptor_json: Vec<u8>,
    },
    /// View dropped.
    CatalogDropView { txn_id: TxnId, view_id_raw: u64 },
    /// Sequence created.
    CatalogCreateSequence {
        txn_id: TxnId,
        descriptor_json: Vec<u8>,
    },
    /// Sequence dropped.
    CatalogDropSequence { txn_id: TxnId, sequence_id_raw: u64 },
    /// Sequence altered (full new descriptor).
    CatalogAlterSequence {
        txn_id: TxnId,
        descriptor_json: Vec<u8>,
    },
    /// Function created.
    CatalogCreateFunction {
        txn_id: TxnId,
        descriptor_json: Vec<u8>,
    },
    /// Function dropped.
    CatalogDropFunction {
        txn_id: TxnId,
        function_name: String,
    },
    /// Trigger created.
    CatalogCreateTrigger {
        txn_id: TxnId,
        descriptor_json: Vec<u8>,
    },
    /// Trigger dropped.
    CatalogDropTrigger {
        txn_id: TxnId,
        trigger_name: String,
        table_name: String,
    },
    /// Privilege granted.
    CatalogGrantPrivilege {
        txn_id: TxnId,
        descriptor_json: Vec<u8>,
    },
    /// Privilege revoked.
    CatalogRevokePrivilege {
        txn_id: TxnId,
        descriptor_json: Vec<u8>,
    },
    /// Table descriptor updated (catalog-level, e.g. ALTER TABLE).
    CatalogSetTableDescriptor {
        txn_id: TxnId,
        descriptor_json: Vec<u8>,
    },
    /// Index descriptor updated (catalog-level, e.g. CREATE/ALTER INDEX).
    CatalogSetIndexDescriptor {
        txn_id: TxnId,
        descriptor_json: Vec<u8>,
    },
    /// Tenant created.
    CatalogCreateTenant {
        txn_id: TxnId,
        descriptor_json: Vec<u8>,
    },
    /// Tenant dropped.
    CatalogDropTenant { txn_id: TxnId, tenant_name: String },
    /// Table dropped from the catalog.
    CatalogDropTable { txn_id: TxnId, table_id_raw: u64 },
    /// Index dropped from the catalog.
    CatalogDropIndex { txn_id: TxnId, index_id_raw: u64 },
    /// Table statistics updated in the catalog.
    CatalogUpdateStatistics {
        txn_id: TxnId,
        descriptor_json: Vec<u8>,
    },
    /// Node label created.
    CatalogCreateNodeLabel {
        txn_id: TxnId,
        descriptor_json: Vec<u8>,
    },
    /// Edge label created.
    CatalogCreateEdgeLabel {
        txn_id: TxnId,
        descriptor_json: Vec<u8>,
    },
    /// Node label dropped.
    CatalogDropNodeLabel { txn_id: TxnId, label_name: String },
    /// Edge label dropped.
    CatalogDropEdgeLabel { txn_id: TxnId, label_name: String },
    /// Sequence runtime value updated.
    CatalogSetSequenceValue {
        txn_id: TxnId,
        sequence_id_raw: u64,
        current_value: i64,
        is_called: bool,
    },

    /// Domain type created (CREATE DOMAIN).
    CatalogCreateDomain {
        txn_id: TxnId,
        descriptor_json: Vec<u8>,
    },
    /// Domain type dropped (DROP DOMAIN).
    CatalogDropDomain { txn_id: TxnId, domain_name: String },
    /// Domain type altered (ALTER DOMAIN ADD/DROP CONSTRAINT, SET/DROP NOT
    /// NULL, SET/DROP DEFAULT, RENAME). The full updated descriptor is
    /// stored - replay overwrites the registry entry, matching the
    /// `create_function` shape used elsewhere in this module.
    CatalogAlterDomain {
        txn_id: TxnId,
        descriptor_json: Vec<u8>,
    },
    /// Composite / enum / shell type created (CREATE TYPE).
    CatalogCreateUserType {
        txn_id: TxnId,
        descriptor_json: Vec<u8>,
    },
    /// User-defined type dropped (DROP TYPE).
    CatalogDropUserType { txn_id: TxnId, type_name: String },
    /// User-defined type altered (ALTER TYPE … ADD VALUE / RENAME / etc.).
    /// The full updated descriptor is stored; replay overwrites.
    CatalogAlterUserType {
        txn_id: TxnId,
        descriptor_json: Vec<u8>,
    },
    /// User-defined cast created (CREATE CAST).
    CatalogCreateCast {
        txn_id: TxnId,
        descriptor_json: Vec<u8>,
    },
    /// User-defined cast dropped (DROP CAST). Identified by the
    /// (source, target) type-name pair.
    CatalogDropCast {
        txn_id: TxnId,
        source_type: String,
        target_type: String,
    },
    /// Row-level security policy created (CREATE POLICY).
    CatalogCreatePolicy {
        txn_id: TxnId,
        descriptor_json: Vec<u8>,
    },
    /// Row-level security policy dropped (DROP POLICY). Identified by
    /// the (name, table) pair, mirroring how `pg_policy` keys policies.
    CatalogDropPolicy {
        txn_id: TxnId,
        policy_name: String,
        table_name: String,
    },
    /// Row-level security policy altered. The full updated descriptor
    /// is stored; replay overwrites by `(name, table)` key.
    CatalogAlterPolicy {
        txn_id: TxnId,
        descriptor_json: Vec<u8>,
    },
    /// Rewrite rule created (CREATE RULE).
    CatalogCreateRule {
        txn_id: TxnId,
        descriptor_json: Vec<u8>,
    },
    /// Rewrite rule dropped (DROP RULE name ON table).
    CatalogDropRule {
        txn_id: TxnId,
        rule_name: String,
        table_name: String,
    },
    /// Compatibility comment upserted.
    CatalogSetComment {
        txn_id: TxnId,
        descriptor_json: Vec<u8>,
    },
    /// Compatibility comment dropped.
    CatalogDropComment {
        txn_id: TxnId,
        object_type: String,
        object_identity: String,
    },

    // -- Adjacency index records (tags 30..=32) -----------------------------
    /// Register an edge table for adjacency indexing, recording which columns
    /// hold the source and target node IDs.
    RegisterEdgeTable {
        table_id: RelationId,
        source_col: usize,
        target_col: usize,
    },
    /// Insert an edge into the adjacency index for a registered edge table.
    AdjacencyInsert {
        table_id: RelationId,
        source_id: Value,
        target_id: Value,
        edge_tuple_id: TupleId,
    },
    /// Remove an edge from the adjacency index for a registered edge table.
    AdjacencyRemove {
        table_id: RelationId,
        source_id: Value,
        target_id: Value,
        edge_tuple_id: TupleId,
    },
}

impl WalRecord {
    /// Returns a unique tag byte for each variant.
    pub fn tag(&self) -> u8 {
        match self {
            Self::BeginTxn { .. } => 0,
            Self::CommitTxn { .. } => 1,
            Self::AbortTxn { .. } => 2,
            Self::InsertRow { .. } => 3,
            Self::DeleteRow { .. } => 4,
            Self::UpdateRow { .. } => 5,
            Self::CreateTable { .. } => 6,
            Self::DropTable { .. } => 7,
            Self::CreateIndex { .. } => 8,
            Self::DropIndex { .. } => 9,
            Self::AlterTable { .. } => 10,
            Self::Checkpoint { .. } => 11,
            Self::UpdateStatistics { .. } => 12,
            // Catalog DDL records
            Self::CatalogCreateSchema { .. } => 13,
            Self::CatalogDropSchema { .. } => 14,
            Self::CatalogCreateRole { .. } => 15,
            Self::CatalogAlterRole { .. } => 16,
            Self::CatalogDropRole { .. } => 17,
            Self::CatalogCreateView { .. } => 18,
            Self::CatalogDropView { .. } => 19,
            Self::CatalogCreateSequence { .. } => 20,
            Self::CatalogDropSequence { .. } => 21,
            Self::CatalogAlterSequence { .. } => 22,
            Self::CatalogCreateFunction { .. } => 23,
            Self::CatalogDropFunction { .. } => 24,
            Self::CatalogCreateTrigger { .. } => 25,
            Self::CatalogDropTrigger { .. } => 26,
            Self::CatalogGrantPrivilege { .. } => 27,
            Self::CatalogRevokePrivilege { .. } => 28,
            Self::CatalogSetTableDescriptor { .. } => 29,
            // Adjacency index records
            Self::RegisterEdgeTable { .. } => 30,
            Self::AdjacencyInsert { .. } => 31,
            Self::AdjacencyRemove { .. } => 32,
            Self::CatalogDropTable { .. } => 33,
            Self::CatalogDropIndex { .. } => 34,
            Self::CatalogUpdateStatistics { .. } => 35,
            Self::CatalogCreateNodeLabel { .. } => 36,
            Self::CatalogCreateEdgeLabel { .. } => 37,
            Self::CatalogDropNodeLabel { .. } => 38,
            Self::CatalogDropEdgeLabel { .. } => 39,
            Self::CatalogSetIndexDescriptor { .. } => 40,
            Self::CatalogCreateTenant { .. } => 41,
            Self::CatalogDropTenant { .. } => 42,
            Self::CatalogSetSequenceValue { .. } => 43,
            Self::FullPageImage { .. } => 44,
            Self::PagedRowRef { .. } => 45,
            Self::FullPageImageBatch { .. } => 46,
            Self::PagePatch { .. } => 47,
            Self::PagePatchBatch { .. } => 48,
            Self::PageSetU64Batch { .. } => 49,
            Self::DiskBtreeMetaUpdate { .. } => 50,
            Self::DiskBtreeLeafInsert { .. } => 51,
            Self::DiskBtreeLeafDelete { .. } => 52,
            Self::DiskBtreeLeafSplit { .. } => 53,
            Self::DiskBtreeInternalInsert { .. } => 54,
            Self::DiskBtreeInternalSplit { .. } => 55,
            Self::DiskBtreeRootGrow { .. } => 56,
            Self::DiskBtreeInternalDelete { .. } => 57,
            Self::DiskBtreeLeafRedistribute { .. } => 58,
            Self::DiskBtreeInternalRedistribute { .. } => 59,
            Self::DiskBtreeLeafMerge { .. } => 60,
            Self::DiskBtreeInternalMerge { .. } => 61,
            Self::DiskBtreeRootShrinkLeaf { .. } => 62,
            Self::DiskBtreeRootShrinkInternal { .. } => 63,
            Self::DiskBtreeInternalCollapse { .. } => 64,
            Self::DiskBtreeRootPromoteSingleChild { .. } => 65,
            Self::DiskBtreeRootPromoteCollapsedChain { .. } => 66,
            Self::DiskBtreeInternalCollapseChain { .. } => 67,
            Self::AutocommitInsertRow { .. } => 68,
            Self::AutocommitDeleteRow { .. } => 69,
            Self::AutocommitUpdateRow { .. } => 70,
            Self::CatalogCreateDomain { .. } => 71,
            Self::CatalogDropDomain { .. } => 72,
            Self::CatalogAlterDomain { .. } => 73,
            Self::CatalogCreateUserType { .. } => 74,
            Self::CatalogDropUserType { .. } => 75,
            Self::CatalogAlterUserType { .. } => 76,
            Self::CatalogCreateCast { .. } => 77,
            Self::CatalogDropCast { .. } => 78,
            Self::CatalogCreatePolicy { .. } => 79,
            Self::CatalogDropPolicy { .. } => 80,
            Self::CatalogAlterPolicy { .. } => 81,
            Self::CatalogCreateRule { .. } => 82,
            Self::CatalogDropRule { .. } => 83,
            Self::CatalogSetComment { .. } => 84,
            Self::CatalogDropComment { .. } => 85,
        }
    }

    /// Returns the transaction id for all variants except `Checkpoint`,
    /// `UpdateStatistics`, and adjacency index records.
    pub fn txn_id(&self) -> Option<TxnId> {
        match self {
            Self::BeginTxn { txn_id, .. }
            | Self::CommitTxn { txn_id, .. }
            | Self::AbortTxn { txn_id }
            | Self::InsertRow { txn_id, .. }
            | Self::DeleteRow { txn_id, .. }
            | Self::UpdateRow { txn_id, .. }
            | Self::AutocommitInsertRow { txn_id, .. }
            | Self::AutocommitDeleteRow { txn_id, .. }
            | Self::AutocommitUpdateRow { txn_id, .. }
            | Self::PagedRowRef { txn_id, .. }
            | Self::CreateTable { txn_id, .. }
            | Self::DropTable { txn_id, .. }
            | Self::CreateIndex { txn_id, .. }
            | Self::DropIndex { txn_id, .. }
            | Self::AlterTable { txn_id, .. }
            | Self::CatalogCreateSchema { txn_id, .. }
            | Self::CatalogDropSchema { txn_id, .. }
            | Self::CatalogCreateRole { txn_id, .. }
            | Self::CatalogAlterRole { txn_id, .. }
            | Self::CatalogDropRole { txn_id, .. }
            | Self::CatalogCreateView { txn_id, .. }
            | Self::CatalogDropView { txn_id, .. }
            | Self::CatalogCreateSequence { txn_id, .. }
            | Self::CatalogDropSequence { txn_id, .. }
            | Self::CatalogAlterSequence { txn_id, .. }
            | Self::CatalogCreateFunction { txn_id, .. }
            | Self::CatalogDropFunction { txn_id, .. }
            | Self::CatalogCreateTrigger { txn_id, .. }
            | Self::CatalogDropTrigger { txn_id, .. }
            | Self::CatalogGrantPrivilege { txn_id, .. }
            | Self::CatalogRevokePrivilege { txn_id, .. }
            | Self::CatalogSetTableDescriptor { txn_id, .. }
            | Self::CatalogSetIndexDescriptor { txn_id, .. }
            | Self::CatalogCreateTenant { txn_id, .. }
            | Self::CatalogDropTenant { txn_id, .. }
            | Self::CatalogSetSequenceValue { txn_id, .. }
            | Self::CatalogDropTable { txn_id, .. }
            | Self::CatalogDropIndex { txn_id, .. }
            | Self::CatalogUpdateStatistics { txn_id, .. }
            | Self::CatalogCreateNodeLabel { txn_id, .. }
            | Self::CatalogCreateEdgeLabel { txn_id, .. }
            | Self::CatalogDropNodeLabel { txn_id, .. }
            | Self::CatalogDropEdgeLabel { txn_id, .. }
            | Self::CatalogCreateDomain { txn_id, .. }
            | Self::CatalogDropDomain { txn_id, .. }
            | Self::CatalogAlterDomain { txn_id, .. }
            | Self::CatalogCreateUserType { txn_id, .. }
            | Self::CatalogDropUserType { txn_id, .. }
            | Self::CatalogAlterUserType { txn_id, .. }
            | Self::CatalogCreateCast { txn_id, .. }
            | Self::CatalogDropCast { txn_id, .. }
            | Self::CatalogCreatePolicy { txn_id, .. }
            | Self::CatalogDropPolicy { txn_id, .. }
            | Self::CatalogAlterPolicy { txn_id, .. }
            | Self::CatalogCreateRule { txn_id, .. }
            | Self::CatalogDropRule { txn_id, .. }
            | Self::CatalogSetComment { txn_id, .. }
            | Self::CatalogDropComment { txn_id, .. } => Some(*txn_id),
            Self::Checkpoint { .. }
            | Self::UpdateStatistics { .. }
            | Self::FullPageImage { .. }
            | Self::FullPageImageBatch { .. }
            | Self::PagePatch { .. }
            | Self::PagePatchBatch { .. }
            | Self::PageSetU64Batch { .. }
            | Self::DiskBtreeMetaUpdate { .. }
            | Self::DiskBtreeLeafInsert { .. }
            | Self::DiskBtreeLeafDelete { .. }
            | Self::DiskBtreeLeafSplit { .. }
            | Self::DiskBtreeInternalInsert { .. }
            | Self::DiskBtreeInternalSplit { .. }
            | Self::DiskBtreeRootGrow { .. }
            | Self::DiskBtreeInternalDelete { .. }
            | Self::DiskBtreeLeafRedistribute { .. }
            | Self::DiskBtreeInternalRedistribute { .. }
            | Self::DiskBtreeLeafMerge { .. }
            | Self::DiskBtreeInternalMerge { .. }
            | Self::DiskBtreeRootShrinkLeaf { .. }
            | Self::DiskBtreeRootShrinkInternal { .. }
            | Self::DiskBtreeInternalCollapse { .. }
            | Self::DiskBtreeRootPromoteSingleChild { .. }
            | Self::DiskBtreeRootPromoteCollapsedChain { .. }
            | Self::DiskBtreeInternalCollapseChain { .. }
            | Self::RegisterEdgeTable { .. }
            | Self::AdjacencyInsert { .. }
            | Self::AdjacencyRemove { .. } => None,
        }
    }

    /// Returns `true` if this record is a catalog DDL operation.
    pub fn is_catalog_record(&self) -> bool {
        matches!(
            self,
            Self::CatalogCreateSchema { .. }
                | Self::CatalogDropSchema { .. }
                | Self::CatalogCreateRole { .. }
                | Self::CatalogAlterRole { .. }
                | Self::CatalogDropRole { .. }
                | Self::CatalogCreateView { .. }
                | Self::CatalogDropView { .. }
                | Self::CatalogCreateSequence { .. }
                | Self::CatalogDropSequence { .. }
                | Self::CatalogAlterSequence { .. }
                | Self::CatalogCreateFunction { .. }
                | Self::CatalogDropFunction { .. }
                | Self::CatalogCreateTrigger { .. }
                | Self::CatalogDropTrigger { .. }
                | Self::CatalogGrantPrivilege { .. }
                | Self::CatalogRevokePrivilege { .. }
                | Self::CatalogSetTableDescriptor { .. }
                | Self::CatalogSetIndexDescriptor { .. }
                | Self::CatalogCreateTenant { .. }
                | Self::CatalogDropTenant { .. }
                | Self::CatalogSetSequenceValue { .. }
                | Self::CatalogDropTable { .. }
                | Self::CatalogDropIndex { .. }
                | Self::CatalogUpdateStatistics { .. }
                | Self::CatalogCreateNodeLabel { .. }
                | Self::CatalogCreateEdgeLabel { .. }
                | Self::CatalogDropNodeLabel { .. }
                | Self::CatalogDropEdgeLabel { .. }
                | Self::CatalogCreateDomain { .. }
                | Self::CatalogDropDomain { .. }
                | Self::CatalogAlterDomain { .. }
                | Self::CatalogCreateUserType { .. }
                | Self::CatalogDropUserType { .. }
                | Self::CatalogAlterUserType { .. }
                | Self::CatalogCreateCast { .. }
                | Self::CatalogDropCast { .. }
                | Self::CatalogCreatePolicy { .. }
                | Self::CatalogDropPolicy { .. }
                | Self::CatalogAlterPolicy { .. }
                | Self::CatalogCreateRule { .. }
                | Self::CatalogDropRule { .. }
                | Self::CatalogSetComment { .. }
                | Self::CatalogDropComment { .. }
        )
    }

    /// Returns `true` if this record is an adjacency index operation (tags 30..=32).
    pub fn is_adjacency_record(&self) -> bool {
        let tag = self.tag();
        (30..=32).contains(&tag)
    }
}

/// Number of `WalRecord` variants frozen for the v0.2 release line.
///
/// Bumping this requires updating [`FROZEN_WAL_RECORD_TAGS_V0_2`] in lock-step.
/// The frozen test `frozen_wal_tag_table_is_dense_and_unique` enforces that
/// `tag()` returns a unique value in `0..FROZEN_WAL_RECORD_TAG_COUNT_V0_2` for
/// every variant.
pub const FROZEN_WAL_RECORD_TAG_COUNT_V0_2: usize = 86;

/// Frozen on-disk tag table for the WAL record kinds shipped in v0.2.
///
/// Every entry is `(tag, variant_name)`. The order of this slice is also the
/// canonical documentation order: see `docs/content/documentation/learn/
/// wal-contract.md`. Adding a new variant means:
///
/// 1. Append it to the end of `WalRecord` and `tag()`.
/// 2. Append the matching `(tag, "VariantName")` entry to this slice.
/// 3. Bump [`FROZEN_WAL_RECORD_TAG_COUNT_V0_2`] and the doc page.
///
/// Reordering, renumbering, or removing an existing entry is a breaking change
/// and is forbidden inside the v0.2 line.
pub const FROZEN_WAL_RECORD_TAGS_V0_2: &[(u8, &str)] = &[
    (0, "BeginTxn"),
    (1, "CommitTxn"),
    (2, "AbortTxn"),
    (3, "InsertRow"),
    (4, "DeleteRow"),
    (5, "UpdateRow"),
    (6, "CreateTable"),
    (7, "DropTable"),
    (8, "CreateIndex"),
    (9, "DropIndex"),
    (10, "AlterTable"),
    (11, "Checkpoint"),
    (12, "UpdateStatistics"),
    (13, "CatalogCreateSchema"),
    (14, "CatalogDropSchema"),
    (15, "CatalogCreateRole"),
    (16, "CatalogAlterRole"),
    (17, "CatalogDropRole"),
    (18, "CatalogCreateView"),
    (19, "CatalogDropView"),
    (20, "CatalogCreateSequence"),
    (21, "CatalogDropSequence"),
    (22, "CatalogAlterSequence"),
    (23, "CatalogCreateFunction"),
    (24, "CatalogDropFunction"),
    (25, "CatalogCreateTrigger"),
    (26, "CatalogDropTrigger"),
    (27, "CatalogGrantPrivilege"),
    (28, "CatalogRevokePrivilege"),
    (29, "CatalogSetTableDescriptor"),
    (30, "RegisterEdgeTable"),
    (31, "AdjacencyInsert"),
    (32, "AdjacencyRemove"),
    (33, "CatalogDropTable"),
    (34, "CatalogDropIndex"),
    (35, "CatalogUpdateStatistics"),
    (36, "CatalogCreateNodeLabel"),
    (37, "CatalogCreateEdgeLabel"),
    (38, "CatalogDropNodeLabel"),
    (39, "CatalogDropEdgeLabel"),
    (40, "CatalogSetIndexDescriptor"),
    (41, "CatalogCreateTenant"),
    (42, "CatalogDropTenant"),
    (43, "CatalogSetSequenceValue"),
    (44, "FullPageImage"),
    (45, "PagedRowRef"),
    (46, "FullPageImageBatch"),
    (47, "PagePatch"),
    (48, "PagePatchBatch"),
    (49, "PageSetU64Batch"),
    (50, "DiskBtreeMetaUpdate"),
    (51, "DiskBtreeLeafInsert"),
    (52, "DiskBtreeLeafDelete"),
    (53, "DiskBtreeLeafSplit"),
    (54, "DiskBtreeInternalInsert"),
    (55, "DiskBtreeInternalSplit"),
    (56, "DiskBtreeRootGrow"),
    (57, "DiskBtreeInternalDelete"),
    (58, "DiskBtreeLeafRedistribute"),
    (59, "DiskBtreeInternalRedistribute"),
    (60, "DiskBtreeLeafMerge"),
    (61, "DiskBtreeInternalMerge"),
    (62, "DiskBtreeRootShrinkLeaf"),
    (63, "DiskBtreeRootShrinkInternal"),
    (64, "DiskBtreeInternalCollapse"),
    (65, "DiskBtreeRootPromoteSingleChild"),
    (66, "DiskBtreeRootPromoteCollapsedChain"),
    (67, "DiskBtreeInternalCollapseChain"),
    (68, "AutocommitInsertRow"),
    (69, "AutocommitDeleteRow"),
    (70, "AutocommitUpdateRow"),
    (71, "CatalogCreateDomain"),
    (72, "CatalogDropDomain"),
    (73, "CatalogAlterDomain"),
    (74, "CatalogCreateUserType"),
    (75, "CatalogDropUserType"),
    (76, "CatalogAlterUserType"),
    (77, "CatalogCreateCast"),
    (78, "CatalogDropCast"),
    (79, "CatalogCreatePolicy"),
    (80, "CatalogDropPolicy"),
    (81, "CatalogAlterPolicy"),
    (82, "CatalogCreateRule"),
    (83, "CatalogDropRule"),
    (84, "CatalogSetComment"),
    (85, "CatalogDropComment"),
];

/// A WAL entry = LSN + database id + record.
#[derive(Clone, Debug, PartialEq)]
pub struct WalEntry {
    pub lsn: Lsn,
    /// Backward link to the previous WAL record (`xl_prev`-style chain).
    ///
    /// `Lsn::ZERO` denotes the start of the WAL stream (no previous record)
    /// or entries that were encoded before backward links existed.
    pub prev_lsn: Lsn,
    /// Database id owning this record (ADR-0014 phase 4bis).
    ///
    /// Default `1` = `DatabaseId::DEFAULT`. WAL segments that do not carry
    /// the field on disk are read back with this value.
    /// Recovery consumers can filter entries by `database_id` to
    /// replay a single base without cross-contamination.
    pub database_id: u32,
    pub record: WalRecord,
}

impl WalEntry {
    /// Default database id assumed for WAL entries whose durable
    /// representation predates the multi-catalog work.
    pub const LEGACY_DATABASE_ID: u32 = 1;

    /// Convenience constructor tagging the entry with `LEGACY_DATABASE_ID`.
    pub fn with_default_database_id(lsn: Lsn, prev_lsn: Lsn, record: WalRecord) -> Self {
        Self {
            lsn,
            prev_lsn,
            database_id: Self::LEGACY_DATABASE_ID,
            record,
        }
    }

    /// Explicit-database constructor.
    pub fn with_database(lsn: Lsn, prev_lsn: Lsn, database_id: u32, record: WalRecord) -> Self {
        Self {
            lsn,
            prev_lsn,
            database_id,
            record,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aiondb_core::{ColumnId, DataType};
    use aiondb_storage_api::StorageColumn;

    fn txn(id: u64) -> TxnId {
        TxnId::new(id)
    }

    #[test]
    fn tag_begin_txn() {
        let r = WalRecord::BeginTxn {
            txn_id: txn(1),
            isolation: IsolationLevel::ReadCommitted,
        };
        assert_eq!(r.tag(), 0);
    }

    #[test]
    fn tag_commit_txn() {
        let r = WalRecord::CommitTxn {
            txn_id: txn(1),
            commit_ts: 100,
        };
        assert_eq!(r.tag(), 1);
    }

    #[test]
    fn tag_abort_txn() {
        let r = WalRecord::AbortTxn { txn_id: txn(1) };
        assert_eq!(r.tag(), 2);
    }

    #[test]
    fn tag_insert_row() {
        let r = WalRecord::InsertRow {
            txn_id: txn(1),
            table_id: RelationId::new(1),
            tuple_id: TupleId::new(1),
            row: Row::new(vec![]),
        };
        assert_eq!(r.tag(), 3);
    }

    #[test]
    fn tag_delete_row() {
        let r = WalRecord::DeleteRow {
            txn_id: txn(1),
            table_id: RelationId::new(1),
            tuple_id: TupleId::new(1),
        };
        assert_eq!(r.tag(), 4);
    }

    #[test]
    fn tag_update_row() {
        let r = WalRecord::UpdateRow {
            txn_id: txn(1),
            table_id: RelationId::new(1),
            old_tuple_id: TupleId::new(1),
            new_tuple_id: TupleId::new(2),
            row: Row::new(vec![]),
        };
        assert_eq!(r.tag(), 5);
    }

    #[test]
    fn tag_create_table() {
        let r = WalRecord::CreateTable {
            txn_id: txn(1),
            descriptor: TableStorageDescriptor {
                table_id: RelationId::new(1),
                columns: vec![],
                primary_key: None,
                shard_config: None,
            },
        };
        assert_eq!(r.tag(), 6);
    }

    #[test]
    fn tag_drop_table() {
        let r = WalRecord::DropTable {
            txn_id: txn(1),
            table_id: RelationId::new(1),
        };
        assert_eq!(r.tag(), 7);
    }

    #[test]
    fn tag_create_index() {
        let r = WalRecord::CreateIndex {
            txn_id: txn(1),
            descriptor: IndexStorageDescriptor {
                index_id: IndexId::new(1),
                table_id: RelationId::new(1),
                unique: false,
                gin: false,
                nulls_not_distinct: false,
                key_columns: vec![],
                include_columns: vec![],
                hnsw_options: None,
            ivf_flat_options: None,
            },
        };
        assert_eq!(r.tag(), 8);
    }

    #[test]
    fn tag_drop_index() {
        let r = WalRecord::DropIndex {
            txn_id: txn(1),
            index_id: IndexId::new(1),
        };
        assert_eq!(r.tag(), 9);
    }

    #[test]
    fn tag_alter_table() {
        let r = WalRecord::AlterTable {
            txn_id: txn(1),
            descriptor: TableStorageDescriptor {
                table_id: RelationId::new(1),
                columns: vec![],
                primary_key: None,
                shard_config: None,
            },
        };
        assert_eq!(r.tag(), 10);
    }

    #[test]
    fn tag_checkpoint() {
        let r = WalRecord::Checkpoint {
            last_committed_lsn: Lsn::ZERO,
        };
        assert_eq!(r.tag(), 11);
    }

    // txn_id tests

    #[test]
    fn txn_id_begin_txn() {
        let r = WalRecord::BeginTxn {
            txn_id: txn(42),
            isolation: IsolationLevel::SnapshotIsolation,
        };
        assert_eq!(r.txn_id(), Some(txn(42)));
    }

    #[test]
    fn txn_id_commit_txn() {
        let r = WalRecord::CommitTxn {
            txn_id: txn(7),
            commit_ts: 50,
        };
        assert_eq!(r.txn_id(), Some(txn(7)));
    }

    #[test]
    fn txn_id_abort_txn() {
        let r = WalRecord::AbortTxn { txn_id: txn(3) };
        assert_eq!(r.txn_id(), Some(txn(3)));
    }

    #[test]
    fn txn_id_insert_row() {
        let r = WalRecord::InsertRow {
            txn_id: txn(5),
            table_id: RelationId::new(1),
            tuple_id: TupleId::new(1),
            row: Row::new(vec![]),
        };
        assert_eq!(r.txn_id(), Some(txn(5)));
    }

    #[test]
    fn txn_id_delete_row() {
        let r = WalRecord::DeleteRow {
            txn_id: txn(6),
            table_id: RelationId::new(1),
            tuple_id: TupleId::new(1),
        };
        assert_eq!(r.txn_id(), Some(txn(6)));
    }

    #[test]
    fn txn_id_update_row() {
        let r = WalRecord::UpdateRow {
            txn_id: txn(8),
            table_id: RelationId::new(1),
            old_tuple_id: TupleId::new(1),
            new_tuple_id: TupleId::new(2),
            row: Row::new(vec![]),
        };
        assert_eq!(r.txn_id(), Some(txn(8)));
    }

    #[test]
    fn txn_id_create_table() {
        let r = WalRecord::CreateTable {
            txn_id: txn(10),
            descriptor: TableStorageDescriptor {
                table_id: RelationId::new(1),
                columns: vec![],
                primary_key: None,
                shard_config: None,
            },
        };
        assert_eq!(r.txn_id(), Some(txn(10)));
    }

    #[test]
    fn txn_id_drop_table() {
        let r = WalRecord::DropTable {
            txn_id: txn(11),
            table_id: RelationId::new(1),
        };
        assert_eq!(r.txn_id(), Some(txn(11)));
    }

    #[test]
    fn txn_id_create_index() {
        let r = WalRecord::CreateIndex {
            txn_id: txn(12),
            descriptor: IndexStorageDescriptor {
                index_id: IndexId::new(1),
                table_id: RelationId::new(1),
                unique: false,
                gin: false,
                nulls_not_distinct: false,
                key_columns: vec![],
                include_columns: vec![],
                hnsw_options: None,
            ivf_flat_options: None,
            },
        };
        assert_eq!(r.txn_id(), Some(txn(12)));
    }

    #[test]
    fn txn_id_drop_index() {
        let r = WalRecord::DropIndex {
            txn_id: txn(13),
            index_id: IndexId::new(1),
        };
        assert_eq!(r.txn_id(), Some(txn(13)));
    }

    #[test]
    fn txn_id_alter_table() {
        let desc = TableStorageDescriptor {
            table_id: RelationId::new(1),
            columns: vec![StorageColumn {
                column_id: ColumnId::new(1),
                data_type: DataType::Int,
                nullable: false,
            }],
            primary_key: Some(vec![ColumnId::new(1)]),
            shard_config: None,
        };
        let r = WalRecord::AlterTable {
            txn_id: txn(14),
            descriptor: desc,
        };
        assert_eq!(r.txn_id(), Some(txn(14)));
    }

    #[test]
    fn txn_id_checkpoint_returns_none() {
        let r = WalRecord::Checkpoint {
            last_committed_lsn: Lsn::new(999),
        };
        assert_eq!(r.txn_id(), None);
    }

    #[test]
    fn tag_update_statistics() {
        let r = WalRecord::UpdateStatistics {
            table_id: RelationId::new(1),
            row_count: 100,
            total_bytes: 2048,
            dead_row_count: 5,
            column_stats: vec![(ColumnId::new(1), 10.0, 0.05, 4)],
        };
        assert_eq!(r.tag(), 12);
    }

    #[test]
    fn txn_id_update_statistics_returns_none() {
        let r = WalRecord::UpdateStatistics {
            table_id: RelationId::new(1),
            row_count: 100,
            total_bytes: 2048,
            dead_row_count: 5,
            column_stats: vec![],
        };
        assert_eq!(r.txn_id(), None);
    }

    #[test]
    fn tag_full_page_image() {
        let r = WalRecord::FullPageImage {
            relation_id: RelationId::new(9),
            page_number: 3,
            page_data: vec![0xAA; 8192],
        };
        assert_eq!(r.tag(), 44);
    }

    #[test]
    fn txn_id_full_page_image_returns_none() {
        let r = WalRecord::FullPageImage {
            relation_id: RelationId::new(9),
            page_number: 3,
            page_data: vec![0xBB; 8192],
        };
        assert_eq!(r.txn_id(), None);
    }

    #[test]
    fn tag_catalog_set_index_descriptor() {
        let r = WalRecord::CatalogSetIndexDescriptor {
            txn_id: txn(15),
            descriptor_json: vec![1, 2, 3],
        };
        assert_eq!(r.tag(), 40);
    }

    #[test]
    fn txn_id_catalog_set_index_descriptor() {
        let r = WalRecord::CatalogSetIndexDescriptor {
            txn_id: txn(16),
            descriptor_json: vec![4, 5, 6],
        };
        assert_eq!(r.txn_id(), Some(txn(16)));
    }

    #[test]
    fn tag_catalog_create_tenant() {
        let r = WalRecord::CatalogCreateTenant {
            txn_id: txn(17),
            descriptor_json: vec![7, 8, 9],
        };
        assert_eq!(r.tag(), 41);
    }

    #[test]
    fn txn_id_catalog_create_tenant() {
        let r = WalRecord::CatalogCreateTenant {
            txn_id: txn(18),
            descriptor_json: vec![10, 11, 12],
        };
        assert_eq!(r.txn_id(), Some(txn(18)));
    }

    #[test]
    fn tag_catalog_drop_tenant() {
        let r = WalRecord::CatalogDropTenant {
            txn_id: txn(19),
            tenant_name: "acme".to_owned(),
        };
        assert_eq!(r.tag(), 42);
    }

    #[test]
    fn txn_id_catalog_drop_tenant() {
        let r = WalRecord::CatalogDropTenant {
            txn_id: txn(20),
            tenant_name: "acme".to_owned(),
        };
        assert_eq!(r.txn_id(), Some(txn(20)));
    }

    #[test]
    fn tags_are_unique() {
        use std::collections::HashSet;
        let tags: Vec<u8> = (0..=42).collect();
        let set: HashSet<u8> = tags.iter().copied().collect();
        assert_eq!(set.len(), 43);
    }

    /// The frozen tag table must be dense over `0..FROZEN_WAL_RECORD_TAG_COUNT_V0_2`,
    /// each tag must appear exactly once, and each variant name must appear
    /// exactly once.
    #[test]
    fn frozen_wal_tag_table_is_dense_and_unique() {
        use std::collections::HashSet;

        assert_eq!(
            FROZEN_WAL_RECORD_TAGS_V0_2.len(),
            FROZEN_WAL_RECORD_TAG_COUNT_V0_2,
            "v0.2 freeze: WAL record tag table length must match the constant; \
             adding a variant requires updating both",
        );

        let mut tag_set: HashSet<u8> = HashSet::new();
        let mut name_set: HashSet<&str> = HashSet::new();
        for (index, (tag, name)) in FROZEN_WAL_RECORD_TAGS_V0_2.iter().enumerate() {
            let expected = u8::try_from(index).expect("index fits in u8");
            assert_eq!(
                *tag, expected,
                "v0.2 freeze: tag at slot {index} must be {expected} (got {tag} for {name})",
            );
            assert!(
                tag_set.insert(*tag),
                "v0.2 freeze: duplicate tag {tag} for {name}",
            );
            assert!(
                name_set.insert(name),
                "v0.2 freeze: duplicate variant name {name}",
            );
        }
    }

    /// Spot-check a representative sample of variants against the frozen
    /// table. The full table is exercised by the unique-and-dense test above
    /// plus the per-variant `tag_*` tests in this module.
    #[test]
    fn frozen_wal_tag_table_matches_record_tag() {
        let samples: Vec<(WalRecord, u8)> = vec![
            (
                WalRecord::BeginTxn {
                    txn_id: txn(1),
                    isolation: IsolationLevel::ReadCommitted,
                },
                0,
            ),
            (
                WalRecord::CommitTxn {
                    txn_id: txn(1),
                    commit_ts: 0,
                },
                1,
            ),
            (WalRecord::AbortTxn { txn_id: txn(1) }, 2),
            (
                WalRecord::Checkpoint {
                    last_committed_lsn: Lsn::new(0),
                },
                11,
            ),
            (
                WalRecord::FullPageImage {
                    relation_id: RelationId::new(1),
                    page_number: 0,
                    page_data: vec![0u8; 0],
                },
                44,
            ),
            (
                WalRecord::AutocommitDeleteRow {
                    txn_id: txn(1),
                    table_id: RelationId::new(1),
                    tuple_id: TupleId::new(1),
                },
                69,
            ),
            (
                WalRecord::CatalogDropComment {
                    txn_id: txn(1),
                    object_type: "table".to_owned(),
                    object_identity: "public.t".to_owned(),
                },
                85,
            ),
        ];

        for (record, expected_tag) in samples {
            assert_eq!(
                record.tag(),
                expected_tag,
                "tag drift detected for {record:?}",
            );
            let table_entry = FROZEN_WAL_RECORD_TAGS_V0_2
                .iter()
                .find(|(tag, _)| *tag == expected_tag)
                .expect("frozen table must contain every sampled tag");
            assert_eq!(table_entry.0, expected_tag);
        }
    }
}
