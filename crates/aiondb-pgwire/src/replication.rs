//! Pgwire replication protocol handler.
//!
//! Handles the `PostgreSQL` streaming replication protocol on top of the
//! pgwire connection layer. Supports:
//!
//! - **Primary side**: `START_REPLICATION` command handling that sets up a
//!   [`WalSender`] and streams `WalData` messages to a connected replica.
//! - **Replica side**: Connection to a primary, receipt of WAL data, and
//!   periodic status updates.
//!
//! # Protocol overview
//!
//! `PostgreSQL`'s streaming replication uses the extended startup protocol
//! with `replication=database` or `replication=true` in the startup
//! parameters. Entering replication mode still requires an engine-level
//! authorization check; `AionDB` currently restricts it to superuser
//! sessions. After authentication and authorization, the replica issues:
//!
//! ```text
//! IDENTIFY_SYSTEM     -- get primary's system identifier and timeline
//! START_REPLICATION   -- begin streaming WAL from a given LSN
//! ```
//!
//! The primary then enters a copy-both mode where it sends `CopyData`
//! messages containing WAL data and keepalives, while the replica sends
//! `CopyData` messages containing standby status updates.
//!
//! A simplified version of this protocol, with `AionDB`'s native
//! replication message format encapsulated in `CopyData` frames.

use std::sync::Arc;
use std::time::Duration;

use aiondb_core::{DbError, DbResult};
use aiondb_wal::replication::{ReplicaRegistry, ReplicationMessage, WalNotifier, WalSender};
use aiondb_wal::Lsn;
use tracing::{debug, error};

/// Interval between keepalive messages when no WAL data is being sent.
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(10);
/// PostgreSQL keeps identifiers below NAMEDATALEN (64 including terminator).
const MAX_REPLICATION_SLOT_NAME_LEN: usize = 63;

/// Handle the primary side of a replication connection.
///
/// This is a long-running loop that:
/// 1. Waits for new WAL data using the [`WalNotifier`]
/// 2. Reads batches of WAL entries via [`WalSender`]
/// 3. Encodes them as [`aiondb_wal::replication::ReplicationMessage::WalData`] and returns the
///    encoded bytes for the caller to send over the wire
///
/// The caller (pgwire connection handler) is responsible for:
/// - Sending the encoded messages as `CopyData` frames
/// - Receiving and dispatching standby status updates
/// - Handling connection termination
pub struct ReplicationStreamHandler {
    sender: WalSender,
    registry: Arc<ReplicaRegistry>,
    notifier: Arc<WalNotifier>,
    last_keepalive: std::time::Instant,
}

impl ReplicationStreamHandler {
    /// Create a new replication stream handler for a replica connection.
    pub fn new(
        sender: WalSender,
        registry: Arc<ReplicaRegistry>,
        notifier: Arc<WalNotifier>,
    ) -> Self {
        Self {
            sender,
            registry,
            notifier,
            last_keepalive: std::time::Instant::now(),
        }
    }

    /// Poll for the next message to send to the replica.
    ///
    /// Returns `Some(encoded_bytes)` when there is a WAL data batch or
    /// keepalive to send, or `None` if the connection should be closed.
    ///
    /// Never blocks: callers should invoke it from their own
    /// event loop after polling the socket.
    pub fn poll_next_message(&mut self) -> DbResult<Option<Vec<u8>>> {
        let last_sent_lsn = self
            .sender
            .last_sent_lsn()
            .unwrap_or_else(|| Lsn::new(self.sender.next_send_lsn().get().saturating_sub(1)));
        let wal_end = self.notifier.current_lsn().max(last_sent_lsn);
        // Try to read a batch of WAL entries.
        match self.sender.next_batch() {
            Ok(Some(msg)) => {
                self.last_keepalive = std::time::Instant::now();
                Ok(Some(msg.encode()?))
            }
            Ok(None) => {
                if self.last_keepalive.elapsed() >= KEEPALIVE_INTERVAL {
                    let keepalive = self.sender.keepalive(wal_end, true);
                    self.last_keepalive = std::time::Instant::now();
                    Ok(Some(keepalive.encode()?))
                } else {
                    Ok(None)
                }
            }
            Err(err) => {
                error!(
                    replica_id = %self.sender.replica_id(),
                    error = %err,
                    "WAL sender error"
                );
                Err(err)
            }
        }
    }

    /// Wait until either new WAL is available or a keepalive becomes due.
    pub async fn wait_for_activity_or_keepalive(&self) {
        let timeout = KEEPALIVE_INTERVAL.saturating_sub(self.last_keepalive.elapsed());
        if timeout.is_zero() {
            return;
        }
        let after_lsn = self
            .sender
            .last_sent_lsn()
            .unwrap_or_else(|| Lsn::new(self.sender.next_send_lsn().get().saturating_sub(1)));
        let _ = self
            .notifier
            .wait_for_new_wal_async(after_lsn, timeout)
            .await;
    }

    /// Process a standby status update received from the replica.
    pub fn handle_status_update(
        &self,
        write_lsn: Lsn,
        flush_lsn: Lsn,
        apply_lsn: Lsn,
    ) -> DbResult<()> {
        let max_reported_lsn = self
            .sender
            .last_sent_lsn()
            .unwrap_or_else(|| Lsn::new(self.sender.next_send_lsn().get().saturating_sub(1)));
        let write_lsn = Lsn::new(write_lsn.get().min(max_reported_lsn.get()));
        let flush_lsn = Lsn::new(flush_lsn.get().min(write_lsn.get()));
        let apply_lsn = Lsn::new(apply_lsn.get().min(flush_lsn.get()));
        debug!(
            replica_id = %self.sender.replica_id(),
            write_lsn = write_lsn.get(),
            flush_lsn = flush_lsn.get(),
            apply_lsn = apply_lsn.get(),
            "received standby status update"
        );
        self.registry
            .update_progress(self.sender.replica_id(), write_lsn, flush_lsn, apply_lsn)
    }

    /// Decode and apply a client `CopyData` frame received in COPY BOTH mode.
    pub fn handle_copy_data(&self, payload: &[u8]) -> DbResult<()> {
        let (message, consumed) = ReplicationMessage::decode(payload)?;
        if consumed != payload.len() {
            return Err(DbError::protocol(
                "replication CopyData frame contains trailing bytes",
            ));
        }

        match message {
            ReplicationMessage::StandbyStatusUpdate {
                write_lsn,
                flush_lsn,
                apply_lsn,
            } => self.handle_status_update(write_lsn, flush_lsn, apply_lsn),
            ReplicationMessage::KeepaliveReply { .. } => Ok(()),
            ReplicationMessage::Keepalive { .. } | ReplicationMessage::WalData { .. } => Err(
                DbError::protocol("unexpected primary-to-standby replication message from client"),
            ),
        }
    }

    /// Return the replica ID being served.
    pub fn replica_id(&self) -> aiondb_wal::replication::ReplicaId {
        self.sender.replica_id()
    }
}

/// Represents an incoming replication command from a replica.
#[derive(Clone, Debug)]
pub enum ReplicationCommand {
    /// `IDENTIFY_SYSTEM` -- replica requests system identification.
    IdentifySystem,
    /// `START_REPLICATION SLOT ... LOGICAL/PHYSICAL ... LSN` -- begin
    /// streaming from the given LSN.
    StartReplication {
        slot_name: Option<String>,
        start_lsn: Lsn,
        timeline: Option<u32>,
    },
    /// `CREATE_REPLICATION_SLOT <slot_name> PHYSICAL [RESERVE_WAL]`.
    CreateReplicationSlot {
        slot_name: String,
        reserve_wal: bool,
    },
    /// `READ_REPLICATION_SLOT <slot_name>`.
    ReadReplicationSlot { slot_name: String },
    /// `TIMELINE_HISTORY [timeline]` -- request timeline history metadata.
    TimelineHistory { timeline: Option<u32> },
    /// `BASE_BACKUP` -- stream the WAL directory so a fresh replica can
    /// bootstrap without manual file copying. AionDB ships its own framing
    /// (see [`aiondb_replication::base_backup`]) rather than PG's tarball.
    BaseBackup,
}

impl ReplicationCommand {
    /// Parse a replication command from a SQL-like string.
    ///
    /// `PostgreSQL` replication commands look like simple queries:
    /// - `IDENTIFY_SYSTEM`
    /// - `START_REPLICATION 0/0`
    /// - `START_REPLICATION SLOT "name" PHYSICAL 0/1A3F`
    /// - `CREATE_REPLICATION_SLOT slot_name PHYSICAL [RESERVE_WAL]`
    /// - `READ_REPLICATION_SLOT slot_name`
    pub fn parse(query: &str) -> Option<Self> {
        let trimmed = query.trim().trim_end_matches(';');
        let upper = trimmed.to_ascii_uppercase();

        if starts_with_replication_keyword(trimmed, &upper, "IDENTIFY_SYSTEM") {
            let parts = tokenize_replication_command(trimmed)?;
            if parts.len() != 1 {
                return None;
            }
            return Some(Self::IdentifySystem);
        }

        if starts_with_replication_keyword(trimmed, &upper, "BASE_BACKUP") {
            let parts = tokenize_replication_command(trimmed)?;
            if parts.len() != 1 {
                return None;
            }
            return Some(Self::BaseBackup);
        }

        if starts_with_replication_keyword(trimmed, &upper, "TIMELINE_HISTORY") {
            let parts = tokenize_replication_command(trimmed)?;
            if parts.len() == 1 {
                return Some(Self::TimelineHistory { timeline: None });
            }
            if parts.len() == 2 {
                let timeline = parts[1].text.parse::<u32>().ok()?;
                if timeline == 0 {
                    return None;
                }
                return Some(Self::TimelineHistory {
                    timeline: Some(timeline),
                });
            }
            return None;
        }

        if starts_with_replication_keyword(trimmed, &upper, "START_REPLICATION") {
            let parts = tokenize_replication_command(trimmed)?;
            if parts.len() < 2 {
                return None;
            }

            // Supported forms:
            // - START_REPLICATION <lsn> [TIMELINE <n>]
            // - START_REPLICATION SLOT "name" PHYSICAL <lsn> [TIMELINE <n>]
            let (slot_name, lsn_idx, options_idx) = if parts.len() >= 5
                && parts[1].text.eq_ignore_ascii_case("SLOT")
                && parts[3].text.eq_ignore_ascii_case("PHYSICAL")
            {
                (Some(parse_slot_name_token(&parts[2])?), 4usize, 5usize)
            } else {
                (None, 1usize, 2usize)
            };

            let lsn_token = parts[lsn_idx]
                .text
                .trim_end_matches(';')
                .trim_end_matches(',');
            let start_lsn = Lsn::from_str_value(lsn_token)?;

            let timeline = if parts.len() > options_idx {
                if parts.len() != options_idx + 2
                    || !parts[options_idx].text.eq_ignore_ascii_case("TIMELINE")
                {
                    return None;
                }
                let timeline_token = parts[options_idx + 1]
                    .text
                    .trim_end_matches(';')
                    .trim_end_matches(',');
                let timeline = timeline_token.parse::<u32>().ok()?;
                if timeline == 0 {
                    return None;
                }
                Some(timeline)
            } else {
                None
            };

            return Some(Self::StartReplication {
                slot_name,
                start_lsn,
                timeline,
            });
        }

        if starts_with_replication_keyword(trimmed, &upper, "CREATE_REPLICATION_SLOT") {
            let parts = tokenize_replication_command(trimmed)?;
            if !(parts.len() == 3 || parts.len() == 4) {
                return None;
            }

            let slot_name = parse_slot_name_token(&parts[1])?;
            if !parts[2].text.eq_ignore_ascii_case("PHYSICAL") {
                return None;
            }
            let reserve_wal = if parts.len() == 4 {
                if !parts[3].text.eq_ignore_ascii_case("RESERVE_WAL") {
                    return None;
                }
                true
            } else {
                false
            };

            return Some(Self::CreateReplicationSlot {
                slot_name,
                reserve_wal,
            });
        }

        if starts_with_replication_keyword(trimmed, &upper, "READ_REPLICATION_SLOT") {
            let parts = tokenize_replication_command(trimmed)?;
            if parts.len() != 2 {
                return None;
            }
            let slot_name = parse_slot_name_token(&parts[1])?;
            return Some(Self::ReadReplicationSlot { slot_name });
        }

        None
    }
}

fn starts_with_replication_keyword(trimmed: &str, upper: &str, keyword: &str) -> bool {
    if !upper.starts_with(keyword) {
        return false;
    }
    match trimmed[keyword.len()..].chars().next() {
        None => true,
        Some(ch) => ch.is_ascii_whitespace(),
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ReplicationToken {
    text: String,
    quoted: bool,
}

fn tokenize_replication_command(sql: &str) -> Option<Vec<ReplicationToken>> {
    let mut tokens = Vec::new();
    let mut cursor = 0usize;
    while cursor < sql.len() {
        while cursor < sql.len() {
            let ch = sql[cursor..].chars().next()?;
            if !ch.is_whitespace() {
                break;
            }
            cursor += ch.len_utf8();
        }
        if cursor >= sql.len() {
            break;
        }

        if sql[cursor..].starts_with('"') {
            cursor += 1;
            let mut text = String::new();
            let mut closed = false;
            while cursor < sql.len() {
                if sql[cursor..].starts_with('"') {
                    cursor += 1;
                    if cursor < sql.len() && sql[cursor..].starts_with('"') {
                        text.push('"');
                        cursor += 1;
                        continue;
                    }
                    closed = true;
                    break;
                }
                let ch = sql[cursor..].chars().next()?;
                text.push(ch);
                cursor += ch.len_utf8();
            }
            if !closed || text.is_empty() {
                return None;
            }
            tokens.push(ReplicationToken { text, quoted: true });
            continue;
        }

        let start = cursor;
        while cursor < sql.len() {
            let ch = sql[cursor..].chars().next()?;
            if ch.is_whitespace() {
                break;
            }
            if ch == '"' {
                return None;
            }
            cursor += ch.len_utf8();
        }
        if cursor == start {
            return None;
        }
        tokens.push(ReplicationToken {
            text: sql[start..cursor].to_owned(),
            quoted: false,
        });
    }
    Some(tokens)
}

fn parse_slot_name_token(token: &ReplicationToken) -> Option<String> {
    let trimmed = token
        .text
        .trim()
        .trim_end_matches(';')
        .trim_end_matches(',');
    if trimmed.is_empty() {
        return None;
    }
    validate_slot_name(trimmed.to_string())
}

fn validate_slot_name(name: String) -> Option<String> {
    if name.len() <= MAX_REPLICATION_SLOT_NAME_LEN
        && name
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_')
    {
        Some(name)
    } else {
        None
    }
}

/// Read-only mode enforcement for replica servers.
///
/// Replicas reject every write with a "read-only" error. The check is
/// done here.
#[derive(Clone, Debug)]
pub struct ReadOnlyGuard {
    /// Whether the server is in read-only (replica) mode.
    read_only: bool,
}

impl ReadOnlyGuard {
    /// Create a new guard. Pass `true` for replica mode.
    pub fn new(read_only: bool) -> Self {
        Self { read_only }
    }

    /// Check if a write operation should be rejected.
    /// Returns `Err` with an appropriate message if the server is read-only.
    pub fn check_writable(&self) -> Result<(), aiondb_core::DbError> {
        if self.read_only {
            Err(aiondb_core::DbError::feature_not_supported(
                "cannot execute write operations on a read-only replica server",
            ))
        } else {
            Ok(())
        }
    }

    /// Returns `true` if the server is in read-only mode.
    pub fn is_read_only(&self) -> bool {
        self.read_only
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;

    use aiondb_wal::replication::{ReplicaId, ReplicaState};

    use super::*;

    fn test_wal_dir(name: &str) -> std::path::PathBuf {
        static TEST_SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = TEST_SEQ.fetch_add(1, Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "aiondb-pgwire-repl-tests-{name}-{}-{seq}-{nanos}",
            std::process::id()
        ));
        let _ = std::fs::create_dir_all(&dir);
        dir
    }

    fn test_handler(start_lsn: u64) -> (ReplicationStreamHandler, Arc<ReplicaRegistry>, ReplicaId) {
        let registry = Arc::new(ReplicaRegistry::new());
        let replica_id = registry
            .register(Lsn::new(start_lsn))
            .expect("register replica");
        let sender = WalSender::new(test_wal_dir("handler"), Lsn::new(start_lsn), replica_id);
        let notifier = Arc::new(WalNotifier::new(Lsn::new(start_lsn.saturating_sub(1))));
        (
            ReplicationStreamHandler::new(sender, Arc::clone(&registry), notifier),
            registry,
            replica_id,
        )
    }

    fn replica_state(registry: &ReplicaRegistry, replica_id: ReplicaId) -> ReplicaState {
        registry
            .snapshot()
            .into_iter()
            .find(|state| state.id == replica_id)
            .expect("replica must remain registered")
    }

    #[test]
    fn handle_status_update_clamps_positions_to_last_sent_lsn() {
        let (handler, registry, replica_id) = test_handler(11);

        handler
            .handle_status_update(Lsn::new(99), Lsn::new(77), Lsn::new(88))
            .expect("status update should succeed");

        let state = replica_state(&registry, replica_id);
        assert_eq!(state.write_lsn, Lsn::new(10));
        assert_eq!(state.flush_lsn, Lsn::new(10));
        assert_eq!(state.apply_lsn, Lsn::new(10));
    }

    #[test]
    fn handle_status_update_keeps_replica_progress_monotonic() {
        let (handler, registry, replica_id) = test_handler(21);

        handler
            .handle_status_update(Lsn::new(20), Lsn::new(18), Lsn::new(17))
            .expect("first status update should succeed");
        handler
            .handle_status_update(Lsn::new(5), Lsn::new(4), Lsn::new(3))
            .expect("second status update should succeed");

        let state = replica_state(&registry, replica_id);
        assert_eq!(state.write_lsn, Lsn::new(20));
        assert_eq!(state.flush_lsn, Lsn::new(18));
        assert_eq!(state.apply_lsn, Lsn::new(17));
    }

    #[test]
    fn copy_data_handler_rejects_trailing_bytes() {
        let (handler, _registry, _replica_id) = test_handler(1);
        let mut payload = ReplicationMessage::KeepaliveReply { timestamp_us: 9 }
            .encode()
            .expect("encode replication message");
        payload.push(0xFF);

        let err = handler
            .handle_copy_data(&payload)
            .expect_err("trailing bytes must be rejected");
        assert!(err.to_string().contains("trailing bytes"));
    }

    #[test]
    fn start_replication_requires_an_explicit_lsn() {
        assert!(ReplicationCommand::parse("START_REPLICATION").is_none());
    }

    #[test]
    fn parse_rejects_prefixed_command_names() {
        assert!(ReplicationCommand::parse("IDENTIFY_SYSTEMX").is_none());
        assert!(ReplicationCommand::parse("TIMELINE_HISTORYX 1").is_none());
        assert!(ReplicationCommand::parse("START_REPLICATIONX 0/10").is_none());
        assert!(ReplicationCommand::parse("CREATE_REPLICATION_SLOTX slot_a PHYSICAL").is_none());
        assert!(ReplicationCommand::parse("READ_REPLICATION_SLOTX slot_a").is_none());
    }

    #[test]
    fn parse_identify_system_rejects_trailing_arguments() {
        assert!(ReplicationCommand::parse("IDENTIFY_SYSTEM extra").is_none());
    }

    #[test]
    fn parse_timeline_history_without_argument() {
        let command =
            ReplicationCommand::parse("TIMELINE_HISTORY").expect("timeline history should parse");
        let ReplicationCommand::TimelineHistory { timeline } = command else {
            panic!("expected TimelineHistory");
        };
        assert_eq!(timeline, None);
    }

    #[test]
    fn parse_timeline_history_with_timeline_argument() {
        let command = ReplicationCommand::parse("TIMELINE_HISTORY 3")
            .expect("timeline history with argument should parse");
        let ReplicationCommand::TimelineHistory { timeline } = command else {
            panic!("expected TimelineHistory");
        };
        assert_eq!(timeline, Some(3));
    }

    #[test]
    fn parse_start_replication_lsn_with_semicolon() {
        let command = ReplicationCommand::parse("START_REPLICATION 0/10;")
            .expect("semicolon-terminated command should parse");
        let ReplicationCommand::StartReplication {
            slot_name,
            start_lsn,
            timeline,
        } = command
        else {
            panic!("expected StartReplication");
        };
        assert!(slot_name.is_none());
        assert_eq!(start_lsn, Lsn::new(16));
        assert_eq!(timeline, None);
    }

    #[test]
    fn parse_start_replication_slot_physical() {
        let command = ReplicationCommand::parse("START_REPLICATION SLOT slot_a PHYSICAL 0/10")
            .expect("slot-based replication command should parse");
        let ReplicationCommand::StartReplication {
            slot_name,
            start_lsn,
            timeline,
        } = command
        else {
            panic!("expected StartReplication");
        };
        assert_eq!(slot_name.as_deref(), Some("slot_a"));
        assert_eq!(start_lsn, Lsn::new(16));
        assert_eq!(timeline, None);
    }

    #[test]
    fn parse_start_replication_with_timeline_argument() {
        let command = ReplicationCommand::parse("START_REPLICATION 0/10 TIMELINE 3")
            .expect("timeline-qualified replication command should parse");
        let ReplicationCommand::StartReplication {
            slot_name,
            start_lsn,
            timeline,
        } = command
        else {
            panic!("expected StartReplication");
        };
        assert!(slot_name.is_none());
        assert_eq!(start_lsn, Lsn::new(16));
        assert_eq!(timeline, Some(3));
    }

    #[test]
    fn parse_start_replication_slot_with_timeline_argument() {
        let command =
            ReplicationCommand::parse("START_REPLICATION SLOT slot_a PHYSICAL 0/10 TIMELINE 5")
                .expect("timeline-qualified slot replication command should parse");
        let ReplicationCommand::StartReplication {
            slot_name,
            start_lsn,
            timeline,
        } = command
        else {
            panic!("expected StartReplication");
        };
        assert_eq!(slot_name.as_deref(), Some("slot_a"));
        assert_eq!(start_lsn, Lsn::new(16));
        assert_eq!(timeline, Some(5));
    }

    #[test]
    fn parse_start_replication_rejects_zero_timeline() {
        assert!(ReplicationCommand::parse("START_REPLICATION 0/10 TIMELINE 0").is_none());
    }

    #[test]
    fn parse_start_replication_rejects_oversized_hex_lsn_component() {
        assert!(ReplicationCommand::parse("START_REPLICATION 0/1FFFFFFFF").is_none());
        assert!(ReplicationCommand::parse("START_REPLICATION 1FFFFFFFF/0").is_none());
    }

    #[test]
    fn parse_create_replication_slot_physical() {
        let command = ReplicationCommand::parse("CREATE_REPLICATION_SLOT slot_a PHYSICAL")
            .expect("create slot command should parse");
        let ReplicationCommand::CreateReplicationSlot {
            slot_name,
            reserve_wal,
        } = command
        else {
            panic!("expected CreateReplicationSlot");
        };
        assert_eq!(slot_name, "slot_a");
        assert!(!reserve_wal);
    }

    #[test]
    fn parse_create_replication_slot_physical_with_reserve_wal() {
        let command =
            ReplicationCommand::parse("CREATE_REPLICATION_SLOT \"slot_a\" PHYSICAL RESERVE_WAL")
                .expect("create slot reserve_wal command should parse");
        let ReplicationCommand::CreateReplicationSlot {
            slot_name,
            reserve_wal,
        } = command
        else {
            panic!("expected CreateReplicationSlot");
        };
        assert_eq!(slot_name, "slot_a");
        assert!(reserve_wal);
    }

    #[test]
    fn parse_create_replication_slot_rejects_non_physical_mode() {
        assert!(ReplicationCommand::parse("CREATE_REPLICATION_SLOT slot_a LOGICAL").is_none());
    }

    #[test]
    fn parse_read_replication_slot() {
        let command = ReplicationCommand::parse("READ_REPLICATION_SLOT slot_a")
            .expect("read slot command should parse");
        let ReplicationCommand::ReadReplicationSlot { slot_name } = command else {
            panic!("expected ReadReplicationSlot");
        };
        assert_eq!(slot_name, "slot_a");
    }

    #[test]
    fn parse_base_backup_no_arguments() {
        let cmd = ReplicationCommand::parse("BASE_BACKUP").expect("BASE_BACKUP parses");
        assert!(matches!(cmd, ReplicationCommand::BaseBackup));
    }

    #[test]
    fn parse_base_backup_rejects_unsupported_arguments() {
        assert!(ReplicationCommand::parse("BASE_BACKUP TABLESPACE_MAP").is_none());
        assert!(ReplicationCommand::parse("BASE_BACKUPX").is_none());
    }

    #[test]
    fn parse_read_replication_slot_with_quoted_name() {
        let command = ReplicationCommand::parse("READ_REPLICATION_SLOT \"slot_a\"")
            .expect("quoted read slot command should parse");
        let ReplicationCommand::ReadReplicationSlot { slot_name } = command else {
            panic!("expected ReadReplicationSlot");
        };
        assert_eq!(slot_name, "slot_a");
    }

    #[test]
    fn parse_slot_commands_reject_names_with_invalid_characters() {
        assert!(ReplicationCommand::parse("READ_REPLICATION_SLOT slot_a;DROP").is_none());
        assert!(ReplicationCommand::parse("READ_REPLICATION_SLOT slot-a").is_none());
        assert!(ReplicationCommand::parse("READ_REPLICATION_SLOT \"slot\"\"a\"").is_none());
        assert!(ReplicationCommand::parse("CREATE_REPLICATION_SLOT slot-a PHYSICAL").is_none());
        assert!(
            ReplicationCommand::parse("START_REPLICATION SLOT slot;bad PHYSICAL 0/10").is_none()
        );
    }

    #[test]
    fn parse_slot_commands_reject_oversized_names() {
        let oversized = "a".repeat(MAX_REPLICATION_SLOT_NAME_LEN + 1);
        assert!(ReplicationCommand::parse(&format!("READ_REPLICATION_SLOT {oversized}")).is_none());
        assert!(ReplicationCommand::parse(&format!(
            "CREATE_REPLICATION_SLOT {oversized} PHYSICAL"
        ))
        .is_none());
        assert!(ReplicationCommand::parse(&format!(
            "START_REPLICATION SLOT {oversized} PHYSICAL 0/10"
        ))
        .is_none());
    }

    #[test]
    fn parse_read_replication_slot_rejects_unterminated_quoted_name() {
        assert!(ReplicationCommand::parse("READ_REPLICATION_SLOT \"slot_a").is_none());
    }
}
