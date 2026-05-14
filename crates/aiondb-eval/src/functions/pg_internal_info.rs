use super::*;

pub(super) fn lookup(name: &str) -> Option<FunctionInfo> {
    match name {
        // Type name constructors
        "name" => Some(FunctionInfo {
            func: ScalarFunction::Generic("name".into()),
            return_type: DataType::Text,
            min_args: 1,
            max_args: Some(1),
        }),
        // inet/cidr containment operators routed through generic dispatch.
        // Names are internal - they're emitted by the parser when it
        // rewrites `<<=` / `>>=` from a `CustomOp` token into a function
        // call (see parser_expr/operators.rs).
        "inet_subnet_contained_by_or_equals" | "inet_subnet_contains_or_equals" => {
            Some(FunctionInfo {
                func: ScalarFunction::Generic(name.into()),
                return_type: DataType::Boolean,
                min_args: 2,
                max_args: Some(2),
            })
        }
        // Stats functions
        "pg_stat_force_next_flush" => Some(FunctionInfo {
            func: ScalarFunction::Generic("pg_stat_force_next_flush".into()),
            return_type: DataType::Text,
            min_args: 0,
            max_args: Some(0),
        }),
        "pg_stat_have_stats" => Some(FunctionInfo {
            func: ScalarFunction::Generic("pg_stat_have_stats".into()),
            return_type: DataType::Boolean,
            min_args: 3,
            max_args: Some(3),
        }),
        // Size utility
        "pg_size_bytes" => Some(FunctionInfo {
            func: ScalarFunction::Generic("pg_size_bytes".into()),
            return_type: DataType::BigInt,
            min_args: 1,
            max_args: Some(1),
        }),
        // Large object functions returning int4 (lo_create, lo_open, lo_close,
        // lo_unlink, lo_lseek, lo_tell, lo_truncate, lowrite). 64-bit
        // variants (`lo_lseek64`, `lo_tell64`, `lo_truncate64`) are matched
        // separately further down with `BigInt` return type.
        "lo_create" | "lo_open" | "lo_close" | "lo_unlink" | "lo_lseek" | "lo_tell"
        | "lo_truncate" | "lowrite" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.into()),
            return_type: DataType::Int,
            min_args: 1,
            max_args: Some(3),
        }),
        "loread" | "lo_get" | "lo_put" | "lo_from_bytea" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.into()),
            return_type: DataType::Blob,
            min_args: 1,
            max_args: Some(3),
        }),
        "lo_import" | "lo_export" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.into()),
            return_type: DataType::Int,
            min_args: 1,
            max_args: Some(2),
        }),
        // Binary I/O functions
        "float4send" | "float8send" | "int4send" | "int8send" | "int2send" | "textsend"
        | "boolsend" | "oidsend" | "byteasend" | "float4recv" | "float8recv" | "int4recv"
        | "int8recv" | "int2recv" | "textrecv" | "boolrecv" | "oidrecv" | "bytearecv" => {
            Some(FunctionInfo {
                func: ScalarFunction::Generic(name.into()),
                return_type: DataType::Blob,
                min_args: 1,
                max_args: Some(3),
            })
        }
        // XML validation
        "xml_is_well_formed" | "xml_is_well_formed_document" | "xml_is_well_formed_content" => {
            Some(FunctionInfo {
                func: ScalarFunction::Generic(name.into()),
                return_type: DataType::Boolean,
                min_args: 1,
                max_args: Some(1),
            })
        }
        // tsquery constructor
        "tsquery" => Some(FunctionInfo {
            func: ScalarFunction::Generic("tsquery".into()),
            return_type: DataType::Text,
            min_args: 1,
            max_args: Some(1),
        }),
        // Text search utility
        "ts_delete" | "ts_filter" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.into()),
            return_type: DataType::Text,
            min_args: 2,
            max_args: Some(2),
        }),
        // Range aggregate functions
        "range_intersect_agg" | "range_agg" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.into()),
            return_type: DataType::Text,
            min_args: 1,
            max_args: Some(1),
        }),
        // Window ranking functions (when parsed as regular functions)
        "rank" | "dense_rank" | "percent_rank" | "cume_dist" | "ntile" | "lag" | "lead"
        | "first_value" | "last_value" | "nth_value" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.into()),
            return_type: DataType::BigInt,
            min_args: 0,
            max_args: None,
        }),
        // Misc utility functions
        "pg_sleep" | "pg_sleep_for" | "pg_sleep_until" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.into()),
            return_type: DataType::Text,
            min_args: 1,
            max_args: Some(1),
        }),
        "txid_current"
        | "txid_current_snapshot"
        | "txid_snapshot_xmin"
        | "txid_snapshot_xmax"
        | "txid_snapshot_xip"
        | "pg_current_xact_id"
        | "pg_current_snapshot"
        | "pg_snapshot_xmin"
        | "pg_snapshot_xmax"
        | "pg_snapshot_xip"
        | "pg_xact_status"
        | "pg_visible_in_snapshot" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.into()),
            return_type: DataType::BigInt,
            min_args: 0,
            max_args: Some(1),
        }),
        "date" => Some(FunctionInfo {
            func: ScalarFunction::Generic("date".into()),
            return_type: DataType::Date,
            min_args: 1,
            max_args: Some(1),
        }),
        "timestamp" => Some(FunctionInfo {
            func: ScalarFunction::Generic("timestamp".into()),
            return_type: DataType::Timestamp,
            min_args: 1,
            max_args: Some(2),
        }),
        "timestamptz" => Some(FunctionInfo {
            func: ScalarFunction::Generic("timestamptz".into()),
            return_type: DataType::TimestampTz,
            min_args: 1,
            max_args: Some(2),
        }),
        "time" => Some(FunctionInfo {
            func: ScalarFunction::Generic("time".into()),
            return_type: DataType::Time,
            min_args: 1,
            max_args: Some(1),
        }),
        "timetz" => Some(FunctionInfo {
            func: ScalarFunction::Generic("timetz".into()),
            return_type: DataType::TimeTz,
            min_args: 1,
            max_args: Some(1),
        }),
        "interval" => Some(FunctionInfo {
            func: ScalarFunction::Generic("interval".into()),
            return_type: DataType::Interval,
            min_args: 1,
            max_args: Some(2),
        }),
        // Type cast/constructor functions - typed return values
        "int2" | "int4" | "oid" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.into()),
            return_type: DataType::Int,
            min_args: 1,
            max_args: Some(2),
        }),
        "int8" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.into()),
            return_type: DataType::BigInt,
            min_args: 1,
            max_args: Some(2),
        }),
        "float4" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.into()),
            return_type: DataType::Real,
            min_args: 1,
            max_args: Some(2),
        }),
        "float8" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.into()),
            return_type: DataType::Double,
            min_args: 1,
            max_args: Some(2),
        }),
        "numeric" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.into()),
            return_type: DataType::Numeric,
            min_args: 1,
            max_args: Some(2),
        }),
        "bool" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.into()),
            return_type: DataType::Boolean,
            min_args: 1,
            max_args: Some(2),
        }),
        // Type cast/constructor functions - text return type
        "char" | "cidr" | "inet" | "pg_lsn" | "varchar" | "bpchar" | "bytea" | "bit" | "varbit"
        | "money" | "uuid" | "xml" | "jsonb" | "json" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.into()),
            return_type: DataType::Text,
            min_args: 1,
            max_args: Some(2),
        }),
        // Geometry functions
        "bound_box" | "circle_center" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.into()),
            return_type: DataType::Text,
            min_args: 1,
            max_args: Some(2),
        }),
        // Money functions
        "cash_words" => Some(FunctionInfo {
            func: ScalarFunction::Generic("cash_words".into()),
            return_type: DataType::Text,
            min_args: 1,
            max_args: Some(1),
        }),
        // Hash functions (internal/system)
        "interval_hash"
        | "hashfloat4"
        | "hashfloat8"
        | "hashint2"
        | "hashint4"
        | "hashint8"
        | "hashtext"
        | "hashname"
        | "hashoid"
        | "hashoidvector"
        | "hashbpchar"
        | "hashchar"
        | "hashenum"
        | "hashinet"
        | "hashmacaddr"
        | "hashmacaddr8"
        | "hash_array"
        | "hash_multirange"
        | "hash_numeric"
        | "hash_range"
        | "hash_record"
        | "time_hash"
        | "timestamp_hash"
        | "timetz_hash"
        | "uuid_hash"
        | "pg_lsn_hash"
        | "xid8cmp"
        | "hash_aclitem"
        | "hash_record_extended"
        | "hash_array_extended"
        | "hash_range_extended"
        | "hash_multirange_extended"
        | "hashfloat4extended"
        | "hashfloat8extended"
        | "hashint2extended"
        | "hashint4extended"
        | "hashint8extended"
        | "hashtextextended"
        | "hashbpcharextended"
        | "hashcharextended"
        | "hashenumextended"
        | "hashinetextended"
        | "hashmacaddrextended"
        | "hashmacaddr8extended"
        | "hashnameextended"
        | "hashoidextended"
        | "hashoidvectorextended" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.into()),
            return_type: DataType::Int,
            min_args: 1,
            max_args: Some(2),
        }),
        // PG system / catalog functions
        "current_database" | "getdatabaseencoding" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.into()),
            return_type: DataType::Text,
            min_args: 0,
            max_args: Some(0),
        }),
        "current_schemas" => Some(FunctionInfo {
            func: ScalarFunction::Generic("current_schemas".into()),
            return_type: DataType::Array(Box::new(DataType::Text)),
            min_args: 1,
            max_args: Some(1),
        }),
        "format_type" => Some(FunctionInfo {
            func: ScalarFunction::Generic("format_type".into()),
            return_type: DataType::Text,
            min_args: 2,
            max_args: Some(2),
        }),
        "gen_random_uuid" | "uuid_generate_v4" => Some(FunctionInfo {
            func: ScalarFunction::Generic("gen_random_uuid".into()),
            return_type: DataType::Uuid,
            min_args: 0,
            max_args: Some(0),
        }),
        "pg_advisory_lock_shared" | "pg_advisory_xact_lock_shared" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.into()),
            return_type: DataType::Boolean,
            min_args: 1,
            max_args: Some(2),
        }),
        "pg_advisory_unlock_shared" => Some(FunctionInfo {
            func: ScalarFunction::Generic("pg_advisory_unlock_shared".into()),
            return_type: DataType::Boolean,
            min_args: 1,
            max_args: Some(2),
        }),
        "pg_current_xact_id_if_assigned" | "txid_current_if_assigned" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.into()),
            return_type: DataType::BigInt,
            min_args: 0,
            max_args: Some(0),
        }),
        "pg_get_function_arg_default" => Some(FunctionInfo {
            func: ScalarFunction::Generic("pg_get_function_arg_default".into()),
            return_type: DataType::Text,
            min_args: 2,
            max_args: Some(2),
        }),
        "pg_get_keywords" => Some(FunctionInfo {
            func: ScalarFunction::Generic("pg_get_keywords".into()),
            return_type: DataType::Text,
            min_args: 0,
            max_args: Some(0),
        }),
        "pg_log_backend_memory_contexts" => Some(FunctionInfo {
            func: ScalarFunction::Generic("pg_log_backend_memory_contexts".into()),
            return_type: DataType::Boolean,
            min_args: 1,
            max_args: Some(1),
        }),
        "pg_ls_dir"
        | "pg_ls_waldir"
        | "pg_ls_logdir"
        | "pg_ls_archive_statusdir"
        | "pg_ls_tmpdir" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.into()),
            return_type: DataType::Text,
            min_args: 0,
            max_args: Some(3),
        }),
        "pg_my_temp_schema" => Some(FunctionInfo {
            func: ScalarFunction::Generic("pg_my_temp_schema".into()),
            return_type: DataType::Int,
            min_args: 0,
            max_args: Some(0),
        }),
        "pg_read_file" => Some(FunctionInfo {
            func: ScalarFunction::Generic("pg_read_file".into()),
            return_type: DataType::Text,
            min_args: 1,
            max_args: Some(4),
        }),
        "pg_read_binary_file" => Some(FunctionInfo {
            func: ScalarFunction::Generic("pg_read_binary_file".into()),
            return_type: DataType::Blob,
            min_args: 1,
            max_args: Some(4),
        }),
        "pg_settings_get_flags" => Some(FunctionInfo {
            func: ScalarFunction::Generic("pg_settings_get_flags".into()),
            return_type: DataType::Array(Box::new(DataType::Text)),
            min_args: 1,
            max_args: Some(1),
        }),
        "pg_stat_get_backend_pid" => Some(FunctionInfo {
            func: ScalarFunction::Generic("pg_stat_get_backend_pid".into()),
            return_type: DataType::Int,
            min_args: 1,
            max_args: Some(1),
        }),
        "pg_stat_get_snapshot_timestamp" | "pg_stat_clear_snapshot" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.into()),
            return_type: DataType::TimestampTz,
            min_args: 0,
            max_args: Some(0),
        }),
        "pg_stat_get_function_calls" | "pg_stat_get_xact_function_calls" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.into()),
            return_type: DataType::BigInt,
            min_args: 1,
            max_args: Some(1),
        }),
        "pg_stat_get_replication_slot" | "pg_stat_get_subscription_stats" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.into()),
            return_type: DataType::Text,
            min_args: 1,
            max_args: Some(1),
        }),
        "pg_stat_get_tuples_hot_updated"
        | "pg_stat_get_live_tuples"
        | "pg_stat_get_tuples_inserted"
        | "pg_stat_get_tuples_updated"
        | "pg_stat_get_tuples_deleted"
        | "pg_stat_get_xact_tuples_inserted"
        | "pg_stat_get_xact_tuples_updated"
        | "pg_stat_get_xact_tuples_deleted" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.into()),
            return_type: DataType::BigInt,
            min_args: 1,
            max_args: Some(1),
        }),
        "pg_stat_reset" => Some(FunctionInfo {
            func: ScalarFunction::Generic("pg_stat_reset".into()),
            return_type: DataType::Text,
            min_args: 0,
            max_args: Some(0),
        }),
        "pg_stat_reset_single_table_counters" => Some(FunctionInfo {
            func: ScalarFunction::Generic("pg_stat_reset_single_table_counters".into()),
            return_type: DataType::Text,
            min_args: 1,
            max_args: Some(1),
        }),
        "pg_stat_reset_shared" | "pg_stat_reset_slru" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.into()),
            return_type: DataType::Text,
            min_args: 1,
            max_args: Some(1),
        }),
        "pg_trigger_depth" => Some(FunctionInfo {
            func: ScalarFunction::Generic("pg_trigger_depth".into()),
            return_type: DataType::Int,
            min_args: 0,
            max_args: Some(0),
        }),
        "pg_catalog.pg_column_is_updatable" => Some(FunctionInfo {
            func: ScalarFunction::Generic("pg_catalog.pg_column_is_updatable".into()),
            return_type: DataType::Boolean,
            min_args: 2,
            max_args: Some(3),
        }),
        "txid_status" => Some(FunctionInfo {
            func: ScalarFunction::Generic("txid_status".into()),
            return_type: DataType::Text,
            min_args: 1,
            max_args: Some(1),
        }),
        "txid_visible_in_snapshot" => Some(FunctionInfo {
            func: ScalarFunction::Generic("txid_visible_in_snapshot".into()),
            return_type: DataType::Boolean,
            min_args: 2,
            max_args: Some(2),
        }),
        // Text search functions
        "numnode" => Some(FunctionInfo {
            func: ScalarFunction::Generic("numnode".into()),
            return_type: DataType::Int,
            min_args: 1,
            max_args: Some(1),
        }),
        "setweight" => Some(FunctionInfo {
            func: ScalarFunction::Generic("setweight".into()),
            return_type: DataType::Text,
            min_args: 2,
            max_args: Some(3),
        }),
        "strip" => Some(FunctionInfo {
            func: ScalarFunction::Generic("strip".into()),
            return_type: DataType::Text,
            min_args: 1,
            max_args: Some(1),
        }),
        "tsquery_phrase" => Some(FunctionInfo {
            func: ScalarFunction::Generic("tsquery_phrase".into()),
            return_type: DataType::Text,
            min_args: 2,
            max_args: Some(3),
        }),
        "tsvector_to_array" => Some(FunctionInfo {
            func: ScalarFunction::Generic("tsvector_to_array".into()),
            return_type: DataType::Array(Box::new(DataType::Text)),
            min_args: 1,
            max_args: Some(1),
        }),
        "tsvector_update_trigger" => Some(FunctionInfo {
            func: ScalarFunction::Generic("tsvector_update_trigger".into()),
            return_type: DataType::Text,
            min_args: 0,
            max_args: None,
        }),
        // Range operator functions
        "range_adjacent" => Some(FunctionInfo {
            func: ScalarFunction::Generic("range_adjacent".into()),
            return_type: DataType::Boolean,
            min_args: 2,
            max_args: Some(2),
        }),
        "range_not_extend_right" | "range_not_extend_left" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.into()),
            return_type: DataType::Boolean,
            min_args: 2,
            max_args: Some(2),
        }),
        "range_contained_by" | "range_contains" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.into()),
            return_type: DataType::Boolean,
            min_args: 2,
            max_args: Some(2),
        }),
        "range_minus" => Some(FunctionInfo {
            func: ScalarFunction::Generic("range_minus".into()),
            return_type: DataType::Text,
            min_args: 2,
            max_args: Some(2),
        }),
        "range_overlaps_multirange"
        | "range_contained_by_multirange"
        | "elem_contained_by_multirange"
        | "multirange_contained_by_multirange"
        | "multirange_contains_elem"
        | "multirange_contains_multirange"
        | "multirange_contains_range"
        | "multirange_overlaps_multirange"
        | "multirange_overlaps_range" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.into()),
            return_type: DataType::Boolean,
            min_args: 2,
            max_args: Some(2),
        }),
        "multirange" => Some(FunctionInfo {
            func: ScalarFunction::Generic("multirange".into()),
            return_type: DataType::Text,
            min_args: 0,
            max_args: None,
        }),
        "float8range"
        | "float8multirange"
        | "textmultirange"
        | "textrange"
        | "arrayrange"
        | "arraymultirange"
        | "intr_multirange"
        | "two_ints_range"
        | "two_ints_multirange"
        | "textrange1"
        | "_textrange1"
        | "textrange2"
        | "textrange_c"
        | "textrange_en_us" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.into()),
            return_type: DataType::Text,
            min_args: 0,
            max_args: None,
        }),
        "currtid2" => Some(FunctionInfo {
            func: ScalarFunction::Generic("currtid2".into()),
            return_type: DataType::Text,
            min_args: 2,
            max_args: Some(2),
        }),
        // Collation introspection
        "pg_collation_for" => Some(FunctionInfo {
            func: ScalarFunction::Generic("pg_collation_for".into()),
            return_type: DataType::Text,
            min_args: 1,
            max_args: Some(1),
        }),
        // Reg* type constructor functions
        "regcollation" | "regnamespace" | "regoper" | "regoperator" | "regproc"
        | "regprocedure" | "regrole" | "to_regcollation" | "regconfig" | "regdictionary" => {
            Some(FunctionInfo {
                func: ScalarFunction::Generic(name.into()),
                return_type: DataType::Text,
                min_args: 1,
                max_args: Some(1),
            })
        }
        // XML aggregate/table functions
        "xmlagg" => Some(FunctionInfo {
            func: ScalarFunction::Generic("xmlagg".into()),
            return_type: DataType::Text,
            min_args: 1,
            max_args: Some(1),
        }),
        "table_to_xml"
        | "table_to_xml_and_xmlschema"
        | "table_to_xmlschema"
        | "schema_to_xml"
        | "schema_to_xml_and_xmlschema"
        | "schema_to_xmlschema"
        | "query_to_xml"
        | "query_to_xml_and_xmlschema"
        | "query_to_xmlschema"
        | "cursor_to_xml"
        | "cursor_to_xmlschema"
        | "database_to_xml"
        | "database_to_xml_and_xmlschema"
        | "database_to_xmlschema" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.into()),
            return_type: DataType::Text,
            min_args: 1,
            max_args: None,
        }),
        // Large object extended functions
        "lo_creat" => Some(FunctionInfo {
            func: ScalarFunction::Generic("lo_creat".into()),
            return_type: DataType::Int,
            min_args: 1,
            max_args: Some(1),
        }),
        "lo_lseek64" | "lo_tell64" | "lo_truncate64" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.into()),
            return_type: DataType::BigInt,
            min_args: 1,
            max_args: Some(3),
        }),
        // Internal aggregate accumulator functions
        "float8_accum"
        | "float8_combine"
        | "float8_regr_combine"
        | "numeric_avg_accum"
        | "numeric_avg_combine"
        | "int8_avg_accum"
        | "int8_avg_combine"
        | "int2_accum"
        | "int4_accum"
        | "int8_accum"
        | "int2_sum"
        | "int4_sum"
        | "int8_sum" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.into()),
            return_type: DataType::Text,
            min_args: 1,
            max_args: Some(2),
        }),
        "float8_regr_accum" => Some(FunctionInfo {
            func: ScalarFunction::Generic("float8_regr_accum".into()),
            return_type: DataType::Text,
            min_args: 3,
            max_args: Some(3),
        }),
        // Boolean operator functions
        "booland_statefunc" | "boolor_statefunc" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.into()),
            return_type: DataType::Boolean,
            min_args: 2,
            max_args: Some(2),
        }),
        "booleq" | "boolne" | "boollt" | "boolgt" | "boolle" | "boolge" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.into()),
            return_type: DataType::Boolean,
            min_args: 2,
            max_args: Some(2),
        }),
        // I/O internal functions
        "anyrange_in" | "array_in" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.into()),
            return_type: DataType::Text,
            min_args: 1,
            max_args: Some(3),
        }),
        // GIN/GiST maintenance functions
        "gin_clean_pending_list" => Some(FunctionInfo {
            func: ScalarFunction::Generic("gin_clean_pending_list".into()),
            return_type: DataType::BigInt,
            min_args: 1,
            max_args: Some(1),
        }),
        // Miscellaneous PG built-in functions
        "values" => Some(FunctionInfo {
            func: ScalarFunction::Generic("values".into()),
            return_type: DataType::Text,
            min_args: 0,
            max_args: None,
        }),
        "set" => Some(FunctionInfo {
            func: ScalarFunction::Generic("set".into()),
            return_type: DataType::Text,
            min_args: 1,
            max_args: None,
        }),
        "suppress_redundant_updates_trigger" => Some(FunctionInfo {
            func: ScalarFunction::Generic("suppress_redundant_updates_trigger".into()),
            return_type: DataType::Text,
            min_args: 0,
            max_args: Some(0),
        }),
        "pg_stat_get_xact_blocks_fetched"
        | "pg_stat_get_xact_blocks_hit"
        | "pg_stat_get_blocks_fetched"
        | "pg_stat_get_blocks_hit"
        | "pg_stat_get_tuples_fetched"
        | "pg_stat_get_tuples_returned" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.into()),
            return_type: DataType::BigInt,
            min_args: 1,
            max_args: Some(1),
        }),
        _ => None,
    }
}
