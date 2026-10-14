//! Acceptance tests A14 and A15 of `docs/mcp-events.md`: fan-out and draining.

use std::cell::{Cell, RefCell};

use incurs_mcp_events::{
    Clock, DeliveryPolicy, DeliveryQueue, Event, QueuedDelivery, Subscription, SubscriptionStore,
    TransportFailure, WebhookClient, WebhookRequest, WebhookResponse, drain, fan_out,
};
use serde_json::{Value, json};

const SECRET: &str = "whsec_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
const HOUR_MS: i64 = 3_600_000;

struct FixedClock(Cell<i64>);
impl Clock for FixedClock {
    fn now_ms(&self) -> i64 {
        self.0.get()
    }
}

/// Answers by URL path: `/ok` gives 200, `/busy` gives 503.
#[derive(Default)]
struct PathClient(RefCell<Vec<String>>);

#[async_trait::async_trait(?Send)]
impl WebhookClient for PathClient {
    async fn post(&self, request: WebhookRequest) -> Result<WebhookResponse, TransportFailure> {
        self.0.borrow_mut().push(request.url.clone());
        let status = if request.url.ends_with("/busy") {
            503
        } else {
            200
        };
        Ok(WebhookResponse {
            status,
            body: Vec::new(),
        })
    }
}

#[derive(Default)]
struct MemoryQueue {
    subscriptions: RefCell<Vec<Subscription>>,
    entries: RefCell<Vec<QueuedDelivery>>,
}

#[async_trait::async_trait(?Send)]
impl SubscriptionStore for MemoryQueue {
    async fn put(&self, subscription: Subscription) -> Result<(), String> {
        self.subscriptions
            .borrow_mut()
            .retain(|known| known.id != subscription.id);
        self.subscriptions.borrow_mut().push(subscription);
        Ok(())
    }
    async fn remove(&self, id: &str) -> Result<(), String> {
        self.subscriptions
            .borrow_mut()
            .retain(|known| known.id != id);
        Ok(())
    }
}

#[async_trait::async_trait(?Send)]
impl DeliveryQueue for MemoryQueue {
    async fn subscriptions_for(
        &self,
        name: &str,
        now_ms: i64,
    ) -> Result<Vec<Subscription>, String> {
        Ok(self
            .subscriptions
            .borrow()
            .iter()
            .filter(|subscription| {
                subscription.name == name && subscription.refresh_before_ms > now_ms
            })
            .cloned()
            .collect())
    }
    async fn enqueue(
        &self,
        subscription_id: &str,
        event: &Event,
        due_ms: i64,
    ) -> Result<(), String> {
        let mut entries = self.entries.borrow_mut();
        if entries.iter().any(|entry| {
            entry.subscription.id == subscription_id && entry.event.event_id == event.event_id
        }) {
            return Ok(());
        }
        let subscription = self
            .subscriptions
            .borrow()
            .iter()
            .find(|known| known.id == subscription_id)
            .cloned()
            .ok_or("unknown subscription")?;
        entries.push(QueuedDelivery {
            subscription,
            event: event.clone(),
            attempt: 1,
            due_ms,
        });
        Ok(())
    }
    async fn due(&self, now_ms: i64, limit: usize) -> Result<Vec<QueuedDelivery>, String> {
        let mut due: Vec<_> = self
            .entries
            .borrow()
            .iter()
            .filter(|entry| entry.due_ms <= now_ms)
            .cloned()
            .collect();
        due.sort_by_key(|entry| entry.due_ms);
        due.truncate(limit);
        Ok(due)
    }
    async fn reschedule(
        &self,
        subscription_id: &str,
        event_id: &str,
        attempt: u32,
        due_ms: i64,
    ) -> Result<(), String> {
        for entry in self.entries.borrow_mut().iter_mut() {
            if entry.subscription.id == subscription_id && entry.event.event_id == event_id {
                entry.attempt = attempt;
                entry.due_ms = due_ms;
            }
        }
        Ok(())
    }
    async fn complete(&self, subscription_id: &str, event_id: &str) -> Result<(), String> {
        self.entries.borrow_mut().retain(|entry| {
            !(entry.subscription.id == subscription_id && entry.event.event_id == event_id)
        });
        Ok(())
    }
    async fn next_due(&self) -> Result<Option<i64>, String> {
        Ok(self.entries.borrow().iter().map(|entry| entry.due_ms).min())
    }
}

fn subscription(
    id: &str,
    name: &str,
    arguments: Value,
    url: &str,
    refresh_before_ms: i64,
) -> Subscription {
    Subscription {
        id: id.into(),
        principal: "p1".into(),
        name: name.into(),
        arguments,
        url: url.into(),
        secret: SECRET.into(),
        refresh_before_ms,
    }
}

fn event(id: &str) -> Event {
    Event {
        event_id: id.into(),
        name: "comment.created".into(),
        timestamp: "2026-10-14T00:00:00Z".into(),
        data: json!({}),
        cursor: None,
    }
}

#[tokio::test]
async fn a14_fan_out() {
    let queue = MemoryQueue::default();
    let now = 1_000_000;
    for item in [
        subscription(
            "doc1",
            "comment.created",
            json!({"document_id": "doc_1"}),
            "https://a.example/ok",
            now + HOUR_MS,
        ),
        subscription(
            "doc2",
            "comment.created",
            json!({"document_id": "doc_2"}),
            "https://a.example/ok",
            now + HOUR_MS,
        ),
        subscription(
            "all",
            "comment.created",
            json!({}),
            "https://a.example/ok",
            now + HOUR_MS,
        ),
        subscription(
            "other",
            "comment.deleted",
            json!({}),
            "https://a.example/ok",
            now + HOUR_MS,
        ),
        subscription(
            "expired",
            "comment.created",
            json!({}),
            "https://a.example/ok",
            now - 1,
        ),
    ] {
        queue.put(item).await.unwrap();
    }
    let attributes = json!({"document_id": "doc_1"});
    fan_out(&queue, &event("evt_1"), &attributes, now)
        .await
        .unwrap();
    fan_out(&queue, &event("evt_1"), &attributes, now)
        .await
        .unwrap();
    let mut queued: Vec<_> = queue
        .entries
        .borrow()
        .iter()
        .map(|entry| entry.subscription.id.clone())
        .collect();
    queued.sort();
    assert_eq!(queued, vec!["all".to_string(), "doc1".to_string()]);
    assert!(
        queue
            .entries
            .borrow()
            .iter()
            .all(|entry| entry.attempt == 1 && entry.due_ms == now)
    );
}

#[tokio::test]
async fn a15_drain() {
    let queue = MemoryQueue::default();
    let far = 10 * HOUR_MS;
    queue
        .put(subscription(
            "ok10",
            "comment.created",
            json!({}),
            "https://a.example/ok",
            far,
        ))
        .await
        .unwrap();
    queue
        .put(subscription(
            "busy20",
            "comment.created",
            json!({}),
            "https://a.example/busy",
            far,
        ))
        .await
        .unwrap();
    queue
        .put(subscription(
            "ok90",
            "comment.created",
            json!({}),
            "https://b.example/ok",
            far,
        ))
        .await
        .unwrap();
    queue
        .enqueue("ok10", &event("evt_a"), 10_000)
        .await
        .unwrap();
    queue
        .enqueue("busy20", &event("evt_b"), 20_000)
        .await
        .unwrap();
    queue
        .enqueue("ok90", &event("evt_c"), 90_000)
        .await
        .unwrap();

    let client = PathClient::default();
    let clock = FixedClock(Cell::new(30_000));
    let policy = DeliveryPolicy {
        max_attempts: 3,
        base_delay_ms: 1000,
    };

    let next = drain(&queue, &queue, &client, &clock, &policy, 10)
        .await
        .unwrap();
    assert_eq!(next, Some(31_000));
    assert_eq!(
        *client.0.borrow(),
        vec![
            "https://a.example/ok".to_string(),
            "https://a.example/busy".to_string()
        ]
    );
    {
        let entries = queue.entries.borrow();
        assert_eq!(entries.len(), 2);
        let busy = entries
            .iter()
            .find(|entry| entry.subscription.id == "busy20")
            .unwrap();
        assert_eq!((busy.attempt, busy.due_ms), (2, 31_000));
        assert!(
            entries
                .iter()
                .any(|entry| entry.subscription.id == "ok90" && entry.due_ms == 90_000)
        );
    }

    clock.0.set(31_000);
    let next = drain(&queue, &queue, &client, &clock, &policy, 10)
        .await
        .unwrap();
    assert_eq!(next, Some(33_000));
    assert_eq!(client.0.borrow().len(), 3);
    {
        let entries = queue.entries.borrow();
        let busy = entries
            .iter()
            .find(|entry| entry.subscription.id == "busy20")
            .unwrap();
        assert_eq!((busy.attempt, busy.due_ms), (3, 33_000));
    }

    let empty = MemoryQueue::default();
    assert_eq!(
        drain(&empty, &empty, &client, &clock, &policy, 10)
            .await
            .unwrap(),
        None
    );
}
