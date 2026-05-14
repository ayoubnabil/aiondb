use super::*;
use crate::connection::helpers::result_columns_to_fields;

fn first_backend_payload(bytes: &[u8], wanted_tag: u8) -> &[u8] {
    let mut offset = 0;
    while offset < bytes.len() {
        let tag = bytes[offset];
        let len =
            u32::from_be_bytes(bytes[offset + 1..offset + 5].try_into().expect("length")) as usize;
        if tag == wanted_tag {
            return &bytes[offset + 5..offset + 1 + len];
        }
        offset += 1 + len;
    }
    panic!("backend message {wanted_tag:?} not found");
}

fn parse_data_row_columns(payload: &[u8]) -> Vec<Option<Vec<u8>>> {
    let mut offset = 0;
    let count = i16::from_be_bytes(payload[offset..offset + 2].try_into().expect("count")) as usize;
    offset += 2;

    let mut columns = Vec::with_capacity(count);
    for _ in 0..count {
        let len = i32::from_be_bytes(payload[offset..offset + 4].try_into().expect("len"));
        offset += 4;
        if len < 0 {
            columns.push(None);
            continue;
        }
        let len = len as usize;
        columns.push(Some(payload[offset..offset + len].to_vec()));
        offset += len;
    }
    columns
}

// -----------------------------------------------------------------------
// Helper function tests
// -----------------------------------------------------------------------

#[test]
fn data_type_to_pg_int() {
    let (oid, size) = data_type_to_pg(&DataType::Int);
    assert_eq!(size, 4);
    assert!(oid > 0);
}

#[test]
fn data_type_to_pg_bigint() {
    let (_oid, size) = data_type_to_pg(&DataType::BigInt);
    assert_eq!(size, 8);
}

#[test]
fn data_type_to_pg_timetz() {
    let (oid, size) = data_type_to_pg(&DataType::TimeTz);
    assert_eq!(oid, 1266);
    assert_eq!(size, 12);
}

#[test]
fn data_type_to_pg_boolean() {
    let (_oid, size) = data_type_to_pg(&DataType::Boolean);
    assert_eq!(size, 1);
}

#[test]
fn data_type_to_pg_text() {
    let (_oid, size) = data_type_to_pg(&DataType::Text);
    assert_eq!(size, -1); // variable length
}

#[test]
fn validate_bind_formats_accepts_default_and_text() {
    assert!(validate_bind_formats(&[], 2).is_ok());
    assert!(validate_bind_formats(&[0], 2).is_ok());
    assert!(validate_bind_formats(&[0, 0], 2).is_ok());
}

#[test]
fn validate_bind_formats_accepts_binary() {
    assert!(validate_bind_formats(&[1], 1).is_ok());
    assert!(validate_bind_formats(&[0, 1], 2).is_ok());
}

#[test]
fn checked_deadline_after_disables_overflowing_timeouts() {
    assert!(checked_deadline_after(Duration::ZERO, "test timeout").is_none());
    assert!(checked_deadline_after(Duration::from_millis(1), "test timeout").is_some());
    assert!(checked_deadline_after(Duration::MAX, "test timeout").is_none());
}

#[test]
fn validate_bind_formats_rejects_bad_counts() {
    let bad_count = validate_bind_formats(&[0, 0], 1).unwrap_err();
    assert!(bad_count
        .to_string()
        .contains("bind parameter format count mismatch"));
}

#[test]
fn validate_result_formats_accepts_binary() {
    assert!(validate_result_formats(&[1], 1).is_ok());
    assert!(validate_result_formats(&[0, 1], 2).is_ok());
}

#[test]
fn result_column_to_field_fmt_keeps_binary_for_supported_types() {
    let field = result_column_to_field_fmt(
        &ResultColumn {
            name: "id".to_owned(),
            data_type: DataType::Int,
            text_type_modifier: None,
            nullable: false,
        },
        &[1],
        0,
    );

    assert_eq!(field.format_code, 1);
}

#[test]
fn data_type_to_pg_vector_uses_custom_oid() {
    let (oid, size) = data_type_to_pg(&DataType::Vector {
        dims: 3,
        element_type: aiondb_core::VectorElementType::Float32,
    });
    assert_eq!(oid, 62_000);
    assert_eq!(size, -1);
}

#[test]
fn result_column_to_field_fmt_uses_varchar_oid_and_typmod() {
    let field = result_column_to_field_fmt(
        &ResultColumn {
            name: "v".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: Some(aiondb_core::TextTypeModifier::VarChar { length: 5 }),
            nullable: false,
        },
        &[],
        0,
    );

    assert_eq!(field.type_oid, 1043);
    assert_eq!(field.type_modifier, 9);
}

#[test]
fn result_column_to_field_fmt_uses_char_oid_and_typmod() {
    let field = result_column_to_field_fmt(
        &ResultColumn {
            name: "c".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: Some(aiondb_core::TextTypeModifier::Char { length: 3 }),
            nullable: false,
        },
        &[],
        0,
    );

    assert_eq!(field.type_oid, 1042);
    assert_eq!(field.type_modifier, 7);
}

#[test]
fn result_column_to_field_fmt_uses_varchar_array_oid_and_typmod() {
    let field = result_column_to_field_fmt(
        &ResultColumn {
            name: "va".to_owned(),
            data_type: DataType::Array(Box::new(DataType::Text)),
            text_type_modifier: Some(aiondb_core::TextTypeModifier::VarChar { length: 5 }),
            nullable: false,
        },
        &[],
        0,
    );

    assert_eq!(field.type_oid, 1015);
    assert_eq!(field.type_modifier, 9);
}

#[test]
fn result_column_to_field_fmt_uses_char_array_oid_and_typmod() {
    let field = result_column_to_field_fmt(
        &ResultColumn {
            name: "ca".to_owned(),
            data_type: DataType::Array(Box::new(DataType::Text)),
            text_type_modifier: Some(aiondb_core::TextTypeModifier::Char { length: 3 }),
            nullable: false,
        },
        &[],
        0,
    );

    assert_eq!(field.type_oid, 1014);
    assert_eq!(field.type_modifier, 7);
}

#[test]
fn result_column_to_field_fmt_uses_name_oid_and_typmod_minus_one() {
    let field = result_column_to_field_fmt(
        &ResultColumn {
            name: "n".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: Some(aiondb_core::TextTypeModifier::Name),
            nullable: false,
        },
        &[],
        0,
    );

    assert_eq!(field.type_oid, 19);
    assert_eq!(field.type_modifier, -1);
}

#[test]
fn result_column_to_field_fmt_uses_unbounded_varchar_oid_and_typmod_minus_one() {
    let field = result_column_to_field_fmt(
        &ResultColumn {
            name: "v".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: Some(aiondb_core::TextTypeModifier::VarCharAny),
            nullable: false,
        },
        &[],
        0,
    );

    assert_eq!(field.type_oid, 1043);
    assert_eq!(field.type_modifier, -1);
}

#[test]
fn result_column_to_field_fmt_uses_internal_char_oid_and_typmod_minus_one() {
    let field = result_column_to_field_fmt(
        &ResultColumn {
            name: "c".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: Some(aiondb_core::TextTypeModifier::InternalChar),
            nullable: false,
        },
        &[],
        0,
    );

    assert_eq!(field.type_oid, 18);
    assert_eq!(field.type_modifier, -1);
}

#[test]
fn result_column_to_field_fmt_uses_oid_alias_for_int_columns() {
    let field = result_column_to_field_fmt(
        &ResultColumn {
            name: "objoid".to_owned(),
            data_type: DataType::Int,
            text_type_modifier: Some(aiondb_core::TextTypeModifier::Oid),
            nullable: false,
        },
        &[],
        0,
    );

    assert_eq!(field.type_oid, 26);
    assert_eq!(field.type_size, 4);
    assert_eq!(field.type_modifier, -1);
}

#[test]
fn result_column_to_field_fmt_uses_oidvector_oid_and_typmod_minus_one() {
    let field = result_column_to_field_fmt(
        &ResultColumn {
            name: "proargtypes".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: Some(aiondb_core::TextTypeModifier::OidVector),
            nullable: false,
        },
        &[],
        0,
    );

    assert_eq!(field.type_oid, 30);
    assert_eq!(field.type_modifier, -1);
}

#[test]
fn result_column_to_field_fmt_uses_regproc_oid_and_typmod_minus_one() {
    let field = result_column_to_field_fmt(
        &ResultColumn {
            name: "typinput".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: Some(aiondb_core::TextTypeModifier::RegProc),
            nullable: false,
        },
        &[],
        0,
    );

    assert_eq!(field.type_oid, 24);
    assert_eq!(field.type_modifier, -1);
}

#[test]
fn result_column_to_field_fmt_uses_regclass_oid_and_typmod_minus_one() {
    let field = result_column_to_field_fmt(
        &ResultColumn {
            name: "regclass_param".to_owned(),
            data_type: DataType::Int,
            text_type_modifier: Some(aiondb_core::TextTypeModifier::RegClass),
            nullable: false,
        },
        &[],
        0,
    );

    assert_eq!(field.type_oid, 2205);
    assert_eq!(field.type_modifier, -1);
}

#[test]
fn pg_oid_to_data_type_maps_common_scalar_types() {
    assert_eq!(pg_oid_to_data_type(705), Some(DataType::Text));
    assert_eq!(pg_oid_to_data_type(23), Some(DataType::Int));
    assert_eq!(pg_oid_to_data_type(20), Some(DataType::BigInt));
    assert_eq!(pg_oid_to_data_type(19), Some(DataType::Text));
    assert_eq!(pg_oid_to_data_type(25), Some(DataType::Text));
    assert_eq!(pg_oid_to_data_type(16), Some(DataType::Boolean));
}

#[test]
fn pg_oid_to_data_type_maps_reg_aliases_to_int() {
    for oid in [
        24_u32, 2202, 2203, 2204, 2205, 2206, 3734, 3769, 4089, 4096, 4191,
    ] {
        assert_eq!(pg_oid_to_data_type(oid), Some(DataType::Int), "oid {oid}");
    }
}

#[test]
fn pg_oid_to_data_type_maps_reg_alias_arrays_to_int_arrays() {
    for oid in [
        1008_u32, 1028, 2207, 2208, 2209, 2210, 2211, 3735, 3770, 4090, 4097, 4192,
    ] {
        assert_eq!(
            pg_oid_to_data_type(oid),
            Some(DataType::Array(Box::new(DataType::Int))),
            "oid {oid}"
        );
    }
}

#[test]
fn pg_oid_to_data_type_maps_vector_and_arrays() {
    assert_eq!(
        pg_oid_to_data_type(62_000),
        Some(DataType::Vector {
            dims: 0,
            element_type: aiondb_core::VectorElementType::Float32
        })
    );
    assert_eq!(
        pg_oid_to_data_type(1007),
        Some(DataType::Array(Box::new(DataType::Int)))
    );
}

#[test]
fn pg_oid_to_data_type_maps_extended_scalar_types() {
    assert_eq!(pg_oid_to_data_type(18), Some(DataType::Text));
    assert_eq!(pg_oid_to_data_type(774), Some(DataType::MacAddr8));
    assert_eq!(pg_oid_to_data_type(790), Some(DataType::Money));
    assert_eq!(pg_oid_to_data_type(829), Some(DataType::MacAddr));
    assert_eq!(pg_oid_to_data_type(2275), Some(DataType::Text));
}

#[test]
fn pg_oid_to_data_type_maps_extended_array_types() {
    assert_eq!(
        pg_oid_to_data_type(1002),
        Some(DataType::Array(Box::new(DataType::Text)))
    );
    assert_eq!(
        pg_oid_to_data_type(791),
        Some(DataType::Array(Box::new(DataType::Money)))
    );
    assert_eq!(
        pg_oid_to_data_type(1003),
        Some(DataType::Array(Box::new(DataType::Text)))
    );
    assert_eq!(
        pg_oid_to_data_type(1014),
        Some(DataType::Array(Box::new(DataType::Text)))
    );
    assert_eq!(
        pg_oid_to_data_type(1010),
        Some(DataType::Array(Box::new(DataType::Tid)))
    );
    assert_eq!(
        pg_oid_to_data_type(1040),
        Some(DataType::Array(Box::new(DataType::MacAddr)))
    );
    assert_eq!(
        pg_oid_to_data_type(1183),
        Some(DataType::Array(Box::new(DataType::Time)))
    );
    assert_eq!(
        pg_oid_to_data_type(1185),
        Some(DataType::Array(Box::new(DataType::TimestampTz)))
    );
    assert_eq!(
        pg_oid_to_data_type(1231),
        Some(DataType::Array(Box::new(DataType::Numeric)))
    );
    assert_eq!(
        pg_oid_to_data_type(1270),
        Some(DataType::Array(Box::new(DataType::TimeTz)))
    );
    assert_eq!(
        pg_oid_to_data_type(3221),
        Some(DataType::Array(Box::new(DataType::PgLsn)))
    );
    assert_eq!(
        pg_oid_to_data_type(3807),
        Some(DataType::Array(Box::new(DataType::Jsonb)))
    );
}

#[test]
fn pg_oid_to_data_type_returns_none_for_unknown_oid() {
    assert_eq!(pg_oid_to_data_type(999_999), None);
}

#[test]
fn result_column_to_field_fmt_keeps_binary_for_array_columns() {
    let field = result_column_to_field_fmt(
        &ResultColumn {
            name: "items".to_owned(),
            data_type: DataType::Array(Box::new(DataType::Text)),
            text_type_modifier: None,
            nullable: false,
        },
        &[1],
        0,
    );

    assert_eq!(field.format_code, 1);
}

#[test]
fn result_columns_to_fields_uses_zero_table_oid_when_relation_id_overflows_pg_oid_space() {
    let columns = vec![ResultColumn {
        name: "id".to_owned(),
        data_type: DataType::Int,
        text_type_modifier: None,
        nullable: false,
    }];
    let origins = vec![Some(aiondb_engine::ResultColumnOrigin {
        relation_id: aiondb_core::RelationId::new(u64::MAX),
        column_attr: 7,
    })];

    let fields = result_columns_to_fields(&columns, &origins, &[]);

    assert_eq!(fields.len(), 1);
    assert_eq!(fields[0].table_oid, 0);
    assert_eq!(fields[0].column_attr, 7);
}

#[test]
fn validate_format_code_rejects_unknown() {
    let error = validate_format_code(2, "test").unwrap_err();
    assert!(error.to_string().contains("unknown test format code: 2"));
}

#[test]
fn coerce_bind_value_int() {
    let value = coerce_bind_value(1, &DataType::Int, Some(b"42")).unwrap();
    assert_eq!(value, Value::Int(42));
}

#[test]
fn coerce_bind_value_bigint() {
    let value = coerce_bind_value(1, &DataType::BigInt, Some(b"42000000000")).unwrap();
    assert_eq!(value, Value::BigInt(42_000_000_000));
}

#[test]
fn coerce_bind_value_text() {
    let value = coerce_bind_value(1, &DataType::Text, Some(b"alice")).unwrap();
    assert_eq!(value, Value::Text("alice".to_owned()));
}

#[test]
fn coerce_bind_value_boolean() {
    assert_eq!(
        coerce_bind_value(1, &DataType::Boolean, Some(b"true")).unwrap(),
        Value::Boolean(true)
    );
    assert_eq!(
        coerce_bind_value(1, &DataType::Boolean, Some(b"f")).unwrap(),
        Value::Boolean(false)
    );
}

#[test]
fn coerce_bind_value_null() {
    let value = coerce_bind_value(1, &DataType::Int, None).unwrap();
    assert_eq!(value, Value::Null);
}

#[test]
fn coerce_bind_value_rejects_invalid_text_and_unsupported_types() {
    let invalid_int = coerce_bind_value(1, &DataType::Int, Some(b"nope")).unwrap_err();
    assert!(invalid_int
        .to_string()
        .contains("invalid INT value for bind parameter $1"));

    let invalid_numeric = coerce_bind_value(1, &DataType::Numeric, Some(b"12.34.56")).unwrap_err();
    assert!(invalid_numeric
        .to_string()
        .contains("invalid NUMERIC value for bind parameter $1"));

    let invalid_date = coerce_bind_value(1, &DataType::Date, Some(b"2024-13-01")).unwrap_err();
    assert!(invalid_date
        .to_string()
        .contains("invalid DATE value for bind parameter $1"));

    // Blob is now supported -- verify text coercion with invalid hex.
    let invalid_blob = coerce_bind_value(1, &DataType::Blob, Some(b"\\xGGHH")).unwrap_err();
    assert!(invalid_blob.to_string().contains("invalid BYTEA hex value"));
}

#[test]
fn coerce_bind_value_supports_numeric_and_uuid() {
    let numeric = coerce_bind_value(1, &DataType::Numeric, Some(b"12.34")).unwrap();
    assert_eq!(
        numeric,
        Value::Numeric(aiondb_core::NumericValue::new(1234, 2))
    );

    let uuid = coerce_bind_value(
        2,
        &DataType::Uuid,
        Some(b"550e8400-e29b-41d4-a716-446655440000"),
    )
    .unwrap();
    assert!(matches!(uuid, Value::Uuid(_)));
}

#[test]
fn coerce_bind_value_supports_floats_and_temporal_types() {
    let real = coerce_bind_value(1, &DataType::Real, Some(b"3.5")).unwrap();
    assert_eq!(real, Value::Real(3.5));

    let double = coerce_bind_value(2, &DataType::Double, Some(b"3.1415926535")).unwrap();
    assert_eq!(double, Value::Double(3.1415926535));

    let date = coerce_bind_value(3, &DataType::Date, Some(b"2024-03-15")).unwrap();
    assert!(matches!(date, Value::Date(_)));

    let time = coerce_bind_value(4, &DataType::Time, Some(b"12:34:56.789")).unwrap();
    assert!(matches!(time, Value::Time(_)));

    let timetz = coerce_bind_value(5, &DataType::TimeTz, Some(b"12:34:56.789+02:30")).unwrap();
    assert_eq!(
        timetz,
        Value::TimeTz(
            time::Time::from_hms_micro(12, 34, 56, 789_000).unwrap(),
            time::UtcOffset::from_hms(2, 30, 0).unwrap()
        )
    );

    let timestamp =
        coerce_bind_value(6, &DataType::Timestamp, Some(b"2024-03-15 12:34:56.789")).unwrap();
    assert!(matches!(timestamp, Value::Timestamp(_)));

    let timestamptz = coerce_bind_value(
        7,
        &DataType::TimestampTz,
        Some(b"2024-03-15 12:34:56.789+02:30"),
    )
    .unwrap();
    assert!(matches!(timestamptz, Value::TimestampTz(_)));

    let interval = coerce_bind_value(8, &DataType::Interval, Some(b"2m 3d 400us")).unwrap();
    assert_eq!(
        interval,
        Value::Interval(aiondb_core::IntervalValue::new(2, 3, 400))
    );
}

#[test]
fn write_portal_batch_suspends_partial_query_batches() {
    let batch = PortalBatch {
        columns: vec![ResultColumn {
            name: "id".to_owned(),
            data_type: DataType::Int,
            text_type_modifier: None,
            nullable: true,
        }],
        rows: vec![Row::new(vec![Value::Int(1)])],
        tag: "SELECT".to_owned(),
        rows_affected: 0,
        exhausted: false,
    };
    let mut writer = MessageWriter::new();

    Connection::<MockEngine, Cursor<Vec<u8>>, Vec<u8>>::write_portal_batch(
        &mut writer,
        &batch,
        &[],
    )
    .unwrap();

    let bytes = writer.finish_message();
    assert_eq!(backend_message_tags(&bytes), vec![b'D', b's']);
}

#[test]
fn write_portal_batch_completes_exhausted_query_batches() {
    let batch = PortalBatch {
        columns: vec![ResultColumn {
            name: "id".to_owned(),
            data_type: DataType::Int,
            text_type_modifier: None,
            nullable: true,
        }],
        rows: vec![Row::new(vec![Value::Int(1)])],
        tag: "SELECT".to_owned(),
        rows_affected: 0,
        exhausted: true,
    };
    let mut writer = MessageWriter::new();

    Connection::<MockEngine, Cursor<Vec<u8>>, Vec<u8>>::write_portal_batch(
        &mut writer,
        &batch,
        &[],
    )
    .unwrap();

    let bytes = writer.finish_message();
    assert_eq!(backend_message_tags(&bytes), vec![b'D', b'C']);
}

#[test]
fn write_portal_batch_uses_precomputed_select_row_count_tag() {
    let batch = PortalBatch {
        columns: vec![ResultColumn {
            name: "id".to_owned(),
            data_type: DataType::Int,
            text_type_modifier: None,
            nullable: true,
        }],
        rows: vec![Row::new(vec![Value::Int(1)])],
        tag: "SELECT 2".to_owned(),
        rows_affected: 0,
        exhausted: true,
    };
    let mut writer = MessageWriter::new();

    Connection::<MockEngine, Cursor<Vec<u8>>, Vec<u8>>::write_portal_batch(
        &mut writer,
        &batch,
        &[],
    )
    .unwrap();

    let bytes = writer.finish_message();
    let payload = first_backend_payload(&bytes, b'C');
    assert_eq!(
        std::str::from_utf8(&payload[..payload.len() - 1]).expect("utf8 command tag"),
        "SELECT 2"
    );
}

#[test]
fn write_portal_batch_writes_notice_response_for_notice_batches() {
    let batch = PortalBatch {
        columns: vec![],
        rows: vec![],
        tag: "NOTICE: compatibility notice".to_owned(),
        rows_affected: 0,
        exhausted: true,
    };
    let mut writer = MessageWriter::new();

    Connection::<MockEngine, Cursor<Vec<u8>>, Vec<u8>>::write_portal_batch(
        &mut writer,
        &batch,
        &[],
    )
    .unwrap();

    let bytes = writer.finish_message();
    assert_eq!(backend_message_tags(&bytes), vec![b'N', b'C']);
    let notice_payload = first_backend_payload(&bytes, b'N');
    let text = String::from_utf8_lossy(notice_payload);
    assert!(text.contains("NOTICE"));
    assert!(text.contains("compatibility notice"));
    let command_payload = first_backend_payload(&bytes, b'C');
    assert_eq!(
        std::str::from_utf8(&command_payload[..command_payload.len() - 1])
            .expect("utf8 command tag"),
        "SELECT 0"
    );
}

#[test]
fn write_portal_batch_converts_empty_sentinel_to_select_zero() {
    let batch = PortalBatch {
        columns: vec![],
        rows: vec![],
        tag: "EMPTY".to_owned(),
        rows_affected: 0,
        exhausted: true,
    };
    let mut writer = MessageWriter::new();

    Connection::<MockEngine, Cursor<Vec<u8>>, Vec<u8>>::write_portal_batch(
        &mut writer,
        &batch,
        &[],
    )
    .unwrap();

    let bytes = writer.finish_message();
    let payload = first_backend_payload(&bytes, b'C');
    assert_eq!(
        std::str::from_utf8(&payload[..payload.len() - 1]).expect("utf8 command tag"),
        "SELECT 0"
    );
}

#[test]
fn write_portal_batch_formats_command_rows_affected() {
    let batch = PortalBatch {
        columns: vec![],
        rows: vec![],
        tag: "UPDATE".to_owned(),
        rows_affected: 2,
        exhausted: true,
    };
    let mut writer = MessageWriter::new();

    Connection::<MockEngine, Cursor<Vec<u8>>, Vec<u8>>::write_portal_batch(
        &mut writer,
        &batch,
        &[],
    )
    .unwrap();

    let bytes = writer.finish_message();
    let payload = first_backend_payload(&bytes, b'C');
    assert_eq!(
        std::str::from_utf8(&payload[..payload.len() - 1]).expect("utf8 command tag"),
        "UPDATE 2"
    );
}

#[test]
fn write_portal_batch_uses_binary_bytes_for_supported_binary_columns() {
    let batch = PortalBatch {
        columns: vec![ResultColumn {
            name: "id".to_owned(),
            data_type: DataType::Int,
            text_type_modifier: None,
            nullable: true,
        }],
        rows: vec![Row::new(vec![Value::Int(42)])],
        tag: "SELECT".to_owned(),
        rows_affected: 0,
        exhausted: true,
    };
    let mut writer = MessageWriter::new();

    Connection::<MockEngine, Cursor<Vec<u8>>, Vec<u8>>::write_portal_batch(
        &mut writer,
        &batch,
        &[1],
    )
    .unwrap();

    let bytes = writer.finish_message();
    let row = parse_data_row_columns(first_backend_payload(&bytes, b'D'));
    assert_eq!(row, vec![Some(42_i32.to_be_bytes().to_vec())]);
}

#[test]
fn write_portal_batch_keeps_binary_bytes_for_array_columns() {
    let value = Value::Array(vec![
        Value::Text("a".to_owned()),
        Value::Text("b".to_owned()),
    ]);
    let batch = PortalBatch {
        columns: vec![ResultColumn {
            name: "items".to_owned(),
            data_type: DataType::Array(Box::new(DataType::Text)),
            text_type_modifier: None,
            nullable: true,
        }],
        rows: vec![Row::new(vec![value.clone()])],
        tag: "SELECT".to_owned(),
        rows_affected: 0,
        exhausted: true,
    };
    let mut writer = MessageWriter::new();

    Connection::<MockEngine, Cursor<Vec<u8>>, Vec<u8>>::write_portal_batch(
        &mut writer,
        &batch,
        &[1],
    )
    .unwrap();

    let bytes = writer.finish_message();
    let row = parse_data_row_columns(first_backend_payload(&bytes, b'D'));
    assert_eq!(
        row,
        vec![Some(
            crate::binary_format::encode_binary_value(&value).unwrap()
        )]
    );
}

#[test]
fn write_portal_batch_rejects_row_width_mismatch_when_columns_missing() {
    let batch = PortalBatch {
        columns: vec![],
        rows: vec![Row::new(vec![Value::Int(1)])],
        tag: "SELECT".to_owned(),
        rows_affected: 0,
        exhausted: true,
    };
    let mut writer = MessageWriter::new();

    let error = Connection::<MockEngine, Cursor<Vec<u8>>, Vec<u8>>::write_portal_batch(
        &mut writer,
        &batch,
        &[],
    )
    .expect_err("row width mismatch must error");

    assert!(error
        .to_string()
        .contains("portal batch row 1 has 1 value(s)"));
    assert!(
        writer.is_empty(),
        "no partial pgwire frames should be emitted"
    );
}

#[tokio::test]
async fn write_portal_batch_rejects_row_width_mismatch_without_emitting_partial_frames() {
    let batch = PortalBatch {
        columns: vec![ResultColumn {
            name: "id".to_owned(),
            data_type: DataType::Int,
            text_type_modifier: None,
            nullable: true,
        }],
        rows: vec![Row::new(vec![])],
        tag: "SELECT".to_owned(),
        rows_affected: 0,
        exhausted: true,
    };
    let mut writer = MessageWriter::new();

    let error = Connection::<MockEngine, Cursor<Vec<u8>>, Vec<u8>>::write_portal_batch(
        &mut writer,
        &batch,
        &[],
    )
    .expect_err("row width mismatch must error");

    assert!(error
        .to_string()
        .contains("portal batch row 1 has 0 value(s)"));
    assert!(
        writer.is_empty(),
        "no partial pgwire frames should be emitted"
    );
}

#[tokio::test]
async fn handle_close_statement_cleans_portal_wire_state_for_related_portals() {
    let engine = std::sync::Arc::new(MockEngine::new());
    let mut conn = Connection::new(
        engine,
        Cursor::new(Vec::new()),
        Vec::new(),
        7,
        11,
        CancelRegistry::new(),
    );
    conn.session = Some(SessionHandle::test_handle());
    conn.statement_wire_state.insert(
        "stmt1".to_owned(),
        StatementWireState {
            param_oids: vec![1043],
            query: "SELECT $1".to_owned(),
            prepared_desc: None,
            deferred_describe_response_cache: std::collections::HashMap::new(),
            direct_param_result_alias_slots: None,
            parsed_statement: None,
            parsed_statement_kind: ParsedStatementKind::Other,
        },
    );
    conn.statement_wire_state.insert(
        "stmt2".to_owned(),
        StatementWireState {
            param_oids: vec![23],
            query: "SELECT $1".to_owned(),
            prepared_desc: None,
            deferred_describe_response_cache: std::collections::HashMap::new(),
            direct_param_result_alias_slots: None,
            parsed_statement: None,
            parsed_statement_kind: ParsedStatementKind::Other,
        },
    );
    conn.portal_wire_state.insert(
        "p_stmt1".to_owned(),
        PortalWireState {
            result_formats: Arc::from(vec![0]),
            statement_name: "stmt1".to_owned(),
            rows_sent: 3,
            created_under_savepoint_generation: None,
            deferred_bind_params: None,
            deferred_describe_response: None,
        },
    );
    conn.portal_wire_state.insert(
        "p_stmt2".to_owned(),
        PortalWireState {
            result_formats: Arc::from(vec![1]),
            statement_name: "stmt2".to_owned(),
            rows_sent: 1,
            created_under_savepoint_generation: None,
            deferred_bind_params: None,
            deferred_describe_response: None,
        },
    );

    conn.handle_close(CloseTarget::Statement, "stmt1")
        .await
        .expect("close statement");

    assert!(!conn.statement_wire_state.contains_key("stmt1"));
    assert!(conn.statement_wire_state.contains_key("stmt2"));
    assert!(!conn.portal_wire_state.contains_key("p_stmt1"));
    assert!(conn.portal_wire_state.contains_key("p_stmt2"));
}

#[tokio::test]
async fn handle_bind_rejects_new_portal_before_calling_engine_when_limit_is_reached() {
    let engine = std::sync::Arc::new(MockEngine::new());
    let mut conn = Connection::new(
        engine.clone(),
        Cursor::new(Vec::new()),
        Vec::new(),
        13,
        17,
        CancelRegistry::new(),
    );
    conn.session = Some(SessionHandle::test_handle());
    conn.max_portals = 1;
    conn.portal_wire_state.insert(
        "existing".to_owned(),
        PortalWireState {
            result_formats: Arc::from(Vec::<i16>::new()),
            statement_name: "stmt_existing".to_owned(),
            rows_sent: 0,
            created_under_savepoint_generation: None,
            deferred_bind_params: None,
            deferred_describe_response: None,
        },
    );

    conn.handle_bind("new_portal", "stmt_new", &[], &[], &[])
        .await
        .expect("bind should return protocol error response, not panic");

    assert_eq!(engine.bind_calls(), 0);
    assert!(conn.portal_wire_state.contains_key("existing"));
    assert!(!conn.portal_wire_state.contains_key("new_portal"));
}

#[tokio::test]
async fn handle_bind_named_portal_defers_engine_bind_until_execute() {
    let engine = std::sync::Arc::new(MockEngine::new());
    let mut conn = Connection::new(
        engine.clone(),
        Cursor::new(Vec::new()),
        Vec::new(),
        19,
        23,
        CancelRegistry::new(),
    );
    conn.session = Some(SessionHandle::test_handle());
    conn.statement_wire_state.insert(
        "stmt_named".to_owned(),
        StatementWireState {
            param_oids: vec![],
            query: "SELECT 1".to_owned(),
            prepared_desc: Some(PreparedStatementDesc {
                name: "stmt_named".to_owned(),
                param_types: vec![],
                result_columns: vec![],
                result_column_origins: vec![],
            }),
            deferred_describe_response_cache: std::collections::HashMap::new(),
            direct_param_result_alias_slots: None,
            parsed_statement: None,
            parsed_statement_kind: ParsedStatementKind::Other,
        },
    );

    conn.handle_bind("named_portal", "stmt_named", &[], &[], &[])
        .await
        .expect("bind named portal");

    assert_eq!(engine.bind_calls(), 0);
    assert!(conn
        .portal_wire_state
        .get("named_portal")
        .and_then(|portal| portal.deferred_bind_params.as_ref())
        .is_some());

    conn.handle_execute("named_portal", 0)
        .await
        .expect("execute named portal");

    assert_eq!(engine.bind_calls(), 1);
    assert!(conn
        .portal_wire_state
        .get("named_portal")
        .and_then(|portal| portal.deferred_bind_params.as_ref())
        .is_none());
}

#[tokio::test]
async fn handle_describe_statement_reuses_parse_descriptor_without_engine_roundtrip() {
    let engine = std::sync::Arc::new(MockEngine::new());
    let mut conn = Connection::new(
        engine.clone(),
        Cursor::new(Vec::new()),
        Vec::new(),
        29,
        31,
        CancelRegistry::new(),
    );
    conn.session = Some(SessionHandle::test_handle());
    conn.statement_wire_state.insert(
        "stmt_cached".to_owned(),
        StatementWireState {
            param_oids: vec![],
            query: "SELECT 1".to_owned(),
            prepared_desc: Some(PreparedStatementDesc {
                name: "stmt_cached".to_owned(),
                param_types: vec![],
                result_columns: vec![],
                result_column_origins: vec![],
            }),
            deferred_describe_response_cache: std::collections::HashMap::new(),
            direct_param_result_alias_slots: None,
            parsed_statement: None,
            parsed_statement_kind: ParsedStatementKind::Other,
        },
    );

    conn.handle_describe(DescribeTarget::Statement, "stmt_cached")
        .await
        .expect("describe statement");

    assert_eq!(engine.describe_statement_calls(), 0);
}

#[test]
fn apply_wire_state_cleanup_deallocate_all_keeps_named_portals_bound_to_unnamed_statement() {
    let engine = std::sync::Arc::new(MockEngine::new());
    let mut conn = Connection::new(
        engine,
        Cursor::new(Vec::new()),
        Vec::new(),
        13,
        17,
        CancelRegistry::new(),
    );
    conn.statement_wire_state
        .insert(String::new(), StatementWireState::default());
    conn.statement_wire_state
        .insert("named_stmt".to_owned(), StatementWireState::default());
    conn.portal_wire_state.insert(
        "named_on_unnamed".to_owned(),
        PortalWireState {
            result_formats: Arc::from(Vec::<i16>::new()),
            statement_name: String::new(),
            rows_sent: 0,
            created_under_savepoint_generation: None,
            deferred_bind_params: None,
            deferred_describe_response: None,
        },
    );
    conn.portal_wire_state.insert(
        "named_on_named".to_owned(),
        PortalWireState {
            result_formats: Arc::from(Vec::<i16>::new()),
            statement_name: "named_stmt".to_owned(),
            rows_sent: 0,
            created_under_savepoint_generation: None,
            deferred_bind_params: None,
            deferred_describe_response: None,
        },
    );

    conn.apply_wire_state_cleanup(&WireStateCleanupHint::DeallocateAll);

    assert!(conn.statement_wire_state.contains_key(""));
    assert!(!conn.statement_wire_state.contains_key("named_stmt"));
    assert!(conn.portal_wire_state.contains_key("named_on_unnamed"));
    assert!(!conn.portal_wire_state.contains_key("named_on_named"));
}
