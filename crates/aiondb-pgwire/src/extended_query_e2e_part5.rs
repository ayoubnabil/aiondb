#[path = "extended_query_e2e_part5_rowdesc_and_casts.rs"]
mod rowdesc_and_casts;

#[tokio::test]
async fn extended_macaddr_text_param_executes_against_real_engine() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes("s_mac", "SELECT $1", &[829]));
    input.extend(build_bind_bytes(
        "p_mac",
        "s_mac",
        &[0],
        &[Some(b"08:00:2b:01:02:03")],
        &[],
    ));
    input.extend(build_execute_bytes("p_mac", 0));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("extended macaddr query should succeed against real engine");

    let messages = backend_messages(conn.writer_ref());
    let data_row = messages
        .iter()
        .find(|(tag, _)| *tag == b'D')
        .map(|(_, payload)| payload.as_slice())
        .expect("data row");
    assert_eq!(
        parse_data_row_columns(data_row),
        vec![Some(b"08:00:2b:01:02:03".to_vec())]
    );
}

#[tokio::test]
async fn extended_money_binary_param_executes_against_real_engine() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));
    let encoded =
        crate::binary_format::encode_binary_value(&Value::Money(123)).expect("encode money");

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes("s_money", "SELECT $1", &[790]));
    input.extend(build_bind_bytes(
        "p_money",
        "s_money",
        &[1],
        &[Some(encoded.as_slice())],
        &[],
    ));
    input.extend(build_execute_bytes("p_money", 0));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("extended money query should succeed against real engine");

    let messages = backend_messages(conn.writer_ref());
    let data_row = messages
        .iter()
        .find(|(tag, _)| *tag == b'D')
        .map(|(_, payload)| payload.as_slice())
        .expect("data row");
    assert_eq!(
        parse_data_row_columns(data_row),
        vec![Some(b"$1.23".to_vec())]
    );
}

#[tokio::test]
async fn extended_money_text_param_executes_against_real_engine() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes("s_money_text", "SELECT $1", &[790]));
    input.extend(build_bind_bytes(
        "p_money_text",
        "s_money_text",
        &[0],
        &[Some(b"$1.23")],
        &[],
    ));
    input.extend(build_execute_bytes("p_money_text", 0));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("extended money text query should succeed against real engine");

    let messages = backend_messages(conn.writer_ref());
    let data_row = messages
        .iter()
        .find(|(tag, _)| *tag == b'D')
        .map(|(_, payload)| payload.as_slice())
        .expect("data row");
    assert_eq!(
        parse_data_row_columns(data_row),
        vec![Some(b"$1.23".to_vec())]
    );
}

#[tokio::test]
async fn extended_vector_binary_param_executes_against_real_engine() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));
    let (session, _) = engine
        .startup(StartupParams {
            database: "default".to_owned(),
            application_name: Some("pgwire-e2e-binary-seed".to_owned()),
            options: std::collections::BTreeMap::new(),
            credential: Credential::Anonymous {
                user: "test".to_owned(),
            },
            transport: TransportInfo::in_process(),
        })
        .expect("startup seed session");
    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, v VECTOR(3)); \
             CREATE INDEX idx_v ON items USING hnsw (v); \
             INSERT INTO items VALUES \
                (1, '[1.0,0.0,0.0]'), \
                (2, '[0.0,1.0,0.0]'), \
                (3, '[0.0,0.0,1.0]')",
        )
        .expect("seed vector table");

    let query_vector = Value::Vector(VectorValue::new(3, vec![1.0, 0.0, 0.0]));
    let encoded =
        crate::binary_format::encode_binary_value(&query_vector).expect("encode vector binary");

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_vec_bin",
        "SELECT id FROM items ORDER BY l2_distance(v, $1) LIMIT 1",
        &[62_000],
    ));
    input.extend(build_bind_bytes(
        "p_vec_bin",
        "s_vec_bin",
        &[1],
        &[Some(encoded.as_slice())],
        &[],
    ));
    input.extend(build_execute_bytes("p_vec_bin", 0));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("extended binary vector query should succeed against real engine");

    let messages = backend_messages(conn.writer_ref());
    let data_row = messages
        .iter()
        .find(|(tag, _)| *tag == b'D')
        .map(|(_, payload)| payload.as_slice())
        .expect("data row");
    assert_eq!(parse_data_row_columns(data_row), vec![Some(b"1".to_vec())]);
}

#[tokio::test]
async fn extended_vector_binary_bind_error_recovers_after_sync() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));
    let (session, _) = engine
        .startup(StartupParams {
            database: "default".to_owned(),
            application_name: Some("pgwire-e2e-binary-recovery-seed".to_owned()),
            options: std::collections::BTreeMap::new(),
            credential: Credential::Anonymous {
                user: "test".to_owned(),
            },
            transport: TransportInfo::in_process(),
        })
        .expect("startup seed session");
    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, v VECTOR(3)); \
             INSERT INTO items VALUES (1, '[1.0,0.0,0.0]')",
        )
        .expect("seed vector table");

    let bad_query_vector = Value::Vector(VectorValue::new(2, vec![1.0, 0.0]));
    let bad_encoded =
        crate::binary_format::encode_binary_value(&bad_query_vector).expect("encode vector binary");

    let mut input = build_startup_bytes();
    // Batch 1: bind error (VECTOR(2) payload for expected VECTOR(3)).
    input.extend(build_parse_bytes(
        "s_bad",
        "SELECT id FROM items ORDER BY l2_distance(v, $1) LIMIT 1",
        &[62_000],
    ));
    input.extend(build_bind_bytes(
        "p_bad",
        "s_bad",
        &[1],
        &[Some(bad_encoded.as_slice())],
        &[],
    ));
    input.extend(build_execute_bytes("p_bad", 0));
    input.extend(build_sync_bytes());
    // Batch 2: must still execute successfully after Sync.
    input.extend(build_parse_bytes("s_ok", "SELECT 7", &[]));
    input.extend(build_bind_bytes("p_ok", "s_ok", &[], &[], &[]));
    input.extend(build_execute_bytes("p_ok", 0));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("binary bind error should recover after sync");

    let messages = backend_messages(conn.writer_ref());
    let tags: Vec<u8> = messages.iter().map(|(tag, _)| *tag).collect();
    assert_eq!(
        count_tag_occurrences(&tags, b'E'),
        1,
        "expected exactly one error response for the bad bind"
    );
    assert_eq!(
        count_tag_occurrences(&tags, b'C'),
        1,
        "execute from the failed batch must be skipped until sync"
    );

    let error_pos = tags
        .iter()
        .position(|tag| *tag == b'E')
        .expect("error response");
    let data_pos = tags
        .iter()
        .rposition(|tag| *tag == b'D')
        .expect("data row from second batch");
    assert!(
        data_pos > error_pos,
        "second batch data row should appear after first batch error"
    );
    let data_row = messages[data_pos].1.as_slice();
    assert_eq!(parse_data_row_columns(data_row), vec![Some(b"7".to_vec())]);
}

/// Parse -> Bind(binary results) -> Describe(portal) -> Execute -> Sync.
///
/// Ensures the extended protocol advertises and returns real PG wire binary
/// payloads for supported result types.
#[tokio::test]
async fn extended_binary_result_format_is_used_on_the_wire() {
    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes("s1", "SELECT id FROM t", &[]));
    input.extend(build_bind_bytes("p1", "s1", &[], &[], &[1]));
    input.extend(build_describe_bytes(b'P', "p1"));
    input.extend(build_execute_bytes("p1", 0));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let engine = Arc::new(ExtendedMockEngine);
    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());

    conn.run()
        .await
        .expect("binary extended query should succeed");

    let messages = backend_messages(conn.writer_ref());
    let row_description = messages
        .iter()
        .find(|(tag, _)| *tag == b'T')
        .map(|(_, payload)| payload.as_slice())
        .expect("row description");
    let data_row = messages
        .iter()
        .find(|(tag, _)| *tag == b'D')
        .map(|(_, payload)| payload.as_slice())
        .expect("data row");

    assert_eq!(parse_row_description_format_codes(row_description), vec![1]);
    assert_eq!(
        parse_data_row_columns(data_row),
        vec![Some(42_i32.to_be_bytes().to_vec())]
    );
}

#[tokio::test]
async fn extended_binary_result_format_without_prior_describe_is_used_on_the_wire() {
    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes("s1", "SELECT id FROM t", &[]));
    input.extend(build_bind_bytes("p1", "s1", &[], &[], &[1]));
    input.extend(build_execute_bytes("p1", 0));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let engine = Arc::new(ExtendedMockEngine);
    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());

    conn.run()
        .await
        .expect("binary extended query without describe should succeed");

    let messages = backend_messages(conn.writer_ref());
    let data_row = messages
        .iter()
        .find(|(tag, _)| *tag == b'D')
        .map(|(_, payload)| payload.as_slice())
        .expect("data row");

    assert!(
        !messages.iter().any(|(tag, _)| *tag == b'T'),
        "Execute should not emit RowDescription without an explicit Describe"
    );
    assert_eq!(
        parse_data_row_columns(data_row),
        vec![Some(42_i32.to_be_bytes().to_vec())]
    );
}

#[tokio::test]
async fn extended_binary_result_format_preserves_varchar_array_element_oid() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));
    let (session, _) = engine
        .startup(StartupParams {
            database: "default".to_owned(),
            application_name: Some("pgwire-e2e-binary-varchar-array-seed".to_owned()),
            options: std::collections::BTreeMap::new(),
            credential: Credential::Anonymous {
                user: "test".to_owned(),
            },
            transport: TransportInfo::in_process(),
        })
        .expect("startup seed session");
    engine
        .execute_sql(
            &session,
            "CREATE TABLE t_varchar_bin (v VARCHAR(5)[]); \
             INSERT INTO t_varchar_bin VALUES ('{\"aa\",\"bb\"}')",
        )
        .expect("seed varchar[] table");

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_varchar_bin_out",
        "SELECT v FROM t_varchar_bin",
        &[],
    ));
    input.extend(build_bind_bytes(
        "p_varchar_bin_out",
        "s_varchar_bin_out",
        &[],
        &[],
        &[1],
    ));
    input.extend(build_describe_bytes(b'P', "p_varchar_bin_out"));
    input.extend(build_execute_bytes("p_varchar_bin_out", 0));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("binary varchar[] extended query should succeed");

    let messages = backend_messages(conn.writer_ref());
    let row_description = messages
        .iter()
        .find(|(tag, _)| *tag == b'T')
        .map(|(_, payload)| payload.as_slice())
        .expect("row description");
    let data_row = messages
        .iter()
        .find(|(tag, _)| *tag == b'D')
        .map(|(_, payload)| payload.as_slice())
        .expect("data row");

    assert_eq!(parse_row_description_format_codes(row_description), vec![1]);
    assert_eq!(
        parse_row_description_type_info(row_description),
        vec![(1015, 9)]
    );
    let columns = parse_data_row_columns(data_row);
    let payload = columns[0].as_ref().expect("varchar[] payload");
    assert_eq!(u32::from_be_bytes(payload[8..12].try_into().unwrap()), 1043);
}

/// Parse -> Close(statement) -> Sync -> Terminate.
///
/// Tests that closing a statement works cleanly.
#[tokio::test]
async fn extended_parse_then_close_statement_sync() {
    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes("s1", "SELECT 1", &[]));
    input.extend(build_close_bytes(b'S', "s1"));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let engine = Arc::new(ExtendedMockEngine);
    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    let result = conn.run().await;
    assert!(result.is_ok(), "parse then close statement should work");
}

/// Error in Parse (bad SQL) -> Sync -> Terminate.
///
/// Should get `ErrorResponse` + `ReadyForQuery` after Sync.
#[tokio::test]
async fn extended_parse_error_then_sync() {
    let mut input = build_startup_bytes();
    // "ERROR" prefix triggers a mock parse error in `ExtendedMockEngine`.
    input.extend(build_parse_bytes("", "ERROR bad sql", &[]));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let engine = Arc::new(ExtendedMockEngine);
    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    let result = conn.run().await;
    assert!(
        result.is_ok(),
        "parse error followed by sync should recover gracefully"
    );
}

#[tokio::test]
async fn extended_parse_duplicate_named_statement_errors_until_sync() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes("s_dup", "SELECT 1", &[]));
    input.extend(build_parse_bytes("s_dup", "SELECT 2", &[]));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("duplicate named Parse should recover after Sync");

    let messages = backend_messages(conn.writer_ref());
    let parse_completes = messages.iter().filter(|(tag, _)| *tag == b'1').count();
    assert_eq!(parse_completes, 1, "only the first Parse should complete");
    let error_message = messages
        .iter()
        .find(|(tag, _)| *tag == b'E')
        .and_then(|(_, payload)| parse_error_response_message(payload))
        .expect("duplicate parse error");
    assert!(
        error_message.contains("already exists"),
        "unexpected duplicate parse error: {error_message}"
    );
    assert_eq!(
        messages.iter().filter(|(tag, _)| *tag == b'Z').count(),
        2,
        "startup and Sync should each emit ReadyForQuery"
    );
}

#[tokio::test]
async fn extended_parse_type_mismatch_does_not_leak_named_statement_after_sync() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes("s_bad", "SELECT $1::text", &[23]));
    input.extend(build_sync_bytes());
    input.extend(build_parse_bytes("s_bad", "SELECT $1::text", &[25]));
    input.extend(build_bind_bytes(
        "p_bad_recovered",
        "s_bad",
        &[],
        &[Some(b"hello")],
        &[],
    ));
    input.extend(build_execute_bytes("p_bad_recovered", 0));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("type-mismatch parse should recover after sync");

    let messages = backend_messages(conn.writer_ref());
    let error_message = messages
        .iter()
        .find(|(tag, _)| *tag == b'E')
        .and_then(|(_, payload)| parse_error_response_message(payload))
        .expect("expected parse type mismatch error");
    assert!(
        !error_message.contains("already exists"),
        "validation failure should not leak named prepared statement: {error_message}"
    );
    assert_eq!(
        messages.iter().filter(|(tag, _)| *tag == b'1').count(),
        1,
        "second Parse should succeed after Sync"
    );
    let data_row = messages
        .iter()
        .find(|(tag, _)| *tag == b'D')
        .map(|(_, payload)| payload.as_slice())
        .expect("expected DataRow from recovered execute");
    let columns = parse_data_row_columns(data_row);
    assert_eq!(columns, vec![Some(b"hello".to_vec())]);
}

#[tokio::test]
async fn extended_bind_duplicate_named_portal_errors_until_sync() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes("s_portal_dup", "SELECT 1", &[]));
    input.extend(build_bind_bytes("p_dup", "s_portal_dup", &[], &[], &[]));
    input.extend(build_bind_bytes("p_dup", "s_portal_dup", &[], &[], &[]));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("duplicate named Bind should recover after Sync");

    let messages = backend_messages(conn.writer_ref());
    let bind_completes = messages.iter().filter(|(tag, _)| *tag == b'2').count();
    assert_eq!(bind_completes, 1, "only the first Bind should complete");
    let error_message = messages
        .iter()
        .find(|(tag, _)| *tag == b'E')
        .and_then(|(_, payload)| parse_error_response_message(payload))
        .expect("duplicate bind error");
    assert!(
        error_message.contains("already exists"),
        "unexpected duplicate bind error: {error_message}"
    );
    assert_eq!(
        messages.iter().filter(|(tag, _)| *tag == b'Z').count(),
        2,
        "startup and Sync should each emit ReadyForQuery"
    );
}

#[tokio::test]
async fn extended_parse_replacing_unnamed_statement_closes_portals_bound_to_it() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes("", "SELECT 1", &[]));
    input.extend(build_bind_bytes("p_old", "", &[], &[], &[]));
    input.extend(build_parse_bytes("", "SELECT 2", &[]));
    input.extend(build_execute_bytes("p_old", 0));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("replacing unnamed statement should recover after Sync");

    let messages = backend_messages(conn.writer_ref());
    let error_message = messages
        .iter()
        .find(|(tag, _)| *tag == b'E')
        .and_then(|(_, payload)| parse_error_response_message(payload))
        .expect("unknown portal after unnamed statement replacement");
    assert!(
        error_message.contains("unknown portal"),
        "unexpected portal cleanup error: {error_message}"
    );
}

#[tokio::test]
async fn extended_failed_unnamed_parse_destroys_prior_unnamed_statement_and_portal() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes("", "SELECT 1", &[]));
    input.extend(build_bind_bytes("", "", &[], &[], &[]));
    input.extend(build_parse_bytes("", "SELECT $1::text", &[23]));
    input.extend(build_sync_bytes());
    input.extend(build_execute_bytes("", 0));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("failed unnamed Parse should recover after Sync");

    let messages = backend_messages(conn.writer_ref());
    let error_messages: Vec<String> = messages
        .iter()
        .filter(|(tag, _)| *tag == b'E')
        .filter_map(|(_, payload)| parse_error_response_message(payload))
        .collect();
    assert!(
        error_messages
            .iter()
            .any(|message| message.contains("could not infer consistent type for parameter $1")),
        "expected unnamed parse validation error, got {error_messages:?}"
    );
    assert!(
        error_messages
            .iter()
            .any(|message| message.contains("unknown portal")),
        "failed unnamed Parse should destroy the prior unnamed portal, got {error_messages:?}"
    );
}

#[tokio::test]
async fn extended_failed_unnamed_bind_destroys_prior_unnamed_portal() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes("", "SELECT 1", &[]));
    input.extend(build_bind_bytes("", "", &[], &[], &[]));
    input.extend(build_bind_bytes("", "", &[], &[], &[0, 0]));
    input.extend(build_sync_bytes());
    input.extend(build_execute_bytes("", 0));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("failed unnamed Bind should recover after Sync");

    let messages = backend_messages(conn.writer_ref());
    let error_messages: Vec<String> = messages
        .iter()
        .filter(|(tag, _)| *tag == b'E')
        .filter_map(|(_, payload)| parse_error_response_message(payload))
        .collect();
    assert!(
        error_messages
            .iter()
            .any(|message| message.contains("result format count mismatch")),
        "expected unnamed bind validation error, got {error_messages:?}"
    );
    assert!(
        error_messages
            .iter()
            .any(|message| message.contains("unknown portal")),
        "failed unnamed Bind should destroy the prior unnamed portal, got {error_messages:?}"
    );
}

#[tokio::test]
async fn simple_query_clears_prior_unnamed_statement_and_portal() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes("", "SELECT 1", &[]));
    input.extend(build_bind_bytes("", "", &[], &[], &[]));
    input.extend(build_query_bytes("SELECT 42"));
    input.extend(build_execute_bytes("", 0));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("simple query should clear prior unnamed extended-query state");

    let messages = backend_messages(conn.writer_ref());
    let error_message = messages
        .iter()
        .find(|(tag, _)| *tag == b'E')
        .and_then(|(_, payload)| parse_error_response_message(payload))
        .expect("Execute on cleared unnamed portal should fail");
    assert!(
        error_message.contains("unknown portal"),
        "expected unnamed portal cleanup after simple query, got {error_message}"
    );
    assert_eq!(
        messages.iter().filter(|(tag, _)| *tag == b'D').count(),
        1,
        "simple query should produce exactly one DataRow before the cleared unnamed Execute fails"
    );
}

/// Full cycle: Parse -> Bind -> Describe(statement) -> Execute -> Close(statement) -> Sync.
///
/// Tests the complete extended query sequence.
#[tokio::test]
async fn extended_full_cycle_parse_bind_describe_execute_close_sync() {
    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes("s1", "SELECT id FROM t", &[]));
    input.extend(build_bind_bytes("p1", "s1", &[], &[], &[]));
    input.extend(build_describe_bytes(b'S', "s1"));
    input.extend(build_execute_bytes("p1", 0));
    input.extend(build_close_bytes(b'S', "s1"));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let engine = Arc::new(ExtendedMockEngine);
    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    let result = conn.run().await;
    assert!(
        result.is_ok(),
        "full extended query cycle should complete without error"
    );
}

/// Multiple extended query batches in a single connection.
///
/// Tests that after one Parse->Bind->Execute->Sync cycle, another can follow.
#[tokio::test]
async fn extended_multiple_batches() {
    let mut input = build_startup_bytes();

    // First batch
    input.extend(build_parse_bytes("", "SELECT 1", &[]));
    input.extend(build_bind_bytes("", "", &[], &[], &[]));
    input.extend(build_execute_bytes("", 0));
    input.extend(build_sync_bytes());

    // Second batch
    input.extend(build_parse_bytes("", "SELECT 2", &[]));
    input.extend(build_bind_bytes("", "", &[], &[], &[]));
    input.extend(build_execute_bytes("", 0));
    input.extend(build_sync_bytes());

    input.extend(build_terminate_bytes());

    let engine = Arc::new(ExtendedMockEngine);
    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    let result = conn.run().await;
    assert!(
        result.is_ok(),
        "multiple extended query batches should work"
    );
}

/// Parse error recovery: error in first batch, then a second successful batch.
///
/// Tests that the connection recovers after an error via Sync.
#[tokio::test]
async fn extended_error_recovery_across_batches() {
    let mut input = build_startup_bytes();

    // First batch: parse error
    input.extend(build_parse_bytes("", "ERROR bad sql", &[]));
    input.extend(build_sync_bytes());

    // Second batch: should succeed
    input.extend(build_parse_bytes("", "SELECT 1", &[]));
    input.extend(build_bind_bytes("", "", &[], &[], &[]));
    input.extend(build_execute_bytes("", 0));
    input.extend(build_sync_bytes());

    input.extend(build_terminate_bytes());

    let engine = Arc::new(ExtendedMockEngine);
    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    let result = conn.run().await;
    assert!(result.is_ok(), "error recovery across batches should work");
}

/// Close portal (not statement) -> Sync -> Terminate.
///
/// Tests that closing a portal works cleanly.
#[tokio::test]
async fn extended_close_portal_sync() {
    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes("s1", "SELECT 1", &[]));
    input.extend(build_bind_bytes("p1", "s1", &[], &[], &[]));
    input.extend(build_close_bytes(b'P', "p1"));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let engine = Arc::new(ExtendedMockEngine);
    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    let result = conn.run().await;
    assert!(result.is_ok(), "close portal should work cleanly");
}

/// Describe statement (not portal) -> Sync -> Terminate.
///
/// Tests that describing a statement returns `ParameterDescription` + `RowDescription`.
#[tokio::test]
async fn extended_describe_statement_sync() {
    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes("s1", "SELECT id FROM t", &[]));
    input.extend(build_describe_bytes(b'S', "s1"));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let engine = Arc::new(ExtendedMockEngine);
    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    let result = conn.run().await;
    assert!(result.is_ok(), "describe statement should work");
}

