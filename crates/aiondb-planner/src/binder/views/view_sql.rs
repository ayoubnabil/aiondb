use super::*;
use aiondb_parser::Literal;

pub(super) fn reconstruct_select_sql(select: &SelectStatement) -> String {
    let mut sql = String::new();
    if !select.ctes.is_empty() {
        sql.push_str("WITH ");
        if select.ctes.iter().any(|cte| cte.recursive) {
            sql.push_str("RECURSIVE ");
        }
        for (i, cte) in select.ctes.iter().enumerate() {
            if i > 0 {
                sql.push_str(", ");
            }
            sql.push_str(&quote_ident(&cte.name));
            if let Some(ref aliases) = cte.column_aliases {
                sql.push('(');
                let quoted: Vec<String> = aliases.iter().map(|a| quote_ident(a)).collect();
                sql.push_str(&quoted.join(", "));
                sql.push(')');
            }
            sql.push_str(" AS (");
            if let Some(recursive_term) = &cte.recursive_term {
                let left = match cte.query.as_ref() {
                    Statement::SetOperation(_) => {
                        format!("({})", reconstruct_statement_sql(&cte.query))
                    }
                    _ => reconstruct_statement_sql(&cte.query),
                };
                let right = reconstruct_select_sql(recursive_term);
                sql.push_str(&left);
                sql.push_str(" UNION");
                if cte.union_all {
                    sql.push_str(" ALL");
                }
                sql.push(' ');
                sql.push_str(&right);
            } else {
                sql.push_str(&reconstruct_statement_sql(&cte.query));
            }
            sql.push(')');
        }
        sql.push(' ');
    }
    sql.push_str("SELECT ");
    match &select.distinct {
        aiondb_parser::DistinctKind::All => {}
        aiondb_parser::DistinctKind::Distinct => {
            sql.push_str("DISTINCT ");
        }
        aiondb_parser::DistinctKind::DistinctOn(exprs) => {
            sql.push_str("DISTINCT ON (");
            for (i, expr) in exprs.iter().enumerate() {
                if i > 0 {
                    sql.push_str(", ");
                }
                sql.push_str(&format_expr(expr));
            }
            sql.push_str(") ");
        }
    }
    for (i, item) in select.items.iter().enumerate() {
        if i > 0 {
            sql.push_str(", ");
        }
        sql.push_str(&format_expr(&item.expr));
        if let Some(ref alias) = item.alias {
            sql.push_str(" AS ");
            sql.push_str(&quote_ident(alias));
        }
    }
    if let Some(ref from) = select.from {
        sql.push_str(" FROM ");
        sql.push_str(&quote_ident_parts(&from.parts));
        if let Some(ref alias) = select.from_alias {
            sql.push(' ');
            sql.push_str(&quote_ident(alias));
        }
    }
    for join in &select.joins {
        sql.push_str(&format_join(join));
    }
    if let Some(ref selection) = select.selection {
        sql.push_str(" WHERE ");
        sql.push_str(&format_expr(selection));
    }
    if !select.group_by.is_empty() {
        sql.push_str(" GROUP BY ");
        for (i, expr) in select.group_by.iter().enumerate() {
            if i > 0 {
                sql.push_str(", ");
            }
            sql.push_str(&format_expr(expr));
        }
    }
    if let Some(ref having) = select.having {
        sql.push_str(" HAVING ");
        sql.push_str(&format_expr(having));
    }
    if !select.order_by.is_empty() {
        sql.push_str(" ORDER BY ");
        for (i, item) in select.order_by.iter().enumerate() {
            if i > 0 {
                sql.push_str(", ");
            }
            sql.push_str(&format_expr(&item.expr));
            if item.descending {
                sql.push_str(" DESC");
            }
            match item.nulls_first {
                Some(true) => sql.push_str(" NULLS FIRST"),
                Some(false) => sql.push_str(" NULLS LAST"),
                None => {}
            }
        }
    }
    use std::fmt::Write;
    if let Some(ref limit) = select.limit {
        let _ = write!(sql, " LIMIT {}", format_expr(limit));
    }
    if let Some(ref offset) = select.offset {
        let _ = write!(sql, " OFFSET {}", format_expr(offset));
    }
    sql
}

use aiondb_parser::identifier::quote_identifier as quote_ident;

fn quote_ident_parts(parts: &[String]) -> String {
    parts
        .iter()
        .map(|p| quote_ident(p))
        .collect::<Vec<_>>()
        .join(".")
}

fn reconstruct_statement_sql(statement: &Statement) -> String {
    match statement {
        Statement::Select(select) => reconstruct_select_sql(select),
        Statement::SetOperation(set_op) => reconstruct_set_operation_sql(set_op),
        _ => String::new(),
    }
}

fn reconstruct_set_operation_sql(set_op: &aiondb_parser::SetOperationStatement) -> String {
    let left = match set_op.left.as_ref() {
        Statement::SetOperation(_) => format!("({})", reconstruct_statement_sql(&set_op.left)),
        _ => reconstruct_statement_sql(&set_op.left),
    };
    let right = match set_op.right.as_ref() {
        Statement::SetOperation(_) => format!("({})", reconstruct_statement_sql(&set_op.right)),
        _ => reconstruct_statement_sql(&set_op.right),
    };
    let mut sql = format!(
        "{} {}{} {}",
        left,
        match set_op.op {
            aiondb_parser::SetOperationType::Union => "UNION",
            aiondb_parser::SetOperationType::Intersect => "INTERSECT",
            aiondb_parser::SetOperationType::Except => "EXCEPT",
        },
        if set_op.all { " ALL" } else { "" },
        right
    );
    if !set_op.order_by.is_empty() {
        sql.push_str(" ORDER BY ");
        for (i, item) in set_op.order_by.iter().enumerate() {
            if i > 0 {
                sql.push_str(", ");
            }
            sql.push_str(&format_expr(&item.expr));
            if item.descending {
                sql.push_str(" DESC");
            }
        }
    }
    if let Some(limit) = &set_op.limit {
        sql.push_str(" LIMIT ");
        sql.push_str(&format_expr(limit));
    }
    if let Some(offset) = &set_op.offset {
        sql.push_str(" OFFSET ");
        sql.push_str(&format_expr(offset));
    }
    sql
}

fn format_join(join: &aiondb_parser::JoinClause) -> String {
    let natural_prefix = if join.natural { "NATURAL " } else { "" };
    let join_keyword = match join.join_type {
        aiondb_parser::AstJoinType::Inner => format!(" {natural_prefix}INNER JOIN "),
        aiondb_parser::AstJoinType::Left => format!(" {natural_prefix}LEFT JOIN "),
        aiondb_parser::AstJoinType::Right => format!(" {natural_prefix}RIGHT JOIN "),
        aiondb_parser::AstJoinType::Full => format!(" {natural_prefix}FULL JOIN "),
        aiondb_parser::AstJoinType::Cross => format!(" {natural_prefix}CROSS JOIN "),
    };
    let mut s = join_keyword;
    s.push_str(&quote_ident_parts(&join.table.parts));
    if let Some(ref alias) = join.alias {
        s.push(' ');
        s.push_str(&quote_ident(alias));
    }
    if let Some(ref cond) = join.condition {
        s.push_str(" ON ");
        s.push_str(&format_expr(cond));
    }
    if !join.using_columns.is_empty() {
        s.push_str(" USING (");
        let quoted: Vec<String> = join.using_columns.iter().map(|c| quote_ident(c)).collect();
        s.push_str(&quoted.join(", "));
        s.push(')');
        if let Some(using_alias) = &join.using_alias {
            s.push_str(" AS ");
            s.push_str(&quote_ident(using_alias));
        }
    }
    s
}

fn literal_text(expr: &Expr) -> Option<&str> {
    match expr {
        Expr::Literal(Literal::String(value), _) => Some(value.as_str()),
        _ => None,
    }
}

fn format_is_json_predicate_expr(args: &[Expr]) -> Option<String> {
    if args.len() != 3 {
        return None;
    }
    let input = format_expr(&args[0]);
    let kind = literal_text(&args[1])?.to_ascii_uppercase();
    let unique_mode = literal_text(&args[2])?.to_ascii_uppercase();
    let mut rendered = format!("{input} IS JSON");
    match kind.as_str() {
        "JSON" | "VALUE" => {}
        "OBJECT" => rendered.push_str(" OBJECT"),
        "ARRAY" => rendered.push_str(" ARRAY"),
        "SCALAR" => rendered.push_str(" SCALAR"),
        _ => return None,
    }
    match unique_mode.as_str() {
        "DEFAULT" => {}
        "WITH" => rendered.push_str(" WITH UNIQUE KEYS"),
        "WITHOUT" => rendered.push_str(" WITHOUT UNIQUE KEYS"),
        _ => return None,
    }
    Some(rendered)
}

pub(super) fn format_expr(expr: &Expr) -> String {
    match expr {
        Expr::Identifier(name) => format_identifier_expr(name),
        Expr::Literal(lit, _) => format_literal(lit),
        Expr::Parameter { index, .. } => format!("${index}"),
        Expr::Default { .. } => "DEFAULT".to_owned(),
        Expr::FunctionCall {
            name,
            args,
            distinct,
            filter,
            ..
        } => {
            if name
                .parts
                .last()
                .is_some_and(|part| part.eq_ignore_ascii_case("__aiondb_json_array_subquery"))
            {
                if let Some(arg) = args.first() {
                    let rendered = match arg {
                        Expr::ArraySubquery { query, .. } | Expr::Subquery { query, .. } => {
                            format!("JSON_ARRAY({})", reconstruct_select_sql(query))
                        }
                        _ => format!("JSON_ARRAY({})", format_expr(arg)),
                    };
                    return rendered;
                }
            }
            if name
                .parts
                .last()
                .is_some_and(|part| part.eq_ignore_ascii_case("__aiondb_is_json"))
            {
                if let Some(rendered) = format_is_json_predicate_expr(args) {
                    return rendered;
                }
            }
            let args_str: Vec<String> = args.iter().map(format_expr).collect();
            let distinct_prefix = if *distinct { "DISTINCT " } else { "" };
            let mut result = format!(
                "{}({distinct_prefix}{})",
                quote_ident_parts(&name.parts),
                args_str.join(", ")
            );
            if let Some(ref f) = filter {
                use std::fmt::Write;
                let _ = write!(result, " FILTER (WHERE {})", format_expr(f));
            }
            result
        }
        Expr::UnaryOp { op, expr, .. } => {
            let inner = format_expr(expr);
            match op {
                aiondb_parser::UnaryOperator::Not => format!("NOT ({inner})"),
                aiondb_parser::UnaryOperator::Minus => format!("(-{inner})"),
                aiondb_parser::UnaryOperator::BitwiseNot => format!("(~{inner})"),
                aiondb_parser::UnaryOperator::Abs => format!("(@{inner})"),
                aiondb_parser::UnaryOperator::SquareRoot => format!("(|/{inner})"),
                aiondb_parser::UnaryOperator::CubeRoot => format!("(||/{inner})"),
            }
        }
        Expr::BinaryOp {
            left, op, right, ..
        } => {
            let op_str = format_binary_op(op);
            format!("({} {op_str} {})", format_expr(left), format_expr(right))
        }
        Expr::IsNull { expr, negated, .. } => {
            let inner = format_expr(expr);
            if *negated {
                format!("{inner} IS NOT NULL")
            } else {
                format!("{inner} IS NULL")
            }
        }
        Expr::IsDistinctFrom {
            left,
            right,
            negated,
            ..
        } => {
            let l = format_expr(left);
            let r = format_expr(right);
            if *negated {
                format!("{l} IS NOT DISTINCT FROM {r}")
            } else {
                format!("{l} IS DISTINCT FROM {r}")
            }
        }
        Expr::Like {
            expr,
            pattern,
            negated,
            case_insensitive,
            ..
        } => {
            let inner = format_expr(expr);
            let pat = format_expr(pattern);
            let op = if *case_insensitive { "ILIKE" } else { "LIKE" };
            if *negated {
                format!("{inner} NOT {op} {pat}")
            } else {
                format!("{inner} {op} {pat}")
            }
        }
        Expr::InList {
            expr,
            list,
            negated,
            ..
        } => {
            let inner = format_expr(expr);
            let items: Vec<String> = list.iter().map(format_expr).collect();
            if *negated {
                format!("{inner} NOT IN ({})", items.join(", "))
            } else {
                format!("{inner} IN ({})", items.join(", "))
            }
        }
        Expr::Between {
            expr,
            low,
            high,
            negated,
            ..
        } => {
            let inner = format_expr(expr);
            let lo = format_expr(low);
            let hi = format_expr(high);
            if *negated {
                format!("{inner} NOT BETWEEN {lo} AND {hi}")
            } else {
                format!("{inner} BETWEEN {lo} AND {hi}")
            }
        }
        Expr::Cast {
            expr, data_type, ..
        } => {
            let inner = format_expr(expr);
            format!("CAST({inner} AS {data_type})")
        }
        Expr::CaseWhen {
            operand,
            conditions,
            results,
            else_result,
            ..
        } => {
            let mut s = String::from("CASE");
            if let Some(op) = operand {
                s.push(' ');
                s.push_str(&format_expr(op));
            }
            for (cond, res) in conditions.iter().zip(results.iter()) {
                s.push_str(" WHEN ");
                s.push_str(&format_expr(cond));
                s.push_str(" THEN ");
                s.push_str(&format_expr(res));
            }
            if let Some(el) = else_result {
                s.push_str(" ELSE ");
                s.push_str(&format_expr(el));
            }
            s.push_str(" END");
            s
        }
        Expr::Array { elements, .. } => {
            let items: Vec<String> = elements.iter().map(format_expr).collect();
            format!("ARRAY[{}]", items.join(", "))
        }
        Expr::ArraySubquery { query, .. } => {
            format!("ARRAY({})", reconstruct_select_sql(query))
        }
        Expr::Subquery { query, .. } => {
            format!("({})", reconstruct_select_sql(query))
        }
        Expr::InSubquery {
            expr,
            query,
            negated,
            ..
        } => {
            let inner = format_expr(expr);
            let sub = reconstruct_select_sql(query);
            if *negated {
                format!("{inner} NOT IN ({sub})")
            } else {
                format!("{inner} IN ({sub})")
            }
        }
        Expr::Exists { query, negated, .. } => {
            let sub = reconstruct_select_sql(query);
            if *negated {
                format!("NOT EXISTS ({sub})")
            } else {
                format!("EXISTS ({sub})")
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
            ..
        } => {
            let func_str = format_expr(function);
            let mut parts = Vec::new();
            if !partition_by.is_empty() {
                let pb: Vec<String> = partition_by.iter().map(format_expr).collect();
                parts.push(format!("PARTITION BY {}", pb.join(", ")));
            }
            if !order_by.is_empty() {
                let ob: Vec<String> = order_by
                    .iter()
                    .map(|item| {
                        let dir = if item.descending { " DESC" } else { "" };
                        format!("{}{dir}", format_expr(&item.expr))
                    })
                    .collect();
                parts.push(format!("ORDER BY {}", ob.join(", ")));
            }
            format!("{func_str} OVER ({})", parts.join(" "))
        }
    }
}

fn format_identifier_expr(name: &ObjectName) -> String {
    if name.parts.len() == 1 && name.parts[0] == "*" {
        return "*".to_owned();
    }
    if name.parts.len() >= 2 && name.parts.last().is_some_and(|part| part == "*") {
        let mut rendered = name
            .parts
            .iter()
            .take(name.parts.len() - 1)
            .map(|part| quote_ident(part))
            .collect::<Vec<_>>()
            .join(".");
        rendered.push_str(".*");
        return rendered;
    }
    quote_ident_parts(&name.parts)
}

fn format_literal(lit: &aiondb_parser::Literal) -> String {
    match lit {
        aiondb_parser::Literal::Integer(n) => n.to_string(),
        aiondb_parser::Literal::String(s) => {
            let escaped = s.replace('\'', "''");
            format!("'{escaped}'")
        }
        aiondb_parser::Literal::Boolean(b) => if *b { "TRUE" } else { "FALSE" }.to_owned(),
        aiondb_parser::Literal::Null => "NULL".to_owned(),
        aiondb_parser::Literal::NumericLit(s) => s.clone(),
    }
}

fn format_binary_op(op: &aiondb_parser::BinaryOperator) -> &'static str {
    match op {
        aiondb_parser::BinaryOperator::Eq => "=",
        aiondb_parser::BinaryOperator::Ne => "<>",
        aiondb_parser::BinaryOperator::Gt => ">",
        aiondb_parser::BinaryOperator::Ge => ">=",
        aiondb_parser::BinaryOperator::Lt => "<",
        aiondb_parser::BinaryOperator::Le => "<=",
        aiondb_parser::BinaryOperator::And => "AND",
        aiondb_parser::BinaryOperator::Or => "OR",
        aiondb_parser::BinaryOperator::Add => "+",
        aiondb_parser::BinaryOperator::Sub => "-",
        aiondb_parser::BinaryOperator::Mul => "*",
        aiondb_parser::BinaryOperator::Div => "/",
        aiondb_parser::BinaryOperator::Mod => "%",
        aiondb_parser::BinaryOperator::Concat => "||",
        aiondb_parser::BinaryOperator::JsonGet => "->",
        aiondb_parser::BinaryOperator::JsonGetText => "->>",
        aiondb_parser::BinaryOperator::JsonPathGet => "#>",
        aiondb_parser::BinaryOperator::JsonPathGetText => "#>>",
        aiondb_parser::BinaryOperator::JsonContains => "@>",
        aiondb_parser::BinaryOperator::JsonContainedBy => "<@",
        aiondb_parser::BinaryOperator::JsonKeyExists => "?",
        aiondb_parser::BinaryOperator::JsonAnyKeyExists => "?|",
        aiondb_parser::BinaryOperator::JsonAllKeysExist => "?&",
        aiondb_parser::BinaryOperator::ArrayOverlap => "&&",
        aiondb_parser::BinaryOperator::Exp => "^",
        aiondb_parser::BinaryOperator::BitwiseAnd => "&",
        aiondb_parser::BinaryOperator::BitwiseOr => "|",
        aiondb_parser::BinaryOperator::BitwiseXor => "#",
        aiondb_parser::BinaryOperator::ShiftLeft => "<<",
        aiondb_parser::BinaryOperator::ShiftRight => ">>",
        aiondb_parser::BinaryOperator::RegexMatch => "~",
        aiondb_parser::BinaryOperator::RegexMatchInsensitive => "~*",
        aiondb_parser::BinaryOperator::NotRegexMatch => "!~",
        aiondb_parser::BinaryOperator::NotRegexMatchInsensitive => "!~*",
        aiondb_parser::BinaryOperator::FullTextSearch => "@@",
        aiondb_parser::BinaryOperator::JsonPathExists => "@?",
        aiondb_parser::BinaryOperator::GeometricEq => "~=",
        aiondb_parser::BinaryOperator::VectorL2Distance => "<->",
        aiondb_parser::BinaryOperator::VectorCosineDistance => "<=>",
        aiondb_parser::BinaryOperator::VectorNegativeInnerProduct => "<#>",
        aiondb_parser::BinaryOperator::VectorL1Distance => "<+>",
        aiondb_parser::BinaryOperator::VectorHammingDistance => "<~>",
        aiondb_parser::BinaryOperator::VectorJaccardDistance => "<%>",
    }
}
