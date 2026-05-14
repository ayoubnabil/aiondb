use aiondb_core::{DbError, DbResult, SqlState};

use super::async_notify::{validate_channel_name, Notification};
use super::Engine;
use crate::engine::support::command_ok;
use crate::prepared::StatementResult;
use crate::session::SessionHandle;

impl Engine {
    /// Prefix the user-provided channel name with the current session's
    /// tenant id so two tenants that subscribe to the same logical channel
    /// name never see each other's NOTIFY payloads (audit notify F-N1).
    fn scope_channel(&self, session: &SessionHandle, channel: &str) -> DbResult<String> {
        let tenant_id = self.with_session(session, |record| Ok(record.tenant_id))?;
        Ok(match tenant_id {
            Some(id) => format!("t{}:{channel}", id.get()),
            None => format!("g:{channel}"),
        })
    }

    pub(super) fn execute_listen_statement(
        &self,
        session: &SessionHandle,
        channel: &str,
    ) -> DbResult<StatementResult> {
        let channel = validate_channel(channel)?;
        let routing_key = self.scope_channel(session, &channel)?;
        self.notification_bus()
            .listen(session, &channel, &routing_key);
        Ok(command_ok("LISTEN"))
    }

    pub(super) fn execute_unlisten_statement(
        &self,
        session: &SessionHandle,
        channel: Option<&str>,
    ) -> DbResult<StatementResult> {
        match channel {
            None => self.notification_bus().unlisten_all(session),
            Some(name) => {
                let channel = validate_channel(name)?;
                let routing_key = self.scope_channel(session, &channel)?;
                self.notification_bus()
                    .unlisten(session, &channel, &routing_key);
            }
        }
        Ok(command_ok("UNLISTEN"))
    }

    pub(super) fn execute_notify_statement(
        &self,
        session: &SessionHandle,
        channel: &str,
        payload: Option<&str>,
    ) -> DbResult<StatementResult> {
        self.enqueue_notification(session, channel, payload.unwrap_or(""))?;
        Ok(command_ok("NOTIFY"))
    }

    pub(super) fn enqueue_notification(
        &self,
        session: &SessionHandle,
        channel: &str,
        payload: &str,
    ) -> DbResult<()> {
        let channel = validate_channel(channel)?;
        let routing_key = self.scope_channel(session, &channel)?;
        if payload.len() > 8000 {
            return Err(DbError::bind_error(
                SqlState::ProgramLimitExceeded,
                "payload string too long",
            ));
        }
        // Mirror PG: refuse `\0` in the payload to avoid C-string truncation
        // surprises and downstream log/HTML injection (audit notify F-N3).
        if payload.contains('\0') {
            return Err(DbError::bind_error(
                SqlState::InvalidParameterValue,
                "unexpected null character in NOTIFY payload",
            ));
        }
        let notification = Notification {
            channel,
            routing_key,
            payload: payload.to_owned(),
        };
        let in_explicit_txn = self.with_session(session, |record| {
            Ok(record.active_txn.is_some() && !record.implicit_txn_active)
        })?;
        if in_explicit_txn {
            self.with_session_mut(session, |record| {
                record.pending_notifications.push(notification.clone());
                Ok(())
            })?;
        } else {
            self.notification_bus()
                .publish(std::slice::from_ref(&notification));
        }
        Ok(())
    }

    /// Append the notifications buffered on `session.pending_notifications`
    /// to the bus. Called on successful commit (explicit or implicit).
    pub(super) fn flush_pending_notifications(&self, session: &SessionHandle) {
        let pending = self
            .with_session_mut(session, |record| {
                Ok(std::mem::take(&mut record.pending_notifications))
            })
            .unwrap_or_default();
        if !pending.is_empty() {
            self.notification_bus().publish(&pending);
        }
    }

    /// Drop notifications buffered on `session.pending_notifications`.
    /// Called on rollback.
    pub(super) fn discard_pending_notifications(&self, session: &SessionHandle) {
        let _ = self.with_session_mut(session, |record| {
            record.pending_notifications.clear();
            Ok(())
        });
    }
}

fn validate_channel(channel: &str) -> DbResult<String> {
    match validate_channel_name(Some(channel)) {
        Ok(name) => Ok(name),
        Err(message) => Err(DbError::bind_error(
            SqlState::InvalidParameterValue,
            message,
        )),
    }
}
