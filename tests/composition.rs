use graf::{
    composition::link_references,
    languages::compiled,
    model::{Coverage, Edge, GraphSnapshot, ImportedGraph, Node, Reference, SCHEMA_VERSION},
    snapshot,
    store::Store,
};
use serde_json::{Value, json};

fn source_snapshot(path: &str, source: &str) -> GraphSnapshot {
    let facts = compiled::parse(path, source, "fixture").unwrap().unwrap();
    assert!(
        facts.diagnostics.is_empty(),
        "{path}: {:?}",
        facts.diagnostics
    );
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::create(&dir.path().join("graph.db")).unwrap();
    store
        .apply_native("fixture", vec![facts], vec![], Coverage::default())
        .unwrap();
    store.snapshot().unwrap()
}

fn generated(graph: &ImportedGraph) -> Vec<&Edge> {
    graph
        .edges
        .iter()
        .filter(|e| e.metadata.get("graf_composition").is_some())
        .collect()
}

fn state(graph: &ImportedGraph) -> Value {
    json!({"nodes": graph.nodes, "edges": graph.edges, "metadata": graph.metadata})
}

fn declaration(id: &str, qualified: &str, kind: &str) -> Node {
    Node {
        id: id.into(),
        label: qualified.rsplit('.').next().unwrap().into(),
        kind: kind.into(),
        file: if id == "caller" {
            "Caller.java"
        } else {
            "Library.java"
        }
        .into(),
        line: Some(1),
        end_line: Some(3),
        qualified_name: Some(qualified.into()),
        binding_key: Some(format!("java:symbol:{qualified}")),
        metadata: json!({"language":"java", "qualified_symbol":qualified,
            "cross_project_public":true,"dynamic_dispatch":false,"static":true,
            "binding_aliases":[format!("java:static:{qualified}")]}),
    }
}

fn reference(id: &str, key: &str) -> Reference {
    Reference {
        id: id.into(),
        source: "caller".into(),
        label: "Tools.run".into(),
        relation: "calls".into(),
        file: "Caller.java".into(),
        line: 7,
        candidate_keys: vec![key.into()],
        reason: "external target".into(),
    }
}

fn graph_snapshot(nodes: Vec<Node>, refs: Vec<Reference>) -> GraphSnapshot {
    GraphSnapshot {
        schema_version: SCHEMA_VERSION,
        generation: 1,
        kind: "native".into(),
        root: Some("fixture".into()),
        nodes,
        edges: vec![],
        metadata: json!({"graf_unresolved_references":refs}),
    }
}

fn fixture(mut targets: Vec<Node>, refs: Vec<Reference>) -> ImportedGraph {
    targets.push(declaration("owner", "api.Tools", "class"));
    snapshot::merge(vec![
        (
            "application".into(),
            graph_snapshot(
                vec![declaration("caller", "app.Client.run", "method")],
                refs,
            ),
        ),
        ("library".into(), graph_snapshot(targets, vec![])),
    ])
    .unwrap()
}

#[test]
fn syntax_backed_same_language_calls_and_types_cross_project_boundaries() {
    for (library_path, library, caller_path, caller, callable) in [
        (
            "Engine.java",
            "package api; public final class Engine { public static void ping() {} public void step() {} }",
            "Client.java",
            "package client; import api.Engine; public class Client { public void run(Engine value) { Engine.ping(); value.step(); } }",
            "ping",
        ),
        (
            "Engine.cs",
            "namespace api; public class Engine { public static void Ping() {} public void step() {} }",
            "Client.cs",
            "using E = api.Engine; namespace client; public class Client { public void Run(E value) { E.Ping(); value.step(); } }",
            "Ping",
        ),
        (
            "Engine.kt",
            "package api\nclass Engine {\n fun step() {}\n}\nfun ping() {}\n",
            "Client.kt",
            "package client\nimport api.Engine\nimport api.ping\nfun run(value: Engine) { ping(); value.step() }\n",
            "ping",
        ),
        (
            "engine.cpp",
            "namespace api { struct Engine { void step() {} }; void ping() {} }",
            "client.cpp",
            "void run(api::Engine value) { api::ping(); value.step(); }",
            "ping",
        ),
    ] {
        let library = source_snapshot(library_path, library);
        let caller = source_snapshot(caller_path, caller);
        assert!(
            !caller.metadata["graf_unresolved_references"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        let mut graph =
            snapshot::merge(vec![("lib".into(), library), ("app".into(), caller)]).unwrap();
        let before_nodes = serde_json::to_value(&graph.nodes).unwrap();
        let before_metadata = graph.metadata.clone();
        assert!(link_references(&mut graph).unwrap() >= 3, "{library_path}");
        let links = generated(&graph);
        for name in [callable, "step"] {
            let target = graph
                .nodes
                .iter()
                .find(|n| n.metadata["project"] == "lib" && n.label == name)
                .unwrap();
            assert!(
                links
                    .iter()
                    .any(|e| e.relation == "calls" && e.target == target.id),
                "{library_path}: {name}"
            );
        }
        let target = graph
            .nodes
            .iter()
            .find(|n| n.metadata["project"] == "lib" && n.label == "Engine")
            .unwrap();
        assert!(
            links.iter().any(
                |e| matches!(e.relation.as_str(), "references" | "uses_type")
                    && e.target == target.id
            ),
            "{library_path}: type"
        );
        assert_eq!(before_nodes, serde_json::to_value(&graph.nodes).unwrap());
        assert_eq!(before_metadata, graph.metadata);
        for edge in links {
            assert!(edge.directed);
            assert_eq!(edge.confidence, "INFERRED");
            let evidence = &edge.metadata["graf_composition"];
            assert_eq!(evidence["source_project"], "app");
            assert_eq!(evidence["target_project"], "lib");
            assert_eq!(edge.file.as_deref(), Some(caller_path));
            assert_eq!(json!(edge.line), evidence["reference"]["line"]);
        }
    }
}

#[test]
fn unique_member_is_retracted_when_its_parsed_owner_becomes_ambiguous() {
    let mut graph = snapshot::merge(vec![
        (
            "application".into(),
            source_snapshot(
                "Client.java",
                "package app; import api.Engine; public class Client { public void run() { Engine.ping(); } }",
            ),
        ),
        (
            "library-one".into(),
            source_snapshot(
                "Engine.java",
                "package api; public class Engine { public static void ping() {} }",
            ),
        ),
    ])
    .unwrap();
    let count = link_references(&mut graph).unwrap();
    assert!(generated(&graph).iter().any(|e| e.relation == "calls"));
    assert!(generated(&graph).iter().any(|e| e.relation == "imports"));
    let before = state(&graph);

    // Only the receiver type collides: the competing library has no ping.
    let competitor = snapshot::merge(vec![(
        "library-two".into(),
        source_snapshot(
            "Engine.java",
            "package api; public class Engine { public static void pong() {} }",
        ),
    )])
    .unwrap();
    let competitor_ids: std::collections::BTreeSet<_> =
        competitor.nodes.iter().map(|n| n.id.clone()).collect();
    graph.nodes.extend(competitor.nodes);
    graph.edges.extend(competitor.edges);
    graph.metadata["projects"]
        .as_array_mut()
        .unwrap()
        .extend(competitor.metadata["projects"].as_array().unwrap().clone());
    link_references(&mut graph).unwrap();
    assert!(!generated(&graph).iter().any(|e| e.relation == "calls"));
    assert!(!generated(&graph).iter().any(|e| e.relation == "imports"));

    graph.nodes.retain(|n| !competitor_ids.contains(&n.id));
    graph
        .edges
        .retain(|e| !competitor_ids.contains(&e.source) && !competitor_ids.contains(&e.target));
    graph.metadata["projects"]
        .as_array_mut()
        .unwrap()
        .retain(|p| p["name"] != "library-two");
    assert_eq!(link_references(&mut graph).unwrap(), count);
    assert_eq!(state(&graph), before);

    // A surviving method is insufficient when its owning type disappears.
    let owner = graph
        .nodes
        .iter()
        .find(|n| n.binding_key.as_deref() == Some("java:symbol:api.Engine"))
        .unwrap()
        .clone();
    graph.nodes.retain(|n| n.id != owner.id);
    let membership: Vec<_> = graph
        .edges
        .iter()
        .filter(|e| {
            e.metadata.get("graf_composition").is_none()
                && (e.source == owner.id || e.target == owner.id)
        })
        .cloned()
        .collect();
    graph.edges.retain(|e| {
        e.metadata.get("graf_composition").is_some()
            || (e.source != owner.id && e.target != owner.id)
    });
    link_references(&mut graph).unwrap();
    assert!(!generated(&graph).iter().any(|e| e.relation == "calls"));
    graph.nodes.push(owner);
    graph.edges.extend(membership);
    assert_eq!(link_references(&mut graph).unwrap(), count);
    assert!(generated(&graph).iter().any(|e| e.relation == "calls"));
}

#[test]
fn method_key_families_require_public_matching_owner_in_the_target_project() {
    for (family, relation) in [
        ("symbol", "calls"),
        ("symbol", "imports"),
        ("static", "calls"),
        ("member", "calls"),
    ] {
        let key = format!("java:{family}:api.Tools.run");
        let mut target = declaration("target", "api.Tools.run", "method");
        target.metadata["static"] = json!(family != "member");
        target.metadata["binding_aliases"] = json!([key]);
        let mut site = reference("site", &key);
        site.relation = relation.into();
        let mut ordinary = fixture(vec![target], vec![site]);
        assert_eq!(link_references(&mut ordinary).unwrap(), 1, "{key}");
        for problem in 0..7 {
            let mut graph = ordinary.clone();
            let owner = graph
                .nodes
                .iter_mut()
                .find(|n| n.metadata["original_id"] == "owner")
                .unwrap();
            match problem {
                0 => {
                    let id = owner.id.clone();
                    graph.nodes.retain(|n| n.id != id);
                }
                1 => owner.metadata["original_metadata"]["cross_project_public"] = json!(false),
                2 => {
                    owner.metadata["original_metadata"]
                        .as_object_mut()
                        .unwrap()
                        .remove("cross_project_public");
                }
                3 => owner.kind = "function".into(),
                4 => owner.qualified_name = Some("other.Tools".into()),
                5 => owner.metadata["original_metadata"]["qualified_symbol"] = json!("other.Tools"),
                6 => {
                    owner.metadata["project"] = json!("application");
                    owner.id = format!("graf:project:{}", json!(["application", "owner"]));
                }
                _ => unreachable!(),
            }
            assert_eq!(
                link_references(&mut graph).unwrap(),
                0,
                "{key} {relation}: owner problem {problem}"
            );
        }
    }
}

#[test]
fn duplicate_owner_in_any_project_blocks_the_member_and_candidate_fallback() {
    let mut site = reference("site", "java:static:api.Tools.run");
    site.candidate_keys.push("java:symbol:api.fallback".into());
    let mut ordinary = fixture(
        vec![
            declaration("target", "api.Tools.run", "method"),
            declaration("fallback", "api.fallback", "function"),
        ],
        vec![site],
    );
    assert_eq!(link_references(&mut ordinary).unwrap(), 1);
    for project in ["application", "library"] {
        for public in [true, false] {
            let mut graph = ordinary.clone();
            let mut duplicate = graph
                .nodes
                .iter()
                .find(|n| n.metadata["original_id"] == "owner")
                .unwrap()
                .clone();
            duplicate.metadata["project"] = json!(project);
            duplicate.metadata["original_id"] = json!("competing-owner");
            duplicate.metadata["original_metadata"]["cross_project_public"] = json!(public);
            duplicate.id = format!("graf:project:{}", json!([project, "competing-owner"]));
            graph.nodes.push(duplicate);
            assert_eq!(
                link_references(&mut graph).unwrap(),
                0,
                "{project} {public}"
            );
        }
    }
    // The fallback really is eligible; an ambiguous owner must prevent reaching it.
    ordinary.metadata["projects"][0]["metadata"]["graf_unresolved_references"][0]["candidate_keys"] =
        json!(["java:symbol:api.fallback"]);
    assert_eq!(link_references(&mut ordinary).unwrap(), 1);
    let target = generated(&ordinary)[0].target.clone();
    assert!(
        ordinary
            .nodes
            .iter()
            .any(|n| n.id == target && n.label == "fallback")
    );
}

#[test]
fn wrong_namespace_private_overloaded_and_dynamic_declarations_do_not_answer_calls() {
    for (path, declaration_source, caller_source) in [
        (
            "Wrong.java",
            "package other; public final class Engine { public static void ping() {} }",
            "package app; import api.Engine; class Client { void run() { Engine.ping(); } }",
        ),
        (
            "Private.java",
            "package api; class Engine { public static void ping() {} }",
            "package app; import api.Engine; class Client { void run() { Engine.ping(); } }",
        ),
        (
            "Overload.java",
            "package api; public class Engine { public static void ping(int x) {} public static void ping(String x) {} }",
            "package app; import api.Engine; class Client { void run() { Engine.ping(1); } }",
        ),
        (
            "Dynamic.java",
            "package api; public class Engine { public void step() {} }",
            "package app; import api.Engine; class Client { void run(Engine value) { value.step(); } }",
        ),
    ] {
        let mut graph = snapshot::merge(vec![
            ("app".into(), source_snapshot("Client.java", caller_source)),
            ("lib".into(), source_snapshot(path, declaration_source)),
        ])
        .unwrap();
        link_references(&mut graph).unwrap();
        assert!(
            !generated(&graph).iter().any(|e| e.relation == "calls"),
            "{path}"
        );
    }
}

#[test]
fn proof_language_qualified_identity_and_dispatch_are_required() {
    let target = declaration("target", "api.Tools.run", "method");
    let site = reference("site", "java:static:api.Tools.run");
    for (field, value) in [
        ("cross_project_public", Value::Null),
        ("cross_project_public", json!(false)),
        ("cross_project_public", json!("true")),
        ("language", json!("csharp")),
        ("qualified_symbol", json!("other.Tools.run")),
        ("dynamic_dispatch", json!(true)),
        ("dynamic_dispatch", Value::Null),
        ("static", json!(false)),
    ] {
        let mut target = target.clone();
        if value.is_null() {
            target.metadata.as_object_mut().unwrap().remove(field);
        } else {
            target.metadata[field] = value;
        }
        let mut graph = fixture(vec![target], vec![site.clone()]);
        assert_eq!(link_references(&mut graph).unwrap(), 0, "{field}");
    }
    let mut wrong_name = target.clone();
    wrong_name.qualified_name = Some("other.Tools.run".into());
    assert_eq!(
        link_references(&mut fixture(vec![wrong_name], vec![site.clone()])).unwrap(),
        0
    );
    let mut graph = fixture(vec![target], vec![site]);
    graph.nodes[0].metadata["original_metadata"]["language"] = json!("kotlin");
    assert_eq!(link_references(&mut graph).unwrap(), 0);
}

#[test]
fn only_stable_namespace_keys_are_admitted_and_ambiguity_blocks_fallback() {
    for key in [
        "java:symbol:run",
        "java:symbol:@Library.java.api.Tools.run",
        "java:file:Library.java",
        "cpp:symbol:@unit.cpp.Tools.run",
        "c-cpp:header-symbol:api.h:run",
        "python:api:Tools.run",
        "javascript:export:api:run",
        "rust:package:.:api:run",
        "swift:symbol:export:3:Api:run",
        "kotlin:symbol:!Api.run",
        "java:symbol:api..run",
    ] {
        let mut target = declaration("target", "api.Tools.run", "method");
        target.binding_key = Some(key.into());
        target.metadata["binding_aliases"] = json!([key]);
        let mut graph = fixture(vec![target], vec![reference("site", key)]);
        assert_eq!(link_references(&mut graph).unwrap(), 0, "{key}");
    }
    let first = declaration("first", "api.Tools.run", "method");
    let second = declaration("second", "api.Tools.run", "method");
    let fallback = declaration("fallback", "backup.Tools.run", "method");
    let mut site = reference("site", "java:static:api.Tools.run");
    site.candidate_keys
        .push("java:static:backup.Tools.run".into());
    let mut graph = fixture(
        vec![
            first,
            second,
            fallback,
            declaration("fallback-owner", "backup.Tools", "class"),
        ],
        vec![site],
    );
    assert_eq!(link_references(&mut graph).unwrap(), 0);
    // A local match cannot be displaced by a foreign declaration or fallback.
    let local = declaration("local", "api.Tools.run", "method");
    let mut graph = snapshot::merge(vec![(
        "app".into(),
        graph_snapshot(
            vec![declaration("caller", "app.Client.run", "method"), local],
            vec![reference("site", "java:static:api.Tools.run")],
        ),
    )])
    .unwrap();
    assert_eq!(link_references(&mut graph).unwrap(), 0);
}

#[test]
fn recomputation_preserves_parallel_sites_source_edges_and_removes_deleted_targets() {
    let mut target = declaration("target", "api.Tools.run", "method");
    target.metadata["binding_aliases"] =
        json!(["java:static:api.Tools.run", "java:static:api.Tools.run"]);
    let first = reference("site-one", "java:static:api.Tools.run");
    let mut second = reference("site-two", "java:static:api.Tools.run");
    second.line = 12;
    let mut graph = fixture(vec![target], vec![first, second]);
    let target = graph.nodes[1].clone();
    let existing = Edge {
        id: "source-edge".into(),
        source: graph.nodes[0].id.clone(),
        target: target.id.clone(),
        relation: "calls".into(),
        directed: false,
        file: None,
        line: None,
        confidence: "EXTRACTED".into(),
        metadata: json!({"original":true}),
    };
    graph.edges.push(existing.clone());
    assert_eq!(link_references(&mut graph).unwrap(), 2);
    assert_eq!(graph.edges.len(), 3);
    assert_eq!(
        serde_json::to_value(&graph.edges[0]).unwrap(),
        serde_json::to_value(existing).unwrap()
    );
    assert_eq!(
        generated(&graph)
            .iter()
            .map(|e| e.line.unwrap())
            .collect::<Vec<_>>(),
        [7, 12]
    );
    let before = state(&graph);
    assert_eq!(link_references(&mut graph).unwrap(), 2);
    assert_eq!(state(&graph), before);
    graph.nodes.retain(|n| n.id != target.id);
    graph.edges.retain(|e| e.id != "source-edge");
    assert_eq!(link_references(&mut graph).unwrap(), 0);
    assert!(graph.edges.is_empty());
    let mut replacement = target;
    replacement.metadata["original_id"] = json!("replacement");
    replacement.id = format!("graf:project:{}", json!(["library", "replacement"]));
    let replacement_id = replacement.id.clone();
    graph.nodes.push(replacement);
    assert_eq!(link_references(&mut graph).unwrap(), 2);
    assert!(generated(&graph).iter().all(|e| e.target == replacement_id));
}

#[test]
fn invalid_provenance_references_and_id_collisions_are_atomic() {
    let target = declaration("target", "api.Tools.run", "method");
    let mut graph = fixture(
        vec![target],
        vec![reference("site", "java:static:api.Tools.run")],
    );
    assert_eq!(link_references(&mut graph).unwrap(), 1);
    for problem in 0..6 {
        let mut invalid = graph.clone();
        match problem {
            0 => {
                invalid.nodes[1].metadata["original_id"] = json!("wrong");
            }
            1 => {
                invalid.metadata["projects"][0]["metadata"]["graf_unresolved_references"] =
                    json!({});
            }
            2 => {
                invalid.metadata["projects"][0]["metadata"]["graf_unresolved_references"][0]["source"] =
                    json!("missing");
            }
            3 => {
                invalid.metadata["projects"][0]["metadata"]["graf_unresolved_references"][0]["file"] =
                    json!("wrong.java");
            }
            4 => {
                let duplicate =
                    invalid.metadata["projects"][0]["metadata"]["graf_unresolved_references"][0]
                        .clone();
                invalid.metadata["projects"][0]["metadata"]["graf_unresolved_references"]
                    .as_array_mut()
                    .unwrap()
                    .push(duplicate);
            }
            5 => {
                invalid.edges[0].metadata = json!({"source_owned":true});
            }
            _ => unreachable!(),
        }
        let before = state(&invalid);
        assert!(link_references(&mut invalid).is_err(), "problem {problem}");
        assert_eq!(state(&invalid), before, "problem {problem}");
    }
}

#[test]
fn project_and_reference_tuple_ids_cannot_collide_or_collapse_callers() {
    let caller = declaration("caller", "app.Client.run", "method");
    let mut graph = snapshot::merge(vec![
        (
            "client".into(),
            graph_snapshot(
                vec![caller.clone()],
                vec![reference("part:site", "java:static:api.Tools.run")],
            ),
        ),
        (
            "client:part".into(),
            graph_snapshot(
                vec![caller],
                vec![reference("site", "java:static:api.Tools.run")],
            ),
        ),
        (
            "library".into(),
            graph_snapshot(
                vec![
                    declaration("target", "api.Tools.run", "method"),
                    declaration("owner", "api.Tools", "class"),
                ],
                vec![],
            ),
        ),
    ])
    .unwrap();
    let nodes = serde_json::to_value(&graph.nodes).unwrap();
    assert_eq!(link_references(&mut graph).unwrap(), 2);
    let links = generated(&graph);
    assert_ne!(links[0].id, links[1].id);
    assert_ne!(links[0].source, links[1].source);
    assert_eq!(links[0].target, links[1].target);
    assert_eq!(nodes, serde_json::to_value(&graph.nodes).unwrap());
}

#[test]
fn snapshot_read_import_roundtrip_retains_reference_evidence_for_composition() {
    let caller = source_snapshot(
        "Client.kt",
        "package client\nimport api.work\nfun run() { work() }\n",
    );
    let library = source_snapshot("Library.kt", "package api\nfun work() {}\n");
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("snapshot.json");
    std::fs::write(&path, serde_json::to_vec(&caller).unwrap()).unwrap();
    let mut store = Store::create(&dir.path().join("imported.db")).unwrap();
    store.import_graph(snapshot::read(&path).unwrap()).unwrap();
    let wrapped = store.snapshot().unwrap();
    assert!(wrapped.metadata["graf_snapshot"]["metadata"]["graf_unresolved_references"].is_array());
    let mut graph =
        snapshot::merge(vec![("app".into(), wrapped), ("lib".into(), library)]).unwrap();
    assert!(link_references(&mut graph).unwrap() > 0);
    assert!(generated(&graph).iter().any(|e| e.relation == "calls"));
}
