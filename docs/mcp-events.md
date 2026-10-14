# MCP Events webhook core

## Stakeholder need

An incurs server author needs to let an MCP client subscribe to server events and receive them as signed webhooks, so that an agent such as ChatGPT reacts to upstream changes without polling.

## Problem statement

MCP Events is a draft extension (design sketch dated 2026-02-19 in `modelcontextprotocol/experimental-ext-triggers-events`). ChatGPT shipped its webhook mode on 2026-09-29, documented at `https://developers.openai.com/plugins/build/mcp-events`. On incurs main `c7e7d23`, `incurs-mcp-cloudflare` and `incurs-codemode-cloudflare` advertise only `capabilities.tools` in `server/discover`, and an `events/list` request returns JSON-RPC error `-32601`. No incurs crate signs, verifies, or delivers Standard Webhooks.

This specification covers the platform-neutral crate `incurs-mcp-events`. Storage, HTTP, and the clock are host traits. A Cloudflare store and an MCP client follow as separate increments.

## Terms

- Principal: the authenticated subject the host passes with each call.
- Subscription key: principal, callback URL, event name, and the canonical JSON of the arguments.
- Callback error: JSON-RPC error `-32015` with `data.reason`, as the OpenAI guide defines it.

## Requirements

- R1: The router shall answer `events/list` with every event definition the provider declares, each with `delivery` equal to `["webhook"]`.
- R2: The router shall add `capabilities.events` as an empty object to a `server/discover` result and to an `initialize` result.
- R3: The router shall leave a request whose method does not start with `events/` for the inner handler.
- R4: The router shall reject an `events/subscribe` secret with error `-32602` unless it starts with `whsec_` and its remainder decodes from base64 to 24 through 64 bytes.
- R5: The router shall reject an `events/subscribe` callback URL with error `-32602` unless its scheme is `https`, it has no user information, and its host is neither `localhost`, a name ending in `.localhost`, nor an IP literal in a loopback, private, link-local, unique-local, or unspecified range.
- R5a: When the host enables local testing, the router shall accept an `http` or `https` callback URL whose host is `localhost` or a loopback IP literal.
- R6: The router shall reject an `events/subscribe` delivery mode other than `webhook` with error `-32026`.
- R7: The router shall reject an `events/subscribe` for an event name the provider does not declare with error `-32023`.
- R8: The subscription identifier shall equal the first 32 hexadecimal digits of the SHA-256 of the canonical JSON array of the subscription key.
- R9: Before storing a subscription, the router shall send one signed verification request whose body contains `type` equal to `verification` and a single-use `challenge`.
- R10: The router shall store a subscription only when the verification response status is 2xx and its JSON `challenge` equals the sent challenge.
- R11: A failed verification shall return the callback error with `data.reason` equal to `challenge_failed`, `http_4xx`, `http_5xx`, `timeout`, `tls_error`, or `connection_refused`.
- R12: The router shall set `refreshBefore` to the subscribe time plus the requested `ttlMs` bounded to the host's minimum and maximum lifetime, and to the maximum lifetime when `ttlMs` is absent or null.
- R13: A repeated `events/subscribe` with the same subscription key shall return the same identifier and replace the stored secret and `refreshBefore`.
- R14: The router shall answer `events/unsubscribe` with an empty object and remove the subscription that matches its name, arguments, delivery URL, and principal.
- R15: The signer shall produce `v1,` followed by the base64 HMAC-SHA256, keyed by the decoded secret, of the message identifier, the Unix-second timestamp, and the body bytes joined by periods.
- R16: The verifier shall accept a request when any space-separated `webhook-signature` entry matches and the timestamp is within 300 seconds of the receiver clock.
- R17: The delivery engine shall refuse, without a request, an event whose serialized body exceeds 262144 bytes.
- R18: The delivery engine shall send each event as one POST with headers `webhook-id` equal to `eventId`, `webhook-timestamp`, `webhook-signature`, and `X-MCP-Subscription-Id`.
- R19: The delivery engine shall schedule a retry with the same `eventId` and a doubled delay after a 408, 429, 5xx, or connection failure, up to the host's attempt limit.
- R20: The delivery engine shall end a subscription after a 410 response and shall not retry a 413 response.
- R21: The delivery engine shall skip a subscription whose `refreshBefore` has passed.
- R22: The receiver shall answer a valid verification request with status 200 and a JSON body whose `challenge` echoes the request's challenge.
- R23: The receiver shall reject an event request whose signature or timestamp fails R16 with status 401.
- R24: The receiver shall report an event whose `webhook-id` it has already accepted as a duplicate.
- R25: Fan-out shall enqueue an event for each unexpired subscription with the same event name whose every argument equals the event attribute of the same name.
- R26: Enqueuing an `eventId` that is already queued for a subscription shall leave one queue entry for that pair.
- R27: Draining shall attempt, in due-time order and up to the host's batch limit, every queue entry whose due time has passed.
- R28: Draining shall remove a queue entry after any delivery outcome other than a retry.
- R29: After a retry outcome, draining shall increment the entry's attempt and set its due time to the time that attempt finished plus the retry delay.
- R30: Draining shall return the earliest due time that remains in the queue, and none when the queue is empty.
- R31: The Durable Object store shall return the subscriptions and queue entries it stored before the Worker restarted.
- R32: The Workers webhook client shall return a redirect response's status without requesting its `Location`.
- R33: The Workers webhook client shall report a timeout when no response arrives within its configured timeout.
- R34: The Durable Object host shall set its alarm to the due time a drain returns and shall drain when the alarm fires.
- R35: The events client shall send each `events/list`, `events/subscribe`, and `events/unsubscribe` call as one JSON-RPC POST with header `MCP-Protocol-Version` equal to `2026-07-28`, an `Accept` header naming `application/json` and `text/event-stream`, and the caller's `Authorization` header.
- R36: The events client shall read the JSON-RPC response from a JSON body, and from the `data:` line of an event-stream body whose JSON-RPC `id` equals the request's.
- R37: The events client shall return the code, message, and data of a JSON-RPC error response.
- R38: The events client shall report a response status outside 2xx as an HTTP error carrying that status.

## Acceptance criteria

### A1 (R1, R2, R3): Discovery and listing

Given a provider declaring `comment.created`
When the router receives `server/discover`, then `events/list`, then `tools/list`
Then the discover result has `capabilities.events` equal to `{}`, the list result names `comment.created` with delivery `["webhook"]`, and `tools/list` reaches the inner handler.

### A2 (R4): Secret bounds

Given secrets whose decoded lengths are 23, 24, 64, and 65 bytes, and a secret without the `whsec_` prefix
When each is used in `events/subscribe`
Then 24 and 64 are accepted, and 23, 65, and the unprefixed secret fail with `-32602`.

### A3 (R5): Callback address

Given callback URLs `http://example.com/cb`, `https://user:pw@example.com/cb`, `https://localhost/cb`, `https://hooks.localhost/cb`, `https://127.0.0.1/cb`, `https://10.1.2.3/cb`, `https://169.254.1.1/cb`, `https://[::1]/cb`, `https://[fd00::1]/cb`, and `https://receiver.example.com/cb`
When each is used in `events/subscribe`
Then only `https://receiver.example.com/cb` proceeds to verification, and every other URL fails with `-32602` without an outbound request.

### A3a (R5a): Local testing

Given a host with local testing enabled
When `events/subscribe` uses `http://127.0.0.1:8787/cb`, then `http://10.1.2.3/cb`
Then the loopback URL proceeds to verification and the private URL fails with `-32602`.

### A4 (R6, R7): Unsupported mode and unknown event

Given delivery mode `poll`, and event name `missing.event`
When each is used in `events/subscribe`
Then the mode fails with `-32026`, the name fails with `-32023`, and no outbound request is sent.

### A5 (R8): Subscription identifier

Given principal `p1`, URL `https://receiver.example.com/cb`, event `comment.created`, and arguments `{"b":1,"a":2}`
When the identifier is derived, and derived again with arguments `{"a":2,"b":1}`, and with principal `p2`
Then both orderings give the 32-digit prefix of SHA-256 of `["p1","https://receiver.example.com/cb","comment.created",{"a":2,"b":1}]`, and `p2` gives a different identifier.

### A6 (R9, R10, R11): Verification handshake

Given a receiver that echoes the challenge, one that returns a different challenge, one that returns 404, one that returns 503, and one that times out
When each is subscribed
Then the echoing receiver gets a stored subscription, the verification request carried a valid signature over its body, and the others fail with reasons `challenge_failed`, `http_4xx`, `http_5xx`, and `timeout` and store nothing.

### A7 (R12, R13): Lifetime and refresh

Given minimum lifetime 60 seconds, maximum lifetime 86400 seconds, and subscribe time `2026-10-14T00:00:00Z`
When `ttlMs` is 3600000, then 1000, then absent, and the same key is subscribed again with a new secret
Then `refreshBefore` is `2026-10-14T01:00:00Z`, then `2026-10-14T00:01:00Z`, then `2026-10-15T00:00:00Z`, the identifier is unchanged, and later deliveries sign with the new secret.

### A8 (R14): Unsubscribe

Given a stored subscription
When `events/unsubscribe` names its event, arguments, and URL, and is then repeated
Then both calls return `{}` and the store holds no subscription for that key.

### A9 (R15): Standard Webhooks signature

Given secret `whsec_C2FVsBQIhrscChlQIMV+b5sSYspob7oD`, message `msg_27UH4WbU6Z5A5EzD8u03UvzRbpk`, timestamp 1649367553, and body `{"email":"test@example.com","username":"test_user"}`
When the signer signs it
Then the signature is `v1,tZ1I4/hDygAJgO5TYxiSd6Sd0kDW6hPenDe+bTa3Kkw=`, the vector from the Standard Webhooks Rust library.

### A10 (R16, R23): Verification of a received request

Given the A9 request
When the receiver clock is 1649367553 + 300, then + 301, the signature header holds a wrong entry followed by the right one, and then one body byte changes
Then the request is accepted at + 300 and with the two entries, and is rejected with status 401 at + 301 and with the changed byte.

### A11 (R17, R18): Delivery request

Given an event of 1000 bytes and an event whose body is 262145 bytes
When each is delivered
Then the first sends one POST whose headers carry `webhook-id` equal to its `eventId`, a timestamp, a signature that verifies, and the subscription identifier, and the second is refused without a request.

### A12 (R19, R20, R21): Delivery outcomes

Given receivers answering 503, 429, 410, and 413, and a subscription whose `refreshBefore` has passed
When an event is delivered with an attempt limit of 3
Then 503 and 429 schedule retries with delays 1 s then 2 s that carry the same `eventId` and fresh signatures before giving up, 410 ends the subscription, 413 is not retried, and the expired subscription sends nothing.

### A13 (R22, R24): Receiver

Given a signed verification request with challenge `c-1`, and a signed event delivered twice
When the receiver handles them
Then the verification answer is status 200 with `{"challenge":"c-1"}`, the first event is accepted, and the second is reported as a duplicate.

### A14 (R25, R26): Fan-out

Given subscriptions to `comment.created` with arguments `{"document_id":"doc_1"}`, `{"document_id":"doc_2"}`, and `{}`, a subscription to `comment.deleted`, and an expired subscription to `comment.created` with arguments `{}`
When an event `comment.created` with attributes `{"document_id":"doc_1"}` fans out twice with the same `eventId`
Then the queue holds exactly two entries, one for the `doc_1` subscription and one for the `{}` subscription.

### A15 (R27, R28, R29, R30): Drain

Given a queue with entries due at 10 s, 20 s, and 90 s for receivers answering 200, 503, and 200, with batch limit 10, an attempt limit of 3, and a base delay of 1 s
When the queue drains at 30 s
Then the 200 entry due at 10 s is removed, the 503 entry has attempt 2 and due time 31 s, the 90 s entry was not attempted, and the drain returns 31 s; a drain at 31 s attempts only the 503 entry, leaves it with attempt 3 and due time 33 s, and returns 33 s; and draining an empty queue returns none.

### A16 (R31, R34): Cloudflare delivery

Given the `events-worker` example under `wrangler dev` with local testing enabled, and a local receiver that checks every signature with its own HMAC-SHA256
When a client subscribes receiver paths `/ok`, `/flaky`, and `/gone`, publishes event `evt_1`, waits 5 seconds, restarts the Worker with its state kept, and publishes `evt_2`
Then each path received one valid verification request, `/ok` received `evt_1` once, `/flaky` received `evt_1` twice with the same `webhook-id` and different signatures at least 1 second apart, `/gone` received `evt_1` and not `evt_2`, and `/ok` received `evt_2` after the restart.

### A17 (R32, R33): Redirect and timeout

Given the A16 setup with a webhook timeout of 2 seconds, a receiver path `/redirect` that answers 302 with `Location: /ok`, and a path `/slow` that answers after 4 seconds
When the client subscribes `/redirect`, then `/slow`
Then `/redirect` fails with callback reason `challenge_failed` and `/ok` receives no request from it, and `/slow` fails with callback reason `timeout`.

### A18 (R35, R36): Client subscribe

Given a server that answers `events/subscribe` with the JSON body `{"jsonrpc":"2.0","id":1,"result":{"id":"sub_1","refreshBefore":"2026-10-15T00:00:00Z","cursor":null,"truncated":false}}`, and another that answers with an event stream holding a notification and then that response
When the client subscribes `cloudflare.alert.workers_observability_real_time_issue` with arguments `{"account_id":"a1"}` and authorization `Bearer t1` to each
Then each request carried `MCP-Protocol-Version: 2026-07-28`, an `Accept` header with both media types, `Authorization: Bearer t1`, and a body whose method is `events/subscribe` with the given name, arguments, URL, and secret, and both calls return subscription `sub_1` with `refreshBefore` `2026-10-15T00:00:00Z`.

### A19 (R37, R38): Client errors

Given a server that answers with error `-32015` and data `{"reason":"challenge_failed"}`, and a server that answers status 401
When the client subscribes to each
Then the first call returns an RPC error with code `-32015` and reason `challenge_failed`, and the second returns an HTTP error with status 401.

