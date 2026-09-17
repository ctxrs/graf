use std::io::Write;

use graf::import::{read_graphify, read_graphify_export};
use graf::model::{Direction, ImportedGraph, QueryOptions};
use graf::store::Store;
use serde_json::{Value, json};
use tempfile::NamedTempFile;

fn read_text(text: &str) -> anyhow::Result<ImportedGraph> {
    let mut file = NamedTempFile::new()?;
    file.write_all(text.as_bytes())?;
    read_graphify(file.path())
}

fn read(value: Value) -> anyhow::Result<ImportedGraph> {
    read_text(&value.to_string())
}

fn read_export_text(text: &str) -> anyhow::Result<ImportedGraph> {
    let mut file = NamedTempFile::new()?;
    file.write_all(text.as_bytes())?;
    read_graphify_export(file.path())
}

fn read_export(value: Value) -> anyhow::Result<ImportedGraph> {
    read_export_text(&value.to_string())
}

#[test]
fn export_legacy_numbers_and_types_keep_original_evidence() {
    let input = json!({
        "nodes":[{"id":"a","name":"Guide","file_type":"markdown","path":"guide.md"},
                 {"id":"b","file_type":"tool"},{"id":"c","file_type":null}],
        "links":[{"from":"a","to":"b","type":"references","confidence":0.9,"confidence_score":"0.4","weight":"2.5"},
                 {"source":"b","target":"c","relation":"uses","confidence":"0.75"}]
    });
    let graph = read_export(input.clone()).unwrap();
    assert_eq!(graph.nodes[0].kind, "document");
    assert_eq!(graph.nodes[0].metadata["file_type"], "markdown");
    assert_eq!(graph.nodes[1].kind, "code");
    assert_eq!(graph.nodes[2].kind, "concept");
    assert_eq!(graph.edges[0].confidence, "INFERRED");
    assert_eq!(graph.edges[0].metadata["confidence_score"], 0.4);
    assert_eq!(graph.edges[0].metadata["weight"], 2.5);
    assert_eq!(
        graph.edges[0].metadata["_graf_import_original"],
        input["links"][0]
    );
    assert_eq!(graph.edges[1].metadata["confidence_score"], 0.75);
    let snapshot = graf::model::GraphSnapshot {
        schema_version: 1,
        generation: 1,
        kind: "imported".into(),
        root: None,
        nodes: graph.nodes,
        edges: graph.edges,
        metadata: graph.metadata,
    };
    let encoded =
        graf::export::render(&snapshot, graf::export::ExportFormat::GraphifyJson).unwrap();
    let restored = read_export_text(&encoded).unwrap();
    assert_eq!(
        serde_json::to_value(&restored.edges).unwrap(),
        serde_json::to_value(&snapshot.edges).unwrap()
    );
    assert!(graf::analysis::analyze(&snapshot, &Default::default()).is_ok());
}

#[test]
fn export_external_stubs_require_a_declared_import_source() {
    let graph = read_export(json!({"nodes":[{"id":"local"}],"edges":[
        {"source":"local","target":"vendor","relation":"imports"},
        {"source":"vendor","target":"local","_src":"local","_tgt":"vendor","relation":"re_exports"}
    ]}))
    .unwrap();
    assert_eq!(graph.nodes.len(), 2);
    assert_eq!(graph.edges.len(), 2);
    let external = graph.nodes.iter().find(|n| n.id == "vendor").unwrap();
    assert_eq!(external.metadata["external"], true);
    assert!(external.file.is_empty());
    assert!(
        graph
            .edges
            .iter()
            .all(|e| e.source == "local" && e.target == "vendor")
    );
    for edges in [
        json!([{"source":"local","target":"absent","relation":"calls"}]),
        json!([{"source":"absent","target":"local","relation":"imports"}]),
        json!([{"source":"local","target":"vendor","relation":"imports"},
               {"source":"vendor","target":"second","relation":"imports"}]),
        json!([{"source":"local","target":"other","_src":"local","_tgt":"vendor","relation":"imports"}]),
    ] {
        assert!(read_export(json!({"nodes":[{"id":"local"}],"edges":edges})).is_err());
    }
    assert!(
        read(
            json!({"directed":true,"multigraph":false,"nodes":[{"id":"local"}],
        "edges":[{"source":"local","target":"vendor","relation":"imports"}]})
        )
        .is_err()
    );
}

#[test]
fn export_legacy_invalid_numbers_are_not_silently_repaired() {
    for (field, value) in [
        ("weight", json!(-1)),
        ("weight", json!("NaN")),
        ("weight", json!(true)),
        ("confidence", json!(1.1)),
        ("confidence_score", json!("Infinity")),
    ] {
        let mut edge = json!({"source":"a","target":"b"});
        edge[field] = value;
        assert!(read_export(json!({"nodes":[{"id":"a"},{"id":"b"}],"edges":[edge]})).is_err());
    }
    let inferred = read_export(json!({"nodes":[{"id":"a"},{"id":"b"}],
        "edges":[{"source":"a","target":"b","confidence_score":0.8}]}))
    .unwrap();
    assert_eq!(inferred.edges[0].confidence, "INFERRED");
}

#[test]
fn preserves_legacy_direction_and_opaque_attributes() {
    let graph = read(json!({
        "directed": false, "multigraph": false,
        "graph": {"title": "Example", "hyperedges": []}, "producer": "example",
        "nodes": [
            {"id": "worker", "label": "Worker", "file_type": "code",
             "source_file": "missing/worker.py", "source_location": "L4-L9",
             "metadata": {"tags": ["service"]}},
            {"id": "queue", "name": "Queue", "path": "missing/queue.py"},
            {"id": "note", "source_location": "section 2"}
        ],
        "links": [
            {"source": "queue", "target": "worker", "_src": "worker", "_tgt": "queue",
             "relation": "calls", "confidence": "INFERRED", "confidence_score": 0.8,
             "source_file": "missing/worker.py", "source_location": "L7", "context": "call"},
            {"source": "queue", "target": "note"}
        ]
    }))
    .unwrap();
    assert_eq!(graph.nodes[0].id, "worker");
    assert_eq!(
        (graph.nodes[0].line, graph.nodes[0].end_line),
        (Some(4), Some(9))
    );
    assert_eq!(
        graph.nodes[0].metadata["metadata"]["tags"],
        json!(["service"])
    );
    assert_eq!(graph.nodes[1].label, "Queue");
    assert_eq!(graph.nodes[2].metadata["source_location"], "section 2");
    let edge = &graph.edges[0];
    assert_eq!((&*edge.source, &*edge.target), ("worker", "queue"));
    assert!(edge.directed);
    assert_eq!((&*edge.relation, &*edge.confidence), ("calls", "INFERRED"));
    assert_eq!(edge.line, Some(7));
    assert_eq!(edge.metadata["confidence_score"], 0.8);
    assert_eq!(edge.metadata["context"], "call");
    assert!(!graph.edges[1].directed);
    assert_eq!(graph.edges[1].confidence, "UNKNOWN");
    assert_eq!(graph.metadata["graph"]["title"], "Example");
    assert_eq!(graph.metadata["producer"], "example");
    assert!(graph.metadata.get("nodes").is_none());
}

#[test]
fn edges_alias_preserves_keyed_parallel_edges_and_typed_ids() {
    let graph = read(json!({
        "directed": true, "multigraph": true,
        "nodes": [{"id": 7}, {"id": "7"}],
        "edges": [
            {"source": 7, "target": "7", "key": 0, "relation": "calls", "confidence": "EXTRACTED"},
            {"from": 7, "to": "7", "key": "0", "type": "imports", "confidence": "AMBIGUOUS"}
        ]
    }))
    .unwrap();
    assert_eq!(graph.nodes[0].id, "graphify:integer:7");
    assert_eq!(graph.nodes[1].id, "7");
    assert_eq!(graph.nodes[0].metadata["id"], 7);
    assert_eq!(graph.edges.len(), 2);
    assert_ne!(graph.edges[0].id, graph.edges[1].id);
    for edge in &graph.edges {
        assert_eq!((&*edge.source, &*edge.target), ("graphify:integer:7", "7"));
        assert!(edge.directed);
    }
    assert_eq!(graph.edges[0].metadata["key"], 0);
    assert_eq!(graph.edges[1].metadata["key"], "0");
    assert_eq!(graph.edges[1].relation, "imports");
    assert_eq!(graph.edges[1].confidence, "AMBIGUOUS");
}

#[test]
fn rejects_ambiguous_structure_and_malformed_hyperedges() {
    let base = json!({"directed": true, "multigraph": false, "nodes": [], "links": []});
    for (field, value) in [
        ("edges", json!([])),
        ("nodes", json!({})),
        ("directed", json!("false")),
        ("hyperedges", json!([{"nodes": ["a", "b", "c"]}])),
        ("graph", json!({"hyperedges": [{"nodes": ["a", "b", "c"]}]})),
    ] {
        let mut input = base.clone();
        input[field] = value;
        assert!(read(input).is_err(), "accepted invalid {field}");
    }
    assert!(
        read_text(
            r#"{"directed":true,"multigraph":false,"nodes":[],"links":[],"graph":{"x":1,"x":2}}"#
        )
        .is_err()
    );
}

#[test]
fn rejects_id_collisions_and_unknown_typed_endpoints() {
    for nodes in [
        json!([{"id": "same"}, {"id": "same"}]),
        json!([{"id": 7}, {"id": "graphify:integer:7"}]),
        json!([{"id": 7.5}]),
    ] {
        assert!(
            read(json!({"directed": true, "multigraph": false, "nodes": nodes, "links": []}))
                .is_err()
        );
    }
    // A tagged string must not resolve to an integer node merely because its
    // spelling matches the imported integer's display ID.
    assert!(
        read(json!({"directed": true, "multigraph": false,
            "nodes": [{"id": 7}],
            "links": [{"source": "graphify:integer:7", "target": 7}]
        }))
        .is_err()
    );
}

#[test]
fn rejects_conflicting_direction_without_dropping_edges() {
    let base = json!({"directed": false, "multigraph": false,
        "nodes": [{"id": "a"}, {"id": "b"}, {"id": "c"}], "links": []});
    for edge in [
        json!({"source": "a", "target": "b", "_src": "b"}),
        json!({"source": "a", "target": "b", "_src": "a", "_tgt": "c"}),
        json!({"source": "a", "target": "b", "_src": "b", "_tgt": "a", "directed": false}),
        json!({"source": "a", "from": "c", "target": "b"}),
    ] {
        let mut input = base.clone();
        input["links"] = json!([edge]);
        assert!(read(input).is_err());
    }
    let mut input = base;
    input["links"] = json!([
        {"source": "a", "target": "b", "relation": "calls"},
        {"source": "b", "target": "a", "relation": "imports"}
    ]);
    assert!(read(input).is_err());
}

#[test]
fn refuses_snapshots_over_the_explicit_limit() {
    let file = NamedTempFile::new().unwrap();
    file.as_file().set_len(256 * 1024 * 1024 + 1).unwrap();
    let error = read_graphify(file.path()).unwrap_err();
    assert!(error.to_string().contains("256 MiB"));
    let error = read_graphify_export(file.path()).unwrap_err();
    assert!(error.to_string().contains("256 MiB"));
}

#[test]
fn export_direction_is_explicit_and_original_metadata_is_unchanged() {
    // Independently authored normal producer shape: storage is undirected,
    // but source/target carry the logical direction for every relation.
    let input = json!({
        "directed": false, "multigraph": false, "hyperedges": [],
        "graph": {"hyperedges": [], "producer_note": "fixture"},
        "nodes": [{"id":"a", "norm_label":"unused"}, {"id":"b"}],
        "links": [{"source":"a", "target":"b", "relation":"custom",
                   "directed":false, "source_location":"L12"}]
    });
    let strict = read(input.clone()).unwrap();
    assert!(!strict.edges[0].directed);
    let exported = read_export(input.clone()).unwrap();
    assert!(exported.edges[0].directed);
    assert_eq!(
        (&*exported.edges[0].source, &*exported.edges[0].target),
        ("a", "b")
    );
    assert_eq!(exported.edges[0].metadata, input["links"][0]);
    assert_eq!(exported.nodes[0].metadata, input["nodes"][0]);
    let mut metadata = input.clone();
    metadata.as_object_mut().unwrap().remove("nodes");
    metadata.as_object_mut().unwrap().remove("links");
    assert_eq!(exported.metadata, metadata);

    // Legacy markers are authoritative in export mode, even if the storage
    // flags disagree. Strict node-link mode still refuses that contradiction.
    let mut legacy = input;
    legacy["directed"] = json!(true);
    legacy["links"][0]["_src"] = json!("b");
    legacy["links"][0]["_tgt"] = json!("a");
    assert!(read(legacy.clone()).is_err());
    for flag in [true, false] {
        legacy["directed"] = json!(flag);
        let graph = read_export(legacy.clone()).unwrap();
        assert_eq!(
            (&*graph.edges[0].source, &*graph.edges[0].target),
            ("b", "a")
        );
        assert!(graph.edges[0].directed);
        assert_eq!(graph.edges[0].metadata, legacy["links"][0]);
        assert_eq!(graph.metadata["directed"], flag);
    }
}

fn raw_export() -> Value {
    json!({
        "nodes": [{"id":"a"}, {"id":"b"}],
        "edges": [
            {"source":"a", "target":"b", "relation":"calls", "source_location":"L3"},
            {"source":"a", "target":"b", "relation":"imports", "source_location":"L1"},
            {"source":"a", "target":"b", "relation":"calls", "source_location":"L3"},
            {"source":"b", "target":"a", "relation":"calls", "source_location":"L8"},
            {"source":"a", "target":"a", "relation":"calls", "source_location":"L4"}
        ],
        "hyperedges": [], "producer_note": "raw fixture"
    })
}

#[test]
fn raw_and_networkx_exports_preserve_every_fact_through_storage_and_queries() {
    // Raw extraction uses edges, raw update uses links. Flags may be absent
    // or false; neither is permission to collapse facts in export mode.
    for edge_field in ["edges", "links"] {
        for flags in [
            json!({}),
            json!({"directed":false}),
            json!({"directed":false,"multigraph":false}),
        ] {
            let mut input = raw_export();
            let edges = input.as_object_mut().unwrap().remove("edges").unwrap();
            input[edge_field] = edges;
            input
                .as_object_mut()
                .unwrap()
                .extend(flags.as_object().unwrap().clone());
            assert!(read(input.clone()).is_err());
            let graph = read_export(input.clone()).unwrap();
            let actual: Vec<_> = graph
                .edges
                .iter()
                .map(|e| {
                    (
                        e.id.as_str(),
                        e.source.as_str(),
                        e.target.as_str(),
                        e.relation.as_str(),
                        e.line,
                    )
                })
                .collect();
            assert_eq!(
                actual,
                [
                    ("graphify:edge:0", "a", "b", "calls", Some(3)),
                    ("graphify:edge:1", "a", "b", "imports", Some(1)),
                    ("graphify:edge:2", "a", "b", "calls", Some(3)),
                    ("graphify:edge:3", "b", "a", "calls", Some(8)),
                    ("graphify:edge:4", "a", "a", "calls", Some(4)),
                ]
            );
            for (index, edge) in graph.edges.iter().enumerate() {
                assert!(edge.directed);
                assert_eq!(edge.metadata, input[edge_field][index]);
            }
            for flag in ["directed", "multigraph"] {
                assert_eq!(graph.metadata.get(flag), input.get(flag));
            }
            let directory = tempfile::tempdir().unwrap();
            let db = directory.path().join("index.db");
            let mut store = Store::create(&db).unwrap();
            assert_eq!(store.import_graph(graph).unwrap().edges, 5);
            drop(store);
            let store = Store::open(&db).unwrap();
            for (direction, relation, expected) in [
                (Direction::Outgoing, Some("calls"), vec![0, 2, 4]),
                (Direction::Incoming, Some("calls"), vec![3, 4]),
                (Direction::Both, Some("imports"), vec![1]),
                (Direction::Both, None, vec![0, 1, 2, 3, 4]),
            ] {
                let result = store
                    .neighbors(
                        "a",
                        &QueryOptions {
                            direction,
                            relation: relation.map(str::to_owned),
                            ..QueryOptions::default()
                        },
                    )
                    .unwrap();
                assert!(!result.truncated);
                let mut ids: Vec<_> = result.edges.iter().map(|e| e.id.clone()).collect();
                ids.sort();
                assert_eq!(
                    ids,
                    expected
                        .iter()
                        .map(|i| format!("graphify:edge:{i}"))
                        .collect::<Vec<_>>()
                );
                for edge in result.edges {
                    let ordinal: usize = edge
                        .id
                        .strip_prefix("graphify:edge:")
                        .unwrap()
                        .parse()
                        .unwrap();
                    assert_eq!(edge.metadata, input[edge_field][ordinal]);
                    assert_eq!(
                        edge.source,
                        input[edge_field][ordinal]["source"].as_str().unwrap()
                    );
                    assert_eq!(
                        edge.target,
                        input[edge_field][ordinal]["target"].as_str().unwrap()
                    );
                }
            }
        }
    }
}

#[test]
fn export_multigraph_keys_follow_serialized_pair_identity() {
    let mut input = raw_export();
    input["directed"] = json!(true);
    input["multigraph"] = json!(true);
    for (edge, key) in input["edges"].as_array_mut().unwrap().iter_mut().zip([
        json!(0),
        json!("0"),
        json!(2),
        json!(0),
        json!(0),
    ]) {
        edge["key"] = key;
    }
    for graph in [
        read_export(input.clone()).unwrap(),
        read(input.clone()).unwrap(),
    ] {
        assert_eq!(graph.edges.len(), 5);
        assert_eq!(graph.edges[1].metadata["key"], "0");
        assert_eq!(graph.edges[3].id, "graphify:edge:3");
    }
    // Undirected serialized pairs share a key namespace, even though the
    // exported logical arcs have opposite directions.
    input["directed"] = json!(false);
    assert!(
        format!("{:#}", read_export(input.clone()).unwrap_err())
            .contains("duplicate edge identity")
    );
    input["edges"][3]["key"] = json!(3);
    assert_eq!(read_export(input.clone()).unwrap().edges.len(), 5);
    input.as_object_mut().unwrap().remove("directed");
    assert_eq!(read_export(input.clone()).unwrap().edges.len(), 5);
    input["edges"][3]["key"] = json!(0);
    assert!(read_export(input).is_err());
}

#[test]
fn malformed_exports_fail_before_any_graph_is_published() {
    let base = raw_export();
    let mut invalid = Vec::new();
    for field in ["directed", "multigraph"] {
        for value in [json!(null), json!(0), json!("false"), json!([])] {
            let mut input = base.clone();
            input[field] = value;
            invalid.push(input);
        }
    }
    for edge in [
        json!({"source":"a","target":"b","directed":"true"}),
        json!({"source":"a","target":"b","_src":"b"}),
        json!({"source":"a","target":"b","_src":"a","_tgt":"a"}),
        json!({"source":"a","target":"b","_src":"a","_tgt":"missing"}),
        json!({"source":"a","from":"b","target":"b"}),
        json!({"source":0,"target":"b"}),
        json!({"source":"a","target":"b","key":0}),
        json!({"source":"a","target":"b","relation":"calls","type":"imports"}),
    ] {
        let mut input = base.clone();
        // Fail after a valid fact, guarding against returning partial output.
        input["edges"][1] = edge;
        invalid.push(input);
    }
    for nodes in [
        json!([{"id":"a"},{"id":"a"}]),
        json!([{"id":false}]),
        json!([{"id":"a","label":"one","name":"two"}]),
        json!([{"id":7},{"id":"graphify:integer:7"}]),
    ] {
        let mut input = base.clone();
        input["nodes"] = nodes;
        invalid.push(input);
    }
    for (field, value) in [
        ("links", json!([])),
        ("hyperedges", json!([{"nodes":["a","missing"]}])),
        ("graph", json!({"hyperedges":[{"nodes":["a","missing"]}]})),
        ("multigraph", json!(true)), // missing keys
    ] {
        let mut input = base.clone();
        input[field] = value;
        invalid.push(input);
    }
    for key in [json!(null), json!(false), json!([]), json!(0.5), json!(0)] {
        let mut input = base.clone();
        input["multigraph"] = json!(true);
        input["directed"] = json!(true);
        for (index, edge) in input["edges"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .enumerate()
        {
            edge["key"] = json!(index);
        }
        input["edges"][1]["key"] = key;
        invalid.push(input);
    }
    let directory = tempfile::tempdir().unwrap();
    let mut store = Store::create(&directory.path().join("index.db")).unwrap();
    let generation = store.stats().unwrap().generation;
    for input in invalid {
        let result = read_export(input.clone()).and_then(|graph| store.import_graph(graph));
        assert!(result.is_err(), "accepted {input}");
        let stats = store.stats().unwrap();
        assert_eq!(stats.kind, "empty");
        assert_eq!(
            (stats.nodes, stats.edges, stats.generation),
            (0, 0, generation)
        );
    }
    assert!(read_export_text(r#"{"nodes":[],"edges":[],"graph":{"x":1,"x":2}}"#).is_err());
    assert_eq!(
        store
            .import_graph(read_export(base).unwrap())
            .unwrap()
            .edges,
        5
    );
}

#[cfg(unix)]
#[test]
fn rejects_non_regular_inputs_without_blocking() {
    use std::process::Command;
    use std::time::{Duration, Instant};

    // Re-exec this test so a FIFO regression can be killed and reaped rather
    // than hanging the suite or leaving a blocked thread behind.
    if let Some(path) = std::env::var_os("GRAF_IMPORT_FIFO_TEST_PATH") {
        for reader in [read_graphify, read_graphify_export, graf::snapshot::read] {
            let error = reader(std::path::Path::new(&path)).unwrap_err();
            assert!(error.to_string().contains("regular file"), "{error:#}");
        }
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let fifo = directory.path().join("snapshot.json");
    assert!(
        Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap()
            .success()
    );
    let link = directory.path().join("link.json");
    std::os::unix::fs::symlink(&fifo, &link).unwrap();
    for path in [&fifo, &link, &directory.path().to_path_buf()] {
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "rejects_non_regular_inputs_without_blocking",
                "--nocapture",
            ])
            .env("GRAF_IMPORT_FIFO_TEST_PATH", path)
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                assert!(status.success());
                break;
            }
            if Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("snapshot reader blocked on {}", path.display());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    let regular = directory.path().join("regular.json");
    std::fs::write(
        &regular,
        r#"{"directed":false,"multigraph":false,"nodes":[],"links":[]}"#,
    )
    .unwrap();
    let regular_link = directory.path().join("regular-link.json");
    std::os::unix::fs::symlink(&regular, &regular_link).unwrap();
    for reader in [read_graphify, read_graphify_export] {
        assert!(reader(&regular).is_ok());
        assert!(reader(&regular_link).is_ok());
    }
    std::fs::write(&regular, r#"{"schema_version":1,"generation":0,"kind":"empty","root":null,"nodes":[],"edges":[],"metadata":null}"#).unwrap();
    assert!(graf::snapshot::read(&regular).is_ok());
    assert!(graf::snapshot::read(&regular_link).is_ok());
}

#[test]
fn groups_preserve_records_members_and_queries_through_graf_roundtrip_and_merge() {
    let groups = json!([
        {"id":"team", "label":"Review Team", "nodes":[7,"b",7], "relation":"participate_in", "extra":{"x":[1,2]}},
        {"label":"Singleton", "members":["b"]}
    ]);
    let input = json!({
        "directed":false, "multigraph":false,
        "nodes":[{"id":7},{"id":"b"},{"id":"graphify:group:0"}], "links":[],
        "hyperedges":groups, "graph":{"hyperedges":groups}
    });
    for reader in [read, read_export] {
        let graph = reader(input.clone()).unwrap();
        assert_eq!((graph.nodes.len(), graph.edges.len()), (5, 4));
        let group = graph
            .nodes
            .iter()
            .find(|n| n.label == "Review Team")
            .unwrap();
        assert_eq!(group.kind, "group");
        assert_ne!(group.id, "graphify:group:0");
        assert_eq!(group.metadata["graphify_group"], groups[0]);
        assert_eq!(graph.metadata["hyperedges"], groups);
        assert!(
            graph
                .edges
                .iter()
                .all(|e| !e.directed && e.relation == "member_of")
        );
        let group_id = group.id.clone();
        let directory = tempfile::tempdir().unwrap();
        let mut store = Store::create(&directory.path().join("groups.db")).unwrap();
        store.import_graph(graph).unwrap();
        let result = store
            .neighbors(&group_id, &QueryOptions::default())
            .unwrap();
        assert_eq!(result.edges.len(), 3); // duplicate membership retains its ordinal
        assert_eq!(result.nodes.len(), 3);
        assert!(
            store
                .query("Review Team", &QueryOptions::default())
                .unwrap()
                .nodes
                .iter()
                .any(|n| n.id == group_id)
        );
        let before = store.snapshot().unwrap();
        let mut file = NamedTempFile::new().unwrap();
        serde_json::to_writer(&mut file, &before).unwrap();
        let reread = graf::snapshot::read(file.path()).unwrap();
        assert_eq!(
            serde_json::to_value(&reread.nodes).unwrap(),
            serde_json::to_value(&before.nodes).unwrap()
        );
        assert_eq!(
            serde_json::to_value(&reread.edges).unwrap(),
            serde_json::to_value(&before.edges).unwrap()
        );
        let merged =
            graf::snapshot::merge(vec![("one".into(), before.clone()), ("two".into(), before)])
                .unwrap();
        let groups: Vec<_> = merged.nodes.iter().filter(|n| n.kind == "group").collect();
        assert_eq!(groups.len(), 4);
        for group in groups {
            let incidence: Vec<_> = merged
                .edges
                .iter()
                .filter(|e| e.target == group.id)
                .collect();
            assert!(!incidence.is_empty());
            for edge in incidence {
                let member = merged.nodes.iter().find(|n| n.id == edge.source).unwrap();
                assert_eq!(member.metadata["project"], group.metadata["project"]);
            }
        }
    }
    // Alternate producer slots are equivalent, including empty companions.
    let graph = read_export(json!({"nodes":[{"id":"x"}],"edges":[],"hyperedges":[],
        "graph":{"groups":[{"id":1,"nodes":["x"]}]}}))
    .unwrap();
    assert_eq!((graph.nodes.len(), graph.edges.len()), (2, 1));
    let record = json!({"node_ids":[{"id":"x","role":"lead"},7]});
    let graph =
        read_export(json!({"nodes":[{"id":"x"},{"id":7}],"edges":[],"groups":[record]})).unwrap();
    assert_eq!((graph.nodes.len(), graph.edges.len()), (3, 2));
    assert_eq!(graph.nodes[2].metadata["graphify_group"], record);
    assert_eq!(graph.edges[0].source, "x");
    assert_eq!(graph.edges[1].source, "graphify:integer:7");
}

#[test]
fn malformed_groups_fail_before_refresh_and_preserve_previous_graph() {
    let directory = tempfile::tempdir().unwrap();
    let mut store = Store::create(&directory.path().join("groups.db")).unwrap();
    store
        .import_graph(read_export(raw_export()).unwrap())
        .unwrap();
    let before = serde_json::to_value(store.snapshot().unwrap()).unwrap();
    for groups in [
        json!({}),
        json!([null]),
        json!([{}]),
        json!([{"nodes":[]}]),
        json!([{"nodes":"a"}]),
        json!([{"nodes":["missing"]}]),
        json!([{"nodes":[true]}]),
        json!([{"nodes":[{}]}]),
        json!([{"node_ids":[{"id":[]}]}]),
        json!([{"nodes":["a"],"node_ids":["b"]}]),
        json!([{"nodes":["a"],"id":false}]),
        json!([{"nodes":["a"],"members":["b"]}]),
        json!([{"nodes":["a"],"label":1}]),
        json!([{"nodes":["a"],"confidence_score":2}]),
        json!([{"nodes":["a"],"relation":false}]),
        json!([{"id":"x","nodes":["a"]},{"id":"x","nodes":["b"]}]),
    ] {
        let mut input = raw_export();
        input["hyperedges"] = groups;
        assert!(
            read_export(input)
                .and_then(|g| store.refresh_import(g))
                .is_err()
        );
        assert_eq!(
            serde_json::to_value(store.snapshot().unwrap()).unwrap(),
            before
        );
    }
    let mut conflict = raw_export();
    conflict["hyperedges"] = json!([{"nodes":["a"]}]);
    conflict["graph"] = json!({"hyperedges":[{"nodes":["b"]}]});
    assert!(read_export(conflict).is_err());
}
