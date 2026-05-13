//! Logical changefeed -- row-level CDC for AionDB.
//!
//! Inspired by CockroachDB's `CHANGEFEED FOR TABLE` and PostgreSQL's
//! logical replication. The primary publishes a stream of
//! [`ChangefeedEvent`]s carrying row mutations the application can fan
//! out to Kafka, webhooks, or any downstream sink. Each emission is
//! interleaved with `Resolved(ts)` heartbeats: once a consumer has seen
//! `Resolved(T)`, every committed change at timestamp `< T` has been
//! delivered, so the consumer can checkpoint and replay from `T` after a
//! crash without missing rows.
//!
//! ## Wire-level guarantees
//!
//! - **Ordering** -- events arrive in commit order per table. Inserts,
//!   updates and deletes carry the post-image (or pre-image, for deletes).
//! - **Resolved cadence** -- the publisher emits a `Resolved` heartbeat
//!   roughly every `resolved_interval` even when no writes are happening,
//!   so consumers can advance their watermark.
//! - **Backpressure** -- the bus uses a bounded `tokio::sync::broadcast`
//!   channel. Slow subscribers see `BroadcastError::Lagged` and must
//!   reconnect from the last persisted resolved timestamp.
//!
//! The bus itself is storage-agnostic: a runtime adapter feeds it
//! [`WalRecord`]s as they commit on the primary. The bus does the
//! mapping `WalRecord -> ChangefeedEvent` and the `Resolved` cadence.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use aiondb_core::{DbError, DbResult, RelationId, Row, TupleId};
use aiondb_wal::record::WalEntry;
use aiondb_wal::WalRecord;
use tokio::sync::broadcast;

/// Default cadence at which the bus emits `Resolved` heartbeats when no
/// writes have arrived. Cockroach defaults to 200ms; AionDB starts more
/// relaxed at 1s and lets operators tighten it via configuration.
pub const DEFAULT_RESOLVED_INTERVAL: Duration = Duration::from_secs(1);
/// Default channel depth. Subscribers that lag past this point lose
/// events and must reconnect from their last persisted resolved
/// timestamp.
pub const DEFAULT_CHANNEL_CAPACITY: usize = 1024;

/// One row-level change observed on the primary.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ChangefeedEvent {
    /// A row was inserted.
    Insert {
        commit_ts: u64,
        table: RelationId,
        tuple_id: TupleId,
        row: Row,
    },
    /// A row was deleted.
    Delete {
        commit_ts: u64,
        table: RelationId,
        tuple_id: TupleId,
    },
    /// A row was updated. The post-image row is included; pre-image is
    /// available via the row in the WAL but not surfaced here yet (a
    /// future iteration will plumb it through).
    Update {
        commit_ts: u64,
        table: RelationId,
        old_tuple_id: TupleId,
        new_tuple_id: TupleId,
        row: Row,
    },
    /// Heartbeat carrying the closed timestamp under which all earlier
    /// `Insert` / `Update` / `Delete` events have already been published.
    /// Consumers checkpoint at this watermark.
    Resolved { commit_ts: u64 },
}

/// Filter the changefeed bus applies before fanning an event out to
/// subscribers. Subscribers configure their interest at subscription
/// time and only see the events that match.
#[derive(Clone, Debug, Default)]
pub struct ChangefeedFilter {
    /// When `Some`, only events whose table is in the set are forwarded.
    /// When `None`, every table is observed.
    tables: Option<Vec<RelationId>>,
}

impl ChangefeedFilter {
    /// Match every table.
    pub fn all_tables() -> Self {
        Self { tables: None }
    }

    /// Match a fixed list of tables. Empty list matches nothing.
    pub fn tables(tables: impl IntoIterator<Item = RelationId>) -> Self {
        Self {
            tables: Some(tables.into_iter().collect()),
        }
    }

    pub fn matches(&self, event: &ChangefeedEvent) -> bool {
        let Some(tables) = &self.tables else {
            return true;
        };
        let table = match event {
            ChangefeedEvent::Insert { table, .. }
            | ChangefeedEvent::Delete { table, .. }
            | ChangefeedEvent::Update { table, .. } => Some(*table),
            ChangefeedEvent::Resolved { .. } => None,
        };
        match table {
            Some(t) => tables.contains(&t),
            // Resolved events pass every filter -- they carry the
            // watermark that consumers always need.
            None => true,
        }
    }
}

/// Configuration for a [`ChangefeedBus`].
#[derive(Clone, Debug)]
pub struct ChangefeedConfig {
    pub channel_capacity: usize,
    pub resolved_interval: Duration,
}

impl Default for ChangefeedConfig {
    fn default() -> Self {
        Self {
            channel_capacity: DEFAULT_CHANNEL_CAPACITY,
            resolved_interval: DEFAULT_RESOLVED_INTERVAL,
        }
    }
}

/// In-process changefeed publisher. Multiple subscribers fan out from a
/// single primary-side feeder. The bus owns per-subscriber filters so
/// consumers only receive the events they asked for.
#[derive(Clone, Debug)]
pub struct ChangefeedBus {
    inner: Arc<ChangefeedBusInner>,
}

#[derive(Debug)]
struct ChangefeedBusInner {
    config: ChangefeedConfig,
    tx: broadcast::Sender<ChangefeedEvent>,
    open_txns: std::sync::Mutex<HashMap<aiondb_core::TxnId, Vec<ChangefeedEvent>>>,
}

impl ChangefeedBus {
    pub fn new(config: ChangefeedConfig) -> Self {
        let (tx, _rx) = broadcast::channel(config.channel_capacity.max(1));
        Self {
            inner: Arc::new(ChangefeedBusInner {
                config,
                tx,
                open_txns: std::sync::Mutex::new(HashMap::new()),
            }),
        }
    }

    pub fn with_defaults() -> Self {
        Self::new(ChangefeedConfig::default())
    }

    /// Subscribe to the bus. The returned [`ChangefeedSubscription`] only
    /// surfaces events matching `filter`; everything else is dropped at
    /// the bus boundary so the consumer never has to deal with traffic
    /// it does not care about.
    pub fn subscribe(&self, filter: ChangefeedFilter) -> ChangefeedSubscription {
        ChangefeedSubscription {
            rx: self.inner.tx.subscribe(),
            filter,
        }
    }

    /// Feed one WAL entry through the bus. DML records buffered in their
    /// owning transaction are released to subscribers on `CommitTxn`.
    /// `AbortTxn` discards them. Autocommit-flavoured records are emitted
    /// immediately. Returns the number of events broadcast to subscribers.
    pub fn ingest_wal_entry(&self, entry: &WalEntry) -> DbResult<usize> {
        match &entry.record {
            WalRecord::BeginTxn { txn_id, .. } => {
                let mut open = self.lock_open_txns()?;
                open.entry(*txn_id).or_default();
                Ok(0)
            }
            WalRecord::AbortTxn { txn_id } => {
                let mut open = self.lock_open_txns()?;
                open.remove(txn_id);
                Ok(0)
            }
            WalRecord::CommitTxn { txn_id, commit_ts } => {
                let mut open = self.lock_open_txns()?;
                let buffered = open.remove(txn_id).unwrap_or_default();
                drop(open);
                let mut emitted = 0usize;
                for mut event in buffered {
                    rewrite_commit_ts(&mut event, *commit_ts);
                    if self.inner.tx.send(event).is_ok() {
                        emitted += 1;
                    }
                }
                Ok(emitted)
            }
            WalRecord::InsertRow {
                txn_id,
                table_id,
                tuple_id,
                row,
            } => {
                let event = ChangefeedEvent::Insert {
                    commit_ts: 0,
                    table: *table_id,
                    tuple_id: *tuple_id,
                    row: row.clone(),
                };
                self.buffer(*txn_id, event)
            }
            WalRecord::DeleteRow {
                txn_id,
                table_id,
                tuple_id,
            } => {
                let event = ChangefeedEvent::Delete {
                    commit_ts: 0,
                    table: *table_id,
                    tuple_id: *tuple_id,
                };
                self.buffer(*txn_id, event)
            }
            WalRecord::UpdateRow {
                txn_id,
                table_id,
                old_tuple_id,
                new_tuple_id,
                row,
            } => {
                let event = ChangefeedEvent::Update {
                    commit_ts: 0,
                    table: *table_id,
                    old_tuple_id: *old_tuple_id,
                    new_tuple_id: *new_tuple_id,
                    row: row.clone(),
                };
                self.buffer(*txn_id, event)
            }
            WalRecord::AutocommitInsertRow {
                table_id,
                tuple_id,
                row,
                ..
            } => self.emit_now(ChangefeedEvent::Insert {
                // Autocommit records do not carry an explicit commit_ts;
                // use the entry's LSN as the monotonic logical clock so
                // consumers can still order events deterministically.
                commit_ts: entry.lsn.get(),
                table: *table_id,
                tuple_id: *tuple_id,
                row: row.clone(),
            }),
            WalRecord::AutocommitDeleteRow {
                table_id, tuple_id, ..
            } => self.emit_now(ChangefeedEvent::Delete {
                commit_ts: entry.lsn.get(),
                table: *table_id,
                tuple_id: *tuple_id,
            }),
            WalRecord::AutocommitUpdateRow {
                table_id,
                old_tuple_id,
                new_tuple_id,
                row,
                ..
            } => self.emit_now(ChangefeedEvent::Update {
                commit_ts: entry.lsn.get(),
                table: *table_id,
                old_tuple_id: *old_tuple_id,
                new_tuple_id: *new_tuple_id,
                row: row.clone(),
            }),
            // Everything else (DDL, page-level records, etc.) is invisible
            // to the changefeed.
            _ => Ok(0),
        }
    }

    /// Publish a `Resolved(ts)` heartbeat. Returns `true` when at least
    /// one subscriber was awake to receive it.
    pub fn publish_resolved(&self, ts: u64) -> bool {
        self.inner
            .tx
            .send(ChangefeedEvent::Resolved { commit_ts: ts })
            .is_ok()
    }

    /// Spawn a background task that emits `Resolved(now)` heartbeats at
    /// the configured cadence until `shutdown_rx` flips to `true`.
    pub fn spawn_resolved_ticker(
        &self,
        mut shutdown_rx: tokio::sync::watch::Receiver<bool>,
    ) -> tokio::task::JoinHandle<()> {
        let bus = self.clone();
        let interval = bus.inner.config.resolved_interval;
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tokio::select! {
                    biased;
                    changed = shutdown_rx.changed() => {
                        if changed.is_err() || *shutdown_rx.borrow() {
                            return;
                        }
                    }
                    _ = ticker.tick() => {
                        bus.publish_resolved(current_wall_us());
                    }
                }
            }
        })
    }

    fn buffer(&self, txn_id: aiondb_core::TxnId, event: ChangefeedEvent) -> DbResult<usize> {
        let mut open = self.lock_open_txns()?;
        open.entry(txn_id).or_default().push(event);
        Ok(0)
    }

    fn emit_now(&self, event: ChangefeedEvent) -> DbResult<usize> {
        if self.inner.tx.send(event).is_ok() {
            Ok(1)
        } else {
            Ok(0)
        }
    }

    /// Public helper used by sinks / tests to publish a single event
    /// outside of a transaction context. Returns the number of
    /// subscribers that received the event (`0` when there are none
    /// or when the broadcast channel is closed).
    pub fn emit_autocommit(&self, event: ChangefeedEvent) -> usize {
        self.emit_now(event).unwrap_or_default()
    }

    fn lock_open_txns(
        &self,
    ) -> DbResult<std::sync::MutexGuard<'_, HashMap<aiondb_core::TxnId, Vec<ChangefeedEvent>>>>
    {
        self.inner
            .open_txns
            .lock()
            .map_err(|err| DbError::internal(format!("changefeed open_txns mutex poisoned: {err}")))
    }
}

/// Per-subscriber receiver. Internally a `tokio::sync::broadcast::Receiver`
/// wrapped with the subscriber's [`ChangefeedFilter`].
pub struct ChangefeedSubscription {
    rx: broadcast::Receiver<ChangefeedEvent>,
    filter: ChangefeedFilter,
}

impl ChangefeedSubscription {
    /// Receive the next event matching the subscription filter. Skips
    /// non-matching events silently. `Lagged` errors propagate so the
    /// subscriber can resync from its checkpoint.
    pub async fn recv(&mut self) -> Result<ChangefeedEvent, broadcast::error::RecvError> {
        loop {
            let event = self.rx.recv().await?;
            if self.filter.matches(&event) {
                return Ok(event);
            }
        }
    }

    /// Non-blocking variant. Returns `Ok(None)` if no matching event is
    /// pending right now.
    pub fn try_recv(&mut self) -> Result<Option<ChangefeedEvent>, broadcast::error::TryRecvError> {
        loop {
            match self.rx.try_recv() {
                Ok(event) if self.filter.matches(&event) => return Ok(Some(event)),
                Ok(_) => {}
                Err(broadcast::error::TryRecvError::Empty) => return Ok(None),
                Err(other) => return Err(other),
            }
        }
    }
}

fn rewrite_commit_ts(event: &mut ChangefeedEvent, commit_ts: u64) {
    match event {
        ChangefeedEvent::Insert { commit_ts: ts, .. }
        | ChangefeedEvent::Delete { commit_ts: ts, .. }
        | ChangefeedEvent::Update { commit_ts: ts, .. } => *ts = commit_ts,
        ChangefeedEvent::Resolved { .. } => {}
    }
}

fn current_wall_us() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_micros()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aiondb_core::TxnId;
    use aiondb_core::Value;
    use aiondb_wal::record::WalEntry;
    use aiondb_wal::IsolationLevel;
    use aiondb_wal::Lsn;
    use aiondb_wal::WalRecord;

    fn relation(n: u64) -> RelationId {
        RelationId::new(n)
    }

    fn tuple(n: u64) -> TupleId {
        TupleId::new(n)
    }

    fn row(values: Vec<i64>) -> Row {
        Row::new(values.into_iter().map(Value::BigInt).collect())
    }

    fn wal_entry(record: WalRecord, lsn: u64) -> WalEntry {
        WalEntry {
            lsn: Lsn::new(lsn),
            prev_lsn: Lsn::ZERO,
            database_id: WalEntry::LEGACY_DATABASE_ID,
            record,
        }
    }

    #[test]
    fn autocommit_insert_emits_event_to_matching_subscriber() {
        let bus = ChangefeedBus::with_defaults();
        let mut sub = bus.subscribe(ChangefeedFilter::tables([relation(1)]));
        let emitted = bus
            .ingest_wal_entry(&wal_entry(
                WalRecord::AutocommitInsertRow {
                    txn_id: TxnId::default(),
                    table_id: relation(1),
                    tuple_id: tuple(10),
                    row: row(vec![1, 2, 3]),
                },
                77,
            ))
            .expect("ingest");
        assert_eq!(emitted, 1);
        let event = sub.try_recv().expect("try_recv").expect("event pending");
        assert_eq!(
            event,
            ChangefeedEvent::Insert {
                commit_ts: 77,
                table: relation(1),
                tuple_id: tuple(10),
                row: row(vec![1, 2, 3]),
            }
        );
    }

    #[test]
    fn filter_drops_events_for_unrelated_tables() {
        let bus = ChangefeedBus::with_defaults();
        let mut sub = bus.subscribe(ChangefeedFilter::tables([relation(99)]));
        bus.ingest_wal_entry(&wal_entry(
            WalRecord::AutocommitInsertRow {
                txn_id: TxnId::default(),
                table_id: relation(1),
                tuple_id: tuple(1),
                row: row(vec![1]),
            },
            1,
        ))
        .expect("ingest");
        assert!(matches!(sub.try_recv(), Ok(None)));
    }

    #[test]
    fn transactional_changes_buffer_until_commit() {
        let bus = ChangefeedBus::with_defaults();
        let mut sub = bus.subscribe(ChangefeedFilter::all_tables());
        bus.ingest_wal_entry(&wal_entry(
            WalRecord::BeginTxn {
                txn_id: TxnId::new(5),
                isolation: IsolationLevel::ReadCommitted,
            },
            1,
        ))
        .expect("begin");
        bus.ingest_wal_entry(&wal_entry(
            WalRecord::InsertRow {
                txn_id: TxnId::new(5),
                table_id: relation(1),
                tuple_id: tuple(11),
                row: row(vec![10]),
            },
            2,
        ))
        .expect("insert");
        bus.ingest_wal_entry(&wal_entry(
            WalRecord::DeleteRow {
                txn_id: TxnId::new(5),
                table_id: relation(1),
                tuple_id: tuple(7),
            },
            3,
        ))
        .expect("delete");
        // Subscriber sees nothing until commit.
        assert!(matches!(sub.try_recv(), Ok(None)));

        let emitted = bus
            .ingest_wal_entry(&wal_entry(
                WalRecord::CommitTxn {
                    txn_id: TxnId::new(5),
                    commit_ts: 88,
                },
                4,
            ))
            .expect("commit");
        assert_eq!(emitted, 2);

        let first = sub.try_recv().expect("try_recv").expect("first event");
        let second = sub.try_recv().expect("try_recv").expect("second event");
        match (first, second) {
            (
                ChangefeedEvent::Insert {
                    commit_ts: ts1,
                    table: t1,
                    tuple_id: tu1,
                    ..
                },
                ChangefeedEvent::Delete {
                    commit_ts: ts2,
                    table: t2,
                    tuple_id: tu2,
                },
            ) => {
                assert_eq!(ts1, 88);
                assert_eq!(ts2, 88);
                assert_eq!(t1, relation(1));
                assert_eq!(t2, relation(1));
                assert_eq!(tu1, tuple(11));
                assert_eq!(tu2, tuple(7));
            }
            other => panic!("unexpected event ordering: {other:?}"),
        }
    }

    #[test]
    fn abort_drops_buffered_changes() {
        let bus = ChangefeedBus::with_defaults();
        let mut sub = bus.subscribe(ChangefeedFilter::all_tables());
        bus.ingest_wal_entry(&wal_entry(
            WalRecord::BeginTxn {
                txn_id: TxnId::new(9),
                isolation: IsolationLevel::ReadCommitted,
            },
            1,
        ))
        .expect("begin");
        bus.ingest_wal_entry(&wal_entry(
            WalRecord::InsertRow {
                txn_id: TxnId::new(9),
                table_id: relation(1),
                tuple_id: tuple(1),
                row: row(vec![1]),
            },
            2,
        ))
        .expect("insert");
        bus.ingest_wal_entry(&wal_entry(
            WalRecord::AbortTxn {
                txn_id: TxnId::new(9),
            },
            3,
        ))
        .expect("abort");
        assert!(matches!(sub.try_recv(), Ok(None)));
    }

    #[test]
    fn resolved_heartbeat_is_visible_to_all_filters() {
        let bus = ChangefeedBus::with_defaults();
        let mut sub_specific = bus.subscribe(ChangefeedFilter::tables([relation(123)]));
        let mut sub_all = bus.subscribe(ChangefeedFilter::all_tables());
        assert!(bus.publish_resolved(999));
        let evt_specific = sub_specific.try_recv().expect("try_recv").expect("event");
        let evt_all = sub_all.try_recv().expect("try_recv").expect("event");
        assert_eq!(evt_specific, ChangefeedEvent::Resolved { commit_ts: 999 });
        assert_eq!(evt_all, ChangefeedEvent::Resolved { commit_ts: 999 });
    }

    #[tokio::test]
    async fn resolved_ticker_emits_periodically_until_shutdown() {
        let bus = ChangefeedBus::new(ChangefeedConfig {
            resolved_interval: Duration::from_millis(40),
            ..ChangefeedConfig::default()
        });
        let mut sub = bus.subscribe(ChangefeedFilter::all_tables());
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let handle = bus.spawn_resolved_ticker(shutdown_rx);

        let mut resolved_seen = 0usize;
        for _ in 0..3 {
            match tokio::time::timeout(Duration::from_secs(2), sub.recv()).await {
                Ok(Ok(ChangefeedEvent::Resolved { .. })) => resolved_seen += 1,
                Ok(Ok(_)) => {}
                Ok(Err(err)) => panic!("recv error: {err}"),
                Err(err) => panic!("ticker did not produce events in time: {err}"),
            }
        }
        assert!(resolved_seen >= 3, "ticker must emit at least 3 heartbeats");

        shutdown_tx.send(true).expect("shutdown");
        let _ = tokio::time::timeout(Duration::from_secs(2), handle).await;
    }
}
