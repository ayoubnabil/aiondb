use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use hmac::{Hmac, Mac};
use sha2::Sha256;
use subtle::ConstantTimeEq;

use aiondb_engine::{Engine, QueryEngine};

type HmacSha256 = Hmac<Sha256>;
const CSRF_CONTEXT_PREFIX: &[u8] = b"csrf:";
const CSRF_FALLBACK_TOKEN: &str = "csrf-signing-unavailable";
#[cfg(test)]
const FORCE_CSRF_SIGN_ERROR_SESSION_ID: &str = "__aiondb_force_csrf_sign_error__";

/// Cryptographically random 32-byte secret generated at startup.
#[derive(Clone)]
pub struct SessionSecret(Arc<[u8; 32]>);

impl SessionSecret {
    pub fn generate() -> Result<Self, getrandom::Error> {
        let mut key = [0u8; 32];
        getrandom_fill(&mut key)?;
        Ok(Self(Arc::new(key)))
    }

    /// Sign a session token to produce a CSRF token.
    ///
    /// Never returns an empty token. On internal signing failure, this returns
    /// a non-empty fallback token that is guaranteed to fail verification.
    pub fn sign_csrf(&self, session_id: &str) -> String {
        self.try_sign_csrf(session_id).unwrap_or_else(|| {
            tracing::error!("HMAC-SHA256 key initialisation failed unexpectedly");
            CSRF_FALLBACK_TOKEN.to_owned()
        })
    }

    /// Verify a CSRF token matches the session.
    pub fn verify_csrf(&self, session_id: &str, token: &str) -> bool {
        if session_id.is_empty() || token.is_empty() {
            return false;
        }

        let Some(expected) = self.try_sign_csrf(session_id) else {
            tracing::error!("CSRF verification failed due to internal signing error");
            return false;
        };

        expected.as_bytes().ct_eq(token.as_bytes()).into()
    }

    fn try_sign_csrf(&self, session_id: &str) -> Option<String> {
        #[cfg(test)]
        if session_id == FORCE_CSRF_SIGN_ERROR_SESSION_ID {
            return None;
        }

        let Ok(mut mac) = HmacSha256::new_from_slice(self.0.as_ref()) else {
            return None;
        };
        mac.update(CSRF_CONTEXT_PREFIX);
        mac.update(session_id.as_bytes());
        let result = mac.finalize();
        Some(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(result.into_bytes()))
    }
}

use base64::Engine as _;

fn getrandom_fill(buf: &mut [u8]) -> Result<(), getrandom::Error> {
    getrandom::fill(buf)
}

/// Active dashboard session.
#[derive(Clone, Debug)]
pub struct DashboardSession {
    pub session_id: String,
    pub username: String,
    pub database: String,
    pub created_at: Instant,
    pub last_activity: Instant,
    pub engine_session: aiondb_engine::SessionHandle,
}

/// Session store with automatic expiration.
pub struct SessionStore {
    sessions: Mutex<HashMap<String, DashboardSession>>,
    max_age: Duration,
    max_sessions: usize,
    engine: Arc<Engine>,
}

impl SessionStore {
    pub fn new(max_age: Duration, max_sessions: usize, engine: Arc<Engine>) -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            max_age,
            max_sessions,
            engine,
        }
    }

    pub fn insert(&self, session: DashboardSession) -> bool {
        let (expired, inserted) = {
            let mut map = self.sessions.lock().unwrap_or_else(|poisoned| {
                tracing::warn!("dashboard session store lock poisoned during insert");
                poisoned.into_inner()
            });
            let expired = Self::evict_expired(&mut map, self.max_age);
            let inserted = if map.len() >= self.max_sessions {
                false
            } else {
                map.insert(session.session_id.clone(), session);
                true
            };
            (expired, inserted)
        };
        self.terminate_sessions(expired);
        inserted
    }

    pub fn get(&self, session_id: &str) -> Option<DashboardSession> {
        let (expired, session) = {
            let mut map = self.sessions.lock().unwrap_or_else(|poisoned| {
                tracing::warn!("dashboard session store lock poisoned during get");
                poisoned.into_inner()
            });
            let expired = Self::evict_expired(&mut map, self.max_age);
            let session = map.get_mut(session_id).map(|session| {
                session.last_activity = Instant::now();
                session.clone()
            });
            (expired, session)
        };
        self.terminate_sessions(expired);
        session
    }

    pub fn remove(&self, session_id: &str) -> Option<DashboardSession> {
        let (expired, removed) = {
            let mut map = self.sessions.lock().unwrap_or_else(|poisoned| {
                tracing::warn!("dashboard session store lock poisoned during remove");
                poisoned.into_inner()
            });
            let expired = Self::evict_expired(&mut map, self.max_age);
            let removed = map.remove(session_id);
            (expired, removed)
        };
        self.terminate_sessions(expired);
        removed
    }

    pub fn active_count(&self) -> usize {
        let (expired, count) = {
            let mut map = self.sessions.lock().unwrap_or_else(|poisoned| {
                tracing::warn!("dashboard session store lock poisoned during active_count");
                poisoned.into_inner()
            });
            let expired = Self::evict_expired(&mut map, self.max_age);
            let count = map.len();
            (expired, count)
        };
        self.terminate_sessions(expired);
        count
    }

    fn evict_expired(
        map: &mut HashMap<String, DashboardSession>,
        max_age: Duration,
    ) -> Vec<DashboardSession> {
        let now = Instant::now();
        let expired_ids: Vec<String> = map
            .iter()
            .filter(|(_, session)| now.duration_since(session.last_activity) >= max_age)
            .map(|(session_id, _)| session_id.clone())
            .collect();

        expired_ids
            .into_iter()
            .filter_map(|session_id| map.remove(&session_id))
            .collect()
    }

    fn terminate_sessions(&self, sessions: Vec<DashboardSession>) {
        for session in sessions {
            if let Err(error) = self.engine.terminate(session.engine_session) {
                tracing::warn!(%error, "failed to terminate expired dashboard engine session");
            }
        }
    }
}

/// Generate a cryptographically random session ID (URL-safe base64, 32 bytes).
pub fn generate_session_id() -> Result<String, getrandom::Error> {
    let mut bytes = [0u8; 32];
    getrandom_fill(&mut bytes)?;
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csrf_roundtrip() {
        let secret = SessionSecret::generate().expect("session secret");
        let sid = "test-session-123";
        let token = secret.sign_csrf(sid);
        assert!(secret.verify_csrf(sid, &token));
        assert!(!secret.verify_csrf("other-session", &token));
        assert!(!secret.verify_csrf(sid, "bad-token"));
    }

    #[test]
    fn csrf_rejects_length_mismatch() {
        let secret = SessionSecret::generate().expect("session secret");
        let sid = "test-session-123";
        let token = secret.sign_csrf(sid);
        let shorter = &token[..token.len() - 1];
        assert!(!secret.verify_csrf(sid, shorter));
    }

    #[test]
    fn csrf_signing_failure_returns_non_empty_fallback_token() {
        let secret = SessionSecret::generate().expect("session secret");
        let token = secret.sign_csrf(FORCE_CSRF_SIGN_ERROR_SESSION_ID);
        assert!(!token.is_empty());
        assert_eq!(token, CSRF_FALLBACK_TOKEN);
    }

    #[test]
    fn csrf_verification_fails_closed_when_signing_errors() {
        let secret = SessionSecret::generate().expect("session secret");
        let token = secret.sign_csrf(FORCE_CSRF_SIGN_ERROR_SESSION_ID);
        assert!(!secret.verify_csrf(FORCE_CSRF_SIGN_ERROR_SESSION_ID, &token));
    }

    #[test]
    fn session_id_uniqueness() {
        let a = generate_session_id().expect("session id");
        let b = generate_session_id().expect("session id");
        assert_ne!(a, b);
        assert_eq!(a.len(), 43); // 32 bytes base64url no-pad
    }

    /// Helper: build a test engine wrapped in Arc.
    fn test_engine() -> Arc<Engine> {
        Arc::new(aiondb_engine::EngineBuilder::for_testing().build().unwrap())
    }

    /// Helper: allocate an engine session handle via startup.
    fn make_engine_session(engine: &Arc<Engine>) -> aiondb_engine::SessionHandle {
        let (handle, _) = engine
            .startup(aiondb_engine::StartupParams {
                database: "default".to_owned(),
                application_name: Some("dashboard-test".to_owned()),
                options: Default::default(),
                credential: aiondb_engine::Credential::Anonymous {
                    user: "admin".to_owned(),
                },
                transport: aiondb_engine::TransportInfo::in_process(),
            })
            .expect("engine startup");
        handle
    }

    /// Helper: create a `DashboardSession` with the given id and engine handle.
    fn make_session(id: &str, engine: &Arc<Engine>) -> DashboardSession {
        let engine_session = make_engine_session(engine);
        let now = Instant::now();
        DashboardSession {
            session_id: id.to_owned(),
            username: "admin".to_owned(),
            database: "aiondb".to_owned(),
            created_at: now,
            last_activity: now,
            engine_session,
        }
    }

    #[test]
    fn session_creation_and_retrieval() {
        let engine = test_engine();
        let store = SessionStore::new(Duration::from_secs(60), 8, engine.clone());

        let session = make_session("sess-1", &engine);
        assert!(store.insert(session));

        let retrieved = store.get("sess-1");
        assert!(retrieved.is_some());
        let retrieved = retrieved.unwrap();
        assert_eq!(retrieved.session_id, "sess-1");
        assert_eq!(retrieved.username, "admin");
        assert_eq!(retrieved.database, "aiondb");
    }

    #[test]
    fn session_expiration() {
        let engine = test_engine();
        // Very short max_age so sessions expire quickly.
        let store = SessionStore::new(Duration::from_millis(50), 8, engine.clone());

        let session = make_session("expire-me", &engine);
        assert!(store.insert(session));

        // Immediately the session should be retrievable.
        assert!(store.get("expire-me").is_some());

        // Wait for expiration.
        std::thread::sleep(Duration::from_millis(120));

        // After expiration, session must be gone.
        assert!(store.get("expire-me").is_none());
        assert_eq!(store.active_count(), 0);
    }

    #[test]
    fn csrf_token_generation_and_verification() {
        let secret = SessionSecret::generate().expect("session secret");
        let sid = "csrf-test-session";
        let token = secret.sign_csrf(sid);

        // Token is non-empty and looks like base64url.
        assert!(!token.is_empty());
        // Verifying the correct pair succeeds.
        assert!(secret.verify_csrf(sid, &token));
        // Generating a second time for the same session produces the same token
        // (HMAC is deterministic for the same key + message).
        let token2 = secret.sign_csrf(sid);
        assert_eq!(token, token2);
    }

    #[test]
    fn csrf_token_rejection_for_invalid_tokens() {
        let secret = SessionSecret::generate().expect("session secret");
        let sid = "my-session";
        let token = secret.sign_csrf(sid);

        // Wrong session id.
        assert!(!secret.verify_csrf("wrong-session", &token));
        // Completely fabricated token.
        assert!(!secret.verify_csrf(sid, "totally-bogus-token"));
        // Empty token.
        assert!(!secret.verify_csrf(sid, ""));
        // Truncated token.
        assert!(!secret.verify_csrf(sid, &token[..4]));
        // Token with one byte flipped.
        let mut corrupted = token.clone().into_bytes();
        corrupted[0] ^= 0xFF;
        let corrupted = String::from_utf8_lossy(&corrupted);
        assert!(!secret.verify_csrf(sid, &corrupted));
        // Different secret produces a different token that does not verify.
        let other_secret = SessionSecret::generate().expect("other secret");
        let other_token = other_secret.sign_csrf(sid);
        assert!(!secret.verify_csrf(sid, &other_token));
    }

    #[test]
    fn max_sessions_enforcement() {
        let engine = test_engine();
        let max = 3;
        let store = SessionStore::new(Duration::from_secs(60), max, engine.clone());

        // Fill up to the limit.
        for i in 0..max {
            let session = make_session(&format!("s-{i}"), &engine);
            assert!(store.insert(session), "insert {i} should succeed");
        }
        assert_eq!(store.active_count(), max);

        // The next insert must be rejected (returns false).
        let overflow = make_session("s-overflow", &engine);
        assert!(!store.insert(overflow), "insert beyond max must fail");
        assert_eq!(store.active_count(), max);

        // The overflow session should not be found.
        assert!(store.get("s-overflow").is_none());
    }

    #[test]
    fn max_sessions_after_expiry_allows_new_inserts() {
        let engine = test_engine();
        let max = 2;
        // Short max_age so we can expire sessions quickly.
        let store = SessionStore::new(Duration::from_millis(50), max, engine.clone());

        // Fill to the limit.
        for i in 0..max {
            let session = make_session(&format!("old-{i}"), &engine);
            assert!(store.insert(session));
        }
        assert_eq!(store.active_count(), max);

        // Wait for sessions to expire.
        std::thread::sleep(Duration::from_millis(120));

        // Now a new insert should succeed because expired sessions are evicted first.
        let fresh = make_session("fresh-1", &engine);
        assert!(
            store.insert(fresh),
            "insert should succeed after expiry eviction"
        );
        assert_eq!(store.active_count(), 1);
    }

    #[test]
    fn session_removal() {
        let engine = test_engine();
        let store = SessionStore::new(Duration::from_secs(60), 8, engine.clone());

        let session = make_session("to-remove", &engine);
        assert!(store.insert(session));
        assert_eq!(store.active_count(), 1);

        let removed = store.remove("to-remove");
        assert!(removed.is_some());
        assert_eq!(removed.unwrap().session_id, "to-remove");
        assert_eq!(store.active_count(), 0);
    }

    #[test]
    fn session_not_found_after_removal() {
        let engine = test_engine();
        let store = SessionStore::new(Duration::from_secs(60), 8, engine.clone());

        let session = make_session("vanish", &engine);
        assert!(store.insert(session));
        assert!(store.get("vanish").is_some());

        store.remove("vanish");

        assert!(store.get("vanish").is_none());
        // Removing again returns None.
        assert!(store.remove("vanish").is_none());
    }

    #[test]
    fn expiring_dashboard_sessions_terminates_engine_sessions() {
        let engine = test_engine();
        let store = SessionStore::new(Duration::from_millis(50), 8, engine.clone());

        let session = make_session("session-1", &engine);
        assert!(store.insert(session));
        assert_eq!(engine.session_count().expect("session count"), 1);

        std::thread::sleep(Duration::from_millis(120));

        assert_eq!(store.active_count(), 0);
        assert_eq!(engine.session_count().expect("session count"), 0);
    }
}
