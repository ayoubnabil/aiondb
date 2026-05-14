//! Pure scanning primitives for PostgreSQL-style DO blocks and PL/pgSQL
//! source text: dollar-quote handling, identifier byte predicates,
//! top-level semicolon / keyword search that respects string, comment and
//! dollar-quoted regions.
//!
//! The functions here are engine-agnostic and work on `&str` only. They are
//! used by the compat DO-block parser in the engine, but any tool that
//! needs to safely tokenise PG source text can reuse them.

pub fn is_identifier_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_'
}

pub fn is_identifier_continue(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// If `sql[start..]` begins a dollar-quoted literal tag (e.g. `$tag$`),
/// return the full tag slice including both `$` delimiters. Returns `None`
/// when the character at `start` is not `$` or the tag is malformed.
pub fn dollar_quote_delimiter(sql: &str, start: usize) -> Option<&str> {
    let bytes = sql.as_bytes();
    if bytes.get(start)? != &b'$' {
        return None;
    }
    let mut cursor = start + 1;
    while cursor < bytes.len() {
        match bytes[cursor] {
            b'$' => return sql.get(start..=cursor),
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_' => cursor += 1,
            _ => return None,
        }
    }
    None
}

/// Advance `cursor` past a single lexical token at the current position
/// without entering string, comment or dollar-quoted regions. Returns
/// `Some(true)` when the cursor now sits on a regular character that the
/// caller may inspect, `Some(false)` when the cursor was moved past a
/// skippable region (string/comment/dollar-quote), and `None` when an
/// unterminated dollar-quoted block was detected.
pub fn scan_compat_do_advance(input: &str, cursor: &mut usize) -> Option<bool> {
    let bytes = input.as_bytes();
    if *cursor >= input.len() {
        return Some(false);
    }
    if bytes[*cursor] == b'\'' {
        *cursor += 1;
        while *cursor < input.len() {
            if bytes[*cursor] == b'\'' {
                *cursor += 1;
                if *cursor < input.len() && bytes[*cursor] == b'\'' {
                    *cursor += 1;
                    continue;
                }
                break;
            }
            *cursor += 1;
        }
        return Some(false);
    }
    if *cursor + 1 < input.len() && bytes[*cursor] == b'-' && bytes[*cursor + 1] == b'-' {
        *cursor += 2;
        while *cursor < input.len() && bytes[*cursor] != b'\n' {
            *cursor += 1;
        }
        return Some(false);
    }
    if *cursor + 1 < input.len() && bytes[*cursor] == b'/' && bytes[*cursor + 1] == b'*' {
        *cursor += 2;
        while *cursor + 1 < input.len() {
            if bytes[*cursor] == b'*' && bytes[*cursor + 1] == b'/' {
                *cursor += 2;
                break;
            }
            *cursor += 1;
        }
        return Some(false);
    }
    if bytes[*cursor] == b'$' {
        if let Some(delim) = dollar_quote_delimiter(input, *cursor) {
            *cursor += delim.len();
            if let Some(end_idx) = input[*cursor..].find(delim) {
                *cursor += end_idx + delim.len();
                return Some(false);
            }
            return None;
        }
    }
    Some(true)
}

pub fn scan_compat_do_top_level<F>(input: &str, mut predicate: F) -> Option<usize>
where
    F: FnMut(usize, char) -> bool,
{
    let mut cursor = 0usize;
    while cursor < input.len() {
        if !scan_compat_do_advance(input, &mut cursor)? {
            continue;
        }
        let ch = input[cursor..].chars().next()?;
        if predicate(cursor, ch) {
            return Some(cursor);
        }
        cursor += ch.len_utf8();
    }
    None
}

pub fn find_compat_do_top_level_semicolon(input: &str) -> Option<usize> {
    scan_compat_do_top_level(input, |_, ch| ch == ';')
}

pub fn find_compat_do_keyword_top_level(input: &str, keyword: &str) -> Option<usize> {
    let lower = input.to_ascii_lowercase();
    scan_compat_do_top_level(input, |idx, _| {
        let end = idx + keyword.len();
        if end > input.len() {
            return false;
        }
        // Slicing `&lower[idx..end]` panics when either bound lands inside a
        // multi-byte UTF-8 codepoint (audit plpgsql_compat F1). Guard the
        // boundaries and bail out instead.
        if !lower.is_char_boundary(idx) || !lower.is_char_boundary(end) {
            return false;
        }
        if &lower[idx..end] != keyword {
            return false;
        }
        let bytes = input.as_bytes();
        let at_start = idx == 0 || !bytes[idx - 1].is_ascii_alphanumeric();
        let at_end = end == bytes.len() || !bytes[end].is_ascii_alphanumeric();
        at_start && at_end
    })
}

pub fn find_compat_do_matching_end(after_begin: &str) -> Option<usize> {
    let lower = after_begin.to_ascii_lowercase();
    let mut begin_depth = 1usize;
    let mut cursor = 0usize;
    let bytes = after_begin.as_bytes();

    while cursor < after_begin.len() {
        let step = scan_compat_do_advance(after_begin, &mut cursor)?;
        if !step {
            continue;
        }
        if !is_identifier_start(bytes[cursor]) {
            cursor += 1;
            continue;
        }
        let start = cursor;
        cursor += 1;
        while cursor < after_begin.len() && is_identifier_continue(bytes[cursor]) {
            cursor += 1;
        }
        let token = &lower[start..cursor];
        if token == "begin" {
            begin_depth = begin_depth.saturating_add(1);
            continue;
        }
        if token != "end" {
            continue;
        }

        let mut next = cursor;
        while next < after_begin.len() && bytes[next].is_ascii_whitespace() {
            next += 1;
        }
        if next < after_begin.len() && is_identifier_start(bytes[next]) {
            let next_start = next;
            next += 1;
            while next < after_begin.len() && is_identifier_continue(bytes[next]) {
                next += 1;
            }
            let trailer = &lower[next_start..next];
            if trailer == "loop" || trailer == "if" || trailer == "case" {
                continue;
            }
        }

        begin_depth = begin_depth.saturating_sub(1);
        if begin_depth == 0 {
            return Some(start);
        }
    }
    None
}

pub fn find_matching_compat_do_end_loop(input: &str) -> Option<usize> {
    let lower = input.to_ascii_lowercase();
    let mut depth = 1usize;
    let mut cursor = 0usize;
    let bytes = input.as_bytes();

    while cursor < input.len() {
        if bytes[cursor].is_ascii_alphabetic() {
            let start = cursor;
            cursor += 1;
            while cursor < input.len()
                && (bytes[cursor].is_ascii_alphanumeric() || bytes[cursor] == b'_')
            {
                cursor += 1;
            }
            let token = &lower[start..cursor];
            if token == "loop" {
                let before = lower[..start].trim_end();
                if !before.ends_with("end") {
                    depth = depth.saturating_add(1);
                }
            } else if token == "end" {
                let tail = lower[cursor..].trim_start();
                if tail.starts_with("loop") {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        return Some(start);
                    }
                }
            }
            continue;
        }
        cursor += 1;
    }
    None
}

pub fn extract_compat_do_nested_block_statement(input: &str) -> Option<(String, &str)> {
    let lower = input.to_ascii_lowercase();
    let mut begin_depth = 0usize;
    let mut loop_depth = 0usize;
    let mut if_depth = 0usize;
    let mut cursor = 0usize;
    let bytes = input.as_bytes();

    while cursor < input.len() {
        if bytes[cursor] == b'\'' {
            cursor += 1;
            while cursor < input.len() {
                if bytes[cursor] == b'\'' {
                    cursor += 1;
                    if cursor < input.len() && bytes[cursor] == b'\'' {
                        cursor += 1;
                        continue;
                    }
                    break;
                }
                cursor += 1;
            }
            continue;
        }
        if cursor + 1 < input.len() && bytes[cursor] == b'-' && bytes[cursor + 1] == b'-' {
            cursor += 2;
            while cursor < input.len() && bytes[cursor] != b'\n' {
                cursor += 1;
            }
            continue;
        }
        if cursor + 1 < input.len() && bytes[cursor] == b'/' && bytes[cursor + 1] == b'*' {
            cursor += 2;
            while cursor + 1 < input.len() {
                if bytes[cursor] == b'*' && bytes[cursor + 1] == b'/' {
                    cursor += 2;
                    break;
                }
                cursor += 1;
            }
            continue;
        }
        if bytes[cursor] == b'$' {
            if let Some(delim) = dollar_quote_delimiter(input, cursor) {
                cursor += delim.len();
                if let Some(end_idx) = input[cursor..].find(delim) {
                    cursor += end_idx + delim.len();
                    continue;
                }
                return None;
            }
        }

        if is_identifier_start(bytes[cursor]) {
            let start = cursor;
            cursor += 1;
            while cursor < input.len() && is_identifier_continue(bytes[cursor]) {
                cursor += 1;
            }
            let token = &lower[start..cursor];
            match token {
                "begin" => begin_depth = begin_depth.saturating_add(1),
                "loop" => {
                    let before = lower[..start].trim_end();
                    if !before.ends_with("end") {
                        loop_depth = loop_depth.saturating_add(1);
                    }
                }
                "if" => {
                    let before = lower[..start].trim_end();
                    if !before.ends_with("end") {
                        if_depth = if_depth.saturating_add(1);
                    }
                }
                "end" => {
                    let tail = lower[cursor..].trim_start();
                    if tail.starts_with("if") {
                        if_depth = if_depth.saturating_sub(1);
                    } else if tail.starts_with("loop") {
                        loop_depth = loop_depth.saturating_sub(1);
                    } else {
                        begin_depth = begin_depth.saturating_sub(1);
                    }
                    if begin_depth == 0 && loop_depth == 0 && if_depth == 0 {
                        let mut rest_cursor = cursor;
                        while rest_cursor < input.len()
                            && input.as_bytes()[rest_cursor].is_ascii_whitespace()
                        {
                            rest_cursor += 1;
                        }
                        if input.as_bytes().get(rest_cursor) == Some(&b';') {
                            let block_sql = input[..rest_cursor].to_owned();
                            let rest = &input[rest_cursor + 1..];
                            return Some((block_sql, rest));
                        }
                    }
                }
                _ => {}
            }
            continue;
        }

        cursor += 1;
    }
    None
}

/// Splits a DO block body into `(declare_sql, body_sql)` pair. `declare_sql`
/// is empty when the block has no `DECLARE` section. Returns `None` if the
/// body lacks a matching `BEGIN ... END` pair or has trailing content after
/// `END`.
pub fn split_compat_do_sections(body: &str) -> Option<(&str, &str)> {
    let trimmed = body.trim();
    let begin_pos = find_compat_do_keyword_top_level(trimmed, "begin")?;
    let declare_sql = trimmed[..begin_pos].trim();
    let declare_sql = if declare_sql.is_empty() {
        ""
    } else {
        declare_sql
            .strip_prefix("DECLARE")
            .or_else(|| declare_sql.strip_prefix("declare"))?
            .trim()
    };

    let after_begin = trimmed[begin_pos + 5..].trim();
    let end_pos = find_compat_do_matching_end(after_begin)?;
    let trailing = after_begin[end_pos + 3..].trim().to_ascii_lowercase();
    if !(trailing.is_empty() || trailing == ";") {
        return None;
    }

    Some((declare_sql, after_begin[..end_pos].trim()))
}

pub fn find_compat_do_keyword_boundary(haystack: &str, keyword: &str) -> Option<usize> {
    let bytes = haystack.as_bytes();
    let keyword_bytes = keyword.as_bytes();
    let limit = haystack.len().checked_sub(keyword.len())?;

    for index in 0..=limit {
        if &bytes[index..index + keyword.len()] != keyword_bytes {
            continue;
        }
        let at_start = index == 0 || !bytes[index - 1].is_ascii_alphanumeric();
        let end = index + keyword.len();
        let at_end = end == bytes.len() || !bytes[end].is_ascii_alphanumeric();
        if at_start && at_end {
            return Some(index);
        }
    }

    None
}
