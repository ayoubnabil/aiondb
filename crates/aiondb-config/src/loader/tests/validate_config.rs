#![allow(clippy::unreadable_literal)]

use super::*;

// ===================================================================
// validate_config tests
// ===================================================================

#[test]
fn validate_rejects_page_size_zero() {
    let mut config = RuntimeConfig::default();
    config.storage.page_size = 0;
    assert!(validate_config(&config).is_err());
}

#[test]
fn validate_rejects_page_size_not_power_of_two() {
    let mut config = RuntimeConfig::default();
    config.storage.page_size = 5000;
    assert!(validate_config(&config).is_err());
}

#[test]
fn validate_rejects_page_size_too_small() {
    let mut config = RuntimeConfig::default();
    config.storage.page_size = 256;
    assert!(validate_config(&config).is_err());
}

#[test]
fn validate_rejects_page_size_too_large() {
    let mut config = RuntimeConfig::default();
    config.storage.page_size = 131072;
    assert!(validate_config(&config).is_err());
}

#[test]
fn validate_accepts_page_size_8192() {
    let mut config = RuntimeConfig::default();
    config.storage.page_size = crate::storage::DEFAULT_STORAGE_PAGE_SIZE;
    assert!(validate_config(&config).is_ok());
}

#[test]
fn validate_rejects_page_size_different_from_supported_size() {
    let mut config = RuntimeConfig::default();
    config.storage.page_size = crate::storage::DEFAULT_STORAGE_PAGE_SIZE / 2;
    assert!(validate_config(&config).is_err());
}

#[test]
fn validate_rejects_zero_max_connections() {
    let mut config = RuntimeConfig::default();
    config.pgwire.max_connections = 0;
    assert!(validate_config(&config).is_err());
}

#[test]
fn validate_rejects_partial_tls_identity_config() {
    let mut config = RuntimeConfig::default();
    config.pgwire.tls_cert_path = Some("/tmp/aiondb/server.crt".into());
    config.pgwire.tls_key_path = None;
    assert!(validate_config(&config).is_err());
}

#[test]
fn validate_rejects_require_tls_without_identity_files() {
    let mut config = RuntimeConfig::default();
    config.pgwire.tls_mode = TlsMode::Require;
    config.pgwire.tls_cert_path = None;
    config.pgwire.tls_key_path = None;
    assert!(validate_config(&config).is_err());
}

#[test]
fn validate_rejects_client_ca_without_server_identity() {
    let mut config = RuntimeConfig::default();
    config.pgwire.tls_mode = TlsMode::Prefer;
    config.pgwire.tls_client_ca_path = Some("/tmp/aiondb/ca.pem".into());
    assert!(validate_config(&config).is_err());
}

#[test]
fn validate_rejects_zero_worker_threads() {
    let mut config = RuntimeConfig::default();
    config.pgwire.engine_pool.worker_threads = 0;
    assert!(validate_config(&config).is_err());
}

#[test]
fn validate_rejects_zero_queue_depth() {
    let mut config = RuntimeConfig::default();
    config.pgwire.engine_pool.queue_depth = 0;
    assert!(validate_config(&config).is_err());
}

#[test]
fn validate_rejects_zero_max_portals() {
    let mut config = RuntimeConfig::default();
    config.limits.max_portals = 0;
    assert!(validate_config(&config).is_err());
}

#[test]
fn validate_rejects_zero_max_prepared_statements() {
    let mut config = RuntimeConfig::default();
    config.limits.max_prepared_statements = 0;
    assert!(validate_config(&config).is_err());
}

#[test]
fn validate_rejects_zero_max_auth_failures() {
    let mut config = RuntimeConfig::default();
    config.security.max_auth_failures = 0;
    assert!(validate_config(&config).is_err());
}

#[test]
fn validate_rejects_zero_auth_audit_max_file_size_bytes() {
    let mut config = RuntimeConfig::default();
    config.security.auth_audit_max_file_size_bytes = 0;
    assert!(validate_config(&config).is_err());
}

#[test]
fn validate_rejects_zero_max_concurrent_sessions_per_role() {
    let mut config = RuntimeConfig::default();
    config.security.max_concurrent_sessions_per_role = Some(0);
    assert!(validate_config(&config).is_err());
}

#[test]
fn validate_rejects_zero_max_open_files() {
    let mut config = RuntimeConfig::default();
    config.storage.max_open_files = 0;
    assert!(validate_config(&config).is_err());
}

#[test]
fn validate_rejects_zero_table_pool_frames() {
    let mut config = RuntimeConfig::default();
    config.storage.table_pool_frames = 0;
    assert!(validate_config(&config).is_err());
}

#[test]
fn validate_rejects_zero_snapshot_pool_frames() {
    let mut config = RuntimeConfig::default();
    config.storage.snapshot_pool_frames = 0;
    assert!(validate_config(&config).is_err());
}

#[test]
fn validate_rejects_zero_max_result_rows() {
    let mut config = RuntimeConfig::default();
    config.limits.max_result_rows = 0;
    assert!(validate_config(&config).is_err());
}

#[test]
fn validate_rejects_zero_max_result_bytes() {
    let mut config = RuntimeConfig::default();
    config.limits.max_result_bytes = 0;
    assert!(validate_config(&config).is_err());
}

#[test]
fn validate_rejects_zero_max_parallel_workers_per_query() {
    let mut config = RuntimeConfig::default();
    config.limits.max_parallel_workers_per_query = 0;
    assert!(validate_config(&config).is_err());
}

#[test]
fn validate_rejects_zero_fragment_transport_port() {
    let mut config = RuntimeConfig::default();
    config.distributed.fragment_transport_port = 0;
    assert!(validate_config(&config).is_err());
}

#[test]
fn validate_rejects_duplicate_remote_node_ids() {
    let mut config = RuntimeConfig::default();
    config.distributed.remote_nodes = vec![
        crate::runtime::RemoteNodeConfig {
            node_id: "node-a".to_owned(),
            addr: "127.0.0.1:7543".to_owned(),
        },
        crate::runtime::RemoteNodeConfig {
            node_id: "NODE-A".to_owned(),
            addr: "127.0.0.1:7544".to_owned(),
        },
    ];
    assert!(validate_config(&config).is_err());
}

#[test]
fn validate_rejects_remote_node_without_port() {
    let mut config = RuntimeConfig::default();
    config.distributed.remote_nodes = vec![crate::runtime::RemoteNodeConfig {
        node_id: "node-a".to_owned(),
        addr: "127.0.0.1".to_owned(),
    }];
    assert!(validate_config(&config).is_err());
}

#[test]
fn validate_rejects_blank_inter_node_auth_token_when_set() {
    let mut config = RuntimeConfig::default();
    config.distributed.inter_node_auth_token = Some("   ".to_owned());
    assert!(validate_config(&config).is_err());
}

#[test]
fn validate_rejects_short_inter_node_auth_token_when_set() {
    let mut config = RuntimeConfig::default();
    config.distributed.inter_node_auth_token = Some("too-short".to_owned());
    assert!(validate_config(&config).is_err());
}

#[test]
fn validate_rejects_fragment_transport_without_inter_node_auth_token() {
    let mut config = RuntimeConfig::default();
    config.distributed.fragment_transport_port = 7543;
    assert!(validate_config(&config).is_err());
}

#[test]
fn validate_accepts_fragment_transport_with_inter_node_auth_token() {
    let mut config = RuntimeConfig::default();
    config.distributed.fragment_transport_port = 7543;
    config.distributed.inter_node_auth_token = Some("0123456789abcdef0123456789abcdef".to_owned());
    assert!(validate_config(&config).is_ok());
}

#[test]
fn validate_rejects_ha_auth_token_shorter_than_hmac_floor() {
    let mut config = RuntimeConfig::default();
    config.ha.inter_node_auth_token = Some("too-short".to_owned());
    assert!(validate_config(&config).is_err());
}

#[test]
fn validate_accepts_ha_auth_token_at_hmac_floor() {
    let mut config = RuntimeConfig::default();
    config.ha.inter_node_auth_token = Some("0123456789abcdef0123456789abcdef".to_owned());
    assert!(validate_config(&config).is_ok());
}

#[test]
fn validate_rejects_ha_without_auth_token() {
    let mut config = RuntimeConfig::default();
    config.ha.enabled = true;
    config.ha.node_id = 1;
    config.ha.cluster_nodes = vec!["127.0.0.1:5433".to_owned()];
    assert!(validate_config(&config).is_err());
}

#[test]
fn validate_rejects_traversal_in_distributed_tls_paths() {
    let mut config = RuntimeConfig::default();
    config.distributed.tls_cert_path = Some("../cert.pem".to_owned());
    assert!(validate_config(&config).is_err());
}

#[test]
fn validate_rejects_traversal_in_ha_fencing_token_path() {
    let mut config = RuntimeConfig::default();
    config.ha.fencing_token_path = Some("../fencing.tok".to_owned());
    assert!(validate_config(&config).is_err());
}

#[test]
fn validate_accepts_default_config() {
    let config = RuntimeConfig::default();
    assert!(validate_config(&config).is_ok());
}

#[test]
fn validate_rejects_replica_without_primary_conninfo() {
    let mut config = RuntimeConfig::default();
    config.replication.role = crate::replication::ReplicationRole::Replica;
    config.replication.primary_conninfo = None;
    assert!(validate_config(&config).is_err());
}

#[test]
fn validate_rejects_replica_with_blank_primary_conninfo() {
    let mut config = RuntimeConfig::default();
    config.replication.role = crate::replication::ReplicationRole::Replica;
    config.replication.primary_conninfo = Some("   ".to_owned());
    let error = validate_config(&config).expect_err("blank conninfo must fail");

    assert!(error.to_string().contains("must be non-empty"));
}

#[test]
fn validate_rejects_primary_without_wal_senders() {
    let mut config = RuntimeConfig::default();
    config.replication.role = crate::replication::ReplicationRole::Primary;
    config.replication.max_wal_senders = 0;
    assert!(validate_config(&config).is_err());
}

#[test]
fn validate_accepts_primary_replication_config() {
    let mut config = RuntimeConfig::default();
    config.replication.role = crate::replication::ReplicationRole::Primary;
    config.replication.max_wal_senders = 8;
    config.replication.wal_keep_segments = 32;
    assert!(validate_config(&config).is_ok());
}

#[test]
fn validate_rejects_factor_write_concern_without_replica_capacity() {
    let mut config = RuntimeConfig::default();
    config.replication.replication_factor = 1;
    config.replication.write_concern = crate::replication::WriteConcern::Factor(1);
    let error = validate_config(&config).expect_err("factor:1 needs at least one replica");

    assert!(error
        .to_string()
        .contains("factor:1 requires more replica acks"));
}

#[test]
fn validate_rejects_zero_factor_write_concern() {
    let mut config = RuntimeConfig::default();
    config.replication.write_concern = crate::replication::WriteConcern::Factor(0);
    let error = validate_config(&config).expect_err("factor:0 must fail");

    assert!(error.to_string().contains("factor:0 is invalid"));
}

#[test]
fn validate_accepts_factor_write_concern_with_replica_capacity() {
    let mut config = RuntimeConfig::default();
    config.replication.replication_factor = 2;
    config.replication.write_concern = crate::replication::WriteConcern::Factor(1);

    assert!(validate_config(&config).is_ok());
}

#[test]
fn validate_rejects_zero_replication_intervals() {
    let mut config = RuntimeConfig::default();
    config.replication.status_interval = std::time::Duration::ZERO;
    let error = validate_config(&config).expect_err("zero status interval must fail");
    assert!(error
        .to_string()
        .contains("AIONDB_REPLICATION_STATUS_INTERVAL_MS"));

    let mut config = RuntimeConfig::default();
    config.replication.sync_commit_timeout = std::time::Duration::ZERO;
    let error = validate_config(&config).expect_err("zero sync timeout must fail");
    assert!(error
        .to_string()
        .contains("AIONDB_REPLICATION_SYNC_COMMIT_TIMEOUT_MS"));
}

#[test]
fn load_from_map_upgrades_legacy_synchronous_commit_without_write_concern() {
    let mut entries = std::collections::HashMap::new();
    entries.insert(
        "AIONDB_REPLICATION_SYNCHRONOUS_COMMIT".to_owned(),
        "true".to_owned(),
    );

    let config = load_from_map(entries).expect("load legacy sync commit config");

    assert_eq!(
        config.replication.write_concern,
        crate::replication::WriteConcern::Majority
    );
}

#[test]
fn load_from_map_preserves_explicit_local_write_concern() {
    let mut entries = std::collections::HashMap::new();
    entries.insert(
        "AIONDB_REPLICATION_SYNCHRONOUS_COMMIT".to_owned(),
        "true".to_owned(),
    );
    entries.insert(
        "AIONDB_REPLICATION_WRITE_CONCERN".to_owned(),
        "local".to_owned(),
    );

    let config = load_from_map(entries).expect("load explicit local write concern");

    assert_eq!(
        config.replication.write_concern,
        crate::replication::WriteConcern::Local
    );
}

#[test]
fn validate_rejects_excessive_default_shard_count() {
    let mut config = RuntimeConfig::default();
    config.distributed.sharding.enabled = true;
    config.distributed.sharding.default_shard_count = aiondb_shard::MAX_STORAGE_SHARD_COUNT + 1;

    let error = validate_config(&config).expect_err("excessive shard count must fail");

    assert!(error
        .to_string()
        .contains("AIONDB_SHARDING_DEFAULT_SHARD_COUNT"));
}

#[test]
fn validate_rejects_excessive_virtual_node_count() {
    let mut config = RuntimeConfig::default();
    config.distributed.sharding.enabled = true;
    config.distributed.sharding.virtual_nodes_per_shard =
        aiondb_shard::MAX_STORAGE_VIRTUAL_NODES_PER_SHARD + 1;

    let error = validate_config(&config).expect_err("excessive virtual node fanout must fail");

    assert!(error
        .to_string()
        .contains("AIONDB_SHARDING_VIRTUAL_NODES_PER_SHARD"));
}

#[test]
fn validate_rejects_excessive_hash_ring_size() {
    let mut config = RuntimeConfig::default();
    config.distributed.sharding.enabled = true;
    config.distributed.sharding.default_shard_count = aiondb_shard::MAX_STORAGE_SHARD_COUNT;
    config.distributed.sharding.virtual_nodes_per_shard = 128;

    let error = validate_config(&config).expect_err("excessive hash ring size must fail");

    assert!(error.to_string().contains("hash-ring points"));
}

#[test]
fn validate_rejects_zero_sharding_leadership_min_load_delta() {
    let mut config = RuntimeConfig::default();
    config.distributed.sharding.enabled = true;
    config.distributed.sharding.leadership_min_load_delta = 0;

    let error = validate_config(&config).expect_err("zero min load delta must fail");

    assert!(error
        .to_string()
        .contains("AIONDB_SHARDING_LEADERSHIP_MIN_LOAD_DELTA"));
}

// ===================================================================
// strict mode tests
// ===================================================================

#[test]
fn strict_mode_rejects_unknown_aiondb_key() {
    let mut entries = std::collections::HashMap::new();
    entries.insert("AIONDB_CONFIG_STRICT".to_owned(), "true".to_owned());
    entries.insert("AIONDB_NONEXISTENT_SETTING".to_owned(), "value".to_owned());
    let result = load_from_map(entries);
    assert!(result.is_err());
    let msg = result.unwrap_err().to_string();
    assert!(msg.contains("unknown configuration key"));
}

#[test]
fn permissive_mode_ignores_unknown_aiondb_key() {
    let mut entries = std::collections::HashMap::new();
    entries.insert("AIONDB_NONEXISTENT_SETTING".to_owned(), "value".to_owned());
    assert!(load_from_map(entries).is_ok());
}

#[test]
fn load_from_map_sets_sharding_learner_throttles() {
    let mut entries = std::collections::HashMap::new();
    entries.insert(
        "AIONDB_SHARDING_MAX_LEARNERS_PER_SHARD".to_owned(),
        "2".to_owned(),
    );
    entries.insert(
        "AIONDB_SHARDING_MAX_LEARNERS_PER_NODE".to_owned(),
        "16".to_owned(),
    );

    let config = load_from_map(entries).expect("load sharding learner throttles");

    assert_eq!(config.distributed.sharding.max_learners_per_shard, 2);
    assert_eq!(config.distributed.sharding.max_learners_per_node, 16);
}

#[test]
fn load_from_map_sets_sharding_leadership_balance_limits() {
    let mut entries = std::collections::HashMap::new();
    entries.insert(
        "AIONDB_SHARDING_LEADERSHIP_MAX_TRANSFERS_PER_MAINTENANCE".to_owned(),
        "5".to_owned(),
    );
    entries.insert(
        "AIONDB_SHARDING_LEADERSHIP_MIN_LOAD_DELTA".to_owned(),
        "3".to_owned(),
    );

    let config = load_from_map(entries).expect("load sharding leadership balance limits");

    assert_eq!(
        config
            .distributed
            .sharding
            .leadership_max_transfers_per_maintenance,
        5
    );
    assert_eq!(config.distributed.sharding.leadership_min_load_delta, 3);
}

#[test]
fn load_from_map_sets_sharding_placement_policy() {
    let mut entries = std::collections::HashMap::new();
    entries.insert(
        "AIONDB_SHARDING_NODE_ATTRIBUTES".to_owned(),
        "local:region=eu-west;zone=az-a,node-b:region=eu-north;zone=az-b".to_owned(),
    );
    entries.insert(
        "AIONDB_SHARDING_PLACEMENT_REQUIRED_ATTRIBUTES".to_owned(),
        "disk=ssd".to_owned(),
    );
    entries.insert(
        "AIONDB_SHARDING_LEASE_PREFERENCE_ATTRIBUTES".to_owned(),
        "region=eu-west".to_owned(),
    );
    entries.insert(
        "AIONDB_SHARDING_PLACEMENT_SPREAD_ATTRIBUTES".to_owned(),
        "region,zone".to_owned(),
    );

    let config = load_from_map(entries).expect("load sharding placement policy");

    assert_eq!(
        config.distributed.sharding.node_attributes["local"]["region"],
        "eu-west"
    );
    assert_eq!(
        config.distributed.sharding.placement_required_attributes[0].key,
        "disk"
    );
    assert_eq!(
        config.distributed.sharding.lease_preference_attributes[0].value,
        "eu-west"
    );
    assert_eq!(
        config.distributed.sharding.placement_spread_attributes,
        vec!["region".to_owned(), "zone".to_owned()]
    );
}

#[test]
fn strict_mode_ignores_non_aiondb_keys() {
    let mut entries = std::collections::HashMap::new();
    entries.insert("AIONDB_CONFIG_STRICT".to_owned(), "true".to_owned());
    entries.insert("PATH".to_owned(), "/usr/bin".to_owned());
    entries.insert("HOME".to_owned(), "/root".to_owned());
    assert!(load_from_map(entries).is_ok());
}

#[test]
fn strict_mode_accepts_server_runtime_only_keys() {
    let accepted_keys = [
        ("AIONDB_IN_MEMORY", "true"),
        ("AIONDB_ALLOW_UNENCRYPTED_STORAGE", "true"),
        ("AIONDB_OBSERVABILITY_BIND", "127.0.0.1"),
        ("AIONDB_OBSERVABILITY_PORT", "9187"),
        ("AIONDB_OBSERVABILITY_FAIL_FAST", "false"),
        ("AIONDB_DISTRIBUTED_FRAGMENT_TRANSPORT_FAIL_FAST", "false"),
        ("AIONDB_ALLOW_PUBLIC_OBSERVABILITY", "false"),
        ("AIONDB_DISABLE_MEMORY_GUARD", "false"),
        ("AIONDB_ENGINE_DISABLE_PARSED_SQL_FINGERPRINT_CACHE", "1"),
        ("AIONDB_REPLICATION_PROMOTE_ON_START", "false"),
        ("AIONDB_PGWIRE_COPY_IN_MAX_BUFFER", "8388608"),
        ("AIONDB_PGWIRE_COPY_IN_TOTAL_TIMEOUT_MS", "900000"),
        ("AIONDB_STORAGE_MAX_SNAPSHOT_BYTES", "536870912"),
    ];

    for (key, value) in accepted_keys {
        let mut entries = std::collections::HashMap::new();
        entries.insert("AIONDB_CONFIG_STRICT".to_owned(), "true".to_owned());
        entries.insert(key.to_owned(), value.to_owned());
        assert!(
            load_from_map(entries).is_ok(),
            "strict mode should accept server/runtime-only key {key}",
        );
    }
}

// ===================================================================
// listen_addr validation tests
// ===================================================================

#[test]
fn validate_rejects_invalid_listen_addr() {
    let mut config = RuntimeConfig::default();
    config.pgwire.listen_addr = "not-a-valid-address".to_owned();
    assert!(validate_config(&config).is_err());
}

#[test]
fn validate_accepts_valid_listen_addr() {
    let mut config = RuntimeConfig::default();
    config.pgwire.listen_addr = "127.0.0.1:5432".to_owned();
    assert!(validate_config(&config).is_ok());
}

#[test]
fn validate_accepts_ipv6_listen_addr() {
    let mut config = RuntimeConfig::default();
    config.pgwire.listen_addr = "[::1]:5432".to_owned();
    assert!(validate_config(&config).is_ok());
}

#[test]
fn validate_accepts_wildcard_listen_addr() {
    let mut config = RuntimeConfig::default();
    config.pgwire.listen_addr = "0.0.0.0:5432".to_owned();
    assert!(validate_config(&config).is_ok());
}
