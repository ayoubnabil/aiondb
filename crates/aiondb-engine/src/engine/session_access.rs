#![allow(
    clippy::doc_markdown,
    clippy::match_same_arms,
    clippy::too_many_lines,
    clippy::uninlined_format_args,
    clippy::wildcard_imports
)]

use super::*;
use std::sync::Arc;
use std::time::Instant;

use crate::engine::session_lifecycle::SessionPolicyEnforcement;

pub(super) struct CachedSqlLookup {
    pub(super) entry: crate::session::ParsedSqlCacheEntry,
    pub(super) matched_shape: bool,
}

impl Engine {
    pub(super) fn plan_cache_txn_id_for_record(&self, record: &SessionRecord) -> DbResult<TxnId> {
        if record.implicit_txn_active {
            return Ok(TxnId::default());
        }
        let Some(txn) = record.active_txn.as_ref() else {
            return Ok(TxnId::default());
        };
        if self.catalog_txn.txn_writes_catalog(txn.id)? {
            Ok(txn.id)
        } else {
            Ok(TxnId::default())
        }
    }

    pub(super) fn consume_cancellation_if_needed(record: &mut SessionRecord) -> DbResult<()> {
        if record.cancel_requested {
            record.cancel_requested = false;
            return Err(DbError::query_canceled("session canceled"));
        }
        Ok(())
    }

    /// Complete a policy-triggered rollback after the session lock has been
    /// released.  Always returns an error (the policy violation).
    fn complete_policy_rollback(
        &self,
        error: DbError,
        txn: aiondb_tx::ActiveTransaction,
        include_catalog_participant: bool,
        include_storage_participant: bool,
    ) -> DbError {
        if let Err(rollback_error) = self.rollback_active_transaction(
            txn,
            include_catalog_participant,
            include_storage_participant,
        ) {
            // Preserve the original error's SqlState (e.g. 25006 read-only,
            // 57014 query-canceled). Wrapping in `DbError::internal` would
            // erase it and clients branching on SqlState would misclassify.
            return super::support::with_appended_internal_detail(
                error,
                format!("rollback also failed: {rollback_error}"),
            );
        }
        error
    }

    pub(super) fn with_session<T>(
        &self,
        handle: &SessionHandle,
        apply: impl FnOnce(&SessionRecord) -> DbResult<T>,
    ) -> DbResult<T> {
        let session = self.session_entry(handle)?;
        let timed_out_txn = {
            let mut record = Self::acquire_session_lock_with_metrics(&session, &self.metrics)?;
            match self.enforce_session_policies(&mut record) {
                SessionPolicyEnforcement::Allowed => return apply(&record),
                SessionPolicyEnforcement::Rejected(error) => return Err(error),
                SessionPolicyEnforcement::RollbackAndReject {
                    error,
                    txn,
                    include_catalog_participant,
                    include_storage_participant,
                } => (
                    error,
                    txn,
                    include_catalog_participant,
                    include_storage_participant,
                ),
            }
        };
        Err(self.complete_policy_rollback(
            timed_out_txn.0,
            timed_out_txn.1,
            timed_out_txn.2,
            timed_out_txn.3,
        ))
    }

    pub(super) fn with_session_mut<T>(
        &self,
        handle: &SessionHandle,
        apply: impl FnOnce(&mut SessionRecord) -> DbResult<T>,
    ) -> DbResult<T> {
        let session = self.session_entry(handle)?;
        let timed_out_txn = {
            let mut record = Self::acquire_session_lock_with_metrics(&session, &self.metrics)?;
            match self.enforce_session_policies(&mut record) {
                SessionPolicyEnforcement::Allowed => {
                    record.last_active = Instant::now();
                    return apply(&mut record);
                }
                SessionPolicyEnforcement::Rejected(error) => return Err(error),
                SessionPolicyEnforcement::RollbackAndReject {
                    error,
                    txn,
                    include_catalog_participant,
                    include_storage_participant,
                } => (
                    error,
                    txn,
                    include_catalog_participant,
                    include_storage_participant,
                ),
            }
        };
        Err(self.complete_policy_rollback(
            timed_out_txn.0,
            timed_out_txn.1,
            timed_out_txn.2,
            timed_out_txn.3,
        ))
    }

    /// Acquire the session lock and report wait time to metrics, but skip
    /// the `Instant::now()` + `record_session_lock_wait` round-trip when
    /// the lock can be acquired without contention. On the OLTP hot path
    /// every `with_session*` call goes through here, so the per-call
    /// elapsed-time measurement was visible (~50-100ns plus three atomic
    /// adds, each multiplied by ~20 calls per query).
    fn acquire_session_lock_with_metrics<'a>(
        session: &'a Arc<Mutex<SessionRecord>>,
        metrics: &super::metrics::EngineMetrics,
    ) -> DbResult<MutexGuard<'a, SessionRecord>> {
        if let Ok(record) = session.try_lock() {
            // Uncontested fast path: nothing waited, nothing to record.
            return Ok(record);
        }
        let lock_start = Instant::now();
        let record = Self::lock_session(session)?;
        let waited_micros = u64::try_from(lock_start.elapsed().as_micros()).unwrap_or(u64::MAX);
        metrics.record_session_lock_wait(waited_micros);
        Ok(record)
    }

    /// Drain all pending notices from the session (returns and clears them).
    /// Extract the active transaction from a session, clearing transaction
    /// state.  Returns `Ok(None)` when no transaction is active (PG-compatible
    pub(super) fn take_session_txn(
        &self,
        session: &SessionHandle,
    ) -> DbResult<Option<aiondb_tx::ActiveTransaction>> {
        self.with_session_mut(session, |record| {
            let Some(txn) = record.active_txn.take() else {
                return Ok(None);
            };
            record.active_txn_includes_catalog_participant = false;
            record.active_txn_includes_storage_participant = false;
            let implicit_txn_active = record.implicit_txn_active;
            record.txn_started_at = None;
            record.savepoints.clear();
            record.clear_transaction_local_state();
            aiondb_eval::plpgsql_clear_compat_cursors();
            if implicit_txn_active {
                // Extended-protocol portals must survive across suspended
                // batches in autocommit mode, but SQL compat cursors remain
                // transaction-scoped and should still be cleaned up.
                record.clear_compat_cursor_portals();
            } else {
                record.clear_transaction_scoped_portals();
            }
            Ok(Some(txn))
        })
    }

    pub(super) fn take_session_txn_with_participants(
        &self,
        session: &SessionHandle,
    ) -> DbResult<Option<(aiondb_tx::ActiveTransaction, bool, bool)>> {
        self.with_session_mut(session, |record| {
            let Some(txn) = record.active_txn.take() else {
                return Ok(None);
            };
            let include_catalog_participant = record.active_txn_includes_catalog_participant;
            let include_storage_participant = record.active_txn_includes_storage_participant;
            record.active_txn_includes_catalog_participant = false;
            record.active_txn_includes_storage_participant = false;
            let implicit_txn_active = record.implicit_txn_active;
            record.txn_started_at = None;
            record.savepoints.clear();
            record.clear_transaction_local_state();
            aiondb_eval::plpgsql_clear_compat_cursors();
            if implicit_txn_active {
                record.clear_compat_cursor_portals();
            } else {
                record.clear_transaction_scoped_portals();
            }
            Ok(Some((
                txn,
                include_catalog_participant,
                include_storage_participant,
            )))
        })
    }

    pub(super) fn drain_pending_notices(&self, handle: &SessionHandle) -> DbResult<Vec<String>> {
        self.with_session_mut(handle, |record| {
            Ok(std::mem::take(&mut record.pending_notices))
        })
    }

    pub(super) fn clear_session_sequence_state(&self, handle: &SessionHandle) -> DbResult<()> {
        self.with_session_mut(handle, |record| {
            let mut state = record
                .sequence_state
                .lock()
                .map_err(|e| DbError::internal(format!("sequence session state poisoned: {e}")))?;
            state.clear();
            Ok(())
        })
    }

    /// Execute `COMMENT ON object_type name IS { 'text' | NULL }`.
    /// Comments are mirrored into the compatibility catalog; `IS NULL`
    /// removes the comment.
    pub(super) fn execute_comment_on(
        &self,
        session: &SessionHandle,
        stmt: &aiondb_parser::CommentStatement,
    ) -> DbResult<crate::prepared::StatementResult> {
        if self.authorizer_is_noop {
            let session_info = self.session_info(session)?;
            if crate::catalog_authorizer::catalog_has_any_roles(self.catalog_reader.as_ref())?
                && !crate::catalog_authorizer::is_superuser_checked(
                    self.catalog_reader.as_ref(),
                    &session_info.identity,
                )?
            {
                return Err(DbError::insufficient_privilege(
                    "must be superuser to COMMENT ON through this interface",
                ));
            }
        }

        fn join_object_name(parts: &[String]) -> String {
            parts.join(".")
        }

        let subject = match &stmt.subject {
            aiondb_parser::CommentSubject::Simple(name) => name.to_ascii_lowercase(),
            aiondb_parser::CommentSubject::Qualified(obj) => obj
                .parts
                .iter()
                .map(|part| part.to_ascii_lowercase())
                .collect::<Vec<_>>()
                .join("."),
        };
        let mut key = (stmt.object_type.clone(), subject);
        let text = stmt.text.clone();
        let object_type_upper = stmt.object_type.to_ascii_uppercase();
        if !matches!(
            object_type_upper.as_str(),
            "TABLE"
                | "VIEW"
                | "MATERIALIZED VIEW"
                | "INDEX"
                | "SEQUENCE"
                | "FOREIGN TABLE"
                | "COLUMN"
                | "CONSTRAINT"
                | "TRIGGER"
                | "STATISTICS"
                | "ROLE"
                | "DATABASE"
                | "SCHEMA"
        ) {
            return Err(DbError::feature_not_supported(format!(
                "COMMENT ON {} is not supported",
                stmt.object_type
            )));
        }
        let subject_display_name = match &stmt.subject {
            aiondb_parser::CommentSubject::Simple(name) => name.clone(),
            aiondb_parser::CommentSubject::Qualified(obj) => obj
                .parts
                .last()
                .cloned()
                .unwrap_or_else(|| obj.parts.join(".")),
        };
        if object_type_upper == "ROLE" {
            let txn_id = self.with_session(session, |record| {
                Ok(record.active_txn.as_ref().map(|t| t.id).unwrap_or_default())
            })?;
            if self
                .catalog_reader
                .get_role(txn_id, &subject_display_name)?
                .is_none()
            {
                return Err(DbError::bind_error(
                    aiondb_core::SqlState::UndefinedObject,
                    format!("role \"{subject_display_name}\" does not exist"),
                ));
            }
        }
        if object_type_upper == "DATABASE"
            && self
                .cluster_catalog
                .get_database_by_name(&subject_display_name)?
                .is_none()
        {
            return Err(DbError::bind_error(
                aiondb_core::SqlState::InvalidCatalogName,
                format!("database \"{subject_display_name}\" does not exist"),
            ));
        }
        if object_type_upper == "SCHEMA" {
            let txn_id = self.with_session(session, |record| {
                Ok(record.active_txn.as_ref().map(|t| t.id).unwrap_or_default())
            })?;
            let exists = self
                .catalog_reader
                .get_schema(
                    txn_id,
                    &aiondb_catalog::QualifiedName::unqualified(&subject_display_name),
                )?
                .is_some();
            if !exists {
                return Err(DbError::bind_error(
                    aiondb_core::SqlState::InvalidSchemaName,
                    format!("schema \"{subject_display_name}\" does not exist"),
                ));
            }
        }
        if stmt.object_type.eq_ignore_ascii_case("STATISTICS") {
            let stats_key = ("CREATE STATISTICS".to_owned(), key.1.clone());
            let (exists, owner) = self.with_session(session, |record| {
                Ok((
                    record.compat_misc_objects.contains_key(&stats_key),
                    record
                        .compat_misc_attrs
                        .get(&stats_key)
                        .and_then(|attrs| attrs.owner.clone())
                        .unwrap_or_default(),
                ))
            })?;
            if !exists {
                return Err(DbError::bind_error(
                    aiondb_core::SqlState::UndefinedObject,
                    format!("statistics object \"{}\" does not exist", key.1),
                ));
            }
            let current_user = self.with_session(session, |record| {
                Ok(super::session_vars::current_user_for_record(record).to_ascii_lowercase())
            })?;
            if !owner.is_empty() && !owner.eq_ignore_ascii_case(&current_user) {
                return Err(DbError::bind_error(
                    aiondb_core::SqlState::InsufficientPrivilege,
                    format!("must be owner of statistics object {}", key.1),
                ));
            }
        }
        if stmt.object_type.eq_ignore_ascii_case("TRIGGER") {
            if let aiondb_parser::CommentSubject::Qualified(obj) = &stmt.subject {
                if obj.parts.len() >= 2 {
                    let trig_name = obj.parts.last().cloned().unwrap_or_default();
                    let table_parts = &obj.parts[..obj.parts.len() - 1];
                    let display_table = table_parts.last().cloned().unwrap_or_default();
                    let txn_id = self.with_session(session, |record| {
                        Ok(record.active_txn.as_ref().map(|t| t.id).unwrap_or_default())
                    })?;
                    let triggers = self.catalog_reader.list_triggers(txn_id, &display_table)?;
                    let exists = triggers
                        .iter()
                        .any(|t| t.name.eq_ignore_ascii_case(&trig_name));
                    if !exists {
                        return Err(DbError::bind_error(
                            aiondb_core::SqlState::UndefinedObject,
                            format!(
                                "trigger \"{trig_name}\" for table \"{display_table}\" does not exist"
                            ),
                        ));
                    }
                }
            }
        }
        if matches!(
            stmt.object_type.to_ascii_uppercase().as_str(),
            "TABLE" | "VIEW" | "MATERIALIZED VIEW" | "INDEX" | "SEQUENCE" | "FOREIGN TABLE"
        ) {
            if let aiondb_parser::CommentSubject::Qualified(obj) = &stmt.subject {
                let display_name = join_object_name(&obj.parts);
                let txn_id = self.with_session(session, |record| {
                    Ok(record.active_txn.as_ref().map(|t| t.id).unwrap_or_default())
                })?;
                let kind_upper = stmt.object_type.to_ascii_uppercase();
                let qname = aiondb_catalog::QualifiedName::parse(&display_name);
                let table_exists = self.catalog_reader.get_table(txn_id, &qname)?.is_some();
                let view_desc = if kind_upper == "VIEW" {
                    self.catalog_reader.get_view(txn_id, &qname)?
                } else {
                    None
                };
                let sequence_desc = if kind_upper == "SEQUENCE" {
                    self.catalog_reader.get_sequence(txn_id, &qname)?
                } else {
                    None
                };
                let foreign_table_exists = kind_upper == "FOREIGN TABLE"
                    && self.with_session(session, |record| {
                        let key = (
                            "CREATE FOREIGN TABLE".to_owned(),
                            display_name.to_ascii_lowercase(),
                        );
                        Ok(record.compat_misc_objects.contains_key(&key))
                    })?;
                let index_oid = if kind_upper == "INDEX" {
                    let explicit_schema = (obj.parts.len() >= 2)
                        .then(|| obj.parts[obj.parts.len() - 2].to_ascii_lowercase());
                    let search_path = if explicit_schema.is_none() {
                        self.with_session(session, |record| {
                            self::session_vars::effective_search_path_schemas_for_record(
                                self.catalog_reader.as_ref(),
                                txn_id,
                                record,
                            )
                        })?
                    } else {
                        Vec::new()
                    };
                    let mut found = None;
                    'schemas: for schema in self.catalog_reader.list_schemas(txn_id)? {
                        for table in self.catalog_reader.list_tables(txn_id, schema.schema_id)? {
                            for index in self.catalog_reader.list_indexes(txn_id, table.table_id)? {
                                if !index.name.name.eq_ignore_ascii_case(&display_name) {
                                    continue;
                                }
                                let schema_matches =
                                    if let Some(schema) = explicit_schema.as_deref() {
                                        index
                                            .name
                                            .schema
                                            .as_deref()
                                            .is_some_and(|s| s.eq_ignore_ascii_case(schema))
                                    } else {
                                        index.name.schema.as_deref().map_or(true, |schema| {
                                            search_path
                                                .iter()
                                                .any(|entry| entry.eq_ignore_ascii_case(schema))
                                        })
                                    };
                                if schema_matches {
                                    found = Some(
                                        index.index_id.get().saturating_add(32_768).to_string(),
                                    );
                                    break 'schemas;
                                }
                            }
                        }
                    }
                    found
                } else {
                    None
                };
                let exists = match kind_upper.as_str() {
                    "INDEX" => index_oid.is_some(),
                    "VIEW" => view_desc.is_some(),
                    "SEQUENCE" => sequence_desc.is_some(),
                    _ => table_exists || foreign_table_exists,
                };
                if !exists {
                    if kind_upper == "INDEX" {
                        return Err(DbError::bind_error(
                            aiondb_core::SqlState::UndefinedObject,
                            format!("index \"{display_name}\" does not exist"),
                        ));
                    }
                    let kind_word = match kind_upper.as_str() {
                        "TABLE" | "FOREIGN TABLE" => "relation",
                        "VIEW" | "MATERIALIZED VIEW" => "relation",
                        "INDEX" => "relation",
                        "SEQUENCE" => "relation",
                        _ => "relation",
                    };
                    return Err(DbError::bind_error(
                        aiondb_core::SqlState::UndefinedTable,
                        format!("{kind_word} \"{display_name}\" does not exist"),
                    ));
                }
                match kind_upper.as_str() {
                    "INDEX" => {
                        if let Some(index_oid) = index_oid {
                            key.1 = index_oid;
                        }
                    }
                    "VIEW" => {
                        if let Some(view_desc) = view_desc {
                            key.1 = view_desc.view_id.get().saturating_add(16_384).to_string();
                        }
                    }
                    "SEQUENCE" => {
                        if let Some(sequence_desc) = sequence_desc {
                            key.1 = sequence_desc
                                .sequence_id
                                .get()
                                .saturating_add(49_152)
                                .to_string();
                        }
                    }
                    _ => {}
                }
            }
        }
        if matches!(
            object_type_upper.as_str(),
            "TABLE" | "MATERIALIZED VIEW" | "FOREIGN TABLE"
        ) {
            let txn_id = self.with_session(session, |record| {
                Ok(record.active_txn.as_ref().map(|t| t.id).unwrap_or_default())
            })?;
            if let Some(table_desc) = self.resolve_compat_table_name(session, txn_id, &key.1)? {
                key.1 = table_desc.table_id.get().saturating_add(16_384).to_string();
            }
        }
        if stmt.object_type.eq_ignore_ascii_case("COLUMN") {
            if let aiondb_parser::CommentSubject::Qualified(obj) = &stmt.subject {
                if obj.parts.len() >= 2 {
                    let col_name = obj.parts.last().cloned().unwrap_or_default();
                    let table_parts = &obj.parts[..obj.parts.len() - 1];
                    let display_table = join_object_name(table_parts);
                    let txn_id = self.with_session(session, |record| {
                        Ok(record.active_txn.as_ref().map(|t| t.id).unwrap_or_default())
                    })?;
                    let table_qname = aiondb_catalog::QualifiedName::parse(&display_table);
                    let foreign_table_exists = self.with_session(session, |record| {
                        let key = (
                            "CREATE FOREIGN TABLE".to_owned(),
                            display_table.to_ascii_lowercase(),
                        );
                        Ok(record.compat_misc_objects.contains_key(&key))
                    })?;
                    if foreign_table_exists
                        && self
                            .catalog_reader
                            .get_table(txn_id, &table_qname)?
                            .is_none()
                    {
                        return Err(DbError::feature_not_supported(
                            "COMMENT ON COLUMN for compatibility foreign tables is not supported",
                        ));
                    }
                    if let Some(table_desc) = self.catalog_reader.get_table(txn_id, &table_qname)? {
                        let column = table_desc
                            .columns
                            .iter()
                            .find(|c| c.name.eq_ignore_ascii_case(&col_name));
                        if column.is_none() {
                            return Err(DbError::bind_error(
                                aiondb_core::SqlState::UndefinedColumn,
                                format!(
                                    "column \"{col_name}\" of relation \"{}\" does not exist",
                                    table_desc.name.object_name()
                                ),
                            ));
                        }
                        if let Some(column) = column {
                            key.1 = format!(
                                "{}.{}",
                                table_desc.table_id.get().saturating_add(16_384),
                                column.ordinal_position
                            );
                        }
                    } else {
                        return Err(DbError::bind_error(
                            aiondb_core::SqlState::UndefinedTable,
                            format!("relation \"{display_table}\" does not exist"),
                        ));
                    }
                }
            }
        }
        if stmt.object_type.eq_ignore_ascii_case("CONSTRAINT") {
            if let aiondb_parser::CommentSubject::Qualified(obj) = &stmt.subject {
                if obj.parts.len() >= 2 {
                    let constraint_name = obj.parts.last().cloned().unwrap_or_default();
                    let table_parts = &obj.parts[..obj.parts.len() - 1];
                    let display_table = join_object_name(table_parts);
                    let txn_id = self.with_session(session, |record| {
                        Ok(record.active_txn.as_ref().map(|t| t.id).unwrap_or_default())
                    })?;
                    let table_qname = aiondb_catalog::QualifiedName::parse(&display_table);
                    if let Some(table_desc) = self.catalog_reader.get_table(txn_id, &table_qname)? {
                        let table_name_lower = table_desc.name.object_name().to_ascii_lowercase();
                        let mut found = false;
                        for fk in &table_desc.foreign_keys {
                            if fk
                                .effective_name(&table_name_lower)
                                .eq_ignore_ascii_case(&constraint_name)
                            {
                                found = true;
                                break;
                            }
                        }
                        if !found {
                            for ck in &table_desc.check_constraints {
                                if let Some(name) = &ck.name {
                                    if name.eq_ignore_ascii_case(&constraint_name) {
                                        found = true;
                                        break;
                                    }
                                }
                            }
                        }
                        if !found {
                            let pkey_default = format!("{}_pkey", table_name_lower);
                            if pkey_default.eq_ignore_ascii_case(&constraint_name)
                                && table_desc.primary_key.is_some()
                            {
                                found = true;
                            }
                        }
                        if !found {
                            for idx in self
                                .catalog_reader
                                .list_indexes(txn_id, table_desc.table_id)?
                            {
                                if idx.name.name.eq_ignore_ascii_case(&constraint_name) {
                                    found = true;
                                    break;
                                }
                            }
                        }
                        if !found {
                            return Err(DbError::bind_error(
                                aiondb_core::SqlState::UndefinedObject,
                                format!(
                                    "constraint \"{constraint_name}\" for table \"{table_name_lower}\" does not exist"
                                ),
                            ));
                        }
                    } else {
                        return Err(DbError::bind_error(
                            aiondb_core::SqlState::UndefinedTable,
                            format!("relation \"{display_table}\" does not exist"),
                        ));
                    }
                }
            }
        }
        let persisted_comment = text.clone();
        let persisted_key = (key.0.to_ascii_uppercase(), key.1.clone());
        let txn_id = self.current_txn_id(session)?;
        if let Some(comment) = persisted_comment.clone() {
            self.catalog_writer.set_comment(
                txn_id,
                aiondb_catalog::CommentDescriptor {
                    object_type: persisted_key.0.clone(),
                    object_identity: persisted_key.1.clone(),
                    comment,
                },
            )?;
        } else {
            self.catalog_writer
                .drop_comment(txn_id, &persisted_key.0, &persisted_key.1)?;
        }
        let shared_key = persisted_key.clone();
        self.with_session_mut(session, |record| {
            if let (Some(existing), Some(new_text)) = (record.comments.get(&key), text.as_ref()) {
                if existing == new_text {
                    return Ok(());
                }
            }
            if let Some(text) = text {
                record.comments.insert(key, text);
            } else {
                record.comments.remove(&key);
            }
            Ok(())
        })?;
        if let Ok(mut shared_comments) = self.compat_global_comments.lock() {
            if let Some(text) = persisted_comment {
                shared_comments.insert(shared_key, text);
            } else {
                shared_comments.remove(&shared_key);
            }
        }
        Ok(super::support::command_ok("COMMENT"))
    }

    /// Execute `SECURITY LABEL [FOR provider] ON object_type name IS
    /// { 'label' | NULL }`. Labels persist per session (AionDB has no
    /// durable `pg_seclabel` catalog yet); `IS NULL` removes the label.
    pub(super) fn execute_security_label(
        &self,
        session: &SessionHandle,
        stmt: &aiondb_parser::SecurityLabelStatement,
    ) -> DbResult<crate::prepared::StatementResult> {
        if self.authorizer_is_noop {
            let session_info = self.session_info(session)?;
            if crate::catalog_authorizer::catalog_has_any_roles(self.catalog_reader.as_ref())?
                && !crate::catalog_authorizer::is_superuser_checked(
                    self.catalog_reader.as_ref(),
                    &session_info.identity,
                )?
            {
                return Err(DbError::insufficient_privilege(
                    "must be superuser to SECURITY LABEL through this interface",
                ));
            }
        }

        let object_type_upper = stmt.object_type.to_ascii_uppercase();
        if !matches!(
            object_type_upper.as_str(),
            "TABLE"
                | "VIEW"
                | "MATERIALIZED VIEW"
                | "INDEX"
                | "SEQUENCE"
                | "FOREIGN TABLE"
                | "ROLE"
                | "DATABASE"
                | "SCHEMA"
        ) {
            return Err(DbError::feature_not_supported(format!(
                "SECURITY LABEL ON {} is not supported",
                stmt.object_type
            )));
        }
        let subject_display_name = match &stmt.subject {
            aiondb_parser::SecurityLabelSubject::Simple(name) => name.clone(),
            aiondb_parser::SecurityLabelSubject::Qualified(obj) => obj
                .parts
                .last()
                .cloned()
                .unwrap_or_else(|| obj.parts.join(".")),
        };
        if object_type_upper == "ROLE" {
            let txn_id = self.with_session(session, |record| {
                Ok(record.active_txn.as_ref().map(|t| t.id).unwrap_or_default())
            })?;
            if self
                .catalog_reader
                .get_role(txn_id, &subject_display_name)?
                .is_none()
            {
                return Err(DbError::bind_error(
                    aiondb_core::SqlState::UndefinedObject,
                    format!("role \"{subject_display_name}\" does not exist"),
                ));
            }
        }
        if object_type_upper == "DATABASE"
            && self
                .cluster_catalog
                .get_database_by_name(&subject_display_name)?
                .is_none()
        {
            return Err(DbError::bind_error(
                aiondb_core::SqlState::InvalidCatalogName,
                format!("database \"{subject_display_name}\" does not exist"),
            ));
        }
        if matches!(
            object_type_upper.as_str(),
            "TABLE" | "VIEW" | "MATERIALIZED VIEW" | "SEQUENCE" | "FOREIGN TABLE" | "INDEX"
        ) {
            if let aiondb_parser::SecurityLabelSubject::Qualified(obj) = &stmt.subject {
                let display_name = obj
                    .parts
                    .last()
                    .cloned()
                    .unwrap_or_else(|| obj.parts.join("."));
                let txn_id = self.with_session(session, |record| {
                    Ok(record.active_txn.as_ref().map(|t| t.id).unwrap_or_default())
                })?;
                if object_type_upper == "INDEX" {
                    let explicit_schema = (obj.parts.len() >= 2)
                        .then(|| obj.parts[obj.parts.len() - 2].to_ascii_lowercase());
                    let search_path = if explicit_schema.is_none() {
                        self.with_session(session, |record| {
                            self::session_vars::effective_search_path_schemas_for_record(
                                self.catalog_reader.as_ref(),
                                txn_id,
                                record,
                            )
                        })?
                    } else {
                        Vec::new()
                    };
                    let mut index_exists = false;
                    'schemas: for schema in self.catalog_reader.list_schemas(txn_id)? {
                        for table in self.catalog_reader.list_tables(txn_id, schema.schema_id)? {
                            for index in self.catalog_reader.list_indexes(txn_id, table.table_id)? {
                                if !index.name.name.eq_ignore_ascii_case(&display_name) {
                                    continue;
                                }
                                let schema_matches =
                                    if let Some(schema) = explicit_schema.as_deref() {
                                        index
                                            .name
                                            .schema
                                            .as_deref()
                                            .is_some_and(|s| s.eq_ignore_ascii_case(schema))
                                    } else {
                                        index.name.schema.as_deref().map_or(true, |schema| {
                                            search_path
                                                .iter()
                                                .any(|entry| entry.eq_ignore_ascii_case(schema))
                                        })
                                    };
                                if schema_matches {
                                    index_exists = true;
                                    break 'schemas;
                                }
                            }
                        }
                    }
                    if !index_exists {
                        return Err(DbError::bind_error(
                            aiondb_core::SqlState::UndefinedObject,
                            format!("index \"{display_name}\" does not exist"),
                        ));
                    }
                    // Index existence fully handled; no table-style check needed.
                    let subject = match &stmt.subject {
                        aiondb_parser::SecurityLabelSubject::Simple(name) => {
                            name.to_ascii_lowercase()
                        }
                        aiondb_parser::SecurityLabelSubject::Qualified(obj) => obj
                            .parts
                            .iter()
                            .map(|part| part.to_ascii_lowercase())
                            .collect::<Vec<_>>()
                            .join("."),
                    };
                    let key = (stmt.object_type.clone(), subject);
                    let provider = stmt.provider.clone();
                    let label = stmt.label.clone();
                    self.with_session_mut(session, |record| {
                        if let Some(label) = label {
                            record.security_labels.insert(key, (provider, label));
                        } else {
                            record.security_labels.remove(&key);
                        }
                        Ok(())
                    })?;
                    return Ok(super::support::command_ok("SECURITY LABEL"));
                }
                let qname = aiondb_catalog::QualifiedName::parse(&display_name);
                let table_exists = self.catalog_reader.get_table(txn_id, &qname)?.is_some();
                let foreign_table_exists = object_type_upper == "FOREIGN TABLE"
                    && self.with_session(session, |record| {
                        let key = (
                            "CREATE FOREIGN TABLE".to_owned(),
                            display_name.to_ascii_lowercase(),
                        );
                        Ok(record.compat_misc_objects.contains_key(&key))
                    })?;
                if !table_exists && !foreign_table_exists {
                    return Err(DbError::bind_error(
                        aiondb_core::SqlState::UndefinedTable,
                        format!("relation \"{display_name}\" does not exist"),
                    ));
                }
            }
        }
        let subject = match &stmt.subject {
            aiondb_parser::SecurityLabelSubject::Simple(name) => name.to_ascii_lowercase(),
            aiondb_parser::SecurityLabelSubject::Qualified(obj) => obj
                .parts
                .iter()
                .map(|part| part.to_ascii_lowercase())
                .collect::<Vec<_>>()
                .join("."),
        };
        let key = (stmt.object_type.clone(), subject);
        let provider = stmt.provider.clone();
        let label = stmt.label.clone();
        self.with_session_mut(session, |record| {
            if let Some(label) = label {
                record.security_labels.insert(key, (provider, label));
            } else {
                record.security_labels.remove(&key);
            }
            Ok(())
        })?;
        Ok(super::support::command_ok("SECURITY LABEL"))
    }

    /// Dispatch a typed `CREATE/ALTER/DROP DATABASE` statement through
    /// the shared `execute_database_command` path. The helper
    /// reconstructs a minimal SQL fragment from the typed variant so
    /// the existing option-tail parser (`parse_compat_database_command`)
    /// still works in the extended-query path where the original SQL
    /// may not be preserved past binding.
    pub(super) fn execute_typed_database_statement(
        &self,
        session: &SessionHandle,
        statement: &aiondb_parser::Statement,
    ) -> DbResult<crate::prepared::StatementResult> {
        use aiondb_parser::Statement;
        let synthesized_sql: String = match statement {
            Statement::CreateDatabase(inner) => format!("CREATE DATABASE {}", inner.name),
            Statement::AlterDatabase(inner) => format!("ALTER DATABASE {}", inner.name),
            Statement::DropDatabase(inner) => {
                if inner.if_exists {
                    format!("DROP DATABASE IF EXISTS {}", inner.name)
                } else {
                    format!("DROP DATABASE {}", inner.name)
                }
            }
            _ => {
                return Err(DbError::internal(
                    "execute_typed_database_statement called on non-database statement",
                ))
            }
        };
        let results = self
            .execute_database_command(session, &synthesized_sql, statement)?
            .ok_or_else(|| {
                DbError::internal("typed DATABASE statement produced no result (should not happen)")
            })?;
        // Queue any accompanying notices on the session, then return
        // the terminal `command_ok`. The caller's single-statement
        // contract requires one result; notices flow through the
        // normal pending-notice drain.
        let mut terminal: Option<crate::prepared::StatementResult> = None;
        for result in results {
            match result {
                crate::prepared::StatementResult::Notice { message } => {
                    let _ = self.with_session_mut(session, |record| {
                        record.push_notice(message);
                        Ok(())
                    });
                }
                other => terminal = Some(other),
            }
        }
        terminal.ok_or_else(|| {
            DbError::internal("typed DATABASE statement emitted only notices, no command tag")
        })
    }

    /// Dispatch a typed TYPE / DOMAIN / CAST / RULE /
    /// CREATE-OR-REPLACE family statement through the compat router's
    /// typed-compat dispatch path. Called from the prepared / portal path
    /// where the typed variant reaches `execute_statement_inner`
    /// without having gone through `run_compat_router` first.
    pub(super) fn execute_typed_compat_family_statement(
        &self,
        session: &SessionHandle,
        statement: &aiondb_parser::Statement,
    ) -> DbResult<crate::prepared::StatementResult> {
        use aiondb_parser::Statement;
        // Each typed compat variant carries the exact raw SQL slice
        // extracted at parse time so the compat handler can re-parse
        // options with full fidelity; the synthesized-name approach
        // used for DATABASE would lose `WITH FUNCTION …`, `AS ENUM …`,
        // `DO …`, etc.
        let synthesized_sql: String = match statement {
            Statement::CreateType(s)
            | Statement::AlterType(s)
            | Statement::CreateDomain(s)
            | Statement::AlterDomain(s) => s.raw_sql.clone(),
            Statement::DropDomain(s) => s.raw_sql.clone(),
            Statement::CreateCast(s) | Statement::DropCast(s) => s.raw_sql.clone(),
            Statement::CreateOrReplaceCompat(s) => s.raw_sql.clone(),
            Statement::CreateAggregate(s)
            | Statement::DropAggregate(s)
            | Statement::CreateProcedure(s)
            | Statement::DropProcedure(s)
            | Statement::DropRoutine(s)
            | Statement::AlterTriggerCompat(s)
            | Statement::CreateStatistics(s)
            | Statement::AlterStatistics(s)
            | Statement::DropStatistics(s)
            | Statement::CreateOperator(s)
            | Statement::DropOperator(s) => s.raw_sql.clone(),
            _ => {
                return Err(DbError::internal(
                    "execute_typed_compat_family_statement called on non-compat statement",
                ))
            }
        };
        let results = match self.run_compat_router(
            session,
            &synthesized_sql,
            &synthesized_sql,
            statement,
            true,
            aiondb_pg_compat::disposition::classify(statement),
        )? {
            crate::engine::compat::router_helpers::CompatHandlerPlan::Handled(results) => results,
            crate::engine::compat::router_helpers::CompatHandlerPlan::Unhandled => {
                return Err(DbError::feature_not_supported(format!(
                    "unsupported compatibility command: {}",
                    statement.compat_tag().unwrap_or("UNKNOWN")
                )));
            }
        };
        let mut terminal: Option<crate::prepared::StatementResult> = None;
        for result in results {
            match result {
                crate::prepared::StatementResult::Notice { message } => {
                    let _ = self.with_session_mut(session, |record| {
                        record.push_notice(message);
                        Ok(())
                    });
                }
                other => terminal = Some(other),
            }
        }
        terminal.ok_or_else(|| {
            DbError::internal("typed compat-family statement emitted only notices, no command tag")
        })
    }

    /// Execute `DISCARD { ALL | PLANS | TEMP | TEMPORARY | SEQUENCES }` as
    pub(super) fn execute_discard(
        &self,
        session: &SessionHandle,
        stmt: &aiondb_parser::DiscardStatement,
    ) -> DbResult<crate::prepared::StatementResult> {
        use aiondb_parser::DiscardTarget;
        match stmt.target {
            DiscardTarget::Sequences => {
                self.clear_session_sequence_state(session)?;
            }
            DiscardTarget::Plans => {
                self.with_session_mut(session, |record| {
                    record.clear_plan_cache();
                    Ok(())
                })?;
            }
            DiscardTarget::Temporary => {
                return Err(DbError::bind_error(
                    aiondb_core::SqlState::FeatureNotSupported,
                    "DISCARD TEMP is not supported",
                ));
            }
            DiscardTarget::All => {
                return Err(DbError::bind_error(
                    aiondb_core::SqlState::FeatureNotSupported,
                    "DISCARD ALL is not supported",
                ));
            }
        }
        Ok(super::support::command_ok("DISCARD"))
    }

    pub(super) fn take_cancellation_if_needed(&self, session: &SessionHandle) -> DbResult<()> {
        self.with_session_mut(session, Self::consume_cancellation_if_needed)
    }

    pub(super) fn take_cancellation_and_cached_sql_with_shape(
        &self,
        session: &SessionHandle,
        sql: &str,
        shape_sql: Option<&str>,
    ) -> DbResult<(Option<CachedSqlLookup>, bool)> {
        self.with_session_mut(session, |record| {
            Self::consume_cancellation_if_needed(record)?;
            let cached = record
                .cached_sql(sql)
                .map(|entry| CachedSqlLookup {
                    entry,
                    matched_shape: false,
                })
                .or_else(|| {
                    shape_sql.and_then(|shape_sql| {
                        record.cached_sql(shape_sql).map(|entry| CachedSqlLookup {
                            entry,
                            matched_shape: true,
                        })
                    })
                });
            Ok((
                cached,
                record.transaction_failed
                    && record.active_txn.is_some()
                    && !record.implicit_txn_active,
            ))
        })
    }

    pub(super) fn take_cancellation_and_failed_txn_status(
        &self,
        session: &SessionHandle,
    ) -> DbResult<bool> {
        self.with_session_mut(session, |record| {
            Self::consume_cancellation_if_needed(record)?;
            Ok(record.transaction_failed
                && record.active_txn.is_some()
                && !record.implicit_txn_active)
        })
    }

    pub(super) fn session_cancellation_checker(
        &self,
        session: &SessionHandle,
    ) -> DbResult<Arc<dyn Fn() -> DbResult<()> + Send + Sync>> {
        let session = self.session_entry(session)?;
        Ok(Arc::new(move || {
            let mut record = Self::lock_session(&session)?;
            Self::consume_cancellation_if_needed(&mut record)
        }))
    }

    pub(super) fn session_info(&self, session: &SessionHandle) -> DbResult<SessionInfo> {
        self.with_session(session, |record| {
            let mut info = record.info.clone();
            info.identity = self::session_vars::session_identity_for_record(record);
            info.limits = self::session_vars::effective_limits_for_record(record)?;
            Ok(info)
        })
    }

    pub(super) fn plan_cache_txn_id(&self, session: &SessionHandle) -> DbResult<TxnId> {
        self.with_session(session, |record| self.plan_cache_txn_id_for_record(record))
    }

    pub(super) fn current_txn_id(&self, session: &SessionHandle) -> DbResult<TxnId> {
        self.with_session(session, |record| {
            Ok(record
                .active_txn
                .as_ref()
                .map(|txn| txn.id)
                .unwrap_or_default())
        })
    }

    pub(super) fn current_txn_context(
        &self,
        session: &SessionHandle,
    ) -> DbResult<(TxnId, Snapshot)> {
        self.with_session(session, |record| {
            let txn_id = record
                .active_txn
                .as_ref()
                .map(|txn| txn.id)
                .unwrap_or_default();
            let snapshot = match record.active_txn.as_ref() {
                Some(txn) => self.snapshot_oracle.statement_snapshot(txn)?,
                None => Snapshot::new(TxnId::default(), TxnId::default(), Vec::new()),
            };
            Ok((txn_id, snapshot))
        })
    }

    pub(super) fn current_snapshot(&self, session: &SessionHandle) -> DbResult<Snapshot> {
        self.with_session(session, |record| match record.active_txn.as_ref() {
            Some(txn) => self.snapshot_oracle.statement_snapshot(txn),
            None => Ok(Snapshot::new(
                TxnId::default(),
                TxnId::default(),
                Vec::new(),
            )),
        })
    }
}
