use super::*;
use crate::executor::node_registry::{CircuitBreakerState, NodeRegistry};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[test]
fn distributed_fragment_target_for_index_uses_configured_nodes_round_robin() {
    let configured = vec!["node-a".to_owned(), "node-b".to_owned()];
    assert_eq!(
        distributed_fragment_target_for_index(0, 4, &configured),
        FragmentTarget::Local
    );
    assert_eq!(
        distributed_fragment_target_for_index(1, 4, &configured),
        FragmentTarget::Remote("node-a".to_owned())
    );
    assert_eq!(
        distributed_fragment_target_for_index(2, 4, &configured),
        FragmentTarget::Remote("node-b".to_owned())
    );
    assert_eq!(
        distributed_fragment_target_for_index(3, 4, &configured),
        FragmentTarget::Remote("node-a".to_owned())
    );
    assert_eq!(
        distributed_fragment_target_for_index(4, 4, &configured),
        FragmentTarget::Local
    );
}

#[test]
fn distributed_fragment_target_for_index_falls_back_to_loopback_targets() {
    let configured: Vec<String> = Vec::new();
    assert_eq!(
        distributed_fragment_target_for_index(0, 3, &configured),
        FragmentTarget::Local
    );
    assert_eq!(
        distributed_fragment_target_for_index(1, 3, &configured),
        FragmentTarget::Remote("loopback:worker-1".to_owned())
    );
    assert_eq!(
        distributed_fragment_target_for_index(2, 3, &configured),
        FragmentTarget::Remote("loopback:worker-2".to_owned())
    );
}

#[test]
fn execute_distributed_fragments_concatenates_rows_in_fragment_order() {
    let (executor, _, _) = make_executor();
    let context = default_context().with_max_parallel_workers_per_query(4);

    let output_fields = vec![ResultField {
        name: "v".to_string(),
        data_type: DataType::Int,
        text_type_modifier: None,
        nullable: false,
    }];
    let fragments = vec![
        PhysicalPlan::ProjectValues {
            output_fields: output_fields.clone(),
            rows: vec![
                vec![TypedExpr::literal(Value::Int(1), DataType::Int, false)],
                vec![TypedExpr::literal(Value::Int(2), DataType::Int, false)],
            ],
            order_by: Vec::new(),
            limit: None,
            offset: None,
        },
        PhysicalPlan::ProjectValues {
            output_fields: output_fields.clone(),
            rows: vec![vec![TypedExpr::literal(
                Value::Int(3),
                DataType::Int,
                false,
            )]],
            order_by: Vec::new(),
            limit: None,
            offset: None,
        },
    ];

    let result = executor
        .execute_distributed_fragments(&fragments, &context)
        .expect("distributed fragments should execute");
    match result {
        ExecutionResult::Query { columns, rows } => {
            assert_eq!(columns, output_fields);
            assert_eq!(
                rows,
                vec![
                    Row::new(vec![Value::Int(1)]),
                    Row::new(vec![Value::Int(2)]),
                    Row::new(vec![Value::Int(3)]),
                ]
            );
        }
        other => panic!("expected Query result, got {other:?}"),
    }
}

#[test]
fn execute_distributed_fragments_rejects_schema_mismatch() {
    let (executor, _, _) = make_executor();
    let context = default_context().with_max_parallel_workers_per_query(2);

    let int_fields = vec![ResultField {
        name: "v".to_string(),
        data_type: DataType::Int,
        text_type_modifier: None,
        nullable: false,
    }];
    let text_fields = vec![ResultField {
        name: "v".to_string(),
        data_type: DataType::Text,
        text_type_modifier: None,
        nullable: false,
    }];
    let fragments = vec![
        PhysicalPlan::ProjectValues {
            output_fields: int_fields,
            rows: vec![vec![TypedExpr::literal(
                Value::Int(1),
                DataType::Int,
                false,
            )]],
            order_by: Vec::new(),
            limit: None,
            offset: None,
        },
        PhysicalPlan::ProjectValues {
            output_fields: text_fields,
            rows: vec![vec![TypedExpr::literal(
                Value::Text("one".to_string()),
                DataType::Text,
                false,
            )]],
            order_by: Vec::new(),
            limit: None,
            offset: None,
        },
    ];

    let error = executor
        .execute_distributed_fragments(&fragments, &context)
        .expect_err("schema mismatch should fail distributed execution");
    assert!(error
        .to_string()
        .contains("distributed fragments produced incompatible result schemas"));
    let detail = error
        .report()
        .internal_detail
        .as_deref()
        .unwrap_or_default();
    assert!(detail.contains("fragment #1 target=local"));
    assert!(detail.contains("expected_schema=[v:"));
    assert!(detail.contains("actual_schema=[v:"));
}

#[test]
fn execute_distributed_fragments_accepts_name_mismatch_when_types_match() {
    let (executor, _, _) = make_executor();
    let context = default_context().with_max_parallel_workers_per_query(2);

    let left_fields = vec![ResultField {
        name: "left_name".to_string(),
        data_type: DataType::Int,
        text_type_modifier: None,
        nullable: false,
    }];
    let right_fields = vec![ResultField {
        name: "right_name".to_string(),
        data_type: DataType::Int,
        text_type_modifier: None,
        nullable: false,
    }];
    let fragments = vec![
        PhysicalPlan::ProjectValues {
            output_fields: left_fields.clone(),
            rows: vec![vec![TypedExpr::literal(
                Value::Int(1),
                DataType::Int,
                false,
            )]],
            order_by: Vec::new(),
            limit: None,
            offset: None,
        },
        PhysicalPlan::ProjectValues {
            output_fields: right_fields,
            rows: vec![vec![TypedExpr::literal(
                Value::Int(2),
                DataType::Int,
                false,
            )]],
            order_by: Vec::new(),
            limit: None,
            offset: None,
        },
    ];

    let result = executor
        .execute_distributed_fragments(&fragments, &context)
        .expect("column name mismatch with matching types should be accepted");
    match result {
        ExecutionResult::Query { columns, rows } => {
            assert_eq!(columns, left_fields);
            assert_eq!(
                rows,
                vec![Row::new(vec![Value::Int(1)]), Row::new(vec![Value::Int(2)])]
            );
        }
        other => panic!("expected Query result, got {other:?}"),
    }
}

#[test]
fn execute_distributed_fragments_rejects_non_query_fragment_results_with_context() {
    let (executor, _, _) = make_executor();
    let context = default_context().with_max_parallel_workers_per_query(2);

    let fragments = vec![DistributedFragment::local(PhysicalPlan::InternalNoOp {
        tag: "NOOP".to_owned(),
        notice: None,
    })];

    let error = executor
        .execute_distributed_fragments_targeted(&fragments, &context)
        .expect_err("command fragments should be rejected for distributed query merge");
    assert!(error
        .to_string()
        .contains("distributed fragment did not return a query result"));
    let detail = error
        .report()
        .internal_detail
        .as_deref()
        .unwrap_or_default();
    assert!(detail.contains("fragment #0 target=local produced a non-query execution result"));
}

#[derive(Debug)]
struct InconsistentWidthDispatcher;

impl FragmentDispatcher for InconsistentWidthDispatcher {
    fn execute_fragment(
        &self,
        fragment: &DistributedFragment,
        _executor: &Executor,
        _context: &ExecutionContext,
    ) -> DbResult<ExecutionResult> {
        match &fragment.target {
            FragmentTarget::Local => Ok(ExecutionResult::Query {
                columns: Vec::new(),
                rows: vec![Row::new(vec![Value::Int(1)])],
            }),
            FragmentTarget::Remote(_) => Ok(ExecutionResult::Query {
                columns: Vec::new(),
                rows: vec![Row::new(vec![Value::Int(2), Value::Int(3)])],
            }),
        }
    }
}

#[test]
fn execute_distributed_fragments_rejects_inconsistent_row_widths_with_context() {
    let (executor, _, _) = make_executor();
    let executor = executor.with_fragment_dispatcher(Arc::new(InconsistentWidthDispatcher));
    let context = default_context().with_max_parallel_workers_per_query(2);

    let fragments = vec![
        DistributedFragment::local(PhysicalPlan::ProjectValues {
            output_fields: Vec::new(),
            rows: Vec::new(),
            order_by: Vec::new(),
            limit: None,
            offset: None,
        }),
        DistributedFragment::remote(
            "node-width",
            PhysicalPlan::ProjectValues {
                output_fields: Vec::new(),
                rows: Vec::new(),
                order_by: Vec::new(),
                limit: None,
                offset: None,
            },
        ),
    ];

    let error = executor
        .execute_distributed_fragments_targeted(&fragments, &context)
        .expect_err("inconsistent row widths must fail distributed merge");
    assert!(error
        .to_string()
        .contains("distributed fragments produced inconsistent row widths"));
    let detail = error
        .report()
        .internal_detail
        .as_deref()
        .unwrap_or_default();
    assert!(detail.contains("fragment #1 target=remote(node-width) row_index=0"));
    assert!(detail.contains("expected_row_width=1"));
    assert!(detail.contains("actual_row_width=2"));
}

#[test]
fn execute_distributed_append_plan_uses_explicit_fragments_and_finalization() {
    let (executor, _, _) = make_executor();
    let dispatcher = RegisteredRemoteFragmentDispatcher::new()
        .with_loopback_remote_targets(false)
        .with_remote_handler(
            "node-a",
            Arc::new(|fragment, executor, context| executor.execute(&fragment.plan, context)),
        );
    let executor = executor.with_fragment_dispatcher(Arc::new(dispatcher));
    let context = default_context()
        .with_max_parallel_workers_per_query(3)
        .with_distributed_loopback_remote_nodes(vec!["node-a".to_owned()]);

    let output_fields = vec![ResultField {
        name: "v".to_string(),
        data_type: DataType::Int,
        text_type_modifier: None,
        nullable: false,
    }];
    let make_fragment = |value: i32| PhysicalPlan::ProjectValues {
        output_fields: output_fields.clone(),
        rows: vec![vec![TypedExpr::literal(
            Value::Int(value),
            DataType::Int,
            false,
        )]],
        order_by: Vec::new(),
        limit: None,
        offset: None,
    };
    let plan = PhysicalPlan::DistributedAppend {
        fragments: vec![make_fragment(1), make_fragment(3), make_fragment(2)],
        output_fields: output_fields.clone(),
        order_by: vec![aiondb_plan::SortExpr {
            expr: TypedExpr::column_ref("v", 0, DataType::Int, false),
            descending: true,
            nulls_first: None,
        }],
        limit: Some(TypedExpr::literal(Value::Int(2), DataType::Int, false)),
        offset: None,
    };

    let result = executor
        .execute(&plan, &context)
        .expect("distributed append plan should execute");
    match result {
        ExecutionResult::Query { rows, columns } => {
            assert_eq!(columns, output_fields);
            assert_eq!(
                rows,
                vec![Row::new(vec![Value::Int(3)]), Row::new(vec![Value::Int(2)])]
            );
        }
        other => panic!("expected Query result, got {other:?}"),
    }
}

#[test]
fn execute_distributed_fragments_targeted_remote_without_dispatcher_fails() {
    let (executor, _, _) = make_executor();
    let context = default_context().with_max_parallel_workers_per_query(2);

    let output_fields = vec![ResultField {
        name: "v".to_string(),
        data_type: DataType::Int,
        text_type_modifier: None,
        nullable: false,
    }];
    let fragments = vec![DistributedFragment::remote(
        "node-a",
        PhysicalPlan::ProjectValues {
            output_fields,
            rows: vec![vec![TypedExpr::literal(
                Value::Int(1),
                DataType::Int,
                false,
            )]],
            order_by: Vec::new(),
            limit: None,
            offset: None,
        },
    )];

    let error = executor
        .execute_distributed_fragments_targeted(&fragments, &context)
        .expect_err("remote target should fail without dispatcher wiring");
    assert!(error
        .to_string()
        .contains("remote fragment execution target \"node-a\" is not configured"));
}

#[test]
fn execute_distributed_fragments_targeted_loopback_remote_uses_default_dispatcher() {
    let (executor, _, _) = make_executor();
    let context = default_context().with_max_parallel_workers_per_query(3);

    let output_fields = vec![ResultField {
        name: "v".to_string(),
        data_type: DataType::Int,
        text_type_modifier: None,
        nullable: false,
    }];
    let fragments = vec![
        DistributedFragment::local(PhysicalPlan::ProjectValues {
            output_fields: output_fields.clone(),
            rows: vec![vec![TypedExpr::literal(
                Value::Int(1),
                DataType::Int,
                false,
            )]],
            order_by: Vec::new(),
            limit: None,
            offset: None,
        }),
        DistributedFragment::remote(
            "loopback:worker-1",
            PhysicalPlan::ProjectValues {
                output_fields: output_fields.clone(),
                rows: vec![vec![TypedExpr::literal(
                    Value::Int(2),
                    DataType::Int,
                    false,
                )]],
                order_by: Vec::new(),
                limit: None,
                offset: None,
            },
        ),
    ];

    let result = executor
        .execute_distributed_fragments_targeted(&fragments, &context)
        .expect("loopback remote targets should execute via default dispatcher");
    match result {
        ExecutionResult::Query { rows, .. } => {
            assert_eq!(
                rows,
                vec![Row::new(vec![Value::Int(1)]), Row::new(vec![Value::Int(2)])]
            );
        }
        other => panic!("expected Query result, got {other:?}"),
    }
}

#[test]
fn execute_distributed_fragments_targeted_registered_dispatcher_routes_remote_targets() {
    let (executor, _, _) = make_executor();
    let (remote_executor, _, _) = make_executor();
    let remote_executor = Arc::new(remote_executor);

    let dispatcher = RegisteredRemoteFragmentDispatcher::new()
        .with_loopback_remote_targets(false)
        .with_remote_handler(
            "node-b",
            Arc::new(move |fragment, _executor, context| {
                remote_executor.execute(&fragment.plan, context)
            }),
        );
    let executor = executor.with_fragment_dispatcher(Arc::new(dispatcher));
    let context = default_context().with_max_parallel_workers_per_query(2);

    let output_fields = vec![ResultField {
        name: "v".to_string(),
        data_type: DataType::Int,
        text_type_modifier: None,
        nullable: false,
    }];
    let fragments = vec![
        DistributedFragment::local(PhysicalPlan::ProjectValues {
            output_fields: output_fields.clone(),
            rows: vec![vec![TypedExpr::literal(
                Value::Int(10),
                DataType::Int,
                false,
            )]],
            order_by: Vec::new(),
            limit: None,
            offset: None,
        }),
        DistributedFragment::remote(
            "node-b",
            PhysicalPlan::ProjectValues {
                output_fields: output_fields.clone(),
                rows: vec![vec![TypedExpr::literal(
                    Value::Int(20),
                    DataType::Int,
                    false,
                )]],
                order_by: Vec::new(),
                limit: None,
                offset: None,
            },
        ),
    ];

    let result = executor
        .execute_distributed_fragments_targeted(&fragments, &context)
        .expect("registered dispatcher should route remote fragments by node id");
    match result {
        ExecutionResult::Query { rows, .. } => {
            assert_eq!(
                rows,
                vec![
                    Row::new(vec![Value::Int(10)]),
                    Row::new(vec![Value::Int(20)])
                ]
            );
        }
        other => panic!("expected Query result, got {other:?}"),
    }
}

#[test]
fn registered_dispatcher_passes_shard_id_to_remote_handler_context() {
    let (executor, _, _) = make_executor();
    let dispatcher = RegisteredRemoteFragmentDispatcher::new()
        .with_loopback_remote_targets(false)
        .with_remote_handler(
            "node-b",
            Arc::new(|fragment, executor, context| {
                assert_eq!(context.distributed_current_shard_id, Some(7));
                executor.execute(&fragment.plan, context)
            }),
        );
    let executor = executor.with_fragment_dispatcher(Arc::new(dispatcher));
    let context = default_context().with_max_parallel_workers_per_query(1);

    let output_fields = vec![ResultField {
        name: "v".to_string(),
        data_type: DataType::Int,
        text_type_modifier: None,
        nullable: false,
    }];
    let fragment = DistributedFragment::remote(
        "node-b",
        PhysicalPlan::ProjectValues {
            output_fields,
            rows: vec![vec![TypedExpr::literal(
                Value::Int(7),
                DataType::Int,
                false,
            )]],
            order_by: Vec::new(),
            limit: None,
            offset: None,
        },
    )
    .with_shard_id(7);

    executor
        .execute_distributed_fragments_targeted(&[fragment], &context)
        .expect("remote handler should receive shard id in execution context");
}

#[test]
fn node_registry_passes_shard_id_to_remote_handler_context() {
    let (executor, _, _) = make_executor();
    let registry = Arc::new(NodeRegistry::with_circuit_breaker_config(
        1,
        std::time::Duration::from_secs(60),
    ));
    registry.register(
        "node-b".to_owned(),
        "127.0.0.1:9001".to_owned(),
        Arc::new(|fragment, executor, context| {
            assert_eq!(context.distributed_current_shard_id, Some(11));
            executor.execute(&fragment.plan, context)
        }),
    );
    let dispatcher = RegisteredRemoteFragmentDispatcher::new()
        .with_loopback_remote_targets(false)
        .with_node_registry(registry);
    let executor = executor.with_fragment_dispatcher(Arc::new(dispatcher));
    let context = default_context().with_max_parallel_workers_per_query(1);

    let output_fields = vec![ResultField {
        name: "v".to_string(),
        data_type: DataType::Int,
        text_type_modifier: None,
        nullable: false,
    }];
    let fragment = DistributedFragment::remote(
        "node-b",
        PhysicalPlan::ProjectValues {
            output_fields,
            rows: vec![vec![TypedExpr::literal(
                Value::Int(11),
                DataType::Int,
                false,
            )]],
            order_by: Vec::new(),
            limit: None,
            offset: None,
        },
    )
    .with_shard_id(11);

    executor
        .execute_distributed_fragments_targeted(&[fragment], &context)
        .expect("registry handler should receive shard id in execution context");
}

#[test]
fn registered_dispatcher_uses_node_registry_circuit_breaker_for_registered_remotes() {
    let (executor, _, _) = make_executor();
    let registry = Arc::new(NodeRegistry::with_circuit_breaker_config(
        1,
        std::time::Duration::from_secs(60),
    ));
    let dispatcher = RegisteredRemoteFragmentDispatcher::new()
        .with_loopback_remote_targets(false)
        .with_node_registry(Arc::clone(&registry));
    registry.register(
        "node-b".to_owned(),
        "127.0.0.1:9001".to_owned(),
        Arc::new(|_fragment, _executor, _context| {
            Err(DbError::protocol("injected remote transport failure"))
        }),
    );
    let executor = executor.with_fragment_dispatcher(Arc::new(dispatcher));
    let context = default_context().with_max_parallel_workers_per_query(1);

    let output_fields = vec![ResultField {
        name: "v".to_string(),
        data_type: DataType::Int,
        text_type_modifier: None,
        nullable: false,
    }];
    let fragment = DistributedFragment::remote(
        "node-b",
        PhysicalPlan::ProjectValues {
            output_fields,
            rows: vec![vec![TypedExpr::literal(
                Value::Int(1),
                DataType::Int,
                false,
            )]],
            order_by: Vec::new(),
            limit: None,
            offset: None,
        },
    );

    let first_error = executor
        .execute_distributed_fragments_targeted(std::slice::from_ref(&fragment), &context)
        .expect_err("first remote failure should be surfaced");
    assert!(first_error
        .to_string()
        .contains("injected remote transport failure"));
    assert_eq!(
        registry
            .get("node-b")
            .expect("node should be registered")
            .circuit_breaker()
            .state(),
        CircuitBreakerState::Open
    );

    let second_error = executor
        .execute_distributed_fragments_targeted(&[fragment], &context)
        .expect_err("open circuit should fail before dispatch");
    assert!(second_error.to_string().contains("circuit breaker is open"));
}

#[test]
fn registered_dispatcher_falls_back_to_direct_handler_for_unregistered_registry_node() {
    let (executor, _, _) = make_executor();
    let registry = Arc::new(NodeRegistry::with_circuit_breaker_config(
        1,
        std::time::Duration::from_secs(60),
    ));
    let dispatcher = RegisteredRemoteFragmentDispatcher::new()
        .with_loopback_remote_targets(false)
        .with_node_registry(registry)
        .with_remote_handler(
            "node-direct",
            Arc::new(|fragment, executor, context| executor.execute(&fragment.plan, context)),
        );
    let executor = executor.with_fragment_dispatcher(Arc::new(dispatcher));
    let context = default_context().with_max_parallel_workers_per_query(1);

    let output_fields = vec![ResultField {
        name: "v".to_string(),
        data_type: DataType::Int,
        text_type_modifier: None,
        nullable: false,
    }];
    let fragment = DistributedFragment::remote(
        "node-direct",
        PhysicalPlan::ProjectValues {
            output_fields,
            rows: vec![vec![TypedExpr::literal(
                Value::Int(7),
                DataType::Int,
                false,
            )]],
            order_by: Vec::new(),
            limit: None,
            offset: None,
        },
    );

    let result = executor
        .execute_distributed_fragments_targeted(&[fragment], &context)
        .expect("direct handler fallback should still route unregistered registry nodes");
    match result {
        ExecutionResult::Query { rows, .. } => {
            assert_eq!(rows, vec![Row::new(vec![Value::Int(7)])]);
        }
        other => panic!("expected Query result, got {other:?}"),
    }
}

#[test]
fn real_remote_fragments_ignore_shared_storage_hash_partition_filter() {
    let (executor, _, _) = make_executor();
    let dispatcher = RegisteredRemoteFragmentDispatcher::new()
        .with_loopback_remote_targets(false)
        .with_remote_handler(
            "node-b",
            Arc::new(|fragment, executor, context| executor.execute(&fragment.plan, context)),
        );
    let executor = executor.with_fragment_dispatcher(Arc::new(dispatcher));
    let context = default_context()
        .with_max_parallel_workers_per_query(2)
        .with_distributed_loopback_remote_nodes(vec!["node-b".to_owned()]);

    let output_fields = vec![ResultField {
        name: "v".to_string(),
        data_type: DataType::Int,
        text_type_modifier: None,
        nullable: false,
    }];
    let row = Row::new(vec![Value::Int(20)]);
    let partition_count = 2;
    let excluded_partition = (hash_partition_for_row(&row, partition_count) + 1) % partition_count;
    let fragment = DistributedFragment::remote(
        "node-b",
        PhysicalPlan::ProjectValues {
            output_fields,
            rows: vec![vec![TypedExpr::literal(
                Value::Int(20),
                DataType::Int,
                false,
            )]],
            order_by: Vec::new(),
            limit: None,
            offset: None,
        },
    )
    .with_partition(excluded_partition, partition_count);

    let result = executor
        .execute_distributed_fragments_targeted(&[fragment], &context)
        .expect("real remote fragment should not be hash-filtered");

    match result {
        ExecutionResult::Query { rows, .. } => {
            assert_eq!(rows, vec![row]);
        }
        other => panic!("expected Query result, got {other:?}"),
    }
}

#[test]
fn shared_storage_fragments_apply_hash_partition_filter() {
    let (executor, _, _) = make_executor();
    let context = default_context().with_max_parallel_workers_per_query(2);

    let output_fields = vec![ResultField {
        name: "v".to_string(),
        data_type: DataType::Int,
        text_type_modifier: None,
        nullable: false,
    }];
    let row = Row::new(vec![Value::Int(20)]);
    let partition_count = 2;
    let excluded_partition = (hash_partition_for_row(&row, partition_count) + 1) % partition_count;
    let fragment = DistributedFragment::local(PhysicalPlan::ProjectValues {
        output_fields,
        rows: vec![vec![TypedExpr::literal(
            Value::Int(20),
            DataType::Int,
            false,
        )]],
        order_by: Vec::new(),
        limit: None,
        offset: None,
    })
    .with_partition(excluded_partition, partition_count);

    let result = executor
        .execute_distributed_fragments_targeted(&[fragment], &context)
        .expect("shared-storage fragment should execute");

    match result {
        ExecutionResult::Query { rows, .. } => {
            assert!(rows.is_empty(), "row must be filtered by partition");
        }
        other => panic!("expected Query result, got {other:?}"),
    }
}

#[test]
fn execute_distributed_fragments_targeted_default_remote_handler_routes_unregistered_nodes() {
    let (executor, _, _) = make_executor();
    let dispatcher = RegisteredRemoteFragmentDispatcher::new()
        .with_loopback_remote_targets(false)
        .with_default_remote_handler(Arc::new(|fragment, executor, context| {
            executor.execute(&fragment.plan, context)
        }));
    let executor = executor.with_fragment_dispatcher(Arc::new(dispatcher));
    let context = default_context().with_max_parallel_workers_per_query(2);

    let output_fields = vec![ResultField {
        name: "v".to_string(),
        data_type: DataType::Int,
        text_type_modifier: None,
        nullable: false,
    }];
    let fragments = vec![DistributedFragment::remote(
        "node-unregistered",
        PhysicalPlan::ProjectValues {
            output_fields,
            rows: vec![vec![TypedExpr::literal(
                Value::Int(42),
                DataType::Int,
                false,
            )]],
            order_by: Vec::new(),
            limit: None,
            offset: None,
        },
    )];

    let result = executor
        .execute_distributed_fragments_targeted(&fragments, &context)
        .expect("default remote handler should execute unregistered targets");
    match result {
        ExecutionResult::Query { rows, .. } => {
            assert_eq!(rows, vec![Row::new(vec![Value::Int(42)])]);
        }
        other => panic!("expected Query result, got {other:?}"),
    }
}

#[test]
fn execute_distributed_fragments_targeted_preserves_registered_handler_over_default() {
    let (executor, _, _) = make_executor();
    let dispatcher = RegisteredRemoteFragmentDispatcher::new()
        .with_loopback_remote_targets(false)
        .with_default_remote_handler(Arc::new(|_fragment, _executor, _context| {
            Err(DbError::internal("default handler should not be used"))
        }))
        .with_remote_handler(
            "node-b",
            Arc::new(|fragment, executor, context| executor.execute(&fragment.plan, context)),
        );
    let executor = executor.with_fragment_dispatcher(Arc::new(dispatcher));
    let context = default_context().with_max_parallel_workers_per_query(2);

    let output_fields = vec![ResultField {
        name: "v".to_string(),
        data_type: DataType::Int,
        text_type_modifier: None,
        nullable: false,
    }];
    let fragments = vec![DistributedFragment::remote(
        "node-b",
        PhysicalPlan::ProjectValues {
            output_fields,
            rows: vec![vec![TypedExpr::literal(
                Value::Int(99),
                DataType::Int,
                false,
            )]],
            order_by: Vec::new(),
            limit: None,
            offset: None,
        },
    )];

    let result = executor
        .execute_distributed_fragments_targeted(&fragments, &context)
        .expect("registered handler should override default for matching node");
    match result {
        ExecutionResult::Query { rows, .. } => {
            assert_eq!(rows, vec![Row::new(vec![Value::Int(99)])]);
        }
        other => panic!("expected Query result, got {other:?}"),
    }
}

#[test]
fn execute_distributed_fragments_targeted_error_includes_fragment_context() {
    let (executor, _, _) = make_executor();
    let dispatcher = RegisteredRemoteFragmentDispatcher::new()
        .with_loopback_remote_targets(false)
        .with_remote_handler(
            "node-b",
            Arc::new(|_fragment, _executor, _context| {
                Err(DbError::internal("injected remote execution failure")
                    .with_internal_detail("remote inner detail"))
            }),
        );
    let executor = executor.with_fragment_dispatcher(Arc::new(dispatcher));
    let context = default_context().with_max_parallel_workers_per_query(2);

    let output_fields = vec![ResultField {
        name: "v".to_string(),
        data_type: DataType::Int,
        text_type_modifier: None,
        nullable: false,
    }];
    let fragments = vec![DistributedFragment::remote(
        "node-b",
        PhysicalPlan::ProjectValues {
            output_fields,
            rows: vec![vec![TypedExpr::literal(
                Value::Int(1),
                DataType::Int,
                false,
            )]],
            order_by: Vec::new(),
            limit: None,
            offset: None,
        },
    )];

    let error = executor
        .execute_distributed_fragments_targeted(&fragments, &context)
        .expect_err("remote failure should bubble with fragment context");
    let detail = error
        .report()
        .internal_detail
        .as_deref()
        .unwrap_or_default();
    assert!(detail.contains("distributed fragment #0 target=remote(node-b) failed"));
    assert!(detail.contains("remote inner detail"));
}

#[test]
fn union_all_nested_fragments_use_context_remote_nodes_for_assignment() {
    let (executor, _, _) = make_executor();
    let dispatcher = RegisteredRemoteFragmentDispatcher::new()
        .with_loopback_remote_targets(false)
        .with_remote_handler(
            "node-a",
            Arc::new(|fragment, executor, context| executor.execute(&fragment.plan, context)),
        );
    let executor = executor.with_fragment_dispatcher(Arc::new(dispatcher));
    let context = default_context()
        .with_max_parallel_workers_per_query(4)
        .with_distributed_loopback_remote_nodes(vec!["node-a".to_owned()]);

    let output_fields = vec![ResultField {
        name: "v".to_string(),
        data_type: DataType::Int,
        text_type_modifier: None,
        nullable: false,
    }];

    let leaf = |value: i32| PhysicalPlan::ProjectValues {
        output_fields: output_fields.clone(),
        rows: vec![vec![TypedExpr::literal(
            Value::Int(value),
            DataType::Int,
            false,
        )]],
        order_by: Vec::new(),
        limit: None,
        offset: None,
    };

    let left = PhysicalPlan::SetOperation {
        op: aiondb_plan::SetOperationType::Union,
        all: true,
        left: Box::new(leaf(1)),
        right: Box::new(leaf(2)),
        output_fields: output_fields.clone(),
        order_by: Vec::new(),
        limit: None,
        offset: None,
    };
    let right = PhysicalPlan::SetOperation {
        op: aiondb_plan::SetOperationType::Union,
        all: true,
        left: Box::new(leaf(3)),
        right: Box::new(leaf(4)),
        output_fields: output_fields.clone(),
        order_by: Vec::new(),
        limit: None,
        offset: None,
    };
    let plan = PhysicalPlan::SetOperation {
        op: aiondb_plan::SetOperationType::Union,
        all: true,
        left: Box::new(left),
        right: Box::new(right),
        output_fields,
        order_by: Vec::new(),
        limit: None,
        offset: None,
    };

    let result = executor
        .execute(&plan, &context)
        .expect("context-configured remote node assignment should execute");
    match result {
        ExecutionResult::Query { rows, .. } => {
            assert_eq!(
                rows,
                vec![
                    Row::new(vec![Value::Int(1)]),
                    Row::new(vec![Value::Int(2)]),
                    Row::new(vec![Value::Int(3)]),
                    Row::new(vec![Value::Int(4)]),
                ]
            );
        }
        other => panic!("expected Query result, got {other:?}"),
    }
}

#[derive(Debug)]
struct LoopbackFragmentDispatcher;

impl FragmentDispatcher for LoopbackFragmentDispatcher {
    fn execute_fragment(
        &self,
        fragment: &DistributedFragment,
        executor: &Executor,
        context: &ExecutionContext,
    ) -> DbResult<ExecutionResult> {
        match &fragment.target {
            FragmentTarget::Local | FragmentTarget::Remote(_) => {
                executor.execute(&fragment.plan, context)
            }
        }
    }
}

#[test]
fn execute_distributed_fragments_targeted_with_dispatcher_supports_remote() {
    let (executor, _, _) = make_executor();
    let executor = executor.with_fragment_dispatcher(Arc::new(LoopbackFragmentDispatcher));
    let context = default_context().with_max_parallel_workers_per_query(4);

    let output_fields = vec![ResultField {
        name: "v".to_string(),
        data_type: DataType::Int,
        text_type_modifier: None,
        nullable: false,
    }];
    let fragments = vec![
        DistributedFragment::local(PhysicalPlan::ProjectValues {
            output_fields: output_fields.clone(),
            rows: vec![vec![TypedExpr::literal(
                Value::Int(10),
                DataType::Int,
                false,
            )]],
            order_by: Vec::new(),
            limit: None,
            offset: None,
        }),
        DistributedFragment::remote(
            "node-b",
            PhysicalPlan::ProjectValues {
                output_fields: output_fields.clone(),
                rows: vec![vec![TypedExpr::literal(
                    Value::Int(20),
                    DataType::Int,
                    false,
                )]],
                order_by: Vec::new(),
                limit: None,
                offset: None,
            },
        ),
    ];

    let result = executor
        .execute_distributed_fragments_targeted(&fragments, &context)
        .expect("custom dispatcher should execute local+remote fragments");
    match result {
        ExecutionResult::Query { rows, .. } => {
            assert_eq!(
                rows,
                vec![
                    Row::new(vec![Value::Int(10)]),
                    Row::new(vec![Value::Int(20)])
                ]
            );
        }
        other => panic!("expected Query result, got {other:?}"),
    }
}

#[test]
fn execute_distributed_plan_routes_shard_fragment_to_context_leader() {
    let (executor, _, _) = make_executor();
    let remote_called = Arc::new(AtomicBool::new(false));
    let dispatcher = RegisteredRemoteFragmentDispatcher::new()
        .with_loopback_remote_targets(false)
        .with_remote_handler("node-b", {
            let remote_called = Arc::clone(&remote_called);
            Arc::new(move |fragment, executor, context| {
                remote_called.store(true, Ordering::SeqCst);
                executor.execute(&fragment.plan, context)
            })
        });
    let executor = executor.with_fragment_dispatcher(Arc::new(dispatcher));
    let context =
        default_context().with_distributed_shard_leader_nodes(vec![(7, "node-b".to_owned())]);

    let output_fields = vec![ResultField {
        name: "v".to_string(),
        data_type: DataType::Int,
        text_type_modifier: None,
        nullable: false,
    }];
    let source_plan = PhysicalPlan::ProjectValues {
        output_fields: output_fields.clone(),
        rows: vec![vec![TypedExpr::literal(
            Value::Int(42),
            DataType::Int,
            false,
        )]],
        order_by: Vec::new(),
        limit: None,
        offset: None,
    };
    let root_plan = PhysicalPlan::ProjectValues {
        output_fields: output_fields.clone(),
        rows: Vec::new(),
        order_by: Vec::new(),
        limit: None,
        offset: None,
    };
    let root_fragment_id = aiondb_cluster::FragmentId::new(0);
    let source_fragment_id = aiondb_cluster::FragmentId::new(1);
    let shard_id = aiondb_cluster::ShardId::new(7);
    let plan = aiondb_plan::DistributedPhysicalPlan::new(
        None,
        Default::default(),
        Default::default(),
        Default::default(),
        root_fragment_id,
        vec![
            aiondb_plan::PlanFragment::new(
                root_fragment_id,
                aiondb_plan::FragmentTarget::Coordinator,
                aiondb_plan::FragmentPlacement::Local,
                None,
                root_plan,
            ),
            aiondb_plan::PlanFragment::new(
                source_fragment_id,
                aiondb_plan::FragmentTarget::ShardLeader { shard_id },
                aiondb_plan::FragmentPlacement::Shard { shard_id },
                None,
                source_plan,
            ),
        ],
        vec![aiondb_plan::FragmentEdge::new(
            source_fragment_id,
            root_fragment_id,
            aiondb_plan::ExchangeKind::Gather,
        )],
    );

    let result = executor
        .execute_distributed(&plan, &context)
        .expect("distributed plan should execute");

    assert!(remote_called.load(Ordering::SeqCst));
    match result {
        ExecutionResult::Query { columns, rows } => {
            assert_eq!(columns, output_fields);
            assert_eq!(rows, vec![Row::new(vec![Value::Int(42)])]);
        }
        other => panic!("expected Query result, got {other:?}"),
    }
}

#[test]
fn union_all_nested_fragments_keep_order_limit_semantics() {
    let (executor, _, _) = make_executor();
    let context = default_context().with_max_parallel_workers_per_query(4);

    let output_fields = vec![ResultField {
        name: "v".to_string(),
        data_type: DataType::Int,
        text_type_modifier: None,
        nullable: false,
    }];

    let leaf = |value: i32| PhysicalPlan::ProjectValues {
        output_fields: output_fields.clone(),
        rows: vec![vec![TypedExpr::literal(
            Value::Int(value),
            DataType::Int,
            false,
        )]],
        order_by: Vec::new(),
        limit: None,
        offset: None,
    };

    let left = PhysicalPlan::SetOperation {
        op: aiondb_plan::SetOperationType::Union,
        all: true,
        left: Box::new(leaf(1)),
        right: Box::new(leaf(2)),
        output_fields: output_fields.clone(),
        order_by: Vec::new(),
        limit: None,
        offset: None,
    };
    let right = PhysicalPlan::SetOperation {
        op: aiondb_plan::SetOperationType::Union,
        all: true,
        left: Box::new(leaf(3)),
        right: Box::new(leaf(4)),
        output_fields: output_fields.clone(),
        order_by: Vec::new(),
        limit: None,
        offset: None,
    };
    let plan = PhysicalPlan::SetOperation {
        op: aiondb_plan::SetOperationType::Union,
        all: true,
        left: Box::new(left),
        right: Box::new(right),
        output_fields: output_fields.clone(),
        order_by: vec![aiondb_plan::SortExpr {
            expr: TypedExpr::column_ref("v", 0, DataType::Int, false),
            descending: true,
            nulls_first: None,
        }],
        limit: Some(TypedExpr::literal(Value::Int(2), DataType::Int, false)),
        offset: None,
    };

    let result = executor
        .execute(&plan, &context)
        .expect("nested union all should execute");
    match result {
        ExecutionResult::Query { rows, columns } => {
            assert_eq!(columns, output_fields);
            assert_eq!(
                rows,
                vec![Row::new(vec![Value::Int(4)]), Row::new(vec![Value::Int(3)])]
            );
        }
        other => panic!("expected Query result, got {other:?}"),
    }
}
