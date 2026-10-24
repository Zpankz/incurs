# incurs Code Mode MCP

`incurs-codemode-mcp` exposes the generic Code Mode lifecycle through a real
`rmcp::ServerHandler` with exactly five tools:

- `codemode_search`
- `codemode_execute`
- `codemode_execution`
- `codemode_decide`
- `codemode_cancel`

`codemode_execution` accepts an optional `artifact_id` to retrieve an oversized
value owned by the named execution. Search results include the TypeScript
declarations or snippet source needed to use each match. Execution snapshots
keep oversized values as artifact references, so responses remain bounded and
expose the identifier needed for retrieval.

`codemode_cancel` and `tasks/cancel` stop the active JavaScript pass and its
incurs tool calls. Finishing the initial request leaves durable work running. HTTP transports also pass their request method, path, and
headers into incurs request context.

Use `CodeModeMcpServer` with any compatible `rmcp` server transport, or call
`serve_stdio` for process stdio.

`serve_stdio` selects the Tasks dialect from the first request. A client that
initializes MCP 2025-11-25 uses `LegacyCodeModeMcpServer`; stateless MCP
2026-07-28 requests use `CodeModeMcpServer`. Hosts embedding a handler directly
select the matching adapter.

| Protocol | Execution opt-in | User input | Final result |
| --- | --- | --- | --- |
| 2025-11-25 | `tools/call` with `task: {}` | `elicitation/create` while awaiting `tasks/result` | `tasks/result` |
| 2026-07-28 | Per-request `extensions["io.modelcontextprotocol/tasks"]` capability | `tasks/get` returns `inputRequests`; client answers with `tasks/update` | Embedded in `tasks/get` |

Only `codemode_execute` creates tasks. Clients that do not opt in retain the
five-tool lifecycle. Task support is advertised only when the service supports
retained execution and detached approvals. The local service generates random
192-bit execution IDs and retains tasks for 24 hours; use `spawn_codemode` with
a database path to preserve them across process restarts. MemoryStore is
ephemeral. History pruning respects each task's persisted retention deadline. Polling
recovers interrupted running jobs using the current request context; active
passes and pending approvals are left in place.

Protected actions request a form with a required boolean `approve` field.
Acceptance with `approve: true` authorizes the action; `false` or decline
rejects it, and dismissal cancels the task. Answered input keys are ignored.
Cancellation of an unknown or terminal task returns invalid params. The 2025
adapter reports tool failures as `failed`; the current extension returns
`completed` with `isError: true`. Task listing and optional status notifications
are not advertised.

Bind each handler and service to one authorized storage partition. The adapters
do not authenticate requests or infer tenants from headers.

Ordinary incurs command handlers can use `context.request.elicitation` to
request a form or URL from a session-based MCP client. Decline and dismissal
remain explicit responses, and unsupported modes fail before contacting the
client. Form values remain untrusted input for the handler to validate.
Stateless task approvals use the `inputRequests` flow above; ordinary stateless
command elicitation is not implemented.

Run the durable stdio example with:

```sh
cargo run -p incurs-codemode-mcp --example mcp -- ./codemode.sqlite
```

Wire behavior follows the [2025 Tasks specification](https://modelcontextprotocol.io/specification/2025-11-25/basic/utilities/tasks)
and the [current Tasks extension](https://tasks.extensions.modelcontextprotocol.io/specification/2026-07-28/tasks).

For host-specific output, implement `ExecutionProjector` and install it with
`with_projector(Arc<dyn ExecutionProjector>)`. Its `arguments()` method adds
optional argument properties to the lifecycle tools; existing lifecycle
properties keep their definitions. Its `project()` method receives the tool
name, caller arguments, and serialized execution from successful execute, read,
approve, reject, and cancel calls. The host projector takes precedence over
`with_projection` in either builder order. Search results, retrieved artifacts,
and service errors bypass projection. Installing another projector replaces
the previous projector and its added argument properties.

Use `CodeModeMcpServer` with any compatible `rmcp` server transport, or call
`serve_stdio` for process stdio.
