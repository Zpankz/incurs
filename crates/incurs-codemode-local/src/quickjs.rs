use std::sync::Arc;
use std::time::Duration;

use incurs_codemode::{
    CodeExecutor, ConnectorDescription, ExecuteResult, ExecutionCheckpoint, ExecutionHost,
    build_checkpoint_source,
};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::native::Engine;

/// Resource limits for JavaScript intervals between durable host calls.
#[derive(Debug, Clone)]
pub struct LocalExecutorOptions {
    /// Maximum uninterrupted wall-clock time spent evaluating JavaScript.
    pub timeout: Duration,
    /// Maximum guest JavaScript heap in bytes.
    pub memory_limit: usize,
    /// Maximum guest stack in bytes.
    pub stack_size: usize,
}

impl Default for LocalExecutorOptions {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(60),
            memory_limit: 64 * 1024 * 1024,
            stack_size: 512 * 1024,
        }
    }
}

/// Executes Code Mode in a dquickts runtime checkpointed before every host call.
///
/// The runtime store supplies continuations. A persistent store enables recovery
/// across host restarts; `MemoryStore` retains checkpoints only in memory.
#[derive(Debug, Clone, Default)]
pub struct LocalExecutor {
    options: LocalExecutorOptions,
}

impl LocalExecutor {
    /// Creates a checkpointing executor with explicit resource limits.
    pub fn new(options: LocalExecutorOptions) -> Self {
        Self { options }
    }
}

#[derive(Deserialize)]
struct State {
    output: Option<String>,
    failure: Option<String>,
    pending: Option<Value>,
}

#[async_trait::async_trait(?Send)]
impl CodeExecutor for LocalExecutor {
    async fn execute(
        &self,
        code: &str,
        connectors: &[ConnectorDescription],
        execution_id: &str,
        host: Arc<ExecutionHost>,
    ) -> Result<ExecuteResult, String> {
        let source = build_checkpoint_source(code, connectors, execution_id)?;
        let identity = program_identity(&source);
        let saved = host.checkpoint().await?;
        if saved
            .as_ref()
            .is_some_and(|saved| saved.identity != identity)
        {
            return Err("Checkpoint identity mismatch; refusing to replay source".into());
        }
        let engine = Engine::new(
            &self.options,
            &source,
            saved.as_ref().map(|value| value.bytes.as_slice()),
        )?;
        run(engine, host, identity).await
    }
}

fn program_identity(source: &str) -> String {
    let mut fingerprint = Sha256::new();
    fingerprint.update(env!("INCURS_ENGINE_BUILD"));
    fingerprint.update(format!(
        "native-v1/{}/{}/{}/{}/{}",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH,
        usize::BITS,
        cfg!(debug_assertions)
    ));
    fingerprint.update(source.as_bytes());
    format!("dquickts/{:x}", fingerprint.finalize())
}

async fn run(
    engine: Engine,
    host: Arc<ExecutionHost>,
    identity: String,
) -> Result<ExecuteResult, String> {
    loop {
        engine.pump()?;
        let state: State = serde_json::from_str(&engine.state()?).map_err(error)?;
        host.save_checkpoint(&ExecutionCheckpoint {
            identity: identity.clone(),
            bytes: engine.snapshot()?,
        })
        .await?;
        if let Some(output) = state.output {
            return serde_json::from_str(&output).map_err(error);
        }
        if let Some(failure) = state.failure {
            return Err(failure);
        }
        let Some(pending) = state.pending else {
            // The heap is durable while an unresolved promise awaits cancellation.
            return std::future::pending().await;
        };
        let response = dispatch(&host, pending).await;
        if response["__codemode_control__"] == "pause" || response["kind"] == "pause" {
            return Ok(ExecuteResult {
                result: None,
                error: None,
                logs: Vec::new(),
            });
        }
        engine.respond(&serde_json::to_string(&response).map_err(error)?)?;
    }
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Request {
    Call {
        seq: u64,
        connector: String,
        method: String,
        arguments: Value,
    },
    BeginStep {
        seq: u64,
        name: String,
    },
    RecordStep {
        seq: u64,
        result: Value,
    },
}

async fn dispatch(host: &ExecutionHost, payload: Value) -> Value {
    let response = match serde_json::from_value::<Request>(payload) {
        Ok(Request::Call {
            seq,
            connector,
            method,
            arguments,
        }) => serde_json::to_value(host.call(seq, &connector, &method, arguments).await)
            .map_err(error),
        Ok(Request::BeginStep { seq, name }) => host
            .begin_step(seq, &name)
            .await
            .and_then(|response| serde_json::to_value(response).map_err(error)),
        Ok(Request::RecordStep { seq, result }) => host
            .record_step(seq, result)
            .await
            .map(|()| json!({"ok":true})),
        Err(e) => Err(error(e)),
    };
    response.unwrap_or_else(|message| json!({"__codemode_control__":"error", "message":message}))
}

fn error(value: impl std::fmt::Display) -> String {
    value.to_string()
}
