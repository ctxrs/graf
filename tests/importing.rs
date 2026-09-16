use std::io::Write;

use graf::import::read_graphify;
use graf::model::ImportedGraph;
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
fn rejects_ambiguous_structure_and_hyperedges() {
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
}
