use graf::{
    languages::compiled::{parse, supports},
    model::{Coverage, FileFacts, Node, Reference},
    store::Store,
};

fn facts(path: &str, text: &str) -> FileFacts {
    let f = parse(path, text, "hash").unwrap().unwrap();
    assert!(f.diagnostics.is_empty(), "{path}: {:?}", f.diagnostics);
    f
}
fn node<'a>(f: &'a FileFacts, name: &str) -> &'a Node {
    f.nodes
        .iter()
        .find(|n| n.label == name && n.kind != "module")
        .unwrap_or_else(|| panic!("missing {name}: {:?}", f.nodes))
}
fn calls<'a>(f: &'a FileFacts, name: &str) -> Vec<&'a Reference> {
    f.references
        .iter()
        .filter(|r| r.relation == "calls" && r.label == name)
        .collect()
}
fn key(n: &Node) -> &str {
    n.binding_key.as_deref().unwrap()
}

#[test]
fn extensions_paths_unicode_and_malformed_sources() {
    for ext in [
        "c", "h", "cc", "cpp", "cxx", "C", "hpp", "hh", "hxx", "H", "java", "cs", "kt", "kts",
        "swift", "cu", "cuh", "metal",
    ] {
        assert!(supports(&format!("src/a.{ext}")));
    }
    for path in ["a.m", "a.mm", "a.csx", "a.kts.txt", "c", "a.dart", "a.h.in"] {
        assert!(!supports(path));
    }
    assert!(parse("a.txt", "", "hash").unwrap().is_none());
    assert!(parse("../a.c", "", "hash").is_err());
    for (path, text) in [
        ("a.c", "int {"),
        ("a.cpp", "class {"),
        ("a.java", "class {"),
        ("a.cs", "class {"),
        ("a.kt", "fun {"),
        ("a.swift", "func {"),
    ] {
        let f = parse(path, text, "hash").unwrap().unwrap();
        assert!(f.nodes.is_empty(), "{path}: {:?}", f.nodes);
        assert!(!f.diagnostics.is_empty(), "{path}");
    }
    for (path, text, name) in [
        ("a.c", "// π\nint café(void) {\n return 1;\n}\n", "café"),
        ("a.cpp", "// π\nint café() { return 1; }", "café"),
        ("a.java", "// π\nclass Café { void café() {} }", "Café"),
        ("a.cs", "// π\nclass Café { void café() {} }", "Café"),
        ("a.kt", "// π\nfun café() {}", "café"),
        ("a.swift", "// π\nfunc café() {}", "café"),
    ] {
        let f = facts(path, text);
        let n = node(&f, name);
        assert_eq!(n.line, Some(2));
        let start = n.metadata["start_byte"].as_u64().unwrap() as usize;
        let end = n.metadata["end_byte"].as_u64().unwrap() as usize;
        assert!(text[start..end].contains(name));
        assert_eq!(n.id, node(&facts(path, text), name).id);
        assert_ne!(n.id, node(&facts(&format!("other/{path}"), text), name).id);
    }
}

#[test]
fn c_functions_structs_includes_and_pointer_shadowing() {
    let f = facts(
        "src/main.c",
        "#include \"types.h\"\n#include <stdio.h>\nstruct Point { int x; };\nstatic int work(void) { return 1; }\nint run(void) { return work(); }\nint dynamic(int (*work)(void)) { return work(); }\n",
    );
    assert_eq!(node(&f, "Point").kind, "struct");
    assert_eq!(calls(&f, "work")[0].candidate_keys, [key(node(&f, "work"))]);
    assert!(calls(&f, "work")[1].candidate_keys.is_empty());
    assert_eq!(calls(&f, "work")[0].source, node(&f, "run").id);
    assert!(
        f.references
            .iter()
            .any(|r| r.relation == "includes" && r.candidate_keys == ["c:file:src/types.h"])
    );
    assert!(
        f.references
            .iter()
            .any(|r| r.label == "<stdio.h>" && r.candidate_keys.is_empty())
    );
    let other = facts("src/other.c", "static int work(void) { return 2; }");
    assert_ne!(key(node(&f, "work")), key(node(&other, "work")));
}

#[test]
fn cpp_namespace_alias_inheritance_static_and_typed_members() {
    let f = facts(
        "src/tools.cpp",
        "namespace api { struct Base {}; struct Tool : Base { static void ping() {} void run() {} virtual void dynamic() {} }; void work() {} } namespace alias = api; void call(api::Tool tool) { alias::work(); api::Tool::ping(); tool.run(); tool.dynamic(); unknown.run(); }",
    );
    assert_eq!(key(node(&f, "Tool")), "cpp:symbol:api.Tool");
    assert!(
        f.references
            .iter()
            .any(|r| r.relation == "inherits" && r.candidate_keys == [key(node(&f, "Base"))])
    );
    assert_eq!(
        calls(&f, "alias::work")[0].candidate_keys,
        [key(node(&f, "work"))]
    );
    assert_eq!(
        calls(&f, "api::Tool::ping")[0].candidate_keys,
        [key(node(&f, "ping"))]
    );
    assert_eq!(
        calls(&f, "tool.run")[0].candidate_keys,
        ["cpp:member:api.Tool.run"]
    );
    assert!(node(&f, "dynamic").binding_key.is_none());
    assert!(calls(&f, "unknown.run")[0].candidate_keys.is_empty());
}

#[test]
fn java_packages_imports_classes_and_final_receiver_calls() {
    let f = facts(
        "src/Runner.java",
        "package demo; import api.Tool; import static api.Actions.work; class Base {} class Runner extends Base { void run(Tool tool) { Tool.ping(); tool.run(); work(); } void shadow(Object Tool) { Tool.ping(); } }",
    );
    assert_eq!(key(node(&f, "Runner")), "java:symbol:demo.Runner");
    assert!(
        f.references
            .iter()
            .any(|r| r.relation == "inherits" && r.candidate_keys == [key(node(&f, "Base"))])
    );
    assert_eq!(
        calls(&f, "Tool.ping")[0].candidate_keys,
        ["java:static:api.Tool.ping"]
    );
    assert_ne!(
        calls(&f, "Tool.ping")[1].candidate_keys,
        calls(&f, "Tool.ping")[0].candidate_keys
    );
    assert_eq!(
        calls(&f, "tool.run")[0].candidate_keys,
        ["java:member:api.Tool.run"]
    );
    assert_eq!(
        calls(&f, "work")[0].candidate_keys,
        ["java:symbol:api.Actions.work"]
    );
    assert_eq!(calls(&f, "tool.run")[0].source, node(&f, "run").id);
}

#[test]
fn csharp_namespace_alias_and_nonvirtual_members() {
    let f = facts(
        "Runner.cs",
        "using T = api.Tool; namespace demo; class Base {} class Runner : Base { void Run(T tool) { T.Ping(); tool.Work(); dynamic unknown = tool; unknown.Work(); } }",
    );
    assert_eq!(key(node(&f, "Runner")), "csharp:symbol:demo.Runner");
    assert!(
        f.references
            .iter()
            .any(|r| r.relation == "inherits" && r.candidate_keys == [key(node(&f, "Base"))])
    );
    assert_eq!(
        calls(&f, "T.Ping")[0].candidate_keys,
        ["csharp:static:api.Tool.Ping"]
    );
    assert_eq!(
        calls(&f, "tool.Work")[0].candidate_keys,
        ["csharp:member:api.Tool.Work"]
    );
    assert!(calls(&f, "unknown.Work")[0].candidate_keys.is_empty());
}

#[test]
fn kotlin_packages_aliases_inheritance_and_typed_calls() {
    let f = facts(
        "Runner.kt",
        "package demo\nimport api.work as go\nimport api.Tool as T\nopen class Base\nclass Runner : Base() { fun run(tool: T) { go(); tool.work() } }\nfun shadow(go: () -> Unit) { go() }\n",
    );
    assert_eq!(key(node(&f, "Runner")), "kotlin:symbol:demo.Runner");
    assert!(
        f.references
            .iter()
            .any(|r| r.relation == "inherits" && r.candidate_keys == [key(node(&f, "Base"))])
    );
    assert_eq!(
        calls(&f, "go")[0].candidate_keys,
        ["kotlin:symbol:api.work"]
    );
    assert!(calls(&f, "go")[1].candidate_keys.is_empty());
    assert_eq!(
        calls(&f, "tool.work")[0].candidate_keys,
        ["kotlin:member:api.Tool.work"]
    );
}

#[test]
fn swift_imports_protocols_structs_methods_and_local_calls() {
    let f = facts(
        "Sources/Runner.swift",
        "import Foundation\nprotocol Base {}\nstruct Tool: Base { static func ping() {}\nfunc work() {} }\nfunc helper() {}\nfunc run(tool: Tool) { helper(); Tool.ping(); tool.work() }\nfunc shadow(helper: () -> Void) { helper() }\n",
    );
    assert_eq!(node(&f, "Base").kind, "interface");
    assert_eq!(node(&f, "Tool").kind, "struct");
    assert!(
        f.references
            .iter()
            .any(|r| r.relation == "implements" && r.candidate_keys == [key(node(&f, "Base"))])
    );
    assert_eq!(
        calls(&f, "helper")[0].candidate_keys,
        [key(node(&f, "helper"))]
    );
    assert!(calls(&f, "helper")[1].candidate_keys.is_empty());
    assert_eq!(
        calls(&f, "Tool.ping")[0].candidate_keys,
        ["swift:static:@Sources/Runner.swift.Tool.ping"]
    );
    assert_eq!(
        calls(&f, "tool.work")[0].candidate_keys,
        ["swift:member:@Sources/Runner.swift.Tool.work"]
    );
    assert!(
        f.references
            .iter()
            .any(|r| r.relation == "imports" && r.label == "Foundation")
    );
}

#[test]
fn store_resolves_explicit_namespaces_and_updates_existing_callers() {
    for (a, b, source, caller, target) in [
        (
            "api.cpp",
            "run.cpp",
            "namespace api { void work() {} }",
            "void run() { api::work(); }",
            "work",
        ),
        (
            "Tool.java",
            "Run.java",
            "package api; public class Tool { public static void work() {} }",
            "package demo; import api.Tool; class Run { void run() { Tool.work(); } }",
            "work",
        ),
        (
            "Tool.cs",
            "Run.cs",
            "namespace api { public class Tool { public static void Work() {} } }",
            "using T = api.Tool; class Run { void Go() { T.Work(); } }",
            "Work",
        ),
        (
            "api.kt",
            "run.kt",
            "package api\nfun work() {}",
            "package demo\nimport api.work as go\nfun run() { go() }",
            "work",
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::create(&dir.path().join("graph.db")).unwrap();
        let definition = facts(a, source);
        let target_id = node(&definition, target).id.clone();
        store
            .apply_native(
                "fixture",
                vec![definition, facts(b, caller)],
                vec![],
                Coverage::default(),
            )
            .unwrap();
        assert!(
            store
                .snapshot()
                .unwrap()
                .edges
                .iter()
                .any(|e| e.relation == "calls" && e.target == target_id),
            "{a}"
        );
        store
            .apply_native("fixture", vec![facts(a, "")], vec![], Coverage::default())
            .unwrap();
        assert!(
            !store
                .snapshot()
                .unwrap()
                .edges
                .iter()
                .any(|e| e.relation == "calls"),
            "{a}"
        );
        store
            .apply_native(
                "fixture",
                vec![facts(a, source)],
                vec![],
                Coverage::default(),
            )
            .unwrap();
        assert!(
            store
                .snapshot()
                .unwrap()
                .edges
                .iter()
                .any(|e| e.relation == "calls" && e.target == target_id),
            "{a}"
        );
    }
}

#[test]
fn store_does_not_resolve_dynamic_overloaded_or_unrelated_units() {
    for (a, b, definition, caller) in [
        (
            "a.c",
            "b.c",
            "static void work(void) {}",
            "void run(void) { work(); }",
        ),
        (
            "a.swift",
            "b.swift",
            "func work() {}",
            "func run() { work() }",
        ),
        (
            "Tool.java",
            "Run.java",
            "package api; class Tool { void work() {} }",
            "package demo; import api.Tool; class Run { void run(Tool tool) { tool.work(); } }",
        ),
        (
            "Tool.cs",
            "Run.cs",
            "namespace api { class Tool { public virtual void Work() {} } }",
            "using T = api.Tool; class Run { void Go(T tool) { tool.Work(); } }",
        ),
        (
            "a.kt",
            "b.kt",
            "package api\nfun work(x: Int) {}\nfun work(x: String) {}",
            "package demo\nimport api.work\nfun run() { work(1) }",
        ),
        (
            "a.cpp",
            "b.cpp",
            "namespace api { void work(int x) {} void work(double x) {} }",
            "void run() { api::work(1); }",
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::create(&dir.path().join("graph.db")).unwrap();
        store
            .apply_native(
                "fixture",
                vec![facts(a, definition), facts(b, caller)],
                vec![],
                Coverage::default(),
            )
            .unwrap();
        assert!(
            !store
                .snapshot()
                .unwrap()
                .edges
                .iter()
                .any(|e| e.relation == "calls"),
            "{a}"
        );
    }
}

#[test]
fn scoped_variables_fields_and_complex_receivers_do_not_choose_bare_names() {
    let java = facts(
        "Runner.java",
        "package demo; import api.Tool; class Runner { Tool service; void run(Object service) { service.work(); this.service.work(); { Object Tool = service; Tool.ping(); } Tool.ping(); for (Object Tool : values) { Tool.ping(); } factory().work(); } }",
    );
    assert_eq!(
        calls(&java, "this.service.work")[0].candidate_keys,
        ["java:member:api.Tool.work"]
    );
    let ping = calls(&java, "Tool.ping");
    assert!(
        !ping[0]
            .candidate_keys
            .iter()
            .any(|k| k == "java:static:api.Tool.ping")
    );
    assert_eq!(ping[1].candidate_keys, ["java:static:api.Tool.ping"]);
    assert!(
        !ping[2]
            .candidate_keys
            .iter()
            .any(|k| k == "java:static:api.Tool.ping")
    );
    assert!(calls(&java, "factory().work")[0].candidate_keys.is_empty());
    let cpp = facts(
        "tool.cpp",
        "namespace api { struct Tool { void work() {} }; } void call(api::Tool *tool) { tool->work(); }",
    );
    assert_eq!(
        calls(&cpp, "tool->work")[0].candidate_keys,
        ["cpp:member:api.Tool.work"]
    );
}

#[test]
fn file_import_edges_and_swift_context_metadata_are_retained() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::create(&dir.path().join("graph.db")).unwrap();
    let header = facts("include/types.h", "struct Point { int x; };\n");
    let header_id = header.nodes[0].id.clone();
    store
        .apply_native(
            "fixture",
            vec![
                header,
                facts(
                    "src/main.c",
                    "#include \"../include/types.h\"\nint run(void) { return 0; }\n",
                ),
            ],
            vec![],
            Coverage::default(),
        )
        .unwrap();
    assert!(
        store
            .snapshot()
            .unwrap()
            .edges
            .iter()
            .any(|e| e.relation == "includes" && e.target == header_id)
    );
    store
        .apply_native(
            "fixture",
            vec![],
            vec!["include/types.h".into()],
            Coverage::default(),
        )
        .unwrap();
    assert!(
        !store
            .snapshot()
            .unwrap()
            .edges
            .iter()
            .any(|e| e.relation == "includes")
    );
    let swift = facts(
        "Sources/Tool.swift",
        "import Foundation\nstruct Tool { func work() {} }\n",
    );
    assert_eq!(swift.nodes[0].metadata["module_context_required"], true);
    assert_eq!(swift.nodes[0].metadata["unit"], "Sources/Tool");
    assert_eq!(
        swift.nodes[0].metadata["imports"][0]["syntax"],
        "import Foundation"
    );
    assert_eq!(
        node(&swift, "work").metadata["qualified_symbol"],
        "@Sources/Tool.swift.Tool.work"
    );
}

#[test]
fn interfaces_and_conditional_definitions_do_not_publish_callable_targets() {
    for (path, source) in [
        ("a.kt", "package api\ninterface Tool {\n    fun work()\n}\n"),
        ("a.cs", "namespace api { interface Tool { void Work(); } }"),
        ("a.java", "package api; interface Tool { void work(); }"),
        ("a.swift", "protocol Tool { func work() }\n"),
    ] {
        let f = facts(path, source);
        assert!(
            f.nodes.iter().any(|n| n.kind == "method"),
            "missing interface method: {path}"
        );
        assert!(
            f.nodes
                .iter()
                .filter(|n| n.kind == "method")
                .all(|n| n.binding_key.is_none()),
            "{path}"
        );
    }
    let f = facts(
        "a.cpp",
        "#ifdef CHOICE\nvoid work() {}\n#else\nvoid work() {}\n#endif\nvoid run() { work(); }\n",
    );
    assert!(
        f.nodes
            .iter()
            .filter(|n| n.label == "work")
            .all(|n| n.binding_key.is_none())
    );
}

#[test]
fn kotlin_compact_abstract_member_preserves_ordinary_source_positions() {
    let compact = facts(
        "Tool.kt",
        "// π\r\ninterface Tool { fun work() }\r\nfun neighbor() {}\r\n",
    );
    assert_eq!(node(&compact, "work").line, Some(2));
    assert_eq!(node(&compact, "neighbor").line, Some(3));
    assert!(node(&compact, "work").binding_key.is_none());
    let malformed = parse("Tool.kt", "interface Tool { fun broken( }\n", "hash")
        .unwrap()
        .unwrap();
    assert!(!malformed.diagnostics.is_empty());
    assert!(malformed.nodes.is_empty());
    let ordinary = facts("Tool.kt", "interface Tool {\n    fun work()\n}\n");
    assert_eq!(node(&ordinary, "Tool").kind, "interface");
    assert_eq!(node(&ordinary, "work").kind, "method");
}

#[test]
fn cuda_and_metal_files_use_cpp_extraction() {
    let header = facts(
        "gpu/helpers.cuh",
        "#pragma once\nnamespace gpu { inline float twice(float value) { return value * 2.0f; } }\n",
    );
    let cuda = facts(
        "gpu/launch.cu",
        "#include \"helpers.cuh\"\nnamespace gpu { float host(float value) { return twice(value); } }\n",
    );
    let metal = facts(
        "gpu/helpers.metal",
        "#include <metal_stdlib>\nusing namespace metal;\nfloat twice(float value) { return value * 2.0f; }\nfloat shade(float value) { return twice(value); }\n",
    );
    for f in [&header, &cuda, &metal] {
        assert_eq!(f.nodes[0].metadata["language"], "cpp");
        assert!(graf::languages::supports(&f.path));
        assert!(
            graf::languages::parse(&f.path, "", "hash")
                .unwrap()
                .is_some()
        );
    }
    assert_eq!(
        calls(&cuda, "twice")[0].candidate_keys,
        [
            key(node(&header, "twice")),
            "c-cpp:header-symbol:gpu/helpers.cuh:twice",
        ]
    );
    assert_eq!(calls(&cuda, "twice")[0].source, node(&cuda, "host").id);
    assert!(
        cuda.references
            .iter()
            .any(|r| r.relation == "includes" && r.candidate_keys == [key(&header.nodes[0])])
    );
    assert_eq!(
        calls(&metal, "twice")[0].candidate_keys,
        [key(node(&metal, "twice"))]
    );
    assert_eq!(calls(&metal, "twice")[0].source, node(&metal, "shade").id);
    let target = node(&header, "twice").id.clone();
    let directory = tempfile::tempdir().unwrap();
    let mut store = Store::create(&directory.path().join("graph.db")).unwrap();
    store
        .apply_native("fixture", vec![header, cuda], vec![], Coverage::default())
        .unwrap();
    assert!(
        store
            .snapshot()
            .unwrap()
            .edges
            .iter()
            .any(|e| e.relation == "calls" && e.target == target)
    );
}

#[test]
fn unsupported_gpu_syntax_reports_without_partial_facts() {
    // Unsupported or malformed syntax remains diagnostic after dialect handling.
    for (path, source) in [
        ("gpu/launch.cu", "void launch() { kernel<<<1, >>>(); }\n"),
        (
            "gpu/kernel.metal",
            "kernel void fill(threadgroup_imageblock float *output) { output[0] = 1.0f; }\n",
        ),
    ] {
        let f = parse(path, source, "hash").unwrap().unwrap();
        assert!(
            !f.diagnostics.is_empty(),
            "expected unsupported GPU grammar: {path}"
        );
        assert!(f.nodes.is_empty(), "no partial facts: {path}");
        assert!(f.edges.is_empty());
        assert!(f.references.is_empty());
    }
}

#[test]
fn declared_field_parameter_and_return_types_keep_context_and_ranges() {
    for (path, text, owner, field_kind) in [
        (
            "types.c",
            "struct Payload { int value; }; struct Holder { struct Payload item; }; struct Payload echo(struct Payload value) { return value; }",
            "Holder",
            "field",
        ),
        (
            "types.cpp",
            "struct Payload {}; struct Holder { Payload item; Payload echo(Payload value) { return value; } };",
            "Holder",
            "field",
        ),
        (
            "Types.java",
            "package api; class Payload {} class Holder { Payload item; Payload echo(Payload value) { return value; } }",
            "Holder",
            "field",
        ),
        (
            "Types.cs",
            "namespace api; class Payload {} class Holder { Payload item; Payload echo(Payload value) { return value; } }",
            "Holder",
            "field",
        ),
        (
            "types.kt",
            "package api\nclass Payload\nclass Holder {\n lateinit var item: Payload\n fun echo(value: Payload): Payload { return value }\n}\n",
            "Holder",
            "property",
        ),
        (
            "types.swift",
            "struct Payload {}\nstruct Holder {\n var item: Payload\n func echo(value: Payload) -> Payload { return value }\n}\n",
            "Holder",
            "property",
        ),
    ] {
        let f = facts(path, text);
        assert_eq!(node(&f, "item").kind, field_kind, "{path}");
        for (name, context) in [
            (owner, "field"),
            ("echo", "parameter_type"),
            ("echo", "return_type"),
        ] {
            let n = node(&f, name);
            let evidence = n.metadata["type_references"]
                .as_array()
                .unwrap_or_else(|| panic!("{path}: {n:?}"));
            let evidence = evidence
                .iter()
                .find(|r| r["context"] == context && r["label"] == "Payload")
                .unwrap_or_else(|| panic!("{path}: missing {context}: {evidence:?}"));
            let start = evidence["start_byte"].as_u64().unwrap() as usize;
            let end = evidence["end_byte"].as_u64().unwrap() as usize;
            assert!(text[start..end].contains("Payload"));
            let reference = f
                .references
                .iter()
                .find(|r| r.id == evidence["reference_id"].as_str().unwrap())
                .unwrap();
            assert_eq!(reference.source, n.id);
            assert_eq!(
                reference.candidate_keys,
                [key(node(&f, "Payload"))],
                "{path}: {context}"
            );
        }
    }
}

#[test]
fn generic_arguments_implements_and_type_parameter_shadowing() {
    let f = facts(
        "Holder.java",
        "package api; class Payload {} class T {} interface Face {} class Base<X> {} class Holder<T> extends Base<Payload> implements Face { T hidden; Payload visible; }",
    );
    let holder = node(&f, "Holder");
    assert!(f.references.iter().any(|r| r.source == holder.id
        && r.relation == "implements"
        && r.candidate_keys == [key(node(&f, "Face"))]));
    assert!(f.references.iter().any(|r| r.source == holder.id
        && r.relation == "inherits"
        && r.candidate_keys == [key(node(&f, "Base"))]));
    let evidence = holder.metadata["type_references"].as_array().unwrap();
    assert!(
        evidence
            .iter()
            .any(|e| e["context"] == "generic_arg" && e["label"] == "Payload")
    );
    let shadowed: Vec<_> = f
        .references
        .iter()
        .filter(|r| r.label == "T" && r.relation == "references")
        .collect();
    assert!(!shadowed.is_empty());
    assert!(shadowed.iter().all(|r| r.candidate_keys.is_empty()));
}

#[test]
fn property_accessors_own_calls_and_keep_backing_fields_distinct() {
    for (path, text, property, backing, callee) in [
        (
            "Box.cs",
            "class Box { int _value; static int Read() { return 1; } int Value { get { return Read(); } set { _value = value; } } }",
            "Value",
            "_value",
            "Read",
        ),
        (
            "Box.swift",
            "struct Box {\n var stored: Int = 0\n static func read() -> Int { return 1 }\n var value: Int { get { return Box.read() } }\n}\n",
            "value",
            "stored",
            "Box.read",
        ),
    ] {
        let f = facts(path, text);
        let p = node(&f, property);
        assert_eq!(p.kind, "property");
        assert_ne!(p.id, node(&f, backing).id);
        let call = calls(&f, callee)[0];
        let accessor = f.nodes.iter().find(|n| n.id == call.source).unwrap();
        assert_eq!(accessor.kind, "accessor");
        assert!(
            f.edges
                .iter()
                .any(|e| e.relation == "contains" && e.source == p.id && e.target == accessor.id)
        );
    }
}

#[test]
fn header_language_uses_ast_markers_and_retains_c_for_ambiguous_headers() {
    for text in [
        "struct Item { int value; };",
        "// class Fake {}; namespace nope {}\nint work(void);",
        "#define TEXT \"namespace fake {}\"\nint work(void);",
        "struct Hook { int (*run)(void); };",
    ] {
        let f = facts("api.h", text);
        assert_eq!(f.nodes[0].metadata["language"], "c", "{text}");
    }
    for text in [
        "namespace api { struct Item {}; }",
        "class Item { public: void work(); };",
        "struct Item { void work(); };",
        "template<class T> struct Item { T value; };",
    ] {
        let f = facts("api.h", text);
        assert_eq!(f.nodes[0].metadata["language"], "cpp", "{text}");
        assert!(
            f.nodes[0].metadata["binding_aliases"]
                .as_array()
                .unwrap()
                .iter()
                .any(|k| k == "c:file:api.h")
        );
    }
}

#[test]
fn quoted_headers_link_declarations_and_bodies_without_merging_nodes() {
    for (
        header,
        declaration,
        body_path,
        body_source,
        caller_path,
        caller_source,
        decl_name,
        impl_name,
    ) in [
        (
            "api/work.h",
            "#ifndef API_WORK_H\n#define API_WORK_H\nint work(void);\n#endif\n",
            "api/work.c",
            "#include \"work.h\"\nint work(void) { return 1; }",
            "api/main.c",
            "#include \"work.h\"\nint main(void) { return work(); }",
            "work",
            "work",
        ),
        (
            "api/tool.h",
            "struct Tool { void work(); virtual void dynamic(); };",
            "api/tool.cpp",
            "#include \"tool.h\"\nvoid Tool::work() {}",
            "api/main.cpp",
            "#include \"tool.h\"\nvoid run(Tool tool) { tool.work(); tool.dynamic(); }",
            "work",
            "Tool.work",
        ),
    ] {
        let decl = facts(header, declaration);
        let body = facts(body_path, body_source);
        let caller = facts(caller_path, caller_source);
        let declaration_id = node(&decl, decl_name).id.clone();
        let body_id = node(&body, impl_name).id.clone();
        let directory = tempfile::tempdir().unwrap();
        let mut store = Store::create(&directory.path().join("graph.db")).unwrap();
        store
            .apply_native(
                "fixture",
                vec![decl, body, caller],
                vec![],
                Coverage::default(),
            )
            .unwrap();
        let snapshot = store.snapshot().unwrap();
        assert!(snapshot.nodes.iter().any(|n| n.id == declaration_id));
        assert!(snapshot.nodes.iter().any(|n| n.id == body_id));
        assert!(
            snapshot
                .edges
                .iter()
                .any(|e| e.relation == "calls" && e.target == declaration_id),
            "{header}: {:?}",
            snapshot.edges
        );
        assert!(snapshot.edges.iter().any(|e| e.relation == "implemented_by"
            && e.source == declaration_id
            && e.target == body_id));
        assert!(snapshot.edges.iter().any(|e| e.relation == "declared_by"
            && e.source == body_id
            && e.target == declaration_id));
        assert!(!snapshot.edges.iter().any(|e| {
            e.relation == "calls"
                && snapshot
                    .nodes
                    .iter()
                    .any(|n| n.id == e.target && n.label == "dynamic")
        }));
        store
            .apply_native(
                "fixture",
                vec![facts(body_path, "")],
                vec![],
                Coverage::default(),
            )
            .unwrap();
        assert!(
            !store
                .snapshot()
                .unwrap()
                .edges
                .iter()
                .any(|e| e.relation == "implemented_by")
        );
    }
    let caller = facts(
        "app.c",
        "#include \"one.h\"\n#include \"two.h\"\nint run(void) { return work(); }",
    );
    assert!(
        !calls(&caller, "work")[0]
            .candidate_keys
            .iter()
            .any(|k| k.starts_with("c-cpp:"))
    );
}

fn swift_facts(path: &str, text: &str, module: &str, imports: &[(&str, &str)]) -> FileFacts {
    let mut f = facts(path, text);
    graf::languages::compiled::apply_swift_context(
        &mut f,
        module,
        &imports
            .iter()
            .map(|(n, id)| (n.to_string(), id.to_string()))
            .collect::<Vec<_>>(),
    );
    for node in &f.nodes {
        assert!(
            node.metadata
                .get("binding_aliases")
                .is_none_or(serde_json::Value::is_array),
            "context must not insert null aliases: {node:?}"
        );
    }
    f
}

#[test]
fn swift_exact_context_resolves_same_module_types_functions_and_extensions() {
    let declaration = swift_facts(
        "Core/Tool.swift",
        "struct Tool {\n static func ping() {}\n}\nfunc work() {}\nprivate func secret() {}\n",
        "core-root",
        &[],
    );
    let extension = swift_facts(
        "Core/Extra.swift",
        "extension Tool {\n func extra() { work() }\n}\n",
        "core-root",
        &[],
    );
    let caller = swift_facts(
        "Core/Run.swift",
        "func run(tool: Tool) {\n work()\n Tool.ping()\n tool.extra()\n secret()\n}\nfunc shadow(work: () -> Void) { work() }\n",
        "core-root",
        &[],
    );
    let other = swift_facts(
        "Other/Tool.swift",
        "struct Tool {}\nfunc work() {}\n",
        "other-root",
        &[],
    );
    let type_id = node(&declaration, "Tool").id.clone();
    let work_id = node(&declaration, "work").id.clone();
    let ping_id = node(&declaration, "ping").id.clone();
    let extra_id = node(&extension, "extra").id.clone();
    let secret_id = node(&declaration, "secret").id.clone();
    assert!(key(node(&declaration, "secret")).starts_with("swift:symbol:@Core/Tool.swift:"));
    assert!(calls(&caller, "work")[1].candidate_keys.is_empty());
    let directory = tempfile::tempdir().unwrap();
    let mut store = Store::create(&directory.path().join("graph.db")).unwrap();
    store
        .apply_native(
            "fixture",
            vec![declaration, extension, caller, other],
            vec![],
            Coverage::default(),
        )
        .unwrap();
    let snapshot = store.snapshot().unwrap();
    for target in [work_id, ping_id, extra_id] {
        assert!(
            snapshot
                .edges
                .iter()
                .any(|e| e.relation == "calls" && e.target == target),
            "{target}: {:?}",
            snapshot.edges
        );
    }
    assert!(
        snapshot
            .edges
            .iter()
            .any(|e| e.relation == "extends" && e.target == type_id)
    );
    assert!(
        !snapshot
            .edges
            .iter()
            .any(|e| e.relation == "calls" && e.target == secret_id)
    );
}

#[test]
fn swift_foreign_modules_require_explicit_export_visibility() {
    let core = swift_facts(
        "Core/API.swift",
        "public func publicWork() {}\nfunc internalWork() {}\npublic struct Tool {\n public static func ping() {}\n static func hidden() {}\n}\nstruct Hidden { public static func ping() {} }\n",
        "core-root",
        &[],
    );
    let local = swift_facts(
        "Core/Local.swift",
        "func local() { internalWork() }\n",
        "core-root",
        &[],
    );
    let app = swift_facts(
        "App/Main.swift",
        "import Core\nfunc run() {\n Core.publicWork()\n publicWork()\n Core.internalWork()\n internalWork()\n Core.Tool.ping()\n Core.Tool.hidden()\n Core.Hidden.ping()\n}\n",
        "app-root",
        &[("Core", "core-root")],
    );
    let internal_id = node(&core, "internalWork").id.clone();
    let public_id = node(&core, "publicWork").id.clone();
    let local_id = node(&local, "local").id.clone();
    let app_id = node(&app, "run").id.clone();
    assert_eq!(node(&core, "publicWork").metadata["swift_exported"], true);
    assert_eq!(
        node(&core, "internalWork").metadata["swift_exported"],
        false
    );
    let directory = tempfile::tempdir().unwrap();
    let mut store = Store::create(&directory.path().join("graph.db")).unwrap();
    store
        .apply_native(
            "fixture",
            vec![core, local, app],
            vec![],
            Coverage::default(),
        )
        .unwrap();
    let snapshot = store.snapshot().unwrap();
    assert!(
        snapshot
            .edges
            .iter()
            .any(|e| e.source == local_id && e.target == internal_id && e.relation == "calls")
    );
    assert_eq!(
        snapshot
            .edges
            .iter()
            .filter(|e| e.source == app_id && e.target == public_id && e.relation == "calls")
            .count(),
        2
    );
    assert!(
        !snapshot
            .edges
            .iter()
            .any(|e| e.source == app_id && e.target == internal_id && e.relation == "calls")
    );
    assert_eq!(
        snapshot
            .edges
            .iter()
            .filter(|e| e.source == app_id && e.relation == "calls")
            .count(),
        3
    );
    let ambiguous = swift_facts(
        "App/Ambiguous.swift",
        "import Core\nimport Other\nfunc run() { publicWork() }\n",
        "app-root",
        &[("Core", "core-root"), ("Other", "other-root")],
    );
    assert!(
        calls(&ambiguous, "publicWork")[0]
            .candidate_keys
            .iter()
            .all(|k| !k.contains(":export:"))
    );
    let conflicting = swift_facts(
        "App/Conflict.swift",
        "import Core\nfunc run() { Core.publicWork() }\n",
        "app-root",
        &[("Core", "core-root"), ("Core", "other-root")],
    );
    assert!(
        calls(&conflicting, "Core.publicWork")[0]
            .candidate_keys
            .iter()
            .all(|k| !k.contains(":export:"))
    );
}

#[test]
fn cuda_kernel_launches_and_device_calls_use_the_cuda_grammar() {
    let header = facts(
        "gpu/kernels.cuh",
        "#pragma once\nnamespace gpu {\nstruct Sample { float value; };\n__device__ inline float scale(float value) { return value * 3.0f; }\n__global__ void fill(float *output) { output[0] = scale(2.0f); }\n}\n",
    );
    let source = "#include \"kernels.cuh\"\n// π keeps byte offsets honest\nint blocks() { return 1; }\nvoid launch(float *output) { gpu::fill<<<blocks(), 32>>>(output); }\n";
    let caller = facts("gpu/launch.cu", source);
    assert_eq!(header.nodes[0].metadata["dialect"], "cuda");
    assert_eq!(caller.nodes[0].metadata["dialect"], "cuda");
    assert!(caller.nodes[0].metadata.get("normalization").is_none());
    let launch = node(&caller, "launch");
    assert_eq!(launch.line, Some(4));
    assert_eq!(calls(&caller, "gpu::fill")[0].source, launch.id);
    assert_eq!(calls(&caller, "blocks")[0].source, launch.id);
    assert_eq!(calls(&header, "scale")[0].source, node(&header, "fill").id);
    let start = launch.metadata["start_byte"].as_u64().unwrap() as usize;
    let end = launch.metadata["end_byte"].as_u64().unwrap() as usize;
    assert_eq!(
        &source[start..end],
        "void launch(float *output) { gpu::fill<<<blocks(), 32>>>(output); }"
    );
    let target = node(&header, "fill").id.clone();
    let directory = tempfile::tempdir().unwrap();
    let mut store = Store::create(&directory.path().join("graph.db")).unwrap();
    store
        .apply_native("fixture", vec![header, caller], vec![], Coverage::default())
        .unwrap();
    assert!(
        store
            .snapshot()
            .unwrap()
            .edges
            .iter()
            .any(|e| e.relation == "calls" && e.target == target)
    );
}

#[test]
fn metal_audited_declarations_keep_original_utf8_crlf_ranges() {
    let source = "#include <metal_stdlib>\r\nusing namespace metal;\r\n// éclair\r\nstruct Sample { float value; };\r\nfloat scale(float value) { return value * 3.0f; }\r\nkernel void fill(\r\n device float *output [[buffer(0)]],\r\n constant float &input [[buffer(1)]],\r\n uint index [[thread_position_in_grid]]\r\n) { output[index] = scale(input); }\r\n";
    let f = facts("shaders/fill.metal", source);
    assert_eq!(f.nodes[0].metadata["dialect"], "metal");
    let fill = node(&f, "fill");
    assert_eq!(fill.line, Some(6));
    assert_eq!(fill.end_line, Some(10));
    assert_eq!(calls(&f, "scale")[0].source, fill.id);
    assert_eq!(
        calls(&f, "scale")[0].candidate_keys,
        [key(node(&f, "scale"))]
    );
    let start = fill.metadata["start_byte"].as_u64().unwrap() as usize;
    let end = fill.metadata["end_byte"].as_u64().unwrap() as usize;
    assert!(source[start..end].contains("device float *output [[buffer(0)]]"));
    let spans = f.nodes[0].metadata["normalization"].as_array().unwrap();
    let original: Vec<_> = spans
        .iter()
        .map(|span| {
            let start = span["start_byte"].as_u64().unwrap() as usize;
            let end = span["end_byte"].as_u64().unwrap() as usize;
            &source[start..end]
        })
        .collect();
    for syntax in [
        "kernel",
        "device",
        "constant",
        "[[buffer(0)]]",
        "[[thread_position_in_grid]]",
    ] {
        assert!(original.contains(&syntax), "missing {syntax}: {original:?}");
    }
    assert_eq!(f.nodes.iter().filter(|n| n.label == "Sample").count(), 1);
}

#[test]
fn cpp_cli_managed_declarations_allocations_and_arithmetic_keep_source_evidence() {
    let source = "[assembly: Version(\r\n \"2.0\"\r\n)];\r\n// naïve arithmetic must survive\r\nnamespace api {\r\n public ref class Wrapper {\r\n public:\r\n  System::String^ name;\r\n  array<System::String^>^ names;\r\n  static void ping() {}\r\n  static System::String^ make(System::Object^ value, int% count) { ping(); return gcnew System::String(); }\r\n  static int fold(int left, int right) { int tmp = left% right; return MASK^ tmp; }\r\n  void dynamicCall() {}\r\n };\r\n}\r\nvoid run(api::Wrapper *value) { api::Wrapper::ping(); value->dynamicCall(); }\r\n";
    let f = facts("managed/Wrapper.h", source);
    assert_eq!(f.nodes[0].metadata["dialect"], "cpp_cli");
    assert_eq!(f.nodes[0].metadata["language"], "cpp");
    assert_eq!(node(&f, "Wrapper").line, Some(6));
    assert_eq!(node(&f, "make").line, Some(11));
    assert_eq!(node(&f, "fold").line, Some(12));
    assert_eq!(node(&f, "name").kind, "field");
    assert_eq!(node(&f, "names").kind, "field");
    assert!(node(&f, "dynamicCall").binding_key.is_none());
    assert_eq!(calls(&f, "ping")[0].candidate_keys, [key(node(&f, "ping"))]);
    assert_eq!(
        calls(&f, "api::Wrapper::ping")[0].candidate_keys,
        [key(node(&f, "ping"))]
    );
    assert!(
        f.references
            .iter()
            .any(|r| r.relation == "calls" && r.label == "System::String")
    );
    let spans = f.nodes[0].metadata["normalization"].as_array().unwrap();
    for arithmetic in ["left% right", "MASK^ tmp"] {
        let start = source.find(arithmetic).unwrap();
        assert!(
            spans
                .iter()
                .all(|s| s["end_byte"].as_u64().unwrap() as usize <= start
                    || s["start_byte"].as_u64().unwrap() as usize >= start + arithmetic.len())
        );
    }
    let constructor = source.find("gcnew").unwrap();
    assert!(
        spans
            .iter()
            .any(|s| s["kind"] == "managed_allocation" && s["start_byte"] == constructor)
    );
    let directory = tempfile::tempdir().unwrap();
    let mut store = Store::create(&directory.path().join("graph.db")).unwrap();
    let dynamic = node(&f, "dynamicCall").id.clone();
    store
        .apply_native("fixture", vec![f], vec![], Coverage::default())
        .unwrap();
    assert!(
        !store
            .snapshot()
            .unwrap()
            .edges
            .iter()
            .any(|e| e.relation == "calls" && e.target == dynamic)
    );
}

#[test]
fn dialect_markers_in_literals_comments_and_macros_do_not_rewrite_code() {
    let source = "// ref class Fake { gcnew Thing(); }\n#define DIALECT_WORDS ref class\nnamespace demo {\nconst char *text = R\"tag(public ref class Hidden { gcnew Thing(); })tag\";\nint gcnew() { return 3; }\nint fold(int left, int right) { return (left ^ right) % 7; }\n}\n";
    for path in ["plain.cpp", "plain.h"] {
        let f = facts(path, source);
        assert!(f.nodes[0].metadata.get("normalization").is_none(), "{path}");
        assert_eq!(node(&f, "gcnew").kind, "function");
        assert!(
            !f.nodes
                .iter()
                .any(|n| matches!(n.label.as_str(), "Fake" | "Hidden"))
        );
    }
    let metal = facts(
        "plain.metal",
        "// kernel void fake(device float* p) {}\nconst char *text = \"kernel void hidden(device float *p)\";\nfloat work(float value) { return value; }\n",
    );
    assert!(metal.nodes[0].metadata.get("normalization").is_none());
    for (path, source) in [
        (
            "unknown.cpp",
            "ref class Box { property System::String^ Label { System::String^ get() { return nullptr; } } };",
        ),
        (
            "broken.metal",
            "kernel void fill(device float *p [[buffer(0 ???)]]) {}",
        ),
    ] {
        let f = parse(path, source, "hash").unwrap().unwrap();
        assert!(
            !f.diagnostics.is_empty(),
            "{path}: unknown syntax must remain visible"
        );
        assert!(f.nodes.is_empty());
        assert!(f.references.is_empty());
    }
}

#[test]
fn cross_project_visibility_requires_access_through_every_enclosing_type() {
    for (path, source) in [
        (
            "Visible.java",
            "package api; public class Visible { public static void exposed() {} static void hidden() {} public interface Contract { void implicit(); } private static class Secret { public static void blocked() {} } } class Hidden { public static void enclosed() {} }",
        ),
        (
            "Visible.cs",
            "namespace api; public class Visible { public static void exposed() {} static void hidden() {} public interface Contract { void implicit(); } private class Secret { public static void blocked() {} } } internal class Hidden { public static void enclosed() {} }",
        ),
        (
            "Visible.kt",
            "package api\nclass Visible {\n fun exposed() {}\n private fun hidden() {}\n interface Contract {\n  fun implicit()\n }\n private class Secret {\n  fun blocked() {}\n }\n}\ninternal class Hidden {\n fun enclosed() {}\n}\n",
        ),
    ] {
        let f = facts(path, source);
        for name in ["Visible", "exposed", "Contract", "implicit"] {
            assert_eq!(
                node(&f, name).metadata["cross_project_public"],
                true,
                "{path}: {name}"
            );
        }
        for name in ["hidden", "Secret", "blocked", "Hidden", "enclosed"] {
            assert_eq!(
                node(&f, name).metadata["cross_project_public"],
                false,
                "{path}: {name}"
            );
        }
        assert_eq!(node(&f, "exposed").metadata["dynamic_dispatch"], false);
        assert_eq!(node(&f, "implicit").metadata["dynamic_dispatch"], true);
    }
    let cpp = facts(
        "visible.cpp",
        "namespace api { void exposed() {} static void internalFunction() {} struct Visible { void implicit() {} private: void hidden() {} struct Secret { public: static void blocked() {} }; }; class DefaultPrivate { void enclosed() {} public: static void publicMember() {} private: void outOfLine(); }; namespace { void anonymous() {} namespace nested { void stillAnonymous() {} } } } void api::DefaultPrivate::outOfLine() {}",
    );
    for name in [
        "exposed",
        "Visible",
        "implicit",
        "DefaultPrivate",
        "publicMember",
    ] {
        assert_eq!(
            node(&cpp, name).metadata["cross_project_public"],
            true,
            "C++: {name}"
        );
    }
    for name in [
        "internalFunction",
        "hidden",
        "Secret",
        "blocked",
        "enclosed",
        "anonymous",
        "stillAnonymous",
        "api.DefaultPrivate.outOfLine",
    ] {
        assert_eq!(
            node(&cpp, name).metadata["cross_project_public"],
            false,
            "C++: {name}"
        );
    }
}

fn compiled_context(mut files: Vec<FileFacts>) -> Vec<FileFacts> {
    let units = files
        .iter()
        .map(|f| (f.path.clone(), "fixture-unit".to_owned()))
        .collect();
    let context = graf::languages::compiled::CompiledContext::new(&files, &units);
    for f in &mut files {
        context.apply(f);
    }
    files
}
fn compiled_snapshot(files: Vec<FileFacts>) -> graf::model::GraphSnapshot {
    let directory = tempfile::tempdir().unwrap();
    let mut store = Store::create(&directory.path().join("graph.db")).unwrap();
    store
        .apply_native("fixture", files, vec![], Coverage::default())
        .unwrap();
    store.snapshot().unwrap()
}

#[test]
fn annotations_enum_cases_and_conformance_keep_written_type_evidence() {
    let java = facts(
        "Marked.java",
        "package api; @interface Uses { Class<?> value(); } class Payload {} @Uses(Payload.class) class Marked { @Uses(Payload.class) void run() {} } enum State { READY, DONE }",
    );
    for owner in ["Marked", "run"] {
        let n = node(&java, owner);
        let evidence = n.metadata["type_references"].as_array().unwrap();
        for label in ["Uses", "Payload"] {
            assert!(
                evidence
                    .iter()
                    .any(|e| e["context"] == "attribute" && e["label"] == label),
                "{owner}: {evidence:?}"
            );
        }
    }
    for (path, source, enum_name, cases) in [
        (
            "State.java",
            "package api; enum State { READY, DONE }",
            "State",
            ["READY", "DONE"],
        ),
        (
            "State.cs",
            "namespace api; public interface Contract {} public class Worker : Contract {} public enum State { READY, DONE = 3 }",
            "State",
            ["READY", "DONE"],
        ),
        (
            "State.kt",
            "package api\ninterface Contract {}\nclass Worker : Contract\nenum class State { READY, DONE }\n",
            "State",
            ["READY", "DONE"],
        ),
        (
            "State.swift",
            "protocol Contract {}\nstruct Worker: Contract {}\nstruct Payload {}\nenum State {\n case ready\n case done(Payload)\n}\n",
            "State",
            ["ready", "done"],
        ),
    ] {
        let f = facts(path, source);
        for case in cases {
            assert_eq!(node(&f, case).kind, "enum_case");
            assert!(
                f.edges.iter().any(|e| e.relation == "case_of"
                    && e.source == node(&f, enum_name).id
                    && e.target == node(&f, case).id),
                "{path}"
            );
        }
        if path != "State.java" {
            assert!(
                f.references.iter().any(|r| r.relation == "implements"
                    && r.source == node(&f, "Worker").id
                    && r.candidate_keys == [key(node(&f, "Contract"))]),
                "{path}: {:?}",
                f.references
            );
        }
        if path.ends_with("swift") {
            assert!(f.references.iter().any(|r| r.source == node(&f, "State").id
                && r.label == "Payload"
                && r.relation == "references"));
        }
    }
    let external = facts(
        "External.java",
        "package api; class Uses {} @external.Uses class Marked {}",
    );
    assert!(
        external
            .references
            .iter()
            .filter(|r| r.label == "external.Uses")
            .all(|r| !r
                .candidate_keys
                .contains(&key(node(&external, "Uses")).to_owned()))
    );
}

#[test]
fn inherited_nonvirtual_members_resolve_only_through_proven_base_chains() {
    for (a, b, c, base, derived, caller) in [
        (
            "Base.java",
            "Derived.java",
            "Run.java",
            "package api; public class Base { public final void work() {} }",
            "package api; public class Derived extends Base {}",
            "package api; class Run { void run(Derived value) { value.work(); } }",
        ),
        (
            "base.cpp",
            "derived.cpp",
            "run.cpp",
            "namespace api { struct Base { void work() {} }; }",
            "namespace api { struct Derived : Base {}; }",
            "namespace api { void run(Derived value) { value.work(); } }",
        ),
        (
            "Base.cs",
            "Derived.cs",
            "Run.cs",
            "namespace api; public class Base { public void work() {} }",
            "namespace api; public class Derived : Base { public void own() { base.work(); } }",
            "namespace api; class Run { void run(Derived value) { value.work(); } }",
        ),
        (
            "Base.kt",
            "Derived.kt",
            "Run.kt",
            "package api\nopen class Base {\n fun work() {}\n}\n",
            "package api\nclass Derived : Base()\n",
            "package api\nfun run(value: Derived) { value.work() }\n",
        ),
    ] {
        let base = facts(a, base);
        let derived = facts(b, derived);
        let caller = facts(c, caller);
        let target = node(&base, "work").id.clone();
        let source = node(&caller, "run").id.clone();
        let snapshot = compiled_snapshot(compiled_context(vec![base, derived, caller]));
        assert!(
            snapshot
                .edges
                .iter()
                .any(|e| e.source == source && e.target == target && e.relation == "calls"),
            "{a}: {:?}",
            snapshot.edges
        );
    }
    let files = compiled_context(vec![
        facts(
            "Base.cs",
            "namespace api; public class Base { public virtual void work() {} } public class Overloads { public void both() {} public void both(int value) {} }",
        ),
        facts(
            "Run.cs",
            "namespace api; public class Derived : Base {} public class Unknown : Missing {} class Run { void run(Derived d, Unknown u, Overloads o) { d.work(); u.work(); o.both(); } }",
        ),
    ]);
    let snapshot = compiled_snapshot(files);
    assert!(!snapshot.edges.iter().any(|e| e.relation == "calls"));
    assert!(
        snapshot
            .edges
            .iter()
            .any(|e| e.relation == "declared_member")
    );
}

#[test]
fn csharp_partial_and_interface_navigation_preserve_ambiguity_and_units() {
    let original = vec![
        facts(
            "One.cs",
            "namespace api; public partial class Hub { public static void one() {} } public interface Contract { void work(); }",
        ),
        facts(
            "Two.cs",
            "namespace api; public partial class Hub { public static void two() { one(); } } public class Worker : Contract { public void work() {} }",
        ),
        facts(
            "Run.cs",
            "namespace api; class Run { void run(Contract value) { Hub.two(); value.work(); } }",
        ),
    ];
    let files = compiled_context(original.clone());
    let interface = node(&files[0], "work").id.clone();
    let implementation = node(&files[1], "work").id.clone();
    let snapshot = compiled_snapshot(files);
    assert!(snapshot.edges.iter().any(|e| e.relation == "partial_of"));
    assert!(snapshot.edges.iter().any(|e| e.relation == "implemented_by"
        && e.source == interface
        && e.target == implementation));
    assert!(
        snapshot
            .edges
            .iter()
            .any(|e| e.relation == "declared_member" && e.target == interface)
    );
    assert!(!snapshot.edges.iter().any(|e|e.relation=="calls" && (e.target==interface || e.target==implementation)));
    let mut split = original;
    let units = split
        .iter()
        .map(|f| (f.path.clone(), f.path.clone()))
        .collect();
    let context = graf::languages::compiled::CompiledContext::new(&split, &units);
    for f in &mut split {
        context.apply(f);
    }
    assert!(
        split
            .iter()
            .flat_map(|f| &f.references)
            .all(|r| !matches!(r.relation.as_str(), "partial_of" | "implemented_by"))
    );
    let multiple = compiled_context(vec![facts(
        "Many.cs",
        "namespace api; public interface Contract { void work(); } public class First : Contract { public void work() {} } public class Second : Contract { public void work() {} }",
    )]);
    assert!(
        multiple[0]
            .references
            .iter()
            .all(|r| r.relation != "implemented_by")
    );
}

#[test]
fn kotlin_written_companion_and_delegation_supply_static_navigation() {
    let files = compiled_context(vec![facts(
        "Forward.kt",
        "package api\ninterface Contract { fun work() }\nclass Delegate : Contract {\n override fun work() {}\n}\nclass Forward(val target: Delegate) : Contract by target\nclass Factory {\n companion object {\n  fun build() {}\n }\n}\nfun run(value: Forward) {\n Factory.build()\n value.work()\n}\n",
    )]);
    let f = &files[0];
    assert!(f.references.iter().any(|r| r.relation == "delegates_to"
        && r.source == node(f, "Forward").id
        && r.candidate_keys == [key(node(f, "Delegate"))]));
    let build = node(f, "build").id.clone();
    let work = node(f, "work").id.clone();
    let snapshot = compiled_snapshot(files);
    assert!(
        snapshot
            .edges
            .iter()
            .any(|e| e.relation == "calls" && e.target == build)
    );
    assert!(
        snapshot
            .edges
            .iter()
            .any(|e| e.relation == "declared_member" && e.target == work)
    );
    assert!(
        !snapshot
            .edges
            .iter()
            .any(|e| e.relation == "calls" && e.target == work)
    );
    let unknown = compiled_context(vec![facts(
        "Unknown.kt",
        "package api\nclass Unknown\nfun run(value: Unknown) { value.work() }\n",
    )]);
    assert!(
        unknown[0]
            .references
            .iter()
            .all(|r| r.relation != "declared_member")
    );
}

#[test]
fn swift_inherited_and_public_extension_members_require_real_module_visibility() {
    let files = compiled_context(vec![
        swift_facts(
            "Core/Types.swift",
            "public class Base {\n public final func work() {}\n}\npublic class Derived: Base {}\npublic struct Widget {}\nstruct Hidden {}\n",
            "Core-id",
            &[],
        ),
        swift_facts(
            "Core/Extras.swift",
            "public extension Widget {\n func added() {}\n private func secret() {}\n}\npublic extension Hidden {\n func blocked() {}\n}\n",
            "Core-id",
            &[],
        ),
        swift_facts(
            "App/Run.swift",
            "import Core\nfunc run(value: Core.Derived, widget: Core.Widget, hidden: Core.Hidden) {\n value.work()\n widget.added()\n widget.secret()\n hidden.blocked()\n}\n",
            "App-id",
            &[("Core", "Core-id")],
        ),
    ]);
    let work = node(&files[0], "work").id.clone();
    let added = node(&files[1], "added").id.clone();
    let secret = node(&files[1], "secret").id.clone();
    let blocked = node(&files[1], "blocked").id.clone();
    let snapshot = compiled_snapshot(files);
    for target in [work, added] {
        assert!(
            snapshot
                .edges
                .iter()
                .any(|e| e.relation == "calls" && e.target == target),
            "{target}: {:?}",
            snapshot.edges
        );
    }
    assert!(!snapshot.edges.iter().any(|e| e.relation == "calls"
        && [secret.as_str(), blocked.as_str()].contains(&e.target.as_str())));
}

#[test]
fn compiled_context_fingerprint_tracks_proof_instead_of_method_body_comments() {
    use graf::languages::compiled::CompiledContext;
    let units = [("Base.cs".to_owned(), "assembly".to_owned())]
        .into_iter()
        .collect();
    let first = CompiledContext::new(
        &[facts(
            "Base.cs",
            "namespace api; public class Base { public void work() {} }",
        )],
        &units,
    );
    let comments = CompiledContext::new(
        &[facts(
            "Base.cs",
            "namespace api; public class Base { public void work() { /* explanation */ } }",
        )],
        &units,
    );
    let hidden = CompiledContext::new(
        &[facts(
            "Base.cs",
            "namespace api; public class Base { private void work() {} }",
        )],
        &units,
    );
    assert_eq!(first.fingerprint(), comments.fingerprint());
    assert_ne!(first.fingerprint(), hidden.fingerprint());
}

#[test]
fn cpp_string_tests_preserve_neighbors_unicode_ranges_and_nested_subcases() {
    let source = "// π\r\nvoid helper() {}\r\nTEST_CASE(\"a \\\"quoted\\\" name\") { SUBCASE(\"inside\") { helper(); } }\r\nTEST_CASE_TEMPLATE(\"***\", T, int, long) { helper(); }\r\nSCENARIO(\"...\") { helper(); }\r\nvoid neighbor() { helper(); }\r\n";
    let f = facts("checks.cpp", source);
    assert_eq!(node(&f, "helper").kind, "function");
    assert_eq!(node(&f, "neighbor").line, Some(6));
    let tests: Vec<_> = f.nodes.iter().filter(|n| n.kind == "test").collect();
    assert_eq!(tests.len(), 3);
    assert_eq!(
        tests
            .iter()
            .map(|n| &n.id)
            .collect::<std::collections::HashSet<_>>()
            .len(),
        3
    );
    for test in tests {
        let start = test.metadata["start_byte"].as_u64().unwrap() as usize;
        let end = test.metadata["end_byte"].as_u64().unwrap() as usize;
        assert!(source[start..end].contains(&test.label));
        assert!(
            f.references
                .iter()
                .any(|r| r.source == test.id && r.label == "helper")
        );
        assert!(
            f.edges.iter().any(|e| e.source == f.nodes[0].id
                && e.target == test.id
                && e.relation == "contains")
        );
    }
    assert!(!f.nodes.iter().any(|n| n.label == "\"inside\""));
    let literals = facts(
        "literal.cpp",
        "const char *text = \"TEST_CASE(\\\"fake\\\") {}\"; void neighbor() {}\n",
    );
    assert!(!literals.nodes.iter().any(|n| n.kind == "test"));
    let broken = parse(
        "broken.cpp",
        "TEST_CASE(\"unfinished\") { helper();",
        "hash",
    )
    .unwrap()
    .unwrap();
    assert!(!broken.diagnostics.is_empty());
    assert!(broken.nodes.is_empty());
}

#[test]
fn inherited_caller_updates_do_not_require_rewriting_unchanged_providers() {
    use graf::languages::compiled::CompiledContext;
    let base = facts(
        "Base.cs",
        "namespace api; public class Base { public void work() {} }",
    );
    let derived = facts(
        "Derived.cs",
        "namespace api; public class Derived : Base {}",
    );
    let empty = facts(
        "Run.cs",
        "namespace api; class Run { void run(Derived value) {} }",
    );
    let call = facts(
        "Run.cs",
        "namespace api; class Run { void run(Derived value) { value.work(); } }",
    );
    let units = ["Base.cs", "Derived.cs", "Run.cs"]
        .into_iter()
        .map(|p| (p.into(), "assembly".into()))
        .collect();
    let initial = CompiledContext::new(&[base.clone(), derived.clone(), empty.clone()], &units);
    let changed = CompiledContext::new(&[base.clone(), derived.clone(), call.clone()], &units);
    assert_eq!(initial.fingerprint(), changed.fingerprint());
    let directory = tempfile::tempdir().unwrap();
    let mut store = Store::create(&directory.path().join("graph.db")).unwrap();
    let mut files = vec![base, derived, empty];
    for f in &mut files {
        initial.apply(f);
    }
    let target = node(&files[0], "work").id.clone();
    store
        .apply_native("fixture", files, vec![], Coverage::default())
        .unwrap();
    let mut call = call;
    changed.apply(&mut call);
    store
        .apply_native("fixture", vec![call], vec![], Coverage::default())
        .unwrap();
    assert!(
        store
            .snapshot()
            .unwrap()
            .edges
            .iter()
            .any(|e| e.relation == "calls" && e.target == target)
    );
    store
        .apply_native(
            "fixture",
            vec![facts("Base.cs", "")],
            vec![],
            Coverage::default(),
        )
        .unwrap();
    assert!(
        !store
            .snapshot()
            .unwrap()
            .edges
            .iter()
            .any(|e| e.relation == "calls")
    );
}

#[test]
fn conditional_cpp_inherited_members_stay_unresolved_across_updates() {
    use graf::languages::compiled::CompiledContext;
    let directory = tempfile::tempdir().unwrap();
    let mut store = Store::create(&directory.path().join("graph.db")).unwrap();
    let mut previous_fingerprint = None;
    for conditional in [false, true, false] {
        let declaration = if conditional {
            "#if FEATURE\n void probe() {}\n#endif\n"
        } else {
            " void probe() {}\n"
        };
        let mut files = vec![
            facts(
                "Root.cpp",
                &format!("namespace sample {{\nstruct Root {{\n{declaration}}};\n}}\n"),
            ),
            facts("Leaf.cpp", "namespace sample { struct Leaf : Root {}; }"),
            facts(
                "Use.cpp",
                "namespace sample { void inspect(Leaf value) { value.probe(); } }",
            ),
        ];
        let original_keys = calls(&files[2], "value.probe")[0].candidate_keys.clone();
        let units = files
            .iter()
            .map(|f| (f.path.clone(), "native-unit".into()))
            .collect();
        let context = CompiledContext::new(&files, &units);
        if let Some(previous) = previous_fingerprint.replace(context.fingerprint().to_owned()) {
            assert_ne!(previous, context.fingerprint());
        }
        for f in &mut files {
            context.apply(f);
        }
        let probe = node(&files[0], "probe");
        let target = probe.id.clone();
        let source = node(&files[2], "inspect").id.clone();
        if conditional {
            assert!(probe.binding_key.is_none());
            assert!(
                probe.metadata["binding_aliases"]
                    .as_array()
                    .is_none_or(|keys| {
                        keys.iter()
                            .all(|key| !key.as_str().unwrap().starts_with("compiled:"))
                    })
            );
            assert_eq!(
                calls(&files[2], "value.probe")[0].candidate_keys,
                original_keys
            );
        }
        store
            .apply_native("fixture", files, vec![], Coverage::default())
            .unwrap();
        let snapshot = store.snapshot().unwrap();
        for relation in ["calls", "declared_member"] {
            assert_eq!(
                snapshot
                    .edges
                    .iter()
                    .any(|e| e.source == source && e.target == target && e.relation == relation),
                !conditional,
                "conditional={conditional}, relation={relation}: {:?}",
                snapshot.edges
            );
        }
        if conditional {
            assert!(!snapshot.edges.iter().any(|e| e.source == source
                && matches!(e.relation.as_str(), "calls" | "declared_member")));
        }
    }
}

#[test]
fn inherited_java_overloads_never_select_the_nearest_wrong_signature() {
    use graf::languages::compiled::CompiledContext;
    let directory = tempfile::tempdir().unwrap();
    let mut store = Store::create(&directory.path().join("graph.db")).unwrap();
    let mut previous_fingerprint = None;
    for middle_name in ["other", "probe", "other"] {
        let mut files = vec![
            facts(
                "Root.java",
                "package sample; public class Root { public final void probe() {} }",
            ),
            facts(
                "Middle.java",
                &format!(
                    "package sample; public class Middle extends Root {{ public final void {middle_name}(int n) {{}} }}"
                ),
            ),
            facts(
                "Leaf.java",
                "package sample; public class Leaf extends Middle {}",
            ),
            facts(
                "Use.java",
                "package sample; class Use { void inspect(Leaf value) { value.probe(); } }",
            ),
        ];
        let original_keys = calls(&files[3], "value.probe")[0].candidate_keys.clone();
        let units = files
            .iter()
            .map(|f| (f.path.clone(), "java-unit".into()))
            .collect();
        let context = CompiledContext::new(&files, &units);
        if let Some(previous) = previous_fingerprint.replace(context.fingerprint().to_owned()) {
            assert_ne!(previous, context.fingerprint());
        }
        for f in &mut files {
            context.apply(f);
        }
        let source = node(&files[3], "inspect").id.clone();
        let root_method = node(&files[0], "probe").id.clone();
        let middle_method = node(&files[1], middle_name).id.clone();
        if middle_name == "probe" {
            assert_eq!(
                calls(&files[3], "value.probe")[0].candidate_keys,
                original_keys
            );
        }
        store
            .apply_native("fixture", files, vec![], Coverage::default())
            .unwrap();
        let snapshot = store.snapshot().unwrap();
        for relation in ["calls", "declared_member"] {
            assert!(!snapshot.edges.iter().any(|e| e.source == source
                && e.target == middle_method
                && e.relation == relation));
            assert_eq!(
                snapshot.edges.iter().any(|e| e.source == source
                    && e.target == root_method
                    && e.relation == relation),
                middle_name == "other",
                "middle={middle_name}, relation={relation}: {:?}",
                snapshot.edges
            );
        }
        if middle_name == "probe" {
            assert!(!snapshot.edges.iter().any(|e| e.source == source
                && matches!(e.relation.as_str(), "calls" | "declared_member")));
        }
    }
}

#[test]
fn swift_extension_explicit_internal_access_overrides_public_default_on_updates() {
    use graf::languages::compiled::CompiledContext;
    let directory = tempfile::tempdir().unwrap();
    let mut store = Store::create(&directory.path().join("graph.db")).unwrap();
    let mut previous_fingerprint = None;
    // Omitted access and explicit internal previously had identical visibility
    // proof. Both transitions must invalidate foreign aliases, not just public.
    for access in ["", "internal ", "public ", "internal "] {
        let mut files = vec![
            swift_facts(
                "Core/Types.swift",
                "public struct Widget {}",
                "Core-unit",
                &[],
            ),
            swift_facts(
                "Core/Extras.swift",
                &format!(
                    "public extension Widget {{\n {access}func hidden() {{}}\n func visible() {{}}\n}}\n"
                ),
                "Core-unit",
                &[],
            ),
            swift_facts(
                "Core/Use.swift",
                "func local(value: Widget) {\n value.hidden()\n value.visible()\n}\n",
                "Core-unit",
                &[],
            ),
            swift_facts(
                "App/Use.swift",
                "import Core\nfunc inspect(value: Core.Widget) {\n value.hidden()\n value.visible()\n}\n",
                "App-unit",
                &[("Core", "Core-unit")],
            ),
        ];
        let context = CompiledContext::new(&files, &Default::default());
        if let Some(previous) = previous_fingerprint.replace(context.fingerprint().to_owned()) {
            assert_ne!(previous, context.fingerprint());
        }
        for f in &mut files {
            context.apply(f);
        }
        let hidden = node(&files[1], "hidden");
        let target = hidden.id.clone();
        let visible = node(&files[1], "visible").id.clone();
        let local = node(&files[2], "local").id.clone();
        let foreign = node(&files[3], "inspect").id.clone();
        if access == "internal " {
            assert!(
                hidden.metadata["binding_aliases"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|key| !key.as_str().unwrap().contains(":export:"))
            );
        }
        store
            .apply_native("fixture", files, vec![], Coverage::default())
            .unwrap();
        let snapshot = store.snapshot().unwrap();
        for (source, target, expected) in [
            (&local, &target, true),
            (&local, &visible, true),
            (&foreign, &visible, true),
            (&foreign, &target, access != "internal "),
        ] {
            assert_eq!(
                snapshot
                    .edges
                    .iter()
                    .any(|e| &e.source == source && &e.target == target && e.relation == "calls"),
                expected,
                "access={access:?}, source={source}, target={target}: {:?}",
                snapshot.edges
            );
        }
    }
}

#[test]
fn inherited_parameterless_overrides_and_hiding_keep_nearest_declaration_navigation() {
    for (path, source) in [
        (
            "Override.cs",
            "namespace sample; public class Base { public virtual void work() {} } public class Middle : Base { public override void work() {} } public class Leaf : Middle {} class Use { void inspect(Leaf value) { value.work(); } }",
        ),
        (
            "Hiding.cs",
            "namespace sample; public class Base { public void work() {} } public class Middle : Base { public new void work() {} } public class Leaf : Middle {} class Use { void inspect(Leaf value) { value.work(); } }",
        ),
        (
            "Override.java",
            "package sample; class Base { public void work() {} } class Middle extends Base { public void work() {} } class Leaf extends Middle {} class Use { void inspect(Leaf value) { value.work(); } }",
        ),
        (
            "override.cpp",
            "namespace sample { struct Base { virtual void work() {} }; struct Middle : Base { void work() override {} }; struct Leaf : Middle {}; void inspect(Leaf value) { value.work(); } }",
        ),
        (
            "Override.kt",
            "package sample\nopen class Base {\n open fun work() {}\n}\nopen class Middle : Base() {\n override fun work() {}\n}\nclass Leaf : Middle()\nfun inspect(value: Leaf) { value.work() }\n",
        ),
        (
            "Override.swift",
            "class Base {\n func work() {}\n}\nclass Middle: Base {\n override func work() {}\n}\nclass Leaf: Middle {}\nfunc inspect(value: Leaf) { value.work() }\n",
        ),
    ] {
        let f = facts(path, source);
        let caller = node(&f, "inspect").id.clone();
        let middle = node(&f, "Middle").id.clone();
        let method = f
            .nodes
            .iter()
            .find(|n| {
                n.label == "work"
                    && f.edges
                        .iter()
                        .any(|e| e.relation == "contains" && e.source == middle && e.target == n.id)
            })
            .unwrap();
        let target = method.id.clone();
        assert_eq!(method.metadata["parameterless"], true, "{path}");
        let original_keys = calls(&f, "value.work")[0].candidate_keys.clone();
        let files = compiled_context(vec![f]);
        assert_eq!(
            calls(&files[0], "value.work")[0].candidate_keys,
            original_keys,
            "{path}"
        );
        let snapshot = compiled_snapshot(files);
        let declared: Vec<_> = snapshot
            .edges
            .iter()
            .filter(|e| e.relation == "declared_member" && e.source == caller)
            .map(|e| e.target.clone())
            .collect();
        assert_eq!(declared, [target], "{path}: {:?}", snapshot.edges);
        assert!(
            !snapshot
                .edges
                .iter()
                .any(|e| e.relation == "calls" && e.source == caller),
            "{path}"
        );
    }
}

#[test]
fn inherited_class_and_interface_branches_keep_distinct_declaration_evidence() {
    // Both interface paths reach the same contract. Preserve its declaration
    // once alongside the class declaration, without selecting a runtime target.
    let provider = facts(
        "Types.cs",
        "namespace sample; public interface Contract { void work(); } public interface Left : Contract {} public interface Right : Contract {} public class Base { public void work() {} } public class Middle : Base, Left, Right {} public class Leaf : Middle {}",
    );
    let caller = facts(
        "Use.cs",
        "namespace sample; class Use { void inspect(Leaf value) { value.work(); } }",
    );
    let source = node(&caller, "inspect").id.clone();
    let expected: std::collections::BTreeSet<_> = provider
        .nodes
        .iter()
        .filter(|n| n.label == "work")
        .map(|n| n.id.clone())
        .collect();
    assert_eq!(expected.len(), 2);
    let original_keys = calls(&caller, "value.work")[0].candidate_keys.clone();
    let files = compiled_context(vec![provider, caller]);
    let refs: Vec<_> = files[1]
        .references
        .iter()
        .filter(|r| r.relation == "declared_member")
        .collect();
    assert_eq!(refs.len(), 2);
    assert_ne!(refs[0].id, refs[1].id);
    assert_eq!(
        calls(&files[1], "value.work")[0].candidate_keys,
        original_keys
    );
    let snapshot = compiled_snapshot(files);
    let actual: std::collections::BTreeSet<_> = snapshot
        .edges
        .iter()
        .filter(|e| e.source == source && e.relation == "declared_member")
        .map(|e| e.target.clone())
        .collect();
    assert_eq!(actual, expected);
    assert!(
        !snapshot
            .edges
            .iter()
            .any(|e| e.source == source && e.relation == "calls")
    );

    for classes in [
        // Unknown ancestry can supply another declaration.
        "public class Base { public void work() {} } public class Leaf : Base, Missing {}",
        // A field hides the callable name.
        "public class Base { public void work() {} } public class Leaf : Base, Contract { public int work; }",
        // Overloads on one branch cannot provide an exact declaration identity.
        "public class Base { public void work() {} public void work(int n) {} } public class Leaf : Base, Contract {}",
        // Unrelated duplicate owners cannot establish the receiver type.
        "public class Base { public void work() {} } public class Leaf : Base, Contract {} public class Leaf : Base, Contract {}",
    ] {
        let files = compiled_context(vec![
            facts(
                "Types.cs",
                &format!(
                    "namespace sample; public interface Contract {{ void work(); }} {classes}"
                ),
            ),
            facts(
                "Use.cs",
                "namespace sample; class Use { void inspect(Leaf value) { value.work(); } }",
            ),
        ]);
        let source = node(&files[1], "inspect").id.clone();
        let snapshot = compiled_snapshot(files);
        assert!(
            !snapshot.edges.iter().any(|e| e.source == source
                && matches!(e.relation.as_str(), "calls" | "declared_member")),
            "{classes}: {:?}",
            snapshot.edges
        );
    }
}

#[test]
fn inherited_declaration_navigation_updates_when_signature_or_access_proof_changes() {
    use graf::languages::compiled::CompiledContext;
    let directory = tempfile::tempdir().unwrap();
    let mut store = Store::create(&directory.path().join("graph.db")).unwrap();
    let mut previous_fingerprint = None;
    for (declaration, expected) in [
        ("public override void work() {}", true),
        ("public void work(int n) {}", false),
        ("private void work() {}", false),
        ("public override void work() {}", true),
    ] {
        let mut files = vec![
            facts(
                "Base.cs",
                "namespace sample; public class Base { public virtual void work() {} }",
            ),
            facts(
                "Middle.cs",
                &format!("namespace sample; public class Middle : Base {{ {declaration} }}"),
            ),
            facts("Leaf.cs", "namespace sample; public class Leaf : Middle {}"),
            facts(
                "Use.cs",
                "namespace sample; class Use { void inspect(Leaf value) { value.work(); } }",
            ),
        ];
        let units = files
            .iter()
            .map(|f| (f.path.clone(), "assembly".into()))
            .collect();
        let context = CompiledContext::new(&files, &units);
        if let Some(previous) = previous_fingerprint.replace(context.fingerprint().to_owned()) {
            assert_ne!(previous, context.fingerprint());
        }
        for f in &mut files {
            context.apply(f);
        }
        let source = node(&files[3], "inspect").id.clone();
        let target = node(&files[1], "work").id.clone();
        store
            .apply_native("fixture", files, vec![], Coverage::default())
            .unwrap();
        let snapshot = store.snapshot().unwrap();
        let declared: Vec<_> = snapshot
            .edges
            .iter()
            .filter(|e| e.source == source && e.relation == "declared_member")
            .map(|e| e.target.clone())
            .collect();
        assert_eq!(
            declared,
            if expected { vec![target] } else { vec![] },
            "{declaration}"
        );
        assert!(
            !snapshot
                .edges
                .iter()
                .any(|e| e.source == source && e.relation == "calls")
        );
    }
}

#[test]
fn swift_existential_receiver_navigates_only_to_protocol_requirement() {
    let source = r#"protocol Channel { func send() -> Int }
struct First: Channel { func send() -> Int { 1 } }
struct Second: Channel { func send() -> Int { 2 } }
func invoke(_ value: any Channel) -> Int { value.send() }
func ordinary(_ value: Channel) -> Int { value.send() }
func shadow(_ value: any Channel) { let value = unknown; value.send() }
func changed(_ input: any Channel, _ other: any Channel) { var value: any Channel = input; value = other; value.send() }
func unknown(_ value: Any) { value.send() }
func staticUse() { Channel.send() }
"#;
    let file = facts("Channel.swift", source);
    let contract = node(&file, "Channel");
    let target = file
        .nodes
        .iter()
        .find(|n| {
            n.label == "send"
                && file.edges.iter().any(|e| {
                    e.relation == "contains" && e.source == contract.id && e.target == n.id
                })
        })
        .unwrap();
    assert!(target.binding_key.is_none());
    let target = target.id.clone();
    let positives: Vec<_> = ["invoke", "ordinary"]
        .iter()
        .map(|name| node(&file, name).id.clone())
        .collect();
    let negatives: Vec<_> = ["shadow", "changed", "unknown", "staticUse"]
        .iter()
        .map(|name| node(&file, name).id.clone())
        .collect();
    let directory = tempfile::tempdir().unwrap();
    let mut store = Store::create(&directory.path().join("graph.db")).unwrap();
    store
        .apply_native(
            "fixture",
            compiled_context(vec![file]),
            vec![],
            Coverage::default(),
        )
        .unwrap();
    let snapshot = store.snapshot().unwrap();
    for source in positives {
        let edges: Vec<_> = snapshot
            .edges
            .iter()
            .filter(|e| {
                e.source == source && matches!(e.relation.as_str(), "calls" | "declared_member")
            })
            .collect();
        assert_eq!(edges.len(), 1, "{edges:?}");
        assert_eq!(edges[0].relation, "declared_member");
        assert_eq!(edges[0].target, target);
    }
    assert!(
        !snapshot.edges.iter().any(|e| negatives.contains(&e.source)
            && matches!(e.relation.as_str(), "calls" | "declared_member")),
        "{:?}",
        snapshot.edges
    );
    let replacement = facts(
        "Channel.swift",
        "protocol Channel { func receive() }\nfunc invoke(_ value: any Channel) { value.send() }\n",
    );
    store
        .apply_native(
            "fixture",
            compiled_context(vec![replacement]),
            vec![],
            Coverage::default(),
        )
        .unwrap();
    let snapshot = store.snapshot().unwrap();
    assert!(!snapshot.nodes.iter().any(|n| n.id == target));
    assert!(
        !snapshot
            .edges
            .iter()
            .any(|e| matches!(e.relation.as_str(), "calls" | "declared_member"))
    );
}

#[test]
fn swift_existential_ambiguous_or_inaccessible_members_stay_unresolved() {
    for source in [
        "protocol Channel { func send(); func send(_ x: Int) }\nfunc invoke(_ value: any Channel) { value.send() }\n",
        "protocol Channel { private func send() }\nfunc invoke(_ value: any Channel) { value.send() }\n",
        "protocol Channel { func send() }\nprotocol Other { func send() }\nfunc invoke(_ value: any Channel & Other) { value.send() }\n",
        "protocol Channel { func send() }\nfunc invoke(_ value: (any Channel)?) { value.send() }\n",
    ] {
        let file = facts("Channel.swift", source);
        let caller = node(&file, "invoke").id.clone();
        let snapshot = compiled_snapshot(compiled_context(vec![file]));
        assert!(
            !snapshot.edges.iter().any(|e| e.source == caller
                && matches!(e.relation.as_str(), "calls" | "declared_member")),
            "{source}: {:?}",
            snapshot.edges
        );
    }
}

#[test]
fn kotlin_cross_file_object_and_companion_calls_select_exact_members() {
    for declaration in [
        "object Beacon {\n fun emit() {}\n}\n",
        "class Beacon {\n companion object {\n  fun emit() {}\n }\n}\n",
        "class Beacon {\n companion object Factory {\n  fun emit() {}\n }\n}\n",
    ] {
        let provider = format!("package signals\n{declaration}");
        for same_file in [false, true] {
            let caller = "fun inspect() { Beacon.emit() }\n";
            let files = if same_file {
                vec![facts("Beacon.kt", &format!("{provider}{caller}"))]
            } else {
                vec![
                    facts("Beacon.kt", &provider),
                    facts(
                        "Use.kt",
                        &format!("package app\nimport signals.Beacon\n{caller}"),
                    ),
                    facts(
                        "Other.kt",
                        "package other\nobject Beacon {\n fun emit() {}\n}\n",
                    ),
                ]
            };
            let target = node(&files[0], "emit").id.clone();
            assert_eq!(node(&files[0], "emit").metadata["static"], true);
            let caller_file = if same_file { 0 } else { 1 };
            let source = node(&files[caller_file], "inspect").id.clone();
            let snapshot = compiled_snapshot(compiled_context(files));
            let actual: Vec<_> = snapshot
                .edges
                .iter()
                .filter(|e| e.source == source && e.relation == "calls")
                .map(|e| (e.target.as_str(), e.confidence.as_str()))
                .collect();
            assert_eq!(
                actual,
                [(target.as_str(), "statically_resolved")],
                "{declaration}, same_file={same_file}"
            );
        }
    }
}

#[test]
fn kotlin_cross_file_member_calls_reject_duplicate_private_and_dynamic_receivers() {
    for (provider, extra, caller) in [
        // A duplicate without the member must still poison the receiver identity.
        (
            "object Beacon {\n fun emit() {}\n}\n",
            "package signals\nobject Beacon {}\n",
            "fun inspect() { Beacon.emit() }\n",
        ),
        (
            "object Beacon {\n private fun emit() {}\n}\n",
            "",
            "fun inspect() { Beacon.emit() }\n",
        ),
        (
            "private object Beacon {\n fun emit() {}\n}\n",
            "",
            "fun inspect() { Beacon.emit() }\n",
        ),
        (
            "class Beacon {\n companion object {\n  private fun emit() {}\n }\n}\n",
            "",
            "fun inspect() { Beacon.emit() }\n",
        ),
        (
            "object Beacon {\n fun emit() {}\n}\n",
            "",
            "fun inspect(Beacon: Any) { Beacon.emit() }\n",
        ),
        (
            "object Beacon {\n fun emit() {}\n}\n",
            "",
            "fun inspect(value: Any) { value.emit() }\n",
        ),
        (
            "open class Beacon {\n open fun emit() {}\n}\n",
            "",
            "fun inspect(value: Beacon) { value.emit() }\n",
        ),
    ] {
        let files = vec![
            facts("Beacon.kt", &format!("package signals\n{provider}")),
            facts("Extra.kt", extra),
            facts(
                "Use.kt",
                &format!("package app\nimport signals.Beacon\n{caller}"),
            ),
        ];
        let source = node(&files[2], "inspect").id.clone();
        let snapshot = compiled_snapshot(compiled_context(files));
        assert!(
            !snapshot
                .edges
                .iter()
                .any(|e| e.source == source && e.relation == "calls"),
            "{provider} / {extra} / {caller}: {:?}",
            snapshot.edges
        );
    }
    let files = vec![
        facts(
            "One.kt",
            "package first\nobject Beacon {\n fun emit() {}\n}\n",
        ),
        facts(
            "Two.kt",
            "package second\nobject Beacon {\n fun emit() {}\n}\n",
        ),
        facts("Use.kt", "package app\nfun inspect() { Beacon.emit() }\n"),
    ];
    let source = node(&files[2], "inspect").id.clone();
    let snapshot = compiled_snapshot(compiled_context(files));
    assert!(
        !snapshot
            .edges
            .iter()
            .any(|e| e.source == source && e.relation == "calls")
    );
}

#[test]
fn cpp_qualified_only_definitions_resolve_without_a_member_declaration() {
    for (header, declared) in [
        ("struct Beacon {};\n", false),
        ("struct Beacon {\n SIGNAL_BODY()\n};\n", false),
        ("struct Beacon { static void emit(); };\n", true),
    ] {
        // Opaque macro syntax may produce diagnostics; the separate, explicit
        // definition still provides an exact identity without preprocessing.
        let files = vec![
            parse("beacon.hpp", header, "hash").unwrap().unwrap(),
            facts(
                "beacon.cpp",
                "#include \"beacon.hpp\"\nvoid Beacon::emit() {}\n",
            ),
            facts(
                "use.cpp",
                "#include \"beacon.hpp\"\nvoid inspect() { Beacon::emit(); }\n",
            ),
        ];
        let target = if declared {
            node(&files[0], "emit").id.clone()
        } else {
            node(&files[1], "Beacon.emit").id.clone()
        };
        assert_eq!(
            key(node(&files[1], "Beacon.emit")),
            "cpp:symbol:Beacon.emit"
        );
        let source = node(&files[2], "inspect").id.clone();
        let snapshot = compiled_snapshot(compiled_context(files));
        let actual: Vec<_> = snapshot
            .edges
            .iter()
            .filter(|e| e.source == source && e.relation == "calls")
            .map(|e| (e.target.as_str(), e.confidence.as_str()))
            .collect();
        assert_eq!(
            actual,
            [(target.as_str(), "statically_resolved")],
            "{header}"
        );
    }
}

#[test]
fn cpp_qualified_calls_reject_duplicate_private_and_dynamic_definitions() {
    for (header, duplicate, caller) in [
        ("struct Beacon {};\n", true, "Beacon::emit();"),
        (
            "class Beacon { static void emit(); };\n",
            false,
            "Beacon::emit();",
        ),
        (
            "struct Beacon { virtual void emit(); };\n",
            false,
            "Beacon::emit();",
        ),
        ("struct Beacon {};\n", false, "unknown.emit();"),
    ] {
        let body = "#include \"beacon.hpp\"\nvoid Beacon::emit() {}\n";
        let mut files = vec![
            facts("beacon.hpp", header),
            facts("beacon.cpp", body),
            facts(
                "use.cpp",
                &format!("#include \"beacon.hpp\"\nvoid inspect() {{ {caller} }}\n"),
            ),
        ];
        let source = node(&files[2], "inspect").id.clone();
        if duplicate {
            files.push(facts("duplicate.cpp", body));
        }
        let snapshot = compiled_snapshot(compiled_context(files));
        assert!(
            !snapshot
                .edges
                .iter()
                .any(|e| e.source == source && e.relation == "calls"),
            "{header}, duplicate={duplicate}, {caller}: {:?}",
            snapshot.edges
        );
    }
}

#[test]
fn compiled_cross_file_members_survive_incremental_caller_changes_and_provider_restoration() {
    use graf::index::{IndexOptions, run_with_options};
    for (provider_path, provider, caller_path, caller, target_label) in [
        (
            "Beacon.kt",
            "package signals\nobject Beacon {\n fun emit() {}\n}\n",
            "Use.kt",
            "package app\nimport signals.Beacon\nfun inspect() { Beacon.emit() }\n",
            "emit",
        ),
        (
            "Beacon.kt",
            "package signals\nclass Beacon {\n companion object {\n  fun emit() {}\n }\n}\n",
            "Use.kt",
            "package app\nimport signals.Beacon\nfun inspect() { Beacon.emit() }\n",
            "emit",
        ),
        (
            "beacon.cpp",
            "#include \"beacon.hpp\"\nvoid Beacon::emit() {}\n",
            "use.cpp",
            "#include \"beacon.hpp\"\nvoid inspect() { Beacon::emit(); }\n",
            "Beacon.emit",
        ),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("src");
        std::fs::create_dir(&root).unwrap();
        let db = directory.path().join("graph.db");
        std::fs::write(root.join(provider_path), provider).unwrap();
        std::fs::write(root.join(caller_path), caller).unwrap();
        if provider_path.ends_with(".cpp") {
            std::fs::write(root.join("beacon.hpp"), "struct Beacon {};\n").unwrap();
        }
        let options = IndexOptions {
            code_only: true,
            ..Default::default()
        };
        run_with_options(&root, &db, &options).unwrap();
        let initial = Store::open(&db).unwrap().snapshot().unwrap();
        let source = initial
            .nodes
            .iter()
            .find(|n| n.file == caller_path && n.label == "inspect")
            .unwrap()
            .id
            .clone();
        let target = initial
            .nodes
            .iter()
            .find(|n| n.file == provider_path && n.label == target_label)
            .unwrap()
            .id
            .clone();
        let assert_calls = |snapshot: &graf::model::GraphSnapshot, present: bool| {
            let actual: Vec<_> = snapshot
                .edges
                .iter()
                .filter(|e| e.source == source && e.relation == "calls")
                .map(|e| (e.target.as_str(), e.confidence.as_str()))
                .collect();
            let expected = if present {
                vec![(target.as_str(), "statically_resolved")]
            } else {
                vec![]
            };
            assert_eq!(actual, expected, "{provider_path}: {:?}", snapshot.edges);
        };
        assert_calls(&initial, true);
        std::fs::write(
            root.join(caller_path),
            format!("{caller}// caller-only edit\n"),
        )
        .unwrap();
        let report = run_with_options(&root, &db, &options).unwrap();
        assert!(
            report.unchanged_files >= 1,
            "provider must be reused: {report:?}"
        );
        assert_calls(&Store::open(&db).unwrap().snapshot().unwrap(), true);

        let duplicate = if provider_path.ends_with(".cpp") {
            "duplicate.cpp"
        } else {
            "Duplicate.kt"
        };
        std::fs::write(root.join(duplicate), provider).unwrap();
        run_with_options(&root, &db, &options).unwrap();
        assert_calls(&Store::open(&db).unwrap().snapshot().unwrap(), false);
        std::fs::remove_file(root.join(duplicate)).unwrap();
        run_with_options(&root, &db, &options).unwrap();
        assert_calls(&Store::open(&db).unwrap().snapshot().unwrap(), true);

        std::fs::remove_file(root.join(provider_path)).unwrap();
        run_with_options(&root, &db, &options).unwrap();
        let removed = Store::open(&db).unwrap().snapshot().unwrap();
        assert!(!removed.nodes.iter().any(|n| n.id == target));
        assert_calls(&removed, false);
        std::fs::write(root.join(provider_path), provider).unwrap();
        run_with_options(&root, &db, &options).unwrap();
        assert_calls(&Store::open(&db).unwrap().snapshot().unwrap(), true);
    }
}
