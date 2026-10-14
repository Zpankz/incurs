//! Platform-neutral MCP Events webhook delivery.
//!
//! Implements the webhook subset of the MCP Events draft as ChatGPT ships it:
//! `events/list`, `events/subscribe`, and `events/unsubscribe`, a signed
//! verification handshake, Standard Webhooks signing, delivery decisions, and a
//! receiver. Storage, HTTP, the clock, and randomness are host traits, so the
//! same core runs in a Cloudflare Worker, a native server, or a test.
//!
//! The specification is `docs/mcp-events.md` in the incurs repository.

mod callback;
mod canonical;
mod delivery;
mod host;
mod receiver;
mod router;
mod signing;
mod time;
mod wire;

pub use callback::validate_callback_url;
pub use delivery::{DeliveryOutcome, DeliveryPolicy, MAX_BODY_BYTES, deliver};
pub use host::{
    Clock, DedupeStore, Entropy, EventProvider, SubscriptionStore, TransportFailure, WebhookClient,
    WebhookRequest, WebhookResponse,
};
pub use receiver::{ReceiverOutcome, receive};
pub use router::{
    EventsRouter, Lifetimes, RouterConfig, augment_discovery, is_events_request, subscription_id,
};
pub use signing::{Secret, SecretError, VerifyError, sign, signed_headers, verify};
pub use time::rfc3339_from_ms;
pub use wire::{
    CALLBACK_ENDPOINT_ERROR, Event, EventDefinition, EventsError, FORBIDDEN, INVALID_PARAMS,
    METHOD_NOT_FOUND, NOT_FOUND, Subscription, UNSUPPORTED,
};
