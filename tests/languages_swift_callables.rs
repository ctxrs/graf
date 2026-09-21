use graf::{
    languages::compiled::{CompiledContext, apply_swift_context, parse},
    model::{Coverage, Direction, FileFacts, Node, QueryOptions, Reference},
    store::Store,
};

fn facts(path: &str, source: &str) -> FileFacts {
    let facts = parse(path, source, source).unwrap().unwrap();
    assert!(
        facts.diagnostics.is_empty(),
        "{path}: {:?}",
        facts.diagnostics
    );
    facts
}

fn function<'a>(facts: &'a FileFacts, name: &str) -> &'a Node {
    let nodes: Vec<_> = facts
        .nodes
        .iter()
        .filter(|node| node.kind == "function" && node.label == name)
        .collect();
    assert_eq!(nodes.len(), 1, "{name}: {nodes:?}");
    nodes[0]
}

fn calls<'a>(facts: &'a FileFacts, label: &str) -> Vec<&'a Reference> {
    facts
        .references
        .iter()
        .filter(|r| r.relation == "calls" && r.label == label)
        .collect()
}

fn targets(facts: &FileFacts, label: &str, expected: &[Option<&str>]) {
    let sites = calls(facts, label);
    assert_eq!(sites.len(), expected.len(), "{label}: {sites:?}");
    for (site, expected) in sites.into_iter().zip(expected) {
        let keys: Vec<_> = expected
            .iter()
            .map(|name| function(facts, name).binding_key.clone().unwrap())
            .collect();
        assert_eq!(site.candidate_keys, keys, "{site:?}");
    }
}

const FUNCTIONS: &str = "func amber() -> Int { 11 }\nfunc violet() -> Int { 22 }\n";

#[test]
fn swift_callable_reassignment_snapshots_each_call_and_original_ranges() {
    let source = format!(
        "// π distinguishes bytes from characters.\n{FUNCTIONS}{}",
        r#"func route() -> Int {
    var selected = amber
    selected()
    selected = violet
    return selected()
}
"#
    );
    let file = facts("Route.swift", &source);
    targets(&file, "selected", &[Some("amber"), Some("violet")]);
    let owner = function(&file, "route");
    let sites = calls(&file, "selected");
    let proofs = owner.metadata["swift_callable_values"].as_array().unwrap();
    assert_eq!(proofs.len(), 2);
    let declaration = source.find("var selected = amber").unwrap();
    let assignment = source.find("selected = violet").unwrap();
    for (index, ((start, text), reference)) in
        source.match_indices("selected()").zip(sites).enumerate()
    {
        let ordinal = file
            .references
            .iter()
            .position(|r| r.id == reference.id)
            .unwrap();
        assert_eq!(
            reference.id,
            format!(
                "calls:{}:{start}-{}:{ordinal}",
                owner.id,
                start + text.len()
            )
        );
        assert_eq!(reference.source, owner.id);
        assert_eq!(reference.file, "Route.swift");
        assert_eq!(
            reference.line as usize,
            source[..start].bytes().filter(|b| *b == b'\n').count() + 1
        );
        let proof = &proofs[index];
        assert_eq!(proof["reference_id"], reference.id);
        assert_eq!(proof["declaration_start_byte"], declaration);
        assert_eq!(
            proof["declaration_end_byte"],
            declaration + "var selected = amber".len()
        );
        let (write, length, target) = if index == 0 {
            (declaration, "var selected = amber".len(), "amber")
        } else {
            (assignment, "selected = violet".len(), "violet")
        };
        assert_eq!(proof["assignment_start_byte"], write);
        assert_eq!(proof["assignment_end_byte"], write + length);
        assert_eq!(proof["target_id"], function(&file, target).id);
    }
}

#[test]
fn swift_callable_unknown_writes_clear_only_the_following_straight_line_value() {
    let source = format!(
        "{FUNCTIONS}{}",
        r#"func route(_ replacement: () -> Int) {
    var selected = amber
    selected()
    selected = replacement
    selected()
    selected = violet
    selected()
    let supplied = replacement
    supplied()
}
"#
    );
    let file = facts("Writes.swift", &source);
    targets(&file, "selected", &[Some("amber"), None, Some("violet")]);
    targets(&file, "supplied", &[None]);
    assert_eq!(
        function(&file, "route").metadata["swift_callable_values"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn swift_callable_sources_require_current_unique_named_function_identity() {
    for body in [
        // A same-spelled parameter or type is not the named function.
        "func route(_ amber: () -> Int) { let selected = amber; selected() }",
        "func route() { struct amber {}; let selected = amber; selected() }",
        "func route() { let selected = amber; struct selected {}; selected() }",
        "func route() { let selected = amber; func selected() {}; selected() }",
        "func route() { let selected = amber; selected = violet; selected() }",
        // A later overload or unknown write invalidates the captured identity.
        "func route() { let selected = amber; selected() }\nfunc amber(_ value: Int) -> Int { value }",
        "func route() { let selected = amber; selected() }\nfunc replace() { amber = violet }",
        "func route() { let selected = amber; selected() }\nfunc replace() { alter(&amber) }",
        // Aliases cannot obtain a target from a future declaration or a guess.
        "func route() { let selected = later; selected() }\nfunc later() -> Int { 33 }",
        "func route() { let selected = absent; selected() }",
        "func generic<Value>(_ value: Value) -> Value { value }\nfunc route() { let selected = generic; selected(11) }",
    ] {
        let file = facts("Identity.swift", &format!("{FUNCTIONS}{body}\n"));
        targets(&file, "selected", &[None]);
    }
    let source = format!(
        "func replace() {{ amber = violet }}\n{FUNCTIONS}func route() {{ let selected = amber; selected() }}\n"
    );
    targets(&facts("EarlierWrite.swift", &source), "selected", &[None]);
    let file = facts(
        "Local.swift",
        r#"func route() {
    func nearby() -> Int { 33 }
    let selected = nearby
    selected()
}
"#,
    );
    targets(&file, "selected", &[Some("nearby")]);
}

#[test]
fn swift_callable_conditional_loop_and_composite_writes_block_proof() {
    for write in [
        "if flag { selected = violet }",
        "while flag { selected = violet; break }",
        "for item in [1, 2] { selected = violet }",
        "do { selected = violet }",
        "selected += replacement",
        "(selected, other) = (violet, amber)",
    ] {
        let source = format!(
            "{FUNCTIONS}func route(_ flag: Bool, _ replacement: () -> Int) {{\n    var other = amber\n    var selected = amber\n    selected()\n    {write}\n    selected()\n}}\n"
        );
        let file = facts("Control.swift", &source);
        targets(&file, "selected", &[None, None]);
    }
    let source = format!(
        "{FUNCTIONS}{}",
        r#"func route(_ flag: Bool) {
    var selected = amber
    if flag { selected() }
    while flag { selected(); break }
    selected = violet
    selected()
}
"#
    );
    let file = facts("ReadOnlyControl.swift", &source);
    targets(&file, "selected", &[None, None, Some("violet")]);
}

#[test]
fn swift_callable_captured_reads_writes_and_inout_do_not_share_a_call_timeline() {
    for escape in [
        "let delayed = { selected() }",
        "let delayed = { selected }",
        "let delayed = { selected = violet }",
        "func delayed() -> Int { return selected() }",
        "alter(&selected)",
    ] {
        let source = format!(
            "{FUNCTIONS}func route() {{\n    var selected = amber\n    selected()\n    {escape}\n    selected = violet\n    selected()\n}}\n"
        );
        let file = facts("Capture.swift", &source);
        let sites = calls(&file, "selected");
        assert_eq!(
            sites.len(),
            if escape.contains("selected()") { 3 } else { 2 }
        );
        assert!(
            sites.iter().all(|r| r.candidate_keys.is_empty()),
            "{escape}: {sites:?}"
        );
        assert!(
            function(&file, "route")
                .metadata
                .get("swift_callable_values")
                .is_none()
        );
    }
    let source = format!(
        "{FUNCTIONS}{}",
        r#"func route() {
    let delayed = { amber() }
    var selected = amber
    selected = violet
    selected()
}
"#
    );
    targets(
        &facts("SeparateCapture.swift", &source),
        "selected",
        &[Some("violet")],
    );
    let source = format!(
        "{FUNCTIONS}{}",
        r#"func route() {
    func delayed() { selected = violet }
    var selected = amber
    selected()
}
"#
    );
    targets(&facts("LaterCapture.swift", &source), "selected", &[None]);
}

#[test]
fn swift_callable_shadowed_parameters_blocks_and_loops_keep_separate_bindings() {
    let source = format!(
        "{FUNCTIONS}{}",
        r#"func route(_ input: () -> Int) {
    var selected = amber
    do {
        let selected = input
        selected()
    }
    for selected in [input] { selected() }
    func delayed(_ selected: () -> Int) { selected() }
    selected = violet
    selected()
}
"#
    );
    let file = facts("Shadows.swift", &source);
    targets(&file, "selected", &[None, None, None, Some("violet")]);
    let sites = calls(&file, "selected");
    assert_eq!(sites[2].source, function(&file, "delayed").id);
    assert_eq!(sites[3].source, function(&file, "route").id);
}

#[test]
fn swift_callable_values_do_not_follow_aliases_factories_members_or_types() {
    let source = format!(
        "{FUNCTIONS}{}",
        r#"func produce() -> () -> Int { return amber }
struct Device { static func perform() -> Int { return 33 } }
func route() {
    let original = amber
    original()
    let selected = original
    selected()
    let produced = produce()
    produced()
    let member = Device.perform
    member()
    let constructor = Device
    constructor()
    let indexed = [amber]
    indexed[0]()
}
"#
    );
    let file = facts("Boundaries.swift", &source);
    targets(&file, "original", &[Some("amber")]);
    for label in [
        "selected",
        "produced",
        "member",
        "constructor",
        "indexed",
        "indexed[0]",
    ] {
        targets(&file, label, &[None]);
    }
    targets(&file, "produce", &[Some("produce")]);
}

fn contextual(path: &str, source: &str, module: &str) -> FileFacts {
    let mut file = facts(path, source);
    apply_swift_context(&mut file, module, &[("Palette".into(), "palette".into())]);
    file
}

fn outgoing(store: &Store, owner: &str) -> graf::model::GraphResult {
    store
        .neighbors(
            owner,
            &QueryOptions {
                direction: Direction::Outgoing,
                relation: Some("calls".into()),
                ..QueryOptions::default()
            },
        )
        .unwrap()
}

#[test]
fn swift_callable_context_remaps_exact_local_identity_without_import_fallback() {
    let source = format!(
        "import Palette\n{FUNCTIONS}func route() {{ let selected = violet; selected() }}\n"
    );
    let mut local = contextual("Route.swift", &source, "scene");
    targets(&local, "selected", &[Some("violet")]);
    assert_eq!(
        calls(&local, "selected")[0].candidate_keys,
        ["swift:symbol:module:5:scene:violet"]
    );
    let mut imported = contextual(
        "Palette.swift",
        "public func violet() -> Int { 99 }\n",
        "palette",
    );
    let mut external = contextual(
        "External.swift",
        "import Palette\nfunc external() { let selected = violet; selected() }\n",
        "scene",
    );
    targets(&external, "selected", &[None]);
    let context = CompiledContext::new(
        &[local.clone(), imported.clone(), external.clone()],
        &Default::default(),
    );
    for file in [&mut local, &mut imported, &mut external] {
        context.apply(file);
    }
    targets(&local, "selected", &[Some("violet")]);
    targets(&external, "selected", &[None]);
    let owner = function(&local, "route").id.clone();
    let target = function(&local, "violet").id.clone();
    let reference = calls(&local, "selected")[0].clone();
    let directory = tempfile::tempdir().unwrap();
    let mut store = Store::create(&directory.path().join("graph.db")).unwrap();
    store
        .apply_native(
            "fixture",
            vec![local, imported, external],
            vec![],
            Coverage::default(),
        )
        .unwrap();
    let graph = outgoing(&store, &owner);
    assert_eq!(graph.edges.len(), 1);
    let edge = &graph.edges[0];
    assert_eq!(edge.target, target);
    assert_eq!(edge.source, owner);
    assert_eq!(edge.metadata["reference_id"], reference.id);
    assert_eq!(edge.file.as_deref(), Some("Route.swift"));
    assert_eq!(edge.line, Some(reference.line));
    assert_eq!(edge.confidence, "statically_resolved");

    // An ambiguous same-module binding must not select the unique import.
    let duplicate = contextual(
        "Duplicate.swift",
        "func violet(_ value: Int) -> Int { value }\n",
        "scene",
    );
    store
        .apply_native("fixture", vec![duplicate], vec![], Coverage::default())
        .unwrap();
    let graph = outgoing(&store, &owner);
    assert!(graph.edges.is_empty());
    assert_eq!(graph.unresolved.len(), 1);
    assert_eq!(graph.unresolved[0].label, "selected");
    store
        .apply_native(
            "fixture",
            vec![],
            vec!["Duplicate.swift".into()],
            Coverage::default(),
        )
        .unwrap();
    assert_eq!(outgoing(&store, &owner).edges[0].target, target);
}

#[test]
fn swift_callable_store_replacement_clears_stale_targets_then_removes_call_sites() {
    let directory = tempfile::tempdir().unwrap();
    let mut store = Store::create(&directory.path().join("graph.db")).unwrap();
    for (value, expected) in [
        ("violet", Some("violet")),
        ("amber", Some("amber")),
        ("replacement", None),
    ] {
        let source = format!(
            "{FUNCTIONS}func route(_ replacement: () -> Int) -> Int {{\n    var selected = amber\n    selected = {value}\n    return selected()\n}}\n"
        );
        let file = contextual("Route.swift", &source, "scene");
        targets(&file, "selected", &[expected]);
        let owner = function(&file, "route").id.clone();
        let target = expected.map(|name| function(&file, name).id.clone());
        let reference = calls(&file, "selected")[0].clone();
        store
            .apply_native("fixture", vec![file], vec![], Coverage::default())
            .unwrap();
        let graph = outgoing(&store, &owner);
        if let Some(target) = target {
            assert_eq!(graph.edges.len(), 1);
            assert_eq!(graph.edges[0].target, target);
            assert_eq!(graph.edges[0].metadata["reference_id"], reference.id);
            assert!(graph.unresolved.is_empty());
        } else {
            assert!(graph.edges.is_empty());
            assert_eq!(graph.unresolved.len(), 1);
            assert_eq!(graph.unresolved[0].source, owner);
            assert_eq!(graph.unresolved[0].label, "selected");
            assert_eq!(graph.unresolved[0].file, reference.file);
            assert_eq!(graph.unresolved[0].line, reference.line);
        }
    }
    store
        .apply_native(
            "fixture",
            vec![],
            vec!["Route.swift".into()],
            Coverage::default(),
        )
        .unwrap();
    let snapshot = store.snapshot().unwrap();
    assert!(snapshot.nodes.is_empty());
    assert!(snapshot.edges.is_empty());
}

#[test]
fn swift_callable_compiler_directives_decline_the_affected_body_only() {
    for conditional in [
        "#if USE_VIOLET\n    selected = violet\n#endif",
        "#if USE_VIOLET\n    selected = violet\n#else\n    selected = amber\n#endif",
        "#if FIRST\n    selected = amber\n#elseif SECOND\n    selected = violet\n#else\n    selected = amber\n#endif",
        "#if OUTER\n#if INNER\n    selected = violet\n#endif\n#endif",
    ] {
        let source = format!(
            "{FUNCTIONS}func route() -> Int {{\n    var selected = amber\n    selected()\n{conditional}\n    return selected()\n}}\nfunc plain() -> Int {{\n    var untouched = amber\n    untouched = violet\n    return untouched()\n}}\n"
        );
        let file = facts("ConditionalBody.swift", &source);
        targets(&file, "selected", &[None, None]);
        targets(&file, "untouched", &[Some("violet")]);
        assert!(
            function(&file, "route")
                .metadata
                .get("swift_callable_values")
                .is_none()
        );
    }
    // Removing the compiler directives restores the ordinary reassignment.
    // Directive-like text in a comment or string is not a compiler condition.
    let source = format!(
        "{FUNCTIONS}{}",
        r##"func route() -> Int {
    let description = "#if USE_VIOLET"
    // #else
    var selected = amber
    selected = violet
    return selected()
}
"##
    );
    targets(
        &facts("UnconditionalBody.swift", &source),
        "selected",
        &[Some("violet")],
    );
}

#[test]
fn swift_callable_conditional_source_functions_and_callers_do_not_supply_proof() {
    for declaration in [
        "#if OPTIONAL\nfunc seasonal() -> Int { 33 }\n#endif",
        "#if OUTER\n#if INNER\nfunc seasonal() -> Int { 33 }\n#endif\n#else\nfunc other() -> Int { 44 }\n#endif",
        "#if FIRST\nfunc other() -> Int { 44 }\n#elseif SECOND\nfunc seasonal() -> Int { 33 }\n#endif",
    ] {
        let source = format!(
            "{FUNCTIONS}{declaration}\nfunc route() {{\n    let selected = seasonal\n    selected()\n}}\n#if CALLER\nfunc guarded() {{\n    let guardedValue = violet\n    guardedValue()\n}}\n#endif\nfunc plain() {{\n    let untouched = violet\n    untouched()\n}}\n"
        );
        let file = facts("ConditionalSource.swift", &source);
        // Declaration extraction remains separate from callable-value certainty.
        assert_eq!(function(&file, "seasonal").file, "ConditionalSource.swift");
        targets(&file, "selected", &[None]);
        targets(&file, "guardedValue", &[None]);
        targets(&file, "untouched", &[Some("violet")]);
    }
    let source = format!(
        "{FUNCTIONS}func seasonal() -> Int {{ 33 }}\nfunc route() {{ let selected = seasonal; selected() }}\n"
    );
    targets(
        &facts("UnconditionalSource.swift", &source),
        "selected",
        &[Some("seasonal")],
    );
}

#[test]
fn swift_callable_member_suffixes_do_not_capture_same_named_locals() {
    for expression in ["box.selected()", "box.selected"] {
        for before_declaration in [false, true] {
            let closure = format!("    let unrelated = {{ {expression} }}\n");
            let declaration = "    var selected = amber\n";
            let setup = if before_declaration {
                format!("{closure}{declaration}")
            } else {
                format!("{declaration}{closure}")
            };
            let source = format!(
                "struct Box {{ let selected: () -> Int }}\n{FUNCTIONS}func route(_ box: Box) {{\n{setup}    selected = violet\n    selected()\n}}\n"
            );
            let file = facts("MemberRead.swift", &source);
            targets(&file, "selected", &[Some("violet")]);
            let owner = function(&file, "route");
            let proofs = owner.metadata["swift_callable_values"].as_array().unwrap();
            assert_eq!(proofs.len(), 1);
            assert_eq!(proofs[0]["reference_id"], calls(&file, "selected")[0].id);
            assert_eq!(proofs[0]["target_id"], function(&file, "violet").id);
        }
    }
}

#[test]
fn swift_callable_navigation_receivers_and_bare_captures_still_block_proof() {
    for expression in [
        "selected()",
        "selected",
        "selected.self",
        "alter(&selected)",
    ] {
        let source = format!(
            "{FUNCTIONS}func route() {{\n    var selected = amber\n    let delayed = {{ {expression} }}\n    selected = violet\n    selected()\n}}\n"
        );
        let file = facts("LexicalRead.swift", &source);
        let expected = if expression == "selected()" {
            vec![None, None]
        } else {
            vec![None]
        };
        targets(&file, "selected", &expected);
        assert!(
            function(&file, "route")
                .metadata
                .get("swift_callable_values")
                .is_none()
        );
    }
}
