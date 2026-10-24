//! Native actor service and optional QuickJS execution adapter for Code Mode.

#[cfg(all(feature = "quickjs", feature = "sqlite"))]
use std::path::Path;
use std::rc::Rc;
#[cfg(all(feature = "quickjs", feature = "sqlite"))]
use std::sync::Arc;

use incurs_codemode::{
    CodeMode, CodeModeRunOptions, CodeModeService, ExecutionState, SearchOutput,
};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};

#[cfg(feature = "quickjs")]
mod native;
#[cfg(feature = "quickjs")]
mod quickjs;
#[cfg(feature = "quickjs")]
pub use quickjs::{LocalExecutor, LocalExecutorOptions};

#[cfg(feature = "sqlite")]
mod store;
#[cfg(feature = "sqlite")]
pub use store::DurableStore;

/// Opens Code Mode with its journal, artifacts, and heap in one SQLite database.
#[cfg(all(feature = "quickjs", feature = "sqlite"))]
pub fn open_codemode(
    path: impl AsRef<Path>,
    options: LocalExecutorOptions,
    connectors: Vec<Arc<dyn incurs_codemode::Connector>>,
) -> Result<CodeMode, String> {
    let store = Arc::new(DurableStore::open(path)?);
    Ok(CodeMode::with_artifact_store(
        store.clone(),
        store,
        LocalExecutor::new(options),
        connectors,
    ))
}

/// Opens a durable actor service for a CLI or MCP host.
///
/// The host owns the database until its service thread exits. Restarting with
/// the same path preserves approvals, artifacts, and engine continuations.
#[cfg(all(feature = "quickjs", feature = "sqlite"))]
pub fn spawn_codemode(
    path: impl AsRef<Path>,
    options: LocalExecutorOptions,
    connectors: Vec<Arc<dyn incurs_codemode::Connector>>,
) -> Result<LocalCodeModeService, String> {
    let store = Arc::new(DurableStore::open(path)?);
    LocalCodeModeService::spawn(move || {
        CodeMode::with_artifact_store(
            store.clone(),
            store,
            LocalExecutor::new(options),
            connectors,
        )
    })
}

fn random_execution_id() -> Result<String, String> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let mut bytes = [0_u8; 24];
        getrandom::fill(&mut bytes).map_err(|error| error.to_string())?;
        Ok(format!(
            "exec_{}",
            bytes
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        ))
    }
    #[cfg(target_arch = "wasm32")]
    {
        Err("SECURE_EXECUTION_ID_UNAVAILABLE: host must supply secure randomness".into())
    }
}

enum ServiceRequest {
    Search {
        query: String,
        response: oneshot::Sender<Result<SearchOutput, String>>,
    },
    Execute {
        code: String,
        retention_ms: Option<u64>,
        options: CodeModeRunOptions,
        response: oneshot::Sender<Result<ExecutionState, String>>,
    },
    Execution {
        execution_id: String,
        response: oneshot::Sender<Result<ExecutionState, String>>,
    },
    Resume {
        execution_id: String,
        recover: bool,
        options: CodeModeRunOptions,
        response: oneshot::Sender<Result<ExecutionState, String>>,
    },
    Artifact {
        execution_id: String,
        artifact_id: String,
        response: oneshot::Sender<Result<Value, String>>,
    },
    Approve {
        execution_id: String,
        detached: bool,
        seq: u64,
        options: CodeModeRunOptions,
        response: oneshot::Sender<Result<ExecutionState, String>>,
    },
    Reject {
        execution_id: String,
        seq: u64,
        response: oneshot::Sender<Result<ExecutionState, String>>,
    },
    Cancel {
        execution_id: String,
        response: oneshot::Sender<Result<ExecutionState, String>>,
    },
}

/// Send-safe actor handle for a local non-`Send` Code Mode runtime.
#[derive(Clone)]
pub struct LocalCodeModeService {
    sender: mpsc::Sender<ServiceRequest>,
}

impl LocalCodeModeService {
    /// Starts a dedicated current-thread runtime and constructs Code Mode on it.
    pub fn spawn(factory: impl FnOnce() -> CodeMode + Send + 'static) -> Result<Self, String> {
        Self::spawn_on_thread(
            std::thread::Builder::new().name("incurs-codemode-local".to_string()),
            factory,
        )
    }

    /// Starts an actor with an explicit native stack size in bytes.
    ///
    /// Allow headroom beyond the executor's QuickJS stack limit so the engine
    /// can raise a JavaScript exception before exhausting the host thread.
    pub fn spawn_with_stack_size(
        stack_size: usize,
        factory: impl FnOnce() -> CodeMode + Send + 'static,
    ) -> Result<Self, String> {
        Self::spawn_on_thread(
            std::thread::Builder::new()
                .name("incurs-codemode-local".to_string())
                .stack_size(stack_size),
            factory,
        )
    }

    fn spawn_on_thread(
        thread: std::thread::Builder,
        factory: impl FnOnce() -> CodeMode + Send + 'static,
    ) -> Result<Self, String> {
        let (sender, receiver) = mpsc::channel(64);
        let (ready_sender, ready_receiver) = std::sync::mpsc::sync_channel(1);
        thread
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_time()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        let _ = ready_sender.send(Err(error.to_string()));
                        return;
                    }
                };
                let codemode = Rc::new(factory());
                let _ = ready_sender.send(Ok(()));
                tokio::task::LocalSet::new().block_on(&runtime, serve_requests(codemode, receiver));
            })
            .map_err(|error| error.to_string())?;
        ready_receiver.recv().map_err(|error| error.to_string())??;
        Ok(Self { sender })
    }

    async fn send(&self, request: ServiceRequest) -> Result<(), String> {
        self.sender
            .send(request)
            .await
            .map_err(|_| "Local Code Mode service stopped".to_string())
    }
}

#[async_trait::async_trait]
impl CodeModeService for LocalCodeModeService {
    async fn recover(
        &self,
        execution_id: String,
        options: CodeModeRunOptions,
    ) -> Result<ExecutionState, String> {
        let (response, receiver) = oneshot::channel();
        self.send(ServiceRequest::Resume {
            execution_id,
            recover: true,
            options,
            response,
        })
        .await?;
        receive(receiver).await
    }
    async fn approve_detached(
        &self,
        execution_id: String,
        seq: u64,
        options: CodeModeRunOptions,
    ) -> Result<ExecutionState, String> {
        let (response, receiver) = oneshot::channel();
        self.send(ServiceRequest::Approve {
            execution_id,
            detached: true,
            seq,
            options,
            response,
        })
        .await?;
        receive(receiver).await
    }
    fn supports_retained_execution(&self) -> bool {
        true
    }

    async fn execute_retained(
        &self,
        code: String,
        retention_ms: u64,
        options: CodeModeRunOptions,
    ) -> Result<ExecutionState, String> {
        let (response, receiver) = oneshot::channel();
        self.send(ServiceRequest::Execute {
            code,
            retention_ms: Some(retention_ms),
            options,
            response,
        })
        .await?;
        receive(receiver).await
    }

    async fn resume(
        &self,
        execution_id: String,
        options: CodeModeRunOptions,
    ) -> Result<ExecutionState, String> {
        let (response, receiver) = oneshot::channel();
        self.send(ServiceRequest::Resume {
            execution_id,
            recover: false,
            options,
            response,
        })
        .await?;
        receive(receiver).await
    }
    async fn search(&self, query: String) -> Result<SearchOutput, String> {
        let (response, receiver) = oneshot::channel();
        self.send(ServiceRequest::Search { query, response })
            .await?;
        receive(receiver).await
    }

    async fn execute(
        &self,
        code: String,
        options: CodeModeRunOptions,
    ) -> Result<ExecutionState, String> {
        let (response, receiver) = oneshot::channel();
        self.send(ServiceRequest::Execute {
            code,
            retention_ms: None,
            options,
            response,
        })
        .await?;
        receive(receiver).await
    }

    async fn execution(&self, execution_id: String) -> Result<ExecutionState, String> {
        let (response, receiver) = oneshot::channel();
        self.send(ServiceRequest::Execution {
            execution_id,
            response,
        })
        .await?;
        receive(receiver).await
    }

    async fn artifact(&self, execution_id: String, artifact_id: String) -> Result<Value, String> {
        let (response, receiver) = oneshot::channel();
        self.send(ServiceRequest::Artifact {
            execution_id,
            artifact_id,
            response,
        })
        .await?;
        receive(receiver).await
    }

    async fn approve(
        &self,
        execution_id: String,
        seq: u64,
        options: CodeModeRunOptions,
    ) -> Result<ExecutionState, String> {
        let (response, receiver) = oneshot::channel();
        self.send(ServiceRequest::Approve {
            execution_id,
            detached: false,
            seq,
            options,
            response,
        })
        .await?;
        receive(receiver).await
    }

    async fn reject(&self, execution_id: String, seq: u64) -> Result<ExecutionState, String> {
        let (response, receiver) = oneshot::channel();
        self.send(ServiceRequest::Reject {
            execution_id,
            seq,
            response,
        })
        .await?;
        receive(receiver).await
    }

    async fn cancel(&self, execution_id: String) -> Result<ExecutionState, String> {
        let (response, receiver) = oneshot::channel();
        self.send(ServiceRequest::Cancel {
            execution_id,
            response,
        })
        .await?;
        receive(receiver).await
    }
}

async fn receive<T>(receiver: oneshot::Receiver<Result<T, String>>) -> Result<T, String> {
    receiver
        .await
        .map_err(|_| "Local Code Mode service stopped".to_string())?
}

async fn serve_requests(codemode: Rc<CodeMode>, mut receiver: mpsc::Receiver<ServiceRequest>) {
    while let Some(request) = receiver.recv().await {
        let codemode = Rc::clone(&codemode);
        tokio::task::spawn_local(async move {
            match request {
                ServiceRequest::Resume {
                    execution_id,
                    recover,
                    options,
                    response,
                } => match codemode.execution_snapshot(&execution_id).await {
                    Ok(state) if state.status == incurs_codemode::ExecutionStatus::Running => {
                        let _ = response.send(Ok(state));
                        if recover {
                            let _ = codemode.drive_if_idle(&execution_id, options).await;
                        } else {
                            let _ = codemode.drive_with(&execution_id, options).await;
                        }
                    }
                    Ok(state) if recover => {
                        let _ = response.send(Ok(state));
                    }
                    Ok(_) => {
                        let _ = response.send(Err("Only an interrupted running execution can resume; pending approvals require a decision".into()));
                    }
                    Err(error) => {
                        let _ = response.send(Err(error));
                    }
                },
                ServiceRequest::Search { query, response } => {
                    let _ = response.send(codemode.search(&query).await);
                }
                ServiceRequest::Execute {
                    code,
                    retention_ms,
                    options,
                    response,
                } => match async {
                    let id = if retention_ms.is_some() {
                        Some(random_execution_id()?)
                    } else {
                        None
                    };
                    codemode.start_retained(&code, retention_ms, id).await
                }
                .await
                {
                    Ok(state) => {
                        let execution_id = state.id.clone();
                        let _ = response.send(Ok(state));
                        let _ = codemode.drive_with(&execution_id, options).await;
                    }
                    Err(error) => {
                        let _ = response.send(Err(error));
                    }
                },
                ServiceRequest::Execution {
                    execution_id,
                    response,
                } => {
                    let _ = response.send(codemode.execution_snapshot(&execution_id).await);
                }
                ServiceRequest::Artifact {
                    execution_id,
                    artifact_id,
                    response,
                } => {
                    let _ = response.send(codemode.artifact(&execution_id, &artifact_id).await);
                }
                ServiceRequest::Approve {
                    execution_id,
                    detached,
                    seq,
                    options,
                    response,
                } => {
                    if detached {
                        match codemode.approve_pending(&execution_id, seq).await {
                            Ok(winner) => {
                                let _ =
                                    response.send(codemode.execution_snapshot(&execution_id).await);
                                if winner {
                                    let _ = codemode.drive_with(&execution_id, options).await;
                                }
                            }
                            Err(error) => {
                                let _ = response.send(Err(error));
                            }
                        }
                        return;
                    }
                    let result = match codemode.approve_with(&execution_id, seq, options).await {
                        Ok(_) => codemode.execution_snapshot(&execution_id).await,
                        Err(error) => Err(error),
                    };
                    let _ = response.send(result);
                }
                ServiceRequest::Reject {
                    execution_id,
                    seq,
                    response,
                } => {
                    let result = match codemode.reject(&execution_id, seq).await {
                        Ok(_) => codemode.execution_snapshot(&execution_id).await,
                        Err(error) => Err(error),
                    };
                    let _ = response.send(result);
                }
                ServiceRequest::Cancel {
                    execution_id,
                    response,
                } => {
                    let result = match codemode.cancel(&execution_id).await {
                        Ok(_) => codemode.execution_snapshot(&execution_id).await,
                        Err(error) => Err(error),
                    };
                    let _ = response.send(result);
                }
            }
        });
    }
}

#[cfg(all(test, feature = "quickjs"))]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use incurs::cli::Cli;
    use incurs::command::{
        CommandContext, CommandDef, CommandHandler, McpAnnotations, McpCommandOptions,
    };
    use incurs::output::CommandResult;
    use incurs::tool::ToolEvent;
    use incurs_codemode::{
        CodeMode, CodeModeRunOptions, CodeModeService, Connector, ConnectorDescription,
        ConnectorTool, ExecutionEvent, ExecutionStatus, IncurConnector, MAX_DURABLE_VALUE_BYTES,
        MemoryStore, ReplayPolicy, ToolAnnotations, ToolContext,
    };
    use serde::Deserialize;
    use serde_json::{Value, json};

    use super::*;

    #[derive(Default)]
    struct MathConnector {
        saves: AtomicUsize,
        reverts: AtomicUsize,
    }

    struct IncurSum;

    struct ContextConnector;

    struct DelayConnector;

    #[async_trait::async_trait]
    impl CommandHandler for IncurSum {
        async fn run(&self, context: CommandContext) -> CommandResult {
            CommandResult::Ok {
                data: json!({
                    "sum": context.options["left"].as_i64().unwrap()
                        + context.options["right"].as_i64().unwrap()
                }),
                cta: None,
                exit_code: None,
            }
        }
    }

    #[derive(incurs::Options, Deserialize)]
    #[allow(dead_code)]
    struct IncurSumOptions {
        /// Left operand.
        left: i64,
        /// Right operand.
        right: i64,
    }

    /// Stands in for a configured server that cannot be reached.
    ///
    /// Counts describes so a test can assert an untouched namespace is never
    /// contacted, rather than inferring it from timing.
    struct UnreachableConnector {
        describes: Arc<std::sync::atomic::AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl Connector for UnreachableConnector {
        fn name(&self) -> &str {
            "unreachable"
        }

        async fn describe(&self) -> Result<ConnectorDescription, String> {
            self.describes
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Err("connection refused".to_string())
        }

        async fn execute(
            &self,
            _method: &str,
            _arguments: Value,
            _context: &ToolContext,
        ) -> Result<Value, String> {
            Err("connection refused".to_string())
        }
    }

    #[async_trait::async_trait]
    impl Connector for MathConnector {
        fn name(&self) -> &str {
            "math"
        }

        async fn describe(&self) -> Result<ConnectorDescription, String> {
            Ok(ConnectorDescription {
                name: "math".to_string(),
                instructions: Some("Local arithmetic and mock persistence.".to_string()),
                tools: vec![
                    ConnectorTool {
                        name: "sum".to_string(),
                        description: Some("Add two integers.".to_string()),
                        input_schema: json!({
                            "type": "object",
                            "properties": {
                                "left": {"type": "integer"},
                                "right": {"type": "integer"}
                            },
                            "required": ["left", "right"]
                        }),
                        output_schema: Some(json!({
                            "type": "object",
                            "properties": {"sum": {"type": "integer"}},
                            "required": ["sum"]
                        })),
                        instructions: None,
                        examples: Vec::new(),
                        annotations: ToolAnnotations {
                            read_only: Some(true),
                            ..ToolAnnotations::default()
                        },
                        policy: incurs_codemode::ToolPolicy {
                            requires_approval: false,
                            replay: ReplayPolicy::Reexecute,
                        },
                    },
                    ConnectorTool {
                        name: "save".to_string(),
                        description: Some("Save one value.".to_string()),
                        input_schema: json!({
                            "type": "object",
                            "properties": {"value": {}},
                            "required": ["value"]
                        }),
                        output_schema: None,
                        instructions: None,
                        examples: Vec::new(),
                        annotations: ToolAnnotations::default(),
                        policy: incurs_codemode::ToolPolicy {
                            requires_approval: true,
                            replay: ReplayPolicy::Log,
                        },
                    },
                ],
            })
        }

        async fn execute(
            &self,
            method: &str,
            arguments: Value,
            _context: &ToolContext,
        ) -> Result<Value, String> {
            match method {
                "sum" => Ok(json!({
                    "sum": arguments["left"].as_i64().unwrap()
                        + arguments["right"].as_i64().unwrap()
                })),
                "save" => {
                    self.saves.fetch_add(1, Ordering::Relaxed);
                    Ok(json!({"saved": arguments["value"]}))
                }
                _ => Err(format!("Unknown method: {method}")),
            }
        }

        async fn revert(
            &self,
            method: &str,
            _arguments: Value,
            _result: Value,
            _context: &ToolContext,
        ) -> Result<bool, String> {
            if method != "save" {
                return Ok(false);
            }
            self.reverts.fetch_add(1, Ordering::Relaxed);
            Ok(true)
        }
    }

    #[async_trait::async_trait]
    impl Connector for ContextConnector {
        fn name(&self) -> &str {
            "context"
        }

        async fn describe(&self) -> Result<ConnectorDescription, String> {
            Ok(ConnectorDescription {
                name: "context".to_string(),
                instructions: None,
                tools: vec![ConnectorTool {
                    name: "inspect".to_string(),
                    description: None,
                    input_schema: json!({"type": "object"}),
                    output_schema: None,
                    instructions: None,
                    examples: Vec::new(),
                    annotations: ToolAnnotations {
                        read_only: Some(true),
                        ..ToolAnnotations::default()
                    },
                    policy: incurs_codemode::ToolPolicy {
                        requires_approval: false,
                        replay: ReplayPolicy::Reexecute,
                    },
                }],
            })
        }

        async fn execute(
            &self,
            _method: &str,
            _arguments: Value,
            context: &ToolContext,
        ) -> Result<Value, String> {
            let events = context
                .control
                .events
                .as_ref()
                .expect("execution event sink");
            events
                .emit(ToolEvent::Progress {
                    message: "halfway".to_string(),
                    fraction: Some(0.5),
                })
                .await;
            events
                .emit(ToolEvent::Log {
                    level: "info".to_string(),
                    message: "inspected".to_string(),
                })
                .await;
            events
                .emit(ToolEvent::Chunk {
                    data: json!({"part": 1}),
                })
                .await;
            Ok(json!({
                "method": context.request.as_ref().map(|request| request.method.clone()),
                "path": context.request.as_ref().map(|request| request.path.clone()),
            }))
        }
    }

    #[async_trait::async_trait]
    impl Connector for DelayConnector {
        fn name(&self) -> &str {
            "delay"
        }

        async fn describe(&self) -> Result<ConnectorDescription, String> {
            Ok(ConnectorDescription {
                name: "delay".to_string(),
                instructions: None,
                tools: vec![ConnectorTool {
                    name: "sleep".to_string(),
                    description: None,
                    input_schema: json!({
                        "type": "object",
                        "properties": {"ms": {"type": "integer"}},
                        "required": ["ms"]
                    }),
                    output_schema: None,
                    instructions: None,
                    examples: Vec::new(),
                    annotations: ToolAnnotations {
                        read_only: Some(true),
                        ..ToolAnnotations::default()
                    },
                    policy: incurs_codemode::ToolPolicy {
                        requires_approval: false,
                        replay: ReplayPolicy::Reexecute,
                    },
                }],
            })
        }

        async fn execute(
            &self,
            _method: &str,
            arguments: Value,
            _context: &ToolContext,
        ) -> Result<Value, String> {
            let ms = arguments["ms"].as_u64().unwrap();
            tokio::time::sleep(Duration::from_millis(ms)).await;
            Ok(json!({"slept": ms}))
        }
    }

    struct WaitingConnector;

    #[tokio::test]
    async fn concurrent_executions_keep_distinct_native_heaps() {
        let mode = CodeMode::new(
            Arc::new(MemoryStore::default()),
            LocalExecutor::default(),
            vec![Arc::new(DelayConnector)],
        );
        let first = mode.execute(
            "async () => { const token = 'first'; await delay.sleep({ms: 20}); return token; }",
        );
        let second = mode.execute(
            "async () => { const token = 'second'; await delay.sleep({ms: 1}); return token; }",
        );
        let (first, second) = tokio::join!(first, second);
        assert_eq!(first.unwrap().result, Some(json!("first")));
        assert_eq!(second.unwrap().result, Some(json!("second")));
    }

    #[async_trait::async_trait]
    impl Connector for WaitingConnector {
        fn name(&self) -> &str {
            "waiting"
        }

        async fn describe(&self) -> Result<ConnectorDescription, String> {
            Ok(ConnectorDescription {
                name: "waiting".to_string(),
                instructions: None,
                tools: vec![ConnectorTool {
                    name: "until_cancelled".to_string(),
                    description: None,
                    input_schema: json!({"type": "object"}),
                    output_schema: None,
                    instructions: None,
                    examples: Vec::new(),
                    annotations: ToolAnnotations {
                        read_only: Some(true),
                        ..ToolAnnotations::default()
                    },
                    policy: incurs_codemode::ToolPolicy {
                        requires_approval: false,
                        replay: ReplayPolicy::Reexecute,
                    },
                }],
            })
        }

        async fn execute(
            &self,
            _method: &str,
            _arguments: Value,
            context: &ToolContext,
        ) -> Result<Value, String> {
            context.control.cancellation.cancelled().await;
            Err("cancelled".to_string())
        }
    }

    #[tokio::test]
    async fn executes_programs_locally() {
        let connector = Arc::new(MathConnector::default());
        let codemode = CodeMode::new(
            Arc::new(MemoryStore::default()),
            LocalExecutor::default(),
            vec![connector],
        );
        let execution = codemode
            .execute(
                "const found = await codemode.search(\"sum\"); \
                 const values = await Promise.all([ \
                   math.sum({ left: 2, right: 3 }), \
                   math.sum({ left: 4, right: 5 }) \
                 ]); \
                 return { found: found.results[0].path, values };",
            )
            .await
            .unwrap();

        assert_eq!(
            execution.status,
            ExecutionStatus::Completed,
            "{execution:?}"
        );
        assert_eq!(
            execution.result,
            Some(json!({
                "found": "math.sum",
                "values": [{"sum": 5}, {"sum": 9}]
            }))
        );
        let capabilities = execution
            .capabilities
            .expect("capabilities must be captured");
        assert_eq!(capabilities.connectors[0].name, "math");
        assert_eq!(capabilities.fingerprint.len(), 64);
    }

    #[tokio::test]
    async fn executes_an_incur_catalog_locally() {
        let catalog = Cli::create("math")
            .command(
                "sum",
                CommandDef::build("sum", IncurSum)
                    .options::<IncurSumOptions>()
                    .mcp(McpCommandOptions {
                        annotations: Some(McpAnnotations {
                            read_only_hint: Some(true),
                            ..McpAnnotations::default()
                        }),
                        ..McpCommandOptions::default()
                    })
                    .done(),
            )
            .tool_catalog();
        let codemode = CodeMode::new(
            Arc::new(MemoryStore::default()),
            LocalExecutor::default(),
            vec![Arc::new(IncurConnector::new(catalog))],
        );

        let execution = codemode
            .execute("math.sum({ left: 20, right: 22 })")
            .await
            .unwrap();

        assert_eq!(execution.status, ExecutionStatus::Completed);
        assert_eq!(execution.result, Some(json!({"sum": 42})));
    }

    #[tokio::test]
    async fn pauses_replays_and_rolls_back_locally() {
        let connector = Arc::new(MathConnector::default());
        let codemode = CodeMode::new(
            Arc::new(MemoryStore::default()),
            LocalExecutor::default(),
            vec![connector.clone()],
        );
        let paused = codemode
            .execute(
                "const token = await codemode.step(\"token\", async () => \"stable\"); \
                 await math.save({ value: token }); \
                 return token;",
            )
            .await
            .unwrap();
        assert_eq!(paused.status, ExecutionStatus::Paused, "{paused:?}");

        let completed = codemode.approve(&paused.id, 1).await.unwrap();
        assert_eq!(completed.status, ExecutionStatus::Completed);
        assert_eq!(completed.result, Some(json!("stable")));
        assert_eq!(connector.saves.load(Ordering::Relaxed), 1);

        let rolled_back = codemode.rollback(&paused.id).await.unwrap();
        assert_eq!(rolled_back.status, ExecutionStatus::RolledBack);
        assert_eq!(connector.reverts.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn interrupts_runaway_local_programs() {
        let codemode = CodeMode::new(
            Arc::new(MemoryStore::default()),
            LocalExecutor::new(LocalExecutorOptions {
                timeout: Duration::from_millis(10),
                ..LocalExecutorOptions::default()
            }),
            Vec::new(),
        );
        let execution = codemode.execute("while (true) {}").await.unwrap();

        assert_eq!(execution.status, ExecutionStatus::Error);
        assert_eq!(
            execution.error.as_deref(),
            Some(
                "CODE_EXECUTION_TIMEOUT: maximum uninterrupted JavaScript execution interval exceeded"
            )
        );
    }

    #[tokio::test]
    async fn host_calls_renew_local_execution_timeout() {
        let codemode = CodeMode::new(
            Arc::new(MemoryStore::default()),
            LocalExecutor::new(LocalExecutorOptions {
                timeout: Duration::from_millis(25),
                ..LocalExecutorOptions::default()
            }),
            vec![Arc::new(DelayConnector)],
        );
        let execution = codemode
            .execute(
                "const result = await delay.sleep({ ms: 75 }); \
                 let total = 0; \
                 for (let i = 0; i < 10_000; i++) total += i; \
                 return { result, total };",
            )
            .await
            .unwrap();

        assert_eq!(
            execution.status,
            ExecutionStatus::Completed,
            "{execution:?}"
        );
        assert_eq!(
            execution.result,
            Some(json!({
                "result": {"slept": 75},
                "total": 49_995_000
            }))
        );
    }

    #[tokio::test]
    async fn propagates_request_control_and_events() {
        let codemode = CodeMode::new(
            Arc::new(MemoryStore::default()),
            LocalExecutor::default(),
            vec![Arc::new(ContextConnector)],
        );
        let execution = codemode
            .execute_with(
                "context.inspect({})",
                CodeModeRunOptions {
                    request: Some(incurs::command::RequestContext {
                        elicitation: None,
                        headers: Default::default(),
                        method: "POST".to_string(),
                        path: "/codemode".to_string(),
                    }),
                    ..CodeModeRunOptions::default()
                },
            )
            .await
            .unwrap();

        assert_eq!(
            execution.result,
            Some(json!({"method": "POST", "path": "/codemode"}))
        );
        assert!(execution.events.iter().any(|event| matches!(
            event,
            ExecutionEvent::Progress {
                message,
                fraction: Some(0.5),
                ..
            } if message == "halfway"
        )));
        assert!(execution.events.iter().any(|event| matches!(
            event,
            ExecutionEvent::Log { message, .. } if message == "inspected"
        )));
        assert!(execution.events.iter().any(|event| matches!(
            event,
            ExecutionEvent::Chunk { data, .. } if data == &json!({"part": 1})
        )));
    }

    #[tokio::test]
    async fn service_returns_running_state_and_cancels_active_quickjs() {
        let service = LocalCodeModeService::spawn(|| {
            CodeMode::new(
                Arc::new(MemoryStore::default()),
                LocalExecutor::default(),
                vec![Arc::new(WaitingConnector)],
            )
        })
        .unwrap();
        let running = service
            .execute(
                "waiting.until_cancelled({})".to_string(),
                CodeModeRunOptions::default(),
            )
            .await
            .unwrap();

        assert_eq!(running.status, ExecutionStatus::Running);
        let cancelled = service.cancel(running.id.clone()).await.unwrap();
        assert_eq!(cancelled.status, ExecutionStatus::Cancelled);
    }

    #[tokio::test]
    async fn service_exposes_artifact_references_for_oversized_results() {
        let service = LocalCodeModeService::spawn(|| {
            CodeMode::new(
                Arc::new(MemoryStore::default()),
                LocalExecutor::default(),
                vec![],
            )
        })
        .unwrap();
        let running = service
            .execute(
                format!("\"x\".repeat({})", MAX_DURABLE_VALUE_BYTES + 1),
                CodeModeRunOptions::default(),
            )
            .await
            .unwrap();
        let execution = loop {
            let execution = service.execution(running.id.clone()).await.unwrap();
            if execution.status == ExecutionStatus::Completed {
                break execution;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        };
        let artifact_id = execution.result.as_ref().unwrap()["$artifact"]["id"]
            .as_str()
            .unwrap();
        assert_eq!(
            service
                .artifact(execution.id.clone(), artifact_id.to_string())
                .await
                .unwrap(),
            json!("x".repeat(MAX_DURABLE_VALUE_BYTES + 1))
        );
    }

    #[tokio::test]
    async fn an_unreachable_namespace_does_not_break_a_program_that_ignores_it() {
        // The whole point of binding lazily: a configured server that is dead must
        // cost nothing until something uses it. Describing every connector up front
        // meant one unreachable server failed every execution, including the ones
        // that never mentioned it -- an unactionable failure, because no program the
        // agent could write would avoid it.
        let describes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let codemode = CodeMode::new(
            Arc::new(MemoryStore::default()),
            LocalExecutor::default(),
            vec![
                Arc::new(MathConnector::default()),
                Arc::new(UnreachableConnector {
                    describes: Arc::clone(&describes),
                }),
            ],
        );

        let execution = codemode
            .execute("return (await math.sum({ left: 2, right: 3 })).sum")
            .await
            .unwrap();

        assert_eq!(
            execution.status,
            ExecutionStatus::Completed,
            "a dead namespace the program never names must not fail it: {execution:?}"
        );
        assert_eq!(execution.result, Some(json!(5)));
        assert_eq!(
            describes.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "an untouched namespace must never be contacted"
        );
    }

    #[tokio::test]
    async fn using_an_unreachable_namespace_fails_with_its_own_error() {
        // Fail fast, scoped: the failure surfaces when the program reaches for it,
        // carrying the reason, so the agent can route around it.
        let describes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let codemode = CodeMode::new(
            Arc::new(MemoryStore::default()),
            LocalExecutor::default(),
            vec![
                Arc::new(MathConnector::default()),
                Arc::new(UnreachableConnector {
                    describes: Arc::clone(&describes),
                }),
            ],
        );

        let execution = codemode
            .execute("return await unreachable.anything({})")
            .await
            .unwrap();

        assert_eq!(execution.status, ExecutionStatus::Error, "{execution:?}");
        assert!(
            execution
                .error
                .as_deref()
                .is_some_and(|error| error.contains("connection refused")),
            "the error must say why: {:?}",
            execution.error
        );
        assert_eq!(
            describes.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "a used namespace is described exactly once per pass"
        );
    }
    #[tokio::test]
    async fn circular_values_fail_without_rejecting_shared_references() {
        let codemode = CodeMode::new(
            Arc::new(MemoryStore::default()),
            LocalExecutor::default(),
            vec![Arc::new(ContextConnector)],
        );
        for code in [
            "const value = {}; value.self = value; return value;",
            "const value = []; value.push(value); return value;",
            "const value = {}; value.self = value; return await context.inspect(value);",
            "return await codemode.step('cycle', async () => { const value = {}; value.self = value; return value; });",
        ] {
            let execution = codemode.execute(code).await.unwrap();
            assert_eq!(execution.status, ExecutionStatus::Error, "{execution:?}");
            let error = execution.error.as_deref().expect("recorded cycle error");
            assert!(
                error.contains("CODE_VALUE_CYCLE"),
                "cycle must have a specific diagnostic: {error}"
            );
        }
        let execution = codemode
            .execute("const shared = {answer: 42}; return {left: shared, right: shared};")
            .await
            .unwrap();
        assert_eq!(execution.status, ExecutionStatus::Completed);
        assert_eq!(
            execution.result,
            Some(json!({"left": {"answer": 42}, "right": {"answer": 42}}))
        );
    }

    #[tokio::test]
    async fn caught_cycles_preserve_recovery_and_unrecorded_step_outcomes() {
        let codemode = CodeMode::new(
            Arc::new(MemoryStore::default()),
            LocalExecutor::default(),
            vec![Arc::new(MathConnector::default())],
        );
        let execution = codemode
            .execute(
                "const value = {}; value.self = value; const errors = []; \
                 try { await math.sum(value); } catch (error) { errors.push(String(error)); } \
                 try { await codemode.step('unrecorded', async () => value); } \
                 catch (error) { errors.push(String(error)); } \
                 const answer = await codemode.step('valid', async () => \
                   (await math.sum({left: 20, right: 22})).sum); \
                 return {errors, answer};",
            )
            .await
            .unwrap();
        assert_eq!(
            execution.status,
            ExecutionStatus::Completed,
            "{execution:?}"
        );
        let result = execution.result.as_ref().unwrap();
        assert_eq!(result["answer"], json!(42));
        let errors = result["errors"].as_array().unwrap();
        assert_eq!(errors.len(), 2);
        assert!(errors.iter().all(|error| {
            error
                .as_str()
                .is_some_and(|message| message.contains("CODE_VALUE_CYCLE"))
        }));
        assert_eq!(execution.log.len(), 3);
        let unrecorded = &execution.log[0];
        assert_eq!(unrecorded.seq, 1);
        assert_eq!(unrecorded.connector, "__step");
        assert_eq!(unrecorded.state, incurs_codemode::LogEntryState::Executing);
        assert!(unrecorded.result.is_none());
        let recorded = &execution.log[1];
        assert_eq!(recorded.seq, 2);
        assert_eq!(recorded.state, incurs_codemode::LogEntryState::Applied);
        assert_eq!(recorded.result, Some(json!(42)));
        let error = codemode.rollback(&execution.id).await.unwrap_err();
        assert!(error.contains("unfinished"), "{error}");
    }

    #[tokio::test]
    async fn javascript_errors_preserve_exception_details() {
        let codemode = CodeMode::new(
            Arc::new(MemoryStore::default()),
            LocalExecutor::default(),
            Vec::new(),
        );
        for (code, expected) in [
            (
                "const value = \"first\nsecond\"; return value;",
                "SyntaxError",
            ),
            (
                "throw new TypeError('diagnostic-sentinel');",
                "diagnostic-sentinel",
            ),
            (
                "await Promise.reject(new Error('rejection-sentinel'));",
                "rejection-sentinel",
            ),
        ] {
            let execution = codemode.execute(code).await.unwrap();
            assert_eq!(execution.status, ExecutionStatus::Error);
            let error = execution.error.as_deref().expect("recorded exception");
            assert!(error.contains(expected), "{expected}: {error}");
            if expected == "SyntaxError" {
                assert!(
                    error.contains("codemode.js"),
                    "missing source location: {error}"
                );
            }
        }
    }

    #[tokio::test]
    async fn javascript_primitive_exceptions_preserve_the_thrown_value() {
        let codemode = CodeMode::new(
            Arc::new(MemoryStore::default()),
            LocalExecutor::default(),
            Vec::new(),
        );
        let execution = codemode.execute("throw 42;").await.unwrap();
        assert_eq!(execution.status, ExecutionStatus::Error);
        assert!(
            execution
                .error
                .as_deref()
                .is_some_and(|error| error.contains("42")),
            "{execution:?}"
        );
    }

    #[tokio::test]
    async fn actor_stack_headroom_preserves_service_after_javascript_overflow() {
        let service = LocalCodeModeService::spawn_with_stack_size(8 * 1024 * 1024, || {
            CodeMode::new(
                Arc::new(MemoryStore::default()),
                LocalExecutor::new(LocalExecutorOptions {
                    stack_size: 2 * 1024 * 1024,
                    ..LocalExecutorOptions::default()
                }),
                vec![],
            )
        })
        .unwrap();
        for (code, expected_status, expected_error) in [
            (
                "function recurse() { return recurse() + 1; } return recurse();",
                ExecutionStatus::Error,
                Some("stack overflow"),
            ),
            ("return 42;", ExecutionStatus::Completed, None),
        ] {
            let running = service
                .execute(code.to_string(), CodeModeRunOptions::default())
                .await
                .unwrap();
            let execution = tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    let execution = service.execution(running.id.clone()).await.unwrap();
                    if execution.status != ExecutionStatus::Running {
                        break execution;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("actor must remain responsive");
            assert_eq!(execution.status, expected_status, "{execution:?}");
            if let Some(expected) = expected_error {
                assert!(
                    execution
                        .error
                        .as_deref()
                        .is_some_and(|error| error.contains(expected)),
                    "{execution:?}"
                );
            } else {
                assert_eq!(execution.result, Some(json!(42)));
            }
        }
    }
}
