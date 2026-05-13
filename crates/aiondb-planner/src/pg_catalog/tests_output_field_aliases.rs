use super::*;

#[test]
fn output_fields_for_pg_acl_and_options_catalogs_use_text_arrays() {
    let database_fields = output_fields_for("pg_database").expect("pg_database fields");
    assert_eq!(database_fields[16].name, "datacl");
    assert_eq!(
        database_fields[16].data_type,
        DataType::Array(Box::new(DataType::Text))
    );

    let language_fields = output_fields_for("pg_language").expect("pg_language fields");
    assert_eq!(language_fields[8].name, "lanacl");
    assert_eq!(
        language_fields[8].data_type,
        DataType::Array(Box::new(DataType::Text))
    );

    let tablespace_fields = output_fields_for("pg_tablespace").expect("pg_tablespace fields");
    assert_eq!(tablespace_fields[3].name, "spcacl");
    assert_eq!(
        tablespace_fields[3].data_type,
        DataType::Array(Box::new(DataType::Text))
    );
    assert_eq!(tablespace_fields[4].name, "spcoptions");
    assert_eq!(
        tablespace_fields[4].data_type,
        DataType::Array(Box::new(DataType::Text))
    );

    let event_trigger_fields =
        output_fields_for("pg_event_trigger").expect("pg_event_trigger fields");
    assert_eq!(
        event_trigger_fields[0].text_type_modifier,
        Some(TextTypeModifier::Oid)
    );
    assert_eq!(
        event_trigger_fields[3].text_type_modifier,
        Some(TextTypeModifier::Oid)
    );
    assert_eq!(
        event_trigger_fields[4].text_type_modifier,
        Some(TextTypeModifier::Oid)
    );
    assert_eq!(event_trigger_fields[6].name, "evttags");
    assert_eq!(
        event_trigger_fields[6].data_type,
        DataType::Array(Box::new(DataType::Text))
    );

    let foreign_server_fields =
        output_fields_for("pg_foreign_server").expect("pg_foreign_server fields");
    assert_eq!(
        foreign_server_fields[0].text_type_modifier,
        Some(TextTypeModifier::Oid)
    );
    assert_eq!(
        foreign_server_fields[2].text_type_modifier,
        Some(TextTypeModifier::Oid)
    );
    assert_eq!(
        foreign_server_fields[3].text_type_modifier,
        Some(TextTypeModifier::Oid)
    );
    assert_eq!(foreign_server_fields[6].name, "srvacl");
    assert_eq!(
        foreign_server_fields[6].data_type,
        DataType::Array(Box::new(DataType::Text))
    );
    assert_eq!(foreign_server_fields[7].name, "srvoptions");
    assert_eq!(
        foreign_server_fields[7].data_type,
        DataType::Array(Box::new(DataType::Text))
    );

    let fdw_fields =
        output_fields_for("pg_foreign_data_wrapper").expect("pg_foreign_data_wrapper fields");
    assert_eq!(
        fdw_fields[0].text_type_modifier,
        Some(TextTypeModifier::Oid)
    );
    assert_eq!(
        fdw_fields[2].text_type_modifier,
        Some(TextTypeModifier::Oid)
    );
    assert_eq!(
        fdw_fields[3].text_type_modifier,
        Some(TextTypeModifier::Oid)
    );
    assert_eq!(
        fdw_fields[4].text_type_modifier,
        Some(TextTypeModifier::Oid)
    );
    assert_eq!(fdw_fields[5].name, "fdwacl");
    assert_eq!(
        fdw_fields[5].data_type,
        DataType::Array(Box::new(DataType::Text))
    );
    assert_eq!(fdw_fields[6].name, "fdwoptions");
    assert_eq!(
        fdw_fields[6].data_type,
        DataType::Array(Box::new(DataType::Text))
    );

    let extension_fields = output_fields_for("pg_extension").expect("pg_extension fields");
    assert_eq!(
        extension_fields[0].text_type_modifier,
        Some(TextTypeModifier::Oid)
    );
    assert_eq!(
        extension_fields[2].text_type_modifier,
        Some(TextTypeModifier::Oid)
    );
    assert_eq!(
        extension_fields[3].text_type_modifier,
        Some(TextTypeModifier::Oid)
    );
    assert_eq!(extension_fields[6].name, "extconfig");
    assert_eq!(
        extension_fields[6].data_type,
        DataType::Array(Box::new(DataType::Text))
    );
    assert_eq!(extension_fields[7].name, "extcondition");
    assert_eq!(
        extension_fields[7].data_type,
        DataType::Array(Box::new(DataType::Text))
    );
}

#[test]
fn output_fields_for_secondary_catalogs_use_oid_aliases() {
    let cast_fields = output_fields_for("pg_cast").expect("pg_cast fields");
    assert_eq!(
        cast_fields[0].text_type_modifier,
        Some(TextTypeModifier::Oid)
    );
    assert_eq!(
        cast_fields[1].text_type_modifier,
        Some(TextTypeModifier::Oid)
    );
    assert_eq!(
        cast_fields[2].text_type_modifier,
        Some(TextTypeModifier::Oid)
    );
    assert_eq!(
        cast_fields[3].text_type_modifier,
        Some(TextTypeModifier::Oid)
    );

    let aggregate_fields = output_fields_for("pg_aggregate").expect("pg_aggregate fields");
    for index in [0_usize, 3, 4, 5, 6, 7, 8, 9, 10, 15, 16, 18] {
        assert_eq!(
            aggregate_fields[index].text_type_modifier,
            Some(TextTypeModifier::Oid)
        );
    }

    let amop_fields = output_fields_for("pg_amop").expect("pg_amop fields");
    for index in [0_usize, 1, 2, 3, 6, 7, 8] {
        assert_eq!(
            amop_fields[index].text_type_modifier,
            Some(TextTypeModifier::Oid)
        );
    }

    let amproc_fields = output_fields_for("pg_amproc").expect("pg_amproc fields");
    for index in [0_usize, 1, 2, 3, 5] {
        assert_eq!(
            amproc_fields[index].text_type_modifier,
            Some(TextTypeModifier::Oid)
        );
    }

    let rewrite_fields = output_fields_for("pg_rewrite").expect("pg_rewrite fields");
    assert_eq!(
        rewrite_fields[0].text_type_modifier,
        Some(TextTypeModifier::Oid)
    );
    assert_eq!(
        rewrite_fields[2].text_type_modifier,
        Some(TextTypeModifier::Oid)
    );

    let collation_fields = output_fields_for("pg_collation").expect("pg_collation fields");
    assert_eq!(
        collation_fields[0].text_type_modifier,
        Some(TextTypeModifier::Oid)
    );
    assert_eq!(
        collation_fields[2].text_type_modifier,
        Some(TextTypeModifier::Oid)
    );
    assert_eq!(
        collation_fields[3].text_type_modifier,
        Some(TextTypeModifier::Oid)
    );

    let tablespace_fields = output_fields_for("pg_tablespace").expect("pg_tablespace fields");
    assert_eq!(
        tablespace_fields[0].text_type_modifier,
        Some(TextTypeModifier::Oid)
    );
    assert_eq!(
        tablespace_fields[2].text_type_modifier,
        Some(TextTypeModifier::Oid)
    );

    let range_fields = output_fields_for("pg_range").expect("pg_range fields");
    for field in range_fields.iter().take(7) {
        assert_eq!(field.text_type_modifier, Some(TextTypeModifier::Oid));
    }

    let enum_fields = output_fields_for("pg_enum").expect("pg_enum fields");
    assert_eq!(
        enum_fields[0].text_type_modifier,
        Some(TextTypeModifier::Oid)
    );
    assert_eq!(
        enum_fields[1].text_type_modifier,
        Some(TextTypeModifier::Oid)
    );

    let inherits_fields = output_fields_for("pg_inherits").expect("pg_inherits fields");
    assert_eq!(
        inherits_fields[0].text_type_modifier,
        Some(TextTypeModifier::Oid)
    );
    assert_eq!(
        inherits_fields[1].text_type_modifier,
        Some(TextTypeModifier::Oid)
    );

    let policy_fields = output_fields_for("pg_policy").expect("pg_policy fields");
    assert_eq!(
        policy_fields[0].text_type_modifier,
        Some(TextTypeModifier::Oid)
    );
    assert_eq!(
        policy_fields[2].text_type_modifier,
        Some(TextTypeModifier::Oid)
    );
    assert_eq!(
        policy_fields[5].data_type,
        DataType::Array(Box::new(DataType::Int))
    );
    assert_eq!(
        policy_fields[5].text_type_modifier,
        Some(TextTypeModifier::Oid)
    );

    let sequence_fields = output_fields_for("pg_sequence").expect("pg_sequence fields");
    assert_eq!(
        sequence_fields[0].text_type_modifier,
        Some(TextTypeModifier::Oid)
    );
    assert_eq!(
        sequence_fields[1].text_type_modifier,
        Some(TextTypeModifier::Oid)
    );
}

#[test]
fn output_fields_for_supporting_catalogs_use_oid_aliases() {
    let am_fields = output_fields_for("pg_am").expect("pg_am fields");
    assert_eq!(am_fields[0].text_type_modifier, Some(TextTypeModifier::Oid));

    let stat_all_tables_fields =
        output_fields_for("pg_stat_all_tables").expect("pg_stat_all_tables fields");
    assert_eq!(
        stat_all_tables_fields[0].text_type_modifier,
        Some(TextTypeModifier::Oid)
    );

    let auth_members_fields = output_fields_for("pg_auth_members").expect("pg_auth_members fields");
    for field in auth_members_fields.iter().take(4) {
        assert_eq!(field.text_type_modifier, Some(TextTypeModifier::Oid));
    }

    let ts_config_fields = output_fields_for("pg_ts_config").expect("pg_ts_config fields");
    for index in [0_usize, 2, 3, 4] {
        assert_eq!(
            ts_config_fields[index].text_type_modifier,
            Some(TextTypeModifier::Oid)
        );
    }

    let ts_dict_fields = output_fields_for("pg_ts_dict").expect("pg_ts_dict fields");
    for index in [0_usize, 2, 3, 4] {
        assert_eq!(
            ts_dict_fields[index].text_type_modifier,
            Some(TextTypeModifier::Oid)
        );
    }

    let ts_parser_fields = output_fields_for("pg_ts_parser").expect("pg_ts_parser fields");
    for index in [0_usize, 2, 3, 4, 5, 6, 7] {
        assert_eq!(
            ts_parser_fields[index].text_type_modifier,
            Some(TextTypeModifier::Oid)
        );
    }
}

#[test]
fn output_fields_for_pg_operator_use_oid_aliases() {
    let fields = output_fields_for("pg_operator").expect("pg_operator fields");
    for index in [0_usize, 2, 3, 7, 8, 9, 10, 11, 12] {
        assert_eq!(
            fields[index].text_type_modifier,
            Some(TextTypeModifier::Oid)
        );
    }
    assert_eq!(fields[1].text_type_modifier, Some(TextTypeModifier::Name));
    assert_eq!(
        fields[4].text_type_modifier,
        Some(TextTypeModifier::InternalChar)
    );
}

#[test]
fn output_fields_for_pg_statistic_use_oid_and_real_array_metadata() {
    let fields = output_fields_for("pg_statistic").expect("pg_statistic fields");
    assert_eq!(fields[0].name, "starelid");
    assert_eq!(fields[0].text_type_modifier, Some(TextTypeModifier::Oid));
    assert_eq!(fields[11].name, "staop1");
    assert_eq!(fields[11].text_type_modifier, Some(TextTypeModifier::Oid));
    assert_eq!(fields[16].name, "stacoll1");
    assert_eq!(fields[16].text_type_modifier, Some(TextTypeModifier::Oid));
    assert_eq!(fields[21].name, "stanumbers1");
    assert_eq!(
        fields[21].data_type,
        DataType::Array(Box::new(DataType::Real))
    );
    assert_eq!(fields[25].name, "stanumbers5");
    assert_eq!(
        fields[25].data_type,
        DataType::Array(Box::new(DataType::Real))
    );
}

#[test]
fn output_fields_for_pg_locks_use_oid_aliases_for_object_identity_columns() {
    let fields = output_fields_for("pg_locks").expect("pg_locks fields");
    assert_eq!(fields[1].name, "database");
    assert_eq!(fields[1].text_type_modifier, Some(TextTypeModifier::Oid));
    assert_eq!(fields[2].name, "relation");
    assert_eq!(fields[2].text_type_modifier, Some(TextTypeModifier::Oid));
    assert_eq!(fields[7].name, "classid");
    assert_eq!(fields[7].text_type_modifier, Some(TextTypeModifier::Oid));
    assert_eq!(fields[8].name, "objid");
    assert_eq!(fields[8].text_type_modifier, Some(TextTypeModifier::Oid));
}

#[test]
fn output_fields_for_pg_stat_activity_use_oid_alias_for_usesysid() {
    let fields = output_fields_for("pg_stat_activity").expect("pg_stat_activity fields");
    assert_eq!(fields[1].name, "usesysid");
    assert_eq!(fields[1].text_type_modifier, Some(TextTypeModifier::Oid));
}

#[test]
fn output_fields_for_pg_prepared_statements_use_oid_arrays_for_type_lists() {
    let fields =
        output_fields_for("pg_prepared_statements").expect("pg_prepared_statements fields");
    assert_eq!(fields[3].name, "parameter_types");
    assert_eq!(
        fields[3].data_type,
        DataType::Array(Box::new(DataType::Int))
    );
    assert_eq!(
        fields[3].text_type_modifier,
        Some(TextTypeModifier::RegType)
    );
    assert_eq!(fields[4].name, "result_types");
    assert_eq!(
        fields[4].data_type,
        DataType::Array(Box::new(DataType::Int))
    );
    assert_eq!(
        fields[4].text_type_modifier,
        Some(TextTypeModifier::RegType)
    );
}

#[test]
fn output_fields_for_unknown_returns_none() {
    assert!(output_fields_for("nonexistent").is_none());
    assert!(output_fields_for("pg_nosuch").is_none());
}

// ---------------------------------------------------------------
// build_plan - pg_namespace
// ---------------------------------------------------------------

#[test]
fn pg_namespace_contains_builtin_schemas() {
    let catalog = mock_catalog();
    let plan = build_plan(&catalog, txn(), "pg_namespace", None, None, None)
        .expect("ok")
        .expect("should be Some");

    let (fields, rows) = unwrap_rows(plan);
    // 4 columns with `nspacl` included (oid, nspname, nspowner, nspacl).
    assert_eq!(fields.len(), 4);

    let names: Vec<String> = rows.iter().map(|r| extract_values(r)[1].clone()).collect();
    assert!(names.contains(&"public".to_owned()));
    assert!(names.contains(&"pg_catalog".to_owned()));
    assert!(names.contains(&"information_schema".to_owned()));
}

// ---------------------------------------------------------------
// build_plan - pg_class
// ---------------------------------------------------------------

#[test]
fn pg_class_lists_tables_and_indexes() {
    let catalog = mock_catalog();
    let plan = build_plan(&catalog, txn(), "pg_class", None, None, None)
        .expect("ok")
        .expect("should be Some");

    let (_fields, rows) = unwrap_rows(plan);

    let names: Vec<String> = rows.iter().map(|r| extract_values(r)[1].clone()).collect();
    assert!(names.contains(&"users".to_owned()));
    assert!(names.contains(&"users_pkey".to_owned()));
    assert!(names.contains(&"user_ids_seq".to_owned()));

    // Check relkind values include user table ('r'), index ('i'), and sequence ('S')
    let kinds: Vec<String> = rows.iter().map(|r| extract_values(r)[3].clone()).collect();
    assert!(kinds.contains(&"r".to_owned())); // table
    assert!(kinds.contains(&"i".to_owned())); // index
    assert!(kinds.contains(&"S".to_owned())); // sequence

    // Rows include the user table, its index, and well-known system catalog tables
    assert!(rows.len() > 2, "should include system catalog tables too");
}

#[test]
fn pg_sequence_lists_user_sequences() {
    let catalog = mock_catalog();
    let plan = build_plan(&catalog, txn(), "pg_sequence", None, None, None)
        .expect("ok")
        .expect("should be Some");

    let (_fields, rows) = unwrap_rows(plan);
    assert_eq!(rows.len(), 1);
    let values = extract_values(&rows[0]);
    assert_eq!(values[2], "1");
    assert_eq!(values[3], "1");
    assert_eq!(values[6], "1");
    assert_eq!(values[7], "false");
}

#[test]
fn pg_class_marks_sidecar_backed_table_as_matview() {
    let catalog = matview_catalog();
    let plan = build_plan(&catalog, txn(), "pg_class", None, None, None)
        .expect("ok")
        .expect("should be Some");

    let (_fields, rows) = unwrap_rows(plan);

    // Find the sales_snapshot row among all rows (which include system catalog tables)
    let snapshot_row = rows
        .iter()
        .find(|r| extract_values(r)[1] == "sales_snapshot")
        .expect("sales_snapshot should appear in pg_class");
    let values = extract_values(snapshot_row);
    assert_eq!(values[3], "m");
    assert_eq!(values[21], "false");

    // The sidecar view itself should not appear
    assert!(
        !rows
            .iter()
            .any(|r| extract_values(r)[1] == "__aiondb_matview_sales_snapshot"),
        "sidecar view should be hidden from pg_class"
    );
}

// ---------------------------------------------------------------
// build_plan - pg_attribute
// ---------------------------------------------------------------

#[test]
fn pg_attribute_lists_columns() {
    let catalog = mock_catalog();
    let plan = build_plan(&catalog, txn(), "pg_attribute", None, None, None)
        .expect("ok")
        .expect("should be Some");

    let (_fields, rows) = unwrap_rows(plan);

    // "users" has 2 columns
    assert_eq!(rows.len(), 2);

    let id_vals = extract_values(&rows[0]);
    assert_eq!(id_vals[1], "id"); // attname
    assert_eq!(id_vals[2], "23"); // atttypid (int4)
    assert_eq!(id_vals[3], "1"); // attnum
    assert_eq!(id_vals[4], "true"); // attnotnull
    assert_eq!(id_vals[5], "false"); // attisdropped

    let name_vals = extract_values(&rows[1]);
    assert_eq!(name_vals[1], "name"); // attname
    assert_eq!(name_vals[2], "25"); // atttypid (text)
    assert_eq!(name_vals[3], "2"); // attnum
    assert_eq!(name_vals[4], "false"); // attnotnull (nullable)
}

// ---------------------------------------------------------------
// build_plan - pg_type
// ---------------------------------------------------------------

#[test]
fn pg_type_contains_standard_types() {
    let catalog = mock_catalog();
    let plan = build_plan(&catalog, txn(), "pg_type", None, None, None)
        .expect("ok")
        .expect("should be Some");

    let (_fields, rows) = unwrap_rows(plan);

    let type_names: Vec<String> = rows.iter().map(|r| extract_values(r)[1].clone()).collect();

    assert!(type_names.contains(&"int4".to_owned()));
    assert!(type_names.contains(&"int8".to_owned()));
    assert!(type_names.contains(&"text".to_owned()));
    assert!(type_names.contains(&"bool".to_owned()));
    assert!(type_names.contains(&"float4".to_owned()));
    assert!(type_names.contains(&"float8".to_owned()));
    assert!(type_names.contains(&"numeric".to_owned()));
    assert!(type_names.contains(&"timestamp".to_owned()));
    assert!(type_names.contains(&"date".to_owned()));
    assert!(type_names.contains(&"uuid".to_owned()));
    assert!(type_names.contains(&"bytea".to_owned()));
}

#[test]
fn pg_type_oids_match_data_type_pg_oid() {
    let catalog = mock_catalog();
    let plan = build_plan(&catalog, txn(), "pg_type", None, None, None)
        .expect("ok")
        .expect("should be Some");

    let (_fields, rows) = unwrap_rows(plan);

    // Find int4 and verify its OID
    let int4_row = rows
        .iter()
        .find(|r| extract_values(r)[1] == "int4")
        .expect("int4 should exist");
    assert_eq!(extract_values(int4_row)[0], "23");

    // Find text and verify its OID
    let text_row = rows
        .iter()
        .find(|r| extract_values(r)[1] == "text")
        .expect("text should exist");
    assert_eq!(extract_values(text_row)[0], "25");
}

// ---------------------------------------------------------------
// build_plan - pg_index
// ---------------------------------------------------------------
