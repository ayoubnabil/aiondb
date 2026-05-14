use super::*;

// =========================================================================
// 6. Rapid disconnect
// =========================================================================

#[tokio::test]
async fn connect_then_immediately_close() {
    // Empty input -- the reader yields EOF on the very first read.
    let result = run_with_bytes(vec![]).await;
    assert!(result.is_err(), "immediate close should surface as error");
}

#[tokio::test]
async fn send_startup_then_close() {
    // Send a valid startup, then EOF.
    let input = build_startup_bytes();
    let result = run_with_bytes(input).await;
    // After startup succeeds, the message loop reads the next tag and hits
    // EOF, which it treats as a disconnection (not necessarily an Err from
    // `run()`, the loop just breaks).
    // Either Ok or Err is acceptable -- no panic is the key.
    let _ = result;
}

#[tokio::test]
async fn send_partial_message_then_close() {
    let mut input = build_startup_bytes();
    // Start a Query message: tag + 2 bytes of the 4-byte length, then EOF.
    input.push(b'Q');
    input.extend_from_slice(&[0, 0]);
    let result = run_with_bytes(input).await;
    // Should not hang.  The connection loop catches the read error and
    // breaks cleanly.
    assert!(
        result.is_ok(),
        "partial message then close handled gracefully"
    );
}

#[tokio::test]
async fn send_startup_tag_only_then_close() {
    let mut input = build_startup_bytes();
    // Send just the tag byte of a message, then EOF.
    input.push(b'Q');
    let result = run_with_bytes(input).await;
    // The connection loop catches the read error and breaks cleanly.
    assert!(result.is_ok(), "tag-only then close handled gracefully");
}

#[tokio::test]
async fn send_half_startup_then_close() {
    // Send only 2 bytes of the startup length (needs 4).
    let input = vec![0u8, 0];
    let result = run_with_bytes(input).await;
    assert!(result.is_err());
}
