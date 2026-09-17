# graf

A persistent local code graph, written in Rust. Index Python code or import a Graphify graph, then find symbols, inspect callers, and follow dependencies from your terminal or an AI agent.

Graf stores its graph in SQLite. Queries use persistent search and adjacency indexes; they do not reload graph JSON or rebuild an in-memory graph.

## Install

Download a prebuilt executable from [Releases](https://github.com/ctxrs/graf/releases).
Linux x64/ARM64, macOS Intel/Apple Silicon, and Windows x64 are supported release
targets. See [download verification](docs/downloads.md) for platform requirements,
signatures, checksums, and installation instructions.

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

The first release indexes Python definitions, containment, imports, and syntactic call sites. It supports conventional packages and `src/` layouts. It respects ignore rules, skips symlinks and common dependency/build directories, and does not execute project code.

Call resolution is deliberately conservative: lexical functions and explicit local import aliases can resolve to definitions. Dynamic dispatch, ambiguous bindings, and unsupported import patterns remain unresolved. Results include source locations and unresolved references; an empty caller list does not prove a function is unused. Tree-sitter syntax errors and duplicate parameters are diagnosed, and their old facts are removed on update. Graf does not validate every Python compiler or type-system rule. Annotation evaluation is omitted from call edges; class-private names and implicit `__class__` calls remain unresolved. Files larger than 4 MiB, non-UTF-8 source, and excessively nested syntax are also diagnosed instead of indexed.

## Migrate from Graphify

Graf has its own commands and MCP tools. It is not a drop-in replacement for Graphify's CLI, Python API, rankings, reports, visualization, or semantic extraction.

Import a supported Graphify node-link graph into a new database:

```sh
graf --db imported.db import graphify graphify-out/graph.json
graf --db imported.db query authentication
graf --db imported.db stats
```

Imported graphs are snapshots, separate from native Python indexes. They can contain nodes from other languages or documents. Graf preserves upstream metadata and confidence assertions; it does not verify them or read files referenced by the import. Re-import into a new database to refresh a snapshot.

The importer accepts node-link JSON with boolean `directed` and `multigraph`, `nodes`, and exactly one of `links` or legacy `edges`. Parallel edges require distinct keys in a multigraph. It preserves edge direction and validates endpoints. String IDs remain unchanged; integer IDs become `graphify:integer:<value>`, with collisions rejected. The maximum snapshot size is 256 MiB. Unsupported hyperedges and ambiguous or malformed structures fail explicitly. Graf does not currently export Graphify graphs or synchronize changes back to Graphify.

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

The stdio server exposes Graf's query, show, callers, callees, impact, path, and stats tools. It is read-only; run `graf update` explicitly after changes. CLI and MCP share the same query engine and structured result format. Query depth and result limits bound traversal; `truncated` signals when a result or path search is incomplete.

## Development

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
```

Validation runs locally. GitHub Actions and Buildkite are not required.

Licensed under Apache-2.0. Graf is an independent project inspired by [Graphify](https://github.com/Graphify-Labs/graphify).
