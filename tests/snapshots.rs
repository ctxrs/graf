use std::{
    io::Write,
    sync::{Arc, Barrier},
};

use graf::{
    model::{Coverage, Edge, GraphSnapshot, ImportedGraph, Node, QueryOptions, SCHEMA_VERSION},
    snapshot,
    store::Store,
};
use serde_json::{Value, json};
use tempfile::{NamedTempFile, tempdir};

fn graph(marker: usize, count: usize) -> ImportedGraph {
    let nodes = (0..count)
        .map(|i| Node {
            id: format!("n{i}"),
            label: format!("epoch{marker}"),
            kind: "concept".into(),
            file: "same.py".into(),
            line: Some(1),
            end_line: Some(2),
            qualified_name: Some(format!("m.n{i}")),
            binding_key: Some(format!("m:n{i}")),
            metadata: json!({"marker": marker, "project": "original", "original_id": [i]}),
        })
        .collect();
    let edges = if count < 2 {
        vec![]
    } else {
        (0..3)
            .map(|i| Edge {
                id: format!("e{i}"),
                source: "n0".into(),
                target: "n1".into(),
                relation: "calls".into(),
                directed: i != 1,
                file: None,
                line: Some(5),
                confidence: "EXTRACTED".into(),
                metadata: json!({"marker": marker, "ordinal": i}),
            })
            .collect()
    };
    ImportedGraph {
        nodes,
        edges,
        metadata: json!({"marker": marker}),
    }
}

fn as_snapshot(graph: ImportedGraph) -> GraphSnapshot {
    GraphSnapshot {
        schema_version: SCHEMA_VERSION,
        generation: 42,
        kind: "imported".into(),
        root: None,
        nodes: graph.nodes,
        edges: graph.edges,
        metadata: graph.metadata,
    }
}

fn read(value: &Value) -> anyhow::Result<ImportedGraph> {
    let mut file = NamedTempFile::new()?;
    file.write_all(value.to_string().as_bytes())?;
    snapshot::read(file.path())
}

fn value(store: &Store) -> Value {
    serde_json::to_value(store.snapshot().unwrap()).unwrap()
}

#[test]
fn full_snapshot_and_graf_interchange_preserve_all_fields_and_multiedges() {
    let dir = tempdir().unwrap();
    let mut store = Store::create(&dir.path().join("source.db")).unwrap();
    let input = graph(9, 610); // larger than ordinary query's maximum result limit
    let expected = as_snapshot(input.clone());
    store.import_graph(input).unwrap();
    let exported = value(&store);
    assert_eq!(exported["nodes"].as_array().unwrap().len(), 610);
    assert_eq!(
        exported["edges"],
        serde_json::to_value(expected.edges).unwrap()
    );
    let imported = read(&exported).unwrap();
    assert_eq!(
        serde_json::to_value(&imported.nodes).unwrap(),
        exported["nodes"]
    );
    assert_eq!(
        serde_json::to_value(&imported.edges).unwrap(),
        exported["edges"]
    );
    assert_eq!(
        imported.metadata["graf_snapshot"]["metadata"],
        exported["metadata"]
    );
    assert_eq!(
        imported.metadata["graf_snapshot"]["generation"],
        exported["generation"]
    );
    let mut destination = Store::create(&dir.path().join("destination.db")).unwrap();
    destination.import_graph(imported).unwrap();
    assert_eq!(value(&destination)["nodes"], exported["nodes"]);
    assert_eq!(value(&destination)["edges"], exported["edges"]);
    assert_eq!(
        destination
            .neighbors("n0", &QueryOptions::default())
            .unwrap()
            .edges
            .len(),
        3
    );
}

#[test]
fn refresh_replaces_everything_and_invalid_or_stale_writes_preserve_old_state() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("graph.db");
    let mut store = Store::create(&path).unwrap();
    assert!(store.refresh_import(graph(1, 2)).is_err());
    store.import_graph(graph(1, 4)).unwrap();
    let mut stale = Store::open(&path).unwrap();
    let old = value(&store);
    let mut bad = graph(2, 2);
    bad.edges[0].target = "missing".into();
    assert!(store.refresh_import(bad).is_err());
    assert_eq!(value(&store), old);
    let mut bad = graph(2, 2);
    bad.nodes.push(bad.nodes[0].clone());
    assert!(store.refresh_import(bad).is_err());
    let mut bad = graph(2, 2);
    bad.edges.push(bad.edges[0].clone());
    assert!(store.refresh_import(bad).is_err());
    assert_eq!(value(&store), old);

    // Force a failure after deletion and insertion have begun, not just a
    // parser rejection before the write transaction.
    let sql = rusqlite::Connection::open(&path).unwrap();
    sql.execute_batch("CREATE TRIGGER fail_refresh BEFORE INSERT ON nodes WHEN new.id='n1' BEGIN SELECT RAISE(ABORT, 'test write failure'); END;").unwrap();
    assert!(store.refresh_import(graph(2, 2)).is_err());
    assert_eq!(value(&store), old);
    sql.execute_batch("DROP TRIGGER fail_refresh").unwrap();
    let report = store.refresh_import(graph(2, 2)).unwrap();
    assert_eq!((report.nodes, report.edges, report.generation), (2, 3, 2));
    assert!(
        store
            .query("epoch1", &QueryOptions::default())
            .unwrap()
            .nodes
            .is_empty()
    );
    assert_eq!(
        store
            .query("epoch2", &QueryOptions::default())
            .unwrap()
            .nodes
            .len(),
        2
    );
    assert!(stale.refresh_import(graph(3, 8)).is_err());
    assert_eq!(value(&stale), value(&store));
    assert_eq!(store.refresh_import(graph(3, 0)).unwrap().generation, 3);
    assert!(store.snapshot().unwrap().nodes.is_empty());
}

#[test]
fn native_indexes_and_index_roots_cannot_be_replaced() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("native.db");
    let mut store = Store::create(&path).unwrap();
    store
        .apply_native("synthetic-root", vec![], vec![], Coverage::default())
        .unwrap();
    let before = value(&store);
    assert!(store.refresh_import(graph(2, 2)).is_err());
    assert!(store.import_graph(graph(2, 2)).is_err());
    assert_eq!(value(&store), before);
    let source = read(&before).unwrap();
    assert_eq!(source.metadata["graf_snapshot"]["root"], "synthetic-root");
    assert_eq!(source.metadata["graf_snapshot"]["kind"], "native");

    let path = dir.path().join("root.db");
    let mut imported = Store::create(&path).unwrap();
    imported.import_graph(graph(0, 2)).unwrap();
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute("UPDATE metadata SET root='protected-root'", [])
        .unwrap();
    assert!(imported.refresh_import(graph(2, 2)).is_err());
    assert_eq!(
        imported.snapshot().unwrap().root.as_deref(),
        Some("protected-root")
    );
}

#[test]
fn snapshots_remain_consistent_during_atomic_refreshes() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("concurrent.db");
    let mut initial = Store::create(&path).unwrap();
    initial.import_graph(graph(0, 250)).unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let writer_barrier = barrier.clone();
    let writer_path = path.clone();
    let writer = std::thread::spawn(move || {
        let mut store = Store::open(&writer_path).unwrap();
        writer_barrier.wait();
        for marker in 1..=30 {
            store.refresh_import(graph(marker, 250 + marker)).unwrap();
        }
    });
    barrier.wait();
    for _ in 0..60 {
        let snapshot = initial.snapshot().unwrap();
        let marker = snapshot.metadata["marker"].as_u64().unwrap();
        assert_eq!(snapshot.generation, marker + 1);
        assert_eq!(snapshot.nodes.len(), 250 + marker as usize);
        assert!(
            snapshot
                .nodes
                .iter()
                .all(|node| node.metadata["marker"] == marker)
        );
        assert!(
            snapshot
                .edges
                .iter()
                .all(|edge| edge.metadata["marker"] == marker)
        );
    }
    writer.join().unwrap();
    assert_eq!(initial.snapshot().unwrap().generation, 31);
}

#[test]
fn composition_uses_disjoint_namespaces_and_lossless_provenance() {
    let one = as_snapshot(graph(1, 2));
    let mut two = as_snapshot(graph(2, 2));
    two.nodes[0].id = "project:x:n0".into();
    for edge in &mut two.edges {
        edge.source = two.nodes[0].id.clone();
    }
    let names = ["a:b", "a", "a\\\"β"];
    let snapshots = vec![
        (names[0].into(), one.clone()),
        (names[1].into(), two),
        (names[2].into(), one.clone()),
    ];
    let merged = snapshot::merge(snapshots).unwrap();
    assert_eq!((merged.nodes.len(), merged.edges.len()), (6, 9));
    let ids: std::collections::HashSet<_> = merged.nodes.iter().map(|n| n.id.as_str()).collect();
    assert_eq!(ids.len(), 6);
    for edge in &merged.edges {
        assert!(ids.contains(edge.source.as_str()) && ids.contains(edge.target.as_str()));
        let source = merged.nodes.iter().find(|n| n.id == edge.source).unwrap();
        assert_eq!(source.metadata["project"], edge.metadata["project"]);
        assert_eq!(edge.directed, edge.metadata["original_id"] != "e1");
    }
    assert_eq!(
        merged.nodes[0].metadata["original_metadata"],
        one.nodes[0].metadata
    );
    assert_eq!(merged.metadata["projects"][0]["generation"], 42);
    assert!(snapshot::merge(vec![]).is_err());
    assert!(
        snapshot::merge(vec![
            ("same".into(), one.clone()),
            ("same".into(), one.clone())
        ])
        .is_err()
    );
    assert!(snapshot::merge(vec![(" ".into(), one.clone())]).is_err());
    let dir = tempdir().unwrap();
    let mut store = Store::create(&dir.path().join("merged.db")).unwrap();
    store.import_graph(merged).unwrap();
    let reread = read(&value(&store)).unwrap();
    assert_eq!(
        serde_json::to_value(reread.nodes).unwrap(),
        value(&store)["nodes"]
    );
}

#[test]
fn graf_reader_rejects_ambiguous_unknown_and_invalid_data() {
    let base = serde_json::to_value(as_snapshot(graph(0, 2))).unwrap();
    for (pointer, replacement) in [
        ("/schema_version", json!(999)),
        ("/generation", json!(-1)),
        ("/kind", json!("other")),
        ("/nodes/1/id", json!("n0")),
        ("/edges/1/id", json!("e0")),
        ("/edges/0/target", json!("missing")),
        ("/nodes/0/id", json!("")),
        ("/edges/0/directed", json!(1)),
    ] {
        let mut input = base.clone();
        *input.pointer_mut(pointer).unwrap() = replacement;
        assert!(read(&input).is_err(), "accepted {pointer}");
    }
    for pointer in ["", "/nodes/0", "/edges/0"] {
        let mut input = base.clone();
        input.pointer_mut(pointer).unwrap()["unknown"] = json!(1);
        assert!(read(&input).is_err());
    }
    let mut file = NamedTempFile::new().unwrap();
    file.write_all(
        base.to_string()
            .replace("\"marker\":0", "\"marker\":0,\"marker\":1")
            .as_bytes(),
    )
    .unwrap();
    assert!(snapshot::read(file.path()).is_err());
    file.as_file().set_len(256 * 1024 * 1024 + 1).unwrap();
    assert!(
        snapshot::read(file.path())
            .unwrap_err()
            .to_string()
            .contains("256 MiB")
    );
    assert!(snapshot::read(tempdir().unwrap().path()).is_err());
}

fn facts(path: &str, id: &str, aliases: Value) -> graf::model::FileFacts {
    let mut node = graph(0, 1).nodes.remove(0);
    node.id = id.into();
    node.file = path.into();
    node.binding_key = Some(format!("primary:{id}"));
    node.metadata = json!({"binding_aliases": aliases});
    graf::model::FileFacts {
        path: path.into(),
        hash: "test-hash".into(),
        module: path.into(),
        nodes: vec![node],
        edges: vec![],
        references: vec![],
        diagnostics: vec![],
    }
}

#[test]
fn native_options_commit_atomically_and_legacy_apply_preserves_metadata() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("options.db");
    let mut store = Store::create(&path).unwrap();
    let options = json!({"code_only":true});
    let first = store
        .apply_native_with_options("root", vec![], vec![], Coverage::default(), options.clone())
        .unwrap();
    assert_eq!(first.generation, 1);
    assert_eq!(
        store.graph_metadata().unwrap()["graf_index_options"],
        options
    );
    let sql = rusqlite::Connection::open(&path).unwrap();
    sql.execute(
        "UPDATE metadata SET graph_metadata=?1",
        [json!({"graf_index_options":options,"other":[1,2]}).to_string()],
    )
    .unwrap();
    assert_eq!(
        store
            .apply_native("root", vec![], vec![], Coverage::default())
            .unwrap()
            .generation,
        1
    );
    assert_eq!(
        store
            .apply_native_with_options("root", vec![], vec![], Coverage::default(), options)
            .unwrap()
            .generation,
        1
    );
    assert_eq!(
        store
            .apply_native_with_options(
                "root",
                vec![],
                vec![],
                Coverage::default(),
                json!({"code_only":false})
            )
            .unwrap()
            .generation,
        2
    );
    assert_eq!(store.graph_metadata().unwrap()["other"], json!([1, 2]));
    let before = value(&store);
    let invalid = facts("bad.py", "bad", json!([false]));
    assert!(
        store
            .apply_native_with_options(
                "root",
                vec![invalid],
                vec![],
                Coverage::default(),
                json!({"code_only":true})
            )
            .is_err()
    );
    assert_eq!(value(&store), before);
}

#[test]
fn aliases_resolve_unique_nodes_and_rebind_after_ambiguity_changes_and_deletion() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("aliases.db");
    let mut store = Store::create(&path).unwrap();
    let mut caller = facts("notes.md", "caller", json!([]));
    caller.references.push(graf::model::Reference {
        id: "link".into(),
        source: "caller".into(),
        label: "Thing".into(),
        relation: "references".into(),
        file: "notes.md".into(),
        line: 1,
        candidate_keys: vec!["symbol:Thing".into()],
        reason: "missing".into(),
    });
    let mut target = facts("one.py", "one", json!(["symbol:Thing", "symbol:Thing"]));
    target.nodes[0].binding_key = Some("symbol:Thing".into()); // same target through both indexes
    store
        .apply_native("root", vec![caller, target], vec![], Coverage::default())
        .unwrap();
    assert_eq!(store.snapshot().unwrap().edges[0].target, "one");
    let second = facts("two.rs", "two", json!(["symbol:Thing"]));
    store
        .apply_native("root", vec![second], vec![], Coverage::default())
        .unwrap();
    assert!(store.snapshot().unwrap().edges.is_empty());
    assert_eq!(store.stats().unwrap().unresolved_references, 1);
    store
        .apply_native("root", vec![], vec!["one.py".into()], Coverage::default())
        .unwrap();
    assert_eq!(store.snapshot().unwrap().edges[0].target, "two");
    store
        .apply_native(
            "root",
            vec![facts("two.rs", "two", json!([]))],
            vec![],
            Coverage::default(),
        )
        .unwrap();
    assert!(store.snapshot().unwrap().edges.is_empty());
    store
        .apply_native(
            "root",
            vec![facts("two.rs", "two", json!(["symbol:Thing"]))],
            vec![],
            Coverage::default(),
        )
        .unwrap();
    assert_eq!(store.snapshot().unwrap().edges[0].target, "two");
    let sql = rusqlite::Connection::open(&path).unwrap();
    assert_eq!(
        sql.query_row("SELECT count(*) FROM node_aliases", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn schema_one_opens_without_migration_and_indexing_backfills_aliases() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("legacy.db");
    let mut store = Store::create(&path).unwrap();
    let target = facts("one.py", "one", json!(["file:one.py"]));
    store
        .apply_native("root", vec![target], vec![], Coverage::default())
        .unwrap();
    let sql = rusqlite::Connection::open(&path).unwrap();
    sql.execute_batch("DROP TABLE node_aliases").unwrap();
    drop(store);
    let mut store = Store::open(&path).unwrap();
    store.snapshot().unwrap();
    store.stats().unwrap();
    store.query("epoch0", &QueryOptions::default()).unwrap();
    assert_eq!(
        sql.query_row(
            "SELECT count(*) FROM sqlite_master WHERE name='node_aliases'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    store
        .apply_native("root", vec![], vec![], Coverage::default())
        .unwrap();
    assert_eq!(
        sql.query_row("SELECT binding_key FROM node_aliases", [], |r| r
            .get::<_, String>(0))
            .unwrap(),
        "file:one.py"
    );
    assert_eq!(
        sql.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn graphify_extensions_roundtrip_exact_records_and_reject_conflicting_topology() {
    use graf::{
        export::{ExportFormat, render},
        import::{read_graphify, read_graphify_export},
    };
    let mut file = NamedTempFile::new().unwrap();
    file.write_all(
        json!({"nodes":[{"id":"a"},{"id":"b"}],"edges":[],
            "hyperedges":[{"id":"shared","label":"Shared flow","nodes":["a","b"]},{"nodes":["a"]}]
        })
        .to_string()
        .as_bytes(),
    )
    .unwrap();
    let groups = as_snapshot(read_graphify_export(file.path()).unwrap());
    let merged = snapshot::merge(vec![
        ("left".into(), groups.clone()),
        ("right".into(), groups),
    ])
    .unwrap();
    let mut input = as_snapshot(merged);
    let mut ordinary = graph(7, 2);
    input.nodes.append(&mut ordinary.nodes);
    input.edges.append(&mut ordinary.edges);
    let rendered = render(&input, ExportFormat::GraphifyJson).unwrap();
    let json: Value = serde_json::from_str(&rendered).unwrap();
    assert_eq!(json["hyperedges"].as_array().unwrap().len(), 4);
    fn ordered<T: serde::Serialize>(records: &[T]) -> Value {
        let mut records = serde_json::to_value(records)
            .unwrap()
            .as_array()
            .unwrap()
            .clone();
        records.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
        Value::Array(records)
    }
    for reader in [read_graphify, read_graphify_export] {
        let mut file = NamedTempFile::new().unwrap();
        file.write_all(rendered.as_bytes()).unwrap();
        let imported = reader(file.path()).unwrap();
        assert_eq!(ordered(&imported.nodes), ordered(&input.nodes));
        assert_eq!(ordered(&imported.edges), ordered(&input.edges));
        assert_eq!(imported.metadata["_graf"]["metadata"], input.metadata);
    }
    for (pointer, replacement) in [
        ("/nodes/0/_graf/id", json!("elsewhere")),
        ("/links/0/_graf/source", json!("n1")),
        ("/links/1/_graf/directed", json!(true)),
        ("/hyperedges/0/_graf_group/incidences/0/source", json!("n0")),
        ("/hyperedges/0/_graf_group/incidences/0/target", json!("n0")),
        ("/hyperedges/0/_graf_group/node/id", json!("n0")),
    ] {
        let mut malformed = json.clone();
        *malformed.pointer_mut(pointer).unwrap() = replacement;
        let mut file = NamedTempFile::new().unwrap();
        file.write_all(malformed.to_string().as_bytes()).unwrap();
        assert!(
            read_graphify(file.path()).is_err(),
            "accepted conflicting {pointer}"
        );
        assert!(
            read_graphify_export(file.path()).is_err(),
            "accepted conflicting {pointer}"
        );
    }
}
