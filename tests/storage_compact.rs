use graf::{
    model::{Coverage, Edge, FileFacts, ImportedGraph, Node, QueryOptions, Reference},
    store::{StaleStore, Store},
};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::path::Path;

#[path = "support/storage_legacy.rs"]
mod storage_legacy;

fn node(id: &str, path: &str, keys: &[&str]) -> Node {
    Node {
        id: id.into(),
        label: format!("Needle {id}"),
        kind: "function".into(),
        file: path.into(),
        line: Some(1),
        end_line: Some(2),
        qualified_name: Some(format!("example.{id}")),
        binding_key: Some(format!("direct:{id}")),
        metadata: json!({"binding_aliases": keys, "original_id": [id, 7]}),
    }
}
fn facts(path: &str, nodes: Vec<Node>, references: Vec<Reference>) -> FileFacts {
    FileFacts {
        path: path.into(),
        hash: format!("python-v10:context:{}", "a".repeat(64)),
        module: path.into(),
        nodes,
        edges: vec![],
        references,
        diagnostics: vec![],
    }
}
fn reference(id: &str, keys: &[&str]) -> Reference {
    Reference {
        id: id.into(),
        source: "z-caller".into(),
        label: "work".into(),
        relation: "calls".into(),
        file: "caller.py".into(),
        line: 3,
        candidate_keys: keys.iter().map(|s| (*s).into()).collect(),
        reason: "not found".into(),
    }
}
fn edge(id: &str, source: &str, target: &str) -> Edge {
    Edge {
        id: id.into(),
        source: source.into(),
        target: target.into(),
        relation: "uses".into(),
        directed: true,
        file: Some("caller.py".into()),
        line: Some(4),
        confidence: "EXTRACTED".into(),
        metadata: json!({"original": [id, 9]}),
    }
}
fn provider() -> FileFacts {
    facts(
        "provider.py",
        vec![
            node("z-target", "provider.py", &["z:priority"]),
            node("a-target", "provider.py", &["a:fallback"]),
        ],
        vec![],
    )
}
fn seed(path: &Path) -> anyhow::Result<()> {
    let mut caller = facts(
        "caller.py",
        vec![
            node("z-caller", "caller.py", &[]),
            node("a-helper", "caller.py", &[]),
        ],
        vec![
            reference("z-ref", &["z:priority", "a:fallback"]),
            reference("a-ref", &["z:priority"]),
            reference("missing", &["absent"]),
        ],
    );
    caller.edges = vec![
        edge("z-direct", "z-caller", "a-target"),
        edge("a-direct", "z-caller", "z-target"),
    ];
    Store::create(path)?.apply_native(
        "root",
        vec![caller, provider()],
        vec![],
        Coverage::default(),
    )?;
    Ok(())
}
fn version(conn: &Connection) -> anyhow::Result<i64> {
    Ok(conn.pragma_query_value(None, "user_version", |row| row.get(0))?)
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
fn snapshot(path: &Path) -> anyhow::Result<Value> {
    Ok(serde_json::to_value(
        Store::open_read_only(path)?.snapshot()?,
    )?)
}
fn persisted(conn: &Connection) -> anyhow::Result<Vec<Vec<String>>> {
    let compact = version(conn)? == 2;
    let sql = if compact {
        [
            "SELECT json_array(fkey,path,hash,module,diagnostics) FROM files ORDER BY path",
            "SELECT json_array(n.nkey,n.id,n.label,n.qualified_name,n.binding_key,n.file,f.path,n.payload,n.search) FROM nodes n LEFT JOIN files f ON f.fkey=n.owner_key ORDER BY n.id",
            "SELECT json_array(r.rkey,r.id,n.id,f.path,r.relation,r.payload,t.id,r.resolution_reason) FROM refs r JOIN nodes n ON n.nkey=r.source_key JOIN files f ON f.fkey=r.owner_key LEFT JOIN nodes t ON t.nkey=r.resolved_target_key ORDER BY r.id",
            "SELECT json_array(r.id,k.priority,k.binding_key) FROM ref_keys k JOIN refs r ON r.rkey=k.ref_key ORDER BY r.id,k.priority",
            "SELECT json_array(n.id,a.binding_key) FROM node_aliases a JOIN nodes n ON n.nkey=a.node_key ORDER BY n.id,a.binding_key",
            "SELECT json_array(e.id,s.id,t.id,e.relation,e.directed,f.path,r.id,e.payload) FROM edges e JOIN nodes s ON s.nkey=e.source_key JOIN nodes t ON t.nkey=e.target_key LEFT JOIN files f ON f.fkey=e.owner_key LEFT JOIN refs r ON r.rkey=e.ref_key ORDER BY e.id",
        ]
    } else {
        [
            "SELECT json_array(rowid,path,hash,module,diagnostics) FROM files ORDER BY path",
            "SELECT json_array(rowid,id,label,qualified_name,binding_key,file,owner_file,payload,search) FROM nodes ORDER BY id",
            "SELECT json_array(rowid,id,source,owner_file,relation,payload,resolved_target,resolution_reason) FROM refs ORDER BY id",
            "SELECT json_array(ref_id,priority,binding_key) FROM ref_keys ORDER BY ref_id,priority",
            "SELECT json_array(node_id,binding_key) FROM node_aliases ORDER BY node_id,binding_key",
            "SELECT json_array(id,source,target,relation,directed,owner_file,ref_id,payload) FROM edges ORDER BY id",
        ]
    };
    sql.into_iter().map(|sql| strings(conn, sql)).collect()
}
fn postings(conn: &Connection) -> anyhow::Result<Vec<String>> {
    conn.execute_batch(
        "CREATE VIRTUAL TABLE IF NOT EXISTS temp.test_postings USING fts5vocab(main,node_search,instance)",
    )?;
    strings(
        conn,
        "SELECT json_array(term,doc,col,offset) FROM temp.test_postings ORDER BY term,doc,col,offset",
    )
}
fn assert_integrity(conn: &Connection) -> anyhow::Result<()> {
    assert_eq!(strings(conn, "PRAGMA integrity_check")?, ["ok"]);
    let violations: i64 =
        conn.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| {
            row.get(0)
        })?;
    assert_eq!(violations, 0);
    let dangling: i64 = conn.query_row("SELECT count(*) FROM refs r WHERE resolved_target_key IS NOT NULL AND NOT EXISTS(SELECT 1 FROM nodes n WHERE n.nkey=r.resolved_target_key)", [], |row| row.get(0))?;
    assert_eq!(dangling, 0);
    Ok(())
}

fn assert_reference_indices(conn: &Connection) -> anyhow::Result<()> {
    assert_eq!(
        strings(
            conn,
            "SELECT name FROM pragma_index_info('refs_source') ORDER BY seqno"
        )?,
        ["source_key"]
    );
    for (sql, index) in [
        ("SELECT rkey FROM refs WHERE source_key=?1", "refs_source"),
        (
            "SELECT payload FROM refs WHERE source_key=?1 AND resolved_target_key IS NULL ORDER BY id LIMIT 10",
            "refs_unresolved_source",
        ),
        (
            "SELECT payload FROM refs WHERE source_key=?1 AND resolved_target_key IS NULL AND relation='calls' ORDER BY id LIMIT 10",
            "refs_unresolved_relation",
        ),
    ] {
        let plan = conn
            .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))?
            .query_map([1_i64], |row| row.get::<_, String>(3))?
            .collect::<rusqlite::Result<Vec<_>>>()?
            .join("\n");
        assert!(
            plan.contains("SEARCH refs") && plan.contains(index),
            "{plan}"
        );
        assert!(!plan.contains("TEMP B-TREE"), "{plan}");
    }
    Ok(())
}

#[test]
fn reference_source_index_changes_only_in_a_successful_explicit_write() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let db = temp.path().join("source-index.db");
    seed(&db)?;
    let sql = Connection::open(&db)?;
    sql.execute_batch(
        "DROP INDEX refs_source;
         CREATE INDEX refs_source ON refs(source_key,id);",
    )?;
    let mut stale = Store::open(&db)?;
    sql.execute("UPDATE metadata SET generation=generation+1", [])?;
    let before = snapshot(&db)?;
    let rows = persisted(&sql)?;
    let old_schema = schema(&sql)?;
    let expected_neighbors = serde_json::to_value(
        Store::open_read_only(&db)?.neighbors("z-caller", &QueryOptions::default())?,
    )?;
    sql.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")?;
    let bytes = std::fs::read(&db)?;
    for reader in [
        Store::create(&db)?,
        Store::open(&db)?,
        Store::open_read_only(&db)?,
    ] {
        assert_eq!(serde_json::to_value(reader.snapshot()?)?, before);
        assert_eq!(
            serde_json::to_value(reader.neighbors("z-caller", &QueryOptions::default())?)?,
            expected_neighbors
        );
    }
    assert_eq!(std::fs::read(&db)?, bytes);
    assert!(
        stale
            .apply_native("root", vec![], vec![], Coverage::default())
            .unwrap_err()
            .is::<StaleStore>()
    );
    assert!(
        Store::open(&db)?
            .apply_native("wrong-root", vec![], vec![], Coverage::default())
            .is_err()
    );
    assert!(
        Store::open(&db)?
            .refresh_import(ImportedGraph {
                nodes: vec![],
                edges: vec![],
                metadata: Value::Null,
            })
            .is_err()
    );
    assert_eq!(schema(&sql)?, old_schema);
    assert_eq!(persisted(&sql)?, rows);

    sql.execute_batch(
        "CREATE TRIGGER reject_index_write BEFORE UPDATE OF generation ON metadata BEGIN
         SELECT CASE WHEN (SELECT count(*) FROM pragma_index_info('refs_source'))=1
           THEN RAISE(ABORT,'after reference index replacement')
           ELSE RAISE(ABORT,'before reference index replacement') END;
         END;",
    )?;
    let rollback_schema = schema(&sql)?;
    let mut writer = Store::open(&db)?;
    let error = writer
        .apply_native("root", vec![provider()], vec![], Coverage::default())
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("after reference index replacement"),
        "{error:#}"
    );
    assert_eq!(schema(&sql)?, rollback_schema);
    assert_eq!(persisted(&sql)?, rows);
    assert_eq!(snapshot(&db)?, before);
    sql.execute_batch("DROP TRIGGER reject_index_write")?;

    let report = writer.apply_native("root", vec![], vec![], Coverage::default())?;
    assert_eq!(report.generation, before["generation"].as_u64().unwrap());
    assert_eq!(version(&sql)?, 2);
    assert_reference_indices(&sql)?;
    assert_eq!(persisted(&sql)?, rows);
    assert_eq!(snapshot(&db)?, before);
    assert_eq!(
        serde_json::to_value(writer.neighbors("z-caller", &QueryOptions::default())?)?,
        expected_neighbors
    );
    let cookie: i64 = sql.pragma_query_value(None, "schema_version", |row| row.get(0))?;
    writer.apply_native("root", vec![], vec![], Coverage::default())?;
    assert_eq!(
        sql.pragma_query_value(None, "schema_version", |row| row.get::<_, i64>(0))?,
        cookie
    );
    // Cascades still remove caller-owned references and their effective keys.
    writer.apply_native(
        "root",
        vec![],
        vec!["caller.py".into()],
        Coverage::default(),
    )?;
    for table in ["refs", "ref_keys", "edges"] {
        assert_eq!(
            sql.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                row.get::<_, i64>(0)
            })?,
            0
        );
    }
    assert_eq!(
        strings(&sql, "SELECT id FROM nodes ORDER BY id")?,
        ["a-target", "z-target"]
    );
    assert_integrity(&sql)?;
    Ok(())
}

#[test]
fn new_compact_stores_keep_public_payloads_and_lexical_order() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let db = temp.path().join("compact.db");
    seed(&db)?;
    let sql = Connection::open(&db)?;
    assert_eq!(version(&sql)?, 2);
    assert_reference_indices(&sql)?;
    for (table, key) in [("files", "fkey"), ("nodes", "nkey"), ("refs", "rkey")] {
        let is_integer_pk: bool = sql.query_row(
            &format!(
                "SELECT type='INTEGER' AND pk=1 FROM pragma_table_info('{table}') WHERE name=?1"
            ),
            [key],
            |row| row.get(0),
        )?;
        assert!(is_integer_pk);
    }
    assert_eq!(
        strings(&sql, "SELECT id FROM nodes ORDER BY nkey")?,
        ["z-caller", "a-helper", "z-target", "a-target"]
    );
    assert_eq!(
        strings(
            &sql,
            "SELECT name FROM pragma_table_info('edges') WHERE name IN ('source','target','owner_file','ref_id')"
        )?,
        Vec::<String>::new()
    );
    assert_eq!(
        strings(&sql, "SELECT name FROM sqlite_master WHERE type='view'")?,
        Vec::<String>::new()
    );
    assert_integrity(&sql)?;
    let store = Store::open_read_only(&db)?;
    let graph = store.snapshot()?;
    assert_eq!(graph.schema_version, 1);
    assert_eq!(store.stats()?.schema_version, 1);
    assert_eq!(
        graph
            .nodes
            .iter()
            .map(|n| n.id.as_str())
            .collect::<Vec<_>>(),
        ["a-helper", "a-target", "z-caller", "z-target"]
    );
    assert_eq!(
        graph
            .edges
            .iter()
            .map(|e| e.id.as_str())
            .collect::<Vec<_>>(),
        ["a-direct", "reference:a-ref", "reference:z-ref", "z-direct"]
    );
    assert_eq!(
        graph.metadata["graf_unresolved_references"][0]["id"],
        "missing"
    );
    assert_eq!(
        strings(
            &sql,
            "SELECT k.binding_key FROM ref_keys k JOIN refs r ON r.rkey=k.ref_key WHERE r.id='z-ref' ORDER BY k.priority"
        )?,
        ["z:priority", "a:fallback"]
    );
    Ok(())
}

#[test]
fn real_legacy_layouts_read_without_mutation_then_upgrade_without_graph_change()
-> anyhow::Result<()> {
    for packed in [false, true] {
        let temp = tempfile::tempdir()?;
        let db = temp.path().join("legacy.db");
        seed(&db)?;
        let sql = Connection::open(&db)?;
        storage_legacy::restore_legacy(&sql, packed)?;
        // Signed rowids, including zero, are valid and define FTS seed ordering.
        sql.execute_batch("UPDATE nodes SET rowid=CASE id WHEN 'z-caller' THEN -7 WHEN 'a-helper' THEN 0 ELSE rowid END;
            DELETE FROM node_search; INSERT INTO node_search(rowid,text) SELECT rowid,search FROM nodes;")?;
        let old_schema = schema(&sql)?;
        let old_rows = persisted(&sql)?;
        let before = snapshot(&db)?;
        let old_postings = strings(
            &sql,
            "SELECT CAST(rowid AS TEXT) FROM node_search WHERE node_search MATCH 'Needle*' ORDER BY rowid",
        )?;
        let expected_query = serde_json::to_value(
            Store::open_read_only(&db)?.query("Needle", &QueryOptions::default())?,
        )?;
        let expected_topics = Store::open_read_only(&db)?.transcription_topics()?;
        sql.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")?;
        let bytes = std::fs::read(&db)?;
        for reader in [
            Store::create(&db)?,
            Store::open(&db)?,
            Store::open_read_only(&db)?,
        ] {
            assert_eq!(serde_json::to_value(reader.snapshot()?)?, before);
            reader.snapshot_bounded(100, 100, 100, 100_000)?;
            assert_eq!(reader.stats()?.schema_version, 1);
            assert_eq!(reader.transcription_topics()?, expected_topics);
            assert_eq!(
                serde_json::to_value(reader.query("Needle", &QueryOptions::default())?)?,
                expected_query
            );
        }
        assert_eq!(std::fs::read(&db)?, bytes);
        assert_eq!(schema(&sql)?, old_schema);
        assert_eq!(version(&sql)?, 1);
        #[cfg(unix)]
        let inode = {
            use std::os::unix::fs::MetadataExt;
            std::fs::metadata(&db)?.ino()
        };
        let mut writer = Store::open(&db)?;
        let generation = writer.stats()?.generation;
        let report = writer.apply_native("root", vec![], vec![], Coverage::default())?;
        assert_eq!(report.generation, generation);
        assert_eq!(version(&sql)?, 2);
        assert_reference_indices(&sql)?;
        assert_eq!(persisted(&sql)?, old_rows);
        assert_eq!(snapshot(&db)?, before);
        assert_eq!(
            strings(
                &sql,
                "SELECT CAST(rowid AS TEXT) FROM node_search WHERE node_search MATCH 'Needle*' ORDER BY rowid"
            )?,
            old_postings
        );
        assert_eq!(
            serde_json::to_value(writer.query("Needle", &QueryOptions::default())?)?,
            expected_query
        );
        assert_eq!(writer.transcription_topics()?, expected_topics);
        assert_integrity(&sql)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            assert_eq!(std::fs::metadata(&db)?.ino(), inode);
        }
        let cookie: i64 = sql.pragma_query_value(None, "schema_version", |row| row.get(0))?;
        writer.apply_native("root", vec![], vec![], Coverage::default())?;
        assert_eq!(
            sql.pragma_query_value(None, "schema_version", |row| row.get::<_, i64>(0))?,
            cookie
        );
    }
    Ok(())
}

#[test]
fn old_wal_snapshot_and_preopened_handles_survive_a_format_only_commit() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let db = temp.path().join("readers.db");
    seed(&db)?;
    let old_sql = Connection::open(&db)?;
    storage_legacy::restore_legacy(&old_sql, true)?;
    let reader = Store::open_read_only(&db)?;
    let mut preopened_writer = Store::open(&db)?;
    let before = serde_json::to_value(reader.snapshot()?)?;
    old_sql.execute_batch("BEGIN")?;
    let old_rows = persisted(&old_sql)?;
    assert_eq!(version(&old_sql)?, 1);
    Store::open(&db)?.apply_native("root", vec![], vec![], Coverage::default())?;
    assert_eq!(version(&old_sql)?, 1);
    assert_eq!(persisted(&old_sql)?, old_rows);
    assert_eq!(serde_json::to_value(reader.snapshot()?)?, before);
    old_sql.execute_batch("COMMIT")?;
    assert_eq!(version(&old_sql)?, 2);
    assert_eq!(persisted(&old_sql)?, old_rows);
    // Generation did not change, so this old handle remains a legitimate writer.
    preopened_writer.apply_native("root", vec![provider()], vec![], Coverage::default())?;
    assert_eq!(
        reader.stats()?.generation,
        before["generation"].as_u64().unwrap() + 1
    );
    assert_integrity(&old_sql)?;
    Ok(())
}

#[test]
fn alias_backfill_and_search_projection_changes_rebind_and_publish_once() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let db = temp.path().join("backfill.db");
    seed(&db)?;
    let sql = Connection::open(&db)?;
    storage_legacy::restore_legacy(&sql, false)?;
    sql.execute_batch(
        "DROP TABLE node_aliases;
        DELETE FROM edges WHERE ref_id IS NOT NULL;
        UPDATE refs SET resolved_target=NULL,resolution_reason='not found';
        UPDATE nodes SET search='Ｆｌｏｗ';
        DELETE FROM node_search; INSERT INTO node_search(rowid,text) SELECT rowid,search FROM nodes;
        UPDATE metadata SET search_version=1;",
    )?;
    let mut writer = Store::open(&db)?;
    let old_generation = writer.stats()?.generation;
    assert_eq!(writer.stats()?.unresolved_references, 3);
    let report = writer.apply_native("root", vec![], vec![], Coverage::default())?;
    assert_eq!(report.generation, old_generation + 1);
    assert_eq!(writer.stats()?.unresolved_references, 1);
    assert_eq!(
        strings(
            &sql,
            "SELECT n.id FROM refs r JOIN nodes n ON n.nkey=r.resolved_target_key ORDER BY r.id"
        )?,
        ["z-target", "z-target"]
    );
    assert_eq!(
        strings(
            &sql,
            "SELECT CAST(rowid AS TEXT) FROM node_search WHERE node_search MATCH 'Flow*'"
        )?,
        Vec::<String>::new()
    );
    assert!(
        !writer
            .query("Needle", &QueryOptions::default())?
            .nodes
            .is_empty()
    );
    assert_eq!(
        writer
            .apply_native("root", vec![], vec![], Coverage::default())?
            .generation,
        report.generation
    );
    assert_integrity(&sql)?;
    Ok(())
}

#[test]
fn replacement_key_reuse_never_retargets_unchanged_references_or_direct_edges() -> anyhow::Result<()>
{
    let temp = tempfile::tempdir()?;
    let db = temp.path().join("reuse.db");
    seed(&db)?;
    let sql = Connection::open(&db)?;
    let highest: i64 = sql.query_row("SELECT nkey FROM nodes WHERE id='a-target'", [], |row| {
        row.get(0)
    })?;
    let mut writer = Store::open(&db)?;
    let replacement = facts(
        "provider.py",
        vec![
            node("other-z", "provider.py", &[]),
            node("other-a", "provider.py", &[]),
        ],
        vec![],
    );
    writer.apply_native("root", vec![replacement], vec![], Coverage::default())?;
    assert_eq!(
        sql.query_row("SELECT nkey FROM nodes WHERE id='other-a'", [], |row| row
            .get::<_, i64>(
            0
        ))?,
        highest
    );
    assert_eq!(writer.stats()?.unresolved_references, 3);
    assert!(writer.snapshot()?.edges.is_empty());
    assert_integrity(&sql)?;
    writer.apply_native("root", vec![provider()], vec![], Coverage::default())?;
    assert_eq!(writer.stats()?.unresolved_references, 1);
    assert_eq!(
        writer
            .snapshot()?
            .edges
            .iter()
            .map(|e| e.target.as_str())
            .collect::<Vec<_>>(),
        ["z-target", "z-target"]
    );
    // Incoming direct assertions survive same-public-ID target replacement.
    let mut caller = facts(
        "caller.py",
        vec![node("z-caller", "caller.py", &[])],
        vec![reference("a-ref", &["z:priority"])],
    );
    caller.edges.push(edge("direct", "z-caller", "z-target"));
    writer.apply_native("root", vec![caller], vec![], Coverage::default())?;
    let mut swapped = provider();
    swapped.nodes.reverse();
    writer.apply_native("root", vec![swapped], vec![], Coverage::default())?;
    assert_eq!(
        writer
            .snapshot()?
            .edges
            .iter()
            .map(|e| (e.id.as_str(), e.target.as_str()))
            .collect::<Vec<_>>(),
        [("direct", "z-target"), ("reference:a-ref", "z-target")]
    );
    assert_integrity(&sql)?;
    Ok(())
}

#[test]
fn failed_copy_and_late_write_restore_schema_graph_postings_and_version() -> anyhow::Result<()> {
    for stage in ["copy", "late"] {
        let temp = tempfile::tempdir()?;
        let db = temp.path().join("rollback.db");
        seed(&db)?;
        let sql = Connection::open(&db)?;
        storage_legacy::restore_legacy(&sql, false)?;
        if stage == "copy" {
            // Legacy databases can have been edited by foreign_keys=OFF clients.
            sql.pragma_update(None, "foreign_keys", false)?;
            sql.execute(
                "UPDATE refs SET source='missing mandatory node' WHERE id='a-ref'",
                [],
            )?;
            sql.pragma_update(None, "foreign_keys", true)?;
        } else {
            // Metadata survives table replacement; a trigger on old nodes would not.
            sql.execute_batch(
                "CREATE TRIGGER reject_commit BEFORE UPDATE OF generation ON metadata BEGIN
                SELECT CASE WHEN (SELECT user_version FROM pragma_user_version)=2
                  AND EXISTS(SELECT 1 FROM pragma_table_info('nodes') WHERE name='nkey')
                  AND NOT EXISTS(SELECT 1 FROM sqlite_master WHERE name='node_search_content')
                  AND EXISTS(SELECT 1 FROM sqlite_master WHERE name='node_search')
                THEN RAISE(ABORT,'after compact replacement') ELSE RAISE(ABORT,'too early') END;
            END;",
            )?;
        }
        let before = snapshot(&db)?;
        let old_schema = schema(&sql)?;
        let old_rows = persisted(&sql)?;
        let postings = strings(
            &sql,
            "SELECT CAST(rowid AS TEXT) FROM node_search WHERE node_search MATCH 'Needle*' ORDER BY rowid",
        )?;
        let cookie: i64 = sql.pragma_query_value(None, "schema_version", |row| row.get(0))?;
        let mut writer = Store::open(&db)?;
        let error = writer
            .apply_native("root", vec![provider()], vec![], Coverage::default())
            .unwrap_err();
        if stage == "late" {
            assert!(
                format!("{error:#}").contains("after compact replacement"),
                "{error:#}"
            );
        }
        assert_eq!(version(&sql)?, 1);
        assert_eq!(schema(&sql)?, old_schema);
        assert_eq!(persisted(&sql)?, old_rows);
        assert_eq!(snapshot(&db)?, before);
        assert_eq!(
            strings(
                &sql,
                "SELECT CAST(rowid AS TEXT) FROM node_search WHERE node_search MATCH 'Needle*' ORDER BY rowid"
            )?,
            postings
        );
        assert_eq!(
            sql.pragma_query_value(None, "schema_version", |row| row.get::<_, i64>(0))?,
            cookie
        );
        if stage == "copy" {
            sql.execute("UPDATE refs SET source='z-caller' WHERE id='a-ref'", [])?;
        } else {
            sql.execute_batch("DROP TRIGGER reject_commit")?;
        }
        writer.apply_native("root", vec![provider()], vec![], Coverage::default())?;
        assert_eq!(version(&sql)?, 2);
        assert_integrity(&sql)?;
    }
    Ok(())
}

#[test]
fn missing_optional_identity_is_not_silently_migrated_to_null() -> anyhow::Result<()> {
    for corrupt in [
        "UPDATE nodes SET owner_file='absent' WHERE id='z-target'",
        "UPDATE refs SET resolved_target='absent' WHERE id='a-ref'",
        "UPDATE edges SET owner_file='absent' WHERE id='a-direct'",
        "UPDATE edges SET ref_id='absent' WHERE id='a-direct'",
    ] {
        let temp = tempfile::tempdir()?;
        let db = temp.path().join("missing.db");
        seed(&db)?;
        let sql = Connection::open(&db)?;
        storage_legacy::restore_legacy(&sql, true)?;
        sql.pragma_update(None, "foreign_keys", false)?;
        sql.execute(corrupt, [])?;
        sql.pragma_update(None, "foreign_keys", true)?;
        let old_rows = persisted(&sql)?;
        let old_schema = schema(&sql)?;
        let error = Store::open(&db)?
            .apply_native("root", vec![], vec![], Coverage::default())
            .unwrap_err();
        assert!(
            format!("{error:#}").contains("cannot map an existing identity"),
            "{error:#}"
        );
        assert_eq!(version(&sql)?, 1);
        assert_eq!(schema(&sql)?, old_schema);
        assert_eq!(persisted(&sql)?, old_rows);
    }
    Ok(())
}

#[test]
fn stale_wrong_kind_and_wrong_root_writes_leave_legacy_layout_untouched() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let db = temp.path().join("baseline.db");
    seed(&db)?;
    let sql = Connection::open(&db)?;
    storage_legacy::restore_legacy(&sql, true)?;
    let mut stale = Store::open(&db)?;
    sql.execute("UPDATE metadata SET generation=generation+1", [])?;
    let old_schema = schema(&sql)?;
    let old_rows = persisted(&sql)?;
    assert!(
        stale
            .apply_native("root", vec![], vec![], Coverage::default())
            .unwrap_err()
            .is::<StaleStore>()
    );
    assert!(
        Store::open(&db)?
            .apply_native("other", vec![], vec![], Coverage::default())
            .is_err()
    );
    assert!(
        Store::open(&db)?
            .refresh_import(ImportedGraph {
                nodes: vec![],
                edges: vec![],
                metadata: Value::Null
            })
            .is_err()
    );
    assert_eq!(version(&sql)?, 1);
    assert_eq!(schema(&sql)?, old_schema);
    assert_eq!(persisted(&sql)?, old_rows);
    Ok(())
}

#[test]
fn imported_null_ownership_and_metadata_references_are_not_fabricated_links() -> anyhow::Result<()>
{
    let temp = tempfile::tempdir()?;
    let db = temp.path().join("import.db");
    let imported = || {
        let mut relation = edge("edge", "z", "a");
        relation.metadata = json!({"reference_id": "not a native reference"});
        ImportedGraph {
            nodes: vec![
                node("z", "not-on-disk.py", &["alias"]),
                node("a", "other.py", &[]),
            ],
            edges: vec![relation],
            metadata: json!({"source":"saved"}),
        }
    };
    Store::create(&db)?.import_graph(imported())?;
    let sql = Connection::open(&db)?;
    storage_legacy::restore_legacy(&sql, false)?;
    let before = snapshot(&db)?;
    let mut writer = Store::open(&db)?;
    assert!(
        writer
            .apply_native("root", vec![], vec![], Coverage::default())
            .is_err()
    );
    assert_eq!(version(&sql)?, 1);
    writer.refresh_import(imported())?;
    assert_eq!(version(&sql)?, 2);
    assert_eq!(
        strings(&sql, "SELECT CAST(count(*) AS TEXT) FROM files")?,
        ["0"]
    );
    assert_eq!(
        strings(
            &sql,
            "SELECT CAST(count(*) AS TEXT) FROM nodes WHERE owner_key IS NOT NULL"
        )?,
        ["0"]
    );
    assert_eq!(
        strings(
            &sql,
            "SELECT CAST(count(*) AS TEXT) FROM edges WHERE owner_key IS NOT NULL OR ref_key IS NOT NULL"
        )?,
        ["0"]
    );
    assert_eq!(
        strings(&sql, "SELECT CAST(count(*) AS TEXT) FROM node_aliases")?,
        ["0"]
    );
    let after = snapshot(&db)?;
    for key in ["nodes", "edges", "metadata", "schema_version"] {
        assert_eq!(after[key], before[key]);
    }
    assert_integrity(&sql)?;
    sql.execute_batch(
        "DROP INDEX refs_source;
         CREATE INDEX refs_source ON refs(source_key,id);",
    )?;
    writer.refresh_import(ImportedGraph {
        nodes: vec![],
        edges: vec![],
        metadata: Value::Null,
    })?;
    assert_reference_indices(&sql)?;
    assert_eq!(writer.stats()?.nodes, 0);
    assert_eq!(
        strings(&sql, "SELECT CAST(count(*) AS TEXT) FROM node_search")?,
        ["0"]
    );
    writer.refresh_import(imported())?;
    let before_compact = snapshot(&db)?;
    let before_rows = persisted(&sql)?;
    writer.compact()?;
    assert_eq!(snapshot(&db)?, before_compact);
    assert_eq!(persisted(&sql)?, before_rows);
    assert_integrity(&sql)?;
    Ok(())
}

#[test]
fn independent_previous_writer_fixture_upgrades_with_exact_records_and_no_generation_change()
-> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let db = temp.path().join("previous-writer.db");
    let sql = Connection::open(&db)?;
    sql.execute_batch(include_str!("fixtures/native-format1.sql"))?;
    sql.pragma_update(None, "journal_mode", "WAL")?;
    let before = snapshot(&db)?;
    let old_rows = persisted(&sql)?;
    let old_schema = schema(&sql)?;
    let coverage: Coverage =
        serde_json::from_str(&sql.query_row("SELECT coverage FROM metadata", [], |row| {
            row.get::<_, String>(0)
        })?)?;
    let old_reader = Store::open_read_only(&db)?;
    let query = serde_json::to_value(old_reader.query("caller", &QueryOptions::default())?)?;
    let neighbors =
        serde_json::to_value(old_reader.neighbors("caller", &QueryOptions::default())?)?;
    let stats = old_reader.stats()?;
    assert_eq!(
        (
            stats.files,
            stats.nodes,
            stats.edges,
            stats.unresolved_references
        ),
        (2, 5, 6, 1)
    );
    assert_eq!(
        before["metadata"]["graf_unresolved_references"][0]["label"],
        "missing"
    );
    assert_eq!(before["schema_version"], 1);
    sql.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")?;
    let bytes = std::fs::read(&db)?;
    for store in [
        Store::create(&db)?,
        Store::open(&db)?,
        Store::open_read_only(&db)?,
    ] {
        assert_eq!(serde_json::to_value(store.snapshot()?)?, before);
        assert_eq!(
            serde_json::to_value(store.query("caller", &QueryOptions::default())?)?,
            query
        );
        assert_eq!(
            serde_json::to_value(store.neighbors("caller", &QueryOptions::default())?)?,
            neighbors
        );
    }
    assert_eq!(std::fs::read(&db)?, bytes);
    assert_eq!(schema(&sql)?, old_schema);
    assert_eq!(version(&sql)?, 1);
    let report = Store::open(&db)?.apply_native("fixture", vec![], vec![], coverage)?;
    assert_eq!(report.generation, stats.generation);
    assert_eq!(version(&sql)?, 2);
    assert_reference_indices(&sql)?;
    assert_eq!(persisted(&sql)?, old_rows);
    assert_eq!(serde_json::to_value(old_reader.snapshot()?)?, before);
    assert_eq!(
        serde_json::to_value(old_reader.query("caller", &QueryOptions::default())?)?,
        query
    );
    assert_eq!(
        serde_json::to_value(old_reader.neighbors("caller", &QueryOptions::default())?)?,
        neighbors
    );
    assert_integrity(&sql)?;
    Ok(())
}

#[test]
fn ruby_effective_candidates_survive_upgrade_and_rebind_unchanged_callers() -> anyhow::Result<()> {
    let parse = |path, source| {
        graf::languages::parse(path, source, "fixture")
            .unwrap()
            .unwrap()
    };
    let base: FileFacts = parse("base.rb", "class Base\n def work; :base; end\nend");
    let child: FileFacts = parse("child.rb", "class Child < Base; end");
    let app: FileFacts = parse("app.rb", "def main\n worker = Child.new\n worker.work\nend");
    let target = base
        .nodes
        .iter()
        .find(|n| n.label == "work")
        .unwrap()
        .id
        .clone();
    let caller = app
        .nodes
        .iter()
        .find(|n| n.label == "main")
        .unwrap()
        .id
        .clone();
    let temp = tempfile::tempdir()?;
    let db = temp.path().join("ruby.db");
    let mut store = Store::create(&db)?;
    store.apply_native(
        "fixture",
        vec![base.clone(), child.clone(), app],
        vec![],
        Coverage::default(),
    )?;
    let sql = Connection::open(&db)?;
    storage_legacy::restore_legacy(&sql, true)?;
    let before = persisted(&sql)?;
    let generation = store.stats()?.generation;
    store.apply_native("fixture", vec![], vec![], Coverage::default())?;
    assert_eq!(persisted(&sql)?, before);
    assert_eq!(store.stats()?.generation, generation);
    sql.execute_batch(
        "DROP INDEX refs_source;
         CREATE INDEX refs_source ON refs(source_key,id);",
    )?;
    store.apply_native("fixture", vec![], vec![], Coverage::default())?;
    assert_reference_indices(&sql)?;
    store.compact()?;
    assert_eq!(persisted(&sql)?, before);
    assert_eq!(store.stats()?.generation, generation);
    let has_call = |store: &Store, target: &str| -> anyhow::Result<bool> {
        Ok(store
            .snapshot()?
            .edges
            .iter()
            .any(|e| e.source == caller && e.target == target && e.relation == "calls"))
    };
    assert!(has_call(&store, &target)?);
    let overriding: FileFacts = parse("child.rb", "class Child < Base\n attr_reader :work\nend");
    let overridden = overriding
        .nodes
        .iter()
        .find(|n| n.label == "work")
        .unwrap()
        .id
        .clone();
    store.apply_native("fixture", vec![overriding], vec![], Coverage::default())?;
    assert!(has_call(&store, &overridden)?);
    assert!(!has_call(&store, &target)?);
    store.apply_native("fixture", vec![child], vec![], Coverage::default())?;
    assert!(has_call(&store, &target)?);
    let barrier: FileFacts = parse("patch.rb", "Base.class_eval { attr_reader :work }");
    store.apply_native("fixture", vec![barrier], vec![], Coverage::default())?;
    assert!(!has_call(&store, &target)?);
    store.apply_native(
        "fixture",
        vec![],
        vec!["patch.rb".into()],
        Coverage::default(),
    )?;
    assert!(has_call(&store, &target)?);
    store.apply_native(
        "fixture",
        vec![],
        vec!["base.rb".into()],
        Coverage::default(),
    )?;
    assert!(!has_call(&store, &target)?);
    assert_integrity(&sql)?;
    Ok(())
}

#[test]
fn unsupported_physical_version_and_mismatched_layout_fail_without_repair() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let db = temp.path().join("unsupported.db");
    seed(&db)?;
    let sql = Connection::open(&db)?;
    for bad in [3, 1] {
        sql.pragma_update(None, "user_version", bad)?;
        let before = schema(&sql)?;
        assert!(Store::open(&db).is_err());
        assert!(Store::open_read_only(&db).is_err());
        assert!(Store::create(&db).is_err());
        assert_eq!(version(&sql)?, bad);
        assert_eq!(schema(&sql)?, before);
    }
    sql.pragma_update(None, "user_version", 2)?;
    assert_eq!(Store::open_read_only(&db)?.snapshot()?.schema_version, 1);
    Ok(())
}

#[test]
fn vacuum_refuses_legacy_then_preserves_signed_keys_payloads_and_postings() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let db = temp.path().join("vacuum.db");
    seed(&db)?;
    let sql = Connection::open(&db)?;
    storage_legacy::restore_legacy(&sql, false)?;
    sql.execute_batch(
        "UPDATE nodes SET rowid=CASE id WHEN 'z-caller' THEN -7 WHEN 'a-helper' THEN 0 ELSE rowid+100 END;
         UPDATE files SET rowid=rowid+100;
         UPDATE refs SET rowid=rowid+100;
         DELETE FROM node_search; INSERT INTO node_search(rowid,text) SELECT rowid,search FROM nodes;
         PRAGMA wal_checkpoint(TRUNCATE);",
    )?;
    let before = snapshot(&db)?;
    let rows = persisted(&sql)?;
    let fts = postings(&sql)?;
    let old_schema = schema(&sql)?;
    let bytes = std::fs::read(&db)?;
    let mut store = Store::open(&db)?;
    let error = store.compact().unwrap_err();
    assert!(format!("{error:#}").contains("storage format 1"));
    assert!(format!("{error:#}").contains("update or import refresh"));
    assert_eq!(std::fs::read(&db)?, bytes);
    assert_eq!(schema(&sql)?, old_schema);
    assert_eq!(snapshot(&db)?, before);
    assert_eq!(persisted(&sql)?, rows);
    assert_eq!(postings(&sql)?, fts);

    store.apply_native("root", vec![], vec![], Coverage::default())?;
    assert_eq!(persisted(&sql)?, rows);
    let compact_schema = schema(&sql)?;
    sql.execute_batch(
        "CREATE TABLE discarded(data BLOB);
         INSERT INTO discarded VALUES(zeroblob(262144));
         DROP TABLE discarded;
         PRAGMA wal_checkpoint(TRUNCATE);",
    )?;
    let bytes = std::fs::read(&db)?;
    let error = Store::open_read_only(&db)?.compact().unwrap_err();
    assert!(format!("{error:#}").contains("read-only"));
    assert_eq!(std::fs::read(&db)?, bytes);
    #[cfg(unix)]
    let inode = {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(&db)?.ino()
    };
    let mut writer = Store::open(&db)?;
    let report = store.compact()?;
    assert_eq!(report.schema_version, 1);
    assert!(report.free_pages_before > 0);
    assert_eq!(report.free_pages_after, 0);
    assert!(report.pages_after < report.pages_before);
    assert!(!report.checkpoint_busy);
    assert_eq!(
        std::fs::metadata(&db)?.len(),
        report.pages_after * report.page_size
    );
    assert_eq!(schema(&sql)?, compact_schema);
    assert_eq!(version(&sql)?, 2);
    assert_eq!(snapshot(&db)?, before);
    assert_eq!(persisted(&sql)?, rows);
    assert_eq!(postings(&sql)?, fts);
    assert_reference_indices(&sql)?;
    assert_integrity(&sql)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        assert_eq!(std::fs::metadata(&db)?.ino(), inode);
    }
    // Physical-only compaction leaves a legitimate writer's baseline usable.
    writer.apply_native("root", vec![provider()], vec![], Coverage::default())?;
    assert_eq!(
        writer.stats()?.generation,
        before["generation"].as_u64().unwrap() + 1
    );
    Ok(())
}

#[test]
fn stale_maintenance_preserves_newer_facts_without_refreshing_its_write_baseline()
-> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let db = temp.path().join("stale-compact.db");
    seed(&db)?;
    let mut stale = Store::open(&db)?;
    let mut writer = Store::open(&db)?;
    writer.apply_native(
        "root",
        vec![],
        vec!["provider.py".into()],
        Coverage::default(),
    )?;
    let sql = Connection::open(&db)?;
    let current = snapshot(&db)?;
    let current_rows = persisted(&sql)?;
    let fts = postings(&sql)?;
    stale.compact()?;
    assert_eq!(snapshot(&db)?, current);
    assert_eq!(persisted(&sql)?, current_rows);
    assert_eq!(postings(&sql)?, fts);
    assert!(
        stale
            .apply_native("root", vec![provider()], vec![], Coverage::default())
            .unwrap_err()
            .is::<StaleStore>()
    );
    assert_eq!(snapshot(&db)?, current);
    writer.apply_native("root", vec![provider()], vec![], Coverage::default())?;
    assert_eq!(writer.stats()?.unresolved_references, 1);
    assert_integrity(&sql)?;
    Ok(())
}

#[test]
fn vacuum_keeps_rare_unresolved_streams_bounded_and_ordered() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let db = temp.path().join("rare-compact.db");
    let mut references: Vec<_> = (0..5_001)
        .rev()
        .map(|i| reference(&format!("resolved-{i:05}"), &["z:priority"]))
        .collect();
    for id in ["zz-unresolved", "za-unresolved"] {
        let mut unresolved = reference(id, &["absent"]);
        unresolved.label = id.into();
        unresolved.relation = "rare".into();
        references.push(unresolved);
    }
    let mut store = Store::create(&db)?;
    store.apply_native(
        "root",
        vec![
            facts(
                "caller.py",
                vec![node("z-caller", "caller.py", &[])],
                references,
            ),
            provider(),
        ],
        vec![],
        Coverage::default(),
    )?;
    let options = QueryOptions {
        relation: Some("rare".into()),
        ..Default::default()
    };
    let before = store.neighbors("z-caller", &options)?;
    assert!(!before.truncated);
    assert!(before.edges.is_empty());
    assert_eq!(
        before
            .unresolved
            .iter()
            .map(|r| r.label.as_str())
            .collect::<Vec<_>>(),
        ["za-unresolved", "zz-unresolved"]
    );
    store.compact()?;
    assert_eq!(
        serde_json::to_value(store.neighbors("z-caller", &options)?)?,
        serde_json::to_value(before)?
    );
    let unfiltered = store.neighbors("z-caller", &QueryOptions::default())?;
    assert!(unfiltered.truncated);
    assert_eq!(unfiltered.edges.len(), 5_000);
    let sql = Connection::open(&db)?;
    assert_reference_indices(&sql)?;
    assert_integrity(&sql)?;
    Ok(())
}
