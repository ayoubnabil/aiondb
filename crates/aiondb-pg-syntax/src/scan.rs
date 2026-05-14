//! Low-level SQL scanning primitives used by the PostgreSQL compatibility
//! layer. Every function here works on `&str` and is free of engine,
//! catalog, or storage coupling.

pub fn trim_compat_statement(sql: &str) -> &str {
    sql.trim()
        .strip_suffix(';')
        .map_or(sql.trim(), str::trim_end)
}

pub fn skip_sql_whitespace(sql: &str, cursor: &mut usize) {
    while *cursor < sql.len() {
        let Some(ch) = sql[*cursor..].chars().next() else {
            break;
        };
        if ch.is_whitespace() {
            *cursor += ch.len_utf8();
            continue;
        }
        break;
    }
}

pub fn consume_word_ci(sql: &str, cursor: &mut usize, expected: &str) -> Option<()> {
    let original = *cursor;
    skip_sql_whitespace(sql, cursor);
    let start = *cursor;
    while *cursor < sql.len() {
        let ch = sql[*cursor..].chars().next()?;
        if ch.is_ascii_alphanumeric() || ch == '_' {
            *cursor += ch.len_utf8();
            continue;
        }
        break;
    }
    if start == *cursor {
        *cursor = original;
        return None;
    }
    let token = &sql[start..*cursor];
    if token.eq_ignore_ascii_case(expected) {
        Some(())
    } else {
        *cursor = original;
        None
    }
}

pub fn consume_word_phrase_ci(sql: &str, cursor: &mut usize, phrase: &str) -> Option<()> {
    let original = *cursor;
    let mut phrase_cursor = 0usize;
    let mut consumed = false;
    while phrase_cursor < phrase.len() {
        let Some(word) = parse_identifier_part(phrase, &mut phrase_cursor) else {
            let Some(ch) = phrase[phrase_cursor..].chars().next() else {
                break;
            };
            if !ch.is_whitespace() {
                *cursor = original;
                return None;
            }
            phrase_cursor += ch.len_utf8();
            continue;
        };
        if consume_word_ci(sql, cursor, &word).is_none() {
            *cursor = original;
            return None;
        }
        consumed = true;
    }
    consumed.then_some(()).or_else(|| {
        *cursor = original;
        None
    })
}

pub fn parse_identifier_part(sql: &str, cursor: &mut usize) -> Option<String> {
    skip_sql_whitespace(sql, cursor);
    if *cursor >= sql.len() {
        return None;
    }

    if sql[*cursor..].starts_with('"') {
        *cursor += 1;
        let mut ident = String::new();
        while *cursor < sql.len() {
            if sql[*cursor..].starts_with('"') {
                *cursor += 1;
                if *cursor < sql.len() && sql[*cursor..].starts_with('"') {
                    ident.push('"');
                    *cursor += 1;
                    continue;
                }
                return (!ident.is_empty()).then_some(ident);
            }
            let ch = sql[*cursor..].chars().next()?;
            ident.push(ch);
            *cursor += ch.len_utf8();
        }
        return None;
    }

    let start = *cursor;
    while *cursor < sql.len() {
        let ch = sql[*cursor..].chars().next()?;
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '$' {
            *cursor += ch.len_utf8();
            continue;
        }
        break;
    }
    if start == *cursor {
        None
    } else {
        sql.get(start..*cursor).map(str::to_owned)
    }
}

pub fn parse_compat_identifier(sql: &str, cursor: &mut usize) -> Option<String> {
    skip_sql_whitespace(sql, cursor);
    if *cursor >= sql.len() {
        return None;
    }

    if sql[*cursor..].starts_with('"') {
        return parse_identifier_part(sql, cursor);
    }

    let start = *cursor;
    while *cursor < sql.len() {
        let ch = sql[*cursor..].chars().next()?;
        if ch.is_ascii_alphanumeric() || ch == '_' {
            *cursor += ch.len_utf8();
        } else {
            break;
        }
    }
    if start == *cursor {
        None
    } else {
        sql.get(start..*cursor)
            .map(|ident| ident.to_ascii_lowercase())
    }
}

pub fn parse_compat_uint(sql: &str, cursor: &mut usize) -> Option<usize> {
    skip_sql_whitespace(sql, cursor);
    let start = *cursor;
    while *cursor < sql.len() {
        let ch = sql[*cursor..].chars().next()?;
        if ch.is_ascii_digit() {
            *cursor += ch.len_utf8();
        } else {
            break;
        }
    }
    if start == *cursor {
        None
    } else {
        sql.get(start..*cursor)?.parse().ok()
    }
}

/// Parses a PG-style boolean literal (`true`/`false`/`on`/`off`/`1`/`0`)
/// after an optional `=` separator. Returns `None` if unparseable.
pub fn parse_compat_bool(sql: &str, cursor: &mut usize) -> Option<bool> {
    skip_sql_whitespace(sql, cursor);
    if sql.as_bytes().get(*cursor).copied() == Some(b'=') {
        *cursor += 1;
        skip_sql_whitespace(sql, cursor);
    }
    if consume_word_ci(sql, cursor, "true").is_some()
        || consume_word_ci(sql, cursor, "on").is_some()
    {
        return Some(true);
    }
    if consume_word_ci(sql, cursor, "false").is_some()
        || consume_word_ci(sql, cursor, "off").is_some()
    {
        return Some(false);
    }
    let start = *cursor;
    if let Some(value) = parse_compat_uint(sql, cursor) {
        // PG `parse_bool` only accepts 0 / 1; reject everything else so
        if value == 0 {
            return Some(false);
        }
        if value == 1 {
            return Some(true);
        }
    }
    *cursor = start;
    None
}

pub fn strip_compat_word_ci<'a>(input: &'a str, expected: &str) -> Option<&'a str> {
    let mut cursor = 0usize;
    consume_word_ci(input, &mut cursor, expected)?;
    Some(input[cursor..].trim_start())
}

pub fn parse_leading_compat_uint(input: &str) -> Option<(usize, &str)> {
    let mut cursor = 0usize;
    let value = parse_compat_uint(input, &mut cursor)?;
    Some((value, input[cursor..].trim_start()))
}

pub fn parse_leading_compat_int(input: &str) -> Option<(i64, &str)> {
    let trimmed = input.trim_start();
    if trimmed.is_empty() {
        return None;
    }

    let mut offset = 0usize;
    let bytes = trimmed.as_bytes();
    if bytes.first().is_some_and(|b| *b == b'+' || *b == b'-') {
        offset = 1;
    }
    let digit_start = offset;
    while offset < bytes.len() && bytes[offset].is_ascii_digit() {
        offset += 1;
    }
    if offset == digit_start {
        return None;
    }

    let parsed = trimmed[..offset].parse::<i64>().ok()?;
    Some((parsed, &trimmed[offset..]))
}

/// Extract parenthesized content, handling nested parentheses.
/// Advances cursor past the closing paren. Returns the content inside (without
/// the outer parens).
pub fn extract_parenthesized(sql: &str, cursor: &mut usize) -> Option<String> {
    skip_sql_whitespace(sql, cursor);
    if !sql.get(*cursor..)?.starts_with('(') {
        return None;
    }
    *cursor += 1;
    let start = *cursor;
    let mut depth = 1u32;
    while *cursor < sql.len() && depth > 0 {
        let ch = sql[*cursor..].chars().next()?;
        match ch {
            '(' => {
                depth += 1;
                *cursor += 1;
            }
            ')' => {
                depth -= 1;
                if depth == 0 {
                    let content = sql[start..*cursor].to_owned();
                    *cursor += 1;
                    return Some(content);
                }
                *cursor += 1;
            }
            '\'' => {
                *cursor += 1;
                while *cursor < sql.len() {
                    if sql[*cursor..].starts_with('\'') {
                        *cursor += 1;
                        if !sql.get(*cursor..)?.starts_with('\'') {
                            break;
                        }
                        *cursor += 1;
                    } else {
                        *cursor += sql[*cursor..].chars().next().map_or(1, |c| c.len_utf8());
                    }
                }
            }
            _ => {
                *cursor += ch.len_utf8();
            }
        }
    }
    None
}

pub fn parse_compat_int(sql: &str, cursor: &mut usize) -> Option<i32> {
    let entry = *cursor;
    skip_sql_whitespace(sql, cursor);
    if *cursor >= sql.len() {
        *cursor = entry;
        return None;
    }

    let mut sign = 1i64;
    if let Some(ch) = sql[*cursor..].chars().next() {
        if ch == '-' {
            sign = -1;
            *cursor += 1;
        } else if ch == '+' {
            *cursor += 1;
        }
    }

    let start = *cursor;
    while *cursor < sql.len() {
        let Some(ch) = sql[*cursor..].chars().next() else {
            break;
        };
        if ch.is_ascii_digit() {
            *cursor += ch.len_utf8();
        } else {
            break;
        }
    }
    if start == *cursor {
        // No digits after the optional sign: rewind so callers using `?`
        // see a pristine cursor and can fall through to alternative parses.
        *cursor = entry;
        return None;
    }

    let Some(magnitude) = sql.get(start..*cursor).and_then(|s| s.parse::<i64>().ok()) else {
        *cursor = entry;
        return None;
    };
    let Some(signed) = magnitude.checked_mul(sign) else {
        *cursor = entry;
        return None;
    };
    match i32::try_from(signed) {
        Ok(value) => Some(value),
        Err(_) => {
            *cursor = entry;
            None
        }
    }
}

pub fn replace_ascii_case_insensitive_all(
    haystack: &str,
    needle: &str,
    replacement: &str,
) -> (String, bool) {
    if needle.is_empty() {
        return (haystack.to_owned(), false);
    }

    let mut rest = haystack;
    let mut output = String::with_capacity(haystack.len());
    let mut changed = false;

    while let Some(pos) = find_ascii_case_insensitive(rest, needle) {
        changed = true;
        output.push_str(&rest[..pos]);
        output.push_str(replacement);
        rest = &rest[pos + needle.len()..];
    }

    output.push_str(rest);
    (output, changed)
}

/// Splits comma-separated items at the top level of `sql`, honouring single
/// quotes and nested parentheses. Trims whitespace around each item.
pub fn split_top_level_csv(sql: &str) -> Option<Vec<String>> {
    let mut items = Vec::new();
    let mut cursor = 0usize;
    let mut current_start = 0usize;
    let mut depth = 0u32;
    let mut in_single_quote = false;
    let bytes = sql.as_bytes();

    while cursor < bytes.len() {
        let ch = bytes[cursor];
        if in_single_quote {
            if ch == b'\'' {
                if cursor + 1 < bytes.len() && bytes[cursor + 1] == b'\'' {
                    cursor += 2;
                    continue;
                }
                in_single_quote = false;
            }
            cursor += 1;
            continue;
        }

        match ch {
            b'\'' => {
                in_single_quote = true;
                cursor += 1;
            }
            b'(' => {
                depth += 1;
                cursor += 1;
            }
            b')' => {
                depth = depth.checked_sub(1)?;
                cursor += 1;
            }
            b',' if depth == 0 => {
                items.push(sql[current_start..cursor].trim().to_owned());
                cursor += 1;
                current_start = cursor;
            }
            _ => cursor += 1,
        }
    }

    let tail = sql[current_start..].trim();
    if !tail.is_empty() {
        items.push(tail.to_owned());
    }
    Some(items)
}

/// Parse a single-quoted string literal `'...'` honouring doubled-quote
/// escapes (`''` → `'`). Returns `None` if the cursor isn't positioned on
/// a quoted token.
pub fn parse_string_literal(sql: &str, cursor: &mut usize) -> Option<String> {
    skip_sql_whitespace(sql, cursor);
    let bytes = sql.as_bytes();
    if *cursor >= bytes.len() || bytes[*cursor] != b'\'' {
        return None;
    }
    *cursor += 1;
    let mut out = String::new();
    while *cursor < bytes.len() {
        if bytes[*cursor] == b'\'' {
            if *cursor + 1 < bytes.len() && bytes[*cursor + 1] == b'\'' {
                out.push('\'');
                *cursor += 2;
                continue;
            }
            *cursor += 1;
            return Some(out);
        }
        // Push the next char honouring UTF-8 boundaries instead of treating
        // the input as Latin-1 - `'café'` would otherwise corrupt the
        // multi-byte `é`. We trust the lexer / caller's `&str` input to be
        // valid UTF-8 so the slice from `*cursor` always starts on a char
        // boundary.
        let ch = sql[*cursor..].chars().next()?;
        out.push(ch);
        *cursor += ch.len_utf8();
    }
    None
}

pub fn consume_punctuation(sql: &str, cursor: &mut usize, ch: char) -> bool {
    skip_sql_whitespace(sql, cursor);
    if sql[*cursor..].starts_with(ch) {
        *cursor += ch.len_utf8();
        true
    } else {
        false
    }
}

/// Consume an optional `IF EXISTS` phrase. Returns `true` iff both keywords
/// matched and advanced the cursor; the cursor is rewound to its entry
/// position otherwise.
pub fn consume_if_exists(sql: &str, cursor: &mut usize) -> bool {
    let probe = *cursor;
    if consume_word_ci(sql, cursor, "if").is_some()
        && consume_word_ci(sql, cursor, "exists").is_some()
    {
        true
    } else {
        *cursor = probe;
        false
    }
}

/// Consume an optional `IF NOT EXISTS` phrase. Returns `true` iff all three
/// keywords matched. Rewinds to entry on mismatch.
pub fn consume_if_not_exists(sql: &str, cursor: &mut usize) -> bool {
    let probe = *cursor;
    if consume_word_ci(sql, cursor, "if").is_some()
        && consume_word_ci(sql, cursor, "not").is_some()
        && consume_word_ci(sql, cursor, "exists").is_some()
    {
        true
    } else {
        *cursor = probe;
        false
    }
}

/// Parse a `( key [=] value [, ...] )` option list into `(prefix, name, value)`
/// triples where `prefix` is one of `""`, `"ADD"`, `"SET"`, `"DROP"`. Stops
/// at the closing `)`. Accepts identifiers and single-quoted literals as
/// values.
pub fn parse_compat_option_list(sql: &str, cursor: &mut usize) -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    skip_sql_whitespace(sql, cursor);
    if !consume_punctuation(sql, cursor, '(') {
        return out;
    }
    loop {
        skip_sql_whitespace(sql, cursor);
        if consume_punctuation(sql, cursor, ')') {
            break;
        }
        let mut prefix = String::new();
        let probe = *cursor;
        if let Some(word) = parse_identifier_part(sql, cursor) {
            let upper = word.to_ascii_uppercase();
            if matches!(upper.as_str(), "ADD" | "SET" | "DROP") {
                prefix = upper;
            } else {
                *cursor = probe;
            }
        }
        let Some(name) = parse_identifier_part(sql, cursor) else {
            break;
        };
        skip_sql_whitespace(sql, cursor);
        let _ = consume_punctuation(sql, cursor, '=');
        let value = parse_string_literal(sql, cursor)
            .or_else(|| parse_identifier_part(sql, cursor))
            .unwrap_or_default();
        out.push((prefix, name, value));
        skip_sql_whitespace(sql, cursor);
        if !consume_punctuation(sql, cursor, ',') {
            let _ = consume_punctuation(sql, cursor, ')');
            break;
        }
    }
    out
}

pub fn parse_compat_declare_query_sql(statement_sql: &str) -> Option<String> {
    let sql = trim_compat_statement(statement_sql);
    let mut cursor = 0usize;
    consume_word_ci(sql, &mut cursor, "declare")?;
    parse_compat_identifier(sql, &mut cursor)?;

    let mut saw_cursor = false;
    while cursor < sql.len() {
        if let Some(token) = parse_compat_identifier(sql, &mut cursor) {
            if !saw_cursor {
                if token == "cursor" {
                    saw_cursor = true;
                }
                continue;
            }
            if token == "for" {
                let query_sql = sql[cursor..].trim();
                return (!query_sql.is_empty()).then(|| query_sql.to_owned());
            }
            continue;
        }
        let ch = sql[cursor..].chars().next()?;
        cursor += ch.len_utf8();
    }

    None
}

pub fn contains_compat_word_pair_ci(sql: &str, first: &str, second: &str) -> bool {
    let mut cursor = 0usize;
    let mut previous: Option<String> = None;
    while cursor < sql.len() {
        if let Some(word) = parse_identifier_part(sql, &mut cursor) {
            if previous
                .as_deref()
                .is_some_and(|prev| prev.eq_ignore_ascii_case(first))
                && word.eq_ignore_ascii_case(second)
            {
                return true;
            }
            previous = Some(word);
            continue;
        }
        let Some(ch) = sql[cursor..].chars().next() else {
            break;
        };
        cursor += ch.len_utf8();
    }
    false
}

pub fn parse_compat_conninfo_host_port(conninfo: &str) -> (Option<String>, Option<i32>) {
    let mut host = None;
    let mut port = None;
    let mut cursor = 0usize;

    while cursor < conninfo.len() {
        skip_sql_whitespace(conninfo, &mut cursor);
        let Some(key) = parse_conninfo_key(conninfo, &mut cursor) else {
            skip_conninfo_token(conninfo, &mut cursor);
            continue;
        };
        skip_sql_whitespace(conninfo, &mut cursor);
        if conninfo.as_bytes().get(cursor).copied() != Some(b'=') {
            skip_conninfo_token(conninfo, &mut cursor);
            continue;
        }
        cursor += 1;
        skip_sql_whitespace(conninfo, &mut cursor);
        let Some(value) = parse_conninfo_value(conninfo, &mut cursor) else {
            continue;
        };
        match key.to_ascii_lowercase().as_str() {
            "host" if !value.is_empty() => host = Some(value),
            "port" => {
                if let Ok(parsed) = value.parse::<i32>() {
                    port = Some(parsed);
                }
            }
            _ => {}
        }
    }

    (host, port)
}

fn parse_conninfo_key(conninfo: &str, cursor: &mut usize) -> Option<String> {
    let start = *cursor;
    while *cursor < conninfo.len() {
        let ch = conninfo[*cursor..].chars().next()?;
        if ch.is_ascii_alphanumeric() || ch == '_' {
            *cursor += ch.len_utf8();
            continue;
        }
        break;
    }
    (*cursor > start).then(|| conninfo[start..*cursor].to_owned())
}

fn parse_conninfo_value(conninfo: &str, cursor: &mut usize) -> Option<String> {
    if *cursor >= conninfo.len() {
        return Some(String::new());
    }
    if conninfo.as_bytes().get(*cursor).copied() == Some(b'\'') {
        *cursor += 1;
        let mut value = String::new();
        while *cursor < conninfo.len() {
            let ch = conninfo[*cursor..].chars().next()?;
            *cursor += ch.len_utf8();
            if ch == '\\' {
                let escaped = conninfo[*cursor..].chars().next()?;
                value.push(escaped);
                *cursor += escaped.len_utf8();
                continue;
            }
            if ch == '\'' {
                return Some(value);
            }
            value.push(ch);
        }
        return None;
    }

    let start = *cursor;
    while *cursor < conninfo.len() {
        let ch = conninfo[*cursor..].chars().next()?;
        if ch.is_whitespace() {
            break;
        }
        *cursor += ch.len_utf8();
    }
    Some(conninfo[start..*cursor].to_owned())
}

fn skip_conninfo_token(conninfo: &str, cursor: &mut usize) {
    while *cursor < conninfo.len() {
        let Some(ch) = conninfo[*cursor..].chars().next() else {
            break;
        };
        if ch.is_whitespace() {
            break;
        }
        *cursor += ch.len_utf8();
    }
}

/// Insert or overwrite a single `(key, value)` entry in an option list.
pub fn upsert_option(options: &mut Vec<(String, String)>, name: &str, value: &str) {
    let normalized = name.to_ascii_lowercase();
    if let Some(entry) = options.iter_mut().find(|(k, _)| k == &normalized) {
        entry.1 = value.to_owned();
    } else {
        options.push((normalized, value.to_owned()));
    }
}

/// Apply a set of `(prefix, name, value)` option triples onto an existing
/// option vector, mirroring PG semantics:
///   * `ADD name value` - append if absent, error-free overwrite if present.
///   * `SET name value` or bare `name value` - upsert.
///   * `DROP name` - remove by name.
pub fn apply_option_list(
    options: &mut Vec<(String, String)>,
    pairs: Vec<(String, String, String)>,
) {
    for (prefix, name, value) in pairs {
        let normalized = name.to_ascii_lowercase();
        match prefix.as_str() {
            "DROP" => {
                options.retain(|(existing, _)| existing != &normalized);
            }
            _ => {
                if let Some(entry) = options.iter_mut().find(|(k, _)| k == &normalized) {
                    entry.1 = value;
                } else {
                    options.push((normalized, value));
                }
            }
        }
    }
}

pub fn find_ascii_case_insensitive(haystack: &str, needle: &str) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }

    let haystack = haystack.as_bytes();
    let needle = needle.as_bytes();
    if needle.len() > haystack.len() {
        return None;
    }

    haystack.windows(needle.len()).position(|window| {
        window
            .iter()
            .zip(needle.iter())
            .all(|(lhs, rhs)| lhs.eq_ignore_ascii_case(rhs))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_compat_declare_query_extracts_select_tail() {
        assert_eq!(
            parse_compat_declare_query_sql("DECLARE c CURSOR FOR SELECT 1"),
            Some("SELECT 1".to_owned())
        );
        assert_eq!(
            parse_compat_declare_query_sql(
                "DECLARE \"c\" NO SCROLL CURSOR WITH HOLD FOR VALUES (1);"
            ),
            Some("VALUES (1)".to_owned())
        );
    }

    #[test]
    fn parse_compat_declare_query_rejects_malformed_declare() {
        assert_eq!(parse_compat_declare_query_sql("DECLARE c CURSOR"), None);
        assert_eq!(
            parse_compat_declare_query_sql("DECLARE c FOR SELECT 1"),
            None
        );
        assert_eq!(parse_compat_declare_query_sql("SELECT 1"), None);
    }

    #[test]
    fn contains_compat_word_pair_matches_words_only() {
        assert!(contains_compat_word_pair_ci(
            "COMMIT AND CHAIN",
            "and",
            "chain"
        ));
        assert!(contains_compat_word_pair_ci(
            "ROLLBACK AND\nCHAIN",
            "AND",
            "CHAIN"
        ));
        assert!(!contains_compat_word_pair_ci(
            "COMMIT candy chain",
            "and",
            "chain"
        ));
        assert!(!contains_compat_word_pair_ci(
            "COMMIT AND x CHAIN",
            "and",
            "chain"
        ));
    }

    #[test]
    fn consume_word_phrase_consumes_multiword_phrase_atomically() {
        let sql = "DROP FOREIGN DATA WRAPPER fdw";
        let mut cursor = 0usize;
        consume_word_ci(sql, &mut cursor, "drop").expect("drop");
        consume_word_phrase_ci(sql, &mut cursor, "FOREIGN DATA WRAPPER").expect("phrase");
        assert_eq!(sql[cursor..].trim_start(), "fdw");

        let mut cursor = "DROP ".len();
        assert!(consume_word_phrase_ci(sql, &mut cursor, "FOREIGN TABLE").is_none());
        assert_eq!(cursor, "DROP ".len());
    }

    #[test]
    fn parse_compat_conninfo_extracts_host_and_port() {
        let (host, port) =
            parse_compat_conninfo_host_port("host=primary.example port=5432 dbname=aion");
        assert_eq!(host.as_deref(), Some("primary.example"));
        assert_eq!(port, Some(5432));
    }

    #[test]
    fn parse_compat_conninfo_handles_quoted_values() {
        let (host, port) =
            parse_compat_conninfo_host_port(r"host='primary node' port='6543' user=replica");
        assert_eq!(host.as_deref(), Some("primary node"));
        assert_eq!(port, Some(6543));

        let (host, _) = parse_compat_conninfo_host_port(r"host='primary\'node'");
        assert_eq!(host.as_deref(), Some("primary'node"));
    }

    #[test]
    fn parse_compat_conninfo_ignores_invalid_port_and_missing_host() {
        let (host, port) = parse_compat_conninfo_host_port("port=not-a-number user=replica");
        assert_eq!(host, None);
        assert_eq!(port, None);
    }

    #[test]
    fn parse_compat_int_rewinds_cursor_on_miss() {
        // Sign followed by a non-digit must not consume the sign character;
        // callers using `?` rely on a pristine cursor to attempt alternative
        // parses (matches `consume_word_phrase_ci` rewind behaviour).
        let sql = "-abc";
        let mut cursor = 0usize;
        assert_eq!(parse_compat_int(sql, &mut cursor), None);
        assert_eq!(cursor, 0, "cursor must rewind when sign has no digits");

        let sql = "  +xyz";
        let mut cursor = 0usize;
        assert_eq!(parse_compat_int(sql, &mut cursor), None);
        assert_eq!(cursor, 0, "cursor must rewind across whitespace + sign");

        // i32 overflow also rewinds so a bigint literal can be re-tried by
        // a wider parser at the same offset.
        let sql = "9999999999";
        let mut cursor = 0usize;
        assert_eq!(parse_compat_int(sql, &mut cursor), None);
        assert_eq!(cursor, 0, "cursor must rewind on i32 overflow");

        // Happy path still advances correctly.
        let sql = "  -42 trailing";
        let mut cursor = 0usize;
        assert_eq!(parse_compat_int(sql, &mut cursor), Some(-42));
        assert_eq!(&sql[cursor..], " trailing");
    }
}
