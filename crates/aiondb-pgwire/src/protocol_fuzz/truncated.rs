use super::*;

// =========================================================================
// 3. Truncated messages
// =========================================================================

#[tokio::test]
async fn truncated_query_message() {
    let mut input = build_startup_bytes();
    // Build a Query message that claims to have 20 bytes of payload but only
    // provides 5.
    input.push(b'Q');
    input.extend_from_slice(&24u32.to_be_bytes()); // 4 + 20 payload
    input.extend_from_slice(b"SELEC"); // only 5 bytes
    let result = run_with_bytes(input).await;
    // The connection loop catches the read error (unexpected EOF) and breaks.
    assert!(
        result.is_ok(),
        "truncated query should be handled gracefully"
    );
}

#[tokio::test]
async fn parse_message_missing_fields() {
    let mut input = build_startup_bytes();
    // A Parse message with only the statement name, missing query and param count.
    input.extend(build_raw_message(b'P', b"stmt\0"));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());
    // The server should send an error but not panic.
    let result = run_with_bytes(input).await;
    assert!(
        result.is_ok(),
        "parse with missing fields handled gracefully"
    );
}

#[tokio::test]
async fn bind_message_wrong_parameter_count() {
    let mut input = build_startup_bytes();
    // First send a Parse to create a prepared statement.
    input.extend(build_parse_bytes("stmt1", "SELECT 1", &[]));
    input.extend(build_sync_bytes());
    // Now send a Bind that claims 5 parameters but provides none.
    let mut payload = Vec::new();
    payload.extend_from_slice(b"\0"); // portal (unnamed)
    payload.extend_from_slice(b"stmt1\0"); // statement
    payload.extend_from_slice(&0i16.to_be_bytes()); // 0 param formats
    payload.extend_from_slice(&5i16.to_be_bytes()); // claim 5 param values
                                                    // ... but provide none -- truncated
    input.extend(build_raw_message(b'B', &payload));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());
    let result = run_with_bytes(input).await;
    // Should handle the truncated bind without panicking.
    assert!(
        result.is_ok(),
        "bind with wrong param count handled gracefully"
    );
}

#[tokio::test]
async fn truncated_describe_message() {
    let mut input = build_startup_bytes();
    // Describe with only the target byte 'S' but no name (no null terminator).
    input.extend(build_raw_message(b'D', b"S"));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());
    let result = run_with_bytes(input).await;
    assert!(result.is_ok(), "truncated describe handled gracefully");
}

#[tokio::test]
async fn truncated_execute_message() {
    let mut input = build_startup_bytes();
    // Execute with only portal name, missing max_rows.
    input.extend(build_raw_message(b'E', b"\0"));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());
    let result = run_with_bytes(input).await;
    assert!(result.is_ok(), "truncated execute handled gracefully");
}
