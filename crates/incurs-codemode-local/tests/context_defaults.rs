use std::sync::{Arc, Mutex};

use incurs_codemode::{
    CodeMode, Connector, ConnectorDescription, ConnectorTool, ExecutionStatus, MemoryStore,
    ToolAnnotations, ToolContext, ToolPolicy,
};
use incurs_codemode_local::LocalExecutor;
use serde_json::{Value, json};

#[derive(Clone)]
struct PreparingConnector {
    executed_arguments: Arc<Mutex<Vec<Value>>>,
}

impl PreparingConnector {
    fn new() -> Self {
        Self {
            executed_arguments: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn executed_arguments(&self) -> Vec<Value> {
        self.executed_arguments.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl Connector for PreparingConnector {
    fn name(&self) -> &str {
        "defaults"
    }

    async fn describe(&self) -> Result<ConnectorDescription, String> {
        Ok(description("defaults"))
    }

    async fn prepare_arguments(
        &self,
        seq: u64,
        tool: &ConnectorTool,
        arguments: Value,
        context: &ToolContext,
    ) -> Result<Value, String> {
        assert_eq!(seq, 0);
        assert_eq!(tool.name, "echo");
        assert_eq!(arguments, caller_arguments());
        Ok(json!({
            "caller": arguments,
            "prepared_by": "fixture-prepare",
            "seq": seq,
            "execution_id": context.execution_id,
        }))
    }

    async fn execute(
        &self,
        method: &str,
        arguments: Value,
        _context: &ToolContext,
    ) -> Result<Value, String> {
        assert_eq!(method, "echo");
        self.executed_arguments
            .lock()
            .unwrap()
            .push(arguments.clone());
        Ok(json!({ "observed": arguments }))
    }
}

#[derive(Clone)]
struct DefaultConnector;

#[async_trait::async_trait]
impl Connector for DefaultConnector {
    fn name(&self) -> &str {
        "plain"
    }

    async fn describe(&self) -> Result<ConnectorDescription, String> {
        Ok(description("plain"))
    }

    async fn execute(
        &self,
        method: &str,
        arguments: Value,
        _context: &ToolContext,
    ) -> Result<Value, String> {
        assert_eq!(method, "echo");
        Ok(json!({ "observed": arguments }))
    }
}

#[tokio::test]
async fn prepared_arguments_reach_execute_and_the_durable_log() {
    let connector = PreparingConnector::new();
    let codemode = CodeMode::new(
        Arc::new(MemoryStore::default()),
        LocalExecutor::default(),
        vec![Arc::new(connector.clone())],
    );

    let execution = codemode
        .execute("return await defaults.echo({ caller: 'literal', nested: { left: 7 }, list: [true, null] });")
        .await
        .unwrap();

    assert_eq!(execution.status, ExecutionStatus::Completed);
    let execution_id = execution.id.clone();
    let prepared = json!({
        "caller": caller_arguments(),
        "prepared_by": "fixture-prepare",
        "seq": 0,
        "execution_id": execution_id,
    });
    assert_eq!(execution.result, Some(json!({ "observed": prepared })));
    assert_eq!(connector.executed_arguments(), vec![prepared.clone()]);
    assert_eq!(execution.log.len(), 1);
    assert_eq!(execution.log[0].arguments, prepared);
}

#[tokio::test]
async fn default_prepare_arguments_preserves_the_exact_caller_json() {
    let codemode = CodeMode::new(
        Arc::new(MemoryStore::default()),
        LocalExecutor::default(),
        vec![Arc::new(DefaultConnector)],
    );

    let execution = codemode
        .execute("return await plain.echo({ caller: 'literal', nested: { left: 7 }, list: [true, null] });")
        .await
        .unwrap();

    assert_eq!(execution.status, ExecutionStatus::Completed);
    assert_eq!(
        execution.result,
        Some(json!({ "observed": caller_arguments() }))
    );
    assert_eq!(execution.log.len(), 1);
    assert_eq!(execution.log[0].arguments, caller_arguments());
}

fn description(name: &str) -> ConnectorDescription {
    ConnectorDescription {
        name: name.to_string(),
        instructions: None,
        tools: vec![ConnectorTool {
            name: "echo".to_string(),
            description: Some("Returns the arguments it observes.".to_string()),
            input_schema: json!({ "type": "object" }),
            output_schema: Some(json!({ "type": "object" })),
            instructions: None,
            examples: Vec::new(),
            annotations: ToolAnnotations {
                read_only: Some(true),
                destructive: Some(false),
                idempotent: Some(true),
                open_world: Some(false),
            },
            policy: ToolPolicy::default(),
        }],
    }
}

fn caller_arguments() -> Value {
    json!({
        "caller": "literal",
        "nested": { "left": 7 },
        "list": [true, null],
    })
}
