use crate::prepared::StatementResult;
use crate::session::SessionHandle;
use super::api::QuerySimpleSql;

pub(in crate::engine) fn first_uint_after(haystack: &str, marker: &str) -> Option<i64> {
    haystack
        .split_once(marker)?
        .1
        .split(|ch: char| !ch.is_ascii_digit())
        .find(|token| !token.is_empty())
        .and_then(|token| token.parse::<i64>().ok())
}

pub(in crate::engine) fn extract_hash_join_batch_counts_from_explain(
    lines: &[String],
) -> (i32, i32) {
    let mut original = None;
    let mut final_batches = None;

    for line in lines {
        let lower = line.to_ascii_lowercase();
        if original.is_none() {
            original = first_uint_after(&lower, "original hash batches:");
        }
        if final_batches.is_none() {
            final_batches = first_uint_after(&lower, "hash batches:");
        }
        if lower.contains("batches:") {
            if final_batches.is_none() {
                final_batches = first_uint_after(&lower, "batches:");
            }
            if original.is_none() {
                original = first_uint_after(&lower, "originally");
            }
        }
    }

    let final_i64 = final_batches.or(original).unwrap_or(1);
    let original_i64 = original.unwrap_or(final_i64);
    let original_i32 = i32::try_from(original_i64).unwrap_or(i32::MAX);
    let final_i32 = i32::try_from(final_i64).unwrap_or(i32::MAX);
    (original_i32, final_i32)
}

pub(in crate::engine) fn normalize_explain_memory_token(line: &str) -> String {
    let Some(memory_idx) = line.find("Memory: ") else {
        return line.to_owned();
    };
    let value_start = memory_idx + "Memory: ".len();
    let mut value_end = value_start;
    for ch in line[value_start..].chars() {
        if ch.is_ascii_whitespace() {
            break;
        }
        value_end += ch.len_utf8();
    }
    let mut out = String::with_capacity(line.len());
    out.push_str(&line[..value_start]);
    out.push_str("xxx");
    out.push_str(&line[value_end..]);
    out
}

#[allow(dead_code)]
fn extract_json_payload_from_explain_line(
    lines: &[String],
    prefix: &str,
) -> Option<serde_json::Value> {
    lines.iter().find_map(|line| {
        let payload = line.strip_prefix(prefix)?.trim();
        serde_json::from_str(payload).ok()
    })
}

#[allow(dead_code)]
pub(crate) fn extract_graph_summary_json_from_explain(
    lines: &[String],
) -> Option<serde_json::Value> {
    extract_json_payload_from_explain_line(lines, "Graph Summary JSON:")
}

#[allow(dead_code)]
pub(crate) fn extract_graph_detail_json_from_explain(
    lines: &[String],
) -> Option<serde_json::Value> {
    extract_json_payload_from_explain_line(lines, "Graph Detail JSON:")
}

#[allow(dead_code)]
pub(crate) fn extract_graph_summary_json_from_statement_result(
    result: &StatementResult,
) -> Option<serde_json::Value> {
    let StatementResult::Query { rows, .. } = result else {
        return None;
    };
    let lines = rows
        .iter()
        .filter_map(|row| match row.values.as_slice() {
            [aiondb_core::Value::Text(line)] => Some(line.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    extract_graph_summary_json_from_explain(&lines)
}

#[allow(dead_code)]
pub(crate) fn extract_graph_detail_json_from_statement_result(
    result: &StatementResult,
) -> Option<serde_json::Value> {
    let StatementResult::Query { rows, .. } = result else {
        return None;
    };
    let lines = rows
        .iter()
        .filter_map(|row| match row.values.as_slice() {
            [aiondb_core::Value::Text(line)] => Some(line.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    extract_graph_detail_json_from_explain(&lines)
}

pub(crate) fn explain_query_rows_to_json(rows: &[aiondb_core::Row]) -> serde_json::Value {
    let lines = rows
        .iter()
        .filter_map(|row| match row.values.as_slice() {
            [aiondb_core::Value::Text(line)] => Some(line.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    let graph_lines = lines
        .iter()
        .filter(|line| line.starts_with("Graph "))
        .cloned()
        .collect::<Vec<_>>();
    let plan_lines = lines
        .iter()
        .filter(|line| !line.starts_with("Graph "))
        .cloned()
        .collect::<Vec<_>>();
    let structural_plan_lines = plan_lines
        .iter()
        .filter(|line| {
            !line.starts_with("Execution: ")
                && !line.starts_with("Rows Returned: ")
                && !line.starts_with("Memory Used: ")
        })
        .cloned()
        .collect::<Vec<_>>();
    let plan_root_line = structural_plan_lines.first().cloned();
    let plan_root_kind = plan_root_line.as_deref().map(|line| {
        line.split_whitespace()
            .take(2)
            .collect::<Vec<_>>()
            .join(" ")
            .trim()
            .to_owned()
    });
    let primary_operator_line = structural_plan_lines
        .iter()
        .find(|line| plan_root_line.as_deref() != Some(line.as_str()))
        .cloned()
        .or_else(|| plan_root_line.clone());
    let primary_operator_kind = primary_operator_line.as_deref().map(|line| {
        line.split_whitespace()
            .take(2)
            .collect::<Vec<_>>()
            .join(" ")
            .trim()
            .to_owned()
    });
    let plan_subcategory = primary_operator_kind.as_deref().map(|kind| {
        let lower = kind.to_ascii_lowercase();
        if lower.contains("nested loop") {
            "nested_loop"
        } else if lower.contains("hash join") {
            "hash_join"
        } else if lower.contains("merge join") {
            "merge_join"
        } else if lower.contains("index scan") {
            "index_scan"
        } else if lower.contains("seq scan") {
            "seq_scan"
        } else if lower.contains("sort") {
            "sort"
        } else if lower.contains("aggregate") {
            "aggregate"
        } else if lower.contains("limit") || lower.contains("topn") {
            "limit"
        } else if lower.contains("project") {
            "project"
        } else if lower.contains("query") {
            "query_wrapper"
        } else {
            "other"
        }
    });
    let plan_category = primary_operator_kind.as_deref().map(|kind| {
        let lower = kind.to_ascii_lowercase();
        if lower.contains("join") || lower.contains("loop") {
            "join"
        } else if lower.contains("scan") {
            "scan"
        } else if lower.contains("sort") {
            "sort"
        } else if lower.contains("aggregate") || lower.contains("group") {
            "aggregate"
        } else if lower.contains("limit") || lower.contains("topn") {
            "limit"
        } else if lower.contains("project") {
            "project"
        } else {
            "other"
        }
    });
    let execution_kind = lines
        .iter()
        .find_map(|line| line.strip_prefix("Execution: ").map(str::to_owned));
    let rows_returned = lines
        .iter()
        .find_map(|line| first_uint_after(line, "Rows Returned:"))
        .and_then(|value| u64::try_from(value).ok());
    let memory_used_bytes = lines
        .iter()
        .find_map(|line| first_uint_after(line, "Memory Used:"))
        .and_then(|value| u64::try_from(value).ok());
    serde_json::json!({
        "schema_version": 1,
        "format_kind": "aiondb.explain_json",
        "query_plan_lines": lines,
        "plan_lines": plan_lines,
        "structural_plan_lines": structural_plan_lines,
        "graph_lines": graph_lines,
        "plan_overview": {
            "root_line": plan_root_line,
            "root_kind": plan_root_kind,
            "primary_operator_line": primary_operator_line,
            "primary_operator_kind": primary_operator_kind,
            "plan_category": plan_category,
            "plan_subcategory": plan_subcategory,
            "line_count": plan_lines.len(),
            "structural_line_count": structural_plan_lines.len(),
            "graph_line_count": graph_lines.len(),
        },
        "graph_summary": extract_graph_summary_json_from_explain(&lines),
        "graph_detail": extract_graph_detail_json_from_explain(&lines),
        "execution_summary": {
            "kind": execution_kind,
            "rows_returned": rows_returned,
            "memory_used_bytes": memory_used_bytes,
        },
    })
}

pub(in crate::engine) fn parse_check_estimated_rows_inner_sql(sql: &str) -> Option<String> {
    // Cheap byte-level rejection before the full case-insensitive parse.
    // `check_estimated_rows` is a debug-only built-in, but the original
    // implementation allocated a lowercase copy of the entire SQL on
    // every call to `execute_sql` just to look for this marker. Skipping
    // the allocation when the marker is absent is a few-µs win on the
    // hot OLTP path.
    super::compat::find_ascii_case_insensitive(sql, "check_estimated_rows")?;
    let trimmed = sql.trim().trim_end_matches(';').trim();
    let lower = trimmed.to_ascii_lowercase();
    let marker = "check_estimated_rows";
    let at = lower.find(marker)?;
    let prefix_norm: String = lower[..at]
        .chars()
        .filter(|ch| !ch.is_whitespace())
        .collect();
    if prefix_norm != "select*from" {
        return None;
    }
    let bytes = trimmed.as_bytes();
    let mut cursor = at + marker.len();
    while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
        cursor += 1;
    }
    if cursor >= bytes.len() || bytes[cursor] != b'(' {
        return None;
    }
    cursor += 1;
    while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
        cursor += 1;
    }
    if cursor >= bytes.len() || bytes[cursor] != b'\'' {
        return None;
    }
    cursor += 1;
    let mut out = String::new();
    while cursor < bytes.len() {
        let ch = trimmed[cursor..].chars().next()?;
        if ch == '\'' {
            let next = cursor + ch.len_utf8();
            if next < bytes.len() && bytes[next] == b'\'' {
                out.push('\'');
                cursor = next + 1;
                continue;
            }
            cursor = next;
            break;
        }
        out.push(ch);
        cursor += ch.len_utf8();
    }
    while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
        cursor += 1;
    }
    if cursor >= bytes.len() || bytes[cursor] != b')' {
        return None;
    }
    cursor += 1;
    while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
        cursor += 1;
    }
    if cursor != bytes.len() {
        return None;
    }
    Some(out)
}

impl super::Engine {
    pub fn execute_explain_graph_summary_json(
        &self,
        session: &SessionHandle,
        sql: &str,
        analyze: bool,
    ) -> aiondb_core::DbResult<serde_json::Value> {
        let explain_sql = if analyze {
            format!("EXPLAIN ANALYZE {sql}")
        } else {
            format!("EXPLAIN {sql}")
        };
        let results = self.execute_sql(session, &explain_sql)?;
        let result = results.first().ok_or_else(|| {
            aiondb_core::DbError::internal("EXPLAIN did not return a query result")
        })?;
        extract_graph_summary_json_from_statement_result(result).ok_or_else(|| {
            aiondb_core::DbError::internal(
                "EXPLAIN output did not contain Graph Summary JSON payload",
            )
        })
    }

    pub fn execute_explain_graph_detail_json(
        &self,
        session: &SessionHandle,
        sql: &str,
        analyze: bool,
    ) -> aiondb_core::DbResult<serde_json::Value> {
        let explain_sql = if analyze {
            format!("EXPLAIN ANALYZE {sql}")
        } else {
            format!("EXPLAIN {sql}")
        };
        let results = self.execute_sql(session, &explain_sql)?;
        let result = results.first().ok_or_else(|| {
            aiondb_core::DbError::internal("EXPLAIN did not return a query result")
        })?;
        extract_graph_detail_json_from_statement_result(result).ok_or_else(|| {
            aiondb_core::DbError::internal(
                "EXPLAIN output did not contain Graph Detail JSON payload",
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_check_estimated_rows_extracts_inner_sql() {
        assert_eq!(
            parse_check_estimated_rows_inner_sql("SELECT * FROM check_estimated_rows('SELECT 1')"),
            Some("SELECT 1".to_owned())
        );
        assert_eq!(
            parse_check_estimated_rows_inner_sql(
                " select * from check_estimated_rows('SELECT ''x'''); "
            ),
            Some("SELECT 'x'".to_owned())
        );
    }

    #[test]
    fn parse_check_estimated_rows_rejects_non_matching_shape() {
        assert_eq!(parse_check_estimated_rows_inner_sql("SELECT 1"), None);
        assert_eq!(
            parse_check_estimated_rows_inner_sql("SELECT * FROM check_estimated_rows('x') extra"),
            None
        );
    }

    #[test]
    fn normalize_explain_memory_redacts_variable_token() {
        assert_eq!(
            normalize_explain_memory_token("Hash  Memory: 128kB  Batches: 1"),
            "Hash  Memory: xxx  Batches: 1"
        );
        assert_eq!(normalize_explain_memory_token("Seq Scan"), "Seq Scan");
    }

    #[test]
    fn extract_hash_join_batch_counts_reads_pg_variants() {
        assert_eq!(
            extract_hash_join_batch_counts_from_explain(&[
                "Hash  Batches: 4  Memory: 128kB".to_owned(),
                "original hash batches: 2".to_owned(),
            ]),
            (2, 4)
        );
        assert_eq!(
            extract_hash_join_batch_counts_from_explain(&["Hash".to_owned()]),
            (1, 1)
        );
    }

    #[test]
    fn extract_graph_summary_json_from_explain_reads_payload() {
        let lines = vec![
            "Graph Summary Severity: severity=watch, reason=...".to_owned(),
            "Graph Summary JSON: {\"severity\":\"watch\",\"fragile_pivots\":1}".to_owned(),
        ];
        let payload = extract_graph_summary_json_from_explain(&lines).expect("payload");
        assert_eq!(payload["severity"], "watch");
        assert_eq!(payload["fragile_pivots"], 1);
    }

    #[test]
    fn extract_graph_detail_json_from_explain_reads_payload() {
        let lines = vec![
            "Graph Detail JSON: {\"summary\":{\"severity\":\"ok\"},\"clauses\":[{\"kind\":\"PipelineMatch\",\"pattern_details\":[{\"shape\":\"(a)\"}]}]}".to_owned(),
        ];
        let payload = extract_graph_detail_json_from_explain(&lines).expect("payload");
        assert_eq!(payload["summary"]["severity"], "ok");
        assert_eq!(payload["clauses"][0]["kind"], "PipelineMatch");
        assert_eq!(payload["clauses"][0]["pattern_details"][0]["shape"], "(a)");
    }

    #[test]
    fn extract_graph_summary_json_from_statement_result_reads_query_rows() {
        let result = StatementResult::Query {
            columns: vec![],
            rows: vec![aiondb_core::Row::new(vec![aiondb_core::Value::Text(
                "Graph Summary JSON: {\"severity\":\"watch\",\"fragile_pivots\":1}".to_owned(),
            )])],
        };
        let payload =
            extract_graph_summary_json_from_statement_result(&result).expect("payload");
        assert_eq!(payload["severity"], "watch");
        assert_eq!(payload["fragile_pivots"], 1);
    }

    #[test]
    fn extract_graph_detail_json_from_statement_result_reads_query_rows() {
        let result = StatementResult::Query {
            columns: vec![],
            rows: vec![aiondb_core::Row::new(vec![aiondb_core::Value::Text(
                "Graph Detail JSON: {\"summary\":{\"severity\":\"ok\"},\"clauses\":[]}".to_owned(),
            )])],
        };
        let payload =
            extract_graph_detail_json_from_statement_result(&result).expect("payload");
        assert_eq!(payload["summary"]["severity"], "ok");
        assert_eq!(payload["clauses"].as_array().map(|v| v.len()), Some(0));
    }

    #[test]
    fn explain_query_rows_to_json_embeds_graph_payloads() {
        let rows = vec![
            aiondb_core::Row::new(vec![aiondb_core::Value::Text(
                "Cypher Query".to_owned(),
            )]),
            aiondb_core::Row::new(vec![aiondb_core::Value::Text(
                "Graph Summary JSON: {\"severity\":\"watch\"}".to_owned(),
            )]),
            aiondb_core::Row::new(vec![aiondb_core::Value::Text(
                "Graph Detail JSON: {\"summary\":{\"severity\":\"watch\"},\"clauses\":[]}".to_owned(),
            )]),
            aiondb_core::Row::new(vec![aiondb_core::Value::Text(
                "Execution: Query".to_owned(),
            )]),
            aiondb_core::Row::new(vec![aiondb_core::Value::Text(
                "Rows Returned: 9".to_owned(),
            )]),
            aiondb_core::Row::new(vec![aiondb_core::Value::Text(
                "Memory Used: 5283 bytes".to_owned(),
            )]),
        ];
        let payload = explain_query_rows_to_json(&rows);
        assert_eq!(payload["schema_version"], 1);
        assert_eq!(payload["format_kind"], "aiondb.explain_json");
        assert_eq!(payload["query_plan_lines"].as_array().map(|v| v.len()), Some(6));
        assert_eq!(payload["plan_lines"].as_array().map(|v| v.len()), Some(4));
        assert_eq!(payload["structural_plan_lines"].as_array().map(|v| v.len()), Some(1));
        assert_eq!(payload["graph_lines"].as_array().map(|v| v.len()), Some(2));
        assert_eq!(payload["plan_overview"]["root_line"], "Cypher Query");
        assert_eq!(payload["plan_overview"]["root_kind"], "Cypher Query");
        assert_eq!(payload["plan_overview"]["primary_operator_line"], "Cypher Query");
        assert_eq!(payload["plan_overview"]["primary_operator_kind"], "Cypher Query");
        assert_eq!(payload["plan_overview"]["plan_category"], "other");
        assert_eq!(payload["plan_overview"]["plan_subcategory"], "query_wrapper");
        assert_eq!(payload["plan_overview"]["line_count"], 4);
        assert_eq!(payload["plan_overview"]["structural_line_count"], 1);
        assert_eq!(payload["plan_overview"]["graph_line_count"], 2);
        assert_eq!(payload["graph_summary"]["severity"], "watch");
        assert_eq!(payload["graph_detail"]["summary"]["severity"], "watch");
        assert_eq!(payload["execution_summary"]["kind"], "Query");
        assert_eq!(payload["execution_summary"]["rows_returned"], 9);
        assert_eq!(payload["execution_summary"]["memory_used_bytes"], 5283);
    }

    #[test]
    fn explain_query_rows_to_json_extracts_primary_operator_when_root_is_wrapper() {
        let rows = vec![
            aiondb_core::Row::new(vec![aiondb_core::Value::Text(
                "Cypher Query".to_owned(),
            )]),
            aiondb_core::Row::new(vec![aiondb_core::Value::Text(
                "Nested Loop  cost=0.00..10.00 rows=1".to_owned(),
            )]),
        ];
        let payload = explain_query_rows_to_json(&rows);
        assert_eq!(payload["schema_version"], 1);
        assert_eq!(payload["format_kind"], "aiondb.explain_json");
        assert_eq!(payload["plan_overview"]["root_line"], "Cypher Query");
        assert_eq!(
            payload["plan_overview"]["primary_operator_line"],
            "Nested Loop  cost=0.00..10.00 rows=1"
        );
        assert_eq!(payload["plan_overview"]["primary_operator_kind"], "Nested Loop");
        assert_eq!(payload["plan_overview"]["plan_category"], "join");
        assert_eq!(payload["plan_overview"]["plan_subcategory"], "nested_loop");
    }
}
