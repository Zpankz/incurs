use incurs_mcp_events::{Clock, Entropy};
use wasm_bindgen::prelude::wasm_bindgen;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = crypto, js_name = randomUUID)]
    fn random_uuid() -> String;
}

/// `Date.now()`.
#[derive(Debug, Clone, Copy, Default)]
pub struct WorkersClock;

impl Clock for WorkersClock {
    fn now_ms(&self) -> i64 {
        worker::Date::now().as_millis() as i64
    }
}

/// `crypto.randomUUID()` without hyphens: 122 random bits per token.
#[derive(Debug, Clone, Copy, Default)]
pub struct WorkersEntropy;

impl Entropy for WorkersEntropy {
    fn token(&self) -> String {
        random_uuid().replace('-', "")
    }
}

/// Set the Durable Object alarm to `next_due_ms`, the value `drain` returns.
/// Setting an alarm replaces the previous one, which is correct because the
/// queue's earliest due time covers every entry. With no due time, any pending
/// alarm is left to fire and find an empty queue.
pub async fn schedule_drain(
    storage: &worker::Storage,
    next_due_ms: Option<i64>,
    now_ms: i64,
) -> worker::Result<()> {
    if let Some(due) = next_due_ms {
        // An integer is an offset from now, not an absolute time.
        storage.set_alarm((due - now_ms).max(0)).await?;
    }
    Ok(())
}
