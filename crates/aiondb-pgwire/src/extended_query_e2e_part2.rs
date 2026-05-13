#[path = "extended_query_e2e_part2_compat_execute_and_notices.rs"]
mod compat_execute_and_notices;

#[tokio::test]
async fn simple_query_move_zero_reports_move_zero_and_keeps_cursor_position() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));
    let (session, _) = engine
        .startup(StartupParams {
            database: "default".to_owned(),
            application_name: Some("pgwire-simple-move-zero-seed".to_owned()),
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
            "CREATE TABLE t_move_zero_tag (id INT);
             INSERT INTO t_move_zero_tag VALUES (1), (2)",
        )
        .expect("seed move zero table");

    let mut input = build_startup_bytes();
    input.extend(build_query_bytes("BEGIN"));
    input.extend(build_query_bytes(
        "DECLARE c CURSOR FOR SELECT id FROM t_move_zero_tag ORDER BY id",
    ));
    input.extend(build_query_bytes("MOVE 0 IN c"));
    input.extend(build_query_bytes("FETCH NEXT IN c"));
    input.extend(build_query_bytes("COMMIT"));
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run().await.expect("simple MOVE 0 should complete");

    let messages = backend_messages(conn.writer_ref());
    let commands: Vec<String> = messages
        .iter()
        .filter(|(tag, _)| *tag == b'C')
        .map(|(_, payload)| parse_cstring_payload(payload).to_owned())
        .collect();
    assert!(
        commands
            .windows(2)
            .any(|window| window == ["MOVE 0", "FETCH 1"]),
        "expected MOVE 0 then FETCH 1 command completes, got {commands:?}"
    );
    let data_rows: Vec<Vec<Option<Vec<u8>>>> = messages
        .iter()
        .filter(|(tag, _)| *tag == b'D')
        .map(|(_, payload)| parse_data_row_columns(payload))
        .collect();
    assert_eq!(data_rows, vec![vec![Some(b"1".to_vec())]]);
}

#[tokio::test]
async fn extended_execute_fetch_reports_fetch_tag() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));
    let (session, _) = engine
        .startup(StartupParams {
            database: "default".to_owned(),
            application_name: Some("pgwire-extended-fetch-tag-seed".to_owned()),
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
            "CREATE TABLE t_fetch_tag_ext (id INT);
             INSERT INTO t_fetch_tag_ext VALUES (1), (2)",
        )
        .expect("seed extended fetch tag table");

    let mut input = build_startup_bytes();
    input.extend(build_query_bytes("BEGIN"));
    input.extend(build_query_bytes(
        "DECLARE c CURSOR FOR SELECT id FROM t_fetch_tag_ext ORDER BY id",
    ));
    input.extend(build_parse_bytes("s_fetch", "FETCH ALL IN c", &[]));
    input.extend(build_bind_bytes("p_fetch", "s_fetch", &[], &[], &[]));
    input.extend(build_execute_bytes("p_fetch", 0));
    input.extend(build_sync_bytes());
    input.extend(build_query_bytes("COMMIT"));
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run().await.expect("extended FETCH should complete");

    let messages = backend_messages(conn.writer_ref());
    let data_rows: Vec<Vec<Option<Vec<u8>>>> = messages
        .iter()
        .filter(|(tag, _)| *tag == b'D')
        .map(|(_, payload)| parse_data_row_columns(payload))
        .collect();
    assert_eq!(
        data_rows,
        vec![vec![Some(b"1".to_vec())], vec![Some(b"2".to_vec())]]
    );
    let commands: Vec<String> = messages
        .iter()
        .filter(|(tag, _)| *tag == b'C')
        .map(|(_, payload)| parse_cstring_payload(payload).to_owned())
        .collect();
    assert!(
        commands.iter().any(|command| command == "FETCH 2"),
        "expected FETCH 2 command complete, got {commands:?}"
    );
}

#[tokio::test]
async fn simple_query_current_of_updates_positioned_row() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));
    let (session, _) = engine
        .startup(StartupParams {
            database: "default".to_owned(),
            application_name: Some("pgwire-simple-current-of-seed".to_owned()),
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
            "CREATE TABLE t_simple_current_of (id INT);
             INSERT INTO t_simple_current_of VALUES (1), (2)",
        )
        .expect("seed current of table");

    let mut input = build_startup_bytes();
    input.extend(build_query_bytes("BEGIN"));
    input.extend(build_query_bytes(
        "DECLARE c CURSOR FOR SELECT id FROM t_simple_current_of ORDER BY id",
    ));
    input.extend(build_query_bytes("FETCH NEXT IN c"));
    input.extend(build_query_bytes(
        "UPDATE t_simple_current_of SET id = 20 WHERE CURRENT OF c",
    ));
    input.extend(build_query_bytes(
        "SELECT id FROM t_simple_current_of ORDER BY id",
    ));
    input.extend(build_query_bytes("COMMIT"));
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("simple query CURRENT OF should complete");

    let messages = backend_messages(conn.writer_ref());
    let commands: Vec<String> = messages
        .iter()
        .filter(|(tag, _)| *tag == b'C')
        .map(|(_, payload)| parse_cstring_payload(payload).to_owned())
        .collect();
    assert!(
        commands.iter().any(|command| command == "FETCH 1"),
        "expected FETCH 1 command complete, got {commands:?}"
    );
    assert!(
        commands.iter().any(|command| command == "UPDATE 1"),
        "expected UPDATE 1 command complete, got {commands:?}"
    );
    assert!(
        commands.iter().any(|command| command == "SELECT 2"),
        "expected SELECT 2 command complete, got {commands:?}"
    );

    let data_rows: Vec<Vec<Option<Vec<u8>>>> = messages
        .iter()
        .filter(|(tag, _)| *tag == b'D')
        .map(|(_, payload)| parse_data_row_columns(payload))
        .collect();
    assert_eq!(
        data_rows,
        vec![
            vec![Some(b"1".to_vec())],
            vec![Some(b"2".to_vec())],
            vec![Some(b"20".to_vec())],
        ]
    );
}

#[tokio::test]
async fn extended_query_current_of_updates_positioned_row() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));
    let (session, _) = engine
        .startup(StartupParams {
            database: "default".to_owned(),
            application_name: Some("pgwire-extended-current-of-seed".to_owned()),
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
            "CREATE TABLE t_extended_current_of (id INT);
             INSERT INTO t_extended_current_of VALUES (1), (2)",
        )
        .expect("seed current of table");

    let mut input = build_startup_bytes();
    input.extend(build_query_bytes("BEGIN"));
    input.extend(build_query_bytes(
        "DECLARE c CURSOR FOR SELECT id FROM t_extended_current_of ORDER BY id",
    ));
    input.extend(build_parse_bytes("s_fetch_cur", "FETCH NEXT IN c", &[]));
    input.extend(build_bind_bytes(
        "p_fetch_cur",
        "s_fetch_cur",
        &[],
        &[],
        &[],
    ));
    input.extend(build_execute_bytes("p_fetch_cur", 0));
    input.extend(build_sync_bytes());
    input.extend(build_parse_bytes(
        "s_update_cur",
        "UPDATE t_extended_current_of SET id = $1 WHERE CURRENT OF c",
        &[23],
    ));
    input.extend(build_bind_bytes(
        "p_update_cur",
        "s_update_cur",
        &[0],
        &[Some(b"20")],
        &[],
    ));
    input.extend(build_execute_bytes("p_update_cur", 0));
    input.extend(build_sync_bytes());
    input.extend(build_query_bytes(
        "SELECT id FROM t_extended_current_of ORDER BY id",
    ));
    input.extend(build_query_bytes("COMMIT"));
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("extended query CURRENT OF should complete");

    let messages = backend_messages(conn.writer_ref());
    let commands: Vec<String> = messages
        .iter()
        .filter(|(tag, _)| *tag == b'C')
        .map(|(_, payload)| parse_cstring_payload(payload).to_owned())
        .collect();
    assert!(
        commands.iter().any(|command| command == "FETCH 1"),
        "expected FETCH 1 command complete, got {commands:?}"
    );
    assert!(
        commands.iter().any(|command| command == "UPDATE 1"),
        "expected UPDATE 1 command complete, got {commands:?}"
    );
    assert!(
        commands.iter().any(|command| command == "SELECT 2"),
        "expected SELECT 2 command complete, got {commands:?}"
    );

    let data_rows: Vec<Vec<Option<Vec<u8>>>> = messages
        .iter()
        .filter(|(tag, _)| *tag == b'D')
        .map(|(_, payload)| parse_data_row_columns(payload))
        .collect();
    assert_eq!(
        data_rows,
        vec![
            vec![Some(b"1".to_vec())],
            vec![Some(b"2".to_vec())],
            vec![Some(b"20".to_vec())],
        ]
    );
}

#[tokio::test]
async fn extended_query_explain_current_of_restores_cursor_name_in_plan_text() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));
    let (session, _) = engine
        .startup(StartupParams {
            database: "default".to_owned(),
            application_name: Some("pgwire-extended-explain-current-of-seed".to_owned()),
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
            "CREATE TABLE t_extended_explain_current_of (id INT);
             INSERT INTO t_extended_explain_current_of VALUES (1), (2)",
        )
        .expect("seed current of table");

    let mut input = build_startup_bytes();
    input.extend(build_query_bytes("BEGIN"));
    input.extend(build_query_bytes(
        "DECLARE c CURSOR FOR SELECT id FROM t_extended_explain_current_of ORDER BY id",
    ));
    input.extend(build_query_bytes("FETCH NEXT IN c"));
    input.extend(build_parse_bytes(
        "s_explain_cur",
        "EXPLAIN UPDATE t_extended_explain_current_of SET id = 20 WHERE CURRENT OF c",
        &[],
    ));
    input.extend(build_bind_bytes(
        "p_explain_cur",
        "s_explain_cur",
        &[],
        &[],
        &[],
    ));
    input.extend(build_execute_bytes("p_explain_cur", 0));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("extended query EXPLAIN CURRENT OF should complete");

    let messages = backend_messages(conn.writer_ref());
    let commands: Vec<String> = messages
        .iter()
        .filter(|(tag, _)| *tag == b'C')
        .map(|(_, payload)| parse_cstring_payload(payload).to_owned())
        .collect();
    assert!(
        commands.iter().any(|command| command == "EXPLAIN"),
        "expected EXPLAIN command complete, got {commands:?}"
    );

    let explain_rows: Vec<String> = messages
        .iter()
        .filter(|(tag, _)| *tag == b'D')
        .flat_map(|(_, payload)| parse_data_row_columns(payload))
        .flatten()
        .map(|bytes| String::from_utf8(bytes).expect("utf8 explain row"))
        .collect();
    assert!(
        explain_rows
            .iter()
            .any(|line| line.contains("CURRENT OF c")),
        "expected plan rows to mention CURRENT OF c, got {explain_rows:?}"
    );
}

#[tokio::test]
async fn extended_execute_sql_execute_returns_query_rows() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_query_bytes("PREPARE stmt(int) AS SELECT $1 + 1 AS v"));
    input.extend(build_parse_bytes("s_exec_stmt", "EXECUTE stmt(41)", &[]));
    input.extend(build_bind_bytes(
        "p_exec_stmt",
        "s_exec_stmt",
        &[],
        &[],
        &[],
    ));
    input.extend(build_describe_bytes(b'P', "p_exec_stmt"));
    input.extend(build_execute_bytes("p_exec_stmt", 0));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("extended EXECUTE stmt should complete");

    let messages = backend_messages(conn.writer_ref());
    let row_description = messages.iter().find(|(tag, _)| *tag == b'T').map_or_else(
        || {
            let tags: Vec<char> = messages.iter().map(|(tag, _)| char::from(*tag)).collect();
            let error = messages
                .iter()
                .rfind(|(tag, _)| *tag == b'E')
                .and_then(|(_, payload)| parse_error_response_message(payload));
            panic!("row description missing; tags={tags:?}; error={error:?}");
        },
        |(_, payload)| payload.as_slice(),
    );
    assert_eq!(
        parse_row_description_field_names(row_description),
        vec!["v"]
    );
    assert_eq!(
        parse_row_description_type_info(row_description),
        vec![(23, -1)]
    );
    assert_eq!(
        parse_row_description_origin_info(row_description),
        vec![(0, 0)]
    );

    let data_rows: Vec<Vec<Option<Vec<u8>>>> = messages
        .iter()
        .filter(|(tag, _)| *tag == b'D')
        .map(|(_, payload)| parse_data_row_columns(payload))
        .collect();
    assert_eq!(data_rows, vec![vec![Some(b"42".to_vec())]]);

    let command = messages
        .iter()
        .rfind(|(tag, _)| *tag == b'C')
        .map(|(_, payload)| parse_cstring_payload(payload))
        .expect("command complete");
    assert_eq!(command, "SELECT 1");
}

#[tokio::test]
async fn extended_execute_ctas_as_execute_reports_create_table_and_materializes_rows() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_query_bytes("PREPARE stmt(int) AS SELECT $1 + 1 AS v"));
    input.extend(build_parse_bytes(
        "s_ctas_exec",
        "CREATE TABLE ctas_exec_portal_ext AS EXECUTE stmt(NULL)",
        &[],
    ));
    input.extend(build_bind_bytes(
        "p_ctas_exec",
        "s_ctas_exec",
        &[],
        &[],
        &[],
    ));
    input.extend(build_execute_bytes("p_ctas_exec", 0));
    input.extend(build_sync_bytes());
    input.extend(build_query_bytes("SELECT v FROM ctas_exec_portal_ext"));
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("extended CTAS AS EXECUTE should complete");

    let messages = backend_messages(conn.writer_ref());
    let commands: Vec<String> = messages
        .iter()
        .filter(|(tag, _)| *tag == b'C')
        .map(|(_, payload)| parse_cstring_payload(payload).to_owned())
        .collect();
    assert!(
        commands.iter().any(|command| command == "PREPARE"),
        "expected PREPARE command complete, got {commands:?}"
    );
    assert!(
        commands.iter().any(|command| command == "CREATE TABLE"),
        "expected CREATE TABLE command complete, got {commands:?}"
    );
    assert!(
        commands.iter().any(|command| command == "SELECT 1"),
        "expected SELECT 1 command complete, got {commands:?}"
    );

    let data_rows: Vec<Vec<Option<Vec<u8>>>> = messages
        .iter()
        .filter(|(tag, _)| *tag == b'D')
        .map(|(_, payload)| parse_data_row_columns(payload))
        .collect();
    assert!(
        data_rows
            .iter()
            .any(|row| row == &vec![Some(b"".to_vec())] || row == &vec![None]),
        "expected materialized NULL row, got {data_rows:?}"
    );
}

