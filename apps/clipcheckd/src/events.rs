//! Fan-out of pushed IPC events to connected subscribers (task D1c).
//!
//! Each IPC connection registers a [`Subscription`] and gets the receiving end
//! of a channel. [`EventBus::publish`] sends every event to the subscribers
//! whose topics match. A subscriber whose receiver is gone is dropped during
//! the next publish, and dropping a [`Subscription`] removes it at once.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};

use clipcheck_ipc::proto::{Event, Topic};

/// One event with the sequence number the daemon assigned to it.
#[derive(Debug, Clone, PartialEq)]
pub struct Delivery {
    /// Per-daemon monotonic sequence number.
    pub seq: u64,
    /// The event payload, shared by every subscriber it is delivered to.
    pub event: Arc<Event>,
}

/// A registered subscriber.
struct Subscriber {
    id: u64,
    topics: Vec<Topic>,
    tx: Sender<Delivery>,
}

/// Shared list of subscribers. Cheap to query and safe to publish from any thread.
#[derive(Default)]
pub struct EventBus {
    subscribers: Mutex<Vec<Subscriber>>,
    next_id: AtomicU64,
    seq: AtomicU64,
}

impl EventBus {
    /// Creates a bus with no subscribers.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a new subscriber with no topics. Returns the subscription
    /// guard and the receiver the connection drains.
    pub fn subscribe(self: &Arc<Self>) -> (Subscription, Receiver<Delivery>) {
        let (tx, rx) = mpsc::channel();
        let id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        self.lock().push(Subscriber {
            id,
            topics: Vec::new(),
            tx,
        });
        let sub = Subscription {
            bus: Arc::clone(self),
            id,
        };
        (sub, rx)
    }

    /// Sends `event` to every subscriber whose topics include it. Events
    /// without a topic (see [`Event::topic`]) go to everyone.
    ///
    /// The sequence number is taken while the subscriber lock is held, so
    /// concurrent publishers cannot deliver `N+1` before `N` to any subscriber.
    pub fn publish(&self, event: Event) {
        let event = Arc::new(event);
        let mut subs = self.lock();
        let seq = self.seq.fetch_add(1, Ordering::Relaxed) + 1;
        subs.retain(|sub| {
            if !wants(&sub.topics, &event) {
                return true;
            }
            sub.tx
                .send(Delivery {
                    seq,
                    event: Arc::clone(&event),
                })
                .is_ok()
        });
    }

    /// Number of live subscribers.
    #[cfg(test)]
    pub fn subscriber_count(&self) -> usize {
        self.lock().len()
    }

    fn set_topics(&self, id: u64, topics: Vec<Topic>) {
        if let Some(sub) = self.lock().iter_mut().find(|s| s.id == id) {
            sub.topics = topics;
        }
    }

    fn remove(&self, id: u64) {
        self.lock().retain(|s| s.id != id);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<Subscriber>> {
        self.subscribers.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Whether a subscriber with `topics` should receive `event`.
fn wants(topics: &[Topic], event: &Event) -> bool {
    match event.topic() {
        Some(topic) => topics.contains(&topic),
        None => true,
    }
}

/// Handle a connection holds for its subscriber entry. Dropping it unregisters.
pub struct Subscription {
    bus: Arc<EventBus>,
    id: u64,
}

impl Subscription {
    /// Replaces the topics this subscriber receives.
    pub fn set_topics(&self, topics: &[Topic]) {
        self.bus.set_topics(self.id, topics.to_vec());
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        self.bus.remove(self.id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clip_event() -> Event {
        Event::ClipSaved {
            clip_id: "1".to_owned(),
            path: "x.mp4".to_owned(),
            duration_ms: 4000,
            bytes: 10,
        }
    }

    #[test]
    fn only_matching_topics_receive() {
        let bus = Arc::new(EventBus::new());
        let (sub, rx) = bus.subscribe();
        sub.set_topics(&[Topic::Sessions]);
        bus.publish(clip_event());
        assert!(rx.try_recv().is_err());

        sub.set_topics(&[Topic::Clips]);
        bus.publish(clip_event());
        let got = rx.try_recv().expect("clip event should be delivered");
        assert_eq!(got.seq, 2);
    }

    #[test]
    fn concurrent_publishers_deliver_strictly_increasing_seq() {
        const THREADS: u64 = 8;
        const PER_THREAD: u64 = 500;
        let bus = Arc::new(EventBus::new());
        let mut subs = Vec::new();
        let mut receivers = Vec::new();
        for _ in 0..3 {
            let (sub, rx) = bus.subscribe();
            sub.set_topics(&[Topic::Clips]);
            subs.push(sub);
            receivers.push(rx);
        }

        let handles: Vec<_> = (0..THREADS)
            .map(|_| {
                let bus = Arc::clone(&bus);
                std::thread::spawn(move || {
                    for _ in 0..PER_THREAD {
                        bus.publish(clip_event());
                    }
                })
            })
            .collect();
        for handle in handles {
            handle.join().expect("publisher thread");
        }

        let total = THREADS * PER_THREAD;
        for rx in &receivers {
            let seqs: Vec<u64> = rx.try_iter().map(|d| d.seq).collect();
            let expected: Vec<u64> = (1..=total).collect();
            assert_eq!(seqs, expected, "every seq once, in order, no gaps");
        }
        drop(subs);
    }

    #[test]
    fn deliveries_share_one_event_allocation() {
        let bus = Arc::new(EventBus::new());
        let (sub, rx) = bus.subscribe();
        sub.set_topics(&[Topic::Clips]);
        let (sub2, rx2) = bus.subscribe();
        sub2.set_topics(&[Topic::Clips]);
        bus.publish(clip_event());
        let a = rx.try_recv().expect("first subscriber");
        let b = rx2.try_recv().expect("second subscriber");
        assert!(Arc::ptr_eq(&a.event, &b.event));
    }

    #[test]
    fn dropped_subscription_is_removed() {
        let bus = Arc::new(EventBus::new());
        let (sub, _rx) = bus.subscribe();
        assert_eq!(bus.subscriber_count(), 1);
        drop(sub);
        assert_eq!(bus.subscriber_count(), 0);
    }

    #[test]
    fn dead_receiver_is_pruned_on_publish() {
        let bus = Arc::new(EventBus::new());
        let (sub, rx) = bus.subscribe();
        sub.set_topics(&[Topic::Clips]);
        drop(rx);
        bus.publish(clip_event());
        assert_eq!(bus.subscriber_count(), 0);
        drop(sub);
    }
}
