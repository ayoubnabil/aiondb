pub(in crate::engine) fn parse_copy_identifier_list(raw: &str) -> HashSet<String> {
    split_top_level_csv_items(raw)
        .unwrap_or_default()
        .into_iter()
        .map(|item| item.trim().trim_matches('"').to_ascii_lowercase())
        .filter(|item| !item.is_empty())
        .collect()
}

pub(in crate::engine) fn copy_option_key(item: &str) -> String {
    let lower = item.trim().to_ascii_lowercase();
    if lower.starts_with("format ") || lower == "csv" || lower == "binary" {
        return "format".to_owned();
    }
    if lower.starts_with("force_quote") {
        return "force_quote".to_owned();
    }
    if lower.starts_with("force_not_null") {
        return "force_not_null".to_owned();
    }
    if lower.starts_with("force_null") {
        return "force_null".to_owned();
    }
    if lower.starts_with("default ") {
        return "default".to_owned();
    }
    let mut cursor = 0usize;
    parse_identifier_part(&lower, &mut cursor).unwrap_or_default()
}

pub(in crate::engine) fn parse_copy_sql_string_literal(sql: &str, cursor: &mut usize) -> Option<(String, String)> {
    skip_sql_whitespace(sql, cursor);
    if *cursor >= sql.len() {
        return None;
    }

    let bytes = sql.as_bytes();
    let start = *cursor;
    let has_escape = matches!(bytes.get(*cursor), Some(b'e' | b'E'))
        && matches!(bytes.get(*cursor + 1), Some(b'\''));
    if has_escape {
        *cursor += 1;
    }
    if !matches!(bytes.get(*cursor), Some(b'\'')) {
        *cursor = start;
        return None;
    }
    *cursor += 1;
    while *cursor < sql.len() {
        match bytes[*cursor] {
            b'\'' => {
                if *cursor + 1 < sql.len() && bytes[*cursor + 1] == b'\'' {
                    *cursor += 2;
                } else {
                    *cursor += 1;
                    let raw = sql[start..*cursor].to_owned();
                    let decoded = decode_sql_single_quoted_literal(&raw)?;
                    return Some((raw, decoded));
                }
            }
            _ => *cursor += 1,
        }
    }
    *cursor = start;
    None
}

pub(in crate::engine) fn parse_copy_legacy_with_item_list(tail: &str) -> Option<Vec<String>> {
    let mut items = Vec::new();
    let mut rest = tail.trim();
    while !rest.is_empty() {
        rest = rest.trim_start();
        if rest.is_empty() || rest.starts_with(';') {
            break;
        }

        let lower = rest.to_ascii_lowercase();
        if lower.starts_with("delimiter as ") {
            let mut cursor = "delimiter as".len();
            let (raw, _) = parse_copy_sql_string_literal(rest, &mut cursor)?;
            items.push(format!("delimiter {raw}"));
            rest = &rest[cursor..];
            continue;
        }
        if lower.starts_with("null as ") {
            let mut cursor = "null as".len();
            let (raw, _) = parse_copy_sql_string_literal(rest, &mut cursor)?;
            items.push(format!("null {raw}"));
            rest = &rest[cursor..];
            continue;
        }
        if lower.starts_with("encoding ") {
            let mut cursor = "encoding".len();
            let (raw, _) = parse_copy_sql_string_literal(rest, &mut cursor)?;
            items.push(format!("encoding {raw}"));
            rest = &rest[cursor..];
            continue;
        }
        return None;
    }
    Some(items)
}

pub(in crate::engine) fn copy_endpoint_info(sql: &str, direction: aiondb_parser::CopyDirection) -> Option<(usize, bool)> {
    let sql = trim_compat_statement(sql);
    let lower = sql.to_ascii_lowercase();
    let supported = match direction {
        aiondb_parser::CopyDirection::From => lower
            .find(" from stdin")
            .map(|pos| (pos + " from stdin".len(), true)),
        aiondb_parser::CopyDirection::To => lower
            .find(" to stdout")
            .map(|pos| (pos + " to stdout".len(), true)),
    };
    if supported.is_some() {
        return supported;
    }
    match direction {
        aiondb_parser::CopyDirection::From => lower
            .find(" from stdout")
            .map(|pos| (pos + " from stdout".len(), false)),
        aiondb_parser::CopyDirection::To => lower
            .find(" to stdin")
            .map(|pos| (pos + " to stdin".len(), false)),
    }
}

pub(in crate::engine) fn parse_copy_sql_options(
    sql: &str,
    direction: aiondb_parser::CopyDirection,
) -> DbResult<CopyCompatOptions> {
    let mut options = CopyCompatOptions::for_direction(direction);
    let sql = trim_compat_statement(sql);
    let (mut cursor, _) = copy_endpoint_info(sql, direction)
        .ok_or_else(|| DbError::parse_error(SqlState::SyntaxError, "invalid COPY statement"))?;

    skip_sql_whitespace(sql, &mut cursor);

    let parse_option_item = |item: &str, options: &mut CopyCompatOptions| -> DbResult<()> {
        let item_trimmed = item.trim();
        let item_lower = item_trimmed.to_ascii_lowercase();
        if item_lower.is_empty() {
            return Ok(());
        }
        if item_lower == "csv" || item_lower == "format csv" {
            if options.format != CopyCompatFormat::Text {
                return Err(DbError::parse_error(
                    SqlState::SyntaxError,
                    "conflicting or redundant options",
                ));
            }
            options.format = CopyCompatFormat::Csv;
            options.delimiter = ',';
            options.null_string.clear();
            return Ok(());
        }
        if item_lower == "format binary" || item_lower == "binary" {
            options.format = CopyCompatFormat::Binary;
            return Ok(());
        }
        if item_lower == "format text" {
            options.format = CopyCompatFormat::Text;
            return Ok(());
        }
        if item_lower == "header" || item_lower == "header true" || item_lower == "header on" {
            if options.header {
                return Err(DbError::parse_error(
                    SqlState::SyntaxError,
                    "conflicting or redundant options",
                ));
            }
            options.header = true;
            return Ok(());
        }
        if item_lower == "header off" {
            return Ok(());
        }
        if item_lower == "header match" {
            options.header = true;
            options.header_match = true;
            return Ok(());
        }
        if let Some(rest) = item_lower.strip_prefix("delimiter ") {
            if options.delimiter != '\t' && options.delimiter != ',' {
                return Err(DbError::parse_error(
                    SqlState::SyntaxError,
                    "conflicting or redundant options",
                ));
            }
            let literal = item_trimmed[item_trimmed.len() - rest.len()..].trim();
            let value = decode_sql_single_quoted_literal(literal).ok_or_else(|| {
                DbError::parse_error(
                    SqlState::SyntaxError,
                    "expected string literal after COPY DELIMITER option",
                )
            })?;
            options.delimiter = value.chars().next().unwrap_or('\t');
            options.delimiter_explicit = true;
            return Ok(());
        }
        if let Some(rest) = item_lower.strip_prefix("delimiter as ") {
            let literal = item_trimmed[item_trimmed.len() - rest.len()..].trim();
            let value = decode_sql_single_quoted_literal(literal).ok_or_else(|| {
                DbError::parse_error(
                    SqlState::SyntaxError,
                    "expected string literal after COPY DELIMITER option",
                )
            })?;
            options.delimiter = value.chars().next().unwrap_or('\t');
            options.delimiter_explicit = true;
            return Ok(());
        }
        if let Some(rest) = item_lower.strip_prefix("null ") {
            let literal = item_trimmed[item_trimmed.len() - rest.len()..].trim();
            options.null_string = decode_sql_single_quoted_literal(literal).ok_or_else(|| {
                DbError::parse_error(
                    SqlState::SyntaxError,
                    "expected string literal after COPY NULL option",
                )
            })?;
            options.null_explicit = true;
            return Ok(());
        }
        if let Some(rest) = item_lower.strip_prefix("null as ") {
            let literal = item_trimmed[item_trimmed.len() - rest.len()..].trim();
            options.null_string = decode_sql_single_quoted_literal(literal).ok_or_else(|| {
                DbError::parse_error(
                    SqlState::SyntaxError,
                    "expected string literal after COPY NULL option",
                )
            })?;
            options.null_explicit = true;
            return Ok(());
        }
        if let Some(rest) = item_lower.strip_prefix("quote ") {
            let literal = item_trimmed[item_trimmed.len() - rest.len()..].trim();
            let value = decode_sql_single_quoted_literal(literal).ok_or_else(|| {
                DbError::parse_error(
                    SqlState::SyntaxError,
                    "expected string literal after COPY QUOTE option",
                )
            })?;
            options.quote = value.chars().next().unwrap_or('"');
            return Ok(());
        }
        if let Some(rest) = item_lower.strip_prefix("escape ") {
            let literal = item_trimmed[item_trimmed.len() - rest.len()..].trim();
            let value = decode_sql_single_quoted_literal(literal).ok_or_else(|| {
                DbError::parse_error(
                    SqlState::SyntaxError,
                    "expected string literal after COPY ESCAPE option",
                )
            })?;
            options.escape = value.chars().next().unwrap_or(options.quote);
            return Ok(());
        }
        if let Some(rest) = item_lower.strip_prefix("default ") {
            let literal = item_trimmed[item_trimmed.len() - rest.len()..].trim();
            let value = decode_sql_single_quoted_literal(literal).ok_or_else(|| {
                DbError::parse_error(
                    SqlState::SyntaxError,
                    "expected string literal after COPY DEFAULT option",
                )
            })?;
            options.default_string = Some(value);
            return Ok(());
        }
        if let Some(rest) = item_lower.strip_prefix("force_quote ") {
            if rest.trim() == "*" {
                options.force_quote_all = true;
                return Ok(());
            }
            let mut inner_cursor = "force_quote".len();
            if let Some(inner) = extract_parenthesized(item_trimmed, &mut inner_cursor) {
                options.force_quote_columns = parse_copy_identifier_list(&inner);
                return Ok(());
            }
        }
        if let Some(rest) = item_lower.strip_prefix("force_quote(") {
            let inner = rest.strip_suffix(')').unwrap_or(rest);
            options.force_quote_columns = parse_copy_identifier_list(inner);
            return Ok(());
        }
        if let Some(rest) = item_lower.strip_prefix("force_not_null ") {
            let mut inner_cursor = "force_not_null".len();
            if let Some(inner) = extract_parenthesized(item_trimmed, &mut inner_cursor) {
                options.force_not_null_columns = parse_copy_identifier_list(&inner);
                return Ok(());
            }
            let _ = rest;
        }
        if let Some(rest) = item_lower.strip_prefix("force_not_null(") {
            let inner = rest.strip_suffix(')').unwrap_or(rest);
            options.force_not_null_columns = parse_copy_identifier_list(inner);
            return Ok(());
        }
        if let Some(rest) = item_lower.strip_prefix("force_null ") {
            let mut inner_cursor = "force_null".len();
            if let Some(inner) = extract_parenthesized(item_trimmed, &mut inner_cursor) {
                options.force_null_columns = parse_copy_identifier_list(&inner);
                return Ok(());
            }
            let _ = rest;
        }
        if let Some(rest) = item_lower.strip_prefix("force_null(") {
            let inner = rest.strip_suffix(')').unwrap_or(rest);
            options.force_null_columns = parse_copy_identifier_list(inner);
            return Ok(());
        }
        if item_lower.starts_with("convert_selectively") {
            return Ok(());
        }
        if item_lower.starts_with("freeze") || item_lower.starts_with("encoding ") {
            return Ok(());
        }
        Err(DbError::feature_not_supported(
            "COPY options are not supported; use plain COPY ... FROM STDIN or COPY ... TO STDOUT",
        ))
    };

    let parse_and_track_item = |item: String,
                                seen: &mut HashSet<String>,
                                options: &mut CopyCompatOptions|
     -> DbResult<()> {
        let key = copy_option_key(&item);
        if !key.is_empty() && !seen.insert(key) {
            return Err(DbError::parse_error(
                SqlState::SyntaxError,
                "conflicting or redundant options",
            ));
        }
        parse_option_item(&item, options)
    };

    if consume_word_ci(sql, &mut cursor, "with").is_some() {
        skip_sql_whitespace(sql, &mut cursor);
        if let Some(inner) = extract_parenthesized(sql, &mut cursor) {
            let items = split_top_level_csv_items(&inner).unwrap_or_default();
            let mut seen = HashSet::new();
            for item in items {
                parse_and_track_item(item, &mut seen, &mut options)?;
            }
        } else {
            let legacy_tail = sql[cursor..].trim();
            if let Some(items) = parse_copy_legacy_with_item_list(legacy_tail) {
                let mut seen = HashSet::new();
                for item in items {
                    parse_and_track_item(item, &mut seen, &mut options)?;
                }
            } else {
                let mut seen = HashSet::new();
                loop {
                    skip_sql_whitespace(sql, &mut cursor);
                    if cursor >= sql.len() || sql[cursor..].starts_with(';') {
                        break;
                    }

                    if consume_word_ci(sql, &mut cursor, "where").is_some() {
                        options.where_clause =
                            Some(sql[cursor..].trim().trim_end_matches(';').trim().to_owned());
                        break;
                    }

                    if consume_word_ci(sql, &mut cursor, "csv").is_some() {
                        parse_and_track_item("csv".to_owned(), &mut seen, &mut options)?;
                        continue;
                    }
                    if consume_word_ci(sql, &mut cursor, "binary").is_some() {
                        parse_and_track_item("binary".to_owned(), &mut seen, &mut options)?;
                        continue;
                    }
                    if consume_word_ci(sql, &mut cursor, "format").is_some() {
                        let format = ["csv", "text", "binary"]
                            .into_iter()
                            .find(|name| consume_word_ci(sql, &mut cursor, name).is_some())
                            .ok_or_else(|| {
                                DbError::parse_error(
                                    SqlState::SyntaxError,
                                    "expected format name after COPY FORMAT option",
                                )
                            })?;
                        parse_and_track_item(format!("format {format}"), &mut seen, &mut options)?;
                        continue;
                    }
                    if consume_word_ci(sql, &mut cursor, "freeze").is_some() {
                        let item = if consume_word_ci(sql, &mut cursor, "off").is_some() {
                            "freeze off".to_owned()
                        } else if consume_word_ci(sql, &mut cursor, "on").is_some() {
                            "freeze on".to_owned()
                        } else {
                            "freeze".to_owned()
                        };
                        parse_and_track_item(item, &mut seen, &mut options)?;
                        continue;
                    }
                    if consume_word_ci(sql, &mut cursor, "header").is_some() {
                        let item = if consume_word_ci(sql, &mut cursor, "match").is_some() {
                            "header match".to_owned()
                        } else if consume_word_ci(sql, &mut cursor, "on").is_some() {
                            "header on".to_owned()
                        } else if consume_word_ci(sql, &mut cursor, "off").is_some() {
                            "header off".to_owned()
                        } else {
                            "header".to_owned()
                        };
                        parse_and_track_item(item, &mut seen, &mut options)?;
                        continue;
                    }
                    if consume_word_ci(sql, &mut cursor, "delimiter").is_some() {
                        let _ = consume_word_ci(sql, &mut cursor, "as");
                        let (raw, _) =
                            parse_copy_sql_string_literal(sql, &mut cursor).ok_or_else(|| {
                                DbError::parse_error(
                                    SqlState::SyntaxError,
                                    "expected string literal after COPY DELIMITER option",
                                )
                            })?;
                        parse_and_track_item(format!("delimiter {raw}"), &mut seen, &mut options)?;
                        continue;
                    }
                    if consume_word_ci(sql, &mut cursor, "null").is_some() {
                        let _ = consume_word_ci(sql, &mut cursor, "as");
                        let (raw, _) =
                            parse_copy_sql_string_literal(sql, &mut cursor).ok_or_else(|| {
                                DbError::parse_error(
                                    SqlState::SyntaxError,
                                    "expected string literal after COPY NULL option",
                                )
                            })?;
                        parse_and_track_item(format!("null {raw}"), &mut seen, &mut options)?;
                        continue;
                    }
                    if consume_word_ci(sql, &mut cursor, "quote").is_some() {
                        let (raw, _) =
                            parse_copy_sql_string_literal(sql, &mut cursor).ok_or_else(|| {
                                DbError::parse_error(
                                    SqlState::SyntaxError,
                                    "expected string literal after COPY QUOTE option",
                                )
                            })?;
                        parse_and_track_item(format!("quote {raw}"), &mut seen, &mut options)?;
                        continue;
                    }
                    if consume_word_ci(sql, &mut cursor, "escape").is_some() {
                        let (raw, _) =
                            parse_copy_sql_string_literal(sql, &mut cursor).ok_or_else(|| {
                                DbError::parse_error(
                                    SqlState::SyntaxError,
                                    "expected string literal after COPY ESCAPE option",
                                )
                            })?;
                        parse_and_track_item(format!("escape {raw}"), &mut seen, &mut options)?;
                        continue;
                    }
                    if consume_word_ci(sql, &mut cursor, "encoding").is_some() {
                        let (raw, _) =
                            parse_copy_sql_string_literal(sql, &mut cursor).ok_or_else(|| {
                                DbError::parse_error(
                                    SqlState::SyntaxError,
                                    "expected string literal after COPY ENCODING option",
                                )
                            })?;
                        parse_and_track_item(format!("encoding {raw}"), &mut seen, &mut options)?;
                        continue;
                    }
                    if consume_word_ci(sql, &mut cursor, "default").is_some() {
                        let (raw, _) =
                            parse_copy_sql_string_literal(sql, &mut cursor).ok_or_else(|| {
                                DbError::parse_error(
                                    SqlState::SyntaxError,
                                    "expected string literal after COPY DEFAULT option",
                                )
                            })?;
                        parse_and_track_item(format!("default {raw}"), &mut seen, &mut options)?;
                        continue;
                    }
                    if consume_word_ci(sql, &mut cursor, "force").is_some() {
                        if consume_word_ci(sql, &mut cursor, "quote").is_some() {
                            skip_sql_whitespace(sql, &mut cursor);
                            let item = if sql[cursor..].starts_with('*') {
                                cursor += 1;
                                "force_quote *".to_owned()
                            } else if let Some(inner) = extract_parenthesized(sql, &mut cursor) {
                                format!("force_quote ({inner})")
                            } else if let Some(column) = parse_compat_identifier(sql, &mut cursor) {
                                format!("force_quote ({column})")
                            } else {
                                return Err(DbError::parse_error(
                                    SqlState::SyntaxError,
                                    "expected column name or * after COPY FORCE QUOTE",
                                ));
                            };
                            parse_and_track_item(item, &mut seen, &mut options)?;
                            continue;
                        }
                        if consume_word_ci(sql, &mut cursor, "not").is_some()
                            && consume_word_ci(sql, &mut cursor, "null").is_some()
                        {
                            let inner =
                                extract_parenthesized(sql, &mut cursor).ok_or_else(|| {
                                    DbError::parse_error(
                                        SqlState::SyntaxError,
                                        "expected column list after COPY FORCE NOT NULL",
                                    )
                                })?;
                            parse_and_track_item(
                                format!("force_not_null ({inner})"),
                                &mut seen,
                                &mut options,
                            )?;
                            continue;
                        }
                        if consume_word_ci(sql, &mut cursor, "null").is_some() {
                            let inner =
                                extract_parenthesized(sql, &mut cursor).ok_or_else(|| {
                                    DbError::parse_error(
                                        SqlState::SyntaxError,
                                        "expected column list after COPY FORCE NULL",
                                    )
                                })?;
                            parse_and_track_item(
                                format!("force_null ({inner})"),
                                &mut seen,
                                &mut options,
                            )?;
                            continue;
                        }
                    }
                    if consume_word_ci(sql, &mut cursor, "convert_selectively").is_some() {
                        let inner = extract_parenthesized(sql, &mut cursor).ok_or_else(|| {
                            DbError::parse_error(
                                SqlState::SyntaxError,
                                "expected column list after COPY CONVERT_SELECTIVELY",
                            )
                        })?;
                        parse_and_track_item(
                            format!("convert_selectively ({inner})"),
                            &mut seen,
                            &mut options,
                        )?;
                        continue;
                    }

                    return Err(DbError::feature_not_supported(
                    "COPY options are not supported; use plain COPY ... FROM STDIN or COPY ... TO STDOUT",
                ));
                }
            }
        }
    } else {
        let item_start = cursor;
        while cursor < sql.len() {
            if sql[cursor..].starts_with(';') {
                break;
            }
            if consume_word_ci(sql, &mut cursor, "where").is_some() {
                options.where_clause =
                    Some(sql[cursor..].trim().trim_end_matches(';').trim().to_owned());
                cursor = item_start;
                break;
            }
            cursor += sql[cursor..].chars().next().map_or(1, |ch| ch.len_utf8());
        }
        let tail = sql[item_start..cursor].trim();
        if !tail.is_empty() {
            if tail.starts_with('(') {
                let items = split_top_level_csv_items(&tail[1..tail.len() - 1]).unwrap_or_default();
                let mut seen = HashSet::new();
                for item in items {
                    parse_and_track_item(item, &mut seen, &mut options)?;
                }
            } else {
                let tail_sql = if tail.to_ascii_lowercase().starts_with("with ") {
                    tail.to_owned()
                } else {
                    format!("WITH {tail}")
                };
                let synthetic_sql = match direction {
                    aiondb_parser::CopyDirection::From => {
                        format!("COPY __copy__ FROM STDIN {tail_sql}")
                    }
                    aiondb_parser::CopyDirection::To => {
                        format!("COPY __copy__ TO STDOUT {tail_sql}")
                    }
                };
                let reparsed = parse_copy_sql_options(&synthetic_sql, direction)?;
                options.format = reparsed.format;
                options.delimiter = reparsed.delimiter;
                options.delimiter_explicit = reparsed.delimiter_explicit;
                options.null_string = reparsed.null_string;
                options.null_explicit = reparsed.null_explicit;
                options.default_string = reparsed.default_string;
                options.header = reparsed.header;
                options.header_match = reparsed.header_match;
                options.quote = reparsed.quote;
                options.escape = reparsed.escape;
                options.force_quote_all = reparsed.force_quote_all;
                options.force_quote_columns = reparsed.force_quote_columns;
                options.force_not_null_columns = reparsed.force_not_null_columns;
                options.force_null_columns = reparsed.force_null_columns;
            }
        }
    }

    if direction == aiondb_parser::CopyDirection::To && options.where_clause.is_some() {
        return Err(DbError::bind_error(
            SqlState::FeatureNotSupported,
            "WHERE clause not allowed with COPY TO",
        ));
    }
    if options.format == CopyCompatFormat::Binary && options.delimiter_explicit {
        return Err(DbError::bind_error(
            SqlState::FeatureNotSupported,
            "cannot specify DELIMITER in BINARY mode",
        ));
    }
    if options.format == CopyCompatFormat::Binary && options.null_explicit {
        return Err(DbError::bind_error(
            SqlState::FeatureNotSupported,
            "cannot specify NULL in BINARY mode",
        ));
    }
    if let Some(default_string) = options.default_string.as_ref() {
        if direction != aiondb_parser::CopyDirection::From {
            return Err(DbError::bind_error(
                SqlState::FeatureNotSupported,
                "COPY DEFAULT only available using COPY FROM",
            ));
        }
        if options.format == CopyCompatFormat::Binary {
            return Err(DbError::bind_error(
                SqlState::FeatureNotSupported,
                "cannot specify DEFAULT in BINARY mode",
            ));
        }
        if default_string.contains('\n') || default_string.contains('\r') {
            return Err(DbError::bind_error(
                SqlState::FeatureNotSupported,
                "COPY default representation cannot use newline or carriage return",
            ));
        }
        if default_string == &options.null_string {
            return Err(DbError::bind_error(
                SqlState::FeatureNotSupported,
                "NULL specification and DEFAULT specification cannot be the same",
            ));
        }
        if default_string.contains(options.delimiter) {
            return Err(DbError::bind_error(
                SqlState::FeatureNotSupported,
                "COPY delimiter must not appear in the DEFAULT specification",
            ));
        }
        if options.format == CopyCompatFormat::Csv && default_string.contains(options.quote) {
            return Err(DbError::bind_error(
                SqlState::FeatureNotSupported,
                "CSV quote character must not appear in the DEFAULT specification",
            ));
        }
    }
    if !options.force_quote_columns.is_empty() || options.force_quote_all {
        if direction != aiondb_parser::CopyDirection::To {
            return Err(DbError::bind_error(
                SqlState::FeatureNotSupported,
                "COPY force quote only available using COPY TO",
            ));
        }
        if options.format != CopyCompatFormat::Csv {
            return Err(DbError::bind_error(
                SqlState::FeatureNotSupported,
                "COPY force quote available only in CSV mode",
            ));
        }
    }
    if !options.force_not_null_columns.is_empty() {
        if options.format != CopyCompatFormat::Csv {
            return Err(DbError::bind_error(
                SqlState::FeatureNotSupported,
                "COPY force not null available only in CSV mode",
            ));
        }
        if direction != aiondb_parser::CopyDirection::From {
            return Err(DbError::bind_error(
                SqlState::FeatureNotSupported,
                "COPY force not null only available using COPY FROM",
            ));
        }
    }
    if !options.force_null_columns.is_empty() {
        if options.format != CopyCompatFormat::Csv {
            return Err(DbError::bind_error(
                SqlState::FeatureNotSupported,
                "COPY force null available only in CSV mode",
            ));
        }
        if direction != aiondb_parser::CopyDirection::From {
            return Err(DbError::bind_error(
                SqlState::FeatureNotSupported,
                "COPY force null only available using COPY FROM",
            ));
        }
    }

    Ok(options)
}

#[cfg(test)]
pub(crate) fn parse_copy_sql_options_for_test(
    sql: &str,
    direction: aiondb_parser::CopyDirection,
) -> DbResult<()> {
    let _ = parse_copy_sql_options(sql, direction)?;
    Ok(())
}

pub(in crate::engine) fn format_copy_value_with_options(
    value: &Value,
    options: &CopyCompatOptions,
    column_name: Option<&str>,
) -> String {
    match options.format {
        CopyCompatFormat::Csv => {
            if matches!(value, Value::Null) {
                return options.null_string.clone();
            }
            let force_quote = options.force_quote_all
                || column_name.is_some_and(|name| {
                    options
                        .force_quote_columns
                        .contains(&name.to_ascii_lowercase())
                });
            format_copy_csv_value(value, force_quote, options)
        }
        CopyCompatFormat::Text | CopyCompatFormat::Binary => {
            if matches!(value, Value::Null) {
                options.null_string.clone()
            } else {
                format_copy_text_value(value)
            }
        }
    }
}

pub(in crate::engine) fn validate_copy_endpoint(sql: &str, direction: aiondb_parser::CopyDirection) -> DbResult<()> {
    let (_, supported_endpoint) = copy_endpoint_info(sql, direction)
        .ok_or_else(|| DbError::parse_error(SqlState::SyntaxError, "invalid COPY statement"))?;
    if supported_endpoint {
        return Ok(());
    }
    match direction {
        aiondb_parser::CopyDirection::From => Err(DbError::feature_not_supported(
            "COPY FROM file is not supported; use COPY FROM STDIN",
        )),
        aiondb_parser::CopyDirection::To => Err(DbError::feature_not_supported(
            "COPY TO file is not supported; use COPY TO STDOUT",
        )),
    }
}

pub(in crate::engine) fn render_copy_rows(
    columns: &[crate::prepared::ResultColumn],
    rows: &[Row],
    options: &CopyCompatOptions,
) -> String {
    let separator = match options.format {
        CopyCompatFormat::Csv => options.delimiter,
        CopyCompatFormat::Text | CopyCompatFormat::Binary => options.delimiter,
    };
    let mut data = String::new();
    if options.header {
        let mut header_line = String::new();
        for (index, column) in columns.iter().enumerate() {
            if index > 0 {
                header_line.push(separator);
            }
            if options.format == CopyCompatFormat::Csv {
                header_line.push_str(&format_copy_csv_value(
                    &Value::Text(column.name.clone()),
                    false,
                    options,
                ));
            } else {
                header_line.push_str(&column.name);
            }
        }
        data.push_str(&header_line);
    }

    for row in rows {
        if !data.is_empty() {
            data.push('\n');
        }
        for (index, value) in row.values.iter().enumerate() {
            if index > 0 {
                data.push(separator);
            }
            data.push_str(&format_copy_value_with_options(
                value,
                options,
                columns.get(index).map(|column| column.name.as_str()),
            ));
        }
    }
    data
}

pub(in crate::engine) fn parse_copy_from_text_line(line: &str, delimiter: char) -> Vec<String> {
    let mut fields = Vec::new();
    let mut field = String::new();
    let mut chars = line.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '\\' {
            field.push(ch);
            if let Some(next) = chars.next() {
                field.push(next);
            }
            continue;
        }
        if ch == delimiter {
            fields.push(std::mem::take(&mut field));
            continue;
        }
        field.push(ch);
    }

    fields.push(field);
    fields
}

pub(in crate::engine) fn parse_copy_from_text_records(data: &str, delimiter: char) -> Vec<Vec<String>> {
    data.lines()
        .filter(|line| *line != "\\.")
        .map(|line| parse_copy_from_text_line(line, delimiter))
        .collect()
}

pub(in crate::engine) fn parse_copy_from_csv_records(
    data: &str,
    delimiter: char,
    quote: char,
) -> DbResult<Vec<Vec<CopyCsvField>>> {
    let mut records = Vec::new();
    let mut record = Vec::new();
    let mut field = String::new();
    let mut chars = data.chars().peekable();
    let mut in_quotes = false;
    let mut field_was_quoted = false;

    while let Some(ch) = chars.next() {
        if in_quotes {
            if ch == quote {
                if chars.peek().copied() == Some(quote) {
                    field.push(quote);
                    chars.next();
                } else {
                    in_quotes = false;
                }
            } else {
                field.push(ch);
            }
            continue;
        }

        if ch == quote {
            in_quotes = true;
            field_was_quoted = true;
            continue;
        }
        if ch == delimiter {
            record.push(CopyCsvField {
                value: std::mem::take(&mut field),
                quoted: std::mem::take(&mut field_was_quoted),
            });
            continue;
        }
        if ch == '\n' {
            record.push(CopyCsvField {
                value: std::mem::take(&mut field),
                quoted: std::mem::take(&mut field_was_quoted),
            });
            records.push(std::mem::take(&mut record));
            continue;
        }
        if ch == '\r' {
            continue;
        }
        field.push(ch);
    }

    if in_quotes {
        return Err(DbError::parse_error(
            SqlState::SyntaxError,
            "unterminated CSV quoted field in COPY data",
        ));
    }
    if !field.is_empty() || !record.is_empty() || field_was_quoted {
        record.push(CopyCsvField {
            value: field,
            quoted: field_was_quoted,
        });
        records.push(record);
    }
    Ok(records)
}

pub(in crate::engine) fn parse_copy_where_predicate(raw: &str) -> DbResult<CopyWherePredicate> {
    let trimmed = raw.trim();
    let lower = trimmed.to_ascii_lowercase();
    if lower.contains(" over(") {
        return Err(DbError::feature_not_supported(
            "window functions are not allowed in COPY FROM WHERE conditions",
        ));
    }
    if lower.contains("generate_series") {
        return Err(DbError::feature_not_supported(
            "set-returning functions are not allowed in COPY FROM WHERE conditions",
        ));
    }
    if lower.contains("select ") {
        return Err(DbError::feature_not_supported(
            "cannot use subquery in COPY FROM WHERE condition",
        ));
    }
    if lower.contains("max(")
        || lower.contains("min(")
        || lower.contains("sum(")
        || lower.contains("avg(")
    {
        return Err(DbError::feature_not_supported(
            "aggregate functions are not allowed in COPY FROM WHERE conditions",
        ));
    }

    for (token, op) in [
        ("<=", CopyWhereOp::Le),
        (">=", CopyWhereOp::Ge),
        ("!=", CopyWhereOp::Ne),
        ("=", CopyWhereOp::Eq),
        (">", CopyWhereOp::Gt),
        ("<", CopyWhereOp::Lt),
    ] {
        if let Some((left, right)) = trimmed.split_once(token) {
            return Ok(CopyWherePredicate {
                column: left.trim().trim_matches('"').to_ascii_lowercase(),
                op,
                literal: right.trim().trim_matches('\'').to_owned(),
            });
        }
    }

    Err(DbError::feature_not_supported(
        "COPY FROM WHERE supports only simple column-vs-literal predicates",
    ))
}

pub(in crate::engine) fn validate_copy_from_where_clause(
    options: &CopyCompatOptions,
    columns: &[CopyColumnCompat],
) -> DbResult<()> {
    let Some(raw) = options.where_clause.as_deref() else {
        return Ok(());
    };
    let predicate = parse_copy_where_predicate(raw)?;
    if columns
        .iter()
        .all(|column| !column.name.eq_ignore_ascii_case(&predicate.column))
    {
        return Err(DbError::bind_error(
            SqlState::UndefinedColumn,
            format!("column \"{}\" does not exist", predicate.column),
        ));
    }
    Ok(())
}

pub(in crate::engine) fn validate_copy_force_column_references(
    options: &CopyCompatOptions,
    columns: &[CopyColumnCompat],
) -> DbResult<()> {
    for column_name in &options.force_not_null_columns {
        if columns
            .iter()
            .all(|column| !column.name.eq_ignore_ascii_case(column_name))
        {
            return Err(DbError::bind_error(
                SqlState::UndefinedColumn,
                format!("FORCE_NOT_NULL column \"{column_name}\" not referenced by COPY"),
            ));
        }
    }
    for column_name in &options.force_null_columns {
        if columns
            .iter()
            .all(|column| !column.name.eq_ignore_ascii_case(column_name))
        {
            return Err(DbError::bind_error(
                SqlState::UndefinedColumn,
                format!("FORCE_NULL column \"{column_name}\" not referenced by COPY"),
            ));
        }
    }
    Ok(())
}

pub(in crate::engine) fn copy_where_matches(
    predicate: &CopyWherePredicate,
    columns: &[CopyColumnCompat],
    fields: &[String],
) -> DbResult<bool> {
    let Some(index) = columns
        .iter()
        .position(|column| column.name.eq_ignore_ascii_case(&predicate.column))
    else {
        return Err(DbError::bind_error(
            SqlState::UndefinedColumn,
            format!("column \"{}\" does not exist", predicate.column),
        ));
    };
    let value = fields.get(index).map(String::as_str).unwrap_or("");
    match columns[index].data_type {
        DataType::Int | DataType::BigInt => {
            let left = value.parse::<i64>().unwrap_or_default();
            let right = predicate.literal.parse::<i64>().unwrap_or_default();
            Ok(match predicate.op {
                CopyWhereOp::Eq => left == right,
                CopyWhereOp::Ne => left != right,
                CopyWhereOp::Gt => left > right,
                CopyWhereOp::Ge => left >= right,
                CopyWhereOp::Lt => left < right,
                CopyWhereOp::Le => left <= right,
            })
        }
        _ => Ok(match predicate.op {
            CopyWhereOp::Eq => value == predicate.literal,
            CopyWhereOp::Ne => value != predicate.literal,
            CopyWhereOp::Gt => value > predicate.literal.as_str(),
            CopyWhereOp::Ge => value >= predicate.literal.as_str(),
            CopyWhereOp::Lt => value < predicate.literal.as_str(),
            CopyWhereOp::Le => value <= predicate.literal.as_str(),
        }),
    }
}

pub(in crate::engine) fn render_copy_default_field(default_expr: &str, data_type: &DataType) -> DbResult<String> {
    let trimmed = default_expr.trim();
    let decoded_literal = decode_sql_single_quoted_literal(trimmed);
    let rendered = match data_type {
        DataType::Text => decoded_literal.unwrap_or_else(|| trimmed.to_owned()),
        DataType::Timestamp
        | DataType::TimestampTz
        | DataType::Date
        | DataType::Time
        | DataType::TimeTz
        | DataType::Interval
        | DataType::Money => decoded_literal.unwrap_or_else(|| trimmed.to_owned()),
        _ => trimmed.to_owned(),
    };
    Ok(escape_copy_text_value(&rendered))
}

pub(in crate::engine) fn copy_default_marker_error(
    table_name: &str,
    column_name: &str,
    line_number: usize,
    raw_line: &str,
) -> DbError {
    DbError::bind_error(
        SqlState::FeatureNotSupported,
        "unexpected default marker in COPY data",
    )
    .with_client_detail(format!("Column \"{column_name}\" has no default value."))
    .with_client_hint(format!(
        "COPY {table_name}, line {line_number}: \"{raw_line}\""
    ))
}

pub(in crate::engine) fn normalize_copy_from_data(
    options: &CopyCompatOptions,
    table_name: &str,
    columns: &[CopyColumnCompat],
    data: &str,
) -> DbResult<String> {
    if options.format == CopyCompatFormat::Binary {
        return Err(DbError::feature_not_supported(
            "COPY BINARY is not supported; only text and CSV formats are supported",
        ));
    }

    let raw_lines: Vec<&str> = data.lines().filter(|line| *line != "\\.").collect();
    let csv_records = if options.format == CopyCompatFormat::Csv {
        Some(parse_copy_from_csv_records(
            data,
            options.delimiter,
            options.quote,
        )?)
    } else {
        None
    };
    let mut records = match options.format {
        CopyCompatFormat::Csv => match csv_records.as_ref() {
            Some(records) => records
                .iter()
                .map(|record| record.iter().map(|field| field.value.clone()).collect())
                .collect(),
            None => {
                return Err(DbError::internal(
                    "COPY CSV parser did not produce records for CSV input",
                ));
            }
        },
        CopyCompatFormat::Text => parse_copy_from_text_records(data, options.delimiter),
        CopyCompatFormat::Binary => unreachable!(),
    };

    if options.header || options.header_match {
        if let Some(header_row) = records.first() {
            if options.header_match {
                let expected: Vec<String> =
                    columns.iter().map(|column| column.name.clone()).collect();
                if *header_row != expected {
                    return Err(DbError::bind_error(
                        SqlState::SyntaxError,
                        "COPY header does not match target columns",
                    ));
                }
            }
        }
        if !records.is_empty() {
            records.remove(0);
        }
    }

    let predicate = options
        .where_clause
        .as_deref()
        .map(parse_copy_where_predicate)
        .transpose()?;

    let mut normalized_rows = Vec::new();
    let start_line_number = usize::from(options.header || options.header_match) + 1;
    for (record_index, mut fields) in records.into_iter().enumerate() {
        if let Some(predicate) = predicate.as_ref() {
            if !copy_where_matches(predicate, columns, &fields)? {
                continue;
            }
        }
        let mut preserve_empty_string = HashSet::new();
        if options.format == CopyCompatFormat::Csv {
            let quoted_flags = csv_records
                .as_ref()
                .and_then(|records| records.get(record_index))
                .map(|record| record.iter().map(|field| field.quoted).collect::<Vec<_>>())
                .unwrap_or_default();
            for (index, field) in fields.iter_mut().enumerate() {
                let column_name = columns
                    .get(index)
                    .map(|column| column.name.to_ascii_lowercase())
                    .unwrap_or_default();
                let was_quoted = quoted_flags.get(index).copied().unwrap_or(false);
                if options.force_null_columns.contains(&column_name)
                    && was_quoted
                    && field.is_empty()
                {
                    *field = options.null_string.clone();
                }
                if options.force_not_null_columns.contains(&column_name)
                    && !was_quoted
                    && *field == options.null_string
                {
                    field.clear();
                    preserve_empty_string.insert(index);
                }
            }
        }

        if let Some(default_marker) = options.default_string.as_ref() {
            let line_number = start_line_number + record_index;
            let raw_line = raw_lines.get(record_index).copied().unwrap_or_default();
            for (index, field) in fields.iter_mut().enumerate() {
                let was_quoted = csv_records
                    .as_ref()
                    .and_then(|records| records.get(record_index))
                    .and_then(|record| record.get(index))
                    .is_some_and(|field_meta| field_meta.quoted);
                if field != default_marker || was_quoted {
                    continue;
                }
                let Some(column) = columns.get(index) else {
                    continue;
                };
                let Some(default_expr) = column.default_value.as_deref() else {
                    return Err(copy_default_marker_error(
                        table_name,
                        &column.name,
                        line_number,
                        raw_line,
                    ));
                };
                *field = render_copy_default_field(default_expr, &column.data_type)?;
            }
        }

        let normalized = fields
            .into_iter()
            .enumerate()
            .map(|(index, field)| {
                if field == options.null_string && !preserve_empty_string.contains(&index) {
                    "\\N".to_owned()
                } else if options.format == CopyCompatFormat::Csv {
                    escape_copy_text_value(&field)
                } else {
                    field
                }
            })
            .collect::<Vec<_>>()
            .join("\t");
        normalized_rows.push(normalized);
    }

    let mut normalized = normalized_rows.join("\n");
    if !normalized_rows.is_empty() {
        normalized.push('\n');
    }
    Ok(normalized)
}

pub(in crate::engine) fn validate_copy_column_count(data: &str, expected_columns: usize) -> DbResult<()> {
    if expected_columns == 0 {
        return Ok(());
    }
    for line in data.lines() {
        if line == "\\." {
            break;
        }
        let got_columns = if line.is_empty() {
            expected_columns
        } else {
            parse_copy_from_text_line(line, '\t').len()
        };
        if got_columns != expected_columns {
            return Err(DbError::bind_error(
                SqlState::InvalidTextRepresentation,
                format!("expected {expected_columns} columns, got {got_columns}"),
            ));
        }
    }
    Ok(())
}

pub(in crate::engine) fn quote_sql_ident(ident: &str) -> String {
    let starts_with_digit = ident
        .as_bytes()
        .first()
        .is_some_and(|ch| ch.is_ascii_digit());
    let simple = !ident.is_empty()
        && !starts_with_digit
        && ident
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_');
    if simple {
        ident.to_owned()
    } else {
        format!("\"{}\"", ident.replace('"', "\"\""))
    }
}

pub(in crate::engine) fn render_copy_insert_expr(field: &str, column: &CopyColumnCompat) -> String {
    if field == "\\N" {
        return "NULL".to_owned();
    }

    match column.data_type {
        DataType::Text => {
            let value = unescape_copy_text_value(field);
            format!("'{}'", value.replace('\'', "''"))
        }
        DataType::Timestamp
        | DataType::TimestampTz
        | DataType::Date
        | DataType::Time
        | DataType::TimeTz
        | DataType::Interval
        | DataType::Money => {
            let value = unescape_copy_text_value(field);
            let value = value
                .strip_prefix('\'')
                .and_then(|inner| inner.strip_suffix('\''))
                .unwrap_or(&value);
            format!("'{}'", value.replace('\'', "''"))
        }
        _ => {
            // Always SQL-quote and escape the value before splicing it into
            // the synthesised INSERT. Without this, a non-text COPY field
            // (Int / BigInt / Bool / Numeric / ...) is interpolated raw and
            // an attacker can inject statements via `field = "1),('x'); DROP
            // TABLE t; --"`. The downstream parser still applies the implicit
            // cast to the column's declared type, so legitimate numeric /
            // boolean literals continue to round-trip.
            let value = unescape_copy_text_value(field);
            format!("'{}'", value.replace('\'', "''"))
        }
    }
}

pub(in crate::engine) fn pending_copy_statement(session_sql: &str) -> DbResult<aiondb_parser::CopyStatement> {
    let mut statements = aiondb_parser::parse_sql(session_sql)?;
    let Some(aiondb_parser::Statement::Copy(copy)) = statements.pop() else {
        return Err(DbError::internal(
            "pending COPY statement SQL did not reparse as COPY",
        ));
    };
    Ok(copy)
}

pub(in crate::engine) fn parse_simple_instead_of_insert_trigger_mapping(
    body: &str,
) -> Option<(aiondb_catalog::QualifiedName, Vec<String>, Vec<String>)> {
    let compact = body.replace(['\n', '\r', '\t'], " ");
    let lower = compact.to_ascii_lowercase();
    let insert_pos = lower.find("insert into ")?;
    let after_insert = &compact[insert_pos + "insert into ".len()..];
    let open_cols = after_insert.find('(')?;
    let table_name = after_insert[..open_cols].trim();
    let after_table = &after_insert[open_cols + 1..];
    let close_cols = after_table.find(')')?;
    let target_columns = after_table[..close_cols]
        .split(',')
        .map(|part| part.trim().trim_matches('"').to_owned())
        .collect::<Vec<_>>();

    let after_columns = &after_table[close_cols + 1..];
    let values_pos = after_columns.to_ascii_lowercase().find("values")?;
    let after_values = &after_columns[values_pos + "values".len()..];
    let open_vals = after_values.find('(')?;
    let after_open_vals = &after_values[open_vals + 1..];
    let close_vals = after_open_vals.find(')')?;
    let source_columns = after_open_vals[..close_vals]
        .split(',')
        .map(|part| {
            let part = part.trim().to_ascii_lowercase();
            part.strip_prefix("new.")
                .map(|value| value.trim_matches('"').to_owned())
        })
        .collect::<Option<Vec<_>>>()?;

    if target_columns.len() != source_columns.len() || target_columns.is_empty() {
        return None;
    }

    Some((
        aiondb_catalog::QualifiedName::parse(table_name),
        target_columns,
        source_columns,
    ))
}

pub(in crate::engine) fn render_sql_literal_from_copy_field(field: &str) -> String {
    if field == "\\N" {
        return "NULL".to_owned();
    }
    let value = unescape_copy_text_value(field);
    format!("'{}'", value.replace('\'', "''"))
}

pub(in crate::engine) fn resolve_copy_trigger_function(
    catalog: &dyn aiondb_catalog::CatalogReader,
    txn_id: aiondb_core::TxnId,
    function_name: &str,
) -> DbResult<Option<aiondb_catalog::FunctionDescriptor>> {
    if let Some(function) = catalog.get_function(txn_id, function_name)? {
        return Ok(Some(function));
    }
    let lookup = function_name.to_ascii_lowercase();
    for function in catalog.list_functions(txn_id)? {
        let candidate = function.name.to_ascii_lowercase();
        if candidate == lookup || candidate.ends_with(&format!(".{lookup}")) {
            return Ok(Some(function));
        }
    }
    Ok(None)
}
