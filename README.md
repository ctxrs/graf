# graf

A persistent local code graph, written in Rust. Index a project or import a
Graphify snapshot, then find symbols, inspect callers, and follow dependencies
from your terminal or an AI agent.

Graf stores its graph in SQLite. Navigation queries use persistent search and
adjacency indexes; they do not reload graph JSON, rebuild the graph, or scan for
source changes. Run an explicit update when you want a new snapshot.

Graf 0.3 adds language and document extraction, graph analysis, exports, and
agent integrations. See the [usage guide](docs/usage.md) for supported workflows
and their limits.

## Install

On Linux or macOS:

```sh
curl -fsSL https://raw.githubusercontent.com/ctxrs/graf/main/install.sh | sh
```

On Windows x64, in PowerShell:

```powershell
irm https://raw.githubusercontent.com/ctxrs/graf/main/install.ps1 | iex
```

The installers verify the signed release manifest and download hashes, decompress
gzip releases, and verify the executable's signed hash and size before installing.
Linux and macOS need `curl`, `openssl`, `gzip`, and standard shell utilities;
Windows needs PowerShell 5.1 or newer. Linux and macOS default to `~/.local/bin`;
Windows defaults to
`%LOCALAPPDATA%\Graf\bin`. Add that directory to your `PATH` if needed; the
installers do not change shell profiles. Run the installer again to upgrade.

Release downloads cover Linux x64/ARM64, macOS Intel/Apple Silicon, and Windows
x64. See [installation and download verification](docs/downloads.md) for
prerequisites, version selection, custom directories, and manual downloads from
[Releases](https://github.com/ctxrs/graf/releases).

For manual downloads, choose your platform's gzip file:

| Platform | Download |
| --- | --- |
| Linux x64 | [graf-linux-x64.gz](https://github.com/ctxrs/graf/releases/latest/download/graf-linux-x64.gz) |
| Linux ARM64 | [graf-linux-aarch64.gz](https://github.com/ctxrs/graf/releases/latest/download/graf-linux-aarch64.gz) |
| macOS Intel | [graf-macos-x64.gz](https://github.com/ctxrs/graf/releases/latest/download/graf-macos-x64.gz) |
| macOS Apple Silicon | [graf-macos-arm64.gz](https://github.com/ctxrs/graf/releases/latest/download/graf-macos-arm64.gz) |
| Windows x64 | [graf-windows-x64.exe.gz](https://github.com/ctxrs/graf/releases/latest/download/graf-windows-x64.exe.gz) |

Follow the [download verification and extraction guide](docs/downloads.md#verify-the-download)
before running a manual download. Each platform also has an SBOM and license
notices. The installers continue to support older releases with raw executables.

### Build from source

Use Rust 1.90 or newer and a C compiler. From a Graf source checkout containing
the features you need:

```sh
cargo install --path . --locked
```

The executable is `graf`. A build from this checkout includes the workflows in
[the usage guide](docs/usage.md). Default static indexing and navigation need no
API key, model, or background service. Explicit semantic extraction and external
source adapters have their own requirements.

## Quick start

Graf indexes the language families and documents described in
[input coverage](docs/usage.md#input-coverage).
From your project directory:

```sh
graf index .
graf query authenticate
graf callers authenticate
graf callees authenticate
graf impact authenticate
graf path main authenticate
graf show authenticate
graf stats
```

Use the returned node ID when a name is ambiguous. `--json` produces structured
results; `--db PATH` selects an explicit database. The default is
`.graf/index.db`. Keep `.graf/` out of version control. Read commands find the
nearest existing index in the current directory or its ancestors.

```sh
graf query authenticate --depth 2 --limit 50 --json
# After editing, adding, or deleting source files:
graf update
```

Updates compare source hashes and replace changed facts in one coherent graph
generation, including affected references in unchanged files. A no-op update
preserves the generation. Queries continue to read the saved state until you
update; they do not judge whether it is fresh.

Call edges describe what the extractors can resolve from source. Dynamic
dispatch, ambiguous bindings, and unsupported constructs can remain unresolved.
Inspect diagnostics and source locations: an empty caller list does not prove a
function is unused. Graf does not run your project or replace its compiler.

## Switch from Graphify

From a project with `graphify-out/graph.json`:

```sh
graf switch graphify
```

Or install the released Graf and switch in one command on Linux or macOS:

```sh
curl -fsSL https://raw.githubusercontent.com/ctxrs/graf/main/install.sh | sh -s -- --from graphify
```

Graf imports the snapshot into `.graf/index.db`, replaces the project's
supported Graphify MCP connection with Graf, and verifies a query over MCP.
Restart your agent client to load the new tools. The original graph, Graphify
installation, generation skills, and hooks remain available.

The command finds project `.mcp.json`, `.cursor/mcp.json`, and `.vscode/mcp.json`
configurations, or creates `.mcp.json` when none exists. Supported connections
run `python -m graphify.serve` over stdio, including a Python executable path or
`uv run`. Select ambiguous connections or other configuration paths explicitly:

```sh
graf switch graphify --config .cursor/mcp.json --server graphify
graf switch graphify --project /path/to/project --graph exports/graph.json
graf switch graphify --config /path/to/config.toml
```

JSON must be strict JSON with `mcpServers` or VS Code's `servers` map; Codex TOML
uses `mcp_servers`. TOML comments and unrelated configuration values are
preserved. JSON with comments, HTTP servers, shell wrappers, and disabled
connections are not switched automatically. Global configurations require an
explicit `--config` path. Graf never runs the old server command.

Repeating the switch verifies the migration without refreshing its graph. An
existing `.graf/index.db` is never replaced. Undo restores the exact saved MCP
configuration and retains the imported database:

```sh
graf switch --undo
```

Supply the same `--project` and `--config` when used for the switch. Undo refuses
to overwrite later configuration edits. Backups remain under `.graf/` and are
ignored by Git.

Graf has its own CLI, analysis methods, and extraction behavior. Snapshot import
and selected MCP compatibility names do not make it a drop-in replacement for
Graphify's Python API or every workflow. There is no synchronization back to
Graphify. See [snapshot import and compatibility](docs/usage.md#snapshots-and-graphify-compatibility)
for explicit refresh, direction rules, and supported group records.

## Use with an agent

Point an MCP client at Graf's read-only stdio server:

```json
{
  "mcpServers": {
    "graf": {
      "command": "graf",
      "args": ["--db", "/path/to/project/.graf/index.db", "serve"]
    }
  }
}
```

The client launches the server. CLI and MCP navigation share the query engine;
the server does not index, refresh, or call a model. Depth and result limits
bound traversal, and `truncated` marks incomplete results. Default traversal
returns examined edges, not every edge among returned nodes.

Graf also offers [reversible agent setup](docs/usage.md#agent-setup-and-mcp),
optional Git refresh hooks, Streamable HTTP, and named project routing. Other
commands cover [analysis and exports](docs/usage.md#analysis-and-exports),
[stored cross-project graphs](docs/usage.md#multiple-projects), and
[explicit database connectors](docs/usage.md#database-connectors).

## Development

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
```

Validation runs locally. GitHub Actions and Buildkite are not required.

Licensed under Apache-2.0. Graf is an independent project inspired by
[Graphify](https://github.com/Graphify-Labs/graphify).
