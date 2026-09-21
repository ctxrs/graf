use graf::{
    model::{Coverage, FileFacts, ImportedGraph, Node, QueryOptions, Reference},
    query::SearchOptions,
    store::{StaleStore, Store},
};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::path::Path;

#[path = "support/storage_legacy.rs"]
mod storage_legacy;

fn node(id: &str, path: &str, label: &str, aliases: &[&str]) -> Node {
    Node {
        id: id.into(),
        label: label.into(),
        kind: "function".into(),
        file: path.into(),
        line: Some(2),
        end_line: Some(3),
        qualified_name: Some(id.into()),
        binding_key: Some(format!("direct:{id}")),
        metadata: json!({"binding_aliases":aliases,"evidence":"preserved source evidence"}),
    }
}

fn facts(path: &str, nodes: Vec<Node>, references: Vec<Reference>) -> FileFacts {
    FileFacts {
        path: path.into(),
        hash: "fixture-hash".into(),
        module: path.into(),
        nodes,
        edges: vec![],
        references,
        diagnostics: vec![],
    }
}

fn definitions() -> FileFacts {
    facts(
        "defs.py",
        vec![
            node("target-z", "defs.py", "NeedleZulu", &["z:key", "z:key"]),
            node("target-a", "defs.py", "NeedleAlpha", &["a:key"]),
        ],
        vec![],
    )
}

fn caller() -> FileFacts {
    facts(
        "caller.py",
        vec![node("caller", "caller.py", "Caller", &[])],
        ["02-ref", "01-ref"]
            .into_iter()
            .map(|id| Reference {
                id: id.into(),
                source: "caller".into(),
                label: "work".into(),
                relation: "calls".into(),
                file: "caller.py".into(),
                line: 4,
                // Priority is intentionally the reverse of lexical key order.
                candidate_keys: vec!["z:key".into(), "a:key".into()],
                reason: "missing or ambiguous target".into(),
            })
            .collect(),
    )
}

fn imported(label: &str) -> ImportedGraph {
    ImportedGraph {
        nodes: vec![node("imported", "external.txt", label, &[])],
        edges: vec![],
        metadata: json!({"original":"preserved"}),
    }
}

fn seed(path: &Path, import: bool) -> anyhow::Result<()> {
    let mut store = Store::create(path)?;
    if import {
        store.import_graph(imported("NeedleOriginal"))?;
    } else {
        store.apply_native(
            "repo",
            vec![caller(), definitions()],
            vec![],
            Coverage::default(),
        )?;
    }
    Ok(())
}

fn strings(conn: &Connection, sql: &str) -> anyhow::Result<Vec<String>> {
    Ok(conn
        .prepare(sql)?
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

fn schema(conn: &Connection) -> anyhow::Result<Vec<String>> {
    strings(
        conn,
        "SELECT json_array(type,name,tbl_name,sql) FROM sqlite_master ORDER BY type,name",
    )
}

fn persisted(conn: &Connection) -> anyhow::Result<Vec<Vec<String>>> {
    let compact = matches!(
        conn.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))?,
        2 | 3
    );
    let statements = if compact {
        [
            "SELECT json_array(rowid,id,payload,search) FROM nodes ORDER BY id",
            "SELECT json_array(e.id,e.payload,r.id) FROM edges e LEFT JOIN refs r ON r.rkey=e.ref_key ORDER BY e.id",
            "SELECT json_array(r.id,r.payload,n.id,r.resolution_reason) FROM refs r LEFT JOIN nodes n ON n.nkey=r.resolved_target_key ORDER BY r.id",
            "SELECT json_array(n.id,a.binding_key) FROM node_aliases a JOIN nodes n ON n.nkey=a.node_key ORDER BY n.id,a.binding_key",
            "SELECT json_array(r.id,k.priority,k.binding_key) FROM ref_keys k JOIN refs r ON r.rkey=k.ref_key ORDER BY r.id,k.priority",
        ]
    } else {
        [
            "SELECT json_array(rowid,id,payload,search) FROM nodes ORDER BY id",
            "SELECT json_array(id,payload,ref_id) FROM edges ORDER BY id",
            "SELECT json_array(id,payload,resolved_target,resolution_reason) FROM refs ORDER BY id",
            "SELECT json_array(node_id,binding_key) FROM node_aliases ORDER BY node_id,binding_key",
            "SELECT json_array(ref_id,priority,binding_key) FROM ref_keys ORDER BY ref_id,priority",
        ]
    };
    statements
        .into_iter()
        .map(|sql| strings(conn, sql))
        .collect()
}

fn matches(conn: &Connection, term: &str) -> anyhow::Result<Vec<i64>> {
    Ok(conn
        .prepare("SELECT rowid FROM node_search WHERE node_search MATCH ? ORDER BY rowid")?
        .query_map([term], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

fn assert_packed(conn: &Connection) -> anyhow::Result<()> {
    for table in ["node_aliases", "ref_keys"] {
        assert!(conn.query_row(
            "SELECT wr FROM pragma_table_list WHERE schema='main' AND name=?1",
            [table],
            |row| row.get::<_, bool>(0),
        )?);
        // The composite PK is the table, not another allocated B-tree.
        assert_eq!(
            conn.query_row(
                "SELECT count(*) FROM sqlite_master WHERE name=?1",
                [format!("sqlite_autoindex_{table}_1")],
                |row| row.get::<_, i64>(0),
            )?,
            0
        );
    }
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM sqlite_master WHERE name IN ('node_aliases_binding','ref_keys_binding') AND type='index'",
            [],
            |row| row.get::<_, i64>(0),
        )?,
        2
    );
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM sqlite_master WHERE name='node_search_content'",
            [],
            |row| row.get::<_, i64>(0),
        )?,
        0
    );
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM node_search WHERE text IS NOT NULL",
            [],
            |row| row.get::<_, i64>(0)
        )?,
        0
    );
    assert_eq!(
        conn.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))?,
        3
    );
    assert_eq!(
        conn.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| {
            row.get::<_, i64>(0)
        })?,
        0
    );
    Ok(())
}

#[test]
fn fresh_layout_packs_only_derived_storage_and_keeps_lookup_orders() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let db = temp.path().join("fresh.db");
    // Even an empty newly created store has the optimal physical layout.
    drop(Store::create(&db)?);
    assert_packed(&Connection::open(&db)?)?;
    seed(&db, false)?;
    let conn = Connection::open(&db)?;
    assert_packed(&conn)?;
    assert_eq!(
        strings(
            &conn,
            "SELECT binding_key FROM ref_keys WHERE ref_key=(SELECT rkey FROM refs WHERE id='01-ref') ORDER BY priority"
        )?,
        ["z:key", "a:key"]
    );
    assert_eq!(
        strings(
            &conn,
            "SELECT binding_key FROM node_aliases WHERE node_key=(SELECT nkey FROM nodes WHERE id='target-z') ORDER BY binding_key"
        )?,
        ["z:key"]
    );
    for (sql, expected) in [
        (
            "EXPLAIN QUERY PLAN SELECT node_key FROM node_aliases WHERE binding_key='z:key' ORDER BY node_key",
            "node_aliases_binding",
        ),
        (
            "EXPLAIN QUERY PLAN SELECT binding_key FROM ref_keys WHERE ref_key=(SELECT rkey FROM refs WHERE id='01-ref') ORDER BY priority",
            "PRIMARY KEY",
        ),
        (
            "EXPLAIN QUERY PLAN SELECT ref_key FROM ref_keys WHERE binding_key='z:key'",
            "ref_keys_binding",
        ),
    ] {
        let plan: Vec<String> = conn
            .prepare(sql)?
            .query_map([], |r| r.get(3))?
            .collect::<rusqlite::Result<_>>()?;
        assert!(plan.iter().any(|s| s.contains(expected)), "{plan:?}");
        assert!(!plan.iter().any(|s| s.contains("TEMP B-TREE")), "{plan:?}");
    }
    let reader = Store::open_read_only(&db)?;
    let graph = reader.neighbors("caller", &QueryOptions::default())?;
    assert_eq!(
        graph
            .edges
            .iter()
            .map(|e| (e.id.as_str(), e.target.as_str()))
            .collect::<Vec<_>>(),
        [
            ("reference:01-ref", "target-z"),
            ("reference:02-ref", "target-z")
        ]
    );
    assert_eq!(reader.snapshot()?.schema_version, 1);
    Ok(())
}

#[test]
fn old_layout_is_read_only_until_explicit_write_then_migrates_once() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let db = temp.path().join("old.db");
    seed(&db, false)?;
    let conn = Connection::open(&db)?;
    storage_legacy::restore_legacy(&conn, false)?;
    let old_schema = schema(&conn)?;
    let records = persisted(&conn)?;
    let postings = matches(&conn, "Needle*")?;
    let before = serde_json::to_value(Store::open_read_only(&db)?.snapshot()?)?;
    let query = serde_json::to_value(
        Store::open_read_only(&db)?.query("Needle", &QueryOptions::default())?,
    )?;
    let extended = serde_json::to_value(
        Store::open_read_only(&db)?.query_extended("Needle", &SearchOptions::default())?,
    )?;
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")?;
    drop(conn);
    let bytes = std::fs::read(&db)?;
    for reader in [
        Store::open_read_only(&db)?,
        Store::open(&db)?,
        Store::create(&db)?,
    ] {
        assert_eq!(serde_json::to_value(reader.snapshot()?)?, before);
        assert_eq!(
            serde_json::to_value(reader.query("Needle", &QueryOptions::default())?)?,
            query
        );
    }
    assert_eq!(std::fs::read(&db)?, bytes);
    let conn = Connection::open(&db)?;
    assert_eq!(schema(&conn)?, old_schema);
    let mut writer = Store::open(&db)?;
    writer.apply_native("repo", vec![], vec![], Coverage::default())?;
    assert_packed(&conn)?;
    assert_eq!(persisted(&conn)?, records);
    assert_eq!(matches(&conn, "Needle*")?, postings);
    assert_eq!(serde_json::to_value(writer.snapshot()?)?, before);
    assert_eq!(
        serde_json::to_value(writer.query("Needle", &QueryOptions::default())?)?,
        query
    );
    assert_eq!(
        serde_json::to_value(writer.query_extended("Needle", &SearchOptions::default())?)?,
        extended
    );
    let packed_schema = schema(&conn)?;
    let cookie: i64 = conn.pragma_query_value(None, "schema_version", |row| row.get(0))?;
    writer.apply_native("repo", vec![], vec![], Coverage::default())?;
    assert_eq!(schema(&conn)?, packed_schema);
    assert_eq!(
        conn.pragma_query_value(None, "schema_version", |r| r.get::<_, i64>(0))?,
        cookie
    );
    assert_eq!(serde_json::to_value(writer.snapshot()?)?, before);
    Ok(())
}

#[test]
fn packed_aliases_preserve_ambiguity_priority_rebinding_and_cascades() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let db = temp.path().join("aliases.db");
    seed(&db, false)?;
    let conn = Connection::open(&db)?;
    storage_legacy::restore_legacy(&conn, false)?;
    let mut writer = Store::open(&db)?;
    writer.apply_native("repo", vec![], vec![], Coverage::default())?;
    let competitor = facts(
        "other.py",
        vec![node("competitor", "other.py", "Other", &["z:key"])],
        vec![],
    );
    writer.apply_native("repo", vec![competitor], vec![], Coverage::default())?;
    let ambiguous = writer.neighbors("caller", &QueryOptions::default())?;
    assert!(ambiguous.edges.is_empty());
    assert_eq!(ambiguous.unresolved.len(), 2);
    assert!(
        ambiguous
            .unresolved
            .iter()
            .all(|r| r.reason == "ambiguous binding: z:key")
    );
    writer.apply_native("repo", vec![], vec!["other.py".into()], Coverage::default())?;
    assert_eq!(
        strings(
            &conn,
            "SELECT n.id FROM refs r LEFT JOIN nodes n ON n.nkey=r.resolved_target_key ORDER BY r.id"
        )?,
        ["target-z", "target-z"]
    );
    let mut changed = definitions();
    changed.nodes[0].metadata["binding_aliases"] = json!([]);
    writer.apply_native("repo", vec![changed], vec![], Coverage::default())?;
    assert_eq!(
        strings(
            &conn,
            "SELECT n.id FROM refs r LEFT JOIN nodes n ON n.nkey=r.resolved_target_key ORDER BY r.id"
        )?,
        ["target-a", "target-a"]
    );
    writer.apply_native("repo", vec![definitions()], vec![], Coverage::default())?;
    assert_eq!(
        strings(
            &conn,
            "SELECT n.id FROM refs r LEFT JOIN nodes n ON n.nkey=r.resolved_target_key ORDER BY r.id"
        )?,
        ["target-z", "target-z"]
    );
    writer.apply_native(
        "repo",
        vec![],
        vec!["caller.py".into()],
        Coverage::default(),
    )?;
    assert_eq!(
        conn.query_row("SELECT count(*) FROM ref_keys", [], |r| r.get::<_, i64>(0))?,
        0
    );
    writer.apply_native("repo", vec![], vec!["defs.py".into()], Coverage::default())?;
    assert_eq!(
        conn.query_row("SELECT count(*) FROM node_aliases", [], |r| r
            .get::<_, i64>(0))?,
        0
    );
    assert!(matches(&conn, "Needle*")?.is_empty());
    assert_packed(&conn)?;
    Ok(())
}

#[test]
fn packing_and_search_migrations_roll_back_with_native_and_imported_graphs() -> anyhow::Result<()> {
    for import in [false, true] {
        let temp = tempfile::tempdir()?;
        let db = temp.path().join("rollback.db");
        seed(&db, import)?;
        let conn = Connection::open(&db)?;
        storage_legacy::restore_legacy(&conn, false)?;
        conn.execute_batch("CREATE TRIGGER reject_node BEFORE UPDATE OF generation ON metadata BEGIN
            SELECT CASE WHEN
                (SELECT wr FROM pragma_table_list WHERE schema='main' AND name='node_aliases')=1 AND
                (SELECT wr FROM pragma_table_list WHERE schema='main' AND name='ref_keys')=1 AND
                NOT EXISTS(SELECT 1 FROM sqlite_master WHERE name='node_search_content')
            THEN RAISE(ABORT,'after packing migration') ELSE RAISE(ABORT,'before packing migration') END;
        END;")?;
        let old_schema = schema(&conn)?;
        let records = persisted(&conn)?;
        let postings = matches(&conn, "Needle*")?;
        let cookie: i64 = conn.pragma_query_value(None, "schema_version", |r| r.get(0))?;
        let mut writer = Store::open(&db)?;
        let before = serde_json::to_value(writer.snapshot()?)?;
        let error = if import {
            writer
                .refresh_import(imported("NeedleReplacement"))
                .unwrap_err()
        } else {
            writer
                .apply_native("repo", vec![definitions()], vec![], Coverage::default())
                .unwrap_err()
        };
        assert!(
            format!("{error:#}").contains("after packing migration"),
            "{error:#}"
        );
        assert_eq!(schema(&conn)?, old_schema);
        assert_eq!(persisted(&conn)?, records);
        assert_eq!(matches(&conn, "Needle*")?, postings);
        assert_eq!(
            conn.pragma_query_value(None, "schema_version", |r| r.get::<_, i64>(0))?,
            cookie
        );
        assert_eq!(serde_json::to_value(writer.snapshot()?)?, before);
        conn.execute_batch("DROP TRIGGER reject_node")?;
        if import {
            writer.refresh_import(imported("NeedleReplacement"))?;
        } else {
            writer.apply_native("repo", vec![definitions()], vec![], Coverage::default())?;
        }
        assert_packed(&conn)?;
        assert_eq!(
            writer.stats()?.generation,
            before["generation"].as_u64().unwrap() + 1
        );
    }
    Ok(())
}

#[test]
fn stale_native_and_import_writers_cannot_start_layout_migration() -> anyhow::Result<()> {
    for import in [false, true] {
        let temp = tempfile::tempdir()?;
        let db = temp.path().join("stale.db");
        seed(&db, import)?;
        let conn = Connection::open(&db)?;
        storage_legacy::restore_legacy(&conn, false)?;
        let mut stale = Store::open(&db)?;
        // A committed generation from a legacy writer, without a packing upgrade.
        conn.execute("UPDATE metadata SET generation=generation+1", [])?;
        let before = serde_json::to_value(Store::open_read_only(&db)?.snapshot()?)?;
        let old_schema = schema(&conn)?;
        let error = if import {
            stale.refresh_import(imported("Other")).unwrap_err()
        } else {
            stale
                .apply_native("repo", vec![], vec![], Coverage::default())
                .unwrap_err()
        };
        assert!(error.downcast_ref::<StaleStore>().is_some());
        assert_eq!(schema(&conn)?, old_schema);
        assert_eq!(
            serde_json::to_value(Store::open_read_only(&db)?.snapshot()?)?,
            before
        );
    }
    Ok(())
}

#[test]
fn contentless_search_deletes_reused_rowids_and_rebuilds_projection_without_payload_loss()
-> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let db = temp.path().join("search.db");
    let mut writer = Store::create(&db)?;
    let document = |label| facts("doc.py", vec![node("stable", "doc.py", label, &[])], vec![]);
    writer.apply_native(
        "repo",
        vec![document("ObsoleteToken")],
        vec![],
        Coverage::default(),
    )?;
    let conn = Connection::open(&db)?;
    let old_rowids = matches(&conn, "ObsoleteToken")?;
    assert_eq!(old_rowids.len(), 1);
    writer.apply_native(
        "repo",
        vec![document("ﬂow controller")],
        vec![],
        Coverage::default(),
    )?;
    assert!(matches(&conn, "ObsoleteToken")?.is_empty());
    assert_eq!(matches(&conn, "flow")?, old_rowids);
    for term in ["ﬂow", "flow"] {
        assert_eq!(
            writer.query(term, &QueryOptions::default())?.nodes[0].id,
            "stable"
        );
        assert_eq!(
            writer
                .query_extended(term, &SearchOptions::default())?
                .graph
                .nodes[0]
                .id,
            "stable"
        );
    }
    // Ordinary REPLACE is supported without access to the discarded old text.
    conn.execute(
        "INSERT OR REPLACE INTO node_search(rowid,text) VALUES(?1,'replacementtoken')",
        [old_rowids[0]],
    )?;
    assert!(matches(&conn, "flow")?.is_empty());
    assert_eq!(matches(&conn, "replacementtoken")?, old_rowids);
    conn.execute_batch(
        "UPDATE nodes SET search='outdatedtoken'; DELETE FROM node_search;
        INSERT INTO node_search(rowid,text) SELECT rowid,search FROM nodes;
        UPDATE metadata SET search_version=1;",
    )?;
    assert!(matches(&conn, "replacementtoken")?.is_empty());
    let payloads = strings(&conn, "SELECT payload FROM nodes ORDER BY id")?;
    let generation = writer.stats()?.generation;
    writer.apply_native("repo", vec![], vec![], Coverage::default())?;
    assert_eq!(writer.stats()?.generation, generation + 1);
    assert!(matches(&conn, "outdatedtoken")?.is_empty());
    assert_eq!(matches(&conn, "flow")?, old_rowids);
    assert_eq!(
        strings(&conn, "SELECT payload FROM nodes ORDER BY id")?,
        payloads
    );
    assert_packed(&conn)?;
    writer.apply_native("repo", vec![], vec!["doc.py".into()], Coverage::default())?;
    assert!(matches(&conn, "flow")?.is_empty());
    assert_eq!(
        conn.query_row("SELECT count(*) FROM node_search", [], |r| r
            .get::<_, i64>(0))?,
        0
    );
    Ok(())
}

#[test]
fn imported_contentless_search_refresh_and_empty_graph_preserve_snapshot_api() -> anyhow::Result<()>
{
    let temp = tempfile::tempdir()?;
    let db = temp.path().join("import.db");
    seed(&db, true)?;
    let conn = Connection::open(&db)?;
    storage_legacy::restore_legacy(&conn, false)?;
    let mut writer = Store::open(&db)?;
    writer.refresh_import(imported("ReplacementToken"))?;
    assert_packed(&conn)?;
    assert!(matches(&conn, "NeedleOriginal")?.is_empty());
    assert_eq!(
        writer
            .query("ReplacementToken", &QueryOptions::default())?
            .nodes[0]
            .id,
        "imported"
    );
    let snapshot = writer.snapshot()?;
    assert_eq!(snapshot.schema_version, 1);
    assert_eq!(snapshot.metadata, json!({"original":"preserved"}));
    assert!(
        conn.query_row("SELECT owner_key IS NULL FROM nodes", [], |r| r
            .get::<_, bool>(0))?
    );
    writer.refresh_import(ImportedGraph {
        nodes: vec![],
        edges: vec![],
        metadata: Value::Null,
    })?;
    assert!(writer.snapshot()?.nodes.is_empty());
    assert!(matches(&conn, "ReplacementToken")?.is_empty());
    writer.refresh_import(imported("RestoredToken"))?;
    assert_eq!(matches(&conn, "RestoredToken")?.len(), 1);
    assert!(matches(&conn, "ReplacementToken")?.is_empty());
    assert_packed(&conn)?;
    // Public JSON can still be imported into a separate schema-1 store.
    let graph = writer.snapshot()?;
    let other = temp.path().join("roundtrip.db");
    let mut target = Store::create(&other)?;
    target.import_graph(ImportedGraph {
        nodes: graph.nodes.clone(),
        edges: graph.edges.clone(),
        metadata: graph.metadata.clone(),
    })?;
    assert_eq!(
        serde_json::to_value(target.snapshot()?.nodes)?,
        serde_json::to_value(graph.nodes)?
    );
    assert_eq!(target.snapshot()?.metadata, graph.metadata);
    Ok(())
}
