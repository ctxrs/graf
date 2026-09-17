use std::{
    collections::BTreeMap,
    net::{IpAddr, Ipv4Addr},
    path::PathBuf,
    sync::{Arc, Mutex, OnceLock},
};

use anyhow::{Context, Result, bail, ensure};
use axum::{extract::Request, http::StatusCode, middleware::Next, response::Response};
use futures_util::StreamExt;
use graf::{
    analysis::{self, AnalysisOptions, AnalysisReport},
    export::{self, ExportFormat},
    model::{Direction, GraphSnapshot, QueryOptions},
    query::{SearchOptions, Traversal},
    store::Store,
};
use rmcp::{
    ErrorData, RoleServer, ServerHandler, ServiceExt,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{
        CallToolResult, ClientJsonRpcMessage, ContentBlock, Implementation, ListResourcesResult,
        PaginatedRequestParams, ReadResourceRequestParams, ReadResourceResponse,
        ReadResourceResult, Resource, ResourceContents, ServerCapabilities, ServerConfig,
        ServerJsonRpcMessage,
    },
    service::RequestContext,
    tool, tool_handler, tool_router,
    transport::{
        async_rw::JsonRpcMessageCodec,
        streamable_http_server::{
            StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
        },
    },
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::Semaphore;
use tokio_util::codec::{FramedRead, FramedWrite};

use crate::{ImpactArgs, PathArgs, QueryArgs, ReadCommand, SymbolArgs, read};

const MAX_MESSAGE: usize = 1024 * 1024;
const MAX_PROJECTS: usize = 32;
const MAX_ANALYSIS_NODES: usize = 5_000;
const MAX_ANALYSIS_EDGES: usize = 20_000;
const MAX_SNAPSHOT_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, Copy, Default, clap::ValueEnum, PartialEq, Eq)]
pub enum Transport {
    #[default]
    Stdio,
    Http,
}

#[derive(Debug, clap::Args)]
pub struct ServeArgs {
    #[arg(long, value_enum, default_value = "stdio")]
    pub transport: Transport,
    /// HTTP bind address. Nonloopback listeners require --bearer-token-env.
    #[arg(long, default_value_t = IpAddr::V4(Ipv4Addr::LOCALHOST))]
    pub host: IpAddr,
    #[arg(long, default_value_t = 8080)]
    pub port: u16,
    #[arg(long, default_value = "/mcp")]
    pub path: String,
    /// Name of an environment variable containing a bearer token (never the token itself).
    #[arg(long)]
    pub bearer_token_env: Option<String>,
    /// Additional exact HTTP Host authorities, e.g. example.org:8080. No wildcards.
    #[arg(long)]
    pub allowed_host: Vec<String>,
    /// Register another read-only database as NAME=DB. Tools select the name with project.
    #[arg(long, value_name = "NAME=DB")]
    pub project: Vec<String>,
}

#[derive(Deserialize, JsonSchema)]
struct Routed<T> {
    /// Registered project name; omitted selects default. Filesystem paths are not accepted.
    project: Option<String>,
    #[serde(flatten)]
    args: T,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ProjectArgs {
    /// Registered project name; omitted selects default.
    project: Option<String>,
}

fn ten() -> usize {
    10
}
fn hundred() -> usize {
    100
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct HubArgs {
    project: Option<String>,
    #[serde(default = "ten")]
    #[schemars(range(min = 1, max = 500))]
    top_n: usize,
    /// Suppress degrees above the empirical degree percentile, 0..100.
    #[schemars(range(min = 0, max = 100))]
    exclude_hubs_percentile: Option<f64>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CommunityArgs {
    project: Option<String>,
    community_id: usize,
    #[serde(default = "token_budget")]
    #[schemars(range(min = 1, max = 100000))]
    token_budget: usize,
    #[serde(default = "hundred")]
    #[schemars(range(min = 1, max = 500))]
    limit: usize,
}

fn three() -> u32 {
    3
}
fn six() -> u32 {
    6
}
fn token_budget() -> usize {
    2000
}

#[derive(Deserialize, JsonSchema, Default)]
#[serde(rename_all = "lowercase")]
enum QueryMode {
    #[default]
    Bfs,
    Dfs,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct GraphQueryArgs {
    project: Option<String>,
    question: String,
    #[serde(default)]
    mode: QueryMode,
    #[serde(default = "three")]
    #[schemars(range(min = 0, max = 6))]
    depth: u32,
    #[serde(default = "hundred")]
    #[schemars(range(min = 1, max = 500))]
    limit: usize,
    #[serde(default = "token_budget")]
    #[schemars(range(min = 1, max = 100000))]
    token_budget: usize,
    #[serde(default)]
    context_filter: Vec<String>,
    #[serde(default)]
    files: Vec<String>,
    #[serde(default)]
    kinds: Vec<String>,
    #[serde(default)]
    induced_edges: bool,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct NodeArgs {
    project: Option<String>,
    /// Exact ID, unique label or file::symbol.
    label: String,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct NeighborArgs {
    project: Option<String>,
    label: String,
    relation_filter: Option<String>,
    #[serde(default = "token_budget")]
    #[schemars(range(min = 1, max = 100000))]
    token_budget: usize,
    #[serde(default = "hundred")]
    #[schemars(range(min = 1, max = 500))]
    limit: usize,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ShortestPathArgs {
    project: Option<String>,
    source: String,
    target: String,
    /// Graf bounds path searches to six hops; default six.
    #[serde(default = "six")]
    #[schemars(range(min = 0, max = 6))]
    max_hops: u32,
    #[serde(default)]
    undirected: bool,
    #[serde(default = "hundred")]
    #[schemars(range(min = 1, max = 500))]
    limit: usize,
    #[serde(default = "token_budget")]
    #[schemars(range(min = 1, max = 100000))]
    token_budget: usize,
}

struct Derived {
    snapshot: GraphSnapshot,
    analysis: AnalysisReport,
    report: OnceLock<std::result::Result<String, String>>,
}

struct Project {
    db: PathBuf,
    cache: Mutex<Option<Arc<Derived>>>,
}

impl Project {
    fn derived(&self) -> Result<Arc<Derived>> {
        // One cached generation per registered database; ordinary SQL reads skip this path.
        let mut cache = self
            .cache
            .lock()
            .map_err(|_| anyhow::anyhow!("analysis worker failed"))?;
        let store = Store::open_read_only(&self.db)?;
        let stats = store.stats()?;
        if let Some(value) = cache
            .as_ref()
            .filter(|v| v.snapshot.generation == stats.generation)
        {
            return Ok(value.clone());
        }
        ensure!(
            stats.nodes <= MAX_ANALYSIS_NODES
                && stats.edges <= MAX_ANALYSIS_EDGES
                && stats.unresolved_references <= MAX_ANALYSIS_EDGES,
            "MCP analysis limit is 5000 nodes, 20000 edges and 20000 unresolved references; use explicit CLI analysis/export for larger graphs"
        );
        // Bound payload loading too, including unusually large metadata in a small graph.
        ensure!(
            std::fs::metadata(&self.db)?.len() <= 64 * 1024 * 1024,
            "MCP analysis database limit is 64 MiB; use CLI analysis/export"
        );
        let snapshot = store.snapshot_bounded(
            MAX_ANALYSIS_NODES,
            MAX_ANALYSIS_EDGES,
            MAX_ANALYSIS_EDGES,
            MAX_SNAPSHOT_BYTES,
        )?;
        ensure!(
            snapshot.nodes.len() <= MAX_ANALYSIS_NODES
                && snapshot.edges.len() <= MAX_ANALYSIS_EDGES,
            "snapshot exceeds MCP analysis limits"
        );
        ensure!(
            serde_json::to_vec(&snapshot)?.len() <= MAX_SNAPSHOT_BYTES,
            "MCP analysis snapshot limit is 8 MiB; use CLI analysis/export"
        );
        let analysis = analysis::analyze(&snapshot, &AnalysisOptions::default())?;
        let value = Arc::new(Derived {
            snapshot,
            analysis,
            report: OnceLock::new(),
        });
        *cache = Some(value.clone());
        Ok(value)
    }
}

#[derive(Clone)]
struct Graf {
    projects: Arc<BTreeMap<String, Arc<Project>>>,
    workers: Arc<Semaphore>,
    tool_router: ToolRouter<Self>,
}

impl Graf {
    async fn run<T: Send + 'static>(
        &self,
        name: Option<String>,
        operation: impl FnOnce(&Project) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let project = self
            .projects
            .get(name.as_deref().unwrap_or("default"))
            .context("unknown project; use a name registered with --project NAME=DB")?
            .clone();
        let permit = self.workers.clone().try_acquire_owned().context(
            "MCP query capacity reached (4 workers); retry after an active query completes",
        )?;
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            operation(&project)
        })
        .await
        .context("query worker failed")?
    }

    async fn execute(&self, name: Option<String>, command: ReadCommand) -> CallToolResult {
        tool_result(
            self.run(name, move |p| {
                Ok(serde_json::to_value(read(&p.db, command)?)?)
            })
            .await,
        )
    }
}

fn validate_tokens(budget: usize) -> Result<()> {
    ensure!(
        (1..=100_000).contains(&budget),
        "token_budget must be between 1 and 100000"
    );
    Ok(())
}

fn tool_result(result: Result<Value>) -> CallToolResult {
    match result.and_then(|value| {
        ensure!(
            serde_json::to_vec(&value)?.len() <= MAX_MESSAGE / 2,
            "tool response exceeds 512 KiB; reduce the result limit or use CLI export"
        );
        Ok(value)
    }) {
        Ok(value) => CallToolResult::structured(value),
        Err(error) => CallToolResult::error(vec![ContentBlock::text(format!("{error:#}"))]),
    }
}

fn graph_stats(derived: &Derived) -> Value {
    let a = &derived.analysis;
    let total = derived.snapshot.edges.len();
    let percentages: BTreeMap<_, _> = a
        .confidence_counts
        .iter()
        .map(|(key, count)| {
            (
                key,
                if total == 0 {
                    0.0
                } else {
                    *count as f64 * 100.0 / total as f64
                },
            )
        })
        .collect();
    json!({"schema_version": a.schema_version, "generation": a.generation,
        "nodes": derived.snapshot.nodes.len(), "edges": total,
        "communities": a.communities.len(), "confidence_counts": a.confidence_counts,
        "confidence_percentages": percentages, "methodology": a.methodology})
}

fn hubs(derived: &Derived, top: usize, percentile: Option<f64>) -> Result<Value> {
    ensure!((1..=500).contains(&top), "top_n must be between 1 and 500");
    let metrics = &derived.analysis.nodes;
    let cutoff = if let Some(p) = percentile {
        ensure!(
            p.is_finite() && (0.0..=100.0).contains(&p),
            "percentile must be between 0 and 100"
        );
        let mut degrees: Vec<_> = metrics.iter().map(|n| n.degree).collect();
        degrees.sort_unstable();
        if degrees.is_empty() {
            0.0
        } else {
            let index = ((degrees.len() as f64 * p / 100.0) as usize).saturating_sub(1);
            degrees[index] as f64
        }
    } else {
        f64::INFINITY
    };
    let mut nodes: Vec<_> = metrics
        .iter()
        .filter(|n| n.degree as f64 <= cutoff)
        .collect();
    nodes.sort_by(|a, b| b.degree.cmp(&a.degree).then(a.id.cmp(&b.id)));
    let truncated = nodes.len() > top;
    nodes.truncate(top);
    Ok(json!({"schema_version": derived.snapshot.schema_version,
        "generation": derived.snapshot.generation, "nodes": nodes, "truncated": truncated,
        "methodology": "Edge incidences; parallel edges counted separately, self edges count twice. Degree is not proof of architectural importance."}))
}

#[tool_router]
impl Graf {
    #[tool(
        description = "Search using bounded SQL BFS/DFS over the indexed snapshot. question, mode, depth, token_budget and context_filter follow Graphify naming. Also accepts exact file/kind filters and induced_edges. project selects a registered alias. No query logs, refresh or network calls; token counts are estimates.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn query_graph(&self, Parameters(a): Parameters<GraphQueryArgs>) -> CallToolResult {
        tool_result(
            self.run(a.project, move |p| {
                crate::nonempty(&a.question, "question")?;
                validate_tokens(a.token_budget)?;
                let options = SearchOptions {
                    graph: QueryOptions {
                        depth: a.depth,
                        limit: a.limit,
                        direction: Direction::Both,
                        relation: None,
                    },
                    traversal: match a.mode {
                        QueryMode::Bfs => Traversal::Bfs,
                        QueryMode::Dfs => Traversal::Dfs,
                    },
                    contexts: a.context_filter,
                    files: a.files,
                    kinds: a.kinds,
                    token_budget: Some(a.token_budget),
                    induced_edges: a.induced_edges,
                    infer_context: true,
                };
                Ok(serde_json::to_value(
                    Store::open_read_only(&p.db)?.query_extended(&a.question, &options)?,
                )?)
            })
            .await,
        )
    }
    #[tool(
        description = "Get an exact node ID, unique label or file::symbol from the snapshot. Ambiguity is an error. project is a registered alias.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn get_node(&self, Parameters(a): Parameters<NodeArgs>) -> CallToolResult {
        tool_result(
            self.run(a.project, move |p| {
                crate::nonempty(&a.label, "label")?;
                let options = SearchOptions {
                    graph: QueryOptions {
                        depth: 0,
                        ..Default::default()
                    },
                    ..Default::default()
                };
                Ok(serde_json::to_value(
                    Store::open_read_only(&p.db)?.neighbors_extended(&a.label, &options)?,
                )?)
            })
            .await,
        )
    }
    #[tool(
        description = "Immediate neighbors in both directions, optional exact relation_filter and estimated token_budget. Default limit 100, maximum 500; truncation is explicit.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn get_neighbors(&self, Parameters(a): Parameters<NeighborArgs>) -> CallToolResult {
        tool_result(
            self.run(a.project, move |p| {
                crate::nonempty(&a.label, "label")?;
                validate_tokens(a.token_budget)?;
                let options = SearchOptions {
                    graph: QueryOptions {
                        depth: 1,
                        limit: a.limit,
                        direction: Direction::Both,
                        relation: a.relation_filter,
                    },
                    token_budget: Some(a.token_budget),
                    ..Default::default()
                };
                Ok(serde_json::to_value(
                    Store::open_read_only(&p.db)?.neighbors_extended(&a.label, &options)?,
                )?)
            })
            .await,
        )
    }
    #[tool(
        description = "Bounded shortest path following stored direction unless undirected=true. max_hops defaults to six and is capped at six. Exact endpoints support file::symbol. Inspect found and result.graph.truncated together.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn shortest_path(&self, Parameters(a): Parameters<ShortestPathArgs>) -> CallToolResult {
        tool_result(
            self.run(a.project, move |p| {
                crate::nonempty(&a.source, "source")?;
                crate::nonempty(&a.target, "target")?;
                validate_tokens(a.token_budget)?;
                let options = SearchOptions {
                    graph: QueryOptions {
                        depth: a.max_hops,
                        limit: a.limit,
                        direction: if a.undirected {
                            Direction::Both
                        } else {
                            Direction::Outgoing
                        },
                        relation: None,
                    },
                    token_budget: Some(a.token_budget),
                    ..Default::default()
                };
                Ok(serde_json::to_value(
                    Store::open_read_only(&p.db)?.path_extended(&a.source, &a.target, &options)?,
                )?)
            })
            .await,
        )
    }

    #[tool(
        description = "Search the indexed snapshot with bounded traversal. No source reads or refresh. Optional project is a registered name.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn query(&self, Parameters(a): Parameters<Routed<QueryArgs>>) -> CallToolResult {
        self.execute(a.project, ReadCommand::Query(a.args)).await
    }
    #[tool(
        description = "Show an exact node ID or unique symbol plus depth-one neighbors in both directions. Ambiguous names are errors.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn show(&self, Parameters(a): Parameters<Routed<SymbolArgs>>) -> CallToolResult {
        self.execute(a.project, ReadCommand::Show(a.args)).await
    }
    #[tool(
        description = "Show immediate incoming calls in the indexed snapshot.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn callers(&self, Parameters(a): Parameters<Routed<SymbolArgs>>) -> CallToolResult {
        self.execute(a.project, ReadCommand::Callers(a.args)).await
    }
    #[tool(
        description = "Show immediate outgoing calls, including bounded unresolved references.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn callees(&self, Parameters(a): Parameters<Routed<SymbolArgs>>) -> CallToolResult {
        self.execute(a.project, ReadCommand::Callees(a.args)).await
    }
    #[tool(
        description = "Follow incoming calls for potential impact. Reachability is not proof of runtime behavior.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn impact(&self, Parameters(a): Parameters<Routed<ImpactArgs>>) -> CallToolResult {
        self.execute(a.project, ReadCommand::Impact(a.args)).await
    }
    #[tool(
        description = "Find a bounded path, outgoing by default. found=false with graph.truncated=true is an incomplete search. Snapshot generation is in graph.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn path(&self, Parameters(a): Parameters<Routed<PathArgs>>) -> CallToolResult {
        self.execute(a.project, ReadCommand::Path(a.args)).await
    }
    #[tool(
        description = "Counts, generation, coverage and diagnostics from the configured snapshot; no live freshness check or full graph analysis.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn stats(&self, Parameters(a): Parameters<ProjectArgs>) -> CallToolResult {
        self.execute(a.project, ReadCommand::Stats).await
    }
    #[tool(
        description = "Node, edge and computed community counts plus confidence counts and percentages. Cached per indexed generation; maximum 5000 nodes/20000 edges/20000 unresolved references, 8 MiB snapshot.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn graph_stats(&self, Parameters(a): Parameters<ProjectArgs>) -> CallToolResult {
        tool_result(
            self.run(a.project, |p| Ok(graph_stats(p.derived()?.as_ref())))
                .await,
        )
    }
    #[tool(
        description = "Highest-degree nodes, optional percentile exclusion. Default top_n 10, maximum 500. Cached analysis capped at 5000 nodes/20000 edges/20000 unresolved references, 8 MiB snapshot.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn god_nodes(&self, Parameters(a): Parameters<HubArgs>) -> CallToolResult {
        tool_result(
            self.run(a.project, move |p| {
                hubs(p.derived()?.as_ref(), a.top_n, a.exclude_hubs_percentile)
            })
            .await,
        )
    }
    #[tool(
        description = "Nodes in a computed structural community. IDs are deterministic for a generation, not upstream imported community labels. Default limit 100, maximum 500. Cached analysis capped at 5000 nodes/20000 edges/20000 unresolved references.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn get_community(&self, Parameters(a): Parameters<CommunityArgs>) -> CallToolResult {
        tool_result(self.run(a.project, move |p| {
            ensure!((1..=500).contains(&a.limit), "limit must be between 1 and 500");
            let d = p.derived()?;
            let community = d.analysis.communities.iter().find(|c| c.id == a.community_id)
                .context("unknown community_id; read the communities resource for this generation")?;
            validate_tokens(a.token_budget)?;
            let mut output = json!({"schema_version": d.snapshot.schema_version, "generation": d.snapshot.generation,
                "community_id": community.id, "cohesion": community.cohesion,
                "total_nodes": community.nodes.len(), "nodes": [], "truncated": false,
                "token_estimate": "ceil(UTF-8 JSON bytes / 4), not a model tokenizer"});
            ensure!(serde_json::to_vec(&output)?.len() <= a.token_budget * 4,
                "token budget cannot hold the response envelope");
            for id in community.nodes.iter().take(a.limit) {
                let node = d.snapshot.nodes.iter().find(|n| n.id == *id).context("community node is missing")?;
                output["nodes"].as_array_mut().unwrap().push(serde_json::to_value(node)?);
                if serde_json::to_vec(&output)?.len() > a.token_budget * 4 {
                    output["nodes"].as_array_mut().unwrap().pop();
                    break;
                }
            }
            output["truncated"] = json!(output["nodes"].as_array().unwrap().len() < community.nodes.len());
            Ok(output)
        }).await)
    }
}

const RESOURCES: &[(&str, &str)] = &[
    ("stats", "Snapshot counts and diagnostics"),
    ("graph", "Complete snapshot JSON, maximum 1 MiB"),
    (
        "report",
        "Offline structural Markdown report, maximum 1 MiB",
    ),
    ("god-nodes", "Ten highest-degree nodes"),
    ("communities", "Computed structural community IDs and sizes"),
    (
        "surprises",
        "Up to five scored structural connections with signals and full recorded edge evidence",
    ),
    ("audit", "Confidence counts and percentages"),
    (
        "questions",
        "Up to seven evidence-backed questions with rationale and candidate counts",
    ),
];

fn resource_text(project: &Project, kind: &str) -> Result<String> {
    if kind == "stats" {
        return Ok(serde_json::to_string(
            &Store::open_read_only(&project.db)?.stats()?,
        )?);
    }
    let d = project.derived()?;
    let a = &d.analysis;
    let value = match kind {
        "graph" => serde_json::to_value(&d.snapshot)?,
        "report" => {
            return d
                .report
                .get_or_init(|| {
                    export::render(&d.snapshot, ExportFormat::Markdown)
                        .map_err(|e| format!("{e:#}"))
                })
                .clone()
                .map_err(anyhow::Error::msg);
        }
        "god-nodes" => hubs(&d, 10, None)?,
        "audit" => graph_stats(&d),
        "communities" => json!({"schema_version": a.schema_version, "generation": a.generation,
            "methodology": a.methodology, "communities": a.communities.iter().map(|c|
                json!({"id": c.id, "nodes": c.nodes.len(), "cohesion": c.cohesion})).collect::<Vec<_>>()}),
        "surprises" => json!({"schema_version": a.schema_version, "generation": a.generation,
            "surprises": a.surprises, "surprise_candidates": a.surprise_candidates,
            "truncated": a.surprise_candidates > a.surprises.len(),
            "methodology": a.methodology}),
        "questions" => json!({"schema_version": a.schema_version, "generation": a.generation,
            "questions": a.suggested_questions,
            "suggested_question_candidates": a.suggested_question_candidates,
            "truncated": a.suggested_question_candidates > a.suggested_questions.len(),
            "methodology": a.methodology}),
        _ => bail!("unknown resource"),
    };
    Ok(serde_json::to_string(&value)?)
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for Graf {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().enable_resources().build())
            .with_server_info(Implementation::new("graf", env!("CARGO_PKG_VERSION")))
            .with_instructions("Graf reads only explicitly registered local databases. Optional tool project selects a registry name, never a path. Reads do not check worktree freshness, refresh, launch processes, or make network/model calls. Run index/update explicitly outside MCP. Generation identifies each snapshot. SQL queries: depth 0..6, limit 1..500, at most 20 seeds/5000 examined edges. Analysis resources/tools: cached per database generation, at most 5000 nodes/20000 edges/20000 unresolved references, 64 MiB database and 8 MiB snapshot. Resource responses at most 1 MiB, structured tool payloads 512 KiB. Four concurrent read workers. Resources enumerate registered project names; graphify:// aliases select default. HTTP is stateless Streamable HTTP at the configured path; browser Origins are rejected. Input messages at most 1 MiB. Inspect truncation and unresolved references.")
    }

    async fn list_resources(
        &self,
        request: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> std::result::Result<ListResourcesResult, ErrorData> {
        if request.is_some_and(|r| r.cursor.is_some()) {
            return Err(ErrorData::invalid_params(
                "resources are returned in one page; cursor is not supported",
                None,
            ));
        }
        let mut resources = Vec::new();
        for (kind, description) in RESOURCES {
            for prefix in ["graf://", "graphify://"] {
                resources.push(
                    Resource::new(format!("{prefix}{kind}"), format!("default/{kind}"))
                        .with_description(*description)
                        .with_mime_type(mime(kind)),
                );
            }
            for name in self.projects.keys() {
                resources.push(
                    Resource::new(
                        format!("graf://projects/{name}/{kind}"),
                        format!("{name}/{kind}"),
                    )
                    .with_description(*description)
                    .with_mime_type(mime(kind)),
                );
            }
        }
        Ok(ListResourcesResult {
            resources,
            ..Default::default()
        })
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _: RequestContext<RoleServer>,
    ) -> std::result::Result<ReadResourceResponse, ErrorData> {
        let uri = request.uri;
        let (project, kind) = if let Some(path) = uri.strip_prefix("graf://projects/") {
            path.split_once('/')
                .ok_or_else(|| ErrorData::invalid_params("invalid resource URI", None))?
        } else if let Some(kind) = uri
            .strip_prefix("graf://")
            .or_else(|| uri.strip_prefix("graphify://"))
        {
            ("default", kind)
        } else {
            return Err(ErrorData::resource_not_found("unknown resource URI", None));
        };
        if !RESOURCES.iter().any(|(key, _)| *key == kind) {
            return Err(ErrorData::resource_not_found("unknown resource URI", None));
        }
        let content_type = mime(kind);
        let kind = kind.to_owned();
        let text = self
            .run(Some(project.to_owned()), move |p| {
                let text = resource_text(p, &kind)?;
                ensure!(
                    text.len() <= MAX_MESSAGE,
                    "resource exceeds 1 MiB; use CLI export"
                );
                Ok(text)
            })
            .await
            .map_err(|e| ErrorData::invalid_params(format!("{e:#}"), None))?;
        Ok(ReadResourceResult::new(vec![
            ResourceContents::text(text, uri).with_mime_type(content_type),
        ])
        .into())
    }
}

fn mime(kind: &str) -> &'static str {
    if kind == "report" {
        "text/markdown"
    } else {
        "application/json"
    }
}

fn registered(db: PathBuf, args: &ServeArgs) -> Result<Graf> {
    ensure!(
        args.project.len() < MAX_PROJECTS,
        "at most 32 databases including default may be registered"
    );
    let mut paths = BTreeMap::from([("default".to_owned(), db)]);
    for entry in &args.project {
        let (name, path) = entry.split_once('=').context("project must be NAME=DB")?;
        ensure!(
            !name.is_empty()
                && name.len() <= 64
                && name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b)),
            "project names must be 1..64 ASCII letters, digits, underscores or hyphens"
        );
        ensure!(!path.is_empty(), "project database path must not be empty");
        ensure!(
            !paths.contains_key(name),
            "duplicate or reserved project name: {name}"
        );
        paths.insert(name.to_owned(), PathBuf::from(path));
    }
    let mut projects = BTreeMap::new();
    for (name, db) in paths {
        let db = db
            .canonicalize()
            .with_context(|| format!("cannot open registered project {name}"))?;
        drop(Store::open_read_only(&db)?);
        projects.insert(
            name,
            Arc::new(Project {
                db,
                cache: Mutex::new(None),
            }),
        );
    }
    Ok(Graf {
        projects: Arc::new(projects),
        workers: Arc::new(Semaphore::new(4)),
        tool_router: Graf::tool_router(),
    })
}

async fn authorize(
    expected: Option<blake3::Hash>,
    request: Request,
    next: Next,
) -> std::result::Result<Response, StatusCode> {
    if let Some(expected) = expected {
        let provided = request
            .headers()
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|h| h.to_str().ok())
            .and_then(|h| h.split_once(' '))
            .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("Bearer"))
            .map(|(_, token)| blake3::hash(token.as_bytes()));
        // blake3::Hash equality compares in constant time.
        if provided != Some(expected) {
            return Err(StatusCode::UNAUTHORIZED);
        }
    }
    Ok(next.run(request).await)
}

pub async fn serve(db: PathBuf, args: ServeArgs) -> Result<()> {
    let server = registered(db, &args)?;
    if args.transport == Transport::Stdio {
        ensure!(
            args.bearer_token_env.is_none() && args.allowed_host.is_empty(),
            "HTTP authentication/host options require --transport http"
        );
        // Use the SDK codec so oversized lines terminate the transport without unbounded buffering.
        let input = FramedRead::new(
            tokio::io::stdin(),
            JsonRpcMessageCodec::<ClientJsonRpcMessage>::new_with_max_length(MAX_MESSAGE),
        )
        .take_while(|result| std::future::ready(result.is_ok()))
        .map(|result| result.expect("codec errors terminate input"));
        let output = FramedWrite::new(
            tokio::io::stdout(),
            JsonRpcMessageCodec::<ServerJsonRpcMessage>::default(),
        );
        server.serve((output, input)).await?.waiting().await?;
        return Ok(());
    }
    ensure!(
        args.path.starts_with('/')
            && !args.path.contains(['{', '}', '*', '?', '#'])
            && args.path.len() <= 256,
        "HTTP path must be an absolute literal path without query or fragment"
    );
    ensure!(
        args.host.is_loopback() || args.bearer_token_env.is_some(),
        "nonloopback HTTP requires --bearer-token-env NAME"
    );
    let bearer = args.bearer_token_env.as_ref().map(|name| -> Result<_> {
        ensure!(!name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'), "invalid bearer environment variable name");
        let token = std::env::var(name).map_err(|_| anyhow::anyhow!("bearer environment variable is missing or not Unicode"))?;
        ensure!(!token.is_empty() && token.len() <= 4096 && token.bytes().all(|b| b.is_ascii_alphanumeric() || b"-._~+/=".contains(&b)),
            "bearer environment variable must contain a nonempty ASCII bearer token of at most 4096 bytes");
        Ok(blake3::hash(token.as_bytes()))
    }).transpose()?;
    let mut config = StreamableHttpServerConfig::default().enforce_origin_validation();
    config.legacy_session_mode = false;
    config.json_response = true;
    config.max_request_body_bytes = MAX_MESSAGE;
    if !args.host.is_unspecified() {
        config.allowed_hosts.push(args.host.to_string());
    }
    for host in args.allowed_host {
        ensure!(
            !host.is_empty()
                && !host.contains(['*', '/', '@'])
                && !host.chars().any(char::is_whitespace),
            "allowed-host must be an exact HTTP authority"
        );
        config.allowed_hosts.push(host);
    }
    let service = StreamableHttpService::new(
        move || Ok(server.clone()),
        Arc::new(LocalSessionManager::default()),
        config,
    );
    let app = axum::Router::new()
        .route_service(&args.path, service)
        .layer(axum::middleware::from_fn(move |request, next| {
            authorize(bearer, request, next)
        }));
    let listener = tokio::net::TcpListener::bind((args.host, args.port)).await?;
    eprintln!(
        "Graf MCP listening on http://{}{}",
        listener.local_addr()?,
        args.path
    );
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}
