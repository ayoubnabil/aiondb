use aiondb_core::DbResult;
use aiondb_plan::{LogicalPlan, ResultField, TypedExpr};

use super::*;

// ---------------------------------------------------------------
// pg_catalog.pg_operator
// ---------------------------------------------------------------

pub(super) fn pg_operator_fields() -> Vec<ResultField> {
    vec![
        oid_field("oid"),
        name_field("oprname"),
        oid_field("oprnamespace"),
        oid_field("oprowner"),
        internal_char_field("oprkind"),
        bool_field("oprcanmerge"),
        bool_field("oprcanhash"),
        oid_field("oprleft"),
        oid_field("oprright"),
        oid_field("oprresult"),
        oid_field("oprcom"),
        oid_field("oprnegate"),
        oid_field("oprcode"),
    ]
}

/// PostgreSQL operator catalog entry.
struct PgOperatorEntry {
    oid: i32,
    name: &'static str,
    kind: &'static str, // "b" = binary, "l" = left-unary (prefix)
    can_merge: bool,
    can_hash: bool,
    left: i32, // 0 for prefix operators
    right: i32,
    result: i32,
    com: i32,    // commutator OID (0 = none)
    negate: i32, // negator OID (0 = none)
    code: i32,   // oprcode -> pg_proc OID
}

// OID constants for types used in operator definitions
const OID_BOOL: i32 = 16;
const OID_INT4: i32 = 23;
const OID_INT8: i32 = 20;
const OID_FLOAT4: i32 = 700;
const OID_FLOAT8: i32 = 701;
const OID_TEXT: i32 = 25;
const OID_NUMERIC: i32 = 1700;
const OID_DATE: i32 = 1082;
const OID_TIMESTAMP: i32 = 1114;
const OID_TIMESTAMPTZ: i32 = 1184;
const OID_INTERVAL: i32 = 1186;
const OID_JSONB: i32 = 3802;

// Well-known PostgreSQL operator OIDs covering the core operators that
// AionDB actually implements. These OIDs match the real PostgreSQL system
// catalog values for maximum compatibility.
include!("pg_operator_part1.inc.rs");
include!("pg_operator_part2.inc.rs");

struct PgvectorOperatorSpec {
    type_oid: i32,
    name: &'static str,
    proc_name: &'static str,
}

const PGVECTOR_OPERATOR_SPECS: &[PgvectorOperatorSpec] = &[
    PgvectorOperatorSpec {
        type_oid: COMPAT_PGVECTOR_VECTOR_OID,
        name: "<->",
        proc_name: "l2_distance",
    },
    PgvectorOperatorSpec {
        type_oid: COMPAT_PGVECTOR_VECTOR_OID,
        name: "<#>",
        proc_name: "negative_inner_product",
    },
    PgvectorOperatorSpec {
        type_oid: COMPAT_PGVECTOR_VECTOR_OID,
        name: "<=>",
        proc_name: "cosine_distance",
    },
    PgvectorOperatorSpec {
        type_oid: COMPAT_PGVECTOR_VECTOR_OID,
        name: "<+>",
        proc_name: "l1_distance",
    },
    PgvectorOperatorSpec {
        type_oid: COMPAT_PGVECTOR_HALFVEC_OID,
        name: "<->",
        proc_name: "l2_distance",
    },
    PgvectorOperatorSpec {
        type_oid: COMPAT_PGVECTOR_HALFVEC_OID,
        name: "<#>",
        proc_name: "negative_inner_product",
    },
    PgvectorOperatorSpec {
        type_oid: COMPAT_PGVECTOR_HALFVEC_OID,
        name: "<=>",
        proc_name: "cosine_distance",
    },
    PgvectorOperatorSpec {
        type_oid: COMPAT_PGVECTOR_HALFVEC_OID,
        name: "<+>",
        proc_name: "l1_distance",
    },
    PgvectorOperatorSpec {
        type_oid: COMPAT_PGVECTOR_SPARSEVEC_OID,
        name: "<->",
        proc_name: "l2_distance",
    },
    PgvectorOperatorSpec {
        type_oid: COMPAT_PGVECTOR_SPARSEVEC_OID,
        name: "<#>",
        proc_name: "negative_inner_product",
    },
    PgvectorOperatorSpec {
        type_oid: COMPAT_PGVECTOR_SPARSEVEC_OID,
        name: "<=>",
        proc_name: "cosine_distance",
    },
    PgvectorOperatorSpec {
        type_oid: COMPAT_PGVECTOR_SPARSEVEC_OID,
        name: "<+>",
        proc_name: "l1_distance",
    },
    PgvectorOperatorSpec {
        type_oid: COMPAT_PG_BIT_OID,
        name: "<~>",
        proc_name: "hamming_distance",
    },
    PgvectorOperatorSpec {
        type_oid: COMPAT_PG_BIT_OID,
        name: "<%>",
        proc_name: "jaccard_distance",
    },
];

pub(super) fn build_pg_operator_plan(owner_oid: i32) -> DbResult<LogicalPlan> {
    let fields = pg_operator_fields();
    let mut rows: Vec<Vec<TypedExpr>> = PG_OPERATORS_PART_1
        .iter()
        .chain(PG_OPERATORS_PART_2.iter())
        .map(|op| {
            vec![
                int_literal(op.oid),
                text_literal(op.name),
                int_literal(PG_CATALOG_NAMESPACE_OID),
                int_literal(owner_oid),
                text_literal(op.kind),
                bool_literal(op.can_merge),
                bool_literal(op.can_hash),
                int_literal(op.left),
                int_literal(op.right),
                int_literal(op.result),
                int_literal(op.com),
                int_literal(op.negate),
                int_literal(op.code),
            ]
        })
        .collect();
    rows.extend(PGVECTOR_OPERATOR_SPECS.iter().map(|op| {
        let oid = compat_pgvector_operator_oid(op.type_oid, op.name);
        let argtypes = format!("{} {}", op.type_oid, op.type_oid);
        vec![
            int_literal(oid),
            text_literal(op.name),
            int_literal(PG_CATALOG_NAMESPACE_OID),
            int_literal(owner_oid),
            text_literal("b"),
            bool_literal(false),
            bool_literal(false),
            int_literal(op.type_oid),
            int_literal(op.type_oid),
            int_literal(OID_FLOAT8),
            int_literal(oid),
            int_literal(0),
            int_literal(compat_pgvector_function_oid(op.proc_name, &argtypes)),
        ]
    }));
    Ok(project_values(fields, rows))
}
