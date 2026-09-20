use crate::model::*;
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OpenFlags, Transaction, params};
use std::{collections::BTreeSet, fs, path::Path, time::Duration};
use unicode_normalization::UnicodeNormalization;

const APPLICATION_ID: i64 = 0x47524146;

pub struct Store {
    pub(crate) conn: Connection,
    baseline_generation: u64,
}

const SCHEMA: &str = r#"
CREATE TABLE metadata (
    singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
    generation INTEGER NOT NULL, kind TEXT NOT NULL,
    root TEXT, coverage TEXT NOT NULL, graph_metadata TEXT NOT NULL
);
INSERT INTO metadata VALUES (1, 0, 'empty', NULL,
 '{"supported_files":0,"unsupported_files":0,"unchanged_files":0}', 'null');
CREATE TABLE files (
    path TEXT PRIMARY KEY, hash TEXT NOT NULL, module TEXT NOT NULL, diagnostics TEXT NOT NULL
);
CREATE TABLE nodes (
    id TEXT PRIMARY KEY, label TEXT NOT NULL, qualified_name TEXT, binding_key TEXT,
    file TEXT NOT NULL, owner_file TEXT REFERENCES files(path) ON DELETE CASCADE,
    payload TEXT NOT NULL, search TEXT NOT NULL
);
CREATE INDEX nodes_label ON nodes(label, id);
CREATE INDEX nodes_qualified ON nodes(qualified_name, id);
CREATE INDEX nodes_binding ON nodes(binding_key, id);
CREATE INDEX nodes_file ON nodes(file, id);
CREATE INDEX nodes_owner ON nodes(owner_file);
CREATE VIRTUAL TABLE node_search USING fts5(text);
CREATE TRIGGER nodes_insert AFTER INSERT ON nodes BEGIN
    INSERT INTO node_search(rowid, text) VALUES(new.rowid, new.search);
END;
CREATE TRIGGER nodes_delete AFTER DELETE ON nodes BEGIN
    DELETE FROM node_search WHERE rowid = old.rowid;
END;
CREATE TABLE refs (
    id TEXT PRIMARY KEY, source TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    owner_file TEXT NOT NULL REFERENCES files(path) ON DELETE CASCADE,
    relation TEXT NOT NULL, payload TEXT NOT NULL,
    resolved_target TEXT, resolution_reason TEXT NOT NULL
);
CREATE INDEX refs_source ON refs(source, id);
CREATE INDEX refs_owner ON refs(owner_file);
CREATE INDEX refs_unresolved_source ON refs(source, id) WHERE resolved_target IS NULL;
CREATE INDEX refs_unresolved_relation ON refs(source, relation, id) WHERE resolved_target IS NULL;
CREATE TABLE ref_keys (
    ref_id TEXT NOT NULL REFERENCES refs(id) ON DELETE CASCADE,
    priority INTEGER NOT NULL, binding_key TEXT NOT NULL,
    PRIMARY KEY(ref_id, priority)
);
CREATE INDEX ref_keys_binding ON ref_keys(binding_key, ref_id);
CREATE TABLE edges (
    id TEXT PRIMARY KEY,
    source TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    target TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    relation TEXT NOT NULL, directed INTEGER NOT NULL CHECK(directed IN (0, 1)),
    owner_file TEXT REFERENCES files(path) ON DELETE CASCADE,
    ref_id TEXT UNIQUE REFERENCES refs(id) ON DELETE CASCADE,
    payload TEXT NOT NULL
);
CREATE INDEX edges_source ON edges(source, id);
CREATE INDEX edges_target ON edges(target, id);
CREATE INDEX edges_source_relation ON edges(source, relation, id);
CREATE INDEX edges_target_relation ON edges(target, relation, id);
CREATE INDEX edges_source_direction ON edges(source, directed, id);
CREATE INDEX edges_target_direction ON edges(target, directed, id);
CREATE INDEX edges_source_direction_relation ON edges(source, directed, relation, id);
CREATE INDEX edges_target_direction_relation ON edges(target, directed, relation, id);
CREATE INDEX edges_owner ON edges(owner_file);
"#;

/// A concurrent writer committed after this handle captured its baseline.
#[derive(Debug)]
pub struct StaleStore;
impl std::fmt::Display for StaleStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("graph changed since this Store was opened; retry from a fresh Store")
    }
}
impl std::error::Error for StaleStore {}

impl Store {
    /// Existing files must already be Graf databases. Even an empty foreign
    /// SQLite database is not an invitation to initialize it.
    pub fn create(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent)?;
        }
        let fresh = match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
        {
            Ok(file) => {
                drop(file);
                true
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => false,
            Err(e) => return Err(e.into()),
        };
        let mut conn = connect(path)?;
        if fresh {
            conn.pragma_update(None, "journal_mode", "WAL")?;
            let tx = conn.transaction()?;
            tx.execute_batch(SCHEMA)?;
            tx.pragma_update(None, "application_id", APPLICATION_ID)?;
            tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
            tx.commit()?;
        }
        validate(&conn)?;
        let baseline_generation = generation(&conn)?;
        Ok(Self {
            conn,
            baseline_generation,
        })
    }

    pub fn open(path: &Path) -> Result<Self> {
        let conn = connect(path)?;
        validate(&conn)?;
        let baseline_generation = generation(&conn)?;
        Ok(Self {
            conn,
            baseline_generation,
        })
    }

    /// Open without write permission or migrations. Normal SQLite WAL locking
    /// remains enabled so later committed generations stay visible.
    pub fn open_read_only(path: &Path) -> Result<Self> {
        Self::open_read_only_with(path, |_| Ok(()))
    }

    /// Configure request limits before any validation or generation reads.
    pub(crate) fn open_read_only_with(
        path: &Path,
        configure: impl FnOnce(&Connection) -> Result<()>,
    ) -> Result<Self> {
        let conn = connect_with_setup(path, OpenFlags::SQLITE_OPEN_READ_ONLY, configure)?;
        validate(&conn)?;
        let baseline_generation = generation(&conn)?;
        Ok(Self {
            conn,
            baseline_generation,
        })
    }

    pub fn file_stamps(&self) -> Result<Vec<FileStamp>> {
        let mut stmt = self
            .conn
            .prepare("SELECT path, hash FROM files ORDER BY path")?;
        Ok(stmt
            .query_map([], |r| {
                Ok(FileStamp {
                    path: r.get(0)?,
                    hash: r.get(1)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?)
    }

    pub fn stats(&self) -> Result<Stats> {
        let tx = self.conn.unchecked_transaction()?;
        let stats = read_stats(&tx)?;
        tx.commit()?;
        Ok(stats)
    }

    /// Read graph-level metadata without loading any nodes or edges.
    pub fn graph_metadata(&self) -> Result<serde_json::Value> {
        let json: String = self.conn.query_row(
            "SELECT graph_metadata FROM metadata WHERE singleton=1",
            [],
            |r| r.get(0),
        )?;
        Ok(serde_json::from_str(&json)?)
    }

    /// Short saved-graph topic labels for an explicitly configured transcription adapter.
    pub fn transcription_topics(&self) -> Result<Vec<String>> {
        let mut statement = self.conn.prepare(
            "WITH incidents AS (SELECT source AS id FROM edges UNION ALL SELECT target FROM edges),
             degrees AS (SELECT id,count(*) AS degree FROM incidents GROUP BY id)
             SELECT n.label FROM degrees d JOIN nodes n ON n.id=d.id
             WHERE json_extract(n.payload,'$.kind') NOT IN ('file','module','document','group','rationale')
             ORDER BY d.degree DESC,n.id LIMIT 64",
        )?;
        let mut topics = Vec::new();
        let mut seen = BTreeSet::new();
        for label in statement.query_map([], |row| row.get::<_, String>(0))? {
            let label: String = label?
                .chars()
                .filter(|c| c.is_alphanumeric() || matches!(c, ' ' | '-' | '_' | '+' | '.' | '#'))
                .take(64)
                .collect();
            let label = label.split_whitespace().collect::<Vec<_>>().join(" ");
            if !label.is_empty() && seen.insert(label.to_lowercase()) {
                topics.push(label);
                if topics.len() == 8 {
                    break;
                }
            }
        }
        Ok(topics)
    }

    /// Read every node and edge in one SQLite read transaction. This explicit
    /// full read has no query limit and never returns a truncated graph.
    pub fn snapshot(&self) -> Result<GraphSnapshot> {
        self.snapshot_inner(None)
    }

    /// Check counts and stored payload bytes inside the same read transaction
    /// before allocating records. Useful for bounded server consumers.
    pub fn snapshot_bounded(
        &self,
        nodes: usize,
        edges: usize,
        references: usize,
        payload_bytes: usize,
    ) -> Result<GraphSnapshot> {
        self.snapshot_inner(Some((nodes, edges, references, payload_bytes)))
    }

    fn snapshot_inner(
        &self,
        limits: Option<(usize, usize, usize, usize)>,
    ) -> Result<GraphSnapshot> {
        let tx = self.conn.unchecked_transaction()?;
        let (generation, kind, root, metadata): (i64, String, Option<String>, String) = tx
            .query_row(
                "SELECT generation,kind,root,graph_metadata FROM metadata WHERE singleton=1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )?;
        if let Some((max_nodes, max_edges, max_refs, max_bytes)) = limits {
            let mut total_bytes = metadata.len() as u64;
            for (sql, limit) in [
                (
                    "SELECT COUNT(*),COALESCE(SUM(length(CAST(payload AS BLOB))),0) FROM nodes",
                    max_nodes,
                ),
                (
                    "SELECT COUNT(*),COALESCE(SUM(length(CAST(payload AS BLOB))),0) FROM edges",
                    max_edges,
                ),
                (
                    "SELECT COUNT(*),COALESCE(SUM(length(CAST(payload AS BLOB))),0) FROM refs WHERE resolved_target IS NULL",
                    max_refs,
                ),
            ] {
                let (count, bytes): (i64, i64) =
                    tx.query_row(sql, [], |row| Ok((row.get(0)?, row.get(1)?)))?;
                let (count, bytes) = (u64::try_from(count)?, u64::try_from(bytes)?);
                ensure!(count <= limit as u64, "snapshot record count exceeds limit");
                total_bytes = total_bytes
                    .checked_add(bytes)
                    .context("snapshot byte count overflow")?;
                ensure!(
                    total_bytes <= max_bytes as u64,
                    "snapshot payload exceeds byte limit"
                );
            }
            if kind == "native" {
                // Charge a conservative JSON-escaped representation before
                // loading source proof. Empty files without nodes add nothing.
                let proof_bytes: i64 = tx.query_row(
                    "SELECT COALESCE(SUM(length(CAST(path AS BLOB))*6+length(CAST(hash AS BLOB))+100),0) FROM files WHERE EXISTS(SELECT 1 FROM nodes WHERE owner_file=files.path)",
                    [], |row| row.get(0),
                )?;
                total_bytes = total_bytes
                    .checked_add(u64::try_from(proof_bytes)?)
                    .and_then(|v| v.checked_add(64))
                    .context("snapshot byte count overflow")?;
                ensure!(
                    total_bytes <= max_bytes as u64,
                    "snapshot source proof exceeds byte limit"
                );
            }
        }
        let nodes = read_payloads(&tx, "SELECT payload FROM nodes ORDER BY id")?;
        let edges = read_payloads(&tx, "SELECT payload FROM edges ORDER BY id")?;
        let mut metadata: serde_json::Value = serde_json::from_str(&metadata)?;
        if kind == "native" {
            let references: Vec<Reference> = read_payloads(
                &tx,
                "SELECT payload FROM refs WHERE resolved_target IS NULL ORDER BY id",
            )?;
            if metadata.is_null() {
                metadata = serde_json::json!({});
            }
            metadata
                .as_object_mut()
                .context("native graph metadata must be an object")?
                .insert(
                    "graf_unresolved_references".into(),
                    serde_json::to_value(references)?,
                );
            let mut files = serde_json::Map::new();
            let mut statement = tx.prepare(
                "SELECT path,hash FROM files WHERE EXISTS(SELECT 1 FROM nodes WHERE owner_file=files.path) ORDER BY path",
            )?;
            let mut rows = statement.query([])?;
            while let Some(row) = rows.next()? {
                let path: String = row.get(0)?;
                let stamp: String = row.get(1)?;
                if let Some(digest) = crate::index::indexed_source_digest(&stamp) {
                    files.insert(path, serde_json::json!(digest));
                }
            }
            // Replace any persisted claim: these rows share this snapshot's
            // generation and node ownership. No source files are read here.
            metadata.as_object_mut().unwrap().insert(
                "graf_source_digests".into(),
                serde_json::json!({"algorithm":"blake3","files":files}),
            );
        }
        let snapshot = GraphSnapshot {
            schema_version: SCHEMA_VERSION,
            generation: u64::try_from(generation)?,
            kind,
            root,
            nodes,
            edges,
            metadata,
        };
        tx.commit()?;
        Ok(snapshot)
    }

    pub(crate) fn semantic_losses(&self, changed: &[FileFacts]) -> Result<Vec<String>> {
        let tx = self.conn.unchecked_transaction()?;
        let mut losses = Vec::new();
        for facts in changed {
            // A managed source's capture key stays stable when its display name changes.
            let source_glob = facts.path.strip_prefix(".graf/sources/").and_then(|path| {
                let (key, _) = path.split_once('/')?;
                (key.len() == 64 && key.bytes().all(|b| b.is_ascii_hexdigit()))
                    .then(|| format!(".graf/sources/{key}/*"))
            });
            let mut old_nodes = 0;
            let mut old_edges = 0;
            for (query, count) in [
                (
                    "SELECT payload FROM nodes WHERE owner_file=?1 OR owner_file IN (SELECT path FROM files WHERE path GLOB ?2)",
                    &mut old_nodes,
                ),
                (
                    "SELECT payload FROM edges WHERE owner_file=?1 OR owner_file IN (SELECT path FROM files WHERE path GLOB ?2)",
                    &mut old_edges,
                ),
            ] {
                let mut stmt = tx.prepare(query)?;
                for payload in
                    stmt.query_map(params![facts.path, source_glob], |r| r.get::<_, String>(0))?
                {
                    let value: serde_json::Value = serde_json::from_str(&payload?)?;
                    if crate::index::semantic_provenance(&value["metadata"]) {
                        *count += 1;
                    }
                }
            }
            let (nodes, edges) = crate::index::semantic_counts(facts);
            if nodes < old_nodes || edges < old_edges {
                losses.push(format!(
                    "{} (nodes {old_nodes}->{nodes}, edges {old_edges}->{edges})",
                    facts.path
                ));
            }
        }
        tx.commit()?;
        Ok(losses)
    }

    pub fn apply_native(
        &mut self,
        root: &str,
        changed: Vec<FileFacts>,
        deleted: Vec<String>,
        coverage: Coverage,
    ) -> Result<IndexReport> {
        self.apply_native_inner(root, changed, deleted, coverage, None)
    }

    pub fn apply_native_with_options(
        &mut self,
        root: &str,
        changed: Vec<FileFacts>,
        deleted: Vec<String>,
        coverage: Coverage,
        options: serde_json::Value,
    ) -> Result<IndexReport> {
        self.apply_native_inner(root, changed, deleted, coverage, Some(options))
    }

    fn apply_native_inner(
        &mut self,
        root: &str,
        changed: Vec<FileFacts>,
        deleted: Vec<String>,
        coverage: Coverage,
        options: Option<serde_json::Value>,
    ) -> Result<IndexReport> {
        ensure!(!root.is_empty(), "native root cannot be empty");
        validate_facts(&changed, &deleted)?;
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        if generation(&tx)? != self.baseline_generation {
            return Err(StaleStore.into());
        }
        let (kind, previous_root): (String, Option<String>) = tx.query_row(
            "SELECT kind, root FROM metadata WHERE singleton=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        ensure!(kind != "imported", "cannot update an imported snapshot");
        if let Some(previous_root) = previous_root {
            ensure!(
                previous_root == root,
                "database root mismatch: indexed {previous_root}, requested {root}"
            );
        }
        let mut keys = BTreeSet::new();
        ensure_aliases(&tx, &mut keys)?;
        let aliases_migrated = !keys.is_empty();
        let search_migrated = ensure_search(&tx)?;
        let mut metadata: serde_json::Value = serde_json::from_str(&tx.query_row(
            "SELECT graph_metadata FROM metadata WHERE singleton=1",
            [],
            |r| r.get::<_, String>(0),
        )?)?;
        let mut options_changed = false;
        if let Some(options) = options {
            if metadata.is_null() {
                metadata = serde_json::json!({});
            }
            let attrs = metadata
                .as_object_mut()
                .context("native graph metadata must be an object")?;
            options_changed = attrs.get("graf_index_options") != Some(&options);
            attrs.insert("graf_index_options".to_owned(), options);
        }
        // Replacing a target cascades away incoming edges even when their
        // unchanged owner still asserts them. References are rebound below;
        // direct edges have no reference record from which to rebuild them.
        let replaced: BTreeSet<_> = deleted
            .iter()
            .map(String::as_str)
            .chain(changed.iter().map(|facts| facts.path.as_str()))
            .collect();
        let mut ruby_changed = changed.iter().any(|facts| {
            facts
                .nodes
                .iter()
                .any(|node| node.metadata["language"] == "ruby")
        });
        if !ruby_changed {
            for path in &replaced {
                ruby_changed = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM nodes WHERE owner_file=?1 AND json_extract(payload,'$.metadata.language')='ruby')",
                    [path], |row| row.get(0),
                )?;
                if ruby_changed {
                    break;
                }
            }
        }
        let mut incoming = Vec::new();
        for facts in &changed {
            let mut stmt = tx.prepare(
                "SELECT e.payload,e.owner_file FROM nodes n JOIN edges e ON e.target=n.id
                 WHERE n.owner_file=?1 AND e.ref_id IS NULL AND e.owner_file IS NOT NULL",
            )?;
            for row in stmt.query_map([&facts.path], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })? {
                let (payload, owner) = row?;
                if !replaced.contains(owner.as_str()) {
                    incoming.push((serde_json::from_str::<Edge>(&payload)?, owner));
                }
            }
        }
        let mut removed = 0;
        for path in deleted.iter().chain(changed.iter().map(|f| &f.path)) {
            let mut stmt = tx.prepare(
                "SELECT binding_key FROM nodes WHERE owner_file=?1 AND binding_key IS NOT NULL
                 UNION SELECT a.binding_key FROM node_aliases a JOIN nodes n ON n.id=a.node_id WHERE n.owner_file=?1",
            )?;
            for key in stmt.query_map([path], |r| r.get::<_, String>(0))? {
                keys.insert(key?);
            }
        }
        for path in &deleted {
            removed += tx.execute("DELETE FROM files WHERE path=?1", [path])?;
        }
        for facts in &changed {
            tx.execute("DELETE FROM files WHERE path=?1", [&facts.path])?;
        }
        for facts in &changed {
            tx.execute(
                "INSERT INTO files(path,hash,module,diagnostics) VALUES(?1,?2,?3,?4)",
                params![
                    facts.path,
                    facts.hash,
                    facts.module,
                    serde_json::to_string(&facts.diagnostics)?
                ],
            )?;
            for node in &facts.nodes {
                if let Some(key) = &node.binding_key {
                    keys.insert(key.clone());
                }
                insert_node(&tx, node, Some(&facts.path))?;
                for alias in binding_aliases(node)? {
                    keys.insert(alias.to_owned());
                    tx.execute(
                        "INSERT OR IGNORE INTO node_aliases(node_id,binding_key) VALUES(?1,?2)",
                        params![node.id, alias],
                    )?;
                }
            }
        }
        for (edge, owner) in incoming {
            let endpoints_survive: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM nodes WHERE id=?1) AND EXISTS(SELECT 1 FROM nodes WHERE id=?2)",
                params![edge.source, edge.target],
                |r| r.get(0),
            )?;
            if endpoints_survive {
                insert_edge(&tx, &edge, Some(&owner), None)?;
            }
        }
        let mut affected = BTreeSet::new();
        for facts in &changed {
            for edge in &facts.edges {
                insert_edge(&tx, edge, Some(&facts.path), None)?;
            }
            for reference in &facts.references {
                tx.execute("INSERT INTO refs(id,source,owner_file,relation,payload,resolved_target,resolution_reason) VALUES(?1,?2,?3,?4,?5,NULL,?6)",
                    params![reference.id, reference.source, facts.path, reference.relation, serde_json::to_string(reference)?, reference.reason])?;
                for (priority, key) in reference.candidate_keys.iter().enumerate() {
                    tx.execute(
                        "INSERT INTO ref_keys(ref_id,priority,binding_key) VALUES(?1,?2,?3)",
                        params![reference.id, priority as i64, key],
                    )?;
                }
                affected.insert(reference.id.clone());
            }
        }
        for key in keys {
            let mut stmt = tx.prepare("SELECT ref_id FROM ref_keys WHERE binding_key=?1")?;
            for id in stmt.query_map([key], |r| r.get::<_, String>(0))? {
                affected.insert(id?);
            }
        }
        if ruby_changed {
            let nodes: Vec<Node> = read_payloads(
                &tx,
                "SELECT payload FROM nodes WHERE json_extract(payload,'$.metadata.language')='ruby' ORDER BY id",
            )?;
            let context = crate::languages::scripted::RubyContext::from_nodes(&nodes);
            let references: Vec<Reference> = read_payloads(
                &tx,
                "SELECT r.payload FROM refs r JOIN nodes n ON n.id=r.source WHERE json_extract(n.payload,'$.metadata.language')='ruby' ORDER BY r.id",
            )?;
            for reference in references {
                let keys = context
                    .inherited_keys(&reference)
                    .unwrap_or_else(|| reference.candidate_keys.clone());
                tx.execute("DELETE FROM ref_keys WHERE ref_id=?1", [&reference.id])?;
                for (priority, key) in keys.iter().enumerate() {
                    tx.execute(
                        "INSERT INTO ref_keys(ref_id,priority,binding_key) VALUES(?1,?2,?3)",
                        params![reference.id, priority as i64, key],
                    )?;
                }
                affected.insert(reference.id);
            }
        }
        for id in affected {
            resolve_reference(&tx, &id)?;
        }
        let previous_coverage: Coverage = serde_json::from_str(&tx.query_row(
            "SELECT coverage FROM metadata WHERE singleton=1",
            [],
            |r| r.get::<_, String>(0),
        )?)?;
        // The unchanged count describes this scan, not a change in stored facts.
        // Keep no-op scans read-only; coverage changes still publish a generation.
        let coverage_changed = previous_coverage.supported_files != coverage.supported_files
            || previous_coverage.unsupported_files != coverage.unsupported_files;
        let changed_generation = kind == "empty"
            || !changed.is_empty()
            || removed > 0
            || options_changed
            || aliases_migrated
            || search_migrated
            || coverage_changed;
        if changed_generation {
            tx.execute("UPDATE metadata SET kind='native', root=?1, coverage=?2, generation=generation+1, graph_metadata=?3 WHERE singleton=1",
                params![root, serde_json::to_string(&coverage)?, serde_json::to_string(&metadata)?])?;
        }
        let stats = read_stats(&tx)?;
        let report = IndexReport {
            schema_version: SCHEMA_VERSION,
            generation: stats.generation,
            parsed_files: changed.len(),
            unchanged_files: coverage.unchanged_files,
            deleted_files: removed,
            nodes: stats.nodes,
            edges: stats.edges,
            diagnostics: stats.diagnostics,
            semantic_usage: None,
            provider_usage: None,
            timings: None,
        };
        tx.commit()?;
        self.baseline_generation = report.generation;
        Ok(report)
    }

    pub fn import_graph(&mut self, graph: ImportedGraph) -> Result<Stats> {
        self.write_import(graph, false)
    }

    /// Atomically replace an imported graph. Native indexes (even empty ones)
    /// and handles opened before another writer committed cannot be replaced.
    pub fn refresh_import(&mut self, graph: ImportedGraph) -> Result<Stats> {
        self.write_import(graph, true)
    }

    fn write_import(&mut self, graph: ImportedGraph, refresh: bool) -> Result<Stats> {
        crate::snapshot::validate_graph(&graph.nodes, &graph.edges)?;
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        if generation(&tx)? != self.baseline_generation {
            return Err(StaleStore.into());
        }
        let (kind, root): (String, Option<String>) = tx.query_row(
            "SELECT kind,root FROM metadata WHERE singleton=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if refresh {
            ensure!(
                kind == "imported" && root.is_none(),
                "refresh requires an imported snapshot without an index root"
            );
            tx.execute("DELETE FROM edges", [])?;
            tx.execute("DELETE FROM nodes", [])?;
        } else {
            ensure!(
                kind == "empty" && root.is_none(),
                "import requires an empty Graf database"
            );
        }
        ensure_search(&tx)?;
        // Deletes, inserts, search triggers and generation advance commit as
        // one transaction. Any validation or SQL failure retains the old graph.
        for node in &graph.nodes {
            insert_node(&tx, node, None)?;
        }
        for edge in &graph.edges {
            insert_edge(&tx, edge, None, None)?;
        }
        tx.execute("UPDATE metadata SET kind='imported', generation=generation+1, graph_metadata=?1 WHERE singleton=1",
            [serde_json::to_string(&graph.metadata)?])?;
        let stats = read_stats(&tx)?;
        tx.commit()?;
        self.baseline_generation = stats.generation;
        Ok(stats)
    }

    pub fn query_extended(
        &self,
        text: &str,
        options: &crate::query::SearchOptions,
    ) -> Result<crate::query::SearchResult> {
        crate::query::query_extended(&self.conn, text, options, false)
    }
    pub fn neighbors_extended(
        &self,
        symbol: &str,
        options: &crate::query::SearchOptions,
    ) -> Result<crate::query::SearchResult> {
        crate::query::query_extended(&self.conn, symbol, options, true)
    }
    pub fn path_extended(
        &self,
        source: &str,
        target: &str,
        options: &crate::query::SearchOptions,
    ) -> Result<crate::query::PathSearchResult> {
        crate::query::path_extended(&self.conn, source, target, options)
    }

    pub fn query(&self, text: &str, options: &QueryOptions) -> Result<GraphResult> {
        crate::query::query(&self.conn, text, options)
    }
    pub fn neighbors(&self, symbol: &str, options: &QueryOptions) -> Result<GraphResult> {
        crate::query::neighbors(&self.conn, symbol, options)
    }
    pub fn path(&self, source: &str, target: &str, options: &QueryOptions) -> Result<PathResult> {
        crate::query::path(&self.conn, source, target, options)
    }
}

fn read_payloads<T: serde::de::DeserializeOwned>(conn: &Connection, sql: &str) -> Result<Vec<T>> {
    let mut stmt = conn.prepare(sql)?;
    stmt.query_map([], |r| r.get::<_, String>(0))?
        .map(|row| Ok(serde_json::from_str(&row?)?))
        .collect()
}

fn connect(path: &Path) -> Result<Connection> {
    connect_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE)
}

fn connect_with_flags(path: &Path, flags: OpenFlags) -> Result<Connection> {
    connect_with_setup(path, flags, |_| Ok(()))
}

fn connect_with_setup(
    path: &Path,
    flags: OpenFlags,
    configure: impl FnOnce(&Connection) -> Result<()>,
) -> Result<Connection> {
    let conn = Connection::open_with_flags(path, flags | OpenFlags::SQLITE_OPEN_NO_MUTEX)
        .with_context(|| format!("cannot open Graf database {}", path.display()))?;
    conn.busy_timeout(Duration::from_secs(5))?;
    configure(&conn)?;
    conn.pragma_update(None, "foreign_keys", true)?;
    Ok(conn)
}

fn validate(conn: &Connection) -> Result<()> {
    let app: i64 = conn.pragma_query_value(None, "application_id", |r| r.get(0))?;
    ensure!(
        app == APPLICATION_ID,
        "not a Graf database; refusing unrelated database"
    );
    let version: u32 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    ensure!(
        version == SCHEMA_VERSION,
        "unsupported Graf schema version {version}; expected {SCHEMA_VERSION}"
    );
    let kind: String = conn.query_row("SELECT kind FROM metadata WHERE singleton=1", [], |r| {
        r.get(0)
    })?;
    ensure!(
        matches!(kind.as_str(), "empty" | "native" | "imported"),
        "invalid Graf database kind"
    );
    // Prepare without scanning or writing. An incomplete schema is not usable.
    conn.prepare("SELECT n.payload,e.payload,r.payload,k.priority,f.hash FROM nodes n,edges e,refs r,ref_keys k,files f LIMIT 0")?;
    conn.prepare("SELECT rowid FROM node_search LIMIT 0")?;
    Ok(())
}

fn validate_facts(changed: &[FileFacts], deleted: &[String]) -> Result<()> {
    let mut paths = BTreeSet::new();
    for facts in changed {
        ensure!(
            !facts.path.is_empty() && paths.insert(&facts.path),
            "duplicate or empty changed file path"
        );
        let ids: BTreeSet<_> = facts.nodes.iter().map(|n| n.id.as_str()).collect();
        for node in &facts.nodes {
            binding_aliases(node)?;
            ensure!(
                node.file == facts.path,
                "node file does not match its owning file"
            );
        }
        for edge in &facts.edges {
            ensure!(
                ids.contains(edge.source.as_str()),
                "native edge source must belong to its file"
            );
            ensure!(
                edge.file.as_ref().is_none_or(|f| f == &facts.path),
                "edge file does not match its owner"
            );
        }
        for reference in &facts.references {
            ensure!(
                reference.file == facts.path && ids.contains(reference.source.as_str()),
                "reference source/file does not match its owner"
            );
        }
    }
    for path in deleted {
        ensure!(paths.insert(path), "duplicate file path in delta: {path}");
    }
    Ok(())
}

fn binding_aliases(node: &Node) -> Result<Vec<&str>> {
    let Some(value) = node.metadata.get("binding_aliases") else {
        return Ok(Vec::new());
    };
    value
        .as_array()
        .context("binding_aliases must be an array")?
        .iter()
        .map(|alias| {
            let alias = alias.as_str().context("binding aliases must be strings")?;
            ensure!(!alias.is_empty(), "binding alias cannot be empty");
            Ok(alias)
        })
        .collect()
}

// Additive schema-1 extension: opening/querying old stores never migrates them.
// Explicit indexing installs and backfills this derived index transactionally.
fn ensure_aliases(tx: &Transaction<'_>, keys: &mut BTreeSet<String>) -> Result<()> {
    let exists: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='node_aliases')",
        [],
        |r| r.get(0),
    )?;
    if exists {
        return Ok(());
    }
    tx.execute_batch(
        "CREATE TABLE node_aliases (
        node_id TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
        binding_key TEXT NOT NULL, PRIMARY KEY(node_id,binding_key)
    ); CREATE INDEX node_aliases_binding ON node_aliases(binding_key,node_id);",
    )?;
    let mut stmt = tx.prepare("SELECT payload FROM nodes WHERE owner_file IS NOT NULL")?;
    for payload in stmt.query_map([], |r| r.get::<_, String>(0))? {
        let node: Node = serde_json::from_str(&payload?)?;
        for alias in binding_aliases(&node)? {
            keys.insert(alias.to_owned());
            tx.execute(
                "INSERT OR IGNORE INTO node_aliases(node_id,binding_key) VALUES(?1,?2)",
                params![node.id, alias],
            )?;
        }
    }
    Ok(())
}

// Search enrichment is another additive schema-1 extension. Migration runs
// only during an explicit write, under the same transaction/generation check.
fn ensure_search(tx: &Transaction<'_>) -> Result<bool> {
    let has_version: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_info('metadata') WHERE name='search_version')",
        [],
        |r| r.get(0),
    )?;
    if !has_version {
        tx.execute_batch(
            "ALTER TABLE metadata ADD COLUMN search_version INTEGER NOT NULL DEFAULT 0",
        )?;
    }
    let version: i64 = tx.query_row(
        "SELECT search_version FROM metadata WHERE singleton=1",
        [],
        |r| r.get(0),
    )?;
    ensure!(
        version <= 3,
        "unsupported Graf search index version {version}"
    );
    if version == 3 {
        return Ok(false);
    }
    let mut changed = false;
    let mut stmt = tx.prepare("SELECT id,payload,search FROM nodes ORDER BY id")?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let node: Node = serde_json::from_str(&row.get::<_, String>(1)?)?;
        let search = search_text(&node);
        if search != row.get::<_, String>(2)? {
            tx.execute(
                "UPDATE nodes SET search=?1 WHERE id=?2",
                params![search, node.id],
            )?;
            changed = true;
        }
    }
    drop(rows);
    drop(stmt);
    if changed {
        tx.execute("DELETE FROM node_search", [])?;
        tx.execute(
            "INSERT INTO node_search(rowid,text) SELECT rowid,search FROM nodes",
            [],
        )?;
    }
    tx.execute("UPDATE metadata SET search_version=3 WHERE singleton=1", [])?;
    Ok(changed)
}

pub(crate) fn cjk(c: char) -> bool {
    matches!(c, '\u{3400}'..='\u{9fff}' | '\u{f900}'..='\u{faff}' | '\u{20000}'..='\u{2fa1f}')
}

fn search_text(node: &Node) -> String {
    let mut text = format!(
        "{} {} {} {}",
        node.id,
        node.label,
        node.qualified_name.as_deref().unwrap_or(""),
        node.file,
    );
    let mut attrs = &node.metadata;
    while let Some(original) = attrs.get("original_metadata") {
        attrs = original;
    }
    // Deliberately index named prose fields, never arbitrary metadata values.
    for field in [
        "rationale",
        "description",
        "summary",
        "text",
        "excerpt",
        "evidence",
    ] {
        if let Some(value) = attrs.get(field).and_then(serde_json::Value::as_str) {
            text.push(' ');
            text.push_str(value);
        }
    }
    // Extractors retain safe written attribute values here after redaction.
    // Index the preserved JSON (keys as well as literal leaves), never source
    // text or arbitrary metadata. Imported attributes use the same contract.
    if let Some(attributes) = attrs.get("attributes").filter(|v| v.is_object()) {
        let mut pending = vec![attributes];
        while let Some(value) = pending.pop() {
            match value {
                serde_json::Value::Object(fields) => {
                    for (key, value) in fields {
                        text.push(' ');
                        text.push_str(key);
                        pending.push(value);
                    }
                }
                serde_json::Value::Array(values) => pending.extend(values),
                serde_json::Value::String(value) => {
                    text.push(' ');
                    text.push_str(value);
                }
                value => {
                    text.push(' ');
                    text.push_str(&value.to_string());
                }
            }
        }
    }
    // Compatibility forms (ligatures, full-width letters) share searchable
    // tokens. NFKC retains composed Hangul and Greek spellings for unicode61;
    // the original text below still serves literal callers of the older API.
    let spelling: String = text.nfkc().collect();
    let mut search = String::with_capacity(spelling.len() * 2);
    let mut previous_lower = false;
    for c in spelling.chars() {
        if c.is_uppercase() && previous_lower {
            search.push(' ');
        }
        previous_lower = c.is_lowercase() || c.is_numeric();
        search.push(if c == '_' { ' ' } else { c });
    }
    // Useful CJK recall without a dictionary or a query-side scan: preserve
    // full strings and index individual ideographs plus adjacent bigrams.
    let mut previous = None;
    for c in spelling.chars() {
        if cjk(c) {
            search.push(' ');
            search.push(c);
            if let Some(before) = previous {
                search.push(' ');
                search.push(before);
                search.push(c);
            }
            previous = Some(c);
        } else {
            previous = None;
        }
    }
    // Retain the original spelling as well as identifier components.
    format!("{text} {spelling} {search}")
}

fn insert_node(conn: &Connection, node: &Node, owner: Option<&str>) -> Result<()> {
    ensure!(!node.id.is_empty(), "node ID cannot be empty");
    conn.execute("INSERT INTO nodes(id,label,qualified_name,binding_key,file,owner_file,payload,search) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
        params![node.id,node.label,node.qualified_name,node.binding_key,node.file,owner,serde_json::to_string(node)?,search_text(node)])?;
    Ok(())
}

fn insert_edge(
    conn: &Connection,
    edge: &Edge,
    owner: Option<&str>,
    reference: Option<&str>,
) -> Result<()> {
    ensure!(!edge.id.is_empty(), "edge ID cannot be empty");
    conn.execute("INSERT INTO edges(id,source,target,relation,directed,owner_file,ref_id,payload) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
        params![edge.id,edge.source,edge.target,edge.relation,edge.directed,owner,reference,serde_json::to_string(edge)?])?;
    Ok(())
}

fn resolve_reference(tx: &Transaction<'_>, id: &str) -> Result<()> {
    let payload: String =
        tx.query_row("SELECT payload FROM refs WHERE id=?1", [id], |r| r.get(0))?;
    let reference: Reference = serde_json::from_str(&payload)?;
    tx.execute("DELETE FROM edges WHERE ref_id=?1", [id])?;
    let mut target = None;
    let mut reason = if reference.reason.is_empty() {
        "no matching binding".to_owned()
    } else {
        reference.reason.clone()
    };
    let keys = tx
        .prepare("SELECT binding_key FROM ref_keys WHERE ref_id=?1 ORDER BY priority")?
        .query_map([id], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for key in &keys {
        let mut stmt = tx.prepare(
            "SELECT id FROM nodes WHERE binding_key=?1
                        UNION SELECT node_id FROM node_aliases WHERE binding_key=?1
                        ORDER BY 1 LIMIT 2",
        )?;
        let ids = stmt
            .query_map([key], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        match ids.as_slice() {
            [] => continue,
            [id] => {
                target = Some(id.clone());
                reason.clear();
                break;
            }
            _ => {
                reason = format!("ambiguous binding: {key}");
                break;
            }
        }
    }
    tx.execute(
        "UPDATE refs SET resolved_target=?1,resolution_reason=?2 WHERE id=?3",
        params![target, reason, id],
    )?;
    if let Some(target) = target {
        let mut metadata = serde_json::json!({"reference_id": reference.id});
        let source_payload: String = tx.query_row(
            "SELECT payload FROM nodes WHERE id=?1",
            [&reference.source],
            |row| row.get(0),
        )?;
        let source: serde_json::Value = serde_json::from_str(&source_payload)?;
        if let Some(context) = source["metadata"]["python_references"]
            .as_array()
            .and_then(|items| {
                items
                    .iter()
                    .find(|item| item["reference_id"] == reference.id)
            })
            .and_then(|item| item["context"].as_str())
        {
            metadata["context"] = serde_json::Value::String(context.to_owned());
        }
        let edge = Edge {
            id: format!("reference:{}", reference.id),
            source: reference.source,
            target,
            relation: reference.relation,
            directed: true,
            file: Some(reference.file.clone()),
            line: Some(reference.line),
            confidence: "statically_resolved".to_owned(),
            metadata,
        };
        insert_edge(tx, &edge, Some(&reference.file), Some(id))?;
    }
    Ok(())
}

pub(crate) fn generation(conn: &Connection) -> Result<u64> {
    let value: i64 = conn.query_row(
        "SELECT generation FROM metadata WHERE singleton=1",
        [],
        |r| r.get(0),
    )?;
    u64::try_from(value).context("invalid negative generation")
}

fn read_stats(conn: &Connection) -> Result<Stats> {
    let (generation, kind, root, coverage): (i64, String, Option<String>, String) = conn
        .query_row(
            "SELECT generation,kind,root,coverage FROM metadata WHERE singleton=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )?;
    let mut diagnostics = Vec::new();
    let mut stmt = conn.prepare("SELECT diagnostics FROM files ORDER BY path")?;
    for json in stmt.query_map([], |r| r.get::<_, String>(0))? {
        diagnostics.extend(serde_json::from_str::<Vec<Diagnostic>>(&json?)?);
    }
    let count = |sql| -> Result<usize> {
        let value: i64 = conn.query_row(sql, [], |r| r.get(0))?;
        Ok(usize::try_from(value)?)
    };
    Ok(Stats {
        schema_version: SCHEMA_VERSION,
        generation: u64::try_from(generation)?,
        root,
        nodes: count("SELECT count(*) FROM nodes")?,
        edges: count("SELECT count(*) FROM edges")?,
        files: if kind == "imported" {
            count("SELECT count(DISTINCT file) FROM nodes WHERE file<>''")?
        } else {
            count("SELECT count(*) FROM files")?
        },
        unresolved_references: count("SELECT count(*) FROM refs WHERE resolved_target IS NULL")?,
        kind,
        coverage: serde_json::from_str(&coverage)?,
        diagnostics,
    })
}
