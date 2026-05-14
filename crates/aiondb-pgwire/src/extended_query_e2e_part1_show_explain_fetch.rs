use super::*;

#[tokio::test]
async fn simple_query_show_reports_show_tag() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_query_bytes("SHOW application_name"));
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run().await.expect("simple SHOW should complete");

    let messages = backend_messages(conn.writer_ref());
    let command = messages
        .iter()
        .rfind(|(tag, _)| *tag == b'C')
        .map(|(_, payload)| parse_cstring_payload(payload))
        .expect("command complete");
    assert_eq!(command, "SHOW");
}

#[tokio::test]
async fn extended_execute_show_reports_show_tag() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes("s_show", "SHOW application_name", &[]));
    input.extend(build_bind_bytes("p_show", "s_show", &[], &[], &[]));
    input.extend(build_execute_bytes("p_show", 0));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run().await.expect("extended SHOW should complete");

    let messages = backend_messages(conn.writer_ref());
    let command = messages
        .iter()
        .rfind(|(tag, _)| *tag == b'C')
        .map(|(_, payload)| parse_cstring_payload(payload))
        .expect("command complete");
    assert_eq!(command, "SHOW");
}

#[tokio::test]
async fn simple_query_execute_prepared_show_reports_show_tag() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_query_bytes(
        "PREPARE show_stmt AS SHOW application_name",
    ));
    input.extend(build_query_bytes("EXECUTE show_stmt"));
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("simple EXECUTE show_stmt should complete");

    let commands: Vec<String> = backend_messages(conn.writer_ref())
        .iter()
        .filter(|(tag, _)| *tag == b'C')
        .map(|(_, payload)| parse_cstring_payload(payload).to_owned())
        .collect();
    assert!(
        commands
            .windows(2)
            .any(|window| window == ["PREPARE", "SHOW"]),
        "expected PREPARE then SHOW command completes, got {commands:?}"
    );
}

#[tokio::test]
async fn simple_query_explain_reports_explain_tag() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_query_bytes("EXPLAIN SELECT 1"));
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run().await.expect("simple EXPLAIN should complete");

    let messages = backend_messages(conn.writer_ref());
    let command = messages
        .iter()
        .rfind(|(tag, _)| *tag == b'C')
        .map(|(_, payload)| parse_cstring_payload(payload))
        .expect("command complete");
    assert_eq!(command, "EXPLAIN");
}

#[tokio::test]
async fn simple_query_execute_prepared_explain_reports_explain_tag() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_query_bytes(
        "PREPARE explain_stmt AS EXPLAIN SELECT 1",
    ));
    input.extend(build_query_bytes("EXECUTE explain_stmt"));
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("simple EXECUTE explain_stmt should complete");

    let commands: Vec<String> = backend_messages(conn.writer_ref())
        .iter()
        .filter(|(tag, _)| *tag == b'C')
        .map(|(_, payload)| parse_cstring_payload(payload).to_owned())
        .collect();
    assert!(
        commands
            .windows(2)
            .any(|window| window == ["PREPARE", "EXPLAIN"]),
        "expected PREPARE then EXPLAIN command completes, got {commands:?}"
    );
}

#[tokio::test]
async fn extended_execute_explain_reports_explain_tag() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes("s_explain", "EXPLAIN SELECT 1", &[]));
    input.extend(build_bind_bytes("p_explain", "s_explain", &[], &[], &[]));
    input.extend(build_execute_bytes("p_explain", 0));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run().await.expect("extended EXPLAIN should complete");

    let messages = backend_messages(conn.writer_ref());
    let command = messages
        .iter()
        .rfind(|(tag, _)| *tag == b'C')
        .map(|(_, payload)| parse_cstring_payload(payload))
        .expect("command complete");
    assert_eq!(command, "EXPLAIN");
}

#[tokio::test]
async fn extended_execute_sql_execute_prepared_explain_reports_explain_tag() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_query_bytes(
        "PREPARE explain_stmt AS EXPLAIN SELECT 1",
    ));
    input.extend(build_parse_bytes(
        "s_exec_explain",
        "EXECUTE explain_stmt",
        &[],
    ));
    input.extend(build_bind_bytes(
        "p_exec_explain",
        "s_exec_explain",
        &[],
        &[],
        &[],
    ));
    input.extend(build_execute_bytes("p_exec_explain", 0));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("extended EXECUTE explain_stmt should complete");

    let commands: Vec<String> = backend_messages(conn.writer_ref())
        .iter()
        .filter(|(tag, _)| *tag == b'C')
        .map(|(_, payload)| parse_cstring_payload(payload).to_owned())
        .collect();
    assert!(
        commands.iter().any(|command| command == "PREPARE"),
        "expected PREPARE command complete, got {commands:?}"
    );
    assert!(
        commands.iter().any(|command| command == "EXPLAIN"),
        "expected EXPLAIN command complete, got {commands:?}"
    );
}

#[tokio::test]
async fn simple_query_fetch_reports_fetch_tag() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));
    let (session, _) = engine
        .startup(StartupParams {
            database: "default".to_owned(),
            application_name: Some("pgwire-simple-fetch-tag-seed".to_owned()),
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
            "CREATE TABLE t_fetch_tag (id INT);
             INSERT INTO t_fetch_tag VALUES (1), (2)",
        )
        .expect("seed fetch tag table");
    let expected_oid = lookup_relation_oid(engine.as_ref(), &session, "t_fetch_tag");

    let mut input = build_startup_bytes();
    input.extend(build_query_bytes("BEGIN"));
    input.extend(build_query_bytes(
        "DECLARE c CURSOR FOR SELECT id FROM t_fetch_tag ORDER BY id",
    ));
    input.extend(build_query_bytes("FETCH ALL IN c"));
    input.extend(build_query_bytes("COMMIT"));
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run().await.expect("simple FETCH should complete");

    let messages = backend_messages(conn.writer_ref());
    let row_description = messages.iter().find(|(tag, _)| *tag == b'T').map_or_else(
        || {
            let tags: Vec<char> = messages.iter().map(|(tag, _)| char::from(*tag)).collect();
            let error = messages
                .iter()
                .rfind(|(tag, _)| *tag == b'E')
                .and_then(|(_, payload)| parse_error_response_message(payload));
            panic!("row description missing; tags={tags:?}; error={error:?}");
        },
        |(_, payload)| payload.as_slice(),
    );
    assert_eq!(
        parse_row_description_field_names(row_description),
        vec!["id"]
    );
    assert_eq!(
        parse_row_description_type_info(row_description),
        vec![(23, -1)]
    );
    assert_eq!(
        parse_row_description_origin_info(row_description),
        vec![(expected_oid, 1)]
    );
    let commands: Vec<String> = messages
        .iter()
        .filter(|(tag, _)| *tag == b'C')
        .map(|(_, payload)| parse_cstring_payload(payload).to_owned())
        .collect();
    assert!(
        commands.iter().any(|command| command == "FETCH 2"),
        "expected FETCH 2 command complete, got {commands:?}"
    );
}

#[tokio::test]
async fn simple_query_fetch_zero_reports_fetch_zero_and_keeps_cursor_position() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));
    let (session, _) = engine
        .startup(StartupParams {
            database: "default".to_owned(),
            application_name: Some("pgwire-simple-fetch-zero-seed".to_owned()),
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
            "CREATE TABLE t_fetch_zero_tag (id INT);
             INSERT INTO t_fetch_zero_tag VALUES (1), (2)",
        )
        .expect("seed fetch zero table");

    let mut input = build_startup_bytes();
    input.extend(build_query_bytes("BEGIN"));
    input.extend(build_query_bytes(
        "DECLARE c CURSOR FOR SELECT id FROM t_fetch_zero_tag ORDER BY id",
    ));
    input.extend(build_query_bytes("FETCH 0 IN c"));
    input.extend(build_query_bytes("FETCH NEXT IN c"));
    input.extend(build_query_bytes("COMMIT"));
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run().await.expect("simple FETCH 0 should complete");

    let messages = backend_messages(conn.writer_ref());
    let commands: Vec<String> = messages
        .iter()
        .filter(|(tag, _)| *tag == b'C')
        .map(|(_, payload)| parse_cstring_payload(payload).to_owned())
        .collect();
    assert!(
        commands
            .windows(2)
            .any(|window| window == ["FETCH 0", "FETCH 1"]),
        "expected FETCH 0 then FETCH 1 command completes, got {commands:?}"
    );
    let data_rows: Vec<Vec<Option<Vec<u8>>>> = messages
        .iter()
        .filter(|(tag, _)| *tag == b'D')
        .map(|(_, payload)| parse_data_row_columns(payload))
        .collect();
    assert_eq!(data_rows, vec![vec![Some(b"1".to_vec())]]);
}
