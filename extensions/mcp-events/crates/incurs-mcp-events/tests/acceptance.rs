//! Acceptance tests for `docs/mcp-events.md`, one per acceptance criterion.
//! Expected values are literals from the specification or independent sources,
//! never recomputed the way the crate computes them.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;

use incurs_mcp_events::{
    Clock, DedupeStore, DeliveryOutcome, DeliveryPolicy, Entropy, Event, EventDefinition,
    EventProvider, EventsError, EventsRouter, Lifetimes, ReceiverOutcome, RouterConfig, Secret,
    Subscription, SubscriptionStore, TransportFailure, WebhookClient, WebhookRequest,
    WebhookResponse, augment_discovery, deliver, is_events_request, receive, sign, subscription_id,
    verify,
};
use serde_json::{Value, json};

// ---- boundary doubles: clock, randomness, network, storage --------------------------------

struct FixedClock(Cell<i64>);
impl Clock for FixedClock {
    fn now_ms(&self) -> i64 {
        self.0.get()
    }
}

struct CountingEntropy(Cell<u64>);
impl Entropy for CountingEntropy {
    fn token(&self) -> String {
        let next = self.0.get() + 1;
        self.0.set(next);
        format!("tok{next}")
    }
}

type Script = Box<dyn Fn(&WebhookRequest) -> Result<WebhookResponse, TransportFailure>>;

struct ScriptedClient {
    script: Script,
    sent: RefCell<Vec<WebhookRequest>>,
}

impl ScriptedClient {
    fn new(
        script: impl Fn(&WebhookRequest) -> Result<WebhookResponse, TransportFailure> + 'static,
    ) -> Self {
        Self {
            script: Box::new(script),
            sent: RefCell::new(Vec::new()),
        }
    }
    fn status(status: u16) -> Self {
        Self::new(move |_| {
            Ok(WebhookResponse {
                status,
                body: Vec::new(),
            })
        })
    }
    fn echo() -> Self {
        Self::new(|request| {
            let body: Value = serde_json::from_slice(&request.body).unwrap();
            Ok(WebhookResponse {
                status: 200,
                body: serde_json::to_vec(&json!({ "challenge": body["challenge"] })).unwrap(),
            })
        })
    }
    fn count(&self) -> usize {
        self.sent.borrow().len()
    }
}

#[async_trait::async_trait(?Send)]
impl WebhookClient for ScriptedClient {
    async fn post(&self, request: WebhookRequest) -> Result<WebhookResponse, TransportFailure> {
        let response = (self.script)(&request);
        self.sent.borrow_mut().push(request);
        response
    }
}

#[derive(Default)]
struct MemoryStore(RefCell<BTreeMap<String, Subscription>>);

#[async_trait::async_trait(?Send)]
impl SubscriptionStore for MemoryStore {
    async fn put(&self, subscription: Subscription) -> Result<(), String> {
        self.0
            .borrow_mut()
            .insert(subscription.id.clone(), subscription);
        Ok(())
    }
    async fn remove(&self, id: &str) -> Result<(), String> {
        self.0.borrow_mut().remove(id);
        Ok(())
    }
}

#[derive(Default)]
struct MemoryDedupe(RefCell<Vec<String>>);

#[async_trait::async_trait(?Send)]
impl DedupeStore for MemoryDedupe {
    async fn first_seen(&self, id: &str, _now_ms: i64) -> Result<bool, String> {
        let mut seen = self.0.borrow_mut();
        if seen.iter().any(|known| known == id) {
            return Ok(false);
        }
        seen.push(id.to_string());
        Ok(true)
    }
}

struct CommentProvider;

#[async_trait::async_trait(?Send)]
impl EventProvider for CommentProvider {
    fn definitions(&self) -> Vec<EventDefinition> {
        vec![EventDefinition {
            name: "comment.created".into(),
            description: "A new review comment was added.".into(),
            input_schema: json!({"type": "object", "properties": {"document_id": {"type": "string"}}}),
            payload_schema: json!({"type": "object"}),
        }]
    }
    async fn authorize(
        &self,
        _principal: &str,
        _name: &str,
        _arguments: &Value,
    ) -> Result<(), EventsError> {
        Ok(())
    }
}

// 2026-10-14T00:00:00Z
const T0: i64 = 1_791_936_000_000;
// 24 bytes and 64 bytes of zeros, base64 encoded.
const SECRET_24: &str = "whsec_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
const SECRET_64: &str = "whsec_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA==";
// 23 and 65 bytes.
const SECRET_23: &str = "whsec_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
const SECRET_65: &str = "whsec_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
const RECEIVER: &str = "https://receiver.example.com/cb";

struct Harness {
    provider: CommentProvider,
    store: MemoryStore,
    client: ScriptedClient,
    clock: FixedClock,
    entropy: CountingEntropy,
    config: RouterConfig,
}

impl Harness {
    fn new(client: ScriptedClient) -> Self {
        Self {
            provider: CommentProvider,
            store: MemoryStore::default(),
            client,
            clock: FixedClock(Cell::new(T0)),
            entropy: CountingEntropy(Cell::new(0)),
            config: RouterConfig {
                lifetimes: Lifetimes {
                    min_ms: 60_000,
                    max_ms: 86_400_000,
                },
                allow_local_testing: false,
            },
        }
    }
    fn router(&self) -> EventsRouter<'_> {
        EventsRouter {
            provider: &self.provider,
            store: &self.store,
            client: &self.client,
            clock: &self.clock,
            entropy: &self.entropy,
            config: self.config.clone(),
        }
    }
    async fn call(&self, principal: &str, method: &str, params: Value) -> Value {
        self.router()
            .handle(
                principal,
                &json!({"jsonrpc": "2.0", "id": 7, "method": method, "params": params}),
            )
            .await
    }
}

fn subscribe_params(url: &str, secret: &str) -> Value {
    json!({
        "name": "comment.created",
        "arguments": {"document_id": "doc_123"},
        "delivery": {"mode": "webhook", "url": url, "secret": secret},
        "cursor": null
    })
}

fn error_code(response: &Value) -> Option<i64> {
    response["error"]["code"].as_i64()
}

fn header<'a>(request: &'a WebhookRequest, name: &str) -> Option<&'a str> {
    request
        .headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

// ---- A1 ---------------------------------------------------------------------------------

#[tokio::test]
async fn a1_discovery_and_listing() {
    let discover = json!({"jsonrpc": "2.0", "id": 1, "method": "server/discover"});
    let mut response = json!({"jsonrpc": "2.0", "id": 1, "result": {"capabilities": {"tools": {"listChanged": false}}}});
    augment_discovery(&discover, &mut response);
    assert_eq!(response["result"]["capabilities"]["events"], json!({}));
    assert_eq!(
        response["result"]["capabilities"]["tools"],
        json!({"listChanged": false})
    );

    let initialize = json!({"jsonrpc": "2.0", "id": 2, "method": "initialize", "params": {}});
    let mut legacy = json!({"jsonrpc": "2.0", "id": 2, "result": {"capabilities": {}}});
    augment_discovery(&initialize, &mut legacy);
    assert_eq!(legacy["result"]["capabilities"]["events"], json!({}));

    let tools = json!({"jsonrpc": "2.0", "id": 3, "method": "tools/list"});
    let mut untouched = json!({"jsonrpc": "2.0", "id": 3, "result": {"tools": []}});
    augment_discovery(&tools, &mut untouched);
    assert_eq!(
        untouched,
        json!({"jsonrpc": "2.0", "id": 3, "result": {"tools": []}})
    );
    assert!(!is_events_request(&tools));
    assert!(is_events_request(
        &json!({"jsonrpc": "2.0", "id": 4, "method": "events/list"})
    ));

    let harness = Harness::new(ScriptedClient::status(200));
    let listed = harness.call("p1", "events/list", json!({})).await;
    assert_eq!(listed["id"], json!(7));
    assert_eq!(
        listed["result"]["events"][0]["name"],
        json!("comment.created")
    );
    assert_eq!(
        listed["result"]["events"][0]["delivery"],
        json!(["webhook"])
    );
    assert_eq!(
        listed["result"]["events"][0]["inputSchema"]["type"],
        json!("object")
    );
    assert_eq!(
        listed["result"]["events"][0]["payloadSchema"],
        json!({"type": "object"})
    );
}

// ---- A2 ---------------------------------------------------------------------------------

#[tokio::test]
async fn a2_secret_bounds() {
    for accepted in [SECRET_24, SECRET_64] {
        let harness = Harness::new(ScriptedClient::echo());
        let response = harness
            .call(
                "p1",
                "events/subscribe",
                subscribe_params(RECEIVER, accepted),
            )
            .await;
        assert!(
            response["result"]["id"].is_string(),
            "{accepted} rejected: {response}"
        );
    }
    for rejected in [SECRET_23, SECRET_65, "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"] {
        let harness = Harness::new(ScriptedClient::echo());
        let response = harness
            .call(
                "p1",
                "events/subscribe",
                subscribe_params(RECEIVER, rejected),
            )
            .await;
        assert_eq!(
            error_code(&response),
            Some(-32602),
            "{rejected}: {response}"
        );
        assert_eq!(harness.client.count(), 0);
    }
}

// ---- A3, A3a ----------------------------------------------------------------------------

#[tokio::test]
async fn a3_callback_address() {
    let rejected = [
        "http://example.com/cb",
        "https://user:pw@example.com/cb",
        "https://localhost/cb",
        "https://hooks.localhost/cb",
        "https://127.0.0.1/cb",
        "https://10.1.2.3/cb",
        "https://169.254.1.1/cb",
        "https://[::1]/cb",
        "https://[fd00::1]/cb",
    ];
    for url in rejected {
        let harness = Harness::new(ScriptedClient::echo());
        let response = harness
            .call("p1", "events/subscribe", subscribe_params(url, SECRET_24))
            .await;
        assert_eq!(error_code(&response), Some(-32602), "{url}: {response}");
        assert_eq!(harness.client.count(), 0, "{url} sent a request");
    }
    let harness = Harness::new(ScriptedClient::echo());
    let response = harness
        .call(
            "p1",
            "events/subscribe",
            subscribe_params(RECEIVER, SECRET_24),
        )
        .await;
    assert!(response["result"]["id"].is_string(), "{response}");
    assert_eq!(harness.client.count(), 1);
}

#[tokio::test]
async fn a3a_local_testing() {
    let mut harness = Harness::new(ScriptedClient::echo());
    harness.config.allow_local_testing = true;
    let loopback = harness
        .call(
            "p1",
            "events/subscribe",
            subscribe_params("http://127.0.0.1:8787/cb", SECRET_24),
        )
        .await;
    assert!(loopback["result"]["id"].is_string(), "{loopback}");
    let private = harness
        .call(
            "p1",
            "events/subscribe",
            subscribe_params("http://10.1.2.3/cb", SECRET_24),
        )
        .await;
    assert_eq!(error_code(&private), Some(-32602), "{private}");
    assert_eq!(harness.client.count(), 1);
}

// ---- A4 ---------------------------------------------------------------------------------

#[tokio::test]
async fn a4_unsupported_mode_and_unknown_event() {
    let harness = Harness::new(ScriptedClient::echo());
    let mut poll = subscribe_params(RECEIVER, SECRET_24);
    poll["delivery"]["mode"] = json!("poll");
    let response = harness.call("p1", "events/subscribe", poll).await;
    assert_eq!(error_code(&response), Some(-32026), "{response}");

    let mut missing = subscribe_params(RECEIVER, SECRET_24);
    missing["name"] = json!("missing.event");
    let response = harness.call("p1", "events/subscribe", missing).await;
    assert_eq!(error_code(&response), Some(-32023), "{response}");
    assert_eq!(harness.client.count(), 0);
}

// ---- A5 ---------------------------------------------------------------------------------

#[test]
fn a5_subscription_identifier() {
    // printf '%s' '["p1","https://receiver.example.com/cb","comment.created",{"a":2,"b":1}]' | shasum -a 256
    let expected = "9652fe9264b28a2d1cac62fc12bac216";
    let first = subscription_id("p1", RECEIVER, "comment.created", &json!({"b": 1, "a": 2}));
    let second = subscription_id("p1", RECEIVER, "comment.created", &json!({"a": 2, "b": 1}));
    assert_eq!(first, expected);
    assert_eq!(second, expected);
    assert_ne!(
        subscription_id("p2", RECEIVER, "comment.created", &json!({"a": 2, "b": 1})),
        expected
    );
}

// ---- A6 ---------------------------------------------------------------------------------

#[tokio::test]
async fn a6_verification_handshake() {
    let harness = Harness::new(ScriptedClient::echo());
    let response = harness
        .call(
            "p1",
            "events/subscribe",
            subscribe_params(RECEIVER, SECRET_24),
        )
        .await;
    assert!(response["result"]["id"].is_string(), "{response}");
    assert_eq!(harness.store.0.borrow().len(), 1);
    let request = &harness.client.sent.borrow()[0].clone();
    let body: Value = serde_json::from_slice(&request.body).unwrap();
    assert_eq!(body["type"], json!("verification"));
    assert!(
        body["challenge"]
            .as_str()
            .is_some_and(|value| !value.is_empty())
    );
    assert_eq!(
        header(request, "X-MCP-Subscription-Id"),
        response["result"]["id"].as_str()
    );
    let secret = Secret::parse(SECRET_24).unwrap();
    verify(
        &secret,
        header(request, "webhook-id").unwrap(),
        header(request, "webhook-timestamp").unwrap(),
        header(request, "webhook-signature").unwrap(),
        &request.body,
        T0 / 1000,
    )
    .expect("verification request is signed");

    let cases: [(&str, ScriptedClient); 4] = [
        (
            "challenge_failed",
            ScriptedClient::new(|_| {
                Ok(WebhookResponse {
                    status: 200,
                    body: br#"{"challenge":"other"}"#.to_vec(),
                })
            }),
        ),
        ("http_4xx", ScriptedClient::status(404)),
        ("http_5xx", ScriptedClient::status(503)),
        (
            "timeout",
            ScriptedClient::new(|_| Err(TransportFailure::Timeout)),
        ),
    ];
    for (reason, client) in cases {
        let harness = Harness::new(client);
        let response = harness
            .call(
                "p1",
                "events/subscribe",
                subscribe_params(RECEIVER, SECRET_24),
            )
            .await;
        assert_eq!(error_code(&response), Some(-32015), "{reason}: {response}");
        assert_eq!(response["error"]["data"]["reason"], json!(reason));
        assert!(
            harness.store.0.borrow().is_empty(),
            "{reason} stored a subscription"
        );
    }
}

// ---- A7 ---------------------------------------------------------------------------------

#[tokio::test]
async fn a7_lifetime_and_refresh() {
    let harness = Harness::new(ScriptedClient::echo());
    let mut expectations = Vec::new();
    for (ttl, expected) in [
        (json!(3_600_000), "2026-10-14T01:00:00Z"),
        (json!(1000), "2026-10-14T00:01:00Z"),
        (Value::Null, "2026-10-15T00:00:00Z"),
    ] {
        let mut params = subscribe_params(RECEIVER, SECRET_24);
        if !ttl.is_null() {
            params["ttlMs"] = ttl;
        }
        let response = harness.call("p1", "events/subscribe", params).await;
        assert_eq!(
            response["result"]["refreshBefore"],
            json!(expected),
            "{response}"
        );
        expectations.push(response["result"]["id"].clone());
    }
    let refreshed = harness
        .call(
            "p1",
            "events/subscribe",
            subscribe_params(RECEIVER, SECRET_64),
        )
        .await;
    assert_eq!(refreshed["result"]["id"], expectations[0]);
    assert!(expectations.iter().all(|id| *id == expectations[0]));
    let store = harness.store.0.borrow();
    assert_eq!(store.len(), 1);
    assert_eq!(store.values().next().unwrap().secret, SECRET_64);
}

// ---- A8 ---------------------------------------------------------------------------------

#[tokio::test]
async fn a8_unsubscribe() {
    let harness = Harness::new(ScriptedClient::echo());
    harness
        .call(
            "p1",
            "events/subscribe",
            subscribe_params(RECEIVER, SECRET_24),
        )
        .await;
    assert_eq!(harness.store.0.borrow().len(), 1);
    let params = json!({"name": "comment.created", "arguments": {"document_id": "doc_123"}, "delivery": {"mode": "webhook", "url": RECEIVER}});
    for _ in 0..2 {
        let response = harness
            .call("p1", "events/unsubscribe", params.clone())
            .await;
        assert_eq!(response["result"], json!({}), "{response}");
    }
    assert!(harness.store.0.borrow().is_empty());
}

// ---- A9 ---------------------------------------------------------------------------------

const VECTOR_SECRET: &str = "whsec_C2FVsBQIhrscChlQIMV+b5sSYspob7oD";
const VECTOR_ID: &str = "msg_27UH4WbU6Z5A5EzD8u03UvzRbpk";
const VECTOR_TS: i64 = 1_649_367_553;
const VECTOR_BODY: &[u8] = br#"{"email":"test@example.com","username":"test_user"}"#;
const VECTOR_SIGNATURE: &str = "v1,tZ1I4/hDygAJgO5TYxiSd6Sd0kDW6hPenDe+bTa3Kkw=";

#[test]
fn a9_standard_webhooks_signature() {
    let secret = Secret::parse(VECTOR_SECRET).unwrap();
    assert_eq!(
        sign(&secret, VECTOR_ID, VECTOR_TS, VECTOR_BODY),
        VECTOR_SIGNATURE
    );
}

// ---- A10 --------------------------------------------------------------------------------

fn vector_headers(signature: &str) -> Vec<(String, String)> {
    vec![
        ("webhook-id".into(), VECTOR_ID.into()),
        ("webhook-timestamp".into(), VECTOR_TS.to_string()),
        ("webhook-signature".into(), signature.into()),
    ]
}

#[tokio::test]
async fn a10_received_request_verification() {
    let secret = Secret::parse(VECTOR_SECRET).unwrap();
    assert!(
        verify(
            &secret,
            VECTOR_ID,
            &VECTOR_TS.to_string(),
            VECTOR_SIGNATURE,
            VECTOR_BODY,
            VECTOR_TS + 300
        )
        .is_ok()
    );
    assert!(
        verify(
            &secret,
            VECTOR_ID,
            &VECTOR_TS.to_string(),
            VECTOR_SIGNATURE,
            VECTOR_BODY,
            VECTOR_TS + 301
        )
        .is_err()
    );
    let rotated = format!("v1,AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA= {VECTOR_SIGNATURE}");
    assert!(
        verify(
            &secret,
            VECTOR_ID,
            &VECTOR_TS.to_string(),
            &rotated,
            VECTOR_BODY,
            VECTOR_TS
        )
        .is_ok()
    );
    let mut changed = VECTOR_BODY.to_vec();
    changed[3] ^= 1;
    assert!(
        verify(
            &secret,
            VECTOR_ID,
            &VECTOR_TS.to_string(),
            VECTOR_SIGNATURE,
            &changed,
            VECTOR_TS
        )
        .is_err()
    );

    let dedupe = MemoryDedupe::default();
    let stale = receive(
        &secret,
        &dedupe,
        &vector_headers(VECTOR_SIGNATURE),
        VECTOR_BODY,
        (VECTOR_TS + 301) * 1000,
    )
    .await;
    assert_eq!(stale.status(), 401);
    let tampered = receive(
        &secret,
        &dedupe,
        &vector_headers(VECTOR_SIGNATURE),
        &changed,
        VECTOR_TS * 1000,
    )
    .await;
    assert_eq!(tampered.status(), 401);
}

// ---- A11 --------------------------------------------------------------------------------

fn subscription(secret: &str) -> Subscription {
    Subscription {
        id: "sub_123".into(),
        principal: "p1".into(),
        name: "comment.created".into(),
        arguments: json!({"document_id": "doc_123"}),
        url: RECEIVER.into(),
        secret: secret.into(),
        refresh_before_ms: T0 + 3_600_000,
    }
}

fn event_with_text(text: String) -> Event {
    Event {
        event_id: "evt_456".into(),
        name: "comment.created".into(),
        timestamp: "2026-10-14T00:05:00Z".into(),
        data: json!({"text": text}),
        cursor: None,
    }
}

fn body_overhead() -> usize {
    // {"eventId":"evt_456","name":"comment.created","timestamp":"2026-10-14T00:05:00Z","data":{"text":""},"cursor":null}
    br#"{"eventId":"evt_456","name":"comment.created","timestamp":"2026-10-14T00:05:00Z","data":{"text":""},"cursor":null}"#.len()
}

#[tokio::test]
async fn a11_delivery_request() {
    let policy = DeliveryPolicy {
        max_attempts: 3,
        base_delay_ms: 1000,
    };
    let client = ScriptedClient::status(200);
    let store = MemoryStore::default();
    let clock = FixedClock(Cell::new(T0));
    let small = event_with_text("x".repeat(1000 - body_overhead()));
    let outcome = deliver(
        &client,
        &store,
        &clock,
        &subscription(SECRET_24),
        &small,
        1,
        &policy,
    )
    .await
    .unwrap();
    assert_eq!(outcome, DeliveryOutcome::Delivered);
    {
        let sent = client.sent.borrow();
        assert_eq!(sent.len(), 1);
        let request = &sent[0];
        assert_eq!(request.body.len(), 1000);
        assert_eq!(request.url, RECEIVER);
        assert_eq!(header(request, "webhook-id"), Some("evt_456"));
        assert_eq!(header(request, "X-MCP-Subscription-Id"), Some("sub_123"));
        assert_eq!(header(request, "content-type"), Some("application/json"));
        let secret = Secret::parse(SECRET_24).unwrap();
        verify(
            &secret,
            "evt_456",
            header(request, "webhook-timestamp").unwrap(),
            header(request, "webhook-signature").unwrap(),
            &request.body,
            T0 / 1000,
        )
        .expect("delivery is signed");
        let body: Value = serde_json::from_slice(&request.body).unwrap();
        assert_eq!(body["eventId"], json!("evt_456"));
        assert_eq!(body["name"], json!("comment.created"));
        assert_eq!(body["timestamp"], json!("2026-10-14T00:05:00Z"));
        assert_eq!(body["cursor"], Value::Null);
    }

    let large = event_with_text("x".repeat(262_145 - body_overhead()));
    let outcome = deliver(
        &client,
        &store,
        &clock,
        &subscription(SECRET_24),
        &large,
        1,
        &policy,
    )
    .await
    .unwrap();
    assert_eq!(outcome, DeliveryOutcome::TooLarge);
    assert_eq!(client.count(), 1);

    let limit = event_with_text("x".repeat(262_144 - body_overhead()));
    let outcome = deliver(
        &client,
        &store,
        &clock,
        &subscription(SECRET_24),
        &limit,
        1,
        &policy,
    )
    .await
    .unwrap();
    assert_eq!(outcome, DeliveryOutcome::Delivered);
}

// ---- A12 --------------------------------------------------------------------------------

#[tokio::test]
async fn a12_delivery_outcomes() {
    let policy = DeliveryPolicy {
        max_attempts: 3,
        base_delay_ms: 1000,
    };
    let event = event_with_text("hello".into());
    for status in [503, 429] {
        let client = ScriptedClient::status(status);
        let store = MemoryStore::default();
        let clock = FixedClock(Cell::new(T0));
        let mut outcomes = Vec::new();
        for attempt in 1..=3 {
            outcomes.push(
                deliver(
                    &client,
                    &store,
                    &clock,
                    &subscription(SECRET_24),
                    &event,
                    attempt,
                    &policy,
                )
                .await
                .unwrap(),
            );
            clock.0.set(clock.0.get() + 5_000);
        }
        assert_eq!(
            outcomes,
            vec![
                DeliveryOutcome::Retry { after_ms: 1000 },
                DeliveryOutcome::Retry { after_ms: 2000 },
                DeliveryOutcome::GaveUp,
            ],
            "status {status}"
        );
        let sent = client.sent.borrow();
        assert!(
            sent.iter()
                .all(|request| header(request, "webhook-id") == Some("evt_456"))
        );
        assert_ne!(
            header(&sent[0], "webhook-signature"),
            header(&sent[1], "webhook-signature")
        );
    }

    let client = ScriptedClient::status(410);
    let store = MemoryStore::default();
    store.put(subscription(SECRET_24)).await.unwrap();
    let clock = FixedClock(Cell::new(T0));
    let outcome = deliver(
        &client,
        &store,
        &clock,
        &subscription(SECRET_24),
        &event,
        1,
        &policy,
    )
    .await
    .unwrap();
    assert_eq!(outcome, DeliveryOutcome::Ended);
    assert!(store.0.borrow().is_empty());

    let client = ScriptedClient::status(413);
    let outcome = deliver(
        &client,
        &store,
        &clock,
        &subscription(SECRET_24),
        &event,
        1,
        &policy,
    )
    .await
    .unwrap();
    assert_eq!(outcome, DeliveryOutcome::Rejected);

    let client = ScriptedClient::status(200);
    let expired = FixedClock(Cell::new(T0 + 3_600_000));
    let outcome = deliver(
        &client,
        &store,
        &expired,
        &subscription(SECRET_24),
        &event,
        1,
        &policy,
    )
    .await
    .unwrap();
    assert_eq!(outcome, DeliveryOutcome::Expired);
    assert_eq!(client.count(), 0);
}

// ---- A13 --------------------------------------------------------------------------------

#[tokio::test]
async fn a13_receiver() {
    let secret = Secret::parse(SECRET_24).unwrap();
    let dedupe = MemoryDedupe::default();
    let now_s = T0 / 1000;
    let signed = |id: &str, body: &[u8]| {
        vec![
            ("webhook-id".to_string(), id.to_string()),
            ("webhook-timestamp".to_string(), now_s.to_string()),
            (
                "webhook-signature".to_string(),
                sign(&secret, id, now_s, body),
            ),
            ("X-MCP-Subscription-Id".to_string(), "sub_123".to_string()),
        ]
    };
    let verification = br#"{"type":"verification","challenge":"c-1"}"#;
    let outcome = receive(
        &secret,
        &dedupe,
        &signed("msg_verification_1", verification),
        verification,
        T0,
    )
    .await;
    assert_eq!(outcome.status(), 200);
    assert_eq!(outcome.response_body(), Some(json!({"challenge": "c-1"})));

    let event = br#"{"eventId":"evt_9","name":"comment.created","timestamp":"2026-10-14T00:05:00Z","data":{"text":"hi"},"cursor":null}"#;
    let first = receive(&secret, &dedupe, &signed("evt_9", event), event, T0).await;
    match &first {
        ReceiverOutcome::Accepted {
            event,
            subscription_id,
        } => {
            assert_eq!(event.event_id, "evt_9");
            assert_eq!(event.data, json!({"text": "hi"}));
            assert_eq!(subscription_id.as_deref(), Some("sub_123"));
        }
        other => panic!("expected accepted, got {other:?}"),
    }
    assert_eq!(first.status(), 200);
    let second = receive(&secret, &dedupe, &signed("evt_9", event), event, T0).await;
    assert!(matches!(second, ReceiverOutcome::Duplicate), "{second:?}");
    assert_eq!(second.status(), 200);
}
