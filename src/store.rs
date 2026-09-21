use crate::model::*;
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OpenFlags, OptionalExtension, Statement, Transaction, params};
use std::{collections::BTreeSet, fs, path::Path, time::Duration};
use unicode_normalization::UnicodeNormalization;

const APPLICATION_ID: i64 = 0x47524146;
const SEARCH_VERSION: i64 = 5;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StorageLayout {
    Legacy,
    Compact,
}

// Physical layout is independent of the public graph/snapshot schema. Call
// inside the operation's transaction so an existing reader keeps its layout.
pub(crate) fn storage_layout(conn: &Connection) -> Result<StorageLayout> {
    let version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    match version {
        1 => Ok(StorageLayout::Legacy),
        // Format 3 projects refs.id from payload; its read columns are identical.
        2 | 3 => Ok(StorageLayout::Compact),
        _ => anyhow::bail!("unsupported Graf storage version {version}; expected 1, 2 or 3"),
    }
}

pub struct Store {
    pub(crate) conn: Connection,
    baseline_generation: u64,
}

/// Logical SQLite page counts, not filesystem sizes. A busy checkpoint can
/// leave the old main-file length and WAL allocated after VACUUM commits.
#[derive(Debug, serde::Serialize)]
pub struct CompactionReport {
    pub schema_version: u32,
    pub page_size: u64,
    pub pages_before: u64,
    pub pages_after: u64,
    pub free_pages_before: u64,
    pub free_pages_after: u64,
    pub checkpoint_busy: bool,
}

const SCHEMA: &str = r#"
CREATE TABLE metadata (
    singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
    generation INTEGER NOT NULL, kind TEXT NOT NULL,
    root TEXT, coverage TEXT NOT NULL, graph_metadata TEXT NOT NULL
);
INSERT INTO metadata VALUES (1, 0, 'empty', NULL,
 '{"supported_files":0,"unsupported_files":0,"unchanged_files":0}', 'null');
"#;

// The same tables serve fresh stores and transactional upgrades. Temporary
// names let the old parents and their children coexist while foreign keys stay on.
const COMPACT_TABLES: &str = r#"
CREATE TABLE compact_files (
    fkey INTEGER PRIMARY KEY, path TEXT NOT NULL UNIQUE,
    hash TEXT NOT NULL, module TEXT NOT NULL, diagnostics TEXT NOT NULL
);
CREATE TABLE compact_nodes (
    nkey INTEGER PRIMARY KEY, id TEXT NOT NULL UNIQUE,
    label TEXT NOT NULL, qualified_name TEXT, binding_key TEXT,
    file TEXT NOT NULL, owner_key INTEGER REFERENCES compact_files(fkey) ON DELETE CASCADE,
    payload TEXT NOT NULL, search TEXT NOT NULL
);
CREATE TABLE compact_node_aliases (
    node_key INTEGER NOT NULL REFERENCES compact_nodes(nkey) ON DELETE CASCADE,
    binding_key TEXT NOT NULL, PRIMARY KEY(node_key, binding_key)
) WITHOUT ROWID;
"#;

const COMPACT_REFERENCE_TABLES: &str = r#"
CREATE TABLE compact_refs (
    rkey INTEGER PRIMARY KEY,
    id TEXT GENERATED ALWAYS AS (json_extract(payload,'$.id')) VIRTUAL NOT NULL UNIQUE,
    source_key INTEGER NOT NULL REFERENCES compact_nodes(nkey) ON DELETE CASCADE,
    owner_key INTEGER NOT NULL REFERENCES compact_files(fkey) ON DELETE CASCADE,
    relation TEXT NOT NULL, payload TEXT NOT NULL,
    resolved_target_key INTEGER, resolution_reason TEXT NOT NULL
);
CREATE TABLE compact_ref_keys (
    ref_key INTEGER NOT NULL REFERENCES compact_refs(rkey) ON DELETE CASCADE,
    priority INTEGER NOT NULL, binding_key TEXT NOT NULL,
    PRIMARY KEY(ref_key, priority)
) WITHOUT ROWID;
CREATE TABLE compact_edges (
    id TEXT PRIMARY KEY,
    source_key INTEGER NOT NULL REFERENCES compact_nodes(nkey) ON DELETE CASCADE,
    target_key INTEGER NOT NULL REFERENCES compact_nodes(nkey) ON DELETE CASCADE,
    relation TEXT NOT NULL, directed INTEGER NOT NULL CHECK(directed IN (0, 1)),
    owner_key INTEGER REFERENCES compact_files(fkey) ON DELETE CASCADE,
    ref_key INTEGER UNIQUE REFERENCES compact_refs(rkey) ON DELETE CASCADE,
    payload TEXT NOT NULL
);
"#;

const COMPACT_PUBLISH: &str = r#"
ALTER TABLE compact_files RENAME TO files;
ALTER TABLE compact_nodes RENAME TO nodes;
ALTER TABLE compact_node_aliases RENAME TO node_aliases;
CREATE INDEX nodes_label ON nodes(label, id);
CREATE INDEX nodes_file ON nodes(file, id);
CREATE INDEX node_aliases_binding ON node_aliases(binding_key, node_key);
CREATE VIRTUAL TABLE node_search USING fts5(text, content='', contentless_delete=1);
INSERT INTO node_search(rowid,text) SELECT nkey,search FROM nodes;
CREATE TRIGGER nodes_insert AFTER INSERT ON nodes BEGIN
    INSERT INTO node_search(rowid,text) VALUES(new.nkey,new.search);
END;
CREATE TRIGGER nodes_delete AFTER DELETE ON nodes BEGIN
    DELETE FROM node_search WHERE rowid=old.nkey;
END;
"#;

const COMPACT_REFERENCE_PUBLISH: &str = r#"
ALTER TABLE compact_refs RENAME TO refs;
ALTER TABLE compact_ref_keys RENAME TO ref_keys;
ALTER TABLE compact_edges RENAME TO edges;
CREATE INDEX refs_owner ON refs(owner_key);
CREATE INDEX refs_unresolved_source ON refs(source_key, id) WHERE resolved_target_key IS NULL;
CREATE INDEX refs_unresolved_relation ON refs(source_key, relation, id) WHERE resolved_target_key IS NULL;
CREATE INDEX ref_keys_binding ON ref_keys(binding_key, ref_key);
CREATE INDEX edges_source ON edges(source_key, id);
CREATE INDEX edges_target ON edges(target_key, id);
CREATE INDEX edges_source_relation ON edges(source_key, relation, id);
CREATE INDEX edges_target_relation ON edges(target_key, relation, id);
"#;

// Shared by fresh databases and explicit-write migration; these indexes do
// not change graph identity, payloads, or schema-1 snapshot compatibility.
const STORAGE_INDICES: &[(&str, &str)] = &[
    // Unfiltered source lookup serves FK deletion and rebinding. Ordered
    // unresolved streams keep their separate (source_key, ..., id) indexes.
    (
        "refs_source",
        "CREATE INDEX refs_source ON refs(source_key)",
    ),
    (
        "nodes_qualified",
        "CREATE INDEX nodes_qualified ON nodes(qualified_name, id) WHERE qualified_name IS NOT NULL",
    ),
    (
        "nodes_binding",
        "CREATE INDEX nodes_binding ON nodes(binding_key, id) WHERE binding_key IS NOT NULL",
    ),
    (
        "nodes_owner",
        "CREATE INDEX nodes_owner ON nodes(owner_key) WHERE owner_key IS NOT NULL",
    ),
    (
        "edges_source_direction",
        "CREATE INDEX edges_source_direction ON edges(source_key, id) WHERE directed=0",
    ),
    (
        "edges_target_direction",
        "CREATE INDEX edges_target_direction ON edges(target_key, id) WHERE directed=0",
    ),
    (
        "edges_source_direction_relation",
        "CREATE INDEX edges_source_direction_relation ON edges(source_key, relation, id) WHERE directed=0",
    ),
    (
        "edges_target_direction_relation",
        "CREATE INDEX edges_target_direction_relation ON edges(target_key, relation, id) WHERE directed=0",
    ),
    (
        "edges_owner",
        "CREATE INDEX edges_owner ON edges(owner_key) WHERE owner_key IS NOT NULL",
    ),
];

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
            tx.execute_batch(COMPACT_TABLES)?;
            tx.execute_batch(COMPACT_REFERENCE_TABLES)?;
            tx.execute_batch(COMPACT_PUBLISH)?;
            tx.execute_batch(COMPACT_REFERENCE_PUBLISH)?;
            ensure_storage_indices(&tx)?;
            tx.pragma_update(None, "application_id", APPLICATION_ID)?;
            tx.pragma_update(None, "user_version", 3)?;
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

    /// Explicitly repack an existing format-2 or format-3 database without changing facts
    /// or the write baseline. SQLite serializes VACUUM with other writers; a
    /// stale handle compacts current data but remains stale for fact writes.
    pub fn compact(&mut self) -> Result<CompactionReport> {
        ensure!(
            self.conn.is_autocommit(),
            "cannot compact inside an active transaction"
        );
        ensure!(
            !self.conn.is_readonly("main")?,
            "cannot compact a read-only Graf database"
        );
        let (page_size, pages_before, free_pages_before) = {
            let tx = self.conn.transaction()?;
            ensure!(
                storage_layout(&tx)? == StorageLayout::Compact,
                "storage format 1 must be upgraded by update or import refresh before compact"
            );
            let counts = (
                tx.pragma_query_value(None, "page_size", |row| {
                    row.get::<_, u32>(0).map(u64::from)
                })?,
                tx.pragma_query_value(None, "page_count", |row| {
                    row.get::<_, u32>(0).map(u64::from)
                })?,
                tx.pragma_query_value(None, "freelist_count", |row| {
                    row.get::<_, u32>(0).map(u64::from)
                })?,
            );
            tx.commit()?;
            counts
        };
        // VACUUM owns its transaction; never wrap it in a write transaction or
        // replace the database path. Formats 2/3 have explicit parent INTEGER PKs.
        self.conn
            .execute_batch("VACUUM main")
            .context("cannot compact Graf database")?;
        let checkpoint_busy = self
            .conn
            .query_row("PRAGMA main.wal_checkpoint(TRUNCATE)", [], |row| {
                row.get::<_, bool>(0)
            })
            .context("compaction completed but checkpoint failed")?;
        let (pages_after, free_pages_after) = (|| -> Result<(u64, u64)> {
            let tx = self.conn.transaction()?;
            let counts = (
                tx.pragma_query_value(None, "page_count", |row| {
                    row.get::<_, u32>(0).map(u64::from)
                })?,
                tx.pragma_query_value(None, "freelist_count", |row| {
                    row.get::<_, u32>(0).map(u64::from)
                })?,
            );
            tx.commit()?;
            Ok(counts)
        })()
        .context("compaction completed but reading page counts failed")?;
        Ok(CompactionReport {
            schema_version: SCHEMA_VERSION,
            page_size,
            pages_before,
            pages_after,
            free_pages_before,
            free_pages_after,
            checkpoint_busy,
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
        let tx = self.conn.unchecked_transaction()?;
        let sql = match storage_layout(&tx)? {
            StorageLayout::Legacy =>
                "WITH incidents AS (SELECT source AS id FROM edges UNION ALL SELECT target FROM edges),
                 degrees AS (SELECT id,count(*) AS degree FROM incidents GROUP BY id)
                 SELECT n.label FROM degrees d JOIN nodes n ON n.id=d.id
                 WHERE json_extract(n.payload,'$.kind') NOT IN ('file','module','document','group','rationale')
                 ORDER BY d.degree DESC,n.id LIMIT 64",
            StorageLayout::Compact =>
                "WITH incidents AS (SELECT source_key AS nkey FROM edges UNION ALL SELECT target_key FROM edges),
                 degrees AS (SELECT nkey,count(*) AS degree FROM incidents GROUP BY nkey)
                 SELECT n.label FROM degrees d JOIN nodes n ON n.nkey=d.nkey
                 WHERE json_extract(n.payload,'$.kind') NOT IN ('file','module','document','group','rationale')
                 ORDER BY d.degree DESC,n.id LIMIT 64",
        };
        let mut statement = tx.prepare(sql)?;
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
        drop(statement);
        tx.commit()?;
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
        let layout = storage_layout(&tx)?;
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
                    match layout {
                        StorageLayout::Legacy => {
                            "SELECT COUNT(*),COALESCE(SUM(length(CAST(payload AS BLOB))),0) FROM refs WHERE resolved_target IS NULL"
                        }
                        StorageLayout::Compact => {
                            "SELECT COUNT(*),COALESCE(SUM(length(CAST(payload AS BLOB))),0) FROM refs WHERE resolved_target_key IS NULL"
                        }
                    },
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
                    match layout { StorageLayout::Legacy => "SELECT COALESCE(SUM(length(CAST(path AS BLOB))*6+length(CAST(hash AS BLOB))+100),0) FROM files WHERE EXISTS(SELECT 1 FROM nodes WHERE owner_file=files.path)", StorageLayout::Compact => "SELECT COALESCE(SUM(length(CAST(path AS BLOB))*6+length(CAST(hash AS BLOB))+100),0) FROM files WHERE EXISTS(SELECT 1 FROM nodes WHERE owner_key=files.fkey)" },
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
                match layout {
                    StorageLayout::Legacy => {
                        "SELECT payload FROM refs WHERE resolved_target IS NULL ORDER BY id"
                    }
                    StorageLayout::Compact => {
                        "SELECT payload FROM refs WHERE resolved_target_key IS NULL ORDER BY id"
                    }
                },
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
                match layout { StorageLayout::Legacy => "SELECT path,hash FROM files WHERE EXISTS(SELECT 1 FROM nodes WHERE owner_file=files.path) ORDER BY path", StorageLayout::Compact => "SELECT path,hash FROM files WHERE EXISTS(SELECT 1 FROM nodes WHERE owner_key=files.fkey) ORDER BY path" },
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
        let layout = storage_layout(&tx)?;
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
                    match layout {
                        StorageLayout::Legacy => {
                            "SELECT payload FROM nodes WHERE owner_file=?1 OR owner_file IN (SELECT path FROM files WHERE path GLOB ?2)"
                        }
                        StorageLayout::Compact => {
                            "SELECT payload FROM nodes WHERE owner_key=(SELECT fkey FROM files WHERE path=?1) OR owner_key IN (SELECT fkey FROM files WHERE path GLOB ?2)"
                        }
                    },
                    &mut old_nodes,
                ),
                (
                    match layout {
                        StorageLayout::Legacy => {
                            "SELECT payload FROM edges WHERE owner_file=?1 OR owner_file IN (SELECT path FROM files WHERE path GLOB ?2)"
                        }
                        StorageLayout::Compact => {
                            "SELECT payload FROM edges WHERE owner_key=(SELECT fkey FROM files WHERE path=?1) OR owner_key IN (SELECT fkey FROM files WHERE path GLOB ?2)"
                        }
                    },
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
        let search_migrated = ensure_compact_storage(&tx, &mut keys)?;
        let aliases_migrated = !keys.is_empty();
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
                    "SELECT EXISTS(SELECT 1 FROM nodes WHERE owner_key=(SELECT fkey FROM files WHERE path=?1) AND json_extract(payload,'$.metadata.language')='ruby')",
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
                "SELECT e.payload,f.path FROM nodes n JOIN edges e ON e.target_key=n.nkey
                 JOIN files f ON f.fkey=e.owner_key
                 WHERE n.owner_key=(SELECT fkey FROM files WHERE path=?1) AND e.ref_key IS NULL",
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
                "SELECT binding_key FROM nodes WHERE owner_key=(SELECT fkey FROM files WHERE path=?1) AND binding_key IS NOT NULL
                 UNION SELECT a.binding_key FROM node_aliases a JOIN nodes n ON n.nkey=a.node_key WHERE n.owner_key=(SELECT fkey FROM files WHERE path=?1)",
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
                        "INSERT INTO node_aliases(node_key,binding_key) VALUES((SELECT nkey FROM nodes WHERE id=?1),?2) ON CONFLICT(node_key,binding_key) DO NOTHING",
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
                tx.execute("INSERT INTO refs(source_key,owner_key,relation,payload,resolved_target_key,resolution_reason) VALUES((SELECT nkey FROM nodes WHERE id=?1),(SELECT fkey FROM files WHERE path=?2),?3,?4,NULL,?5)",
                    params![reference.source, facts.path, reference.relation, serde_json::to_string(reference)?, reference.reason])?;
                for (priority, key) in reference.candidate_keys.iter().enumerate() {
                    tx.execute(
                        "INSERT INTO ref_keys(ref_key,priority,binding_key) VALUES((SELECT rkey FROM refs WHERE id=?1),?2,?3)",
                        params![reference.id, priority as i64, key],
                    )?;
                }
                affected.insert(reference.id.clone());
            }
        }
        for key in keys {
            let mut stmt = tx.prepare("SELECT r.id FROM ref_keys k JOIN refs r ON r.rkey=k.ref_key WHERE k.binding_key=?1")?;
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
                "SELECT r.payload FROM refs r JOIN nodes n ON n.nkey=r.source_key WHERE json_extract(n.payload,'$.metadata.language')='ruby' ORDER BY r.id",
            )?;
            for reference in references {
                let keys = context
                    .inherited_keys(&reference)
                    .unwrap_or_else(|| reference.candidate_keys.clone());
                tx.execute(
                    "DELETE FROM ref_keys WHERE ref_key=(SELECT rkey FROM refs WHERE id=?1)",
                    [&reference.id],
                )?;
                for (priority, key) in keys.iter().enumerate() {
                    tx.execute(
                        "INSERT INTO ref_keys(ref_key,priority,binding_key) VALUES((SELECT rkey FROM refs WHERE id=?1),?2,?3)",
                        params![reference.id, priority as i64, key],
                    )?;
                }
                affected.insert(reference.id);
            }
        }
        if !affected.is_empty() {
            let mut payload_statement = tx.prepare("SELECT payload FROM refs WHERE id=?1")?;
            let mut delete_edges =
                tx.prepare("DELETE FROM edges WHERE ref_key=(SELECT rkey FROM refs WHERE id=?1)")?;
            let mut candidate_keys = tx.prepare("SELECT binding_key FROM ref_keys WHERE ref_key=(SELECT rkey FROM refs WHERE id=?1) ORDER BY priority")?;
            let mut update_resolution = tx.prepare("UPDATE refs SET resolved_target_key=(SELECT nkey FROM nodes WHERE id=?1),resolution_reason=?2 WHERE id=?3")?;
            for id in affected {
                resolve_reference(
                    &tx,
                    &id,
                    &mut payload_statement,
                    &mut delete_edges,
                    &mut candidate_keys,
                    &mut update_resolution,
                )?;
            }
        }
        let previous_coverage: Coverage = serde_json::from_str(&tx.query_row(
            "SELECT coverage FROM metadata WHERE singleton=1",
            [],
            |r| r.get::<_, String>(0),
        )?)?;
        // The unchanged count describes this scan, not a change in stored facts.
        // Physical upgrades may write on a no-op scan; only logical changes
        // (including coverage) publish a graph generation.
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
        } else {
            ensure!(
                kind == "empty" && root.is_none(),
                "import requires an empty Graf database"
            );
        }
        ensure_compact_storage(&tx, &mut BTreeSet::new())?;
        if refresh {
            tx.execute("DELETE FROM edges", [])?;
            tx.execute("DELETE FROM nodes", [])?;
        }
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
    let tx = conn.unchecked_transaction()?;
    let conn = &tx;
    let app: i64 = conn.pragma_query_value(None, "application_id", |r| r.get(0))?;
    ensure!(
        app == APPLICATION_ID,
        "not a Graf database; refusing unrelated database"
    );
    let layout = storage_layout(conn)?;
    let kind: String = conn.query_row("SELECT kind FROM metadata WHERE singleton=1", [], |r| {
        r.get(0)
    })?;
    ensure!(
        matches!(kind.as_str(), "empty" | "native" | "imported"),
        "invalid Graf database kind"
    );
    // Prepare without scanning or writing. An incomplete schema is not usable.
    conn.prepare(match layout {
        StorageLayout::Legacy => "SELECT n.payload,n.owner_file,e.payload,e.source,e.target,e.owner_file,e.ref_id,r.payload,r.source,r.owner_file,r.resolved_target,k.ref_id,k.priority,f.hash FROM nodes n,edges e,refs r,ref_keys k,files f LIMIT 0",
        StorageLayout::Compact => "SELECT n.nkey,n.payload,n.owner_key,e.payload,e.source_key,e.target_key,e.owner_key,e.ref_key,r.rkey,r.id,r.payload,r.source_key,r.owner_key,r.resolved_target_key,k.ref_key,k.priority,f.fkey,f.hash FROM nodes n,edges e,refs r,ref_keys k,files f LIMIT 0",
    })?;
    conn.prepare("SELECT rowid FROM node_search LIMIT 0")?;
    tx.commit()?;
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

// Only called inside an explicit write transaction, after its generation/kind
// checks (or during creation). Index replacement rolls back with the graph on
// failure and does not advance the logical graph generation on its own.
fn ensure_storage_indices(tx: &Transaction<'_>) -> Result<()> {
    for &(name, sql) in STORAGE_INDICES {
        let current: Option<String> = tx
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type='index' AND name=?1",
                [name],
                |row| row.get(0),
            )
            .optional()?;
        if current.as_deref() != Some(sql) {
            // Names and definitions are static, never user-provided SQL.
            tx.execute_batch(&format!("DROP INDEX IF EXISTS {name}; {sql};"))?;
        }
    }
    Ok(())
}

// Called only after the immediate transaction's baseline/kind/root checks.
// Return projection changes separately: physical-only upgrades do not publish
// a new graph generation. Backfilled aliases must reach the caller's rebind set.
fn ensure_compact_storage(tx: &Transaction<'_>, keys: &mut BTreeSet<String>) -> Result<bool> {
    let layout = storage_layout(tx)?;
    let version: i64 = tx.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version < 3 {
        // A projection must preserve the old public identity exactly, not
        // silently substitute a missing, coerced, or different payload value.
        let invalid: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM refs WHERE typeof(id)!='text'
             OR json_type(payload,'$.id') IS NOT 'text'
             OR id IS NOT json_extract(payload,'$.id'))",
            [],
            |row| row.get(0),
        )?;
        ensure!(
            !invalid,
            "storage upgrade reference payload identity mismatch"
        );
    }
    // Legacy replacement builds FTS once, after the copied nodes are published.
    let search_changed = ensure_search(tx, layout == StorageLayout::Compact)?;
    if layout == StorageLayout::Legacy {
        let aliases: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='node_aliases')",
            [],
            |row| row.get(0),
        )?;
        tx.execute_batch(COMPACT_TABLES)?;
        tx.execute_batch(COMPACT_REFERENCE_TABLES)?;
        tx.execute_batch(
            "INSERT INTO compact_files(fkey,path,hash,module,diagnostics)
                 SELECT rowid,path,hash,module,diagnostics FROM files;
             INSERT INTO compact_nodes(nkey,id,label,qualified_name,binding_key,file,owner_key,payload,search)
                 SELECT rowid,id,label,qualified_name,binding_key,file,
                     (SELECT fkey FROM compact_files WHERE path=nodes.owner_file),payload,search FROM nodes;
             INSERT INTO compact_refs(rkey,source_key,owner_key,relation,payload,resolved_target_key,resolution_reason)
                 SELECT rowid,(SELECT nkey FROM compact_nodes WHERE id=refs.source),
                     (SELECT fkey FROM compact_files WHERE path=refs.owner_file),relation,payload,
                     (SELECT nkey FROM compact_nodes WHERE id=refs.resolved_target),resolution_reason FROM refs;
             INSERT INTO compact_ref_keys(ref_key,priority,binding_key)
                 SELECT (SELECT rkey FROM compact_refs WHERE id=ref_keys.ref_id),priority,binding_key FROM ref_keys;
             INSERT INTO compact_edges(id,source_key,target_key,relation,directed,owner_key,ref_key,payload)
                 SELECT id,(SELECT nkey FROM compact_nodes WHERE id=edges.source),
                     (SELECT nkey FROM compact_nodes WHERE id=edges.target),relation,directed,
                     (SELECT fkey FROM compact_files WHERE path=edges.owner_file),
                     (SELECT rkey FROM compact_refs WHERE id=edges.ref_id),payload FROM edges;",
        )?;
        if aliases {
            tx.execute_batch(
                "INSERT INTO compact_node_aliases(node_key,binding_key)
                     SELECT (SELECT nkey FROM compact_nodes WHERE id=node_aliases.node_id),binding_key FROM node_aliases;",
            )?;
        }
        for table in [
            "files",
            "nodes",
            "refs",
            "ref_keys",
            "edges",
            "node_aliases",
        ] {
            if table == "node_aliases" && !aliases {
                continue;
            }
            let equal: bool = tx.query_row(
                &format!(
                    "SELECT (SELECT count(*) FROM {table})=(SELECT count(*) FROM compact_{table})"
                ),
                [],
                |row| row.get(0),
            )?;
            ensure!(equal, "storage upgrade row count mismatch for {table}");
        }
        // NOT NULL/FK constraints reject missing mandatory links. Optional
        // links must distinguish a legitimate NULL from a failed lookup too.
        for sql in [
            "SELECT EXISTS(SELECT 1 FROM nodes o JOIN compact_nodes n ON n.nkey=o.rowid WHERE o.owner_file IS NOT NULL AND n.owner_key IS NULL)",
            "SELECT EXISTS(SELECT 1 FROM refs o JOIN compact_refs r ON r.rkey=o.rowid WHERE o.resolved_target IS NOT NULL AND r.resolved_target_key IS NULL)",
            "SELECT EXISTS(SELECT 1 FROM edges o JOIN compact_edges e ON e.id=o.id WHERE (o.owner_file IS NOT NULL AND e.owner_key IS NULL) OR (o.ref_id IS NOT NULL AND e.ref_key IS NULL))",
        ] {
            let missing: bool = tx.query_row(sql, [], |row| row.get(0))?;
            ensure!(!missing, "storage upgrade cannot map an existing identity");
        }
        tx.execute_batch(
            "DROP TRIGGER nodes_insert;
             DROP TRIGGER nodes_delete;
             DROP TABLE node_search;
             DROP TABLE edges;
             DROP TABLE ref_keys;
             DROP TABLE IF EXISTS node_aliases;
             DROP TABLE refs;
             DROP TABLE nodes;
             DROP TABLE files;",
        )?;
        tx.execute_batch(COMPACT_PUBLISH)?;
        tx.execute_batch(COMPACT_REFERENCE_PUBLISH)?;
        if !aliases {
            backfill_aliases(tx, keys)?;
        }
    } else if version == 2 {
        // Reuse the same reference schema with the already-published parents.
        // Copy FK children before dropping them; foreign_keys stays enabled.
        tx.execute_batch(
            &COMPACT_REFERENCE_TABLES
                .replace("compact_nodes", "nodes")
                .replace("compact_files", "files"),
        )?;
        tx.execute_batch(
            "INSERT INTO compact_refs(rkey,source_key,owner_key,relation,payload,resolved_target_key,resolution_reason)
                 SELECT rkey,source_key,owner_key,relation,payload,resolved_target_key,resolution_reason FROM refs;
             INSERT INTO compact_ref_keys(ref_key,priority,binding_key)
                 SELECT ref_key,priority,binding_key FROM ref_keys;
             INSERT INTO compact_edges(rowid,id,source_key,target_key,relation,directed,owner_key,ref_key,payload)
                 SELECT rowid,id,source_key,target_key,relation,directed,owner_key,ref_key,payload FROM edges;",
        )?;
        for table in ["refs", "ref_keys", "edges"] {
            let equal: bool = tx.query_row(
                &format!(
                    "SELECT (SELECT count(*) FROM {table})=(SELECT count(*) FROM compact_{table})"
                ),
                [],
                |row| row.get(0),
            )?;
            ensure!(equal, "storage upgrade row count mismatch for {table}");
        }
        tx.execute_batch("DROP TABLE edges; DROP TABLE ref_keys; DROP TABLE refs;")?;
        tx.execute_batch(COMPACT_REFERENCE_PUBLISH)?;
    }
    if version < 3 {
        let violations: i64 =
            tx.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| {
                row.get(0)
            })?;
        ensure!(violations == 0, "storage upgrade foreign key check failed");
        tx.pragma_update(None, "user_version", 3)?;
    }
    ensure_aliases(tx, keys)?;
    ensure_storage_indices(tx)?;
    Ok(search_changed)
}

fn ensure_aliases(tx: &Transaction<'_>, keys: &mut BTreeSet<String>) -> Result<()> {
    let exists: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='node_aliases')",
        [],
        |row| row.get(0),
    )?;
    if !exists {
        tx.execute_batch(
            "CREATE TABLE node_aliases (
                node_key INTEGER NOT NULL REFERENCES nodes(nkey) ON DELETE CASCADE,
                binding_key TEXT NOT NULL, PRIMARY KEY(node_key,binding_key)
             ) WITHOUT ROWID;
             CREATE INDEX node_aliases_binding ON node_aliases(binding_key,node_key);",
        )?;
        backfill_aliases(tx, keys)?;
    }
    Ok(())
}

fn backfill_aliases(tx: &Transaction<'_>, keys: &mut BTreeSet<String>) -> Result<()> {
    let mut stmt = tx.prepare("SELECT payload FROM nodes WHERE owner_key IS NOT NULL")?;
    for payload in stmt.query_map([], |row| row.get::<_, String>(0))? {
        let node: Node = serde_json::from_str(&payload?)?;
        for alias in binding_aliases(&node)? {
            keys.insert(alias.to_owned());
            tx.execute(
                "INSERT INTO node_aliases(node_key,binding_key) VALUES((SELECT nkey FROM nodes WHERE id=?1),?2) ON CONFLICT(node_key,binding_key) DO NOTHING",
                params![node.id, alias],
            )?;
        }
    }
    Ok(())
}

// Search enrichment is another additive schema-1 extension. Migration runs
// only during an explicit write, under the same transaction/generation check.
fn ensure_search(tx: &Transaction<'_>, rebuild_fts: bool) -> Result<bool> {
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
        version <= SEARCH_VERSION,
        "unsupported Graf search index version {version}"
    );
    let packed: bool = tx.query_row(
        "SELECT instr(sql,'contentless_delete=1')>0 FROM sqlite_master
         WHERE type='table' AND name='node_search'",
        [],
        |row| row.get(0),
    )?;
    if version == SEARCH_VERSION && (!rebuild_fts || packed) {
        return Ok(false);
    }
    let mut changed = false;
    if version != SEARCH_VERSION {
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
    }
    if rebuild_fts && !packed {
        // Contentless-delete FTS5 retains ordinary DELETE/INSERT semantics and
        // positions/docsize, without another copy of nodes.search. Queries read
        // only rowid/MATCH and fetch all public data from nodes.
        tx.execute_batch(
            "DROP TRIGGER nodes_insert;
             DROP TRIGGER nodes_delete;
             DROP TABLE node_search;
             CREATE VIRTUAL TABLE node_search USING fts5(text, content='', contentless_delete=1);
             CREATE TRIGGER nodes_insert AFTER INSERT ON nodes BEGIN
                 INSERT INTO node_search(rowid,text) VALUES(new.rowid,new.search);
             END;
             CREATE TRIGGER nodes_delete AFTER DELETE ON nodes BEGIN
                 DELETE FROM node_search WHERE rowid=old.rowid;
             END;",
        )?;
    }
    if rebuild_fts && (changed || !packed) {
        if packed {
            tx.execute("DELETE FROM node_search", [])?;
        }
        tx.execute(
            "INSERT INTO node_search(rowid,text) SELECT rowid,search FROM nodes",
            [],
        )?;
    }
    tx.execute(
        "UPDATE metadata SET search_version=?1 WHERE singleton=1",
        [SEARCH_VERSION],
    )?;
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
    if spelling
        .nfkd()
        .any(unicode_normalization::char::is_combining_mark)
    {
        let folded: String = spelling
            .nfkd()
            .filter(|c| !unicode_normalization::char::is_combining_mark(*c))
            .flat_map(char::to_lowercase)
            .collect();
        search.push(' ');
        search.push_str(&folded);
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
    // Retain the normalized spelling for exact identifier tokens, then the
    // transformed form for camel-case and CJK recall. Literal callers of the
    // original query API still need compatibility-form tokens when NFKC
    // changes their spelling; ordinary text needs no duplicate raw copy.
    if text == spelling {
        format!("{spelling} {search}")
    } else {
        format!("{text} {spelling} {search}")
    }
}

fn insert_node(conn: &Connection, node: &Node, owner: Option<&str>) -> Result<()> {
    ensure!(!node.id.is_empty(), "node ID cannot be empty");
    let owner = owner
        .map(|path| {
            conn.query_row("SELECT fkey FROM files WHERE path=?1", [path], |row| {
                row.get::<_, i64>(0)
            })
        })
        .transpose()
        .context("missing node owner")?;
    conn.execute("INSERT INTO nodes(id,label,qualified_name,binding_key,file,owner_key,payload,search) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
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
    let owner = owner
        .map(|path| {
            conn.query_row("SELECT fkey FROM files WHERE path=?1", [path], |row| {
                row.get::<_, i64>(0)
            })
        })
        .transpose()
        .context("missing edge owner")?;
    let reference = reference
        .map(|id| {
            conn.query_row("SELECT rkey FROM refs WHERE id=?1", [id], |row| {
                row.get::<_, i64>(0)
            })
        })
        .transpose()
        .context("missing edge reference")?;
    conn.execute("INSERT INTO edges(id,source_key,target_key,relation,directed,owner_key,ref_key,payload) VALUES(?1,(SELECT nkey FROM nodes WHERE id=?2),(SELECT nkey FROM nodes WHERE id=?3),?4,?5,?6,?7,?8)",
        params![edge.id,edge.source,edge.target,edge.relation,edge.directed,owner,reference,serde_json::to_string(edge)?])?;
    Ok(())
}

fn resolve_reference(
    tx: &Transaction<'_>,
    id: &str,
    payload_statement: &mut Statement<'_>,
    delete_edges: &mut Statement<'_>,
    candidate_keys: &mut Statement<'_>,
    update_resolution: &mut Statement<'_>,
) -> Result<()> {
    let payload: String = payload_statement.query_row([id], |r| r.get(0))?;
    let reference: Reference = serde_json::from_str(&payload)?;
    delete_edges.execute([id])?;
    let mut target = None;
    let mut reason = if reference.reason.is_empty() {
        "no matching binding".to_owned()
    } else {
        reference.reason.clone()
    };
    let keys = candidate_keys
        .query_map([id], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for key in &keys {
        let mut stmt = tx.prepare(
            "SELECT id FROM nodes WHERE binding_key=?1
                        UNION SELECT n.id FROM node_aliases a JOIN nodes n ON n.nkey=a.node_key WHERE a.binding_key=?1
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
    update_resolution.execute(params![target, reason, id])?;
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
    let layout = storage_layout(conn)?;
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
        unresolved_references: count(match layout {
            StorageLayout::Legacy => "SELECT count(*) FROM refs WHERE resolved_target IS NULL",
            StorageLayout::Compact => "SELECT count(*) FROM refs WHERE resolved_target_key IS NULL",
        })?,
        kind,
        coverage: serde_json::from_str(&coverage)?,
        diagnostics,
    })
}

#[cfg(test)]
mod compaction_tests {
    use super::*;
    use rusqlite::hooks::{AuthAction, AuthContext, Authorization};

    fn freed_pages(store: &Store) -> Result<()> {
        store.conn.execute_batch(
            "CREATE TABLE discarded(data BLOB);
             INSERT INTO discarded VALUES(zeroblob(262144));
             DROP TABLE discarded;",
        )?;
        Ok(())
    }

    #[test]
    fn compaction_lock_failure_and_active_transaction_leave_graph_unchanged() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("locked.db");
        let mut store = Store::create(&path)?;
        store.conn.busy_timeout(Duration::from_millis(20))?;
        freed_pages(&store)?;
        let before = serde_json::to_value(store.snapshot()?)?;
        let other = Connection::open(&path)?;
        other.execute_batch("BEGIN IMMEDIATE")?;
        let error = store.compact().unwrap_err();
        assert!(format!("{error:#}").contains("locked"), "{error:#}");
        assert_eq!(serde_json::to_value(store.snapshot()?)?, before);
        other.execute_batch("ROLLBACK")?;
        store.conn.execute_batch("BEGIN")?;
        let error = store.compact().unwrap_err();
        assert!(format!("{error:#}").contains("active transaction"));
        store.conn.execute_batch("ROLLBACK")?;
        assert_eq!(serde_json::to_value(store.snapshot()?)?, before);
        let report = store.compact()?;
        assert_eq!(report.free_pages_after, 0);
        assert!(report.pages_after < report.pages_before);
        assert!(!report.checkpoint_busy);
        assert_eq!(serde_json::to_value(store.snapshot()?)?, before);
        Ok(())
    }

    #[test]
    fn compaction_reports_a_pinned_wal_reader_without_claiming_disk_shrink() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("reader.db");
        let mut store = Store::create(&path)?;
        store.conn.busy_timeout(Duration::from_millis(20))?;
        store
            .conn
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")?;
        let reader = Connection::open(&path)?;
        reader.execute_batch("BEGIN")?;
        let generation: i64 =
            reader.query_row("SELECT generation FROM metadata", [], |row| row.get(0))?;
        let old_pages: i64 = reader.pragma_query_value(None, "page_count", |row| row.get(0))?;
        // Commit pages after the pinned reader's end mark, then vacuum them.
        freed_pages(&store)?;
        let before = serde_json::to_value(store.snapshot()?)?;
        let report = store.compact()?;
        assert!(report.checkpoint_busy);
        assert_eq!(report.free_pages_after, 0);
        assert!(std::fs::metadata(path.with_extension("db-wal"))?.len() > 0);
        assert_eq!(
            reader.query_row("SELECT generation FROM metadata", [], |row| row
                .get::<_, i64>(0))?,
            generation
        );
        assert_eq!(
            reader.pragma_query_value(None, "page_count", |row| row.get::<_, i64>(0))?,
            old_pages
        );
        assert_eq!(serde_json::to_value(store.snapshot()?)?, before);
        reader.execute_batch("COMMIT")?;
        let report = store.compact()?;
        assert!(!report.checkpoint_busy);
        assert_eq!(
            std::fs::metadata(&path)?.len(),
            report.pages_after * report.page_size
        );
        assert_eq!(std::fs::metadata(path.with_extension("db-wal"))?.len(), 0);
        assert_eq!(serde_json::to_value(store.snapshot()?)?, before);
        Ok(())
    }

    #[test]
    fn checkpoint_error_explicitly_reports_that_compaction_already_completed() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("checkpoint.db");
        let mut store = Store::create(&path)?;
        freed_pages(&store)?;
        let before = serde_json::to_value(store.snapshot()?)?;
        let free: i64 = store
            .conn
            .pragma_query_value(None, "freelist_count", |row| row.get(0))?;
        assert!(free > 0);
        store
            .conn
            .authorizer(Some(|ctx: AuthContext<'_>| match ctx.action {
                AuthAction::Pragma {
                    pragma_name: "wal_checkpoint",
                    ..
                } => Authorization::Deny,
                _ => Authorization::Allow,
            }))?;
        let error = store.compact().unwrap_err();
        assert!(
            format!("{error:#}").contains("compaction completed but checkpoint failed"),
            "{error:#}"
        );
        assert_eq!(
            store
                .conn
                .pragma_query_value(None, "freelist_count", |row| row.get::<_, i64>(0))?,
            0
        );
        assert_eq!(serde_json::to_value(store.snapshot()?)?, before);
        store
            .conn
            .authorizer(None::<fn(AuthContext<'_>) -> Authorization>)?;
        assert!(!store.compact()?.checkpoint_busy);
        Ok(())
    }
}
