/// Accumulate rows-returned and rows-affected counts from statement results.
pub(super) fn accumulate_statement_metrics(results: &[StatementResult]) -> (u64, u64) {
    results.iter().fold((0u64, 0u64), |acc, r| match r {
        StatementResult::Query { rows, .. } => (
            acc.0
                .saturating_add(aiondb_core::convert::usize_to_u64_saturating(rows.len())),
            acc.1,
        ),
        StatementResult::Command { rows_affected, .. } => {
            (acc.0, acc.1.saturating_add(*rows_affected))
        }
        StatementResult::CopyIn { .. }
        | StatementResult::CopyOut { .. }
        | StatementResult::Notice { .. } => acc,
    })
}

pub(super) fn enforce_cumulative_statement_result_limits(
    results: &[StatementResult],
    total_rows: &mut u64,
    total_bytes: &mut u64,
    max_rows: u64,
    max_bytes: u64,
) -> DbResult<()> {
    for result in results {
        let StatementResult::Query { rows, .. } = result else {
            continue;
        };
        for row in rows {
            if *total_rows >= max_rows {
                return Err(DbError::program_limit(
                    "maximum number of result rows reached",
                ));
            }
            let row_bytes = estimate_query_row_bytes(row);
            let next_total_bytes = total_bytes.saturating_add(row_bytes);
            if next_total_bytes > max_bytes {
                return Err(DbError::program_limit(
                    "maximum number of result bytes reached",
                ));
            }
            *total_rows = total_rows.saturating_add(1);
            *total_bytes = next_total_bytes;
        }
    }
    Ok(())
}

pub(super) fn estimate_query_row_bytes(row: &aiondb_core::Row) -> u64 {
    row.values.iter().fold(0u64, |acc, value| {
        acc.saturating_add(estimate_query_value_bytes(value))
    })
}

const MAX_QUERY_VALUE_ESTIMATE_DEPTH: usize = 256;
const DEEP_QUERY_VALUE_ESTIMATED_BYTES: u64 = 1 << 20;

pub(super) fn estimate_query_value_bytes(value: &Value) -> u64 {
    estimate_query_value_bytes_at_depth(value, 0)
}

fn estimate_query_value_bytes_at_depth(value: &Value, depth: usize) -> u64 {
    if depth >= MAX_QUERY_VALUE_ESTIMATE_DEPTH {
        return DEEP_QUERY_VALUE_ESTIMATED_BYTES;
    }
    match value {
        Value::Null => 1,
        Value::Int(_) => 4,
        Value::BigInt(_) => 8,
        Value::Real(_) => 4,
        Value::Double(_) => 8,
        Value::Numeric(_) => 20,
        Value::Money(_) => 8,
        Value::Text(text) => aiondb_core::convert::usize_to_u64_saturating(text.len()),
        Value::Boolean(_) => 1,
        Value::Blob(bytes) => aiondb_core::convert::usize_to_u64_saturating(bytes.len()),
        Value::Timestamp(_) => 16,
        Value::Date(_) => 8,
        Value::LargeDate(_) => 12,
        Value::Time(_) => 8,
        Value::TimeTz(_, _) => 12,
        Value::Interval(_) => 16,
        Value::Tid(_) => 8,
        Value::MacAddr(_) => 6,
        Value::MacAddr8(_) => 8,
        Value::PgLsn(_) => 8,
        Value::Uuid(_) => 16,
        Value::TimestampTz(_) => 16,
        Value::Jsonb(v) => estimate_json_value_bytes_at_depth(v, 0),
        Value::Vector(vector) => 4u64.saturating_add(
            aiondb_core::convert::usize_to_u64_saturating(vector.values.len()).saturating_mul(4),
        ),
        Value::Array(elements) => elements.iter().fold(8u64, |acc, element| {
            acc.saturating_add(estimate_query_value_bytes_at_depth(element, depth + 1))
        }),
    }
}

fn estimate_json_value_bytes_at_depth(value: &serde_json::Value, depth: usize) -> u64 {
    if depth >= MAX_QUERY_VALUE_ESTIMATE_DEPTH {
        return DEEP_QUERY_VALUE_ESTIMATED_BYTES;
    }
    match value {
        serde_json::Value::Null => 4,
        serde_json::Value::Bool(false) => 5,
        serde_json::Value::Bool(true) => 4,
        serde_json::Value::Number(number) => {
            aiondb_core::convert::usize_to_u64_saturating(number.to_string().len())
        }
        serde_json::Value::String(text) => {
            // Approximate serialized size: quotes + payload (escaped form can be larger).
            2u64.saturating_add(aiondb_core::convert::usize_to_u64_saturating(text.len()))
        }
        serde_json::Value::Array(items) => {
            if items.is_empty() {
                return 2;
            }
            let mut total = 1u64;
            for item in items {
                total = total
                    .saturating_add(estimate_json_value_bytes_at_depth(item, depth + 1))
                    .saturating_add(1);
            }
            total
        }
        serde_json::Value::Object(map) => {
            if map.is_empty() {
                return 2;
            }
            let mut total = 1u64;
            for (key, item) in map {
                total = total
                    .saturating_add(2) // key quotes
                    .saturating_add(aiondb_core::convert::usize_to_u64_saturating(key.len()))
                    .saturating_add(1) // colon
                    .saturating_add(estimate_json_value_bytes_at_depth(item, depth + 1))
                    .saturating_add(1); // comma
            }
            total
        }
    }
}

#[cfg(test)]
mod cumulative_result_limit_tests {
    use super::*;

    #[test]
    fn enforce_cumulative_statement_result_limits_rejects_excess_rows() {
        let results = vec![StatementResult::Query {
            columns: vec![],
            rows: vec![
                aiondb_core::Row::new(vec![Value::Int(1)]),
                aiondb_core::Row::new(vec![Value::Int(2)]),
            ],
        }];

        let mut total_rows = 0u64;
        let mut total_bytes = 0u64;
        let err = enforce_cumulative_statement_result_limits(
            &results,
            &mut total_rows,
            &mut total_bytes,
            1,
            u64::MAX,
        )
        .expect_err("row limit should reject second row");
        assert_eq!(err.sqlstate(), aiondb_core::SqlState::ProgramLimitExceeded);
    }

    #[test]
    fn enforce_cumulative_statement_result_limits_rejects_excess_bytes() {
        let results = vec![StatementResult::Query {
            columns: vec![],
            rows: vec![aiondb_core::Row::new(vec![Value::Blob(vec![0u8; 32])])],
        }];

        let mut total_rows = 0u64;
        let mut total_bytes = 0u64;
        let err = enforce_cumulative_statement_result_limits(
            &results,
            &mut total_rows,
            &mut total_bytes,
            u64::MAX,
            8,
        )
        .expect_err("byte limit should reject oversized row");
        assert_eq!(err.sqlstate(), aiondb_core::SqlState::ProgramLimitExceeded);
    }
}

fn elapsed_micros_u64(start: &Instant) -> u64 {
    u64::try_from(start.elapsed().as_micros()).unwrap_or(u64::MAX)
}

pub(super) fn unknown_prepared_statement_error() -> DbError {
    DbError::parse_error(
        aiondb_core::SqlState::UndefinedObject,
        "unknown prepared statement",
    )
}

fn unknown_portal_error() -> DbError {
    DbError::parse_error(aiondb_core::SqlState::UndefinedObject, "unknown portal")
}

fn bind_parameter_count_error(expected: usize, actual: usize) -> DbError {
    DbError::parse_error(
        aiondb_core::SqlState::InvalidParameterValue,
        format!("expected {expected} bound parameter(s), received {actual}"),
    )
}

fn missing_compat_prepared_statement_error(name: &str) -> DbError {
    DbError::parse_error(
        aiondb_core::SqlState::UndefinedObject,
        format!("prepared statement \"{name}\" does not exist"),
    )
}

fn compat_missing_cursor_error(portal_name: &str) -> DbError {
    DbError::parse_error(
        aiondb_core::SqlState::InvalidCursorName,
        format!("cursor \"{portal_name}\" does not exist"),
    )
}

pub(super) fn failed_transaction_error() -> DbError {
    DbError::transaction_error(
        aiondb_core::SqlState::InFailedSqlTransaction,
        FAILED_TRANSACTION_MESSAGE,
    )
}

pub(super) fn parse_compat_prepare_transaction_gid(sql: &str) -> Option<String> {
    parse_compat_prepared_xact_gid(sql, "prepare", "transaction")
}

pub(super) fn parse_compat_commit_prepared_gid(sql: &str) -> Option<String> {
    parse_compat_prepared_xact_gid(sql, "commit", "prepared")
}

pub(super) fn parse_compat_rollback_prepared_gid(sql: &str) -> Option<String> {
    parse_compat_prepared_xact_gid(sql, "rollback", "prepared")
}

fn parse_compat_prepared_xact_gid(sql: &str, first: &str, second: &str) -> Option<String> {
    let sql = trim_compat_statement(sql);
    parse_compat_prepared_xact_gid_with_prefix(sql, &[first, second]).or_else(|| {
        // Some parser spans for COMMIT/ROLLBACK PREPARED currently start at PREPARED.
        // Accept that fragment only for prepared-xact command tags.
        parse_compat_prepared_xact_gid_with_prefix(sql, &[second])
    })
}

fn parse_compat_prepared_xact_gid_with_prefix(sql: &str, prefix: &[&str]) -> Option<String> {
    let mut cursor = 0usize;
    for word in prefix {
        consume_word_ci(sql, &mut cursor, word)?;
    }
    skip_sql_whitespace(sql, &mut cursor);
    let gid = if sql.get(cursor..)?.starts_with('\'') {
        parse_compat_single_quoted_string(sql, &mut cursor)?
    } else {
        parse_compat_identifier(sql, &mut cursor)?
    };
    skip_sql_whitespace(sql, &mut cursor);
    if cursor != sql.len() {
        return None;
    }
    Some(gid)
}

fn parse_compat_single_quoted_string(sql: &str, cursor: &mut usize) -> Option<String> {
    if !sql.get(*cursor..)?.starts_with('\'') {
        return None;
    }
    *cursor += 1;
    let mut out = String::new();
    while *cursor < sql.len() {
        let ch = sql.get(*cursor..)?.chars().next()?;
        *cursor += ch.len_utf8();
        if ch == '\'' {
            if sql.get(*cursor..)?.starts_with('\'') {
                *cursor += 1;
                out.push('\'');
                continue;
            }
            return Some(out);
        }
        out.push(ch);
    }
    None
}
