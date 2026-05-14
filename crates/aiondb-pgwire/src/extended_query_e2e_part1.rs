#[path = "extended_query_e2e_part1_show_explain_fetch.rs"]
mod show_explain_fetch;

/// Happy path: Parse(SELECT) -> Bind -> Execute -> Sync -> Terminate.
#[tokio::test]
async fn extended_select_parse_bind_execute_sync() {
    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes("", "SELECT 1", &[]));
    input.extend(build_bind_bytes("", "", &[], &[], &[]));
    input.extend(build_execute_bytes("", 0));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let engine = Arc::new(ExtendedMockEngine);
    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    let result = conn.run().await;
    assert!(
        result.is_ok(),
        "extended SELECT should complete without error"
    );
}

/// Happy path: Parse(INSERT) -> Bind -> Execute -> Sync -> Terminate.
#[tokio::test]
async fn extended_insert_parse_bind_execute_sync() {
    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes("", "INSERT INTO t VALUES (1)", &[]));
    input.extend(build_bind_bytes("", "", &[], &[], &[]));
    input.extend(build_execute_bytes("", 0));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let engine = Arc::new(InsertMockEngine);
    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    let result = conn.run().await;
    assert!(
        result.is_ok(),
        "extended INSERT should complete without error"
    );

    let messages = backend_messages(conn.writer_ref());
    let command = messages
        .iter()
        .rfind(|(tag, _)| *tag == b'C')
        .map(|(_, payload)| parse_cstring_payload(payload))
        .expect("command complete");
    assert_eq!(command, "INSERT 0 1");
}

#[tokio::test]
async fn simple_query_insert_returning_reports_insert_tag() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));
    let (session, _) = engine
        .startup(StartupParams {
            database: "default".to_owned(),
            application_name: Some("pgwire-simple-returning-seed".to_owned()),
            options: std::collections::BTreeMap::new(),
            credential: Credential::Anonymous {
                user: "test".to_owned(),
            },
            transport: TransportInfo::in_process(),
        })
        .expect("startup seed session");
    engine
        .execute_sql(&session, "CREATE TABLE t_ret_simple (id INT)")
        .expect("create simple returning table");

    let mut input = build_startup_bytes();
    input.extend(build_query_bytes(
        "INSERT INTO t_ret_simple VALUES (1) RETURNING id",
    ));
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("simple INSERT RETURNING should complete");

    let messages = backend_messages(conn.writer_ref());
    let data_row = messages
        .iter()
        .find(|(tag, _)| *tag == b'D')
        .map(|(_, payload)| payload.as_slice())
        .expect("data row");
    assert_eq!(parse_data_row_columns(data_row), vec![Some(b"1".to_vec())]);
    let command = messages
        .iter()
        .rfind(|(tag, _)| *tag == b'C')
        .map(|(_, payload)| parse_cstring_payload(payload))
        .expect("command complete");
    assert_eq!(command, "INSERT 0 1");
}

#[tokio::test]
async fn simple_query_reports_direct_column_origin() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));
    let (session, _) = engine
        .startup(StartupParams {
            database: "default".to_owned(),
            application_name: Some("pgwire-simple-origin".to_owned()),
            options: std::collections::BTreeMap::new(),
            credential: Credential::Anonymous {
                user: "test".to_owned(),
            },
            transport: TransportInfo::in_process(),
        })
        .expect("startup seed session");
    engine
        .execute_sql(&session, "CREATE TABLE t_simple_origin (id INT, name TEXT)")
        .expect("create simple origin table");
    let expected_oid = match engine
        .execute_sql(
            &session,
            "SELECT oid FROM pg_class WHERE relname = 't_simple_origin'",
        )
        .expect("lookup pg_class oid")
        .into_iter()
        .next()
        .expect("oid row")
    {
        StatementResult::Query { rows, .. } => match rows
            .first()
            .and_then(|row| row.values.first())
            .expect("oid value")
        {
            Value::Int(oid) => u32::try_from(*oid).expect("positive oid"),
            other => panic!("expected oid int, got {other:?}"),
        },
        other => panic!("expected oid query result, got {other:?}"),
    };

    let mut input = build_startup_bytes();
    input.extend(build_query_bytes("SELECT id FROM t_simple_origin"));
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run().await.expect("simple query direct column");

    let origin_info = backend_messages(conn.writer_ref())
        .iter()
        .find(|(tag, _)| *tag == b'T')
        .map(|(_, payload)| parse_row_description_origin_info(payload))
        .expect("row description");
    assert_eq!(origin_info, vec![(expected_oid, 1)]);
}

#[tokio::test]
async fn simple_query_leaves_expression_origin_empty() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));
    let (session, _) = engine
        .startup(StartupParams {
            database: "default".to_owned(),
            application_name: Some("pgwire-simple-origin-expr".to_owned()),
            options: std::collections::BTreeMap::new(),
            credential: Credential::Anonymous {
                user: "test".to_owned(),
            },
            transport: TransportInfo::in_process(),
        })
        .expect("startup seed session");
    engine
        .execute_sql(&session, "CREATE TABLE t_simple_origin_expr (id INT)")
        .expect("create simple origin expr table");

    let mut input = build_startup_bytes();
    input.extend(build_query_bytes("SELECT id + 1 FROM t_simple_origin_expr"));
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run().await.expect("simple query expression");

    let origin_info = backend_messages(conn.writer_ref())
        .iter()
        .find(|(tag, _)| *tag == b'T')
        .map(|(_, payload)| parse_row_description_origin_info(payload))
        .expect("row description");
    assert_eq!(origin_info, vec![(0, 0)]);
}

#[tokio::test]
async fn extended_execute_insert_returning_reports_insert_tag() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));
    let (session, _) = engine
        .startup(StartupParams {
            database: "default".to_owned(),
            application_name: Some("pgwire-extended-returning-seed".to_owned()),
            options: std::collections::BTreeMap::new(),
            credential: Credential::Anonymous {
                user: "test".to_owned(),
            },
            transport: TransportInfo::in_process(),
        })
        .expect("startup seed session");
    engine
        .execute_sql(&session, "CREATE TABLE t_ret_extended (id INT)")
        .expect("create extended returning table");

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_ret",
        "INSERT INTO t_ret_extended VALUES (1) RETURNING id",
        &[],
    ));
    input.extend(build_bind_bytes("p_ret", "s_ret", &[], &[], &[]));
    input.extend(build_execute_bytes("p_ret", 0));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("extended INSERT RETURNING should complete");

    let messages = backend_messages(conn.writer_ref());
    let data_row = messages
        .iter()
        .find(|(tag, _)| *tag == b'D')
        .map(|(_, payload)| payload.as_slice())
        .expect("data row");
    assert_eq!(parse_data_row_columns(data_row), vec![Some(b"1".to_vec())]);
    let command = messages
        .iter()
        .rfind(|(tag, _)| *tag == b'C')
        .map(|(_, payload)| parse_cstring_payload(payload))
        .expect("command complete");
    assert_eq!(command, "INSERT 0 1");
}

#[tokio::test]
async fn extended_execute_sql_execute_prepared_insert_returning_reports_insert_tag() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));
    let (session, _) = engine
        .startup(StartupParams {
            database: "default".to_owned(),
            application_name: Some("pgwire-extended-exec-returning-seed".to_owned()),
            options: std::collections::BTreeMap::new(),
            credential: Credential::Anonymous {
                user: "test".to_owned(),
            },
            transport: TransportInfo::in_process(),
        })
        .expect("startup seed session");
    engine
        .execute_sql(&session, "CREATE TABLE t_exec_ret_extended (id INT)")
        .expect("create execute returning table");

    let mut input = build_startup_bytes();
    input.extend(build_query_bytes(
        "PREPARE ins_ret AS INSERT INTO t_exec_ret_extended VALUES (1) RETURNING id",
    ));
    input.extend(build_parse_bytes("s_exec_ins_ret", "EXECUTE ins_ret", &[]));
    input.extend(build_bind_bytes(
        "p_exec_ins_ret",
        "s_exec_ins_ret",
        &[],
        &[],
        &[],
    ));
    input.extend(build_execute_bytes("p_exec_ins_ret", 0));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("extended EXECUTE ins_ret should complete");

    let messages = backend_messages(conn.writer_ref());
    let data_row = messages
        .iter()
        .find(|(tag, _)| *tag == b'D')
        .map(|(_, payload)| payload.as_slice())
        .expect("data row");
    assert_eq!(parse_data_row_columns(data_row), vec![Some(b"1".to_vec())]);
    let commands: Vec<String> = messages
        .iter()
        .filter(|(tag, _)| *tag == b'C')
        .map(|(_, payload)| parse_cstring_payload(payload).to_owned())
        .collect();
    assert!(
        commands.iter().any(|command| command == "PREPARE"),
        "expected PREPARE command complete, got {commands:?}"
    );
    assert!(
        commands.iter().any(|command| command == "INSERT 0 1"),
        "expected INSERT 0 1 command complete, got {commands:?}"
    );
}

#[tokio::test]
async fn extended_execute_insert_returning_does_not_reapply_side_effects_across_batches() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));
    let (session, _) = engine
        .startup(StartupParams {
            database: "default".to_owned(),
            application_name: Some("pgwire-extended-returning-suspended-seed".to_owned()),
            options: std::collections::BTreeMap::new(),
            credential: Credential::Anonymous {
                user: "test".to_owned(),
            },
            transport: TransportInfo::in_process(),
        })
        .expect("startup seed session");
    engine
        .execute_sql(&session, "CREATE TABLE t_ret_extended_susp (id INT)")
        .expect("create extended suspended returning table");

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_ret_susp",
        "INSERT INTO t_ret_extended_susp VALUES (1), (2) RETURNING id",
        &[],
    ));
    input.extend(build_bind_bytes("p_ret_susp", "s_ret_susp", &[], &[], &[]));
    input.extend(build_execute_bytes("p_ret_susp", 1));
    input.extend(build_execute_bytes("p_ret_susp", 1));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine.clone(), reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("extended INSERT RETURNING batches should complete");

    let messages = backend_messages(conn.writer_ref());
    let tags: Vec<u8> = messages.iter().map(|(tag, _)| *tag).collect();
    assert!(
        tags.windows(2).any(|window| window == [b'D', b's']),
        "expected a DataRow followed by PortalSuspended on the first Execute"
    );
    let data_rows: Vec<&[u8]> = messages
        .iter()
        .filter(|(tag, _)| *tag == b'D')
        .map(|(_, payload)| payload.as_slice())
        .collect();
    assert_eq!(
        data_rows.len(),
        2,
        "expected two returned rows across batches"
    );
    assert_eq!(
        parse_data_row_columns(data_rows[0]),
        vec![Some(b"1".to_vec())]
    );
    assert_eq!(
        parse_data_row_columns(data_rows[1]),
        vec![Some(b"2".to_vec())]
    );
    let command = messages
        .iter()
        .rfind(|(tag, _)| *tag == b'C')
        .map(|(_, payload)| parse_cstring_payload(payload))
        .expect("command complete");
    assert_eq!(command, "INSERT 0 2");

    let count = engine
        .execute_sql(&session, "SELECT COUNT(*) FROM t_ret_extended_susp")
        .expect("count inserted rows");
    assert!(matches!(
        count.as_slice(),
        [StatementResult::Query { rows, .. }]
            if rows == &[aiondb_core::Row::new(vec![aiondb_core::Value::BigInt(2)])]
    ));
}

#[tokio::test]
async fn simple_query_update_returning_reports_update_tag() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));
    let (session, _) = engine
        .startup(StartupParams {
            database: "default".to_owned(),
            application_name: Some("pgwire-simple-update-returning-seed".to_owned()),
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
            "CREATE TABLE t_ret_simple_upd (id INT);
             INSERT INTO t_ret_simple_upd VALUES (1)",
        )
        .expect("seed simple update returning table");

    let mut input = build_startup_bytes();
    input.extend(build_query_bytes(
        "UPDATE t_ret_simple_upd SET id = 2 WHERE id = 1 RETURNING id",
    ));
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("simple UPDATE RETURNING should complete");

    let messages = backend_messages(conn.writer_ref());
    let data_row = messages
        .iter()
        .find(|(tag, _)| *tag == b'D')
        .map(|(_, payload)| payload.as_slice())
        .expect("data row");
    assert_eq!(parse_data_row_columns(data_row), vec![Some(b"2".to_vec())]);
    let command = messages
        .iter()
        .rfind(|(tag, _)| *tag == b'C')
        .map(|(_, payload)| parse_cstring_payload(payload))
        .expect("command complete");
    assert_eq!(command, "UPDATE 1");
}

#[tokio::test]
async fn simple_query_delete_returning_reports_delete_tag() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));
    let (session, _) = engine
        .startup(StartupParams {
            database: "default".to_owned(),
            application_name: Some("pgwire-simple-delete-returning-seed".to_owned()),
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
            "CREATE TABLE t_ret_simple_del (id INT);
             INSERT INTO t_ret_simple_del VALUES (1)",
        )
        .expect("seed simple delete returning table");

    let mut input = build_startup_bytes();
    input.extend(build_query_bytes(
        "DELETE FROM t_ret_simple_del WHERE id = 1 RETURNING id",
    ));
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("simple DELETE RETURNING should complete");

    let messages = backend_messages(conn.writer_ref());
    let data_row = messages
        .iter()
        .find(|(tag, _)| *tag == b'D')
        .map(|(_, payload)| payload.as_slice())
        .expect("data row");
    assert_eq!(parse_data_row_columns(data_row), vec![Some(b"1".to_vec())]);
    let command = messages
        .iter()
        .rfind(|(tag, _)| *tag == b'C')
        .map(|(_, payload)| parse_cstring_payload(payload))
        .expect("command complete");
    assert_eq!(command, "DELETE 1");
}

#[tokio::test]
async fn simple_query_update_from_returning_source_origin_with_target_alias() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));
    let (session, _) = engine
        .startup(StartupParams {
            database: "default".to_owned(),
            application_name: Some("pgwire-simple-update-from-alias-returning".to_owned()),
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
            "CREATE TABLE t_simple_upd_alias_target (id INT, src_id INT);
             CREATE TABLE t_simple_upd_alias_source (id INT, payload TEXT);
             INSERT INTO t_simple_upd_alias_target VALUES (1, 10);
             INSERT INTO t_simple_upd_alias_source VALUES (10, 'match')",
        )
        .expect("seed simple update from alias returning tables");
    let expected_oid = match engine
        .execute_sql(
            &session,
            "SELECT oid FROM pg_class WHERE relname = 't_simple_upd_alias_source'",
        )
        .expect("lookup pg_class oid")
        .into_iter()
        .next()
        .expect("oid row")
    {
        StatementResult::Query { rows, .. } => match rows
            .first()
            .and_then(|row| row.values.first())
            .expect("oid value")
        {
            Value::Int(oid) => u32::try_from(*oid).expect("positive oid"),
            other => panic!("expected oid int, got {other:?}"),
        },
        other => panic!("expected oid query result, got {other:?}"),
    };

    let mut input = build_startup_bytes();
    input.extend(build_query_bytes(
        "UPDATE t_simple_upd_alias_target AS t \
         SET src_id = s.id \
         FROM t_simple_upd_alias_source AS s \
         WHERE t.src_id = s.id \
         RETURNING s.payload",
    ));
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("simple UPDATE ... FROM alias RETURNING should complete");

    let messages = backend_messages(conn.writer_ref());
    let origin_info = messages
        .iter()
        .find(|(tag, _)| *tag == b'T')
        .map(|(_, payload)| parse_row_description_origin_info(payload))
        .expect("row description");
    assert_eq!(origin_info, vec![(expected_oid, 2)]);
    let data_row = messages
        .iter()
        .find(|(tag, _)| *tag == b'D')
        .map(|(_, payload)| payload.as_slice())
        .expect("data row");
    assert_eq!(
        parse_data_row_columns(data_row),
        vec![Some(b"match".to_vec())]
    );
    let command = messages
        .iter()
        .rfind(|(tag, _)| *tag == b'C')
        .map(|(_, payload)| parse_cstring_payload(payload))
        .expect("command complete");
    assert_eq!(command, "UPDATE 1");
}

#[tokio::test]
async fn simple_query_delete_using_returning_source_origin_with_target_alias() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));
    let (session, _) = engine
        .startup(StartupParams {
            database: "default".to_owned(),
            application_name: Some("pgwire-simple-delete-using-alias-returning".to_owned()),
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
            "CREATE TABLE t_simple_del_alias_target (id INT, src_id INT);
             CREATE TABLE t_simple_del_alias_source (id INT, payload TEXT);
             INSERT INTO t_simple_del_alias_target VALUES (1, 10);
             INSERT INTO t_simple_del_alias_source VALUES (10, 'match')",
        )
        .expect("seed simple delete using alias returning tables");
    let expected_oid = match engine
        .execute_sql(
            &session,
            "SELECT oid FROM pg_class WHERE relname = 't_simple_del_alias_source'",
        )
        .expect("lookup pg_class oid")
        .into_iter()
        .next()
        .expect("oid row")
    {
        StatementResult::Query { rows, .. } => match rows
            .first()
            .and_then(|row| row.values.first())
            .expect("oid value")
        {
            Value::Int(oid) => u32::try_from(*oid).expect("positive oid"),
            other => panic!("expected oid int, got {other:?}"),
        },
        other => panic!("expected oid query result, got {other:?}"),
    };

    let mut input = build_startup_bytes();
    input.extend(build_query_bytes(
        "DELETE FROM t_simple_del_alias_target AS t \
         USING t_simple_del_alias_source AS s \
         WHERE t.src_id = s.id \
         RETURNING s.payload",
    ));
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("simple DELETE ... USING alias RETURNING should complete");

    let messages = backend_messages(conn.writer_ref());
    let origin_info = messages
        .iter()
        .find(|(tag, _)| *tag == b'T')
        .map(|(_, payload)| parse_row_description_origin_info(payload))
        .expect("row description");
    assert_eq!(origin_info, vec![(expected_oid, 2)]);
    let data_row = messages
        .iter()
        .find(|(tag, _)| *tag == b'D')
        .map(|(_, payload)| payload.as_slice())
        .expect("data row");
    assert_eq!(
        parse_data_row_columns(data_row),
        vec![Some(b"match".to_vec())]
    );
    let command = messages
        .iter()
        .rfind(|(tag, _)| *tag == b'C')
        .map(|(_, payload)| parse_cstring_payload(payload))
        .expect("command complete");
    assert_eq!(command, "DELETE 1");
}

#[tokio::test]
async fn extended_execute_update_returning_reports_update_tag() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));
    let (session, _) = engine
        .startup(StartupParams {
            database: "default".to_owned(),
            application_name: Some("pgwire-extended-update-returning-seed".to_owned()),
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
            "CREATE TABLE t_ret_extended_upd (id INT);
             INSERT INTO t_ret_extended_upd VALUES (1)",
        )
        .expect("seed extended update returning table");

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_ret_upd",
        "UPDATE t_ret_extended_upd SET id = 2 WHERE id = 1 RETURNING id",
        &[],
    ));
    input.extend(build_bind_bytes("p_ret_upd", "s_ret_upd", &[], &[], &[]));
    input.extend(build_execute_bytes("p_ret_upd", 0));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("extended UPDATE RETURNING should complete");

    let messages = backend_messages(conn.writer_ref());
    let data_row = messages
        .iter()
        .find(|(tag, _)| *tag == b'D')
        .map(|(_, payload)| payload.as_slice())
        .expect("data row");
    assert_eq!(parse_data_row_columns(data_row), vec![Some(b"2".to_vec())]);
    let command = messages
        .iter()
        .rfind(|(tag, _)| *tag == b'C')
        .map(|(_, payload)| parse_cstring_payload(payload))
        .expect("command complete");
    assert_eq!(command, "UPDATE 1");
}

#[tokio::test]
async fn extended_execute_update_from_returning_with_target_alias() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));
    let (session, _) = engine
        .startup(StartupParams {
            database: "default".to_owned(),
            application_name: Some("pgwire-extended-update-from-alias-returning".to_owned()),
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
            "CREATE TABLE t_ret_upd_alias_target (id INT, src_id INT);
             CREATE TABLE t_ret_upd_alias_source (id INT, payload TEXT);
             INSERT INTO t_ret_upd_alias_target VALUES (1, 10);
             INSERT INTO t_ret_upd_alias_source VALUES (10, 'match')",
        )
        .expect("seed update from alias returning tables");
    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_ret_upd_alias",
        "UPDATE t_ret_upd_alias_target AS t \
         SET src_id = s.id \
         FROM t_ret_upd_alias_source AS s \
         WHERE t.src_id = s.id \
         RETURNING s.payload",
        &[],
    ));
    input.extend(build_bind_bytes(
        "p_ret_upd_alias",
        "s_ret_upd_alias",
        &[],
        &[],
        &[],
    ));
    input.extend(build_execute_bytes("p_ret_upd_alias", 0));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("extended UPDATE ... FROM alias RETURNING should complete");

    let messages = backend_messages(conn.writer_ref());
    let error_message = messages
        .iter()
        .find(|(tag, _)| *tag == b'E')
        .and_then(|(_, payload)| parse_error_response_message(payload));
    assert!(
        error_message.is_none(),
        "unexpected ErrorResponse in backend stream: {error_message:?}"
    );
    let data_row = messages
        .iter()
        .find(|(tag, _)| *tag == b'D')
        .map(|(_, payload)| payload.as_slice())
        .expect("data row");
    assert_eq!(
        parse_data_row_columns(data_row),
        vec![Some(b"match".to_vec())]
    );
    let command = messages
        .iter()
        .rfind(|(tag, _)| *tag == b'C')
        .map(|(_, payload)| parse_cstring_payload(payload))
        .expect("command complete");
    assert_eq!(command, "UPDATE 1");
}

#[tokio::test]
async fn extended_execute_delete_returning_reports_delete_tag() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));
    let (session, _) = engine
        .startup(StartupParams {
            database: "default".to_owned(),
            application_name: Some("pgwire-extended-delete-returning-seed".to_owned()),
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
            "CREATE TABLE t_ret_extended_del (id INT);
             INSERT INTO t_ret_extended_del VALUES (1)",
        )
        .expect("seed extended delete returning table");

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_ret_del",
        "DELETE FROM t_ret_extended_del WHERE id = 1 RETURNING id",
        &[],
    ));
    input.extend(build_bind_bytes("p_ret_del", "s_ret_del", &[], &[], &[]));
    input.extend(build_execute_bytes("p_ret_del", 0));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("extended DELETE RETURNING should complete");

    let messages = backend_messages(conn.writer_ref());
    let data_row = messages
        .iter()
        .find(|(tag, _)| *tag == b'D')
        .map(|(_, payload)| payload.as_slice())
        .expect("data row");
    assert_eq!(parse_data_row_columns(data_row), vec![Some(b"1".to_vec())]);
    let command = messages
        .iter()
        .rfind(|(tag, _)| *tag == b'C')
        .map(|(_, payload)| parse_cstring_payload(payload))
        .expect("command complete");
    assert_eq!(command, "DELETE 1");
}

#[tokio::test]
async fn extended_execute_merge_binds_params_in_on_and_when_clauses() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));
    let (session, _) = engine
        .startup(StartupParams {
            database: "default".to_owned(),
            application_name: Some("pgwire-extended-merge-params-seed".to_owned()),
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
            "CREATE TABLE merge_target_ext (id INT PRIMARY KEY, val TEXT); \
             CREATE TABLE merge_source_ext (id INT PRIMARY KEY, val TEXT); \
             INSERT INTO merge_target_ext VALUES (1, 'old'); \
             INSERT INTO merge_source_ext VALUES (1, 'seed')",
        )
        .expect("seed merge tables");

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_merge",
        "MERGE INTO merge_target_ext AS t \
         USING merge_source_ext AS s \
         ON t.id = s.id AND t.id = $1 \
         WHEN MATCHED AND s.val = $2 THEN UPDATE SET val = $3 \
         WHEN NOT MATCHED THEN INSERT (id, val) VALUES ($4, $5)",
        &[],
    ));
    input.extend(build_bind_bytes(
        "p_merge",
        "s_merge",
        &[],
        &[
            Some(b"1"),
            Some(b"seed"),
            Some(b"updated"),
            Some(b"2"),
            Some(b"inserted"),
        ],
        &[],
    ));
    input.extend(build_execute_bytes("p_merge", 0));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine.clone(), reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("extended MERGE with params should complete");

    let messages = backend_messages(conn.writer_ref());
    assert!(
        !messages.iter().any(|(tag, _)| *tag == b'E'),
        "unexpected ErrorResponse during MERGE"
    );

    let row = engine
        .execute_sql(&session, "SELECT val FROM merge_target_ext WHERE id = 1")
        .expect("select merged row");
    assert!(matches!(
        row.as_slice(),
        [StatementResult::Query { rows, .. }]
            if rows == &[aiondb_core::Row::new(vec![aiondb_core::Value::Text(
                "updated".to_owned()
            )])]
    ));
}

#[tokio::test]
async fn extended_execute_delete_using_returning_with_target_alias() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));
    let (session, _) = engine
        .startup(StartupParams {
            database: "default".to_owned(),
            application_name: Some("pgwire-extended-delete-using-alias-returning".to_owned()),
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
            "CREATE TABLE t_ret_del_alias_target (id INT, src_id INT);
             CREATE TABLE t_ret_del_alias_source (id INT, payload TEXT);
             INSERT INTO t_ret_del_alias_target VALUES (1, 10);
             INSERT INTO t_ret_del_alias_source VALUES (10, 'match')",
        )
        .expect("seed delete using alias returning tables");
    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_ret_del_alias",
        "DELETE FROM t_ret_del_alias_target AS t \
         USING t_ret_del_alias_source AS s \
         WHERE t.src_id = s.id \
         RETURNING s.payload",
        &[],
    ));
    input.extend(build_bind_bytes(
        "p_ret_del_alias",
        "s_ret_del_alias",
        &[],
        &[],
        &[],
    ));
    input.extend(build_execute_bytes("p_ret_del_alias", 0));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("extended DELETE ... USING alias RETURNING should complete");

    let messages = backend_messages(conn.writer_ref());
    let error_message = messages
        .iter()
        .find(|(tag, _)| *tag == b'E')
        .and_then(|(_, payload)| parse_error_response_message(payload));
    assert!(
        error_message.is_none(),
        "unexpected ErrorResponse in backend stream: {error_message:?}"
    );
    let data_row = messages
        .iter()
        .find(|(tag, _)| *tag == b'D')
        .map(|(_, payload)| payload.as_slice())
        .expect("data row");
    assert_eq!(
        parse_data_row_columns(data_row),
        vec![Some(b"match".to_vec())]
    );
    let command = messages
        .iter()
        .rfind(|(tag, _)| *tag == b'C')
        .map(|(_, payload)| parse_cstring_payload(payload))
        .expect("command complete");
    assert_eq!(command, "DELETE 1");
}

#[tokio::test]
async fn extended_execute_update_from_returning_zero_rows_reports_update_zero() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));
    let (session, _) = engine
        .startup(StartupParams {
            database: "default".to_owned(),
            application_name: Some("pgwire-extended-update-from-alias-returning-empty".to_owned()),
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
            "CREATE TABLE t_ret_upd_alias_empty_target (id INT, src_id INT);
             CREATE TABLE t_ret_upd_alias_empty_source (id INT, payload TEXT);
             INSERT INTO t_ret_upd_alias_empty_target VALUES (1, 10);
             INSERT INTO t_ret_upd_alias_empty_source VALUES (20, 'miss')",
        )
        .expect("seed empty update from alias returning tables");
    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_ret_upd_alias_empty",
        "UPDATE t_ret_upd_alias_empty_target AS t \
         SET src_id = s.id \
         FROM t_ret_upd_alias_empty_source AS s \
         WHERE t.src_id = s.id \
         RETURNING s.payload",
        &[],
    ));
    input.extend(build_bind_bytes(
        "p_ret_upd_alias_empty",
        "s_ret_upd_alias_empty",
        &[],
        &[],
        &[],
    ));
    input.extend(build_execute_bytes("p_ret_upd_alias_empty", 0));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("extended UPDATE ... FROM alias RETURNING empty should complete");

    let messages = backend_messages(conn.writer_ref());
    assert!(
        !messages.iter().any(|(tag, _)| *tag == b'E'),
        "unexpected ErrorResponse in backend stream"
    );
    assert!(
        !messages.iter().any(|(tag, _)| *tag == b'T'),
        "extended Execute should not emit RowDescription for RETURNING"
    );
    assert!(
        !messages.iter().any(|(tag, _)| *tag == b'D'),
        "empty returning result should not emit DataRow"
    );
    let command = messages
        .iter()
        .rfind(|(tag, _)| *tag == b'C')
        .map(|(_, payload)| parse_cstring_payload(payload))
        .expect("command complete");
    assert_eq!(command, "UPDATE 0");
}
