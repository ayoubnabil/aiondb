use super::*;

// =========================================================================
// 2. Invalid message types (after successful startup)
// =========================================================================

#[tokio::test]
async fn unknown_message_type_after_startup() {
    let mut input = build_startup_bytes();
    // Send an unknown tag 0xFF with a minimal payload.
    input.extend(build_raw_message(0xFF, b"hello\0"));
    // Server should send an error response but not panic.
    // Follow up with Terminate so the connection loop can exit.
    input.extend(build_terminate_bytes());
    let result = run_with_bytes(input).await;
    assert!(
        result.is_ok(),
        "unknown message type should be handled gracefully"
    );
}

#[tokio::test]
async fn message_with_length_zero() {
    let mut input = build_startup_bytes();
    // Tag + length of 0 (which is < 4, minimum).
    input.push(b'Q');
    input.extend_from_slice(&0u32.to_be_bytes());
    let result = run_with_bytes(input).await;
    // The codec rejects length < 4 with an error.  The connection message
    // loop catches read errors and breaks cleanly, so run() returns Ok.
    assert!(
        result.is_ok(),
        "length-0 message should be handled gracefully"
    );
}

#[tokio::test]
async fn message_with_length_exceeding_data() {
    let mut input = build_startup_bytes();
    // Claim a length of 1000 but provide no actual data afterward.
    input.push(b'Q');
    input.extend_from_slice(&1000u32.to_be_bytes());
    // EOF will be hit when trying to read the payload.  The connection
    // message loop catches the read error and breaks cleanly.
    let result = run_with_bytes(input).await;
    assert!(
        result.is_ok(),
        "length-exceeding message should be handled gracefully"
    );
}

#[tokio::test]
async fn message_with_length_less_than_four() {
    let mut input = build_startup_bytes();
    input.push(b'Q');
    input.extend_from_slice(&3u32.to_be_bytes()); // length 3 < 4
    let result = run_with_bytes(input).await;
    // The connection loop catches the codec error and breaks cleanly.
    assert!(
        result.is_ok(),
        "length < 4 message should be handled gracefully"
    );
}

#[tokio::test]
async fn message_with_length_one() {
    let mut input = build_startup_bytes();
    input.push(b'Q');
    input.extend_from_slice(&1u32.to_be_bytes());
    let result = run_with_bytes(input).await;
    // The connection loop catches the codec error and breaks cleanly.
    assert!(
        result.is_ok(),
        "length-1 message should be handled gracefully"
    );
}
