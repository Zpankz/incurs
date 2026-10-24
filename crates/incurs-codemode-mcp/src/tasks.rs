use std::collections::BTreeMap;

use incurs_codemode::{CodeModeRunOptions, ExecutionState, ExecutionStatus, LogEntryState};
use rmcp::model::{
    CallToolResult, DetailedTask, GetTaskResult, ProtocolVersion, Task, TaskPayload, TaskStatus,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer};
use serde_json::{Value, json};

use super::{CodeModeMcpServer, ExecutionProjection, incur_request_context, reduce_execution};

pub(crate) const RETENTION_MS: u64 = 24 * 60 * 60 * 1_000;

pub(crate) fn run_options(name: &str, context: &RequestContext<RoleServer>) -> CodeModeRunOptions {
    CodeModeRunOptions {
        cancellation: tokio_util::sync::CancellationToken::new(),
        request: Some(incur_request_context(
            name,
            context.extensions.get::<http::request::Parts>(),
        )),
    }
}

pub(crate) fn require_modern(context: &RequestContext<RoleServer>) -> Result<(), ErrorData> {
    if context.protocol_version() != Some(ProtocolVersion::V_2026_07_28) {
        return Err(ErrorData::invalid_request(
            "Use the matching Tasks protocol adapter",
            None,
        ));
    }
    Ok(())
}

pub(crate) fn require_form(context: &RequestContext<RoleServer>) -> Result<(), ErrorData> {
    if context
        .client_capabilities()
        .and_then(|caps| caps.elicitation)
        .is_some_and(|caps| caps.form.is_some())
    {
        Ok(())
    } else {
        Err(ErrorData::new(
            rmcp::model::ErrorCode(-32021),
            "This task requires form elicitation",
            Some(json!({"requiredCapabilities":{"elicitation":{"form":{}}}})),
        ))
    }
}

fn timestamp(ms: u64) -> Result<String, ErrorData> {
    time::OffsetDateTime::from_unix_timestamp_nanos(i128::from(ms) * 1_000_000)
        .map_err(|error| ErrorData::internal_error(error.to_string(), None))?
        .format(&time::format_description::well_known::Rfc3339)
        .map_err(|error| ErrorData::internal_error(error.to_string(), None))
}

pub(crate) fn metadata(state: &ExecutionState) -> Result<Task, ErrorData> {
    let status = match state.status {
        ExecutionStatus::Running => TaskStatus::Working,
        ExecutionStatus::Paused => TaskStatus::InputRequired,
        ExecutionStatus::Cancelled => TaskStatus::Cancelled,
        _ => TaskStatus::Completed,
    };
    Ok(Task::new(
        state.id.clone(),
        status,
        timestamp(state.created_at)?,
        timestamp(state.updated_at)?,
    )
    .with_ttl_ms(
        state
            .retained_until
            .unwrap_or(state.created_at)
            .saturating_sub(state.created_at),
    )
    .with_poll_interval_ms(1_000))
}

pub(crate) fn approval_request(state: &ExecutionState, seq: u64) -> Value {
    let entry = state
        .log
        .iter()
        .find(|entry| entry.seq == seq)
        .expect("pending entry exists");
    json!({"method":"elicitation/create", "params":{
        "mode":"form",
        "message":format!("Approve {}.{} with arguments {}?", entry.connector, entry.method, entry.arguments),
        "requestedSchema":{
            "type":"object", "properties":{"approve":{"type":"boolean", "title":"Approve this action"}},
            "required":["approve"]
        }
    }})
}

pub(crate) fn pending(state: &ExecutionState) -> Option<u64> {
    state
        .log
        .iter()
        .find(|entry| entry.state == LogEntryState::Pending)
        .map(|entry| entry.seq)
}

pub(crate) fn result(
    server: &CodeModeMcpServer,
    state: ExecutionState,
) -> Result<CallToolResult, ErrorData> {
    let failed = matches!(
        state.status,
        ExecutionStatus::Error | ExecutionStatus::Rejected | ExecutionStatus::RolledBack
    );
    let value = serde_json::to_value(state)
        .map_err(|error| ErrorData::internal_error(error.to_string(), None))?;
    let value = if server.projection == ExecutionProjection::Reduced {
        reduce_execution(value)
    } else {
        value
    };
    Ok(if failed {
        CallToolResult::structured_error(value)
    } else {
        CallToolResult::structured(value)
    })
}

pub(crate) fn detailed(
    server: &CodeModeMcpServer,
    state: ExecutionState,
    context: &RequestContext<RoleServer>,
) -> Result<GetTaskResult, ErrorData> {
    let task = metadata(&state)?;
    let payload = match state.status {
        ExecutionStatus::Running => TaskPayload::Working,
        ExecutionStatus::Paused => {
            require_form(context)?;
            let seq = pending(&state).ok_or_else(|| {
                ErrorData::internal_error("Paused task has no pending action", None)
            })?;
            let requests = serde_json::from_value(
                json!({format!("approval:{seq}"):approval_request(&state, seq)}),
            )
            .map_err(|error| ErrorData::internal_error(error.to_string(), None))?;
            TaskPayload::InputRequired {
                input_requests: requests,
            }
        }
        ExecutionStatus::Cancelled => TaskPayload::Cancelled,
        _ => TaskPayload::Completed {
            result: serde_json::from_value(
                serde_json::to_value(result(server, state)?)
                    .map_err(|error| ErrorData::internal_error(error.to_string(), None))?,
            )
            .map_err(|error| ErrorData::internal_error(error.to_string(), None))?,
        },
    };
    Ok(GetTaskResult::new(DetailedTask::new(task, payload)))
}

impl CodeModeMcpServer {
    pub(crate) async fn recover_task(
        &self,
        id: String,
        options: CodeModeRunOptions,
    ) -> Result<ExecutionState, ErrorData> {
        let state = self.task_state(id.clone()).await?;
        if state.status != ExecutionStatus::Running {
            return Ok(state);
        }
        self.service
            .recover(id, options)
            .await
            .map_err(|error| ErrorData::internal_error(error, None))
    }
    pub(crate) async fn task_state(&self, id: String) -> Result<ExecutionState, ErrorData> {
        let state = self
            .service
            .execution(id)
            .await
            .map_err(|_| ErrorData::invalid_params("Unknown task", None))?;
        if state.retained_until.is_none() {
            return Err(ErrorData::invalid_params("Unknown task", None));
        }
        Ok(state)
    }

    pub(crate) async fn cancel_task_execution(
        &self,
        id: String,
    ) -> Result<ExecutionState, ErrorData> {
        let state = self.task_state(id.clone()).await?;
        if !matches!(
            state.status,
            ExecutionStatus::Running | ExecutionStatus::Paused
        ) {
            return Err(ErrorData::invalid_params("Task is already terminal", None));
        }
        self.service
            .cancel(id)
            .await
            .map_err(|error| ErrorData::invalid_params(error, None))
    }

    pub(crate) async fn answer_task(
        &self,
        state: ExecutionState,
        responses: BTreeMap<String, Value>,
        options: CodeModeRunOptions,
    ) -> Result<(), ErrorData> {
        let Some(seq) = pending(&state) else {
            return Ok(());
        };
        let Some(response) = responses.get(&format!("approval:{seq}")) else {
            return Ok(());
        };
        let decision = match response.get("action").and_then(Value::as_str) {
            Some("accept") => match response
                .get("content")
                .and_then(|content| content.get("approve"))
                .and_then(Value::as_bool)
            {
                Some(true) => "approve",
                Some(false) => "reject",
                None => {
                    return Err(ErrorData::invalid_params(
                        "Accepted approval requires a boolean approve field",
                        None,
                    ));
                }
            },
            Some("decline") => "reject",
            Some("cancel") => "cancel",
            _ => {
                return Err(ErrorData::invalid_params(
                    "Expected accept, decline, or cancel",
                    None,
                ));
            }
        };
        let outcome = match decision {
            "approve" => self.service.approve_detached(state.id, seq, options).await,
            "reject" => self.service.reject(state.id, seq).await,
            _ => self.service.cancel(state.id).await,
        };
        outcome
            .map(|_| ())
            .map_err(|error| ErrorData::invalid_params(error, None))
    }
}
