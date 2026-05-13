use super::*;
use aiondb_core::SqlState;

#[test]
fn transaction_status_byte_values() {
    assert_eq!(TransactionStatus::Idle.as_byte(), b'I');
    assert_eq!(TransactionStatus::InTransaction.as_byte(), b'T');
    assert_eq!(TransactionStatus::Failed.as_byte(), b'E');
}

#[test]
fn parse_query_message() {
    let payload = BytesMut::from(&b"SELECT 1\0"[..]);
    let msg = FrontendMessage::parse(b'Q', payload).unwrap();
    match msg {
        FrontendMessage::Query(sql) => assert_eq!(sql, "SELECT 1"),
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn parse_query_message_rejects_trailing_bytes() {
    let payload = BytesMut::from(&b"SELECT 1\0junk"[..]);
    let error =
        FrontendMessage::parse(b'Q', payload).expect_err("trailing query bytes must be rejected");
    assert!(error
        .to_string()
        .contains("trailing bytes in Query message"));
}

#[test]
fn parse_terminate_message() {
    let payload = BytesMut::new();
    let msg = FrontendMessage::parse(b'X', payload).unwrap();
    assert!(matches!(msg, FrontendMessage::Terminate));
}

#[test]
fn parse_sync_message() {
    let payload = BytesMut::new();
    let msg = FrontendMessage::parse(b'S', payload).unwrap();
    assert!(matches!(msg, FrontendMessage::Sync));
}

#[test]
fn parse_sync_message_rejects_unexpected_payload() {
    let payload = BytesMut::from(&b"x"[..]);
    let error = FrontendMessage::parse(b'S', payload).expect_err("sync payload must be empty");
    assert!(error.to_string().contains("trailing bytes in Sync message"));
}

#[test]
fn parse_describe_statement() {
    let mut payload = BytesMut::new();
    payload.extend_from_slice(b"S");
    payload.extend_from_slice(b"my_stmt\0");
    let msg = FrontendMessage::parse(b'D', payload).unwrap();
    match msg {
        FrontendMessage::Describe { target, name } => {
            assert_eq!(target, DescribeTarget::Statement);
            assert_eq!(name, "my_stmt");
        }
        other => panic!("expected Describe, got {other:?}"),
    }
}

#[test]
fn parse_describe_portal() {
    let mut payload = BytesMut::new();
    payload.extend_from_slice(b"P");
    payload.extend_from_slice(b"my_portal\0");
    let msg = FrontendMessage::parse(b'D', payload).unwrap();
    match msg {
        FrontendMessage::Describe { target, name } => {
            assert_eq!(target, DescribeTarget::Portal);
            assert_eq!(name, "my_portal");
        }
        other => panic!("expected Describe, got {other:?}"),
    }
}

#[test]
fn parse_execute_message() {
    let mut payload = BytesMut::new();
    payload.extend_from_slice(b"\0"); // empty portal name
    payload.extend_from_slice(&100i32.to_be_bytes());
    let msg = FrontendMessage::parse(b'E', payload).unwrap();
    match msg {
        FrontendMessage::Execute { portal, max_rows } => {
            assert_eq!(portal, "");
            assert_eq!(max_rows, 100);
        }
        other => panic!("expected Execute, got {other:?}"),
    }
}

#[test]
fn parse_execute_negative_max_rows_errors() {
    let mut payload = BytesMut::new();
    payload.extend_from_slice(b"\0"); // empty portal name
    payload.extend_from_slice(&(-1i32).to_be_bytes());

    let err = FrontendMessage::parse(b'E', payload)
        .expect_err("negative Execute max_rows must be rejected");
    assert!(format!("{err}").contains("invalid Execute max_rows"));
}

#[test]
fn parse_close_statement() {
    let mut payload = BytesMut::new();
    payload.extend_from_slice(b"S");
    payload.extend_from_slice(b"stmt\0");
    let msg = FrontendMessage::parse(b'C', payload).unwrap();
    match msg {
        FrontendMessage::Close { target, name } => {
            assert_eq!(target, CloseTarget::Statement);
            assert_eq!(name, "stmt");
        }
        other => panic!("expected Close, got {other:?}"),
    }
}

#[test]
fn parse_unknown_tag_errors() {
    let payload = BytesMut::new();
    let result = FrontendMessage::parse(b'Z', payload);
    assert!(result.is_err());
}

#[test]
fn write_auth_ok_produces_correct_bytes() {
    let mut w = MessageWriter::new();
    write_auth_ok(&mut w);
    let buf = w.finish_message();
    assert_eq!(buf[0], b'R');
    let len = u32::from_be_bytes([buf[1], buf[2], buf[3], buf[4]]);
    assert_eq!(len, 8); // 4 (length itself) + 4 (auth type)
    let auth_type = i32::from_be_bytes([buf[5], buf[6], buf[7], buf[8]]);
    assert_eq!(auth_type, 0);
}

#[test]
fn write_ready_for_query_idle() {
    let mut w = MessageWriter::new();
    write_ready_for_query(&mut w, TransactionStatus::Idle);
    let buf = w.finish_message();
    assert_eq!(buf[0], b'Z');
    let len = u32::from_be_bytes([buf[1], buf[2], buf[3], buf[4]]);
    assert_eq!(len, 5); // 4 + 1
    assert_eq!(buf[5], b'I');
}

#[test]
fn write_command_complete_select() {
    let mut w = MessageWriter::new();
    write_command_complete(&mut w, "SELECT 5");
    let buf = w.finish_message();
    assert_eq!(buf[0], b'C');
    // payload: "SELECT 5\0" = 9 bytes + 4 = 13
    let len = u32::from_be_bytes([buf[1], buf[2], buf[3], buf[4]]);
    assert_eq!(len, 13);
}

#[test]
fn write_error_response_basic() {
    let report = ErrorReport::new(SqlState::SyntaxError, "bad query");
    let mut w = MessageWriter::new();
    write_error_response_from_report(&mut w, &report);
    let buf = w.finish_message();
    assert_eq!(buf[0], b'E');
    // Should contain the SQLSTATE code "42601"
    let s = String::from_utf8_lossy(&buf);
    assert!(s.contains("42601"));
    assert!(s.contains("bad query"));
}

#[test]
fn write_notice_response_basic() {
    let mut w = MessageWriter::new();
    write_notice_response(&mut w, "compatibility notice");
    let buf = w.finish_message();
    assert_eq!(buf[0], b'N');
    let s = String::from_utf8_lossy(&buf);
    assert!(s.contains("NOTICE"));
    assert!(s.contains("00000"));
    assert!(s.contains("compatibility notice"));
}

#[test]
fn write_parse_complete_minimal() {
    let mut w = MessageWriter::new();
    write_parse_complete(&mut w);
    let buf = w.finish_message();
    assert_eq!(buf[0], b'1');
    let len = u32::from_be_bytes([buf[1], buf[2], buf[3], buf[4]]);
    assert_eq!(len, 4);
}

#[test]
fn write_bind_complete_minimal() {
    let mut w = MessageWriter::new();
    write_bind_complete(&mut w);
    let buf = w.finish_message();
    assert_eq!(buf[0], b'2');
}

#[test]
fn write_copy_both_response_uses_pgwire_tag_w() {
    let mut w = MessageWriter::new();
    write_copy_both_response(&mut w, 0).unwrap();
    let buf = w.finish_message();
    assert_eq!(buf[0], b'W');
    let len = u32::from_be_bytes([buf[1], buf[2], buf[3], buf[4]]);
    assert_eq!(len, 7);
}

#[test]
fn write_copy_both_response_rejects_excessive_column_count_without_partial_frames() {
    let mut w = MessageWriter::new();
    let error = write_copy_both_response(&mut w, i16::MAX as usize + 1)
        .expect_err("too many columns must fail");

    assert!(error
        .to_string()
        .contains("too many columns in COPY response"));
    assert!(w.is_empty(), "no partial COPY response should be emitted");
}

#[test]
fn write_copy_data_chunks_at_backend_payload_cap() {
    let data = vec![b'x'; MAX_BACKEND_MESSAGE_PAYLOAD + 1];
    let mut w = MessageWriter::new();
    write_copy_data(&mut w, &data).expect("write chunked CopyData");
    let buf = w.finish_message();

    assert_eq!(buf[0], b'd');
    let first_len = u32::from_be_bytes([buf[1], buf[2], buf[3], buf[4]]) as usize;
    assert_eq!(first_len, MAX_BACKEND_MESSAGE_PAYLOAD + 4);

    let second_offset = 1 + first_len;
    assert_eq!(buf[second_offset], b'd');
    let second_len = u32::from_be_bytes([
        buf[second_offset + 1],
        buf[second_offset + 2],
        buf[second_offset + 3],
        buf[second_offset + 4],
    ]) as usize;
    assert_eq!(second_len, 5);
    assert_eq!(buf.len(), second_offset + 1 + second_len);
}

#[test]
fn write_data_row_with_null_and_value() {
    let mut w = MessageWriter::new();
    write_data_row(&mut w, &[None, Some(b"hello")]).unwrap();
    let buf = w.finish_message();
    assert_eq!(buf[0], b'D');
    // column count
    let ncols = i16::from_be_bytes([buf[5], buf[6]]);
    assert_eq!(ncols, 2);
}

#[test]
fn parse_parse_message() {
    // Build a Parse message payload:
    // statement_name\0 query\0 num_params(i16) [oid(i32)]...
    let mut payload = BytesMut::new();
    payload.extend_from_slice(b"stmt1\0");
    payload.extend_from_slice(b"SELECT $1\0");
    payload.extend_from_slice(&1i16.to_be_bytes()); // 1 param
    payload.extend_from_slice(&23u32.to_be_bytes()); // int4 OID

    let msg = FrontendMessage::parse(b'P', payload).unwrap();
    match msg {
        FrontendMessage::Parse {
            name,
            query,
            param_types,
        } => {
            assert_eq!(name, "stmt1");
            assert_eq!(query, "SELECT $1");
            assert_eq!(param_types, vec![23]);
        }
        other => panic!("expected Parse, got {other:?}"),
    }
}

#[test]
fn parse_parse_message_rejects_oversized_statement_name() {
    let mut payload = BytesMut::new();
    payload.extend_from_slice(&vec![b'a'; MAX_FRONTEND_NAME_BYTES + 1]);
    payload.extend_from_slice(b"\0");
    payload.extend_from_slice(b"SELECT 1\0");
    payload.extend_from_slice(&0i16.to_be_bytes());

    let error = FrontendMessage::parse(b'P', payload)
        .expect_err("oversized statement name must be rejected");
    assert!(error.to_string().contains("Parse statement name"));
    assert!(error.to_string().contains("maximum length"));
}

#[test]
fn parse_bind_message() {
    // Build a Bind payload.
    let mut payload = BytesMut::new();
    payload.extend_from_slice(b"\0"); // portal (unnamed)
    payload.extend_from_slice(b"stmt1\0"); // statement
    payload.extend_from_slice(&0i16.to_be_bytes()); // 0 param formats
    payload.extend_from_slice(&2i16.to_be_bytes()); // 2 param values
                                                    // param 1: "42"
    payload.extend_from_slice(&2i32.to_be_bytes());
    payload.extend_from_slice(b"42");
    // param 2: NULL
    payload.extend_from_slice(&(-1i32).to_be_bytes());
    // 0 result formats
    payload.extend_from_slice(&0i16.to_be_bytes());

    let msg = FrontendMessage::parse(b'B', payload).unwrap();
    match msg {
        FrontendMessage::Bind {
            portal,
            statement,
            param_formats,
            param_values,
            result_formats,
        } => {
            assert_eq!(portal, "");
            assert_eq!(statement, "stmt1");
            assert!(param_formats.is_empty());
            assert_eq!(param_values.len(), 2);
            assert_eq!(param_values[0], Some(bytes::Bytes::from_static(b"42")));
            assert_eq!(param_values[1], None);
            assert!(result_formats.is_empty());
        }
        other => panic!("expected Bind, got {other:?}"),
    }
}

#[test]
fn parse_bind_message_rejects_oversized_portal_name() {
    let mut payload = BytesMut::new();
    payload.extend_from_slice(&vec![b'p'; MAX_FRONTEND_NAME_BYTES + 1]);
    payload.extend_from_slice(b"\0");
    payload.extend_from_slice(b"stmt1\0");
    payload.extend_from_slice(&0i16.to_be_bytes());
    payload.extend_from_slice(&0i16.to_be_bytes());
    payload.extend_from_slice(&0i16.to_be_bytes());

    let error =
        FrontendMessage::parse(b'B', payload).expect_err("oversized portal name must be rejected");
    assert!(error.to_string().contains("Bind portal name"));
    assert!(error.to_string().contains("maximum length"));
}

#[test]
fn parse_bind_message_rejects_trailing_bytes() {
    let mut payload = BytesMut::new();
    payload.extend_from_slice(b"\0");
    payload.extend_from_slice(b"stmt1\0");
    payload.extend_from_slice(&0i16.to_be_bytes());
    payload.extend_from_slice(&0i16.to_be_bytes());
    payload.extend_from_slice(&0i16.to_be_bytes());
    payload.extend_from_slice(b"x");

    let error =
        FrontendMessage::parse(b'B', payload).expect_err("trailing bind bytes must be rejected");
    assert!(error.to_string().contains("trailing bytes in Bind message"));
}

#[test]
fn write_parameter_description_basic() {
    let mut w = MessageWriter::new();
    write_parameter_description(&mut w, &[23, 25]).unwrap();
    let buf = w.finish_message();
    assert_eq!(buf[0], b't');
    let ncols = i16::from_be_bytes([buf[5], buf[6]]);
    assert_eq!(ncols, 2);
}

#[test]
fn write_row_description_single_column() {
    let mut w = MessageWriter::new();
    let fields = vec![FieldDescription {
        name: "id".to_string(),
        table_oid: 0,
        column_attr: 0,
        type_oid: 23,
        type_size: 4,
        type_modifier: -1,
        format_code: 0,
    }];
    write_row_description(&mut w, &fields).unwrap();
    let buf = w.finish_message();
    assert_eq!(buf[0], b'T');
    let ncols = i16::from_be_bytes([buf[5], buf[6]]);
    assert_eq!(ncols, 1);
}

// -----------------------------------------------------------------------
// Malformed / edge-case message parsing
// -----------------------------------------------------------------------

#[test]
fn parse_query_missing_null_terminator() {
    // A Query payload without a trailing null byte should fail.
    let payload = BytesMut::from(&b"SELECT 1"[..]);
    let result = FrontendMessage::parse(b'Q', payload);
    assert!(result.is_err());
}

#[test]
fn parse_describe_empty_payload() {
    // Describe with empty payload (no target byte) should fail.
    let payload = BytesMut::new();
    let result = FrontendMessage::parse(b'D', payload);
    assert!(result.is_err());
}

#[test]
fn parse_describe_invalid_target() {
    // Describe with invalid target byte 'X'.
    let mut payload = BytesMut::new();
    payload.extend_from_slice(b"X");
    payload.extend_from_slice(b"name\0");
    let result = FrontendMessage::parse(b'D', payload);
    assert!(result.is_err());
}

#[test]
fn parse_close_empty_payload() {
    // Close with empty payload (no target byte) should fail.
    let payload = BytesMut::new();
    let result = FrontendMessage::parse(b'C', payload);
    assert!(result.is_err());
}

#[test]
fn parse_close_invalid_target() {
    // Close with invalid target byte 'Z'.
    let mut payload = BytesMut::new();
    payload.extend_from_slice(b"Z");
    payload.extend_from_slice(b"name\0");
    let result = FrontendMessage::parse(b'C', payload);
    assert!(result.is_err());
}

#[test]
fn parse_execute_too_short() {
    // Execute with only the portal name but no max_rows i32.
    let payload = BytesMut::from(&b"\0"[..]);
    let result = FrontendMessage::parse(b'E', payload);
    assert!(result.is_err());
}

#[test]
fn parse_parse_message_too_short() {
    // Parse with only a statement name, no query or param count.
    let payload = BytesMut::from(&b"stmt\0"[..]);
    let result = FrontendMessage::parse(b'P', payload);
    assert!(result.is_err());
}

#[test]
fn parse_bind_truncated_param_value() {
    // Bind claims a parameter of length 100 but provides only 2 bytes.
    let mut payload = BytesMut::new();
    payload.extend_from_slice(b"\0"); // portal
    payload.extend_from_slice(b"stmt\0"); // statement
    payload.extend_from_slice(&0i16.to_be_bytes()); // 0 param formats
    payload.extend_from_slice(&1i16.to_be_bytes()); // 1 param value
    payload.extend_from_slice(&100i32.to_be_bytes()); // length 100
    payload.extend_from_slice(b"AB"); // only 2 bytes
    let result = FrontendMessage::parse(b'B', payload);
    assert!(result.is_err());
}

#[test]
fn parse_flush_message() {
    let payload = BytesMut::new();
    let msg = FrontendMessage::parse(b'H', payload).unwrap();
    assert!(matches!(msg, FrontendMessage::Flush));
}

#[test]
fn parse_password_message() {
    let payload = BytesMut::from(&b"secret123\0"[..]);
    let msg = FrontendMessage::parse(b'p', payload).unwrap();
    match msg {
        FrontendMessage::Password(pwd) => assert_eq!(pwd, "secret123"),
        other => panic!("expected Password, got {other:?}"),
    }
}

#[test]
fn parse_password_message_rejects_oversized_password() {
    let mut payload = BytesMut::new();
    payload.extend_from_slice(&vec![b's'; MAX_FRONTEND_PASSWORD_BYTES + 1]);
    payload.extend_from_slice(b"\0");

    let error =
        FrontendMessage::parse(b'p', payload).expect_err("oversized password must be rejected");
    assert!(error.to_string().contains("Password response"));
    assert!(error.to_string().contains("maximum length"));
}

#[test]
fn parse_copy_fail_rejects_oversized_message() {
    let mut payload = BytesMut::new();
    payload.extend_from_slice(&vec![b'f'; MAX_COPY_FAIL_MESSAGE_BYTES + 1]);
    payload.extend_from_slice(b"\0");

    let error =
        FrontendMessage::parse(b'f', payload).expect_err("oversized CopyFail must be rejected");
    assert!(error.to_string().contains("CopyFail message"));
    assert!(error.to_string().contains("maximum length"));
}

#[test]
fn write_auth_cleartext_password_produces_correct_bytes() {
    let mut w = MessageWriter::new();
    write_auth_cleartext_password(&mut w);
    let buf = w.finish_message();
    assert_eq!(buf[0], b'R');
    let len = u32::from_be_bytes([buf[1], buf[2], buf[3], buf[4]]);
    assert_eq!(len, 8); // 4 (length itself) + 4 (auth type)
    let auth_type = i32::from_be_bytes([buf[5], buf[6], buf[7], buf[8]]);
    assert_eq!(auth_type, 3); // CleartextPassword
}

// -----------------------------------------------------------------------
// Additional backend message serialization tests
// -----------------------------------------------------------------------

#[test]
fn write_empty_query_response_tag() {
    let mut w = MessageWriter::new();
    write_empty_query_response(&mut w);
    let buf = w.finish_message();
    assert_eq!(buf[0], b'I');
    let len = u32::from_be_bytes([buf[1], buf[2], buf[3], buf[4]]);
    assert_eq!(len, 4);
}

#[test]
fn write_close_complete_tag() {
    let mut w = MessageWriter::new();
    write_close_complete(&mut w);
    let buf = w.finish_message();
    assert_eq!(buf[0], b'3');
    let len = u32::from_be_bytes([buf[1], buf[2], buf[3], buf[4]]);
    assert_eq!(len, 4);
}

#[test]
fn write_portal_suspended_tag() {
    let mut w = MessageWriter::new();
    write_portal_suspended(&mut w);
    let buf = w.finish_message();
    assert_eq!(buf[0], b's');
    let len = u32::from_be_bytes([buf[1], buf[2], buf[3], buf[4]]);
    assert_eq!(len, 4);
}

#[test]
fn write_no_data_tag() {
    let mut w = MessageWriter::new();
    write_no_data(&mut w);
    let buf = w.finish_message();
    assert_eq!(buf[0], b'n');
    let len = u32::from_be_bytes([buf[1], buf[2], buf[3], buf[4]]);
    assert_eq!(len, 4);
}

#[test]
fn write_backend_key_data_values() {
    let mut w = MessageWriter::new();
    write_backend_key_data(&mut w, 42, 99);
    let buf = w.finish_message();
    assert_eq!(buf[0], b'K');
    let pid = u32::from_be_bytes([buf[5], buf[6], buf[7], buf[8]]);
    let secret = u32::from_be_bytes([buf[9], buf[10], buf[11], buf[12]]);
    assert_eq!(pid, 42);
    assert_eq!(secret, 99);
}

#[test]
fn write_parameter_status_contents() {
    let mut w = MessageWriter::new();
    write_parameter_status(&mut w, "server_version", "16.0");
    let buf = w.finish_message();
    assert_eq!(buf[0], b'S');
    let s = String::from_utf8_lossy(&buf);
    assert!(s.contains("server_version"));
    assert!(s.contains("16.0"));
}

#[test]
fn write_ready_for_query_in_transaction() {
    let mut w = MessageWriter::new();
    write_ready_for_query(&mut w, TransactionStatus::InTransaction);
    let buf = w.finish_message();
    assert_eq!(buf[0], b'Z');
    assert_eq!(buf[5], b'T');
}

#[test]
fn write_ready_for_query_failed() {
    let mut w = MessageWriter::new();
    write_ready_for_query(&mut w, TransactionStatus::Failed);
    let buf = w.finish_message();
    assert_eq!(buf[0], b'Z');
    assert_eq!(buf[5], b'E');
}

#[test]
fn write_row_description_empty() {
    let mut w = MessageWriter::new();
    write_row_description(&mut w, &[]).unwrap();
    let buf = w.finish_message();
    assert_eq!(buf[0], b'T');
    let ncols = i16::from_be_bytes([buf[5], buf[6]]);
    assert_eq!(ncols, 0);
}

#[test]
fn write_data_row_empty() {
    let mut w = MessageWriter::new();
    write_data_row(&mut w, &[]).unwrap();
    let buf = w.finish_message();
    assert_eq!(buf[0], b'D');
    let ncols = i16::from_be_bytes([buf[5], buf[6]]);
    assert_eq!(ncols, 0);
}

#[test]
fn write_data_row_all_nulls() {
    let mut w = MessageWriter::new();
    write_data_row(&mut w, &[None, None, None]).unwrap();
    let buf = w.finish_message();
    let ncols = i16::from_be_bytes([buf[5], buf[6]]);
    assert_eq!(ncols, 3);
    // Each NULL column is -1 (4 bytes), so payload after col count = 12.
    let len = u32::from_be_bytes([buf[1], buf[2], buf[3], buf[4]]);
    // 4 (length) + 2 (col count) + 3*4 (null markers) = 18
    assert_eq!(len, 18);
}

#[test]
fn write_error_response_with_detail_and_hint() {
    let mut report = ErrorReport::new(SqlState::InternalError, "boom");
    report.client_detail = Some("detail info".to_string());
    report.client_hint = Some("try again".to_string());
    let mut w = MessageWriter::new();
    write_error_response_from_report(&mut w, &report);
    let buf = w.finish_message();
    let s = String::from_utf8_lossy(&buf);
    assert!(s.contains("boom"));
    assert!(s.contains("detail info"));
    assert!(s.contains("try again"));
}

#[test]
fn write_error_response_sanitizes_embedded_nulls_in_fields() {
    let mut report = ErrorReport::new(SqlState::InternalError, "boom\0zap");
    report.client_detail = Some("detail\0info".to_string());
    report.client_hint = Some("try\0again".to_string());
    let mut w = MessageWriter::new();
    write_error_response_from_report(&mut w, &report);
    let buf = w.finish_message();
    let s = String::from_utf8_lossy(&buf);

    assert!(s.contains("boom zap"));
    assert!(s.contains("detail info"));
    assert!(s.contains("try again"));
    assert!(!buf
        .windows(b"boom\0zap".len())
        .any(|window| window == b"boom\0zap"));
    assert!(!buf
        .windows(b"detail\0info".len())
        .any(|window| window == b"detail\0info"));
    assert!(!buf
        .windows(b"try\0again".len())
        .any(|window| window == b"try\0again"));
}

#[test]
fn write_parameter_description_empty() {
    let mut w = MessageWriter::new();
    write_parameter_description(&mut w, &[]).unwrap();
    let buf = w.finish_message();
    assert_eq!(buf[0], b't');
    let ncols = i16::from_be_bytes([buf[5], buf[6]]);
    assert_eq!(ncols, 0);
}

// -----------------------------------------------------------------------
// Transaction status transitions
// -----------------------------------------------------------------------

#[test]
fn transaction_status_idle_byte() {
    assert_eq!(TransactionStatus::Idle.as_byte(), b'I');
}

#[test]
fn transaction_status_in_transaction_byte() {
    assert_eq!(TransactionStatus::InTransaction.as_byte(), b'T');
}

#[test]
fn transaction_status_failed_byte() {
    assert_eq!(TransactionStatus::Failed.as_byte(), b'E');
}

#[test]
fn transaction_status_clone_and_eq() {
    let s = TransactionStatus::InTransaction;
    let s2 = s;
    assert_eq!(s, s2);
}

#[test]
fn close_target_eq() {
    assert_eq!(CloseTarget::Statement, CloseTarget::Statement);
    assert_ne!(CloseTarget::Statement, CloseTarget::Portal);
}

#[test]
fn describe_target_eq() {
    assert_eq!(DescribeTarget::Statement, DescribeTarget::Statement);
    assert_ne!(DescribeTarget::Statement, DescribeTarget::Portal);
}
