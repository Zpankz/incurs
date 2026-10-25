# Code Mode integration

Code Mode is a library/service integration, not an `incurs codemode` subcommand.
`incurs-codemode` owns dispatch, lifecycle, approval, replay and rollback;
`incurs-codemode-local` supplies bounded local JavaScript execution and SQLite
persistence; `incurs-codemode-mcp` exposes lifecycle tools to agents.
For pi-specific lifecycle, approval and ownership boundaries, read the
[pi integration design](pi-integration.md) before connecting a sidecar.

## Embed your command graph

Given an existing `cli: Cli`, register its canonical tool catalog:

```rust
use std::sync::Arc;
use incurs_codemode::IncurConnector;
use incurs_codemode_local::open_codemode;

let codemode = open_codemode(
    "codemode.sqlite",
    Default::default(),
    vec![Arc::new(IncurConnector::new(cli.tool_catalog()))],
)?;
// For a CLI named math with a sum command accepting left/right options:
let execution = codemode.execute("math.sum({ left: 2, right: 3 })").await?;
```

Add compatible versions of the three crates your host uses. Verify against
`crates/incurs-codemode-local/examples/local.rs` in the checkout. Do not invent
tool identifiers: discovered declarations define the JavaScript namespace.

For MCP, use the actor boundary so lifecycle calls remain responsive:

```rust
let service = incurs_codemode_local::spawn_codemode(
    "codemode.sqlite", Default::default(),
    vec![Arc::new(IncurConnector::new(cli.tool_catalog()))],
)?;
incurs_codemode_mcp::serve_stdio(Arc::new(service)).await?;
```

## Agent workflow

1. `codemode_search`: find tools/snippets and read returned declarations.
2. `codemode_execute`: submit a small JavaScript program using those declarations.
3. `codemode_execution`: inspect durable state/results/artifacts by execution ID.
4. If approval is pending, show the proposed side effect to the user; only
   `codemode_decide` may approve/reject it. Never auto-approve or use resume to
   bypass approval. `codemode_cancel` stops running or paused work.
5. After restart, `codemode_execution({"id":"…","resume":true})` resumes
   interrupted running work; pending approvals still require a decision.

Keep one host per SQLite database. Configure LocalExecutorOptions time/heap/stack
limits. Interrupted external calls may execute again before their result was
journaled: make effects idempotent. Checkpoints require matching engine/program;
finish executions before incompatible engine upgrades.

## Using pi's codemode as a client

Pi's `codemode` tool is a separate host sandbox, not this embedded executor.
Discover the available MCP lifecycle tool with `searchTools`, inspect its schema
with `describeTool`, then invoke the returned tool through `tools.<identifier>`.
Await every call, filter large results, and print only useful state. Neither
sandbox implies Node filesystem/network access; do not copy pi-only globals
into incurs JavaScript programs or assume `incurs --mcp` automatically hosts
Code Mode. Build and register the Code Mode MCP host explicitly.
