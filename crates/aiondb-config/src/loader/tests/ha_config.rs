use super::*;

// ===================================================================
// HA config parsing tests
// ===================================================================

#[test]
fn map_ha_enabled_true() {
    let mut entries = HashMap::new();
    entries.insert("AIONDB_HA_ENABLED".to_owned(), "true".to_owned());
    entries.insert("AIONDB_HA_NODE_ID".to_owned(), "1".to_owned());
    entries.insert(
        "AIONDB_HA_CLUSTER_NODES".to_owned(),
        "a:5433,b:5433".to_owned(),
    );
    entries.insert(
        "AIONDB_HA_AUTH_TOKEN".to_owned(),
        "0123456789abcdef0123456789abcdef".to_owned(),
    );
    let cfg = load_from_map(entries).unwrap();
    assert!(cfg.ha.enabled);
}

#[test]
fn map_ha_enabled_false() {
    let mut entries = HashMap::new();
    entries.insert("AIONDB_HA_ENABLED".to_owned(), "false".to_owned());
    let cfg = load_from_map(entries).unwrap();
    assert!(!cfg.ha.enabled);
}

#[test]
fn map_ha_node_id() {
    let mut entries = HashMap::new();
    entries.insert("AIONDB_HA_NODE_ID".to_owned(), "42".to_owned());
    let cfg = load_from_map(entries).unwrap();
    assert_eq!(cfg.ha.node_id, 42);
}

#[test]
fn map_ha_cluster_nodes() {
    let mut entries = HashMap::new();
    entries.insert(
        "AIONDB_HA_CLUSTER_NODES".to_owned(),
        "a:5433,b:5433".to_owned(),
    );
    let cfg = load_from_map(entries).unwrap();
    assert_eq!(
        cfg.ha.cluster_nodes,
        vec!["a:5433".to_owned(), "b:5433".to_owned()]
    );
}

#[test]
fn map_ha_cluster_nodes_trims_whitespace() {
    let mut entries = HashMap::new();
    entries.insert(
        "AIONDB_HA_CLUSTER_NODES".to_owned(),
        " a:5433 , b:5433 , c:5433 ".to_owned(),
    );
    let cfg = load_from_map(entries).unwrap();
    assert_eq!(
        cfg.ha.cluster_nodes,
        vec![
            "a:5433".to_owned(),
            "b:5433".to_owned(),
            "c:5433".to_owned()
        ]
    );
}

#[test]
fn map_ha_cluster_nodes_filters_empty() {
    let mut entries = HashMap::new();
    entries.insert(
        "AIONDB_HA_CLUSTER_NODES".to_owned(),
        "a:5433,,b:5433,".to_owned(),
    );
    let cfg = load_from_map(entries).unwrap();
    assert_eq!(
        cfg.ha.cluster_nodes,
        vec!["a:5433".to_owned(), "b:5433".to_owned()]
    );
}

#[test]
fn map_ha_health_check_interval_ms() {
    let mut entries = HashMap::new();
    entries.insert(
        "AIONDB_HA_HEALTH_CHECK_INTERVAL_MS".to_owned(),
        "5000".to_owned(),
    );
    let cfg = load_from_map(entries).unwrap();
    assert_eq!(cfg.ha.health_check_interval, Duration::from_secs(5));
}

#[test]
fn map_ha_health_check_timeout_ms() {
    let mut entries = HashMap::new();
    entries.insert(
        "AIONDB_HA_HEALTH_CHECK_TIMEOUT_MS".to_owned(),
        "15000".to_owned(),
    );
    let cfg = load_from_map(entries).unwrap();
    assert_eq!(cfg.ha.health_check_timeout, Duration::from_secs(15));
}

#[test]
fn map_ha_election_timeout_ms() {
    let mut entries = HashMap::new();
    entries.insert(
        "AIONDB_HA_ELECTION_TIMEOUT_MS".to_owned(),
        "20000".to_owned(),
    );
    let cfg = load_from_map(entries).unwrap();
    assert_eq!(cfg.ha.election_timeout, Duration::from_secs(20));
}

#[test]
fn map_ha_max_failover_lag() {
    let mut entries = HashMap::new();
    entries.insert(
        "AIONDB_HA_MAX_FAILOVER_LAG".to_owned(),
        "1048576".to_owned(),
    );
    let cfg = load_from_map(entries).unwrap();
    assert_eq!(cfg.ha.max_failover_lag, 1_048_576);
}

#[test]
fn map_ha_port() {
    let mut entries = HashMap::new();
    entries.insert("AIONDB_HA_PORT".to_owned(), "6000".to_owned());
    let cfg = load_from_map(entries).unwrap();
    assert_eq!(cfg.ha.ha_port, 6000);
}

#[test]
fn map_ha_fencing_token_path() {
    let mut entries = HashMap::new();
    entries.insert(
        "AIONDB_HA_FENCING_TOKEN_PATH".to_owned(),
        "/var/lib/aiondb/fencing.tok".to_owned(),
    );
    let cfg = load_from_map(entries).unwrap();
    assert_eq!(
        cfg.ha.fencing_token_path,
        Some("/var/lib/aiondb/fencing.tok".to_owned())
    );
}

#[test]
fn map_ha_auth_token() {
    let mut entries = HashMap::new();
    entries.insert(
        "AIONDB_HA_AUTH_TOKEN".to_owned(),
        "0123456789abcdef0123456789abcdef".to_owned(),
    );
    let cfg = load_from_map(entries).unwrap();
    assert_eq!(
        cfg.ha.inter_node_auth_token,
        Some("0123456789abcdef0123456789abcdef".to_owned())
    );
}

#[test]
fn map_ha_auth_token_empty_clears_value() {
    let mut entries = HashMap::new();
    entries.insert("AIONDB_HA_AUTH_TOKEN".to_owned(), "   ".to_owned());
    let cfg = load_from_map(entries).unwrap();
    assert_eq!(cfg.ha.inter_node_auth_token, None);
}

// ===================================================================
// HA validation tests
// ===================================================================

#[test]
fn validate_rejects_ha_enabled_without_cluster_nodes() {
    let mut config = RuntimeConfig::default();
    config.ha.enabled = true;
    config.ha.node_id = 1;
    config.ha.cluster_nodes = Vec::new();
    let err = validate_config(&config).unwrap_err();
    assert!(err
        .to_string()
        .contains("AIONDB_HA_CLUSTER_NODES must be set when HA is enabled"));
}

#[test]
fn validate_rejects_ha_enabled_with_zero_node_id() {
    let mut config = RuntimeConfig::default();
    config.ha.enabled = true;
    config.ha.node_id = 0;
    config.ha.cluster_nodes = vec!["a:5433".to_owned()];
    let err = validate_config(&config).unwrap_err();
    assert!(err
        .to_string()
        .contains("AIONDB_HA_NODE_ID must be a non-zero value when HA is enabled"));
}

#[test]
fn validate_rejects_ha_timeout_lte_interval() {
    let mut config = RuntimeConfig::default();
    config.ha.enabled = true;
    config.ha.node_id = 1;
    config.ha.cluster_nodes = vec!["a:5433".to_owned()];
    config.ha.health_check_interval = Duration::from_secs(10);
    config.ha.health_check_timeout = Duration::from_secs(10);
    let err = validate_config(&config).unwrap_err();
    assert!(err.to_string().contains(
        "AIONDB_HA_HEALTH_CHECK_TIMEOUT_MS must be greater than AIONDB_HA_HEALTH_CHECK_INTERVAL_MS"
    ));
}

#[test]
fn validate_rejects_ha_election_timeout_lte_health_check_timeout() {
    let mut config = RuntimeConfig::default();
    config.ha.enabled = true;
    config.ha.node_id = 1;
    config.ha.cluster_nodes = vec!["a:5433".to_owned()];
    config.ha.health_check_interval = Duration::from_secs(3);
    config.ha.health_check_timeout = Duration::from_secs(10);
    config.ha.election_timeout = Duration::from_secs(10);
    let err = validate_config(&config).unwrap_err();
    assert!(err.to_string().contains(
        "AIONDB_HA_ELECTION_TIMEOUT_MS must be greater than AIONDB_HA_HEALTH_CHECK_TIMEOUT_MS"
    ));
}

#[test]
fn validate_accepts_valid_ha_config() {
    let mut config = RuntimeConfig::default();
    config.ha.enabled = true;
    config.ha.node_id = 1;
    config.ha.cluster_nodes = vec!["a:5433".to_owned(), "b:5433".to_owned()];
    config.ha.inter_node_auth_token = Some("0123456789abcdef0123456789abcdef".to_owned());
    config.ha.health_check_interval = Duration::from_secs(3);
    config.ha.health_check_timeout = Duration::from_secs(10);
    config.ha.election_timeout = Duration::from_secs(15);
    assert!(validate_config(&config).is_ok());
}

#[test]
fn validate_skips_ha_checks_when_disabled() {
    let mut config = RuntimeConfig::default();
    config.ha.enabled = false;
    config.ha.node_id = 0;
    config.ha.cluster_nodes = Vec::new();
    assert!(validate_config(&config).is_ok());
}
