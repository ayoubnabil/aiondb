//! 0-day audit POCs for pgwire state machine.
//!
//! Each test here is an adversarial scenario that attempts to drive the
//! connection state machine into a confused, bypassed, or crashing state
//! by emitting raw octets that a well-behaved client would never send.
//!
//! These tests are GREEN when the server behaves correctly (refuses the
//! abuse). Any test that begins to fail is a confirmed regression and
//! should be treated as a protocol-security issue.

use super::*;

fn build_raw_startup_payload(params: &[(&str, &str)]) -> Vec<u8> {
    build_startup_bytes_with_params(params)
}

// ---------------------------------------------------------------------------
// Scenario 1: double StartupMessage.
//
// After a successful startup, a second startup frame is injected. The
// post-auth loop reads tagged frontend messages, so the first byte of the
// second startup's length field becomes a bogus tag (usually 0x00) and the
// following 4 bytes are interpreted as an oversized length. The stream
// reader either rejects the unknown tag or hits EOF/underflow and closes
// the connection - CRUCIALLY without ever letting the "evil_root" user
// reach the engine.
// ---------------------------------------------------------------------------
#[tokio::test]
async fn double_startup_does_not_rebind_user() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static STARTUP_CALLS: AtomicUsize = AtomicUsize::new(0);

    struct CountingEngine(MockEngine);
    impl QueryEngine for CountingEngine {
        fn startup_authentication(
            &self,
            u: &str,
            d: &str,
            t: &TransportInfo,
        ) -> DbResult<StartupAuthentication> {
            <MockEngine as QueryEngine>::startup_authentication(&self.0, u, d, t)
        }
        fn startup(&self, p: StartupParams) -> DbResult<(SessionHandle, SessionInfo)> {
            STARTUP_CALLS.fetch_add(1, Ordering::SeqCst);
            assert_ne!(
                p.credential.user(),
                "evil_root",
                "second startup must not reach the engine"
            );
            <MockEngine as QueryEngine>::startup(&self.0, p)
        }
        fn has_active_transaction(&self, s: &SessionHandle) -> DbResult<bool> {
            <MockEngine as QueryEngine>::has_active_transaction(&self.0, s)
        }
        fn begin_transaction(&self, s: &SessionHandle, i: IsolationLevel) -> DbResult<()> {
            <MockEngine as QueryEngine>::begin_transaction(&self.0, s, i)
        }
        fn commit_transaction(&self, s: &SessionHandle) -> DbResult<()> {
            <MockEngine as QueryEngine>::commit_transaction(&self.0, s)
        }
        fn rollback_transaction(&self, s: &SessionHandle) -> DbResult<()> {
            <MockEngine as QueryEngine>::rollback_transaction(&self.0, s)
        }
        fn execute_sql(&self, s: &SessionHandle, sql: &str) -> DbResult<Vec<StatementResult>> {
            <MockEngine as QueryEngine>::execute_sql(&self.0, s, sql)
        }
        fn prepare(
            &self,
            s: &SessionHandle,
            n: String,
            q: String,
        ) -> DbResult<PreparedStatementDesc> {
            <MockEngine as QueryEngine>::prepare(&self.0, s, n, q)
        }
        fn describe_statement(
            &self,
            s: &SessionHandle,
            n: &str,
        ) -> DbResult<PreparedStatementDesc> {
            <MockEngine as QueryEngine>::describe_statement(&self.0, s, n)
        }
        fn bind(&self, s: &SessionHandle, p: String, n: String, v: Vec<Value>) -> DbResult<()> {
            <MockEngine as QueryEngine>::bind(&self.0, s, p, n, v)
        }
        fn describe_portal(&self, s: &SessionHandle, n: &str) -> DbResult<PortalDescription> {
            <MockEngine as QueryEngine>::describe_portal(&self.0, s, n)
        }
        fn execute_portal(&self, s: &SessionHandle, n: &str, m: usize) -> DbResult<PortalBatch> {
            <MockEngine as QueryEngine>::execute_portal(&self.0, s, n, m)
        }
        fn close_statement(&self, s: &SessionHandle, n: &str) -> DbResult<()> {
            <MockEngine as QueryEngine>::close_statement(&self.0, s, n)
        }
        fn close_portal(&self, s: &SessionHandle, n: &str) -> DbResult<()> {
            <MockEngine as QueryEngine>::close_portal(&self.0, s, n)
        }
        fn execute_copy_from(
            &self,
            s: &SessionHandle,
            t: aiondb_core::RelationId,
            d: &str,
        ) -> DbResult<StatementResult> {
            <MockEngine as QueryEngine>::execute_copy_from(&self.0, s, t, d)
        }
        fn cancel_session(&self, s: &SessionHandle) -> DbResult<()> {
            <MockEngine as QueryEngine>::cancel_session(&self.0, s)
        }
        fn terminate(&self, s: SessionHandle) -> DbResult<()> {
            <MockEngine as QueryEngine>::terminate(&self.0, s)
        }
        fn session_count(&self) -> DbResult<usize> {
            <MockEngine as QueryEngine>::session_count(&self.0)
        }
    }

    let engine = Arc::new(CountingEngine(MockEngine::new()));
    let mut input = build_startup_bytes_with_user("alice");
    input.extend(build_raw_startup_payload(&[("user", "evil_root")]));
    input.extend(build_query_bytes("SELECT 1"));
    input.extend(build_terminate_bytes());

    let mut conn = make_connection_with_query_engine(input, engine);
    // The connection may close cleanly or with a read error - either is
    // acceptable, the critical property is that startup is called exactly
    // once and never with the "evil_root" user.
    let _ = conn.run().await;
    assert_eq!(
        STARTUP_CALLS.load(Ordering::SeqCst),
        1,
        "engine startup must not be invoked a second time"
    );
}

// ---------------------------------------------------------------------------
// Scenario 2: Query before authentication.
//
// message. The connection must reject the out-of-order stream with a
// protocol error coming from the startup reader.
// ---------------------------------------------------------------------------
#[tokio::test]
async fn query_before_startup_is_rejected() {
    // Emit a Query frame as the very first bytes. read_startup will read
    // four bytes as the supposed length (= 0x51 << 24 | ...) which is a
    // large, nonsensical value, and the reader will fail before any
    // engine code is invoked.
    let input = build_query_bytes("SELECT 1");
    let mut conn = make_connection(input);
    let err = conn
        .run()
        .await
        .expect_err("query before startup must fail");
    // Any startup parsing error - we just require that the session was
    // never handed a Query for execution.
    assert_eq!(err.sqlstate(), aiondb_core::SqlState::InternalError);
}

// ---------------------------------------------------------------------------
// Scenario 3: Bind referencing an unknown prepared statement.
//
// must write an ErrorResponse and skip to the next Sync.
// ---------------------------------------------------------------------------
#[tokio::test]
async fn bind_unknown_statement_errors_cleanly() {
    // Engine that errors describe_statement for unknown names.
    struct StrictEngine(MockEngine);
    impl QueryEngine for StrictEngine {
        fn startup_authentication(
            &self,
            u: &str,
            d: &str,
            t: &TransportInfo,
        ) -> DbResult<StartupAuthentication> {
            <MockEngine as QueryEngine>::startup_authentication(&self.0, u, d, t)
        }
        fn startup(&self, p: StartupParams) -> DbResult<(SessionHandle, SessionInfo)> {
            <MockEngine as QueryEngine>::startup(&self.0, p)
        }
        fn has_active_transaction(&self, s: &SessionHandle) -> DbResult<bool> {
            <MockEngine as QueryEngine>::has_active_transaction(&self.0, s)
        }
        fn begin_transaction(&self, s: &SessionHandle, i: IsolationLevel) -> DbResult<()> {
            <MockEngine as QueryEngine>::begin_transaction(&self.0, s, i)
        }
        fn commit_transaction(&self, s: &SessionHandle) -> DbResult<()> {
            <MockEngine as QueryEngine>::commit_transaction(&self.0, s)
        }
        fn rollback_transaction(&self, s: &SessionHandle) -> DbResult<()> {
            <MockEngine as QueryEngine>::rollback_transaction(&self.0, s)
        }
        fn execute_sql(&self, s: &SessionHandle, sql: &str) -> DbResult<Vec<StatementResult>> {
            <MockEngine as QueryEngine>::execute_sql(&self.0, s, sql)
        }
        fn prepare(
            &self,
            s: &SessionHandle,
            n: String,
            q: String,
        ) -> DbResult<PreparedStatementDesc> {
            <MockEngine as QueryEngine>::prepare(&self.0, s, n, q)
        }
        fn describe_statement(
            &self,
            _s: &SessionHandle,
            name: &str,
        ) -> DbResult<PreparedStatementDesc> {
            Err(DbError::protocol(format!(
                "prepared statement \"{name}\" does not exist"
            )))
        }
        fn bind(&self, s: &SessionHandle, p: String, n: String, v: Vec<Value>) -> DbResult<()> {
            <MockEngine as QueryEngine>::bind(&self.0, s, p, n, v)
        }
        fn describe_portal(&self, s: &SessionHandle, n: &str) -> DbResult<PortalDescription> {
            <MockEngine as QueryEngine>::describe_portal(&self.0, s, n)
        }
        fn execute_portal(&self, s: &SessionHandle, n: &str, m: usize) -> DbResult<PortalBatch> {
            <MockEngine as QueryEngine>::execute_portal(&self.0, s, n, m)
        }
        fn close_statement(&self, s: &SessionHandle, n: &str) -> DbResult<()> {
            <MockEngine as QueryEngine>::close_statement(&self.0, s, n)
        }
        fn close_portal(&self, s: &SessionHandle, n: &str) -> DbResult<()> {
            <MockEngine as QueryEngine>::close_portal(&self.0, s, n)
        }
        fn execute_copy_from(
            &self,
            s: &SessionHandle,
            t: aiondb_core::RelationId,
            d: &str,
        ) -> DbResult<StatementResult> {
            <MockEngine as QueryEngine>::execute_copy_from(&self.0, s, t, d)
        }
        fn cancel_session(&self, s: &SessionHandle) -> DbResult<()> {
            <MockEngine as QueryEngine>::cancel_session(&self.0, s)
        }
        fn terminate(&self, s: SessionHandle) -> DbResult<()> {
            <MockEngine as QueryEngine>::terminate(&self.0, s)
        }
        fn session_count(&self) -> DbResult<usize> {
            <MockEngine as QueryEngine>::session_count(&self.0)
        }
    }

    let engine = Arc::new(StrictEngine(MockEngine::new()));
    let mut input = build_startup_bytes();
    input.extend(build_bind_bytes("p1", "ghost", &[], &[], &[]));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());
    let mut conn = make_connection_with_query_engine(input, engine);
    conn.run().await.expect("connection must stay alive");

    let codes = error_response_codes(&conn.writer);
    assert!(
        !codes.is_empty(),
        "expected an ErrorResponse for Bind of unknown statement"
    );
    let tags = backend_message_tags(&conn.writer);
    // Tags after startup banner must contain exactly E (error) then Z
    // (ready for query after Sync). No BindComplete ('2'), no 1/2 confirms.
    assert!(!tags.contains(&b'2'), "must not send BindComplete");
}

// ---------------------------------------------------------------------------
// Scenario 4: Execute portal that was never bound.
//
// Must error (propagated through skip-until-sync state machine) and
// NEVER send CommandComplete/DataRow for the phantom portal.
// ---------------------------------------------------------------------------
#[tokio::test]
async fn execute_unknown_portal_errors_cleanly() {
    struct NoPortalEngine(MockEngine);
    impl QueryEngine for NoPortalEngine {
        fn startup_authentication(
            &self,
            u: &str,
            d: &str,
            t: &TransportInfo,
        ) -> DbResult<StartupAuthentication> {
            <MockEngine as QueryEngine>::startup_authentication(&self.0, u, d, t)
        }
        fn startup(&self, p: StartupParams) -> DbResult<(SessionHandle, SessionInfo)> {
            <MockEngine as QueryEngine>::startup(&self.0, p)
        }
        fn has_active_transaction(&self, s: &SessionHandle) -> DbResult<bool> {
            <MockEngine as QueryEngine>::has_active_transaction(&self.0, s)
        }
        fn begin_transaction(&self, s: &SessionHandle, i: IsolationLevel) -> DbResult<()> {
            <MockEngine as QueryEngine>::begin_transaction(&self.0, s, i)
        }
        fn commit_transaction(&self, s: &SessionHandle) -> DbResult<()> {
            <MockEngine as QueryEngine>::commit_transaction(&self.0, s)
        }
        fn rollback_transaction(&self, s: &SessionHandle) -> DbResult<()> {
            <MockEngine as QueryEngine>::rollback_transaction(&self.0, s)
        }
        fn execute_sql(&self, s: &SessionHandle, sql: &str) -> DbResult<Vec<StatementResult>> {
            <MockEngine as QueryEngine>::execute_sql(&self.0, s, sql)
        }
        fn prepare(
            &self,
            s: &SessionHandle,
            n: String,
            q: String,
        ) -> DbResult<PreparedStatementDesc> {
            <MockEngine as QueryEngine>::prepare(&self.0, s, n, q)
        }
        fn describe_statement(
            &self,
            s: &SessionHandle,
            n: &str,
        ) -> DbResult<PreparedStatementDesc> {
            <MockEngine as QueryEngine>::describe_statement(&self.0, s, n)
        }
        fn bind(&self, s: &SessionHandle, p: String, n: String, v: Vec<Value>) -> DbResult<()> {
            <MockEngine as QueryEngine>::bind(&self.0, s, p, n, v)
        }
        fn describe_portal(&self, s: &SessionHandle, n: &str) -> DbResult<PortalDescription> {
            <MockEngine as QueryEngine>::describe_portal(&self.0, s, n)
        }
        fn execute_portal(
            &self,
            _s: &SessionHandle,
            name: &str,
            _m: usize,
        ) -> DbResult<PortalBatch> {
            Err(DbError::protocol(format!(
                "portal \"{name}\" does not exist"
            )))
        }
        fn close_statement(&self, s: &SessionHandle, n: &str) -> DbResult<()> {
            <MockEngine as QueryEngine>::close_statement(&self.0, s, n)
        }
        fn close_portal(&self, s: &SessionHandle, n: &str) -> DbResult<()> {
            <MockEngine as QueryEngine>::close_portal(&self.0, s, n)
        }
        fn execute_copy_from(
            &self,
            s: &SessionHandle,
            t: aiondb_core::RelationId,
            d: &str,
        ) -> DbResult<StatementResult> {
            <MockEngine as QueryEngine>::execute_copy_from(&self.0, s, t, d)
        }
        fn cancel_session(&self, s: &SessionHandle) -> DbResult<()> {
            <MockEngine as QueryEngine>::cancel_session(&self.0, s)
        }
        fn terminate(&self, s: SessionHandle) -> DbResult<()> {
            <MockEngine as QueryEngine>::terminate(&self.0, s)
        }
        fn session_count(&self) -> DbResult<usize> {
            <MockEngine as QueryEngine>::session_count(&self.0)
        }
    }

    let engine = Arc::new(NoPortalEngine(MockEngine::new()));
    let mut input = build_startup_bytes();
    input.extend(build_execute_bytes("ghost_portal", 0));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let mut conn = make_connection_with_query_engine(input, engine);
    conn.run().await.expect("connection stays alive");

    let tags = backend_message_tags(&conn.writer);
    assert!(
        tags.contains(&b'E'),
        "expected an ErrorResponse; got tags {tags:?}"
    );
    assert!(
        !tags.contains(&b'C'),
        "must not emit CommandComplete for phantom portal"
    );
}

// ---------------------------------------------------------------------------
// Scenario 7: Tiny header-only frame (length = 4, empty payload).
//
// For messages that require payload (Query, Parse, Bind, Describe, Execute,
// Close, Password, CopyFail) the server must emit a clean protocol error
// and not panic.
// ---------------------------------------------------------------------------
#[tokio::test]
async fn empty_payload_query_frame_does_not_panic() {
    let mut input = build_startup_bytes();
    // Query with length=4 and no body: missing cstring terminator.
    input.push(b'Q');
    input.extend_from_slice(&4u32.to_be_bytes());
    input.extend(build_terminate_bytes());

    let mut conn = make_connection(input);
    conn.run().await.expect("connection must not panic");

    let codes = error_response_codes(&conn.writer);
    assert!(
        !codes.is_empty(),
        "server must reply with an ErrorResponse for malformed Query, got {codes:?}"
    );
}

// ---------------------------------------------------------------------------
// Scenario 8: trailing bytes in a frontend message.
//
// Every handler calls reject_trailing_payload, so trailing bytes must
// trigger a protocol error instead of desynchronising the parser.
// ---------------------------------------------------------------------------
#[tokio::test]
async fn trailing_bytes_in_query_are_rejected() {
    let mut input = build_startup_bytes();
    // Manually build a Query frame with trailing junk after the cstring nul.
    let sql = b"SELECT 1";
    let mut payload = Vec::new();
    payload.extend_from_slice(sql);
    payload.push(0); // cstring terminator
    payload.extend_from_slice(b"junk"); // trailing bytes
    let mut frame = Vec::new();
    frame.push(b'Q');
    frame.extend_from_slice(&((payload.len() + 4) as u32).to_be_bytes());
    frame.extend_from_slice(&payload);
    input.extend_from_slice(&frame);
    input.extend(build_terminate_bytes());

    let mut conn = make_connection(input);
    conn.run().await.expect("connection must not panic");

    let codes = error_response_codes(&conn.writer);
    assert!(
        !codes.is_empty(),
        "server must reply with an ErrorResponse for trailing-bytes Query, got {codes:?}"
    );
}

// ---------------------------------------------------------------------------
// Scenario 9: CancelRequest for a (pid, secret_key) that does not exist.
//
// response written to the socket (cancel requests are one-shot and the
// server closes the connection after processing).
// ---------------------------------------------------------------------------
#[tokio::test]
async fn cancel_request_with_wrong_secret_is_ignored() {
    // Build a CancelRequest startup frame for an unknown pid/key.
    let mut payload = Vec::new();
    payload.extend_from_slice(&codec::CANCEL_REQUEST.to_be_bytes());
    payload.extend_from_slice(&42u32.to_be_bytes()); // pid
    payload.extend_from_slice(&0xDEAD_BEEFu32.to_be_bytes()); // wrong key
    let mut input = Vec::new();
    let total_len = (payload.len() + 4) as u32;
    input.extend_from_slice(&total_len.to_be_bytes());
    input.extend_from_slice(&payload);

    let mut conn = make_connection(input);
    conn.run().await.expect("cancel request returns cleanly");

    // No backend bytes should have been emitted.
    assert!(
        conn.writer.is_empty(),
        "cancel request must not emit any backend message, got {} bytes",
        conn.writer.len()
    );
}

// ---------------------------------------------------------------------------
// Extra: parser/int overflow. A negative num_params must be rejected.
// ---------------------------------------------------------------------------
#[tokio::test]
async fn parse_with_negative_num_params_is_rejected() {
    let mut input = build_startup_bytes();
    // Hand-build a Parse frame with num_params = -1
    let mut payload = Vec::new();
    payload.push(0); // empty stmt name
    payload.push(0); // empty query
    payload.extend_from_slice(&(-1_i16).to_be_bytes());
    let mut frame = Vec::new();
    frame.push(b'P');
    frame.extend_from_slice(&((payload.len() + 4) as u32).to_be_bytes());
    frame.extend_from_slice(&payload);
    input.extend_from_slice(&frame);
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let mut conn = make_connection(input);
    conn.run().await.expect("must not panic");
    let codes = error_response_codes(&conn.writer);
    assert!(
        !codes.is_empty(),
        "server must reject negative num_params, got {codes:?}"
    );
}
