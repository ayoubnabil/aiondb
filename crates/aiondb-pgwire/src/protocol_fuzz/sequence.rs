use super::*;

// =========================================================================
// 4. Sequence violations
// =========================================================================

#[tokio::test]
async fn query_before_startup() {
    // Send a Query message without any startup message first.
    // The server expects a startup packet (no tag byte, just length + version).
    // Sending a tagged message will confuse the startup reader.
    let input = build_query_bytes("SELECT 1");
    let result = run_with_bytes(input).await;
    assert!(result.is_err(), "query before startup should fail");
}

#[tokio::test]
async fn execute_without_prior_bind() {
    let mut input = build_startup_bytes();
    input.extend(build_execute_bytes("nonexistent_portal", 0));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());
    let result = run_with_bytes(input).await;
    // The mock engine returns Ok for execute_portal, but in a real scenario
    // this would error.  The key assertion is no panic.
    assert!(result.is_ok());
}

#[tokio::test]
async fn describe_nonexistent_statement() {
    let mut input = build_startup_bytes();
    input.extend(build_describe_bytes(b'S', "no_such_stmt"));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());
    let result = run_with_bytes(input).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn describe_nonexistent_portal() {
    let mut input = build_startup_bytes();
    input.extend(build_describe_bytes(b'P', "no_such_portal"));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());
    let result = run_with_bytes(input).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn close_nonexistent_statement() {
    let mut input = build_startup_bytes();
    let mut payload = Vec::new();
    payload.push(b'S');
    payload.extend_from_slice(b"no_such_stmt\0");
    input.extend(build_raw_message(b'C', &payload));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());
    let result = run_with_bytes(input).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn close_nonexistent_portal() {
    let mut input = build_startup_bytes();
    let mut payload = Vec::new();
    payload.push(b'P');
    payload.extend_from_slice(b"no_such_portal\0");
    input.extend(build_raw_message(b'C', &payload));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());
    let result = run_with_bytes(input).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn sync_before_any_extended_query() {
    let mut input = build_startup_bytes();
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());
    let result = run_with_bytes(input).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn multiple_syncs_in_a_row() {
    let mut input = build_startup_bytes();
    for _ in 0..10 {
        input.extend(build_sync_bytes());
    }
    input.extend(build_terminate_bytes());
    let result = run_with_bytes(input).await;
    assert!(result.is_ok());
}
