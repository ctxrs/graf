use graf::{
    index::{self, IndexOptions},
    model::{Direction, GraphSnapshot, Node, QueryOptions},
    store::Store,
};
use std::{collections::BTreeMap, fs};
use tempfile::{TempDir, tempdir};

struct Fixture {
    temp: TempDir,
}
impl Fixture {
    fn new() -> Self {
        let result = Self {
            temp: tempdir().unwrap(),
        };
        fs::create_dir(result.root()).unwrap();
        result
    }
    fn root(&self) -> std::path::PathBuf {
        self.temp.path().join("repo")
    }
    fn db(&self) -> std::path::PathBuf {
        self.temp.path().join("graph.db")
    }
    fn write(&self, path: &str, source: &str) {
        let target = self.root().join(path);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(target, source).unwrap();
    }
    fn index(&self) -> GraphSnapshot {
        self.index_with(&IndexOptions {
            code_only: true,
            ..Default::default()
        })
    }
    fn index_with(&self, options: &IndexOptions) -> GraphSnapshot {
        index::run_with_options(&self.root(), &self.db(), options).unwrap();
        Store::open(&self.db()).unwrap().snapshot().unwrap()
    }
    fn unresolved(&self, source: &Node, label: &str) -> bool {
        Store::open(&self.db())
            .unwrap()
            .neighbors(
                &source.id,
                &QueryOptions {
                    direction: Direction::Outgoing,
                    relation: Some("calls".into()),
                    ..Default::default()
                },
            )
            .unwrap()
            .unresolved
            .iter()
            .any(|r| r.label == label)
    }
}
fn node<'a>(graph: &'a GraphSnapshot, file: &str, label: &str) -> &'a Node {
    graph
        .nodes
        .iter()
        .find(|n| n.file == file && n.label == label)
        .unwrap_or_else(|| panic!("missing {file}:{label}"))
}
fn calls(graph: &GraphSnapshot, from: (&str, &str), to: (&str, &str)) -> bool {
    let source = &node(graph, from.0, from.1).id;
    let target = &node(graph, to.0, to.1).id;
    graph
        .edges
        .iter()
        .any(|e| e.relation == "calls" && &e.source == source && &e.target == target)
}

fn file_relation(
    graph: &GraphSnapshot,
    source: &str,
    relation: &str,
    target: (&str, &str),
) -> bool {
    let target = &node(graph, target.0, target.1).id;
    graph.edges.iter().any(|e| {
        e.relation == relation
            && &e.target == target
            && graph
                .nodes
                .iter()
                .any(|n| n.id == e.source && n.file == source)
    })
}

fn javascript_assert_fresh_equivalent(f: &Fixture, incremental: &GraphSnapshot) {
    let normalized = |graph: &GraphSnapshot| {
        let mut graph = graph.clone();
        graph.generation = 0;
        graph.root = None;
        serde_json::to_value(graph).unwrap()
    };
    let expected = normalized(incremental);
    let forced = f.index_with(&IndexOptions {
        code_only: true,
        force: true,
        ..Default::default()
    });
    assert_eq!(expected, normalized(&forced), "incremental versus forced");
    let fresh = tempdir().unwrap();
    let db = fresh.path().join("fresh.db");
    index::run_with_options(
        &f.root(),
        &db,
        &IndexOptions {
            code_only: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        expected,
        normalized(&Store::open(&db).unwrap().snapshot().unwrap()),
        "incremental versus fresh"
    );
    assert!(index::check_update(&f.root(), &f.db()).unwrap().fresh);
}

#[test]
fn javascript_unrelated_inventory_and_terminal_edits_keep_unchanged_caller_facts() {
    let f = Fixture::new();
    f.write("lib.ts", "export function work() {}\n");
    f.write(
        "main.ts",
        "import {work} from './lib'; export function use() { work(); }\n",
    );
    let original = f.index();
    assert!(calls(&original, ("main.ts", "use"), ("lib.ts", "work")));
    let stamp = || {
        Store::open(&f.db())
            .unwrap()
            .file_stamps()
            .unwrap()
            .into_iter()
            .find(|s| s.path == "main.ts")
            .unwrap()
            .hash
    };
    let caller_stamp = stamp();
    f.write("unrelated.ts", "export function elsewhere() {}\n");
    let changes = index::check_update(&f.root(), &f.db()).unwrap();
    assert!(
        !changes
            .changed
            .iter()
            .any(|p| p == "main.ts" || p == "lib.ts")
    );
    let added = f.index();
    assert_eq!(stamp(), caller_stamp);
    javascript_assert_fresh_equivalent(&f, &added);
    fs::remove_file(f.root().join("unrelated.ts")).unwrap();
    let removed = f.index();
    assert_eq!(stamp(), caller_stamp);
    javascript_assert_fresh_equivalent(&f, &removed);
    for (source, present) in [
        ("export {};\n", false),
        ("\nexport function work() {}\n", true),
    ] {
        f.write("lib.ts", source);
        assert!(
            !index::check_update(&f.root(), &f.db())
                .unwrap()
                .changed
                .iter()
                .any(|p| p == "main.ts")
        );
        let graph = f.index();
        assert_eq!(stamp(), caller_stamp);
        let caller = node(&graph, "main.ts", "use");
        assert_eq!(f.unresolved(caller, "work"), !present);
        assert_eq!(
            graph
                .edges
                .iter()
                .filter(|e| e.source == caller.id && e.relation == "calls")
                .count(),
            usize::from(present)
        );
        if present {
            assert!(calls(&graph, ("main.ts", "use"), ("lib.ts", "work")));
            assert_eq!(node(&graph, "lib.ts", "work").line, Some(2));
        }
        javascript_assert_fresh_equivalent(&f, &graph);
        let noop = f.index();
        let generation = noop.generation;
        assert_eq!(generation, f.index().generation);
    }
}

#[test]
fn javascript_selected_module_add_delete_matches_fresh_without_export_fallback() {
    let f = Fixture::new();
    f.write("lib/index.ts", "export function work() {}\n");
    f.write(
        "main.ts",
        "import {work} from './lib'; function use() { work(); }\n",
    );
    let first = f.index();
    assert!(calls(&first, ("main.ts", "use"), ("lib/index.ts", "work")));
    for source in [
        Some("export {};\n"),
        Some("export function work() {}\n"),
        None,
    ] {
        if let Some(source) = source {
            f.write("lib.ts", source);
        } else {
            fs::remove_file(f.root().join("lib.ts")).unwrap();
        }
        let graph = f.index();
        let caller = node(&graph, "main.ts", "use");
        if source == Some("export {};\n") {
            assert!(f.unresolved(caller, "work"));
            assert!(
                !graph
                    .edges
                    .iter()
                    .any(|e| e.source == caller.id && e.relation == "calls")
            );
        } else {
            let target = if source.is_some() {
                "lib.ts"
            } else {
                "lib/index.ts"
            };
            assert!(calls(&graph, ("main.ts", "use"), (target, "work")));
        }
        javascript_assert_fresh_equivalent(&f, &graph);
    }
}

#[test]
fn javascript_local_factory_returns_link_bodies_without_promoting_values_or_types() {
    let f = Fixture::new();
    for definition in [
        "function build() { function Body() {} return Body; }",
        "function build() { return function Body() {}; }",
    ] {
        f.write("main.ts", &format!("{definition}\ninterface Value {{}}\nconst Value = build();\nexport function use(): Value {{ Value(); return new Value(); }}\n"));
        let graph = f.index();
        let caller = node(&graph, "main.ts", "use");
        let body = node(&graph, "main.ts", "Body");
        let value = graph
            .nodes
            .iter()
            .find(|n| n.label == "Value" && n.kind == "constant")
            .unwrap();
        let interface = graph
            .nodes
            .iter()
            .find(|n| n.label == "Value" && n.kind == "interface")
            .unwrap();
        let runtime: Vec<_> = graph
            .edges
            .iter()
            .filter(|e| e.source == caller.id && e.relation == "calls")
            .collect();
        assert_eq!(runtime.len(), 2);
        assert!(
            runtime
                .iter()
                .all(|e| e.target == body.id && e.line == Some(4))
        );
        let declarations: Vec<_> = graph
            .edges
            .iter()
            .filter(|e| e.source == caller.id && e.relation == "declared_callee")
            .collect();
        assert_eq!(declarations.len(), 2);
        for call in runtime {
            assert!(declarations.iter().any(|e| e.target == value.id
                && e.metadata["reference_id"]
                    == format!(
                        "{}:declared_callee",
                        call.metadata["reference_id"].as_str().unwrap()
                    )
                && e.file == call.file
                && e.line == call.line));
        }
        assert!(graph.edges.iter().any(|e| e.source == caller.id
            && e.target == interface.id
            && e.relation == "return_type"));
        assert!(!graph.edges.iter().any(|e| e.source == caller.id
            && e.relation == "calls"
            && (e.target == value.id || e.target == interface.id)));
        javascript_assert_fresh_equivalent(&f, &graph);
    }
}

#[test]
fn javascript_namespace_factory_return_proof_tracks_updates_deletion_and_ambiguity() {
    let f = Fixture::new();
    f.write("barrel.ts", "export * from './factory.js';\n");
    f.write("value.ts", "import * as core from './barrel.js';\nexport interface Value {}\nexport const Value: core.build = core.build();\n");
    f.write("main.ts", "import { Value } from './value.js';\nexport function use(): Value {\n return new Value();\n}\n");
    let provider = |body: &str| {
        format!(
            "export interface build {{ new(): unknown; }}\nexport function build() {{\n function helper(flag) {{ if (flag) return 1; return 2; }}\n function {body}() {{ return helper(true); }}\n Object.defineProperty({body}, 'name', {{ value: 'Value' }});\n return {body} as any;\n}}\n"
        )
    };
    for step in 0..7 {
        match step {
            0 => f.write("factory.ts", &provider("First")),
            1 => f.write("factory.ts", &provider("Replacement")),
            2 => fs::remove_file(f.root().join("factory.ts")).unwrap(),
            3 | 6 => f.write("factory.ts", &provider("Restored")),
            4 => f.write("factory.ts", "export interface build {} export function build(flag) { function Left() {} function Right() {} if (flag) return Left; return Right; }"),
            5 => {
                f.write("factory.ts", &provider("Restored"));
                f.write("competitor.ts", &provider("Competing"));
                f.write("barrel.ts", "export * from './factory.js'; export * from './competitor.js';");
            }
            _ => unreachable!(),
        }
        if step == 6 {
            fs::remove_file(f.root().join("competitor.ts")).unwrap();
            f.write("barrel.ts", "export * from './factory.js';");
        }
        let graph = f.index();
        let caller = node(&graph, "main.ts", "use");
        let runtime: Vec<_> = graph
            .edges
            .iter()
            .filter(|e| e.source == caller.id && e.relation == "calls")
            .collect();
        let expected = match step {
            0 => Some("First"),
            1 => Some("Replacement"),
            3 | 6 => Some("Restored"),
            _ => None,
        };
        if let Some(expected) = expected {
            assert_eq!(runtime.len(), 1, "step {step}");
            assert_eq!(runtime[0].target, node(&graph, "factory.ts", expected).id);
            assert_eq!(runtime[0].line, Some(3));
            assert_eq!(runtime[0].file.as_deref(), Some("main.ts"));
            assert!(!f.unresolved(caller, "Value"));
        } else {
            assert!(runtime.is_empty(), "step {step}");
            assert!(f.unresolved(caller, "Value"), "step {step}");
        }
        let value = graph
            .nodes
            .iter()
            .find(|n| n.file == "value.ts" && n.kind == "constant")
            .unwrap();
        assert!(graph.edges.iter().any(|e| e.source == caller.id
            && e.relation == "declared_callee"
            && e.target == value.id));
        assert!(!graph.edges.iter().any(|e| {
            e.source == caller.id
                && e.relation == "calls"
                && graph.nodes.iter().any(|n| {
                    n.id == e.target && matches!(n.kind.as_str(), "constant" | "interface")
                })
        }));
        javascript_assert_fresh_equivalent(&f, &graph);
    }
}

#[test]
fn javascript_factory_return_calls_require_executable_unique_immutable_proof() {
    let f = Fixture::new();
    f.write(
        "factory.ts",
        "export interface build {} export function build() { function Body() {} return Body; }",
    );
    for (setup, result) in [
        (
            "import type { build } from './factory';",
            "const Value = build();",
        ),
        (
            "import type * as core from './factory';",
            "const Value = core.build();",
        ),
        (
            "interface build { new(): unknown; } declare const unknown: build;",
            "const Value = unknown();",
        ),
        (
            "import * as core from './factory'; core.build = other;",
            "const Value = core.build();",
        ),
        ("import { build } from './factory';", "let Value = build();"),
        (
            "import { build } from './factory';",
            "const Value = build(); Value = other;",
        ),
        (
            "import { build } from './factory';",
            "const Value = build?.();",
        ),
        (
            "import { build } from './factory';",
            "const Value = build()();",
        ),
        (
            "import * as core from './factory';",
            "const Value = core[key]();",
        ),
        (
            "import * as core from './factory';",
            "const Value = core?.build();",
        ),
    ] {
        f.write(
            "main.ts",
            &format!("{setup}\n{result}\nexport function use() {{ new Value(); }}"),
        );
        let graph = f.index();
        let caller = node(&graph, "main.ts", "use");
        assert!(
            !graph
                .edges
                .iter()
                .any(|e| e.source == caller.id && e.relation == "calls"),
            "{setup} {result}"
        );
        assert!(f.unresolved(caller, "Value"));
    }
    // A parameter hides the imported factory; a value parameter hides the const.
    for source in [
        "import { build } from './factory'; export function use(build) { const Value = build(); return new Value(); }",
        "import { build } from './factory'; const Value = build(); export function use(Value) { return new Value(); }",
    ] {
        f.write("main.ts", source);
        let graph = f.index();
        let caller = node(&graph, "main.ts", "use");
        assert!(
            !graph
                .edges
                .iter()
                .any(|e| e.source == caller.id && e.relation == "calls"),
            "{source}"
        );
        assert!(f.unresolved(caller, "Value"));
    }
    f.write("main.ts", "import { build } from './factory'; const Value = build(); export function use() { return new Value(); }");
    for provider in [
        "export interface build { new(): unknown; }",
        "export async function build() { function Body() {} return Body; }",
        "export function* build() { function Body() {} return Body; }",
        "export function build() { return () => {}; }",
        "export function build() { function Body() {} Body = other; return Body; }",
        "export function build() { function Body() {} function change() { Body = other; } return Body; }",
        "export function build(Body) { return Body; }",
        "export function build() { function Body() {} if (flag) return Body; return Body; }",
        "export function build() { function Body() {} return Body; } build = other;",
    ] {
        f.write("factory.ts", provider);
        let graph = f.index();
        let caller = node(&graph, "main.ts", "use");
        assert!(
            !graph
                .edges
                .iter()
                .any(|e| e.source == caller.id && e.relation == "calls"),
            "{provider}"
        );
        assert!(f.unresolved(caller, "Value"));
    }
}

#[test]
fn javascript_imported_factory_proof_refreshes_without_promoting_the_interface() {
    let f = Fixture::new();
    f.write("main.ts", "import {Shape as Value, ordinary, Plain} from './provider';\nexport function use(value: Value): Value {\n Value();\n new Value();\n Value?.();\n Value[key]();\n ordinary(); new Plain(); return value;\n}\n");
    for (declaration, eligible) in [
        ("export const Shape = factory();", true),
        ("", false),
        ("export const Shape = external;", false),
        ("export const Shape = factory(); Shape = external;", false),
        (
            "export const Shape = factory(); const Shape = factory();",
            false,
        ),
        ("export const Shape = factory();", true),
    ] {
        f.write("provider.ts", &format!("export interface Shape {{}}\n{declaration}\nexport function ordinary() {{}}\nexport class Plain {{}}\n"));
        let graph = f.index();
        let caller = node(&graph, "main.ts", "use");
        let interface = graph
            .nodes
            .iter()
            .find(|n| n.file == "provider.ts" && n.label == "Shape" && n.kind == "interface")
            .unwrap();
        assert!(graph.edges.iter().any(|e| e.source == caller.id
            && e.relation == "return_type"
            && e.target == interface.id));
        assert!(calls(
            &graph,
            ("main.ts", "use"),
            ("provider.ts", "ordinary")
        ));
        assert!(calls(&graph, ("main.ts", "use"), ("provider.ts", "Plain")));
        let original: Vec<_> = graph.metadata["graf_unresolved_references"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| {
                r["source"] == caller.id && r["relation"] == "calls" && r["label"] == "Value"
            })
            .collect();
        assert_eq!(original.len(), 3);
        assert!(
            original
                .iter()
                .all(|r| r["candidate_keys"] == serde_json::json!([]))
        );
        let siblings: Vec<_> = graph
            .edges
            .iter()
            .filter(|e| e.source == caller.id && e.relation == "declared_callee")
            .collect();
        assert_eq!(siblings.len(), if eligible { 3 } else { 0 });
        // Optional invocation still identifies the written imported binding;
        // declaration navigation does not assert that the value is callable.
        assert!(original.iter().any(|r| r["line"] == 5));
        assert_eq!(siblings.iter().any(|e| e.line == Some(5)), eligible);
        assert!(
            graph.metadata["graf_unresolved_references"]
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r["source"] == caller.id
                    && r["relation"] == "calls"
                    && r["label"] == "Value[key]"
                    && r["candidate_keys"] == serde_json::json!([]))
        );
        assert!(!graph.edges.iter().any(|e| {
            e.source == caller.id
                && e.relation == "calls"
                && graph
                    .nodes
                    .iter()
                    .any(|n| n.id == e.target && (n.kind == "interface" || n.kind == "constant"))
        }));
        for sibling in siblings {
            let value = graph.nodes.iter().find(|n| n.id == sibling.target).unwrap();
            assert_eq!(
                (
                    value.file.as_str(),
                    value.label.as_str(),
                    value.kind.as_str()
                ),
                ("provider.ts", "Shape", "constant")
            );
            assert_eq!(value.metadata["declared_callee_binding"], true);
            let call = original
                .iter()
                .find(|r| {
                    sibling.metadata["reference_id"]
                        == format!("{}:declared_callee", r["id"].as_str().unwrap())
                })
                .unwrap();
            assert_eq!(sibling.file.as_deref(), Some("main.ts"));
            assert_eq!(sibling.line.map(u64::from), call["line"].as_u64());
            assert!(matches!(sibling.line, Some(3..=5)));
        }
        javascript_assert_fresh_equivalent(&f, &graph);
    }
}

#[test]
fn javascript_provider_scratch_pruning_preserves_local_facts_and_competing_origins() {
    let f = Fixture::new();
    f.write(
        "barrel.ts",
        "export * from './provider'; export * from './competitor';\n",
    );
    f.write("main.ts", "import {Shape, ordinary} from './barrel'; export function use(value: Shape): Shape { Shape(); ordinary(); return value; }\n");
    let mut previous = None;
    for (private_count, competitor, eligible) in [
        (0, "export {};\n", true),
        (24, "export {};\n", true),
        (0, "export {};\n", true),
        (24, "export function Shape() {}\n", false),
        (24, "export {};\n", true),
        (24, "export const Shape = otherFactory();\n", false),
        (24, "export {};\n", true),
    ] {
        let mut provider = String::from(
            "export interface Shape {}\nexport const Shape = factory();\nexport function ordinary() {}\nfunction privateScope() {\n/** Scoped helper. */\nfunction local() {}\nlocal();\n",
        );
        for i in 0..private_count {
            provider.push_str(&format!("function helper_{i}() {{}} helper_{i}();\n"));
        }
        provider.push_str("}\n");
        f.write("provider.ts", &provider);
        f.write("competitor.ts", competitor);
        let graph = f.index();
        let caller = node(&graph, "main.ts", "use");
        let declarations: Vec<_> = graph
            .edges
            .iter()
            .filter(|edge| edge.source == caller.id && edge.relation == "declared_callee")
            .collect();
        assert_eq!(declarations.len(), usize::from(eligible));
        if eligible {
            let target = graph
                .nodes
                .iter()
                .find(|node| node.id == declarations[0].target)
                .unwrap();
            assert_eq!(
                (
                    target.file.as_str(),
                    target.label.as_str(),
                    target.kind.as_str()
                ),
                ("provider.ts", "Shape", "constant")
            );
            assert_eq!(target.metadata["declared_callee_binding"], true);
            let interface = graph
                .nodes
                .iter()
                .find(|node| {
                    node.file == "provider.ts" && node.label == "Shape" && node.kind == "interface"
                })
                .unwrap();
            assert!(graph.edges.iter().any(|edge| edge.source == caller.id
                && edge.relation == "return_type"
                && edge.target == interface.id));
        }
        assert!(f.unresolved(caller, "Shape"));
        let runtime: Vec<_> = graph
            .edges
            .iter()
            .filter(|edge| edge.source == caller.id && edge.relation == "calls")
            .collect();
        assert_eq!(runtime.len(), 1);
        assert_eq!(
            runtime[0].target,
            node(&graph, "provider.ts", "ordinary").id
        );

        // These local definitions never publish own-file provider keys, but
        // must remain indexed and callable in their original lexical scope.
        assert!(calls(
            &graph,
            ("provider.ts", "privateScope"),
            ("provider.ts", "local")
        ));
        let local = node(&graph, "provider.ts", "local");
        assert!(
            !local
                .binding_key
                .iter()
                .map(String::as_str)
                .chain(
                    local.metadata["binding_aliases"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|key| key.as_str())
                )
                .any(|key| key.starts_with("javascript:file:provider.ts:")
                    || key.starts_with("javascript:cjs-file:provider.ts:"))
        );
        assert_eq!(
            graph
                .nodes
                .iter()
                .filter(|node| node.file == "provider.ts"
                    && node.label.starts_with("helper_")
                    && node.kind == "function")
                .count(),
            private_count
        );
        for i in 0..private_count {
            assert!(calls(
                &graph,
                ("provider.ts", "privateScope"),
                ("provider.ts", &format!("helper_{i}"))
            ));
        }

        let stamp = Store::open_read_only(&f.db())
            .unwrap()
            .file_stamps()
            .unwrap()
            .into_iter()
            .find(|stamp| stamp.path == "main.ts")
            .unwrap()
            .hash;
        if let Some((previous_eligible, previous_stamp)) = previous {
            assert_eq!(previous_stamp == stamp, previous_eligible == eligible);
        }
        previous = Some((eligible, stamp));
        // This existing comparison includes all persisted references/keys and
        // aliases as well as the graph; it is independent of source language.
        rust_assert_fresh_forced_equivalent(&f);
    }
}

#[test]
fn javascript_imported_factory_stars_preserve_ambiguity_and_direct_value_precedence() {
    let f = Fixture::new();
    f.write(
        "a.ts",
        "export interface Shape {} export const Shape = firstFactory();",
    );
    f.write("b.ts", "export const Shape = secondFactory();");
    f.write("ordinary.ts", "export function Shape() {}");
    f.write("left.ts", "export * from './a'; export * from './right';");
    f.write("right.ts", "export * from './a'; export * from './left';");
    f.write(
        "main.ts",
        "import {Shape} from './barrel'; export function use() { Shape(); }",
    );
    for (source, target) in [
        (
            "export * from './left'; export * from './right';",
            Some(("a.ts", "declared_callee")),
        ),
        ("export * from './a'; export * from './b';", None),
        ("export * from './a'; export * from './ordinary';", None),
        (
            "export * from './a'; export function Shape() {}",
            Some(("barrel.ts", "calls")),
        ),
        (
            "export interface Shape {} export * from './a';",
            Some(("a.ts", "declared_callee")),
        ),
        ("export * from './a';", Some(("a.ts", "declared_callee"))),
    ] {
        f.write("barrel.ts", source);
        let graph = f.index();
        let caller = node(&graph, "main.ts", "use");
        let edges: Vec<_> = graph
            .edges
            .iter()
            .filter(|e| {
                e.source == caller.id && matches!(e.relation.as_str(), "calls" | "declared_callee")
            })
            .collect();
        assert_eq!(edges.len(), usize::from(target.is_some()), "{source}");
        if let Some((file, relation)) = target {
            let edge = edges[0];
            assert_eq!(edge.relation, relation);
            let target = graph.nodes.iter().find(|n| n.id == edge.target).unwrap();
            assert_eq!(target.file, file);
            assert_eq!(
                target.kind,
                if relation == "calls" {
                    "function"
                } else {
                    "constant"
                }
            );
        }
        assert_eq!(
            f.unresolved(caller, "Shape"),
            target.is_none_or(|(_, relation)| relation != "calls")
        );
        javascript_assert_fresh_equivalent(&f, &graph);
    }
}

#[test]
fn javascript_imported_factory_follows_only_published_named_default_routes() {
    let f = Fixture::new();
    f.write(
        "provider.ts",
        "export interface Shape {} export const Shape = factory(); export function ordinary() {}",
    );
    f.write(
        "named.ts",
        "export {Shape as Renamed, ordinary} from './provider';",
    );
    f.write("default.ts", "export {Renamed as default} from './named';");
    f.write("main.ts", "import Value from './default'; import {Renamed, ordinary} from './named'; function use() { Value(); Renamed(); ordinary(); }");
    for route in [
        "export {Renamed as default} from './named';",
        "export {Missing as default} from './named';",
        "export {default} from './default';",
        "export {Renamed as default} from './named';",
    ] {
        f.write("default.ts", route);
        let graph = f.index();
        let caller = node(&graph, "main.ts", "use");
        let declarations: Vec<_> = graph
            .edges
            .iter()
            .filter(|e| e.source == caller.id && e.relation == "declared_callee")
            .collect();
        assert_eq!(
            declarations.len(),
            if route.contains("Renamed") { 2 } else { 1 }
        );
        for declaration in declarations {
            let target = graph
                .nodes
                .iter()
                .find(|n| n.id == declaration.target)
                .unwrap();
            assert_eq!(target.file, "provider.ts");
            assert_eq!(target.kind, "constant");
            assert_eq!(target.label, "Shape");
        }
        assert!(f.unresolved(caller, "Value"));
        assert!(f.unresolved(caller, "Renamed"));
        // Ordinary forwarding retains its existing alias target.
        assert!(calls(&graph, ("main.ts", "use"), ("named.ts", "ordinary")));
        assert!(!graph.edges.iter().any(|e| {
            e.source == caller.id
                && e.relation == "calls"
                && graph
                    .nodes
                    .iter()
                    .any(|n| n.id == e.target && n.kind == "interface")
        }));
        javascript_assert_fresh_equivalent(&f, &graph);
    }
}

#[test]
fn javascript_config_changes_match_fresh_and_preserve_unrelated_outcomes() {
    let f = Fixture::new();
    f.write("tsconfig.json", r#"{"extends":"./base.json"}"#);
    f.write("one.ts", "export function work() {}");
    f.write("two.ts", "export function work() {}");
    f.write(
        "typed.ts",
        "import {work} from 'target'; function use() { work(); }",
    );
    f.write("lib.cjs", "exports.work = function work() {};");
    f.write(
        "mode.js",
        "const lib = require('./lib.cjs'); function use() { lib.work(); }",
    );
    f.write(
        "fixed.cjs",
        "const lib = require('./lib.cjs'); function use() { lib.work(); }",
    );
    f.write("unrelated.ts", "export function unrelated() {}");
    let mut unrelated_stamp = None;
    for (target, esm) in [
        (Some("one.ts"), false),
        (Some("two.ts"), true),
        (None, false),
        (Some("one.ts"), false),
    ] {
        if let Some(target) = target {
            f.write(
                "base.json",
                &serde_json::json!({"compilerOptions":{"baseUrl":".","paths":{"target":[target]}}})
                    .to_string(),
            );
        } else {
            fs::remove_file(f.root().join("base.json")).unwrap();
        }
        f.write(
            "package.json",
            if esm { r#"{"type":"module"}"# } else { "{}" },
        );
        let graph = f.index();
        if let Some(target) = target {
            assert!(calls(&graph, ("typed.ts", "use"), (target, "work")));
        } else {
            assert!(f.unresolved(node(&graph, "typed.ts", "use"), "work"));
        }
        assert_eq!(calls(&graph, ("mode.js", "use"), ("lib.cjs", "work")), !esm);
        assert!(calls(&graph, ("fixed.cjs", "use"), ("lib.cjs", "work")));
        let stamp = Store::open(&f.db())
            .unwrap()
            .file_stamps()
            .unwrap()
            .into_iter()
            .find(|s| s.path == "unrelated.ts")
            .unwrap()
            .hash;
        if let Some(previous) = &unrelated_stamp {
            assert_eq!(&stamp, previous);
        }
        unrelated_stamp = Some(stamp);
        javascript_assert_fresh_equivalent(&f, &graph);
    }
}

#[test]
fn go_uses_declared_package_names_excludes_external_tests_and_rebinds_modules() {
    let f = Fixture::new();
    f.write("go.mod", "module example.org/project\n\ngo 1.22\n");
    f.write("main.go", "package main\nimport \"example.org/project/wire/v2\"\nfunc Main() { wire.Read() }\nfunc Shadow(wire interface{ Read() }) { wire.Read() }\n");
    f.write("wire/v2/a.go", "package wire\nfunc Read() {}\n");
    f.write("wire/v2/a_test.go", "package wire_test\nfunc Read() {}\n");
    let graph = f.index();
    assert!(calls(&graph, ("main.go", "Main"), ("wire/v2/a.go", "Read")));
    assert!(!calls(
        &graph,
        ("main.go", "Main"),
        ("wire/v2/a_test.go", "Read")
    ));
    assert!(f.unresolved(node(&graph, "main.go", "Shadow"), "wire.Read"));
    f.write("go.mod", "module other.org/project\n\ngo 1.22\n");
    assert!(
        index::check_update(&f.root(), &f.db())
            .unwrap()
            .changed
            .contains(&"main.go".into())
    );
    let graph = f.index();
    assert!(f.unresolved(node(&graph, "main.go", "Main"), "wire.Read"));
    assert!(!calls(
        &graph,
        ("main.go", "Main"),
        ("wire/v2/a.go", "Read")
    ));
}

#[test]
fn go_nested_modules_and_package_owner_deletion_do_not_leave_stale_nodes() {
    let f = Fixture::new();
    f.write("go.mod", "module example.org/root\n");
    f.write("sub/go.mod", "module example.org/child\n");
    f.write("sub/a.go", "package service\nfunc First() {}\n");
    f.write("sub/b.go", "package service\nfunc Work() {}\n");
    f.write(
        "main.go",
        "package main\nimport svc \"example.org/child\"\nfunc Main() { svc.Work() }\n",
    );
    let graph = f.index();
    assert!(calls(&graph, ("main.go", "Main"), ("sub/b.go", "Work")));
    fs::remove_file(f.root().join("sub/a.go")).unwrap();
    let graph = f.index();
    let packages: Vec<_> = graph
        .nodes
        .iter()
        .filter(|n| n.binding_key.as_deref() == Some("go:import-module:example.org/child"))
        .collect();
    assert_eq!(packages.len(), 1);
    assert_eq!(packages[0].file, "sub/b.go");
    assert!(calls(&graph, ("main.go", "Main"), ("sub/b.go", "Work")));
}

#[test]
fn typescript_jsonc_paths_baseurl_extends_and_changes_are_applied() {
    let f = Fixture::new();
    f.write(
        "configs/base.json",
        r#"{ // shared options
      "compilerOptions": {"baseUrl":"..", "paths":{"@core/*":["shared/*"]},},
    }"#,
    );
    f.write(
        "apps/tsconfig.json",
        r#"{"extends":"../configs/base.json"}"#,
    );
    f.write("shared/util.ts", "export function work() {}\n");
    f.write("other/util.ts", "export function work() {}\n");
    f.write("apps/main.ts", "import {work} from '@core/util'; import {work as direct} from 'shared/util'; export function Main() { work(); direct(); }\n");
    let graph = f.index();
    assert!(calls(
        &graph,
        ("apps/main.ts", "Main"),
        ("shared/util.ts", "work")
    ));
    f.write(
        "configs/base.json",
        r#"{"compilerOptions":{"baseUrl":"..","paths":{"@core/*":["other/*"]}}}"#,
    );
    assert!(
        index::check_update(&f.root(), &f.db())
            .unwrap()
            .changed
            .contains(&"apps/main.ts".into())
    );
    let graph = f.index();
    assert!(calls(
        &graph,
        ("apps/main.ts", "Main"),
        ("other/util.ts", "work")
    ));
    assert!(calls(
        &graph,
        ("apps/main.ts", "Main"),
        ("shared/util.ts", "work")
    ));
}

#[test]
fn javascript_workspaces_exports_subpaths_and_explicit_file_dependencies_resolve() {
    let f = Fixture::new();
    f.write(
        "package.json",
        r#"{"private":true,"workspaces":["packages/*"]}"#,
    );
    f.write("packages/lib/package.json", r#"{"name":"@sample/lib","exports":{".":"./src/index.ts","./feature/*":"./src/feature/*.ts"}}"#);
    f.write("packages/lib/src/index.ts", "export function work() {}\n");
    f.write(
        "packages/lib/src/feature/tools.ts",
        "export function tool() {}\n",
    );
    f.write(
        "packages/lib/src/private.ts",
        "export function secret() {}\n",
    );
    f.write(
        "packages/app/package.json",
        r#"{"name":"app","dependencies":{"local":"file:../lib"}}"#,
    );
    f.write("packages/app/main.ts", "import {work} from '@sample/lib'; import {tool} from '@sample/lib/feature/tools'; import {work as local} from 'local'; import {secret} from '@sample/lib/src/private'; export function Main() {work(); tool(); local(); secret();}\n");
    let graph = f.index();
    assert!(calls(
        &graph,
        ("packages/app/main.ts", "Main"),
        ("packages/lib/src/index.ts", "work")
    ));
    assert!(calls(
        &graph,
        ("packages/app/main.ts", "Main"),
        ("packages/lib/src/feature/tools.ts", "tool")
    ));
    assert!(f.unresolved(node(&graph, "packages/app/main.ts", "Main"), "secret"));
    assert_eq!(
        graph
            .edges
            .iter()
            .filter(|e| e.relation == "calls"
                && e.source == node(&graph, "packages/app/main.ts", "Main").id
                && e.target == node(&graph, "packages/lib/src/index.ts", "work").id)
            .count(),
        2
    );
}

#[test]
fn package_names_are_not_guessed_and_duplicate_workspace_names_are_ambiguous() {
    let f = Fixture::new();
    f.write("package.json", r#"{"workspaces":["packages/*"]}"#);
    for dir in ["one", "two"] {
        f.write(
            &format!("packages/{dir}/package.json"),
            r#"{"name":"duplicate","exports":"./index.js"}"#,
        );
        f.write(
            &format!("packages/{dir}/index.js"),
            "export function work() {}\n",
        );
    }
    f.write("main.js", "import {work} from 'duplicate'; import {work as guessed} from 'one'; function Main(){work();guessed();}\n");
    let graph = f.index();
    assert!(f.unresolved(node(&graph, "main.js", "Main"), "work"));
    assert!(f.unresolved(node(&graph, "main.js", "Main"), "guessed"));
}

#[test]
fn cargo_workspace_dependencies_use_actual_library_aliases_and_public_modules() {
    let f = Fixture::new();
    f.write("Cargo.toml", "[workspace]\nmembers=[\"crates/*\"]\n[workspace.dependencies]\nutility={path=\"crates/toolbox\",package=\"toolbox\"}\n");
    f.write(
        "crates/toolbox/Cargo.toml",
        "[package]\nname=\"toolbox\"\nversion=\"0.1.0\"\n[lib]\nname=\"actual_tool\"\n",
    );
    f.write("crates/toolbox/src/lib.rs", "pub fn work() {} pub(crate) fn restricted() {} pub mod nested; mod secret { pub fn hidden() {} }\n");
    f.write("crates/toolbox/src/nested.rs", "pub fn deep() {}\n");
    f.write("crates/toolbox/src/main.rs", "pub fn only_binary() {}\n");
    f.write(
        "crates/app/Cargo.toml",
        "[package]\nname=\"app\"\nversion=\"0.1.0\"\n[dependencies]\nutility.workspace=true\n",
    );
    f.write("crates/app/src/main.rs", "use utility::{work as run, restricted, only_binary}; use utility::secret::hidden; fn Main(){run(); utility::nested::deep(); restricted(); only_binary(); hidden();}\n");
    let graph = f.index();
    assert!(calls(
        &graph,
        ("crates/app/src/main.rs", "Main"),
        ("crates/toolbox/src/lib.rs", "work")
    ));
    assert!(calls(
        &graph,
        ("crates/app/src/main.rs", "Main"),
        ("crates/toolbox/src/nested.rs", "deep")
    ));
    for label in ["restricted", "only_binary", "hidden"] {
        assert!(
            f.unresolved(node(&graph, "crates/app/src/main.rs", "Main"), label),
            "{label}"
        );
    }
    let aliases = node(&graph, "crates/toolbox/src/lib.rs", "work").metadata["binding_aliases"]
        .as_array()
        .unwrap();
    assert!(
        aliases
            .iter()
            .any(|a| a == "rust:package:crates/toolbox:actual_tool:work")
    );
    assert!(
        aliases
            .iter()
            .any(|a| a == "symbol:crates/toolbox/src/lib.rs::work")
    );
}

#[test]
fn cargo_path_dependency_change_and_module_membership_changes_rebind() {
    let f = Fixture::new();
    f.write(
        "Cargo.toml",
        "[workspace]\nmembers=[\"app\",\"left\",\"right\"]\n",
    );
    for dir in ["left", "right"] {
        f.write(
            &format!("{dir}/Cargo.toml"),
            &format!("[package]\nname=\"{dir}\"\nversion=\"0.1.0\"\n"),
        );
        f.write(&format!("{dir}/src/lib.rs"), "pub fn work() {}\n");
    }
    f.write("app/Cargo.toml", "[package]\nname=\"app\"\nversion=\"0.1.0\"\n[dependencies]\nchosen={package=\"left\",path=\"../left\"}\n");
    f.write("app/src/main.rs", "use chosen::work; fn Main(){work();}\n");
    let graph = f.index();
    assert!(calls(
        &graph,
        ("app/src/main.rs", "Main"),
        ("left/src/lib.rs", "work")
    ));
    f.write("app/Cargo.toml", "[package]\nname=\"app\"\nversion=\"0.1.0\"\n[dependencies]\nchosen={package=\"right\",path=\"../right\"}\n");
    assert!(
        index::check_update(&f.root(), &f.db())
            .unwrap()
            .changed
            .contains(&"app/src/main.rs".into())
    );
    let graph = f.index();
    assert!(calls(
        &graph,
        ("app/src/main.rs", "Main"),
        ("right/src/lib.rs", "work")
    ));
    assert!(!calls(
        &graph,
        ("app/src/main.rs", "Main"),
        ("left/src/lib.rs", "work")
    ));
}

#[test]
fn config_targets_cannot_escape_repository_and_qualified_document_aliases_survive() {
    let f = Fixture::new();
    fs::write(
        f.temp.path().join("outside.ts"),
        "export function escape() {}\n",
    )
    .unwrap();
    f.write(
        "tsconfig.json",
        r#"{"compilerOptions":{"paths":{"outside":["../outside.ts"]}}}"#,
    );
    f.write(
        "main.ts",
        "import {escape} from 'outside'; function Main(){escape();}\n",
    );
    f.write(
        "src/foo.py",
        "class Widget:\n    def render(self):\n        pass\n",
    );
    let graph = f.index();
    assert!(f.unresolved(node(&graph, "main.ts", "Main"), "escape"));
    assert!(!graph.nodes.iter().any(|n| n.label == "escape"));
    let aliases = node(&graph, "src/foo.py", "render").metadata["binding_aliases"]
        .as_array()
        .unwrap();
    for key in [
        "symbol:render",
        "symbol:Widget.render",
        "symbol:src/foo.py::Widget.render",
    ] {
        assert!(aliases.iter().any(|v| v == key));
    }
}

#[cfg(unix)]
#[test]
fn config_symlinks_are_rejected_before_following_them() {
    let f = Fixture::new();
    f.write("main.ts", "function Main() {}\n");
    let outside = f.temp.path().join("outside.json");
    fs::write(&outside, "{}").unwrap();
    std::os::unix::fs::symlink(outside, f.root().join("tsconfig.json")).unwrap();
    let error = index::run_with_options(
        &f.root(),
        &f.db(),
        &IndexOptions {
            code_only: true,
            ..Default::default()
        },
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains("symlinks"));
}

#[test]
fn invalid_baseurl_does_not_turn_paths_into_repository_relative_matches() {
    let f = Fixture::new();
    f.write(
        "tsconfig.json",
        r#"{"compilerOptions":{"baseUrl":"..","paths":{"target":["lib.ts"]}}}"#,
    );
    f.write("lib.ts", "export function work() {}\n");
    f.write(
        "main.ts",
        "import {work} from 'target'; function Main(){work();}\n",
    );
    let graph = f.index();
    assert!(f.unresolved(node(&graph, "main.ts", "Main"), "work"));
    f.write(
        "tsconfig.json",
        r#"{"compilerOptions":{"baseUrl":".","paths":{"target":["lib.ts"]}}}"#,
    );
    let graph = f.index();
    assert!(calls(&graph, ("main.ts", "Main"), ("lib.ts", "work")));
}

#[test]
fn javascript_relative_directory_selection_does_not_fall_through_missing_exports() {
    let f = Fixture::new();
    f.write("dir/index.ts", "export function work() {}\n");
    f.write(
        "main.ts",
        "import {work} from './dir'; export function Main(){work();}\n",
    );
    let graph = f.index();
    assert!(calls(&graph, ("main.ts", "Main"), ("dir/index.ts", "work")));
    f.write("dir.ts", "export function different() {}\n");
    let graph = f.index();
    assert!(f.unresolved(node(&graph, "main.ts", "Main"), "work"));
    assert!(!calls(
        &graph,
        ("main.ts", "Main"),
        ("dir/index.ts", "work")
    ));
    f.write("exact.ts", "export function work(){}\n");
    f.write("exact.mjs", "export function different(){}\n");
    f.write(
        "literal.mjs",
        "import {work} from './exact.mjs';function Main(){work();}\n",
    );
    let graph = f.index();
    assert!(f.unresolved(node(&graph, "literal.mjs", "Main"), "work"));
}

#[test]
fn javascript_same_stem_lexical_calls_and_explicit_imports_keep_file_identity() {
    let f = Fixture::new();
    f.write(
        "foo.ts",
        "export function f() { f(); } export {f as alias}; export function own() { f(); }\n",
    );
    f.write(
        "foo.mjs",
        "export function f() { f(); } export {f as alias}; export function own() { f(); }\n",
    );
    f.write("main.mjs", "import {f as typed, alias as typedAlias} from './foo.ts'; import {f as module, alias as moduleAlias} from './foo.mjs'; import * as api from './foo.mjs'; function Main(){typed();typedAlias();module();moduleAlias();api.f();} function Shadow(f){f();}\n");
    let graph = f.index();
    for path in ["foo.ts", "foo.mjs"] {
        let other = if path == "foo.ts" {
            "foo.mjs"
        } else {
            "foo.ts"
        };
        for caller in ["f", "own"] {
            assert!(calls(&graph, (path, caller), (path, "f")));
            assert!(!calls(&graph, (path, caller), (other, "f")));
        }
        for label in ["f", "alias"] {
            assert!(calls(&graph, ("main.mjs", "Main"), (path, label)));
            assert!(
                node(&graph, path, label).metadata["binding_aliases"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|key| key == &format!("javascript:file:{path}:{label}"))
            );
        }
        assert!(graph.edges.iter().any(|e| e.relation == "aliases"
            && e.source == node(&graph, path, "alias").id
            && e.target == node(&graph, path, "f").id));
        assert!(!graph.edges.iter().any(|e| e.relation == "aliases"
            && e.source == node(&graph, path, "alias").id
            && e.target == node(&graph, other, "f").id));
    }
    assert!(f.unresolved(node(&graph, "main.mjs", "Shadow"), "f"));
}

#[test]
fn star_reexports_follow_cycles_exclude_default_and_preserve_ambiguity() {
    let f = Fixture::new();
    f.write(
        "a.ts",
        "export function work() {} export default function Default() {}\n",
    );
    f.write("b.ts", "export * from './a'; export * from './c';\n");
    f.write("c.ts", "export * from './b';\n");
    f.write(
        "main.ts",
        "import {work} from './c'; import Default from './c'; function Main(){work();Default();}\n",
    );
    let graph = f.index();
    assert!(calls(&graph, ("main.ts", "Main"), ("a.ts", "work")));
    assert!(f.unresolved(node(&graph, "main.ts", "Main"), "Default"));
    f.write("other.ts", "export function work() {}\n");
    f.write(
        "b.ts",
        "export * from './a'; export * from './other'; export * from './c';\n",
    );
    let graph = f.index();
    assert!(f.unresolved(node(&graph, "main.ts", "Main"), "work"));
    f.write(
        "b.ts",
        "export * from './a'; export * from './other'; export function work() {}\n",
    );
    let graph = f.index();
    assert!(calls(&graph, ("main.ts", "Main"), ("b.ts", "work")));
}
#[test]
fn commonjs_context_links_require_forwarding_and_respects_package_module_type() {
    let f = Fixture::new();
    f.write("package.json", r#"{"dependencies":{"dual":"file:./pkg"}}"#);
    f.write(
        "pkg/package.json",
        r#"{"name":"dual","exports":{"import":"./import.mjs","require":"./require.cjs"}}"#,
    );
    f.write("pkg/import.mjs", "export function work(){}\n");
    f.write("pkg/require.cjs", "exports.work=function work(){};\n");
    f.write(
        "use.cjs",
        "const dual=require('dual');function Use(){dual.work();}\n",
    );
    f.write(
        "use.mjs",
        "import {work} from 'dual';function Use(){work();}\n",
    );
    f.write("lib.cjs", "exports.work=function work(){};\n");
    f.write("barrel.cjs", "module.exports=require('./lib.cjs');\n");
    f.write(
        "main.cjs",
        "const lib=require('./barrel.cjs');function Main(){lib.work();}\n",
    );
    let graph = f.index();
    assert!(calls(&graph, ("main.cjs", "Main"), ("lib.cjs", "work")));
    assert!(calls(
        &graph,
        ("use.cjs", "Use"),
        ("pkg/require.cjs", "work")
    ));
    assert!(calls(
        &graph,
        ("use.mjs", "Use"),
        ("pkg/import.mjs", "work")
    ));
    f.write("package.json", r#"{"type":"module"}"#);
    f.write(
        "esm.js",
        "const lib=require('./lib.cjs');function ESM(){lib.work();}\n",
    );
    let graph = f.index();
    assert!(f.unresolved(node(&graph, "esm.js", "ESM"), "lib.work"));
    assert!(calls(&graph, ("main.cjs", "Main"), ("lib.cjs", "work")));
}

#[test]
fn node_shebang_uses_actual_path_for_local_require_context() {
    let f = Fixture::new();
    f.write("lib.cjs", "exports.work=function work(){};\n");
    f.write(
        "bin.v1/tool",
        "#!/usr/bin/env node\nconst lib=require('../lib.cjs');function Main(){lib.work();}\n",
    );
    let graph = f.index();
    assert!(calls(&graph, ("bin.v1/tool", "Main"), ("lib.cjs", "work")));
    assert_eq!(node(&graph, "bin.v1/tool", "Main").line, Some(2));
}

#[test]
fn xaml_project_context_refreshes_bindings_from_source_and_literal_namespace() {
    let f = Fixture::new();
    let project = "Shop/App.csproj";
    let vm = "Shop/ViewModels/OrdersViewModel.cs";
    let view = "Shop/Views/OrdersView.xaml";
    f.write(project, "<Project Sdk=\"Microsoft.NET.Sdk\"><PropertyGroup><RootNamespace>Shop</RootNamespace></PropertyGroup></Project>");
    let source = "using CommunityToolkit.Mvvm.ComponentModel; using CommunityToolkit.Mvvm.Input; namespace Shop.ViewModels; public partial class OrdersViewModel { [ObservableProperty] private string _customerName; [RelayCommand] private void Save() {} }";
    let xaml = "<Window xmlns:prism=\"http://prismlibrary.com/\" prism:ViewModelLocator.AutoWireViewModel=\"True\"><TextBlock Text=\"{Binding CustomerName}\"/><Button Command=\"{Binding SaveCommand}\"/></Window>";
    f.write(vm, source);
    f.write(view, xaml);
    let graph = f.index();
    assert!(file_relation(
        &graph,
        view,
        "view_model",
        (vm, "OrdersViewModel")
    ));
    assert!(file_relation(&graph, view, "binds", (vm, "CustomerName")));
    assert!(file_relation(
        &graph,
        view,
        "binds_command",
        (vm, "SaveCommand")
    ));
    assert_eq!(
        node(&graph, vm, "CustomerName").metadata["generated_by"],
        "CommunityToolkit.Mvvm.ComponentModel.ObservableProperty"
    );

    f.write(vm, &source.replace("_customerName", "_displayName"));
    assert!(
        index::check_update(&f.root(), &f.db())
            .unwrap()
            .changed
            .contains(&view.into())
    );
    let graph = f.index();
    assert!(
        !graph
            .nodes
            .iter()
            .any(|n| n.file == vm && n.label == "CustomerName")
    );
    assert!(!file_relation(&graph, view, "binds", (vm, "DisplayName")));
    let generated = |graph: &GraphSnapshot| -> BTreeMap<String, serde_json::Value> {
        graph
            .nodes
            .iter()
            .filter(|n| n.file == vm && n.metadata["generated_by"].is_string())
            .map(|n| (n.id.clone(), serde_json::to_value(n).unwrap()))
            .collect()
    };
    let generated_before = generated(&graph);
    assert!(!generated_before.is_empty());
    assert_eq!(
        node(&graph, vm, "DisplayName").metadata["generated_by"],
        "CommunityToolkit.Mvvm.ComponentModel.ObservableProperty"
    );
    f.write(view, &xaml.replace("CustomerName", "DisplayName"));
    let changed = index::check_update(&f.root(), &f.db()).unwrap().changed;
    assert!(changed.contains(&view.into()));
    // Editing a consumer binding does not change the C# declaration/generator proof.
    assert!(!changed.contains(&vm.into()));
    let graph = f.index();
    assert!(file_relation(&graph, view, "binds", (vm, "DisplayName")));
    assert!(file_relation(
        &graph,
        view,
        "binds_command",
        (vm, "SaveCommand")
    ));
    assert_eq!(generated(&graph), generated_before);

    f.write(
        project,
        "<Project><PropertyGroup><RootNamespace>Other</RootNamespace></PropertyGroup></Project>",
    );
    let graph = f.index();
    assert!(!file_relation(
        &graph,
        view,
        "view_model",
        (vm, "OrdersViewModel")
    ));
    for declaration in [
        "<RootNamespace>$(AssemblyName)</RootNamespace>",
        "<RootNamespace Condition=\"'$(Configuration)' == 'Debug'\">Shop</RootNamespace>",
    ] {
        f.write(
            project,
            &format!("<Project><PropertyGroup>{declaration}</PropertyGroup></Project>"),
        );
        let graph = f.index();
        assert!(!file_relation(
            &graph,
            view,
            "view_model",
            (vm, "OrdersViewModel")
        ));
        assert!(
            !graph
                .nodes
                .iter()
                .any(|n| n.file == vm && n.metadata["generated_by"].is_string())
        );
    }
}

#[test]
fn xaml_project_context_respects_ignored_nested_and_ambiguous_project_scopes() {
    let f = Fixture::new();
    let manifest =
        "<Project><PropertyGroup><RootNamespace>Shop</RootNamespace></PropertyGroup></Project>";
    let source = "namespace Shop.ViewModels; public class OrdersViewModel { public string Title {get;set;} }";
    let xaml = "<Window xmlns:x=\"http://schemas.microsoft.com/winfx/2006/xaml\" x:Class=\"Shop.Views.OrdersView\"><TextBlock Text=\"{Binding Title}\"/></Window>";
    f.write(".grafignore", "Shop/Ignored/\nLoose/*.csproj\n");
    f.write("Shop/App.csproj", manifest);
    f.write("Shop/ViewModels/OrdersViewModel.cs", source);
    f.write("Shop/Views/OrdersView.xaml", xaml);
    f.write("Shop/Nested/Nested.csproj", manifest);
    f.write("Shop/Nested/ViewModels/OrdersViewModel.cs", source);
    f.write("Shop/Nested/Views/OrdersView.xaml", xaml);
    f.write("Shop/Ignored/Bad.csproj", "not XML");
    f.write("Shop/Ignored/ViewModels/OrdersViewModel.cs", source);
    f.write("Loose/Excluded.csproj", manifest);
    f.write("Loose/ViewModels/OrdersViewModel.cs", source);
    f.write("Loose/Views/OrdersView.xaml", xaml);
    let graph = f.index();
    for prefix in ["Shop", "Shop/Nested"] {
        let view = format!("{prefix}/Views/OrdersView.xaml");
        let vm = format!("{prefix}/ViewModels/OrdersViewModel.cs");
        assert!(file_relation(
            &graph,
            &view,
            "view_model",
            (&vm, "OrdersViewModel")
        ));
    }
    assert!(!file_relation(
        &graph,
        "Shop/Views/OrdersView.xaml",
        "view_model",
        (
            "Shop/Nested/ViewModels/OrdersViewModel.cs",
            "OrdersViewModel"
        )
    ));
    assert!(!file_relation(
        &graph,
        "Loose/Views/OrdersView.xaml",
        "view_model",
        ("Loose/ViewModels/OrdersViewModel.cs", "OrdersViewModel")
    ));
    assert!(
        !graph
            .nodes
            .iter()
            .any(|n| n.file.starts_with("Shop/Ignored/"))
    );
    f.write("Shop/Second.csproj", manifest);
    let graph = f.index();
    assert!(!file_relation(
        &graph,
        "Shop/Views/OrdersView.xaml",
        "view_model",
        ("Shop/ViewModels/OrdersViewModel.cs", "OrdersViewModel")
    ));
    assert!(file_relation(
        &graph,
        "Shop/Nested/Views/OrdersView.xaml",
        "view_model",
        (
            "Shop/Nested/ViewModels/OrdersViewModel.cs",
            "OrdersViewModel"
        )
    ));
}

#[test]
fn explicit_native_javascript_imports_do_not_substitute_typescript_siblings() {
    let f = Fixture::new();
    for path in ["foo.js", "foo.ts", "module.mjs", "module.mts"] {
        f.write(path, "export function work() {}\n");
    }
    for path in ["common.cjs", "common.cts"] {
        f.write(path, "exports.work = function work() {};\n");
    }
    f.write("main.mjs", "import {work as js} from './foo.js'; import {work as mjs} from './module.mjs'; function Main(){js();mjs();}\n");
    f.write(
        "main.cjs",
        "const lib=require('./common.cjs');function Main(){lib.work();}\n",
    );
    f.write("typed.ts", "import {work as js} from './foo.js'; import {work as mjs} from './module.mjs'; function Main(){js();mjs();}\n");
    f.write(
        "typed.cts",
        "const lib=require('./common.cjs');function Main(){lib.work();}\n",
    );
    let graph = f.index();
    for (caller, target, wrong) in [
        ("main.mjs", "foo.js", "foo.ts"),
        ("main.mjs", "module.mjs", "module.mts"),
        ("main.cjs", "common.cjs", "common.cts"),
        ("typed.ts", "foo.ts", "foo.js"),
        ("typed.ts", "module.mts", "module.mjs"),
        ("typed.cts", "common.cts", "common.cjs"),
    ] {
        assert!(
            calls(&graph, (caller, "Main"), (target, "work")),
            "{caller} -> {target}"
        );
        assert!(
            !calls(&graph, (caller, "Main"), (wrong, "work")),
            "{caller} -> {wrong}"
        );
    }
}

#[test]
fn non_utf8_csharp_and_xaml_clear_old_facts_without_blocking_unrelated_updates() {
    for project in [false, true] {
        let f = Fixture::new();
        if project {
            f.write("App.csproj", "<Project />");
        }
        f.write("Model.cs", "public class Before {}\n");
        f.write(
            "View.xaml",
            "<Window><TextBlock Name=\"Before\" /></Window>",
        );
        f.write("other.ts", "export function before(){}\n");
        let graph = f.index();
        assert!(graph.nodes.iter().any(|n| n.file == "Model.cs"));
        assert!(graph.nodes.iter().any(|n| n.file == "View.xaml"));
        fs::write(f.root().join("Model.cs"), [0xff, 0xfe]).unwrap();
        fs::write(f.root().join("View.xaml"), [0xff]).unwrap();
        f.write("other.ts", "export function after(){}\n");
        assert!(!index::check_update(&f.root(), &f.db()).unwrap().fresh);
        let report = index::run_with_options(
            &f.root(),
            &f.db(),
            &IndexOptions {
                code_only: true,
                ..Default::default()
            },
        )
        .unwrap();
        for path in ["Model.cs", "View.xaml"] {
            assert!(
                report
                    .diagnostics
                    .iter()
                    .any(|d| d.file == path && d.message.contains("UTF-8"))
            );
        }
        let graph = Store::open(&f.db()).unwrap().snapshot().unwrap();
        assert!(
            !graph
                .nodes
                .iter()
                .any(|n| matches!(n.file.as_str(), "Model.cs" | "View.xaml"))
        );
        assert!(
            graph
                .nodes
                .iter()
                .any(|n| n.file == "other.ts" && n.label == "after")
        );
        assert!(index::check_update(&f.root(), &f.db()).unwrap().fresh);
        f.write("Model.cs", "public class Recovered {}\n");
        f.write(
            "View.xaml",
            "<Window><TextBlock Name=\"Recovered\" /></Window>",
        );
        let graph = f.index();
        assert!(
            graph
                .nodes
                .iter()
                .any(|n| n.file == "Model.cs" && n.label == "Recovered")
        );
        assert!(graph.nodes.iter().any(|n| n.file == "View.xaml"));
        assert!(index::check_update(&f.root(), &f.db()).unwrap().fresh);
    }
    let f = Fixture::new();
    fs::write(f.root().join("App.csproj"), [0xff]).unwrap();
    f.write("Model.cs", "public class Model {}\n");
    let error = index::run_with_options(
        &f.root(),
        &f.db(),
        &IndexOptions {
            code_only: true,
            ..Default::default()
        },
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains("project configuration is not UTF-8"));
}

#[test]
fn inherited_cargo_optional_dependencies_remain_unresolved_until_nonoptional() {
    let f = Fixture::new();
    f.write(
        "Cargo.toml",
        "[workspace]\nmembers=[\"app\",\"tool\"]\n[workspace.dependencies]\ntool={path=\"tool\"}\n",
    );
    f.write(
        "tool/Cargo.toml",
        "[package]\nname=\"tool\"\nversion=\"0.1.0\"\n",
    );
    f.write("tool/src/lib.rs", "pub fn work(){}\n");
    let manifest = "[package]\nname=\"app\"\nversion=\"0.1.0\"\n[dependencies]\ntool={workspace=true,optional=true}\n";
    f.write("app/Cargo.toml", manifest);
    f.write(
        "app/src/main.rs",
        "use tool::work; fn Main(){work();} fn Direct(){tool::work();}\n",
    );
    let graph = f.index();
    assert!(f.unresolved(node(&graph, "app/src/main.rs", "Main"), "work"));
    assert!(f.unresolved(node(&graph, "app/src/main.rs", "Direct"), "tool::work"));
    f.write(
        "app/Cargo.toml",
        &manifest.replace("optional=true", "optional=false"),
    );
    assert!(
        index::check_update(&f.root(), &f.db())
            .unwrap()
            .changed
            .contains(&"app/src/main.rs".into())
    );
    let graph = f.index();
    for caller in ["Main", "Direct"] {
        assert!(calls(
            &graph,
            ("app/src/main.rs", caller),
            ("tool/src/lib.rs", "work")
        ));
    }
    f.write("app/Cargo.toml", manifest);
    let graph = f.index();
    assert!(f.unresolved(node(&graph, "app/src/main.rs", "Main"), "work"));
    assert!(!calls(
        &graph,
        ("app/src/main.rs", "Main"),
        ("tool/src/lib.rs", "work")
    ));
    f.write(
        "app/Cargo.toml",
        &manifest.replace("workspace=true", "path=\"../tool\""),
    );
    let graph = f.index();
    assert!(f.unresolved(node(&graph, "app/src/main.rs", "Direct"), "tool::work"));
}

#[test]
fn configured_swift_modules_use_exact_roots_imports_and_nested_ownership() {
    let f = Fixture::new();
    f.write("Sources/Library/a.swift", "public func work() {}\n");
    f.write("Sources/Library/b.swift", "func run() { work() }\n");
    f.write(
        "Sources/Library/Nested/n.swift",
        "public func work() {}\nfunc own() { work() }\n",
    );
    f.write("Sources/App/main.swift", "import Core\nimport External\nfunc main() { work(); Core.work(); External.missing() }\nfunc shadow(work: () -> Void) { work() }\n");
    f.write(
        "Loose/tool.swift",
        "import Core\nfunc loose() { Core.work() }\n",
    );
    f.write("Other/Library/a.swift", "public func work() {}\n");
    let mut options = IndexOptions {
        code_only: true,
        swift_modules: BTreeMap::from([
            ("Core".into(), "Sources/Library".into()),
            ("Nested".into(), "Sources/Library/Nested".into()),
            ("App".into(), "Sources/App".into()),
        ]),
        ..Default::default()
    };
    let graph = f.index_with(&options);
    for caller in [
        ("Sources/Library/b.swift", "run"),
        ("Sources/App/main.swift", "main"),
    ] {
        assert!(calls(&graph, caller, ("Sources/Library/a.swift", "work")));
        assert!(!calls(
            &graph,
            caller,
            ("Sources/Library/Nested/n.swift", "work")
        ));
        assert!(!calls(&graph, caller, ("Other/Library/a.swift", "work")));
    }
    assert!(calls(
        &graph,
        ("Sources/Library/Nested/n.swift", "own"),
        ("Sources/Library/Nested/n.swift", "work")
    ));
    assert!(f.unresolved(
        node(&graph, "Sources/App/main.swift", "main"),
        "External.missing"
    ));
    assert!(f.unresolved(node(&graph, "Sources/App/main.swift", "shadow"), "work"));
    assert!(f.unresolved(node(&graph, "Loose/tool.swift", "loose"), "Core.work"));
    assert_eq!(
        index::stored_options(&f.db()).unwrap().swift_modules,
        options.swift_modules
    );
    assert!(index::check_update(&f.root(), &f.db()).unwrap().fresh);

    options
        .swift_modules
        .insert("Core".into(), "Other/Library".into());
    let graph = f.index_with(&options);
    assert!(calls(
        &graph,
        ("Sources/App/main.swift", "main"),
        ("Other/Library/a.swift", "work")
    ));
    assert!(!calls(
        &graph,
        ("Sources/App/main.swift", "main"),
        ("Sources/Library/a.swift", "work")
    ));
    f.write("Other/Library/new.swift", "public func added() {}\n");
    assert!(
        index::check_update(&f.root(), &f.db())
            .unwrap()
            .changed
            .contains(&"Sources/App/main.swift".into())
    );
    f.index_with(&options);
    fs::remove_file(f.root().join("Other/Library/a.swift")).unwrap();
    let graph = f.index_with(&options);
    assert!(f.unresolved(node(&graph, "Sources/App/main.swift", "main"), "Core.work"));
}

#[test]
fn swift_duplicate_roots_and_escaping_configuration_do_not_guess_modules() {
    let f = Fixture::new();
    f.write("src/a.swift", "public func work() {}\n");
    f.write("src/b.swift", "func run() { work() }\n");
    let mut options = IndexOptions {
        code_only: true,
        swift_modules: BTreeMap::from([("One".into(), "src".into()), ("Two".into(), "src".into())]),
        ..Default::default()
    };
    let graph = f.index_with(&options);
    assert!(f.unresolved(node(&graph, "src/b.swift", "run"), "work"));
    assert!(!calls(
        &graph,
        ("src/b.swift", "run"),
        ("src/a.swift", "work")
    ));
    options.swift_modules.remove("Two");
    let graph = f.index_with(&options);
    assert!(calls(
        &graph,
        ("src/b.swift", "run"),
        ("src/a.swift", "work")
    ));
    for root in ["../outside", "/absolute", "src/../outside", "src\\nested"] {
        options.swift_modules.insert("One".into(), root.into());
        assert!(
            index::run_with_options(&f.root(), &f.db(), &options).is_err(),
            "{root}"
        );
    }
}

#[test]
fn configured_component_and_namespace_type_references_use_exact_files() {
    let f = Fixture::new();
    f.write(
        "tsconfig.json",
        r#"{"compilerOptions":{"baseUrl":".","paths":{"@ui/*":["ui/*"],"@types":["types.ts"]}}}"#,
    );
    f.write("ui/Card.vue", "<template><div>Card</div></template>");
    f.write("ui/Card.ts", "export default function Card() {}");
    f.write(
        "types.ts",
        "export interface Shape {} export namespace API { export function run() {} }",
    );
    f.write("app.tsx", "import Card from '@ui/Card.vue'; import type {Shape} from '@types'; import {API} from '@types'; export interface Local extends Shape {} export function App() { API.run(); return <Card/>; } function Shadow(Card: unknown) { return <Card/>; }");
    let graph = f.index();
    assert!(file_relation(
        &graph,
        "app.tsx",
        "uses_component",
        ("ui/Card.vue", "Card")
    ));
    assert!(!file_relation(
        &graph,
        "app.tsx",
        "uses_component",
        ("ui/Card.ts", "Card")
    ));
    assert!(file_relation(
        &graph,
        "app.tsx",
        "inherits",
        ("types.ts", "Shape")
    ));
    assert!(calls(&graph, ("app.tsx", "App"), ("types.ts", "run")));
    assert!(!graph.edges.iter().any(
        |e| e.relation == "uses_component" && e.source == node(&graph, "app.tsx", "Shadow").id
    ));
    fs::remove_file(f.root().join("ui/Card.vue")).unwrap();
    let graph = f.index();
    assert!(!file_relation(
        &graph,
        "app.tsx",
        "uses_component",
        ("ui/Card.ts", "Card")
    ));
}

#[test]
fn go_explicit_imported_receivers_types_and_embeds_respect_export_visibility() {
    let f = Fixture::new();
    f.write("go.mod", "module example.org/app\n\ngo 1.22\n");
    f.write("wire/v2/api.go", "package transport\ntype Packet struct {}\nfunc (p Packet) Send() {}\nfunc (p Packet) hidden() {}\n");
    f.write("main.go", "package main\nimport \"example.org/app/wire/v2\"\ntype Wrapped struct { transport.Packet }\nfunc Send(p transport.Packet) { p.Send(); p.hidden() }\nfunc Shadow(transport interface{ Send() }) { transport.Send() }\n");
    let graph = f.index();
    assert!(calls(
        &graph,
        ("main.go", "Send"),
        ("wire/v2/api.go", "Send")
    ));
    assert!(!calls(
        &graph,
        ("main.go", "Send"),
        ("wire/v2/api.go", "hidden")
    ));
    assert!(file_relation(
        &graph,
        "main.go",
        "embeds",
        ("wire/v2/api.go", "Packet")
    ));
    assert!(file_relation(
        &graph,
        "main.go",
        "parameter_type",
        ("wire/v2/api.go", "Packet")
    ));
    assert!(f.unresolved(node(&graph, "main.go", "Send"), "p.hidden"));
    assert!(f.unresolved(node(&graph, "main.go", "Shadow"), "transport.Send"));
}

#[test]
fn rust_split_impls_and_public_use_forwarding_refresh_on_source_change() {
    let f = Fixture::new();
    f.write("Cargo.toml", "[workspace]\nmembers = [\"core\", \"app\"]\n");
    f.write(
        "core/Cargo.toml",
        "[package]\nname = \"engine\"\nversion = \"0.1.0\"\n",
    );
    f.write("core/src/lib.rs", "mod state; mod apply; mod save; pub use crate::state::State as Engine; pub use crate::apply::start;");
    f.write("core/src/state.rs", "pub struct State;");
    f.write("core/src/apply.rs", "use crate::state::State; impl State { pub fn apply(&self) { self.save(); } } pub fn start() {} ");
    f.write(
        "core/src/save.rs",
        "use crate::state::State; impl State { pub fn save(&self) {} } ",
    );
    f.write(
        "core/src/orphan.rs",
        "use crate::state::State; impl State { pub fn ghost(&self) {} }",
    );
    f.write("app/Cargo.toml", "[package]\nname = \"app\"\nversion = \"0.1.0\"\n[dependencies]\nengine = { path = \"../core\" }\n");
    f.write("app/src/lib.rs", "use engine::Engine; pub fn run() { Engine::apply(); Engine::save(); Engine::ghost(); engine::start(); }");
    let graph = f.index();
    assert!(calls(
        &graph,
        ("core/src/apply.rs", "apply"),
        ("core/src/save.rs", "save")
    ));
    assert!(calls(
        &graph,
        ("app/src/lib.rs", "run"),
        ("core/src/apply.rs", "apply")
    ));
    assert!(calls(
        &graph,
        ("app/src/lib.rs", "run"),
        ("core/src/save.rs", "save")
    ));
    assert!(calls(
        &graph,
        ("app/src/lib.rs", "run"),
        ("core/src/apply.rs", "start")
    ));
    assert!(!calls(
        &graph,
        ("app/src/lib.rs", "run"),
        ("core/src/orphan.rs", "ghost")
    ));
    let before = rust_file_stamps(&f);
    f.write("core/src/lib.rs", "mod state; mod apply; mod save; pub use crate::state::State as Renamed; pub use crate::apply::start;");
    let changed = index::check_update(&f.root(), &f.db()).unwrap().changed;
    assert!(changed.iter().any(|p| p == "core/src/apply.rs"));
    assert!(!changed.iter().any(|p| p == "app/src/lib.rs"));
    let graph = f.index();
    assert_eq!(
        before["app/src/lib.rs"],
        rust_file_stamps(&f)["app/src/lib.rs"]
    );
    assert!(f.unresolved(node(&graph, "app/src/lib.rs", "run"), "Engine::apply"));
    assert!(calls(
        &graph,
        ("core/src/apply.rs", "apply"),
        ("core/src/save.rs", "save")
    ));
}

#[test]
fn rust_cached_reexports_resolve_local_calls_to_terminal_definitions() {
    let f = Fixture::new();
    f.write(
        "Cargo.toml",
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    let check = |graph: &GraphSnapshot, expected: Option<&str>| {
        let caller = node(graph, "src/lib.rs", "caller");
        let outgoing: Vec<_> = graph
            .edges
            .iter()
            .filter(|edge| edge.relation == "calls" && edge.source == caller.id)
            .collect();
        assert_eq!(outgoing.len(), usize::from(expected.is_some()));
        assert_eq!(f.unresolved(caller, "crate::exposed"), expected.is_none());
        if let Some(target) = expected {
            assert!(calls(
                graph,
                ("src/lib.rs", "caller"),
                ("src/lib.rs", target)
            ));
            assert_eq!(node(graph, "src/lib.rs", target).kind, "function");
        }
        for reexport in graph.nodes.iter().filter(|node| node.kind == "reexport") {
            assert!(reexport.binding_key.is_none());
            assert!(reexport.metadata.get("binding_aliases").is_none());
        }
    };
    // Change the terminal target, remove the alias, remove its definition, restore.
    for (source, expected) in [
        (
            "mod inner { pub fn work() {} } pub use crate::inner::work as exposed;",
            Some("work"),
        ),
        (
            "mod inner { pub fn other() {} } pub use crate::inner::other as exposed;",
            Some("other"),
        ),
        ("mod inner { pub fn work() {} }", None),
        ("mod inner {} pub use crate::inner::work as exposed;", None),
        (
            "mod inner { pub fn work() {} } pub use crate::inner::work as exposed;",
            Some("work"),
        ),
    ] {
        f.write(
            "src/lib.rs",
            &format!("{source}\npub fn caller() {{ crate::exposed(); }}\n"),
        );
        let graph = f.index();
        check(&graph, expected);
        let noop = f.index();
        check(&noop, expected);
        assert_eq!(noop.generation, graph.generation);
        check(
            &f.index_with(&IndexOptions {
                code_only: true,
                force: true,
                ..Default::default()
            }),
            expected,
        );
        assert!(index::check_update(&f.root(), &f.db()).unwrap().fresh);
    }
}

#[test]
fn rust_reexport_package_aliases_follow_enclosing_module_visibility() {
    // Exercise both visibility states on a cold index, then change them without
    // touching the binary consumer. Repeat each outcome on no-op and forced runs.
    for initially_public in [false, true] {
        let f = Fixture::new();
        f.write(
            "Cargo.toml",
            "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        );
        f.write("src/main.rs", "fn main() { demo::hidden::exposed(); }\n");
        let check = |graph: &GraphSnapshot, resolved: bool| {
            let caller = node(graph, "src/main.rs", "main");
            let outgoing: Vec<_> = graph
                .edges
                .iter()
                .filter(|edge| edge.relation == "calls" && edge.source == caller.id)
                .collect();
            assert_eq!(outgoing.len(), usize::from(resolved));
            assert_eq!(f.unresolved(caller, "demo::hidden::exposed"), !resolved);
            if resolved {
                assert!(calls(
                    graph,
                    ("src/main.rs", "main"),
                    ("src/lib.rs", "work")
                ));
                assert_eq!(node(graph, "src/lib.rs", "work").kind, "function");
            }
        };
        let exported = "pub fn work() {} pub use self::work as exposed;";
        for (public, body, resolved) in [
            (initially_public, exported, initially_public),
            (!initially_public, exported, !initially_public),
            (initially_public, exported, initially_public),
            (true, "pub fn work() {}", false),
            (true, "pub use self::work as exposed;", false),
            (true, exported, true),
            (false, exported, false),
        ] {
            let visibility = if public { "pub " } else { "" };
            f.write(
                "src/lib.rs",
                &format!("{visibility}mod hidden {{ {body} }}\n"),
            );
            let graph = f.index();
            check(&graph, resolved);
            let noop = f.index();
            check(&noop, resolved);
            assert_eq!(noop.generation, graph.generation);
            check(
                &f.index_with(&IndexOptions {
                    code_only: true,
                    force: true,
                    ..Default::default()
                }),
                resolved,
            );
            assert!(index::check_update(&f.root(), &f.db()).unwrap().fresh);
        }
    }
}

#[test]
fn rust_standalone_add_delete_rebinds_without_reparsing_the_family() {
    let f = Fixture::new();
    f.write("a.rs", "fn a() {}\n");
    f.write("caller.rs", "fn caller() { a(); }\n");
    let first = index::run_with_options(
        &f.root(),
        &f.db(),
        &IndexOptions {
            code_only: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(first.parsed_files, 2);

    f.write("added.rs", "fn added() {}\n");
    let added = index::run_with_options(
        &f.root(),
        &f.db(),
        &IndexOptions {
            code_only: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!((added.parsed_files, added.unchanged_files), (1, 2));

    fs::remove_file(f.root().join("added.rs")).unwrap();
    let deleted = index::run_with_options(
        &f.root(),
        &f.db(),
        &IndexOptions {
            code_only: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!((deleted.parsed_files, deleted.unchanged_files), (0, 2));
    assert_eq!(deleted.deleted_files, 1);
}

fn rust_file_stamps(f: &Fixture) -> BTreeMap<String, String> {
    Store::open_read_only(&f.db())
        .unwrap()
        .file_stamps()
        .unwrap()
        .into_iter()
        .map(|stamp| (stamp.path, stamp.hash))
        .collect()
}

fn rust_stored_outcome(db: &std::path::Path) -> serde_json::Value {
    let mut graph = Store::open_read_only(db).unwrap().snapshot().unwrap();
    graph.generation = 0;
    graph.root = None;
    let connection =
        rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    let connection = connection.unchecked_transaction().unwrap();
    // Include every persisted reference, its resolution and ordered keys, not
    // just the unresolved references exposed in a graph snapshot. Compare
    // public identities rather than incidental integer allocation order.
    let physical_version: u32 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    let relationship_queries = match physical_version {
        1 => [
            "SELECT json_array(id,source,owner_file,relation,payload,resolved_target,resolution_reason) FROM refs ORDER BY id",
            "SELECT json_array(ref_id,priority,binding_key) FROM ref_keys ORDER BY ref_id,priority",
            "SELECT json_array(node_id,binding_key) FROM node_aliases ORDER BY node_id,binding_key",
        ],
        2 | 3 => [
            "SELECT json_array(r.id,s.id,f.path,r.relation,r.payload,t.id,r.resolution_reason) FROM refs r JOIN nodes s ON s.nkey=r.source_key JOIN files f ON f.fkey=r.owner_key LEFT JOIN nodes t ON t.nkey=r.resolved_target_key ORDER BY r.id",
            "SELECT json_array(r.id,k.priority,k.binding_key) FROM ref_keys k JOIN refs r ON r.rkey=k.ref_key ORDER BY r.id,k.priority",
            "SELECT json_array(n.id,a.binding_key) FROM node_aliases a JOIN nodes n ON n.nkey=a.node_key ORDER BY n.id,a.binding_key",
        ],
        other => panic!("unexpected physical format {other}"),
    };
    assert!(
        connection
            .prepare("PRAGMA foreign_key_check")
            .unwrap()
            .query([])
            .unwrap()
            .next()
            .unwrap()
            .is_none()
    );
    if matches!(physical_version, 2 | 3) {
        let dangling: i64 = connection
            .query_row(
                "SELECT count(*) FROM refs r LEFT JOIN nodes n ON n.nkey=r.resolved_target_key WHERE r.resolved_target_key IS NOT NULL AND n.nkey IS NULL",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(dangling, 0, "resolved references must retain their targets");
    }
    let mut records = vec![];
    for query in
        std::iter::once("SELECT json_array(path,hash,module,diagnostics) FROM files ORDER BY path")
            .chain(relationship_queries)
    {
        records.push(
            connection
                .prepare(query)
                .unwrap()
                .query_map([], |row| row.get::<_, String>(0))
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap(),
        );
    }
    for (rows, table) in records
        .iter()
        .zip(["files", "refs", "ref_keys", "node_aliases"])
    {
        let count: i64 = connection
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(
            i64::try_from(rows.len()).unwrap(),
            count,
            "every {table} row must remain visible"
        );
    }
    serde_json::json!({"graph": graph, "records": records})
}

fn rust_assert_fresh_forced_equivalent(f: &Fixture) {
    let expected = rust_stored_outcome(&f.db());
    f.index_with(&IndexOptions {
        code_only: true,
        force: true,
        ..Default::default()
    });
    assert_eq!(expected, rust_stored_outcome(&f.db()), "forced output");
    let fresh = tempdir().unwrap();
    let db = fresh.path().join("fresh.db");
    index::run_with_options(
        &f.root(),
        &db,
        &IndexOptions {
            code_only: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(expected, rust_stored_outcome(&db), "fresh output");
    assert!(index::check_update(&f.root(), &f.db()).unwrap().fresh);
    let generation = Store::open_read_only(&f.db())
        .unwrap()
        .stats()
        .unwrap()
        .generation;
    assert_eq!(f.index().generation, generation, "no-op generation");
}

fn rust_incremental_fixture() -> Fixture {
    let f = Fixture::new();
    f.write(
        "Cargo.toml",
        "[package]\nname='demo'\nversion='0.1.0'\nedition='2021'\n",
    );
    f.write(
        "src/lib.rs",
        "pub mod builder; pub use crate::builder::Command; mod caller; mod sibling;\n",
    );
    f.write(
        "src/builder/mod.rs",
        "mod command; pub use command::Command;\n",
    );
    f.write(
        "src/builder/command.rs",
        "pub struct Command; impl Command { pub fn new() -> Self { Self } }\n",
    );
    f.write(
        "src/caller.rs",
        "use crate::Command; pub fn caller() { Command::new(); }\n",
    );
    f.write("src/sibling.rs", "pub fn unrelated() {}\n");
    let graph = f.index();
    assert!(calls(
        &graph,
        ("src/caller.rs", "caller"),
        ("src/builder/command.rs", "new")
    ));
    f
}

#[test]
fn rust_provider_body_and_comment_edits_keep_unchanged_outcomes() {
    let f = rust_incremental_fixture();
    for provider in [
        "pub struct Command; impl Command { pub fn new() -> Self { let _value = 1; Self } }\n",
        "// Move the terminal method without changing its exported path.\npub struct Command; impl Command { pub fn new() -> Self { let _value = 1; Self } }\n",
    ] {
        let before = rust_file_stamps(&f);
        let old = Store::open_read_only(&f.db()).unwrap().snapshot().unwrap();
        let old_target = node(&old, "src/builder/command.rs", "new").id.clone();
        f.write("src/builder/command.rs", provider);
        assert_eq!(
            index::check_update(&f.root(), &f.db()).unwrap().changed,
            ["src/builder/command.rs"]
        );
        let report = index::run_with_options(
            &f.root(),
            &f.db(),
            &IndexOptions {
                code_only: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!((report.parsed_files, report.unchanged_files), (1, 5));
        let after = rust_file_stamps(&f);
        for (path, stamp) in before {
            assert_eq!(
                stamp == after[&path],
                path != "src/builder/command.rs",
                "{path}"
            );
        }
        let graph = Store::open_read_only(&f.db()).unwrap().snapshot().unwrap();
        assert!(calls(
            &graph,
            ("src/caller.rs", "caller"),
            ("src/builder/command.rs", "new")
        ));
        assert!(!f.unresolved(node(&graph, "src/caller.rs", "caller"), "Command::new"));
        let target = node(&graph, "src/builder/command.rs", "new");
        if provider.starts_with("//") {
            assert_ne!(old_target, target.id);
            assert!(!graph.edges.iter().any(|edge| edge.target == old_target));
        }
        rust_assert_fresh_forced_equivalent(&f);
    }
}

#[test]
fn rust_unrelated_file_and_module_add_delete_keep_existing_outcomes() {
    let f = rust_incremental_fixture();
    let baseline = rust_file_stamps(&f);
    let root = "pub mod builder; pub use crate::builder::Command; mod caller; mod sibling;\n";
    for declared in [false, true] {
        f.write("src/unrelated.rs", "pub fn elsewhere() {}\n");
        if declared {
            f.write("src/lib.rs", &format!("{root}pub mod unrelated;\n"));
        }
        let added = index::run_with_options(
            &f.root(),
            &f.db(),
            &IndexOptions {
                code_only: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            (added.parsed_files, added.unchanged_files),
            if declared { (2, 5) } else { (1, 6) }
        );
        let stamps = rust_file_stamps(&f);
        for (path, stamp) in &baseline {
            assert_eq!(
                stamp == &stamps[path],
                !declared || path != "src/lib.rs",
                "{path}"
            );
        }
        let graph = Store::open_read_only(&f.db()).unwrap().snapshot().unwrap();
        assert!(calls(
            &graph,
            ("src/caller.rs", "caller"),
            ("src/builder/command.rs", "new")
        ));
        rust_assert_fresh_forced_equivalent(&f);

        fs::remove_file(f.root().join("src/unrelated.rs")).unwrap();
        if declared {
            f.write("src/lib.rs", root);
        }
        let deleted = index::run_with_options(
            &f.root(),
            &f.db(),
            &IndexOptions {
                code_only: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            (
                deleted.parsed_files,
                deleted.unchanged_files,
                deleted.deleted_files
            ),
            if declared { (1, 5, 1) } else { (0, 6, 1) }
        );
        assert_eq!(baseline, rust_file_stamps(&f));
        let graph = Store::open_read_only(&f.db()).unwrap().snapshot().unwrap();
        assert!(
            !graph
                .nodes
                .iter()
                .any(|node| node.file == "src/unrelated.rs")
        );
        assert!(calls(
            &graph,
            ("src/caller.rs", "caller"),
            ("src/builder/command.rs", "new")
        ));
        rust_assert_fresh_forced_equivalent(&f);
    }
}

#[test]
fn rust_outcomes_retract_aliases_generic_impls_and_cargo_origins() {
    let f = rust_incremental_fixture();
    let manifest = "[package]\nname='app'\nversion='0.1.0'\n[dependencies]\ndemo={path='..'}\n";
    f.write("app/Cargo.toml", manifest);
    f.write("app/src/lib.rs", "pub fn root_call() { demo::Command::new(); } pub fn module_call() { demo::builder::Command::new(); }\n");
    f.index();
    let root = "pub mod builder; pub use crate::builder::Command; mod caller; mod sibling;\n";
    for (source, root_visible, module_visible) in [
        ("pub mod builder; mod caller; mod sibling;\n", false, true),
        (root, true, true),
        (
            "mod builder; pub use crate::builder::Command; mod caller; mod sibling;\n",
            true,
            false,
        ),
        (root, true, true),
    ] {
        let before = rust_file_stamps(&f);
        f.write("src/lib.rs", source);
        let changed = index::check_update(&f.root(), &f.db()).unwrap().changed;
        assert!(changed.iter().any(|path| path == "src/builder/command.rs"));
        assert!(!changed.iter().any(|path| path == "app/src/lib.rs"));
        let graph = f.index();
        let after = rust_file_stamps(&f);
        assert_ne!(
            before["src/builder/command.rs"],
            after["src/builder/command.rs"]
        );
        assert_eq!(before["app/src/lib.rs"], after["app/src/lib.rs"]);
        for (caller, visible) in [("root_call", root_visible), ("module_call", module_visible)] {
            assert_eq!(
                calls(
                    &graph,
                    ("app/src/lib.rs", caller),
                    ("src/builder/command.rs", "new")
                ),
                visible
            );
            let source = node(&graph, "app/src/lib.rs", caller);
            let calls: Vec<_> = graph
                .edges
                .iter()
                .filter(|edge| edge.source == source.id && edge.relation == "calls")
                .collect();
            assert_eq!(calls.len(), usize::from(visible));
            assert_eq!(
                f.unresolved(
                    source,
                    if caller == "root_call" {
                        "demo::Command::new"
                    } else {
                        "demo::builder::Command::new"
                    }
                ),
                !visible
            );
        }
        rust_assert_fresh_forced_equivalent(&f);
    }
    for optional in [true, false] {
        f.write(
            "app/Cargo.toml",
            &manifest.replace(
                "path='..'",
                if optional {
                    "path='..',optional=true"
                } else {
                    "path='..'"
                },
            ),
        );
        assert!(
            index::check_update(&f.root(), &f.db())
                .unwrap()
                .changed
                .iter()
                .any(|path| path == "app/src/lib.rs")
        );
        let graph = f.index();
        for caller in ["root_call", "module_call"] {
            assert_eq!(
                calls(
                    &graph,
                    ("app/src/lib.rs", caller),
                    ("src/builder/command.rs", "new")
                ),
                !optional
            );
        }
        rust_assert_fresh_forced_equivalent(&f);
    }

    let g = Fixture::new();
    g.write("Cargo.toml", "[package]\nname='generic'\nversion='0.1.0'\n");
    g.write("src/lib.rs", "pub mod model; mod provider; mod caller;\n");
    let model = "pub struct Register<A, B>(pub A, pub B);\n";
    g.write("src/model.rs", model);
    g.write(
        "src/provider.rs",
        "use crate::model::Register; impl<X, Y> Register<X, Y> { pub fn empty() {} }\n",
    );
    g.write(
        "src/caller.rs",
        "use crate::model::Register; pub fn caller() { Register::empty(); }\n",
    );
    assert!(calls(
        &g.index(),
        ("src/caller.rs", "caller"),
        ("src/provider.rs", "empty")
    ));
    for (source, supported) in [("pub struct Register<A>(pub A);\n", false), (model, true)] {
        let before = rust_file_stamps(&g);
        g.write("src/model.rs", source);
        assert!(
            index::check_update(&g.root(), &g.db())
                .unwrap()
                .changed
                .iter()
                .any(|path| path == "src/provider.rs")
        );
        let graph = g.index();
        let after = rust_file_stamps(&g);
        assert_ne!(before["src/provider.rs"], after["src/provider.rs"]);
        assert_eq!(before["src/caller.rs"], after["src/caller.rs"]);
        assert_eq!(
            node(&graph, "src/provider.rs", "empty")
                .binding_key
                .is_some(),
            supported
        );
        assert_eq!(
            calls(
                &graph,
                ("src/caller.rs", "caller"),
                ("src/provider.rs", "empty")
            ),
            supported
        );
        assert_eq!(
            g.unresolved(node(&graph, "src/caller.rs", "caller"), "Register::empty"),
            !supported
        );
        rust_assert_fresh_forced_equivalent(&g);
    }
}

#[test]
fn rust_declared_module_file_addition_still_refreshes_context_dependents() {
    let f = Fixture::new();
    f.write(
        "Cargo.toml",
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n",
    );
    f.write("src/lib.rs", "mod caller;\nmod target;\n");
    f.write("src/caller.rs", "fn caller() { crate::target::work(); }\n");
    let initial = f.index();
    assert!(f.unresolved(
        node(&initial, "src/caller.rs", "caller"),
        "crate::target::work"
    ));

    // Root visibility changes and a missing declared module acquires a source.
    // The caller's previously rejected candidate must now be restored.
    f.write("src/lib.rs", "mod caller;\npub mod target;\n");
    f.write("src/target.rs", "pub fn work() {}\n");
    let report = index::run_with_options(
        &f.root(),
        &f.db(),
        &IndexOptions {
            code_only: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(report.parsed_files, 3);
    let graph = Store::open(&f.db()).unwrap().snapshot().unwrap();
    assert!(calls(
        &graph,
        ("src/caller.rs", "caller"),
        ("src/target.rs", "work")
    ));
}

#[test]
fn rust_public_use_never_selects_ambiguous_private_or_optional_targets() {
    let f = Fixture::new();
    f.write("Cargo.toml", "[workspace]\nmembers = [\"core\", \"app\"]\n");
    f.write(
        "core/Cargo.toml",
        "[package]\nname = \"engine\"\nversion = \"0.1.0\"\n",
    );
    f.write("core/src/lib.rs", "mod a; mod b; pub use crate::a::run as duplicate; pub use crate::b::run as duplicate; pub use crate::a::secret; pub use crate::a::run as okay;");
    f.write("core/src/a.rs", "pub fn run() {} fn secret() {}");
    f.write("core/src/b.rs", "pub fn run() {}");
    f.write("app/Cargo.toml", "[package]\nname = \"app\"\nversion = \"0.1.0\"\n[dependencies]\nengine = { path = \"../core\" }\n");
    f.write(
        "app/src/lib.rs",
        "pub fn call() { engine::duplicate(); engine::secret(); engine::okay(); }",
    );
    let graph = f.index();
    assert!(f.unresolved(node(&graph, "app/src/lib.rs", "call"), "engine::duplicate"));
    assert!(f.unresolved(node(&graph, "app/src/lib.rs", "call"), "engine::secret"));
    assert!(calls(
        &graph,
        ("app/src/lib.rs", "call"),
        ("core/src/a.rs", "run")
    ));
    f.write("app/Cargo.toml", "[package]\nname = \"app\"\nversion = \"0.1.0\"\n[dependencies]\nengine = { path = \"../core\", optional = true }\n");
    let graph = f.index();
    assert!(f.unresolved(node(&graph, "app/src/lib.rs", "call"), "engine::okay"));
}

#[test]
fn terraform_context_is_used_by_index_and_refreshes_local_module_outputs() {
    let f = Fixture::new();
    f.write(
        "env/main.tf",
        "module \"app\" { source = \"../app\" }\noutput \"result\" { value = module.app.id }\n",
    );
    f.write("app/main.tf", "output \"id\" { value = \"first\" }\n");
    f.write("other/main.tf", "output \"id\" { value = \"other\" }\n");
    // `env` is excluded as generated content unless the caller opts in.
    let excluded = f.index();
    assert!(!excluded.nodes.iter().any(|n| n.file == "env/main.tf"));
    let options = IndexOptions {
        code_only: true,
        include_generated: true,
        ..Default::default()
    };
    let graph = f.index_with(&options);
    assert!(graph.nodes.iter().any(|n| n.file == "env/main.tf"));
    assert!(file_relation(
        &graph,
        "env/main.tf",
        "module_source",
        ("app/main.tf", "Terraform module: app")
    ));
    assert!(file_relation(
        &graph,
        "env/main.tf",
        "references",
        ("app/main.tf", "output.id")
    ));
    f.write(
        "app/main.tf",
        "output \"renamed\" { value = \"changed\" }\n",
    );
    assert!(
        index::check_update(&f.root(), &f.db())
            .unwrap()
            .changed
            .iter()
            .any(|p| p == "env/main.tf")
    );
    let graph = f.index_with(&options);
    assert!(!file_relation(
        &graph,
        "env/main.tf",
        "references",
        ("app/main.tf", "output.renamed")
    ));
    assert!(!file_relation(
        &graph,
        "env/main.tf",
        "references",
        ("other/main.tf", "output.id")
    ));
    f.write(
        "env/main.tf",
        "module \"app\" { source = \"../other\" }\noutput \"result\" { value = module.app.id }\n",
    );
    let graph = f.index_with(&options);
    assert!(file_relation(
        &graph,
        "env/main.tf",
        "references",
        ("other/main.tf", "output.id")
    ));
}

#[test]
fn cargo_package_graph_refreshes_unchanged_manifests_on_add_change_and_delete() {
    let f = Fixture::new();
    f.write("Cargo.toml", "[workspace]\nmembers = [\"app\", \"lib\"]\n");
    f.write("app/Cargo.toml", "[package]\nname=\"app\"\nversion=\"0.1.0\"\n[dependencies]\ncore={package=\"library\",path=\"../lib\"}\n");
    let graph = f.index();
    assert!(!graph.edges.iter().any(|e| e.relation == "crate_depends_on"));
    f.write(
        "lib/Cargo.toml",
        "[package]\nname=\"library\"\nversion=\"0.1.0\"\n",
    );
    let refresh = || {
        assert!(
            index::check_update(&f.root(), &f.db())
                .unwrap()
                .changed
                .iter()
                .any(|p| p == "app/Cargo.toml")
        )
    };
    refresh();
    let graph = f.index();
    for relation in ["depends_on", "crate_depends_on"] {
        assert!(file_relation(
            &graph,
            "app/Cargo.toml",
            relation,
            ("lib/Cargo.toml", "library")
        ));
    }
    assert!(graph.edges.iter().any(|e| e.relation == "crate_depends_on"
        && e.metadata["alias"] == "core"
        && e.metadata["context"] == "cargo_dependency"));
    f.write(
        "lib/Cargo.toml",
        "[package]\nname=\"renamed\"\nversion=\"0.1.0\"\n",
    );
    refresh();
    let graph = f.index();
    assert!(!graph.edges.iter().any(|e| e.relation == "crate_depends_on"));
    f.write(
        "lib/Cargo.toml",
        "[package]\nname=\"library\"\nversion=\"0.1.0\"\n",
    );
    f.index();
    fs::remove_file(f.root().join("lib/Cargo.toml")).unwrap();
    refresh();
    let graph = f.index();
    assert!(!graph.nodes.iter().any(|n| n.file == "lib/Cargo.toml"));
    assert!(!graph.edges.iter().any(|e| e.relation == "crate_depends_on"));
}

#[test]
fn javascript_receiver_calls_resolve_exact_files_and_refresh_changed_members() {
    let f = Fixture::new();
    f.write("service.ts", "export class Service { run() {} static create() {} private hidden() {} } export function local() { const x = new Service(); x.run(); }");
    f.write("service.mjs", "export class Service { run() {} static create() {} } export function local() { const x = new Service(); x.run(); }");
    f.write(
        "default.ts",
        "export default class Hidden { forbidden() {} }",
    );
    f.write("barrel.ts", "export * from './default';");
    f.write(
        "main.ts",
        r#"
import {Service as Typed} from './service.ts';
import {Service as Native} from './service.mjs';
import type {Service as External} from 'external';
import Hidden from './barrel';
export function noDefault() { const x = new Hidden(); x.forbidden(); }
export function typed(x: Typed) { x.run(); x.hidden(); Typed.create(); }
export function native() { const x = new Native(); x.run(); }
export function external(x: External) { x.run(); }
export function untyped(x) { x.run(); }
"#,
    );
    let graph = f.index();
    for (from, to) in [
        (("service.ts", "local"), ("service.ts", "run")),
        (("service.mjs", "local"), ("service.mjs", "run")),
        (("main.ts", "typed"), ("service.ts", "run")),
        (("main.ts", "typed"), ("service.ts", "create")),
        (("main.ts", "native"), ("service.mjs", "run")),
    ] {
        assert!(calls(&graph, from, to), "{from:?} -> {to:?}");
    }
    assert!(!calls(
        &graph,
        ("service.ts", "local"),
        ("service.mjs", "run")
    ));
    assert!(!calls(
        &graph,
        ("service.mjs", "local"),
        ("service.ts", "run")
    ));
    for owner in ["external", "untyped"] {
        assert!(f.unresolved(node(&graph, "main.ts", owner), "x.run"));
    }
    assert!(f.unresolved(node(&graph, "main.ts", "typed"), "x.hidden"));
    assert!(f.unresolved(node(&graph, "main.ts", "noDefault"), "x.forbidden"));
    f.write(
        "service.ts",
        "export class Service { renamed() {} static create() {} private hidden() {} }",
    );
    assert!(
        !index::check_update(&f.root(), &f.db())
            .unwrap()
            .changed
            .iter()
            .any(|p| p == "main.ts")
    );
    let graph = f.index();
    assert!(f.unresolved(node(&graph, "main.ts", "typed"), "x.run"));
    assert!(calls(&graph, ("main.ts", "native"), ("service.mjs", "run")));
    javascript_assert_fresh_equivalent(&f, &graph);
}

#[test]
fn javascript_citations_exist_without_documents_and_update_without_stale_edges() {
    let f = Fixture::new();
    f.write(
        "main.ts",
        "export function run() {\n // WHY: ADR12 and rfc2119\n}\n",
    );
    let graph = f.index();
    let citation = node(&graph, "main.ts", "ADR-0012");
    assert_eq!(citation.kind, "doc_ref");
    assert!(
        graph
            .edges
            .iter()
            .any(|e| e.relation == "cites" && e.target == citation.id)
    );
    assert!(graph.nodes.iter().all(|n| n.file == "main.ts"));
    f.write("main.ts", "export function run() {\n // TODO: RFC 822\n}\n");
    let graph = f.index();
    assert!(
        !graph
            .nodes
            .iter()
            .any(|n| n.label == "ADR-0012" || n.label == "RFC-2119")
    );
    let citation = node(&graph, "main.ts", "RFC-822");
    assert!(
        graph
            .edges
            .iter()
            .any(|e| e.relation == "cites" && e.target == citation.id)
    );
}

#[test]
fn rust_constants_follow_public_use_and_actual_module_boundaries() {
    let f = Fixture::new();
    f.write(
        "Cargo.toml",
        "[package]\nname=\"library\"\nversion=\"0.1.0\"\n",
    );
    f.write("src/lib.rs", "mod values; pub use crate::values::DEFAULT as DEFAULT_VALUE; pub use crate::values::Value as PublicValue;");
    f.write("src/values.rs", "pub struct Value; pub const DEFAULT: Value = Value; impl Value { pub const EMPTY: Self = Value; const PRIVATE: Self = Value; }");
    f.write(
        "src/orphan.rs",
        "use crate::values::Value; impl Value { pub const ORPHAN: Self = Value; }",
    );
    let graph = f.index();
    let aliases = |label: &str| {
        node(&graph, "src/values.rs", label).metadata["binding_aliases"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    assert!(
        aliases("DEFAULT")
            .iter()
            .any(|k| k.ends_with(":DEFAULT_VALUE"))
    );
    assert!(
        aliases("EMPTY")
            .iter()
            .any(|k| k.ends_with(":PublicValue::EMPTY"))
    );
    assert!(
        !node(&graph, "src/values.rs", "PRIVATE").metadata["binding_aliases"]
            .as_array()
            .is_some_and(|keys| keys.iter().any(|k| k
                .as_str()
                .is_some_and(|k| k.ends_with(":PublicValue::PRIVATE"))))
    );
    assert!(
        node(&graph, "src/orphan.rs", "ORPHAN")
            .binding_key
            .is_none()
    );
    f.write("src/lib.rs", "mod values; pub use crate::values::DEFAULT as CHANGED; pub use crate::values::Value as PublicValue;");
    assert!(
        index::check_update(&f.root(), &f.db())
            .unwrap()
            .changed
            .iter()
            .any(|p| p == "src/values.rs")
    );
    let graph = f.index();
    let values = node(&graph, "src/values.rs", "DEFAULT").metadata["binding_aliases"]
        .as_array()
        .unwrap();
    assert!(
        values
            .iter()
            .any(|v| v.as_str().is_some_and(|k| k.ends_with(":CHANGED")))
    );
    assert!(
        !values
            .iter()
            .any(|v| v.as_str().is_some_and(|k| k.ends_with(":DEFAULT_VALUE")))
    );
}

#[test]
fn compiled_csharp_project_units_refresh_proof_but_ignore_body_comments() {
    let f = Fixture::new();
    f.write(
        "App/App.csproj",
        "<Project><PropertyGroup><RootNamespace>api</RootNamespace></PropertyGroup></Project>",
    );
    let base = "namespace api; public class Base { public void work() {} }";
    f.write("App/Base.cs", base);
    f.write(
        "App/Derived.cs",
        "namespace api; public class Derived : Base {}",
    );
    f.write(
        "App/Run.cs",
        "namespace api; class Run { void run(Derived value) { value.work(); } }",
    );
    f.write(
        "App/View.xaml",
        "<Window xmlns:x=\"http://schemas.microsoft.com/winfx/2006/xaml\" x:Class=\"api.View\"/>",
    );
    f.write("Other/Other.csproj", "<Project/>");
    f.write("Other/Base.cs", base);
    let graph = f.index();
    assert!(calls(
        &graph,
        ("App/Run.cs", "run"),
        ("App/Base.cs", "work")
    ));
    assert!(!calls(
        &graph,
        ("App/Run.cs", "run"),
        ("Other/Base.cs", "work")
    ));
    f.write(
        "App/Base.cs",
        "namespace api; public class Base { public void work() { /* explanation */ } }",
    );
    let changed = index::check_update(&f.root(), &f.db()).unwrap().changed;
    assert!(changed.iter().any(|p| p == "App/Base.cs"));
    assert!(changed.iter().any(|p| p == "App/View.xaml"));
    assert!(
        !changed
            .iter()
            .any(|p| p == "App/Run.cs" || p == "Other/Base.cs")
    );
    let graph = f.index();
    assert!(calls(
        &graph,
        ("App/Run.cs", "run"),
        ("App/Base.cs", "work")
    ));
    f.write(
        "App/Base.cs",
        "namespace api; public class Base { private void work() {} }",
    );
    assert!(
        index::check_update(&f.root(), &f.db())
            .unwrap()
            .changed
            .iter()
            .any(|p| p == "App/Run.cs")
    );
    let graph = f.index();
    assert!(!calls(
        &graph,
        ("App/Run.cs", "run"),
        ("App/Base.cs", "work")
    ));
    f.write("App/Base.cs", base);
    f.write("App/Ambiguous.csproj", "<Project/>");
    let graph = f.index();
    assert!(!calls(
        &graph,
        ("App/Run.cs", "run"),
        ("App/Base.cs", "work")
    ));
    fs::remove_file(f.root().join("App/Ambiguous.csproj")).unwrap();
    let graph = f.index();
    assert!(calls(
        &graph,
        ("App/Run.cs", "run"),
        ("App/Base.cs", "work")
    ));
}

#[test]
fn compiled_root_analysis_units_stop_at_indexed_split_markers() {
    for (extension, base, derived, caller, marker, manifest) in [
        (
            "java",
            "package api; public class Base { public final void work() {} }",
            "package api; public class Derived extends Base {}",
            "package api; class Run { void run(Derived value) { value.work(); } }",
            "nested/pom.xml",
            "<project><modelVersion>4.0.0</modelVersion><groupId>sample</groupId><artifactId>nested</artifactId><version>1</version></project>",
        ),
        (
            "kt",
            "package api\nopen class Base {\n fun work() {}\n}\n",
            "package api\nclass Derived : Base()\n",
            "package api\nfun run(value: Derived) { value.work() }\n",
            "nested/build.gradle.kts",
            "",
        ),
        (
            "cpp",
            "namespace api { struct Base { void work() {} }; }",
            "namespace api { struct Derived : Base {}; }",
            "namespace api { void run(Derived value) { value.work(); } }",
            "nested/CMakeLists.txt",
            "project(nested)\n",
        ),
    ] {
        let f = Fixture::new();
        let base_path = format!("Base.{extension}");
        let derived_path = format!("Derived.{extension}");
        let run_path = format!("Run.{extension}");
        f.write(&base_path, base);
        f.write(&derived_path, derived);
        f.write(&run_path, caller);
        // Plain CMake text participates in normal indexing, not code-only mode.
        let options = IndexOptions {
            code_only: extension != "cpp",
            ..Default::default()
        };
        let graph = f.index_with(&options);
        assert!(
            calls(&graph, (&run_path, "run"), (&base_path, "work")),
            "{extension}"
        );
        f.write(marker, manifest);
        assert!(
            index::check_update(&f.root(), &f.db())
                .unwrap()
                .changed
                .contains(&run_path)
        );
        let graph = f.index_with(&options);
        assert!(
            !calls(&graph, (&run_path, "run"), (&base_path, "work")),
            "{extension}"
        );
        fs::remove_file(f.root().join(marker)).unwrap();
        assert!(
            index::check_update(&f.root(), &f.db())
                .unwrap()
                .changed
                .contains(&run_path)
        );
        let graph = f.index_with(&options);
        assert!(
            calls(&graph, (&run_path, "run"), (&base_path, "work")),
            "{extension}"
        );
    }
}

#[test]
fn extended_pascal_context_refreshes_unchanged_callers_and_removes_deleted_targets() {
    let f = Fixture::new();
    let base = "unit Foundation;\ninterface\ntype TBase = class\n procedure Prepare;\nend;\nimplementation\nprocedure TBase.Prepare; begin end;\nend.\n";
    f.write("Foundation.pas", base);
    f.write("Child.pas", "unit Child;\ninterface\nuses Foundation;\ntype TChild = class(TBase)\n procedure Run;\nend;\nimplementation\nprocedure TChild.Run; begin inherited Prepare; end;\nend.\n");
    let graph = f.index();
    assert!(calls(
        &graph,
        ("Child.pas", "tchild.run"),
        ("Foundation.pas", "tbase.prepare")
    ));
    f.write("Foundation.pas", &base.replace("Prepare", "Changed"));
    assert!(
        index::check_update(&f.root(), &f.db())
            .unwrap()
            .changed
            .iter()
            .any(|p| p == "Child.pas")
    );
    let graph = f.index();
    assert!(!file_relation(
        &graph,
        "Child.pas",
        "calls",
        ("Foundation.pas", "tbase.changed")
    ));
    f.write("Foundation.pas", base);
    f.index();
    fs::remove_file(f.root().join("Foundation.pas")).unwrap();
    assert!(
        index::check_update(&f.root(), &f.db())
            .unwrap()
            .changed
            .iter()
            .any(|p| p == "Child.pas")
    );
    let graph = f.index();
    assert!(!graph.nodes.iter().any(|n| n.file == "Foundation.pas"));
    assert!(!graph.edges.iter().any(|e| e.relation == "calls"));
}

#[test]
fn razor_index_uses_loose_or_nearest_project_types_and_refreshes_on_removal() {
    let f = Fixture::new();
    let source = "namespace Demo; public class Service {}";
    f.write("Service.cs", source);
    f.write("Page.razor", "@using Demo\n@inject Service service\n");
    f.write("View.cshtml", "@inject Demo.Service service\n");
    let graph = f.index();
    for page in ["Page.razor", "View.cshtml"] {
        assert!(file_relation(
            &graph,
            page,
            "uses_type",
            ("Service.cs", "Service")
        ));
    }
    f.write("Nested/App.csproj", "<Project/>");
    f.write("Nested/Service.cs", source);
    f.write("Nested/Page.razor", "@inject Demo.Service service\n");
    let graph = f.index();
    assert!(file_relation(
        &graph,
        "Nested/Page.razor",
        "uses_type",
        ("Nested/Service.cs", "Service")
    ));
    assert!(file_relation(
        &graph,
        "Page.razor",
        "uses_type",
        ("Service.cs", "Service")
    ));
    assert!(!file_relation(
        &graph,
        "Page.razor",
        "uses_type",
        ("Nested/Service.cs", "Service")
    ));
    f.write("Nested/Other.csproj", "<Project/>");
    let graph = f.index();
    for provider in ["Service.cs", "Nested/Service.cs"] {
        assert!(!file_relation(
            &graph,
            "Nested/Page.razor",
            "uses_type",
            (provider, "Service")
        ));
    }
    fs::remove_file(f.root().join("Nested/Other.csproj")).unwrap();
    f.write("Nested/App.csproj", "<Project><PropertyGroup><RootNamespace>$(Unknown)</RootNamespace></PropertyGroup></Project>");
    let graph = f.index();
    assert!(!file_relation(
        &graph,
        "Nested/Page.razor",
        "uses_type",
        ("Nested/Service.cs", "Service")
    ));
    fs::remove_file(f.root().join("Service.cs")).unwrap();
    assert!(
        index::check_update(&f.root(), &f.db())
            .unwrap()
            .changed
            .iter()
            .any(|p| p == "Page.razor")
    );
    let graph = f.index();
    for page in ["Page.razor", "View.cshtml"] {
        assert!(!file_relation(
            &graph,
            page,
            "uses_type",
            ("Nested/Service.cs", "Service")
        ));
    }
    assert!(!graph.nodes.iter().any(|n| n.file == "Service.cs"));
}

fn swift_manifest(targets: &str) -> String {
    format!(
        "// swift-tools-version: 6.0\nimport PackageDescription\nlet package = Package(name: \"Example\", targets: [{targets}])\n"
    )
}

#[test]
fn swiftpm_default_sources_tests_and_declared_imports_need_no_configuration() {
    let f = Fixture::new();
    f.write(
        "Package.swift",
        &swift_manifest(
            r#"
        .target(name: "Core"),
        .executableTarget(name: "App", dependencies: ["Core"]),
        .testTarget(name: "CoreTests", dependencies: [.target(name: "Core")]),
        .target(name: "Unrelated")
    "#,
        ),
    );
    f.write(
        "Sources/Core/a.swift",
        "public func work() {}\nfunc internalWork() {}\n",
    );
    f.write(
        "Sources/Core/b.swift",
        "func run() { work(); internalWork() }\n",
    );
    f.write(
        "Sources/App/main.swift",
        "import Core\nfunc main() { Core.work(); Core.internalWork() }\n",
    );
    f.write(
        "Tests/CoreTests/check.swift",
        "import Core\nfunc check() { Core.work() }\n",
    );
    f.write(
        "Sources/Unrelated/other.swift",
        "import Core\nfunc other() { Core.work() }\n",
    );
    f.write(
        "Loose/tool.swift",
        "import Core\nfunc loose() { Core.work() }\n",
    );
    let graph = f.index();
    assert!(calls(
        &graph,
        ("Sources/Core/b.swift", "run"),
        ("Sources/Core/a.swift", "internalWork")
    ));
    for caller in [
        ("Sources/Core/b.swift", "run"),
        ("Sources/App/main.swift", "main"),
        ("Tests/CoreTests/check.swift", "check"),
    ] {
        assert!(calls(&graph, caller, ("Sources/Core/a.swift", "work")));
    }
    for caller in [
        ("Sources/Unrelated/other.swift", "other"),
        ("Loose/tool.swift", "loose"),
    ] {
        assert!(f.unresolved(node(&graph, caller.0, caller.1), "Core.work"));
    }
    assert!(f.unresolved(
        node(&graph, "Sources/App/main.swift", "main"),
        "Core.internalWork"
    ));
    assert!(
        index::stored_options(&f.db())
            .unwrap()
            .swift_modules
            .is_empty()
    );
    assert!(index::check_update(&f.root(), &f.db()).unwrap().fresh);
}

#[test]
fn swiftpm_literal_paths_sources_excludes_and_products_are_not_name_guesses() {
    let f = Fixture::new();
    f.write("Package.swift", &swift_manifest(r#"
        .target(name: "Core", path: "lib", exclude: ["keep/ignored.swift"], sources: ["keep", "one.swift"]),
        .target(name: "App", dependencies: [.byName(name: "Core"), .product(name: "Remote", package: "remote")], path: "app"),
        .target(name: "Remote", path: "remote")
    "#));
    f.write("lib/keep/a.swift", "public func work() {}\n");
    f.write("lib/keep/ignored.swift", "public func work() {}\n");
    f.write("lib/elsewhere.swift", "public func work() {}\n");
    f.write("lib/one.swift", "func run() { work() }\n");
    f.write("remote/remote.swift", "public func remoteWork() {}\n");
    f.write(
        "app/main.swift",
        "import Core\nimport Remote\nfunc main() { Core.work(); Remote.remoteWork() }\n",
    );
    let graph = f.index();
    for caller in [("lib/one.swift", "run"), ("app/main.swift", "main")] {
        assert!(calls(&graph, caller, ("lib/keep/a.swift", "work")));
        assert!(!calls(&graph, caller, ("lib/keep/ignored.swift", "work")));
        assert!(!calls(&graph, caller, ("lib/elsewhere.swift", "work")));
    }
    assert!(f.unresolved(node(&graph, "app/main.swift", "main"), "Remote.remoteWork"));
}

#[test]
fn swiftpm_computed_conditional_duplicate_and_overlapping_declarations_refuse_membership() {
    let f = Fixture::new();
    f.write("Sources/Core/a.swift", "public func work() {}\n");
    f.write("Sources/Core/b.swift", "func run() { work() }\n");
    for targets in [
        r#".target(name: targetName)"#,
        r#".target(name: "Core", path: sourcePath())"#,
        r#".target(name: "Core", sources: files)"#,
        r#".target(name: "Core", dependencies: dependencies())"#,
        r#".target(name: "Core", dependencies: [.target(name: "Other", condition: .when(platforms: [.macOS]))])"#,
        r#".target(name: "Core", swiftSettings: [.define("FEATURE")])"#,
        r#".target(name: "Core", path: "../outside")"#,
        r#".target(name: "Core"), .target(name: "Core", path: "Elsewhere")"#,
        r#".target(name: "Core"), .target(name: "Other", path: "Sources/Core")"#,
        r#".target(name: "Core"), .target(name: "Other", path: "Sources/Core/nested")"#,
        r#".target(name: "Core", name: "Other")"#,
        r#".target(name: "Core"), makeTarget()"#,
    ] {
        f.write("Package.swift", &swift_manifest(targets));
        let graph = f.index();
        assert!(
            f.unresolved(node(&graph, "Sources/Core/b.swift", "run"), "work"),
            "{targets}"
        );
    }
    let literal = swift_manifest(r#".target(name: "Core")"#);
    for manifest in [
        format!("#if os(macOS)\n{literal}#endif\n"),
        format!("{literal}\npackage.targets.append(.target(name: \"Other\"))\n"),
        literal.replace("let package", "var package"),
        literal.replace("name: \"Core\"", "name: #\"Core\"#"),
        literal.replace("name: \"Core\"", "name: \"\\(targetName)\""),
    ] {
        f.write("Package.swift", &manifest);
        let graph = f.index();
        assert!(
            f.unresolved(node(&graph, "Sources/Core/b.swift", "run"), "work"),
            "{manifest}"
        );
    }
    f.write("Package.swift", &literal);
    let graph = f.index();
    assert!(calls(
        &graph,
        ("Sources/Core/b.swift", "run"),
        ("Sources/Core/a.swift", "work")
    ));
    f.write("Package@swift-6.0.swift", &literal);
    let graph = f.index();
    assert!(f.unresolved(node(&graph, "Sources/Core/b.swift", "run"), "work"));
}

#[test]
fn swiftpm_nested_invalid_packages_and_duplicate_module_names_do_not_leak() {
    let f = Fixture::new();
    let package =
        swift_manifest(r#".target(name: "Core"), .target(name: "App", dependencies: ["Core"])"#);
    for root in ["one", "two"] {
        f.write(&format!("{root}/Package.swift"), &package);
        f.write(
            &format!("{root}/Sources/Core/a.swift"),
            "public func work() {}\n",
        );
        f.write(
            &format!("{root}/Sources/App/main.swift"),
            "import Core\nfunc main() { Core.work() }\n",
        );
    }
    f.write(
        "one/Sources/Core/nested/Package.swift",
        "import PackageDescription\nlet package = makePackage()\n",
    );
    f.write("one/Sources/Core/nested/a.swift", "public func work() {}\n");
    f.write("one/Sources/Core/nested/b.swift", "func run() { work() }\n");
    let graph = f.index();
    for root in ["one", "two"] {
        let caller = format!("{root}/Sources/App/main.swift");
        let target = format!("{root}/Sources/Core/a.swift");
        assert!(calls(&graph, (&caller, "main"), (&target, "work")));
    }
    assert!(!calls(
        &graph,
        ("one/Sources/App/main.swift", "main"),
        ("two/Sources/Core/a.swift", "work")
    ));
    assert!(f.unresolved(
        node(&graph, "one/Sources/Core/nested/b.swift", "run"),
        "work"
    ));
    f.write(
        "one/Sources/Core/nested/Package.swift",
        &swift_manifest(r#".target(name: "Core", path: ".")"#),
    );
    let graph = f.index();
    assert!(calls(
        &graph,
        ("one/Sources/Core/nested/b.swift", "run"),
        ("one/Sources/Core/nested/a.swift", "work")
    ));
    assert!(!calls(
        &graph,
        ("one/Sources/App/main.swift", "main"),
        ("one/Sources/Core/nested/a.swift", "work")
    ));
}

#[test]
fn swiftpm_manifest_changes_removals_and_explicit_override_invalidate_navigation() {
    let f = Fixture::new();
    let literal =
        swift_manifest(r#".target(name: "Core"), .target(name: "App", dependencies: ["Core"])"#);
    f.write("Package.swift", &literal);
    f.write("Sources/Core/a.swift", "public func work() {}\n");
    f.write("Sources/Core/b.swift", "func run() { work() }\n");
    f.write(
        "Sources/App/main.swift",
        "import Core\nfunc main() { Core.work() }\n",
    );
    f.index();
    f.write(
        "Package.swift",
        &literal.replace("dependencies: [\"Core\"]", "dependencies: []"),
    );
    assert!(
        index::check_update(&f.root(), &f.db())
            .unwrap()
            .changed
            .contains(&"Sources/App/main.swift".into())
    );
    let graph = f.index();
    assert!(f.unresolved(node(&graph, "Sources/App/main.swift", "main"), "Core.work"));
    f.write("Package.swift", &literal);
    f.index();
    fs::remove_file(f.root().join("Sources/Core/a.swift")).unwrap();
    let graph = f.index();
    assert!(f.unresolved(node(&graph, "Sources/App/main.swift", "main"), "Core.work"));
    f.write("Sources/Core/a.swift", "public func work() {}\n");
    f.index();
    fs::remove_file(f.root().join("Package.swift")).unwrap();
    assert!(
        index::check_update(&f.root(), &f.db())
            .unwrap()
            .changed
            .contains(&"Sources/Core/b.swift".into())
    );
    let graph = f.index();
    assert!(f.unresolved(node(&graph, "Sources/Core/b.swift", "run"), "work"));

    f.write("Package.swift", &literal);
    f.write("Elsewhere/a.swift", "public func work() {}\n");
    let options = IndexOptions {
        code_only: true,
        swift_modules: BTreeMap::from([
            ("Core".into(), "Elsewhere".into()),
            ("App".into(), "Sources/App".into()),
        ]),
        ..Default::default()
    };
    let graph = f.index_with(&options);
    assert!(calls(
        &graph,
        ("Sources/App/main.swift", "main"),
        ("Elsewhere/a.swift", "work")
    ));
    assert!(f.unresolved(node(&graph, "Sources/Core/b.swift", "run"), "work"));
}

#[test]
fn swiftpm_ignored_manifests_and_sources_are_not_discovered_indirectly() {
    let f = Fixture::new();
    f.write("Package.swift", &swift_manifest(r#".target(name: "Core")"#));
    f.write("Sources/Core/a.swift", "public func work() {}\n");
    f.write("Sources/Core/b.swift", "func run() { work() }\n");
    f.write(".grafignore", "Package.swift\n");
    let graph = f.index();
    assert!(f.unresolved(node(&graph, "Sources/Core/b.swift", "run"), "work"));
    f.write(".grafignore", "Sources/Core/a.swift\n");
    let graph = f.index();
    assert!(f.unresolved(node(&graph, "Sources/Core/b.swift", "run"), "work"));
    assert!(!graph.nodes.iter().any(|n| n.file == "Sources/Core/a.swift"));
}

#[cfg(unix)]
#[test]
fn swiftpm_symlinked_manifests_and_sources_are_not_discovered_indirectly() {
    use std::os::unix::fs::symlink;
    let f = Fixture::new();
    let outside = tempdir().unwrap();
    fs::write(
        outside.path().join("Package.swift"),
        swift_manifest(r#".target(name: "Core")"#),
    )
    .unwrap();
    fs::write(outside.path().join("a.swift"), "public func work() {}\n").unwrap();
    f.write("Sources/Core/b.swift", "func run() { work() }\n");
    f.write("Sources/Core/a.swift", "public func work() {}\n");
    symlink(
        outside.path().join("Package.swift"),
        f.root().join("Package.swift"),
    )
    .unwrap();
    let graph = f.index();
    assert!(f.unresolved(node(&graph, "Sources/Core/b.swift", "run"), "work"));
    fs::remove_file(f.root().join("Package.swift")).unwrap();
    f.write("Package.swift", &swift_manifest(r#".target(name: "Core")"#));
    fs::remove_file(f.root().join("Sources/Core/a.swift")).unwrap();
    symlink(outside.path(), f.root().join("Sources/Core/linked")).unwrap();
    let graph = f.index();
    assert!(f.unresolved(node(&graph, "Sources/Core/b.swift", "run"), "work"));
    assert!(!graph.nodes.iter().any(|n| n.file.contains("linked")));
}

#[test]
fn go_interface_owner_ambiguity_retracts_and_restores_unchanged_callers() {
    // Distinct members cannot disambiguate the shared owning type. Include
    // aliases and build alternatives: this index does not select a Go build.
    for (duplicate_path, duplicate) in [
        (
            "b.go",
            "package wire\ntype Channel interface { Receive() }\n",
        ),
        ("b.go", "package wire\ntype Channel struct {}\n"),
        ("b.go", "package wire\ntype (Channel = int)\n"),
        (
            "b_test.go",
            "package wire\ntype Channel interface { Receive() }\n",
        ),
        (
            "b_linux.go",
            "//go:build linux\n\npackage wire\ntype Channel interface { Receive() }\n",
        ),
    ] {
        let f = Fixture::new();
        f.write("go.mod", "module example.org/navigation\n");
        f.write("a.go", "package wire\ntype Channel interface { Send() }\n");
        f.write(
            "caller.go",
            "package wire\nfunc invoke(value Channel) { value.Send() }\n",
        );
        f.write("consumer/main.go", "package consumer\nimport \"example.org/navigation\"\nfunc invoke(value wire.Channel) { value.Send() }\n");
        f.write(
            "implementation.go",
            "package wire\ntype Concrete struct {}\nfunc (Concrete) Send() {}\n",
        );
        let check = |graph: &GraphSnapshot, expected: bool| {
            let contract = node(graph, "a.go", "Channel");
            let member = node(graph, "a.go", "Send");
            assert!(member.binding_key.is_none());
            assert!(graph.edges.iter().any(|e| e.source == contract.id
                && e.target == member.id
                && e.relation == "contains"));
            for file in ["caller.go", "consumer/main.go"] {
                let caller = node(graph, file, "invoke");
                let edges: Vec<_> = graph
                    .edges
                    .iter()
                    .filter(|e| {
                        e.source == caller.id
                            && matches!(e.relation.as_str(), "calls" | "declared_member")
                    })
                    .collect();
                assert_eq!(
                    edges.len(),
                    usize::from(expected),
                    "{duplicate_path}: {duplicate}: {file}: {edges:?}"
                );
                if expected {
                    assert_eq!(edges[0].target, member.id);
                    assert_eq!(edges[0].relation, "declared_member");
                }
            }
        };
        check(&f.index(), true);
        f.write(duplicate_path, duplicate);
        let changed = index::check_update(&f.root(), &f.db()).unwrap().changed;
        for path in ["a.go", "caller.go", "consumer/main.go"] {
            assert!(
                changed.iter().any(|p| p == path),
                "{duplicate_path}: {changed:?}"
            );
        }
        check(&f.index(), false);
        fs::remove_file(f.root().join(duplicate_path)).unwrap();
        let changed = index::check_update(&f.root(), &f.db()).unwrap().changed;
        for path in ["a.go", "caller.go", "consumer/main.go"] {
            assert!(
                changed.iter().any(|p| p == path),
                "{duplicate_path}: {changed:?}"
            );
        }
        check(&f.index(), true);
    }
}

#[test]
fn go_interface_owner_counts_respect_lexical_package_and_directory_boundaries() {
    let f = Fixture::new();
    f.write("go.mod", "module example.org/navigation\n");
    f.write("a.go", "package wire\ntype Channel interface { Send() }\n");
    f.write(
        "caller.go",
        "package wire\nfunc invoke(value Channel) { value.Send() }\n",
    );
    f.write(
        "other/decoy.go",
        "package wire\ntype Channel interface { Receive() }\n",
    );
    f.write(
        "external_test.go",
        "package wire_test\ntype Channel interface { Receive() }\n",
    );
    let local = "package wire\nfunc local() { type Channel interface { Receive() }; var value Channel; value.Receive() }\n";
    f.write("local.go", local);
    let graph = f.index();
    for (file, caller, member_file, member) in [
        ("caller.go", "invoke", "a.go", "Send"),
        ("local.go", "local", "local.go", "Receive"),
    ] {
        let caller = node(&graph, file, caller);
        let member = node(&graph, member_file, member);
        let edges: Vec<_> = graph
            .edges
            .iter()
            .filter(|e| {
                e.source == caller.id && matches!(e.relation.as_str(), "calls" | "declared_member")
            })
            .collect();
        assert_eq!(edges.len(), 1, "{file}: {edges:?}");
        assert_eq!(edges[0].relation, "declared_member");
        assert_eq!(edges[0].target, member.id);
    }
    // Formatting/body edits do not change owner proof or refresh other files.
    f.write("local.go", &format!("{local}// a comment\n"));
    let changed = index::check_update(&f.root(), &f.db()).unwrap().changed;
    assert!(changed.iter().any(|p| p == "local.go"));
    assert!(
        !changed.iter().any(|p| p == "a.go" || p == "caller.go"),
        "{changed:?}"
    );
}

#[test]
fn rust_alpha_renamed_generic_impls_follow_exact_declared_cargo_modules() {
    let f = Fixture::new();
    f.write(
        "Cargo.toml",
        "[package]\nname = \"register-demo\"\nversion = \"0.1.0\"\n",
    );
    let root = "pub mod model; mod provider; mod consumer; mod unrelated;";
    let model = "pub struct Register<A, B>(pub A, pub B);";
    f.write("src/lib.rs", root);
    f.write("src/model.rs", model);
    f.write("src/provider.rs", "use crate::model::Register; impl<Key, Value> Register<Key, Value> { pub fn inspect(&self) {} pub fn empty() -> Option<Self> { None } }");
    f.write("src/consumer.rs", "use crate::model::Register as Table; impl<Left, Right> Table<Left, Right> { pub fn by_value(&self) { self.inspect(); } pub fn by_type() { Self::empty(); } pub fn missing(&self) { self.orphan(); } }");
    f.write("src/unrelated.rs", "pub struct Register<A, B>(A, B); impl<X, Y> Register<X, Y> { pub fn inspect(&self) {} pub fn empty() {} }");
    f.write("src/orphan.rs", "use crate::model::Register; impl<A, B> Register<A, B> { pub fn orphan(&self) {} pub fn inspect(&self) {} }");
    let check = |graph: &GraphSnapshot, present: bool| {
        for (caller, target) in [("by_value", "inspect"), ("by_type", "empty")] {
            let source = node(graph, "src/consumer.rs", caller);
            let edges: Vec<_> = graph
                .edges
                .iter()
                .filter(|e| {
                    e.source == source.id
                        && matches!(e.relation.as_str(), "calls" | "declared_member")
                })
                .collect();
            assert_eq!(edges.len(), usize::from(present), "{caller}: {edges:?}");
            if present {
                assert_eq!(edges[0].relation, "calls");
                assert_eq!(edges[0].target, node(graph, "src/provider.rs", target).id);
            }
        }
        let missing = node(graph, "src/consumer.rs", "missing");
        assert!(
            !graph
                .edges
                .iter()
                .any(|e| e.source == missing.id && e.relation == "calls")
        );
    };
    check(&f.index(), true);
    // Removing module membership retracts links without replacing the caller.
    f.write("src/lib.rs", "pub mod model; mod consumer; mod unrelated;");
    let changed = index::check_update(&f.root(), &f.db()).unwrap().changed;
    assert!(changed.iter().any(|p| p == "src/provider.rs"));
    assert!(!changed.iter().any(|p| p == "src/consumer.rs"));
    check(&f.index(), false);
    f.write("src/lib.rs", root);
    check(&f.index(), true);
    // Impls alone cannot prove a generic family, even with explicit imports.
    f.write("src/model.rs", "pub struct Other;");
    assert!(
        index::check_update(&f.root(), &f.db())
            .unwrap()
            .changed
            .iter()
            .any(|p| p == "src/consumer.rs")
    );
    check(&f.index(), false);
    f.write("src/model.rs", model);
    check(&f.index(), true);
}

#[test]
fn rust_generic_impl_family_rejects_unproven_headers_and_owner_declarations() {
    for (declaration, provider, consumer) in [
        (
            "pub struct Register<T>(pub T);",
            "impl<T: Copy> Register<T>",
            "impl<U> Register<U>",
        ),
        (
            "pub struct Register<T>(pub T);",
            "impl<T> Register<T>",
            "impl<U: Copy> Register<U>",
        ),
        (
            "pub struct Register<T>(pub T);",
            "impl<T> Register<T> where T: Copy",
            "impl<U> Register<U> where U: Copy",
        ),
        (
            "pub struct Register<T>(pub T);",
            "impl<T> Inspect for Register<T>",
            "impl<U> Register<U>",
        ),
        (
            "pub struct Register<'a, T>(pub &'a T);",
            "impl<'a, T> Register<'a, T>",
            "impl<'b, U> Register<'b, U>",
        ),
        (
            "pub struct Register<const N: usize>(pub [u8; N]);",
            "impl<const N: usize> Register<N>",
            "impl<const M: usize> Register<M>",
        ),
        (
            "pub struct Register<T>(pub T);",
            "impl<T> Register<Option<T>>",
            "impl<U> Register<U>",
        ),
        (
            "pub struct Register<T>(pub T);",
            "impl Register<u16>",
            "impl<U> Register<U>",
        ),
        (
            "pub struct Register<A, B>(pub A, pub B);",
            "impl<T> Register<T, T>",
            "impl<X, Y> Register<X, Y>",
        ),
        (
            "pub struct Register<A, B>(pub A, pub B);",
            "impl<A, B> Register<B, A>",
            "impl<X, Y> Register<X, Y>",
        ),
        ("", "impl<T> Register<T>", "impl<U> Register<U>"),
        (
            "pub struct Register<T>(pub T); pub struct Register<U>(pub U);",
            "impl<T> Register<T>",
            "impl<U> Register<U>",
        ),
        (
            "pub struct Register<A, B>(pub A, pub B);",
            "impl<T> Register<T>",
            "impl<U> Register<U>",
        ),
        (
            "pub type Register<T> = Option<T>;",
            "impl<T> Register<T>",
            "impl<U> Register<U>",
        ),
    ] {
        let f = Fixture::new();
        f.write(
            "Cargo.toml",
            "[package]\nname = \"register-demo\"\nversion = \"0.1.0\"\n",
        );
        f.write("src/lib.rs", "mod model; mod provider; mod consumer;");
        f.write("src/model.rs", declaration);
        f.write("src/provider.rs", &format!("use crate::model::Register; trait Inspect {{ fn inspect(&self); }} {provider} {{ pub fn inspect(&self) {{}} }}"));
        f.write("src/consumer.rs", &format!("use crate::model::Register; fn helper() {{}} {consumer} {{ pub fn visit(&self) {{ self.inspect(); Self::inspect(self); helper(); }} }}"));
        let graph = f.index();
        let caller = node(&graph, "src/consumer.rs", "visit");
        let helper = node(&graph, "src/consumer.rs", "helper");
        let edges: Vec<_> = graph
            .edges
            .iter()
            .filter(|e| {
                e.source == caller.id && matches!(e.relation.as_str(), "calls" | "declared_member")
            })
            .collect();
        assert_eq!(
            edges.len(),
            1,
            "{declaration} / {provider} / {consumer}: {edges:?}"
        );
        assert_eq!(edges[0].target, helper.id);
        assert_eq!(edges[0].relation, "calls");
    }
}

#[test]
fn rust_generic_impls_do_not_borrow_an_unrelated_owner_or_a_mixed_block() {
    for (provider, consumer) in [
        (
            "use crate::model::Register; impl<T> Register<T> { pub fn inspect(&self) {} }",
            "use crate::other::Register; impl<U> Register<U> { pub fn visit(&self) { self.inspect(); } }",
        ),
        (
            "use crate::model::Register; impl<T> Register<T> { fn marker(&self) {} } impl<T: Copy> Register<T> { pub fn inspect(&self) {} }",
            "use crate::model::Register; impl<U> Register<U> { fn marker2(&self) {} } impl<U: Copy> Register<U> { pub fn visit(&self) { self.inspect(); } }",
        ),
        (
            "use crate::model::Register; impl<T: Copy> Register<T> { pub fn inspect(&self) {} } impl<T> Register<T> { fn marker(&self) {} }",
            "use crate::model::Register; impl<U: Copy> Register<U> { pub fn visit(&self) { self.inspect(); } } impl<U> Register<U> { fn marker2(&self) {} }",
        ),
    ] {
        let f = Fixture::new();
        f.write(
            "Cargo.toml",
            "[package]\nname = \"register-demo\"\nversion = \"0.1.0\"\n",
        );
        f.write(
            "src/lib.rs",
            "mod model; mod other; mod provider; mod consumer;",
        );
        f.write("src/model.rs", "pub struct Register<T>(pub T);");
        f.write("src/other.rs", "pub struct Register<T>(pub T);");
        f.write("src/provider.rs", provider);
        f.write("src/consumer.rs", consumer);
        let graph = f.index();
        let caller = node(&graph, "src/consumer.rs", "visit");
        assert!(
            !graph.edges.iter().any(|e| e.source == caller.id
                && matches!(e.relation.as_str(), "calls" | "declared_member")),
            "{provider} / {consumer}"
        );
    }
}
