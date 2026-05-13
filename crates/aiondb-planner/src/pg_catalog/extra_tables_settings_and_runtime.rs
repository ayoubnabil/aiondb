use super::*;

// ---------------------------------------------------------------
// pg_catalog.pg_settings
// ---------------------------------------------------------------

pub(crate) fn pg_settings_fields() -> Vec<ResultField> {
    vec![
        text_field("name"),
        text_field("setting"),
        nullable_text_field("unit"),
        text_field("category"),
        nullable_text_field("short_desc"),
        nullable_text_field("extra_desc"),
        text_field("context"),
        text_field("vartype"),
        text_field("source"),
        nullable_text_field("min_val"),
        nullable_text_field("max_val"),
        nullable_text_field("enumvals"),
        nullable_text_field("boot_val"),
        nullable_text_field("reset_val"),
        nullable_text_field("sourcefile"),
        nullable_int_field("sourceline"),
        bool_field("pending_restart"),
    ]
}

pub(crate) fn build_pg_settings_plan() -> DbResult<LogicalPlan> {
    let fields = pg_settings_fields();
    let rows: Vec<Vec<TypedExpr>> = STATIC_SETTINGS
        .iter()
        .map(|s| {
            let default_setting = pg_setting_value(s.name);
            settings_row(
                s.name,
                current_setting_expr(s.name),
                &default_setting,
                s.category,
                s.vartype,
            )
        })
        .collect();
    Ok(project_values(fields, rows))
}

struct StaticSetting {
    name: &'static str,
    category: &'static str,
    vartype: &'static str,
}

const STATIC_SETTINGS: &[StaticSetting] = &[
    StaticSetting {
        name: "server_version",
        category: "Preset Options",
        vartype: "string",
    },
    StaticSetting {
        name: "server_version_num",
        category: "Preset Options",
        vartype: "string",
    },
    StaticSetting {
        name: "server_encoding",
        category: "Client Connection Defaults / Locale and Formatting",
        vartype: "string",
    },
    StaticSetting {
        name: "client_encoding",
        category: "Client Connection Defaults / Locale and Formatting",
        vartype: "string",
    },
    StaticSetting {
        name: "lc_collate",
        category: "Client Connection Defaults / Locale and Formatting",
        vartype: "string",
    },
    StaticSetting {
        name: "lc_ctype",
        category: "Client Connection Defaults / Locale and Formatting",
        vartype: "string",
    },
    StaticSetting {
        name: "max_connections",
        category: "Connections and Authentication / Connection Settings",
        vartype: "integer",
    },
    StaticSetting {
        name: "standard_conforming_strings",
        category: "Client Connection Defaults / Statement Behavior",
        vartype: "bool",
    },
    StaticSetting {
        name: "search_path",
        category: "Client Connection Defaults / Statement Behavior",
        vartype: "string",
    },
    StaticSetting {
        name: "wal_segment_size",
        category: "Write-Ahead Log / Settings",
        vartype: "integer",
    },
    StaticSetting {
        name: "default_transaction_isolation",
        category: "Client Connection Defaults / Statement Behavior",
        vartype: "enum",
    },
    StaticSetting {
        name: "TimeZone",
        category: "Client Connection Defaults / Locale and Formatting",
        vartype: "string",
    },
    StaticSetting {
        name: "DateStyle",
        category: "Client Connection Defaults / Locale and Formatting",
        vartype: "string",
    },
    StaticSetting {
        name: "IntervalStyle",
        category: "Client Connection Defaults / Locale and Formatting",
        vartype: "string",
    },
    StaticSetting {
        name: "integer_datetimes",
        category: "Preset Options",
        vartype: "bool",
    },
    StaticSetting {
        name: "max_identifier_length",
        category: "Preset Options",
        vartype: "integer",
    },
    StaticSetting {
        name: "is_superuser",
        category: "Preset Options",
        vartype: "bool",
    },
    StaticSetting {
        name: "enable_async_append",
        category: "Query Tuning / Planner Method Configuration",
        vartype: "bool",
    },
    StaticSetting {
        name: "enable_bitmapscan",
        category: "Query Tuning / Planner Method Configuration",
        vartype: "bool",
    },
    StaticSetting {
        name: "enable_gathermerge",
        category: "Query Tuning / Planner Method Configuration",
        vartype: "bool",
    },
    StaticSetting {
        name: "enable_hashagg",
        category: "Query Tuning / Planner Method Configuration",
        vartype: "bool",
    },
    StaticSetting {
        name: "enable_hashjoin",
        category: "Query Tuning / Planner Method Configuration",
        vartype: "bool",
    },
    StaticSetting {
        name: "enable_incremental_sort",
        category: "Query Tuning / Planner Method Configuration",
        vartype: "bool",
    },
    StaticSetting {
        name: "enable_indexonlyscan",
        category: "Query Tuning / Planner Method Configuration",
        vartype: "bool",
    },
    StaticSetting {
        name: "enable_indexscan",
        category: "Query Tuning / Planner Method Configuration",
        vartype: "bool",
    },
    StaticSetting {
        name: "enable_material",
        category: "Query Tuning / Planner Method Configuration",
        vartype: "bool",
    },
    StaticSetting {
        name: "enable_memoize",
        category: "Query Tuning / Planner Method Configuration",
        vartype: "bool",
    },
    StaticSetting {
        name: "enable_mergejoin",
        category: "Query Tuning / Planner Method Configuration",
        vartype: "bool",
    },
    StaticSetting {
        name: "enable_nestloop",
        category: "Query Tuning / Planner Method Configuration",
        vartype: "bool",
    },
    StaticSetting {
        name: "enable_parallel_append",
        category: "Query Tuning / Planner Method Configuration",
        vartype: "bool",
    },
    StaticSetting {
        name: "enable_parallel_hash",
        category: "Query Tuning / Planner Method Configuration",
        vartype: "bool",
    },
    StaticSetting {
        name: "enable_partition_pruning",
        category: "Query Tuning / Planner Method Configuration",
        vartype: "bool",
    },
    StaticSetting {
        name: "enable_partitionwise_aggregate",
        category: "Query Tuning / Planner Method Configuration",
        vartype: "bool",
    },
    StaticSetting {
        name: "enable_partitionwise_join",
        category: "Query Tuning / Planner Method Configuration",
        vartype: "bool",
    },
    StaticSetting {
        name: "enable_presorted_aggregate",
        category: "Query Tuning / Planner Method Configuration",
        vartype: "bool",
    },
    StaticSetting {
        name: "enable_seqscan",
        category: "Query Tuning / Planner Method Configuration",
        vartype: "bool",
    },
    StaticSetting {
        name: "enable_sort",
        category: "Query Tuning / Planner Method Configuration",
        vartype: "bool",
    },
    StaticSetting {
        name: "enable_tidscan",
        category: "Query Tuning / Planner Method Configuration",
        vartype: "bool",
    },
    StaticSetting {
        name: "hnsw.ef_search",
        category: "Customized Options",
        vartype: "integer",
    },
    StaticSetting {
        name: "hnsw.iterative_scan",
        category: "Customized Options",
        vartype: "enum",
    },
    StaticSetting {
        name: "hnsw.max_scan_tuples",
        category: "Customized Options",
        vartype: "integer",
    },
    StaticSetting {
        name: "hnsw.scan_mem_multiplier",
        category: "Customized Options",
        vartype: "real",
    },
    StaticSetting {
        name: "ivfflat.probes",
        category: "Customized Options",
        vartype: "integer",
    },
    StaticSetting {
        name: "ivfflat.iterative_scan",
        category: "Customized Options",
        vartype: "enum",
    },
    StaticSetting {
        name: "ivfflat.max_probes",
        category: "Customized Options",
        vartype: "integer",
    },
];

fn pg_setting_value(name: &str) -> String {
    match name {
        "max_connections" => "128".to_owned(),
        "wal_segment_size" => "16777216".to_owned(),
        "is_superuser" => "on".to_owned(),
        "enable_partitionwise_aggregate" | "enable_partitionwise_join" => "off".to_owned(),
        setting if setting.starts_with("enable_") => "on".to_owned(),
        other => compat_setting_value(other)
            .map(std::borrow::Cow::into_owned)
            .unwrap_or_default(),
    }
}

fn current_setting_expr(name: &str) -> TypedExpr {
    TypedExpr::scalar_function(
        aiondb_plan::ScalarFunction::Generic("current_setting".to_owned()),
        vec![text_literal(name), bool_literal(true)],
        DataType::Text,
        true,
    )
}

fn settings_row(
    name: &str,
    setting: TypedExpr,
    default_setting: &str,
    category: &str,
    vartype: &str,
) -> Vec<TypedExpr> {
    vec![
        text_literal(name),
        setting,
        null_literal(DataType::Text),
        text_literal(category),
        null_literal(DataType::Text),
        null_literal(DataType::Text),
        text_literal("internal"),
        text_literal(vartype),
        text_literal("default"),
        null_literal(DataType::Text),
        null_literal(DataType::Text),
        null_literal(DataType::Text),
        text_literal(default_setting),
        text_literal(default_setting),
        null_literal(DataType::Text),
        null_literal(DataType::Int),
        bool_literal(false),
    ]
}

// ---------------------------------------------------------------
// Additional pg_catalog compatibility views used by sysviews
// ---------------------------------------------------------------

pub(crate) fn pg_available_extension_versions_fields() -> Vec<ResultField> {
    vec![
        text_field("name"),
        text_field("version"),
        bool_field("installed"),
        bool_field("superuser"),
        bool_field("trusted"),
        bool_field("relocatable"),
        nullable_text_field("schema"),
        ResultField {
            name: "requires".to_owned(),
            data_type: DataType::Array(Box::new(DataType::Text)),
            text_type_modifier: None,
            nullable: true,
        },
        nullable_text_field("comment"),
    ]
}

struct AvailableExtensionCompatRow {
    name: String,
    version: String,
    installed_version: Option<String>,
    requires: Vec<String>,
    comment: String,
}

fn available_extension_compat_rows() -> Vec<AvailableExtensionCompatRow> {
    let mut rows = vec![AvailableExtensionCompatRow {
        name: "plpgsql".to_owned(),
        version: "1.0".to_owned(),
        installed_version: Some("1.0".to_owned()),
        requires: Vec::new(),
        comment: "PL/pgSQL procedural language".to_owned(),
    }];
    if let Some(registry) = aiondb_eval::extension_registry() {
        rows.extend(registry.list_available().into_iter().map(|extension| {
            let installed_version = registry.installed_version(&extension.name);
            AvailableExtensionCompatRow {
                name: extension.name,
                version: extension.default_version,
                installed_version,
                requires: extension.dependencies,
                comment: extension.description,
            }
        }));
    } else {
        rows.push(AvailableExtensionCompatRow {
            name: "vector".to_owned(),
            version: "0.8.2".to_owned(),
            installed_version: None,
            requires: Vec::new(),
            comment: "vector data type and similarity search".to_owned(),
        });
    }
    rows.sort_by(|left, right| left.name.cmp(&right.name));
    rows
}

fn available_extension_requires_literal(requires: &[String]) -> TypedExpr {
    if requires.is_empty() {
        null_literal(DataType::Array(Box::new(DataType::Text)))
    } else {
        super::extra_tables::typed_array_literal(
            requires
                .iter()
                .map(|name| Value::Text(name.clone()))
                .collect(),
            DataType::Text,
        )
    }
}

pub(crate) fn build_pg_available_extension_versions_plan() -> DbResult<LogicalPlan> {
    let rows = available_extension_compat_rows()
        .into_iter()
        .map(|extension| {
            vec![
                text_literal(&extension.name),
                text_literal(&extension.version),
                bool_literal(extension.installed_version.is_some()),
                bool_literal(true),
                bool_literal(true),
                bool_literal(false),
                text_literal("pg_catalog"),
                available_extension_requires_literal(&extension.requires),
                text_literal(&extension.comment),
            ]
        })
        .collect();
    Ok(project_values(
        pg_available_extension_versions_fields(),
        rows,
    ))
}

pub(crate) fn pg_available_extensions_fields() -> Vec<ResultField> {
    vec![
        text_field("name"),
        text_field("default_version"),
        nullable_text_field("installed_version"),
        nullable_text_field("comment"),
    ]
}

pub(crate) fn build_pg_available_extensions_plan() -> DbResult<LogicalPlan> {
    let rows = available_extension_compat_rows()
        .into_iter()
        .map(|extension| {
            vec![
                text_literal(&extension.name),
                text_literal(&extension.version),
                extension
                    .installed_version
                    .as_deref()
                    .map(text_literal)
                    .unwrap_or_else(|| null_literal(DataType::Text)),
                text_literal(&extension.comment),
            ]
        })
        .collect();
    Ok(project_values(pg_available_extensions_fields(), rows))
}

pub(crate) fn pg_backend_memory_contexts_fields() -> Vec<ResultField> {
    vec![
        text_field("name"),
        nullable_text_field("ident"),
        nullable_text_field("parent"),
        int_field("level"),
        bigint_field("total_bytes"),
        bigint_field("total_nblocks"),
        bigint_field("free_bytes"),
        bigint_field("free_chunks"),
        bigint_field("used_bytes"),
    ]
}

pub(crate) fn build_pg_backend_memory_contexts_plan() -> DbResult<LogicalPlan> {
    Ok(project_values(
        pg_backend_memory_contexts_fields(),
        vec![vec![
            text_literal("TopMemoryContext"),
            null_literal(DataType::Text),
            null_literal(DataType::Text),
            int_literal(0),
            bigint_literal(8192),
            bigint_literal(1),
            bigint_literal(1024),
            bigint_literal(1),
            bigint_literal(7168),
        ]],
    ))
}

pub(crate) fn pg_config_fields() -> Vec<ResultField> {
    vec![text_field("name"), text_field("setting")]
}

pub(crate) fn build_pg_config_plan() -> DbResult<LogicalPlan> {
    let rows = STATIC_PG_CONFIG
        .iter()
        .map(|(name, setting)| {
            let setting = if *name == "VERSION" {
                format!("PostgreSQL {COMPAT_SERVER_VERSION}")
            } else {
                (*setting).to_owned()
            };
            vec![text_literal(name), text_literal(&setting)]
        })
        .collect();
    Ok(project_values(pg_config_fields(), rows))
}

pub(crate) fn pg_cursors_fields() -> Vec<ResultField> {
    vec![
        text_field("name"),
        text_field("statement"),
        bool_field("is_holdable"),
        bool_field("is_binary"),
        bool_field("is_scrollable"),
        nullable_timestamp_field("creation_time"),
    ]
}

pub(crate) fn build_pg_cursors_plan() -> DbResult<LogicalPlan> {
    Ok(project_values(pg_cursors_fields(), Vec::new()))
}

pub(crate) fn pg_file_settings_fields() -> Vec<ResultField> {
    vec![
        text_field("sourcefile"),
        int_field("sourceline"),
        int_field("seqno"),
        text_field("name"),
        nullable_text_field("setting"),
        bool_field("applied"),
        nullable_text_field("error"),
    ]
}

pub(crate) fn build_pg_file_settings_plan() -> DbResult<LogicalPlan> {
    Ok(project_values(pg_file_settings_fields(), Vec::new()))
}

pub(crate) fn pg_hba_file_rules_fields() -> Vec<ResultField> {
    vec![
        int_field("rule_number"),
        text_field("file_name"),
        int_field("line_number"),
        text_field("type"),
        text_field("database"),
        text_field("user_name"),
        nullable_text_field("address"),
        nullable_text_field("netmask"),
        text_field("auth_method"),
        nullable_text_field("options"),
        nullable_text_field("error"),
    ]
}

pub(crate) fn build_pg_hba_file_rules_plan() -> DbResult<LogicalPlan> {
    Ok(project_values(
        pg_hba_file_rules_fields(),
        vec![vec![
            int_literal(1),
            text_literal("pg_hba.conf"),
            int_literal(1),
            text_literal("local"),
            text_literal("{all}"),
            text_literal("{all}"),
            null_literal(DataType::Text),
            null_literal(DataType::Text),
            text_literal("trust"),
            null_literal(DataType::Text),
            null_literal(DataType::Text),
        ]],
    ))
}

pub(crate) fn pg_ident_file_mappings_fields() -> Vec<ResultField> {
    vec![
        int_field("map_number"),
        text_field("file_name"),
        int_field("line_number"),
        text_field("map_name"),
        text_field("sys_name"),
        text_field("pg_username"),
        nullable_text_field("error"),
    ]
}

pub(crate) fn build_pg_ident_file_mappings_plan() -> DbResult<LogicalPlan> {
    Ok(project_values(pg_ident_file_mappings_fields(), Vec::new()))
}

pub(crate) fn pg_locks_fields() -> Vec<ResultField> {
    vec![
        text_field("locktype"),
        nullable_oid_field("database"),
        nullable_oid_field("relation"),
        nullable_int_field("page"),
        nullable_int_field("tuple"),
        nullable_text_field("virtualxid"),
        nullable_text_field("transactionid"),
        nullable_oid_field("classid"),
        nullable_oid_field("objid"),
        nullable_int_field("objsubid"),
        text_field("virtualtransaction"),
        int_field("pid"),
        text_field("mode"),
        bool_field("granted"),
        bool_field("fastpath"),
        nullable_timestamp_field("waitstart"),
    ]
}

pub(crate) fn build_pg_locks_plan() -> DbResult<LogicalPlan> {
    let pid = int_literal(i32::try_from(std::process::id()).unwrap_or(i32::MAX));
    Ok(project_values(
        pg_locks_fields(),
        vec![
            vec![
                text_literal("virtualxid"),
                int_literal(1),
                null_literal(DataType::Int),
                null_literal(DataType::Int),
                null_literal(DataType::Int),
                text_literal("1/1"),
                null_literal(DataType::Text),
                null_literal(DataType::Int),
                null_literal(DataType::Int),
                null_literal(DataType::Int),
                text_literal("1/1"),
                pid.clone(),
                text_literal("ExclusiveLock"),
                bool_literal(true),
                bool_literal(false),
                null_literal(DataType::TimestampTz),
            ],
            vec![
                text_literal("tuple"),
                int_literal(1),
                null_literal(DataType::Int),
                null_literal(DataType::Int),
                null_literal(DataType::Int),
                null_literal(DataType::Text),
                null_literal(DataType::Text),
                null_literal(DataType::Int),
                null_literal(DataType::Int),
                null_literal(DataType::Int),
                text_literal("1/1"),
                pid,
                text_literal("SIReadLock"),
                bool_literal(true),
                bool_literal(false),
                null_literal(DataType::TimestampTz),
            ],
        ],
    ))
}

pub(crate) fn pg_prepared_statements_fields() -> Vec<ResultField> {
    vec![
        text_field("name"),
        text_field("statement"),
        nullable_timestamp_field("prepare_time"),
        ResultField {
            name: "parameter_types".to_owned(),
            data_type: DataType::Array(Box::new(DataType::Int)),
            text_type_modifier: Some(TextTypeModifier::RegType),
            nullable: false,
        },
        ResultField {
            name: "result_types".to_owned(),
            data_type: DataType::Array(Box::new(DataType::Int)),
            text_type_modifier: Some(TextTypeModifier::RegType),
            nullable: false,
        },
        bool_field("from_sql"),
        bigint_field("generic_plans"),
        bigint_field("custom_plans"),
    ]
}

pub(crate) fn build_pg_prepared_statements_plan() -> DbResult<LogicalPlan> {
    Ok(project_values(pg_prepared_statements_fields(), Vec::new()))
}

pub(crate) fn pg_stat_statements_fields() -> Vec<ResultField> {
    // PG 16 layout (subset). AionDB does not collect per-query timing
    // counters, so the view returns no rows; the column shape lets
    // monitoring tooling inspect the schema without errors.
    vec![
        oid_field("userid"),
        oid_field("dbid"),
        bool_field("toplevel"),
        bigint_field("queryid"),
        text_field("query"),
        bigint_field("plans"),
        double_field("total_plan_time"),
        double_field("min_plan_time"),
        double_field("max_plan_time"),
        double_field("mean_plan_time"),
        double_field("stddev_plan_time"),
        bigint_field("calls"),
        double_field("total_exec_time"),
        double_field("min_exec_time"),
        double_field("max_exec_time"),
        double_field("mean_exec_time"),
        double_field("stddev_exec_time"),
        bigint_field("rows"),
        bigint_field("shared_blks_hit"),
        bigint_field("shared_blks_read"),
        bigint_field("shared_blks_dirtied"),
        bigint_field("shared_blks_written"),
        bigint_field("local_blks_hit"),
        bigint_field("local_blks_read"),
        bigint_field("local_blks_dirtied"),
        bigint_field("local_blks_written"),
        bigint_field("temp_blks_read"),
        bigint_field("temp_blks_written"),
        double_field("blk_read_time"),
        double_field("blk_write_time"),
        bigint_field("wal_records"),
        bigint_field("wal_fpi"),
        bigint_field("wal_bytes"),
    ]
}

pub(crate) fn build_pg_stat_statements_plan() -> DbResult<LogicalPlan> {
    Ok(project_values(pg_stat_statements_fields(), Vec::new()))
}

pub(crate) fn pg_stat_user_indexes_fields() -> Vec<ResultField> {
    vec![
        oid_field("relid"),
        oid_field("indexrelid"),
        text_field("schemaname"),
        text_field("relname"),
        text_field("indexrelname"),
        bigint_field("idx_scan"),
        nullable_timestamp_field("last_idx_scan"),
        bigint_field("idx_tup_read"),
        bigint_field("idx_tup_fetch"),
    ]
}

pub(crate) fn build_pg_stat_user_indexes_plan() -> DbResult<LogicalPlan> {
    Ok(project_values(pg_stat_user_indexes_fields(), Vec::new()))
}

pub(crate) fn pg_statio_user_indexes_fields() -> Vec<ResultField> {
    vec![
        oid_field("relid"),
        oid_field("indexrelid"),
        text_field("schemaname"),
        text_field("relname"),
        text_field("indexrelname"),
        bigint_field("idx_blks_read"),
        bigint_field("idx_blks_hit"),
    ]
}

pub(crate) fn build_pg_statio_user_indexes_plan() -> DbResult<LogicalPlan> {
    Ok(project_values(pg_statio_user_indexes_fields(), Vec::new()))
}

pub(crate) fn pg_stat_slru_fields() -> Vec<ResultField> {
    vec![
        text_field("name"),
        bigint_field("blks_zeroed"),
        bigint_field("blks_hit"),
        bigint_field("blks_read"),
        bigint_field("blks_written"),
        bigint_field("blks_exists"),
        bigint_field("flushes"),
        bigint_field("truncates"),
        nullable_timestamp_field("stats_reset"),
    ]
}

pub(crate) fn build_pg_stat_slru_plan() -> DbResult<LogicalPlan> {
    Ok(project_values(
        pg_stat_slru_fields(),
        vec![
            vec![
                text_literal("CommitTs"),
                bigint_literal(0),
                bigint_literal(0),
                bigint_literal(0),
                bigint_literal(0),
                bigint_literal(0),
                bigint_literal(0),
                bigint_literal(0),
                null_literal(DataType::TimestampTz),
            ],
            vec![
                text_literal("Notify"),
                bigint_literal(0),
                bigint_literal(0),
                bigint_literal(0),
                bigint_literal(0),
                bigint_literal(0),
                bigint_literal(0),
                bigint_literal(0),
                null_literal(DataType::TimestampTz),
            ],
        ],
    ))
}

pub(crate) fn pg_stat_wal_fields() -> Vec<ResultField> {
    vec![
        bigint_field("wal_records"),
        bigint_field("wal_fpi"),
        bigint_field("wal_bytes"),
        bigint_field("wal_buffers_full"),
        bigint_field("wal_write"),
        bigint_field("wal_sync"),
        double_field("wal_write_time"),
        double_field("wal_sync_time"),
        nullable_timestamp_field("stats_reset"),
    ]
}

pub(crate) fn build_pg_stat_wal_plan() -> DbResult<LogicalPlan> {
    Ok(project_values(
        pg_stat_wal_fields(),
        vec![vec![
            bigint_literal(1),
            bigint_literal(0),
            bigint_literal(16 * 1024),
            bigint_literal(0),
            bigint_literal(1),
            bigint_literal(1),
            double_literal(0.0),
            double_literal(0.0),
            null_literal(DataType::TimestampTz),
        ]],
    ))
}

pub(crate) fn pg_stat_wal_receiver_fields() -> Vec<ResultField> {
    vec![
        int_field("pid"),
        text_field("status"),
        nullable_text_field("receive_start_lsn"),
        nullable_int_field("receive_start_tli"),
        nullable_text_field("written_lsn"),
        nullable_text_field("flushed_lsn"),
        nullable_int_field("received_tli"),
        nullable_timestamp_field("last_msg_send_time"),
        nullable_timestamp_field("last_msg_receipt_time"),
        nullable_text_field("latest_end_lsn"),
        nullable_timestamp_field("latest_end_time"),
        nullable_text_field("slot_name"),
        nullable_text_field("sender_host"),
        nullable_int_field("sender_port"),
        nullable_text_field("conninfo"),
    ]
}

pub(crate) fn build_pg_stat_wal_receiver_plan() -> DbResult<LogicalPlan> {
    Ok(project_values(pg_stat_wal_receiver_fields(), Vec::new()))
}

pub(crate) fn pg_timezone_abbrevs_fields() -> Vec<ResultField> {
    vec![
        text_field("abbrev"),
        text_field("utc_offset"),
        bool_field("is_dst"),
    ]
}

pub(crate) fn build_pg_timezone_abbrevs_plan() -> DbResult<LogicalPlan> {
    let rows = TIMEZONE_COMPAT_ROWS
        .iter()
        .map(|(name, utc_offset)| {
            vec![
                text_literal(name),
                text_literal(utc_offset),
                bool_literal(false),
            ]
        })
        .collect();
    Ok(project_values(pg_timezone_abbrevs_fields(), rows))
}

pub(crate) fn pg_timezone_names_fields() -> Vec<ResultField> {
    vec![
        text_field("name"),
        text_field("abbrev"),
        text_field("utc_offset"),
        bool_field("is_dst"),
    ]
}

pub(crate) fn build_pg_timezone_names_plan() -> DbResult<LogicalPlan> {
    let rows = TIMEZONE_COMPAT_ROWS
        .iter()
        .map(|(name, utc_offset)| {
            vec![
                text_literal(name),
                text_literal(name),
                text_literal(utc_offset),
                bool_literal(false),
            ]
        })
        .collect();
    Ok(project_values(pg_timezone_names_fields(), rows))
}

const STATIC_PG_CONFIG: &[(&str, &str)] = &[
    ("BINDIR", "/usr/lib/postgresql/bin"),
    ("DOCDIR", "/usr/share/doc/postgresql"),
    ("HTMLDIR", "/usr/share/doc/postgresql/html"),
    ("INCLUDEDIR", "/usr/include/postgresql"),
    ("PKGINCLUDEDIR", "/usr/include/postgresql/server"),
    ("INCLUDEDIR-SERVER", "/usr/include/postgresql/server"),
    ("LIBDIR", "/usr/lib/postgresql"),
    ("PKGLIBDIR", "/usr/lib/postgresql/lib"),
    ("LOCALEDIR", "/usr/share/locale"),
    ("MANDIR", "/usr/share/man"),
    ("SHAREDIR", "/usr/share/postgresql"),
    ("SYSCONFDIR", "/etc/postgresql"),
    ("PGXS", "/usr/lib/postgresql/pgxs/src/makefiles/pgxs.mk"),
    ("CONFIGURE", "--with-openssl --with-icu"),
    ("CC", "cc"),
    ("CPPFLAGS", "-I/usr/include"),
    ("CFLAGS", "-O2"),
    ("CFLAGS_SL", "-fPIC"),
    ("LDFLAGS", ""),
    ("LDFLAGS_EX", ""),
    ("LDFLAGS_SL", ""),
    ("LIBS", "-lm"),
    ("VERSION", ""),
];

const TIMEZONE_COMPAT_ROWS: &[(&str, &str)] = &[
    ("UTC-12", "-12:00"),
    ("UTC-11", "-11:00"),
    ("UTC-10", "-10:00"),
    ("UTC-9", "-09:00"),
    ("UTC-8", "-08:00"),
    ("UTC-7", "-07:00"),
    ("UTC-6", "-06:00"),
    ("UTC-5", "-05:00"),
    ("UTC-4", "-04:00"),
    ("UTC-3", "-03:00"),
    ("UTC-2", "-02:00"),
    ("UTC-1", "-01:00"),
    ("UTC", "+00:00"),
    ("UTC+1", "+01:00"),
    ("UTC+2", "+02:00"),
    ("UTC+3", "+03:00"),
    ("UTC+4", "+04:00"),
    ("UTC+5", "+05:00"),
    ("UTC+6", "+06:00"),
    ("UTC+7", "+07:00"),
    ("UTC+8", "+08:00"),
    ("UTC+9", "+09:00"),
    ("UTC+10", "+10:00"),
    ("UTC+11", "+11:00"),
    ("UTC+12", "+12:00"),
];
