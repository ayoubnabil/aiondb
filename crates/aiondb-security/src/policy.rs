#![allow(clippy::default_trait_access, clippy::no_effect_underscore_binding)]

use aiondb_core::{DatabaseId, DbResult};

use crate::{
    AccessRequest, AuthenticatedIdentity, Authenticator, Authorizer, Credential, TransportInfo,
};

/// Test-only authenticator that trusts whatever credential is presented.
/// Module is `mod policy` (not `pub`), so this type is reachable only from
/// inside the crate's own tests; downstream embedders cannot wire it in.
#[allow(dead_code)]
#[derive(Debug, Default)]
pub struct AllowAllAuthenticator;

impl Authenticator for AllowAllAuthenticator {
    fn authenticate(
        &self,
        credential: &Credential,
        _database: &str,
        _transport: &TransportInfo,
    ) -> DbResult<AuthenticatedIdentity> {
        Ok(AuthenticatedIdentity {
            user: credential.user().to_owned(),
            database_id: DatabaseId::new(0),
            roles: vec![credential.user().to_owned()],
        })
    }
}

#[derive(Debug, Default)]
pub struct AllowAllAuthorizer;

impl Authorizer for AllowAllAuthorizer {
    fn authorize(
        &self,
        _identity: &AuthenticatedIdentity,
        _request: &AccessRequest,
    ) -> DbResult<()> {
        Ok(())
    }

    fn is_noop(&self) -> bool {
        true
    }
}

/// An authorizer that denies **all** operations.
///
/// This is the default authorizer for the engine builder so that production
/// deployments are secure-by-default. Test code should explicitly opt in to
/// [`AllowAllAuthorizer`] via the testing builder path.
#[derive(Debug, Default)]
pub struct DenyAllAuthorizer;

impl Authorizer for DenyAllAuthorizer {
    fn authorize(&self, identity: &AuthenticatedIdentity, request: &AccessRequest) -> DbResult<()> {
        Err(aiondb_core::DbError::insufficient_privilege(format!(
            "permission denied for user \"{}\" (action {:?}): \
             no authorizer configured — use an explicit Authorizer",
            identity.user, request.action,
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AccessTarget, Action, SecretBytes, SecretString};
    use aiondb_core::RelationId;

    fn transport() -> TransportInfo {
        TransportInfo::in_process()
    }

    fn make_identity(user: &str) -> AuthenticatedIdentity {
        AuthenticatedIdentity {
            user: user.to_string(),
            database_id: DatabaseId::new(0),
            roles: vec![user.to_string()],
        }
    }

    // --- AllowAllAuthenticator authenticates any user (anonymous) ---
    #[test]
    fn allow_all_authenticator_authenticates_anonymous() {
        let auth = AllowAllAuthenticator;
        let cred = Credential::Anonymous {
            user: "alice".to_string(),
        };
        let result = auth.authenticate(&cred, "mydb", &transport());
        assert!(result.is_ok());
        let id = result.unwrap();
        assert_eq!(id.user, "alice");
    }

    // --- AllowAllAuthenticator returns DatabaseId(0) ---
    #[test]
    fn allow_all_authenticator_returns_database_id_zero() {
        let auth = AllowAllAuthenticator;
        let cred = Credential::CleartextPassword {
            user: "bob".to_string(),
            password: SecretString::new("pass".to_string()),
        };
        let id = auth.authenticate(&cred, "anydb", &transport()).unwrap();
        assert_eq!(id.database_id, DatabaseId::new(0));
    }

    // --- AllowAllAuthenticator puts user in their own role ---
    #[test]
    fn allow_all_authenticator_puts_user_in_own_role() {
        let auth = AllowAllAuthenticator;
        let cred = Credential::Token {
            user: "charlie".to_string(),
            token: SecretBytes::new(vec![1, 2, 3]),
        };
        let id = auth.authenticate(&cred, "db", &transport()).unwrap();
        assert_eq!(id.roles, vec!["charlie".to_string()]);
    }

    // --- AllowAllAuthorizer authorizes any request ---
    #[test]
    fn allow_all_authorizer_authorizes_any_request() {
        let authz = AllowAllAuthorizer;
        let id = make_identity("user");
        let req = AccessRequest {
            action: Action::Select,
            target: Some(AccessTarget::Relation(RelationId::new(1))),
        };
        assert!(authz.authorize(&id, &req).is_ok());
    }

    // --- AllowAllAuthorizer works with all Action variants ---
    #[test]
    fn allow_all_authorizer_works_with_all_action_variants() {
        let authz = AllowAllAuthorizer;
        let id = make_identity("admin");
        let actions = [
            Action::Connect,
            Action::Select,
            Action::Insert,
            Action::Update,
            Action::Delete,
            Action::Create,
            Action::Drop,
            Action::Alter,
            Action::Execute,
            Action::Usage,
        ];
        for action in actions {
            let req = AccessRequest {
                action,
                target: None,
            };
            assert!(
                authz.authorize(&id, &req).is_ok(),
                "AllowAllAuthorizer should authorize action {action:?}"
            );
        }
    }

    // ===================================================================
    // NEW EDGE CASE TESTS
    // ===================================================================

    // --- AllowAllAuthenticator: with empty user ---

    #[test]
    fn allow_all_authenticator_empty_user() {
        let auth = AllowAllAuthenticator;
        let cred = Credential::Anonymous {
            user: String::new(),
        };
        let id = auth.authenticate(&cred, "db", &transport()).unwrap();
        assert_eq!(id.user, "");
        assert_eq!(id.roles, vec![String::new()]);
    }

    // --- AllowAllAuthenticator: user with special characters ---

    #[test]
    fn allow_all_authenticator_special_chars_user() {
        let auth = AllowAllAuthenticator;
        let cred = Credential::Anonymous {
            user: "user@domain/path#fragment".to_string(),
        };
        let id = auth.authenticate(&cred, "db", &transport()).unwrap();
        assert_eq!(id.user, "user@domain/path#fragment");
        assert_eq!(id.roles[0], "user@domain/path#fragment");
    }

    // --- AllowAllAuthenticator: user with unicode ---

    #[test]
    fn allow_all_authenticator_unicode_user() {
        let auth = AllowAllAuthenticator;
        let cred = Credential::Anonymous {
            user: "日本語ユーザー".to_string(),
        };
        let id = auth.authenticate(&cred, "db", &transport()).unwrap();
        assert_eq!(id.user, "日本語ユーザー");
    }

    // --- AllowAllAuthenticator: database name is ignored ---

    #[test]
    fn allow_all_authenticator_ignores_database_name() {
        let auth = AllowAllAuthenticator;
        let cred = Credential::Anonymous {
            user: "user".to_string(),
        };
        let id1 = auth.authenticate(&cred, "db1", &transport()).unwrap();
        let cred2 = Credential::Anonymous {
            user: "user".to_string(),
        };
        let id2 = auth
            .authenticate(&cred2, "totally_different_db", &transport())
            .unwrap();
        // Both return DatabaseId(0) regardless of database name
        assert_eq!(id1.database_id, id2.database_id);
        assert_eq!(id1.database_id, DatabaseId::new(0));
    }

    // --- AllowAllAuthenticator: transport is ignored ---

    #[test]
    fn allow_all_authenticator_ignores_transport() {
        let auth = AllowAllAuthenticator;
        let cred = Credential::Anonymous {
            user: "user".to_string(),
        };
        let network_transport = TransportInfo {
            kind: crate::TransportKind::Network {
                tls: true,
                peer_addr: Some("10.0.0.1:1234".to_string()),
            },
        };
        let id = auth.authenticate(&cred, "db", &network_transport).unwrap();
        assert_eq!(id.user, "user");
    }

    // --- AllowAllAuthenticator: with CleartextPassword (password is ignored) ---

    #[test]
    fn allow_all_authenticator_cleartext_password_ignored() {
        let auth = AllowAllAuthenticator;
        let cred = Credential::CleartextPassword {
            user: "user".to_string(),
            password: SecretString::new("wrong_password".to_string()),
        };
        let id = auth.authenticate(&cred, "db", &transport()).unwrap();
        assert_eq!(id.user, "user");
    }

    // --- AllowAllAuthenticator: with Token (token is ignored) ---

    #[test]
    fn allow_all_authenticator_token_ignored() {
        let auth = AllowAllAuthenticator;
        let cred = Credential::Token {
            user: "token_user".to_string(),
            token: SecretBytes::new(vec![0xFF; 1024]),
        };
        let id = auth.authenticate(&cred, "db", &transport()).unwrap();
        assert_eq!(id.user, "token_user");
    }

    // --- AllowAllAuthenticator: very long user ---

    #[test]
    fn allow_all_authenticator_very_long_user() {
        let auth = AllowAllAuthenticator;
        let long_user = "u".repeat(10_000);
        let cred = Credential::Anonymous {
            user: long_user.clone(),
        };
        let id = auth.authenticate(&cred, "db", &transport()).unwrap();
        assert_eq!(id.user.len(), 10_000);
    }

    // --- AllowAllAuthenticator: Debug format ---

    #[test]
    fn allow_all_authenticator_debug() {
        let auth = AllowAllAuthenticator;
        let dbg = format!("{auth:?}");
        assert!(dbg.contains("AllowAllAuthenticator"));
    }

    // --- AllowAllAuthenticator: Default trait ---

    #[test]
    fn allow_all_authenticator_default() {
        let _auth = AllowAllAuthenticator;
    }

    // --- AllowAllAuthorizer: with various AccessTarget types ---

    #[test]
    fn allow_all_authorizer_database_target() {
        let authz = AllowAllAuthorizer;
        let id = make_identity("user");
        let req = AccessRequest {
            action: Action::Connect,
            target: Some(AccessTarget::Database(DatabaseId::new(42))),
        };
        assert!(authz.authorize(&id, &req).is_ok());
    }

    #[test]
    fn allow_all_authorizer_index_target() {
        let authz = AllowAllAuthorizer;
        let id = make_identity("user");
        let req = AccessRequest {
            action: Action::Select,
            target: Some(AccessTarget::Index(aiondb_core::IndexId::new(99))),
        };
        assert!(authz.authorize(&id, &req).is_ok());
    }

    #[test]
    fn allow_all_authorizer_sequence_target() {
        let authz = AllowAllAuthorizer;
        let id = make_identity("user");
        let req = AccessRequest {
            action: Action::Usage,
            target: Some(AccessTarget::Sequence(aiondb_core::SequenceId::new(7))),
        };
        assert!(authz.authorize(&id, &req).is_ok());
    }

    #[test]
    fn allow_all_authorizer_schema_target() {
        let authz = AllowAllAuthorizer;
        let id = make_identity("user");
        let req = AccessRequest {
            action: Action::Create,
            target: Some(AccessTarget::Schema(aiondb_core::SchemaId::new(1))),
        };
        assert!(authz.authorize(&id, &req).is_ok());
    }

    // --- AllowAllAuthorizer: Debug format ---

    #[test]
    fn allow_all_authorizer_debug() {
        let authz = AllowAllAuthorizer;
        let dbg = format!("{authz:?}");
        assert!(dbg.contains("AllowAllAuthorizer"));
    }

    // --- AllowAllAuthorizer: Default trait ---

    #[test]
    fn allow_all_authorizer_default() {
        let _authz = AllowAllAuthorizer;
    }

    // --- AllowAllAuthorizer: empty user identity ---

    #[test]
    fn allow_all_authorizer_empty_user() {
        let authz = AllowAllAuthorizer;
        let id = make_identity("");
        let req = AccessRequest {
            action: Action::Select,
            target: None,
        };
        assert!(authz.authorize(&id, &req).is_ok());
    }

    // --- AllowAllAuthorizer: identity with no roles ---

    #[test]
    fn allow_all_authorizer_no_roles() {
        let authz = AllowAllAuthorizer;
        let id = AuthenticatedIdentity {
            user: "noroles".to_string(),
            database_id: DatabaseId::new(0),
            roles: vec![],
        };
        let req = AccessRequest {
            action: Action::Drop,
            target: None,
        };
        assert!(authz.authorize(&id, &req).is_ok());
    }

    // --- AllowAllAuthorizer: called many times in sequence ---

    #[test]
    fn allow_all_authorizer_repeated_calls() {
        let authz = AllowAllAuthorizer;
        let id = make_identity("user");
        for _ in 0..100 {
            let req = AccessRequest {
                action: Action::Select,
                target: None,
            };
            assert!(authz.authorize(&id, &req).is_ok());
        }
    }

    // --- AllowAllAuthenticator: called many times in sequence ---

    #[test]
    fn allow_all_authenticator_repeated_calls() {
        let auth = AllowAllAuthenticator;
        for i in 0..100 {
            let user = format!("user_{i}");
            let cred = Credential::Anonymous { user: user.clone() };
            let id = auth.authenticate(&cred, "db", &transport()).unwrap();
            assert_eq!(id.user, user);
        }
    }

    // ===================================================================
    // DenyAllAuthorizer TESTS
    // ===================================================================

    // --- DenyAllAuthorizer: denies any request ---

    #[test]
    fn deny_all_authorizer_denies_any_request() {
        let authz = DenyAllAuthorizer;
        let id = make_identity("user");
        let req = AccessRequest {
            action: Action::Select,
            target: Some(AccessTarget::Relation(RelationId::new(1))),
        };
        assert!(authz.authorize(&id, &req).is_err());
    }

    // --- DenyAllAuthorizer: denies all Action variants ---

    #[test]
    fn deny_all_authorizer_denies_all_action_variants() {
        let authz = DenyAllAuthorizer;
        let id = make_identity("admin");
        let actions = [
            Action::Connect,
            Action::Select,
            Action::Insert,
            Action::Update,
            Action::Delete,
            Action::Create,
            Action::Drop,
            Action::Alter,
            Action::Execute,
            Action::Usage,
        ];
        for action in actions {
            let req = AccessRequest {
                action,
                target: None,
            };
            assert!(
                authz.authorize(&id, &req).is_err(),
                "DenyAllAuthorizer should deny action {action:?}"
            );
        }
    }

    // --- DenyAllAuthorizer: error is InsufficientPrivilege ---

    #[test]
    fn deny_all_authorizer_returns_insufficient_privilege() {
        let authz = DenyAllAuthorizer;
        let id = make_identity("alice");
        let req = AccessRequest {
            action: Action::Select,
            target: None,
        };
        let err = authz.authorize(&id, &req).unwrap_err();
        // The error message should contain the user name and action
        let msg = format!("{err}");
        assert!(msg.contains("alice"), "error should mention user: {msg}");
        assert!(msg.contains("Select"), "error should mention action: {msg}");
    }

    // --- DenyAllAuthorizer: Debug format ---

    #[test]
    fn deny_all_authorizer_debug() {
        let authz = DenyAllAuthorizer;
        let dbg = format!("{authz:?}");
        assert!(dbg.contains("DenyAllAuthorizer"));
    }

    // --- DenyAllAuthorizer: Default trait ---

    #[test]
    fn deny_all_authorizer_default() {
        let _authz: DenyAllAuthorizer = Default::default();
    }
}
