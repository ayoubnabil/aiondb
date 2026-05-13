use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::{Mutex, MutexGuard},
};

use super::support::usize_to_f64;
use crate::session::SessionHandle;

pub(crate) const CHANNEL_NAME_MAX: usize = 63;
const NOTIFICATION_QUEUE_CAPACITY: usize = 65_536;

#[derive(Clone, Debug)]
pub struct Notification {
    /// Original channel name as observed by the SQL client. Stable across
    /// the bus boundary so listeners see the same name they LISTEN'd on.
    #[cfg_attr(not(test), allow(dead_code))]
    pub channel: String,
    /// Internal routing key used to match listeners; tenant-scoped so two
    /// tenants that LISTEN on the same logical channel do not see each
    /// other's NOTIFYs (audit notify F-N1).
    pub routing_key: String,
    #[cfg_attr(not(test), allow(dead_code))]
    pub payload: String,
}

#[derive(Default, Debug)]
struct BusInner {
    channel_listeners: HashMap<String, HashSet<SessionHandle>>,
    session_channels: HashMap<SessionHandle, HashSet<(String, String)>>,
    queues: HashMap<SessionHandle, VecDeque<Notification>>,
    total_pending: usize,
}

#[derive(Debug)]
pub struct NotificationBus {
    inner: Mutex<BusInner>,
}

impl Default for NotificationBus {
    fn default() -> Self {
        Self {
            inner: Mutex::new(BusInner::default()),
        }
    }
}

impl NotificationBus {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock_inner(&self) -> MutexGuard<'_, BusInner> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn listen(&self, session: &SessionHandle, channel: &str, routing_key: &str) {
        let mut inner = self.lock_inner();
        inner
            .channel_listeners
            .entry(routing_key.to_owned())
            .or_default()
            .insert(session.clone());
        inner
            .session_channels
            .entry(session.clone())
            .or_default()
            .insert((channel.to_owned(), routing_key.to_owned()));
    }

    pub fn unlisten(&self, session: &SessionHandle, channel: &str, routing_key: &str) {
        let mut inner = self.lock_inner();
        if let Some(listeners) = inner.channel_listeners.get_mut(routing_key) {
            listeners.remove(session);
            if listeners.is_empty() {
                inner.channel_listeners.remove(routing_key);
            }
        }
        if let Some(channels) = inner.session_channels.get_mut(session) {
            channels.remove(&(channel.to_owned(), routing_key.to_owned()));
            if channels.is_empty() {
                inner.session_channels.remove(session);
            }
        }
    }

    pub fn unlisten_all(&self, session: &SessionHandle) {
        let mut inner = self.lock_inner();
        if let Some(channels) = inner.session_channels.remove(session) {
            for (_channel, routing_key) in channels {
                if let Some(listeners) = inner.channel_listeners.get_mut(&routing_key) {
                    listeners.remove(session);
                    if listeners.is_empty() {
                        inner.channel_listeners.remove(&routing_key);
                    }
                }
            }
        }
    }

    pub fn publish(&self, notifications: &[Notification]) {
        if notifications.is_empty() {
            return;
        }
        let mut inner = self.lock_inner();
        for notification in notifications {
            let Some(listeners) = inner
                .channel_listeners
                .get(&notification.routing_key)
                .cloned()
            else {
                continue;
            };
            for listener in listeners {
                let queue = inner.queues.entry(listener).or_default();
                if queue.len() < NOTIFICATION_QUEUE_CAPACITY {
                    queue.push_back(notification.clone());
                    inner.total_pending += 1;
                }
            }
        }
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn drain_for(&self, session: &SessionHandle) -> Vec<Notification> {
        let mut inner = self.lock_inner();
        let Some(queue) = inner.queues.remove(session) else {
            return Vec::new();
        };
        inner.total_pending = inner.total_pending.saturating_sub(queue.len());
        queue.into_iter().collect()
    }

    pub fn remove_session(&self, session: &SessionHandle) {
        self.unlisten_all(session);
        let mut inner = self.lock_inner();
        if let Some(queue) = inner.queues.remove(session) {
            inner.total_pending = inner.total_pending.saturating_sub(queue.len());
        }
    }

    pub fn queue_usage(&self) -> f64 {
        let inner = self.lock_inner();
        if inner.total_pending == 0 {
            0.0
        } else {
            let usage =
                usize_to_f64(inner.total_pending) / usize_to_f64(NOTIFICATION_QUEUE_CAPACITY);
            usage.min(1.0)
        }
    }

    pub fn listening_channels(&self, session: &SessionHandle) -> Vec<String> {
        let inner = self.lock_inner();
        inner
            .session_channels
            .get(session)
            .map(|set| {
                let mut channels: Vec<String> =
                    set.iter().map(|(channel, _)| channel.clone()).collect();
                channels.sort();
                channels
            })
            .unwrap_or_default()
    }
}

pub fn validate_channel_name(channel: Option<&str>) -> Result<String, &'static str> {
    let Some(name) = channel else {
        return Err("channel name cannot be empty");
    };
    if name.is_empty() {
        return Err("channel name cannot be empty");
    }
    if name.len() > CHANNEL_NAME_MAX {
        return Err("channel name too long");
    }

    // Reject control bytes and non-ASCII so a NUL-truncated client or
    // a Unicode confusable cannot fan out to the wrong listener set
    // (audit notify F-N2).
    for ch in name.chars() {
        if ch == '\0' || ch.is_ascii_control() || !ch.is_ascii() {
            return Err("channel name contains invalid character");
        }
    }

    Ok(name.to_owned())
}
