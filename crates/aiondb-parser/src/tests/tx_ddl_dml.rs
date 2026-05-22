#![allow(clippy::match_wildcard_for_single_variants)]

use crate::*;

#[test]
fn tx_simple_begin() {
    let stmt = parse_prepared_statement("BEGIN").expect("parse");
    let Statement::Begin { mode, .. } = stmt else {
        panic!("expected BEGIN");
    };
    assert_eq!(mode, None);
}

#[test]
fn tx_begin_isolation_level_read_committed() {
    let stmt = parse_prepared_statement("BEGIN ISOLATION LEVEL READ COMMITTED").expect("parse");
    let Statement::Begin { mode, .. } = stmt else {
        panic!("expected BEGIN");
    };
    assert_eq!(mode, Some(TransactionMode::ReadCommitted));
}

#[test]
fn tx_begin_isolation_level_snapshot_isolation() {
    let stmt = parse_prepared_statement("BEGIN ISOLATION LEVEL SNAPSHOT ISOLATION").expect("parse");
    let Statement::Begin { mode, .. } = stmt else {
        panic!("expected BEGIN");
    };
    assert_eq!(mode, Some(TransactionMode::SnapshotIsolation));
}

#[test]
fn tx_begin_isolation_level_serializable() {
    let stmt = parse_prepared_statement("BEGIN ISOLATION LEVEL SERIALIZABLE").expect("parse");
    let Statement::Begin {
        mode,
        read_only,
        deferrable,
        ..
    } = stmt
    else {
        panic!("expected BEGIN");
    };
    assert_eq!(mode, Some(TransactionMode::Serializable));
    assert_eq!(read_only, None);
    assert_eq!(deferrable, None);
}

#[test]
fn tx_begin_read_only_deferrable() {
    let stmt = parse_prepared_statement("BEGIN READ ONLY, DEFERRABLE").expect("parse");
    let Statement::Begin {
        mode,
        read_only,
        deferrable,
        ..
    } = stmt
    else {
        panic!("expected BEGIN");
    };
    assert_eq!(mode, None);
    assert_eq!(read_only, Some(true));
    assert_eq!(deferrable, Some(true));
}

#[test]
fn tx_start_transaction() {
    let stmt = parse_prepared_statement("START TRANSACTION").expect("parse");
    let Statement::Begin { mode, .. } = stmt else {
        panic!("expected BEGIN (from START TRANSACTION)");
    };
    assert_eq!(mode, None);
}

#[test]
fn tx_start_transaction_read_committed() {
    let stmt = parse_prepared_statement("START TRANSACTION ISOLATION LEVEL READ COMMITTED")
        .expect("parse");
    let Statement::Begin { mode, .. } = stmt else {
        panic!("expected BEGIN");
    };
    assert_eq!(mode, Some(TransactionMode::ReadCommitted));
}

#[test]
fn tx_start_transaction_snapshot_isolation() {
    let stmt = parse_prepared_statement("START TRANSACTION ISOLATION LEVEL SNAPSHOT ISOLATION")
        .expect("parse");
    let Statement::Begin { mode, .. } = stmt else {
        panic!("expected BEGIN");
    };
    assert_eq!(mode, Some(TransactionMode::SnapshotIsolation));
}

#[test]
fn tx_start_transaction_serializable() {
    let stmt =
        parse_prepared_statement("START TRANSACTION ISOLATION LEVEL SERIALIZABLE").expect("parse");
    let Statement::Begin {
        mode,
        read_only,
        deferrable,
        ..
    } = stmt
    else {
        panic!("expected BEGIN");
    };
    assert_eq!(mode, Some(TransactionMode::Serializable));
    assert_eq!(read_only, None);
    assert_eq!(deferrable, None);
}

#[test]
fn tx_set_transaction_parses_options() {
    let stmt = parse_prepared_statement(
        "SET TRANSACTION ISOLATION LEVEL READ COMMITTED, READ WRITE, NOT DEFERRABLE",
    )
    .expect("parse");
    let Statement::SetTransaction(control) = stmt else {
        panic!("expected SetTransaction");
    };
    assert_eq!(control.isolation, Some(TransactionMode::ReadCommitted));
    assert_eq!(control.read_only, Some(false));
    assert_eq!(control.deferrable, Some(false));
}

#[test]
fn tx_set_session_characteristics_parses_options() {
    let stmt = parse_prepared_statement(
        "SET SESSION CHARACTERISTICS AS TRANSACTION ISOLATION LEVEL SNAPSHOT ISOLATION, READ ONLY",
    )
    .expect("parse");
    let Statement::SetSessionCharacteristics(control) = stmt else {
        panic!("expected SetSessionCharacteristics");
    };
    assert_eq!(control.isolation, Some(TransactionMode::SnapshotIsolation));
    assert_eq!(control.read_only, Some(true));
    assert_eq!(control.deferrable, None);
}

#[test]
fn tx_simple_commit() {
    let stmt = parse_prepared_statement("COMMIT").expect("parse");
    assert!(matches!(stmt, Statement::Commit { .. }));
}

#[test]
fn tx_simple_rollback() {
    let stmt = parse_prepared_statement("ROLLBACK").expect("parse");
    assert!(matches!(stmt, Statement::Rollback { .. }));
}

#[test]
fn tx_begin_commit_rollback_multi_statement() {
    let stmts = parse_sql("BEGIN; COMMIT; ROLLBACK;").expect("parse");
    assert_eq!(stmts.len(), 3);
    assert!(matches!(stmts[0], Statement::Begin { .. }));
    assert!(matches!(stmts[1], Statement::Commit { .. }));
    assert!(matches!(stmts[2], Statement::Rollback { .. }));
}

#[test]
fn tx_begin_isolation_level_then_invalid_mode_error() {
    let result = parse_prepared_statement("BEGIN ISOLATION LEVEL DELETE");
    assert!(result.is_err());
}

#[test]
fn tx_start_without_transaction_error() {
    let result = parse_prepared_statement("START");
    assert!(result.is_err());
}

#[test]
fn select_for_update_is_accepted_for_pg_compat() {
    let stmt = parse_prepared_statement("SELECT * FROM t FOR UPDATE")
        .expect("FOR UPDATE should parse for pg compatibility");
    assert!(matches!(stmt, Statement::Select(_)));
}

#[test]
fn select_for_share_is_accepted_for_pg_compat() {
    let stmt = parse_prepared_statement("SELECT * FROM t FOR SHARE")
        .expect("FOR SHARE should parse for pg compatibility");
    assert!(matches!(stmt, Statement::Select(_)));
}

#[test]
fn set_operation_for_update_errors() {
    let err = parse_prepared_statement("SELECT 1 UNION SELECT 1 FOR NO KEY UPDATE")
        .expect_err("set operation row locking must fail");
    assert!(
        err.to_string()
            .contains("FOR NO KEY UPDATE is not allowed with UNION/INTERSECT/EXCEPT"),
        "got: {err}"
    );
}

// ── Savepoint parsing ─────────────────────────────────────────

#[test]
fn tx_savepoint_basic() {
    let stmt = parse_prepared_statement("SAVEPOINT sp1").expect("parse");
    let Statement::Savepoint { name, .. } = stmt else {
        panic!("expected SAVEPOINT");
    };
    assert_eq!(name, "sp1");
}

#[test]
fn tx_rollback_to_savepoint() {
    let stmt = parse_prepared_statement("ROLLBACK TO SAVEPOINT sp1").expect("parse");
    let Statement::RollbackToSavepoint { name, .. } = stmt else {
        panic!("expected ROLLBACK TO SAVEPOINT");
    };
    assert_eq!(name, "sp1");
}

#[test]
fn tx_rollback_to_savepoint_without_savepoint_keyword() {
    let stmt = parse_prepared_statement("ROLLBACK TO sp1").expect("parse");
    let Statement::RollbackToSavepoint { name, .. } = stmt else {
        panic!("expected ROLLBACK TO SAVEPOINT");
    };
    assert_eq!(name, "sp1");
}

#[test]
fn tx_release_savepoint() {
    let stmt = parse_prepared_statement("RELEASE SAVEPOINT sp1").expect("parse");
    let Statement::ReleaseSavepoint { name, .. } = stmt else {
        panic!("expected RELEASE SAVEPOINT");
    };
    assert_eq!(name, "sp1");
}

#[test]
fn tx_release_savepoint_without_savepoint_keyword() {
    let stmt = parse_prepared_statement("RELEASE sp1").expect("parse");
    let Statement::ReleaseSavepoint { name, .. } = stmt else {
        panic!("expected RELEASE SAVEPOINT");
    };
    assert_eq!(name, "sp1");
}

#[test]
fn tx_savepoint_missing_name_error() {
    let result = parse_prepared_statement("SAVEPOINT");
    assert!(result.is_err());
}

#[test]
fn tx_release_missing_name_error() {
    let result = parse_prepared_statement("RELEASE");
    assert!(result.is_err());
}

#[test]
fn tx_rollback_to_missing_name_error() {
    let result = parse_prepared_statement("ROLLBACK TO");
    assert!(result.is_err());
}

#[test]
fn tx_savepoint_multi_statement() {
    let stmts =
        parse_sql("BEGIN; SAVEPOINT sp1; ROLLBACK TO SAVEPOINT sp1; RELEASE SAVEPOINT sp1; COMMIT")
            .expect("parse");
    assert_eq!(stmts.len(), 5);
    assert!(matches!(stmts[0], Statement::Begin { .. }));
    assert!(matches!(stmts[1], Statement::Savepoint { .. }));
    assert!(matches!(stmts[2], Statement::RollbackToSavepoint { .. }));
    assert!(matches!(stmts[3], Statement::ReleaseSavepoint { .. }));
    assert!(matches!(stmts[4], Statement::Commit { .. }));
}

// ═══════════════════════════════════════════════════════════════
//  DDL PARSING TESTS (parser_ddl.rs)
// ═══════════════════════════════════════════════════════════════

#[test]
fn ddl_create_table_all_data_types() {
    let sql = "CREATE TABLE t (
            c1 INT,
            c2 BIGINT,
            c3 REAL,
            c4 DOUBLE,
            c5 NUMERIC,
            c6 TEXT,
            c7 BOOLEAN,
            c8 BLOB,
            c9 TIMESTAMP,
            c10 DATE,
            c11 INTERVAL
        )";
    let stmt = parse_prepared_statement(sql).expect("parse");
    let Statement::CreateTable(ct) = stmt else {
        panic!("expected CREATE TABLE");
    };
    assert_eq!(ct.columns.len(), 11);
    assert_eq!(ct.columns[0].data_type, aiondb_core::DataType::Int);
    assert_eq!(ct.columns[1].data_type, aiondb_core::DataType::BigInt);
    assert_eq!(ct.columns[2].data_type, aiondb_core::DataType::Real);
    assert_eq!(ct.columns[3].data_type, aiondb_core::DataType::Double);
    assert_eq!(ct.columns[4].data_type, aiondb_core::DataType::Numeric);
    assert_eq!(ct.columns[5].data_type, aiondb_core::DataType::Text);
    assert_eq!(ct.columns[6].data_type, aiondb_core::DataType::Boolean);
    assert_eq!(ct.columns[7].data_type, aiondb_core::DataType::Blob);
    assert_eq!(ct.columns[8].data_type, aiondb_core::DataType::Timestamp);
    assert_eq!(ct.columns[9].data_type, aiondb_core::DataType::Date);
    assert_eq!(ct.columns[10].data_type, aiondb_core::DataType::Interval);
}

#[test]
fn ddl_create_table_single_column() {
    let stmt = parse_prepared_statement("CREATE TABLE t (id INT)").expect("parse");
    let Statement::CreateTable(ct) = stmt else {
        panic!("expected CREATE TABLE");
    };
    assert_eq!(ct.name.parts, vec!["t".to_owned()]);
    assert_eq!(ct.columns.len(), 1);
    assert_eq!(ct.columns[0].name, "id");
    assert_eq!(ct.columns[0].data_type, aiondb_core::DataType::Int);
}

#[test]
fn ddl_create_table_many_columns() {
    let sql = "CREATE TABLE t (c1 INT, c2 INT, c3 INT, c4 INT, c5 INT, c6 INT, c7 INT, c8 INT, c9 INT, c10 INT)";
    let stmt = parse_prepared_statement(sql).expect("parse");
    let Statement::CreateTable(ct) = stmt else {
        panic!("expected CREATE TABLE");
    };
    assert_eq!(ct.columns.len(), 10);
}

#[test]
fn ddl_create_table_missing_lparen_error() {
    let result = parse_prepared_statement("CREATE TABLE t id INT)");
    assert!(result.is_err());
}

#[test]
fn ddl_create_table_missing_rparen_error() {
    let result = parse_prepared_statement("CREATE TABLE t (id INT");
    assert!(result.is_err());
}

#[test]
fn ddl_create_table_user_defined_type_accepted() {
    // User-defined / unknown types are accepted and mapped to Text.
    let result = parse_prepared_statement("CREATE TABLE t (id FOOBAR)");
    assert!(result.is_ok());
}

#[test]
fn ddl_create_table_point_keeps_raw_type_name() {
    let stmt = parse_prepared_statement("CREATE TABLE t (p point)").expect("parse");
    let Statement::CreateTable(ct) = stmt else {
        panic!("expected CREATE TABLE");
    };
    assert_eq!(ct.columns.len(), 1);
    assert_eq!(
        ct.columns[0].raw_type_name.as_deref(),
        Some("point"),
        "point should keep raw type name for compat handling"
    );
}

#[test]
fn ddl_create_table_sparsevec_keeps_raw_type_name() {
    let stmt = parse_prepared_statement("CREATE TABLE t (s SPARSEVEC(5))").expect("parse");
    let Statement::CreateTable(ct) = stmt else {
        panic!("expected CREATE TABLE");
    };
    assert_eq!(ct.columns.len(), 1);
    assert_eq!(
        ct.columns[0].data_type,
        aiondb_core::DataType::Vector {
            dims: 5,
            element_type: aiondb_core::VectorElementType::Float32
        }
    );
    assert_eq!(ct.columns[0].raw_type_name.as_deref(), Some("sparsevec"));
}

#[test]
fn ddl_create_table_no_columns_empty_parens_accepted() {
    // PG allows empty column lists; used in partition definitions etc.
    let result = parse_prepared_statement("CREATE TABLE t ()");
    assert!(result.is_ok());
}

#[test]
fn ddl_create_index_single_column() {
    let stmt = parse_prepared_statement("CREATE INDEX idx ON users (id)").expect("parse");
    let Statement::CreateIndex(ci) = stmt else {
        panic!("expected CREATE INDEX");
    };
    assert_eq!(ci.name.parts, vec!["idx".to_owned()]);
    assert_eq!(ci.table.parts, vec!["users".to_owned()]);
    assert_eq!(ci.columns.len(), 1);
    assert_eq!(ci.columns[0].parts, vec!["id".to_owned()]);
}

#[test]
fn ddl_create_index_multiple_columns() {
    let stmt = parse_prepared_statement("CREATE INDEX idx ON users (id, name)").expect("parse");
    let Statement::CreateIndex(ci) = stmt else {
        panic!("expected CREATE INDEX");
    };
    assert_eq!(ci.columns.len(), 2);
    assert_eq!(ci.columns[0].parts, vec!["id".to_owned()]);
    assert_eq!(ci.columns[1].parts, vec!["name".to_owned()]);
}

#[test]
fn ddl_create_index_missing_on_error() {
    let result = parse_prepared_statement("CREATE INDEX idx users (id)");
    assert!(result.is_err());
}

#[test]
fn ddl_create_index_missing_table_name_error() {
    let result = parse_prepared_statement("CREATE INDEX idx ON (id)");
    assert!(result.is_err());
}

#[test]
fn ddl_create_index_missing_column_list_error() {
    let result = parse_prepared_statement("CREATE INDEX idx ON users");
    assert!(result.is_err());
}

#[test]
fn ddl_create_index_using_gin_is_accepted() {
    let stmt = parse_prepared_statement("CREATE INDEX idx ON users USING gin (id)").expect("parse");
    let Statement::CreateIndex(ci) = stmt else {
        panic!("expected CREATE INDEX");
    };
    assert_eq!(ci.method, Some(IndexMethod::Gin));
}

#[test]
fn ddl_create_index_using_hash_is_accepted() {
    let stmt =
        parse_prepared_statement("CREATE INDEX idx ON users USING hash (id)").expect("parse");
    let Statement::CreateIndex(ci) = stmt else {
        panic!("expected CREATE INDEX");
    };
    assert_eq!(ci.method, Some(IndexMethod::Hash));
}

#[test]
fn ddl_create_index_using_gist_is_accepted() {
    let stmt =
        parse_prepared_statement("CREATE INDEX idx ON users USING gist (id)").expect("parse");
    let Statement::CreateIndex(ci) = stmt else {
        panic!("expected CREATE INDEX");
    };
    assert_eq!(ci.method, Some(IndexMethod::Gist));
}

#[test]
fn ddl_create_index_hnsw_with_integer_options() {
    let stmt = parse_prepared_statement(
        "CREATE INDEX idx ON docs USING hnsw (embedding) WITH (m = 32, ef_construction = 400)",
    )
    .expect("parse");
    let Statement::CreateIndex(ci) = stmt else {
        panic!("expected CREATE INDEX");
    };
    assert_eq!(ci.method, Some(IndexMethod::Hnsw));
    assert_eq!(ci.with_options.len(), 2);
    assert_eq!(ci.with_options[0].key, "m");
    assert_eq!(ci.with_options[0].as_integer(), Some(32));
    assert_eq!(ci.with_options[1].key, "ef_construction");
    assert_eq!(ci.with_options[1].as_integer(), Some(400));
}

#[test]
fn ddl_create_index_hnsw_with_string_options() {
    let stmt = parse_prepared_statement(
        "CREATE INDEX idx ON docs USING hnsw (embedding) \
         WITH (distance = 'cosine', quantization = 'sq')",
    )
    .expect("parse");
    let Statement::CreateIndex(ci) = stmt else {
        panic!("expected CREATE INDEX");
    };
    assert_eq!(ci.method, Some(IndexMethod::Hnsw));
    assert_eq!(ci.with_options.len(), 2);
    assert_eq!(ci.with_options[0].key, "distance");
    assert_eq!(ci.with_options[0].as_string(), Some("cosine"));
    assert_eq!(ci.with_options[1].key, "quantization");
    assert_eq!(ci.with_options[1].as_string(), Some("sq"));
}

#[test]
fn ddl_create_index_ivfflat_with_lists_option() {
    let stmt = parse_prepared_statement(
        "CREATE INDEX idx ON docs USING ivfflat (embedding) WITH (lists = 100)",
    )
    .expect("parse");
    let Statement::CreateIndex(ci) = stmt else {
        panic!("expected CREATE INDEX");
    };
    assert_eq!(ci.method, Some(IndexMethod::IvfFlat));
    assert_eq!(ci.with_options.len(), 1);
    assert_eq!(ci.with_options[0].key, "lists");
    assert_eq!(ci.with_options[0].as_integer(), Some(100));
}

#[test]
fn ddl_create_index_pgvector_operator_class() {
    let stmt = parse_prepared_statement(
        "CREATE INDEX idx ON docs USING hnsw (embedding vector_cosine_ops)",
    )
    .expect("parse");
    let Statement::CreateIndex(ci) = stmt else {
        panic!("expected CREATE INDEX");
    };
    assert_eq!(ci.method, Some(IndexMethod::Hnsw));
    assert_eq!(ci.columns.len(), 1);
    assert_eq!(
        ci.operator_classes,
        vec![Some("vector_cosine_ops".to_owned())]
    );
}

#[test]
fn ddl_create_index_pgvector_halfvec_operator_class() {
    let stmt = parse_prepared_statement(
        "CREATE INDEX idx ON docs USING hnsw (embedding halfvec_cosine_ops)",
    )
    .expect("parse");
    let Statement::CreateIndex(ci) = stmt else {
        panic!("expected CREATE INDEX");
    };
    assert_eq!(ci.method, Some(IndexMethod::Hnsw));
    assert_eq!(
        ci.operator_classes,
        vec![Some("halfvec_cosine_ops".to_owned())]
    );
}

#[test]
fn ddl_create_index_pgvector_sparsevec_operator_class() {
    let stmt = parse_prepared_statement(
        "CREATE INDEX idx ON docs USING hnsw (embedding sparsevec_l1_ops)",
    )
    .expect("parse");
    let Statement::CreateIndex(ci) = stmt else {
        panic!("expected CREATE INDEX");
    };
    assert_eq!(ci.method, Some(IndexMethod::Hnsw));
    assert_eq!(
        ci.operator_classes,
        vec![Some("sparsevec_l1_ops".to_owned())]
    );
}

#[test]
fn ddl_create_index_pgvector_bit_operator_class() {
    let stmt =
        parse_prepared_statement("CREATE INDEX idx ON docs USING hnsw (embedding bit_jaccard_ops)")
            .expect("parse");
    let Statement::CreateIndex(ci) = stmt else {
        panic!("expected CREATE INDEX");
    };
    assert_eq!(ci.method, Some(IndexMethod::Hnsw));
    assert_eq!(
        ci.operator_classes,
        vec![Some("bit_jaccard_ops".to_owned())]
    );
}

#[test]
fn ddl_create_extension_accepts_vector_keyword_name() {
    let stmt = parse_prepared_statement("CREATE EXTENSION IF NOT EXISTS vector").expect("parse");
    let Statement::CreateExtension(extension) = stmt else {
        panic!("expected CREATE EXTENSION");
    };
    assert_eq!(extension.name, "vector");
    assert!(extension.if_not_exists);
}

#[test]
fn ddl_create_index_hnsw_mixed_options() {
    let stmt = parse_prepared_statement(
        "CREATE INDEX idx ON docs USING hnsw (embedding) \
         WITH (m = 16, distance = 'manhattan', ef_construction = 200, quantization = 'bq')",
    )
    .expect("parse");
    let Statement::CreateIndex(ci) = stmt else {
        panic!("expected CREATE INDEX");
    };
    assert_eq!(ci.with_options.len(), 4);
    assert_eq!(ci.with_options[0].as_integer(), Some(16));
    assert_eq!(ci.with_options[1].as_string(), Some("manhattan"));
    assert_eq!(ci.with_options[2].as_integer(), Some(200));
    assert_eq!(ci.with_options[3].as_string(), Some("bq"));
}

#[test]
fn ddl_create_sequence() {
    let stmt = parse_prepared_statement("CREATE SEQUENCE user_id_seq").expect("parse");
    let Statement::CreateSequence(sequence) = stmt else {
        panic!("expected CREATE SEQUENCE");
    };
    assert_eq!(sequence.name.parts, vec!["user_id_seq".to_owned()]);
}

#[test]
fn ddl_schema_qualified_create_sequence() {
    let stmt = parse_prepared_statement("CREATE SEQUENCE myschema.user_id_seq").expect("parse");
    let Statement::CreateSequence(sequence) = stmt else {
        panic!("expected CREATE SEQUENCE");
    };
    assert_eq!(
        sequence.name.parts,
        vec!["myschema".to_owned(), "user_id_seq".to_owned()]
    );
}

#[test]
fn ddl_create_sequence_missing_name_error() {
    let result = parse_prepared_statement("CREATE SEQUENCE");
    assert!(result.is_err());
}

#[test]
fn ddl_drop_table() {
    let stmt = parse_prepared_statement("DROP TABLE users").expect("parse");
    let Statement::DropTable(drop_table) = stmt else {
        panic!("expected DROP TABLE");
    };
    assert_eq!(drop_table.name.parts, vec!["users".to_owned()]);
}

#[test]
fn ddl_drop_index() {
    let stmt = parse_prepared_statement("DROP INDEX users_id_idx").expect("parse");
    let Statement::DropIndex(drop_index) = stmt else {
        panic!("expected DROP INDEX");
    };
    assert_eq!(drop_index.name.parts, vec!["users_id_idx".to_owned()]);
}

#[test]
fn ddl_drop_sequence() {
    let stmt = parse_prepared_statement("DROP SEQUENCE user_id_seq").expect("parse");
    let Statement::DropSequence(drop_sequence) = stmt else {
        panic!("expected DROP SEQUENCE");
    };
    assert_eq!(drop_sequence.name.parts, vec!["user_id_seq".to_owned()]);
}

#[test]
fn ddl_drop_missing_name_error() {
    let result = parse_prepared_statement("DROP TABLE");
    assert!(result.is_err());
}

#[test]
fn ddl_drop_unknown_object_returns_noop() {
    // Unknown DROP objects are parsed as parser-level compat stubs.
    let stmt = parse_prepared_statement("DROP FOOBAR users").expect("should parse as noop");
    assert!(matches!(stmt, Statement::CompatTagged(_)));
    assert_eq!(stmt.compat_tag(), Some("DROP FOOBAR"));
}

#[test]
fn ddl_create_unknown_object_returns_noop() {
    // Unknown CREATE objects are parsed as parser-level compat stubs.
    let stmt = parse_prepared_statement("CREATE foo bar").expect("should parse as noop");
    assert!(matches!(stmt, Statement::CompatTagged(_)));
    assert_eq!(stmt.compat_tag(), Some("CREATE FOO"));
}

#[test]
fn ddl_create_bare_unknown_object_is_syntax_error() {
    let err = parse_prepared_statement("CREATE foo").expect_err("should fail");
    assert!(format!("{err}").contains("syntax error at or near \"foo\""));
}

#[test]
fn ddl_schema_qualified_create_table() {
    let stmt = parse_prepared_statement("CREATE TABLE myschema.users (id INT)").expect("parse");
    let Statement::CreateTable(ct) = stmt else {
        panic!("expected CREATE TABLE");
    };
    assert_eq!(
        ct.name.parts,
        vec!["myschema".to_owned(), "users".to_owned()]
    );
}

// ═══════════════════════════════════════════════════════════════
//  DDL CONSTRAINT PARSING TESTS
// ═══════════════════════════════════════════════════════════════

#[test]
fn ddl_create_table_inline_primary_key() {
    let stmt =
        parse_prepared_statement("CREATE TABLE t (id INT PRIMARY KEY, name TEXT)").expect("parse");
    let Statement::CreateTable(ct) = stmt else {
        panic!("expected CREATE TABLE");
    };
    assert_eq!(ct.columns.len(), 2);
    assert!(ct.columns[0].primary_key);
    assert!(!ct.columns[0].unique);
    assert!(!ct.columns[1].primary_key);
}

#[test]
fn ddl_create_table_inline_unique() {
    let stmt =
        parse_prepared_statement("CREATE TABLE t (id INT, email TEXT UNIQUE)").expect("parse");
    let Statement::CreateTable(ct) = stmt else {
        panic!("expected CREATE TABLE");
    };
    assert!(!ct.columns[0].unique);
    assert!(ct.columns[1].unique);
}

#[test]
fn ddl_create_table_table_level_primary_key() {
    let sql = "CREATE TABLE t (id INT, name TEXT, PRIMARY KEY (id))";
    let stmt = parse_prepared_statement(sql).expect("parse");
    let Statement::CreateTable(ct) = stmt else {
        panic!("expected CREATE TABLE");
    };
    assert_eq!(ct.constraints.len(), 1);
    match &ct.constraints[0] {
        TableConstraint::PrimaryKey { columns, name, .. } => {
            assert_eq!(columns, &["id".to_owned()]);
            assert!(name.is_none());
        }
        _ => panic!("expected PrimaryKey constraint"),
    }
}

#[test]
fn ddl_create_table_table_level_unique() {
    let sql = "CREATE TABLE t (id INT, email TEXT, UNIQUE (email))";
    let stmt = parse_prepared_statement(sql).expect("parse");
    let Statement::CreateTable(ct) = stmt else {
        panic!("expected CREATE TABLE");
    };
    assert_eq!(ct.constraints.len(), 1);
    match &ct.constraints[0] {
        TableConstraint::Unique { columns, .. } => {
            assert_eq!(columns, &["email".to_owned()]);
        }
        _ => panic!("expected Unique constraint"),
    }
}

#[test]
fn ddl_create_table_composite_primary_key() {
    let sql = "CREATE TABLE t (a INT, b INT, PRIMARY KEY (a, b))";
    let stmt = parse_prepared_statement(sql).expect("parse");
    let Statement::CreateTable(ct) = stmt else {
        panic!("expected CREATE TABLE");
    };
    assert_eq!(ct.constraints.len(), 1);
    match &ct.constraints[0] {
        TableConstraint::PrimaryKey { columns, .. } => {
            assert_eq!(columns, &["a".to_owned(), "b".to_owned()]);
        }
        _ => panic!("expected PrimaryKey constraint"),
    }
}

#[test]
fn ddl_create_table_named_constraint() {
    let sql = "CREATE TABLE t (id INT, CONSTRAINT pk_t PRIMARY KEY (id))";
    let stmt = parse_prepared_statement(sql).expect("parse");
    let Statement::CreateTable(ct) = stmt else {
        panic!("expected CREATE TABLE");
    };
    assert_eq!(ct.constraints.len(), 1);
    match &ct.constraints[0] {
        TableConstraint::PrimaryKey { columns, name, .. } => {
            assert_eq!(name.as_deref(), Some("pk_t"));
            assert_eq!(columns, &["id".to_owned()]);
        }
        _ => panic!("expected PrimaryKey constraint"),
    }
}

#[test]
fn ddl_create_table_check_constraint() {
    let sql = "CREATE TABLE t (age INT, CHECK (age > 0))";
    let stmt = parse_prepared_statement(sql).expect("parse");
    let Statement::CreateTable(ct) = stmt else {
        panic!("expected CREATE TABLE");
    };
    assert_eq!(ct.constraints.len(), 1);
    assert!(matches!(ct.constraints[0], TableConstraint::Check { .. }));
}

#[test]
fn ddl_create_table_foreign_key() {
    let sql =
        "CREATE TABLE orders (id INT, user_id INT, FOREIGN KEY (user_id) REFERENCES users (id))";
    let stmt = parse_prepared_statement(sql).expect("parse");
    let Statement::CreateTable(ct) = stmt else {
        panic!("expected CREATE TABLE");
    };
    assert_eq!(ct.constraints.len(), 1);
    match &ct.constraints[0] {
        TableConstraint::ForeignKey {
            columns,
            ref_table,
            ref_columns,
            ..
        } => {
            assert_eq!(columns, &["user_id".to_owned()]);
            assert_eq!(ref_table.parts, vec!["users".to_owned()]);
            assert_eq!(ref_columns, &["id".to_owned()]);
        }
        _ => panic!("expected ForeignKey constraint"),
    }
}

#[test]
fn ddl_create_table_inline_references_skipped_leniently() {
    // Inline REFERENCES is skipped (not a hard error) - the table is
    // created without the FK constraint.
    let sql = "CREATE TABLE orders (id INT, user_id INT REFERENCES users (id))";
    let stmt = parse_prepared_statement(sql).expect("parse");
    let Statement::CreateTable(ct) = stmt else {
        panic!("expected CreateTable");
    };
    assert_eq!(ct.columns.len(), 2);
}

#[test]
fn ddl_create_table_multiple_constraints() {
    let sql = "CREATE TABLE t (id INT, email TEXT, PRIMARY KEY (id), UNIQUE (email))";
    let stmt = parse_prepared_statement(sql).expect("parse");
    let Statement::CreateTable(ct) = stmt else {
        panic!("expected CREATE TABLE");
    };
    assert_eq!(ct.constraints.len(), 2);
    assert!(matches!(
        ct.constraints[0],
        TableConstraint::PrimaryKey { .. }
    ));
    assert!(matches!(ct.constraints[1], TableConstraint::Unique { .. }));
}

// ═══════════════════════════════════════════════════════════════
//  DML PARSING TESTS (parser_dml.rs)
// ═══════════════════════════════════════════════════════════════

#[test]
fn dml_insert_single_row_single_value() {
    let stmt = parse_prepared_statement("INSERT INTO t VALUES (1)").expect("parse");
    let Statement::Insert(ins) = stmt else {
        panic!("expected INSERT");
    };
    assert_eq!(ins.table.parts, vec!["t".to_owned()]);
    assert!(ins.columns.is_empty());
    assert!(ins.query.is_none());
    assert_eq!(ins.rows.len(), 1);
    assert_eq!(ins.rows[0].len(), 1);
    assert!(matches!(
        ins.rows[0][0],
        Expr::Literal(Literal::Integer(1), _)
    ));
}

#[test]
fn dml_insert_single_row_multi_values() {
    let stmt =
        parse_prepared_statement("INSERT INTO t VALUES (1, 'a', TRUE, NULL)").expect("parse");
    let Statement::Insert(ins) = stmt else {
        panic!("expected INSERT");
    };
    assert_eq!(ins.rows.len(), 1);
    assert!(ins.query.is_none());
    assert_eq!(ins.rows[0].len(), 4);
    assert!(matches!(
        ins.rows[0][0],
        Expr::Literal(Literal::Integer(1), _)
    ));
    assert!(matches!(
        ins.rows[0][1],
        Expr::Literal(Literal::String(_), _)
    ));
    assert!(matches!(
        ins.rows[0][2],
        Expr::Literal(Literal::Boolean(true), _)
    ));
    assert!(matches!(ins.rows[0][3], Expr::Literal(Literal::Null, _)));
}

#[test]
fn dml_insert_multiple_rows() {
    let stmt = parse_prepared_statement("INSERT INTO t VALUES (1), (2), (3)").expect("parse");
    let Statement::Insert(ins) = stmt else {
        panic!("expected INSERT");
    };
    assert!(ins.columns.is_empty());
    assert!(ins.query.is_none());
    assert_eq!(ins.rows.len(), 3);
    assert!(matches!(
        ins.rows[0][0],
        Expr::Literal(Literal::Integer(1), _)
    ));
    assert!(matches!(
        ins.rows[1][0],
        Expr::Literal(Literal::Integer(2), _)
    ));
    assert!(matches!(
        ins.rows[2][0],
        Expr::Literal(Literal::Integer(3), _)
    ));
}

#[test]
fn dml_insert_missing_into_error() {
    let result = parse_prepared_statement("INSERT t VALUES (1)");
    assert!(result.is_err());
}

#[test]
fn dml_insert_column_list() {
    let stmt = parse_prepared_statement("INSERT INTO t (b, a) VALUES (1, 2)").expect("parse");
    let Statement::Insert(ins) = stmt else {
        panic!("expected INSERT");
    };
    assert_eq!(ins.columns.len(), 2);
    assert!(ins.query.is_none());
    assert_eq!(ins.columns[0].parts, vec!["b".to_owned()]);
    assert_eq!(ins.columns[1].parts, vec!["a".to_owned()]);
    assert_eq!(ins.rows.len(), 1);
    assert_eq!(ins.rows[0].len(), 2);
}

#[test]
fn dml_insert_default_values() {
    let stmt = parse_prepared_statement("INSERT INTO t DEFAULT VALUES").expect("parse");
    let Statement::Insert(ins) = stmt else {
        panic!("expected INSERT");
    };
    assert!(ins.columns.is_empty());
    assert_eq!(ins.rows, vec![vec![]]);
    assert!(ins.query.is_none());
}

#[test]
fn dml_insert_select() {
    let stmt =
        parse_prepared_statement("INSERT INTO t SELECT a, b FROM src WHERE a = 1").expect("parse");
    let Statement::Insert(ins) = stmt else {
        panic!("expected INSERT");
    };
    assert!(ins.rows.is_empty());
    let query = ins.query.expect("query");
    assert_eq!(query.items.len(), 2);
    assert!(query.selection.is_some());
}

#[test]
fn dml_insert_missing_values_error() {
    let result = parse_prepared_statement("INSERT INTO t (1)");
    assert!(result.is_err());
}

#[test]
fn dml_insert_empty_row_error() {
    // Empty parens after VALUES => parser tries to parse_expr and fails
    let result = parse_prepared_statement("INSERT INTO t VALUES ()");
    assert!(result.is_err());
}

#[test]
fn dml_delete_no_where() {
    let stmt = parse_prepared_statement("DELETE FROM t").expect("parse");
    let Statement::Delete(del) = stmt else {
        panic!("expected DELETE");
    };
    assert_eq!(del.table.parts, vec!["t".to_owned()]);
    assert!(del.selection.is_none());
    assert!(del.where_span.is_none());
}

#[test]
fn dml_delete_with_where() {
    let stmt = parse_prepared_statement("DELETE FROM t WHERE id = 1").expect("parse");
    let Statement::Delete(del) = stmt else {
        panic!("expected DELETE");
    };
    assert!(del.selection.is_some());
    assert!(del.where_span.is_some());
}

#[test]
fn dml_delete_with_target_alias() {
    let stmt = parse_prepared_statement("DELETE FROM t AS d USING s src WHERE d.id = src.id")
        .expect("parse");
    let Statement::Delete(del) = stmt else {
        panic!("expected DELETE");
    };
    assert_eq!(del.table.parts, vec!["t".to_owned()]);
    assert_eq!(del.table_alias.as_deref(), Some("d"));
    assert_eq!(del.using_tables.len(), 1);
    assert_eq!(del.using_tables[0].1.as_deref(), Some("src"));
}

#[test]
fn dml_delete_missing_from_error() {
    let result = parse_prepared_statement("DELETE t WHERE id = 1");
    assert!(result.is_err());
}

#[test]
fn dml_update_single_assignment_no_where() {
    let stmt = parse_prepared_statement("UPDATE t SET x = 1").expect("parse");
    let Statement::Update(upd) = stmt else {
        panic!("expected UPDATE");
    };
    assert_eq!(upd.table.parts, vec!["t".to_owned()]);
    assert_eq!(upd.assignments.len(), 1);
    assert_eq!(upd.assignments[0].column, "x");
    assert!(upd.selection.is_none());
}

#[test]
fn dml_update_multi_assignment() {
    let stmt = parse_prepared_statement("UPDATE t SET x = 1, y = 'a'").expect("parse");
    let Statement::Update(upd) = stmt else {
        panic!("expected UPDATE");
    };
    assert_eq!(upd.assignments.len(), 2);
    assert_eq!(upd.assignments[0].column, "x");
    assert_eq!(upd.assignments[1].column, "y");
}

#[test]
fn dml_update_with_where() {
    let stmt = parse_prepared_statement("UPDATE t SET x = 1 WHERE id = 2").expect("parse");
    let Statement::Update(upd) = stmt else {
        panic!("expected UPDATE");
    };
    assert_eq!(upd.assignments.len(), 1);
    assert!(upd.selection.is_some());
    assert!(upd.where_span.is_some());
}

#[test]
fn dml_update_with_target_alias() {
    let stmt = parse_prepared_statement("UPDATE t AS u SET x = 1 FROM s src WHERE u.id = src.id")
        .expect("parse");
    let Statement::Update(upd) = stmt else {
        panic!("expected UPDATE");
    };
    assert_eq!(upd.table.parts, vec!["t".to_owned()]);
    assert_eq!(upd.table_alias.as_deref(), Some("u"));
    assert_eq!(upd.from_tables.len(), 1);
    assert_eq!(upd.from_tables[0].1.as_deref(), Some("src"));
}

#[test]
fn dml_update_missing_set_error() {
    let result = parse_prepared_statement("UPDATE t x = 1");
    assert!(result.is_err());
}

// ═══════════════════════════════════════════════════════════════
//  INSERT ... ON CONFLICT (UPSERT) PARSING TESTS
// ═══════════════════════════════════════════════════════════════

#[test]
fn dml_insert_on_conflict_do_nothing() {
    let stmt = parse_prepared_statement("INSERT INTO t VALUES (1, 'a') ON CONFLICT DO NOTHING")
        .expect("parse");
    let Statement::Insert(ins) = stmt else {
        panic!("expected INSERT");
    };
    assert_eq!(ins.rows.len(), 1);
    let oc = ins.on_conflict.expect("on_conflict");
    assert!(oc.columns.is_empty());
    assert!(matches!(oc.action, ast::OnConflictAction::DoNothing));
}

#[test]
fn dml_insert_on_conflict_single_column_do_update() {
    let stmt = parse_prepared_statement(
        "INSERT INTO t VALUES (1, 'a') ON CONFLICT (id) DO UPDATE SET name = 'b'",
    )
    .expect("parse");
    let Statement::Insert(ins) = stmt else {
        panic!("expected INSERT");
    };
    assert_eq!(ins.rows.len(), 1);
    let oc = ins.on_conflict.expect("on_conflict");
    assert_eq!(oc.columns, vec!["id".to_owned()]);
    match &oc.action {
        ast::OnConflictAction::DoUpdate { assignments, .. } => {
            assert_eq!(assignments.len(), 1);
            assert_eq!(assignments[0].column, "name");
            assert!(matches!(
                assignments[0].expr,
                Expr::Literal(Literal::String(_), _)
            ));
        }
        _ => panic!("expected DoUpdate"),
    }
}

#[test]
fn dml_insert_on_conflict_multi_assignment() {
    let stmt = parse_prepared_statement(
        "INSERT INTO t VALUES (1, 'a') ON CONFLICT (id) DO UPDATE SET name = 'b', age = 30",
    )
    .expect("parse");
    let Statement::Insert(ins) = stmt else {
        panic!("expected INSERT");
    };
    let oc = ins.on_conflict.expect("on_conflict");
    assert_eq!(oc.columns, vec!["id".to_owned()]);
    match &oc.action {
        ast::OnConflictAction::DoUpdate { assignments, .. } => {
            assert_eq!(assignments.len(), 2);
            assert_eq!(assignments[0].column, "name");
            assert_eq!(assignments[1].column, "age");
            assert!(matches!(
                assignments[1].expr,
                Expr::Literal(Literal::Integer(30), _)
            ));
        }
        _ => panic!("expected DoUpdate"),
    }
}

#[test]
fn dml_insert_no_on_conflict_still_works() {
    let stmt = parse_prepared_statement("INSERT INTO t VALUES (1, 'a')").expect("parse");
    let Statement::Insert(ins) = stmt else {
        panic!("expected INSERT");
    };
    assert!(ins.on_conflict.is_none());
    assert_eq!(ins.rows.len(), 1);
    assert_eq!(ins.rows[0].len(), 2);
}

#[test]
fn dml_insert_on_conflict_do_nothing_with_target_column() {
    let stmt = parse_prepared_statement("INSERT INTO t VALUES (1) ON CONFLICT (id) DO NOTHING")
        .expect("parse");
    let Statement::Insert(ins) = stmt else {
        panic!("expected INSERT");
    };
    let oc = ins.on_conflict.expect("on_conflict");
    assert_eq!(oc.columns, vec!["id".to_owned()]);
    assert!(matches!(oc.action, ast::OnConflictAction::DoNothing));
}

#[test]
fn dml_insert_on_conflict_multi_target_columns() {
    let stmt = parse_prepared_statement("INSERT INTO t VALUES (1) ON CONFLICT (a, b) DO NOTHING")
        .expect("parse");
    let Statement::Insert(ins) = stmt else {
        panic!("expected INSERT");
    };
    let oc = ins.on_conflict.expect("on_conflict");
    assert_eq!(oc.columns, vec!["a".to_owned(), "b".to_owned()]);
    assert!(matches!(oc.action, ast::OnConflictAction::DoNothing));
}

#[test]
fn dml_insert_on_conflict_missing_do_error() {
    let result = parse_prepared_statement("INSERT INTO t VALUES (1) ON CONFLICT (id)");
    assert!(result.is_err());
}

#[test]
fn dml_insert_on_conflict_missing_action_error() {
    let result = parse_prepared_statement("INSERT INTO t VALUES (1) ON CONFLICT (id) DO");
    assert!(result.is_err());
}

#[test]
fn dml_insert_on_conflict_do_update_missing_set_error() {
    let result =
        parse_prepared_statement("INSERT INTO t VALUES (1) ON CONFLICT (id) DO UPDATE name = 'b'");
    assert!(result.is_err());
}
