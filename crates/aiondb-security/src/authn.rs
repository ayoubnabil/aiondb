#![allow(clippy::missing_errors_doc, clippy::must_use_candidate)]

use aiondb_core::{DatabaseId, DbResult};

use crate::{SecretBytes, SecretString};

#[non_exhaustive]
pub enum Credential {
    Anonymous {
        user: String,
    },
    CleartextPassword {
        user: String,
        password: SecretString,
    },
    Token {
        user: String,
        token: SecretBytes,
    },
}

impl std::fmt::Debug for Credential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Anonymous { user } => f.debug_struct("Credential").field("user", user).finish(),
            Self::CleartextPassword { user, .. } => f
                .debug_struct("Credential::CleartextPassword")
                .field("user", user)
                .field("password", &"**redacted**")
                .finish(),
            Self::Token { user, .. } => f
                .debug_struct("Credential::Token")
                .field("user", user)
                .field("token", &"**redacted**")
                .finish(),
        }
    }
}

impl Credential {
    #[must_use]
    pub fn user(&self) -> &str {
        match self {
            Self::Anonymous { user }
            | Self::CleartextPassword { user, .. }
            | Self::Token { user, .. } => user,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum TransportKind {
    InProcess,
    Network {
        tls: bool,
        peer_addr: Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransportInfo {
    pub kind: TransportKind,
}

impl TransportInfo {
    pub const fn in_process() -> Self {
        Self {
            kind: TransportKind::InProcess,
        }
    }

    #[must_use]
    pub fn tls_enabled(&self) -> bool {
        match &self.kind {
            TransportKind::InProcess => false,
            TransportKind::Network { tls, .. } => *tls,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthenticatedIdentity {
    pub user: String,
    pub database_id: DatabaseId,
    pub roles: Vec<String>,
}

#[allow(clippy::missing_errors_doc)]
pub trait Authenticator: Send + Sync {
    fn authenticate(
        &self,
        credential: &Credential,
        database: &str,
        transport: &TransportInfo,
    ) -> DbResult<AuthenticatedIdentity>;
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Credential::Anonymous user() returns user ---
    #[test]
    fn credential_anonymous_user() {
        let cred = Credential::Anonymous {
            user: "alice".to_string(),
        };
        assert_eq!(cred.user(), "alice");
    }

    // --- Credential::CleartextPassword user() returns user ---
    #[test]
    fn credential_cleartext_password_user() {
        let cred = Credential::CleartextPassword {
            user: "bob".to_string(),
            password: SecretString::new("s3cret".to_string()),
        };
        assert_eq!(cred.user(), "bob");
    }

    // --- Credential::Token user() returns user ---
    #[test]
    fn credential_token_user() {
        let cred = Credential::Token {
            user: "charlie".to_string(),
            token: SecretBytes::new(vec![0xAA, 0xBB]),
        };
        assert_eq!(cred.user(), "charlie");
    }

    // --- Credential Debug does NOT reveal password ---
    #[test]
    fn credential_debug_does_not_reveal_password() {
        let cred = Credential::CleartextPassword {
            user: "bob".to_string(),
            password: SecretString::new("my_password_123".to_string()),
        };
        let debug = format!("{cred:?}");
        assert!(
            debug.contains("**redacted**"),
            "Debug should contain **redacted**, got: {debug}"
        );
        assert!(
            !debug.contains("my_password_123"),
            "Debug must not reveal the password"
        );
    }

    // --- Credential Debug does NOT reveal token ---
    #[test]
    fn credential_debug_does_not_reveal_token() {
        let cred = Credential::Token {
            user: "charlie".to_string(),
            token: SecretBytes::new(vec![0xDE, 0xAD]),
        };
        let debug = format!("{cred:?}");
        assert!(
            debug.contains("**redacted**"),
            "Debug should contain **redacted**, got: {debug}"
        );
    }

    // --- TransportInfo::in_process() tls_enabled() -> false ---
    #[test]
    fn transport_info_in_process_tls_disabled() {
        let ti = TransportInfo::in_process();
        assert!(!ti.tls_enabled());
    }

    // --- TransportInfo Network with tls: true -> tls_enabled() true ---
    #[test]
    fn transport_info_network_tls_true() {
        let ti = TransportInfo {
            kind: TransportKind::Network {
                tls: true,
                peer_addr: Some("127.0.0.1:5432".to_string()),
            },
        };
        assert!(ti.tls_enabled());
    }

    // --- TransportInfo Network with tls: false -> tls_enabled() false ---
    #[test]
    fn transport_info_network_tls_false() {
        let ti = TransportInfo {
            kind: TransportKind::Network {
                tls: false,
                peer_addr: None,
            },
        };
        assert!(!ti.tls_enabled());
    }

    // --- AuthenticatedIdentity fields accessible ---
    #[test]
    fn authenticated_identity_fields_accessible() {
        let id = AuthenticatedIdentity {
            user: "admin".to_string(),
            database_id: DatabaseId::new(5),
            roles: vec!["admin".to_string(), "superuser".to_string()],
        };
        assert_eq!(id.user, "admin");
        assert_eq!(id.database_id, DatabaseId::new(5));
        assert_eq!(id.roles.len(), 2);
        assert_eq!(id.roles[0], "admin");
        assert_eq!(id.roles[1], "superuser");
    }

    // ===================================================================
    // NEW EDGE CASE TESTS
    // ===================================================================

    // --- Credential: user with empty string ---

    #[test]
    fn credential_anonymous_empty_user() {
        let cred = Credential::Anonymous {
            user: String::new(),
        };
        assert_eq!(cred.user(), "");
    }

    #[test]
    fn credential_cleartext_empty_user() {
        let cred = Credential::CleartextPassword {
            user: String::new(),
            password: SecretString::new("pass".to_string()),
        };
        assert_eq!(cred.user(), "");
    }

    #[test]
    fn credential_token_empty_user() {
        let cred = Credential::Token {
            user: String::new(),
            token: SecretBytes::new(vec![1]),
        };
        assert_eq!(cred.user(), "");
    }

    // --- Credential: user with special characters ---

    #[test]
    fn credential_user_with_unicode() {
        let cred = Credential::Anonymous {
            user: "日本語ユーザー".to_string(),
        };
        assert_eq!(cred.user(), "日本語ユーザー");
    }

    #[test]
    fn credential_user_with_special_chars() {
        let cred = Credential::Anonymous {
            user: "user@domain.com".to_string(),
        };
        assert_eq!(cred.user(), "user@domain.com");
    }

    #[test]
    fn credential_user_with_spaces() {
        let cred = Credential::Anonymous {
            user: "user with spaces".to_string(),
        };
        assert_eq!(cred.user(), "user with spaces");
    }

    #[test]
    fn credential_user_very_long() {
        let long_user = "u".repeat(10_000);
        let cred = Credential::Anonymous {
            user: long_user.clone(),
        };
        assert_eq!(cred.user(), long_user.as_str());
    }

    // --- Credential: Debug format for Anonymous ---

    #[test]
    fn credential_debug_anonymous_shows_user() {
        let cred = Credential::Anonymous {
            user: "testuser".to_string(),
        };
        let dbg = format!("{cred:?}");
        assert!(dbg.contains("testuser"));
        assert!(dbg.contains("Credential"));
    }

    // --- Credential: Debug for CleartextPassword shows user but not password ---

    #[test]
    fn credential_debug_cleartext_shows_user_not_password() {
        let cred = Credential::CleartextPassword {
            user: "visible_user".to_string(),
            password: SecretString::new("hidden_pass".to_string()),
        };
        let dbg = format!("{cred:?}");
        assert!(dbg.contains("visible_user"));
        assert!(!dbg.contains("hidden_pass"));
        assert!(dbg.contains("**redacted**"));
    }

    // --- Credential: Debug for Token shows user but not token ---

    #[test]
    fn credential_debug_token_shows_user_not_token() {
        let cred = Credential::Token {
            user: "token_user".to_string(),
            token: SecretBytes::new(vec![0xDE, 0xAD, 0xBE, 0xEF]),
        };
        let dbg = format!("{cred:?}");
        assert!(dbg.contains("token_user"));
        assert!(dbg.contains("**redacted**"));
    }

    // --- Credential: CleartextPassword with empty password ---

    #[test]
    fn credential_cleartext_empty_password() {
        let cred = Credential::CleartextPassword {
            user: "user".to_string(),
            password: SecretString::new(String::new()),
        };
        assert_eq!(cred.user(), "user");
    }

    // --- Credential: Token with empty token ---

    #[test]
    fn credential_token_empty_token() {
        let cred = Credential::Token {
            user: "user".to_string(),
            token: SecretBytes::new(vec![]),
        };
        assert_eq!(cred.user(), "user");
    }

    // --- TransportInfo: clone works ---

    #[test]
    fn transport_info_clone() {
        let ti = TransportInfo {
            kind: TransportKind::Network {
                tls: true,
                peer_addr: Some("10.0.0.1:1234".to_string()),
            },
        };
        let ti2 = ti.clone();
        assert!(ti2.tls_enabled());
    }

    // --- TransportInfo: Network with None peer_addr ---

    #[test]
    fn transport_info_network_no_peer_addr() {
        let ti = TransportInfo {
            kind: TransportKind::Network {
                tls: false,
                peer_addr: None,
            },
        };
        assert!(!ti.tls_enabled());
    }

    // --- TransportInfo: Network with IPv6 peer_addr ---

    #[test]
    fn transport_info_network_ipv6_peer() {
        let ti = TransportInfo {
            kind: TransportKind::Network {
                tls: true,
                peer_addr: Some("[::1]:5432".to_string()),
            },
        };
        assert!(ti.tls_enabled());
    }

    // --- TransportInfo: in_process is const and tls always false ---

    #[test]
    fn transport_info_in_process_is_const() {
        const TI: TransportInfo = TransportInfo::in_process();
        assert!(!TI.tls_enabled());
    }

    // --- TransportInfo: Debug output ---

    #[test]
    fn transport_info_debug_in_process() {
        let ti = TransportInfo::in_process();
        let dbg = format!("{ti:?}");
        assert!(dbg.contains("InProcess"));
    }

    #[test]
    fn transport_info_debug_network() {
        let ti = TransportInfo {
            kind: TransportKind::Network {
                tls: false,
                peer_addr: Some("1.2.3.4:5678".to_string()),
            },
        };
        let dbg = format!("{ti:?}");
        assert!(dbg.contains("Network"));
        assert!(dbg.contains("1.2.3.4:5678"));
    }

    // --- AuthenticatedIdentity: empty roles ---

    #[test]
    fn authenticated_identity_empty_roles() {
        let id = AuthenticatedIdentity {
            user: "user".to_string(),
            database_id: DatabaseId::new(1),
            roles: vec![],
        };
        assert!(id.roles.is_empty());
    }

    // --- AuthenticatedIdentity: database_id zero ---

    #[test]
    fn authenticated_identity_database_id_zero() {
        let id = AuthenticatedIdentity {
            user: "user".to_string(),
            database_id: DatabaseId::new(0),
            roles: vec![],
        };
        assert_eq!(id.database_id, DatabaseId::new(0));
    }

    // --- AuthenticatedIdentity: clone ---

    #[test]
    fn authenticated_identity_clone() {
        let id = AuthenticatedIdentity {
            user: "admin".to_string(),
            database_id: DatabaseId::new(42),
            roles: vec!["role1".to_string(), "role2".to_string()],
        };
        let cloned = id.clone();
        assert_eq!(cloned.user, "admin");
        assert_eq!(cloned.database_id, DatabaseId::new(42));
        assert_eq!(cloned.roles.len(), 2);
    }

    // --- AuthenticatedIdentity: debug output ---

    #[test]
    fn authenticated_identity_debug() {
        let id = AuthenticatedIdentity {
            user: "debuguser".to_string(),
            database_id: DatabaseId::new(1),
            roles: vec!["r1".to_string()],
        };
        let dbg = format!("{id:?}");
        assert!(dbg.contains("debuguser"));
    }

    // --- AuthenticatedIdentity: user with special characters ---

    #[test]
    fn authenticated_identity_unicode_user() {
        let id = AuthenticatedIdentity {
            user: "用户名".to_string(),
            database_id: DatabaseId::new(1),
            roles: vec!["角色".to_string()],
        };
        assert_eq!(id.user, "用户名");
        assert_eq!(id.roles[0], "角色");
    }

    // --- AuthenticatedIdentity: many roles ---

    #[test]
    fn authenticated_identity_many_roles() {
        let roles: Vec<String> = (0..1000).map(|i| format!("role_{i}")).collect();
        let id = AuthenticatedIdentity {
            user: "user".to_string(),
            database_id: DatabaseId::new(1),
            roles: roles.clone(),
        };
        assert_eq!(id.roles.len(), 1000);
        assert_eq!(id.roles[999], "role_999");
    }
}
