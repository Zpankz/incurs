# incurs Code Mode local executor

`incurs-codemode-local` runs Code Mode in a pinned dquickts engine. The default
executor checkpoints the JavaScript heap before each host call, preserving
closures, random values, pending promises, and loop state across approvals.

```rust
use std::sync::Arc;

use incurs::cli::Cli;
use incurs_codemode::{ExecutionState, IncurConnector};
use incurs_codemode_local::open_codemode;

async fn execute(cli: &Cli) -> Result<ExecutionState, String> {
    let codemode = open_codemode(
        "codemode.sqlite",
        Default::default(),
        vec![Arc::new(IncurConnector::new(cli.tool_catalog()))],
    )?;
    codemode.execute("tools.lookup({ id: 42 })").await
}
```

Run the complete local example:

```sh
cargo run -p incurs-codemode-local --example local
```

The example stores its state in `codemode.sqlite`. Supply a database path and
program as positional arguments to choose another location and command.

`LocalExecutorOptions` bounds uninterrupted JavaScript execution time, QuickJS
heap usage, and stack size. Host waits do not consume the JavaScript time budget.
`open_codemode` persists the journal, snippets, oversized artifacts, and heap in
one database. One host owns the database at a time; its SQLite lock is released
when that host exits, including an abrupt kill.

For MCP, pass `spawn_codemode(path, options, connectors)` to
`incurs_codemode_mcp::serve_stdio`. After a host restart, use
`codemode_execution({"id":"...","resume":true})` for an interrupted running
execution, or `codemode_decide` for a pending approval. Resume inherits the new
request context and cannot bypass approval.

`MemoryStore` is intentionally ephemeral. Custom `RuntimeStore` implementations
must implement `get_checkpoint` and `put_checkpoint`; unsupported stores fail
explicitly. Completed work retained in a checkpoint is resumed without replay.
An interrupted call whose result was not journaled may run again, so connectors
need idempotency for external effects.

Checkpoints contain trusted bytecode and accept only the matching engine build
and program. Finish existing executions before upgrading an incompatible build;
the executor rejects a mismatch instead of restarting the source. The vendored
MIT-licensed source and its hashes are in `native/quickjs/provenance.json`.

Runaway JavaScript terminates the pass with this stable error contract:
`CODE_EXECUTION_TIMEOUT: maximum uninterrupted JavaScript execution interval exceeded`.
