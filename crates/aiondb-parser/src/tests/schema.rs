use super::*;

#[test]
fn create_schema_basic() {
    let stmt = parse_prepared_statement("CREATE SCHEMA myschema").expect("parse");
    let Statement::CreateSchema(s) = stmt else {
        panic!("expected CreateSchema, got {stmt:?}");
    };
    assert_eq!(s.name, "myschema");
    assert!(!s.if_not_exists);
}

#[test]
fn create_schema_if_not_exists() {
    let stmt = parse_prepared_statement("CREATE SCHEMA IF NOT EXISTS myschema").expect("parse");
    let Statement::CreateSchema(s) = stmt else {
        panic!("expected CreateSchema, got {stmt:?}");
    };
    assert_eq!(s.name, "myschema");
    assert!(s.if_not_exists);
}

#[test]
fn drop_schema_basic() {
    let stmt = parse_prepared_statement("DROP SCHEMA myschema").expect("parse");
    let Statement::DropSchema(s) = stmt else {
        panic!("expected DropSchema, got {stmt:?}");
    };
    assert_eq!(s.name, "myschema");
    assert!(!s.if_exists);
    assert!(!s.cascade);
}

#[test]
fn drop_schema_if_exists() {
    let stmt = parse_prepared_statement("DROP SCHEMA IF EXISTS myschema").expect("parse");
    let Statement::DropSchema(s) = stmt else {
        panic!("expected DropSchema, got {stmt:?}");
    };
    assert_eq!(s.name, "myschema");
    assert!(s.if_exists);
    assert!(!s.cascade);
}

#[test]
fn drop_schema_cascade() {
    let stmt = parse_prepared_statement("DROP SCHEMA myschema CASCADE").expect("parse");
    let Statement::DropSchema(s) = stmt else {
        panic!("expected DropSchema, got {stmt:?}");
    };
    assert_eq!(s.name, "myschema");
    assert!(!s.if_exists);
    assert!(s.cascade);
}

#[test]
fn drop_schema_if_exists_cascade() {
    let stmt = parse_prepared_statement("DROP SCHEMA IF EXISTS myschema CASCADE").expect("parse");
    let Statement::DropSchema(s) = stmt else {
        panic!("expected DropSchema, got {stmt:?}");
    };
    assert_eq!(s.name, "myschema");
    assert!(s.if_exists);
    assert!(s.cascade);
}

#[test]
fn create_schema_case_insensitive_keywords() {
    let stmt = parse_prepared_statement("create schema test_schema").expect("parse");
    let Statement::CreateSchema(s) = stmt else {
        panic!("expected CreateSchema, got {stmt:?}");
    };
    assert_eq!(s.name, "test_schema");
    assert!(!s.if_not_exists);
}

#[test]
fn create_schema_authorization_uses_authorized_name() {
    let stmt = parse_prepared_statement("CREATE SCHEMA AUTHORIZATION app_owner").expect("parse");
    let Statement::CreateSchema(s) = stmt else {
        panic!("expected CreateSchema, got {stmt:?}");
    };
    assert_eq!(s.name, "app_owner");
}

#[test]
fn create_schema_name_with_authorization_is_accepted() {
    let stmt = parse_prepared_statement("CREATE SCHEMA app AUTHORIZATION admin").expect("parse");
    let Statement::CreateSchema(s) = stmt else {
        panic!("expected CreateSchema, got {stmt:?}");
    };
    assert_eq!(s.name, "app");
}

#[test]
fn create_schema_with_inline_table_body_is_parsed() {
    let stmt = parse_prepared_statement(
        "CREATE SCHEMA app AUTHORIZATION admin CREATE TABLE app.events (id int)",
    )
    .expect("parse");
    let Statement::CreateSchema(s) = stmt else {
        panic!("expected CreateSchema, got {stmt:?}");
    };
    assert_eq!(s.name, "app");
    assert_eq!(s.body.len(), 1);
    match &s.body[0] {
        Statement::CreateTable(create_table) => {
            assert_eq!(create_table.name.parts, vec!["app", "events"]);
        }
        other => panic!("expected CreateTable body, got {other:?}"),
    }
}

#[test]
fn create_schema_with_inline_body_rejects_mismatched_schema_names() {
    let error = parse_prepared_statement(
        "CREATE SCHEMA app AUTHORIZATION admin CREATE TABLE other.events (id int)",
    )
    .expect_err("mismatched inline schema should fail");

    assert_eq!(error.sqlstate().code(), "3F000");
    assert!(
        error.report().message.contains(
            "CREATE specifies a schema (other) different from the one being created (app)"
        ),
        "unexpected error: {}",
        error.report().message
    );
}

#[test]
fn drop_schema_case_insensitive_keywords() {
    let stmt = parse_prepared_statement("drop schema test_schema cascade").expect("parse");
    let Statement::DropSchema(s) = stmt else {
        panic!("expected DropSchema, got {stmt:?}");
    };
    assert_eq!(s.name, "test_schema");
    assert!(s.cascade);
}
