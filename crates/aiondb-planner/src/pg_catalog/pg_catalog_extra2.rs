use aiondb_core::{
    DataType, DbResult, Value, COMPAT_BOOTSTRAP_ROLE_OID, COMPAT_PG_DEFAULT_TABLESPACE_OID,
    COMPAT_PG_GLOBAL_TABLESPACE_OID,
};
use aiondb_plan::{LogicalPlan, ResultField, TypedExpr};

use super::extra_tables::typed_array_literal;
use super::matview::parse_matview_sidecar;
use super::*;

// ---------------------------------------------------------------
// pg_catalog.pg_cast
// ---------------------------------------------------------------

pub(super) fn pg_cast_fields() -> Vec<ResultField> {
    vec![
        oid_field("oid"),
        oid_field("castsource"),
        oid_field("casttarget"),
        oid_field("castfunc"),
        internal_char_field("castcontext"), // "e" = explicit, "a" = assignment, "i" = implicit
        internal_char_field("castmethod"),  // "f" = function, "b" = binary-coercible, "i" = I/O
    ]
}

/// A cast entry.
struct CastEntry {
    oid: i32,
    source: i32,
    target: i32,
    func: i32,
    context: &'static str,
    method: &'static str,
}

struct PgvectorCastSpec {
    source: i32,
    target: i32,
    func_name: &'static str,
    func_argtypes: &'static str,
    context: &'static str,
    method: &'static str,
}

/// Well-known casts matching PostgreSQL's pg_cast for the types AionDB supports.
const PG_CASTS: &[CastEntry] = &[
    // int4 -> int8 (implicit)
    CastEntry {
        oid: 1,
        source: 23,
        target: 20,
        func: 480,
        context: "i",
        method: "f",
    },
    // int8 -> int4 (assignment)
    CastEntry {
        oid: 2,
        source: 20,
        target: 23,
        func: 481,
        context: "a",
        method: "f",
    },
    // int4 -> float8 (implicit)
    CastEntry {
        oid: 3,
        source: 23,
        target: 701,
        func: 316,
        context: "i",
        method: "f",
    },
    // int8 -> float8 (implicit)
    CastEntry {
        oid: 4,
        source: 20,
        target: 701,
        func: 318,
        context: "i",
        method: "f",
    },
    // float4 -> float8 (implicit)
    CastEntry {
        oid: 5,
        source: 700,
        target: 701,
        func: 312,
        context: "i",
        method: "f",
    },
    // float8 -> float4 (assignment)
    CastEntry {
        oid: 6,
        source: 701,
        target: 700,
        func: 313,
        context: "a",
        method: "f",
    },
    // float8 -> int4 (assignment)
    CastEntry {
        oid: 7,
        source: 701,
        target: 23,
        func: 319,
        context: "a",
        method: "f",
    },
    // float8 -> int8 (assignment)
    CastEntry {
        oid: 8,
        source: 701,
        target: 20,
        func: 482,
        context: "a",
        method: "f",
    },
    // int4 -> numeric (implicit)
    CastEntry {
        oid: 9,
        source: 23,
        target: 1700,
        func: 0,
        context: "i",
        method: "f",
    },
    // int8 -> numeric (implicit)
    CastEntry {
        oid: 10,
        source: 20,
        target: 1700,
        func: 0,
        context: "i",
        method: "f",
    },
    // float4 -> numeric (assignment)
    CastEntry {
        oid: 11,
        source: 700,
        target: 1700,
        func: 0,
        context: "a",
        method: "f",
    },
    // float8 -> numeric (assignment)
    CastEntry {
        oid: 12,
        source: 701,
        target: 1700,
        func: 0,
        context: "a",
        method: "f",
    },
    // numeric -> int4 (assignment)
    CastEntry {
        oid: 13,
        source: 1700,
        target: 23,
        func: 0,
        context: "a",
        method: "f",
    },
    // numeric -> int8 (assignment)
    CastEntry {
        oid: 14,
        source: 1700,
        target: 20,
        func: 0,
        context: "a",
        method: "f",
    },
    // numeric -> float4 (implicit)
    CastEntry {
        oid: 15,
        source: 1700,
        target: 700,
        func: 0,
        context: "i",
        method: "f",
    },
    // numeric -> float8 (implicit)
    CastEntry {
        oid: 16,
        source: 1700,
        target: 701,
        func: 0,
        context: "i",
        method: "f",
    },
    // int4 -> text (assignment via I/O)
    CastEntry {
        oid: 17,
        source: 23,
        target: 25,
        func: 0,
        context: "a",
        method: "i",
    },
    // text -> int4 (explicit via I/O)
    CastEntry {
        oid: 18,
        source: 25,
        target: 23,
        func: 0,
        context: "e",
        method: "i",
    },
    // int4 -> bool (explicit)
    CastEntry {
        oid: 19,
        source: 23,
        target: 16,
        func: 0,
        context: "e",
        method: "f",
    },
    // date -> timestamp (implicit)
    CastEntry {
        oid: 20,
        source: 1082,
        target: 1114,
        func: 0,
        context: "i",
        method: "f",
    },
    // date -> timestamptz (implicit)
    CastEntry {
        oid: 21,
        source: 1082,
        target: 1184,
        func: 0,
        context: "i",
        method: "f",
    },
    // timestamp -> timestamptz (implicit)
    CastEntry {
        oid: 22,
        source: 1114,
        target: 1184,
        func: 0,
        context: "i",
        method: "f",
    },
    // timestamptz -> timestamp (assignment)
    CastEntry {
        oid: 23,
        source: 1184,
        target: 1114,
        func: 0,
        context: "a",
        method: "f",
    },
    // timestamp -> date (assignment)
    CastEntry {
        oid: 24,
        source: 1114,
        target: 1082,
        func: 0,
        context: "a",
        method: "f",
    },
    // timestamptz -> date (assignment)
    CastEntry {
        oid: 25,
        source: 1184,
        target: 1082,
        func: 0,
        context: "a",
        method: "f",
    },
    // int4 -> float4 (implicit)
    CastEntry {
        oid: 26,
        source: 23,
        target: 700,
        func: 0,
        context: "i",
        method: "f",
    },
];

const PGVECTOR_CASTS: &[PgvectorCastSpec] = &[
    PgvectorCastSpec {
        source: 1007,
        target: COMPAT_PGVECTOR_VECTOR_OID,
        func_name: "array_to_vector",
        func_argtypes: "1007 23 16",
        context: "a",
        method: "f",
    },
    PgvectorCastSpec {
        source: 1021,
        target: COMPAT_PGVECTOR_VECTOR_OID,
        func_name: "array_to_vector",
        func_argtypes: "1021 23 16",
        context: "a",
        method: "f",
    },
    PgvectorCastSpec {
        source: 1022,
        target: COMPAT_PGVECTOR_VECTOR_OID,
        func_name: "array_to_vector",
        func_argtypes: "1022 23 16",
        context: "a",
        method: "f",
    },
    PgvectorCastSpec {
        source: 1231,
        target: COMPAT_PGVECTOR_VECTOR_OID,
        func_name: "array_to_vector",
        func_argtypes: "1231 23 16",
        context: "a",
        method: "f",
    },
    PgvectorCastSpec {
        source: COMPAT_PGVECTOR_VECTOR_OID,
        target: 1021,
        func_name: "vector_to_float4",
        func_argtypes: "80001 23 16",
        context: "a",
        method: "f",
    },
    PgvectorCastSpec {
        source: COMPAT_PGVECTOR_VECTOR_OID,
        target: COMPAT_PGVECTOR_HALFVEC_OID,
        func_name: "vector_to_halfvec",
        func_argtypes: "80001 23 16",
        context: "i",
        method: "f",
    },
    PgvectorCastSpec {
        source: COMPAT_PGVECTOR_HALFVEC_OID,
        target: COMPAT_PGVECTOR_VECTOR_OID,
        func_name: "halfvec_to_vector",
        func_argtypes: "80003 23 16",
        context: "a",
        method: "f",
    },
    PgvectorCastSpec {
        source: COMPAT_PGVECTOR_VECTOR_OID,
        target: COMPAT_PGVECTOR_SPARSEVEC_OID,
        func_name: "vector_to_sparsevec",
        func_argtypes: "80001 23 16",
        context: "i",
        method: "f",
    },
    PgvectorCastSpec {
        source: COMPAT_PGVECTOR_VECTOR_OID,
        target: COMPAT_PG_BIT_OID,
        func_name: "binary_quantize",
        func_argtypes: "80001 23 16",
        context: "e",
        method: "f",
    },
    PgvectorCastSpec {
        source: 1007,
        target: COMPAT_PGVECTOR_SPARSEVEC_OID,
        func_name: "array_to_sparsevec",
        func_argtypes: "1007 23 16",
        context: "a",
        method: "f",
    },
    PgvectorCastSpec {
        source: 1021,
        target: COMPAT_PGVECTOR_SPARSEVEC_OID,
        func_name: "array_to_sparsevec",
        func_argtypes: "1021 23 16",
        context: "a",
        method: "f",
    },
    PgvectorCastSpec {
        source: 1022,
        target: COMPAT_PGVECTOR_SPARSEVEC_OID,
        func_name: "array_to_sparsevec",
        func_argtypes: "1022 23 16",
        context: "a",
        method: "f",
    },
    PgvectorCastSpec {
        source: 1231,
        target: COMPAT_PGVECTOR_SPARSEVEC_OID,
        func_name: "array_to_sparsevec",
        func_argtypes: "1231 23 16",
        context: "a",
        method: "f",
    },
];

pub(super) fn build_pg_cast_plan() -> DbResult<LogicalPlan> {
    let fields = pg_cast_fields();
    let mut rows: Vec<Vec<TypedExpr>> = PG_CASTS
        .iter()
        .map(|c| {
            vec![
                int_literal(c.oid),
                int_literal(c.source),
                int_literal(c.target),
                int_literal(c.func),
                text_literal(c.context),
                text_literal(c.method),
            ]
        })
        .collect();
    rows.extend(PGVECTOR_CASTS.iter().map(|cast| {
        vec![
            int_literal(aiondb_core::compat_function_oid(&format!(
                "cast:pgvector:{}:{}:{}",
                cast.source, cast.target, cast.func_name
            ))),
            int_literal(cast.source),
            int_literal(cast.target),
            int_literal(compat_pgvector_function_oid(
                cast.func_name,
                cast.func_argtypes,
            )),
            text_literal(cast.context),
            text_literal(cast.method),
        ]
    }));
    rows.extend(aiondb_eval::with_current_session_context(|context| {
        context
            .compat_user_casts
            .iter()
            .filter_map(|cast| {
                let source_oid =
                    if let Some(user_type) = context.compat_user_type(&cast.source_type) {
                        user_type.oid
                    } else {
                        super::virtual_query::resolve_regtype_oid(&cast.source_type)?
                    };
                let target_oid =
                    if let Some(user_type) = context.compat_user_type(&cast.target_type) {
                        user_type.oid
                    } else {
                        super::virtual_query::resolve_regtype_oid(&cast.target_type)?
                    };
                Some(vec![
                    int_literal(cast.oid),
                    int_literal(source_oid),
                    int_literal(target_oid),
                    int_literal(cast.method.function_oid()),
                    text_literal(cast.context.as_pg_code()),
                    text_literal(cast.method.as_pg_code()),
                ])
            })
            .collect::<Vec<_>>()
    }));
    Ok(project_values(fields, rows))
}

// ---------------------------------------------------------------
// pg_catalog.pg_aggregate
// ---------------------------------------------------------------

pub(super) fn pg_aggregate_fields() -> Vec<ResultField> {
    vec![
        oid_field("aggfnoid"),
        internal_char_field("aggkind"),
        int_field("aggnumdirectargs"),
        oid_field("aggtransfn"),
        oid_field("aggfinalfn"),
        oid_field("aggcombinefn"),
        oid_field("aggserialfn"),
        oid_field("aggdeserialfn"),
        oid_field("aggmtransfn"),
        oid_field("aggminvtransfn"),
        oid_field("aggmfinalfn"),
        bool_field("aggfinalextra"),
        bool_field("aggmfinalextra"),
        internal_char_field("aggfinalmodify"),
        internal_char_field("aggmfinalmodify"),
        oid_field("aggsortop"),
        oid_field("aggtranstype"),
        int_field("aggtransspace"),
        oid_field("aggmtranstype"),
        int_field("aggmtransspace"),
        nullable_text_field("agginitval"),
        nullable_text_field("aggminitval"),
    ]
}

pub(super) fn build_pg_aggregate_plan() -> DbResult<LogicalPlan> {
    // Static rows for PG's well-known aggregates so ORM tooling that joins
    // pg_proc against pg_aggregate (psql \da, sqlx, Diesel introspection)
    // sees a non-empty result set. We don't expose internal transition
    // functions; aggtransfn/aggfinalfn are 0 because AionDB does not
    // implement aggregates as separate procs.
    let entries = pg_aggregate_static_entries();
    let fields = pg_aggregate_fields();
    let mut rows: Vec<Vec<TypedExpr>> = entries
        .iter()
        .map(|entry| make_aggregate_row(entry))
        .collect();
    rows.extend(
        [
            AggregateEntry {
                aggfnoid: compat_pgvector_function_oid("sum", "80001"),
                aggkind: "n",
                aggnumdirectargs: 0,
                aggsortop: 0,
                aggtranstype: COMPAT_PGVECTOR_VECTOR_OID,
            },
            AggregateEntry {
                aggfnoid: compat_pgvector_function_oid("avg", "80001"),
                aggkind: "n",
                aggnumdirectargs: 0,
                aggsortop: 0,
                aggtranstype: COMPAT_PGVECTOR_VECTOR_OID,
            },
            AggregateEntry {
                aggfnoid: compat_pgvector_function_oid("sum", "80003"),
                aggkind: "n",
                aggnumdirectargs: 0,
                aggsortop: 0,
                aggtranstype: COMPAT_PGVECTOR_HALFVEC_OID,
            },
            AggregateEntry {
                aggfnoid: compat_pgvector_function_oid("avg", "80003"),
                aggkind: "n",
                aggnumdirectargs: 0,
                aggsortop: 0,
                aggtranstype: COMPAT_PGVECTOR_HALFVEC_OID,
            },
        ]
        .iter()
        .map(make_aggregate_row),
    );
    Ok(project_values(fields, rows))
}

struct AggregateEntry {
    aggfnoid: i32,
    aggkind: &'static str,
    aggnumdirectargs: i32,
    aggsortop: i32,
    aggtranstype: i32,
}

fn pg_aggregate_static_entries() -> &'static [AggregateEntry] {
    // OIDs sourced from upstream PG 16 pg_aggregate.dat for the function
    // OIDs (aggfnoid). aggtranstype is the OID of the transition state type.
    // Unknown values are left at 0.
    &[
        // count(any) → 2147, count(*) → 2803
        AggregateEntry {
            aggfnoid: 2147,
            aggkind: "n",
            aggnumdirectargs: 0,
            aggsortop: 0,
            aggtranstype: 20,
        },
        AggregateEntry {
            aggfnoid: 2803,
            aggkind: "n",
            aggnumdirectargs: 0,
            aggsortop: 0,
            aggtranstype: 20,
        },
        // sum across numeric types
        AggregateEntry {
            aggfnoid: 2107,
            aggkind: "n",
            aggnumdirectargs: 0,
            aggsortop: 0,
            aggtranstype: 1700,
        }, // sum(int8) → numeric
        AggregateEntry {
            aggfnoid: 2108,
            aggkind: "n",
            aggnumdirectargs: 0,
            aggsortop: 0,
            aggtranstype: 20,
        }, // sum(int4) → bigint
        AggregateEntry {
            aggfnoid: 2109,
            aggkind: "n",
            aggnumdirectargs: 0,
            aggsortop: 0,
            aggtranstype: 20,
        }, // sum(int2)
        AggregateEntry {
            aggfnoid: 2110,
            aggkind: "n",
            aggnumdirectargs: 0,
            aggsortop: 0,
            aggtranstype: 700,
        }, // sum(real)
        AggregateEntry {
            aggfnoid: 2111,
            aggkind: "n",
            aggnumdirectargs: 0,
            aggsortop: 0,
            aggtranstype: 701,
        }, // sum(double)
        AggregateEntry {
            aggfnoid: 2114,
            aggkind: "n",
            aggnumdirectargs: 0,
            aggsortop: 0,
            aggtranstype: 1700,
        }, // sum(numeric)
        // avg
        AggregateEntry {
            aggfnoid: 2100,
            aggkind: "n",
            aggnumdirectargs: 0,
            aggsortop: 0,
            aggtranstype: 1700,
        }, // avg(int8)
        AggregateEntry {
            aggfnoid: 2101,
            aggkind: "n",
            aggnumdirectargs: 0,
            aggsortop: 0,
            aggtranstype: 1700,
        }, // avg(int4)
        AggregateEntry {
            aggfnoid: 2102,
            aggkind: "n",
            aggnumdirectargs: 0,
            aggsortop: 0,
            aggtranstype: 1700,
        }, // avg(int2)
        AggregateEntry {
            aggfnoid: 2104,
            aggkind: "n",
            aggnumdirectargs: 0,
            aggsortop: 0,
            aggtranstype: 700,
        }, // avg(real)
        AggregateEntry {
            aggfnoid: 2105,
            aggkind: "n",
            aggnumdirectargs: 0,
            aggsortop: 0,
            aggtranstype: 701,
        }, // avg(double)
        AggregateEntry {
            aggfnoid: 2103,
            aggkind: "n",
            aggnumdirectargs: 0,
            aggsortop: 0,
            aggtranstype: 1700,
        }, // avg(numeric)
        // min
        AggregateEntry {
            aggfnoid: 2131,
            aggkind: "n",
            aggnumdirectargs: 0,
            aggsortop: 95,
            aggtranstype: 21,
        }, // min(int2)
        AggregateEntry {
            aggfnoid: 2132,
            aggkind: "n",
            aggnumdirectargs: 0,
            aggsortop: 97,
            aggtranstype: 23,
        }, // min(int4)
        AggregateEntry {
            aggfnoid: 2133,
            aggkind: "n",
            aggnumdirectargs: 0,
            aggsortop: 412,
            aggtranstype: 20,
        }, // min(int8)
        AggregateEntry {
            aggfnoid: 2134,
            aggkind: "n",
            aggnumdirectargs: 0,
            aggsortop: 622,
            aggtranstype: 700,
        }, // min(real)
        AggregateEntry {
            aggfnoid: 2135,
            aggkind: "n",
            aggnumdirectargs: 0,
            aggsortop: 672,
            aggtranstype: 701,
        }, // min(double)
        AggregateEntry {
            aggfnoid: 2145,
            aggkind: "n",
            aggnumdirectargs: 0,
            aggsortop: 664,
            aggtranstype: 25,
        }, // min(text)
        AggregateEntry {
            aggfnoid: 2146,
            aggkind: "n",
            aggnumdirectargs: 0,
            aggsortop: 1754,
            aggtranstype: 1700,
        }, // min(numeric)
        // max
        AggregateEntry {
            aggfnoid: 2115,
            aggkind: "n",
            aggnumdirectargs: 0,
            aggsortop: 520,
            aggtranstype: 21,
        }, // max(int2)
        AggregateEntry {
            aggfnoid: 2116,
            aggkind: "n",
            aggnumdirectargs: 0,
            aggsortop: 521,
            aggtranstype: 23,
        }, // max(int4)
        AggregateEntry {
            aggfnoid: 2117,
            aggkind: "n",
            aggnumdirectargs: 0,
            aggsortop: 413,
            aggtranstype: 20,
        }, // max(int8)
        AggregateEntry {
            aggfnoid: 2118,
            aggkind: "n",
            aggnumdirectargs: 0,
            aggsortop: 623,
            aggtranstype: 700,
        },
        AggregateEntry {
            aggfnoid: 2119,
            aggkind: "n",
            aggnumdirectargs: 0,
            aggsortop: 674,
            aggtranstype: 701,
        },
        AggregateEntry {
            aggfnoid: 2129,
            aggkind: "n",
            aggnumdirectargs: 0,
            aggsortop: 666,
            aggtranstype: 25,
        }, // max(text)
        AggregateEntry {
            aggfnoid: 2130,
            aggkind: "n",
            aggnumdirectargs: 0,
            aggsortop: 1756,
            aggtranstype: 1700,
        },
        // bool aggregates
        AggregateEntry {
            aggfnoid: 2517,
            aggkind: "n",
            aggnumdirectargs: 0,
            aggsortop: 0,
            aggtranstype: 16,
        }, // bool_and
        AggregateEntry {
            aggfnoid: 2518,
            aggkind: "n",
            aggnumdirectargs: 0,
            aggsortop: 0,
            aggtranstype: 16,
        }, // bool_or
        AggregateEntry {
            aggfnoid: 2519,
            aggkind: "n",
            aggnumdirectargs: 0,
            aggsortop: 0,
            aggtranstype: 16,
        }, // every (alias)
        // array/string/json aggregates
        AggregateEntry {
            aggfnoid: 2335,
            aggkind: "n",
            aggnumdirectargs: 0,
            aggsortop: 0,
            aggtranstype: 2281,
        }, // array_agg(any)
        AggregateEntry {
            aggfnoid: 4053,
            aggkind: "n",
            aggnumdirectargs: 0,
            aggsortop: 0,
            aggtranstype: 2281,
        }, // array_agg(anyarray)
        AggregateEntry {
            aggfnoid: 3538,
            aggkind: "n",
            aggnumdirectargs: 0,
            aggsortop: 0,
            aggtranstype: 25,
        }, // string_agg(text)
        AggregateEntry {
            aggfnoid: 3545,
            aggkind: "n",
            aggnumdirectargs: 0,
            aggsortop: 0,
            aggtranstype: 17,
        }, // string_agg(bytea)
        AggregateEntry {
            aggfnoid: 3175,
            aggkind: "n",
            aggnumdirectargs: 0,
            aggsortop: 0,
            aggtranstype: 2281,
        }, // json_agg
        AggregateEntry {
            aggfnoid: 3267,
            aggkind: "n",
            aggnumdirectargs: 0,
            aggsortop: 0,
            aggtranstype: 2281,
        }, // jsonb_agg
        // statistics
        AggregateEntry {
            aggfnoid: 2155,
            aggkind: "n",
            aggnumdirectargs: 0,
            aggsortop: 0,
            aggtranstype: 1700,
        }, // stddev_pop
        AggregateEntry {
            aggfnoid: 2156,
            aggkind: "n",
            aggnumdirectargs: 0,
            aggsortop: 0,
            aggtranstype: 1700,
        }, // stddev_samp
        AggregateEntry {
            aggfnoid: 2157,
            aggkind: "n",
            aggnumdirectargs: 0,
            aggsortop: 0,
            aggtranstype: 1700,
        }, // var_pop
        AggregateEntry {
            aggfnoid: 2158,
            aggkind: "n",
            aggnumdirectargs: 0,
            aggsortop: 0,
            aggtranstype: 1700,
        }, // var_samp
        // hypothetical-set / ordered-set
        AggregateEntry {
            aggfnoid: 3972,
            aggkind: "o",
            aggnumdirectargs: 1,
            aggsortop: 0,
            aggtranstype: 2281,
        }, // percentile_cont(double)
        AggregateEntry {
            aggfnoid: 3974,
            aggkind: "o",
            aggnumdirectargs: 1,
            aggsortop: 0,
            aggtranstype: 2281,
        }, // percentile_disc(any)
        AggregateEntry {
            aggfnoid: 3992,
            aggkind: "o",
            aggnumdirectargs: 0,
            aggsortop: 0,
            aggtranstype: 2281,
        }, // mode()
    ]
}

fn make_aggregate_row(entry: &AggregateEntry) -> Vec<TypedExpr> {
    vec![
        int_literal(entry.aggfnoid),
        text_literal(entry.aggkind),
        int_literal(entry.aggnumdirectargs),
        int_literal(0),      // aggtransfn
        int_literal(0),      // aggfinalfn
        int_literal(0),      // aggcombinefn
        int_literal(0),      // aggserialfn
        int_literal(0),      // aggdeserialfn
        int_literal(0),      // aggmtransfn
        int_literal(0),      // aggminvtransfn
        int_literal(0),      // aggmfinalfn
        bool_literal(false), // aggfinalextra
        bool_literal(false), // aggmfinalextra
        text_literal("r"),   // aggfinalmodify - read_only
        text_literal("r"),   // aggmfinalmodify
        int_literal(entry.aggsortop),
        int_literal(entry.aggtranstype),
        int_literal(0),               // aggtransspace
        int_literal(0),               // aggmtranstype
        int_literal(0),               // aggmtransspace
        null_literal(DataType::Text), // agginitval
        null_literal(DataType::Text), // aggminitval
    ]
}

// ---------------------------------------------------------------
// pg_catalog.pg_amop
// ---------------------------------------------------------------

struct PgvectorOpclassSpec {
    am_oid: i32,
    opcname: &'static str,
    opcintype: i32,
    operator: &'static str,
    is_default: bool,
}

const PGVECTOR_OPCLASS_SPECS: &[PgvectorOpclassSpec] = &[
    PgvectorOpclassSpec {
        am_oid: COMPAT_PGVECTOR_HNSW_AM_OID,
        opcname: "vector_l2_ops",
        opcintype: COMPAT_PGVECTOR_VECTOR_OID,
        operator: "<->",
        is_default: true,
    },
    PgvectorOpclassSpec {
        am_oid: COMPAT_PGVECTOR_HNSW_AM_OID,
        opcname: "vector_ip_ops",
        opcintype: COMPAT_PGVECTOR_VECTOR_OID,
        operator: "<#>",
        is_default: false,
    },
    PgvectorOpclassSpec {
        am_oid: COMPAT_PGVECTOR_HNSW_AM_OID,
        opcname: "vector_cosine_ops",
        opcintype: COMPAT_PGVECTOR_VECTOR_OID,
        operator: "<=>",
        is_default: false,
    },
    PgvectorOpclassSpec {
        am_oid: COMPAT_PGVECTOR_HNSW_AM_OID,
        opcname: "vector_l1_ops",
        opcintype: COMPAT_PGVECTOR_VECTOR_OID,
        operator: "<+>",
        is_default: false,
    },
    PgvectorOpclassSpec {
        am_oid: COMPAT_PGVECTOR_HNSW_AM_OID,
        opcname: "halfvec_l2_ops",
        opcintype: COMPAT_PGVECTOR_HALFVEC_OID,
        operator: "<->",
        is_default: true,
    },
    PgvectorOpclassSpec {
        am_oid: COMPAT_PGVECTOR_HNSW_AM_OID,
        opcname: "halfvec_ip_ops",
        opcintype: COMPAT_PGVECTOR_HALFVEC_OID,
        operator: "<#>",
        is_default: false,
    },
    PgvectorOpclassSpec {
        am_oid: COMPAT_PGVECTOR_HNSW_AM_OID,
        opcname: "halfvec_cosine_ops",
        opcintype: COMPAT_PGVECTOR_HALFVEC_OID,
        operator: "<=>",
        is_default: false,
    },
    PgvectorOpclassSpec {
        am_oid: COMPAT_PGVECTOR_HNSW_AM_OID,
        opcname: "halfvec_l1_ops",
        opcintype: COMPAT_PGVECTOR_HALFVEC_OID,
        operator: "<+>",
        is_default: false,
    },
    PgvectorOpclassSpec {
        am_oid: COMPAT_PGVECTOR_HNSW_AM_OID,
        opcname: "sparsevec_l2_ops",
        opcintype: COMPAT_PGVECTOR_SPARSEVEC_OID,
        operator: "<->",
        is_default: true,
    },
    PgvectorOpclassSpec {
        am_oid: COMPAT_PGVECTOR_HNSW_AM_OID,
        opcname: "sparsevec_ip_ops",
        opcintype: COMPAT_PGVECTOR_SPARSEVEC_OID,
        operator: "<#>",
        is_default: false,
    },
    PgvectorOpclassSpec {
        am_oid: COMPAT_PGVECTOR_HNSW_AM_OID,
        opcname: "sparsevec_cosine_ops",
        opcintype: COMPAT_PGVECTOR_SPARSEVEC_OID,
        operator: "<=>",
        is_default: false,
    },
    PgvectorOpclassSpec {
        am_oid: COMPAT_PGVECTOR_HNSW_AM_OID,
        opcname: "sparsevec_l1_ops",
        opcintype: COMPAT_PGVECTOR_SPARSEVEC_OID,
        operator: "<+>",
        is_default: false,
    },
    PgvectorOpclassSpec {
        am_oid: COMPAT_PGVECTOR_HNSW_AM_OID,
        opcname: "bit_hamming_ops",
        opcintype: COMPAT_PG_BIT_OID,
        operator: "<~>",
        is_default: true,
    },
    PgvectorOpclassSpec {
        am_oid: COMPAT_PGVECTOR_HNSW_AM_OID,
        opcname: "bit_jaccard_ops",
        opcintype: COMPAT_PG_BIT_OID,
        operator: "<%>",
        is_default: false,
    },
    PgvectorOpclassSpec {
        am_oid: COMPAT_PGVECTOR_IVFFLAT_AM_OID,
        opcname: "vector_l2_ops",
        opcintype: COMPAT_PGVECTOR_VECTOR_OID,
        operator: "<->",
        is_default: true,
    },
    PgvectorOpclassSpec {
        am_oid: COMPAT_PGVECTOR_IVFFLAT_AM_OID,
        opcname: "vector_ip_ops",
        opcintype: COMPAT_PGVECTOR_VECTOR_OID,
        operator: "<#>",
        is_default: false,
    },
    PgvectorOpclassSpec {
        am_oid: COMPAT_PGVECTOR_IVFFLAT_AM_OID,
        opcname: "vector_cosine_ops",
        opcintype: COMPAT_PGVECTOR_VECTOR_OID,
        operator: "<=>",
        is_default: false,
    },
    PgvectorOpclassSpec {
        am_oid: COMPAT_PGVECTOR_IVFFLAT_AM_OID,
        opcname: "halfvec_l2_ops",
        opcintype: COMPAT_PGVECTOR_HALFVEC_OID,
        operator: "<->",
        is_default: true,
    },
    PgvectorOpclassSpec {
        am_oid: COMPAT_PGVECTOR_IVFFLAT_AM_OID,
        opcname: "halfvec_ip_ops",
        opcintype: COMPAT_PGVECTOR_HALFVEC_OID,
        operator: "<#>",
        is_default: false,
    },
    PgvectorOpclassSpec {
        am_oid: COMPAT_PGVECTOR_IVFFLAT_AM_OID,
        opcname: "halfvec_cosine_ops",
        opcintype: COMPAT_PGVECTOR_HALFVEC_OID,
        operator: "<=>",
        is_default: false,
    },
    PgvectorOpclassSpec {
        am_oid: COMPAT_PGVECTOR_IVFFLAT_AM_OID,
        opcname: "bit_hamming_ops",
        opcintype: COMPAT_PG_BIT_OID,
        operator: "<~>",
        is_default: true,
    },
];

pub(super) fn pg_amop_fields() -> Vec<ResultField> {
    vec![
        oid_field("oid"),
        oid_field("amopfamily"),
        oid_field("amoplefttype"),
        oid_field("amoprighttype"),
        int_field("amopstrategy"),
        internal_char_field("amoppurpose"),
        oid_field("amopopr"),
        oid_field("amopmethod"),
        oid_field("amopsortfamily"),
    ]
}

pub(super) fn build_pg_amop_plan() -> DbResult<LogicalPlan> {
    let rows = PGVECTOR_OPCLASS_SPECS
        .iter()
        .map(|spec| {
            let family_oid = compat_pgvector_opclass_oid(spec.am_oid, spec.opcname);
            let amop_oid = aiondb_core::compat_function_oid(&format!(
                "amop:pgvector:{}:{}:{}",
                spec.am_oid, spec.opcname, spec.operator
            ));
            vec![
                int_literal(amop_oid),
                int_literal(family_oid),
                int_literal(spec.opcintype),
                int_literal(spec.opcintype),
                int_literal(1),
                text_literal("s"),
                int_literal(compat_pgvector_operator_oid(spec.opcintype, spec.operator)),
                int_literal(spec.am_oid),
                int_literal(0),
            ]
        })
        .collect();
    Ok(project_values(pg_amop_fields(), rows))
}

// ---------------------------------------------------------------
// pg_catalog.pg_amproc
// ---------------------------------------------------------------

pub(super) fn pg_amproc_fields() -> Vec<ResultField> {
    vec![
        oid_field("oid"),
        oid_field("amprocfamily"),
        oid_field("amproclefttype"),
        oid_field("amprocrighttype"),
        int_field("amprocnum"),
        oid_field("amproc"),
    ]
}

pub(super) fn build_pg_amproc_plan() -> DbResult<LogicalPlan> {
    let rows = PGVECTOR_OPCLASS_SPECS
        .iter()
        .map(|spec| {
            let family_oid = compat_pgvector_opclass_oid(spec.am_oid, spec.opcname);
            let argtypes = format!("{} {}", spec.opcintype, spec.opcintype);
            let proc_oid =
                compat_pgvector_function_oid(pgvector_support_proc_name(spec.operator), &argtypes);
            let amproc_oid = aiondb_core::compat_function_oid(&format!(
                "amproc:pgvector:{}:{}:{}",
                spec.am_oid, spec.opcname, spec.operator
            ));
            vec![
                int_literal(amproc_oid),
                int_literal(family_oid),
                int_literal(spec.opcintype),
                int_literal(spec.opcintype),
                int_literal(1),
                int_literal(proc_oid),
            ]
        })
        .collect();
    Ok(project_values(pg_amproc_fields(), rows))
}

fn pgvector_support_proc_name(operator: &str) -> &'static str {
    match operator {
        "<->" => "l2_distance",
        "<#>" => "negative_inner_product",
        "<=>" => "cosine_distance",
        "<+>" => "l1_distance",
        "<~>" => "hamming_distance",
        "<%>" => "jaccard_distance",
        _ => "l2_distance",
    }
}

// ---------------------------------------------------------------
// pg_catalog.pg_opclass
// ---------------------------------------------------------------

pub(super) fn pg_opclass_fields() -> Vec<ResultField> {
    vec![
        oid_field("oid"),
        oid_field("opcmethod"),
        name_field("opcname"),
        oid_field("opcnamespace"),
        oid_field("opcowner"),
        oid_field("opcfamily"),
        oid_field("opcintype"),
        bool_field("opcdefault"),
        oid_field("opckeytype"),
    ]
}

pub(super) fn build_pg_opclass_plan() -> DbResult<LogicalPlan> {
    let rows = PGVECTOR_OPCLASS_SPECS
        .iter()
        .map(|spec| {
            let oid = compat_pgvector_opclass_oid(spec.am_oid, spec.opcname);
            vec![
                int_literal(oid),
                int_literal(spec.am_oid),
                text_literal(spec.opcname),
                int_literal(PG_CATALOG_NAMESPACE_OID),
                int_literal(COMPAT_BOOTSTRAP_ROLE_OID),
                int_literal(oid),
                int_literal(spec.opcintype),
                bool_literal(spec.is_default),
                int_literal(0),
            ]
        })
        .collect();
    Ok(project_values(pg_opclass_fields(), rows))
}

// ---------------------------------------------------------------
// pg_catalog.pg_opfamily
// ---------------------------------------------------------------

pub(super) fn pg_opfamily_fields() -> Vec<ResultField> {
    vec![
        oid_field("oid"),
        oid_field("opfmethod"),
        name_field("opfname"),
        oid_field("opfnamespace"),
        oid_field("opfowner"),
    ]
}

pub(super) fn build_pg_opfamily_plan() -> DbResult<LogicalPlan> {
    let rows = PGVECTOR_OPCLASS_SPECS
        .iter()
        .map(|spec| {
            let oid = compat_pgvector_opclass_oid(spec.am_oid, spec.opcname);
            vec![
                int_literal(oid),
                int_literal(spec.am_oid),
                text_literal(spec.opcname),
                int_literal(PG_CATALOG_NAMESPACE_OID),
                int_literal(COMPAT_BOOTSTRAP_ROLE_OID),
            ]
        })
        .collect();
    Ok(project_values(pg_opfamily_fields(), rows))
}

// ---------------------------------------------------------------
// pg_catalog.pg_conversion
// ---------------------------------------------------------------

pub(super) fn pg_conversion_fields() -> Vec<ResultField> {
    vec![
        oid_field("oid"),
        name_field("conname"),
        oid_field("connamespace"),
        oid_field("conowner"),
        int_field("conforencoding"),
        int_field("contoencoding"),
        oid_field("conproc"),
        bool_field("condefault"),
    ]
}

pub(super) fn build_pg_conversion_plan(_owner_oid: i32) -> DbResult<LogicalPlan> {
    Ok(project_values(pg_conversion_fields(), Vec::new()))
}

// ---------------------------------------------------------------
// pg_catalog.pg_language
// ---------------------------------------------------------------

pub(super) fn pg_language_fields() -> Vec<ResultField> {
    vec![
        oid_field("oid"),
        name_field("lanname"),
        oid_field("lanowner"),
        bool_field("lanispl"),
        bool_field("lanpltrusted"),
        oid_field("lanplcallfoid"),
        oid_field("laninline"),
        oid_field("lanvalidator"),
        ResultField {
            name: "lanacl".to_owned(),
            data_type: DataType::Array(Box::new(DataType::Text)),
            text_type_modifier: None,
            nullable: true,
        },
    ]
}

pub(super) fn build_pg_language_plan(owner_oid: i32) -> DbResult<LogicalPlan> {
    let fields = pg_language_fields();
    let mut rows: Vec<Vec<TypedExpr>> = vec![
        // internal language (OID 12)
        vec![
            int_literal(12),
            text_literal("internal"),
            int_literal(owner_oid),
            bool_literal(false),
            bool_literal(false),
            int_literal(0),
            int_literal(0),
            int_literal(0),
            null_literal(DataType::Array(Box::new(DataType::Text))),
        ],
        // SQL language (OID 14)
        vec![
            int_literal(14),
            text_literal("sql"),
            int_literal(owner_oid),
            bool_literal(false),
            bool_literal(true),
            int_literal(0),
            int_literal(0),
            int_literal(0),
            null_literal(DataType::Array(Box::new(DataType::Text))),
        ],
        // C language (OID 13)
        vec![
            int_literal(13),
            text_literal("c"),
            int_literal(owner_oid),
            bool_literal(false),
            bool_literal(false),
            int_literal(0),
            int_literal(0),
            int_literal(0),
            null_literal(DataType::Text),
        ],
    ];

    // User-added procedural languages registered via `CREATE LANGUAGE`.
    rows.extend(aiondb_eval::with_current_session_context(|context| {
        let mut extra: Vec<Vec<TypedExpr>> = Vec::new();
        for ((kind, name), (owner, _schema, _, options_joined, _, _)) in
            context.compat_misc_attrs.iter()
        {
            if kind != "CREATE LANGUAGE" {
                continue;
            }
            let oid = synth_oid_from_name(name);
            let owner_override = if owner.is_empty() {
                owner_oid
            } else {
                aiondb_core::compat_role_oid(owner)
            };
            let handler_name = options_joined
                .split(',')
                .map(str::trim)
                .find_map(|pair| pair.strip_prefix("handler=").map(str::to_owned));
            let inline_name = options_joined
                .split(',')
                .map(str::trim)
                .find_map(|pair| pair.strip_prefix("inline=").map(str::to_owned));
            let validator_name = options_joined
                .split(',')
                .map(str::trim)
                .find_map(|pair| pair.strip_prefix("validator=").map(str::to_owned));
            let handler_oid = handler_name
                .as_deref()
                .map(synth_oid_from_name)
                .unwrap_or(0);
            let inline_oid = inline_name.as_deref().map(synth_oid_from_name).unwrap_or(0);
            let validator_oid = validator_name
                .as_deref()
                .map(synth_oid_from_name)
                .unwrap_or(0);
            extra.push(vec![
                int_literal(oid),
                text_literal(name),
                int_literal(owner_override),
                bool_literal(true), // lanispl
                bool_literal(true), // lanpltrusted - default assumption
                int_literal(handler_oid),
                int_literal(inline_oid),
                int_literal(validator_oid),
                null_literal(DataType::Array(Box::new(DataType::Text))),
            ]);
        }
        extra
    }));

    Ok(project_values(fields, rows))
}

// ---------------------------------------------------------------
// pg_catalog.pg_collation
// ---------------------------------------------------------------

pub(super) fn pg_collation_fields() -> Vec<ResultField> {
    vec![
        oid_field("oid"),
        name_field("collname"),
        oid_field("collnamespace"),
        oid_field("collowner"),
        internal_char_field("collprovider"),
        bool_field("collisdeterministic"),
        int_field("collencoding"),
        nullable_text_field("collcollate"),
        nullable_text_field("collctype"),
        nullable_text_field("colliculocale"),
        nullable_text_field("collicurules"),
        nullable_text_field("collversion"),
    ]
}

pub(super) fn build_pg_collation_plan(owner_oid: i32) -> DbResult<LogicalPlan> {
    let fields = pg_collation_fields();
    let rows = vec![
        vec![
            int_literal(100),
            text_literal("default"),
            int_literal(PG_CATALOG_NAMESPACE_OID),
            int_literal(owner_oid),
            text_literal("d"),
            bool_literal(true),
            int_literal(-1),
            null_literal(DataType::Text),
            null_literal(DataType::Text),
            null_literal(DataType::Text),
            null_literal(DataType::Text),
            null_literal(DataType::Text),
        ],
        vec![
            int_literal(950),
            text_literal("C"),
            int_literal(PG_CATALOG_NAMESPACE_OID),
            int_literal(owner_oid),
            text_literal("c"),
            bool_literal(true),
            int_literal(-1),
            text_literal("C"),
            text_literal("C"),
            null_literal(DataType::Text),
            null_literal(DataType::Text),
            null_literal(DataType::Text),
        ],
        vec![
            int_literal(951),
            text_literal("POSIX"),
            int_literal(PG_CATALOG_NAMESPACE_OID),
            int_literal(owner_oid),
            text_literal("c"),
            bool_literal(true),
            int_literal(-1),
            text_literal("POSIX"),
            text_literal("POSIX"),
            null_literal(DataType::Text),
            null_literal(DataType::Text),
            null_literal(DataType::Text),
        ],
    ];
    Ok(project_values(fields, rows))
}

// ---------------------------------------------------------------
// pg_catalog.pg_tablespace
// ---------------------------------------------------------------

pub(super) fn pg_tablespace_fields() -> Vec<ResultField> {
    vec![
        oid_field("oid"),
        name_field("spcname"),
        oid_field("spcowner"),
        ResultField {
            name: "spcacl".to_owned(),
            data_type: DataType::Array(Box::new(DataType::Text)),
            text_type_modifier: None,
            nullable: true,
        },
        ResultField {
            name: "spcoptions".to_owned(),
            data_type: DataType::Array(Box::new(DataType::Text)),
            text_type_modifier: None,
            nullable: true,
        },
    ]
}

pub(super) fn build_pg_tablespace_plan(owner_oid: i32) -> DbResult<LogicalPlan> {
    let fields = pg_tablespace_fields();
    let mut rows = vec![
        vec![
            int_literal(COMPAT_PG_DEFAULT_TABLESPACE_OID),
            text_literal("pg_default"),
            int_literal(owner_oid),
            null_literal(DataType::Array(Box::new(DataType::Text))),
            null_literal(DataType::Array(Box::new(DataType::Text))),
        ],
        vec![
            int_literal(COMPAT_PG_GLOBAL_TABLESPACE_OID),
            text_literal("pg_global"),
            int_literal(owner_oid),
            null_literal(DataType::Array(Box::new(DataType::Text))),
            null_literal(DataType::Array(Box::new(DataType::Text))),
        ],
    ];
    // Surface CREATE TABLESPACE entries from the session compat registry.
    // `pg_tablespace` also returns user-created tablespaces, so `\db`
    // in psql and ORM introspection stay in sync.
    let session_rows = aiondb_eval::with_current_session_context(|context| {
        let mut emitted: Vec<Vec<TypedExpr>> = Vec::new();
        for ((kind, name), (owner, _, _, options_joined, _, _)) in context.compat_misc_attrs.iter()
        {
            if kind != "CREATE TABLESPACE" {
                continue;
            }
            let oid = synth_oid_from_name(name);
            let owner_resolved = if owner.is_empty() {
                owner_oid
            } else {
                aiondb_core::compat_role_oid(owner)
            };
            let spcoptions: Vec<aiondb_core::Value> = options_joined
                .split(',')
                .map(str::trim)
                .filter(|pair| !pair.is_empty())
                .map(|pair| aiondb_core::Value::Text(pair.to_owned()))
                .collect();
            emitted.push(vec![
                int_literal(oid),
                text_literal(name),
                int_literal(owner_resolved),
                null_literal(DataType::Array(Box::new(DataType::Text))),
                if spcoptions.is_empty() {
                    null_literal(DataType::Array(Box::new(DataType::Text)))
                } else {
                    typed_array_literal(spcoptions, DataType::Text)
                },
            ]);
        }
        emitted
    });
    rows.extend(session_rows);
    Ok(project_values(fields, rows))
}

// ---------------------------------------------------------------
// pg_catalog.pg_range
// ---------------------------------------------------------------

pub(super) fn pg_range_fields() -> Vec<ResultField> {
    vec![
        oid_field("rngtypid"),
        oid_field("rngsubtype"),
        oid_field("rngmultitypid"),
        oid_field("rngcollation"),
        oid_field("rngsubopc"),
        oid_field("rngcanonical"),
        oid_field("rngsubdiff"),
    ]
}

pub(super) fn build_pg_range_plan() -> DbResult<LogicalPlan> {
    Ok(project_values(pg_range_fields(), Vec::new()))
}

// ---------------------------------------------------------------
// pg_catalog.pg_enum
// ---------------------------------------------------------------

pub(super) fn pg_enum_fields() -> Vec<ResultField> {
    vec![
        oid_field("oid"),
        oid_field("enumtypid"),
        double_field("enumsortorder"),
        name_field("enumlabel"),
    ]
}

pub(super) fn build_pg_enum_plan_with_catalog(
    catalog: &Arc<dyn CatalogReader>,
    txn_id: TxnId,
) -> DbResult<LogicalPlan> {
    // Populate from list_user_types so pg_dump and ORM enum probes see
    // the real labels. Each label gets a synthetic oid (compat hash);
    // enumsortorder is a 1-based monotonically increasing index.
    let mut rows: Vec<Vec<TypedExpr>> = Vec::new();
    for ut in catalog.list_user_types(txn_id)? {
        if ut.enum_labels.is_empty() {
            continue;
        }
        for (idx, label) in ut.enum_labels.iter().enumerate() {
            let synth = aiondb_core::compat_function_oid(&format!("{}.{label}", ut.name));
            rows.push(vec![
                int_literal(synth),
                int_literal(ut.oid),
                TypedExpr::literal(
                    aiondb_core::Value::Double(idx as f64 + 1.0),
                    DataType::Double,
                    false,
                ),
                text_literal(label),
            ]);
        }
    }
    Ok(project_values(pg_enum_fields(), rows))
}

// ---------------------------------------------------------------
// pg_catalog.pg_trigger
// ---------------------------------------------------------------

pub(super) fn pg_trigger_fields() -> Vec<ResultField> {
    vec![
        oid_field("oid"),
        oid_field("tgrelid"),
        oid_field("tgparentid"),
        name_field("tgname"),
        oid_field("tgfoid"),
        int_field("tgtype"),
        internal_char_field("tgenabled"),
        bool_field("tgisinternal"),
        oid_field("tgconstrrelid"),
        oid_field("tgconstrindid"),
        oid_field("tgconstraint"),
        bool_field("tgdeferrable"),
        bool_field("tginitdeferred"),
        int_field("tgnargs"),
        ResultField {
            name: "tgattr".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: Some(TextTypeModifier::Int2Vector),
            nullable: true,
        },
        ResultField {
            name: "tgargs".to_owned(),
            data_type: DataType::Blob,
            text_type_modifier: None,
            nullable: true,
        },
        nullable_text_field("tgqual"),
        nullable_text_field("tgoldtable"),
        nullable_text_field("tgnewtable"),
    ]
}

pub(super) fn build_pg_trigger_plan(
    catalog: &Arc<dyn CatalogReader>,
    txn_id: TxnId,
    default_schema: Option<&str>,
) -> DbResult<LogicalPlan> {
    use aiondb_catalog::{TriggerEventDescriptor, TriggerTimingDescriptor};

    let tables = list_user_tables(catalog, txn_id, default_schema)?;
    let fields = pg_trigger_fields();
    let mut rows: Vec<Vec<TypedExpr>> = Vec::new();
    let mut oid_counter: i32 = 50000; // start at a high range to avoid clashes

    for table in &tables {
        let table_oid = relation_id_to_oid(table);
        let triggers = catalog.list_triggers(txn_id, &table.name.to_string())?;

        for trigger in &triggers {
            oid_counter += 1;

            // Build tgtype bitmask (PostgreSQL encoding)
            let mut tgtype: i32 = 0;
            if trigger.for_each_row {
                tgtype |= 1; // TRIGGER_TYPE_ROW
            }
            match trigger.timing {
                TriggerTimingDescriptor::Before => tgtype |= 2, // TRIGGER_TYPE_BEFORE
                TriggerTimingDescriptor::After => {}            // AFTER = 0 (no bit set)
                TriggerTimingDescriptor::InsteadOf => tgtype |= 64, // TRIGGER_TYPE_INSTEAD
            }
            // Set bits for primary event and all extra events
            for evt in std::iter::once(&trigger.event).chain(trigger.extra_events.iter()) {
                match evt {
                    TriggerEventDescriptor::Insert => tgtype |= 4, // TRIGGER_TYPE_INSERT
                    TriggerEventDescriptor::Delete => tgtype |= 8, // TRIGGER_TYPE_DELETE
                    TriggerEventDescriptor::Update => tgtype |= 16, // TRIGGER_TYPE_UPDATE
                }
            }

            // tgfoid: function OID - we don't track it, use 0
            let tgfoid = 0i32;

            // Reflect ALTER TABLE/ALTER TRIGGER ENABLE/DISABLE state into
            // the `tgenabled` column so pg_trigger shows the right state.
            let tgenabled = aiondb_eval::with_current_session_context(|ctx| {
                let table_lc = table.name.object_name().to_ascii_lowercase();
                let trigger_lc = trigger.name.to_ascii_lowercase();
                ctx.compat_trigger_state
                    .get(&(table_lc.clone(), trigger_lc))
                    .or_else(|| ctx.compat_trigger_state.get(&(table_lc, "*".to_owned())))
                    .map(|s| match s.as_str() {
                        "disabled" => "D",
                        "replica" => "R",
                        "always" => "A",
                        _ => "O",
                    })
                    .unwrap_or("O")
                    .to_owned()
            });

            let row = vec![
                int_literal(oid_counter),    // oid
                int_literal(table_oid),      // tgrelid
                int_literal(0),              // tgparentid
                text_literal(&trigger.name), // tgname
                int_literal(tgfoid),         // tgfoid
                int_literal(tgtype),         // tgtype
                text_literal(&tgenabled), // tgenabled ('O'=origin, 'D'=disabled, 'R'=replica, 'A'=always)
                bool_literal(false),      // tgisinternal
                int_literal(0),           // tgconstrrelid
                int_literal(0),           // tgconstrindid
                int_literal(0),           // tgconstraint
                bool_literal(false),      // tgdeferrable
                bool_literal(false),      // tginitdeferred
                int_literal(i32::try_from(trigger.function_args.len()).unwrap_or(i32::MAX)), // tgnargs
                null_literal(DataType::Text), // tgattr
                if trigger.function_args.is_empty() {
                    null_literal(DataType::Blob) // tgargs
                } else {
                    TypedExpr::literal(
                        Value::Blob(encode_trigger_args(&trigger.function_args)),
                        DataType::Blob,
                        false,
                    )
                }, // tgargs
                null_literal(DataType::Text), // tgqual
                null_literal(DataType::Text), // tgoldtable
                null_literal(DataType::Text), // tgnewtable
            ];
            rows.push(row);
        }
    }

    Ok(project_values(fields, rows))
}

fn encode_trigger_args(args: &[String]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for arg in args {
        bytes.extend_from_slice(arg.as_bytes());
        bytes.push(0);
    }
    bytes
}

// ---------------------------------------------------------------
// pg_catalog.pg_rewrite
// ---------------------------------------------------------------

pub(super) fn pg_rewrite_fields() -> Vec<ResultField> {
    vec![
        oid_field("oid"),
        name_field("rulename"),
        oid_field("ev_class"),
        internal_char_field("ev_type"),
        internal_char_field("ev_enabled"),
        bool_field("is_instead"),
        nullable_text_field("ev_qual"),
        nullable_text_field("ev_action"),
    ]
}

pub(super) fn build_pg_rewrite_plan() -> DbResult<LogicalPlan> {
    let rows = aiondb_eval::with_current_session_context(|context| {
        let mut rows: Vec<Vec<TypedExpr>> = Vec::new();
        for ((relation, event), action) in context.compat_rules.iter() {
            // Synthesize a rule name since compat_rules is keyed by
            // `(relation, event)` rather than by pg-style rule name.
            let rule_name = format!("{}_{}", relation, event.to_ascii_lowercase());
            let rule_oid = synth_oid_from_name(&rule_name);
            let ev_relation_oid = synth_oid_from_name(&relation.to_ascii_lowercase());
            let ev_type = match event.to_ascii_uppercase().as_str() {
                "SELECT" => "1",
                "UPDATE" => "2",
                "INSERT" => "3",
                "DELETE" => "4",
                _ => "1",
            };
            let is_instead = action.to_ascii_lowercase().contains("instead of");
            rows.push(vec![
                int_literal(rule_oid),
                text_literal(&rule_name),
                int_literal(ev_relation_oid),
                text_literal(ev_type),
                text_literal("O"), // ev_enabled = origin
                bool_literal(is_instead),
                null_literal(DataType::Text),
                text_literal(action),
            ]);
        }
        rows
    });
    Ok(project_values(pg_rewrite_fields(), rows))
}

// ---------------------------------------------------------------
// pg_catalog.pg_inherits
// ---------------------------------------------------------------

pub(super) fn pg_inherits_fields() -> Vec<ResultField> {
    vec![
        oid_field("inhrelid"),
        oid_field("inhparent"),
        int_field("inhseqno"),
        bool_field("inhdetachpending"),
    ]
}

pub(super) fn build_pg_inherits_plan() -> DbResult<LogicalPlan> {
    Ok(project_values(pg_inherits_fields(), Vec::new()))
}

// ---------------------------------------------------------------
// pg_catalog.pg_shdescription
// ---------------------------------------------------------------

pub(super) fn pg_shdescription_fields() -> Vec<ResultField> {
    vec![
        oid_field("objoid"),
        oid_field("classoid"),
        text_field("description"),
    ]
}

pub(super) fn build_pg_shdescription_plan() -> DbResult<LogicalPlan> {
    Ok(project_values(pg_shdescription_fields(), Vec::new()))
}

// ---------------------------------------------------------------
// pg_catalog.pg_extension
// ---------------------------------------------------------------

pub(super) fn pg_extension_fields() -> Vec<ResultField> {
    vec![
        oid_field("oid"),
        name_field("extname"),
        oid_field("extowner"),
        oid_field("extnamespace"),
        bool_field("extrelocatable"),
        text_field("extversion"),
        ResultField {
            name: "extconfig".to_owned(),
            data_type: DataType::Array(Box::new(DataType::Text)),
            text_type_modifier: None,
            nullable: true,
        },
        ResultField {
            name: "extcondition".to_owned(),
            data_type: DataType::Array(Box::new(DataType::Text)),
            text_type_modifier: None,
            nullable: true,
        },
    ]
}

pub(super) fn build_pg_extension_plan() -> DbResult<LogicalPlan> {
    // Cross-reference the extension registry (persisted installs) with
    // the session compat_misc_attrs (CREATE EXTENSION overrides for SCHEMA/
    // VERSION/OWNER and ALTER EXTENSION mutations).
    let rows = if let Some(registry) = aiondb_eval::extension_registry() {
        aiondb_eval::with_current_session_context(|context| {
            registry
                .list_installed()
                .into_iter()
                .map(|ext| {
                    let key = ("CREATE EXTENSION".to_owned(), ext.name.to_ascii_lowercase());
                    let attrs = context.compat_misc_attrs.get(&key);
                    let (owner, schema, _state, _opts, _ts, version) =
                        attrs.cloned().unwrap_or_default();
                    let owner_oid = if owner.is_empty() {
                        10 // bootstrap superuser
                    } else {
                        aiondb_core::compat_role_oid(&owner)
                    };
                    let namespace_oid = if schema.is_empty() {
                        PG_CATALOG_NAMESPACE_OID
                    } else {
                        synth_oid_from_name(&schema.to_ascii_lowercase())
                    };
                    let version_text = if version.is_empty() {
                        ext.version.clone()
                    } else {
                        version
                    };
                    vec![
                        int_literal(ext.oid),
                        text_literal(&ext.name),
                        int_literal(owner_oid),
                        int_literal(namespace_oid),
                        bool_literal(ext.relocatable),
                        text_literal(&version_text),
                        null_literal(DataType::Array(Box::new(DataType::Text))),
                        null_literal(DataType::Array(Box::new(DataType::Text))),
                    ]
                })
                .collect()
        })
    } else {
        Vec::new()
    };
    Ok(project_values(pg_extension_fields(), rows))
}

// ---------------------------------------------------------------
// pg_catalog.pg_event_trigger
// ---------------------------------------------------------------

pub(super) fn pg_event_trigger_fields() -> Vec<ResultField> {
    vec![
        oid_field("oid"),
        name_field("evtname"),
        text_field("evtevent"),
        oid_field("evtowner"),
        oid_field("evtfoid"),
        internal_char_field("evtenabled"),
        ResultField {
            name: "evttags".to_owned(),
            data_type: DataType::Array(Box::new(DataType::Text)),
            text_type_modifier: None,
            nullable: true,
        },
    ]
}

pub(super) fn build_pg_event_trigger_plan() -> DbResult<LogicalPlan> {
    let rows = aiondb_eval::with_current_session_context(|context| {
        let mut rows: Vec<Vec<TypedExpr>> = Vec::new();
        for ((kind, name), (owner, _, state, options_joined, _, _)) in
            context.compat_misc_attrs.iter()
        {
            if kind != "CREATE EVENT TRIGGER" {
                continue;
            }
            let oid = synth_oid_from_name(name);
            let event = options_joined
                .split(',')
                .map(str::trim)
                .find_map(|pair| pair.strip_prefix("event=").map(str::to_owned))
                .unwrap_or_else(|| "ddl_command_end".to_owned());
            let evtfoid = 0i32;
            let owner_oid = if owner.is_empty() {
                0
            } else {
                aiondb_core::compat_role_oid(owner)
            };
            let evtenabled = match state.as_str() {
                "disabled" => "D",
                "replica" => "R",
                "always" => "A",
                _ => "O",
            };
            let when_tags_csv = options_joined
                .split(',')
                .map(str::trim)
                .find_map(|pair| pair.strip_prefix("when_tags=").map(str::to_owned))
                .unwrap_or_default();
            let tag_elements: Vec<aiondb_core::Value> = when_tags_csv
                .split(',')
                .map(str::trim)
                .filter(|tag| !tag.is_empty())
                .map(|tag| aiondb_core::Value::Text(tag.to_owned()))
                .collect();
            rows.push(vec![
                int_literal(oid),
                text_literal(name),
                text_literal(&event),
                int_literal(owner_oid),
                int_literal(evtfoid),
                text_literal(evtenabled),
                if tag_elements.is_empty() {
                    null_literal(DataType::Array(Box::new(DataType::Text)))
                } else {
                    typed_array_literal(tag_elements, DataType::Text)
                },
            ]);
        }
        rows
    });
    Ok(project_values(pg_event_trigger_fields(), rows))
}

// ---------------------------------------------------------------
// pg_catalog.pg_foreign_server
// ---------------------------------------------------------------

pub(super) fn pg_foreign_server_fields() -> Vec<ResultField> {
    vec![
        oid_field("oid"),
        name_field("srvname"),
        oid_field("srvowner"),
        oid_field("srvfdw"),
        nullable_text_field("srvtype"),
        nullable_text_field("srvversion"),
        ResultField {
            name: "srvacl".to_owned(),
            data_type: DataType::Array(Box::new(DataType::Text)),
            text_type_modifier: None,
            nullable: true,
        },
        ResultField {
            name: "srvoptions".to_owned(),
            data_type: DataType::Array(Box::new(DataType::Text)),
            text_type_modifier: None,
            nullable: true,
        },
    ]
}

pub(super) fn build_pg_foreign_server_plan() -> DbResult<LogicalPlan> {
    // Read CREATE SERVER entries from the current session's compat state.
    // Key layout in `compat_misc_attrs`: ("CREATE SERVER", lower(name)) →
    // (owner, schema, state, options_joined, tablespace, version).
    let rows = aiondb_eval::with_current_session_context(|context| {
        let mut rows: Vec<Vec<TypedExpr>> = Vec::new();
        for ((kind, name), (_, _, _, options, _, version)) in context.compat_misc_attrs.iter() {
            if kind != "CREATE SERVER" {
                continue;
            }
            // Stable synthetic oid from the server name - pg_foreign_server
            // clients only need it to join, not to resolve a real catalog
            // row.
            let oid = synth_oid_from_name(name);
            let srvname = name.clone();
            let srvtype = options
                .split(',')
                .map(|pair| pair.trim())
                .find_map(|pair| pair.strip_prefix("type="))
                .map(str::to_owned);
            let srvversion = if version.is_empty() {
                None
            } else {
                Some(version.clone())
            };
            let srvoptions_elements: Vec<aiondb_core::Value> = options
                .split(',')
                .map(|pair| pair.trim())
                .filter(|pair| !pair.is_empty())
                .map(|pair| aiondb_core::Value::Text(pair.to_owned()))
                .collect();
            rows.push(vec![
                int_literal(oid),
                text_literal(&srvname),
                int_literal(0), // srvowner: role oid unresolved (0)
                int_literal(0), // srvfdw: fdw oid unresolved
                match srvtype {
                    Some(t) => text_literal(&t),
                    None => null_literal(DataType::Text),
                },
                match srvversion {
                    Some(v) => text_literal(&v),
                    None => null_literal(DataType::Text),
                },
                null_literal(DataType::Array(Box::new(DataType::Text))),
                if srvoptions_elements.is_empty() {
                    null_literal(DataType::Array(Box::new(DataType::Text)))
                } else {
                    TypedExpr::literal(
                        Value::Array(srvoptions_elements),
                        DataType::Array(Box::new(DataType::Text)),
                        false,
                    )
                },
            ]);
        }
        rows
    });
    Ok(project_values(pg_foreign_server_fields(), rows))
}

// ---------------------------------------------------------------
// pg_catalog.pg_foreign_table
// ---------------------------------------------------------------

pub(super) fn pg_foreign_table_fields() -> Vec<ResultField> {
    vec![
        oid_field("ftrelid"),
        oid_field("ftserver"),
        ResultField {
            name: "ftoptions".to_owned(),
            data_type: DataType::Array(Box::new(DataType::Text)),
            text_type_modifier: None,
            nullable: true,
        },
    ]
}

pub(super) fn build_pg_foreign_table_plan() -> DbResult<LogicalPlan> {
    Ok(project_values(pg_foreign_table_fields(), Vec::new()))
}

/// Synthetic oid derived deterministically from the object name. Used when
/// the compat layer does not assign real pg_catalog oids but still wants
/// pg_catalog views to return stable joinable identifiers across queries.
fn synth_oid_from_name(name: &str) -> i32 {
    let mut hash: u32 = 0x811c_9dc5;
    for byte in name.bytes() {
        hash ^= u32::from(byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    // Stay above pg_catalog reserved range (< 16384) by masking and adding
    // a small offset.
    ((hash & 0x7fff_ffff) | 0x8000).cast_signed()
}

// ---------------------------------------------------------------
// pg_catalog.pg_foreign_data_wrapper
// ---------------------------------------------------------------

pub(super) fn pg_foreign_data_wrapper_fields() -> Vec<ResultField> {
    vec![
        oid_field("oid"),
        name_field("fdwname"),
        oid_field("fdwowner"),
        oid_field("fdwhandler"),
        oid_field("fdwvalidator"),
        ResultField {
            name: "fdwacl".to_owned(),
            data_type: DataType::Array(Box::new(DataType::Text)),
            text_type_modifier: None,
            nullable: true,
        },
        ResultField {
            name: "fdwoptions".to_owned(),
            data_type: DataType::Array(Box::new(DataType::Text)),
            text_type_modifier: None,
            nullable: true,
        },
    ]
}

pub(super) fn build_pg_foreign_data_wrapper_plan() -> DbResult<LogicalPlan> {
    let rows = aiondb_eval::with_current_session_context(|context| {
        let mut rows: Vec<Vec<TypedExpr>> = Vec::new();
        for ((kind, name), (owner, _, _, options_joined, _, _)) in context.compat_misc_attrs.iter()
        {
            if kind != "CREATE FOREIGN DATA WRAPPER" {
                continue;
            }
            let oid = synth_oid_from_name(name);
            let owner_oid = if owner.is_empty() {
                0
            } else {
                aiondb_core::compat_role_oid(owner)
            };
            let handler = options_joined
                .split(',')
                .map(str::trim)
                .find_map(|pair| pair.strip_prefix("handler=").map(str::to_owned));
            let validator = options_joined
                .split(',')
                .map(str::trim)
                .find_map(|pair| pair.strip_prefix("validator=").map(str::to_owned));
            let handler_oid = handler
                .as_deref()
                .map(|h| synth_oid_from_name(h))
                .unwrap_or(0);
            let validator_oid = validator
                .as_deref()
                .map(|v| synth_oid_from_name(v))
                .unwrap_or(0);
            let fdwoptions_elements: Vec<aiondb_core::Value> = options_joined
                .split(',')
                .map(str::trim)
                .filter(|pair| {
                    !pair.is_empty()
                        && !pair.starts_with("handler=")
                        && !pair.starts_with("validator=")
                })
                .map(|pair| aiondb_core::Value::Text(pair.to_owned()))
                .collect();
            rows.push(vec![
                int_literal(oid),
                text_literal(name),
                int_literal(owner_oid),
                int_literal(handler_oid),
                int_literal(validator_oid),
                null_literal(DataType::Array(Box::new(DataType::Text))), // fdwacl
                if fdwoptions_elements.is_empty() {
                    null_literal(DataType::Array(Box::new(DataType::Text)))
                } else {
                    typed_array_literal(fdwoptions_elements, DataType::Text)
                },
            ]);
        }
        rows
    });
    Ok(project_values(pg_foreign_data_wrapper_fields(), rows))
}

// ---------------------------------------------------------------
// pg_catalog.pg_user_mappings
// ---------------------------------------------------------------

pub(super) fn pg_user_mappings_fields() -> Vec<ResultField> {
    vec![
        oid_field("umid"),
        oid_field("srvid"),
        name_field("srvname"),
        oid_field("umuser"),
        nullable_name_field("usename"),
        ResultField {
            name: "umoptions".to_owned(),
            data_type: DataType::Array(Box::new(DataType::Text)),
            text_type_modifier: None,
            nullable: true,
        },
    ]
}

pub(super) fn build_pg_user_mappings_plan() -> DbResult<LogicalPlan> {
    // compat_misc_attrs key is ("CREATE USER MAPPING", "role@server").
    // The value tuple's `options_joined` field holds the USER MAPPING OPTIONS.
    let rows = aiondb_eval::with_current_session_context(|context| {
        let mut rows: Vec<Vec<TypedExpr>> = Vec::new();
        for ((kind, name), (_, _, _, options_joined, _, _)) in context.compat_misc_attrs.iter() {
            if kind != "CREATE USER MAPPING" {
                continue;
            }
            let Some((role, server)) = name.split_once('@') else {
                continue;
            };
            let um_oid = synth_oid_from_name(name);
            let server_oid = synth_oid_from_name(server);
            let um_user_oid = if role.eq_ignore_ascii_case("public")
                || role.eq_ignore_ascii_case("current_user")
                || role.eq_ignore_ascii_case("session_user")
            {
                0
            } else {
                aiondb_core::compat_role_oid(role)
            };
            let usename: TypedExpr = if role.eq_ignore_ascii_case("public") {
                null_literal(DataType::Text)
            } else {
                text_literal(role)
            };
            let options_elements: Vec<aiondb_core::Value> = options_joined
                .split(',')
                .map(str::trim)
                .filter(|p| !p.is_empty())
                .map(|p| aiondb_core::Value::Text(p.to_owned()))
                .collect();
            rows.push(vec![
                int_literal(um_oid),
                int_literal(server_oid),
                text_literal(server),
                int_literal(um_user_oid),
                usename,
                if options_elements.is_empty() {
                    null_literal(DataType::Array(Box::new(DataType::Text)))
                } else {
                    typed_array_literal(options_elements, DataType::Text)
                },
            ]);
        }
        rows
    });
    Ok(project_values(pg_user_mappings_fields(), rows))
}

// ---------------------------------------------------------------
// pg_catalog.pg_user_mapping (system catalog table form - distinct from
// the `pg_user_mappings` view that exposes role names. The catalog table
// stores oid + umuser/umserver/umoptions).
// ---------------------------------------------------------------

pub(super) fn pg_user_mapping_fields() -> Vec<ResultField> {
    vec![
        oid_field("oid"),
        oid_field("umuser"),
        oid_field("umserver"),
        ResultField {
            name: "umoptions".to_owned(),
            data_type: DataType::Array(Box::new(DataType::Text)),
            text_type_modifier: None,
            nullable: true,
        },
    ]
}

pub(super) fn build_pg_user_mapping_plan() -> DbResult<LogicalPlan> {
    let rows = aiondb_eval::with_current_session_context(|context| {
        let mut rows: Vec<Vec<TypedExpr>> = Vec::new();
        for ((kind, name), (_, _, _, options_joined, _, _)) in context.compat_misc_attrs.iter() {
            if kind != "CREATE USER MAPPING" {
                continue;
            }
            let Some((role, server)) = name.split_once('@') else {
                continue;
            };
            let um_oid = synth_oid_from_name(name);
            let server_oid = synth_oid_from_name(server);
            let um_user_oid = if role.eq_ignore_ascii_case("public")
                || role.eq_ignore_ascii_case("current_user")
                || role.eq_ignore_ascii_case("session_user")
            {
                0
            } else {
                aiondb_core::compat_role_oid(role)
            };
            let options_elements: Vec<aiondb_core::Value> = options_joined
                .split(',')
                .map(str::trim)
                .filter(|p| !p.is_empty())
                .map(|p| aiondb_core::Value::Text(p.to_owned()))
                .collect();
            rows.push(vec![
                int_literal(um_oid),
                int_literal(um_user_oid),
                int_literal(server_oid),
                if options_elements.is_empty() {
                    null_literal(DataType::Array(Box::new(DataType::Text)))
                } else {
                    typed_array_literal(options_elements, DataType::Text)
                },
            ]);
        }
        rows
    });
    Ok(project_values(pg_user_mapping_fields(), rows))
}

// ---------------------------------------------------------------
// pg_catalog.pg_db_role_setting
// ---------------------------------------------------------------

pub(super) fn pg_db_role_setting_fields() -> Vec<ResultField> {
    vec![
        oid_field("setdatabase"),
        oid_field("setrole"),
        ResultField {
            name: "setconfig".to_owned(),
            data_type: DataType::Array(Box::new(DataType::Text)),
            text_type_modifier: None,
            nullable: true,
        },
    ]
}

pub(super) fn build_pg_db_role_setting_plan() -> DbResult<LogicalPlan> {
    Ok(project_values(pg_db_role_setting_fields(), Vec::new()))
}

// ---------------------------------------------------------------
// pg_catalog.pg_matviews
// ---------------------------------------------------------------

pub(super) fn pg_matviews_fields() -> Vec<ResultField> {
    vec![
        name_field("schemaname"),
        name_field("matviewname"),
        nullable_name_field("matviewowner"),
        nullable_name_field("tablespace"),
        bool_field("hasindexes"),
        bool_field("ispopulated"),
        nullable_text_field("definition"),
    ]
}

pub(super) fn build_pg_matviews_plan(
    catalog: &Arc<dyn CatalogReader>,
    txn_id: TxnId,
    default_schema: Option<&str>,
) -> DbResult<LogicalPlan> {
    let tenant_filter = tenant_schema_filter(default_schema);
    let mut rows: Vec<Vec<TypedExpr>> = Vec::new();
    for schema in catalog.list_schemas(txn_id)? {
        if !schema_visible_with_tenant_filter(&schema.name, tenant_filter.as_deref()) {
            continue;
        }
        for view in catalog.list_views(txn_id, schema.schema_id)? {
            let Some(metadata) = parse_matview_sidecar(&view) else {
                continue;
            };
            let Some(table) = catalog.get_table(txn_id, &metadata.relation_name)? else {
                continue;
            };
            let owner_expr: TypedExpr = table
                .owner
                .as_ref()
                .map_or_else(|| null_literal(DataType::Text), |owner| text_literal(owner));
            let has_indexes = !catalog.list_indexes(txn_id, table.table_id)?.is_empty();
            rows.push(vec![
                text_literal(&visible_schema_name(
                    table.name.schema_name().unwrap_or("public"),
                    default_schema,
                )),
                text_literal(table.name.object_name()),
                owner_expr,
                null_literal(DataType::Text),
                bool_literal(has_indexes),
                bool_literal(metadata.relispopulated),
                text_literal(matview_sidecar_definition(&view.query_sql).unwrap_or("")),
            ]);
        }
    }
    Ok(project_values(pg_matviews_fields(), rows))
}

fn matview_sidecar_definition(query_sql: &str) -> Option<&str> {
    let sql = query_sql.trim_start();
    let (_, body) = sql.strip_prefix("/*")?.split_once("*/")?;
    Some(body.trim())
}

// ---------------------------------------------------------------
// pg_catalog.pg_policy
// ---------------------------------------------------------------

pub(super) fn pg_policy_fields() -> Vec<ResultField> {
    vec![
        oid_field("oid"),
        name_field("polname"),
        oid_field("polrelid"),
        internal_char_field("polcmd"),
        bool_field("polpermissive"),
        ResultField {
            name: "polroles".to_owned(),
            data_type: DataType::Array(Box::new(DataType::Int)),
            text_type_modifier: Some(TextTypeModifier::Oid),
            nullable: true,
        },
        nullable_text_field("polqual"),
        nullable_text_field("polwithcheck"),
    ]
}

// ---------------------------------------------------------------
// pg_catalog.pg_publication
// ---------------------------------------------------------------

pub(super) fn pg_publication_fields() -> Vec<ResultField> {
    vec![
        oid_field("oid"),
        name_field("pubname"),
        oid_field("pubowner"),
        bool_field("puballtables"),
        bool_field("pubinsert"),
        bool_field("pubupdate"),
        bool_field("pubdelete"),
        bool_field("pubtruncate"),
        bool_field("pubviaroot"),
    ]
}

pub(super) fn build_pg_publication_plan() -> DbResult<LogicalPlan> {
    let rows = aiondb_eval::with_current_session_context(|context| {
        let mut rows: Vec<Vec<TypedExpr>> = Vec::new();
        for ((kind, name), (owner, _, _, options_joined, _, _)) in context.compat_misc_attrs.iter()
        {
            if kind != "CREATE PUBLICATION" {
                continue;
            }
            let oid = synth_oid_from_name(name);
            let puballtables = options_joined
                .split(',')
                .map(str::trim)
                .any(|pair| pair == "for_all_tables=true");
            let publish = options_joined
                .split(',')
                .map(str::trim)
                .find_map(|pair| pair.strip_prefix("publish=").map(str::to_owned))
                .unwrap_or_else(|| "insert, update, delete, truncate".to_owned());
            let publish_lc = publish.to_ascii_lowercase();
            let pubinsert = publish_lc.contains("insert");
            let pubupdate = publish_lc.contains("update");
            let pubdelete = publish_lc.contains("delete");
            let pubtruncate = publish_lc.contains("truncate");
            let pubviaroot = options_joined
                .split(',')
                .map(str::trim)
                .any(|pair| pair == "publish_via_partition_root=true");
            let owner_oid = if owner.is_empty() {
                0
            } else {
                aiondb_core::compat_role_oid(owner)
            };
            rows.push(vec![
                int_literal(oid),
                text_literal(name),
                int_literal(owner_oid),
                bool_literal(puballtables),
                bool_literal(pubinsert),
                bool_literal(pubupdate),
                bool_literal(pubdelete),
                bool_literal(pubtruncate),
                bool_literal(pubviaroot),
            ]);
        }
        rows
    });
    Ok(project_values(pg_publication_fields(), rows))
}

pub(super) fn pg_publication_namespace_fields() -> Vec<ResultField> {
    vec![oid_field("oid"), oid_field("pnpubid"), oid_field("pnnspid")]
}

pub(super) fn build_pg_publication_namespace_plan() -> DbResult<LogicalPlan> {
    Ok(project_values(
        pg_publication_namespace_fields(),
        Vec::new(),
    ))
}

pub(super) fn pg_publication_rel_fields() -> Vec<ResultField> {
    vec![
        oid_field("oid"),
        oid_field("prpubid"),
        oid_field("prrelid"),
        nullable_text_field("prqual"),
        ResultField {
            name: "prattrs".to_owned(),
            data_type: DataType::Array(Box::new(DataType::Int)),
            text_type_modifier: Some(TextTypeModifier::Int2Vector),
            nullable: true,
        },
    ]
}

pub(super) fn build_pg_publication_rel_plan() -> DbResult<LogicalPlan> {
    Ok(project_values(pg_publication_rel_fields(), Vec::new()))
}

// ---------------------------------------------------------------
// pg_catalog.pg_subscription
// ---------------------------------------------------------------

pub(super) fn pg_subscription_fields() -> Vec<ResultField> {
    vec![
        oid_field("oid"),
        oid_field("subdbid"),
        oid_field("subskiplsn"),
        name_field("subname"),
        oid_field("subowner"),
        bool_field("subenabled"),
        bool_field("subbinary"),
        internal_char_field("substream"),
        internal_char_field("subtwophasestate"),
        bool_field("subdisableonerr"),
        nullable_text_field("suborigin"),
        bool_field("subpasswordrequired"),
        bool_field("subrunasowner"),
        nullable_text_field("subconninfo"),
        nullable_text_field("subslotname"),
        nullable_text_field("subsynccommit"),
        ResultField {
            name: "subpublications".to_owned(),
            data_type: DataType::Array(Box::new(DataType::Text)),
            text_type_modifier: None,
            nullable: true,
        },
    ]
}

pub(super) fn build_pg_subscription_plan() -> DbResult<LogicalPlan> {
    let rows = aiondb_eval::with_current_session_context(|context| {
        let mut rows: Vec<Vec<TypedExpr>> = Vec::new();
        for ((kind, name), (owner, _, state, options_joined, _, _)) in
            context.compat_misc_attrs.iter()
        {
            if kind != "CREATE SUBSCRIPTION" {
                continue;
            }
            let oid = synth_oid_from_name(name);
            let connection = options_joined
                .split(',')
                .map(str::trim)
                .find_map(|pair| pair.strip_prefix("connection=").map(str::to_owned));
            let pubs_csv = options_joined
                .split(',')
                .map(str::trim)
                .find_map(|pair| pair.strip_prefix("publication=").map(str::to_owned))
                .unwrap_or_default();
            let pub_elements: Vec<aiondb_core::Value> = pubs_csv
                .split(',')
                .map(str::trim)
                .filter(|p| !p.is_empty())
                .map(|p| aiondb_core::Value::Text(p.to_owned()))
                .collect();
            let enabled = !matches!(state.as_str(), "disabled");
            let binary = options_joined
                .split(',')
                .map(str::trim)
                .any(|pair| pair == "binary=true");
            let streaming = options_joined
                .split(',')
                .map(str::trim)
                .any(|pair| pair == "streaming=true" || pair == "streaming=on");
            let stream_mode = if streaming { "t" } else { "f" };
            let two_phase = if options_joined
                .split(',')
                .map(str::trim)
                .any(|pair| pair == "two_phase=true")
            {
                "e"
            } else {
                "d"
            };
            let disable_on_err = options_joined
                .split(',')
                .map(str::trim)
                .any(|pair| pair == "disable_on_error=true");
            let slot_name = options_joined
                .split(',')
                .map(str::trim)
                .find_map(|pair| pair.strip_prefix("slot_name=").map(str::to_owned));
            let sync_commit = options_joined
                .split(',')
                .map(str::trim)
                .find_map(|pair| pair.strip_prefix("synchronous_commit=").map(str::to_owned));
            let origin = options_joined
                .split(',')
                .map(str::trim)
                .find_map(|pair| pair.strip_prefix("origin=").map(str::to_owned));
            let password_required = !options_joined
                .split(',')
                .map(str::trim)
                .any(|pair| pair == "password_required=false");
            let run_as_owner = options_joined
                .split(',')
                .map(str::trim)
                .any(|pair| pair == "run_as_owner=true");
            let owner_oid = if owner.is_empty() {
                0
            } else {
                aiondb_core::compat_role_oid(owner)
            };
            rows.push(vec![
                int_literal(oid),
                int_literal(0), // subdbid
                int_literal(0), // subskiplsn
                text_literal(name),
                int_literal(owner_oid),
                bool_literal(enabled),
                bool_literal(binary),
                text_literal(stream_mode),
                text_literal(two_phase),
                bool_literal(disable_on_err),
                origin
                    .map(|value| text_literal(&value))
                    .unwrap_or_else(|| null_literal(DataType::Text)),
                bool_literal(password_required),
                bool_literal(run_as_owner),
                connection
                    .map(|c| text_literal(&c))
                    .unwrap_or_else(|| null_literal(DataType::Text)),
                slot_name
                    .map(|s| text_literal(&s))
                    .unwrap_or_else(|| null_literal(DataType::Text)),
                sync_commit
                    .map(|s| text_literal(&s))
                    .unwrap_or_else(|| null_literal(DataType::Text)),
                if pub_elements.is_empty() {
                    null_literal(DataType::Array(Box::new(DataType::Text)))
                } else {
                    typed_array_literal(pub_elements, DataType::Text)
                },
            ]);
        }
        rows
    });
    Ok(project_values(pg_subscription_fields(), rows))
}

// ---------------------------------------------------------------
// pg_catalog.pg_statistic_ext
// ---------------------------------------------------------------

pub(super) fn pg_statistic_ext_fields() -> Vec<ResultField> {
    vec![
        oid_field("oid"),
        oid_field("stxrelid"),
        name_field("stxname"),
        oid_field("stxnamespace"),
        oid_field("stxowner"),
        ResultField {
            name: "stxkeys".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: Some(TextTypeModifier::Int2Vector),
            nullable: false,
        },
        ResultField {
            name: "stxkind".to_owned(),
            data_type: DataType::Array(Box::new(DataType::Text)),
            text_type_modifier: None,
            nullable: false,
        },
        int_field("stxstattarget"),
        ResultField {
            name: "stxexprs".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: true,
        },
    ]
}

pub(super) fn pg_statistic_ext_data_fields() -> Vec<ResultField> {
    vec![
        oid_field("stxoid"),
        bool_field("stxdinherit"),
        ResultField {
            name: "stxdndistinct".to_owned(),
            data_type: DataType::Blob,
            text_type_modifier: None,
            nullable: true,
        },
        ResultField {
            name: "stxddependencies".to_owned(),
            data_type: DataType::Blob,
            text_type_modifier: None,
            nullable: true,
        },
        ResultField {
            name: "stxdmcv".to_owned(),
            data_type: DataType::Blob,
            text_type_modifier: None,
            nullable: true,
        },
    ]
}

pub(super) fn pg_stats_ext_fields() -> Vec<ResultField> {
    vec![
        text_field("schemaname"),
        text_field("tablename"),
        text_field("statistics_schemaname"),
        text_field("statistics_name"),
        text_field("statistics_owner"),
        ResultField {
            name: "attnames".to_owned(),
            data_type: DataType::Array(Box::new(DataType::Text)),
            text_type_modifier: None,
            nullable: true,
        },
        ResultField {
            name: "exprs".to_owned(),
            data_type: DataType::Array(Box::new(DataType::Text)),
            text_type_modifier: None,
            nullable: true,
        },
        ResultField {
            name: "kinds".to_owned(),
            data_type: DataType::Array(Box::new(DataType::Text)),
            text_type_modifier: None,
            nullable: true,
        },
        nullable_text_field("n_distinct"),
        nullable_text_field("dependencies"),
        nullable_text_field("most_common_vals"),
        nullable_text_field("most_common_val_nulls"),
        nullable_text_field("most_common_freqs"),
        nullable_text_field("most_common_base_freqs"),
    ]
}

pub(super) fn pg_stats_ext_exprs_fields() -> Vec<ResultField> {
    vec![
        text_field("schemaname"),
        text_field("tablename"),
        text_field("statistics_schemaname"),
        text_field("statistics_name"),
        text_field("statistics_owner"),
        text_field("expr"),
        ResultField {
            name: "kinds".to_owned(),
            data_type: DataType::Array(Box::new(DataType::Text)),
            text_type_modifier: None,
            nullable: true,
        },
        nullable_text_field("n_distinct"),
        nullable_text_field("dependencies"),
        nullable_text_field("most_common_vals"),
        nullable_text_field("most_common_val_nulls"),
        nullable_text_field("most_common_freqs"),
        nullable_text_field("most_common_base_freqs"),
    ]
}

fn stats_option_value(options_joined: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}=");
    let mut parts = Vec::new();
    let mut collecting = false;
    for pair in options_joined.split(',').map(str::trim) {
        if let Some(value) = pair.strip_prefix(&prefix) {
            parts.push(value.to_owned());
            collecting = true;
        } else if collecting && !pair.contains('=') {
            parts.push(pair.to_owned());
        } else if collecting {
            break;
        }
    }
    (!parts.is_empty()).then(|| parts.join(", "))
}

fn split_statistics_key_items(raw: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut in_string = false;
    let mut item_start = 0usize;
    let bytes = raw.as_bytes();
    let mut idx = 0usize;
    while idx < bytes.len() {
        let b = bytes[idx];
        if in_string {
            if b == b'\'' {
                if idx + 1 < bytes.len() && bytes[idx + 1] == b'\'' {
                    idx += 2;
                    continue;
                }
                in_string = false;
            }
            idx += 1;
            continue;
        }
        match b {
            b'\'' => in_string = true,
            b'(' => depth += 1,
            b')' => {
                if depth > 0 {
                    depth -= 1;
                }
            }
            b',' if depth == 0 => {
                if let Some(segment) = raw.get(item_start..idx) {
                    let trimmed = segment.trim();
                    if !trimmed.is_empty() {
                        out.push(trimmed.to_owned());
                    }
                }
                item_start = idx + 1;
            }
            _ => {}
        }
        idx += 1;
    }
    if let Some(segment) = raw.get(item_start..) {
        let trimmed = segment.trim();
        if !trimmed.is_empty() {
            out.push(trimmed.to_owned());
        }
    }
    out
}

pub(super) fn build_pg_statistic_ext_plan() -> DbResult<LogicalPlan> {
    let rows = aiondb_eval::with_current_session_context(|context| {
        let mut rows: Vec<Vec<TypedExpr>> = Vec::new();
        for ((kind, name), (owner, schema, _, options_joined, _, _)) in
            context.compat_misc_attrs.iter()
        {
            if kind != "CREATE STATISTICS" {
                continue;
            }
            let (schema_from_name, bare_name) = name
                .split_once('.')
                .map(|(schema_part, object_part)| (Some(schema_part), object_part))
                .unwrap_or((None, name.as_str()));
            let oid = synth_oid_from_name(name);
            let owner_oid = if owner.is_empty() {
                0
            } else {
                aiondb_core::compat_role_oid(owner)
            };
            let table = stats_option_value(options_joined, "table").unwrap_or_default();
            let stxrelid = stats_option_value(options_joined, "table_id")
                .and_then(|value| value.parse::<u64>().ok())
                .map(|table_id| {
                    i32::try_from(table_id)
                        .unwrap_or(i32::MAX)
                        .saturating_add(16384)
                })
                .unwrap_or_else(|| {
                    if table.is_empty() {
                        0
                    } else {
                        synth_oid_from_name(&table.to_ascii_lowercase())
                    }
                });
            let columns = stats_option_value(options_joined, "columns").unwrap_or_default();
            let definition_schema = schema_from_name
                .or_else(|| (!schema.is_empty()).then_some(schema))
                .unwrap_or("public");
            let definition_kinds = stats_option_value(options_joined, "kinds")
                .map(|kinds| format!(" ({kinds})"))
                .unwrap_or_default();
            aiondb_eval::register_pg_statistics_objdef(
                oid,
                format!(
                    "CREATE STATISTICS {definition_schema}.{bare_name}{definition_kinds} ON {columns} FROM {table}"
                ),
            );
            let key_items = split_statistics_key_items(&columns);
            let stxkeys_text = (1..=key_items.len())
                .map(|idx| idx.to_string())
                .collect::<Vec<_>>()
                .join(" ");
            let kinds_csv = stats_option_value(options_joined, "kinds")
                .unwrap_or_else(|| "ndistinct, dependencies, mcv".to_owned());
            let kind_chars: Vec<aiondb_core::Value> = kinds_csv
                .split(',')
                .map(str::trim)
                .filter(|k| !k.is_empty())
                .map(|k| {
                    let code = match k.to_ascii_lowercase().as_str() {
                        "ndistinct" => "d",
                        "dependencies" => "f",
                        "mcv" => "m",
                        "e" | "expressions" => "e",
                        _ => "d",
                    };
                    aiondb_core::Value::Text(code.to_owned())
                })
                .collect();
            rows.push(vec![
                int_literal(oid),
                int_literal(stxrelid),
                text_literal(bare_name),
                int_literal(
                    schema_from_name
                        .map(synth_oid_from_name)
                        .or_else(|| (!schema.is_empty()).then(|| synth_oid_from_name(schema)))
                        .unwrap_or(PG_CATALOG_NAMESPACE_OID),
                ),
                int_literal(owner_oid),
                text_literal(&stxkeys_text),
                if kind_chars.is_empty() {
                    typed_array_literal(
                        vec![aiondb_core::Value::Text("d".to_owned())],
                        DataType::Text,
                    )
                } else {
                    typed_array_literal(kind_chars, DataType::Text)
                },
                int_literal(
                    stats_option_value(options_joined, "stattarget")
                        .and_then(|value| value.parse::<i32>().ok())
                        .unwrap_or(-1),
                ),
                null_literal(DataType::Text),
            ]);
        }
        rows
    });
    Ok(project_values(pg_statistic_ext_fields(), rows))
}

pub(super) fn build_pg_statistic_ext_data_plan() -> DbResult<LogicalPlan> {
    let rows = aiondb_eval::with_current_session_context(|context| {
        let mut rows: Vec<Vec<TypedExpr>> = Vec::new();
        for ((kind, name), (_, _, _, options_joined, _, _)) in context.compat_misc_attrs.iter() {
            if kind != "CREATE STATISTICS" {
                continue;
            }
            let oid = synth_oid_from_name(name);
            rows.push(vec![
                int_literal(oid),
                stats_option_value(options_joined, "stxdinherit")
                    .map(|value| {
                        let truthy =
                            !matches!(value.to_ascii_lowercase().as_str(), "f" | "false" | "0");
                        bool_literal(truthy)
                    })
                    .unwrap_or_else(|| null_literal(DataType::Boolean)),
                stats_option_value(options_joined, "stxdndistinct")
                    .map(|payload| {
                        TypedExpr::literal(Value::Blob(payload.into_bytes()), DataType::Blob, false)
                    })
                    .unwrap_or_else(|| null_literal(DataType::Blob)),
                stats_option_value(options_joined, "stxddependencies")
                    .map(|payload| {
                        TypedExpr::literal(Value::Blob(payload.into_bytes()), DataType::Blob, false)
                    })
                    .unwrap_or_else(|| null_literal(DataType::Blob)),
                stats_option_value(options_joined, "stxdmcv")
                    .map(|payload| {
                        TypedExpr::literal(Value::Blob(payload.into_bytes()), DataType::Blob, false)
                    })
                    .unwrap_or_else(|| null_literal(DataType::Blob)),
            ]);
        }
        rows
    });
    Ok(project_values(pg_statistic_ext_data_fields(), rows))
}

pub(super) fn build_pg_stats_ext_plan() -> DbResult<LogicalPlan> {
    let rows = aiondb_eval::with_current_session_context(|context| {
        let mut rows: Vec<Vec<TypedExpr>> = Vec::new();
        for ((kind, name), (owner, schema, _, options_joined, _, _)) in
            context.compat_misc_attrs.iter()
        {
            if kind != "CREATE STATISTICS" {
                continue;
            }
            let bare_name = name
                .split_once('.')
                .map(|(_, object_part)| object_part)
                .unwrap_or(name.as_str());
            let table = stats_option_value(options_joined, "table").unwrap_or_default();
            let columns = stats_option_value(options_joined, "columns").unwrap_or_default();
            let kinds = stats_option_value(options_joined, "kinds")
                .unwrap_or_else(|| "ndistinct, dependencies, mcv".to_owned());
            let key_items = split_statistics_key_items(&columns);
            let attnames: Vec<Value> = key_items
                .iter()
                .map(|item| item.trim())
                .filter(|item| !item.is_empty() && !item.starts_with('('))
                .map(|item| Value::Text(item.to_owned()))
                .collect();
            let exprs: Vec<Value> = key_items
                .iter()
                .map(|item| item.trim())
                .filter(|item| item.starts_with('('))
                .map(|item| Value::Text(item.to_owned()))
                .collect();
            let kind_values: Vec<Value> = kinds
                .split(',')
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(|item| Value::Text(item.to_ascii_lowercase()))
                .collect();
            rows.push(vec![
                text_literal(if schema.is_empty() { "public" } else { schema }),
                text_literal(&table),
                text_literal(if schema.is_empty() { "public" } else { schema }),
                text_literal(bare_name),
                text_literal(owner),
                if attnames.is_empty() {
                    null_literal(DataType::Array(Box::new(DataType::Text)))
                } else {
                    typed_array_literal(attnames, DataType::Text)
                },
                if exprs.is_empty() {
                    null_literal(DataType::Array(Box::new(DataType::Text)))
                } else {
                    typed_array_literal(exprs, DataType::Text)
                },
                if kind_values.is_empty() {
                    null_literal(DataType::Array(Box::new(DataType::Text)))
                } else {
                    typed_array_literal(kind_values, DataType::Text)
                },
                stats_option_value(options_joined, "n_distinct")
                    .map(|value| text_literal(&value))
                    .unwrap_or_else(|| null_literal(DataType::Text)),
                stats_option_value(options_joined, "dependencies")
                    .map(|value| text_literal(&value))
                    .unwrap_or_else(|| null_literal(DataType::Text)),
                stats_option_value(options_joined, "most_common_vals")
                    .map(|value| text_literal(&value))
                    .unwrap_or_else(|| null_literal(DataType::Text)),
                stats_option_value(options_joined, "most_common_val_nulls")
                    .map(|value| text_literal(&value))
                    .unwrap_or_else(|| null_literal(DataType::Text)),
                stats_option_value(options_joined, "most_common_freqs")
                    .map(|value| text_literal(&value))
                    .unwrap_or_else(|| null_literal(DataType::Text)),
                stats_option_value(options_joined, "most_common_base_freqs")
                    .map(|value| text_literal(&value))
                    .unwrap_or_else(|| null_literal(DataType::Text)),
            ]);
        }
        rows
    });
    Ok(project_values(pg_stats_ext_fields(), rows))
}

pub(super) fn build_pg_stats_ext_exprs_plan() -> DbResult<LogicalPlan> {
    let rows = aiondb_eval::with_current_session_context(|context| {
        let mut rows: Vec<Vec<TypedExpr>> = Vec::new();
        for ((kind, name), (owner, schema, _, options_joined, _, _)) in
            context.compat_misc_attrs.iter()
        {
            if kind != "CREATE STATISTICS" {
                continue;
            }
            let bare_name = name
                .split_once('.')
                .map(|(_, object_part)| object_part)
                .unwrap_or(name.as_str());
            let table = stats_option_value(options_joined, "table").unwrap_or_default();
            let columns = stats_option_value(options_joined, "columns").unwrap_or_default();
            let kinds = stats_option_value(options_joined, "kinds")
                .unwrap_or_else(|| "ndistinct, dependencies, mcv".to_owned());
            let kind_values: Vec<Value> = kinds
                .split(',')
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(|item| Value::Text(item.to_ascii_lowercase()))
                .collect();
            for expr in split_statistics_key_items(&columns)
                .into_iter()
                .map(|item| item.trim().to_owned())
                .filter(|item| item.starts_with('('))
            {
                rows.push(vec![
                    text_literal(if schema.is_empty() { "public" } else { schema }),
                    text_literal(&table),
                    text_literal(if schema.is_empty() { "public" } else { schema }),
                    text_literal(bare_name),
                    text_literal(owner),
                    text_literal(&expr),
                    if kind_values.is_empty() {
                        null_literal(DataType::Array(Box::new(DataType::Text)))
                    } else {
                        typed_array_literal(kind_values.clone(), DataType::Text)
                    },
                    stats_option_value(options_joined, "n_distinct")
                        .map(|value| text_literal(&value))
                        .unwrap_or_else(|| null_literal(DataType::Text)),
                    stats_option_value(options_joined, "dependencies")
                        .map(|value| text_literal(&value))
                        .unwrap_or_else(|| null_literal(DataType::Text)),
                    stats_option_value(options_joined, "most_common_vals")
                        .map(|value| text_literal(&value))
                        .unwrap_or_else(|| null_literal(DataType::Text)),
                    stats_option_value(options_joined, "most_common_val_nulls")
                        .map(|value| text_literal(&value))
                        .unwrap_or_else(|| null_literal(DataType::Text)),
                    stats_option_value(options_joined, "most_common_freqs")
                        .map(|value| text_literal(&value))
                        .unwrap_or_else(|| null_literal(DataType::Text)),
                    stats_option_value(options_joined, "most_common_base_freqs")
                        .map(|value| text_literal(&value))
                        .unwrap_or_else(|| null_literal(DataType::Text)),
                ]);
            }
        }
        rows
    });
    Ok(project_values(pg_stats_ext_exprs_fields(), rows))
}

pub(super) fn build_pg_policy_plan() -> DbResult<LogicalPlan> {
    let rows = aiondb_eval::with_current_session_context(|context| {
        let mut rows: Vec<Vec<TypedExpr>> = Vec::new();
        for ((kind, name), (_, _, _, options_joined, _, _)) in context.compat_misc_attrs.iter() {
            if kind != "CREATE POLICY" {
                continue;
            }
            let oid = synth_oid_from_name(name);
            let table = options_joined
                .split(',')
                .map(str::trim)
                .find_map(|pair| pair.strip_prefix("table=").map(str::to_owned))
                .unwrap_or_default();
            let polrelid = if table.is_empty() {
                0
            } else {
                synth_oid_from_name(&table.to_ascii_lowercase())
            };
            let cmd = options_joined
                .split(',')
                .map(str::trim)
                .find_map(|pair| pair.strip_prefix("for=").map(str::to_owned))
                .unwrap_or_else(|| "all".to_owned());
            let polcmd = match cmd.to_ascii_lowercase().as_str() {
                "select" => "r",
                "insert" => "a",
                "update" => "w",
                "delete" => "d",
                _ => "*",
            };
            let permissive = options_joined
                .split(',')
                .map(str::trim)
                .find_map(|pair| pair.strip_prefix("permissive=").map(str::to_owned))
                .map(|value| !value.eq_ignore_ascii_case("restrictive"))
                .unwrap_or(true);
            let roles = options_joined
                .split(',')
                .map(str::trim)
                .find_map(|pair| pair.strip_prefix("to=").map(str::to_owned));
            let using_expr = options_joined
                .split(',')
                .map(str::trim)
                .find_map(|pair| pair.strip_prefix("using=").map(str::to_owned));
            let check_expr = options_joined
                .split(',')
                .map(str::trim)
                .find_map(|pair| pair.strip_prefix("with_check=").map(str::to_owned));
            rows.push(vec![
                int_literal(oid),
                text_literal(name),
                int_literal(polrelid),
                text_literal(polcmd),
                bool_literal(permissive),
                roles
                    .map(|role_list| {
                        let role_oids = role_list
                            .split(',')
                            .map(str::trim)
                            .filter(|role| !role.is_empty())
                            .map(|role| {
                                if role.eq_ignore_ascii_case("public") {
                                    Value::Int(0)
                                } else {
                                    Value::Int(aiondb_core::compat_role_oid(role))
                                }
                            })
                            .collect::<Vec<_>>();
                        typed_array_literal(role_oids, DataType::Int)
                    })
                    .unwrap_or_else(|| typed_array_literal(vec![Value::Int(0)], DataType::Int)),
                using_expr
                    .map(|r| text_literal(&r))
                    .unwrap_or_else(|| null_literal(DataType::Text)),
                check_expr
                    .map(|r| text_literal(&r))
                    .unwrap_or_else(|| null_literal(DataType::Text)),
            ]);
        }
        rows
    });
    Ok(project_values(pg_policy_fields(), rows))
}

// ---------------------------------------------------------------
// pg_catalog.pg_sequence
// ---------------------------------------------------------------

pub(super) fn pg_sequence_fields() -> Vec<ResultField> {
    vec![
        oid_field("seqrelid"),
        oid_field("seqtypid"),
        bigint_field("seqstart"),
        bigint_field("seqincrement"),
        bigint_field("seqmax"),
        bigint_field("seqmin"),
        bigint_field("seqcache"),
        bool_field("seqcycle"),
    ]
}

pub(super) fn build_pg_sequence_plan(
    catalog: &Arc<dyn CatalogReader>,
    txn_id: TxnId,
    default_schema: Option<&str>,
) -> DbResult<LogicalPlan> {
    let rows = list_user_sequences(catalog, txn_id, default_schema)?
        .into_iter()
        .map(|sequence| {
            vec![
                int_literal(sequence_id_to_oid(&sequence)),
                int_literal(
                    sequence
                        .data_type
                        .pg_oid()
                        .map_or(0, aiondb_core::convert::u32_to_i32_saturating),
                ),
                TypedExpr::literal(Value::BigInt(sequence.start_value), DataType::BigInt, false),
                TypedExpr::literal(
                    Value::BigInt(sequence.increment_by),
                    DataType::BigInt,
                    false,
                ),
                TypedExpr::literal(Value::BigInt(sequence.max_value), DataType::BigInt, false),
                TypedExpr::literal(Value::BigInt(sequence.min_value), DataType::BigInt, false),
                TypedExpr::literal(
                    Value::BigInt(i64::try_from(sequence.cache_size).unwrap_or(i64::MAX)),
                    DataType::BigInt,
                    false,
                ),
                bool_literal(sequence.cycle),
            ]
        })
        .collect();
    Ok(project_values(pg_sequence_fields(), rows))
}

// ---------------------------------------------------------------
// pg_catalog.pg_sequences (the user-facing view)
// ---------------------------------------------------------------

pub(super) fn pg_sequences_fields() -> Vec<ResultField> {
    vec![
        text_field("schemaname"),
        text_field("sequencename"),
        text_field("sequenceowner"),
        ResultField {
            name: "data_type".to_owned(),
            data_type: DataType::Int,
            text_type_modifier: Some(TextTypeModifier::RegType),
            nullable: true,
        },
        bigint_field("start_value"),
        bigint_field("min_value"),
        bigint_field("max_value"),
        bigint_field("increment_by"),
        bool_field("cycle"),
        bigint_field("cache_size"),
        ResultField {
            name: "last_value".to_owned(),
            data_type: DataType::BigInt,
            text_type_modifier: None,
            nullable: true,
        },
    ]
}

pub(super) fn build_pg_sequences_plan(
    catalog: &Arc<dyn CatalogReader>,
    txn_id: TxnId,
    default_schema: Option<&str>,
) -> DbResult<LogicalPlan> {
    let owner_name = aiondb_core::COMPAT_BOOTSTRAP_ROLE_NAME.to_owned();
    let rows = list_user_sequences(catalog, txn_id, default_schema)?
        .into_iter()
        .map(|sequence| {
            let schema_name = sequence.name.schema_name().unwrap_or("public").to_owned();
            vec![
                text_literal(&schema_name),
                text_literal(sequence.name.object_name()),
                text_literal(&owner_name),
                int_literal(
                    sequence
                        .data_type
                        .pg_oid()
                        .map_or(0, aiondb_core::convert::u32_to_i32_saturating),
                ),
                TypedExpr::literal(Value::BigInt(sequence.start_value), DataType::BigInt, false),
                TypedExpr::literal(Value::BigInt(sequence.min_value), DataType::BigInt, false),
                TypedExpr::literal(Value::BigInt(sequence.max_value), DataType::BigInt, false),
                TypedExpr::literal(
                    Value::BigInt(sequence.increment_by),
                    DataType::BigInt,
                    false,
                ),
                bool_literal(sequence.cycle),
                TypedExpr::literal(
                    Value::BigInt(i64::try_from(sequence.cache_size).unwrap_or(i64::MAX)),
                    DataType::BigInt,
                    false,
                ),
                null_literal(DataType::BigInt),
            ]
        })
        .collect();
    Ok(project_values(pg_sequences_fields(), rows))
}

// ---------------------------------------------------------------
// pg_catalog.pg_statistic
// ---------------------------------------------------------------

pub(super) fn pg_statistic_fields() -> Vec<ResultField> {
    vec![
        oid_field("starelid"),
        int_field("staattnum"),
        bool_field("stainherit"),
        double_field("stanullfrac"),
        int_field("stawidth"),
        double_field("stadistinct"),
        int_field("stakind1"),
        int_field("stakind2"),
        int_field("stakind3"),
        int_field("stakind4"),
        int_field("stakind5"),
        oid_field("staop1"),
        oid_field("staop2"),
        oid_field("staop3"),
        oid_field("staop4"),
        oid_field("staop5"),
        oid_field("stacoll1"),
        oid_field("stacoll2"),
        oid_field("stacoll3"),
        oid_field("stacoll4"),
        oid_field("stacoll5"),
        ResultField {
            name: "stanumbers1".to_owned(),
            data_type: DataType::Array(Box::new(DataType::Real)),
            text_type_modifier: None,
            nullable: true,
        },
        ResultField {
            name: "stanumbers2".to_owned(),
            data_type: DataType::Array(Box::new(DataType::Real)),
            text_type_modifier: None,
            nullable: true,
        },
        ResultField {
            name: "stanumbers3".to_owned(),
            data_type: DataType::Array(Box::new(DataType::Real)),
            text_type_modifier: None,
            nullable: true,
        },
        ResultField {
            name: "stanumbers4".to_owned(),
            data_type: DataType::Array(Box::new(DataType::Real)),
            text_type_modifier: None,
            nullable: true,
        },
        ResultField {
            name: "stanumbers5".to_owned(),
            data_type: DataType::Array(Box::new(DataType::Real)),
            text_type_modifier: None,
            nullable: true,
        },
        nullable_text_field("stavalues1"),
        nullable_text_field("stavalues2"),
        nullable_text_field("stavalues3"),
        nullable_text_field("stavalues4"),
        nullable_text_field("stavalues5"),
    ]
}

pub(super) fn build_pg_statistic_plan() -> DbResult<LogicalPlan> {
    let fields = pg_statistic_fields();
    // Keep pg_statistic effectively non-materialized while still forcing
    // planner/executor paths to type-check expressions that reference
    // polymorphic stavaluesN columns.
    let null_row = fields
        .iter()
        .map(|field| TypedExpr::literal(Value::Null, field.data_type.clone(), true))
        .collect();
    Ok(project_values(fields, vec![null_row]))
}
