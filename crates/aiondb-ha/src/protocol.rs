#![allow(
    clippy::cast_possible_truncation,
    clippy::doc_markdown,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::must_use_candidate,
    clippy::return_self_not_must_use
)]

use std::fmt;
use std::time::SystemTime;

use aiondb_core::{DbError, DbResult};
use hmac::{Hmac, Mac};
use sha2::Sha256;

const TAG_HEARTBEAT: u8 = 0x10;
const TAG_HEARTBEAT_ACK: u8 = 0x11;
const TAG_VOTE_REQUEST: u8 = 0x12;
const TAG_VOTE_RESPONSE: u8 = 0x13;
const TAG_PROMOTE_NOTIFY: u8 = 0x14;
const TAG_DEMOTE_REQUEST: u8 = 0x15;
const TAG_APPEND_ENTRIES: u8 = 0x16;
const TAG_APPEND_ENTRIES_RESPONSE: u8 = 0x17;

const MAX_PAYLOAD: usize = 8 * 1024 * 1024;
const MAX_NODE_ADDRESS_BYTES: usize = 512;

/// HMAC-SHA256 tag length.
const HMAC_LEN: usize = 32;

// ---------------------------------------------------------------------------
// NodeId
// ---------------------------------------------------------------------------

/// Unique identifier for a cluster node.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Ord, PartialOrd)]
pub struct NodeId(u64);

impl NodeId {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "node-{}", self.0)
    }
}

// ---------------------------------------------------------------------------
// Epoch
// ---------------------------------------------------------------------------

/// Monotonically increasing election term.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq, Ord, PartialOrd)]
pub struct Epoch(u64);

impl Epoch {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u64 {
        self.0
    }

    /// Return the next epoch (saturating).
    pub const fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }
}

impl fmt::Display for Epoch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "epoch-{}", self.0)
    }
}

// ---------------------------------------------------------------------------
// NodeRole
// ---------------------------------------------------------------------------

/// Role of a node within the cluster.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NodeRole {
    Standalone,
    Primary,
    Replica,
}

impl NodeRole {
    pub const fn to_u8(self) -> u8 {
        match self {
            Self::Standalone => 0,
            Self::Primary => 1,
            Self::Replica => 2,
        }
    }

    pub const fn from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::Standalone),
            1 => Some(Self::Primary),
            2 => Some(Self::Replica),
            _ => None,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Standalone => "standalone",
            Self::Primary => "primary",
            Self::Replica => "replica",
        }
    }
}

// ---------------------------------------------------------------------------
// HaMessage
// ---------------------------------------------------------------------------

/// Inter-node HA protocol message.
#[derive(Clone, Debug, PartialEq)]
pub enum HaMessage {
    Heartbeat {
        epoch: Epoch,
        node_id: NodeId,
        wal_lsn: u64,
        role: NodeRole,
        timestamp_us: u64,
    },
    HeartbeatAck {
        epoch: Epoch,
        node_id: NodeId,
        wal_lsn: u64,
    },
    VoteRequest {
        epoch: Epoch,
        candidate_id: NodeId,
        last_lsn: u64,
    },
    VoteResponse {
        epoch: Epoch,
        voter_id: NodeId,
        granted: bool,
        voter_lsn: u64,
    },
    PromoteNotify {
        epoch: Epoch,
        new_primary_id: NodeId,
        new_primary_addr: String,
    },
    DemoteRequest {
        epoch: Epoch,
        target_id: NodeId,
    },
    /// Raft `AppendEntries` RPC (serialized as JSON payload).
    AppendEntries {
        /// JSON-serialized `AppendEntriesRequest`.
        payload: Vec<u8>,
    },
    /// Raft `AppendEntries` response (serialized as JSON payload).
    AppendEntriesResponse {
        /// JSON-serialized `AppendEntriesResponse`.
        payload: Vec<u8>,
    },
}

impl HaMessage {
    /// Encode this message into a wire-format byte buffer.
    ///
    /// Format: `tag(u8) | payload_len(u32 LE) | payload`.
    pub fn encode(&self) -> DbResult<Vec<u8>> {
        let mut buf = Vec::new();
        match self {
            Self::Heartbeat {
                epoch,
                node_id,
                wal_lsn,
                role,
                timestamp_us,
            } => {
                let payload_len: u32 = 8 + 8 + 8 + 1 + 8;
                buf.push(TAG_HEARTBEAT);
                buf.extend_from_slice(&payload_len.to_le_bytes());
                buf.extend_from_slice(&epoch.get().to_le_bytes());
                buf.extend_from_slice(&node_id.get().to_le_bytes());
                buf.extend_from_slice(&wal_lsn.to_le_bytes());
                buf.push(role.to_u8());
                buf.extend_from_slice(&timestamp_us.to_le_bytes());
            }
            Self::HeartbeatAck {
                epoch,
                node_id,
                wal_lsn,
            } => {
                let payload_len: u32 = 8 + 8 + 8;
                buf.push(TAG_HEARTBEAT_ACK);
                buf.extend_from_slice(&payload_len.to_le_bytes());
                buf.extend_from_slice(&epoch.get().to_le_bytes());
                buf.extend_from_slice(&node_id.get().to_le_bytes());
                buf.extend_from_slice(&wal_lsn.to_le_bytes());
            }
            Self::VoteRequest {
                epoch,
                candidate_id,
                last_lsn,
            } => {
                let payload_len: u32 = 8 + 8 + 8;
                buf.push(TAG_VOTE_REQUEST);
                buf.extend_from_slice(&payload_len.to_le_bytes());
                buf.extend_from_slice(&epoch.get().to_le_bytes());
                buf.extend_from_slice(&candidate_id.get().to_le_bytes());
                buf.extend_from_slice(&last_lsn.to_le_bytes());
            }
            Self::VoteResponse {
                epoch,
                voter_id,
                granted,
                voter_lsn,
            } => {
                let payload_len: u32 = 8 + 8 + 1 + 8;
                buf.push(TAG_VOTE_RESPONSE);
                buf.extend_from_slice(&payload_len.to_le_bytes());
                buf.extend_from_slice(&epoch.get().to_le_bytes());
                buf.extend_from_slice(&voter_id.get().to_le_bytes());
                buf.push(u8::from(*granted));
                buf.extend_from_slice(&voter_lsn.to_le_bytes());
            }
            Self::PromoteNotify {
                epoch,
                new_primary_id,
                new_primary_addr,
            } => {
                let addr_bytes = new_primary_addr.as_bytes();
                if addr_bytes.len() > MAX_NODE_ADDRESS_BYTES {
                    return Err(DbError::internal(format!(
                        "HA PromoteNotify address too large ({} bytes, max {MAX_NODE_ADDRESS_BYTES})",
                        addr_bytes.len()
                    )));
                }
                let addr_len_u32 = u32::try_from(addr_bytes.len()).map_err(|_| {
                    DbError::internal("HA PromoteNotify address exceeds u32 frame limit")
                })?;
                let payload_len =
                    checked_outbound_payload_len("HA PromoteNotify", &[8, 8, 4, addr_bytes.len()])?;
                buf.push(TAG_PROMOTE_NOTIFY);
                buf.extend_from_slice(&payload_len.to_le_bytes());
                buf.extend_from_slice(&epoch.get().to_le_bytes());
                buf.extend_from_slice(&new_primary_id.get().to_le_bytes());
                buf.extend_from_slice(&addr_len_u32.to_le_bytes());
                buf.extend_from_slice(addr_bytes);
            }
            Self::DemoteRequest { epoch, target_id } => {
                let payload_len: u32 = 8 + 8;
                buf.push(TAG_DEMOTE_REQUEST);
                buf.extend_from_slice(&payload_len.to_le_bytes());
                buf.extend_from_slice(&epoch.get().to_le_bytes());
                buf.extend_from_slice(&target_id.get().to_le_bytes());
            }
            Self::AppendEntries { payload } => {
                let payload_len = outbound_payload_len("HA AppendEntries", payload.len())?;
                buf.push(TAG_APPEND_ENTRIES);
                buf.extend_from_slice(&payload_len.to_le_bytes());
                buf.extend_from_slice(payload);
            }
            Self::AppendEntriesResponse { payload } => {
                let payload_len = outbound_payload_len("HA AppendEntriesResponse", payload.len())?;
                buf.push(TAG_APPEND_ENTRIES_RESPONSE);
                buf.extend_from_slice(&payload_len.to_le_bytes());
                buf.extend_from_slice(payload);
            }
        }
        Ok(buf)
    }

    /// Decode a message from a wire-format byte buffer.
    ///
    /// Returns the decoded message and the number of bytes consumed.
    pub fn decode(data: &[u8]) -> DbResult<(Self, usize)> {
        if data.len() < 5 {
            return Err(DbError::internal("HA message too short for header"));
        }
        let tag = data[0];
        let payload_len = u32::from_le_bytes([data[1], data[2], data[3], data[4]]) as usize;
        if payload_len > MAX_PAYLOAD {
            return Err(DbError::internal("HA message payload exceeds 8 MiB limit"));
        }
        let total = 5 + payload_len;
        if data.len() < total {
            return Err(DbError::internal("HA message truncated"));
        }
        let payload = &data[5..total];
        let msg = match tag {
            TAG_HEARTBEAT => {
                ensure_payload_len(payload, 33)?;
                Self::Heartbeat {
                    epoch: Epoch::new(read_u64(payload, 0)),
                    node_id: NodeId::new(read_u64(payload, 8)),
                    wal_lsn: read_u64(payload, 16),
                    role: NodeRole::from_u8(payload[24])
                        .ok_or_else(|| DbError::internal("invalid node role in heartbeat"))?,
                    timestamp_us: read_u64(payload, 25),
                }
            }
            TAG_HEARTBEAT_ACK => {
                ensure_payload_len(payload, 24)?;
                Self::HeartbeatAck {
                    epoch: Epoch::new(read_u64(payload, 0)),
                    node_id: NodeId::new(read_u64(payload, 8)),
                    wal_lsn: read_u64(payload, 16),
                }
            }
            TAG_VOTE_REQUEST => {
                ensure_payload_len(payload, 24)?;
                Self::VoteRequest {
                    epoch: Epoch::new(read_u64(payload, 0)),
                    candidate_id: NodeId::new(read_u64(payload, 8)),
                    last_lsn: read_u64(payload, 16),
                }
            }
            TAG_VOTE_RESPONSE => {
                ensure_payload_len(payload, 25)?;
                let granted = match payload[16] {
                    0 => false,
                    1 => true,
                    other => {
                        return Err(DbError::internal(format!(
                            "invalid VoteResponse granted flag {other}"
                        )));
                    }
                };
                Self::VoteResponse {
                    epoch: Epoch::new(read_u64(payload, 0)),
                    voter_id: NodeId::new(read_u64(payload, 8)),
                    granted,
                    voter_lsn: read_u64(payload, 17),
                }
            }
            TAG_PROMOTE_NOTIFY => {
                if payload.len() < 20 {
                    return Err(DbError::internal("PromoteNotify payload too short"));
                }
                let epoch = Epoch::new(read_u64(payload, 0));
                let new_primary_id = NodeId::new(read_u64(payload, 8));
                let addr_len =
                    u32::from_le_bytes([payload[16], payload[17], payload[18], payload[19]])
                        as usize;
                if addr_len > MAX_NODE_ADDRESS_BYTES {
                    return Err(DbError::internal(format!(
                        "PromoteNotify address exceeds maximum length of {MAX_NODE_ADDRESS_BYTES} bytes"
                    )));
                }
                if payload.len() < 20 + addr_len {
                    return Err(DbError::internal("PromoteNotify address truncated"));
                }
                if payload.len() != 20 + addr_len {
                    return Err(DbError::internal(
                        "PromoteNotify payload contains trailing bytes",
                    ));
                }
                let new_primary_addr = String::from_utf8(payload[20..20 + addr_len].to_vec())
                    .map_err(|_| DbError::internal("PromoteNotify address is not valid UTF-8"))?;
                Self::PromoteNotify {
                    epoch,
                    new_primary_id,
                    new_primary_addr,
                }
            }
            TAG_DEMOTE_REQUEST => {
                ensure_payload_len(payload, 16)?;
                Self::DemoteRequest {
                    epoch: Epoch::new(read_u64(payload, 0)),
                    target_id: NodeId::new(read_u64(payload, 8)),
                }
            }
            TAG_APPEND_ENTRIES => Self::AppendEntries {
                payload: payload.to_vec(),
            },
            TAG_APPEND_ENTRIES_RESPONSE => Self::AppendEntriesResponse {
                payload: payload.to_vec(),
            },
            _ => {
                return Err(DbError::internal(format!(
                    "unknown HA message tag: 0x{tag:02X}"
                )));
            }
        };
        Ok((msg, total))
    }
}

/// Return the current wall-clock timestamp in microseconds since the Unix epoch.
pub fn current_timestamp_us() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_micros()).unwrap_or(u64::MAX))
}

fn read_u64(buf: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes([
        buf[offset],
        buf[offset + 1],
        buf[offset + 2],
        buf[offset + 3],
        buf[offset + 4],
        buf[offset + 5],
        buf[offset + 6],
        buf[offset + 7],
    ])
}

/// Minimum HA HMAC key length, in bytes.
///
/// HMAC-SHA256 technically accepts arbitrary key lengths (including the empty
/// string), but a key shorter than the SHA-256 output is the practical
/// security floor — anything below 32 bytes adds no meaningful resistance
/// against forgery. An empty key in particular makes the authenticated frame
/// trivially forgeable by any peer that knows the protocol uses an empty key.
/// We refuse to produce or verify HMACs below this floor and the caller must
/// handle the resulting `false`/zero-tag explicitly.
const MIN_HA_HMAC_KEY_BYTES: usize = 32;

/// Compute HMAC-SHA256 of message bytes using the shared secret.
///
/// Returns an all-zero tag if the secret is shorter than
/// [`MIN_HA_HMAC_KEY_BYTES`]; the matching [`verify_hmac`] also rejects
/// short secrets, so a misconfigured pair fails closed instead of silently
/// authenticating with a guessable key.
pub fn compute_hmac(secret: &[u8], data: &[u8]) -> Vec<u8> {
    if secret.len() < MIN_HA_HMAC_KEY_BYTES {
        return vec![0; HMAC_LEN];
    }
    let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(secret) else {
        return vec![0; HMAC_LEN];
    };
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

/// Verify HMAC-SHA256 of message bytes.
///
/// Returns `false` for any secret shorter than [`MIN_HA_HMAC_KEY_BYTES`] so
/// an empty/short cluster secret cannot accept attacker-forged frames.
pub fn verify_hmac(secret: &[u8], data: &[u8], expected_hmac: &[u8]) -> bool {
    if secret.len() < MIN_HA_HMAC_KEY_BYTES {
        return false;
    }
    let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(secret) else {
        return false;
    };
    mac.update(data);
    mac.verify_slice(expected_hmac).is_ok()
}

/// Encode an HA message with HMAC authentication.
///
/// Wire format: `hmac(32 bytes) | msg_type(u8) | payload_len(u32 LE) | payload`
///
/// The HMAC covers the entire message after the HMAC field itself.
pub fn encode_authenticated(msg: &HaMessage, secret: &[u8]) -> DbResult<Vec<u8>> {
    let raw = msg.encode()?;
    let hmac = compute_hmac(secret, &raw);
    let mut buf = Vec::with_capacity(HMAC_LEN + raw.len());
    buf.extend_from_slice(&hmac);
    buf.extend_from_slice(&raw);
    Ok(buf)
}

/// Decode an HMAC-authenticated HA message.
///
/// Returns `(message, bytes_consumed)` or an error if HMAC verification fails.
pub fn decode_authenticated(data: &[u8], secret: &[u8]) -> DbResult<(HaMessage, usize)> {
    if data.len() < HMAC_LEN {
        return Err(DbError::internal(
            "HA protocol: authenticated message too short for HMAC",
        ));
    }
    let received_hmac = &data[..HMAC_LEN];
    let message_data = &data[HMAC_LEN..];
    if !verify_hmac(secret, message_data, received_hmac) {
        return Err(DbError::invalid_authorization(
            "HA protocol: HMAC verification failed — message rejected",
        ));
    }
    let (msg, consumed) = HaMessage::decode(message_data)?;
    Ok((msg, HMAC_LEN + consumed))
}

fn ensure_payload_len(payload: &[u8], expected: usize) -> DbResult<()> {
    if payload.len() != expected {
        return Err(DbError::internal(format!(
            "HA payload length mismatch: expected {expected} bytes, got {}",
            payload.len()
        )));
    }
    Ok(())
}

fn checked_outbound_payload_len(kind: &str, parts: &[usize]) -> DbResult<u32> {
    let mut total = 0usize;
    for part in parts {
        total = total
            .checked_add(*part)
            .ok_or_else(|| DbError::internal(format!("{kind} payload length overflowed usize")))?;
    }
    outbound_payload_len(kind, total)
}

fn outbound_payload_len(kind: &str, payload_len: usize) -> DbResult<u32> {
    if payload_len > MAX_PAYLOAD {
        return Err(DbError::internal(format!(
            "{kind} payload too large ({payload_len} bytes, max {MAX_PAYLOAD})"
        )));
    }
    u32::try_from(payload_len).map_err(|_| {
        DbError::internal(format!(
            "{kind} payload exceeds u32 frame length ({payload_len} bytes)"
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_id_display() {
        assert_eq!(NodeId::new(42).to_string(), "node-42");
    }

    #[test]
    fn epoch_next() {
        assert_eq!(Epoch::new(5).next(), Epoch::new(6));
    }

    #[test]
    fn epoch_next_saturates() {
        assert_eq!(Epoch::new(u64::MAX).next(), Epoch::new(u64::MAX));
    }

    #[test]
    fn epoch_display() {
        assert_eq!(Epoch::new(3).to_string(), "epoch-3");
    }

    #[test]
    fn node_role_round_trip() {
        for role in [NodeRole::Standalone, NodeRole::Primary, NodeRole::Replica] {
            assert_eq!(NodeRole::from_u8(role.to_u8()), Some(role));
        }
    }

    #[test]
    fn node_role_invalid() {
        assert_eq!(NodeRole::from_u8(99), None);
    }

    #[test]
    fn heartbeat_round_trip() {
        let msg = HaMessage::Heartbeat {
            epoch: Epoch::new(7),
            node_id: NodeId::new(1),
            wal_lsn: 12345,
            role: NodeRole::Primary,
            timestamp_us: 999_000,
        };
        let encoded = msg.encode().expect("encode HA message");
        let (decoded, consumed) = HaMessage::decode(&encoded).unwrap();
        assert_eq!(consumed, encoded.len());
        assert_eq!(decoded, msg);
    }

    #[test]
    fn heartbeat_ack_round_trip() {
        let msg = HaMessage::HeartbeatAck {
            epoch: Epoch::new(3),
            node_id: NodeId::new(2),
            wal_lsn: 500,
        };
        let encoded = msg.encode().expect("encode HA message");
        let (decoded, consumed) = HaMessage::decode(&encoded).unwrap();
        assert_eq!(consumed, encoded.len());
        assert_eq!(decoded, msg);
    }

    #[test]
    fn vote_request_round_trip() {
        let msg = HaMessage::VoteRequest {
            epoch: Epoch::new(10),
            candidate_id: NodeId::new(3),
            last_lsn: 9999,
        };
        let encoded = msg.encode().expect("encode HA message");
        let (decoded, consumed) = HaMessage::decode(&encoded).unwrap();
        assert_eq!(consumed, encoded.len());
        assert_eq!(decoded, msg);
    }

    #[test]
    fn vote_response_round_trip() {
        let msg = HaMessage::VoteResponse {
            epoch: Epoch::new(10),
            voter_id: NodeId::new(2),
            granted: true,
            voter_lsn: 8888,
        };
        let encoded = msg.encode().expect("encode HA message");
        let (decoded, consumed) = HaMessage::decode(&encoded).unwrap();
        assert_eq!(consumed, encoded.len());
        assert_eq!(decoded, msg);
    }

    #[test]
    fn vote_response_denied_round_trip() {
        let msg = HaMessage::VoteResponse {
            epoch: Epoch::new(5),
            voter_id: NodeId::new(7),
            granted: false,
            voter_lsn: 100,
        };
        let encoded = msg.encode().expect("encode HA message");
        let (decoded, _) = HaMessage::decode(&encoded).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn vote_response_rejects_invalid_granted_flag() {
        let mut encoded = HaMessage::VoteResponse {
            epoch: Epoch::new(5),
            voter_id: NodeId::new(7),
            granted: false,
            voter_lsn: 100,
        }
        .encode()
        .expect("encode HA message");
        encoded[5 + 16] = 2;

        let error = HaMessage::decode(&encoded).expect_err("invalid bool flag must be rejected");
        assert!(error.to_string().contains("invalid VoteResponse"));
    }

    #[test]
    fn promote_notify_round_trip() {
        let msg = HaMessage::PromoteNotify {
            epoch: Epoch::new(20),
            new_primary_id: NodeId::new(5),
            new_primary_addr: "192.168.1.10:5433".to_string(),
        };
        let encoded = msg.encode().expect("encode HA message");
        let (decoded, consumed) = HaMessage::decode(&encoded).unwrap();
        assert_eq!(consumed, encoded.len());
        assert_eq!(decoded, msg);
    }

    #[test]
    fn promote_notify_empty_addr() {
        let msg = HaMessage::PromoteNotify {
            epoch: Epoch::new(1),
            new_primary_id: NodeId::new(1),
            new_primary_addr: String::new(),
        };
        let encoded = msg.encode().expect("encode HA message");
        let (decoded, _) = HaMessage::decode(&encoded).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn promote_notify_rejects_oversized_address_on_encode() {
        let msg = HaMessage::PromoteNotify {
            epoch: Epoch::new(1),
            new_primary_id: NodeId::new(1),
            new_primary_addr: "a".repeat(MAX_NODE_ADDRESS_BYTES + 1),
        };

        let error = msg
            .encode()
            .expect_err("oversized promote address must be rejected");
        assert!(error.to_string().contains("address too large"));
    }

    #[test]
    fn demote_request_round_trip() {
        let msg = HaMessage::DemoteRequest {
            epoch: Epoch::new(15),
            target_id: NodeId::new(4),
        };
        let encoded = msg.encode().expect("encode HA message");
        let (decoded, consumed) = HaMessage::decode(&encoded).unwrap();
        assert_eq!(consumed, encoded.len());
        assert_eq!(decoded, msg);
    }

    #[test]
    fn encode_rejects_outbound_payload_above_decode_limit() {
        let msg = HaMessage::AppendEntries {
            payload: vec![0; MAX_PAYLOAD + 1],
        };

        let error = msg
            .encode()
            .expect_err("outbound HA payload should respect decoder limit");
        assert!(error.to_string().contains("payload too large"), "{error}");
    }

    #[test]
    fn decode_too_short() {
        assert!(HaMessage::decode(&[0x10, 0, 0]).is_err());
    }

    #[test]
    fn decode_unknown_tag() {
        let data = [0xFF, 0, 0, 0, 0];
        assert!(HaMessage::decode(&data).is_err());
    }

    #[test]
    fn decode_truncated_payload() {
        let mut encoded = HaMessage::DemoteRequest {
            epoch: Epoch::new(1),
            target_id: NodeId::new(1),
        }
        .encode()
        .expect("encode HA message");
        encoded.truncate(encoded.len() - 1);
        assert!(HaMessage::decode(&encoded).is_err());
    }

    #[test]
    fn decode_rejects_fixed_payload_trailing_bytes() {
        let mut encoded = HaMessage::DemoteRequest {
            epoch: Epoch::new(1),
            target_id: NodeId::new(1),
        }
        .encode()
        .expect("encode HA message");
        let payload_len = u32::from_le_bytes([encoded[1], encoded[2], encoded[3], encoded[4]]);
        encoded[1..5].copy_from_slice(&(payload_len + 1).to_le_bytes());
        encoded.push(0xAA);

        let err =
            HaMessage::decode(&encoded).expect_err("fixed-size HA frames must be exact length");
        assert!(err.to_string().contains("length mismatch"), "{err}");
    }

    #[test]
    fn decode_rejects_promote_notify_trailing_bytes() {
        let mut encoded = HaMessage::PromoteNotify {
            epoch: Epoch::new(20),
            new_primary_id: NodeId::new(5),
            new_primary_addr: "node5:5433".to_owned(),
        }
        .encode()
        .expect("encode HA message");
        let payload_len = u32::from_le_bytes([encoded[1], encoded[2], encoded[3], encoded[4]]);
        encoded[1..5].copy_from_slice(&(payload_len + 1).to_le_bytes());
        encoded.push(0xAA);

        let err = HaMessage::decode(&encoded)
            .expect_err("PromoteNotify must reject bytes outside the declared address");
        assert!(err.to_string().contains("trailing bytes"), "{err}");
    }

    #[test]
    fn decode_rejects_promote_notify_oversized_address() {
        let addr_len = MAX_NODE_ADDRESS_BYTES + 1;
        let payload_len = 20 + addr_len;
        let mut encoded = Vec::with_capacity(5 + payload_len);
        encoded.push(TAG_PROMOTE_NOTIFY);
        encoded.extend_from_slice(&(payload_len as u32).to_le_bytes());
        encoded.extend_from_slice(&Epoch::new(1).get().to_le_bytes());
        encoded.extend_from_slice(&NodeId::new(1).get().to_le_bytes());
        encoded.extend_from_slice(&(addr_len as u32).to_le_bytes());
        encoded.extend_from_slice(&vec![b'a'; addr_len]);

        let err = HaMessage::decode(&encoded)
            .expect_err("oversized PromoteNotify address must be rejected");
        assert!(err.to_string().contains("maximum length"), "{err}");
    }

    #[test]
    fn current_timestamp_us_nonzero() {
        assert!(current_timestamp_us() > 0);
    }

    /// Helper: 32-byte secret (the minimum accepted length).
    const TEST_SECRET_A: &[u8; 32] = b"cluster-secret-key-padded--32byt";
    const TEST_SECRET_B: &[u8; 32] = b"different-secret-padded----32byt";

    #[test]
    fn authenticated_encode_decode_roundtrip() {
        let msg = HaMessage::Heartbeat {
            epoch: Epoch::new(5),
            node_id: NodeId::new(1),
            wal_lsn: 1000,
            role: NodeRole::Primary,
            timestamp_us: 12345,
        };
        let encoded =
            encode_authenticated(&msg, TEST_SECRET_A).expect("encode authenticated HA message");
        assert!(encoded.len() > HMAC_LEN);
        let (decoded, consumed) = decode_authenticated(&encoded, TEST_SECRET_A).unwrap();
        assert_eq!(decoded, msg);
        assert_eq!(consumed, encoded.len());
    }

    #[test]
    fn authenticated_decode_rejects_wrong_secret() {
        let msg = HaMessage::VoteRequest {
            epoch: Epoch::new(3),
            candidate_id: NodeId::new(2),
            last_lsn: 500,
        };
        let encoded =
            encode_authenticated(&msg, TEST_SECRET_A).expect("encode authenticated HA message");
        let err = decode_authenticated(&encoded, TEST_SECRET_B)
            .expect_err("wrong secret must be rejected");
        assert!(err.to_string().contains("HMAC verification failed"));
    }

    #[test]
    fn authenticated_decode_rejects_tampered_message() {
        let msg = HaMessage::PromoteNotify {
            epoch: Epoch::new(10),
            new_primary_id: NodeId::new(3),
            new_primary_addr: "node3:5433".to_owned(),
        };
        let mut encoded =
            encode_authenticated(&msg, TEST_SECRET_A).expect("encode authenticated HA message");
        // Tamper with a byte in the message body (after HMAC)
        if let Some(byte) = encoded.get_mut(HMAC_LEN + 6) {
            *byte ^= 0xFF;
        }
        assert!(decode_authenticated(&encoded, TEST_SECRET_A).is_err());
    }

    #[test]
    fn authenticated_decode_rejects_too_short() {
        let err = decode_authenticated(&[0u8; 10], TEST_SECRET_A).expect_err("too short must fail");
        assert!(err.to_string().contains("too short"));
    }

    /// Regression: an empty cluster secret must not produce a valid HMAC, so
    /// encode/decode round-tripping with an empty secret on both sides must
    /// fail. Otherwise, any peer who knows the protocol uses an empty key
    /// could forge HA frames against a misconfigured cluster.
    #[test]
    fn authenticated_encode_decode_rejects_empty_secret() {
        let msg = HaMessage::DemoteRequest {
            epoch: Epoch::new(1),
            target_id: NodeId::new(2),
        };
        let encoded = encode_authenticated(&msg, b"").expect("encode authenticated HA message");
        // The HMAC must be the zero placeholder (not a real HMAC over the
        // body), and decode must refuse it.
        assert_eq!(&encoded[..HMAC_LEN], &[0u8; HMAC_LEN]);
        let err = decode_authenticated(&encoded, b"")
            .expect_err("empty-secret authenticated frames must be rejected");
        assert!(err.to_string().contains("HMAC verification failed"));
    }

    /// Regression: a short (sub-32-byte) cluster secret must also be refused
    /// on both sides, even when the same short key is used.
    #[test]
    fn authenticated_encode_decode_rejects_short_secret() {
        let short = b"only-12-byte"; // 12 bytes < 32-byte floor
        assert!(short.len() < MIN_HA_HMAC_KEY_BYTES);
        let msg = HaMessage::DemoteRequest {
            epoch: Epoch::new(1),
            target_id: NodeId::new(2),
        };
        let encoded = encode_authenticated(&msg, short).expect("encode authenticated HA message");
        let err = decode_authenticated(&encoded, short)
            .expect_err("short-secret authenticated frames must be rejected");
        assert!(err.to_string().contains("HMAC verification failed"));
    }
}
