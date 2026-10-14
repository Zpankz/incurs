use serde_json::{Value, json};

/// JSON-RPC invalid params: malformed arguments, URL, or secret.
pub const INVALID_PARAMS: i64 = -32602;
/// JSON-RPC method not found.
pub const METHOD_NOT_FOUND: i64 = -32601;
/// Callback verification failed. ChatGPT's guide uses -32015; the design sketch
/// numbers it -32027. This crate follows the shipped client.
pub const CALLBACK_ENDPOINT_ERROR: i64 = -32015;
/// Unknown event name.
pub const NOT_FOUND: i64 = -32023;
/// The principal may not subscribe.
pub const FORBIDDEN: i64 = -32024;
/// A requested option, such as a delivery mode, is not offered.
pub const UNSUPPORTED: i64 = -32026;

/// One event type a server offers.
#[derive(Debug, Clone, PartialEq)]
pub struct EventDefinition {
    pub name: String,
    pub description: String,
    /// JSON Schema for the subscription `arguments`.
    pub input_schema: Value,
    /// JSON Schema for a delivered event's `data`.
    pub payload_schema: Value,
}

impl EventDefinition {
    pub(crate) fn to_wire(&self) -> Value {
        json!({
            "name": self.name,
            "description": self.description,
            "delivery": ["webhook"],
            "inputSchema": self.input_schema,
            "payloadSchema": self.payload_schema,
        })
    }
}

/// A verified, stored webhook subscription.
#[derive(Debug, Clone, PartialEq)]
pub struct Subscription {
    pub id: String,
    pub principal: String,
    pub name: String,
    pub arguments: Value,
    pub url: String,
    /// The client's `whsec_` secret, as supplied.
    pub secret: String,
    /// Unix milliseconds after which nothing is delivered.
    pub refresh_before_ms: i64,
}

/// One event, as delivered in a webhook body.
#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    /// Stable across retries; also sent as `webhook-id`.
    pub event_id: String,
    pub name: String,
    /// RFC 3339 time the event happened.
    pub timestamp: String,
    pub data: Value,
    pub cursor: Option<String>,
}

impl Event {
    pub(crate) fn to_wire(&self) -> Value {
        json!({
            "eventId": self.event_id,
            "name": self.name,
            "timestamp": self.timestamp,
            "data": self.data,
            "cursor": self.cursor,
        })
    }

    pub(crate) fn from_wire(value: &Value) -> Option<Self> {
        Some(Self {
            event_id: value.get("eventId")?.as_str()?.to_string(),
            name: value.get("name")?.as_str()?.to_string(),
            timestamp: value.get("timestamp")?.as_str()?.to_string(),
            data: value.get("data")?.clone(),
            cursor: value
                .get("cursor")
                .and_then(Value::as_str)
                .map(str::to_string),
        })
    }
}

/// A JSON-RPC error raised by the router or the provider.
#[derive(Debug, Clone, PartialEq)]
pub struct EventsError {
    pub code: i64,
    pub message: String,
    pub data: Option<Value>,
}

impl EventsError {
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }

    pub fn with_data(mut self, data: Value) -> Self {
        self.data = Some(data);
        self
    }

    pub fn invalid_params(message: impl Into<String>) -> Self {
        Self::new(INVALID_PARAMS, message)
    }

    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::new(FORBIDDEN, message)
    }

    pub(crate) fn callback(reason: &str) -> Self {
        Self::new(
            CALLBACK_ENDPOINT_ERROR,
            "Callback endpoint verification failed",
        )
        .with_data(json!({ "reason": reason }))
    }

    pub(crate) fn to_wire(&self) -> Value {
        let mut error = json!({ "code": self.code, "message": self.message });
        if let Some(data) = &self.data {
            error["data"] = data.clone();
        }
        error
    }
}
