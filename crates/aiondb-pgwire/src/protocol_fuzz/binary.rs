use super::*;

// =========================================================================
// 7. Binary injection
// =========================================================================

#[tokio::test]
async fn null_bytes_in_query_string() {
    let mut input = build_startup_bytes();
    // A query with embedded null bytes.  The PG wire protocol uses
    // null-terminated strings, so the parser sees only "SELECT" before \0.
    let sql_with_nulls = b"SELECT\0; DROP TABLE users;\0";
    let mut msg = Vec::new();
    msg.push(b'Q');
    let msg_len = (sql_with_nulls.len() as u32) + 4;
    msg.extend_from_slice(&msg_len.to_be_bytes());
    msg.extend_from_slice(sql_with_nulls);
    input.extend(msg);
    input.extend(build_terminate_bytes());
    let result = run_with_bytes(input).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn non_utf8_bytes_in_identifier() {
    let mut input = build_startup_bytes();
    // Parse message with non-UTF8 statement name.
    let mut payload = Vec::new();
    payload.extend_from_slice(&[0xFF, 0xFE, 0x00]); // invalid UTF-8 + null
    payload.extend_from_slice(b"SELECT 1\0");
    payload.extend_from_slice(&0i16.to_be_bytes());
    input.extend(build_raw_message(b'P', &payload));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());
    let result = run_with_bytes(input).await;
    // Should handle UTF-8 error gracefully.
    assert!(result.is_ok());
}

#[tokio::test]
async fn control_characters_in_parameter_values() {
    let mut input = build_startup_bytes();
    // Startup with control characters in parameter values is already handled
    // during startup, so test during message loop.
    input.extend(build_parse_bytes("stmt1", "SELECT 1", &[]));
    input.extend(build_sync_bytes());
    // Bind with a parameter value containing control characters.
    let ctrl_value: &[u8] = &[0x01, 0x02, 0x03, 0x7F, 0x1B]; // SOH, STX, ETX, DEL, ESC
    input.extend(build_bind_bytes("", "stmt1", &[], &[Some(ctrl_value)], &[]));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());
    let result = run_with_bytes(input).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn non_utf8_in_query_payload() {
    let mut input = build_startup_bytes();
    // Build a Query message where the SQL bytes are not valid UTF-8.
    let bad_bytes: &[u8] = &[0xFF, 0xFE, 0xFD, 0x00]; // invalid UTF-8 + null
    input.extend(build_raw_message(b'Q', bad_bytes));
    input.extend(build_terminate_bytes());
    let result = run_with_bytes(input).await;
    // Should produce an error response, not a panic.
    assert!(result.is_ok());
}

#[tokio::test]
async fn non_utf8_in_startup_user() {
    // Startup message with non-UTF8 bytes in the user field.
    let mut payload = Vec::new();
    payload.extend_from_slice(&codec::PROTOCOL_V3.to_be_bytes());
    payload.extend_from_slice(b"user\0");
    payload.extend_from_slice(&[0xFF, 0xFE, 0x00]); // invalid UTF-8 value + null
    payload.push(0); // trailing null
    let data = build_startup_raw(&payload);
    let result = run_with_bytes(data).await;
    // Should error on invalid UTF-8 during startup parsing.
    assert!(result.is_err());
}

#[tokio::test]
async fn all_zero_bytes_as_startup() {
    let input = vec![0u8; 64];
    let result = run_with_bytes(input).await;
    // Length field is 0 -- too short.
    assert!(result.is_err());
}

#[tokio::test]
async fn all_0xff_bytes_as_startup() {
    let input = vec![0xFFu8; 64];
    // Length field would be 0xFFFFFFFF (4 GB) -- will EOF before reading.
    let result = run_with_bytes(input).await;
    assert!(result.is_err());
}
