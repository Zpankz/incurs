# incurs-mcp-events

Platform-neutral webhook delivery for the MCP Events draft extension, matching
the subset ChatGPT supports: `events/list`, `events/subscribe`, and
`events/unsubscribe`, a signed verification handshake, Standard Webhooks
signatures, delivery decisions with retry and backoff, and a receiver.

The crate does no I/O. A host supplies:

- `EventProvider`: the event definitions and who may subscribe.
- `SubscriptionStore`: durable subscriptions keyed by subscription id.
- `WebhookClient`: outbound POSTs that never follow redirects.
- `Clock` and `Entropy`: time and cryptographic random tokens.
- `DedupeStore`: received message ids, for the receiving side.

Put the router beside an existing MCP handler:

1. If `is_events_request(&request)`, answer with `EventsRouter::handle(principal, &request)`.
2. Otherwise pass the request to the handler, then call `augment_discovery(&request, &mut response)`
   so `server/discover` and `initialize` advertise `capabilities.events`.

Delivery is one decision per attempt: `deliver` returns `Delivered`, `Retry { after_ms }`,
`GaveUp`, `Ended` (410, subscription removed), `Rejected`, `TooLarge`, or `Expired`. The host
stores the queue and schedules retries.

The draft is not part of the MCP specification and can change. Callback failures use
error `-32015`, as ChatGPT documents; the design sketch numbers it `-32027`.

Specification: `docs/mcp-events.md`.
