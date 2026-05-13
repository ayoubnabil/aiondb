use super::*;

// =========================================================================
// 1. Malformed startup messages
// =========================================================================

#[tokio::test]
async fn startup_wrong_protocol_version() {
    let payload_version = 0x0002_0000u32; // protocol v2
    let data = build_startup_raw(&payload_version.to_be_bytes());
    let result = run_with_bytes(data).await;
    assert!(
        result.is_err(),
        "wrong protocol version should produce error"
    );
}

#[tokio::test]
async fn startup_truncated_length_only() {
    // Only 4 bytes: the length field claiming 8 bytes, but no version follows.
    let data = vec![0, 0, 0, 8];
    let result = run_with_bytes(data).await;
    assert!(result.is_err(), "truncated startup should produce error");
}

#[tokio::test]
async fn startup_oversized_beyond_10kb() {
    // Craft a startup message claiming to be >10KB and provide matching data.
    let size: usize = 11_000;
    let mut payload = Vec::with_capacity(size);
    payload.extend_from_slice(&codec::PROTOCOL_V3.to_be_bytes());
    // Fill the rest with 'A' pairs: "AAAA\0AAAA\0" ...
    while payload.len() < size - 4 - 1 {
        payload.push(b'A');
    }
    payload.push(0); // final null
    let data = build_startup_raw(&payload);

    // The server should not hang or panic.  It may accept or reject.
    let _result = run_with_bytes(data).await;
}

#[tokio::test]
async fn startup_missing_null_terminators() {
    // Version is correct but the key-value params have no null terminators.
    let mut payload = Vec::new();
    payload.extend_from_slice(&codec::PROTOCOL_V3.to_be_bytes());
    payload.extend_from_slice(b"user_without_null"); // no \0
    let data = build_startup_raw(&payload);
    let result = run_with_bytes(data).await;
    assert!(
        result.is_err(),
        "missing null terminator should produce error"
    );
}

#[tokio::test]
async fn startup_invalid_parameter_names() {
    // Use invalid (control-character-laden) parameter names.
    let mut payload = Vec::new();
    payload.extend_from_slice(&codec::PROTOCOL_V3.to_be_bytes());
    payload.extend_from_slice(b"\x01\x02\x03\0value\0\0");
    let data = build_startup_raw(&payload);
    // Should not panic.
    let _result = run_with_bytes(data).await;
}

#[tokio::test]
async fn startup_empty_payload() {
    // Length says 4 (just itself) -- too short, no version.
    let data: Vec<u8> = 4u32.to_be_bytes().to_vec();
    let result = run_with_bytes(data).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn startup_zero_length() {
    // Length field = 0.
    let data: Vec<u8> = 0u32.to_be_bytes().to_vec();
    let result = run_with_bytes(data).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn startup_garbage_version() {
    let data = build_startup_raw(&0xDEAD_BEEFu32.to_be_bytes());
    let result = run_with_bytes(data).await;
    assert!(result.is_err());
}
