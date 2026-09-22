use graf::{model::*, store::Store};
use serde_json::json;

fn node(id: &str, file: &str, key: &str) -> Node {
    Node {
        id: id.into(),
        label: id.into(),
        kind: "function".into(),
        file: file.into(),
        line: Some(1),
        end_line: Some(2),
        qualified_name: Some(id.into()),
        binding_key: Some(key.into()),
        metadata: json!({}),
    }
}
fn file(path: &str, nodes: Vec<Node>, references: Vec<Reference>) -> FileFacts {
    FileFacts {
        path: path.into(),
        hash: "hash".into(),
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
        source: "caller".into(),
        label: "work".into(),
        relation: "calls".into(),
        file: "caller.py".into(),
        line: 2,
        candidate_keys: keys.iter().map(|s| (*s).into()).collect(),
        reason: "missing target".into(),
    }
}

#[test]
fn native_snapshot_source_proof_is_owned_bounded_and_excludes_capture_stamps() -> anyhow::Result<()>
{
    let temp = tempfile::tempdir()?;
    let db = temp.path().join("proof.db");
    let digest = blake3::hash(b"indexed fixture bytes").to_hex().to_string();
    let mut facts = Vec::new();
    for (path, stamp) in [
        ("local.py", format!("python-v9:context:{digest}")),
        (
            "typed.ts",
            format!("languages-native-languages-13:context::deep:settings:{digest}"),
        ),
        ("notes.md", format!("ingest-v1:settings:{digest}")),
        ("capture.md", format!("managed-v1:{digest}")),
        ("unknown.txt", format!("future-v1:context:{digest}")),
        ("oversize.py", "python-v9:context:oversized:4MiB".into()),
        ("bad.py", format!("python-v9:{digest}")),
    ] {
        let mut record = file(path, vec![node(path, path, path)], vec![]);
        record.hash = stamp;
        facts.push(record);
    }
    let mut empty = file("empty.txt", vec![], vec![]);
    empty.hash = "unrelated".repeat(40_000);
    facts.push(empty);
    let mut store = Store::create(&db)?;
    store.apply_native(
        "nonexistent-source-root",
        facts,
        vec![],
        Coverage::default(),
    )?;
    drop(store);
    let sql = rusqlite::Connection::open(&db)?;
    sql.execute(
        "UPDATE metadata SET graph_metadata=?1",
        [r#"{"graf_source_digests":{"algorithm":"blake3","files":{"invented.txt":"forged"}}}"#],
    )?;
    let baseline_bytes = usize::try_from(sql.query_row("SELECT length(graph_metadata)+(SELECT COALESCE(SUM(length(CAST(payload AS BLOB))),0) FROM nodes) FROM metadata", [], |r| r.get::<_, i64>(0))?)?;
    drop(sql);
    let before = std::fs::read(&db)?;
    let store = Store::open_read_only(&db)?;
    assert!(
        store
            .snapshot_bounded(10, 0, 0, baseline_bytes + 64)
            .is_err()
    );
    let bounded = store.snapshot_bounded(10, 0, 0, 30_000)?;
    let plain = store.snapshot()?;
    assert_eq!(
        serde_json::to_value(&bounded)?,
        serde_json::to_value(&plain)?
    );
    assert_eq!(
        plain.metadata["graf_source_digests"],
        json!({"algorithm":"blake3","files":{
            "local.py":digest,"typed.ts":digest,"notes.md":digest
        }})
    );
    assert_eq!(std::fs::read(&db)?, before);
    Ok(())
}
fn options(direction: Direction) -> QueryOptions {
    QueryOptions {
        direction,
        ..QueryOptions::default()
    }
}

#[test]
fn compatibility_search_preserves_literals_and_migrates_only_on_explicit_write()
-> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let db = directory.path().join("index.db");
    let mut item = node("ligature-id", "example.py", "python:example:flow");
    item.label = "ﬂow controller".into();
    item.qualified_name = None;
    item.binding_key = None;
    let mut store = Store::create(&db)?;
    store.apply_native(
        "repo",
        vec![file("example.py", vec![item.clone()], vec![])],
        vec![],
        Coverage::default(),
    )?;
    let query = graf::query::SearchOptions::default();
    assert_eq!(
        store.query_extended("flow", &query)?.graph.nodes[0].id,
        item.id
    );
    assert_eq!(
        store.query_extended("ﬂow", &query)?.graph.nodes[0].id,
        item.id
    );
    let generation = store.stats()?.generation;
    drop(store);

    // Restore the exact relevant old-index representation, not a new-store
    // simulation that already contains compatibility-normalized postings.
    let sql = rusqlite::Connection::open(&db)?;
    sql.execute_batch("UPDATE metadata SET search_version=1; UPDATE nodes SET search='ﬂow controller example.py'; DELETE FROM node_search; INSERT INTO node_search(rowid,text) SELECT rowid,search FROM nodes;")?;
    drop(sql);
    let before = std::fs::read(&db)?;
    let read = Store::open_read_only(&db)?;
    assert!(read.query_extended("flow", &query)?.graph.nodes.is_empty());
    assert_eq!(
        read.query_extended("ﬂow", &query)?.graph.nodes[0].id,
        item.id
    );
    assert_eq!(
        read.query_extended("ﬂow controller", &query)?.graph.nodes[0].id,
        item.id
    );
    assert_eq!(read.stats()?.generation, generation);
    drop(read);
    assert_eq!(std::fs::read(&db)?, before);

    let mut store = Store::open(&db)?;
    let updated = store.apply_native("repo", vec![], vec![], Coverage::default())?;
    assert_eq!(updated.generation, generation + 1);
    assert_eq!(updated.parsed_files, 0);
    assert_eq!(
        store.query_extended("flow", &query)?.graph.nodes[0].label,
        item.label
    );
    assert_eq!(
        store.query_extended("ﬂow", &query)?.graph.nodes[0].id,
        item.id
    );
    assert_eq!(
        store
            .apply_native("repo", vec![], vec![], Coverage::default())?
            .generation,
        updated.generation
    );

    let imported = directory.path().join("imported.db");
    let mut store = Store::create(&imported)?;
    store.import_graph(ImportedGraph {
        nodes: vec![item],
        edges: vec![],
        metadata: json!({}),
    })?;
    assert_eq!(store.query_extended("flow", &query)?.graph.nodes.len(), 1);
    Ok(())
}

#[test]
fn unversioned_search_keeps_fullwidth_literals_without_read_side_migration() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let db = directory.path().join("index.db");
    let mut item = node("fullwidth-id", "example.py", "python:example:flow");
    item.label = "Ｆｌｏｗ manager".into();
    item.qualified_name = None;
    item.binding_key = None;
    let mut store = Store::create(&db)?;
    store.apply_native(
        "repo",
        vec![file("example.py", vec![item.clone()], vec![])],
        vec![],
        Coverage::default(),
    )?;
    let generation = store.stats()?.generation;
    drop(store);
    let sql = rusqlite::Connection::open(&db)?;
    sql.execute_batch("UPDATE nodes SET search='Ｆｌｏｗ manager example.py'; DELETE FROM node_search; INSERT INTO node_search(rowid,text) SELECT rowid,search FROM nodes; ALTER TABLE metadata DROP COLUMN search_version;")?;
    drop(sql);
    let before = std::fs::read(&db)?;
    let query = graf::query::SearchOptions::default();
    let read = Store::open_read_only(&db)?;
    assert_eq!(
        read.query_extended("Ｆｌｏｗ", &query)?.graph.nodes[0].id,
        item.id
    );
    assert!(read.query_extended("flow", &query)?.graph.nodes.is_empty());
    assert_eq!(read.stats()?.generation, generation);
    drop(read);
    assert_eq!(std::fs::read(&db)?, before);
    let mut store = Store::open(&db)?;
    store.apply_native("repo", vec![], vec![], Coverage::default())?;
    for text in ["flow", "Ｆｌｏｗ"] {
        assert_eq!(
            store.query_extended(text, &query)?.graph.nodes[0].id,
            item.id
        );
    }
    assert_eq!(store.stats()?.generation, generation + 1);
    Ok(())
}

#[test]
fn compatibility_search_migration_failure_rolls_back_postings_and_generation() -> anyhow::Result<()>
{
    let directory = tempfile::tempdir()?;
    let db = directory.path().join("index.db");
    let mut item = node("item", "example.py", "key");
    item.label = "Ｆｌｏｗ manager".into();
    item.qualified_name = None;
    let mut store = Store::create(&db)?;
    store.apply_native(
        "repo",
        vec![file("example.py", vec![item], vec![])],
        vec![],
        Coverage::default(),
    )?;
    let generation = store.stats()?.generation;
    drop(store);
    let sql = rusqlite::Connection::open(&db)?;
    sql.execute_batch("UPDATE metadata SET search_version=1; UPDATE nodes SET search='Ｆｌｏｗ manager'; DELETE FROM node_search; INSERT INTO node_search(rowid,text) SELECT rowid,search FROM nodes; CREATE TRIGGER reject_search BEFORE UPDATE OF search ON nodes BEGIN SELECT RAISE(ABORT,'synthetic migration failure'); END;")?;
    let mut store = Store::open(&db)?;
    assert!(
        store
            .apply_native("repo", vec![], vec![], Coverage::default())
            .is_err()
    );
    assert_eq!(store.stats()?.generation, generation);
    assert_eq!(
        sql.query_row("SELECT search_version FROM metadata", [], |r| r
            .get::<_, i64>(0))?,
        1
    );
    assert_eq!(
        sql.query_row(
            "SELECT count(*) FROM node_search WHERE node_search MATCH 'Flow'",
            [],
            |r| r.get::<_, i64>(0)
        )?,
        0
    );
    sql.execute_batch("DROP TRIGGER reject_search")?;
    store.apply_native("repo", vec![], vec![], Coverage::default())?;
    assert_eq!(
        store
            .query_extended("flow", &graf::query::SearchOptions::default())?
            .graph
            .nodes
            .len(),
        1
    );
    Ok(())
}
fn edge(id: &str, source: &str, target: &str, directed: bool) -> Edge {
    Edge {
        id: id.into(),
        source: source.into(),
        target: target.into(),
        relation: "calls".into(),
        directed,
        file: None,
        line: None,
        confidence: "EXTRACTED".into(),
        metadata: json!({"upstream":true}),
    }
}

#[test]
fn empty_native_node_and_edge_ids_are_rejected_in_fresh_and_initialized_stores()
-> anyhow::Result<()> {
    for initialized in [false, true] {
        let directory = tempfile::tempdir()?;
        let db = directory.path().join("index.db");
        let mut store = Store::create(&db)?;
        if initialized {
            store.apply_native(
                "repo",
                vec![file(
                    "seed.py",
                    vec![node("seed", "seed.py", "python:seed")],
                    vec![],
                )],
                vec![],
                Coverage::default(),
            )?;
        }

        let mut empty_node = node("", "bad-node.py", "python:bad");
        empty_node.label = "bad".into();
        let invalid_node = file("bad-node.py", vec![empty_node], vec![]);
        let mut invalid_edge = file(
            "bad-edge.py",
            vec![
                node("left", "bad-edge.py", "python:left"),
                node("right", "bad-edge.py", "python:right"),
            ],
            vec![],
        );
        invalid_edge.edges.push(edge("", "left", "right", true));

        for (facts, expected) in [
            (invalid_node, "node ID cannot be empty"),
            (invalid_edge, "edge ID cannot be empty"),
        ] {
            let before = serde_json::to_value(store.snapshot()?)?;
            let generation = store.stats()?.generation;
            let error = store
                .apply_native("repo", vec![facts], vec![], Coverage::default())
                .unwrap_err();
            assert!(error.to_string().contains(expected), "{error:#}");
            assert_eq!(store.stats()?.generation, generation);
            assert_eq!(serde_json::to_value(store.snapshot()?)?, before);
        }
    }
    Ok(())
}

#[test]
fn bounded_snapshots_include_unresolved_evidence_and_check_payload_before_loading()
-> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let db = directory.path().join("index.db");
    let mut store = Store::create(&db)?;
    let mut source = node("caller", "caller.py", "caller");
    source.metadata = json!({"evidence":"é".repeat(1000)});
    store.apply_native(
        "repo",
        vec![file(
            "caller.py",
            vec![source],
            vec![reference("pending", &["missing"])],
        )],
        vec![],
        Coverage::default(),
    )?;
    drop(store);
    let before = std::fs::read(&db)?;
    let store = Store::open_read_only(&db)?;
    assert!(store.snapshot_bounded(1, 0, 0, 10000).is_err());
    assert!(store.snapshot_bounded(0, 0, 1, 10000).is_err());
    assert!(store.snapshot_bounded(1, 0, 1, 1500).is_err());
    let snapshot = store.snapshot_bounded(1, 0, 1, 10000)?;
    let references: Vec<Reference> =
        serde_json::from_value(snapshot.metadata["graf_unresolved_references"].clone())?;
    assert_eq!(references.len(), 1);
    assert_eq!(references[0].candidate_keys, ["missing"]);
    assert_eq!(references[0].source, "caller");
    assert_eq!(
        serde_json::to_value(snapshot)?,
        serde_json::to_value(store.snapshot()?)?
    );
    assert_eq!(std::fs::read(&db)?, before);
    Ok(())
}

#[test]
fn multiple_reference_resolutions_preserve_bindings_and_call_sites() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let db = dir.path().join("index.db");
    let mut store = Store::create(&db)?;
    let mut last = reference("c-last", &["last"]);
    last.source = "other-caller".into();
    last.relation = "uses".into();
    last.line = 8;
    store.apply_native(
        "repo",
        vec![
            file(
                "caller.py",
                vec![
                    node("caller", "caller.py", "caller"),
                    node("other-caller", "caller.py", "other-caller"),
                ],
                // Resolution order is by ID, independent of insertion order.
                vec![
                    last,
                    reference("b-missing", &["missing"]),
                    reference("a-first", &["first"]),
                ],
            ),
            file(
                "targets.py",
                vec![
                    node("first", "targets.py", "first"),
                    node("last", "targets.py", "last"),
                ],
                vec![],
            ),
        ],
        vec![],
        Coverage::default(),
    )?;
    let graph = store.snapshot()?;
    assert_eq!(
        graph
            .edges
            .iter()
            .map(|e| json!([e.id, e.source, e.target, e.relation, e.line]))
            .collect::<Vec<_>>(),
        vec![
            json!(["reference:a-first", "caller", "first", "calls", 2]),
            json!(["reference:c-last", "other-caller", "last", "uses", 8]),
        ]
    );
    let conn = rusqlite::Connection::open(&db)?;
    let resolutions = conn
        .prepare("SELECT r.id,n.id,r.resolution_reason FROM refs r LEFT JOIN nodes n ON n.nkey=r.resolved_target_key ORDER BY r.id")?
        .query_map([], |row| {
            Ok(json!([
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?
            ]))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    assert_eq!(
        resolutions,
        vec![
            json!(["a-first", "first", ""]),
            json!(["b-missing", null, "missing target"]),
            json!(["c-last", "last", ""]),
        ]
    );
    Ok(())
}

#[test]
fn deltas_revisit_negative_and_ambiguous_bindings_without_losing_call_sites() -> anyhow::Result<()>
{
    let dir = tempfile::tempdir()?;
    let mut store = Store::create(&dir.path().join("index.db"))?;
    store.apply_native(
        "repo",
        vec![file(
            "caller.py",
            vec![node("caller", "caller.py", "caller")],
            vec![reference("site", &["preferred", "fallback"])],
        )],
        vec![],
        Coverage::default(),
    )?;
    assert_eq!(store.stats()?.unresolved_references, 1);
    // A newly available negative lookup resolves a reference in an unchanged file.
    store.apply_native(
        "repo",
        vec![file(
            "fallback.py",
            vec![node("fallback", "fallback.py", "fallback")],
            vec![],
        )],
        vec![],
        Coverage::default(),
    )?;
    assert_eq!(
        store
            .neighbors("caller", &options(Direction::Outgoing))?
            .edges[0]
            .target,
        "fallback"
    );
    // Ambiguity at the preferred key must not fall through to the fallback.
    store.apply_native(
        "repo",
        vec![
            file("a.py", vec![node("a", "a.py", "preferred")], vec![]),
            file("b.py", vec![node("b", "b.py", "preferred")], vec![]),
        ],
        vec![],
        Coverage::default(),
    )?;
    let result = store.neighbors("caller", &options(Direction::Outgoing))?;
    assert!(result.edges.is_empty());
    assert!(result.unresolved[0].reason.contains("ambiguous"));
    store.apply_native("repo", vec![], vec!["b.py".into()], Coverage::default())?;
    assert_eq!(
        store
            .neighbors("caller", &options(Direction::Outgoing))?
            .edges[0]
            .target,
        "a"
    );
    store.apply_native(
        "repo",
        vec![],
        vec!["a.py".into(), "fallback.py".into()],
        Coverage::default(),
    )?;
    let result = store.neighbors("caller", &options(Direction::Outgoing))?;
    assert_eq!(result.unresolved.len(), 1);
    assert_eq!(result.unresolved[0].line, 2);
    assert_eq!(store.file_stamps()?.len(), 1);
    Ok(())
}

#[test]
fn target_only_replacement_preserves_direct_edges_and_rebinds_references() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let mut store = Store::create(&dir.path().join("index.db"))?;
    let mut caller = file(
        "caller.py",
        vec![node("caller", "caller.py", "caller")],
        vec![reference("site", &["work"])],
    );
    caller.edges = vec![
        edge("direct-a", "caller", "target", true),
        edge("direct-b", "caller", "target", false),
    ];
    store.apply_native(
        "repo",
        vec![
            caller,
            file(
                "target.py",
                vec![node("target", "target.py", "work")],
                vec![],
            ),
        ],
        vec![],
        Coverage::default(),
    )?;
    let before = store.snapshot()?;
    assert_eq!(before.edges.len(), 3);
    let mut target = node("target", "target.py", "work");
    target.label = "updated label".into();
    store.apply_native(
        "repo",
        vec![file("target.py", vec![target], vec![])],
        vec![],
        Coverage::default(),
    )?;
    let after = store.snapshot()?;
    assert_eq!(after.generation, before.generation + 1);
    assert_eq!(
        serde_json::to_value(after.edges)?,
        serde_json::to_value(before.edges)?
    );
    assert_eq!(store.stats()?.unresolved_references, 0);

    // A different target ID removes direct assertions; a reference still
    // resolves to the surviving binding instead of retaining its old edge.
    store.apply_native(
        "repo",
        vec![file(
            "target.py",
            vec![node("replacement", "target.py", "work")],
            vec![],
        )],
        vec![],
        Coverage::default(),
    )?;
    let rebound = store.snapshot()?;
    assert_eq!(rebound.edges.len(), 1);
    assert_eq!(rebound.edges[0].target, "replacement");
    assert!(!rebound.edges[0].id.starts_with("direct-"));
    assert_eq!(store.stats()?.unresolved_references, 0);
    store.apply_native(
        "repo",
        vec![],
        vec!["target.py".into()],
        Coverage::default(),
    )?;
    assert!(store.snapshot()?.edges.is_empty());
    assert_eq!(store.stats()?.unresolved_references, 1);
    Ok(())
}

#[test]
fn preserved_direct_edges_follow_owner_changes_and_transaction_rollback() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let mut store = Store::create(&dir.path().join("index.db"))?;
    let mut caller = file(
        "caller.py",
        vec![node("caller", "caller.py", "caller")],
        vec![],
    );
    caller.edges.push(edge("direct", "caller", "target", true));
    let target = file(
        "target.py",
        vec![node("target", "target.py", "work")],
        vec![],
    );
    store.apply_native(
        "repo",
        vec![caller.clone(), target.clone()],
        vec![],
        Coverage::default(),
    )?;
    let before = serde_json::to_value(store.snapshot()?)?;
    let mut bad = target.clone();
    bad.edges.push(edge("broken", "target", "absent", true));
    assert!(
        store
            .apply_native("repo", vec![bad], vec![], Coverage::default())
            .is_err()
    );
    assert_eq!(serde_json::to_value(store.snapshot()?)?, before);

    // Replacing both files must respect the owner's removal of its assertion.
    let mut without_edge = caller.clone();
    without_edge.edges.clear();
    store.apply_native(
        "repo",
        vec![without_edge, target.clone()],
        vec![],
        Coverage::default(),
    )?;
    assert!(store.snapshot()?.edges.is_empty());
    store.apply_native("repo", vec![caller.clone()], vec![], Coverage::default())?;
    store.apply_native(
        "repo",
        vec![target.clone()],
        vec!["caller.py".into()],
        Coverage::default(),
    )?;
    assert!(store.snapshot()?.edges.is_empty());

    // Deleting the target outright also removes an unchanged owner's edge.
    store.apply_native("repo", vec![caller], vec![], Coverage::default())?;
    assert_eq!(store.snapshot()?.edges.len(), 1);
    store.apply_native(
        "repo",
        vec![],
        vec!["target.py".into()],
        Coverage::default(),
    )?;
    assert!(store.snapshot()?.edges.is_empty());
    Ok(())
}

#[test]
fn failures_roll_back_and_noop_preserves_generation() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let mut store = Store::create(&dir.path().join("index.db"))?;
    let first = store.apply_native(
        "repo",
        vec![file("one.py", vec![node("one", "one.py", "one")], vec![])],
        vec![],
        Coverage::default(),
    )?;
    let noop = store.apply_native("repo", vec![], vec![], Coverage::default())?;
    assert_eq!(first.generation, noop.generation);
    let mut bad = file(
        "one.py",
        vec![node("replacement", "one.py", "other")],
        vec![],
    );
    bad.edges
        .push(edge("broken", "replacement", "absent", true));
    assert!(
        store
            .apply_native("repo", vec![bad], vec![], Coverage::default())
            .is_err()
    );
    assert_eq!(
        store.neighbors("one", &QueryOptions::default())?.generation,
        first.generation
    );
    assert!(
        store
            .apply_native("other-root", vec![], vec![], Coverage::default())
            .is_err()
    );
    assert!(
        store
            .import_graph(ImportedGraph {
                nodes: vec![],
                edges: vec![],
                metadata: json!({})
            })
            .is_err()
    );
    assert_eq!(store.stats()?.nodes, 1);
    Ok(())
}

#[test]
fn import_preserves_direction_parallel_edges_and_bounds() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("index.db");
    let mut store = Store::create(&path)?;
    let mut nodes = vec![node("a", "", "a"), node("b", "", "b"), node("c", "", "c")];
    nodes[1].label = "Shared".into();
    nodes[2].label = "Shared".into();
    store.import_graph(ImportedGraph {
        nodes,
        edges: vec![
            edge("ab", "a", "b", true),
            edge("ab2", "a", "b", true),
            edge("bc", "b", "c", false),
        ],
        metadata: json!({"producer":"fixture"}),
    })?;
    let incoming_a = store.neighbors("a", &options(Direction::Incoming))?;
    assert!(incoming_a.edges.is_empty());
    let outgoing_c = store.neighbors("c", &options(Direction::Outgoing))?;
    assert_eq!(outgoing_c.edges[0].source, "b");
    assert_eq!(outgoing_c.edges[0].target, "c");
    assert!(!outgoing_c.edges[0].directed);
    assert_eq!(
        store
            .neighbors("a", &options(Direction::Outgoing))?
            .edges
            .len(),
        2
    );
    assert!(
        store
            .neighbors("Shared", &QueryOptions::default())
            .unwrap_err()
            .to_string()
            .contains("ambiguous")
    );
    let opts = QueryOptions {
        depth: 2,
        direction: Direction::Outgoing,
        ..QueryOptions::default()
    };
    let path_result = store.path("a", "c", &opts)?;
    assert!(path_result.found);
    assert_eq!(
        path_result
            .graph
            .nodes
            .iter()
            .map(|n| n.id.as_str())
            .collect::<Vec<_>>(),
        vec!["a", "b", "c"]
    );
    assert!(!store.path("c", "a", &opts)?.found);
    let limited = QueryOptions {
        depth: 1,
        limit: 1,
        ..opts.clone()
    };
    assert!(store.neighbors("a", &limited)?.truncated);
    assert!(store.path("a", "c", &limited)?.graph.truncated);
    assert!(store.query("Shared", &limited)?.nodes.len() <= 1);
    assert!(
        store
            .query(
                "a",
                &QueryOptions {
                    depth: 7,
                    ..opts.clone()
                }
            )
            .is_err()
    );
    assert!(
        store
            .query("a", &QueryOptions { limit: 501, ..opts })
            .is_err()
    );
    assert!(
        store
            .apply_native("repo", vec![], vec![], Coverage::default())
            .is_err()
    );
    drop(store);
    let conn = rusqlite::Connection::open(&path)?;
    let metadata: String =
        conn.query_row("SELECT graph_metadata FROM metadata", [], |r| r.get(0))?;
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&metadata)?,
        json!({"producer":"fixture"})
    );
    Ok(())
}

#[test]
fn opening_or_importing_never_clobbers_unrelated_data() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let missing = dir.path().join("missing.db");
    assert!(Store::open(&missing).is_err());
    assert!(!missing.exists());
    let path = dir.path().join("foreign.db");
    let conn = rusqlite::Connection::open(&path)?;
    conn.execute_batch(
        "CREATE TABLE important(value TEXT); INSERT INTO important VALUES('keep');",
    )?;
    drop(conn);
    let before = std::fs::read(&path)?;
    assert!(Store::create(&path).is_err());
    assert!(Store::open(&path).is_err());
    assert_eq!(std::fs::read(&path)?, before);
    let mut store = Store::create(&missing)?;
    assert!(
        store
            .import_graph(ImportedGraph {
                nodes: vec![node("a", "", "a")],
                edges: vec![edge("bad", "a", "absent", true)],
                metadata: json!({})
            })
            .is_err()
    );
    assert_eq!(store.stats()?.kind, "empty");
    assert_eq!(store.stats()?.nodes, 0);
    Ok(())
}

#[test]
fn fts_is_persistent_and_removed_with_file_facts() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("index.db");
    let mut store = Store::create(&path)?;
    let mut symbol = node("id", "api.py", "binding");
    symbol.label = "HttpClient".into();
    store.apply_native(
        "repo",
        vec![file("api.py", vec![symbol], vec![])],
        vec![],
        Coverage::default(),
    )?;
    drop(store);
    let mut store = Store::open(&path)?;
    assert_eq!(
        store.query("client", &QueryOptions::default())?.nodes[0].id,
        "id"
    );
    assert!(store.query("***", &QueryOptions::default()).is_err());
    store.apply_native("repo", vec![], vec!["api.py".into()], Coverage::default())?;
    assert!(
        store
            .query("client", &QueryOptions::default())?
            .nodes
            .is_empty()
    );
    Ok(())
}

#[test]
fn compact_search_projection_keeps_identifier_and_prose_recall() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("index.db");
    let mut item = node("camel-id", "example.py", "key");
    item.label = "CamelCase".into();
    item.metadata = json!({"description":"searchable prose"});
    let expected = "camel-id CamelCase camel-id example.py searchable prose camel-id Camel Case camel-id example.py searchable prose";
    let legacy = "camel-id CamelCase camel-id example.py searchable prose camel-id CamelCase camel-id example.py searchable prose camel-id Camel Case camel-id example.py searchable prose";
    let mut store = Store::create(&path)?;
    store.apply_native(
        "repo",
        vec![file("example.py", vec![item.clone()], vec![])],
        vec![],
        Coverage::default(),
    )?;

    let sql = rusqlite::Connection::open(&path)?;
    let search: String = sql.query_row("SELECT search FROM nodes", [], |r| r.get(0))?;
    assert_eq!(search, expected);
    assert!(search.len() < legacy.len());
    for text in ["camelcase", "camel case", "searchable"] {
        assert_eq!(
            store.query(text, &QueryOptions::default())?.nodes[0].id,
            item.id
        );
    }
    assert_eq!(
        serde_json::to_value(store.snapshot()?.nodes)?,
        serde_json::to_value(vec![item.clone()])?
    );

    drop(store);
    let sql = rusqlite::Connection::open(&path)?;
    sql.execute("UPDATE metadata SET search_version=3", [])?;
    sql.execute("UPDATE nodes SET search=?1", [&legacy])?;
    sql.execute_batch(
        "DELETE FROM node_search; INSERT INTO node_search(rowid,text) SELECT rowid,search FROM nodes;",
    )?;
    drop(sql);
    let before = std::fs::read(&path)?;
    let read = Store::open_read_only(&path)?;
    assert_eq!(
        serde_json::to_value(read.snapshot()?.nodes)?,
        serde_json::to_value(vec![item.clone()])?
    );
    drop(read);
    assert_eq!(std::fs::read(&path)?, before);

    let mut store = Store::open(&path)?;
    store.apply_native("repo", vec![], vec![], Coverage::default())?;
    let sql = rusqlite::Connection::open(&path)?;
    assert_eq!(
        sql.query_row("SELECT search_version FROM metadata", [], |r| r
            .get::<_, i64>(0))?,
        5
    );
    let search: String = sql.query_row("SELECT search FROM nodes", [], |r| r.get(0))?;
    assert_eq!(search, expected);
    assert_eq!(
        serde_json::to_value(store.snapshot()?.nodes)?,
        serde_json::to_value(vec![item])?
    );
    Ok(())
}

#[test]
fn literal_compatibility_prefixes_survive_explicit_search_migration() -> anyhow::Result<()> {
    for (label, prefix) in [("ﬂow controller", "ﬂow"), ("Ｆｌｏｗ manager", "Ｆｌｏｗ")]
    {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("index.db");
        let mut item = node("item", "example.py", "key");
        item.label = label.into();
        let mut store = Store::create(&path)?;
        store.apply_native(
            "repo",
            vec![file("example.py", vec![item.clone()], vec![])],
            vec![],
            Coverage::default(),
        )?;
        assert_eq!(
            store.query(prefix, &QueryOptions::default())?.nodes[0].id,
            "item"
        );
        assert_eq!(
            store.query("flow", &QueryOptions::default())?.nodes[0].id,
            "item"
        );
        let mut snapshot = serde_json::to_value(store.snapshot()?)?;
        drop(store);

        // Authentic old raw postings: changing only the version on new
        // postings would not exercise literal old-index compatibility.
        let sql = rusqlite::Connection::open(&path)?;
        let raw = format!("item {label} item example.py");
        sql.execute("UPDATE metadata SET search_version=3", [])?;
        sql.execute("UPDATE nodes SET search=?1", [&raw])?;
        sql.execute_batch("DELETE FROM node_search; INSERT INTO node_search(rowid,text) SELECT rowid,search FROM nodes;")?;
        drop(sql);
        let reader = Store::open_read_only(&path)?;
        assert_eq!(
            reader.query(prefix, &QueryOptions::default())?.nodes[0].id,
            "item"
        );
        drop(reader);

        let mut store = Store::open(&path)?;
        store.apply_native("repo", vec![], vec![], Coverage::default())?;
        // Explicit search migration publishes one new generation, preserving
        // the complete graph and all remaining snapshot fields.
        snapshot["generation"] = (snapshot["generation"].as_u64().unwrap() + 1).into();
        for query in [prefix, "flow", label] {
            assert_eq!(
                store.query(query, &QueryOptions::default())?.nodes[0].id,
                "item"
            );
        }
        assert_eq!(serde_json::to_value(store.snapshot()?)?, snapshot);
    }
    Ok(())
}

#[test]
fn a_query_never_combines_generations_during_updates() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("index.db");
    let mut writer = Store::create(&path)?;
    let facts = |generation: usize| {
        let mut n = node("stable", "one.py", "stable");
        n.label = format!("generation{generation}");
        file("one.py", vec![n], vec![])
    };
    writer.apply_native("repo", vec![facts(1)], vec![], Coverage::default())?;
    let reader = Store::open(&path)?;
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let writer_barrier = barrier.clone();
    let handle = std::thread::spawn(move || -> anyhow::Result<()> {
        writer_barrier.wait();
        for generation in 2..=20 {
            writer.apply_native("repo", vec![facts(generation)], vec![], Coverage::default())?;
        }
        Ok(())
    });
    barrier.wait();
    for _ in 0..40 {
        let result = reader.neighbors("stable", &QueryOptions::default())?;
        assert_eq!(
            result.nodes[0].label,
            format!("generation{}", result.generation)
        );
    }
    handle.join().unwrap()?;
    Ok(())
}

#[test]
fn a_stale_scan_cannot_undo_another_indexer() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("index.db");
    let mut writer = Store::create(&path)?;
    writer.apply_native(
        "repo",
        vec![file("one.py", vec![node("one", "one.py", "one")], vec![])],
        vec![],
        Coverage::default(),
    )?;
    let mut stale = Store::open(&path)?;
    assert_eq!(stale.file_stamps()?.len(), 1);
    writer.apply_native(
        "repo",
        vec![file("one.py", vec![node("new", "one.py", "new")], vec![])],
        vec![],
        Coverage::default(),
    )?;
    let error = stale
        .apply_native("repo", vec![], vec!["one.py".into()], Coverage::default())
        .unwrap_err();
    assert!(error.to_string().contains("retry"));
    assert_eq!(stale.stats()?.nodes, 1);
    assert_eq!(
        stale.neighbors("new", &QueryOptions::default())?.nodes[0].id,
        "new"
    );
    // The writer refreshes its own baseline only after a successful commit.
    let before = writer.stats()?.generation;
    writer.apply_native("repo", vec![], vec![], Coverage::default())?;
    assert_eq!(writer.stats()?.generation, before);
    Ok(())
}

#[test]
fn directional_hubs_use_filtered_indexes_and_a_shared_edge_budget() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("index.db");
    let mut store = Store::create(&path)?;
    // Parallel edges stress the scan limit without a large node fixture.
    let mut edges: Vec<_> = (0..5_001)
        .map(|i| edge(&format!("directed{i:05}"), "hub", "leaf", true))
        .collect();
    edges.push(edge("undirected", "hub", "other", false));
    store.import_graph(ImportedGraph {
        nodes: vec![
            node("hub", "", ""),
            node("leaf", "", ""),
            node("other", "", ""),
        ],
        edges,
        metadata: json!({}),
    })?;
    let incoming = store.neighbors("hub", &options(Direction::Incoming))?;
    assert!(!incoming.truncated);
    assert_eq!(incoming.edges.len(), 1);
    assert_eq!(incoming.edges[0].id, "undirected");
    let outgoing = store.neighbors("hub", &options(Direction::Outgoing))?;
    assert!(outgoing.truncated);
    assert_eq!(outgoing.edges.len(), 5_000);
    let conn = rusqlite::Connection::open(&path)?;
    for (column, expected) in [
        ("source_key", "edges_source_direction_relation"),
        ("target_key", "edges_target_direction_relation"),
    ] {
        let sql = format!(
            "EXPLAIN QUERY PLAN SELECT payload FROM edges WHERE {column}=(SELECT nkey FROM nodes WHERE id=?1) AND directed=0 AND relation=?2 ORDER BY id LIMIT 10"
        );
        let mut stmt = conn.prepare(&sql)?;
        let plan = stmt
            .query_map(rusqlite::params!["hub", "calls"], |r| r.get::<_, String>(3))?
            .collect::<rusqlite::Result<Vec<_>>>()?
            .join("\n");
        assert!(plan.contains(expected), "{plan}");
        assert!(
            !plan.contains("SCAN ") && !plan.contains("TEMP B-TREE"),
            "{plan}"
        );
    }
    Ok(())
}
