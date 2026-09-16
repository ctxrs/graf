use std::path::PathBuf;

use anyhow::Result;
use graf::store::Store;
use rmcp::{
    ServerHandler, ServiceExt,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerConfig},
    tool, tool_handler, tool_router,
};

use crate::{ImpactArgs, PathArgs, QueryArgs, ReadCommand, SymbolArgs, read};

#[derive(Clone)]
struct Graf {
    db: PathBuf,
    tool_router: ToolRouter<Self>,
}

impl Graf {
    async fn execute(&self, command: ReadCommand) -> CallToolResult {
        let db = self.db.clone();
        match tokio::task::spawn_blocking(move || {
            let output = read(&db, command)?;
            Ok::<_, anyhow::Error>(serde_json::to_value(output)?)
        })
        .await
        {
            Ok(Ok(value)) => CallToolResult::structured(value),
            Ok(Err(error)) => CallToolResult::error(vec![ContentBlock::text(format!("{error:#}"))]),
            Err(error) => CallToolResult::error(vec![ContentBlock::text(format!(
                "query worker failed: {error}"
            ))]),
        }
    }
}

#[tool_router]
impl Graf {
    #[tool(
        description = "Search the indexed snapshot and traverse a bounded neighborhood. No live source reads or freshness check. Truncation and unresolved references are explicit.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn query(&self, Parameters(args): Parameters<QueryArgs>) -> CallToolResult {
        self.execute(ReadCommand::Query(args)).await
    }

    #[tool(
        description = "Show an exact node ID or unique symbol plus depth-one neighbors in both directions. Ambiguous names return an error listing IDs.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn show(&self, Parameters(args): Parameters<SymbolArgs>) -> CallToolResult {
        self.execute(ReadCommand::Show(args)).await
    }

    #[tool(
        description = "Show immediate incoming calls to an exact node ID or unique symbol in the indexed snapshot.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn callers(&self, Parameters(args): Parameters<SymbolArgs>) -> CallToolResult {
        self.execute(ReadCommand::Callers(args)).await
    }

    #[tool(
        description = "Show immediate outgoing calls from an exact node ID or unique symbol, including unresolved references within result bounds.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn callees(&self, Parameters(args): Parameters<SymbolArgs>) -> CallToolResult {
        self.execute(ReadCommand::Callees(args)).await
    }

    #[tool(
        description = "Follow incoming calls to find potential impact, default depth three. Static graph reachability is not proof of runtime behavior.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn impact(&self, Parameters(args): Parameters<ImpactArgs>) -> CallToolResult {
        self.execute(ReadCommand::Impact(args)).await
    }

    #[tool(
        description = "Find a bounded path between exact IDs or unique symbols; outgoing by default. found=false with graph.truncated=true means an incomplete search, not proof that no path exists. Snapshot schema_version and generation are in graph.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn path(&self, Parameters(args): Parameters<PathArgs>) -> CallToolResult {
        self.execute(ReadCommand::Path(args)).await
    }

    #[tool(
        description = "Report graph-wide counts, index generation, source coverage, and diagnostics for the configured snapshot. Does not check live worktree freshness.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn stats(&self) -> CallToolResult {
        self.execute(ReadCommand::Stats).await
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for Graf {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("graf", env!("CARGO_PKG_VERSION")))
            .with_instructions("Graf exposes a read-only indexed snapshot of one configured database. Reads do not check live worktree freshness or update the graph. Run graf index/update explicitly outside MCP. Locations refer to the indexed snapshot, not necessarily current files. Results retain schema_version and generation; path nests these in graph. Bounds: depth 0..6, limit 1..500, at most 20 search seeds and 5000 examined edges. Inspect truncated and unresolved results before drawing conclusions.")
    }
}

pub async fn serve(db: PathBuf) -> Result<()> {
    // Validate without creating a database; pin discovery once for this server.
    let db = db.canonicalize()?;
    drop(Store::open(&db)?);
    let server = Graf {
        db,
        tool_router: Graf::tool_router(),
    };
    server
        .serve(rmcp::transport::stdio())
        .await?
        .waiting()
        .await?;
    Ok(())
}
