use std::sync::Mutex;

use crate::lsn::Lsn;

/// A broadcast-style notification that new WAL entries are available.
///
/// The primary's WAL writer notifies this channel after each commit so
/// that WAL senders can wake up and send new data to replicas without
/// polling.
#[derive(Debug)]
pub struct WalNotifier {
    /// Current end-of-WAL LSN. Senders read this to know if there are
    /// new entries.
    current_lsn: std::sync::atomic::AtomicU64,
    /// Async notification channel used by pgwire senders without timer-polling.
    async_notify: tokio::sync::watch::Sender<u64>,
    /// Condvar for blocking senders that are caught up and waiting for
    /// new WAL data.
    notify: std::sync::Condvar,
    /// Associated mutex for the condvar.
    pub(super) lock: Mutex<()>,
}

impl WalNotifier {
    pub fn new(initial_lsn: Lsn) -> Self {
        let (async_notify, _receiver) = tokio::sync::watch::channel(initial_lsn.get());
        Self {
            current_lsn: std::sync::atomic::AtomicU64::new(initial_lsn.get()),
            async_notify,
            notify: std::sync::Condvar::new(),
            lock: Mutex::new(()),
        }
    }

    /// Called by the WAL writer after appending new entries.
    pub fn notify_new_wal(&self, lsn: Lsn) {
        self.current_lsn
            .store(lsn.get(), std::sync::atomic::Ordering::Release);
        let _ = self.async_notify.send(lsn.get());
        self.notify.notify_all();
    }

    /// Return the current end-of-WAL LSN.
    pub fn current_lsn(&self) -> Lsn {
        Lsn::new(self.current_lsn.load(std::sync::atomic::Ordering::Acquire))
    }

    /// Block until the WAL advances past `after_lsn`, or until `timeout`
    /// elapses. Returns the new current LSN.
    pub fn wait_for_new_wal(&self, after_lsn: Lsn, timeout: std::time::Duration) -> Lsn {
        let current = self.current_lsn();
        if current > after_lsn {
            return current;
        }

        let guard = self
            .lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _guard = match self.notify.wait_timeout_while(guard, timeout, |()| {
            Lsn::new(self.current_lsn.load(std::sync::atomic::Ordering::Acquire)) <= after_lsn
        }) {
            Ok(pair) => pair,
            Err(poisoned) => poisoned.into_inner(),
        };

        self.current_lsn()
    }

    /// Async variant of [`WalNotifier::wait_for_new_wal`] for pgwire senders.
    pub async fn wait_for_new_wal_async(
        &self,
        after_lsn: Lsn,
        timeout: std::time::Duration,
    ) -> Lsn {
        let current = self.current_lsn();
        if current > after_lsn || timeout.is_zero() {
            return current;
        }

        let mut receiver = self.async_notify.subscribe();
        if Lsn::new(*receiver.borrow()) > after_lsn {
            return self.current_lsn();
        }

        let wait_result = tokio::time::timeout(timeout, async {
            loop {
                if receiver.changed().await.is_err() {
                    break;
                }
                if Lsn::new(*receiver.borrow()) > after_lsn {
                    break;
                }
            }
        })
        .await;

        if wait_result.is_err() {
            return self.current_lsn();
        }
        self.current_lsn()
    }
}

impl Default for WalNotifier {
    fn default() -> Self {
        Self::new(Lsn::ZERO)
    }
}
