//! CALL procedure clause translation.

use aiondb_parser::ast::{Expr, Literal};
use aiondb_parser::cypher_ast::CypherClause;

use super::escape::qi;
use super::expr::expr_to_sql_plain;
use super::return_translate::translate_return_from_source;

/// Translate CALL procedure patterns to SQL.
pub(crate) fn translate_call_pipeline(clauses: &[CypherClause]) -> Option<String> {
    let call_clause = clauses.iter().find_map(|c| {
        if let CypherClause::Call(call) = c {
            Some(call)
        } else {
            None
        }
    })?;
    if call_clause.subquery.is_some() {
        return None;
    }

    if clauses.iter().any(|c| matches!(c, CypherClause::Match(_))) {
        return None;
    }

    let proc_table = format!(
        "\"__cypher_proc_{}\"",
        call_clause.procedure.replace('.', "_").replace('"', "\"\"")
    );

    let where_clause = if call_clause.args.is_empty() {
        String::new()
    } else {
        let conds: Vec<String> = call_clause
            .args
            .iter()
            .enumerate()
            .map(|(i, arg)| {
                if matches!(arg, Expr::Literal(Literal::Null, _)) {
                    format!("\"__param_{i}\" IS NULL")
                } else {
                    let val = expr_to_sql_plain(arg);
                    format!("\"__param_{i}\" = ({val})::TEXT")
                }
            })
            .collect();
        format!(" WHERE {}", conds.join(" AND "))
    };

    let has_return = clauses.iter().any(|c| matches!(c, CypherClause::Return(_)));

    if !has_return {
        if call_clause.yields.is_empty() {
            return Some(format!("SELECT * FROM {proc_table}{where_clause}"));
        }
        let cols: Vec<String> = call_clause.yields.iter().map(|y| qi(y)).collect();
        return Some(format!(
            "SELECT {} FROM {proc_table}{where_clause}",
            cols.join(", ")
        ));
    }

    if let Some(ret) = clauses.iter().rev().find_map(|c| {
        if let CypherClause::Return(r) = c {
            Some(r)
        } else {
            None
        }
    }) {
        if !call_clause.yields.is_empty() {
            let cols: Vec<String> = call_clause.yields.iter().map(|y| qi(y)).collect();
            let source = format!("SELECT {} FROM {proc_table}{where_clause}", cols.join(", "));
            return translate_return_from_source(ret, &source);
        }
    }

    None
}
