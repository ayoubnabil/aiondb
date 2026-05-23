#![allow(clippy::similar_names)]

//! End-to-end tests for distributed query execution.
//!
//! Uses loopback remote nodes to simulate multi-node execution within
//! a single process, testing fragment dispatch, timeout, cancellation,
//! and partial failure scenarios.

use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use aiondb_cluster::{MetadataReader, MetadataWriter};
use aiondb_config::{RemoteNodeConfig, SecurityConfig, SecurityProfile};
use aiondb_core::{DataType, RelationId, Value};
use aiondb_executor::ExecutionResult;
use aiondb_fragment_transport::server::{FragmentExecutor, FragmentServerConfig};
use aiondb_fragment_transport::{AuthToken, FragmentServer};
use aiondb_plan::{
    DistributedPhysicalPlan, ExchangeKind, FragmentEdge, FragmentPlacement, FragmentTarget,
    PhysicalPlan, PlanFragment, ProjectionExpr, ResultField, ScanAccessPath, TypedExpr,
};
use aiondb_security::AllowAllAuthorizer;

use super::*;

const TEST_INTER_NODE_AUTH_TOKEN: &str = "test-fragment-token-32-bytes-long";

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn real_fragment_transport_test_guard() -> MutexGuard<'static, ()> {
    static GUARD: OnceLock<Mutex<()>> = OnceLock::new();
    GUARD
        .get_or_init(|| Mutex::new(()))
        .lock()
        .expect("real fragment transport test guard poisoned")
}

/// Build an engine with loopback distributed workers configured.
///
/// `worker_count` controls the total number of workers including the
/// coordinator.  When `worker_count` is 1 no remote loopback nodes are
/// registered which means the engine behaves like a normal single-node
/// instance.
fn build_distributed_engine(worker_count: usize) -> Engine {
    let mut builder = EngineBuilder::for_testing();

    // Patch the runtime config to add loopback remote nodes.
    let mut runtime = RuntimeConfig::default();
    runtime.security = SecurityConfig::from_profile(SecurityProfile::Development);
    runtime.security.allow_ephemeral_users = true;
    runtime.limits.max_parallel_workers_per_query = worker_count;
    for i in 1..worker_count {
        runtime
            .distributed
            .loopback_remote_nodes
            .push(format!("loopback:worker-{i}"));
    }
    runtime.distributed.allow_unregistered_loopback_nodes = true;

    builder = builder.with_runtime_config(runtime);
    builder.build().expect("distributed engine should build")
}

fn build_remote_node_coordinator(remote_addr: String) -> Engine {
    let mut builder = EngineBuilder::for_testing();
    let mut runtime = RuntimeConfig::default();
    runtime.security = SecurityConfig::from_profile(SecurityProfile::Development);
    runtime.security.allow_ephemeral_users = true;
    runtime.limits.max_parallel_workers_per_query = 2;
    runtime.distributed.remote_nodes = vec![RemoteNodeConfig {
        node_id: "node-b".to_owned(),
        addr: remote_addr,
    }];
    runtime.distributed.inter_node_auth_token = Some(TEST_INTER_NODE_AUTH_TOKEN.to_owned());
    runtime.distributed.require_tls = false;

    builder = builder.with_runtime_config(runtime);
    builder
        .build()
        .expect("remote-node coordinator engine should build")
}

fn build_sharding_enabled_engine(label: &str) -> Engine {
    let mut runtime = RuntimeConfig::default();
    runtime.security = SecurityConfig::from_profile(SecurityProfile::Development);
    runtime.security.allow_ephemeral_users = true;
    runtime.distributed.sharding.enabled = true;
    let data_dir = crate::test_support::unique_temp_path("engine-tests-distributed", label);
    EngineBuilder::new_with_config(data_dir, runtime)
        .expect("sharded builder")
        .with_authorizer(Arc::new(AllowAllAuthorizer))
        .with_allow_ephemeral_users(true)
        .build()
        .expect("sharded engine")
}

fn unused_loopback_addr() -> String {
    let listener =
        std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback port");
    let addr = listener
        .local_addr()
        .expect("ephemeral loopback local addr");
    drop(listener);
    addr.to_string()
}

fn wait_for_fragment_server(addr: &str) {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        if std::net::TcpStream::connect(addr).is_ok() {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("fragment server did not start listening on {addr}");
}

struct RunningFragmentServer {
    addr: String,
    shutdown_tx: tokio::sync::watch::Sender<bool>,
    server_thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for RunningFragmentServer {
    fn drop(&mut self) {
        let _ = self.shutdown_tx.send(true);
        if let Some(thread) = self.server_thread.take() {
            thread.join().expect("fragment server thread joined");
        }
    }
}

fn start_fragment_server(remote_engine: Arc<Engine>) -> RunningFragmentServer {
    let addr = unused_loopback_addr();
    let server_config = FragmentServerConfig {
        listen_addr: addr.clone(),
        auth_token: AuthToken::new(TEST_INTER_NODE_AUTH_TOKEN),
        tls: None,
        max_connections: 8,
        request_timeout: Duration::from_secs(10),
        max_concurrent_executions: Some(4),
        allow_dev_mode_relaxations: true,
    };
    let remote_executor: Arc<dyn FragmentExecutor> = remote_engine;
    let server = FragmentServer::new(server_config, remote_executor);
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let server_thread = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("fragment server tokio runtime");
        runtime
            .block_on(server.run(shutdown_rx))
            .expect("fragment server should stop cleanly");
    });
    wait_for_fragment_server(&addr);
    RunningFragmentServer {
        addr,
        shutdown_tx,
        server_thread: Some(server_thread),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn distributed_union_all_executes_across_loopback_workers() {
    let engine = build_distributed_engine(3);
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE dist_t1 (id INT, label TEXT); \
             INSERT INTO dist_t1 VALUES \
                (1,'a'),(2,'b'),(3,'c'),(4,'d'),(5,'e'), \
                (6,'f'),(7,'g'),(8,'h'),(9,'i'),(10,'j')",
        )
        .expect("setup");

    let rows = query_rows(
        &engine,
        &session,
        "SELECT id, label FROM dist_t1 ORDER BY id",
    );
    assert_eq!(rows.len(), 10, "all 10 rows should be returned");
    assert_eq!(rows[0].values[0], Value::Int(1));
    assert_eq!(rows[9].values[0], Value::Int(10));
}

#[test]
fn distributed_scan_executes_against_real_fragment_transport_node() {
    let _guard = real_fragment_transport_test_guard();
    let remote_engine = Arc::new(build_distributed_engine(1));
    let (remote_session, _) = remote_engine
        .startup(startup_params())
        .expect("remote startup");
    remote_engine
        .execute_sql(&remote_session, "CREATE TABLE remote_dist_t (id INT)")
        .expect("remote create");
    let remote_server = start_fragment_server(remote_engine.clone());
    let coordinator = build_remote_node_coordinator(remote_server.addr.clone());
    let (session, _) = coordinator.startup(startup_params()).expect("startup");
    coordinator
        .execute_sql(
            &session,
            "CREATE TABLE remote_dist_t (id INT); INSERT INTO remote_dist_t VALUES (1)",
        )
        .expect("coordinator setup");
    remote_engine
        .execute_sql(&remote_session, "INSERT INTO remote_dist_t VALUES (2)")
        .expect("remote insert");

    let mut values: Vec<i32> = query_rows(&coordinator, &session, "SELECT id FROM remote_dist_t")
        .into_iter()
        .map(|row| match row.values[0] {
            Value::Int(value) => value,
            ref other => panic!("expected INT id, got {other:?}"),
        })
        .collect();
    values.sort_unstable();

    assert_eq!(values, vec![1, 2]);
}

#[test]
fn distributed_plan_shard_leader_executes_against_real_fragment_transport_node() {
    let _guard = real_fragment_transport_test_guard();
    let remote_engine = Arc::new(build_distributed_engine(1));
    let remote_addr = unused_loopback_addr();
    let server_config = FragmentServerConfig {
        listen_addr: remote_addr.clone(),
        auth_token: AuthToken::new(TEST_INTER_NODE_AUTH_TOKEN),
        tls: None,
        max_connections: 8,
        request_timeout: Duration::from_secs(10),
        max_concurrent_executions: Some(4),
        allow_dev_mode_relaxations: true,
    };
    let remote_executor: Arc<dyn FragmentExecutor> = remote_engine.clone();
    let server = FragmentServer::new(server_config, remote_executor);
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let server_thread = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("fragment server tokio runtime");
        runtime
            .block_on(server.run(shutdown_rx))
            .expect("fragment server should stop cleanly");
    });
    wait_for_fragment_server(&remote_addr);

    let coordinator = build_remote_node_coordinator(remote_addr);
    let (session, _) = coordinator.startup(startup_params()).expect("startup");
    let shard_id = aiondb_cluster::ShardId::new(7);
    coordinator
        .distributed_control_plane()
        .upsert_shard(aiondb_cluster::ShardDescriptor {
            database_id: aiondb_cluster::DatabaseId::DEFAULT,
            table_id: RelationId::new(42),
            shard_id,
            placements: vec![aiondb_cluster::ShardPlacement {
                shard_id,
                node_id: aiondb_cluster::NodeId::new("node-b"),
                role: aiondb_cluster::ReplicaRole::Leader,
                lease_epoch: aiondb_cluster::PlacementEpoch::default(),
            }],
        })
        .expect("register shard placement");

    let output_fields = vec![ResultField {
        name: "v".to_owned(),
        data_type: DataType::Int,
        text_type_modifier: None,
        nullable: false,
    }];
    let root_fragment_id = aiondb_cluster::FragmentId::new(0);
    let source_fragment_id = aiondb_cluster::FragmentId::new(1);
    let plan = DistributedPhysicalPlan::new(
        None,
        Default::default(),
        Default::default(),
        Default::default(),
        root_fragment_id,
        vec![
            PlanFragment::new(
                root_fragment_id,
                FragmentTarget::Coordinator,
                FragmentPlacement::Local,
                None,
                PhysicalPlan::ProjectValues {
                    output_fields: output_fields.clone(),
                    rows: Vec::new(),
                    order_by: Vec::new(),
                    limit: None,
                    offset: None,
                },
            ),
            PlanFragment::new(
                source_fragment_id,
                FragmentTarget::ShardLeader { shard_id },
                FragmentPlacement::Shard { shard_id },
                None,
                PhysicalPlan::ProjectValues {
                    output_fields: output_fields.clone(),
                    rows: vec![vec![TypedExpr::literal(
                        Value::Int(42),
                        DataType::Int,
                        false,
                    )]],
                    order_by: Vec::new(),
                    limit: None,
                    offset: None,
                },
            ),
        ],
        vec![FragmentEdge::new(
            source_fragment_id,
            root_fragment_id,
            ExchangeKind::Gather,
        )],
    );

    let (result, _) = coordinator
        .execute_distributed_plan(&session, &plan, None, 0)
        .expect("distributed plan should execute through remote shard leader");

    shutdown_tx
        .send(true)
        .expect("signal fragment server shutdown");
    server_thread.join().expect("fragment server thread joined");

    let StatementResult::Query { rows, .. } = result else {
        panic!("expected query result, got {result:?}");
    };
    assert_eq!(rows, vec![aiondb_core::Row::new(vec![Value::Int(42)])]);
}

#[test]
fn create_sharded_table_registers_control_plane_placements() {
    let _guard = real_fragment_transport_test_guard();
    let remote_engine = Arc::new(build_sharding_enabled_engine("sharded-cp-remote"));
    let remote_server = start_fragment_server(remote_engine);
    let mut builder = EngineBuilder::for_testing();
    let mut runtime = RuntimeConfig::default();
    runtime.security = SecurityConfig::from_profile(SecurityProfile::Development);
    runtime.security.allow_ephemeral_users = true;
    runtime.distributed.sharding.enabled = true;
    runtime.distributed.remote_nodes = vec![RemoteNodeConfig {
        node_id: "node-b".to_owned(),
        addr: remote_server.addr.clone(),
    }];
    runtime.distributed.inter_node_auth_token = Some(TEST_INTER_NODE_AUTH_TOKEN.to_owned());
    runtime.distributed.require_tls = false;
    builder = builder.with_runtime_config(runtime);
    let engine = builder.build().expect("sharded distributed engine");
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE sharded_cp (id INT, value TEXT) \
             WITH (shard_key = 'id', shard_count = 3)",
        )
        .expect("create sharded table");

    let shards = engine
        .distributed_control_plane()
        .database_shards(aiondb_cluster::DatabaseId::DEFAULT)
        .expect("control-plane shards");
    let leaders: Vec<_> = shards
        .iter()
        .map(|shard| {
            shard
                .placements
                .iter()
                .find(|placement| placement.role == aiondb_cluster::ReplicaRole::Leader)
                .map(|placement| placement.node_id.as_str().to_owned())
                .expect("leader placement")
        })
        .collect();

    assert_eq!(shards.len(), 3);
    assert_eq!(leaders, vec!["local", "node-b", "local"]);

    engine
        .execute_sql(
            &session,
            "CREATE TABLE sharded_cp_two (id INT, value TEXT) \
             WITH (shard_key = 'id', shard_count = 3)",
        )
        .expect("create second sharded table with the same logical shard ids");
    let first_table = engine
        .catalog_reader
        .get_table(
            aiondb_core::TxnId::default(),
            &aiondb_catalog::QualifiedName::parse("sharded_cp"),
        )
        .expect("catalog read")
        .expect("first sharded table");
    let second_table = engine
        .catalog_reader
        .get_table(
            aiondb_core::TxnId::default(),
            &aiondb_catalog::QualifiedName::parse("sharded_cp_two"),
        )
        .expect("catalog read")
        .expect("second sharded table");
    assert_eq!(
        engine
            .distributed_control_plane()
            .table_shards(aiondb_cluster::DatabaseId::DEFAULT, first_table.table_id)
            .expect("first table shards")
            .len(),
        3
    );
    assert_eq!(
        engine
            .distributed_control_plane()
            .table_shards(aiondb_cluster::DatabaseId::DEFAULT, second_table.table_id)
            .expect("second table shards")
            .len(),
        3
    );
    assert_eq!(
        engine
            .distributed_control_plane()
            .database_shards(aiondb_cluster::DatabaseId::DEFAULT)
            .expect("all database shards")
            .len(),
        6
    );
}

#[test]
fn sharded_insert_routes_remote_leader_rows_over_fragment_transport() {
    let _guard = real_fragment_transport_test_guard();
    let remote_engine = Arc::new(build_sharding_enabled_engine("remote-sharded-insert"));
    let (remote_session, _) = remote_engine
        .startup(startup_params())
        .expect("remote startup");
    let remote_server = start_fragment_server(remote_engine.clone());

    let mut runtime = RuntimeConfig::default();
    runtime.security = SecurityConfig::from_profile(SecurityProfile::Development);
    runtime.security.allow_ephemeral_users = true;
    runtime.distributed.sharding.enabled = true;
    runtime.distributed.remote_nodes = vec![RemoteNodeConfig {
        node_id: "node-b".to_owned(),
        addr: remote_server.addr.clone(),
    }];
    runtime.distributed.inter_node_auth_token = Some(TEST_INTER_NODE_AUTH_TOKEN.to_owned());
    runtime.distributed.require_tls = false;
    let data_dir =
        crate::test_support::unique_temp_path("engine-tests-distributed", "sharded-remote-insert");
    let coordinator = EngineBuilder::new_with_config(data_dir, runtime)
        .expect("coordinator builder")
        .with_authorizer(Arc::new(AllowAllAuthorizer))
        .with_allow_ephemeral_users(true)
        .build()
        .expect("coordinator");
    let (session, _) = coordinator.startup(startup_params()).expect("startup");

    coordinator
        .execute_sql(
            &session,
            "CREATE TABLE sharded_remote_insert (id INT) \
             WITH (shard_key = 'id', shard_count = 2); \
             INSERT INTO sharded_remote_insert VALUES \
                (0),(1),(2),(3),(4),(5),(6),(7),(8),(9),(10),(11),(12),(13),(14),(15)",
        )
        .expect("create and insert through coordinator");

    let remote_rows = query_rows(
        remote_engine.as_ref(),
        &remote_session,
        "SELECT id FROM sharded_remote_insert",
    );
    assert!(
        !remote_rows.is_empty() && remote_rows.len() < 16,
        "remote shard leader should receive a non-empty subset, got {} rows",
        remote_rows.len()
    );

    coordinator
        .execute_sql(
            &session,
            "CREATE TABLE sharded_on_conflict (id INT PRIMARY KEY, note TEXT) \
             WITH (shard_key = 'id', shard_count = 2); \
             INSERT INTO sharded_on_conflict VALUES \
                (16, 'a'), (17, 'b'), (18, 'c'), (19, 'd')",
        )
        .expect("create and seed sharded on conflict table");
    let on_conflict_results = coordinator
        .execute_sql(
            &session,
            "INSERT INTO sharded_on_conflict VALUES \
             (17, 'dup-b'), (18, 'dup-c'), (24, 'e'), (25, 'f') \
             ON CONFLICT (id) DO NOTHING",
        )
        .expect("remote sharded insert on conflict do nothing");
    assert_eq!(
        on_conflict_results,
        vec![StatementResult::Command {
            tag: "INSERT".to_owned(),
            rows_affected: 2
        }]
    );
    let on_conflict_rows = query_rows(
        &coordinator,
        &session,
        "SELECT id, note FROM sharded_on_conflict ORDER BY id",
    );
    assert_eq!(
        on_conflict_rows
            .iter()
            .map(|row| row.values.clone())
            .collect::<Vec<_>>(),
        vec![
            vec![Value::Int(16), Value::Text("a".to_owned())],
            vec![Value::Int(17), Value::Text("b".to_owned())],
            vec![Value::Int(18), Value::Text("c".to_owned())],
            vec![Value::Int(19), Value::Text("d".to_owned())],
            vec![Value::Int(24), Value::Text("e".to_owned())],
            vec![Value::Int(25), Value::Text("f".to_owned())],
        ]
    );
    let on_conflict_update_results = coordinator
        .execute_sql(
            &session,
            "INSERT INTO sharded_on_conflict VALUES \
             (17, 'updated-b'), (26, 'g') \
             ON CONFLICT (id) DO UPDATE SET note = excluded.note",
        )
        .expect("remote sharded insert on conflict do update");
    assert_eq!(
        on_conflict_update_results,
        vec![StatementResult::Command {
            tag: "INSERT".to_owned(),
            rows_affected: 2
        }]
    );
    let on_conflict_update_rows = query_rows(
        &coordinator,
        &session,
        "SELECT id, note FROM sharded_on_conflict ORDER BY id",
    );
    assert_eq!(
        on_conflict_update_rows
            .iter()
            .map(|row| row.values.clone())
            .collect::<Vec<_>>(),
        vec![
            vec![Value::Int(16), Value::Text("a".to_owned())],
            vec![Value::Int(17), Value::Text("updated-b".to_owned())],
            vec![Value::Int(18), Value::Text("c".to_owned())],
            vec![Value::Int(19), Value::Text("d".to_owned())],
            vec![Value::Int(24), Value::Text("e".to_owned())],
            vec![Value::Int(25), Value::Text("f".to_owned())],
            vec![Value::Int(26), Value::Text("g".to_owned())],
        ]
    );
    coordinator
        .execute_sql(&session, "DROP TABLE sharded_on_conflict")
        .expect("drop sharded on conflict table");

    coordinator
        .execute_sql(
            &session,
            "CREATE TABLE sharded_returning (id INT, note TEXT) \
             WITH (shard_key = 'id', shard_count = 2)",
        )
        .expect("create sharded returning table");
    let returning_results = coordinator
        .execute_sql(
            &session,
            "INSERT INTO sharded_returning VALUES \
             (20, 'a'), (21, 'b'), (22, 'c'), (23, 'd') RETURNING id, note",
        )
        .expect("remote sharded insert returning");
    let [StatementResult::Query {
        rows: returning_rows,
        ..
    }] = returning_results.as_slice()
    else {
        panic!("expected INSERT RETURNING query result, got {returning_results:?}");
    };
    assert_eq!(
        returning_rows
            .iter()
            .map(|row| row.values.clone())
            .collect::<Vec<_>>(),
        vec![
            vec![Value::Int(20), Value::Text("a".to_owned())],
            vec![Value::Int(21), Value::Text("b".to_owned())],
            vec![Value::Int(22), Value::Text("c".to_owned())],
            vec![Value::Int(23), Value::Text("d".to_owned())],
        ]
    );
    coordinator
        .execute_sql(&session, "DROP TABLE sharded_returning")
        .expect("drop sharded returning table");

    coordinator
        .execute_sql(
            &session,
            "CREATE TABLE sharded_insert_defaults \
             (id INT, note TEXT DEFAULT 'fallback') \
             WITH (shard_key = 'id', shard_count = 2)",
        )
        .expect("create sharded defaults table");
    coordinator
        .execute_sql(
            &session,
            "INSERT INTO sharded_insert_defaults (note, id) VALUES \
             ('explicit-a', 30), (DEFAULT, 31), ('explicit-c', 32)",
        )
        .expect("remote sharded insert should accept explicit column order and defaults");
    let defaults_rows = query_rows(
        &coordinator,
        &session,
        "SELECT id, note FROM sharded_insert_defaults ORDER BY id",
    );
    assert_eq!(
        defaults_rows
            .iter()
            .map(|row| row.values.clone())
            .collect::<Vec<_>>(),
        vec![
            vec![Value::Int(30), Value::Text("explicit-a".to_owned())],
            vec![Value::Int(31), Value::Text("fallback".to_owned())],
            vec![Value::Int(32), Value::Text("explicit-c".to_owned())],
        ]
    );
    coordinator
        .execute_sql(&session, "DROP TABLE sharded_insert_defaults")
        .expect("drop sharded defaults table");

    coordinator
        .execute_sql(
            &session,
            "CREATE TABLE sharded_delete (id INT, note TEXT) \
             WITH (shard_key = 'id', shard_count = 2); \
             INSERT INTO sharded_delete VALUES \
                (40, 'a'), (41, 'b'), (42, 'c'), (43, 'd'), (44, 'e'), (45, 'f')",
        )
        .expect("create and seed sharded delete table");
    let delete_results = coordinator
        .execute_sql(&session, "DELETE FROM sharded_delete WHERE id < 43")
        .expect("remote sharded delete");
    assert_eq!(
        delete_results,
        vec![StatementResult::Command {
            tag: "DELETE".to_owned(),
            rows_affected: 3
        }]
    );
    let mut delete_returning_rows = query_rows(
        &coordinator,
        &session,
        "DELETE FROM sharded_delete WHERE id >= 44 RETURNING id",
    );
    delete_returning_rows.sort_by(|left, right| {
        let Value::Int(left_key) = left.values[0] else {
            panic!(
                "expected INT delete returning key, got {:?}",
                left.values[0]
            );
        };
        let Value::Int(right_key) = right.values[0] else {
            panic!(
                "expected INT delete returning key, got {:?}",
                right.values[0]
            );
        };
        left_key.cmp(&right_key)
    });
    assert_eq!(
        delete_returning_rows
            .iter()
            .map(|row| row.values.clone())
            .collect::<Vec<_>>(),
        vec![vec![Value::Int(44)], vec![Value::Int(45)]]
    );
    let remaining_delete_rows = query_rows(
        &coordinator,
        &session,
        "SELECT id, note FROM sharded_delete ORDER BY id",
    );
    assert_eq!(
        remaining_delete_rows
            .iter()
            .map(|row| row.values.clone())
            .collect::<Vec<_>>(),
        vec![vec![Value::Int(43), Value::Text("d".to_owned())]]
    );
    coordinator
        .execute_sql(&session, "DROP TABLE sharded_delete")
        .expect("drop sharded delete table");

    coordinator
        .execute_sql(
            &session,
            "CREATE TABLE sharded_delete_using_target (id INT, note TEXT) \
                WITH (shard_key = 'id', shard_count = 2); \
             CREATE TABLE sharded_delete_using_source (id INT) \
                WITH (shard_key = 'id', shard_count = 2); \
             INSERT INTO sharded_delete_using_target VALUES \
                (200, 'keep-a'), (201, 'delete-a'), (202, 'keep-b'), \
                (203, 'keep-c'), (204, 'delete-b'), (205, 'keep-d'); \
             INSERT INTO sharded_delete_using_source VALUES (201), (204), (999)",
        )
        .expect("create and seed sharded delete using tables");
    let mut delete_using_rows = query_rows(
        &coordinator,
        &session,
        "DELETE FROM sharded_delete_using_target t \
         USING sharded_delete_using_source s \
         WHERE t.id = s.id RETURNING t.id",
    );
    delete_using_rows.sort_by(|left, right| {
        let Value::Int(left_key) = left.values[0] else {
            panic!(
                "expected INT delete using returning key, got {:?}",
                left.values[0]
            );
        };
        let Value::Int(right_key) = right.values[0] else {
            panic!(
                "expected INT delete using returning key, got {:?}",
                right.values[0]
            );
        };
        left_key.cmp(&right_key)
    });
    assert_eq!(
        delete_using_rows
            .iter()
            .map(|row| row.values.clone())
            .collect::<Vec<_>>(),
        vec![vec![Value::Int(201)], vec![Value::Int(204)]]
    );
    let remaining_delete_using_rows = query_rows(
        &coordinator,
        &session,
        "SELECT id, note FROM sharded_delete_using_target ORDER BY id",
    );
    assert_eq!(
        remaining_delete_using_rows
            .iter()
            .map(|row| row.values.clone())
            .collect::<Vec<_>>(),
        vec![
            vec![Value::Int(200), Value::Text("keep-a".to_owned())],
            vec![Value::Int(202), Value::Text("keep-b".to_owned())],
            vec![Value::Int(203), Value::Text("keep-c".to_owned())],
            vec![Value::Int(205), Value::Text("keep-d".to_owned())],
        ]
    );
    coordinator
        .execute_sql(&session, "DROP TABLE sharded_delete_using_source")
        .expect("drop sharded delete using source table");
    coordinator
        .execute_sql(&session, "DROP TABLE sharded_delete_using_target")
        .expect("drop sharded delete using target table");

    coordinator
        .execute_sql(
            &session,
            "CREATE TABLE sharded_update (id INT, note TEXT) \
             WITH (shard_key = 'id', shard_count = 2); \
             INSERT INTO sharded_update VALUES \
                (50, 'a'), (51, 'b'), (52, 'c'), (53, 'd'), (54, 'e'), (55, 'f')",
        )
        .expect("create and seed sharded update table");
    let update_results = coordinator
        .execute_sql(
            &session,
            "UPDATE sharded_update SET note = 'low' WHERE id < 53",
        )
        .expect("remote sharded update");
    assert_eq!(
        update_results,
        vec![StatementResult::Command {
            tag: "UPDATE".to_owned(),
            rows_affected: 3
        }]
    );
    let mut update_returning_rows = query_rows(
        &coordinator,
        &session,
        "UPDATE sharded_update SET note = 'high' WHERE id >= 54 RETURNING id, note",
    );
    update_returning_rows.sort_by(|left, right| {
        let Value::Int(left_key) = left.values[0] else {
            panic!(
                "expected INT update returning key, got {:?}",
                left.values[0]
            );
        };
        let Value::Int(right_key) = right.values[0] else {
            panic!(
                "expected INT update returning key, got {:?}",
                right.values[0]
            );
        };
        left_key.cmp(&right_key)
    });
    assert_eq!(
        update_returning_rows
            .iter()
            .map(|row| row.values.clone())
            .collect::<Vec<_>>(),
        vec![
            vec![Value::Int(54), Value::Text("high".to_owned())],
            vec![Value::Int(55), Value::Text("high".to_owned())],
        ]
    );
    let shard_key_update_err = coordinator
        .execute_sql(
            &session,
            "UPDATE sharded_update SET id = id + 100 WHERE id = 53",
        )
        .expect_err("remote sharded update should reject shard-key movement");
    assert_eq!(
        shard_key_update_err.sqlstate(),
        aiondb_core::SqlState::FeatureNotSupported
    );
    assert!(
        shard_key_update_err
            .to_string()
            .contains("cannot modify shard key columns"),
        "unexpected shard-key UPDATE error: {shard_key_update_err}"
    );
    let _ = coordinator.execute_sql(&session, "ROLLBACK");
    let updated_rows = query_rows(
        &coordinator,
        &session,
        "SELECT id, note FROM sharded_update ORDER BY id",
    );
    assert_eq!(
        updated_rows
            .iter()
            .map(|row| row.values.clone())
            .collect::<Vec<_>>(),
        vec![
            vec![Value::Int(50), Value::Text("low".to_owned())],
            vec![Value::Int(51), Value::Text("low".to_owned())],
            vec![Value::Int(52), Value::Text("low".to_owned())],
            vec![Value::Int(53), Value::Text("d".to_owned())],
            vec![Value::Int(54), Value::Text("high".to_owned())],
            vec![Value::Int(55), Value::Text("high".to_owned())],
        ]
    );
    coordinator
        .execute_sql(&session, "DROP TABLE sharded_update")
        .expect("drop sharded update table");

    coordinator
        .execute_sql(
            &session,
            "CREATE TABLE sharded_update_from_target (id INT, note TEXT) \
                WITH (shard_key = 'id', shard_count = 2); \
             CREATE TABLE sharded_update_from_source (id INT, note TEXT) \
                WITH (shard_key = 'id', shard_count = 2); \
             INSERT INTO sharded_update_from_target VALUES \
                (300, 'keep-a'), (301, 'old-a'), (302, 'keep-b'), \
                (303, 'old-b'), (304, 'keep-c'), (305, 'old-c'); \
             INSERT INTO sharded_update_from_source VALUES \
                (301, 'new-a'), (303, 'new-b'), (305, 'new-c'), (999, 'ignored')",
        )
        .expect("create and seed sharded update from tables");
    let mut update_from_rows = query_rows(
        &coordinator,
        &session,
        "UPDATE sharded_update_from_target t \
         SET note = s.note \
         FROM sharded_update_from_source s \
         WHERE t.id = s.id RETURNING t.id, t.note",
    );
    update_from_rows.sort_by(|left, right| {
        let Value::Int(left_key) = left.values[0] else {
            panic!(
                "expected INT update from returning key, got {:?}",
                left.values[0]
            );
        };
        let Value::Int(right_key) = right.values[0] else {
            panic!(
                "expected INT update from returning key, got {:?}",
                right.values[0]
            );
        };
        left_key.cmp(&right_key)
    });
    assert_eq!(
        update_from_rows
            .iter()
            .map(|row| row.values.clone())
            .collect::<Vec<_>>(),
        vec![
            vec![Value::Int(301), Value::Text("new-a".to_owned())],
            vec![Value::Int(303), Value::Text("new-b".to_owned())],
            vec![Value::Int(305), Value::Text("new-c".to_owned())],
        ]
    );
    let update_from_target_rows = query_rows(
        &coordinator,
        &session,
        "SELECT id, note FROM sharded_update_from_target ORDER BY id",
    );
    assert_eq!(
        update_from_target_rows
            .iter()
            .map(|row| row.values.clone())
            .collect::<Vec<_>>(),
        vec![
            vec![Value::Int(300), Value::Text("keep-a".to_owned())],
            vec![Value::Int(301), Value::Text("new-a".to_owned())],
            vec![Value::Int(302), Value::Text("keep-b".to_owned())],
            vec![Value::Int(303), Value::Text("new-b".to_owned())],
            vec![Value::Int(304), Value::Text("keep-c".to_owned())],
            vec![Value::Int(305), Value::Text("new-c".to_owned())],
        ]
    );
    coordinator
        .execute_sql(&session, "DROP TABLE sharded_update_from_source")
        .expect("drop sharded update from source table");
    coordinator
        .execute_sql(&session, "DROP TABLE sharded_update_from_target")
        .expect("drop sharded update from target table");

    coordinator
        .execute_sql(
            &session,
            "CREATE TABLE sharded_copy (id INT, note TEXT) \
             WITH (shard_key = 'id', shard_count = 2)",
        )
        .expect("create sharded copy table");
    let copy_in = coordinator
        .execute_sql(&session, "COPY sharded_copy FROM STDIN")
        .expect("remote sharded copy marker");
    let [StatementResult::CopyIn { table_id, .. }] = copy_in.as_slice() else {
        panic!("expected COPY IN marker, got {copy_in:?}");
    };
    let copy_result = coordinator
        .execute_copy_from(
            &session,
            *table_id,
            "60\tcopy-a\n61\tcopy-b\n62\tcopy-c\n63\tcopy-d\n64\tcopy-e\n65\tcopy-f\n",
        )
        .expect("remote sharded copy from data");
    assert_eq!(
        copy_result,
        StatementResult::Command {
            tag: "COPY".to_owned(),
            rows_affected: 6
        }
    );
    let copy_rows = query_rows(
        &coordinator,
        &session,
        "SELECT id, note FROM sharded_copy ORDER BY id",
    );
    assert_eq!(
        copy_rows
            .iter()
            .map(|row| row.values.clone())
            .collect::<Vec<_>>(),
        vec![
            vec![Value::Int(60), Value::Text("copy-a".to_owned())],
            vec![Value::Int(61), Value::Text("copy-b".to_owned())],
            vec![Value::Int(62), Value::Text("copy-c".to_owned())],
            vec![Value::Int(63), Value::Text("copy-d".to_owned())],
            vec![Value::Int(64), Value::Text("copy-e".to_owned())],
            vec![Value::Int(65), Value::Text("copy-f".to_owned())],
        ]
    );
    let remote_copy_rows = query_rows(
        remote_engine.as_ref(),
        &remote_session,
        "SELECT id FROM sharded_copy",
    );
    assert!(
        !remote_copy_rows.is_empty() && remote_copy_rows.len() < 6,
        "remote shard leader should receive a non-empty COPY subset, got {} rows",
        remote_copy_rows.len()
    );
    coordinator
        .execute_sql(&session, "DROP TABLE sharded_copy")
        .expect("drop sharded copy table");

    let table = coordinator
        .catalog_reader
        .get_table(
            aiondb_core::TxnId::default(),
            &aiondb_catalog::QualifiedName::parse("sharded_remote_insert"),
        )
        .expect("catalog read")
        .expect("created table");
    let output_fields = vec![ResultField {
        name: "id".to_owned(),
        data_type: DataType::Int,
        text_type_modifier: None,
        nullable: true,
    }];
    let output = ProjectionExpr {
        field: output_fields[0].clone(),
        expr: TypedExpr::column_ref("id", 0, DataType::Int, true),
    };
    let make_scan = || PhysicalPlan::ProjectTable {
        table_id: table.table_id,
        outputs: vec![output.clone()],
        filter: None,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
        access_path: ScanAccessPath::SeqScan,
    };
    let root_fragment_id = aiondb_cluster::FragmentId::new(0);
    let shard0_fragment_id = aiondb_cluster::FragmentId::new(1);
    let shard1_fragment_id = aiondb_cluster::FragmentId::new(2);
    let shard0 = aiondb_cluster::ShardId::new(0);
    let shard1 = aiondb_cluster::ShardId::new(1);
    let plan = DistributedPhysicalPlan::new(
        None,
        Default::default(),
        Default::default(),
        Default::default(),
        root_fragment_id,
        vec![
            PlanFragment::new(
                root_fragment_id,
                FragmentTarget::Coordinator,
                FragmentPlacement::Local,
                None,
                PhysicalPlan::ProjectValues {
                    output_fields: output_fields.clone(),
                    rows: Vec::new(),
                    order_by: Vec::new(),
                    limit: None,
                    offset: None,
                },
            ),
            PlanFragment::new(
                shard0_fragment_id,
                FragmentTarget::ShardLeader { shard_id: shard0 },
                FragmentPlacement::Shard { shard_id: shard0 },
                None,
                make_scan(),
            ),
            PlanFragment::new(
                shard1_fragment_id,
                FragmentTarget::ShardLeader { shard_id: shard1 },
                FragmentPlacement::Shard { shard_id: shard1 },
                None,
                make_scan(),
            ),
        ],
        vec![
            FragmentEdge::new(shard0_fragment_id, root_fragment_id, ExchangeKind::Gather),
            FragmentEdge::new(shard1_fragment_id, root_fragment_id, ExchangeKind::Gather),
        ],
    );

    let (result, _) = coordinator
        .execute_distributed_plan(&session, &plan, None, 0)
        .expect("distributed sharded scan");
    let StatementResult::Query { rows, .. } = result else {
        panic!("expected query result, got {result:?}");
    };
    let mut values: Vec<i32> = rows
        .into_iter()
        .map(|row| match &row.values[0] {
            Value::Int(value) => *value,
            other => panic!("expected int, got {other:?}"),
        })
        .collect();
    values.sort_unstable();

    assert_eq!(values, (0..16).collect::<Vec<_>>());

    let mut sql_values: Vec<i32> = query_rows(
        &coordinator,
        &session,
        "SELECT id FROM sharded_remote_insert",
    )
    .into_iter()
    .map(|row| match row.values[0] {
        Value::Int(value) => value,
        ref other => panic!("expected INT id, got {other:?}"),
    })
    .collect();
    sql_values.sort_unstable();
    assert_eq!(sql_values, (0..16).collect::<Vec<_>>());

    let ordered_limited: Vec<i32> = query_rows(
        &coordinator,
        &session,
        "SELECT id FROM sharded_remote_insert ORDER BY id DESC LIMIT 5 OFFSET 2",
    )
    .into_iter()
    .map(|row| match row.values[0] {
        Value::Int(value) => value,
        ref other => panic!("expected INT id, got {other:?}"),
    })
    .collect();
    assert_eq!(ordered_limited, vec![13, 12, 11, 10, 9]);

    let count_rows = query_rows(
        &coordinator,
        &session,
        "SELECT COUNT(*) FROM sharded_remote_insert",
    );
    assert_eq!(count_rows.len(), 1);
    assert_eq!(count_rows[0].values, vec![Value::BigInt(16)]);

    let aggregate_rows = query_rows(
        &coordinator,
        &session,
        "SELECT SUM(id), MIN(id), MAX(id) FROM sharded_remote_insert",
    );
    assert_eq!(aggregate_rows.len(), 1);
    assert_eq!(
        aggregate_rows[0].values,
        vec![Value::Int(120), Value::Int(0), Value::Int(15)]
    );

    let mut grouped_rows = query_rows(
        &coordinator,
        &session,
        "SELECT id % 4, COUNT(*), SUM(id), MIN(id), MAX(id) \
         FROM sharded_remote_insert GROUP BY id % 4",
    );
    grouped_rows.sort_by(|left, right| {
        let left_key = match left.values[0] {
            Value::Int(value) => value,
            ref other => panic!("expected INT group key, got {other:?}"),
        };
        let right_key = match right.values[0] {
            Value::Int(value) => value,
            ref other => panic!("expected INT group key, got {other:?}"),
        };
        left_key.cmp(&right_key)
    });
    assert_eq!(
        grouped_rows
            .into_iter()
            .map(|row| row.values)
            .collect::<Vec<_>>(),
        vec![
            vec![
                Value::Int(0),
                Value::BigInt(4),
                Value::Int(24),
                Value::Int(0),
                Value::Int(12)
            ],
            vec![
                Value::Int(1),
                Value::BigInt(4),
                Value::Int(28),
                Value::Int(1),
                Value::Int(13)
            ],
            vec![
                Value::Int(2),
                Value::BigInt(4),
                Value::Int(32),
                Value::Int(2),
                Value::Int(14)
            ],
            vec![
                Value::Int(3),
                Value::BigInt(4),
                Value::Int(36),
                Value::Int(3),
                Value::Int(15)
            ],
        ]
    );

    let ordered_grouped_rows = query_rows(
        &coordinator,
        &session,
        "SELECT id % 4, COUNT(*), SUM(id) \
         FROM sharded_remote_insert GROUP BY id % 4 \
         ORDER BY SUM(id) DESC LIMIT 2 OFFSET 1",
    );
    assert_eq!(
        ordered_grouped_rows
            .into_iter()
            .map(|row| row.values)
            .collect::<Vec<_>>(),
        vec![
            vec![Value::Int(2), Value::BigInt(4), Value::Int(32)],
            vec![Value::Int(1), Value::BigInt(4), Value::Int(28)],
        ]
    );

    let avg_rows = query_rows(
        &coordinator,
        &session,
        "SELECT AVG(id) FROM sharded_remote_insert",
    );
    assert_eq!(avg_rows.len(), 1);
    let Value::Numeric(avg_value) = &avg_rows[0].values[0] else {
        panic!("expected NUMERIC AVG, got {:?}", avg_rows[0].values[0]);
    };
    assert_eq!(avg_value.to_string(), "7.5000000000000000");

    let mut grouped_avg_rows = query_rows(
        &coordinator,
        &session,
        "SELECT id % 4, AVG(id) FROM sharded_remote_insert GROUP BY id % 4",
    );
    grouped_avg_rows.sort_by(|left, right| {
        let left_key = match left.values[0] {
            Value::Int(value) => value,
            ref other => panic!("expected INT group key, got {other:?}"),
        };
        let right_key = match right.values[0] {
            Value::Int(value) => value,
            ref other => panic!("expected INT group key, got {other:?}"),
        };
        left_key.cmp(&right_key)
    });
    let grouped_avg_values = grouped_avg_rows
        .into_iter()
        .map(|row| {
            let Value::Int(group_key) = row.values[0] else {
                panic!("expected INT group key, got {:?}", row.values[0]);
            };
            let Value::Numeric(avg_value) = &row.values[1] else {
                panic!("expected NUMERIC AVG, got {:?}", row.values[1]);
            };
            (group_key, avg_value.to_string())
        })
        .collect::<Vec<_>>();
    assert_eq!(
        grouped_avg_values,
        vec![
            (0, "6.0000000000000000".to_owned()),
            (1, "7.0000000000000000".to_owned()),
            (2, "8.0000000000000000".to_owned()),
            (3, "9.0000000000000000".to_owned()),
        ]
    );

    let unsupported = coordinator
        .execute_sql(
            &session,
            "SELECT AVG(DISTINCT id) FROM sharded_remote_insert",
        )
        .expect_err("unsupported remote sharded aggregate should not execute locally");
    assert_eq!(
        unsupported.sqlstate(),
        aiondb_core::SqlState::FeatureNotSupported
    );
    assert!(
        unsupported.to_string().contains(
            "remote sharded aggregate currently supports only simple grouped or ungrouped"
        ),
        "unexpected error: {unsupported}"
    );

    coordinator
        .execute_sql(
            &session,
            "CREATE TABLE sharded_insert_select_src (id INT); \
             INSERT INTO sharded_insert_select_src VALUES (70), (71), (72), (73); \
             INSERT INTO sharded_remote_insert SELECT id FROM sharded_insert_select_src",
        )
        .expect("remote sharded insert select from coordinator-local source");
    let insert_select_rows = query_rows(
        &coordinator,
        &session,
        "SELECT id FROM sharded_remote_insert WHERE id >= 70 ORDER BY id",
    );
    assert_eq!(
        insert_select_rows
            .iter()
            .map(|row| row.values.clone())
            .collect::<Vec<_>>(),
        vec![
            vec![Value::Int(70)],
            vec![Value::Int(71)],
            vec![Value::Int(72)],
            vec![Value::Int(73)],
        ]
    );
    let insert_select_count = query_rows(
        &coordinator,
        &session,
        "SELECT COUNT(*) FROM sharded_remote_insert",
    );
    assert_eq!(insert_select_count[0].values, vec![Value::BigInt(20)]);
    coordinator
        .execute_sql(&session, "DROP TABLE sharded_insert_select_src")
        .expect("drop sharded insert select source");

    coordinator
        .execute_sql(
            &session,
            "CREATE TABLE sharded_insert_select_returning_src (id INT); \
             INSERT INTO sharded_insert_select_returning_src VALUES (80), (81), (82), (83)",
        )
        .expect("create sharded insert select returning source");
    let insert_select_returning = coordinator
        .execute_sql(
            &session,
            "INSERT INTO sharded_remote_insert \
             SELECT id FROM sharded_insert_select_returning_src RETURNING id",
        )
        .expect("remote sharded insert select returning");
    let [StatementResult::Query {
        rows: insert_select_returning_rows,
        ..
    }] = insert_select_returning.as_slice()
    else {
        panic!("expected INSERT SELECT RETURNING query, got {insert_select_returning:?}");
    };
    assert_eq!(
        insert_select_returning_rows
            .iter()
            .map(|row| row.values.clone())
            .collect::<Vec<_>>(),
        vec![
            vec![Value::Int(80)],
            vec![Value::Int(81)],
            vec![Value::Int(82)],
            vec![Value::Int(83)],
        ]
    );
    let insert_select_returning_count = query_rows(
        &coordinator,
        &session,
        "SELECT COUNT(*) FROM sharded_remote_insert",
    );
    assert_eq!(
        insert_select_returning_count[0].values,
        vec![Value::BigInt(24)]
    );
    coordinator
        .execute_sql(&session, "DROP TABLE sharded_insert_select_returning_src")
        .expect("drop sharded insert select returning source");

    coordinator
        .execute_sql(
            &session,
            "CREATE TABLE sharded_insert_select_expr_src (id INT); \
             INSERT INTO sharded_insert_select_expr_src VALUES (90), (91), (92), (93)",
        )
        .expect("create sharded insert select expression source");
    let insert_select_expr_returning = coordinator
        .execute_sql(
            &session,
            "INSERT INTO sharded_remote_insert \
             SELECT id + 20 FROM sharded_insert_select_expr_src RETURNING id",
        )
        .expect("remote sharded insert select expression returning");
    let [StatementResult::Query {
        rows: insert_select_expr_rows,
        ..
    }] = insert_select_expr_returning.as_slice()
    else {
        panic!("expected INSERT SELECT expression RETURNING query, got {insert_select_expr_returning:?}");
    };
    assert_eq!(
        insert_select_expr_rows
            .iter()
            .map(|row| row.values.clone())
            .collect::<Vec<_>>(),
        vec![
            vec![Value::Int(110)],
            vec![Value::Int(111)],
            vec![Value::Int(112)],
            vec![Value::Int(113)],
        ]
    );
    let insert_select_expr_count = query_rows(
        &coordinator,
        &session,
        "SELECT COUNT(*) FROM sharded_remote_insert",
    );
    assert_eq!(insert_select_expr_count[0].values, vec![Value::BigInt(28)]);
    coordinator
        .execute_sql(&session, "DROP TABLE sharded_insert_select_expr_src")
        .expect("drop sharded insert select expression source");

    let count_after_rejected_dml = query_rows(
        &coordinator,
        &session,
        "SELECT COUNT(*) FROM sharded_remote_insert",
    );
    assert_eq!(count_after_rejected_dml[0].values, vec![Value::BigInt(28)]);

    let copy_in = coordinator
        .execute_sql(&session, "COPY sharded_remote_insert FROM STDIN")
        .expect("COPY FROM setup should still describe the target");
    let [StatementResult::CopyIn { table_id, .. }] = copy_in.as_slice() else {
        panic!("expected CopyIn result, got {copy_in:?}");
    };
    let copy_from_result = coordinator
        .execute_copy_from(&session, *table_id, "100\n")
        .expect("remote sharded COPY FROM data should route rows");
    assert_eq!(
        copy_from_result,
        StatementResult::Command {
            tag: "COPY".to_owned(),
            rows_affected: 1
        }
    );
    let count_after_copy_from = query_rows(
        &coordinator,
        &session,
        "SELECT COUNT(*) FROM sharded_remote_insert",
    );
    assert_eq!(count_after_copy_from[0].values, vec![Value::BigInt(29)]);

    {
        let sql = "ALTER TABLE sharded_remote_insert ADD COLUMN extra INT";
        let err = coordinator
            .execute_sql(&session, sql)
            .expect_err("unsupported remote sharded DDL should not update only local catalog");
        assert_eq!(err.sqlstate(), aiondb_core::SqlState::FeatureNotSupported);
        assert!(
            err.to_string()
                .contains("refusing to update only the local catalog"),
            "unexpected error for {sql}: {err}"
        );
        let _ = coordinator.execute_sql(&session, "ROLLBACK");
    }
    let create_index_err = coordinator
        .execute_sql(
            &session,
            "CREATE INDEX sharded_remote_insert_id_idx ON sharded_remote_insert(id)",
        )
        .expect_err("unsupported remote sharded CREATE INDEX should not update only local catalog");
    assert_eq!(
        create_index_err.sqlstate(),
        aiondb_core::SqlState::FeatureNotSupported
    );
    assert!(
        create_index_err
            .to_string()
            .contains("refusing to update only the local catalog"),
        "unexpected CREATE INDEX error: {create_index_err}"
    );
    let _ = coordinator.execute_sql(&session, "ROLLBACK");

    let analyze_err = coordinator
        .execute_sql(&session, "ANALYZE sharded_remote_insert")
        .expect_err("remote sharded ANALYZE should not record local-only stats");
    assert_eq!(
        analyze_err.sqlstate(),
        aiondb_core::SqlState::FeatureNotSupported
    );
    assert!(
        analyze_err
            .to_string()
            .contains("statistics for only local shards"),
        "unexpected ANALYZE error: {analyze_err}"
    );
    let _ = coordinator.execute_sql(&session, "ROLLBACK");

    let lock_err = coordinator
        .execute_sql(
            &session,
            "LOCK TABLE sharded_remote_insert IN ACCESS SHARE MODE",
        )
        .expect_err("remote sharded LOCK should not claim distributed locks");
    assert_eq!(
        lock_err.sqlstate(),
        aiondb_core::SqlState::FeatureNotSupported
    );
    assert!(
        lock_err.to_string().contains("remote internal locks"),
        "unexpected LOCK error: {lock_err}"
    );
    let _ = coordinator.execute_sql(&session, "ROLLBACK");

    coordinator
        .execute_sql(&session, "VACUUM sharded_remote_insert")
        .expect("remote sharded VACUUM should run on all shard leaders");

    let count_after_rejected_ddl = query_rows(
        &coordinator,
        &session,
        "SELECT COUNT(*) FROM sharded_remote_insert",
    );
    assert_eq!(count_after_rejected_ddl[0].values, vec![Value::BigInt(29)]);

    coordinator
        .execute_sql(&session, "TRUNCATE sharded_remote_insert")
        .expect("remote sharded truncate should clear all shard leaders");
    let count_after_truncate = query_rows(
        &coordinator,
        &session,
        "SELECT COUNT(*) FROM sharded_remote_insert",
    );
    assert_eq!(count_after_truncate[0].values, vec![Value::BigInt(0)]);
    let remote_count_after_truncate = query_rows(
        remote_engine.as_ref(),
        &remote_session,
        "SELECT COUNT(*) FROM sharded_remote_insert",
    );
    assert_eq!(
        remote_count_after_truncate[0].values,
        vec![Value::BigInt(0)]
    );

    coordinator
        .execute_sql(&session, "DROP TABLE sharded_remote_insert")
        .expect("remote sharded drop should remove all shard leaders");
    assert!(coordinator
        .distributed_control_plane()
        .table_shards(aiondb_cluster::DatabaseId::DEFAULT, table.table_id)
        .expect("control-plane read after drop")
        .is_empty());
    assert_eq!(
        coordinator
            .distributed_control_plane_snapshot()
            .expect("control-plane snapshot after drop")
            .total_shards,
        0
    );
    coordinator
        .execute_sql(&session, "SELECT COUNT(*) FROM sharded_remote_insert")
        .expect_err("coordinator table should be dropped");
    remote_engine
        .execute_sql(
            &remote_session,
            "SELECT COUNT(*) FROM sharded_remote_insert",
        )
        .expect_err("remote table should be dropped");
}

#[test]
fn distributed_plan_shard_fragments_scan_each_local_storage_shard_once() {
    let mut runtime = RuntimeConfig::default();
    runtime.security = SecurityConfig::from_profile(SecurityProfile::Development);
    runtime.security.allow_ephemeral_users = true;
    runtime.distributed.sharding.enabled = true;
    let data_dir =
        crate::test_support::unique_temp_path("engine-tests-distributed", "sharded-scan-once");
    let builder = EngineBuilder::new_with_config(data_dir, runtime)
        .expect("sharded builder")
        .with_authorizer(Arc::new(AllowAllAuthorizer))
        .with_allow_ephemeral_users(true);
    let engine = builder.build().expect("sharded engine");
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE sharded_scan_once (id INT) \
             WITH (shard_key = 'id', shard_count = 2); \
             INSERT INTO sharded_scan_once VALUES (0),(1),(2),(3),(4),(5),(6),(7)",
        )
        .expect("setup sharded table");

    let table = engine
        .catalog_reader
        .get_table(
            aiondb_core::TxnId::default(),
            &aiondb_catalog::QualifiedName::parse("sharded_scan_once"),
        )
        .expect("catalog read")
        .expect("created table");
    let output_fields = vec![ResultField {
        name: "id".to_owned(),
        data_type: DataType::Int,
        text_type_modifier: None,
        nullable: true,
    }];
    let output = ProjectionExpr {
        field: output_fields[0].clone(),
        expr: TypedExpr::column_ref("id", 0, DataType::Int, true),
    };
    let make_scan = || PhysicalPlan::ProjectTable {
        table_id: table.table_id,
        outputs: vec![output.clone()],
        filter: None,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
        access_path: ScanAccessPath::SeqScan,
    };
    let root_fragment_id = aiondb_cluster::FragmentId::new(0);
    let shard0_fragment_id = aiondb_cluster::FragmentId::new(1);
    let shard1_fragment_id = aiondb_cluster::FragmentId::new(2);
    let shard0 = aiondb_cluster::ShardId::new(0);
    let shard1 = aiondb_cluster::ShardId::new(1);
    let plan = DistributedPhysicalPlan::new(
        None,
        Default::default(),
        Default::default(),
        Default::default(),
        root_fragment_id,
        vec![
            PlanFragment::new(
                root_fragment_id,
                FragmentTarget::Coordinator,
                FragmentPlacement::Local,
                None,
                PhysicalPlan::ProjectValues {
                    output_fields: output_fields.clone(),
                    rows: Vec::new(),
                    order_by: Vec::new(),
                    limit: None,
                    offset: None,
                },
            ),
            PlanFragment::new(
                shard0_fragment_id,
                FragmentTarget::ShardLeader { shard_id: shard0 },
                FragmentPlacement::Shard { shard_id: shard0 },
                None,
                make_scan(),
            ),
            PlanFragment::new(
                shard1_fragment_id,
                FragmentTarget::ShardLeader { shard_id: shard1 },
                FragmentPlacement::Shard { shard_id: shard1 },
                None,
                make_scan(),
            ),
        ],
        vec![
            FragmentEdge::new(shard0_fragment_id, root_fragment_id, ExchangeKind::Gather),
            FragmentEdge::new(shard1_fragment_id, root_fragment_id, ExchangeKind::Gather),
        ],
    );

    let (result, _) = engine
        .execute_distributed_plan(&session, &plan, None, 0)
        .expect("distributed sharded scan");
    let StatementResult::Query { rows, .. } = result else {
        panic!("expected query result, got {result:?}");
    };
    let mut values: Vec<i32> = rows
        .into_iter()
        .map(|row| match &row.values[0] {
            Value::Int(value) => *value,
            other => panic!("expected int, got {other:?}"),
        })
        .collect();
    values.sort_unstable();

    assert_eq!(values, vec![0, 1, 2, 3, 4, 5, 6, 7]);
}

#[test]
fn simple_project_table_on_local_sharded_table_builds_distributed_scan_plan() {
    let mut runtime = RuntimeConfig::default();
    runtime.security = SecurityConfig::from_profile(SecurityProfile::Development);
    runtime.security.allow_ephemeral_users = true;
    runtime.distributed.sharding.enabled = true;
    let data_dir =
        crate::test_support::unique_temp_path("engine-tests-distributed", "sharded-auto-plan");
    let builder = EngineBuilder::new_with_config(data_dir, runtime)
        .expect("sharded builder")
        .with_authorizer(Arc::new(AllowAllAuthorizer))
        .with_allow_ephemeral_users(true);
    let engine = builder.build().expect("sharded engine");
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE sharded_auto_plan (id INT) \
             WITH (shard_key = 'id', shard_count = 2); \
             INSERT INTO sharded_auto_plan VALUES (0),(1),(2),(3),(4),(5)",
        )
        .expect("setup sharded table");

    let table = engine
        .catalog_reader
        .get_table(
            aiondb_core::TxnId::default(),
            &aiondb_catalog::QualifiedName::parse("sharded_auto_plan"),
        )
        .expect("catalog read")
        .expect("created table");
    let output_field = ResultField {
        name: "id".to_owned(),
        data_type: DataType::Int,
        text_type_modifier: None,
        nullable: true,
    };
    let physical_plan = PhysicalPlan::ProjectTable {
        table_id: table.table_id,
        outputs: vec![ProjectionExpr {
            field: output_field,
            expr: TypedExpr::column_ref("id", 0, DataType::Int, true),
        }],
        filter: None,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
        access_path: ScanAccessPath::SeqScan,
    };
    let distributed_plan = engine
        .try_build_local_sharded_scan_plan(
            &physical_plan,
            aiondb_core::TxnId::default(),
            aiondb_cluster::DatabaseId::DEFAULT,
        )
        .expect("auto distributed plan")
        .expect("sharded project table should produce distributed plan");

    distributed_plan.validate().expect("valid distributed plan");
    assert_eq!(distributed_plan.root_fragment_id.get(), 0);
    assert_eq!(distributed_plan.fragments.len(), 3);
    assert_eq!(distributed_plan.edges.len(), 2);
    assert_eq!(
        distributed_plan
            .shard_leader_node(aiondb_cluster::ShardId::new(0))
            .map(aiondb_cluster::NodeId::as_str),
        Some("local")
    );
    assert_eq!(
        distributed_plan
            .shard_leader_node(aiondb_cluster::ShardId::new(1))
            .map(aiondb_cluster::NodeId::as_str),
        Some("local")
    );
    let shard_ids: Vec<_> = distributed_plan
        .fragments
        .iter()
        .filter_map(|fragment| fragment.shard_id().map(|shard_id| shard_id.get()))
        .collect();
    assert_eq!(shard_ids, vec![0, 1]);

    let mut values: Vec<i32> = query_rows(&engine, &session, "SELECT id FROM sharded_auto_plan")
        .into_iter()
        .map(|row| match row.values[0] {
            Value::Int(value) => value,
            ref other => panic!("expected INT id, got {other:?}"),
        })
        .collect();
    values.sort_unstable();
    assert_eq!(values, vec![0, 1, 2, 3, 4, 5]);
}

#[test]
fn distributed_query_with_single_worker_is_local() {
    let engine = build_distributed_engine(1);
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE local_t (id INT); \
             INSERT INTO local_t VALUES (1),(2),(3),(4),(5)",
        )
        .expect("setup");

    let rows = query_rows(&engine, &session, "SELECT id FROM local_t ORDER BY id");
    assert_eq!(rows.len(), 5);
    assert_eq!(rows[0].values[0], Value::Int(1));
    assert_eq!(rows[4].values[0], Value::Int(5));
}

#[test]
fn distributed_query_handles_empty_table() {
    let engine = build_distributed_engine(2);
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE empty_dist (id INT, val TEXT)")
        .expect("create");

    let rows = query_rows(&engine, &session, "SELECT id, val FROM empty_dist");
    assert_eq!(rows.len(), 0, "empty table should return 0 rows");
}

#[test]
fn distributed_query_timeout_propagation() {
    let engine = build_distributed_engine(2);
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE timeout_t (id INT); \
             INSERT INTO timeout_t VALUES (1),(2),(3)",
        )
        .expect("setup");

    // Set a very short statement timeout (1 ms).
    engine
        .execute_sql(&session, "SET statement_timeout = '1ms'")
        .expect("set timeout");

    // Execute a query.  With a 1 ms timeout it might succeed (if fast
    // enough) or fail with a timeout error; either outcome is valid.
    // The key assertion is that the engine does **not** hang.
    let result = engine.execute_sql(&session, "SELECT id FROM timeout_t ORDER BY id");
    match result {
        Ok(results) => {
            // Query completed within the timeout; verify correctness.
            let rows = match results.last().expect("result") {
                StatementResult::Query { rows, .. } => rows,
                other => panic!("expected Query, got {other:?}"),
            };
            assert_eq!(rows.len(), 3);
        }
        Err(err) => {
            // Timeout is the expected failure mode.
            let msg = err.to_string().to_lowercase();
            assert!(
                msg.contains("timeout") || msg.contains("cancel"),
                "unexpected error: {err}"
            );
        }
    }
}

#[test]
fn distributed_query_with_filter() {
    let engine = build_distributed_engine(3);
    let (session, _) = engine.startup(startup_params()).expect("startup");

    // Insert 20 rows (id 1..=20).
    let insert_values: String = (1..=20)
        .map(|i| format!("({i})"))
        .collect::<Vec<_>>()
        .join(",");
    engine
        .execute_sql(
            &session,
            &format!(
                "CREATE TABLE filter_dist (id INT); \
                 INSERT INTO filter_dist VALUES {insert_values}"
            ),
        )
        .expect("setup");

    let rows = query_rows(
        &engine,
        &session,
        "SELECT id FROM filter_dist WHERE id > 10 ORDER BY id",
    );
    assert_eq!(rows.len(), 10, "ids 11..=20");
    assert_eq!(rows[0].values[0], Value::Int(11));
    assert_eq!(rows[9].values[0], Value::Int(20));
}

#[test]
fn distributed_aggregate_count() {
    let engine = build_distributed_engine(2);
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let insert_values: String = (1..=15)
        .map(|i| format!("({i})"))
        .collect::<Vec<_>>()
        .join(",");
    engine
        .execute_sql(
            &session,
            &format!(
                "CREATE TABLE agg_dist (id INT); \
                 INSERT INTO agg_dist VALUES {insert_values}"
            ),
        )
        .expect("setup");

    let rows = query_rows(&engine, &session, "SELECT COUNT(*) FROM agg_dist");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::BigInt(15));
}

#[test]
fn distributed_query_with_order_and_limit() {
    let engine = build_distributed_engine(2);
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let insert_values: String = (1..=20)
        .map(|i| format!("({i})"))
        .collect::<Vec<_>>()
        .join(",");
    engine
        .execute_sql(
            &session,
            &format!(
                "CREATE TABLE ordered_dist (id INT); \
                 INSERT INTO ordered_dist VALUES {insert_values}"
            ),
        )
        .expect("setup");

    let rows = query_rows(
        &engine,
        &session,
        "SELECT id FROM ordered_dist ORDER BY id LIMIT 5",
    );
    assert_eq!(rows.len(), 5);
    for (i, row) in rows.iter().enumerate() {
        let expected = i32::try_from(i + 1).expect("fits i32");
        assert_eq!(row.values[0], Value::Int(expected), "row {i}");
    }
}

#[test]
fn fragment_transport_executor_accepts_debug_isolation_levels() {
    let engine = build_distributed_engine(1);
    let plan = PhysicalPlan::InternalNoOp {
        tag: "SELECT".to_owned(),
        notice: None,
    };

    let result = FragmentExecutor::execute_plan(
        &engine,
        &plan,
        7,
        "SnapshotIsolation",
        None,
        None,
        10_000,
        8 * 1024 * 1024,
        64 * 1024 * 1024,
        256 * 1024 * 1024,
        None,
    )
    .expect("fragment execution should succeed");

    assert_eq!(result, ExecutionResult::command("SELECT"));
}

#[test]
fn fragment_transport_executor_rejects_unknown_isolation_levels() {
    let engine = build_distributed_engine(1);
    let plan = PhysicalPlan::InternalNoOp {
        tag: "SELECT".to_owned(),
        notice: None,
    };

    let error = FragmentExecutor::execute_plan(
        &engine,
        &plan,
        7,
        "unknown-isolation",
        None,
        None,
        10_000,
        8 * 1024 * 1024,
        64 * 1024 * 1024,
        256 * 1024 * 1024,
        None,
    )
    .expect_err("unknown isolation level must fail");

    assert!(error
        .to_string()
        .contains("unsupported remote fragment isolation level"));
}
