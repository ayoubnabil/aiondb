//! Unit tests for the Bolt-compat server (split out of `bolt_compat.rs`).
//!
//! Body preserved byte-for-byte; reached parent items via `use super::*`.
#![allow(clippy::too_many_lines)]

    use super::*;
    use aiondb_config::{RuntimeConfig, StorageBackend};

    fn test_engine() -> Arc<Engine> {
        let mut config = RuntimeConfig::default();
        crate::apply_server_security_baseline(&mut config, StorageBackend::InMemory);
        crate::build_server_engine(None, &config, StorageBackend::InMemory, false)
            .expect("build test engine")
    }

    fn assert_run_success_message(
        payload: &[u8],
        expected_fields: &[&str],
        expected_qid: i64,
        expected_db: &str,
    ) {
        assert!(payload.len() >= 2);
        assert_eq!(payload[0], 0xB1);
        assert_eq!(payload[1], BOLT_SUCCESS_SIGNATURE);
        let mut remaining = &payload[2..];
        let map_len = decode_map_len(&mut remaining).expect("run success map");
        assert_eq!(map_len, 5);

        let mut fields = None;
        let mut qid = None;
        let mut db = None;
        let mut t_first = None;
        let mut result_available_after = None;

        for _ in 0..map_len {
            let key = decode_string(&mut remaining).expect("run success key");
            match key.as_str() {
                "fields" => {
                    let len = decode_list_len(&mut remaining).expect("fields len");
                    let mut values = Vec::with_capacity(len);
                    for _ in 0..len {
                        values.push(decode_string(&mut remaining).expect("field"));
                    }
                    fields = Some(values);
                }
                "qid" => qid = decode_optional_i64(&mut remaining).expect("qid"),
                "db" => db = Some(decode_string(&mut remaining).expect("db")),
                "t_first" => t_first = decode_optional_i64(&mut remaining).expect("t_first"),
                "result_available_after" => {
                    result_available_after =
                        decode_optional_i64(&mut remaining).expect("result_available_after")
                }
                other => panic!("unexpected key {other}"),
            }
        }

        assert!(remaining.is_empty());
        assert_eq!(
            fields.as_deref(),
            Some(
                &expected_fields
                    .iter()
                    .map(|value| (*value).to_owned())
                    .collect::<Vec<_>>()[..]
            )
        );
        assert_eq!(qid, Some(expected_qid));
        assert_eq!(db.as_deref(), Some(expected_db));
        assert!(t_first.unwrap_or(-1) >= 0);
        assert_eq!(t_first, result_available_after);
    }

    fn assert_hello_success_message(payload: &[u8]) {
        assert!(payload.len() >= 2);
        assert_eq!(payload[0], 0xB1);
        assert_eq!(payload[1], BOLT_SUCCESS_SIGNATURE);
        let mut remaining = &payload[2..];
        let map_len = decode_map_len(&mut remaining).expect("hello success map");
        assert_eq!(map_len, 3);

        let mut server = None;
        let mut connection_id = None;
        let mut telemetry_enabled = None;
        let mut recv_timeout = None;

        for _ in 0..map_len {
            let key = decode_string(&mut remaining).expect("hello success key");
            match key.as_str() {
                "server" => server = Some(decode_string(&mut remaining).expect("server")),
                "connection_id" => {
                    connection_id = Some(decode_string(&mut remaining).expect("connection_id"));
                }
                "hints" => {
                    let hints_len = decode_map_len(&mut remaining).expect("hello hints map");
                    assert_eq!(hints_len, 2);
                    for _ in 0..hints_len {
                        let hint_key = decode_string(&mut remaining).expect("hint key");
                        match hint_key.as_str() {
                            "telemetry.enabled" => {
                                let Some((&marker, rest)) = remaining.split_first() else {
                                    panic!("missing telemetry bool payload");
                                };
                                remaining = rest;
                                telemetry_enabled = Some(match marker {
                                    0xC3 => true,
                                    0xC2 => false,
                                    other => panic!("unexpected bool marker {other:#x}"),
                                });
                            }
                            "connection.recv_timeout_seconds" => {
                                recv_timeout =
                                    decode_optional_i64(&mut remaining).expect("recv timeout");
                            }
                            other => panic!("unexpected hint key {other}"),
                        }
                    }
                }
                other => panic!("unexpected key {other}"),
            }
        }

        assert!(remaining.is_empty());
        assert_eq!(server.as_deref(), Some(BOLT_SERVER_AGENT));
        let connection_id = connection_id.expect("connection_id");
        assert!(connection_id.starts_with(BOLT_APPLICATION_NAME));
        assert!(connection_id.contains(':'));
        assert_eq!(telemetry_enabled, Some(false));
        assert_eq!(recv_timeout, Some(BOLT_CONNECTION_RECV_TIMEOUT_SECONDS));
    }

    fn assert_pull_success_message(
        payload: &[u8],
        expected_pairs: &[(&str, &str)],
        expected_has_more: bool,
        expected_status: BoltStatusKind,
    ) {
        assert_pull_success_message_with_stats(
            payload,
            expected_pairs,
            expected_has_more,
            expected_status,
            &BoltQueryStats::default(),
        );
    }

    fn assert_pull_success_message_with_stats(
        payload: &[u8],
        expected_pairs: &[(&str, &str)],
        expected_has_more: bool,
        expected_status: BoltStatusKind,
        expected_stats: &BoltQueryStats,
    ) {
        assert!(payload.len() >= 2);
        assert_eq!(payload[0], 0xB1);
        assert_eq!(payload[1], BOLT_SUCCESS_SIGNATURE);
        let mut remaining = &payload[2..];
        let map_len = decode_map_len(&mut remaining).expect("pull success map");

        let mut values = JsonMap::new();
        let mut saw_stats = false;
        let mut saw_statuses = false;
        let mut qid = None;
        let mut t_last = None;
        let mut result_consumed_after = None;

        for _ in 0..map_len {
            let key = decode_string(&mut remaining).expect("pull success key");
            match key.as_str() {
                "has_more" => {
                    let Some((&marker, rest)) = remaining.split_first() else {
                        panic!("missing bool payload for has_more");
                    };
                    remaining = rest;
                    values.insert(
                        key,
                        JsonValue::Bool(match marker {
                            0xC3 => true,
                            0xC2 => false,
                            other => panic!("unexpected bool marker {other:#x}"),
                        }),
                    );
                }
                "stats" => {
                    let stats_len = decode_map_len(&mut remaining).expect("stats map");
                    assert_eq!(stats_len, 14);
                    let mut saw_zero_keys = std::collections::BTreeSet::new();
                    let mut saw_bool_keys = JsonMap::new();
                    for _ in 0..stats_len {
                        let stats_key = decode_string(&mut remaining).expect("stats key");
                        match stats_key.as_str() {
                            "nodes-created"
                            | "nodes-deleted"
                            | "relationships-created"
                            | "relationships-deleted"
                            | "properties-set"
                            | "labels-added"
                            | "labels-removed"
                            | "indexes-added"
                            | "indexes-removed"
                            | "constraints-added"
                            | "constraints-removed"
                            | "system-updates" => {
                                assert_eq!(
                                    decode_optional_i64(&mut remaining).expect("zero stat"),
                                    Some(0)
                                );
                                saw_zero_keys.insert(stats_key);
                            }
                            "contains-updates" | "contains-system-updates" => {
                                let Some((&marker, rest)) = remaining.split_first() else {
                                    panic!("missing boolean stat payload");
                                };
                                remaining = rest;
                                saw_bool_keys.insert(
                                    stats_key,
                                    JsonValue::Bool(match marker {
                                        0xC3 => true,
                                        0xC2 => false,
                                        other => panic!("unexpected bool marker {other:#x}"),
                                    }),
                                );
                            }
                            other => panic!("unexpected stats key {other}"),
                        }
                    }
                    assert_eq!(
                        saw_zero_keys,
                        [
                            "nodes-created",
                            "nodes-deleted",
                            "relationships-created",
                            "relationships-deleted",
                            "properties-set",
                            "labels-added",
                            "labels-removed",
                            "indexes-added",
                            "indexes-removed",
                            "constraints-added",
                            "constraints-removed",
                            "system-updates",
                        ]
                        .into_iter()
                        .map(str::to_owned)
                        .collect::<std::collections::BTreeSet<_>>()
                    );
                    assert_eq!(
                        saw_bool_keys.get("contains-updates"),
                        Some(&JsonValue::Bool(expected_stats.contains_updates))
                    );
                    assert_eq!(
                        saw_bool_keys.get("contains-system-updates"),
                        Some(&JsonValue::Bool(expected_stats.contains_system_updates))
                    );
                    saw_stats = true;
                }
                "statuses" => {
                    let statuses_len = decode_list_len(&mut remaining).expect("statuses len");
                    assert_eq!(statuses_len, 1);
                    let status_len = decode_map_len(&mut remaining).expect("status map");
                    assert_eq!(status_len, 5);
                    let mut gql_status = None;
                    let mut status_description = None;
                    let mut title = None;
                    let mut description = None;
                    let mut saw_diagnostic_record = false;
                    for _ in 0..status_len {
                        let status_key = decode_string(&mut remaining).expect("status key");
                        match status_key.as_str() {
                            "gql_status" => {
                                gql_status = Some(decode_string(&mut remaining).expect("gql_status"));
                            }
                            "status_description" => {
                                status_description = Some(
                                    decode_string(&mut remaining).expect("status_description"),
                                );
                            }
                            "title" => {
                                title = Some(decode_string(&mut remaining).expect("title"));
                            }
                            "description" => {
                                description =
                                    Some(decode_string(&mut remaining).expect("description"));
                            }
                            "diagnostic_record" => {
                                let diagnostic_len =
                                    decode_map_len(&mut remaining).expect("diagnostic record");
                                assert_eq!(diagnostic_len, 3);
                                for _ in 0..diagnostic_len {
                                    let _ = decode_string(&mut remaining).expect("diag key");
                                    let _ = decode_string(&mut remaining).expect("diag value");
                                }
                                saw_diagnostic_record = true;
                            }
                            other => panic!("unexpected status key {other}"),
                        }
                    }
                    assert_eq!(
                        gql_status.as_deref(),
                        Some(match expected_status {
                            BoltStatusKind::Success => "00000",
                            BoltStatusKind::OmittedResult => "00001",
                            BoltStatusKind::NoData => "02000",
                        })
                    );
                    assert_eq!(
                        status_description.as_deref(),
                        Some(match expected_status {
                            BoltStatusKind::Success => "note: successful completion",
                            BoltStatusKind::OmittedResult => {
                                "note: successful completion - omitted result"
                            }
                            BoltStatusKind::NoData => "note: no data",
                        })
                    );
                    assert_eq!(
                        title.as_deref(),
                        Some(match expected_status {
                            BoltStatusKind::Success => "Successful completion",
                            BoltStatusKind::OmittedResult => {
                                "Successful completion - omitted result"
                            }
                            BoltStatusKind::NoData => "No data",
                        })
                    );
                    assert_eq!(description, status_description);
                    assert!(saw_diagnostic_record);
                    saw_statuses = true;
                }
                "qid" => qid = decode_optional_i64(&mut remaining).expect("qid"),
                "t_last" => t_last = decode_optional_i64(&mut remaining).expect("t_last"),
                "result_consumed_after" => {
                    result_consumed_after =
                        decode_optional_i64(&mut remaining).expect("result_consumed_after")
                }
                _ => {
                    let value = decode_string(&mut remaining).expect("string summary value");
                    values.insert(key, JsonValue::String(value));
                }
            }
        }

        assert!(remaining.is_empty());
        for (key, value) in expected_pairs {
            assert_eq!(values.get(*key), Some(&JsonValue::String((*value).to_owned())));
        }
        assert_eq!(
            values.get("has_more"),
            if expected_has_more {
                Some(&JsonValue::Bool(true))
            } else {
                None
            }
        );
        assert!(saw_stats);
        assert!(saw_statuses);
        assert_eq!(qid, Some(0));
        assert!(t_last.unwrap_or(-1) >= 0);
        assert_eq!(t_last, result_consumed_after);
    }

    fn assert_failure_message(
        payload: &[u8],
        expected_code: &str,
        expected_gql_status: &str,
        expected_description: &str,
    ) {
        assert!(payload.len() >= 2);
        assert_eq!(payload[0], 0xB1);
        assert_eq!(payload[1], BOLT_FAILURE_SIGNATURE);
        let mut remaining = &payload[2..];
        let map_len = decode_map_len(&mut remaining).expect("failure map");
        assert_eq!(map_len, 6);
        let mut code = None;
        let mut neo4j_code = None;
        let mut message = None;
        let mut gql_status = None;
        let mut description = None;
        let mut saw_diagnostic_record = false;
        for _ in 0..map_len {
            let key = decode_string(&mut remaining).expect("failure key");
            match key.as_str() {
                "code" => code = Some(decode_string(&mut remaining).expect("code")),
                "neo4j_code" => {
                    neo4j_code = Some(decode_string(&mut remaining).expect("neo4j_code"))
                }
                "message" => message = Some(decode_string(&mut remaining).expect("message")),
                "gql_status" => {
                    gql_status = Some(decode_string(&mut remaining).expect("gql_status"))
                }
                "description" => {
                    description = Some(decode_string(&mut remaining).expect("description"))
                }
                "diagnostic_record" => {
                    let diagnostic_len = decode_map_len(&mut remaining).expect("diagnostic map");
                    assert_eq!(diagnostic_len, 0);
                    saw_diagnostic_record = true;
                }
                other => panic!("unexpected failure key {other}"),
            }
        }
        assert_eq!(code.as_deref(), Some(expected_code));
        assert_eq!(neo4j_code.as_deref(), Some(expected_code));
        assert!(message.is_some());
        assert_eq!(gql_status.as_deref(), Some(expected_gql_status));
        assert_eq!(description.as_deref(), Some(expected_description));
        assert!(saw_diagnostic_record);
        assert!(remaining.is_empty());
    }

    fn decode_list_len<'a>(input: &mut &'a [u8]) -> Result<usize, io::Error> {
        let Some((&marker, rest)) = input.split_first() else {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "unexpected eof in bolt list",
            ));
        };
        *input = rest;
        match marker {
            0x90..=0x9F => Ok((marker & 0x0F) as usize),
            0xD4 => {
                let (len, tail) = decode_sized_len(*input, 1)?;
                *input = tail;
                Ok(len)
            }
            0xD5 => {
                let (len, tail) = decode_sized_len(*input, 2)?;
                *input = tail;
                Ok(len)
            }
            0xD6 => {
                let (len, tail) = decode_sized_len(*input, 4)?;
                *input = tail;
                Ok(len)
            }
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "bolt value must be a list",
            )),
        }
    }

    #[test]
    fn negotiate_exact_version_match() {
        let offered = [0x0000_0105, 0, 0, 0];
        assert_eq!(negotiate_version(&offered), Some(0x0000_0105));
    }

    #[test]
    fn negotiate_minor_range_match() {
        let offered = [0x0003_0404, 0, 0, 0];
        assert_eq!(negotiate_version(&offered), Some(0x0000_0404));
    }

    #[test]
    fn negotiate_manifest_is_not_supported_yet() {
        let offered = [BOLT_MANIFEST_V1, 0x0000_0105, 0, 0];
        assert_eq!(negotiate_version(&offered), Some(0x0000_0105));
    }

    #[tokio::test]
    async fn bolt_handshake_returns_selected_version() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let engine = test_engine();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            handle_bolt_connection(stream, engine).await.expect("handshake");
        });

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect");
        let mut request = Vec::from(BOLT_MAGIC);
        request.extend_from_slice(&0x0000_0105u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        client.write_all(&request).await.expect("write handshake");
        let mut reply = [0u8; 4];
        client.read_exact(&mut reply).await.expect("read reply");
        assert_eq!(u32::from_be_bytes(reply), 0x0000_0105);
        client
            .write_all(&0u16.to_be_bytes())
            .await
            .expect("write empty message");
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn chunked_message_roundtrip() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept");
            let payload = read_chunked_message(&mut stream).await.expect("read payload");
            assert_eq!(payload, b"hello bolt");
            write_chunked_message(&mut stream, b"ack").await.expect("write ack");
        });

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect");
        write_chunked_message(&mut client, b"hello bolt")
            .await
            .expect("write payload");
        let ack = read_chunked_message(&mut client).await.expect("read ack");
        assert_eq!(ack, b"ack");
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn chunked_message_rejects_oversize_payload() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept");
            let err = read_chunked_message(&mut stream)
                .await
                .expect_err("oversize must fail");
            assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        });

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect");
        let chunk = vec![0u8; u16::MAX as usize];
        let mut sent = 0usize;
        while sent <= BOLT_MAX_MESSAGE_BYTES {
            client
                .write_all(&(chunk.len() as u16).to_be_bytes())
                .await
                .expect("write header");
            client.write_all(&chunk).await.expect("write chunk");
            sent += chunk.len();
        }
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn bolt_hello_without_auth_returns_success_message() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let engine = test_engine();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let _ = handle_bolt_connection(stream, engine).await;
        });

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect");
        let mut request = Vec::from(BOLT_MAGIC);
        request.extend_from_slice(&0x0000_0404u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        client.write_all(&request).await.expect("write handshake");

        let mut reply = [0u8; 4];
        client.read_exact(&mut reply).await.expect("read reply");
        assert_eq!(u32::from_be_bytes(reply), 0x0000_0404);

        write_chunked_message(&mut client, &[0xB1, BOLT_HELLO_SIGNATURE, 0xA0])
            .await
            .expect("write hello");
        let response = read_chunked_message(&mut client)
            .await
            .expect("read hello response");
        assert_hello_success_message(&response);
        drop(client);
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn bolt_logon_without_auth_returns_failure_message() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let engine = test_engine();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let _ = handle_bolt_connection(stream, engine).await;
        });

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect");
        let mut request = Vec::from(BOLT_MAGIC);
        request.extend_from_slice(&0x0000_0404u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        client.write_all(&request).await.expect("write handshake");
        let mut reply = [0u8; 4];
        client.read_exact(&mut reply).await.expect("read reply");
        assert_eq!(u32::from_be_bytes(reply), 0x0000_0404);

        write_chunked_message(&mut client, &[0xB1, BOLT_HELLO_SIGNATURE, 0xA0])
            .await
            .expect("write hello");
        let _ = read_chunked_message(&mut client)
            .await
            .expect("read hello response");

        write_chunked_message(&mut client, &[0xB1, BOLT_LOGON_SIGNATURE, 0xA0])
            .await
            .expect("write logon");
        let response = read_chunked_message(&mut client)
            .await
            .expect("read logon response");
        let expected = encode_failure_message(BOLT_AUTH_FAILURE_CODE, "authentication required");
        assert_eq!(response, expected);
        drop(client);
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn bolt_hello_with_basic_auth_returns_success_message() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let engine = test_engine();
        engine
            .bootstrap_role("admin", "StrongPass123!", true)
            .expect("bootstrap admin");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            handle_bolt_connection(stream, engine)
                .await
                .expect("bolt hello with auth");
        });

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect");
        let mut request = Vec::from(BOLT_MAGIC);
        request.extend_from_slice(&0x0000_0404u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        client.write_all(&request).await.expect("write handshake");

        let mut reply = [0u8; 4];
        client.read_exact(&mut reply).await.expect("read reply");
        assert_eq!(u32::from_be_bytes(reply), 0x0000_0404);

        let mut hello = vec![0xB1, BOLT_HELLO_SIGNATURE];
        encode_map_header_into(&mut hello, 4);
        encode_string_into(&mut hello, "scheme");
        encode_string_into(&mut hello, "basic");
        encode_string_into(&mut hello, "principal");
        encode_string_into(&mut hello, "admin");
        encode_string_into(&mut hello, "credentials");
        encode_string_into(&mut hello, "StrongPass123!");
        encode_string_into(&mut hello, "user_agent");
        encode_string_into(&mut hello, "neo4j/test");
        write_chunked_message(&mut client, &hello)
            .await
            .expect("write hello");
        let response = read_chunked_message(&mut client)
            .await
            .expect("read hello response");
        assert_hello_success_message(&response);
        drop(client);
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn bolt_hello_with_invalid_basic_auth_returns_failure_message() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let engine = test_engine();
        engine
            .bootstrap_role("admin", "StrongPass123!", true)
            .expect("bootstrap admin");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let _ = handle_bolt_connection(stream, engine).await;
        });

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect");
        let mut request = Vec::from(BOLT_MAGIC);
        request.extend_from_slice(&0x0000_0404u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        client.write_all(&request).await.expect("write handshake");

        let mut reply = [0u8; 4];
        client.read_exact(&mut reply).await.expect("read reply");
        assert_eq!(u32::from_be_bytes(reply), 0x0000_0404);

        let mut hello = vec![0xB1, BOLT_HELLO_SIGNATURE];
        encode_map_header_into(&mut hello, 4);
        encode_string_into(&mut hello, "scheme");
        encode_string_into(&mut hello, "basic");
        encode_string_into(&mut hello, "principal");
        encode_string_into(&mut hello, "admin");
        encode_string_into(&mut hello, "credentials");
        encode_string_into(&mut hello, "wrong");
        encode_string_into(&mut hello, "user_agent");
        encode_string_into(&mut hello, "neo4j/test");
        write_chunked_message(&mut client, &hello)
            .await
            .expect("write hello");
        let response = read_chunked_message(&mut client)
            .await
            .expect("read hello response");
        assert_failure_message(
            &response,
            BOLT_AUTH_FAILURE_CODE,
            "28000",
            "error: invalid authorization specification",
        );
        drop(client);
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn bolt_hello_rejects_unsupported_auth_scheme() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let engine = test_engine();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let _ = handle_bolt_connection(stream, engine).await;
        });

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect");
        let mut request = Vec::from(BOLT_MAGIC);
        request.extend_from_slice(&0x0000_0404u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        client.write_all(&request).await.expect("write handshake");

        let mut reply = [0u8; 4];
        client.read_exact(&mut reply).await.expect("read reply");
        assert_eq!(u32::from_be_bytes(reply), 0x0000_0404);

        let mut hello = vec![0xB1, BOLT_HELLO_SIGNATURE];
        encode_map_header_into(&mut hello, 4);
        encode_string_into(&mut hello, "scheme");
        encode_string_into(&mut hello, "bearer");
        encode_string_into(&mut hello, "principal");
        encode_string_into(&mut hello, "admin");
        encode_string_into(&mut hello, "credentials");
        encode_string_into(&mut hello, "token");
        encode_string_into(&mut hello, "user_agent");
        encode_string_into(&mut hello, "neo4j/test");
        write_chunked_message(&mut client, &hello)
            .await
            .expect("write hello");
        let response = read_chunked_message(&mut client)
            .await
            .expect("read hello response");
        assert_eq!(
            response,
            encode_failure_message(
                BOLT_AUTH_FAILURE_CODE,
                "Bolt compatibility only supports auth scheme \"basic\", got \"bearer\"",
            )
        );
        drop(client);
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn bolt_hello_rejects_incomplete_auth_metadata() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let engine = test_engine();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let _ = handle_bolt_connection(stream, engine).await;
        });

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect");
        let mut request = Vec::from(BOLT_MAGIC);
        request.extend_from_slice(&0x0000_0404u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        client.write_all(&request).await.expect("write handshake");

        let mut reply = [0u8; 4];
        client.read_exact(&mut reply).await.expect("read reply");
        assert_eq!(u32::from_be_bytes(reply), 0x0000_0404);

        let mut hello = vec![0xB1, BOLT_HELLO_SIGNATURE];
        encode_map_header_into(&mut hello, 3);
        encode_string_into(&mut hello, "scheme");
        encode_string_into(&mut hello, "basic");
        encode_string_into(&mut hello, "principal");
        encode_string_into(&mut hello, "admin");
        encode_string_into(&mut hello, "user_agent");
        encode_string_into(&mut hello, "neo4j/test");
        write_chunked_message(&mut client, &hello)
            .await
            .expect("write hello");
        let response = read_chunked_message(&mut client)
            .await
            .expect("read hello response");
        assert_eq!(
            response,
            encode_failure_message(
                BOLT_AUTH_FAILURE_CODE,
                "Bolt authentication metadata must include both principal and credentials",
            )
        );
        drop(client);
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn bolt_hello_rejects_auth_without_scheme() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let engine = test_engine();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let _ = handle_bolt_connection(stream, engine).await;
        });

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect");
        let mut request = Vec::from(BOLT_MAGIC);
        request.extend_from_slice(&0x0000_0404u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        client.write_all(&request).await.expect("write handshake");

        let mut reply = [0u8; 4];
        client.read_exact(&mut reply).await.expect("read reply");
        assert_eq!(u32::from_be_bytes(reply), 0x0000_0404);

        let mut hello = vec![0xB1, BOLT_HELLO_SIGNATURE];
        encode_map_header_into(&mut hello, 3);
        encode_string_into(&mut hello, "principal");
        encode_string_into(&mut hello, "admin");
        encode_string_into(&mut hello, "credentials");
        encode_string_into(&mut hello, "StrongPass123!");
        encode_string_into(&mut hello, "user_agent");
        encode_string_into(&mut hello, "neo4j/test");
        write_chunked_message(&mut client, &hello)
            .await
            .expect("write hello");
        let response = read_chunked_message(&mut client)
            .await
            .expect("read hello response");
        assert_eq!(
            response,
            encode_failure_message(
                BOLT_AUTH_FAILURE_CODE,
                "Bolt authentication metadata must include scheme",
            )
        );
        drop(client);
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn bolt_logon_failure_clears_existing_session() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let engine = test_engine();
        engine
            .bootstrap_role("admin", "StrongPass123!", true)
            .expect("bootstrap admin");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let _ = handle_bolt_connection(stream, engine).await;
        });

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect");
        let mut request = Vec::from(BOLT_MAGIC);
        request.extend_from_slice(&0x0000_0404u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        client.write_all(&request).await.expect("write handshake");

        let mut reply = [0u8; 4];
        client.read_exact(&mut reply).await.expect("read reply");
        assert_eq!(u32::from_be_bytes(reply), 0x0000_0404);

        let mut hello = vec![0xB1, BOLT_HELLO_SIGNATURE];
        encode_map_header_into(&mut hello, 4);
        encode_string_into(&mut hello, "scheme");
        encode_string_into(&mut hello, "basic");
        encode_string_into(&mut hello, "principal");
        encode_string_into(&mut hello, "admin");
        encode_string_into(&mut hello, "credentials");
        encode_string_into(&mut hello, "StrongPass123!");
        encode_string_into(&mut hello, "user_agent");
        encode_string_into(&mut hello, "neo4j/test");
        write_chunked_message(&mut client, &hello)
            .await
            .expect("write hello");
        let _ = read_chunked_message(&mut client)
            .await
            .expect("read hello response");

        let mut logon = vec![0xB1, BOLT_LOGON_SIGNATURE];
        encode_map_header_into(&mut logon, 3);
        encode_string_into(&mut logon, "scheme");
        encode_string_into(&mut logon, "basic");
        encode_string_into(&mut logon, "principal");
        encode_string_into(&mut logon, "admin");
        encode_string_into(&mut logon, "credentials");
        encode_string_into(&mut logon, "wrong");
        write_chunked_message(&mut client, &logon)
            .await
            .expect("write logon");
        let response = read_chunked_message(&mut client)
            .await
            .expect("read logon response");
        assert_eq!(response[1], BOLT_FAILURE_SIGNATURE);

        let reset = vec![0xB0, BOLT_RESET_SIGNATURE];
        let mut run = vec![0xB3, BOLT_RUN_SIGNATURE];
        encode_string_into(&mut run, "SELECT 1 AS n");
        encode_map_header_into(&mut run, 0);
        encode_map_header_into(&mut run, 0);
        write_chunked_message(&mut client, &run)
            .await
            .expect("write run");
        let _ = read_chunked_message(&mut client)
            .await
            .expect("read run response");

        write_chunked_message(&mut client, &reset)
            .await
            .expect("write reset");
        let reset_response = read_chunked_message(&mut client)
            .await
            .expect("read reset response");
        assert_eq!(reset_response, encode_success_message(&[]));

        let mut run = vec![0xB3, BOLT_RUN_SIGNATURE];
        encode_string_into(&mut run, "SELECT 1 AS n");
        encode_map_header_into(&mut run, 0);
        encode_map_header_into(&mut run, 0);
        write_chunked_message(&mut client, &run)
            .await
            .expect("write run");
        let run_response = read_chunked_message(&mut client)
            .await
            .expect("read run response");
        assert_eq!(
            run_response,
            encode_failure_message(BOLT_AUTH_FAILURE_CODE, "authentication required")
        );

        drop(client);
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn bolt_logon_missing_auth_clears_existing_session() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let engine = test_engine();
        engine
            .bootstrap_role("admin", "StrongPass123!", true)
            .expect("bootstrap admin");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let _ = handle_bolt_connection(stream, engine).await;
        });

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect");
        let mut request = Vec::from(BOLT_MAGIC);
        request.extend_from_slice(&0x0000_0404u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        client.write_all(&request).await.expect("write handshake");

        let mut reply = [0u8; 4];
        client.read_exact(&mut reply).await.expect("read reply");
        assert_eq!(u32::from_be_bytes(reply), 0x0000_0404);

        let mut hello = vec![0xB1, BOLT_HELLO_SIGNATURE];
        encode_map_header_into(&mut hello, 4);
        encode_string_into(&mut hello, "scheme");
        encode_string_into(&mut hello, "basic");
        encode_string_into(&mut hello, "principal");
        encode_string_into(&mut hello, "admin");
        encode_string_into(&mut hello, "credentials");
        encode_string_into(&mut hello, "StrongPass123!");
        encode_string_into(&mut hello, "user_agent");
        encode_string_into(&mut hello, "neo4j/test");
        write_chunked_message(&mut client, &hello)
            .await
            .expect("write hello");
        let _ = read_chunked_message(&mut client)
            .await
            .expect("read hello response");

        let logon = vec![0xB1, BOLT_LOGON_SIGNATURE, 0xA0];
        write_chunked_message(&mut client, &logon)
            .await
            .expect("write logon");
        let response = read_chunked_message(&mut client)
            .await
            .expect("read logon response");
        assert_eq!(
            response,
            encode_failure_message(BOLT_AUTH_FAILURE_CODE, "authentication required")
        );

        let reset = vec![0xB0, BOLT_RESET_SIGNATURE];
        write_chunked_message(&mut client, &reset)
            .await
            .expect("write reset");
        let reset_response = read_chunked_message(&mut client)
            .await
            .expect("read reset response");
        assert_eq!(reset_response, encode_success_message(&[]));

        let mut run = vec![0xB3, BOLT_RUN_SIGNATURE];
        encode_string_into(&mut run, "SELECT 1 AS n");
        encode_map_header_into(&mut run, 0);
        encode_map_header_into(&mut run, 0);
        write_chunked_message(&mut client, &run)
            .await
            .expect("write run");
        let run_response = read_chunked_message(&mut client)
            .await
            .expect("read run response");
        assert_eq!(
            run_response,
            encode_failure_message(BOLT_AUTH_FAILURE_CODE, "authentication required")
        );

        drop(client);
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn bolt_logon_invalid_metadata_clears_existing_session() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let engine = test_engine();
        engine
            .bootstrap_role("admin", "StrongPass123!", true)
            .expect("bootstrap admin");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let _ = handle_bolt_connection(stream, engine).await;
        });

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect");
        let mut request = Vec::from(BOLT_MAGIC);
        request.extend_from_slice(&0x0000_0404u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        client.write_all(&request).await.expect("write handshake");

        let mut reply = [0u8; 4];
        client.read_exact(&mut reply).await.expect("read reply");
        assert_eq!(u32::from_be_bytes(reply), 0x0000_0404);

        let mut hello = vec![0xB1, BOLT_HELLO_SIGNATURE];
        encode_map_header_into(&mut hello, 4);
        encode_string_into(&mut hello, "scheme");
        encode_string_into(&mut hello, "basic");
        encode_string_into(&mut hello, "principal");
        encode_string_into(&mut hello, "admin");
        encode_string_into(&mut hello, "credentials");
        encode_string_into(&mut hello, "StrongPass123!");
        encode_string_into(&mut hello, "user_agent");
        encode_string_into(&mut hello, "neo4j/test");
        write_chunked_message(&mut client, &hello)
            .await
            .expect("write hello");
        let _ = read_chunked_message(&mut client)
            .await
            .expect("read hello response");

        let mut logon = vec![0xB1, BOLT_LOGON_SIGNATURE];
        encode_map_header_into(&mut logon, 4);
        encode_string_into(&mut logon, "scheme");
        encode_string_into(&mut logon, "basic");
        encode_string_into(&mut logon, "principal");
        encode_string_into(&mut logon, "admin");
        encode_string_into(&mut logon, "credentials");
        encode_string_into(&mut logon, "StrongPass123!");
        encode_string_into(&mut logon, "notifications_minimum_severity");
        encode_i64_into(&mut logon, 42);
        write_chunked_message(&mut client, &logon)
            .await
            .expect("write logon");
        let response = read_chunked_message(&mut client)
            .await
            .expect("read logon response");
        assert_eq!(
            response,
            encode_failure_message(
                BOLT_REQUEST_FAILURE_CODE,
                "Bolt notifications_minimum_severity metadata must be a string",
            )
        );

        let reset = vec![0xB0, BOLT_RESET_SIGNATURE];
        write_chunked_message(&mut client, &reset)
            .await
            .expect("write reset");
        let reset_response = read_chunked_message(&mut client)
            .await
            .expect("read reset response");
        assert_eq!(reset_response, encode_success_message(&[]));

        let mut run = vec![0xB3, BOLT_RUN_SIGNATURE];
        encode_string_into(&mut run, "SELECT 1 AS n");
        encode_map_header_into(&mut run, 0);
        encode_map_header_into(&mut run, 0);
        write_chunked_message(&mut client, &run)
            .await
            .expect("write run");
        let run_response = read_chunked_message(&mut client)
            .await
            .expect("read run response");
        assert_eq!(
            run_response,
            encode_failure_message(BOLT_AUTH_FAILURE_CODE, "authentication required")
        );

        drop(client);
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn bolt_logon_malformed_message_clears_existing_session() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let engine = test_engine();
        engine
            .bootstrap_role("admin", "StrongPass123!", true)
            .expect("bootstrap admin");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let _ = handle_bolt_connection(stream, engine).await;
        });

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect");
        let mut request = Vec::from(BOLT_MAGIC);
        request.extend_from_slice(&0x0000_0404u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        client.write_all(&request).await.expect("write handshake");

        let mut reply = [0u8; 4];
        client.read_exact(&mut reply).await.expect("read reply");
        assert_eq!(u32::from_be_bytes(reply), 0x0000_0404);

        let mut hello = vec![0xB1, BOLT_HELLO_SIGNATURE];
        encode_map_header_into(&mut hello, 4);
        encode_string_into(&mut hello, "scheme");
        encode_string_into(&mut hello, "basic");
        encode_string_into(&mut hello, "principal");
        encode_string_into(&mut hello, "admin");
        encode_string_into(&mut hello, "credentials");
        encode_string_into(&mut hello, "StrongPass123!");
        encode_string_into(&mut hello, "user_agent");
        encode_string_into(&mut hello, "neo4j/test");
        write_chunked_message(&mut client, &hello)
            .await
            .expect("write hello");
        let _ = read_chunked_message(&mut client)
            .await
            .expect("read hello response");

        let mut logon = vec![0xB1, BOLT_LOGON_SIGNATURE];
        encode_map_header_into(&mut logon, 1);
        encode_string_into(&mut logon, "principal");
        encode_i64_into(&mut logon, 42);
        write_chunked_message(&mut client, &logon)
            .await
            .expect("write logon");
        let response = read_chunked_message(&mut client)
            .await
            .expect("read logon response");
        assert_eq!(
            response,
            encode_failure_message(BOLT_REQUEST_FAILURE_CODE, "invalid LOGON message")
        );

        let reset = vec![0xB0, BOLT_RESET_SIGNATURE];
        write_chunked_message(&mut client, &reset)
            .await
            .expect("write reset");
        let reset_response = read_chunked_message(&mut client)
            .await
            .expect("read reset response");
        assert_eq!(reset_response, encode_success_message(&[]));

        let mut run = vec![0xB3, BOLT_RUN_SIGNATURE];
        encode_string_into(&mut run, "SELECT 1 AS n");
        encode_map_header_into(&mut run, 0);
        encode_map_header_into(&mut run, 0);
        write_chunked_message(&mut client, &run)
            .await
            .expect("write run");
        let run_response = read_chunked_message(&mut client)
            .await
            .expect("read run response");
        assert_eq!(
            run_response,
            encode_failure_message(BOLT_AUTH_FAILURE_CODE, "authentication required")
        );

        drop(client);
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn bolt_logoff_rejects_non_empty_payload() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let engine = test_engine();
        engine
            .bootstrap_role("admin", "StrongPass123!", true)
            .expect("bootstrap admin");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let _ = handle_bolt_connection(stream, engine).await;
        });

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect");
        let mut request = Vec::from(BOLT_MAGIC);
        request.extend_from_slice(&0x0000_0404u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        client.write_all(&request).await.expect("write handshake");

        let mut reply = [0u8; 4];
        client.read_exact(&mut reply).await.expect("read reply");
        assert_eq!(u32::from_be_bytes(reply), 0x0000_0404);

        let mut hello = vec![0xB1, BOLT_HELLO_SIGNATURE];
        encode_map_header_into(&mut hello, 4);
        encode_string_into(&mut hello, "scheme");
        encode_string_into(&mut hello, "basic");
        encode_string_into(&mut hello, "principal");
        encode_string_into(&mut hello, "admin");
        encode_string_into(&mut hello, "credentials");
        encode_string_into(&mut hello, "StrongPass123!");
        encode_string_into(&mut hello, "user_agent");
        encode_string_into(&mut hello, "neo4j/test");
        write_chunked_message(&mut client, &hello)
            .await
            .expect("write hello");
        let _ = read_chunked_message(&mut client)
            .await
            .expect("read hello response");

        let mut logoff = vec![0xB1, BOLT_LOGOFF_SIGNATURE];
        encode_map_header_into(&mut logoff, 0);
        write_chunked_message(&mut client, &logoff)
            .await
            .expect("write logoff");
        let response = read_chunked_message(&mut client)
            .await
            .expect("read logoff response");
        assert_eq!(
            response,
            encode_failure_message(
                BOLT_REQUEST_FAILURE_CODE,
                "Bolt LOGOFF must be a zero-field struct",
            )
        );

        drop(client);
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn bolt_reset_clears_pending_query_inside_transaction() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let engine = test_engine();
        engine
            .bootstrap_role("admin", "StrongPass123!", true)
            .expect("bootstrap admin");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let _ = handle_bolt_connection(stream, engine).await;
        });

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect");
        let mut request = Vec::from(BOLT_MAGIC);
        request.extend_from_slice(&0x0000_0404u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        client.write_all(&request).await.expect("write handshake");

        let mut reply = [0u8; 4];
        client.read_exact(&mut reply).await.expect("read reply");
        assert_eq!(u32::from_be_bytes(reply), 0x0000_0404);

        let mut hello = vec![0xB1, BOLT_HELLO_SIGNATURE];
        encode_map_header_into(&mut hello, 4);
        encode_string_into(&mut hello, "scheme");
        encode_string_into(&mut hello, "basic");
        encode_string_into(&mut hello, "principal");
        encode_string_into(&mut hello, "admin");
        encode_string_into(&mut hello, "credentials");
        encode_string_into(&mut hello, "StrongPass123!");
        encode_string_into(&mut hello, "user_agent");
        encode_string_into(&mut hello, "neo4j/test");
        write_chunked_message(&mut client, &hello)
            .await
            .expect("write hello");
        let _ = read_chunked_message(&mut client)
            .await
            .expect("read hello response");

        let mut begin = vec![0xB1, BOLT_BEGIN_SIGNATURE];
        encode_map_header_into(&mut begin, 0);
        write_chunked_message(&mut client, &begin)
            .await
            .expect("write begin");
        let begin_response = read_chunked_message(&mut client)
            .await
            .expect("read begin response");
        assert_eq!(begin_response, encode_success_message(&[]));

        let reset = vec![0xB0, BOLT_RESET_SIGNATURE];
        write_chunked_message(&mut client, &reset)
            .await
            .expect("write reset");
        let reset_response = read_chunked_message(&mut client)
            .await
            .expect("read reset response");
        assert_eq!(reset_response, encode_success_message(&[]));

        let mut pull = vec![0xB1, BOLT_PULL_SIGNATURE];
        encode_map_header_into(&mut pull, 0);
        write_chunked_message(&mut client, &pull)
            .await
            .expect("write pull");
        let pull_response = read_chunked_message(&mut client)
            .await
            .expect("read pull response");
        assert_eq!(
            pull_response,
            encode_failure_message(BOLT_REQUEST_FAILURE_CODE, "no pending result to pull")
        );

        drop(client);
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn bolt_hello_rejects_non_string_notification_severity() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let engine = test_engine();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let _ = handle_bolt_connection(stream, engine).await;
        });

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect");
        let mut request = Vec::from(BOLT_MAGIC);
        request.extend_from_slice(&0x0000_0404u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        client.write_all(&request).await.expect("write handshake");

        let mut reply = [0u8; 4];
        client.read_exact(&mut reply).await.expect("read reply");
        assert_eq!(u32::from_be_bytes(reply), 0x0000_0404);

        let mut hello = vec![0xB1, BOLT_HELLO_SIGNATURE];
        encode_map_header_into(&mut hello, 1);
        encode_string_into(&mut hello, "notifications_minimum_severity");
        encode_i64_into(&mut hello, 42);
        write_chunked_message(&mut client, &hello)
            .await
            .expect("write hello");
        let response = read_chunked_message(&mut client)
            .await
            .expect("read hello response");
        assert_eq!(
            response,
            encode_failure_message(
                BOLT_REQUEST_FAILURE_CODE,
                "Bolt notifications_minimum_severity metadata must be a string",
            )
        );
        drop(client);
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn bolt_hello_rejects_session_auth_metadata() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let engine = test_engine();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let _ = handle_bolt_connection(stream, engine).await;
        });

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect");
        let mut request = Vec::from(BOLT_MAGIC);
        request.extend_from_slice(&0x0000_0404u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        client.write_all(&request).await.expect("write handshake");

        let mut reply = [0u8; 4];
        client.read_exact(&mut reply).await.expect("read reply");
        assert_eq!(u32::from_be_bytes(reply), 0x0000_0404);

        let mut hello = vec![0xB1, BOLT_HELLO_SIGNATURE];
        encode_map_header_into(&mut hello, 1);
        encode_string_into(&mut hello, "session_auth");
        encode_map_header_into(&mut hello, 0);
        write_chunked_message(&mut client, &hello)
            .await
            .expect("write hello");
        let response = read_chunked_message(&mut client)
            .await
            .expect("read hello response");
        assert_eq!(
            response,
            encode_failure_message(
                BOLT_REQUEST_FAILURE_CODE,
                "Bolt compatibility does not support session_auth metadata",
            )
        );
        drop(client);
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn bolt_hello_rejects_db_metadata() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let engine = test_engine();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let _ = handle_bolt_connection(stream, engine).await;
        });

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect");
        let mut request = Vec::from(BOLT_MAGIC);
        request.extend_from_slice(&0x0000_0404u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        client.write_all(&request).await.expect("write handshake");

        let mut reply = [0u8; 4];
        client.read_exact(&mut reply).await.expect("read reply");
        assert_eq!(u32::from_be_bytes(reply), 0x0000_0404);

        let mut hello = vec![0xB1, BOLT_HELLO_SIGNATURE];
        encode_map_header_into(&mut hello, 1);
        encode_string_into(&mut hello, "db");
        encode_string_into(&mut hello, "default");
        write_chunked_message(&mut client, &hello)
            .await
            .expect("write hello");
        let response = read_chunked_message(&mut client)
            .await
            .expect("read hello response");
        assert_eq!(
            response,
            encode_failure_message(
                BOLT_REQUEST_FAILURE_CODE,
                "Bolt compatibility does not support db metadata in HELLO/LOGON",
            )
        );
        drop(client);
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn bolt_hello_rejects_unknown_metadata_key() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let engine = test_engine();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let _ = handle_bolt_connection(stream, engine).await;
        });

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect");
        let mut request = Vec::from(BOLT_MAGIC);
        request.extend_from_slice(&0x0000_0404u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        client.write_all(&request).await.expect("write handshake");

        let mut reply = [0u8; 4];
        client.read_exact(&mut reply).await.expect("read reply");
        assert_eq!(u32::from_be_bytes(reply), 0x0000_0404);

        let mut hello = vec![0xB1, BOLT_HELLO_SIGNATURE];
        encode_map_header_into(&mut hello, 2);
        encode_string_into(&mut hello, "user_agent");
        encode_string_into(&mut hello, "neo4j/test");
        encode_string_into(&mut hello, "foo");
        encode_string_into(&mut hello, "bar");
        write_chunked_message(&mut client, &hello)
            .await
            .expect("write hello");
        let response = read_chunked_message(&mut client)
            .await
            .expect("read hello response");
        assert_eq!(
            response,
            encode_failure_message(
                BOLT_REQUEST_FAILURE_CODE,
                "unsupported Bolt metadata key \"foo\" in HELLO/LOGON",
            )
        );

        drop(client);
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn bolt_telemetry_message_returns_success() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let engine = test_engine();
        engine
            .bootstrap_role("admin", "StrongPass123!", true)
            .expect("bootstrap admin");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            handle_bolt_connection(stream, engine)
                .await
                .expect("bolt telemetry");
        });

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect");
        let mut request = Vec::from(BOLT_MAGIC);
        request.extend_from_slice(&0x0000_0404u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        client.write_all(&request).await.expect("write handshake");

        let mut reply = [0u8; 4];
        client.read_exact(&mut reply).await.expect("read reply");
        assert_eq!(u32::from_be_bytes(reply), 0x0000_0404);

        let mut hello = vec![0xB1, BOLT_HELLO_SIGNATURE];
        encode_map_header_into(&mut hello, 4);
        encode_string_into(&mut hello, "scheme");
        encode_string_into(&mut hello, "basic");
        encode_string_into(&mut hello, "principal");
        encode_string_into(&mut hello, "admin");
        encode_string_into(&mut hello, "credentials");
        encode_string_into(&mut hello, "StrongPass123!");
        encode_string_into(&mut hello, "user_agent");
        encode_string_into(&mut hello, "neo4j/test");
        write_chunked_message(&mut client, &hello)
            .await
            .expect("write hello");
        let hello_response = read_chunked_message(&mut client)
            .await
            .expect("read hello response");
        assert_hello_success_message(&hello_response);

        let mut telemetry = vec![0xB1, BOLT_TELEMETRY_SIGNATURE];
        encode_i64_into(&mut telemetry, 0);
        write_chunked_message(&mut client, &telemetry)
            .await
            .expect("write telemetry");
        let telemetry_response = read_chunked_message(&mut client)
            .await
            .expect("read telemetry response");
        assert_eq!(telemetry_response, encode_success_message(&[]));

        drop(client);
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn bolt_run_pull_returns_query_records() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let engine = test_engine();
        engine
            .bootstrap_role("admin", "StrongPass123!", true)
            .expect("bootstrap admin");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            handle_bolt_connection(stream, engine)
                .await
                .expect("bolt run pull");
        });

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect");
        let mut request = Vec::from(BOLT_MAGIC);
        request.extend_from_slice(&0x0000_0404u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        client.write_all(&request).await.expect("write handshake");

        let mut reply = [0u8; 4];
        client.read_exact(&mut reply).await.expect("read reply");
        assert_eq!(u32::from_be_bytes(reply), 0x0000_0404);

        let mut hello = vec![0xB1, BOLT_HELLO_SIGNATURE];
        encode_map_header_into(&mut hello, 4);
        encode_string_into(&mut hello, "scheme");
        encode_string_into(&mut hello, "basic");
        encode_string_into(&mut hello, "principal");
        encode_string_into(&mut hello, "admin");
        encode_string_into(&mut hello, "credentials");
        encode_string_into(&mut hello, "StrongPass123!");
        encode_string_into(&mut hello, "user_agent");
        encode_string_into(&mut hello, "neo4j/test");
        write_chunked_message(&mut client, &hello)
            .await
            .expect("write hello");
        let hello_response = read_chunked_message(&mut client)
            .await
            .expect("read hello response");
        assert_hello_success_message(&hello_response);

        let mut run = vec![0xB3, BOLT_RUN_SIGNATURE];
        encode_string_into(&mut run, "SELECT 1 AS n");
        encode_map_header_into(&mut run, 0);
        encode_map_header_into(&mut run, 0);
        write_chunked_message(&mut client, &run)
            .await
            .expect("write run");
        let run_response = read_chunked_message(&mut client)
            .await
            .expect("read run success");
        assert_run_success_message(&run_response, &["n"], 0, BOLT_DEFAULT_DATABASE);

        let mut pull = vec![0xB1, BOLT_PULL_SIGNATURE];
        encode_map_header_into(&mut pull, 0);
        write_chunked_message(&mut client, &pull)
            .await
            .expect("write pull");
        let record = read_chunked_message(&mut client)
            .await
            .expect("read record");
        assert_eq!(record, encode_record_message(&[aiondb_engine::Value::Int(1)]).unwrap());
        let summary = read_chunked_message(&mut client)
            .await
            .expect("read summary");
        assert_pull_success_message(
            &summary,
            &[("type", "r"), ("db", BOLT_DEFAULT_DATABASE)],
            false,
            BoltStatusKind::Success,
        );
        drop(client);
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn bolt_run_write_mode_returns_update_summary_and_persists_changes() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let engine = test_engine();
        engine
            .bootstrap_role("admin", "StrongPass123!", true)
            .expect("bootstrap admin");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            handle_bolt_connection(stream, engine)
                .await
                .expect("bolt write mode");
        });

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect");
        let mut request = Vec::from(BOLT_MAGIC);
        request.extend_from_slice(&0x0000_0404u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        client.write_all(&request).await.expect("write handshake");

        let mut reply = [0u8; 4];
        client.read_exact(&mut reply).await.expect("read reply");
        assert_eq!(u32::from_be_bytes(reply), 0x0000_0404);

        let mut hello = vec![0xB1, BOLT_HELLO_SIGNATURE];
        encode_map_header_into(&mut hello, 4);
        encode_string_into(&mut hello, "scheme");
        encode_string_into(&mut hello, "basic");
        encode_string_into(&mut hello, "principal");
        encode_string_into(&mut hello, "admin");
        encode_string_into(&mut hello, "credentials");
        encode_string_into(&mut hello, "StrongPass123!");
        encode_string_into(&mut hello, "user_agent");
        encode_string_into(&mut hello, "neo4j/test");
        write_chunked_message(&mut client, &hello)
            .await
            .expect("write hello");
        let _ = read_chunked_message(&mut client)
            .await
            .expect("read hello response");

        let mut run = vec![0xB3, BOLT_RUN_SIGNATURE];
        encode_string_into(&mut run, "CREATE TABLE bolt_write_probe (id INT)");
        encode_map_header_into(&mut run, 0);
        encode_map_header_into(&mut run, 1);
        encode_string_into(&mut run, "mode");
        encode_string_into(&mut run, "write");
        write_chunked_message(&mut client, &run)
            .await
            .expect("write run create");
        let run_response = read_chunked_message(&mut client)
            .await
            .expect("read run create success");
        assert_run_success_message(&run_response, &[], 0, BOLT_DEFAULT_DATABASE);

        let mut pull = vec![0xB1, BOLT_PULL_SIGNATURE];
        encode_map_header_into(&mut pull, 0);
        write_chunked_message(&mut client, &pull)
            .await
            .expect("write pull create");
        let create_summary = read_chunked_message(&mut client)
            .await
            .expect("read create summary");
        assert_pull_success_message_with_stats(
            &create_summary,
            &[
                ("type", "w"),
                ("db", BOLT_DEFAULT_DATABASE),
                ("bookmark", "aiondb:bookmark:1"),
            ],
            false,
            BoltStatusKind::NoData,
            &BoltQueryStats {
                contains_updates: true,
                contains_system_updates: false,
            },
        );

        let mut insert = vec![0xB3, BOLT_RUN_SIGNATURE];
        encode_string_into(&mut insert, "INSERT INTO bolt_write_probe VALUES (7)");
        encode_map_header_into(&mut insert, 0);
        encode_map_header_into(&mut insert, 1);
        encode_string_into(&mut insert, "mode");
        encode_string_into(&mut insert, "w");
        write_chunked_message(&mut client, &insert)
            .await
            .expect("write run insert");
        let insert_run_response = read_chunked_message(&mut client)
            .await
            .expect("read run insert success");
        assert_run_success_message(&insert_run_response, &[], 0, BOLT_DEFAULT_DATABASE);
        write_chunked_message(&mut client, &pull)
            .await
            .expect("write pull insert");
        let insert_summary = read_chunked_message(&mut client)
            .await
            .expect("read insert summary");
        assert_pull_success_message_with_stats(
            &insert_summary,
            &[
                ("type", "w"),
                ("db", BOLT_DEFAULT_DATABASE),
                ("bookmark", "aiondb:bookmark:2"),
            ],
            false,
            BoltStatusKind::NoData,
            &BoltQueryStats {
                contains_updates: true,
                contains_system_updates: false,
            },
        );

        let mut select = vec![0xB3, BOLT_RUN_SIGNATURE];
        encode_string_into(&mut select, "SELECT id FROM bolt_write_probe");
        encode_map_header_into(&mut select, 0);
        encode_map_header_into(&mut select, 0);
        write_chunked_message(&mut client, &select)
            .await
            .expect("write run select");
        let select_run_response = read_chunked_message(&mut client)
            .await
            .expect("read run select success");
        assert_run_success_message(&select_run_response, &["id"], 0, BOLT_DEFAULT_DATABASE);
        write_chunked_message(&mut client, &pull)
            .await
            .expect("write pull select");
        let record = read_chunked_message(&mut client)
            .await
            .expect("read select record");
        assert_eq!(record, encode_record_message(&[aiondb_engine::Value::Int(7)]).unwrap());
        let select_summary = read_chunked_message(&mut client)
            .await
            .expect("read select summary");
        assert_pull_success_message(
            &select_summary,
            &[
                ("type", "r"),
                ("db", BOLT_DEFAULT_DATABASE),
                ("bookmark", "aiondb:bookmark:3"),
            ],
            false,
            BoltStatusKind::Success,
        );

        drop(client);
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn bolt_run_rejects_transaction_control_statement() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let engine = test_engine();
        engine
            .bootstrap_role("admin", "StrongPass123!", true)
            .expect("bootstrap admin");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            handle_bolt_connection(stream, engine)
                .await
                .expect("bolt reject begin via run");
        });

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect");
        let mut request = Vec::from(BOLT_MAGIC);
        request.extend_from_slice(&0x0000_0404u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        client.write_all(&request).await.expect("write handshake");
        let mut reply = [0u8; 4];
        client.read_exact(&mut reply).await.expect("read reply");
        assert_eq!(u32::from_be_bytes(reply), 0x0000_0404);

        let mut hello = vec![0xB1, BOLT_HELLO_SIGNATURE];
        encode_map_header_into(&mut hello, 4);
        encode_string_into(&mut hello, "scheme");
        encode_string_into(&mut hello, "basic");
        encode_string_into(&mut hello, "principal");
        encode_string_into(&mut hello, "admin");
        encode_string_into(&mut hello, "credentials");
        encode_string_into(&mut hello, "StrongPass123!");
        encode_string_into(&mut hello, "user_agent");
        encode_string_into(&mut hello, "neo4j/test");
        write_chunked_message(&mut client, &hello)
            .await
            .expect("write hello");
        let _ = read_chunked_message(&mut client)
            .await
            .expect("read hello response");

        let mut run = vec![0xB3, BOLT_RUN_SIGNATURE];
        encode_string_into(&mut run, "BEGIN");
        encode_map_header_into(&mut run, 0);
        encode_map_header_into(&mut run, 0);
        write_chunked_message(&mut client, &run)
            .await
            .expect("write run");
        let response = read_chunked_message(&mut client)
            .await
            .expect("read failure");
        assert_failure_message(
            &response,
            BOLT_FORBIDDEN_FAILURE_CODE,
            "28000",
            "error: invalid authorization specification",
        );
        let mut remaining = &response[2..];
        let map_len = decode_map_len(&mut remaining).expect("failure map");
        let mut message = None;
        for _ in 0..map_len {
            let key = decode_string(&mut remaining).expect("failure key");
            match key.as_str() {
                "message" => message = Some(decode_string(&mut remaining).expect("message")),
                "diagnostic_record" => {
                    let diagnostic_len = decode_map_len(&mut remaining).expect("diagnostic map");
                    for _ in 0..diagnostic_len {
                        let _ = decode_string(&mut remaining).expect("diag key");
                        let _ = decode_string(&mut remaining).expect("diag value");
                    }
                }
                _ => {
                    let _ = decode_string(&mut remaining).expect("string failure value");
                }
            }
        }
        let message = message.expect("failure message");
        assert!(message.contains("transaction-control or copy statements in RUN"));
        assert!(message.contains("sql=\"BEGIN\""));

        drop(client);
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn bolt_pull_no_data_returns_no_data_status() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let engine = test_engine();
        engine
            .bootstrap_role("admin", "StrongPass123!", true)
            .expect("bootstrap admin");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            handle_bolt_connection(stream, engine)
                .await
                .expect("bolt no data");
        });

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect");
        let mut request = Vec::from(BOLT_MAGIC);
        request.extend_from_slice(&0x0000_0404u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        client.write_all(&request).await.expect("write handshake");

        let mut reply = [0u8; 4];
        client.read_exact(&mut reply).await.expect("read reply");
        assert_eq!(u32::from_be_bytes(reply), 0x0000_0404);

        let mut hello = vec![0xB1, BOLT_HELLO_SIGNATURE];
        encode_map_header_into(&mut hello, 4);
        encode_string_into(&mut hello, "scheme");
        encode_string_into(&mut hello, "basic");
        encode_string_into(&mut hello, "principal");
        encode_string_into(&mut hello, "admin");
        encode_string_into(&mut hello, "credentials");
        encode_string_into(&mut hello, "StrongPass123!");
        encode_string_into(&mut hello, "user_agent");
        encode_string_into(&mut hello, "neo4j/test");
        write_chunked_message(&mut client, &hello)
            .await
            .expect("write hello");
        let hello_response = read_chunked_message(&mut client)
            .await
            .expect("read hello response");
        assert_hello_success_message(&hello_response);

        let mut run = vec![0xB3, BOLT_RUN_SIGNATURE];
        encode_string_into(&mut run, "SELECT 1 AS n WHERE FALSE");
        encode_map_header_into(&mut run, 0);
        encode_map_header_into(&mut run, 0);
        write_chunked_message(&mut client, &run)
            .await
            .expect("write run");
        let run_response = read_chunked_message(&mut client)
            .await
            .expect("read run success");
        assert_run_success_message(&run_response, &["n"], 0, BOLT_DEFAULT_DATABASE);

        let mut pull = vec![0xB1, BOLT_PULL_SIGNATURE];
        encode_map_header_into(&mut pull, 0);
        write_chunked_message(&mut client, &pull)
            .await
            .expect("write pull");
        let summary = read_chunked_message(&mut client)
            .await
            .expect("read summary");
        assert_pull_success_message(
            &summary,
            &[("type", "r"), ("db", BOLT_DEFAULT_DATABASE)],
            false,
            BoltStatusKind::NoData,
        );

        drop(client);
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn bolt_discard_returns_omitted_result_status() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let engine = test_engine();
        engine
            .bootstrap_role("admin", "StrongPass123!", true)
            .expect("bootstrap admin");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            handle_bolt_connection(stream, engine)
                .await
                .expect("bolt discard");
        });

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect");
        let mut request = Vec::from(BOLT_MAGIC);
        request.extend_from_slice(&0x0000_0404u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        client.write_all(&request).await.expect("write handshake");

        let mut reply = [0u8; 4];
        client.read_exact(&mut reply).await.expect("read reply");
        assert_eq!(u32::from_be_bytes(reply), 0x0000_0404);

        let mut hello = vec![0xB1, BOLT_HELLO_SIGNATURE];
        encode_map_header_into(&mut hello, 4);
        encode_string_into(&mut hello, "scheme");
        encode_string_into(&mut hello, "basic");
        encode_string_into(&mut hello, "principal");
        encode_string_into(&mut hello, "admin");
        encode_string_into(&mut hello, "credentials");
        encode_string_into(&mut hello, "StrongPass123!");
        encode_string_into(&mut hello, "user_agent");
        encode_string_into(&mut hello, "neo4j/test");
        write_chunked_message(&mut client, &hello)
            .await
            .expect("write hello");
        let hello_response = read_chunked_message(&mut client)
            .await
            .expect("read hello response");
        assert_hello_success_message(&hello_response);

        let mut run = vec![0xB3, BOLT_RUN_SIGNATURE];
        encode_string_into(&mut run, "SELECT 1 AS n");
        encode_map_header_into(&mut run, 0);
        encode_map_header_into(&mut run, 0);
        write_chunked_message(&mut client, &run)
            .await
            .expect("write run");
        let run_response = read_chunked_message(&mut client)
            .await
            .expect("read run success");
        assert_run_success_message(&run_response, &["n"], 0, BOLT_DEFAULT_DATABASE);

        let mut discard = vec![0xB1, BOLT_DISCARD_SIGNATURE];
        encode_map_header_into(&mut discard, 0);
        write_chunked_message(&mut client, &discard)
            .await
            .expect("write discard");
        let summary = read_chunked_message(&mut client)
            .await
            .expect("read discard summary");
        assert_pull_success_message(
            &summary,
            &[
                ("type", "r"),
                ("db", BOLT_DEFAULT_DATABASE),
                ("bookmark", "aiondb:bookmark:1"),
            ],
            false,
            BoltStatusKind::OmittedResult,
        );

        drop(client);
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn bolt_pull_n_pages_records() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let engine = test_engine();
        engine
            .bootstrap_role("admin", "StrongPass123!", true)
            .expect("bootstrap admin");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            handle_bolt_connection(stream, engine)
                .await
                .expect("bolt paged pull");
        });

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect");
        let mut request = Vec::from(BOLT_MAGIC);
        request.extend_from_slice(&0x0000_0404u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        client.write_all(&request).await.expect("write handshake");
        let mut reply = [0u8; 4];
        client.read_exact(&mut reply).await.expect("read reply");
        assert_eq!(u32::from_be_bytes(reply), 0x0000_0404);

        let mut hello = vec![0xB1, BOLT_HELLO_SIGNATURE];
        encode_map_header_into(&mut hello, 4);
        encode_string_into(&mut hello, "scheme");
        encode_string_into(&mut hello, "basic");
        encode_string_into(&mut hello, "principal");
        encode_string_into(&mut hello, "admin");
        encode_string_into(&mut hello, "credentials");
        encode_string_into(&mut hello, "StrongPass123!");
        encode_string_into(&mut hello, "user_agent");
        encode_string_into(&mut hello, "neo4j/test");
        write_chunked_message(&mut client, &hello)
            .await
            .expect("write hello");
        let _ = read_chunked_message(&mut client)
            .await
            .expect("read hello response");

        let mut run = vec![0xB3, BOLT_RUN_SIGNATURE];
        encode_string_into(&mut run, "SELECT 1 AS n UNION ALL SELECT 2 AS n");
        encode_map_header_into(&mut run, 0);
        encode_map_header_into(&mut run, 0);
        write_chunked_message(&mut client, &run)
            .await
            .expect("write run");
        let _ = read_chunked_message(&mut client)
            .await
            .expect("read run success");

        let mut pull = vec![0xB1, BOLT_PULL_SIGNATURE];
        encode_map_header_into(&mut pull, 1);
        encode_string_into(&mut pull, "n");
        encode_i64_into(&mut pull, 1);
        write_chunked_message(&mut client, &pull)
            .await
            .expect("write pull 1");
        let record1 = read_chunked_message(&mut client)
            .await
            .expect("read record1");
        assert_eq!(record1, encode_record_message(&[aiondb_engine::Value::Int(1)]).unwrap());
        let summary1 = read_chunked_message(&mut client)
            .await
            .expect("read summary1");
        assert_pull_success_message(
            &summary1,
            &[("type", "r"), ("db", BOLT_DEFAULT_DATABASE)],
            true,
            BoltStatusKind::Success,
        );

        write_chunked_message(&mut client, &pull)
            .await
            .expect("write pull 2");
        let record2 = read_chunked_message(&mut client)
            .await
            .expect("read record2");
        assert_eq!(record2, encode_record_message(&[aiondb_engine::Value::Int(2)]).unwrap());
        let summary2 = read_chunked_message(&mut client)
            .await
            .expect("read summary2");
        assert_pull_success_message(
            &summary2,
            &[
                ("type", "r"),
                ("db", BOLT_DEFAULT_DATABASE),
                ("bookmark", "aiondb:bookmark:1"),
            ],
            false,
            BoltStatusKind::Success,
        );

        drop(client);
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn bolt_run_with_named_parameter_returns_record() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let engine = test_engine();
        engine
            .bootstrap_role("admin", "StrongPass123!", true)
            .expect("bootstrap admin");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            handle_bolt_connection(stream, engine)
                .await
                .expect("bolt run with param");
        });

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect");
        let mut request = Vec::from(BOLT_MAGIC);
        request.extend_from_slice(&0x0000_0404u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        client.write_all(&request).await.expect("write handshake");
        let mut reply = [0u8; 4];
        client.read_exact(&mut reply).await.expect("read reply");
        assert_eq!(u32::from_be_bytes(reply), 0x0000_0404);

        let mut hello = vec![0xB1, BOLT_HELLO_SIGNATURE];
        encode_map_header_into(&mut hello, 4);
        encode_string_into(&mut hello, "scheme");
        encode_string_into(&mut hello, "basic");
        encode_string_into(&mut hello, "principal");
        encode_string_into(&mut hello, "admin");
        encode_string_into(&mut hello, "credentials");
        encode_string_into(&mut hello, "StrongPass123!");
        encode_string_into(&mut hello, "user_agent");
        encode_string_into(&mut hello, "neo4j/test");
        write_chunked_message(&mut client, &hello)
            .await
            .expect("write hello");
        let _ = read_chunked_message(&mut client)
            .await
            .expect("read hello response");

        let mut run = vec![0xB3, BOLT_RUN_SIGNATURE];
        encode_string_into(&mut run, "SELECT $n AS n");
        encode_map_header_into(&mut run, 1);
        encode_string_into(&mut run, "n");
        encode_i64_into(&mut run, 7);
        encode_map_header_into(&mut run, 0);
        write_chunked_message(&mut client, &run)
            .await
            .expect("write run");
        let run_response = read_chunked_message(&mut client)
            .await
            .expect("read run response");
        assert_run_success_message(&run_response, &["n"], 0, BOLT_DEFAULT_DATABASE);

        let mut pull = vec![0xB1, BOLT_PULL_SIGNATURE];
        encode_map_header_into(&mut pull, 0);
        write_chunked_message(&mut client, &pull)
            .await
            .expect("write pull");
        let record = read_chunked_message(&mut client)
            .await
            .expect("read record");
        assert_eq!(record, encode_record_message(&[aiondb_engine::Value::Int(7)]).unwrap());
        let summary = read_chunked_message(&mut client)
            .await
            .expect("read summary");
        assert_pull_success_message(
            &summary,
            &[
                ("type", "r"),
                ("db", BOLT_DEFAULT_DATABASE),
                ("bookmark", "aiondb:bookmark:1"),
            ],
            false,
            BoltStatusKind::Success,
        );

        drop(client);
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn bolt_autocommit_pull_returns_bookmark() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let engine = test_engine();
        engine
            .bootstrap_role("admin", "StrongPass123!", true)
            .expect("bootstrap admin");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            handle_bolt_connection(stream, engine)
                .await
                .expect("bolt autocommit bookmark");
        });

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect");
        let mut request = Vec::from(BOLT_MAGIC);
        request.extend_from_slice(&0x0000_0404u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        client.write_all(&request).await.expect("write handshake");
        let mut reply = [0u8; 4];
        client.read_exact(&mut reply).await.expect("read reply");
        assert_eq!(u32::from_be_bytes(reply), 0x0000_0404);

        let mut hello = vec![0xB1, BOLT_HELLO_SIGNATURE];
        encode_map_header_into(&mut hello, 4);
        encode_string_into(&mut hello, "scheme");
        encode_string_into(&mut hello, "basic");
        encode_string_into(&mut hello, "principal");
        encode_string_into(&mut hello, "admin");
        encode_string_into(&mut hello, "credentials");
        encode_string_into(&mut hello, "StrongPass123!");
        encode_string_into(&mut hello, "user_agent");
        encode_string_into(&mut hello, "neo4j/test");
        write_chunked_message(&mut client, &hello)
            .await
            .expect("write hello");
        let _ = read_chunked_message(&mut client)
            .await
            .expect("read hello response");

        let mut run = vec![0xB3, BOLT_RUN_SIGNATURE];
        encode_string_into(&mut run, "SELECT 1 AS n");
        encode_map_header_into(&mut run, 0);
        encode_map_header_into(&mut run, 0);
        write_chunked_message(&mut client, &run)
            .await
            .expect("write run");
        let _ = read_chunked_message(&mut client)
            .await
            .expect("read run success");

        let mut pull = vec![0xB1, BOLT_PULL_SIGNATURE];
        encode_map_header_into(&mut pull, 0);
        write_chunked_message(&mut client, &pull)
            .await
            .expect("write pull");
        let record = read_chunked_message(&mut client)
            .await
            .expect("read record");
        assert_eq!(record, encode_record_message(&[aiondb_engine::Value::Int(1)]).unwrap());
        let summary = read_chunked_message(&mut client)
            .await
            .expect("read summary");
        assert_pull_success_message(
            &summary,
            &[
                ("type", "r"),
                ("db", BOLT_DEFAULT_DATABASE),
                ("bookmark", "aiondb:bookmark:1"),
            ],
            false,
            BoltStatusKind::Success,
        );

        drop(client);
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn bolt_run_rejects_impersonation_metadata() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let engine = test_engine();
        engine
            .bootstrap_role("admin", "StrongPass123!", true)
            .expect("bootstrap admin");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            handle_bolt_connection(stream, engine)
                .await
                .expect("bolt run impersonation");
        });

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect");
        let mut request = Vec::from(BOLT_MAGIC);
        request.extend_from_slice(&0x0000_0404u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        client.write_all(&request).await.expect("write handshake");
        let mut reply = [0u8; 4];
        client.read_exact(&mut reply).await.expect("read reply");
        assert_eq!(u32::from_be_bytes(reply), 0x0000_0404);

        let mut hello = vec![0xB1, BOLT_HELLO_SIGNATURE];
        encode_map_header_into(&mut hello, 4);
        encode_string_into(&mut hello, "scheme");
        encode_string_into(&mut hello, "basic");
        encode_string_into(&mut hello, "principal");
        encode_string_into(&mut hello, "admin");
        encode_string_into(&mut hello, "credentials");
        encode_string_into(&mut hello, "StrongPass123!");
        encode_string_into(&mut hello, "user_agent");
        encode_string_into(&mut hello, "neo4j/test");
        write_chunked_message(&mut client, &hello)
            .await
            .expect("write hello");
        let _ = read_chunked_message(&mut client)
            .await
            .expect("read hello response");

        let mut run = vec![0xB3, BOLT_RUN_SIGNATURE];
        encode_string_into(&mut run, "SELECT 1 AS n");
        encode_map_header_into(&mut run, 0);
        encode_map_header_into(&mut run, 1);
        encode_string_into(&mut run, "imp_user");
        encode_string_into(&mut run, "alice");
        write_chunked_message(&mut client, &run)
            .await
            .expect("write run");
        let response = read_chunked_message(&mut client)
            .await
            .expect("read run response");
        assert_eq!(
            response,
            encode_failure_message(
                BOLT_FORBIDDEN_FAILURE_CODE,
                "Bolt compatibility does not support impersonation (\"alice\")",
            )
        );

        drop(client);
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn bolt_route_returns_single_node_routing_table() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let engine = test_engine();
        engine
            .bootstrap_role("admin", "StrongPass123!", true)
            .expect("bootstrap admin");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            handle_bolt_connection(stream, engine)
                .await
                .expect("bolt route");
        });

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect");
        let mut request = Vec::from(BOLT_MAGIC);
        request.extend_from_slice(&0x0000_0404u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        client.write_all(&request).await.expect("write handshake");
        let mut reply = [0u8; 4];
        client.read_exact(&mut reply).await.expect("read reply");
        assert_eq!(u32::from_be_bytes(reply), 0x0000_0404);

        let mut hello = vec![0xB1, BOLT_HELLO_SIGNATURE];
        encode_map_header_into(&mut hello, 4);
        encode_string_into(&mut hello, "scheme");
        encode_string_into(&mut hello, "basic");
        encode_string_into(&mut hello, "principal");
        encode_string_into(&mut hello, "admin");
        encode_string_into(&mut hello, "credentials");
        encode_string_into(&mut hello, "StrongPass123!");
        encode_string_into(&mut hello, "user_agent");
        encode_string_into(&mut hello, "neo4j/test");
        write_chunked_message(&mut client, &hello)
            .await
            .expect("write hello");
        let _ = read_chunked_message(&mut client)
            .await
            .expect("read hello response");

        let mut route = vec![0xB3, BOLT_ROUTE_SIGNATURE];
        encode_map_header_into(&mut route, 0);
        encode_list_header_into(&mut route, 0);
        encode_map_header_into(&mut route, 1);
        encode_string_into(&mut route, "db");
        encode_string_into(&mut route, "default");
        write_chunked_message(&mut client, &route)
            .await
            .expect("write route");
        let route_response = read_chunked_message(&mut client)
            .await
            .expect("read route response");
        assert_eq!(
            route_response,
            encode_route_success_message(&addr.to_string(), "default")
        );

        drop(client);
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn bolt_route_rejects_invalid_bookmarks_field() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let engine = test_engine();
        engine
            .bootstrap_role("admin", "StrongPass123!", true)
            .expect("bootstrap admin");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            handle_bolt_connection(stream, engine)
                .await
                .expect("bolt route reject bookmarks field");
        });

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect");
        let mut request = Vec::from(BOLT_MAGIC);
        request.extend_from_slice(&0x0000_0404u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        client.write_all(&request).await.expect("write handshake");
        let mut reply = [0u8; 4];
        client.read_exact(&mut reply).await.expect("read reply");
        assert_eq!(u32::from_be_bytes(reply), 0x0000_0404);

        let mut hello = vec![0xB1, BOLT_HELLO_SIGNATURE];
        encode_map_header_into(&mut hello, 4);
        encode_string_into(&mut hello, "scheme");
        encode_string_into(&mut hello, "basic");
        encode_string_into(&mut hello, "principal");
        encode_string_into(&mut hello, "admin");
        encode_string_into(&mut hello, "credentials");
        encode_string_into(&mut hello, "StrongPass123!");
        encode_string_into(&mut hello, "user_agent");
        encode_string_into(&mut hello, "neo4j/test");
        write_chunked_message(&mut client, &hello)
            .await
            .expect("write hello");
        let _ = read_chunked_message(&mut client)
            .await
            .expect("read hello response");

        let mut route = vec![0xB3, BOLT_ROUTE_SIGNATURE];
        encode_map_header_into(&mut route, 0);
        encode_string_into(&mut route, "not-a-list");
        encode_map_header_into(&mut route, 1);
        encode_string_into(&mut route, "db");
        encode_string_into(&mut route, "default");
        write_chunked_message(&mut client, &route)
            .await
            .expect("write route");
        let response = read_chunked_message(&mut client)
            .await
            .expect("read route response");
        assert_eq!(
            response,
            encode_failure_message(
                BOLT_REQUEST_FAILURE_CODE,
                "bolt value must be a list",
            )
        );

        drop(client);
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn bolt_route_accepts_write_access_mode() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let engine = test_engine();
        engine
            .bootstrap_role("admin", "StrongPass123!", true)
            .expect("bootstrap admin");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            handle_bolt_connection(stream, engine)
                .await
                .expect("bolt route write mode");
        });

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect");
        let mut request = Vec::from(BOLT_MAGIC);
        request.extend_from_slice(&0x0000_0404u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        client.write_all(&request).await.expect("write handshake");
        let mut reply = [0u8; 4];
        client.read_exact(&mut reply).await.expect("read reply");
        assert_eq!(u32::from_be_bytes(reply), 0x0000_0404);

        let mut hello = vec![0xB1, BOLT_HELLO_SIGNATURE];
        encode_map_header_into(&mut hello, 4);
        encode_string_into(&mut hello, "scheme");
        encode_string_into(&mut hello, "basic");
        encode_string_into(&mut hello, "principal");
        encode_string_into(&mut hello, "admin");
        encode_string_into(&mut hello, "credentials");
        encode_string_into(&mut hello, "StrongPass123!");
        encode_string_into(&mut hello, "user_agent");
        encode_string_into(&mut hello, "neo4j/test");
        write_chunked_message(&mut client, &hello)
            .await
            .expect("write hello");
        let _ = read_chunked_message(&mut client)
            .await
            .expect("read hello response");

        let mut route = vec![0xB3, BOLT_ROUTE_SIGNATURE];
        encode_map_header_into(&mut route, 0);
        encode_list_header_into(&mut route, 0);
        encode_map_header_into(&mut route, 2);
        encode_string_into(&mut route, "db");
        encode_string_into(&mut route, "default");
        encode_string_into(&mut route, "mode");
        encode_string_into(&mut route, "write");
        write_chunked_message(&mut client, &route)
            .await
            .expect("write route");
        let route_response = read_chunked_message(&mut client)
            .await
            .expect("read route response");
        assert_eq!(
            route_response,
            encode_route_success_message(&addr.to_string(), "default")
        );

        drop(client);
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn bolt_route_rejects_invalid_routing_context_field() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let engine = test_engine();
        engine
            .bootstrap_role("admin", "StrongPass123!", true)
            .expect("bootstrap admin");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            handle_bolt_connection(stream, engine)
                .await
                .expect("bolt route reject routing context field");
        });

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect");
        let mut request = Vec::from(BOLT_MAGIC);
        request.extend_from_slice(&0x0000_0404u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        client.write_all(&request).await.expect("write handshake");
        let mut reply = [0u8; 4];
        client.read_exact(&mut reply).await.expect("read reply");
        assert_eq!(u32::from_be_bytes(reply), 0x0000_0404);

        let mut hello = vec![0xB1, BOLT_HELLO_SIGNATURE];
        encode_map_header_into(&mut hello, 4);
        encode_string_into(&mut hello, "scheme");
        encode_string_into(&mut hello, "basic");
        encode_string_into(&mut hello, "principal");
        encode_string_into(&mut hello, "admin");
        encode_string_into(&mut hello, "credentials");
        encode_string_into(&mut hello, "StrongPass123!");
        encode_string_into(&mut hello, "user_agent");
        encode_string_into(&mut hello, "neo4j/test");
        write_chunked_message(&mut client, &hello)
            .await
            .expect("write hello");
        let _ = read_chunked_message(&mut client)
            .await
            .expect("read hello response");

        let mut route = vec![0xB3, BOLT_ROUTE_SIGNATURE];
        encode_string_into(&mut route, "not-a-map");
        encode_list_header_into(&mut route, 0);
        encode_map_header_into(&mut route, 1);
        encode_string_into(&mut route, "db");
        encode_string_into(&mut route, "default");
        write_chunked_message(&mut client, &route)
            .await
            .expect("write route");
        let response = read_chunked_message(&mut client)
            .await
            .expect("read route response");
        assert_eq!(
            response,
            encode_failure_message(
                BOLT_REQUEST_FAILURE_CODE,
                "bolt hello metadata must be a map",
            )
        );

        drop(client);
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn bolt_route_rejects_non_default_database() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let engine = test_engine();
        engine
            .bootstrap_role("admin", "StrongPass123!", true)
            .expect("bootstrap admin");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            handle_bolt_connection(stream, engine)
                .await
                .expect("bolt route reject db");
        });

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect");
        let mut request = Vec::from(BOLT_MAGIC);
        request.extend_from_slice(&0x0000_0404u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        client.write_all(&request).await.expect("write handshake");
        let mut reply = [0u8; 4];
        client.read_exact(&mut reply).await.expect("read reply");
        assert_eq!(u32::from_be_bytes(reply), 0x0000_0404);

        let mut hello = vec![0xB1, BOLT_HELLO_SIGNATURE];
        encode_map_header_into(&mut hello, 4);
        encode_string_into(&mut hello, "scheme");
        encode_string_into(&mut hello, "basic");
        encode_string_into(&mut hello, "principal");
        encode_string_into(&mut hello, "admin");
        encode_string_into(&mut hello, "credentials");
        encode_string_into(&mut hello, "StrongPass123!");
        encode_string_into(&mut hello, "user_agent");
        encode_string_into(&mut hello, "neo4j/test");
        write_chunked_message(&mut client, &hello)
            .await
            .expect("write hello");
        let _ = read_chunked_message(&mut client)
            .await
            .expect("read hello response");

        let mut route = vec![0xB3, BOLT_ROUTE_SIGNATURE];
        encode_map_header_into(&mut route, 0);
        encode_list_header_into(&mut route, 0);
        encode_map_header_into(&mut route, 1);
        encode_string_into(&mut route, "db");
        encode_string_into(&mut route, "other");
        write_chunked_message(&mut client, &route)
            .await
            .expect("write route");
        let response = read_chunked_message(&mut client)
            .await
            .expect("read route response");
        assert_eq!(
            response,
            encode_failure_message(
                BOLT_DATABASE_FAILURE_CODE,
                "Bolt compatibility only supports database \"default\", got \"other\"",
            )
        );

        drop(client);
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn bolt_begin_run_commit_returns_record() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let engine = test_engine();
        engine
            .bootstrap_role("admin", "StrongPass123!", true)
            .expect("bootstrap admin");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            handle_bolt_connection(stream, engine)
                .await
                .expect("bolt begin run commit");
        });

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect");
        let mut request = Vec::from(BOLT_MAGIC);
        request.extend_from_slice(&0x0000_0404u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        client.write_all(&request).await.expect("write handshake");
        let mut reply = [0u8; 4];
        client.read_exact(&mut reply).await.expect("read reply");
        assert_eq!(u32::from_be_bytes(reply), 0x0000_0404);

        let mut hello = vec![0xB1, BOLT_HELLO_SIGNATURE];
        encode_map_header_into(&mut hello, 4);
        encode_string_into(&mut hello, "scheme");
        encode_string_into(&mut hello, "basic");
        encode_string_into(&mut hello, "principal");
        encode_string_into(&mut hello, "admin");
        encode_string_into(&mut hello, "credentials");
        encode_string_into(&mut hello, "StrongPass123!");
        encode_string_into(&mut hello, "user_agent");
        encode_string_into(&mut hello, "neo4j/test");
        write_chunked_message(&mut client, &hello)
            .await
            .expect("write hello");
        let _ = read_chunked_message(&mut client)
            .await
            .expect("read hello response");

        let mut begin = vec![0xB1, BOLT_BEGIN_SIGNATURE];
        encode_map_header_into(&mut begin, 0);
        write_chunked_message(&mut client, &begin)
            .await
            .expect("write begin");
        let begin_response = read_chunked_message(&mut client)
            .await
            .expect("read begin response");
        assert_eq!(begin_response, encode_success_message(&[]));

        let mut run = vec![0xB3, BOLT_RUN_SIGNATURE];
        encode_string_into(&mut run, "SELECT 3 AS n");
        encode_map_header_into(&mut run, 0);
        encode_map_header_into(&mut run, 0);
        write_chunked_message(&mut client, &run)
            .await
            .expect("write run");
        let _ = read_chunked_message(&mut client)
            .await
            .expect("read run response");

        let mut pull = vec![0xB1, BOLT_PULL_SIGNATURE];
        encode_map_header_into(&mut pull, 0);
        write_chunked_message(&mut client, &pull)
            .await
            .expect("write pull");
        let record = read_chunked_message(&mut client)
            .await
            .expect("read record");
        assert_eq!(record, encode_record_message(&[aiondb_engine::Value::Int(3)]).unwrap());
        let _ = read_chunked_message(&mut client)
            .await
            .expect("read summary");

        let commit = vec![0xB0, BOLT_COMMIT_SIGNATURE];
        write_chunked_message(&mut client, &commit)
            .await
            .expect("write commit");
        let commit_response = read_chunked_message(&mut client)
            .await
            .expect("read commit response");
        assert_eq!(
            commit_response,
            encode_success_message(&[
                ("bookmark", "aiondb:bookmark:1"),
                ("db", BOLT_DEFAULT_DATABASE),
            ])
        );

        drop(client);
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn bolt_run_rejects_non_default_database() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let engine = test_engine();
        engine
            .bootstrap_role("admin", "StrongPass123!", true)
            .expect("bootstrap admin");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            handle_bolt_connection(stream, engine)
                .await
                .expect("bolt run reject db");
        });

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect");
        let mut request = Vec::from(BOLT_MAGIC);
        request.extend_from_slice(&0x0000_0404u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        client.write_all(&request).await.expect("write handshake");
        let mut reply = [0u8; 4];
        client.read_exact(&mut reply).await.expect("read reply");
        assert_eq!(u32::from_be_bytes(reply), 0x0000_0404);

        let mut hello = vec![0xB1, BOLT_HELLO_SIGNATURE];
        encode_map_header_into(&mut hello, 4);
        encode_string_into(&mut hello, "scheme");
        encode_string_into(&mut hello, "basic");
        encode_string_into(&mut hello, "principal");
        encode_string_into(&mut hello, "admin");
        encode_string_into(&mut hello, "credentials");
        encode_string_into(&mut hello, "StrongPass123!");
        encode_string_into(&mut hello, "user_agent");
        encode_string_into(&mut hello, "neo4j/test");
        write_chunked_message(&mut client, &hello)
            .await
            .expect("write hello");
        let _ = read_chunked_message(&mut client)
            .await
            .expect("read hello response");

        let mut run = vec![0xB3, BOLT_RUN_SIGNATURE];
        encode_string_into(&mut run, "SELECT 1 AS n");
        encode_map_header_into(&mut run, 0);
        encode_map_header_into(&mut run, 1);
        encode_string_into(&mut run, "db");
        encode_string_into(&mut run, "other");
        write_chunked_message(&mut client, &run)
            .await
            .expect("write run");
        let response = read_chunked_message(&mut client)
            .await
            .expect("read failure");
        assert_eq!(
            response,
            encode_failure_message(
                BOLT_DATABASE_FAILURE_CODE,
                "Bolt compatibility only supports database \"default\", got \"other\"",
            )
        );

        drop(client);
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn bolt_run_rejects_non_string_database_metadata() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let engine = test_engine();
        engine
            .bootstrap_role("admin", "StrongPass123!", true)
            .expect("bootstrap admin");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            handle_bolt_connection(stream, engine)
                .await
                .expect("bolt run reject non-string db");
        });

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect");
        let mut request = Vec::from(BOLT_MAGIC);
        request.extend_from_slice(&0x0000_0404u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        client.write_all(&request).await.expect("write handshake");
        let mut reply = [0u8; 4];
        client.read_exact(&mut reply).await.expect("read reply");
        assert_eq!(u32::from_be_bytes(reply), 0x0000_0404);

        let mut hello = vec![0xB1, BOLT_HELLO_SIGNATURE];
        encode_map_header_into(&mut hello, 4);
        encode_string_into(&mut hello, "scheme");
        encode_string_into(&mut hello, "basic");
        encode_string_into(&mut hello, "principal");
        encode_string_into(&mut hello, "admin");
        encode_string_into(&mut hello, "credentials");
        encode_string_into(&mut hello, "StrongPass123!");
        encode_string_into(&mut hello, "user_agent");
        encode_string_into(&mut hello, "neo4j/test");
        write_chunked_message(&mut client, &hello)
            .await
            .expect("write hello");
        let _ = read_chunked_message(&mut client)
            .await
            .expect("read hello response");

        let mut run = vec![0xB3, BOLT_RUN_SIGNATURE];
        encode_string_into(&mut run, "SELECT 1 AS n");
        encode_map_header_into(&mut run, 0);
        encode_map_header_into(&mut run, 1);
        encode_string_into(&mut run, "db");
        encode_i64_into(&mut run, 42);
        write_chunked_message(&mut client, &run)
            .await
            .expect("write run");
        let response = read_chunked_message(&mut client)
            .await
            .expect("read failure");
        assert_eq!(
            response,
            encode_failure_message(BOLT_REQUEST_FAILURE_CODE, "Bolt db metadata must be a string")
        );

        drop(client);
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn bolt_run_rejects_non_string_notification_severity() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let engine = test_engine();
        engine
            .bootstrap_role("admin", "StrongPass123!", true)
            .expect("bootstrap admin");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            handle_bolt_connection(stream, engine)
                .await
                .expect("bolt run reject notification severity");
        });

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect");
        let mut request = Vec::from(BOLT_MAGIC);
        request.extend_from_slice(&0x0000_0404u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        client.write_all(&request).await.expect("write handshake");
        let mut reply = [0u8; 4];
        client.read_exact(&mut reply).await.expect("read reply");
        assert_eq!(u32::from_be_bytes(reply), 0x0000_0404);

        let mut hello = vec![0xB1, BOLT_HELLO_SIGNATURE];
        encode_map_header_into(&mut hello, 4);
        encode_string_into(&mut hello, "scheme");
        encode_string_into(&mut hello, "basic");
        encode_string_into(&mut hello, "principal");
        encode_string_into(&mut hello, "admin");
        encode_string_into(&mut hello, "credentials");
        encode_string_into(&mut hello, "StrongPass123!");
        encode_string_into(&mut hello, "user_agent");
        encode_string_into(&mut hello, "neo4j/test");
        write_chunked_message(&mut client, &hello)
            .await
            .expect("write hello");
        let _ = read_chunked_message(&mut client)
            .await
            .expect("read hello response");

        let mut run = vec![0xB3, BOLT_RUN_SIGNATURE];
        encode_string_into(&mut run, "SELECT 1 AS n");
        encode_map_header_into(&mut run, 0);
        encode_map_header_into(&mut run, 1);
        encode_string_into(&mut run, "notifications_minimum_severity");
        encode_i64_into(&mut run, 42);
        write_chunked_message(&mut client, &run)
            .await
            .expect("write run");
        let response = read_chunked_message(&mut client)
            .await
            .expect("read failure");
        assert_eq!(
            response,
            encode_failure_message(
                BOLT_REQUEST_FAILURE_CODE,
                "Bolt notifications_minimum_severity metadata must be a string",
            )
        );

        drop(client);
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn bolt_run_rejects_invalid_bookmarks_metadata() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let engine = test_engine();
        engine
            .bootstrap_role("admin", "StrongPass123!", true)
            .expect("bootstrap admin");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            handle_bolt_connection(stream, engine)
                .await
                .expect("bolt run reject bookmarks");
        });

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect");
        let mut request = Vec::from(BOLT_MAGIC);
        request.extend_from_slice(&0x0000_0404u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        client.write_all(&request).await.expect("write handshake");
        let mut reply = [0u8; 4];
        client.read_exact(&mut reply).await.expect("read reply");
        assert_eq!(u32::from_be_bytes(reply), 0x0000_0404);

        let mut hello = vec![0xB1, BOLT_HELLO_SIGNATURE];
        encode_map_header_into(&mut hello, 4);
        encode_string_into(&mut hello, "scheme");
        encode_string_into(&mut hello, "basic");
        encode_string_into(&mut hello, "principal");
        encode_string_into(&mut hello, "admin");
        encode_string_into(&mut hello, "credentials");
        encode_string_into(&mut hello, "StrongPass123!");
        encode_string_into(&mut hello, "user_agent");
        encode_string_into(&mut hello, "neo4j/test");
        write_chunked_message(&mut client, &hello)
            .await
            .expect("write hello");
        let _ = read_chunked_message(&mut client)
            .await
            .expect("read hello response");

        let mut run = vec![0xB3, BOLT_RUN_SIGNATURE];
        encode_string_into(&mut run, "SELECT 1 AS n");
        encode_map_header_into(&mut run, 0);
        encode_map_header_into(&mut run, 1);
        encode_string_into(&mut run, "bookmarks");
        encode_string_into(&mut run, "not-a-list");
        write_chunked_message(&mut client, &run)
            .await
            .expect("write run");
        let response = read_chunked_message(&mut client)
            .await
            .expect("read run response");
        assert_eq!(
            response,
            encode_failure_message(
                BOLT_REQUEST_FAILURE_CODE,
                "Bolt bookmarks metadata must be a list of strings",
            )
        );

        drop(client);
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn bolt_run_accepts_tx_timeout_metadata() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let engine = test_engine();
        engine
            .bootstrap_role("admin", "StrongPass123!", true)
            .expect("bootstrap admin");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            handle_bolt_connection(stream, engine)
                .await
                .expect("bolt run accept tx_timeout");
        });

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect");
        let mut request = Vec::from(BOLT_MAGIC);
        request.extend_from_slice(&0x0000_0404u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        client.write_all(&request).await.expect("write handshake");
        let mut reply = [0u8; 4];
        client.read_exact(&mut reply).await.expect("read reply");
        assert_eq!(u32::from_be_bytes(reply), 0x0000_0404);

        let mut hello = vec![0xB1, BOLT_HELLO_SIGNATURE];
        encode_map_header_into(&mut hello, 4);
        encode_string_into(&mut hello, "scheme");
        encode_string_into(&mut hello, "basic");
        encode_string_into(&mut hello, "principal");
        encode_string_into(&mut hello, "admin");
        encode_string_into(&mut hello, "credentials");
        encode_string_into(&mut hello, "StrongPass123!");
        encode_string_into(&mut hello, "user_agent");
        encode_string_into(&mut hello, "neo4j/test");
        write_chunked_message(&mut client, &hello)
            .await
            .expect("write hello");
        let _ = read_chunked_message(&mut client)
            .await
            .expect("read hello response");

        let mut run = vec![0xB3, BOLT_RUN_SIGNATURE];
        encode_string_into(&mut run, "SELECT 1 AS n");
        encode_map_header_into(&mut run, 0);
        encode_map_header_into(&mut run, 1);
        encode_string_into(&mut run, "tx_timeout");
        encode_i64_into(&mut run, 1000);
        write_chunked_message(&mut client, &run)
            .await
            .expect("write run");
        let response = read_chunked_message(&mut client)
            .await
            .expect("read run response");
        assert_eq!(response[1], BOLT_SUCCESS_SIGNATURE);

        drop(client);
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn bolt_run_accepts_tx_metadata() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let engine = test_engine();
        engine
            .bootstrap_role("admin", "StrongPass123!", true)
            .expect("bootstrap admin");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            handle_bolt_connection(stream, engine)
                .await
                .expect("bolt run accept tx_metadata");
        });

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect");
        let mut request = Vec::from(BOLT_MAGIC);
        request.extend_from_slice(&0x0000_0404u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        client.write_all(&request).await.expect("write handshake");
        let mut reply = [0u8; 4];
        client.read_exact(&mut reply).await.expect("read reply");
        assert_eq!(u32::from_be_bytes(reply), 0x0000_0404);

        let mut hello = vec![0xB1, BOLT_HELLO_SIGNATURE];
        encode_map_header_into(&mut hello, 4);
        encode_string_into(&mut hello, "scheme");
        encode_string_into(&mut hello, "basic");
        encode_string_into(&mut hello, "principal");
        encode_string_into(&mut hello, "admin");
        encode_string_into(&mut hello, "credentials");
        encode_string_into(&mut hello, "StrongPass123!");
        encode_string_into(&mut hello, "user_agent");
        encode_string_into(&mut hello, "neo4j/test");
        write_chunked_message(&mut client, &hello)
            .await
            .expect("write hello");
        let _ = read_chunked_message(&mut client)
            .await
            .expect("read hello response");

        let mut run = vec![0xB3, BOLT_RUN_SIGNATURE];
        encode_string_into(&mut run, "SELECT 1 AS n");
        encode_map_header_into(&mut run, 0);
        encode_map_header_into(&mut run, 1);
        encode_string_into(&mut run, "tx_metadata");
        encode_map_header_into(&mut run, 1);
        encode_string_into(&mut run, "app");
        encode_string_into(&mut run, "cypher-shell");
        write_chunked_message(&mut client, &run)
            .await
            .expect("write run");
        let response = read_chunked_message(&mut client)
            .await
            .expect("read run response");
        assert_eq!(response[1], BOLT_SUCCESS_SIGNATURE);

        drop(client);
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn bolt_run_accepts_db_ping_compat_call() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let engine = test_engine();
        engine
            .bootstrap_role("admin", "StrongPass123!", true)
            .expect("bootstrap admin");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            handle_bolt_connection(stream, engine)
                .await
                .expect("bolt run accept db.ping");
        });

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect");
        let mut request = Vec::from(BOLT_MAGIC);
        request.extend_from_slice(&0x0000_0404u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        client.write_all(&request).await.expect("write handshake");
        let mut reply = [0u8; 4];
        client.read_exact(&mut reply).await.expect("read reply");
        assert_eq!(u32::from_be_bytes(reply), 0x0000_0404);

        let mut hello = vec![0xB1, BOLT_HELLO_SIGNATURE];
        encode_map_header_into(&mut hello, 4);
        encode_string_into(&mut hello, "scheme");
        encode_string_into(&mut hello, "basic");
        encode_string_into(&mut hello, "principal");
        encode_string_into(&mut hello, "admin");
        encode_string_into(&mut hello, "credentials");
        encode_string_into(&mut hello, "StrongPass123!");
        encode_string_into(&mut hello, "user_agent");
        encode_string_into(&mut hello, "neo4j/test");
        write_chunked_message(&mut client, &hello)
            .await
            .expect("write hello");
        let _ = read_chunked_message(&mut client)
            .await
            .expect("read hello response");

        let mut run = vec![0xB3, BOLT_RUN_SIGNATURE];
        encode_string_into(&mut run, "CALL db.ping()");
        encode_map_header_into(&mut run, 0);
        encode_map_header_into(&mut run, 0);
        write_chunked_message(&mut client, &run)
            .await
            .expect("write run");
        let run_response = read_chunked_message(&mut client)
            .await
            .expect("read run response");
        assert_eq!(run_response[1], BOLT_SUCCESS_SIGNATURE);

        let mut pull = vec![0xB2, BOLT_PULL_SIGNATURE];
        encode_map_header_into(&mut pull, 2);
        encode_string_into(&mut pull, "n");
        encode_i64_into(&mut pull, -1);
        encode_string_into(&mut pull, "qid");
        encode_i64_into(&mut pull, 0);
        write_chunked_message(&mut client, &pull)
            .await
            .expect("write pull");
        let pull_response = read_chunked_message(&mut client)
            .await
            .expect("read pull response");
        assert_eq!(pull_response[1], BOLT_SUCCESS_SIGNATURE);

        drop(client);
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn bolt_run_returns_graph_metadata_compat_calls() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let engine = test_engine();
        engine
            .bootstrap_role("admin", "StrongPass123!", true)
            .expect("bootstrap admin");
        let admin = engine
            .startup(StartupParams {
                database: BOLT_DEFAULT_DATABASE.to_owned(),
                application_name: Some("bolt-compat-prep".to_owned()),
                options: std::collections::BTreeMap::new(),
                credential: Credential::CleartextPassword {
                    user: "admin".to_owned(),
                    password: SecretString::new("StrongPass123!".to_owned()),
                },
                transport: TransportInfo {
                    kind: TransportKind::Network {
                        tls: true,
                        peer_addr: Some("127.0.0.1:7687".to_owned()),
                    },
                },
            })
            .expect("startup admin")
            .0;
        engine
            .execute_sql(
                &admin,
                "CREATE TABLE people (id INT, name TEXT); \
                 CREATE TABLE companies (id INT, sector TEXT); \
                 CREATE TABLE works_at (source_id INT, target_id INT, since_year INT); \
                 CREATE NODE LABEL Person ON people; \
                 CREATE NODE LABEL Company ON companies; \
                 CREATE EDGE LABEL WORKS_AT ON works_at SOURCE Person TARGET Company",
            )
            .expect("prepare graph metadata");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            handle_bolt_connection(stream, engine)
                .await
                .expect("bolt graph metadata");
        });

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect");
        let mut request = Vec::from(BOLT_MAGIC);
        request.extend_from_slice(&0x0000_0404u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        client.write_all(&request).await.expect("write handshake");
        let mut reply = [0u8; 4];
        client.read_exact(&mut reply).await.expect("read reply");
        assert_eq!(u32::from_be_bytes(reply), 0x0000_0404);

        let mut hello = vec![0xB1, BOLT_HELLO_SIGNATURE];
        encode_map_header_into(&mut hello, 4);
        encode_string_into(&mut hello, "scheme");
        encode_string_into(&mut hello, "basic");
        encode_string_into(&mut hello, "principal");
        encode_string_into(&mut hello, "admin");
        encode_string_into(&mut hello, "credentials");
        encode_string_into(&mut hello, "StrongPass123!");
        encode_string_into(&mut hello, "user_agent");
        encode_string_into(&mut hello, "neo4j/test");
        write_chunked_message(&mut client, &hello)
            .await
            .expect("write hello");
        let _ = read_chunked_message(&mut client)
            .await
            .expect("read hello response");

        async fn run_and_collect_single_column(
            client: &mut tokio::net::TcpStream,
            statement: &str,
            field: &str,
        ) -> Vec<String> {
            let mut run = vec![0xB3, BOLT_RUN_SIGNATURE];
            encode_string_into(&mut run, statement);
            encode_map_header_into(&mut run, 0);
            encode_map_header_into(&mut run, 0);
            write_chunked_message(client, &run)
                .await
                .expect("write run");
            let run_response = read_chunked_message(client)
                .await
                .expect("read run response");
            assert_run_success_message(&run_response, &[field], 0, BOLT_DEFAULT_DATABASE);

            let mut pull = vec![0xB1, BOLT_PULL_SIGNATURE];
            encode_map_header_into(&mut pull, 0);
            write_chunked_message(client, &pull)
                .await
                .expect("write pull");

            let mut values = Vec::new();
            loop {
                let message = read_chunked_message(client)
                    .await
                    .expect("read response");
                if message[1] == BOLT_RECORD_SIGNATURE {
                    let mut remaining = &message[2..];
                    let len = decode_list_len(&mut remaining).expect("record list");
                    assert_eq!(len, 1);
                    values.push(decode_string(&mut remaining).expect("record string"));
                    assert!(remaining.is_empty());
                } else {
                    assert_pull_success_message(
                        &message,
                        &[
                            ("type", "r"),
                            ("db", BOLT_DEFAULT_DATABASE),
                            ("bookmark", "aiondb:bookmark:1"),
                        ],
                        false,
                        BoltStatusKind::Success,
                    );
                    break;
                }
            }
            values
        }

        let labels = run_and_collect_single_column(
            &mut client,
            "CALL db.labels() YIELD label RETURN label",
            "label",
        )
        .await;
        assert_eq!(labels, vec!["Company".to_owned(), "Person".to_owned()]);

        let relationship_types = run_and_collect_single_column(
            &mut client,
            "CALL db.relationshipTypes() YIELD relationshipType RETURN relationshipType",
            "relationshipType",
        )
        .await;
        assert_eq!(relationship_types, vec!["WORKS_AT".to_owned()]);

        let property_keys = run_and_collect_single_column(
            &mut client,
            "CALL db.propertyKeys() YIELD propertyKey RETURN propertyKey",
            "propertyKey",
        )
        .await;
        assert_eq!(
            property_keys,
            vec![
                "id".to_owned(),
                "name".to_owned(),
                "sector".to_owned(),
                "since_year".to_owned(),
                "source_id".to_owned(),
                "target_id".to_owned(),
            ]
        );

        drop(client);
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn bolt_run_rejects_auth_metadata() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let engine = test_engine();
        engine
            .bootstrap_role("admin", "StrongPass123!", true)
            .expect("bootstrap admin");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            handle_bolt_connection(stream, engine)
                .await
                .expect("bolt run reject auth metadata");
        });

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect");
        let mut request = Vec::from(BOLT_MAGIC);
        request.extend_from_slice(&0x0000_0404u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        client.write_all(&request).await.expect("write handshake");
        let mut reply = [0u8; 4];
        client.read_exact(&mut reply).await.expect("read reply");
        assert_eq!(u32::from_be_bytes(reply), 0x0000_0404);

        let mut hello = vec![0xB1, BOLT_HELLO_SIGNATURE];
        encode_map_header_into(&mut hello, 4);
        encode_string_into(&mut hello, "scheme");
        encode_string_into(&mut hello, "basic");
        encode_string_into(&mut hello, "principal");
        encode_string_into(&mut hello, "admin");
        encode_string_into(&mut hello, "credentials");
        encode_string_into(&mut hello, "StrongPass123!");
        encode_string_into(&mut hello, "user_agent");
        encode_string_into(&mut hello, "neo4j/test");
        write_chunked_message(&mut client, &hello)
            .await
            .expect("write hello");
        let _ = read_chunked_message(&mut client)
            .await
            .expect("read hello response");

        let mut run = vec![0xB3, BOLT_RUN_SIGNATURE];
        encode_string_into(&mut run, "SELECT 1 AS n");
        encode_map_header_into(&mut run, 0);
        encode_map_header_into(&mut run, 1);
        encode_string_into(&mut run, "auth");
        encode_map_header_into(&mut run, 0);
        write_chunked_message(&mut client, &run)
            .await
            .expect("write run");
        let response = read_chunked_message(&mut client)
            .await
            .expect("read run response");
        assert_eq!(
            response,
            encode_failure_message(
                BOLT_REQUEST_FAILURE_CODE,
                "Bolt compatibility does not support auth metadata",
            )
        );

        drop(client);
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn bolt_run_rejects_write_access_mode() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let engine = test_engine();
        engine
            .bootstrap_role("admin", "StrongPass123!", true)
            .expect("bootstrap admin");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            handle_bolt_connection(stream, engine)
                .await
                .expect("bolt run reject write mode");
        });

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect");
        let mut request = Vec::from(BOLT_MAGIC);
        request.extend_from_slice(&0x0000_0404u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        client.write_all(&request).await.expect("write handshake");
        let mut reply = [0u8; 4];
        client.read_exact(&mut reply).await.expect("read reply");
        assert_eq!(u32::from_be_bytes(reply), 0x0000_0404);

        let mut hello = vec![0xB1, BOLT_HELLO_SIGNATURE];
        encode_map_header_into(&mut hello, 4);
        encode_string_into(&mut hello, "scheme");
        encode_string_into(&mut hello, "basic");
        encode_string_into(&mut hello, "principal");
        encode_string_into(&mut hello, "admin");
        encode_string_into(&mut hello, "credentials");
        encode_string_into(&mut hello, "StrongPass123!");
        encode_string_into(&mut hello, "user_agent");
        encode_string_into(&mut hello, "neo4j/test");
        write_chunked_message(&mut client, &hello)
            .await
            .expect("write hello");
        let _ = read_chunked_message(&mut client)
            .await
            .expect("read hello response");

        let mut run = vec![0xB3, BOLT_RUN_SIGNATURE];
        encode_string_into(&mut run, "SELECT 1 AS n");
        encode_map_header_into(&mut run, 0);
        encode_map_header_into(&mut run, 1);
        encode_string_into(&mut run, "mode");
        encode_string_into(&mut run, "w");
        write_chunked_message(&mut client, &run)
            .await
            .expect("write run");
        let response = read_chunked_message(&mut client)
            .await
            .expect("read run response");
        assert_eq!(
            response,
            encode_failure_message(
                BOLT_FORBIDDEN_FAILURE_CODE,
                "Bolt compatibility is read-only and does not support write access mode",
            )
        );

        drop(client);
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn bolt_run_rejects_non_string_impersonation_metadata() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let engine = test_engine();
        engine
            .bootstrap_role("admin", "StrongPass123!", true)
            .expect("bootstrap admin");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            handle_bolt_connection(stream, engine)
                .await
                .expect("bolt run reject non-string impersonation metadata");
        });

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect");
        let mut request = Vec::from(BOLT_MAGIC);
        request.extend_from_slice(&0x0000_0404u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        client.write_all(&request).await.expect("write handshake");
        let mut reply = [0u8; 4];
        client.read_exact(&mut reply).await.expect("read reply");
        assert_eq!(u32::from_be_bytes(reply), 0x0000_0404);

        let mut hello = vec![0xB1, BOLT_HELLO_SIGNATURE];
        encode_map_header_into(&mut hello, 4);
        encode_string_into(&mut hello, "scheme");
        encode_string_into(&mut hello, "basic");
        encode_string_into(&mut hello, "principal");
        encode_string_into(&mut hello, "admin");
        encode_string_into(&mut hello, "credentials");
        encode_string_into(&mut hello, "StrongPass123!");
        encode_string_into(&mut hello, "user_agent");
        encode_string_into(&mut hello, "neo4j/test");
        write_chunked_message(&mut client, &hello)
            .await
            .expect("write hello");
        let _ = read_chunked_message(&mut client)
            .await
            .expect("read hello response");

        let mut run = vec![0xB3, BOLT_RUN_SIGNATURE];
        encode_string_into(&mut run, "SELECT 1 AS n");
        encode_map_header_into(&mut run, 0);
        encode_map_header_into(&mut run, 1);
        encode_string_into(&mut run, "imp_user");
        encode_i64_into(&mut run, 42);
        write_chunked_message(&mut client, &run)
            .await
            .expect("write run");
        let response = read_chunked_message(&mut client)
            .await
            .expect("read run response");
        assert_eq!(
            response,
            encode_failure_message(
                BOLT_REQUEST_FAILURE_CODE,
                "Bolt imp_user metadata must be a string",
            )
        );

        drop(client);
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn bolt_run_rejects_unknown_metadata_key() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let engine = test_engine();
        engine
            .bootstrap_role("admin", "StrongPass123!", true)
            .expect("bootstrap admin");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            handle_bolt_connection(stream, engine)
                .await
                .expect("bolt run reject unknown metadata key");
        });

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect");
        let mut request = Vec::from(BOLT_MAGIC);
        request.extend_from_slice(&0x0000_0404u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        client.write_all(&request).await.expect("write handshake");
        let mut reply = [0u8; 4];
        client.read_exact(&mut reply).await.expect("read reply");
        assert_eq!(u32::from_be_bytes(reply), 0x0000_0404);

        let mut hello = vec![0xB1, BOLT_HELLO_SIGNATURE];
        encode_map_header_into(&mut hello, 4);
        encode_string_into(&mut hello, "scheme");
        encode_string_into(&mut hello, "basic");
        encode_string_into(&mut hello, "principal");
        encode_string_into(&mut hello, "admin");
        encode_string_into(&mut hello, "credentials");
        encode_string_into(&mut hello, "StrongPass123!");
        encode_string_into(&mut hello, "user_agent");
        encode_string_into(&mut hello, "neo4j/test");
        write_chunked_message(&mut client, &hello)
            .await
            .expect("write hello");
        let _ = read_chunked_message(&mut client)
            .await
            .expect("read hello response");

        let mut run = vec![0xB3, BOLT_RUN_SIGNATURE];
        encode_string_into(&mut run, "SELECT 1 AS n");
        encode_map_header_into(&mut run, 0);
        encode_map_header_into(&mut run, 1);
        encode_string_into(&mut run, "foo");
        encode_string_into(&mut run, "bar");
        write_chunked_message(&mut client, &run)
            .await
            .expect("write run");
        let response = read_chunked_message(&mut client)
            .await
            .expect("read run response");
        assert_eq!(
            response,
            encode_failure_message(
                BOLT_REQUEST_FAILURE_CODE,
                "unsupported Bolt metadata key \"foo\"",
            )
        );

        drop(client);
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn bolt_pull_rejects_unsupported_qid() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let engine = test_engine();
        engine
            .bootstrap_role("admin", "StrongPass123!", true)
            .expect("bootstrap admin");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            handle_bolt_connection(stream, engine)
                .await
                .expect("bolt pull reject qid");
        });

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect");
        let mut request = Vec::from(BOLT_MAGIC);
        request.extend_from_slice(&0x0000_0404u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        client.write_all(&request).await.expect("write handshake");
        let mut reply = [0u8; 4];
        client.read_exact(&mut reply).await.expect("read reply");
        assert_eq!(u32::from_be_bytes(reply), 0x0000_0404);

        let mut hello = vec![0xB1, BOLT_HELLO_SIGNATURE];
        encode_map_header_into(&mut hello, 4);
        encode_string_into(&mut hello, "scheme");
        encode_string_into(&mut hello, "basic");
        encode_string_into(&mut hello, "principal");
        encode_string_into(&mut hello, "admin");
        encode_string_into(&mut hello, "credentials");
        encode_string_into(&mut hello, "StrongPass123!");
        encode_string_into(&mut hello, "user_agent");
        encode_string_into(&mut hello, "neo4j/test");
        write_chunked_message(&mut client, &hello)
            .await
            .expect("write hello");
        let _ = read_chunked_message(&mut client)
            .await
            .expect("read hello response");

        let mut run = vec![0xB3, BOLT_RUN_SIGNATURE];
        encode_string_into(&mut run, "SELECT 1 AS n");
        encode_map_header_into(&mut run, 0);
        encode_map_header_into(&mut run, 0);
        write_chunked_message(&mut client, &run)
            .await
            .expect("write run");
        let _ = read_chunked_message(&mut client)
            .await
            .expect("read run response");

        let mut pull = vec![0xB1, BOLT_PULL_SIGNATURE];
        encode_map_header_into(&mut pull, 1);
        encode_string_into(&mut pull, "qid");
        encode_i64_into(&mut pull, 42);
        write_chunked_message(&mut client, &pull)
            .await
            .expect("write pull");
        let response = read_chunked_message(&mut client)
            .await
            .expect("read failure");
        assert_eq!(
            response,
            encode_failure_message(BOLT_REQUEST_FAILURE_CODE, "unsupported Bolt qid 42")
        );

        drop(client);
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn bolt_pull_rejects_unknown_metadata_key() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let engine = test_engine();
        engine
            .bootstrap_role("admin", "StrongPass123!", true)
            .expect("bootstrap admin");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            handle_bolt_connection(stream, engine)
                .await
                .expect("bolt pull reject unknown metadata");
        });

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect");
        let mut request = Vec::from(BOLT_MAGIC);
        request.extend_from_slice(&0x0000_0404u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        client.write_all(&request).await.expect("write handshake");
        let mut reply = [0u8; 4];
        client.read_exact(&mut reply).await.expect("read reply");
        assert_eq!(u32::from_be_bytes(reply), 0x0000_0404);

        let mut hello = vec![0xB1, BOLT_HELLO_SIGNATURE];
        encode_map_header_into(&mut hello, 4);
        encode_string_into(&mut hello, "scheme");
        encode_string_into(&mut hello, "basic");
        encode_string_into(&mut hello, "principal");
        encode_string_into(&mut hello, "admin");
        encode_string_into(&mut hello, "credentials");
        encode_string_into(&mut hello, "StrongPass123!");
        encode_string_into(&mut hello, "user_agent");
        encode_string_into(&mut hello, "neo4j/test");
        write_chunked_message(&mut client, &hello)
            .await
            .expect("write hello");
        let _ = read_chunked_message(&mut client)
            .await
            .expect("read hello response");

        let mut run = vec![0xB3, BOLT_RUN_SIGNATURE];
        encode_string_into(&mut run, "SELECT 1 AS n");
        encode_map_header_into(&mut run, 0);
        encode_map_header_into(&mut run, 0);
        write_chunked_message(&mut client, &run)
            .await
            .expect("write run");
        let _ = read_chunked_message(&mut client)
            .await
            .expect("read run success");

        let mut pull = vec![0xB1, BOLT_PULL_SIGNATURE];
        encode_map_header_into(&mut pull, 1);
        encode_string_into(&mut pull, "foo");
        encode_string_into(&mut pull, "bar");
        write_chunked_message(&mut client, &pull)
            .await
            .expect("write pull");
        let response = read_chunked_message(&mut client)
            .await
            .expect("read pull failure");
        assert_eq!(
            response,
            encode_failure_message(
                BOLT_REQUEST_FAILURE_CODE,
                "unsupported Bolt stream control metadata key \"foo\"",
            )
        );

        drop(client);
        server.await.expect("server join");
    }

    #[test]
    fn encode_record_message_supports_arrays() {
        let encoded = encode_record_message(&[aiondb_engine::Value::Array(vec![
            aiondb_engine::Value::Int(1),
            aiondb_engine::Value::Text("x".to_owned()),
        ])])
        .expect("encode array");
        assert_eq!(encoded, vec![0xB1, BOLT_RECORD_SIGNATURE, 0x91, 0x92, 0x01, 0x81, b'x']);
    }

    #[test]
    fn encode_record_message_supports_jsonb_objects() {
        let encoded = encode_record_message(&[aiondb_engine::Value::Jsonb(serde_json::json!({
            "a": 1,
            "b": [true, null]
        }))])
        .expect("encode jsonb");
        assert_eq!(
            encoded,
            vec![
                0xB1,
                BOLT_RECORD_SIGNATURE,
                0x91,
                0xA2,
                0x81,
                b'a',
                0x01,
                0x81,
                b'b',
                0x92,
                0xC3,
                0xC0,
            ]
        );
    }

    #[test]
    fn encode_record_message_supports_bolt_date() {
        let date = time::Date::from_calendar_date(1970, time::Month::January, 2)
            .expect("date");
        let encoded =
            encode_record_message(&[aiondb_engine::Value::Date(date)]).expect("encode date");
        assert_eq!(encoded, vec![0xB1, BOLT_RECORD_SIGNATURE, 0x91, 0xB1, b'D', 0x01]);
    }

    #[test]
    fn encode_record_message_supports_bolt_duration() {
        let mut encoded = vec![0xB1, BOLT_RECORD_SIGNATURE, 0x91];
        encode_bolt_duration(&mut encoded, 14, 3, 4_500_001);
        assert_eq!(
            encoded,
            vec![
                0xB1,
                BOLT_RECORD_SIGNATURE,
                0x91,
                0xB4,
                b'E',
                0x0E,
                0x03,
                0x04,
                0xCA,
                0x1D,
                0xCD,
                0x68,
                0xE8,
            ]
        );
    }

    #[test]
    fn encode_record_message_supports_bolt_time_with_offset() {
        let time = time::Time::from_hms_micro(12, 34, 56, 123_456).expect("time");
        let offset = time::UtcOffset::from_whole_seconds(3600).expect("offset");
        let mut expected = vec![0xB1, BOLT_RECORD_SIGNATURE, 0x91];
        encode_struct_header_into(&mut expected, 2, b'T');
        encode_i64_into(&mut expected, 45_296_123_456_000);
        encode_i64_into(&mut expected, 3600);
        let encoded =
            encode_record_message(&[aiondb_engine::Value::TimeTz(time, offset)]).expect("encode timetz");
        assert_eq!(encoded, expected);
    }

    #[test]
    fn encode_record_message_supports_bolt_datetime_with_offset() {
        let date =
            time::Date::from_calendar_date(1970, time::Month::January, 2).expect("date");
        let time = time::Time::from_hms_micro(3, 4, 5, 123_456).expect("time");
        let offset = time::UtcOffset::from_whole_seconds(3600).expect("offset");
        let datetime = time::PrimitiveDateTime::new(date, time).assume_offset(offset);
        let mut expected = vec![0xB1, BOLT_RECORD_SIGNATURE, 0x91];
        encode_struct_header_into(&mut expected, 3, b'I');
        encode_i64_into(&mut expected, 93_845);
        encode_i64_into(&mut expected, 123_456_000);
        encode_i64_into(&mut expected, 3600);
        let encoded = encode_record_message(&[aiondb_engine::Value::TimestampTz(datetime)])
            .expect("encode timestamptz");
        assert_eq!(encoded, expected);
    }
