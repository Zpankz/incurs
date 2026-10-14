//! The receiving side: answer verification, check signatures, drop duplicates.

use serde_json::{Value, json};

use crate::host::DedupeStore;
use crate::signing::{Secret, verify};
use crate::wire::Event;

#[derive(Debug, Clone, PartialEq)]
pub enum ReceiverOutcome {
    /// A verification request; answer 200 with the echoed challenge.
    Verification { challenge: String },
    /// A new event.
    Accepted {
        event: Event,
        subscription_id: Option<String>,
    },
    /// An event whose `webhook-id` was already accepted.
    Duplicate,
    /// Missing or failing signature, or a stale timestamp.
    Unauthorized,
    /// Signed but not a verification request or an event.
    BadRequest,
    /// The dedupe record could not be written; the sender should retry.
    Unavailable,
}

impl ReceiverOutcome {
    /// HTTP status to answer with. Duplicates are acknowledged so the sender stops.
    pub fn status(&self) -> u16 {
        match self {
            Self::Verification { .. } | Self::Accepted { .. } | Self::Duplicate => 200,
            Self::Unauthorized => 401,
            Self::BadRequest => 400,
            Self::Unavailable => 503,
        }
    }

    pub fn response_body(&self) -> Option<Value> {
        match self {
            Self::Verification { challenge } => Some(json!({ "challenge": challenge })),
            _ => None,
        }
    }
}

/// Handle one received webhook request over its raw body bytes.
pub async fn receive(
    secret: &Secret,
    dedupe: &dyn DedupeStore,
    headers: &[(String, String)],
    body: &[u8],
    now_ms: i64,
) -> ReceiverOutcome {
    let header = |name: &str| {
        headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    };
    let (Some(id), Some(timestamp), Some(signature)) = (
        header("webhook-id"),
        header("webhook-timestamp"),
        header("webhook-signature"),
    ) else {
        return ReceiverOutcome::Unauthorized;
    };
    if verify(
        secret,
        id,
        timestamp,
        signature,
        body,
        now_ms.div_euclid(1000),
    )
    .is_err()
    {
        return ReceiverOutcome::Unauthorized;
    }
    let Ok(value) = serde_json::from_slice::<Value>(body) else {
        return ReceiverOutcome::BadRequest;
    };
    if value.get("type").and_then(Value::as_str) == Some("verification") {
        return match value.get("challenge").and_then(Value::as_str) {
            Some(challenge) => ReceiverOutcome::Verification {
                challenge: challenge.to_string(),
            },
            None => ReceiverOutcome::BadRequest,
        };
    }
    let Some(event) = Event::from_wire(&value) else {
        return ReceiverOutcome::BadRequest;
    };
    match dedupe.first_seen(id, now_ms).await {
        Ok(true) => ReceiverOutcome::Accepted {
            event,
            subscription_id: header("X-MCP-Subscription-Id").map(str::to_string),
        },
        Ok(false) => ReceiverOutcome::Duplicate,
        Err(_) => ReceiverOutcome::Unavailable,
    }
}
