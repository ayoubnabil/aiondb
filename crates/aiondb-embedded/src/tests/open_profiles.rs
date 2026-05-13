use super::*;

fn temp_data_dir(name: &str) -> std::path::PathBuf {
    unique_temp_path("data-dir", name)
}

fn relative_backup_path(name: &str) -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    std::path::PathBuf::from(format!(
        "target/aiondb-embedded-backups/{name}-{}-{nanos}.sql",
        std::process::id()
    ))
}

fn resolved_backup_path(path: &std::path::Path) -> std::path::PathBuf {
    std::env::current_dir()
        .expect("current dir")
        .join("backups")
        .join(path)
}

#[test]
fn in_memory_profile_supports_anonymous_connect() {
    let database = Database::in_memory().unwrap();
    let connection = database.connect_anonymous("default", "alice").unwrap();

    let results = connection.execute("SELECT 1").unwrap();
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows[0].values[0], Value::Int(1));
        }
        other => panic!("expected query result, got {other:?}"),
    }
}

#[test]
fn durable_profile_with_config_round_trips_data() {
    let data_dir = temp_data_dir("durable-profile");
    let database = Database::open_with_config(&data_dir, RuntimeConfig::default()).unwrap();
    let connection = database.connect_anonymous("default", "alice").unwrap();

    connection
        .execute(
            "CREATE TABLE items (id INT NOT NULL, name TEXT); INSERT INTO items VALUES (1, 'one')",
        )
        .unwrap();

    let results = connection
        .execute("SELECT name FROM items ORDER BY id")
        .unwrap();
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows[0].values[0], Value::Text("one".to_owned()));
        }
        other => panic!("expected query result, got {other:?}"),
    }

    let _ = std::fs::remove_dir_all(&data_dir);
}
#[test]
fn durable_profile_backup_writes_manifest_with_checksum() {
    let source_dir = temp_data_dir("durable-backup-source");
    let backup_path = relative_backup_path("durable-roundtrip");
    let resolved_backup_path = resolved_backup_path(&backup_path);
    let backup_parent = resolved_backup_path.parent().expect("backup parent");
    std::fs::create_dir_all(backup_parent).expect("create backup dir");
    let backup_path_str = backup_path.to_str().unwrap();

    {
        let database = Database::open(&source_dir).unwrap();
        let connection = database.connect_anonymous("default", "alice").unwrap();
        connection
            .execute(
                "CREATE TABLE docs (id INT NOT NULL, title TEXT); \
                 INSERT INTO docs VALUES (1, 'alpha'), (2, 'beta')",
            )
            .unwrap();
        connection
            .execute(&format!("BACKUP DATABASE TO '{backup_path_str}'"))
            .unwrap();
    }

    let backup_contents = std::fs::read_to_string(&resolved_backup_path).expect("read backup");
    assert!(
        backup_contents.starts_with("-- AionDB Backup\n"),
        "backup must start with manifest banner"
    );
    assert!(
        backup_contents.contains("-- backup-format-version: 2\n"),
        "backup must include format version"
    );
    assert!(
        backup_contents.contains("-- backup-payload-sha256: "),
        "backup must include payload checksum"
    );

    let _ = std::fs::remove_file(&resolved_backup_path);
    let _ = std::fs::remove_dir_all(&source_dir);
}

#[test]
fn explicit_open_profile_matches_constructor_behavior() {
    let database = Database::open_with_profile(OpenProfile::InMemory).unwrap();
    let connection = database.connect_anonymous("default", "alice").unwrap();

    connection
        .execute("CREATE TABLE profile_t (id INT NOT NULL); INSERT INTO profile_t VALUES (7)")
        .unwrap();

    let results = connection
        .execute("SELECT COUNT(*) FROM profile_t")
        .unwrap();
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows[0].values[0], Value::BigInt(1));
        }
        other => panic!("expected query result, got {other:?}"),
    }
}
