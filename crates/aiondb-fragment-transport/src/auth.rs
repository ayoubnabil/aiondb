//! Inter-node authentication for fragment transport.
//!
//! Uses a shared secret token for authenticating fragment execution
//! requests between cluster nodes. Both client and server must be
//! configured with the same token.

use aiondb_core::{DbError, DbResult};
use subtle::ConstantTimeEq;

/// Shared-secret authentication token for inter-node communication.
#[derive(Clone)]
pub struct AuthToken(String);

impl std::fmt::Debug for AuthToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.0.is_empty() {
            f.write_str("AuthToken(<empty>)")
        } else {
            f.write_str("AuthToken(<redacted>)")
        }
    }
}

impl AuthToken {
    /// Create a new auth token from a secret string.
    #[must_use]
    pub fn new(token: impl Into<String>) -> Self {
        Self(token.into())
    }

    /// Returns `true` if the token is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Returns an error if the token is empty.
    ///
    /// Call this at startup to enforce that inter-node auth is configured
    /// when remote nodes are present.
    ///
    /// # Errors
    ///
    /// Returns an authorization error when the configured token is empty.
    pub fn require_non_empty(&self) -> DbResult<()> {
        if self.0.is_empty() {
            return Err(DbError::invalid_authorization(
                "inter-node auth token must not be empty when remote nodes are configured",
            ));
        }
        Ok(())
    }

    /// Return the token as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Validate an incoming token against this expected token.
    ///
    /// Uses constant-time comparison to prevent timing attacks.
    ///
    /// Always returns `false` if the configured (server-side) token is
    /// empty, even if the incoming token is also empty. This prevents a
    /// misconfigured server with no auth token from accepting any peer
    /// (the "empty == empty" auth bypass). Operators must configure a
    /// non-empty token for fragment transport authentication to succeed.
    #[must_use]
    pub fn validate(&self, incoming: &str) -> bool {
        if self.0.is_empty() {
            return false;
        }
        // `subtle::ConstantTimeEq` short-circuits when lengths differ
        // (length is not secret here; both sides know the deployed token
        // length). The byte comparison itself is constant-time over the
        // shared length, which is what blocks the timing oracle.
        self.0.as_bytes().ct_eq(incoming.as_bytes()).unwrap_u8() == 1
    }
}

/// Validate an auth token from an incoming envelope.
///
/// # Errors
///
/// Returns an authorization error when the server token is empty, the
/// incoming token is empty, or the token check fails.
pub fn validate_request_auth(expected: &AuthToken, incoming: &str) -> DbResult<()> {
    if expected.is_empty() {
        return Err(DbError::invalid_authorization(
            "fragment transport: server auth token is not configured",
        ));
    }
    if incoming.is_empty() {
        return Err(DbError::invalid_authorization(
            "fragment transport: client sent empty auth token",
        ));
    }
    if !expected.validate(incoming) {
        return Err(DbError::invalid_authorization(
            "fragment transport: authentication failed \u{2014} invalid inter-node token",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_token_matches() {
        let token = AuthToken::new("secret-key-123");
        assert!(token.validate("secret-key-123"));
    }

    #[test]
    fn invalid_token_rejected() {
        let token = AuthToken::new("secret-key-123");
        assert!(!token.validate("wrong-key"));
    }

    #[test]
    fn empty_token_rejected() {
        let token = AuthToken::new("secret");
        assert!(!token.validate(""));
    }

    #[test]
    fn empty_server_token_never_validates() {
        // Regression: if the server is started with an empty `auth_token`
        // (e.g. operator forgot to configure the secret), `validate`
        // previously returned `true` for an empty incoming token, letting
        // any unauthenticated peer execute fragments.
        let token = AuthToken::new("");
        assert!(!token.validate(""), "empty == empty must not auth-bypass");
        assert!(!token.validate("anything"));
        assert!(!token.validate("\0\0\0"));
    }

    #[test]
    fn validate_request_auth_success() {
        let token = AuthToken::new("my-secret");
        assert!(validate_request_auth(&token, "my-secret").is_ok());
    }

    #[test]
    fn validate_request_auth_failure() {
        let token = AuthToken::new("my-secret");
        assert!(validate_request_auth(&token, "bad").is_err());
    }

    #[test]
    fn is_empty_returns_true_for_empty_token() {
        assert!(AuthToken::new("").is_empty());
    }

    #[test]
    fn is_empty_returns_false_for_nonempty_token() {
        assert!(!AuthToken::new("secret").is_empty());
    }

    #[test]
    fn require_non_empty_rejects_empty() {
        assert!(AuthToken::new("").require_non_empty().is_err());
    }

    #[test]
    fn require_non_empty_accepts_nonempty() {
        assert!(AuthToken::new("secret").require_non_empty().is_ok());
    }

    #[test]
    fn validate_request_rejects_empty_server_token() {
        let token = AuthToken::new("");
        assert!(validate_request_auth(&token, "anything").is_err());
    }

    #[test]
    fn validate_request_rejects_empty_client_token() {
        let token = AuthToken::new("secret");
        assert!(validate_request_auth(&token, "").is_err());
    }

    #[test]
    fn debug_does_not_expose_token() {
        let token = AuthToken::new("super-secret-123");
        let debug = format!("{token:?}");
        assert!(!debug.contains("super-secret-123"));
        assert!(debug.contains("redacted"));
    }

    #[test]
    fn debug_shows_empty_for_empty_token() {
        let token = AuthToken::new("");
        let debug = format!("{token:?}");
        assert!(debug.contains("empty"));
    }
}
