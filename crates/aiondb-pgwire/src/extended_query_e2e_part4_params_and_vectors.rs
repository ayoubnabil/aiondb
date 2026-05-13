use super::*;

/// Tests that parameter binding works end-to-end.
#[tokio::test]
async fn extended_parse_bind_with_params_execute_sync() {
    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes("", "SELECT $1", &[23]));
    input.extend(build_bind_bytes("", "", &[0], &[Some(b"42")], &[]));
    input.extend(build_execute_bytes("", 0));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let engine = Arc::new(ExtendedMockEngine);
    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    let result = conn.run().await;
    assert!(result.is_ok(), "extended query with params should work");
}

#[tokio::test]
async fn extended_parse_oid_hint_untyped_select_param_executes_against_real_engine() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes("s_hint", "SELECT $1", &[23]));
    input.extend(build_bind_bytes(
        "p_hint",
        "s_hint",
        &[0],
        &[Some(b"42")],
        &[],
    ));
    input.extend(build_execute_bytes("p_hint", 0));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("extended hinted SELECT $1 should succeed against real engine");

    let messages = backend_messages(conn.writer_ref());
    assert!(
        !messages.iter().any(|(tag, _)| *tag == b'E'),
        "unexpected ErrorResponse in backend stream"
    );
    let data_row = messages
        .iter()
        .find(|(tag, _)| *tag == b'D')
        .map(|(_, payload)| payload.as_slice())
        .expect("data row");
    assert_eq!(parse_data_row_columns(data_row), vec![Some(b"42".to_vec())]);
}

#[tokio::test]
async fn extended_parse_oid_hint_int_array_multidimensional_param_executes_against_real_engine() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_int_arr_2d",
        "SELECT cardinality($1)",
        &[1007],
    ));
    input.extend(build_bind_bytes(
        "p_int_arr_2d",
        "s_int_arr_2d",
        &[0],
        &[Some(b"{{1,2},{3,4}}")],
        &[],
    ));
    input.extend(build_execute_bytes("p_int_arr_2d", 0));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("extended multidimensional int[] parameter should execute against real engine");

    let messages = backend_messages(conn.writer_ref());
    assert!(
        !messages.iter().any(|(tag, _)| *tag == b'E'),
        "unexpected ErrorResponse in backend stream"
    );
    let data_row = messages
        .iter()
        .find(|(tag, _)| *tag == b'D')
        .map(|(_, payload)| payload.as_slice())
        .expect("data row");
    assert_eq!(parse_data_row_columns(data_row), vec![Some(b"4".to_vec())]);
}

#[tokio::test]
async fn extended_parse_oid_hint_int_array_with_explicit_bounds_executes_against_real_engine() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_int_arr_bounds",
        "SELECT cardinality($1)",
        &[1007],
    ));
    input.extend(build_bind_bytes(
        "p_int_arr_bounds",
        "s_int_arr_bounds",
        &[0],
        &[Some(b"[2:3]={10,20}")],
        &[],
    ));
    input.extend(build_execute_bytes("p_int_arr_bounds", 0));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("extended int[] parameter with explicit bounds should execute against real engine");

    let messages = backend_messages(conn.writer_ref());
    assert!(
        !messages.iter().any(|(tag, _)| *tag == b'E'),
        "unexpected ErrorResponse in backend stream"
    );
    let data_row = messages
        .iter()
        .find(|(tag, _)| *tag == b'D')
        .map(|(_, payload)| payload.as_slice())
        .expect("data row");
    assert_eq!(parse_data_row_columns(data_row), vec![Some(b"2".to_vec())]);
}

#[tokio::test]
async fn extended_parse_oid_hint_internal_char_param_executes_against_real_engine() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_internal_char",
        "SELECT upper($1)",
        &[18],
    ));
    input.extend(build_bind_bytes(
        "p_internal_char",
        "s_internal_char",
        &[0],
        &[Some(b"x")],
        &[],
    ));
    input.extend(build_execute_bytes("p_internal_char", 0));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("extended internal char parameter should execute against real engine");

    let messages = backend_messages(conn.writer_ref());
    assert!(
        !messages.iter().any(|(tag, _)| *tag == b'E'),
        "unexpected ErrorResponse in backend stream"
    );
    let data_row = messages
        .iter()
        .find(|(tag, _)| *tag == b'D')
        .map(|(_, payload)| payload.as_slice())
        .expect("data row");
    assert_eq!(parse_data_row_columns(data_row), vec![Some(b"X".to_vec())]);
}

#[tokio::test]
async fn extended_parse_oid_hint_name_param_executes_against_real_engine() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes("s_name", "SELECT upper($1)", &[19]));
    input.extend(build_bind_bytes(
        "p_name",
        "s_name",
        &[0],
        &[Some(b"hello")],
        &[],
    ));
    input.extend(build_execute_bytes("p_name", 0));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("extended name parameter should execute against real engine");

    let messages = backend_messages(conn.writer_ref());
    assert!(
        !messages.iter().any(|(tag, _)| *tag == b'E'),
        "unexpected ErrorResponse in backend stream"
    );
    let data_row = messages
        .iter()
        .find(|(tag, _)| *tag == b'D')
        .map(|(_, payload)| payload.as_slice())
        .expect("data row");
    assert_eq!(
        parse_data_row_columns(data_row),
        vec![Some(b"HELLO".to_vec())]
    );
}

#[tokio::test]
async fn extended_parse_oid_hint_unknown_param_executes_against_real_engine() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes("s_unknown", "SELECT upper($1)", &[705]));
    input.extend(build_bind_bytes(
        "p_unknown",
        "s_unknown",
        &[0],
        &[Some(b"hello")],
        &[],
    ));
    input.extend(build_execute_bytes("p_unknown", 0));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("extended unknown parameter should execute against real engine");

    let messages = backend_messages(conn.writer_ref());
    assert!(
        !messages.iter().any(|(tag, _)| *tag == b'E'),
        "unexpected ErrorResponse in backend stream"
    );
    let data_row = messages
        .iter()
        .find(|(tag, _)| *tag == b'D')
        .map(|(_, payload)| payload.as_slice())
        .expect("data row");
    assert_eq!(
        parse_data_row_columns(data_row),
        vec![Some(b"HELLO".to_vec())]
    );
}

#[tokio::test]
async fn extended_parse_oid_hint_cstring_param_executes_against_real_engine() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes("s_cstring", "SELECT upper($1)", &[2275]));
    input.extend(build_bind_bytes(
        "p_cstring",
        "s_cstring",
        &[0],
        &[Some(b"hello")],
        &[],
    ));
    input.extend(build_execute_bytes("p_cstring", 0));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("extended cstring parameter should execute against real engine");

    let messages = backend_messages(conn.writer_ref());
    assert!(
        !messages.iter().any(|(tag, _)| *tag == b'E'),
        "unexpected ErrorResponse in backend stream"
    );
    let data_row = messages
        .iter()
        .find(|(tag, _)| *tag == b'D')
        .map(|(_, payload)| payload.as_slice())
        .expect("data row");
    assert_eq!(
        parse_data_row_columns(data_row),
        vec![Some(b"HELLO".to_vec())]
    );
}

#[tokio::test]
async fn extended_parse_oid_hint_regclass_param_executes_against_real_engine() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes("s_regclass", "SELECT $1", &[2205]));
    input.extend(build_bind_bytes(
        "p_regclass",
        "s_regclass",
        &[0],
        &[Some(b"123")],
        &[],
    ));
    input.extend(build_execute_bytes("p_regclass", 0));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("extended regclass parameter should execute against real engine");

    let messages = backend_messages(conn.writer_ref());
    assert!(
        !messages.iter().any(|(tag, _)| *tag == b'E'),
        "unexpected ErrorResponse in backend stream"
    );
    let data_row = messages
        .iter()
        .find(|(tag, _)| *tag == b'D')
        .map(|(_, payload)| payload.as_slice())
        .expect("data row");
    assert_eq!(
        parse_data_row_columns(data_row),
        vec![Some(b"123".to_vec())]
    );
}

#[tokio::test]
async fn extended_cast_regclass_unknown_param_executes_against_real_engine() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_regclass_cast",
        "SELECT CAST($1 AS REGCLASS)::TEXT",
        &[0],
    ));
    input.extend(build_bind_bytes(
        "p_regclass_cast",
        "s_regclass_cast",
        &[0],
        &[Some(b"pg_catalog.pg_class")],
        &[],
    ));
    input.extend(build_execute_bytes("p_regclass_cast", 0));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("extended cast regclass parameter should execute against real engine");

    let messages = backend_messages(conn.writer_ref());
    assert!(
        !messages.iter().any(|(tag, _)| *tag == b'E'),
        "unexpected ErrorResponse in backend stream"
    );
    let data_row = messages
        .iter()
        .find(|(tag, _)| *tag == b'D')
        .map(|(_, payload)| payload.as_slice())
        .expect("data row");
    assert_eq!(
        parse_data_row_columns(data_row),
        vec![Some(b"pg_catalog.pg_class".to_vec())]
    );
}

#[tokio::test]
async fn extended_parse_oid_hint_internal_char_array_param_executes_against_real_engine() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_internal_char_arr",
        "SELECT cardinality($1)",
        &[1002],
    ));
    input.extend(build_bind_bytes(
        "p_internal_char_arr",
        "s_internal_char_arr",
        &[0],
        &[Some(br#"{"a","b"}"#)],
        &[],
    ));
    input.extend(build_execute_bytes("p_internal_char_arr", 0));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("extended internal char[] parameter should execute against real engine");

    let messages = backend_messages(conn.writer_ref());
    assert!(
        !messages.iter().any(|(tag, _)| *tag == b'E'),
        "unexpected ErrorResponse in backend stream"
    );
    let data_row = messages
        .iter()
        .find(|(tag, _)| *tag == b'D')
        .map(|(_, payload)| payload.as_slice())
        .expect("data row");
    assert_eq!(parse_data_row_columns(data_row), vec![Some(b"2".to_vec())]);
}

#[tokio::test]
async fn extended_parse_oid_hint_name_array_param_executes_against_real_engine() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_name_arr",
        "SELECT cardinality($1)",
        &[1003],
    ));
    input.extend(build_bind_bytes(
        "p_name_arr",
        "s_name_arr",
        &[0],
        &[Some(br#"{"aa","bb"}"#)],
        &[],
    ));
    input.extend(build_execute_bytes("p_name_arr", 0));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("extended name[] parameter should execute against real engine");

    let messages = backend_messages(conn.writer_ref());
    assert!(
        !messages.iter().any(|(tag, _)| *tag == b'E'),
        "unexpected ErrorResponse in backend stream"
    );
    let data_row = messages
        .iter()
        .find(|(tag, _)| *tag == b'D')
        .map(|(_, payload)| payload.as_slice())
        .expect("data row");
    assert_eq!(parse_data_row_columns(data_row), vec![Some(b"2".to_vec())]);
}

#[tokio::test]
async fn extended_int_array_binary_param_with_non_default_lower_bound_executes_against_real_engine()
{
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));
    let encoded = build_text_array_binary_bytes_with_lbound(23, &["10", "20"], 2);

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_int_arr_bin_lbound",
        "SELECT cardinality($1)",
        &[1007],
    ));
    input.extend(build_bind_bytes(
        "p_int_arr_bin_lbound",
        "s_int_arr_bin_lbound",
        &[1],
        &[Some(encoded.as_slice())],
        &[],
    ));
    input.extend(build_execute_bytes("p_int_arr_bin_lbound", 0));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("extended int[] binary parameter with non-default lower bound should execute");

    let messages = backend_messages(conn.writer_ref());
    assert!(
        !messages.iter().any(|(tag, _)| *tag == b'E'),
        "unexpected ErrorResponse in backend stream"
    );
    let data_row = messages
        .iter()
        .find(|(tag, _)| *tag == b'D')
        .map(|(_, payload)| payload.as_slice())
        .expect("data row");
    assert_eq!(parse_data_row_columns(data_row), vec![Some(b"2".to_vec())]);
}

#[tokio::test]
async fn extended_internal_char_array_binary_param_executes_against_real_engine() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));
    let encoded = build_text_array_binary_bytes(18, &["a", "b"]);

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_internal_char_arr_bin",
        "SELECT cardinality($1)",
        &[1002],
    ));
    input.extend(build_bind_bytes(
        "p_internal_char_arr_bin",
        "s_internal_char_arr_bin",
        &[1],
        &[Some(encoded.as_slice())],
        &[],
    ));
    input.extend(build_execute_bytes("p_internal_char_arr_bin", 0));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("extended internal char[] binary parameter should execute against real engine");

    let messages = backend_messages(conn.writer_ref());
    assert!(
        !messages.iter().any(|(tag, _)| *tag == b'E'),
        "unexpected ErrorResponse in backend stream"
    );
    let data_row = messages
        .iter()
        .find(|(tag, _)| *tag == b'D')
        .map(|(_, payload)| payload.as_slice())
        .expect("data row");
    assert_eq!(parse_data_row_columns(data_row), vec![Some(b"2".to_vec())]);
}

#[tokio::test]
async fn extended_name_array_binary_param_executes_against_real_engine() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));
    let encoded = build_text_array_binary_bytes(19, &["aa", "bb"]);

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_name_arr_bin",
        "SELECT cardinality($1)",
        &[1003],
    ));
    input.extend(build_bind_bytes(
        "p_name_arr_bin",
        "s_name_arr_bin",
        &[1],
        &[Some(encoded.as_slice())],
        &[],
    ));
    input.extend(build_execute_bytes("p_name_arr_bin", 0));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("extended name[] binary parameter should execute against real engine");

    let messages = backend_messages(conn.writer_ref());
    assert!(
        !messages.iter().any(|(tag, _)| *tag == b'E'),
        "unexpected ErrorResponse in backend stream"
    );
    let data_row = messages
        .iter()
        .find(|(tag, _)| *tag == b'D')
        .map(|(_, payload)| payload.as_slice())
        .expect("data row");
    assert_eq!(parse_data_row_columns(data_row), vec![Some(b"2".to_vec())]);
}

#[tokio::test]
async fn extended_parse_oid_hint_bpchar_array_param_executes_against_real_engine() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_bpchar_arr",
        "SELECT cardinality($1)",
        &[1014],
    ));
    input.extend(build_bind_bytes(
        "p_bpchar_arr",
        "s_bpchar_arr",
        &[0],
        &[Some(br#"{"aa","bb"}"#)],
        &[],
    ));
    input.extend(build_execute_bytes("p_bpchar_arr", 0));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("extended bpchar[] parameter should execute against real engine");

    let messages = backend_messages(conn.writer_ref());
    assert!(
        !messages.iter().any(|(tag, _)| *tag == b'E'),
        "unexpected ErrorResponse in backend stream"
    );
    let data_row = messages
        .iter()
        .find(|(tag, _)| *tag == b'D')
        .map(|(_, payload)| payload.as_slice())
        .expect("data row");
    assert_eq!(parse_data_row_columns(data_row), vec![Some(b"2".to_vec())]);
}

#[tokio::test]
async fn extended_varchar_array_binary_param_executes_against_real_engine() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));
    let encoded = build_text_array_binary_bytes(1043, &["aa", "bb"]);

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_varchar_arr_bin",
        "SELECT cardinality($1)",
        &[1015],
    ));
    input.extend(build_bind_bytes(
        "p_varchar_arr_bin",
        "s_varchar_arr_bin",
        &[1],
        &[Some(encoded.as_slice())],
        &[],
    ));
    input.extend(build_execute_bytes("p_varchar_arr_bin", 0));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("extended varchar[] binary parameter should execute against real engine");

    let messages = backend_messages(conn.writer_ref());
    assert!(
        !messages.iter().any(|(tag, _)| *tag == b'E'),
        "unexpected ErrorResponse in backend stream"
    );
    let data_row = messages
        .iter()
        .find(|(tag, _)| *tag == b'D')
        .map(|(_, payload)| payload.as_slice())
        .expect("data row");
    assert_eq!(parse_data_row_columns(data_row), vec![Some(b"2".to_vec())]);
}

#[tokio::test]
async fn extended_vector_text_param_executes_against_real_engine() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));
    let (session, _) = engine
        .startup(StartupParams {
            database: "default".to_owned(),
            application_name: Some("pgwire-e2e-seed".to_owned()),
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

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_vec",
        "SELECT id FROM items ORDER BY l2_distance(v, $1) LIMIT 1",
        &[62_000],
    ));
    input.extend(build_bind_bytes(
        "p_vec",
        "s_vec",
        &[0],
        &[Some(b"[1.0,0.0,0.0]")],
        &[],
    ));
    input.extend(build_execute_bytes("p_vec", 0));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("extended vector query should succeed against real engine");

    let messages = backend_messages(conn.writer_ref());
    let data_row = messages
        .iter()
        .find(|(tag, _)| *tag == b'D')
        .map(|(_, payload)| payload.as_slice())
        .expect("data row");
    assert_eq!(parse_data_row_columns(data_row), vec![Some(b"1".to_vec())]);
}
