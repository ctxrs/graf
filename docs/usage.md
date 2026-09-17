# Graf usage

This guide covers Graf 0.3 and later. Run `graf --version` to check your version;
see the [README](../README.md#install) for installation and upgrades.

## Keep a useful local snapshot

```sh
graf index .
graf query authenticate --depth 2 --limit 50 --json
graf query authenticate --kind function --relation calls --induced-edges
graf check-update
graf update
```

`index` includes supported local documents by default; `--code-only` excludes
them from discovery. `check-update` compares local fingerprints without invoking
models or converters. `update` uses the stored root and extraction settings.
Later `index` runs also start from stored settings; use `--config FILE` to change
boolean settings back to false. Already added source facts remain available
even when document discovery is disabled.
Queries only read SQLite: they never refresh, fetch sources, or launch a model.
Use `--db PATH` to select a database; otherwise reads discover the nearest
ancestor `.graf/index.db`.

Query supports BFS by default, `--dfs`, repeated `--file`, `--kind`, and
`--context` filters, plus `--direction in|out|both` and `--relation`. Depth is
0–6 and the result limit is 1–500. `--budget` estimates tokens from JSON bytes,
not a model tokenizer. Inspect `truncated` and unresolved references when
interpreting results. `show`, `callers`, `callees`, `impact`, and `path` accept
exact IDs when names are ambiguous. `explain` aliases `show`; `affected` aliases
`impact`.

`impact` follows recorded call, import, type, and other dependency relations
backwards. A file or class also seeds its contained definitions. Repeat
`--relation calls --relation references` to narrow the relation set. Its JSON
result includes `graph` plus `seeds`: seeds are starting evidence, not affected
dependents. `show`, `impact`, and path endpoints prefer exact IDs and names,
then try Unicode/accent-normalized names, prefixes, and substrings. Punctuation
stays literal and ambiguous matches remain errors. Convenience lookup examines
at most 5,000 scoped nodes and 8 MiB of name fields; use an exact ID or narrower
`--file`/`--kind` scope when that bound is reached. File paths stay exact.

`graf watch --interval-ms 1000` is an explicit foreground polling loop. It runs
updates when local fingerprints change; it is not installed as a service.
Changes between polls are combined into the next update. A busy database or
concurrent committed update is retried at the next poll; extraction failures
still stop the command with the previous graph intact.
Updates and watch reuse any previously enabled provider/converter settings.
`index --force` and `update --force` re-extract local files for that invocation;
saved remote sources remain offline, and valid semantic cache entries remain
reusable. Add the remote source again to explicitly fetch a replacement.
`--refresh-cache` also bypasses semantic and transcript cache reads for this
invocation. It does not fetch previously saved remote sources.

If re-extraction returns fewer model-generated nodes or edges for an existing
source, Graf refuses the update and preserves the graph and source stamps.
Growth in another file cannot hide that reduction. For an intentional reduction,
use `--allow-semantic-shrink` on `index`, `update`, or `add`; Graf first saves a
portable graph snapshot in `.graf/backups`. Deleting a source normally remains
allowed. Explicit `--no-semantic` also saves a backup before removing semantic
facts. Re-adding a managed source protects its stable source identity even when
`--name` changes. Accepted reductions also preserve the previous source-cache
record, including captures awaiting a successful index. These `source-*.json`
backups are source records; `graph-*.json` backups are portable graph snapshots.
Counts detect reductions, not every possible change of meaning.
`--query-log FILE` explicitly appends read-command metadata; adding
`--log-responses` also records returned graph data. MCP does not write query logs.

Keep the SQLite index out of Git and update it after merging source changes.
Graf does not install a graph merge driver or intercept an agent's individual
read/search calls. Installed guidance and MCP tools provide graph access;
`graf merge` explicitly combines saved graphs while retaining project identities.

`graf clone https://github.com/OWNER/REPO --index` keeps a shallow checkout in
`~/.graf/repos/OWNER/REPO`; `--output DIR` selects another location. Repeating
the command reuses the checkout offline. Add `--refresh` to fetch and fast-forward
the current branch. Existing origins/branches must match, and local changes are
never reset. Git must be installed separately.

To measure a particular read workload without changing the graph:

```sh
graf benchmark --query authenticate --query main --iterations 20 --depth 1 --json
```

The benchmark uses one read-only SQLite connection and one unmeasured warm-up
per query. It reports median/p95 milliseconds, actual result counts, truncation,
generation, and query options. It times SQL reads and result construction, not
database opening or output serialization. It accepts up to 32 explicit queries,
1–1,000 iterations each, and at most 10,000 measured calls in total. No source
scan, provider call, or graph write occurs. Timings depend on machine load and
cache state; the output does not compare Graphify or other tools.

For extraction timings, use `graf index . --timing --json`,
`graf update --timing --json`, or `graf add FILE --timing --json`. The report
adds measured detection, extraction, commit, and total milliseconds; `add` also
reports source capture time. Detection includes discovery and project context
preparation. Without `--json`, index/update timing appears on stderr. This flag
is per invocation and does not change saved extraction settings.

## Input coverage

Extractors record definitions, relationships, and source locations where the
syntax supports them. Coverage varies by language and construct:

| Family | Inputs handled in this checkout |
| --- | --- |
| Python and web code | Python; JavaScript/JSX and TypeScript/TSX; Vue, Svelte, Astro |
| Systems and application code | Rust, Go, C/C++, Objective-C, Java, C#, Kotlin, Swift, Scala, Dart, Zig |
| Scripts | Ruby, PHP, Lua/Luau, shell, PowerShell, Elixir; recognized extensionless shebang scripts |
| Additional languages | Julia, Fortran, OCaml, Pascal, Common Lisp, Verilog/SystemVerilog, Groovy, Apex, BYOND DM |
| Templates and assets | Blade, Razor/CSHTML, XAML; Robot Framework's static English subset; Pascal forms/packages and BYOND map/interface/icon metadata |
| Project configuration | SQL, Terraform/HCL, package manifests, .NET project/solution files, and recognized MCP/configuration JSON |
| Local documents | Markdown/MDX/QMD/skills, text, HTML, reStructuredText, YAML, text-bearing PDF, DOCX, XLSX |

This is syntax and document extraction, not compiler-complete analysis. Graf
does not execute code, expand arbitrary macros, run preprocessors, select every
overload, or infer dynamic receiver types. CUDA uses a dedicated grammar; Metal
and C++/CLI accept bounded, position-preserving syntax adaptations. Unsupported
dialect constructs produce diagnostics. Template and Robot
support does not execute templates or test libraries. Images and audio/video
need explicit OCR, vision, or transcription adapters for content extraction.
Scanned PDFs may need a configured converter. Unrecognized files are counted as
unsupported; recognized files that cannot be parsed produce errors or diagnostics.
Check `graf stats` for coverage.

Python supports conventional packages, `src/` layouts, and explicit roots such
as `graf index . --python-source-root services/api`. Selected Go module,
JavaScript/TypeScript package/configuration, and Rust manifest/module facts help
resolve project identities. These are conservative rules, not substitutes for
the language toolchain. Python annotation evaluation and some implicit/private
bindings remain unresolved.

Static Python export lists, star imports, and literal class ancestry can add
cross-file navigation; computed exports, colliding bindings, and uncertain
inheritance stay unresolved. Java, Kotlin, and C++ member analysis treats the
selected index root as one analysis unit unless indexed build/module markers
show a split. This is a source-analysis assumption, not a compiler configuration;
ignored or unindexed build files cannot establish boundaries. C# uses the nearest
unambiguous indexed project, without evaluating MSBuild. Partial declarations
and interface navigation retain separate source records and do not assert a
runtime dispatch target.

Supply Swift module membership explicitly when cross-file navigation is needed:

```sh
graf index . --swift-module Core=Sources/Core --swift-module App=Sources/App
```

The longest matching source root determines membership. Duplicate module roots
remain ambiguous, and foreign references require visible exported declarations.
Graf does not execute `Package.swift`.

Indexing respects ignore rules and skips symlinks and common dependency/build
directories. `--no-gitignore` disables Git ignore filtering; `--include-generated`
includes normally excluded dependency/build directories. `.git` and `.graf`
remain excluded. Invalid ignore rules stop an update without replacing the last
graph. Ordinary code files over 4 MiB, invalid UTF-8, and unsupported syntax are
diagnosed instead of treated as complete source facts.

Robot files use a native static parser by default. For official Robot Framework
syntax and localized headings, select an existing interpreter containing Robot
Framework 7.5.x with `graf index --robot-python /path/to/python`. This optional
adapter reads the official syntax model; it does not run tests or import declared
libraries, variable files, or custom languages. Missing dependencies fail without
falling back. The selection is saved; ordinary queries and unchanged updates do
not start Python. Use `update --force` after changing the installed package at
the same interpreter path.

## Add documents and remote sources

```sh
graf add ./architecture.pdf --project .
graf add https://example.org/architecture --name architecture.html --project .
graf add ./planning.gdoc --google --project .
```

`add` performs the requested import and indexes the project. It saves extracted
facts under `.graf/sources/`, so later updates keep them without refetching the
URL, reopening the original document, or rerunning its conversion. Repeat `add`
with the same source to replace that source's saved facts. Adding requires the
project's native index, or creates one; it cannot append to an imported snapshot.
Optional `--contributor` and `--captured-at-unix-secs` record capture provenance.

URL imports fetch the selected source, not a recursive crawl. Private/localhost
URLs require `--allow-private-urls`. Merely indexing a Google pointer or URL
shortcut does not download its target. `--google` uses an installed, configured
`gws`; `--download-media` uses installed `yt-dlp`; `--ocr` uses Tesseract; and
`--whisper MODEL` uses installed Whisper/FFmpeg. Graf does not install or sign in
to these tools. Keep the source cache and database out of version control.

Selecting `--whisper` uses up to eight topic labels from the existing graph as
transcription hints. An empty graph supplies no hints. Use
`--whisper MODEL --whisper-prompt "domain terms"` to override them, or an empty
prompt to disable hints. The selected prompt is saved with the converter;
ordinary updates reuse it. Graf does not call a model to generate these hints.

Markdown wikilinks such as `[[Design]]` try a sibling document, then the exact
root-relative document, then a unique matching path suffix anywhere in the
indexed documents. `[[architecture/Design]]` can disambiguate a suffix; duplicate
matches remain unresolved. Anchors and display labels are retained. Ordinary
Markdown links such as `[Design](Design.md)` keep their relative-path meaning.

## Opt in to semantic extraction

Static extraction is the default. `--provider` enables model extraction for
documents; `--deep` additionally enriches code. `--code-only` controls which
files are discovered, so combining it with `--deep` still permits model calls
for code. Images are uploaded only with the separate `--vision` option.

For example, after choosing a compatible full request URL and setting the key
in your environment:

```sh
graf index . --provider open_ai --model gpt-6-astra \
  --endpoint "$GRAF_PROVIDER_URL" --key-env GRAF_API_KEY --max-semantic-files 8
graf index . --no-semantic
```

The first command sends eligible text to the selected provider. Provider settings
are stored with the index and reused by later updates; the second command
disables semantic extraction for subsequent runs. Keys remain in environment
variables, not the stored configuration. There is no default provider endpoint.
OpenAI/Azure use a chat-completions-compatible endpoint; other HTTP adapters
cover Anthropic, Gemini, and Ollama. Bedrock and Claude CLI use installed clients;
a generic CLI adapter is also available. Model/endpoint support depends on the
selected provider.

`--max-semantic-calls` and `--max-semantic-output-tokens` cap one invocation
across managed capture and local files. Retries and split attempts consume the
same allowance; validated cache hits do not. Reports expose attempted calls and
reserved output tokens, not a bill or measured provider usage. Per-file limits
and `--max-semantic-files` still apply. Advanced JSON settings include provider
temperature, thinking, and permitted extra request fields; Graf retains control
of input, authentication, and output limits.

Inspect a cache without provider calls using `graf cache inspect DIRECTORY`.
Remove a reported invalid entry with `graf cache remove DIRECTORY KEY`, then
retry extraction. `--refresh-cache` bypasses cache reads explicitly. Optional
`ingest.transcript_cache_dir` reuses converter output for unchanged media and
converter settings. Credentials belong in environment variables, not prompts
or converter arguments.

`graf provider --project . add NAME settings.json` registers a
`SemanticOptions` JSON file; `list`, `show NAME`, and `remove NAME` inspect or
manage it. Omitting `--project` explicitly manages `~/.graf/providers.json`.
Advanced indexing settings use `graf index . --config FILE`, with fields from
[`IndexOptions`](../src/index.rs) and nested
[`SemanticOptions`](../src/ingest/semantic.rs). Call, token, response-size, retry,
and splitting limits are configurable. Model-derived relationships carry
inference/evidence metadata; they are not verified compiler facts.

## Analysis and exports

```sh
graf analyze
graf communities
graf hubs --sort pagerank --top 20
graf report --output graph-report.md
graf export html --output graph.html
graf export snapshot-json --output graph.json
graf tree --output tree.html
graf diagnose multigraph --max-examples 5
graf label --output community-labels.json
```

Analysis explicitly loads the complete saved graph. It does not rebuild the
index or persist community assignments into it. `cluster-only` aliases `analyze`;
`god-nodes` aliases `hubs`. `--exclude-hubs PERCENTILE` changes partitioning and
hub eligibility; `--include-noise` includes otherwise filtered container/builtin
nodes in rankings and labels. These options do not delete topology.

`--resolution` accepts a finite positive value (default 1); larger values favor
smaller communities. `--max-community-size N` and `--min-cohesion VALUE` (0–1)
request additional splitting within the shared analysis pass budget. These are
soft targets: inspect `community_split_attempts` and
`unsatisfied_community_constraints`, rather than assuming every target was met.

For a native database, `report` includes recorded corpus coverage and marks
freshness as `not_checked` by default. `report --check-freshness` explicitly
compares current local source fingerprints, reporting `fresh`, `stale`, or
`unavailable` if the source cannot be checked. It does not update the graph or
call models/converters. This option requires a native database, not an imported
graph or JSON snapshot. Source context appears in Markdown/HTML reports and a
vault's report.md, and accompanies JSON command results; interchange artifacts
remain unchanged. Other export commands do not scan source freshness.

Export formats are `snapshot-json`, `graphify-json`, `graphml`, `cypher`,
`mermaid`, `svg`, `html`, `markdown`, `canvas`, `callflow-html`, `tree-html`,
`wiki`, and `obsidian`. `report` defaults to Markdown and accepts `--format`.
Interactive views accept `--node-limit` and `--edge-limit`; displayed limits do
not discard the full snapshot embedded in the HTML. Callflow/tree views show
recorded relationships, not an execution trace.

Wiki/Obsidian export requires an existing destination directory and creates a
fresh folder inside it, preserving existing notes:

```sh
mkdir -p notes
graf export obsidian --output notes
```

Single-file exports print the artifact unless `--output` is supplied. With
`--json`, stdout wraps an artifact as `{format,content}`. Output files are staged
and replaced atomically; explicit output can replace a prior export, but not a
source snapshot, SQLite database, or its sidecars. `--snapshot FILE` reads a Graf
JSON snapshot instead of `--db`.
Changed existing outputs, including reviewed label files, are saved by content
hash under the output directory's `.graf/export-backups` before replacement.
Identical output is left untouched. JSON graph exports reject malformed existing
graphs and reductions in node or edge counts; use `--allow-shrink` for an
intentional reduction, or choose a new filename. Backup failures leave the
previous output intact. Backups are retained until you remove them.

Diagnostics report parallel, mixed-direction, and self-loop edges plus bounded
examples of what an endpoint-only collapse would lose; they never collapse
edges. Labels are deterministic local community names. `label` reuses labels
from its existing output, or `--input FILE`, only when membership is identical.
Numeric community IDs may change. The separate label file stores BLAKE3
membership signatures, is limited to 8 MiB, and is not automatically applied to
other reports or to the source database. To use reviewed labels, pass
`--labels community-labels.json` to report/export/tree. Only rendered community
labels with exact matching membership change; stale labels are ignored, and
the source graph remains unchanged.

## Snapshots and Graphify compatibility

```sh
graf --db imported.db import graphify graphify-out/graph.json --format export
graf --db imported.db import graphify graphify-out/graph.json --format export --refresh
graf --db copy.db import graf graph.json
```

Import requires an empty database; `--refresh` atomically replaces an imported
graph and preserves the old graph on failure. Native indexes cannot be replaced
this way. Graf snapshots retain graph records and provenance, not the native
file ownership needed to resume incremental indexing.

`--format export`, also used by `switch graphify`, treats ordered source/target
endpoints as logical direction even when Graphify's root says `directed:false`;
legacy `_src`/`_tgt` markers take precedence. Raw no-cluster exports are accepted.
Export mode also recognizes legacy document/code type names and numeric
confidence or weight strings. Numeric confidence becomes `INFERRED`; converted
edge records retain their original attributes. A missing target of `imports`,
`imports_from`, or `re_exports` becomes an explicitly marked external concept.
Missing sources and other dangling edges are errors. Invalid weights or scores
are rejected instead of silently replaced.
The default `node-link` format instead honors boolean `directed`/`multigraph`
flags and supports genuinely undirected graphs. Multigraph keys distinguish
parallel records. Typed IDs, endpoints, duplicate JSON keys, and the 256 MiB
input limit are validated.

Source builds accept supported `groups`/`hyperedges` layouts as queryable group
nodes and `member_of` edges, retaining their original records. Imported metadata
and confidence are upstream assertions; importing does not verify them or read
referenced source paths. Graf-generated exports retain extra Graf records for
lossless reimport; this does not guarantee every external consumer preserves
those fields. Graf does not implement Graphify's Python internals, PR dashboard,
or agent memory/reflection workflows, and does not require graph reads before
source reads.

## Multiple projects

```sh
graf merge --project api=../api --snapshot docs=docs-graph.json --output combined.db
graf global add api ../api
graf global add worker ../worker
graf global query authenticate --depth 2
graf global refresh
graf global list
graf global remove worker
```

Merge requires at least two named databases/projects or Graf snapshots and a new
output database. IDs remain distinct through project namespaces. Global
registration stores named paths and an aggregate at `~/.graf/global.db`;
`--db PATH` overrides that location. Use `global add NAME FILE --snapshot` for
JSON. Only add/remove/refresh rebuild the aggregate. List and query use saved
SQLite data even if source projects are unavailable. If a rebuild cannot read
any required source, the previous graph and registry remain intact. Unchanged
aggregate content, including its provenance, skips the generation/write.

Global aggregates link distinct package nodes with undirected `same_package`
edges only for exact canonical `package:<ecosystem>:<name>` binding keys.
Versions and original nodes remain separate; these links are not cross-project
calls. Merge enables these links only with `--link-packages`. Basenames and
ordinary symbol names are not used to infer shared identity.

Global rebuilds also link saved unresolved Java, C#, Kotlin, and C++ references
when an exact qualified binding has one public target and compatible type or
static-call evidence. Merge opts in with `--link-references`. These inferred
edges retain their original reference and project evidence; nodes are never
collapsed. Private, ambiguous, dynamic, and file-relative bindings remain
unresolved. Linking uses saved facts and does not evaluate a compiler or build
configuration.

## Agent setup and MCP

```sh
graf install --platform codex --project . --skill --mcp
graf uninstall --platform codex --project . --skill --mcp
graf install --platform aider --project . --skill
graf hook install --project .
graf hook status --project .
graf hook uninstall --project .
```

Setup defaults to project scope and guidance/skills. `--mcp` selects MCP only;
use both flags for both components. `--global` explicitly selects user scope.
The `agents` platform uses the portable Agent Skills layout. Named host routes
include Claude, Codex, Cursor, Gemini, OpenCode, Copilot/VS Code, Kilo, and other
hosts supported by `install`. Aider adds the guidance file to its YAML `read`
setting. Native MCP setup is implemented for
Claude, Codex, Cursor, Gemini, and VS Code. Other hosts can use
skills with manual `graf serve` configuration.

Setup preserves unrelated configuration and tracks its changes for uninstall.
Uninstall restores only receipt-owned files and refuses later edits. VS Code
MCP files can contain comments and trailing commas; Gemini settings can contain
comments. Setup preserves those bytes without reformatting the file. Claude and
Cursor MCP configurations currently require strict JSON. Global MCP relies on
the host starting Graf in an indexed project; explicit `--db` in a manual
configuration removes that dependency. Codex project MCP requires a trusted
project.

For VS Code global MCP, run **MCP: Open User Configuration** and pass the
directory containing that profile's `mcp.json` to
`graf install --platform vscode --global --profile "/path/to/profile" --mcp`.
The directory must already exist; Graf does not resolve profile names or select
the host's active profile. Global VS Code skills use `~/.copilot/skills` and are
shared across profiles. [VS Code profile configuration](https://code.visualstudio.com/docs/copilot/customization/mcp-servers)
and [personal skills](https://code.visualstudio.com/docs/copilot/customization/agent-skills)
describe these separate locations.

`--config-root "/path/to/root" --global` selects an existing native Claude,
Codex or Hermes root. Select that same root in the host through
`CLAUDE_CONFIG_DIR`, `CODEX_HOME` or `HERMES_HOME`; Graf does not change those
variables. For example:

```sh
graf install --platform claude --global --config-root "/path/to/claude-profile" --skill --mcp
graf uninstall --platform claude --global --config-root "/path/to/claude-profile" --skill --mcp
```

Claude skills and `CLAUDE.md` go directly under the selected root, with skills in
`skills/graf/SKILL.md`. MCP uses that root's existing `.config.json` when present,
otherwise `.claude.json`; the default home profile is left untouched.
Codex configuration and
`AGENTS.md` go in that root, while its personal skill stays in
`~/.agents/skills`. Hermes skills go in the selected root's `skills` directory.
Without an override, Hermes uses `~/.hermes` on Unix and
`%LOCALAPPDATA%\hermes` on Windows (falling back to
`%USERPROFILE%\AppData\Local\hermes`). A set `CLAUDE_CONFIG_DIR`, `CODEX_HOME` or
`HERMES_HOME` requires explicit `--config-root` for installation. Custom roots
for other hosts are not supported. Use the same root/profile
flags for reinstall and uninstall; their receipts live in the selected
directory.

`graf install --platform cursor --global --skill` installs an on-demand local
skill in `~/.cursor/skills`. Cursor's always-on global User Rules remain managed
in **Customize → Rules**; Graf does not edit that UI storage or enable cloud
sync. Project installs retain `.cursor/rules/graf.mdc`.
[Cursor documents the distinction between skills and User Rules.](https://cursor.com/docs/context/skills)

Installed guidance and its receipt carry a guidance version. Rerunning install
with the same platform, scope, and component flags upgrades unchanged owned
guidance while retaining the original bytes for uninstall. User-edited guidance
is refused. An interrupted guidance upgrade can be resumed with install or
reversed with uninstall using the same selection.

Ordinary CLI use checks only the first 4 KiB of known Graf skill files in the
selected project and personal skill locations, including active Claude/Hermes
environment roots. Older installed guidance gets a stderr notice to rerun
install with the same selection; newer guidance gets advice to upgrade Graf.
The comparison uses numeric release versions and guidance revisions; prerelease
and build suffixes do not change compatibility. Matching, missing, unreadable
or malformed stamps are silent. Checks do not scan source, read setup receipts,
write files or change JSON output. Reinstall refreshes an older executable stamp
without losing original undo bytes and refuses to downgrade newer guidance.

Hooks are optional foreground refreshes after commit, branch checkout, and
merge. They chain existing executable hooks, preserve their exit status, and
never stage or commit graph files. They run `update` only when `.graf/index.db`
exists and therefore reuse its extraction settings. Setup installs no watcher
or service and imposes no restriction on reading source files.

MCP defaults to stdio. To explicitly start HTTP or register another database:

```sh
graf --db .graf/index.db serve --transport http --port 8080
graf --db .graf/index.db serve --project worker=../worker/.graf/index.db
```

HTTP defaults to loopback at `/mcp`. Nonloopback binds require
`--bearer-token-env NAME`; it is the name of an environment variable, not a
literal token. There is no built-in TLS; use a suitable TLS endpoint for remote
access. `--allowed-host HOST:PORT` adds exact HTTP Host authorities. Tools select
registered project names, not arbitrary filesystem paths.

Alongside Graf navigation tools, the server exposes `query_graph`, `get_node`,
`get_neighbors`, `shortest_path`, `graph_stats`, `god_nodes`, and `get_community`,
plus report/graph/community resources and default-project `graphify://` resource
aliases. Analysis is cached per generation and bounded to 5,000 nodes, 20,000
edges, a 64 MiB database, and an 8 MiB snapshot. Use explicit CLI analysis/export
for larger graphs. MCP remains read-only and never invokes providers or updates.

## Database connectors

For an independent writable SQL scratch workspace alongside Graf's read-only
MCP tools, see the optional [Docker SQLite guide](docker-sqlite.md).

These commands explicitly contact a database using an already installed client:

```sh
# Uses pg_dump and standard PG* environment settings; saves schema facts only.
graf introspect postgres --name catalog --project .
graf index .

# Connection variables must already be set; these commands write remotely.
graf push neo4j --uri-env NEO4J_URI --database graf
graf push falkordb --uri-env FALKORDB_URI --graph graf --password-env FALKORDB_PASSWORD
```

PostgreSQL introspection also accepts `--dsn-env NAME`. It extracts schema facts,
not table rows, and requires a separate index/update to include them in the
graph. Neo4j uses `cypher-shell` with `NEO4J_USERNAME`/`NEO4J_PASSWORD` by default;
FalkorDB uses `redis-cli`. Their connection URLs must not embed credentials.
Both push commands accept `--db` to select the local graph and explicit timeout
and output bounds.

Neo4j submits one transaction; a lost acknowledgment can leave its commit
outcome uncertain. FalkorDB commits statements individually, so failures may
leave a partial import. Neither retries automatically. Push scopes records to
the snapshot: repeating the same snapshot uses upserts, while a changed snapshot
gets a new scope and leaves earlier imports in place. Use
`graf export cypher --output graph.cypher` to generate a local artifact instead.
