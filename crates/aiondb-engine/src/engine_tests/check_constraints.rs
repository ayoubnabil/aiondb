use super::*;

// ===================================================================
// CHECK constraint enforcement: INSERT
// ===================================================================

#[test]
fn insert_satisfying_check_constraint_succeeds() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE t (id INT, age INT, CHECK (age > 0))",
        )
        .expect("create");

    let results = engine
        .execute_sql(&session, "INSERT INTO t VALUES (1, 25)")
        .expect("insert");
    assert!(matches!(
        &results[0],
        StatementResult::Command { tag, rows_affected }
        if tag == "INSERT" && *rows_affected == 1
    ));
}

#[test]
fn insert_violating_check_constraint_fails() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE t (id INT, age INT, CHECK (age > 0))",
        )
        .expect("create");

    let err = engine
        .execute_sql(&session, "INSERT INTO t VALUES (1, -5)")
        .expect_err("should violate CHECK");
    assert_eq!(err.sqlstate(), aiondb_core::SqlState::CheckViolation);
    assert!(
        err.report().message.contains("violates check constraint"),
        "unexpected message: {}",
        err.report().message
    );
}

#[test]
fn insert_boundary_value_exactly_at_check_limit() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE t (val INT, CHECK (val >= 0))")
        .expect("create");

    // val = 0 should satisfy val >= 0
    engine
        .execute_sql(&session, "INSERT INTO t VALUES (0)")
        .expect("boundary value should pass");

    // val = -1 should fail
    let err = engine
        .execute_sql(&session, "INSERT INTO t VALUES (-1)")
        .expect_err("should violate CHECK");
    assert_eq!(err.sqlstate(), aiondb_core::SqlState::CheckViolation);
}

#[test]
fn insert_null_satisfies_check_constraint() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE t (id INT, age INT, CHECK (age > 0))",
        )
        .expect("create");

    // Per SQL standard, NULL satisfies CHECK (only FALSE is a violation)
    engine
        .execute_sql(&session, "INSERT INTO t VALUES (1, NULL)")
        .expect("NULL should satisfy CHECK");

    let rows = engine
        .execute_sql(&session, "SELECT age FROM t WHERE id = 1")
        .expect("select");
    assert!(matches!(
        &rows[0],
        StatementResult::Query { rows, .. } if rows.len() == 1
    ));
}

#[test]
fn insert_satisfying_char_length_check_constraint_succeeds() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE t (name TEXT, CHECK (char_length(name) >= 2))",
        )
        .expect("create");

    engine
        .execute_sql(&session, "INSERT INTO t VALUES ('alice')")
        .expect("char_length check should pass");
}

#[test]
fn insert_violating_char_length_check_constraint_fails() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE t (name TEXT, CHECK (char_length(name) >= 2))",
        )
        .expect("create");

    let err = engine
        .execute_sql(&session, "INSERT INTO t VALUES ('a')")
        .expect_err("short text should violate CHECK");
    assert_eq!(err.sqlstate(), aiondb_core::SqlState::CheckViolation);
}

// ===================================================================
// CHECK constraint enforcement: UPDATE
// ===================================================================

#[test]
fn update_satisfying_check_constraint_succeeds() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE t (id INT, age INT, CHECK (age > 0))",
        )
        .expect("create");
    engine
        .execute_sql(&session, "INSERT INTO t VALUES (1, 25)")
        .expect("insert");

    engine
        .execute_sql(&session, "UPDATE t SET age = 30 WHERE id = 1")
        .expect("update should succeed");

    let rows = engine
        .execute_sql(&session, "SELECT age FROM t WHERE id = 1")
        .expect("select");
    if let StatementResult::Query { rows, .. } = &rows[0] {
        assert_eq!(rows[0].values[0], aiondb_core::Value::Int(30));
    } else {
        panic!("expected query result");
    }
}

#[test]
fn update_violating_check_constraint_fails() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE t (id INT, age INT, CHECK (age > 0))",
        )
        .expect("create");
    engine
        .execute_sql(&session, "INSERT INTO t VALUES (1, 25)")
        .expect("insert");

    let err = engine
        .execute_sql(&session, "UPDATE t SET age = -10 WHERE id = 1")
        .expect_err("should violate CHECK");
    assert_eq!(err.sqlstate(), aiondb_core::SqlState::CheckViolation);

    // Original value should be unchanged
    let rows = engine
        .execute_sql(&session, "SELECT age FROM t WHERE id = 1")
        .expect("select");
    if let StatementResult::Query { rows, .. } = &rows[0] {
        assert_eq!(rows[0].values[0], aiondb_core::Value::Int(25));
    } else {
        panic!("expected query result");
    }
}

// ===================================================================
// Multiple CHECK constraints
// ===================================================================

#[test]
fn multiple_check_constraints_all_satisfied() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE t (age INT, salary INT, CHECK (age > 0), CHECK (salary >= 0))",
        )
        .expect("create");

    engine
        .execute_sql(&session, "INSERT INTO t VALUES (25, 50000)")
        .expect("both checks satisfied");
}

#[test]
fn multiple_check_constraints_first_violated() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE t (age INT, salary INT, CHECK (age > 0), CHECK (salary >= 0))",
        )
        .expect("create");

    let err = engine
        .execute_sql(&session, "INSERT INTO t VALUES (-1, 50000)")
        .expect_err("first check violated");
    assert_eq!(err.sqlstate(), aiondb_core::SqlState::CheckViolation);
}

#[test]
fn multiple_check_constraints_second_violated() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE t (age INT, salary INT, CHECK (age > 0), CHECK (salary >= 0))",
        )
        .expect("create");

    let err = engine
        .execute_sql(&session, "INSERT INTO t VALUES (25, -100)")
        .expect_err("second check violated");
    assert_eq!(err.sqlstate(), aiondb_core::SqlState::CheckViolation);
}

#[test]
fn geometric_point_column_rejects_invalid_text_on_insert() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE point_tbl (f1 point)")
        .expect("create table");

    let err = engine
        .execute_sql(&session, "INSERT INTO point_tbl (f1) VALUES ('asdfasdf')")
        .expect_err("invalid point text should fail");
    assert_eq!(
        err.sqlstate(),
        aiondb_core::SqlState::InvalidTextRepresentation
    );
    assert!(
        err.report()
            .message
            .contains("invalid input syntax for type point"),
        "unexpected message: {}",
        err.report().message
    );

    engine
        .execute_sql(&session, "INSERT INTO point_tbl (f1) VALUES ('(1,2)')")
        .expect("valid point literal should succeed");
}

// ===================================================================
// Complex CHECK expressions
// ===================================================================

#[test]
fn check_constraint_with_and_expression() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE t (age INT, CHECK (age >= 0 AND age <= 150))",
        )
        .expect("create");

    engine
        .execute_sql(&session, "INSERT INTO t VALUES (25)")
        .expect("within range");

    let err = engine
        .execute_sql(&session, "INSERT INTO t VALUES (200)")
        .expect_err("above range");
    assert_eq!(err.sqlstate(), aiondb_core::SqlState::CheckViolation);

    let err = engine
        .execute_sql(&session, "INSERT INTO t VALUES (-1)")
        .expect_err("below range");
    assert_eq!(err.sqlstate(), aiondb_core::SqlState::CheckViolation);
}

#[test]
fn check_constraint_with_or_expression() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE t (status INT, CHECK (status = 0 OR status = 1))",
        )
        .expect("create");

    engine
        .execute_sql(&session, "INSERT INTO t VALUES (0)")
        .expect("status 0 ok");
    engine
        .execute_sql(&session, "INSERT INTO t VALUES (1)")
        .expect("status 1 ok");

    let err = engine
        .execute_sql(&session, "INSERT INTO t VALUES (2)")
        .expect_err("status 2 should fail");
    assert_eq!(err.sqlstate(), aiondb_core::SqlState::CheckViolation);
}

#[test]
fn check_constraint_with_not_equal() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE t (val INT, CHECK (val != 0))")
        .expect("create");

    engine
        .execute_sql(&session, "INSERT INTO t VALUES (1)")
        .expect("non-zero ok");

    let err = engine
        .execute_sql(&session, "INSERT INTO t VALUES (0)")
        .expect_err("zero should fail");
    assert_eq!(err.sqlstate(), aiondb_core::SqlState::CheckViolation);
}

#[test]
fn check_constraint_with_in_list() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE t (priority INT, CHECK (priority IN (1, 2, 3)))",
        )
        .expect("create");

    engine
        .execute_sql(&session, "INSERT INTO t VALUES (1)")
        .expect("in list");
    engine
        .execute_sql(&session, "INSERT INTO t VALUES (3)")
        .expect("in list");

    let err = engine
        .execute_sql(&session, "INSERT INTO t VALUES (4)")
        .expect_err("not in list");
    assert_eq!(err.sqlstate(), aiondb_core::SqlState::CheckViolation);
}

#[test]
fn check_constraint_with_between() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE t (score INT, CHECK (score BETWEEN 0 AND 100))",
        )
        .expect("create");

    engine
        .execute_sql(&session, "INSERT INTO t VALUES (50)")
        .expect("in range");
    engine
        .execute_sql(&session, "INSERT INTO t VALUES (0)")
        .expect("lower boundary");
    engine
        .execute_sql(&session, "INSERT INTO t VALUES (100)")
        .expect("upper boundary");

    let err = engine
        .execute_sql(&session, "INSERT INTO t VALUES (101)")
        .expect_err("above range");
    assert_eq!(err.sqlstate(), aiondb_core::SqlState::CheckViolation);
}

#[test]
fn check_constraint_with_is_not_null() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE t (name TEXT, CHECK (name IS NOT NULL))",
        )
        .expect("create");

    engine
        .execute_sql(&session, "INSERT INTO t VALUES ('alice')")
        .expect("non-null ok");

    let err = engine
        .execute_sql(&session, "INSERT INTO t VALUES (NULL)")
        .expect_err("null should fail");
    assert_eq!(err.sqlstate(), aiondb_core::SqlState::CheckViolation);
}

// ===================================================================
// INSERT ... SELECT with CHECK constraints
// ===================================================================

#[test]
fn insert_select_satisfying_check_succeeds() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE src (val INT)")
        .expect("create src");
    engine
        .execute_sql(&session, "CREATE TABLE dst (val INT, CHECK (val > 0))")
        .expect("create dst");
    engine
        .execute_sql(&session, "INSERT INTO src VALUES (10)")
        .expect("insert src");

    engine
        .execute_sql(&session, "INSERT INTO dst SELECT val FROM src")
        .expect("insert select should pass");
}

#[test]
fn insert_select_violating_check_fails() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE src (val INT)")
        .expect("create src");
    engine
        .execute_sql(&session, "CREATE TABLE dst (val INT, CHECK (val > 0))")
        .expect("create dst");
    engine
        .execute_sql(&session, "INSERT INTO src VALUES (-5)")
        .expect("insert src");

    let err = engine
        .execute_sql(&session, "INSERT INTO dst SELECT val FROM src")
        .expect_err("should violate CHECK");
    assert_eq!(err.sqlstate(), aiondb_core::SqlState::CheckViolation);
}

// ===================================================================
// Error reporting
// ===================================================================

#[test]
fn check_violation_error_code_is_23514() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE t (x INT, CHECK (x > 0))")
        .expect("create");

    let err = engine
        .execute_sql(&session, "INSERT INTO t VALUES (0)")
        .expect_err("should violate CHECK");
    assert_eq!(err.sqlstate(), aiondb_core::SqlState::CheckViolation);
    assert_eq!(err.sqlstate().code(), "23514");
}

#[test]
fn check_violation_message_includes_table_name() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE employees (age INT, CHECK (age > 0))",
        )
        .expect("create");

    let err = engine
        .execute_sql(&session, "INSERT INTO employees VALUES (-1)")
        .expect_err("should violate CHECK");
    assert!(
        err.report().message.contains("employees"),
        "error message should contain table name: {}",
        err.report().message
    );
}

// ===================================================================
// Table without CHECK constraints (no regression)
// ===================================================================

#[test]
fn table_without_check_constraints_works_normally() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE t (id INT, val INT)")
        .expect("create");
    engine
        .execute_sql(&session, "INSERT INTO t VALUES (1, -999)")
        .expect("any value ok without CHECK");
    engine
        .execute_sql(&session, "UPDATE t SET val = -1 WHERE id = 1")
        .expect("any value ok on update");
}

#[test]
fn polygon_column_registers_compat_check_constraint() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE polygon_tbl (f1 polygon)")
        .expect("create polygon table");

    let table = engine
        .catalog_reader
        .get_table(
            aiondb_core::TxnId::default(),
            &aiondb_catalog::QualifiedName::unqualified("polygon_tbl"),
        )
        .expect("catalog read")
        .expect("table exists");

    assert!(
        table.check_constraints.iter().any(|constraint| constraint
            .expression
            .contains("__aiondb_compat_cast")
            && constraint.expression.contains("polygon")),
        "expected compat geometric CHECK constraint, got: {:?}",
        table.check_constraints
    );
}

#[test]
fn polygon_invalid_literal_is_rejected_on_insert() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE polygon_tbl (f1 polygon)")
        .expect("create polygon table");

    let err = engine
        .execute_sql(&session, "INSERT INTO polygon_tbl(f1) VALUES ('asdf')")
        .expect_err("invalid polygon literal should fail");
    assert_eq!(
        err.sqlstate(),
        aiondb_core::SqlState::InvalidTextRepresentation
    );
    assert!(
        err.report()
            .message
            .contains("invalid input syntax for type polygon"),
        "unexpected message: {}",
        err.report().message
    );
}

#[test]
fn box_column_registers_compat_check_constraint() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE box_tbl (f1 box)")
        .expect("create box table");

    let table = engine
        .catalog_reader
        .get_table(
            aiondb_core::TxnId::default(),
            &aiondb_catalog::QualifiedName::unqualified("box_tbl"),
        )
        .expect("catalog read")
        .expect("table exists");

    assert!(
        table.check_constraints.iter().any(|constraint| constraint
            .expression
            .contains("__aiondb_compat_cast")
            && constraint.expression.contains("box")),
        "expected compat geometric CHECK constraint, got: {:?}",
        table.check_constraints
    );
}

#[test]
fn lseg_column_registers_compat_check_constraint() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE lseg_tbl (f1 lseg)")
        .expect("create lseg table");

    let table = engine
        .catalog_reader
        .get_table(
            aiondb_core::TxnId::default(),
            &aiondb_catalog::QualifiedName::unqualified("lseg_tbl"),
        )
        .expect("catalog read")
        .expect("table exists");

    assert!(
        table.check_constraints.iter().any(|constraint| constraint
            .expression
            .contains("__aiondb_compat_cast")
            && constraint.expression.contains("lseg")),
        "expected compat geometric CHECK constraint, got: {:?}",
        table.check_constraints
    );
}

#[test]
fn box_invalid_literal_is_rejected_on_insert() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE box_tbl (f1 box)")
        .expect("create box table");

    let err = engine
        .execute_sql(&session, "INSERT INTO box_tbl(f1) VALUES ('(2.3, 4.5)')")
        .expect_err("invalid box literal should fail");
    assert_eq!(
        err.sqlstate(),
        aiondb_core::SqlState::InvalidTextRepresentation
    );
    assert!(
        err.report()
            .message
            .contains("invalid input syntax for type box"),
        "unexpected message: {}",
        err.report().message
    );
}

#[test]
fn lseg_invalid_literal_is_rejected_on_insert() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE lseg_tbl (f1 lseg)")
        .expect("create lseg table");

    let err = engine
        .execute_sql(&session, "INSERT INTO lseg_tbl(f1) VALUES ('[(1,2),(3,4)')")
        .expect_err("invalid lseg literal should fail");
    assert_eq!(
        err.sqlstate(),
        aiondb_core::SqlState::InvalidTextRepresentation
    );
    assert!(
        err.report()
            .message
            .contains("invalid input syntax for type lseg"),
        "unexpected message: {}",
        err.report().message
    );
}
