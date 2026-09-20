# Graf usage

This guide targets Graf 0.4.0. Its workflow additions require Graf 0.4.0 or a
build from this checkout; installing 0.3.0 does not enable them. Installers fetch
the latest published release by default, which may precede this checkout.
See the [README](../README.md#install) for release installation and
[building from source](../README.md#build-from-source).

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
By default, queries only read SQLite: they never refresh, fetch sources, or launch
a model. Explicit learning annotations also check cited local files.
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
Graf does not install a graph merge driver. Installed guidance and MCP tools
provide graph access; [optional tool hooks](#optional-tool-guidance)
can also suggest it before supported reads/searches, without requiring a graph
read first. `graf merge` explicitly combines saved graphs while retaining project
identities.

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
overload, or infer dynamic receiver types. Reassigned function pointers and
dynamic dispatch can remain unresolved; a static edge is not proof of the target
chosen at runtime. CUDA uses a dedicated grammar; Metal
and C++/CLI accept bounded, position-preserving syntax adaptations. Unsupported
dialect constructs produce diagnostics. Template and Robot
support does not execute templates or test libraries. Images and audio/video
need explicit OCR, vision, or transcription adapters for content extraction.
PDF text extraction does not preserve page layout. Scanned PDFs and unsupported
font encodings need a configured converter. Symbol and ZapfDingbats fonts need an
explicit supported encoding or a ToUnicode map for native extraction.
Unrecognized files are counted as
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
variables, not the stored configuration. Endpoint overrides must include the
full request route. The presets below supply provider-specific routes.
OpenAI/Azure use a chat-completions-compatible endpoint; other HTTP adapters
cover Anthropic, Gemini, and Ollama. Bedrock and Claude CLI use installed clients;
a generic CLI adapter is also available. Model/endpoint support depends on the
selected provider.

`--max-semantic-calls` and `--max-semantic-output-tokens` cap one invocation
across managed capture and local files. Retries and split attempts consume the
same allowance; validated cache hits do not. These caps count reservations,
including the native Claude turn allowances described below, and reserved
output tokens. The usage receipts separately report counters
returned by the provider. Advanced JSON settings include temperature,
thinking, and permitted extra request fields; Graf retains control of input,
authentication, and output limits.

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

### Provider discovery and setup

Inspect available configuration, preview a preset, then register it explicitly:

```sh
graf provider detect --json
graf provider template open_ai --json
graf provider --project . setup docs open_ai
# This step enables extraction and can incur provider charges:
graf index . --provider docs --max-semantic-files 8 \
  --max-semantic-calls 12 --max-semantic-output-tokens 24576 --json
```

`detect` reports environment-variable presence and executable paths without
running those executables or contacting providers. Even an empty variable counts
as present; detection does not verify authentication, CLI versions or service
availability. It does not select or save a provider. `template PRESET` prints
validated `SemanticOptions` JSON without saving or enabling it.

`setup NAME PRESET` creates a new named configuration in
`.graf/providers.json` when `--project PATH` is supplied before `setup`, or
`~/.graf/providers.json` otherwise. It refuses existing or built-in names.
Native CLI presets must already be installed; setup records their resolved
executable path but does not run or authenticate them. Later
`index --provider NAME` enables the saved configuration; `update` reuses the
index's stored extraction settings. Updating a registry entry does not alter
extraction settings already saved in an index.
Use `show NAME` to inspect it, `remove NAME` to remove it, or the existing
`add NAME FILE` to explicitly register or replace a custom JSON configuration.

| Preset | Default route or installed client | Default key environment name | Model |
| --- | --- | --- | --- |
| `open_ai`, alias `openai` | `https://api.openai.com/v1/chat/completions` | `OPENAI_API_KEY` | `gpt-6-astra` |
| `anthropic` | `https://api.anthropic.com/v1/messages` | `ANTHROPIC_API_KEY` | Required with `--model` |
| `gemini` | `https://generativelanguage.googleapis.com/v1beta/models/{model}:generateContent` | `GEMINI_API_KEY` | Required with `--model` |
| `ollama` | `http://localhost:11434/api/chat` | None | Required with `--model` |
| `azure` | Required full deployment URL via `--endpoint` | None; use `--key-env` for API-key authentication | Required with `--model` |
| `bedrock` | `aws bedrock-runtime converse` | Uses the client's authentication | Required model ID with `--model` |
| `claude_cli` | `claude` | Uses the client's authentication | Required model/alias with `--model` |

An explicit `--endpoint` suppresses the default key environment name; supply
`--key-env NAME` when that endpoint needs a key. Endpoint and key values are
never inferred from detection results. For example, Gemini can use
`--key-env GOOGLE_API_KEY`, and an OpenAI-compatible local endpoint can omit
authentication. OpenAI retains its default model even with a custom endpoint;
override `--model` for a server that serves another model. CLI presets reject
`--endpoint` and `--key-env`. Generic `cli` adapters require JSON through
`provider add`, rather than a preset.

```sh
graf provider --project . setup local open_ai \
  --endpoint http://localhost:1234/v1/chat/completions --model "$LOCAL_MODEL"
graf provider --project . setup subscription claude_cli --model "$CLAUDE_MODEL"
```

Direct built-in selection, such as `graf index . --provider openai`, uses the
same presets. Claude CLI extraction probes the selected client's help for
structured-output support, using strict graph-JSON validation when that support
is unavailable. This probe occurs during explicit extraction, not detection or
setup. Vision remains a separate opt-in and requires the native restricted
Claude command recipe.

### Usage and recovery limits

Completed JSON extraction reports separate `semantic_usage` reservations from
`provider_usage` receipts. Each admitted HTTP request or CLI invocation produces
one receipt, including failed attempts; a native CLI invocation with multiple
turns still has one aggregate receipt. Receipts retain requested/reported models
and any reported input, output, total, cache, reasoning-token or USD-cost
counters. Missing counters stay unknown (`null`), not zero. Cache and reasoning
counters may overlap other counters, so do not add them to invent a total.
Cache hits and rejected reservations produce no receipt. Reported usage is not
a verified invoice.

If `index` or `add` fails after reserving or recording usage, the error retains
the available reservations and receipts. The CLI includes their JSON in the
stderr error message, with terminal controls escaped; stdout stays empty even
with `--json`. This is an error message containing JSON, not a standalone JSON
report. `usage_unavailable: true` means some accounting could not be recovered;
missing counters do not establish that no usage occurred.

Defaults and configurable limits are:

| Setting | Default | Bound or scope |
| --- | --- | --- |
| `--max-semantic-files` | 32 | Up to 100,000 files |
| `--max-semantic-calls` | Unset | Optional cap on reserved generations across the invocation |
| `--max-semantic-output-tokens` | Unset | Optional invocation-wide reserved-output cap |
| `SemanticOptions.max_calls` | 4 | 1–128 generation reservations per file, including recovery |
| `timeout_secs` | 60 | 1–3,600 seconds per attempt |
| `max_input_tokens` | 8,192 | 2,048–1,048,576; conservative UTF-8 byte estimate with 1,024 reserved for instructions/protocol |
| `max_output_tokens` | 2,048 | 1–65,536 reserved per generation |
| `max_total_output_tokens` | 8,192 | Per-file reservation cap; at least `max_output_tokens` |
| `max_response_bytes` | 1 MiB | 1 KiB–16 MiB |
| `max_image_bytes` | 4 MiB | 1 byte–16 MiB; upload still requires `--vision` |
| `max_retries` / `retry_delay_ms` | 0 / 250 ms | At most 8 retries; delay at most 30,000 ms |
| `max_split_depth` | 0 | At most 8 levels; text recovery only |

HTTP, Bedrock and generic CLI attempts reserve one generation. The native
Claude recipe reserves one turn for text or three turns for vision, plus
`max_output_tokens` for each allowed turn, before starting the client. Vision
therefore needs `max_calls >= 3` and at least `3 * max_output_tokens` of remaining
per-file output allowance (6,144 tokens at the default). Any invocation-wide
caps must also have that capacity remaining. A retry needs another full
allowance. Unused turns are not refunded; `semantic_usage.calls` counts reserved
generations, not observed model turns or subprocesses.

Set per-file options in provider JSON or nested `ingest.semantic` within
`--config FILE`. Recovery is disabled by default. Configured retries handle
empty responses and recognized transient failures. Configured splitting handles
truncated responses, recognized context-overflow errors and timeouts by retrying
smaller text chunks; images are not split. Authentication failures, arbitrary
error prose and invalid graph JSON do not trigger speculative recovery.

All recovery shares the original file's call/output budget and a request
deadline of `timeout_secs * max_calls` (240 seconds by default), including
backoff and capability probing. A split needs remaining allowance for its child
requests. Incomplete extraction fails with the prior stored graph intact;
increase the explicit budget, correct the provider configuration or reduce the
input before retrying.

### Semantic identity policy

By default, Graf preserves distinct semantic source IDs and their evidence,
including parallel relation records. This lets each source be edited or removed
independently and keeps each relationship's provenance inspectable. The tradeoff
is more duplicate-looking nodes: similar names, spelling variants and mentions
across files are not automatically collapsed into one global entity.

Set `SemanticOptions.deduplicate` to `true` to merge newly extracted `concept`
or `entity` nodes within one file when their kinds and trimmed labels match
exactly, including case. The default is `false`. This option retains merged
node evidence as `corroborating_evidence` and rewrites relation endpoints; it
removes self-edges and redundant `mentions` edges while retaining other parallel
relation records. Graf does not perform typo-based merging or call an LLM to
break identity ties. This is a choice about source identity, not a guarantee of
fewer duplicates or better entity resolution for every dataset.

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

### Community algorithms

Louvain remains the default. Select native Leiden explicitly for analysis and
commands that accept analysis options:

```sh
graf analyze --community-algorithm leiden --community-seed 42 \
  --community-local-max-passes 100 --json
graf export html --community-algorithm leiden --output graph.html
```

Both algorithms use a weighted, symmetric projection of recorded edges and
split disconnected components. Louvain is deterministic; Leiden uses a fixed
seed (default 42) for reproducible stochastic refinement. Neither supplies
semantic meaning or guarantees the same partition as another implementation.
Stored imported memberships are not overwritten.

The default total budget is 100 Louvain sweeps or Leiden outer iterations,
including retries for requested size/cohesion targets. Leiden's
`--community-local-max-passes` defaults to 100 and must be positive; it bounds
node processing per local-moving call at each level, not total runtime.
Nontrivial Leiden runs report `community_converged=false` and
`community_convergence_known=false` because the backend does not report whether
its local cap was exhausted. An unchanged partition alone does not certify
convergence. Inspect these fields and the unmet-constraint report when using a
capped result. The community seed and local-pass option do not change Louvain.

### Export formats and saved labels

For a native database, `report` includes recorded corpus coverage and marks
freshness as `not_checked` by default. `report --check-freshness` explicitly
compares current local source fingerprints, reporting `fresh`, `stale`, or
`unavailable` if the source cannot be checked. It does not update the graph or
call models/converters. This option requires a native database, not an imported
graph or JSON snapshot. Source context appears in Markdown/HTML reports and a
vault's report.md, and accompanies JSON command results; interchange artifacts
remain unchanged. Other export commands do not scan source freshness unless
explicit work-memory annotations require checking cited files as described below.

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

### Explore an HTML graph

```sh
graf export html --node-limit 300 --edge-limit 1000 --output graph.html
```

Open the file locally. The self-contained viewer makes no network requests.
Search covers labels, IDs, sources and metadata in the complete embedded
snapshot. Automatic mode groups larger matches by community; select a community
to open its detailed member pages, or choose Detailed pages yourself. The
positive `--node-limit` and `--edge-limit` default to 300 nodes and 1,000 drawn
edges per page. Download snapshot JSON always retains the complete graph.

Select a node in the graph or its keyboard-accessible list to focus it and
inspect its recorded relations. Neighbor buttons show incoming, outgoing,
undirected or self-loop direction, relation text, endpoint source and edge
evidence. Parallel records remain separate. Neighbor direction/search filters
cover all incident records, independently of the main view's filters; pages
show at most `min(50, node_limit, edge_limit)` records. Following a hidden node
opens its detailed page and clears the main filters. Back restores the previous
filters, graph and relation pages, selection and viewport, retaining up to 100
navigation steps. Fit graph and Zoom buttons provide keyboard alternatives to
scroll zoom; drag the background to pan.

Layout separates connected components and places nodes by graph hops from a
hub, using only drawn edges. It does not run an all-pairs force simulation.
Direction remains visible through arrows; distance does not mean relationship
strength or execution order. Omitted edges can split the displayed components,
dense graphs can have overlapping labels or crossing edges, and long paths may
need zoom. Use the neighbor list and complete download for evidence outside the
drawn view.

Recorded `group`/`hyperedge` nodes also have outlines based on their actual
`member_of` incidence edges. The viewer takes the convex hull of visible member
positions; one or two distinct positions use a circle or segment. It retains
group nodes and every incidence record, without adding pairwise relationships.
The group list shows outlined, visible and total recorded member counts, with
partial-group notices and keyboard access to the original group and relation
evidence. Group search covers the complete group set. Each group page contains
at most `min(50, node_limit, edge_limit)` entries and uses at most `edge_limit`
member samples across all outlines. Hidden or unsampled members do not
contribute to outlines. Community summaries require drilling down before drawing
outlines.

### Work-memory annotations in exports

Select saved observations explicitly:

```sh
graf report --memory-dir graf-out/memory --output report.md
graf export html --memory-dir graf-out/memory --output graph.html
graf export wiki --memory-dir graf-out/memory --output ./notes
```

For report/export, `--memory-dir` supports only HTML, Markdown, wiki and Obsidian
output; other formats fail.
It reads memory and checks cited local sources using a 30-day decay half-life
and two verified useful events for corroboration. It does not write a lessons
sidecar, change the graph or affect graph ranking. Without the option, exports
do not load memory. Wiki output still requires an existing destination and
creates a new generated folder.

HTML shows status rings, a text legend, node counts/scores/reasons and literal
lesson text. Dashed rings identify unverified, stale or unmarked observations;
colors are supplemented by text. Lesson text is paged at up to 16,385 UTF-16
code units to preserve Unicode pairs. Markdown includes a work-memory section;
wiki node notes include their own observations and the report includes lessons.
Shared lesson category headings remain readable in Markdown; their contents
use literal code blocks so saved HTML and links stay inert. Snapshot downloads
retain the original graph, without display annotations or new relations.

The library's optional `ExportOptions.learning` must match the BLAKE3 hash of
the exact serialized snapshot, including metadata and generation; a mismatched
or missing hash is rejected before output. Regenerate annotations for another
snapshot. The offline viewer does not check files again: cited-source proof is
limited to the observation and never certifies the whole project or an answer.

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
those fields. Graf does not implement Graphify's Python internals or require
graph reads before source reads. The
[local memory](#save-answers-and-reflect) and
[PR inspection](#inspect-github-pull-requests) commands below have
their own interfaces and limits.

### Preserved communities

Imported `community` IDs and recorded `community_name` values remain separate
from newly computed structural communities. Integer ID `7` and string ID `"7"`
are distinct. Composition paths distinguish the same recorded ID in different
merged projects, and conflicting recorded names remain visible. Missing or
unsupported memberships remain unassigned.

In MCP, read `graf://communities` (or the default-project alias
`graphify://communities`) to discover IDs, names and composition paths.
`get_community` defaults to `community_source: "auto"`: it uses preserved
memberships when any are present, otherwise computed communities. Set
`community_source: "computed"` or read `graf://computed-communities` to request
structural analysis explicitly. `graf analyze`, `graf communities` and the HTML
community view continue to describe computed communities.

For a merged snapshot, copy the exact composition path from the resource into
`community_project`; this differs from `project`, which selects a registered
MCP database. For example, if the resource lists integer ID 7 under `["api"]`,
the `get_community` arguments are:

```json
{"community_id":7,"community_source":"preserved","community_project":["api"],"limit":100}
```

An ambiguous ID without its composition path is an error. Community reads
default to 100 nodes (maximum 500) and a 2,000-token estimated budget
(1–100,000); inspect `total_nodes` and `truncated`. Preserved lookups do not
cluster and use the larger snapshot limits described under
[MCP](#agent-setup-and-mcp). Explicit computed lookups retain the analysis cap.

## Save answers and reflect

Save an answer explicitly after reviewing it:

```sh
graf save-result --question "Where is authentication checked?" \
  --answer-file answer.txt --outcome useful
# NODE_ID must be an exact ID from the selected graph, not a label:
graf save-result --question "Where is authentication checked?" \
  --answer "The initial answer named the wrong handler." \
  --outcome corrected --correction "Use the middleware handler." --nodes "$NODE_ID"
graf reflect --half-life-days 30 --min-corroboration 2 --if-stale
```

Supply exactly one of `--answer` and `--answer-file`. `--type` defaults to
`query`; optional outcomes are `useful`, `dead_end` and `corrected`.
`--correction` requires `--outcome corrected`. Each save creates a distinct local
event under `graf-out/memory`; `--memory-dir DIR` changes the directory for both
commands. No command captures ordinary queries automatically or changes graph
topology, query ranking or edge weights.

Both commands can use `--snapshot FILE` or global `--db PATH`, or discover the
nearest local index. They also work without a graph unless `--nodes` is supplied,
which requires exact-ID validation. These explicit commands may read cited
local source files, but do not index, call a model or access the network.

Saving citations records node/source fingerprints where available. A source
fingerprint records disk bytes at save time; it does not establish which bytes
produced the graph node. Native snapshots can also carry indexed source digests.
A cited node is current only when its saved and current node/graph identity
match and its saved and current disk bytes match the indexed digest captured
at save time and the current indexed digest. This checks the cited source,
not the whole project or answer correctness.

Old records or snapshots without that proof remain `unverified (unchanged since
save; snapshot/source correspondence unknown)` even when node/disk baselines
match; later proof never upgrades the original save. Known source/index mismatch
at save time, later node/source/identity changes and disappearance of a previously
fingerprinted source are stale and excluded from recommendations.

`reflect` writes `graf-out/reflections/LESSONS.md` by default; `--out FILE`
chooses another path. It lists preferred, tentative, contested, dead-end and
corrected observations with age-decayed scores. The decay half-life
defaults to 30 days; zero disables decay. Minimum corroboration defaults to 2
distinct events (range 1–4,096); copying a saved record does not corroborate it.
Preferred requires the requested number of verified useful events. Unverified
events neither supply nor veto that corroboration; useful evidence with no proof
remains tentative even with `--min-corroboration 1`. Mixed useful and negative
observations are contested. Recommended sources sort by
decayed score, then exact node ID, with their verification status and reason
visible. These scores do not certify that an answer describes the current code.

`--if-stale` preserves an already identical lessons file; it does not certify
source freshness. Existing outputs must be Graf-generated lessons; unrelated
files and memory records are not overwritten.

Questions are limited to 16 KiB, answers and corrections to 256 KiB each, and
citations to 100 node IDs per save. Reflection reads immediate `.md` files only,
at most 4,096 documents of at most 2 MiB each and 32 MiB total. Keep memory and
reflections out of version control when they contain private work.

### Read learning observations during navigation

```sh
graf show authenticate --memory-dir graf-out/memory
graf explain authenticate --memory-dir graf-out/memory
graf --db .graf/index.db serve --memory-dir graf-out/memory
```

`show` and its `explain` alias annotate returned nodes. With the server option,
MCP `query_graph`, `get_node` and `get_neighbors` annotate returned nodes from
the default database only; named `--project` databases remain unannotated.
Tools cannot supply filesystem memory paths. Without the option, these reads
do not load memory.

Each annotated request rereads memory and checks cited source files, even when
the graph generation is unchanged. The defaults are a 30-day decay half-life
and two verified useful events. Annotations do not change selection, ranking,
traversal or stored graph data, and do not write lessons. Verified correspondence
covers the cited file and its indexed bytes; it certifies neither the answer nor
the entire checkout's freshness. Unreadable or invalid memory, unavailable bounded
snapshots and exhausted annotation budgets produce an explicit omission or
truncation notice while retaining the graph result.

## Inspect GitHub pull requests

These commands explicitly contact GitHub through an installed, authenticated
`gh`. They inspect data without posting reviews/comments, merging, or changing
branches/worktrees:

```sh
graf prs --repo OWNER/REPO
graf prs 123 --repo OWNER/REPO --snapshot graph.json --json
graf prs --repo OWNER/REPO --triage --conflicts --worktrees --wrong-base
```

Omitting `--repo` (alias `-R`) uses the checkout's GitHub `origin`. Only
`github.com` repositories in `OWNER/REPO` form are supported. `--base BRANCH`
(alias `-b`) selects the expected base; otherwise Graf reads the repository's
actual default branch. List mode fetches at most 50 open PRs, then hides PRs
targeting another base unless `--wrong-base` is supplied. A numbered lookup
also includes closed/merged PRs and shows that PR even when its base differs.

`--triage` explains a deterministic attention order based on base branch, CI,
review status, draft state and update age; 14 days without an update is stale.
It does not call a model. `--conflicts` adds changed-file/community overlaps;
triage includes these automatically. Overlap identifies shared review context,
not a textual merge conflict or a safe merge order.

When a graph is available through `--snapshot`, `--db` or local discovery,
changed files map to direct node/community impact in that stored snapshot.
Recorded community identities are used when present. Without them, structural
communities are computed only within 5,000 nodes, 20,000 edges, 20,000 unresolved
references and an 8 MiB serialized snapshot. Beyond any of those bounds,
file/node impact and file overlaps remain available, while computed communities
are omitted with an explicit notice. Empty communities then do not establish
that no community overlap exists. Snapshot loading and response limits still
apply. Ambiguous and unmatched paths remain explicit. The snapshot's repository
and revision are not verified against the PR. Without a graph, PR inspection
still works but omits node/community impact.
`--worktrees` adds local branch mappings only when the local origin matches the
selected repository; cross-repository PRs are not assigned a worktree by a
matching branch name alone.

Git/`gh` subprocesses share a 30-second budget, with an 8 MiB cap per stdout or
stderr stream and at most 3,000 file records per PR. Read
`list_may_be_truncated`, `files_complete`, `unmatched_files`, `ambiguous_files`
and notices before treating a result as complete. For agent access, explicitly
enable [GitHub MCP tools](#github-mcp-tools) for one repository.

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
use `--skill --mcp` for both components. `--tool-hooks` selects
optional tool guidance only; combine it with other component flags explicitly.
`--global` selects user scope for skills/MCP; tool hooks require project scope.
The `agents` platform uses the portable Agent Skills layout. Named host routes
include Claude, Codex, Cursor, Gemini, OpenCode, Copilot/VS Code, Kilo, and other
hosts supported by `install`. Aider adds the guidance file to its YAML `read`
setting. Native MCP setup is implemented for
Claude, Codex, Cursor, Gemini, and VS Code. Other hosts can use
skills with manual `graf serve` configuration.

Setup preserves unrelated configuration and tracks its changes for uninstall.
Uninstall normally restores only unchanged files recorded by the installation;
later edits are refused. The
[Claude global MCP cleanup](#claude-global-mcp-cleanup) below has a
limited exception for unrelated host changes. VS Code MCP files can contain
comments and trailing commas; Gemini settings can contain comments. Setup
preserves those bytes without reformatting the file. Claude and
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

Git hooks are optional foreground refreshes after commit, branch checkout, and
merge. They chain existing executable hooks, preserve their exit status, and
never stage or commit graph files. They run `update` only when `.graf/index.db`
exists and therefore reuse its extraction settings. Setup installs no watcher
or service and imposes no restriction on reading source files.

### Optional tool guidance

Opt in to project hooks for Claude, CodeBuddy or Gemini:

```sh
graf install --platform claude --project . --tool-hooks
graf uninstall --platform claude --project . --tool-hooks
graf install --platform codebuddy --project . --tool-hooks
graf uninstall --platform codebuddy --project . --tool-hooks
# Gemini MCP and hooks share one settings file; select both together:
graf install --platform gemini --project . --mcp --tool-hooks
graf uninstall --platform gemini --project . --mcp --tool-hooks
```

Plain `install` does not enable these hooks. `--tool-hooks` alone does not
install skills or MCP; add `--skill` or supported `--mcp` explicitly. Global
tool hooks and other hosts are unsupported. Graf must be on the host's `PATH`.

| Platform | Project configuration | Tools receiving optional guidance |
| --- | --- | --- |
| `claude` | `.claude/settings.json` | `Read`, `Glob`, `Grep`, recognized searches in `Bash` |
| `codebuddy` | `.codebuddy/settings.json` | `Read`, `Glob`, `Grep`, recognized searches in `Bash` |
| `gemini` | `.gemini/settings.json` | `read_file`, `list_directory` |

The installed command checks a matching native `.graf/index.db` and recorded
source membership. When relevant, it supplies context suggesting `query`,
`show`, `callers` or `impact` and states that freshness has not been checked.
Graf never denies the original tool, requires graph-first access or creates a
session marker. Missing/malformed input or indexes and unsupported scopes
produce no guidance; Gemini still receives an explicit allow response.
Evaluation does not scan source contents, refresh the graph, change graph or
configuration data, or call a model/network service.

Input is limited to 64 KiB. Directory/glob checks consider at most 512 sorted
indexed paths per scope, so a matching file beyond that bound may receive no
guidance. Shell recognition handles supported searches such as `rg`, `grep`,
`git grep` and `find` with static arguments; it does not execute the command.
Dynamic scopes, unsupported flags and commands that change directories receive
no guidance. Paths outside the selected project, including symlink escapes,
are excluded.

Installation preserves existing settings and hook entries. Reinstall/uninstall
uses the same platform, project and component flags and refuses later edits to
the recorded settings file. Gemini MCP and hooks share a receipt: if one is
already installed separately, uninstall that selection before installing
`--mcp --tool-hooks` together. Removing a combined installation also requires
both flags.

### Claude global MCP cleanup

Claude may add first-run settings to its global MCP configuration. When
uninstalling global MCP, Graf can remove its recorded `mcpServers.graf` entry
while preserving those current settings, other servers and unrelated formatting,
provided the Graf entry still matches the installed configuration. This also
applies to a selected `--config-root`; use the same root and component flags
that were used for installation. If the entire file is unchanged, uninstall
restores the original bytes or removes a file created by Graf.

Changes to Graf's command or arguments, malformed JSON, duplicate keys or unclear
ownership cause refusal without changing the configuration. An already absent
Graf entry completes cleanup without restoring or removing unrelated content.
This exception applies only to Claude global MCP uninstall. Install/reinstall,
guidance, project configurations, tool hooks and other hosts retain their
existing protections against later edits.

### MCP transport and project routing

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
aliases. Graph tools and resources remain local reads: they do not refresh or
invoke providers. GitHub calls require the separate opt-in below.

### MCP snapshot and analysis limits

Snapshot caching and structural analysis have separate bounds per registered
project:

| Operation | Limit |
| --- | --- |
| Snapshot cache, including preserved-community reads | 100,000 nodes, 1,000,000 edges and 1,000,000 unresolved references |
| Cached snapshot payload | 64 MiB by default; `serve --snapshot-max-bytes BYTES` accepts 1 byte–256 MiB |
| Computed structural analysis | 5,000 nodes, 20,000 edges, 20,000 unresolved references and an 8 MiB serialized snapshot |
| Resource response / structured tool payload | 1 MiB / 512 KiB |

The payload budget counts stored graph data, not SQLite file size. Analysis is
cached per generation and runs only for operations that need it. Preserved
membership lookup does not spend the analysis budget. A result that exceeds
response bounds needs a narrower tool request or an explicit CLI export; larger
snapshot limits do not raise response or structural-analysis limits.

### GitHub MCP tools

Enable read-only GitHub PR calls for one repository explicitly:

```sh
graf --db .graf/index.db serve --github-repo OWNER/REPO
```

Without `--github-repo`, the server does not expose `list_prs`, `get_pr_impact`
or `triage_prs`. With it, invoking one of those tools runs bounded GitHub reads
through installed, authenticated `gh`. Starting the server or using ordinary
graph tools does not fetch PRs. No PR tool calls a model, writes to GitHub, or
inspects local worktrees.

`list_prs` and `triage_prs` accept optional `base`, `repo` and registered
`project` arguments. `get_pr_impact` requires `pr_number` and accepts `repo` and
`project`; it also handles closed/merged PRs. An omitted `repo` selects the
configured repository, and a supplied value must match it. A `project` chooses
an already registered database, never a path or a different GitHub repository.

These tools share the [PR command's limits](#inspect-github-pull-requests):
50 PRs for list/triage, a shared 30-second subprocess budget, 8 MiB per output
stream and 3,000 files per PR. MCP response limits still apply. Impact and triage
also use snapshot limits. Without recorded memberships, exceeding the smaller
analysis cap omits computed communities with a notice while retaining file/node
impact. Inspect completeness and snapshot notices rather than treating empty
communities as proof of no overlap or an overlap as a merge recommendation.

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
