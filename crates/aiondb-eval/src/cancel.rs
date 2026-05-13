//! Thread-local cancellation hook for scalar evaluation.
//!
//! Long-running scalar functions (e.g. `pg_sleep`) need a cooperative
//! interruption point so that pgwire `CancelRequest` does not stall behind
//! a synchronous sleep. The engine registers a checker before invoking the
//! evaluator on a session-bound thread; scalar functions call
//! [`check_cancellation`] periodically. When the checker reports cancellation
//! the call returns the matching `DbError`, propagating SQLSTATE 57014.
//!
//! Production note: keep callers cooperative. The checker is invoked from the
//! same thread as evaluation; it must be cheap and lock-free in the hot path.

use std::cell::RefCell;
use std::sync::Arc;

use aiondb_core::DbResult;

type CancelChecker = Arc<dyn Fn() -> DbResult<()> + Send + Sync>;

thread_local! {
    static CANCEL_CHECKER: RefCell<Option<CancelChecker>> = const { RefCell::new(None) };
}

struct CancelGuard(Option<CancelChecker>);

impl Drop for CancelGuard {
    fn drop(&mut self) {
        CANCEL_CHECKER.with(|cell| *cell.borrow_mut() = self.0.take());
    }
}

/// Install `checker` for the duration of `f`. Restores the previous checker
/// (if any) when `f` returns or unwinds.
pub fn with_cancellation_checker<T>(checker: CancelChecker, f: impl FnOnce() -> T) -> T {
    let previous = CANCEL_CHECKER.with(|cell| cell.borrow_mut().replace(checker));
    let _guard = CancelGuard(previous);
    f()
}

/// Run `f` to completion, propagating cancellation errors observed via the
/// thread-local checker. Returns `Ok(())` when no checker is installed - the
/// caller assumes responsibility for any deadline/cancel semantics in that
/// case.
pub fn check_cancellation() -> DbResult<()> {
    CANCEL_CHECKER.with(|cell| match cell.borrow().as_ref() {
        Some(checker) => checker(),
        None => Ok(()),
    })
}

/// Returns whether a cancellation checker is installed on the current thread.
pub fn has_cancellation_checker() -> bool {
    CANCEL_CHECKER.with(|cell| cell.borrow().is_some())
}

#[cfg(test)]
mod tests {
    use super::*;
    use aiondb_core::{DbError, SqlState};

    fn cancel_error(message: &str) -> DbError {
        DbError::query_canceled(message.to_owned())
    }

    #[test]
    fn check_cancellation_is_ok_without_checker() {
        assert!(check_cancellation().is_ok());
    }

    #[test]
    fn check_cancellation_propagates_error() {
        let checker: CancelChecker =
            Arc::new(|| Err(cancel_error("canceling statement due to user request")));
        let result = with_cancellation_checker(checker, check_cancellation);
        let err = result.unwrap_err();
        assert_eq!(err.sqlstate(), SqlState::QueryCanceled);
    }

    #[test]
    fn checker_is_restored_after_scope() {
        let outer: CancelChecker = Arc::new(|| Ok(()));
        with_cancellation_checker(Arc::clone(&outer), || {
            assert!(has_cancellation_checker());
            let inner: CancelChecker = Arc::new(|| Err(cancel_error("inner cancel")));
            with_cancellation_checker(inner, || {
                assert!(check_cancellation().is_err());
            });
            assert!(check_cancellation().is_ok());
        });
        assert!(!has_cancellation_checker());
    }
}
