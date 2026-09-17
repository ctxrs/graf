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
fn options(direction: Direction) -> QueryOptions {
    QueryOptions {
        direction,
        ..QueryOptions::default()
    }
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
        ("source", "edges_source_direction_relation"),
        ("target", "edges_target_direction_relation"),
    ] {
        let sql = format!(
            "EXPLAIN QUERY PLAN SELECT payload FROM edges WHERE {column}=?1 AND directed=0 AND relation=?2 ORDER BY id LIMIT 10"
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
