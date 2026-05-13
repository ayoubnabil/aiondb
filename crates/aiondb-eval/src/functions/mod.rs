use aiondb_core::DataType;
use aiondb_plan::ScalarFunction;

mod cypher_info;
mod datetime_info;
mod json_array_info;
mod math_info;
mod pg_catalog_info;
mod pg_internal_info;
mod text_info;

#[derive(Debug, Default)]
pub struct FunctionRegistry;

/// Metadata about a resolved scalar function.
#[derive(Debug, Clone)]
pub struct FunctionInfo {
    pub func: ScalarFunction,
    pub return_type: DataType,
    pub min_args: usize,
    pub max_args: Option<usize>,
}

impl FunctionRegistry {
    /// Attempt to look up a built-in scalar function by name (case-insensitive).
    ///
    /// Returns `None` for reserved/stub functions, aggregate functions, and
    /// unknown names.  Also checks the extension registry for functions
    /// contributed by installed extensions.
    #[must_use]
    pub fn lookup(name: &str) -> Option<FunctionInfo> {
        let lower = name.to_ascii_lowercase();
        if is_executor_backed_generic(&lower) {
            return Self::lookup_any(&lower);
        }
        if let Some(info) = Self::lookup_any(&lower).filter(Self::is_implemented) {
            return Some(info);
        }
        // Check extension registry for functions from installed extensions.
        if let Some(ext_fn) =
            crate::extension_registry().and_then(|reg| reg.lookup_function(&lower))
        {
            return Some(FunctionInfo {
                func: ScalarFunction::Generic(ext_fn.name.clone()),
                return_type: ext_fn.return_type.clone(),
                min_args: ext_fn.min_args,
                max_args: ext_fn.max_args,
            });
        }
        None
    }

    /// Look up a reserved/stub scalar function by name.
    #[must_use]
    pub fn lookup_reserved(name: &str) -> Option<FunctionInfo> {
        let lower = name.to_ascii_lowercase();
        if is_executor_backed_generic(&lower) {
            return None;
        }
        Self::lookup_any(&lower).filter(|info| !Self::is_implemented(info))
    }

    fn is_implemented(info: &FunctionInfo) -> bool {
        match &info.func {
            ScalarFunction::PgGetViewdef => true,
            ScalarFunction::Generic(name) if is_explicit_pg_stub(name) => false,
            ScalarFunction::Generic(name) => {
                matches!(
                    name.as_str(),
                    "multirange"
                        | "range_minus"
                        | "range_adjacent"
                        | "range_not_extend_right"
                        | "range_not_extend_left"
                        | "range_contains"
                        | "range_contained_by"
                        | "range_overlaps_multirange"
                        | "range_contained_by_multirange"
                        | "elem_contained_by_multirange"
                        | "multirange_contained_by_multirange"
                        | "multirange_contains_elem"
                        | "multirange_contains_multirange"
                        | "multirange_contains_range"
                        | "multirange_overlaps_multirange"
                        | "multirange_overlaps_range"
                        | "multirange_of_text"
                        | "range_agg"
                        | "range_intersect_agg"
                        | "float8range"
                        | "textrange"
                        | "textmultirange"
                        | "arrayrange"
                        | "arraymultirange"
                        | "float8multirange"
                        | "intr_multirange"
                        | "two_ints_range"
                        | "two_ints_multirange"
                        | "textrange1"
                        | "_textrange1"
                        | "textrange2"
                        | "textrange_c"
                        | "textrange_en_us"
                        | "width_bucket"
                        // jsonb_each / jsonb_each_text internals exposed
                        // through the planner as set-returning function
                        // shims; their FunctionInfo is registered in
                        // json_array_info.rs and they're implemented in
                        // the executor.
                        | "__aiondb_jsonb_each_keys"
                        | "__aiondb_jsonb_each_values"
                        | "__aiondb_jsonb_each_text_values"
                        // Size functions
                        | "pg_size_pretty"
                        | "pg_size_bytes"
                        | "current_setting"
                        | "set_config"
                        | "pg_backend_pid"
                        | "pg_notify"
                        | "pg_notification_queue_usage"
                        | "pg_listening_channels"
                        | "inet_client_addr"
                        | "inet_server_addr"
                        | "inet_client_port"
                        | "inet_server_port"
                        | "pg_postmaster_start_time"
                        | "pg_conf_load_time"
                        | "pg_export_snapshot"
                        | "pg_replication_origin_progress"
                        | "pg_client_encoding"
                        | "getdatabaseencoding"
                        | "pg_encoding_to_char"
                        | "pg_catalog.pg_encoding_to_char"
                        | "pg_char_to_encoding"
                        | "pg_catalog.pg_char_to_encoding"
                        | "current_database"
                        | "version"
                        | "current_schema"
                        | "current_catalog"
                        | "current_schemas"
                        | "pg_is_in_recovery"
                        | "pg_is_wal_replay_paused"
                        | "pg_log_backend_memory_contexts"
                        | "pg_ls_dir"
                        | "pg_ls_archive_statusdir"
                        | "pg_ls_logdir"
                        | "pg_ls_tmpdir"
                        | "pg_read_file"
                        | "pg_read_binary_file"
                        | "pg_get_userbyid"
                        | "pg_relation_is_publishable"
                        | "pg_stat_force_next_flush"
                        | "pg_stat_get_snapshot_timestamp"
                        | "pg_stat_clear_snapshot"
                        | "pg_stat_get_function_calls"
                        | "pg_stat_get_xact_function_calls"
                        | "pg_stat_get_live_tuples"
                        | "pg_stat_get_tuples_inserted"
                        | "pg_stat_get_tuples_updated"
                        | "pg_stat_get_tuples_deleted"
                        | "pg_stat_get_tuples_hot_updated"
                        | "pg_stat_get_xact_tuples_inserted"
                        | "pg_stat_get_xact_tuples_updated"
                        | "pg_stat_get_xact_tuples_deleted"
                        | "pg_stat_reset"
                        | "pg_stat_reset_single_table_counters"
                        | "pg_stat_reset_shared"
                        | "pg_stat_reset_slru"
                        | "pg_sleep"
                        | "pg_sleep_for"
                        | "pg_sleep_until"
                        | "pg_get_indexdef"
                        | "pg_catalog.pg_get_indexdef"
                        | "format_type"
                        | "pg_catalog.format_type"
                        | "pg_input_error_info"
                        | "__aiondb_pg_input_error_info_message"
                        | "__aiondb_pg_input_error_info_detail"
                        | "__aiondb_pg_input_error_info_hint"
                        | "__aiondb_pg_input_error_info_sqlstate"
                        | "__aiondb_pg_char_cast"
                        | "__aiondb_regclass_cast"
                        | "__aiondb_regproc_cast"
                        | "__aiondb_regprocedure_cast"
                        | "__aiondb_regtype_cast"
                        | "__aiondb_regtype_out"
                        | "__aiondb_regrole_cast"
                        | "__aiondb_regclass_out"
                        | "__aiondb_regproc_out"
                        | "__aiondb_regprocedure_out"
                        | "__aiondb_regrole_out"
                        | "__aiondb_xid_cast"
                        | "__aiondb_xid8_cast"
                        | "__aiondb_pg_snapshot_cast"
                        | "__aiondb_compat_cast"
                        | "__aiondb_composite_field"
                        | "__aiondb_composite_assign"
                        | "localtimestamp"
                        | "interval_hash"
                        | "to_regclass"
                        | "to_regtype"
                        | "to_regnamespace"
                        | "regclass"
                        | "regtype"
                        | "pg_relation_size"
                        | "pg_table_size"
                        | "pg_total_relation_size"
                        | "pg_indexes_size"
                        | "pg_database_size"
                        | "pg_tablespace_size"
                        | "pg_column_size"
                        | "pg_tablespace_location"
                        | "pg_catalog.pg_tablespace_location"
                        | "pg_tablespace_databases"
                        | "makeaclitem"
                        | "pg_catalog.makeaclitem"
                        | "inet_subnet_contained_by_or_equals"
                        | "inet_subnet_contains_or_equals"
                        | "lo_create"
                        | "pg_catalog.lo_create"
                        | "lo_creat"
                        | "pg_catalog.lo_creat"
                        | "lo_open"
                        | "pg_catalog.lo_open"
                        | "lo_close"
                        | "pg_catalog.lo_close"
                        | "loread"
                        | "pg_catalog.loread"
                        | "lowrite"
                        | "pg_catalog.lowrite"
                        | "lo_unlink"
                        | "pg_catalog.lo_unlink"
                        | "lo_truncate"
                        | "pg_catalog.lo_truncate"
                        | "lo_truncate64"
                        | "pg_catalog.lo_truncate64"
                        | "lo_lseek"
                        | "pg_catalog.lo_lseek"
                        | "lo_lseek64"
                        | "pg_catalog.lo_lseek64"
                        | "lo_tell"
                        | "pg_catalog.lo_tell"
                        | "lo_tell64"
                        | "pg_catalog.lo_tell64"
                        | "lo_get"
                        | "pg_catalog.lo_get"
                        | "lo_put"
                        | "pg_catalog.lo_put"
                        | "lo_from_bytea"
                        | "pg_catalog.lo_from_bytea"
                        | "brin_summarize_range"
                        | "pg_catalog.brin_summarize_range"
                        | "brin_desummarize_range"
                        | "pg_catalog.brin_desummarize_range"
                        // SQL/JSON constructor functions
                        | "json_object"
                        | "json_array"
                        | "json_scalar"
                        | "__aiondb_is_json"
                        | "__aiondb_json_array_subquery"
                        // Geometric type constructors
                        | "box"
                        | "circle"
                        | "line"
                        | "lseg"
                        | "path"
                        | "point"
                        | "polygon"
                        // Geometric operators/functions
                        | "area"
                        | "center"
                        | "diameter"
                        | "height"
                        | "isclosed"
                        | "isopen"
                        | "npoints"
                        | "pclose"
                        | "popen"
                        | "radius"
                        | "slope"
                        | "width"
                        | "diagonal"
                        | "ishorizontal"
                        | "isvertical"
                        // ANY/ALL/SOME array comparison
                        | "any"
                        | "all"
                        | "some"
                        // Network type functions
                        | "inet_merge"
                        | "inet_same_family"
                        | "broadcast"
                        | "host"
                        | "hostmask"
                        | "masklen"
                        | "netmask"
                        | "network"
                        | "set_masklen"
                        | "text"
                        | "abbrev"
                        | "family"
                        | "macaddr8_set7bit"
                        // Type I/O functions
                        | "textin"
                        | "textout"
                        | "int4in"
                        | "int4out"
                        | "int8in"
                        | "int8out"
                        | "float4in"
                        | "float4out"
                        | "float8in"
                        | "float8out"
                        | "boolin"
                        | "boolout"
                        // Boolean comparison functions
                        | "booleq"
                        | "boolne"
                        | "boollt"
                        | "boolgt"
                        | "boolle"
                        | "boolge"
                        | "oidin"
                        | "oidout"
                        // Stats/aggregate helpers
                        | "any_value"
                        | "every"
                        | "bit_and"
                        | "bit_or"
                        | "bit_xor"
                        | "float8_accum"
                        | "float8_combine"
                        | "float8_regr_accum"
                        | "float8_regr_combine"
                        | "booland_statefunc"
                        | "boolor_statefunc"
                        // JSON aggregate functions
                        | "json_build_object"
                        | "json_build_array"
                        | "json_extract_path"
                        | "json_extract_path_text"
                        | "json_array_length"
                        | "row_to_json"
                        | "to_json"
                        | "to_jsonb"
                        | "json_strip_nulls"
                        | "json_typeof"
                        | "array_to_json"
                        | "jsonb_set"
                        | "jsonb_delete"
                        | "jsonb_delete_path"
                        | "jsonb_insert"
                        | "jsonb_object"
                        | "jsonb_set_lax"
                        | "jsonb_object_keys"
                        | "jsonb_concat"
                        | "jsonb_contains"
                        | "jsonb_contained"
                        | "jsonb_exists"
                        | "jsonb_exists_any"
                        | "jsonb_exists_all"
                        | "jsonb_to_tsvector"
                        | "json_to_tsvector"
                        // JSONB set-returning functions
                        | "jsonb_each"
                        | "jsonb_each_text"
                        | "jsonb_array_elements"
                        | "jsonb_array_elements_text"
                        | "jsonb_populate_record"
                        | "json_populate_record"
                        | "jsonb_to_record"
                        | "json_to_record"
                        | "jsonb_populate_recordset"
                        | "json_populate_recordset"
                        | "jsonb_to_recordset"
                        | "json_to_recordset"
                        | "__aiondb_jsonb_to_record"
                        | "__aiondb_json_to_record"
                        | "__aiondb_jsonb_populate_record"
                        | "__aiondb_json_populate_record"
                        | "__aiondb_jsonb_to_recordset"
                        | "__aiondb_json_to_recordset"
                        | "__aiondb_jsonb_populate_recordset"
                        | "__aiondb_json_populate_recordset"
                        | "json_each"
                        | "json_each_text"
                        | "json_array_elements"
                        | "json_array_elements_text"
                        | "json_object_keys"
                        // Trig/math functions
                        | "sin"
                        | "cos"
                        | "tan"
                        | "asin"
                        | "acos"
                        | "atan"
                        | "atan2"
                        | "sinh"
                        | "cosh"
                        | "tanh"
                        | "asinh"
                        | "acosh"
                        | "atanh"
                        | "sind"
                        | "cosd"
                        | "tand"
                        | "asind"
                        | "acosd"
                        | "atand"
                        | "atan2d"
                        | "cotd"
                        | "cot"
                        | "degrees"
                        | "radians"
                        | "cbrt"
                        | "erf"
                        | "erfc"
                        | "scale"
                        | "div"
                        | "gcd"
                        | "lcm"
                        | "factorial"
                        | "min_scale"
                        | "trim_scale"
                        | "setseed"
                        | "timeofday"
                        | "suppress_redundant_updates_trigger"
                        | "justify_days"
                        | "justify_hours"
                        | "justify_interval"
                        | "__aiondb_interval_fields"
                        | "__aiondb_interval_precision"
                        | "date"
                        | "timestamp"
                        | "timestamptz"
                        | "time"
                        | "timetz"
                        | "interval"
                        | "isfinite"
                        | "overlaps"
                        | "num_nulls"
                        | "num_nonnulls"
                        | "__aiondb_variadic_num_nulls"
                        | "__aiondb_variadic_num_nonnulls"
                        | "__aiondb_variadic_concat"
                        | "__aiondb_variadic_concat_ws"
                        | "__aiondb_variadic_format"
                        // Generate subscripts
                        | "generate_subscripts"
                        // JSONB path query (SRF)
                        | "jsonb_path_query"
                        | "string_to_table"
                        // Newly implemented text/regex/bytea functions
                        | "regexp_count"
                        | "regexp_like"
                        | "regexp_instr"
                        | "regexp_substr"
                        | "btrim"
                        | "unistr"
                        | "normalize"
                        | "is_normalized"
                        | "sha224"
                        | "sha256"
                        | "sha384"
                        | "sha512"
                        | "get_bit"
                        | "set_bit"
                        | "get_byte"
                        | "set_byte"
                        | "bit_count"
                        | "cashlarger"
                        | "cashsmaller"
                        | "cash_words"
                        // ── Formerly-stubbed PG compat functions (now implemented) ──
                        // Privilege checking (single-user → always true)
                        | "has_table_privilege"
                        | "has_schema_privilege"
                        | "has_column_privilege"
                        | "has_any_column_privilege"
                        | "has_function_privilege"
                        | "has_sequence_privilege"
                        | "has_type_privilege"
                        | "has_database_privilege"
                        | "has_server_privilege"
                        | "has_tablespace_privilege"
                        | "has_language_privilege"
                        | "has_foreign_data_wrapper_privilege"
                        | "pg_has_role"
                        | "row_security_active"
                        // Description functions (no comment system → NULL)
                        | "obj_description"
                        | "shobj_description"
                        | "pg_catalog.shobj_description"
                        | "col_description"
                        // Expression/constraint def
                        | "pg_get_expr"
                        | "pg_get_constraintdef"
                        // Visibility functions (public schema → true)
                        | "pg_function_is_visible"
                        | "pg_catalog.pg_function_is_visible"
                        | "pg_proc_is_visible"
                        | "pg_catalog.pg_proc_is_visible"
                        | "pg_table_is_visible"
                        | "pg_catalog.pg_table_is_visible"
                        | "pg_type_is_visible"
                        | "pg_catalog.pg_type_is_visible"
                        | "pg_operator_is_visible"
                        | "pg_catalog.pg_operator_is_visible"
                        | "pg_collation_is_visible"
                        | "pg_catalog.pg_collation_is_visible"
                        | "pg_opclass_is_visible"
                        | "pg_catalog.pg_opclass_is_visible"
                        | "pg_opfamily_is_visible"
                        | "pg_catalog.pg_opfamily_is_visible"
                        | "pg_ts_dict_is_visible"
                        | "pg_catalog.pg_ts_dict_is_visible"
                        | "pg_ts_config_is_visible"
                        | "pg_catalog.pg_ts_config_is_visible"
                        | "pg_ts_parser_is_visible"
                        | "pg_catalog.pg_ts_parser_is_visible"
                        | "pg_ts_template_is_visible"
                        | "pg_catalog.pg_ts_template_is_visible"
                        | "pg_conversion_is_visible"
                        | "pg_catalog.pg_conversion_is_visible"
                        | "pg_statistics_obj_is_visible"
                        | "pg_catalog.pg_statistics_obj_is_visible"
                        | "pg_advisory_lock"
                        | "pg_advisory_lock_shared"
                        | "pg_advisory_xact_lock"
                        | "pg_advisory_xact_lock_shared"
                        | "pg_try_advisory_lock"
                        | "pg_try_advisory_lock_shared"
                        | "pg_try_advisory_xact_lock"
                        | "pg_try_advisory_xact_lock_shared"
                        | "pg_advisory_unlock"
                        | "pg_advisory_unlock_shared"
                        | "pg_advisory_unlock_all"
                        | "pg_cancel_backend"
                        | "pg_terminate_backend"
                        // Reg* conversion
                        | "regproc"
                        | "regprocedure"
                        | "regoper"
                        | "regoperator"
                        | "regrole"
                        | "regnamespace"
                        | "regcollation"
                        | "to_regproc"
                        | "to_regprocedure"
                        | "to_regoper"
                        | "to_regoperator"
                        | "to_regrole"
                        | "to_regcollation"
                        // Serial/index/trigger/rule definition
                        | "pg_get_serial_sequence"
            | "pg_get_triggerdef"
                        | "pg_get_ruledef"
                        | "pg_get_partkeydef"
                        | "pg_get_statisticsobjdef"
                        | "pg_get_statisticsobjdef_columns"
                        | "pg_get_partition_constraintdef"
                        // Function definition
                        | "pg_get_functiondef"
                        | "pg_get_function_arguments"
                        | "pg_get_function_result"
                        | "pg_get_function_identity_arguments"
                        // Object identification
                        | "pg_get_object_address"
                        | "pg_identify_object"
                        | "pg_identify_object_as_address"
                        | "pg_describe_object"
                        | "pg_collation_for"
                        // GROUPING (returns 0 outside GROUPING SETS)
                        | "grouping" // Input error info
                        | "binary_coercible"
                        | "check_ddl_rewrite"
                        | "xmlexists"
                        | "xml_is_well_formed"
                        | "xml_is_well_formed_document"
                        | "xml_is_well_formed_content"
                        | "xmlcomment"
                        | "xmlconcat"
                        | "xmlpi"
                        | "xmlroot"
                        | "xmlserialize"
                        | "xmlelement"
                        | "xmlforest"
                        | "xmlparse"
                        | "xpath"
                        | "xpath_exists"
                        | "gin_clean_pending_list"
                        // Type cast functions
                        | "float8"
                        | "float4"
                        | "int2"
                        | "int4"
                        | "int8"
                        | "oid"
                        // Test-helper identity/constructor functions
                        | "vol"
                        | "volfoo"
                        | "make_ad"
                        // Enum introspection
                        | "enum_range"
                        | "enum_first"
                        | "enum_last"
                        // Numeric increment (PG internal)
                        | "numeric_inc"
                        | "int4mul"
                        // Trigger depth
                        | "pg_trigger_depth"
                        // ── Cypher type conversion functions ──
                        | "cypher_toboolean"
                        | "cypher_tointeger"
                        | "cypher_tofloat"
                        | "cypher_tostring"
                        | "cypher_tobooleanornull"
                        | "cypher_tointegerornull"
                        | "cypher_tofloatornull"
                        | "cypher_tostringornull"
                        // ── Cypher temporal constructors ──
                        | "cypher_date"
                        | "cypher_time"
                        | "cypher_datetime"
                        | "cypher_localtime"
                        | "cypher_localdatetime"
                        | "cypher_duration"
                        // ── Cypher math ──
                        | "e"
                        // ── Cypher range ──
                        | "range"
                        // ── Cypher list/string utilities ──
                        | "cypher_size"
                        | "cypher_head"
                        | "cypher_last"
                        | "cypher_tail"
                        | "cypher_array_get"
                        | "__cypher_starts_with"
                        | "__cypher_ends_with"
                        | "__cypher_contains"
                        | "__cypher_in"
                        | "__cypher_has_label"
                        // ── Cypher temporal truncate/between ──
                        | "date.truncate"
                        | "datetime.truncate"
                        | "localdatetime.truncate"
                        | "time.truncate"
                        | "localtime.truncate"
                        | "duration.between"
                        | "duration.inmonths"
                        | "duration.indays"
                        | "duration.inseconds"
                        // ── Cypher graph element introspection ──
                        | "graph_labels"
                        | "graph_type"
                        | "graph_id"
                        | "graph_properties"
                        | "graph_start_node"
                        | "graph_end_node"
                        | "graph_path_length"
                        | "graph_nodes"
                        | "graph_relationships"
                )
            }
            _ => true,
        }
    }

    fn lookup_any(name: &str) -> Option<FunctionInfo> {
        let lower = name.to_ascii_lowercase();
        text_info::lookup(&lower)
            .or_else(|| math_info::lookup(&lower))
            .or_else(|| datetime_info::lookup(&lower))
            .or_else(|| json_array_info::lookup(&lower))
            .or_else(|| pg_catalog_info::lookup(&lower))
            .or_else(|| pg_internal_info::lookup(&lower))
            .or_else(|| cypher_info::lookup(&lower))
            .or_else(|| {
                lower.strip_prefix("pg_catalog.").and_then(|stripped| {
                    text_info::lookup(stripped)
                        .or_else(|| math_info::lookup(stripped))
                        .or_else(|| datetime_info::lookup(stripped))
                        .or_else(|| json_array_info::lookup(stripped))
                        .or_else(|| pg_catalog_info::lookup(stripped))
                        .or_else(|| pg_internal_info::lookup(stripped))
                        .or_else(|| cypher_info::lookup(stripped))
                })
            })
    }
}

fn is_executor_backed_generic(name: &str) -> bool {
    matches!(
        name,
        "current_setting"
            | "row_security_active"
            | "graph_neighbors"
            | "pg_backend_pid"
            | "pg_current_xact_id"
            | "pg_current_xact_id_if_assigned"
            | "txid_current"
            | "txid_current_if_assigned"
            | "has_function_privilege"
            | "pg_get_serial_sequence"
            | "pg_get_statisticsobjdef"
            | "__aiondb_regclass_cast"
            | "__aiondb_regproc_cast"
            | "__aiondb_regprocedure_cast"
            | "__aiondb_regtype_cast"
            | "__aiondb_regrole_cast"
            | "__aiondb_regclass_out"
            | "__aiondb_regproc_out"
            | "__aiondb_regprocedure_out"
            | "__aiondb_regrole_out"
            | "__aiondb_compat_cast"
            | "setval"
            | "currval"
            | "lastval"
            | "gen_random_uuid"
            | "uuid_generate_v4"
            | "vector_top_k_ids"
            | "vector_top_k_hits"
            | "vector_prefetch_top_k_hits"
            | "vector_recommend_top_k_hits"
            | "full_text_top_k_hits"
            | "hybrid_search_top_k_hits"
            | "hybrid_fuse_rrf_hits"
            | "hybrid_fuse_dbsf_hits"
            | "hybrid_group_hits_by"
    )
}

/// Returns `true` only for functions that genuinely cannot produce any
/// meaningful result and must remain hard errors. All former stubs that
/// can return a reasonable compatibility value have been moved to the
/// executor (`eval_pg_compat_stub`).
pub(crate) fn is_explicit_pg_stub(_name: &str) -> bool {
    // All former stubs are now implemented - none remain.
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_upper_exists() {
        let info = FunctionRegistry::lookup("upper").unwrap();
        assert_eq!(info.func, ScalarFunction::Upper);
        assert_eq!(info.return_type, DataType::Text);
        assert_eq!(info.min_args, 1);
        assert_eq!(info.max_args, Some(1));
    }

    #[test]
    fn lookup_case_insensitive() {
        assert!(FunctionRegistry::lookup("UPPER").is_some());
        assert!(FunctionRegistry::lookup("Upper").is_some());
        assert!(FunctionRegistry::lookup("NOW").is_some());
    }

    #[test]
    fn lookup_unknown_returns_none() {
        assert!(FunctionRegistry::lookup("foobar").is_none());
    }

    #[test]
    fn lookup_concat_is_variadic() {
        let info = FunctionRegistry::lookup("concat").unwrap();
        assert_eq!(info.max_args, None);
    }

    #[test]
    fn lookup_pg_get_viewdef_remains_supported() {
        assert!(FunctionRegistry::lookup("pg_get_viewdef").is_some());
        assert!(FunctionRegistry::lookup_reserved("pg_get_viewdef").is_none());
    }

    #[test]
    fn lookup_current_setting_is_supported() {
        assert!(FunctionRegistry::lookup("current_setting").is_some());
        assert!(FunctionRegistry::lookup_reserved("current_setting").is_none());
    }

    #[test]
    fn lookup_pg_backend_pid_is_supported() {
        assert!(FunctionRegistry::lookup("pg_backend_pid").is_some());
        assert!(FunctionRegistry::lookup_reserved("pg_backend_pid").is_none());
    }

    #[test]
    fn lookup_shobj_description_is_supported() {
        assert!(FunctionRegistry::lookup("shobj_description").is_some());
        assert!(FunctionRegistry::lookup("pg_catalog.shobj_description").is_some());
        assert!(FunctionRegistry::lookup_reserved("shobj_description").is_none());
    }

    #[test]
    fn lookup_unicode_compat_functions_are_supported() {
        for name in &[
            "current_database",
            "getdatabaseencoding",
            "normalize",
            "is_normalized",
        ] {
            assert!(FunctionRegistry::lookup(name).is_some());
            assert!(FunctionRegistry::lookup_reserved(name).is_none());
        }
    }

    #[test]
    fn lookup_all_text_functions() {
        for name in &[
            "upper",
            "lower",
            "length",
            "char_length",
            "octet_length",
            "substring",
            "trim",
            "ltrim",
            "rtrim",
            "replace",
            "strpos",
            "left",
            "right",
            "repeat",
            "reverse",
            "starts_with",
            "concat",
            "lpad",
            "rpad",
            "position",
        ] {
            assert!(
                FunctionRegistry::lookup(name).is_some(),
                "expected {name} to be found"
            );
        }
    }

    #[test]
    fn lookup_all_datetime_functions() {
        for name in &[
            "now",
            "current_timestamp",
            "current_date",
            "date_part",
            "date_trunc",
            "age",
            "to_char",
            "extract",
        ] {
            assert!(
                FunctionRegistry::lookup(name).is_some(),
                "expected {name} to be found"
            );
        }
    }

    #[test]
    fn lookup_substr_alias() {
        let info = FunctionRegistry::lookup("substr").unwrap();
        assert_eq!(info.func, ScalarFunction::Substring);
    }

    #[test]
    fn lookup_character_length_alias() {
        let info = FunctionRegistry::lookup("character_length").unwrap();
        assert_eq!(info.func, ScalarFunction::CharLength);
    }

    #[test]
    fn lookup_jsonb_path_query_is_implemented() {
        // jsonb_path_query is now fully implemented (not reserved)
        let info = FunctionRegistry::lookup("jsonb_path_query")
            .expect("should find implemented jsonb_path_query");
        assert_eq!(info.min_args, 2);
        assert_eq!(info.max_args, Some(4));
    }

    #[test]
    fn lookup_numrange_is_implemented() {
        // numrange is now a fully implemented function (not reserved)
        let info = FunctionRegistry::lookup("numrange").expect("should find implemented numrange");
        assert_eq!(info.min_args, 0);
    }

    #[test]
    fn lookup_currval_is_executor_backed() {
        assert!(FunctionRegistry::lookup("currval").is_some());
        assert!(FunctionRegistry::lookup_reserved("currval").is_none());
    }

    #[test]
    fn lookup_jsonb_array_elements_reports_jsonb_output_type() {
        let info = FunctionRegistry::lookup("jsonb_array_elements")
            .expect("jsonb_array_elements should exist");
        assert_eq!(info.return_type, DataType::Jsonb);
    }

    #[test]
    fn lookup_jsonb_each_internal_values_reports_jsonb_output_type() {
        let info = FunctionRegistry::lookup("__aiondb_jsonb_each_values")
            .expect("__aiondb_jsonb_each_values should exist");
        assert_eq!(info.return_type, DataType::Jsonb);
    }
}
