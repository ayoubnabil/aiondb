//! Pure types and parsers for PostgreSQL-style advisory lock calls
//! (`pg_advisory_lock`, `pg_advisory_unlock`, `pg_try_advisory_*`,
//! `pg_advisory_unlock_all`). The engine owns the per-session lock
//! registry and conflict detection; this module only classifies the
//! function calls and their arguments.

use aiondb_parser::{Expr, Literal, SelectStatement, UnaryOperator};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CompatAdvisoryMode {
    Exclusive,
    Shared,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CompatAdvisoryScope {
    Session,
    Transaction,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum CompatAdvisoryResource {
    Int8(i64),
    Int4Pair(i32, i32),
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct CompatAdvisoryKey {
    pub resource: CompatAdvisoryResource,
    pub mode: CompatAdvisoryMode,
}

#[derive(Clone, Copy, Debug)]
pub enum CompatAdvisoryOperation {
    Lock {
        mode: CompatAdvisoryMode,
        scope: CompatAdvisoryScope,
        try_only: bool,
    },
    Unlock {
        mode: CompatAdvisoryMode,
    },
    UnlockAll,
}

pub struct ParsedCompatAdvisorySelectItem {
    pub column_name: String,
    pub operation: CompatAdvisoryOperation,
    pub resource: Option<CompatAdvisoryResource>,
}

pub fn advisory_operation_for_function_name(name: &str) -> Option<CompatAdvisoryOperation> {
    match name {
        "pg_advisory_lock" => Some(CompatAdvisoryOperation::Lock {
            mode: CompatAdvisoryMode::Exclusive,
            scope: CompatAdvisoryScope::Session,
            try_only: false,
        }),
        "pg_advisory_lock_shared" => Some(CompatAdvisoryOperation::Lock {
            mode: CompatAdvisoryMode::Shared,
            scope: CompatAdvisoryScope::Session,
            try_only: false,
        }),
        "pg_advisory_xact_lock" => Some(CompatAdvisoryOperation::Lock {
            mode: CompatAdvisoryMode::Exclusive,
            scope: CompatAdvisoryScope::Transaction,
            try_only: false,
        }),
        "pg_advisory_xact_lock_shared" => Some(CompatAdvisoryOperation::Lock {
            mode: CompatAdvisoryMode::Shared,
            scope: CompatAdvisoryScope::Transaction,
            try_only: false,
        }),
        "pg_try_advisory_lock" => Some(CompatAdvisoryOperation::Lock {
            mode: CompatAdvisoryMode::Exclusive,
            scope: CompatAdvisoryScope::Session,
            try_only: true,
        }),
        "pg_try_advisory_lock_shared" => Some(CompatAdvisoryOperation::Lock {
            mode: CompatAdvisoryMode::Shared,
            scope: CompatAdvisoryScope::Session,
            try_only: true,
        }),
        "pg_try_advisory_xact_lock" => Some(CompatAdvisoryOperation::Lock {
            mode: CompatAdvisoryMode::Exclusive,
            scope: CompatAdvisoryScope::Transaction,
            try_only: true,
        }),
        "pg_try_advisory_xact_lock_shared" => Some(CompatAdvisoryOperation::Lock {
            mode: CompatAdvisoryMode::Shared,
            scope: CompatAdvisoryScope::Transaction,
            try_only: true,
        }),
        "pg_advisory_unlock" => Some(CompatAdvisoryOperation::Unlock {
            mode: CompatAdvisoryMode::Exclusive,
        }),
        "pg_advisory_unlock_shared" => Some(CompatAdvisoryOperation::Unlock {
            mode: CompatAdvisoryMode::Shared,
        }),
        "pg_advisory_unlock_all" => Some(CompatAdvisoryOperation::UnlockAll),
        _ => None,
    }
}

pub fn advisory_function_name(expr: &Expr) -> Option<String> {
    let Expr::FunctionCall { name, .. } = expr else {
        return None;
    };
    match name.parts.as_slice() {
        [function_name] => Some(function_name.to_ascii_lowercase()),
        [schema_name, function_name] if schema_name.eq_ignore_ascii_case("pg_catalog") => {
            Some(function_name.to_ascii_lowercase())
        }
        _ => None,
    }
}

pub fn advisory_i64_from_expr(expr: &Expr) -> Option<i64> {
    match expr {
        Expr::Literal(Literal::Integer(value), _) => Some(*value),
        Expr::UnaryOp {
            op: UnaryOperator::Minus,
            expr,
            ..
        } => advisory_i64_from_expr(expr).map(i64::saturating_neg),
        _ => None,
    }
}

pub fn advisory_resource_from_args(args: &[Expr]) -> Option<CompatAdvisoryResource> {
    match args {
        [arg] => Some(CompatAdvisoryResource::Int8(advisory_i64_from_expr(arg)?)),
        [first, second] => {
            let first = i32::try_from(advisory_i64_from_expr(first)?).ok()?;
            let second = i32::try_from(advisory_i64_from_expr(second)?).ok()?;
            Some(CompatAdvisoryResource::Int4Pair(first, second))
        }
        _ => None,
    }
}

pub fn advisory_select_column_name(item: &aiondb_parser::SelectItem, default_name: &str) -> String {
    item.alias
        .clone()
        .unwrap_or_else(|| default_name.to_owned())
}

pub fn parse_compat_advisory_select(
    select: &SelectStatement,
) -> Option<Vec<ParsedCompatAdvisorySelectItem>> {
    if !matches!(select.distinct, aiondb_parser::DistinctKind::All)
        || !select.ctes.is_empty()
        || select.from.is_some()
        || !select.joins.is_empty()
        || select.selection.is_some()
        || !select.group_by.is_empty()
        || !select.group_by_items.is_empty()
        || select.having.is_some()
        || !select.window_definitions.is_empty()
        || !select.order_by.is_empty()
        || select.limit.is_some()
        || select.offset.is_some()
    {
        return None;
    }

    let mut parsed = Vec::with_capacity(select.items.len());
    for item in &select.items {
        let Expr::FunctionCall {
            args,
            distinct,
            filter,
            name,
            ..
        } = &item.expr
        else {
            return None;
        };
        if *distinct || filter.is_some() {
            return None;
        }
        let function_name = advisory_function_name(&item.expr)?;
        let operation = advisory_operation_for_function_name(&function_name)?;
        let resource = match operation {
            CompatAdvisoryOperation::UnlockAll => {
                if !args.is_empty() {
                    return None;
                }
                None
            }
            CompatAdvisoryOperation::Lock { .. } | CompatAdvisoryOperation::Unlock { .. } => {
                Some(advisory_resource_from_args(args)?)
            }
        };
        let default_name = name
            .parts
            .last()
            .map_or(function_name.as_str(), String::as_str);
        parsed.push(ParsedCompatAdvisorySelectItem {
            column_name: advisory_select_column_name(item, default_name),
            operation,
            resource,
        });
    }
    Some(parsed)
}

pub fn compat_select_only_uses_advisory_locks(select: &SelectStatement) -> bool {
    parse_compat_advisory_select(select).is_some()
}
