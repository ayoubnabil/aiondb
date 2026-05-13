use std::path::PathBuf;
use std::time::Duration;

use crate::runtime::EnginePoolConfig;

pub const DEFAULT_PGWIRE_BIND_ADDRESS: &str = "127.0.0.1";
pub const DEFAULT_PGWIRE_PORT: u16 = 5432;
pub const DEFAULT_PGWIRE_LISTEN_ADDR: &str = "127.0.0.1:5432";

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum TlsMode {
    Disable,
    #[default]
    Prefer,
    Require,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PgWireConfig {
    pub listen_addr: String,
    pub max_connections: u32,
    pub max_connections_per_ip: u32,
    pub startup_timeout: Duration,
    pub auth_failure_backoff: Duration,
    pub idle_timeout: Duration,
    pub tls_mode: TlsMode,
    pub tls_cert_path: Option<PathBuf>,
    pub tls_key_path: Option<PathBuf>,
    pub tls_client_ca_path: Option<PathBuf>,
    pub engine_pool: EnginePoolConfig,
}

impl Default for PgWireConfig {
    fn default() -> Self {
        Self {
            listen_addr: DEFAULT_PGWIRE_LISTEN_ADDR.to_owned(),
            max_connections: 128,
            max_connections_per_ip: 128,
            startup_timeout: Duration::from_secs(5),
            auth_failure_backoff: Duration::from_millis(250),
            idle_timeout: Duration::from_secs(60 * 5),
            tls_mode: TlsMode::Prefer,
            tls_cert_path: None,
            tls_key_path: None,
            tls_client_ca_path: None,
            engine_pool: EnginePoolConfig::default(),
        }
    }
}

impl PgWireConfig {
    #[must_use]
    pub fn bind_address_and_port(&self) -> (String, u16) {
        split_listen_addr(&self.listen_addr)
    }
}

#[must_use]
pub fn split_listen_addr(listen_addr: &str) -> (String, u16) {
    let Some((host, port)) = listen_addr.rsplit_once(':') else {
        return (listen_addr.to_owned(), DEFAULT_PGWIRE_PORT);
    };

    let port = port.parse().unwrap_or(DEFAULT_PGWIRE_PORT);
    (host.to_owned(), port)
}

#[cfg(test)]
mod tests {
    use super::*;

    // -------------------------------------------------------------------
    // PgWireConfig defaults
    // -------------------------------------------------------------------

    #[test]
    fn default_listen_addr() {
        let cfg = PgWireConfig::default();
        assert_eq!(cfg.listen_addr, DEFAULT_PGWIRE_LISTEN_ADDR);
    }

    #[test]
    fn default_max_connections_is_128() {
        let cfg = PgWireConfig::default();
        assert_eq!(cfg.max_connections, 128);
    }

    #[test]
    fn default_max_connections_per_ip_is_128() {
        let cfg = PgWireConfig::default();
        assert_eq!(cfg.max_connections_per_ip, 128);
    }

    #[test]
    fn default_startup_timeout_is_5s() {
        let cfg = PgWireConfig::default();
        assert_eq!(cfg.startup_timeout, Duration::from_secs(5));
    }

    #[test]
    fn default_auth_failure_backoff_is_250ms() {
        let cfg = PgWireConfig::default();
        assert_eq!(cfg.auth_failure_backoff, Duration::from_millis(250));
    }

    #[test]
    fn default_tls_mode_is_prefer() {
        let cfg = PgWireConfig::default();
        assert_eq!(cfg.tls_mode, TlsMode::Prefer);
    }

    #[test]
    fn default_tls_cert_path_is_none() {
        let cfg = PgWireConfig::default();
        assert_eq!(cfg.tls_cert_path, None);
    }

    #[test]
    fn default_tls_key_path_is_none() {
        let cfg = PgWireConfig::default();
        assert_eq!(cfg.tls_key_path, None);
    }

    #[test]
    fn default_tls_client_ca_path_is_none() {
        let cfg = PgWireConfig::default();
        assert_eq!(cfg.tls_client_ca_path, None);
    }

    #[test]
    fn default_engine_pool_matches_engine_pool_default() {
        let cfg = PgWireConfig::default();
        assert_eq!(cfg.engine_pool, EnginePoolConfig::default());
    }

    #[test]
    fn bind_address_and_port_splits_ipv6_listen_addr() {
        let cfg = PgWireConfig {
            listen_addr: "[::1]:15432".to_owned(),
            ..PgWireConfig::default()
        };
        assert_eq!(cfg.bind_address_and_port(), ("[::1]".to_owned(), 15432));
    }

    #[test]
    fn split_listen_addr_uses_default_port_when_missing() {
        assert_eq!(
            split_listen_addr("db.internal"),
            ("db.internal".to_owned(), DEFAULT_PGWIRE_PORT)
        );
    }

    // -------------------------------------------------------------------
    // TlsMode
    // -------------------------------------------------------------------

    #[test]
    fn tls_mode_default_is_prefer() {
        assert_eq!(TlsMode::default(), TlsMode::Prefer);
    }

    #[test]
    fn tls_mode_disable_ne_prefer() {
        assert_ne!(TlsMode::Disable, TlsMode::Prefer);
    }

    #[test]
    fn tls_mode_disable_ne_require() {
        assert_ne!(TlsMode::Disable, TlsMode::Require);
    }

    #[test]
    fn tls_mode_prefer_ne_require() {
        assert_ne!(TlsMode::Prefer, TlsMode::Require);
    }

    #[test]
    fn tls_mode_clone_preserves_value() {
        let a = TlsMode::Require;
        let b = a;
        assert_eq!(a, b);
    }

    #[test]
    fn tls_mode_copy_semantics() {
        let a = TlsMode::Disable;
        let b = a; // Copy
        assert_eq!(a, b);
    }

    #[test]
    fn tls_mode_debug_format_disable() {
        let dbg = format!("{:?}", TlsMode::Disable);
        assert_eq!(dbg, "Disable");
    }

    #[test]
    fn tls_mode_debug_format_prefer() {
        let dbg = format!("{:?}", TlsMode::Prefer);
        assert_eq!(dbg, "Prefer");
    }

    #[test]
    fn tls_mode_debug_format_require() {
        let dbg = format!("{:?}", TlsMode::Require);
        assert_eq!(dbg, "Require");
    }

    // -------------------------------------------------------------------
    // PgWireConfig Clone, Debug, Eq
    // -------------------------------------------------------------------

    #[test]
    fn pgwire_clone_eq() {
        let a = PgWireConfig::default();
        let b = a.clone();
        assert_eq!(a, b);
    }

    #[test]
    fn pgwire_debug_contains_fields() {
        let cfg = PgWireConfig::default();
        let dbg = format!("{cfg:?}");
        assert!(dbg.contains("listen_addr"));
        assert!(dbg.contains("max_connections"));
        assert!(dbg.contains("tls_mode"));
        assert!(dbg.contains("tls_cert_path"));
    }

    #[test]
    fn pgwire_ne_when_listen_addr_differs() {
        let mut a = PgWireConfig::default();
        let b = PgWireConfig::default();
        a.listen_addr = "0.0.0.0:5433".to_owned();
        assert_ne!(a, b);
    }

    #[test]
    fn pgwire_ne_when_tls_mode_differs() {
        let mut a = PgWireConfig::default();
        let b = PgWireConfig::default();
        a.tls_mode = TlsMode::Require;
        assert_ne!(a, b);
    }

    // ===================================================================
    // NEW EDGE CASE TESTS
    // ===================================================================

    // --- PgWireConfig: port boundary values in listen_addr ---

    #[test]
    fn pgwire_listen_addr_port_zero() {
        let mut cfg = PgWireConfig::default();
        cfg.listen_addr = "127.0.0.1:0".to_owned();
        assert_eq!(cfg.listen_addr, "127.0.0.1:0");
    }

    #[test]
    fn pgwire_listen_addr_port_65535() {
        let mut cfg = PgWireConfig::default();
        cfg.listen_addr = "127.0.0.1:65535".to_owned();
        assert_eq!(cfg.listen_addr, "127.0.0.1:65535");
    }

    #[test]
    fn pgwire_listen_addr_ipv6() {
        let mut cfg = PgWireConfig::default();
        cfg.listen_addr = "[::1]:5432".to_owned();
        assert_eq!(cfg.listen_addr, "[::1]:5432");
    }

    #[test]
    fn pgwire_listen_addr_all_interfaces() {
        let mut cfg = PgWireConfig::default();
        cfg.listen_addr = "0.0.0.0:5432".to_owned();
        assert_eq!(cfg.listen_addr, "0.0.0.0:5432");
    }

    #[test]
    fn pgwire_listen_addr_empty_string() {
        let mut cfg = PgWireConfig::default();
        cfg.listen_addr = String::new();
        assert!(cfg.listen_addr.is_empty());
    }

    // --- PgWireConfig: connection boundary values ---

    #[test]
    fn pgwire_max_connections_zero() {
        let mut cfg = PgWireConfig::default();
        cfg.max_connections = 0;
        assert_eq!(cfg.max_connections, 0);
    }

    #[test]
    fn pgwire_max_connections_u32_max() {
        let mut cfg = PgWireConfig::default();
        cfg.max_connections = u32::MAX;
        assert_eq!(cfg.max_connections, u32::MAX);
    }

    #[test]
    fn pgwire_max_connections_per_ip_u32_max() {
        let mut cfg = PgWireConfig::default();
        cfg.max_connections_per_ip = u32::MAX;
        assert_eq!(cfg.max_connections_per_ip, u32::MAX);
    }

    // --- PgWireConfig: timeout boundary values ---

    #[test]
    fn pgwire_startup_timeout_zero() {
        let mut cfg = PgWireConfig::default();
        cfg.startup_timeout = Duration::ZERO;
        assert_eq!(cfg.startup_timeout, Duration::ZERO);
    }

    #[test]
    fn pgwire_startup_timeout_very_large() {
        let mut cfg = PgWireConfig::default();
        cfg.startup_timeout = Duration::from_secs(60 * 60 * 24);
        assert_eq!(cfg.startup_timeout.as_secs(), 86400);
    }

    #[test]
    fn pgwire_auth_failure_backoff_zero() {
        let mut cfg = PgWireConfig::default();
        cfg.auth_failure_backoff = Duration::ZERO;
        assert_eq!(cfg.auth_failure_backoff, Duration::ZERO);
    }

    #[test]
    fn pgwire_auth_failure_backoff_sub_millis() {
        let mut cfg = PgWireConfig::default();
        cfg.auth_failure_backoff = Duration::from_nanos(1);
        assert_eq!(cfg.auth_failure_backoff.as_nanos(), 1);
    }

    // --- TlsMode: self-equality for all variants ---

    #[test]
    fn tls_mode_disable_eq_self() {
        assert_eq!(TlsMode::Disable, TlsMode::Disable);
    }

    #[test]
    fn tls_mode_prefer_eq_self() {
        assert_eq!(TlsMode::Prefer, TlsMode::Prefer);
    }

    #[test]
    fn tls_mode_require_eq_self() {
        assert_eq!(TlsMode::Require, TlsMode::Require);
    }

    // --- TlsMode: all 3 variants are distinct (exhaustive) ---

    #[test]
    fn tls_mode_all_three_distinct() {
        let variants = [TlsMode::Disable, TlsMode::Prefer, TlsMode::Require];
        for i in 0..variants.len() {
            for j in (i + 1)..variants.len() {
                assert_ne!(variants[i], variants[j]);
            }
        }
    }

    // --- PgWireConfig: ne when max_connections_per_ip differs ---

    #[test]
    fn pgwire_ne_when_max_connections_per_ip_differs() {
        let mut a = PgWireConfig::default();
        let b = PgWireConfig::default();
        a.max_connections_per_ip = 1;
        assert_ne!(a, b);
    }

    // --- PgWireConfig: ne when startup_timeout differs ---

    #[test]
    fn pgwire_ne_when_startup_timeout_differs() {
        let mut a = PgWireConfig::default();
        let b = PgWireConfig::default();
        a.startup_timeout = Duration::from_secs(999);
        assert_ne!(a, b);
    }

    // --- PgWireConfig: ne when auth_failure_backoff differs ---

    #[test]
    fn pgwire_ne_when_auth_failure_backoff_differs() {
        let mut a = PgWireConfig::default();
        let b = PgWireConfig::default();
        a.auth_failure_backoff = Duration::from_secs(999);
        assert_ne!(a, b);
    }

    // --- PgWireConfig: ne when engine_pool differs ---

    #[test]
    fn pgwire_ne_when_engine_pool_differs() {
        let mut a = PgWireConfig::default();
        let b = PgWireConfig::default();
        a.engine_pool.worker_threads = 99;
        assert_ne!(a, b);
    }

    // --- PgWireConfig: ne when max_connections differs ---

    #[test]
    fn pgwire_ne_when_max_connections_differs() {
        let mut a = PgWireConfig::default();
        let b = PgWireConfig::default();
        a.max_connections = 1;
        assert_ne!(a, b);
    }

    // --- PgWireConfig debug includes all field values ---

    #[test]
    fn pgwire_debug_contains_listen_addr_value() {
        let cfg = PgWireConfig::default();
        let dbg = format!("{cfg:?}");
        assert!(dbg.contains(DEFAULT_PGWIRE_LISTEN_ADDR));
        assert!(dbg.contains("128")); // max_connections
    }

    // --- PgWireConfig: identical non-default configs compare equal ---

    #[test]
    fn pgwire_custom_configs_equal() {
        let a = PgWireConfig {
            listen_addr: "10.0.0.1:9999".to_owned(),
            max_connections: 1,
            max_connections_per_ip: 1,
            startup_timeout: Duration::from_millis(1),
            auth_failure_backoff: Duration::from_millis(1),
            idle_timeout: Duration::from_secs(60),
            tls_mode: TlsMode::Require,
            tls_cert_path: Some(PathBuf::from("/tmp/server.crt")),
            tls_key_path: Some(PathBuf::from("/tmp/server.key")),
            tls_client_ca_path: Some(PathBuf::from("/tmp/ca.pem")),
            engine_pool: EnginePoolConfig {
                worker_threads: 1,
                queue_depth: 1,
            },
        };
        let b = a.clone();
        assert_eq!(a, b);
    }
}
