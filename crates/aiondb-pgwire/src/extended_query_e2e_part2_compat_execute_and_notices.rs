use super::*;

#[tokio::test]
async fn extended_execute_do_block_emits_notice_response_and_do_complete() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_do_stmt",
        "DO $$ BEGIN RAISE NOTICE '%', 'hello from extended do'; END $$ LANGUAGE plpgsql",
        &[],
    ));
    input.extend(build_bind_bytes("p_do_stmt", "s_do_stmt", &[], &[], &[]));
    input.extend(build_execute_bytes("p_do_stmt", 0));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run().await.expect("extended DO block should complete");

    let messages = backend_messages(conn.writer_ref());
    let tags: Vec<u8> = messages.iter().map(|(tag, _)| *tag).collect();
    assert!(
        tags.contains(&b'N'),
        "expected NoticeResponse, got {tags:?}"
    );
    assert!(
        !tags.contains(&b'T'),
        "DO block should not emit RowDescription"
    );

    let notice_payload = messages
        .iter()
        .find(|(tag, _)| *tag == b'N')
        .map(|(_, payload)| payload.as_slice())
        .expect("notice response");
    assert!(
        String::from_utf8_lossy(notice_payload).contains("hello from extended do"),
        "expected DO notice payload, got {:?}",
        String::from_utf8_lossy(notice_payload)
    );

    let commands: Vec<String> = messages
        .iter()
        .filter(|(tag, _)| *tag == b'C')
        .map(|(_, payload)| parse_cstring_payload(payload).to_owned())
        .collect();
    assert!(
        commands.iter().any(|command| command == "DO"),
        "expected DO command complete, got {commands:?}"
    );
}

#[tokio::test]
async fn extended_execute_sql_prepare_then_execute_returns_query_rows() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_prepare_stmt",
        "PREPARE stmt(int) AS SELECT $1 + 1 AS v",
        &[],
    ));
    input.extend(build_bind_bytes(
        "p_prepare_stmt",
        "s_prepare_stmt",
        &[],
        &[],
        &[],
    ));
    input.extend(build_execute_bytes("p_prepare_stmt", 0));
    input.extend(build_sync_bytes());
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
        .expect("extended PREPARE then EXECUTE stmt should complete");

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
        commands.iter().any(|command| command == "SELECT 1"),
        "expected SELECT 1 command complete, got {commands:?}"
    );

    let row_description = messages
        .iter()
        .find(|(tag, _)| *tag == b'T')
        .map(|(_, payload)| payload.as_slice())
        .expect("row description");
    assert_eq!(
        parse_row_description_field_names(row_description),
        vec!["v"]
    );
    assert_eq!(
        parse_row_description_type_info(row_description),
        vec![(23, -1)]
    );

    let data_rows: Vec<Vec<Option<Vec<u8>>>> = messages
        .iter()
        .filter(|(tag, _)| *tag == b'D')
        .map(|(_, payload)| parse_data_row_columns(payload))
        .collect();
    assert_eq!(data_rows, vec![vec![Some(b"42".to_vec())]]);
}

#[tokio::test]
async fn extended_execute_sql_execute_prepared_do_emits_notice_response_and_do_complete() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_query_bytes(
        "PREPARE stmt AS DO $$ BEGIN RAISE NOTICE '%', 'hello from prepared extended do'; END $$ LANGUAGE plpgsql",
    ));
    input.extend(build_parse_bytes("s_exec_stmt", "EXECUTE stmt", &[]));
    input.extend(build_bind_bytes(
        "p_exec_stmt",
        "s_exec_stmt",
        &[],
        &[],
        &[],
    ));
    input.extend(build_execute_bytes("p_exec_stmt", 0));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("extended prepared DO execute should complete");

    let messages = backend_messages(conn.writer_ref());
    let tags: Vec<u8> = messages.iter().map(|(tag, _)| *tag).collect();
    assert!(
        tags.contains(&b'N'),
        "expected NoticeResponse, got {tags:?}"
    );

    let notice_payload = messages
        .iter()
        .find(|(tag, _)| *tag == b'N')
        .map(|(_, payload)| payload.as_slice())
        .expect("notice response");
    assert!(
        String::from_utf8_lossy(notice_payload).contains("hello from prepared extended do"),
        "expected prepared DO notice payload, got {:?}",
        String::from_utf8_lossy(notice_payload)
    );

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
        commands.iter().any(|command| command == "DO"),
        "expected DO command complete, got {commands:?}"
    );
}

#[tokio::test]
async fn simple_query_do_then_select_emits_notice_do_and_select_results() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_query_bytes(
        "DO $$ BEGIN RAISE NOTICE '%', 'hello from simple query do'; END $$ LANGUAGE plpgsql; SELECT 1",
    ));
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("simple query DO + SELECT batch should complete");

    let messages = backend_messages(conn.writer_ref());
    let tags: Vec<u8> = messages.iter().map(|(tag, _)| *tag).collect();
    assert!(
        tags.contains(&b'N'),
        "expected NoticeResponse, got {tags:?}"
    );
    assert!(
        tags.contains(&b'T'),
        "expected RowDescription, got {tags:?}"
    );
    assert!(tags.contains(&b'D'), "expected DataRow, got {tags:?}");

    let notice_payload = messages
        .iter()
        .find(|(tag, _)| *tag == b'N')
        .map(|(_, payload)| payload.as_slice())
        .expect("notice response");
    assert!(
        String::from_utf8_lossy(notice_payload).contains("hello from simple query do"),
        "expected simple-query DO notice payload, got {:?}",
        String::from_utf8_lossy(notice_payload)
    );

    let commands: Vec<String> = messages
        .iter()
        .filter(|(tag, _)| *tag == b'C')
        .map(|(_, payload)| parse_cstring_payload(payload).to_owned())
        .collect();
    assert!(
        commands.iter().any(|command| command == "DO"),
        "expected DO command complete, got {commands:?}"
    );
    assert!(
        commands.iter().any(|command| command == "SELECT 1"),
        "expected SELECT 1 command complete, got {commands:?}"
    );

    let data_row = messages
        .iter()
        .find(|(tag, _)| *tag == b'D')
        .map(|(_, payload)| payload.as_slice())
        .expect("data row");
    assert_eq!(parse_data_row_columns(data_row), vec![Some(b"1".to_vec())]);
}

#[tokio::test]
async fn simple_query_do_then_explain_preserves_explain_command_tag() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_query_bytes(
        "DO $$ BEGIN RAISE NOTICE '%', 'hello from simple query explain'; END $$ LANGUAGE plpgsql; EXPLAIN SELECT 1",
    ));
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("simple query DO + EXPLAIN batch should complete");

    let messages = backend_messages(conn.writer_ref());
    let notice_payload = messages
        .iter()
        .find(|(tag, _)| *tag == b'N')
        .map(|(_, payload)| payload.as_slice())
        .expect("notice response");
    assert!(
        String::from_utf8_lossy(notice_payload).contains("hello from simple query explain"),
        "expected simple-query DO notice payload, got {:?}",
        String::from_utf8_lossy(notice_payload)
    );

    let commands: Vec<String> = messages
        .iter()
        .filter(|(tag, _)| *tag == b'C')
        .map(|(_, payload)| parse_cstring_payload(payload).to_owned())
        .collect();
    assert!(
        commands
            .windows(2)
            .any(|window| window == ["DO", "EXPLAIN"]),
        "expected DO then EXPLAIN command completes, got {commands:?}"
    );
}

#[tokio::test]
async fn simple_query_begin_twice_emits_notice_and_two_begin_tags() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_query_bytes("BEGIN; BEGIN"));
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("simple query BEGIN; BEGIN should complete");

    let messages = backend_messages(conn.writer_ref());
    let notice_payload = messages
        .iter()
        .find(|(tag, _)| *tag == b'N')
        .map(|(_, payload)| payload.as_slice())
        .expect("notice response");
    assert!(
        String::from_utf8_lossy(notice_payload)
            .contains("there is already a transaction in progress"),
        "expected duplicate BEGIN notice, got {:?}",
        String::from_utf8_lossy(notice_payload)
    );

    let commands: Vec<String> = messages
        .iter()
        .filter(|(tag, _)| *tag == b'C')
        .map(|(_, payload)| parse_cstring_payload(payload).to_owned())
        .collect();
    assert_eq!(commands, vec!["BEGIN".to_owned(), "BEGIN".to_owned()]);
}

#[tokio::test]
async fn extended_execute_commit_outside_transaction_emits_notice_and_commit_tag() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes("s_commit", "COMMIT", &[]));
    input.extend(build_bind_bytes("p_commit", "s_commit", &[], &[], &[]));
    input.extend(build_execute_bytes("p_commit", 0));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("extended COMMIT outside transaction should complete");

    let messages = backend_messages(conn.writer_ref());
    let notice_payload = messages
        .iter()
        .find(|(tag, _)| *tag == b'N')
        .map(|(_, payload)| payload.as_slice())
        .expect("notice response");
    assert!(
        String::from_utf8_lossy(notice_payload).contains("there is no transaction in progress"),
        "expected no-transaction notice, got {:?}",
        String::from_utf8_lossy(notice_payload)
    );

    let command = messages
        .iter()
        .rfind(|(tag, _)| *tag == b'C')
        .map(|(_, payload)| parse_cstring_payload(payload))
        .expect("command complete");
    assert_eq!(command, "COMMIT");
}

#[tokio::test]
async fn extended_execute_begin_twice_emits_notice_and_two_begin_tags() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes("s_begin", "BEGIN", &[]));
    input.extend(build_bind_bytes("p_begin_1", "s_begin", &[], &[], &[]));
    input.extend(build_execute_bytes("p_begin_1", 0));
    input.extend(build_sync_bytes());
    input.extend(build_bind_bytes("p_begin_2", "s_begin", &[], &[], &[]));
    input.extend(build_execute_bytes("p_begin_2", 0));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("extended BEGIN twice should complete");

    let messages = backend_messages(conn.writer_ref());
    let notice_payload = messages
        .iter()
        .find(|(tag, _)| *tag == b'N')
        .map(|(_, payload)| payload.as_slice())
        .expect("notice response");
    assert!(
        String::from_utf8_lossy(notice_payload)
            .contains("there is already a transaction in progress"),
        "expected duplicate BEGIN notice, got {:?}",
        String::from_utf8_lossy(notice_payload)
    );
    let commands: Vec<String> = messages
        .iter()
        .filter(|(tag, _)| *tag == b'C')
        .map(|(_, payload)| parse_cstring_payload(payload).to_owned())
        .collect();
    assert_eq!(commands, vec!["BEGIN".to_owned(), "BEGIN".to_owned()]);
}

#[tokio::test]
async fn simple_query_rollback_outside_transaction_emits_notice_and_rollback_tag() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_query_bytes("ROLLBACK"));
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("simple query ROLLBACK outside transaction should complete");

    let messages = backend_messages(conn.writer_ref());
    let notice_payload = messages
        .iter()
        .find(|(tag, _)| *tag == b'N')
        .map(|(_, payload)| payload.as_slice())
        .expect("notice response");
    assert!(
        String::from_utf8_lossy(notice_payload).contains("there is no transaction in progress"),
        "expected no-transaction notice, got {:?}",
        String::from_utf8_lossy(notice_payload)
    );
    let command = messages
        .iter()
        .rfind(|(tag, _)| *tag == b'C')
        .map(|(_, payload)| parse_cstring_payload(payload))
        .expect("command complete");
    assert_eq!(command, "ROLLBACK");
}

#[tokio::test]
async fn extended_do_block_with_declared_null_variable_emits_notice_and_do_complete() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_bad_do",
        "DO $$ DECLARE x text; BEGIN RAISE NOTICE '%', x; END $$ LANGUAGE plpgsql",
        &[],
    ));
    input.extend(build_bind_bytes("p_bad_do", "s_bad_do", &[], &[], &[]));
    input.extend(build_execute_bytes("p_bad_do", 0));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run().await.expect("DO block should complete");

    let messages = backend_messages(conn.writer_ref());
    let notice_payload = messages
        .iter()
        .find(|(tag, _)| *tag == b'N')
        .map(|(_, payload)| payload.as_slice())
        .expect("expected NoticeResponse for DO");
    assert!(
        String::from_utf8_lossy(notice_payload).contains("<NULL>"),
        "expected NULL notice, got {:?}",
        String::from_utf8_lossy(notice_payload)
    );
    let commands: Vec<String> = messages
        .iter()
        .filter(|(tag, _)| *tag == b'C')
        .map(|(_, payload)| parse_cstring_payload(payload).to_owned())
        .collect();
    assert!(
        commands.iter().any(|command| command == "DO"),
        "DO should emit command complete, got {commands:?}"
    );
}

#[tokio::test]
async fn extended_execute_create_type_tracks_compat_type_for_following_function() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_create_type",
        "CREATE TYPE shell_alias",
        &[],
    ));
    input.extend(build_bind_bytes(
        "p_create_type",
        "s_create_type",
        &[],
        &[],
        &[],
    ));
    input.extend(build_execute_bytes("p_create_type", 0));
    input.extend(build_sync_bytes());
    input.extend(build_query_bytes(
        "CREATE FUNCTION shell_alias_out(shell_alias) RETURNS cstring \
         STRICT IMMUTABLE LANGUAGE internal AS 'int8out'",
    ));
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("extended CREATE TYPE and following CREATE FUNCTION should complete");

    let messages = backend_messages(conn.writer_ref());
    let tags: Vec<u8> = messages.iter().map(|(tag, _)| *tag).collect();
    let commands: Vec<String> = messages
        .iter()
        .filter(|(tag, _)| *tag == b'C')
        .map(|(_, payload)| parse_cstring_payload(payload).to_owned())
        .collect();
    assert!(
        commands.iter().any(|command| command == "CREATE TYPE"),
        "expected CREATE TYPE command complete, got {commands:?}"
    );
    assert!(
        commands.iter().any(|command| command == "CREATE FUNCTION"),
        "expected CREATE FUNCTION command complete, got {commands:?}"
    );
    assert!(
        tags.contains(&b'N'),
        "expected NoticeResponse, got {tags:?}"
    );

    let notice_payload = messages
        .iter()
        .find(|(tag, _)| *tag == b'N')
        .map(|(_, payload)| payload.as_slice())
        .expect("notice response");
    assert!(
        String::from_utf8_lossy(notice_payload)
            .contains("argument type shell_alias is only a shell"),
        "expected shell type notice payload, got {:?}",
        String::from_utf8_lossy(notice_payload)
    );
}

#[tokio::test]
async fn extended_execute_sql_execute_prepared_create_type_tracks_following_function() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_query_bytes("PREPARE stmt AS CREATE TYPE shell_alias"));
    input.extend(build_parse_bytes("s_exec_stmt", "EXECUTE stmt", &[]));
    input.extend(build_bind_bytes(
        "p_exec_stmt",
        "s_exec_stmt",
        &[],
        &[],
        &[],
    ));
    input.extend(build_execute_bytes("p_exec_stmt", 0));
    input.extend(build_sync_bytes());
    input.extend(build_query_bytes(
        "CREATE FUNCTION shell_alias_out(shell_alias) RETURNS cstring \
         STRICT IMMUTABLE LANGUAGE internal AS 'int8out'",
    ));
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("extended EXECUTE stmt for CREATE TYPE should complete");

    let messages = backend_messages(conn.writer_ref());
    let tags: Vec<u8> = messages.iter().map(|(tag, _)| *tag).collect();
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
        commands.iter().any(|command| command == "CREATE TYPE"),
        "expected CREATE TYPE command complete, got {commands:?}"
    );
    assert!(
        commands.iter().any(|command| command == "CREATE FUNCTION"),
        "expected CREATE FUNCTION command complete, got {commands:?}"
    );
    assert!(
        tags.contains(&b'N'),
        "expected NoticeResponse, got {tags:?}"
    );

    let notice_payload = messages
        .iter()
        .find(|(tag, _)| *tag == b'N')
        .map(|(_, payload)| payload.as_slice())
        .expect("notice response");
    assert!(
        String::from_utf8_lossy(notice_payload)
            .contains("argument type shell_alias is only a shell"),
        "expected shell type notice payload, got {:?}",
        String::from_utf8_lossy(notice_payload)
    );
}

#[tokio::test]
async fn extended_execute_sql_execute_prepared_cursor_commands() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_query_bytes(
        "CREATE TABLE compat_exec_cursor_ext (id INT);
         INSERT INTO compat_exec_cursor_ext VALUES (1), (2)",
    ));
    input.extend(build_query_bytes("BEGIN"));
    input.extend(build_query_bytes(
        "PREPARE decl AS DECLARE c CURSOR FOR SELECT id FROM compat_exec_cursor_ext ORDER BY id",
    ));
    input.extend(build_parse_bytes("s_exec_decl", "EXECUTE decl", &[]));
    input.extend(build_bind_bytes(
        "p_exec_decl",
        "s_exec_decl",
        &[],
        &[],
        &[],
    ));
    input.extend(build_execute_bytes("p_exec_decl", 0));
    input.extend(build_sync_bytes());
    input.extend(build_query_bytes("PREPARE fetch_stmt AS FETCH ALL IN c"));
    input.extend(build_parse_bytes("s_exec_fetch", "EXECUTE fetch_stmt", &[]));
    input.extend(build_bind_bytes(
        "p_exec_fetch",
        "s_exec_fetch",
        &[],
        &[],
        &[],
    ));
    input.extend(build_describe_bytes(b'P', "p_exec_fetch"));
    input.extend(build_execute_bytes("p_exec_fetch", 0));
    input.extend(build_sync_bytes());
    input.extend(build_query_bytes("PREPARE close_stmt AS CLOSE c"));
    input.extend(build_parse_bytes("s_exec_close", "EXECUTE close_stmt", &[]));
    input.extend(build_bind_bytes(
        "p_exec_close",
        "s_exec_close",
        &[],
        &[],
        &[],
    ));
    input.extend(build_execute_bytes("p_exec_close", 0));
    input.extend(build_sync_bytes());
    input.extend(build_query_bytes("COMMIT"));
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("extended prepared cursor commands should complete");

    let messages = backend_messages(conn.writer_ref());
    let commands: Vec<String> = messages
        .iter()
        .filter(|(tag, _)| *tag == b'C')
        .map(|(_, payload)| parse_cstring_payload(payload).to_owned())
        .collect();
    assert!(
        commands.iter().any(|command| command == "BEGIN"),
        "expected BEGIN command complete, got {commands:?}"
    );
    assert!(
        commands.iter().any(|command| command == "DECLARE CURSOR"),
        "expected DECLARE CURSOR command complete, got {commands:?}"
    );
    assert!(
        commands.iter().any(|command| command == "FETCH 2"),
        "expected FETCH 2 command complete, got {commands:?}"
    );
    assert!(
        commands.iter().any(|command| command == "CLOSE CURSOR"),
        "expected CLOSE CURSOR command complete, got {commands:?}"
    );
    assert!(
        commands.iter().any(|command| command == "COMMIT"),
        "expected COMMIT command complete, got {commands:?}"
    );

    let row_description = messages
        .iter()
        .find(|(tag, _)| *tag == b'T')
        .map(|(_, payload)| payload.as_slice())
        .expect("row description");
    assert_eq!(
        parse_row_description_field_names(row_description),
        vec!["id"]
    );

    let data_rows: Vec<Vec<Option<Vec<u8>>>> = messages
        .iter()
        .filter(|(tag, _)| *tag == b'D')
        .map(|(_, payload)| parse_data_row_columns(payload))
        .collect();
    assert_eq!(
        data_rows,
        vec![vec![Some(b"1".to_vec())], vec![Some(b"2".to_vec())]]
    );
}

#[tokio::test]
async fn extended_execute_sql_execute_emits_post_statement_compat_notices() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_query_bytes(
        "CREATE TYPE casttesttype;
         CREATE CAST (text AS casttesttype) WITHOUT FUNCTION;
         CREATE FUNCTION int4_casttesttype(int4) RETURNS casttesttype LANGUAGE SQL AS
         $$ SELECT ('foo'::text || $1::text)::casttesttype; $$;
         CREATE CAST (int4 AS casttesttype) WITH FUNCTION int4_casttesttype(int4) AS IMPLICIT;",
    ));
    input.extend(build_query_bytes(
        "PREPARE stmt AS DROP FUNCTION int4_casttesttype(int4) CASCADE",
    ));
    input.extend(build_parse_bytes("s_exec_stmt", "EXECUTE stmt", &[]));
    input.extend(build_bind_bytes(
        "p_exec_stmt",
        "s_exec_stmt",
        &[],
        &[],
        &[],
    ));
    input.extend(build_execute_bytes("p_exec_stmt", 0));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("extended EXECUTE stmt should emit compat notices");

    let messages = backend_messages(conn.writer_ref());
    let tags: Vec<u8> = messages.iter().map(|(tag, _)| *tag).collect();
    assert!(
        tags.contains(&b'N'),
        "expected NoticeResponse, got {tags:?}"
    );

    let notice_payload = messages
        .iter()
        .filter(|(tag, _)| *tag == b'N')
        .map(|(_, payload)| payload.as_slice())
        .find(|payload| {
            String::from_utf8_lossy(payload)
                .contains("drop cascades to cast from integer to casttesttype")
        })
        .expect("cascade notice response");
    assert!(
        String::from_utf8_lossy(notice_payload)
            .contains("drop cascades to cast from integer to casttesttype"),
        "expected cascade notice payload, got {:?}",
        String::from_utf8_lossy(notice_payload)
    );

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
        commands.iter().any(|command| command == "DROP FUNCTION"),
        "expected DROP FUNCTION command complete, got {commands:?}"
    );
}

#[tokio::test]
async fn extended_execute_rule_create_and_insert_rewrites_to_base_table() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_query_bytes(
        "CREATE TABLE base_table (id INT);
         CREATE VIEW base_view AS SELECT id FROM base_table",
    ));
    input.extend(build_parse_bytes(
        "s_create_rule",
        "CREATE RULE base_view_insert AS ON INSERT TO base_view DO INSTEAD INSERT INTO base_table VALUES (new.id)",
        &[],
    ));
    input.extend(build_bind_bytes(
        "p_create_rule",
        "s_create_rule",
        &[],
        &[],
        &[],
    ));
    input.extend(build_execute_bytes("p_create_rule", 0));
    input.extend(build_sync_bytes());
    input.extend(build_parse_bytes(
        "s_insert_view",
        "INSERT INTO base_view VALUES (7)",
        &[],
    ));
    input.extend(build_bind_bytes(
        "p_insert_view",
        "s_insert_view",
        &[],
        &[],
        &[],
    ));
    input.extend(build_execute_bytes("p_insert_view", 0));
    input.extend(build_sync_bytes());
    input.extend(build_query_bytes("SELECT id FROM base_table ORDER BY id"));
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("extended compat rule flow should complete");

    let messages = backend_messages(conn.writer_ref());
    let commands: Vec<String> = messages
        .iter()
        .filter(|(tag, _)| *tag == b'C')
        .map(|(_, payload)| parse_cstring_payload(payload).to_owned())
        .collect();
    assert!(
        commands.iter().any(|command| command == "CREATE RULE"),
        "expected CREATE RULE command complete, got {commands:?}"
    );
    assert!(
        commands.iter().any(|command| command == "INSERT 0 1"),
        "expected INSERT 0 1 command complete, got {commands:?}"
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
    assert_eq!(data_rows, vec![vec![Some(b"7".to_vec())]]);
}
