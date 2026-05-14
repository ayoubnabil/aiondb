use super::*;
use crate::connection::helpers::write_row_description_from_result_columns;
use aiondb_core::{DataType, TextTypeModifier};
use aiondb_engine::ResultColumn;
use aiondb_parser::Statement;
use std::borrow::Cow;

pub(super) fn write_statement_result(
    w: &mut MessageWriter,
    result: &StatementResult,
    statement: Option<&Statement>,
    result_column_origins: Option<&[Option<aiondb_engine::ResultColumnOrigin>]>,
) -> Result<(), DbError> {
    match result {
        StatementResult::Query { columns, rows } => {
            let patched_columns = patch_sequelize_show_indexes_columns(columns);
            for (row_index, row) in rows.iter().enumerate() {
                validate_row_width(
                    "query result",
                    row_index,
                    patched_columns.len(),
                    row.values.len(),
                )?;
            }
            write_row_description_from_result_columns(
                w,
                patched_columns.as_ref(),
                result_column_origins.unwrap_or(&[]),
                &[],
            )?;

            for row in rows {
                messages::write_data_row_direct_from_columns(
                    w,
                    &row.values,
                    patched_columns.as_ref(),
                    &[],
                )?;
            }

            write_query_command_complete(w, statement, rows.len());
        }
        StatementResult::Command { tag, rows_affected } => {
            write_command_complete_for(w, tag, *rows_affected);
        }
        StatementResult::CopyOut { data, column_count } => {
            copy::write_copy_out_result(w, data, *column_count)?;
        }
        StatementResult::CopyIn { .. } => {
            // CopyIn is handled in handle_simple_query; safe fallback.
            messages::write_command_complete(w, "COPY");
        }
        StatementResult::Notice { message } => {
            messages::write_notice_response(w, message);
        }
    }
    Ok(())
}

fn patch_sequelize_show_indexes_columns(columns: &[ResultColumn]) -> Cow<'_, [ResultColumn]> {
    let expected = [
        "name",
        "primary",
        "unique",
        "indkey",
        "column_indexes",
        "column_names",
        "definition",
    ];
    if columns.len() != expected.len()
        || columns
            .iter()
            .zip(expected)
            .any(|(column, expected)| !column.name.eq_ignore_ascii_case(expected))
    {
        return Cow::Borrowed(columns);
    }

    Cow::Owned(
        columns
            .iter()
            .map(|column| {
                let mut patched = column.clone();
                if patched.name.eq_ignore_ascii_case("indkey")
                    && matches!(patched.data_type, DataType::Array(_))
                {
                    patched.text_type_modifier = Some(TextTypeModifier::Int2Vector);
                } else if patched.name.eq_ignore_ascii_case("column_names")
                    && matches!(patched.data_type, DataType::Array(_))
                {
                    // PostgreSQL reports array_agg(name) as name[] (OID 1003).
                    // node-postgres leaves name[] as raw text, which Sequelize's
                    // showIndex parser expects and parses itself.
                    patched.text_type_modifier = Some(TextTypeModifier::Name);
                }
                patched
            })
            .collect(),
    )
}

pub(super) fn validate_row_width(
    context: &str,
    row_index: usize,
    expected_columns: usize,
    actual_columns: usize,
) -> Result<(), DbError> {
    if actual_columns != expected_columns {
        return Err(DbError::internal(format!(
            "{context} row {} has {actual_columns} value(s), but result metadata declares {expected_columns} column(s)",
            row_index + 1
        )));
    }
    Ok(())
}

pub(super) fn write_copy_in_completion_result(
    w: &mut MessageWriter,
    result: &StatementResult,
) -> Result<(), DbError> {
    match result {
        StatementResult::Notice { message } => {
            messages::write_notice_response(w, message);
            messages::write_command_complete(w, "COPY 0");
            Ok(())
        }
        _ => write_statement_result(w, result, None, None),
    }
}

/// Write the CommandComplete tag for a successful Query statement
/// directly into `w`, skipping the per-query `format!("SELECT {n}")`
/// String allocation.
fn write_query_command_complete(
    w: &mut MessageWriter,
    statement: Option<&Statement>,
    row_count: usize,
) {
    let count = row_count as u64;
    match statement {
        Some(Statement::Explain { .. }) => messages::write_command_complete(w, "EXPLAIN"),
        Some(Statement::ShowVariable(_)) => messages::write_command_complete(w, "SHOW"),
        Some(Statement::FetchStmt { .. }) => {
            messages::write_command_complete_with_count(w, "FETCH", false, count);
        }
        Some(Statement::CompatParserStub { tag, .. }) if tag == "FETCH" => {
            messages::write_command_complete_with_count(w, "FETCH", false, count);
        }
        Some(Statement::Insert(insert)) if !insert.returning.is_empty() => {
            messages::write_command_complete_with_count(w, "INSERT", true, count);
        }
        Some(Statement::Update(update)) if !update.returning.is_empty() => {
            messages::write_command_complete_with_count(w, "UPDATE", false, count);
        }
        Some(Statement::Delete(delete)) if !delete.returning.is_empty() => {
            messages::write_command_complete_with_count(w, "DELETE", false, count);
        }
        _ => messages::write_command_complete_with_count(w, "SELECT", false, count),
    }
}

/// Write the CommandComplete tag for a Command statement (tag plus
/// row count) directly into `w`. Skips the per-execute
/// `format!("INSERT 0 {n}")` / `format!("{tag} {n}")` String alloc.
pub(super) fn write_command_complete_for(w: &mut MessageWriter, tag: &str, rows_affected: u64) {
    match tag {
        "INSERT" => {
            messages::write_command_complete_with_count(w, "INSERT", true, rows_affected);
        }
        "DELETE" | "UPDATE" | "MERGE" | "COPY" | "FETCH" | "MOVE" => {
            messages::write_command_complete_with_count(w, tag, false, rows_affected);
        }
        _ if rows_affected > 0 => {
            messages::write_command_complete_with_count(w, tag, false, rows_affected);
        }
        _ => messages::write_command_complete(w, tag),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aiondb_core::{DataType, Row};
    use aiondb_engine::ResultColumn;

    #[test]
    fn notice_result_writes_notice_response() {
        let mut w = MessageWriter::new();
        write_statement_result(
            &mut w,
            &StatementResult::Notice {
                message: "compatibility notice".to_owned(),
            },
            None,
            None,
        )
        .expect("serialize notice");

        let buf = w.finish_message();
        assert_eq!(buf[0], b'N');
        let payload = String::from_utf8_lossy(&buf);
        assert!(payload.contains("NOTICE"));
        assert!(payload.contains("compatibility notice"));
    }

    #[test]
    fn copy_in_notice_result_writes_notice_and_copy_complete() {
        let mut w = MessageWriter::new();
        write_copy_in_completion_result(
            &mut w,
            &StatementResult::Notice {
                message: "copy compatibility notice".to_owned(),
            },
        )
        .expect("serialize copy-in notice");

        let buf = w.finish_message();
        assert_eq!(buf[0], b'N');
        assert!(buf.windows(b"COPY 0\0".len()).any(|w| w == b"COPY 0\0"));
    }

    /// Render the CommandComplete tag a Query-shape result would emit.
    /// The wire writer streams into a `MessageWriter`; this helper
    /// extracts the cstring body so tests can keep their old equality
    /// asserts unchanged.
    fn rendered_query_complete_tag(stmt: Option<&Statement>, rows: usize) -> String {
        let mut w = MessageWriter::new();
        write_query_command_complete(&mut w, stmt, rows);
        let buf = w.finish_message();
        // Body sits between the 5-byte 'C' header and the trailing NUL.
        std::str::from_utf8(&buf[5..buf.len() - 1])
            .expect("ASCII")
            .to_owned()
    }

    fn rendered_command_complete_tag(tag: &str, rows: u64) -> String {
        let mut w = MessageWriter::new();
        write_command_complete_for(&mut w, tag, rows);
        let buf = w.finish_message();
        std::str::from_utf8(&buf[5..buf.len() - 1])
            .expect("ASCII")
            .to_owned()
    }

    #[test]
    fn command_complete_tag_preserves_zero_row_counts_for_counted_commands() {
        assert_eq!(rendered_command_complete_tag("INSERT", 0), "INSERT 0 0");
        assert_eq!(rendered_command_complete_tag("UPDATE", 0), "UPDATE 0");
        assert_eq!(rendered_command_complete_tag("DELETE", 0), "DELETE 0");
        assert_eq!(rendered_command_complete_tag("COPY", 0), "COPY 0");
    }

    #[test]
    fn query_command_complete_tag_uses_dml_returning_tag() {
        let insert = aiondb_parser::parse_sql("INSERT INTO t VALUES (1) RETURNING 1")
            .expect("parse insert returning")
            .into_iter()
            .next()
            .expect("statement");
        assert_eq!(rendered_query_complete_tag(Some(&insert), 1), "INSERT 0 1");
    }

    #[test]
    fn query_command_complete_tag_uses_show_tag() {
        let show = aiondb_parser::parse_sql("SHOW application_name")
            .expect("parse show")
            .into_iter()
            .next()
            .expect("statement");
        assert_eq!(rendered_query_complete_tag(Some(&show), 1), "SHOW");
    }

    #[test]
    fn query_command_complete_tag_uses_explain_tag() {
        let explain = aiondb_parser::parse_sql("EXPLAIN SELECT 1")
            .expect("parse explain")
            .into_iter()
            .next()
            .expect("statement");
        assert_eq!(rendered_query_complete_tag(Some(&explain), 1), "EXPLAIN");
    }

    #[test]
    fn query_command_complete_tag_uses_fetch_tag_for_query_results() {
        let fetch = aiondb_parser::parse_sql("FETCH ALL IN c")
            .expect("parse fetch")
            .into_iter()
            .next()
            .expect("statement");
        assert_eq!(rendered_query_complete_tag(Some(&fetch), 2), "FETCH 2");
    }

    #[test]
    fn write_statement_result_rejects_row_width_mismatch_without_emitting_partial_frames() {
        let mut w = MessageWriter::new();
        let result = StatementResult::Query {
            columns: vec![ResultColumn {
                name: "id".to_owned(),
                data_type: DataType::Int,
                text_type_modifier: None,
                nullable: true,
            }],
            rows: vec![Row::new(vec![])],
        };

        let error =
            write_statement_result(&mut w, &result, None, None).expect_err("row width must match");

        assert!(error
            .to_string()
            .contains("query result row 1 has 0 value(s)"));
        assert!(w.is_empty(), "no partial pgwire frames should be emitted");
    }
}
