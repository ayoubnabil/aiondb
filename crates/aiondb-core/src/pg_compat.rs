use std::borrow::Cow;

pub const COMPAT_SERVER_VERSION: &str = "16.0";
pub const COMPAT_SERVER_VERSION_NUM: i32 = 160_000;
pub const COMPAT_SERVER_ENCODING: &str = "UTF8";
pub const COMPAT_CLIENT_ENCODING: &str = "UTF8";
pub const COMPAT_CLIENT_MIN_MESSAGES: &str = "notice";
pub const COMPAT_STANDARD_CONFORMING_STRINGS: &str = "on";
pub const COMPAT_INTEGER_DATETIMES: &str = "on";
pub const COMPAT_DATE_STYLE: &str = "ISO, MDY";
pub const COMPAT_INTERVAL_STYLE: &str = "postgres";
pub const COMPAT_DEFAULT_TIMEZONE: &str = "UTC";
pub const COMPAT_DEFAULT_LOCALE: &str = "en_US.UTF-8";
pub const COMPAT_DEFAULT_SEARCH_PATH: &str = "\"$user\", public";
pub const COMPAT_DEFAULT_TRANSACTION_ISOLATION: &str = "read committed";
pub const COMPAT_DEFAULT_TRANSACTION_READ_ONLY: &str = "off";
pub const COMPAT_DEFAULT_TRANSACTION_DEFERRABLE: &str = "off";
pub const COMPAT_PGVECTOR_HNSW_EF_SEARCH: &str = "40";
pub const COMPAT_PGVECTOR_HNSW_ITERATIVE_SCAN: &str = "off";
pub const COMPAT_PGVECTOR_HNSW_MAX_SCAN_TUPLES: &str = "20000";
pub const COMPAT_PGVECTOR_HNSW_SCAN_MEM_MULTIPLIER: &str = "1";
pub const COMPAT_PGVECTOR_IVFFLAT_PROBES: &str = "1";
pub const COMPAT_PGVECTOR_IVFFLAT_ITERATIVE_SCAN: &str = "off";
pub const COMPAT_PGVECTOR_IVFFLAT_MAX_PROBES: &str = "32768";
pub const COMPAT_BYTEA_OUTPUT: &str = "hex";
pub const COMPAT_MAX_IDENTIFIER_LENGTH: &str = "63";
pub const COMPAT_DEFAULT_DATABASE_NAME: &str = "default";
pub const COMPAT_DEFAULT_DATABASE_OID: i32 = 1;
pub const COMPAT_PG_DEFAULT_TABLESPACE_OID: i32 = 1663;
pub const COMPAT_PG_GLOBAL_TABLESPACE_OID: i32 = 1664;
pub const COMPAT_PUBLIC_NAMESPACE_OID: i32 = 2200;
pub const COMPAT_PG_CATALOG_NAMESPACE_OID: i32 = 11;
pub const COMPAT_INFORMATION_SCHEMA_NAMESPACE_OID: i32 = 13394;
pub const COMPAT_BOOTSTRAP_ROLE_OID: i32 = 10;
pub const COMPAT_BOOTSTRAP_ROLE_NAME: &str = "aiondb";
pub const PG_TEMP_SCHEMA_NAME: &str = "pg_temp";

// pgvector / bit-string type OIDs. Single source of truth so that the
// planner (catalog rows), the evaluator (regtype / format_type lookup) and
// the wire layer can never disagree on which OID belongs to which type.
pub const COMPAT_PGVECTOR_VECTOR_OID: i32 = 80_001;
pub const COMPAT_PGVECTOR_VECTOR_ARRAY_OID: i32 = 80_002;
pub const COMPAT_PGVECTOR_HALFVEC_OID: i32 = 80_003;
pub const COMPAT_PGVECTOR_HALFVEC_ARRAY_OID: i32 = 80_004;
pub const COMPAT_PGVECTOR_SPARSEVEC_OID: i32 = 80_005;
pub const COMPAT_PGVECTOR_SPARSEVEC_ARRAY_OID: i32 = 80_006;
pub const COMPAT_PGVECTOR_HNSW_AM_OID: i32 = 80_020;
pub const COMPAT_PGVECTOR_IVFFLAT_AM_OID: i32 = 80_021;
pub const COMPAT_PG_BIT_OID: i32 = 1_560;
pub const COMPAT_PG_BIT_ARRAY_OID: i32 = 1_561;
pub const COMPAT_PG_VARBIT_OID: i32 = 1_562;
pub const COMPAT_PG_VARBIT_ARRAY_OID: i32 = 1_563;

/// Custom OID emitted on the wire for AionDB's native `VECTOR` type.
/// Lives in the user-OID range so it doesn't collide with PostgreSQL or
/// pgvector identifiers.
pub const AIONDB_VECTOR_TYPE_OID: u32 = 62_000;

/// The bucket must stay empty.
pub const COMPAT_EXECUTOR_INTENTIONAL_NOOP_TAGS: &[&str] = &[];

#[must_use]
pub fn is_compat_executor_intentional_noop_tag(tag: &str) -> bool {
    COMPAT_EXECUTOR_INTENTIONAL_NOOP_TAGS.contains(&tag)
}

#[must_use]
pub fn compat_version_banner() -> String {
    format!("PostgreSQL {COMPAT_SERVER_VERSION} (AionDB)")
}

#[must_use]
pub fn compat_server_version_num_string() -> String {
    COMPAT_SERVER_VERSION_NUM.to_string()
}

#[must_use]
pub fn compat_timezone() -> String {
    std::env::var("TZ")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| COMPAT_DEFAULT_TIMEZONE.to_owned())
}

#[must_use]
pub fn compat_locale() -> String {
    for key in ["LC_ALL", "LC_CTYPE", "LANG"] {
        if let Ok(value) = std::env::var(key) {
            let value = value.trim();
            if !value.is_empty() {
                return value.to_owned();
            }
        }
    }
    COMPAT_DEFAULT_LOCALE.to_owned()
}

#[must_use]
pub fn compat_setting_value(name: &str) -> Option<Cow<'static, str>> {
    // PG client drivers (libpq, psycopg, asyncpg, JDBC) emit setting
    // names in canonical lowercase form, so skip the
    // `to_ascii_lowercase()` allocation on the dominant path. Only fall
    // back to allocating when the caller passes mixed case.
    let normalized: Cow<'_, str> = if name.bytes().any(|b| b.is_ascii_uppercase()) {
        Cow::Owned(name.to_ascii_lowercase())
    } else {
        Cow::Borrowed(name)
    };
    match normalized.as_ref() {
        "server_version" => Some(Cow::Borrowed(COMPAT_SERVER_VERSION)),
        "server_version_num" => Some(Cow::Owned(compat_server_version_num_string())),
        "server_encoding" => Some(Cow::Borrowed(COMPAT_SERVER_ENCODING)),
        "client_encoding" => Some(Cow::Borrowed(COMPAT_CLIENT_ENCODING)),
        "client_min_messages" => Some(Cow::Borrowed(COMPAT_CLIENT_MIN_MESSAGES)),
        "standard_conforming_strings" => Some(Cow::Borrowed(COMPAT_STANDARD_CONFORMING_STRINGS)),
        "integer_datetimes" => Some(Cow::Borrowed(COMPAT_INTEGER_DATETIMES)),
        "timezone" => Some(Cow::Owned(compat_timezone())),
        "datestyle" => Some(Cow::Borrowed(COMPAT_DATE_STYLE)),
        "intervalstyle" => Some(Cow::Borrowed(COMPAT_INTERVAL_STYLE)),
        "lc_collate" | "lc_ctype" => Some(Cow::Owned(compat_locale())),
        "search_path" => Some(Cow::Borrowed(COMPAT_DEFAULT_SEARCH_PATH)),
        "default_transaction_isolation" | "transaction_isolation" => {
            Some(Cow::Borrowed(COMPAT_DEFAULT_TRANSACTION_ISOLATION))
        }
        "default_transaction_read_only" | "transaction_read_only" | "in_hot_standby" => {
            Some(Cow::Borrowed(COMPAT_DEFAULT_TRANSACTION_READ_ONLY))
        }
        "default_transaction_deferrable" | "transaction_deferrable" => {
            Some(Cow::Borrowed(COMPAT_DEFAULT_TRANSACTION_DEFERRABLE))
        }
        "hnsw.ef_search" => Some(Cow::Borrowed(COMPAT_PGVECTOR_HNSW_EF_SEARCH)),
        "hnsw.iterative_scan" => Some(Cow::Borrowed(COMPAT_PGVECTOR_HNSW_ITERATIVE_SCAN)),
        "hnsw.max_scan_tuples" => Some(Cow::Borrowed(COMPAT_PGVECTOR_HNSW_MAX_SCAN_TUPLES)),
        "hnsw.scan_mem_multiplier" => Some(Cow::Borrowed(COMPAT_PGVECTOR_HNSW_SCAN_MEM_MULTIPLIER)),
        "ivfflat.probes" => Some(Cow::Borrowed(COMPAT_PGVECTOR_IVFFLAT_PROBES)),
        "ivfflat.iterative_scan" => Some(Cow::Borrowed(COMPAT_PGVECTOR_IVFFLAT_ITERATIVE_SCAN)),
        "ivfflat.max_probes" => Some(Cow::Borrowed(COMPAT_PGVECTOR_IVFFLAT_MAX_PROBES)),
        "bytea_output" => Some(Cow::Borrowed(COMPAT_BYTEA_OUTPUT)),
        "max_identifier_length" => Some(Cow::Borrowed(COMPAT_MAX_IDENTIFIER_LENGTH)),
        "track_counts" | "fsync" => Some(Cow::Borrowed("on")),
        "block_size" => Some(Cow::Borrowed("8192")),
        // PG defaults that pg_settings advertises and psql `\dconfig`
        // walks one-by-one through current_setting(); each name listed in
        // pg_settings must resolve here or the meta command errors out.
        "max_connections" => Some(Cow::Borrowed("128")),
        "wal_segment_size" => Some(Cow::Borrowed("16777216")),
        "is_superuser" => Some(Cow::Borrowed("on")),
        "allow_system_table_mods" => Some(Cow::Borrowed("off")),
        "enable_partitionwise_aggregate" | "enable_partitionwise_join" => {
            Some(Cow::Borrowed("off"))
        }
        _ if normalized.starts_with("enable_") => Some(Cow::Borrowed("on")),
        _ => None,
    }
}

#[must_use]
pub fn compat_role_oid(name: &str) -> i32 {
    if name.eq_ignore_ascii_case("postgres")
        || name.eq_ignore_ascii_case(COMPAT_BOOTSTRAP_ROLE_NAME)
    {
        return COMPAT_BOOTSTRAP_ROLE_OID;
    }
    compat_hash_oid(name, 200_000)
}

#[must_use]
pub fn compat_function_oid(signature: &str) -> i32 {
    compat_hash_oid_with_base(signature, 350_000, 1_000_000)
}

#[must_use]
pub fn compat_database_oid(_name: &str) -> i32 {
    COMPAT_DEFAULT_DATABASE_OID
}

fn compat_hash_oid(name: &str, seed: u32) -> i32 {
    compat_hash_oid_with_base(name, seed, 20_000)
}

fn compat_hash_oid_with_base(name: &str, seed: u32, base: i32) -> i32 {
    let mut hash = 0x811c_9dc5_u32 ^ seed;
    for byte in name.as_bytes() {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(16_777_619);
    }
    let bucket = i32::try_from(hash % 500_000).unwrap_or(i32::MAX);
    base.saturating_add(bucket)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_oid_is_stable_for_same_name() {
        assert_eq!(compat_role_oid("alice"), compat_role_oid("alice"));
    }

    #[test]
    fn bootstrap_role_oid_matches_postgres_convention() {
        assert_eq!(compat_role_oid("postgres"), COMPAT_BOOTSTRAP_ROLE_OID);
        assert_eq!(
            compat_role_oid(COMPAT_BOOTSTRAP_ROLE_NAME),
            COMPAT_BOOTSTRAP_ROLE_OID
        );
    }

    #[test]
    fn function_oid_is_stable_for_same_signature() {
        assert_eq!(
            compat_function_oid("stats_test_func1()"),
            compat_function_oid("stats_test_func1()")
        );
    }

    #[test]
    fn compat_setting_value_returns_runtime_locale_and_timezone() {
        assert!(compat_setting_value("lc_collate").is_some());
        assert!(compat_setting_value("timezone").is_some());
    }
}
