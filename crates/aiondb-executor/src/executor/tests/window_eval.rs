use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use aiondb_plan::{SortExpr, TypedExprKind, WindowFunctionKind};

#[test]
fn evaluate_windows_honors_cancellation_checker() {
    let (executor, _, _) = make_executor();

    let source_rows: Vec<Row> = (0..128).map(|i| Row::new(vec![Value::Int(i)])).collect();
    let outputs = vec![ProjectionExpr {
        field: ResultField {
            name: "rn".to_string(),
            data_type: DataType::BigInt,
            text_type_modifier: None,
            nullable: false,
        },
        expr: TypedExpr {
            kind: TypedExprKind::WindowFunction {
                func: WindowFunctionKind::RowNumber,
                args: vec![],
                partition_by: vec![],
                order_by: vec![SortExpr {
                    expr: TypedExpr::column_ref("val", 0, DataType::Int, false),
                    descending: false,
                    nulls_first: Some(false),
                }],
            },
            data_type: DataType::BigInt,
            nullable: false,
        },
    }];

    let checks = Arc::new(AtomicUsize::new(0));
    let cancellation_checker = {
        let checks = checks.clone();
        Arc::new(move || {
            let seen = checks.fetch_add(1, Ordering::Relaxed);
            if seen >= 300 {
                Err(DbError::query_canceled("session canceled"))
            } else {
                Ok(())
            }
        })
    };
    let ctx = ExecutionContext::default().with_cancellation_checker(cancellation_checker);

    let err =
        crate::executor::window_eval::evaluate_windows(&executor, &outputs, &source_rows, &ctx)
            .expect_err("window evaluation should stop when cancellation fires");
    assert_eq!(err.sqlstate(), aiondb_core::SqlState::QueryCanceled);
}
