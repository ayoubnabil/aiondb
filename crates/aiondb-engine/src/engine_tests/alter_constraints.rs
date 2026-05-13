use super::*;

// ===================================================================
// ALTER TABLE ADD CONSTRAINT ... CHECK
// ===================================================================

#[test]
fn alter_table_add_check_constraint() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE t (id INT NOT NULL, val INT)")
        .expect("create");

    // Add a CHECK constraint via ALTER TABLE
    let results = engine
        .execute_sql(
            &session,
            "ALTER TABLE t ADD CONSTRAINT chk_val CHECK (val > 0)",
        )
        .expect("add constraint");
    assert_eq!(
        results,
        vec![StatementResult::Command {
            tag: "ALTER TABLE".to_owned(),
            rows_affected: 0,
        }]
    );

    // Insert satisfying the constraint should succeed
    engine
        .execute_sql(&session, "INSERT INTO t VALUES (1, 10)")
        .expect("insert valid row");

    // Insert violating the constraint should fail
    let err = engine
        .execute_sql(&session, "INSERT INTO t VALUES (2, -1)")
        .expect_err("should violate CHECK");
    assert_eq!(err.sqlstate(), aiondb_core::SqlState::CheckViolation);
}

#[test]
fn alter_table_drop_check_constraint() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    // Create the table without an inline CHECK, then add the constraint via
    // ALTER TABLE so the constraint name is preserved in the catalog.
    // (CREATE TABLE currently discards inline constraint names.)
    engine
        .execute_sql(&session, "CREATE TABLE t (id INT NOT NULL, val INT)")
        .expect("create");

    engine
        .execute_sql(
            &session,
            "ALTER TABLE t ADD CONSTRAINT chk_val CHECK (val > 0)",
        )
        .expect("add constraint");

    // Before dropping: inserting a violating value should fail
    let err = engine
        .execute_sql(&session, "INSERT INTO t VALUES (1, -1)")
        .expect_err("should violate CHECK before drop");
    assert_eq!(err.sqlstate(), aiondb_core::SqlState::CheckViolation);

    // Drop the constraint
    let results = engine
        .execute_sql(&session, "ALTER TABLE t DROP CONSTRAINT chk_val")
        .expect("drop constraint");
    assert_eq!(
        results,
        vec![StatementResult::Command {
            tag: "ALTER TABLE".to_owned(),
            rows_affected: 0,
        }]
    );

    // After dropping: inserting the same violating value should succeed
    engine
        .execute_sql(&session, "INSERT INTO t VALUES (1, -1)")
        .expect("insert should succeed after dropping CHECK");
}

// ===================================================================
// ALTER TABLE ADD CONSTRAINT ... UNIQUE
// ===================================================================

#[test]
fn alter_table_add_unique_constraint_enforces_future_inserts() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE t (id INT NOT NULL, email TEXT)")
        .expect("create");

    let results = engine
        .execute_sql(
            &session,
            "ALTER TABLE t ADD CONSTRAINT uq_email UNIQUE (email)",
        )
        .expect("add unique constraint");
    assert_eq!(
        results,
        vec![StatementResult::Command {
            tag: "ALTER TABLE".to_owned(),
            rows_affected: 0,
        }]
    );

    engine
        .execute_sql(&session, "INSERT INTO t VALUES (1, 'alice@example.com')")
        .expect("first insert");

    let err = engine
        .execute_sql(&session, "INSERT INTO t VALUES (2, 'alice@example.com')")
        .expect_err("duplicate unique key should fail");
    assert_eq!(err.sqlstate(), aiondb_core::SqlState::UniqueViolation);
}

#[test]
fn alter_table_add_unique_constraint_rejects_existing_duplicates() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE t (id INT NOT NULL, email TEXT)")
        .expect("create");
    engine
        .execute_sql(
            &session,
            "INSERT INTO t VALUES (1, 'dupe@example.com'), (2, 'dupe@example.com')",
        )
        .expect("seed duplicates");

    let err = engine
        .execute_sql(
            &session,
            "ALTER TABLE t ADD CONSTRAINT uq_email UNIQUE (email)",
        )
        .expect_err("existing duplicates should block UNIQUE");
    assert_eq!(err.sqlstate(), aiondb_core::SqlState::UniqueViolation);
}

// ===================================================================
// ALTER TABLE ADD PRIMARY KEY
// ===================================================================

#[test]
fn alter_table_add_primary_key_enforces_uniqueness_and_not_null() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE t (id INT NOT NULL, name TEXT)")
        .expect("create");

    let results = engine
        .execute_sql(&session, "ALTER TABLE t ADD PRIMARY KEY (id)")
        .expect("add primary key");
    assert_eq!(
        results,
        vec![StatementResult::Command {
            tag: "ALTER TABLE".to_owned(),
            rows_affected: 0,
        }]
    );

    engine
        .execute_sql(&session, "INSERT INTO t VALUES (1, 'alice')")
        .expect("first insert");

    let duplicate = engine
        .execute_sql(&session, "INSERT INTO t VALUES (1, 'bob')")
        .expect_err("duplicate PK should fail");
    assert_eq!(duplicate.sqlstate(), aiondb_core::SqlState::UniqueViolation);

    let err = engine
        .execute_sql(&session, "INSERT INTO t VALUES (NULL, 'carol')")
        .expect_err("PRIMARY KEY NOT NULL must reject NULL insert");
    assert_eq!(err.sqlstate(), aiondb_core::SqlState::NotNullViolation);
}

#[test]
fn alter_table_add_primary_key_rejects_existing_nulls() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE t (id INT, name TEXT)")
        .expect("create");
    engine
        .execute_sql(&session, "INSERT INTO t VALUES (NULL, 'alice')")
        .expect("seed null");

    let err = engine
        .execute_sql(&session, "ALTER TABLE t ADD PRIMARY KEY (id)")
        .expect_err("existing NULL should block PRIMARY KEY");
    assert_eq!(err.sqlstate(), aiondb_core::SqlState::CheckViolation);
}

#[test]
fn alter_table_add_primary_key_using_index_blocks_dropping_backing_index() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE cwi_test (a INT, b INT)")
        .expect("create table");
    engine
        .execute_sql(
            &session,
            "CREATE UNIQUE INDEX cwi_uniq_idx ON cwi_test(a, b)",
        )
        .expect("create first unique index");
    engine
        .execute_sql(
            &session,
            "ALTER TABLE cwi_test ADD CONSTRAINT cwi_uniq_idx PRIMARY KEY USING INDEX cwi_uniq_idx",
        )
        .expect("attach first index as primary key");

    engine
        .execute_sql(
            &session,
            "CREATE UNIQUE INDEX cwi_uniq2_idx ON cwi_test(b, a)",
        )
        .expect("create second unique index");
    engine
        .execute_sql(
            &session,
            "ALTER TABLE cwi_test DROP CONSTRAINT cwi_uniq_idx, \
             ADD CONSTRAINT cwi_replaced_pkey PRIMARY KEY USING INDEX cwi_uniq2_idx",
        )
        .expect("replace primary key using second index");

    let err = engine
        .execute_sql(&session, "DROP INDEX cwi_replaced_pkey")
        .expect_err("dropping backing index should fail");
    assert_eq!(
        err.sqlstate(),
        aiondb_core::SqlState::DependentObjectsStillExist
    );
}

#[test]
fn alter_table_add_primary_key_using_index_without_named_constraint_blocks_drop() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE cwi_test (a INT, b INT)")
        .expect("create table");
    engine
        .execute_sql(
            &session,
            "CREATE UNIQUE INDEX cwi_uniq_idx ON cwi_test(a, b)",
        )
        .expect("create first unique index");
    engine
        .execute_sql(
            &session,
            "ALTER TABLE cwi_test ADD PRIMARY KEY USING INDEX cwi_uniq_idx",
        )
        .expect("attach first index as primary key");

    engine
        .execute_sql(
            &session,
            "CREATE UNIQUE INDEX cwi_uniq2_idx ON cwi_test(b, a)",
        )
        .expect("create second unique index");
    engine
        .execute_sql(
            &session,
            "ALTER TABLE cwi_test DROP CONSTRAINT cwi_uniq_idx, \
             ADD CONSTRAINT cwi_replaced_pkey PRIMARY KEY USING INDEX cwi_uniq2_idx",
        )
        .expect("replace primary key using second index");

    let err = engine
        .execute_sql(&session, "DROP INDEX cwi_replaced_pkey")
        .expect_err("dropping backing index should fail");
    assert_eq!(
        err.sqlstate(),
        aiondb_core::SqlState::DependentObjectsStillExist
    );
}

// ===================================================================
// ALTER TABLE ADD CONSTRAINT ... FOREIGN KEY
// ===================================================================

#[test]
fn alter_table_add_foreign_key() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE parent (id INT NOT NULL PRIMARY KEY)",
        )
        .expect("create parent");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE child (id INT NOT NULL, parent_id INT)",
        )
        .expect("create child");

    // Add a FOREIGN KEY constraint via ALTER TABLE
    let results = engine
        .execute_sql(
            &session,
            "ALTER TABLE child ADD CONSTRAINT fk_parent \
             FOREIGN KEY (parent_id) REFERENCES parent (id)",
        )
        .expect("add foreign key");
    assert_eq!(
        results,
        vec![StatementResult::Command {
            tag: "ALTER TABLE".to_owned(),
            rows_affected: 0,
        }]
    );

    // Insert a valid parent
    engine
        .execute_sql(&session, "INSERT INTO parent VALUES (1)")
        .expect("insert parent");

    // Insert a child with a valid FK reference should succeed
    engine
        .execute_sql(&session, "INSERT INTO child VALUES (1, 1)")
        .expect("insert child with valid FK");

    // Insert a child referencing a non-existent parent should fail
    let err = engine
        .execute_sql(&session, "INSERT INTO child VALUES (2, 99)")
        .expect_err("should violate FOREIGN KEY");
    let err_msg = format!("{err}");
    assert!(
        err_msg.contains("foreign key"),
        "error should mention foreign key: {err_msg}"
    );
}

// ===================================================================
// ALTER TABLE ALTER COLUMN TYPE
// ===================================================================

#[test]
fn alter_table_alter_column_type_basic() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE t_coltype (id INT NOT NULL, name TEXT)",
        )
        .expect("create");

    // ALTER COLUMN ... TYPE syntax
    let results = engine
        .execute_sql(
            &session,
            "ALTER TABLE t_coltype ALTER COLUMN name TYPE TEXT",
        )
        .expect("alter column type");
    assert_eq!(
        results,
        vec![StatementResult::Command {
            tag: "ALTER TABLE".to_owned(),
            rows_affected: 0,
        }]
    );
}

#[test]
fn alter_table_alter_column_set_data_type() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE t_setdata (id INT NOT NULL, val INT)",
        )
        .expect("create");

    // SET DATA TYPE syntax
    let results = engine
        .execute_sql(
            &session,
            "ALTER TABLE t_setdata ALTER COLUMN val SET DATA TYPE BIGINT",
        )
        .expect("set data type");
    assert_eq!(
        results,
        vec![StatementResult::Command {
            tag: "ALTER TABLE".to_owned(),
            rows_affected: 0,
        }]
    );
}

#[test]
fn alter_table_alter_column_type_unknown_column() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE t_noexist (id INT NOT NULL, val INT)",
        )
        .expect("create");

    // Altering a non-existent column should fail
    let err = engine
        .execute_sql(
            &session,
            "ALTER TABLE t_noexist ALTER COLUMN bogus TYPE TEXT",
        )
        .expect_err("should fail for unknown column");
    assert_eq!(err.sqlstate(), aiondb_core::SqlState::UndefinedColumn);
}

#[test]
fn alter_table_alter_column_type_with_btree_index_succeeds() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE t_coltype_idx (id INT NOT NULL, val INT)",
        )
        .expect("create table");
    engine
        .execute_sql(
            &session,
            "CREATE UNIQUE INDEX t_coltype_idx_val_uq ON t_coltype_idx (val)",
        )
        .expect("create index");
    engine
        .execute_sql(
            &session,
            "INSERT INTO t_coltype_idx VALUES (1, 10), (2, 20)",
        )
        .expect("seed rows");

    engine
        .execute_sql(
            &session,
            "ALTER TABLE t_coltype_idx ALTER COLUMN val TYPE BIGINT",
        )
        .expect("alter column type with btree index");

    // Ensure catalog-visible type changed and subsequent inserts still succeed.
    let rows = engine
        .execute_sql(
            &session,
            "SELECT atttypid AS typ_oid \
             FROM pg_attribute \
             WHERE attrelid = 't_coltype_idx'::regclass AND attname = 'val'",
        )
        .expect("inspect catalog type");
    let StatementResult::Query {
        rows: result_rows, ..
    } = &rows[0]
    else {
        panic!("expected query result, got {:?}", rows[0]);
    };
    assert_eq!(result_rows, &vec![Row::new(vec![Value::Int(20)])]);

    engine
        .execute_sql(&session, "INSERT INTO t_coltype_idx VALUES (3, 30)")
        .expect("insert after type change");
}

#[test]
fn alter_table_drop_not_null_rejects_primary_key_column() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE t_pk_not_null (id INT PRIMARY KEY, payload TEXT)",
        )
        .expect("create table");

    let err = engine
        .execute_sql(
            &session,
            "ALTER TABLE t_pk_not_null ALTER COLUMN id DROP NOT NULL",
        )
        .expect_err("primary key column must stay NOT NULL");
    assert_eq!(
        err.sqlstate(),
        aiondb_core::SqlState::InvalidTableDefinition
    );
}

#[test]
fn alter_table_drop_column_rejects_dependent_check_constraint() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE t_drop_col_chk (a INT, b INT)")
        .expect("create table");
    engine
        .execute_sql(
            &session,
            "ALTER TABLE t_drop_col_chk ADD CONSTRAINT t_drop_col_chk_a_pos CHECK (a > 0)",
        )
        .expect("add check");

    let err = engine
        .execute_sql(&session, "ALTER TABLE t_drop_col_chk DROP COLUMN a")
        .expect_err("check constraint should block column drop");
    assert_eq!(
        err.sqlstate(),
        aiondb_core::SqlState::DependentObjectsStillExist
    );
}

#[test]
fn alter_table_drop_column_rejects_dependent_foreign_key_constraint() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE t_fk_parent (id INT PRIMARY KEY)")
        .expect("create parent");
    engine
        .execute_sql(
            &session,
            "CREATE TABLE t_fk_child (id INT PRIMARY KEY, parent_id INT)",
        )
        .expect("create child");
    engine
        .execute_sql(
            &session,
            "ALTER TABLE t_fk_child ADD CONSTRAINT t_fk_child_parent_fk \
             FOREIGN KEY (parent_id) REFERENCES t_fk_parent(id)",
        )
        .expect("add fk");

    let err = engine
        .execute_sql(&session, "ALTER TABLE t_fk_child DROP COLUMN parent_id")
        .expect_err("foreign key should block column drop");
    assert_eq!(
        err.sqlstate(),
        aiondb_core::SqlState::DependentObjectsStillExist
    );
}
