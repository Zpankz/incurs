//! JSON-RPC routing for `events/*` and the discovery capability.
//!
//! The router sits beside an existing MCP handler: the host sends `events/*`
//! requests here, passes every other request to its handler, and runs
//! [`augment_discovery`] over the handler's response.

use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::callback::validate_callback_url;
use crate::canonical::canonical_json;
use crate::host::{
    Clock, Entropy, EventProvider, SubscriptionStore, WebhookClient, WebhookRequest,
};
use crate::signing::{Secret, signed_headers};
use crate::time::rfc3339_from_ms;
use crate::wire::{EventsError, METHOD_NOT_FOUND, NOT_FOUND, Subscription, UNSUPPORTED};

/// Bounds on the lifetime granted to a subscription.
#[derive(Debug, Clone, Copy)]
pub struct Lifetimes {
    pub min_ms: i64,
    pub max_ms: i64,
}

#[derive(Debug, Clone)]
pub struct RouterConfig {
    pub lifetimes: Lifetimes,
    /// Accept loopback callback URLs, including `http`. Local testing only.
    pub allow_local_testing: bool,
}

pub struct EventsRouter<'a> {
    pub provider: &'a dyn EventProvider,
    pub store: &'a dyn SubscriptionStore,
    pub client: &'a dyn WebhookClient,
    pub clock: &'a dyn Clock,
    pub entropy: &'a dyn Entropy,
    pub config: RouterConfig,
}

/// True when the JSON-RPC request belongs to the events router.
pub fn is_events_request(request: &Value) -> bool {
    method(request).is_some_and(|name| name.starts_with("events/"))
}

/// Add `capabilities.events` to a `server/discover` or `initialize` result.
pub fn augment_discovery(request: &Value, response: &mut Value) {
    if !matches!(method(request), Some("server/discover" | "initialize")) {
        return;
    }
    if let Some(capabilities) = response
        .get_mut("result")
        .and_then(|result| result.get_mut("capabilities"))
        .and_then(Value::as_object_mut)
    {
        capabilities.insert("events".to_string(), json!({}));
    }
}

/// First 32 hex digits of SHA-256 over the canonical subscription key.
pub fn subscription_id(principal: &str, url: &str, name: &str, arguments: &Value) -> String {
    let key = canonical_json(&json!([principal, url, name, arguments]));
    let digest = Sha256::digest(key.as_bytes());
    digest
        .iter()
        .take(16)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn method(request: &Value) -> Option<&str> {
    request.get("method").and_then(Value::as_str)
}

impl EventsRouter<'_> {
    /// Answer one `events/*` JSON-RPC request for an authenticated principal.
    pub async fn handle(&self, principal: &str, request: &Value) -> Value {
        let id = request.get("id").cloned().unwrap_or(Value::Null);
        let params = request.get("params").cloned().unwrap_or_else(|| json!({}));
        let outcome = match method(request) {
            Some("events/list") => Ok(self.list()),
            Some("events/subscribe") => self.subscribe(principal, &params).await,
            Some("events/unsubscribe") => self.unsubscribe(principal, &params).await,
            other => Err(EventsError::new(
                METHOD_NOT_FOUND,
                format!("Method not found: {}", other.unwrap_or("")),
            )),
        };
        match outcome {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err(error) => json!({ "jsonrpc": "2.0", "id": id, "error": error.to_wire() }),
        }
    }

    fn list(&self) -> Value {
        let events: Vec<Value> = self
            .provider
            .definitions()
            .iter()
            .map(|event| event.to_wire())
            .collect();
        json!({ "events": events })
    }

    async fn subscribe(&self, principal: &str, params: &Value) -> Result<Value, EventsError> {
        let request = SubscribeParams::parse(params)?;
        if request.mode != "webhook" {
            return Err(EventsError::new(
                UNSUPPORTED,
                format!("delivery mode {} is not offered", request.mode),
            ));
        }
        if !self
            .provider
            .definitions()
            .iter()
            .any(|event| event.name == request.name)
        {
            return Err(
                EventsError::new(NOT_FOUND, format!("unknown event {}", request.name))
                    .with_data(json!({ "kind": "event" })),
            );
        }
        let secret_text = request
            .secret
            .ok_or_else(|| EventsError::invalid_params("delivery.secret is required"))?;
        let secret = Secret::parse(&secret_text)
            .map_err(|error| EventsError::invalid_params(error.to_string()))?;
        validate_callback_url(&request.url, self.config.allow_local_testing)
            .map_err(EventsError::invalid_params)?;
        if principal.is_empty() {
            return Err(EventsError::forbidden(
                "an authenticated principal is required",
            ));
        }
        self.provider
            .authorize(principal, &request.name, &request.arguments)
            .await?;

        let id = subscription_id(principal, &request.url, &request.name, &request.arguments);
        self.verify_callback(&id, &request.url, &secret).await?;

        let now = self.clock.now_ms();
        let refresh_before_ms = now + self.granted_lifetime(request.ttl_ms);
        self.store
            .put(Subscription {
                id: id.clone(),
                principal: principal.to_string(),
                name: request.name,
                arguments: request.arguments,
                url: request.url,
                secret: secret_text,
                refresh_before_ms,
            })
            .await
            .map_err(|error| {
                EventsError::new(-32603, format!("subscription store failed: {error}"))
            })?;
        Ok(json!({
            "id": id,
            "refreshBefore": rfc3339_from_ms(refresh_before_ms),
            "cursor": null,
            "truncated": false,
        }))
    }

    fn granted_lifetime(&self, requested: Option<i64>) -> i64 {
        let Lifetimes { min_ms, max_ms } = self.config.lifetimes;
        requested.map_or(max_ms, |ttl| ttl.clamp(min_ms, max_ms))
    }

    async fn verify_callback(
        &self,
        id: &str,
        url: &str,
        secret: &Secret,
    ) -> Result<(), EventsError> {
        let challenge = self.entropy.token();
        let body = serde_json::to_vec(&json!({ "type": "verification", "challenge": challenge }))
            .expect("verification body serializes");
        let message_id = format!("msg_verification_{}", self.entropy.token());
        let timestamp = self.clock.now_ms().div_euclid(1000);
        let headers = signed_headers(secret, &message_id, timestamp, &body, id);
        let response = self
            .client
            .post(WebhookRequest {
                url: url.to_string(),
                headers,
                body,
            })
            .await
            .map_err(|failure| EventsError::callback(failure.reason()))?;
        match response.status {
            200..=299 => {}
            400..=499 => return Err(EventsError::callback("http_4xx")),
            500..=599 => return Err(EventsError::callback("http_5xx")),
            _ => return Err(EventsError::callback("challenge_failed")),
        }
        let echoed = serde_json::from_slice::<Value>(&response.body)
            .ok()
            .and_then(|value| {
                value
                    .get("challenge")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .unwrap_or_default();
        if bool::from(echoed.as_bytes().ct_eq(challenge.as_bytes())) {
            Ok(())
        } else {
            Err(EventsError::callback("challenge_failed"))
        }
    }

    async fn unsubscribe(&self, principal: &str, params: &Value) -> Result<Value, EventsError> {
        let params = params
            .as_object()
            .ok_or_else(|| EventsError::invalid_params("params must be an object"))?;
        let name = required_str(params, "name")?;
        let arguments = arguments(params)?;
        let delivery = params
            .get("delivery")
            .and_then(Value::as_object)
            .ok_or_else(|| EventsError::invalid_params("delivery is required"))?;
        let url = required_str(delivery, "url")?;
        if principal.is_empty() {
            return Err(EventsError::forbidden(
                "an authenticated principal is required",
            ));
        }
        self.store
            .remove(&subscription_id(principal, &url, &name, &arguments))
            .await
            .map_err(|error| {
                EventsError::new(-32603, format!("subscription store failed: {error}"))
            })?;
        Ok(json!({}))
    }
}

struct SubscribeParams {
    name: String,
    arguments: Value,
    mode: String,
    url: String,
    secret: Option<String>,
    ttl_ms: Option<i64>,
}

impl SubscribeParams {
    fn parse(params: &Value) -> Result<Self, EventsError> {
        let params = params
            .as_object()
            .ok_or_else(|| EventsError::invalid_params("params must be an object"))?;
        let delivery = params
            .get("delivery")
            .and_then(Value::as_object)
            .ok_or_else(|| EventsError::invalid_params("delivery is required"))?;
        let ttl_ms = match params.get("ttlMs") {
            None | Some(Value::Null) => None,
            Some(value) => Some(value.as_i64().filter(|ttl| *ttl > 0).ok_or_else(|| {
                EventsError::invalid_params("ttlMs must be a positive integer or null")
            })?),
        };
        Ok(Self {
            name: required_str(params, "name")?,
            arguments: arguments(params)?,
            mode: required_str(delivery, "mode")?,
            url: required_str(delivery, "url")?,
            secret: delivery
                .get("secret")
                .and_then(Value::as_str)
                .map(str::to_string),
            ttl_ms,
        })
    }
}

fn required_str(object: &Map<String, Value>, key: &str) -> Result<String, EventsError> {
    object
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| EventsError::invalid_params(format!("{key} must be a string")))
}

fn arguments(params: &Map<String, Value>) -> Result<Value, EventsError> {
    match params.get("arguments") {
        None | Some(Value::Null) => Ok(json!({})),
        Some(value @ Value::Object(_)) => Ok(value.clone()),
        Some(_) => Err(EventsError::invalid_params("arguments must be an object")),
    }
}
