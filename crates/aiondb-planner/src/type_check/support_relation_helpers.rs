use super::*;

pub(crate) fn is_system_column(name: &str) -> bool {
    matches!(
        name,
        "tableoid" | "ctid" | "xmin" | "xmax" | "cmin" | "cmax" | "oid"
    )
}

pub(crate) fn compat_relation_with_system_columns(relation: &TableDescriptor) -> TableDescriptor {
    let mut columns = relation.columns.clone();
    let next_ordinal = |columns: &[ColumnDescriptor]| {
        u32::try_from(columns.len())
            .unwrap_or(u32::MAX)
            .saturating_add(1)
    };

    let system_columns = [
        ("ctid", DataType::Tid, false),
        ("tableoid", DataType::Int, true),
        ("xmin", DataType::Int, true),
        ("xmax", DataType::Int, true),
        ("cmin", DataType::Int, true),
        ("cmax", DataType::Int, true),
        ("oid", DataType::Int, true),
    ];

    for (index, (name, data_type, nullable)) in system_columns.into_iter().enumerate() {
        if columns
            .iter()
            .any(|column| column.name.eq_ignore_ascii_case(name))
        {
            continue;
        }
        columns.push(ColumnDescriptor {
            column_id: ColumnId::new(
                u64::MAX.saturating_sub(u64::try_from(index).unwrap_or(u64::MAX)),
            ),
            name: name.to_owned(),
            data_type,
            raw_type_name: None,
            text_type_modifier: None,
            nullable,
            ordinal_position: next_ordinal(&columns),
            default_value: None,
        });
    }

    TableDescriptor {
        table_id: relation.table_id,
        schema_id: relation.schema_id,
        name: relation.name.clone(),
        columns,
        identity_columns: relation.identity_columns.clone(),
        primary_key: relation.primary_key.clone(),
        foreign_keys: relation.foreign_keys.clone(),
        check_constraints: relation.check_constraints.clone(),
        shard_config: None,
        owner: None,
    }
}

pub(crate) fn relation_with_alias_columns(
    relation: &TableDescriptor,
    alias: Option<&str>,
) -> TableDescriptor {
    let mut columns = relation.columns.clone();
    let relation_name = relation.name.object_name().to_owned();
    let qualifiers = match alias {
        // SQL aliasing hides the base relation name inside the current scope.
        Some(alias) => vec![alias.to_owned()],
        None => vec![relation_name.clone()],
    };

    let base_len = columns.len();
    // Build a HashSet for O(1) dedup instead of O(n) linear scan.
    let mut seen_names: std::collections::HashSet<String> = columns
        .iter()
        .map(|c| c.name.to_ascii_lowercase())
        .collect();
    for qualifier in qualifiers {
        for column in &relation.columns[..base_len] {
            let bare_name = column.name.rsplit('\0').next().unwrap_or(&column.name);
            let qualified_name = format!("{qualifier}\x00{bare_name}");
            if !seen_names.insert(qualified_name.to_ascii_lowercase()) {
                continue;
            }
            columns.push(ColumnDescriptor {
                column_id: column.column_id,
                name: qualified_name,
                data_type: column.data_type.clone(),
                raw_type_name: None,
                text_type_modifier: column.text_type_modifier,
                nullable: column.nullable,
                ordinal_position: column.ordinal_position,
                default_value: column.default_value.clone(),
            });
        }
    }

    TableDescriptor {
        table_id: relation.table_id,
        schema_id: relation.schema_id,
        name: relation.name.clone(),
        columns,
        identity_columns: relation.identity_columns.clone(),
        primary_key: relation.primary_key.clone(),
        foreign_keys: relation.foreign_keys.clone(),
        check_constraints: relation.check_constraints.clone(),
        shard_config: None,
        owner: None,
    }
}

pub(crate) fn resolve_session_variable(
    name: &str,
    current_user: Option<&str>,
    session_user: Option<&str>,
    current_schema: Option<&str>,
    current_database: Option<&str>,
) -> Option<Value> {
    match name.to_ascii_lowercase().as_str() {
        "current_user" | "user" => current_user
            .or(session_user)
            .map(|value| Value::Text(value.to_owned())),
        "session_user" => session_user
            .or(current_user)
            .map(|value| Value::Text(value.to_owned())),
        "current_schema" => Some(Value::Text(current_schema.unwrap_or("public").to_owned())),
        "current_catalog" | "current_database" => {
            current_database.map(|value| Value::Text(value.to_owned()))
        }
        _ => None,
    }
}

pub(crate) fn undefined_column(position: usize, column_name: &str) -> DbError {
    DbError::Bind(Box::new(
        ErrorReport::new(
            SqlState::UndefinedColumn,
            format!("column \"{column_name}\" does not exist"),
        )
        .with_position(position),
    ))
}

pub(crate) fn ambiguous_column(position: usize, column_name: &str) -> DbError {
    DbError::Bind(Box::new(
        ErrorReport::new(
            SqlState::SyntaxError,
            format!("column reference \"{column_name}\" is ambiguous"),
        )
        .with_position(position),
    ))
}

pub(crate) fn rewrite_table_aliases(expr: &Expr) -> Expr {
    match expr {
        Expr::Identifier(name) if name.parts.len() == 2 => {
            if name.parts[1] == "*" {
                return expr.clone();
            }
            let qualified = format!("{}\x00{}", name.parts[0], name.parts[1]);
            Expr::Identifier(ObjectName {
                parts: vec![qualified],
                span: name.span,
            })
        }
        Expr::Identifier(name) if name.parts.len() == 3 => {
            if name.parts[2] == "*" {
                return Expr::Identifier(ObjectName {
                    parts: vec![name.parts[1].clone(), "*".to_owned()],
                    span: name.span,
                });
            }
            let qualified = format!("{}\x00{}", name.parts[1], name.parts[2]);
            Expr::Identifier(ObjectName {
                parts: vec![qualified],
                span: name.span,
            })
        }
        Expr::BinaryOp {
            left,
            op,
            right,
            span,
        } => Expr::BinaryOp {
            left: Box::new(rewrite_table_aliases(left)),
            op: *op,
            right: Box::new(rewrite_table_aliases(right)),
            span: *span,
        },
        Expr::UnaryOp {
            op,
            expr: inner,
            span,
        } => Expr::UnaryOp {
            op: *op,
            expr: Box::new(rewrite_table_aliases(inner)),
            span: *span,
        },
        Expr::FunctionCall {
            name,
            args,
            distinct,
            filter,
            span,
        } => Expr::FunctionCall {
            name: name.clone(),
            args: args.iter().map(rewrite_table_aliases).collect(),
            distinct: *distinct,
            filter: filter.as_ref().map(|f| Box::new(rewrite_table_aliases(f))),
            span: *span,
        },
        Expr::IsNull {
            expr: inner,
            negated,
            span,
        } => Expr::IsNull {
            expr: Box::new(rewrite_table_aliases(inner)),
            negated: *negated,
            span: *span,
        },
        Expr::IsDistinctFrom {
            left,
            right,
            negated,
            span,
        } => Expr::IsDistinctFrom {
            left: Box::new(rewrite_table_aliases(left)),
            right: Box::new(rewrite_table_aliases(right)),
            negated: *negated,
            span: *span,
        },
        Expr::Cast {
            expr: inner,
            data_type,
            span,
        } => Expr::Cast {
            expr: Box::new(rewrite_table_aliases(inner)),
            data_type: data_type.clone(),
            span: *span,
        },
        Expr::Like {
            expr: inner,
            pattern,
            negated,
            case_insensitive,
            span,
        } => Expr::Like {
            expr: Box::new(rewrite_table_aliases(inner)),
            pattern: Box::new(rewrite_table_aliases(pattern)),
            negated: *negated,
            case_insensitive: *case_insensitive,
            span: *span,
        },
        Expr::InList {
            expr: inner,
            list,
            negated,
            span,
        } => Expr::InList {
            expr: Box::new(rewrite_table_aliases(inner)),
            list: list.iter().map(rewrite_table_aliases).collect(),
            negated: *negated,
            span: *span,
        },
        Expr::Between {
            expr: inner,
            low,
            high,
            negated,
            span,
        } => Expr::Between {
            expr: Box::new(rewrite_table_aliases(inner)),
            low: Box::new(rewrite_table_aliases(low)),
            high: Box::new(rewrite_table_aliases(high)),
            negated: *negated,
            span: *span,
        },
        Expr::CaseWhen {
            operand,
            conditions,
            results,
            else_result,
            span,
        } => Expr::CaseWhen {
            operand: operand.as_ref().map(|e| Box::new(rewrite_table_aliases(e))),
            conditions: conditions.iter().map(rewrite_table_aliases).collect(),
            results: results.iter().map(rewrite_table_aliases).collect(),
            else_result: else_result
                .as_ref()
                .map(|e| Box::new(rewrite_table_aliases(e))),
            span: *span,
        },
        Expr::Array { elements, span } => Expr::Array {
            elements: elements.iter().map(rewrite_table_aliases).collect(),
            span: *span,
        },
        Expr::WindowFunction {
            function,
            partition_by,
            order_by,
            window_name,
            span,
        } => Expr::WindowFunction {
            function: Box::new(rewrite_table_aliases(function)),
            partition_by: partition_by.iter().map(rewrite_table_aliases).collect(),
            order_by: order_by
                .iter()
                .map(|item| aiondb_parser::OrderByItem {
                    expr: rewrite_table_aliases(&item.expr),
                    descending: item.descending,
                    nulls_first: item.nulls_first,
                    span: item.span,
                })
                .collect(),
            window_name: window_name.clone(),
            span: *span,
        },
        Expr::InSubquery {
            expr: inner,
            query,
            negated,
            span,
        } => Expr::InSubquery {
            expr: Box::new(rewrite_table_aliases(inner)),
            query: query.clone(),
            negated: *negated,
            span: *span,
        },
        other => other.clone(),
    }
}
