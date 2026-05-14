use super::*;

// ===================================================================
// CREATE TABLE IF NOT EXISTS
// ===================================================================

#[test]
fn create_table_if_not_exists() {
    let stmt =
        parse_prepared_statement("CREATE TABLE IF NOT EXISTS users (id INT)").expect("parse");
    let Statement::CreateTable(ct) = stmt else {
        panic!("expected CreateTable, got {stmt:?}");
    };
    assert_eq!(ct.name.parts, vec!["users".to_string()]);
    assert!(ct.if_not_exists);
    assert_eq!(ct.columns.len(), 1);
}

#[test]
fn create_table_without_if_not_exists() {
    let stmt = parse_prepared_statement("CREATE TABLE users (id INT)").expect("parse");
    let Statement::CreateTable(ct) = stmt else {
        panic!("expected CreateTable, got {stmt:?}");
    };
    assert!(!ct.if_not_exists);
}

// ===================================================================
// DROP TABLE IF EXISTS
// ===================================================================

#[test]
fn drop_table_if_exists() {
    let stmt = parse_prepared_statement("DROP TABLE IF EXISTS users").expect("parse");
    let Statement::DropTable(dt) = stmt else {
        panic!("expected DropTable, got {stmt:?}");
    };
    assert_eq!(dt.name.parts, vec!["users".to_string()]);
    assert!(dt.if_exists);
}

#[test]
fn drop_table_without_if_exists() {
    let stmt = parse_prepared_statement("DROP TABLE users").expect("parse");
    let Statement::DropTable(dt) = stmt else {
        panic!("expected DropTable, got {stmt:?}");
    };
    assert!(!dt.if_exists);
}

// ===================================================================
// CREATE INDEX IF NOT EXISTS
// ===================================================================

#[test]
fn create_index_if_not_exists() {
    let stmt =
        parse_prepared_statement("CREATE INDEX IF NOT EXISTS idx ON users (id)").expect("parse");
    let Statement::CreateIndex(ci) = stmt else {
        panic!("expected CreateIndex, got {stmt:?}");
    };
    assert_eq!(ci.name.parts, vec!["idx".to_string()]);
    assert!(ci.if_not_exists);
}

#[test]
fn create_index_without_if_not_exists() {
    let stmt = parse_prepared_statement("CREATE INDEX idx ON users (id)").expect("parse");
    let Statement::CreateIndex(ci) = stmt else {
        panic!("expected CreateIndex, got {stmt:?}");
    };
    assert!(!ci.if_not_exists);
}

#[test]
fn create_index_if_not_exists_without_name_is_rejected() {
    let err =
        parse_prepared_statement("CREATE INDEX IF NOT EXISTS ON users (id)").expect_err("error");
    assert!(format!("{err}").contains("syntax error at or near \"ON\""));
}

// ===================================================================
// DROP INDEX IF EXISTS
// ===================================================================

#[test]
fn drop_index_if_exists() {
    let stmt = parse_prepared_statement("DROP INDEX IF EXISTS idx").expect("parse");
    let Statement::DropIndex(di) = stmt else {
        panic!("expected DropIndex, got {stmt:?}");
    };
    assert_eq!(di.name.parts, vec!["idx".to_string()]);
    assert!(di.if_exists);
}

#[test]
fn drop_index_without_if_exists() {
    let stmt = parse_prepared_statement("DROP INDEX idx").expect("parse");
    let Statement::DropIndex(di) = stmt else {
        panic!("expected DropIndex, got {stmt:?}");
    };
    assert!(!di.if_exists);
}

// ===================================================================
// CREATE SEQUENCE IF NOT EXISTS
// ===================================================================

#[test]
fn create_sequence_if_not_exists() {
    let stmt = parse_prepared_statement("CREATE SEQUENCE IF NOT EXISTS my_seq").expect("parse");
    let Statement::CreateSequence(cs) = stmt else {
        panic!("expected CreateSequence, got {stmt:?}");
    };
    assert_eq!(cs.name.parts, vec!["my_seq".to_string()]);
    assert!(cs.if_not_exists);
}

#[test]
fn create_sequence_without_if_not_exists() {
    let stmt = parse_prepared_statement("CREATE SEQUENCE my_seq").expect("parse");
    let Statement::CreateSequence(cs) = stmt else {
        panic!("expected CreateSequence, got {stmt:?}");
    };
    assert!(!cs.if_not_exists);
}

// ===================================================================
// DROP SEQUENCE IF EXISTS
// ===================================================================

#[test]
fn drop_sequence_if_exists() {
    let stmt = parse_prepared_statement("DROP SEQUENCE IF EXISTS my_seq").expect("parse");
    let Statement::DropSequence(ds) = stmt else {
        panic!("expected DropSequence, got {stmt:?}");
    };
    assert_eq!(ds.name.parts, vec!["my_seq".to_string()]);
    assert!(ds.if_exists);
}

#[test]
fn drop_sequence_without_if_exists() {
    let stmt = parse_prepared_statement("DROP SEQUENCE my_seq").expect("parse");
    let Statement::DropSequence(ds) = stmt else {
        panic!("expected DropSequence, got {stmt:?}");
    };
    assert!(!ds.if_exists);
}

// ===================================================================
// CREATE VIEW IF NOT EXISTS
// ===================================================================

#[test]
fn create_view_if_not_exists() {
    let stmt = parse_prepared_statement("CREATE VIEW IF NOT EXISTS v AS SELECT 1").expect("parse");
    let Statement::CreateView(cv) = stmt else {
        panic!("expected CreateView, got {stmt:?}");
    };
    assert_eq!(cv.name.parts, vec!["v".to_string()]);
    assert!(cv.if_not_exists);
}

#[test]
fn create_view_without_if_not_exists() {
    let stmt = parse_prepared_statement("CREATE VIEW v AS SELECT 1").expect("parse");
    let Statement::CreateView(cv) = stmt else {
        panic!("expected CreateView, got {stmt:?}");
    };
    assert!(!cv.if_not_exists);
}

// ===================================================================
// DROP VIEW IF EXISTS
// ===================================================================

#[test]
fn drop_view_if_exists() {
    let stmt = parse_prepared_statement("DROP VIEW IF EXISTS v").expect("parse");
    let Statement::DropView(dv) = stmt else {
        panic!("expected DropView, got {stmt:?}");
    };
    assert_eq!(dv.name.parts, vec!["v".to_string()]);
    assert!(dv.if_exists);
}

#[test]
fn drop_view_without_if_exists() {
    let stmt = parse_prepared_statement("DROP VIEW v").expect("parse");
    let Statement::DropView(dv) = stmt else {
        panic!("expected DropView, got {stmt:?}");
    };
    assert!(!dv.if_exists);
}

// ===================================================================
// Case insensitivity for IF NOT EXISTS / IF EXISTS
// ===================================================================

#[test]
fn create_table_if_not_exists_case_insensitive() {
    let stmt = parse_prepared_statement("create table if not exists t (x int)").expect("parse");
    let Statement::CreateTable(ct) = stmt else {
        panic!("expected CreateTable, got {stmt:?}");
    };
    assert!(ct.if_not_exists);
}

#[test]
fn drop_table_if_exists_case_insensitive() {
    let stmt = parse_prepared_statement("drop table if exists t").expect("parse");
    let Statement::DropTable(dt) = stmt else {
        panic!("expected DropTable, got {stmt:?}");
    };
    assert!(dt.if_exists);
}
