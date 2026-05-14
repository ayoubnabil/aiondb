#[path = "extended_query_e2e_part8_catalog_and_command_tags.rs"]
mod catalog_and_command_tags;

#[tokio::test]
async fn extended_describe_statement_preserves_pg_extension_array_oids() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_pg_extension_arrays",
        "SELECT extconfig, extcondition FROM pg_catalog.pg_extension LIMIT 1",
        &[],
    ));
    input.extend(build_describe_bytes(b'S', "s_pg_extension_arrays"));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("describe statement should preserve pg_extension array oids");

    let messages = backend_messages(conn.writer_ref());
    let row_description = messages
        .iter()
        .find(|(tag, _)| *tag == b'T')
        .map(|(_, payload)| payload.as_slice())
        .expect("row description");
    assert_eq!(
        parse_row_description_type_info(row_description),
        vec![(1009, -1), (1009, -1)]
    );
}

#[tokio::test]
async fn extended_describe_statement_preserves_pg_trigger_tgargs_bytea_oid() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_pg_trigger_tgargs",
        "SELECT tgargs FROM pg_catalog.pg_trigger LIMIT 1",
        &[],
    ));
    input.extend(build_describe_bytes(b'S', "s_pg_trigger_tgargs"));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("describe statement should preserve pg_trigger tgargs bytea oid");

    let messages = backend_messages(conn.writer_ref());
    let row_description = messages
        .iter()
        .find(|(tag, _)| *tag == b'T')
        .map(|(_, payload)| payload.as_slice())
        .expect("row description");
    assert_eq!(
        parse_row_description_type_info(row_description),
        vec![(17, -1)]
    );
}

#[tokio::test]
async fn extended_describe_statement_preserves_secondary_catalog_oid_aliases() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_pg_trigger_oids",
        "SELECT oid, tgrelid, tgparentid, tgfoid, tgconstrrelid, tgconstrindid, tgconstraint \
         FROM pg_catalog.pg_trigger LIMIT 1",
        &[],
    ));
    input.extend(build_describe_bytes(b'S', "s_pg_trigger_oids"));
    input.extend(build_sync_bytes());

    input.extend(build_parse_bytes(
        "s_pg_rewrite_oids",
        "SELECT oid, ev_class FROM pg_catalog.pg_rewrite LIMIT 1",
        &[],
    ));
    input.extend(build_describe_bytes(b'S', "s_pg_rewrite_oids"));
    input.extend(build_sync_bytes());

    input.extend(build_parse_bytes(
        "s_pg_inherits_oids",
        "SELECT inhrelid, inhparent FROM pg_catalog.pg_inherits LIMIT 1",
        &[],
    ));
    input.extend(build_describe_bytes(b'S', "s_pg_inherits_oids"));
    input.extend(build_sync_bytes());

    input.extend(build_parse_bytes(
        "s_pg_extension_oids",
        "SELECT oid, extowner, extnamespace FROM pg_catalog.pg_extension LIMIT 1",
        &[],
    ));
    input.extend(build_describe_bytes(b'S', "s_pg_extension_oids"));
    input.extend(build_sync_bytes());

    input.extend(build_parse_bytes(
        "s_pg_event_trigger_oids",
        "SELECT oid, evtowner, evtfoid FROM pg_catalog.pg_event_trigger LIMIT 1",
        &[],
    ));
    input.extend(build_describe_bytes(b'S', "s_pg_event_trigger_oids"));
    input.extend(build_sync_bytes());

    input.extend(build_parse_bytes(
        "s_pg_foreign_server_oids",
        "SELECT oid, srvowner, srvfdw FROM pg_catalog.pg_foreign_server LIMIT 1",
        &[],
    ));
    input.extend(build_describe_bytes(b'S', "s_pg_foreign_server_oids"));
    input.extend(build_sync_bytes());

    input.extend(build_parse_bytes(
        "s_pg_fdw_oids",
        "SELECT oid, fdwowner, fdwhandler, fdwvalidator FROM pg_catalog.pg_foreign_data_wrapper LIMIT 1",
        &[],
    ));
    input.extend(build_describe_bytes(b'S', "s_pg_fdw_oids"));
    input.extend(build_sync_bytes());

    input.extend(build_parse_bytes(
        "s_pg_policy_oids",
        "SELECT oid, polrelid FROM pg_catalog.pg_policy LIMIT 1",
        &[],
    ));
    input.extend(build_describe_bytes(b'S', "s_pg_policy_oids"));
    input.extend(build_sync_bytes());

    input.extend(build_parse_bytes(
        "s_pg_sequence_oids",
        "SELECT seqrelid, seqtypid FROM pg_catalog.pg_sequence LIMIT 1",
        &[],
    ));
    input.extend(build_describe_bytes(b'S', "s_pg_sequence_oids"));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("describe statement should preserve secondary catalog oid aliases");

    let messages = backend_messages(conn.writer_ref());
    let row_descriptions: Vec<_> = messages
        .iter()
        .filter(|(tag, _)| *tag == b'T')
        .map(|(_, payload)| parse_row_description_type_info(payload))
        .collect();
    assert_eq!(
        row_descriptions,
        vec![
            vec![
                (26, -1),
                (26, -1),
                (26, -1),
                (26, -1),
                (26, -1),
                (26, -1),
                (26, -1)
            ],
            vec![(26, -1), (26, -1)],
            vec![(26, -1), (26, -1)],
            vec![(26, -1), (26, -1), (26, -1)],
            vec![(26, -1), (26, -1), (26, -1)],
            vec![(26, -1), (26, -1), (26, -1)],
            vec![(26, -1), (26, -1), (26, -1), (26, -1)],
            vec![(26, -1), (26, -1)],
            vec![(26, -1), (26, -1)],
        ]
    );
}

#[tokio::test]
async fn extended_describe_statement_preserves_operator_catalog_oid_aliases() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_pg_cast_oids",
        "SELECT oid, castsource, casttarget, castfunc FROM pg_catalog.pg_cast LIMIT 1",
        &[],
    ));
    input.extend(build_describe_bytes(b'S', "s_pg_cast_oids"));
    input.extend(build_sync_bytes());

    input.extend(build_parse_bytes(
        "s_pg_aggregate_oids",
        "SELECT aggfnoid, aggtransfn, aggfinalfn, aggcombinefn, aggserialfn, aggdeserialfn, \
         aggmtransfn, aggminvtransfn, aggmfinalfn, aggsortop, aggtranstype, aggmtranstype \
         FROM pg_catalog.pg_aggregate LIMIT 1",
        &[],
    ));
    input.extend(build_describe_bytes(b'S', "s_pg_aggregate_oids"));
    input.extend(build_sync_bytes());

    input.extend(build_parse_bytes(
        "s_pg_amop_oids",
        "SELECT oid, amopfamily, amoplefttype, amoprighttype, amopopr, amopmethod, amopsortfamily \
         FROM pg_catalog.pg_amop LIMIT 1",
        &[],
    ));
    input.extend(build_describe_bytes(b'S', "s_pg_amop_oids"));
    input.extend(build_sync_bytes());

    input.extend(build_parse_bytes(
        "s_pg_amproc_oids",
        "SELECT oid, amprocfamily, amproclefttype, amprocrighttype, amproc \
         FROM pg_catalog.pg_amproc LIMIT 1",
        &[],
    ));
    input.extend(build_describe_bytes(b'S', "s_pg_amproc_oids"));
    input.extend(build_sync_bytes());

    input.extend(build_parse_bytes(
        "s_pg_opclass_oids",
        "SELECT oid, opcmethod, opcnamespace, opcowner, opcfamily, opcintype, opckeytype \
         FROM pg_catalog.pg_opclass LIMIT 1",
        &[],
    ));
    input.extend(build_describe_bytes(b'S', "s_pg_opclass_oids"));
    input.extend(build_sync_bytes());

    input.extend(build_parse_bytes(
        "s_pg_opfamily_oids",
        "SELECT oid, opfmethod, opfnamespace, opfowner FROM pg_catalog.pg_opfamily LIMIT 1",
        &[],
    ));
    input.extend(build_describe_bytes(b'S', "s_pg_opfamily_oids"));
    input.extend(build_sync_bytes());

    input.extend(build_parse_bytes(
        "s_pg_conversion_oids",
        "SELECT oid, connamespace, conowner, conproc FROM pg_catalog.pg_conversion LIMIT 1",
        &[],
    ));
    input.extend(build_describe_bytes(b'S', "s_pg_conversion_oids"));
    input.extend(build_sync_bytes());

    input.extend(build_parse_bytes(
        "s_pg_language_oids",
        "SELECT oid, lanowner, lanplcallfoid, laninline, lanvalidator FROM pg_catalog.pg_language LIMIT 1",
        &[],
    ));
    input.extend(build_describe_bytes(b'S', "s_pg_language_oids"));
    input.extend(build_sync_bytes());

    input.extend(build_parse_bytes(
        "s_pg_collation_oids",
        "SELECT oid, collnamespace, collowner FROM pg_catalog.pg_collation LIMIT 1",
        &[],
    ));
    input.extend(build_describe_bytes(b'S', "s_pg_collation_oids"));
    input.extend(build_sync_bytes());

    input.extend(build_parse_bytes(
        "s_pg_tablespace_oids",
        "SELECT oid, spcowner FROM pg_catalog.pg_tablespace LIMIT 1",
        &[],
    ));
    input.extend(build_describe_bytes(b'S', "s_pg_tablespace_oids"));
    input.extend(build_sync_bytes());

    input.extend(build_parse_bytes(
        "s_pg_range_oids",
        "SELECT rngtypid, rngsubtype, rngmultitypid, rngcollation, rngsubopc, rngcanonical, rngsubdiff \
         FROM pg_catalog.pg_range LIMIT 1",
        &[],
    ));
    input.extend(build_describe_bytes(b'S', "s_pg_range_oids"));
    input.extend(build_sync_bytes());

    input.extend(build_parse_bytes(
        "s_pg_enum_oids",
        "SELECT oid, enumtypid FROM pg_catalog.pg_enum LIMIT 1",
        &[],
    ));
    input.extend(build_describe_bytes(b'S', "s_pg_enum_oids"));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("describe statement should preserve operator catalog oid aliases");

    let messages = backend_messages(conn.writer_ref());
    let row_descriptions: Vec<_> = messages
        .iter()
        .filter(|(tag, _)| *tag == b'T')
        .map(|(_, payload)| parse_row_description_type_info(payload))
        .collect();
    assert_eq!(
        row_descriptions,
        vec![
            vec![(26, -1), (26, -1), (26, -1), (26, -1)],
            vec![
                (26, -1),
                (26, -1),
                (26, -1),
                (26, -1),
                (26, -1),
                (26, -1),
                (26, -1),
                (26, -1),
                (26, -1),
                (26, -1),
                (26, -1),
                (26, -1)
            ],
            vec![
                (26, -1),
                (26, -1),
                (26, -1),
                (26, -1),
                (26, -1),
                (26, -1),
                (26, -1)
            ],
            vec![(26, -1), (26, -1), (26, -1), (26, -1), (26, -1)],
            vec![
                (26, -1),
                (26, -1),
                (26, -1),
                (26, -1),
                (26, -1),
                (26, -1),
                (26, -1)
            ],
            vec![(26, -1), (26, -1), (26, -1), (26, -1)],
            vec![(26, -1), (26, -1), (26, -1), (26, -1)],
            vec![(26, -1), (26, -1), (26, -1), (26, -1), (26, -1)],
            vec![(26, -1), (26, -1), (26, -1)],
            vec![(26, -1), (26, -1)],
            vec![
                (26, -1),
                (26, -1),
                (26, -1),
                (26, -1),
                (26, -1),
                (26, -1),
                (26, -1)
            ],
            vec![(26, -1), (26, -1)],
        ]
    );
}

#[tokio::test]
async fn extended_describe_statement_preserves_supporting_catalog_oid_aliases() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_pg_am_oid",
        "SELECT oid FROM pg_catalog.pg_am LIMIT 1",
        &[],
    ));
    input.extend(build_describe_bytes(b'S', "s_pg_am_oid"));
    input.extend(build_sync_bytes());

    input.extend(build_parse_bytes(
        "s_pg_database_oids",
        "SELECT oid, datdba, dattablespace FROM pg_catalog.pg_database LIMIT 1",
        &[],
    ));
    input.extend(build_describe_bytes(b'S', "s_pg_database_oids"));
    input.extend(build_sync_bytes());

    input.extend(build_parse_bytes(
        "s_pg_stat_all_tables_relid",
        "SELECT relid FROM pg_catalog.pg_stat_all_tables LIMIT 1",
        &[],
    ));
    input.extend(build_describe_bytes(b'S', "s_pg_stat_all_tables_relid"));
    input.extend(build_sync_bytes());

    input.extend(build_parse_bytes(
        "s_pg_auth_members_oids",
        "SELECT oid, roleid, member, grantor FROM pg_catalog.pg_auth_members LIMIT 1",
        &[],
    ));
    input.extend(build_describe_bytes(b'S', "s_pg_auth_members_oids"));
    input.extend(build_sync_bytes());

    input.extend(build_parse_bytes(
        "s_pg_ts_config_oids",
        "SELECT oid, cfgnamespace, cfgowner, cfgparser FROM pg_catalog.pg_ts_config LIMIT 1",
        &[],
    ));
    input.extend(build_describe_bytes(b'S', "s_pg_ts_config_oids"));
    input.extend(build_sync_bytes());

    input.extend(build_parse_bytes(
        "s_pg_ts_dict_oids",
        "SELECT oid, dictnamespace, dictowner, dicttemplate FROM pg_catalog.pg_ts_dict LIMIT 1",
        &[],
    ));
    input.extend(build_describe_bytes(b'S', "s_pg_ts_dict_oids"));
    input.extend(build_sync_bytes());

    input.extend(build_parse_bytes(
        "s_pg_ts_parser_oids",
        "SELECT oid, prsnamespace, prsstart, prstoken, prsend, prsheadline, prslextype \
         FROM pg_catalog.pg_ts_parser LIMIT 1",
        &[],
    ));
    input.extend(build_describe_bytes(b'S', "s_pg_ts_parser_oids"));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("describe statement should preserve supporting catalog oid aliases");

    let messages = backend_messages(conn.writer_ref());
    let row_descriptions: Vec<_> = messages
        .iter()
        .filter(|(tag, _)| *tag == b'T')
        .map(|(_, payload)| parse_row_description_type_info(payload))
        .collect();
    assert_eq!(
        row_descriptions,
        vec![
            vec![(26, -1)],
            vec![(26, -1), (26, -1), (26, -1)],
            vec![(26, -1)],
            vec![(26, -1), (26, -1), (26, -1), (26, -1)],
            vec![(26, -1), (26, -1), (26, -1), (26, -1)],
            vec![(26, -1), (26, -1), (26, -1), (26, -1)],
            vec![
                (26, -1),
                (26, -1),
                (26, -1),
                (26, -1),
                (26, -1),
                (26, -1),
                (26, -1)
            ],
        ]
    );
}

#[tokio::test]
async fn extended_describe_statement_preserves_pg_operator_oid_aliases() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_pg_operator_oids",
        "SELECT oid, oprnamespace, oprowner, oprleft, oprright, oprresult, oprcom, oprnegate, oprcode \
         FROM pg_catalog.pg_operator ORDER BY oid LIMIT 1",
        &[],
    ));
    input.extend(build_describe_bytes(b'S', "s_pg_operator_oids"));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("describe statement should preserve pg_operator oid aliases");

    let messages = backend_messages(conn.writer_ref());
    let row_description = messages
        .iter()
        .find(|(tag, _)| *tag == b'T')
        .map(|(_, payload)| payload.as_slice())
        .expect("row description");
    assert_eq!(
        parse_row_description_type_info(row_description),
        vec![
            (26, -1),
            (26, -1),
            (26, -1),
            (26, -1),
            (26, -1),
            (26, -1),
            (26, -1),
            (26, -1),
            (26, -1),
        ]
    );
}

#[tokio::test]
async fn extended_describe_statement_preserves_pg_statistic_oid_and_real_array_oids() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_pg_statistic_metadata",
        "SELECT starelid, staop1, stacoll1, stanumbers1 FROM pg_catalog.pg_statistic LIMIT 1",
        &[],
    ));
    input.extend(build_describe_bytes(b'S', "s_pg_statistic_metadata"));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("describe statement should preserve pg_statistic oid and real[] oids");

    let messages = backend_messages(conn.writer_ref());
    let row_description = messages
        .iter()
        .find(|(tag, _)| *tag == b'T')
        .map(|(_, payload)| payload.as_slice())
        .expect("row description");
    assert_eq!(
        parse_row_description_type_info(row_description),
        vec![(26, -1), (26, -1), (26, -1), (1021, -1)]
    );
}

#[tokio::test]
async fn extended_describe_statement_preserves_pg_locks_oid_aliases() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_pg_locks_oids",
        "SELECT database, relation, classid, objid FROM pg_catalog.pg_locks LIMIT 1",
        &[],
    ));
    input.extend(build_describe_bytes(b'S', "s_pg_locks_oids"));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("describe statement should preserve pg_locks oid aliases");

    let messages = backend_messages(conn.writer_ref());
    let row_description = messages
        .iter()
        .find(|(tag, _)| *tag == b'T')
        .map(|(_, payload)| payload.as_slice())
        .expect("row description");
    assert_eq!(
        parse_row_description_type_info(row_description),
        vec![(26, -1), (26, -1), (26, -1), (26, -1)]
    );
}

#[tokio::test]
async fn extended_describe_statement_preserves_pg_stat_activity_usesysid_oid_alias() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_pg_stat_activity_usesysid",
        "SELECT usesysid FROM pg_catalog.pg_stat_activity LIMIT 1",
        &[],
    ));
    input.extend(build_describe_bytes(b'S', "s_pg_stat_activity_usesysid"));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("describe statement should preserve pg_stat_activity usesysid oid alias");

    let messages = backend_messages(conn.writer_ref());
    let row_description = messages
        .iter()
        .find(|(tag, _)| *tag == b'T')
        .map(|(_, payload)| payload.as_slice())
        .expect("row description");
    assert_eq!(
        parse_row_description_type_info(row_description),
        vec![(26, -1)]
    );
}

#[tokio::test]
async fn extended_describe_statement_preserves_pg_class_oid_aliases() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_pg_class_oids",
        "SELECT oid, relnamespace, relowner, reltablespace, relam, relfilenode \
         FROM pg_catalog.pg_class LIMIT 1",
        &[],
    ));
    input.extend(build_describe_bytes(b'S', "s_pg_class_oids"));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("describe statement should preserve pg_class oid aliases");

    let messages = backend_messages(conn.writer_ref());
    let row_description = messages
        .iter()
        .find(|(tag, _)| *tag == b'T')
        .map(|(_, payload)| payload.as_slice())
        .expect("row description");
    assert_eq!(
        parse_row_description_type_info(row_description),
        vec![(26, -1), (26, -1), (26, -1), (26, -1), (26, -1), (26, -1)]
    );
}
