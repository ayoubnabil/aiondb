pub(super) fn parse_compat_cursor_declare(sql: &str) -> Option<CompatCursorDeclare> {
    let sql = trim_compat_statement(sql);
    let mut cursor = 0usize;
    consume_word_ci(sql, &mut cursor, "declare")?;
    let portal_name = parse_compat_identifier(sql, &mut cursor)?;

    let mut saw_cursor = false;
    let mut scrollable = true;
    let mut holdable = false;
    while cursor < sql.len() {
        if let Some(token) = parse_compat_identifier(sql, &mut cursor) {
            if !saw_cursor {
                match token.as_str() {
                    "scroll" => scrollable = true,
                    "no" => {
                        let saved = cursor;
                        if let Some(next) = parse_compat_identifier(sql, &mut cursor) {
                            if next == "scroll" {
                                scrollable = false;
                                continue;
                            }
                        }
                        cursor = saved;
                    }
                    "cursor" => saw_cursor = true,
                    "with" => {
                        let saved = cursor;
                        if let Some(next) = parse_compat_identifier(sql, &mut cursor) {
                            if next == "hold" {
                                holdable = true;
                                continue;
                            }
                        }
                        cursor = saved;
                    }
                    "without" => {
                        let saved = cursor;
                        if let Some(next) = parse_compat_identifier(sql, &mut cursor) {
                            if next == "hold" {
                                holdable = false;
                                continue;
                            }
                        }
                        cursor = saved;
                    }
                    _ => {}
                }
                continue;
            }

            match token.as_str() {
                "with" => {
                    let saved = cursor;
                    if let Some(next) = parse_compat_identifier(sql, &mut cursor) {
                        if next == "hold" {
                            holdable = true;
                            continue;
                        }
                    }
                    cursor = saved;
                }
                "without" => {
                    let saved = cursor;
                    if let Some(next) = parse_compat_identifier(sql, &mut cursor) {
                        if next == "hold" {
                            holdable = false;
                            continue;
                        }
                    }
                    cursor = saved;
                }
                _ => {}
            }

            if token == "for" {
                let query_sql = sql[cursor..].trim();
                if query_sql.is_empty() {
                    return None;
                }
                return Some(CompatCursorDeclare {
                    portal_name,
                    query_sql: query_sql.to_owned(),
                    scrollable,
                    holdable,
                });
            }
            continue;
        }
        let ch = sql[cursor..].chars().next()?;
        cursor += ch.len_utf8();
    }

    None
}

pub(super) fn parse_compat_cursor_fetch(sql: &str) -> Option<CompatCursorFetch> {
    let sql = trim_compat_statement(sql);
    let mut rest = strip_compat_word_ci(sql, "fetch")?;
    let mut max_rows = 1usize;
    let mut all_rows = false;
    let mut direction = CompatCursorFetchDirection::Forward;
    if let Some(next) = strip_compat_word_ci(rest, "all") {
        all_rows = true;
        rest = next;
    } else if let Some(next) = strip_compat_word_ci(rest, "next") {
        rest = next;
        if let Some((count, remaining)) = parse_leading_compat_uint(rest) {
            max_rows = count;
            rest = remaining;
        }
    } else if let Some(next) = strip_compat_word_ci(rest, "forward") {
        rest = next;
        if let Some(next) = strip_compat_word_ci(rest, "all") {
            all_rows = true;
            rest = next;
        } else if let Some((count, remaining)) = parse_leading_compat_uint(rest) {
            max_rows = count;
            rest = remaining;
        }
    } else if let Some(next) = strip_compat_word_ci(rest, "backward") {
        direction = CompatCursorFetchDirection::Backward;
        rest = next;
        if let Some(next) = strip_compat_word_ci(rest, "all") {
            all_rows = true;
            rest = next;
        } else if let Some((count, remaining)) = parse_leading_compat_uint(rest) {
            max_rows = count;
            rest = remaining;
        }
    } else if let Some(next) = strip_compat_word_ci(rest, "prior") {
        direction = CompatCursorFetchDirection::Prior;
        rest = next;
    } else if let Some(next) = strip_compat_word_ci(rest, "first") {
        direction = CompatCursorFetchDirection::First;
        rest = next;
    } else if let Some(next) = strip_compat_word_ci(rest, "last") {
        direction = CompatCursorFetchDirection::Last;
        rest = next;
    } else if let Some(next) = strip_compat_word_ci(rest, "absolute") {
        rest = next;
        let (target, remaining) = parse_leading_compat_int(rest)?;
        direction = CompatCursorFetchDirection::Absolute(target);
        max_rows = 1;
        all_rows = false;
        rest = remaining;
    } else if let Some((count, remaining)) = parse_leading_compat_uint(rest) {
        max_rows = count;
        rest = remaining;
    }

    if let Some(next) =
        strip_compat_word_ci(rest, "in").or_else(|| strip_compat_word_ci(rest, "from"))
    {
        rest = next;
    }

    let mut cursor = 0usize;
    let portal_name = parse_compat_identifier(rest, &mut cursor)?;
    skip_sql_whitespace(rest, &mut cursor);
    if cursor != rest.len() {
        return None;
    }

    Some(CompatCursorFetch {
        portal_name,
        max_rows,
        all_rows,
        direction,
    })
}

pub(super) fn parse_compat_cursor_move(sql: &str) -> Option<CompatCursorFetch> {
    let sql = trim_compat_statement(sql);
    let mut rest = strip_compat_word_ci(sql, "move")?;
    let mut max_rows = 1usize;
    let mut all_rows = false;
    let mut direction = CompatCursorFetchDirection::Forward;
    if let Some(next) = strip_compat_word_ci(rest, "all") {
        all_rows = true;
        rest = next;
    } else if let Some(next) = strip_compat_word_ci(rest, "forward") {
        rest = next;
        if let Some(next) = strip_compat_word_ci(rest, "all") {
            all_rows = true;
            rest = next;
        } else if let Some((count, remaining)) = parse_leading_compat_uint(rest) {
            max_rows = count;
            rest = remaining;
        }
    } else if let Some(next) = strip_compat_word_ci(rest, "backward") {
        direction = CompatCursorFetchDirection::Backward;
        rest = next;
        if let Some(next) = strip_compat_word_ci(rest, "all") {
            all_rows = true;
            rest = next;
        } else if let Some((count, remaining)) = parse_leading_compat_uint(rest) {
            max_rows = count;
            rest = remaining;
        }
    } else if let Some((count, remaining)) = parse_leading_compat_uint(rest) {
        max_rows = count;
        rest = remaining;
    }

    if let Some(next) =
        strip_compat_word_ci(rest, "in").or_else(|| strip_compat_word_ci(rest, "from"))
    {
        rest = next;
    }

    let mut cursor = 0usize;
    let portal_name = parse_compat_identifier(rest, &mut cursor)?;
    skip_sql_whitespace(rest, &mut cursor);
    if cursor != rest.len() {
        return None;
    }

    Some(CompatCursorFetch {
        portal_name,
        max_rows,
        all_rows,
        direction,
    })
}

pub(super) fn parse_compat_cursor_close(sql: &str) -> Option<String> {
    let sql = trim_compat_statement(sql);
    let mut cursor = 0usize;
    consume_word_ci(sql, &mut cursor, "close")?;
    let portal_name = parse_compat_identifier(sql, &mut cursor)?;
    skip_sql_whitespace(sql, &mut cursor);
    if cursor != sql.len() {
        return None;
    }
    Some(portal_name)
}

struct ParsedCreateRule {
    relation_name: String,
    event: String,
    do_instead: bool,
    action_sql: String,
    returning_count: usize,
    with_dml_unsupported_error: Option<&'static str>,
    with_query_transition_ref_error: Option<&'static str>,
    action_validation_error: Option<ParsedRuleActionValidationError>,
}

struct ParsedRuleActionValidationError {
    sqlstate: aiondb_core::SqlState,
    message: String,
    hint: Option<&'static str>,
}

struct ParsedDropRuleTarget {
    rule_name: String,
    relation_name: String,
    if_exists: bool,
}

struct ParsedDropRuleName {
    rule_name: String,
    if_exists: bool,
}

fn parse_create_rule_sql(sql: &str) -> Option<ParsedCreateRule> {
    let sql = trim_compat_statement(sql);
    let upper = sql.to_ascii_uppercase();
    let event;
    let on_pos;
    if let Some(p) = upper.find(" ON INSERT TO ") {
        event = "INSERT";
        on_pos = p;
    } else if let Some(p) = upper.find(" ON UPDATE TO ") {
        event = "UPDATE";
        on_pos = p;
    } else if let Some(p) = upper.find(" ON DELETE TO ") {
        event = "DELETE";
        on_pos = p;
    } else {
        return None;
    }

    let after_to = on_pos + format!(" ON {event} TO ").len();
    let mut relation_cursor = after_to;
    let mut relation_name = parse_compat_identifier(sql, &mut relation_cursor)?;
    skip_sql_whitespace(sql, &mut relation_cursor);
    if sql
        .get(relation_cursor..)
        .is_some_and(|rest| rest.starts_with('.'))
    {
        relation_cursor += 1;
        let relation_part = parse_compat_identifier(sql, &mut relation_cursor)?;
        relation_name.push('.');
        relation_name.push_str(&relation_part);
    }

    let do_rel_pos = find_keyword_not_in_parens(&upper[after_to..], "DO")?;
    let do_abs_pos = after_to.saturating_add(do_rel_pos);
    let where_between = find_keyword_not_in_parens(&upper[after_to..do_abs_pos], "WHERE").is_some();

    let mut do_cursor = do_abs_pos;
    consume_word_ci(sql, &mut do_cursor, "DO")?;
    let do_clause = sql[do_cursor..].trim_start();
    let do_clause_upper = do_clause.to_ascii_uppercase();

    let (do_instead, action_sql, with_dml_unsupported_error) = if do_clause_upper
        .starts_with("INSTEAD")
    {
        let mut clause_cursor = 0usize;
        consume_word_ci(do_clause, &mut clause_cursor, "INSTEAD")?;
        let action_sql = do_clause[clause_cursor..].trim().to_owned();
        let action_upper = action_sql.to_ascii_uppercase();
        if action_upper.starts_with("NOTHING") && action_upper != "NOTHING" {
            return None;
        }

        let unsupported = if where_between {
            Some(
                    "conditional DO INSTEAD rules are not supported for data-modifying statements in WITH",
                )
        } else if action_upper == "NOTHING" {
            Some("DO INSTEAD NOTHING rules are not supported for data-modifying statements in WITH")
        } else if action_upper == "NOTIFY" || action_upper.starts_with("NOTIFY ") {
            Some("DO INSTEAD NOTIFY rules are not supported for data-modifying statements in WITH")
        } else if action_sql.starts_with('(') && action_sql.contains(';') {
            Some(
                    "multi-statement DO INSTEAD rules are not supported for data-modifying statements in WITH",
                )
        } else {
            None
        };
        (true, action_sql, unsupported)
    } else if do_clause_upper.starts_with("ALSO") {
        let mut clause_cursor = 0usize;
        consume_word_ci(do_clause, &mut clause_cursor, "ALSO")?;
        (
            false,
            do_clause[clause_cursor..].trim().to_owned(),
            Some("DO ALSO rules are not supported for data-modifying statements in WITH"),
        )
    } else {
        return None;
    };

    let returning_count = if do_instead {
        count_returning_items(&action_sql)
    } else {
        0
    };

    let with_query_transition_ref_error = detect_with_query_transition_ref_error(&action_sql);

    let action_validation_error = detect_for_update_transition_relation_error(&action_sql)
        .map_or_else(
            || {
                detect_insert_values_single_identifier_error(&action_sql).map(|identifier| {
                    ParsedRuleActionValidationError {
                        sqlstate: aiondb_core::SqlState::UndefinedColumn,
                        message: format!("column \"{identifier}\" does not exist"),
                        hint: Some("Try using a table-qualified name."),
                    }
                })
            },
            |relation| {
                Some(ParsedRuleActionValidationError {
                    sqlstate: aiondb_core::SqlState::UndefinedTable,
                    message: format!(
                        "relation \"{relation}\" in FOR UPDATE clause not found in FROM clause"
                    ),
                    hint: None,
                })
            },
        );

    Some(ParsedCreateRule {
        relation_name,
        event: event.to_owned(),
        do_instead,
        action_sql,
        returning_count,
        with_dml_unsupported_error,
        with_query_transition_ref_error,
        action_validation_error,
    })
}

fn detect_for_update_transition_relation_error(action_sql: &str) -> Option<String> {
    let upper_sql = action_sql.to_ascii_uppercase();
    let mut scan_offset = 0usize;
    while scan_offset < upper_sql.len() {
        let rel_pos = find_keyword_not_in_parens(&upper_sql[scan_offset..], "FOR UPDATE OF")?;
        let mut cursor = scan_offset + rel_pos + "FOR UPDATE OF".len();
        loop {
            let relation_name = parse_compat_identifier(action_sql, &mut cursor)?;
            if relation_name.eq_ignore_ascii_case("old")
                || relation_name.eq_ignore_ascii_case("new")
            {
                return Some(relation_name.to_ascii_lowercase());
            }
            skip_sql_whitespace(action_sql, &mut cursor);
            if action_sql
                .get(cursor..)
                .is_some_and(|remaining| remaining.starts_with(','))
            {
                cursor += 1;
                continue;
            }
            break;
        }
        scan_offset = cursor.saturating_add(1);
    }
    None
}

fn detect_insert_values_single_identifier_error(action_sql: &str) -> Option<String> {
    let statement = aiondb_parser::parse_prepared_statement(action_sql).ok()?;
    let aiondb_parser::Statement::Insert(insert) = statement else {
        return None;
    };
    if insert.rows.len() != 1 {
        return None;
    }
    let row = insert.rows.first()?;
    if row.len() != 1 {
        return None;
    }
    let aiondb_parser::Expr::Identifier(identifier) = row.first()? else {
        return None;
    };
    if identifier.parts.len() != 1 {
        return None;
    }
    let name = identifier.parts.first()?.to_ascii_lowercase();
    if matches!(
        name.as_str(),
        "current_user"
            | "session_user"
            | "current_role"
            | "user"
            | "current_date"
            | "current_time"
            | "current_timestamp"
            | "localtime"
            | "localtimestamp"
    ) {
        return None;
    }
    Some(name)
}

fn detect_with_query_transition_ref_error(action_sql: &str) -> Option<&'static str> {
    let sql = action_sql.trim_start();
    strip_compat_word_ci(sql, "WITH")?;

    let upper = sql.to_ascii_uppercase();
    let mut statement_start = None;
    for keyword in ["INSERT", "UPDATE", "DELETE", "SELECT", "VALUES"] {
        if let Some(pos) = find_keyword_not_in_parens(&upper, keyword) {
            if pos > 0 {
                statement_start =
                    Some(statement_start.map_or(pos, |current: usize| current.min(pos)));
            }
        }
    }
    let statement_start = statement_start?;

    let with_clause = &sql[..statement_start];
    if contains_transition_ref_with_dot(with_clause, "OLD") {
        return Some("cannot refer to OLD within WITH query");
    }
    if contains_transition_ref_with_dot(with_clause, "NEW") {
        return Some("cannot refer to NEW within WITH query");
    }
    None
}

fn contains_transition_ref_with_dot(haystack: &str, ident: &str) -> bool {
    let bytes = haystack.as_bytes();
    let ident_bytes = ident.as_bytes();
    if bytes.len() < ident_bytes.len() {
        return false;
    }

    for i in 0..=bytes.len() - ident_bytes.len() {
        if !bytes[i..i + ident_bytes.len()].eq_ignore_ascii_case(ident_bytes) {
            continue;
        }

        if i > 0 && (bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_') {
            continue;
        }

        let mut j = i + ident_bytes.len();
        while j < bytes.len() && bytes[j].is_ascii_whitespace() {
            j += 1;
        }
        if j < bytes.len() && bytes[j] == b'.' {
            return true;
        }
    }

    false
}

fn parse_drop_rule_target(sql: &str) -> Option<ParsedDropRuleTarget> {
    let sql = trim_compat_statement(sql);
    let mut cursor = 0usize;
    consume_word_ci(sql, &mut cursor, "DROP")?;
    consume_word_ci(sql, &mut cursor, "RULE")?;

    let if_exists = consume_word_ci(sql, &mut cursor, "IF")
        .and_then(|()| consume_word_ci(sql, &mut cursor, "EXISTS"));
    let if_exists = if_exists.is_some();

    let rule_name = parse_compat_identifier(sql, &mut cursor)?;
    consume_word_ci(sql, &mut cursor, "ON")?;

    let mut relation_name = parse_compat_identifier(sql, &mut cursor)?;
    skip_sql_whitespace(sql, &mut cursor);
    if sql.get(cursor..).is_some_and(|rest| rest.starts_with('.')) {
        cursor += 1;
        let relation_part = parse_compat_identifier(sql, &mut cursor)?;
        relation_name.push('.');
        relation_name.push_str(&relation_part);
    }
    skip_sql_whitespace(sql, &mut cursor);
    let tail = sql.get(cursor..)?.trim();
    if !tail.is_empty() && tail != ";" {
        return None;
    }
    Some(ParsedDropRuleTarget {
        rule_name,
        relation_name,
        if_exists,
    })
}

fn parse_drop_rule_name(sql: &str) -> Option<ParsedDropRuleName> {
    let sql = trim_compat_statement(sql);
    let mut cursor = 0usize;
    consume_word_ci(sql, &mut cursor, "DROP")?;
    consume_word_ci(sql, &mut cursor, "RULE")?;

    let if_exists = consume_word_ci(sql, &mut cursor, "IF")
        .and_then(|()| consume_word_ci(sql, &mut cursor, "EXISTS"))
        .is_some();
    let rule_name = parse_compat_identifier(sql, &mut cursor)?;
    if consume_word_ci(sql, &mut cursor, "ON").is_some() {
        return None;
    }
    skip_sql_whitespace(sql, &mut cursor);
    let tail = sql.get(cursor..)?.trim();
    if !tail.is_empty() && tail != ";" {
        return None;
    }
    Some(ParsedDropRuleName {
        rule_name,
        if_exists,
    })
}

fn count_returning_items(sql: &str) -> usize {
    let upper = sql.to_ascii_uppercase();
    let Some(ret_pos) = find_keyword_not_in_parens(&upper, "RETURNING") else {
        return 0;
    };
    let returning_list = &sql[ret_pos + "RETURNING".len()..].trim();
    if returning_list.is_empty() {
        return 0;
    }
    let mut count = 1usize;
    let mut depth = 0i32;
    for ch in returning_list.chars() {
        match ch {
            '(' => depth += 1,
            ')' => depth -= 1,
            ',' if depth == 0 => count += 1,
            _ => {}
        }
    }
    if returning_list.trim() == "*" {
        return usize::MAX;
    }
    count
}

/// Re-emit a parsed identifier safely. Each part is wrapped in `"..."` with
/// any embedded `"` doubled, so a quoted source-text identifier like
/// `"x); DROP TABLE secret; SELECT (1"` round-trips as a single quoted name
/// and cannot smuggle SQL on re-parse (audit engine_compat F-CL-1).
fn quote_object_name(name: &aiondb_parser::ObjectName) -> String {
    let mut out = String::new();
    for (i, part) in name.parts.iter().enumerate() {
        if i > 0 {
            out.push('.');
        }
        out.push('"');
        for ch in part.chars() {
            if ch == '"' {
                out.push('"');
                out.push('"');
            } else {
                out.push(ch);
            }
        }
        out.push('"');
    }
    out
}

fn reconstruct_expr_sql(expr: &aiondb_parser::Expr) -> String {
    use aiondb_parser::Expr;
    match expr {
        Expr::Identifier(name) => quote_object_name(name),
        Expr::Literal(lit, _span) => match lit {
            aiondb_parser::Literal::Integer(n) => n.to_string(),
            aiondb_parser::Literal::NumericLit(s) => s.clone(),
            aiondb_parser::Literal::String(s) => format!("'{}'", s.replace('\'', "''")),
            aiondb_parser::Literal::Boolean(b) => if *b { "TRUE" } else { "FALSE" }.to_owned(),
            aiondb_parser::Literal::Null => "NULL".to_owned(),
        },
        Expr::Parameter { index, .. } => format!("${index}"),
        Expr::Default { .. } => "DEFAULT".to_owned(),
        Expr::FunctionCall {
            name,
            args,
            distinct,
            filter,
            ..
        } => {
            let func_name = quote_object_name(name);
            let args_sql: Vec<String> = args.iter().map(reconstruct_expr_sql).collect();
            let distinct_str = if *distinct { "DISTINCT " } else { "" };
            let mut out = format!("{func_name}({distinct_str}{})", args_sql.join(", "));
            if let Some(f) = filter {
                out.push_str(" FILTER (WHERE ");
                out.push_str(&reconstruct_expr_sql(f));
                out.push(')');
            }
            out
        }
        Expr::UnaryOp {
            op, expr: inner, ..
        } => {
            let inner_sql = reconstruct_expr_sql(inner);
            match op {
                aiondb_parser::UnaryOperator::Not => format!("NOT ({inner_sql})"),
                aiondb_parser::UnaryOperator::Minus => format!("-{inner_sql}"),
                aiondb_parser::UnaryOperator::BitwiseNot => format!("~{inner_sql}"),
                aiondb_parser::UnaryOperator::Abs => format!("@{inner_sql}"),
                aiondb_parser::UnaryOperator::SquareRoot => format!("|/{inner_sql}"),
                aiondb_parser::UnaryOperator::CubeRoot => format!("||/{inner_sql}"),
            }
        }
        Expr::BinaryOp {
            left, op, right, ..
        } => format!(
            "({} {} {})",
            reconstruct_expr_sql(left),
            reconstruct_binop_sql(op),
            reconstruct_expr_sql(right)
        ),
        Expr::IsNull {
            expr: inner,
            negated,
            ..
        } => {
            if *negated {
                format!("{} IS NOT NULL", reconstruct_expr_sql(inner))
            } else {
                format!("{} IS NULL", reconstruct_expr_sql(inner))
            }
        }
        Expr::IsDistinctFrom {
            left,
            right,
            negated,
            ..
        } => {
            if *negated {
                format!(
                    "{} IS NOT DISTINCT FROM {}",
                    reconstruct_expr_sql(left),
                    reconstruct_expr_sql(right)
                )
            } else {
                format!(
                    "{} IS DISTINCT FROM {}",
                    reconstruct_expr_sql(left),
                    reconstruct_expr_sql(right)
                )
            }
        }
        Expr::Like {
            expr: inner,
            pattern,
            negated,
            case_insensitive,
            ..
        } => {
            let op = if *case_insensitive { "ILIKE" } else { "LIKE" };
            if *negated {
                format!(
                    "{} NOT {} {}",
                    reconstruct_expr_sql(inner),
                    op,
                    reconstruct_expr_sql(pattern)
                )
            } else {
                format!(
                    "{} {} {}",
                    reconstruct_expr_sql(inner),
                    op,
                    reconstruct_expr_sql(pattern)
                )
            }
        }
        Expr::InList {
            expr: inner,
            list,
            negated,
            ..
        } => {
            let list_sql = list
                .iter()
                .map(reconstruct_expr_sql)
                .collect::<Vec<_>>()
                .join(", ");
            if *negated {
                format!("{} NOT IN ({list_sql})", reconstruct_expr_sql(inner))
            } else {
                format!("{} IN ({list_sql})", reconstruct_expr_sql(inner))
            }
        }
        Expr::Between {
            expr: inner,
            low,
            high,
            negated,
            ..
        } => {
            if *negated {
                format!(
                    "{} NOT BETWEEN {} AND {}",
                    reconstruct_expr_sql(inner),
                    reconstruct_expr_sql(low),
                    reconstruct_expr_sql(high)
                )
            } else {
                format!(
                    "{} BETWEEN {} AND {}",
                    reconstruct_expr_sql(inner),
                    reconstruct_expr_sql(low),
                    reconstruct_expr_sql(high)
                )
            }
        }
        Expr::Cast {
            expr: inner,
            data_type,
            ..
        } => format!("CAST({} AS {})", reconstruct_expr_sql(inner), data_type),
        Expr::CaseWhen {
            operand,
            conditions,
            results,
            else_result,
            ..
        } => {
            let mut out = String::from("CASE");
            if let Some(op) = operand {
                out.push(' ');
                out.push_str(&reconstruct_expr_sql(op));
            }
            for (condition, result) in conditions.iter().zip(results.iter()) {
                out.push_str(" WHEN ");
                out.push_str(&reconstruct_expr_sql(condition));
                out.push_str(" THEN ");
                out.push_str(&reconstruct_expr_sql(result));
            }
            if let Some(el) = else_result {
                out.push_str(" ELSE ");
                out.push_str(&reconstruct_expr_sql(el));
            }
            out.push_str(" END");
            out
        }
        Expr::Array { elements, .. } => {
            let elements = elements
                .iter()
                .map(reconstruct_expr_sql)
                .collect::<Vec<_>>()
                .join(", ");
            format!("ARRAY[{elements}]")
        }
        Expr::ArraySubquery { query, .. } => format!("ARRAY({})", reconstruct_select_sql(query)),
        Expr::Subquery { query, .. } => format!("({})", reconstruct_select_sql(query)),
        Expr::InSubquery {
            expr: inner,
            query,
            negated,
            ..
        } => {
            if *negated {
                format!(
                    "{} NOT IN ({})",
                    reconstruct_expr_sql(inner),
                    reconstruct_select_sql(query)
                )
            } else {
                format!(
                    "{} IN ({})",
                    reconstruct_expr_sql(inner),
                    reconstruct_select_sql(query)
                )
            }
        }
        Expr::Exists { query, negated, .. } => {
            if *negated {
                format!("NOT EXISTS ({})", reconstruct_select_sql(query))
            } else {
                format!("EXISTS ({})", reconstruct_select_sql(query))
            }
        }
        Expr::CypherExists { negated, .. } => {
            if *negated {
                "NOT EXISTS { ... }".to_owned()
            } else {
                "EXISTS { ... }".to_owned()
            }
        }
        Expr::CypherPatternComprehension { .. } => "[... | ...]".to_owned(),
        Expr::WindowFunction {
            function,
            partition_by,
            order_by,
            window_name,
            ..
        } => {
            let mut over_parts = Vec::new();
            if !partition_by.is_empty() {
                over_parts.push(format!(
                    "PARTITION BY {}",
                    partition_by
                        .iter()
                        .map(reconstruct_expr_sql)
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            if !order_by.is_empty() {
                over_parts.push(format!(
                    "ORDER BY {}",
                    order_by
                        .iter()
                        .map(|item| {
                            let mut rendered = reconstruct_expr_sql(&item.expr);
                            if item.descending {
                                rendered.push_str(" DESC");
                            }
                            match item.nulls_first {
                                Some(true) => rendered.push_str(" NULLS FIRST"),
                                Some(false) => rendered.push_str(" NULLS LAST"),
                                None => {}
                            }
                            rendered
                        })
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            if let Some(name) = window_name {
                over_parts.push(name.clone());
            }
            if over_parts.is_empty() {
                format!("{} OVER ()", reconstruct_expr_sql(function))
            } else {
                format!(
                    "{} OVER ({})",
                    reconstruct_expr_sql(function),
                    over_parts.join(" ")
                )
            }
        }
    }
}

fn reconstruct_select_sql(select: &aiondb_parser::SelectStatement) -> String {
    let mut sql = String::new();
    sql.push_str("SELECT ");

    match &select.distinct {
        aiondb_parser::DistinctKind::All => {}
        aiondb_parser::DistinctKind::Distinct => sql.push_str("DISTINCT "),
        aiondb_parser::DistinctKind::DistinctOn(exprs) => {
            sql.push_str("DISTINCT ON (");
            sql.push_str(
                &exprs
                    .iter()
                    .map(reconstruct_expr_sql)
                    .collect::<Vec<_>>()
                    .join(", "),
            );
            sql.push_str(") ");
        }
    }

    if select.items.is_empty() {
        sql.push('*');
    } else {
        let items = select
            .items
            .iter()
            .map(reconstruct_select_item_sql)
            .collect::<Vec<_>>()
            .join(", ");
        sql.push_str(&items);
    }

    if let Some(from) = &select.from {
        sql.push_str(" FROM ");
        sql.push_str(&quote_object_name(from));
        if let Some(alias) = &select.from_alias {
            sql.push(' ');
            sql.push_str(alias);
        }
    }

    for join in &select.joins {
        sql.push(' ');
        sql.push_str(&reconstruct_join_sql(join));
    }

    if let Some(selection) = &select.selection {
        sql.push_str(" WHERE ");
        sql.push_str(&reconstruct_expr_sql(selection));
    }

    if !select.group_by.is_empty() {
        sql.push_str(" GROUP BY ");
        sql.push_str(
            &select
                .group_by
                .iter()
                .map(reconstruct_expr_sql)
                .collect::<Vec<_>>()
                .join(", "),
        );
    }

    if let Some(having) = &select.having {
        sql.push_str(" HAVING ");
        sql.push_str(&reconstruct_expr_sql(having));
    }

    if !select.order_by.is_empty() {
        sql.push_str(" ORDER BY ");
        sql.push_str(
            &select
                .order_by
                .iter()
                .map(|item| {
                    let mut rendered = reconstruct_expr_sql(&item.expr);
                    if item.descending {
                        rendered.push_str(" DESC");
                    }
                    match item.nulls_first {
                        Some(true) => rendered.push_str(" NULLS FIRST"),
                        Some(false) => rendered.push_str(" NULLS LAST"),
                        None => {}
                    }
                    rendered
                })
                .collect::<Vec<_>>()
                .join(", "),
        );
    }

    if let Some(limit) = &select.limit {
        sql.push_str(" LIMIT ");
        sql.push_str(&reconstruct_expr_sql(limit));
    }

    if let Some(offset) = &select.offset {
        sql.push_str(" OFFSET ");
        sql.push_str(&reconstruct_expr_sql(offset));
    }

    sql
}

fn reconstruct_select_item_sql(item: &aiondb_parser::SelectItem) -> String {
    let mut sql = reconstruct_expr_sql(&item.expr);
    if let Some(alias) = &item.alias {
        sql.push_str(" AS ");
        sql.push_str(alias);
    }
    sql
}

fn reconstruct_join_sql(join: &aiondb_parser::JoinClause) -> String {
    let mut sql = String::new();
    let join_keyword = match join.join_type {
        aiondb_parser::ast::JoinType::Inner => "JOIN",
        aiondb_parser::ast::JoinType::Left => "LEFT JOIN",
        aiondb_parser::ast::JoinType::Right => "RIGHT JOIN",
        aiondb_parser::ast::JoinType::Full => "FULL JOIN",
        aiondb_parser::ast::JoinType::Cross => "CROSS JOIN",
    };
    if join.natural {
        sql.push_str("NATURAL ");
    }
    sql.push_str(join_keyword);
    sql.push(' ');
    sql.push_str(&quote_object_name(&join.table));
    if let Some(alias) = &join.alias {
        sql.push(' ');
        sql.push_str(alias);
    }
    if let Some(condition) = &join.condition {
        sql.push_str(" ON ");
        sql.push_str(&reconstruct_expr_sql(condition));
    }
    if !join.using_columns.is_empty() {
        sql.push_str(" USING (");
        sql.push_str(&join.using_columns.join(", "));
        sql.push(')');
        if let Some(using_alias) = &join.using_alias {
            sql.push_str(" AS ");
            sql.push_str(using_alias);
        }
    }
    sql
}

fn reconstruct_binop_sql(op: &aiondb_parser::BinaryOperator) -> &'static str {
    use aiondb_parser::BinaryOperator;
    match op {
        BinaryOperator::Add => "+",
        BinaryOperator::Exp => "^",
        BinaryOperator::BitwiseAnd => "&",
        BinaryOperator::BitwiseOr => "|",
        BinaryOperator::BitwiseXor => "#",
        BinaryOperator::ShiftLeft => "<<",
        BinaryOperator::ShiftRight => ">>",
        BinaryOperator::Sub => "-",
        BinaryOperator::Mul => "*",
        BinaryOperator::Div => "/",
        BinaryOperator::Mod => "%",
        BinaryOperator::Eq => "=",
        BinaryOperator::Ne => "!=",
        BinaryOperator::Lt => "<",
        BinaryOperator::Le => "<=",
        BinaryOperator::Gt => ">",
        BinaryOperator::Ge => ">=",
        BinaryOperator::And => "AND",
        BinaryOperator::Or => "OR",
        BinaryOperator::Concat => "||",
        BinaryOperator::RegexMatch => "~",
        BinaryOperator::RegexMatchInsensitive => "~*",
        BinaryOperator::NotRegexMatch => "!~",
        BinaryOperator::NotRegexMatchInsensitive => "!~*",
        BinaryOperator::JsonGet => "->",
        BinaryOperator::JsonGetText => "->>",
        BinaryOperator::JsonPathGet => "#>",
        BinaryOperator::JsonPathGetText => "#>>",
        BinaryOperator::JsonContains => "@>",
        BinaryOperator::JsonContainedBy => "<@",
        BinaryOperator::JsonKeyExists => "?",
        BinaryOperator::JsonAnyKeyExists => "?|",
        BinaryOperator::JsonAllKeysExist => "?&",
        BinaryOperator::ArrayOverlap => "&&",
        BinaryOperator::FullTextSearch => "@@",
        BinaryOperator::JsonPathExists => "@?",
        BinaryOperator::GeometricEq => "~=",
        BinaryOperator::VectorL2Distance => "<->",
        BinaryOperator::VectorCosineDistance => "<=>",
        BinaryOperator::VectorNegativeInnerProduct => "<#>",
        BinaryOperator::VectorL1Distance => "<+>",
        BinaryOperator::VectorHammingDistance => "<~>",
        BinaryOperator::VectorJaccardDistance => "<%>",
    }
}

fn case_insensitive_replace_identifier(sql: &str, pattern: &str, replacement: &str) -> String {
    // Track string-literal / quoted-identifier / line-comment / block-comment
    // state and only replace `pattern` in true SQL token positions. A naive
    // `match_indices` collides with patterns that occur inside `'...'`
    // literals or `"..."` quoted identifiers, letting an attacker-crafted
    // INSERT value escape its enclosing literal during rule rewriting (audit
    // engine_compat F-CL-2).
    let bytes = sql.as_bytes();
    let pattern_lower_bytes = pattern.to_ascii_lowercase().into_bytes();
    let pat_len = pattern_lower_bytes.len();
    let mut result = String::with_capacity(sql.len());
    let mut last = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        let c = bytes[i];
        match c {
            b'\'' => {
                // Single-quoted string literal — skip until closing `'`,
                // honouring `''` doubled escape.
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == b'\'' {
                        if i + 1 < bytes.len() && bytes[i + 1] == b'\'' {
                            i += 2;
                            continue;
                        }
                        i += 1;
                        break;
                    }
                    i += 1;
                }
                continue;
            }
            b'"' => {
                // Quoted identifier — skip until closing `"`, honouring `""`.
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == b'"' {
                        if i + 1 < bytes.len() && bytes[i + 1] == b'"' {
                            i += 2;
                            continue;
                        }
                        i += 1;
                        break;
                    }
                    i += 1;
                }
                continue;
            }
            b'-' if i + 1 < bytes.len() && bytes[i + 1] == b'-' => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'*' => {
                i += 2;
                while i + 1 < bytes.len() {
                    if bytes[i] == b'*' && bytes[i + 1] == b'/' {
                        i += 2;
                        break;
                    }
                    i += 1;
                }
                continue;
            }
            _ => {}
        }
        if i + pat_len <= bytes.len()
            && bytes[i..i + pat_len].eq_ignore_ascii_case(&pattern_lower_bytes)
        {
            let before_ok =
                i == 0 || !(bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_');
            let after = i + pat_len;
            let after_ok = after >= bytes.len()
                || !(bytes[after].is_ascii_alphanumeric() || bytes[after] == b'_');
            if before_ok && after_ok {
                result.push_str(&sql[last..i]);
                result.push_str(replacement);
                last = after;
                i = after;
                continue;
            }
        }
        i += 1;
    }
    result.push_str(&sql[last..]);
    result
}

fn find_top_level_where(upper_sql: &str) -> Option<usize> {
    find_keyword_not_in_parens(upper_sql, "WHERE")
}

fn find_keyword_not_in_parens(upper_sql: &str, keyword: &str) -> Option<usize> {
    // Walk `upper_sql` and skip over string literals, quoted identifiers, and
    // line/block comments before testing for `keyword`, so a kw embedded in a
    // `'...'` literal does not falsely match (audit engine_compat F-CL-4).
    let bytes = upper_sql.as_bytes();
    let kw_bytes = keyword.as_bytes();
    let kw_len = kw_bytes.len();
    if bytes.len() < kw_len {
        return None;
    }
    let mut depth = 0i32;
    let mut i = 0usize;
    while i + kw_len <= bytes.len() {
        let c = bytes[i];
        match c {
            b'(' => {
                depth += 1;
                i += 1;
                continue;
            }
            b')' => {
                depth -= 1;
                i += 1;
                continue;
            }
            b'\'' => {
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == b'\'' {
                        if i + 1 < bytes.len() && bytes[i + 1] == b'\'' {
                            i += 2;
                            continue;
                        }
                        i += 1;
                        break;
                    }
                    i += 1;
                }
                continue;
            }
            b'"' => {
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == b'"' {
                        if i + 1 < bytes.len() && bytes[i + 1] == b'"' {
                            i += 2;
                            continue;
                        }
                        i += 1;
                        break;
                    }
                    i += 1;
                }
                continue;
            }
            b'-' if i + 1 < bytes.len() && bytes[i + 1] == b'-' => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'*' => {
                i += 2;
                while i + 1 < bytes.len() {
                    if bytes[i] == b'*' && bytes[i + 1] == b'/' {
                        i += 2;
                        break;
                    }
                    i += 1;
                }
                continue;
            }
            _ => {}
        }
        if depth == 0 && bytes[i..i + kw_len].eq_ignore_ascii_case(kw_bytes) {
            let before_ok = i == 0 || !bytes[i - 1].is_ascii_alphanumeric();
            let after_ok = i + kw_len >= bytes.len() || !bytes[i + kw_len].is_ascii_alphanumeric();
            if before_ok && after_ok {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

// parse_compat_execute and substitute_prepared_params moved to session_compat.rs
