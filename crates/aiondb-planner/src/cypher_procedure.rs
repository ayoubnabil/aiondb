//! Shared native Cypher procedure resolution.

use aiondb_core::{DbError, DbResult};

pub(crate) struct ResolvedCypherProcedure {
    pub name: String,
    pub yields: Vec<String>,
}

pub(crate) fn resolve_graph_procedure_call(
    procedure: &str,
    requested_yields: &[String],
    arg_count: usize,
) -> DbResult<Option<ResolvedCypherProcedure>> {
    let Some(info) = aiondb_graph::algorithms::procedures::procedure_info(procedure) else {
        return Ok(None);
    };

    if arg_count > info.args.len() {
        if info.args.is_empty() {
            return Err(DbError::syntax_error(format!(
                "CALL {procedure} does not accept algorithm config arguments"
            )));
        }
        return Err(DbError::syntax_error(format!(
            "CALL {procedure} accepts at most {} algorithm config arguments",
            info.args.len()
        )));
    }

    for requested in requested_yields {
        if !info
            .yields
            .iter()
            .any(|(available, _)| available.eq_ignore_ascii_case(requested))
        {
            return Err(DbError::syntax_error(format!(
                "CALL {procedure} cannot YIELD `{requested}`"
            )));
        }
    }

    let yields = if requested_yields.is_empty() {
        info.yields.iter().map(|(name, _)| name.clone()).collect()
    } else {
        requested_yields.to_vec()
    };

    Ok(Some(ResolvedCypherProcedure {
        name: info.name,
        yields,
    }))
}
