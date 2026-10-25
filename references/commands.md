# Command reference

# incurs

## incurs explain

### incurs explain

Explain how to build an incurs CLI in Rust

#### Arguments

| Name | Type | Required | Description |
|------|------|----------|-------------|
| `topic` | `string` | no | Topic name. Omit to list every topic. |

#### Output

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `topics` | `array` | yes | One topic when named, every topic summary otherwise. |

#### Examples

```sh
# List every topic
incurs explain 

# Read one topic
incurs explain typed-commands
```

> Run without a topic to list every topic.

## incurs gen

### incurs gen

Generate Rust command types and JSON manifests

#### Options

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--dir` | `string` | `"."` | Cargo project root. |
| `--entry` | `string` |  | Cargo binary name or executable path. |
| `--output` | `string` |  | Rust output path, relative to `--dir` unless absolute. |
| `--json-output` | `string` |  | JSON manifest output path, relative to `--dir` unless absolute. |
| `--config-schema` | `boolean` | `false` | Also generate `config.schema.json`. |
| `--plugin-output` | `string` |  | Also build an Agent Plugins 1.0 directory through the target CLI. |
| `--plugin-skill-depth` | `number` | `1` | Skill grouping depth for the generated Agent Plugin. |
| `--plugin-no-mcp` | `boolean` | `false` | Skip `mcp.json` generation. |
| `--plugin-bundle-cli` | `boolean` | `false` | Bundle the target CLI as the plugin Tool Runtime. |
| `--plugin-force` | `boolean` | `false` | Replace existing owned plugin artifacts. |

#### Output

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `dir` | `string` | yes | Resolved Cargo project root. |
| `output` | `string` | yes | Generated Rust module. |
| `manifest` | `string` | yes | Generated canonical JSON manifest. |
| `configSchema` | `unknown` | no | Generated config schema, when `--config-schema` was passed. |
| `plugin` | `unknown` | no | Generated Agent Plugin directory, when `--plugin-output` was passed. |

#### Examples

```sh
# Generate into a project directory
incurs gen --dir ./my-cli

# Use a built executable instead of cargo run
incurs gen --entry target/debug/my-cli

# Also write config.schema.json
incurs gen --config-schema
```

> Run this from the Cargo project whose CLI you want typed helpers for.

## incurs plugin

Install, validate, connect, or call an Agent Plugins 1.0 directory

### incurs plugin call

Invoke one namespaced tool with flat JSON arguments

#### Arguments

| Name | Type | Required | Description |
|------|------|----------|-------------|
| `path` | `string` | yes | Path to the Agent Plugin directory. |
| `tool` | `string` | yes | Namespaced tool name to invoke. |

#### Options

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--data-dir` | `string` |  | Dedicated persistent writable directory for this plugin instance. |
| `--arguments` | `string` |  | Flat JSON object passed to the selected tool. |

#### Output

Type: `unknown`

#### Examples

```sh
# Call a tool on a loaded plugin
incurs plugin call ./demo-plugin server_ping --data-dir ./plugin-data
```

### incurs plugin install

Install a portable plugin and its declared shell command

#### Arguments

| Name | Type | Required | Description |
|------|------|----------|-------------|
| `path` | `string` | yes | Path to the Agent Plugin directory to install. |

#### Options

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--bin-dir` | `string` |  | Managed user command directory. |
| `--force` | `boolean` | `false` | Replace artifacts owned by the same installed plugin. |

#### Output

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `name` | `string` | yes | Installed plugin name. |
| `version` | `unknown` | no | Installed plugin version, when declared. |
| `pluginRoot` | `string` | yes | Managed package directory. |
| `dataRoot` | `string` | yes | Persistent plugin-data directory. |
| `command` | `unknown` | no | Installed command path, when the package contains a Tool Runtime. |
| `pathReady` | `boolean` | yes | Whether the command directory is already on `PATH`. |
| `warning` | `unknown` | no | Actionable `PATH` warning, when needed. |

#### Examples

```sh
# Install into the managed command directory
incurs plugin install ./demo-plugin
```

### incurs plugin tools

Connect every valid MCP server and print namespaced tools

#### Arguments

| Name | Type | Required | Description |
|------|------|----------|-------------|
| `path` | `string` | yes | Path to the Agent Plugin directory. |

#### Options

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--data-dir` | `string` |  | Dedicated persistent writable directory for this plugin instance. |

#### Output

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `diagnostics` | `array` | yes | Every diagnostic produced while loading. |
| `name` | `string` | yes | Plugin name. |
| `servers` | `array` | yes | Connection result per declared server. |
| `tools` | `unknown` | yes | Namespaced tool definitions, in stable name order. |

#### Examples

```sh
# List every tool the plugin exposes
incurs plugin tools ./demo-plugin --data-dir ./plugin-data
```

### incurs plugin uninstall

Remove installer-owned plugin and command artifacts

#### Arguments

| Name | Type | Required | Description |
|------|------|----------|-------------|
| `name` | `string` | yes | Installed plugin name. |

#### Options

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--purge` | `boolean` | `false` | Remove persistent plugin data during uninstall. |

#### Output

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `name` | `string` | yes | Removed plugin name. |
| `pluginRoot` | `string` | yes | Former managed package directory. |
| `dataRoot` | `string` | yes | Persistent plugin-data directory. |
| `command` | `unknown` | no | Removed command path, when the package contained a Tool Runtime. |
| `purgedData` | `boolean` | yes | Whether persistent plugin data was removed. |

#### Examples

```sh
# Remove a plugin and its persistent data
incurs plugin uninstall demo-tools --purge
```

> Confirm with the user before executing this destructive command.

### incurs plugin validate

Load all fixed components and print diagnostics

#### Arguments

| Name | Type | Required | Description |
|------|------|----------|-------------|
| `path` | `string` | yes | Path to the Agent Plugin directory. |

#### Options

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--data-dir` | `string` |  | Dedicated persistent writable directory for this plugin instance. |

#### Output

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `diagnostics` | `array` | yes | Every diagnostic produced while loading. |
| `plugin` | `unknown` | no | The loaded plugin, when the manifest is valid. |
| `valid` | `boolean` | yes | Whether the manifest loaded. |

#### Examples

```sh
# Validate without connecting any server
incurs plugin validate ./demo-plugin --data-dir ./plugin-data
```
