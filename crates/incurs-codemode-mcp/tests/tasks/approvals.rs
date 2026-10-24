use super::support::{Effects, Wire, meta, service};
use serde_json::json;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

#[tokio::test]
async fn pending_task_input_requires_the_declared_form_capability() {
    let count = Arc::new(AtomicUsize::new(0));
    let mut wire = Wire::connect(
        true,
        service(vec![Arc::new(Effects {
            count: count.clone(),
            file: None,
        })]),
    )
    .await;
    let id = wire.start("async () => await effects.save({})").await;
    wire.poll(&id, "input_required").await;
    let mut metadata = meta();
    metadata["io.modelcontextprotocol/clientCapabilities"]["elicitation"] = json!({"url":{}});
    for method in ["tasks/get", "tasks/update"] {
        wire.seq += 1;
        wire.write(json!({"jsonrpc":"2.0","id":wire.seq,"method":method,"params":{
            "_meta":metadata,"taskId":id,"inputResponses":{"approval:0":{"action":"accept","content":{"approve":true}}}
        }})).await;
        let response = wire.read().await;
        assert_eq!(response["error"]["code"], -32021, "{response}");
        assert_eq!(count.load(Ordering::SeqCst), 0);
    }
    wire.answer_ok(
        &id,
        "approval:0",
        json!({"action":"accept","content":{"approve":true}}),
    )
    .await;
    wire.poll(&id, "completed").await;
    assert_eq!(count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn modern_approvals_validate_input_and_ignore_duplicate_keys() {
    let count = Arc::new(AtomicUsize::new(0));
    let mut wire = Wire::connect(
        true,
        service(vec![Arc::new(Effects {
            count: count.clone(),
            file: None,
        })]),
    )
    .await;
    let id = wire
        .start("async () => { await effects.save({n:1}); return await effects.save({n:2}); }")
        .await;
    let paused = wire.poll(&id, "input_required").await;
    assert_eq!(
        paused["inputRequests"]["approval:0"]["method"],
        "elicitation/create"
    );
    assert_eq!(
        paused["inputRequests"]["approval:0"]["params"]["requestedSchema"]["required"],
        json!(["approve"])
    );
    assert_eq!(
        paused["inputRequests"]["approval:0"]["params"]["requestedSchema"]["properties"]["approve"]
            ["type"],
        "boolean"
    );
    assert_eq!(count.load(Ordering::SeqCst), 0);
    let invalid = wire
        .answer(
            &id,
            "approval:0",
            json!({"action":"accept","content":{"approve":"true"}}),
        )
        .await;
    assert_eq!(invalid["error"]["code"], -32602);
    assert_eq!(count.load(Ordering::SeqCst), 0);
    let yes = json!({"action":"accept","content":{"approve":true}});
    wire.answer_ok(&id, "approval:0", yes.clone()).await;
    let second = wire.poll(&id, "input_required").await;
    assert!(second["inputRequests"].get("approval:1").is_some());
    wire.answer_ok(&id, "approval:0", yes.clone()).await;
    assert_eq!(count.load(Ordering::SeqCst), 1);
    wire.answer_ok(&id, "approval:1", yes.clone()).await;
    let done = wire.poll(&id, "completed").await;
    assert_eq!(done["result"]["structuredContent"]["result"], 42);
    wire.answer_ok(&id, "approval:1", yes).await;
    assert_eq!(count.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn declined_or_dismissed_approval_never_runs_the_action() {
    for (response, status) in [
        (json!({"action":"decline"}), "completed"),
        (json!({"action":"cancel"}), "cancelled"),
        (
            json!({"action":"accept","content":{"approve":false}}),
            "completed",
        ),
    ] {
        let count = Arc::new(AtomicUsize::new(0));
        let mut wire = Wire::connect(
            true,
            service(vec![Arc::new(Effects {
                count: count.clone(),
                file: None,
            })]),
        )
        .await;
        let id = wire.start("async () => await effects.save({})").await;
        wire.poll(&id, "input_required").await;
        assert!(
            wire.answer(&id, "approval:0", response)
                .await
                .get("error")
                .is_none()
        );
        let done = wire.poll(&id, status).await;
        if status == "completed" {
            assert_eq!(done["result"]["isError"], true);
        }
        assert_eq!(count.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn legacy_task_result_elicits_approval_from_the_client() {
    let count = Arc::new(AtomicUsize::new(0));
    let mut wire = Wire::connect(
        false,
        service(vec![Arc::new(Effects {
            count: count.clone(),
            file: None,
        })]),
    )
    .await;
    let id = wire.start("async () => await effects.save({})").await;
    wire.poll(&id, "input_required").await;
    wire.send("tasks/result", json!({"taskId":id})).await;
    let request = wire.read().await;
    assert_eq!(request["method"], "elicitation/create", "{request}");
    assert_eq!(
        request["params"]["_meta"]["io.modelcontextprotocol/related-task"]["taskId"],
        id
    );
    assert_eq!(count.load(Ordering::SeqCst), 0);
    wire.write(json!({"jsonrpc":"2.0","id":request["id"],"result":{"action":"accept","content":{"approve":true}}})).await;
    let done = wire.read().await;
    assert_eq!(done["result"]["structuredContent"]["result"], 42, "{done}");
    assert_eq!(count.load(Ordering::SeqCst), 1);
}
