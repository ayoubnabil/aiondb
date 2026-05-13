use super::*;

// =========================================================================
// Additional edge cases
// =========================================================================

#[tokio::test]
async fn ssl_request_then_normal_startup_then_terminate() {
    // First send an SSL request, then a normal startup, then terminate.
    let mut input = Vec::new();
    // SSL request: length 8, version = SSL_REQUEST magic.
    input.extend_from_slice(&8u32.to_be_bytes());
    input.extend_from_slice(&codec::SSL_REQUEST.to_be_bytes());
    // After SSL decline ('N'), client retries with normal startup.
    input.extend(build_startup_bytes());
    input.extend(build_terminate_bytes());
    let result = run_with_bytes(input).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn double_startup() {
    // Send two startup messages back-to-back.  The first completes startup;
    // the second will be interpreted as a regular message (wrong format).
    let mut input = build_startup_bytes();
    // The second "startup" is read as a tagged message. The first 4 bytes of
    // the startup length look like tag + partial length, causing an error.
    input.extend(build_startup_bytes());
    let result = run_with_bytes(input).await;
    // Either completes or errors -- no panic.
    let _ = result;
}

#[tokio::test]
async fn cancel_request_during_startup() {
    let mut data = Vec::new();
    data.extend_from_slice(&16u32.to_be_bytes());
    data.extend_from_slice(&codec::CANCEL_REQUEST.to_be_bytes());
    data.extend_from_slice(&1u32.to_be_bytes()); // pid
    data.extend_from_slice(&42u32.to_be_bytes()); // secret
    let result = run_with_bytes(data).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn flush_message_after_startup() {
    let mut input = build_startup_bytes();
    // Flush = 'H' + length 4.
    input.extend(build_raw_message(b'H', &[]));
    input.extend(build_terminate_bytes());
    let result = run_with_bytes(input).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn many_unknown_tags_then_terminate() {
    let mut input = build_startup_bytes();
    // Send 50 messages with unknown tag bytes.
    for tag in 200u8..250u8 {
        input.extend(build_raw_message(tag, b"\0"));
    }
    input.extend(build_terminate_bytes());
    let result = run_with_bytes(input).await;
    // All unknown tags should be handled (error response) without panicking.
    assert!(result.is_ok());
}

#[tokio::test]
async fn interleaved_simple_and_extended_protocol() {
    let mut input = build_startup_bytes();
    // Parse + Sync
    input.extend(build_parse_bytes("s1", "SELECT 1", &[]));
    input.extend(build_sync_bytes());
    // Simple query in between
    input.extend(build_query_bytes("SELECT 2"));
    // Describe + Sync
    input.extend(build_describe_bytes(b'S', "s1"));
    input.extend(build_sync_bytes());
    // Close + Sync
    let mut close_payload = Vec::new();
    close_payload.push(b'S');
    close_payload.extend_from_slice(b"s1\0");
    input.extend(build_raw_message(b'C', &close_payload));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());
    let result = run_with_bytes(input).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn empty_query_string() {
    let mut input = build_startup_bytes();
    input.extend(build_query_bytes(""));
    input.extend(build_terminate_bytes());
    let result = run_with_bytes(input).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn only_whitespace_query() {
    let mut input = build_startup_bytes();
    input.extend(build_query_bytes("   \t\n  "));
    input.extend(build_terminate_bytes());
    let result = run_with_bytes(input).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn message_exactly_length_four() {
    let mut input = build_startup_bytes();
    // A message with tag 'Q' and length = 4 means zero payload bytes.
    // This is technically valid framing but bad for Query (no SQL + null).
    input.extend(build_raw_message(b'Q', &[]));
    input.extend(build_terminate_bytes());
    let result = run_with_bytes(input).await;
    // Should send an error (missing null terminator) but not panic.
    assert!(result.is_ok());
}

#[tokio::test]
async fn bind_with_no_prior_parse_for_statement() {
    let mut input = build_startup_bytes();
    // Bind references a statement that was never parsed.
    input.extend(build_bind_bytes("", "nonexistent", &[], &[], &[]));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());
    let result = run_with_bytes(input).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn describe_with_invalid_target_byte() {
    let mut input = build_startup_bytes();
    // Describe with target byte 'X' (neither 'S' nor 'P').
    input.extend(build_raw_message(b'D', b"Xname\0"));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());
    let result = run_with_bytes(input).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn close_with_invalid_target_byte() {
    let mut input = build_startup_bytes();
    // Close with target byte 'Z' (neither 'S' nor 'P').
    input.extend(build_raw_message(b'C', b"Zname\0"));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());
    let result = run_with_bytes(input).await;
    assert!(result.is_ok());
}
