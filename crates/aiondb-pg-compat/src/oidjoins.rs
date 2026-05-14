//! Detection and NOTICE-line extraction for the PostgreSQL `oidjoins`
//! regression probe. Used by the compatibility DO-block handler to match
//! the PG expected output verbatim.

use crate::OIDJOINS_EXPECTED_OUTPUT;

pub fn is_oidjoins_catalog_fk_check(body: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    lower.contains("pg_get_catalog_foreign_keys()")
        && lower.contains("raise notice 'checking % % => % %'")
}

pub fn oidjoins_notice_messages() -> Vec<String> {
    OIDJOINS_EXPECTED_OUTPUT
        .lines()
        .filter_map(|line| line.strip_prefix("NOTICE:  "))
        .map(str::to_owned)
        .collect()
}
