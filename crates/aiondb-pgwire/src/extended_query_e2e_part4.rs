#[path = "extended_query_e2e_part4_params_and_vectors.rs"]
mod params_and_vectors;

#[tokio::test]
async fn extended_describe_statement_for_sql_explain_execute_rechecks_metadata_after_parse() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_explain_exec_late_desc",
        "EXPLAIN EXECUTE stmt(41)",
        &[],
    ));
    input.extend(build_query_bytes("PREPARE stmt(int) AS SELECT $1 + 1 AS v"));
    input.extend(build_describe_bytes(b'S', "s_explain_exec_late_desc"));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("extended EXPLAIN EXECUTE statement describe should re-check metadata");

    let messages = backend_messages(conn.writer_ref());
    let row_description = messages
        .iter()
        .rfind(|(tag, _)| *tag == b'T')
        .map(|(_, payload)| payload.as_slice())
        .expect("row description");
    assert_eq!(
        parse_row_description_field_names(row_description),
        vec!["QUERY PLAN"]
    );
    assert_eq!(
        parse_row_description_type_info(row_description),
        vec![(25, -1)]
    );
}

#[tokio::test]
async fn extended_describe_portal_for_sql_explain_execute_rechecks_metadata_after_parse() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_explain_exec_portal_late_desc",
        "EXPLAIN EXECUTE stmt(41)",
        &[],
    ));
    input.extend(build_query_bytes("PREPARE stmt(int) AS SELECT $1 + 1 AS v"));
    input.extend(build_bind_bytes(
        "p_explain_exec_portal_late_desc",
        "s_explain_exec_portal_late_desc",
        &[],
        &[],
        &[],
    ));
    input.extend(build_describe_bytes(
        b'P',
        "p_explain_exec_portal_late_desc",
    ));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("extended EXPLAIN EXECUTE portal describe should re-check metadata");

    let messages = backend_messages(conn.writer_ref());
    let row_description = messages
        .iter()
        .rfind(|(tag, _)| *tag == b'T')
        .map(|(_, payload)| payload.as_slice())
        .expect("row description");
    assert_eq!(
        parse_row_description_field_names(row_description),
        vec!["QUERY PLAN"]
    );
    assert_eq!(
        parse_row_description_type_info(row_description),
        vec![(25, -1)]
    );
}

#[tokio::test]
async fn extended_describe_portal_for_fetch_reports_row_description() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));
    let (session, _) = engine
        .startup(StartupParams {
            database: "default".to_owned(),
            application_name: Some("pgwire-extended-fetch-describe-seed".to_owned()),
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
            "CREATE TABLE t_fetch_desc (id INT);
             INSERT INTO t_fetch_desc VALUES (1), (2)",
        )
        .expect("seed fetch describe table");
    let expected_oid = lookup_relation_oid(engine.as_ref(), &session, "t_fetch_desc");

    let mut input = build_startup_bytes();
    input.extend(build_query_bytes("BEGIN"));
    input.extend(build_query_bytes(
        "DECLARE c CURSOR FOR SELECT id FROM t_fetch_desc ORDER BY id",
    ));
    input.extend(build_parse_bytes("s_fetch_desc", "FETCH ALL IN c", &[]));
    input.extend(build_bind_bytes(
        "p_fetch_desc",
        "s_fetch_desc",
        &[],
        &[],
        &[],
    ));
    input.extend(build_describe_bytes(b'P', "p_fetch_desc"));
    input.extend(build_sync_bytes());
    input.extend(build_query_bytes("COMMIT"));
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("extended FETCH portal describe should complete");

    let type_info = backend_messages(conn.writer_ref())
        .iter()
        .rfind(|(tag, _)| *tag == b'T')
        .map(|(_, payload)| {
            assert_eq!(parse_row_description_field_names(payload), vec!["id"]);
            assert_eq!(
                parse_row_description_origin_info(payload),
                vec![(expected_oid, 1)]
            );
            parse_row_description_type_info(payload)
        })
        .expect("row description");
    assert_eq!(type_info, vec![(23, -1)]);
}

#[tokio::test]
async fn extended_describe_statement_for_fetch_reports_row_description() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));
    let (session, _) = engine
        .startup(StartupParams {
            database: "default".to_owned(),
            application_name: Some("pgwire-extended-fetch-stmt-describe-seed".to_owned()),
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
            "CREATE TABLE t_fetch_stmt_desc (id INT);
             INSERT INTO t_fetch_stmt_desc VALUES (1), (2)",
        )
        .expect("seed fetch statement describe table");
    let expected_oid = lookup_relation_oid(engine.as_ref(), &session, "t_fetch_stmt_desc");

    let mut input = build_startup_bytes();
    input.extend(build_query_bytes("BEGIN"));
    input.extend(build_query_bytes(
        "DECLARE c CURSOR FOR SELECT id FROM t_fetch_stmt_desc ORDER BY id",
    ));
    input.extend(build_parse_bytes(
        "s_fetch_stmt_desc",
        "FETCH ALL IN c",
        &[],
    ));
    input.extend(build_describe_bytes(b'S', "s_fetch_stmt_desc"));
    input.extend(build_sync_bytes());
    input.extend(build_query_bytes("COMMIT"));
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("extended FETCH statement describe should complete");

    let type_info = backend_messages(conn.writer_ref())
        .iter()
        .rfind(|(tag, _)| *tag == b'T')
        .map(|(_, payload)| {
            assert_eq!(parse_row_description_field_names(payload), vec!["id"]);
            assert_eq!(
                parse_row_description_origin_info(payload),
                vec![(expected_oid, 1)]
            );
            parse_row_description_type_info(payload)
        })
        .expect("row description");
    assert_eq!(type_info, vec![(23, -1)]);
}

#[tokio::test]
async fn extended_describe_statement_for_fetch_forward_count_reports_row_description() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));
    let (session, _) = engine
        .startup(StartupParams {
            database: "default".to_owned(),
            application_name: Some("pgwire-extended-fetch-forward-stmt-describe-seed".to_owned()),
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
            "CREATE TABLE t_fetch_forward_stmt_desc (id INT);
             INSERT INTO t_fetch_forward_stmt_desc VALUES (1), (2), (3)",
        )
        .expect("seed fetch forward statement describe table");
    let expected_oid = lookup_relation_oid(engine.as_ref(), &session, "t_fetch_forward_stmt_desc");

    let mut input = build_startup_bytes();
    input.extend(build_query_bytes("BEGIN"));
    input.extend(build_query_bytes(
        "DECLARE c CURSOR FOR SELECT id FROM t_fetch_forward_stmt_desc ORDER BY id",
    ));
    input.extend(build_parse_bytes(
        "s_fetch_forward_stmt_desc",
        "FETCH FORWARD 2 IN c",
        &[],
    ));
    input.extend(build_describe_bytes(b'S', "s_fetch_forward_stmt_desc"));
    input.extend(build_sync_bytes());
    input.extend(build_query_bytes("COMMIT"));
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("extended FETCH FORWARD statement describe should complete");

    let type_info = backend_messages(conn.writer_ref())
        .iter()
        .rfind(|(tag, _)| *tag == b'T')
        .map(|(_, payload)| {
            assert_eq!(parse_row_description_field_names(payload), vec!["id"]);
            assert_eq!(
                parse_row_description_origin_info(payload),
                vec![(expected_oid, 1)]
            );
            parse_row_description_type_info(payload)
        })
        .expect("row description");
    assert_eq!(type_info, vec![(23, -1)]);
}

#[tokio::test]
async fn extended_describe_statement_for_missing_fetch_cursor_reports_error() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_fetch_missing_cursor",
        "FETCH ALL IN missing_cursor",
        &[],
    ));
    input.extend(build_describe_bytes(b'S', "s_fetch_missing_cursor"));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("extended FETCH missing cursor describe should complete with protocol error");

    let messages = backend_messages(conn.writer_ref());
    assert!(
        messages.iter().any(|(tag, _)| *tag == b'1'),
        "expected ParseComplete before Describe failure"
    );
    assert!(
        !messages.iter().any(|(tag, _)| *tag == b'T'),
        "missing cursor describe should not emit RowDescription"
    );
    let error_payload = messages
        .iter()
        .find(|(tag, _)| *tag == b'E')
        .map(|(_, payload)| payload.as_slice())
        .expect("error response");
    let error_message =
        parse_error_response_message(error_payload).expect("parse error response message");
    assert!(
        error_message.contains("cursor \"missing_cursor\" does not exist"),
        "expected missing cursor error, got {error_message:?}"
    );
}

#[tokio::test]
async fn extended_describe_statement_for_missing_close_cursor_reports_error() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_close_missing_cursor",
        "CLOSE missing_cursor",
        &[],
    ));
    input.extend(build_describe_bytes(b'S', "s_close_missing_cursor"));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("extended CLOSE missing cursor describe should complete with protocol error");

    let messages = backend_messages(conn.writer_ref());
    assert!(
        messages.iter().any(|(tag, _)| *tag == b'1'),
        "expected ParseComplete before Describe failure"
    );
    assert!(
        !messages.iter().any(|(tag, _)| *tag == b'T'),
        "missing cursor close describe should not emit RowDescription"
    );
    let error_payload = messages
        .iter()
        .find(|(tag, _)| *tag == b'E')
        .map(|(_, payload)| payload.as_slice())
        .expect("error response");
    let error_message =
        parse_error_response_message(error_payload).expect("parse error response message");
    assert!(
        error_message.contains("cursor \"missing_cursor\" does not exist"),
        "expected missing cursor error, got {error_message:?}"
    );
}

#[tokio::test]
async fn extended_describe_statement_for_missing_sql_explain_fetch_reports_error() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_explain_fetch_missing_cursor",
        "EXPLAIN FETCH ALL IN missing_cursor",
        &[],
    ));
    input.extend(build_describe_bytes(b'S', "s_explain_fetch_missing_cursor"));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run().await.expect(
        "extended EXPLAIN FETCH missing cursor describe should complete with protocol error",
    );

    let messages = backend_messages(conn.writer_ref());
    assert!(
        messages.iter().any(|(tag, _)| *tag == b'1'),
        "expected ParseComplete before Describe failure"
    );
    assert!(
        !messages.iter().any(|(tag, _)| *tag == b'T'),
        "missing cursor describe should not emit RowDescription"
    );
    let error_payload = messages
        .iter()
        .find(|(tag, _)| *tag == b'E')
        .map(|(_, payload)| payload.as_slice())
        .expect("error response");
    let error_message =
        parse_error_response_message(error_payload).expect("parse error response message");
    assert!(
        error_message.contains("cursor \"missing_cursor\" does not exist"),
        "expected missing cursor error, got {error_message:?}"
    );
}

#[tokio::test]
async fn extended_describe_statement_for_non_rowset_declare_cursor_reports_error() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));
    let (session, _) = engine
        .startup(StartupParams {
            database: "default".to_owned(),
            application_name: Some("pgwire-extended-declare-non-rowset-seed".to_owned()),
            options: std::collections::BTreeMap::new(),
            credential: Credential::Anonymous {
                user: "test".to_owned(),
            },
            transport: TransportInfo::in_process(),
        })
        .expect("startup seed session");
    engine
        .execute_sql(&session, "CREATE TABLE declare_non_rowset_ext (id INT)")
        .expect("create table");

    let mut input = build_startup_bytes();
    input.extend(build_query_bytes("BEGIN"));
    input.extend(build_parse_bytes(
        "s_decl_non_rowset",
        "DECLARE c CURSOR FOR INSERT INTO declare_non_rowset_ext VALUES (1)",
        &[],
    ));
    input.extend(build_describe_bytes(b'S', "s_decl_non_rowset"));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("extended DECLARE describe should complete with protocol error");

    let messages = backend_messages(conn.writer_ref());
    assert!(
        messages.iter().any(|(tag, _)| *tag == b'1'),
        "expected ParseComplete before Describe failure"
    );
    assert!(
        !messages.iter().any(|(tag, _)| *tag == b'T'),
        "non-rowset declare describe should not emit RowDescription"
    );
    let error_payload = messages
        .iter()
        .find(|(tag, _)| *tag == b'E')
        .map(|(_, payload)| payload.as_slice())
        .expect("error response");
    let error_message =
        parse_error_response_message(error_payload).expect("parse error response message");
    assert!(
        error_message.contains("DECLARE CURSOR only supports row-returning statements"),
        "expected non-rowset declare error, got {error_message:?}"
    );
}

#[tokio::test]
async fn extended_describe_statement_for_fetch_rechecks_cursor_metadata_after_parse() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));
    let (session, _) = engine
        .startup(StartupParams {
            database: "default".to_owned(),
            application_name: Some("pgwire-extended-fetch-stmt-late-describe-seed".to_owned()),
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
            "CREATE TABLE t_fetch_stmt_late_desc (id INT);
             INSERT INTO t_fetch_stmt_late_desc VALUES (1), (2)",
        )
        .expect("seed late fetch statement describe table");
    let expected_oid = lookup_relation_oid(engine.as_ref(), &session, "t_fetch_stmt_late_desc");

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_fetch_stmt_late_desc",
        "FETCH ALL IN c",
        &[],
    ));
    input.extend(build_query_bytes("BEGIN"));
    input.extend(build_query_bytes(
        "DECLARE c CURSOR FOR SELECT id FROM t_fetch_stmt_late_desc ORDER BY id",
    ));
    input.extend(build_describe_bytes(b'S', "s_fetch_stmt_late_desc"));
    input.extend(build_sync_bytes());
    input.extend(build_query_bytes("COMMIT"));
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("extended FETCH statement describe should re-check cursor metadata");

    let type_info = backend_messages(conn.writer_ref())
        .iter()
        .rfind(|(tag, _)| *tag == b'T')
        .map(|(_, payload)| {
            assert_eq!(parse_row_description_field_names(payload), vec!["id"]);
            assert_eq!(
                parse_row_description_origin_info(payload),
                vec![(expected_oid, 1)]
            );
            parse_row_description_type_info(payload)
        })
        .expect("row description");
    assert_eq!(type_info, vec![(23, -1)]);
}

#[tokio::test]
async fn extended_describe_portal_for_fetch_rechecks_cursor_metadata_after_parse() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));
    let (session, _) = engine
        .startup(StartupParams {
            database: "default".to_owned(),
            application_name: Some("pgwire-extended-fetch-portal-late-describe-seed".to_owned()),
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
            "CREATE TABLE t_fetch_portal_late_desc (id INT);
             INSERT INTO t_fetch_portal_late_desc VALUES (1), (2)",
        )
        .expect("seed late fetch portal describe table");
    let expected_oid = lookup_relation_oid(engine.as_ref(), &session, "t_fetch_portal_late_desc");

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_fetch_portal_late_desc",
        "FETCH ALL IN c",
        &[],
    ));
    input.extend(build_query_bytes("BEGIN"));
    input.extend(build_query_bytes(
        "DECLARE c CURSOR FOR SELECT id FROM t_fetch_portal_late_desc ORDER BY id",
    ));
    input.extend(build_bind_bytes(
        "p_fetch_portal_late_desc",
        "s_fetch_portal_late_desc",
        &[],
        &[],
        &[],
    ));
    input.extend(build_describe_bytes(b'P', "p_fetch_portal_late_desc"));
    input.extend(build_sync_bytes());
    input.extend(build_query_bytes("COMMIT"));
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("extended FETCH portal describe should re-check cursor metadata");

    let type_info = backend_messages(conn.writer_ref())
        .iter()
        .rfind(|(tag, _)| *tag == b'T')
        .map(|(_, payload)| {
            assert_eq!(parse_row_description_field_names(payload), vec!["id"]);
            assert_eq!(
                parse_row_description_origin_info(payload),
                vec![(expected_oid, 1)]
            );
            parse_row_description_type_info(payload)
        })
        .expect("row description");
    assert_eq!(type_info, vec![(23, -1)]);
}

#[tokio::test]
async fn extended_execute_move_reports_move_tag() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));
    let (session, _) = engine
        .startup(StartupParams {
            database: "default".to_owned(),
            application_name: Some("pgwire-extended-move-tag-seed".to_owned()),
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
            "CREATE TABLE t_move_tag_ext (id INT);
             INSERT INTO t_move_tag_ext VALUES (1), (2)",
        )
        .expect("seed extended move tag table");

    let mut input = build_startup_bytes();
    input.extend(build_query_bytes("BEGIN"));
    input.extend(build_query_bytes(
        "DECLARE c CURSOR FOR SELECT id FROM t_move_tag_ext ORDER BY id",
    ));
    input.extend(build_parse_bytes("s_move", "MOVE FORWARD ALL IN c", &[]));
    input.extend(build_bind_bytes("p_move", "s_move", &[], &[], &[]));
    input.extend(build_execute_bytes("p_move", 0));
    input.extend(build_sync_bytes());
    input.extend(build_query_bytes("COMMIT"));
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run().await.expect("extended MOVE should complete");

    let messages = backend_messages(conn.writer_ref());
    let commands: Vec<String> = messages
        .iter()
        .filter(|(tag, _)| *tag == b'C')
        .map(|(_, payload)| parse_cstring_payload(payload).to_owned())
        .collect();
    assert!(
        commands.iter().any(|command| command == "MOVE 2"),
        "expected MOVE 2 command complete, got {commands:?}"
    );
}

/// Parse -> Bind -> Describe(portal) -> Execute -> Sync -> Terminate.
///
/// The `RowDescription` should come from `Describe(portal)`, not from `Execute`.
#[tokio::test]
async fn extended_describe_portal_before_execute() {
    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes("s1", "SELECT id FROM t", &[]));
    input.extend(build_bind_bytes("p1", "s1", &[], &[], &[]));
    input.extend(build_describe_bytes(b'P', "p1"));
    input.extend(build_execute_bytes("p1", 0));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let engine = Arc::new(ExtendedMockEngine);
    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("describe portal before execute should work");

    let tags: Vec<u8> = backend_messages(conn.writer_ref())
        .iter()
        .map(|(tag, _)| *tag)
        .collect();
    let row_desc_positions: Vec<_> = tags
        .iter()
        .enumerate()
        .filter_map(|(index, tag)| (*tag == b'T').then_some(index))
        .collect();
    assert_eq!(
        row_desc_positions.len(),
        1,
        "Describe Portal should emit exactly one RowDescription"
    );
    let row_desc_pos = row_desc_positions[0];
    let data_row_pos = tags.iter().position(|tag| *tag == b'D').expect("data row");
    assert!(
        row_desc_pos < data_row_pos,
        "Describe Portal RowDescription should precede Execute DataRow"
    );
}
