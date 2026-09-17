# graf

A persistent local code graph, written in Rust. Index Python code or import a Graphify graph, then find symbols, inspect callers, and follow dependencies from your terminal or an AI agent.

Graf stores its graph in SQLite. Queries use persistent search and adjacency indexes; they do not reload graph JSON or rebuild an in-memory graph.

## Install

On Linux or macOS:

```sh
curl -fsSL https://raw.githubusercontent.com/ctxrs/graf/main/install.sh | sh
```

On Windows x64, in PowerShell:

```powershell
irm https://raw.githubusercontent.com/ctxrs/graf/main/install.ps1 | iex
```

The installers verify the signed release manifest and download hashes before
installing. Linux and macOS default to `~/.local/bin`; Windows defaults to
`%LOCALAPPDATA%\Graf\bin`. Add that directory to your `PATH` if needed; the
installers do not change shell profiles. Run the installer again to upgrade.

Linux x64/ARM64, macOS Intel/Apple Silicon, and Windows x64 are supported.
See [installation and download verification](docs/downloads.md) for prerequisites,
version selection, custom directories, and manual downloads from
[Releases](https://github.com/ctxrs/graf/releases).

To build from source, use Rust 1.90 or newer and a C compiler:

```sh
cargo install --git https://github.com/ctxrs/graf --locked graf-cli
```

The executable is `graf`. Indexing and queries run locally without API keys, model calls, or a background service.

## Use

From a Python project:

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

Use the returned node ID when a name is ambiguous. `--json` produces structured results; `--db PATH` selects an explicit index. The default database is `.graf/index.db`; add `.graf/` to your project's ignore file. Read commands find the nearest existing index in the current directory or its ancestors.

```sh
graf query authenticate --depth 2 --limit 50 --json
# After editing, adding, or deleting source files:
graf update
```

Updates hash source files and parse only changed files. Changed definitions also update affected references in unchanged files. Each completed update commits one coherent graph generation. A no-op update preserves the generation. Queries read the last indexed state and do not check whether the worktree has changed.

## Native indexing

Native indexing covers Python definitions, containment, imports, and syntactic call sites. It supports conventional packages and `src/` layouts. It respects ignore rules, skips symlinks and common dependency/build directories, and does not execute project code.

Call resolution is deliberately conservative: lexical functions and explicit local import aliases can resolve to definitions. Dynamic dispatch, ambiguous bindings, and unsupported import patterns remain unresolved. Results include source locations and unresolved references; an empty caller list does not prove a function is unused. Tree-sitter syntax errors and duplicate parameters are diagnosed, and their old facts are removed on update. Graf does not validate every Python compiler or type-system rule. Annotation evaluation is omitted from call edges; class-private names and implicit `__class__` calls remain unresolved. Files larger than 4 MiB, non-UTF-8 source, and excessively nested syntax are also diagnosed instead of indexed.

## Switch from Graphify

From a project with `graphify-out/graph.json`:

```sh
graf switch graphify
```

Or install Graf and switch in one command on Linux or macOS:

```sh
curl -fsSL https://raw.githubusercontent.com/ctxrs/graf/main/install.sh | sh -s -- --from graphify
```

Graf imports the snapshot into `.graf/index.db`, replaces the project's Graphify
MCP connection with Graf, and verifies a query over MCP. Restart your agent client
to load the new tools. The original graph, Graphify installation, generation
skills, and hooks remain available.

The command finds project `.mcp.json`, `.cursor/mcp.json`, and `.vscode/mcp.json`
configurations. If none exists, it creates `.mcp.json`. A supported connection
runs `python -m graphify.serve` (including a Python executable path or `uv run`)
over stdio. When several connections match, select one explicitly:

```sh
graf switch graphify --config .cursor/mcp.json --server graphify
# Select a project or a nondefault snapshot:
graf switch graphify --project /path/to/project --graph exports/graph.json
# Global JSON or Codex TOML configurations require an explicit path:
graf switch graphify --config /path/to/config.toml
```

MCP JSON must be strict JSON with `mcpServers` or VS Code's `servers` map; Codex
TOML uses `mcp_servers`. Comments in TOML and unrelated configuration values are
preserved. JSON with comments, HTTP servers, shell wrappers, and disabled
connections are not switched automatically. Graf never runs the old server's
command or changes a global config without `--config`.

Repeat the command to verify the existing migration. It keeps the imported
snapshot; it does not refresh from a changed Graphify graph. Undo restores the
exact saved MCP configuration and retains the database:

```sh
graf switch --undo
```

Use the same `--project` and `--config` options when supplied. Undo refuses to
overwrite configuration edited since switching. Backups are stored locally under
`.graf/` and ignored by Git. An existing `.graf/index.db` is never replaced.

Graf has its own commands and MCP tools. It is not a drop-in replacement for
Graphify's CLI, Python API, rankings, reports, visualization, or semantic
extraction. Keep Graphify for generating graphs from languages and documents
that Graf does not index natively.

To import a snapshot without changing an agent configuration:

```sh
graf --db imported.db import graphify graphify-out/graph.json --format export
graf --db imported.db query authentication
```

Imported graphs preserve upstream metadata and confidence assertions; Graf does
not verify those assertions or read files referenced by the import. Re-import
into a new database to refresh a snapshot. There is no synchronization back to
Graphify.

Switching uses Graphify export semantics: ordered source/target endpoints describe
logical direction even when the root says `directed: false`; legacy `_src`/`_tgt`
markers take precedence. Raw no-cluster exports are also accepted.

Without `--format export`, the import command retains the strict node-link
interpretation from Graf 0.1, including genuinely undirected graphs. That
importer accepts node-link JSON with boolean `directed` and `multigraph`,
`nodes`, and exactly one of `links` or legacy `edges`. Parallel edges require
distinct keys in a multigraph. It preserves edge direction and validates
endpoints. String IDs remain unchanged; integer IDs become
`graphify:integer:<value>`, with collisions rejected. The maximum snapshot size
is 256 MiB. Unsupported hyperedges and ambiguous or malformed structures fail
explicitly.

## Use with an agent

Run `graf serve` in an indexed project, or give the server an explicit database. Add this entry to your MCP client's server configuration:

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

The stdio server exposes Graf's query, show, callers, callees, impact, path, and stats tools. It is read-only; run `graf update` explicitly after changes. CLI and MCP share the same query engine and structured result format. Query depth and result limits bound traversal; `truncated` signals when a result or path search is incomplete. Results include edges examined during traversal, not every possible edge among the returned nodes; nodes at the depth boundary are not expanded.

## Development

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
```

Validation runs locally. GitHub Actions and Buildkite are not required.

Licensed under Apache-2.0. Graf is an independent project inspired by [Graphify](https://github.com/Graphify-Labs/graphify).
