use super::*;
use aiondb_core::SqlState;

/// Assert that a `DbError` coming from the parser carries `SyntaxError`,
/// `ProgramLimitExceeded`, or `FeatureNotSupported` sqlstate (the three
/// the parser is expected to emit).
fn assert_parse_error(err: &aiondb_core::DbError) {
    let state = err.sqlstate();
    assert!(
        state == SqlState::SyntaxError
            || state == SqlState::ProgramLimitExceeded
            || state == SqlState::FeatureNotSupported,
        "unexpected sqlstate {state:?} for parser error: {err}"
    );
}

/// Convenience: call `parse_sql` and, if it fails, verify error structure.
fn parse_sql_no_panic(sql: &str) {
    match parse_sql(sql) {
        Ok(_) => {}
        Err(e) => assert_parse_error(&e),
    }
}

mod properties;
mod robustness;
mod trivial_and_malformed;
