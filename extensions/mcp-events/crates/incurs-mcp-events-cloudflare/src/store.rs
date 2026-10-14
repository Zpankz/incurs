use incurs_mcp_events::{
    DedupeStore, DeliveryQueue, Event, QueuedDelivery, Subscription, SubscriptionStore,
};
use serde::Deserialize;
use serde_json::Value;
use worker::{SqlStorage, SqlStorageValue};

/// How long received message ids are kept for duplicate detection.
const RECEIVED_RETENTION_MS: i64 = 7 * 24 * 60 * 60 * 1000;

/// Durable Object SQLite storage for subscriptions, queued deliveries, and
/// received message ids. Tables are prefixed `incurs_ev_`.
#[derive(Clone)]
pub struct DurableEventsStore {
    sql: SqlStorage,
}

// SQLite integers arrive as JavaScript numbers.
#[derive(Deserialize)]
struct SubscriptionRow {
    id: String,
    principal: String,
    name: String,
    arguments: String,
    url: String,
    secret: String,
    refresh_before_ms: f64,
}

#[derive(Deserialize)]
struct QueueRow {
    id: String,
    principal: String,
    name: String,
    arguments: String,
    url: String,
    secret: String,
    refresh_before_ms: f64,
    event: String,
    attempt: f64,
    due_ms: f64,
}

#[derive(Deserialize)]
struct DueRow {
    due_ms: Option<f64>,
}

#[derive(Deserialize)]
struct CountRow {
    count: f64,
}

fn subscription(
    id: String,
    principal: String,
    name: String,
    arguments: &str,
    url: String,
    secret: String,
    refresh_before_ms: f64,
) -> Result<Subscription, String> {
    Ok(Subscription {
        id,
        principal,
        name,
        arguments: serde_json::from_str(arguments).map_err(|error| error.to_string())?,
        url,
        secret,
        refresh_before_ms: refresh_before_ms as i64,
    })
}

fn text(error: worker::Error) -> String {
    error.to_string()
}

impl DurableEventsStore {
    /// Create the tables when absent.
    pub fn new(sql: SqlStorage) -> worker::Result<Self> {
        sql.exec(
            "CREATE TABLE IF NOT EXISTS incurs_ev_subscriptions (
                id TEXT PRIMARY KEY,
                principal TEXT NOT NULL,
                name TEXT NOT NULL,
                arguments TEXT NOT NULL,
                url TEXT NOT NULL,
                secret TEXT NOT NULL,
                refresh_before_ms INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS incurs_ev_subscriptions_name
               ON incurs_ev_subscriptions(name, refresh_before_ms);
             CREATE TABLE IF NOT EXISTS incurs_ev_queue (
                subscription_id TEXT NOT NULL,
                event_id TEXT NOT NULL,
                event TEXT NOT NULL,
                attempt INTEGER NOT NULL,
                due_ms INTEGER NOT NULL,
                PRIMARY KEY (subscription_id, event_id)
             );
             CREATE INDEX IF NOT EXISTS incurs_ev_queue_due ON incurs_ev_queue(due_ms);
             CREATE TABLE IF NOT EXISTS incurs_ev_received (
                id TEXT PRIMARY KEY,
                seen_ms INTEGER NOT NULL
             );",
            None,
        )?;
        Ok(Self { sql })
    }

    /// Remove subscriptions whose `refreshBefore` has passed, with their queue
    /// entries. Returns how many were removed.
    pub fn purge_expired(&self, now_ms: i64) -> Result<usize, String> {
        let count = self
            .sql
            .exec(
                "SELECT COUNT(*) AS count FROM incurs_ev_subscriptions WHERE refresh_before_ms <= ?",
                vec![SqlStorageValue::from(now_ms)],
            )
            .map_err(text)?
            .to_array::<CountRow>()
            .map_err(text)?
            .first()
            .map_or(0, |row| row.count as usize);
        self.sql
            .exec(
                "DELETE FROM incurs_ev_queue WHERE subscription_id IN
                   (SELECT id FROM incurs_ev_subscriptions WHERE refresh_before_ms <= ?)",
                vec![SqlStorageValue::from(now_ms)],
            )
            .map_err(text)?;
        self.sql
            .exec(
                "DELETE FROM incurs_ev_subscriptions WHERE refresh_before_ms <= ?",
                vec![SqlStorageValue::from(now_ms)],
            )
            .map_err(text)?;
        Ok(count)
    }

    /// Every stored subscription, live or expired, for inspection.
    pub fn subscriptions(&self) -> Result<Vec<Subscription>, String> {
        self.sql
            .exec(
                "SELECT id, principal, name, arguments, url, secret, refresh_before_ms
                   FROM incurs_ev_subscriptions ORDER BY id",
                None,
            )
            .map_err(text)?
            .to_array::<SubscriptionRow>()
            .map_err(text)?
            .into_iter()
            .map(|row| {
                subscription(
                    row.id,
                    row.principal,
                    row.name,
                    &row.arguments,
                    row.url,
                    row.secret,
                    row.refresh_before_ms,
                )
            })
            .collect()
    }
}

#[async_trait::async_trait(?Send)]
impl SubscriptionStore for DurableEventsStore {
    async fn put(&self, subscription: Subscription) -> Result<(), String> {
        let arguments =
            serde_json::to_string(&subscription.arguments).map_err(|error| error.to_string())?;
        self.sql
            .exec(
                "INSERT OR REPLACE INTO incurs_ev_subscriptions
                   (id, principal, name, arguments, url, secret, refresh_before_ms)
                 VALUES (?, ?, ?, ?, ?, ?, ?)",
                vec![
                    subscription.id.into(),
                    subscription.principal.into(),
                    subscription.name.into(),
                    arguments.into(),
                    subscription.url.into(),
                    subscription.secret.into(),
                    subscription.refresh_before_ms.into(),
                ],
            )
            .map_err(text)?;
        Ok(())
    }

    async fn remove(&self, id: &str) -> Result<(), String> {
        self.sql
            .exec(
                "DELETE FROM incurs_ev_queue WHERE subscription_id = ?",
                vec![id.into()],
            )
            .map_err(text)?;
        self.sql
            .exec(
                "DELETE FROM incurs_ev_subscriptions WHERE id = ?",
                vec![id.into()],
            )
            .map_err(text)?;
        Ok(())
    }
}

#[async_trait::async_trait(?Send)]
impl DeliveryQueue for DurableEventsStore {
    async fn subscriptions_for(
        &self,
        name: &str,
        now_ms: i64,
    ) -> Result<Vec<Subscription>, String> {
        self.sql
            .exec(
                "SELECT id, principal, name, arguments, url, secret, refresh_before_ms
                   FROM incurs_ev_subscriptions WHERE name = ? AND refresh_before_ms > ?",
                vec![name.into(), now_ms.into()],
            )
            .map_err(text)?
            .to_array::<SubscriptionRow>()
            .map_err(text)?
            .into_iter()
            .map(|row| {
                subscription(
                    row.id,
                    row.principal,
                    row.name,
                    &row.arguments,
                    row.url,
                    row.secret,
                    row.refresh_before_ms,
                )
            })
            .collect()
    }

    async fn enqueue(
        &self,
        subscription_id: &str,
        event: &Event,
        due_ms: i64,
    ) -> Result<(), String> {
        let body = serde_json::to_string(&event.to_wire()).map_err(|error| error.to_string())?;
        self.sql
            .exec(
                "INSERT OR IGNORE INTO incurs_ev_queue (subscription_id, event_id, event, attempt, due_ms)
                 VALUES (?, ?, ?, 1, ?)",
                vec![
                    subscription_id.into(),
                    event.event_id.as_str().into(),
                    body.into(),
                    due_ms.into(),
                ],
            )
            .map_err(text)?;
        Ok(())
    }

    async fn due(&self, now_ms: i64, limit: usize) -> Result<Vec<QueuedDelivery>, String> {
        // Entries whose subscription is gone are dropped by the join; remove them.
        self.sql
            .exec(
                "DELETE FROM incurs_ev_queue WHERE subscription_id NOT IN
                   (SELECT id FROM incurs_ev_subscriptions)",
                None,
            )
            .map_err(text)?;
        self.sql
            .exec(
                "SELECT s.id, s.principal, s.name, s.arguments, s.url, s.secret, s.refresh_before_ms,
                        q.event, q.attempt, q.due_ms
                   FROM incurs_ev_queue q JOIN incurs_ev_subscriptions s ON s.id = q.subscription_id
                  WHERE q.due_ms <= ?
                  ORDER BY q.due_ms
                  LIMIT ?",
                vec![now_ms.into(), (limit as i64).into()],
            )
            .map_err(text)?
            .to_array::<QueueRow>()
            .map_err(text)?
            .into_iter()
            .map(|row| {
                let event = serde_json::from_str::<Value>(&row.event)
                    .ok()
                    .as_ref()
                    .and_then(Event::from_wire)
                    .ok_or("queued event is malformed")?;
                Ok(QueuedDelivery {
                    subscription: subscription(
                        row.id,
                        row.principal,
                        row.name,
                        &row.arguments,
                        row.url,
                        row.secret,
                        row.refresh_before_ms,
                    )?,
                    event,
                    attempt: row.attempt as u32,
                    due_ms: row.due_ms as i64,
                })
            })
            .collect()
    }

    async fn reschedule(
        &self,
        subscription_id: &str,
        event_id: &str,
        attempt: u32,
        due_ms: i64,
    ) -> Result<(), String> {
        self.sql
            .exec(
                "UPDATE incurs_ev_queue SET attempt = ?, due_ms = ?
                  WHERE subscription_id = ? AND event_id = ?",
                vec![
                    i64::from(attempt).into(),
                    due_ms.into(),
                    subscription_id.into(),
                    event_id.into(),
                ],
            )
            .map_err(text)?;
        Ok(())
    }

    async fn complete(&self, subscription_id: &str, event_id: &str) -> Result<(), String> {
        self.sql
            .exec(
                "DELETE FROM incurs_ev_queue WHERE subscription_id = ? AND event_id = ?",
                vec![subscription_id.into(), event_id.into()],
            )
            .map_err(text)?;
        Ok(())
    }

    async fn next_due(&self) -> Result<Option<i64>, String> {
        Ok(self
            .sql
            .exec("SELECT MIN(due_ms) AS due_ms FROM incurs_ev_queue", None)
            .map_err(text)?
            .to_array::<DueRow>()
            .map_err(text)?
            .first()
            .and_then(|row| row.due_ms)
            .map(|due| due as i64))
    }
}

#[async_trait::async_trait(?Send)]
impl DedupeStore for DurableEventsStore {
    async fn first_seen(&self, id: &str, now_ms: i64) -> Result<bool, String> {
        self.sql
            .exec(
                "DELETE FROM incurs_ev_received WHERE seen_ms < ?",
                vec![SqlStorageValue::from(now_ms - RECEIVED_RETENTION_MS)],
            )
            .map_err(text)?;
        let known = self
            .sql
            .exec(
                "SELECT COUNT(*) AS count FROM incurs_ev_received WHERE id = ?",
                vec![id.into()],
            )
            .map_err(text)?
            .to_array::<CountRow>()
            .map_err(text)?
            .first()
            .is_some_and(|row| row.count > 0.0);
        if known {
            return Ok(false);
        }
        self.sql
            .exec(
                "INSERT INTO incurs_ev_received (id, seen_ms) VALUES (?, ?)",
                vec![id.into(), now_ms.into()],
            )
            .map_err(text)?;
        Ok(true)
    }
}
