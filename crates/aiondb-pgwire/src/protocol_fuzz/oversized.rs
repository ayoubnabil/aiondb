use super::*;

// =========================================================================
// 5. Oversized payloads
// =========================================================================

#[tokio::test]
async fn oversized_query_string() {
    let mut input = build_startup_bytes();
    // Build a query string of 200KB (well beyond any reasonable max_sql_length).
    let big_sql: String = "A".repeat(200_000);
    input.extend(build_query_bytes(&big_sql));
    input.extend(build_terminate_bytes());
    let result = run_with_bytes(input).await;
    // The mock engine will just accept it, but no panic is the key.
    assert!(result.is_ok());
}

#[tokio::test]
async fn oversized_statement_name_in_parse() {
    let mut input = build_startup_bytes();
    let big_name: String = "s".repeat(70_000); // > 64KB
    input.extend(build_parse_bytes(&big_name, "SELECT 1", &[]));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());
    let result = run_with_bytes(input).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn many_parameters_in_bind() {
    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes("stmt1", "SELECT 1", &[]));
    input.extend(build_sync_bytes());
    // Build a Bind with 10000 NULL parameters.
    let params: Vec<Option<&[u8]>> = vec![None; 10_000];
    input.extend(build_bind_bytes("", "stmt1", &[], &params, &[]));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());
    let result = run_with_bytes(input).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn oversized_portal_name_in_bind() {
    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes("stmt1", "SELECT 1", &[]));
    input.extend(build_sync_bytes());
    let big_portal: String = "p".repeat(70_000);
    input.extend(build_bind_bytes(&big_portal, "stmt1", &[], &[], &[]));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());
    let result = run_with_bytes(input).await;
    assert!(result.is_ok());
}
