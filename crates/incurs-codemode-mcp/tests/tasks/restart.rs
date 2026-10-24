use super::support::{Effects, Wire};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::{Arc, atomic::AtomicUsize};

#[test]
fn task_survives_a_server_process_restart() {
    let root = std::env::temp_dir().join(format!(
        "incurs-tasks-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&root).unwrap();
    for stage in ["start", "finish"] {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "restart::task_restart_child", "--nocapture"])
            .env("INCURS_TASK_TEST_ROOT", &root)
            .env("INCURS_TASK_TEST_STAGE", stage)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let effects = std::fs::read_to_string(root.join("effects.jsonl")).unwrap();
    assert_eq!(effects.lines().count(), 1);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn task_restart_child() {
    let Ok(root) = std::env::var("INCURS_TASK_TEST_ROOT") else {
        return;
    };
    let root = PathBuf::from(root);
    let stage = std::env::var("INCURS_TASK_TEST_STAGE").unwrap();
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
        let service = incurs_codemode_local::spawn_codemode(root.join("state.sqlite"),Default::default(),vec![Arc::new(Effects {count:Arc::new(AtomicUsize::new(0)),file:Some(root.join("effects.jsonl"))})]).unwrap();
        let mut wire = Wire::connect(true,Arc::new(service)).await;
        if stage == "start" {
            let id = wire.start("async () => { const stamp = Math.random().toString(); const remember = () => stamp; await effects.save({stamp}); return {stamp:remember(),answer:42}; }").await;
            wire.poll(&id,"input_required").await;
            let snapshot = wire.rpc("tools/call",json!({"name":"codemode_execution","arguments":{"id":id}})).await;
            let stamp = &snapshot["result"]["structuredContent"]["log"][0]["arguments"]["stamp"];
            assert!(stamp.is_string());
            std::fs::write(root.join("expected-stamp.json"),stamp.to_string()).unwrap();
            std::fs::write(root.join("task-id"),id).unwrap();
            assert!(!root.join("effects.jsonl").exists());
        } else {
            let id = std::fs::read_to_string(root.join("task-id")).unwrap();
            wire.poll(&id,"input_required").await;
            assert!(wire.answer(&id,"approval:0",json!({"action":"accept","content":{"approve":true}})).await.get("error").is_none());
            let done = wire.poll(&id,"completed").await;
            assert_eq!(done["result"]["structuredContent"]["result"]["answer"],42);
            let effect: Value = serde_json::from_str(std::fs::read_to_string(root.join("effects.jsonl")).unwrap().trim()).unwrap();
            assert_eq!(effect["request_path"],"tasks/update");
            let expected: Value = serde_json::from_str(&std::fs::read_to_string(root.join("expected-stamp.json")).unwrap()).unwrap();
            assert_eq!(done["result"]["structuredContent"]["result"]["stamp"],expected);
            assert_eq!(effect["stamp"],expected);
        }
    });
}
