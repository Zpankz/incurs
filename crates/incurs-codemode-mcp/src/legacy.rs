//! Session-based MCP 2025-11-25 Tasks adapter.

use std::sync::Arc;

use incurs_codemode::{CodeModeService, ExecutionState, ExecutionStatus};
use rmcp_legacy::model::*;
use rmcp_legacy::service::RequestContext;
use rmcp_legacy::{ErrorData, RoleServer, ServerHandler};
use serde_json::json;

use super::{CodeModeMcpServer, ExecutionProjection, tasks};

/// MCP 2025-11-25 server supporting task-augmented Code Mode execution.
///
/// Use this adapter for clients using the session-based Tasks protocol. The
/// current stateless Tasks extension is served by [`CodeModeMcpServer`]. Each
/// instance must be bound to one authorized service/storage partition.
#[derive(Clone)]
pub struct LegacyCodeModeMcpServer {
    inner: CodeModeMcpServer,
}

mod model;
use model::{error, options, task};

impl LegacyCodeModeMcpServer {
    /// Creates a server over a host that supports retained asynchronous execution.
    pub fn new(service: Arc<dyn CodeModeService>) -> Self {
        Self {
            inner: CodeModeMcpServer::new(service),
        }
    }

    /// Sets the amount of execution detail included in final task results.
    #[must_use]
    pub fn with_projection(mut self, projection: ExecutionProjection) -> Self {
        self.inner = self.inner.with_projection(projection);
        self
    }

    /// Sets the product name and version reported during initialization.
    #[must_use]
    pub fn with_identity(mut self, name: impl Into<String>, version: impl Into<String>) -> Self {
        self.inner = self.inner.with_identity(name, version);
        self
    }

    fn tools(&self) -> Vec<Tool> {
        self.inner
            .tools()
            .iter()
            .map(|tool| {
                let mut value = serde_json::to_value(tool).expect("serializable tool");
                if tool.name == "codemode_execute"
                    && self.inner.service.supports_retained_execution()
                {
                    value["execution"] = json!({"taskSupport":"optional"});
                }
                serde_json::from_value(value).expect("shared tool schema")
            })
            .collect()
    }
}

impl LegacyCodeModeMcpServer {
    async fn elicit_approval(
        &self,
        state: ExecutionState,
        task_id: &str,
        context: &RequestContext<RoleServer>,
    ) -> Result<(), ErrorData> {
        let supports_form = context
            .peer
            .peer_info()
            .and_then(|info| info.capabilities.elicitation.clone())
            .is_some_and(|cap| cap.form.is_some() || cap.url.is_none());
        if !supports_form {
            return Err(ErrorData::invalid_params(
                "This task requires form elicitation",
                None,
            ));
        }
        let seq = tasks::pending(&state)
            .ok_or_else(|| ErrorData::internal_error("Paused task has no pending action", None))?;
        let mut params = tasks::approval_request(&state, seq)["params"].clone();
        params["_meta"] = json!({"io.modelcontextprotocol/related-task":{"taskId":task_id}});
        let params: CreateElicitationRequestParams = serde_json::from_value(params)
            .map_err(|error| ErrorData::internal_error(error.to_string(), None))?;
        let response = tokio::select! {
            _ = context.ct.cancelled() => return Err(ErrorData::internal_error("Request cancelled", None)),
            response = context.peer.create_elicitation(params) => response.map_err(|error| ErrorData::internal_error(error.to_string(), None))?,
        };
        self.inner
            .answer_task(
                state,
                [(
                    format!("approval:{seq}"),
                    serde_json::to_value(response).expect("elicitation response"),
                )]
                .into(),
                options("tasks/result", context),
            )
            .await
            .map_err(error)?;
        Ok(())
    }
}

impl ServerHandler for LegacyCodeModeMcpServer {
    fn get_info(&self) -> ServerInfo {
        let mut capabilities = ServerCapabilities::builder()
            .enable_tools()
            .enable_prompts()
            .enable_resources()
            .build();
        if self.inner.service.supports_retained_execution() {
            capabilities.tasks = Some(
                serde_json::from_value(json!({"cancel":{}, "requests":{"tools":{"call":{}}}}))
                    .expect("task capabilities"),
            );
        }
        let mut info = ServerInfo::new(capabilities);
        info.protocol_version = ProtocolVersion::V_2025_11_25;
        info.server_info = Implementation::new(
            self.inner.identity.name.clone(),
            self.inner.identity.version.clone(),
        );
        info
    }

    async fn initialize(
        &self,
        request: InitializeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<InitializeResult, ErrorData> {
        if request.protocol_version != ProtocolVersion::V_2025_11_25 {
            return Err(ErrorData::invalid_params(
                "This adapter requires MCP 2025-11-25",
                None,
            ));
        }
        context.peer.set_peer_info(request);
        Ok(self.get_info())
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        self.tools().into_iter().find(|tool| tool.name == name)
    }

    async fn list_prompts(
        &self,
        _: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, ErrorData> {
        Ok(Default::default())
    }
    async fn list_resources(
        &self,
        _: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        Ok(Default::default())
    }
    async fn list_resource_templates(
        &self,
        _: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, ErrorData> {
        Ok(Default::default())
    }

    async fn list_tools(
        &self,
        _: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult {
            tools: self.tools(),
            next_cursor: None,
            meta: None,
        })
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let options = options(&request.name, &context);
        let result = self
            .inner
            .call_lifecycle(
                &request.name,
                request.arguments.unwrap_or_default(),
                options,
            )
            .await
            .map_err(error)?;
        serde_json::from_value(serde_json::to_value(result).expect("tool result"))
            .map_err(|error| ErrorData::internal_error(error.to_string(), None))
    }

    async fn enqueue_task(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CreateTaskResult, ErrorData> {
        if request.name != "codemode_execute" || !self.inner.service.supports_retained_execution() {
            return Err(ErrorData::invalid_params(
                "Tool does not support Tasks",
                None,
            ));
        }
        if request
            .task
            .as_ref()
            .and_then(|task| task.get("ttl"))
            .is_some_and(|ttl| ttl.as_u64().is_none())
        {
            return Err(ErrorData::invalid_params(
                "Task ttl must be a nonnegative integer",
                None,
            ));
        }
        let arguments = request.arguments.unwrap_or_default();
        let code = super::string(&arguments, "code").map_err(error)?;
        let state = self
            .inner
            .service
            .execute_retained(
                code.into(),
                tasks::RETENTION_MS,
                options("codemode_execute", &context),
            )
            .await
            .map_err(|error| ErrorData::internal_error(error, None))?;
        Ok(CreateTaskResult::new(task(&state)?))
    }

    async fn get_task_info(
        &self,
        request: GetTaskInfoParams,
        context: RequestContext<RoleServer>,
    ) -> Result<GetTaskResult, ErrorData> {
        let state = self
            .inner
            .recover_task(request.task_id, options("tasks/get", &context))
            .await
            .map_err(error)?;
        Ok(GetTaskResult {
            meta: None,
            task: task(&state)?,
        })
    }

    async fn get_task_result(
        &self,
        request: GetTaskResultParams,
        context: RequestContext<RoleServer>,
    ) -> Result<GetTaskPayloadResult, ErrorData> {
        loop {
            let state = self
                .inner
                .recover_task(request.task_id.clone(), options("tasks/result", &context))
                .await
                .map_err(error)?;
            match state.status {
                ExecutionStatus::Paused => {
                    self.elicit_approval(state, &request.task_id, &context)
                        .await?
                }
                ExecutionStatus::Running => {
                    tokio::select! {
                        _ = context.ct.cancelled() => return Err(ErrorData::internal_error("Request cancelled", None)),
                        _ = tokio::time::sleep(std::time::Duration::from_millis(10)) => {}
                    }
                }
                _ => {
                    let mut value =
                        serde_json::to_value(tasks::result(&self.inner, state).map_err(error)?)
                            .expect("task result");
                    value
                        .as_object_mut()
                        .expect("result object")
                        .remove("resultType");
                    value["_meta"] =
                        json!({"io.modelcontextprotocol/related-task":{"taskId":request.task_id}});
                    return Ok(GetTaskPayloadResult::new(value));
                }
            }
        }
    }

    async fn cancel_task(
        &self,
        request: CancelTaskParams,
        _: RequestContext<RoleServer>,
    ) -> Result<CancelTaskResult, ErrorData> {
        let state = self
            .inner
            .cancel_task_execution(request.task_id)
            .await
            .map_err(error)?;
        Ok(CancelTaskResult {
            meta: None,
            task: task(&state)?,
        })
    }
}
