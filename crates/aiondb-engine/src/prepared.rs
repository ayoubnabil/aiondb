#![allow(clippy::pedantic)]

use aiondb_core::{DataType, RelationId, Row, TextTypeModifier, Value};
use aiondb_parser::Statement;
use std::sync::Arc;

pub const PORTAL_BATCH_COPY_OUT_TAG: &str = "__aiondb_copy_out__";
const PORTAL_BATCH_COPY_IN_PREFIX: &str = "__aiondb_copy_in__:";

#[must_use]
pub fn portal_batch_copy_in_tag(table_id: RelationId, column_count: usize) -> String {
    format!(
        "{PORTAL_BATCH_COPY_IN_PREFIX}{}:{column_count}",
        table_id.get()
    )
}

#[must_use]
pub fn decode_portal_batch_copy_in_tag(tag: &str) -> Option<(RelationId, usize)> {
    let payload = tag.strip_prefix(PORTAL_BATCH_COPY_IN_PREFIX)?;
    let (table_id_raw, column_count_raw) = payload.split_once(':')?;
    let table_id = table_id_raw.parse::<u64>().ok()?;
    let column_count = column_count_raw.parse::<usize>().ok()?;
    Some((RelationId::new(table_id), column_count))
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResultColumn {
    pub name: String,
    pub data_type: DataType,
    pub text_type_modifier: Option<TextTypeModifier>,
    pub nullable: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResultColumnOrigin {
    pub relation_id: RelationId,
    pub column_attr: i16,
}

#[derive(Clone, Debug, PartialEq)]
pub enum StatementResult {
    Query {
        columns: Vec<ResultColumn>,
        rows: Vec<Row>,
    },
    Command {
        tag: String,
        rows_affected: u64,
    },
    /// A NOTICE message emitted by the server (e.g., "table \"x\" does not
    /// exist, skipping" for DROP IF EXISTS).
    Notice {
        message: String,
    },
    CopyIn {
        table_id: aiondb_core::RelationId,
        columns: Vec<ResultColumn>,
    },
    CopyOut {
        data: String,
        column_count: usize,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreparedStatementDesc {
    pub name: String,
    pub param_types: Vec<DataType>,
    pub result_columns: Vec<ResultColumn>,
    pub result_column_origins: Vec<Option<ResultColumnOrigin>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PortalDescription {
    pub result_columns: Vec<ResultColumn>,
    pub result_column_origins: Vec<Option<ResultColumnOrigin>>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PortalBatch {
    pub columns: Vec<ResultColumn>,
    pub rows: Vec<Row>,
    pub tag: String,
    pub rows_affected: u64,
    pub exhausted: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct PreparedStatementState {
    pub sql: String,
    pub statement: Arc<Statement>,
    pub desc: PreparedStatementDesc,
    pub catalog_revision: u64,
    pub param_types: Arc<[DataType]>,
    pub contains_parameters: bool,
    pub uses_compat_command_hooks: bool,
    pub uses_compat_rule_dml: bool,
    pub may_use_drop_if_exists_notice: bool,
    pub parameterized_plan_literal_rewrite: bool,
    pub parameterized_plan_literal_rewrite_seeded: bool,
    pub plan_fingerprint: Option<crate::session::StatementFingerprint>,
    pub contains_recursive_cte: bool,
    pub needs_statement_sql_at_execute: bool,
    pub notice_free_execute: bool,
    /// Cached `parameterized_eq_bind_param_index(statement)`: `Some(i)`
    /// iff the prepared statement is a `WHERE col = $i` shape eligible
    /// for the literal-rewrite fast path. Computed once at PREPARE
    /// so we avoid re-walking the AST per EXECUTE.
    pub parameterized_eq_param_index: Option<usize>,
    /// Cached row slots for parameterized single-row INSERT VALUES.
    /// `Some(param_index)` means this VALUES slot is `$param_index`
    /// (zero-based); `None` means keep the literal from the seeded plan.
    pub parameterized_insert_values_param_slots: Option<Arc<[Option<usize>]>>,
}

#[derive(Clone, Debug)]
pub(crate) struct PortalState {
    pub statement_name: String,
    pub params: Vec<Value>,
    pub created_under_savepoint_generation: Option<u64>,
    pub position: usize,
    pub exhausted: bool,
    pub holdable: bool,
    pub scrollable: bool,
    /// The ctid of the row the cursor is currently positioned on (after the
    /// last FETCH).  Used to implement `WHERE CURRENT OF` rewrites.
    pub current_ctid: Option<String>,
    /// For compat cursors that inject a hidden trailing `ctid` column, this
    /// stores the internal column index so the wire-visible rowset can strip
    /// it while `CURRENT OF` still tracks row identity.
    pub hidden_ctid_column: Option<usize>,
    /// When known, the base table used by a compat cursor.  This lets
    /// `CURRENT OF` rewrite to `tableoid = ... AND ctid = ...`, preventing
    /// accidental cross-table updates on matching ctids.
    pub current_of_relation_id: Option<RelationId>,
    /// Optional visible result metadata override for compat cursors whose
    /// hidden bookkeeping columns should not be exposed to FETCH/DESCRIBE.
    pub visible_result_columns: Option<Vec<ResultColumn>>,
    pub visible_result_column_origins: Option<Vec<Option<ResultColumnOrigin>>>,
    /// Cached result set for scrollable cursors.  Populated on the first
    /// FETCH so that subsequent operations (including FETCH BACKWARD /
    /// FETCH FIRST and CURRENT OF) see a stable snapshot.
    pub cached_columns: Option<Vec<ResultColumn>>,
    pub cached_rows: Option<Vec<Row>>,
}
