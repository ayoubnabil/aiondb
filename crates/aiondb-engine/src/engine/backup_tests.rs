use super::*;
use aiondb_core::{IntervalValue, NumericValue, VectorValue};

fn literal(value: &Value) -> String {
    value_to_sql_literal(value).unwrap()
}

#[test]
fn literal_null() {
    assert_eq!(literal(&Value::Null), "NULL");
}

#[test]
fn literal_int() {
    assert_eq!(literal(&Value::Int(42)), "42");
}

#[test]
fn literal_bigint() {
    assert_eq!(literal(&Value::BigInt(100)), "100");
}

#[test]
fn literal_negative_int() {
    assert_eq!(literal(&Value::Int(-5)), "-5");
}

#[test]
fn literal_text() {
    assert_eq!(literal(&Value::Text("hello".to_owned())), "'hello'");
}

#[test]
fn literal_text_with_quote() {
    assert_eq!(literal(&Value::Text("it's".to_owned())), "'it''s'");
}

#[test]
fn literal_boolean_true() {
    assert_eq!(literal(&Value::Boolean(true)), "TRUE");
}

#[test]
fn literal_boolean_false() {
    assert_eq!(literal(&Value::Boolean(false)), "FALSE");
}

#[test]
fn literal_double() {
    let lit = literal(&Value::Double(3.14));
    assert!(lit.contains("3.14"));
}

#[test]
fn literal_real() {
    let lit = literal(&Value::Real(1.5));
    assert!(lit.contains("1.5"));
}

#[test]
fn literal_numeric() {
    let lit = literal(&Value::Numeric(NumericValue::new(12345, 2)));
    assert_eq!(lit, "123.45");
}

#[test]
fn literal_uuid() {
    let bytes = [
        0x55, 0x0e, 0x84, 0x00, 0xe2, 0x9b, 0x41, 0xd4, 0xa7, 0x16, 0x44, 0x66, 0x55, 0x44, 0x00,
        0x00,
    ];
    let lit = literal(&Value::Uuid(bytes));
    assert!(lit.contains("AS UUID)"));
    assert!(lit.contains("550e8400-e29b-41d4-a716-446655440000"));
}

#[test]
fn literal_blob() {
    let lit = literal(&Value::Blob(vec![0xCA, 0xFE]));
    assert!(lit.contains("cafe"));
}

#[test]
fn literal_vector() {
    let v = VectorValue::new(3, vec![1.0, 2.0, 3.0]);
    let lit = literal(&Value::Vector(v));
    assert!(lit.contains("AS VECTOR(3))"));
    assert!(lit.contains("[1,2,3]"));
}

#[test]
fn literal_interval() {
    let iv = IntervalValue::new(1, 2, 3);
    let lit = literal(&Value::Interval(iv));
    assert!(lit.contains("AS INTERVAL)"));
}

#[test]
fn literal_array() {
    let arr = vec![Value::Int(1), Value::Int(2), Value::Int(3)];
    let lit = literal(&Value::Array(arr));
    assert_eq!(lit, "ARRAY[1, 2, 3]");
}

#[test]
fn literal_array_with_null() {
    let arr = vec![Value::Int(1), Value::Null];
    let lit = literal(&Value::Array(arr));
    assert_eq!(lit, "ARRAY[1, NULL]");
}

#[test]
fn format_data_type_all_variants() {
    assert_eq!(format_data_type(&DataType::Int), "INT");
    assert_eq!(format_data_type(&DataType::BigInt), "BIGINT");
    assert_eq!(format_data_type(&DataType::Real), "REAL");
    assert_eq!(format_data_type(&DataType::Double), "DOUBLE");
    assert_eq!(format_data_type(&DataType::Numeric), "NUMERIC");
    assert_eq!(format_data_type(&DataType::Text), "TEXT");
    assert_eq!(format_data_type(&DataType::Boolean), "BOOLEAN");
    assert_eq!(format_data_type(&DataType::Blob), "BLOB");
    assert_eq!(format_data_type(&DataType::Timestamp), "TIMESTAMP");
    assert_eq!(format_data_type(&DataType::Date), "DATE");
    assert_eq!(format_data_type(&DataType::Time), "TIME");
    assert_eq!(format_data_type(&DataType::TimeTz), "TIMETZ");
    assert_eq!(format_data_type(&DataType::Interval), "INTERVAL");
    assert_eq!(format_data_type(&DataType::Uuid), "UUID");
    assert_eq!(format_data_type(&DataType::TimestampTz), "TIMESTAMPTZ");
    assert_eq!(format_data_type(&DataType::Jsonb), "JSONB");
    assert_eq!(
        format_data_type(&DataType::Vector {
            dims: 3,
            element_type: aiondb_core::VectorElementType::Float32
        }),
        "VECTOR(3)"
    );
    assert_eq!(
        format_data_type(&DataType::Array(Box::new(DataType::Int))),
        "INT[]"
    );
}

#[test]
fn escape_sql_string_no_quotes() {
    assert_eq!(escape_sql_string("hello"), "hello");
}

#[test]
fn escape_sql_string_with_quotes() {
    assert_eq!(escape_sql_string("it's a test"), "it''s a test");
}

#[test]
fn escape_sql_string_multiple_quotes() {
    assert_eq!(escape_sql_string("''"), "''''");
}

#[test]
fn format_identifier_simple() {
    assert_eq!(format_identifier("users"), "\"users\"");
}

#[test]
fn format_identifier_preserves_name() {
    assert_eq!(format_identifier("my_table"), "\"my_table\"");
}

#[test]
fn format_object_name_preserves_schema_qualification() {
    assert_eq!(
        format_object_name(&QualifiedName::qualified("analytics", "events")),
        "\"analytics\".\"events\""
    );
}

#[test]
fn format_float_normal() {
    let s = format_float(3.14);
    assert!(s.contains("3.14"));
}

#[test]
fn format_float_nan() {
    assert_eq!(format_float(f64::NAN), "CAST('NaN' AS DOUBLE)");
}

#[test]
fn format_float_infinity() {
    assert_eq!(format_float(f64::INFINITY), "CAST('Infinity' AS DOUBLE)");
}

#[test]
fn format_float_neg_infinity() {
    assert_eq!(
        format_float(f64::NEG_INFINITY),
        "CAST('-Infinity' AS DOUBLE)"
    );
}

#[test]
fn hex_encode_empty() {
    assert_eq!(hex_encode(&[]), "");
}

#[test]
fn hex_encode_values() {
    assert_eq!(hex_encode(&[0xCA, 0xFE, 0xBA, 0xBE]), "cafebabe");
}

#[test]
fn validate_path_rejects_parent_dir() {
    assert!(validate_path("../etc/passwd").is_err());
    assert!(validate_path("foo/../../bar").is_err());
}

#[test]
fn validate_path_rejects_absolute_paths() {
    assert!(validate_path("/tmp/backup.sql").is_err());
    assert!(validate_path("/etc/shadow").is_err());
    assert!(validate_path("/tmp/../etc/passwd").is_err());
}

#[test]
fn validate_path_accepts_relative_paths() {
    assert!(validate_path("backup.sql").is_ok());
    assert!(validate_path("backups/daily/2026-03-07.sql").is_ok());
}

#[test]
fn resolve_backup_path_from_base_keeps_relative_path_under_base() {
    let base = unique_backup_test_dir("resolve-relative");
    std::fs::create_dir_all(&base).expect("create base dir");

    let resolved = resolve_backup_path_from_base(&base, "backups/daily/snapshot.sql")
        .expect("path should resolve under base");
    assert_eq!(resolved, base.join("backups/daily/snapshot.sql"));

    let _ = std::fs::remove_dir_all(base);
}

#[cfg(unix)]
#[test]
fn resolve_backup_path_from_base_rejects_symlink_leaf() {
    let base = unique_backup_test_dir("reject-leaf-symlink");
    let outside = unique_backup_test_dir("outside-leaf-symlink");
    std::fs::create_dir_all(base.join("backups")).expect("create base tree");
    std::fs::create_dir_all(&outside).expect("create outside dir");
    std::fs::write(outside.join("shadow.sql"), "secret").expect("write outside file");
    std::os::unix::fs::symlink(outside.join("shadow.sql"), base.join("backups/latest.sql"))
        .expect("create symlink");

    let err = resolve_backup_path_from_base(&base, "backups/latest.sql")
        .expect_err("symlink leaf must be rejected");
    assert!(format!("{err}").contains("symlinks"));

    let _ = std::fs::remove_dir_all(base);
    let _ = std::fs::remove_dir_all(outside);
}

#[cfg(unix)]
#[test]
fn resolve_backup_path_from_base_rejects_symlink_directory() {
    let base = unique_backup_test_dir("reject-dir-symlink");
    let outside = unique_backup_test_dir("outside-dir-symlink");
    std::fs::create_dir_all(&base).expect("create base dir");
    std::fs::create_dir_all(outside.join("nested")).expect("create outside tree");
    std::os::unix::fs::symlink(&outside, base.join("backups")).expect("create dir symlink");

    let err = resolve_backup_path_from_base(&base, "backups/nested/snapshot.sql")
        .expect_err("symlink directory must be rejected");
    assert!(format!("{err}").contains("symlinks"));

    let _ = std::fs::remove_dir_all(base);
    let _ = std::fs::remove_dir_all(outside);
}

#[cfg(unix)]
#[test]
fn read_restore_file_rejects_symbolic_link_path() {
    let base = unique_backup_test_dir("read-restore-symlink-base");
    let outside = unique_backup_test_dir("read-restore-symlink-outside");
    std::fs::create_dir_all(&base).expect("create base dir");
    std::fs::create_dir_all(&outside).expect("create outside dir");

    let target = outside.join("restore.sql");
    std::fs::write(&target, "SELECT 1;").expect("write target restore file");

    let symlink_path = base.join("restore-link.sql");
    std::os::unix::fs::symlink(&target, &symlink_path).expect("create symlink restore file");

    let err = read_restore_file(&symlink_path).expect_err("symlink restore path must fail");
    assert!(
        format!("{err}").contains("must not be a symbolic link"),
        "unexpected error: {err}"
    );

    let _ = std::fs::remove_dir_all(base);
    let _ = std::fs::remove_dir_all(outside);
}

#[test]
fn write_new_backup_file_refuses_overwrite() {
    let base = unique_backup_test_dir("write-refuses-overwrite");
    std::fs::create_dir_all(&base).expect("create base dir");
    let path = base.join("snapshot.sql");

    write_new_backup_file(&path, b"first").expect("initial backup write");
    let err = write_new_backup_file(&path, b"second").expect_err("overwrite must fail");

    assert!(
        format!("{err}").contains("refusing to overwrite existing backup file"),
        "unexpected error: {err}"
    );
    assert_eq!(std::fs::read(&path).expect("read backup"), b"first");

    let _ = std::fs::remove_dir_all(base);
}

#[cfg(unix)]
#[test]
fn write_new_backup_file_creates_private_file() {
    use std::os::unix::fs::PermissionsExt;

    let base = unique_backup_test_dir("write-private-file");
    std::fs::create_dir_all(&base).expect("create base dir");
    let path = base.join("snapshot.sql");

    write_new_backup_file(&path, b"secret backup").expect("backup write");

    let mode = std::fs::metadata(&path)
        .expect("backup metadata")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600);

    let _ = std::fs::remove_dir_all(base);
}

fn unique_backup_test_dir(name: &str) -> std::path::PathBuf {
    crate::test_support::unique_temp_path("engine-backup-tests", name)
}

#[test]
fn parse_backup_manifest_accepts_current_versioned_header() {
    let payload = "SELECT 1;";
    let sql = format!(
        "{BACKUP_HEADER_BANNER}\n{BACKUP_HEADER_FORMAT_PREFIX} {CURRENT_BACKUP_FORMAT_VERSION}\n{BACKUP_HEADER_ENGINE_PREFIX} 0.1.0\n{BACKUP_HEADER_PAYLOAD_SHA256_PREFIX} {}\n\n{payload}",
        payload_sha256(payload)
    );
    assert_eq!(
        parse_backup_manifest(&sql).expect("manifest should parse"),
        BackupManifest {
            format_version: CURRENT_BACKUP_FORMAT_VERSION,
            engine_version: Some("0.1.0".to_owned()),
            payload_sha256: Some(payload_sha256(payload)),
        }
    );
}

#[test]
fn parse_backup_manifest_accepts_previous_version_without_checksum() {
    let sql = format!(
        "{BACKUP_HEADER_BANNER}\n{BACKUP_HEADER_FORMAT_PREFIX} 1\n{BACKUP_HEADER_ENGINE_PREFIX} 0.1.0\n\nSELECT 1;"
    );
    assert_eq!(
        parse_backup_manifest(&sql).expect("v1 manifest should parse"),
        BackupManifest {
            format_version: 1,
            engine_version: Some("0.1.0".to_owned()),
            payload_sha256: None,
        }
    );
}

#[test]
fn parse_backup_manifest_accepts_legacy_banner_only() {
    let sql = format!("{BACKUP_HEADER_BANNER}\nCREATE TABLE t (id INT);");
    assert_eq!(
        parse_backup_manifest(&sql).expect("legacy manifest should parse"),
        BackupManifest {
            format_version: LEGACY_BACKUP_FORMAT_VERSION,
            engine_version: None,
            payload_sha256: None,
        }
    );
}

#[test]
fn parse_backup_manifest_accepts_raw_sql_legacy_import() {
    let sql = "CREATE TABLE t (id INT);";
    assert_eq!(
        parse_backup_manifest(sql).expect("raw SQL should be treated as legacy"),
        BackupManifest::legacy()
    );
}

#[test]
fn parse_backup_manifest_rejects_future_version() {
    let sql = format!(
        "{BACKUP_HEADER_BANNER}\n{BACKUP_HEADER_FORMAT_PREFIX} {}\nCREATE TABLE t (id INT);",
        CURRENT_BACKUP_FORMAT_VERSION + 1
    );
    let err = parse_backup_manifest(&sql).expect_err("future format should be rejected");
    assert!(
        format!("{err}").contains("unsupported backup format version"),
        "unexpected error: {err}"
    );
}

#[test]
fn validate_backup_payload_integrity_accepts_matching_checksum() {
    let payload = "CREATE TABLE t (id INT);\n";
    let manifest = BackupManifest {
        format_version: CURRENT_BACKUP_FORMAT_VERSION,
        engine_version: Some("0.1.0".to_owned()),
        payload_sha256: Some(payload_sha256(payload)),
    };
    assert!(validate_backup_payload_integrity(&manifest, payload).is_ok());
}

#[test]
fn validate_backup_payload_integrity_rejects_mismatch() {
    let payload = "CREATE TABLE t (id INT);\n";
    let manifest = BackupManifest {
        format_version: CURRENT_BACKUP_FORMAT_VERSION,
        engine_version: Some("0.1.0".to_owned()),
        payload_sha256: Some("deadbeef".to_owned()),
    };
    let err = validate_backup_payload_integrity(&manifest, payload)
        .expect_err("checksum mismatch should fail");
    assert!(
        format!("{err}").contains("checksum mismatch"),
        "unexpected error: {err}"
    );
}

#[test]
fn write_backup_header_contains_all_fields() {
    let mut output = String::new();
    write_backup_header(&mut output, "abc123");
    assert!(output.contains(BACKUP_HEADER_BANNER));
    assert!(output.contains(&format!(
        "{BACKUP_HEADER_FORMAT_PREFIX} {CURRENT_BACKUP_FORMAT_VERSION}"
    )));
    assert!(output.contains(BACKUP_HEADER_ENGINE_PREFIX));
    assert!(output.contains(&format!("{BACKUP_HEADER_PAYLOAD_SHA256_PREFIX} abc123")));
}

#[test]
fn validate_backup_payload_integrity_skips_for_v1() {
    let manifest = BackupManifest {
        format_version: 1,
        engine_version: Some("0.1.0".to_owned()),
        payload_sha256: None,
    };
    assert!(validate_backup_payload_integrity(&manifest, "anything").is_ok());
}

#[test]
fn validate_backup_payload_integrity_requires_checksum_for_v2() {
    let manifest = BackupManifest {
        format_version: CURRENT_BACKUP_FORMAT_VERSION,
        engine_version: Some("0.1.0".to_owned()),
        payload_sha256: None,
    };
    let err = validate_backup_payload_integrity(&manifest, "payload")
        .expect_err("v2 without checksum should fail");
    assert!(
        format!("{err}").contains("requires a payload checksum"),
        "unexpected error: {err}"
    );
}

#[test]
fn enforce_restore_manifest_security_rejects_legacy_by_default() {
    let manifest = BackupManifest {
        format_version: 1,
        engine_version: Some("0.1.0".to_owned()),
        payload_sha256: None,
    };
    let err = enforce_restore_manifest_security(&manifest)
        .expect_err("legacy restore should be rejected by default");
    assert!(
        format!("{err}").contains("legacy") && format!("{err}").contains("checksum"),
        "unexpected error: {err}"
    );
}

#[test]
fn read_restore_file_rejects_oversized_input() {
    let base = unique_backup_test_dir("restore-size-limit");
    std::fs::create_dir_all(&base).expect("create base dir");
    let path = base.join("oversized.sql");
    let file = std::fs::File::create(&path).expect("create restore file");
    file.set_len(MAX_BACKUP_SIZE as u64 + 1)
        .expect("set oversized length");

    let err = read_restore_file(&path).expect_err("oversized restore should fail");
    assert!(
        format!("{err}").contains("restore file exceeds maximum size"),
        "unexpected error: {err}"
    );

    let _ = std::fs::remove_dir_all(base);
}
