use super::*;

// ═══════════════════════════════════════════════════════════════
//  RETURNING CLAUSE PARSING TESTS
// ═══════════════════════════════════════════════════════════════

#[test]
fn dml_insert_returning_star() {
    let stmt = parse_prepared_statement("INSERT INTO t VALUES (1) RETURNING *").expect("parse");
    let Statement::Insert(ins) = stmt else {
        panic!("expected INSERT");
    };
    assert_eq!(ins.rows.len(), 1);
    assert_eq!(ins.returning.len(), 1);
    match &ins.returning[0].expr {
        Expr::Identifier(name) => assert_eq!(name.parts, vec!["*".to_owned()]),
        _ => panic!("expected star identifier"),
    }
}

#[test]
fn dml_insert_returning_columns() {
    let stmt =
        parse_prepared_statement("INSERT INTO t VALUES (1) RETURNING id, name").expect("parse");
    let Statement::Insert(ins) = stmt else {
        panic!("expected INSERT");
    };
    assert_eq!(ins.rows.len(), 1);
    assert_eq!(ins.returning.len(), 2);
    match &ins.returning[0].expr {
        Expr::Identifier(name) => assert_eq!(name.parts, vec!["id".to_owned()]),
        _ => panic!("expected identifier 'id'"),
    }
    match &ins.returning[1].expr {
        Expr::Identifier(name) => assert_eq!(name.parts, vec!["name".to_owned()]),
        _ => panic!("expected identifier 'name'"),
    }
}

#[test]
fn dml_update_returning() {
    let stmt = parse_prepared_statement("UPDATE t SET x = 1 RETURNING x").expect("parse");
    let Statement::Update(upd) = stmt else {
        panic!("expected UPDATE");
    };
    assert_eq!(upd.assignments.len(), 1);
    assert_eq!(upd.returning.len(), 1);
    match &upd.returning[0].expr {
        Expr::Identifier(name) => assert_eq!(name.parts, vec!["x".to_owned()]),
        _ => panic!("expected identifier 'x'"),
    }
}

#[test]
fn dml_delete_returning_star() {
    let stmt = parse_prepared_statement("DELETE FROM t WHERE id = 1 RETURNING *").expect("parse");
    let Statement::Delete(del) = stmt else {
        panic!("expected DELETE");
    };
    assert!(del.selection.is_some());
    assert_eq!(del.returning.len(), 1);
    match &del.returning[0].expr {
        Expr::Identifier(name) => assert_eq!(name.parts, vec!["*".to_owned()]),
        _ => panic!("expected star identifier"),
    }
}

#[test]
fn dml_insert_no_returning_still_works() {
    let stmt = parse_prepared_statement("INSERT INTO t VALUES (1)").expect("parse");
    let Statement::Insert(ins) = stmt else {
        panic!("expected INSERT");
    };
    assert!(ins.returning.is_empty());
    assert_eq!(ins.rows.len(), 1);
}

#[test]
fn dml_update_no_returning_still_works() {
    let stmt = parse_prepared_statement("UPDATE t SET x = 1").expect("parse");
    let Statement::Update(upd) = stmt else {
        panic!("expected UPDATE");
    };
    assert!(upd.returning.is_empty());
}

#[test]
fn dml_delete_no_returning_still_works() {
    let stmt = parse_prepared_statement("DELETE FROM t").expect("parse");
    let Statement::Delete(del) = stmt else {
        panic!("expected DELETE");
    };
    assert!(del.returning.is_empty());
}

#[test]
fn dml_delete_returning_columns() {
    let stmt =
        parse_prepared_statement("DELETE FROM t WHERE id = 1 RETURNING id, name").expect("parse");
    let Statement::Delete(del) = stmt else {
        panic!("expected DELETE");
    };
    assert_eq!(del.returning.len(), 2);
    match &del.returning[0].expr {
        Expr::Identifier(name) => assert_eq!(name.parts, vec!["id".to_owned()]),
        _ => panic!("expected identifier 'id'"),
    }
    match &del.returning[1].expr {
        Expr::Identifier(name) => assert_eq!(name.parts, vec!["name".to_owned()]),
        _ => panic!("expected identifier 'name'"),
    }
}

#[test]
fn dml_insert_returning_with_alias() {
    let stmt =
        parse_prepared_statement("INSERT INTO t VALUES (1) RETURNING id AS row_id").expect("parse");
    let Statement::Insert(ins) = stmt else {
        panic!("expected INSERT");
    };
    assert_eq!(ins.returning.len(), 1);
    assert_eq!(ins.returning[0].alias.as_deref(), Some("row_id"));
}

#[test]
fn dml_update_returning_star() {
    let stmt =
        parse_prepared_statement("UPDATE t SET x = 1 WHERE id = 2 RETURNING *").expect("parse");
    let Statement::Update(upd) = stmt else {
        panic!("expected UPDATE");
    };
    assert!(upd.selection.is_some());
    assert_eq!(upd.returning.len(), 1);
    match &upd.returning[0].expr {
        Expr::Identifier(name) => assert_eq!(name.parts, vec!["*".to_owned()]),
        _ => panic!("expected star identifier"),
    }
}

#[test]
fn dml_delete_no_where_returning() {
    let stmt = parse_prepared_statement("DELETE FROM t RETURNING *").expect("parse");
    let Statement::Delete(del) = stmt else {
        panic!("expected DELETE");
    };
    assert!(del.selection.is_none());
    assert_eq!(del.returning.len(), 1);
}
