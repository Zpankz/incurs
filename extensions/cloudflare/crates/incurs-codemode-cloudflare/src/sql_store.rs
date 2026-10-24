use async_trait::async_trait;
use incurs_codemode::{
    ArtifactRef, ArtifactStore, ExecutionCheckpoint, ExecutionState, RuntimeStore, Snippet,
};
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use worker::{Result, SqlStorage, SqlStorageValue};

/// Durable Object SQLite implementation of the Code Mode store.
#[derive(Clone)]
pub struct DurableSqlStore {
    sql: SqlStorage,
}

#[derive(Deserialize)]
struct JsonRow {
    json: String,
}

#[derive(Deserialize)]
struct CheckpointRow {
    part: usize,
    json: String,
}

impl DurableSqlStore {
    /// Creates the Code Mode tables when absent.
    pub fn new(sql: SqlStorage) -> Result<Self> {
        sql.exec(
            "CREATE TABLE IF NOT EXISTS incurs_cm_executions (
                id TEXT PRIMARY KEY,
                json TEXT NOT NULL,
                status TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS incurs_cm_execution_status
               ON incurs_cm_executions(status, created_at);
             CREATE TABLE IF NOT EXISTS incurs_cm_snippets (
                name TEXT PRIMARY KEY,
                json TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS incurs_cm_artifacts (
                id TEXT PRIMARY KEY,
                execution_id TEXT NOT NULL,
                json TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS incurs_cm_checkpoint_parts (
                execution_id TEXT NOT NULL,
                part INTEGER NOT NULL,
                json TEXT NOT NULL,
                PRIMARY KEY(execution_id, part)
             );",
            None,
        )?;
        Ok(Self { sql })
    }
}

#[async_trait]
impl ArtifactStore for DurableSqlStore {
    async fn put(
        &self,
        execution_id: &str,
        value: &Value,
    ) -> std::result::Result<ArtifactRef, String> {
        let bytes = serde_json::to_vec(value).map_err(|error| error.to_string())?;
        let id = format!(
            "{:x}",
            Sha256::digest([execution_id.as_bytes(), &bytes].concat())
        );
        self.sql
            .exec(
                "INSERT OR REPLACE INTO incurs_cm_artifacts (id, execution_id, json)
                 VALUES (?, ?, ?)",
                vec![
                    id.clone().into(),
                    execution_id.into(),
                    String::from_utf8_lossy(&bytes).into_owned().into(),
                ],
            )
            .map_err(|error| error.to_string())?;
        let mut preview = String::from_utf8_lossy(&bytes).into_owned();
        if preview.len() > 512 {
            let mut end = 512;
            while !preview.is_char_boundary(end) {
                end -= 1;
            }
            preview.truncate(end);
        }
        Ok(ArtifactRef {
            id,
            execution_id: execution_id.to_string(),
            bytes: bytes.len(),
            preview,
        })
    }

    async fn get(
        &self,
        execution_id: &str,
        artifact_id: &str,
    ) -> std::result::Result<Option<Value>, String> {
        let rows = self
            .sql
            .exec(
                "SELECT json FROM incurs_cm_artifacts WHERE id = ? AND execution_id = ?",
                vec![artifact_id.into(), execution_id.into()],
            )
            .map_err(|error| error.to_string())?
            .to_array::<JsonRow>()
            .map_err(|error| error.to_string())?;
        rows.into_iter()
            .next()
            .map(|row| serde_json::from_str(&row.json).map_err(|error| error.to_string()))
            .transpose()
    }

    async fn delete_execution(&self, execution_id: &str) -> std::result::Result<(), String> {
        self.sql
            .exec(
                "DELETE FROM incurs_cm_artifacts WHERE execution_id = ?",
                vec![execution_id.into()],
            )
            .map_err(|error| error.to_string())?;
        Ok(())
    }
}

#[async_trait]
impl RuntimeStore for DurableSqlStore {
    async fn get_checkpoint(
        &self,
        id: &str,
    ) -> std::result::Result<Option<ExecutionCheckpoint>, String> {
        let rows = self
            .sql
            .exec(
                "SELECT part, json FROM incurs_cm_checkpoint_parts WHERE execution_id = ? ORDER BY part",
                vec![id.into()],
            )
            .map_err(|e| e.to_string())?
            .to_array::<CheckpointRow>()
            .map_err(|e| e.to_string())?;
        let Some(header) = rows.first() else {
            return Ok(None);
        };
        let count = header.json.parse::<usize>().map_err(|e| e.to_string())?;
        if header.part != 0 || count > 32 || rows.len() < count + 1 {
            return Err("Incomplete engine checkpoint".into());
        }
        let mut json = String::new();
        for (index, row) in rows.into_iter().skip(1).take(count).enumerate() {
            if row.part != index + 1 {
                return Err("Incomplete engine checkpoint".into());
            }
            json.push_str(&row.json);
        }
        serde_json::from_str(&json)
            .map(Some)
            .map_err(|e| e.to_string())
    }

    async fn put_checkpoint(
        &self,
        id: &str,
        checkpoint: &ExecutionCheckpoint,
    ) -> std::result::Result<(), String> {
        let json = serde_json::to_string(checkpoint).map_err(|e| e.to_string())?;
        if json.len() > 32_000_000 {
            return Err("Engine checkpoint exceeds 32 MB storage limit".into());
        }
        let mut chunks = Vec::new();
        let mut start = 0;
        while start < json.len() {
            let mut end = (start + 1_000_000).min(json.len());
            while !json.is_char_boundary(end) {
                end -= 1;
            }
            chunks.push(json[start..end].to_string());
            start = end;
        }
        let count = chunks.len();
        let mut values = vec![SqlStorageValue::from(id)];
        let mut bindings = Vec::new();
        for (part, text) in std::iter::once(count.to_string()).chain(chunks).enumerate() {
            let position = values.len() + 1;
            bindings.push(format!("(?1, ?{position}, ?{})", position + 1));
            values.push((part as i64).into());
            values.push(text.into());
        }
        self.sql.exec(&format!("INSERT OR REPLACE INTO incurs_cm_checkpoint_parts (execution_id, part, json) VALUES {}", bindings.join(",")), values).map_err(|e| e.to_string())?;
        self.sql
            .exec(
                "DELETE FROM incurs_cm_checkpoint_parts WHERE execution_id = ? AND part > ?",
                vec![id.into(), (count as i64).into()],
            )
            .map_err(|e| e.to_string())?;
        Ok(())
    }
    async fn get_execution(&self, id: &str) -> std::result::Result<Option<ExecutionState>, String> {
        let rows = self
            .sql
            .exec(
                "SELECT json FROM incurs_cm_executions WHERE id = ?",
                vec![id.into()],
            )
            .map_err(|error| error.to_string())?
            .to_array::<JsonRow>()
            .map_err(|error| error.to_string())?;
        rows.into_iter()
            .next()
            .map(|row| serde_json::from_str(&row.json).map_err(|error| error.to_string()))
            .transpose()
    }

    async fn put_execution(&self, execution: &ExecutionState) -> std::result::Result<(), String> {
        self.sql
            .exec(
                "INSERT OR REPLACE INTO incurs_cm_executions
                   (id, json, status, created_at, updated_at)
                 VALUES (?, ?, ?, ?, ?)",
                vec![
                    execution.id.clone().into(),
                    serde_json::to_string(execution)
                        .map_err(|error| error.to_string())?
                        .into(),
                    format!("{:?}", execution.status).to_lowercase().into(),
                    (execution.created_at as i64).into(),
                    (execution.updated_at as i64).into(),
                ],
            )
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    async fn list_executions(&self) -> std::result::Result<Vec<ExecutionState>, String> {
        self.sql
            .exec(
                "SELECT json FROM incurs_cm_executions ORDER BY created_at DESC, id DESC",
                None,
            )
            .map_err(|error| error.to_string())?
            .to_array::<JsonRow>()
            .map_err(|error| error.to_string())?
            .into_iter()
            .map(|row| serde_json::from_str(&row.json).map_err(|error| error.to_string()))
            .collect()
    }

    async fn delete_execution(&self, id: &str) -> std::result::Result<(), String> {
        self.sql
            .exec(
                "DELETE FROM incurs_cm_checkpoint_parts WHERE execution_id = ?",
                vec![id.into()],
            )
            .map_err(|e| e.to_string())?;
        self.sql
            .exec(
                "DELETE FROM incurs_cm_executions WHERE id = ?",
                vec![id.into()],
            )
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    async fn get_snippet(&self, name: &str) -> std::result::Result<Option<Snippet>, String> {
        let rows = self
            .sql
            .exec(
                "SELECT json FROM incurs_cm_snippets WHERE name = ?",
                vec![name.into()],
            )
            .map_err(|error| error.to_string())?
            .to_array::<JsonRow>()
            .map_err(|error| error.to_string())?;
        rows.into_iter()
            .next()
            .map(|row| serde_json::from_str(&row.json).map_err(|error| error.to_string()))
            .transpose()
    }

    async fn put_snippet(&self, snippet: &Snippet) -> std::result::Result<(), String> {
        self.sql
            .exec(
                "INSERT OR REPLACE INTO incurs_cm_snippets (name, json) VALUES (?, ?)",
                vec![
                    snippet.name.clone().into(),
                    serde_json::to_string(snippet)
                        .map_err(|error| error.to_string())?
                        .into(),
                ],
            )
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    async fn list_snippets(&self) -> std::result::Result<Vec<Snippet>, String> {
        self.sql
            .exec("SELECT json FROM incurs_cm_snippets ORDER BY name", None)
            .map_err(|error| error.to_string())?
            .to_array::<JsonRow>()
            .map_err(|error| error.to_string())?
            .into_iter()
            .map(|row| serde_json::from_str(&row.json).map_err(|error| error.to_string()))
            .collect()
    }

    async fn delete_snippet(&self, name: &str) -> std::result::Result<bool, String> {
        Ok(self
            .sql
            .exec(
                "DELETE FROM incurs_cm_snippets WHERE name = ?",
                vec![SqlStorageValue::from(name)],
            )
            .map_err(|error| error.to_string())?
            .rows_written()
            > 0)
    }
}
