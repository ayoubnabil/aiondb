use super::*;

// ===================================================================
// NEW EDGE CASE TESTS
// ===================================================================

// --- parse helpers: leading plus sign rejected for all unsigned types ---

#[test]
fn parse_u64_leading_plus_errors() {
    assert!(parse_u64("key", "+42").is_err());
}

#[test]
fn parse_usize_leading_plus_errors() {
    assert!(parse_usize("key", "+100").is_err());
}

// --- parse_u64 max value ---

#[test]
fn parse_u64_max_ok() {
    assert_eq!(parse_u64("key", "18446744073709551615").unwrap(), u64::MAX);
}

// --- parse_usize max value ---

#[test]
fn parse_usize_max_ok() {
    let max_str = usize::MAX.to_string();
    assert_eq!(parse_usize("key", &max_str).unwrap(), usize::MAX);
}

// --- overflow for u64 ---

#[test]
fn parse_u64_overflow_errors() {
    // u64::MAX + 1
    assert!(parse_u64("key", "18446744073709551616").is_err());
}

// --- overflow for usize ---

#[test]
fn parse_usize_overflow_errors() {
    // Well beyond usize::MAX on any platform
    assert!(parse_usize("key", "99999999999999999999999").is_err());
}

// --- whitespace-only value for numeric field ---

#[test]
fn parse_u32_whitespace_only_errors() {
    assert!(parse_u32("key", "   ").is_err());
}

#[test]
fn parse_u64_whitespace_only_errors() {
    assert!(parse_u64("key", "  ").is_err());
}

// --- hex notation not accepted ---

#[test]
fn parse_u32_hex_notation_errors() {
    assert!(parse_u32("key", "0xFF").is_err());
}

#[test]
fn parse_u64_hex_notation_errors() {
    assert!(parse_u64("key", "0x10").is_err());
}

// --- parse_bool empty string ---

#[test]
fn parse_bool_empty_string_errors() {
    assert!(parse_bool("key", "").is_err());
}

// --- parse_bool yes/no/on/off are not valid Rust bools ---

#[test]
fn parse_bool_yes_errors() {
    assert!(parse_bool("key", "yes").is_err());
}

#[test]
fn parse_bool_no_errors() {
    assert!(parse_bool("key", "no").is_err());
}

#[test]
fn parse_bool_on_errors() {
    assert!(parse_bool("key", "on").is_err());
}

#[test]
fn parse_bool_off_errors() {
    assert!(parse_bool("key", "off").is_err());
}

#[test]
fn parse_bool_zero_errors() {
    assert!(parse_bool("key", "0").is_err());
}

// --- parse_tls_mode case insensitivity edge cases ---

#[test]
fn parse_tls_mode_mixed_case_disable() {
    assert_eq!(parse_tls_mode("dIsAbLe").unwrap(), TlsMode::Disable);
}

#[test]
fn parse_tls_mode_mixed_case_require() {
    assert_eq!(parse_tls_mode("rEqUiRe").unwrap(), TlsMode::Require);
}

// --- parse_tls_mode with extra content ---

#[test]
fn parse_tls_mode_with_trailing_space_errors() {
    // "disable " has trailing space, to_ascii_lowercase still has space
    assert!(parse_tls_mode("disable ").is_err());
}

#[test]
fn parse_tls_mode_with_leading_space_errors() {
    assert!(parse_tls_mode(" prefer").is_err());
}

// --- load_from_file: unicode in values ---

#[test]
fn load_from_file_unicode_value() {
    let content = "AIONDB_STORAGE_DATA_DIR = /données/日本語/路径\n";
    let path = temp_config_file("unicode.cfg", content);
    let cfg = load_from_file(&path).unwrap();
    assert_eq!(cfg.storage.data_dir, PathBuf::from("/données/日本語/路径"));
    let _ = std::fs::remove_file(&path);
}

// --- load_from_file: very long value for data_dir ---

#[test]
fn load_from_file_very_long_data_dir() {
    let long_path = "/".to_owned() + &"a".repeat(4096);
    let content = format!("AIONDB_STORAGE_DATA_DIR = {long_path}\n");
    let path = temp_config_file("longpath.cfg", &content);
    let cfg = load_from_file(&path).unwrap();
    assert_eq!(cfg.storage.data_dir, PathBuf::from(&long_path));
    let _ = std::fs::remove_file(&path);
}

// --- load_from_file: special characters in string value ---

#[test]
fn load_from_file_special_chars_in_listen_addr() {
    let content = "AIONDB_PGWIRE_LISTEN_ADDR = [::1]:5432\n";
    let path = temp_config_file("ipv6.cfg", content);
    let cfg = load_from_file(&path).unwrap();
    assert_eq!(cfg.pgwire.listen_addr, "[::1]:5432");
    let _ = std::fs::remove_file(&path);
}

// --- load_from_file: empty value for string field is accepted ---

#[test]
fn load_from_file_empty_string_value_accepted() {
    let content = "AIONDB_PGWIRE_LISTEN_ADDR =\n";
    let path = temp_config_file("empty_val.cfg", content);
    let cfg = load_from_file(&path).unwrap();
    assert_eq!(cfg.pgwire.listen_addr, "");
    let _ = std::fs::remove_file(&path);
}

// --- load_from_file: multiple equals signs in value ---

#[test]
fn load_from_file_multiple_equals_in_value() {
    let content = "AIONDB_STORAGE_DATA_DIR = a=b=c=d\n";
    let path = temp_config_file("multi_eq.cfg", content);
    let cfg = load_from_file(&path).unwrap();
    assert_eq!(cfg.storage.data_dir, PathBuf::from("a=b=c=d"));
    let _ = std::fs::remove_file(&path);
}

// --- load_from_file: line with only whitespace treated as blank ---

#[test]
fn load_from_file_whitespace_only_line_treated_as_blank() {
    let content = "   \t  \n";
    let path = temp_config_file("wsonly.cfg", content);
    let cfg = load_from_file(&path).unwrap();
    assert_eq!(cfg, RuntimeConfig::default());
    let _ = std::fs::remove_file(&path);
}

// --- load_from_file: comment with no space after # ---

#[test]
fn load_from_file_comment_no_space_after_hash() {
    let content = "#comment with no space\n";
    let path = temp_config_file("nospace_comment.cfg", content);
    let cfg = load_from_file(&path).unwrap();
    assert_eq!(cfg, RuntimeConfig::default());
    let _ = std::fs::remove_file(&path);
}

// --- load_from_map: zero timeout is allowed ---

#[test]
fn map_zero_statement_timeout() {
    let mut entries = HashMap::new();
    entries.insert(
        "AIONDB_LIMITS_STATEMENT_TIMEOUT_MS".to_owned(),
        "0".to_owned(),
    );
    let cfg = load_from_map(entries).unwrap();
    assert_eq!(cfg.limits.statement_timeout, Duration::from_millis(0));
}

// --- load_from_map: max u64 for limits ---

#[test]
fn map_max_u64_for_max_result_rows() {
    let mut entries = HashMap::new();
    entries.insert(
        "AIONDB_LIMITS_MAX_RESULT_ROWS".to_owned(),
        u64::MAX.to_string(),
    );
    let cfg = load_from_map(entries).unwrap();
    assert_eq!(cfg.limits.max_result_rows, u64::MAX);
}

// --- load_from_map: zero for all limit fields ---

#[test]
fn map_zero_max_memory_bytes() {
    let mut entries = HashMap::new();
    entries.insert("AIONDB_LIMITS_MAX_MEMORY_BYTES".to_owned(), "0".to_owned());
    let cfg = load_from_map(entries).unwrap();
    assert_eq!(cfg.limits.max_memory_bytes, 0);
}

#[test]
fn map_zero_max_temp_bytes() {
    let mut entries = HashMap::new();
    entries.insert("AIONDB_LIMITS_MAX_TEMP_BYTES".to_owned(), "0".to_owned());
    let cfg = load_from_map(entries).unwrap();
    assert_eq!(cfg.limits.max_temp_bytes, 0);
}

#[test]
fn map_zero_max_portals() {
    let mut entries = HashMap::new();
    entries.insert("AIONDB_LIMITS_MAX_PORTALS".to_owned(), "0".to_owned());
    assert!(load_from_map(entries).is_err());
}

#[test]
fn map_zero_max_prepared_statements() {
    let mut entries = HashMap::new();
    entries.insert(
        "AIONDB_LIMITS_MAX_PREPARED_STATEMENTS".to_owned(),
        "0".to_owned(),
    );
    assert!(load_from_map(entries).is_err());
}

// --- load_from_map: zero connections ---

#[test]
fn map_zero_max_connections() {
    let mut entries = HashMap::new();
    entries.insert("AIONDB_PGWIRE_MAX_CONNECTIONS".to_owned(), "0".to_owned());
    assert!(load_from_map(entries).is_err());
}

#[test]
fn map_zero_max_connections_per_ip() {
    let mut entries = HashMap::new();
    entries.insert(
        "AIONDB_PGWIRE_MAX_CONNECTIONS_PER_IP".to_owned(),
        "0".to_owned(),
    );
    assert!(load_from_map(entries).is_err());
}

// --- load_from_map: u32::MAX for connections is now rejected (audit config M-01) ---

#[test]
fn map_u32_max_connections() {
    let mut entries = HashMap::new();
    entries.insert(
        "AIONDB_PGWIRE_MAX_CONNECTIONS".to_owned(),
        u32::MAX.to_string(),
    );
    assert!(load_from_map(entries).is_err());
}

// --- load_from_map: zero security fields ---

#[test]
fn map_zero_max_auth_failures() {
    let mut entries = HashMap::new();
    entries.insert(
        "AIONDB_SECURITY_MAX_AUTH_FAILURES".to_owned(),
        "0".to_owned(),
    );
    assert!(load_from_map(entries).is_err());
}

#[test]
fn map_zero_auth_lockout_window() {
    let mut entries = HashMap::new();
    entries.insert(
        "AIONDB_SECURITY_AUTH_LOCKOUT_WINDOW_MS".to_owned(),
        "0".to_owned(),
    );
    let err = load_from_map(entries).unwrap_err();
    assert!(
        err.to_string().contains("lockout window must be > 0"),
        "unexpected error: {err}"
    );
}

// --- load_from_map: engine pool zero values ---

#[test]
fn map_zero_engine_pool_worker_threads() {
    let mut entries = HashMap::new();
    entries.insert(
        "AIONDB_ENGINE_POOL_WORKER_THREADS".to_owned(),
        "0".to_owned(),
    );
    assert!(load_from_map(entries).is_err());
}

#[test]
fn map_zero_engine_pool_queue_depth() {
    let mut entries = HashMap::new();
    entries.insert("AIONDB_ENGINE_POOL_QUEUE_DEPTH".to_owned(), "0".to_owned());
    assert!(load_from_map(entries).is_err());
}

// --- load_from_map: empty map gives defaults ---

#[test]
fn map_empty_gives_defaults() {
    let entries = HashMap::new();
    let cfg = load_from_map(entries).unwrap();
    assert_eq!(cfg, RuntimeConfig::default());
}

// --- load_from_file: file with only comments and whitespace ---

#[test]
fn load_from_file_only_comments_many_lines() {
    let content = "# line 1\n# line 2\n# line 3\n# line 4\n# line 5\n";
    let path = temp_config_file("many_comments.cfg", content);
    let cfg = load_from_file(&path).unwrap();
    assert_eq!(cfg, RuntimeConfig::default());
    let _ = std::fs::remove_file(&path);
}

// --- load_from_file: duplicate keys (last write wins via HashMap) ---

#[test]
fn load_from_file_duplicate_keys_last_wins() {
    let content = "AIONDB_STORAGE_PAGE_SIZE = 1024\nAIONDB_STORAGE_PAGE_SIZE = 8192\n";
    let path = temp_config_file("dup_keys.cfg", content);
    let cfg = load_from_file(&path).unwrap();
    // HashMap means last insert wins
    assert_eq!(
        cfg.storage.page_size,
        crate::storage::DEFAULT_STORAGE_PAGE_SIZE
    );
    let _ = std::fs::remove_file(&path);
}

// --- parse_u32 with leading zeros ---

#[test]
fn parse_u32_leading_zeros_ok() {
    assert_eq!(parse_u32("key", "007").unwrap(), 7);
}

// --- parse_u64 with leading zeros ---

#[test]
fn parse_u64_leading_zeros_ok() {
    assert_eq!(parse_u64("key", "0042").unwrap(), 42);
}

// --- float value for u32 -> error ---

#[test]
fn parse_u32_float_errors() {
    assert!(parse_u32("key", "1.5").is_err());
}

// --- float value for usize -> error ---

#[test]
fn parse_usize_float_errors() {
    assert!(parse_usize("key", "2.7").is_err());
}

// --- parse_u32 with scientific notation -> error ---

#[test]
fn parse_u32_scientific_notation_errors() {
    assert!(parse_u32("key", "1e5").is_err());
}

// --- parse_u64 negative overflow ---

#[test]
fn parse_u64_negative_errors() {
    assert!(parse_u64("key", "-1").is_err());
}

// --- parse_usize negative ---

#[test]
fn parse_usize_negative_errors() {
    assert!(parse_usize("key", "-1").is_err());
}

// --- load_from_file with tab-separated key=value ---

#[test]
fn load_from_file_tabs_around_equals() {
    let content = "\tAIONDB_STORAGE_PAGE_SIZE\t=\t8192\t\n";
    let path = temp_config_file("tabs.cfg", content);
    let cfg = load_from_file(&path).unwrap();
    assert_eq!(
        cfg.storage.page_size,
        crate::storage::DEFAULT_STORAGE_PAGE_SIZE
    );
    let _ = std::fs::remove_file(&path);
}

// --- load_from_file: value with hash char (not a comment because mid-line) ---

#[test]
fn load_from_file_value_containing_hash() {
    let content = "AIONDB_STORAGE_DATA_DIR = /path#with#hashes\n";
    let path = temp_config_file("hash_val.cfg", content);
    let cfg = load_from_file(&path).unwrap();
    // The # in the value is not stripped; entire "right side" is the value
    assert_eq!(cfg.storage.data_dir, PathBuf::from("/path#with#hashes"));
    let _ = std::fs::remove_file(&path);
}

// --- load_from_map: zero storage page_size ---

#[test]
fn map_zero_page_size() {
    let mut entries = HashMap::new();
    entries.insert("AIONDB_STORAGE_PAGE_SIZE".to_owned(), "0".to_owned());
    assert!(load_from_map(entries).is_err());
}

// --- load_from_map: zero max_open_files ---

#[test]
fn map_zero_max_open_files() {
    let mut entries = HashMap::new();
    entries.insert("AIONDB_STORAGE_MAX_OPEN_FILES".to_owned(), "0".to_owned());
    assert!(load_from_map(entries).is_err());
}

// --- load_from_map: require_tls_for_password false ---

#[test]
fn map_security_require_tls_for_password_false() {
    let mut entries = HashMap::new();
    entries.insert(
        "AIONDB_SECURITY_REQUIRE_TLS_FOR_PASSWORD".to_owned(),
        "false".to_owned(),
    );
    let cfg = load_from_map(entries).unwrap();
    assert!(!cfg.security.require_tls_for_password);
}
