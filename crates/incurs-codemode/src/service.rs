use async_trait::async_trait;
use serde_json::Value;

use crate::{CodeModeRunOptions, ExecutionState, SearchOutput};

/// Send-safe lifecycle boundary used by transports and native actor handles.
#[async_trait]
#[allow(
    clippy::double_must_use,
    reason = "async_trait annotates generated futures"
)]
pub trait CodeModeService: Send + Sync {
    /// Recovers interrupted running work without duplicating an active pass.
    /// Hosts with independent job workers may return their current snapshot.
    async fn recover(
        &self,
        execution_id: String,
        _options: CodeModeRunOptions,
    ) -> Result<ExecutionState, String> {
        self.execution(execution_id).await
    }
    /// Whether this host implements retained asynchronous execution and detached approval.
    /// Task-capable hosts must implement both `execute_retained` and `approve_detached`.
    fn supports_retained_execution(&self) -> bool {
        false
    }

    /// Commits an approval before returning and drives its continuation independently.
    async fn approve_detached(
        &self,
        _execution_id: String,
        _seq: u64,
        _options: CodeModeRunOptions,
    ) -> Result<ExecutionState, String> {
        Err("This host does not support detached approval".into())
    }

    /// Starts work retained for the supplied duration, returning before driving it.
    async fn execute_retained(
        &self,
        _code: String,
        _retention_ms: u64,
        _options: CodeModeRunOptions,
    ) -> Result<ExecutionState, String> {
        Err("This host does not support retained asynchronous execution".into())
    }

    /// Searches current connector methods and saved snippets.
    async fn search(&self, query: String) -> Result<SearchOutput, String>;

    /// Starts an execution and returns its durable running state.
    async fn execute(
        &self,
        code: String,
        options: CodeModeRunOptions,
    ) -> Result<ExecutionState, String>;

    /// Returns one bounded execution snapshot by identifier.
    ///
    /// Oversized values remain artifact references so transports can return the
    /// snapshot before retrieving a selected artifact.
    async fn execution(&self, execution_id: String) -> Result<ExecutionState, String>;

    /// Restarts an interrupted running execution with current transport authority.
    /// Paused approvals must be decided through `approve` or `reject`.
    async fn resume(
        &self,
        _execution_id: String,
        _options: CodeModeRunOptions,
    ) -> Result<ExecutionState, String> {
        Err("This host does not support resuming interrupted executions".into())
    }

    /// Returns one artifact owned by an execution.
    async fn artifact(&self, execution_id: String, artifact_id: String) -> Result<Value, String>;

    /// Approves a pending action and drives the next replay pass.
    async fn approve(
        &self,
        execution_id: String,
        seq: u64,
        options: CodeModeRunOptions,
    ) -> Result<ExecutionState, String>;

    /// Rejects a pending action.
    async fn reject(&self, execution_id: String, seq: u64) -> Result<ExecutionState, String>;

    /// Cancels a running or paused execution.
    async fn cancel(&self, execution_id: String) -> Result<ExecutionState, String>;
}
