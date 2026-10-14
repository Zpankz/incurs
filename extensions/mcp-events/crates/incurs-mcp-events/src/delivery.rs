//! One delivery attempt and the decision that follows it. Hosts persist the
//! queue and schedule retries; this module decides what happens next.

use crate::host::{Clock, SubscriptionStore, WebhookClient, WebhookRequest};
use crate::signing::{Secret, signed_headers};
use crate::wire::{Event, Subscription};

/// The largest request body a receiver accepts: 256 KiB.
pub const MAX_BODY_BYTES: usize = 262_144;

#[derive(Debug, Clone, Copy)]
pub struct DeliveryPolicy {
    /// Total attempts, including the first.
    pub max_attempts: u32,
    /// Delay before the second attempt; each later delay doubles.
    pub base_delay_ms: i64,
}

impl Default for DeliveryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 8,
            base_delay_ms: 1000,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum DeliveryOutcome {
    /// The receiver answered 2xx.
    Delivered,
    /// Transient failure; attempt again after this delay with the same event.
    Retry { after_ms: i64 },
    /// Transient failures used every attempt.
    GaveUp,
    /// The receiver answered 410; the subscription was removed.
    Ended,
    /// The receiver refused this event permanently (413 or another 4xx).
    Rejected,
    /// The body exceeds [`MAX_BODY_BYTES`]; nothing was sent.
    TooLarge,
    /// The subscription's `refreshBefore` has passed; nothing was sent.
    Expired,
}

/// Attempt `attempt` (starting at 1) of delivering `event` to `subscription`.
pub async fn deliver(
    client: &dyn WebhookClient,
    store: &dyn SubscriptionStore,
    clock: &dyn Clock,
    subscription: &Subscription,
    event: &Event,
    attempt: u32,
    policy: &DeliveryPolicy,
) -> Result<DeliveryOutcome, String> {
    let now = clock.now_ms();
    if now >= subscription.refresh_before_ms {
        return Ok(DeliveryOutcome::Expired);
    }
    let body = serde_json::to_vec(&event.to_wire()).map_err(|error| error.to_string())?;
    if body.len() > MAX_BODY_BYTES {
        return Ok(DeliveryOutcome::TooLarge);
    }
    let secret = Secret::parse(&subscription.secret).map_err(|error| error.to_string())?;
    let headers = signed_headers(
        &secret,
        &event.event_id,
        now.div_euclid(1000),
        &body,
        &subscription.id,
    );
    let transient = match client
        .post(WebhookRequest {
            url: subscription.url.clone(),
            headers,
            body,
        })
        .await
    {
        Ok(response) => match response.status {
            200..=299 => return Ok(DeliveryOutcome::Delivered),
            410 => {
                store.remove(&subscription.id).await?;
                return Ok(DeliveryOutcome::Ended);
            }
            408 | 429 | 500..=599 => true,
            _ => false,
        },
        Err(_) => true,
    };
    if !transient {
        return Ok(DeliveryOutcome::Rejected);
    }
    if attempt >= policy.max_attempts {
        return Ok(DeliveryOutcome::GaveUp);
    }
    let exponent = attempt.saturating_sub(1).min(30);
    Ok(DeliveryOutcome::Retry {
        after_ms: policy.base_delay_ms.saturating_mul(1 << exponent),
    })
}
