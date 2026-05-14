use super::*;

#[test]
fn check_deadline_with_past_deadline_returns_error() {
    let past = Instant::now()
        .checked_sub(Duration::from_secs(1))
        .expect("subtract 1s");
    let ctx = ExecutionContext {
        statement_deadline: Some(past),
        ..ExecutionContext::default()
    };
    let error = ctx.check_deadline().expect_err("deadline already passed");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::QueryCanceled);
}

#[test]
fn check_deadline_with_future_deadline_returns_ok() {
    let future = Instant::now() + Duration::from_secs(60);
    let ctx = ExecutionContext {
        statement_deadline: Some(future),
        ..ExecutionContext::default()
    };
    ctx.check_deadline().expect("deadline is in the future");
}

#[test]
fn check_deadline_with_no_deadline_returns_ok() {
    let ctx = ExecutionContext {
        statement_deadline: None,
        ..ExecutionContext::default()
    };
    ctx.check_deadline().expect("no deadline set");
}

#[test]
fn ensure_result_bytes_fit_rejects_oversized_row() {
    let ctx = ExecutionContext {
        max_result_bytes: 5,
        ..ExecutionContext::default()
    };
    let row = Row::new(vec![Value::Text(
        "this string is longer than five bytes".to_owned(),
    )]);
    let error = ensure_result_bytes_fit(&ctx, &row, 0).expect_err("row too large");
    assert_eq!(
        error.sqlstate(),
        aiondb_core::SqlState::ProgramLimitExceeded
    );
}
