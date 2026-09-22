use graf::{
    model::{Coverage, Edge, FileFacts, ImportedGraph, Node, QueryOptions, Reference},
    query::SearchOptions,
    store::{StaleStore, Store},
};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::path::Path;

#[path = "support/storage_legacy.rs"]
mod storage_legacy;

fn node(id: &str, path: &str, aliases: &[&str]) -> Node {
    Node {
        id: id.into(),
        label: format!("Needle {id}"),
        kind: "function".into(),
        file: path.into(),
        line: Some(1),
        end_line: Some(2),
        qualified_name: Some(format!("example.{id}")),
        binding_key: Some(format!("direct:{id}")),
        metadata: json!({"binding_aliases": aliases, "evidence": "original full payload"}),
    }
}
fn facts(path: &str, nodes: Vec<Node>, references: Vec<Reference>) -> FileFacts {
    FileFacts {
        path: path.into(),
        hash: format!("stamp:{path}"),
        module: path.into(),
        nodes,
        references,
        edges: vec![],
        diagnostics: vec![],
    }
}
fn reference(id: &str, keys: &[&str]) -> Reference {
    Reference {
        id: id.into(),
        source: "caller".into(),
        label: id.into(),
        relation: "calls".into(),
        file: "caller.py".into(),
        line: 4,
        candidate_keys: keys.iter().map(|s| (*s).into()).collect(),
        reason: "original unresolved evidence".into(),
    }
}
fn caller() -> FileFacts {
    let mut facts = facts(
        "caller.py",
        vec![node("caller", "caller.py", &[])],
        vec![
            reference("z-ref", &["public:work"]),
            reference("a-ref", &["absent"]),
        ],
    );
    facts.edges.push(Edge {
        id: "static-edge".into(),
        source: "caller".into(),
        target: "target".into(),
        relation: "uses".into(),
        directed: false,
        file: Some("caller.py".into()),
        line: Some(7),
        confidence: "EXTRACTED".into(),
        metadata: json!({"evidence": [1, "preserved"]}),
    });
    facts
}
fn provider(id: &str) -> FileFacts {
    facts(
        "provider.py",
        vec![node(id, "provider.py", &["public:work"])],
        vec![],
    )
}
fn seed(path: &Path) -> anyhow::Result<()> {
    Store::create(path)?.apply_native(
        "fixture",
        vec![caller(), provider("target")],
        vec![],
        Coverage::default(),
    )?;
    Ok(())
}
fn version(conn: &Connection) -> anyhow::Result<i64> {
    Ok(conn.pragma_query_value(None, "user_version", |r| r.get(0))?)
}
fn strings(conn: &Connection, sql: &str) -> anyhow::Result<Vec<String>> {
    Ok(conn
        .prepare(sql)?
        .query_map([], |r| r.get(0))?
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
fn coverage(conn: &Connection) -> anyhow::Result<Coverage> {
    Ok(serde_json::from_str(&conn.query_row(
        "SELECT coverage FROM metadata",
        [],
        |r| r.get::<_, String>(0),
    )?)?)
}
fn postings(conn: &Connection) -> anyhow::Result<Vec<String>> {
    conn.execute_batch("CREATE VIRTUAL TABLE IF NOT EXISTS temp.postings USING fts5vocab(main,node_search,instance)")?;
    strings(
        conn,
        "SELECT json_array(term,doc,col,offset) FROM temp.postings ORDER BY term,doc,col,offset",
    )
}
fn search_storage(conn: &Connection) -> anyhow::Result<Vec<Vec<Vec<rusqlite::types::Value>>>> {
    [
        "SELECT * FROM node_search_data ORDER BY id",
        "SELECT * FROM node_search_idx ORDER BY segid,term",
        "SELECT * FROM node_search_docsize ORDER BY id",
        "SELECT * FROM node_search_config ORDER BY k",
    ]
    .into_iter()
    .map(|sql| {
        let mut stmt = conn.prepare(sql)?;
        let columns = stmt.column_count();
        Ok(stmt
            .query_map([], |row| (0..columns).map(|i| row.get(i)).collect())?
            .collect::<rusqlite::Result<_>>()?)
    })
    .collect()
}
fn records(conn: &Connection) -> anyhow::Result<Vec<Vec<String>>> {
    let sql = if version(conn)? == 1 {
        [
            "SELECT json_array(rowid,path,hash,module,diagnostics) FROM files ORDER BY path",
            "SELECT json_array(rowid,id,label,qualified_name,binding_key,file,owner_file,payload,search) FROM nodes ORDER BY id",
            "SELECT json_array(rowid,id,source,owner_file,relation,payload,resolved_target,resolution_reason) FROM refs ORDER BY id",
            "SELECT json_array(ref_id,priority,binding_key) FROM ref_keys ORDER BY ref_id,priority",
            "SELECT json_array(node_id,binding_key) FROM node_aliases ORDER BY node_id,binding_key",
            "SELECT json_array(id,source,target,relation,directed,owner_file,ref_id,payload) FROM edges ORDER BY id",
        ]
    } else {
        [
            "SELECT json_array(fkey,path,hash,module,diagnostics) FROM files ORDER BY path",
            "SELECT json_array(n.nkey,n.id,n.label,n.qualified_name,n.binding_key,n.file,f.path,printf('%s',n.payload),n.search) FROM nodes n LEFT JOIN files f ON f.fkey=n.owner_key ORDER BY n.id",
            "SELECT json_array(r.rkey,r.id,n.id,f.path,r.relation,printf('%s',r.payload),t.id,r.resolution_reason) FROM refs r JOIN nodes n ON n.nkey=r.source_key JOIN files f ON f.fkey=r.owner_key LEFT JOIN nodes t ON t.nkey=r.resolved_target_key ORDER BY r.id",
            "SELECT json_array(r.id,k.priority,k.binding_key) FROM ref_keys k JOIN refs r ON r.rkey=k.ref_key ORDER BY r.id,k.priority",
            "SELECT json_array(n.id,a.binding_key) FROM node_aliases a JOIN nodes n ON n.nkey=a.node_key ORDER BY n.id,a.binding_key",
            "SELECT json_array(e.id,s.id,t.id,e.relation,e.directed,f.path,r.id,printf('%s',e.payload)) FROM edges e JOIN nodes s ON s.nkey=e.source_key JOIN nodes t ON t.nkey=e.target_key LEFT JOIN files f ON f.fkey=e.owner_key LEFT JOIN refs r ON r.rkey=e.ref_key ORDER BY e.id",
        ]
    };
    let mut result: Vec<_> = sql
        .into_iter()
        .map(|s| strings(conn, s))
        .collect::<anyhow::Result<_>>()?;
    result.push(strings(conn, "SELECT json_array(generation,kind,root,coverage,graph_metadata,search_version) FROM metadata")?);
    Ok(result)
}
fn integrity(conn: &Connection) -> anyhow::Result<()> {
    assert_eq!(strings(conn, "PRAGMA integrity_check")?, ["ok"]);
    assert_eq!(
        conn.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| r
            .get::<_, i64>(
            0
        ))?,
        0
    );
    Ok(())
}
fn projected(conn: &Connection) -> anyhow::Result<()> {
    assert_eq!(version(conn)?, 5);
    assert_eq!(
        conn.query_row(
            "SELECT hidden FROM pragma_table_xinfo('refs') WHERE name='id'",
            [],
            |r| r.get::<_, i64>(0)
        )?,
        0
    );
    assert_eq!(
        strings(
            conn,
            "SELECT type FROM pragma_table_xinfo('refs') WHERE name='id'"
        )?,
        ["TEXT"]
    );
    assert_eq!(conn.query_row("SELECT count(*) FROM refs WHERE typeof(id)!='text' OR id IS NOT json_extract(payload,'$.id')", [], |r| r.get::<_, i64>(0))?, 0);
    integrity(conn)
}

// Reconstruct the preceding stored-ID table with its original column contract.
// Its child schemas/indexes are unchanged between formats; retain their SQL.
// This is a structural format-2 fixture, not an old-binary execution claim.
fn restore_format2(conn: &Connection) -> anyhow::Result<()> {
    conn.pragma_update(None, "foreign_keys", true)?;
    let tx = conn.unchecked_transaction()?;
    let indexes = strings(
        &tx,
        "SELECT sql FROM sqlite_master WHERE type='index' AND tbl_name IN ('refs','ref_keys','edges') AND sql IS NOT NULL ORDER BY name",
    )?;
    tx.execute_batch(
        "CREATE TEMP TABLE saved_refs AS
            SELECT rkey,id,source_key,owner_key,relation,printf('%s',payload),resolved_target_key,resolution_reason FROM refs;
        CREATE TEMP TABLE saved_keys AS SELECT * FROM ref_keys;
        CREATE TEMP TABLE saved_edges AS
            SELECT rowid AS saved_rowid,id,source_key,target_key,relation,directed,owner_key,ref_key,printf('%s',payload) FROM edges;
        DROP TABLE edges; DROP TABLE ref_keys; DROP TABLE refs;
        CREATE TABLE refs (
            rkey INTEGER PRIMARY KEY, id TEXT NOT NULL UNIQUE,
            source_key INTEGER NOT NULL REFERENCES nodes(nkey) ON DELETE CASCADE,
            owner_key INTEGER NOT NULL REFERENCES files(fkey) ON DELETE CASCADE,
            relation TEXT NOT NULL, payload TEXT NOT NULL,
            resolved_target_key INTEGER, resolution_reason TEXT NOT NULL
        );
        CREATE TABLE ref_keys (
            ref_key INTEGER NOT NULL REFERENCES refs(rkey) ON DELETE CASCADE,
            priority INTEGER NOT NULL, binding_key TEXT NOT NULL,
            PRIMARY KEY(ref_key, priority)
        ) WITHOUT ROWID;
        CREATE TABLE edges (
            id TEXT PRIMARY KEY,
            source_key INTEGER NOT NULL REFERENCES nodes(nkey) ON DELETE CASCADE,
            target_key INTEGER NOT NULL REFERENCES nodes(nkey) ON DELETE CASCADE,
            relation TEXT NOT NULL, directed INTEGER NOT NULL CHECK(directed IN (0, 1)),
            owner_key INTEGER REFERENCES files(fkey) ON DELETE CASCADE,
            ref_key INTEGER UNIQUE REFERENCES refs(rkey) ON DELETE CASCADE,
            payload TEXT NOT NULL
        );",
    )?;
    tx.execute_batch("INSERT INTO refs SELECT * FROM saved_refs;
        INSERT INTO ref_keys SELECT * FROM saved_keys;
        INSERT INTO edges(rowid,id,source_key,target_key,relation,directed,owner_key,ref_key,payload) SELECT * FROM saved_edges;
        DROP TABLE temp.saved_refs; DROP TABLE temp.saved_keys; DROP TABLE temp.saved_edges;
        PRAGMA user_version=2;")?;
    for sql in indexes {
        tx.execute_batch(&sql)?;
    }
    tx.commit()?;
    integrity(conn)
}

fn old_fixture(path: &Path, format: i64) -> anyhow::Result<Connection> {
    if format == 1 {
        let conn = Connection::open(path)?;
        conn.execute_batch(include_str!("fixtures/native-format1.sql"))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        // Text FKs in format 1 do not depend on refs.rowid. Preserve signed keys.
        conn.execute("UPDATE refs SET rowid=1-rowid", [])?;
        Ok(conn)
    } else {
        seed(path)?;
        let conn = Connection::open(path)?;
        restore_format2(&conn)?;
        let tx = conn.unchecked_transaction()?;
        tx.execute_batch(
            "PRAGMA defer_foreign_keys=ON;
            UPDATE refs SET rkey=1-rkey;
            UPDATE ref_keys SET ref_key=1-ref_key;
            UPDATE edges SET ref_key=1-ref_key WHERE ref_key IS NOT NULL;",
        )?;
        tx.commit()?;
        Ok(conn)
    }
}

#[test]
fn fresh_projection_preserves_text_identity_uniqueness_and_order() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let db = temp.path().join("fresh.db");
    let mut store = Store::create(&db)?;
    let conn = Connection::open(&db)?;
    projected(&conn)?;
    let ids = ["z-ref", "2", "10", "a\0tail", "a", "é\\\"\n"];
    let refs: Vec<_> = ids.iter().map(|id| reference(id, &["absent"])).collect();
    store.apply_native(
        "fixture",
        vec![facts(
            "caller.py",
            vec![node("caller", "caller.py", &[])],
            refs.clone(),
        )],
        vec![],
        Coverage::default(),
    )?;
    let mut expected: Vec<_> = ids.iter().map(|s| (*s).to_owned()).collect();
    expected.sort();
    assert_eq!(strings(&conn, "SELECT id FROM refs ORDER BY id")?, expected);
    for reference in refs {
        let payload: String = conn.query_row(
            "SELECT payload FROM refs WHERE id=?1",
            [&reference.id],
            |r| r.get(0),
        )?;
        assert_eq!(payload, serde_json::to_string(&reference)?);
    }
    assert!(conn.execute("INSERT INTO refs(source_key,owner_key,relation,payload,resolved_target_key,resolution_reason) SELECT source_key,owner_key,relation,payload,resolved_target_key,resolution_reason FROM refs LIMIT 1", []).is_err());
    // The preceding writer's INSERT shape cannot silently populate format 3.
    let old_write = conn.execute("INSERT INTO refs(id,source_key,owner_key,relation,payload,resolved_target_key,resolution_reason) SELECT 'old-writer-id',source_key,owner_key,relation,payload,resolved_target_key,resolution_reason FROM refs LIMIT 1", []).unwrap_err();
    assert!(
        old_write.to_string().contains("generated column"),
        "{old_write}"
    );
    assert_eq!(store.snapshot()?.schema_version, 1);
    projected(&conn)
}

#[test]
fn explicit_upgrade_preserves_all_records_maps_indexes_and_fts() -> anyhow::Result<()> {
    for format in [1, 2] {
        let temp = tempfile::tempdir()?;
        let db = temp.path().join("upgrade.db");
        let conn = old_fixture(&db, format)?;
        let before = records(&conn)?;
        let graph = snapshot(&db)?;
        let search = postings(&conn)?;
        let search_bytes = (format == 2).then(|| search_storage(&conn)).transpose()?;
        #[cfg(unix)]
        let inode = {
            use std::os::unix::fs::MetadataExt;
            std::fs::metadata(&db)?.ino()
        };
        let indexes = strings(
            &conn,
            "SELECT json_array(name,sql) FROM sqlite_master WHERE type='index' ORDER BY name",
        )?;
        let report = Store::open(&db)?.apply_native("fixture", vec![], vec![], coverage(&conn)?)?;
        assert_eq!(report.parsed_files, 0);
        assert_eq!(records(&conn)?, before);
        assert_eq!(snapshot(&db)?, graph);
        assert_eq!(postings(&conn)?, search);
        if let Some(search_bytes) = search_bytes {
            assert_eq!(search_storage(&conn)?, search_bytes);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            assert_eq!(std::fs::metadata(&db)?.ino(), inode);
        }
        if format == 2 {
            assert_eq!(
                strings(
                    &conn,
                    "SELECT json_array(name,sql) FROM sqlite_master WHERE type='index' ORDER BY name"
                )?,
                indexes
            );
        }
        assert_eq!(
            conn.query_row("SELECT count(*) FROM refs WHERE rkey<=0", [], |r| r
                .get::<_, i64>(0))?,
            if format == 1 { 4 } else { 2 }
        );
        projected(&conn)?;
    }
    Ok(())
}

#[test]
fn read_open_and_explicit_compact_do_not_upgrade_old_layouts() -> anyhow::Result<()> {
    for format in [1, 2] {
        let temp = tempfile::tempdir()?;
        let db = temp.path().join("read.db");
        let conn = old_fixture(&db, format)?;
        let mut stale = Store::open(&db)?;
        conn.execute("UPDATE metadata SET generation=generation+1", [])?;
        let before = records(&conn)?;
        let old_schema = schema(&conn)?;
        conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")?;
        let bytes = std::fs::read(&db)?;
        let graph = snapshot(&db)?;
        for store in [
            Store::create(&db)?,
            Store::open(&db)?,
            Store::open_read_only(&db)?,
        ] {
            assert_eq!(serde_json::to_value(store.snapshot()?)?, graph);
            store.query("Needle", &QueryOptions::default())?;
            store.stats()?;
            store.file_stamps()?;
        }
        assert_eq!(std::fs::read(&db)?, bytes);
        assert!(
            stale
                .apply_native("fixture", vec![], vec![], coverage(&conn)?)
                .unwrap_err()
                .is::<StaleStore>()
        );
        assert!(
            Store::open_read_only(&db)?
                .apply_native("fixture", vec![], vec![], coverage(&conn)?)
                .is_err()
        );
        assert!(
            Store::open(&db)?
                .apply_native("wrong", vec![], vec![], coverage(&conn)?)
                .is_err()
        );
        assert_eq!(schema(&conn)?, old_schema);
        if format == 2 {
            Store::open(&db)?.compact()?;
        } else {
            assert!(Store::open(&db)?.compact().is_err());
        }
        assert_eq!(version(&conn)?, format);
        assert_eq!(records(&conn)?, before);
    }
    Ok(())
}

#[test]
fn late_write_failure_rolls_back_upgrade_and_all_graph_state() -> anyhow::Result<()> {
    for format in [1, 2] {
        let temp = tempfile::tempdir()?;
        let db = temp.path().join("rollback.db");
        let conn = old_fixture(&db, format)?;
        conn.execute_batch(
            "CREATE TRIGGER reject_late BEFORE UPDATE OF generation ON metadata BEGIN
            SELECT CASE WHEN (SELECT user_version FROM pragma_user_version)=5
            AND (SELECT hidden FROM pragma_table_xinfo('refs') WHERE name='id')=0
            THEN RAISE(ABORT,'after projected replacement') ELSE RAISE(ABORT,'too early') END;
        END;",
        )?;
        let before = records(&conn)?;
        let old_schema = schema(&conn)?;
        let graph = snapshot(&db)?;
        let search = postings(&conn)?;
        let mut store = Store::open(&db)?;
        let extra = facts("extra.py", vec![node("extra", "extra.py", &[])], vec![]);
        let error = store
            .apply_native("fixture", vec![extra.clone()], vec![], coverage(&conn)?)
            .unwrap_err();
        assert!(
            format!("{error:#}").contains("after projected replacement"),
            "{error:#}"
        );
        assert_eq!(version(&conn)?, format);
        assert_eq!(schema(&conn)?, old_schema);
        assert_eq!(records(&conn)?, before);
        assert_eq!(snapshot(&db)?, graph);
        assert_eq!(postings(&conn)?, search);
        conn.execute_batch("DROP TRIGGER reject_late")?;
        store.apply_native("fixture", vec![extra], vec![], coverage(&conn)?)?;
        projected(&conn)?;
    }
    Ok(())
}

#[test]
fn incompatible_payload_identity_aborts_without_rewriting_old_ids() -> anyhow::Result<()> {
    for replacement in [json!("different"), json!(42), Value::Null] {
        let temp = tempfile::tempdir()?;
        let db = temp.path().join("mismatch.db");
        let conn = old_fixture(&db, 2)?;
        let payload: String =
            conn.query_row("SELECT payload FROM refs WHERE id='z-ref'", [], |r| {
                r.get(0)
            })?;
        let mut payload: Value = serde_json::from_str(&payload)?;
        if replacement.is_null() {
            payload.as_object_mut().unwrap().remove("id");
        } else {
            payload["id"] = replacement;
        }
        conn.execute(
            "UPDATE refs SET payload=?1 WHERE id='z-ref'",
            [payload.to_string()],
        )?;
        let before = records(&conn)?;
        let old_schema = schema(&conn)?;
        let error = Store::open(&db)?
            .apply_native("fixture", vec![], vec![], coverage(&conn)?)
            .unwrap_err();
        assert!(
            format!("{error:#}").contains("reference payload identity mismatch"),
            "{error:#}"
        );
        assert_eq!(version(&conn)?, 2);
        assert_eq!(records(&conn)?, before);
        assert_eq!(schema(&conn)?, old_schema);
    }
    Ok(())
}

#[test]
fn pinned_old_reader_and_stale_handles_keep_transaction_semantics() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let db = temp.path().join("readers.db");
    let conn = old_fixture(&db, 2)?;
    let before = records(&conn)?;
    let reader = Store::open_read_only(&db)?;
    let mut writer = Store::open(&db)?;
    let mut same_generation = Store::open(&db)?;
    conn.execute_batch("BEGIN")?;
    assert_eq!(version(&conn)?, 2);
    writer.apply_native("fixture", vec![], vec![], coverage(&conn)?)?;
    assert_eq!(version(&conn)?, 2);
    assert_eq!(records(&conn)?, before);
    assert_eq!(
        conn.query_row(
            "SELECT hidden FROM pragma_table_xinfo('refs') WHERE name='id'",
            [],
            |r| r.get::<_, i64>(0)
        )?,
        0
    );
    assert_eq!(reader.snapshot()?.generation, writer.stats()?.generation);
    conn.execute_batch("COMMIT")?;
    projected(&conn)?;
    assert_eq!(records(&conn)?, before);
    // The physical-only upgrade leaves a same-generation new-code writer valid.
    same_generation.apply_native(
        "fixture",
        vec![provider("changed-target")],
        vec![],
        Coverage::default(),
    )?;
    let after = snapshot(&db)?;
    assert_eq!(serde_json::to_value(reader.snapshot()?)?, after);
    assert!(
        writer
            .apply_native("fixture", vec![], vec![], Coverage::default())
            .unwrap_err()
            .is::<StaleStore>()
    );
    assert_eq!(snapshot(&db)?, after);
    Ok(())
}

#[test]
fn native_stages_rebind_unchanged_callers_without_changing_reference_payloads() -> anyhow::Result<()>
{
    let temp = tempfile::tempdir()?;
    let db = temp.path().join("stages.db");
    seed(&db)?;
    let conn = Connection::open(&db)?;
    let mut store = Store::open(&db)?;
    let original = strings(&conn, "SELECT payload FROM refs ORDER BY id")?;
    let generation = store.stats()?.generation;
    store.apply_native("fixture", vec![], vec![], Coverage::default())?;
    assert_eq!(store.stats()?.generation, generation);
    // Same public target ID replacement retains direct assertions as well.
    store.apply_native(
        "fixture",
        vec![provider("target")],
        vec![],
        Coverage::default(),
    )?;
    assert!(
        store
            .snapshot()?
            .edges
            .iter()
            .any(|e| e.id == "static-edge")
    );
    store.apply_native(
        "fixture",
        vec![provider("replacement")],
        vec![],
        Coverage::default(),
    )?;
    let resolved = store.snapshot()?;
    assert_eq!(
        resolved
            .edges
            .iter()
            .find(|e| e.id == "reference:z-ref")
            .unwrap()
            .target,
        "replacement"
    );
    let duplicate = facts(
        "duplicate.py",
        vec![node("ambiguous", "duplicate.py", &["public:work"])],
        vec![],
    );
    store.apply_native("fixture", vec![duplicate], vec![], Coverage::default())?;
    assert!(
        store
            .snapshot()?
            .edges
            .iter()
            .all(|e| e.id != "reference:z-ref")
    );
    assert!(
        strings(&conn, "SELECT resolution_reason FROM refs WHERE id='z-ref'")?[0]
            .contains("ambiguous")
    );
    store.apply_native(
        "fixture",
        vec![],
        vec!["duplicate.py".into()],
        Coverage::default(),
    )?;
    assert!(
        store
            .snapshot()?
            .edges
            .iter()
            .any(|e| e.id == "reference:z-ref" && e.target == "replacement")
    );
    store.apply_native(
        "fixture",
        vec![],
        vec!["provider.py".into()],
        Coverage::default(),
    )?;
    assert!(store.snapshot()?.edges.is_empty());
    store.apply_native(
        "fixture",
        vec![provider("restored")],
        vec![],
        Coverage::default(),
    )?;
    assert!(
        store
            .snapshot()?
            .edges
            .iter()
            .any(|e| e.id == "reference:z-ref" && e.target == "restored")
    );
    assert_eq!(
        strings(&conn, "SELECT payload FROM refs ORDER BY id")?,
        original
    );
    store.apply_native(
        "fixture",
        vec![],
        vec!["caller.py".into()],
        Coverage::default(),
    )?;
    assert_eq!(
        conn.query_row("SELECT count(*) FROM refs", [], |r| r.get::<_, i64>(0))?,
        0
    );
    assert_eq!(
        conn.query_row("SELECT count(*) FROM ref_keys", [], |r| r.get::<_, i64>(0))?,
        0
    );
    projected(&conn)
}

#[test]
fn import_and_refresh_upgrade_only_with_the_authorized_kind() -> anyhow::Result<()> {
    for format in [1, 2] {
        let temp = tempfile::tempdir()?;
        let db = temp.path().join("import.db");
        drop(Store::create(&db)?);
        let conn = Connection::open(&db)?;
        restore_format2(&conn)?;
        if format == 1 {
            storage_legacy::restore_legacy(&conn, true)?;
        }
        let imported = || ImportedGraph {
            nodes: vec![node("external", "external.txt", &[])],
            edges: vec![],
            metadata: json!({"portable": [1, "complete"]}),
        };
        Store::open(&db)?.import_graph(imported())?;
        projected(&conn)?;
        restore_format2(&conn)?;
        if format == 1 {
            storage_legacy::restore_legacy(&conn, true)?;
        }
        let before = snapshot(&db)?;
        assert!(
            Store::open(&db)?
                .apply_native("fixture", vec![], vec![], Coverage::default())
                .is_err()
        );
        assert_eq!(version(&conn)?, format);
        assert_eq!(snapshot(&db)?, before);
        Store::open(&db)?.refresh_import(imported())?;
        projected(&conn)?;
        assert_eq!(Store::open(&db)?.snapshot()?.schema_version, 1);
    }
    Ok(())
}

#[test]
fn partial_order_indexes_and_bounded_queries_survive_projection() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let db = temp.path().join("ordered.db");
    let mut refs: Vec<_> = (0..700)
        .rev()
        .map(|i| reference(&format!("ref-{i:04}"), &["missing"]))
        .collect();
    refs[0].relation = "rare_relation".into();
    Store::create(&db)?.apply_native(
        "fixture",
        vec![facts(
            "caller.py",
            vec![node("caller", "caller.py", &[])],
            refs,
        )],
        vec![],
        Coverage::default(),
    )?;
    let conn = Connection::open(&db)?;
    restore_format2(&conn)?;
    let options = SearchOptions {
        graph: QueryOptions {
            relation: Some("rare_relation".into()),
            ..Default::default()
        },
        ..Default::default()
    };
    let budget = SearchOptions {
        token_budget: Some(2048),
        ..Default::default()
    };
    let before = Store::open_read_only(&db)?.neighbors_extended("caller", &options)?;
    let before_budget = Store::open_read_only(&db)?.neighbors_extended("caller", &budget)?;
    assert_eq!(before.graph.unresolved.len(), 1);
    assert!(
        before_budget
            .truncation_reasons
            .iter()
            .any(|s| s == "token_budget")
    );
    Store::open(&db)?.apply_native("fixture", vec![], vec![], Coverage::default())?;
    assert_eq!(
        serde_json::to_value(Store::open_read_only(&db)?.neighbors_extended("caller", &options)?)?,
        serde_json::to_value(before)?
    );
    assert_eq!(
        serde_json::to_value(Store::open_read_only(&db)?.neighbors_extended("caller", &budget)?)?,
        serde_json::to_value(before_budget)?
    );
    for sql in [
        "SELECT payload FROM refs INDEXED BY refs_unresolved_relation WHERE source_key=1 AND resolved_target_key IS NULL AND relation='rare_relation' ORDER BY id LIMIT 10",
        "SELECT relation FROM refs INDEXED BY refs_unresolved_relation WHERE source_key=1 AND resolved_target_key IS NULL AND relation>'rare_relation' ORDER BY relation LIMIT 1",
    ] {
        let plan = conn
            .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))?
            .query_map([], |r| r.get::<_, String>(3))?
            .collect::<rusqlite::Result<Vec<_>>>()?
            .join("\n");
        assert!(
            plan.contains("SEARCH refs") && plan.contains("INDEX"),
            "{plan}"
        );
        assert!(
            !plan.contains("SCAN refs") && !plan.contains("TEMP B-TREE"),
            "{plan}"
        );
    }
    projected(&conn)
}
