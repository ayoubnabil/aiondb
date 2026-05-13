use super::*;
use sha2::Digest;

fn backup_test_root() -> std::path::PathBuf {
    std::env::current_dir()
        .expect("current dir")
        .join("backups")
}

fn resolved_backup_path(path: &std::path::Path) -> std::path::PathBuf {
    let root = backup_test_root();
    std::fs::create_dir_all(&root).expect("create backup dir");
    root.join(path)
}

fn checksum_hex(payload: &str) -> String {
    let digest = sha2::Sha256::digest(payload.as_bytes());
    aiondb_core::hex_encode(digest.as_ref())
}

fn checksum_protected_backup_document(payload: &str) -> String {
    let checksum = checksum_hex(payload);
    format!(
        "-- AionDB Backup\n-- backup-format-version: 2\n-- engine-version: test\n-- backup-payload-sha256: {checksum}\n\n{payload}"
    )
}

fn write_checksum_protected_restore_file(path: &std::path::Path, payload: &str) {
    std::fs::write(path, checksum_protected_backup_document(payload)).expect("write restore file");
}

#[test]
fn backup_and_restore_round_trip() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE users (id INT NOT NULL, name TEXT, active BOOLEAN, PRIMARY KEY (id)); \
             INSERT INTO users VALUES (1, 'Alice', TRUE); \
             INSERT INTO users VALUES (2, 'Bob', FALSE); \
             INSERT INTO users VALUES (3, 'Charlie', TRUE)",
        )
        .expect("setup");

    let path = unique_relative_backup_path("backup-test-roundtrip");
    let path_str = path.to_str().unwrap();

    let results = engine
        .execute_sql(&session, &format!("BACKUP DATABASE TO '{path_str}'"))
        .expect("backup");
    assert!(matches!(&results[0], StatementResult::Command { tag, .. } if tag == "BACKUP"));

    // Verify the file was created and contains SQL
    let resolved_path = resolved_backup_path(&path);
    let content = std::fs::read_to_string(&resolved_path).expect("read backup file");
    assert!(content.contains("-- AionDB Backup"));
    assert!(content.contains("-- backup-format-version: 2"));
    assert!(content.contains("-- backup-payload-sha256: "));
    assert!(content.contains("CREATE TABLE"));
    assert!(content.contains("INSERT INTO"));
    assert!(content.contains("Alice"));

    // Restore into a fresh engine
    let engine2 = EngineBuilder::for_testing().build().unwrap();
    let (session2, _) = engine2.startup(startup_params()).expect("startup2");

    let results = engine2
        .execute_sql(&session2, &format!("RESTORE DATABASE FROM '{path_str}'"))
        .expect("restore");
    assert!(matches!(&results[0], StatementResult::Command { tag, .. } if tag == "RESTORE"));

    // Verify data is intact
    let results = engine2
        .execute_sql(&session2, "SELECT id, name, active FROM users ORDER BY id")
        .expect("select after restore");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 3);
            assert_eq!(rows[0].values[0], Value::Int(1));
            assert_eq!(rows[0].values[1], Value::Text("Alice".to_owned()));
            assert_eq!(rows[0].values[2], Value::Boolean(true));
            assert_eq!(rows[1].values[0], Value::Int(2));
            assert_eq!(rows[1].values[1], Value::Text("Bob".to_owned()));
            assert_eq!(rows[2].values[0], Value::Int(3));
        }
        other => panic!("expected Query, got {other:?}"),
    }

    let _ = std::fs::remove_file(&resolved_path);
}

#[test]
fn backup_database_respects_session_statement_timeout() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE timeout_backup_t (id INT NOT NULL, name TEXT); \
             INSERT INTO timeout_backup_t VALUES (1, 'Alice'), (2, 'Bob')",
        )
        .expect("setup");

    engine
        .with_session_mut(&session, |record| {
            record.info.limits.statement_timeout = std::time::Duration::ZERO;
            Ok(())
        })
        .expect("set session timeout");

    let path = unique_relative_backup_path("backup-timeout");
    let path_str = path.to_str().expect("backup path utf-8");

    let error = engine
        .execute_sql(&session, &format!("BACKUP DATABASE TO '{path_str}'"))
        .expect_err("backup should honor session timeout");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::QueryCanceled);

    let _ = std::fs::remove_file(resolved_backup_path(&path));
}

#[test]
fn backup_multiple_tables() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE t1 (id INT NOT NULL, val TEXT); \
             CREATE TABLE t2 (id INT NOT NULL, num INT); \
             INSERT INTO t1 VALUES (1, 'hello'); \
             INSERT INTO t2 VALUES (10, 42)",
        )
        .expect("setup");

    let path = std::path::PathBuf::from("aiondb_backup_test_multi.sql");
    let path_str = path.to_str().unwrap();

    engine
        .execute_sql(&session, &format!("BACKUP DATABASE TO '{path_str}'"))
        .expect("backup");

    let engine2 = EngineBuilder::for_testing().build().unwrap();
    let (session2, _) = engine2.startup(startup_params()).expect("startup2");

    engine2
        .execute_sql(&session2, &format!("RESTORE DATABASE FROM '{path_str}'"))
        .expect("restore");

    let r1 = engine2
        .execute_sql(&session2, "SELECT val FROM t1")
        .expect("select t1");
    match &r1[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::Text("hello".to_owned()));
        }
        other => panic!("expected Query, got {other:?}"),
    }

    let r2 = engine2
        .execute_sql(&session2, "SELECT num FROM t2")
        .expect("select t2");
    match &r2[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::Int(42));
        }
        other => panic!("expected Query, got {other:?}"),
    }

    let _ = std::fs::remove_file(resolved_backup_path(&path));
}

#[test]
fn backup_and_restore_round_trip_with_non_public_schema() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE SCHEMA analytics; \
             CREATE TABLE analytics.events (id INT NOT NULL, name TEXT, PRIMARY KEY (id)); \
             INSERT INTO analytics.events VALUES (1, 'launch'), (2, 'retention'); \
             CREATE VIEW analytics.event_names AS SELECT name FROM analytics.events ORDER BY name",
        )
        .expect("setup");

    let path = unique_relative_backup_path("backup-test-non-public-schema");
    let path_str = path.to_str().unwrap();

    engine
        .execute_sql(&session, &format!("BACKUP DATABASE TO '{path_str}'"))
        .expect("backup");

    let resolved_path = resolved_backup_path(&path);
    let content = std::fs::read_to_string(&resolved_path).expect("read backup");
    assert!(content.contains("CREATE SCHEMA \"analytics\";"));
    assert!(content.contains("CREATE TABLE \"analytics\".\"events\""));
    assert!(content.contains("INSERT INTO \"analytics\".\"events\""));
    assert!(content.contains("CREATE VIEW \"analytics\".\"event_names\" AS"));

    let engine2 = EngineBuilder::for_testing().build().unwrap();
    let (session2, _) = engine2.startup(startup_params()).expect("startup2");

    engine2
        .execute_sql(&session2, &format!("RESTORE DATABASE FROM '{path_str}'"))
        .expect("restore");

    let results = engine2
        .execute_sql(
            &session2,
            "SELECT id, name FROM analytics.events ORDER BY id",
        )
        .expect("select restored table");
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 2);
            assert_eq!(rows[0].values[0], Value::Int(1));
            assert_eq!(rows[0].values[1], Value::Text("launch".to_owned()));
            assert_eq!(rows[1].values[0], Value::Int(2));
            assert_eq!(rows[1].values[1], Value::Text("retention".to_owned()));
        }
        other => panic!("expected Query, got {other:?}"),
    }

    let view_results = engine2
        .execute_sql(&session2, "SELECT name FROM analytics.event_names")
        .expect("select restored view");
    match &view_results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 2);
            assert_eq!(rows[0].values[0], Value::Text("launch".to_owned()));
            assert_eq!(rows[1].values[0], Value::Text("retention".to_owned()));
        }
        other => panic!("expected Query, got {other:?}"),
    }

    let _ = std::fs::remove_file(resolved_path);
}

#[test]
fn backup_and_restore_preserves_view_creation_search_path_for_updatable_view() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE SCHEMA analytics; \
             CREATE TABLE analytics.base_tbl (a INT PRIMARY KEY, b TEXT); \
             INSERT INTO analytics.base_tbl VALUES (1, 'Row 1'), (2, 'Row 2'); \
             SET search_path TO public, analytics; \
             CREATE VIEW rw_view_path AS SELECT * FROM base_tbl WHERE a > 0",
        )
        .expect("setup search_path-created updatable view");

    let path = std::path::PathBuf::from("aiondb_backup_test_view_search_path.sql");
    let path_str = path.to_str().unwrap();
    let resolved_path = resolved_backup_path(&path);
    let _ = std::fs::remove_file(&resolved_path);

    engine
        .execute_sql(&session, &format!("BACKUP DATABASE TO '{path_str}'"))
        .expect("backup");

    let content = std::fs::read_to_string(&resolved_path).expect("read backup");
    assert!(content.contains("SET search_path TO \"public\", \"analytics\";"));
    assert!(content.contains("CREATE VIEW \"public\".\"rw_view_path\" AS "));
    assert!(content.contains("RESET search_path;"));

    let engine2 = EngineBuilder::for_testing().build().unwrap();
    let (session2, _) = engine2.startup(startup_params()).expect("startup2");

    engine2
        .execute_sql(&session2, &format!("RESTORE DATABASE FROM '{path_str}'"))
        .expect("restore");

    engine2
        .execute_sql(&session2, "INSERT INTO rw_view_path VALUES (3, 'Row 3')")
        .expect("insert through restored view should succeed");

    let results = engine2
        .execute_sql(&session2, "SELECT * FROM analytics.base_tbl ORDER BY a")
        .expect("select restored base table");
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 3);
            assert_eq!(rows[2].values[0], Value::Int(3));
            assert_eq!(rows[2].values[1], Value::Text("Row 3".to_owned()));
        }
        other => panic!("expected Query, got {other:?}"),
    }

    let _ = std::fs::remove_file(resolved_path);
}

#[test]
fn backup_and_restore_round_trip_with_cross_schema_foreign_key() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE public.parents (id INT NOT NULL, PRIMARY KEY (id)); \
             CREATE SCHEMA analytics; \
             CREATE TABLE analytics.children (id INT NOT NULL, parent_id INT, PRIMARY KEY (id), \
                 FOREIGN KEY (parent_id) REFERENCES public.parents (id)); \
             INSERT INTO public.parents VALUES (1); \
             INSERT INTO analytics.children VALUES (10, 1)",
        )
        .expect("setup cross-schema foreign key");

    let path = unique_relative_backup_path("backup-test-cross-schema-fk");
    let path_str = path.to_str().unwrap();

    engine
        .execute_sql(&session, &format!("BACKUP DATABASE TO '{path_str}'"))
        .expect("backup");

    let resolved_path = resolved_backup_path(&path);
    let content = std::fs::read_to_string(&resolved_path).expect("read backup");
    assert!(content.contains("REFERENCES \"public\".\"parents\" (\"id\")"));

    let engine2 = EngineBuilder::for_testing().build().unwrap();
    let (session2, _) = engine2.startup(startup_params()).expect("startup2");

    engine2
        .execute_sql(&session2, &format!("RESTORE DATABASE FROM '{path_str}'"))
        .expect("restore");

    let rows = query_rows(
        &engine2,
        &session2,
        "SELECT id, parent_id FROM analytics.children",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::Int(10));
    assert_eq!(rows[0].values[1], Value::Int(1));

    let fk_error = engine2
        .execute_sql(&session2, "INSERT INTO analytics.children VALUES (11, 999)")
        .expect_err("restored foreign key should still be enforced");
    assert!(
        format!("{fk_error}").contains("foreign key"),
        "unexpected error: {fk_error}"
    );

    let _ = std::fs::remove_file(resolved_path);
}

#[test]
fn backup_empty_table() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE empty_tbl (id INT, name TEXT)")
        .expect("create");

    let path = std::path::PathBuf::from("aiondb_backup_test_empty.sql");
    let path_str = path.to_str().unwrap();

    engine
        .execute_sql(&session, &format!("BACKUP DATABASE TO '{path_str}'"))
        .expect("backup");

    let engine2 = EngineBuilder::for_testing().build().unwrap();
    let (session2, _) = engine2.startup(startup_params()).expect("startup2");

    engine2
        .execute_sql(&session2, &format!("RESTORE DATABASE FROM '{path_str}'"))
        .expect("restore");

    let results = engine2
        .execute_sql(&session2, "SELECT count(*) FROM empty_tbl")
        .expect("select");
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows[0].values[0], Value::BigInt(0));
        }
        other => panic!("expected Query, got {other:?}"),
    }

    let _ = std::fs::remove_file(resolved_backup_path(&path));
}

#[test]
fn backup_with_nulls() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE nulls_tbl (id INT NOT NULL, val TEXT); \
             INSERT INTO nulls_tbl VALUES (1, NULL); \
             INSERT INTO nulls_tbl VALUES (2, 'present')",
        )
        .expect("setup");

    let path = std::path::PathBuf::from("aiondb_backup_test_nulls.sql");
    let path_str = path.to_str().unwrap();

    engine
        .execute_sql(&session, &format!("BACKUP DATABASE TO '{path_str}'"))
        .expect("backup");

    let engine2 = EngineBuilder::for_testing().build().unwrap();
    let (session2, _) = engine2.startup(startup_params()).expect("startup2");

    engine2
        .execute_sql(&session2, &format!("RESTORE DATABASE FROM '{path_str}'"))
        .expect("restore");

    let results = engine2
        .execute_sql(&session2, "SELECT id, val FROM nulls_tbl ORDER BY id")
        .expect("select");
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows[0].values[0], Value::Int(1));
            assert_eq!(rows[0].values[1], Value::Null);
            assert_eq!(rows[1].values[0], Value::Int(2));
            assert_eq!(rows[1].values[1], Value::Text("present".to_owned()));
        }
        other => panic!("expected Query, got {other:?}"),
    }

    let _ = std::fs::remove_file(resolved_backup_path(&path));
}

#[test]
fn backup_with_special_characters() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE special_tbl (id INT NOT NULL, val TEXT); \
             INSERT INTO special_tbl VALUES (1, 'it''s a test')",
        )
        .expect("setup");

    let path = std::path::PathBuf::from("aiondb_backup_test_special.sql");
    let path_str = path.to_str().unwrap();

    engine
        .execute_sql(&session, &format!("BACKUP DATABASE TO '{path_str}'"))
        .expect("backup");

    let engine2 = EngineBuilder::for_testing().build().unwrap();
    let (session2, _) = engine2.startup(startup_params()).expect("startup2");

    engine2
        .execute_sql(&session2, &format!("RESTORE DATABASE FROM '{path_str}'"))
        .expect("restore");

    let results = engine2
        .execute_sql(&session2, "SELECT val FROM special_tbl")
        .expect("select");
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows[0].values[0], Value::Text("it's a test".to_owned()));
        }
        other => panic!("expected Query, got {other:?}"),
    }

    let _ = std::fs::remove_file(resolved_backup_path(&path));
}

#[test]
fn backup_path_traversal_rejected() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let err = engine
        .execute_sql(&session, "BACKUP DATABASE TO '../../../etc/evil.sql'")
        .expect_err("path traversal should be rejected");
    let msg = format!("{err:?}");
    assert!(
        msg.contains(".."),
        "error should mention path traversal: {msg}"
    );
}

#[test]
fn backup_refuses_to_overwrite_existing_file() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE overwrite_guard_tbl (id INT NOT NULL)",
        )
        .expect("create");

    let path = std::path::PathBuf::from("aiondb_backup_existing.sql");
    let resolved_path = resolved_backup_path(&path);
    std::fs::create_dir_all(backup_test_root()).expect("create backup dir");
    std::fs::write(&resolved_path, "existing").expect("seed existing backup");

    let err = engine
        .execute_sql(&session, "BACKUP DATABASE TO 'aiondb_backup_existing.sql'")
        .expect_err("backup must not overwrite an existing file");
    assert!(
        format!("{err}").contains("refusing to overwrite existing backup file"),
        "unexpected error: {err}"
    );

    let content = std::fs::read_to_string(&resolved_path).expect("read seeded backup");
    assert_eq!(content, "existing");

    let _ = std::fs::remove_file(resolved_path);
}

#[test]
fn restore_nonexistent_file_errors() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let err = engine
        .execute_sql(
            &session,
            "RESTORE DATABASE FROM 'nonexistent_aiondb_backup_xyz.sql'",
        )
        .expect_err("should fail for nonexistent file");
    let msg = format!("{err:?}");
    assert!(
        msg.contains("failed to read"),
        "error should mention read failure: {msg}"
    );
}

#[test]
fn restore_path_traversal_rejected() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let err = engine
        .execute_sql(&session, "RESTORE DATABASE FROM '../../etc/passwd'")
        .expect_err("path traversal should be rejected");
    let msg = format!("{err:?}");
    assert!(
        msg.contains(".."),
        "error should mention path traversal: {msg}"
    );
}

#[test]
fn restore_legacy_backup_without_version_header_is_rejected() {
    let path = std::path::PathBuf::from("aiondb_backup_test_legacy.sql");
    let resolved_path = resolved_backup_path(&path);
    std::fs::write(
        &resolved_path,
        "-- AionDB Backup\nCREATE TABLE legacy_users (id INT NOT NULL, name TEXT);\nINSERT INTO legacy_users (id, name) VALUES (1, 'Alice');\n",
    )
    .expect("write legacy backup");

    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let path_str = path.to_str().unwrap();

    let err = engine
        .execute_sql(&session, &format!("RESTORE DATABASE FROM '{path_str}'"))
        .expect_err("legacy restore should be rejected by default");
    assert!(
        err.to_string().contains("legacy") || err.to_string().contains("checksum-protected"),
        "unexpected error: {err}"
    );

    let _ = std::fs::remove_file(resolved_path);
}

#[test]
fn restore_version_1_backup_without_checksum_is_rejected() {
    let path = std::path::PathBuf::from("aiondb_backup_test_v1.sql");
    let resolved_path = resolved_backup_path(&path);
    std::fs::write(
        &resolved_path,
        "-- AionDB Backup\n-- backup-format-version: 1\n-- engine-version: 0.1.0\n\nCREATE TABLE compat_users (id INT NOT NULL, name TEXT);\nINSERT INTO compat_users (id, name) VALUES (1, 'Alice');\n",
    )
    .expect("write v1 backup");

    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let path_str = path.to_str().unwrap();

    let err = engine
        .execute_sql(&session, &format!("RESTORE DATABASE FROM '{path_str}'"))
        .expect_err("v1 restore should be rejected by default");
    assert!(
        err.to_string().contains("legacy") || err.to_string().contains("checksum-protected"),
        "unexpected error: {err}"
    );

    let _ = std::fs::remove_file(resolved_path);
}

#[test]
fn restore_rejects_corrupted_checksum_protected_backup() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE users (id INT NOT NULL, name TEXT); \
             INSERT INTO users VALUES (1, 'Alice')",
        )
        .expect("setup");

    let path = std::path::PathBuf::from("aiondb_backup_test_corrupted.sql");
    let path_str = path.to_str().unwrap();

    engine
        .execute_sql(&session, &format!("BACKUP DATABASE TO '{path_str}'"))
        .expect("backup");

    let resolved_path = resolved_backup_path(&path);
    let content = std::fs::read_to_string(&resolved_path).expect("read backup");
    let corrupted = content.replacen("Alice", "Mallory", 1);
    std::fs::write(&resolved_path, corrupted).expect("corrupt backup");

    let restore_engine = EngineBuilder::for_testing().build().unwrap();
    let (restore_session, _) = restore_engine.startup(startup_params()).expect("startup2");

    let err = restore_engine
        .execute_sql(
            &restore_session,
            &format!("RESTORE DATABASE FROM '{path_str}'"),
        )
        .expect_err("corrupted backup should be rejected");
    assert!(
        format!("{err}").contains("checksum mismatch"),
        "unexpected error: {err}"
    );

    let _ = std::fs::remove_file(resolved_path);
}

#[test]
fn restore_rejects_unsupported_backup_format_version() {
    let path = std::path::PathBuf::from("aiondb_backup_test_future.sql");
    let resolved_path = resolved_backup_path(&path);
    std::fs::write(
        &resolved_path,
        "-- AionDB Backup\n-- backup-format-version: 99\nCREATE TABLE future_tbl (id INT);\n",
    )
    .expect("write future backup");

    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let path_str = path.to_str().unwrap();

    let err = engine
        .execute_sql(&session, &format!("RESTORE DATABASE FROM '{path_str}'"))
        .expect_err("future format should be rejected");
    assert!(
        format!("{err}").contains("unsupported backup format version"),
        "unexpected error: {err}"
    );

    let _ = std::fs::remove_file(resolved_path);
}

#[test]
fn restore_database_respects_session_statement_timeout() {
    let path = unique_relative_backup_path("restore-timeout");
    let resolved_path = resolved_backup_path(&path);
    write_checksum_protected_restore_file(
        &resolved_path,
        "CREATE TABLE restore_timeout_t (id INT NOT NULL, name TEXT);\n\
         INSERT INTO restore_timeout_t VALUES (1, 'Alice');\n",
    );

    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let path_str = path.to_str().expect("restore path utf-8");

    engine
        .with_session_mut(&session, |record| {
            record.info.limits.statement_timeout = std::time::Duration::ZERO;
            Ok(())
        })
        .expect("set session timeout");

    let error = engine
        .execute_sql(&session, &format!("RESTORE DATABASE FROM '{path_str}'"))
        .expect_err("restore should honor session timeout");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::QueryCanceled);

    let _ = std::fs::remove_file(resolved_path);
}

#[test]
fn restore_failure_rolls_back_entire_restore_when_not_in_transaction() {
    let path = std::path::PathBuf::from("aiondb_backup_test_atomic_restore.sql");
    let resolved_path = resolved_backup_path(&path);
    write_checksum_protected_restore_file(
        &resolved_path,
        "CREATE TABLE restore_atomic (id INT NOT NULL, PRIMARY KEY (id));\n\
         INSERT INTO restore_atomic VALUES (1);\n\
         INSERT INTO restore_atomic VALUES (1);\n",
    );

    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let path_str = path.to_str().unwrap();

    engine
        .execute_sql(&session, &format!("RESTORE DATABASE FROM '{path_str}'"))
        .expect_err("restore should fail");

    assert!(
        !engine
            .has_active_transaction(&session)
            .expect("transaction state"),
        "restore failure should not leave an implicit transaction open"
    );

    let err = engine
        .execute_sql(&session, "SELECT COUNT(*) FROM restore_atomic")
        .expect_err("failed restore must not leave the table behind");
    assert!(
        format!("{err:?}").contains("does not exist"),
        "unexpected error: {err:?}"
    );

    let _ = std::fs::remove_file(resolved_path);
}

#[test]
fn restore_failure_rolls_back_to_savepoint_inside_active_transaction() {
    let path = std::path::PathBuf::from("aiondb_backup_test_restore_savepoint.sql");
    let resolved_path = resolved_backup_path(&path);
    write_checksum_protected_restore_file(
        &resolved_path,
        "CREATE TABLE restore_do_not_keep (id INT NOT NULL, PRIMARY KEY (id));\n\
         INSERT INTO restore_do_not_keep VALUES (1);\n\
         INSERT INTO restore_do_not_keep VALUES (1);\n",
    );

    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let path_str = path.to_str().unwrap();

    engine
        .execute_sql(
            &session,
            "CREATE TABLE keep_me (id INT NOT NULL, PRIMARY KEY (id))",
        )
        .expect("create keep_me");
    engine
        .begin_transaction(&session, aiondb_tx::IsolationLevel::ReadCommitted)
        .expect("begin");
    engine
        .execute_sql(&session, "INSERT INTO keep_me VALUES (7)")
        .expect("insert before restore");

    engine
        .execute_sql(&session, &format!("RESTORE DATABASE FROM '{path_str}'"))
        .expect_err("restore should fail");

    assert!(
        engine
            .has_active_transaction(&session)
            .expect("transaction state"),
        "restore failure should preserve the caller transaction"
    );

    let keep_results = engine
        .execute_sql(&session, "SELECT id FROM keep_me")
        .expect("prior work should remain visible");
    match &keep_results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::Int(7));
        }
        other => panic!("expected Query, got {other:?}"),
    }

    engine
        .execute_sql(&session, "SAVEPOINT verify_restore_absent")
        .expect("create verification savepoint");
    let err = engine
        .execute_sql(&session, "SELECT COUNT(*) FROM restore_do_not_keep")
        .expect_err("failed restore must roll back to savepoint");
    assert!(
        format!("{err:?}").contains("does not exist"),
        "unexpected error: {err:?}"
    );
    engine
        .execute_sql(&session, "ROLLBACK TO SAVEPOINT verify_restore_absent")
        .expect("recover verification savepoint");

    engine
        .commit_transaction(&session)
        .expect("commit outer transaction");

    let keep_results = engine
        .execute_sql(&session, "SELECT id FROM keep_me")
        .expect("committed row should persist");
    match &keep_results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::Int(7));
        }
        other => panic!("expected Query, got {other:?}"),
    }

    let _ = std::fs::remove_file(resolved_path);
}

#[test]
fn restore_failure_does_not_collide_with_user_savepoint_name() {
    let path = std::path::PathBuf::from("aiondb_backup_test_restore_savepoint_collision.sql");
    let resolved_path = resolved_backup_path(&path);
    write_checksum_protected_restore_file(
        &resolved_path,
        "CREATE TABLE restore_do_not_keep (id INT NOT NULL, PRIMARY KEY (id));\n\
         INSERT INTO restore_do_not_keep VALUES (1);\n\
         INSERT INTO restore_do_not_keep VALUES (1);\n",
    );

    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let path_str = path.to_str().unwrap();

    engine
        .execute_sql(
            &session,
            "CREATE TABLE keep_me (id INT NOT NULL, PRIMARY KEY (id))",
        )
        .expect("create keep_me");
    engine.execute_sql(&session, "BEGIN").expect("begin");
    engine
        .execute_sql(&session, "INSERT INTO keep_me VALUES (1)")
        .expect("insert before savepoint");
    engine
        .execute_sql(&session, "SAVEPOINT __aiondb_restore__")
        .expect("create user savepoint");
    engine
        .execute_sql(&session, "INSERT INTO keep_me VALUES (2)")
        .expect("insert after savepoint");

    engine
        .execute_sql(&session, &format!("RESTORE DATABASE FROM '{path_str}'"))
        .expect_err("restore should fail");

    engine
        .execute_sql(&session, "ROLLBACK TO SAVEPOINT __aiondb_restore__")
        .expect("user savepoint should remain addressable");

    let keep_results = engine
        .execute_sql(&session, "SELECT id FROM keep_me ORDER BY id")
        .expect("prior work should remain visible after rollback");
    match &keep_results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::Int(1));
        }
        other => panic!("expected Query, got {other:?}"),
    }

    engine
        .execute_sql(&session, "RELEASE SAVEPOINT __aiondb_restore__")
        .expect("release user savepoint");
    engine.commit_transaction(&session).expect("commit");

    let keep_results = engine
        .execute_sql(&session, "SELECT id FROM keep_me ORDER BY id")
        .expect("committed row should persist");
    match &keep_results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::Int(1));
        }
        other => panic!("expected Query, got {other:?}"),
    }

    let err = engine
        .execute_sql(&session, "SELECT COUNT(*) FROM restore_do_not_keep")
        .expect_err("failed restore must not leak objects");
    assert!(
        format!("{err:?}").contains("does not exist"),
        "unexpected error: {err:?}"
    );

    let _ = std::fs::remove_file(resolved_path);
}
