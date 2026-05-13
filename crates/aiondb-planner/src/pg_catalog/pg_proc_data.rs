use aiondb_core::{DataType, DbResult};
use aiondb_plan::{LogicalPlan, ResultField, TypedExpr};

use super::*;

// ---------------------------------------------------------------
// pg_catalog.pg_proc - populated with AionDB's built-in functions
// ---------------------------------------------------------------

pub(super) fn pg_proc_fields() -> Vec<ResultField> {
    vec![
        oid_field("oid"),
        name_field("proname"),
        oid_field("pronamespace"),
        oid_field("proowner"),
        oid_field("prolang"),
        double_field("procost"),
        double_field("prorows"),
        oid_field("provariadic"),
        ResultField {
            name: "prosupport".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: Some(TextTypeModifier::RegProc),
            nullable: true,
        },
        internal_char_field("prokind"),
        bool_field("prosecdef"),
        bool_field("proleakproof"),
        bool_field("proisstrict"),
        bool_field("proretset"),
        internal_char_field("provolatile"),
        internal_char_field("proparallel"),
        int_field("pronargs"),
        int_field("pronargdefaults"),
        oid_field("prorettype"),
        ResultField {
            name: "proargtypes".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: Some(TextTypeModifier::OidVector),
            nullable: true,
        },
        ResultField {
            name: "proallargtypes".to_owned(),
            data_type: DataType::Array(Box::new(DataType::Int)),
            text_type_modifier: Some(TextTypeModifier::Oid),
            nullable: true,
        },
        ResultField {
            name: "proargmodes".to_owned(),
            data_type: DataType::Array(Box::new(DataType::Text)),
            text_type_modifier: Some(TextTypeModifier::InternalChar),
            nullable: true,
        },
        ResultField {
            name: "proargnames".to_owned(),
            data_type: DataType::Array(Box::new(DataType::Text)),
            text_type_modifier: None,
            nullable: true,
        },
        nullable_text_field("proargdefaults"),
        ResultField {
            name: "protrftypes".to_owned(),
            data_type: DataType::Array(Box::new(DataType::Int)),
            text_type_modifier: Some(TextTypeModifier::Oid),
            nullable: true,
        },
        nullable_text_field("prosrc"),
        nullable_text_field("probin"),
        nullable_text_field("prosqlbody"),
        ResultField {
            name: "proconfig".to_owned(),
            data_type: DataType::Array(Box::new(DataType::Text)),
            text_type_modifier: None,
            nullable: true,
        },
        ResultField {
            name: "proacl".to_owned(),
            data_type: DataType::Array(Box::new(DataType::Text)),
            text_type_modifier: None,
            nullable: true,
        },
    ]
}

/// Internal language OID (like SQL in PG = 14, internal = 12)
const LANG_INTERNAL: i32 = 12;

/// A built-in function entry for pg_proc.
struct ProcEntry {
    oid: i32,
    name: &'static str,
    nargs: i32,
    rettype: i32,
    argtypes: &'static str,
    /// "f" = function, "a" = aggregate, "w" = window
    prokind: &'static str,
    /// Is this set-returning?
    retset: bool,
    /// "i" = immutable, "s" = stable, "v" = volatile
    volatility: &'static str,
}

struct PgvectorProcEntry {
    name: &'static str,
    rettype: i32,
    argtypes: &'static str,
    prokind: &'static str,
}

const PGVECTOR_PROC_ENTRIES: &[PgvectorProcEntry] = &[
    PgvectorProcEntry {
        name: "vector_in",
        rettype: COMPAT_PGVECTOR_VECTOR_OID,
        argtypes: "2275",
        prokind: "f",
    },
    PgvectorProcEntry {
        name: "vector_out",
        rettype: 2275,
        argtypes: "80001",
        prokind: "f",
    },
    PgvectorProcEntry {
        name: "halfvec_in",
        rettype: COMPAT_PGVECTOR_HALFVEC_OID,
        argtypes: "2275",
        prokind: "f",
    },
    PgvectorProcEntry {
        name: "halfvec_out",
        rettype: 2275,
        argtypes: "80003",
        prokind: "f",
    },
    PgvectorProcEntry {
        name: "sparsevec_in",
        rettype: COMPAT_PGVECTOR_SPARSEVEC_OID,
        argtypes: "2275",
        prokind: "f",
    },
    PgvectorProcEntry {
        name: "sparsevec_out",
        rettype: 2275,
        argtypes: "80005",
        prokind: "f",
    },
    PgvectorProcEntry {
        name: "l2_distance",
        rettype: 701,
        argtypes: "80001 80001",
        prokind: "f",
    },
    PgvectorProcEntry {
        name: "cosine_distance",
        rettype: 701,
        argtypes: "80001 80001",
        prokind: "f",
    },
    PgvectorProcEntry {
        name: "inner_product",
        rettype: 701,
        argtypes: "80001 80001",
        prokind: "f",
    },
    PgvectorProcEntry {
        name: "negative_inner_product",
        rettype: 701,
        argtypes: "80001 80001",
        prokind: "f",
    },
    PgvectorProcEntry {
        name: "l1_distance",
        rettype: 701,
        argtypes: "80001 80001",
        prokind: "f",
    },
    PgvectorProcEntry {
        name: "l2_distance",
        rettype: 701,
        argtypes: "80003 80003",
        prokind: "f",
    },
    PgvectorProcEntry {
        name: "cosine_distance",
        rettype: 701,
        argtypes: "80003 80003",
        prokind: "f",
    },
    PgvectorProcEntry {
        name: "inner_product",
        rettype: 701,
        argtypes: "80003 80003",
        prokind: "f",
    },
    PgvectorProcEntry {
        name: "negative_inner_product",
        rettype: 701,
        argtypes: "80003 80003",
        prokind: "f",
    },
    PgvectorProcEntry {
        name: "l1_distance",
        rettype: 701,
        argtypes: "80003 80003",
        prokind: "f",
    },
    PgvectorProcEntry {
        name: "l2_distance",
        rettype: 701,
        argtypes: "80005 80005",
        prokind: "f",
    },
    PgvectorProcEntry {
        name: "cosine_distance",
        rettype: 701,
        argtypes: "80005 80005",
        prokind: "f",
    },
    PgvectorProcEntry {
        name: "inner_product",
        rettype: 701,
        argtypes: "80005 80005",
        prokind: "f",
    },
    PgvectorProcEntry {
        name: "negative_inner_product",
        rettype: 701,
        argtypes: "80005 80005",
        prokind: "f",
    },
    PgvectorProcEntry {
        name: "l1_distance",
        rettype: 701,
        argtypes: "80005 80005",
        prokind: "f",
    },
    PgvectorProcEntry {
        name: "array_to_vector",
        rettype: COMPAT_PGVECTOR_VECTOR_OID,
        argtypes: "1007 23 16",
        prokind: "f",
    },
    PgvectorProcEntry {
        name: "array_to_vector",
        rettype: COMPAT_PGVECTOR_VECTOR_OID,
        argtypes: "1021 23 16",
        prokind: "f",
    },
    PgvectorProcEntry {
        name: "array_to_vector",
        rettype: COMPAT_PGVECTOR_VECTOR_OID,
        argtypes: "1022 23 16",
        prokind: "f",
    },
    PgvectorProcEntry {
        name: "array_to_vector",
        rettype: COMPAT_PGVECTOR_VECTOR_OID,
        argtypes: "1231 23 16",
        prokind: "f",
    },
    PgvectorProcEntry {
        name: "vector_to_float4",
        rettype: 1021,
        argtypes: "80001 23 16",
        prokind: "f",
    },
    PgvectorProcEntry {
        name: "vector_to_halfvec",
        rettype: COMPAT_PGVECTOR_HALFVEC_OID,
        argtypes: "80001 23 16",
        prokind: "f",
    },
    PgvectorProcEntry {
        name: "halfvec_to_vector",
        rettype: COMPAT_PGVECTOR_VECTOR_OID,
        argtypes: "80003 23 16",
        prokind: "f",
    },
    PgvectorProcEntry {
        name: "vector_to_sparsevec",
        rettype: COMPAT_PGVECTOR_SPARSEVEC_OID,
        argtypes: "80001 23 16",
        prokind: "f",
    },
    PgvectorProcEntry {
        name: "binary_quantize",
        rettype: COMPAT_PG_BIT_OID,
        argtypes: "80001 23 16",
        prokind: "f",
    },
    PgvectorProcEntry {
        name: "vector_dims",
        rettype: 23,
        argtypes: "80003",
        prokind: "f",
    },
    PgvectorProcEntry {
        name: "l2_norm",
        rettype: 701,
        argtypes: "80003",
        prokind: "f",
    },
    PgvectorProcEntry {
        name: "l2_normalize",
        rettype: COMPAT_PGVECTOR_HALFVEC_OID,
        argtypes: "80003",
        prokind: "f",
    },
    PgvectorProcEntry {
        name: "subvector",
        rettype: COMPAT_PGVECTOR_HALFVEC_OID,
        argtypes: "80003 23 23",
        prokind: "f",
    },
    PgvectorProcEntry {
        name: "binary_quantize",
        rettype: COMPAT_PG_BIT_OID,
        argtypes: "80003",
        prokind: "f",
    },
    PgvectorProcEntry {
        name: "array_to_sparsevec",
        rettype: COMPAT_PGVECTOR_SPARSEVEC_OID,
        argtypes: "1007 23 16",
        prokind: "f",
    },
    PgvectorProcEntry {
        name: "array_to_sparsevec",
        rettype: COMPAT_PGVECTOR_SPARSEVEC_OID,
        argtypes: "1021 23 16",
        prokind: "f",
    },
    PgvectorProcEntry {
        name: "array_to_sparsevec",
        rettype: COMPAT_PGVECTOR_SPARSEVEC_OID,
        argtypes: "1022 23 16",
        prokind: "f",
    },
    PgvectorProcEntry {
        name: "array_to_sparsevec",
        rettype: COMPAT_PGVECTOR_SPARSEVEC_OID,
        argtypes: "1231 23 16",
        prokind: "f",
    },
    PgvectorProcEntry {
        name: "vector_dims",
        rettype: 23,
        argtypes: "80001",
        prokind: "f",
    },
    PgvectorProcEntry {
        name: "vector_norm",
        rettype: 701,
        argtypes: "80001",
        prokind: "f",
    },
    PgvectorProcEntry {
        name: "l2_norm",
        rettype: 701,
        argtypes: "80001",
        prokind: "f",
    },
    PgvectorProcEntry {
        name: "l2_normalize",
        rettype: COMPAT_PGVECTOR_VECTOR_OID,
        argtypes: "80001",
        prokind: "f",
    },
    PgvectorProcEntry {
        name: "subvector",
        rettype: COMPAT_PGVECTOR_VECTOR_OID,
        argtypes: "80001 23 23",
        prokind: "f",
    },
    PgvectorProcEntry {
        name: "binary_quantize",
        rettype: COMPAT_PG_BIT_OID,
        argtypes: "80001",
        prokind: "f",
    },
    PgvectorProcEntry {
        name: "hamming_distance",
        rettype: 701,
        argtypes: "1560 1560",
        prokind: "f",
    },
    PgvectorProcEntry {
        name: "jaccard_distance",
        rettype: 701,
        argtypes: "1560 1560",
        prokind: "f",
    },
    PgvectorProcEntry {
        name: "sum",
        rettype: COMPAT_PGVECTOR_VECTOR_OID,
        argtypes: "80001",
        prokind: "a",
    },
    PgvectorProcEntry {
        name: "avg",
        rettype: COMPAT_PGVECTOR_VECTOR_OID,
        argtypes: "80001",
        prokind: "a",
    },
    PgvectorProcEntry {
        name: "sum",
        rettype: COMPAT_PGVECTOR_HALFVEC_OID,
        argtypes: "80003",
        prokind: "a",
    },
    PgvectorProcEntry {
        name: "avg",
        rettype: COMPAT_PGVECTOR_HALFVEC_OID,
        argtypes: "80003",
        prokind: "a",
    },
];

// Well-known pg_proc OIDs matching PostgreSQL system catalog conventions.
// These cover the core scalar functions, operators, and I/O functions that
// AionDB implements, with real PostgreSQL OIDs for maximum compatibility.
include!("pg_proc_data_part1.inc.rs");
include!("pg_proc_data_part2.inc.rs");

pub(super) fn build_pg_proc_plan_with_catalog(
    catalog: &std::sync::Arc<dyn aiondb_catalog::CatalogReader>,
    txn_id: aiondb_core::TxnId,
    owner_oid: i32,
) -> DbResult<LogicalPlan> {
    let fields = pg_proc_fields();
    let mut rows: Vec<Vec<TypedExpr>> = PG_PROCS_PART_1
        .iter()
        .chain(PG_PROCS_PART_2.iter())
        .map(|p| {
            vec![
                int_literal(p.oid),
                text_literal(p.name),
                int_literal(PG_CATALOG_NAMESPACE_OID),
                int_literal(owner_oid),
                int_literal(LANG_INTERNAL),
                double_literal(1.0),                                 // procost
                double_literal(if p.retset { 1000.0 } else { 0.0 }), // prorows
                int_literal(0),                                      // provariadic
                null_literal(DataType::Text),                        // prosupport
                text_literal(p.prokind),
                bool_literal(false), // prosecdef
                bool_literal(false), // proleakproof
                bool_literal(true),  // proisstrict
                bool_literal(p.retset),
                text_literal(p.volatility),
                text_literal("s"), // proparallel = safe
                int_literal(p.nargs),
                int_literal(0), // pronargdefaults
                int_literal(p.rettype),
                text_literal(p.argtypes), // proargtypes
                null_literal(DataType::Array(Box::new(DataType::Int))), // proallargtypes
                null_literal(DataType::Array(Box::new(DataType::Text))), // proargmodes
                null_literal(DataType::Array(Box::new(DataType::Text))), // proargnames
                null_literal(DataType::Text), // proargdefaults
                null_literal(DataType::Array(Box::new(DataType::Int))), // protrftypes
                text_literal(p.name),     // prosrc
                null_literal(DataType::Text), // probin
                null_literal(DataType::Text), // prosqlbody
                null_literal(DataType::Array(Box::new(DataType::Text))), // proconfig
                null_literal(DataType::Array(Box::new(DataType::Text))), // proacl
            ]
        })
        .collect();
    rows.extend(PGVECTOR_PROC_ENTRIES.iter().map(|p| {
        vec![
            int_literal(compat_pgvector_function_oid(p.name, p.argtypes)),
            text_literal(p.name),
            int_literal(PG_CATALOG_NAMESPACE_OID),
            int_literal(owner_oid),
            int_literal(LANG_INTERNAL),
            double_literal(1.0),
            double_literal(0.0),
            int_literal(0),
            null_literal(DataType::Text),
            text_literal(p.prokind),
            bool_literal(false),
            bool_literal(false),
            bool_literal(true),
            bool_literal(false),
            text_literal("i"),
            text_literal("s"),
            int_literal(aiondb_core::convert::usize_to_i32_saturating(
                p.argtypes.split_whitespace().count(),
            )),
            int_literal(0),
            int_literal(p.rettype),
            text_literal(p.argtypes),
            null_literal(DataType::Array(Box::new(DataType::Int))),
            null_literal(DataType::Array(Box::new(DataType::Text))),
            null_literal(DataType::Array(Box::new(DataType::Text))),
            null_literal(DataType::Text),
            null_literal(DataType::Array(Box::new(DataType::Int))),
            text_literal(p.name),
            null_literal(DataType::Text),
            null_literal(DataType::Text),
            null_literal(DataType::Array(Box::new(DataType::Text))),
            null_literal(DataType::Array(Box::new(DataType::Text))),
        ]
    }));
    // Append rows for user-defined functions (CREATE FUNCTION). Without
    // this, ORM probes that look up `pg_proc.proname='my_fn'` see only
    // built-ins and treat user functions as nonexistent.
    for func in catalog.list_functions(txn_id)? {
        let arg_types = func
            .params
            .iter()
            .map(|p| {
                p.data_type
                    .pg_oid()
                    .map(|oid| oid.to_string())
                    .unwrap_or_else(|| "0".to_owned())
            })
            .collect::<Vec<_>>()
            .join(" ");
        let return_type_oid = func
            .return_type
            .pg_oid()
            .map(aiondb_core::convert::u32_to_i32_saturating)
            .unwrap_or(0);
        let oid = aiondb_core::compat_function_oid(&func.name);
        let lang_oid = if func.language.eq_ignore_ascii_case("plpgsql") {
            14_i32
        } else if func.language.eq_ignore_ascii_case("sql") {
            12_i32
        } else {
            13_i32
        };
        rows.push(vec![
            int_literal(oid),
            text_literal(&func.name),
            int_literal(crate::pg_catalog::PUBLIC_NAMESPACE_OID),
            int_literal(owner_oid),
            int_literal(lang_oid),
            double_literal(100.0),
            double_literal(0.0),
            int_literal(0),
            null_literal(DataType::Text),
            text_literal("f"),
            bool_literal(false),
            bool_literal(false),
            bool_literal(false),
            bool_literal(false),
            text_literal("v"),
            text_literal("u"),
            int_literal(aiondb_core::convert::usize_to_i32_saturating(
                func.params.len(),
            )),
            int_literal(0),
            int_literal(return_type_oid),
            text_literal(&arg_types),
            null_literal(DataType::Array(Box::new(DataType::Int))),
            null_literal(DataType::Array(Box::new(DataType::Text))),
            null_literal(DataType::Array(Box::new(DataType::Text))),
            null_literal(DataType::Text),
            null_literal(DataType::Array(Box::new(DataType::Int))),
            text_literal(&func.body),
            null_literal(DataType::Text),
            null_literal(DataType::Text),
            null_literal(DataType::Array(Box::new(DataType::Text))),
            null_literal(DataType::Array(Box::new(DataType::Text))),
        ]);
    }
    Ok(project_values(fields, rows))
}
