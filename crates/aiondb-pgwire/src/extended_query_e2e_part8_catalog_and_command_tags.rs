use super::*;

#[tokio::test]
async fn extended_describe_statement_preserves_pg_attribute_oid_aliases() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_pg_attribute_oids",
        "SELECT attrelid, atttypid, attcollation FROM pg_catalog.pg_attribute LIMIT 1",
        &[],
    ));
    input.extend(build_describe_bytes(b'S', "s_pg_attribute_oids"));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("describe statement should preserve pg_attribute oid aliases");

    let messages = backend_messages(conn.writer_ref());
    let row_description = messages
        .iter()
        .find(|(tag, _)| *tag == b'T')
        .map(|(_, payload)| payload.as_slice())
        .expect("row description");
    assert_eq!(
        parse_row_description_type_info(row_description),
        vec![(26, -1), (26, -1), (26, -1)]
    );
}

#[tokio::test]
async fn extended_describe_statement_preserves_pg_attrdef_oid_aliases() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_pg_attrdef_oids",
        "SELECT oid, adrelid FROM pg_catalog.pg_attrdef LIMIT 1",
        &[],
    ));
    input.extend(build_describe_bytes(b'S', "s_pg_attrdef_oids"));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("describe statement should preserve pg_attrdef oid aliases");

    let messages = backend_messages(conn.writer_ref());
    let row_description = messages
        .iter()
        .find(|(tag, _)| *tag == b'T')
        .map(|(_, payload)| payload.as_slice())
        .expect("row description");
    assert_eq!(
        parse_row_description_type_info(row_description),
        vec![(26, -1), (26, -1)]
    );
}

#[tokio::test]
async fn extended_describe_statement_preserves_pg_prepared_statements_oid_array_fallback() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_pg_prepared_statement_types",
        "SELECT parameter_types, result_types FROM pg_catalog.pg_prepared_statements LIMIT 1",
        &[],
    ));
    input.extend(build_describe_bytes(b'S', "s_pg_prepared_statement_types"));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("describe statement should preserve pg_prepared_statements oid[] fallback");

    let messages = backend_messages(conn.writer_ref());
    let row_description = messages
        .iter()
        .find(|(tag, _)| *tag == b'T')
        .map(|(_, payload)| payload.as_slice())
        .expect("row description");
    assert_eq!(
        parse_row_description_type_info(row_description),
        vec![(2211, -1), (2211, -1)]
    );
}

#[tokio::test]
async fn extended_describe_statement_preserves_pg_index_int2vector_oid() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_pg_index_indkey",
        "SELECT indexrelid, indrelid, indkey FROM pg_catalog.pg_index LIMIT 1",
        &[],
    ));
    input.extend(build_describe_bytes(b'S', "s_pg_index_indkey"));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("describe statement should preserve pg_index int2vector oid");

    let messages = backend_messages(conn.writer_ref());
    let row_description = messages
        .iter()
        .find(|(tag, _)| *tag == b'T')
        .map(|(_, payload)| payload.as_slice())
        .expect("row description");
    assert_eq!(
        parse_row_description_type_info(row_description),
        vec![(26, -1), (26, -1), (22, -1)]
    );
}

#[tokio::test]
async fn extended_describe_statement_preserves_pg_proc_oidvector_oid() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_pg_proc_proargtypes",
        "SELECT proargtypes FROM pg_catalog.pg_proc ORDER BY oid LIMIT 1",
        &[],
    ));
    input.extend(build_describe_bytes(b'S', "s_pg_proc_proargtypes"));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("describe statement should preserve pg_proc oidvector oid");

    let messages = backend_messages(conn.writer_ref());
    let row_description = messages
        .iter()
        .find(|(tag, _)| *tag == b'T')
        .map(|(_, payload)| payload.as_slice())
        .expect("row description");
    assert_eq!(
        parse_row_description_type_info(row_description),
        vec![(30, -1)]
    );
}

#[tokio::test]
async fn extended_describe_statement_preserves_pg_object_identity_oid_aliases() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_pg_depend_oids",
        "SELECT classid, objid, refclassid, refobjid FROM pg_catalog.pg_depend LIMIT 1",
        &[],
    ));
    input.extend(build_describe_bytes(b'S', "s_pg_depend_oids"));
    input.extend(build_sync_bytes());
    input.extend(build_parse_bytes(
        "s_pg_description_oids",
        "SELECT objoid, classoid FROM pg_catalog.pg_description LIMIT 1",
        &[],
    ));
    input.extend(build_describe_bytes(b'S', "s_pg_description_oids"));
    input.extend(build_sync_bytes());
    input.extend(build_parse_bytes(
        "s_pg_shdescription_oids",
        "SELECT objoid, classoid FROM pg_catalog.pg_shdescription LIMIT 1",
        &[],
    ));
    input.extend(build_describe_bytes(b'S', "s_pg_shdescription_oids"));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("describe statement should preserve pg object identity oid aliases");

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
            vec![(26, -1), (26, -1)],
            vec![(26, -1), (26, -1)],
        ]
    );
}

#[tokio::test]
async fn extended_describe_statement_preserves_pg_constraint_oid_aliases() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_pg_constraint_oids",
        "SELECT oid, connamespace, conrelid, contypid, conindid, confrelid \
         FROM pg_catalog.pg_constraint LIMIT 1",
        &[],
    ));
    input.extend(build_describe_bytes(b'S', "s_pg_constraint_oids"));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("describe statement should preserve pg_constraint oid aliases");

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

#[tokio::test]
async fn extended_execute_real_insert_reports_rows_affected() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));
    let (session, _) = engine
        .startup(StartupParams {
            database: "default".to_owned(),
            application_name: Some("pgwire-e2e-insert-count-seed".to_owned()),
            options: std::collections::BTreeMap::new(),
            credential: Credential::Anonymous {
                user: "test".to_owned(),
            },
            transport: TransportInfo::in_process(),
        })
        .expect("startup seed session");
    engine
        .execute_sql(&session, "CREATE TABLE t_cmd_insert (id INT)")
        .expect("create insert target table");

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_insert_count",
        "INSERT INTO t_cmd_insert VALUES (1)",
        &[],
    ));
    input.extend(build_bind_bytes(
        "p_insert_count",
        "s_insert_count",
        &[],
        &[],
        &[],
    ));
    input.extend(build_execute_bytes("p_insert_count", 0));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("real extended insert should complete");

    let messages = backend_messages(conn.writer_ref());
    let command = messages
        .iter()
        .rfind(|(tag, _)| *tag == b'C')
        .map(|(_, payload)| parse_cstring_payload(payload))
        .expect("command complete");
    assert_eq!(command, "INSERT 0 1");
}

#[tokio::test]
async fn extended_execute_real_update_reports_rows_affected() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));
    let (session, _) = engine
        .startup(StartupParams {
            database: "default".to_owned(),
            application_name: Some("pgwire-e2e-update-count-seed".to_owned()),
            options: std::collections::BTreeMap::new(),
            credential: Credential::Anonymous {
                user: "test".to_owned(),
            },
            transport: TransportInfo::in_process(),
        })
        .expect("startup seed session");
    engine
        .execute_sql(
            &session,
            "CREATE TABLE t_cmd_update (id INT);
             INSERT INTO t_cmd_update VALUES (1)",
        )
        .expect("seed update target table");

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_update_count",
        "UPDATE t_cmd_update SET id = 2 WHERE id = 1",
        &[],
    ));
    input.extend(build_bind_bytes(
        "p_update_count",
        "s_update_count",
        &[],
        &[],
        &[],
    ));
    input.extend(build_execute_bytes("p_update_count", 0));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("real extended update should complete");

    let messages = backend_messages(conn.writer_ref());
    let command = messages
        .iter()
        .rfind(|(tag, _)| *tag == b'C')
        .map(|(_, payload)| parse_cstring_payload(payload))
        .expect("command complete");
    assert_eq!(command, "UPDATE 1");
}

#[tokio::test]
async fn extended_execute_real_update_zero_reports_zero_row_count() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));
    let (session, _) = engine
        .startup(StartupParams {
            database: "default".to_owned(),
            application_name: Some("pgwire-e2e-update-zero-seed".to_owned()),
            options: std::collections::BTreeMap::new(),
            credential: Credential::Anonymous {
                user: "test".to_owned(),
            },
            transport: TransportInfo::in_process(),
        })
        .expect("startup seed session");
    engine
        .execute_sql(
            &session,
            "CREATE TABLE t_cmd_update_zero (id INT);
             INSERT INTO t_cmd_update_zero VALUES (1)",
        )
        .expect("seed update-zero target table");

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_update_zero",
        "UPDATE t_cmd_update_zero SET id = 2 WHERE id = 999",
        &[],
    ));
    input.extend(build_bind_bytes(
        "p_update_zero",
        "s_update_zero",
        &[],
        &[],
        &[],
    ));
    input.extend(build_execute_bytes("p_update_zero", 0));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("real extended zero-row update should complete");

    let messages = backend_messages(conn.writer_ref());
    let command = messages
        .iter()
        .rfind(|(tag, _)| *tag == b'C')
        .map(|(_, payload)| parse_cstring_payload(payload))
        .expect("command complete");
    assert_eq!(command, "UPDATE 0");
}

#[tokio::test]
async fn extended_execute_real_delete_reports_rows_affected() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));
    let (session, _) = engine
        .startup(StartupParams {
            database: "default".to_owned(),
            application_name: Some("pgwire-e2e-delete-count-seed".to_owned()),
            options: std::collections::BTreeMap::new(),
            credential: Credential::Anonymous {
                user: "test".to_owned(),
            },
            transport: TransportInfo::in_process(),
        })
        .expect("startup seed session");
    engine
        .execute_sql(
            &session,
            "CREATE TABLE t_cmd_delete (id INT);
             INSERT INTO t_cmd_delete VALUES (1)",
        )
        .expect("seed delete target table");

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_delete_count",
        "DELETE FROM t_cmd_delete WHERE id = 1",
        &[],
    ));
    input.extend(build_bind_bytes(
        "p_delete_count",
        "s_delete_count",
        &[],
        &[],
        &[],
    ));
    input.extend(build_execute_bytes("p_delete_count", 0));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("real extended delete should complete");

    let messages = backend_messages(conn.writer_ref());
    let command = messages
        .iter()
        .rfind(|(tag, _)| *tag == b'C')
        .map(|(_, payload)| parse_cstring_payload(payload))
        .expect("command complete");
    assert_eq!(command, "DELETE 1");
}

#[tokio::test]
async fn extended_reexecute_exhausted_portal_reports_select_zero_instead_of_empty() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes("s_once", "SELECT 1", &[]));
    input.extend(build_bind_bytes("p_once", "s_once", &[], &[], &[]));
    input.extend(build_execute_bytes("p_once", 1));
    input.extend(build_execute_bytes("p_once", 1));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("re-executing exhausted portal should succeed");

    let commands: Vec<String> = backend_messages(conn.writer_ref())
        .iter()
        .filter(|(tag, _)| *tag == b'C')
        .map(|(_, payload)| parse_cstring_payload(payload).to_owned())
        .collect();
    assert_eq!(commands, vec!["SELECT 1".to_owned(), "SELECT 0".to_owned()]);
}

#[tokio::test]
async fn extended_reexecute_exhausted_fetch_portal_reports_fetch_zero() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));
    let (session, _) = engine
        .startup(StartupParams {
            database: "default".to_owned(),
            application_name: Some("pgwire-e2e-fetch-exhausted-seed".to_owned()),
            options: std::collections::BTreeMap::new(),
            credential: Credential::Anonymous {
                user: "test".to_owned(),
            },
            transport: TransportInfo::in_process(),
        })
        .expect("startup seed session");
    engine
        .execute_sql(
            &session,
            "CREATE TABLE t_fetch_exhausted (id INT);
             INSERT INTO t_fetch_exhausted VALUES (1), (2)",
        )
        .expect("seed fetch table");

    let mut input = build_startup_bytes();
    input.extend(build_query_bytes("BEGIN"));
    input.extend(build_query_bytes(
        "DECLARE c CURSOR FOR SELECT id FROM t_fetch_exhausted ORDER BY id",
    ));
    input.extend(build_parse_bytes("s_fetch_once", "FETCH ALL IN c", &[]));
    input.extend(build_bind_bytes(
        "p_fetch_once",
        "s_fetch_once",
        &[],
        &[],
        &[],
    ));
    input.extend(build_execute_bytes("p_fetch_once", 0));
    input.extend(build_execute_bytes("p_fetch_once", 0));
    input.extend(build_sync_bytes());
    input.extend(build_query_bytes("COMMIT"));
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("re-executing exhausted fetch portal should succeed");

    let commands: Vec<String> = backend_messages(conn.writer_ref())
        .iter()
        .filter(|(tag, _)| *tag == b'C')
        .map(|(_, payload)| parse_cstring_payload(payload).to_owned())
        .collect();
    assert!(
        commands
            .windows(2)
            .any(|window| window == ["FETCH 2", "FETCH 0"]),
        "expected FETCH 2 then FETCH 0 command completes, got {commands:?}"
    );
}

#[tokio::test]
async fn extended_execute_exhausted_portal_reports_select_zero_not_empty() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));
    let (session, _) = engine
        .startup(StartupParams {
            database: "default".to_owned(),
            application_name: Some("pgwire-e2e-empty-sentinel-seed".to_owned()),
            options: std::collections::BTreeMap::new(),
            credential: Credential::Anonymous {
                user: "test".to_owned(),
            },
            transport: TransportInfo::in_process(),
        })
        .expect("startup seed session");
    engine
        .execute_sql(
            &session,
            "CREATE TABLE t_paged (id INT);
             INSERT INTO t_paged VALUES (1), (2), (3)",
        )
        .expect("seed paged table");

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_paged",
        "SELECT id FROM t_paged ORDER BY id",
        &[],
    ));
    input.extend(build_bind_bytes("p_paged", "s_paged", &[], &[], &[]));
    input.extend(build_execute_bytes("p_paged", 2));
    input.extend(build_execute_bytes("p_paged", 2));
    input.extend(build_execute_bytes("p_paged", 2));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("re-executing exhausted portal should succeed");

    let commands: Vec<String> = backend_messages(conn.writer_ref())
        .iter()
        .filter(|(tag, _)| *tag == b'C')
        .map(|(_, payload)| parse_cstring_payload(payload).to_owned())
        .collect();
    assert_eq!(commands, vec!["SELECT 3".to_owned(), "SELECT 0".to_owned()]);
}
