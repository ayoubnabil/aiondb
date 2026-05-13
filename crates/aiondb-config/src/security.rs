use std::{path::PathBuf, time::Duration};

/// Preset security profiles for different deployment environments.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SecurityProfile {
    /// Permissive: no TLS, no lockout, no password policy.
    Development,
    /// Moderate: password policy enforced, lockout enabled, TLS optional.
    Staging,
    /// Strict: TLS required, strong passwords, lockout, audit enabled.
    Production,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SecurityConfig {
    /// The security profile that was used to construct (or override) this
    /// config.  Defaults to `Development` so that tests/dev setups remain
    /// permissive unless an operator explicitly selects a stricter profile.
    pub profile: SecurityProfile,
    pub allow_anonymous_local: bool,
    pub max_auth_failures: u32,
    pub auth_lockout_window: Duration,
    pub durable_auth_lockout: bool,
    pub auth_lockout_state_path: Option<PathBuf>,
    pub durable_auth_audit: bool,
    pub auth_audit_log_path: Option<PathBuf>,
    pub auth_audit_max_file_size_bytes: u64,
    pub auth_audit_max_rotated_files: usize,
    pub password_min_length: usize,
    pub reject_role_name_as_password: bool,
    pub password_require_lowercase: bool,
    pub password_require_uppercase: bool,
    pub password_require_digit: bool,
    pub password_require_symbol: bool,
    pub require_tls_for_password: bool,
    /// Allow ephemeral (non-cataloged) users to authenticate.
    /// When `false`, only roles that exist in the catalog may connect.
    /// Defaults to `false`; tests and development helpers must opt in
    /// explicitly when permissive startup is desired.
    pub allow_ephemeral_users: bool,
    /// Maximum idle time before a session is considered expired.
    pub max_session_idle_timeout: Option<Duration>,
    /// Absolute maximum session lifetime regardless of activity.
    pub max_session_lifetime: Option<Duration>,
    /// Maximum number of concurrent sessions per role.
    pub max_concurrent_sessions_per_role: Option<u32>,
    /// Maximum time a transaction may remain open without activity before the
    /// session is forcibly terminated and the transaction rolled back.  Acts
    /// as a safety net for orphaned transactions left behind by abrupt client
    /// disconnections that bypassed normal cleanup.
    pub max_transaction_idle_timeout: Option<Duration>,
    /// Enable DDL audit trail (CREATE/DROP/ALTER logged to audit sink).
    pub ddl_audit_enabled: bool,
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            profile: SecurityProfile::Development,
            allow_anonymous_local: false,
            max_auth_failures: 5,
            auth_lockout_window: Duration::from_secs(60),
            durable_auth_lockout: false,
            auth_lockout_state_path: None,
            durable_auth_audit: false,
            auth_audit_log_path: None,
            auth_audit_max_file_size_bytes: 8 * 1024 * 1024,
            auth_audit_max_rotated_files: 4,
            password_min_length: 8,
            reject_role_name_as_password: true,
            password_require_lowercase: false,
            password_require_uppercase: false,
            password_require_digit: false,
            password_require_symbol: false,
            require_tls_for_password: true,
            allow_ephemeral_users: false,
            max_session_idle_timeout: None,
            max_session_lifetime: None,
            max_concurrent_sessions_per_role: None,
            max_transaction_idle_timeout: None,
            ddl_audit_enabled: false,
        }
    }
}

impl SecurityConfig {
    /// Create a config from a preset security profile.
    pub fn from_profile(profile: SecurityProfile) -> Self {
        match profile {
            SecurityProfile::Development => Self {
                profile,
                allow_anonymous_local: false,
                max_auth_failures: 5,
                auth_lockout_window: Duration::from_secs(60),
                durable_auth_lockout: false,
                auth_lockout_state_path: None,
                durable_auth_audit: false,
                auth_audit_log_path: None,
                auth_audit_max_file_size_bytes: 8 * 1024 * 1024,
                auth_audit_max_rotated_files: 4,
                password_min_length: 0,
                reject_role_name_as_password: false,
                password_require_lowercase: false,
                password_require_uppercase: false,
                password_require_digit: false,
                password_require_symbol: false,
                require_tls_for_password: false,
                allow_ephemeral_users: false,
                max_session_idle_timeout: None,
                max_session_lifetime: None,
                max_concurrent_sessions_per_role: None,
                max_transaction_idle_timeout: None,
                ddl_audit_enabled: false,
            },
            SecurityProfile::Staging => Self {
                profile,
                allow_anonymous_local: false,
                max_auth_failures: 5,
                auth_lockout_window: Duration::from_secs(60 * 5),
                durable_auth_lockout: true,
                auth_lockout_state_path: None,
                durable_auth_audit: false,
                auth_audit_log_path: None,
                auth_audit_max_file_size_bytes: 8 * 1024 * 1024,
                auth_audit_max_rotated_files: 4,
                password_min_length: 8,
                reject_role_name_as_password: true,
                password_require_lowercase: true,
                password_require_uppercase: true,
                password_require_digit: true,
                password_require_symbol: false,
                require_tls_for_password: false,
                allow_ephemeral_users: false,
                max_session_idle_timeout: Some(Duration::from_secs(60 * 30)),
                max_session_lifetime: Some(Duration::from_secs(60 * 60 * 8)),
                max_concurrent_sessions_per_role: None,
                max_transaction_idle_timeout: Some(Duration::from_secs(60 * 15)),
                ddl_audit_enabled: true,
            },
            SecurityProfile::Production => Self {
                profile,
                allow_anonymous_local: false,
                max_auth_failures: 3,
                auth_lockout_window: Duration::from_secs(60 * 15),
                durable_auth_lockout: true,
                auth_lockout_state_path: None,
                durable_auth_audit: true,
                auth_audit_log_path: None,
                auth_audit_max_file_size_bytes: 64 * 1024 * 1024,
                auth_audit_max_rotated_files: 10,
                password_min_length: 12,
                reject_role_name_as_password: true,
                password_require_lowercase: true,
                password_require_uppercase: true,
                password_require_digit: true,
                password_require_symbol: true,
                require_tls_for_password: true,
                allow_ephemeral_users: false,
                max_session_idle_timeout: Some(Duration::from_secs(60 * 15)),
                max_session_lifetime: Some(Duration::from_secs(60 * 60 * 4)),
                max_concurrent_sessions_per_role: Some(50),
                max_transaction_idle_timeout: Some(Duration::from_secs(60 * 10)),
                ddl_audit_enabled: true,
            },
        }
    }

    /// Log warnings via tracing if production-level settings are weak.
    pub fn validate_production_warnings(&self) {
        for issue in self
            .validate_production_requirements()
            .err()
            .unwrap_or_default()
        {
            tracing::warn!("security: {issue}");
        }
    }

    /// Validate a production-like security posture and return all blocking gaps.
    pub fn validate_production_requirements(&self) -> Result<(), Vec<String>> {
        let mut issues = Vec::new();

        if !self.require_tls_for_password {
            issues.push(
                "require_tls_for_password must be enabled so passwords are never accepted over cleartext transport"
                    .to_owned(),
            );
        }
        if self.password_min_length < 12 {
            issues.push(format!(
                "password_min_length must be >= 12 for production-like deployments (got {})",
                self.password_min_length
            ));
        }
        if !self.reject_role_name_as_password {
            issues.push(
                "reject_role_name_as_password must be enabled to block trivial password choices"
                    .to_owned(),
            );
        }
        if !self.password_require_lowercase {
            issues.push("password_require_lowercase must be enabled".to_owned());
        }
        if !self.password_require_uppercase {
            issues.push("password_require_uppercase must be enabled".to_owned());
        }
        if !self.password_require_digit {
            issues.push("password_require_digit must be enabled".to_owned());
        }
        if !self.password_require_symbol {
            issues.push("password_require_symbol must be enabled".to_owned());
        }
        if !self.durable_auth_lockout {
            issues.push(
                "durable_auth_lockout must be enabled so lockout state survives restarts"
                    .to_owned(),
            );
        }
        if !self.durable_auth_audit {
            issues.push(
                "durable_auth_audit must be enabled so authentication events are persisted"
                    .to_owned(),
            );
        }
        if self.allow_ephemeral_users {
            issues.push(
                "allow_ephemeral_users must be disabled for production-like deployments".to_owned(),
            );
        }
        if self.allow_anonymous_local {
            issues.push(
                "allow_anonymous_local must be disabled for production-like deployments".to_owned(),
            );
        }
        if self.max_session_idle_timeout.is_none() {
            issues.push("max_session_idle_timeout must be set so idle sessions expire".to_owned());
        }
        if self.max_session_lifetime.is_none() {
            issues.push(
                "max_session_lifetime must be set so long-lived sessions are capped".to_owned(),
            );
        }
        if self.max_concurrent_sessions_per_role.is_none() {
            issues.push(
                "max_concurrent_sessions_per_role must be set to limit session fan-out".to_owned(),
            );
        }
        if self.max_transaction_idle_timeout.is_none() {
            issues.push(
                "max_transaction_idle_timeout must be set so abandoned transactions are cleaned up"
                    .to_owned(),
            );
        }
        if !self.ddl_audit_enabled {
            issues.push("ddl_audit_enabled must be enabled".to_owned());
        }

        if issues.is_empty() {
            Ok(())
        } else {
            Err(issues)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_allow_anonymous_local_is_false() {
        let cfg = SecurityConfig::default();
        assert!(!cfg.allow_anonymous_local);
    }

    #[test]
    fn default_max_auth_failures_is_5() {
        let cfg = SecurityConfig::default();
        assert_eq!(cfg.max_auth_failures, 5);
    }

    #[test]
    fn default_auth_lockout_window_is_60s() {
        let cfg = SecurityConfig::default();
        assert_eq!(cfg.auth_lockout_window, Duration::from_secs(60));
    }

    #[test]
    fn default_require_tls_for_password_is_true() {
        let cfg = SecurityConfig::default();
        assert!(cfg.require_tls_for_password);
    }

    #[test]
    fn default_durable_auth_lockout_is_false() {
        let cfg = SecurityConfig::default();
        assert!(!cfg.durable_auth_lockout);
    }

    #[test]
    fn default_auth_lockout_state_path_is_none() {
        let cfg = SecurityConfig::default();
        assert_eq!(cfg.auth_lockout_state_path, None);
    }

    #[test]
    fn default_durable_auth_audit_is_false() {
        let cfg = SecurityConfig::default();
        assert!(!cfg.durable_auth_audit);
    }

    #[test]
    fn default_auth_audit_log_path_is_none() {
        let cfg = SecurityConfig::default();
        assert_eq!(cfg.auth_audit_log_path, None);
    }

    #[test]
    fn default_auth_audit_max_file_size_is_8mb() {
        let cfg = SecurityConfig::default();
        assert_eq!(cfg.auth_audit_max_file_size_bytes, 8 * 1024 * 1024);
    }

    #[test]
    fn default_auth_audit_max_rotated_files_is_4() {
        let cfg = SecurityConfig::default();
        assert_eq!(cfg.auth_audit_max_rotated_files, 4);
    }

    #[test]
    fn default_password_min_length_is_8() {
        let cfg = SecurityConfig::default();
        assert_eq!(cfg.password_min_length, 8);
    }

    #[test]
    fn default_reject_role_name_as_password_is_true() {
        let cfg = SecurityConfig::default();
        assert!(cfg.reject_role_name_as_password);
    }

    #[test]
    fn default_password_require_lowercase_is_false() {
        let cfg = SecurityConfig::default();
        assert!(!cfg.password_require_lowercase);
    }

    #[test]
    fn default_password_require_uppercase_is_false() {
        let cfg = SecurityConfig::default();
        assert!(!cfg.password_require_uppercase);
    }

    #[test]
    fn default_password_require_digit_is_false() {
        let cfg = SecurityConfig::default();
        assert!(!cfg.password_require_digit);
    }

    #[test]
    fn default_password_require_symbol_is_false() {
        let cfg = SecurityConfig::default();
        assert!(!cfg.password_require_symbol);
    }

    #[test]
    fn clone_produces_equal_config() {
        let a = SecurityConfig::default();
        let b = a.clone();
        assert_eq!(a, b);
    }

    #[test]
    fn ne_when_allow_anonymous_differs() {
        let mut a = SecurityConfig::default();
        let b = SecurityConfig::default();
        a.allow_anonymous_local = true;
        assert_ne!(a, b);
    }

    #[test]
    fn ne_when_max_auth_failures_differs() {
        let mut a = SecurityConfig::default();
        let b = SecurityConfig::default();
        a.max_auth_failures = 10;
        assert_ne!(a, b);
    }

    #[test]
    fn ne_when_lockout_window_differs() {
        let mut a = SecurityConfig::default();
        let b = SecurityConfig::default();
        a.auth_lockout_window = Duration::from_secs(60 * 2);
        assert_ne!(a, b);
    }

    #[test]
    fn ne_when_require_tls_differs() {
        let mut a = SecurityConfig::default();
        let b = SecurityConfig::default();
        a.require_tls_for_password = false;
        assert_ne!(a, b);
    }

    #[test]
    fn ne_when_durable_auth_lockout_differs() {
        let mut a = SecurityConfig::default();
        let b = SecurityConfig::default();
        a.durable_auth_lockout = true;
        assert_ne!(a, b);
    }

    #[test]
    fn ne_when_auth_lockout_state_path_differs() {
        let mut a = SecurityConfig::default();
        let b = SecurityConfig::default();
        a.auth_lockout_state_path = Some(PathBuf::from("/tmp/aiondb.lockout"));
        assert_ne!(a, b);
    }

    #[test]
    fn ne_when_durable_auth_audit_differs() {
        let mut a = SecurityConfig::default();
        let b = SecurityConfig::default();
        a.durable_auth_audit = true;
        assert_ne!(a, b);
    }

    #[test]
    fn ne_when_auth_audit_log_path_differs() {
        let mut a = SecurityConfig::default();
        let b = SecurityConfig::default();
        a.auth_audit_log_path = Some(PathBuf::from("/tmp/aiondb-audit.log"));
        assert_ne!(a, b);
    }

    #[test]
    fn ne_when_auth_audit_max_file_size_differs() {
        let mut a = SecurityConfig::default();
        let b = SecurityConfig::default();
        a.auth_audit_max_file_size_bytes = 1024;
        assert_ne!(a, b);
    }

    #[test]
    fn ne_when_auth_audit_max_rotated_files_differs() {
        let mut a = SecurityConfig::default();
        let b = SecurityConfig::default();
        a.auth_audit_max_rotated_files = 8;
        assert_ne!(a, b);
    }

    #[test]
    fn ne_when_password_min_length_differs() {
        let mut a = SecurityConfig::default();
        let b = SecurityConfig::default();
        a.password_min_length = 12;
        assert_ne!(a, b);
    }

    #[test]
    fn ne_when_reject_role_name_as_password_differs() {
        let mut a = SecurityConfig::default();
        let b = SecurityConfig::default();
        a.reject_role_name_as_password = false;
        assert_ne!(a, b);
    }

    #[test]
    fn ne_when_password_require_lowercase_differs() {
        let mut a = SecurityConfig::default();
        let b = SecurityConfig::default();
        a.password_require_lowercase = true;
        assert_ne!(a, b);
    }

    #[test]
    fn ne_when_password_require_uppercase_differs() {
        let mut a = SecurityConfig::default();
        let b = SecurityConfig::default();
        a.password_require_uppercase = true;
        assert_ne!(a, b);
    }

    #[test]
    fn ne_when_password_require_digit_differs() {
        let mut a = SecurityConfig::default();
        let b = SecurityConfig::default();
        a.password_require_digit = true;
        assert_ne!(a, b);
    }

    #[test]
    fn ne_when_password_require_symbol_differs() {
        let mut a = SecurityConfig::default();
        let b = SecurityConfig::default();
        a.password_require_symbol = true;
        assert_ne!(a, b);
    }

    #[test]
    fn debug_format_contains_fields() {
        let cfg = SecurityConfig::default();
        let dbg = format!("{cfg:?}");
        assert!(dbg.contains("profile"));
        assert!(dbg.contains("allow_anonymous_local"));
        assert!(dbg.contains("max_auth_failures"));
        assert!(dbg.contains("auth_lockout_window"));
        assert!(dbg.contains("durable_auth_lockout"));
        assert!(dbg.contains("auth_lockout_state_path"));
        assert!(dbg.contains("durable_auth_audit"));
        assert!(dbg.contains("auth_audit_log_path"));
        assert!(dbg.contains("auth_audit_max_file_size_bytes"));
        assert!(dbg.contains("auth_audit_max_rotated_files"));
        assert!(dbg.contains("password_min_length"));
        assert!(dbg.contains("reject_role_name_as_password"));
        assert!(dbg.contains("password_require_lowercase"));
        assert!(dbg.contains("password_require_uppercase"));
        assert!(dbg.contains("password_require_digit"));
        assert!(dbg.contains("password_require_symbol"));
        assert!(dbg.contains("require_tls_for_password"));
        assert!(dbg.contains("allow_ephemeral_users"));
        assert!(dbg.contains("max_session_idle_timeout"));
        assert!(dbg.contains("max_session_lifetime"));
        assert!(dbg.contains("max_concurrent_sessions_per_role"));
        assert!(dbg.contains("max_transaction_idle_timeout"));
        assert!(dbg.contains("ddl_audit_enabled"));
    }

    // ===================================================================
    // NEW EDGE CASE TESTS
    // ===================================================================

    // --- boundary values for max_auth_failures ---

    #[test]
    fn security_zero_max_auth_failures() {
        let mut cfg = SecurityConfig::default();
        cfg.max_auth_failures = 0;
        assert_eq!(cfg.max_auth_failures, 0);
    }

    #[test]
    fn security_u32_max_auth_failures() {
        let mut cfg = SecurityConfig::default();
        cfg.max_auth_failures = u32::MAX;
        assert_eq!(cfg.max_auth_failures, u32::MAX);
    }

    // --- boundary values for auth_lockout_window ---

    #[test]
    fn security_zero_lockout_window() {
        let mut cfg = SecurityConfig::default();
        cfg.auth_lockout_window = Duration::ZERO;
        assert_eq!(cfg.auth_lockout_window, Duration::ZERO);
    }

    #[test]
    fn security_very_large_lockout_window() {
        let mut cfg = SecurityConfig::default();
        cfg.auth_lockout_window = Duration::from_secs(60 * 60 * 8760);
        assert_eq!(cfg.auth_lockout_window.as_secs(), 86400 * 365);
    }

    #[test]
    fn security_sub_millisecond_lockout_window() {
        let mut cfg = SecurityConfig::default();
        cfg.auth_lockout_window = Duration::from_nanos(500);
        assert_eq!(cfg.auth_lockout_window.as_nanos(), 500);
    }

    #[test]
    fn security_password_min_length_boundary_values() {
        let mut cfg = SecurityConfig::default();
        cfg.password_min_length = 0;
        assert_eq!(cfg.password_min_length, 0);
        cfg.password_min_length = 64;
        assert_eq!(cfg.password_min_length, 64);
    }

    #[test]
    fn security_auth_audit_rotation_boundary_values() {
        let mut cfg = SecurityConfig::default();
        cfg.auth_audit_max_file_size_bytes = 1;
        cfg.auth_audit_max_rotated_files = 0;
        assert_eq!(cfg.auth_audit_max_file_size_bytes, 1);
        assert_eq!(cfg.auth_audit_max_rotated_files, 0);
    }

    // --- both booleans true ---

    #[test]
    fn security_both_booleans_true() {
        let cfg = SecurityConfig {
            profile: SecurityProfile::Development,
            allow_anonymous_local: true,
            max_auth_failures: 5,
            auth_lockout_window: Duration::from_secs(60),
            durable_auth_lockout: true,
            auth_lockout_state_path: Some(PathBuf::from("/tmp/aiondb.lockout")),
            durable_auth_audit: true,
            auth_audit_log_path: Some(PathBuf::from("/tmp/aiondb-audit.log")),
            auth_audit_max_file_size_bytes: 4096,
            auth_audit_max_rotated_files: 6,
            password_min_length: 12,
            reject_role_name_as_password: true,
            password_require_lowercase: true,
            password_require_uppercase: true,
            password_require_digit: true,
            password_require_symbol: true,
            require_tls_for_password: true,
            allow_ephemeral_users: true,
            max_session_idle_timeout: Some(Duration::from_secs(60 * 10)),
            max_session_lifetime: Some(Duration::from_secs(60 * 60 * 4)),
            max_concurrent_sessions_per_role: Some(50),
            max_transaction_idle_timeout: Some(Duration::from_secs(60 * 10)),
            ddl_audit_enabled: true,
        };
        assert!(cfg.allow_anonymous_local);
        assert!(cfg.durable_auth_lockout);
        assert!(cfg.durable_auth_audit);
        assert!(cfg.reject_role_name_as_password);
        assert!(cfg.password_require_lowercase);
        assert!(cfg.password_require_uppercase);
        assert!(cfg.password_require_digit);
        assert!(cfg.password_require_symbol);
        assert!(cfg.require_tls_for_password);
        assert!(cfg.ddl_audit_enabled);
    }

    // --- both booleans false ---

    #[test]
    fn security_both_booleans_false() {
        let cfg = SecurityConfig {
            profile: SecurityProfile::Development,
            allow_anonymous_local: false,
            max_auth_failures: 5,
            auth_lockout_window: Duration::from_secs(60),
            durable_auth_lockout: false,
            auth_lockout_state_path: None,
            durable_auth_audit: false,
            auth_audit_log_path: None,
            auth_audit_max_file_size_bytes: 4096,
            auth_audit_max_rotated_files: 0,
            password_min_length: 0,
            reject_role_name_as_password: false,
            password_require_lowercase: false,
            password_require_uppercase: false,
            password_require_digit: false,
            password_require_symbol: false,
            require_tls_for_password: false,
            allow_ephemeral_users: false,
            max_session_idle_timeout: None,
            max_session_lifetime: None,
            max_concurrent_sessions_per_role: None,
            max_transaction_idle_timeout: None,
            ddl_audit_enabled: false,
        };
        assert!(!cfg.allow_anonymous_local);
        assert!(!cfg.durable_auth_lockout);
        assert!(!cfg.durable_auth_audit);
        assert!(!cfg.reject_role_name_as_password);
        assert!(!cfg.password_require_lowercase);
        assert!(!cfg.password_require_uppercase);
        assert!(!cfg.password_require_digit);
        assert!(!cfg.password_require_symbol);
        assert!(!cfg.require_tls_for_password);
        assert!(!cfg.ddl_audit_enabled);
    }

    // --- all fields at maximum/extreme values ---

    #[test]
    fn security_extreme_values() {
        let cfg = SecurityConfig {
            profile: SecurityProfile::Production,
            allow_anonymous_local: true,
            max_auth_failures: u32::MAX,
            auth_lockout_window: Duration::from_secs(u64::MAX / 2),
            durable_auth_lockout: true,
            auth_lockout_state_path: Some(PathBuf::from("/var/lib/aiondb/lockout.state")),
            durable_auth_audit: true,
            auth_audit_log_path: Some(PathBuf::from("/var/lib/aiondb/auth_audit.log")),
            auth_audit_max_file_size_bytes: u64::MAX,
            auth_audit_max_rotated_files: usize::MAX,
            password_min_length: usize::MAX,
            reject_role_name_as_password: true,
            password_require_lowercase: true,
            password_require_uppercase: true,
            password_require_digit: true,
            password_require_symbol: true,
            require_tls_for_password: true,
            allow_ephemeral_users: true,
            max_session_idle_timeout: None,
            max_session_lifetime: None,
            max_concurrent_sessions_per_role: Some(u32::MAX),
            max_transaction_idle_timeout: None,
            ddl_audit_enabled: true,
        };
        assert_eq!(cfg.max_auth_failures, u32::MAX);
        assert!(cfg.auth_lockout_window.as_secs() > 0);
    }

    // --- custom config clone eq ---

    #[test]
    fn security_custom_clone_eq() {
        let cfg = SecurityConfig {
            profile: SecurityProfile::Staging,
            allow_anonymous_local: true,
            max_auth_failures: 100,
            auth_lockout_window: Duration::from_millis(999),
            durable_auth_lockout: true,
            auth_lockout_state_path: Some(PathBuf::from("/tmp/custom state")),
            durable_auth_audit: true,
            auth_audit_log_path: Some(PathBuf::from("/tmp/custom audit.log")),
            auth_audit_max_file_size_bytes: 16 * 1024,
            auth_audit_max_rotated_files: 7,
            password_min_length: 14,
            reject_role_name_as_password: true,
            password_require_lowercase: true,
            password_require_uppercase: true,
            password_require_digit: true,
            password_require_symbol: true,
            require_tls_for_password: true,
            allow_ephemeral_users: true,
            max_session_idle_timeout: Some(Duration::from_secs(60 * 5)),
            max_session_lifetime: Some(Duration::from_secs(60 * 60 * 2)),
            max_concurrent_sessions_per_role: Some(25),
            max_transaction_idle_timeout: Some(Duration::from_secs(60 * 10)),
            ddl_audit_enabled: true,
        };
        let clone = cfg.clone();
        assert_eq!(cfg, clone);
    }

    // --- debug output includes specific default values ---

    #[test]
    fn security_debug_contains_default_values() {
        let cfg = SecurityConfig::default();
        let dbg = format!("{cfg:?}");
        assert!(dbg.contains("true")); // secure defaults include TLS/password policy
        assert!(dbg.contains('5')); // max_auth_failures
        assert!(dbg.contains("password_min_length"));
    }

    // --- all-zero config ---

    #[test]
    fn security_all_minimum_config() {
        let cfg = SecurityConfig {
            profile: SecurityProfile::Development,
            allow_anonymous_local: false,
            max_auth_failures: 0,
            auth_lockout_window: Duration::ZERO,
            durable_auth_lockout: false,
            auth_lockout_state_path: None,
            durable_auth_audit: false,
            auth_audit_log_path: None,
            auth_audit_max_file_size_bytes: 1,
            auth_audit_max_rotated_files: 0,
            password_min_length: 0,
            reject_role_name_as_password: false,
            password_require_lowercase: false,
            password_require_uppercase: false,
            password_require_digit: false,
            password_require_symbol: false,
            require_tls_for_password: false,
            allow_ephemeral_users: false,
            max_session_idle_timeout: None,
            max_session_lifetime: None,
            max_concurrent_sessions_per_role: None,
            max_transaction_idle_timeout: None,
            ddl_audit_enabled: false,
        };
        assert_eq!(cfg.max_auth_failures, 0);
        assert_eq!(cfg.auth_lockout_window, Duration::ZERO);
    }

    // --- SecurityProfile tests ---

    #[test]
    fn profile_development_is_more_permissive_than_default() {
        let cfg = SecurityConfig::from_profile(SecurityProfile::Development);
        assert_eq!(cfg.password_min_length, 0);
        assert!(!cfg.reject_role_name_as_password);
        assert!(!cfg.require_tls_for_password);
    }

    #[test]
    fn profile_staging_has_moderate_settings() {
        let cfg = SecurityConfig::from_profile(SecurityProfile::Staging);
        assert_eq!(cfg.password_min_length, 8);
        assert!(cfg.durable_auth_lockout);
        assert!(cfg.reject_role_name_as_password);
        assert!(cfg.ddl_audit_enabled);
        assert!(cfg.max_session_idle_timeout.is_some());
        assert!(!cfg.require_tls_for_password);
    }

    #[test]
    fn profile_production_has_strict_settings() {
        let cfg = SecurityConfig::from_profile(SecurityProfile::Production);
        assert_eq!(cfg.password_min_length, 12);
        assert!(cfg.require_tls_for_password);
        assert!(cfg.durable_auth_lockout);
        assert!(cfg.durable_auth_audit);
        assert!(cfg.ddl_audit_enabled);
        assert!(cfg.password_require_symbol);
        assert!(cfg.max_concurrent_sessions_per_role.is_some());
        assert!(cfg.max_session_lifetime.is_some());
    }

    #[test]
    fn default_session_policies_are_none() {
        let cfg = SecurityConfig::default();
        assert_eq!(cfg.max_session_idle_timeout, None);
        assert_eq!(cfg.max_session_lifetime, None);
        assert_eq!(cfg.max_concurrent_sessions_per_role, None);
    }

    #[test]
    fn default_ddl_audit_is_disabled() {
        let cfg = SecurityConfig::default();
        assert!(!cfg.ddl_audit_enabled);
    }

    #[test]
    fn default_allow_ephemeral_users_is_false() {
        let cfg = SecurityConfig::default();
        assert!(!cfg.allow_ephemeral_users);
    }

    #[test]
    fn ne_when_allow_ephemeral_users_differs() {
        let mut a = SecurityConfig::default();
        let b = SecurityConfig::default();
        a.allow_ephemeral_users = true;
        assert_ne!(a, b);
    }

    #[test]
    fn profile_staging_rejects_ephemeral_users() {
        let cfg = SecurityConfig::from_profile(SecurityProfile::Staging);
        assert!(!cfg.allow_ephemeral_users);
    }

    #[test]
    fn profile_production_rejects_ephemeral_users() {
        let cfg = SecurityConfig::from_profile(SecurityProfile::Production);
        assert!(!cfg.allow_ephemeral_users);
    }

    #[test]
    fn production_profile_satisfies_production_requirements() {
        let cfg = SecurityConfig::from_profile(SecurityProfile::Production);
        assert_eq!(cfg.validate_production_requirements(), Ok(()));
    }

    #[test]
    fn default_security_config_fails_production_requirements_with_multiple_issues() {
        let cfg = SecurityConfig::default();
        let issues = cfg
            .validate_production_requirements()
            .expect_err("default config should not pass production validation");

        assert!(issues
            .iter()
            .any(|issue| issue.contains("password_min_length")));
        assert!(issues
            .iter()
            .any(|issue| issue.contains("durable_auth_lockout")));
        assert!(issues
            .iter()
            .any(|issue| issue.contains("password_require_symbol")));
        assert!(issues
            .iter()
            .any(|issue| issue.contains("max_session_idle_timeout")));
        assert!(issues
            .iter()
            .any(|issue| issue.contains("ddl_audit_enabled")));
    }

    #[test]
    fn production_requirements_collect_all_missing_controls() {
        let cfg = SecurityConfig {
            require_tls_for_password: false,
            password_min_length: 6,
            reject_role_name_as_password: false,
            password_require_lowercase: false,
            password_require_uppercase: false,
            password_require_digit: false,
            password_require_symbol: false,
            durable_auth_lockout: false,
            durable_auth_audit: false,
            allow_ephemeral_users: true,
            allow_anonymous_local: true,
            max_session_idle_timeout: None,
            max_session_lifetime: None,
            max_concurrent_sessions_per_role: None,
            max_transaction_idle_timeout: None,
            ddl_audit_enabled: false,
            ..SecurityConfig::from_profile(SecurityProfile::Production)
        };

        let issues = cfg
            .validate_production_requirements()
            .expect_err("missing controls should be reported");

        assert!(issues
            .iter()
            .any(|issue| issue.contains("require_tls_for_password")));
        assert!(issues
            .iter()
            .any(|issue| issue.contains("reject_role_name_as_password")));
        assert!(issues
            .iter()
            .any(|issue| issue.contains("durable_auth_lockout")));
        assert!(issues
            .iter()
            .any(|issue| issue.contains("durable_auth_audit")));
        assert!(issues
            .iter()
            .any(|issue| issue.contains("allow_ephemeral_users")));
        assert!(issues
            .iter()
            .any(|issue| issue.contains("allow_anonymous_local")));
        assert!(issues
            .iter()
            .any(|issue| issue.contains("max_session_lifetime")));
        assert!(issues
            .iter()
            .any(|issue| issue.contains("max_transaction_idle_timeout")));
    }
}
