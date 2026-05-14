#![allow(clippy::redundant_closure_for_method_calls)]

use super::*;
use crate::span::Span;

fn s(start: usize, end: usize) -> Span {
    Span::new(start, end)
}

fn obj(parts: &[&str], span: Span) -> ObjectName {
    ObjectName {
        parts: parts.iter().map(|s| s.to_string()).collect(),
        span,
    }
}

fn lit_int(v: i64, span: Span) -> Expr {
    Expr::Literal(Literal::Integer(v), span)
}

fn lit_str(v: &str, span: Span) -> Expr {
    Expr::Literal(Literal::String(v.to_string()), span)
}

fn lit_bool(v: bool, span: Span) -> Expr {
    Expr::Literal(Literal::Boolean(v), span)
}

fn lit_null(span: Span) -> Expr {
    Expr::Literal(Literal::Null, span)
}

fn ident(parts: &[&str], span: Span) -> Expr {
    Expr::Identifier(obj(parts, span))
}

mod create_drop;
mod dml_structs;
mod edge_cases;
mod literals_and_exprs;
mod operators_and_items;
mod select_structs;
mod statement_enum;
