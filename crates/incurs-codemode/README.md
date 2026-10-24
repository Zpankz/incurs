# incurs Code Mode

`incurs-codemode` is the provider-neutral durable execution layer for incurs
tools. It owns connector discovery, JavaScript harness generation, approval,
replay, cancellation, events, artifacts, rollback, snippets, and lifecycle
state.

Executors and stores are traits. Use `incurs-codemode-local` for an embedded
QuickJS runtime, `incurs-codemode-mcp` for the five-tool MCP lifecycle, or place
platform-specific adapters in a standalone extension workspace.

```rust
use std::sync::Arc;

use incurs::cli::Cli;
use incurs_codemode::IncurConnector;
use incurs_codemode_local::open_codemode;

let codemode = open_codemode(
    "codemode.sqlite",
    Default::default(),
    vec![Arc::new(IncurConnector::new(cli.tool_catalog()))],
)?;
let execution = codemode.execute("tools.lookup({ id: 42 })").await?;
```

Model-generated programs are JavaScript. The public lifecycle, connectors,
storage, policy, and transport contracts are Rust.
