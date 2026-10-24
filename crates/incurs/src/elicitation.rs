//! Transport-neutral requests for human input during a command.

use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A form or external URL presented by an MCP client to its user.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "lowercase")]
pub enum ElicitationRequest {
    /// Requests structured input using the MCP form schema subset.
    Form {
        /// Explains why the input is needed.
        message: String,
        /// Object schema whose properties use MCP-supported primitive types.
        #[serde(rename = "requestedSchema")]
        requested_schema: Value,
    },
    /// Directs the user to an external HTTPS page, including for sensitive input.
    Url {
        /// Explains what the user needs to do.
        message: String,
        /// HTTPS URL opened by the client.
        url: String,
        /// Host-generated identifier for the external interaction.
        #[serde(rename = "elicitationId")]
        elicitation_id: String,
    },
}

/// The user's explicit response to an elicitation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ElicitationAction {
    /// The user submitted the form or confirmed the external interaction.
    Accept,
    /// The user declined the request.
    Decline,
    /// The user dismissed the request without accepting or declining.
    Cancel,
}

/// Client response to a request for human input.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ElicitationResponse {
    /// Whether the user accepted, declined, or dismissed the request.
    pub action: ElicitationAction,
    /// Submitted form values. Validate these before performing domain actions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<Value>,
}

/// Host implementation of an execution-scoped human input channel.
#[async_trait]
pub trait ElicitationHandler: Send + Sync {
    /// Requests human input, failing when its mode or transport is unsupported.
    async fn request(&self, request: ElicitationRequest) -> Result<ElicitationResponse, String>;
}

/// Cloneable execution-scoped human input channel supplied by a transport.
#[derive(Clone)]
pub struct Elicitation(Arc<dyn ElicitationHandler>);

impl std::fmt::Debug for Elicitation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Elicitation")
    }
}

impl Elicitation {
    /// Wraps a host-provided human input channel.
    pub fn new(handler: Arc<dyn ElicitationHandler>) -> Self {
        Self(handler)
    }

    /// Requests input without interpreting a decline or cancellation as acceptance.
    pub async fn request(
        &self,
        request: ElicitationRequest,
    ) -> Result<ElicitationResponse, String> {
        self.0.request(request).await
    }
}
