# Optional SQLite workspace through Docker MCP

Graf's MCP server reads your saved graph. You can separately connect a SQLite
MCP server for disposable SQL experiments. Docker is optional; Graf neither
installs this server nor uses it to store graph data.

The recipe below uses the **SQLite (Archived)** reference server, `mcp/sqlite`.
Its source is archived, so choose it only for an isolated scratch workspace.
The [Docker catalog entry](https://github.com/docker/mcp-registry/blob/main/servers/SQLite/server.yaml)
documents `--db-path /mcp/db.sqlite` and the persistent `mcp-sqlite:/mcp` volume.
The separate catalog entry `sqlite-mcp-server` is a different implementation.

## Connect through Docker Desktop

In Docker Desktop's **MCP Toolkit**, create a profile named `graf-scratch`, add
**SQLite (Archived)** from the Catalog, then connect your client from the
**Clients** tab. This uses the catalog's `mcp-sqlite` volume. Removing the server
from a profile does not delete that volume. See Docker's current
[Toolkit instructions](https://docs.docker.com/ai/mcp-catalog-and-toolkit/toolkit/)
and [profiles guide](https://docs.docker.com/ai/mcp-catalog-and-toolkit/profiles/).

For a client with manual stdio configuration, select executable `docker` and
arguments:

```json
["mcp", "gateway", "run", "--profile", "graf-scratch"]
```

Use your client's normal MCP configuration editor; its enclosing JSON format
depends on the client. Keep Graf configured separately with `graf install
--platform HOST --mcp`. Never mount `.graf/index.db` or its directory into this write-capable
SQL server.

## Use a dedicated volume without the Toolkit

An MCP client can also launch the server directly. The following image digest
was checked on Linux with two fresh containers: table creation, insertion, query,
and persistence after restarting. Docker Desktop's UI was not part of that
check. The reference server's [own documentation](https://github.com/modelcontextprotocol/servers-archived/tree/main/src/sqlite)
describes the same database-path and volume interface.

Choose executable `docker` and arguments:

```json
[
  "run", "--rm", "-i", "--network", "none",
  "--cpus", "1", "--memory", "512m", "--pids-limit", "128",
  "--mount", "type=volume,source=graf-sqlite-scratch,target=/mcp",
  "mcp/sqlite@sha256:efbc05ccace18df122f26b674bd1730c76ece716551df2b3961d519909c34696",
  "--db-path", "/mcp/scratch.db"
]
```

Docker creates the named volume on first use. The container has no network or
host-directory mount; only this scratch volume is writable. This pinned digest
was qualified on Linux/amd64. Other platforms need a compatible image; a mutable
`latest` tag is a different artifact and should be checked separately.

## Check the connection and persistence

Inspect the client's tool list, then call these tools with a `query` argument:

| Tool | SQL |
| --- | --- |
| `create_table` | `CREATE TABLE IF NOT EXISTS scratch (id INTEGER PRIMARY KEY, value TEXT NOT NULL)` |
| `write_query` | `INSERT OR REPLACE INTO scratch VALUES (7, 'example')` |
| `read_query` | `SELECT id, value FROM scratch ORDER BY id` |

The last call should return `7` and `example`. Disconnect and reconnect the MCP
server, then repeat the query to check persistence. `list_tables` and
`describe_table` are also available. These are the SQLite server's tools, not
Graf graph tools.

To stop using it, remove its client configuration or Toolkit profile entry.
Keep the volume if you want the SQL data later. To permanently discard the
dedicated volume, first stop every client using it, then explicitly run
`docker volume rm graf-sqlite-scratch`. The Toolkit recipe uses the different
volume name `mcp-sqlite`; inspect it before deleting any data.
