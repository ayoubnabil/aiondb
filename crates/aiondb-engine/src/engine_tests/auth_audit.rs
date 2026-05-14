use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

use serde_json::Value as JsonValue;

use super::*;
use aiondb_security::{SecretString, TransportKind};

use crate::auth_audit::{
    AuthAuditEvent, AuthAuditMethod, AuthAuditOutcome, AuthAuditSink, AuthAuditStage,
};
use crate::AuthAuditQuery;

#[derive(Default)]
struct RecordingAuthAuditSink {
    events: Mutex<Vec<AuthAuditEvent>>,
}

impl RecordingAuthAuditSink {
    fn snapshot(&self) -> Vec<AuthAuditEvent> {
        self.events.lock().expect("audit events").clone()
    }

    fn clear(&self) {
        self.events.lock().expect("audit events").clear();
    }
}

impl AuthAuditSink for RecordingAuthAuditSink {
    fn record(&self, event: AuthAuditEvent) {
        self.events.lock().expect("audit events").push(event);
    }
}

fn build_engine_with_audit_sink(sink: Arc<RecordingAuthAuditSink>) -> Engine {
    EngineBuilder::for_testing()
        .with_auth_audit_sink(sink)
        .build()
        .unwrap()
}

fn temp_audit_path(name: &str) -> PathBuf {
    let path = crate::test_support::unique_temp_path("engine-tests-auth-audit", name)
        .with_extension("log");
    let _ = std::fs::remove_file(&path);
    path
}

fn temp_data_dir(name: &str) -> PathBuf {
    let path = crate::test_support::unique_temp_path("engine-tests-data", name);
    let _ = std::fs::remove_dir_all(&path);
    path
}

fn startup_with_password(user: &str, password: &str) -> StartupParams {
    StartupParams {
        database: "default".to_owned(),
        application_name: Some("test".to_owned()),
        options: BTreeMap::new(),
        credential: Credential::CleartextPassword {
            user: user.to_owned(),
            password: SecretString::new(password.to_owned()),
        },
        transport: TransportInfo::in_process(),
    }
}

fn read_audit_records(path: &std::path::Path) -> Vec<JsonValue> {
    let content = std::fs::read_to_string(path).expect("read audit log");
    content
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("valid audit json"))
        .collect()
}

#[test]
fn startup_authentication_audits_issued_scram_challenge() {
    let sink = Arc::new(RecordingAuthAuditSink::default());
    let engine = build_engine_with_audit_sink(sink.clone());
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE ROLE app_user LOGIN PASSWORD 'secret'")
        .expect("create role");
    sink.clear();

    let auth = engine
        .startup_authentication(
            "app_user",
            "default",
            &TransportInfo {
                kind: TransportKind::Network {
                    tls: true,
                    peer_addr: Some("127.0.0.1:5432".to_owned()),
                },
            },
        )
        .expect("startup auth");
    assert!(matches!(auth, StartupAuthentication::ScramSha256 { .. }));

    assert_eq!(
        sink.snapshot(),
        vec![AuthAuditEvent {
            stage: AuthAuditStage::StartupAuthentication,
            outcome: AuthAuditOutcome::ChallengeIssued,
            principal: "app_user".to_owned(),
            database: "default".to_owned(),
            transport_kind: "network",
            tls_enabled: true,
            peer_addr: Some("127.0.0.1:5432".to_owned()),
            auth_method: Some(AuthAuditMethod::ScramSha256),
            sqlstate: None,
            message: None,
        }]
    );
}

#[test]
fn startup_audits_invalid_password_failure() {
    let sink = Arc::new(RecordingAuthAuditSink::default());
    let engine = build_engine_with_audit_sink(sink.clone());
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE ROLE app_user LOGIN PASSWORD 'secret'")
        .expect("create role");
    sink.clear();

    let error = engine
        .startup(startup_with_password("app_user", "wrong"))
        .expect_err("wrong password should fail");
    assert_eq!(
        error.sqlstate(),
        aiondb_core::SqlState::InvalidAuthorizationSpecification
    );

    assert_eq!(
        sink.snapshot(),
        vec![AuthAuditEvent {
            stage: AuthAuditStage::Startup,
            outcome: AuthAuditOutcome::Failure,
            principal: "app_user".to_owned(),
            database: "default".to_owned(),
            transport_kind: "in_process",
            tls_enabled: false,
            peer_addr: None,
            auth_method: Some(AuthAuditMethod::CleartextPassword),
            sqlstate: Some(aiondb_core::SqlState::InvalidAuthorizationSpecification),
            message: Some("invalid user name or password".to_owned()),
        }]
    );
}

#[test]
fn startup_audits_successful_login() {
    let sink = Arc::new(RecordingAuthAuditSink::default());
    let engine = build_engine_with_audit_sink(sink.clone());
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE ROLE app_user LOGIN PASSWORD 'secret'")
        .expect("create role");
    sink.clear();

    let (role_session, _) = engine
        .startup(startup_with_password("app_user", "secret"))
        .expect("login should succeed");
    engine
        .terminate(role_session)
        .expect("terminate role session");

    assert_eq!(
        sink.snapshot(),
        vec![AuthAuditEvent {
            stage: AuthAuditStage::Startup,
            outcome: AuthAuditOutcome::Success,
            principal: "app_user".to_owned(),
            database: "default".to_owned(),
            transport_kind: "in_process",
            tls_enabled: false,
            peer_addr: None,
            auth_method: Some(AuthAuditMethod::CleartextPassword),
            sqlstate: None,
            message: None,
        }]
    );
}

#[test]
fn startup_writes_durable_auth_audit_file_when_enabled() {
    let audit_path = temp_audit_path("explicit-path");
    let mut runtime = aiondb_config::RuntimeConfig::default();
    runtime.security =
        aiondb_config::SecurityConfig::from_profile(aiondb_config::SecurityProfile::Development);
    runtime.security.durable_auth_audit = true;
    runtime.security.auth_audit_log_path = Some(audit_path.clone());
    let engine = EngineBuilder::for_testing()
        .with_runtime_config(runtime)
        .build()
        .unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE ROLE app_user LOGIN PASSWORD 'secret'")
        .expect("create role");

    let error = engine
        .startup(startup_with_password("app_user", "wrong"))
        .expect_err("wrong password should fail");
    assert_eq!(
        error.sqlstate(),
        aiondb_core::SqlState::InvalidAuthorizationSpecification
    );

    let records = read_audit_records(&audit_path);
    let record = records
        .iter()
        .rev()
        .find(|record| record["principal"] == "app_user")
        .expect("app_user audit record");
    assert_eq!(record["record_type"], "auth_audit");
    assert_eq!(record["schema_version"], 1);
    assert_eq!(record["principal"], "app_user");
    assert_eq!(record["outcome"], "failure");
    assert_eq!(record["sqlstate"], "28000");
    let _ = std::fs::remove_file(audit_path);
}

#[test]
fn startup_uses_default_durable_auth_audit_path_when_not_configured() {
    let data_dir = temp_data_dir("default-path");
    let expected_log = data_dir.join("security").join("auth_audit.log");
    let mut runtime = aiondb_config::RuntimeConfig::default();
    runtime.storage.data_dir = data_dir.clone();
    runtime.security.durable_auth_audit = true;
    let engine = EngineBuilder::for_testing()
        .with_runtime_config(runtime)
        .build()
        .unwrap();

    let (_session, _) = engine.startup(startup_params()).expect("startup");

    assert!(expected_log.exists(), "expected audit file at default path");
    let _ = std::fs::remove_file(&expected_log);
    let _ = std::fs::remove_dir_all(&data_dir);
}

#[test]
fn engine_reads_durable_auth_audit_records_with_filters() {
    let audit_path = temp_audit_path("engine-read");
    let mut runtime = aiondb_config::RuntimeConfig::default();
    runtime.security =
        aiondb_config::SecurityConfig::from_profile(aiondb_config::SecurityProfile::Development);
    runtime.security.durable_auth_audit = true;
    runtime.security.auth_audit_log_path = Some(audit_path.clone());
    let engine = EngineBuilder::for_testing()
        .with_runtime_config(runtime)
        .build()
        .unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE ROLE app_user LOGIN PASSWORD 'secret'")
        .expect("create role");

    let _ = engine.startup(startup_with_password("app_user", "wrong"));
    let _ = engine.startup(startup_with_password("app_user", "wrong"));

    let all_records = engine
        .read_auth_audit_records(&AuthAuditQuery::default())
        .expect("read audit records");
    assert!(
        all_records.len() >= 2,
        "expected at least two audit records, got {}",
        all_records.len()
    );
    assert_eq!(all_records[0].principal, "app_user");
    assert_eq!(all_records[0].outcome, "failure");
    assert_eq!(all_records[0].source_path, audit_path);
    assert_eq!(engine.durable_auth_audit_path(), Some(audit_path.clone()));

    let filtered = engine
        .read_auth_audit_records(&AuthAuditQuery {
            principal: Some("app_user".to_owned()),
            outcome: Some("failure".to_owned()),
            limit: 1,
            ..AuthAuditQuery::default()
        })
        .expect("filtered audit records");
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].principal, "app_user");
    assert_eq!(filtered[0].outcome, "failure");

    let _ = std::fs::remove_file(audit_path);
}

#[test]
fn startup_rotates_durable_auth_audit_log_when_size_limit_is_reached() {
    let audit_path = temp_audit_path("rotation");
    let rotated_path = PathBuf::from(format!("{}.1", audit_path.display()));
    let mut runtime = aiondb_config::RuntimeConfig::default();
    runtime.security =
        aiondb_config::SecurityConfig::from_profile(aiondb_config::SecurityProfile::Development);
    runtime.security.durable_auth_audit = true;
    runtime.security.auth_audit_log_path = Some(audit_path.clone());
    runtime.security.auth_audit_max_file_size_bytes = 1;
    runtime.security.auth_audit_max_rotated_files = 1;
    let engine = EngineBuilder::for_testing()
        .with_runtime_config(runtime)
        .build()
        .unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE ROLE app_user LOGIN PASSWORD 'secret'")
        .expect("create role");

    let _ = engine.startup(startup_with_password("app_user", "wrong"));
    let _ = engine.startup(startup_with_password("app_user", "wrong"));

    assert!(audit_path.exists(), "expected current audit log");
    assert!(rotated_path.exists(), "expected rotated audit log");
    let current_records = read_audit_records(&audit_path);
    let rotated_records = read_audit_records(&rotated_path);
    let total_records = current_records.len() + rotated_records.len();
    assert!(
        total_records >= 2,
        "expected at least two persisted auth audit records across rotated logs, got {total_records}"
    );
    assert!(current_records
        .iter()
        .chain(rotated_records.iter())
        .all(|record| record["record_type"] == "auth_audit"));
    let _ = std::fs::remove_file(audit_path);
    let _ = std::fs::remove_file(rotated_path);
}
