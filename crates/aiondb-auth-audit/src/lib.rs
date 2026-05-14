#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::{
    fs::{self, OpenOptions},
    io::{Read as _, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use aiondb_config::RuntimeConfig;
use aiondb_core::{DbError, DbResult, SqlState};
use aiondb_security::TransportInfo;
use serde_json::json;
use tracing::{info, warn};

mod api;

pub use api::{AuthAuditQuery, AuthAuditRecord};

const AUTH_AUDIT_SCHEMA_VERSION: u32 = 1;
const MAX_AUTH_AUDIT_READ_FILE_BYTES: u64 = 128 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthAuditStage {
    StartupAuthentication,
    Startup,
}

impl AuthAuditStage {
    fn as_str(self) -> &'static str {
        match self {
            Self::StartupAuthentication => "startup_authentication",
            Self::Startup => "startup",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthAuditOutcome {
    ChallengeIssued,
    Success,
    Failure,
}

impl AuthAuditOutcome {
    fn as_str(self) -> &'static str {
        match self {
            Self::ChallengeIssued => "challenge_issued",
            Self::Success => "success",
            Self::Failure => "failure",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthAuditMethod {
    Trust,
    CleartextPassword,
    ScramSha256,
    ScramProofToken,
    Unknown,
}

impl AuthAuditMethod {
    fn as_str(self) -> &'static str {
        match self {
            Self::Trust => "trust",
            Self::CleartextPassword => "cleartext_password",
            Self::ScramSha256 => "scram_sha_256",
            Self::ScramProofToken => "scram_proof_token",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthAuditEvent {
    pub stage: AuthAuditStage,
    pub outcome: AuthAuditOutcome,
    pub principal: String,
    pub database: String,
    pub transport_kind: &'static str,
    pub tls_enabled: bool,
    pub peer_addr: Option<String>,
    pub auth_method: Option<AuthAuditMethod>,
    pub sqlstate: Option<SqlState>,
    pub message: Option<String>,
}

impl AuthAuditEvent {
    pub fn new(
        stage: AuthAuditStage,
        outcome: AuthAuditOutcome,
        principal: &str,
        database: &str,
        transport: &TransportInfo,
    ) -> Self {
        Self {
            stage,
            outcome,
            principal: principal.to_owned(),
            database: database.to_owned(),
            transport_kind: transport_kind(transport),
            tls_enabled: transport.tls_enabled(),
            peer_addr: peer_addr(transport),
            auth_method: None,
            sqlstate: None,
            message: None,
        }
    }

    pub fn with_auth_method(mut self, auth_method: AuthAuditMethod) -> Self {
        self.auth_method = Some(auth_method);
        self
    }

    pub fn with_error(mut self, error: &DbError) -> Self {
        self.sqlstate = Some(error.sqlstate());
        self.message = Some(error.report().message.clone());
        self
    }
}

/// DDL audit event for CREATE/DROP/ALTER operations.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DdlAuditEvent {
    pub role: String,
    pub operation: String,
    pub object_type: String,
    pub object_name: String,
    pub success: bool,
    pub error_message: Option<String>,
}

pub trait AuthAuditSink: Send + Sync {
    fn record(&self, event: AuthAuditEvent);
    fn record_ddl(&self, _event: DdlAuditEvent) {}
}

#[derive(Debug, Default)]
pub struct TracingAuthAuditSink;

impl AuthAuditSink for TracingAuthAuditSink {
    fn record(&self, event: AuthAuditEvent) {
        let peer_addr = event.peer_addr.as_deref().unwrap_or("-");
        let auth_method = event.auth_method.map_or("-", AuthAuditMethod::as_str);
        let sqlstate = event.sqlstate.map_or("-", SqlState::code);
        let message = event.message.as_deref().unwrap_or("-");

        if event.outcome == AuthAuditOutcome::Failure {
            warn!(
                target: "aiondb_audit",
                stage = event.stage.as_str(),
                outcome = event.outcome.as_str(),
                principal = %event.principal,
                database = %event.database,
                transport = event.transport_kind,
                tls = event.tls_enabled,
                peer_addr = peer_addr,
                auth_method = auth_method,
                sqlstate = sqlstate,
                message = message,
                "auth audit"
            );
        } else {
            info!(
                target: "aiondb_audit",
                stage = event.stage.as_str(),
                outcome = event.outcome.as_str(),
                principal = %event.principal,
                database = %event.database,
                transport = event.transport_kind,
                tls = event.tls_enabled,
                peer_addr = peer_addr,
                auth_method = auth_method,
                sqlstate = sqlstate,
                message = message,
                "auth audit"
            );
        }
    }

    fn record_ddl(&self, event: DdlAuditEvent) {
        let outcome = if event.success { "success" } else { "failure" };
        let message = event.error_message.as_deref().unwrap_or("-");
        info!(
            target: "aiondb_audit",
            record_type = "ddl_audit",
            role = %event.role,
            operation = %event.operation,
            object_type = %event.object_type,
            object_name = %event.object_name,
            outcome = outcome,
            message = message,
            "ddl audit"
        );
    }
}

#[derive(Debug)]
pub struct FileAuthAuditSink {
    path: PathBuf,
    max_file_size_bytes: u64,
    max_rotated_files: usize,
    write_lock: Mutex<()>,
}

impl FileAuthAuditSink {
    pub fn new(
        path: impl Into<PathBuf>,
        max_file_size_bytes: u64,
        max_rotated_files: usize,
    ) -> Self {
        Self {
            path: path.into(),
            max_file_size_bytes,
            max_rotated_files,
            write_lock: Mutex::new(()),
        }
    }

    fn append_event(&self, event: &AuthAuditEvent) -> std::io::Result<()> {
        let line = format_event_line(event);
        self.append_line(&line)
    }

    fn append_line(&self, line: &str) -> std::io::Result<()> {
        let _guard = self
            .write_lock
            .lock()
            .map_err(|e| std::io::Error::other(format!("auth audit write lock poisoned: {e}")))?;
        if let Some(parent) = self
            .path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent)?;
        }
        self.rotate_if_needed(line.len().saturating_add(1))?;
        let mut opts = OpenOptions::new();
        opts.create(true).append(true);
        #[cfg(unix)]
        opts.mode(0o600);
        let mut file = opts.open(&self.path)?;
        file.write_all(line.as_bytes())?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        Ok(())
    }

    fn rotate_if_needed(&self, next_write_len: usize) -> std::io::Result<()> {
        let current_size = match fs::metadata(&self.path) {
            Ok(metadata) => metadata.len(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
            Err(error) => return Err(error),
        };
        let next_write_len_u64 = u64::try_from(next_write_len).unwrap_or(u64::MAX);
        let projected_size = current_size.saturating_add(next_write_len_u64);
        if current_size == 0 || projected_size <= self.max_file_size_bytes {
            return Ok(());
        }

        if self.max_rotated_files == 0 {
            match fs::remove_file(&self.path) {
                Ok(()) => return Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
                Err(error) => return Err(error),
            }
        }

        let oldest = rotated_audit_path(&self.path, self.max_rotated_files);
        match fs::remove_file(&oldest) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }

        for index in (1..self.max_rotated_files).rev() {
            let from = rotated_audit_path(&self.path, index);
            let to = rotated_audit_path(&self.path, index + 1);
            match fs::rename(&from, &to) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }

        match fs::rename(&self.path, rotated_audit_path(&self.path, 1)) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }
}

impl AuthAuditSink for FileAuthAuditSink {
    fn record(&self, event: AuthAuditEvent) {
        if let Err(error) = self.append_event(&event) {
            warn!(
                target: "aiondb_audit",
                path = %self.path.display(),
                error = %error,
                "failed to persist auth audit event"
            );
        }
    }

    fn record_ddl(&self, event: DdlAuditEvent) {
        let line = format_ddl_event_line(&event);
        if let Err(error) = self.append_line(&line) {
            warn!(
                target: "aiondb_audit",
                path = %self.path.display(),
                error = %error,
                "failed to persist ddl audit event"
            );
        }
    }
}

pub struct CompositeAuthAuditSink {
    sinks: Vec<Arc<dyn AuthAuditSink>>,
}

impl CompositeAuthAuditSink {
    pub fn new(sinks: Vec<Arc<dyn AuthAuditSink>>) -> Self {
        Self { sinks }
    }
}

impl AuthAuditSink for CompositeAuthAuditSink {
    fn record(&self, event: AuthAuditEvent) {
        for sink in &self.sinks {
            sink.record(event.clone());
        }
    }

    fn record_ddl(&self, event: DdlAuditEvent) {
        for sink in &self.sinks {
            sink.record_ddl(event.clone());
        }
    }
}

pub fn default_auth_audit_sink(runtime_config: &RuntimeConfig) -> Arc<dyn AuthAuditSink> {
    let tracing_sink: Arc<dyn AuthAuditSink> = Arc::new(TracingAuthAuditSink);
    if let Some(path) = durable_auth_audit_path(runtime_config) {
        let file_sink: Arc<dyn AuthAuditSink> = Arc::new(FileAuthAuditSink::new(
            path,
            runtime_config.security.auth_audit_max_file_size_bytes,
            runtime_config.security.auth_audit_max_rotated_files,
        ));
        Arc::new(CompositeAuthAuditSink::new(vec![tracing_sink, file_sink]))
    } else {
        tracing_sink
    }
}

pub fn durable_auth_audit_path(runtime_config: &RuntimeConfig) -> Option<PathBuf> {
    if !runtime_config.security.durable_auth_audit {
        return None;
    }

    Some(
        runtime_config
            .security
            .auth_audit_log_path
            .clone()
            .unwrap_or_else(|| {
                runtime_config
                    .storage
                    .data_dir
                    .join("security")
                    .join("auth_audit.log")
            }),
    )
}

pub fn read_durable_auth_audit_records(
    runtime_config: &RuntimeConfig,
    query: &AuthAuditQuery,
) -> DbResult<Vec<AuthAuditRecord>> {
    let Some(path) = durable_auth_audit_path(runtime_config) else {
        return Ok(Vec::new());
    };
    read_auth_audit_records_from_path(
        &path,
        runtime_config.security.auth_audit_max_rotated_files,
        query,
    )
}

fn format_ddl_event_line(event: &DdlAuditEvent) -> String {
    json!({
        "record_type": "ddl_audit",
        "schema_version": AUTH_AUDIT_SCHEMA_VERSION,
        "ts_ms": now_epoch_millis(),
        "role": event.role,
        "operation": event.operation,
        "object_type": event.object_type,
        "object_name": event.object_name,
        "outcome": if event.success { "success" } else { "failure" },
        "message": event.error_message,
    })
    .to_string()
}

fn format_event_line(event: &AuthAuditEvent) -> String {
    json!({
        "record_type": "auth_audit",
        "schema_version": AUTH_AUDIT_SCHEMA_VERSION,
        "ts_ms": now_epoch_millis(),
        "stage": event.stage.as_str(),
        "outcome": event.outcome.as_str(),
        "principal": event.principal,
        "database": event.database,
        "transport": event.transport_kind,
        "tls": event.tls_enabled,
        "peer_addr": event.peer_addr,
        "auth_method": event.auth_method.map(AuthAuditMethod::as_str),
        "sqlstate": event.sqlstate.map(SqlState::code),
        "message": event.message,
    })
    .to_string()
}

fn now_epoch_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn rotated_audit_path(path: &Path, index: usize) -> PathBuf {
    PathBuf::from(format!("{}.{}", path.display(), index))
}

fn transport_kind(transport: &TransportInfo) -> &'static str {
    match &transport.kind {
        aiondb_security::TransportKind::Network { .. } => "network",
        _ => "in_process",
    }
}

fn peer_addr(transport: &TransportInfo) -> Option<String> {
    match &transport.kind {
        aiondb_security::TransportKind::Network { peer_addr, .. } => peer_addr.clone(),
        _ => None,
    }
}

fn read_auth_audit_records_from_path(
    path: &Path,
    max_rotated_files: usize,
    query: &AuthAuditQuery,
) -> DbResult<Vec<AuthAuditRecord>> {
    if query.limit == 0 {
        return Ok(Vec::new());
    }

    let mut records = Vec::new();
    for candidate in candidate_audit_paths(path, max_rotated_files, query.include_rotated) {
        let content = match read_auth_audit_file_to_string(&candidate) {
            Ok(content) => content,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(DbError::internal(format!(
                    "failed to read auth audit log {}: {error}",
                    candidate.display()
                )))
            }
        };

        let lines: Vec<&str> = content.lines().collect();
        for (index, line) in lines.iter().enumerate().rev() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            let record = parse_auth_audit_record(&candidate, index + 1, line)?;
            if record_matches_query(&record, query) {
                records.push(record);
                if records.len() >= query.limit {
                    return Ok(records);
                }
            }
        }
    }

    Ok(records)
}

fn read_auth_audit_file_to_string(path: &Path) -> std::io::Result<String> {
    let file = fs::File::open(path)?;
    let file_len = file.metadata()?.len();
    if file_len > MAX_AUTH_AUDIT_READ_FILE_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "auth audit log exceeds maximum read size of {MAX_AUTH_AUDIT_READ_FILE_BYTES} bytes"
            ),
        ));
    }

    let capacity = usize::try_from(file_len).map_err(std::io::Error::other)?;
    let mut bytes = Vec::with_capacity(capacity);
    let mut limited = file.take(MAX_AUTH_AUDIT_READ_FILE_BYTES.saturating_add(1));
    limited.read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_AUTH_AUDIT_READ_FILE_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "auth audit log grew while reading beyond {MAX_AUTH_AUDIT_READ_FILE_BYTES} bytes"
            ),
        ));
    }
    String::from_utf8(bytes)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
}

fn candidate_audit_paths(
    path: &Path,
    max_rotated_files: usize,
    include_rotated: bool,
) -> Vec<PathBuf> {
    let mut paths = vec![path.to_path_buf()];
    if include_rotated {
        for index in 1..=max_rotated_files {
            paths.push(rotated_audit_path(path, index));
        }
    }
    paths
}

fn parse_auth_audit_record(
    source_path: &Path,
    line_number: usize,
    line: &str,
) -> DbResult<AuthAuditRecord> {
    let value: serde_json::Value = serde_json::from_str(line).map_err(|error| {
        DbError::internal(format!(
            "invalid auth audit json in {}:{}: {error}",
            source_path.display(),
            line_number
        ))
    })?;
    let object = value.as_object().ok_or_else(|| {
        DbError::internal(format!(
            "auth audit record in {}:{} must be a JSON object",
            source_path.display(),
            line_number
        ))
    })?;

    let record_type = required_str_field(object, "record_type", source_path, line_number)?;
    if record_type != "auth_audit" {
        return Err(DbError::internal(format!(
            "unsupported auth audit record type '{}' in {}:{}",
            record_type,
            source_path.display(),
            line_number
        )));
    }

    let schema_version = required_u32_field(object, "schema_version", source_path, line_number)?;
    if schema_version != AUTH_AUDIT_SCHEMA_VERSION {
        return Err(DbError::internal(format!(
            "unsupported auth audit schema version {} in {}:{}; supported versions: {}",
            schema_version,
            source_path.display(),
            line_number,
            AUTH_AUDIT_SCHEMA_VERSION
        )));
    }

    Ok(AuthAuditRecord {
        source_path: source_path.to_path_buf(),
        line_number,
        schema_version,
        ts_ms: required_u128_field(object, "ts_ms", source_path, line_number)?,
        stage: required_str_field(object, "stage", source_path, line_number)?,
        outcome: required_str_field(object, "outcome", source_path, line_number)?,
        principal: required_str_field(object, "principal", source_path, line_number)?,
        database: required_str_field(object, "database", source_path, line_number)?,
        transport: required_str_field(object, "transport", source_path, line_number)?,
        tls: required_bool_field(object, "tls", source_path, line_number)?,
        peer_addr: optional_str_field(object, "peer_addr", source_path, line_number)?,
        auth_method: optional_str_field(object, "auth_method", source_path, line_number)?,
        sqlstate: optional_str_field(object, "sqlstate", source_path, line_number)?,
        message: optional_str_field(object, "message", source_path, line_number)?,
    })
}

fn record_matches_query(record: &AuthAuditRecord, query: &AuthAuditQuery) -> bool {
    matches_string_filter(query.principal.as_deref(), Some(record.principal.as_str()))
        && matches_string_filter(query.database.as_deref(), Some(record.database.as_str()))
        && matches_string_filter(query.stage.as_deref(), Some(record.stage.as_str()))
        && matches_string_filter(query.outcome.as_deref(), Some(record.outcome.as_str()))
        && matches_string_filter(query.transport.as_deref(), Some(record.transport.as_str()))
        && matches_bool_filter(query.tls, record.tls)
        && matches_string_filter(query.auth_method.as_deref(), record.auth_method.as_deref())
        && matches_string_filter(query.sqlstate.as_deref(), record.sqlstate.as_deref())
        && matches_min_ts_filter(query.min_ts_ms, record.ts_ms)
        && matches_max_ts_filter(query.max_ts_ms, record.ts_ms)
}

fn matches_string_filter(expected: Option<&str>, actual: Option<&str>) -> bool {
    match expected {
        Some(expected) => actual == Some(expected),
        None => true,
    }
}

fn matches_bool_filter(expected: Option<bool>, actual: bool) -> bool {
    match expected {
        Some(expected) => actual == expected,
        None => true,
    }
}

fn matches_min_ts_filter(min_ts_ms: Option<u128>, actual: u128) -> bool {
    match min_ts_ms {
        Some(min_ts_ms) => actual >= min_ts_ms,
        None => true,
    }
}

fn matches_max_ts_filter(max_ts_ms: Option<u128>, actual: u128) -> bool {
    match max_ts_ms {
        Some(max_ts_ms) => actual <= max_ts_ms,
        None => true,
    }
}

fn required_str_field(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
    source_path: &Path,
    line_number: usize,
) -> DbResult<String> {
    let value = object.get(field).ok_or_else(|| {
        DbError::internal(format!(
            "missing auth audit field '{}' in {}:{}",
            field,
            source_path.display(),
            line_number
        ))
    })?;
    value.as_str().map(str::to_owned).ok_or_else(|| {
        DbError::internal(format!(
            "auth audit field '{}' in {}:{} must be a string",
            field,
            source_path.display(),
            line_number
        ))
    })
}

fn optional_str_field(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
    source_path: &Path,
    line_number: usize,
) -> DbResult<Option<String>> {
    match object.get(field) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(value) => value
            .as_str()
            .map(|value| Some(value.to_owned()))
            .ok_or_else(|| {
                DbError::internal(format!(
                    "auth audit field '{}' in {}:{} must be a string or null",
                    field,
                    source_path.display(),
                    line_number
                ))
            }),
    }
}

fn required_bool_field(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
    source_path: &Path,
    line_number: usize,
) -> DbResult<bool> {
    let value = object.get(field).ok_or_else(|| {
        DbError::internal(format!(
            "missing auth audit field '{}' in {}:{}",
            field,
            source_path.display(),
            line_number
        ))
    })?;
    value.as_bool().ok_or_else(|| {
        DbError::internal(format!(
            "auth audit field '{}' in {}:{} must be a boolean",
            field,
            source_path.display(),
            line_number
        ))
    })
}

fn required_u32_field(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
    source_path: &Path,
    line_number: usize,
) -> DbResult<u32> {
    let value = object.get(field).ok_or_else(|| {
        DbError::internal(format!(
            "missing auth audit field '{}' in {}:{}",
            field,
            source_path.display(),
            line_number
        ))
    })?;
    let value = value.as_u64().ok_or_else(|| {
        DbError::internal(format!(
            "auth audit field '{}' in {}:{} must be an unsigned integer",
            field,
            source_path.display(),
            line_number
        ))
    })?;
    u32::try_from(value).map_err(|_| {
        DbError::internal(format!(
            "auth audit field '{}' in {}:{} exceeds u32",
            field,
            source_path.display(),
            line_number
        ))
    })
}

fn required_u128_field(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
    source_path: &Path,
    line_number: usize,
) -> DbResult<u128> {
    let value = object.get(field).ok_or_else(|| {
        DbError::internal(format!(
            "missing auth audit field '{}' in {}:{}",
            field,
            source_path.display(),
            line_number
        ))
    })?;
    value.as_u64().map(u128::from).ok_or_else(|| {
        DbError::internal(format!(
            "auth audit field '{}' in {}:{} must be an unsigned integer",
            field,
            source_path.display(),
            line_number
        ))
    })
}

#[cfg(test)]
mod tests;
