use incurs_codemode::{ConnectorDescription, build_checkpoint_source};
use sha2::{Digest, Sha256};

pub(crate) const ENGINE_WASM: &[u8] = include_bytes!("dquickts/engine.wasm");
pub(crate) const ENGINE_JS: &str = include_str!("dquickts/engine.js");
pub(crate) const RUNNER_JS: &str = include_str!("dquickts/runner.js");

/// Dynamic Worker sandbox settings.
#[derive(Debug, Clone)]
pub struct DynamicWorkerOptions {
    /// Maximum execution time in milliseconds.
    pub timeout_ms: u64,
    /// Compatibility date for the child Worker.
    pub compatibility_date: String,
    /// Additional JavaScript modules.
    pub modules: Vec<(String, String)>,
    /// Maximum guest JavaScript heap in bytes.
    pub memory_limit: usize,
    /// Maximum guest stack in bytes.
    pub stack_size: usize,
    /// Maximum interrupt callbacks per host interval, including when Worker clocks are frozen.
    pub interrupt_budget: u32,
}

impl Default for DynamicWorkerOptions {
    fn default() -> Self {
        Self {
            timeout_ms: 60_000,
            compatibility_date: "2025-06-01".to_string(),
            modules: Vec::new(),
            memory_limit: 64 * 1024 * 1024,
            stack_size: 512 * 1024,
            interrupt_budget: 10_000,
        }
    }
}

/// Builds the internal Dynamic Worker module for one sandbox program.
pub fn build_executor_module(
    code: &str,
    connectors: &[ConnectorDescription],
    timeout_ms: u64,
) -> Result<String, String> {
    build_checkpoint_executor_module(
        code,
        connectors,
        "",
        &DynamicWorkerOptions {
            timeout_ms,
            ..Default::default()
        },
    )
}

pub(crate) fn build_checkpoint_executor_module(
    code: &str,
    connectors: &[ConnectorDescription],
    execution_id: &str,
    options: &DynamicWorkerOptions,
) -> Result<String, String> {
    let source = build_checkpoint_source(code, connectors, execution_id)?;
    let mut fingerprint = Sha256::new();
    fingerprint.update(ENGINE_WASM);
    fingerprint.update(ENGINE_JS);
    fingerprint.update(RUNNER_JS);
    fingerprint.update(source.as_bytes());
    let identity = format!("dquickts-wasm-v1/{:x}", fingerprint.finalize());
    let settings = serde_json::json!({
        "source":source, "identity":identity, "executionId":execution_id,
        "memoryLimit":options.memory_limit, "stackSize":options.stack_size,
        "timeoutMs":options.timeout_ms, "interruptBudget":options.interrupt_budget,
    });
    Ok(format!(
        r#"import {{ WorkerEntrypoint }} from "cloudflare:workers";
import createEngine from "incurs-engine.js";
import engineWasm from "incurs-engine.wasm";
import {{ runCheckpointProgram }} from "incurs-runner.js";
export default class CodeExecutor extends WorkerEntrypoint {{
  async evaluate() {{
    const settings = {settings};
    settings.executionId ||= this.env.__INCURS_CODEMODE_EXECUTION_ID;
    return runCheckpointProgram(settings, createEngine, engineWasm, async payload => {{
      const response = await fetch(
        "https://incurs-codemode.internal/__incurs_codemode_dispatch",
        {{
          method: "POST",
          headers: {{ "content-type": "application/json" }},
          body: JSON.stringify(payload)
        }}
      );
      if (!response.ok) throw new Error(await response.text());
      return response.json();
    }});
  }}
}}
"#
    ))
}

#[cfg(test)]
mod tests {
    use incurs_codemode::{ConnectorTool, ToolAnnotations};
    use serde_json::json;

    use super::*;

    #[test]
    fn emits_isolated_fetch_harness() {
        let source = build_executor_module(
            "state.read({ id: 1 })",
            &[ConnectorDescription {
                name: "state".to_string(),
                instructions: None,
                tools: vec![ConnectorTool {
                    name: "read".to_string(),
                    description: None,
                    input_schema: json!({"type": "object"}),
                    output_schema: None,
                    instructions: None,
                    examples: Vec::new(),
                    annotations: ToolAnnotations::default(),
                    policy: incurs_codemode::ToolPolicy::default(),
                }],
            }],
            60_000,
        )
        .unwrap();
        assert!(source.contains("const state"));
        assert!(source.contains("const response = await fetch("));
        // The dispatch shape moved from per-method bindings into the namespace proxy,
        // so the harness spells it with double quotes now.
        assert!(source.contains("incurs-engine.wasm"));
        assert!(source.contains("runCheckpointProgram"));
    }
}
