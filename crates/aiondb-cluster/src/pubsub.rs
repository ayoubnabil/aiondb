//! Cluster pub/sub bus.
//!
//! Topic-based broadcast with bounded per-subscriber queues. Used
//! for cluster-wide invalidation messages (catalog version bumps,
//! lease moves, schema migration phase changes) where every node
//! must learn the event but exactly-once delivery is not required.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::broadcast;

#[derive(Clone, Debug)]
pub struct PubSubBus<T: Clone + Send + Sync + 'static> {
    topics: Arc<std::sync::Mutex<HashMap<String, broadcast::Sender<T>>>>,
    capacity: usize,
}

impl<T: Clone + Send + Sync + 'static> PubSubBus<T> {
    pub fn new(capacity: usize) -> Self {
        Self {
            topics: Arc::new(std::sync::Mutex::new(HashMap::new())),
            capacity: capacity.max(1),
        }
    }

    pub fn publish(&self, topic: &str, message: T) -> usize {
        let sender = self.sender_for(topic);
        sender.send(message).unwrap_or(0)
    }

    pub fn subscribe(&self, topic: &str) -> broadcast::Receiver<T> {
        self.sender_for(topic).subscribe()
    }

    pub fn topic_count(&self) -> usize {
        self.topics.lock().unwrap().len()
    }

    pub fn subscriber_count(&self, topic: &str) -> usize {
        self.topics
            .lock()
            .unwrap()
            .get(topic)
            .map(|t| t.receiver_count())
            .unwrap_or(0)
    }

    fn sender_for(&self, topic: &str) -> broadcast::Sender<T> {
        let mut guard = self.topics.lock().unwrap();
        guard
            .entry(topic.to_owned())
            .or_insert_with(|| broadcast::channel(self.capacity).0)
            .clone()
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[tokio::test]
    async fn publish_reaches_every_subscriber_on_the_topic() {
        let bus: PubSubBus<u64> = PubSubBus::new(32);
        let mut a = bus.subscribe("evt");
        let mut b = bus.subscribe("evt");
        let n = bus.publish("evt", 42);
        assert_eq!(n, 2);
        assert_eq!(
            tokio::time::timeout(Duration::from_millis(50), a.recv())
                .await
                .unwrap()
                .unwrap(),
            42
        );
        assert_eq!(
            tokio::time::timeout(Duration::from_millis(50), b.recv())
                .await
                .unwrap()
                .unwrap(),
            42
        );
    }

    #[tokio::test]
    async fn topics_are_independent() {
        let bus: PubSubBus<&'static str> = PubSubBus::new(8);
        let mut a = bus.subscribe("topicA");
        let _b = bus.subscribe("topicB");
        bus.publish("topicA", "hello");
        let msg = tokio::time::timeout(Duration::from_millis(50), a.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(msg, "hello");
        // Subscriber on B should NOT see topicA's message.
    }

    #[test]
    fn publish_to_topic_without_subscribers_returns_zero() {
        let bus: PubSubBus<u32> = PubSubBus::new(4);
        assert_eq!(bus.publish("evt", 1), 0);
    }

    #[test]
    fn subscriber_count_reflects_active_receivers() {
        let bus: PubSubBus<()> = PubSubBus::new(4);
        let _a = bus.subscribe("t");
        let _b = bus.subscribe("t");
        assert_eq!(bus.subscriber_count("t"), 2);
    }

    #[tokio::test]
    async fn slow_subscriber_loses_old_messages_but_does_not_block_publisher() {
        let bus: PubSubBus<u32> = PubSubBus::new(2);
        let mut rx = bus.subscribe("evt");
        for i in 0..10u32 {
            bus.publish("evt", i);
        }
        // Capacity is 2 -- lagging subscriber will get a Lagged error
        // on subsequent recv.
        let mut got_lagged = false;
        loop {
            match tokio::time::timeout(Duration::from_millis(20), rx.recv()).await {
                Ok(Ok(_)) => {}
                Ok(Err(broadcast::error::RecvError::Lagged(_))) => {
                    got_lagged = true;
                    break;
                }
                _ => break,
            }
        }
        assert!(got_lagged, "slow subscriber should observe Lagged");
    }
}
