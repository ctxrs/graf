use graf::{
    model::*,
    query::{ImpactOptions, SearchOptions},
    store::Store,
};
use rusqlite::{Connection, params};
use serde_json::{Value, json};

#[path = "support/storage_legacy.rs"]
mod legacy;

const OLD_INDICES: &str = "
DROP INDEX IF EXISTS nodes_qualified;
CREATE INDEX nodes_qualified ON nodes(qualified_name, id);
DROP INDEX IF EXISTS nodes_binding;
CREATE INDEX nodes_binding ON nodes(binding_key, id);
DROP INDEX IF EXISTS nodes_owner;
CREATE INDEX nodes_owner ON nodes(owner_file);
DROP INDEX IF EXISTS edges_owner;
CREATE INDEX edges_owner ON edges(owner_file);
DROP INDEX IF EXISTS edges_source_direction;
CREATE INDEX edges_source_direction ON edges(source, directed, id);
DROP INDEX IF EXISTS edges_target_direction;
CREATE INDEX edges_target_direction ON edges(target, directed, id);
DROP INDEX IF EXISTS edges_source_direction_relation;
CREATE INDEX edges_source_direction_relation ON edges(source, directed, relation, id);
DROP INDEX IF EXISTS edges_target_direction_relation;
CREATE INDEX edges_target_direction_relation ON edges(target, directed, relation, id);
";

fn node(id: &str) -> Node {
    Node {
        id: id.into(),
        label: id.into(),
        kind: "function".into(),
        file: "fixture.py".into(),
        line: Some(1),
        end_line: Some(2),
        qualified_name: None,
        binding_key: None,
        metadata: json!({"evidence":"kept"}),
    }
}

fn edge(id: &str, source: &str, target: &str, directed: bool, relation: &str) -> Edge {
    Edge {
        id: id.into(),
        source: source.into(),
        target: target.into(),
        relation: relation.into(),
        directed,
        file: None,
        line: None,
        confidence: "EXTRACTED".into(),
        metadata: json!({"evidence":"kept"}),
    }
}

fn graph() -> ImportedGraph {
    let mut named = node("b");
    named.qualified_name = Some("module.b".into());
    named.binding_key = Some("binding:b".into());
    ImportedGraph {
        nodes: vec![node("a"), named, node("c")],
        edges: vec![
            edge("01-out", "a", "b", true, "calls"),
            edge("02-in", "c", "a", true, "calls"),
            edge("03-peer-out", "a", "c", false, "calls"),
            edge("04-peer-in", "b", "a", false, "calls"),
            edge("05-loop", "a", "a", true, "calls"),
            edge("06-peer-loop", "a", "a", false, "calls"),
            edge("07-relation", "b", "a", false, "uses"),
        ],
        metadata: json!({"fixture":"mixed"}),
    }
}

fn facts() -> FileFacts {
    let graph = graph();
    FileFacts {
        path: "fixture.py".into(),
        hash: "fixture".into(),
        module: "fixture".into(),
        nodes: graph.nodes,
        edges: graph.edges,
        references: vec![Reference {
            id: "ref".into(),
            source: "a".into(),
            label: "b".into(),
            relation: "references".into(),
            file: "fixture.py".into(),
            line: 1,
            candidate_keys: vec!["binding:b".into()],
            reason: String::new(),
        }],
        diagnostics: vec![],
    }
}

fn snapshot(store: &Store) -> anyhow::Result<Value> {
    Ok(serde_json::to_value(store.snapshot()?)?)
}

fn schema(conn: &Connection) -> rusqlite::Result<Vec<(String, Option<String>)>> {
    conn.prepare("SELECT name,sql FROM sqlite_master ORDER BY name")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect()
}

fn assert_layout(conn: &Connection) -> anyhow::Result<()> {
    for (table, index, columns, predicate) in [
        (
            "nodes",
            "nodes_qualified",
            "qualified_name,id",
            "qualified_name IS NOT NULL",
        ),
        (
            "nodes",
            "nodes_binding",
            "binding_key,id",
            "binding_key IS NOT NULL",
        ),
        ("nodes", "nodes_owner", "owner_key", "owner_key IS NOT NULL"),
        ("edges", "edges_owner", "owner_key", "owner_key IS NOT NULL"),
    ] {
        let sql: String = conn.query_row(
            "SELECT sql FROM sqlite_master WHERE name=?1",
            [index],
            |r| r.get(0),
        )?;
        assert!(sql.ends_with(&format!("WHERE {predicate}")), "{sql}");
        let actual = conn
            .prepare("SELECT name FROM pragma_index_info(?1) ORDER BY seqno")?
            .query_map([index], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?
            .join(",");
        assert_eq!(actual, columns);
        let partial: bool = conn.query_row(
            "SELECT partial FROM pragma_index_list(?1) WHERE name=?2",
            params![table, index],
            |r| r.get(0),
        )?;
        assert!(partial, "{index}");
    }
    assert_eq!(
        conn.pragma_query_value(None, "user_version", |r| r.get::<_, u32>(0))?,
        5
    );
    // General adjacency and ref_key's original uniqueness constraint stay intact.
    for (table, index, columns, partial) in [
        (
            "edges",
            "edges_source_relation",
            "source_key,relation,id",
            false,
        ),
        (
            "edges",
            "edges_target_relation",
            "target_key,relation,id",
            false,
        ),
        (
            "refs",
            "refs_unresolved_relation",
            "source_key,relation,id",
            true,
        ),
    ] {
        assert_eq!(
            conn.query_row(
                "SELECT partial FROM pragma_index_list(?1) WHERE name=?2",
                params![table, index],
                |r| r.get::<_, bool>(0)
            )?,
            partial,
        );
        let actual = conn
            .prepare("SELECT name FROM pragma_index_info(?1) ORDER BY seqno")?
            .query_map([index], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?
            .join(",");
        assert_eq!(actual, columns, "{index}");
    }
    for (index, columns) in [
        ("edges_source_direction", "source_key,id"),
        ("edges_target_direction", "target_key,id"),
        ("edges_source_direction_relation", "source_key,relation,id"),
        ("edges_target_direction_relation", "target_key,relation,id"),
    ] {
        let sql: String = conn.query_row(
            "SELECT sql FROM sqlite_master WHERE name=?1",
            [index],
            |r| r.get(0),
        )?;
        assert!(sql.ends_with("WHERE directed=0"), "{sql}");
        let actual = conn
            .prepare("SELECT name FROM pragma_index_info(?1) ORDER BY seqno")?
            .query_map([index], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?
            .join(",");
        assert_eq!(actual, columns, "{index}");
    }
    for obsolete in ["refs_unresolved_source", "edges_source", "edges_target"] {
        assert_eq!(
            conn.query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='index' AND name=?1",
                [obsolete],
                |row| row.get::<_, i64>(0)
            )?,
            0,
            "{obsolete}"
        );
    }
    let unique_ref: i64 = conn.query_row(
        "SELECT count(*) FROM pragma_index_list('edges') i JOIN pragma_index_info(i.name) c WHERE i.origin='u' AND c.name='ref_key'", [], |r| r.get(0),
    )?;
    assert_eq!(unique_ref, 1);
    Ok(())
}

fn plan(conn: &Connection, sql: &str, values: &[&str]) -> anyhow::Result<String> {
    Ok(conn
        .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))?
        .query_map(rusqlite::params_from_iter(values), |r| {
            r.get::<_, String>(3)
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .join("\n"))
}

fn assert_seek(plan: &str, index: &str) {
    assert!(plan.contains(index), "{index}: {plan}");
    assert!(
        !plan.contains("SCAN ") && !plan.contains("TEMP B-TREE"),
        "{plan}"
    );
}

fn assert_indexed(plan: &str, index: &str) {
    assert!(plan.contains(index), "{index}: {plan}");
    assert!(!plan.contains("SCAN "), "{plan}");
}

#[test]
fn fresh_and_imported_layout_omits_nulls_and_preserves_lookup_plans() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let db = temp.path().join("graph.db");
    let mut store = Store::create(&db)?;
    let conn = Connection::open(&db)?;
    assert_layout(&conn)?;
    store.import_graph(graph())?;
    assert_layout(&conn)?;
    for (table, index, predicate, expected) in [
        ("nodes", "nodes_qualified", "qualified_name IS NOT NULL", 1),
        ("nodes", "nodes_binding", "binding_key IS NOT NULL", 1),
        ("nodes", "nodes_owner", "owner_key IS NOT NULL", 0),
        ("edges", "edges_owner", "owner_key IS NOT NULL", 0),
        ("edges", "edges_source_direction", "directed=0", 4),
        ("edges", "edges_target_direction", "directed=0", 4),
        ("edges", "edges_source_direction_relation", "directed=0", 4),
        ("edges", "edges_target_direction_relation", "directed=0", 4),
    ] {
        let count: i64 = conn.query_row(
            &format!("SELECT count(*) FROM {table} INDEXED BY {index} WHERE {predicate}"),
            [],
            |r| r.get(0),
        )?;
        assert_eq!(count, expected, "{index}");
    }
    for (table, column, index, value) in [
        ("nodes", "qualified_name", "nodes_qualified", "module.b"),
        ("nodes", "binding_key", "nodes_binding", "binding:b"),
        ("nodes", "owner_key", "nodes_owner", "1"),
        ("edges", "owner_key", "edges_owner", "1"),
    ] {
        assert_seek(
            &plan(
                &conn,
                &format!("SELECT payload FROM {table} WHERE {column}=?1"),
                &[value],
            )?,
            index,
        );
    }
    for table in ["nodes", "edges"] {
        let index = format!("{table}_owner");
        let sql = format!(
            "SELECT payload FROM {table} WHERE owner_key=(SELECT fkey FROM files WHERE path=?1) OR owner_key IN (SELECT fkey FROM files WHERE path GLOB ?2)"
        );
        assert!(plan(&conn, &sql, &["fixture.py", "*.py"])?.contains(&index));
    }
    assert!(
        plan(
            &conn,
            "SELECT path FROM files WHERE EXISTS(SELECT 1 FROM nodes WHERE owner_key=files.fkey)",
            &[]
        )?
        .contains("nodes_owner")
    );
    assert_eq!(
        store
            .neighbors("module.b", &QueryOptions::default())?
            .nodes
            .len(),
        2
    );
    drop(conn);
    drop(store);
    assert_layout(&Connection::open(&db)?)?;
    Ok(())
}

#[test]
fn rare_undirected_streams_seek_past_directed_hubs_under_query_budget() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let db = temp.path().join("hub.db");
    let mut store = Store::create(&db)?;
    // Parallel edges keep the node/payload fixture small while exceeding the
    // query's 5,000-edge admission cap in both stored orientations.
    let mut edges = Vec::new();
    for i in 0..5_001 {
        edges.push(edge(
            &format!("out-{i:05}"),
            "out",
            "leaf",
            true,
            &format!("relation-{i:05}"),
        ));
        edges.push(edge(&format!("in-{i:05}"), "leaf", "in", true, "calls"));
    }
    edges.push(edge("000-first", "out", "peer", true, "uses"));
    edges.push(edge("rare-out", "out", "peer", false, "calls"));
    edges.push(edge("rare-in", "peer", "in", false, "calls"));
    store.import_graph(ImportedGraph {
        nodes: ["out", "in", "leaf", "peer"]
            .into_iter()
            .map(node)
            .collect(),
        edges,
        metadata: json!({}),
    })?;
    for (hub, direction, rare) in [
        ("out", Direction::Incoming, "rare-out"),
        ("in", Direction::Outgoing, "rare-in"),
    ] {
        for relation in [None, Some("calls".into())] {
            let options = QueryOptions {
                direction,
                relation,
                ..Default::default()
            };
            let result = store.neighbors(hub, &options)?;
            assert!(!result.truncated);
            assert_eq!(
                result
                    .edges
                    .iter()
                    .map(|e| e.id.as_str())
                    .collect::<Vec<_>>(),
                [rare]
            );
            let extended = store.neighbors_extended(
                hub,
                &SearchOptions {
                    graph: options,
                    ..Default::default()
                },
            )?;
            assert_eq!(extended.graph.edges.len(), 1);
            assert_eq!(extended.graph.edges[0].id, rare);
            assert!(!extended.graph.truncated);
        }
        // Normalized relation selection also uses the indexed distinct-name seek.
        let extended = store.neighbors_resolved(
            hub,
            &SearchOptions {
                graph: QueryOptions {
                    direction,
                    relation: Some("CALLS".into()),
                    ..Default::default()
                },
                ..Default::default()
            },
        )?;
        assert_eq!(extended.graph.edges[0].id, rare);
    }
    for (hub, direction) in [("out", Direction::Outgoing), ("in", Direction::Incoming)] {
        let result = store.neighbors(
            hub,
            &QueryOptions {
                direction,
                ..Default::default()
            },
        )?;
        assert!(result.truncated);
        assert_eq!(result.edges.len(), 5_000);
        if hub == "out" {
            assert_eq!(result.edges[0].id, "000-first");
            assert!(!result.edges.iter().any(|edge| edge.id == "out-04999"));
        }
    }
    let conn = Connection::open(&db)?;
    for column in ["source", "target"] {
        for undirected in [false, true] {
            let filter = if undirected { " AND directed=0" } else { "" };
            let stem = format!(
                "edges_{column}{}",
                if undirected { "_direction" } else { "" }
            );
            let unordered_index = if undirected {
                stem.clone()
            } else {
                format!("edges_{column}_relation")
            };
            let unordered_plan = plan(
                &conn,
                &format!(
                    "SELECT payload FROM edges{} WHERE {column}_key=?1{filter} ORDER BY id LIMIT 10",
                    if undirected {
                        format!(" INDEXED BY {stem}")
                    } else {
                        String::new()
                    }
                ),
                &["1"],
            )?;
            if undirected {
                assert_seek(&unordered_plan, &unordered_index);
            } else {
                assert_indexed(&unordered_plan, &unordered_index);
            }
            for suffix in [
                "AND relation=?2 ORDER BY id LIMIT 10",
                "AND relation>?2 ORDER BY relation LIMIT 1",
            ] {
                let relation_plan = plan(
                    &conn,
                    &format!(
                        "SELECT payload FROM edges{} WHERE {column}_key=?1{filter} {suffix}",
                        if undirected {
                            format!(" INDEXED BY {stem}_relation")
                        } else {
                            String::new()
                        }
                    ),
                    &["1", "calls"],
                )?;
                let index = if undirected {
                    format!("{stem}_relation")
                } else {
                    format!("edges_{column}_relation")
                };
                assert_seek(&relation_plan, &index);
            }
        }
    }
    assert_seek(
        &plan(&conn, "SELECT nkey FROM nodes WHERE id=?1", &["out"])?,
        "sqlite_autoindex_nodes_1",
    );
    assert_indexed(
        &plan(&conn, "SELECT rkey FROM refs WHERE source_key=?1", &["1"])?,
        "refs_source",
    );
    for sql in [
        "SELECT payload,resolution_reason FROM refs INDEXED BY refs_unresolved_relation WHERE source_key=?1 AND resolved_target_key IS NULL AND relation=?2 ORDER BY id LIMIT 10",
        "SELECT relation FROM refs INDEXED BY refs_unresolved_relation WHERE source_key=?1 AND resolved_target_key IS NULL AND relation>?2 ORDER BY relation LIMIT 1",
    ] {
        assert_seek(
            &plan(&conn, sql, &["1", "calls"])?,
            "refs_unresolved_relation",
        );
    }
    Ok(())
}

#[test]
fn open_rejects_an_incomplete_storage_counter_schema() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let db = temp.path().join("damaged.db");
    drop(Store::create(&db)?);
    Connection::open(&db)?.execute_batch("DROP TRIGGER storage_count_edges_delete")?;

    for error in [
        Store::open(&db).err().expect("damaged schema must fail"),
        Store::open_read_only(&db)
            .err()
            .expect("damaged schema must fail"),
    ] {
        assert!(
            error
                .to_string()
                .contains("incomplete Graf storage counters"),
            "{error:#}"
        );
    }
    Ok(())
}

#[test]
fn mixed_directions_relations_and_self_loops_keep_identity_and_order() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let db = temp.path().join("mixed.db");
    let mut store = Store::create(&db)?;
    store.import_graph(graph())?;
    for (direction, expected) in [
        (
            Direction::Outgoing,
            vec![
                "01-out",
                "03-peer-out",
                "05-loop",
                "06-peer-loop",
                "04-peer-in",
                "07-relation",
            ],
        ),
        (
            Direction::Incoming,
            vec![
                "02-in",
                "04-peer-in",
                "05-loop",
                "06-peer-loop",
                "07-relation",
                "03-peer-out",
            ],
        ),
        (
            Direction::Both,
            // Source stream first, then unseen target-stream edges; each
            // stream orders by ID, and repeated self-loops are deduplicated.
            vec![
                "01-out",
                "03-peer-out",
                "05-loop",
                "06-peer-loop",
                "02-in",
                "04-peer-in",
                "07-relation",
            ],
        ),
    ] {
        for relation in [None, Some("calls".into()), Some("uses".into())] {
            let options = QueryOptions {
                direction,
                relation: relation.clone(),
                ..Default::default()
            };
            let expected: Vec<_> = expected
                .iter()
                .copied()
                .filter(|id| match relation.as_deref() {
                    Some("calls") => *id != "07-relation",
                    Some("uses") => *id == "07-relation",
                    _ => true,
                })
                .collect();
            let result = store.neighbors("a", &options)?;
            assert!(!result.truncated);
            assert_eq!(
                result
                    .edges
                    .iter()
                    .map(|e| e.id.as_str())
                    .collect::<Vec<_>>(),
                expected
            );
            let extended = store.neighbors_extended(
                "a",
                &SearchOptions {
                    graph: options,
                    ..Default::default()
                },
            )?;
            assert_eq!(
                serde_json::to_value(&result.edges)?,
                serde_json::to_value(&extended.graph.edges)?
            );
            for edge in result.edges {
                let original = graph().edges.into_iter().find(|e| e.id == edge.id).unwrap();
                assert_eq!(serde_json::to_value(edge)?, serde_json::to_value(original)?);
            }
        }
    }
    let impact = store.impact_extended(
        "a",
        &ImpactOptions {
            relations: vec!["calls".into(), "uses".into()],
            ..Default::default()
        },
    )?;
    assert!(!impact.graph.truncated);
    assert_eq!(
        impact
            .graph
            .edges
            .iter()
            .map(|e| e.id.as_str())
            .collect::<Vec<_>>(),
        [
            "02-in",
            "04-peer-in",
            "05-loop",
            "06-peer-loop",
            "07-relation",
            "03-peer-out"
        ]
    );
    Ok(())
}

#[test]
fn old_native_layout_reads_unchanged_then_migrates_once_on_explicit_write() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let db = temp.path().join("old.db");
    let mut store = Store::create(&db)?;
    store.apply_native("repo", vec![facts()], vec![], Coverage::default())?;
    let before = snapshot(&store)?;
    drop(store);
    let conn = Connection::open(&db)?;
    legacy::restore_legacy(&conn, true)?;
    conn.execute_batch(OLD_INDICES)?;
    let old_schema = schema(&conn)?;
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")?;
    drop(conn);
    let bytes = std::fs::read(&db)?;
    for store in [
        Store::open_read_only(&db)?,
        Store::open(&db)?,
        Store::create(&db)?,
    ] {
        assert_eq!(snapshot(&store)?, before);
        assert_eq!(
            store
                .neighbors("module.b", &QueryOptions::default())?
                .nodes
                .len(),
            2
        );
    }
    assert_eq!(std::fs::read(&db)?, bytes);
    assert_eq!(schema(&Connection::open(&db)?)?, old_schema);
    let mut store = Store::open(&db)?;
    store.apply_native("repo", vec![], vec![], Coverage::default())?;
    assert_eq!(snapshot(&store)?, before);
    let conn = Connection::open(&db)?;
    assert_layout(&conn)?;
    let schema_cookie: i64 = conn.pragma_query_value(None, "schema_version", |r| r.get(0))?;
    store.apply_native("repo", vec![], vec![], Coverage::default())?;
    assert_eq!(
        conn.pragma_query_value(None, "schema_version", |r| r.get::<_, i64>(0))?,
        schema_cookie
    );
    assert_eq!(snapshot(&store)?, before);
    // Binding resolution and owner cascades still use non-NULL entries.
    store.apply_native("repo", vec![facts()], vec![], Coverage::default())?;
    assert!(
        store
            .snapshot()?
            .edges
            .iter()
            .any(|e| e.id == "reference:ref")
    );
    store.apply_native(
        "repo",
        vec![],
        vec!["fixture.py".into()],
        Coverage::default(),
    )?;
    assert_eq!(store.stats()?.nodes, 0);
    assert_eq!(store.stats()?.edges, 0);
    Ok(())
}

#[test]
fn failed_native_and_import_writes_roll_back_layout_and_graph() -> anyhow::Result<()> {
    for imported in [false, true] {
        let temp = tempfile::tempdir()?;
        let db = temp.path().join("rollback.db");
        let mut store = Store::create(&db)?;
        if imported {
            store.import_graph(graph())?;
        } else {
            store.apply_native("repo", vec![facts()], vec![], Coverage::default())?;
        }
        let before = snapshot(&store)?;
        let conn = Connection::open(&db)?;
        conn.execute_batch(
            &OLD_INDICES
                .replace("owner_file", "owner_key")
                .replace("(source,", "(source_key,")
                .replace("(target,", "(target_key,"),
        )?;
        let old_layout = schema(&conn)?;
        assert_eq!(snapshot(&Store::open_read_only(&db)?)?, before);
        assert_eq!(schema(&conn)?, old_layout);
        // Prove the failure happens after replacement, not during validation.
        conn.execute_batch("CREATE TRIGGER reject_insert BEFORE INSERT ON nodes BEGIN
            SELECT CASE WHEN EXISTS(SELECT 1 FROM pragma_index_list('edges') WHERE name='edges_source_direction')
                AND NOT EXISTS(SELECT 1 FROM pragma_index_list('edges') WHERE name='edges_source')
                AND EXISTS(SELECT 1 FROM pragma_index_list('edges') WHERE name='edges_source_relation')
                THEN RAISE(ABORT,'after index migration') ELSE RAISE(ABORT,'before index migration') END;
            END;")?;
        let old_schema = schema(&conn)?;
        let old_cookie: i64 = conn.pragma_query_value(None, "schema_version", |r| r.get(0))?;
        let mut changed = facts();
        changed.nodes[0].label.push_str(" changed");
        let error = if imported {
            store.refresh_import(graph()).unwrap_err()
        } else {
            store
                .apply_native("repo", vec![changed.clone()], vec![], Coverage::default())
                .unwrap_err()
        };
        assert!(
            format!("{error:#}").contains("after index migration"),
            "{error:#}"
        );
        assert_eq!(schema(&conn)?, old_schema);
        assert_eq!(
            conn.pragma_query_value(None, "schema_version", |r| r.get::<_, i64>(0))?,
            old_cookie
        );
        assert_eq!(snapshot(&store)?, before);
        conn.execute_batch("DROP TRIGGER reject_insert")?;
        if imported {
            store.refresh_import(graph())?;
        } else {
            store.apply_native("repo", vec![changed], vec![], Coverage::default())?;
        }
        assert_layout(&conn)?;
        let after = snapshot(&store)?;
        assert_eq!(
            after["generation"],
            json!(before["generation"].as_u64().unwrap() + 1)
        );
        if imported {
            let mut expected = before;
            expected["generation"] = after["generation"].clone();
            assert_eq!(after, expected);
        } else {
            assert!(
                after["nodes"].as_array().unwrap()[0]["label"]
                    .as_str()
                    .unwrap()
                    .contains("changed")
            );
        }
    }
    Ok(())
}
