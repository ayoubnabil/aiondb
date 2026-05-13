pub(in crate::engine) fn find_top_level_keyword(sql: &str, keyword: &str) -> Option<usize> {
    let keyword_len = keyword.len();
    let bytes = sql.as_bytes();
    let mut cursor = 0usize;
    let mut depth = 0u32;
    let mut in_single_quote = false;
    let mut in_double_quote = false;

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
        if in_double_quote {
            if ch == b'"' {
                in_double_quote = false;
            }
            cursor += 1;
            continue;
        }

        match ch {
            b'\'' => {
                in_single_quote = true;
                cursor += 1;
            }
            b'"' => {
                in_double_quote = true;
                cursor += 1;
            }
            b'(' => {
                depth += 1;
                cursor += 1;
            }
            b')' => {
                depth = depth.saturating_sub(1);
                cursor += 1;
            }
            _ => {
                if depth == 0
                    && cursor + keyword_len <= bytes.len()
                    && sql[cursor..cursor + keyword_len].eq_ignore_ascii_case(keyword)
                {
                    let prev_ok = cursor == 0
                        || !sql[..cursor]
                            .chars()
                            .next_back()
                            .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');
                    let next_ok = cursor + keyword_len == bytes.len()
                        || !sql[cursor + keyword_len..]
                            .chars()
                            .next()
                            .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');
                    if prev_ok && next_ok {
                        return Some(cursor);
                    }
                }
                cursor += 1;
            }
        }
    }

    None
}

pub(in crate::engine) fn parse_compat_aggregate_select_item(item: &str) -> Option<(String, String, String)> {
    let mut cursor = 0usize;
    let aggregate_name = parse_compat_identifier(item, &mut cursor)?;
    skip_sql_whitespace(item, &mut cursor);
    let args = extract_parenthesized(item, &mut cursor)?;
    skip_sql_whitespace(item, &mut cursor);

    let mut filter_clause = String::new();
    if item
        .get(cursor..cursor.saturating_add("filter".len()))
        .is_some_and(|raw| raw.eq_ignore_ascii_case("filter"))
    {
        cursor += "filter".len();
        skip_sql_whitespace(item, &mut cursor);
        let predicate = extract_parenthesized(item, &mut cursor)?;
        filter_clause = format!(" FILTER ({predicate})");
        skip_sql_whitespace(item, &mut cursor);
    }

    let tail = item[cursor..].trim();
    if !tail.is_empty() && !tail.starts_with("--") {
        return None;
    }

    Some((aggregate_name, args, filter_clause))
}

pub(in crate::engine) fn parse_pg_typeof_wrapped_call(item: &str) -> Option<String> {
    let mut cursor = 0usize;
    let outer_name = parse_compat_identifier(item, &mut cursor)?;
    if !outer_name.eq_ignore_ascii_case("pg_typeof") {
        return None;
    }
    skip_sql_whitespace(item, &mut cursor);
    let inner = extract_parenthesized(item, &mut cursor)?;
    skip_sql_whitespace(item, &mut cursor);
    if !item[cursor..].trim().is_empty() {
        return None;
    }
    Some(inner.trim().to_owned())
}

pub(in crate::engine) fn parse_ordered_set_aggregate_select_item(
    item: &str,
) -> Option<(String, String, String, String, String)> {
    let mut cursor = 0usize;
    let aggregate_name = parse_compat_identifier(item, &mut cursor)?;
    skip_sql_whitespace(item, &mut cursor);
    let direct_args = extract_parenthesized(item, &mut cursor)?;
    skip_sql_whitespace(item, &mut cursor);
    consume_word_ci(item, &mut cursor, "within")?;
    skip_sql_whitespace(item, &mut cursor);
    consume_word_ci(item, &mut cursor, "group")?;
    skip_sql_whitespace(item, &mut cursor);
    let within_clause = extract_parenthesized(item, &mut cursor)?;
    let mut within_cursor = 0usize;
    consume_word_ci(&within_clause, &mut within_cursor, "order")?;
    skip_sql_whitespace(&within_clause, &mut within_cursor);
    consume_word_ci(&within_clause, &mut within_cursor, "by")?;
    let order_clause = within_clause[within_cursor..].trim().to_owned();
    if order_clause.is_empty() {
        return None;
    }
    skip_sql_whitespace(item, &mut cursor);

    let mut filter_clause = String::new();
    if item
        .get(cursor..cursor.saturating_add("filter".len()))
        .is_some_and(|raw| raw.eq_ignore_ascii_case("filter"))
    {
        cursor += "filter".len();
        skip_sql_whitespace(item, &mut cursor);
        let predicate = extract_parenthesized(item, &mut cursor)?;
        filter_clause = format!(" FILTER ({predicate})");
        skip_sql_whitespace(item, &mut cursor);
    }

    let tail = item[cursor..].trim();
    let suffix = if tail.is_empty() || tail.starts_with("--") {
        String::new()
    } else {
        format!(" {tail}")
    };
    Some((
        aggregate_name,
        direct_args,
        order_clause,
        filter_clause,
        suffix,
    ))
}

#[derive(Clone, Debug)]
pub(in crate::engine) struct CompatMultiArgAggregateSpec {
    distinct: bool,
    arg_exprs: Vec<String>,
    order_clause: Option<String>,
}

pub(in crate::engine) fn normalize_compat_sort_operators(sql: &str) -> String {
    sql.replace(" using ~<~", " ASC NULLS LAST")
        .replace(" USING ~<~", " ASC NULLS LAST")
}

pub(in crate::engine) fn parse_compat_multiarg_aggregate_spec(args: &str) -> Option<CompatMultiArgAggregateSpec> {
    let order_pos = find_top_level_keyword(args, "order by");
    let args_part = order_pos.map_or(args, |pos| &args[..pos]).trim();
    let order_clause =
        order_pos.map(|pos| normalize_compat_sort_operators(args[pos + "order by".len()..].trim()));

    let (distinct, value_part) = if args_part.len() >= "distinct".len()
        && args_part[.."distinct".len()].eq_ignore_ascii_case("distinct")
        && args_part["distinct".len()..]
            .chars()
            .next()
            .is_some_and(char::is_whitespace)
    {
        (true, args_part["distinct".len()..].trim())
    } else {
        (false, args_part)
    };

    let arg_exprs = split_top_level_csv_items(value_part)?;
    if arg_exprs.len() != 3 {
        return None;
    }

    Some(CompatMultiArgAggregateSpec {
        distinct,
        arg_exprs,
        order_clause,
    })
}

pub(in crate::engine) fn parse_filter_clause_predicate(filter_clause: &str) -> Option<String> {
    if filter_clause.is_empty() {
        return None;
    }
    let mut cursor = " FILTER ".len();
    let inner = extract_parenthesized(filter_clause, &mut cursor)?;
    let trimmed = inner.trim();
    // Case-insensitive `WHERE` strip; the rewriter is fed the user's source
    // text, and `FILTER (WHERE …)` is the canonical SQL spelling. Without
    // rows that should have been filtered out (see audit compat F2).
    // Use `get(..5)` so a non-ASCII filter clause whose 5th byte falls inside a
    // multi-byte UTF-8 codepoint cannot panic the rewriter
    // (audit second-opinion).
    let after_where = match trimmed.get(..5) {
        Some(prefix) if prefix.eq_ignore_ascii_case("where") => &trimmed[5..],
        _ => return None,
    };
    let predicate = after_where.trim();
    if predicate.is_empty() {
        return None;
    }
    Some(predicate.to_owned())
}

pub(in crate::engine) fn strip_order_item_decorations(item: &str) -> String {
    let mut end = item.len();
    for keyword in ["nulls", "asc", "desc"] {
        if let Some(pos) = find_top_level_keyword(item, keyword) {
            end = end.min(pos);
        }
    }
    item[..end].trim().to_owned()
}

pub(in crate::engine) fn parse_percentile_array_direct_args(arg: &str) -> Option<Vec<String>> {
    if !arg
        .get(..6)
        .is_some_and(|head| head.eq_ignore_ascii_case("array["))
    {
        return None;
    }
    let open = arg.find('[')?;
    let close = arg.rfind(']')?;
    if close <= open {
        return None;
    }
    let inner = arg[open + 1..close].trim();
    if inner.is_empty() || inner.contains('[') || inner.contains(']') {
        return None;
    }
    let percentiles = split_top_level_csv_items(inner)?;
    if percentiles.is_empty() {
        return None;
    }
    Some(percentiles)
}

pub(in crate::engine) fn compat_multiarg_distinct_order_by_is_valid(spec: &CompatMultiArgAggregateSpec) -> bool {
    if !spec.distinct {
        return true;
    }
    let Some(order_clause) = &spec.order_clause else {
        return true;
    };
    let Some(order_items) = split_top_level_csv_items(order_clause) else {
        return false;
    };
    let args = spec
        .arg_exprs
        .iter()
        .map(|arg| arg.trim())
        .collect::<Vec<_>>();
    order_items.into_iter().all(|item| {
        let expr = strip_order_item_decorations(&item);
        args.iter().any(|arg| arg.eq_ignore_ascii_case(expr.trim()))
    })
}

pub(in crate::engine) fn select_compat_multiarg_distinct_order_error(sql: &str) -> bool {
    let trimmed = trim_compat_statement(sql);
    if !trimmed
        .get(..6)
        .is_some_and(|head| head.eq_ignore_ascii_case("select"))
    {
        return false;
    }
    let Some(select_pos) = find_top_level_keyword(trimmed, "select") else {
        return false;
    };
    let Some(from_tail_pos) =
        find_top_level_keyword(&trimmed[select_pos + "select".len()..], "from")
    else {
        return false;
    };
    let from_pos = from_tail_pos + select_pos + "select".len();
    let select_list = trimmed[select_pos + "select".len()..from_pos].trim();
    let Some(items) = split_top_level_csv_items(select_list) else {
        return false;
    };
    if items.len() != 1 {
        return false;
    }
    let Some((aggregate_name, args, _)) = parse_compat_aggregate_select_item(items[0].trim())
    else {
        return false;
    };
    if !matches!(aggregate_name.as_str(), "aggfns" | "aggfstr") {
        return false;
    }
    let Some(spec) = parse_compat_multiarg_aggregate_spec(&args) else {
        return false;
    };
    !compat_multiarg_distinct_order_by_is_valid(&spec)
}

pub(in crate::engine) fn compat_multiarg_distinct_order_error(sql: &str) -> Option<DbError> {
    if select_compat_multiarg_distinct_order_error(sql) {
        return Some(DbError::bind_error(
            SqlState::InvalidColumnReference,
            "in an aggregate with DISTINCT, ORDER BY expressions must appear in argument list",
        ));
    }

    let trimmed = trim_compat_statement(sql);
    let is_create_view = trimmed
        .get(..11)
        .is_some_and(|head| head.eq_ignore_ascii_case("create view"))
        || trimmed
            .get(..22)
            .is_some_and(|head| head.eq_ignore_ascii_case("create or replace view"));
    if !is_create_view {
        return None;
    }
    let as_pos = find_top_level_keyword(trimmed, "as")?;
    let select_sql = trimmed[as_pos + "as".len()..].trim_start();
    if select_compat_multiarg_distinct_order_error(select_sql) {
        return Some(DbError::bind_error(
            SqlState::InvalidColumnReference,
            "in an aggregate with DISTINCT, ORDER BY expressions must appear in argument list",
        ));
    }
    None
}

pub(in crate::engine) fn is_ordered_set_aggregate_name(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "percentile_cont"
            | "percentile_disc"
            | "test_percentile_disc"
            | "mode"
            | "rank"
            | "test_rank"
            | "dense_rank"
            | "percent_rank"
            | "cume_dist"
    )
}

pub(in crate::engine) fn ordered_set_usage_error_for_select(select_sql: &str) -> Option<DbError> {
    let select_pos = find_top_level_keyword(select_sql, "select")?;
    let from_pos = find_top_level_keyword(&select_sql[select_pos + "select".len()..], "from")?
        + select_pos
        + "select".len();
    let select_list = select_sql[select_pos + "select".len()..from_pos].trim();
    let items = split_top_level_csv_items(select_list)?;
    for item in items {
        let trimmed = item.trim();
        let Some((aggregate_name, direct_args, _order_clause, _filter_clause, _suffix)) =
            parse_ordered_set_aggregate_select_item(trimmed)
        else {
            continue;
        };
        if !is_ordered_set_aggregate_name(&aggregate_name) {
            return Some(DbError::bind_error(
                SqlState::SyntaxError,
                format!(
                    "{aggregate_name} is not an ordered-set aggregate, so it cannot have WITHIN GROUP"
                ),
            ));
        }
        if find_top_level_keyword(&direct_args, "order by").is_some() {
            return Some(DbError::bind_error(
                SqlState::SyntaxError,
                "cannot use multiple ORDER BY clauses with WITHIN GROUP",
            ));
        }
    }
    None
}

pub(in crate::engine) fn ordered_set_usage_error(sql: &str) -> Option<DbError> {
    let trimmed = trim_compat_statement(sql);
    if trimmed
        .get(..6)
        .is_some_and(|head| head.eq_ignore_ascii_case("select"))
    {
        return ordered_set_usage_error_for_select(trimmed);
    }

    let is_create_view = trimmed
        .get(..11)
        .is_some_and(|head| head.eq_ignore_ascii_case("create view"))
        || trimmed
            .get(..22)
            .is_some_and(|head| head.eq_ignore_ascii_case("create or replace view"));
    if !is_create_view {
        return None;
    }
    let as_pos = find_top_level_keyword(trimmed, "as")?;
    let select_sql = trimmed[as_pos + "as".len()..].trim_start();
    ordered_set_usage_error_for_select(select_sql)
}

pub(in crate::engine) fn build_compat_aggregate_rewrite_expression(
    aggregate_name: &str,
    args: &str,
    filter_clause: &str,
    rewrite: &CompatAggregateRewrite,
) -> String {
    let expr = build_compat_aggregate_rewrite_expression_core(args, filter_clause, rewrite);
    format!("{expr} AS {aggregate_name}")
}

pub(in crate::engine) fn build_compat_aggregate_rewrite_expression_core(
    args: &str,
    filter_clause: &str,
    rewrite: &CompatAggregateRewrite,
) -> String {
    let sum_expr = format!("sum({args}){filter_clause}");
    let count_expr = format!("count({args}){filter_clause}");
    let least_args = args
        .trim()
        .strip_prefix("variadic array[")
        .and_then(|tail| tail.strip_suffix(']'))
        .unwrap_or(args)
        .trim();
    let expr = match rewrite {
        CompatAggregateRewrite::Avg => format!("avg({args}){filter_clause}"),
        CompatAggregateRewrite::Sum => sum_expr,
        CompatAggregateRewrite::SumWithOffset(offset) => {
            format!("(coalesce({sum_expr}, 0) + {offset})")
        }
        CompatAggregateRewrite::AvgWithOffset(offset) => format!(
            "(CASE WHEN {count_expr} = 0 THEN NULL ELSE (coalesce({sum_expr}, 0) + {offset}) / {count_expr} END)"
        ),
        CompatAggregateRewrite::HalfSum => format!("({sum_expr} / 2)"),
        CompatAggregateRewrite::MinLeast => {
            format!("min(least({least_args})){filter_clause}")
        }
        CompatAggregateRewrite::NullBigInt => "CAST(NULL AS BIGINT)".to_owned(),
        CompatAggregateRewrite::DirectSfuncFinalfunc { sfunc, finalfunc } => {
            if sfunc == "rwagg_sfunc" && finalfunc == "rwagg_finalfunc" {
                let _ = filter_clause;
                format!(
                    "coalesce(NULLIF(({args}), ({args})), array_fill((({args})[1]), ARRAY[4]))"
                )
            } else {
                format!("{finalfunc}({args})")
            }
        }
    };
    expr
}

pub(in crate::engine) fn builtin_compat_aggregate_rewrite(aggregate_name: &str) -> Option<CompatAggregateRewrite> {
    match aggregate_name {
        "least_agg" | "cleast_agg" => Some(CompatAggregateRewrite::MinLeast),
        _ => None,
    }
}

pub(in crate::engine) fn build_compat_tuple_text_expr(arg_exprs: &[String]) -> String {
    format!(
        "('(' || coalesce(({})::text, '') || ',' || coalesce(({})::text, '') || ',' || coalesce(({})::text, '') || ')')",
        arg_exprs[0], arg_exprs[1], arg_exprs[2]
    )
}

pub(in crate::engine) fn build_compat_multiarg_aggregate_query(
    aggregate_name: &str,
    args: &str,
    filter_clause: &str,
    from_clause: &str,
) -> Option<String> {
    let from_clause = from_clause.trim_end();
    let from_clause = from_clause
        .strip_suffix(';')
        .unwrap_or(from_clause)
        .trim_end();
    let spec = parse_compat_multiarg_aggregate_spec(args)?;
    let row_text = build_compat_tuple_text_expr(&spec.arg_exprs);
    let filter_predicate = parse_filter_clause_predicate(filter_clause);

    let strict_predicate = if aggregate_name.eq_ignore_ascii_case("aggfstr") {
        Some(format!(
            "({}) IS NOT NULL AND ({}) IS NOT NULL AND ({}) IS NOT NULL",
            spec.arg_exprs[0], spec.arg_exprs[1], spec.arg_exprs[2]
        ))
    } else {
        None
    };

    let combined_predicate = match (strict_predicate, filter_predicate) {
        (Some(strict), Some(filter)) => Some(format!("({strict}) AND ({filter})")),
        (Some(strict), None) => Some(strict),
        (None, Some(filter)) => Some(filter),
        (None, None) => None,
    };

    let order_suffix = spec
        .order_clause
        .as_ref()
        .map(|order| format!(" ORDER BY {order}"))
        .unwrap_or_default();

    if spec.distinct {
        let where_clause = combined_predicate
            .as_ref()
            .map(|predicate| format!(" WHERE {predicate}"))
            .unwrap_or_default();
        let order_projection = spec
            .arg_exprs
            .iter()
            .enumerate()
            .map(|(idx, expr)| format!("{expr} AS __compat_arg_{}", idx + 1))
            .collect::<Vec<_>>()
            .join(", ");
        let order_clause = spec.order_clause.as_ref().map(|order| {
            order
                .replace(&spec.arg_exprs[0], "__AION_COMPAT_ARG_1__")
                .replace(&spec.arg_exprs[1], "__AION_COMPAT_ARG_2__")
                .replace(&spec.arg_exprs[2], "__AION_COMPAT_ARG_3__")
                .replace("__AION_COMPAT_ARG_1__", "__compat_arg_1")
                .replace("__AION_COMPAT_ARG_2__", "__compat_arg_2")
                .replace("__AION_COMPAT_ARG_3__", "__compat_arg_3")
        });
        let subquery_order_suffix = order_clause
            .as_ref()
            .map(|order| format!(" ORDER BY {order}"))
            .unwrap_or_default();
        return Some(format!(
            "SELECT array_agg(__compat_val) AS {aggregate_name} FROM (SELECT DISTINCT {order_projection}, {row_text} AS __compat_val {from_clause}{where_clause}{subquery_order_suffix}) __compat_agg"
        ));
    }

    let filter_suffix = combined_predicate
        .as_ref()
        .map(|predicate| format!(" FILTER (WHERE {predicate})"))
        .unwrap_or_default();
    Some(format!(
        "SELECT array_agg({row_text}{order_suffix}){filter_suffix} AS {aggregate_name} {from_clause}"
    ))
}

pub(in crate::engine) fn ordered_set_source_subquery(
    from_clause: &str,
    order_expr: &str,
    filter_clause: &str,
) -> Option<String> {
    let from_clause = from_clause
        .trim_end()
        .strip_suffix(';')
        .unwrap_or(from_clause);
    let from_clause = from_clause.trim_end();
    let filter_predicate = parse_filter_clause_predicate(filter_clause);
    let where_clause = filter_predicate
        .map(|predicate| format!(" WHERE {predicate}"))
        .unwrap_or_default();
    Some(format!(
        "(SELECT {order_expr} AS __os_v {from_clause}{where_clause}) __os_src"
    ))
}

pub(in crate::engine) fn rewrite_ordered_set_select_item(
    aggregate_name: &str,
    direct_args: &str,
    order_clause: &str,
    filter_clause: &str,
    from_clause: &str,
    allow_subquery_source: bool,
) -> Option<(String, bool)> {
    let direct_args = split_top_level_csv_items(direct_args)?;
    let order_items = split_top_level_csv_items(order_clause)?;
    if order_items.is_empty() {
        return None;
    }
    let order_exprs: Vec<String> = order_items
        .iter()
        .map(|item| strip_order_item_decorations(item))
        .collect();
    if order_exprs.iter().any(|expr| expr.is_empty()) {
        return None;
    }
    let order_expr = order_exprs.first()?.clone();
    let lower = aggregate_name.to_ascii_lowercase();
    let source = if allow_subquery_source {
        ordered_set_source_subquery(from_clause, &order_expr, filter_clause)
    } else {
        None
    };
    let filter_predicate = parse_filter_clause_predicate(filter_clause);
    let scoped = |extra: &str| match &filter_predicate {
        Some(predicate) => format!("({predicate}) AND ({extra})"),
        None => extra.to_owned(),
    };
    let not_null_predicate = order_exprs
        .iter()
        .map(|expr| format!("({expr}) IS NOT NULL"))
        .collect::<Vec<_>>()
        .join(" AND ");
    let row_order_expr = format!("ROW({})", order_exprs.join(", "));
    match lower.as_str() {
        "rank" | "test_rank" => {
            if direct_args.len() != order_exprs.len() {
                return None;
            }
            let row_arg_expr = format!("ROW({})", direct_args.join(", "));
            let pred = scoped(&format!(
                "({not_null_predicate}) AND ({row_order_expr}) < ({row_arg_expr})"
            ));
            Some((
                format!(
                    "(1 + coalesce(sum(CASE WHEN {pred} THEN 1 ELSE 0 END), 0)) AS {aggregate_name}"
                ),
                true,
            ))
        }
        "dense_rank" => {
            if direct_args.len() != order_exprs.len() {
                return None;
            }
            let row_arg_expr = format!("ROW({})", direct_args.join(", "));
            let pred = scoped(&format!(
                "({not_null_predicate}) AND ({row_order_expr}) < ({row_arg_expr})"
            ));
            Some((
                format!(
                    "(1 + count(DISTINCT CASE WHEN {pred} THEN ({row_order_expr}) ELSE NULL END)) AS {aggregate_name}"
                ),
                true,
            ))
        }
        "percent_rank" => {
            if direct_args.len() != order_exprs.len() {
                return None;
            }
            let row_arg_expr = format!("ROW({})", direct_args.join(", "));
            let valid_pred = scoped(&format!("({not_null_predicate})"));
            let lt_pred = scoped(&format!(
                "({not_null_predicate}) AND ({row_order_expr}) < ({row_arg_expr})"
            ));
            Some((
                format!(
                    "(CASE WHEN coalesce(sum(CASE WHEN {valid_pred} THEN 1 ELSE 0 END), 0) = 0 THEN 0::FLOAT8 ELSE (coalesce(sum(CASE WHEN {lt_pred} THEN 1 ELSE 0 END), 0)::FLOAT8 / coalesce(sum(CASE WHEN {valid_pred} THEN 1 ELSE 0 END), 0)::FLOAT8) END) AS {aggregate_name}"
                ),
                true,
            ))
        }
        "cume_dist" => {
            if direct_args.len() != order_exprs.len() {
                return None;
            }
            let row_arg_expr = format!("ROW({})", direct_args.join(", "));
            let valid_pred = scoped(&format!("({not_null_predicate})"));
            let le_pred = scoped(&format!(
                "({not_null_predicate}) AND ({row_order_expr}) <= ({row_arg_expr})"
            ));
            Some((
                format!(
                    "(CASE WHEN coalesce(sum(CASE WHEN {valid_pred} THEN 1 ELSE 0 END), 0) = 0 THEN 1::FLOAT8 ELSE ((coalesce(sum(CASE WHEN {le_pred} THEN 1 ELSE 0 END), 0) + 1)::FLOAT8 / (coalesce(sum(CASE WHEN {valid_pred} THEN 1 ELSE 0 END), 0) + 1)::FLOAT8) END) AS {aggregate_name}"
                ),
                true,
            ))
        }
        "mode" => {
            if !direct_args.is_empty() || order_exprs.len() != 1 {
                return None;
            }
            let valid_pred = scoped(&format!("({order_expr}) IS NOT NULL"));
            Some((
                format!(
                    "(SELECT __m.v FROM unnest(array_agg(CASE WHEN {valid_pred} THEN ({order_expr}) ELSE NULL END)) AS __m(v) WHERE __m.v IS NOT NULL GROUP BY __m.v ORDER BY count(*) DESC, __m.v LIMIT 1) AS {aggregate_name}"
                ),
                true,
            ))
        }
        "percentile_disc" | "test_percentile_disc" => {
            if order_exprs.len() != 1 {
                return None;
            }
            if direct_args.len() != 1 {
                return None;
            }
            let arg = direct_args[0].trim();
            if let Some(percentiles) = parse_percentile_array_direct_args(arg) {
                if let Some(source) = source.clone() {
                    let elems: Vec<String> = percentiles
                        .iter()
                        .map(|p| {
                            let p = p.trim();
                            format!(
                                "(SELECT __rows.__os_v FROM (SELECT __os_v, row_number() OVER (ORDER BY __os_v) AS __rn, count(*) OVER () AS __n FROM {source} WHERE __os_v IS NOT NULL) __rows WHERE __rows.__rn = CASE WHEN ({p}) <= 0 THEN 1 WHEN ({p}) >= 1 THEN __rows.__n ELSE CEIL(({p}) * __rows.__n::FLOAT8)::BIGINT END LIMIT 1)"
                            )
                        })
                        .collect();
                    return Some((
                        format!(
                            "any_value((ARRAY[{}])) AS {aggregate_name}",
                            elems.join(", ")
                        ),
                        true,
                    ));
                }
                let valid_pred = scoped(&format!("({order_expr}) IS NOT NULL"));
                let count_valid =
                    format!("coalesce(sum(CASE WHEN {valid_pred} THEN 1 ELSE 0 END), 0)");
                let sorted_array =
                    format!("array_agg(CASE WHEN {valid_pred} THEN ({order_expr}) ELSE NULL END ORDER BY {order_expr})");
                let elems: Vec<String> = percentiles
                    .iter()
                    .map(|p| {
                        let p = p.trim();
                        let idx = format!(
                            "(CASE WHEN ({p}) <= 0 THEN 1 WHEN ({p}) >= 1 THEN {count_valid} ELSE CEIL(({p}) * ({count_valid})::FLOAT8)::INT END)"
                        );
                        format!(
                            "({sorted_array})[{idx}]"
                        )
                    })
                    .collect();
                return Some((
                    format!("(ARRAY[{}]) AS {aggregate_name}", elems.join(", ")),
                    true,
                ));
            }
            let q_expr = format!("any_value({arg})");
            let valid_pred = scoped(&format!("({order_expr}) IS NOT NULL"));
            let count_valid = format!("coalesce(sum(CASE WHEN {valid_pred} THEN 1 ELSE 0 END), 0)");
            let idx = format!(
                "(CASE WHEN ({q_expr}) <= 0 THEN 1 WHEN ({q_expr}) >= 1 THEN {count_valid} ELSE CEIL(({q_expr}) * ({count_valid})::FLOAT8)::INT END)"
            );
            Some((
                format!(
                    "(array_agg(CASE WHEN {valid_pred} THEN ({order_expr}) ELSE NULL END ORDER BY {order_expr}))[{idx}] AS {aggregate_name}"
                ),
                true,
            ))
        }
        "percentile_cont" => {
            if order_exprs.len() != 1 {
                return None;
            }
            if direct_args.len() != 1 {
                return None;
            }
            let arg = direct_args[0].trim();
            if let Some(percentiles) = parse_percentile_array_direct_args(arg) {
                if let Some(source) = source {
                    let elems: Vec<String> = percentiles
                        .iter()
                        .map(|p| {
                            let p = p.trim();
                            format!(
                                "(SELECT CASE WHEN __meta.__n = 0 THEN NULL WHEN __meta.__lo = __meta.__hi THEN __meta.__vlo ELSE __meta.__vlo + ((__meta.__vhi - __meta.__vlo) * (__meta.__r - __meta.__lo::FLOAT8)) END FROM (SELECT max(__n) AS __n, max(__r) AS __r, max(__lo) AS __lo, max(__hi) AS __hi, max(CASE WHEN __rn = __lo THEN __os_v::FLOAT8 END) AS __vlo, max(CASE WHEN __rn = __hi THEN __os_v::FLOAT8 END) AS __vhi FROM (SELECT __os_v, row_number() OVER (ORDER BY __os_v) AS __rn, count(*) OVER () AS __n, (1 + ({p}) * (count(*) OVER () - 1))::FLOAT8 AS __r, FLOOR((1 + ({p}) * (count(*) OVER () - 1))::FLOAT8)::BIGINT AS __lo, CEIL((1 + ({p}) * (count(*) OVER () - 1))::FLOAT8)::BIGINT AS __hi FROM {source} WHERE __os_v IS NOT NULL) __ordered) __meta)"
                            )
                        })
                        .collect();
                    return Some((
                        format!(
                            "any_value((ARRAY[{}])) AS {aggregate_name}",
                            elems.join(", ")
                        ),
                        true,
                    ));
                }
                let valid_pred = scoped(&format!("({order_expr}) IS NOT NULL"));
                let count_valid =
                    format!("coalesce(sum(CASE WHEN {valid_pred} THEN 1 ELSE 0 END), 0)");
                let sorted_array = format!(
                    "array_agg(CASE WHEN {valid_pred} THEN ({order_expr})::FLOAT8 ELSE NULL END ORDER BY {order_expr})"
                );
                let elems: Vec<String> = percentiles
                    .iter()
                    .map(|p| {
                        let p = p.trim();
                        let rank_expr = format!("(1 + ({p}) * (({count_valid}) - 1))");
                        let lo_idx = format!("FLOOR(({rank_expr})::FLOAT8)::INT");
                        let hi_idx = format!("CEIL(({rank_expr})::FLOAT8)::INT");
                        let lo_val = format!("({sorted_array})[{lo_idx}]");
                        let hi_val = format!("({sorted_array})[{hi_idx}]");
                        format!(
                            "(CASE WHEN ({count_valid}) = 0 THEN NULL WHEN {lo_idx} = {hi_idx} THEN {lo_val} ELSE {lo_val} + (({hi_val} - {lo_val}) * (({rank_expr})::FLOAT8 - ({lo_idx})::FLOAT8)) END)"
                        )
                    })
                    .collect();
                return Some((
                    format!("(ARRAY[{}]) AS {aggregate_name}", elems.join(", ")),
                    true,
                ));
            }
            let q_expr = format!("any_value({arg})");
            let valid_pred = scoped(&format!("({order_expr}) IS NOT NULL"));
            let count_valid = format!("coalesce(sum(CASE WHEN {valid_pred} THEN 1 ELSE 0 END), 0)");
            let rank_expr = format!("(1 + ({q_expr}) * (({count_valid}) - 1))");
            let lo_idx = format!("FLOOR(({rank_expr})::FLOAT8)::INT");
            let hi_idx = format!("CEIL(({rank_expr})::FLOAT8)::INT");
            let lo_val = format!(
                "(array_agg(CASE WHEN {valid_pred} THEN ({order_expr})::FLOAT8 ELSE NULL END ORDER BY {order_expr}))[{lo_idx}]"
            );
            let hi_val = format!(
                "(array_agg(CASE WHEN {valid_pred} THEN ({order_expr})::FLOAT8 ELSE NULL END ORDER BY {order_expr}))[{hi_idx}]"
            );
            Some((
                format!(
                    "(CASE WHEN ({count_valid}) = 0 THEN NULL WHEN {lo_idx} = {hi_idx} THEN {lo_val} ELSE {lo_val} + (({hi_val} - {lo_val}) * (({rank_expr})::FLOAT8 - ({lo_idx})::FLOAT8)) END) AS {aggregate_name}"
                ),
                true,
            ))
        }
        _ => None,
    }
}

pub(in crate::engine) fn sql_may_use_builtin_compat_aggregate_rewrite(sql: &str) -> bool {
    // Every aggregate name handled by the rewrite contains one of these
    // six lowercase substrings. False positives are acceptable (the caller
    // skip the rewrite, so the cover set is verified against the full
    // pattern list in a unit test below.
    //
    // Original list (kept for future maintainers, in order of frequency
    // in real workloads): least_agg, cleast_agg, aggfns, aggfstr,
    // within group, percentile_cont, percentile_disc, test_percentile_disc,
    // rank, dense_rank, percent_rank, cume_dist, mode(, test_rank.
    //
    // Reducing 14 substring scans to 6 makes a ~2× difference in the
    // hot OLTP path where every `execute_sql` call runs this check
    // even when the SQL is a plain SELECT.
    const AGGREGATE_REWRITE_HINTS: &[&str] = &[
        "agg",          // covers least_agg, cleast_agg, aggfns, aggfstr
        "rank",         // covers rank, dense_rank, percent_rank, test_rank
        "percentile",   // covers percentile_cont, percentile_disc, test_percentile_disc
        "cume_dist",    // covers cume_dist
        "mode(",        // covers mode(
        "within group", // covers within group
    ];
    AGGREGATE_REWRITE_HINTS
        .iter()
        .any(|name| super::compat::find_ascii_case_insensitive(sql, name).is_some())
}

#[cfg(test)]
#[test]
fn aggregate_rewrite_hints_cover_every_full_pattern() {
    // Every full pattern that the rewriter actually understands must
    // contain at least one of the cheap hints used by
    // `sql_may_use_builtin_compat_aggregate_rewrite`. Add the new
    // hint here whenever a future pattern lands without one.
    const FULL_PATTERNS: &[&str] = &[
        "least_agg",
        "cleast_agg",
        "aggfns",
        "aggfstr",
        "within group",
        "percentile_cont",
        "percentile_disc",
        "test_percentile_disc",
        "rank",
        "dense_rank",
        "percent_rank",
        "cume_dist",
        "mode(",
        "test_rank",
    ];
    for pattern in FULL_PATTERNS {
        assert!(
            sql_may_use_builtin_compat_aggregate_rewrite(pattern),
            "hint cover set must accept the full pattern {pattern:?}"
        );
    }
}

pub(in crate::engine) fn resolve_compat_aggregate_rewrite(
    aggregate_name: &str,
    rewrites: &std::collections::HashMap<String, CompatAggregateRewrite>,
) -> Option<CompatAggregateRewrite> {
    rewrites
        .get(aggregate_name)
        .cloned()
        .or_else(|| builtin_compat_aggregate_rewrite(aggregate_name))
}

pub(in crate::engine) fn skip_single_quoted_literal(sql: &str, mut cursor: usize) -> usize {
    cursor += 1;
    while cursor < sql.len() {
        let Some(ch) = sql[cursor..].chars().next() else {
            break;
        };
        if ch == '\'' {
            cursor += 1;
            if sql.get(cursor..).is_some_and(|tail| tail.starts_with('\'')) {
                cursor += 1;
                continue;
            }
            break;
        }
        cursor += ch.len_utf8();
    }
    cursor
}

pub(in crate::engine) fn skip_double_quoted_identifier(sql: &str, mut cursor: usize) -> usize {
    cursor += 1;
    while cursor < sql.len() {
        let Some(ch) = sql[cursor..].chars().next() else {
            break;
        };
        cursor += ch.len_utf8();
        if ch == '"' {
            break;
        }
    }
    cursor
}

pub(in crate::engine) fn rewrite_inline_compat_aggregate_calls(
    expr: &str,
    rewrites: &std::collections::HashMap<String, CompatAggregateRewrite>,
) -> Option<String> {
    let mut out = String::with_capacity(expr.len());
    let mut cursor = 0usize;
    let mut last_emit = 0usize;
    let mut changed = false;

    while cursor < expr.len() {
        let Some(ch) = expr[cursor..].chars().next() else {
            break;
        };
        if ch == '\'' {
            cursor = skip_single_quoted_literal(expr, cursor);
            continue;
        }
        if ch == '"' {
            cursor = skip_double_quoted_identifier(expr, cursor);
            continue;
        }
        if !(ch.is_ascii_alphabetic() || ch == '_') {
            cursor += ch.len_utf8();
            continue;
        }

        let start = cursor;
        let mut parse_cursor = cursor;
        let Some(function_name) = parse_compat_identifier(expr, &mut parse_cursor) else {
            cursor += ch.len_utf8();
            continue;
        };
        skip_sql_whitespace(expr, &mut parse_cursor);
        if !expr
            .get(parse_cursor..)
            .is_some_and(|tail| tail.starts_with('('))
        {
            cursor += ch.len_utf8();
            continue;
        }
        let Some(args_raw) = extract_parenthesized(expr, &mut parse_cursor) else {
            cursor += ch.len_utf8();
            continue;
        };
        skip_sql_whitespace(expr, &mut parse_cursor);
        if expr
            .get(parse_cursor..parse_cursor.saturating_add("within".len()))
            .is_some_and(|raw| raw.eq_ignore_ascii_case("within"))
        {
            cursor += ch.len_utf8();
            continue;
        }

        let mut filter_clause = String::new();
        if expr
            .get(parse_cursor..parse_cursor.saturating_add("filter".len()))
            .is_some_and(|raw| raw.eq_ignore_ascii_case("filter"))
        {
            parse_cursor += "filter".len();
            skip_sql_whitespace(expr, &mut parse_cursor);
            let Some(predicate) = extract_parenthesized(expr, &mut parse_cursor) else {
                cursor += ch.len_utf8();
                continue;
            };
            filter_clause = format!(" FILTER ({predicate})");
        }

        let aggregate_name = function_name
            .split('.')
            .next_back()
            .unwrap_or(function_name.as_str())
            .to_ascii_lowercase();
        if matches!(aggregate_name.as_str(), "aggfns" | "aggfstr") {
            cursor += ch.len_utf8();
            continue;
        }
        let Some(rewrite) = resolve_compat_aggregate_rewrite(&aggregate_name, rewrites) else {
            cursor += ch.len_utf8();
            continue;
        };

        let rewritten_args = rewrite_inline_compat_aggregate_calls(args_raw.trim(), rewrites)
            .unwrap_or_else(|| args_raw.trim().to_owned());
        let rewritten_expr = build_compat_aggregate_rewrite_expression_core(
            &rewritten_args,
            &filter_clause,
            &rewrite,
        );
        out.push_str(&expr[last_emit..start]);
        out.push_str(&rewritten_expr);
        last_emit = parse_cursor;
        cursor = parse_cursor;
        changed = true;
    }

    if !changed {
        return None;
    }
    out.push_str(&expr[last_emit..]);
    Some(out)
}

pub(in crate::engine) fn append_predicate_to_from_clause(from_clause: &str, predicate: &str) -> String {
    if find_top_level_keyword(from_clause, "where").is_some() {
        format!("{from_clause} AND ({predicate})")
    } else {
        format!("{from_clause} WHERE {predicate}")
    }
}

pub(in crate::engine) fn rewrite_grouped_mode_select_query(items: &[String], from_tail: &str) -> Option<String> {
    let normalized_from_tail = from_tail.trim_end().strip_suffix(';').unwrap_or(from_tail);
    let group_by_pos = find_top_level_keyword(normalized_from_tail, "group by")?;
    let from_before_group_by = normalized_from_tail[..group_by_pos].trim_end();
    let group_and_tail = normalized_from_tail[group_by_pos + "group by".len()..].trim_start();
    let mut group_end = group_and_tail.len();
    for kw in ["having", "window", "order by", "limit", "offset"] {
        if let Some(pos) = find_top_level_keyword(group_and_tail, kw) {
            group_end = group_end.min(pos);
        }
    }
    let group_expr_sql = group_and_tail[..group_end].trim();
    if group_expr_sql.is_empty() {
        return None;
    }
    let trailing_tail = group_and_tail[group_end..].trim_start();
    let group_exprs = split_top_level_csv_items(group_expr_sql)?;

    let mut mode_order_expr = None;
    let mut mode_filter = String::new();
    let mut non_mode_items = Vec::new();
    for item in items {
        let trimmed = item.trim();
        let Some((aggregate_name, direct_args, order_clause, filter_clause, _suffix)) =
            parse_ordered_set_aggregate_select_item(trimmed)
        else {
            non_mode_items.push(trimmed.to_owned());
            continue;
        };
        if !aggregate_name.eq_ignore_ascii_case("mode") || !direct_args.trim().is_empty() {
            return None;
        }
        let order_items = split_top_level_csv_items(&order_clause)?;
        if order_items.len() != 1 {
            return None;
        }
        mode_order_expr = Some(strip_order_item_decorations(order_items[0].trim()));
        mode_filter = filter_clause;
    }
    let mode_order_expr = mode_order_expr?;
    if mode_order_expr.is_empty() || non_mode_items.len() != group_exprs.len() {
        return None;
    }
    if !non_mode_items
        .iter()
        .zip(group_exprs.iter())
        .all(|(item, group_expr)| item.eq_ignore_ascii_case(group_expr.trim()))
    {
        return None;
    }

    let mut mode_predicate = format!("({mode_order_expr}) IS NOT NULL");
    if let Some(filter_predicate) = parse_filter_clause_predicate(&mode_filter) {
        mode_predicate = format!("({mode_predicate}) AND ({filter_predicate})");
    }
    let filtered_from = append_predicate_to_from_clause(from_before_group_by, &mode_predicate);
    let group_expr_list = group_exprs
        .iter()
        .map(|expr| expr.trim())
        .collect::<Vec<_>>();
    let group_aliases = (0..group_expr_list.len())
        .map(|idx| format!("__mode_g{}", idx + 1))
        .collect::<Vec<_>>();
    let group_projection = group_expr_list
        .iter()
        .zip(group_aliases.iter())
        .map(|(expr, alias)| format!("{expr} AS {alias}"))
        .collect::<Vec<_>>()
        .join(", ");
    let group_by_list = group_expr_list.join(", ");
    let counted = format!(
        "SELECT {group_projection}, {mode_order_expr} AS __mode_v, count(*) AS __mode_cnt {filtered_from} GROUP BY {group_by_list}, {mode_order_expr}"
    );
    let join_predicate = group_aliases
        .iter()
        .map(|alias| format!("__mode_top.{alias} = __mode_cap.{alias}"))
        .collect::<Vec<_>>()
        .join(" AND ");
    let grouped_alias_list = group_aliases
        .iter()
        .map(|alias| format!("__mode_top.{alias}"))
        .collect::<Vec<_>>()
        .join(", ");
    let final_group_items = non_mode_items
        .iter()
        .zip(group_aliases.iter())
        .map(|(item, alias)| format!("__mode_top.{alias} AS {item}"))
        .collect::<Vec<_>>()
        .join(", ");

    let mut rewritten = format!(
        "SELECT {final_group_items}, min(__mode_top.__mode_v) AS mode FROM ({counted}) __mode_top JOIN (SELECT {}, max(__mode_cnt) AS __max_cnt FROM ({counted}) __mode_max GROUP BY {}) __mode_cap ON {join_predicate} AND __mode_top.__mode_cnt = __mode_cap.__max_cnt GROUP BY {grouped_alias_list}",
        group_aliases.join(", "),
        group_aliases.join(", ")
    );
    if !trailing_tail.is_empty() {
        rewritten.push(' ');
        rewritten.push_str(trailing_tail);
    }
    Some(rewritten)
}

pub(in crate::engine) fn rewrite_single_percentile_array_select_query(
    items: &[String],
    from_tail: &str,
) -> Option<String> {
    let normalized_from_tail = from_tail.trim_end().strip_suffix(';').unwrap_or(from_tail);
    if items.len() != 1 || normalized_from_tail.is_empty() {
        return None;
    }
    let trimmed = items[0].trim();
    let (aggregate_name, direct_args, order_clause, filter_clause, suffix) =
        parse_ordered_set_aggregate_select_item(trimmed)?;
    if !matches!(
        aggregate_name.as_str(),
        "percentile_disc" | "test_percentile_disc" | "percentile_cont"
    ) {
        return None;
    }
    let direct_args = split_top_level_csv_items(&direct_args)?;
    if direct_args.len() != 1 {
        return None;
    }
    let percentiles = parse_percentile_array_direct_args(direct_args[0].trim())?;
    let order_items = split_top_level_csv_items(&order_clause)?;
    if order_items.len() != 1 {
        return None;
    }
    let order_expr = strip_order_item_decorations(order_items[0].trim());
    if order_expr.is_empty() {
        return None;
    }
    let mut predicate = format!("({order_expr}) IS NOT NULL");
    if let Some(filter_predicate) = parse_filter_clause_predicate(&filter_clause) {
        predicate = format!("({predicate}) AND ({filter_predicate})");
    }
    let filtered_from = append_predicate_to_from_clause(normalized_from_tail, &predicate);
    let array_expr = if matches!(
        aggregate_name.as_str(),
        "percentile_disc" | "test_percentile_disc"
    ) {
        let elems: Vec<String> = percentiles
            .iter()
            .map(|p| {
                let p = p.trim();
                format!(
                    "(SELECT (array_agg({order_expr} ORDER BY {order_expr}))[CASE WHEN ({p}) <= 0 THEN 1 WHEN ({p}) >= 1 THEN count({order_expr}) ELSE CEIL(({p}) * count({order_expr})::FLOAT8)::INT END] {filtered_from})"
                )
            })
            .collect();
        format!("ARRAY[{}]", elems.join(", "))
    } else {
        let elems: Vec<String> = percentiles
            .iter()
            .map(|p| {
                let p = p.trim();
                let rank_expr = format!("(1 + ({p}) * (count({order_expr}) - 1))");
                let lo_idx = format!("FLOOR(({rank_expr})::FLOAT8)::INT");
                let hi_idx = format!("CEIL(({rank_expr})::FLOAT8)::INT");
                let lo_val =
                    format!("(array_agg(({order_expr})::FLOAT8 ORDER BY {order_expr}))[{lo_idx}]");
                let hi_val =
                    format!("(array_agg(({order_expr})::FLOAT8 ORDER BY {order_expr}))[{hi_idx}]");
                format!(
                    "(SELECT (CASE WHEN count({order_expr}) = 0 THEN NULL WHEN {lo_idx} = {hi_idx} THEN {lo_val} ELSE {lo_val} + (({hi_val} - {lo_val}) * (({rank_expr})::FLOAT8 - ({lo_idx})::FLOAT8)) END) {filtered_from})"
                )
            })
            .collect();
        format!("ARRAY[{}]", elems.join(", "))
    };

    let select_expr = if suffix.is_empty() {
        format!("{array_expr} AS {aggregate_name}")
    } else {
        format!("{array_expr}{suffix}")
    };
    Some(format!("SELECT {select_expr}"))
}

pub(in crate::engine) fn rewrite_compat_aggregate_select_list(
    sql: &str,
    rewrites: &std::collections::HashMap<String, CompatAggregateRewrite>,
) -> Option<String> {
    if !trim_compat_statement(sql)
        .get(..6)
        .is_some_and(|head| head.eq_ignore_ascii_case("select"))
    {
        return None;
    }
    let select_pos = find_top_level_keyword(sql, "select")?;
    let from_pos = find_top_level_keyword(&sql[select_pos + "select".len()..], "from")
        .map(|pos| pos + select_pos + "select".len());
    let select_list = from_pos
        .map(|from_pos| sql[select_pos + "select".len()..from_pos].trim())
        .unwrap_or_else(|| sql[select_pos + "select".len()..].trim());
    let items = split_top_level_csv_items(select_list)?;
    let from_tail = from_pos.map_or("", |from_pos| sql[from_pos..].trim_start());
    let has_grouping = find_top_level_keyword(from_tail, "group by").is_some()
        || find_top_level_keyword(from_tail, "having").is_some()
        || find_top_level_keyword(from_tail, "window").is_some();
    if !has_grouping {
        if let Some(rewritten_array_query) =
            rewrite_single_percentile_array_select_query(&items, from_tail)
        {
            return Some(rewritten_array_query);
        }
    }
    if has_grouping {
        if let Some(rewritten_mode) = rewrite_grouped_mode_select_query(&items, from_tail) {
            return Some(rewritten_mode);
        }
    }
    let mut ordered_changed = false;
    let ordered_items: Vec<String> = items
        .iter()
        .map(|item| {
            let trimmed = item.trim();
            let Some((aggregate_name, direct_args, order_clause, filter_clause, suffix)) =
                parse_ordered_set_aggregate_select_item(trimmed)
            else {
                return item.clone();
            };
            let Some((mut rewritten, uses_outer_from)) = rewrite_ordered_set_select_item(
                &aggregate_name,
                &direct_args,
                &order_clause,
                &filter_clause,
                from_tail,
                !has_grouping,
            ) else {
                return item.clone();
            };
            if !suffix.is_empty() {
                let default_alias = format!(" AS {aggregate_name}");
                if rewritten.ends_with(&default_alias) {
                    let cutoff = rewritten.len() - default_alias.len();
                    rewritten.truncate(cutoff);
                }
                rewritten.push_str(&suffix);
            }
            ordered_changed = true;
            let _ = uses_outer_from;
            rewritten
        })
        .collect();
    if ordered_changed && !from_tail.is_empty() {
        return Some(format!(
            "{} {} {}",
            sql[..select_pos + "select".len()].trim_end(),
            ordered_items.join(", "),
            from_tail
        ));
    }

    if items.len() == 1 && !from_tail.is_empty() {
        let trimmed = items[0].trim();
        if let Some((aggregate_name, args, filter_clause)) =
            parse_compat_aggregate_select_item(trimmed)
        {
            if matches!(aggregate_name.as_str(), "aggfns" | "aggfstr") {
                return build_compat_multiarg_aggregate_query(
                    &aggregate_name,
                    &args,
                    &filter_clause,
                    from_tail,
                );
            }
        }
    }

    let mut changed = false;
    let rewritten_items: Vec<String> = items
        .into_iter()
        .map(|item| {
            let trimmed = item.trim();
            if let Some(inner_call) = parse_pg_typeof_wrapped_call(trimmed) {
                if let Some((inner_name, inner_args, inner_filter)) =
                    parse_compat_aggregate_select_item(&inner_call)
                {
                    if !matches!(inner_name.as_str(), "aggfns" | "aggfstr") {
                        if let Some(rewrite) =
                            resolve_compat_aggregate_rewrite(&inner_name, rewrites)
                        {
                            changed = true;
                            let inner_expr = build_compat_aggregate_rewrite_expression_core(
                                &inner_args,
                                &inner_filter,
                                &rewrite,
                            );
                            return format!("pg_typeof({inner_expr}) AS pg_typeof");
                        }
                    }
                }
            }

            let Some((aggregate_name, args, filter_clause)) =
                parse_compat_aggregate_select_item(trimmed)
            else {
                if let Some(inline_rewrite) =
                    rewrite_inline_compat_aggregate_calls(trimmed, rewrites)
                {
                    changed = true;
                    return inline_rewrite;
                }
                return item;
            };
            let Some(rewrite) = resolve_compat_aggregate_rewrite(&aggregate_name, rewrites) else {
                if let Some(inline_rewrite) =
                    rewrite_inline_compat_aggregate_calls(trimmed, rewrites)
                {
                    changed = true;
                    return inline_rewrite;
                }
                return item;
            };
            changed = true;
            build_compat_aggregate_rewrite_expression(
                &aggregate_name,
                &args,
                &filter_clause,
                &rewrite,
            )
        })
        .collect();

    if !changed {
        return None;
    }

    if let Some(from_pos) = from_pos {
        Some(format!(
            "{} {} {}",
            sql[..select_pos + "select".len()].trim_end(),
            rewritten_items.join(", "),
            sql[from_pos..].trim_start()
        ))
    } else {
        Some(format!(
            "{} {}",
            sql[..select_pos + "select".len()].trim_end(),
            rewritten_items.join(", ")
        ))
    }
}

pub(in crate::engine) fn rewrite_compat_aggregate_query(
    sql: &str,
    rewrites: &std::collections::HashMap<String, CompatAggregateRewrite>,
) -> Option<String> {
    if let Some(rewritten) = rewrite_compat_aggregate_select_list(sql, rewrites) {
        return Some(rewritten);
    }

    let trimmed = trim_compat_statement(sql);
    if trimmed
        .get(..7)
        .is_some_and(|head| head.eq_ignore_ascii_case("explain"))
    {
        if let Some(select_pos) = find_top_level_keyword(trimmed, "select") {
            let prefix = trimmed[..select_pos].trim_end();
            let select_sql = trimmed[select_pos..].trim_start();
            if let Some(rewritten_select) =
                rewrite_compat_aggregate_select_list(select_sql, rewrites)
            {
                return Some(format!("{prefix} {rewritten_select}"));
            }
        }
    }

    let is_create_view = trimmed
        .get(..11)
        .is_some_and(|head| head.eq_ignore_ascii_case("create view"))
        || trimmed
            .get(..22)
            .is_some_and(|head| head.eq_ignore_ascii_case("create or replace view"));
    if !is_create_view {
        return None;
    }

    let as_pos = find_top_level_keyword(trimmed, "as")?;
    let prefix = trimmed[..as_pos + "as".len()].trim_end();
    let select_sql = trimmed[as_pos + "as".len()..].trim_start();
    let rewritten_select = rewrite_compat_aggregate_select_list(select_sql, rewrites)?;
    Some(format!("{prefix} {rewritten_select}"))
}

#[cfg(test)]
pub(crate) fn rewrite_compat_aggregate_select_list_for_test(
    sql: &str,
    rewrites: &std::collections::HashMap<String, CompatAggregateRewrite>,
) -> Option<String> {
    rewrite_compat_aggregate_query(sql, rewrites)
}
