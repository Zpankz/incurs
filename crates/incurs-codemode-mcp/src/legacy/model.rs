//! MCP 2025-11-25 wire projections.
use crate::tasks;
use incurs_codemode::{CodeModeRunOptions, ExecutionState, ExecutionStatus};
use rmcp_legacy::model::Task;
use rmcp_legacy::service::RequestContext;
use rmcp_legacy::{ErrorData, RoleServer};
use serde_json::json;

pub(super) fn error(error: rmcp::ErrorData) -> ErrorData {
    serde_json::from_value(serde_json::to_value(error).expect("serializable error"))
        .expect("shared JSON-RPC error")
}

pub(super) fn task(state: &ExecutionState) -> Result<Task, ErrorData> {
    let mut value = serde_json::to_value(tasks::metadata(state).map_err(error)?)
        .expect("serializable metadata");
    let object = value.as_object_mut().expect("task object");
    let ttl = object.remove("ttlMs").expect("TTL");
    let interval = object.remove("pollIntervalMs").expect("poll interval");
    if matches!(
        state.status,
        ExecutionStatus::Error | ExecutionStatus::Rejected | ExecutionStatus::RolledBack
    ) {
        object.insert("status".into(), json!("failed"));
        object.insert(
            "statusMessage".into(),
            json!(state.error.as_deref().unwrap_or("Execution failed")),
        );
    }
    object.insert("ttl".into(), ttl);
    object.insert("pollInterval".into(), interval);
    serde_json::from_value(value)
        .map_err(|error| ErrorData::internal_error(error.to_string(), None))
}

pub(super) fn options(name: &str, context: &RequestContext<RoleServer>) -> CodeModeRunOptions {
    CodeModeRunOptions {
        cancellation: tokio_util::sync::CancellationToken::new(),
        request: Some(crate::incur_request_context(
            name,
            context.extensions.get::<http::request::Parts>(),
        )),
    }
}
