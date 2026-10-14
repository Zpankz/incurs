# incurs-mcp-events-cloudflare

Cloudflare host for `incurs-mcp-events`:

- `DurableEventsStore`: Durable Object SQLite tables (`incurs_ev_*`) for
  subscriptions, the delivery queue, and received message ids. It implements
  `SubscriptionStore`, `DeliveryQueue`, and `DedupeStore`.
- `WorkersWebhookClient`: `fetch` with manual redirects and an abort timeout
  (10 seconds by default).
- `WorkersClock` and `WorkersEntropy`: `Date.now()` and `crypto.randomUUID()`.
- `schedule_drain`: sets the Durable Object alarm to the next due delivery.

Pattern inside a Durable Object:

1. Publish: `fan_out(&store, &event, &attributes, now)`, then `schedule_drain(storage, Some(now), now)`
   so delivery runs from the alarm and the publisher never waits on receivers.
2. Alarm: `store.purge_expired(now)`, `drain(...)`, then `schedule_drain` with the returned due time.

`examples/events-worker` is a complete Worker; `examples/events-worker/scripts/smoke.py`
runs it under `wrangler dev` against a receiver that checks signatures independently.
