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
}
