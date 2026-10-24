use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use incurs_codemode::{
    ArtifactStore, CodeMode, Connector, ConnectorDescription, ConnectorTool, ExecutionStatus,
    ReplayPolicy, RuntimeStore, Snippet, ToolAnnotations, ToolContext, ToolPolicy,
};
use incurs_codemode_local::{
    DurableStore, LocalExecutor as DurableExecutor, LocalExecutorOptions as DurableExecutorOptions,
};
use incurs_codemode_mcp::CodeModeMcpServer;
use rmcp::{
    ServiceExt,
    model::{CallToolRequestParams, ClientInfo},
};
use serde_json::{Value, json};

struct Effects(PathBuf);

#[async_trait::async_trait]
impl Connector for Effects {
    fn name(&self) -> &str {
        "effects"
    }

    async fn describe(&self) -> Result<ConnectorDescription, String> {
        Ok(ConnectorDescription {
            name: self.name().into(),
            instructions: None,
            tools: ["observe", "save", "block"]
                .into_iter()
                .map(|name| ConnectorTool {
                    name: name.into(),
                    description: None,
                    input_schema: json!({"type":"object"}),
                    output_schema: None,
                    instructions: None,
                    examples: Vec::new(),
                    annotations: ToolAnnotations::default(),
                    policy: ToolPolicy {
                        requires_approval: name == "save",
                        replay: ReplayPolicy::Log,
                    },
                })
                .collect(),
        })
    }

    async fn execute(
        &self,
        method: &str,
        arguments: Value,
        _: &ToolContext,
    ) -> Result<Value, String> {
        if method == "block" && std::env::var("INCURS_DQUICKTS_STAGE").as_deref() == Ok("crash") {
            fs::write(self.0.with_file_name("ready"), b"waiting on host")
                .map_err(|e| e.to_string())?;
            std::future::pending::<()>().await;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.0)
            .map_err(|e| e.to_string())?;
        writeln!(file, "{}", json!({"method":method,"arguments":arguments}))
            .map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        Ok(json!({"accepted":true}))
    }
}

const PROGRAM: &str = r#"async () => {
    const stamp = Math.random().toString();
    const remember = () => stamp;
    const counts = new Map([['total', 0]]);
    const bytes = new Uint8Array([7, 8, 255]);
    console.log('prelude', stamp);
    await effects.observe({stamp});
    for (let index = 0; index < 2; index++) {
        await effects.save({stamp, index});
        counts.set('total', counts.get('total') + index + 1);
    }
    return {stamp, remembered: remember(), total: counts.get('total'), bytes,
            large: 'z'.repeat(1000100)};
}"#;

const CRASH_PROGRAM: &str = r#"async () => {
    const stamp = Math.random().toString();
    await effects.observe({stamp});
    await effects.block({stamp});
    return {stamp};
}"#;

fn mode(store: Arc<DurableStore>, directory: &Path, timeout: Duration) -> CodeMode {
    CodeMode::with_artifact_store(
        store.clone(),
        store.clone(),
        DurableExecutor::new(DurableExecutorOptions {
            timeout,
            ..Default::default()
        }),
        vec![Arc::new(Effects(directory.join("effects.jsonl")))],
    )
}

fn stage(directory: &Path, stage: &str) {
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "child", "--nocapture"])
        .env("INCURS_DQUICKTS_STAGE", stage)
        .env("INCURS_DQUICKTS_DIR", directory)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stage {stage}: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn r1_r2_r3_restart_retains_heap_approval_and_artifacts() {
    let directory = tempfile::tempdir().unwrap();
    stage(directory.path(), "start");
    let first = fs::read_to_string(directory.path().join("effects.jsonl")).unwrap();
    assert_eq!(
        first.lines().count(),
        1,
        "approval must prevent the first write"
    );
    stage(directory.path(), "approve-first");
    stage(directory.path(), "approve-second");
    stage(directory.path(), "verify");
    let effects: Vec<Value> = fs::read_to_string(directory.path().join("effects.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(effects.len(), 3);
    assert_eq!(effects[0]["method"], "observe");
    assert_eq!(effects[1]["arguments"]["index"], 0);
    assert_eq!(effects[2]["arguments"]["index"], 1);
    for effect in &effects[1..] {
        assert_eq!(
            effect["arguments"]["stamp"],
            effects[0]["arguments"]["stamp"]
        );
    }
}

#[test]
fn r7_mcp_approval_survives_server_process_restart() {
    let directory = tempfile::tempdir().unwrap();
    stage(directory.path(), "mcp-start");
    stage(directory.path(), "mcp-approve");
    let effects: Vec<Value> = fs::read_to_string(directory.path().join("effects.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(effects.len(), 2);
    assert_eq!(effects[0]["method"], "observe");
    assert_eq!(effects[1]["method"], "save");
    assert_eq!(
        effects[1]["arguments"]["stamp"],
        effects[0]["arguments"]["stamp"]
    );
}

type McpClient = rmcp::service::RunningService<rmcp::RoleClient, ClientInfo>;

async fn mcp_call(client: &McpClient, name: &'static str, arguments: Value) -> Value {
    let response = client
        .call_tool(
            CallToolRequestParams::new(name).with_arguments(arguments.as_object().unwrap().clone()),
        )
        .await
        .unwrap();
    assert_ne!(response.is_error, Some(true), "{response:?}");
    response.structured_content.unwrap()
}

async fn mcp_stage(directory: &Path, stage: &str) {
    let service = incurs_codemode_local::spawn_codemode(
        directory.join("runtime.sqlite"),
        Default::default(),
        vec![Arc::new(Effects(directory.join("effects.jsonl")))],
    )
    .unwrap();
    let (server_transport, client_transport) = tokio::io::duplex(1024 * 1024);
    let task = tokio::spawn(async move {
        CodeModeMcpServer::new(Arc::new(service))
            .serve(server_transport)
            .await
            .unwrap()
            .waiting()
            .await
            .unwrap();
    });
    let client = ClientInfo::default().serve(client_transport).await.unwrap();
    let listed = client.list_tools(None).await.unwrap();
    assert_eq!(listed.tools.len(), 5);
    let (id, expected) = if stage == "mcp-start" {
        let state = mcp_call(&client, "codemode_execute", json!({"code": "async () => { const stamp = Math.random().toString(); await effects.observe({stamp}); await effects.save({stamp}); return {stamp}; }"})).await;
        let id = state["id"].as_str().unwrap().to_owned();
        fs::write(directory.join("id"), &id).unwrap();
        (id, "paused")
    } else if stage == "mcp-recover" {
        let id = fs::read_to_string(directory.join("id")).unwrap();
        let state = mcp_call(&client, "codemode_execution", json!({"id":id})).await;
        assert_eq!(state["status"], "running");
        mcp_call(
            &client,
            "codemode_execution",
            json!({"id":id,"resume":true}),
        )
        .await;
        (id, "completed")
    } else {
        let id = fs::read_to_string(directory.join("id")).unwrap();
        let state = mcp_call(&client, "codemode_execution", json!({"id":id})).await;
        assert_eq!(state["status"], "paused");
        let state = mcp_call(
            &client,
            "codemode_decide",
            json!({"id":id,"seq":1,"decision":"approve"}),
        )
        .await;
        assert_eq!(state["status"], "completed", "{state}");
        let first: Value = serde_json::from_str(
            fs::read_to_string(directory.join("effects.jsonl"))
                .unwrap()
                .lines()
                .next()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(state["result"]["stamp"], first["arguments"]["stamp"]);
        (id, "completed")
    };
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let state = mcp_call(&client, "codemode_execution", json!({"id":id})).await;
        if state["status"] == expected {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "MCP state did not become {expected}: {state}"
        );
        assert_eq!(state["status"], "running", "{state}");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    if expected == "paused" {
        let rejected = client
            .call_tool(
                CallToolRequestParams::new("codemode_execution")
                    .with_arguments(json!({"id":id,"resume":true}).as_object().unwrap().clone()),
            )
            .await
            .unwrap();
        assert_eq!(rejected.is_error, Some(true));
        let invalid = client
            .call_tool(
                CallToolRequestParams::new("codemode_execution")
                    .with_arguments(json!({"id":id,"resume":"yes"}).as_object().unwrap().clone()),
            )
            .await;
        assert!(invalid.is_err());
    }
    client.cancel().await.unwrap();
    task.await.unwrap();
}

#[test]
fn killed_host_resumes_running_execution_without_repeating_recorded_calls() {
    let directory = tempfile::tempdir().unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "child", "--nocapture"])
        .env("INCURS_DQUICKTS_STAGE", "crash")
        .env("INCURS_DQUICKTS_DIR", directory.path())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !directory.path().join("ready").exists() {
        assert!(
            child.try_wait().unwrap().is_none(),
            "child exited before host wait"
        );
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("child did not reach host wait");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    child.kill().unwrap();
    assert!(!child.wait().unwrap().success());
    stage(directory.path(), "mcp-recover");
    let effects: Vec<Value> = fs::read_to_string(directory.path().join("effects.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(effects.len(), 2);
    assert_eq!(effects[0]["method"], "observe");
    assert_eq!(effects[1]["method"], "block");
    assert_eq!(
        effects[1]["arguments"]["stamp"],
        effects[0]["arguments"]["stamp"]
    );
}

#[tokio::test]
async fn child() {
    let Ok(stage) = std::env::var("INCURS_DQUICKTS_STAGE") else {
        return;
    };
    let directory = PathBuf::from(std::env::var_os("INCURS_DQUICKTS_DIR").unwrap());
    if stage.starts_with("mcp-") {
        mcp_stage(&directory, &stage).await;
        return;
    }
    let store = Arc::new(DurableStore::open(directory.join("runtime.sqlite")).unwrap());
    let mode = mode(store.clone(), &directory, Duration::from_secs(5));
    match stage.as_str() {
        "crash" => {
            let state = mode.start(CRASH_PROGRAM).await.unwrap();
            fs::write(directory.join("id"), &state.id).unwrap();
            mode.drive_with(&state.id, Default::default())
                .await
                .unwrap();
            panic!("parent must kill the waiting host");
        }
        "recover" => {
            let id = fs::read_to_string(directory.join("id")).unwrap();
            let state = mode.drive_with(&id, Default::default()).await.unwrap();
            assert_eq!(
                state.status,
                ExecutionStatus::Completed,
                "{:?}",
                state.error
            );
            let first: Value = serde_json::from_str(
                fs::read_to_string(directory.join("effects.jsonl"))
                    .unwrap()
                    .lines()
                    .next()
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(state.result.unwrap()["stamp"], first["arguments"]["stamp"]);
        }
        "start" => {
            let state = mode.execute(PROGRAM).await.unwrap();
            assert_eq!(state.status, ExecutionStatus::Paused, "{:?}", state.error);
            assert_eq!(state.log.last().unwrap().seq, 1);
            fs::write(directory.join("id"), &state.id).unwrap();
            store
                .put_snippet(&Snippet {
                    name: "saved".into(),
                    description: "restart fixture".into(),
                    code: PROGRAM.into(),
                    saved_at: 1,
                    input_schema: None,
                    connectors: vec!["effects".into()],
                })
                .await
                .unwrap();
        }
        "approve-first" | "approve-second" => {
            let id = fs::read_to_string(directory.join("id")).unwrap();
            let seq = if stage == "approve-first" { 1 } else { 2 };
            let state = mode.approve(&id, seq).await.unwrap();
            let expected = if seq == 1 {
                ExecutionStatus::Paused
            } else {
                ExecutionStatus::Completed
            };
            assert_eq!(state.status, expected, "{:?}", state.error);
            if seq == 1 {
                assert_eq!(state.log.last().unwrap().seq, 2);
            }
        }
        "verify" => {
            let id = fs::read_to_string(directory.join("id")).unwrap();
            let state = mode.execution(&id).await.unwrap();
            assert_eq!(state.status, ExecutionStatus::Completed);
            let result = state.result.unwrap();
            let first: Value = serde_json::from_str(
                fs::read_to_string(directory.join("effects.jsonl"))
                    .unwrap()
                    .lines()
                    .next()
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(result["stamp"], first["arguments"]["stamp"]);
            assert_eq!(result["remembered"], result["stamp"]);
            assert_eq!(result["total"], 3);
            assert_eq!(
                result["bytes"],
                json!({"__codemode_type":"binary","value":"Bwj/"})
            );
            assert_eq!(result["large"].as_str().unwrap(), "z".repeat(1000100));
            assert_eq!(
                state.logs,
                vec![format!("prelude {}", result["stamp"].as_str().unwrap())]
            );
            assert_eq!(
                store.get_snippet("saved").await.unwrap().unwrap().code,
                PROGRAM
            );
            assert_eq!(store.list_snippets().await.unwrap().len(), 1);
            assert!(store.delete_snippet("saved").await.unwrap());
            assert!(!store.delete_snippet("saved").await.unwrap());
            assert_eq!(store.list_executions().await.unwrap().len(), 1);
            assert!(
                ArtifactStore::get(store.as_ref(), "wrong-owner", "unknown")
                    .await
                    .unwrap()
                    .is_none()
            );
            RuntimeStore::delete_execution(store.as_ref(), &id)
                .await
                .unwrap();
            assert!(store.get_execution(&id).await.unwrap().is_none());
        }
        _ => panic!("unknown stage"),
    }
}

#[tokio::test]
async fn r4_changed_source_rejects_checkpoint_before_dispatch() {
    let directory = tempfile::tempdir().unwrap();
    let store = Arc::new(DurableStore::open(directory.path().join("runtime.sqlite")).unwrap());
    let mode = mode(store.clone(), directory.path(), Duration::from_secs(5));
    let state = mode.execute(PROGRAM).await.unwrap();
    assert_eq!(state.status, ExecutionStatus::Paused);
    let mut changed = store.get_execution(&state.id).await.unwrap().unwrap();
    changed.code = "async () => 99".into();
    store.put_execution(&changed).await.unwrap();
    let state = mode.approve(&state.id, 1).await.unwrap();
    assert_eq!(state.status, ExecutionStatus::Error);
    assert!(
        state
            .error
            .unwrap()
            .contains("Checkpoint identity mismatch")
    );
    assert_eq!(
        fs::read_to_string(directory.path().join("effects.jsonl"))
            .unwrap()
            .lines()
            .count(),
        1
    );
}

#[test]
fn r5_store_requires_exclusive_owner() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("runtime.sqlite");
    let owner = DurableStore::open(&path).unwrap();
    assert!(
        DurableStore::open(&path)
            .err()
            .unwrap()
            .contains("ownership unavailable")
    );
    drop(owner);
    assert!(DurableStore::open(&path).is_ok());
}

#[tokio::test]
async fn r6_uninterrupted_javascript_times_out() {
    let directory = tempfile::tempdir().unwrap();
    let store = Arc::new(DurableStore::open(directory.path().join("runtime.sqlite")).unwrap());
    let state = mode(store, directory.path(), Duration::from_millis(20))
        .execute("async () => { while (true) {} }")
        .await
        .unwrap();
    assert_eq!(state.status, ExecutionStatus::Error);
    assert!(state.error.unwrap().contains("CODE_EXECUTION_TIMEOUT"));
}

#[tokio::test]
async fn artifact_reads_enforce_the_recorded_owner() {
    let directory = tempfile::tempdir().unwrap();
    let store = DurableStore::open(directory.path().join("runtime.sqlite")).unwrap();
    let artifact = ArtifactStore::put(&store, "owner", &json!({"secret":7}))
        .await
        .unwrap();
    assert_eq!(
        ArtifactStore::get(&store, "owner", &artifact.id)
            .await
            .unwrap(),
        Some(json!({"secret":7}))
    );
    assert!(
        ArtifactStore::get(&store, "wrong-owner", &artifact.id)
            .await
            .unwrap()
            .is_none()
    );
    ArtifactStore::delete_execution(&store, "owner")
        .await
        .unwrap();
    assert!(
        ArtifactStore::get(&store, "owner", &artifact.id)
            .await
            .unwrap()
            .is_none()
    );
}
