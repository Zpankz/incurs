//! Fan-out and draining over a host-persisted delivery queue.

use serde_json::Value;

use crate::delivery::{DeliveryOutcome, DeliveryPolicy, deliver};
use crate::host::{Clock, SubscriptionStore, WebhookClient};
use crate::wire::{Event, Subscription};

/// One queued attempt of one event for one subscription.
#[derive(Debug, Clone, PartialEq)]
pub struct QueuedDelivery {
    pub subscription: Subscription,
    pub event: Event,
    /// The attempt this entry makes next, starting at 1.
    pub attempt: u32,
    pub due_ms: i64,
}

/// Durable delivery queue, keyed by (subscription id, event id).
#[async_trait::async_trait(?Send)]
pub trait DeliveryQueue {
    /// Every subscription to `name` whose `refreshBefore` is after `now_ms`.
    async fn subscriptions_for(&self, name: &str, now_ms: i64)
    -> Result<Vec<Subscription>, String>;
    /// Insert attempt 1 due at `due_ms`; keep an existing entry for the same pair.
    async fn enqueue(
        &self,
        subscription_id: &str,
        event: &Event,
        due_ms: i64,
    ) -> Result<(), String>;
    /// Entries due at or before `now_ms`, earliest first, at most `limit`.
    async fn due(&self, now_ms: i64, limit: usize) -> Result<Vec<QueuedDelivery>, String>;
    async fn reschedule(
        &self,
        subscription_id: &str,
        event_id: &str,
        attempt: u32,
        due_ms: i64,
    ) -> Result<(), String>;
    async fn complete(&self, subscription_id: &str, event_id: &str) -> Result<(), String>;
    /// The earliest due time in the queue.
    async fn next_due(&self) -> Result<Option<i64>, String>;
}

/// True when every subscription argument equals the event attribute of the same name.
pub fn arguments_match(arguments: &Value, attributes: &Value) -> bool {
    match arguments.as_object() {
        Some(arguments) => arguments
            .iter()
            .all(|(name, expected)| attributes.get(name) == Some(expected)),
        None => false,
    }
}

/// Queue `event` for every matching live subscription. Returns how many matched.
pub async fn fan_out(
    queue: &dyn DeliveryQueue,
    event: &Event,
    attributes: &Value,
    now_ms: i64,
) -> Result<usize, String> {
    let mut matched = 0;
    for subscription in queue.subscriptions_for(&event.name, now_ms).await? {
        if arguments_match(&subscription.arguments, attributes) {
            queue.enqueue(&subscription.id, event, now_ms).await?;
            matched += 1;
        }
    }
    Ok(matched)
}

/// Attempt every due entry and return the earliest due time left.
pub async fn drain(
    queue: &dyn DeliveryQueue,
    store: &dyn SubscriptionStore,
    client: &dyn WebhookClient,
    clock: &dyn Clock,
    policy: &DeliveryPolicy,
    limit: usize,
) -> Result<Option<i64>, String> {
    let now = clock.now_ms();
    for entry in queue.due(now, limit).await? {
        let outcome = deliver(
            client,
            store,
            clock,
            &entry.subscription,
            &entry.event,
            entry.attempt,
            policy,
        )
        .await?;
        let (subscription_id, event_id) = (&entry.subscription.id, &entry.event.event_id);
        match outcome {
            DeliveryOutcome::Retry { after_ms } => {
                queue
                    .reschedule(
                        subscription_id,
                        event_id,
                        entry.attempt + 1,
                        // Count the delay from when the attempt finished, so it is
                        // the minimum gap between attempts.
                        clock.now_ms() + after_ms,
                    )
                    .await?
            }
            _ => queue.complete(subscription_id, event_id).await?,
        }
    }
    queue.next_due().await
}
