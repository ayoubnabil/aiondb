#![allow(clippy::duplicate_mod)]

pub(crate) use crate::*;

mod basic_parse;
mod cast_case;
mod cypher;
mod functions;
mod if_exists_guards;
mod noop_stmts;
mod pg_compat_regressions;
mod postfix_cast;
mod recursive_cte_parse;
mod returning;
mod schema;
mod select_and_exprs;
mod tx_ddl_dml;
