use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

use incurs_codemode::{
    CodeExecutor, CodeMode, CodeModeRunOptions, ConnectorDescription, ExecuteResult, ExecutionHost,
    ExecutionStatus, MemoryStore,
};
use serde_json::json;
use tokio::sync::Notify;

struct Waiting {
    started: Arc<Notify>,
    calls: Arc<AtomicUsize>,
}
#[async_trait::async_trait(?Send)]
impl CodeExecutor for Waiting {
    async fn execute(
        &self,
        _: &str,
        _: &[ConnectorDescription],
        _: &str,
        _: Arc<ExecutionHost>,
    ) -> Result<ExecuteResult, String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.started.notify_one();
        std::future::pending().await
    }
}

fn fixture() -> (CodeMode, Arc<Notify>, Arc<AtomicUsize>) {
    let started = Arc::new(Notify::new());
    let calls = Arc::new(AtomicUsize::new(0));
    let mode = CodeMode::new(
        Arc::new(MemoryStore::default()),
        Waiting {
            started: started.clone(),
            calls: calls.clone(),
        },
        vec![],
    );
    (mode, started, calls)
}

#[tokio::test]
async fn idle_recovery_returns_without_queueing_an_active_pass() {
    let (mode, started, calls) = fixture();
    let state = mode.start("async () => 42").await.unwrap();
    let first = mode.drive_with(&state.id, CodeModeRunOptions::default());
    tokio::pin!(first);
    tokio::select! { _ = started.notified() => {}, result = &mut first => panic!("pass unexpectedly ended: {result:?}") }
    let observed = tokio::time::timeout(
        Duration::from_millis(100),
        mode.drive_if_idle(&state.id, CodeModeRunOptions::default()),
    )
    .await
    .expect("recovery queued behind the active pass")
    .unwrap();
    assert_eq!(observed.status, ExecutionStatus::Running);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    mode.cancel(&state.id).await.unwrap();
    assert_eq!(first.await.unwrap().status, ExecutionStatus::Cancelled);
}

#[tokio::test]
async fn idle_recovery_leaves_approval_pauses_and_terminal_results_unchanged() {
    let (mode, _, calls) = fixture();
    let state = mode.start("async () => 42").await.unwrap();
    mode.runtime()
        .decide(
            &state.id,
            0,
            "fixture",
            "save",
            json!({}),
            true,
            false,
            state.created_at,
        )
        .await
        .unwrap();
    let paused = tokio::time::timeout(
        Duration::from_millis(100),
        mode.drive_if_idle(&state.id, CodeModeRunOptions::default()),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(paused.status, ExecutionStatus::Paused);
    let terminal = mode.start("async () => 42").await.unwrap();
    mode.runtime()
        .complete(&terminal.id, json!(42), vec![], terminal.created_at)
        .await
        .unwrap();
    let done = mode
        .drive_if_idle(&terminal.id, CodeModeRunOptions::default())
        .await
        .unwrap();
    assert_eq!(done.status, ExecutionStatus::Completed);
    assert_eq!(done.result, Some(json!(42)));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}
