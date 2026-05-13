#[path = "extended_query_e2e_part3_describe_exec_metadata.rs"]
mod describe_exec_metadata;

#[tokio::test]
async fn extended_execute_sql_deallocate_removes_compat_statement() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_query_bytes("PREPARE stmt(int) AS SELECT $1 + 1 AS v"));
    input.extend(build_parse_bytes(
        "s_deallocate_stmt",
        "DEALLOCATE PREPARE stmt",
        &[],
    ));
    input.extend(build_bind_bytes(
        "p_deallocate_stmt",
        "s_deallocate_stmt",
        &[],
        &[],
        &[],
    ));
    input.extend(build_execute_bytes("p_deallocate_stmt", 0));
    input.extend(build_sync_bytes());
    input.extend(build_parse_bytes("s_exec_stmt", "EXECUTE stmt(41)", &[]));
    input.extend(build_bind_bytes(
        "p_exec_stmt",
        "s_exec_stmt",
        &[],
        &[],
        &[],
    ));
    input.extend(build_execute_bytes("p_exec_stmt", 0));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("extended DEALLOCATE then EXECUTE should complete with protocol error response");

    let messages = backend_messages(conn.writer_ref());
    let commands: Vec<String> = messages
        .iter()
        .filter(|(tag, _)| *tag == b'C')
        .map(|(_, payload)| parse_cstring_payload(payload).to_owned())
        .collect();
    assert!(
        commands.iter().any(|command| command == "DEALLOCATE"),
        "expected DEALLOCATE command complete, got {commands:?}"
    );

    let error_message = messages
        .iter()
        .rfind(|(tag, _)| *tag == b'E')
        .and_then(|(_, payload)| parse_error_response_message(payload))
        .expect("error response");
    assert!(
        error_message.contains("prepared statement \"stmt\" does not exist"),
        "expected missing compat statement error, got {error_message}"
    );
}

#[tokio::test]
async fn extended_parse_rejects_name_used_by_sql_prepare() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_query_bytes("PREPARE stmt AS SELECT 1"));
    input.extend(build_parse_bytes("stmt", "SELECT 2", &[]));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("extended Parse duplicate should complete with protocol error response");

    let messages = backend_messages(conn.writer_ref());
    let error_message = messages
        .iter()
        .rfind(|(tag, _)| *tag == b'E')
        .and_then(|(_, payload)| parse_error_response_message(payload))
        .expect("error response");
    assert!(
        error_message.contains("prepared statement \"stmt\" already exists"),
        "expected duplicate prepared statement error, got {error_message}"
    );
}

#[tokio::test]
async fn extended_execute_sql_deallocate_removes_protocol_statement() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes("stmt", "SELECT 1", &[]));
    input.extend(build_sync_bytes());
    input.extend(build_query_bytes("DEALLOCATE stmt"));
    input.extend(build_bind_bytes("p_stmt", "stmt", &[], &[], &[]));
    input.extend(build_execute_bytes("p_stmt", 0));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run().await.expect(
        "extended DEALLOCATE protocol statement should complete with protocol error response",
    );

    let messages = backend_messages(conn.writer_ref());
    let commands: Vec<String> = messages
        .iter()
        .filter(|(tag, _)| *tag == b'C')
        .map(|(_, payload)| parse_cstring_payload(payload).to_owned())
        .collect();
    assert!(
        commands.iter().any(|command| command == "DEALLOCATE"),
        "expected DEALLOCATE command complete, got {commands:?}"
    );

    let error_message = messages
        .iter()
        .rfind(|(tag, _)| *tag == b'E')
        .and_then(|(_, payload)| parse_error_response_message(payload))
        .expect("error response");
    assert!(
        error_message.contains("unknown prepared statement"),
        "expected missing protocol statement error, got {error_message}"
    );
}

#[tokio::test]
async fn extended_execute_sql_deallocate_all_clears_stale_pgwire_portal_slots() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes("s_old", "SELECT 1", &[]));
    input.extend(build_bind_bytes("p_old", "s_old", &[], &[], &[]));
    input.extend(build_sync_bytes());
    input.extend(build_query_bytes(
        "PREPARE dealloc_all_stmt AS DEALLOCATE ALL",
    ));
    input.extend(build_parse_bytes(
        "s_exec_deallocate",
        "EXECUTE dealloc_all_stmt",
        &[],
    ));
    input.extend(build_bind_bytes(
        "p_exec",
        "s_exec_deallocate",
        &[],
        &[],
        &[],
    ));
    input.extend(build_execute_bytes("p_exec", 0));
    input.extend(build_sync_bytes());
    input.extend(build_parse_bytes("s_new", "SELECT 2", &[]));
    input.extend(build_bind_bytes("p_new", "s_new", &[], &[], &[]));
    input.extend(build_execute_bytes("p_new", 0));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.set_max_portals(2);
    conn.run()
        .await
        .expect("extended EXECUTE dealloc_all_stmt should free pgwire portal slots for rebinding");

    let messages = backend_messages(conn.writer_ref());
    let error_messages: Vec<String> = messages
        .iter()
        .filter(|(tag, _)| *tag == b'E')
        .filter_map(|(_, payload)| parse_error_response_message(payload))
        .collect();
    assert!(
        error_messages.is_empty(),
        "unexpected ErrorResponse while rebinding portal after extended EXECUTE dealloc_all_stmt: {error_messages:?}"
    );
    assert_eq!(
        messages.iter().filter(|(tag, _)| *tag == b'2').count(),
        3,
        "expected all three Bind messages to succeed"
    );
    let commands: Vec<String> = messages
        .iter()
        .filter(|(tag, _)| *tag == b'C')
        .map(|(_, payload)| parse_cstring_payload(payload).to_owned())
        .collect();
    assert!(
        commands.iter().any(|command| command == "DEALLOCATE"),
        "expected DEALLOCATE command complete, got {commands:?}"
    );
    let data_row = messages
        .iter()
        .rfind(|(tag, _)| *tag == b'D')
        .map(|(_, payload)| payload.as_slice())
        .expect("data row from rebound portal");
    assert_eq!(parse_data_row_columns(data_row), vec![Some(b"2".to_vec())]);
}

#[tokio::test]
async fn extended_execute_sql_close_clears_stale_pgwire_portal_slots() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes("s_old", "SELECT 1", &[]));
    input.extend(build_bind_bytes("p_old", "s_old", &[], &[], &[]));
    input.extend(build_sync_bytes());
    input.extend(build_query_bytes("PREPARE close_stmt AS CLOSE p_old"));
    input.extend(build_parse_bytes("s_exec_close", "EXECUTE close_stmt", &[]));
    input.extend(build_bind_bytes("p_exec", "s_exec_close", &[], &[], &[]));
    input.extend(build_execute_bytes("p_exec", 0));
    input.extend(build_sync_bytes());
    input.extend(build_parse_bytes("s_new", "SELECT 2", &[]));
    input.extend(build_bind_bytes("p_new", "s_new", &[], &[], &[]));
    input.extend(build_execute_bytes("p_new", 0));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.set_max_portals(2);
    conn.run()
        .await
        .expect("extended EXECUTE close_stmt should free pgwire portal slots for rebinding");

    let messages = backend_messages(conn.writer_ref());
    assert!(
        !messages.iter().any(|(tag, _)| *tag == b'E'),
        "unexpected ErrorResponse while rebinding portal after extended EXECUTE close_stmt"
    );
    assert_eq!(
        messages.iter().filter(|(tag, _)| *tag == b'2').count(),
        3,
        "expected all three Bind messages to succeed"
    );
    let commands: Vec<String> = messages
        .iter()
        .filter(|(tag, _)| *tag == b'C')
        .map(|(_, payload)| parse_cstring_payload(payload).to_owned())
        .collect();
    assert!(
        commands.iter().any(|command| command == "CLOSE CURSOR"),
        "expected CLOSE CURSOR command complete, got {commands:?}"
    );
    let data_row = messages
        .iter()
        .rfind(|(tag, _)| *tag == b'D')
        .map(|(_, payload)| payload.as_slice())
        .expect("data row from rebound portal");
    assert_eq!(parse_data_row_columns(data_row), vec![Some(b"2".to_vec())]);
}

#[tokio::test]
async fn extended_execute_sql_rollback_to_savepoint_clears_stale_pgwire_portal_slots() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_query_bytes("BEGIN"));
    input.extend(build_query_bytes("SAVEPOINT sp1"));
    input.extend(build_parse_bytes("s_old", "SELECT 1", &[]));
    input.extend(build_bind_bytes("p_old", "s_old", &[], &[], &[]));
    input.extend(build_sync_bytes());
    input.extend(build_query_bytes(
        "PREPARE rb_stmt AS ROLLBACK TO SAVEPOINT sp1",
    ));
    input.extend(build_parse_bytes("s_exec_rb", "EXECUTE rb_stmt", &[]));
    input.extend(build_bind_bytes("p_exec", "s_exec_rb", &[], &[], &[]));
    input.extend(build_execute_bytes("p_exec", 0));
    input.extend(build_sync_bytes());
    input.extend(build_parse_bytes("s_new", "SELECT 2", &[]));
    input.extend(build_bind_bytes("p_new", "s_new", &[], &[], &[]));
    input.extend(build_execute_bytes("p_new", 0));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.set_max_portals(2);
    conn.run()
        .await
        .expect("extended EXECUTE rb_stmt should free post-savepoint pgwire portal slots");

    let messages = backend_messages(conn.writer_ref());
    assert!(
        !messages.iter().any(|(tag, _)| *tag == b'E'),
        "unexpected ErrorResponse while rebinding portal after extended EXECUTE rb_stmt"
    );
    assert_eq!(
        messages.iter().filter(|(tag, _)| *tag == b'2').count(),
        3,
        "expected all three Bind messages to succeed"
    );
    let data_row = messages
        .iter()
        .rfind(|(tag, _)| *tag == b'D')
        .map(|(_, payload)| payload.as_slice())
        .expect("data row from rebound portal");
    assert_eq!(parse_data_row_columns(data_row), vec![Some(b"2".to_vec())]);
}

#[tokio::test]
async fn extended_execute_sql_commit_clears_stale_pgwire_portal_slots() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_query_bytes("BEGIN"));
    input.extend(build_parse_bytes("s_old", "SELECT 1", &[]));
    input.extend(build_bind_bytes("p_old", "s_old", &[], &[], &[]));
    input.extend(build_sync_bytes());
    input.extend(build_query_bytes("PREPARE commit_stmt AS COMMIT"));
    input.extend(build_parse_bytes(
        "s_exec_commit",
        "EXECUTE commit_stmt",
        &[],
    ));
    input.extend(build_bind_bytes("p_exec", "s_exec_commit", &[], &[], &[]));
    input.extend(build_execute_bytes("p_exec", 0));
    input.extend(build_sync_bytes());
    input.extend(build_parse_bytes("s_new", "SELECT 2", &[]));
    input.extend(build_bind_bytes("p_new", "s_new", &[], &[], &[]));
    input.extend(build_execute_bytes("p_new", 0));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.set_max_portals(2);
    conn.run()
        .await
        .expect("extended EXECUTE commit_stmt should clear pgwire portal slots after COMMIT");

    let messages = backend_messages(conn.writer_ref());
    assert!(
        !messages.iter().any(|(tag, _)| *tag == b'E'),
        "unexpected ErrorResponse while rebinding portal after extended EXECUTE commit_stmt"
    );
    assert_eq!(
        messages.iter().filter(|(tag, _)| *tag == b'2').count(),
        3,
        "expected all three Bind messages to succeed"
    );
    let data_row = messages
        .iter()
        .rfind(|(tag, _)| *tag == b'D')
        .map(|(_, payload)| payload.as_slice())
        .expect("data row from rebound portal");
    assert_eq!(parse_data_row_columns(data_row), vec![Some(b"2".to_vec())]);
}

#[tokio::test]
async fn extended_execute_deallocate_all_clears_stale_pgwire_portal_slots() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes("s_old", "SELECT 1", &[]));
    input.extend(build_bind_bytes("p_old", "s_old", &[], &[], &[]));
    input.extend(build_sync_bytes());
    input.extend(build_parse_bytes("s_deallocate_all", "DEALLOCATE ALL", &[]));
    input.extend(build_bind_bytes(
        "p_deallocate",
        "s_deallocate_all",
        &[],
        &[],
        &[],
    ));
    input.extend(build_execute_bytes("p_deallocate", 0));
    input.extend(build_sync_bytes());
    input.extend(build_parse_bytes("s_new", "SELECT 2", &[]));
    input.extend(build_bind_bytes("p_new", "s_new", &[], &[], &[]));
    input.extend(build_execute_bytes("p_new", 0));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.set_max_portals(2);
    conn.run()
        .await
        .expect("extended DEALLOCATE ALL should free pgwire portal slot for rebinding");

    let messages = backend_messages(conn.writer_ref());
    assert!(
        !messages.iter().any(|(tag, _)| *tag == b'E'),
        "unexpected ErrorResponse while rebinding portal after extended DEALLOCATE ALL"
    );
    assert_eq!(
        messages.iter().filter(|(tag, _)| *tag == b'2').count(),
        3,
        "expected all three Bind messages to succeed"
    );
    let commands: Vec<String> = messages
        .iter()
        .filter(|(tag, _)| *tag == b'C')
        .map(|(_, payload)| parse_cstring_payload(payload).to_owned())
        .collect();
    assert!(
        commands.iter().any(|command| command == "DEALLOCATE"),
        "expected DEALLOCATE command complete, got {commands:?}"
    );
    let data_row = messages
        .iter()
        .rfind(|(tag, _)| *tag == b'D')
        .map(|(_, payload)| payload.as_slice())
        .expect("data row from rebound portal");
    assert_eq!(parse_data_row_columns(data_row), vec![Some(b"2".to_vec())]);
}

#[tokio::test]
async fn extended_execute_close_clears_stale_pgwire_portal_slots() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes("s_old", "SELECT 1", &[]));
    input.extend(build_bind_bytes("p_old", "s_old", &[], &[], &[]));
    input.extend(build_sync_bytes());
    input.extend(build_parse_bytes("s_close_old", "CLOSE p_old", &[]));
    input.extend(build_bind_bytes("p_close", "s_close_old", &[], &[], &[]));
    input.extend(build_execute_bytes("p_close", 0));
    input.extend(build_sync_bytes());
    input.extend(build_parse_bytes("s_new", "SELECT 2", &[]));
    input.extend(build_bind_bytes("p_new", "s_new", &[], &[], &[]));
    input.extend(build_execute_bytes("p_new", 0));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.set_max_portals(2);
    conn.run()
        .await
        .expect("extended CLOSE should free pgwire portal slot for rebinding");

    let messages = backend_messages(conn.writer_ref());
    assert!(
        !messages.iter().any(|(tag, _)| *tag == b'E'),
        "unexpected ErrorResponse while rebinding portal after extended CLOSE"
    );
    assert_eq!(
        messages.iter().filter(|(tag, _)| *tag == b'2').count(),
        3,
        "expected all three Bind messages to succeed"
    );
    let commands: Vec<String> = messages
        .iter()
        .filter(|(tag, _)| *tag == b'C')
        .map(|(_, payload)| parse_cstring_payload(payload).to_owned())
        .collect();
    assert!(
        commands.iter().any(|command| command == "CLOSE CURSOR"),
        "expected CLOSE CURSOR command complete, got {commands:?}"
    );
    let data_row = messages
        .iter()
        .rfind(|(tag, _)| *tag == b'D')
        .map(|(_, payload)| payload.as_slice())
        .expect("data row from rebound portal");
    assert_eq!(parse_data_row_columns(data_row), vec![Some(b"2".to_vec())]);
}

#[tokio::test]
async fn extended_execute_sql_explain_execute_returns_plan_rows() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_query_bytes("PREPARE stmt(int) AS SELECT $1 + 1 AS v"));
    input.extend(build_parse_bytes(
        "s_explain_exec_stmt",
        "EXPLAIN EXECUTE stmt(41)",
        &[],
    ));
    input.extend(build_bind_bytes(
        "p_explain_exec_stmt",
        "s_explain_exec_stmt",
        &[],
        &[],
        &[],
    ));
    input.extend(build_describe_bytes(b'P', "p_explain_exec_stmt"));
    input.extend(build_execute_bytes("p_explain_exec_stmt", 0));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("extended EXPLAIN EXECUTE stmt should complete");

    let messages = backend_messages(conn.writer_ref());
    let row_description = messages
        .iter()
        .find(|(tag, _)| *tag == b'T')
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
    assert_eq!(
        parse_row_description_origin_info(row_description),
        vec![(0, 0)]
    );

    let data_rows: Vec<Vec<Option<Vec<u8>>>> = messages
        .iter()
        .filter(|(tag, _)| *tag == b'D')
        .map(|(_, payload)| parse_data_row_columns(payload))
        .collect();
    assert!(
        data_rows
            .iter()
            .any(|row| row == &vec![Some(b"Result".to_vec())]),
        "expected plan rows to contain Result, got {data_rows:?}"
    );

    let command = messages
        .iter()
        .rfind(|(tag, _)| *tag == b'C')
        .map(|(_, payload)| parse_cstring_payload(payload))
        .expect("command complete");
    assert_eq!(command, "EXPLAIN");
}

#[tokio::test]
async fn extended_describe_statement_for_sql_execute_rechecks_metadata_after_parse() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_exec_late_desc",
        "EXECUTE stmt(41)",
        &[],
    ));
    input.extend(build_query_bytes("PREPARE stmt(int) AS SELECT $1 + 1 AS v"));
    input.extend(build_describe_bytes(b'S', "s_exec_late_desc"));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("extended EXECUTE statement describe should re-check metadata");

    let messages = backend_messages(conn.writer_ref());
    let row_description = messages
        .iter()
        .rfind(|(tag, _)| *tag == b'T')
        .map(|(_, payload)| payload.as_slice())
        .expect("row description");
    assert_eq!(
        parse_row_description_field_names(row_description),
        vec!["v"]
    );
    assert_eq!(
        parse_row_description_type_info(row_description),
        vec![(23, -1)]
    );
}

#[tokio::test]
async fn extended_describe_statement_for_missing_sql_execute_reports_error() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_exec_missing_stmt",
        "EXECUTE missing_stmt(41)",
        &[],
    ));
    input.extend(build_describe_bytes(b'S', "s_exec_missing_stmt"));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("extended EXECUTE missing statement describe should complete with protocol error");

    let messages = backend_messages(conn.writer_ref());
    assert!(
        messages.iter().any(|(tag, _)| *tag == b'1'),
        "expected ParseComplete before Describe failure"
    );
    assert!(
        !messages.iter().any(|(tag, _)| *tag == b'T'),
        "missing statement describe should not emit RowDescription"
    );
    let error_payload = messages
        .iter()
        .find(|(tag, _)| *tag == b'E')
        .map(|(_, payload)| payload.as_slice())
        .expect("error response");
    let error_message =
        parse_error_response_message(error_payload).expect("parse error response message");
    assert!(
        error_message.contains("prepared statement \"missing_stmt\" does not exist"),
        "expected missing prepared statement error, got {error_message:?}"
    );
}

#[tokio::test]
async fn extended_describe_statement_for_missing_sql_explain_execute_reports_error() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_explain_exec_missing_stmt",
        "EXPLAIN EXECUTE missing_stmt(41)",
        &[],
    ));
    input.extend(build_describe_bytes(b'S', "s_explain_exec_missing_stmt"));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run().await.expect(
        "extended EXPLAIN EXECUTE missing statement describe should complete with protocol error",
    );

    let messages = backend_messages(conn.writer_ref());
    assert!(
        messages.iter().any(|(tag, _)| *tag == b'1'),
        "expected ParseComplete before Describe failure"
    );
    assert!(
        !messages.iter().any(|(tag, _)| *tag == b'T'),
        "missing statement describe should not emit RowDescription"
    );
    let error_payload = messages
        .iter()
        .find(|(tag, _)| *tag == b'E')
        .map(|(_, payload)| payload.as_slice())
        .expect("error response");
    let error_message =
        parse_error_response_message(error_payload).expect("parse error response message");
    assert!(
        error_message.contains("prepared statement \"missing_stmt\" does not exist"),
        "expected missing prepared statement error, got {error_message:?}"
    );
}

#[tokio::test]
async fn extended_describe_statement_for_unknown_show_variable_reports_error() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_show_missing_setting",
        "SHOW no_such_setting",
        &[],
    ));
    input.extend(build_describe_bytes(b'S', "s_show_missing_setting"));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("extended SHOW missing setting describe should complete with protocol error");

    let messages = backend_messages(conn.writer_ref());
    assert!(
        messages.iter().any(|(tag, _)| *tag == b'1'),
        "expected ParseComplete before Describe failure"
    );
    assert!(
        !messages.iter().any(|(tag, _)| *tag == b'T'),
        "missing SHOW variable describe should not emit RowDescription"
    );
    let error_payload = messages
        .iter()
        .find(|(tag, _)| *tag == b'E')
        .map(|(_, payload)| payload.as_slice())
        .expect("error response");
    let error_message =
        parse_error_response_message(error_payload).expect("parse error response message");
    assert!(
        error_message.contains("unrecognized configuration parameter"),
        "expected unknown configuration parameter error, got {error_message:?}"
    );
}

#[tokio::test]
async fn extended_describe_statement_for_unknown_set_role_reports_error() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));
    let (session, _) = engine
        .startup(StartupParams {
            database: "default".to_owned(),
            application_name: Some("pgwire-extended-set-role-seed".to_owned()),
            options: std::collections::BTreeMap::new(),
            credential: Credential::Anonymous {
                user: "test".to_owned(),
            },
            transport: TransportInfo::in_process(),
        })
        .expect("startup seed session");
    engine
        .execute_sql(&session, "CREATE ROLE bootstrap_admin SUPERUSER LOGIN")
        .expect("create bootstrap admin role");
    let (admin_session, _) = engine
        .startup(StartupParams {
            database: "default".to_owned(),
            application_name: Some("pgwire-extended-set-role-admin".to_owned()),
            options: std::collections::BTreeMap::new(),
            credential: Credential::Anonymous {
                user: "bootstrap_admin".to_owned(),
            },
            transport: TransportInfo::in_process(),
        })
        .expect("startup bootstrap admin session");
    engine
        .execute_sql(&admin_session, "CREATE ROLE existing_role LOGIN")
        .expect("create role to activate role system");

    let mut input = build_startup_bytes_with_user("bootstrap_admin");
    input.extend(build_parse_bytes(
        "s_set_role_missing_role",
        "SET ROLE no_such_role",
        &[],
    ));
    input.extend(build_describe_bytes(b'S', "s_set_role_missing_role"));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("extended SET ROLE missing target describe should complete with protocol error");

    let messages = backend_messages(conn.writer_ref());
    assert!(messages.iter().any(|(tag, _)| *tag == b'1'));
    assert!(!messages.iter().any(|(tag, _)| *tag == b'T'));
    let error_payload = messages
        .iter()
        .find(|(tag, _)| *tag == b'E')
        .map(|(_, payload)| payload.as_slice())
        .expect("error response");
    let error_message =
        parse_error_response_message(error_payload).expect("parse error response message");
    assert!(
        error_message.contains("role \"no_such_role\" does not exist"),
        "expected unknown role error, got {error_message:?}"
    );
}

#[tokio::test]
async fn extended_describe_statement_for_set_role_without_membership_reports_error() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));
    let (session, _) = engine
        .startup(StartupParams {
            database: "default".to_owned(),
            application_name: Some("pgwire-extended-set-role-membership-seed".to_owned()),
            options: std::collections::BTreeMap::new(),
            credential: Credential::Anonymous {
                user: "test".to_owned(),
            },
            transport: TransportInfo::in_process(),
        })
        .expect("startup seed session");
    engine
        .execute_sql(&session, "CREATE ROLE bootstrap_admin SUPERUSER LOGIN")
        .expect("create bootstrap admin role");
    let (admin_session, _) = engine
        .startup(StartupParams {
            database: "default".to_owned(),
            application_name: Some("pgwire-extended-set-role-membership-admin".to_owned()),
            options: std::collections::BTreeMap::new(),
            credential: Credential::Anonymous {
                user: "bootstrap_admin".to_owned(),
            },
            transport: TransportInfo::in_process(),
        })
        .expect("startup bootstrap admin session");
    engine
        .execute_sql(&admin_session, "CREATE ROLE app_user LOGIN")
        .expect("create app_user role");
    engine
        .execute_sql(&admin_session, "CREATE ROLE target_role LOGIN")
        .expect("create target role");

    let mut input = build_startup_bytes_with_user("app_user");
    input.extend(build_parse_bytes(
        "s_set_role_without_membership",
        "SET ROLE target_role",
        &[],
    ));
    input.extend(build_describe_bytes(b'S', "s_set_role_without_membership"));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run().await.expect(
        "extended SET ROLE without membership describe should complete with protocol error",
    );

    let messages = backend_messages(conn.writer_ref());
    assert!(messages.iter().any(|(tag, _)| *tag == b'1'));
    assert!(!messages.iter().any(|(tag, _)| *tag == b'T'));
    let error_payload = messages
        .iter()
        .find(|(tag, _)| *tag == b'E')
        .map(|(_, payload)| payload.as_slice())
        .expect("error response");
    let error_message =
        parse_error_response_message(error_payload).expect("parse error response message");
    assert!(
        error_message.contains("permission denied to set role"),
        "expected set role privilege error, got {error_message:?}"
    );
}

#[tokio::test]
async fn extended_describe_statement_for_unknown_set_session_authorization_reports_error() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));
    let (session, _) = engine
        .startup(StartupParams {
            database: "default".to_owned(),
            application_name: Some("pgwire-extended-set-session-auth-seed".to_owned()),
            options: std::collections::BTreeMap::new(),
            credential: Credential::Anonymous {
                user: "test".to_owned(),
            },
            transport: TransportInfo::in_process(),
        })
        .expect("startup seed session");
    engine
        .execute_sql(&session, "CREATE ROLE bootstrap_admin SUPERUSER LOGIN")
        .expect("create bootstrap admin role");
    let (admin_session, _) = engine
        .startup(StartupParams {
            database: "default".to_owned(),
            application_name: Some("pgwire-extended-set-session-auth-admin".to_owned()),
            options: std::collections::BTreeMap::new(),
            credential: Credential::Anonymous {
                user: "bootstrap_admin".to_owned(),
            },
            transport: TransportInfo::in_process(),
        })
        .expect("startup bootstrap admin session");
    engine
        .execute_sql(&admin_session, "CREATE ROLE existing_role LOGIN")
        .expect("create role to activate role system");

    let mut input = build_startup_bytes_with_user("bootstrap_admin");
    input.extend(build_parse_bytes(
        "s_set_session_auth_missing_role",
        "SET SESSION AUTHORIZATION no_such_role",
        &[],
    ));
    input.extend(build_describe_bytes(
        b'S',
        "s_set_session_auth_missing_role",
    ));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect(
            "extended SET SESSION AUTHORIZATION missing target describe should complete with protocol error",
        );

    let messages = backend_messages(conn.writer_ref());
    assert!(messages.iter().any(|(tag, _)| *tag == b'1'));
    assert!(!messages.iter().any(|(tag, _)| *tag == b'T'));
    let error_payload = messages
        .iter()
        .find(|(tag, _)| *tag == b'E')
        .map(|(_, payload)| payload.as_slice())
        .expect("error response");
    let error_message =
        parse_error_response_message(error_payload).expect("parse error response message");
    assert!(
        error_message.contains("role \"no_such_role\" does not exist"),
        "expected unknown role error, got {error_message:?}"
    );
}

#[tokio::test]
async fn extended_describe_statement_for_set_session_authorization_without_superuser_reports_error()
{
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));
    let (session, _) = engine
        .startup(StartupParams {
            database: "default".to_owned(),
            application_name: Some("pgwire-extended-set-session-auth-priv-seed".to_owned()),
            options: std::collections::BTreeMap::new(),
            credential: Credential::Anonymous {
                user: "test".to_owned(),
            },
            transport: TransportInfo::in_process(),
        })
        .expect("startup seed session");
    engine
        .execute_sql(&session, "CREATE ROLE bootstrap_admin SUPERUSER LOGIN")
        .expect("create bootstrap admin role");
    let (admin_session, _) = engine
        .startup(StartupParams {
            database: "default".to_owned(),
            application_name: Some("pgwire-extended-set-session-auth-priv-admin".to_owned()),
            options: std::collections::BTreeMap::new(),
            credential: Credential::Anonymous {
                user: "bootstrap_admin".to_owned(),
            },
            transport: TransportInfo::in_process(),
        })
        .expect("startup bootstrap admin session");
    engine
        .execute_sql(&admin_session, "CREATE ROLE app_user LOGIN")
        .expect("create app_user role");

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_set_session_auth_without_superuser",
        "SET SESSION AUTHORIZATION app_user",
        &[],
    ));
    input.extend(build_describe_bytes(
        b'S',
        "s_set_session_auth_without_superuser",
    ));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect(
            "extended SET SESSION AUTHORIZATION without superuser describe should complete with protocol error",
        );

    let messages = backend_messages(conn.writer_ref());
    assert!(messages.iter().any(|(tag, _)| *tag == b'1'));
    assert!(!messages.iter().any(|(tag, _)| *tag == b'T'));
    let error_payload = messages
        .iter()
        .find(|(tag, _)| *tag == b'E')
        .map(|(_, payload)| payload.as_slice())
        .expect("error response");
    let error_message =
        parse_error_response_message(error_payload).expect("parse error response message");
    assert!(
        error_message.contains("permission denied to set session authorization"),
        "expected set session authorization privilege error, got {error_message:?}"
    );
}

#[tokio::test]
async fn extended_describe_statement_for_invalid_set_statement_timeout_reports_error() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_set_statement_timeout_bogus",
        "SET statement_timeout = bogus",
        &[],
    ));
    input.extend(build_describe_bytes(b'S', "s_set_statement_timeout_bogus"));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run().await.expect(
        "extended invalid SET statement_timeout describe should complete with protocol error",
    );

    let messages = backend_messages(conn.writer_ref());
    assert!(messages.iter().any(|(tag, _)| *tag == b'1'));
    assert!(!messages.iter().any(|(tag, _)| *tag == b'T'));
    let error_payload = messages
        .iter()
        .find(|(tag, _)| *tag == b'E')
        .map(|(_, payload)| payload.as_slice())
        .expect("error response");
    let error_message =
        parse_error_response_message(error_payload).expect("parse error response message");
    assert!(
        error_message.contains("invalid value for timeout parameter"),
        "expected invalid timeout error, got {error_message:?}"
    );
}

#[tokio::test]
async fn extended_describe_statement_for_invalid_set_client_encoding_reports_error() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_set_client_encoding_invalid",
        "SET client_encoding = latin1",
        &[],
    ));
    input.extend(build_describe_bytes(b'S', "s_set_client_encoding_invalid"));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run().await.expect(
        "extended invalid SET client_encoding describe should complete with protocol error",
    );

    let messages = backend_messages(conn.writer_ref());
    assert!(messages.iter().any(|(tag, _)| *tag == b'1'));
    assert!(!messages.iter().any(|(tag, _)| *tag == b'T'));
    let error_payload = messages
        .iter()
        .find(|(tag, _)| *tag == b'E')
        .map(|(_, payload)| payload.as_slice())
        .expect("error response");
    let error_message =
        parse_error_response_message(error_payload).expect("parse error response message");
    assert!(
        error_message.contains("only UTF8 encoding is supported"),
        "expected encoding error, got {error_message:?}"
    );
}
