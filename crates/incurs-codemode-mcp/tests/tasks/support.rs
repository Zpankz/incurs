use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

use incurs_codemode::{
    CodeMode, CodeModeService, Connector, ConnectorDescription, ConnectorTool, MemoryStore,
    ReplayPolicy, ToolAnnotations, ToolContext, ToolPolicy,
};
use incurs_codemode_local::{LocalCodeModeService, LocalExecutor};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

type Reader = BufReader<tokio::io::ReadHalf<tokio::io::DuplexStream>>;
pub(super) struct Wire {
    reader: Reader,
    writer: tokio::io::WriteHalf<tokio::io::DuplexStream>,
    server: tokio::task::JoinHandle<()>,
    modern: bool,
    pub(super) seq: u64,
}

pub(super) fn meta() -> Value {
    json!({
        "io.modelcontextprotocol/protocolVersion":"2026-07-28",
        "io.modelcontextprotocol/clientInfo":{"name":"literal-client","version":"1"},
        "io.modelcontextprotocol/clientCapabilities":{
            "extensions":{"io.modelcontextprotocol/tasks":{}},"elicitation":{"form":{}}
        }
    })
}

impl Drop for Wire {
    fn drop(&mut self) {
        self.server.abort();
    }
}
impl Wire {
    pub(super) async fn connect(modern: bool, service: Arc<dyn CodeModeService>) -> Self {
        let (transport, client) = tokio::io::duplex(256 * 1024);
        let server = tokio::spawn(async move {
            let (input, output) = tokio::io::split(transport);
            incurs_codemode_mcp::serve_stream(service, input, output)
                .await
                .unwrap();
        });
        let (reader, writer) = tokio::io::split(client);
        let mut wire = Self {
            reader: BufReader::new(reader),
            writer,
            server,
            modern,
            seq: 0,
        };
        if !modern {
            let response = wire.rpc("initialize", json!({"protocolVersion":"2025-11-25", "clientInfo":{"name":"literal-client","version":"1"},"capabilities":{"elicitation":{"form":{}},"tasks":{}}})).await;
            assert_eq!(
                response["result"]["capabilities"]["tasks"]["requests"]["tools"]["call"],
                json!({}),
                "{response}"
            );
            wire.write(json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
                .await;
            for (method, field) in [
                ("resources/list", "resources"),
                ("prompts/list", "prompts"),
                ("resources/templates/list", "resourceTemplates"),
            ] {
                let response = wire.rpc(method, json!({})).await;
                assert_eq!(response["result"][field], json!([]), "{response}");
            }
        }
        wire
    }
    pub(super) async fn write(&mut self, value: Value) {
        self.writer
            .write_all(format!("{value}\n").as_bytes())
            .await
            .unwrap();
    }
    pub(super) async fn read(&mut self) -> Value {
        let mut line = String::new();
        let count = tokio::time::timeout(Duration::from_secs(5), self.reader.read_line(&mut line))
            .await
            .expect("MCP response timed out")
            .unwrap();
        assert!(count > 0, "MCP disconnected");
        serde_json::from_str(&line).unwrap()
    }
    pub(super) async fn send(&mut self, method: &str, mut params: Value) {
        self.seq += 1;
        if self.modern {
            params["_meta"] = meta();
        }
        self.write(json!({"jsonrpc":"2.0","id":self.seq,"method":method,"params":params}))
            .await;
    }
    pub(super) async fn rpc(&mut self, method: &str, params: Value) -> Value {
        self.send(method, params).await;
        let response = self.read().await;
        assert_eq!(response["id"], self.seq, "{response}");
        response
    }
    pub(super) async fn start(&mut self, code: &str) -> String {
        let mut params = json!({"name":"codemode_execute","arguments":{"code":code}});
        if !self.modern {
            params["task"] = json!({"ttl":60_000});
        }
        let response = self.rpc("tools/call", params).await;
        let task = if self.modern {
            assert_eq!(response["result"]["resultType"], "task", "{response}");
            &response["result"]
        } else {
            &response["result"]["task"]
        };
        assert_eq!(task["status"], "working");
        assert!(task["createdAt"].as_str().unwrap().contains('T'));
        let ttl = if self.modern { "ttlMs" } else { "ttl" };
        assert_eq!(task[ttl], 86_400_000);
        assert_eq!(
            task[if self.modern {
                "pollIntervalMs"
            } else {
                "pollInterval"
            }],
            1_000
        );
        task["taskId"].as_str().expect("task ID").to_owned()
    }
    pub(super) async fn poll(&mut self, id: &str, status: &str) -> Value {
        for _ in 0..200 {
            let response = self.rpc("tasks/get", json!({"taskId":id})).await;
            assert!(response.get("error").is_none(), "{response}");
            if response["result"]["status"] == status {
                return response["result"].clone();
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("task did not reach {status}");
    }
    pub(super) async fn answer_ok(&mut self, id: &str, key: &str, response: Value) {
        let result = self.answer(id, key, response).await;
        assert!(result.get("error").is_none(), "{result}");
    }
    pub(super) async fn answer(&mut self, id: &str, key: &str, response: Value) -> Value {
        self.rpc(
            "tasks/update",
            json!({"taskId":id,"inputResponses":{key:response}}),
        )
        .await
    }
}

pub(super) fn service(connectors: Vec<Arc<dyn Connector>>) -> Arc<dyn CodeModeService> {
    Arc::new(
        LocalCodeModeService::spawn(move || {
            CodeMode::new(
                Arc::new(MemoryStore::default()),
                LocalExecutor::default(),
                connectors,
            )
        })
        .unwrap(),
    )
}

pub(super) struct Effects {
    pub(super) count: Arc<AtomicUsize>,
    pub(super) file: Option<PathBuf>,
}
#[async_trait::async_trait]
impl Connector for Effects {
    fn name(&self) -> &str {
        "effects"
    }
    async fn describe(&self) -> Result<ConnectorDescription, String> {
        Ok(ConnectorDescription {
            name: "effects".into(),
            instructions: None,
            tools: vec![ConnectorTool {
                name: "save".into(),
                description: None,
                input_schema: json!({"type":"object"}),
                output_schema: None,
                instructions: None,
                examples: vec![],
                annotations: ToolAnnotations::default(),
                policy: ToolPolicy {
                    requires_approval: true,
                    replay: ReplayPolicy::Log,
                },
            }],
        })
    }
    async fn execute(
        &self,
        _: &str,
        mut arguments: Value,
        context: &ToolContext,
    ) -> Result<Value, String> {
        self.count.fetch_add(1, Ordering::SeqCst);
        if let Some(path) = &self.file {
            arguments["request_path"] =
                json!(context.request.as_ref().map(|request| request.path.clone()));
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .map_err(|error| error.to_string())?;
            writeln!(file, "{arguments}").map_err(|error| error.to_string())?;
            file.sync_all().map_err(|error| error.to_string())?;
        }
        Ok(json!(42))
    }
}
