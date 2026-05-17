#![allow(clippy::pedantic)]

//! Translates Cypher AST statements to equivalent SQL strings.
//!
//! Graph CREATE and MATCH support: nodes are stored in a single SQL table
//! `__cypher_nodes` with columns `__id SERIAL`, `__labels TEXT[]`,
//! and `__props JSONB`.

mod call_translate;
mod create_translate;
pub(crate) mod escape;
pub(crate) mod expr;
mod match_translate;
mod mutate_translate;
mod return_translate;

use aiondb_parser::cypher_ast::{CypherClause, CypherStatement};
use std::fmt;
use tracing::warn;

/// Maximum SQL string length for Cypher-to-SQL translation.
/// Prevents stack overflow in the SQL planner for deeply nested subqueries.
/// Value chosen empirically: queries above this threshold typically contain
/// deeply nested quantifier expressions that exceed the planner's recursion limit.
const MAX_GENERATED_SQL_LEN: usize = 32768;

/// Maximum number of SELECT sub-statements allowed in generated SQL.
/// Guards against pathological Cypher patterns that expand into O(2^n) SQL
/// sub-queries (e.g. deeply nested OPTIONAL MATCH chains). The value of 32
/// accommodates realistic queries while preventing runaway compilation times.
const MAX_SUBQUERY_DEPTH: usize = 32;

/// Table name for the Cypher node store. All graph nodes are stored here
/// with labels as TEXT[] and properties as JSONB.
pub(crate) const CYPHER_NODES_TABLE: &str = "__cypher_nodes";

/// Table name for the Cypher edge (relationship) store. All graph edges are
/// stored here with type, source/target node IDs, and properties as JSONB.
pub(crate) const CYPHER_EDGES_TABLE: &str = "__cypher_edges";
pub(crate) const UNSUPPORTED_CYPHER_PATTERN_COMPREHENSION_SENTINEL: &str =
    "__aiondb_unsupported_cypher_pattern_comprehension__";

// ── Error type ─────────────────────────────────────────────────────

/// Errors produced when translating Cypher AST to SQL.
#[derive(Debug, Clone)]
pub(crate) enum CypherTranslateError {
    /// Feature not yet supported (e.g., variable-length paths).
    Unsupported(String),
    /// Generated SQL exceeds complexity limits.
    TooComplex,
    /// Cypher semantic error (e.g., invalid UNION).
    SemanticError(String),
}

impl fmt::Display for CypherTranslateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CypherTranslateError::Unsupported(msg) => write!(f, "{msg}"),
            CypherTranslateError::TooComplex => {
                write!(f, "generated SQL too complex for in-memory execution")
            }
            CypherTranslateError::SemanticError(msg) => write!(f, "{msg}"),
        }
    }
}

// ── Match context ──────────────────────────────────────────────────

/// Context for translating MATCH expressions to SQL.
pub(crate) struct MatchContext {
    /// Node variable names bound in the MATCH pattern.
    pub node_vars: Vec<String>,
    /// Relationship variable names bound in the MATCH pattern.
    pub rel_vars: Vec<String>,
    /// FROM clause parts (table aliases).
    pub from_parts: Vec<String>,
    /// JOIN clause parts.
    pub join_parts: Vec<String>,
    /// WHERE clause conditions.
    pub where_conditions: Vec<String>,
}

impl MatchContext {
    pub(crate) fn new() -> Self {
        MatchContext {
            node_vars: Vec::new(),
            rel_vars: Vec::new(),
            from_parts: Vec::new(),
            join_parts: Vec::new(),
            where_conditions: Vec::new(),
        }
    }

    /// Combined node + relationship variable names.
    pub(crate) fn all_vars(&self) -> Vec<String> {
        self.node_vars
            .iter()
            .chain(self.rel_vars.iter())
            .cloned()
            .collect()
    }

    /// Build the FROM ... JOIN ... WHERE ... fragment.
    #[allow(clippy::wrong_self_convention)]
    pub(crate) fn from_where_sql(&self) -> String {
        let mut sql = self.from_parts.join(", ");
        for join_part in &self.join_parts {
            sql.push(' ');
            sql.push_str(join_part);
        }
        if !self.where_conditions.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&self.where_conditions.join(" AND "));
        }
        sql
    }
}

// ── Main entry point ───────────────────────────────────────────────

pub(crate) fn cypher_to_sql(stmt: &CypherStatement) -> Result<String, CypherTranslateError> {
    if let Some(sql) = create_translate::translate_create_only(&stmt.clauses) {
        return Ok(sql);
    }
    let mut sql = return_translate::translate_clauses(&stmt.clauses).ok_or_else(|| {
        warn!("cypher translate: no clause handler matched the statement");
        CypherTranslateError::Unsupported(
            "this Cypher statement requires graph pattern matching which is not yet supported"
                .into(),
        )
    })?;
    if sql.contains(UNSUPPORTED_CYPHER_PATTERN_COMPREHENSION_SENTINEL) {
        return Err(CypherTranslateError::Unsupported(
            "Cypher pattern comprehensions are not supported by the SQL fallback path".into(),
        ));
    }
    if sql.len() > MAX_GENERATED_SQL_LEN {
        return Err(CypherTranslateError::TooComplex);
    }
    let select_count = sql.matches("SELECT ").count() + sql.matches("select ").count();
    if select_count > MAX_SUBQUERY_DEPTH {
        return Err(CypherTranslateError::TooComplex);
    }
    if let Some(ref union) = stmt.union {
        return_translate::validate_union_consistency(stmt)?;
        return_translate::validate_union_columns(stmt)?;
        let kw = if union.all { " UNION ALL " } else { " UNION " };
        let right = cypher_to_sql(&union.right)?;
        sql.push_str(kw);
        sql.push_str(&right);
    }
    let needs_ddl = stmt.clauses.iter().any(|c| {
        matches!(
            c,
            CypherClause::Match(_)
                | CypherClause::Merge(_)
                | CypherClause::Set(_)
                | CypherClause::Delete(_)
                | CypherClause::Remove(_)
        )
    });
    if needs_ddl {
        let has_rels = stmt.clauses.iter().any(|c| {
            if let CypherClause::Match(m) = c {
                m.patterns.iter().any(|p| !p.rels.is_empty())
            } else {
                false
            }
        });
        let has_detach = stmt
            .clauses
            .iter()
            .any(|c| matches!(c, CypherClause::Delete(d) if d.detach));
        let ddl = cypher_nodes_ddl();
        if has_rels || has_detach {
            let eddl = cypher_edges_ddl();
            sql = format!("{ddl}; {eddl}; {sql}");
        } else {
            sql = format!("{ddl}; {sql}");
        }
    }
    Ok(sql)
}

fn cypher_nodes_ddl() -> String {
    format!(
        "CREATE TABLE IF NOT EXISTS \"{CYPHER_NODES_TABLE}\" (\
         \"__id\" SERIAL, \
         \"__labels\" TEXT[] DEFAULT ARRAY[]::TEXT[], \
         \"__props\" JSONB DEFAULT '{{}}'::JSONB); \
         CREATE INDEX IF NOT EXISTS idx_cypher_nodes_labels \
         ON \"{CYPHER_NODES_TABLE}\" USING gin (\"__labels\"); \
         CREATE INDEX IF NOT EXISTS idx_cypher_nodes_props \
         ON \"{CYPHER_NODES_TABLE}\" USING gin (\"__props\" jsonb_path_ops)"
    )
}

fn cypher_edges_ddl() -> String {
    format!(
        "CREATE TABLE IF NOT EXISTS \"{CYPHER_EDGES_TABLE}\" (\
         \"__id\" SERIAL, \
         \"__type\" TEXT, \
         \"__source\" INTEGER, \
         \"__target\" INTEGER, \
         \"__props\" JSONB DEFAULT '{{}}'::JSONB); \
         CREATE INDEX IF NOT EXISTS idx_cypher_edges_source \
         ON \"{CYPHER_EDGES_TABLE}\" (\"__source\"); \
         CREATE INDEX IF NOT EXISTS idx_cypher_edges_target \
         ON \"{CYPHER_EDGES_TABLE}\" (\"__target\"); \
         CREATE INDEX IF NOT EXISTS idx_cypher_edges_type \
         ON \"{CYPHER_EDGES_TABLE}\" (\"__type\")"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: parse a Cypher statement and translate it to SQL.
    fn translate(cypher: &str) -> Result<String, CypherTranslateError> {
        let stmt = aiondb_parser::parse_cypher(cypher).expect("Cypher parse failed");
        cypher_to_sql(&stmt)
    }

    #[test]
    fn pattern_comprehension_is_rejected_by_sql_fallback() {
        let err = translate("MATCH (n) RETURN [(n)-[:KNOWS]->(m) | m.name] AS friends")
            .expect_err("pattern comprehension must not silently translate through SQL fallback");
        assert!(matches!(err, CypherTranslateError::Unsupported(_)));
        assert!(
            err.to_string()
                .contains("Cypher pattern comprehensions are not supported by the SQL fallback path")
        );
    }

    // ── RETURN-only (pure expressions) ──────────────────────────────

    #[test]
    fn return_literal_integer() {
        let sql = translate("RETURN 1 AS x").unwrap();
        assert!(sql.contains("SELECT"), "expected SELECT in: {sql}");
        assert!(sql.contains('1'));
    }

    #[test]
    fn debug_with_collect_alias() {
        use crate::engine::api::QueryEngine;
        use aiondb_catalog_store::CatalogStore;
        use aiondb_storage_engine::InMemoryStorage;
        use std::sync::Arc;
        let q = "UNWIND [true, false, null] AS a \
                 UNWIND [true, false, null] AS b \
                 UNWIND [true, false, null] AS c \
                 WITH collect((a OR b XOR c) = (a OR (b XOR c))) AS eq, \
                      collect((a OR b XOR c) <> ((a OR b) XOR c)) AS neq \
                 RETURN all(x IN eq WHERE x) AND any(x IN neq WHERE x) AS result";
        let sql = translate(q).unwrap();
        assert!(
            sql.contains("collect"),
            "expected collect rewrite in: {sql}"
        );
        let catalog = Arc::new(CatalogStore::new());
        let storage = Arc::new(InMemoryStorage::new_without_wal());
        let mut runtime = aiondb_config::RuntimeConfig::default();
        runtime.limits.statement_timeout = std::time::Duration::from_secs(30);
        let engine = crate::EngineBuilder::for_testing()
            .with_runtime_config(runtime)
            .with_catalog_txn(catalog.clone())
            .with_catalog_reader(catalog.clone())
            .with_catalog_writer(catalog.clone())
            .with_sequence_manager(catalog)
            .with_storage_ddl(storage.clone())
            .with_storage_dml(storage.clone())
            .with_storage_txn(storage)
            .build()
            .expect("engine");
        let (sess, _) = engine
            .startup(crate::StartupParams {
                database: "default".into(),
                application_name: Some("dbg".into()),
                options: std::collections::BTreeMap::new(),
                credential: crate::Credential::Anonymous {
                    user: "alice".into(),
                },
                transport: crate::TransportInfo::in_process(),
            })
            .expect("startup");
        let error = engine
            .execute_sql(&sess, q)
            .expect_err("collect rewrite is not executable until Cypher aggregates are registered");
        assert!(
            error.to_string().contains("collect"),
            "expected collect aggregate bind error, got {error}"
        );
    }

    #[test]
    fn cypher_date_map_routes_to_cypher_date() {
        // Regression: `date({year:...})` must resolve to `cypher_date`, not
        // PG's `date(text)` cast. Both the literal and the WITH-bound form
        // need to emit the quoted `cypher_date` constructor.
        let sql = translate("RETURN date({year: 1984, month: 11, day: 11}) AS result").unwrap();
        assert!(
            sql.contains("\"cypher_date\""),
            "expected cypher_date in: {sql}"
        );
        let sql2 = translate(
            "WITH date({year: 1984, month: 11, day: 11}) AS other RETURN date(other) AS result",
        )
        .unwrap();
        assert!(
            sql2.contains("\"cypher_date\""),
            "expected cypher_date in: {sql2}"
        );
        // Ensure the outer `date(other)` is also mangled (scope path).
        let outer_call = sql2
            .find("AS \"result\"")
            .map(|i| &sql2[..i])
            .unwrap_or(&sql2);
        assert!(
            outer_call.matches("\"cypher_date\"").count() >= 2,
            "outer date(other) must also use cypher_date: {sql2}"
        );
    }

    #[test]
    fn return_string_literal() {
        let sql = translate("RETURN 'hello' AS greeting").unwrap();
        assert!(sql.contains("hello"), "expected 'hello' in: {sql}");
    }

    #[test]
    fn return_with_alias() {
        let sql = translate("RETURN 1 + 2 AS result").unwrap();
        assert!(sql.contains("\"result\""), "expected alias in: {sql}");
    }

    // ── CREATE (node-only) ──────────────────────────────────────────

    #[test]
    fn create_single_node() {
        let sql = translate("CREATE (n:Person {name: 'Alice'})").unwrap();
        assert!(sql.contains("INSERT INTO"), "expected INSERT in: {sql}");
        assert!(
            sql.contains("__cypher_nodes"),
            "expected nodes table in: {sql}"
        );
        assert!(sql.contains("Person"), "expected label Person in: {sql}");
    }

    #[test]
    fn create_node_with_relationship() {
        let sql = translate("CREATE (a:Person)-[:KNOWS]->(b:Person)").unwrap();
        assert!(
            sql.contains("__cypher_nodes"),
            "expected nodes table in: {sql}"
        );
        assert!(
            sql.contains("__cypher_edges"),
            "expected edges table in: {sql}"
        );
        assert!(sql.contains("KNOWS"), "expected rel type KNOWS in: {sql}");
    }

    // ── MATCH with relationships (JOIN generation) ──────────────────

    #[test]
    fn match_node_only() {
        let sql = translate("MATCH (n:Person) RETURN n.name").unwrap();
        assert!(
            sql.contains("__cypher_nodes"),
            "expected nodes table in: {sql}"
        );
        assert!(sql.contains("Person"), "expected label filter in: {sql}");
        assert!(sql.contains("SELECT"), "expected SELECT in: {sql}");
    }

    #[test]
    fn match_with_relationship_generates_join() {
        let sql = translate("MATCH (a:Person)-[:KNOWS]->(b:Person) RETURN a, b").unwrap();
        assert!(
            sql.contains("JOIN"),
            "expected JOIN for relationship in: {sql}"
        );
        assert!(
            sql.contains("__cypher_edges"),
            "expected edges table in: {sql}"
        );
    }

    #[test]
    fn match_with_where_clause() {
        let sql = translate("MATCH (n:Person) WHERE n.age > 30 RETURN n.name").unwrap();
        assert!(sql.contains("Person"), "expected label in: {sql}");
        assert!(sql.contains("30"), "expected WHERE value in: {sql}");
    }

    // ── SET property (UPDATE SQL) ───────────────────────────────────

    #[test]
    fn match_set_property() {
        let sql = translate("MATCH (n:Person) SET n.age = 42").unwrap();
        assert!(sql.contains("UPDATE"), "expected UPDATE in: {sql}");
        assert!(sql.contains("jsonb_set"), "expected jsonb_set in: {sql}");
        assert!(sql.contains("age"), "expected property 'age' in: {sql}");
    }

    #[test]
    fn match_set_null_deletes_property() {
        let sql = translate("MATCH (n:Person) SET n.age = null").unwrap();
        assert!(sql.contains("UPDATE"), "expected UPDATE in: {sql}");
        assert!(
            sql.contains("jsonb_delete"),
            "expected jsonb_delete for null SET in: {sql}"
        );
    }

    // ── DELETE (DELETE SQL) ─────────────────────────────────────────

    #[test]
    fn match_delete_node() {
        let sql = translate("MATCH (n:Temp) DELETE n").unwrap();
        assert!(sql.contains("DELETE FROM"), "expected DELETE in: {sql}");
        assert!(
            sql.contains("__cypher_nodes"),
            "expected nodes table in: {sql}"
        );
    }

    #[test]
    fn match_detach_delete() {
        let sql = translate("MATCH (n:Temp) DETACH DELETE n").unwrap();
        assert!(sql.contains("DELETE FROM"), "expected DELETE in: {sql}");
        assert!(
            sql.contains("__cypher_edges"),
            "expected edges cleanup in: {sql}"
        );
    }

    // ── MERGE (INSERT WHERE NOT EXISTS) ─────────────────────────────

    #[test]
    fn merge_node() {
        let sql = translate("MERGE (n:Person {name: 'Alice'})").unwrap();
        assert!(sql.contains("INSERT INTO"), "expected INSERT in: {sql}");
        assert!(
            sql.contains("NOT EXISTS"),
            "expected NOT EXISTS guard in: {sql}"
        );
        assert!(sql.contains("Person"), "expected label in: {sql}");
    }

    #[test]
    fn merge_with_on_create_set() {
        let sql = translate("MERGE (n:Person {name: 'Alice'}) ON CREATE SET n.age = 30").unwrap();
        assert!(sql.contains("INSERT INTO"), "expected INSERT in: {sql}");
        assert!(
            sql.contains("UPDATE"),
            "expected ON CREATE UPDATE in: {sql}"
        );
    }

    // ── Complex WITH pipeline ───────────────────────────────────────

    #[test]
    fn with_pipeline() {
        let sql = translate("WITH 1 AS x RETURN x").unwrap();
        assert!(sql.contains("SELECT"), "expected SELECT in: {sql}");
    }

    #[test]
    fn unwind_return() {
        let sql = translate("UNWIND [1, 2, 3] AS x RETURN x").unwrap();
        assert!(
            sql.contains("unnest"),
            "expected unnest for UNWIND in: {sql}"
        );
    }

    #[test]
    fn with_where_filters() {
        let sql = translate("WITH 10 AS x WHERE x > 5 RETURN x").unwrap();
        assert!(sql.contains("WHERE"), "expected WHERE from WITH in: {sql}");
    }

    // ── Error / edge cases ──────────────────────────────────────────

    #[test]
    fn variable_length_path_does_not_panic() {
        let result = translate("MATCH (a)-[:R*1..5]->(b) RETURN a, b");
        assert!(result.is_ok() || result.is_err());
    }

    #[test]
    fn unsupported_does_not_panic() {
        let result = translate("CALL db.labels()");
        assert!(result.is_ok() || result.is_err());
    }

    // ── UNION ───────────────────────────────────────────────────────

    #[test]
    fn union_all() {
        let sql = translate("RETURN 1 AS x UNION ALL RETURN 2 AS x").unwrap();
        assert!(sql.contains("UNION ALL"), "expected UNION ALL in: {sql}");
    }

    #[test]
    fn union_column_mismatch_errors() {
        let result = translate("RETURN 1 AS x UNION RETURN 2 AS y");
        assert!(result.is_err(), "mismatched UNION columns should error");
    }

    // ── DDL generation ──────────────────────────────────────────────

    #[test]
    fn match_generates_ddl_prefix() {
        let sql = translate("MATCH (n:Person) RETURN n.name").unwrap();
        assert!(
            sql.contains("CREATE TABLE IF NOT EXISTS"),
            "MATCH should prepend DDL: {sql}"
        );
    }

    #[test]
    fn return_only_no_ddl() {
        let sql = translate("RETURN 42 AS answer").unwrap();
        assert!(
            !sql.contains("CREATE TABLE IF NOT EXISTS"),
            "pure RETURN should not have DDL: {sql}"
        );
    }
}
