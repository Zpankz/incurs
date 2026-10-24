# incurs-codemode-cloudflare

This crate supplies Cloudflare-specific execution, storage, lifecycle, and
Streamable HTTP adapters for the provider-neutral `incurs-codemode` runtime.

`DynamicWorkerExecutor` runs the shared checkpoint harness in a pinned dquickts
WebAssembly engine. The owning Durable Object stores the heap before dispatch,
including awaiting promises and local variables. SQLite checkpoints are chunked
into rows below the platform row limit, with a 32 MB serialized checkpoint cap.
The chunk set is replaced in one SQL statement. Program and engine mismatches
fail without replaying source.

The checked-in engine has its source revision, compiler, license, and hashes in
`src/dquickts/provenance.json`. Rebuild it from the vendored native source with
`python3 extensions/cloudflare/build_dquickts.py` from the repository root.
Run `worker-build --release` in the example directory, then run its
`verify_durability.py` to exercise real Worker restarts, approvals, artifacts,
save receipts, and runaway code. This proves local workerd behavior; production
qualification remains separate.

Use the workspace example for a complete Worker, Durable Object, authentication,
Origin allowlist, and Dynamic Worker configuration:
[`../../examples/codemode-worker`](../../examples/codemode-worker).

The remote endpoint fails closed until `MCP_AUTH_TOKEN` is configured. Set
`MCP_ALLOWED_ORIGINS` to a comma-separated list of additional browser origins.
