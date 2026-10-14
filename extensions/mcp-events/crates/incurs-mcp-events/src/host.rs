use serde_json::Value;

use crate::wire::{EventDefinition, EventsError, Subscription};

/// Wall clock in Unix milliseconds.
pub trait Clock {
    fn now_ms(&self) -> i64;
}

/// Unpredictable tokens for challenges and control message ids. Hosts back this
/// with a cryptographic random source.
pub trait Entropy {
    fn token(&self) -> String;
}

/// What a server offers and who may subscribe to it.
#[async_trait::async_trait(?Send)]
pub trait EventProvider {
    fn definitions(&self) -> Vec<EventDefinition>;

    /// Decide whether `principal` may subscribe to `name` with `arguments`.
    /// Return [`EventsError::forbidden`] or [`EventsError::invalid_params`] to refuse.
    async fn authorize(
        &self,
        principal: &str,
        name: &str,
        arguments: &Value,
    ) -> Result<(), EventsError>;
}

/// Durable subscription storage keyed by subscription id.
#[async_trait::async_trait(?Send)]
pub trait SubscriptionStore {
    /// Insert or replace.
    async fn put(&self, subscription: Subscription) -> Result<(), String>;
    /// Remove if present; removing an absent id succeeds.
    async fn remove(&self, id: &str) -> Result<(), String>;
}

/// Records which received message ids were already accepted.
#[async_trait::async_trait(?Send)]
pub trait DedupeStore {
    /// Record `id`; return `true` the first time it is seen.
    async fn first_seen(&self, id: &str, now_ms: i64) -> Result<bool, String>;
}

/// One outbound webhook POST.
#[derive(Debug, Clone)]
pub struct WebhookRequest {
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct WebhookResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TransportFailure {
    Timeout,
    ConnectionRefused,
    Tls,
    Other(String),
}

impl TransportFailure {
    pub(crate) fn reason(&self) -> &'static str {
        match self {
            Self::Timeout => "timeout",
            Self::Tls => "tls_error",
            Self::ConnectionRefused | Self::Other(_) => "connection_refused",
        }
    }
}

/// Sends webhook POSTs. Implementations must not follow redirects and must
/// bound the request time.
#[async_trait::async_trait(?Send)]
pub trait WebhookClient {
    async fn post(&self, request: WebhookRequest) -> Result<WebhookResponse, TransportFailure>;
}
