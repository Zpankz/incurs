//! Cloudflare host for `incurs-mcp-events`.
//!
//! - [`DurableEventsStore`]: Durable Object SQLite for subscriptions, the
//!   delivery queue, and received message ids.
//! - [`WorkersWebhookClient`]: `fetch` with manual redirects and a timeout.
//! - [`WorkersClock`] and [`WorkersEntropy`]: `Date.now()` and `crypto.randomUUID()`.
//! - [`schedule_drain`]: point the Durable Object alarm at the next due delivery.
//!
//! A Durable Object hosting events calls `fan_out` then `schedule_drain` when it
//! publishes, and `drain` then `schedule_drain` from its alarm handler.

#[cfg(target_arch = "wasm32")]
mod client;
#[cfg(target_arch = "wasm32")]
mod runtime;
#[cfg(target_arch = "wasm32")]
mod store;

#[cfg(target_arch = "wasm32")]
pub use client::WorkersWebhookClient;
#[cfg(target_arch = "wasm32")]
pub use runtime::{WorkersClock, WorkersEntropy, schedule_drain};
#[cfg(target_arch = "wasm32")]
pub use store::DurableEventsStore;
