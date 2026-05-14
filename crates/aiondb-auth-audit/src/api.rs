use std::path::PathBuf;

/// Query parameters for reading durable authentication audit records.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthAuditQuery {
    pub principal: Option<String>,
    pub database: Option<String>,
    pub stage: Option<String>,
    pub outcome: Option<String>,
    pub transport: Option<String>,
    pub tls: Option<bool>,
    pub auth_method: Option<String>,
    pub sqlstate: Option<String>,
    pub min_ts_ms: Option<u128>,
    pub max_ts_ms: Option<u128>,
    pub include_rotated: bool,
    pub limit: usize,
}

impl Default for AuthAuditQuery {
    fn default() -> Self {
        Self {
            principal: None,
            database: None,
            stage: None,
            outcome: None,
            transport: None,
            tls: None,
            auth_method: None,
            sqlstate: None,
            min_ts_ms: None,
            max_ts_ms: None,
            include_rotated: true,
            limit: 100,
        }
    }
}

/// A single durable authentication audit record loaded from the JSONL log.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthAuditRecord {
    pub source_path: PathBuf,
    pub line_number: usize,
    pub schema_version: u32,
    pub ts_ms: u128,
    pub stage: String,
    pub outcome: String,
    pub principal: String,
    pub database: String,
    pub transport: String,
    pub tls: bool,
    pub peer_addr: Option<String>,
    pub auth_method: Option<String>,
    pub sqlstate: Option<String>,
    pub message: Option<String>,
}
