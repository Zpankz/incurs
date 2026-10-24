use std::fs::OpenOptions;
use std::path::Path;
use std::sync::Mutex;

use async_trait::async_trait;
use incurs_codemode::{
    ArtifactRef, ArtifactStore, ExecutionCheckpoint, ExecutionState, RuntimeStore, Snippet,
};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;
use sha2::{Digest, Sha256};

/// SQLite execution, artifact, snippet, and heap storage owned by one host.
///
/// Keep this store in an `Arc` shared by the executor and Code Mode. A second
/// opener is rejected until the owning store is dropped or its process exits.
/// The database contains trusted engine bytecode and must not be user-uploaded.
pub struct DurableStore {
    connection: Mutex<Connection>,
}

impl DurableStore {
    /// Opens a file database with full synchronous writes and an exclusive host lease.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, String> {
        let path = path.as_ref();
        if path.as_os_str().is_empty() || path == Path::new(":memory:") {
            return Err("Durable store requires a file database".into());
        }
        let mut options = OpenOptions::new();
        options.create(true).truncate(false).read(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        drop(options.open(path).map_err(error)?);
        let connection = Connection::open(path).map_err(error)?;
        connection
            .busy_timeout(std::time::Duration::ZERO)
            .map_err(error)?;
        connection
            .execute_batch(
                "PRAGMA locking_mode=EXCLUSIVE; PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;
             BEGIN EXCLUSIVE;
             CREATE TABLE IF NOT EXISTS records (
               kind TEXT NOT NULL, key TEXT NOT NULL, owner TEXT NOT NULL,
               data BLOB NOT NULL, PRIMARY KEY(kind, key, owner));
             CREATE TABLE IF NOT EXISTS checkpoints (
               id TEXT PRIMARY KEY, identity TEXT NOT NULL, heap BLOB NOT NULL);
             COMMIT;",
            )
            .map_err(|e| {
                format!("Durable store ownership unavailable or initialization failed: {e}")
            })?;
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    fn get<T: DeserializeOwned>(
        &self,
        kind: &str,
        key: &str,
        owner: &str,
    ) -> Result<Option<T>, String> {
        let data: Option<Vec<u8>> = self
            .connection
            .lock()
            .map_err(error)?
            .query_row(
                "SELECT data FROM records WHERE kind=?1 AND key=?2 AND owner=?3",
                params![kind, key, owner],
                |row| row.get(0),
            )
            .optional()
            .map_err(error)?;
        data.map(|bytes| serde_json::from_slice(&bytes).map_err(error))
            .transpose()
    }

    fn put<T: Serialize>(
        &self,
        kind: &str,
        key: &str,
        owner: &str,
        value: &T,
    ) -> Result<(), String> {
        let data = serde_json::to_vec(value).map_err(error)?;
        self.connection
            .lock()
            .map_err(error)?
            .execute(
                "INSERT OR REPLACE INTO records VALUES (?1, ?2, ?3, ?4)",
                params![kind, key, owner, data],
            )
            .map_err(error)?;
        Ok(())
    }

    fn list<T: DeserializeOwned>(&self, kind: &str) -> Result<Vec<T>, String> {
        let connection = self.connection.lock().map_err(error)?;
        let mut statement = connection
            .prepare("SELECT data FROM records WHERE kind=?1 ORDER BY key")
            .map_err(error)?;
        statement
            .query_map([kind], |row| row.get::<_, Vec<u8>>(0))
            .map_err(error)?
            .map(|row| serde_json::from_slice(&row.map_err(error)?).map_err(error))
            .collect()
    }

    fn checkpoint(&self, id: &str) -> Result<Option<ExecutionCheckpoint>, String> {
        let row: Option<(String, Vec<u8>)> = self
            .connection
            .lock()
            .map_err(error)?
            .query_row(
                "SELECT identity, heap FROM checkpoints WHERE id=?1",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(error)?;
        Ok(row.map(|(identity, bytes)| ExecutionCheckpoint { identity, bytes }))
    }

    pub(crate) fn save_checkpoint(
        &self,
        id: &str,
        identity: &str,
        heap: &[u8],
    ) -> Result<(), String> {
        self.connection
            .lock()
            .map_err(error)?
            .execute(
                "INSERT OR REPLACE INTO checkpoints VALUES (?1, ?2, ?3)",
                params![id, identity, heap],
            )
            .map_err(error)?;
        Ok(())
    }
}

#[async_trait]
impl RuntimeStore for DurableStore {
    async fn get_checkpoint(&self, id: &str) -> Result<Option<ExecutionCheckpoint>, String> {
        self.checkpoint(id)
    }
    async fn put_checkpoint(
        &self,
        id: &str,
        checkpoint: &ExecutionCheckpoint,
    ) -> Result<(), String> {
        self.save_checkpoint(id, &checkpoint.identity, &checkpoint.bytes)
    }
    async fn get_execution(&self, id: &str) -> Result<Option<ExecutionState>, String> {
        self.get("execution", id, "")
    }
    async fn put_execution(&self, value: &ExecutionState) -> Result<(), String> {
        self.put("execution", &value.id, "", value)
    }
    async fn list_executions(&self) -> Result<Vec<ExecutionState>, String> {
        self.list("execution")
    }
    async fn delete_execution(&self, id: &str) -> Result<(), String> {
        let mut connection = self.connection.lock().map_err(error)?;
        let tx = connection.transaction().map_err(error)?;
        tx.execute("DELETE FROM records WHERE (kind='execution' AND key=?1) OR (kind='artifact' AND owner=?1)", [id]).map_err(error)?;
        tx.execute("DELETE FROM checkpoints WHERE id=?1", [id])
            .map_err(error)?;
        tx.commit().map_err(error)
    }
    async fn get_snippet(&self, name: &str) -> Result<Option<Snippet>, String> {
        self.get("snippet", name, "")
    }
    async fn put_snippet(&self, value: &Snippet) -> Result<(), String> {
        self.put("snippet", &value.name, "", value)
    }
    async fn list_snippets(&self) -> Result<Vec<Snippet>, String> {
        self.list("snippet")
    }
    async fn delete_snippet(&self, name: &str) -> Result<bool, String> {
        self.connection
            .lock()
            .map_err(error)?
            .execute(
                "DELETE FROM records WHERE kind='snippet' AND key=?1",
                [name],
            )
            .map(|count| count > 0)
            .map_err(error)
    }
}

#[async_trait]
impl ArtifactStore for DurableStore {
    async fn put(&self, execution_id: &str, value: &Value) -> Result<ArtifactRef, String> {
        let data = serde_json::to_vec(value).map_err(error)?;
        let id = format!(
            "{:x}",
            Sha256::digest([execution_id.as_bytes(), &data].concat())
        );
        self.put("artifact", &id, execution_id, value)?;
        Ok(ArtifactRef {
            id,
            execution_id: execution_id.into(),
            bytes: data.len(),
            preview: String::from_utf8_lossy(&data).chars().take(256).collect(),
        })
    }
    async fn get(&self, execution_id: &str, artifact_id: &str) -> Result<Option<Value>, String> {
        self.get("artifact", artifact_id, execution_id)
    }
    async fn delete_execution(&self, execution_id: &str) -> Result<(), String> {
        self.connection
            .lock()
            .map_err(error)?
            .execute(
                "DELETE FROM records WHERE kind='artifact' AND owner=?1",
                [execution_id],
            )
            .map_err(error)?;
        Ok(())
    }
}

fn error(value: impl std::fmt::Display) -> String {
    value.to_string()
}
