use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use serde_json::Value as JsonValue;

use super::*;

static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

fn temp_audit_path(name: &str) -> PathBuf {
    let unique_id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir()
        .join("aiondb-auth-audit-tests")
        .join(format!("{name}-{unique_id}"))
        .with_extension("log");
    let _ = std::fs::remove_file(&path);
    path
}

fn cleanup_test_root(path: &Path) {
    if let Some(root) = path.parent().and_then(Path::parent) {
        let _ = std::fs::remove_dir_all(root);
    }
}

fn sample_event() -> AuthAuditEvent {
    AuthAuditEvent {
        stage: AuthAuditStage::Startup,
        outcome: AuthAuditOutcome::Failure,
        principal: "app_user".to_owned(),
        database: "default".to_owned(),
        transport_kind: "network",
        tls_enabled: true,
        peer_addr: Some("127.0.0.1:5432".to_owned()),
        auth_method: Some(AuthAuditMethod::ScramSha256),
        sqlstate: Some(SqlState::InvalidAuthorizationSpecification),
        message: Some("invalid user name or password".to_owned()),
    }
}

fn read_json_line(path: &Path) -> JsonValue {
    let content = std::fs::read_to_string(path).expect("read audit file");
    let line = content
        .lines()
        .find(|line| !line.trim().is_empty())
        .expect("audit line");
    serde_json::from_str(line).expect("valid audit json line")
}

#[test]
fn file_auth_audit_sink_appends_structured_line() {
    let path = temp_audit_path("append");
    let sink = FileAuthAuditSink::new(path.clone(), 8 * 1024 * 1024, 4);

    sink.record(sample_event());

    let record = read_json_line(&path);
    assert_eq!(record["record_type"], "auth_audit");
    assert_eq!(record["schema_version"], AUTH_AUDIT_SCHEMA_VERSION);
    assert_eq!(record["stage"], "startup");
    assert_eq!(record["outcome"], "failure");
    assert_eq!(record["principal"], "app_user");
    assert_eq!(record["database"], "default");
    assert_eq!(record["transport"], "network");
    assert_eq!(record["tls"], true);
    assert_eq!(record["auth_method"], "scram_sha_256");
    assert_eq!(record["sqlstate"], "28000");
    let _ = std::fs::remove_file(path);
}

#[test]
fn file_auth_audit_sink_creates_parent_directories() {
    let mut path = temp_audit_path("create-parents");
    path = path.with_extension("");
    path.push("nested");
    path.push("auth_audit.log");
    cleanup_test_root(&path);

    let sink = FileAuthAuditSink::new(path.clone(), 8 * 1024 * 1024, 4);
    sink.record(sample_event());

    assert!(path.exists(), "audit file should exist");
    let _ = std::fs::remove_file(&path);
    cleanup_test_root(&path);
}

#[test]
fn file_auth_audit_sink_rotates_and_limits_retention() {
    let path = temp_audit_path("rotation");
    let sink = FileAuthAuditSink::new(path.clone(), 1, 2);

    for principal in ["first", "second", "third"] {
        let mut event = sample_event();
        event.principal = principal.to_owned();
        sink.record(event);
    }

    let current = read_json_line(&path);
    let rotated_1 = read_json_line(&rotated_audit_path(&path, 1));
    let rotated_2 = read_json_line(&rotated_audit_path(&path, 2));

    assert_eq!(current["principal"], "third");
    assert_eq!(rotated_1["principal"], "second");
    assert_eq!(rotated_2["principal"], "first");
    assert!(!rotated_audit_path(&path, 3).exists());

    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(rotated_audit_path(&path, 1));
    let _ = std::fs::remove_file(rotated_audit_path(&path, 2));
}

#[test]
fn read_auth_audit_records_returns_newest_first_across_rotations() {
    let path = temp_audit_path("read-rotation");
    let sink = FileAuthAuditSink::new(path.clone(), 1, 2);

    for principal in ["first", "second", "third"] {
        let mut event = sample_event();
        event.principal = principal.to_owned();
        sink.record(event);
    }

    let all_records =
        read_auth_audit_records_from_path(&path, 2, &AuthAuditQuery::default()).expect("read");
    assert_eq!(all_records.len(), 3);
    assert_eq!(all_records[0].principal, "third");
    assert_eq!(all_records[0].source_path, path);
    assert_eq!(all_records[1].principal, "second");
    assert_eq!(all_records[1].source_path, rotated_audit_path(&path, 1));
    assert_eq!(all_records[2].principal, "first");
    assert_eq!(all_records[2].source_path, rotated_audit_path(&path, 2));

    let filtered = read_auth_audit_records_from_path(
        &path,
        2,
        &AuthAuditQuery {
            principal: Some("second".to_owned()),
            ..AuthAuditQuery::default()
        },
    )
    .expect("filtered read");
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].principal, "second");

    let current_only = read_auth_audit_records_from_path(
        &path,
        2,
        &AuthAuditQuery {
            include_rotated: false,
            ..AuthAuditQuery::default()
        },
    )
    .expect("current generation read");
    assert_eq!(current_only.len(), 1);
    assert_eq!(current_only[0].principal, "third");

    let limited = read_auth_audit_records_from_path(
        &path,
        2,
        &AuthAuditQuery {
            limit: 2,
            ..AuthAuditQuery::default()
        },
    )
    .expect("limited read");
    assert_eq!(limited.len(), 2);
    assert_eq!(limited[0].principal, "third");
    assert_eq!(limited[1].principal, "second");

    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(rotated_audit_path(&path, 1));
    let _ = std::fs::remove_file(rotated_audit_path(&path, 2));
}

#[test]
fn read_auth_audit_records_rejects_unknown_schema_version() {
    let path = temp_audit_path("schema-version");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create audit dir");
    }
    std::fs::write(
        &path,
        r#"{"record_type":"auth_audit","schema_version":999,"ts_ms":1,"stage":"startup","outcome":"failure","principal":"app_user","database":"default","transport":"network","tls":true,"peer_addr":null,"auth_method":"scram_sha_256","sqlstate":"28000","message":"bad login"}"#,
    )
    .expect("write audit file");

    let error = read_auth_audit_records_from_path(&path, 0, &AuthAuditQuery::default())
        .expect_err("unsupported schema version should fail");
    assert!(
        error
            .report()
            .message
            .contains("unsupported auth audit schema version 999"),
        "unexpected error: {}",
        error.report().message
    );

    let _ = std::fs::remove_file(path);
}

#[test]
fn file_auth_audit_sink_with_zero_rotated_files_discards_previous_generation() {
    let path = temp_audit_path("rotation-zero");
    let sink = FileAuthAuditSink::new(path.clone(), 1, 0);

    for principal in ["first", "second"] {
        let mut event = sample_event();
        event.principal = principal.to_owned();
        sink.record(event);
    }

    let current = read_json_line(&path);
    assert_eq!(current["principal"], "second");
    assert!(!rotated_audit_path(&path, 1).exists());
    let _ = std::fs::remove_file(path);
}

#[test]
fn ddl_audit_event_persisted_to_file() {
    let path = temp_audit_path("ddl-audit");
    let sink = FileAuthAuditSink::new(path.clone(), 8 * 1024 * 1024, 4);

    sink.record_ddl(DdlAuditEvent {
        role: "admin".to_owned(),
        operation: "CREATE".to_owned(),
        object_type: "TABLE".to_owned(),
        object_name: "users".to_owned(),
        success: true,
        error_message: None,
    });

    let record = read_json_line(&path);
    assert_eq!(record["record_type"], "ddl_audit");
    assert_eq!(record["role"], "admin");
    assert_eq!(record["operation"], "CREATE");
    assert_eq!(record["object_type"], "TABLE");
    assert_eq!(record["object_name"], "users");
    assert_eq!(record["outcome"], "success");
    let _ = std::fs::remove_file(path);
}
