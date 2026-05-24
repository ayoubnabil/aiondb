//! Wire protocol types and codec for fragment transport.
//!
//! Framed binary protocol:
//! ```text
//! msg_type (u8) | payload_len (u32 LE) | payload (JSON bytes)
//! ```
//!
//! Message type constants distinguish request / response / cancel
//! directions so the receiver can validate before parsing JSON.

use aiondb_core::{DbError, DbResult};
use aiondb_executor::ExecutionResult;
use aiondb_plan::PhysicalPlan;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Current protocol version emitted by this build.
pub const PROTOCOL_VERSION: u8 = 1;

/// Minimum protocol version this build is willing to accept on the wire.
///
/// Combined with [`PROTOCOL_VERSION`], the server accepts any envelope
/// whose `version` falls in the inclusive range
/// `MIN_PROTOCOL_VERSION..=PROTOCOL_VERSION`. Equal values mean no
/// cross-version compatibility; bump only `PROTOCOL_VERSION` first to
/// support a rolling upgrade window.
pub const MIN_PROTOCOL_VERSION: u8 = 1;

/// Maximum payload size (64 MiB) to guard against unbounded allocations.
pub const MAX_PAYLOAD_BYTES: usize = 64 * 1024 * 1024;

/// V2-05 : tight cap applied to the first envelope on a connection,
/// i.e. before the auth handshake has succeeded. A peer that turns
/// out to be unauthenticated cannot force the server to stream and
/// JSON-parse the full 64 MiB. Any envelope larger than this on an
/// unauthenticated connection is rejected at the header check.
pub const MAX_PRE_AUTH_PAYLOAD_BYTES: usize = 64 * 1024;

/// Message type: execute a query plan fragment.
pub const MSG_EXECUTE_FRAGMENT: u8 = 0x01;

/// Message type: cancel an in-flight fragment execution.
pub const MSG_CANCEL_FRAGMENT: u8 = 0x02;

/// Message type: fragment execution result (success, error, or cancel ack).
pub const MSG_FRAGMENT_RESULT: u8 = 0x81;

/// Message type: fragment execution error.
pub const MSG_FRAGMENT_ERROR: u8 = 0x82;

/// Message type: cancel acknowledged.
pub const MSG_CANCEL_ACK: u8 = 0x83;

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

/// Request to execute a query plan fragment on a remote node.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FragmentRequest {
    pub request_id: u64,
    pub plan: PhysicalPlan,
    pub txn_id: u64,
    pub isolation: String,
    pub max_result_rows: u64,
    pub max_result_bytes: u64,
    pub max_memory_bytes: u64,
    pub max_temp_bytes: u64,
    /// Optional MVCC snapshot selected by the coordinator for this fragment.
    #[serde(default)]
    pub snapshot: Option<FragmentSnapshot>,
    /// Absolute deadline as milliseconds since UNIX epoch. `None` means
    /// no deadline.
    pub deadline_epoch_ms: Option<u64>,
    /// When set, restricts fragment execution to the specified shard of a
    /// sharded table. Used by the shard-aware query coordinator to direct
    /// fragments to the correct node/shard partition.
    #[serde(default)]
    pub shard_id: Option<u32>,
    /// Random secret the coordinator must replay in `CancelRequest` to
    /// authorize cancelling this in-flight fragment. Mirrors PG's
    /// pid+key cancel-authorization model so any peer who merely guesses
    /// `request_id` cannot terminate another tenant's work. Older
    /// clients that do not set this field default to 0; the server
    /// rejects cancels when the registered key is non-zero and the
    /// presented key does not match.
    #[serde(default)]
    pub cancel_key: u64,
}

/// Serializable MVCC snapshot metadata for fragment execution.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FragmentSnapshot {
    pub xmin: u64,
    pub xmax: u64,
    #[serde(default)]
    pub active: Vec<u64>,
}

/// Maximum number of active xids the wire is willing to accept inside a
/// `FragmentSnapshot`. Even authenticated peers can hand us a malformed
/// snapshot; we cap the size so a single envelope cannot pin millions of
/// `u64`s on the wrong side of the trust boundary while we walk the
/// vector to validate it.
pub const MAX_FRAGMENT_SNAPSHOT_ACTIVE_XIDS: usize = 1 << 20;

impl FragmentSnapshot {
    /// Validate that an incoming `FragmentSnapshot` is internally
    /// consistent. The fragment-transport server calls this at the wire
    /// trust boundary so the local MVCC engine never sees a snapshot with
    /// `xmin > xmax`, an `active` xid outside `[xmin, xmax)`, or an
    /// unbounded `active` set.
    ///
    /// # Errors
    ///
    /// Returns a protocol error when any invariant is violated.
    pub fn validate(&self) -> DbResult<()> {
        if self.xmin > self.xmax {
            return Err(DbError::protocol(format!(
                "fragment snapshot xmin ({}) > xmax ({})",
                self.xmin, self.xmax
            )));
        }
        if self.active.len() > MAX_FRAGMENT_SNAPSHOT_ACTIVE_XIDS {
            return Err(DbError::protocol(format!(
                "fragment snapshot active xid set too large ({}, max {MAX_FRAGMENT_SNAPSHOT_ACTIVE_XIDS})",
                self.active.len()
            )));
        }
        for &xid in &self.active {
            if xid < self.xmin || xid >= self.xmax {
                return Err(DbError::protocol(format!(
                    "fragment snapshot active xid {xid} outside [{}, {})",
                    self.xmin, self.xmax
                )));
            }
        }
        Ok(())
    }
}

/// Request to cancel an in-flight fragment execution.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CancelRequest {
    pub request_id: u64,
    /// Authorization key matching the `cancel_key` registered with the
    /// fragment by the coordinator. The server rejects cancels whose
    /// key does not match the registered owner. Old clients that do
    /// not set this field default to 0; cancelling fragments registered
    /// with a non-zero key from such clients is refused.
    #[serde(default)]
    pub cancel_key: u64,
}

/// Response from a remote fragment execution.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum FragmentResponse {
    Success {
        request_id: u64,
        result: ExecutionResult,
    },
    Error {
        request_id: u64,
        message: String,
        sql_state: Option<String>,
    },
    CancelAck {
        request_id: u64,
    },
}

/// Envelope wrapping any transport message with auth.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TransportEnvelope {
    pub version: u8,
    pub auth_token: String,
    pub payload: TransportPayload,
}

/// Discriminated payload within a transport envelope.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum TransportPayload {
    ExecuteFragment(Box<FragmentRequest>),
    CancelFragment(CancelRequest),
    FragmentResult(FragmentResponse),
}

// ---------------------------------------------------------------------------
// Codec helpers
// ---------------------------------------------------------------------------

/// Determine the wire message type byte for a given payload variant.
fn msg_type_for_payload(payload: &TransportPayload) -> u8 {
    match payload {
        TransportPayload::ExecuteFragment(_) => MSG_EXECUTE_FRAGMENT,
        TransportPayload::CancelFragment(_) => MSG_CANCEL_FRAGMENT,
        TransportPayload::FragmentResult(_) => MSG_FRAGMENT_RESULT,
    }
}

fn msg_type_matches_payload(msg_type: u8, payload: &TransportPayload) -> bool {
    match payload {
        TransportPayload::ExecuteFragment(_) => msg_type == MSG_EXECUTE_FRAGMENT,
        TransportPayload::CancelFragment(_) => msg_type == MSG_CANCEL_FRAGMENT,
        TransportPayload::FragmentResult(FragmentResponse::Success { .. }) => {
            msg_type == MSG_FRAGMENT_RESULT
        }
        TransportPayload::FragmentResult(FragmentResponse::Error { .. }) => {
            matches!(msg_type, MSG_FRAGMENT_RESULT | MSG_FRAGMENT_ERROR)
        }
        TransportPayload::FragmentResult(FragmentResponse::CancelAck { .. }) => {
            matches!(msg_type, MSG_FRAGMENT_RESULT | MSG_CANCEL_ACK)
        }
    }
}

fn validate_msg_type_matches_payload(msg_type: u8, payload: &TransportPayload) -> DbResult<()> {
    if msg_type_matches_payload(msg_type, payload) {
        Ok(())
    } else {
        Err(DbError::protocol(format!(
            "message type 0x{msg_type:02x} does not match transport payload"
        )))
    }
}

/// Encode an envelope into a framed byte buffer.
///
/// Wire format: `msg_type(u8) | payload_len(u32 LE) | json_bytes`.
///
/// # Errors
///
/// Returns a `DbError` if JSON serialization fails or the payload exceeds
/// [`MAX_PAYLOAD_BYTES`].
pub fn encode_envelope(envelope: &TransportEnvelope) -> DbResult<Vec<u8>> {
    let json = serde_json::to_vec(envelope)
        .map_err(|e| DbError::protocol(format!("serialize envelope: {e}")))?;

    if json.len() > MAX_PAYLOAD_BYTES {
        return Err(DbError::protocol(format!(
            "payload too large: {} bytes exceeds {MAX_PAYLOAD_BYTES} limit",
            json.len(),
        )));
    }

    let msg_type = msg_type_for_payload(&envelope.payload);
    let len = u32::try_from(json.len())
        .map_err(|_| DbError::internal("fragment payload length exceeds u32"))?;

    // 1 byte msg_type + 4 bytes length + payload
    let mut buf = Vec::with_capacity(1 + 4 + json.len());
    buf.push(msg_type);
    buf.extend_from_slice(&len.to_le_bytes());
    buf.extend_from_slice(&json);

    Ok(buf)
}

/// Decode a framed envelope from a byte buffer.
///
/// Returns the decoded envelope and the number of bytes consumed.
///
/// # Errors
///
/// Returns a `DbError` on truncated input, unknown message type, payload
/// exceeding [`MAX_PAYLOAD_BYTES`], or JSON deserialization failure.
pub fn decode_envelope(data: &[u8]) -> DbResult<(TransportEnvelope, usize)> {
    const HEADER_LEN: usize = 5; // 1 byte msg_type + 4 bytes payload_len

    if data.len() < HEADER_LEN {
        return Err(DbError::protocol(format!(
            "frame too short: need at least {HEADER_LEN} bytes, got {}",
            data.len(),
        )));
    }

    let msg_type = data[0];
    if !matches!(
        msg_type,
        MSG_EXECUTE_FRAGMENT
            | MSG_CANCEL_FRAGMENT
            | MSG_FRAGMENT_RESULT
            | MSG_FRAGMENT_ERROR
            | MSG_CANCEL_ACK
    ) {
        return Err(DbError::protocol(format!(
            "unknown message type: 0x{msg_type:02x}"
        )));
    }

    let payload_len = u32::from_le_bytes([data[1], data[2], data[3], data[4]]) as usize;

    if payload_len > MAX_PAYLOAD_BYTES {
        return Err(DbError::protocol(format!(
            "payload too large: {payload_len} bytes exceeds {MAX_PAYLOAD_BYTES} limit",
        )));
    }

    let total = HEADER_LEN + payload_len;
    if data.len() < total {
        return Err(DbError::protocol(format!(
            "incomplete frame: need {total} bytes, got {}",
            data.len(),
        )));
    }

    let json_slice = &data[HEADER_LEN..total];
    let envelope: TransportEnvelope = serde_json::from_slice(json_slice)
        .map_err(|e| DbError::protocol(format!("deserialize envelope: {e}")))?;
    validate_msg_type_matches_payload(msg_type, &envelope.payload)?;

    Ok((envelope, total))
}

/// Write a framed envelope to an async stream.
///
/// # Errors
///
/// Returns a `DbError` on serialization or I/O failure.
pub async fn write_envelope(
    stream: &mut (impl AsyncWrite + Unpin),
    envelope: &TransportEnvelope,
) -> DbResult<()> {
    let frame = encode_envelope(envelope)?;

    stream
        .write_all(&frame)
        .await
        .map_err(|e| DbError::protocol(format!("write frame: {e}")))?;

    stream
        .flush()
        .await
        .map_err(|e| DbError::protocol(format!("flush: {e}")))?;

    Ok(())
}

/// Read a framed envelope from an async stream.
///
/// # Errors
///
/// Returns a `DbError` on I/O failure, unknown message type, or
/// deserialization error.
pub async fn read_envelope(stream: &mut (impl AsyncRead + Unpin)) -> DbResult<TransportEnvelope> {
    read_envelope_with_limit(stream, MAX_PAYLOAD_BYTES).await
}

/// Same as [`read_envelope`] but bounded by an explicit per-frame limit.
/// Used by the framed server during the pre-auth handshake so an
/// unauthenticated peer cannot force a 64 MiB read+parse.
pub async fn read_envelope_with_limit(
    stream: &mut (impl AsyncRead + Unpin),
    max_payload_bytes: usize,
) -> DbResult<TransportEnvelope> {
    // Read the 5-byte header: msg_type (1) + payload_len (4 LE).
    let mut header = [0u8; 5];
    stream
        .read_exact(&mut header)
        .await
        .map_err(|e| DbError::protocol(format!("read header: {e}")))?;

    let msg_type = header[0];
    if !matches!(
        msg_type,
        MSG_EXECUTE_FRAGMENT
            | MSG_CANCEL_FRAGMENT
            | MSG_FRAGMENT_RESULT
            | MSG_FRAGMENT_ERROR
            | MSG_CANCEL_ACK
    ) {
        return Err(DbError::protocol(format!(
            "unknown message type: 0x{msg_type:02x}"
        )));
    }

    let payload_len = u32::from_le_bytes([header[1], header[2], header[3], header[4]]) as usize;

    if payload_len > max_payload_bytes {
        return Err(DbError::protocol(format!(
            "payload too large: {payload_len} bytes exceeds {max_payload_bytes} limit",
        )));
    }

    // Stream into the buffer instead of pre-allocating `payload_len` bytes so a
    // peer that sends only the 5-byte header (auth has not yet run) cannot pin
    // up to `max_payload_bytes` of physical memory per connection.
    let mut payload = Vec::with_capacity(payload_len.min(64 * 1024));
    let mut chunk = [0u8; 8192];
    while payload.len() < payload_len {
        let want = (payload_len - payload.len()).min(chunk.len());
        let n = stream
            .read(&mut chunk[..want])
            .await
            .map_err(|e| DbError::protocol(format!("read payload: {e}")))?;
        if n == 0 {
            return Err(DbError::protocol("payload truncated"));
        }
        payload.extend_from_slice(&chunk[..n]);
    }

    let envelope: TransportEnvelope = serde_json::from_slice(&payload)
        .map_err(|e| DbError::protocol(format!("deserialize envelope: {e}")))?;
    validate_msg_type_matches_payload(msg_type, &envelope.payload)?;
    Ok(envelope)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_cancel_envelope() -> TransportEnvelope {
        TransportEnvelope {
            version: PROTOCOL_VERSION,
            auth_token: "test-token".to_owned(),
            payload: TransportPayload::CancelFragment(CancelRequest {
                request_id: 42,
                cancel_key: 0,
            }),
        }
    }

    fn make_error_response_envelope() -> TransportEnvelope {
        TransportEnvelope {
            version: PROTOCOL_VERSION,
            auth_token: String::new(),
            payload: TransportPayload::FragmentResult(FragmentResponse::Error {
                request_id: 99,
                message: "something went wrong".to_owned(),
                sql_state: Some("XX000".to_owned()),
            }),
        }
    }

    fn make_cancel_ack_envelope() -> TransportEnvelope {
        TransportEnvelope {
            version: PROTOCOL_VERSION,
            auth_token: String::new(),
            payload: TransportPayload::FragmentResult(FragmentResponse::CancelAck {
                request_id: 42,
            }),
        }
    }

    fn make_success_envelope() -> TransportEnvelope {
        TransportEnvelope {
            version: PROTOCOL_VERSION,
            auth_token: "tok".to_owned(),
            payload: TransportPayload::FragmentResult(FragmentResponse::Success {
                request_id: 1,
                result: ExecutionResult::command("SELECT"),
            }),
        }
    }

    fn make_execute_fragment_request() -> FragmentRequest {
        FragmentRequest {
            request_id: 1,
            plan: aiondb_plan::PhysicalPlan::ProjectOnce {
                outputs: vec![],
                filter: None,
                order_by: vec![],
                limit: None,
                offset: None,
                distinct: false,
                distinct_on: vec![],
            },
            txn_id: 10,
            isolation: "read committed".to_owned(),
            max_result_rows: 1000,
            max_result_bytes: 1_048_576,
            max_memory_bytes: 10_485_760,
            max_temp_bytes: 5_242_880,
            snapshot: Some(FragmentSnapshot {
                xmin: 1,
                xmax: 4,
                active: vec![2],
            }),
            deadline_epoch_ms: None,
            shard_id: None,
            cancel_key: 0,
        }
    }

    // ---------------------------------------------------------------
    // encode / decode round-trips
    // ---------------------------------------------------------------

    #[test]
    fn roundtrip_cancel_request() {
        let envelope = make_cancel_envelope();
        let bytes = encode_envelope(&envelope).unwrap();

        // First byte is msg_type for CancelFragment.
        assert_eq!(bytes[0], MSG_CANCEL_FRAGMENT);

        let (decoded, consumed) = decode_envelope(&bytes).unwrap();
        assert_eq!(consumed, bytes.len());
        assert_eq!(decoded.version, PROTOCOL_VERSION);
        assert_eq!(decoded.auth_token, "test-token");

        if let TransportPayload::CancelFragment(req) = &decoded.payload {
            assert_eq!(req.request_id, 42);
        } else {
            panic!("expected CancelFragment payload");
        }
    }

    #[test]
    fn roundtrip_error_response() {
        let envelope = make_error_response_envelope();
        let bytes = encode_envelope(&envelope).unwrap();
        assert_eq!(bytes[0], MSG_FRAGMENT_RESULT);

        let (decoded, consumed) = decode_envelope(&bytes).unwrap();
        assert_eq!(consumed, bytes.len());

        if let TransportPayload::FragmentResult(FragmentResponse::Error {
            request_id,
            message,
            sql_state,
        }) = &decoded.payload
        {
            assert_eq!(*request_id, 99);
            assert_eq!(message, "something went wrong");
            assert_eq!(sql_state.as_deref(), Some("XX000"));
        } else {
            panic!("expected FragmentResult::Error payload");
        }
    }

    #[test]
    fn roundtrip_cancel_ack() {
        let envelope = make_cancel_ack_envelope();
        let bytes = encode_envelope(&envelope).unwrap();

        let (decoded, _) = decode_envelope(&bytes).unwrap();
        if let TransportPayload::FragmentResult(FragmentResponse::CancelAck { request_id }) =
            &decoded.payload
        {
            assert_eq!(*request_id, 42);
        } else {
            panic!("expected CancelAck");
        }
    }

    #[test]
    fn roundtrip_success_response() {
        let envelope = make_success_envelope();
        let bytes = encode_envelope(&envelope).unwrap();
        assert_eq!(bytes[0], MSG_FRAGMENT_RESULT);

        let (decoded, consumed) = decode_envelope(&bytes).unwrap();
        assert_eq!(consumed, bytes.len());

        if let TransportPayload::FragmentResult(FragmentResponse::Success { request_id, result }) =
            &decoded.payload
        {
            assert_eq!(*request_id, 1);
            assert!(matches!(result, ExecutionResult::Command { .. }));
        } else {
            panic!("expected FragmentResult::Success");
        }
    }

    // ---------------------------------------------------------------
    // wire format structure
    // ---------------------------------------------------------------

    #[test]
    fn frame_header_layout() {
        let envelope = make_cancel_envelope();
        let bytes = encode_envelope(&envelope).unwrap();

        // byte 0: msg_type
        assert_eq!(bytes[0], MSG_CANCEL_FRAGMENT);

        // bytes 1..5: payload length as u32 LE
        let payload_len = u32::from_le_bytes([bytes[1], bytes[2], bytes[3], bytes[4]]) as usize;
        assert_eq!(payload_len, bytes.len() - 5);
    }

    #[test]
    fn msg_type_values() {
        assert_eq!(MSG_EXECUTE_FRAGMENT, 0x01);
        assert_eq!(MSG_CANCEL_FRAGMENT, 0x02);
        assert_eq!(MSG_FRAGMENT_RESULT, 0x81);
        assert_eq!(MSG_FRAGMENT_ERROR, 0x82);
        assert_eq!(MSG_CANCEL_ACK, 0x83);
    }

    #[test]
    fn msg_type_for_execute_fragment() {
        let payload = TransportPayload::ExecuteFragment(Box::new(make_execute_fragment_request()));
        assert_eq!(msg_type_for_payload(&payload), MSG_EXECUTE_FRAGMENT);
    }

    #[test]
    fn fragment_request_snapshot_defaults_to_none_for_old_payloads() {
        let request = make_execute_fragment_request();
        let mut json = serde_json::to_value(&request).expect("serialize request");
        json.as_object_mut()
            .expect("request json object")
            .remove("snapshot");

        let decoded: FragmentRequest =
            serde_json::from_value(json).expect("deserialize request without snapshot");
        assert!(decoded.snapshot.is_none());
    }

    #[test]
    fn msg_type_for_cancel_fragment() {
        let payload = TransportPayload::CancelFragment(CancelRequest {
            request_id: 1,
            cancel_key: 0,
        });
        assert_eq!(msg_type_for_payload(&payload), MSG_CANCEL_FRAGMENT);
    }

    #[test]
    fn msg_type_for_fragment_result() {
        let payload =
            TransportPayload::FragmentResult(FragmentResponse::CancelAck { request_id: 1 });
        assert_eq!(msg_type_for_payload(&payload), MSG_FRAGMENT_RESULT);
    }

    #[test]
    fn msg_type_payload_match_accepts_legacy_result_tags() {
        let error_payload = TransportPayload::FragmentResult(FragmentResponse::Error {
            request_id: 1,
            message: "boom".to_owned(),
            sql_state: None,
        });
        assert!(msg_type_matches_payload(MSG_FRAGMENT_ERROR, &error_payload));

        let cancel_payload =
            TransportPayload::FragmentResult(FragmentResponse::CancelAck { request_id: 1 });
        assert!(msg_type_matches_payload(MSG_CANCEL_ACK, &cancel_payload));
    }

    // ---------------------------------------------------------------
    // error cases
    // ---------------------------------------------------------------

    #[test]
    fn decode_too_short() {
        let result = decode_envelope(&[0x01, 0x00]);
        assert!(result.is_err());
    }

    #[test]
    fn decode_unknown_msg_type() {
        let result = decode_envelope(&[0xFF, 0x00, 0x00, 0x00, 0x00]);
        assert!(result.is_err());
    }

    #[test]
    fn decode_incomplete_payload() {
        // Header says 100 bytes of payload, but we only provide 5 bytes total.
        let mut buf = vec![MSG_EXECUTE_FRAGMENT];
        buf.extend_from_slice(&100u32.to_le_bytes());
        let result = decode_envelope(&buf);
        assert!(result.is_err());
    }

    #[test]
    fn decode_invalid_json() {
        let garbage = b"this is not json";
        let mut buf = vec![MSG_EXECUTE_FRAGMENT];
        let len = u32::try_from(garbage.len()).expect("garbage payload length fits into u32");
        buf.extend_from_slice(&len.to_le_bytes());
        buf.extend_from_slice(garbage);

        let result = decode_envelope(&buf);
        assert!(result.is_err());
    }

    #[test]
    fn decode_rejects_msg_type_payload_mismatch() {
        let envelope = make_cancel_envelope();
        let json = serde_json::to_vec(&envelope).expect("serialize cancel envelope");
        let mut buf = vec![MSG_EXECUTE_FRAGMENT];
        let len = u32::try_from(json.len()).expect("payload length fits into u32");
        buf.extend_from_slice(&len.to_le_bytes());
        buf.extend_from_slice(&json);

        let result = decode_envelope(&buf);
        assert!(result.is_err());
    }

    // ---------------------------------------------------------------
    // async round-trip
    // ---------------------------------------------------------------

    #[tokio::test]
    async fn async_roundtrip_cancel() {
        let envelope = make_cancel_envelope();

        let mut buf = Vec::new();
        write_envelope(&mut buf, &envelope).await.unwrap();

        let mut cursor = std::io::Cursor::new(buf);
        let decoded = read_envelope(&mut cursor).await.unwrap();

        assert_eq!(decoded.version, PROTOCOL_VERSION);
        assert_eq!(decoded.auth_token, "test-token");
        if let TransportPayload::CancelFragment(req) = &decoded.payload {
            assert_eq!(req.request_id, 42);
        } else {
            panic!("expected CancelFragment");
        }
    }

    #[tokio::test]
    async fn async_roundtrip_success() {
        let envelope = make_success_envelope();

        let mut buf = Vec::new();
        write_envelope(&mut buf, &envelope).await.unwrap();

        let mut cursor = std::io::Cursor::new(buf);
        let decoded = read_envelope(&mut cursor).await.unwrap();

        if let TransportPayload::FragmentResult(FragmentResponse::Success { request_id, .. }) =
            &decoded.payload
        {
            assert_eq!(*request_id, 1);
        } else {
            panic!("expected Success");
        }
    }

    #[tokio::test]
    async fn async_read_empty_stream_fails() {
        let mut cursor = std::io::Cursor::new(Vec::<u8>::new());
        let result = read_envelope(&mut cursor).await;
        assert!(result.is_err());
    }

    // ---------------------------------------------------------------
    // constants
    // ---------------------------------------------------------------

    #[test]
    fn protocol_version_is_one() {
        assert_eq!(PROTOCOL_VERSION, 1);
    }

    #[test]
    fn max_payload_is_64mib() {
        assert_eq!(MAX_PAYLOAD_BYTES, 64 * 1024 * 1024);
    }

    // ---------------------------------------------------------------
    // FragmentSnapshot::validate
    // ---------------------------------------------------------------

    /// Sane snapshot is accepted.
    #[test]
    fn fragment_snapshot_validate_accepts_well_formed_input() {
        let snapshot = FragmentSnapshot {
            xmin: 10,
            xmax: 20,
            active: vec![11, 15, 19],
        };
        assert!(snapshot.validate().is_ok());
    }

    /// Empty active set is acceptable as long as xmin <= xmax.
    #[test]
    fn fragment_snapshot_validate_accepts_empty_active_set() {
        let snapshot = FragmentSnapshot {
            xmin: 10,
            xmax: 10,
            active: Vec::new(),
        };
        assert!(snapshot.validate().is_ok());
    }

    /// xmin > xmax is rejected. Pre-fix the wire-receiver constructed a
    /// `Snapshot` from this directly and the local MVCC visibility code
    /// would short-circuit in attacker-favourable ways (every xid < xmax
    /// is invisible, every xid >= xmin is invisible — i.e. nothing visible
    /// or, with a tweaked active set, exactly the rows the attacker wants).
    #[test]
    fn fragment_snapshot_validate_rejects_xmin_greater_than_xmax() {
        let snapshot = FragmentSnapshot {
            xmin: 100,
            xmax: 10,
            active: Vec::new(),
        };
        let err = snapshot
            .validate()
            .expect_err("xmin > xmax must be rejected at the wire boundary");
        assert!(err.to_string().contains("xmin"));
    }

    /// Active xid below xmin is rejected (snapshot internal inconsistency).
    #[test]
    fn fragment_snapshot_validate_rejects_active_xid_below_xmin() {
        let snapshot = FragmentSnapshot {
            xmin: 10,
            xmax: 20,
            active: vec![5],
        };
        let err = snapshot
            .validate()
            .expect_err("active xid below xmin must be rejected");
        assert!(err.to_string().contains("active xid"));
    }

    /// Active xid >= xmax is rejected.
    #[test]
    fn fragment_snapshot_validate_rejects_active_xid_above_xmax() {
        let snapshot = FragmentSnapshot {
            xmin: 10,
            xmax: 20,
            active: vec![25],
        };
        let err = snapshot
            .validate()
            .expect_err("active xid >= xmax must be rejected");
        assert!(err.to_string().contains("active xid"));
    }

    /// Active xid set is capped to keep the wire-receiver's validation
    /// walk bounded. Pre-fix a peer could ship `active = [..; usize::MAX]`
    /// to force unbounded heap allocation just to deserialize.
    #[test]
    fn fragment_snapshot_validate_rejects_oversized_active_set() {
        // We don't actually allocate `MAX + 1` u64s here; we craft an
        // `active` of length `MAX + 1` with all-zero placeholders. Memory
        // for this test is bounded by `MAX_FRAGMENT_SNAPSHOT_ACTIVE_XIDS *
        // 8 bytes` ≈ 8 MiB, well under the test budget.
        let active = vec![0u64; MAX_FRAGMENT_SNAPSHOT_ACTIVE_XIDS + 1];
        let snapshot = FragmentSnapshot {
            xmin: 0,
            xmax: u64::MAX,
            active,
        };
        let err = snapshot
            .validate()
            .expect_err("oversized active set must be rejected");
        assert!(err.to_string().contains("too large"));
    }

    // -----------------------------------------------------------------
    // V2-05 — pre-auth payload cap
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn v2_05_read_envelope_with_limit_rejects_oversized_header() {
        // Build a header that advertises a payload one byte over the
        // pre-auth cap. The reader must reject it at the header check
        // without consuming any payload bytes.
        let payload_len = MAX_PRE_AUTH_PAYLOAD_BYTES + 1;
        let mut bytes = vec![MSG_EXECUTE_FRAGMENT];
        bytes.extend_from_slice(&(payload_len as u32).to_le_bytes());
        // No payload bytes appended on purpose : the reader must
        // refuse before attempting to read them.
        let mut cursor = std::io::Cursor::new(bytes);
        let err = read_envelope_with_limit(&mut cursor, MAX_PRE_AUTH_PAYLOAD_BYTES)
            .await
            .expect_err("oversized pre-auth envelope must be rejected");
        let msg = err.to_string();
        assert!(
            msg.contains("payload too large"),
            "expected too-large error, got: {msg}"
        );
    }

    #[tokio::test]
    async fn v2_05_read_envelope_with_limit_accepts_small_envelope() {
        // A short, well-formed cancel envelope must pass under the
        // tight pre-auth cap so the auth handshake can complete.
        let envelope = make_cancel_envelope();
        let bytes = encode_envelope(&envelope).unwrap();
        assert!(bytes.len() <= MAX_PRE_AUTH_PAYLOAD_BYTES);
        let mut cursor = std::io::Cursor::new(bytes);
        let decoded = read_envelope_with_limit(&mut cursor, MAX_PRE_AUTH_PAYLOAD_BYTES)
            .await
            .expect("small envelope must pass pre-auth cap");
        assert_eq!(decoded.version, PROTOCOL_VERSION);
    }
}
