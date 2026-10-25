---
name: incurs
description: Build and extend Rust incurs CLIs, generate typed command contracts, package binaries, and integrate Code Mode tool execution. Use for incurs (.rs), not TypeScript incur.
command: incurs
---

# incurs

```text
incurs/
├── SKILL.md                 # workflow and routing
└── references/
    ├── authoring.md         # typed commands, surfaces, config, tests
    ├── commands.md          # generated CLI flags and schemas
    ├── build-gen.md         # Cargo builds and Rust code generation
    └── codemode.md          # discovery, execution, approvals, host setup
```

Define commands once in Rust; expose the same validated contracts through CLI,
MCP, HTTP, OpenAPI, skills, and Agent Plugins.

## Workflow

1. Inspect `Cargo.toml` and the existing command graph. Run `incurs --help` and
   `incurs explain` rather than assuming TypeScript flags work.
2. Read [authoring](references/authoring.md) for typed commands and a minimal app.
   Use doc comments, typed inputs, and serializable outputs with JSON Schema.
3. Read [build and generation](references/build-gen.md) before packaging binaries
   or generating helpers. Use Cargo for builds; `incurs gen` is not `incur gen`.
4. Read [Code Mode](references/codemode.md) when integrating tool orchestration.
   Discover declarations first, execute against `ToolCatalog`, and preserve
   approval/cancellation/replay boundaries.
5. Validate CLI stdout and exit code together using `serve_to`; inspect
   `tool_catalog()` definitions and test non-CLI calls too. Keep structured stdout
   free of human logging.

[Command reference](references/commands.md) contains generated flags. Prefer live
`incurs <command> --help` if the installed version differs. Commands that publish,
replace files, or approve side effects require explicit user approval.

## Local naming

`incurs` is the Rust tool and skill. A local `incur` compatibility alias may point
to it; this does not make TypeScript scripts or build/gen flags compatible.
Do not migrate existing TypeScript projects implicitly.
