use graf::{
    languages::parse,
    model::{Coverage, FileFacts, GraphSnapshot, Node, Reference},
    store::Store,
};

const PATHS: [&str; 4] = ["values.js", "values.ts", "values.go", "src/lib.rs"];

fn source(path: &str, javascript: &str, go: &str, rust: &str) -> String {
    match path {
        "values.js" | "values.ts" => format!(
            "function alpha() {{ return 1; }}\nfunction omega() {{ return 2; }}\nexport function route(flag, unknown) {{\n{javascript}\n}}\nexport function control() {{ return omega(); }}\n"
        ),
        "values.go" => format!(
            "package values\nfunc alpha() int {{ return 1 }}\nfunc omega() int {{ return 2 }}\nfunc route(flag bool, unknown func() int) int {{\n{go}\n}}\nfunc control() int {{ return omega() }}\n"
        ),
        "src/lib.rs" => format!(
            "fn alpha() -> i32 {{ 1 }}\nfn omega() -> i32 {{ 2 }}\npub fn route(flag: bool, unknown: fn() -> i32) -> i32 {{\n{rust}\n}}\npub fn control() -> i32 {{ omega() }}\n"
        ),
        _ => unreachable!(),
    }
}

fn facts(path: &str, source: &str) -> FileFacts {
    let facts = parse(
        path,
        source,
        blake3::hash(source.as_bytes()).to_hex().as_ref(),
    )
    .unwrap()
    .unwrap();
    assert!(
        facts.diagnostics.is_empty(),
        "{path}: {:?}\n{source}",
        facts.diagnostics
    );
    facts
}

fn node<'a>(facts: &'a FileFacts, name: &str) -> &'a Node {
    facts
        .nodes
        .iter()
        .find(|n| n.qualified_name.as_deref() == Some(name))
        .unwrap()
}

fn selected_calls(facts: &FileFacts) -> Vec<&Reference> {
    facts
        .references
        .iter()
        .filter(|r| r.relation == "calls" && r.label == "selected")
        .collect()
}

fn assert_targets(path: &str, source: &str, expected: &[Option<&str>]) -> FileFacts {
    let facts = facts(path, source);
    let calls = selected_calls(&facts);
    assert_eq!(calls.len(), expected.len(), "{path}: {source}");
    let mut ids = std::collections::HashSet::new();
    for (call, target) in calls.iter().zip(expected) {
        assert!(ids.insert(&call.id));
        assert_eq!(call.file, path);
        let (start, end) = call.id.rsplit_once(':').unwrap().1.split_once('-').unwrap();
        let start: usize = start.parse().unwrap();
        let end: usize = end.parse().unwrap();
        assert_eq!(&source[start..end], "selected()");
        assert_eq!(
            call.line as usize,
            source[..start].bytes().filter(|b| *b == b'\n').count() + 1
        );
        match target {
            Some(name) => {
                let target = node(&facts, name);
                assert_eq!(call.candidate_keys.len(), 1, "{path}: {source}");
                assert!(
                    call.candidate_keys.iter().all(|key| {
                        target.binding_key.as_ref() == Some(key)
                            || target.metadata["binding_aliases"]
                                .as_array()
                                .is_some_and(|aliases| aliases.iter().any(|alias| alias == key))
                    }),
                    "{path}: {name}: {:?}",
                    call.candidate_keys
                );
                assert!(
                    call.reason
                        .starts_with("straight-line local function value;")
                );
            }
            None => assert!(call.candidate_keys.is_empty(), "{path}: {source}\n{call:?}"),
        }
    }
    facts
}

fn graph_value(mut graph: GraphSnapshot) -> serde_json::Value {
    graph.nodes.sort_by(|a, b| a.id.cmp(&b.id));
    graph.edges.sort_by(|a, b| a.id.cmp(&b.id));
    serde_json::json!({"nodes": graph.nodes, "edges": graph.edges})
}

#[test]
fn callable_values_capture_each_written_call_before_later_assignment() {
    for path in PATHS {
        let contents = source(
            path,
            "let selected = alpha;\nselected();\nselected = omega;\nreturn selected();",
            "selected := alpha\nselected()\nselected = omega\nreturn selected()",
            "let mut selected: fn() -> i32 = alpha;\nselected();\nselected = omega;\nselected()",
        );
        for forward in [false, true] {
            let contents = if forward {
                // Item/hoisted function definitions also work after the caller.
                let split = contents
                    .find(match path {
                        "values.go" => "func route",
                        "src/lib.rs" => "pub fn route",
                        _ => "export function route",
                    })
                    .unwrap();
                if path == "values.go" {
                    let package_end = contents.find('\n').unwrap() + 1;
                    format!(
                        "{}{}{}",
                        &contents[..package_end],
                        &contents[split..],
                        &contents[package_end..split]
                    )
                } else {
                    format!("{}{}", &contents[split..], &contents[..split])
                }
            } else {
                contents.clone()
            };
            let original = assert_targets(path, &contents, &[Some("alpha"), Some("omega")]);
            let calls = selected_calls(&original);
            assert!(
                calls
                    .iter()
                    .all(|r| r.source == node(&original, "route").id)
            );
            let provenance: Vec<_> = calls.iter().map(|r| (r.id.clone(), r.line)).collect();
            assert!(!original.nodes.iter().any(|n| n.label == "selected"));
            let directory = tempfile::tempdir().unwrap();
            let mut store = Store::create(&directory.path().join("graph.db")).unwrap();
            store
                .apply_native(
                    "fixture",
                    vec![original.clone()],
                    vec![],
                    Coverage::default(),
                )
                .unwrap();
            let graph = store.snapshot().unwrap();
            for ((id, line), target) in provenance.iter().zip(["alpha", "omega"]) {
                let edge = graph
                    .edges
                    .iter()
                    .find(|e| e.metadata["reference_id"] == *id)
                    .unwrap();
                assert_eq!(edge.relation, "calls");
                assert_eq!(edge.source, node(&original, "route").id);
                assert_eq!(edge.target, node(&original, target).id);
                assert_eq!(edge.file.as_deref(), Some(path));
                assert_eq!(edge.line, Some(*line));
            }
            assert_eq!(
                graph.edges.iter().filter(|e| e.relation == "calls").count(),
                3
            );
            assert!(graph.edges.iter().any(|e| e.relation == "calls"
                && e.source == node(&original, "control").id
                && e.target == node(&original, "omega").id));
        }
        // Also cover ordinary function-local var declarations without reassigning.
        let contents = source(
            path,
            "var selected = alpha;\nreturn selected();",
            "var selected func() int = alpha\nreturn selected()",
            "let selected: fn() -> i32 = alpha;\nselected()",
        );
        assert_targets(path, &contents, &[Some("alpha")]);
    }
}

#[test]
fn callable_values_keep_control_unknown_and_lexical_shadow_boundaries() {
    let cases: &[(&str, &str, &str, &[Option<&str>])] = &[
        (
            "let selected = alpha;\nselected();\nselected = unknown;\nreturn selected();",
            "selected := alpha\nselected()\nselected = unknown\nreturn selected()",
            "let mut selected: fn() -> i32 = alpha;\nselected();\nselected = unknown;\nselected()",
            &[Some("alpha"), None],
        ),
        (
            "let selected = alpha;\nselected();\nif (flag) { if (flag) { selected = omega; } }\nselected();\nselected = omega;\nreturn selected();",
            "selected := alpha\nselected()\nif flag { if flag { selected = omega } }\nselected()\nselected = omega\nreturn selected()",
            "let mut selected: fn() -> i32 = alpha;\nselected();\nif flag { if flag { selected = omega; } }\nselected();\nselected = omega;\nselected()",
            &[Some("alpha"), None, Some("omega")],
        ),
        (
            "let selected = alpha;\nselected();\nwhile (flag) { selected = omega; selected(); }\nselected();\nselected = omega;\nreturn selected();",
            "selected := alpha\nselected()\nfor flag { selected = omega; selected() }\nselected()\nselected = omega\nreturn selected()",
            "let mut selected: fn() -> i32 = alpha;\nselected();\nwhile flag { selected = omega; selected(); }\nselected();\nselected = omega;\nselected()",
            &[Some("alpha"), None, None, Some("omega")],
        ),
        (
            "let selected = alpha;\n{ let selected = unknown; selected(); }\nreturn selected();",
            "selected := alpha\n{ selected := unknown; selected() }\nreturn selected()",
            "let selected: fn() -> i32 = alpha;\n{ let selected = unknown; selected(); }\nselected()",
            &[None, Some("alpha")],
        ),
        (
            "let alpha = unknown;\nlet selected = alpha;\nselected();\nselected = omega;\nreturn selected();",
            "alpha := unknown\nselected := alpha\nselected()\nselected = omega\nreturn selected()",
            "let alpha = unknown;\nlet mut selected: fn() -> i32 = alpha;\nselected();\nselected = omega;\nselected()",
            &[None, Some("omega")],
        ),
        (
            "let middle = alpha;\nlet selected = middle;\nreturn selected();",
            "middle := alpha\nselected := middle\nreturn selected()",
            "let middle: fn() -> i32 = alpha;\nlet selected = middle;\nselected()",
            &[None],
        ),
        (
            "let selected = alpha;\nselected = manufacture();\nreturn selected();",
            "selected := alpha\nselected = manufacture()\nreturn selected()",
            "let mut selected: fn() -> i32 = alpha;\nselected = manufacture();\nselected()",
            &[None],
        ),
    ];
    for path in PATHS {
        for (javascript, go, rust, expected) in cases {
            let contents = source(path, javascript, go, rust);
            let parsed = assert_targets(path, &contents, expected);
            assert!(
                parsed
                    .references
                    .iter()
                    .any(|r| r.source == node(&parsed, "control").id
                        && r.relation == "calls"
                        && !r.candidate_keys.is_empty())
            );
        }
        // A same-package/global function with the alias spelling is no fallback
        // for a parameter or a tracked local that has become unknown.
        let extra = match path {
            "values.go" => "\nfunc selected() int { return 9 }\n",
            "src/lib.rs" => "\nfn selected() -> i32 { 9 }\n",
            _ => "\nfunction selected() { return 9; }\n",
        };
        let contents = source(
            path,
            "let selected = alpha;\nselected = unknown;\nreturn selected();",
            "selected := alpha\nselected = unknown\nreturn selected()",
            "let mut selected: fn() -> i32 = alpha;\nselected = unknown;\nselected()",
        ) + extra;
        assert_targets(path, &contents, &[None]);
        let contents = source(path, "return unknown();", "return unknown()", "unknown()")
            .replace("unknown", "selected")
            + extra;
        assert_targets(path, &contents, &[None]);
    }
}

#[test]
fn callable_values_do_not_cross_capture_address_or_dynamic_boundaries() {
    for path in PATHS {
        let contents = source(
            path,
            "let selected = alpha;\nconst later = () => selected();\nship(later);\nselected = omega;\nreturn selected();",
            "selected := alpha\nlater := func() int { return selected() }\nship(later)\nselected = omega\nreturn selected()",
            "let mut selected: fn() -> i32 = alpha;\nlet later = move || selected();\nship(later);\nselected = omega;\nselected()",
        );
        let parsed = assert_targets(path, &contents, &[None, Some("omega")]);
        let calls = selected_calls(&parsed);
        assert_ne!(calls[0].source, node(&parsed, "route").id);
        assert_eq!(calls[1].source, node(&parsed, "route").id);

        let contents = source(
            path,
            "let selected = alpha;\nfunction change() { selected = omega; }\nship(change);\nselected = omega;\nreturn selected();",
            "selected := alpha\nchange := func() { selected = omega }\nship(change)\nselected = omega\nreturn selected()",
            "let mut selected: fn() -> i32 = alpha;\nlet change = || { selected = omega; };\nship(change);\nselected = omega;\nselected()",
        );
        assert_targets(path, &contents, &[None]);

        let contents = source(
            path,
            "let selected = alpha;\neval(code);\nselected = omega;\nreturn selected();",
            "selected := alpha\ntouch(&selected)\nselected = omega\nreturn selected()",
            "let mut selected: fn() -> i32 = alpha;\ntouch(&mut selected);\nselected = omega;\nselected()",
        );
        assert_targets(path, &contents, &[None]);
    }
    let contents = source(
        "src/lib.rs",
        "",
        "",
        "let mut selected: fn() -> i32 = alpha;\nopaque!();\nselected = omega;\nselected()",
    );
    assert_targets("src/lib.rs", &contents, &[None]);
    let contents = source(
        "src/lib.rs",
        "",
        "",
        "let mut selected: fn() -> i32 = alpha;\n#[cfg(feature = \"optional\")]\nselected = omega;\nselected()",
    );
    assert_targets("src/lib.rs", &contents, &[None]);
    for path in ["values.js", "values.ts"] {
        let contents = source(
            path,
            "let selected = alpha;\nwith (context) { selected = omega; }\nreturn selected();",
            "",
            "",
        );
        assert_targets(path, &contents, &[None]);
        let contents = source(
            path,
            "let selected = alpha;\nsink(selected, alpha);\nreturn selected();",
            "",
            "",
        );
        let parsed = assert_targets(path, &contents, &[Some("alpha")]);
        let callbacks: Vec<_> = parsed
            .references
            .iter()
            .filter(|r| r.reason.starts_with("callback argument;"))
            .collect();
        assert_eq!(callbacks.len(), 2);
        assert!(callbacks.iter().all(|r| r.relation == "references"));
        assert!(
            callbacks
                .iter()
                .find(|r| r.label == "selected")
                .unwrap()
                .candidate_keys
                .is_empty()
        );
        assert!(
            !callbacks
                .iter()
                .find(|r| r.label == "alpha")
                .unwrap()
                .candidate_keys
                .is_empty()
        );
    }
}

#[test]
fn callable_values_rebind_and_retract_through_store_updates_and_removal() {
    for path in PATHS {
        let original = source(
            path,
            "let selected = alpha;\nselected = omega;\nreturn selected();",
            "selected := alpha\nselected = omega\nreturn selected()",
            "let mut selected: fn() -> i32 = alpha;\nselected = omega;\nselected()",
        );
        let no_target = original.replace(
            match path {
                "values.go" => "func omega() int { return 2 }",
                "src/lib.rs" => "fn omega() -> i32 { 2 }",
                _ => "function omega() { return 2; }",
            },
            "",
        );
        let directory = tempfile::tempdir().unwrap();
        let mut store = Store::create(&directory.path().join("graph.db")).unwrap();
        for (contents, target) in [
            (original.clone(), Some("omega")),
            (
                original.replace("selected = omega", "selected = alpha"),
                Some("alpha"),
            ),
            (
                original.replace("selected = omega", "selected = unknown"),
                None,
            ),
            (no_target, None),
            (original.clone(), Some("omega")),
        ] {
            let parsed = assert_targets(path, &contents, &[target]);
            let call_id = selected_calls(&parsed)[0].id.clone();
            store
                .apply_native("fixture", vec![parsed.clone()], vec![], Coverage::default())
                .unwrap();
            let graph = store.snapshot().unwrap();
            let edges: Vec<_> = graph
                .edges
                .iter()
                .filter(|e| e.metadata["reference_id"] == call_id)
                .collect();
            if let Some(target) = target {
                assert_eq!(edges.len(), 1);
                assert_eq!(edges[0].target, node(&parsed, target).id);
            } else {
                assert!(edges.is_empty());
            }
            let fresh_dir = tempfile::tempdir().unwrap();
            let mut fresh = Store::create(&fresh_dir.path().join("graph.db")).unwrap();
            fresh
                .apply_native("fixture", vec![parsed], vec![], Coverage::default())
                .unwrap();
            assert_eq!(graph_value(graph), graph_value(fresh.snapshot().unwrap()));
        }
        store
            .apply_native("fixture", vec![], vec![path.into()], Coverage::default())
            .unwrap();
        let graph = store.snapshot().unwrap();
        assert!(graph.nodes.is_empty());
        assert!(graph.edges.is_empty());
    }
}

#[test]
fn callable_values_keep_write_and_borrow_identity_before_later_shadows() {
    let cases = [
        (
            "selected := alpha\nchange := func() {\nselected = omega\nselected := alpha\n_ = selected\n}\nchange()\nreturn selected()",
            "let mut selected: fn() -> i32 = alpha;\nlet mut change = || {\nselected = omega;\nlet selected: fn() -> i32 = alpha;\nlet _ = selected;\n};\nchange();\nselected()",
            None,
        ),
        // Declaring the inner binding first keeps its later write local.
        (
            "selected := alpha\nchange := func() {\nselected := alpha\nselected = omega\n_ = selected\n}\nchange()\nreturn selected()",
            "let selected: fn() -> i32 = alpha;\nlet mut change = || {\nlet mut selected: fn() -> i32 = alpha;\nselected = omega;\nlet _ = selected;\n};\nchange();\nselected()",
            Some("alpha"),
        ),
        (
            "selected := alpha\n{\npointer := &selected\nselected := alpha\n*pointer = omega\n_ = selected\n}\nreturn selected()",
            "let mut selected: fn() -> i32 = alpha;\n{\nlet pointer = &mut selected;\nlet selected: fn() -> i32 = alpha;\n*pointer = omega;\nlet _ = selected;\n}\nselected()",
            None,
        ),
        // Taking the address after the shadow declaration escapes only that local.
        (
            "selected := alpha\n{\nselected := alpha\npointer := &selected\n*pointer = omega\n_ = selected\n}\nreturn selected()",
            "let selected: fn() -> i32 = alpha;\n{\nlet mut selected: fn() -> i32 = alpha;\nlet pointer = &mut selected;\n*pointer = omega;\nlet _ = selected;\n}\nselected()",
            Some("alpha"),
        ),
    ];
    for path in ["values.go", "src/lib.rs"] {
        for (go, rust, expected) in cases {
            let contents = source(path, "", go, rust);
            let parsed = assert_targets(path, &contents, &[expected]);
            assert_eq!(selected_calls(&parsed)[0].source, node(&parsed, "route").id);
            assert!(parsed.references.iter().any(|r| {
                r.source == node(&parsed, "control").id
                    && r.relation == "calls"
                    && r.candidate_keys
                        .iter()
                        .any(|key| node(&parsed, "omega").binding_key.as_ref() == Some(key))
            }));
        }
    }
}

#[test]
fn callable_values_bound_opaque_rust_escapes_to_capturable_locals() {
    let cases: &[(&str, &[Option<&str>])] = &[
        (
            "let mut selected: fn() -> i32 = alpha;\nlet mut change = || { replace!(selected); };\nchange();\nselected()",
            &[None],
        ),
        // An opaque write observed before a shadow still affects the outer value.
        (
            "let mut selected: fn() -> i32 = alpha;\nlet mut change = || {\nreplace!(selected);\nlet selected: fn() -> i32 = alpha;\nlet _ = selected;\n};\nchange();\nselected()",
            &[None],
        ),
        // An opaque expansion can refer to its definition-site outer binding.
        (
            "let mut selected: fn() -> i32 = alpha;\nmacro_rules! replace_outer { () => { selected = omega; }; }\nlet mut change = || {\nlet selected: fn() -> i32 = alpha;\nreplace_outer!();\nlet _ = selected;\n};\nchange();\nselected()",
            &[None],
        ),
        // An ordinary read-only closure does not invalidate the outer call.
        (
            "let selected: fn() -> i32 = alpha;\nlet later = || selected();\nlet _ = later();\nselected()",
            &[None, Some("alpha")],
        ),
        // A function item cannot capture locals from its surrounding function.
        (
            "let selected: fn() -> i32 = alpha;\nfn unrelated() {\nlet mut change = || {\nlet mut slot: fn() -> i32 = alpha;\nreplace!(slot);\n};\nchange();\n}\nunrelated();\nselected()",
            &[Some("alpha")],
        ),
    ];
    for (body, expected) in cases {
        let contents = format!(
            "macro_rules! replace {{ ($slot:ident) => {{ $slot = omega; }}; }}\n{}\nfn elsewhere() {{ let mut slot: fn() -> i32 = alpha; replace!(slot); }}\n",
            source("src/lib.rs", "", "", body)
        );
        let parsed = assert_targets("src/lib.rs", &contents, expected);
        let calls = selected_calls(&parsed);
        assert_eq!(calls.last().unwrap().source, node(&parsed, "route").id);
        if calls.len() == 2 {
            assert_ne!(calls[0].source, node(&parsed, "route").id);
        }
        assert!(parsed.references.iter().any(|r| {
            r.source == node(&parsed, "control").id
                && r.relation == "calls"
                && r.candidate_keys
                    .iter()
                    .any(|key| node(&parsed, "omega").binding_key.as_ref() == Some(key))
        }));
    }
}
