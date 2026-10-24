use std::sync::Arc;

use rmcp::model::{ClientCapabilities, ElicitRequestParams, ProtocolVersion};
use rmcp::service::RequestContext;
use rmcp::{Peer, RoleServer};
use tokio_util::sync::CancellationToken;

use crate::elicitation::{
    Elicitation, ElicitationHandler, ElicitationRequest, ElicitationResponse,
};

struct ClientElicitation {
    peer: Peer<RoleServer>,
    capabilities: Option<ClientCapabilities>,
    version: Option<ProtocolVersion>,
    cancellation: CancellationToken,
}

/// Captures a session-based MCP request's human input channel.
///
/// The channel checks the client's declared modes and races requests against
/// cancellation. Stateless task approvals use Tasks input requests instead.
pub fn elicitation_for_request(context: &RequestContext<RoleServer>) -> Elicitation {
    Elicitation::new(Arc::new(ClientElicitation {
        peer: context.peer.clone(),
        capabilities: context.client_capabilities(),
        version: context.protocol_version(),
        cancellation: context.ct.clone(),
    }))
}

#[async_trait::async_trait]
impl ElicitationHandler for ClientElicitation {
    async fn request(&self, request: ElicitationRequest) -> Result<ElicitationResponse, String> {
        if !matches!(
            self.version.as_ref().map(ProtocolVersion::as_str),
            Some("2025-06-18" | "2025-11-25")
        ) {
            return Err("This channel requires session-based MCP elicitation; stateless Tasks use inputRequests".into());
        }
        let capability = self
            .capabilities
            .as_ref()
            .and_then(|caps| caps.elicitation.as_ref())
            .ok_or("Client does not support elicitation")?;
        let supported = match &request {
            ElicitationRequest::Form { .. } => {
                capability.form.is_some() || capability.url.is_none()
            }
            ElicitationRequest::Url { .. } => {
                self.version == Some(ProtocolVersion::V_2025_11_25) && capability.url.is_some()
            }
        };
        if !supported {
            return Err("Client does not support this elicitation mode".into());
        }
        let value = serde_json::to_value(request).map_err(|error| error.to_string())?;
        let params: ElicitRequestParams = serde_json::from_value(value)
            .map_err(|error| format!("Invalid elicitation request: {error}"))?;
        let response = tokio::select! {
            biased;
            _ = self.cancellation.cancelled() => return Err("Elicitation cancelled".into()),
            response = self.peer.create_elicitation(params) => response.map_err(|error|error.to_string())?,
        };
        serde_json::from_value(serde_json::to_value(response).map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())
    }
}
