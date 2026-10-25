# Pi harness integration

## Recommendation and evidence scope

Use incurs as an **opt-in durable workflow sidecar**, with a thin pi extension
for trusted approvals and session bookkeeping. Keep pi's built-in `codemode`
for short-lived tool composition. This is a researched design, **not an installed
bridge**; the skill installation alone does not register a Code Mode server.

Examined pi `@earendil-works/pi-coding-agent` **1.4.0** and incurs base revision
`b105e4c33a06dd0f8c3607aac13ac45157a61663`. Confirm APIs against your installed
version before implementing. Pi documentation paths below are relative to that
package root; incurs paths are relative to this repository.

## Why use both?

| Concern | Native pi codemode | Incurs companion |
| --- | --- | --- |
| Tool composition | Existing pi tools and classifier/image models | Explicitly registered connector tools |
| State | Branch-visible small JSON `store`/`load` entries | SQLite journal, heap checkpoints, durable executions and artifacts |
| Lifetime | Calls still running when a script ends are cancelled | Starting request can finish while durable work continues |
| Approval | Pi's normal nested tool execution pipeline | Pending action with explicit approve/reject transition |
| Recovery | Persisted JSON is not a suspended JavaScript continuation | Matching engine/program can recover checkpointed state |
| Runtime API | `tools`, `models`, `text`, `searchTools`, etc. | Generated connector namespaces and incurs program contract |

These runtimes are not source-compatible. Do not replace pi's `codemode` tool
name, or promise pi-only globals inside incurs scripts. Compensation is not an
ACID transaction: a connector's default `revert` returns `Ok(false)`.

Sources: pi `docs/codemode.md`; incurs
`crates/incurs-codemode-local/README.md`,
`crates/incurs-codemode/src/program.rs`, and
`crates/incurs-codemode/src/connector.rs`.

## Stage 1 — explicit MCP sidecar

Build a host using `spawn_codemode(database, options, connectors)` and
`incurs_codemode_mcp::serve_stdio(Arc::new(service))`; see
[Code Mode setup](codemode.md). Register only known, allowlisted connectors.
The repository's `--example mcp` starts with **an empty connector list**: it
proves the transport, not useful pi-tool integration.

Pi supports stdio MCP registration in `~/.pi/agent/mcp.json`, trusted project
`.pi/mcp.json`, or an extension's `pi.registerMcpServer()`. An extension-managed
registration must be recreated when the extension loads. Prefer a project-local,
opt-in server while evaluating the design; do not change global MCP settings just
because this reference exists.

Use the five ordinary lifecycle operations first: search, execute, execution,
decide and cancel. Pi's native codemode can discover and call the MCP tools;
that gives access to the sidecar's tools, **not the reverse**.

Do not expose an unrestricted model-callable approval path. A sidecar's generic
`codemode_decide` API is not proof of user consent. Block model-originated
decisions and provide a separate trusted operator path. Hiding a tool from the
prompt is not access control. If that gate cannot be established, keep protected
actions disabled and limit the proof to explicitly safe local reads.

Sources: pi `docs/mcp.md`, `docs/extensions.md`; incurs
`crates/incurs-codemode-mcp/examples/mcp.rs`,
`crates/incurs-codemode-mcp/README.md`.

## Stage 2 — thin pi extension

Suggested UI: `/incurs status`, `/incurs approve`, `/incurs reject`, and
`/incurs cancel`. Use a distinct model-facing name such as `incurs_execute`.
These are proposed commands, not commands supplied by this fork.

- Show execution ID, action sequence, canonical tool and exact arguments before
  asking the operator. Only a trusted UI/command path may issue the decision.
  Fail closed in noninteractive contexts without an independent approval channel.
- Persist references with `pi.appendEntry()`; recover branch-visible entries from
  `ctx.sessionManager.getBranch()`. Retain execution IDs, sidecar identity,
  ownership binding and schema/program identity, not secrets or entire heaps.
- Initialize on `session_start`; clean up idempotently on `session_shutdown`.
  Define whether shutdown cancels work or deliberately detaches it, and tell the
  user. Do not equate disconnect with cancellation.
- Branching copies references, not external effects. A fork must create a new
  execution or explicitly transfer ownership; never silently replay writes.
- Keep one sidecar owner per SQLite database. Share a host through an explicit
  supervisor if needed; do not launch one database writer per branch.
- Treat execution IDs as locators, not credentials. Bind reads, artifacts,
  approvals, resume and cancellation to an authorized storage/session partition.
  The MCP adapters do not authenticate requests or infer tenants from headers.
- Use `ExecutionProjector` for bounded, pi-friendly status summaries and widget
  details if useful. Its projection is presentation, not authorization; search,
  artifacts and service errors bypass it.

Pi extensions run with the process's OS privileges. A QuickJS heap limit or an
approval widget is not a filesystem/network sandbox for connector handlers.

Sources: pi `docs/extensions.md`, `docs/security.md`; incurs
`crates/incurs-codemode-mcp/README.md` and
`crates/incurs-codemode-local/src/lib.rs`.

## Stage 3 — selectively bridge pi tools back to incurs

MCP client registration alone cannot export pi's tool registry to the sidecar.
A bidirectional, authenticated connector bridge would be new implementation.
Within a live pi tool context, use the documented:

```ts
ctx.executeTool(name, args, { signal, onUpdate })
```

Do not call an extension tool's implementation directly: that can bypass schema
validation, permissions, tool hooks and nested-call auditing. The bridge needs:

1. An allowlist with canonical identities and schemas; preserve discovery
   instructions and return types, and reject normalized-name collisions.
2. Host-supplied session authority and per-call cancellation/progress propagation.
   Never trust a model-supplied session ID as authority.
3. A rule for detached jobs: do not retain a stale tool context or silently renew
   old permissions after restart. Suspend pi-tool dispatch until a fresh,
   authorized context exists, or keep detached work on sidecar-native connectors.
4. A recursion barrier excluding native `codemode`, the incurs lifecycle tools,
   approval controls and bridge tools from their own nested catalogs.
5. Full error/result fidelity, including `isError`, structured results, images,
   artifact ownership and output limits. Avoid collapsing everything to strings.

Prefer read-only tools initially. Existing `models` credentials and classifier
accounting should remain owned by pi; exposing them to Rust would require a
separate reviewed adapter rather than copying credentials into a database.

Source: pi `docs/extensions.md` (`executeTool`) and `docs/codemode.md`.

## Lifecycle correctness requirements

- Explicitly call `codemode_cancel` when cancellation is intended. The MCP
  execute/decide/resume paths detach durable work from the request token.
- Propagate cancellation to every downstream connector. Dropping a local future
  does not guarantee an already accepted remote operation stopped.
- Resume interrupted running executions only; pending approvals require a
  decision, never a generic resume. Re-evaluate current authority after restart.
- External effects can repeat if interruption occurs before journaling the
  result. Use idempotency keys or reconciliation; do not claim exactly-once.
- Finish pending executions before incompatible engine upgrades. A checkpoint
  requires the matching engine/program; do not silently restart the source.
- Bound JS CPU, heap, stack, artifacts, queued jobs and retention; do not turn a
  short MCP timeout into an unbounded background workload.

Sources: `crates/incurs-codemode-local/README.md`,
`crates/incurs-codemode-mcp/src/lib.rs`,
`crates/incurs-codemode/src/connector.rs`.

## MCP Tasks: promising, but a separate compatibility gate

Incurs supports the 2025-11-25 Tasks dialect and 2026-07-28 Tasks extension.
They have different opt-in, approval and final-result flows. Do not assume pi's
client negotiates either correctly just because ordinary MCP works. Start with
the five-tool lifecycle; test Tasks capability negotiation, form elicitation,
reject/dismiss, restart polling and cancellation before enabling Tasks.

## Acceptance tests before enabling a bridge

- A model cannot approve itself through direct calls, native pi codemode, or a
  nested incurs program; rejected/dismissed actions produce no external effect.
- Pi schema validation, `tool_call`/`tool_result` hooks and permission checks
  observe every bridged call; deny controls are tested, not only happy paths.
- Another session/branch cannot inspect artifacts, approve, resume or cancel a
  job without explicit authorized ownership transfer.
- Pi abort and shutdown follow the selected cancellation/detachment policy;
  cancellation reaches both a running JS pass and active downstream commands.
- Restart recovers a checkpoint; an interrupted unjournaled external action is
  reconciled without duplicating the effect.
- Recursion, namespace collisions, oversized results, malformed schemas,
  disconnected hosts, engine mismatch and concurrent database owners fail closed.

Validation performed for this research: incurs CLI's 28 tests passed. All 35
non-doc tests in `incurs-codemode-mcp` passed, including approval, cancellation,
Tasks and restart tests. The subsequent rustdoc process was killed with SIGKILL;
therefore the full crate test command was **not** clean. These are incurs-side
checks, not evidence that a pi/incurs bridge or MCP Tasks negotiation was tested.
