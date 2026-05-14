//! COPY-protocol helpers: text/CSV formatting, options parsing,
//! WHERE predicate evaluation, trigger resolution.
//!
//! Kept outside `query_api.rs` so the query facade stays closer to protocol
//! orchestration. Visibility is `pub(in crate::engine)` so the outer COPY flow
//! in `execute_sql_statement_results` can still call these helpers.

#![allow(
    clippy::assigning_clones,
    clippy::map_unwrap_or,
    clippy::match_same_arms,
    clippy::redundant_closure_for_method_calls,
    clippy::too_many_lines,
    clippy::unnecessary_wraps
)]

use std::collections::HashSet;

use aiondb_core::{DataType, DbError, DbResult, Row, SqlState, Value};

use super::compat::{
    consume_word_ci, extract_parenthesized, parse_compat_identifier, parse_identifier_part,
    skip_sql_whitespace, trim_compat_statement,
};
use super::query_api::{
    decode_sql_single_quoted_literal, escape_copy_text_value, format_copy_csv_value,
    format_copy_text_value, split_top_level_csv_items, unescape_copy_text_value, CopyColumnCompat,
    CopyCompatFormat, CopyCompatOptions, CopyCsvField, CopyWhereOp, CopyWherePredicate,
};

include!("copy_support_body.rs");
