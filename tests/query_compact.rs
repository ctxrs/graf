use graf::{
    model::*,
    query::{ImpactOptions, SearchOptions, Traversal},
    store::Store,
};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::path::Path;

#[path = "support/storage_legacy.rs"]
mod legacy;

fn node(id: &str, label: &str) -> Node {
    Node {
        id: id.into(),
        label: label.into(),
        kind: "function".into(),
        file: "fixture.rs".into(),
        line: Some(2),
        end_line: Some(4),
        qualified_name: Some(format!("fixture::{label}")),
        binding_key: Some(format!("binding:{id}")),
        metadata: json!({"description":"proseword", "evidence":{"literal":"é_%"}}),
    }
}

fn edge(id: &str, source: &str, target: &str, relation: &str, directed: bool) -> Edge {
    Edge {
        id: id.into(),
        source: source.into(),
        target: target.into(),
        relation: relation.into(),
        directed,
        file: Some("fixture.rs".into()),
        line: Some(3),
        confidence: "EXTRACTED".into(),
        metadata: json!({"original_metadata":{"context":relation},"evidence":"kept"}),
    }
}

fn reference(id: &str, relation: &str, candidate: &str) -> Reference {
    Reference {
        id: id.into(),
        source: "n-consumer".into(),
        label: id.into(),
        relation: relation.into(),
        file: "fixture.rs".into(),
        line: 3,
        candidate_keys: vec![candidate.into()],
        reason: "written call has no proven runtime target".into(),
    }
}

fn fixture() -> FileFacts {
    let mut root = node("z-root", "Container");
    root.kind = "class".into();
    let mut nodes = vec![
        root,
        node("n-consumer", "Consumer"),
        node("m-flow", "ﬂow"),
        node("a-member", "CaféMember()"),
        node("twin-z", "Shared"),
        node("twin-a", "Shared"),
    ];
    // The ordinary FTS query caps a rowid-ordered prefix. An upgrade must
    // preserve that prefix even when public lexical order is the reverse.
    nodes.extend(
        (0..25)
            .rev()
            .map(|i| node(&format!("token-{i:02}"), &format!("Tokenword{i:02}"))),
    );
    FileFacts {
        path: "fixture.rs".into(),
        hash: "fixture-v1".into(),
        module: "fixture".into(),
        nodes,
        edges: vec![
            edge("z-call", "n-consumer", "a-member", "calls", true),
            edge("a-call", "n-consumer", "m-flow", "calls", true),
            edge("member", "z-root", "a-member", "contains", true),
            edge("peer", "a-member", "n-consumer", "route_%", false),
            edge("loop", "n-consumer", "n-consumer", "references", true),
            edge("closure", "m-flow", "a-member", "uses_type", true),
        ],
        references: vec![
            reference("z-missing", "calls", "missing:z"),
            reference("a-missing", "calls", "missing:a"),
            reference("rare-ref", "route_%", "missing:rare"),
            reference("resolved", "declared_member", "binding:a-member"),
        ],
        diagnostics: vec![],
    }
}

fn seed(path: &Path) -> anyhow::Result<()> {
    Store::create(path)?.apply_native("repo", vec![fixture()], vec![], Coverage::default())?;
    Ok(())
}

fn reads(store: &Store) -> anyhow::Result<Value> {
    let mut results = vec![serde_json::to_value(store.snapshot()?)?];
    for direction in [Direction::Outgoing, Direction::Incoming, Direction::Both] {
        for relation in [None, Some("calls".into()), Some("route_%".into())] {
            let graph = QueryOptions {
                direction,
                relation,
                depth: 2,
                ..Default::default()
            };
            results.push(serde_json::to_value(
                store.neighbors("n-consumer", &graph)?,
            )?);
            results.push(serde_json::to_value(store.query("Consumer", &graph)?)?);
            results.push(serde_json::to_value(store.path(
                "n-consumer",
                "a-member",
                &graph,
            )?)?);
            for traversal in [Traversal::Bfs, Traversal::Dfs] {
                let options = SearchOptions {
                    graph: graph.clone(),
                    traversal,
                    induced_edges: true,
                    ..Default::default()
                };
                results.push(serde_json::to_value(
                    store.neighbors_extended("n-consumer", &options)?,
                )?);
                if traversal == Traversal::Bfs {
                    results.push(serde_json::to_value(store.path_extended(
                        "Consumer",
                        "fixture.rs::CaféMember()",
                        &options,
                    )?)?);
                }
            }
        }
    }
    for text in ["Tokenword", "proseword", "ﬂow"] {
        results.push(serde_json::to_value(
            store.query(text, &QueryOptions::default())?,
        )?);
        results.push(serde_json::to_value(
            store.query_extended(text, &SearchOptions::default())?,
        )?);
    }
    for text in [
        "a-member",
        "fixture::Consumer",
        "./fixture.rs::cafe",
        "FLOW",
    ] {
        results.push(serde_json::to_value(
            store.resolve_endpoint(text, &SearchOptions::default())?,
        )?);
    }
    for relation in ["CALLS", "_%", "declared", "missing-relation"] {
        results.push(serde_json::to_value(store.neighbors_resolved(
            "Consumer",
            &SearchOptions {
                graph: QueryOptions {
                    relation: Some(relation.into()),
                    ..Default::default()
                },
                ..Default::default()
            },
        )?)?);
    }
    for text in ["z-root", "a-member", "fixture.rs"] {
        for relations in [vec![], vec!["calls".into(), "declared_member".into()]] {
            results.push(serde_json::to_value(store.impact_extended(
                text,
                &ImpactOptions {
                    relations,
                    search: SearchOptions {
                        induced_edges: true,
                        ..Default::default()
                    },
                },
            )?)?);
        }
    }
    for options in [
        SearchOptions {
            graph: QueryOptions {
                limit: 1,
                ..Default::default()
            },
            ..Default::default()
        },
        SearchOptions {
            graph: QueryOptions {
                depth: 0,
                ..Default::default()
            },
            induced_edges: true,
            ..Default::default()
        },
        SearchOptions {
            files: vec!["fixture.rs".into()],
            kinds: vec!["function".into()],
            contexts: vec!["calls".into()],
            ..Default::default()
        },
    ] {
        results.push(serde_json::to_value(
            store.neighbors_extended("n-consumer", &options)?,
        )?);
    }
    for text in ["Shared", "absent.rs::Consumer", "missing"] {
        results.push(json!(
            store
                .resolve_endpoint(text, &SearchOptions::default())
                .unwrap_err()
                .to_string()
        ));
    }
    results.push(json!(
        store
            .neighbors_extended(
                "n-consumer",
                &SearchOptions {
                    token_budget: Some(1),
                    ..Default::default()
                }
            )
            .unwrap_err()
            .to_string()
    ));
    Ok(json!(results))
}

fn assert_order_and_evidence(store: &Store) -> anyhow::Result<()> {
    let result = store.neighbors(
        "n-consumer",
        &QueryOptions {
            direction: Direction::Outgoing,
            relation: Some("calls".into()),
            depth: 1,
            ..Default::default()
        },
    )?;
    assert_eq!(
        result
            .edges
            .iter()
            .map(|e| e.id.as_str())
            .collect::<Vec<_>>(),
        ["a-call", "z-call"]
    );
    assert_eq!(
        result
            .nodes
            .iter()
            .map(|n| n.id.as_str())
            .collect::<Vec<_>>(),
        ["n-consumer", "m-flow", "a-member"]
    );
    assert_eq!(
        result
            .unresolved
            .iter()
            .map(|r| r.label.as_str())
            .collect::<Vec<_>>(),
        ["a-missing", "z-missing"]
    );
    assert!(!result.truncated);
    assert_eq!(result.schema_version, 1);
    for e in result.edges {
        let expected = fixture()
            .edges
            .into_iter()
            .find(|original| original.id == e.id)
            .unwrap();
        assert_eq!(serde_json::to_value(e)?, serde_json::to_value(expected)?);
    }
    let impact = store.impact_extended("z-root", &ImpactOptions::default())?;
    assert_eq!(impact.seeds, ["z-root", "a-member"]);
    assert!(impact.graph.nodes.iter().any(|n| n.id == "n-consumer"));
    assert!(
        impact
            .graph
            .edges
            .iter()
            .any(|e| e.relation == "declared_member")
    );
    let fts = store.query(
        "Tokenword",
        &QueryOptions {
            depth: 0,
            ..Default::default()
        },
    )?;
    assert!(fts.truncated);
    assert_eq!(fts.nodes.len(), 20);
    assert_eq!(fts.nodes[0].id, "token-24");
    assert_eq!(fts.nodes[19].id, "token-05");
    Ok(())
}

#[test]
fn compact_and_legacy_queries_preserve_full_results_order_errors_and_read_bytes()
-> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let compact = dir.path().join("compact.db");
    let old = dir.path().join("legacy.db");
    let packed = dir.path().join("packed-legacy.db");
    seed(&compact)?;
    seed(&old)?;
    seed(&packed)?;
    legacy::restore_legacy(&Connection::open(&old)?, false)?;
    legacy::restore_legacy(&Connection::open(&packed)?, true)?;
    let expected = reads(&Store::open_read_only(&old)?)?;
    for (path, version) in [(&old, 1), (&packed, 1), (&compact, 2)] {
        let conn = Connection::open(path)?;
        assert_eq!(
            conn.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))?,
            version
        );
        conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")?;
        drop(conn);
        let before = std::fs::read(path)?;
        let reader = Store::open_read_only(path)?;
        assert_order_and_evidence(&reader)?;
        assert_eq!(reads(&reader)?, expected);
        drop(reader);
        assert_eq!(std::fs::read(path)?, before);
    }
    Ok(())
}

#[test]
fn existing_read_handle_switches_layout_after_physical_upgrade_without_generation_change()
-> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("upgrade.db");
    seed(&path)?;
    legacy::restore_legacy(&Connection::open(&path)?, true)?;
    let reader = Store::open_read_only(&path)?;
    let expected = reads(&reader)?;
    let generation = reader.stats()?.generation;
    let conn = Connection::open(&path)?;
    let tx = conn.unchecked_transaction()?;
    let rowids = tx
        .prepare("SELECT rowid,id FROM nodes ORDER BY rowid")?
        .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    assert_eq!(
        tx.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))?,
        1
    );
    Store::open(&path)?.apply_native("repo", vec![], vec![], Coverage::default())?;
    // An already-pinned old snapshot remains valid, including its old columns.
    assert_eq!(
        tx.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))?,
        1
    );
    assert_eq!(
        tx.query_row("SELECT source FROM edges WHERE id='a-call'", [], |r| r
            .get::<_, String>(
            0
        ))?,
        "n-consumer"
    );
    tx.commit()?;
    assert_eq!(
        conn.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))?,
        2
    );
    let keys = conn
        .prepare("SELECT nkey,id FROM nodes ORDER BY nkey")?
        .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    assert_eq!(keys, rowids);
    assert_eq!(reader.stats()?.generation, generation);
    assert_order_and_evidence(&reader)?;
    assert_eq!(reads(&reader)?, expected);
    Ok(())
}

fn rare_reads(store: &Store) -> anyhow::Result<Value> {
    let mut results = vec![];
    for (id, direction, expected) in [
        ("n-consumer", Direction::Incoming, "rare-out"),
        ("z-hub", Direction::Outgoing, "rare-in"),
    ] {
        for relation in [None, Some("rare_relation".into())] {
            let options = QueryOptions {
                direction,
                relation,
                ..Default::default()
            };
            let result = store.neighbors(id, &options)?;
            assert!(!result.truncated);
            assert_eq!(result.edges.len(), 1);
            assert_eq!(result.edges[0].id, expected);
            results.push(serde_json::to_value(result)?);
        }
    }
    let options = SearchOptions {
        graph: QueryOptions {
            direction: Direction::Outgoing,
            relation: Some("rare_relation".into()),
            ..Default::default()
        },
        induced_edges: true,
        ..Default::default()
    };
    let result = store.neighbors_extended("n-consumer", &options)?;
    assert!(!result.graph.truncated);
    assert_eq!(result.graph.edges.len(), 1);
    assert_eq!(result.graph.edges[0].id, "rare-out");
    assert_eq!(result.graph.unresolved.len(), 1);
    assert_eq!(result.graph.unresolved[0].label, "zz-rare-ref");
    results.push(serde_json::to_value(result)?);
    let resolved = store.neighbors_resolved(
        "Consumer",
        &SearchOptions {
            graph: QueryOptions {
                relation: Some("RARE_RELATION".into()),
                ..options.graph.clone()
            },
            ..options
        },
    )?;
    assert_eq!(resolved.graph.unresolved.len(), 1);
    assert!(!resolved.graph.truncated);
    results.push(serde_json::to_value(resolved)?);
    let impact = store.impact_extended(
        "z-hub",
        &ImpactOptions {
            relations: vec!["rare_relation".into(), "declared_member".into()],
            ..Default::default()
        },
    )?;
    assert!(!impact.graph.truncated);
    assert_eq!(impact.graph.edges.len(), 1);
    assert_eq!(impact.graph.edges[0].id, "rare-in");
    results.push(serde_json::to_value(impact)?);
    // Removing the rare filter still observes the original admission bound.
    let crowded = store.neighbors(
        "z-hub",
        &QueryOptions {
            direction: Direction::Incoming,
            relation: Some("calls".into()),
            ..Default::default()
        },
    )?;
    assert!(crowded.truncated);
    assert_eq!(crowded.edges.len(), 5_000);
    assert_eq!(crowded.edges[0].id, "edge-00000");
    assert_eq!(crowded.edges[4_999].id, "edge-04999");
    results.push(serde_json::to_value(crowded)?);
    Ok(json!(results))
}

#[test]
fn compact_and_legacy_rare_direction_relation_and_unresolved_streams_keep_bounds()
-> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("rare.db");
    let mut facts = fixture();
    facts.nodes = vec![
        node("z-hub", "Hub"),
        node("n-consumer", "Consumer"),
        node("a-peer", "Peer"),
    ];
    facts.edges = (0..5_001)
        .rev()
        .map(|i| {
            edge(
                &format!("edge-{i:05}"),
                "n-consumer",
                "z-hub",
                "calls",
                true,
            )
        })
        .collect();
    facts.edges.extend([
        edge("rare-out", "n-consumer", "a-peer", "rare_relation", false),
        edge("rare-in", "a-peer", "z-hub", "rare_relation", false),
    ]);
    facts.references = (0..5_001)
        .rev()
        .map(|i| reference(&format!("ref-{i:05}"), "calls", "missing"))
        .collect();
    facts
        .references
        .push(reference("zz-rare-ref", "rare_relation", "missing"));
    Store::create(&path)?.apply_native("repo", vec![facts], vec![], Coverage::default())?;
    let compact = rare_reads(&Store::open_read_only(&path)?)?;
    legacy::restore_legacy(&Connection::open(&path)?, true)?;
    assert_eq!(rare_reads(&Store::open_read_only(&path)?)?, compact);
    Ok(())
}

#[test]
fn an_existing_reader_rejects_an_unsupported_new_physical_version() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("future.db");
    seed(&path)?;
    let reader = Store::open_read_only(&path)?;
    reader.resolve_endpoint("n-consumer", &SearchOptions::default())?;
    let sql = Connection::open(&path)?;
    sql.pragma_update(None, "user_version", 3)?;
    for error in [
        reader
            .query("n-consumer", &QueryOptions::default())
            .unwrap_err(),
        reader
            .resolve_endpoint("n-consumer", &SearchOptions::default())
            .unwrap_err(),
        reader
            .query_extended("Consumer", &SearchOptions::default())
            .unwrap_err(),
    ] {
        assert!(
            error
                .to_string()
                .contains("unsupported Graf storage version"),
            "{error:#}"
        );
    }
    sql.pragma_update(None, "user_version", 2)?;
    assert_order_and_evidence(&reader)?;
    Ok(())
}

#[test]
fn previous_writer_fixture_reads_unchanged_then_upgrades_on_the_same_handle() -> anyhow::Result<()>
{
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("previous-writer.db");
    let sql = Connection::open(&path)?;
    sql.execute_batch(include_str!("fixtures/native-format1.sql"))?;
    drop(sql);
    let bytes = std::fs::read(&path)?;
    let reader = Store::open_read_only(&path)?;
    let capture = |store: &Store| -> anyhow::Result<Value> {
        let options = SearchOptions {
            graph: QueryOptions {
                direction: Direction::Outgoing,
                relation: Some("calls".into()),
                ..Default::default()
            },
            induced_edges: true,
            ..Default::default()
        };
        let caller = store.neighbors("caller", &options.graph)?;
        assert_eq!(caller.edges.len(), 1);
        assert_eq!(caller.edges[0].source, "python:consumer.py:caller@38");
        assert_eq!(caller.edges[0].target, "python:provider.py:target@0");
        let unknown = store.neighbors_resolved(
            "unknown",
            &SearchOptions {
                graph: QueryOptions {
                    relation: Some("CALL".into()),
                    ..options.graph.clone()
                },
                ..options.clone()
            },
        )?;
        assert!(unknown.graph.edges.is_empty());
        assert_eq!(unknown.graph.unresolved.len(), 1);
        assert_eq!(unknown.graph.unresolved[0].label, "missing");
        assert_eq!(
            unknown.graph.unresolved[0].reason,
            "dynamic, shadowed, or uncertain Python binding"
        );
        Ok(json!([
            store.snapshot()?,
            caller,
            unknown,
            store.query("targ", &QueryOptions::default())?,
            store.query_extended("targ", &SearchOptions::default())?,
            store.resolve_endpoint("consumer.py::CALLER", &options)?,
            store.neighbors_extended("caller", &options)?,
            store.path("caller", "target", &options.graph)?,
            store.path_extended("caller", "target", &options)?,
            store.impact_extended("provider.py", &ImpactOptions::default())?,
        ]))
    };
    let before = capture(&reader)?;
    assert_eq!(std::fs::read(&path)?, bytes);
    let stats = reader.stats()?;
    assert_eq!(
        (
            stats.generation,
            stats.nodes,
            stats.edges,
            stats.unresolved_references
        ),
        (1, 5, 6, 1)
    );
    Store::open(&path)?.apply_native("fixture", vec![], vec![], stats.coverage)?;
    assert_eq!(reader.stats()?.generation, 1);
    assert_eq!(capture(&reader)?, before);
    assert_eq!(
        Connection::open(&path)?
            .pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))?,
        2
    );
    Ok(())
}
