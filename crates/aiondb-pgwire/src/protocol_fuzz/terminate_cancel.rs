use super::*;

// =========================================================================
// MISS-W5: Terminate without completing startup
// =========================================================================

#[tokio::test]
async fn terminate_without_completing_startup() {
    // Send just a Terminate message as if it were a startup message.
    // The startup reader expects length(u32) + version(u32), so the 'X'
    // byte and length bytes will be misinterpreted. The server should not
    // panic and should return an error or handle gracefully.
    let input = build_terminate_bytes();
    let result = run_with_bytes(input).await;
    // The startup phase will read the 'X' byte as the first byte of the
    // 4-byte length, which will produce a bogus length and likely error.
    assert!(
        result.is_err(),
        "terminate without startup should produce an error"
    );
}

#[tokio::test]
async fn terminate_as_first_tagged_message_before_v3_startup() {
    // Send a startup-sized message that looks like a Terminate.
    // Length = 8, version = 'X' padded -- this is not a valid version.
    let mut input = Vec::new();
    input.extend_from_slice(&8u32.to_be_bytes());
    input.push(b'X');
    input.extend_from_slice(&[0, 0, 0]); // pad to 4 bytes for "version"
    let result = run_with_bytes(input).await;
    assert!(
        result.is_err(),
        "terminate-like startup should produce an error"
    );
}

#[tokio::test]
async fn eof_during_startup_before_any_message() {
    // An empty input: the client connects and immediately disconnects
    // before sending any startup message. The server should handle this
    // as a startup read error.
    let result = run_with_bytes(vec![]).await;
    assert!(result.is_err(), "EOF before startup should be an error");
}

// =========================================================================
// MISS-W6: CancelRequest with wrong secret key
// =========================================================================

#[tokio::test]
async fn cancel_request_wrong_secret_key() {
    // Register a session in the cancel registry, then send a CancelRequest
    // with the correct pid but wrong secret key. The lookup should fail
    // (return None), and the connection should close cleanly without
    // cancelling anything.
    let registry = CancelRegistry::new();
    let handle = aiondb_engine::SessionHandle::test_handle();
    registry.register(1, 42, handle);

    // Build CancelRequest with pid=1 but wrong secret=999.
    let mut data = Vec::new();
    data.extend_from_slice(&16u32.to_be_bytes());
    data.extend_from_slice(&codec::CANCEL_REQUEST.to_be_bytes());
    data.extend_from_slice(&1u32.to_be_bytes()); // correct pid
    data.extend_from_slice(&999u32.to_be_bytes()); // WRONG secret key

    let engine = std::sync::Arc::new(MockEngine);
    let reader = std::io::Cursor::new(data);
    let writer: Vec<u8> = Vec::new();
    let mut conn =
        crate::connection::Connection::new(engine, reader, writer, 99, 0, registry.clone());
    let result = conn.run().await;
    assert!(
        result.is_ok(),
        "cancel request with wrong secret should complete without error"
    );

    // The session should still be registered (not cancelled/removed).
    assert!(
        registry.lookup(1, 42).is_some(),
        "session should still be registered after failed cancel"
    );
}

#[tokio::test]
async fn cancel_request_wrong_pid() {
    // CancelRequest with a pid that does not exist in the registry.
    let registry = CancelRegistry::new();
    let handle = aiondb_engine::SessionHandle::test_handle();
    registry.register(1, 42, handle);

    // Build CancelRequest with wrong pid=999 and secret=42.
    let mut data = Vec::new();
    data.extend_from_slice(&16u32.to_be_bytes());
    data.extend_from_slice(&codec::CANCEL_REQUEST.to_be_bytes());
    data.extend_from_slice(&999u32.to_be_bytes()); // WRONG pid
    data.extend_from_slice(&42u32.to_be_bytes()); // correct secret

    let engine = std::sync::Arc::new(MockEngine);
    let reader = std::io::Cursor::new(data);
    let writer: Vec<u8> = Vec::new();
    let mut conn =
        crate::connection::Connection::new(engine, reader, writer, 99, 0, registry.clone());
    let result = conn.run().await;
    assert!(
        result.is_ok(),
        "cancel request with wrong pid should complete without error"
    );

    // Original session should still be there.
    assert!(
        registry.lookup(1, 42).is_some(),
        "session should still be registered"
    );
}

#[tokio::test]
async fn cancel_request_both_wrong() {
    // CancelRequest where both pid and secret are wrong.
    let registry = CancelRegistry::new();
    let handle = aiondb_engine::SessionHandle::test_handle();
    registry.register(1, 42, handle);

    let mut data = Vec::new();
    data.extend_from_slice(&16u32.to_be_bytes());
    data.extend_from_slice(&codec::CANCEL_REQUEST.to_be_bytes());
    data.extend_from_slice(&999u32.to_be_bytes()); // wrong pid
    data.extend_from_slice(&888u32.to_be_bytes()); // wrong secret

    let engine = std::sync::Arc::new(MockEngine);
    let reader = std::io::Cursor::new(data);
    let writer: Vec<u8> = Vec::new();
    let mut conn =
        crate::connection::Connection::new(engine, reader, writer, 99, 0, registry.clone());
    let result = conn.run().await;
    assert!(
        result.is_ok(),
        "cancel request with both wrong should complete without error"
    );
}

#[tokio::test]
async fn cancel_request_empty_registry() {
    // CancelRequest when the registry is completely empty.
    let registry = CancelRegistry::new();

    let mut data = Vec::new();
    data.extend_from_slice(&16u32.to_be_bytes());
    data.extend_from_slice(&codec::CANCEL_REQUEST.to_be_bytes());
    data.extend_from_slice(&1u32.to_be_bytes());
    data.extend_from_slice(&42u32.to_be_bytes());

    let engine = std::sync::Arc::new(MockEngine);
    let reader = std::io::Cursor::new(data);
    let writer: Vec<u8> = Vec::new();
    let mut conn = crate::connection::Connection::new(engine, reader, writer, 99, 0, registry);
    let result = conn.run().await;
    assert!(
        result.is_ok(),
        "cancel request against empty registry should complete without error"
    );
}
