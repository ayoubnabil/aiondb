use std::sync::Arc;

use aiondb_catalog::CatalogReader;
use aiondb_core::{
    compat_database_oid, compat_function_oid, compat_locale, compat_role_oid, compat_setting_value,
    DataType, DbResult, IntervalValue, TxnId, Value, COMPAT_BOOTSTRAP_ROLE_NAME,
    COMPAT_BOOTSTRAP_ROLE_OID, COMPAT_DEFAULT_DATABASE_NAME, COMPAT_PG_DEFAULT_TABLESPACE_OID,
    COMPAT_SERVER_VERSION,
};
use aiondb_plan::{LogicalPlan, ResultField, TypedExpr};

use super::*;
#[path = "extra_tables_settings_and_runtime.rs"]
mod extra_tables_settings_and_runtime;
pub(super) use self::extra_tables_settings_and_runtime::{
    build_pg_available_extension_versions_plan, build_pg_available_extensions_plan,
    build_pg_backend_memory_contexts_plan, build_pg_config_plan, build_pg_cursors_plan,
    build_pg_file_settings_plan, build_pg_hba_file_rules_plan, build_pg_ident_file_mappings_plan,
    build_pg_locks_plan, build_pg_prepared_statements_plan, build_pg_settings_plan,
    build_pg_stat_slru_plan, build_pg_stat_statements_plan, build_pg_stat_wal_plan,
    build_pg_stat_wal_receiver_plan, build_pg_timezone_abbrevs_plan, build_pg_timezone_names_plan,
    pg_available_extension_versions_fields, pg_available_extensions_fields,
    pg_backend_memory_contexts_fields, pg_config_fields, pg_cursors_fields,
    pg_file_settings_fields, pg_hba_file_rules_fields, pg_ident_file_mappings_fields,
    pg_locks_fields, pg_prepared_statements_fields, pg_settings_fields, pg_stat_slru_fields,
    pg_stat_statements_fields, pg_stat_user_indexes_fields, pg_stat_wal_fields,
    pg_stat_wal_receiver_fields, pg_statio_user_indexes_fields, pg_timezone_abbrevs_fields,
    pg_timezone_names_fields,
};
pub(super) use self::extra_tables_settings_and_runtime::{
    build_pg_stat_user_indexes_plan, build_pg_statio_user_indexes_plan,
};

pub(super) fn pg_am_fields() -> Vec<ResultField> {
    vec![
        oid_field("oid"),
        name_field("amname"),
        internal_char_field("amtype"),
        ResultField {
            name: "amhandler".to_owned(),
            data_type: DataType::Int,
            text_type_modifier: Some(TextTypeModifier::Oid),
            nullable: true,
        },
    ]
}

pub(super) fn build_pg_am_plan() -> DbResult<LogicalPlan> {
    let fields = pg_am_fields();
    let rows = vec![
        vec![
            int_literal(2),
            text_literal("heap"),
            text_literal("t"),
            null_literal(DataType::Int),
        ],
        vec![
            int_literal(403),
            text_literal("btree"),
            text_literal("i"),
            null_literal(DataType::Int),
        ],
        vec![
            int_literal(405),
            text_literal("hash"),
            text_literal("i"),
            null_literal(DataType::Int),
        ],
        vec![
            int_literal(783),
            text_literal("gist"),
            text_literal("i"),
            null_literal(DataType::Int),
        ],
        vec![
            int_literal(2742),
            text_literal("gin"),
            text_literal("i"),
            null_literal(DataType::Int),
        ],
        vec![
            int_literal(3580),
            text_literal("brin"),
            text_literal("i"),
            null_literal(DataType::Int),
        ],
        vec![
            int_literal(COMPAT_PGVECTOR_HNSW_AM_OID),
            text_literal("hnsw"),
            text_literal("i"),
            null_literal(DataType::Int),
        ],
        vec![
            int_literal(COMPAT_PGVECTOR_IVFFLAT_AM_OID),
            text_literal("ivfflat"),
            text_literal("i"),
            null_literal(DataType::Int),
        ],
    ];
    Ok(project_values(fields, rows))
}

pub(super) fn build_pg_indexes_plan(
    catalog: &Arc<dyn CatalogReader>,
    txn_id: TxnId,
    default_schema: Option<&str>,
) -> DbResult<LogicalPlan> {
    let fields = pg_indexes_fields();
    let tables = list_user_tables(catalog, txn_id, default_schema)?;
    let mut rows: Vec<Vec<TypedExpr>> = Vec::new();

    for table in &tables {
        let schema_name =
            visible_schema_name(table.name.schema_name().unwrap_or("public"), default_schema);
        let indexes = catalog.list_indexes(txn_id, table.table_id)?;
        for idx in &indexes {
            let key_cols: Vec<String> = idx
                .key_columns
                .iter()
                .filter_map(|kc| {
                    table
                        .columns
                        .iter()
                        .find(|c| c.column_id == kc.column_id)
                        .map(|c| c.name.clone())
                })
                .collect();
            let unique_str = if idx.unique { "UNIQUE " } else { "" };
            let method = match idx.kind {
                aiondb_catalog::IndexKind::BTree => "btree",
                aiondb_catalog::IndexKind::Hash => "hash",
                aiondb_catalog::IndexKind::GiST => "gist",
                aiondb_catalog::IndexKind::Gin => "gin",
                aiondb_catalog::IndexKind::Brin => "brin",
                aiondb_catalog::IndexKind::Hnsw => "hnsw",
                _ => "btree",
            };
            let indexdef = format!(
                "CREATE {}INDEX {} ON {}.{} USING {} ({})",
                unique_str,
                idx.name.object_name(),
                schema_name,
                table.name.object_name(),
                method,
                key_cols.join(", ")
            );

            rows.push(vec![
                text_literal(&schema_name),
                text_literal(table.name.object_name()),
                text_literal(idx.name.object_name()),
                null_literal(DataType::Text),
                text_literal(&indexdef),
            ]);
        }
    }

    Ok(project_values(fields, rows))
}

pub(super) fn pg_indexes_fields() -> Vec<ResultField> {
    vec![
        text_field("schemaname"),
        text_field("tablename"),
        text_field("indexname"),
        nullable_text_field("tablespace"),
        text_field("indexdef"),
    ]
}

pub(super) fn pg_user_fields() -> Vec<ResultField> {
    vec![
        text_field("usename"),
        oid_field("usesysid"),
        bool_field("usecreatedb"),
        bool_field("usesuper"),
        bool_field("userepl"),
        bool_field("usebypassrls"),
        nullable_text_field("passwd"),
        nullable_timestamp_field("valuntil"),
        ResultField {
            name: "useconfig".to_owned(),
            data_type: DataType::Array(Box::new(DataType::Text)),
            text_type_modifier: None,
            nullable: true,
        },
    ]
}

pub(super) fn build_pg_user_plan(
    catalog: &Arc<dyn CatalogReader>,
    txn_id: TxnId,
) -> DbResult<LogicalPlan> {
    let mut rows: Vec<Vec<TypedExpr>> = Vec::new();
    for role in catalog.list_roles(txn_id)? {
        if !role.login {
            continue;
        }
        let oid = compat_role_oid(&role.name);
        rows.push(vec![
            text_literal(&role.name),
            int_literal(oid),
            bool_literal(role.createdb),
            bool_literal(role.superuser),
            bool_literal(role.replication),
            bool_literal(role.bypassrls),
            text_literal("********"),
            null_literal(DataType::TimestampTz),
            null_literal(DataType::Array(Box::new(DataType::Text))),
        ]);
    }
    Ok(project_values(pg_user_fields(), rows))
}

pub(super) fn pg_shadow_fields() -> Vec<ResultField> {
    pg_user_fields()
}

pub(super) fn build_pg_shadow_plan(
    catalog: &Arc<dyn CatalogReader>,
    txn_id: TxnId,
) -> DbResult<LogicalPlan> {
    build_pg_user_plan(catalog, txn_id)
}

pub(super) fn pg_replication_slots_fields() -> Vec<ResultField> {
    vec![
        text_field("slot_name"),
        text_field("plugin"),
        text_field("slot_type"),
        oid_field("datoid"),
        text_field("database"),
        bool_field("temporary"),
        bool_field("active"),
        nullable_int_field("active_pid"),
        nullable_int_field("xmin"),
        nullable_int_field("catalog_xmin"),
        nullable_text_field("restart_lsn"),
        nullable_text_field("confirmed_flush_lsn"),
        nullable_text_field("wal_status"),
        nullable_int_field("safe_wal_size"),
        bool_field("two_phase"),
        nullable_text_field("conflicting"),
    ]
}

pub(super) fn build_pg_replication_slots_plan() -> DbResult<LogicalPlan> {
    Ok(project_values(pg_replication_slots_fields(), Vec::new()))
}

pub(super) fn pg_stat_replication_fields() -> Vec<ResultField> {
    vec![
        nullable_int_field("pid"),
        oid_field("usesysid"),
        text_field("usename"),
        text_field("application_name"),
        nullable_text_field("client_addr"),
        nullable_text_field("client_hostname"),
        nullable_int_field("client_port"),
        nullable_timestamp_field("backend_start"),
        nullable_text_field("backend_xmin"),
        text_field("state"),
        nullable_text_field("sent_lsn"),
        nullable_text_field("write_lsn"),
        nullable_text_field("flush_lsn"),
        nullable_text_field("replay_lsn"),
        nullable_text_field("write_lag"),
        nullable_text_field("flush_lag"),
        nullable_text_field("replay_lag"),
        nullable_int_field("sync_priority"),
        text_field("sync_state"),
        nullable_timestamp_field("reply_time"),
    ]
}

pub(super) fn build_pg_stat_replication_plan() -> DbResult<LogicalPlan> {
    Ok(project_values(pg_stat_replication_fields(), Vec::new()))
}

pub(super) fn pg_replication_origin_fields() -> Vec<ResultField> {
    vec![oid_field("roident"), text_field("roname")]
}

pub(super) fn build_pg_replication_origin_plan() -> DbResult<LogicalPlan> {
    Ok(project_values(pg_replication_origin_fields(), Vec::new()))
}

pub(super) fn pg_tables_fields() -> Vec<ResultField> {
    vec![
        text_field("schemaname"),
        text_field("tablename"),
        text_field("tableowner"),
        nullable_text_field("tablespace"),
        bool_field("hasindexes"),
        bool_field("hasrules"),
        bool_field("hastriggers"),
        bool_field("rowsecurity"),
    ]
}

pub(super) fn build_pg_tables_plan(
    catalog: &Arc<dyn CatalogReader>,
    txn_id: TxnId,
    default_schema: Option<&str>,
) -> DbResult<LogicalPlan> {
    let tenant_filter = super::tenant_schema_filter(default_schema);
    let owner_name = COMPAT_BOOTSTRAP_ROLE_NAME.to_owned();
    let mut rows: Vec<Vec<TypedExpr>> = Vec::new();
    for schema in catalog.list_schemas(txn_id)? {
        if !super::schema_visible_with_tenant_filter(&schema.name, tenant_filter.as_deref()) {
            continue;
        }
        for table in catalog.list_tables(txn_id, schema.schema_id)? {
            let has_indexes = !catalog.list_indexes(txn_id, table.table_id)?.is_empty();
            rows.push(vec![
                text_literal(&schema.name),
                text_literal(table.name.object_name()),
                text_literal(&owner_name),
                null_literal(DataType::Text),
                bool_literal(has_indexes),
                bool_literal(false),
                bool_literal(false),
                bool_literal(false),
            ]);
        }
    }
    Ok(project_values(pg_tables_fields(), rows))
}

pub(super) fn pg_views_fields() -> Vec<ResultField> {
    vec![
        text_field("schemaname"),
        text_field("viewname"),
        text_field("viewowner"),
        text_field("definition"),
    ]
}

pub(super) fn build_pg_views_plan(
    catalog: &Arc<dyn CatalogReader>,
    txn_id: TxnId,
    default_schema: Option<&str>,
) -> DbResult<LogicalPlan> {
    let tenant_filter = super::tenant_schema_filter(default_schema);
    let owner_oid = COMPAT_BOOTSTRAP_ROLE_OID;
    let owner_name = if owner_oid == COMPAT_BOOTSTRAP_ROLE_OID {
        COMPAT_BOOTSTRAP_ROLE_NAME.to_owned()
    } else {
        String::new()
    };
    let mut rows: Vec<Vec<TypedExpr>> = Vec::new();
    for schema in catalog.list_schemas(txn_id)? {
        if !super::schema_visible_with_tenant_filter(&schema.name, tenant_filter.as_deref()) {
            continue;
        }
        for view in catalog.list_views(txn_id, schema.schema_id)? {
            let definition = view.query_sql.trim_end_matches(';').to_owned();
            rows.push(vec![
                text_literal(&schema.name),
                text_literal(&view.name.object_name()),
                text_literal(&owner_name),
                text_literal(&definition),
            ]);
        }
    }
    Ok(project_values(pg_views_fields(), rows))
}

// ---------------------------------------------------------------
// pg_catalog.pg_stat_all_tables
// ---------------------------------------------------------------

pub(super) fn pg_stat_all_tables_fields() -> Vec<ResultField> {
    vec![
        oid_field("relid"),
        text_field("schemaname"),
        text_field("relname"),
        bigint_field("seq_scan"),
        bigint_field("seq_tup_read"),
        nullable_int_field("idx_scan"),
        nullable_int_field("idx_tup_fetch"),
        bigint_field("n_tup_ins"),
        bigint_field("n_tup_upd"),
        bigint_field("n_tup_del"),
        bigint_field("n_live_tup"),
        bigint_field("n_dead_tup"),
        nullable_timestamp_field("last_vacuum"),
        nullable_timestamp_field("last_autovacuum"),
        nullable_timestamp_field("last_analyze"),
        nullable_timestamp_field("last_autoanalyze"),
        nullable_timestamp_field("last_seq_scan"),
        nullable_timestamp_field("last_idx_scan"),
        bigint_field("vacuum_count"),
        bigint_field("autovacuum_count"),
        bigint_field("analyze_count"),
        bigint_field("autoanalyze_count"),
    ]
}

pub(super) fn build_pg_stat_all_tables_plan(
    catalog: &Arc<dyn CatalogReader>,
    txn_id: TxnId,
    default_schema: Option<&str>,
) -> DbResult<LogicalPlan> {
    let mut rows = Vec::new();
    for table in list_user_tables(catalog, txn_id, default_schema)? {
        let relid = i32::try_from(table.table_id.get()).unwrap_or(i32::MAX);
        let schema_name =
            visible_schema_name(table.name.schema_name().unwrap_or("public"), default_schema);
        rows.push(vec![
            int_literal(relid),
            text_literal(&schema_name),
            text_literal(table.name.object_name()),
            bigint_literal(0),
            bigint_literal(0),
            int_literal(0),
            int_literal(0),
            bigint_literal(0),
            bigint_literal(0),
            bigint_literal(0),
            bigint_literal(0),
            bigint_literal(0),
            null_literal(DataType::TimestampTz),
            null_literal(DataType::TimestampTz),
            null_literal(DataType::TimestampTz),
            null_literal(DataType::TimestampTz),
            null_literal(DataType::TimestampTz),
            null_literal(DataType::TimestampTz),
            bigint_literal(0),
            bigint_literal(0),
            bigint_literal(0),
            bigint_literal(0),
        ]);
    }
    Ok(project_values(pg_stat_all_tables_fields(), rows))
}

pub(super) fn pg_stat_user_tables_fields() -> Vec<ResultField> {
    pg_stat_all_tables_fields()
}

pub(super) fn build_pg_stat_user_tables_plan(
    catalog: &Arc<dyn CatalogReader>,
    txn_id: TxnId,
    default_schema: Option<&str>,
) -> DbResult<LogicalPlan> {
    build_pg_stat_all_tables_plan(catalog, txn_id, default_schema)
}

pub(super) fn pg_statio_user_tables_fields() -> Vec<ResultField> {
    vec![
        oid_field("relid"),
        text_field("schemaname"),
        text_field("relname"),
        bigint_field("heap_blks_read"),
        bigint_field("heap_blks_hit"),
        bigint_field("idx_blks_read"),
        bigint_field("idx_blks_hit"),
        bigint_field("toast_blks_read"),
        bigint_field("toast_blks_hit"),
        bigint_field("tidx_blks_read"),
        bigint_field("tidx_blks_hit"),
    ]
}

pub(super) fn pg_statio_all_tables_fields() -> Vec<ResultField> {
    pg_statio_user_tables_fields()
}

pub(super) fn build_pg_statio_all_tables_plan(
    catalog: &Arc<dyn CatalogReader>,
    txn_id: TxnId,
    default_schema: Option<&str>,
) -> DbResult<LogicalPlan> {
    let mut rows = Vec::new();
    for table in list_user_tables(catalog, txn_id, default_schema)? {
        let relid = i32::try_from(table.table_id.get()).unwrap_or(i32::MAX);
        let schema_name =
            visible_schema_name(table.name.schema_name().unwrap_or("public"), default_schema);
        rows.push(vec![
            int_literal(relid),
            text_literal(&schema_name),
            text_literal(table.name.object_name()),
            bigint_literal(0),
            bigint_literal(0),
            bigint_literal(0),
            bigint_literal(0),
            bigint_literal(0),
            bigint_literal(0),
            bigint_literal(0),
            bigint_literal(0),
        ]);
    }
    Ok(project_values(pg_statio_user_tables_fields(), rows))
}

pub(super) fn build_pg_statio_user_tables_plan(
    catalog: &Arc<dyn CatalogReader>,
    txn_id: TxnId,
    default_schema: Option<&str>,
) -> DbResult<LogicalPlan> {
    build_pg_statio_all_tables_plan(catalog, txn_id, default_schema)
}

pub(super) fn pg_stat_user_functions_fields() -> Vec<ResultField> {
    vec![
        oid_field("funcid"),
        text_field("schemaname"),
        text_field("funcname"),
        bigint_field("calls"),
        nullable_double_field("total_time"),
        nullable_double_field("self_time"),
    ]
}

pub(super) fn build_pg_stat_user_functions_plan(
    catalog: &Arc<dyn CatalogReader>,
    txn_id: TxnId,
) -> DbResult<LogicalPlan> {
    let mut rows = Vec::new();
    for function in catalog.list_functions(txn_id)? {
        let schema = function
            .name
            .split_once('.')
            .map_or("public", |(schema, _)| schema);
        let funcname = function
            .name
            .split('.')
            .next_back()
            .unwrap_or(function.name.as_str());
        rows.push(vec![
            int_literal(compat_function_oid(&compat_function_signature(&function))),
            text_literal(schema),
            text_literal(funcname),
            bigint_literal(0),
            null_literal(DataType::Double),
            null_literal(DataType::Double),
        ]);
    }
    Ok(project_values(pg_stat_user_functions_fields(), rows))
}

pub(super) fn pg_stat_database_fields() -> Vec<ResultField> {
    vec![
        oid_field("datid"),
        text_field("datname"),
        bigint_field("sessions"),
        nullable_timestamp_field("stats_reset"),
    ]
}

pub(super) fn build_pg_stat_database_plan(
    database_name: Option<&str>,
    _owner_oid: i32,
) -> DbResult<LogicalPlan> {
    let db_name = database_name.unwrap_or(COMPAT_DEFAULT_DATABASE_NAME);
    let rows = vec![vec![
        int_literal(compat_database_oid(db_name)),
        text_literal(db_name),
        bigint_literal(1),
        null_literal(DataType::TimestampTz),
    ]];
    Ok(project_values(pg_stat_database_fields(), rows))
}

pub(super) fn pg_stat_bgwriter_fields() -> Vec<ResultField> {
    vec![
        bigint_field("checkpoints_req"),
        nullable_timestamp_field("stats_reset"),
    ]
}

pub(super) fn build_pg_stat_bgwriter_plan() -> DbResult<LogicalPlan> {
    let rows = vec![vec![bigint_literal(0), null_literal(DataType::TimestampTz)]];
    Ok(project_values(pg_stat_bgwriter_fields(), rows))
}

pub(super) fn pg_stat_archiver_fields() -> Vec<ResultField> {
    vec![nullable_timestamp_field("stats_reset")]
}

pub(super) fn build_pg_stat_archiver_plan() -> DbResult<LogicalPlan> {
    let rows = vec![vec![null_literal(DataType::TimestampTz)]];
    Ok(project_values(pg_stat_archiver_fields(), rows))
}

pub(super) fn pg_stat_io_fields() -> Vec<ResultField> {
    vec![
        text_field("backend_type"),
        text_field("object"),
        text_field("context"),
        bigint_field("reads"),
        bigint_field("writes"),
        bigint_field("writebacks"),
        bigint_field("extends"),
        bigint_field("hits"),
        bigint_field("evictions"),
        bigint_field("reuses"),
        bigint_field("fsyncs"),
    ]
}

pub(super) fn build_pg_stat_io_plan() -> DbResult<LogicalPlan> {
    let rows = vec![vec![
        text_literal("client backend"),
        text_literal("relation"),
        text_literal("normal"),
        bigint_literal(0),
        bigint_literal(0),
        bigint_literal(0),
        bigint_literal(0),
        bigint_literal(0),
        bigint_literal(0),
        bigint_literal(0),
        bigint_literal(0),
    ]];
    Ok(project_values(pg_stat_io_fields(), rows))
}

// ---------------------------------------------------------------
// pg_catalog.pg_stats
// ---------------------------------------------------------------

pub(super) fn pg_stats_fields() -> Vec<ResultField> {
    vec![
        text_field("schemaname"),
        text_field("tablename"),
        text_field("attname"),
        bool_field("inherited"),
        nullable_double_field("null_frac"),
        nullable_int_field("avg_width"),
        nullable_double_field("n_distinct"),
        nullable_text_field("most_common_vals"),
        nullable_text_field("most_common_freqs"),
        ResultField {
            name: "histogram_bounds".to_owned(),
            data_type: DataType::Array(Box::new(DataType::Text)),
            text_type_modifier: None,
            nullable: true,
        },
        nullable_double_field("correlation"),
        nullable_text_field("most_common_elems"),
        nullable_text_field("most_common_elem_freqs"),
        nullable_text_field("elem_count_histogram"),
    ]
}

pub(super) fn build_pg_stats_plan() -> DbResult<LogicalPlan> {
    let rows = vec![
        vec![
            text_literal("pg_catalog"),
            text_literal("pg_am"),
            text_literal("oid"),
            bool_literal(false),
            null_literal(DataType::Double),
            null_literal(DataType::Int),
            null_literal(DataType::Double),
            null_literal(DataType::Text),
            null_literal(DataType::Text),
            typed_array_literal(
                vec![Value::Int(2), Value::Int(403), Value::Int(405)],
                DataType::Int,
            ),
            null_literal(DataType::Double),
            null_literal(DataType::Text),
            null_literal(DataType::Text),
            null_literal(DataType::Text),
        ],
        vec![
            text_literal("pg_catalog"),
            text_literal("pg_am"),
            text_literal("amname"),
            bool_literal(false),
            null_literal(DataType::Double),
            null_literal(DataType::Int),
            null_literal(DataType::Double),
            null_literal(DataType::Text),
            null_literal(DataType::Text),
            typed_array_literal(
                vec![
                    Value::Interval(IntervalValue::new(0, 1, 0)),
                    Value::Interval(IntervalValue::new(0, 2, 0)),
                ],
                DataType::Interval,
            ),
            null_literal(DataType::Double),
            null_literal(DataType::Text),
            null_literal(DataType::Text),
            null_literal(DataType::Text),
        ],
    ];
    Ok(project_values(pg_stats_fields(), rows))
}

pub(super) fn typed_array_literal(elements: Vec<Value>, element_type: DataType) -> TypedExpr {
    TypedExpr::literal(
        Value::Array(elements),
        DataType::Array(Box::new(element_type)),
        false,
    )
}

fn compat_function_signature(function: &aiondb_catalog::FunctionDescriptor) -> String {
    let normalized_name = function.name.to_ascii_lowercase();
    let arg_types = function
        .params
        .iter()
        .map(|param| {
            let raw = param
                .raw_type_name
                .as_deref()
                .unwrap_or_else(|| param.data_type.pg_type_name());
            aiondb_eval::normalize_compat_type_name(raw)
        })
        .collect::<Vec<_>>()
        .join(",");
    format!("{normalized_name}({arg_types})")
}

// ---------------------------------------------------------------
// pg_catalog.pg_rules
// ---------------------------------------------------------------

pub(super) fn pg_rules_fields() -> Vec<ResultField> {
    vec![
        text_field("schemaname"),
        text_field("tablename"),
        text_field("rulename"),
        text_field("definition"),
    ]
}

/// Must match the constant in
/// `aiondb-engine/src/engine/compat/statement_rewrite.rs`: rule-name
/// registry entries are keyed by `("__aiondb_rule_name_registry__.<rel>",
/// "<rule_name>") → ""`. We duplicate it here because the planner crate
/// has no dependency on the engine.
const RULE_NAME_REGISTRY_PREFIX: &str = "__aiondb_rule_name_registry__.";

pub(super) fn build_pg_rules_plan() -> DbResult<LogicalPlan> {
    // Two built-in rules that always show up, matching PG's baseline.
    let mut rows = vec![
        vec![
            text_literal("pg_catalog"),
            text_literal("pg_settings"),
            text_literal("pg_settings_n"),
            text_literal(
                "CREATE RULE pg_settings_n AS\n    ON UPDATE TO pg_catalog.pg_settings DO INSTEAD NOTHING;",
            ),
        ],
        vec![
            text_literal("pg_catalog"),
            text_literal("pg_settings"),
            text_literal("pg_settings_u"),
            text_literal(
                "CREATE RULE pg_settings_u AS\n    ON UPDATE TO pg_catalog.pg_settings\n   WHERE (new.name = old.name) DO  SELECT set_config(old.name, new.setting, false) AS set_config;",
            ),
        ],
    ];

    // Now walk session.compat_rules and emit a row for each stored rewrite
    // rule. The registry has two entry shapes:
    //   * `(relation_lowercase, EVENT_UPPERCASE) → action_sql`   - real
    //     rewrite entries that drive query rewriting.
    //   * `(RULE_NAME_REGISTRY_PREFIX + relation, rule_name) → ""`  -
    //     name-registry entries that record the user-chosen rule name.
    // We use the registry entries to map (relation → rule_name) and then
    // emit one row per real rewrite entry, falling back to a synthesized
    // name if no registry entry exists.
    let session_rows = aiondb_eval::with_current_session_context(|context| {
        let mut rule_names_by_relation: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        for ((key0, key1), _) in context.compat_rules.iter() {
            if let Some(relation) = key0.strip_prefix(RULE_NAME_REGISTRY_PREFIX) {
                rule_names_by_relation
                    .entry(relation.to_owned())
                    .or_insert_with(|| key1.clone());
            }
        }

        let mut emitted: Vec<Vec<TypedExpr>> = Vec::new();
        for ((relation, event), action_sql) in context.compat_rules.iter() {
            if relation.starts_with(RULE_NAME_REGISTRY_PREFIX) {
                continue;
            }
            if action_sql.is_empty() {
                // A rule was declared but the action is tracked elsewhere
                // (RETURNING-only entries, etc.). Skip emitting rather than
                // fabricating a misleading DO INSTEAD line.
                continue;
            }
            let rule_name = rule_names_by_relation
                .get(relation)
                .cloned()
                .unwrap_or_else(|| format!("aiondb_{relation}_{}", event.to_ascii_lowercase()));
            let definition = format!(
                "CREATE RULE {rule_name} AS\n    ON {event} TO {relation} DO INSTEAD {};",
                action_sql.trim_end_matches(';').trim(),
            );
            emitted.push(vec![
                text_literal("public"),
                text_literal(relation.as_str()),
                text_literal(rule_name.as_str()),
                text_literal(definition.as_str()),
            ]);
        }
        emitted
    });

    rows.extend(session_rows);
    Ok(project_values(pg_rules_fields(), rows))
}

// ---------------------------------------------------------------
// pg_catalog.pg_seclabel
// ---------------------------------------------------------------

pub(super) fn pg_seclabel_fields() -> Vec<ResultField> {
    vec![
        oid_field("objoid"),
        oid_field("classoid"),
        int_field("objsubid"),
        text_field("provider"),
        text_field("label"),
    ]
}

pub(super) fn build_pg_seclabel_plan() -> DbResult<LogicalPlan> {
    let rows = aiondb_eval::with_current_session_context(|context| {
        context
            .compat_security_labels
            .iter()
            .map(|((object_type, subject), (provider, label))| {
                let classid = compat_description_classoid(object_type);
                let objoid = compat_description_objoid(object_type, subject);
                vec![
                    int_literal(objoid),
                    int_literal(classid),
                    int_literal(0),
                    text_literal(provider.as_deref().unwrap_or("none")),
                    text_literal(label),
                ]
            })
            .collect::<Vec<_>>()
    });
    Ok(project_values(pg_seclabel_fields(), rows))
}

// ---------------------------------------------------------------
// pg_catalog.pg_compat_object_attrs (AionDB-specific)
// ---------------------------------------------------------------

pub(super) fn pg_compat_object_attrs_fields() -> Vec<ResultField> {
    vec![
        text_field("object_kind"),
        text_field("object_name"),
        nullable_text_field("owner"),
        nullable_text_field("schema"),
        nullable_text_field("state"),
        nullable_text_field("options"),
        nullable_text_field("tablespace"),
        nullable_text_field("version"),
    ]
}

pub(super) fn build_pg_compat_object_attrs_plan() -> DbResult<LogicalPlan> {
    let rows = aiondb_eval::with_current_session_context(|context| {
        context
            .compat_misc_attrs
            .iter()
            .map(
                |((kind, name), (owner, schema, state, options, tablespace, version))| {
                    let to_nullable = |s: &str| {
                        if s.is_empty() {
                            null_literal(DataType::Text)
                        } else {
                            text_literal(s)
                        }
                    };
                    vec![
                        text_literal(kind),
                        text_literal(name),
                        to_nullable(owner),
                        to_nullable(schema),
                        to_nullable(state),
                        to_nullable(options),
                        to_nullable(tablespace),
                        to_nullable(version),
                    ]
                },
            )
            .collect::<Vec<_>>()
    });
    Ok(project_values(pg_compat_object_attrs_fields(), rows))
}

// ---------------------------------------------------------------
// pg_catalog.pg_compat_trigger_state (AionDB-specific)
// ---------------------------------------------------------------

pub(super) fn pg_compat_trigger_state_fields() -> Vec<ResultField> {
    vec![
        text_field("tablename"),
        text_field("trigger_name"),
        text_field("state"),
    ]
}

pub(super) fn build_pg_compat_trigger_state_plan() -> DbResult<LogicalPlan> {
    let rows = aiondb_eval::with_current_session_context(|context| {
        context
            .compat_trigger_state
            .iter()
            .map(|((table, trigger), state)| {
                vec![
                    text_literal(table),
                    text_literal(trigger),
                    text_literal(state),
                ]
            })
            .collect::<Vec<_>>()
    });
    Ok(project_values(pg_compat_trigger_state_fields(), rows))
}

// ---------------------------------------------------------------
// pg_catalog.pg_auth_members
// ---------------------------------------------------------------

pub(super) fn pg_auth_members_fields() -> Vec<ResultField> {
    vec![
        oid_field("oid"),
        oid_field("roleid"),
        oid_field("member"),
        oid_field("grantor"),
        bool_field("admin_option"),
        bool_field("inherit_option"),
        bool_field("set_option"),
    ]
}

pub(super) fn build_pg_auth_members_plan(
    catalog: &Arc<dyn CatalogReader>,
    txn_id: TxnId,
) -> DbResult<LogicalPlan> {
    let fields = pg_auth_members_fields();
    let roles = catalog.list_roles(txn_id)?;
    let mut rows: Vec<Vec<TypedExpr>> = Vec::new();
    let mut memberships: std::collections::BTreeMap<(String, String), bool> =
        std::collections::BTreeMap::new();
    let membership_grantors: std::collections::HashMap<(String, String), String> =
        aiondb_eval::with_current_session_context(|context| {
            context
                .role_membership_grantors
                .iter()
                .map(|(granted_role, grantee, grantor)| {
                    (
                        (
                            granted_role.to_ascii_lowercase(),
                            grantee.to_ascii_lowercase(),
                        ),
                        grantor.clone(),
                    )
                })
                .collect()
        });
    for role in &roles {
        let member_name = role.name.clone();
        let privileges = catalog.get_privileges(txn_id, &member_name)?;
        for priv_desc in privileges {
            let aiondb_catalog::PrivilegeTarget::Role(parent_role) = priv_desc.target else {
                continue;
            };
            let pair = (
                parent_role.to_ascii_lowercase(),
                member_name.to_ascii_lowercase(),
            );
            let has_admin = priv_desc.privilege == aiondb_catalog::CatalogPrivilege::All;
            memberships
                .entry(pair)
                .and_modify(|admin| *admin = *admin || has_admin)
                .or_insert(has_admin);
        }
    }
    for ((parent_role, member_name), admin_option) in memberships {
        let roleid = compat_role_oid(&parent_role);
        let memberid = compat_role_oid(&member_name);
        let grantor_oid = membership_grantors
            .get(&(parent_role.clone(), member_name.clone()))
            .map(|grantor| compat_role_oid(grantor))
            .unwrap_or(COMPAT_BOOTSTRAP_ROLE_OID);
        rows.push(vec![
            int_literal(0),           // oid (pg_auth_members has no stable oid in AionDB)
            int_literal(roleid),      // roleid
            int_literal(memberid),    // member
            int_literal(grantor_oid), // grantor
            bool_literal(admin_option),
            bool_literal(true), // inherit_option
            bool_literal(true), // set_option
        ]);
    }
    Ok(project_values(fields, rows))
}

// ---------------------------------------------------------------
// pg_catalog.pg_prepared_xacts
// ---------------------------------------------------------------

pub(super) fn pg_prepared_xacts_fields() -> Vec<ResultField> {
    vec![
        bigint_field("transaction"),
        text_field("gid"),
        nullable_timestamp_field("prepared"),
        text_field("owner"),
        text_field("database"),
    ]
}

pub(super) fn build_pg_prepared_xacts_plan() -> DbResult<LogicalPlan> {
    Ok(project_values(pg_prepared_xacts_fields(), Vec::new()))
}

// ---------------------------------------------------------------
// pg_catalog.pg_ts_config
// ---------------------------------------------------------------

pub(super) fn pg_ts_config_fields() -> Vec<ResultField> {
    vec![
        oid_field("oid"),
        name_field("cfgname"),
        oid_field("cfgnamespace"),
        oid_field("cfgowner"),
        oid_field("cfgparser"),
    ]
}

pub(super) fn build_pg_ts_config_plan(
    catalog: &Arc<dyn CatalogReader>,
    txn_id: TxnId,
) -> DbResult<LogicalPlan> {
    let rows = aiondb_eval::with_current_session_context(|context| {
        let mut rows = Vec::new();
        for ((kind, canonical_name), (owner, schema, _, options_joined, _, _)) in
            context.compat_misc_attrs.iter()
        {
            if kind != "CREATE TEXT SEARCH" {
                continue;
            }
            let Some(("configuration", object_name)) =
                compat_text_search_object_parts(canonical_name)
            else {
                continue;
            };
            let options = parse_compat_options_joined_local(options_joined);
            let (schema_name, cfg_name) =
                compat_text_search_schema_and_name(object_name, schema.as_str());
            let parser_name = options
                .iter()
                .find(|(key, _)| key == "parser" || key == "copy")
                .map(|(_, value)| value.as_str())
                .unwrap_or("");
            rows.push(vec![
                int_literal(synth_catalog_oid_from_name(canonical_name)),
                text_literal(&cfg_name),
                int_literal(compat_text_search_namespace_oid(
                    catalog,
                    txn_id,
                    &schema_name,
                )),
                int_literal(compat_text_search_owner_oid(owner)),
                int_literal(compat_text_search_function_oid(parser_name)),
            ]);
        }
        rows
    });
    Ok(project_values(pg_ts_config_fields(), rows))
}

// ---------------------------------------------------------------
// pg_catalog.pg_ts_dict
// ---------------------------------------------------------------

pub(super) fn pg_ts_dict_fields() -> Vec<ResultField> {
    vec![
        oid_field("oid"),
        name_field("dictname"),
        oid_field("dictnamespace"),
        oid_field("dictowner"),
        oid_field("dicttemplate"),
        nullable_text_field("dictinitoption"),
    ]
}

pub(super) fn build_pg_ts_dict_plan(
    catalog: &Arc<dyn CatalogReader>,
    txn_id: TxnId,
) -> DbResult<LogicalPlan> {
    let rows = aiondb_eval::with_current_session_context(|context| {
        let mut rows = Vec::new();
        for ((kind, canonical_name), (owner, schema, _, options_joined, _, _)) in
            context.compat_misc_attrs.iter()
        {
            if kind != "CREATE TEXT SEARCH" {
                continue;
            }
            let Some(("dictionary", object_name)) = compat_text_search_object_parts(canonical_name)
            else {
                continue;
            };
            let options = parse_compat_options_joined_local(options_joined);
            let (schema_name, dict_name) =
                compat_text_search_schema_and_name(object_name, schema.as_str());
            let template_name = options
                .iter()
                .find(|(key, _)| key == "template")
                .map(|(_, value)| value.as_str())
                .unwrap_or("simple");
            let init_options = compat_text_search_non_template_options(&options);
            rows.push(vec![
                int_literal(synth_catalog_oid_from_name(canonical_name)),
                text_literal(&dict_name),
                int_literal(compat_text_search_namespace_oid(
                    catalog,
                    txn_id,
                    &schema_name,
                )),
                int_literal(compat_text_search_owner_oid(owner)),
                int_literal(compat_text_search_template_oid(template_name)),
                init_options.map_or_else(
                    || null_literal(DataType::Text),
                    |value| text_literal(&value),
                ),
            ]);
        }
        rows
    });
    Ok(project_values(pg_ts_dict_fields(), rows))
}

// ---------------------------------------------------------------
// pg_catalog.pg_ts_parser
// ---------------------------------------------------------------

pub(super) fn pg_ts_parser_fields() -> Vec<ResultField> {
    vec![
        oid_field("oid"),
        name_field("prsname"),
        oid_field("prsnamespace"),
        oid_field("prsstart"),
        oid_field("prstoken"),
        oid_field("prsend"),
        oid_field("prsheadline"),
        oid_field("prslextype"),
    ]
}

pub(super) fn build_pg_ts_parser_plan(
    catalog: &Arc<dyn CatalogReader>,
    txn_id: TxnId,
) -> DbResult<LogicalPlan> {
    let rows = aiondb_eval::with_current_session_context(|context| {
        let mut rows = Vec::new();
        for ((kind, canonical_name), (_, schema, _, options_joined, _, _)) in
            context.compat_misc_attrs.iter()
        {
            if kind != "CREATE TEXT SEARCH" {
                continue;
            }
            let Some(("parser", object_name)) = compat_text_search_object_parts(canonical_name)
            else {
                continue;
            };
            let options = parse_compat_options_joined_local(options_joined);
            let (schema_name, parser_name) =
                compat_text_search_schema_and_name(object_name, schema.as_str());
            rows.push(vec![
                int_literal(synth_catalog_oid_from_name(canonical_name)),
                text_literal(&parser_name),
                int_literal(compat_text_search_namespace_oid(
                    catalog,
                    txn_id,
                    &schema_name,
                )),
                int_literal(compat_text_search_function_oid(
                    compat_text_search_option_value(&options, "start"),
                )),
                int_literal(compat_text_search_function_oid(
                    compat_text_search_option_value(&options, "gettoken"),
                )),
                int_literal(compat_text_search_function_oid(
                    compat_text_search_option_value(&options, "end"),
                )),
                int_literal(compat_text_search_function_oid(
                    compat_text_search_option_value(&options, "headline"),
                )),
                int_literal(compat_text_search_function_oid(
                    compat_text_search_option_value(&options, "lextypes"),
                )),
            ]);
        }
        rows
    });
    Ok(project_values(pg_ts_parser_fields(), rows))
}

// ---------------------------------------------------------------
// pg_catalog.pg_ts_template
// ---------------------------------------------------------------

pub(super) fn pg_ts_template_fields() -> Vec<ResultField> {
    vec![
        oid_field("oid"),
        name_field("tmplname"),
        oid_field("tmplnamespace"),
        oid_field("tmplinit"),
        oid_field("tmpllexize"),
    ]
}

pub(super) fn build_pg_ts_template_plan(
    catalog: &Arc<dyn CatalogReader>,
    txn_id: TxnId,
) -> DbResult<LogicalPlan> {
    let rows = aiondb_eval::with_current_session_context(|context| {
        let mut rows = vec![vec![
            int_literal(compat_text_search_template_oid("simple")),
            text_literal("simple"),
            int_literal(PG_CATALOG_NAMESPACE_OID),
            int_literal(0),
            int_literal(0),
        ]];
        for ((kind, canonical_name), (_, schema, _, options_joined, _, _)) in
            context.compat_misc_attrs.iter()
        {
            if kind != "CREATE TEXT SEARCH" {
                continue;
            }
            let Some(("template", object_name)) = compat_text_search_object_parts(canonical_name)
            else {
                continue;
            };
            let options = parse_compat_options_joined_local(options_joined);
            let (schema_name, template_name) =
                compat_text_search_schema_and_name(object_name, schema.as_str());
            rows.push(vec![
                int_literal(synth_catalog_oid_from_name(canonical_name)),
                text_literal(&template_name),
                int_literal(compat_text_search_namespace_oid(
                    catalog,
                    txn_id,
                    &schema_name,
                )),
                int_literal(compat_text_search_function_oid(
                    compat_text_search_option_value(&options, "init"),
                )),
                int_literal(compat_text_search_function_oid(
                    compat_text_search_option_value(&options, "lexize"),
                )),
            ]);
        }
        rows
    });
    Ok(project_values(pg_ts_template_fields(), rows))
}

fn synth_catalog_oid_from_name(name: &str) -> i32 {
    let mut hash: u32 = 0x811c_9dc5;
    for byte in name.bytes() {
        hash ^= u32::from(byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    i32::try_from((hash & 0x7fff_ffff) | 0x8000).unwrap_or(i32::MAX)
}

fn parse_compat_options_joined_local(joined: &str) -> Vec<(String, String)> {
    joined
        .split(',')
        .filter_map(|entry| {
            let trimmed = entry.trim();
            let (key, value) = trimmed.split_once('=')?;
            Some((key.trim().to_ascii_lowercase(), value.trim().to_owned()))
        })
        .collect()
}

fn compat_text_search_object_parts(canonical_name: &str) -> Option<(&str, &str)> {
    canonical_name.split_once(':')
}

fn compat_text_search_schema_and_name(
    object_name: &str,
    schema_override: &str,
) -> (String, String) {
    if !schema_override.is_empty() {
        return (
            schema_override.to_ascii_lowercase(),
            object_name
                .rsplit_once('.')
                .map(|(_, bare)| bare.to_ascii_lowercase())
                .unwrap_or_else(|| object_name.to_ascii_lowercase()),
        );
    }
    if let Some((schema_name, bare_name)) = object_name.rsplit_once('.') {
        (
            schema_name.to_ascii_lowercase(),
            bare_name.to_ascii_lowercase(),
        )
    } else {
        ("public".to_owned(), object_name.to_ascii_lowercase())
    }
}

fn compat_text_search_namespace_oid(
    catalog: &Arc<dyn CatalogReader>,
    txn_id: TxnId,
    schema_name: &str,
) -> i32 {
    if schema_name.eq_ignore_ascii_case("public") {
        return PUBLIC_NAMESPACE_OID;
    }
    if schema_name.eq_ignore_ascii_case("pg_catalog") {
        return PG_CATALOG_NAMESPACE_OID;
    }
    if schema_name.eq_ignore_ascii_case("information_schema") {
        return INFORMATION_SCHEMA_NAMESPACE_OID;
    }
    if let Ok(schemas) = catalog.list_schemas(txn_id) {
        if let Some(schema) = schemas
            .into_iter()
            .find(|descriptor| descriptor.name.eq_ignore_ascii_case(schema_name))
        {
            return u64_to_i32_saturating(schema.schema_id.get()).saturating_add(16384);
        }
    }
    synth_catalog_oid_from_name(&schema_name.to_ascii_lowercase())
}

fn compat_text_search_owner_oid(owner: &str) -> i32 {
    if owner.is_empty() {
        COMPAT_BOOTSTRAP_ROLE_OID
    } else {
        compat_role_oid(owner)
    }
}

fn compat_text_search_template_oid(template_name: &str) -> i32 {
    synth_catalog_oid_from_name(&template_name.to_ascii_lowercase())
}

fn compat_text_search_function_oid(function_name: &str) -> i32 {
    if function_name.is_empty() {
        0
    } else {
        synth_catalog_oid_from_name(&function_name.to_ascii_lowercase())
    }
}

fn compat_text_search_option_value<'a>(options: &'a [(String, String)], key: &str) -> &'a str {
    options
        .iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value.as_str())
        .unwrap_or("")
}

fn compat_text_search_non_template_options(options: &[(String, String)]) -> Option<String> {
    let joined = options
        .iter()
        .filter(|(key, _)| key != "template")
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join(", ");
    if joined.is_empty() {
        None
    } else {
        Some(joined)
    }
}

// ---------------------------------------------------------------
// pg_catalog.pg_authid
// ---------------------------------------------------------------

pub(super) fn pg_authid_fields() -> Vec<ResultField> {
    vec![
        oid_field("oid"),
        name_field("rolname"),
        bool_field("rolsuper"),
        bool_field("rolinherit"),
        bool_field("rolcreaterole"),
        bool_field("rolcreatedb"),
        bool_field("rolcanlogin"),
        bool_field("rolreplication"),
        bool_field("rolbypassrls"),
        int_field("rolconnlimit"),
        nullable_text_field("rolpassword"),
        nullable_text_field("rolvaliduntil"),
    ]
}

pub(super) fn build_pg_authid_plan(
    catalog: &Arc<dyn CatalogReader>,
    txn_id: TxnId,
    session_user: Option<&str>,
) -> DbResult<LogicalPlan> {
    let fields = pg_authid_fields();
    let roles = catalog.list_roles(txn_id)?;
    let mut rows: Vec<Vec<TypedExpr>> = Vec::with_capacity(roles.len().max(1));

    if roles.is_empty() {
        // Always emit at least the session user / default superuser.
        let name = session_user.unwrap_or(COMPAT_BOOTSTRAP_ROLE_NAME);
        rows.push(pg_authid_row(compat_role_oid(name), name, true, true));
    } else {
        for role in &roles {
            rows.push(pg_authid_row(
                compat_role_oid(&role.name),
                &role.name,
                role.superuser,
                role.login,
            ));
        }
        if let Some(user) = session_user {
            if !roles
                .iter()
                .any(|role| role.name.eq_ignore_ascii_case(user))
            {
                rows.push(pg_authid_row(compat_role_oid(user), user, true, true));
            }
        }
    }

    Ok(project_values(fields, rows))
}

fn pg_authid_row(oid: i32, name: &str, superuser: bool, canlogin: bool) -> Vec<TypedExpr> {
    vec![
        int_literal(oid),
        text_literal(name),
        bool_literal(superuser),      // rolsuper
        bool_literal(true),           // rolinherit
        bool_literal(superuser),      // rolcreaterole
        bool_literal(superuser),      // rolcreatedb
        bool_literal(canlogin),       // rolcanlogin
        bool_literal(false),          // rolreplication
        bool_literal(superuser),      // rolbypassrls
        int_literal(-1),              // rolconnlimit (-1 = no limit)
        null_literal(DataType::Text), // rolpassword (hidden)
        null_literal(DataType::Text), // rolvaliduntil
    ]
}

// ---------------------------------------------------------------
// pg_catalog.pg_depend
// ---------------------------------------------------------------

pub(super) fn pg_depend_fields() -> Vec<ResultField> {
    vec![
        oid_field("classid"),
        oid_field("objid"),
        int_field("objsubid"),
        oid_field("refclassid"),
        oid_field("refobjid"),
        int_field("refobjsubid"),
        internal_char_field("deptype"),
    ]
}

pub(super) fn build_pg_depend_plan() -> DbResult<LogicalPlan> {
    let mut rows = Vec::new();
    let Some(pg_cast_classid) = synthetic_table_id("pg_cast").and_then(|id| i32::try_from(id).ok())
    else {
        return Ok(project_values(pg_depend_fields(), rows));
    };
    let Some(pg_type_classid) = synthetic_table_id("pg_type").and_then(|id| i32::try_from(id).ok())
    else {
        return Ok(project_values(pg_depend_fields(), rows));
    };
    let Some(pg_proc_classid) = synthetic_table_id("pg_proc").and_then(|id| i32::try_from(id).ok())
    else {
        return Ok(project_values(pg_depend_fields(), rows));
    };

    rows.extend(aiondb_eval::with_current_session_context(|context| {
        let mut rows = Vec::new();
        for cast in context.compat_user_casts.iter() {
            if let Some(target_type) = context.compat_user_type(&cast.target_type) {
                rows.push(vec![
                    int_literal(pg_cast_classid),
                    int_literal(cast.oid),
                    int_literal(0),
                    int_literal(pg_type_classid),
                    int_literal(target_type.oid),
                    int_literal(0),
                    text_literal("n"),
                ]);
            }
            if cast.method.function_oid() != 0 {
                rows.push(vec![
                    int_literal(pg_cast_classid),
                    int_literal(cast.oid),
                    int_literal(0),
                    int_literal(pg_proc_classid),
                    int_literal(cast.method.function_oid()),
                    int_literal(0),
                    text_literal("n"),
                ]);
            }
            if cast.source_type != "text" {
                if let Some(text_cast) = context.compat_user_casts.iter().find(|entry| {
                    entry.source_type == "text"
                        && entry.target_type == cast.target_type
                        && entry.oid != cast.oid
                }) {
                    rows.push(vec![
                        int_literal(pg_cast_classid),
                        int_literal(cast.oid),
                        int_literal(0),
                        int_literal(pg_cast_classid),
                        int_literal(text_cast.oid),
                        int_literal(0),
                        text_literal("n"),
                    ]);
                }
            }
        }
        rows
    }));

    Ok(project_values(pg_depend_fields(), rows))
}

// ---------------------------------------------------------------
// pg_catalog.pg_description
// ---------------------------------------------------------------

pub(super) fn pg_description_fields() -> Vec<ResultField> {
    vec![
        oid_field("objoid"),
        oid_field("classoid"),
        int_field("objsubid"),
        text_field("description"),
    ]
}

pub(super) fn build_pg_description_plan() -> DbResult<LogicalPlan> {
    let rows = aiondb_eval::with_current_session_context(|context| {
        context
            .compat_comments
            .iter()
            .map(|((object_type, subject), comment)| {
                let classid = compat_description_classoid(object_type);
                let (objoid, objsubid) = compat_description_object_identity(object_type, subject);
                vec![
                    int_literal(objoid),
                    int_literal(classid),
                    int_literal(objsubid),
                    text_literal(comment),
                ]
            })
            .collect::<Vec<_>>()
    });
    Ok(project_values(pg_description_fields(), rows))
}

fn compat_description_classoid(object_type: &str) -> i32 {
    match object_type.to_ascii_uppercase().as_str() {
        // Use PostgreSQL's well-known catalog OIDs for classoid joins that
        // ORMs perform through `CAST('pg_catalog.<table>' AS regclass)`.
        "TABLE" | "VIEW" | "MATERIALIZED VIEW" | "SEQUENCE" | "INDEX" | "COLUMN" => 1259,
        "FUNCTION" | "PROCEDURE" | "AGGREGATE" | "ROUTINE" => 1255,
        "TYPE" | "DOMAIN" => 1247,
        "SCHEMA" => 2615,
        "ROLE" | "USER" | "GROUP" => 1260,
        "DATABASE" => 2964,
        "TABLESPACE" => 1213,
        "OPERATOR" => 2617,
        "CONSTRAINT" => 2606,
        _ => 1259,
    }
}

fn compat_description_objoid(object_type: &str, subject: &str) -> i32 {
    if let Ok(oid) = subject.parse::<i32>() {
        return oid;
    }
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    use std::hash::{Hash, Hasher};
    object_type.to_ascii_uppercase().hash(&mut hasher);
    subject.to_ascii_lowercase().hash(&mut hasher);
    let h = hasher.finish();
    i32::try_from(h & 0x7fff_ffff).unwrap_or(0)
}

fn compat_description_object_identity(object_type: &str, subject: &str) -> (i32, i32) {
    if object_type.eq_ignore_ascii_case("COLUMN") {
        if let Some((objoid, objsubid)) = subject.split_once('.') {
            let parsed_objoid = objoid.parse::<i32>().unwrap_or(0);
            let parsed_objsubid = objsubid.parse::<i32>().unwrap_or(0);
            return (parsed_objoid, parsed_objsubid);
        }
    }
    (compat_description_objoid(object_type, subject), 0)
}

// ---------------------------------------------------------------
// pg_catalog.pg_init_privs
// ---------------------------------------------------------------

pub(super) fn pg_init_privs_fields() -> Vec<ResultField> {
    vec![
        oid_field("objoid"),
        oid_field("classoid"),
        int_field("objsubid"),
        text_field("privtype"),
        ResultField {
            name: "initprivs".to_owned(),
            data_type: DataType::Array(Box::new(DataType::Text)),
            text_type_modifier: None,
            nullable: false,
        },
    ]
}

pub(super) fn build_pg_init_privs_plan() -> DbResult<LogicalPlan> {
    Ok(project_values(
        pg_init_privs_fields(),
        vec![vec![
            int_literal(1255),
            int_literal(1255),
            int_literal(0),
            text_literal("i"),
            TypedExpr::literal(
                Value::Array(vec![Value::Text("=X/aiondb".to_owned())]),
                DataType::Array(Box::new(DataType::Text)),
                false,
            ),
        ]],
    ))
}

// ---------------------------------------------------------------
// pg_catalog.pg_database
// ---------------------------------------------------------------

pub(super) fn pg_database_fields() -> Vec<ResultField> {
    vec![
        oid_field("oid"),
        name_field("datname"),
        oid_field("datdba"),
        int_field("encoding"),
        internal_char_field("datlocprovider"),
        bool_field("datistemplate"),
        bool_field("datallowconn"),
        int_field("datconnlimit"),
        int_field("datfrozenxid"),
        int_field("datminmxid"),
        oid_field("dattablespace"),
        text_field("datcollate"),
        text_field("datctype"),
        nullable_text_field("daticulocale"),
        nullable_text_field("daticurules"),
        nullable_text_field("datcollversion"),
        ResultField {
            name: "datacl".to_owned(),
            data_type: DataType::Array(Box::new(DataType::Text)),
            text_type_modifier: None,
            nullable: true,
        },
    ]
}

pub(super) fn build_pg_database_plan(
    catalog: &std::sync::Arc<dyn aiondb_catalog::CatalogReader>,
    txn_id: aiondb_core::TxnId,
    database_name: Option<&str>,
    owner_oid: i32,
) -> DbResult<LogicalPlan> {
    let fields = pg_database_fields();
    // Walk roles once to build per-database ACL groups. `pg_database.datacl`
    // is derived from catalog privileges so `GRANT CONNECT ON DATABASE` /
    // `GRANT TEMP ON DATABASE` remain visible.
    let datacl_by_db: std::collections::HashMap<String, Vec<String>> = {
        let mut per_role: std::collections::HashMap<(String, String), String> =
            std::collections::HashMap::new();
        for role in catalog.list_roles(txn_id)? {
            for privilege in catalog.get_privileges(txn_id, &role.name)? {
                let aiondb_catalog::PrivilegeTarget::Database(db) = privilege.target else {
                    continue;
                };
                per_role
                    .entry((db, role.name.clone()))
                    .or_default()
                    .push_str(database_privilege_acl_char(privilege.privilege));
            }
        }
        let mut out: std::collections::HashMap<String, Vec<String>> =
            std::collections::HashMap::new();
        for ((db, role), chars) in per_role {
            let mut seen: Vec<char> = Vec::new();
            for ch in chars.chars() {
                if !seen.contains(&ch) {
                    seen.push(ch);
                }
            }
            let compressed: String = seen.into_iter().collect();
            out.entry(db)
                .or_default()
                .push(format!("{role}={compressed}/"));
        }
        out
    };

    // ADR-0014 phase 4: emit one row per database registered in the
    // cluster catalog. If the snapshot is empty (session context without
    // cluster_databases - mocked test case), fall back to the current
    // database.
    let known_roles: std::collections::HashSet<String> = catalog
        .list_roles(txn_id)?
        .into_iter()
        .map(|role| role.name.to_ascii_lowercase())
        .collect();
    let rows = aiondb_eval::with_current_session_context(|context| {
        if context.cluster_databases.is_empty() {
            let db_name = database_name.unwrap_or(COMPAT_DEFAULT_DATABASE_NAME);
            let locale = compat_locale();
            let datacl = datacl_by_db.get(db_name).cloned();
            let datacl_literal = match datacl {
                Some(entries) if !entries.is_empty() => {
                    let values: Vec<aiondb_core::Value> =
                        entries.into_iter().map(aiondb_core::Value::Text).collect();
                    typed_array_literal(values, DataType::Text)
                }
                _ => null_literal(DataType::Array(Box::new(DataType::Text))),
            };
            vec![vec![
                int_literal(compat_database_oid(db_name)),
                text_literal(db_name),
                int_literal(owner_oid),
                int_literal(6),
                text_literal("c"),
                bool_literal(false),
                bool_literal(true),
                int_literal(-1),
                int_literal(0),
                int_literal(1),
                int_literal(COMPAT_PG_DEFAULT_TABLESPACE_OID),
                text_literal(&locale),
                text_literal(&locale),
                null_literal(DataType::Text),
                null_literal(DataType::Text),
                null_literal(DataType::Text),
                datacl_literal,
            ]]
        } else {
            context
                .cluster_databases
                .iter()
                .map(|d| {
                    let datdba_oid = if known_roles.contains(&d.owner.to_ascii_lowercase()) {
                        aiondb_core::compat_role_oid(&d.owner)
                    } else {
                        owner_oid
                    };
                    render_pg_database_row(d, datdba_oid, datacl_by_db.get(&d.name).cloned())
                })
                .collect::<Vec<_>>()
        }
    });
    Ok(project_values(fields, rows))
}

fn database_privilege_acl_char(privilege: aiondb_catalog::CatalogPrivilege) -> &'static str {
    use aiondb_catalog::CatalogPrivilege;
    match privilege {
        CatalogPrivilege::Connect => "c",
        CatalogPrivilege::Create => "C",
        CatalogPrivilege::Temporary => "T",
        CatalogPrivilege::All => "CTc",
        _ => "",
    }
}

fn render_pg_database_row(
    d: &aiondb_eval::ClusterDatabaseSummary,
    datdba_oid: i32,
    datacl: Option<Vec<String>>,
) -> Vec<TypedExpr> {
    let encoding_num: i32 = match d.encoding.to_ascii_uppercase().as_str() {
        "UTF8" => 6,
        "SQL_ASCII" => 0,
        _ => 6,
    };
    let datacl_literal = match datacl {
        Some(entries) if !entries.is_empty() => {
            let values: Vec<aiondb_core::Value> =
                entries.into_iter().map(aiondb_core::Value::Text).collect();
            typed_array_literal(values, DataType::Text)
        }
        _ => null_literal(DataType::Array(Box::new(DataType::Text))),
    };
    vec![
        int_literal(d.id.cast_signed()),
        text_literal(&d.name),
        int_literal(datdba_oid),
        int_literal(encoding_num),
        text_literal("c"),
        bool_literal(d.is_template),
        bool_literal(d.allow_connections),
        int_literal(d.connection_limit.unwrap_or(-1)),
        int_literal(0),
        int_literal(1),
        int_literal(
            d.tablespace_oid
                .map(u32::cast_signed)
                .unwrap_or(COMPAT_PG_DEFAULT_TABLESPACE_OID),
        ),
        text_literal(&d.collate),
        text_literal(&d.ctype),
        null_literal(DataType::Text),
        null_literal(DataType::Text),
        null_literal(DataType::Text),
        datacl_literal,
    ]
}

// ---------------------------------------------------------------
// pg_catalog.pg_partitioned_table
// ---------------------------------------------------------------

pub(super) fn pg_partitioned_table_fields() -> Vec<ResultField> {
    vec![
        oid_field("partrelid"),
        internal_char_field("partstrat"),
        int_field("partnatts"),
        oid_field("partdefid"),
        ResultField {
            name: "partattrs".to_owned(),
            data_type: DataType::Array(Box::new(DataType::Int)),
            text_type_modifier: None,
            nullable: false,
        },
        ResultField {
            name: "partclass".to_owned(),
            data_type: DataType::Array(Box::new(DataType::Int)),
            text_type_modifier: None,
            nullable: false,
        },
        ResultField {
            name: "partcollation".to_owned(),
            data_type: DataType::Array(Box::new(DataType::Int)),
            text_type_modifier: None,
            nullable: false,
        },
        nullable_text_field("partexprs"),
    ]
}

pub(super) fn build_pg_partitioned_table_plan() -> DbResult<LogicalPlan> {
    // AionDB currently has no declarative partitioned tables in pg_catalog.
    // PostgreSQL clients still expect the relation to exist.
    Ok(project_values(pg_partitioned_table_fields(), Vec::new()))
}

// ---------------------------------------------------------------
// pg_catalog.pg_stat_activity
// ---------------------------------------------------------------

pub(super) fn pg_stat_activity_fields() -> Vec<ResultField> {
    vec![
        nullable_oid_field("datid"),
        nullable_name_field("datname"),
        int_field("pid"),
        nullable_int_field("leader_pid"),
        oid_field("usesysid"),
        nullable_name_field("usename"),
        nullable_text_field("application_name"),
        nullable_text_field("client_addr"),
        nullable_text_field("client_hostname"),
        nullable_int_field("client_port"),
        nullable_text_field("backend_start"),
        nullable_text_field("xact_start"),
        nullable_text_field("query_start"),
        nullable_text_field("state_change"),
        nullable_text_field("wait_event_type"),
        nullable_text_field("wait_event"),
        nullable_text_field("state"),
        nullable_text_field("backend_xid"),
        nullable_text_field("backend_xmin"),
        nullable_text_field("query_id"),
        nullable_text_field("query"),
        text_field("backend_type"),
    ]
}

pub(super) fn build_pg_stat_activity_plan(owner_oid: i32) -> DbResult<LogicalPlan> {
    let fields = pg_stat_activity_fields();
    let pid = int_literal(i32::try_from(std::process::id()).unwrap_or(i32::MAX));
    let datname = text_literal("default");
    let usename = text_literal("admin");
    let app = text_literal("aiondb");
    let state = text_literal("active");
    let backend = text_literal("client backend");
    let rows = vec![vec![
        null_literal(DataType::Int),
        datname,
        pid,
        null_literal(DataType::Int),
        int_literal(owner_oid),
        usename,
        app,
        null_literal(DataType::Text),
        null_literal(DataType::Text),
        null_literal(DataType::Int),
        null_literal(DataType::Text),
        null_literal(DataType::Text),
        null_literal(DataType::Text),
        null_literal(DataType::Text),
        null_literal(DataType::Text),
        null_literal(DataType::Text),
        state,
        null_literal(DataType::Text),
        null_literal(DataType::Text),
        null_literal(DataType::Text),
        null_literal(DataType::Text),
        backend,
    ]];
    Ok(project_values(fields, rows))
}
