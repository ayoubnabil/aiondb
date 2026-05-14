use super::*;

pub(super) fn lookup(name: &str) -> Option<FunctionInfo> {
    match name {
        // ── Implemented utility functions ──
        "pg_typeof" => Some(FunctionInfo {
            func: ScalarFunction::PgTypeof,
            return_type: DataType::Text,
            min_args: 1,
            max_args: Some(1),
        }),
        "row" => Some(FunctionInfo {
            func: ScalarFunction::Row,
            return_type: DataType::Text,
            min_args: 0,
            max_args: None, // variadic
        }),
        // ── Implemented set-returning functions ──
        "generate_series" => Some(FunctionInfo {
            func: ScalarFunction::GenerateSeries,
            return_type: DataType::Int,
            min_args: 2,
            max_args: Some(4),
        }),
        "generate_subscripts" => Some(FunctionInfo {
            func: ScalarFunction::Generic("generate_subscripts".into()),
            return_type: DataType::Int,
            min_args: 2,
            max_args: Some(3),
        }),
        "unnest" => Some(FunctionInfo {
            func: ScalarFunction::Unnest,
            return_type: DataType::Text,
            min_args: 1,
            max_args: Some(1),
        }),
        "setval" => Some(FunctionInfo {
            func: ScalarFunction::Generic("setval".into()),
            return_type: DataType::BigInt,
            min_args: 2,
            max_args: Some(3),
        }),
        "currval" => Some(FunctionInfo {
            func: ScalarFunction::Generic("currval".into()),
            return_type: DataType::BigInt,
            min_args: 1,
            max_args: Some(1),
        }),
        "lastval" => Some(FunctionInfo {
            func: ScalarFunction::Generic("lastval".into()),
            return_type: DataType::BigInt,
            min_args: 0,
            max_args: Some(0),
        }),
        "pg_get_viewdef" => Some(FunctionInfo {
            func: ScalarFunction::PgGetViewdef,
            return_type: DataType::Text,
            min_args: 1,
            max_args: Some(2),
        }),
        "pg_get_indexdef" => Some(FunctionInfo {
            func: ScalarFunction::Generic("pg_get_indexdef".into()),
            return_type: DataType::Text,
            min_args: 1,
            max_args: Some(3),
        }),
        "has_table_privilege" => Some(FunctionInfo {
            func: ScalarFunction::Generic("has_table_privilege".into()),
            return_type: DataType::Boolean,
            min_args: 2,
            max_args: Some(3),
        }),
        "has_schema_privilege" => Some(FunctionInfo {
            func: ScalarFunction::Generic("has_schema_privilege".into()),
            return_type: DataType::Boolean,
            min_args: 2,
            max_args: Some(3),
        }),
        "has_column_privilege" => Some(FunctionInfo {
            func: ScalarFunction::Generic("has_column_privilege".into()),
            return_type: DataType::Boolean,
            min_args: 3,
            max_args: Some(4),
        }),
        "has_any_column_privilege" => Some(FunctionInfo {
            func: ScalarFunction::Generic("has_any_column_privilege".into()),
            return_type: DataType::Boolean,
            min_args: 2,
            max_args: Some(3),
        }),
        "has_function_privilege" => Some(FunctionInfo {
            func: ScalarFunction::Generic("has_function_privilege".into()),
            return_type: DataType::Boolean,
            min_args: 2,
            max_args: Some(3),
        }),
        "has_sequence_privilege" => Some(FunctionInfo {
            func: ScalarFunction::Generic("has_sequence_privilege".into()),
            return_type: DataType::Boolean,
            min_args: 2,
            max_args: Some(3),
        }),
        "pg_input_is_valid" => Some(FunctionInfo {
            func: ScalarFunction::PgInputIsValid,
            return_type: DataType::Boolean,
            min_args: 2,
            max_args: Some(2),
        }),
        "pg_input_error_info" => Some(FunctionInfo {
            func: ScalarFunction::Generic("pg_input_error_info".into()),
            return_type: DataType::Text,
            min_args: 2,
            max_args: Some(2),
        }),
        "__aiondb_pg_input_error_info_message" => Some(FunctionInfo {
            func: ScalarFunction::Generic("__aiondb_pg_input_error_info_message".into()),
            return_type: DataType::Text,
            min_args: 2,
            max_args: Some(2),
        }),
        "__aiondb_pg_input_error_info_detail" => Some(FunctionInfo {
            func: ScalarFunction::Generic("__aiondb_pg_input_error_info_detail".into()),
            return_type: DataType::Text,
            min_args: 2,
            max_args: Some(2),
        }),
        "__aiondb_pg_input_error_info_hint" => Some(FunctionInfo {
            func: ScalarFunction::Generic("__aiondb_pg_input_error_info_hint".into()),
            return_type: DataType::Text,
            min_args: 2,
            max_args: Some(2),
        }),
        "__aiondb_pg_input_error_info_sqlstate" => Some(FunctionInfo {
            func: ScalarFunction::Generic("__aiondb_pg_input_error_info_sqlstate".into()),
            return_type: DataType::Text,
            min_args: 2,
            max_args: Some(2),
        }),
        "__aiondb_pg_char_cast" => Some(FunctionInfo {
            func: ScalarFunction::Generic("__aiondb_pg_char_cast".into()),
            return_type: DataType::Text,
            min_args: 1,
            max_args: Some(1),
        }),
        "__aiondb_regclass_cast" => Some(FunctionInfo {
            func: ScalarFunction::Generic("__aiondb_regclass_cast".into()),
            return_type: DataType::Int,
            min_args: 1,
            max_args: Some(1),
        }),
        "__aiondb_regproc_cast" => Some(FunctionInfo {
            func: ScalarFunction::Generic("__aiondb_regproc_cast".into()),
            return_type: DataType::Int,
            min_args: 1,
            max_args: Some(1),
        }),
        "__aiondb_regprocedure_cast" => Some(FunctionInfo {
            func: ScalarFunction::Generic("__aiondb_regprocedure_cast".into()),
            return_type: DataType::Int,
            min_args: 1,
            max_args: Some(1),
        }),
        "__aiondb_regtype_cast" => Some(FunctionInfo {
            func: ScalarFunction::Generic("__aiondb_regtype_cast".into()),
            return_type: DataType::Int,
            min_args: 1,
            max_args: Some(1),
        }),
        "__aiondb_regtype_out" => Some(FunctionInfo {
            func: ScalarFunction::Generic("__aiondb_regtype_out".into()),
            return_type: DataType::Text,
            min_args: 1,
            max_args: Some(1),
        }),
        "__aiondb_regrole_cast" => Some(FunctionInfo {
            func: ScalarFunction::Generic("__aiondb_regrole_cast".into()),
            return_type: DataType::Int,
            min_args: 1,
            max_args: Some(1),
        }),
        "__aiondb_xid_cast" => Some(FunctionInfo {
            func: ScalarFunction::Generic("__aiondb_xid_cast".into()),
            return_type: DataType::BigInt,
            min_args: 1,
            max_args: Some(1),
        }),
        "__aiondb_xid8_cast" => Some(FunctionInfo {
            func: ScalarFunction::Generic("__aiondb_xid8_cast".into()),
            return_type: DataType::Numeric,
            min_args: 1,
            max_args: Some(1),
        }),
        "__aiondb_pg_snapshot_cast" => Some(FunctionInfo {
            func: ScalarFunction::Generic("__aiondb_pg_snapshot_cast".into()),
            return_type: DataType::Text,
            min_args: 1,
            max_args: Some(1),
        }),
        "__aiondb_regclass_out" => Some(FunctionInfo {
            func: ScalarFunction::Generic("__aiondb_regclass_out".into()),
            return_type: DataType::Text,
            min_args: 1,
            max_args: Some(1),
        }),
        "__aiondb_regproc_out" => Some(FunctionInfo {
            func: ScalarFunction::Generic("__aiondb_regproc_out".into()),
            return_type: DataType::Text,
            min_args: 1,
            max_args: Some(1),
        }),
        "__aiondb_regprocedure_out" => Some(FunctionInfo {
            func: ScalarFunction::Generic("__aiondb_regprocedure_out".into()),
            return_type: DataType::Text,
            min_args: 1,
            max_args: Some(1),
        }),
        "__aiondb_regrole_out" => Some(FunctionInfo {
            func: ScalarFunction::Generic("__aiondb_regrole_out".into()),
            return_type: DataType::Text,
            min_args: 1,
            max_args: Some(1),
        }),
        "__aiondb_compat_cast" => Some(FunctionInfo {
            func: ScalarFunction::Generic("__aiondb_compat_cast".into()),
            return_type: DataType::Text,
            min_args: 3,
            max_args: Some(3),
        }),
        "__aiondb_composite_field" => Some(FunctionInfo {
            func: ScalarFunction::Generic("__aiondb_composite_field".into()),
            return_type: DataType::Text,
            min_args: 2,
            max_args: Some(2),
        }),
        "__aiondb_composite_assign" => Some(FunctionInfo {
            func: ScalarFunction::Generic("__aiondb_composite_assign".into()),
            return_type: DataType::Text,
            min_args: 3,
            max_args: Some(3),
        }),
        "obj_description" | "pg_catalog.obj_description" => Some(FunctionInfo {
            func: ScalarFunction::Generic("obj_description".into()),
            return_type: DataType::Text,
            min_args: 1,
            max_args: Some(2),
        }),
        "shobj_description" | "pg_catalog.shobj_description" => Some(FunctionInfo {
            func: ScalarFunction::Generic("shobj_description".into()),
            return_type: DataType::Text,
            min_args: 2,
            max_args: Some(2),
        }),
        "col_description" | "pg_catalog.col_description" => Some(FunctionInfo {
            func: ScalarFunction::Generic("col_description".into()),
            return_type: DataType::Text,
            min_args: 2,
            max_args: Some(2),
        }),
        "pg_get_expr" => Some(FunctionInfo {
            func: ScalarFunction::Generic("pg_get_expr".into()),
            return_type: DataType::Text,
            min_args: 2,
            max_args: Some(3),
        }),
        "pg_get_constraintdef" => Some(FunctionInfo {
            func: ScalarFunction::Generic("pg_get_constraintdef".into()),
            return_type: DataType::Text,
            min_args: 1,
            max_args: Some(2),
        }),
        "pg_catalog.pg_get_viewdef" => Some(FunctionInfo {
            func: ScalarFunction::PgGetViewdef,
            return_type: DataType::Text,
            min_args: 1,
            max_args: Some(2),
        }),
        "pg_catalog.pg_get_indexdef" => Some(FunctionInfo {
            func: ScalarFunction::Generic("pg_catalog.pg_get_indexdef".into()),
            return_type: DataType::Text,
            min_args: 1,
            max_args: Some(3),
        }),
        // Range type constructors
        "int4range" => Some(FunctionInfo {
            func: ScalarFunction::Int4Range,
            return_type: DataType::Text,
            min_args: 0,
            max_args: Some(3),
        }),
        "int8range" => Some(FunctionInfo {
            func: ScalarFunction::Int8Range,
            return_type: DataType::Text,
            min_args: 0,
            max_args: Some(3),
        }),
        "numrange" => Some(FunctionInfo {
            func: ScalarFunction::NumRange,
            return_type: DataType::Text,
            min_args: 0,
            max_args: Some(3),
        }),
        "daterange" => Some(FunctionInfo {
            func: ScalarFunction::DateRange,
            return_type: DataType::Text,
            min_args: 0,
            max_args: Some(3),
        }),
        "tsrange" => Some(FunctionInfo {
            func: ScalarFunction::TsRange,
            return_type: DataType::Text,
            min_args: 0,
            max_args: Some(3),
        }),
        "tstzrange" => Some(FunctionInfo {
            func: ScalarFunction::TsTzRange,
            return_type: DataType::Text,
            min_args: 0,
            max_args: Some(3),
        }),
        // Multirange constructors
        "nummultirange" => Some(FunctionInfo {
            func: ScalarFunction::NumMultirange,
            return_type: DataType::Text,
            min_args: 0,
            max_args: None,
        }),
        "int4multirange" => Some(FunctionInfo {
            func: ScalarFunction::Int4Multirange,
            return_type: DataType::Text,
            min_args: 0,
            max_args: None,
        }),
        "int8multirange" => Some(FunctionInfo {
            func: ScalarFunction::Int8Multirange,
            return_type: DataType::Text,
            min_args: 0,
            max_args: None,
        }),
        "datemultirange" => Some(FunctionInfo {
            func: ScalarFunction::DateMultirange,
            return_type: DataType::Text,
            min_args: 0,
            max_args: None,
        }),
        "tsmultirange" => Some(FunctionInfo {
            func: ScalarFunction::TsMultirange,
            return_type: DataType::Text,
            min_args: 0,
            max_args: None,
        }),
        "tstzmultirange" => Some(FunctionInfo {
            func: ScalarFunction::TsTzMultirange,
            return_type: DataType::Text,
            min_args: 0,
            max_args: None,
        }),
        // Range functions
        "isempty" => Some(FunctionInfo {
            func: ScalarFunction::RangeIsEmpty,
            return_type: DataType::Boolean,
            min_args: 1,
            max_args: Some(1),
        }),
        "lower_inc" => Some(FunctionInfo {
            func: ScalarFunction::RangeLowerInc,
            return_type: DataType::Boolean,
            min_args: 1,
            max_args: Some(1),
        }),
        "upper_inc" => Some(FunctionInfo {
            func: ScalarFunction::RangeUpperInc,
            return_type: DataType::Boolean,
            min_args: 1,
            max_args: Some(1),
        }),
        "lower_inf" => Some(FunctionInfo {
            func: ScalarFunction::RangeLowerInf,
            return_type: DataType::Boolean,
            min_args: 1,
            max_args: Some(1),
        }),
        "upper_inf" => Some(FunctionInfo {
            func: ScalarFunction::RangeUpperInf,
            return_type: DataType::Boolean,
            min_args: 1,
            max_args: Some(1),
        }),
        "range_merge" => Some(FunctionInfo {
            func: ScalarFunction::RangeMerge,
            return_type: DataType::Text,
            min_args: 1,
            max_args: Some(2),
        }),
        "range_contains" | "range_contains_elem" => Some(FunctionInfo {
            func: ScalarFunction::RangeContains,
            return_type: DataType::Boolean,
            min_args: 2,
            max_args: Some(2),
        }),
        "range_contained_by" | "elem_contained_by_range" => Some(FunctionInfo {
            func: ScalarFunction::RangeContainedBy,
            return_type: DataType::Boolean,
            min_args: 2,
            max_args: Some(2),
        }),
        "range_adjacent" => Some(FunctionInfo {
            func: ScalarFunction::RangeAdjacent,
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
        "range_minus" => Some(FunctionInfo {
            func: ScalarFunction::Generic("range_minus".into()),
            return_type: DataType::Text,
            min_args: 2,
            max_args: Some(2),
        }),
        // Text search functions
        "to_tsvector" => Some(FunctionInfo {
            func: ScalarFunction::ToTsvector,
            return_type: DataType::Text,
            min_args: 1,
            max_args: Some(2),
        }),
        "to_tsquery" => Some(FunctionInfo {
            func: ScalarFunction::ToTsquery,
            return_type: DataType::Text,
            min_args: 1,
            max_args: Some(2),
        }),
        "plainto_tsquery" => Some(FunctionInfo {
            func: ScalarFunction::PlaintoTsquery,
            return_type: DataType::Text,
            min_args: 1,
            max_args: Some(2),
        }),
        "phraseto_tsquery" => Some(FunctionInfo {
            func: ScalarFunction::PhrasetoTsquery,
            return_type: DataType::Text,
            min_args: 1,
            max_args: Some(2),
        }),
        "websearch_to_tsquery" => Some(FunctionInfo {
            func: ScalarFunction::WebsearchToTsquery,
            return_type: DataType::Text,
            min_args: 1,
            max_args: Some(2),
        }),
        "ts_lexize" => Some(FunctionInfo {
            func: ScalarFunction::TsLexize,
            return_type: DataType::Array(Box::new(DataType::Text)),
            min_args: 2,
            max_args: Some(2),
        }),
        "ts_headline" => Some(FunctionInfo {
            func: ScalarFunction::TsHeadline,
            return_type: DataType::Text,
            min_args: 2,
            max_args: Some(4),
        }),
        "ts_rank" => Some(FunctionInfo {
            func: ScalarFunction::TsRank,
            return_type: DataType::Real,
            min_args: 2,
            max_args: Some(4),
        }),
        "ts_rank_cd" => Some(FunctionInfo {
            func: ScalarFunction::TsRankCd,
            return_type: DataType::Real,
            min_args: 2,
            max_args: Some(4),
        }),
        "ts_rewrite" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.into()),
            return_type: DataType::Text,
            min_args: 1,
            max_args: None,
        }),
        // XML functions
        "xmlelement" | "xmlforest" | "xmlcomment" | "xmlconcat" | "xmlpi" | "xmlroot"
        | "xmlparse" | "xmlserialize" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.into()),
            return_type: DataType::Text,
            min_args: 0,
            max_args: None,
        }),
        "xpath" | "xpath_exists" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.into()),
            return_type: DataType::Text,
            min_args: 2,
            max_args: Some(3),
        }),
        "any_value" | "every" | "bit_and" | "bit_or" | "bit_xor" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.into()),
            return_type: DataType::Text,
            min_args: 1,
            max_args: Some(1),
        }),
        // Advisory lock functions (session/xact, exclusive/shared, blocking
        // and try-non-blocking variants).
        "pg_advisory_lock"
        | "pg_advisory_lock_shared"
        | "pg_advisory_xact_lock"
        | "pg_advisory_xact_lock_shared"
        | "pg_try_advisory_lock"
        | "pg_try_advisory_lock_shared"
        | "pg_try_advisory_xact_lock"
        | "pg_try_advisory_xact_lock_shared"
        | "pg_advisory_unlock"
        | "pg_advisory_unlock_shared"
        | "pg_advisory_unlock_all" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.into()),
            return_type: DataType::Boolean,
            min_args: 0,
            max_args: Some(2),
        }),
        // pg_catalog utility functions
        "pg_get_object_address" => Some(FunctionInfo {
            func: ScalarFunction::Generic("pg_get_object_address".into()),
            return_type: DataType::Text,
            min_args: 3,
            max_args: Some(3),
        }),
        "pg_identify_object" | "pg_identify_object_as_address" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.into()),
            return_type: DataType::Text,
            min_args: 3,
            max_args: Some(3),
        }),
        "pg_describe_object" => Some(FunctionInfo {
            func: ScalarFunction::Generic("pg_describe_object".into()),
            return_type: DataType::Text,
            min_args: 3,
            max_args: Some(3),
        }),
        "pg_get_functiondef"
        | "pg_get_function_arguments"
        | "pg_get_function_result"
        | "pg_get_function_identity_arguments" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.into()),
            return_type: DataType::Text,
            min_args: 1,
            max_args: Some(1),
        }),
        "pg_get_serial_sequence" => Some(FunctionInfo {
            func: ScalarFunction::Generic("pg_get_serial_sequence".into()),
            return_type: DataType::Text,
            min_args: 2,
            max_args: Some(2),
        }),
        "binary_coercible" => Some(FunctionInfo {
            func: ScalarFunction::Generic("binary_coercible".into()),
            return_type: DataType::Boolean,
            min_args: 2,
            max_args: Some(2),
        }),
        "check_ddl_rewrite" => Some(FunctionInfo {
            func: ScalarFunction::Generic("check_ddl_rewrite".into()),
            return_type: DataType::Boolean,
            min_args: 2,
            max_args: Some(2),
        }),
        "xmlexists" => Some(FunctionInfo {
            func: ScalarFunction::Generic("xmlexists".into()),
            return_type: DataType::Boolean,
            min_args: 2,
            max_args: Some(2),
        }),
        "pg_relation_size" | "pg_table_size" | "pg_total_relation_size" | "pg_indexes_size" => {
            Some(FunctionInfo {
                func: ScalarFunction::Generic(name.into()),
                return_type: DataType::BigInt,
                min_args: 1,
                max_args: Some(2),
            })
        }
        "pg_size_pretty" => Some(FunctionInfo {
            func: ScalarFunction::Generic("pg_size_pretty".into()),
            return_type: DataType::Text,
            min_args: 1,
            max_args: Some(1),
        }),
        "pg_column_size" | "pg_column_compression" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.into()),
            return_type: DataType::Int,
            min_args: 1,
            max_args: Some(1),
        }),
        "pg_stat_get_last_vacuum_time"
        | "pg_stat_get_last_analyze_time"
        | "pg_stat_get_last_autovacuum_time"
        | "pg_stat_get_last_autoanalyze_time" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.into()),
            return_type: DataType::Timestamp,
            min_args: 1,
            max_args: Some(1),
        }),
        "pg_backend_pid" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.into()),
            return_type: DataType::Int,
            min_args: 0,
            max_args: Some(0),
        }),
        "pg_notification_queue_usage" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.into()),
            return_type: DataType::Double,
            min_args: 0,
            max_args: Some(0),
        }),
        "pg_cancel_backend" | "pg_terminate_backend" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.into()),
            return_type: DataType::Boolean,
            min_args: 1,
            max_args: Some(2),
        }),
        "current_setting" => Some(FunctionInfo {
            func: ScalarFunction::Generic("current_setting".into()),
            return_type: DataType::Text,
            min_args: 1,
            max_args: Some(2),
        }),
        "row_security_active" => Some(FunctionInfo {
            func: ScalarFunction::Generic("row_security_active".into()),
            return_type: DataType::Boolean,
            min_args: 1,
            max_args: Some(1),
        }),
        "set_config" => Some(FunctionInfo {
            func: ScalarFunction::Generic("set_config".into()),
            return_type: DataType::Text,
            min_args: 3,
            max_args: Some(3),
        }),
        "pg_get_userbyid" => Some(FunctionInfo {
            func: ScalarFunction::Generic("pg_get_userbyid".into()),
            return_type: DataType::Text,
            min_args: 1,
            max_args: Some(1),
        }),
        "pg_relation_is_publishable" | "pg_catalog.pg_relation_is_publishable" => {
            Some(FunctionInfo {
                func: ScalarFunction::Generic("pg_relation_is_publishable".into()),
                return_type: DataType::Boolean,
                min_args: 1,
                max_args: Some(1),
            })
        }
        "pg_encoding_to_char" | "pg_catalog.pg_encoding_to_char" => Some(FunctionInfo {
            func: ScalarFunction::Generic("pg_encoding_to_char".into()),
            return_type: DataType::Text,
            min_args: 1,
            max_args: Some(1),
        }),
        "pg_char_to_encoding" | "pg_catalog.pg_char_to_encoding" => Some(FunctionInfo {
            func: ScalarFunction::Generic("pg_char_to_encoding".into()),
            return_type: DataType::Int,
            min_args: 1,
            max_args: Some(1),
        }),
        "has_type_privilege"
        | "has_database_privilege"
        | "has_server_privilege"
        | "has_tablespace_privilege"
        | "has_language_privilege"
        | "has_foreign_data_wrapper_privilege" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.into()),
            return_type: DataType::Boolean,
            min_args: 2,
            max_args: Some(3),
        }),
        "pg_has_role" => Some(FunctionInfo {
            func: ScalarFunction::Generic("pg_has_role".into()),
            return_type: DataType::Boolean,
            min_args: 2,
            max_args: Some(3),
        }),
        "aclexplode" | "makeaclitem" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.into()),
            return_type: DataType::Text,
            min_args: 1,
            max_args: Some(4),
        }),
        "to_regclass" | "to_regtype" | "to_regproc" | "to_regprocedure" | "to_regoper"
        | "to_regoperator" | "to_regnamespace" | "to_regrole" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.into()),
            return_type: DataType::Text,
            min_args: 1,
            max_args: Some(1),
        }),
        "regclass" | "regtype" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.into()),
            return_type: DataType::Text,
            min_args: 1,
            max_args: Some(1),
        }),
        "enum_first" | "enum_last" | "enum_range" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.into()),
            return_type: DataType::Text,
            min_args: 1,
            max_args: Some(2),
        }),
        "pg_tablespace_location" => Some(FunctionInfo {
            func: ScalarFunction::Generic("pg_tablespace_location".into()),
            return_type: DataType::Text,
            min_args: 1,
            max_args: Some(1),
        }),
        "pg_tablespace_databases" => Some(FunctionInfo {
            func: ScalarFunction::Generic("pg_tablespace_databases".into()),
            return_type: DataType::Int,
            min_args: 1,
            max_args: Some(1),
        }),
        "pg_stat_get_tuples_inserted"
        | "pg_stat_get_tuples_updated"
        | "pg_stat_get_tuples_deleted"
        | "pg_stat_get_live_tuples"
        | "pg_stat_get_dead_tuples"
        | "pg_stat_get_numscans" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.into()),
            return_type: DataType::BigInt,
            min_args: 1,
            max_args: Some(1),
        }),
        "pg_client_encoding" | "inet_client_addr" | "inet_server_addr" | "version"
        | "current_schema" | "current_catalog" | "current_query" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.into()),
            return_type: DataType::Text,
            min_args: 0,
            max_args: Some(0),
        }),
        "inet_client_port" | "inet_server_port" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.into()),
            return_type: DataType::Int,
            min_args: 0,
            max_args: Some(0),
        }),
        "pg_postmaster_start_time" | "pg_conf_load_time" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.into()),
            return_type: DataType::TimestampTz,
            min_args: 0,
            max_args: Some(0),
        }),
        "pg_export_snapshot" => Some(FunctionInfo {
            func: ScalarFunction::Generic("pg_export_snapshot".into()),
            return_type: DataType::Text,
            min_args: 0,
            max_args: Some(0),
        }),
        "pg_replication_origin_progress" => Some(FunctionInfo {
            func: ScalarFunction::Generic("pg_replication_origin_progress".into()),
            return_type: DataType::Text,
            min_args: 2,
            max_args: Some(2),
        }),
        "pg_is_in_recovery" | "pg_is_wal_replay_paused" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.into()),
            return_type: DataType::Boolean,
            min_args: 0,
            max_args: Some(0),
        }),
        // ANY/ALL array comparison operators
        "any" | "all" | "some" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.into()),
            return_type: DataType::Boolean,
            min_args: 1,
            max_args: Some(1),
        }),
        // Geometric type constructors
        "box" | "circle" | "line" | "lseg" | "path" | "point" | "polygon" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.into()),
            return_type: DataType::Text,
            min_args: 0,
            max_args: None,
        }),
        // Geometric operators/functions
        "area" | "center" | "diameter" | "height" | "isclosed" | "isopen" | "npoints"
        | "pclose" | "popen" | "radius" | "slope" | "width" | "diagonal" | "ishorizontal"
        | "isvertical" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.into()),
            return_type: DataType::Double,
            min_args: 1,
            max_args: Some(2),
        }),
        // Network type functions
        "inet_merge" | "inet_same_family" | "broadcast" | "host" | "hostmask" | "masklen"
        | "netmask" | "network" | "set_masklen" | "text" | "abbrev" | "family" => {
            Some(FunctionInfo {
                func: ScalarFunction::Generic(name.into()),
                return_type: DataType::Text,
                min_args: 1,
                max_args: Some(2),
            })
        }
        "macaddr8_set7bit" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.into()),
            return_type: DataType::MacAddr8,
            min_args: 1,
            max_args: Some(2),
        }),
        // System catalog size functions
        "pg_database_size" | "pg_tablespace_size" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.into()),
            return_type: DataType::BigInt,
            min_args: 1,
            max_args: Some(1),
        }),
        // Notification functions
        "pg_notify" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.into()),
            return_type: DataType::Text,
            min_args: 0,
            max_args: Some(2),
        }),
        "pg_listening_channels" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.into()),
            return_type: DataType::Array(Box::new(DataType::Text)),
            min_args: 0,
            max_args: Some(0),
        }),
        // Stats functions
        "pg_stat_get_snapshot_timestamp" | "pg_stat_clear_snapshot" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.into()),
            return_type: DataType::Timestamp,
            min_args: 0,
            max_args: Some(0),
        }),
        // Information functions
        "pg_function_is_visible"
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
        | "pg_catalog.pg_statistics_obj_is_visible" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.into()),
            return_type: DataType::Boolean,
            min_args: 1,
            max_args: Some(1),
        }),
        "pg_get_triggerdef"
        | "pg_get_ruledef"
        | "pg_get_partkeydef"
        | "pg_get_statisticsobjdef"
        | "pg_get_statisticsobjdef_columns"
        | "pg_get_partition_constraintdef" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.into()),
            return_type: DataType::Text,
            min_args: 1,
            max_args: Some(2),
        }),
        "pg_index_column_has_property" | "pg_index_has_property" | "pg_indexam_has_property" => {
            Some(FunctionInfo {
                func: ScalarFunction::Generic(name.into()),
                return_type: DataType::Boolean,
                min_args: 2,
                max_args: Some(3),
            })
        }
        // Type I/O functions
        "textin" | "textout" | "int4in" | "int4out" | "int8in" | "int8out" | "float4in"
        | "float4out" | "float8in" | "float8out" | "boolin" | "boolout" | "oidin" | "oidout"
        | "tsvectorin" | "tsvectorout" | "tsqueryin" | "tsqueryout" | "jsonbin" | "jsonbout"
        | "jsonin" | "jsonout" | "byteain" | "byteaout" | "xmlin" | "xmlout" | "uuidin"
        | "uuidout" | "datein" | "dateout" | "timestampin" | "timestampout" | "intervalin"
        | "intervalout" | "timein" | "timeout" | "timetypemod" | "timestamptypemod"
        | "numerictypemod" | "varchartypemod" | "bpchartypemod" | "bittypemod"
        | "varbittypemod" | "intervaltypemod" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.into()),
            return_type: DataType::Text,
            min_args: 1,
            max_args: Some(1),
        }),
        // GROUPING function (SQL standard)
        "grouping" => Some(FunctionInfo {
            func: ScalarFunction::Generic("grouping".into()),
            return_type: DataType::Int,
            min_args: 1,
            max_args: None,
        }),
        // Text-search tsvector constructors
        "json_to_tsvector" | "jsonb_to_tsvector" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.into()),
            return_type: DataType::Text,
            min_args: 2,
            max_args: Some(3),
        }),
        // BRIN index maintenance
        "brin_summarize_range" | "brin_desummarize_range" | "brin_summarize_new_values" => {
            Some(FunctionInfo {
                func: ScalarFunction::Generic(name.into()),
                return_type: DataType::Int,
                min_args: 2,
                max_args: Some(3),
            })
        }
        // Partition functions
        "satisfies_hash_partition" => Some(FunctionInfo {
            func: ScalarFunction::Generic("satisfies_hash_partition".into()),
            return_type: DataType::Boolean,
            min_args: 3,
            max_args: None,
        }),
        "pg_partition_root" | "pg_partition_ancestors" | "pg_partition_tree" => {
            Some(FunctionInfo {
                func: ScalarFunction::Generic(name.into()),
                return_type: DataType::Text,
                min_args: 1,
                max_args: Some(1),
            })
        }
        // ── Test-helper identity/constructor functions ──
        // These are normally created as PL/pgSQL functions in pg_regress tests,
        // but since AionDB does not support PL/pgSQL, they are provided as
        // built-in functions with identical semantics.
        "vol" => Some(FunctionInfo {
            func: ScalarFunction::Generic("vol".into()),
            return_type: DataType::Text,
            min_args: 1,
            max_args: Some(1),
        }),
        "volfoo" => Some(FunctionInfo {
            func: ScalarFunction::Generic("volfoo".into()),
            return_type: DataType::Text,
            min_args: 1,
            max_args: Some(1),
        }),
        "make_ad" => Some(FunctionInfo {
            func: ScalarFunction::Generic("make_ad".into()),
            return_type: DataType::Array(Box::new(DataType::Int)),
            min_args: 2,
            max_args: Some(2),
        }),
        _ => None,
    }
}
