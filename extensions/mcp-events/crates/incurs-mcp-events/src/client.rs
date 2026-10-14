//! Calls `events/*` on a remote MCP server, for a host that subscribes to
//! another server's events.

use serde_json::{Value, json};

use crate::host::{TransportFailure, WebhookClient, WebhookRequest};
use crate::wire::EventsError;

/// The MCP protocol version sent with every call.
pub const CLIENT_PROTOCOL_VERSION: &str = "2026-07-28";

/// A remote MCP endpoint and the credentials to call it.
pub struct EventsClient<'a> {
    /// Any POST transport; a webhook client works.
    pub http: &'a dyn WebhookClient,
    pub endpoint: String,
    /// Full `Authorization` header value, for example `Bearer <token>`.
    pub authorization: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SubscribeResult {
    pub id: String,
    pub refresh_before: Option<String>,
    pub cursor: Option<String>,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ClientError {
    /// The server answered with a JSON-RPC error.
    Rpc(EventsError),
    /// The server answered with a status outside 2xx.
    Http(u16),
    Transport(TransportFailure),
    /// The response could not be read as the expected JSON-RPC response.
    Malformed(String),
}

impl EventsClient<'_> {
    pub async fn list(&self) -> Result<Vec<Value>, ClientError> {
        let result = self.call("events/list", json!({})).await?;
        result
            .get("events")
            .and_then(Value::as_array)
            .cloned()
            .ok_or_else(|| ClientError::Malformed("result.events is not an array".into()))
    }

    pub async fn subscribe(
        &self,
        name: &str,
        arguments: &Value,
        callback_url: &str,
        secret: &str,
        ttl_ms: Option<i64>,
    ) -> Result<SubscribeResult, ClientError> {
        let mut params = json!({
            "name": name,
            "arguments": arguments,
            "delivery": {"mode": "webhook", "url": callback_url, "secret": secret},
            "cursor": null,
        });
        if let Some(ttl) = ttl_ms {
            params["ttlMs"] = json!(ttl);
        }
        let result = self.call("events/subscribe", params).await?;
        let id = result
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| ClientError::Malformed("result.id is not a string".into()))?;
        let text = |key: &str| result.get(key).and_then(Value::as_str).map(str::to_string);
        Ok(SubscribeResult {
            id: id.to_string(),
            refresh_before: text("refreshBefore"),
            cursor: text("cursor"),
            truncated: result
                .get("truncated")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        })
    }

    pub async fn unsubscribe(
        &self,
        name: &str,
        arguments: &Value,
        callback_url: &str,
    ) -> Result<(), ClientError> {
        let params = json!({
            "name": name,
            "arguments": arguments,
            "delivery": {"mode": "webhook", "url": callback_url},
        });
        self.call("events/unsubscribe", params).await.map(|_| ())
    }

    async fn call(&self, method: &str, params: Value) -> Result<Value, ClientError> {
        let id = 1;
        let body = serde_json::to_vec(
            &json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}),
        )
        .map_err(|error| ClientError::Malformed(error.to_string()))?;
        let mut headers = vec![
            ("Content-Type".to_string(), "application/json".to_string()),
            (
                "Accept".to_string(),
                "application/json, text/event-stream".to_string(),
            ),
            (
                "MCP-Protocol-Version".to_string(),
                CLIENT_PROTOCOL_VERSION.to_string(),
            ),
        ];
        if let Some(authorization) = &self.authorization {
            headers.push(("Authorization".to_string(), authorization.clone()));
        }
        let response = self
            .http
            .post(WebhookRequest {
                url: self.endpoint.clone(),
                headers,
                body,
            })
            .await
            .map_err(ClientError::Transport)?;
        if !(200..300).contains(&response.status) {
            return Err(ClientError::Http(response.status));
        }
        let message = response_message(&response.body, id)?;
        if let Some(error) = message.get("error") {
            return Err(ClientError::Rpc(EventsError {
                code: error.get("code").and_then(Value::as_i64).unwrap_or(0),
                message: error
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                data: error.get("data").cloned(),
            }));
        }
        message
            .get("result")
            .cloned()
            .ok_or_else(|| ClientError::Malformed("response has neither result nor error".into()))
    }
}

/// The JSON-RPC response with `id`, from a JSON body or an event-stream body.
fn response_message(body: &[u8], id: i64) -> Result<Value, ClientError> {
    let text = std::str::from_utf8(body)
        .map_err(|_| ClientError::Malformed("body is not UTF-8".into()))?;
    if text.trim_start().starts_with('{') {
        return serde_json::from_str(text)
            .map_err(|error| ClientError::Malformed(error.to_string()));
    }
    text.lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .filter_map(|data| serde_json::from_str::<Value>(data.trim()).ok())
        .find(|message| message.get("id").and_then(Value::as_i64) == Some(id))
        .ok_or_else(|| {
            ClientError::Malformed("event stream has no response for the request".into())
        })
}
