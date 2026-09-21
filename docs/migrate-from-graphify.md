# Migrate from Graphify

Graf can import an existing Graphify snapshot and replace a supported project MCP connection without deleting the old setup.

## Install and switch in one command

Run this from a project containing `graphify-out/graph.json`.

Linux or macOS:

```bash
curl -fsSL https://raw.githubusercontent.com/ctxrs/graf/main/install.sh | sh -s -- --from graphify
```

Windows PowerShell:

```powershell
$env:GRAF_FROM = 'graphify'
try {
    irm https://raw.githubusercontent.com/ctxrs/graf/main/install.ps1 | iex
} finally {
    Remove-Item Env:GRAF_FROM -ErrorAction SilentlyContinue
}
```

The installer verifies and installs the latest Graf release, then runs `graf switch graphify` from the current directory.

If Graf is already installed:

```bash
graf switch graphify
```

## What Graf changes

The switch command:

1. imports the Graphify snapshot into `.graf/index.db`;
2. finds a supported project Graphify MCP connection;
3. replaces that connection with Graf; and
4. starts Graf over MCP and verifies a query.

Restart your agent client after the switch so it loads the new command.

Graf leaves the original snapshot, Graphify installation, generated skills, and hooks in place. It never runs the old Graphify server command. An existing `.graf/index.db` is never replaced.

## Select a custom graph or MCP configuration

The command finds project `.mcp.json`, `.cursor/mcp.json`, and `.vscode/mcp.json` files, or creates `.mcp.json` when none exists. Supported Graphify connections run `python -m graphify.serve` over stdio, including a Python executable path or `uv run`.

Choose an ambiguous connection or custom path explicitly:

```bash
graf switch graphify --config .cursor/mcp.json --server graphify
graf switch graphify --project /path/to/project --graph exports/graph.json
graf switch graphify --config /path/to/config.toml
```

JSON configuration must be strict JSON with an `mcpServers` map or VS Code `servers` map. Codex TOML uses `mcp_servers`; Graf preserves TOML comments and unrelated values. JSON with comments, HTTP servers, shell wrappers, and disabled connections are not switched automatically. Global configurations require an explicit `--config` path.

## Undo the MCP change

```bash
graf switch --undo
```

Pass the same `--project` and `--config` options used during the switch. Undo restores the exact saved MCP configuration and keeps the imported Graf database. It refuses to overwrite configuration edited after migration.

Backups and the migration receipt stay under `.graf/`, which should remain outside version control.

## Compatibility

Graf imports Graphify node-link snapshots, groups, relationships, metadata, provenance, and preserved community identities. It also exposes selected Graphify-compatible MCP tool and resource names.

Graf has its own CLI, extraction rules, analysis methods, and storage format. It is not a drop-in replacement for Graphify's Python API, and changes made in Graf do not synchronize back to Graphify. See [snapshots and Graphify compatibility](usage.md#snapshots-and-graphify-compatibility) for import formats, refresh behavior, direction rules, and group records.
