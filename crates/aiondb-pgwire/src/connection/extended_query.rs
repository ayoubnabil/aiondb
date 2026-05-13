use super::result_wire::write_command_complete_for;
use super::*;
use crate::connection::helpers::{
    apply_direct_param_result_aliases, direct_param_result_alias_slots, preserve_direct_param_oids,
    write_row_description_from_result_columns,
};
use aiondb_core::Value;
use aiondb_engine::prepared::{decode_portal_batch_copy_in_tag, PORTAL_BATCH_COPY_OUT_TAG};
use bytes::Bytes;

const BIND_COMPLETE_FRAME: &[u8; 5] = b"2\0\0\0\x04";

/// Returns `true` when an Execute on this prepared statement may need
/// the engine-side wire metadata (`cleanup_hint` /
/// `changes_result_metadata` / resolved `effective_statement`). For
/// plain Select/Insert/Update/Delete/Merge/SetOperation prepareds the
/// This is the conservative side: when in doubt, return `true` so the
/// metadata path still runs.
fn statement_might_need_wire_metadata(statement: &aiondb_parser::Statement) -> bool {
    !matches!(
        statement,
        aiondb_parser::Statement::Select(_)
            | aiondb_parser::Statement::Insert(_)
            | aiondb_parser::Statement::Update(_)
            | aiondb_parser::Statement::Delete(_)
            | aiondb_parser::Statement::Merge(_)
            | aiondb_parser::Statement::SetOperation(_)
    )
}

impl<E, R, W> Connection<E, R, W>
where
    E: PgWireEngine + 'static,
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    fn build_deferred_portal_describe_response(
        &self,
        statement_name: &str,
        desc: &PreparedStatementDesc,
        result_formats: &[i16],
    ) -> DbResult<Arc<[u8]>> {
        let patched_columns = self
            .statement_wire_state
            .get(statement_name)
            .map(|state| {
                apply_direct_param_result_aliases(
                    state.direct_param_result_alias_slots.as_deref(),
                    &state.param_oids,
                    &desc.result_columns,
                )
            })
            .unwrap_or_else(|| desc.result_columns.clone());
        let mut w = MessageWriter::new();
        if patched_columns.is_empty() {
            messages::write_no_data(&mut w);
        } else {
            write_row_description_from_result_columns(
                &mut w,
                &patched_columns,
                &desc.result_column_origins,
                result_formats,
            )?;
        }
        Ok(Arc::from(w.finish_message()))
    }

    async fn write_extended_query_error(&mut self, error: &DbError) -> Result<(), DbError> {
        let mut w = MessageWriter::new();
        messages::write_error_response(&mut w, error);
        w.flush(&mut self.writer).await
    }

    fn parsed_statement_for_portal(&self, portal_name: &str) -> Option<aiondb_parser::Statement> {
        let statement_name = &self.portal_wire_state.get(portal_name)?.statement_name;
        self.statement_wire_state
            .get(statement_name)?
            .parsed_statement
            .clone()
    }

    fn clear_portal_wire_state_if_transaction_ended(
        &mut self,
        txn_status_before: messages::TransactionStatus,
        _batch: &PortalBatch,
        statement: Option<&aiondb_parser::Statement>,
    ) {
        if matches!(txn_status_before, messages::TransactionStatus::Idle) {
            return;
        }
        let Some(statement) = statement else {
            return;
        };
        if matches!(
            statement,
            aiondb_parser::Statement::Commit { .. } | aiondb_parser::Statement::Rollback { .. }
        ) {
            self.portal_wire_state.clear();
            self.savepoint_wire_state.clear();
        }
    }

    fn clear_portal_wire_state_created_since(&mut self, savepoint_generation: u64) {
        self.portal_wire_state.retain(|_, portal| {
            portal
                .created_under_savepoint_generation
                .map_or(true, |generation| generation < savepoint_generation)
        });
    }

    pub(super) async fn reset_unnamed_statement_state(
        &mut self,
        session: &SessionHandle,
    ) -> Result<(), DbError> {
        let had_unnamed_statement = self.statement_wire_state.remove("").is_some();
        let had_unnamed_portal_binding = self
            .portal_wire_state
            .values()
            .any(|state| state.statement_name.is_empty());
        self.portal_wire_state
            .retain(|name, state| !name.is_empty() && !state.statement_name.is_empty());
        if had_unnamed_statement || had_unnamed_portal_binding {
            self.run_engine({
                let session = session.clone();
                move |engine| engine.close_statement(&session, "")
            })
            .await?;
        }
        Ok(())
    }

    #[allow(clippy::unused_async)]
    pub(super) async fn reset_unnamed_portal_state(
        &mut self,
        _session: &SessionHandle,
    ) -> Result<(), DbError> {
        // Fast path: unnamed portal state is replaced on the next Bind for
        // the empty portal name, so avoid an extra close round-trip into the
        // engine on every Bind/Execute cycle.
        self.portal_wire_state.remove("");
        Ok(())
    }

    pub(super) async fn sync_savepoint_wire_state_after_success(
        &mut self,
        session: &SessionHandle,
        statement: &aiondb_parser::Statement,
    ) -> Result<(), DbError> {
        match statement {
            aiondb_parser::Statement::Savepoint { name, .. } => {
                let generation = self
                    .run_engine({
                        let session = session.clone();
                        let name = name.clone();
                        move |engine| engine.savepoint_generation(&session, &name)
                    })
                    .await?;
                if let Some(generation) = generation {
                    // Cap nested savepoints visible at the wire layer so a
                    // hostile client cannot grow `savepoint_wire_state`
                    // unboundedly via repeated `SAVEPOINT` without ROLLBACK
                    // (audit pgwire F-7).
                    const MAX_SAVEPOINT_WIRE_STATES: usize = 1024;
                    if self.savepoint_wire_state.len() >= MAX_SAVEPOINT_WIRE_STATES {
                        return Err(aiondb_core::DbError::program_limit(
                            "too many active savepoints in this session",
                        ));
                    }
                    self.savepoint_wire_state.push(SavepointWireState {
                        name: name.clone(),
                        generation,
                    });
                }
            }
            aiondb_parser::Statement::RollbackToSavepoint { name, .. } => {
                if let Some(pos) = self
                    .savepoint_wire_state
                    .iter()
                    .rposition(|entry| entry.name.eq_ignore_ascii_case(name))
                {
                    let generation = self.savepoint_wire_state[pos].generation;
                    self.clear_portal_wire_state_created_since(generation);
                    self.savepoint_wire_state.truncate(pos + 1);
                }
            }
            aiondb_parser::Statement::ReleaseSavepoint { name, .. } => {
                if let Some(pos) = self
                    .savepoint_wire_state
                    .iter()
                    .rposition(|entry| entry.name.eq_ignore_ascii_case(name))
                {
                    self.savepoint_wire_state.truncate(pos);
                }
            }
            aiondb_parser::Statement::Commit { .. } | aiondb_parser::Statement::Rollback { .. } => {
                self.portal_wire_state.clear();
                self.savepoint_wire_state.clear();
            }
            _ => {}
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Extended Query Protocol
    // -----------------------------------------------------------------------

    pub(super) async fn handle_parse(
        &mut self,
        name: &str,
        query: &str,
        param_oids: &[u32],
    ) -> Result<(), DbError> {
        let session = self.require_session()?.clone();

        debug!(
            pid = self.pid,
            statement_name = %name,
            query_kind = %Self::sql_debug_kind(query),
            query_len = query.len(),
            "parse statement"
        );

        let mut w = MessageWriter::new();
        if name.is_empty() {
            if self
                .statement_wire_state
                .get("")
                .is_some_and(|state| state.query == query && state.param_oids == param_oids)
            {
                self.portal_wire_state.retain(|portal_name, state| {
                    !portal_name.is_empty() && !state.statement_name.is_empty()
                });
                messages::write_parse_complete(&mut w);
                w.flush(&mut self.writer).await?;
                return Ok(());
            }
            self.reset_unnamed_statement_state(&session).await?;
        }
        let param_type_hints = match parse_param_type_hints(param_oids) {
            Ok(hints) => hints,
            Err(e) => {
                self.enter_extended_query_error_state().await?;
                messages::write_error_response(&mut w, &e);
                w.flush(&mut self.writer).await?;
                return Ok(());
            }
        };
        match self
            .run_engine({
                let session = session.clone();
                let name = name.to_string();
                let query = query.to_string();
                move |engine| {
                    engine.prepare_with_param_hints(&session, name, query, param_type_hints)
                }
            })
            .await
        {
            Ok(desc) => {
                if let Err(e) = validate_parse_param_oids(param_oids, &desc.param_types) {
                    let _ = self
                        .run_engine({
                            let session = session.clone();
                            let name = name.to_string();
                            move |engine| engine.close_statement(&session, &name)
                        })
                        .await;
                    self.enter_extended_query_error_state().await?;
                    messages::write_error_response(&mut w, &e);
                } else {
                    let parsed_statement = parse_single_statement(query);
                    self.statement_wire_state.insert(
                        name.to_string(),
                        StatementWireState {
                            param_oids: param_oids.to_vec(),
                            query: query.to_string(),
                            // Reuse the descriptor returned by Parse's
                            // prepare call so the first Bind does not issue an
                            // immediate describe round-trip.
                            prepared_desc: Some(desc),
                            deferred_describe_response_cache: std::collections::HashMap::new(),
                            direct_param_result_alias_slots: parsed_statement
                                .as_ref()
                                .and_then(direct_param_result_alias_slots),
                            parsed_statement_kind: ParsedStatementKind::from_parsed_statement(
                                parsed_statement.as_ref(),
                            ),
                            parsed_statement,
                        },
                    );
                    messages::write_parse_complete(&mut w);
                }
            }
            Err(e) => {
                self.enter_extended_query_error_state().await?;
                messages::write_error_response(&mut w, &e);
            }
        }
        w.flush(&mut self.writer).await?;
        Ok(())
    }

    pub(super) async fn handle_bind(
        &mut self,
        portal_name: &str,
        statement_name: &str,
        param_formats: &[i16],
        param_values: &[Option<Bytes>],
        result_formats: &[i16],
    ) -> Result<(), DbError> {
        let session = self.require_session()?.clone();

        debug!(
            pid = self.pid,
            portal = %portal_name,
            statement_name = %statement_name,
            param_count = param_values.len(),
            result_format_count = result_formats.len(),
            "bind portal"
        );

        if portal_name.is_empty() {
            self.reset_unnamed_portal_state(&session).await?;
        }
        let mut fetched_desc: Option<PreparedStatementDesc> = None;
        if self
            .statement_wire_state
            .get(statement_name)
            .and_then(|state| state.prepared_desc.as_ref())
            .is_none()
        {
            match self
                .run_engine({
                    let session = session.clone();
                    let statement_name = statement_name.to_string();
                    move |engine| engine.describe_statement(&session, &statement_name)
                })
                .await
            {
                Ok(desc) => {
                    if let Some(state) = self.statement_wire_state.get_mut(statement_name) {
                        state.prepared_desc = Some(desc);
                    } else {
                        fetched_desc = Some(desc);
                    }
                }
                Err(e) => {
                    self.enter_extended_query_error_state().await?;
                    self.write_extended_query_error(&e).await?;
                    return Ok(());
                }
            }
        }
        let desc = self
            .statement_wire_state
            .get(statement_name)
            .and_then(|state| state.prepared_desc.as_ref())
            .or(fetched_desc.as_ref())
            .ok_or_else(|| DbError::protocol("unknown prepared statement"))?;

        if let Err(e) = validate_bind_formats(param_formats, param_values.len())
            .and_then(|()| validate_result_formats(result_formats, desc.result_columns.len()))
        {
            self.enter_extended_query_error_state().await?;
            self.write_extended_query_error(&e).await?;
            return Ok(());
        }

        let limit = self.max_portals;
        if self.portal_wire_state.len() >= limit
            && !self.portal_wire_state.contains_key(portal_name)
        {
            self.enter_extended_query_error_state().await?;
            let e = DbError::program_limit(format!("maximum number of portals reached ({limit})"));
            self.write_extended_query_error(&e).await?;
            return Ok(());
        }

        if !portal_name.is_empty() && self.portal_wire_state.contains_key(portal_name) {
            self.enter_extended_query_error_state().await?;
            let e = DbError::bind_error(
                aiondb_core::SqlState::DuplicateObject,
                format!("portal \"{portal_name}\" already exists"),
            );
            self.write_extended_query_error(&e).await?;
            return Ok(());
        }

        let params =
            match coerce_bind_params_dispatched(&desc.param_types, param_formats, param_values) {
                Ok(params) => params,
                Err(e) => {
                    self.enter_extended_query_error_state().await?;
                    self.write_extended_query_error(&e).await?;
                    return Ok(());
                }
            };

        // Defer the engine-side Bind until Describe Portal / Execute / Close
        // actually need a materialized engine portal. This removes one engine
        // round-trip from the extended-query hot path for both unnamed and
        // named portals while preserving wire-visible portal semantics locally.
        let deferred_describe_response = {
            if let Some(cached) = self
                .statement_wire_state
                .get(statement_name)
                .and_then(|state| state.deferred_describe_response_cache.get(result_formats))
                .cloned()
            {
                Some(cached)
            } else {
                match self.build_deferred_portal_describe_response(
                    statement_name,
                    desc,
                    result_formats,
                ) {
                    Ok(response) => {
                        if let Some(state) = self.statement_wire_state.get_mut(statement_name) {
                            // Bound the per-statement describe-response cache so a
                            // hostile client cannot mint up to 2^columns distinct
                            // result_formats keys. Drop the whole cache on overflow
                            // (cheap, RowDescription rebuild is O(columns)).
                            const MAX_DESCRIBE_CACHE_ENTRIES: usize = 32;
                            if state.deferred_describe_response_cache.len()
                                >= MAX_DESCRIBE_CACHE_ENTRIES
                            {
                                state.deferred_describe_response_cache.clear();
                            }
                            state
                                .deferred_describe_response_cache
                                .insert(result_formats.to_vec(), response.clone());
                        }
                        Some(response)
                    }
                    Err(e) => {
                        self.enter_extended_query_error_state().await?;
                        self.write_extended_query_error(&e).await?;
                        return Ok(());
                    }
                }
            }
        };
        let created_under_savepoint_generation = self
            .savepoint_wire_state
            .last()
            .map(|entry| entry.generation);
        if let Some(portal) = self.portal_wire_state.get_mut(portal_name) {
            if portal.result_formats.as_ref() != result_formats {
                // `Arc::from(&[i16])` does the underlying allocation
                // exactly once. The previous `to_vec()` first then
                // `Arc::from(Vec)` paid a transient Vec allocation.
                portal.result_formats = Arc::from(result_formats);
            }
            portal.statement_name.clear();
            portal.statement_name.push_str(statement_name);
            portal.rows_sent = 0;
            portal.created_under_savepoint_generation = created_under_savepoint_generation;
            portal.deferred_bind_params = Some(params);
            portal.deferred_describe_response = deferred_describe_response;
        } else {
            self.portal_wire_state.insert(
                portal_name.to_string(),
                PortalWireState {
                    result_formats: Arc::from(result_formats),
                    statement_name: statement_name.to_string(),
                    rows_sent: 0,
                    created_under_savepoint_generation,
                    deferred_bind_params: Some(params),
                    deferred_describe_response,
                },
            );
        }
        self.writer
            .write_all(BIND_COMPLETE_FRAME)
            .await
            .map_err(|e| DbError::protocol(format!("write: {e}")))?;
        Ok(())
    }

    pub(super) async fn handle_describe(
        &mut self,
        target: DescribeTarget,
        name: &str,
    ) -> Result<(), DbError> {
        let session = self.require_session()?;

        debug!(pid = self.pid, ?target, name = %name, "describe target");

        let mut w = MessageWriter::new();
        match target {
            DescribeTarget::Statement => {
                let cached_desc = self
                    .statement_wire_state
                    .get(name)
                    .and_then(|state| state.prepared_desc.clone());
                let desc_result = if let Some(desc) = cached_desc {
                    Ok(desc)
                } else {
                    self.run_engine({
                        let session = session.clone();
                        let name = name.to_string();
                        move |engine| engine.describe_statement(&session, &name)
                    })
                    .await
                };
                match desc_result {
                    Ok(desc) => {
                        if let Some(state) = self.statement_wire_state.get_mut(name) {
                            state.prepared_desc = Some(desc.clone());
                        }
                        let mut effective_desc = desc.clone();
                        let explicit_param_oids =
                            self.statement_wire_state.get(name).map(|state| {
                                effective_desc.result_columns = apply_direct_param_result_aliases(
                                    state.direct_param_result_alias_slots.as_deref(),
                                    &state.param_oids,
                                    &effective_desc.result_columns,
                                );
                                preserve_direct_param_oids(
                                    &state.query,
                                    &state.param_oids,
                                    effective_desc.param_types.len(),
                                )
                            });
                        let explicit_param_oids = explicit_param_oids.as_deref();
                        if let Err(e) =
                            write_stmt_description(&mut w, &effective_desc, explicit_param_oids)
                        {
                            self.enter_extended_query_error_state().await?;
                            messages::write_error_response(&mut w, &e);
                        }
                    }
                    Err(e) => {
                        self.enter_extended_query_error_state().await?;
                        messages::write_error_response(&mut w, &e);
                    }
                }
            }
            DescribeTarget::Portal => {
                if let Some(response) = self
                    .portal_wire_state
                    .get(name)
                    .and_then(|state| state.deferred_describe_response.as_ref())
                {
                    self.writer
                        .write_all(response.as_ref())
                        .await
                        .map_err(|e| DbError::protocol(format!("write: {e}")))?;
                    return Ok(());
                }
                let deferred_statement_name = self.portal_wire_state.get(name).and_then(|state| {
                    state
                        .deferred_bind_params
                        .as_ref()
                        .map(|_| state.statement_name.clone())
                });
                let describe_result = if let Some(statement_name) = deferred_statement_name {
                    if let Some(desc) = self
                        .statement_wire_state
                        .get(&statement_name)
                        .and_then(|state| state.prepared_desc.clone())
                    {
                        Ok(PortalDescription {
                            result_columns: desc.result_columns,
                            result_column_origins: desc.result_column_origins,
                        })
                    } else {
                        self.run_engine({
                            let session = session.clone();
                            let statement_name = statement_name.clone();
                            move |engine| engine.describe_statement(&session, &statement_name)
                        })
                        .await
                        .map(|desc| PortalDescription {
                            result_columns: desc.result_columns,
                            result_column_origins: desc.result_column_origins,
                        })
                    }
                } else {
                    self.run_engine({
                        let session = session.clone();
                        let name = name.to_string();
                        move |engine| engine.describe_portal(&session, &name)
                    })
                    .await
                };
                match describe_result {
                    Ok(desc) => {
                        if desc.result_columns.is_empty() {
                            messages::write_no_data(&mut w);
                        } else {
                            let patched_columns = self
                                .portal_wire_state
                                .get(name)
                                .and_then(|portal| {
                                    self.statement_wire_state.get(&portal.statement_name).map(
                                        |state| {
                                            apply_direct_param_result_aliases(
                                                state.direct_param_result_alias_slots.as_deref(),
                                                &state.param_oids,
                                                &desc.result_columns,
                                            )
                                        },
                                    )
                                })
                                .unwrap_or_else(|| desc.result_columns.clone());
                            let formats: &[i16] = self
                                .portal_wire_state
                                .get(name)
                                .map_or(&[], |state| state.result_formats.as_ref());
                            if let Err(e) = write_row_description_from_result_columns(
                                &mut w,
                                &patched_columns,
                                &desc.result_column_origins,
                                formats,
                            ) {
                                self.enter_extended_query_error_state().await?;
                                messages::write_error_response(&mut w, &e);
                            }
                        }
                    }
                    Err(e) => {
                        self.enter_extended_query_error_state().await?;
                        messages::write_error_response(&mut w, &e);
                    }
                }
            }
        }
        w.flush(&mut self.writer).await?;
        Ok(())
    }

    pub(super) async fn handle_execute(
        &mut self,
        portal_name: &str,
        max_rows: i32,
    ) -> Result<(), DbError> {
        let session = self.require_session()?.clone();

        debug!(
            pid = self.pid,
            portal = %portal_name,
            max_rows,
            "execute portal"
        );

        let limit = match max_rows {
            0 => usize::MAX,
            1.. => usize::try_from(max_rows)
                .map_err(|_| DbError::protocol(format!("invalid Execute max_rows: {max_rows}")))?,
            _ => {
                return Err(DbError::protocol(format!(
                    "invalid Execute max_rows: {max_rows}"
                )));
            }
        };

        if let Some(m) = &self.metrics {
            m.total_queries.fetch_add(1, Ordering::Relaxed);
        }
        let txn_status_before = self.txn_status;
        // Fast path: regular Select/Insert/Update/Delete/Merge/SetOperation
        // prepared statements have no wire-metadata side-effects:
        //   * `cleanup_hint` is only set for Deallocate/Close/compat-Execute
        //   * `changes_result_metadata` is only true for DDL statements
        //     savepoint-touching statement kinds.
        // The bench-shaped `SELECT ... FROM ... WHERE ...` path therefore
        // always sees `None` / `false` from this engine round-trip, so
        // skipping it removes one engine pool dispatch and lock acquisition
        // per Execute on the prepared OLTP hot path.
        //
        // Performance: the upfront lookup grabs `statement_kind` and
        // `statement_for_wire` in a single nested `HashMap::get`. The SQL
        // text and statement name are only cloned when we actually take the
        // slow path, so plain prepared DML pays just one nested lookup and
        // an `Arc` clone per Execute.
        let (statement_kind, statement_for_wire) = self
            .portal_wire_state
            .get(portal_name)
            .and_then(|portal| {
                self.statement_wire_state
                    .get(&portal.statement_name)
                    .map(|state| (state.parsed_statement_kind, state.parsed_statement.clone()))
            })
            .unwrap_or((ParsedStatementKind::Other, None));
        let needs_wire_metadata = !matches!(statement_kind, ParsedStatementKind::Other)
            || statement_for_wire
                .as_ref()
                .is_some_and(statement_might_need_wire_metadata);
        let wire_metadata = if needs_wire_metadata {
            // Slow path: re-resolve to grab the SQL text. The double lookup
            // here is fine because this branch is rare (DDL, compat
            // statements, BEGIN/COMMIT/...).
            let sql_and_statement = self.portal_wire_state.get(portal_name).and_then(|portal| {
                self.statement_wire_state
                    .get(&portal.statement_name)
                    .map(|state| (state.query.clone(), state.parsed_statement.clone()))
            });
            if let Some((statement_sql, Some(statement))) = sql_and_statement {
                self.run_engine({
                    let session = session.clone();
                    move |engine| {
                        engine.sql_statement_wire_metadata(&session, &statement_sql, &statement)
                    }
                })
                .await
                .ok()
            } else {
                None
            }
        } else {
            None
        };
        let effective_statement =
            if statement_kind.is_execute_noop() || statement_kind.touches_savepoint_state() {
                wire_metadata
                    .as_ref()
                    .and_then(|metadata| metadata.effective_statement.clone())
                    .or_else(|| self.parsed_statement_for_portal(portal_name))
            } else {
                None
            };
        let opens_explicit_txn = if statement_kind.is_execute_noop() {
            effective_statement
                .as_ref()
                .is_some_and(Self::statement_opens_explicit_transaction)
        } else {
            statement_kind.opens_explicit_transaction()
        };
        let deferred_bind = self
            .portal_wire_state
            .get_mut(portal_name)
            .and_then(|portal| {
                portal
                    .deferred_bind_params
                    .take()
                    .map(|params| (portal.statement_name.clone(), params))
            });
        let mut w = MessageWriter::new();
        // The deferred-bind path for unnamed portals does not consume
        // `portal_name`, so don't pay the String allocation just to satisfy
        // the closure's `'static` requirement. Named deferred portals still
        // need an owned copy, but they avoid the separate Bind dispatch.
        let engine_result = if let Some((statement_name, params)) = deferred_bind {
            if portal_name.is_empty() {
                self.run_engine({
                    let session = session.clone();
                    move |engine| {
                        engine.execute_prepared_statement_with_notices(
                            &session,
                            statement_name,
                            params,
                            limit,
                        )
                    }
                })
                .await
            } else {
                self.run_engine({
                    let session = session.clone();
                    let portal_name = portal_name.to_string();
                    move |engine| {
                        engine.bind(&session, portal_name.clone(), statement_name, params)?;
                        engine.execute_portal_with_notices(&session, &portal_name, limit)
                    }
                })
                .await
            }
        } else {
            self.run_engine({
                let session = session.clone();
                let portal_name = portal_name.to_string();
                move |engine| engine.execute_portal_with_notices(&session, &portal_name, limit)
            })
            .await
        };
        match engine_result {
            Ok((mut batch, notices)) => {
                debug!(
                    pid = self.pid,
                    row_count = batch.rows.len(),
                    exhausted = batch.exhausted,
                    tag = %batch.tag,
                    "execute portal result"
                );
                self.finalize_portal_query_tag(portal_name, &mut batch);
                for message in &notices {
                    messages::write_notice_response(&mut w, message);
                }
                if let Some((table_id, column_count)) = decode_portal_batch_copy_in_tag(&batch.tag)
                {
                    if let Err(e) = messages::write_copy_in_response(&mut w, column_count) {
                        self.enter_extended_query_error_state().await?;
                        messages::write_error_response(&mut w, &e);
                        w.flush(&mut self.writer).await?;
                        return Ok(());
                    }
                    w.flush(&mut self.writer).await?;

                    let mut post_copy = MessageWriter::new();
                    match self.handle_copy_in_data(table_id).await {
                        Ok(result) => {
                            if matches!(txn_status_before, TransactionStatus::Idle)
                                && !opens_explicit_txn
                            {
                                self.txn_status = TransactionStatus::Idle;
                            } else {
                                self.refresh_txn_status_after_success().await?;
                            }
                            let notices = match self
                                .run_engine({
                                    let session = session.clone();
                                    move |engine| engine.drain_pending_notices(&session)
                                })
                                .await
                            {
                                Ok(notices) => notices,
                                Err(e) => {
                                    self.enter_extended_query_error_state().await?;
                                    messages::write_error_response(&mut post_copy, &e);
                                    post_copy.flush(&mut self.writer).await?;
                                    return Ok(());
                                }
                            };
                            for message in &notices {
                                messages::write_notice_response(&mut post_copy, message);
                            }
                            if let Err(e) = result_wire::write_copy_in_completion_result(
                                &mut post_copy,
                                &result,
                            ) {
                                self.enter_extended_query_error_state().await?;
                                messages::write_error_response(&mut post_copy, &e);
                            }
                        }
                        Err(e) => {
                            self.enter_extended_query_error_state().await?;
                            messages::write_error_response(&mut post_copy, &e);
                        }
                    }
                    post_copy.flush(&mut self.writer).await?;
                    return Ok(());
                }

                let copy_out = Self::portal_batch_copy_out_data(&batch);
                if matches!(txn_status_before, TransactionStatus::Idle) && !opens_explicit_txn {
                    self.txn_status = TransactionStatus::Idle;
                } else {
                    self.refresh_txn_status_after_success().await?;
                }
                if statement_kind.touches_savepoint_state() || statement_kind.is_execute_noop() {
                    if let Some(statement) = effective_statement.as_ref() {
                        self.sync_savepoint_wire_state_after_success(&session, statement)
                            .await?;
                    }
                }
                if let Some(hint) = wire_metadata
                    .as_ref()
                    .and_then(|metadata| metadata.cleanup_hint.as_ref())
                {
                    self.apply_wire_state_cleanup(hint);
                }
                if wire_metadata
                    .as_ref()
                    .is_some_and(|metadata| metadata.changes_result_metadata)
                {
                    self.invalidate_prepared_desc_cache();
                }
                self.clear_portal_wire_state_if_transaction_ended(
                    txn_status_before,
                    &batch,
                    if statement_kind.is_commit_or_rollback() || statement_kind.is_execute_noop() {
                        effective_statement.as_ref()
                    } else {
                        None
                    },
                );
                if let Some((data, column_count)) = copy_out {
                    if let Err(e) = copy::write_copy_out_result(&mut w, data, column_count) {
                        self.enter_extended_query_error_state().await?;
                        messages::write_error_response(&mut w, &e);
                    }
                } else if let Err(e) = Self::write_portal_batch(
                    &mut w,
                    &batch,
                    self.portal_wire_state
                        .get(portal_name)
                        .map_or(&[], |state| state.result_formats.as_ref()),
                ) {
                    self.enter_extended_query_error_state().await?;
                    messages::write_error_response(&mut w, &e);
                }
            }
            Err(e) => {
                self.enter_extended_query_error_state().await?;
                messages::write_error_response(&mut w, &e);
            }
        }
        w.flush(&mut self.writer).await?;
        Ok(())
    }

    fn portal_batch_copy_out_data(batch: &PortalBatch) -> Option<(&str, usize)> {
        if !batch.exhausted || batch.tag != PORTAL_BATCH_COPY_OUT_TAG || batch.rows.len() != 1 {
            return None;
        }
        let row = batch.rows.first()?;
        let Value::Text(data) = row.values.first()? else {
            return None;
        };
        Some((data.as_str(), batch.columns.len()))
    }

    fn finalize_portal_query_tag(&mut self, portal_name: &str, batch: &mut PortalBatch) {
        if batch.tag != "SELECT" {
            return;
        }

        let batch_rows = u64::try_from(batch.rows.len()).unwrap_or(u64::MAX);
        if let Some(state) = self.portal_wire_state.get_mut(portal_name) {
            state.rows_sent = state.rows_sent.saturating_add(batch_rows);
            if batch.exhausted {
                batch.tag = format!("SELECT {}", state.rows_sent);
                state.rows_sent = 0;
            }
        } else if batch.exhausted {
            batch.tag = format!("SELECT {batch_rows}");
        }
    }

    pub(super) async fn handle_close(
        &mut self,
        target: CloseTarget,
        name: &str,
    ) -> Result<(), DbError> {
        let session = self.require_session()?.clone();

        let mut w = MessageWriter::new();
        let local_deferred_portal = matches!(target, CloseTarget::Portal)
            && self
                .portal_wire_state
                .get(name)
                .and_then(|state| state.deferred_bind_params.as_ref())
                .is_some();
        let result = match target {
            CloseTarget::Statement => {
                self.run_engine({
                    let session = session.clone();
                    let name = name.to_string();
                    move |engine| engine.close_statement(&session, &name)
                })
                .await
            }
            CloseTarget::Portal => {
                if local_deferred_portal {
                    Ok(())
                } else {
                    self.run_engine({
                        let session = session.clone();
                        let name = name.to_string();
                        move |engine| engine.close_portal(&session, &name)
                    })
                    .await
                }
            }
        };

        match result {
            Ok(()) => {
                if matches!(target, CloseTarget::Statement) {
                    self.statement_wire_state.remove(name);
                    self.portal_wire_state
                        .retain(|_, state| state.statement_name != name);
                } else {
                    self.portal_wire_state.remove(name);
                }
                messages::write_close_complete(&mut w);
            }
            Err(e) => {
                self.enter_extended_query_error_state().await?;
                messages::write_error_response(&mut w, &e);
            }
        }
        w.flush(&mut self.writer).await?;
        Ok(())
    }

    pub(super) fn write_portal_batch(
        w: &mut MessageWriter,
        batch: &PortalBatch,
        result_formats: &[i16],
    ) -> Result<(), DbError> {
        if batch.exhausted && batch.columns.is_empty() && batch.rows.is_empty() {
            if let Some(message) = batch.tag.strip_prefix("NOTICE: ") {
                messages::write_notice_response(w, message);
                messages::write_command_complete(w, "SELECT 0");
                return Ok(());
            }
            if batch.tag == "EMPTY" {
                messages::write_command_complete(w, "SELECT 0");
                return Ok(());
            }
        }

        for (row_index, row) in batch.rows.iter().enumerate() {
            result_wire::validate_row_width(
                "portal batch",
                row_index,
                batch.columns.len(),
                row.values.len(),
            )?;
        }

        reserve_portal_batch_wire_capacity(w, batch);
        // Resolve format codes once per batch instead of once per cell. For a
        // 1000-row × N-column SELECT this removes ~N×1000 calls to
        // `resolve_result_format_code` from the inner encode loop.
        let binary_flags =
            messages::resolve_per_column_binary_flags(&batch.columns, result_formats);
        for row in &batch.rows {
            messages::write_data_row_direct_from_columns_resolved(
                w,
                &row.values,
                &batch.columns,
                &binary_flags,
            )?;
        }
        if batch.exhausted {
            if batch.tag.is_empty() || batch.tag == "EMPTY" {
                messages::write_command_complete_with_count(
                    w,
                    "SELECT",
                    false,
                    batch.rows.len() as u64,
                );
            } else if batch.columns.is_empty() {
                write_command_complete_for(w, &batch.tag, batch.rows_affected);
            } else {
                messages::write_command_complete(w, &batch.tag);
            }
        } else {
            messages::write_portal_suspended(w);
        }
        Ok(())
    }
}

fn reserve_portal_batch_wire_capacity(w: &mut MessageWriter, batch: &PortalBatch) {
    if batch.rows.is_empty() || batch.columns.is_empty() {
        return;
    }

    let fixed_per_row = 7usize.saturating_add(batch.columns.len().saturating_mul(4));
    let mut estimated = batch.rows.len().saturating_mul(fixed_per_row);
    for row in &batch.rows {
        for value in &row.values {
            estimated = estimated.saturating_add(match value {
                aiondb_core::Value::Null => 0,
                aiondb_core::Value::Int(_) => 11,
                aiondb_core::Value::BigInt(_) => 20,
                aiondb_core::Value::Boolean(_) => 1,
                aiondb_core::Value::Real(_) | aiondb_core::Value::Double(_) => 24,
                aiondb_core::Value::Text(text) => text.len(),
                _ => 32,
            });
        }
    }
    w.reserve(estimated);
}

fn parse_single_statement(sql: &str) -> Option<aiondb_parser::Statement> {
    let mut statements = parse_wire_metadata_statements(sql)?;
    if statements.len() == 1 {
        statements.pop()
    } else {
        None
    }
}

fn parse_param_type_hints(param_oids: &[u32]) -> Result<Vec<Option<DataType>>, DbError> {
    param_oids
        .iter()
        .enumerate()
        .map(|(index, oid)| {
            if *oid == 0 {
                return Ok(None);
            }
            let Some(data_type) = pg_oid_to_data_type(*oid) else {
                return Err(DbError::protocol(format!(
                    "Parse parameter ${} uses unsupported type oid {}",
                    index + 1,
                    oid
                )));
            };
            Ok(Some(data_type))
        })
        .collect()
}

fn validate_parse_param_oids(
    param_oids: &[u32],
    expected_types: &[DataType],
) -> Result<(), DbError> {
    if param_oids.is_empty() {
        return Ok(());
    }
    if param_oids.len() > expected_types.len() {
        return Err(DbError::protocol(format!(
            "Parse specified {} parameter type oid(s), but statement has {} parameter(s)",
            param_oids.len(),
            expected_types.len()
        )));
    }

    for (index, (provided_oid, expected_type)) in param_oids.iter().zip(expected_types).enumerate()
    {
        // OID 0 means "unspecified" in the protocol; keep planner inference.
        if *provided_oid == 0 {
            continue;
        }
        let Some(provided_type) = pg_oid_to_data_type(*provided_oid) else {
            return Err(DbError::protocol(format!(
                "Parse parameter ${} uses unsupported type oid {}",
                index + 1,
                provided_oid
            )));
        };
        let matches = provided_type == *expected_type
            || matches!(
                (&provided_type, expected_type),
                (
                    DataType::Vector {
                        dims: 0,
                        element_type: aiondb_core::VectorElementType::Float32
                    },
                    DataType::Vector { .. }
                )
            );
        if !matches {
            return Err(DbError::protocol(format!(
                "Parse parameter ${}: type oid {} ({}) does not match inferred type {}",
                index + 1,
                provided_oid,
                provided_type.pg_type_name(),
                expected_type.pg_type_name()
            )));
        }
    }
    Ok(())
}
