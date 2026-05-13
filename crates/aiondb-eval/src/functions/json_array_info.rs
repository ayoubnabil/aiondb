use super::*;

pub(super) fn lookup(name: &str) -> Option<FunctionInfo> {
    match name {
        // ── Implemented array functions ──
        "array_length" => Some(FunctionInfo {
            func: ScalarFunction::ArrayLength,
            return_type: DataType::Int,
            min_args: 1,
            max_args: Some(2),
        }),
        "array_upper" => Some(FunctionInfo {
            func: ScalarFunction::ArrayUpper,
            return_type: DataType::Int,
            min_args: 2,
            max_args: Some(2),
        }),
        "array_lower" => Some(FunctionInfo {
            func: ScalarFunction::ArrayLower,
            return_type: DataType::Int,
            min_args: 2,
            max_args: Some(2),
        }),
        "array_position" => Some(FunctionInfo {
            func: ScalarFunction::ArrayPosition,
            return_type: DataType::Int,
            min_args: 2,
            max_args: Some(3),
        }),
        "array_remove" => Some(FunctionInfo {
            func: ScalarFunction::ArrayRemove,
            return_type: DataType::Array(Box::new(DataType::Text)),
            min_args: 2,
            max_args: Some(2),
        }),
        "array_cat" => Some(FunctionInfo {
            func: ScalarFunction::ArrayCat,
            return_type: DataType::Array(Box::new(DataType::Text)),
            min_args: 2,
            max_args: Some(2),
        }),
        "array_append" => Some(FunctionInfo {
            func: ScalarFunction::ArrayAppend,
            return_type: DataType::Array(Box::new(DataType::Text)),
            min_args: 2,
            max_args: Some(2),
        }),
        "array_prepend" => Some(FunctionInfo {
            func: ScalarFunction::ArrayPrepend,
            return_type: DataType::Array(Box::new(DataType::Text)),
            min_args: 2,
            max_args: Some(2),
        }),
        "array_to_string" => Some(FunctionInfo {
            func: ScalarFunction::ArrayToString,
            return_type: DataType::Text,
            min_args: 2,
            max_args: Some(3),
        }),
        "cardinality" => Some(FunctionInfo {
            func: ScalarFunction::Cardinality,
            return_type: DataType::Int,
            min_args: 1,
            max_args: Some(1),
        }),
        "array_get" => Some(FunctionInfo {
            func: ScalarFunction::ArrayGet,
            return_type: DataType::Text,
            min_args: 2,
            max_args: Some(2),
        }),
        "__aiondb_array_assign" => Some(FunctionInfo {
            func: ScalarFunction::ArrayAssign,
            return_type: DataType::Array(Box::new(DataType::Text)),
            min_args: 5,
            max_args: None,
        }),
        "__aiondb_array_slice" => Some(FunctionInfo {
            func: ScalarFunction::ArraySlice,
            return_type: DataType::Array(Box::new(DataType::Text)),
            min_args: 5,
            max_args: None,
        }),
        // ── Implemented JSONB functions ──
        "jsonb_typeof" => Some(FunctionInfo {
            func: ScalarFunction::JsonbTypeof,
            return_type: DataType::Text,
            min_args: 1,
            max_args: Some(1),
        }),
        "jsonb_array_length" => Some(FunctionInfo {
            func: ScalarFunction::JsonbArrayLength,
            return_type: DataType::Int,
            min_args: 1,
            max_args: Some(1),
        }),
        "jsonb_build_object" => Some(FunctionInfo {
            func: ScalarFunction::JsonbBuildObject,
            return_type: DataType::Jsonb,
            min_args: 0,
            max_args: None, // variadic
        }),
        "jsonb_build_array" => Some(FunctionInfo {
            func: ScalarFunction::JsonbBuildArray,
            return_type: DataType::Jsonb,
            min_args: 0,
            max_args: None, // variadic
        }),
        "jsonb_strip_nulls" => Some(FunctionInfo {
            func: ScalarFunction::JsonbStripNulls,
            return_type: DataType::Jsonb,
            min_args: 1,
            max_args: Some(1),
        }),
        "jsonb_set" => Some(FunctionInfo {
            func: ScalarFunction::JsonbSet,
            return_type: DataType::Jsonb,
            min_args: 3,
            max_args: Some(4),
        }),
        "jsonb_extract_path" => Some(FunctionInfo {
            func: ScalarFunction::JsonbExtractPath,
            return_type: DataType::Jsonb,
            min_args: 1,
            max_args: None, // variadic
        }),
        "jsonb_extract_path_text" => Some(FunctionInfo {
            func: ScalarFunction::JsonbExtractPathText,
            return_type: DataType::Text,
            min_args: 1,
            max_args: None, // variadic
        }),
        "jsonb_object_keys" => Some(FunctionInfo {
            func: ScalarFunction::JsonbObjectKeys,
            return_type: DataType::Array(Box::new(DataType::Text)),
            min_args: 1,
            max_args: Some(1),
        }),
        "jsonb_pretty" => Some(FunctionInfo {
            func: ScalarFunction::JsonbPretty,
            return_type: DataType::Text,
            min_args: 1,
            max_args: Some(1),
        }),
        // ── JSONB path query functions ──
        // jsonb_path_query is a set-returning function, handled specially;
        // keep it as Generic so the SRF path can intercept it.
        "jsonb_path_query" => Some(FunctionInfo {
            func: ScalarFunction::Generic("jsonb_path_query".into()),
            return_type: DataType::Jsonb,
            min_args: 2,
            max_args: Some(4),
        }),
        "jsonb_path_query_array" => Some(FunctionInfo {
            func: ScalarFunction::JsonbPathQueryArray,
            return_type: DataType::Jsonb,
            min_args: 2,
            max_args: Some(4),
        }),
        "jsonb_path_query_first" => Some(FunctionInfo {
            func: ScalarFunction::JsonbPathQueryFirst,
            return_type: DataType::Jsonb,
            min_args: 2,
            max_args: Some(4),
        }),
        "jsonb_path_exists" => Some(FunctionInfo {
            func: ScalarFunction::JsonbPathExists,
            return_type: DataType::Boolean,
            min_args: 2,
            max_args: Some(4),
        }),
        "jsonb_path_match" => Some(FunctionInfo {
            func: ScalarFunction::JsonbPathMatch,
            return_type: DataType::Boolean,
            min_args: 2,
            max_args: Some(4),
        }),
        "jsonb_path_query_tz"
        | "jsonb_path_query_array_tz"
        | "jsonb_path_query_first_tz"
        | "jsonb_path_exists_tz"
        | "jsonb_path_match_tz" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.into()),
            return_type: DataType::Jsonb,
            min_args: 2,
            max_args: Some(4),
        }),
        // ── SQL/JSON constructor functions ──
        "json_object" | "json_array" | "json_scalar" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.into()),
            return_type: DataType::Jsonb,
            min_args: 0,
            max_args: None,
        }),
        "__aiondb_is_json" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.into()),
            return_type: DataType::Boolean,
            min_args: 3,
            max_args: Some(3),
        }),
        "__aiondb_json_array_subquery" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.into()),
            return_type: DataType::Jsonb,
            min_args: 1,
            max_args: Some(1),
        }),
        // Reserved SQL/JSON query-table helpers. They remain recognized so
        // the planner can emit a clear "not implemented" error.
        "json_exists" | "json_value" | "json_query" | "json_table" | "json_serialize" => {
            Some(FunctionInfo {
                func: ScalarFunction::Generic(name.into()),
                return_type: DataType::Text,
                min_args: 1,
                max_args: None,
            })
        }
        // ── Array functions ──
        "array_ndims" => Some(FunctionInfo {
            func: ScalarFunction::ArrayNdims,
            return_type: DataType::Int,
            min_args: 1,
            max_args: Some(1),
        }),
        "array_dims" => Some(FunctionInfo {
            func: ScalarFunction::ArrayDims,
            return_type: DataType::Text,
            min_args: 1,
            max_args: Some(1),
        }),
        "string_to_array" => Some(FunctionInfo {
            func: ScalarFunction::StringToArray,
            return_type: DataType::Array(Box::new(DataType::Text)),
            min_args: 2,
            max_args: Some(3),
        }),
        "array_positions" => Some(FunctionInfo {
            func: ScalarFunction::ArrayPositions,
            return_type: DataType::Array(Box::new(DataType::Int)),
            min_args: 2,
            max_args: Some(2),
        }),
        "array_replace" => Some(FunctionInfo {
            func: ScalarFunction::ArrayReplace,
            return_type: DataType::Array(Box::new(DataType::Text)),
            min_args: 3,
            max_args: Some(3),
        }),
        "array_agg" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.into()),
            return_type: DataType::Array(Box::new(DataType::Text)),
            min_args: 1,
            max_args: Some(1),
        }),
        "array_fill" => Some(FunctionInfo {
            func: ScalarFunction::ArrayFill,
            return_type: DataType::Array(Box::new(DataType::Text)),
            min_args: 2,
            max_args: Some(3),
        }),
        "array_sample" => Some(FunctionInfo {
            func: ScalarFunction::ArraySample,
            return_type: DataType::Array(Box::new(DataType::Text)),
            min_args: 2,
            max_args: Some(2),
        }),
        "array_shuffle" => Some(FunctionInfo {
            func: ScalarFunction::ArrayShuffle,
            return_type: DataType::Array(Box::new(DataType::Text)),
            min_args: 1,
            max_args: Some(1),
        }),
        "trim_array" => Some(FunctionInfo {
            func: ScalarFunction::TrimArray,
            return_type: DataType::Array(Box::new(DataType::Text)),
            min_args: 2,
            max_args: Some(2),
        }),
        // ── JSON construction/extraction helpers ──
        "row_to_json" | "to_json" | "to_jsonb" | "json_strip_nulls" | "json_typeof" => {
            Some(FunctionInfo {
                func: ScalarFunction::Generic(name.into()),
                return_type: DataType::Jsonb,
                min_args: 1,
                max_args: Some(2),
            })
        }
        "json_build_object" => Some(FunctionInfo {
            func: ScalarFunction::Generic("json_build_object".into()),
            return_type: DataType::Jsonb,
            min_args: 0,
            max_args: None,
        }),
        "json_build_array" => Some(FunctionInfo {
            func: ScalarFunction::Generic("json_build_array".into()),
            return_type: DataType::Jsonb,
            min_args: 0,
            max_args: None,
        }),
        "json_extract_path" | "json_extract_path_text" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.into()),
            return_type: DataType::Text,
            min_args: 2,
            max_args: None,
        }),
        "json_array_length" => Some(FunctionInfo {
            func: ScalarFunction::Generic("json_array_length".into()),
            return_type: DataType::Int,
            min_args: 1,
            max_args: Some(1),
        }),
        "jsonb_each"
        | "jsonb_each_text"
        | "json_each"
        | "json_each_text"
        | "json_object_keys"
        | "json_array_elements"
        | "jsonb_array_elements_text"
        | "json_array_elements_text" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.into()),
            return_type: DataType::Text,
            min_args: 1,
            max_args: Some(1),
        }),
        "__aiondb_jsonb_each_keys" | "__aiondb_jsonb_each_text_values" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.into()),
            return_type: DataType::Text,
            min_args: 1,
            max_args: Some(1),
        }),
        "__aiondb_jsonb_each_values" | "jsonb_array_elements" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.into()),
            return_type: DataType::Jsonb,
            min_args: 1,
            max_args: Some(1),
        }),
        "jsonb_populate_record"
        | "json_populate_record"
        | "jsonb_to_record"
        | "json_to_record"
        | "jsonb_populate_recordset"
        | "json_populate_recordset"
        | "jsonb_to_recordset"
        | "json_to_recordset" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.into()),
            return_type: DataType::Text,
            min_args: 1,
            max_args: Some(2),
        }),
        "__aiondb_jsonb_to_record"
        | "__aiondb_json_to_record"
        | "__aiondb_jsonb_populate_record"
        | "__aiondb_json_populate_record"
        | "__aiondb_jsonb_to_recordset"
        | "__aiondb_json_to_recordset"
        | "__aiondb_jsonb_populate_recordset"
        | "__aiondb_json_populate_recordset" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.into()),
            return_type: DataType::Array(Box::new(DataType::Text)),
            min_args: 3,
            max_args: None,
        }),
        "jsonb_insert" => Some(FunctionInfo {
            func: ScalarFunction::Generic("jsonb_insert".into()),
            return_type: DataType::Jsonb,
            min_args: 3,
            max_args: Some(4),
        }),
        "jsonb_set_lax" => Some(FunctionInfo {
            func: ScalarFunction::Generic("jsonb_set_lax".into()),
            return_type: DataType::Jsonb,
            min_args: 3,
            max_args: Some(5),
        }),
        "jsonb_object" | "json_agg" | "jsonb_agg" | "json_object_agg" | "jsonb_object_agg" => {
            Some(FunctionInfo {
                func: ScalarFunction::Generic(name.into()),
                return_type: DataType::Jsonb,
                min_args: 0,
                max_args: None,
            })
        }
        "string_agg" => Some(FunctionInfo {
            func: ScalarFunction::Generic("string_agg".into()),
            return_type: DataType::Text,
            min_args: 2,
            max_args: Some(2),
        }),
        "bool_and" | "bool_or" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.into()),
            return_type: DataType::Boolean,
            min_args: 1,
            max_args: Some(1),
        }),
        "array_to_json" => Some(FunctionInfo {
            func: ScalarFunction::Generic("array_to_json".into()),
            return_type: DataType::Jsonb,
            min_args: 1,
            max_args: Some(2),
        }),
        // JSONB operator functions
        "jsonb_concat" => Some(FunctionInfo {
            func: ScalarFunction::Generic("jsonb_concat".into()),
            return_type: DataType::Jsonb,
            min_args: 2,
            max_args: Some(2),
        }),
        "jsonb_contained" | "jsonb_contains" | "jsonb_exists" | "jsonb_exists_all"
        | "jsonb_exists_any" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.into()),
            return_type: DataType::Boolean,
            min_args: 2,
            max_args: Some(2),
        }),
        "jsonb_delete" | "jsonb_delete_path" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.into()),
            return_type: DataType::Jsonb,
            min_args: 2,
            max_args: Some(2),
        }),
        "jsonb_hash" | "jsonb_hash_extended" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.into()),
            return_type: DataType::Int,
            min_args: 1,
            max_args: Some(2),
        }),
        // JSON SQL/2016 standard aggregate functions
        "json_objectagg" | "json_arrayagg" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.into()),
            return_type: DataType::Jsonb,
            min_args: 0,
            max_args: None,
        }),
        // Array tsvector
        "array_to_tsvector" => Some(FunctionInfo {
            func: ScalarFunction::Generic("array_to_tsvector".into()),
            return_type: DataType::Text,
            min_args: 1,
            max_args: Some(1),
        }),
        _ => None,
    }
}
