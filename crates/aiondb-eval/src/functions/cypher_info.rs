use super::*;

pub(super) fn lookup(name: &str) -> Option<FunctionInfo> {
    match name {
        "toboolean" => Some(FunctionInfo {
            func: ScalarFunction::Generic("cypher_toboolean".into()),
            return_type: DataType::Boolean,
            min_args: 1,
            max_args: Some(1),
        }),
        "tointeger" => Some(FunctionInfo {
            func: ScalarFunction::Generic("cypher_tointeger".into()),
            return_type: DataType::BigInt,
            min_args: 1,
            max_args: Some(1),
        }),
        "tofloat" => Some(FunctionInfo {
            func: ScalarFunction::Generic("cypher_tofloat".into()),
            return_type: DataType::Double,
            min_args: 1,
            max_args: Some(1),
        }),
        "tostring" => Some(FunctionInfo {
            func: ScalarFunction::Generic("cypher_tostring".into()),
            return_type: DataType::Text,
            min_args: 1,
            max_args: Some(1),
        }),
        "tobooleanornull" => Some(FunctionInfo {
            func: ScalarFunction::Generic("cypher_tobooleanornull".into()),
            return_type: DataType::Boolean,
            min_args: 1,
            max_args: Some(1),
        }),
        "tointegerornull" => Some(FunctionInfo {
            func: ScalarFunction::Generic("cypher_tointegerornull".into()),
            return_type: DataType::BigInt,
            min_args: 1,
            max_args: Some(1),
        }),
        "tofloatornull" => Some(FunctionInfo {
            func: ScalarFunction::Generic("cypher_tofloatornull".into()),
            return_type: DataType::Double,
            min_args: 1,
            max_args: Some(1),
        }),
        "tostringornull" => Some(FunctionInfo {
            func: ScalarFunction::Generic("cypher_tostringornull".into()),
            return_type: DataType::Text,
            min_args: 1,
            max_args: Some(1),
        }),
        // Cypher math aliases
        "rand" => Some(FunctionInfo {
            func: ScalarFunction::Random,
            return_type: DataType::Double,
            min_args: 0,
            max_args: Some(0),
        }),
        "e" => Some(FunctionInfo {
            func: ScalarFunction::Generic("e".into()),
            return_type: DataType::Double,
            min_args: 0,
            max_args: Some(0),
        }),
        // Cypher keys() function - returns list of property keys from a map/node/relationship
        "keys" => Some(FunctionInfo {
            func: ScalarFunction::JsonbObjectKeys,
            return_type: DataType::Array(Box::new(DataType::Text)),
            min_args: 1,
            max_args: Some(1),
        }),
        // Cypher temporal constructor functions. The translator emits
        // `cypher_date(...)` (etc.) directly so that lookup never collides
        // with the PG `date(text)` cast in pg_internal_info.
        "date" | "cypher_date" => Some(FunctionInfo {
            func: ScalarFunction::Generic("cypher_date".into()),
            return_type: DataType::Date,
            min_args: 0,
            max_args: Some(1),
        }),
        "time" | "cypher_time" => Some(FunctionInfo {
            func: ScalarFunction::Generic("cypher_time".into()),
            return_type: DataType::TimeTz,
            min_args: 0,
            max_args: Some(1),
        }),
        "datetime" | "cypher_datetime" => Some(FunctionInfo {
            func: ScalarFunction::Generic("cypher_datetime".into()),
            return_type: DataType::TimestampTz,
            min_args: 0,
            max_args: Some(1),
        }),
        "localtime" | "cypher_localtime" => Some(FunctionInfo {
            func: ScalarFunction::Generic("cypher_localtime".into()),
            return_type: DataType::Time,
            min_args: 0,
            max_args: Some(1),
        }),
        "localdatetime" | "cypher_localdatetime" => Some(FunctionInfo {
            func: ScalarFunction::Generic("cypher_localdatetime".into()),
            return_type: DataType::Timestamp,
            min_args: 0,
            max_args: Some(1),
        }),
        "duration" | "cypher_duration" => Some(FunctionInfo {
            func: ScalarFunction::Generic("cypher_duration".into()),
            return_type: DataType::Interval,
            min_args: 1,
            max_args: Some(1),
        }),
        // Cypher range() function - generates a list of integers
        "range" => Some(FunctionInfo {
            func: ScalarFunction::Generic("range".into()),
            return_type: DataType::Array(Box::new(DataType::BigInt)),
            min_args: 2,
            max_args: Some(3),
        }),
        // Cypher dotted temporal functions: truncate and duration.between/inMonths/inDays/inSeconds
        "date.truncate" => Some(FunctionInfo {
            func: ScalarFunction::Generic("date.truncate".into()),
            return_type: DataType::Date,
            min_args: 2,
            max_args: Some(3),
        }),
        "datetime.truncate" => Some(FunctionInfo {
            func: ScalarFunction::Generic("datetime.truncate".into()),
            return_type: DataType::TimestampTz,
            min_args: 2,
            max_args: Some(3),
        }),
        "localdatetime.truncate" => Some(FunctionInfo {
            func: ScalarFunction::Generic("localdatetime.truncate".into()),
            return_type: DataType::Timestamp,
            min_args: 2,
            max_args: Some(3),
        }),
        "time.truncate" => Some(FunctionInfo {
            func: ScalarFunction::Generic("time.truncate".into()),
            return_type: DataType::TimeTz,
            min_args: 2,
            max_args: Some(3),
        }),
        "localtime.truncate" => Some(FunctionInfo {
            func: ScalarFunction::Generic("localtime.truncate".into()),
            return_type: DataType::Time,
            min_args: 2,
            max_args: Some(3),
        }),
        "duration.between" => Some(FunctionInfo {
            func: ScalarFunction::Generic("duration.between".into()),
            return_type: DataType::Interval,
            min_args: 2,
            max_args: Some(2),
        }),
        "duration.inmonths" => Some(FunctionInfo {
            func: ScalarFunction::Generic("duration.inmonths".into()),
            return_type: DataType::Interval,
            min_args: 2,
            max_args: Some(2),
        }),
        "duration.indays" => Some(FunctionInfo {
            func: ScalarFunction::Generic("duration.indays".into()),
            return_type: DataType::Interval,
            min_args: 2,
            max_args: Some(2),
        }),
        "duration.inseconds" => Some(FunctionInfo {
            func: ScalarFunction::Generic("duration.inseconds".into()),
            return_type: DataType::Interval,
            min_args: 2,
            max_args: Some(2),
        }),
        // ── Cypher list/string size ──
        // Cypher's `size()` returns the length of a list or string.
        // Routed to a generic dispatcher that picks array_length vs char_length.
        "size" => Some(FunctionInfo {
            func: ScalarFunction::Generic("cypher_size".into()),
            return_type: DataType::BigInt,
            min_args: 1,
            max_args: Some(1),
        }),
        // ── Cypher head/tail/last on lists ──
        "head" => Some(FunctionInfo {
            func: ScalarFunction::Generic("cypher_head".into()),
            return_type: DataType::Text,
            min_args: 1,
            max_args: Some(1),
        }),
        "last" => Some(FunctionInfo {
            func: ScalarFunction::Generic("cypher_last".into()),
            return_type: DataType::Text,
            min_args: 1,
            max_args: Some(1),
        }),
        "tail" => Some(FunctionInfo {
            func: ScalarFunction::Generic("cypher_tail".into()),
            return_type: DataType::Array(Box::new(DataType::Text)),
            min_args: 1,
            max_args: Some(1),
        }),
        // ── Graph element introspection functions ──
        "labels" => Some(FunctionInfo {
            func: ScalarFunction::Generic("graph_labels".into()),
            return_type: DataType::Array(Box::new(DataType::Text)),
            min_args: 1,
            max_args: Some(1),
        }),
        "type" => Some(FunctionInfo {
            func: ScalarFunction::Generic("graph_type".into()),
            return_type: DataType::Text,
            min_args: 1,
            max_args: Some(1),
        }),
        "id" => Some(FunctionInfo {
            func: ScalarFunction::Generic("graph_id".into()),
            return_type: DataType::BigInt,
            min_args: 1,
            max_args: Some(1),
        }),
        "properties" => Some(FunctionInfo {
            func: ScalarFunction::Generic("graph_properties".into()),
            return_type: DataType::Jsonb,
            min_args: 1,
            max_args: Some(1),
        }),
        "startnode" => Some(FunctionInfo {
            func: ScalarFunction::Generic("graph_start_node".into()),
            return_type: DataType::BigInt,
            min_args: 1,
            max_args: Some(1),
        }),
        "endnode" => Some(FunctionInfo {
            func: ScalarFunction::Generic("graph_end_node".into()),
            return_type: DataType::BigInt,
            min_args: 1,
            max_args: Some(1),
        }),
        "nodes" => Some(FunctionInfo {
            func: ScalarFunction::Generic("graph_nodes".into()),
            return_type: DataType::Array(Box::new(DataType::Jsonb)),
            min_args: 1,
            max_args: Some(1),
        }),
        "relationships" => Some(FunctionInfo {
            func: ScalarFunction::Generic("graph_relationships".into()),
            return_type: DataType::Array(Box::new(DataType::Jsonb)),
            min_args: 1,
            max_args: Some(1),
        }),
        // ── Hybrid SQL + graph + vector primitives ──
        "graph_neighbors" => Some(FunctionInfo {
            func: ScalarFunction::Generic("graph_neighbors".into()),
            return_type: DataType::BigInt,
            min_args: 2,
            max_args: Some(4),
        }),
        "vector_top_k_ids" => Some(FunctionInfo {
            func: ScalarFunction::Generic("vector_top_k_ids".into()),
            return_type: DataType::BigInt,
            min_args: 4,
            max_args: Some(10),
        }),
        "vector_top_k_hits" => Some(FunctionInfo {
            func: ScalarFunction::Generic("vector_top_k_hits".into()),
            return_type: DataType::Jsonb,
            min_args: 4,
            max_args: Some(10),
        }),
        "vector_prefetch_top_k_hits" => Some(FunctionInfo {
            func: ScalarFunction::Generic("vector_prefetch_top_k_hits".into()),
            return_type: DataType::Jsonb,
            min_args: 5,
            max_args: Some(9),
        }),
        "vector_recommend_top_k_hits" => Some(FunctionInfo {
            func: ScalarFunction::Generic("vector_recommend_top_k_hits".into()),
            return_type: DataType::Jsonb,
            min_args: 5,
            max_args: Some(11),
        }),
        "full_text_top_k_hits" => Some(FunctionInfo {
            func: ScalarFunction::Generic("full_text_top_k_hits".into()),
            return_type: DataType::Jsonb,
            min_args: 4,
            max_args: Some(8),
        }),
        "hybrid_search_top_k_hits" => Some(FunctionInfo {
            func: ScalarFunction::Generic("hybrid_search_top_k_hits".into()),
            return_type: DataType::Jsonb,
            min_args: 6,
            max_args: Some(7),
        }),
        "hybrid_fuse_rrf_hits" => Some(FunctionInfo {
            func: ScalarFunction::Generic("hybrid_fuse_rrf_hits".into()),
            return_type: DataType::Jsonb,
            min_args: 3,
            max_args: Some(6),
        }),
        "hybrid_fuse_dbsf_hits" => Some(FunctionInfo {
            func: ScalarFunction::Generic("hybrid_fuse_dbsf_hits".into()),
            return_type: DataType::Jsonb,
            min_args: 3,
            max_args: Some(5),
        }),
        "hybrid_group_hits_by" => Some(FunctionInfo {
            func: ScalarFunction::Generic("hybrid_group_hits_by".into()),
            return_type: DataType::Jsonb,
            min_args: 3,
            max_args: Some(4),
        }),
        _ => None,
    }
}
