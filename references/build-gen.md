# Build and generate

Adapted from the local incur-build and incur-gen skills: discover live flags, keep build identity explicit, select targets deliberately, and inspect generated contracts. Their TypeScript commands are not Rust commands.

## Build standalone binaries

```sh
cargo build --locked --release --bin my-cli
# Cross-compile only after installing the target and configuring its linker:
cargo build --locked --release --bin my-cli --target aarch64-apple-darwin
```

The artifact is in `target/release/` (or `target/<triple>/release/`). Rust has no
`incurs build` command. TypeScript `--installer`, `--repository`, `--tag`,
`--name`, `--version`, and `--output` build flags are not accepted here.
Set name/version in Cargo.toml; use your release tooling for installers and
checksums. Never assume a public repository or publish without approval.

To install this checkout: `cargo install --locked --path crates/incurs-cli --bin incurs`.

## Generate Rust helpers

```sh
incurs gen --help
incurs gen --dir ./my-cli --entry my-cli --config-schema
# Or interrogate an already-built executable:
incurs gen --dir ./my-cli --entry /absolute/path/to/my-cli \
  --output src/incurs_generated.rs --json-output incurs.manifest.json
```

`--dir` selects the Cargo project; `--entry` is a Cargo binary name or executable,
not a TypeScript entry file. Defaults produce `src/incurs_generated.rs` and
`incurs.manifest.json`. `--config-schema` also emits `config.schema.json`;
the TypeScript spelling `--configSchema` is not supported.

Generation executes the target CLI to inspect `--llms-full --format json` and
optionally `--config-schema --format json`: only run trusted targets. Inspect
output paths before overwriting, review the diff, and compile/test the consumer.
For portable packages use the target app's `plugin build --help`, or the
`incurs gen --plugin-output` options in [commands](commands.md). Packaging and
installing are separate from publishing.
