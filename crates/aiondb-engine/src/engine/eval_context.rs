#![allow(clippy::redundant_closure_for_method_calls, clippy::wildcard_imports)]

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Mutex;

use aiondb_core::{compat_role_oid, DbError, DbResult};
use aiondb_eval::{
    async_notify::{with_sink as with_notify_sink, NotifySink},
    cancel::with_cancellation_checker,
    set_global_compat_definition_caches, with_extension_registry,
    with_session_context as with_eval_session_context, EvalSessionContext,
};
use aiondb_parser::Statement;

use super::*;

fn compat_relation_oid(relation_id: aiondb_core::RelationId) -> i32 {
    i32::try_from(relation_id.get())
        .unwrap_or(i32::MAX)
        .saturating_add(16_384)
}

fn compat_index_oid(index_id: aiondb_core::IndexId) -> i32 {
    i32::try_from(index_id.get())
        .unwrap_or(i32::MAX)
        .saturating_add(32_768)
}

fn format_compat_qualified_name(name: &aiondb_catalog::QualifiedName) -> String {
    match name.schema_name() {
        Some(schema) => format!("{schema}.{}", name.object_name()),
        None => format!("public.{}", name.object_name()),
    }
}

fn format_compat_fk_reference_name(name: &aiondb_catalog::QualifiedName) -> String {
    match name.schema_name() {
        Some(schema) if !schema.eq_ignore_ascii_case("public") => {
            format!("{schema}.{}", name.object_name())
        }
        _ => name.object_name().to_owned(),
    }
}

fn normalize_compat_fk_reference_text(name: &str) -> String {
    if let Some((schema, object)) = name.split_once('.') {
        if schema.eq_ignore_ascii_case("public") && !object.is_empty() {
            return object.to_owned();
        }
    }
    name.to_owned()
}

thread_local! {
    static COMPAT_SESSION_STACK: RefCell<Vec<SessionHandle>> = const { RefCell::new(Vec::new()) };
}

/// RAII guard that owns its push/pop pair atomically. The previous shape
/// (`stack.push(...); let _guard = CompatSessionGuard;`) had a tiny
/// window where a panic between push and guard creation would leak the
/// entry. Bundling push into `new()` removes the window.
struct CompatSessionGuard;

impl CompatSessionGuard {
    fn enter(session: &SessionHandle) -> Self {
        COMPAT_SESSION_STACK.with(|stack| {
            stack.borrow_mut().push(session.clone());
        });
        Self
    }
}

impl Drop for CompatSessionGuard {
    fn drop(&mut self) {
        COMPAT_SESSION_STACK.with(|stack| {
            let _ = stack.borrow_mut().pop();
        });
    }
}

struct CompatNotifySink {
    bus: Arc<super::async_notify::NotificationBus>,
    session: SessionHandle,
    pending: Arc<Mutex<Vec<(String, String)>>>,
}

impl NotifySink for CompatNotifySink {
    fn push(&self, channel: &str, payload: &str) -> DbResult<()> {
        let mut pending = self
            .pending
            .lock()
            .map_err(|_| DbError::internal("notification sink lock poisoned"))?;
        pending.push((channel.to_owned(), payload.to_owned()));
        Ok(())
    }

    fn queue_usage(&self) -> f64 {
        self.bus.queue_usage()
    }

    fn listening_channels(&self) -> Vec<String> {
        self.bus.listening_channels(&self.session)
    }
}

struct PlanRequestSessionContext {
    txn_id: TxnId,
    default_schema: Option<String>,
    current_user: String,
    session_user: String,
    database_name: String,
    datestyle: Option<String>,
    timezone: Option<String>,
    hnsw_ef_search: Option<usize>,
}

impl Engine {
    pub(super) fn describe_planned_statement_with_param_hints(
        &self,
        session: &SessionHandle,
        statement: &Statement,
        param_type_hints: Option<&[Option<aiondb_core::DataType>]>,
    ) -> DbResult<(
        Vec<ResultColumn>,
        Vec<Option<crate::prepared::ResultColumnOrigin>>,
        Vec<aiondb_core::DataType>,
    )> {
        self.with_compat_eval_session(session, || {
            let statement = if super::recursive_cte::statement_contains_recursive_cte(statement) {
                super::recursive_cte::maybe_rewrite_for_execution(self, session, statement)?
            } else {
                std::borrow::Cow::Borrowed(statement)
            };
            let planning_context = self.plan_request_session_context(session)?;
            let request = PlanRequest {
                statement: statement.as_ref(),
                txn_id: planning_context.txn_id,
                default_schema: planning_context.default_schema,
                current_user: Some(planning_context.current_user),
                session_user: Some(planning_context.session_user),
                database_name: Some(planning_context.database_name),
                datestyle: planning_context.datestyle,
                timezone: planning_context.timezone,
            };
            let description = self
                .planner
                .describe_with_param_hints(request, param_type_hints)?;
            Ok((
                description
                    .output_fields
                    .into_iter()
                    .map(|field| ResultColumn {
                        name: field.name,
                        data_type: field.data_type,
                        text_type_modifier: field.text_type_modifier,
                        nullable: field.nullable,
                    })
                    .collect(),
                description
                    .output_origins
                    .into_iter()
                    .map(|origin| {
                        origin.map(|origin| crate::prepared::ResultColumnOrigin {
                            relation_id: origin.relation_id,
                            column_attr: origin.column_attr,
                        })
                    })
                    .collect(),
                description.param_types,
            ))
        })
    }

    pub(super) fn build_physical_plan(
        &self,
        session: &SessionHandle,
        statement: &Statement,
    ) -> DbResult<aiondb_plan::PhysicalPlan> {
        self.with_compat_eval_session(session, || {
            let planning_context = self.plan_request_session_context(session)?;
            let logical_plan = self.planner.plan(PlanRequest {
                statement,
                txn_id: planning_context.txn_id,
                default_schema: planning_context.default_schema,
                current_user: Some(planning_context.current_user),
                session_user: Some(planning_context.session_user),
                database_name: Some(planning_context.database_name),
                datestyle: planning_context.datestyle,
                timezone: planning_context.timezone,
            })?;
            let physical_plan = self.optimizer.optimize_with_hnsw_ef_search(
                OptimizeRequest {
                    logical_plan,
                    txn_id: planning_context.txn_id,
                },
                planning_context.hnsw_ef_search,
            )?;
            Ok(physical_plan)
        })
    }

    fn plan_request_session_context(
        &self,
        session: &SessionHandle,
    ) -> DbResult<PlanRequestSessionContext> {
        self.with_session(session, |record| {
            let txn_id = record
                .active_txn
                .as_ref()
                .map(|txn| txn.id)
                .unwrap_or_default();
            let session_settings = self::session_vars::session_settings_for_record(
                self.catalog_reader.as_ref(),
                txn_id,
                record,
            )?;
            Ok(PlanRequestSessionContext {
                txn_id,
                default_schema: self::session_vars::primary_search_path_schema_for_record(
                    self.catalog_reader.as_ref(),
                    txn_id,
                    record,
                )?,
                current_user: self::session_vars::current_user_for_record(record),
                session_user: self::session_vars::session_user_for_record(record),
                database_name: record.info.database_name.clone(),
                datestyle: self::session_vars::effective_session_variable_for_record(
                    record,
                    "datestyle",
                ),
                timezone: self::session_vars::effective_session_variable_for_record(
                    record, "timezone",
                ),
                hnsw_ef_search: self::session_vars::resolve_hnsw_ef_search_setting(
                    &session_settings,
                )?,
            })
        })
    }

    fn compat_eval_session_context(&self, session: &SessionHandle) -> DbResult<EvalSessionContext> {
        self.with_session(session, |record| {
            let txn_id = record
                .active_txn
                .as_ref()
                .map(|txn| txn.id)
                .unwrap_or_default();
            let current_user = self::session_vars::current_user_for_record(record);
            let search_path_schemas = self::session_vars::effective_search_path_schemas_for_record(
                self.catalog_reader.as_ref(),
                txn_id,
                record,
            )?;
            let current_schema = search_path_schemas.first().cloned();
            let mut role_names_by_oid = self.cached_role_names_by_oid(txn_id)?;
            if !role_names_by_oid.contains_key(&compat_role_oid(&current_user)) {
                let mut cloned_role_names = role_names_by_oid.as_ref().clone();
                cloned_role_names.insert(compat_role_oid(&current_user), current_user.clone());
                role_names_by_oid = Arc::new(cloned_role_names);
            }

            let mut eval_session = EvalSessionContext::from_settings_with_interval_style(
                self::session_vars::effective_session_variable_for_record(record, "datestyle")
                    .as_deref(),
                self::session_vars::effective_session_variable_for_record(record, "timezone")
                    .as_deref(),
                self::session_vars::effective_session_variable_for_record(record, "intervalstyle")
                    .as_deref(),
            )
            .with_current_schema(current_schema.clone())
            .with_current_database(Some(record.info.database_name.clone()))
            .with_lo_session_key(session.stable_hash_key())
            .with_search_path_schemas(search_path_schemas);
            eval_session.role_names_by_oid = role_names_by_oid;
            eval_session.compat_user_types = record.compat_user_types.clone();
            eval_session.compat_user_casts = record.compat_user_casts.clone();
            eval_session.domain_defs = record.domain_defs.clone();
            eval_session.compat_comments = Arc::new(record.comments.clone());
            eval_session.compat_security_labels = Arc::new(record.security_labels.clone());
            let misc_attrs = record
                .compat_misc_attrs
                .iter()
                .map(|(key, attrs)| {
                    let options_joined = attrs
                        .options
                        .iter()
                        .map(|(k, v)| format!("{k}={v}"))
                        .collect::<Vec<_>>()
                        .join(", ");
                    (
                        key.clone(),
                        (
                            attrs.owner.clone().unwrap_or_default(),
                            attrs.schema.clone().unwrap_or_default(),
                            attrs.state.clone().unwrap_or_default(),
                            options_joined,
                            attrs.tablespace.clone().unwrap_or_default(),
                            attrs.version.clone().unwrap_or_default(),
                        ),
                    )
                })
                .collect::<HashMap<_, _>>();
            eval_session.compat_misc_attrs = Arc::new(misc_attrs);
            eval_session.compat_misc_objects = Arc::new(record.compat_misc_objects.clone());
            eval_session.compat_trigger_state = Arc::new(record.compat_trigger_state.clone());
            let rules_view: HashMap<(String, String), String> = record
                .compat_rules
                .iter()
                .map(|(key, rule)| (key.clone(), rule.action_sql.clone()))
                .collect();
            eval_session.compat_rules = Arc::new(rules_view);
            let tenant_filter = current_schema
                .as_deref()
                .filter(|schema| schema.starts_with("tenant_"))
                .map(str::to_owned);
            let mut compat_tables = Vec::new();
            let mut compat_views = Vec::new();
            for schema in self.catalog_reader.list_schemas(txn_id)? {
                if tenant_filter
                    .as_deref()
                    .is_some_and(|filter| !schema.name.eq_ignore_ascii_case(filter))
                {
                    continue;
                }
                compat_tables.extend(self.catalog_reader.list_tables(txn_id, schema.schema_id)?);
                compat_views.extend(self.catalog_reader.list_views(txn_id, schema.schema_id)?);
            }
            let compat_relation_schemas_by_oid = compat_tables
                .iter()
                .map(|table| {
                    (
                        compat_relation_oid(table.table_id),
                        table.name.schema_name().unwrap_or("public").to_ascii_lowercase(),
                    )
                })
                .chain(compat_views.iter().map(|view| {
                    (
                        compat_relation_oid(view.view_id),
                        view.name.schema_name().unwrap_or("public").to_ascii_lowercase(),
                    )
                }))
                .collect::<HashMap<_, _>>();
            let compat_relation_names_by_oid = compat_tables
                .iter()
                .map(|table| {
                    (
                        compat_relation_oid(table.table_id),
                        table.name.object_name().to_ascii_lowercase(),
                    )
                })
                .chain(compat_views.iter().map(|view| {
                    (
                        compat_relation_oid(view.view_id),
                        view.name.object_name().to_ascii_lowercase(),
                    )
                }))
                .collect::<HashMap<_, _>>();
            eval_session.compat_relation_schemas_by_oid =
                Arc::new(compat_relation_schemas_by_oid);
            eval_session.compat_relation_names_by_oid = Arc::new(compat_relation_names_by_oid);
            let table_lookup: HashMap<String, aiondb_catalog::TableDescriptor> = compat_tables
                .iter()
                .cloned()
                .flat_map(|table| {
                    let qualified = format_compat_qualified_name(&table.name).to_ascii_lowercase();
                    let unqualified = table.name.object_name().to_ascii_lowercase();
                    [(qualified, table.clone()), (unqualified, table)]
                })
                .collect();
            let mut compat_index_defs = HashMap::new();
            let mut compat_constraint_defs = HashMap::new();
            let mut constraint_oid: i32 = 100_000;
            for table in &compat_tables {
                let indexes = self.catalog_reader.list_indexes(txn_id, table.table_id)?;
                for index in &indexes {
                    let key_columns = index
                        .key_columns
                        .iter()
                        .filter_map(|key_col| {
                            table
                                .columns
                                .iter()
                                .find(|column| column.column_id == key_col.column_id)
                                .map(|column| column.name.clone())
                        })
                        .collect::<Vec<_>>()
                        .join(", ");
                    let unique = if index.unique { "UNIQUE " } else { "" };
                    compat_index_defs.insert(
                        compat_index_oid(index.index_id),
                        format!(
                            "CREATE {unique}INDEX {} ON {} USING btree ({key_columns})",
                            index.name.object_name(),
                            format_compat_qualified_name(&table.name)
                        ),
                    );
                }
                if let Some(pk_cols) = &table.primary_key {
                    let key_columns = pk_cols
                        .iter()
                        .filter_map(|column_id| {
                            table
                                .columns
                                .iter()
                                .find(|column| column.column_id == *column_id)
                                .map(|column| column.name.clone())
                        })
                        .collect::<Vec<_>>()
                        .join(", ");
                    compat_constraint_defs.insert(
                        constraint_oid,
                        format!("PRIMARY KEY ({key_columns})"),
                    );
                    constraint_oid += 1;
                }
                for index in &indexes {
                    let is_primary = table.primary_key.as_ref().is_some_and(|pk_cols| {
                        index.unique
                            && pk_cols.len() == index.key_columns.len()
                            && pk_cols
                                .iter()
                                .zip(index.key_columns.iter())
                                .all(|(pk_col, idx_col)| *pk_col == idx_col.column_id)
                    });
                    if index.unique && !is_primary && index.constraint_name.is_some() {
                        let key_columns = index
                            .key_columns
                            .iter()
                            .filter_map(|key_col| {
                                table
                                    .columns
                                    .iter()
                                    .find(|column| column.column_id == key_col.column_id)
                                    .map(|column| column.name.clone())
                            })
                            .collect::<Vec<_>>()
                            .join(", ");
                        compat_constraint_defs
                            .insert(constraint_oid, format!("UNIQUE ({key_columns})"));
                        constraint_oid += 1;
                    }
                }
                for fk in &table.foreign_keys {
                    let local_columns = fk.columns.join(", ");
                    let referenced_table = table_lookup
                        .get(&fk.referenced_table.to_ascii_lowercase())
                        .map(|table| format_compat_fk_reference_name(&table.name))
                        .unwrap_or_else(|| {
                            normalize_compat_fk_reference_text(&fk.referenced_table)
                        });
                    let referenced_columns = fk.referenced_columns.join(", ");
                    let mut def = format!(
                        "FOREIGN KEY ({local_columns}) REFERENCES {referenced_table}({referenced_columns})"
                    );
                    // Render MATCH / ON UPDATE / ON DELETE only when they
                    // diverge from PG's defaults (SIMPLE, NO ACTION) so the
                    // pretty form matches what `pg_get_constraintdef` emits.
                    match fk.match_type {
                        aiondb_core::FkMatchType::Full => def.push_str(" MATCH FULL"),
                        aiondb_core::FkMatchType::Partial => def.push_str(" MATCH PARTIAL"),
                        aiondb_core::FkMatchType::Simple => {}
                    }
                    fn fk_clause_label(action: &aiondb_core::FkAction) -> Option<&'static str> {
                        match action {
                            aiondb_core::FkAction::NoAction => None,
                            aiondb_core::FkAction::Restrict => Some("RESTRICT"),
                            aiondb_core::FkAction::Cascade => Some("CASCADE"),
                            aiondb_core::FkAction::SetNull => Some("SET NULL"),
                            aiondb_core::FkAction::SetDefault => Some("SET DEFAULT"),
                        }
                    }
                    if let Some(label) = fk_clause_label(&fk.on_update) {
                        def.push_str(" ON UPDATE ");
                        def.push_str(label);
                    }
                    if let Some(label) = fk_clause_label(&fk.on_delete) {
                        def.push_str(" ON DELETE ");
                        def.push_str(label);
                    }
                    compat_constraint_defs.insert(constraint_oid, def);
                    constraint_oid += 1;
                }
                // CHECK constraints render as `CHECK (<expr>)` and pg_constraint
                // surfaces them as contype='c'. Without this loop ORM probes
                // and pg_dump-style queries see empty pg_get_constraintdef.
                for check in &table.check_constraints {
                    compat_constraint_defs
                        .insert(constraint_oid, format!("CHECK ({})", check.expression));
                    constraint_oid += 1;
                }
            }
            for domain in self.catalog_reader.list_domains(txn_id)? {
                for check in &domain.constraints {
                    compat_constraint_defs
                        .insert(constraint_oid, format!("CHECK ({})", check.check_expr));
                    constraint_oid += 1;
                }
            }
            let compat_view_defs = compat_views
                .into_iter()
                .map(|view| (compat_relation_oid(view.view_id), view.query_sql))
                .collect::<HashMap<_, _>>();
            eval_session.compat_index_defs = Arc::new(compat_index_defs);
            eval_session.compat_constraint_defs = Arc::new(compat_constraint_defs);
            eval_session.compat_view_defs = Arc::new(compat_view_defs);
            set_global_compat_definition_caches(
                Arc::clone(&eval_session.compat_index_defs),
                Arc::clone(&eval_session.compat_constraint_defs),
            );
            let role_membership_grantors = self
                .compat_role_membership_dependencies
                .read()
                .grantor_tuples();
            eval_session.role_membership_grantors = Arc::new(role_membership_grantors);
            // ADR-0014 phase 4: snapshot cluster databases so
            // pg_catalog.pg_database returns one row per database.
            let cluster_databases: Vec<aiondb_eval::ClusterDatabaseSummary> = self
                .cluster_catalog
                .list_databases()
                .map(|list| {
                    list.into_iter()
                        .map(|d| aiondb_eval::ClusterDatabaseSummary {
                            id: d.id.get(),
                            name: d.name,
                            owner: d.owner,
                            encoding: d.encoding,
                            collate: d.collate,
                            ctype: d.ctype,
                            tablespace_oid: d.tablespace_id.map(|t| t.get()),
                            connection_limit: d.connection_limit,
                            is_template: d.is_template,
                            allow_connections: d.allow_connections,
                        })
                        .collect()
                })
                .unwrap_or_default();
            eval_session.cluster_databases = Arc::new(cluster_databases);
            Ok(eval_session)
        })
    }

    fn cached_role_names_by_oid(&self, txn_id: TxnId) -> DbResult<Arc<HashMap<i32, String>>> {
        let role_names_by_oid = self
            .catalog_reader
            .list_roles(txn_id)?
            .into_iter()
            .map(|role| (compat_role_oid(&role.name), role.name))
            .collect();
        Ok(Arc::new(role_names_by_oid))
    }

    pub(super) fn with_compat_eval_session<T>(
        &self,
        session: &SessionHandle,
        f: impl FnOnce() -> DbResult<T>,
    ) -> DbResult<T> {
        let already_active = COMPAT_SESSION_STACK.with(|stack| {
            stack
                .borrow()
                .last()
                .is_some_and(|active_session| active_session == session)
        });
        if already_active {
            return f();
        }

        let eval_session = self.compat_eval_session_context(session)?;
        let cancel_checker = self.session_cancellation_checker(session)?;
        let pending_notifications = Arc::new(Mutex::new(Vec::new()));
        let notify_sink: Arc<dyn NotifySink> = Arc::new(CompatNotifySink {
            bus: Arc::clone(self.notification_bus()),
            session: session.clone(),
            pending: Arc::clone(&pending_notifications),
        });
        let result = with_extension_registry(Arc::clone(&self.extension_registry), || {
            let _guard = CompatSessionGuard::enter(session);
            // Threading the cancellation checker into the eval layer lets
            // long-running scalar functions (e.g. pg_sleep) cooperatively
            // honor pgwire CancelRequest/SQLSTATE 57014.
            with_cancellation_checker(cancel_checker, || {
                with_notify_sink(notify_sink, || with_eval_session_context(eval_session, f))
            })
        });
        let value = result?;
        let notifications = {
            let mut pending = pending_notifications
                .lock()
                .map_err(|_| DbError::internal("notification sink lock poisoned"))?;
            std::mem::take(&mut *pending)
        };
        for (channel, payload) in notifications {
            self.enqueue_notification(session, &channel, &payload)?;
        }
        Ok(value)
    }
}
