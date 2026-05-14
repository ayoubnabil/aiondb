// End-to-end tests for the extended query protocol (MISS-W7).
//
// Each test constructs raw byte sequences representing a full
// Parse -> Bind -> Describe -> Execute -> Close -> Sync flow and feeds them
// into a `Connection` via in-memory readers/writers. The tests verify that
// the connection processes the complete sequence without error.

use std::io::Cursor;
use std::sync::{Arc, Mutex};

use aiondb_core::{DataType, DatabaseId, DbError, RelationId, Row, Value, VectorValue};
use aiondb_engine::prepared::{portal_batch_copy_in_tag, PORTAL_BATCH_COPY_OUT_TAG};
use aiondb_engine::{
    AuthenticatedIdentity, Credential, DbResult, Engine, EngineBuilder, IsolationLevel,
    PortalBatch, PortalDescription, PreparedStatementDesc, QueryEngine, ResultColumn,
    SessionHandle, SessionInfo, SessionLimits, StartupParams, StatementResult, TransportInfo,
};

use crate::codec;
use crate::connection::Connection;
use crate::server::CancelRegistry;

// -------------------------------------------------------------------------
// Mock engine
// -------------------------------------------------------------------------

/// Mock engine that returns configurable results for extended query operations.
struct ExtendedMockEngine;

impl ExtendedMockEngine {
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

impl QueryEngine for ExtendedMockEngine {
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
        sql: String,
    ) -> DbResult<PreparedStatementDesc> {
        // If the SQL starts with "ERROR", simulate a parse error.
        if sql.trim().to_uppercase().starts_with("ERROR") {
            return Err(DbError::protocol("mock parse error"));
        }
        // If the SQL starts with "INSERT", return a command-style description.
        if sql.trim().to_uppercase().starts_with("INSERT") {
            return Ok(PreparedStatementDesc {
                name: String::new(),
                param_types: vec![],
                result_columns: vec![],
                result_column_origins: vec![],
            });
        }
        // Default: return a single-column SELECT description.
        Ok(PreparedStatementDesc {
            name: String::new(),
            param_types: vec![],
            result_columns: vec![ResultColumn {
                name: "id".to_string(),
                data_type: DataType::Int,
                text_type_modifier: None,
                nullable: false,
            }],
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
            result_columns: vec![ResultColumn {
                name: "id".to_string(),
                data_type: DataType::Int,
                text_type_modifier: None,
                nullable: false,
            }],
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
            result_columns: vec![ResultColumn {
                name: "id".to_string(),
                data_type: DataType::Int,
                text_type_modifier: None,
                nullable: false,
            }],
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
            columns: vec![ResultColumn {
                name: "id".to_string(),
                data_type: DataType::Int,
                text_type_modifier: None,
                nullable: false,
            }],
            rows: vec![Row::new(vec![Value::Int(42)])],
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

/// Mock engine for INSERT commands (returns empty result columns and INSERT tag).
struct InsertMockEngine;

impl InsertMockEngine {
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

impl QueryEngine for InsertMockEngine {
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
        Ok(vec![])
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
            tag: "INSERT".to_string(),
            rows_affected: 1,
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

struct NoticePortalMockEngine;

impl NoticePortalMockEngine {
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

impl QueryEngine for NoticePortalMockEngine {
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
        Ok(vec![])
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
            tag: "NOTICE: compatibility notice".to_string(),
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

struct CopyInPortalMockEngine;

impl CopyInPortalMockEngine {
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

impl QueryEngine for CopyInPortalMockEngine {
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
        Ok(vec![])
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
            tag: portal_batch_copy_in_tag(RelationId::new(42), 2),
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
        table_id: RelationId,
        data: &str,
    ) -> DbResult<StatementResult> {
        assert_eq!(table_id, RelationId::new(42));
        assert_eq!(data, "1\talice\n");
        Ok(StatementResult::Command {
            tag: "COPY".to_owned(),
            rows_affected: 1,
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

struct CopyInNoticePortalMockEngine;

impl CopyInNoticePortalMockEngine {
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

impl QueryEngine for CopyInNoticePortalMockEngine {
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
        Ok(vec![])
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
            tag: portal_batch_copy_in_tag(RelationId::new(42), 2),
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
        table_id: RelationId,
        data: &str,
    ) -> DbResult<StatementResult> {
        assert_eq!(table_id, RelationId::new(42));
        assert_eq!(data, "1\talice\n");
        Ok(StatementResult::Notice {
            message: "copy compatibility notice".to_owned(),
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

struct CopyOutPortalMockEngine;

impl CopyOutPortalMockEngine {
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

impl QueryEngine for CopyOutPortalMockEngine {
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
        Ok(vec![])
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
            columns: vec![
                ResultColumn {
                    name: "id".to_string(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                },
                ResultColumn {
                    name: "name".to_string(),
                    data_type: DataType::Text,
                    text_type_modifier: None,
                    nullable: true,
                },
            ],
            rows: vec![Row::new(vec![Value::Text("1\talice\n2\tbob".to_owned())])],
            tag: PORTAL_BATCH_COPY_OUT_TAG.to_owned(),
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
        _table_id: RelationId,
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

struct CopyInPendingNoticePortalMockEngine {
    pending_notice: Mutex<bool>,
}

impl CopyInPendingNoticePortalMockEngine {
    fn new() -> Self {
        Self {
            pending_notice: Mutex::new(false),
        }
    }

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

impl QueryEngine for CopyInPendingNoticePortalMockEngine {
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
    fn execute_sql(&self, _session: &SessionHandle, sql: &str) -> DbResult<Vec<StatementResult>> {
        if sql.trim().eq_ignore_ascii_case("COPY t FROM STDIN") {
            return Ok(vec![StatementResult::CopyIn {
                table_id: RelationId::new(42),
                columns: vec![
                    ResultColumn {
                        name: "id".to_owned(),
                        data_type: DataType::Int,
                        text_type_modifier: None,
                        nullable: true,
                    },
                    ResultColumn {
                        name: "name".to_owned(),
                        data_type: DataType::Text,
                        text_type_modifier: None,
                        nullable: true,
                    },
                ],
            }]);
        }
        Ok(vec![])
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
            tag: portal_batch_copy_in_tag(RelationId::new(42), 2),
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
        table_id: RelationId,
        data: &str,
    ) -> DbResult<StatementResult> {
        assert_eq!(table_id, RelationId::new(42));
        assert_eq!(data, "1\talice\n");
        *self
            .pending_notice
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = true;
        Ok(StatementResult::Command {
            tag: "COPY".to_owned(),
            rows_affected: 1,
        })
    }
    fn drain_pending_notices(&self, _session: &SessionHandle) -> DbResult<Vec<String>> {
        let mut pending = self
            .pending_notice
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if std::mem::take(&mut *pending) {
            Ok(vec!["copy pending notice".to_owned()])
        } else {
            Ok(Vec::new())
        }
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

struct EmptyCopyOutPortalMockEngine;

impl QueryEngine for EmptyCopyOutPortalMockEngine {
    fn requires_password(&self) -> bool {
        false
    }
    fn startup(&self, _params: StartupParams) -> DbResult<(SessionHandle, SessionInfo)> {
        Ok((
            SessionHandle::test_handle(),
            ExtendedMockEngine::dummy_session_info(),
        ))
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
        Ok(vec![])
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
            columns: vec![
                ResultColumn {
                    name: "id".to_string(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                },
                ResultColumn {
                    name: "name".to_string(),
                    data_type: DataType::Text,
                    text_type_modifier: None,
                    nullable: true,
                },
            ],
            rows: vec![Row::new(vec![Value::Text(String::new())])],
            tag: PORTAL_BATCH_COPY_OUT_TAG.to_owned(),
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
        _table_id: RelationId,
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
