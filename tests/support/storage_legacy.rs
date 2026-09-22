// Test-only reconstruction of actual format-1 tables. Public identities and
// every persisted projection are copied; merely lowering user_version is not
// a legacy fixture. Connections and databases must belong to the calling test.
use rusqlite::{Connection, types::Value};

const LEGACY_SCHEMA: &str = r#"
CREATE TABLE files (
    path TEXT PRIMARY KEY, hash TEXT NOT NULL, module TEXT NOT NULL, diagnostics TEXT NOT NULL
);
CREATE TABLE nodes (
    id TEXT PRIMARY KEY, label TEXT NOT NULL, qualified_name TEXT, binding_key TEXT,
    file TEXT NOT NULL, owner_file TEXT REFERENCES files(path) ON DELETE CASCADE,
    payload TEXT NOT NULL, search TEXT NOT NULL
);
CREATE INDEX nodes_label ON nodes(label, id);
CREATE INDEX nodes_file ON nodes(file, id);
CREATE VIRTUAL TABLE node_search USING fts5(text, content='', contentless_delete=1);
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
) WITHOUT ROWID;
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
CREATE INDEX nodes_qualified ON nodes(qualified_name, id) WHERE qualified_name IS NOT NULL;
CREATE INDEX nodes_binding ON nodes(binding_key, id) WHERE binding_key IS NOT NULL;
CREATE INDEX nodes_owner ON nodes(owner_file) WHERE owner_file IS NOT NULL;
CREATE INDEX edges_source_direction ON edges(source, id) WHERE directed=0;
CREATE INDEX edges_target_direction ON edges(target, id) WHERE directed=0;
CREATE INDEX edges_source_direction_relation ON edges(source, relation, id) WHERE directed=0;
CREATE INDEX edges_target_direction_relation ON edges(target, relation, id) WHERE directed=0;
CREATE INDEX edges_owner ON edges(owner_file) WHERE owner_file IS NOT NULL;
CREATE TABLE node_aliases (
    node_id TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    binding_key TEXT NOT NULL, PRIMARY KEY(node_id,binding_key)
) WITHOUT ROWID;
CREATE INDEX node_aliases_binding ON node_aliases(binding_key,node_id);
"#;

fn rows(conn: &Connection, sql: &str) -> anyhow::Result<Vec<Vec<Value>>> {
    let mut statement = conn.prepare(sql)?;
    let columns = statement.column_count();
    Ok(statement
        .query_map([], |row| (0..columns).map(|i| row.get(i)).collect())?
        .collect::<rusqlite::Result<_>>()?)
}

pub fn restore_legacy(conn: &Connection, packed: bool) -> anyhow::Result<()> {
    conn.pragma_update(None, "foreign_keys", true)?;
    let tx = conn.unchecked_transaction()?;
    let version: i64 = tx.pragma_query_value(None, "user_version", |row| row.get(0))?;
    anyhow::ensure!(
        matches!(version, 2..=4),
        "legacy fixture expects a compact input"
    );
    let definitions = [
        (
            "files",
            "rowid,path,hash,module,diagnostics",
            "SELECT fkey,path,hash,module,diagnostics FROM files ORDER BY fkey",
        ),
        (
            "nodes",
            "rowid,id,label,qualified_name,binding_key,file,owner_file,payload,search",
            "SELECT n.nkey,n.id,n.label,n.qualified_name,n.binding_key,n.file,f.path,n.payload,n.search FROM nodes n LEFT JOIN files f ON f.fkey=n.owner_key ORDER BY n.nkey",
        ),
        (
            "refs",
            "rowid,id,source,owner_file,relation,payload,resolved_target,resolution_reason",
            "SELECT r.rkey,r.id,n.id,f.path,r.relation,r.payload,t.id,r.resolution_reason FROM refs r JOIN nodes n ON n.nkey=r.source_key JOIN files f ON f.fkey=r.owner_key LEFT JOIN nodes t ON t.nkey=r.resolved_target_key ORDER BY r.rkey",
        ),
        (
            "ref_keys",
            "ref_id,priority,binding_key",
            "SELECT r.id,k.priority,k.binding_key FROM ref_keys k JOIN refs r ON r.rkey=k.ref_key ORDER BY r.id,k.priority",
        ),
        (
            "node_aliases",
            "node_id,binding_key",
            "SELECT n.id,a.binding_key FROM node_aliases a JOIN nodes n ON n.nkey=a.node_key ORDER BY n.id,a.binding_key",
        ),
        (
            "edges",
            "id,source,target,relation,directed,owner_file,ref_id,payload",
            "SELECT e.id,s.id,t.id,e.relation,e.directed,f.path,r.id,e.payload FROM edges e JOIN nodes s ON s.nkey=e.source_key JOIN nodes t ON t.nkey=e.target_key LEFT JOIN files f ON f.fkey=e.owner_key LEFT JOIN refs r ON r.rkey=e.ref_key ORDER BY e.id",
        ),
    ];
    let mut records = Vec::new();
    for (table, _, select) in definitions {
        let values = rows(&tx, select)?;
        let count: i64 = tx.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
            row.get(0)
        })?;
        anyhow::ensure!(
            values.len() as i64 == count,
            "fixture lost rows from {table}"
        );
        records.push(values);
    }
    let dangling: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM refs WHERE resolved_target_key IS NOT NULL AND NOT EXISTS(SELECT 1 FROM nodes WHERE nkey=resolved_target_key))", [], |row| row.get(0))?;
    anyhow::ensure!(!dangling, "fixture cannot copy dangling resolution");
    tx.execute_batch(
        "DROP TRIGGER nodes_insert; DROP TRIGGER nodes_delete; DROP TABLE node_search;
        DROP TABLE edges; DROP TABLE ref_keys; DROP TABLE node_aliases;
        DROP TABLE refs; DROP TABLE nodes; DROP TABLE files;",
    )?;
    let schema = if packed {
        LEGACY_SCHEMA.to_owned()
    } else {
        LEGACY_SCHEMA
            .replace(") WITHOUT ROWID;", ");")
            .replace("fts5(text, content='', contentless_delete=1)", "fts5(text)")
    };
    tx.execute_batch(&schema)?;
    for ((table, columns, _), records) in definitions.iter().zip(records) {
        for row in records {
            let marks = vec!["?"; row.len()].join(",");
            tx.execute(
                &format!("INSERT INTO {table}({columns}) VALUES({marks})"),
                rusqlite::params_from_iter(row),
            )?;
        }
    }
    let violations: i64 =
        tx.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| {
            row.get(0)
        })?;
    anyhow::ensure!(violations == 0, "legacy fixture violated foreign keys");
    tx.pragma_update(None, "user_version", 1)?;
    tx.commit()?;
    Ok(())
}
