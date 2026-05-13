//! Compat-aggregate rewrite: pre-planning SQL rewrites that lower
//! PostgreSQL aggregate features AionDB doesn't implement natively
//! (ordered-set aggregates, DISTINCT + ORDER BY in `array_agg`,
//! multi-arg aggregates, `WITHIN GROUP` percentile_cont/disc/mode,
//! etc.) into a sequence of plain SELECT statements the planner can
//! handle.
//!
//! Kept outside `query_api.rs` so the query facade stays closer to protocol
//! orchestration. Visibility is `pub(in crate::engine)` so the outer aggregate
//! flow in `execute_sql_statement_results` can still call these helpers.

#![allow(
    clippy::doc_markdown,
    clippy::map_unwrap_or,
    clippy::redundant_closure_for_method_calls,
    clippy::too_many_lines,
    clippy::unnecessary_wraps
)]

use aiondb_core::{DbError, SqlState};

use super::compat::{
    consume_word_ci, extract_parenthesized, parse_compat_identifier, skip_sql_whitespace,
    trim_compat_statement,
};
use super::query_api::split_top_level_csv_items;
use crate::session::CompatAggregateRewrite;

include!("compat_aggregate_rewrite_body.rs");
