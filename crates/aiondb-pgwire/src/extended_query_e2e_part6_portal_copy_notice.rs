use super::*;

#[tokio::test]
async fn extended_execute_portal_writes_notice_response_for_notice_batches() {
    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes("s_notice", "SELECT 1", &[]));
    input.extend(build_bind_bytes("p_notice", "s_notice", &[], &[], &[]));
    input.extend(build_execute_bytes("p_notice", 0));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let engine = Arc::new(NoticePortalMockEngine);
    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("extended notice portal should succeed");

    let messages = backend_messages(conn.writer_ref());
    let tags: Vec<u8> = messages.iter().map(|(tag, _)| *tag).collect();
    assert!(tags.contains(&b'N'), "expected a NoticeResponse");
    assert!(
        tags.contains(&b'C'),
        "notice-only portal batches should terminate"
    );
    let notice_payload = messages
        .iter()
        .find(|(tag, _)| *tag == b'N')
        .map(|(_, payload)| payload.as_slice())
        .expect("notice response");
    assert!(String::from_utf8_lossy(notice_payload).contains("compatibility notice"));
    let command_payload = messages
        .iter()
        .rfind(|(tag, _)| *tag == b'C')
        .map(|(_, payload)| payload.as_slice())
        .expect("command complete");
    assert_eq!(parse_cstring_payload(command_payload), "SELECT 0");
}

#[tokio::test]
async fn extended_execute_portal_reports_select_zero_for_empty_result_sets() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes("s_empty", "SELECT 1 WHERE FALSE", &[]));
    input.extend(build_bind_bytes("p_empty", "s_empty", &[], &[], &[]));
    input.extend(build_execute_bytes("p_empty", 0));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("extended empty portal query should succeed");

    let messages = backend_messages(conn.writer_ref());
    assert!(
        !messages.iter().any(|(tag, _)| *tag == b'T'),
        "extended Execute should not emit RowDescription for empty result sets"
    );
    assert!(
        !messages.iter().any(|(tag, _)| *tag == b'D'),
        "empty result set should not emit DataRow"
    );
    let last_command = messages
        .iter()
        .rfind(|(tag, _)| *tag == b'C')
        .map(|(_, payload)| parse_cstring_payload(payload))
        .expect("command complete");
    assert_eq!(last_command, "SELECT 0");
}

#[tokio::test]
async fn extended_execute_portal_enters_copy_in_subprotocol_for_copy_batches() {
    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes("s_copy_in", "COPY t FROM STDIN", &[]));
    input.extend(build_bind_bytes("p_copy_in", "s_copy_in", &[], &[], &[]));
    input.extend(build_execute_bytes("p_copy_in", 0));
    input.extend(build_copy_data_bytes(b"1\talice\n"));
    input.extend(build_copy_done_bytes());
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let engine = Arc::new(CopyInPortalMockEngine);
    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("extended copy in portal should succeed");

    let messages = backend_messages(conn.writer_ref());
    let tags: Vec<u8> = messages.iter().map(|(tag, _)| *tag).collect();
    assert!(tags.contains(&b'G'), "expected CopyInResponse");
    let last_command = messages
        .iter()
        .rfind(|(tag, _)| *tag == b'C')
        .map(|(_, payload)| parse_cstring_payload(payload))
        .expect("command complete");
    assert_eq!(last_command, "COPY 1");
}

#[tokio::test]
async fn extended_execute_portal_copy_in_notice_still_emits_command_complete() {
    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_copy_in_notice",
        "COPY t FROM STDIN",
        &[],
    ));
    input.extend(build_bind_bytes(
        "p_copy_in_notice",
        "s_copy_in_notice",
        &[],
        &[],
        &[],
    ));
    input.extend(build_execute_bytes("p_copy_in_notice", 0));
    input.extend(build_copy_data_bytes(b"1\talice\n"));
    input.extend(build_copy_done_bytes());
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let engine = Arc::new(CopyInNoticePortalMockEngine);
    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("extended copy in notice portal should succeed");

    let messages = backend_messages(conn.writer_ref());
    let tags: Vec<u8> = messages.iter().map(|(tag, _)| *tag).collect();
    assert!(tags.contains(&b'G'), "expected CopyInResponse");
    assert!(tags.contains(&b'N'), "expected NoticeResponse");
    let last_command = messages
        .iter()
        .rfind(|(tag, _)| *tag == b'C')
        .map(|(_, payload)| parse_cstring_payload(payload))
        .expect("command complete");
    assert_eq!(last_command, "COPY 0");
}

#[tokio::test]
async fn extended_execute_portal_drains_pending_notices_after_copy_in_data() {
    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_copy_in_pending_notice",
        "COPY t FROM STDIN",
        &[],
    ));
    input.extend(build_bind_bytes(
        "p_copy_in_pending_notice",
        "s_copy_in_pending_notice",
        &[],
        &[],
        &[],
    ));
    input.extend(build_execute_bytes("p_copy_in_pending_notice", 0));
    input.extend(build_copy_data_bytes(b"1\talice\n"));
    input.extend(build_copy_done_bytes());
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let engine = Arc::new(CopyInPendingNoticePortalMockEngine::new());
    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("extended copy in pending notice portal should succeed");

    let messages = backend_messages(conn.writer_ref());
    let tags: Vec<u8> = messages.iter().map(|(tag, _)| *tag).collect();
    assert!(tags.contains(&b'G'), "expected CopyInResponse");
    assert!(tags.contains(&b'N'), "expected NoticeResponse");
    let notice_payload = messages
        .iter()
        .find(|(tag, _)| *tag == b'N')
        .map(|(_, payload)| payload.as_slice())
        .expect("notice response");
    assert!(
        String::from_utf8_lossy(notice_payload).contains("copy pending notice"),
        "expected pending notice payload"
    );
    let last_command = messages
        .iter()
        .rfind(|(tag, _)| *tag == b'C')
        .map(|(_, payload)| parse_cstring_payload(payload))
        .expect("command complete");
    assert_eq!(last_command, "COPY 1");
}
