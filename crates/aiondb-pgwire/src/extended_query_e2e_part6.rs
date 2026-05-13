#[path = "extended_query_e2e_part6_portal_copy_notice.rs"]
mod portal_copy_notice;

#[tokio::test]
async fn extended_describe_statement_preserves_information_schema_fast_path_cast_metadata() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_information_schema_casts",
        "SELECT CAST(table_name AS VARCHAR), CAST(table_type AS CHAR(3)) FROM information_schema.tables ORDER BY table_name LIMIT 1",
        &[],
    ));
    input.extend(build_describe_bytes(b'S', "s_information_schema_casts"));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("information_schema fast-path cast describe should succeed");

    let messages = backend_messages(conn.writer_ref());
    let row_description = messages
        .iter()
        .find(|(tag, _)| *tag == b'T')
        .map(|(_, payload)| payload.as_slice())
        .expect("row description");
    assert_eq!(
        parse_row_description_type_info(row_description),
        vec![(1043, -1), (1042, 7)]
    );
}

#[tokio::test]
async fn extended_describe_statement_accepts_explicit_oid_array_param_oid() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_oid_array_hint",
        "SELECT cardinality($1)",
        &[1028],
    ));
    input.extend(build_describe_bytes(b'S', "s_oid_array_hint"));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("describe statement should accept explicit oid[] param oid");

    let param_oids = backend_messages(conn.writer_ref())
        .iter()
        .find(|(tag, _)| *tag == b't')
        .map(|(_, payload)| parse_parameter_description_oids(payload))
        .expect("parameter description");
    assert_eq!(param_oids, vec![1028]);
}

/// Parse -> Bind with NULL parameter -> Execute -> Sync -> Terminate.
///
/// Tests that NULL parameter values are handled.
#[tokio::test]
async fn extended_bind_with_null_param() {
    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes("", "SELECT $1", &[23]));
    input.extend(build_bind_bytes("", "", &[0], &[None], &[]));
    input.extend(build_execute_bytes("", 0));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let engine = Arc::new(ExtendedMockEngine);
    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    let result = conn.run().await;
    assert!(result.is_ok(), "bind with NULL param should work");
}

#[tokio::test]
async fn extended_execute_portal_accumulates_select_row_count_across_suspended_batches() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_rows",
        "SELECT x FROM (VALUES (1), (2)) AS t(x) ORDER BY x",
        &[],
    ));
    input.extend(build_bind_bytes("p_rows", "s_rows", &[], &[], &[]));
    input.extend(build_execute_bytes("p_rows", 1));
    input.extend(build_execute_bytes("p_rows", 1));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("suspended portal executes should succeed against real engine");

    let messages = backend_messages(conn.writer_ref());
    let tags: Vec<u8> = messages.iter().map(|(tag, _)| *tag).collect();
    assert!(
        tags.windows(2).any(|window| window == [b'D', b's']),
        "expected a DataRow followed by PortalSuspended on the first Execute"
    );

    let last_command = messages
        .iter()
        .rfind(|(tag, _)| *tag == b'C')
        .map(|(_, payload)| parse_cstring_payload(payload))
        .expect("command complete");
    assert_eq!(last_command, "SELECT 2");

    let data_rows: Vec<&[u8]> = messages
        .iter()
        .filter(|(tag, _)| *tag == b'D')
        .map(|(_, payload)| payload.as_slice())
        .collect();
    assert_eq!(
        data_rows.len(),
        2,
        "expected two data rows across two batches"
    );
    assert_eq!(
        parse_data_row_columns(data_rows[0]),
        vec![Some(b"1".to_vec())]
    );
    assert_eq!(
        parse_data_row_columns(data_rows[1]),
        vec![Some(b"2".to_vec())]
    );
}

#[tokio::test]
async fn simple_commit_clears_stale_pgwire_portal_slots() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_query_bytes("BEGIN"));
    input.extend(build_parse_bytes("s_old", "SELECT 1", &[]));
    input.extend(build_bind_bytes("p_old", "s_old", &[], &[], &[]));
    input.extend(build_sync_bytes());
    input.extend(build_query_bytes("COMMIT"));
    input.extend(build_parse_bytes("s_new", "SELECT 2", &[]));
    input.extend(build_bind_bytes("p_new", "s_new", &[], &[], &[]));
    input.extend(build_execute_bytes("p_new", 0));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.set_max_portals(1);
    conn.run()
        .await
        .expect("commit should free pgwire portal slot for rebinding");

    let messages = backend_messages(conn.writer_ref());
    assert!(
        !messages.iter().any(|(tag, _)| *tag == b'E'),
        "unexpected ErrorResponse while rebinding portal after COMMIT"
    );
    assert_eq!(
        messages.iter().filter(|(tag, _)| *tag == b'2').count(),
        2,
        "expected both Bind messages to succeed"
    );
    let data_row = messages
        .iter()
        .rfind(|(tag, _)| *tag == b'D')
        .map(|(_, payload)| payload.as_slice())
        .expect("data row from second portal");
    assert_eq!(parse_data_row_columns(data_row), vec![Some(b"2".to_vec())]);
}

#[tokio::test]
async fn simple_notice_then_commit_clears_stale_pgwire_portal_slots() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_query_bytes("BEGIN"));
    input.extend(build_parse_bytes("s_old", "SELECT 1", &[]));
    input.extend(build_bind_bytes("p_old", "s_old", &[], &[], &[]));
    input.extend(build_sync_bytes());
    input.extend(build_query_bytes(
        "DO $$ BEGIN RAISE NOTICE '%', 'commit sync'; END $$ LANGUAGE plpgsql; COMMIT",
    ));
    input.extend(build_parse_bytes("s_new", "SELECT 2", &[]));
    input.extend(build_bind_bytes("p_new", "s_new", &[], &[], &[]));
    input.extend(build_execute_bytes("p_new", 0));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.set_max_portals(1);
    conn.run()
        .await
        .expect("notice + commit should free pgwire portal slot for rebinding");

    let messages = backend_messages(conn.writer_ref());
    assert!(
        !messages.iter().any(|(tag, _)| *tag == b'E'),
        "unexpected ErrorResponse while rebinding portal after notice + COMMIT"
    );
    assert!(
        messages.iter().any(|(tag, payload)| {
            *tag == b'N' && String::from_utf8_lossy(payload).contains("commit sync")
        }),
        "expected NoticeResponse from DO block"
    );
    assert_eq!(
        messages.iter().filter(|(tag, _)| *tag == b'2').count(),
        2,
        "expected both Bind messages to succeed"
    );
    let data_row = messages
        .iter()
        .rfind(|(tag, _)| *tag == b'D')
        .map(|(_, payload)| payload.as_slice())
        .expect("data row from second portal");
    assert_eq!(parse_data_row_columns(data_row), vec![Some(b"2".to_vec())]);
}

#[tokio::test]
async fn simple_rollback_to_savepoint_clears_stale_pgwire_portal_slots() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_query_bytes("BEGIN"));
    input.extend(build_query_bytes("SAVEPOINT sp1"));
    input.extend(build_parse_bytes("s_old", "SELECT 1", &[]));
    input.extend(build_bind_bytes("p_old", "s_old", &[], &[], &[]));
    input.extend(build_sync_bytes());
    input.extend(build_query_bytes("ROLLBACK TO SAVEPOINT sp1"));
    input.extend(build_parse_bytes("s_new", "SELECT 2", &[]));
    input.extend(build_bind_bytes("p_new", "s_new", &[], &[], &[]));
    input.extend(build_execute_bytes("p_new", 0));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.set_max_portals(1);
    conn.run()
        .await
        .expect("rollback to savepoint should free pgwire portal slot for rebinding");

    let messages = backend_messages(conn.writer_ref());
    assert!(
        !messages.iter().any(|(tag, _)| *tag == b'E'),
        "unexpected ErrorResponse while rebinding portal after ROLLBACK TO SAVEPOINT"
    );
    assert_eq!(
        messages.iter().filter(|(tag, _)| *tag == b'2').count(),
        2,
        "expected both Bind messages to succeed"
    );
    let data_row = messages
        .iter()
        .rfind(|(tag, _)| *tag == b'D')
        .map(|(_, payload)| payload.as_slice())
        .expect("data row from second portal");
    assert_eq!(parse_data_row_columns(data_row), vec![Some(b"2".to_vec())]);
}

#[tokio::test]
async fn simple_notice_then_rollback_to_savepoint_clears_stale_pgwire_portal_slots() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_query_bytes("BEGIN"));
    input.extend(build_query_bytes("SAVEPOINT sp1"));
    input.extend(build_parse_bytes("s_old", "SELECT 1", &[]));
    input.extend(build_bind_bytes("p_old", "s_old", &[], &[], &[]));
    input.extend(build_sync_bytes());
    input.extend(build_query_bytes(
        "DO $$ BEGIN RAISE NOTICE '%', 'rollback sync'; END $$ LANGUAGE plpgsql; ROLLBACK TO SAVEPOINT sp1",
    ));
    input.extend(build_parse_bytes("s_new", "SELECT 2", &[]));
    input.extend(build_bind_bytes("p_new", "s_new", &[], &[], &[]));
    input.extend(build_execute_bytes("p_new", 0));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.set_max_portals(1);
    conn.run()
        .await
        .expect("notice + rollback to savepoint should free pgwire portal slot for rebinding");

    let messages = backend_messages(conn.writer_ref());
    assert!(
        !messages.iter().any(|(tag, _)| *tag == b'E'),
        "unexpected ErrorResponse while rebinding portal after notice + ROLLBACK TO SAVEPOINT"
    );
    assert!(
        messages.iter().any(|(tag, payload)| {
            *tag == b'N' && String::from_utf8_lossy(payload).contains("rollback sync")
        }),
        "expected NoticeResponse from DO block"
    );
    assert_eq!(
        messages.iter().filter(|(tag, _)| *tag == b'2').count(),
        2,
        "expected both Bind messages to succeed"
    );
    let data_row = messages
        .iter()
        .rfind(|(tag, _)| *tag == b'D')
        .map(|(_, payload)| payload.as_slice())
        .expect("data row from second portal");
    assert_eq!(parse_data_row_columns(data_row), vec![Some(b"2".to_vec())]);
}

#[tokio::test]
async fn simple_deallocate_all_clears_stale_pgwire_portal_slots() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes("s_old", "SELECT 1", &[]));
    input.extend(build_bind_bytes("p_old", "s_old", &[], &[], &[]));
    input.extend(build_sync_bytes());
    input.extend(build_query_bytes("DEALLOCATE ALL"));
    input.extend(build_parse_bytes("s_new", "SELECT 2", &[]));
    input.extend(build_bind_bytes("p_new", "s_new", &[], &[], &[]));
    input.extend(build_execute_bytes("p_new", 0));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.set_max_portals(1);
    conn.run()
        .await
        .expect("DEALLOCATE ALL should free pgwire portal slot for rebinding");

    let messages = backend_messages(conn.writer_ref());
    assert!(
        !messages.iter().any(|(tag, _)| *tag == b'E'),
        "unexpected ErrorResponse while rebinding portal after DEALLOCATE ALL"
    );
    assert_eq!(
        messages.iter().filter(|(tag, _)| *tag == b'2').count(),
        2,
        "expected both Bind messages to succeed"
    );
    let data_row = messages
        .iter()
        .rfind(|(tag, _)| *tag == b'D')
        .map(|(_, payload)| payload.as_slice())
        .expect("data row from rebound portal");
    assert_eq!(parse_data_row_columns(data_row), vec![Some(b"2".to_vec())]);
}

#[tokio::test]
async fn simple_notice_then_deallocate_all_clears_stale_pgwire_portal_slots() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes("s_old", "SELECT 1", &[]));
    input.extend(build_bind_bytes("p_old", "s_old", &[], &[], &[]));
    input.extend(build_sync_bytes());
    input.extend(build_query_bytes(
        "DO $$ BEGIN RAISE NOTICE '%', 'deallocate sync'; END $$ LANGUAGE plpgsql; DEALLOCATE ALL",
    ));
    input.extend(build_parse_bytes("s_new", "SELECT 2", &[]));
    input.extend(build_bind_bytes("p_new", "s_new", &[], &[], &[]));
    input.extend(build_execute_bytes("p_new", 0));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.set_max_portals(1);
    conn.run()
        .await
        .expect("notice + DEALLOCATE ALL should free pgwire portal slot for rebinding");

    let messages = backend_messages(conn.writer_ref());
    assert!(
        !messages.iter().any(|(tag, _)| *tag == b'E'),
        "unexpected ErrorResponse while rebinding portal after notice + DEALLOCATE ALL"
    );
    assert!(
        messages.iter().any(|(tag, payload)| {
            *tag == b'N' && String::from_utf8_lossy(payload).contains("deallocate sync")
        }),
        "expected NoticeResponse from DO block"
    );
    assert_eq!(
        messages.iter().filter(|(tag, _)| *tag == b'2').count(),
        2,
        "expected both Bind messages to succeed"
    );
    let data_row = messages
        .iter()
        .rfind(|(tag, _)| *tag == b'D')
        .map(|(_, payload)| payload.as_slice())
        .expect("data row from rebound portal");
    assert_eq!(parse_data_row_columns(data_row), vec![Some(b"2".to_vec())]);
}

#[tokio::test]
async fn extended_deallocate_all_keeps_named_portal_bound_to_unnamed_statement() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes("", "SELECT 1", &[]));
    input.extend(build_bind_bytes("p_old", "", &[], &[], &[]));
    input.extend(build_sync_bytes());
    input.extend(build_parse_bytes("s_deallocate_all", "DEALLOCATE ALL", &[]));
    input.extend(build_bind_bytes(
        "p_deallocate_all",
        "s_deallocate_all",
        &[],
        &[],
        &[],
    ));
    input.extend(build_execute_bytes("p_deallocate_all", 0));
    input.extend(build_sync_bytes());
    input.extend(build_parse_bytes("s_new", "SELECT 2", &[]));
    input.extend(build_bind_bytes("p_new", "s_new", &[], &[], &[]));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.set_max_portals(1);
    conn.run()
        .await
        .expect("extended DEALLOCATE ALL with unnamed statement portal should not crash");

    let messages = backend_messages(conn.writer_ref());
    let error_message = messages
        .iter()
        .rfind(|(tag, _)| *tag == b'E')
        .and_then(|(_, payload)| parse_error_response_message(payload))
        .expect("expected portal limit error after preserved unnamed portal");
    assert!(
        error_message.contains("maximum number of portals reached"),
        "expected max portals error, got {error_message:?}"
    );
}

#[tokio::test]
async fn simple_close_clears_stale_pgwire_portal_slots() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes("s_old", "SELECT 2", &[]));
    input.extend(build_bind_bytes("p_old", "s_old", &[], &[], &[]));
    input.extend(build_sync_bytes());
    input.extend(build_query_bytes("CLOSE p_old"));
    input.extend(build_bind_bytes("p_new", "s_old", &[], &[], &[]));
    input.extend(build_execute_bytes("p_new", 0));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.set_max_portals(1);
    conn.run()
        .await
        .expect("CLOSE should free pgwire portal slot for rebinding");

    let messages = backend_messages(conn.writer_ref());
    assert!(
        !messages.iter().any(|(tag, _)| *tag == b'E'),
        "unexpected ErrorResponse while rebinding portal after CLOSE"
    );
    assert_eq!(
        messages.iter().filter(|(tag, _)| *tag == b'2').count(),
        2,
        "expected both Bind messages to succeed"
    );
    let data_row = messages
        .iter()
        .rfind(|(tag, _)| *tag == b'D')
        .map(|(_, payload)| payload.as_slice())
        .expect("data row from rebound portal");
    assert_eq!(parse_data_row_columns(data_row), vec![Some(b"2".to_vec())]);
}

#[tokio::test]
async fn simple_close_with_trailing_garbage_does_not_close_existing_portal() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes("s_old", "SELECT 2", &[]));
    input.extend(build_bind_bytes("p_old", "s_old", &[], &[], &[]));
    input.extend(build_sync_bytes());
    input.extend(build_query_bytes("CLOSE p_old trailing"));
    input.extend(build_execute_bytes("p_old", 0));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("malformed CLOSE should not kill existing portal");

    let messages = backend_messages(conn.writer_ref());
    let error_message = messages
        .iter()
        .find(|(tag, _)| *tag == b'E')
        .and_then(|(_, payload)| parse_error_response_message(payload))
        .expect("expected ErrorResponse for malformed CLOSE");
    assert!(
        error_message.contains("unsupported compatibility command: CLOSE"),
        "expected malformed CLOSE error, got {error_message:?}"
    );
    let data_row = messages
        .iter()
        .rfind(|(tag, _)| *tag == b'D')
        .map(|(_, payload)| payload.as_slice())
        .expect("data row from preserved portal");
    assert_eq!(parse_data_row_columns(data_row), vec![Some(b"2".to_vec())]);
}

#[tokio::test]
async fn simple_execute_sql_deallocate_all_clears_stale_pgwire_portal_slots() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes("s_old", "SELECT 1", &[]));
    input.extend(build_bind_bytes("p_old", "s_old", &[], &[], &[]));
    input.extend(build_sync_bytes());
    input.extend(build_query_bytes(
        "PREPARE dealloc_all_stmt AS DEALLOCATE ALL",
    ));
    input.extend(build_query_bytes("EXECUTE dealloc_all_stmt"));
    input.extend(build_parse_bytes("s_new", "SELECT 2", &[]));
    input.extend(build_bind_bytes("p_new", "s_new", &[], &[], &[]));
    input.extend(build_execute_bytes("p_new", 0));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.set_max_portals(1);
    conn.run()
        .await
        .expect("simple EXECUTE dealloc_all_stmt should free pgwire portal slot for rebinding");

    let messages = backend_messages(conn.writer_ref());
    let error_messages: Vec<String> = messages
        .iter()
        .filter(|(tag, _)| *tag == b'E')
        .filter_map(|(_, payload)| parse_error_response_message(payload))
        .collect();
    assert!(
        error_messages.is_empty(),
        "unexpected ErrorResponse while rebinding portal after simple EXECUTE dealloc_all_stmt: {error_messages:?}"
    );
    assert_eq!(
        messages.iter().filter(|(tag, _)| *tag == b'2').count(),
        2,
        "expected both Bind messages to succeed"
    );
    let data_row = messages
        .iter()
        .rfind(|(tag, _)| *tag == b'D')
        .map(|(_, payload)| payload.as_slice())
        .expect("data row from rebound portal");
    assert_eq!(parse_data_row_columns(data_row), vec![Some(b"2".to_vec())]);
}

#[tokio::test]
async fn simple_execute_sql_rollback_to_savepoint_clears_stale_pgwire_portal_slots() {
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
    input.extend(build_query_bytes("EXECUTE rb_stmt"));
    input.extend(build_parse_bytes("s_new", "SELECT 2", &[]));
    input.extend(build_bind_bytes("p_new", "s_new", &[], &[], &[]));
    input.extend(build_execute_bytes("p_new", 0));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.set_max_portals(1);
    conn.run()
        .await
        .expect("simple EXECUTE rb_stmt should free post-savepoint pgwire portal slot");

    let messages = backend_messages(conn.writer_ref());
    assert!(
        !messages.iter().any(|(tag, _)| *tag == b'E'),
        "unexpected ErrorResponse while rebinding portal after simple EXECUTE rb_stmt"
    );
    assert_eq!(
        messages.iter().filter(|(tag, _)| *tag == b'2').count(),
        2,
        "expected both Bind messages to succeed"
    );
    let data_row = messages
        .iter()
        .rfind(|(tag, _)| *tag == b'D')
        .map(|(_, payload)| payload.as_slice())
        .expect("data row from rebound portal");
    assert_eq!(parse_data_row_columns(data_row), vec![Some(b"2".to_vec())]);
}

#[tokio::test]
async fn simple_failed_transaction_allows_rollback_to_savepoint_recovery() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_query_bytes(
        "CREATE TABLE tx_recover_simple (id INT NOT NULL)",
    ));
    input.extend(build_query_bytes("BEGIN"));
    input.extend(build_query_bytes("SAVEPOINT sp1"));
    input.extend(build_query_bytes(
        "INSERT INTO tx_recover_simple VALUES (NULL)",
    ));
    input.extend(build_query_bytes("ROLLBACK TO SAVEPOINT sp1"));
    input.extend(build_query_bytes("SELECT 1"));
    input.extend(build_query_bytes("COMMIT"));
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("rollback to savepoint should recover failed transaction");

    let messages = backend_messages(conn.writer_ref());
    let error_messages: Vec<String> = messages
        .iter()
        .filter(|(tag, _)| *tag == b'E')
        .filter_map(|(_, payload)| parse_error_response_message(payload))
        .collect();
    assert!(
        !error_messages
            .iter()
            .any(|message| message.contains("current transaction is aborted")),
        "unexpected failed-transaction error after recovery: {error_messages:?}"
    );
    let data_row = messages
        .iter()
        .rfind(|(tag, _)| *tag == b'D')
        .map(|(_, payload)| payload.as_slice())
        .expect("data row after recovery");
    assert_eq!(parse_data_row_columns(data_row), vec![Some(b"1".to_vec())]);
}

#[tokio::test]
async fn simple_failed_transaction_allows_commit_then_followup_statement_in_same_batch() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_query_bytes(
        "CREATE TABLE tx_commit_recover_simple (id INT NOT NULL)",
    ));
    input.extend(build_query_bytes("BEGIN"));
    input.extend(build_query_bytes(
        "INSERT INTO tx_commit_recover_simple VALUES (NULL)",
    ));
    input.extend(build_query_bytes("COMMIT; SELECT 1"));
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("COMMIT should terminate failed transaction and allow follow-up query");

    let messages = backend_messages(conn.writer_ref());
    let error_messages: Vec<String> = messages
        .iter()
        .filter(|(tag, _)| *tag == b'E')
        .filter_map(|(_, payload)| parse_error_response_message(payload))
        .collect();
    assert!(
        !error_messages
            .iter()
            .any(|message| message.contains("current transaction is aborted")),
        "unexpected failed-transaction error after COMMIT recovery: {error_messages:?}"
    );
    let commands: Vec<String> = messages
        .iter()
        .filter(|(tag, _)| *tag == b'C')
        .map(|(_, payload)| parse_cstring_payload(payload).to_owned())
        .collect();
    assert!(
        commands.iter().any(|command| command == "ROLLBACK"),
        "expected ROLLBACK command complete after COMMIT in failed transaction, got {commands:?}"
    );
    let data_row = messages
        .iter()
        .rfind(|(tag, _)| *tag == b'D')
        .map(|(_, payload)| payload.as_slice())
        .expect("data row after COMMIT recovery");
    assert_eq!(parse_data_row_columns(data_row), vec![Some(b"1".to_vec())]);
}

#[tokio::test]
async fn simple_failed_transaction_allows_compat_execute_commit_recovery() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_query_bytes(
        "CREATE TABLE tx_exec_commit_recover_simple (id INT NOT NULL)",
    ));
    input.extend(build_query_bytes("BEGIN"));
    input.extend(build_query_bytes("PREPARE commit_stmt AS COMMIT"));
    input.extend(build_query_bytes(
        "INSERT INTO tx_exec_commit_recover_simple VALUES (NULL)",
    ));
    input.extend(build_query_bytes("EXECUTE commit_stmt"));
    input.extend(build_query_bytes("SELECT 1"));
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("compat EXECUTE commit should terminate failed transaction");

    let messages = backend_messages(conn.writer_ref());
    let error_messages: Vec<String> = messages
        .iter()
        .filter(|(tag, _)| *tag == b'E')
        .filter_map(|(_, payload)| parse_error_response_message(payload))
        .collect();
    assert!(
        !error_messages
            .iter()
            .any(|message| message.contains("current transaction is aborted")),
        "unexpected failed-transaction error after compat EXECUTE COMMIT recovery: {error_messages:?}"
    );
    let commands: Vec<String> = messages
        .iter()
        .filter(|(tag, _)| *tag == b'C')
        .map(|(_, payload)| parse_cstring_payload(payload).to_owned())
        .collect();
    assert!(
        commands.iter().any(|command| command == "ROLLBACK"),
        "expected ROLLBACK command complete after compat EXECUTE COMMIT in failed transaction, got {commands:?}"
    );
    let data_row = messages
        .iter()
        .rfind(|(tag, _)| *tag == b'D')
        .map(|(_, payload)| payload.as_slice())
        .expect("data row after compat EXECUTE COMMIT recovery");
    assert_eq!(parse_data_row_columns(data_row), vec![Some(b"1".to_vec())]);
}

#[tokio::test]
async fn simple_failed_transaction_still_rejects_non_recovery_compat_execute() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_query_bytes(
        "CREATE TABLE tx_exec_non_recovery_simple (id INT NOT NULL)",
    ));
    input.extend(build_query_bytes("PREPARE sel_stmt AS SELECT 1"));
    input.extend(build_query_bytes("BEGIN"));
    input.extend(build_query_bytes(
        "INSERT INTO tx_exec_non_recovery_simple VALUES (NULL)",
    ));
    input.extend(build_query_bytes("EXECUTE sel_stmt"));
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("non-recovery compat EXECUTE should return a normal error response");

    let messages = backend_messages(conn.writer_ref());
    let error_messages: Vec<String> = messages
        .iter()
        .filter(|(tag, _)| *tag == b'E')
        .filter_map(|(_, payload)| parse_error_response_message(payload))
        .collect();
    assert!(
        error_messages
            .iter()
            .any(|message| message.contains("current transaction is aborted")),
        "expected failed-transaction error for non-recovery compat EXECUTE, got {error_messages:?}"
    );
}

#[tokio::test]
async fn simple_copy_from_error_enters_failed_transaction_until_rollback() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_query_bytes(
        "CREATE TABLE tx_copy_failed_pgwire (id INT NOT NULL)",
    ));
    input.extend(build_query_bytes("BEGIN"));
    input.extend(build_query_bytes("COPY tx_copy_failed_pgwire FROM STDIN"));
    input.extend(build_copy_data_bytes(b"1\textra\n"));
    input.extend(build_copy_done_bytes());
    input.extend(build_query_bytes("SELECT 1"));
    input.extend(build_query_bytes("ROLLBACK"));
    input.extend(build_query_bytes("SELECT 2"));
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("COPY FROM error should leave connection usable after rollback");

    let messages = backend_messages(conn.writer_ref());
    let error_messages: Vec<String> = messages
        .iter()
        .filter(|(tag, _)| *tag == b'E')
        .filter_map(|(_, payload)| parse_error_response_message(payload))
        .collect();
    assert!(
        error_messages
            .iter()
            .any(|message| message.contains("expected 1 columns, got 2")),
        "expected COPY FROM shape error, got {error_messages:?}"
    );
    assert!(
        error_messages
            .iter()
            .any(|message| message.contains("current transaction is aborted")),
        "expected failed-transaction error after COPY FROM failure, got {error_messages:?}"
    );
    let commands: Vec<String> = messages
        .iter()
        .filter(|(tag, _)| *tag == b'C')
        .map(|(_, payload)| parse_cstring_payload(payload).to_owned())
        .collect();
    assert!(
        commands.iter().any(|command| command == "ROLLBACK"),
        "expected rollback command complete after COPY FROM failure, got {commands:?}"
    );
    let data_row = messages
        .iter()
        .rfind(|(tag, _)| *tag == b'D')
        .map(|(_, payload)| payload.as_slice())
        .expect("data row after rollback recovery");
    assert_eq!(parse_data_row_columns(data_row), vec![Some(b"2".to_vec())]);
}

#[tokio::test]
async fn extended_failed_transaction_allows_prepared_rollback_to_savepoint_recovery() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_query_bytes(
        "CREATE TABLE tx_recover_extended (id INT NOT NULL)",
    ));
    input.extend(build_query_bytes("BEGIN"));
    input.extend(build_query_bytes("SAVEPOINT sp1"));
    input.extend(build_query_bytes(
        "INSERT INTO tx_recover_extended VALUES (NULL)",
    ));
    input.extend(build_parse_bytes(
        "s_recover_sp",
        "ROLLBACK TO SAVEPOINT sp1",
        &[],
    ));
    input.extend(build_bind_bytes(
        "p_recover_sp",
        "s_recover_sp",
        &[],
        &[],
        &[],
    ));
    input.extend(build_execute_bytes("p_recover_sp", 0));
    input.extend(build_sync_bytes());
    input.extend(build_parse_bytes("s_after_recover", "SELECT 1", &[]));
    input.extend(build_bind_bytes(
        "p_after_recover",
        "s_after_recover",
        &[],
        &[],
        &[],
    ));
    input.extend(build_execute_bytes("p_after_recover", 0));
    input.extend(build_sync_bytes());
    input.extend(build_query_bytes("COMMIT"));
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("prepared rollback to savepoint should recover failed transaction");

    let messages = backend_messages(conn.writer_ref());
    let error_messages: Vec<String> = messages
        .iter()
        .filter(|(tag, _)| *tag == b'E')
        .filter_map(|(_, payload)| parse_error_response_message(payload))
        .collect();
    assert!(
        !error_messages
            .iter()
            .any(|message| message.contains("current transaction is aborted")),
        "unexpected failed-transaction error after prepared recovery: {error_messages:?}"
    );
    let data_row = messages
        .iter()
        .rfind(|(tag, _)| *tag == b'D')
        .map(|(_, payload)| payload.as_slice())
        .expect("data row after prepared recovery");
    assert_eq!(parse_data_row_columns(data_row), vec![Some(b"1".to_vec())]);
}

#[tokio::test]
async fn extended_failed_transaction_allows_prepared_commit_recovery() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_query_bytes(
        "CREATE TABLE tx_commit_recover_extended (id INT NOT NULL)",
    ));
    input.extend(build_query_bytes("BEGIN"));
    input.extend(build_query_bytes(
        "INSERT INTO tx_commit_recover_extended VALUES (NULL)",
    ));
    input.extend(build_parse_bytes("s_commit_recover", "COMMIT", &[]));
    input.extend(build_bind_bytes(
        "p_commit_recover",
        "s_commit_recover",
        &[],
        &[],
        &[],
    ));
    input.extend(build_execute_bytes("p_commit_recover", 0));
    input.extend(build_sync_bytes());
    input.extend(build_parse_bytes("s_after_commit_recover", "SELECT 1", &[]));
    input.extend(build_bind_bytes(
        "p_after_commit_recover",
        "s_after_commit_recover",
        &[],
        &[],
        &[],
    ));
    input.extend(build_execute_bytes("p_after_commit_recover", 0));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("prepared COMMIT should terminate failed transaction");

    let messages = backend_messages(conn.writer_ref());
    let error_messages: Vec<String> = messages
        .iter()
        .filter(|(tag, _)| *tag == b'E')
        .filter_map(|(_, payload)| parse_error_response_message(payload))
        .collect();
    assert!(
        !error_messages
            .iter()
            .any(|message| message.contains("current transaction is aborted")),
        "unexpected failed-transaction error after prepared COMMIT recovery: {error_messages:?}"
    );
    let commands: Vec<String> = messages
        .iter()
        .filter(|(tag, _)| *tag == b'C')
        .map(|(_, payload)| parse_cstring_payload(payload).to_owned())
        .collect();
    assert!(
        commands.iter().any(|command| command == "ROLLBACK"),
        "expected ROLLBACK command complete after prepared COMMIT in failed transaction, got {commands:?}"
    );
    let data_row = messages
        .iter()
        .rfind(|(tag, _)| *tag == b'D')
        .map(|(_, payload)| payload.as_slice())
        .expect("data row after prepared COMMIT recovery");
    assert_eq!(parse_data_row_columns(data_row), vec![Some(b"1".to_vec())]);
}

#[tokio::test]
async fn extended_failed_transaction_allows_prepared_sql_execute_commit_recovery() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_query_bytes(
        "CREATE TABLE tx_exec_commit_recover_extended (id INT NOT NULL)",
    ));
    input.extend(build_query_bytes("BEGIN"));
    input.extend(build_query_bytes("PREPARE commit_stmt AS COMMIT"));
    input.extend(build_query_bytes(
        "INSERT INTO tx_exec_commit_recover_extended VALUES (NULL)",
    ));
    input.extend(build_parse_bytes(
        "s_exec_commit",
        "EXECUTE commit_stmt",
        &[],
    ));
    input.extend(build_bind_bytes(
        "p_exec_commit",
        "s_exec_commit",
        &[],
        &[],
        &[],
    ));
    input.extend(build_execute_bytes("p_exec_commit", 0));
    input.extend(build_sync_bytes());
    input.extend(build_parse_bytes("s_after_exec_commit", "SELECT 1", &[]));
    input.extend(build_bind_bytes(
        "p_after_exec_commit",
        "s_after_exec_commit",
        &[],
        &[],
        &[],
    ));
    input.extend(build_execute_bytes("p_after_exec_commit", 0));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("prepared SQL EXECUTE commit should terminate failed transaction");

    let messages = backend_messages(conn.writer_ref());
    let error_messages: Vec<String> = messages
        .iter()
        .filter(|(tag, _)| *tag == b'E')
        .filter_map(|(_, payload)| parse_error_response_message(payload))
        .collect();
    assert!(
        !error_messages
            .iter()
            .any(|message| message.contains("current transaction is aborted")),
        "unexpected failed-transaction error after prepared SQL EXECUTE COMMIT recovery: {error_messages:?}"
    );
    let commands: Vec<String> = messages
        .iter()
        .filter(|(tag, _)| *tag == b'C')
        .map(|(_, payload)| parse_cstring_payload(payload).to_owned())
        .collect();
    assert!(
        commands.iter().any(|command| command == "ROLLBACK"),
        "expected ROLLBACK command complete after prepared SQL EXECUTE COMMIT in failed transaction, got {commands:?}"
    );
    let data_row = messages
        .iter()
        .rfind(|(tag, _)| *tag == b'D')
        .map(|(_, payload)| payload.as_slice())
        .expect("data row after prepared SQL EXECUTE COMMIT recovery");
    assert_eq!(parse_data_row_columns(data_row), vec![Some(b"1".to_vec())]);
}

#[tokio::test]
async fn extended_failed_transaction_allows_describe_for_prepared_rollback_to_savepoint() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_query_bytes(
        "CREATE TABLE tx_recover_describe (id INT NOT NULL)",
    ));
    input.extend(build_query_bytes("BEGIN"));
    input.extend(build_query_bytes("SAVEPOINT sp1"));
    input.extend(build_query_bytes(
        "INSERT INTO tx_recover_describe VALUES (NULL)",
    ));
    input.extend(build_parse_bytes(
        "s_recover_desc",
        "ROLLBACK TO SAVEPOINT sp1",
        &[],
    ));
    input.extend(build_describe_bytes(b'S', "s_recover_desc"));
    input.extend(build_bind_bytes(
        "p_recover_desc",
        "s_recover_desc",
        &[],
        &[],
        &[],
    ));
    input.extend(build_describe_bytes(b'P', "p_recover_desc"));
    input.extend(build_execute_bytes("p_recover_desc", 0));
    input.extend(build_sync_bytes());
    input.extend(build_parse_bytes("s_after_desc_recover", "SELECT 1", &[]));
    input.extend(build_bind_bytes(
        "p_after_desc_recover",
        "s_after_desc_recover",
        &[],
        &[],
        &[],
    ));
    input.extend(build_execute_bytes("p_after_desc_recover", 0));
    input.extend(build_sync_bytes());
    input.extend(build_query_bytes("COMMIT"));
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("describe should be allowed for prepared rollback-to-savepoint recovery");

    let messages = backend_messages(conn.writer_ref());
    let error_messages: Vec<String> = messages
        .iter()
        .filter(|(tag, _)| *tag == b'E')
        .filter_map(|(_, payload)| parse_error_response_message(payload))
        .collect();
    assert!(
        !error_messages
            .iter()
            .any(|message| message.contains("current transaction is aborted")),
        "unexpected failed-transaction error during describe-driven recovery: {error_messages:?}"
    );
    let data_row = messages
        .iter()
        .rfind(|(tag, _)| *tag == b'D')
        .map(|(_, payload)| payload.as_slice())
        .expect("data row after describe-driven recovery");
    assert_eq!(parse_data_row_columns(data_row), vec![Some(b"1".to_vec())]);
}

#[tokio::test]
async fn copy_done_outside_copy_subprotocol_returns_error_response() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_copy_done_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("stray CopyDone should return protocol error, not abort connection");

    let messages = backend_messages(conn.writer_ref());
    let tags: Vec<u8> = messages.iter().map(|(tag, _)| *tag).collect();
    assert!(tags.contains(&b'E'), "expected ErrorResponse");
    assert!(tags.contains(&b'Z'), "expected ReadyForQuery");
    let error_payload = messages
        .iter()
        .find(|(tag, _)| *tag == b'E')
        .map(|(_, payload)| String::from_utf8_lossy(payload).into_owned())
        .expect("error response payload");
    assert!(error_payload.contains("unexpected COPY message outside COPY sub-protocol"));
}
