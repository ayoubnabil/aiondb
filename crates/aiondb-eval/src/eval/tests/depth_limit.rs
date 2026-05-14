use super::*;

/// Build a chain of `depth` nested `IsNull` expressions around a literal.
/// `IsNull` is a good choice because it is a unary node that always produces
/// `Boolean`, so the nesting is pure recursion with no type mismatches.
fn build_nested_is_null(depth: usize) -> TypedExpr {
    let mut expr = lit_int(1);
    for _ in 0..depth {
        expr = TypedExpr::is_null(expr, false);
    }
    expr
}

#[test]
fn deeply_nested_expression_returns_error() {
    // Use a dedicated thread with a large stack to avoid stack overflow
    // during construction and drop of the deeply nested TypedExpr tree.
    let result = std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024) // 64 MiB
        .spawn(|| {
            let expr = build_nested_is_null(1_100);
            eval(&expr)
        })
        .expect("failed to spawn thread")
        .join()
        .expect("thread panicked");

    assert!(
        result.is_err(),
        "expected depth-limit error, got {result:?}"
    );
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains("depth"),
        "error should mention 'depth', got: {msg}"
    );
}

#[test]
fn normal_depth_expression_succeeds() {
    let expr = build_nested_is_null(50);
    let result = eval(&expr);
    assert!(result.is_ok(), "expected success, got {result:?}");
}

#[test]
fn depth_resets_after_evaluation() {
    // First evaluation should leave the counter at 0.
    let expr = build_nested_is_null(50);
    let _ = eval(&expr);

    // Read the thread-local depth; it must be back to 0.
    let depth = super::super::EVAL_DEPTH.with(|d| d.get());
    assert_eq!(depth, 0, "depth counter should reset to 0 after evaluation");

    // A second evaluation should succeed identically.
    let result = eval(&expr);
    assert!(
        result.is_ok(),
        "second evaluation should succeed, got {result:?}"
    );
}
