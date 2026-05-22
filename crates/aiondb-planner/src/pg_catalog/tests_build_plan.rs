use super::*;
use crate::pg_catalog::core_tables::SYSTEM_CATALOG_TABLES;

#[test]
fn pg_index_lists_indexes_with_properties() {
    let catalog = mock_catalog();
    let plan = build_plan(&catalog, txn(), "pg_index", None, None, None)
        .expect("ok")
        .expect("should be Some");

    let (_fields, rows) = unwrap_rows(plan);

    // One index (users_pkey)
    assert_eq!(rows.len(), 1);

    let vals = extract_values(&rows[0]);
    assert_eq!(vals[2], "true"); // indisunique
    assert_eq!(vals[3], "true"); // indisprimary
    assert_eq!(vals[4], "[0:0]={1}"); // indkey (zero-based pg vector)
}

// ---------------------------------------------------------------
// build_plan - pg_constraint
// ---------------------------------------------------------------

#[test]
fn pg_constraint_lists_primary_key() {
    let catalog = mock_catalog();
    let plan = build_plan(&catalog, txn(), "pg_constraint", None, None, None)
        .expect("ok")
        .expect("should be Some");

    let (fields, rows) = unwrap_rows(plan);

    // One PK constraint
    assert_eq!(rows.len(), 1);
    assert_eq!(
        fields[7].data_type,
        DataType::Array(Box::new(DataType::Int))
    );
    assert_eq!(
        fields[9].data_type,
        DataType::Array(Box::new(DataType::Int))
    );

    let vals = extract_values(&rows[0]);
    assert_eq!(vals[1], "users_pkey"); // conname
    assert_eq!(vals[3], "p"); // contype = primary key
    assert_eq!(vals[7], "{1}"); // conkey
    assert_eq!(vals[9], "NULL"); // confkey
}

// ---------------------------------------------------------------
// build_plan - unknown table returns None
// ---------------------------------------------------------------

#[test]
fn build_plan_unknown_table_returns_none() {
    let catalog = mock_catalog();
    let result = build_plan(&catalog, txn(), "pg_nosuch_table", None, None, None).expect("ok");
    assert!(result.is_none());
}

// ---------------------------------------------------------------
// build_plan - case insensitive table names
// ---------------------------------------------------------------

#[test]
fn build_plan_case_insensitive() {
    let catalog = mock_catalog();
    assert!(
        build_plan(&catalog, txn(), "PG_NAMESPACE", None, None, None)
            .expect("ok")
            .is_some()
    );
    assert!(build_plan(&catalog, txn(), "Pg_Class", None, None, None)
        .expect("ok")
        .is_some());
    assert!(build_plan(&catalog, txn(), "PG_TYPE", None, None, None)
        .expect("ok")
        .is_some());
}

#[test]
fn pg_tables_is_recognized_for_compat_introspection() {
    let table = "pg_tables";
    assert!(is_pg_catalog_table(table), "{table} should be recognized");
    assert!(
        output_fields_for(table).is_some(),
        "{table} should describe"
    );
    assert!(
        build_plan(&mock_catalog(), txn(), table, None, None, None)
            .expect("ok")
            .is_some(),
        "{table} should plan"
    );
}

#[test]
fn build_table_descriptor_uses_stable_unique_ids() {
    let namespace = build_table_descriptor("pg_namespace").expect("descriptor");
    let indexes = build_table_descriptor("pg_indexes").expect("descriptor");
    assert_ne!(namespace.table_id, indexes.table_id);
}

// ---------------------------------------------------------------
// Empty catalog: pg_class/pg_attribute produce no rows
// ---------------------------------------------------------------

#[test]
fn pg_class_empty_catalog() {
    let catalog: Arc<dyn CatalogReader> = Arc::new(crate::EmptyCatalog);
    let plan = build_plan(&catalog, txn(), "pg_class", None, None, None)
        .expect("ok")
        .expect("should be Some");

    let (_fields, rows) = unwrap_rows(plan);
    // No user tables, but well-known system catalog tables are always emitted
    assert!(!rows.is_empty(), "system catalog tables should be present");
    // All rows should be system catalog tables (relkind = 'r' in pg_catalog namespace)
    for row in &rows {
        let vals = extract_values(row);
        assert_eq!(vals[3], "r", "system table should have relkind = 'r'");
    }
}

#[test]
fn pg_attribute_empty_catalog() {
    let catalog: Arc<dyn CatalogReader> = Arc::new(crate::EmptyCatalog);
    let plan = build_plan(&catalog, txn(), "pg_attribute", None, None, None)
        .expect("ok")
        .expect("should be Some");

    let (_fields, rows) = unwrap_rows(plan);
    assert!(
        !rows.is_empty(),
        "system catalog attribute rows should be present"
    );
    for row in &rows {
        let vals = extract_values(row);
        let attrelid = vals[0].parse::<i32>().expect("attrelid");
        assert!(
            SYSTEM_CATALOG_TABLES
                .iter()
                .any(|(oid, _name)| *oid == attrelid),
            "unexpected non-system attrelid {attrelid}"
        );
    }
}

#[test]
fn pg_namespace_always_has_builtin_schemas() {
    let catalog: Arc<dyn CatalogReader> = Arc::new(crate::EmptyCatalog);
    let plan = build_plan(&catalog, txn(), "pg_namespace", None, None, None)
        .expect("ok")
        .expect("should be Some");

    let (_fields, rows) = unwrap_rows(plan);
    let names: Vec<String> = rows.iter().map(|r| extract_values(r)[1].clone()).collect();
    assert!(names.contains(&"pg_catalog".to_owned()));
    assert!(names.contains(&"information_schema".to_owned()));
}

#[test]
fn pg_type_always_has_entries() {
    let catalog: Arc<dyn CatalogReader> = Arc::new(crate::EmptyCatalog);
    let plan = build_plan(&catalog, txn(), "pg_type", None, None, None)
        .expect("ok")
        .expect("should be Some");

    let (_fields, rows) = unwrap_rows(plan);
    assert_eq!(rows.len(), PG_TYPE_ENTRIES.len());
}

#[test]
fn pgvector_catalog_rows_are_cross_referenced() {
    let catalog: Arc<dyn CatalogReader> = Arc::new(crate::EmptyCatalog);

    let pg_type = build_plan(&catalog, txn(), "pg_type", None, None, None)
        .expect("pg_type")
        .expect("pg_type plan");
    let (_fields, type_rows) = unwrap_rows(pg_type);
    let type_values = type_rows
        .iter()
        .map(|row| extract_values(row))
        .collect::<Vec<_>>();
    assert!(type_values.iter().any(|row| {
        row[0] == COMPAT_PGVECTOR_VECTOR_OID.to_string()
            && row[1] == "vector"
            && row[2] == COMPAT_PGVECTOR_VECTOR_ARRAY_OID.to_string()
    }));
    let expected_vector_in_oid = compat_pgvector_function_oid("vector_in", "2275");
    let expected_vector_out_oid =
        compat_pgvector_function_oid("vector_out", &COMPAT_PGVECTOR_VECTOR_OID.to_string());
    assert!(type_values.iter().any(|row| {
        row[0] == COMPAT_PGVECTOR_VECTOR_OID.to_string()
            && row[17] == expected_vector_in_oid.to_string()
            && row[18] == expected_vector_out_oid.to_string()
    }));

    let pg_proc = build_plan(&catalog, txn(), "pg_proc", None, None, None)
        .expect("pg_proc")
        .expect("pg_proc plan");
    let (_fields, proc_rows) = unwrap_rows(pg_proc);
    let expected_l2_oid = compat_pgvector_function_oid(
        "l2_distance",
        &format!("{COMPAT_PGVECTOR_VECTOR_OID} {COMPAT_PGVECTOR_VECTOR_OID}"),
    );
    assert!(proc_rows.iter().map(|row| extract_values(row)).any(|row| {
        row[0] == expected_l2_oid.to_string() && row[1] == "l2_distance" && row[18] == "701"
    }));
    let expected_sparse_l2_oid = compat_pgvector_function_oid(
        "l2_distance",
        &format!("{COMPAT_PGVECTOR_SPARSEVEC_OID} {COMPAT_PGVECTOR_SPARSEVEC_OID}"),
    );
    assert!(proc_rows.iter().map(|row| extract_values(row)).any(|row| {
        row[0] == expected_sparse_l2_oid.to_string()
            && row[1] == "l2_distance"
            && row[19] == format!("{COMPAT_PGVECTOR_SPARSEVEC_OID} {COMPAT_PGVECTOR_SPARSEVEC_OID}")
    }));
    let expected_sparse_l2_norm_oid =
        compat_pgvector_function_oid("l2_norm", &COMPAT_PGVECTOR_SPARSEVEC_OID.to_string());
    assert!(proc_rows.iter().map(|row| extract_values(row)).any(|row| {
        row[0] == expected_sparse_l2_norm_oid.to_string()
            && row[1] == "l2_norm"
            && row[18] == "701"
            && row[19] == COMPAT_PGVECTOR_SPARSEVEC_OID.to_string()
    }));
    let expected_sparse_l2_normalize_oid =
        compat_pgvector_function_oid("l2_normalize", &COMPAT_PGVECTOR_SPARSEVEC_OID.to_string());
    assert!(proc_rows.iter().map(|row| extract_values(row)).any(|row| {
        row[0] == expected_sparse_l2_normalize_oid.to_string()
            && row[1] == "l2_normalize"
            && row[18] == COMPAT_PGVECTOR_SPARSEVEC_OID.to_string()
            && row[19] == COMPAT_PGVECTOR_SPARSEVEC_OID.to_string()
    }));
    let expected_sum_oid = compat_pgvector_function_oid("sum", "80001");
    assert!(proc_rows
        .iter()
        .map(|row| extract_values(row))
        .any(|row| { row[0] == expected_sum_oid.to_string() && row[1] == "sum" && row[9] == "a" }));
    let expected_vector_in_typmod_oid = compat_pgvector_function_oid("vector_in", "2275 26 23");
    assert!(proc_rows.iter().map(|row| extract_values(row)).any(|row| {
        row[0] == expected_vector_in_typmod_oid.to_string()
            && row[1] == "vector_in"
            && row[16] == "3"
            && row[19] == "2275 26 23"
    }));
    let expected_array_to_halfvec_oid =
        compat_pgvector_function_oid("array_to_halfvec", "1022 23 16");
    assert!(proc_rows.iter().map(|row| extract_values(row)).any(|row| {
        row[0] == expected_array_to_halfvec_oid.to_string()
            && row[1] == "array_to_halfvec"
            && row[18] == COMPAT_PGVECTOR_HALFVEC_OID.to_string()
            && row[19] == "1022 23 16"
    }));
    let expected_halfvec_to_float4_oid =
        compat_pgvector_function_oid("halfvec_to_float4", "80003 23 16");
    assert!(proc_rows.iter().map(|row| extract_values(row)).any(|row| {
        row[0] == expected_halfvec_to_float4_oid.to_string()
            && row[1] == "halfvec_to_float4"
            && row[18] == "1021"
            && row[19] == "80003 23 16"
    }));
    let expected_vector_add_oid = compat_pgvector_function_oid(
        "vector_add",
        &format!("{COMPAT_PGVECTOR_VECTOR_OID} {COMPAT_PGVECTOR_VECTOR_OID}"),
    );
    assert!(proc_rows.iter().map(|row| extract_values(row)).any(|row| {
        row[0] == expected_vector_add_oid.to_string()
            && row[1] == "vector_add"
            && row[18] == COMPAT_PGVECTOR_VECTOR_OID.to_string()
            && row[19] == format!("{COMPAT_PGVECTOR_VECTOR_OID} {COMPAT_PGVECTOR_VECTOR_OID}")
    }));
    let expected_halfvec_concat_oid = compat_pgvector_function_oid(
        "halfvec_concat",
        &format!("{COMPAT_PGVECTOR_HALFVEC_OID} {COMPAT_PGVECTOR_HALFVEC_OID}"),
    );
    assert!(proc_rows.iter().map(|row| extract_values(row)).any(|row| {
        row[0] == expected_halfvec_concat_oid.to_string()
            && row[1] == "halfvec_concat"
            && row[18] == COMPAT_PGVECTOR_HALFVEC_OID.to_string()
            && row[19] == format!("{COMPAT_PGVECTOR_HALFVEC_OID} {COMPAT_PGVECTOR_HALFVEC_OID}")
    }));
    let expected_halfvec_binary_quantize_oid =
        compat_pgvector_function_oid("binary_quantize", &COMPAT_PGVECTOR_HALFVEC_OID.to_string());
    assert!(proc_rows.iter().map(|row| extract_values(row)).any(|row| {
        row[0] == expected_halfvec_binary_quantize_oid.to_string()
            && row[1] == "binary_quantize"
            && row[18] == COMPAT_PG_BIT_OID.to_string()
            && row[19] == COMPAT_PGVECTOR_HALFVEC_OID.to_string()
    }));

    let pg_aggregate = build_plan(&catalog, txn(), "pg_aggregate", None, None, None)
        .expect("pg_aggregate")
        .expect("pg_aggregate plan");
    let (_fields, aggregate_rows) = unwrap_rows(pg_aggregate);
    assert!(aggregate_rows
        .iter()
        .map(|row| extract_values(row))
        .any(|row| row[0] == expected_sum_oid.to_string()
            && row[16] == COMPAT_PGVECTOR_VECTOR_OID.to_string()));

    let pg_operator = build_plan(&catalog, txn(), "pg_operator", None, None, None)
        .expect("pg_operator")
        .expect("pg_operator plan");
    let (_fields, operator_rows) = unwrap_rows(pg_operator);
    let expected_operator_oid = compat_pgvector_operator_oid(COMPAT_PGVECTOR_VECTOR_OID, "<->");
    assert!(operator_rows
        .iter()
        .map(|row| extract_values(row))
        .any(|row| row[0] == expected_operator_oid.to_string()
            && row[1] == "<->"
            && row[12] == expected_l2_oid.to_string()));
    let expected_vector_add_operator_oid =
        compat_pgvector_operator_oid(COMPAT_PGVECTOR_VECTOR_OID, "+");
    assert!(operator_rows
        .iter()
        .map(|row| extract_values(row))
        .any(|row| row[0] == expected_vector_add_operator_oid.to_string()
            && row[1] == "+"
            && row[9] == COMPAT_PGVECTOR_VECTOR_OID.to_string()
            && row[12] == expected_vector_add_oid.to_string()));
    let expected_halfvec_concat_operator_oid =
        compat_pgvector_operator_oid(COMPAT_PGVECTOR_HALFVEC_OID, "||");
    assert!(operator_rows
        .iter()
        .map(|row| extract_values(row))
        .any(
            |row| row[0] == expected_halfvec_concat_operator_oid.to_string()
                && row[1] == "||"
                && row[9] == COMPAT_PGVECTOR_HALFVEC_OID.to_string()
                && row[12] == expected_halfvec_concat_oid.to_string(),
        ));

    let pg_opclass = build_plan(&catalog, txn(), "pg_opclass", None, None, None)
        .expect("pg_opclass")
        .expect("pg_opclass plan");
    let (_fields, opclass_rows) = unwrap_rows(pg_opclass);
    let vector_l2_opclass_oid =
        compat_pgvector_opclass_oid(COMPAT_PGVECTOR_HNSW_AM_OID, "vector_l2_ops");
    assert!(opclass_rows
        .iter()
        .map(|row| extract_values(row))
        .any(|row| {
            row[0] == vector_l2_opclass_oid.to_string()
                && row[1] == COMPAT_PGVECTOR_HNSW_AM_OID.to_string()
                && row[2] == "vector_l2_ops"
                && row[5] == vector_l2_opclass_oid.to_string()
                && row[6] == COMPAT_PGVECTOR_VECTOR_OID.to_string()
                && row[7] == "true"
        }));
    let sparse_l2_opclass_oid =
        compat_pgvector_opclass_oid(COMPAT_PGVECTOR_HNSW_AM_OID, "sparsevec_l2_ops");
    assert!(opclass_rows
        .iter()
        .map(|row| extract_values(row))
        .any(|row| {
            row[0] == sparse_l2_opclass_oid.to_string()
                && row[2] == "sparsevec_l2_ops"
                && row[6] == COMPAT_PGVECTOR_SPARSEVEC_OID.to_string()
        }));
    let bit_hamming_ivfflat_oid =
        compat_pgvector_opclass_oid(COMPAT_PGVECTOR_IVFFLAT_AM_OID, "bit_hamming_ops");
    assert!(opclass_rows
        .iter()
        .map(|row| extract_values(row))
        .any(|row| {
            row[0] == bit_hamming_ivfflat_oid.to_string()
                && row[1] == COMPAT_PGVECTOR_IVFFLAT_AM_OID.to_string()
                && row[2] == "bit_hamming_ops"
                && row[6] == COMPAT_PG_BIT_OID.to_string()
        }));
    let vector_l1_ivfflat_oid =
        compat_pgvector_opclass_oid(COMPAT_PGVECTOR_IVFFLAT_AM_OID, "vector_l1_ops");
    assert!(!opclass_rows
        .iter()
        .map(|row| extract_values(row))
        .any(|row| row[0] == vector_l1_ivfflat_oid.to_string()));
    let sparse_l2_ivfflat_oid =
        compat_pgvector_opclass_oid(COMPAT_PGVECTOR_IVFFLAT_AM_OID, "sparsevec_l2_ops");
    assert!(!opclass_rows
        .iter()
        .map(|row| extract_values(row))
        .any(|row| row[0] == sparse_l2_ivfflat_oid.to_string()));

    let pg_opfamily = build_plan(&catalog, txn(), "pg_opfamily", None, None, None)
        .expect("pg_opfamily")
        .expect("pg_opfamily plan");
    let (_fields, opfamily_rows) = unwrap_rows(pg_opfamily);
    assert!(opfamily_rows
        .iter()
        .map(|row| extract_values(row))
        .any(|row| row[0] == vector_l2_opclass_oid.to_string()
            && row[1] == COMPAT_PGVECTOR_HNSW_AM_OID.to_string()
            && row[2] == "vector_l2_ops"));

    let pg_amop = build_plan(&catalog, txn(), "pg_amop", None, None, None)
        .expect("pg_amop")
        .expect("pg_amop plan");
    let (_fields, amop_rows) = unwrap_rows(pg_amop);
    assert!(amop_rows.iter().map(|row| extract_values(row)).any(|row| {
        row[1] == vector_l2_opclass_oid.to_string()
            && row[2] == COMPAT_PGVECTOR_VECTOR_OID.to_string()
            && row[6] == expected_operator_oid.to_string()
            && row[7] == COMPAT_PGVECTOR_HNSW_AM_OID.to_string()
    }));

    let pg_amproc = build_plan(&catalog, txn(), "pg_amproc", None, None, None)
        .expect("pg_amproc")
        .expect("pg_amproc plan");
    let (_fields, amproc_rows) = unwrap_rows(pg_amproc);
    assert!(amproc_rows
        .iter()
        .map(|row| extract_values(row))
        .any(|row| {
            row[1] == vector_l2_opclass_oid.to_string()
                && row[2] == COMPAT_PGVECTOR_VECTOR_OID.to_string()
                && row[3] == COMPAT_PGVECTOR_VECTOR_OID.to_string()
                && row[4] == "1"
                && row[5] == expected_l2_oid.to_string()
        }));
}

#[test]
fn pgvector_cast_catalog_rows_reference_pgvector_procs() {
    let catalog: Arc<dyn CatalogReader> = Arc::new(crate::EmptyCatalog);
    let pg_cast = build_plan(&catalog, txn(), "pg_cast", None, None, None)
        .expect("pg_cast")
        .expect("pg_cast plan");
    let (_fields, cast_rows) = unwrap_rows(pg_cast);

    let array_to_vector_oid = compat_pgvector_function_oid("array_to_vector", "1007 23 16");
    assert!(cast_rows.iter().map(|row| extract_values(row)).any(|row| {
        row[1] == "1007"
            && row[2] == COMPAT_PGVECTOR_VECTOR_OID.to_string()
            && row[3] == array_to_vector_oid.to_string()
            && row[4] == "a"
            && row[5] == "f"
    }));

    let vector_to_halfvec_oid = compat_pgvector_function_oid("vector_to_halfvec", "80001 23 16");
    assert!(cast_rows.iter().map(|row| extract_values(row)).any(|row| {
        row[1] == COMPAT_PGVECTOR_VECTOR_OID.to_string()
            && row[2] == COMPAT_PGVECTOR_HALFVEC_OID.to_string()
            && row[3] == vector_to_halfvec_oid.to_string()
            && row[4] == "i"
            && row[5] == "f"
    }));

    let halfvec_to_float4_oid = compat_pgvector_function_oid("halfvec_to_float4", "80003 23 16");
    assert!(cast_rows.iter().map(|row| extract_values(row)).any(|row| {
        row[1] == COMPAT_PGVECTOR_HALFVEC_OID.to_string()
            && row[2] == "1021"
            && row[3] == halfvec_to_float4_oid.to_string()
            && row[4] == "a"
            && row[5] == "f"
    }));

    let array_to_halfvec_oid = compat_pgvector_function_oid("array_to_halfvec", "1007 23 16");
    assert!(cast_rows.iter().map(|row| extract_values(row)).any(|row| {
        row[1] == "1007"
            && row[2] == COMPAT_PGVECTOR_HALFVEC_OID.to_string()
            && row[3] == array_to_halfvec_oid.to_string()
            && row[4] == "a"
            && row[5] == "f"
    }));

    let halfvec_to_sparsevec_oid =
        compat_pgvector_function_oid("halfvec_to_sparsevec", "80003 23 16");
    assert!(cast_rows.iter().map(|row| extract_values(row)).any(|row| {
        row[1] == COMPAT_PGVECTOR_HALFVEC_OID.to_string()
            && row[2] == COMPAT_PGVECTOR_SPARSEVEC_OID.to_string()
            && row[3] == halfvec_to_sparsevec_oid.to_string()
            && row[4] == "i"
            && row[5] == "f"
    }));

    let sparsevec_to_vector_oid =
        compat_pgvector_function_oid("sparsevec_to_vector", "80005 23 16");
    assert!(cast_rows.iter().map(|row| extract_values(row)).any(|row| {
        row[1] == COMPAT_PGVECTOR_SPARSEVEC_OID.to_string()
            && row[2] == COMPAT_PGVECTOR_VECTOR_OID.to_string()
            && row[3] == sparsevec_to_vector_oid.to_string()
            && row[4] == "a"
            && row[5] == "f"
    }));

    let sparsevec_to_halfvec_oid =
        compat_pgvector_function_oid("sparsevec_to_halfvec", "80005 23 16");
    assert!(cast_rows.iter().map(|row| extract_values(row)).any(|row| {
        row[1] == COMPAT_PGVECTOR_SPARSEVEC_OID.to_string()
            && row[2] == COMPAT_PGVECTOR_HALFVEC_OID.to_string()
            && row[3] == sparsevec_to_halfvec_oid.to_string()
            && row[4] == "a"
            && row[5] == "f"
    }));

    let array_to_sparsevec_oid = compat_pgvector_function_oid("array_to_sparsevec", "1007 23 16");
    assert!(cast_rows.iter().map(|row| extract_values(row)).any(|row| {
        row[1] == "1007"
            && row[2] == COMPAT_PGVECTOR_SPARSEVEC_OID.to_string()
            && row[3] == array_to_sparsevec_oid.to_string()
            && row[4] == "a"
            && row[5] == "f"
    }));
}

#[test]
fn pg_type_select_plan_supports_filter_projection_and_alias() {
    let catalog: Arc<dyn CatalogReader> = Arc::new(crate::EmptyCatalog);
    let stmt = parse_prepared_statement(
        "SELECT typname AS name, oid, typarray AS array_oid, oid::text AS regtype, typdelim AS delimiter \
         FROM pg_type t WHERE t.oid = to_regtype('int4') ORDER BY t.oid",
    )
    .expect("parse");
    let aiondb_parser::Statement::Select(select) = stmt else {
        panic!("expected SELECT");
    };

    let plan = build_select_plan(&catalog, txn(), &select, None, None, None)
        .expect("plan should succeed")
        .expect("pg_type should be recognized");
    let (fields, rows) = unwrap_rows(plan);

    assert_eq!(
        fields
            .iter()
            .map(|field| field.name.as_str())
            .collect::<Vec<_>>(),
        vec!["name", "oid", "array_oid", "regtype", "delimiter"]
    );
    assert_eq!(rows.len(), 1);
    let values = extract_values(&rows[0]);
    assert_eq!(values[0], "int4");
    assert_eq!(values[1], "23");
}

#[test]
fn pg_type_select_plan_resolves_pgvector_typmod_regtype() {
    let catalog: Arc<dyn CatalogReader> = Arc::new(crate::EmptyCatalog);
    let stmt = parse_prepared_statement(
        "SELECT typname, oid FROM pg_type WHERE oid = to_regtype('vector(3)')",
    )
    .expect("parse");
    let aiondb_parser::Statement::Select(select) = stmt else {
        panic!("expected SELECT");
    };

    let plan = build_select_plan(&catalog, txn(), &select, None, None, None)
        .expect("plan should succeed")
        .expect("pg_type should be recognized");
    let (_fields, rows) = unwrap_rows(plan);

    assert_eq!(rows.len(), 1);
    let values = extract_values(&rows[0]);
    assert_eq!(values[0], "vector");
    assert_eq!(values[1], COMPAT_PGVECTOR_VECTOR_OID.to_string());
}

#[test]
fn pg_operator_select_plan_supports_pgvector_to_regoperator_filter() {
    let catalog: Arc<dyn CatalogReader> = Arc::new(crate::EmptyCatalog);
    for (signature, name, type_oid) in [
        ("<->(vector, vector)", "<->", COMPAT_PGVECTOR_VECTOR_OID),
        ("+(vector, vector)", "+", COMPAT_PGVECTOR_VECTOR_OID),
        ("||(halfvec, halfvec)", "||", COMPAT_PGVECTOR_HALFVEC_OID),
    ] {
        let stmt = parse_prepared_statement(&format!(
            "SELECT oprname, oid \
             FROM pg_operator \
             WHERE oid = to_regoperator('{signature}')",
        ))
        .expect("parse");
        let aiondb_parser::Statement::Select(select) = stmt else {
            panic!("expected SELECT");
        };

        let plan = build_select_plan(&catalog, txn(), &select, None, None, None)
            .expect("plan should succeed")
            .expect("pg_operator should be recognized");
        let (_fields, rows) = unwrap_rows(plan);

        assert_eq!(rows.len(), 1);
        let values = extract_values(&rows[0]);
        assert_eq!(values[0], name);
        assert_eq!(
            values[1],
            compat_pgvector_operator_oid(type_oid, name).to_string()
        );
    }
}

#[test]
fn pg_proc_select_plan_supports_pgvector_to_regprocedure_filter() {
    let catalog: Arc<dyn CatalogReader> = Arc::new(crate::EmptyCatalog);
    for (signature, name, argtypes) in [
        (
            "l2_distance(vector, vector)",
            "l2_distance",
            format!("{COMPAT_PGVECTOR_VECTOR_OID} {COMPAT_PGVECTOR_VECTOR_OID}"),
        ),
        (
            "vector_add(vector, vector)",
            "vector_add",
            format!("{COMPAT_PGVECTOR_VECTOR_OID} {COMPAT_PGVECTOR_VECTOR_OID}"),
        ),
        (
            "halfvec_concat(halfvec, halfvec)",
            "halfvec_concat",
            format!("{COMPAT_PGVECTOR_HALFVEC_OID} {COMPAT_PGVECTOR_HALFVEC_OID}"),
        ),
        (
            "l2_norm(sparsevec)",
            "l2_norm",
            COMPAT_PGVECTOR_SPARSEVEC_OID.to_string(),
        ),
        (
            "l2_normalize(sparsevec)",
            "l2_normalize",
            COMPAT_PGVECTOR_SPARSEVEC_OID.to_string(),
        ),
        (
            "binary_quantize(halfvec)",
            "binary_quantize",
            COMPAT_PGVECTOR_HALFVEC_OID.to_string(),
        ),
        (
            "halfvec_to_float4(halfvec, integer, boolean)",
            "halfvec_to_float4",
            "80003 23 16".to_string(),
        ),
    ] {
        let stmt = parse_prepared_statement(&format!(
            "SELECT proname, oid \
             FROM pg_proc \
             WHERE oid = to_regprocedure('{signature}')",
        ))
        .expect("parse");
        let aiondb_parser::Statement::Select(select) = stmt else {
            panic!("expected SELECT");
        };

        let plan = build_select_plan(&catalog, txn(), &select, None, None, None)
            .expect("plan should succeed")
            .expect("pg_proc should be recognized");
        let (_fields, rows) = unwrap_rows(plan);

        assert_eq!(rows.len(), 1);
        let values = extract_values(&rows[0]);
        assert_eq!(values[0], name);
        assert_eq!(
            values[1],
            compat_pgvector_function_oid(name, &argtypes).to_string()
        );
    }
}

#[test]
fn pg_proc_select_plan_supports_pgvector_typmod_input_regprocedure_filter() {
    let catalog: Arc<dyn CatalogReader> = Arc::new(crate::EmptyCatalog);
    let stmt = parse_prepared_statement(
        "SELECT proname, oid \
         FROM pg_proc \
         WHERE oid = to_regprocedure('vector_in(cstring, oid, integer)')",
    )
    .expect("parse");
    let aiondb_parser::Statement::Select(select) = stmt else {
        panic!("expected SELECT");
    };

    let plan = build_select_plan(&catalog, txn(), &select, None, None, None)
        .expect("plan should succeed")
        .expect("pg_proc should be recognized");
    let (_fields, rows) = unwrap_rows(plan);

    assert_eq!(rows.len(), 1);
    let values = extract_values(&rows[0]);
    assert_eq!(values[0], "vector_in");
    assert_eq!(
        values[1],
        compat_pgvector_function_oid("vector_in", "2275 26 23").to_string()
    );
}

#[test]
fn pg_proc_select_plan_supports_pgvector_cast_regprocedure_filter() {
    let catalog: Arc<dyn CatalogReader> = Arc::new(crate::EmptyCatalog);
    let stmt = parse_prepared_statement(
        "SELECT proname, oid \
         FROM pg_proc \
         WHERE oid = to_regprocedure('array_to_vector(integer[], integer, boolean)')",
    )
    .expect("parse");
    let aiondb_parser::Statement::Select(select) = stmt else {
        panic!("expected SELECT");
    };

    let plan = build_select_plan(&catalog, txn(), &select, None, None, None)
        .expect("plan should succeed")
        .expect("pg_proc should be recognized");
    let (_fields, rows) = unwrap_rows(plan);

    assert_eq!(rows.len(), 1);
    let values = extract_values(&rows[0]);
    assert_eq!(values[0], "array_to_vector");
    assert_eq!(
        values[1],
        compat_pgvector_function_oid("array_to_vector", "1007 23 16").to_string()
    );
}

#[test]
fn pg_settings_select_plan_supports_like_filter() {
    let catalog: Arc<dyn CatalogReader> = Arc::new(crate::EmptyCatalog);
    let stmt = parse_prepared_statement(
        "SELECT name, setting FROM pg_settings WHERE name LIKE 'enable%' ORDER BY name",
    )
    .expect("parse");
    let aiondb_parser::Statement::Select(select) = stmt else {
        panic!("expected SELECT");
    };

    let plan = build_select_plan(&catalog, txn(), &select, None, None, None)
        .expect("plan should succeed")
        .expect("pg_settings should be recognized");
    match plan {
        LogicalPlan::ProjectSource {
            source,
            outputs,
            filter,
            order_by,
            ..
        } => {
            assert!(matches!(*source, LogicalPlan::ProjectValues { .. }));
            assert_eq!(outputs.len(), 2);
            assert_eq!(outputs[0].field.name, "name");
            assert_eq!(outputs[1].field.name, "setting");
            assert!(filter.is_some(), "expected LIKE filter to be preserved");
            assert_eq!(order_by.len(), 1, "expected ORDER BY name");
        }
        other => panic!("expected ProjectSource, got {other:?}"),
    }
}

#[test]
fn pg_settings_select_plan_supports_in_list_filter() {
    let catalog: Arc<dyn CatalogReader> = Arc::new(crate::EmptyCatalog);
    let stmt = parse_prepared_statement(
        "SELECT name \
         FROM pg_settings \
         WHERE name IN ('search_path', 'server_version') \
         ORDER BY name",
    )
    .expect("parse");
    let aiondb_parser::Statement::Select(select) = stmt else {
        panic!("expected SELECT");
    };

    let plan = build_select_plan(&catalog, txn(), &select, None, None, None)
        .expect("plan should succeed")
        .expect("pg_settings should be recognized");
    match plan {
        LogicalPlan::ProjectSource {
            source,
            outputs,
            filter,
            order_by,
            ..
        } => {
            assert!(matches!(*source, LogicalPlan::ProjectValues { .. }));
            assert_eq!(outputs.len(), 1);
            assert_eq!(outputs[0].field.name, "name");
            assert!(filter.is_some(), "expected IN-list filter to be preserved");
            assert_eq!(order_by.len(), 1, "expected ORDER BY name");
        }
        other => panic!("expected ProjectSource, got {other:?}"),
    }
}

#[test]
fn pg_available_extensions_lists_vector_marker() {
    let catalog: Arc<dyn CatalogReader> = Arc::new(crate::EmptyCatalog);
    let plan = build_plan(&catalog, txn(), "pg_available_extensions", None, None, None)
        .expect("pg_available_extensions")
        .expect("pg_available_extensions plan");
    let (_fields, rows) = unwrap_rows(plan);

    assert!(rows.iter().map(|row| extract_values(row)).any(|row| {
        row[0] == "vector"
            && row[1] == "0.8.2"
            && row[2] == "NULL"
            && row[3] == "vector data type and similarity search"
    }));
}

#[test]
fn pg_available_extension_versions_lists_vector_marker() {
    let catalog: Arc<dyn CatalogReader> = Arc::new(crate::EmptyCatalog);
    let plan = build_plan(
        &catalog,
        txn(),
        "pg_available_extension_versions",
        None,
        None,
        None,
    )
    .expect("pg_available_extension_versions")
    .expect("pg_available_extension_versions plan");
    let (_fields, rows) = unwrap_rows(plan);

    assert!(rows.iter().map(|row| extract_values(row)).any(|row| {
        row[0] == "vector"
            && row[1] == "0.8.2"
            && row[2] == "false"
            && row[6] == "pg_catalog"
            && row[8] == "vector data type and similarity search"
    }));
}

#[test]
fn pg_namespace_select_plan_supports_regnamespace_filter() {
    let catalog: Arc<dyn CatalogReader> = Arc::new(crate::EmptyCatalog);
    let stmt = parse_prepared_statement(
        "SELECT oid, nspname \
         FROM pg_namespace \
         WHERE oid = to_regnamespace('pg_catalog')",
    )
    .expect("parse");
    let aiondb_parser::Statement::Select(select) = stmt else {
        panic!("expected SELECT");
    };

    let plan = build_select_plan(&catalog, txn(), &select, None, None, None)
        .expect("plan should succeed")
        .expect("pg_namespace should be recognized");
    let (_fields, rows) = unwrap_rows(plan);

    assert_eq!(rows.len(), 1);
    let values = extract_values(&rows[0]);
    assert_eq!(values[0], "11");
    assert_eq!(values[1], "pg_catalog");
}

#[test]
fn pg_backend_memory_contexts_supports_binary_projection() {
    let catalog: Arc<dyn CatalogReader> = Arc::new(crate::EmptyCatalog);
    let stmt = parse_prepared_statement(
        "SELECT name, ident, parent, level, total_bytes >= free_bytes \
         FROM pg_backend_memory_contexts WHERE level = 0",
    )
    .expect("parse");
    let aiondb_parser::Statement::Select(select) = stmt else {
        panic!("expected SELECT");
    };

    let plan = build_select_plan(&catalog, txn(), &select, None, None, None)
        .expect("plan should succeed")
        .expect("pg_backend_memory_contexts should be recognized");
    let (_fields, rows) = unwrap_rows(plan);

    assert_eq!(rows.len(), 1);
    let values = extract_values(&rows[0]);
    assert_eq!(values[0], "TopMemoryContext");
    assert_eq!(values[1], "NULL");
    assert_eq!(values[2], "NULL");
    assert_eq!(values[3], "0");
    assert_eq!(values[4], "true");
}

#[test]
fn pg_config_contains_more_than_twenty_rows() {
    let catalog: Arc<dyn CatalogReader> = Arc::new(crate::EmptyCatalog);
    let plan = build_plan(&catalog, txn(), "pg_config", None, None, None)
        .expect("ok")
        .expect("should be Some");

    let (_fields, rows) = unwrap_rows(plan);
    assert!(rows.len() > 20);
}

#[test]
fn pg_virtual_aggregates_support_count_filter() {
    let catalog: Arc<dyn CatalogReader> = Arc::new(crate::EmptyCatalog);
    let stmt = parse_prepared_statement(
        "SELECT count(*) > 0 AS ok, count(*) FILTER (WHERE error IS NOT NULL) = 0 AS no_err \
         FROM pg_hba_file_rules",
    )
    .expect("parse");
    let aiondb_parser::Statement::Select(select) = stmt else {
        panic!("expected SELECT");
    };

    let plan = build_select_plan(&catalog, txn(), &select, None, None, None)
        .expect("plan should succeed")
        .expect("pg_hba_file_rules should be recognized");
    let (_fields, rows) = unwrap_rows(plan);

    assert_eq!(rows.len(), 1);
    let values = extract_values(&rows[0]);
    assert_eq!(values, vec!["true", "true"]);
}

#[test]
fn pg_virtual_aggregates_support_count_distinct() {
    let catalog: Arc<dyn CatalogReader> = Arc::new(crate::EmptyCatalog);
    let stmt = parse_prepared_statement(
        "SELECT count(DISTINCT utc_offset) >= 24 AS ok FROM pg_timezone_names",
    )
    .expect("parse");
    let aiondb_parser::Statement::Select(select) = stmt else {
        panic!("expected SELECT");
    };

    let plan = build_select_plan(&catalog, txn(), &select, None, None, None)
        .expect("plan should succeed")
        .expect("pg_timezone_names should be recognized");
    let (_fields, rows) = unwrap_rows(plan);

    assert_eq!(rows.len(), 1);
    let values = extract_values(&rows[0]);
    assert_eq!(values, vec!["true"]);
}

#[test]
fn pg_virtual_select_fast_path_falls_back_for_unsupported_function_wrappers() {
    let catalog: Arc<dyn CatalogReader> = Arc::new(crate::EmptyCatalog);
    let stmt = parse_prepared_statement(
        "SELECT lower(typname) AS name \
         FROM pg_type \
         WHERE coalesce(typname, '') = 'int4' \
         ORDER BY upper(typname)",
    )
    .expect("parse");
    let aiondb_parser::Statement::Select(select) = stmt else {
        panic!("expected SELECT");
    };

    let plan = build_select_plan(&catalog, txn(), &select, None, None, None)
        .expect("fast-path should defer instead of erroring");
    assert!(
        plan.is_none(),
        "unsupported virtual-query wrappers should fall back to the general binder"
    );
}
