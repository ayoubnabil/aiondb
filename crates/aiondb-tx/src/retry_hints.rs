//! Statement-level retry hints.
//!
//! Maps `(error_code, statement_kind)` pairs to a retry hint :
//! whether the statement is safe to retry, and if so what action.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StatementKind {
    Read,
    Write,
    Schema,
    Mixed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RetryHint {
    SafeToRetry,
    RetryAfterRefresh,
    RouteToLeaseholder,
    NotRetryable,
}

pub fn classify(error_code: u32, kind: StatementKind) -> RetryHint {
    // Schema changes are never silently retried regardless of the
    // underlying error.
    if matches!(kind, StatementKind::Schema) {
        return RetryHint::NotRetryable;
    }
    match (error_code, kind) {
        // 40001 : serialization failure.
        (40001, _) => RetryHint::SafeToRetry,
        // 40P01 : deadlock.
        (40_5001, _) => RetryHint::SafeToRetry,
        // 4001 : geo-pinning rejection.
        (4001, _) => RetryHint::NotRetryable,
        // 55P03 : lock timeout.
        (55_5003, StatementKind::Read | StatementKind::Write) => RetryHint::SafeToRetry,
        // 53300 : too many connections.
        (53300, _) => RetryHint::SafeToRetry,
        // 4501 : follower read stale.
        (4501, StatementKind::Read) => RetryHint::RouteToLeaseholder,
        // 4502 : refresh required.
        (4502, _) => RetryHint::RetryAfterRefresh,
        // Everything else : not retryable by default.
        _ => RetryHint::NotRetryable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialization_failure_is_safe() {
        assert_eq!(
            classify(40001, StatementKind::Write),
            RetryHint::SafeToRetry
        );
    }

    #[test]
    fn deadlock_is_safe() {
        assert_eq!(
            classify(40_5001, StatementKind::Write),
            RetryHint::SafeToRetry
        );
    }

    #[test]
    fn follower_read_stale_routes_to_leader() {
        assert_eq!(
            classify(4501, StatementKind::Read),
            RetryHint::RouteToLeaseholder
        );
    }

    #[test]
    fn refresh_required_returns_refresh_hint() {
        assert_eq!(
            classify(4502, StatementKind::Write),
            RetryHint::RetryAfterRefresh
        );
    }

    #[test]
    fn schema_change_never_silently_retried() {
        assert_eq!(
            classify(53300, StatementKind::Schema),
            RetryHint::NotRetryable
        );
    }

    #[test]
    fn unknown_error_is_not_retryable() {
        assert_eq!(
            classify(99999, StatementKind::Read),
            RetryHint::NotRetryable
        );
    }
}
