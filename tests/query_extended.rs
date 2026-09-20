use graf::{
    model::*,
    query::{ImpactOptions, SearchOptions, Traversal},
    store::Store,
};
use serde_json::{Value, json};
use tempfile::TempDir;

fn node(id: &str, label: &str, file: &str) -> Node {
    Node {
        id: id.into(),
        label: label.into(),
        kind: "function".into(),
        file: file.into(),
        line: Some(1),
        end_line: Some(2),
        qualified_name: None,
        binding_key: None,
        metadata: json!({}),
    }
}
fn edge(id: &str, source: &str, target: &str, relation: &str) -> Edge {
    Edge {
        id: id.into(),
        source: source.into(),
        target: target.into(),
        relation: relation.into(),
        directed: true,
        file: None,
        line: None,
        confidence: "EXTRACTED".into(),
        metadata: json!({}),
    }
}
fn store(nodes: Vec<Node>, edges: Vec<Edge>) -> (TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::create(&dir.path().join("graph.db")).unwrap();
    store
        .import_graph(ImportedGraph {
            nodes,
            edges,
            metadata: Value::Null,
        })
        .unwrap();
    (dir, store)
}
fn tree() -> (TempDir, Store) {
    store(
        ["a", "b", "c", "d", "e"]
            .map(|s| node(s, s, "tree.rs"))
            .to_vec(),
        vec![
            edge("1", "a", "b", "calls"),
            edge("2", "a", "c", "calls"),
            edge("3", "b", "d", "calls"),
            edge("4", "c", "e", "calls"),
        ],
    )
}
fn ids(result: &GraphResult) -> Vec<&str> {
    result.nodes.iter().map(|n| n.id.as_str()).collect()
}

#[test]
fn endpoint_convenience_preserves_exact_priority_literal_punctuation_and_scope() {
    let (_dir, store) = store(
        vec![
            node("a", "LoaderService", "a.rs"),
            node("b", "OtherLoaderService", "b.rs"),
            node("c", "SharedOne", "a.rs"),
            node("d", "SharedTwo", "b.rs"),
            node("e", "Execute()", "a.rs"),
            node("f", "ExecuteSlow()", "a.rs"),
            node("literal", "Markup_%Code", "c.rs"),
            node("load", "Unrelated", "c.rs"),
            node("load-long", "Load", "a.rs"),
        ],
        vec![edge("call", "a", "c", "calls")],
    );
    let options = SearchOptions::default();
    for (text, expected) in [
        ("load", "load"),
        ("Load", "load-long"),
        ("LOADER", "a"),
        ("Execute", "e"),
        ("Markup_%", "literal"),
        ("%", "literal"),
        ("./a.rs::Shared", "c"),
    ] {
        assert_eq!(
            store.resolve_endpoint(text, &options).unwrap().id,
            expected,
            "{text}"
        );
    }
    for text in ["Shared", "Service", "missing.rs::Shared"] {
        assert!(store.resolve_endpoint(text, &options).is_err(), "{text}");
    }
    let filtered = SearchOptions {
        files: vec!["b.rs".into()],
        ..options.clone()
    };
    assert_eq!(store.resolve_endpoint("Shared", &filtered).unwrap().id, "d");
    // An excluded exact ID cannot silently become a different label match.
    assert!(
        store
            .resolve_endpoint(
                "load",
                &SearchOptions {
                    files: vec!["a.rs".into()],
                    ..options.clone()
                }
            )
            .is_err()
    );
    assert!(store.neighbors_extended("Loader", &options).is_err());
    let path = store
        .path_extended("Loader", "a.rs::Shared", &options)
        .unwrap();
    assert!(path.found);
    assert_eq!(ids(&path.result.graph), ["a", "c"]);
    assert!(store.path_extended("Loader", "Shared", &options).is_err());
}

#[test]
fn resolved_neighbors_use_compatibility_tiers_and_literal_relation_shorthand() {
    let nodes = vec![
        node("root", "ﬂow()", "a.rs"),
        node("later", "FlowHistory", "a.rs"),
        node("other", "Receiver", "b.rs"),
    ];
    let mut edges = vec![
        edge("1", "root", "later", "calls"),
        edge("2", "root", "other", "calls_async"),
        edge("3", "other", "root", "references"),
        edge("4", "root", "other", "route_%"),
    ];
    for _ in 0..2 {
        let (_dir, graph) = store(nodes.clone(), edges.clone());
        for (relation, expected) in [
            ("calls", "1"),
            ("ＣＡＬＬＳ", "1"),
            ("async", "2"),
            ("erenc", "3"),
            ("_%", "4"),
        ] {
            let result = graph
                .neighbors_resolved(
                    "FLOW",
                    &SearchOptions {
                        graph: QueryOptions {
                            depth: 1,
                            relation: Some(relation.into()),
                            ..QueryOptions::default()
                        },
                        ..SearchOptions::default()
                    },
                )
                .unwrap();
            assert_eq!(result.seeds, ["root"]);
            assert_eq!(result.graph.edges.len(), 1, "{relation}");
            assert_eq!(result.graph.edges[0].id, expected, "{relation}");
        }
        let mut options = SearchOptions {
            graph: QueryOptions {
                depth: 1,
                relation: Some("call".into()),
                ..QueryOptions::default()
            },
            ..SearchOptions::default()
        };
        let error = graph
            .neighbors_resolved("flow", &options)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("ambiguous relation") && error.ends_with("calls, calls_async"),
            "{error}"
        );
        options.graph.relation = Some("not-present".into());
        assert!(
            graph
                .neighbors_resolved("flow", &options)
                .unwrap()
                .graph
                .edges
                .is_empty()
        );
        options.graph.relation = Some(" ".into());
        assert_eq!(
            graph
                .neighbors_resolved("flow", &options)
                .unwrap()
                .graph
                .edges
                .len(),
            4
        );
        options.graph.relation = Some(" ".repeat(1025));
        assert!(graph.neighbors_resolved("flow", &options).is_err());
        // Strict APIs still reject convenience spelling.
        assert!(
            graph
                .neighbors_extended("flow", &SearchOptions::default())
                .is_err()
        );
        edges.reverse();
    }
}

#[test]
fn compatibility_endpoints_keep_exact_spelling_and_report_folded_ties() {
    let (_dir, graph) = store(
        vec![
            node("ligature", "ﬂow", "a.rs"),
            node("plain", "flow", "a.rs"),
            node("width", "Ｗｉｄｅ", "b.rs"),
        ],
        vec![],
    );
    for (text, expected) in [("ﬂow", "ligature"), ("flow", "plain"), ("wide", "width")] {
        assert_eq!(
            graph
                .resolve_endpoint(text, &SearchOptions::default())
                .unwrap()
                .id,
            expected
        );
    }
    let error = graph
        .resolve_endpoint("FLOW", &SearchOptions::default())
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("ambiguous") && error.contains("ligature, plain"),
        "{error}"
    );
}

#[test]
fn relation_shorthand_seeks_distinct_names_at_large_hubs_and_includes_unresolved() {
    let edges = (0..5_100)
        .map(|i| edge(&format!("e{i:04}"), "root", "other", "calls"))
        .collect();
    let (_dir, graph) = store(
        vec![node("root", "Root", "a.rs"), node("other", "Other", "a.rs")],
        edges,
    );
    let options = SearchOptions {
        graph: QueryOptions {
            depth: 0,
            relation: Some("call".into()),
            ..QueryOptions::default()
        },
        ..SearchOptions::default()
    };
    // Resolution must seek names, not exhaust its budget on identical edges.
    assert_eq!(
        graph.neighbors_resolved("root", &options).unwrap().seeds,
        ["root"]
    );
    let dir = tempfile::tempdir().unwrap();
    let mut native = Store::create(&dir.path().join("native.db")).unwrap();
    native
        .apply_native(
            "repo",
            vec![FileFacts {
                path: "a.rs".into(),
                hash: "same".into(),
                module: "a".into(),
                nodes: vec![node("root", "Root", "a.rs")],
                edges: vec![],
                references: vec![Reference {
                    id: "pending".into(),
                    source: "root".into(),
                    label: "Missing".into(),
                    relation: "calls".into(),
                    file: "a.rs".into(),
                    line: 2,
                    candidate_keys: vec![],
                    reason: "no target".into(),
                }],
                diagnostics: vec![],
            }],
            vec![],
            Coverage::default(),
        )
        .unwrap();
    let result = native.neighbors_resolved("root", &options).unwrap();
    assert_eq!(result.graph.unresolved.len(), 1);
    assert_eq!(result.graph.unresolved[0].relation, "calls");
}

#[test]
fn normalized_endpoints_accept_composed_decomposed_and_accentless_spelling() {
    for label in ["Révision", "Re\u{301}vision"] {
        let mut qualified = node("qualified", "Separate", "review.rs");
        qualified.qualified_name = Some("Módulo.revisar".into());
        let (_dir, store) = store(
            vec![
                node("review", label, "review.rs"),
                node("longer", "RevisionHistory", "review.rs"),
                node("longer2", "RevisionLog", "review.rs"),
                node("caller", "Caller", "caller.rs"),
                qualified,
                node("Ópaque", "Another", "review.rs"),
            ],
            vec![edge("call", "caller", "review", "calls")],
        );
        let options = SearchOptions::default();
        for text in ["Révision", "Re\u{301}vision", "Revision", "RÉVISION"] {
            assert_eq!(store.resolve_endpoint(text, &options).unwrap().id, "review");
            let path = store.path_extended("caller", text, &options).unwrap();
            assert!(path.found);
            assert_eq!(ids(&path.result.graph), ["caller", "review"]);
            let impact = store
                .impact_extended(text, &ImpactOptions::default())
                .unwrap();
            assert_eq!(impact.seeds, ["review"]);
            assert_eq!(ids(&impact.graph), ["review", "caller"]);
        }
        assert_eq!(
            store
                .resolve_endpoint("Modulo.revisar", &options)
                .unwrap()
                .id,
            "qualified"
        );
        assert_eq!(
            store.resolve_endpoint("opaque", &options).unwrap().id,
            "Ópaque"
        );
        // Exact neighbor APIs retain their previous contract.
        assert!(store.neighbors_extended("Revision", &options).is_err());
    }
}

#[test]
fn normalized_endpoints_preserve_distinct_exact_ids_labels_and_literal_scope_syntax() {
    let (_dir, store) = store(
        vec![
            node("Café", "First", "a.rs"),
            node("Cafe\u{301}", "Second", "b.rs"),
            node("decoy", "CAFE", "b.rs"),
            node("plain", "Resume", "a.rs"),
            node("accent", "Résumé", "b.rs"),
            node("literal", "a.rs::Résumé", "b.rs"),
            node("scoped", "Résumé", "a.rs"),
        ],
        vec![],
    );
    let options = SearchOptions::default();
    for (text, expected) in [
        ("Café", "Café"),
        ("Cafe\u{301}", "Cafe\u{301}"),
        ("Resume", "plain"),
        ("a.rs::Résumé", "literal"),
    ] {
        assert_eq!(store.resolve_endpoint(text, &options).unwrap().id, expected);
    }
    let only_b = SearchOptions {
        files: vec!["b.rs".into()],
        ..options.clone()
    };
    assert_eq!(
        store.resolve_endpoint("Résumé", &only_b).unwrap().id,
        "accent"
    );
    // An excluded exact ID must not be reinterpreted as a folded ID or label.
    assert!(
        store
            .resolve_endpoint("Café", &only_b)
            .unwrap_err()
            .to_string()
            .contains("no symbol")
    );
    assert!(
        store
            .resolve_endpoint("cafe", &options)
            .unwrap_err()
            .to_string()
            .contains("ambiguous")
    );
    assert!(
        store
            .resolve_endpoint("RESUME", &options)
            .unwrap_err()
            .to_string()
            .contains("ambiguous")
    );
}

#[test]
fn normalized_endpoint_ties_never_choose_by_order_limit_or_spelling() {
    let nodes = vec![
        node("a", "Élan()", "one.rs"),
        node("b", "E\u{301}lan()", "one.rs"),
        node("c", "ElanExtra", "two.rs"),
    ];
    let mut errors = Vec::new();
    for nodes in [nodes.clone(), nodes.into_iter().rev().collect()] {
        let (_dir, store) = store(nodes, vec![]);
        for limit in [1, 100] {
            let options = SearchOptions {
                graph: QueryOptions {
                    limit,
                    ..QueryOptions::default()
                },
                ..SearchOptions::default()
            };
            for text in ["Elan", "ELAN()", "one.rs::Elan", "Ela", "lan"] {
                let error = store
                    .resolve_endpoint(text, &options)
                    .unwrap_err()
                    .to_string();
                assert!(error.contains("ambiguous"), "{text}: {error}");
                assert!(error.contains("a, b"), "{text}: {error}");
                if text == "Elan" {
                    errors.push(error);
                }
            }
        }
        // Exact label bytes keep priority despite the other canonical spelling.
        assert_eq!(
            store
                .resolve_endpoint("Élan()", &SearchOptions::default())
                .unwrap()
                .id,
            "a"
        );
        assert_eq!(
            store
                .resolve_endpoint("E\u{301}lan()", &SearchOptions::default())
                .unwrap()
                .id,
            "b"
        );
    }
    assert!(errors.windows(2).all(|pair| pair[0] == pair[1]));
}

#[test]
fn normalized_endpoint_filters_keep_file_and_kind_scope_strict() {
    let mut class = node("class", "Crème()", "src/one.rs");
    class.kind = "class".into();
    let (_dir, store) = store(
        vec![
            class,
            node("function", "Cre\u{300}me()", "src/one.rs"),
            node("foreign", "Crème()", "src/two.rs"),
        ],
        vec![],
    );
    let options = SearchOptions {
        kinds: vec!["class".into()],
        ..SearchOptions::default()
    };
    for text in [
        "Creme",
        "src/one.rs::Creme",
        "./src/one.rs::Creme",
        "src/one.rs::Crè",
    ] {
        assert_eq!(store.resolve_endpoint(text, &options).unwrap().id, "class");
    }
    for text in [
        "missing.rs::Creme",
        "src/two.rs::Creme",
        "src/óne.rs::Creme",
    ] {
        assert!(
            store
                .resolve_endpoint(text, &options)
                .unwrap_err()
                .to_string()
                .contains("no symbol")
        );
    }
    let only_two = SearchOptions {
        files: vec!["src/two.rs".into()],
        ..SearchOptions::default()
    };
    assert_eq!(
        store.resolve_endpoint("Creme", &only_two).unwrap().id,
        "foreign"
    );
    assert!(
        store
            .resolve_endpoint("src/one.rs::Creme", &only_two)
            .is_err()
    );
    assert!(
        store
            .resolve_endpoint("src/one.rs::Creme", &SearchOptions::default())
            .unwrap_err()
            .to_string()
            .contains("ambiguous")
    );
}

#[test]
fn normalized_endpoint_prefix_substring_and_punctuation_remain_literal() {
    let (_dir, store) = store(
        vec![
            node("punctuated", "Plan /réviser — Code_%\\Étape", "a.rs"),
            node("wildcard-decoy", "CodeZZEtape", "a.rs"),
            node("greek", "Σύνδεση", "b.rs"),
            node("korean", "한글처리", "b.rs"),
            node("callable", "Déployer()", "a.rs"),
            node("long-callable", "DeployerLater()", "a.rs"),
            node("bare", "Évaluer", "a.rs"),
        ],
        vec![],
    );
    let options = SearchOptions::default();
    for (text, expected) in [
        ("Plan /reviser — Code_%\\Etape", "punctuated"),
        ("Plan /rev", "punctuated"),
        ("Code_%\\E", "punctuated"),
        ("%", "punctuated"),
        ("Συνδ", "greek"),
        ("\u{1112}\u{1161}\u{11ab}글", "korean"),
        ("Deployer", "callable"),
        ("Evaluer()", "bare"),
    ] {
        assert_eq!(
            store.resolve_endpoint(text, &options).unwrap().id,
            expected,
            "{text}"
        );
    }
    assert!(store.resolve_endpoint("Code_%\\Missing", &options).is_err());
    assert!(store.resolve_endpoint("\u{301}", &options).is_err());
}

#[test]
fn normalized_endpoint_catalog_finds_late_rivals_and_keeps_scope_priority() {
    let mut nodes = vec![node("a-first", "Révision", "small.rs")];
    nodes.extend((0..25_000).map(|i| {
        node(
            &format!("n{i:04}"),
            if i == 0 { "BoundaryCafé" } else { "Unrelated" },
            "large.rs",
        )
    }));
    // A canonical rival lies beyond the traversal limit; a sampled winner is wrong.
    nodes.push(node("z-last", "Re\u{301}vision", "other.rs"));
    let (_dir, store) = store(nodes, vec![]);
    let options = SearchOptions::default();
    let error = store
        .resolve_endpoint("Revision", &options)
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("ambiguous") && error.contains("a-first, z-last"),
        "{error}"
    );
    assert!(!error.contains("no symbol"));
    // Convenience lookup examines the catalog without loading full graph records.
    assert_eq!(
        store
            .resolve_endpoint("large.rs::BoundaryCafe", &options)
            .unwrap()
            .id,
        "n0000"
    );
    assert_eq!(
        store
            .resolve_endpoint("small.rs::Revision", &options)
            .unwrap()
            .id,
        "a-first"
    );
    assert_eq!(
        store.resolve_endpoint("a-first", &options).unwrap().id,
        "a-first"
    );
    // Ordinary spelling and exact labels still work on the same larger store.
    assert_eq!(
        store.resolve_endpoint("Révision", &options).unwrap().id,
        "a-first"
    );
    assert_eq!(
        store
            .resolve_endpoint("small.rs::Rév", &options)
            .unwrap()
            .id,
        "a-first"
    );
    assert_eq!(store.stats().unwrap().nodes, 25_002);
}

#[test]
fn late_endpoint_prefix_and_substring_support_neighbors_path_and_impact() {
    let mut nodes: Vec<_> = (0..25_000)
        .map(|i| node(&format!("n{i:05}"), "Unrelated", "large.rs"))
        .collect();
    nodes.push(node("z-source", "CrémeCaller", "late.rs"));
    nodes.push(node("z-target", "UsefulDestination", "late.rs"));
    let (_dir, store) = store(nodes, vec![edge("call", "z-source", "z-target", "calls")]);
    let options = SearchOptions::default();
    assert_eq!(
        store.resolve_endpoint("cremecall", &options).unwrap().id,
        "z-source"
    );
    assert_eq!(
        store.resolve_endpoint("Destination", &options).unwrap().id,
        "z-target"
    );
    let neighbors = store.neighbors_resolved("cremecall", &options).unwrap();
    assert_eq!(neighbors.seeds, ["z-source"]);
    assert_eq!(neighbors.graph.edges[0].id, "call");
    let path = store
        .path_extended("cremecall", "Destination", &options)
        .unwrap();
    assert!(path.found);
    assert_eq!(path.result.graph.edges[0].id, "call");
    let impact = store
        .impact_extended("Destination", &ImpactOptions::default())
        .unwrap();
    assert_eq!(impact.seeds, ["z-target"]);
    assert_eq!(impact.graph.edges[0].id, "call");
    assert_eq!(store.stats().unwrap().generation, 1);
}

#[test]
fn lexical_endpoint_scope_preserves_exact_accent_spelling_before_convenience() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::create(&dir.path().join("graph.db")).unwrap();
    store
        .apply_native(
            "/endpoint-fixture",
            vec![FileFacts {
                path: "src/one.rs".into(),
                hash: "one".into(),
                module: "one".into(),
                nodes: vec![
                    node("plain", "Resume", "src/one.rs"),
                    node("accent", "Résumé", "src/one.rs"),
                ],
                edges: vec![edge("call", "plain", "accent", "calls")],
                references: vec![],
                diagnostics: vec![],
            }],
            vec![],
            Coverage::default(),
        )
        .unwrap();
    let options = SearchOptions::default();
    for file in ["src/one.rs", "./src/one.rs", "/endpoint-fixture/src/one.rs"] {
        for (symbol, expected) in [("Resume", "plain"), ("Résumé", "accent")] {
            let text = format!("{file}::{symbol}");
            assert_eq!(
                store.resolve_endpoint(&text, &options).unwrap().id,
                expected,
                "{text}"
            );
        }
        let error = store
            .resolve_endpoint(&format!("{file}::RESUME"), &options)
            .unwrap_err()
            .to_string();
        assert!(error.contains("ambiguous"), "{file}: {error}");
        assert!(error.contains("accent, plain"), "{file}: {error}");
    }
    let path = store
        .path_extended("./src/one.rs::Resume", "./src/one.rs::Résumé", &options)
        .unwrap();
    assert!(path.found);
    assert_eq!(ids(&path.result.graph), ["plain", "accent"]);
    let impact = store
        .impact_extended("./src/one.rs::Résumé", &ImpactOptions::default())
        .unwrap();
    assert_eq!(impact.seeds, ["accent"]);
    assert_eq!(ids(&impact.graph), ["accent", "plain"]);
    for text in ["./missing.rs::Resume", "/outside/src/one.rs::Resume"] {
        assert!(
            store
                .resolve_endpoint(text, &options)
                .unwrap_err()
                .to_string()
                .contains("no symbol")
        );
    }
}

#[test]
fn lexical_endpoint_scope_keeps_raw_literals_and_excluded_id_precedence() {
    let mut qualified = node("qualified", "Other", "other.rs");
    qualified.qualified_name = Some("./src/one.rs::plain".into());
    let (_dir, store) = store(
        vec![
            node("plain", "Resume", "src/one.rs"),
            node("accent", "Résumé", "src/one.rs"),
            node("literal-label", "./src/one.rs::Resume", "other.rs"),
            node("./src/one.rs::Résumé", "OtherID", "other.rs"),
            node("src/one.rs::Resume", "RewrittenID", "other.rs"),
            qualified,
        ],
        vec![],
    );
    let options = SearchOptions::default();
    for (text, expected) in [
        ("./src/one.rs::Resume", "literal-label"),
        ("./src/one.rs::Résumé", "./src/one.rs::Résumé"),
        ("./src/one.rs::plain", "qualified"),
        // A normalized scope is not a new whole-input ID lookup.
        ("././src/one.rs::Resume", "plain"),
    ] {
        assert_eq!(
            store.resolve_endpoint(text, &options).unwrap().id,
            expected,
            "{text}"
        );
    }
    let only_one = SearchOptions {
        files: vec!["src/one.rs".into()],
        ..options.clone()
    };
    assert_eq!(
        store
            .resolve_endpoint("./src/one.rs::Resume", &only_one)
            .unwrap()
            .id,
        "plain"
    );
    assert!(
        store
            .resolve_endpoint("./src/one.rs::Résumé", &only_one)
            .unwrap_err()
            .to_string()
            .contains("no symbol")
    );
    let only_other = SearchOptions {
        files: vec!["other.rs".into()],
        ..options.clone()
    };
    assert!(
        store
            .resolve_endpoint("././src/one.rs::Resume", &only_other)
            .unwrap_err()
            .to_string()
            .contains("no symbol")
    );
    let wrong_kind = SearchOptions {
        kinds: vec!["class".into()],
        ..options
    };
    assert!(
        store
            .resolve_endpoint("././src/one.rs::Resume", &wrong_kind)
            .unwrap_err()
            .to_string()
            .contains("no symbol")
    );
}

#[test]
fn normalized_endpoint_byte_budget_rejects_oversized_name_but_exact_id_works() {
    let label = format!("Énorme{}", "x".repeat(8 * 1024 * 1024));
    let label_len = label.len();
    let (_dir, store) = store(vec![node("oversized", &label, "one.rs")], vec![]);
    let options = SearchOptions::default();
    let error = store
        .resolve_endpoint("Enorme", &options)
        .unwrap_err()
        .to_string();
    assert!(error.contains("normalization byte budget"), "{error}");
    let exact = store.resolve_endpoint("oversized", &options).unwrap();
    assert_eq!(exact.id, "oversized");
    assert_eq!(exact.label.len(), label_len);
    assert_eq!(store.stats().unwrap().nodes, 1);
}

#[test]
fn endpoint_ambiguity_is_independent_of_insertion_order_and_output_limit() {
    let nodes: Vec<_> = (0..150)
        .map(|i| node(&format!("n{i:03}"), &format!("Repeated{i:03}"), "a.rs"))
        .collect();
    let mut errors = Vec::new();
    for nodes in [nodes.clone(), nodes.into_iter().rev().collect()] {
        let (_dir, store) = store(nodes, vec![]);
        let error = store
            .resolve_endpoint(
                "Repeat",
                &SearchOptions {
                    graph: QueryOptions {
                        limit: 1,
                        ..QueryOptions::default()
                    },
                    ..SearchOptions::default()
                },
            )
            .unwrap_err()
            .to_string();
        assert!(error.contains("ambiguous"));
        assert!(error.contains("n000, n001"));
        errors.push(error);
    }
    assert_eq!(errors[0], errors[1]);
}

#[test]
fn impact_expands_only_initial_members_then_reverse_dependencies_with_sites() {
    let mut nodes: Vec<_> = [
        "root",
        "member",
        "nested",
        "caller",
        "typed",
        "derived",
        "impl",
        "doc",
        "importer",
        "outer",
        "peer",
        "noise",
        "downstream",
        "helper",
    ]
    .iter()
    .map(|s| node(s, s, "file.rs"))
    .collect();
    nodes[0].kind = "class".into();
    nodes[1].kind = "method".into();
    let mut call = edge("call", "caller", "nested", "calls");
    call.file = Some("site.rs".into());
    call.line = Some(73);
    let mut peer = edge("peer", "root", "peer", "uses");
    peer.directed = false;
    let (_dir, store) = store(
        nodes,
        vec![
            edge("member", "root", "member", "method"),
            edge("nested", "member", "nested", "contains"),
            call.clone(),
            edge("parallel", "caller", "nested", "calls"),
            edge("typed", "typed", "root", "uses_type"),
            edge("derived", "derived", "root", "extends"),
            edge("impl", "impl", "root", "implements"),
            edge("doc", "doc", "root", "documents"),
            edge("import", "importer", "root", "imports_from"),
            edge("outer", "outer", "importer", "dynamic_import"),
            peer.clone(),
            edge("noise", "noise", "root", "contains"),
            edge("downstream", "member", "downstream", "calls"),
            edge("helper", "importer", "helper", "contains"),
        ],
    );
    let options = ImpactOptions {
        search: SearchOptions {
            graph: QueryOptions {
                depth: 2,
                direction: Direction::Outgoing,
                ..QueryOptions::default()
            },
            induced_edges: true,
            ..SearchOptions::default()
        },
        ..ImpactOptions::default()
    };
    let result = store.impact_extended("root", &options).unwrap();
    assert_eq!(result.seeds, ["root", "member", "nested"]);
    assert_eq!(result.graph.nodes[0].id, "root");
    for expected in [
        "caller", "typed", "derived", "impl", "doc", "importer", "outer", "peer",
    ] {
        assert!(ids(&result.graph).contains(&expected), "{expected}");
    }
    for excluded in ["noise", "downstream", "helper"] {
        assert!(!ids(&result.graph).contains(&excluded), "{excluded}");
    }
    assert_eq!(result.graph.nodes.len(), 11);
    assert!(!result.graph.truncated);
    assert!(
        result
            .graph
            .edges
            .iter()
            .all(|e| e.relation != "contains" && e.relation != "method")
    );
    assert_eq!(
        serde_json::to_value(result.graph.edges.iter().find(|e| e.id == "call").unwrap()).unwrap(),
        serde_json::to_value(call).unwrap()
    );
    assert_eq!(
        serde_json::to_value(result.graph.edges.iter().find(|e| e.id == "peer").unwrap()).unwrap(),
        serde_json::to_value(peer).unwrap()
    );

    let calls = store
        .impact_extended(
            "root",
            &ImpactOptions {
                relations: vec!["calls".into()],
                ..options.clone()
            },
        )
        .unwrap();
    assert_eq!(ids(&calls.graph), ["root", "member", "nested", "caller"]);
    assert_eq!(calls.graph.edges.len(), 2);
    let contextual = store
        .impact_extended(
            "root",
            &ImpactOptions {
                search: SearchOptions {
                    contexts: vec!["call".into()],
                    ..options.search.clone()
                },
                ..ImpactOptions::default()
            },
        )
        .unwrap();
    assert_eq!(
        ids(&contextual.graph),
        ["root", "member", "nested", "caller"]
    );
    let documentation = store
        .impact_extended(
            "root",
            &ImpactOptions {
                relations: vec!["calls".into(), "documents".into()],
                search: SearchOptions {
                    graph: QueryOptions {
                        relation: Some("documents".into()),
                        ..QueryOptions::default()
                    },
                    ..SearchOptions::default()
                },
            },
        )
        .unwrap();
    assert_eq!(
        ids(&documentation.graph),
        ["root", "member", "nested", "doc"]
    );
    for relations in [
        vec!["contains".into()],
        vec!["".into()],
        vec!["x".into(); 33],
    ] {
        assert!(
            store
                .impact_extended(
                    "root",
                    &ImpactOptions {
                        relations,
                        ..ImpactOptions::default()
                    }
                )
                .is_err()
        );
    }
    let serialized = serde_json::to_value(&options).unwrap();
    let _: ImpactOptions = serde_json::from_value(serialized).unwrap();
}

#[test]
fn impact_file_seeds_include_recorded_members_and_native_absolute_paths() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::create(&dir.path().join("graph.db")).unwrap();
    let mut module = node("z-module", "core.rs", "src/core.rs");
    module.kind = "module".into();
    let function = node("a-function", "work", "src/core.rs");
    let caller = node("caller", "caller", "app.rs");
    store
        .apply_native(
            "/repo",
            vec![
                FileFacts {
                    path: "src/core.rs".into(),
                    hash: "core".into(),
                    module: "core".into(),
                    nodes: vec![function, module],
                    edges: vec![],
                    references: vec![],
                    diagnostics: vec![],
                },
                FileFacts {
                    path: "app.rs".into(),
                    hash: "app".into(),
                    module: "app".into(),
                    nodes: vec![caller],
                    edges: vec![edge("call", "caller", "a-function", "calls")],
                    references: vec![],
                    diagnostics: vec![],
                },
            ],
            vec![],
            Coverage::default(),
        )
        .unwrap();
    for text in [
        "src/core.rs",
        "./src/core.rs",
        "/repo/src/core.rs",
        "z-module",
    ] {
        let result = store
            .impact_extended(text, &ImpactOptions::default())
            .unwrap();
        assert_eq!(result.seeds, ["z-module", "a-function"]);
        assert_eq!(ids(&result.graph), ["z-module", "a-function", "caller"]);
    }
    assert!(
        store
            .impact_extended("/elsewhere/src/core.rs", &ImpactOptions::default())
            .is_err()
    );
    assert_eq!(
        store
            .resolve_endpoint("src/core.rs", &SearchOptions::default())
            .unwrap()
            .id,
        "z-module"
    );
    let filtered = store
        .impact_extended(
            "src/core.rs",
            &ImpactOptions {
                search: SearchOptions {
                    files: vec!["src/core.rs".into()],
                    ..SearchOptions::default()
                },
                ..ImpactOptions::default()
            },
        )
        .unwrap();
    assert_eq!(ids(&filtered.graph), ["z-module", "a-function"]);
    let functions = store
        .impact_extended(
            "src/core.rs",
            &ImpactOptions {
                search: SearchOptions {
                    kinds: vec!["function".into()],
                    ..SearchOptions::default()
                },
                ..ImpactOptions::default()
            },
        )
        .unwrap();
    assert_eq!(functions.seeds, ["a-function"]);
    assert_eq!(ids(&functions.graph), ["a-function", "caller"]);
}

#[test]
fn impact_keeps_root_first_and_reports_seed_token_and_membership_work_bounds() {
    let mut nodes = vec![node("z-root", "Root", "root.rs")];
    nodes.extend((0..30).map(|i| node(&format!("member{i:02}"), "member", "members.rs")));
    let mut edges: Vec<_> = (0..30)
        .map(|i| {
            edge(
                &format!("own{i:02}"),
                "z-root",
                &format!("member{i:02}"),
                "contains",
            )
        })
        .collect();
    let (_dir, mut store) = store(nodes.clone(), edges.clone());
    let all = store
        .impact_extended("z-root", &ImpactOptions::default())
        .unwrap();
    assert_eq!(all.seeds.len(), 31);
    let limited = store
        .impact_extended(
            "z-root",
            &ImpactOptions {
                search: SearchOptions {
                    graph: QueryOptions {
                        limit: 1,
                        ..QueryOptions::default()
                    },
                    ..SearchOptions::default()
                },
                ..ImpactOptions::default()
            },
        )
        .unwrap();
    assert_eq!(ids(&limited.graph), ["z-root"]);
    assert!(limited.truncation_reasons.iter().any(|s| s == "seed_limit"));
    let tokens = store
        .impact_extended(
            "z-root",
            &ImpactOptions {
                search: SearchOptions {
                    token_budget: Some(limited.estimated_tokens + 10),
                    ..SearchOptions::default()
                },
                ..ImpactOptions::default()
            },
        )
        .unwrap();
    assert_eq!(ids(&tokens.graph), ["z-root"]);
    assert!(
        tokens
            .truncation_reasons
            .iter()
            .any(|s| s == "token_budget")
    );
    edges.extend((0..5_100).map(|i| {
        edge(
            &format!("duplicate{i:04}"),
            "z-root",
            "member00",
            "contains",
        )
    }));
    store
        .refresh_import(ImportedGraph {
            nodes,
            edges,
            metadata: Value::Null,
        })
        .unwrap();
    let work = store
        .impact_extended("z-root", &ImpactOptions::default())
        .unwrap();
    assert!(work.truncation_reasons.iter().any(|s| s == "work_limit"));
    assert_eq!(work.graph.nodes[0].id, "z-root");
}

#[test]
fn bfs_and_dfs_admit_different_branches_under_a_node_budget() {
    let (_dir, store) = tree();
    let mut options = SearchOptions {
        graph: QueryOptions {
            depth: 3,
            limit: 3,
            direction: Direction::Outgoing,
            relation: None,
        },
        ..SearchOptions::default()
    };
    let bfs = store.neighbors_extended("a", &options).unwrap();
    options.traversal = Traversal::Dfs;
    let dfs = store.neighbors_extended("a", &options).unwrap();
    assert_eq!(ids(&bfs.graph), ["a", "b", "c"]);
    assert_eq!(ids(&dfs.graph), ["a", "b", "d"]);
    for result in [bfs, dfs] {
        assert!(result.graph.truncated);
        assert!(result.truncation_reasons.iter().any(|s| s == "node_limit"));
        assert!(
            result
                .graph
                .edges
                .iter()
                .all(|e| ids(&result.graph).contains(&e.source.as_str())
                    && ids(&result.graph).contains(&e.target.as_str()))
        );
    }
    // The old API retains its bounded breadth-first behavior.
    assert_eq!(
        ids(&store.neighbors("a", &options.graph).unwrap()),
        ["a", "b", "c"]
    );
}

#[test]
fn context_file_kind_filters_apply_to_seeds_and_every_expansion() {
    let mut nodes = vec![
        node("a", "Entrypoint", "keep.rs"),
        node("b", "Worker", "keep.rs"),
        node("c", "Module", "keep.rs"),
        node("d", "Elsewhere", "other.rs"),
        node("e", "Imported", "keep.rs"),
    ];
    nodes[2].kind = "module".into();
    let mut relation = edge("1", "a", "b", "custom");
    relation.metadata = json!({"project":"one","original_metadata":{"context":"call"}});
    let (_dir, store) = store(
        nodes,
        vec![
            relation,
            edge("2", "a", "c", "calls"),
            edge("3", "a", "d", "calls"),
            edge("4", "b", "e", "imports"),
        ],
    );
    let options = SearchOptions {
        graph: QueryOptions {
            depth: 3,
            ..QueryOptions::default()
        },
        contexts: vec!["calls".into()],
        files: vec!["keep.rs".into()],
        kinds: vec!["function".into()],
        ..SearchOptions::default()
    };
    let result = store
        .query_extended("Who calls Entrypoint?", &options)
        .unwrap();
    assert_eq!(ids(&result.graph), ["a", "b"]);
    assert_eq!(result.contexts, ["call"]);
    assert!(!result.graph.truncated);
    assert_eq!(
        store
            .query_extended("Elsewhere", &options)
            .unwrap()
            .graph
            .nodes
            .len(),
        0
    );
    let mut inferred = options.clone();
    inferred.contexts.clear();
    inferred.infer_context = true;
    assert_eq!(
        ids(&store
            .query_extended("Who calls Entrypoint?", &inferred)
            .unwrap()
            .graph),
        ["a", "b"]
    );
    inferred.contexts = vec!["import".into()];
    assert_eq!(
        ids(&store
            .query_extended("Who calls Entrypoint?", &inferred)
            .unwrap()
            .graph),
        ["a"]
    );
}

#[test]
fn scoped_endpoints_are_exact_and_ambiguity_never_guesses() {
    let (_dir, mut store) = store(
        vec![node("a", "Same", "alpha.rs"), node("b", "Same", "beta.rs")],
        vec![edge("e", "a", "b", "calls")],
    );
    let options = SearchOptions::default();
    assert!(
        store
            .neighbors_extended("Same", &options)
            .unwrap_err()
            .to_string()
            .contains("ambiguous")
    );
    assert_eq!(
        ids(&store
            .neighbors_extended(
                "alpha.rs::Same",
                &SearchOptions {
                    graph: QueryOptions {
                        depth: 0,
                        ..QueryOptions::default()
                    },
                    ..options.clone()
                }
            )
            .unwrap()
            .graph),
        ["a"]
    );
    assert!(
        store
            .neighbors_extended("missing.rs::Same", &options)
            .is_err()
    );
    let path = store
        .path_extended("alpha.rs::Same", "beta.rs::Same", &options)
        .unwrap();
    assert!(path.found);
    assert_eq!(ids(&path.result.graph), ["a", "b"]);
    let scoped = SearchOptions {
        files: vec!["beta.rs".into()],
        ..options.clone()
    };
    assert_eq!(
        ids(&store.neighbors_extended("Same", &scoped).unwrap().graph),
        ["b"]
    );
    // A real ID or label containing :: takes priority over parsing that syntax.
    store
        .refresh_import(ImportedGraph {
            nodes: vec![
                node("a", "Same", "alpha.rs"),
                node("literal", "alpha.rs::Same", "other.rs"),
            ],
            edges: vec![],
            metadata: Value::Null,
        })
        .unwrap();
    assert_eq!(
        ids(&store
            .neighbors_extended("alpha.rs::Same", &options)
            .unwrap()
            .graph),
        ["literal"]
    );
}

#[test]
fn induced_closure_preserves_parallel_mutual_undirected_and_self_edges() {
    let mut edges = vec![
        edge("1", "a", "b", "calls"),
        edge("2", "b", "a", "calls"),
        edge("3", "a", "b", "calls"),
        edge("4", "a", "a", "calls"),
    ];
    edges[2].directed = false;
    let (_dir, store) = store(
        vec![node("a", "seed", "a.rs"), node("b", "seed", "b.rs")],
        edges,
    );
    let mut options = SearchOptions {
        graph: QueryOptions {
            depth: 0,
            direction: Direction::Incoming,
            ..QueryOptions::default()
        },
        ..SearchOptions::default()
    };
    assert!(
        store
            .query_extended("seed", &options)
            .unwrap()
            .graph
            .edges
            .is_empty()
    );
    options.induced_edges = true;
    let result = store.query_extended("seed", &options).unwrap();
    assert_eq!(result.graph.edges.len(), 4);
    assert_eq!(
        result
            .graph
            .edges
            .iter()
            .map(|e| e.id.as_str())
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        4
    );
    assert!(
        !result
            .graph
            .edges
            .iter()
            .find(|e| e.id == "3")
            .unwrap()
            .directed
    );
    options.graph.relation = Some("imports".into());
    assert!(
        store
            .query_extended("seed", &options)
            .unwrap()
            .graph
            .edges
            .is_empty()
    );
}

#[test]
fn token_budget_keeps_complete_records_primary_seed_and_truthful_paths() {
    let (_dir, store) = tree();
    let base = store
        .neighbors_extended(
            "a",
            &SearchOptions {
                graph: QueryOptions {
                    depth: 0,
                    ..QueryOptions::default()
                },
                ..SearchOptions::default()
            },
        )
        .unwrap();
    let cap = base.estimated_tokens + 10;
    let options = SearchOptions {
        graph: QueryOptions {
            depth: 3,
            ..QueryOptions::default()
        },
        token_budget: Some(cap),
        ..SearchOptions::default()
    };
    let result = store.neighbors_extended("a", &options).unwrap();
    assert_eq!(ids(&result.graph), ["a"]);
    assert!(result.graph.edges.is_empty());
    assert_eq!(result.seeds, ["a"]);
    assert!(result.estimated_tokens <= cap);
    assert!(
        result
            .truncation_reasons
            .iter()
            .any(|s| s == "token_budget")
    );
    assert_eq!(
        result.estimated_tokens,
        serde_json::to_vec(&result.graph).unwrap().len().div_ceil(4)
    );
    assert!(
        store
            .neighbors_extended(
                "a",
                &SearchOptions {
                    token_budget: Some(1),
                    ..SearchOptions::default()
                }
            )
            .is_err()
    );
    assert!(store.path_extended("a", "d", &options).is_err());
    let result = store
        .path_extended(
            "a",
            "d",
            &SearchOptions {
                graph: QueryOptions {
                    depth: 3,
                    ..QueryOptions::default()
                },
                ..SearchOptions::default()
            },
        )
        .unwrap();
    assert!(result.found);
    assert_eq!(ids(&result.result.graph), ["a", "b", "d"]);
}

#[test]
fn ranked_search_finds_late_multiterm_winner_independent_of_insertion_order() {
    let mut nodes: Vec<_> = (0..256)
        .flat_map(|i| {
            [
                node(&format!("a{i:03}"), &format!("OrchidNoise{i:03}"), "a.rs"),
                node(&format!("b{i:03}"), &format!("CobaltNoise{i:03}"), "b.rs"),
            ]
        })
        .collect();
    nodes.push(node("z-winner", "Orchid Cobalt Coordinator", "winner.rs"));
    let options = SearchOptions {
        graph: QueryOptions {
            depth: 0,
            ..QueryOptions::default()
        },
        ..SearchOptions::default()
    };
    let mut expected = None;
    for _ in 0..3 {
        let (_dir, graph) = store(nodes.clone(), vec![]);
        let result = graph.query_extended("Orchid Cobalt", &options).unwrap();
        assert_eq!(result.seeds[0], "z-winner");
        assert!(!result.graph.truncated);
        if let Some(expected) = &expected {
            assert_eq!(&result.seeds, expected);
        } else {
            expected = Some(result.seeds);
        }
        nodes.reverse();
        nodes.rotate_left(71);
    }
}

#[test]
fn ranked_search_late_exact_and_prose_hits_beat_early_prefix_and_file_hits() {
    let mut nodes: Vec<_> = (0..128)
        .map(|i| {
            node(
                &format!("a{i:03}"),
                &format!("WaypointNoise{i:03}"),
                "glassword.rs",
            )
        })
        .collect();
    nodes.push(node("z-exact", "Waypoint()", "exact.rs"));
    let mut prose = node("z-prose", "Decision", "notes.md");
    prose.metadata = json!({"rationale":"glassword tradeoff"});
    nodes.push(prose);
    let options = SearchOptions {
        graph: QueryOptions {
            depth: 0,
            ..QueryOptions::default()
        },
        ..SearchOptions::default()
    };
    for _ in 0..2 {
        let (_dir, graph) = store(nodes.clone(), vec![]);
        for (query, winner) in [("who uses Waypoint", "z-exact"), ("glassword", "z-prose")] {
            assert_eq!(
                graph.query_extended(query, &options).unwrap().seeds[0],
                winner
            );
        }
        nodes.reverse();
    }
}

#[test]
fn ranked_search_budget_is_independent_of_traversal_size_and_keeps_late_hits() {
    let mut nodes: Vec<_> = (0..4_999)
        .map(|i| {
            node(
                &format!("n{i:04}"),
                &format!("SaffronNoise{i:04}"),
                "large.rs",
            )
        })
        .collect();
    nodes.push(node("z-winner", "Saffron()", "large.rs"));
    nodes.push(node("other", "SaffronRival", "other.rs"));
    let (_dir, graph) = store(nodes, vec![]);
    let options = SearchOptions {
        graph: QueryOptions {
            depth: 0,
            limit: 1,
            ..QueryOptions::default()
        },
        ..SearchOptions::default()
    };
    assert_eq!(
        graph.query_extended("Saffron", &options).unwrap().seeds,
        ["z-winner"]
    );
    let scoped = SearchOptions {
        files: vec!["large.rs".into()],
        ..options.clone()
    };
    assert_eq!(
        graph.query_extended("Saffron", &scoped).unwrap().seeds,
        ["z-winner"]
    );
    assert_eq!(
        graph.query_extended("z-winner", &options).unwrap().seeds,
        ["z-winner"]
    );
    assert_eq!(graph.stats().unwrap().nodes, 5_001);
}

#[test]
fn ranked_search_byte_budget_is_explicit_and_does_not_poison_next_read() {
    let mut huge = node("huge", "SmallLabel", "file.rs");
    huge.metadata = json!({"summary": format!("memoryword {}", "x".repeat(8 * 1024 * 1024))});
    let (_dir, graph) = store(vec![huge], vec![]);
    let error = graph
        .query_extended("memoryword", &SearchOptions::default())
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("candidate enumeration exceeded its byte budget"),
        "{error}"
    );
    assert_eq!(graph.stats().unwrap().nodes, 1);
}

#[test]
fn ranked_search_uses_content_terms_diversity_prose_and_unicode() {
    let mut rationale = node("r", "Decision", "notes.md");
    rationale.metadata = json!({"rationale":"latency tradeoff", "private_field":"sensitiveword"});
    let (_dir, store) = store(
        vec![
            node("a", "AlphaProcessor", "a.rs"),
            node("b", "BetaEngine", "b.rs"),
            node("c", "CallsDecoy", "c.rs"),
            node("u", "Café", "cafe.md"),
            node("z", "知识图谱缓存", "zh.md"),
            rationale,
        ],
        vec![],
    );
    let options = SearchOptions {
        graph: QueryOptions {
            depth: 0,
            ..QueryOptions::default()
        },
        ..SearchOptions::default()
    };
    let result = store
        .query_extended("who calls Alpha Beta", &options)
        .unwrap();
    assert!(ids(&result.graph).contains(&"a"));
    assert!(ids(&result.graph).contains(&"b"));
    assert!(!ids(&result.graph).contains(&"c"));
    assert_eq!(
        ids(&store.query_extended("latency", &options).unwrap().graph),
        ["r"]
    );
    assert_eq!(
        ids(&store.query_extended("cafe", &options).unwrap().graph),
        ["u"]
    );
    assert_eq!(
        ids(&store.query_extended("图谱", &options).unwrap().graph),
        ["z"]
    );
    assert!(
        store
            .query_extended("sensitiveword", &options)
            .unwrap()
            .graph
            .nodes
            .is_empty()
    );
    let serialized = serde_json::to_value(&options).unwrap();
    assert_eq!(serialized["traversal"], "bfs");
    let parsed: SearchOptions = serde_json::from_value(json!({"traversal":"dfs"})).unwrap();
    assert_eq!(parsed.traversal, Traversal::Dfs);
}

#[test]
fn search_compatibility_forms_are_indexed_without_changing_stored_labels() {
    let nodes = vec![
        node("ligature", "ﬂux processor", "a.rs"),
        node("compound", "ＷｉｄｅＰｒｏｃｅｓｓｏｒ", "c.rs"),
    ];
    let (_dir, graph) = store(nodes, vec![]);
    for (query, id, label) in [
        ("flux", "ligature", "ﬂux processor"),
        ("ＦＬＵＸ", "ligature", "ﬂux processor"),
        ("WideProcessor", "compound", "ＷｉｄｅＰｒｏｃｅｓｓｏｒ"),
    ] {
        let result = graph
            .query_extended(query, &SearchOptions::default())
            .unwrap();
        assert_eq!(result.seeds, [id], "{query}");
        assert_eq!(result.graph.nodes[0].label, label);
    }
}

#[test]
fn fts_preserves_korean_and_greek_spelling_for_imported_and_native_prefixes() {
    let nodes = vec![
        node("ko", "한국어 문서", "notes.txt"),
        node("el", "άδεια χρήστη", "notes.txt"),
    ];
    let (dir, imported) = store(nodes.clone(), vec![]);
    let mut native = Store::create(&dir.path().join("native.db")).unwrap();
    native
        .apply_native(
            "repo",
            vec![FileFacts {
                path: "notes.txt".into(),
                hash: "content".into(),
                module: "notes".into(),
                nodes,
                edges: vec![],
                references: vec![],
                diagnostics: vec![],
            }],
            vec![],
            Coverage::default(),
        )
        .unwrap();
    let options = SearchOptions {
        graph: QueryOptions {
            depth: 0,
            ..QueryOptions::default()
        },
        ..SearchOptions::default()
    };
    for graph in [&imported, &native] {
        // Neither IDs nor filenames match; full-label lookup cannot hide a
        // mismatch between the query spelling and the FTS tokenizer.
        for (text, expected) in [("한국", "ko"), ("άδε", "el"), ("άδεια", "el")] {
            let result = graph.query_extended(text, &options).unwrap();
            assert_eq!(ids(&result.graph), [expected], "{text}");
            assert_eq!(result.seeds, [expected]);
            assert!(!result.graph.truncated);
            assert_eq!(ids(&graph.query(text, &options.graph).unwrap()), [expected]);
        }
    }
}

#[test]
fn complete_candidate_ranking_and_traversal_work_caps_are_independent() {
    let mut nodes: Vec<_> = (0..80)
        .map(|i| node(&format!("n{i}"), &format!("Shared{i}"), "file.rs"))
        .collect();
    nodes.push(node("root", "Root", "file.rs"));
    let edges = (0..5100)
        .map(|i| {
            let mut e = edge(&format!("e{i:05}"), "root", "n0", "calls");
            e.metadata = json!({"context":"other"});
            e
        })
        .collect();
    let (_dir, store) = store(nodes, edges);
    let result = store
        .query_extended(
            "Shared",
            &SearchOptions {
                graph: QueryOptions {
                    depth: 0,
                    ..QueryOptions::default()
                },
                ..SearchOptions::default()
            },
        )
        .unwrap();
    assert!(!result.graph.truncated);
    assert_eq!(ids(&result.graph), ["n0", "n1", "n2"]);
    let result = store
        .neighbors_extended(
            "root",
            &SearchOptions {
                contexts: vec!["call".into()],
                ..SearchOptions::default()
            },
        )
        .unwrap();
    assert!(result.graph.edges.is_empty());
    assert!(result.truncation_reasons.iter().any(|s| s == "work_limit"));
    for options in [
        SearchOptions {
            files: vec!["x".into(); 33],
            ..SearchOptions::default()
        },
        SearchOptions {
            kinds: vec!["x".repeat(1025)],
            ..SearchOptions::default()
        },
        SearchOptions {
            contexts: vec!["".into()],
            ..SearchOptions::default()
        },
        SearchOptions {
            token_budget: Some(1_000_001),
            ..SearchOptions::default()
        },
    ] {
        assert!(store.query_extended("root", &options).is_err());
    }
    assert_eq!(store.stats().unwrap().edges, 5100); // handler never leaks into other requests
}

#[test]
fn search_migration_is_write_only_atomic_and_advances_generation_once() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("native.db");
    let mut store = Store::create(&path).unwrap();
    let mut n = node("stable-id", "Decision", "notes.md");
    n.metadata = json!({"description":"searchableword"});
    store
        .apply_native(
            "root",
            vec![FileFacts {
                path: "notes.md".into(),
                hash: "same".into(),
                module: "notes".into(),
                nodes: vec![n],
                edges: vec![],
                references: vec![],
                diagnostics: vec![],
            }],
            vec![],
            Coverage::default(),
        )
        .unwrap();
    let sql = rusqlite::Connection::open(&path).unwrap();
    // Emulate the previous public FTS representation without changing payloads.
    sql.execute_batch("UPDATE nodes SET search='Decision notes.md'; DELETE FROM node_search; INSERT INTO node_search(rowid,text) SELECT rowid,search FROM nodes; ALTER TABLE metadata DROP COLUMN search_version;").unwrap();
    drop(store);
    let mut store = Store::open(&path).unwrap();
    assert!(
        store
            .query_extended("searchableword", &SearchOptions::default())
            .unwrap()
            .graph
            .nodes
            .is_empty()
    );
    assert_eq!(store.stats().unwrap().generation, 1);
    assert_eq!(
        sql.query_row(
            "SELECT count(*) FROM pragma_table_info('metadata') WHERE name='search_version'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    sql.execute_batch("CREATE TRIGGER block_migration BEFORE UPDATE OF search ON nodes BEGIN SELECT RAISE(ABORT,'test migration failure'); END;").unwrap();
    assert!(
        store
            .apply_native("root", vec![], vec![], Coverage::default())
            .is_err()
    );
    assert_eq!(store.stats().unwrap().generation, 1);
    assert_eq!(
        sql.query_row(
            "SELECT count(*) FROM pragma_table_info('metadata') WHERE name='search_version'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    sql.execute_batch("DROP TRIGGER block_migration").unwrap();
    assert_eq!(
        store
            .apply_native("root", vec![], vec![], Coverage::default())
            .unwrap()
            .generation,
        2
    );
    assert_eq!(
        ids(&store
            .query_extended("searchableword", &SearchOptions::default())
            .unwrap()
            .graph),
        ["stable-id"]
    );
    assert_eq!(
        store
            .apply_native("root", vec![], vec![], Coverage::default())
            .unwrap()
            .generation,
        2
    );
    assert_eq!(
        sql.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn read_only_handles_see_new_wal_commits_and_refuse_writes() {
    let (dir, mut writer) = tree();
    let path = dir.path().join("graph.db");
    let reader = Store::open_read_only(&path).unwrap();
    assert_eq!(
        serde_json::to_vec(&reader.query("a", &QueryOptions::default()).unwrap()).unwrap(),
        serde_json::to_vec(&writer.query("a", &QueryOptions::default()).unwrap()).unwrap()
    );
    writer
        .refresh_import(ImportedGraph {
            nodes: vec![node("new", "New", "new.rs")],
            edges: vec![],
            metadata: json!({"new":true}),
        })
        .unwrap();
    assert_eq!(reader.snapshot().unwrap().generation, 2);
    assert_eq!(
        ids(&reader
            .query_extended("New", &SearchOptions::default())
            .unwrap()
            .graph),
        ["new"]
    );
    assert_eq!(reader.graph_metadata().unwrap(), json!({"new":true}));
    let mut fresh_reader = Store::open_read_only(&path).unwrap();
    assert!(
        fresh_reader
            .refresh_import(ImportedGraph {
                nodes: vec![],
                edges: vec![],
                metadata: Value::Null
            })
            .is_err()
    );
    assert_eq!(writer.stats().unwrap().generation, 2);
}

#[cfg(unix)]
#[test]
fn read_only_os_file_supports_normal_reads_without_database_changes() {
    use std::os::unix::fs::PermissionsExt;
    let (dir, writer) = tree();
    let path = dir.path().join("graph.db");
    let expected =
        serde_json::to_vec(&writer.query("a", &QueryOptions::default()).unwrap()).unwrap();
    drop(writer);
    let sql = rusqlite::Connection::open(&path).unwrap();
    sql.pragma_update(None, "journal_mode", "DELETE").unwrap();
    drop(sql);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444)).unwrap();
    let before = std::fs::read(&path).unwrap();
    let reader = Store::open_read_only(&path).unwrap();
    assert_eq!(
        serde_json::to_vec(&reader.query("a", &QueryOptions::default()).unwrap()).unwrap(),
        expected
    );
    reader
        .neighbors_extended("a", &SearchOptions::default())
        .unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o444
    );
    drop(reader);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
}

#[test]
fn dfs_revisits_shorter_routes_without_losing_depth_bounded_neighbors() {
    let (_dir, store) = store(
        ["a", "b", "c", "x", "y"]
            .map(|s| node(s, s, "tree.rs"))
            .to_vec(),
        vec![
            edge("1", "a", "b", "calls"),
            edge("2", "b", "c", "calls"),
            edge("3", "c", "x", "calls"),
            edge("4", "a", "x", "calls"),
            edge("5", "x", "y", "calls"),
        ],
    );
    let options = SearchOptions {
        graph: QueryOptions {
            depth: 3,
            direction: Direction::Outgoing,
            ..QueryOptions::default()
        },
        traversal: Traversal::Dfs,
        ..SearchOptions::default()
    };
    let result = store.neighbors_extended("a", &options).unwrap();
    assert_eq!(ids(&result.graph), ["a", "b", "c", "x", "y"]);
    assert_eq!(result.graph.edges.len(), 5);
    assert!(!result.graph.truncated);
}
