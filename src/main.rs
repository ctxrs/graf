mod agent_setup;
mod commands;
mod connect;
mod extraction;
mod mcp;
mod switch;
mod switch_config;
mod switch_files;

use std::{
    io::{self, Write},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail, ensure};
use clap::{Args, Parser, Subcommand, ValueEnum};
use graf::{hook_guard, import, index, model::*, store::Store};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Parser)]
#[command(
    bin_name = "graf",
    styles = clap::builder::Styles::plain(),
    version,
    about = "Navigate a persistent local code graph",
    after_help = "Reads use the indexed snapshot. Only explicit --memory-dir annotations check live source evidence. Run update explicitly to refresh native indexes."
)]
struct Cli {
    /// Database path. Otherwise discover the nearest ancestor .graf/index.db.
    #[arg(long, global = true)]
    db: Option<PathBuf>,
    /// Print machine-readable JSON instead of human-readable output.
    #[arg(long, global = true)]
    json: bool,
    /// Explicitly append read-command metadata to a JSONL file.
    #[arg(long, global = true)]
    query_log: Option<PathBuf>,
    /// Also include returned graph records in the explicit query log.
    #[arg(long, global = true, requires = "query_log")]
    log_responses: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    #[command(flatten)]
    Extended(commands::Command),
    #[command(flatten)]
    Connect(connect::Command),
    /// Install reversible agent guidance, skills or MCP configuration.
    Install(agent_setup::SetupArgs),
    /// Remove only the selected Graf-owned agent integration.
    Uninstall(agent_setup::SetupArgs),
    /// Explicitly manage optional Git refresh hooks.
    Hook(agent_setup::HookArgs),
    /// Supply optional local graph context to an agent hook; never deny a tool.
    HookGuard(hook_guard::HookGuardArgs),
    /// Import a Graphify snapshot and switch this project's MCP connection.
    Switch(switch::SwitchArgs),
    /// Index supported source code and documents into a persistent local graph.
    #[command(alias = "extract")]
    Index {
        #[arg(default_value = ".")]
        path: PathBuf,
        #[command(flatten)]
        extraction: extraction::ExtractionArgs,
    },
    /// Refresh the native source root recorded in the database.
    Update {
        /// Include measured detection, extraction and commit times in the report.
        #[arg(long)]
        timing: bool,
        /// Re-extract local files once; saved remote sources remain offline.
        #[arg(long)]
        force: bool,
        /// Bypass semantic/transcript cache reads for local sources once.
        #[arg(long)]
        refresh_cache: bool,
        /// Accept fewer semantic facts, saving the previous graph in .graf/backups.
        #[arg(long)]
        allow_semantic_shrink: bool,
    },
    /// Compare local source fingerprints without model or converter calls.
    CheckUpdate,
    /// Explicit foreground polling; queries themselves never refresh the graph.
    Watch {
        #[arg(long, default_value_t = 1000, value_parser = clap::value_parser!(u64).range(100..=3_600_000))]
        interval_ms: u64,
        /// Stop after this many polls; omitted means until interrupted.
        #[arg(long, value_parser = clap::value_parser!(u32).range(1..))]
        iterations: Option<u32>,
    },
    /// Explicitly import a URL, Google pointer or local document and retain its extracted facts.
    Add {
        source: String,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        contributor: Option<String>,
        #[arg(long)]
        captured_at_unix_secs: Option<u64>,
        #[arg(long, default_value = ".")]
        project: PathBuf,
        #[command(flatten)]
        extraction: extraction::ExtractionArgs,
    },
    /// Manage semantic provider configurations; keys remain in the environment.
    Provider(extraction::ProviderArgs),
    /// Inspect or explicitly repair an extraction cache without provider calls.
    Cache(extraction::CacheArgs),
    /// Clone a GitHub repository with Git; optionally index it after checkout.
    Clone {
        url: String,
        #[arg(long)]
        output: Option<PathBuf>,
        #[arg(long)]
        branch: Option<String>,
        #[arg(long)]
        index: bool,
        /// Fetch and fast-forward an existing cached checkout; local changes are never reset.
        #[arg(long)]
        refresh: bool,
    },
    /// Find symbols and explore a bounded neighborhood.
    Query(QueryArgs),
    /// Show an exact ID or unique symbol and its immediate neighbors.
    #[command(alias = "explain")]
    Show(ShowArgs),
    /// Show immediate incoming calls to a symbol.
    Callers(SymbolArgs),
    /// Show immediate outgoing calls from a symbol.
    Callees(SymbolArgs),
    /// Follow reverse dependencies from a symbol, class members, or source file.
    #[command(alias = "affected")]
    Impact(ImpactArgs),
    /// Find a bounded path, following outgoing edges by default.
    Path(PathArgs),
    /// Report graph-wide counts, coverage, and diagnostics.
    Stats,
    /// Import a graph snapshot into an empty database (default: .graf/index.db).
    Import {
        #[command(subcommand)]
        format: ImportFormat,
    },
    /// Serve read-only MCP tools over stdin/stdout or explicit HTTP.
    Serve(mcp::ServeArgs),
}

#[derive(Subcommand)]
enum ImportFormat {
    Graphify {
        file: PathBuf,
        /// node-link honors graph flags; export uses Graphify's logical edge direction and raw export layouts.
        #[arg(long, value_enum, default_value = "node-link")]
        format: SnapshotFormat,
        /// Atomically replace an existing imported graph, retaining it on failure.
        #[arg(long)]
        refresh: bool,
    },
    Graf {
        file: PathBuf,
        #[arg(long)]
        refresh: bool,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum SnapshotFormat {
    NodeLink,
    Export,
}

#[derive(Debug, Clone, Copy, ValueEnum, Serialize, Deserialize, JsonSchema)]
enum TraversalDirection {
    #[serde(rename = "in")]
    In,
    #[serde(rename = "out")]
    Out,
    #[serde(rename = "both")]
    Both,
}

impl From<TraversalDirection> for Direction {
    fn from(value: TraversalDirection) -> Self {
        match value {
            TraversalDirection::In => Self::Incoming,
            TraversalDirection::Out => Self::Outgoing,
            TraversalDirection::Both => Self::Both,
        }
    }
}

fn one() -> u32 {
    1
}
fn three() -> u32 {
    3
}
fn six() -> u32 {
    6
}
fn hundred() -> u32 {
    100
}
fn both() -> TraversalDirection {
    TraversalDirection::Both
}
fn outgoing() -> TraversalDirection {
    TraversalDirection::Out
}

#[derive(Debug, Args, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct QueryArgs {
    /// Symbol search text (nonempty).
    #[schemars(length(min = 1))]
    text: String,
    /// Maximum traversal depth, 0..6. Default 1.
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u32).range(0..=6))]
    #[serde(default = "one")]
    #[schemars(range(min = 0, max = 6))]
    depth: u32,
    /// Maximum results, 1..500. Default 100. Truncation is reported explicitly.
    #[arg(long, default_value_t = 100, value_parser = clap::value_parser!(u32).range(1..=500))]
    #[serde(default = "hundred")]
    #[schemars(range(min = 1, max = 500))]
    limit: u32,
    /// Traverse incoming, outgoing, or both directions. Default both.
    #[arg(long, value_enum, default_value = "both")]
    #[serde(default = "both")]
    direction: TraversalDirection,
    /// Exact relation filter, such as calls, imports, or contains; omitted means all.
    #[arg(long)]
    relation: Option<String>,
    #[command(flatten)]
    #[serde(flatten)]
    navigation: NavigationArgs,
}

#[derive(Debug, Default, Args, Deserialize, JsonSchema)]
#[serde(default)]
struct NavigationArgs {
    /// Use depth-first traversal instead of breadth-first.
    #[arg(long)]
    dfs: bool,
    /// Restrict relationship contexts (repeatable).
    #[arg(long)]
    context: Vec<String>,
    /// Restrict node source files (repeatable).
    #[arg(long)]
    file: Vec<String>,
    /// Restrict node kinds (repeatable).
    #[arg(long)]
    kind: Vec<String>,
    /// Approximate JSON token budget: UTF-8 bytes divided by four.
    #[arg(long)]
    budget: Option<usize>,
    /// Include edges between any returned nodes, within the query bounds.
    #[arg(long)]
    induced_edges: bool,
    #[arg(long)]
    infer_context: bool,
}
impl NavigationArgs {
    fn enabled(&self) -> bool {
        self.dfs
            || !self.context.is_empty()
            || !self.file.is_empty()
            || !self.kind.is_empty()
            || self.budget.is_some()
            || self.induced_edges
            || self.infer_context
    }
    fn options(self, graph: QueryOptions) -> graf::query::SearchOptions {
        graf::query::SearchOptions {
            graph,
            traversal: if self.dfs {
                graf::query::Traversal::Dfs
            } else {
                graf::query::Traversal::Bfs
            },
            contexts: self.context,
            files: self.file,
            kinds: self.kind,
            token_budget: self.budget,
            induced_edges: self.induced_edges,
            infer_context: self.infer_context,
        }
    }
}

#[derive(Debug, Args, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SymbolArgs {
    /// Exact node ID or unique label/qualified name. Ambiguous names are errors.
    #[schemars(length(min = 1))]
    symbol: String,
    /// Maximum results, 1..500. Default 100.
    #[arg(long, default_value_t = 100, value_parser = clap::value_parser!(u32).range(1..=500))]
    #[serde(default = "hundred")]
    #[schemars(range(min = 1, max = 500))]
    limit: u32,
    #[command(flatten)]
    #[serde(flatten)]
    navigation: NavigationArgs,
}

#[derive(Debug, Args)]
struct ShowArgs {
    #[command(flatten)]
    symbol: SymbolArgs,
    /// Read fresh learning evidence for returned nodes; never writes or changes selection.
    /// Uses remaining --budget space, or at most 8 KiB of annotations without --budget.
    #[arg(long)]
    memory_dir: Option<PathBuf>,
}

#[derive(Debug, Args, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ImpactArgs {
    /// Exact node ID or unique label/qualified name. Ambiguous names are errors.
    #[schemars(length(min = 1))]
    symbol: String,
    /// Maximum reverse dependency depth, 0..6. Default 3.
    #[arg(long, default_value_t = 3, value_parser = clap::value_parser!(u32).range(0..=6))]
    #[serde(default = "three")]
    #[schemars(range(min = 0, max = 6))]
    depth: u32,
    /// Maximum results, 1..500. Default 100.
    #[arg(long, default_value_t = 100, value_parser = clap::value_parser!(u32).range(1..=500))]
    #[serde(default = "hundred")]
    #[schemars(range(min = 1, max = 500))]
    limit: u32,
    /// Follow these dependency relations; repeat to combine. Defaults to known dependency relations.
    #[arg(long)]
    #[serde(default)]
    relation: Vec<String>,
    #[command(flatten)]
    #[serde(flatten)]
    navigation: NavigationArgs,
}

#[derive(Debug, Args, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct PathArgs {
    /// Starting node: exact ID or unique label/qualified name.
    #[schemars(length(min = 1))]
    source: String,
    /// Destination node: exact ID or unique label/qualified name.
    #[schemars(length(min = 1))]
    target: String,
    /// Maximum search depth, 0..6. Default 6.
    #[arg(long, default_value_t = 6, value_parser = clap::value_parser!(u32).range(0..=6))]
    #[serde(default = "six")]
    #[schemars(range(min = 0, max = 6))]
    depth: u32,
    /// Maximum results, 1..500. Default 100.
    #[arg(long, default_value_t = 100, value_parser = clap::value_parser!(u32).range(1..=500))]
    #[serde(default = "hundred")]
    #[schemars(range(min = 1, max = 500))]
    limit: u32,
    /// Edge direction. Default out; undirected imported edges remain bidirectional.
    #[arg(long, value_enum, default_value = "out")]
    #[serde(default = "outgoing")]
    direction: TraversalDirection,
    /// Exact relation filter; omitted means all relations.
    #[arg(long)]
    relation: Option<String>,
    #[command(flatten)]
    #[serde(flatten)]
    navigation: NavigationArgs,
}

enum ReadCommand {
    Query(QueryArgs),
    Show(SymbolArgs),
    Callers(SymbolArgs),
    Callees(SymbolArgs),
    Impact(ImpactArgs),
    Path(PathArgs),
    Stats,
}

#[derive(Serialize)]
#[serde(untagged)]
enum Output {
    Search(graf::query::SearchResult),
    SearchPath(graf::query::PathSearchResult),
    Graph(GraphResult),
    Path(PathResult),
    Stats(Stats),
    Index(IndexReport),
}

fn options(
    depth: u32,
    limit: u32,
    direction: Direction,
    relation: Option<String>,
) -> Result<QueryOptions> {
    ensure!(depth <= 6, "depth must be between 0 and 6");
    ensure!(
        (1..=500).contains(&limit),
        "limit must be between 1 and 500"
    );
    if let Some(relation) = &relation {
        nonempty(relation, "relation")?;
    }
    Ok(QueryOptions {
        depth,
        limit: limit as usize,
        direction,
        relation,
    })
}

fn nonempty(value: &str, name: &str) -> Result<()> {
    ensure!(!value.trim().is_empty(), "{name} must not be empty");
    Ok(())
}

fn read(db: &Path, command: ReadCommand) -> Result<Output> {
    let store = Store::open_read_only(db)?;
    let (symbol, options, navigation) = match command {
        ReadCommand::Stats => return Ok(Output::Stats(store.stats()?)),
        ReadCommand::Query(a) => {
            nonempty(&a.text, "text")?;
            let graph = options(a.depth, a.limit, a.direction.into(), a.relation)?;
            let extended = a.navigation.enabled();
            let result = store.query_extended(&a.text, &a.navigation.options(graph))?;
            return Ok(if extended {
                Output::Search(result)
            } else {
                Output::Graph(result.graph)
            });
        }
        ReadCommand::Path(a) => {
            nonempty(&a.source, "source")?;
            nonempty(&a.target, "target")?;
            let extended = a.navigation.enabled();
            let path = store.path_extended(
                &a.source,
                &a.target,
                &a.navigation
                    .options(options(a.depth, a.limit, a.direction.into(), a.relation)?),
            )?;
            if extended {
                return Ok(Output::SearchPath(path));
            }
            return Ok(Output::Path(PathResult {
                found: path.found,
                graph: path.result.graph,
            }));
        }
        ReadCommand::Show(a) => {
            nonempty(&a.symbol, "symbol")?;
            let extended = a.navigation.enabled();
            let options = a
                .navigation
                .options(options(1, a.limit, Direction::Both, None)?);
            let node = store.resolve_endpoint(&a.symbol, &options)?;
            let result = store.neighbors_extended(&node.id, &options)?;
            return Ok(if extended {
                Output::Search(result)
            } else {
                Output::Graph(result.graph)
            });
        }
        ReadCommand::Callers(a) => (
            a.symbol,
            options(1, a.limit, Direction::Incoming, Some("calls".into()))?,
            a.navigation,
        ),
        ReadCommand::Callees(a) => (
            a.symbol,
            options(1, a.limit, Direction::Outgoing, Some("calls".into()))?,
            a.navigation,
        ),
        ReadCommand::Impact(a) => {
            nonempty(&a.symbol, "symbol")?;
            let result = store.impact_extended(
                &a.symbol,
                &graf::query::ImpactOptions {
                    search: a.navigation.options(options(
                        a.depth,
                        a.limit,
                        Direction::Incoming,
                        None,
                    )?),
                    relations: a.relation,
                },
            )?;
            return Ok(Output::Search(result));
        }
    };
    nonempty(&symbol, "symbol")?;
    let extended = navigation.enabled();
    let result = store.neighbors_extended(&symbol, &navigation.options(options))?;
    Ok(if extended {
        Output::Search(result)
    } else {
        Output::Graph(result.graph)
    })
}

fn database(cli: &Cli) -> Result<PathBuf> {
    if let Some(db) = &cli.db {
        return Ok(db.clone());
    }
    match &cli.command {
        Command::Index { path, .. } => Ok(path.join(".graf/index.db")),
        Command::Add { project, .. } => Ok(project.join(".graf/index.db")),
        Command::Import { .. } => Ok(std::env::current_dir()?.join(".graf/index.db")),
        _ => {
            let cwd = std::env::current_dir()?;
            for ancestor in cwd.ancestors() {
                let candidate = ancestor.join(".graf/index.db");
                if candidate.try_exists()? {
                    return Ok(candidate);
                }
            }
            bail!(
                "no .graf/index.db found in this directory or its ancestors; run graf index or pass --db"
            )
        }
    }
}

// Escape terminal controls and nonprinting Unicode (including bidi/format
// characters) only at the human-output boundary. Keep machine values intact.
fn human(text: &str) -> impl std::fmt::Display + '_ {
    text.escape_debug()
}

fn diagnostics(out: &mut impl Write, items: &[Diagnostic]) -> io::Result<()> {
    for d in items {
        writeln!(
            out,
            "{}:{}: {}",
            human(&d.file),
            d.line.map(|n| n.to_string()).unwrap_or_else(|| "?".into()),
            human(&d.message)
        )?;
    }
    Ok(())
}

fn print_graph(out: &mut impl Write, graph: &GraphResult) -> io::Result<()> {
    writeln!(out, "Generation {} (indexed snapshot)", graph.generation)?;
    for n in &graph.nodes {
        writeln!(
            out,
            "{}  {}  {}  {}:{}",
            human(&n.id),
            human(&n.kind),
            human(&n.label),
            human(&n.file),
            n.line.map(|n| n.to_string()).unwrap_or_else(|| "?".into())
        )?;
    }
    for e in &graph.edges {
        writeln!(
            out,
            "{} --{}{} {}",
            human(&e.source),
            human(&e.relation),
            if e.directed { "-->" } else { "---" },
            human(&e.target)
        )?;
    }
    for r in &graph.unresolved {
        writeln!(
            out,
            "unresolved: {} --{}--> {} ({}:{}; {})",
            human(&r.source),
            human(&r.relation),
            human(&r.label),
            human(&r.file),
            r.line,
            human(&r.reason)
        )?;
    }
    if graph.nodes.is_empty() {
        writeln!(out, "No matching symbols.")?;
    }
    if graph.truncated {
        writeln!(out, "Result truncated by query bounds.")?;
    }
    Ok(())
}

fn print_output(output: Output, json: bool) -> Result<()> {
    let mut stdout = io::stdout().lock();
    if json {
        serde_json::to_writer(&mut stdout, &output)?;
        writeln!(stdout)?;
    } else {
        match &output {
            Output::Search(result) => {
                print_graph(&mut stdout, &result.graph)?;
                writeln!(stdout, "Estimated JSON tokens: {}", result.estimated_tokens)?;
                for reason in &result.truncation_reasons {
                    writeln!(stdout, "{}", human(reason))?;
                }
            }
            Output::Graph(graph) => print_graph(&mut stdout, graph)?,
            Output::SearchPath(path) => {
                writeln!(
                    stdout,
                    "{}",
                    if path.found {
                        "Path found."
                    } else if path.result.graph.truncated {
                        "Search incomplete: no path found within query bounds."
                    } else {
                        "No path found."
                    }
                )?;
                print_graph(&mut stdout, &path.result.graph)?;
                writeln!(
                    stdout,
                    "Estimated JSON tokens: {}",
                    path.result.estimated_tokens
                )?;
                for reason in &path.result.truncation_reasons {
                    writeln!(stdout, "{}", human(reason))?;
                }
            }
            Output::Path(path) => {
                writeln!(
                    stdout,
                    "{}",
                    if path.found {
                        "Path found."
                    } else if path.graph.truncated {
                        "Search incomplete: no path found within query bounds."
                    } else {
                        "No path found."
                    }
                )?;
                print_graph(&mut stdout, &path.graph)?;
            }
            Output::Stats(s) => {
                writeln!(
                    stdout,
                    "Generation {} ({})\n{} nodes, {} edges, {} files, {} unresolved references",
                    s.generation,
                    human(&s.kind),
                    s.nodes,
                    s.edges,
                    s.files,
                    s.unresolved_references
                )?;
                if let Some(root) = &s.root {
                    writeln!(stdout, "Root: {}", human(root))?;
                }
                writeln!(
                    stdout,
                    "Coverage: {} supported, {} unsupported, {} unchanged files",
                    s.coverage.supported_files,
                    s.coverage.unsupported_files,
                    s.coverage.unchanged_files
                )?;
            }
            Output::Index(r) => writeln!(
                stdout,
                "Generation {}: {} parsed, {} unchanged, {} deleted files; {} nodes, {} edges",
                r.generation, r.parsed_files, r.unchanged_files, r.deleted_files, r.nodes, r.edges
            )?,
        }
    }
    // JSON retains the full report; diagnostics never contaminate stdout as prose.
    match &output {
        Output::Index(r) => {
            diagnostics(&mut io::stderr().lock(), &r.diagnostics)?;
            if !json && let Some(t) = &r.timings {
                eprintln!(
                    "Timing (ms): detect={:.3} extract={:.3} commit={:.3} total={:.3}",
                    t.detect_ms, t.extract_ms, t.commit_ms, t.total_ms
                );
            }
        }
        Output::Stats(s) => diagnostics(&mut io::stderr().lock(), &s.diagnostics)?,
        _ => {}
    }
    Ok(())
}

fn print_value(value: &impl Serialize, json: bool) -> Result<()> {
    let mut out = io::stdout().lock();
    if json {
        serde_json::to_writer(&mut out, value)?;
    } else {
        serde_json::to_writer_pretty(&mut out, value)?;
    }
    writeln!(out)?;
    Ok(())
}

// Only explicit CLI/MCP memory configuration calls this helper. The full bounded
// snapshot preserves citation ambiguity and source proofs; selection stays SQL.
fn learning_annotations(
    memory_dir: &Path,
    graph: &GraphResult,
    snapshot: Option<&GraphSnapshot>,
    token_budget: Option<usize>,
) -> serde_json::Value {
    use graf::memory::{ReflectArgs, learning_overlay};
    use serde_json::json;

    let Some(snapshot) = snapshot else {
        return json!({"learning_notice":"Learning omitted: bounded snapshot unavailable."});
    };
    if snapshot.generation != graph.generation {
        return json!({"learning_notice":"Learning omitted: snapshot generation differs from query result."});
    }
    // Deliberately recompute: memory and live source files can change while the
    // indexed generation remains unchanged. This API writes neither out nor a sidecar.
    let args = ReflectArgs {
        memory_dir: memory_dir.to_owned(),
        out: PathBuf::new(),
        half_life_days: 30.0,
        min_corroboration: 2,
        if_stale: false,
    };
    let Ok(overlay) = learning_overlay(&args, Some(snapshot)) else {
        return json!({"learning_notice":"Learning omitted: memory could not be read or validated."});
    };
    let budget = token_budget.map_or(8 * 1024, |tokens| {
        tokens
            .saturating_mul(4)
            .saturating_sub(serde_json::to_vec(graph).map_or(usize::MAX, |bytes| bytes.len()))
    });
    let selected: Vec<_> = graph
        .nodes
        .iter()
        .filter_map(|node| {
            overlay
                .nodes
                .get(&node.id)
                .map(|learning| (&node.id, learning))
        })
        .collect();
    let mut annotation = json!({"learning":{
        "schema_version":overlay.schema_version,
        "snapshot_hash":overlay.snapshot_hash,
        "generated_unix_secs":overlay.generated_unix_secs,
        "status":"truncated", "omitted_nodes":selected.len(), "nodes":{}
    }});
    let fits = |value: &serde_json::Value| {
        serde_json::to_vec(value).is_ok_and(|bytes| bytes.len() <= budget)
    };
    if !fits(&annotation) {
        return json!({"learning_notice":"Learning omitted: annotation budget exhausted."});
    }
    let total = selected.len();
    let mut included = 0;
    for (id, learning) in selected {
        annotation["learning"]["nodes"][id] = json!(learning);
        // Count the serialized envelope too. Keep whole entries and a stable
        // prefix of the already selected nodes; never spend their graph budget.
        if !fits(&annotation) {
            annotation["learning"]["nodes"]
                .as_object_mut()
                .unwrap()
                .remove(id);
            break;
        }
        included += 1;
    }
    annotation["learning"]["omitted_nodes"] = json!(total - included);
    if included == total {
        annotation["learning"]["status"] = json!("complete");
    } else {
        // Notices are response metadata, outside the graph-payload estimate.
        annotation["learning_notice"] = json!("Learning truncated: annotation budget exhausted.");
    }
    annotation
}

fn print_show_output(output: Output, annotation: serde_json::Value, json: bool) -> Result<()> {
    if json {
        let mut value = serde_json::to_value(output)?;
        value
            .as_object_mut()
            .context("show response must be an object")?
            .extend(annotation.as_object().unwrap().clone());
        return print_value(&value, true);
    }
    print_output(output, false)?;
    let mut out = io::stdout().lock();
    if let Some(nodes) = annotation["learning"]["nodes"].as_object() {
        for (id, learning) in nodes {
            writeln!(
                out,
                "Lesson {}: {} (useful={}, negative={}, verified={}, unverified={}); {}",
                human(id),
                human(learning["status"].as_str().unwrap_or("unmarked")),
                learning["useful"],
                learning["negative"],
                learning["verified_useful"],
                learning["unverified"],
                human(learning["reason"].as_str().unwrap_or(""))
            )?;
        }
    }
    if let Some(notice) = annotation["learning_notice"].as_str() {
        writeln!(out, "{}", human(notice))?;
    }
    Ok(())
}

fn native_root(db: &Path) -> Result<PathBuf> {
    let stats = Store::open_read_only(db)?.stats()?;
    ensure!(
        stats.kind == "native",
        "this command requires a native index"
    );
    Ok(PathBuf::from(
        stats.root.context("native index has no source root")?,
    ))
}

fn retryable_update(error: &anyhow::Error) -> bool {
    error.is::<graf::store::StaleStore>() || error.chain().any(|cause| {
        matches!(cause.downcast_ref::<rusqlite::Error>(), Some(rusqlite::Error::SqliteFailure(code, _)) if matches!(code.code, rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked))
    })
}

fn run(cli: Cli) -> Result<()> {
    if let Command::HookGuard(args) = &cli.command {
        if let Some(value) = hook_guard::run(
            args.platform,
            args.project.as_deref(),
            cli.db.as_deref(),
            io::stdin().lock(),
        ) {
            // Hook output is independent of normal CLI formatting and notices.
            // A closed host pipe must not turn optional context into a denial.
            let _ = writeln!(io::stdout().lock(), "{value}");
        }
        return Ok(());
    }
    if !matches!(
        cli.command,
        Command::Install(_) | Command::Uninstall(_) | Command::Hook(_) | Command::Serve(_)
    ) {
        let current = std::env::current_dir().ok();
        let project = match &cli.command {
            Command::Index { path, .. } => Some(path.as_path()),
            Command::Add { project, .. } => Some(project.as_path()),
            Command::Provider(args) => args.project.as_deref(),
            _ => cli
                .db
                .as_deref()
                .and_then(Path::parent)
                .filter(|directory| directory.file_name().is_some_and(|name| name == ".graf"))
                .and_then(Path::parent)
                .or(current.as_deref()),
        };
        for notice in agent_setup::guidance_notices(project) {
            eprintln!("{}", human(&notice));
        }
    }
    match &cli.command {
        Command::Clone {
            url,
            output,
            branch,
            index,
            refresh,
        } => {
            let parsed =
                reqwest::Url::parse(url).context("expected an HTTPS GitHub repository URL")?;
            ensure!(
                parsed.scheme() == "https"
                    && parsed.host_str() == Some("github.com")
                    && parsed.username().is_empty()
                    && parsed.password().is_none()
                    && parsed.query().is_none()
                    && parsed.fragment().is_none()
                    && parsed.port().is_none(),
                "expected an HTTPS GitHub repository URL without credentials or query parameters"
            );
            let parts: Vec<_> = parsed.path().trim_matches('/').split('/').collect();
            ensure!(
                parts.len() == 2
                    && parts.iter().all(|p| !matches!(*p, "" | "." | "..")
                        && p.bytes()
                            .all(|c| c.is_ascii_alphanumeric() || b"_.-".contains(&c))),
                "expected github.com/OWNER/REPOSITORY"
            );
            let name = parts[1].strip_suffix(".git").unwrap_or(parts[1]);
            ensure!(
                !name.is_empty() && name != "." && name != "..",
                "invalid repository name"
            );
            let output = match output {
                Some(output) => output.clone(),
                None => PathBuf::from(
                    std::env::var_os("HOME")
                        .or_else(|| std::env::var_os("USERPROFILE"))
                        .context("home directory unavailable; pass --output")?,
                )
                .join(".graf/repos")
                .join(parts[0])
                .join(name),
            };
            ensure!(*index || cli.db.is_none(), "--db requires clone --index");
            let reused = output.try_exists()?;
            if reused {
                ensure!(
                    output.is_dir() && output.join(".git").try_exists()?,
                    "clone destination is not a Git checkout"
                );
                let git = |args: &[&str]| -> Result<String> {
                    let result = std::process::Command::new("git")
                        .arg("-C")
                        .arg(&output)
                        .args(args)
                        .env("GIT_TERMINAL_PROMPT", "0")
                        .output()
                        .context("cannot launch Git")?;
                    ensure!(
                        result.status.success(),
                        "Git cache operation failed; inspect the checkout and repository access"
                    );
                    Ok(String::from_utf8(result.stdout)
                        .context("Git returned non-UTF-8 metadata")?
                        .trim_end_matches(['\r', '\n'])
                        .to_owned())
                };
                let origin = git(&["config", "--get", "remote.origin.url"])?;
                ensure!(
                    origin
                        .trim_end_matches('/')
                        .trim_end_matches(".git")
                        .eq_ignore_ascii_case(
                            parsed
                                .as_str()
                                .trim_end_matches('/')
                                .trim_end_matches(".git")
                        ),
                    "existing checkout has a different origin; choose another --output"
                );
                let current = git(&["symbolic-ref", "--short", "HEAD"])?;
                if let Some(branch) = branch {
                    ensure!(
                        branch == &current,
                        "existing checkout uses a different branch; choose another --output"
                    );
                }
                if *refresh {
                    git(&["pull", "--ff-only", "--", "origin", &current])?;
                }
            } else {
                if let Some(parent) = output.parent().filter(|p| !p.as_os_str().is_empty()) {
                    std::fs::create_dir_all(parent)?;
                }
                let mut command = std::process::Command::new("git");
                command
                    .args(["clone", "--depth", "1"])
                    .env("GIT_TERMINAL_PROMPT", "0");
                if let Some(branch) = branch {
                    command.arg("--branch").arg(branch);
                }
                let result = command
                    .arg("--")
                    .arg(parsed.as_str())
                    .arg(&output)
                    .output()
                    .context("cannot launch Git")?;
                ensure!(
                    result.status.success(),
                    "Git clone failed; check repository access, branch and destination"
                );
            }
            let report = if *index {
                let db = cli
                    .db
                    .clone()
                    .unwrap_or_else(|| output.join(".graf/index.db"));
                Some(graf::index::run(&output, &db)?)
            } else {
                None
            };
            return print_value(
                &serde_json::json!({"status":if reused { if *refresh { "refreshed" } else { "reused" } } else { "cloned" },"path":output,"index":report}),
                cli.json,
            );
        }
        Command::Extended(args) => return commands::run(args, cli.db.as_deref(), cli.json),
        Command::Connect(args) => return connect::run(args, cli.db.as_deref(), cli.json),
        Command::Provider(args) => return print_value(&extraction::provider(args)?, cli.json),
        Command::Cache(args) => return print_value(&extraction::cache(args)?, cli.json),
        Command::Install(args) => {
            ensure!(cli.db.is_none(), "install selects a project; omit --db");
            return print_value(&agent_setup::install(args)?, cli.json);
        }
        Command::Uninstall(args) => {
            ensure!(cli.db.is_none(), "uninstall selects a project; omit --db");
            return print_value(&agent_setup::uninstall(args)?, cli.json);
        }
        Command::Hook(args) => {
            ensure!(cli.db.is_none(), "hook selects a project; omit --db");
            return print_value(&agent_setup::hook(args)?, cli.json);
        }
        _ => (),
    }

    if let Command::Switch(args) = cli.command {
        ensure!(
            cli.db.is_none(),
            "switch uses the project's .graf/index.db; omit --db"
        );
        let report = switch::run(args)?;
        if cli.json {
            println!("{}", serde_json::to_string(&report)?);
        } else {
            println!(
                "{}: {} nodes, {} edges.\nMCP config: {}\nDatabase: {}",
                report.status,
                report.nodes,
                report.edges,
                human(&report.config.display().to_string()),
                human(&report.database.display().to_string())
            );
            if report.status != "undone" {
                println!(
                    "Verified Graf MCP. Restart your client to load Graf's tools.\nImported graphs are snapshots; Graphify generation remains available.\nUndo: graf switch --undo (use the same --project and --config, if supplied)"
                );
            } else {
                println!(
                    "Restored the MCP configuration; the imported database was retained. Restart your client."
                );
            }
        }
        return Ok(());
    }
    let db = database(&cli)?;
    let show_learning = match &cli.command {
        Command::Show(args) => args
            .memory_dir
            .as_ref()
            .map(|dir| (dir.clone(), args.symbol.navigation.budget)),
        _ => None,
    };
    let command = match cli.command {
        Command::Switch(_)
        | Command::Install(_)
        | Command::Uninstall(_)
        | Command::Hook(_)
        | Command::HookGuard(_)
        | Command::Extended(_)
        | Command::Connect(_)
        | Command::Provider(_)
        | Command::Cache(_)
        | Command::Clone { .. } => unreachable!(),
        Command::Index { path, extraction } => {
            let options = extraction.configure(index::stored_options(&db)?, &path, &db)?;
            return print_output(
                Output::Index(index::run_with_options(&path, &db, &options)?),
                cli.json,
            );
        }
        Command::Add {
            source,
            name,
            contributor,
            captured_at_unix_secs,
            project,
            extraction,
        } => {
            let root = project.canonicalize().context("cannot resolve project")?;
            ensure!(root.is_dir(), "project must be a directory");
            let options = extraction.configure(index::stored_options(&db)?, &root, &db)?;
            if db.try_exists()? {
                let stats = Store::open_read_only(&db)?.stats()?;
                ensure!(
                    stats.kind == "native" && stats.root.as_deref() == root.to_str(),
                    "add requires this project's native graph"
                );
            }
            let capture = graf::ingest::CaptureMetadata {
                contributor,
                captured_at_unix_secs,
            };
            let (record, report) = graf::sources::add_and_index(
                &root,
                &db,
                &source,
                name.as_deref(),
                &options,
                &capture,
            )?;
            return print_value(
                &serde_json::json!({"source":record.source,"path":record.facts.path,"index":report}),
                cli.json,
            );
        }
        Command::CheckUpdate => {
            return print_value(&index::check_update(&native_root(&db)?, &db)?, cli.json);
        }
        Command::Watch {
            interval_ms,
            iterations,
        } => {
            let root = native_root(&db)?;
            let mut polls = 0;
            loop {
                let result = (|| -> Result<()> {
                    if !index::check_update(&root, &db)?.fresh {
                        print_output(Output::Index(index::run(&root, &db)?), cli.json)?;
                    }
                    Ok(())
                })();
                let pending = match result {
                    Err(error) if retryable_update(&error) => {
                        eprintln!(
                            "index is busy or changed concurrently; retrying at the next watch poll"
                        );
                        Some(error)
                    }
                    Err(error) => return Err(error),
                    Ok(()) => None,
                };
                polls += 1;
                if iterations.is_some_and(|limit| polls >= limit) {
                    return pending.map_or(Ok(()), Err);
                }
                std::thread::sleep(std::time::Duration::from_millis(interval_ms));
            }
        }
        Command::Update {
            timing,
            force,
            refresh_cache,
            allow_semantic_shrink,
        } => {
            let stats = Store::open_read_only(&db)?.stats()?;
            ensure!(
                stats.kind == "native",
                "update requires a native index, not {}",
                stats.kind
            );
            let root = stats
                .root
                .context("native index has no recorded source root")?;
            let mut options = index::stored_options(&db)?;
            options.force = force || refresh_cache;
            options.timing = timing;
            options.ingest.force_cache_refresh = refresh_cache;
            options.allow_semantic_shrink = allow_semantic_shrink;
            return print_output(
                Output::Index(index::run_with_options(Path::new(&root), &db, &options)?),
                cli.json,
            );
        }
        Command::Import { format } => {
            let (graph, refresh) = match format {
                ImportFormat::Graphify {
                    file,
                    format,
                    refresh,
                } => (
                    match format {
                        SnapshotFormat::NodeLink => import::read_graphify(&file)?,
                        SnapshotFormat::Export => import::read_graphify_export(&file)?,
                    },
                    refresh,
                ),
                ImportFormat::Graf { file, refresh } => (graf::snapshot::read(&file)?, refresh),
            };
            if let Some(parent) = db.parent().filter(|p| !p.as_os_str().is_empty()) {
                std::fs::create_dir_all(parent)?;
            }
            let mut store = Store::create(&db)?;
            let stats = if refresh {
                store.refresh_import(graph)?
            } else {
                store.import_graph(graph)?
            };
            return print_output(Output::Stats(stats), cli.json);
        }
        Command::Serve(args) => {
            return tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?
                .block_on(mcp::serve(db, args));
        }
        Command::Query(a) => ReadCommand::Query(a),
        Command::Show(a) => ReadCommand::Show(a.symbol),
        Command::Callers(a) => ReadCommand::Callers(a),
        Command::Callees(a) => ReadCommand::Callees(a),
        Command::Impact(a) => ReadCommand::Impact(a),
        Command::Path(a) => ReadCommand::Path(a),
        Command::Stats => ReadCommand::Stats,
    };
    let (kind, question) = match &command {
        ReadCommand::Query(a) => ("query", a.text.clone()),
        ReadCommand::Show(a) => ("show", a.symbol.clone()),
        ReadCommand::Callers(a) => ("callers", a.symbol.clone()),
        ReadCommand::Callees(a) => ("callees", a.symbol.clone()),
        ReadCommand::Impact(a) => ("impact", a.symbol.clone()),
        ReadCommand::Path(a) => ("path", format!("{} -> {}", a.source, a.target)),
        ReadCommand::Stats => ("stats", String::new()),
    };
    let start = std::time::Instant::now();
    let output = read(&db, command)?;
    if let Some(path) = cli.query_log.as_deref() {
        let response = serde_json::to_value(&output)?;
        let graph = response.get("result").unwrap_or(&response);
        let graph = graph.get("graph").unwrap_or(graph);
        let mut record = serde_json::json!({
            "timestamp_unix_secs":std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs(),
            "kind":kind,"question":question,"corpus":db,
            "duration_ms":start.elapsed().as_millis(),"generation":graph.get("generation"),
            "nodes":graph.get("nodes").and_then(|v|v.as_array()).map(Vec::len),
            "response_bytes":serde_json::to_vec(&response)?.len(),
        });
        if cli.log_responses {
            record["response"] = response;
        }
        if append_query_log(path, &record).is_err() {
            eprintln!("graf: query log could not be written; query result is still available");
        }
    }
    if let Some((memory_dir, budget)) = show_learning {
        let graph = match &output {
            Output::Graph(graph) => graph,
            Output::Search(result) => &result.graph,
            _ => unreachable!("show returns a graph or search result"),
        };
        let snapshot = mcp::snapshot_for_learning(&db).ok();
        let annotation = learning_annotations(&memory_dir, graph, snapshot.as_ref(), budget);
        return print_show_output(output, annotation, cli.json);
    }
    print_output(output, cli.json)
}

fn append_query_log(path: &Path, record: &serde_json::Value) -> Result<()> {
    use std::io::{BufRead, Read, Seek};
    let mut bytes = serde_json::to_vec(record)?;
    ensure!(bytes.len() <= 1024 * 1024, "query log record exceeds 1 MiB");
    bytes.push(b'\n');
    let mut options = std::fs::OpenOptions::new();
    options.read(true).append(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    if let Ok(metadata) = std::fs::symlink_metadata(path) {
        ensure!(metadata.is_file(), "query log must be a regular file");
    }
    let mut file = options.open(path)?;
    ensure!(
        file.metadata()?.is_file(),
        "query log must be a regular file"
    );
    if file.metadata()?.len() > 0 {
        let mut first = Vec::new();
        std::io::BufReader::new((&mut file).take(1024 * 1024 + 1)).read_until(b'\n', &mut first)?;
        ensure!(
            serde_json::from_slice::<serde_json::Value>(&first)?.is_object(),
            "existing log must contain JSON objects"
        );
        file.seek(std::io::SeekFrom::End(-1))?;
        let mut tail = [0];
        file.read_exact(&mut tail)?;
        if tail[0] != b'\n' {
            bytes.insert(0, b'\n');
        }
    }
    file.write_all(&bytes)?;
    Ok(())
}

fn main() -> std::process::ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) => {
            if error.use_stderr() {
                // Preserve argument controls until we can visibly escape them;
                // StyledStr's plain Display would silently strip ANSI input.
                eprintln!("{}", human(error.render().ansi().to_string().trim_end()));
            } else {
                // Help/version contain only static command metadata.
                let _ = error.print();
            }
            return std::process::ExitCode::from(error.exit_code() as u8);
        }
    };
    match run(cli) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("graf: {}", human(&format!("{error:#}")));
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod learning_tests {
    use super::*;

    #[test]
    fn learning_omits_proof_from_a_snapshot_newer_than_the_selected_graph() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("source");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("policy.py"), "def policy():\n    return 1\n").unwrap();
        let db = dir.path().join("graph.db");
        index::run(&root, &db).unwrap();
        let initial = mcp::snapshot_for_learning(&db).unwrap();
        let id = &initial
            .nodes
            .iter()
            .find(|node| node.label == "policy" && node.kind == "function")
            .unwrap()
            .id;
        let selected = Store::open_read_only(&db)
            .unwrap()
            .neighbors_resolved(id, &graf::query::SearchOptions::default())
            .unwrap();
        std::fs::write(root.join("policy.py"), "def policy():\n    return 2\n").unwrap();
        index::run(&root, &db).unwrap();
        let current = mcp::snapshot_for_learning(&db).unwrap();
        assert_ne!(current.generation, selected.graph.generation);
        let memory = dir.path().join("memory");
        let annotated = learning_annotations(&memory, &selected.graph, Some(&current), None);
        assert!(annotated.get("learning").is_none());
        assert!(
            annotated["learning_notice"]
                .as_str()
                .unwrap()
                .contains("generation differs")
        );
        assert!(!memory.exists());
    }
}
