#![allow(clippy::redundant_closure_for_method_calls)]

use crate::{ast::*, lexer, Parser};

fn parse_cypher(sql: &str) -> CypherStatement {
    let tokens = lexer::lex_sql(sql).unwrap();
    let stmt = Parser::new(tokens).parse_statements().unwrap();
    assert_eq!(stmt.len(), 1, "expected exactly one statement");
    match stmt.into_iter().next().unwrap() {
        Statement::Cypher(c) => c,
        other => panic!("expected Cypher statement, got {other:?}"),
    }
}

fn parse_cypher_err(sql: &str) -> String {
    let tokens = lexer::lex_sql(sql).unwrap();
    Parser::new(tokens)
        .parse_statements()
        .unwrap_err()
        .to_string()
}

// -----------------------------------------------------------------------
// Simple MATCH ... RETURN
// -----------------------------------------------------------------------

#[test]
fn simple_match_return() {
    let stmt = parse_cypher("MATCH (n:Person) RETURN n");
    assert_eq!(stmt.clauses.len(), 2);

    // MATCH clause
    let m = match &stmt.clauses[0] {
        CypherClause::Match(m) => m,
        other => panic!("expected Match, got {other:?}"),
    };
    assert!(!m.optional);
    assert_eq!(m.patterns.len(), 1);
    assert_eq!(m.patterns[0].nodes.len(), 1);
    assert_eq!(m.patterns[0].nodes[0].variable.as_deref(), Some("n"));
    assert_eq!(
        m.patterns[0].nodes[0].labels.first().map(|s| s.as_str()),
        Some("Person")
    );
    assert!(m.where_clause.is_none());

    // RETURN clause
    let r = match &stmt.clauses[1] {
        CypherClause::Return(r) => r,
        other => panic!("expected Return, got {other:?}"),
    };
    assert!(!r.distinct);
    assert_eq!(r.items.len(), 1);
}

// -----------------------------------------------------------------------
// MATCH with relationship
// -----------------------------------------------------------------------

#[test]
fn match_with_relationship() {
    let stmt = parse_cypher("MATCH (a)-[r:KNOWS]->(b) RETURN a, b");
    let m = match &stmt.clauses[0] {
        CypherClause::Match(m) => m,
        other => panic!("expected Match, got {other:?}"),
    };
    assert_eq!(m.patterns[0].nodes.len(), 2);
    assert_eq!(m.patterns[0].rels.len(), 1);
    let rel = &m.patterns[0].rels[0];
    assert_eq!(rel.variable.as_deref(), Some("r"));
    assert_eq!(rel.rel_type.as_deref(), Some("KNOWS"));
    assert_eq!(rel.direction, CypherDirection::Outgoing);

    // RETURN has 2 items
    let r = match &stmt.clauses[1] {
        CypherClause::Return(r) => r,
        other => panic!("expected Return, got {other:?}"),
    };
    assert_eq!(r.items.len(), 2);
}

#[test]
fn match_with_relationship_type_alternatives() {
    let stmt = parse_cypher("MATCH (a)-[r:KNOWS|LIKES]->(b) RETURN b");
    let m = match &stmt.clauses[0] {
        CypherClause::Match(m) => m,
        other => panic!("expected Match, got {other:?}"),
    };
    let rel = &m.patterns[0].rels[0];
    assert_eq!(rel.variable.as_deref(), Some("r"));
    assert_eq!(rel.rel_type.as_deref(), Some("KNOWS"));
    assert_eq!(rel.rel_types_alt, vec!["likes"]);
    assert_eq!(rel.direction, CypherDirection::Outgoing);
}

#[test]
fn match_named_path_variable() {
    let stmt = parse_cypher("MATCH p = (a)-[r:KNOWS]->(b) RETURN p");
    let m = match &stmt.clauses[0] {
        CypherClause::Match(m) => m,
        other => panic!("expected Match, got {other:?}"),
    };
    assert_eq!(m.patterns[0].path_variable.as_deref(), Some("p"));
    assert_eq!(m.patterns[0].nodes.len(), 2);
    assert_eq!(m.patterns[0].rels.len(), 1);
    assert_eq!(m.patterns[0].rels[0].variable.as_deref(), Some("r"));
}

// -----------------------------------------------------------------------
// Variable-length paths
// -----------------------------------------------------------------------

#[test]
fn variable_length_path() {
    let stmt = parse_cypher("MATCH (a)-[*1..3]->(b) RETURN a, b");
    let m = match &stmt.clauses[0] {
        CypherClause::Match(m) => m,
        other => panic!("expected Match, got {other:?}"),
    };
    let rel = &m.patterns[0].rels[0];
    assert!(rel.variable.is_none());
    assert!(rel.rel_type.is_none());
    assert_eq!(rel.min_hops, Some(1));
    assert_eq!(rel.max_hops, Some(3));
    assert_eq!(rel.direction, CypherDirection::Outgoing);
}

#[test]
fn variable_length_unbounded() {
    let stmt = parse_cypher("MATCH (a)-[*]->(b) RETURN a");
    let rel = &match &stmt.clauses[0] {
        CypherClause::Match(m) => m,
        other => panic!("expected Match, got {other:?}"),
    }
    .patterns[0]
        .rels[0];
    assert!(rel.min_hops.is_none());
    assert!(rel.max_hops.is_none());
}

#[test]
fn variable_length_min_only() {
    let stmt = parse_cypher("MATCH (a)-[*2..]->(b) RETURN a");
    let rel = &match &stmt.clauses[0] {
        CypherClause::Match(m) => m,
        other => panic!("expected Match, got {other:?}"),
    }
    .patterns[0]
        .rels[0];
    assert_eq!(rel.min_hops, Some(2));
    assert!(rel.max_hops.is_none());
}

#[test]
fn variable_length_max_only() {
    let stmt = parse_cypher("MATCH (a)-[*..5]->(b) RETURN a");
    let rel = &match &stmt.clauses[0] {
        CypherClause::Match(m) => m,
        other => panic!("expected Match, got {other:?}"),
    }
    .patterns[0]
        .rels[0];
    assert!(rel.min_hops.is_none());
    assert_eq!(rel.max_hops, Some(5));
}

#[test]
fn variable_length_exact() {
    let stmt = parse_cypher("MATCH (a)-[*3]->(b) RETURN a");
    let rel = &match &stmt.clauses[0] {
        CypherClause::Match(m) => m,
        other => panic!("expected Match, got {other:?}"),
    }
    .patterns[0]
        .rels[0];
    assert_eq!(rel.min_hops, Some(3));
    assert_eq!(rel.max_hops, Some(3));
}

// -----------------------------------------------------------------------
// OPTIONAL MATCH
// -----------------------------------------------------------------------

#[test]
fn optional_match() {
    let stmt = parse_cypher("OPTIONAL MATCH (n:Person) RETURN n");
    let m = match &stmt.clauses[0] {
        CypherClause::Match(m) => m,
        other => panic!("expected Match, got {other:?}"),
    };
    assert!(m.optional);
}

// -----------------------------------------------------------------------
// WHERE clause
// -----------------------------------------------------------------------

#[test]
fn match_with_where() {
    let stmt = parse_cypher("MATCH (n:Person) WHERE n.age > 30 RETURN n");
    let m = match &stmt.clauses[0] {
        CypherClause::Match(m) => m,
        other => panic!("expected Match, got {other:?}"),
    };
    assert!(m.where_clause.is_some());
}

// -----------------------------------------------------------------------
// Directions
// -----------------------------------------------------------------------

#[test]
fn incoming_relationship() {
    let stmt = parse_cypher("MATCH (a)<-[r:KNOWS]-(b) RETURN a");
    let rel = &match &stmt.clauses[0] {
        CypherClause::Match(m) => m,
        other => panic!("expected Match, got {other:?}"),
    }
    .patterns[0]
        .rels[0];
    assert_eq!(rel.direction, CypherDirection::Incoming);
}

#[test]
fn undirected_relationship() {
    let stmt = parse_cypher("MATCH (a)-[r:KNOWS]-(b) RETURN a");
    let rel = &match &stmt.clauses[0] {
        CypherClause::Match(m) => m,
        other => panic!("expected Match, got {other:?}"),
    }
    .patterns[0]
        .rels[0];
    assert_eq!(rel.direction, CypherDirection::Both);
}

// -----------------------------------------------------------------------
// CREATE
// -----------------------------------------------------------------------

#[test]
fn create_node() {
    let stmt = parse_cypher("CREATE (n:Person {name: 'Alice'})");
    assert_eq!(stmt.clauses.len(), 1);
    let c = match &stmt.clauses[0] {
        CypherClause::Create(c) => c,
        other => panic!("expected Create, got {other:?}"),
    };
    assert_eq!(c.patterns.len(), 1);
    let node = &c.patterns[0].nodes[0];
    assert_eq!(node.variable.as_deref(), Some("n"));
    assert_eq!(node.labels.first().map(|s| s.as_str()), Some("Person"));
    assert_eq!(node.properties.len(), 1);
    assert_eq!(node.properties[0].0, "name");
}

#[test]
fn create_relationship() {
    let stmt = parse_cypher("CREATE (a)-[:KNOWS]->(b)");
    let c = match &stmt.clauses[0] {
        CypherClause::Create(c) => c,
        other => panic!("expected Create, got {other:?}"),
    };
    assert_eq!(c.patterns[0].nodes.len(), 2);
    assert_eq!(c.patterns[0].rels.len(), 1);
    assert_eq!(c.patterns[0].rels[0].rel_type.as_deref(), Some("KNOWS"));
}

// -----------------------------------------------------------------------
// MERGE with ON CREATE / ON MATCH
// -----------------------------------------------------------------------

#[test]
fn merge_with_actions() {
    let stmt = parse_cypher(
        "MERGE (n:Person {name: 'Alice'}) ON CREATE SET n.created = true ON MATCH SET n.updated = true",
    );
    assert_eq!(stmt.clauses.len(), 1);
    let m = match &stmt.clauses[0] {
        CypherClause::Merge(m) => m,
        other => panic!("expected Merge, got {other:?}"),
    };
    assert_eq!(m.pattern.nodes.len(), 1);
    assert_eq!(m.actions.len(), 2);
    assert!(m.actions[0].on_create);
    assert!(!m.actions[1].on_create);
}

#[test]
fn merge_without_actions() {
    let stmt = parse_cypher("MERGE (n:Person {name: 'Alice'})");
    let m = match &stmt.clauses[0] {
        CypherClause::Merge(m) => m,
        other => panic!("expected Merge, got {other:?}"),
    };
    assert!(m.actions.is_empty());
}

// -----------------------------------------------------------------------
// SET clause
// -----------------------------------------------------------------------

#[test]
fn set_property() {
    let stmt = parse_cypher("MATCH (n) SET n.name = 'Bob' RETURN n");
    assert_eq!(stmt.clauses.len(), 3); // MATCH, SET, RETURN
    let s = match &stmt.clauses[1] {
        CypherClause::Set(s) => s,
        other => panic!("expected Set, got {other:?}"),
    };
    assert_eq!(s.items.len(), 1);
    match &s.items[0] {
        CypherSetItem::Property {
            variable, property, ..
        } => {
            assert_eq!(variable, "n");
            assert_eq!(property, "name");
        }
        other => panic!("expected Property, got {other:?}"),
    }
}

#[test]
fn set_label() {
    let stmt = parse_cypher("MATCH (n) SET n:Admin RETURN n");
    let s = match &stmt.clauses[1] {
        CypherClause::Set(s) => s,
        other => panic!("expected Set, got {other:?}"),
    };
    match &s.items[0] {
        CypherSetItem::Label {
            variable, label, ..
        } => {
            assert_eq!(variable, "n");
            assert_eq!(label, "Admin");
        }
        other => panic!("expected Label, got {other:?}"),
    }
}

// -----------------------------------------------------------------------
// DELETE / DETACH DELETE
// -----------------------------------------------------------------------

#[test]
fn delete_variable() {
    let stmt = parse_cypher("MATCH (n) DELETE n");
    assert_eq!(stmt.clauses.len(), 2);
    let d = match &stmt.clauses[1] {
        CypherClause::Delete(d) => d,
        other => panic!("expected Delete, got {other:?}"),
    };
    assert!(!d.detach);
    assert_eq!(d.variables, vec!["n"]);
}

#[test]
fn detach_delete() {
    let stmt = parse_cypher("MATCH (n) DETACH DELETE n, m");
    let d = match &stmt.clauses[1] {
        CypherClause::Delete(d) => d,
        other => panic!("expected Delete, got {other:?}"),
    };
    assert!(d.detach);
    assert_eq!(d.variables, vec!["n", "m"]);
}

// -----------------------------------------------------------------------
// Complex multi-clause query
// -----------------------------------------------------------------------

#[test]
fn multi_clause_query() {
    let stmt = parse_cypher(
        "MATCH (a:Person)-[:KNOWS]->(b:Person) \
         OPTIONAL MATCH (b)-[:LIVES_IN]->(c:City) \
         RETURN a.name, b.name, c.name",
    );
    assert_eq!(stmt.clauses.len(), 3);

    // First MATCH (non-optional)
    let m1 = match &stmt.clauses[0] {
        CypherClause::Match(m) => m,
        other => panic!("expected Match, got {other:?}"),
    };
    assert!(!m1.optional);

    // Second MATCH (optional)
    let m2 = match &stmt.clauses[1] {
        CypherClause::Match(m) => m,
        other => panic!("expected Match, got {other:?}"),
    };
    assert!(m2.optional);

    // RETURN
    let r = match &stmt.clauses[2] {
        CypherClause::Return(r) => r,
        other => panic!("expected Return, got {other:?}"),
    };
    assert_eq!(r.items.len(), 3);
}

// -----------------------------------------------------------------------
// RETURN clause features
// -----------------------------------------------------------------------

#[test]
fn return_distinct() {
    let stmt = parse_cypher("MATCH (n) RETURN DISTINCT n.name");
    let r = match &stmt.clauses[1] {
        CypherClause::Return(r) => r,
        other => panic!("expected Return, got {other:?}"),
    };
    assert!(r.distinct);
}

#[test]
fn return_with_alias() {
    let stmt = parse_cypher("MATCH (n) RETURN n.name AS person_name");
    let r = match &stmt.clauses[1] {
        CypherClause::Return(r) => r,
        other => panic!("expected Return, got {other:?}"),
    };
    assert_eq!(r.items[0].alias.as_deref(), Some("person_name"));
}

#[test]
fn return_map_projection() {
    let stmt = parse_cypher("MATCH (n) RETURN n {.name, born: n.born, .*}");
    let r = match &stmt.clauses[1] {
        CypherClause::Return(r) => r,
        other => panic!("expected Return, got {other:?}"),
    };
    let Expr::FunctionCall { name, args, .. } = &r.items[0].expr else {
        panic!("expected map projection function");
    };
    assert_eq!(name.parts, vec!["__cypher_map_projection"]);
    assert_eq!(args.len(), 7);
}

#[test]
fn return_with_order_by_skip_limit() {
    let stmt = parse_cypher("MATCH (n) RETURN n ORDER BY n.name DESC SKIP 5 LIMIT 10");
    let r = match &stmt.clauses[1] {
        CypherClause::Return(r) => r,
        other => panic!("expected Return, got {other:?}"),
    };
    assert_eq!(r.order_by.len(), 1);
    assert!(r.order_by[0].descending);
    assert!(r.skip.is_some());
    assert!(r.limit.is_some());
}

// -----------------------------------------------------------------------
// Node pattern variations
// -----------------------------------------------------------------------

#[test]
fn node_without_variable() {
    let stmt = parse_cypher("MATCH (:Person) RETURN 1");
    let m = match &stmt.clauses[0] {
        CypherClause::Match(m) => m,
        other => panic!("expected Match, got {other:?}"),
    };
    assert!(m.patterns[0].nodes[0].variable.is_none());
    assert_eq!(
        m.patterns[0].nodes[0].labels.first().map(|s| s.as_str()),
        Some("Person")
    );
}

#[test]
fn node_without_label() {
    let stmt = parse_cypher("MATCH (n) RETURN n");
    let m = match &stmt.clauses[0] {
        CypherClause::Match(m) => m,
        other => panic!("expected Match, got {other:?}"),
    };
    assert_eq!(m.patterns[0].nodes[0].variable.as_deref(), Some("n"));
    assert!(m.patterns[0].nodes[0].labels.is_empty());
}

#[test]
fn empty_node() {
    let stmt = parse_cypher("MATCH () RETURN 1");
    let m = match &stmt.clauses[0] {
        CypherClause::Match(m) => m,
        other => panic!("expected Match, got {other:?}"),
    };
    assert!(m.patterns[0].nodes[0].variable.is_none());
    assert!(m.patterns[0].nodes[0].labels.is_empty());
}

// -----------------------------------------------------------------------
// Multiple patterns in MATCH
// -----------------------------------------------------------------------

#[test]
fn multiple_patterns() {
    let stmt = parse_cypher("MATCH (a:Person), (b:City) RETURN a, b");
    let m = match &stmt.clauses[0] {
        CypherClause::Match(m) => m,
        other => panic!("expected Match, got {other:?}"),
    };
    assert_eq!(m.patterns.len(), 2);
}

// -----------------------------------------------------------------------
// Inline properties
// -----------------------------------------------------------------------

#[test]
fn node_with_properties() {
    let stmt = parse_cypher("MATCH (n:Person {age: 30, name: 'Alice'}) RETURN n");
    let node = &match &stmt.clauses[0] {
        CypherClause::Match(m) => m,
        other => panic!("expected Match, got {other:?}"),
    }
    .patterns[0]
        .nodes[0];
    assert_eq!(node.properties.len(), 2);
    assert_eq!(node.properties[0].0, "age");
    assert_eq!(node.properties[1].0, "name");
}

// -----------------------------------------------------------------------
// Relationship without brackets
// -----------------------------------------------------------------------

// -----------------------------------------------------------------------
// Relationship without type/variable (empty brackets)
// Note: `-->` shorthand without brackets is NOT supported because `--`
// is lexed as a SQL line comment. Use `-[]->` instead.
// -----------------------------------------------------------------------

#[test]
fn relationship_empty_brackets() {
    let stmt = parse_cypher("MATCH (a)-[]->(b) RETURN a, b");
    let m = match &stmt.clauses[0] {
        CypherClause::Match(m) => m,
        other => panic!("expected Match, got {other:?}"),
    };
    assert_eq!(m.patterns[0].nodes.len(), 2);
    assert_eq!(m.patterns[0].rels.len(), 1);
    let rel = &m.patterns[0].rels[0];
    assert!(rel.variable.is_none());
    assert!(rel.rel_type.is_none());
    assert_eq!(rel.direction, CypherDirection::Outgoing);
}

// -----------------------------------------------------------------------
// MATCH + CREATE multi-clause
// -----------------------------------------------------------------------

#[test]
fn match_create_return() {
    let stmt = parse_cypher(
        "MATCH (a:Person {name: 'Alice'}) \
         CREATE (a)-[:KNOWS]->(b:Person {name: 'Bob'}) \
         RETURN b",
    );
    assert_eq!(stmt.clauses.len(), 3);
    assert!(matches!(&stmt.clauses[0], CypherClause::Match(_)));
    assert!(matches!(&stmt.clauses[1], CypherClause::Create(_)));
    assert!(matches!(&stmt.clauses[2], CypherClause::Return(_)));
}

// -----------------------------------------------------------------------
// Error cases
// -----------------------------------------------------------------------

#[test]
fn error_empty_match() {
    // MATCH without a pattern should fail
    let err = parse_cypher_err("MATCH RETURN n");
    assert!(err.contains("expected"), "error: {err}");
}

// -----------------------------------------------------------------------
// Long chain of relationships
// -----------------------------------------------------------------------

#[test]
fn long_chain() {
    let stmt = parse_cypher("MATCH (a)-[:R1]->(b)-[:R2]->(c)-[:R3]->(d) RETURN d");
    let m = match &stmt.clauses[0] {
        CypherClause::Match(m) => m,
        other => panic!("expected Match, got {other:?}"),
    };
    assert_eq!(m.patterns[0].nodes.len(), 4);
    assert_eq!(m.patterns[0].rels.len(), 3);
}

// -----------------------------------------------------------------------
// SET with multiple items
// -----------------------------------------------------------------------

#[test]
fn set_multiple_items() {
    let stmt = parse_cypher("MATCH (n) SET n.name = 'Bob', n.age = 30 RETURN n");
    let s = match &stmt.clauses[1] {
        CypherClause::Set(s) => s,
        other => panic!("expected Set, got {other:?}"),
    };
    assert_eq!(s.items.len(), 2);
}

// -----------------------------------------------------------------------
// CALL clause
// -----------------------------------------------------------------------

#[test]
fn call_db_labels_yield() {
    let stmt = parse_cypher("CALL db.labels() YIELD label RETURN label");
    assert_eq!(stmt.clauses.len(), 2);
    let call = match &stmt.clauses[0] {
        CypherClause::Call(c) => c,
        other => panic!("expected Call, got {other:?}"),
    };
    assert_eq!(call.procedure, "db.labels");
    assert!(call.args.is_empty());
    assert_eq!(call.yields, vec!["label"]);
}

#[test]
fn call_db_relationship_types_yield() {
    // Note: the lexer folds unquoted identifiers to lowercase (PG-compatible)
    let stmt =
        parse_cypher("CALL db.relationshipTypes() YIELD relationshipType RETURN relationshipType");
    assert_eq!(stmt.clauses.len(), 2);
    let call = match &stmt.clauses[0] {
        CypherClause::Call(c) => c,
        other => panic!("expected Call, got {other:?}"),
    };
    assert_eq!(call.procedure, "db.relationshiptypes");
    assert!(call.args.is_empty());
    assert_eq!(call.yields, vec!["relationshiptype"]);
}

#[test]
fn call_db_property_keys_yield() {
    let stmt = parse_cypher("CALL db.propertyKeys() YIELD propertyKey RETURN propertyKey");
    assert_eq!(stmt.clauses.len(), 2);
    let call = match &stmt.clauses[0] {
        CypherClause::Call(c) => c,
        other => panic!("expected Call, got {other:?}"),
    };
    assert_eq!(call.procedure, "db.propertykeys");
    assert!(call.args.is_empty());
    assert_eq!(call.yields, vec!["propertykey"]);
}

#[test]
fn call_no_yield() {
    let stmt = parse_cypher("CALL db.labels() RETURN 1");
    let call = match &stmt.clauses[0] {
        CypherClause::Call(c) => c,
        other => panic!("expected Call, got {other:?}"),
    };
    assert_eq!(call.procedure, "db.labels");
    assert!(call.yields.is_empty());
}

#[test]
fn call_multiple_yield_items() {
    let stmt = parse_cypher(
        "CALL db.schema.nodeTypeProperties() YIELD nodeType, propertyName RETURN nodeType",
    );
    let call = match &stmt.clauses[0] {
        CypherClause::Call(c) => c,
        other => panic!("expected Call, got {other:?}"),
    };
    assert_eq!(call.procedure, "db.schema.nodetypeproperties");
    assert_eq!(call.yields, vec!["nodetype", "propertyname"]);
}

#[test]
fn call_subquery_parses_nested_cypher_statement() {
    let stmt = parse_cypher("MATCH (n) CALL { WITH n RETURN n.name AS name } RETURN name");
    assert_eq!(stmt.clauses.len(), 3);
    let call = match &stmt.clauses[1] {
        CypherClause::Call(c) => c,
        other => panic!("expected Call, got {other:?}"),
    };
    assert!(call.procedure.is_empty());
    let subquery = call.subquery.as_ref().expect("subquery call");
    assert_eq!(subquery.clauses.len(), 2);
    assert!(matches!(subquery.clauses[0], CypherClause::With(_)));
    assert!(matches!(subquery.clauses[1], CypherClause::Return(_)));
}

#[test]
fn exists_subquery_expression_parses_nested_match() {
    let stmt = parse_cypher("MATCH (n) WHERE EXISTS { MATCH (n)-[:KNOWS]->(m) } RETURN n");
    let match_clause = match &stmt.clauses[0] {
        CypherClause::Match(m) => m,
        other => panic!("expected Match, got {other:?}"),
    };
    let exists = match match_clause.where_clause.as_ref() {
        Some(Expr::CypherExists {
            query,
            negated: false,
            ..
        }) => query,
        other => panic!("expected positive Cypher EXISTS, got {other:?}"),
    };
    assert_eq!(exists.clauses.len(), 1);
    assert!(matches!(exists.clauses[0], CypherClause::Match(_)));
}

#[test]
fn not_exists_subquery_expression_parses_nested_match() {
    let stmt = parse_cypher("MATCH (n) WHERE NOT EXISTS { MATCH (n)-[:KNOWS]->(m) } RETURN n");
    let match_clause = match &stmt.clauses[0] {
        CypherClause::Match(m) => m,
        other => panic!("expected Match, got {other:?}"),
    };
    let exists = match match_clause.where_clause.as_ref() {
        Some(Expr::CypherExists {
            query,
            negated: true,
            ..
        }) => query,
        other => panic!("expected negated Cypher EXISTS, got {other:?}"),
    };
    assert_eq!(exists.clauses.len(), 1);
    assert!(matches!(exists.clauses[0], CypherClause::Match(_)));
}

#[test]
fn pattern_comprehension_expression_parses_pattern_where_and_projection() {
    let stmt = parse_cypher(
        "MATCH (a:Person) RETURN [(a)-[:KNOWS]->(b) WHERE b.active | b.name] AS friends",
    );
    let ret = match &stmt.clauses[1] {
        CypherClause::Return(r) => r,
        other => panic!("expected Return, got {other:?}"),
    };
    let Expr::CypherPatternComprehension {
        pattern,
        where_clause,
        map_expr,
        ..
    } = &ret.items[0].expr
    else {
        panic!(
            "expected Cypher pattern comprehension, got {:?}",
            ret.items[0].expr
        );
    };
    assert_eq!(pattern.nodes.len(), 2);
    assert_eq!(pattern.rels.len(), 1);
    assert!(where_clause.is_some());
    assert!(matches!(map_expr.as_ref(), Expr::Identifier(_)));
}
