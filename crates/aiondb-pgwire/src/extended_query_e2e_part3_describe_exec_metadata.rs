use super::*;

#[tokio::test]
async fn extended_describe_statement_for_missing_sql_deallocate_reports_error() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_deallocate_missing_stmt",
        "DEALLOCATE stmt",
        &[],
    ));
    input.extend(build_describe_bytes(b'S', "s_deallocate_missing_stmt"));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run().await.expect(
        "extended DEALLOCATE missing statement describe should complete with protocol error",
    );

    let messages = backend_messages(conn.writer_ref());
    assert!(
        messages.iter().any(|(tag, _)| *tag == b'1'),
        "expected ParseComplete before Describe failure"
    );
    assert!(
        !messages.iter().any(|(tag, _)| *tag == b'T'),
        "missing statement deallocate describe should not emit RowDescription"
    );
    let error_payload = messages
        .iter()
        .find(|(tag, _)| *tag == b'E')
        .map(|(_, payload)| payload.as_slice())
        .expect("error response");
    let error_message =
        parse_error_response_message(error_payload).expect("parse error response message");
    assert!(
        error_message.contains("prepared statement \"stmt\" does not exist"),
        "expected missing prepared statement error, got {error_message:?}"
    );
}

#[tokio::test]
async fn extended_describe_statement_for_sql_execute_with_string_comma_argument_reports_metadata() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_query_bytes(
        "PREPARE stmt(int, text) AS SELECT $1 AS id, $2 AS note",
    ));
    input.extend(build_parse_bytes(
        "s_exec_stmt_with_string_comma",
        "EXECUTE stmt(1, 'a,b')",
        &[],
    ));
    input.extend(build_describe_bytes(b'S', "s_exec_stmt_with_string_comma"));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("extended EXECUTE describe with string comma argument should succeed");

    let messages = backend_messages(conn.writer_ref());
    let row_description = messages
        .iter()
        .rfind(|(tag, _)| *tag == b'T')
        .map(|(_, payload)| payload.as_slice())
        .expect("row description");
    assert_eq!(
        parse_row_description_field_names(row_description),
        vec!["id", "note"]
    );
    assert_eq!(
        parse_row_description_type_info(row_description),
        vec![(23, -1), (25, -1)]
    );
}

#[tokio::test]
async fn extended_describe_statement_for_sql_execute_with_comment_comma_argument_reports_metadata()
{
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_query_bytes(
        "PREPARE stmt(int, int) AS SELECT $1 AS a, $2 AS b",
    ));
    input.extend(build_parse_bytes(
        "s_exec_stmt_with_comment_comma",
        "EXECUTE stmt(1 /*, ignored */, 2)",
        &[],
    ));
    input.extend(build_describe_bytes(b'S', "s_exec_stmt_with_comment_comma"));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("extended EXECUTE describe with comment comma argument should succeed");

    let messages = backend_messages(conn.writer_ref());
    let row_description = messages
        .iter()
        .rfind(|(tag, _)| *tag == b'T')
        .map(|(_, payload)| payload.as_slice())
        .expect("row description");
    assert_eq!(
        parse_row_description_field_names(row_description),
        vec!["a", "b"]
    );
    assert_eq!(
        parse_row_description_type_info(row_description),
        vec![(23, -1), (23, -1)]
    );
}

#[tokio::test]
async fn extended_describe_statement_for_sql_execute_wrapped_missing_close_reports_error() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_query_bytes(
        "PREPARE close_stmt AS CLOSE missing_cursor",
    ));
    input.extend(build_parse_bytes(
        "s_exec_close_missing_cursor",
        "EXECUTE close_stmt",
        &[],
    ));
    input.extend(build_describe_bytes(b'S', "s_exec_close_missing_cursor"));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run().await.expect(
        "extended EXECUTE wrapped missing close describe should complete with protocol error",
    );

    let messages = backend_messages(conn.writer_ref());
    assert!(
        messages.iter().any(|(tag, _)| *tag == b'1'),
        "expected ParseComplete before Describe failure"
    );
    assert!(
        !messages.iter().any(|(tag, _)| *tag == b'T'),
        "missing cursor wrapped close describe should not emit RowDescription"
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
async fn extended_describe_statement_for_deallocate_protocol_statement_succeeds() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes("stmt", "SELECT 1", &[]));
    input.extend(build_sync_bytes());
    input.extend(build_parse_bytes(
        "s_deallocate_stmt",
        "DEALLOCATE stmt",
        &[],
    ));
    input.extend(build_describe_bytes(b'S', "s_deallocate_stmt"));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("extended DEALLOCATE protocol statement describe should succeed");

    let messages = backend_messages(conn.writer_ref());
    assert!(
        messages.iter().any(|(tag, _)| *tag == b'n'),
        "expected NoData for DEALLOCATE describe, got {messages:?}"
    );
    assert!(
        !messages.iter().any(|(tag, _)| *tag == b'E'),
        "unexpected ErrorResponse for DEALLOCATE describe"
    );
}

#[tokio::test]
async fn extended_describe_statement_for_sql_execute_wrapped_protocol_deallocate_succeeds() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes("stmt", "SELECT 1", &[]));
    input.extend(build_sync_bytes());
    input.extend(build_query_bytes("PREPARE dealloc_stmt AS DEALLOCATE stmt"));
    input.extend(build_parse_bytes(
        "s_exec_dealloc_protocol_stmt",
        "EXECUTE dealloc_stmt",
        &[],
    ));
    input.extend(build_describe_bytes(b'S', "s_exec_dealloc_protocol_stmt"));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("extended EXECUTE wrapped protocol deallocate describe should succeed");

    let messages = backend_messages(conn.writer_ref());
    assert!(
        messages.iter().any(|(tag, _)| *tag == b'n'),
        "expected NoData for wrapped DEALLOCATE describe, got {messages:?}"
    );
    assert!(
        !messages.iter().any(|(tag, _)| *tag == b'E'),
        "unexpected ErrorResponse for wrapped DEALLOCATE describe"
    );
}

#[tokio::test]
async fn extended_describe_statement_for_sql_execute_wrapped_missing_deallocate_reports_error() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_query_bytes("PREPARE dealloc_stmt AS DEALLOCATE stmt"));
    input.extend(build_parse_bytes(
        "s_exec_dealloc_missing_stmt",
        "EXECUTE dealloc_stmt",
        &[],
    ));
    input.extend(build_describe_bytes(b'S', "s_exec_dealloc_missing_stmt"));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run().await.expect(
        "extended EXECUTE wrapped missing deallocate describe should complete with protocol error",
    );

    let messages = backend_messages(conn.writer_ref());
    assert!(
        messages.iter().any(|(tag, _)| *tag == b'1'),
        "expected ParseComplete before Describe failure"
    );
    assert!(
        !messages.iter().any(|(tag, _)| *tag == b'T'),
        "missing statement wrapped deallocate describe should not emit RowDescription"
    );
    let error_payload = messages
        .iter()
        .find(|(tag, _)| *tag == b'E')
        .map(|(_, payload)| payload.as_slice())
        .expect("error response");
    let error_message =
        parse_error_response_message(error_payload).expect("parse error response message");
    assert!(
        error_message.contains("prepared statement \"stmt\" does not exist"),
        "expected missing prepared statement error, got {error_message:?}"
    );
}

#[tokio::test]
async fn extended_describe_portal_for_sql_execute_rechecks_metadata_after_parse() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_exec_portal_late_desc",
        "EXECUTE stmt(41)",
        &[],
    ));
    input.extend(build_query_bytes("PREPARE stmt(int) AS SELECT $1 + 1 AS v"));
    input.extend(build_bind_bytes(
        "p_exec_portal_late_desc",
        "s_exec_portal_late_desc",
        &[],
        &[],
        &[],
    ));
    input.extend(build_describe_bytes(b'P', "p_exec_portal_late_desc"));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("extended EXECUTE portal describe should re-check metadata");

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
