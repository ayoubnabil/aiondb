use std::{cell::RefCell, sync::Arc};

use aiondb_core::DbResult;

/// Trait the engine implements to accept NOTIFY side-effects produced
/// by the `pg_notify()` scalar function during statement evaluation.
pub trait NotifySink: Send + Sync + 'static {
    /// Record a NOTIFY request for (channel, payload).
    fn push(&self, channel: &str, payload: &str) -> DbResult<()>;
    /// Report the current notification queue usage in [0.0, 1.0].
    fn queue_usage(&self) -> f64;
    /// Return the channels listened to by the current session.
    fn listening_channels(&self) -> Vec<String>;
}

thread_local! {
    static CURRENT_SINK: RefCell<Option<Arc<dyn NotifySink>>> = const { RefCell::new(None) };
}

/// Install `sink` as the current thread's notify sink for the duration
/// of `f`, restoring any previous sink when `f` returns (or panics).
pub fn with_sink<F, R>(sink: Arc<dyn NotifySink>, f: F) -> R
where
    F: FnOnce() -> R,
{
    struct Restore {
        previous: Option<Arc<dyn NotifySink>>,
    }
    impl Drop for Restore {
        fn drop(&mut self) {
            let previous = self.previous.take();
            CURRENT_SINK.with(|slot| {
                *slot.borrow_mut() = previous;
            });
        }
    }
    let previous = CURRENT_SINK.with(|slot| slot.borrow_mut().replace(sink));
    let _restore = Restore { previous };
    f()
}

/// Push a NOTIFY (channel, payload) to the current thread's sink, if any.
pub fn push_notification(channel: &str, payload: &str) -> DbResult<()> {
    CURRENT_SINK.with(|slot| {
        if let Some(sink) = slot.borrow().as_ref() {
            sink.push(channel, payload)?;
        }
        Ok(())
    })
}

/// Report the current thread's sink queue usage in [0.0, 1.0].
pub fn current_queue_usage() -> f64 {
    CURRENT_SINK.with(|slot| {
        slot.borrow()
            .as_ref()
            .map_or(0.0, |sink| sink.queue_usage())
    })
}

/// Return the channels listened to by the current session, if any.
pub fn listening_channels() -> Vec<String> {
    CURRENT_SINK.with(|slot| {
        slot.borrow()
            .as_ref()
            .map_or_else(Vec::new, |sink| sink.listening_channels())
    })
}
