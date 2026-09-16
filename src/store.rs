use crate::model::*;
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OpenFlags, Transaction, params};
use std::{collections::BTreeSet, fs, path::Path, time::Duration};

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

    pub fn apply_native(
        &mut self,
        root: &str,
        changed: Vec<FileFacts>,
        deleted: Vec<String>,
        coverage: Coverage,
    ) -> Result<IndexReport> {
        ensure!(!root.is_empty(), "native root cannot be empty");
        validate_facts(&changed, &deleted)?;
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        ensure!(
            generation(&tx)? == self.baseline_generation,
            "index changed since this scan started; retry indexing from a fresh Store"
        );
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
        let mut removed = 0;
        for path in deleted.iter().chain(changed.iter().map(|f| &f.path)) {
            let mut stmt = tx.prepare(
                "SELECT binding_key FROM nodes WHERE owner_file=?1 AND binding_key IS NOT NULL",
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
        for id in affected {
            resolve_reference(&tx, &id)?;
        }
        let changed_generation = kind == "empty" || !changed.is_empty() || removed > 0;
        tx.execute("UPDATE metadata SET kind='native', root=?1, coverage=?2, generation=generation+?3 WHERE singleton=1",
            params![root, serde_json::to_string(&coverage)?, i64::from(changed_generation)])?;
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
        };
        tx.commit()?;
        self.baseline_generation = report.generation;
        Ok(report)
    }

    pub fn import_graph(&mut self, graph: ImportedGraph) -> Result<Stats> {
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let kind: String =
            tx.query_row("SELECT kind FROM metadata WHERE singleton=1", [], |r| {
                r.get(0)
            })?;
        ensure!(kind == "empty", "import requires an empty Graf database");
        // All inserts and mode changes roll back together on duplicate IDs or
        // dangling endpoints. Nothing replaces an existing row.
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

fn connect(path: &Path) -> Result<Connection> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| format!("cannot open Graf database {}", path.display()))?;
    conn.busy_timeout(Duration::from_secs(5))?;
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

fn search_text(node: &Node) -> String {
    let text = format!(
        "{} {} {}",
        node.label,
        node.qualified_name.as_deref().unwrap_or(""),
        node.file
    );
    let mut search = String::with_capacity(text.len() * 2);
    let mut previous_lower = false;
    for c in text.chars() {
        if c.is_uppercase() && previous_lower {
            search.push(' ');
        }
        previous_lower = c.is_lowercase() || c.is_numeric();
        search.push(if c == '_' { ' ' } else { c });
    }
    // Retain the original spelling as well as identifier components.
    format!("{text} {search}")
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
    for key in &reference.candidate_keys {
        let mut stmt =
            tx.prepare("SELECT id FROM nodes WHERE binding_key=?1 ORDER BY id LIMIT 2")?;
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
        let edge = Edge {
            id: format!("reference:{}", reference.id),
            source: reference.source,
            target,
            relation: reference.relation,
            directed: true,
            file: Some(reference.file.clone()),
            line: Some(reference.line),
            confidence: "statically_resolved".to_owned(),
            metadata: serde_json::json!({"reference_id": reference.id}),
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
