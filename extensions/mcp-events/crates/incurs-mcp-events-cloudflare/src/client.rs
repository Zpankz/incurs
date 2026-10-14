use std::time::Duration;

use futures::future::{Either, select};
use incurs_mcp_events::{TransportFailure, WebhookClient, WebhookRequest, WebhookResponse};
use worker::{
    AbortController, Delay, Fetch, Headers, Method, Request, RequestInit, RequestRedirect,
};

/// Webhook POSTs through Workers `fetch`. Redirects are returned, never
/// followed, and a request that outlives the timeout is aborted.
#[derive(Debug, Clone, Copy)]
pub struct WorkersWebhookClient {
    timeout: Duration,
}

impl Default for WorkersWebhookClient {
    /// A 10-second timeout, as ChatGPT's guide uses.
    fn default() -> Self {
        Self::new(Duration::from_secs(10))
    }
}

impl WorkersWebhookClient {
    pub fn new(timeout: Duration) -> Self {
        Self { timeout }
    }
}

fn classify(error: worker::Error) -> TransportFailure {
    let message = error.to_string();
    let lower = message.to_ascii_lowercase();
    if lower.contains("tls") || lower.contains("certificate") || lower.contains("ssl") {
        TransportFailure::Tls
    } else {
        TransportFailure::Other(message)
    }
}

#[async_trait::async_trait(?Send)]
impl WebhookClient for WorkersWebhookClient {
    async fn post(&self, request: WebhookRequest) -> Result<WebhookResponse, TransportFailure> {
        let headers = Headers::new();
        for (name, value) in &request.headers {
            headers
                .set(name, value)
                .map_err(|error| TransportFailure::Other(error.to_string()))?;
        }
        let mut init = RequestInit::new();
        init.with_method(Method::Post)
            .with_headers(headers)
            .with_redirect(RequestRedirect::Manual)
            .with_body(Some(
                js_sys::Uint8Array::from(request.body.as_slice()).into(),
            ));
        let outbound = Request::new_with_init(&request.url, &init)
            .map_err(|error| TransportFailure::Other(error.to_string()))?;
        let controller = AbortController::default();
        let signal = controller.signal();
        let exchange = async {
            let mut response = Fetch::Request(outbound).send_with_signal(&signal).await?;
            let status = response.status_code();
            let body = response.bytes().await.unwrap_or_default();
            Ok::<_, worker::Error>(WebhookResponse { status, body })
        };
        let deadline = Delay::from(self.timeout);
        futures::pin_mut!(exchange);
        match select(exchange, deadline).await {
            Either::Left((result, _)) => result.map_err(classify),
            Either::Right(_) => {
                controller.abort();
                Err(TransportFailure::Timeout)
            }
        }
    }
}
