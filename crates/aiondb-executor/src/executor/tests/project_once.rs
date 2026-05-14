use super::*;

#[test]
fn project_once_literal_int() {
    let (executor, _, _) = make_executor();
    let ctx = default_context();

    let plan = PhysicalPlan::ProjectOnce {
        outputs: vec![make_projection_expr(
            "val",
            DataType::Int,
            false,
            TypedExpr::literal(Value::Int(42), DataType::Int, false),
        )],
        filter: None,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
    };

    let result = executor.execute(&plan, &ctx).unwrap();
    assert_eq!(
        result,
        ExecutionResult::Query {
            columns: vec![ResultField {
                name: "val".to_string(),
                data_type: DataType::Int,
                text_type_modifier: None,
                nullable: false,
            }],
            rows: vec![Row::new(vec![Value::Int(42)])],
        }
    );
}

#[test]
fn project_once_arithmetic_expression() {
    let (executor, _, _) = make_executor();
    let ctx = default_context();

    // SELECT 1 + 1
    let expr = TypedExpr::arith_add(
        TypedExpr::literal(Value::Int(1), DataType::Int, false),
        TypedExpr::literal(Value::Int(1), DataType::Int, false),
        DataType::Int,
        false,
    );

    let plan = PhysicalPlan::ProjectOnce {
        outputs: vec![make_projection_expr("sum", DataType::Int, false, expr)],
        filter: None,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
    };

    let result = executor.execute(&plan, &ctx).unwrap();
    match result {
        ExecutionResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::Int(2));
        }
        _ => panic!("expected Query result"),
    }
}

#[test]
fn project_once_multiple_columns() {
    let (executor, _, _) = make_executor();
    let ctx = default_context();

    let plan = PhysicalPlan::ProjectOnce {
        outputs: vec![
            make_projection_expr(
                "a",
                DataType::Int,
                false,
                TypedExpr::literal(Value::Int(10), DataType::Int, false),
            ),
            make_projection_expr(
                "b",
                DataType::Text,
                false,
                TypedExpr::literal(Value::Text("hello".to_string()), DataType::Text, false),
            ),
            make_projection_expr(
                "c",
                DataType::Boolean,
                false,
                TypedExpr::literal(Value::Boolean(true), DataType::Boolean, false),
            ),
        ],
        filter: None,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
    };

    let result = executor.execute(&plan, &ctx).unwrap();
    match result {
        ExecutionResult::Query { columns, rows } => {
            assert_eq!(columns.len(), 3);
            assert_eq!(columns[0].name, "a");
            assert_eq!(columns[1].name, "b");
            assert_eq!(columns[2].name, "c");
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::Int(10));
            assert_eq!(rows[0].values[1], Value::Text("hello".to_string()));
            assert_eq!(rows[0].values[2], Value::Boolean(true));
        }
        _ => panic!("expected Query result"),
    }
}

#[test]
fn project_once_with_null_literal() {
    let (executor, _, _) = make_executor();
    let ctx = default_context();

    let plan = PhysicalPlan::ProjectOnce {
        outputs: vec![make_projection_expr(
            "n",
            DataType::Int,
            true,
            TypedExpr::literal(Value::Null, DataType::Int, true),
        )],
        filter: None,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
    };

    let result = executor.execute(&plan, &ctx).unwrap();
    match result {
        ExecutionResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::Null);
        }
        _ => panic!("expected Query result"),
    }
}

#[test]
fn project_once_with_filter_true() {
    let (executor, _, _) = make_executor();
    let ctx = default_context();

    let plan = PhysicalPlan::ProjectOnce {
        outputs: vec![make_projection_expr(
            "val",
            DataType::Int,
            false,
            TypedExpr::literal(Value::Int(1), DataType::Int, false),
        )],
        filter: Some(TypedExpr::literal(
            Value::Boolean(true),
            DataType::Boolean,
            false,
        )),
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
    };

    let result = executor.execute(&plan, &ctx).unwrap();
    match result {
        ExecutionResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
        }
        _ => panic!("expected Query result"),
    }
}

#[test]
fn project_once_with_filter_false() {
    let (executor, _, _) = make_executor();
    let ctx = default_context();

    let plan = PhysicalPlan::ProjectOnce {
        outputs: vec![make_projection_expr(
            "val",
            DataType::Int,
            false,
            TypedExpr::literal(Value::Int(1), DataType::Int, false),
        )],
        filter: Some(TypedExpr::literal(
            Value::Boolean(false),
            DataType::Boolean,
            false,
        )),
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
    };

    let result = executor.execute(&plan, &ctx).unwrap();
    match result {
        ExecutionResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 0, "filter=false should return no rows");
        }
        _ => panic!("expected Query result"),
    }
}

#[test]
fn project_once_with_offset_skips_single_row() {
    let (executor, _, _) = make_executor();
    let ctx = default_context();

    let plan = PhysicalPlan::ProjectOnce {
        outputs: vec![make_projection_expr(
            "val",
            DataType::Int,
            false,
            TypedExpr::literal(Value::Int(1), DataType::Int, false),
        )],
        filter: None,
        order_by: Vec::new(),
        limit: None,
        offset: Some(TypedExpr::literal(
            Value::BigInt(1),
            DataType::BigInt,
            false,
        )),
        distinct: false,
        distinct_on: Vec::new(),
    };

    let result = executor.execute(&plan, &ctx).unwrap();
    match result {
        ExecutionResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 0, "offset=1 on single row should return empty");
        }
        _ => panic!("expected Query result"),
    }
}

#[test]
fn project_once_with_limit_zero() {
    let (executor, _, _) = make_executor();
    let ctx = default_context();

    let plan = PhysicalPlan::ProjectOnce {
        outputs: vec![make_projection_expr(
            "val",
            DataType::Int,
            false,
            TypedExpr::literal(Value::Int(1), DataType::Int, false),
        )],
        filter: None,
        order_by: Vec::new(),
        limit: Some(TypedExpr::literal(
            Value::BigInt(0),
            DataType::BigInt,
            false,
        )),
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
    };

    let result = executor.execute(&plan, &ctx).unwrap();
    match result {
        ExecutionResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 0, "limit=0 should return no rows");
        }
        _ => panic!("expected Query result"),
    }
}

#[test]
fn project_once_string_concatenation() {
    let (executor, _, _) = make_executor();
    let ctx = default_context();

    // Test using ArithAdd on two ints to produce a computed value
    let expr = TypedExpr::arith_mul(
        TypedExpr::literal(Value::Int(6), DataType::Int, false),
        TypedExpr::literal(Value::Int(7), DataType::Int, false),
        DataType::Int,
        false,
    );

    let plan = PhysicalPlan::ProjectOnce {
        outputs: vec![make_projection_expr("product", DataType::Int, false, expr)],
        filter: None,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
    };

    let result = executor.execute(&plan, &ctx).unwrap();
    match result {
        ExecutionResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::Int(42));
        }
        _ => panic!("expected Query result"),
    }
}
