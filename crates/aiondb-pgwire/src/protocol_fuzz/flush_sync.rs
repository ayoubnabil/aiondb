use super::*;

// =========================================================================
// MISS-W3: Flush with unexpected payload
// =========================================================================

#[tokio::test]
async fn flush_with_unexpected_payload() {
    let mut input = build_startup_bytes();
    // Flush is 'H' -- per protocol it should have an empty payload (length=4).
    // Here we send extra garbage bytes in the payload. The server should
    // handle this gracefully (the extra bytes are simply ignored by the
    // FrontendMessage parser, which only checks the tag).
    input.extend(build_raw_message(b'H', b"unexpected extra data"));
    input.extend(build_terminate_bytes());
    let result = run_with_bytes(input).await;
    assert!(
        result.is_ok(),
        "flush with unexpected payload should be handled gracefully"
    );
}

#[tokio::test]
async fn flush_with_large_unexpected_payload() {
    let mut input = build_startup_bytes();
    // Flush with a large unexpected payload (1KB of data).
    let big_payload = vec![0xAA; 1024];
    input.extend(build_raw_message(b'H', &big_payload));
    input.extend(build_terminate_bytes());
    let result = run_with_bytes(input).await;
    assert!(
        result.is_ok(),
        "flush with large unexpected payload should be handled gracefully"
    );
}

// =========================================================================
// MISS-W4: Sync with unexpected payload
// =========================================================================

#[tokio::test]
async fn sync_with_unexpected_payload() {
    let mut input = build_startup_bytes();
    // Sync is 'S' -- per protocol it should have an empty payload (length=4).
    // Here we send extra bytes in the payload.
    input.extend(build_raw_message(b'S', b"unexpected sync data"));
    input.extend(build_terminate_bytes());
    let result = run_with_bytes(input).await;
    assert!(
        result.is_ok(),
        "sync with unexpected payload should be handled gracefully"
    );
}

#[tokio::test]
async fn sync_with_large_unexpected_payload() {
    let mut input = build_startup_bytes();
    let big_payload = vec![0xBB; 2048];
    input.extend(build_raw_message(b'S', &big_payload));
    input.extend(build_terminate_bytes());
    let result = run_with_bytes(input).await;
    assert!(
        result.is_ok(),
        "sync with large unexpected payload should be handled gracefully"
    );
}
