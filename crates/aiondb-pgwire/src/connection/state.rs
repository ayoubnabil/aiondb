use super::*;

impl<E, R, W> Connection<E, R, W>
where
    E: PgWireEngine + 'static,
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    pub(super) fn message_allowed_in_failed_transaction(&self, msg: &FrontendMessage) -> bool {
        match msg {
            FrontendMessage::Sync | FrontendMessage::Flush | FrontendMessage::Terminate => true,
            FrontendMessage::Describe { .. } | FrontendMessage::Close { .. } => true,
            FrontendMessage::Query(sql) => Self::is_failed_transaction_recovery_query(sql),
            FrontendMessage::Parse { query, .. } => {
                Self::is_failed_transaction_recovery_query(query)
            }
            FrontendMessage::Bind { statement, .. } => self
                .statement_wire_state
                .get(statement)
                .is_some_and(Self::is_failed_transaction_recovery_wire_state),
            FrontendMessage::Execute { portal, .. } => self
                .portal_wire_state
                .get(portal)
                .and_then(|portal_state| {
                    self.statement_wire_state.get(&portal_state.statement_name)
                })
                .is_some_and(Self::is_failed_transaction_recovery_wire_state),
            _ => false,
        }
    }

    fn is_failed_transaction_recovery_wire_state(state: &StatementWireState) -> bool {
        state
            .parsed_statement
            .as_ref()
            .is_some_and(Self::is_failed_transaction_recovery_statement)
            || Self::is_failed_transaction_recovery_query(&state.query)
    }

    pub(super) fn is_failed_transaction_recovery_query(sql: &str) -> bool {
        let Ok(statements) = aiondb_parser::parse_sql(sql) else {
            return false;
        };
        if statements.is_empty() {
            return false;
        }

        let mut failed_transaction_active = true;
        for statement in &statements {
            if failed_transaction_active {
                if !Self::is_failed_transaction_recovery_statement(statement) {
                    return false;
                }
                failed_transaction_active = false;
            }
        }
        true
    }

    fn is_failed_transaction_recovery_statement(statement: &aiondb_parser::Statement) -> bool {
        match statement {
            aiondb_parser::Statement::Commit { .. }
            | aiondb_parser::Statement::Rollback { .. }
            | aiondb_parser::Statement::RollbackToSavepoint { .. } => true,
            aiondb_parser::Statement::ExecuteStmt { .. } => true,
            aiondb_parser::Statement::CompatParserStub { tag, .. } => tag == "EXECUTE",
            _ => false,
        }
    }

    pub(super) fn failed_transaction_error() -> DbError {
        DbError::transaction_error(SqlState::InFailedSqlTransaction, FAILED_TRANSACTION_MESSAGE)
    }

    pub(super) async fn write_failed_transaction_response(&mut self) -> Result<(), DbError> {
        let mut w = MessageWriter::new();
        let error = Self::failed_transaction_error();
        messages::write_error_response(&mut w, &error);
        messages::write_ready_for_query(&mut w, self.txn_status);
        w.flush(&mut self.writer).await?;
        Ok(())
    }

    pub(super) async fn active_transaction_status(&self) -> Result<TransactionStatus, DbError> {
        let session = self
            .session
            .as_ref()
            .ok_or_else(|| DbError::protocol("no active session"))?;
        let has_active = self
            .run_engine({
                let session = session.clone();
                move |engine| engine.has_active_transaction(&session)
            })
            .await?;
        Ok(if has_active {
            TransactionStatus::InTransaction
        } else {
            TransactionStatus::Idle
        })
    }

    pub(super) async fn refresh_txn_status_after_success(&mut self) -> Result<(), DbError> {
        self.txn_status = self.active_transaction_status().await?;
        Ok(())
    }

    pub(super) async fn refresh_txn_status_after_error(&mut self) -> Result<(), DbError> {
        self.txn_status = match self.active_transaction_status().await? {
            TransactionStatus::InTransaction => TransactionStatus::Failed,
            _ => TransactionStatus::Idle,
        };
        Ok(())
    }

    pub(super) async fn enter_extended_query_error_state(&mut self) -> Result<(), DbError> {
        self.skip_until_sync = true;
        self.refresh_txn_status_after_error().await
    }

    pub(super) async fn ready_for_query_status(&self) -> Result<TransactionStatus, DbError> {
        Ok(
            match (self.txn_status, self.active_transaction_status().await?) {
                (TransactionStatus::Failed, TransactionStatus::InTransaction) => {
                    TransactionStatus::Failed
                }
                (_, status) => status,
            },
        )
    }
}
