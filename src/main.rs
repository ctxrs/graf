mod mcp;

use std::{
    io::{self, Write},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail, ensure};
use clap::{Args, Parser, Subcommand, ValueEnum};
use graf::{import, index, model::*, store::Store};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Parser)]
#[command(
    bin_name = "graf",
    styles = clap::builder::Styles::plain(),
    version,
    about = "Navigate a persistent local code graph",
    after_help = "Reads use the indexed snapshot; they do not check live worktree freshness. Run update explicitly to refresh native indexes."
)]
struct Cli {
    /// Database path. Otherwise discover the nearest ancestor .graf/index.db.
    #[arg(long, global = true)]
    db: Option<PathBuf>,
    /// Print machine-readable JSON instead of human-readable output.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Index Python sources into PATH/.graf/index.db unless --db is supplied.
    Index {
        #[arg(default_value = ".")]
        path: PathBuf,
    },
    /// Refresh the native source root recorded in the database.
    Update,
    /// Find symbols and explore a bounded neighborhood.
    Query(QueryArgs),
    /// Show an exact ID or unique symbol and its immediate neighbors.
    Show(SymbolArgs),
    /// Show immediate incoming calls to a symbol.
    Callers(SymbolArgs),
    /// Show immediate outgoing calls from a symbol.
    Callees(SymbolArgs),
    /// Follow incoming calls to find potentially affected symbols.
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
    /// Serve seven read-only MCP tools over stdin/stdout.
    Serve,
}

#[derive(Subcommand)]
enum ImportFormat {
    Graphify { file: PathBuf },
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
}

#[derive(Debug, Args, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ImpactArgs {
    /// Exact node ID or unique label/qualified name. Ambiguous names are errors.
    #[schemars(length(min = 1))]
    symbol: String,
    /// Maximum incoming call depth, 0..6. Default 3.
    #[arg(long, default_value_t = 3, value_parser = clap::value_parser!(u32).range(0..=6))]
    #[serde(default = "three")]
    #[schemars(range(min = 0, max = 6))]
    depth: u32,
    /// Maximum results, 1..500. Default 100.
    #[arg(long, default_value_t = 100, value_parser = clap::value_parser!(u32).range(1..=500))]
    #[serde(default = "hundred")]
    #[schemars(range(min = 1, max = 500))]
    limit: u32,
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
    let store = Store::open(db)?;
    let (symbol, options) = match command {
        ReadCommand::Stats => return Ok(Output::Stats(store.stats()?)),
        ReadCommand::Query(a) => {
            nonempty(&a.text, "text")?;
            return Ok(Output::Graph(store.query(
                &a.text,
                &options(a.depth, a.limit, a.direction.into(), a.relation)?,
            )?));
        }
        ReadCommand::Path(a) => {
            nonempty(&a.source, "source")?;
            nonempty(&a.target, "target")?;
            return Ok(Output::Path(store.path(
                &a.source,
                &a.target,
                &options(a.depth, a.limit, a.direction.into(), a.relation)?,
            )?));
        }
        ReadCommand::Show(a) => (a.symbol, options(1, a.limit, Direction::Both, None)?),
        ReadCommand::Callers(a) => (
            a.symbol,
            options(1, a.limit, Direction::Incoming, Some("calls".into()))?,
        ),
        ReadCommand::Callees(a) => (
            a.symbol,
            options(1, a.limit, Direction::Outgoing, Some("calls".into()))?,
        ),
        ReadCommand::Impact(a) => (
            a.symbol,
            options(a.depth, a.limit, Direction::Incoming, Some("calls".into()))?,
        ),
    };
    nonempty(&symbol, "symbol")?;
    Ok(Output::Graph(store.neighbors(&symbol, &options)?))
}

fn database(cli: &Cli) -> Result<PathBuf> {
    if let Some(db) = &cli.db {
        return Ok(db.clone());
    }
    match &cli.command {
        Command::Index { path } => Ok(path.join(".graf/index.db")),
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
            Output::Graph(graph) => print_graph(&mut stdout, graph)?,
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
        Output::Index(r) => diagnostics(&mut io::stderr().lock(), &r.diagnostics)?,
        Output::Stats(s) => diagnostics(&mut io::stderr().lock(), &s.diagnostics)?,
        _ => {}
    }
    Ok(())
}

async fn run(cli: Cli) -> Result<()> {
    let db = database(&cli)?;
    let command = match cli.command {
        Command::Index { path } => {
            return print_output(Output::Index(index::run(&path, &db)?), cli.json);
        }
        Command::Update => {
            let stats = Store::open(&db)?.stats()?;
            ensure!(
                stats.kind == "native",
                "update requires a native index, not {}",
                stats.kind
            );
            let root = stats
                .root
                .context("native index has no recorded source root")?;
            return print_output(Output::Index(index::run(Path::new(&root), &db)?), cli.json);
        }
        Command::Import {
            format: ImportFormat::Graphify { file },
        } => {
            let graph = import::read_graphify(&file)?;
            if let Some(parent) = db.parent().filter(|p| !p.as_os_str().is_empty()) {
                std::fs::create_dir_all(parent)?;
            }
            return print_output(
                Output::Stats(Store::create(&db)?.import_graph(graph)?),
                cli.json,
            );
        }
        Command::Serve => return mcp::serve(db).await,
        Command::Query(a) => ReadCommand::Query(a),
        Command::Show(a) => ReadCommand::Show(a),
        Command::Callers(a) => ReadCommand::Callers(a),
        Command::Callees(a) => ReadCommand::Callees(a),
        Command::Impact(a) => ReadCommand::Impact(a),
        Command::Path(a) => ReadCommand::Path(a),
        Command::Stats => ReadCommand::Stats,
    };
    print_output(read(&db, command)?, cli.json)
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::process::ExitCode {
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
    match run(cli).await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("graf: {}", human(&format!("{error:#}")));
            std::process::ExitCode::FAILURE
        }
    }
}
