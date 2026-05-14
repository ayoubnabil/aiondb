use aiondb_core::{DbError, DbResult};

pub(in super::super) fn merge_with_lock_release_error<T>(
    result: DbResult<T>,
    release_result: DbResult<()>,
    context: &'static str,
) -> DbResult<T> {
    match (result, release_result) {
        (Ok(value), Ok(())) => Ok(value),
        (Ok(_), Err(release_error)) => Err(release_error),
        (Err(error), Ok(())) => Err(error),
        (Err(error), Err(release_error)) => Err(DbError::internal(format!(
            "{context} failed: {error}; lock release also failed: {release_error}"
        ))),
    }
}

pub(in super::super) fn mark_commit_outcome_ambiguous(
    error: DbError,
    stage: &'static str,
) -> DbError {
    let detail = format!(
        "transaction commit may already have succeeded: {stage} failed after the commit timestamp was published"
    );
    with_appended_internal_detail(
        error.with_client_detail(detail.clone()).with_client_hint(
            "verify whether the transaction effects are visible before retrying COMMIT",
        ),
        detail,
    )
}

pub(in super::super) fn with_appended_internal_detail(
    error: DbError,
    detail: impl Into<String>,
) -> DbError {
    let detail = detail.into();
    let merged = match error.report().internal_detail.as_deref() {
        Some(existing) => format!("{existing}; {detail}"),
        None => detail,
    };
    error.with_internal_detail(merged)
}

pub(in crate::engine) fn preserves_outer_transaction(error: &DbError) -> bool {
    error
        .report()
        .internal_detail
        .as_deref()
        .is_some_and(|detail| {
            detail.contains(
                "statement was rolled back to an internal savepoint; outer transaction remains usable",
            )
        })
}
