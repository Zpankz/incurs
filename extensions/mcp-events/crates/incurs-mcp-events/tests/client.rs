//! Acceptance tests A18 and A19 of `docs/mcp-events.md`: the events client.

use std::cell::RefCell;

use incurs_mcp_events::{
    ClientError, EventsClient, SubscribeResult, TransportFailure, WebhookClient, WebhookRequest,
    WebhookResponse,
};
use serde_json::{Value, json};

struct Server {
    status: u16,
    body: &'static str,
    seen: RefCell<Vec<WebhookRequest>>,
}

impl Server {
    fn new(status: u16, body: &'static str) -> Self {
        Self {
            status,
            body,
            seen: RefCell::new(Vec::new()),
        }
    }
}

#[async_trait::async_trait(?Send)]
impl WebhookClient for Server {
    async fn post(&self, request: WebhookRequest) -> Result<WebhookResponse, TransportFailure> {
        self.seen.borrow_mut().push(request);
        Ok(WebhookResponse {
            status: self.status,
            body: self.body.as_bytes().to_vec(),
        })
    }
}

const NAME: &str = "cloudflare.alert.workers_observability_real_time_issue";
const CALLBACK: &str = "https://gitfoundry.example/api/v1/mcp-events/cloudflare";
const SECRET: &str = "whsec_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
const RESULT: &str = r#"{"jsonrpc":"2.0","id":1,"result":{"id":"sub_1","refreshBefore":"2026-10-15T00:00:00Z","cursor":null,"truncated":false}}"#;
const STREAM: &str = "event: message\ndata: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\",\"params\":{}}\n\nevent: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"id\":\"sub_1\",\"refreshBefore\":\"2026-10-15T00:00:00Z\",\"cursor\":null,\"truncated\":false}}\n\n";

fn header<'a>(request: &'a WebhookRequest, name: &str) -> Option<&'a str> {
    request
        .headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

async fn subscribe(server: &Server) -> Result<SubscribeResult, ClientError> {
    let client = EventsClient {
        http: server,
        endpoint: "https://mcp.cloudflare.com/mcp".into(),
        authorization: Some("Bearer t1".into()),
    };
    client
        .subscribe(NAME, &json!({"account_id": "a1"}), CALLBACK, SECRET, None)
        .await
}

#[tokio::test]
async fn a18_client_subscribe() {
    for body in [RESULT, STREAM] {
        let server = Server::new(200, body);
        let result = subscribe(&server).await;
        assert_eq!(
            result,
            Ok(SubscribeResult {
                id: "sub_1".into(),
                refresh_before: Some("2026-10-15T00:00:00Z".into()),
                cursor: None,
                truncated: false
            })
        );
        let seen = server.seen.borrow();
        assert_eq!(seen.len(), 1);
        let request = &seen[0];
        assert_eq!(request.url, "https://mcp.cloudflare.com/mcp");
        assert_eq!(header(request, "MCP-Protocol-Version"), Some("2026-07-28"));
        let accept = header(request, "Accept").unwrap();
        assert!(
            accept.contains("application/json") && accept.contains("text/event-stream"),
            "{accept}"
        );
        assert_eq!(header(request, "Authorization"), Some("Bearer t1"));
        assert_eq!(header(request, "Content-Type"), Some("application/json"));
        let rpc: Value = serde_json::from_slice(&request.body).unwrap();
        assert_eq!(rpc["jsonrpc"], json!("2.0"));
        assert_eq!(rpc["method"], json!("events/subscribe"));
        assert_eq!(rpc["params"]["name"], json!(NAME));
        assert_eq!(rpc["params"]["arguments"], json!({"account_id": "a1"}));
        assert_eq!(
            rpc["params"]["delivery"],
            json!({"mode": "webhook", "url": CALLBACK, "secret": SECRET})
        );
    }
}

#[tokio::test]
async fn a19_client_errors() {
    let rpc = Server::new(
        200,
        r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32015,"message":"Callback endpoint verification failed","data":{"reason":"challenge_failed"}}}"#,
    );
    match subscribe(&rpc).await {
        Err(ClientError::Rpc(error)) => {
            assert_eq!(error.code, -32015);
            assert_eq!(error.data, Some(json!({"reason": "challenge_failed"})));
        }
        other => panic!("expected RPC error, got {other:?}"),
    }
    let denied = Server::new(401, r#"{"error":"unauthorized"}"#);
    assert_eq!(subscribe(&denied).await, Err(ClientError::Http(401)));
}
