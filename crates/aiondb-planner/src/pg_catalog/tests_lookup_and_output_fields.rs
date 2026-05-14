use super::*;

// ---------------------------------------------------------------
// is_pg_catalog
// ---------------------------------------------------------------

#[test]
fn is_pg_catalog_lowercase() {
    assert!(is_pg_catalog("pg_catalog"));
}

#[test]
fn is_pg_catalog_mixed_case() {
    assert!(is_pg_catalog("PG_CATALOG"));
    assert!(is_pg_catalog("Pg_Catalog"));
}

#[test]
fn is_pg_catalog_rejects_other() {
    assert!(!is_pg_catalog("public"));
    assert!(!is_pg_catalog("information_schema"));
}

// ---------------------------------------------------------------
// is_pg_catalog_table
// ---------------------------------------------------------------

#[test]
fn recognizes_known_pg_catalog_tables() {
    assert!(is_pg_catalog_table("pg_namespace"));
    assert!(is_pg_catalog_table("pg_class"));
    assert!(is_pg_catalog_table("pg_attribute"));
    assert!(is_pg_catalog_table("pg_type"));
    assert!(is_pg_catalog_table("pg_index"));
    assert!(is_pg_catalog_table("pg_constraint"));
    assert!(is_pg_catalog_table("pg_am"));
    assert!(is_pg_catalog_table("pg_indexes"));
    assert!(is_pg_catalog_table("pg_views"));
    assert!(is_pg_catalog_table("pg_init_privs"));
    assert!(is_pg_catalog_table("pg_config"));
    assert!(is_pg_catalog_table("pg_locks"));
    assert!(is_pg_catalog_table("pg_timezone_names"));
}

#[test]
fn is_pg_catalog_table_case_insensitive() {
    assert!(is_pg_catalog_table("PG_CLASS"));
    assert!(is_pg_catalog_table("Pg_Namespace"));
}

#[test]
fn is_pg_catalog_table_rejects_unknown() {
    assert!(!is_pg_catalog_table("users"));
    assert!(!is_pg_catalog_table("nonexistent"));
}

// ---------------------------------------------------------------
// output_fields_for
// ---------------------------------------------------------------

#[test]
fn output_fields_for_pg_namespace() {
    let fields = output_fields_for("pg_namespace").expect("should be Some");
    assert_eq!(fields.len(), 4);
    assert_eq!(fields[0].name, "oid");
    assert_eq!(fields[0].text_type_modifier, Some(TextTypeModifier::Oid));
    assert_eq!(fields[1].name, "nspname");
    assert_eq!(fields[2].name, "nspowner");
    assert_eq!(fields[2].text_type_modifier, Some(TextTypeModifier::Oid));
    // nspacl covers schema GRANT {USAGE,CREATE} entries.
    assert_eq!(fields[3].name, "nspacl");
}

#[test]
fn output_fields_for_pg_views() {
    let fields = output_fields_for("pg_views").expect("should be Some");
    assert_eq!(fields.len(), 4);
    assert_eq!(fields[0].name, "schemaname");
    assert_eq!(fields[1].name, "viewname");
    assert_eq!(fields[2].name, "viewowner");
    assert_eq!(fields[3].name, "definition");
}

#[test]
fn output_fields_for_pg_class() {
    let fields = output_fields_for("pg_class").expect("should be Some");
    assert_eq!(fields.len(), 33);
    assert_eq!(fields[0].name, "oid");
    assert_eq!(fields[0].text_type_modifier, Some(TextTypeModifier::Oid));
    assert_eq!(fields[1].name, "relname");
    assert_eq!(fields[2].name, "relnamespace");
    assert_eq!(fields[2].text_type_modifier, Some(TextTypeModifier::Oid));
    assert_eq!(fields[3].name, "relkind");
    assert_eq!(fields[4].name, "relowner");
    assert_eq!(fields[4].text_type_modifier, Some(TextTypeModifier::Oid));
    assert_eq!(fields[5].name, "reltuples");
    assert_eq!(fields[5].data_type, DataType::Double);
    assert_eq!(fields[6].name, "relhasindex");
    assert_eq!(fields[6].data_type, DataType::Boolean);
    assert_eq!(fields[7].name, "reltablespace");
    assert_eq!(fields[7].text_type_modifier, Some(TextTypeModifier::Oid));
    assert_eq!(fields[8].name, "relam");
    assert_eq!(fields[8].text_type_modifier, Some(TextTypeModifier::Oid));
    assert_eq!(fields[9].name, "relfilenode");
    assert_eq!(fields[9].text_type_modifier, Some(TextTypeModifier::Oid));
    assert_eq!(fields[10].name, "relpages");
    assert_eq!(fields[21].name, "relispopulated");
    assert_eq!(fields[21].data_type, DataType::Boolean);
    assert_eq!(fields[22].name, "reltoastrelid");
    assert_eq!(fields[22].text_type_modifier, Some(TextTypeModifier::Oid));
    assert_eq!(fields[23].name, "reloptions");
    assert_eq!(
        fields[23].data_type,
        DataType::Array(Box::new(DataType::Text))
    );
    assert_eq!(fields[24].name, "reltype");
    assert_eq!(fields[24].text_type_modifier, Some(TextTypeModifier::Oid));
    assert_eq!(fields[25].name, "reloftype");
    assert_eq!(fields[25].text_type_modifier, Some(TextTypeModifier::Oid));
    assert_eq!(fields[26].name, "relallvisible");
    assert_eq!(fields[27].name, "relreplident");
    assert_eq!(fields[28].name, "relrewrite");
    assert_eq!(fields[28].text_type_modifier, Some(TextTypeModifier::Oid));
    assert_eq!(fields[29].name, "relfrozenxid");
    assert_eq!(fields[30].name, "relminmxid");
    assert_eq!(fields[31].name, "relacl");
    assert_eq!(
        fields[31].data_type,
        DataType::Array(Box::new(DataType::Text))
    );
    assert_eq!(fields[32].name, "relpartbound");
}

#[test]
fn output_fields_for_pg_attribute() {
    let fields = output_fields_for("pg_attribute").expect("should be Some");
    assert_eq!(fields.len(), 19);
    assert_eq!(fields[0].name, "attrelid");
    assert_eq!(fields[0].text_type_modifier, Some(TextTypeModifier::Oid));
    assert_eq!(fields[1].name, "attname");
    assert_eq!(fields[2].name, "atttypid");
    assert_eq!(fields[2].text_type_modifier, Some(TextTypeModifier::Oid));
    assert_eq!(fields[4].name, "attnotnull");
    assert_eq!(fields[4].data_type, DataType::Boolean);
    assert_eq!(fields[10].name, "attcollation");
    assert_eq!(fields[10].text_type_modifier, Some(TextTypeModifier::Oid));
    assert_eq!(fields[18].name, "attacl");
    assert_eq!(
        fields[18].data_type,
        DataType::Array(Box::new(DataType::Text))
    );
    assert!(fields[18].nullable);
}

#[test]
fn output_fields_for_pg_type() {
    let fields = output_fields_for("pg_type").expect("should be Some");
    assert_eq!(fields.len(), 30);
    assert_eq!(fields[0].name, "oid");
    assert_eq!(fields[0].text_type_modifier, Some(TextTypeModifier::Oid));
    assert_eq!(fields[1].name, "typname");
    assert_eq!(fields[1].text_type_modifier, Some(TextTypeModifier::Name));
    assert_eq!(fields[2].name, "typarray");
    assert_eq!(fields[2].text_type_modifier, Some(TextTypeModifier::Oid));
    assert_eq!(fields[3].name, "typnamespace");
    assert_eq!(fields[3].text_type_modifier, Some(TextTypeModifier::Oid));
    assert_eq!(fields[4].name, "typlen");
    assert_eq!(fields[5].name, "typdelim");
    assert_eq!(
        fields[5].text_type_modifier,
        Some(TextTypeModifier::InternalChar)
    );
    assert_eq!(
        fields[6].text_type_modifier,
        Some(TextTypeModifier::InternalChar)
    );
    assert_eq!(fields[7].name, "typbasetype");
    assert_eq!(fields[7].text_type_modifier, Some(TextTypeModifier::Oid));
    assert_eq!(fields[8].name, "typcollation");
    assert_eq!(fields[8].text_type_modifier, Some(TextTypeModifier::Oid));
    assert_eq!(fields[9].name, "typrelid");
    assert_eq!(fields[9].text_type_modifier, Some(TextTypeModifier::Oid));
    assert_eq!(fields[10].name, "typelem");
    assert_eq!(fields[10].text_type_modifier, Some(TextTypeModifier::Oid));
    assert_eq!(fields[17].name, "typinput");
    assert_eq!(
        fields[17].text_type_modifier,
        Some(TextTypeModifier::RegProc)
    );
    assert_eq!(fields[18].name, "typoutput");
    assert_eq!(
        fields[18].text_type_modifier,
        Some(TextTypeModifier::RegProc)
    );
    assert_eq!(fields[19].name, "typreceive");
    assert_eq!(
        fields[19].text_type_modifier,
        Some(TextTypeModifier::RegProc)
    );
    assert_eq!(fields[20].name, "typsend");
    assert_eq!(
        fields[20].text_type_modifier,
        Some(TextTypeModifier::RegProc)
    );
    assert_eq!(fields[21].name, "typmodin");
    assert_eq!(
        fields[21].text_type_modifier,
        Some(TextTypeModifier::RegProc)
    );
    assert_eq!(fields[22].name, "typmodout");
    assert_eq!(
        fields[22].text_type_modifier,
        Some(TextTypeModifier::RegProc)
    );
    assert_eq!(fields[23].name, "typanalyze");
    assert_eq!(
        fields[23].text_type_modifier,
        Some(TextTypeModifier::RegProc)
    );
    assert_eq!(fields[24].name, "typsubscript");
    assert_eq!(
        fields[24].text_type_modifier,
        Some(TextTypeModifier::RegProc)
    );
    assert_eq!(fields[25].name, "typowner");
    assert_eq!(fields[25].text_type_modifier, Some(TextTypeModifier::Oid));
    assert_eq!(fields[26].name, "typalign");
    assert_eq!(
        fields[26].text_type_modifier,
        Some(TextTypeModifier::InternalChar)
    );
    assert_eq!(fields[27].name, "typstorage");
    assert_eq!(
        fields[27].text_type_modifier,
        Some(TextTypeModifier::InternalChar)
    );
    assert_eq!(fields[28].name, "typdefault");
    assert_eq!(fields[29].name, "typacl");
    assert_eq!(
        fields[29].data_type,
        DataType::Array(Box::new(DataType::Text))
    );
    assert!(fields[29].nullable);
}

#[test]
fn output_fields_for_pg_authid_use_oid_alias_on_role_oid() {
    let fields = output_fields_for("pg_authid").expect("pg_authid fields");
    assert_eq!(fields[0].name, "oid");
    assert_eq!(fields[0].text_type_modifier, Some(TextTypeModifier::Oid));
}

#[test]
fn output_fields_for_pg_init_privs() {
    let fields = output_fields_for("pg_init_privs").expect("should be Some");
    assert_eq!(fields.len(), 5);
    assert_eq!(fields[0].name, "objoid");
    assert_eq!(fields[0].text_type_modifier, Some(TextTypeModifier::Oid));
    assert_eq!(fields[1].name, "classoid");
    assert_eq!(fields[1].text_type_modifier, Some(TextTypeModifier::Oid));
    assert_eq!(fields[3].name, "privtype");
    assert_eq!(fields[4].name, "initprivs");
    assert_eq!(
        fields[4].data_type,
        DataType::Array(Box::new(DataType::Text))
    );
}

#[test]
fn output_fields_for_pg_attrdef_use_oid_aliases() {
    let fields = output_fields_for("pg_attrdef").expect("pg_attrdef fields");
    assert_eq!(fields[0].name, "oid");
    assert_eq!(fields[0].text_type_modifier, Some(TextTypeModifier::Oid));
    assert_eq!(fields[1].name, "adrelid");
    assert_eq!(fields[1].text_type_modifier, Some(TextTypeModifier::Oid));
}

#[test]
fn output_fields_for_pg_object_identity_tables_use_oid_aliases() {
    let depend_fields = output_fields_for("pg_depend").expect("pg_depend fields");
    assert_eq!(depend_fields[0].name, "classid");
    assert_eq!(
        depend_fields[0].text_type_modifier,
        Some(TextTypeModifier::Oid)
    );
    assert_eq!(depend_fields[1].name, "objid");
    assert_eq!(
        depend_fields[1].text_type_modifier,
        Some(TextTypeModifier::Oid)
    );
    assert_eq!(depend_fields[3].name, "refclassid");
    assert_eq!(
        depend_fields[3].text_type_modifier,
        Some(TextTypeModifier::Oid)
    );
    assert_eq!(depend_fields[4].name, "refobjid");
    assert_eq!(
        depend_fields[4].text_type_modifier,
        Some(TextTypeModifier::Oid)
    );

    let description_fields = output_fields_for("pg_description").expect("pg_description fields");
    assert_eq!(description_fields[0].name, "objoid");
    assert_eq!(
        description_fields[0].text_type_modifier,
        Some(TextTypeModifier::Oid)
    );
    assert_eq!(description_fields[1].name, "classoid");
    assert_eq!(
        description_fields[1].text_type_modifier,
        Some(TextTypeModifier::Oid)
    );

    let shdescription_fields =
        output_fields_for("pg_shdescription").expect("pg_shdescription fields");
    assert_eq!(shdescription_fields[0].name, "objoid");
    assert_eq!(
        shdescription_fields[0].text_type_modifier,
        Some(TextTypeModifier::Oid)
    );
    assert_eq!(shdescription_fields[1].name, "classoid");
    assert_eq!(
        shdescription_fields[1].text_type_modifier,
        Some(TextTypeModifier::Oid)
    );
}

#[test]
fn output_fields_for_pg_index() {
    let fields = output_fields_for("pg_index").expect("should be Some");
    assert_eq!(fields.len(), 21);
    assert_eq!(fields[0].name, "indexrelid");
    assert_eq!(fields[0].text_type_modifier, Some(TextTypeModifier::Oid));
    assert_eq!(fields[1].name, "indrelid");
    assert_eq!(fields[1].text_type_modifier, Some(TextTypeModifier::Oid));
    assert_eq!(fields[2].name, "indisunique");
    assert_eq!(fields[2].data_type, DataType::Boolean);
    assert_eq!(fields[4].name, "indkey");
    assert_eq!(
        fields[4].data_type,
        DataType::Array(Box::new(DataType::Int))
    );
    assert_eq!(fields[5].name, "indisexclusion");
    assert_eq!(fields[12].name, "indisreplident");
    assert_eq!(fields[13].name, "indnullsnotdistinct");
    assert_eq!(fields[14].name, "indnatts");
    assert_eq!(fields[15].name, "indnkeyatts");
    assert_eq!(fields[16].name, "indcollation");
    assert_eq!(fields[17].name, "indclass");
    assert_eq!(fields[18].name, "indoption");
    assert_eq!(fields[19].name, "indexprs");
    assert_eq!(fields[20].name, "indpred");
}

#[test]
fn output_fields_for_pg_constraint() {
    let fields = output_fields_for("pg_constraint").expect("should be Some");
    assert_eq!(fields.len(), 20);
    assert_eq!(fields[0].name, "oid");
    assert_eq!(fields[0].text_type_modifier, Some(TextTypeModifier::Oid));
    assert_eq!(fields[1].name, "conname");
    assert_eq!(fields[1].text_type_modifier, Some(TextTypeModifier::Name));
    assert_eq!(fields[2].name, "connamespace");
    assert_eq!(fields[2].text_type_modifier, Some(TextTypeModifier::Oid));
    assert_eq!(fields[3].name, "contype");
    assert_eq!(
        fields[3].text_type_modifier,
        Some(TextTypeModifier::InternalChar)
    );
    assert_eq!(fields[4].name, "conrelid");
    assert_eq!(fields[4].text_type_modifier, Some(TextTypeModifier::Oid));
    assert_eq!(fields[5].name, "contypid");
    assert_eq!(fields[5].text_type_modifier, Some(TextTypeModifier::Oid));
    assert_eq!(fields[6].name, "conindid");
    assert_eq!(fields[6].text_type_modifier, Some(TextTypeModifier::Oid));
    assert_eq!(fields[7].name, "conkey");
    assert_eq!(
        fields[7].data_type,
        DataType::Array(Box::new(DataType::Int))
    );
    assert_eq!(fields[8].name, "confrelid");
    assert_eq!(fields[8].text_type_modifier, Some(TextTypeModifier::Oid));
    assert_eq!(fields[9].name, "confkey");
    assert_eq!(
        fields[9].data_type,
        DataType::Array(Box::new(DataType::Int))
    );
    assert_eq!(fields[17].name, "confupdtype");
    assert_eq!(
        fields[17].text_type_modifier,
        Some(TextTypeModifier::InternalChar)
    );
    assert_eq!(fields[18].name, "confdeltype");
    assert_eq!(
        fields[18].text_type_modifier,
        Some(TextTypeModifier::InternalChar)
    );
    assert_eq!(fields[19].name, "confmatchtype");
    assert_eq!(
        fields[19].text_type_modifier,
        Some(TextTypeModifier::InternalChar)
    );
}

#[test]
fn output_fields_for_pg_am_use_name_and_internal_char_aliases() {
    let fields = output_fields_for("pg_am").expect("should be Some");
    assert_eq!(fields[1].name, "amname");
    assert_eq!(fields[1].text_type_modifier, Some(TextTypeModifier::Name));
    assert_eq!(fields[2].name, "amtype");
    assert_eq!(
        fields[2].text_type_modifier,
        Some(TextTypeModifier::InternalChar)
    );
    assert_eq!(fields[3].name, "amhandler");
    assert_eq!(fields[3].text_type_modifier, Some(TextTypeModifier::Oid));
    assert!(fields[3].nullable);
}

#[test]
fn output_fields_for_pg_cast_use_internal_char_aliases() {
    let fields = output_fields_for("pg_cast").expect("should be Some");
    assert_eq!(fields[4].name, "castcontext");
    assert_eq!(
        fields[4].text_type_modifier,
        Some(TextTypeModifier::InternalChar)
    );
    assert_eq!(fields[5].name, "castmethod");
    assert_eq!(
        fields[5].text_type_modifier,
        Some(TextTypeModifier::InternalChar)
    );
}

#[test]
fn output_fields_for_pg_database_use_name_and_internal_char_aliases() {
    let fields = output_fields_for("pg_database").expect("should be Some");
    assert_eq!(fields[0].text_type_modifier, Some(TextTypeModifier::Oid));
    assert_eq!(fields[1].name, "datname");
    assert_eq!(fields[1].text_type_modifier, Some(TextTypeModifier::Name));
    assert_eq!(fields[2].text_type_modifier, Some(TextTypeModifier::Oid));
    assert_eq!(fields[4].name, "datlocprovider");
    assert_eq!(
        fields[4].text_type_modifier,
        Some(TextTypeModifier::InternalChar)
    );
    assert_eq!(fields[10].text_type_modifier, Some(TextTypeModifier::Oid));
}

#[test]
fn output_fields_for_pg_trigger_use_name_and_internal_char_aliases() {
    let fields = output_fields_for("pg_trigger").expect("should be Some");
    assert_eq!(fields[3].name, "tgname");
    assert_eq!(fields[3].text_type_modifier, Some(TextTypeModifier::Name));
    assert_eq!(fields[0].name, "oid");
    assert_eq!(fields[0].text_type_modifier, Some(TextTypeModifier::Oid));
    assert_eq!(fields[1].name, "tgrelid");
    assert_eq!(fields[1].text_type_modifier, Some(TextTypeModifier::Oid));
    assert_eq!(fields[2].name, "tgparentid");
    assert_eq!(fields[2].text_type_modifier, Some(TextTypeModifier::Oid));
    assert_eq!(fields[4].name, "tgfoid");
    assert_eq!(fields[4].text_type_modifier, Some(TextTypeModifier::Oid));
    assert_eq!(fields[6].name, "tgenabled");
    assert_eq!(
        fields[6].text_type_modifier,
        Some(TextTypeModifier::InternalChar)
    );
    assert_eq!(fields[8].name, "tgconstrrelid");
    assert_eq!(fields[8].text_type_modifier, Some(TextTypeModifier::Oid));
    assert_eq!(fields[9].name, "tgconstrindid");
    assert_eq!(fields[9].text_type_modifier, Some(TextTypeModifier::Oid));
    assert_eq!(fields[10].name, "tgconstraint");
    assert_eq!(fields[10].text_type_modifier, Some(TextTypeModifier::Oid));
    assert_eq!(fields[14].name, "tgattr");
    assert_eq!(
        fields[14].text_type_modifier,
        Some(TextTypeModifier::Int2Vector)
    );
    assert_eq!(fields[15].name, "tgargs");
    assert_eq!(fields[15].data_type, DataType::Blob);
}

#[test]
fn output_fields_for_pg_opclass_opfamily_conversion_language_use_name_aliases() {
    let opclass_fields = output_fields_for("pg_opclass").expect("pg_opclass fields");
    assert_eq!(
        opclass_fields[0].text_type_modifier,
        Some(TextTypeModifier::Oid)
    );
    assert_eq!(
        opclass_fields[1].text_type_modifier,
        Some(TextTypeModifier::Oid)
    );
    assert_eq!(opclass_fields[2].name, "opcname");
    assert_eq!(
        opclass_fields[2].text_type_modifier,
        Some(TextTypeModifier::Name)
    );
    assert_eq!(
        opclass_fields[3].text_type_modifier,
        Some(TextTypeModifier::Oid)
    );
    assert_eq!(
        opclass_fields[4].text_type_modifier,
        Some(TextTypeModifier::Oid)
    );
    assert_eq!(
        opclass_fields[5].text_type_modifier,
        Some(TextTypeModifier::Oid)
    );
    assert_eq!(
        opclass_fields[6].text_type_modifier,
        Some(TextTypeModifier::Oid)
    );
    assert_eq!(
        opclass_fields[8].text_type_modifier,
        Some(TextTypeModifier::Oid)
    );

    let opfamily_fields = output_fields_for("pg_opfamily").expect("pg_opfamily fields");
    assert_eq!(
        opfamily_fields[0].text_type_modifier,
        Some(TextTypeModifier::Oid)
    );
    assert_eq!(
        opfamily_fields[1].text_type_modifier,
        Some(TextTypeModifier::Oid)
    );
    assert_eq!(opfamily_fields[2].name, "opfname");
    assert_eq!(
        opfamily_fields[2].text_type_modifier,
        Some(TextTypeModifier::Name)
    );
    assert_eq!(
        opfamily_fields[3].text_type_modifier,
        Some(TextTypeModifier::Oid)
    );
    assert_eq!(
        opfamily_fields[4].text_type_modifier,
        Some(TextTypeModifier::Oid)
    );

    let conversion_fields = output_fields_for("pg_conversion").expect("pg_conversion fields");
    assert_eq!(
        conversion_fields[0].text_type_modifier,
        Some(TextTypeModifier::Oid)
    );
    assert_eq!(conversion_fields[1].name, "conname");
    assert_eq!(
        conversion_fields[1].text_type_modifier,
        Some(TextTypeModifier::Name)
    );
    assert_eq!(
        conversion_fields[2].text_type_modifier,
        Some(TextTypeModifier::Oid)
    );
    assert_eq!(
        conversion_fields[3].text_type_modifier,
        Some(TextTypeModifier::Oid)
    );
    assert_eq!(
        conversion_fields[6].text_type_modifier,
        Some(TextTypeModifier::Oid)
    );

    let language_fields = output_fields_for("pg_language").expect("pg_language fields");
    assert_eq!(
        language_fields[0].text_type_modifier,
        Some(TextTypeModifier::Oid)
    );
    assert_eq!(language_fields[1].name, "lanname");
    assert_eq!(
        language_fields[1].text_type_modifier,
        Some(TextTypeModifier::Name)
    );
    assert_eq!(
        language_fields[2].text_type_modifier,
        Some(TextTypeModifier::Oid)
    );
    assert_eq!(
        language_fields[5].text_type_modifier,
        Some(TextTypeModifier::Oid)
    );
    assert_eq!(
        language_fields[6].text_type_modifier,
        Some(TextTypeModifier::Oid)
    );
    assert_eq!(
        language_fields[7].text_type_modifier,
        Some(TextTypeModifier::Oid)
    );
}

#[test]
fn output_fields_for_pg_proc_use_array_metadata_for_argument_lists() {
    let fields = output_fields_for("pg_proc").expect("should be Some");
    assert_eq!(fields[0].name, "oid");
    assert_eq!(fields[0].text_type_modifier, Some(TextTypeModifier::Oid));
    assert_eq!(fields[2].name, "pronamespace");
    assert_eq!(fields[2].text_type_modifier, Some(TextTypeModifier::Oid));
    assert_eq!(fields[3].name, "proowner");
    assert_eq!(fields[3].text_type_modifier, Some(TextTypeModifier::Oid));
    assert_eq!(fields[4].name, "prolang");
    assert_eq!(fields[4].text_type_modifier, Some(TextTypeModifier::Oid));
    assert_eq!(fields[7].name, "provariadic");
    assert_eq!(fields[7].text_type_modifier, Some(TextTypeModifier::Oid));
    assert_eq!(fields[8].name, "prosupport");
    assert_eq!(
        fields[8].text_type_modifier,
        Some(TextTypeModifier::RegProc)
    );
    assert_eq!(fields[18].name, "prorettype");
    assert_eq!(fields[18].text_type_modifier, Some(TextTypeModifier::Oid));
    assert_eq!(fields[19].name, "proargtypes");
    assert_eq!(
        fields[19].text_type_modifier,
        Some(TextTypeModifier::OidVector)
    );
    assert_eq!(fields[20].name, "proallargtypes");
    assert_eq!(
        fields[20].data_type,
        DataType::Array(Box::new(DataType::Int))
    );
    assert_eq!(fields[20].text_type_modifier, Some(TextTypeModifier::Oid));
    assert_eq!(fields[21].name, "proargmodes");
    assert_eq!(
        fields[21].data_type,
        DataType::Array(Box::new(DataType::Text))
    );
    assert_eq!(
        fields[21].text_type_modifier,
        Some(TextTypeModifier::InternalChar)
    );
    assert_eq!(fields[22].name, "proargnames");
    assert_eq!(
        fields[22].data_type,
        DataType::Array(Box::new(DataType::Text))
    );
    assert_eq!(fields[24].name, "protrftypes");
    assert_eq!(
        fields[24].data_type,
        DataType::Array(Box::new(DataType::Int))
    );
    assert_eq!(fields[24].text_type_modifier, Some(TextTypeModifier::Oid));
    assert_eq!(fields[28].name, "proconfig");
    assert_eq!(
        fields[28].data_type,
        DataType::Array(Box::new(DataType::Text))
    );
    assert_eq!(fields[29].name, "proacl");
    assert_eq!(
        fields[29].data_type,
        DataType::Array(Box::new(DataType::Text))
    );
}

#[test]
fn output_fields_for_pg_publication_catalogs_expose_expected_shapes() {
    let namespace_fields =
        output_fields_for("pg_publication_namespace").expect("pg_publication_namespace fields");
    assert_eq!(
        namespace_fields[0].text_type_modifier,
        Some(TextTypeModifier::Oid)
    );
    assert_eq!(
        namespace_fields[1].text_type_modifier,
        Some(TextTypeModifier::Oid)
    );
    assert_eq!(
        namespace_fields[2].text_type_modifier,
        Some(TextTypeModifier::Oid)
    );

    let rel_fields = output_fields_for("pg_publication_rel").expect("pg_publication_rel fields");
    assert_eq!(
        rel_fields[0].text_type_modifier,
        Some(TextTypeModifier::Oid)
    );
    assert_eq!(
        rel_fields[1].text_type_modifier,
        Some(TextTypeModifier::Oid)
    );
    assert_eq!(
        rel_fields[2].text_type_modifier,
        Some(TextTypeModifier::Oid)
    );
    assert_eq!(
        rel_fields[4].data_type,
        DataType::Array(Box::new(DataType::Int))
    );
    assert_eq!(
        rel_fields[4].text_type_modifier,
        Some(TextTypeModifier::Int2Vector)
    );
}
