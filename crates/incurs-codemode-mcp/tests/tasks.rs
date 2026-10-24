#[path = "tasks/approvals.rs"]
mod approvals;
#[path = "tasks/recovery.rs"]
mod recovery;
#[path = "tasks/restart.rs"]
mod restart;
#[path = "tasks/support.rs"]
mod support;
use serde_json::json;
use support::{Wire, meta, service};

#[tokio::test]
async fn opted_in_execution_returns_a_task_and_its_final_result() {
    for modern in [true, false] {
        let mut wire = Wire::connect(modern, service(vec![])).await;
        let id = wire.start("async () => 42").await;
        let completed = wire.poll(&id, "completed").await;
        let result = if modern {
            completed["result"].clone()
        } else {
            wire.rpc("tasks/result", json!({"taskId":id})).await["result"].clone()
        };
        assert_eq!(result["structuredContent"]["result"], 42, "{result}");
        assert_eq!(result["isError"], false);
        let unknown = wire.rpc("tasks/get", json!({"taskId":"not-issued"})).await;
        assert_eq!(unknown["error"]["code"], -32602);
        let cancel = wire.rpc("tasks/cancel", json!({"taskId":id})).await;
        assert_eq!(cancel["error"]["code"], -32602);
    }
}

#[tokio::test]
async fn both_dialects_cancel_running_tasks() {
    for modern in [true, false] {
        let mut wire = Wire::connect(modern, service(vec![])).await;
        let id = wire.start("async () => await new Promise(() => {})").await;
        let cancelled = wire.rpc("tasks/cancel", json!({"taskId":id})).await;
        assert!(cancelled.get("error").is_none(), "{cancelled}");
        wire.poll(&id, "cancelled").await;
        let unknown = wire
            .rpc("tasks/cancel", json!({"taskId":"not-issued"}))
            .await;
        assert_eq!(unknown["error"]["code"], -32602);
    }
}

#[tokio::test]
async fn task_errors_follow_each_dialects_status_contract() {
    for modern in [true, false] {
        let mut wire = Wire::connect(modern, service(vec![])).await;
        let id = wire
            .start("async () => { throw new Error('literal failure'); }")
            .await;
        let state = wire
            .poll(&id, if modern { "completed" } else { "failed" })
            .await;
        let result = if modern {
            state["result"].clone()
        } else {
            wire.rpc("tasks/result", json!({"taskId":id})).await["result"].clone()
        };
        assert_eq!(result["isError"], true, "{result}");
        assert!(
            result["structuredContent"]["error"]
                .as_str()
                .unwrap()
                .contains("literal failure")
        );
    }
}

#[tokio::test]
async fn tasks_require_capability_opt_in_and_ordinary_calls_still_work() {
    let mut wire = Wire::connect(true, service(vec![])).await;
    let mut capabilities = meta();
    capabilities["io.modelcontextprotocol/clientCapabilities"] = json!({});
    wire.seq += 1;
    wire.write(
        json!({"jsonrpc":"2.0","id":wire.seq,"method":"tools/call","params":{
            "_meta":capabilities,"name":"codemode_execute","arguments":{"code":"async () => 42"}
        }}),
    )
    .await;
    let ordinary = wire.read().await;
    assert_eq!(ordinary["result"]["resultType"], "complete", "{ordinary}");
    assert_eq!(ordinary["result"]["structuredContent"]["status"], "running");
    let ordinary_id = ordinary["result"]["structuredContent"]["id"]
        .as_str()
        .unwrap();
    let hidden = wire.rpc("tasks/get", json!({"taskId":ordinary_id})).await;
    assert_eq!(hidden["error"]["code"], -32602);
    let id = wire.start("async () => 42").await;
    assert_eq!(id.len(), 53);
    let other = wire.start("async () => 42").await;
    assert_ne!(id, other);
    wire.seq += 1;
    wire.write(json!({"jsonrpc":"2.0","id":wire.seq,"method":"tasks/get","params":{"_meta":capabilities,"taskId":id}})).await;
    let response = wire.read().await;
    assert_eq!(response["error"]["code"], -32021, "{response}");
}
