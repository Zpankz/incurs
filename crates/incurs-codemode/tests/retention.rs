use std::sync::Arc;

use incurs_codemode::{CodeModeRuntime, MemoryStore, RuntimeStore};
use serde_json::json;

#[tokio::test]
async fn persisted_retention_survives_history_pruning() {
    let store = Arc::new(MemoryStore::default());
    let runtime = CodeModeRuntime::new(store.clone());
    let id = runtime.begin("async () => 42", vec![], 1).await.unwrap();
    runtime.complete(&id, json!(42), vec![], 2).await.unwrap();
    let state = store.get_execution(&id).await.unwrap().unwrap();
    let mut wire = serde_json::to_value(state).unwrap();
    wire["retained_until"] = json!(86_400_001_u64);
    store
        .put_execution(&serde_json::from_value(wire).unwrap())
        .await
        .unwrap();
    let reopened = CodeModeRuntime::new(store);
    reopened.prune(0).await.unwrap();
    assert_eq!(
        reopened
            .execution(&id)
            .await
            .unwrap()
            .map(|state| state.result),
        Some(Some(json!(42)))
    );
    assert_eq!(reopened.prune_at(0, 86_400_001).await.unwrap(), 1);
    assert!(reopened.execution(&id).await.unwrap().is_none());
}
