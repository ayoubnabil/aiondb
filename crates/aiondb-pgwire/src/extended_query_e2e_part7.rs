#[path = "extended_query_e2e_part7_describe_metadata.rs"]
mod describe_metadata;

use base64::Engine as _;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use tokio::io::{duplex, split, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

struct LiveBackendMessage {
    tag: u8,
    payload: bytes::BytesMut,
}

async fn read_live_backend_message<S>(stream: &mut S) -> DbResult<LiveBackendMessage>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut tag = [0u8; 1];
    stream
        .read_exact(&mut tag)
        .await
        .map_err(|error| DbError::protocol(format!("read backend tag: {error}")))?;

    let mut len = [0u8; 4];
    stream
        .read_exact(&mut len)
        .await
        .map_err(|error| DbError::protocol(format!("read backend length: {error}")))?;
    let payload_len = u32::from_be_bytes(len) as usize - 4;
    let mut payload = bytes::BytesMut::zeroed(payload_len);
    if payload_len > 0 {
        stream
            .read_exact(&mut payload)
            .await
            .map_err(|error| DbError::protocol(format!("read backend payload: {error}")))?;
    }
    Ok(LiveBackendMessage {
        tag: tag[0],
        payload,
    })
}

fn parse_auth_request(mut payload: bytes::BytesMut) -> DbResult<i32> {
    codec::read_i32_from_buf(&mut payload)
}

fn parse_auth_sasl_continue(mut payload: bytes::BytesMut) -> DbResult<String> {
    let auth_type = codec::read_i32_from_buf(&mut payload)?;
    if auth_type != 11 {
        return Err(DbError::protocol(format!(
            "expected AuthenticationSASLContinue, got auth type {auth_type}"
        )));
    }
    String::from_utf8(payload.to_vec())
        .map_err(|_| DbError::protocol("invalid UTF-8 in SASL continue payload"))
}

fn parse_auth_sasl_final(mut payload: bytes::BytesMut) -> DbResult<String> {
    let auth_type = codec::read_i32_from_buf(&mut payload)?;
    if auth_type != 12 {
        return Err(DbError::protocol(format!(
            "expected AuthenticationSASLFinal, got auth type {auth_type}"
        )));
    }
    String::from_utf8(payload.to_vec())
        .map_err(|_| DbError::protocol("invalid UTF-8 in SASL final payload"))
}

fn build_sasl_initial_response_bytes(mechanism: &str, initial_response: &str) -> Vec<u8> {
    let mut payload = Vec::new();
    payload.extend_from_slice(mechanism.as_bytes());
    payload.push(0);
    payload.extend_from_slice(&(initial_response.len() as i32).to_be_bytes());
    payload.extend_from_slice(initial_response.as_bytes());
    build_raw_message(b'p', &payload)
}

fn build_sasl_response_bytes(response: &str) -> Vec<u8> {
    build_raw_message(b'p', response.as_bytes())
}

fn build_scram_client_final(password: &str, client_first: &str, server_first: &str) -> String {
    let combined_nonce = server_first
        .split(',')
        .find_map(|part| part.strip_prefix("r="))
        .expect("server-first nonce");
    let salt_b64 = server_first
        .split(',')
        .find_map(|part| part.strip_prefix("s="))
        .expect("server-first salt");
    let iterations = server_first
        .split(',')
        .find_map(|part| part.strip_prefix("i="))
        .expect("server-first iterations")
        .parse::<u32>()
        .expect("iterations parse");

    let salt = base64::engine::general_purpose::STANDARD
        .decode(salt_b64)
        .expect("salt decode");
    let mut salted_password = [0u8; 32];
    pbkdf2::pbkdf2_hmac::<Sha256>(password.as_bytes(), &salt, iterations, &mut salted_password);

    let mut client_key = hmac_sha256(&salted_password, b"Client Key");
    let stored_key = sha256(&client_key);
    let client_final_without_proof = format!("c=biws,r={combined_nonce}");
    let auth_message = format!(
        "{},{},{}",
        client_first
            .strip_prefix("n,,")
            .expect("client-first prefix"),
        server_first,
        client_final_without_proof
    );
    let client_signature = hmac_sha256(&stored_key, auth_message.as_bytes());
    for (key_byte, signature_byte) in client_key.iter_mut().zip(client_signature) {
        *key_byte ^= signature_byte;
    }
    let proof_b64 = base64::engine::general_purpose::STANDARD.encode(client_key);
    format!("{client_final_without_proof},p={proof_b64}")
}

fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    type HmacSha256 = Hmac<Sha256>;

    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(data);
    mac.finalize().into_bytes().into()
}

fn sha256(data: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hasher.finalize().into()
}

#[tokio::test]
async fn simple_query_copy_in_drains_pending_notices_after_copy_in_data() {
    let mut input = build_startup_bytes();
    input.extend(build_query_bytes("COPY t FROM STDIN"));
    input.extend(build_copy_data_bytes(b"1\talice\n"));
    input.extend(build_copy_done_bytes());
    input.extend(build_terminate_bytes());

    let engine = Arc::new(CopyInPendingNoticePortalMockEngine::new());
    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("simple query copy in pending notice should succeed");

    let messages = backend_messages(conn.writer_ref());
    let tags: Vec<u8> = messages.iter().map(|(tag, _)| *tag).collect();
    assert!(tags.contains(&b'G'), "expected CopyInResponse");
    assert!(tags.contains(&b'N'), "expected NoticeResponse");
    let notice_payload = messages
        .iter()
        .find(|(tag, _)| *tag == b'N')
        .map(|(_, payload)| payload.as_slice())
        .expect("notice response");
    assert!(
        String::from_utf8_lossy(notice_payload).contains("copy pending notice"),
        "expected pending notice payload"
    );
    let last_command = messages
        .iter()
        .rfind(|(tag, _)| *tag == b'C')
        .map(|(_, payload)| parse_cstring_payload(payload))
        .expect("command complete");
    assert_eq!(last_command, "COPY 1");
}

#[tokio::test]
async fn simple_query_django_constraint_reflection_with_ordinality_works_on_durable_engine() {
    let data_dir = std::env::temp_dir().join(format!(
        "aiondb-pgwire-durable-django-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    let engine = Arc::new(
        EngineBuilder::new_durable(data_dir.clone())
            .expect("durable builder")
            .with_authorizer(Arc::new(aiondb_security::AllowAllAuthorizer))
            .with_allow_ephemeral_users(true)
            .build()
            .expect("build durable engine"),
    );
    let mut input = build_startup_bytes();
    input.extend(build_query_bytes(
        "CREATE TABLE django_wire_parent (id INT PRIMARY KEY)",
    ));
    input.extend(build_query_bytes(
        "CREATE TABLE django_wire_child ( \
             id INT PRIMARY KEY, \
             parent_id INT NOT NULL REFERENCES django_wire_parent(id), \
             slug TEXT UNIQUE \
         )",
    ));
    input.extend(build_query_bytes(
        "SELECT c.conname, \
                array( \
                    SELECT attname \
                    FROM unnest(c.conkey) WITH ORDINALITY cols(colid, arridx) \
                    JOIN pg_attribute AS ca ON cols.colid = ca.attnum \
                    WHERE ca.attrelid = c.conrelid \
                    ORDER BY cols.arridx \
                ), \
                c.contype, \
                (SELECT fkc.relname || '.' || fka.attname \
                   FROM pg_attribute AS fka \
                   JOIN pg_class AS fkc ON fka.attrelid = fkc.oid \
                  WHERE fka.attrelid = c.confrelid AND fka.attnum = c.confkey[1]), \
                cl.reloptions \
           FROM pg_constraint AS c \
           JOIN pg_class AS cl ON c.conrelid = cl.oid \
          WHERE cl.relname = 'django_wire_child' \
            AND pg_catalog.pg_table_is_visible(cl.oid)",
    ));
    input.extend(build_terminate_bytes());

    let pool_engine = Arc::clone(&engine);
    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.set_engine_pool(crate::engine_pool::EnginePool::new(
        pool_engine,
        aiondb_config::EnginePoolConfig {
            worker_threads: 8,
            queue_depth: 256,
        },
    ));
    conn.run()
        .await
        .expect("simple query django reflection should succeed");

    let messages = backend_messages(conn.writer_ref());
    let error_message = messages
        .iter()
        .find(|(tag, _)| *tag == b'E')
        .and_then(|(_, payload)| parse_error_response_message(payload));
    assert_eq!(error_message, None, "unexpected error: {error_message:?}");
    let data_rows = messages.iter().filter(|(tag, _)| *tag == b'D').count();
    assert_eq!(data_rows, 3);

    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn simple_query_django_constraint_reflection_with_ordinality_works_with_password_auth() {
    let data_dir = std::env::temp_dir().join(format!(
        "aiondb-pgwire-auth-django-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    let engine = Arc::new(
        EngineBuilder::new_durable(data_dir.clone())
            .expect("durable builder")
            .with_authorizer(Arc::new(aiondb_security::AllowAllAuthorizer))
            .build()
            .expect("build durable engine"),
    );
    engine
        .bootstrap_role("admin", "StrongPass123!", true)
        .expect("bootstrap admin role");

    let pool_engine = Arc::clone(&engine);
    let (mut client, server) = duplex(1 << 20);
    let (server_reader, server_writer) = split(server);
    let mut conn = Connection::new(
        engine,
        server_reader,
        server_writer,
        1,
        42,
        CancelRegistry::new(),
    );
    conn.set_peer_addr(Some("127.0.0.1:5432".to_owned()));
    conn.set_engine_pool(crate::engine_pool::EnginePool::new(
        pool_engine,
        aiondb_config::EnginePoolConfig {
            worker_threads: 8,
            queue_depth: 256,
        },
    ));
    let task = tokio::spawn(async move { conn.run().await });

    client
        .write_all(&build_startup_bytes_with_user("admin"))
        .await
        .expect("write startup");

    let sasl = read_live_backend_message(&mut client)
        .await
        .expect("read SASL auth request");
    assert_eq!(sasl.tag, b'R');
    assert_eq!(parse_auth_request(sasl.payload.clone()).expect("parse auth type"), 10);
    assert!(
        String::from_utf8_lossy(&sasl.payload[4..]).contains("SCRAM-SHA-256"),
        "expected SCRAM mechanism advertisement"
    );

    let client_first = "n,,n=admin,r=clientnonce";
    client
        .write_all(&build_sasl_initial_response_bytes(
            "SCRAM-SHA-256",
            client_first,
        ))
        .await
        .expect("write SASL initial response");

    let server_continue = read_live_backend_message(&mut client)
        .await
        .expect("read SASL continue");
    assert_eq!(server_continue.tag, b'R');
    let server_first =
        parse_auth_sasl_continue(server_continue.payload).expect("parse server-first");

    let client_final = build_scram_client_final("StrongPass123!", client_first, &server_first);
    client
        .write_all(&build_sasl_response_bytes(&client_final))
        .await
        .expect("write SASL final response");

    let server_final = read_live_backend_message(&mut client)
        .await
        .expect("read SASL final");
    assert_eq!(server_final.tag, b'R');
    assert!(
        parse_auth_sasl_final(server_final.payload)
            .expect("parse server-final")
            .starts_with("v=")
    );

    let auth_ok = read_live_backend_message(&mut client)
        .await
        .expect("read auth ok");
    assert_eq!(auth_ok.tag, b'R');
    assert_eq!(
        parse_auth_request(auth_ok.payload).expect("parse auth ok"),
        0
    );

    while read_live_backend_message(&mut client)
        .await
        .expect("read startup completion")
        .tag
        != b'Z'
    {}

    for ddl in [
        "CREATE TABLE django_wire_auth_parent (id INT PRIMARY KEY)",
        "CREATE TABLE django_wire_auth_child ( \
             id INT PRIMARY KEY, \
             parent_id INT NOT NULL REFERENCES django_wire_auth_parent(id), \
             slug TEXT UNIQUE \
         )",
    ] {
        client
            .write_all(&build_query_bytes(ddl))
            .await
            .expect("write DDL query");
        let mut ddl_error = None;
        loop {
            let message = read_live_backend_message(&mut client)
                .await
                .expect("read DDL response");
            if message.tag == b'E' {
                ddl_error = parse_error_response_message(&message.payload);
            }
            if message.tag == b'Z' {
                break;
            }
        }
        assert_eq!(ddl_error, None, "unexpected DDL error: {ddl_error:?}");
    }

    client
        .write_all(&build_query_bytes(
            "SELECT c.conname, \
                    array( \
                        SELECT attname \
                        FROM unnest(c.conkey) WITH ORDINALITY cols(colid, arridx) \
                        JOIN pg_attribute AS ca ON cols.colid = ca.attnum \
                        WHERE ca.attrelid = c.conrelid \
                        ORDER BY cols.arridx \
                    ), \
                    c.contype, \
                    (SELECT fkc.relname || '.' || fka.attname \
                       FROM pg_attribute AS fka \
                       JOIN pg_class AS fkc ON fka.attrelid = fkc.oid \
                      WHERE fka.attrelid = c.confrelid AND fka.attnum = c.confkey[1]), \
                    cl.reloptions \
               FROM pg_constraint AS c \
               JOIN pg_class AS cl ON c.conrelid = cl.oid \
              WHERE cl.relname = 'django_wire_auth_child' \
                AND pg_catalog.pg_table_is_visible(cl.oid)",
        ))
        .await
        .expect("write reflection query");

    let mut error_message = None;
    let mut data_rows = 0usize;
    loop {
        let message = read_live_backend_message(&mut client)
            .await
            .expect("read reflection response");
        match message.tag {
            b'D' => data_rows += 1,
            b'E' => error_message = parse_error_response_message(&message.payload),
            b'Z' => break,
            _ => {}
        }
    }
    assert_eq!(error_message, None, "unexpected error: {error_message:?}");
    assert_eq!(data_rows, 3);

    client
        .write_all(&build_terminate_bytes())
        .await
        .expect("write terminate");
    task.await
        .expect("join connection task")
        .expect("SCRAM django reflection connection should succeed");

    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn simple_query_django_constraint_reflection_with_ordinality_works_over_tcp_socket() {
    let data_dir = std::env::temp_dir().join(format!(
        "aiondb-pgwire-tcp-django-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    let engine = Arc::new(
        EngineBuilder::new_durable(data_dir.clone())
            .expect("durable builder")
            .with_authorizer(Arc::new(aiondb_security::AllowAllAuthorizer))
            .build()
            .expect("build durable engine"),
    );
    engine
        .bootstrap_role("admin", "StrongPass123!", true)
        .expect("bootstrap admin role");

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind tcp listener");
    let addr = listener.local_addr().expect("listener addr");
    let pool_engine = Arc::clone(&engine);
    let server_task = tokio::spawn(async move {
        let (socket, peer_addr) = listener.accept().await.expect("accept client");
        let (server_reader, server_writer) = split(socket);
        let mut conn = Connection::new(
            engine,
            BufReader::with_capacity(16 * 1024, server_reader),
            server_writer,
            1,
            42,
            CancelRegistry::new(),
        );
        conn.set_peer_addr(Some(peer_addr.to_string()));
        conn.set_engine_pool(crate::engine_pool::EnginePool::new(
            pool_engine,
            aiondb_config::EnginePoolConfig {
                worker_threads: 8,
                queue_depth: 256,
            },
        ));
        conn.run().await
    });

    let mut client = TcpStream::connect(addr).await.expect("connect tcp client");
    client
        .write_all(&build_startup_bytes_with_user("admin"))
        .await
        .expect("write startup");

    let sasl = read_live_backend_message(&mut client)
        .await
        .expect("read SASL auth request");
    assert_eq!(sasl.tag, b'R');
    assert_eq!(parse_auth_request(sasl.payload.clone()).expect("parse auth type"), 10);
    let client_first = "n,,n=admin,r=clientnonce";
    client
        .write_all(&build_sasl_initial_response_bytes(
            "SCRAM-SHA-256",
            client_first,
        ))
        .await
        .expect("write SASL initial response");

    let server_continue = read_live_backend_message(&mut client)
        .await
        .expect("read SASL continue");
    let server_first =
        parse_auth_sasl_continue(server_continue.payload).expect("parse server-first");
    let client_final = build_scram_client_final("StrongPass123!", client_first, &server_first);
    client
        .write_all(&build_sasl_response_bytes(&client_final))
        .await
        .expect("write SASL final response");

    let server_final = read_live_backend_message(&mut client)
        .await
        .expect("read SASL final");
    assert!(
        parse_auth_sasl_final(server_final.payload)
            .expect("parse server-final")
            .starts_with("v=")
    );
    let auth_ok = read_live_backend_message(&mut client)
        .await
        .expect("read auth ok");
    assert_eq!(
        parse_auth_request(auth_ok.payload).expect("parse auth ok"),
        0
    );
    while read_live_backend_message(&mut client)
        .await
        .expect("read startup completion")
        .tag
        != b'Z'
    {}

    for ddl in [
        "DROP TABLE IF EXISTS django_wire_tcp_child",
        "DROP TABLE IF EXISTS django_wire_tcp_parent",
        "CREATE TABLE django_wire_tcp_parent (id INT PRIMARY KEY)",
        "CREATE TABLE django_wire_tcp_child ( \
             id INT PRIMARY KEY, \
             parent_id INT NOT NULL REFERENCES django_wire_tcp_parent(id), \
             slug TEXT UNIQUE \
         )",
    ] {
        client
            .write_all(&build_query_bytes(ddl))
            .await
            .expect("write DDL query");
        let mut ddl_error = None;
        loop {
            let message = read_live_backend_message(&mut client)
                .await
                .expect("read DDL response");
            if message.tag == b'E' {
                ddl_error = parse_error_response_message(&message.payload);
            }
            if message.tag == b'Z' {
                break;
            }
        }
        assert_eq!(ddl_error, None, "unexpected DDL error: {ddl_error:?}");
    }

    client
        .write_all(&build_query_bytes(
            "SELECT c.conname, \
                    array( \
                        SELECT attname \
                        FROM unnest(c.conkey) WITH ORDINALITY cols(colid, arridx) \
                        JOIN pg_attribute AS ca ON cols.colid = ca.attnum \
                        WHERE ca.attrelid = c.conrelid \
                        ORDER BY cols.arridx \
                    ), \
                    c.contype, \
                    (SELECT fkc.relname || '.' || fka.attname \
                       FROM pg_attribute AS fka \
                       JOIN pg_class AS fkc ON fka.attrelid = fkc.oid \
                      WHERE fka.attrelid = c.confrelid AND fka.attnum = c.confkey[1]), \
                    cl.reloptions \
               FROM pg_constraint AS c \
               JOIN pg_class AS cl ON c.conrelid = cl.oid \
              WHERE cl.relname = 'django_wire_tcp_child' \
                AND pg_catalog.pg_table_is_visible(cl.oid)",
        ))
        .await
        .expect("write reflection query");

    let mut error_message = None;
    let mut data_rows = 0usize;
    loop {
        let message = read_live_backend_message(&mut client)
            .await
            .expect("read reflection response");
        match message.tag {
            b'D' => data_rows += 1,
            b'E' => error_message = parse_error_response_message(&message.payload),
            b'Z' => break,
            _ => {}
        }
    }
    assert_eq!(error_message, None, "unexpected error: {error_message:?}");
    assert_eq!(data_rows, 3);

    client
        .write_all(&build_terminate_bytes())
        .await
        .expect("write terminate");
    server_task
        .await
        .expect("join tcp connection task")
        .expect("tcp SCRAM django reflection connection should succeed");

    let _ = std::fs::remove_dir_all(data_dir);
}

async fn run_django_constraint_reflection_roundtrip_over_tcp(
    addr: std::net::SocketAddr,
    prefix: &str,
) -> DbResult<usize> {
    let mut client = TcpStream::connect(addr)
        .await
        .map_err(|error| DbError::protocol(format!("connect tcp client: {error}")))?;
    client
        .write_all(&build_startup_bytes_with_user("admin"))
        .await
        .map_err(|error| DbError::protocol(format!("write startup: {error}")))?;

    let sasl = read_live_backend_message(&mut client).await?;
    if sasl.tag != b'R' || parse_auth_request(sasl.payload.clone())? != 10 {
        return Err(DbError::protocol("expected SCRAM auth request"));
    }
    let client_first = "n,,n=admin,r=clientnonce";
    client
        .write_all(&build_sasl_initial_response_bytes(
            "SCRAM-SHA-256",
            client_first,
        ))
        .await
        .map_err(|error| DbError::protocol(format!("write SASL initial response: {error}")))?;

    let server_continue = read_live_backend_message(&mut client).await?;
    let server_first = parse_auth_sasl_continue(server_continue.payload)?;
    let client_final = build_scram_client_final("StrongPass123!", client_first, &server_first);
    client
        .write_all(&build_sasl_response_bytes(&client_final))
        .await
        .map_err(|error| DbError::protocol(format!("write SASL final response: {error}")))?;

    let server_final = read_live_backend_message(&mut client).await?;
    if !parse_auth_sasl_final(server_final.payload)?.starts_with("v=") {
        return Err(DbError::protocol("expected SCRAM server-final proof"));
    }
    let auth_ok = read_live_backend_message(&mut client).await?;
    if parse_auth_request(auth_ok.payload)? != 0 {
        return Err(DbError::protocol("expected AuthenticationOk"));
    }
    while read_live_backend_message(&mut client).await?.tag != b'Z' {}

    for ddl in [
        format!("DROP TABLE IF EXISTS {prefix}_child"),
        format!("DROP TABLE IF EXISTS {prefix}_parent"),
        format!("CREATE TABLE {prefix}_parent (id INT PRIMARY KEY)"),
        format!(
            "CREATE TABLE {prefix}_child ( \
                 id INT PRIMARY KEY, \
                 parent_id INT NOT NULL REFERENCES {prefix}_parent(id), \
                 slug TEXT UNIQUE \
             )"
        ),
    ] {
        client
            .write_all(&build_query_bytes(&ddl))
            .await
            .map_err(|error| DbError::protocol(format!("write DDL query: {error}")))?;
        let mut ddl_error = None;
        loop {
            let message = read_live_backend_message(&mut client).await?;
            if message.tag == b'E' {
                ddl_error = parse_error_response_message(&message.payload);
            }
            if message.tag == b'Z' {
                break;
            }
        }
        if let Some(message) = ddl_error {
            return Err(DbError::protocol(format!("unexpected DDL error: {message}")));
        }
    }

    let query = format!(
        "SELECT c.conname, \
                array( \
                    SELECT attname \
                    FROM unnest(c.conkey) WITH ORDINALITY cols(colid, arridx) \
                    JOIN pg_attribute AS ca ON cols.colid = ca.attnum \
                    WHERE ca.attrelid = c.conrelid \
                    ORDER BY cols.arridx \
                ), \
                c.contype, \
                (SELECT fkc.relname || '.' || fka.attname \
                   FROM pg_attribute AS fka \
                   JOIN pg_class AS fkc ON fka.attrelid = fkc.oid \
                  WHERE fka.attrelid = c.confrelid AND fka.attnum = c.confkey[1]), \
                cl.reloptions \
           FROM pg_constraint AS c \
           JOIN pg_class AS cl ON c.conrelid = cl.oid \
          WHERE cl.relname = '{prefix}_child' \
            AND pg_catalog.pg_table_is_visible(cl.oid)"
    );
    client
        .write_all(&build_query_bytes(&query))
        .await
        .map_err(|error| DbError::protocol(format!("write reflection query: {error}")))?;

    let mut error_message = None;
    let mut data_rows = 0usize;
    loop {
        let message = read_live_backend_message(&mut client).await?;
        match message.tag {
            b'D' => data_rows += 1,
            b'E' => error_message = parse_error_response_message(&message.payload),
            b'Z' => break,
            _ => {}
        }
    }

    client
        .write_all(&build_terminate_bytes())
        .await
        .map_err(|error| DbError::protocol(format!("write terminate: {error}")))?;

    if let Some(message) = error_message {
        return Err(DbError::protocol(message));
    }
    Ok(data_rows)
}

#[tokio::test]
async fn simple_query_django_constraint_reflection_remains_stable_across_connections() {
    let data_dir = std::env::temp_dir().join(format!(
        "aiondb-pgwire-tcp-django-repeat-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    let engine = Arc::new(
        EngineBuilder::new_durable(data_dir.clone())
            .expect("durable builder")
            .with_authorizer(Arc::new(aiondb_security::AllowAllAuthorizer))
            .build()
            .expect("build durable engine"),
    );
    engine
        .bootstrap_role("admin", "StrongPass123!", true)
        .expect("bootstrap admin role");

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind tcp listener");
    let addr = listener.local_addr().expect("listener addr");
    let pool_engine = Arc::clone(&engine);
    let server_task = tokio::spawn(async move {
        for pid in [1u32, 2u32] {
            let (socket, peer_addr) = listener.accept().await.expect("accept client");
            let pool_engine = Arc::clone(&pool_engine);
            let engine = Arc::clone(&engine);
            tokio::spawn(async move {
                let (server_reader, server_writer) = split(socket);
                let mut conn = Connection::new(
                    engine,
                    BufReader::with_capacity(16 * 1024, server_reader),
                    server_writer,
                    pid,
                    42 + pid,
                    CancelRegistry::new(),
                );
                conn.set_peer_addr(Some(peer_addr.to_string()));
                conn.set_engine_pool(crate::engine_pool::EnginePool::new(
                    pool_engine,
                    aiondb_config::EnginePoolConfig {
                        worker_threads: 8,
                        queue_depth: 256,
                    },
                ));
                conn.run().await
            })
            .await
            .expect("join spawned connection task")
            .expect("tcp connection run should succeed");
        }
    });

    let rows_first = run_django_constraint_reflection_roundtrip_over_tcp(addr, "repeat_psy1")
        .await
        .expect("first reflection roundtrip should succeed");
    let rows_second = run_django_constraint_reflection_roundtrip_over_tcp(addr, "repeat_psy2")
        .await
        .expect("second reflection roundtrip should succeed");

    assert_eq!(rows_first, 3);
    assert_eq!(rows_second, 3);

    server_task.await.expect("join server task");
    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn extended_execute_portal_writes_copy_out_subprotocol_for_copy_batches() {
    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes("s_copy_out", "COPY t TO STDOUT", &[]));
    input.extend(build_bind_bytes("p_copy_out", "s_copy_out", &[], &[], &[]));
    input.extend(build_execute_bytes("p_copy_out", 0));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let engine = Arc::new(CopyOutPortalMockEngine);
    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("extended copy out portal should succeed");

    let messages = backend_messages(conn.writer_ref());
    let tags: Vec<u8> = messages.iter().map(|(tag, _)| *tag).collect();
    assert!(tags.contains(&b'H'), "expected CopyOutResponse");
    let copy_out_payload = messages
        .iter()
        .find(|(tag, _)| *tag == b'H')
        .map(|(_, payload)| payload.as_slice())
        .expect("copy out response");
    assert_eq!(parse_copy_response_column_count(copy_out_payload), 2);
    assert_eq!(
        count_tag_occurrences(&tags, b'd'),
        2,
        "expected one CopyData per exported line"
    );
    assert!(tags.contains(&b'c'), "expected CopyDone");
    let last_command = messages
        .iter()
        .rfind(|(tag, _)| *tag == b'C')
        .map(|(_, payload)| parse_cstring_payload(payload))
        .expect("command complete");
    assert_eq!(last_command, "COPY 2");
}

#[tokio::test]
async fn extended_execute_portal_preserves_copy_out_column_count_for_empty_exports() {
    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_copy_out_empty",
        "COPY t TO STDOUT",
        &[],
    ));
    input.extend(build_bind_bytes(
        "p_copy_out_empty",
        "s_copy_out_empty",
        &[],
        &[],
        &[],
    ));
    input.extend(build_execute_bytes("p_copy_out_empty", 0));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let engine = Arc::new(EmptyCopyOutPortalMockEngine);
    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("extended empty copy out portal should succeed");

    let messages = backend_messages(conn.writer_ref());
    let copy_out_payload = messages
        .iter()
        .find(|(tag, _)| *tag == b'H')
        .map(|(_, payload)| payload.as_slice())
        .expect("copy out response");
    assert_eq!(parse_copy_response_column_count(copy_out_payload), 2);
    assert!(
        !messages.iter().any(|(tag, _)| *tag == b'd'),
        "empty copy out should not emit CopyData frames"
    );
    let last_command = messages
        .iter()
        .rfind(|(tag, _)| *tag == b'C')
        .map(|(_, payload)| parse_cstring_payload(payload))
        .expect("command complete");
    assert_eq!(last_command, "COPY 0");
}

#[tokio::test]
async fn extended_execute_portal_writes_notice_response_for_real_engine_notices() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_drop_if_exists_notice",
        "DROP TABLE IF EXISTS missing_table",
        &[],
    ));
    input.extend(build_bind_bytes(
        "p_drop_if_exists_notice",
        "s_drop_if_exists_notice",
        &[],
        &[],
        &[],
    ));
    input.extend(build_execute_bytes("p_drop_if_exists_notice", 0));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("extended portal notice path should succeed");

    let messages = backend_messages(conn.writer_ref());
    let tags: Vec<u8> = messages.iter().map(|(tag, _)| *tag).collect();
    assert!(tags.contains(&b'N'), "expected a NoticeResponse");
    let notice_payload = messages
        .iter()
        .find(|(tag, _)| *tag == b'N')
        .map(|(_, payload)| payload.as_slice())
        .expect("notice response");
    assert!(
        String::from_utf8_lossy(notice_payload).contains("missing_table"),
        "expected notice to mention missing table"
    );
    let last_command = messages
        .iter()
        .rfind(|(tag, _)| *tag == b'C')
        .map(|(_, payload)| parse_cstring_payload(payload))
        .expect("command complete");
    assert_eq!(last_command, "DROP TABLE");
}

#[tokio::test]
async fn extended_describe_statement_preserves_pg_catalog_name_and_internal_char_oids() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_pg_type_aliases",
        "SELECT typname, typtype FROM pg_catalog.pg_type ORDER BY oid LIMIT 1",
        &[],
    ));
    input.extend(build_describe_bytes(b'S', "s_pg_type_aliases"));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("describe statement should preserve pg_catalog alias oids");

    let messages = backend_messages(conn.writer_ref());
    let row_description = messages
        .iter()
        .find(|(tag, _)| *tag == b'T')
        .map(|(_, payload)| payload.as_slice())
        .expect("row description");
    assert_eq!(
        parse_row_description_type_info(row_description),
        vec![(19, -1), (18, -1)]
    );
}

#[tokio::test]
async fn extended_describe_statement_preserves_pg_namespace_type_and_authid_oid_aliases() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_pg_namespace_oids",
        "SELECT oid, nspowner FROM pg_catalog.pg_namespace ORDER BY oid LIMIT 1",
        &[],
    ));
    input.extend(build_describe_bytes(b'S', "s_pg_namespace_oids"));
    input.extend(build_sync_bytes());

    input.extend(build_parse_bytes(
        "s_pg_type_oids",
        "SELECT oid, typarray, typnamespace, typbasetype, typcollation, typrelid, typelem, typowner \
         FROM pg_catalog.pg_type ORDER BY oid LIMIT 1",
        &[],
    ));
    input.extend(build_describe_bytes(b'S', "s_pg_type_oids"));
    input.extend(build_sync_bytes());

    input.extend(build_parse_bytes(
        "s_pg_authid_oid",
        "SELECT oid FROM pg_catalog.pg_authid ORDER BY oid LIMIT 1",
        &[],
    ));
    input.extend(build_describe_bytes(b'S', "s_pg_authid_oid"));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run().await.expect(
        "describe statement should preserve pg_namespace, pg_type and pg_authid oid aliases",
    );

    let messages = backend_messages(conn.writer_ref());
    let row_descriptions: Vec<_> = messages
        .iter()
        .filter(|(tag, _)| *tag == b'T')
        .map(|(_, payload)| parse_row_description_type_info(payload))
        .collect();
    assert_eq!(
        row_descriptions,
        vec![
            vec![(26, -1), (26, -1)],
            vec![
                (26, -1),
                (26, -1),
                (26, -1),
                (26, -1),
                (26, -1),
                (26, -1),
                (26, -1),
                (26, -1),
            ],
            vec![(26, -1)],
        ]
    );
}

#[tokio::test]
async fn extended_describe_statement_preserves_pg_type_internal_char_oids() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_pg_type_internal_chars",
        "SELECT typalign, typstorage FROM pg_catalog.pg_type ORDER BY oid LIMIT 1",
        &[],
    ));
    input.extend(build_describe_bytes(b'S', "s_pg_type_internal_chars"));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("describe statement should preserve pg_type internal char aliases");

    let messages = backend_messages(conn.writer_ref());
    let row_description = messages
        .iter()
        .find(|(tag, _)| *tag == b'T')
        .map(|(_, payload)| payload.as_slice())
        .expect("row description");
    assert_eq!(
        parse_row_description_type_info(row_description),
        vec![(18, -1), (18, -1)]
    );
}

#[tokio::test]
async fn extended_describe_statement_preserves_pg_type_regproc_oids() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_pg_type_regprocs",
        "SELECT typinput, typoutput FROM pg_catalog.pg_type ORDER BY oid LIMIT 1",
        &[],
    ));
    input.extend(build_describe_bytes(b'S', "s_pg_type_regprocs"));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("describe statement should preserve pg_type regproc aliases");

    let messages = backend_messages(conn.writer_ref());
    let row_description = messages
        .iter()
        .find(|(tag, _)| *tag == b'T')
        .map(|(_, payload)| payload.as_slice())
        .expect("row description");
    assert_eq!(
        parse_row_description_type_info(row_description),
        vec![(24, -1), (24, -1)]
    );
}

#[tokio::test]
async fn extended_describe_statement_preserves_pg_constraint_int_array_oids() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));
    let (session, _) = engine
        .startup(StartupParams {
            database: "default".to_owned(),
            application_name: Some("pgwire-e2e-pg-constraint-seed".to_owned()),
            options: std::collections::BTreeMap::new(),
            credential: Credential::Anonymous {
                user: "test".to_owned(),
            },
            transport: TransportInfo::in_process(),
        })
        .expect("startup seed session");
    engine
        .execute_sql(&session, "CREATE TABLE t_con_meta (id INT PRIMARY KEY)")
        .expect("create constraint table");

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_pg_constraint_arrays",
        "SELECT conkey, confkey FROM pg_catalog.pg_constraint ORDER BY oid LIMIT 1",
        &[],
    ));
    input.extend(build_describe_bytes(b'S', "s_pg_constraint_arrays"));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("describe statement should preserve pg_constraint int[] oids");

    let messages = backend_messages(conn.writer_ref());
    let row_description = messages
        .iter()
        .find(|(tag, _)| *tag == b'T')
        .map(|(_, payload)| payload.as_slice())
        .expect("row description");
    assert_eq!(
        parse_row_description_type_info(row_description),
        vec![(1007, -1), (1007, -1)]
    );
}

#[tokio::test]
async fn extended_describe_statement_keeps_int4_oid_for_integer_primary_key_columns() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));
    let (session, _) = engine
        .startup(StartupParams {
            database: "default".to_owned(),
            application_name: Some("pgwire-e2e-int4-pk-oid".to_owned()),
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
            "CREATE TABLE t_pk_oid_regression (id INTEGER PRIMARY KEY, payload TEXT)",
        )
        .expect("create regression table");
    engine
        .execute_sql(&session, "INSERT INTO t_pk_oid_regression VALUES (1, 'ok')")
        .expect("seed regression table");

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_pk_oid_regression",
        "SELECT id FROM t_pk_oid_regression",
        &[],
    ));
    input.extend(build_describe_bytes(b'S', "s_pk_oid_regression"));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("describe statement should keep INT4 OID for user integer PK columns");

    let messages = backend_messages(conn.writer_ref());
    let row_description = messages
        .iter()
        .find(|(tag, _)| *tag == b'T')
        .map(|(_, payload)| payload.as_slice())
        .expect("row description");
    assert_eq!(parse_row_description_type_info(row_description), vec![(23, -1)]);
}

#[tokio::test]
async fn extended_describe_statement_reports_int4_parameter_oid_for_integer_primary_key_predicate()
{
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));
    let (session, _) = engine
        .startup(StartupParams {
            database: "default".to_owned(),
            application_name: Some("pgwire-e2e-int4-pk-param-oid".to_owned()),
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
            "CREATE TABLE t_pk_param_oid_regression (id INTEGER PRIMARY KEY, payload TEXT)",
        )
        .expect("create regression table");

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_pk_param_oid_regression",
        "SELECT payload FROM t_pk_param_oid_regression WHERE id = $1",
        &[],
    ));
    input.extend(build_describe_bytes(b'S', "s_pk_param_oid_regression"));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("describe statement should infer INT4 param OID for user integer PK predicate");

    let messages = backend_messages(conn.writer_ref());
    let parameter_description = messages
        .iter()
        .find(|(tag, _)| *tag == b't')
        .map(|(_, payload)| payload.as_slice())
        .expect("parameter description");
    assert_eq!(parse_parameter_description_oids(parameter_description), vec![23]);
}

#[tokio::test]
async fn extended_describe_statement_preserves_pg_proc_array_like_oids() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_pg_proc_arrays",
        "SELECT proallargtypes, proargmodes, proargnames, protrftypes, proconfig, proacl \
         FROM pg_catalog.pg_proc ORDER BY oid LIMIT 1",
        &[],
    ));
    input.extend(build_describe_bytes(b'S', "s_pg_proc_arrays"));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("describe statement should preserve pg_proc array-like oids");

    let messages = backend_messages(conn.writer_ref());
    let row_description = messages
        .iter()
        .find(|(tag, _)| *tag == b'T')
        .map(|(_, payload)| payload.as_slice())
        .expect("row description");
    assert_eq!(
        parse_row_description_type_info(row_description),
        vec![
            (1028, -1),
            (1002, -1),
            (1009, -1),
            (1028, -1),
            (1009, -1),
            (1009, -1)
        ]
    );
}

#[tokio::test]
async fn extended_describe_statement_preserves_pg_proc_prosupport_regproc_oid() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_pg_proc_prosupport",
        "SELECT prosupport FROM pg_catalog.pg_proc ORDER BY oid LIMIT 1",
        &[],
    ));
    input.extend(build_describe_bytes(b'S', "s_pg_proc_prosupport"));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("describe statement should preserve pg_proc prosupport regproc oid");

    let messages = backend_messages(conn.writer_ref());
    let row_description = messages
        .iter()
        .find(|(tag, _)| *tag == b'T')
        .map(|(_, payload)| payload.as_slice())
        .expect("row description");
    assert_eq!(
        parse_row_description_type_info(row_description),
        vec![(24, -1)]
    );
}

#[tokio::test]
async fn extended_describe_statement_preserves_pg_proc_oid_identity_columns() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_pg_proc_oid_identities",
        "SELECT oid, pronamespace, proowner, prolang, provariadic, prorettype \
         FROM pg_catalog.pg_proc ORDER BY oid LIMIT 1",
        &[],
    ));
    input.extend(build_describe_bytes(b'S', "s_pg_proc_oid_identities"));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("describe statement should preserve pg_proc oid identity columns");

    let messages = backend_messages(conn.writer_ref());
    let row_description = messages
        .iter()
        .find(|(tag, _)| *tag == b'T')
        .map(|(_, payload)| payload.as_slice())
        .expect("row description");
    assert_eq!(
        parse_row_description_type_info(row_description),
        vec![(26, -1), (26, -1), (26, -1), (26, -1), (26, -1), (26, -1)]
    );
}

#[tokio::test]
async fn extended_describe_statement_preserves_char_and_varchar_typmods() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));
    let (session, _) = engine
        .startup(StartupParams {
            database: "default".to_owned(),
            application_name: Some("pgwire-e2e-typmod-seed".to_owned()),
            options: std::collections::BTreeMap::new(),
            credential: Credential::Anonymous {
                user: "test".to_owned(),
            },
            transport: TransportInfo::in_process(),
        })
        .expect("startup seed session");
    engine
        .execute_sql(&session, "CREATE TABLE t_typmod (v VARCHAR(5), c CHAR(3))")
        .expect("create typmod table");

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_typmod",
        "SELECT v, c FROM t_typmod",
        &[],
    ));
    input.extend(build_describe_bytes(b'S', "s_typmod"));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("describe statement should preserve typmods");

    let messages = backend_messages(conn.writer_ref());
    let row_description = messages
        .iter()
        .find(|(tag, _)| *tag == b'T')
        .map(|(_, payload)| payload.as_slice())
        .expect("row description");
    assert_eq!(
        parse_row_description_type_info(row_description),
        vec![(1043, 9), (1042, 7)]
    );
}

#[tokio::test]
async fn extended_describe_statement_preserves_char_and_varchar_array_typmods() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));
    let (session, _) = engine
        .startup(StartupParams {
            database: "default".to_owned(),
            application_name: Some("pgwire-e2e-array-typmod-seed".to_owned()),
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
            "CREATE TABLE t_typmod_arr (v VARCHAR(5)[], c CHAR(3)[])",
        )
        .expect("create typmod array table");

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_typmod_arr",
        "SELECT v, c FROM t_typmod_arr",
        &[],
    ));
    input.extend(build_describe_bytes(b'S', "s_typmod_arr"));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("describe statement should preserve array typmods");

    let messages = backend_messages(conn.writer_ref());
    let row_description = messages
        .iter()
        .find(|(tag, _)| *tag == b'T')
        .map(|(_, payload)| payload.as_slice())
        .expect("row description");
    assert_eq!(
        parse_row_description_type_info(row_description),
        vec![(1015, 9), (1014, 7)]
    );
}

#[tokio::test]
async fn extended_describe_statement_preserves_char_and_varchar_typmods_through_view() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));
    let (session, _) = engine
        .startup(StartupParams {
            database: "default".to_owned(),
            application_name: Some("pgwire-e2e-view-typmod-seed".to_owned()),
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
            "CREATE TABLE t_typmod_view (v VARCHAR(5), c CHAR(3));
             CREATE VIEW v_typmod AS SELECT v, c FROM t_typmod_view",
        )
        .expect("create typmod view");

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_typmod_view",
        "SELECT * FROM v_typmod",
        &[],
    ));
    input.extend(build_describe_bytes(b'S', "s_typmod_view"));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("describe statement should preserve view typmods");

    let messages = backend_messages(conn.writer_ref());
    let row_description = messages
        .iter()
        .find(|(tag, _)| *tag == b'T')
        .map(|(_, payload)| payload.as_slice())
        .expect("row description");
    assert_eq!(
        parse_row_description_type_info(row_description),
        vec![(1043, 9), (1042, 7)]
    );
}

#[tokio::test]
async fn extended_describe_statement_preserves_char_and_varchar_typmods_through_ctas() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));
    let (session, _) = engine
        .startup(StartupParams {
            database: "default".to_owned(),
            application_name: Some("pgwire-e2e-ctas-typmod-seed".to_owned()),
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
            "CREATE TABLE t_typmod_ctas_src (v VARCHAR(5), c CHAR(3));
             CREATE TABLE t_typmod_ctas_dst AS SELECT v, c FROM t_typmod_ctas_src",
        )
        .expect("create typmod ctas table");

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_typmod_ctas",
        "SELECT v, c FROM t_typmod_ctas_dst",
        &[],
    ));
    input.extend(build_describe_bytes(b'S', "s_typmod_ctas"));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("describe statement should preserve ctas typmods");

    let messages = backend_messages(conn.writer_ref());
    let row_description = messages
        .iter()
        .find(|(tag, _)| *tag == b'T')
        .map(|(_, payload)| payload.as_slice())
        .expect("row description");
    assert_eq!(
        parse_row_description_type_info(row_description),
        vec![(1043, 9), (1042, 7)]
    );
}

#[tokio::test]
async fn extended_describe_statement_preserves_direct_varchar_param_oid() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes("s_varchar_param", "SELECT $1", &[1043]));
    input.extend(build_describe_bytes(b'S', "s_varchar_param"));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("describe statement should preserve direct varchar parameter oid");

    let messages = backend_messages(conn.writer_ref());
    let row_description = messages
        .iter()
        .find(|(tag, _)| *tag == b'T')
        .map(|(_, payload)| payload.as_slice())
        .expect("row description");
    assert_eq!(
        parse_row_description_type_info(row_description),
        vec![(1043, -1)]
    );
}
