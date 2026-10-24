use std::path::PathBuf;
use std::sync::Arc;

use incurs_codemode::{
    Connector, ConnectorDescription, ConnectorTool, ReplayPolicy, ToolAnnotations, ToolContext,
    ToolPolicy,
};
use serde_json::{Value, json};

use super::support::Wire;

struct Hold(PathBuf);

#[async_trait::async_trait]
impl Connector for Hold {
    fn name(&self) -> &str {
        "hold"
    }
    async fn describe(&self) -> Result<ConnectorDescription, String> {
        Ok(ConnectorDescription {
            name: "hold".into(),
            instructions: None,
            tools: vec![ConnectorTool {
                name: "wait".into(),
                description: None,
                input_schema: json!({"type":"object"}),
                output_schema: None,
                instructions: None,
                examples: vec![],
                annotations: ToolAnnotations {
                    read_only: Some(true),
                    ..Default::default()
                },
                policy: ToolPolicy {
                    requires_approval: false,
                    replay: ReplayPolicy::Log,
                },
            }],
        })
    }
    async fn execute(
        &self,
        _: &str,
        arguments: Value,
        context: &ToolContext,
    ) -> Result<Value, String> {
        if std::env::var("INCURS_TASK_RECOVERY_STAGE").as_deref() == Ok("start") {
            std::fs::write(self.0.join("expected.json"), arguments.to_string())
                .map_err(|error| error.to_string())?;
            std::future::pending::<()>().await;
        }
        std::fs::write(
            self.0.join("context.json"),
            json!({"path":context.request.as_ref().map(|request|request.path.clone())}).to_string(),
        )
        .map_err(|error| error.to_string())?;
        Ok(json!(42))
    }
}

#[test]
fn running_task_recovers_on_poll_after_restart() {
    for modern in [true, false] {
        let root = std::env::temp_dir().join(format!(
            "incurs-task-recovery-{}-{}",
            std::process::id(),
            modern
        ));
        std::fs::create_dir(&root).unwrap();
        for stage in ["start", "finish"] {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "recovery::recovery_child", "--nocapture"])
                .env("INCURS_TASK_RECOVERY_ROOT", &root)
                .env("INCURS_TASK_RECOVERY_STAGE", stage)
                .env("INCURS_TASK_RECOVERY_MODERN", modern.to_string())
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}

async fn run_child(root: PathBuf, stage: String, modern: bool) {
    let service = incurs_codemode_local::spawn_codemode(
        root.join("state.sqlite"),
        Default::default(),
        vec![Arc::new(Hold(root.clone()))],
    )
    .unwrap();
    let mut wire = Wire::connect(modern, Arc::new(service)).await;
    if stage == "start" {
        let id = wire.start("async () => { const stamp = Math.random().toString(); const remember = () => stamp; await hold.wait({stamp}); return {stamp:remember(),answer:42}; }").await;
        for _ in 0..200 {
            if root.join("expected.json").exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        assert!(
            root.join("expected.json").exists(),
            "host call was never reached"
        );
        std::fs::write(root.join("task-id"), id).unwrap();
    } else {
        let id = std::fs::read_to_string(root.join("task-id")).unwrap();
        let done = wire.poll(&id, "completed").await;
        let result = if modern {
            done["result"].clone()
        } else {
            wire.rpc("tasks/result", json!({"taskId":id})).await["result"].clone()
        };
        let expected: Value =
            serde_json::from_str(&std::fs::read_to_string(root.join("expected.json")).unwrap())
                .unwrap();
        assert_eq!(result["structuredContent"]["result"]["answer"], 42);
        assert_eq!(
            result["structuredContent"]["result"]["stamp"],
            expected["stamp"]
        );
        let context: Value =
            serde_json::from_str(&std::fs::read_to_string(root.join("context.json")).unwrap())
                .unwrap();
        assert_eq!(context["path"], "tasks/get");
    }
}

#[test]
fn recovery_child() {
    let Ok(root) = std::env::var("INCURS_TASK_RECOVERY_ROOT") else {
        return;
    };
    let stage = std::env::var("INCURS_TASK_RECOVERY_STAGE").unwrap();
    let modern = std::env::var("INCURS_TASK_RECOVERY_MODERN").unwrap() == "true";
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(run_child(root.into(), stage, modern));
}
