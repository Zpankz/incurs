//! A Worker that offers one event, `thing.changed`, over MCP Events webhooks.
//!
//! - `POST /mcp`: JSON-RPC `server/discover` and `events/*`. The `x-principal`
//!   header stands in for authentication.
//! - `POST /publish`: `{eventId, name, attributes, data}` queues the event for
//!   matching subscriptions; the Durable Object alarm delivers it.
#![cfg(target_arch = "wasm32")]

use std::time::Duration;

use incurs_mcp_events::{
    DeliveryPolicy, Event, EventDefinition, EventProvider, EventsError, EventsRouter, Lifetimes,
    RouterConfig, augment_discovery, drain, fan_out, is_events_request, rfc3339_from_ms,
};
use incurs_mcp_events_cloudflare::{
    DurableEventsStore, WorkersClock, WorkersEntropy, WorkersWebhookClient, schedule_drain,
};
use serde_json::{Value, json};
use worker::{
    Context, DurableObject, Env, Method, Request, Response, Result, State, durable_object, event,
    wasm_bindgen,
};

const DRAIN_BATCH: usize = 50;

#[event(fetch)]
async fn fetch(request: Request, env: Env, _context: Context) -> Result<Response> {
    env.durable_object("EVENTS")?
        .id_from_name("hub")?
        .get_stub()?
        .fetch_with_request(request)
        .await
}

struct ThingProvider;

#[async_trait::async_trait(?Send)]
impl EventProvider for ThingProvider {
    fn definitions(&self) -> Vec<EventDefinition> {
        vec![EventDefinition {
            name: "thing.changed".into(),
            description: "A thing changed.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {"thing": {"type": "string"}},
                "required": ["thing"]
            }),
            payload_schema: json!({"type": "object"}),
        }]
    }

    async fn authorize(
        &self,
        _principal: &str,
        _name: &str,
        arguments: &Value,
    ) -> std::result::Result<(), EventsError> {
        if arguments.get("thing").and_then(Value::as_str).is_some() {
            Ok(())
        } else {
            Err(EventsError::invalid_params("arguments.thing is required"))
        }
    }
}

#[durable_object]
pub struct EventsHub {
    state: State,
    env: Env,
}

impl DurableObject for EventsHub {
    fn new(state: State, env: Env) -> Self {
        Self { state, env }
    }

    async fn fetch(&self, mut request: Request) -> Result<Response> {
        if request.method() != Method::Post {
            return Response::error("Method not allowed", 405);
        }
        let store = DurableEventsStore::new(self.state.storage().sql())?;
        let body: Value = request.json().await?;
        match request.path().as_str() {
            "/mcp" => {
                let principal = request.headers().get("x-principal")?.unwrap_or_default();
                Response::from_json(&self.rpc(&store, &principal, &body).await)
            }
            "/publish" => self.publish(&store, &body).await,
            _ => Response::error("Not found", 404),
        }
    }

    async fn alarm(&self) -> Result<Response> {
        let store = DurableEventsStore::new(self.state.storage().sql())?;
        let clock = WorkersClock;
        store
            .purge_expired(worker::Date::now().as_millis() as i64)
            .map_err(worker::Error::RustError)?;
        let next = drain(
            &store,
            &store,
            &self.client(),
            &clock,
            &DeliveryPolicy::default(),
            DRAIN_BATCH,
        )
        .await
        .map_err(worker::Error::RustError)?;
        schedule_drain(
            &self.state.storage(),
            next,
            worker::Date::now().as_millis() as i64,
        )
        .await?;
        Response::ok("")
    }
}

impl EventsHub {
    fn client(&self) -> WorkersWebhookClient {
        let timeout = self
            .env
            .var("WEBHOOK_TIMEOUT_MS")
            .ok()
            .and_then(|value| value.to_string().parse().ok())
            .unwrap_or(10_000);
        WorkersWebhookClient::new(Duration::from_millis(timeout))
    }

    async fn rpc(&self, store: &DurableEventsStore, principal: &str, request: &Value) -> Value {
        if is_events_request(request) {
            let allow_local_testing = self
                .env
                .var("ALLOW_LOCAL_TESTING")
                .is_ok_and(|value| value.to_string() == "true");
            let client = self.client();
            let router = EventsRouter {
                provider: &ThingProvider,
                store,
                client: &client,
                clock: &WorkersClock,
                entropy: &WorkersEntropy,
                config: RouterConfig {
                    lifetimes: Lifetimes {
                        min_ms: 60_000,
                        max_ms: 86_400_000,
                    },
                    allow_local_testing,
                },
            };
            return router.handle(principal, request).await;
        }
        let id = request.get("id").cloned().unwrap_or(Value::Null);
        if request.get("method").and_then(Value::as_str) != Some("server/discover") {
            return json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": "Method not found"}});
        }
        let mut response = json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {"resultType": "complete", "supportedVersions": ["2026-07-28"], "capabilities": {}}
        });
        augment_discovery(request, &mut response);
        response
    }

    async fn publish(&self, store: &DurableEventsStore, body: &Value) -> Result<Response> {
        let now = worker::Date::now().as_millis() as i64;
        let field = |name: &str| body.get(name).and_then(Value::as_str).map(str::to_string);
        let (Some(event_id), Some(name)) = (field("eventId"), field("name")) else {
            return Response::error("eventId and name are required", 400);
        };
        let event = Event {
            event_id,
            name,
            timestamp: rfc3339_from_ms(now),
            data: body.get("data").cloned().unwrap_or_else(|| json!({})),
            cursor: None,
        };
        let attributes = body.get("attributes").cloned().unwrap_or_else(|| json!({}));
        let matched = fan_out(store, &event, &attributes, now)
            .await
            .map_err(worker::Error::RustError)?;
        // Deliver from the alarm so publishing never waits on receivers.
        schedule_drain(&self.state.storage(), Some(now), now).await?;
        Response::from_json(&json!({ "matched": matched }))
    }
}
