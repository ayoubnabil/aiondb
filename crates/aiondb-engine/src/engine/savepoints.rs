#![allow(clippy::wildcard_imports)]

use super::*;

impl Engine {
    fn no_active_savepoint_transaction_error(command: &str) -> DbError {
        DbError::transaction_error(
            aiondb_core::SqlState::NoActiveSqlTransaction,
            format!("{command} can only be used in transaction block"),
        )
    }

    fn missing_savepoint_error(name: &str) -> DbError {
        DbError::transaction_error(
            aiondb_core::SqlState::InvalidSavepointSpecification,
            format!("savepoint \"{name}\" does not exist"),
        )
    }

    fn abort_failed_savepoint_rollback(&self, session: &SessionHandle, error: DbError) -> DbError {
        let error = error
            .with_client_detail(
                "transaction was rolled back because ROLLBACK TO SAVEPOINT could not be completed",
            )
            .with_client_hint("BEGIN a new transaction before continuing");
        match self.take_session_txn(session) {
            Ok(Some(txn)) => match self.rollback_active_transaction(txn, true, true) {
                Ok(()) => error,
                Err(rollback_error) => super::support::with_appended_internal_detail(
                    error,
                    format!(
                        "full transaction rollback after savepoint failure also failed: {rollback_error}"
                    ),
                ),
            },
            Ok(None) => error,
            Err(session_error) => super::support::with_appended_internal_detail(
                error,
                format!("session cleanup after savepoint failure failed: {session_error}"),
            ),
        }
    }

    fn create_subsystem_savepoint_pair(&self, txn_id: TxnId) -> DbResult<(u64, u64)> {
        let storage_savepoint_id = self.storage_txn.create_savepoint(txn_id)?;
        let catalog_savepoint_id = match self.catalog_txn.create_savepoint(txn_id) {
            Ok(savepoint_id) => savepoint_id,
            Err(error) => {
                let mut error = error;
                if let Err(cleanup_error) = self
                    .storage_txn
                    .release_savepoint(txn_id, storage_savepoint_id)
                {
                    error = super::support::with_appended_internal_detail(
                        error,
                        format!(
                            "storage savepoint cleanup after catalog savepoint failure failed: {cleanup_error}"
                        ),
                    );
                }
                return Err(error);
            }
        };
        Ok((storage_savepoint_id, catalog_savepoint_id))
    }

    fn cleanup_subsystem_savepoint_pair(
        &self,
        txn_id: TxnId,
        storage_savepoint_id: u64,
        catalog_savepoint_id: u64,
    ) -> DbResult<()> {
        let mut first_error = None;
        if let Err(error) = self
            .catalog_txn
            .release_savepoint(txn_id, catalog_savepoint_id)
        {
            first_error = Some(error);
        }
        if let Err(error) = self
            .storage_txn
            .release_savepoint(txn_id, storage_savepoint_id)
        {
            first_error.get_or_insert(error);
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    fn autorelease_savepoint_after_rollback_enabled() -> bool {
        std::env::var("AIONDB_SAVEPOINT_AUTORELEASE_AFTER_ROLLBACK")
            .ok()
            .is_some_and(|value| {
                let normalized = value.trim().to_ascii_lowercase();
                matches!(normalized.as_str(), "1" | "true" | "yes" | "on")
            })
    }

    fn is_engine_internal_savepoint(name: &str) -> bool {
        name.starts_with("__aiondb_")
    }

    pub(super) fn create_savepoint(&self, session: &SessionHandle, name: &str) -> DbResult<()> {
        let txn_id = self.with_session(session, |record| {
            record
                .active_txn
                .as_ref()
                .map(|txn| txn.id)
                .ok_or_else(|| Self::no_active_savepoint_transaction_error("SAVEPOINT"))
        })?;

        let (storage_savepoint_id, catalog_savepoint_id) =
            self.create_subsystem_savepoint_pair(txn_id)?;
        let store_result = self.with_session_mut(session, |record| {
            let generation = record.next_savepoint_generation;
            let session_state = record.snapshot_savepoint_state();
            record.next_savepoint_generation = record
                .next_savepoint_generation
                .checked_add(1)
                .ok_or_else(|| DbError::program_limit("too many savepoints"))?;
            record.savepoints.push(crate::session::SavepointEntry {
                name: name.to_owned(),
                generation,
                storage_savepoint_id,
                catalog_savepoint_id,
                session_state,
            });
            Ok(())
        });
        if let Err(error) = store_result {
            let mut error = error;
            if let Err(cleanup_error) = self.cleanup_subsystem_savepoint_pair(
                txn_id,
                storage_savepoint_id,
                catalog_savepoint_id,
            ) {
                error = super::support::with_appended_internal_detail(
                    error,
                    format!("savepoint cleanup after failed session state store failed: {cleanup_error}"),
                );
            }
            return Err(error);
        }
        Ok(())
    }

    pub(super) fn create_unique_savepoint(
        &self,
        session: &SessionHandle,
        name_prefix: &str,
    ) -> DbResult<String> {
        let (txn_id, name) = self.with_session(session, |record| {
            let txn_id = record
                .active_txn
                .as_ref()
                .map(|txn| txn.id)
                .ok_or_else(|| Self::no_active_savepoint_transaction_error("SAVEPOINT"))?;

            let mut next_suffix = 0usize;
            loop {
                let candidate = if next_suffix == 0 {
                    name_prefix.to_owned()
                } else {
                    format!("{name_prefix}#{next_suffix}")
                };
                if !record
                    .savepoints
                    .iter()
                    .any(|entry| entry.name == candidate)
                {
                    break Ok((txn_id, candidate));
                }
                next_suffix = next_suffix
                    .checked_add(1)
                    .ok_or_else(|| DbError::program_limit("too many savepoints"))?;
            }
        })?;

        let (storage_savepoint_id, catalog_savepoint_id) =
            self.create_subsystem_savepoint_pair(txn_id)?;
        let store_result = self.with_session_mut(session, |record| {
            let generation = record.next_savepoint_generation;
            let session_state = record.snapshot_savepoint_state();
            record.next_savepoint_generation = record
                .next_savepoint_generation
                .checked_add(1)
                .ok_or_else(|| DbError::program_limit("too many savepoints"))?;
            record.savepoints.push(crate::session::SavepointEntry {
                name: name.clone(),
                generation,
                storage_savepoint_id,
                catalog_savepoint_id,
                session_state,
            });
            Ok(())
        });
        if let Err(error) = store_result {
            let mut error = error;
            if let Err(cleanup_error) = self.cleanup_subsystem_savepoint_pair(
                txn_id,
                storage_savepoint_id,
                catalog_savepoint_id,
            ) {
                error = super::support::with_appended_internal_detail(
                    error,
                    format!("savepoint cleanup after failed session state store failed: {cleanup_error}"),
                );
            }
            return Err(error);
        }
        Ok(name)
    }

    pub(super) fn rollback_to_savepoint(
        &self,
        session: &SessionHandle,
        name: &str,
    ) -> DbResult<()> {
        let (
            txn_id,
            target_generation,
            storage_savepoint_id,
            catalog_savepoint_id,
            target_session_state,
        ) = self.with_session(session, |record| {
            let txn_id = record
                .active_txn
                .as_ref()
                .map(|txn| txn.id)
                .ok_or_else(|| {
                    Self::no_active_savepoint_transaction_error("ROLLBACK TO SAVEPOINT")
                })?;
            let entry = record
                .savepoints
                .iter()
                .rev()
                .find(|entry| entry.name == name)
                .ok_or_else(|| Self::missing_savepoint_error(name))?;
            Ok((
                txn_id,
                entry.generation,
                entry.storage_savepoint_id,
                entry.catalog_savepoint_id,
                entry.session_state.clone(),
            ))
        })?;

        if let Err(error) = self
            .storage_txn
            .rollback_to_savepoint(txn_id, storage_savepoint_id)
        {
            // Storage rewind failed before catalog: session-record still
            // believes it is at the pre-rollback snapshot. Restore the
            // savepoint's session state BEFORE aborting the whole txn so
            // a subsequent retry observes the correct portal/plan/cache
            // state, not the half-rewound view.
            let _ = self.with_session_mut(session, |record| {
                record.restore_savepoint_state(&target_session_state);
                record.clear_portals_created_since(target_generation);
                record.clear_plan_cache();
                Ok(())
            });
            return Err(self.abort_failed_savepoint_rollback(session, error));
        }
        if let Err(error) = self
            .catalog_txn
            .rollback_to_savepoint(txn_id, catalog_savepoint_id)
        {
            // Storage already rewound but catalog failed: same restore
            // as above plus document the inconsistency in the abort.
            let _ = self.with_session_mut(session, |record| {
                record.restore_savepoint_state(&target_session_state);
                record.clear_portals_created_since(target_generation);
                record.clear_plan_cache();
                Ok(())
            });
            return Err(self.abort_failed_savepoint_rollback(session, error));
        }

        // Keep PostgreSQL semantics for user-visible SAVEPOINTs: after
        // ROLLBACK TO, the named savepoint remains valid until explicitly
        // released. The optional autorelease mode is limited to internal
        // engine savepoints used for implementation details.
        let auto_release_after_rollback = Self::autorelease_savepoint_after_rollback_enabled()
            && Self::is_engine_internal_savepoint(name);
        let released_pair = self.with_session_mut(session, |record| {
            // Remove all savepoints created after this one, but keep the
            // target savepoint itself so it can be rolled back to again.
            // Clear session-local plans because the catalog/storage state may
            // have been rewound to a different transactional snapshot.
            let mut released = None;
            if let Some(pos) = record
                .savepoints
                .iter()
                .rposition(|entry| entry.name == name)
            {
                record.savepoints.truncate(pos + 1);
                if auto_release_after_rollback {
                    if let Some(entry) = record.savepoints.pop() {
                        released = Some((entry.storage_savepoint_id, entry.catalog_savepoint_id));
                    }
                }
            }
            record.restore_savepoint_state(&target_session_state);
            record.clear_portals_created_since(target_generation);
            record.clear_plan_cache();
            Ok(released)
        })?;

        if let Some((storage_savepoint_id, catalog_savepoint_id)) = released_pair {
            self.cleanup_subsystem_savepoint_pair(
                txn_id,
                storage_savepoint_id,
                catalog_savepoint_id,
            )?;
        }

        Ok(())
    }

    pub(super) fn release_savepoint(&self, session: &SessionHandle, name: &str) -> DbResult<()> {
        let (txn_id, storage_savepoint_id, catalog_savepoint_id) =
            self.with_session(session, |record| {
                let txn_id = record
                    .active_txn
                    .as_ref()
                    .map(|txn| txn.id)
                    .ok_or_else(|| {
                        Self::no_active_savepoint_transaction_error("RELEASE SAVEPOINT")
                    })?;
                let entry = record
                    .savepoints
                    .iter()
                    .rev()
                    .find(|entry| entry.name == name)
                    .ok_or_else(|| Self::missing_savepoint_error(name))?;
                Ok((
                    txn_id,
                    entry.storage_savepoint_id,
                    entry.catalog_savepoint_id,
                ))
            })?;

        self.storage_txn
            .release_savepoint(txn_id, storage_savepoint_id)?;
        if let Err(error) = self
            .catalog_txn
            .release_savepoint(txn_id, catalog_savepoint_id)
        {
            let mut error = super::support::with_appended_internal_detail(
                error
                    .with_client_detail("savepoint release may already have partially succeeded")
                    .with_client_hint(
                        "ROLLBACK the transaction if you need a clean savepoint state",
                    ),
                "storage savepoint release succeeded before catalog savepoint release failed",
            );
            if let Err(cleanup_error) = self.with_session_mut(session, |record| {
                if let Some(pos) = record
                    .savepoints
                    .iter()
                    .rposition(|entry| entry.name == name)
                {
                    record.savepoints.truncate(pos);
                }
                Ok(())
            }) {
                error = super::support::with_appended_internal_detail(
                    error,
                    format!(
                        "session savepoint cleanup after partial release failure failed: {cleanup_error}"
                    ),
                );
            }
            return Err(error);
        }

        self.with_session_mut(session, |record| {
            // Remove the named savepoint and all savepoints created after it.
            if let Some(pos) = record
                .savepoints
                .iter()
                .rposition(|entry| entry.name == name)
            {
                record.savepoints.truncate(pos);
            }
            Ok(())
        })
    }
}
