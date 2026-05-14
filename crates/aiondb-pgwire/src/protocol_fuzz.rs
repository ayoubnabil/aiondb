//! Protocol-level fuzz smoke tests.
//!
//! Each test feeds crafted (malformed) byte sequences into a [`Connection`] via
//! an in-memory `Cursor` reader and verifies that:
//! - The server does **not** panic.
//! - The server either responds with an error message or cleanly disconnects.
//!
//! The tests are deterministic and fast -- no network I/O is involved.

use std::io::Cursor;
use std::sync::Arc;

use aiondb_core::{DataType, DatabaseId, DbError, Value};
use aiondb_engine::{
    AuthenticatedIdentity, DbResult, IsolationLevel, PortalBatch, PortalDescription,
    PreparedStatementDesc, QueryEngine, ResultColumn, SessionHandle, SessionInfo, SessionLimits,
    StartupParams, StatementResult,
};

use crate::codec;
use crate::connection::Connection;
use crate::server::CancelRegistry;

// -------------------------------------------------------------------------
// Mock engine (mirrors the one in connection::tests)
// -------------------------------------------------------------------------

struct MockEngine;

impl MockEngine {
    fn dummy_session_info() -> SessionInfo {
        SessionInfo {
            identity: AuthenticatedIdentity {
                user: "test".to_string(),
                database_id: DatabaseId::new(0),
                roles: vec!["test".to_string()],
            },
            is_superuser: false,
            limits: SessionLimits::default(),
            database_name: "test".to_string(),
            active_database: aiondb_cluster::DatabaseId::DEFAULT,
        }
    }
}

impl QueryEngine for MockEngine {
    fn requires_password(&self) -> bool {
        false
    }
    fn startup(&self, _params: StartupParams) -> DbResult<(SessionHandle, SessionInfo)> {
        Ok((SessionHandle::test_handle(), Self::dummy_session_info()))
    }
    fn has_active_transaction(&self, _session: &SessionHandle) -> DbResult<bool> {
        Ok(false)
    }
    fn begin_transaction(
        &self,
        _session: &SessionHandle,
        _isolation: IsolationLevel,
    ) -> DbResult<()> {
        Ok(())
    }
    fn commit_transaction(&self, _session: &SessionHandle) -> DbResult<()> {
        Ok(())
    }
    fn rollback_transaction(&self, _session: &SessionHandle) -> DbResult<()> {
        Ok(())
    }
    fn execute_sql(&self, _session: &SessionHandle, _sql: &str) -> DbResult<Vec<StatementResult>> {
        Ok(vec![StatementResult::Query {
            columns: vec![ResultColumn {
                name: "col".to_string(),
                data_type: DataType::Int,
                text_type_modifier: None,
                nullable: false,
            }],
            rows: vec![],
        }])
    }
    fn prepare(
        &self,
        _session: &SessionHandle,
        _name: String,
        _sql: String,
    ) -> DbResult<PreparedStatementDesc> {
        Ok(PreparedStatementDesc {
            name: String::new(),
            param_types: vec![],
            result_columns: vec![],
            result_column_origins: vec![],
        })
    }
    fn describe_statement(
        &self,
        _session: &SessionHandle,
        _name: &str,
    ) -> DbResult<PreparedStatementDesc> {
        Ok(PreparedStatementDesc {
            name: String::new(),
            param_types: vec![],
            result_columns: vec![],
            result_column_origins: vec![],
        })
    }
    fn bind(
        &self,
        _session: &SessionHandle,
        _portal: String,
        _stmt: String,
        _params: Vec<Value>,
    ) -> DbResult<()> {
        Ok(())
    }
    fn describe_portal(
        &self,
        _session: &SessionHandle,
        _name: &str,
    ) -> DbResult<PortalDescription> {
        Ok(PortalDescription {
            result_columns: vec![],
            result_column_origins: vec![],
        })
    }
    fn execute_portal(
        &self,
        _session: &SessionHandle,
        _name: &str,
        _max: usize,
    ) -> DbResult<PortalBatch> {
        Ok(PortalBatch {
            columns: vec![],
            rows: vec![],
            tag: "SELECT".to_string(),
            rows_affected: 0,
            exhausted: true,
        })
    }
    fn close_statement(&self, _session: &SessionHandle, _name: &str) -> DbResult<()> {
        Ok(())
    }
    fn close_portal(&self, _session: &SessionHandle, _name: &str) -> DbResult<()> {
        Ok(())
    }
    fn execute_copy_from(
        &self,
        _session: &SessionHandle,
        _table_id: aiondb_core::RelationId,
        _data: &str,
    ) -> DbResult<StatementResult> {
        Ok(StatementResult::Command {
            tag: "COPY".to_owned(),
            rows_affected: 0,
        })
    }
    fn cancel_session(&self, _session: &SessionHandle) -> DbResult<()> {
        Ok(())
    }
    fn terminate(&self, _session: SessionHandle) -> DbResult<()> {
        Ok(())
    }
    fn session_count(&self) -> DbResult<usize> {
        Ok(0)
    }
}

// -------------------------------------------------------------------------
// Helpers
// -------------------------------------------------------------------------

/// Run the connection with the given raw input bytes and return the result.
///
async fn run_with_bytes(input: Vec<u8>) -> Result<(), DbError> {
    let engine = Arc::new(MockEngine);
    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run().await
}

/// Build a valid v3 startup message.
fn build_startup_bytes() -> Vec<u8> {
    let mut payload = Vec::new();
    payload.extend_from_slice(&codec::PROTOCOL_V3.to_be_bytes());
    payload.extend_from_slice(b"user\0test\0\0");
    let len = (payload.len() as u32) + 4;
    let mut data = Vec::new();
    data.extend_from_slice(&len.to_be_bytes());
    data.extend_from_slice(&payload);
    data
}

/// Build a Terminate message (`X` + length 4).
fn build_terminate_bytes() -> Vec<u8> {
    let mut data = Vec::new();
    data.push(b'X');
    data.extend_from_slice(&4u32.to_be_bytes());
    data
}

/// Build a simple Query message for the given SQL.
fn build_query_bytes(sql: &str) -> Vec<u8> {
    let mut data = Vec::new();
    data.push(b'Q');
    let payload_len = sql.len() + 1; // +1 for null terminator
    let msg_len = (payload_len as u32) + 4;
    data.extend_from_slice(&msg_len.to_be_bytes());
    data.extend_from_slice(sql.as_bytes());
    data.push(0);
    data
}

/// Build a raw frontend message with the given tag and payload.
fn build_raw_message(tag: u8, payload: &[u8]) -> Vec<u8> {
    let mut data = Vec::new();
    data.push(tag);
    let msg_len = (payload.len() as u32) + 4;
    data.extend_from_slice(&msg_len.to_be_bytes());
    data.extend_from_slice(payload);
    data
}

/// Build a startup message with a given raw payload (after the 4-byte length).
fn build_startup_raw(payload: &[u8]) -> Vec<u8> {
    let len = (payload.len() as u32) + 4;
    let mut data = Vec::new();
    data.extend_from_slice(&len.to_be_bytes());
    data.extend_from_slice(payload);
    data
}

/// Build a Parse message (`P`).
fn build_parse_bytes(stmt_name: &str, query: &str, param_oids: &[u32]) -> Vec<u8> {
    let mut payload = Vec::new();
    payload.extend_from_slice(stmt_name.as_bytes());
    payload.push(0);
    payload.extend_from_slice(query.as_bytes());
    payload.push(0);
    payload.extend_from_slice(&(param_oids.len() as i16).to_be_bytes());
    for &oid in param_oids {
        payload.extend_from_slice(&oid.to_be_bytes());
    }
    build_raw_message(b'P', &payload)
}

/// Build a Bind message (`B`).
fn build_bind_bytes(
    portal: &str,
    statement: &str,
    param_formats: &[i16],
    param_values: &[Option<&[u8]>],
    result_formats: &[i16],
) -> Vec<u8> {
    let mut payload = Vec::new();
    payload.extend_from_slice(portal.as_bytes());
    payload.push(0);
    payload.extend_from_slice(statement.as_bytes());
    payload.push(0);
    // Param format codes.
    payload.extend_from_slice(&(param_formats.len() as i16).to_be_bytes());
    for &f in param_formats {
        payload.extend_from_slice(&f.to_be_bytes());
    }
    // Param values.
    payload.extend_from_slice(&(param_values.len() as i16).to_be_bytes());
    for v in param_values {
        match v {
            None => payload.extend_from_slice(&(-1i32).to_be_bytes()),
            Some(data) => {
                payload.extend_from_slice(&(data.len() as i32).to_be_bytes());
                payload.extend_from_slice(data);
            }
        }
    }
    // Result format codes.
    payload.extend_from_slice(&(result_formats.len() as i16).to_be_bytes());
    for &f in result_formats {
        payload.extend_from_slice(&f.to_be_bytes());
    }
    build_raw_message(b'B', &payload)
}

/// Build a Describe message (`D`).
fn build_describe_bytes(target: u8, name: &str) -> Vec<u8> {
    let mut payload = Vec::new();
    payload.push(target);
    payload.extend_from_slice(name.as_bytes());
    payload.push(0);
    build_raw_message(b'D', &payload)
}

/// Build an Execute message (`E`).
fn build_execute_bytes(portal: &str, max_rows: i32) -> Vec<u8> {
    let mut payload = Vec::new();
    payload.extend_from_slice(portal.as_bytes());
    payload.push(0);
    payload.extend_from_slice(&max_rows.to_be_bytes());
    build_raw_message(b'E', &payload)
}

/// Build a Sync message (`S` + length 4).
fn build_sync_bytes() -> Vec<u8> {
    build_raw_message(b'S', &[])
}

mod additional_edges;
mod binary;
mod disconnect;
mod flush_sync;
mod invalid_messages;
mod oversized;
mod sequence;
mod startup;
mod terminate_cancel;
mod truncated;
