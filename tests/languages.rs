use graf::{
    languages::{parse, recognizes, revision, supports},
    model::{FileFacts, Node, Reference},
};

fn facts(path: &str, source: &str) -> FileFacts {
    let f = parse(path, source, "hash").unwrap().unwrap();
    assert!(f.diagnostics.is_empty(), "{:?}", f.diagnostics);
    f
}
fn node<'a>(f: &'a FileFacts, label: &str) -> &'a Node {
    f.nodes
        .iter()
        .find(|n| n.label == label)
        .unwrap_or_else(|| panic!("missing node {label}: {:?}", f.nodes))
}
fn calls<'a>(f: &'a FileFacts, label: &str) -> Vec<&'a Reference> {
    f.references
        .iter()
        .filter(|r| r.relation == "calls" && r.label == label)
        .collect()
}
fn key(n: &Node) -> &str {
    n.binding_key.as_deref().unwrap()
}

#[test]
fn dispatch_errors_and_ranges() {
    for path in [
        "a.js", "a.jsx", "a.ts", "a.tsx", "a.mjs", "a.cjs", "a.mts", "a.cts", "a.rs", "a.go",
    ] {
        assert!(supports(path));
    }
    assert!(!supports("a.py"));
    assert!(parse("a.py", "", "h").unwrap().is_none());
    assert!(!revision().is_empty());
    assert!(parse("../a.ts", "", "h").is_err());
    for (path, text) in [
        ("a.js", "function {"),
        ("a.rs", "fn {"),
        ("a.go", "package p; func {"),
    ] {
        let f = parse(path, text, "h").unwrap().unwrap();
        assert!(f.nodes.is_empty());
        assert!(!f.diagnostics.is_empty());
    }
    let source = "// π\nexport function hi() {\n  return 1;\n}\n";
    let f = facts("lib.ts", source);
    let n = node(&f, "hi");
    assert_eq!((n.line, n.end_line), (Some(2), Some(4)));
    assert_eq!(
        &source[n.metadata["start_byte"].as_u64().unwrap() as usize
            ..n.metadata["end_byte"].as_u64().unwrap() as usize],
        "function hi() {\n  return 1;\n}"
    );
}
#[test]
fn javascript_named_default_namespace_and_tsx() {
    let lib = facts(
        "ui/lib.ts",
        "export function work() {}\nexport default function Main() {}\nfunction secret() {}",
    );
    let app = facts(
        "ui/app.tsx",
        "import Main, {work as run} from './lib';\nimport * as api from './lib';\nexport const App = () => { run(); api.work(); Main(); return <div/>; };",
    );
    assert_eq!(
        calls(&app, "run")[0].candidate_keys[0],
        key(node(&lib, "work"))
    );
    assert_eq!(
        calls(&app, "api.work")[0].candidate_keys[0],
        key(node(&lib, "work"))
    );
    assert_eq!(
        calls(&app, "Main")[0].candidate_keys[0],
        key(node(&lib, "Main"))
    );
    assert_eq!(calls(&app, "run")[0].source, node(&app, "App").id);
    assert_eq!(key(node(&lib, "work")), "javascript:ui/lib:work");
    assert_ne!(key(node(&lib, "secret")), "javascript:ui/lib:secret");
}
#[test]
fn javascript_shadowing_mutation_and_ownership() {
    let f = facts(
        "app.js",
        "import {run} from './lib';\nfunction outer(run) { run(); }\nfunction okay() { run(); { let run = other; run(); } }\nfunction nested() { return () => run(); }\n",
    );
    let c = calls(&f, "run");
    assert_eq!(c.len(), 4);
    assert!(c[0].candidate_keys.is_empty());
    assert_eq!(c[1].candidate_keys[0], "javascript:lib:run");
    assert!(c[2].candidate_keys.is_empty());
    assert_ne!(c[3].source, node(&f, "nested").id);
    let f = facts(
        "app.js",
        "function run() {} function f() { run = other; run(); }",
    );
    assert!(calls(&f, "run")[0].candidate_keys.is_empty());
    assert!(node(&f, "run").binding_key.is_none());
}
#[test]
fn javascript_types_dynamic_and_colliding_names() {
    let f = facts(
        "app.ts",
        "import type {run} from './lib'; interface Work { run(): void } type Fn = () => void; function f() { run(); obj.run(); obj[key](); } ",
    );
    assert_eq!(node(&f, "Work").kind, "interface");
    assert_eq!(node(&f, "Fn").kind, "type");
    assert!(
        f.references
            .iter()
            .filter(|r| r.relation == "calls")
            .all(|r| r.candidate_keys.is_empty())
    );
    let f = facts(
        "app.js",
        "function run() {} function run() {} function caller() { run(); }",
    );
    assert!(calls(&f, "run")[0].candidate_keys.is_empty());
}
#[test]
fn rust_cross_file_use_groups_and_modules() {
    let lib = facts("src/tools.rs", "pub fn work() {} pub fn other() {}");
    let main = facts(
        "src/lib.rs",
        "mod tools; use crate::tools::{work as run, other}; fn main() { run(); other(); tools::work(); crate::tools::work(); }",
    );
    assert_eq!(key(node(&lib, "work")), "rust:.:tools::work");
    for label in ["run", "tools::work", "crate::tools::work"] {
        assert_eq!(
            calls(&main, label)[0].candidate_keys[0],
            key(node(&lib, "work"))
        );
    }
    assert_eq!(
        calls(&main, "other")[0].candidate_keys[0],
        key(node(&lib, "other"))
    );
    assert_eq!(calls(&main, "run")[0].source, node(&main, "main").id);
    let nested = facts(
        "src/tools/deep.rs",
        "use super::work; pub fn here() { work(); }",
    );
    assert_eq!(
        calls(&nested, "work")[0].candidate_keys[0],
        key(node(&lib, "work"))
    );
}
#[test]
fn rust_impls_closures_shadowing_and_macros() {
    let f = facts(
        "src/lib.rs",
        "fn work() {} struct Tool; impl Tool { fn new() -> Self { work(); Self } fn run(&self) { self.other(); } } fn call(work: fn()) { work(); let c = |work: fn()| work(); println!(\"x\"); } ",
    );
    assert_eq!(node(&f, "Tool").kind, "struct");
    assert_eq!(node(&f, "new").kind, "method");
    assert_eq!(key(node(&f, "new")), "rust:.:Tool::new");
    let c = calls(&f, "work");
    assert_eq!(c.len(), 3);
    assert_eq!(c[0].candidate_keys[0], key(node(&f, "work")));
    assert!(c[1].candidate_keys.is_empty());
    assert!(c[2].candidate_keys.is_empty());
    assert_eq!(
        calls(&f, "self.other")[0].candidate_keys,
        ["rust:.:Tool::other"]
    );
    assert!(
        f.references
            .iter()
            .any(|r| r.reason.contains("macro expansion"))
    );
}
#[test]
fn rust_inline_modules_and_distinct_crates() {
    let f = facts(
        "src/lib.rs",
        "mod tools { pub fn work() {} pub fn run() { self::work(); } } fn main() { tools::work(); }",
    );
    assert_eq!(
        calls(&f, "self::work")[0].candidate_keys[0],
        key(node(&f, "work"))
    );
    assert_eq!(
        calls(&f, "tools::work")[0].candidate_keys[0],
        key(node(&f, "work"))
    );
    let a = facts("crates/a/src/lib.rs", "pub fn work() {}");
    let b = facts("crates/b/src/lib.rs", "pub fn work() {}");
    assert_ne!(key(node(&a, "work")), key(node(&b, "work")));
}
#[test]
fn go_package_functions_import_alias_and_receiver() {
    let a = facts(
        "tools/a.go",
        "package tools\nfunc Work() {}\ntype Tool struct {}\nfunc (t *Tool) Run() { Work() }\n",
    );
    let b = facts(
        "tools/b.go",
        "package tools\nimport io \"example.org/project/io\"\nfunc Main() { Work(); io.Read(); }\n",
    );
    assert_eq!(key(node(&a, "Work")), "go:tools:tools:Work");
    assert_eq!(
        calls(&b, "Work")[0].candidate_keys[0],
        key(node(&a, "Work"))
    );
    assert_eq!(
        calls(&b, "io.Read")[0].candidate_keys,
        ["go:import:example.org/project/io:Read"]
    );
    assert_eq!(node(&a, "Run").kind, "method");
    assert_eq!(key(node(&a, "Run")), "go:tools:tools:Tool.Run");
    assert_eq!(calls(&a, "Work")[0].source, node(&a, "Run").id);
    assert_eq!(a.nodes[0].metadata["package_name"], "tools");
}
#[test]
fn go_shadowing_closures_dot_imports_and_external_test_packages() {
    let f = facts(
        "p/a.go",
        "package p\nfunc Work() {}\nfunc Main(Work func()) { Work(); f := func(Work func()) { Work() }; f(Work) }\n",
    );
    assert!(
        calls(&f, "Work")
            .iter()
            .all(|r| r.candidate_keys.is_empty())
    );
    assert_ne!(calls(&f, "Work")[1].source, node(&f, "Main").id);
    let external = facts("p/a_test.go", "package p_test\nfunc Test() { Work() }\n");
    assert_ne!(
        calls(&external, "Work")[0].candidate_keys[0],
        key(node(&f, "Work"))
    );
    let dot = facts(
        "p/b.go",
        "package p\nimport . \"external.org/p\"\nfunc Main() { Work() }\n",
    );
    assert!(calls(&dot, "Work")[0].candidate_keys.is_empty());
}
#[test]
fn identity_and_containment_are_path_qualified() {
    for (path, source) in [
        ("one/a.ts", "export function work() {}"),
        ("one/a.rs", "pub fn work() {}"),
        ("one/a.go", "package p\nfunc work() {}\n"),
    ] {
        let f = facts(path, source);
        let g = facts(&path.replace("one/", "two/"), source);
        assert_ne!(node(&f, "work").id, node(&g, "work").id);
        assert!(f.edges.iter().any(|e| e.relation == "contains"
            && e.source == f.nodes[0].id
            && e.target == node(&f, "work").id));
    }
}

#[test]
fn javascript_var_hoisting_aliases_and_bare_import_evidence() {
    let f = facts(
        "src/main.js",
        "import {run} from 'pkg'; function f() { if (true) { var run = other; } run(); } function g() { run(); }",
    );
    assert!(calls(&f, "run")[0].candidate_keys.is_empty());
    assert_eq!(
        calls(&f, "run")[1].candidate_keys,
        ["javascript:import:pkg:run"]
    );
    assert!(
        f.references
            .iter()
            .any(|r| r.candidate_keys == ["javascript:import-module:pkg"])
    );
    let lib = facts("lib.js", "export function work() {} export {work as run};");
    assert_eq!(key(node(&lib, "run")), "javascript:lib:run");
    let aliases = lib
        .references
        .iter()
        .find(|r| r.relation == "aliases")
        .unwrap();
    assert_eq!(aliases.candidate_keys, ["javascript:file:lib.js:work"]);
    assert!(has_alias(node(&lib, "work"), &aliases.candidate_keys[0]));
    let barrel = facts("barrel.js", "export {work as run} from './lib';");
    assert_eq!(key(node(&barrel, "run")), "javascript:barrel:run");
    assert!(
        barrel
            .references
            .iter()
            .any(|r| r.relation == "aliases" && r.candidate_keys[0] == key(node(&lib, "work")))
    );
}
#[test]
fn go_default_import_names_need_context_and_shadowing_blocks_them() {
    let f = facts(
        "main.go",
        "package main\nimport \"example.org/wire/v2\"\nfunc main() { wire.Read(); object.Read(); }\nfunc f(wire interface{Read()}) { wire.Read() }\n",
    );
    assert_eq!(
        f.nodes[0].metadata["imports"],
        serde_json::json!([{"path":"example.org/wire/v2","alias":null}])
    );
    assert_eq!(
        calls(&f, "wire.Read")[0].candidate_keys,
        ["go:selector:wire:Read"]
    );
    assert!(calls(&f, "wire.Read")[1].candidate_keys.is_empty());
    let local = facts(
        "main.go",
        "package main\nimport \"example.org/wire/v2\"\nfunc main() { object := makeObject(); object.Read() }\n",
    );
    assert!(calls(&local, "object.Read")[0].candidate_keys.is_empty());
}

#[test]
fn generics_patterns_and_conditional_items_do_not_invent_targets() {
    let r = facts(
        "src/lib.rs",
        "struct T; impl T { fn make() {} } fn f<T>() { T::make(); } fn work() {} fn g(x: Option<fn()>) { match x { Some(work) => work(), _ => () } } #[cfg(feature=\"extra\")] fn maybe() {} fn h() { maybe(); }",
    );
    assert!(calls(&r, "T::make")[0].candidate_keys.is_empty());
    assert!(calls(&r, "work")[0].candidate_keys.is_empty());
    assert!(node(&r, "maybe").binding_key.is_none());
    assert!(calls(&r, "maybe")[0].candidate_keys.is_empty());
    let g = facts(
        "main.go",
        "package main\ntype T int\nfunc f[T any](x T) { T(x) }\n",
    );
    assert!(calls(&g, "T")[0].candidate_keys.is_empty());
}
#[test]
fn javascript_evaluated_class_syntax_keeps_call_ownership() {
    let f = facts(
        "main.js",
        "function base() {} function name() {} class C extends base() { [name()]() { base(); } } class D extends C {}",
    );
    assert_eq!(calls(&f, "base").len(), 2);
    assert_eq!(calls(&f, "base")[0].source, f.nodes[0].id);
    assert_eq!(calls(&f, "name")[0].source, node(&f, "C").id);
    assert_ne!(calls(&f, "base")[1].source, f.nodes[0].id);
    assert!(
        !f.references
            .iter()
            .any(|r| r.source == node(&f, "C").id && r.relation == "inherits")
    );
    assert!(f.references.iter().any(|r| r.source == node(&f, "D").id
        && r.relation == "inherits"
        && r.candidate_keys == [key(node(&f, "C"))]));
}

#[test]
fn assignments_to_later_declarations_and_loop_targets_are_conservative() {
    let f = facts(
        "a.js",
        "function mutate() { work = other; } function work() {} function f() { work(); }",
    );
    assert!(calls(&f, "work")[0].candidate_keys.is_empty());
    assert!(node(&f, "work").binding_key.is_none());
    let f = facts("a.js", "function work() {} for (work of list) {} work();");
    assert!(calls(&f, "work")[0].candidate_keys.is_empty());
}
#[test]
fn declaration_signatures_and_named_function_recursion_are_retained() {
    let f = facts(
        "a.ts",
        "declare function external(): void; interface Job { run(): void } const f = function recursive() { recursive(); }; namespace N { export function hidden() {} function privateMember() {} } function caller() { hidden(); N.hidden(); N.privateMember(); } function shadow(N: unknown) { N.hidden(); }",
    );
    assert_eq!(node(&f, "external").kind, "function");
    assert_eq!(node(&f, "run").kind, "method");
    assert_eq!(
        calls(&f, "recursive")[0].candidate_keys,
        [key(node(&f, "f"))]
    );
    assert!(calls(&f, "hidden")[0].candidate_keys.is_empty());
    assert_eq!(
        calls(&f, "N.hidden")[0].candidate_keys,
        [key(node(&f, "hidden"))]
    );
    assert!(calls(&f, "N.hidden")[1].candidate_keys.is_empty());
    assert!(
        !calls(&f, "N.privateMember")[0]
            .candidate_keys
            .iter()
            .any(|k| k == key(node(&f, "privateMember")))
    );
    let g = facts("a.go", "package p\ntype Job interface { Run() }\n");
    assert_eq!(node(&g, "Run").kind, "method");
    assert!(node(&g, "Run").binding_key.is_none());
}

#[test]
fn javascript_comments_default_exports_and_dynamic_imports() {
    let f = facts(
        "lib.js",
        "export /* comment */ default class { method() {} }",
    );
    assert_eq!(key(node(&f, "default")), "javascript:lib:default");
    let f = facts(
        "a.ts",
        "import /* comment */ type {run} from './lib'; export /* comment */ default function Main() { run(); import('./lib'); }",
    );
    assert!(calls(&f, "run")[0].candidate_keys.is_empty());
    assert_eq!(key(node(&f, "Main")), "javascript:a:default");
    assert!(f.references.iter().any(|r| {
        r.relation == "imports"
            && r.source == node(&f, "Main").id
            && r.candidate_keys
                .first()
                .is_some_and(|k| k == "javascript:module:lib")
    }));
    let f = facts(
        "barrel.js",
        "import Main from './lib'; export default Main;",
    );
    assert_eq!(key(node(&f, "default")), "javascript:barrel:default");
    assert!(
        f.references
            .iter()
            .any(|r| r.relation == "aliases" && r.candidate_keys[0] == "javascript:lib:default")
    );
}

#[test]
fn javascript_identifier_escapes_preserve_binding_and_shadowing() {
    let f = facts(
        "a.js",
        r"export function f() {} function caller(\u0066) { f(); } function other() { \u0066(); }",
    );
    assert!(calls(&f, "f")[0].candidate_keys.is_empty());
    assert_eq!(
        calls(&f, r"\u0066")[0].candidate_keys,
        ["javascript:file:a.js:f"]
    );
    assert!(has_alias(node(&f, "f"), "javascript:file:a.js:f"));
    let escaped = facts("b.js", r"export function \u0066() {}");
    assert_eq!(key(node(&escaped, r"\u0066")), "javascript:b:f");
}

#[test]
fn javascript_lexical_exports_have_exact_file_keys_before_context() {
    for path in ["foo.ts", "foo.mjs"] {
        let f = facts(
            path,
            "export function f(){f();} export {f as alias}; function Shadow(f){f();}\n",
        );
        let exact = format!("javascript:file:{path}:f");
        let own = calls(&f, "f");
        assert_eq!(
            own[0].candidate_keys.as_slice(),
            std::slice::from_ref(&exact)
        );
        assert!(own[1].candidate_keys.is_empty());
        assert!(has_alias(node(&f, "f"), &exact));
        assert!(
            f.references
                .iter()
                .any(|r| r.relation == "aliases" && r.candidate_keys == [exact.clone()])
        );
    }
}

fn has_alias(node: &Node, key: &str) -> bool {
    node.metadata["binding_aliases"]
        .as_array()
        .is_some_and(|a| a.iter().any(|v| v == key))
}
#[test]
fn commonjs_static_exports_require_aliases_and_direct_invocations() {
    let library = facts(
        "lib.cjs",
        "function work() {} module.exports = {work, run: () => work()};\n",
    );
    assert!(has_alias(node(&library, "work"), "javascript:cjs:lib:work"));
    assert!(has_alias(node(&library, "run"), "javascript:cjs:lib:run"));
    let main = facts(
        "main.cjs",
        "const api = require('./lib'); const {work: run} = require('./lib'); const again = require('./lib').work; function Main(){api.work(); run(); again(); require('./lib').work();}\n",
    );
    for label in ["api.work", "run", "again", "require('./lib').work"] {
        assert_eq!(
            calls(&main, label)[0].candidate_keys[0],
            "javascript:cjs:lib:work",
            "{label}"
        );
    }
    assert_eq!(calls(&main, "run")[0].source, node(&main, "Main").id);
    let default = facts("fn.cjs", "module.exports = function work() {};\n");
    assert!(has_alias(
        node(&default, "work"),
        "javascript:cjs:fn:default"
    ));
    let caller = facts("use.cjs", "const work = require('./fn'); work();\n");
    assert_eq!(
        calls(&caller, "work")[0].candidate_keys[0],
        "javascript:cjs:fn:default"
    );
}
#[test]
fn commonjs_shadowing_mutations_and_dynamic_exports_remain_unresolved() {
    let f = facts(
        "main.cjs",
        "function f(require) { const api=require('./lib'); api.work(); } function g(){const api=require(name);api.work();} function h(){let api=require('./lib');api=other;api.work();}\n",
    );
    assert!(
        calls(&f, "api.work")
            .iter()
            .all(|r| r.candidate_keys.is_empty())
    );
    for source in [
        "const module={}; module.exports=function fake(){};",
        "exports.work=function first(){};exports.work=function second(){};",
        "exports.work=function first(){};exports[name]=other;",
        "if(flag){exports.work=function conditional(){}}",
        "if(flag) exports.work=function conditional(){};",
        "flag && (exports.work=function conditional(){});",
        "module.exports={...other, work(){}};",
    ] {
        let f = facts("lib.cjs", source);
        assert!(
            f.nodes.iter().all(
                |n| !n.metadata["binding_aliases"].as_array().is_some_and(|a| a
                    .iter()
                    .any(|v| v.as_str().is_some_and(|s| s.starts_with("javascript:cjs:"))))
            ),
            "{source}"
        );
    }
    let f = facts(
        "main.cjs",
        "require=custom;const api=require('./lib');api.work();\n",
    );
    assert!(calls(&f, "api.work")[0].candidate_keys.is_empty());
}
#[test]
fn source_comments_retain_doc_ranges_rationale_and_exact_document_links() {
    let source = "/** Rationale: use an explicit table because startup must be bounded.\n * @see ../docs/adr/001.md\n * [RFC](../docs/rfc/cache.md#choice) ADR-001 RFC2119\n */\nexport function work() { return 1; }\nconst text = '/** rationale: this is a string */';\n";
    let f = facts("src/main.ts", source);
    let rationale = f.nodes.iter().find(|n| n.kind == "rationale").unwrap();
    assert_eq!((rationale.line, rationale.end_line), (Some(1), Some(4)));
    assert!(f.edges.iter().any(|e| e.source == node(&f, "work").id
        && e.target == rationale.id
        && e.relation == "explains"));
    for key in ["file:docs/adr/001.md", "file:docs/rfc/cache.md"] {
        assert!(
            f.references
                .iter()
                .any(|r| r.source == rationale.id && r.candidate_keys == [key])
        );
    }
    let citation = node(&f, "ADR-0001");
    assert_eq!(citation.kind, "doc_ref");
    assert!(
        f.edges
            .iter()
            .any(|e| e.source == rationale.id && e.target == citation.id && e.relation == "cites")
    );
    assert_eq!(
        f.nodes
            .iter()
            .filter(|n| matches!(n.kind.as_str(), "rationale" | "documentation"))
            .count(),
        1
    );
    assert!(
        rationale.metadata["evidence"]
            .as_str()
            .unwrap()
            .starts_with("/**")
    );
}

#[test]
fn extensionless_shebangs_preserve_paths_and_do_not_claim_unknown_interpreters() {
    for (path, source) in [
        (
            "bin.v1/tool",
            "#!/usr/bin/env -S node --no-warnings\nfunction work() {}\nwork();\n",
        ),
        (
            "bin/python-tool",
            "#!/usr/bin/python3\ndef work():\n    pass\nwork()\n",
        ),
        (
            "bin/julia-tool",
            "#!/usr/bin/env julia\nfunction work()\nend\nwork()\n",
        ),
        ("bin/shell-tool", "#!/bin/sh\nwork() { :; }\nwork\n"),
    ] {
        assert!(recognizes(path, source));
        let f = facts(path, source);
        assert_eq!(f.path, path);
        assert!(f.nodes.iter().all(|n| n.file == path));
        assert_eq!(node(&f, "work").line, Some(2));
        assert!(!calls(&f, "work").is_empty());
    }
    let js = facts(
        "bin.v1/tool",
        "#!/usr/bin/node\nfunction work(){} work();\n",
    );
    assert_eq!(js.module, "bin.v1/tool");
    assert!(node(&js, "work").id.contains("bin.v1/tool"));
    for source in [
        "#!/bin/fish\nfunction work; end\n",
        "#!/usr/bin/env perl\n",
        "#!/usr/bin/env custom\n",
        "// #!/bin/node\n",
    ] {
        assert!(!recognizes("bin/tool", source));
        assert!(parse("bin/tool", source, "h").unwrap().is_none());
    }
    assert!(!recognizes("file.unknown", "#!/usr/bin/node\n"));
    assert!(!recognizes("data.json", "{\"ordinary\":true}"));
}

#[test]
fn typescript_namespace_type_heritage_and_component_evidence() {
    let source = "import type {Base as Parent, Payload} from './types';\nimport View from './View';\nexport namespace Geometry { export interface Shape extends Parent { value: Payload } export function draw() {} }\nexport interface Derived<T> extends Parent { value: T; other: Payload }\nexport class Concrete implements Parent {}\nexport function App() { Geometry.draw(); return <View/>; }\nfunction Shadow(View: unknown) { return <View/>; }\nfunction generic<Payload>(x: Payload): Payload { return x; }\n";
    let f = facts("ui/app.tsx", source);
    assert_eq!(key(node(&f, "Geometry")), "javascript:ui/app:Geometry");
    assert_eq!(key(node(&f, "Shape")), "javascript:ui/app:Geometry.Shape");
    assert_eq!(key(node(&f, "draw")), "javascript:ui/app:Geometry.draw");
    assert_eq!(
        calls(&f, "Geometry.draw")[0].candidate_keys,
        ["javascript:file:ui/app.tsx:Geometry.draw"]
    );
    assert!(f.references.iter().any(|r| {
        r.source == node(&f, "Derived").id
            && r.relation == "inherits"
            && r.candidate_keys
                .contains(&"javascript:ui/types:Base".into())
    }));
    assert!(f.references.iter().any(|r| {
        r.source == node(&f, "Concrete").id
            && r.relation == "implements"
            && r.candidate_keys
                .contains(&"javascript:ui/types:Base".into())
    }));
    let components: Vec<_> = f
        .references
        .iter()
        .filter(|r| r.relation == "uses_component")
        .collect();
    assert_eq!(components.len(), 2);
    assert_eq!(components[0].source, node(&f, "App").id);
    assert_eq!(
        components[0].candidate_keys[0],
        "javascript:ui/View:default"
    );
    assert!(components[1].candidate_keys.is_empty());
    assert!(
        f.references
            .iter()
            .filter(|r| r.source == node(&f, "generic").id && r.label == "Payload")
            .all(|r| r.candidate_keys.is_empty())
    );
    let query = facts(
        "ui/query.ts",
        "type Payload = import('./types').Payload; function after() {} ",
    );
    assert!(query.references.iter().any(|r| {
        r.relation == "references_type"
            && r.candidate_keys
                .contains(&"javascript:ui/types:Payload".into())
    }));
    assert_eq!(node(&query, "after").kind, "function");
    let f = facts(
        "ui/bad.ts",
        "import type {Base} from './types'; interface A extends Base {} class B extends mixin(Base) {} function f() { Base(); }",
    );
    assert!(calls(&f, "Base")[0].candidate_keys.is_empty());
    assert!(
        !f.references
            .iter()
            .any(|r| r.source == node(&f, "B").id && r.relation == "inherits")
    );
}

#[test]
fn typescript_same_stem_type_and_namespace_keys_keep_actual_file() {
    for path in ["foo.ts", "foo.mts"] {
        let f = facts(
            path,
            "export interface Shape {} export namespace API { export function run() {} } export function f(x: Shape) { API.run(); }",
        );
        assert!(
            f.references.iter().any(|r| r.label == "Shape"
                && r.candidate_keys == [format!("javascript:file:{path}:Shape")])
        );
        assert_eq!(
            calls(&f, "API.run")[0].candidate_keys,
            [format!("javascript:file:{path}:API.run")]
        );
    }
    let f = facts(
        "foo.tsx",
        "import View from './View'; function f() { View = other; return <View/>; }",
    );
    assert!(
        f.references
            .iter()
            .find(|r| r.relation == "uses_component")
            .unwrap()
            .candidate_keys
            .is_empty()
    );
}

#[test]
fn go_embedded_types_and_explicit_receiver_bindings() {
    let f = facts(
        "p/a.go",
        "package p\nimport ext \"example.org/types\"\ntype Base struct {}\ntype Item struct { *Base; Data ext.Payload; Values []ext.Payload }\ntype Reader interface { ext.Reader; Read(ext.Payload) Base }\nfunc (b *Base) Work() {}\nfunc (b *Base) Again() { b.Work() }\nfunc Run(b *Base, x ext.Payload, unknown interface{ Work() }) ext.Payload { b.Work(); x.Save(); unknown.Work(); { b := other; b.Work() }; return x }\nfunc Local() { var b Base; b.Work() }\n",
    );
    assert!(f.references.iter().any(|r| r.source == node(&f, "Item").id
        && r.relation == "embeds"
        && r.candidate_keys == ["go:p:p:Base"]));
    assert!(
        f.references
            .iter()
            .any(|r| r.source == node(&f, "Reader").id
                && r.relation == "embeds"
                && r.candidate_keys == ["go:import:example.org/types:Reader"])
    );
    assert!(f.references.iter().any(|r| r.relation == "field_type"
        && r.candidate_keys == ["go:import:example.org/types:Payload"]));
    let receiver_calls = calls(&f, "b.Work");
    assert_eq!(receiver_calls.len(), 4);
    assert_eq!(receiver_calls[0].candidate_keys, ["go:p:p:Base.Work"]);
    assert_eq!(receiver_calls[1].candidate_keys, ["go:p:p:Base.Work"]);
    assert!(receiver_calls[2].candidate_keys.is_empty());
    assert_eq!(receiver_calls[3].candidate_keys, ["go:p:p:Base.Work"]);
    assert_eq!(
        calls(&f, "x.Save")[0].candidate_keys,
        ["go:import:example.org/types:Payload.Save"]
    );
    assert!(calls(&f, "unknown.Work")[0].candidate_keys.is_empty());
}

#[test]
fn go_generic_or_reassigned_receivers_do_not_guess_concrete_types() {
    let f = facts(
        "p/a.go",
        "package p\ntype T struct {}\nfunc (x T) Work() {}\nfunc Generic[T any](x T) { x.Work() }\nfunc Slice(x []T) { x.Work() }\nfunc Mutated(x T) { x = other; x.Work() }\nfunc Inferred() { x := factory(); x.Work() }\n",
    );
    assert!(
        calls(&f, "x.Work")
            .iter()
            .all(|r| r.candidate_keys.is_empty())
    );
}

#[test]
fn rust_split_impl_self_and_type_trait_evidence() {
    let f = facts(
        "src/apply.rs",
        "impl State { pub fn apply(&self, data: Payload) -> Payload { self.save(); Self::new(); data } }\nuse crate::state::State; use crate::data::Payload;\ntrait Parent {} trait Child: Parent { fn read(&self, x: Payload) -> Payload; }\nimpl Child for State { fn read(&self, x: Payload) -> Payload { x } }\nstruct Wrapper<T> { generic: T, value: Payload }\n",
    );
    let method = f
        .nodes
        .iter()
        .find(|n| n.label == "apply" && n.kind == "method")
        .unwrap();
    assert_eq!(key(node(&f, "apply")), "rust:module:.:apply");
    assert_eq!(key(method), "rust:.:state::State::apply");
    assert_eq!(calls(&f, "self.save")[0].source, method.id);
    assert_eq!(method.metadata["impl_type"], "rust:.:state::State");
    assert_eq!(
        calls(&f, "self.save")[0].candidate_keys,
        ["rust:.:state::State::save"]
    );
    assert_eq!(
        calls(&f, "Self::new")[0].candidate_keys,
        ["rust:.:state::State::new"]
    );
    assert!(f.references.iter().any(|r| r.relation == "inherits"
        && r.label == "Parent"
        && r.candidate_keys == ["rust:.:apply::Parent"]));
    assert!(f.references.iter().any(|r| r.relation == "implements"
        && r.label == "Child"
        && r.candidate_keys == ["rust:.:apply::Child"]));
    assert!(f.references.iter().any(|r| r.relation == "field_type"
        && r.label == "Payload"
        && r.candidate_keys == ["rust:.:data::Payload"]));
    assert!(
        f.references
            .iter()
            .find(|r| r.relation == "field_type" && r.label == "T")
            .unwrap()
            .candidate_keys
            .is_empty()
    );
    assert!(
        f.nodes
            .iter()
            .filter(|n| n.label == "read")
            .all(|n| n.binding_key.is_none())
    );
}

#[test]
fn rust_ambiguous_imports_generics_and_public_use_remain_conservative() {
    let ambiguous = facts(
        "src/impls.rs",
        "use crate::a::State; use crate::b::State; impl State { fn run(&self) { self.save(); } }",
    );
    assert!(node(&ambiguous, "run").binding_key.is_none());
    assert!(calls(&ambiguous, "self.save")[0].candidate_keys.is_empty());
    let generic = facts(
        "src/lib.rs",
        "struct T; impl<T> T { fn run(&self) { self.save(); } }",
    );
    assert!(node(&generic, "run").binding_key.is_none());
    let exported = facts(
        "src/lib.rs",
        "pub use crate::private::{State as PublicState, work};",
    );
    assert_eq!(
        node(&exported, "PublicState").metadata["reexport_key"],
        "rust:.:PublicState"
    );
    assert!(node(&exported, "PublicState").binding_key.is_none());
    assert!(
        exported
            .references
            .iter()
            .any(|r| r.relation == "reexports" && r.candidate_keys == ["rust:.:private::State"])
    );
}

#[test]
fn javascript_comment_markers_and_normalized_citations_are_file_owned() {
    let source = "// NOTE: ADR1, adr-0001, Adr 1 and rfc2119\n// WHY: retain a bound\n// TODO: revisit\n// IMPORTANT: stable contract\n// HACK: compatibility\n// FIXME: narrow exception\n/** RATIONALE: RFC-2119; RFC 822 */\n// XADR2 ADR123456 ADR4suffix _RFC5 RFC6_ éADR7 ADR8é\nconst text = 'NOTE: ADR999 RFC999';\n";
    let f = facts("notes.ts", source);
    assert_eq!(f.nodes.iter().filter(|n| n.kind == "rationale").count(), 7);
    let mut citations: Vec<_> = f.nodes.iter().filter(|n| n.kind == "doc_ref").collect();
    citations.sort_by_key(|n| &n.label);
    assert_eq!(
        citations
            .iter()
            .map(|n| n.label.as_str())
            .collect::<Vec<_>>(),
        ["ADR-0001", "RFC-2119", "RFC-822"]
    );
    for n in citations {
        assert_eq!(n.file, "notes.ts");
        assert_eq!(n.metadata["citation_id"], n.label);
        let start = n.metadata["start_byte"].as_u64().unwrap() as usize;
        let end = n.metadata["end_byte"].as_u64().unwrap() as usize;
        assert_eq!(
            &source[start..end],
            n.metadata["spelling"].as_str().unwrap()
        );
        assert!(
            f.edges
                .iter()
                .any(|e| e.source == f.nodes[0].id && e.target == n.id && e.relation == "cites")
        );
    }
    let adr = node(&f, "ADR-0001");
    assert_eq!(
        f.edges
            .iter()
            .filter(|e| e.target == adr.id && e.relation == "cites")
            .count(),
        4
    );
    assert_ne!(
        adr.id,
        node(&facts("other.ts", "// NOTE: ADR1"), "ADR-0001").id
    );
}

#[test]
fn javascript_written_and_constructed_receivers_preserve_exact_class_members() {
    let f = facts(
        "service.ts",
        r#"
export class Service {
  run() { this.hidden(); }
  private hidden() {}
  static create() {}
}
class Consumer {
  constructor(private service: Service) {}
  invoke() { this.service.run(); }
}
export function use(service: Service) {
  service.run();
  const local = new Service();
  local.run();
  Service.create();
  return () => service.run();
}
"#,
    );
    assert_eq!(
        key(node(&f, "run")),
        "javascript:service:Service#instance.run"
    );
    assert_eq!(
        key(node(&f, "create")),
        "javascript:service:Service#static.create"
    );
    for label in ["service.run", "local.run", "this.service.run"] {
        assert!(!calls(&f, label).is_empty(), "missing {label}");
        for call in calls(&f, label) {
            assert_eq!(
                call.candidate_keys,
                ["javascript:file:service.ts:Service#instance.run"]
            );
        }
    }
    assert_eq!(
        calls(&f, "this.hidden")[0].candidate_keys,
        [key(node(&f, "hidden"))]
    );
    assert_eq!(
        calls(&f, "this.service.run")[0].source,
        node(&f, "invoke").id
    );
    assert!(
        calls(&f, "Service.create")[0]
            .candidate_keys
            .contains(&"javascript:file:service.ts:Service#static.create".into())
    );
    assert_ne!(calls(&f, "service.run")[1].source, node(&f, "use").id);
}

#[test]
fn javascript_receiver_evidence_stops_at_shadow_writes_and_dynamic_types() {
    let f = facts(
        "negative.ts",
        r#"
class Service { run() {} private hidden() {} static create() {} }
function unknown(service) { service.run(); }
function list(service: Service[]) { service.run(); }
function generic<Service>(service: Service) { service.run(); }
function replaced(service: Service) { service = other; service.run(); }
function factory() { const service = make(); service.run(); }
function nested(service: Service) { { const service = other; service.run(); } }
function duplicate() { class Service { run() {} } class Service { run() {} }
  const service = new Service(); service.run(); }
class Consumer { constructor(private service: Service) {}
  change() { this.service = other; this.service.run(); }
  nested() { return function () { this.service.run(); }; }
}
function privateAccess(service: Service) { service.hidden(); Service.run(); service.create(); }
"#,
    );
    assert!(
        calls(&f, "service.run")
            .iter()
            .all(|r| r.candidate_keys.is_empty())
    );
    assert!(
        calls(&f, "this.service.run")
            .iter()
            .all(|r| r.candidate_keys.is_empty())
    );
    assert!(
        !calls(&f, "service.hidden")[0]
            .candidate_keys
            .contains(&key(node(&f, "hidden")).into())
    );
    assert!(
        !calls(&f, "Service.run")[0]
            .candidate_keys
            .contains(&key(node(&f, "run")).into())
    );
    assert!(
        !calls(&f, "service.create")[0]
            .candidate_keys
            .contains(&key(node(&f, "create")).into())
    );
    for source in [
        "@replace class Service { run() {} } const x = new Service(); x.run();",
        "class Service { @replace run() {} } const x = new Service(); x.run();",
        "class Service { run() {} run() {} } const x = new Service(); x.run();",
        "class Service { run() {} [name]() {} } const x = new Service(); x.run();",
    ] {
        let f = facts("dynamic.ts", source);
        assert!(
            f.nodes
                .iter()
                .filter(|n| n.kind == "method")
                .all(|n| n.binding_key.is_none())
        );
    }
}

#[test]
fn typescript_receiver_imports_keep_the_written_origin() {
    let f = facts(
        "main.ts",
        r#"
import type {Service as External} from 'external';
import {Service as Local} from './service';
import * as api from './service';
class Service { run() {} }
function external(x: External) { x.run(); }
function local(x: Local) { x.run(); }
function namespaced(x: api.Service) { x.run(); api.Service.create(); }
class Consumer { constructor(private x: External) {} run() { this.x.run(); } }
"#,
    );
    let c = calls(&f, "x.run");
    assert_eq!(
        c[0].candidate_keys,
        ["javascript:import:external:Service#instance.run"]
    );
    assert_eq!(
        c[1].candidate_keys,
        [
            "javascript:service:Service#instance.run",
            "javascript:service/index:Service#instance.run"
        ]
    );
    assert_eq!(c[2].candidate_keys, c[1].candidate_keys);
    assert_eq!(
        calls(&f, "this.x.run")[0].candidate_keys,
        ["javascript:import:external:Service#instance.run"]
    );
    assert!(
        calls(&f, "api.Service.create")[0]
            .candidate_keys
            .contains(&"javascript:service:Service#static.create".into())
    );
}

#[test]
fn rust_const_static_declarations_keep_ranges_types_and_initializer_owners() {
    let source = r#"pub struct Value;
const fn make() -> Value { Value }
pub const DEFAULT: Value = make();
pub static mut CURRENT: Value = make();
impl Value { pub const EMPTY: Self = make(); }
fn local() { const LIMIT: usize = 3; LIMIT(); }
#[cfg(feature = "optional")]
pub const OPTIONAL: Value = make();
const _: () = ();
"#;
    let f = facts("src/lib.rs", source);
    for (label, kind, binding, line) in [
        ("DEFAULT", "constant", "rust:.:DEFAULT", 3),
        ("CURRENT", "static", "rust:.:CURRENT", 4),
        ("EMPTY", "constant", "rust:.:Value::EMPTY", 5),
    ] {
        let n = node(&f, label);
        assert_eq!(n.kind, kind);
        assert_eq!(key(n), binding);
        assert_eq!(n.line, Some(line));
        assert_eq!(n.metadata["public"], true);
        let start = n.metadata["start_byte"].as_u64().unwrap() as usize;
        let end = n.metadata["end_byte"].as_u64().unwrap() as usize;
        assert!(source[start..end].contains(label));
        assert!(source[start..end].ends_with(';'));
        assert!(f.references.iter().any(|r| r.source == n.id
            && r.relation == "references_type"
            && r.candidate_keys == ["rust:.:Value"]));
        assert!(
            calls(&f, "make")
                .iter()
                .any(|r| r.source == n.id && r.candidate_keys == ["rust:.:make"])
        );
    }
    assert_eq!(node(&f, "CURRENT").metadata["mutable"], true);
    assert_eq!(node(&f, "DEFAULT").metadata["mutable"], false);
    let limit = node(&f, "LIMIT");
    assert!(key(limit).starts_with("rust:local:src/lib.rs:"));
    assert!(f.edges.iter().any(|e| e.source == node(&f, "local").id
        && e.target == limit.id
        && e.relation == "contains"));
    assert!(calls(&f, "LIMIT")[0].candidate_keys.is_empty());
    assert!(node(&f, "OPTIONAL").binding_key.is_none());
    assert_eq!(node(&f, "OPTIONAL").metadata["conditional"], true);
    assert!(node(&f, "_").binding_key.is_none());
}

#[test]
fn written_contract_members_are_navigation_not_runtime_calls() {
    use graf::{model::Coverage, store::Store};

    for (path, source, replacement, member, caller, negatives) in [
        (
            "contract.ts",
            r#"export interface Channel { send(): number }
class First { send() { return 1; } }
class Second { send() { return 2; } }
function invoke(value: Channel) { return value.send(); }
function shadow(value: Channel) { { let value: unknown; value.send(); } }
function changed(value: Channel, other: Channel) { value = other; value.send(); }
function structural(value: { send(): number }) { value.send(); }
function staticUse() { Channel.send(); }
function constructed() { const value = new Channel(); value.send(); }
"#,
            "export interface Channel { receive(): number } function invoke(value: Channel) { value.send(); }",
            "send",
            "invoke",
            vec![
                "shadow",
                "changed",
                "structural",
                "staticUse",
                "constructed",
            ],
        ),
        (
            "contract.go",
            r#"package wire
type Channel interface { Send() int }
type First struct {}
func (First) Send() int { return 1 }
type Second struct {}
func (Second) Send() int { return 2 }
func invoke(value Channel) int { return value.Send() }
func shadow(value Channel) { { value := unknown; value.Send() } }
func changed(value Channel, other Channel) { value = other; value.Send() }
func structural(value interface { Send() int }) { value.Send() }
func staticUse() { Channel.Send() }
func pointer(value *Channel) { value.Send() }
"#,
            "package wire\ntype Channel interface { Receive() int }\nfunc invoke(value Channel) { value.Send() }",
            "Send",
            "invoke",
            vec!["shadow", "changed", "structural", "staticUse", "pointer"],
        ),
        (
            "src/lib.rs",
            r#"trait Channel { fn send(&self) -> i32; }
struct First;
impl Channel for First { fn send(&self) -> i32 { 1 } }
struct Second;
impl Channel for Second { fn send(&self) -> i32 { 2 } }
fn invoke(value: &dyn Channel) -> i32 { value.send() }
fn shadow(value: &dyn Channel) { let value = unknown; value.send(); }
fn changed(mut value: &dyn Channel, other: &dyn Channel) { value = other; value.send(); }
fn structural<T>(value: T) { value.send(); }
fn static_use() { Channel::send(); }
"#,
            "trait Channel { fn receive(&self); } fn invoke(value: &dyn Channel) { value.send(); }",
            "send",
            "invoke",
            vec!["shadow", "changed", "structural", "static_use"],
        ),
    ] {
        let original = facts(path, source);
        let contract = node(&original, "Channel");
        let target = original
            .nodes
            .iter()
            .find(|n| {
                n.label == member
                    && original.edges.iter().any(|e| {
                        e.relation == "contains" && e.source == contract.id && e.target == n.id
                    })
            })
            .unwrap();
        assert!(
            target.binding_key.is_none(),
            "{path}: signature became callable"
        );
        let target = target.id.clone();
        let source = node(&original, caller).id.clone();
        let forbidden: Vec<_> = negatives
            .iter()
            .map(|name| node(&original, name).id.clone())
            .collect();
        let directory = tempfile::tempdir().unwrap();
        let mut store = Store::create(&directory.path().join("graph.db")).unwrap();
        store
            .apply_native("fixture", vec![original], vec![], Coverage::default())
            .unwrap();
        let snapshot = store.snapshot().unwrap();
        let navigation: Vec<_> = snapshot
            .edges
            .iter()
            .filter(|e| {
                e.source == source && matches!(e.relation.as_str(), "calls" | "declared_member")
            })
            .collect();
        assert_eq!(navigation.len(), 1, "{path}: {navigation:?}");
        assert_eq!(
            (&navigation[0].relation, &navigation[0].target),
            (&"declared_member".to_owned(), &target)
        );
        assert!(
            !snapshot.edges.iter().any(|e| forbidden.contains(&e.source)
                && matches!(e.relation.as_str(), "calls" | "declared_member")),
            "{path}: {:?}",
            snapshot.edges
        );
        store
            .apply_native(
                "fixture",
                vec![facts(path, replacement)],
                vec![],
                Coverage::default(),
            )
            .unwrap();
        let snapshot = store.snapshot().unwrap();
        assert!(!snapshot.nodes.iter().any(|n| n.id == target), "{path}");
        assert!(
            !snapshot
                .edges
                .iter()
                .any(|e| matches!(e.relation.as_str(), "calls" | "declared_member")),
            "{path}: {:?}",
            snapshot.edges
        );
    }
}

#[test]
fn imported_typescript_interface_member_keeps_its_written_declaration() {
    use graf::{model::Coverage, store::Store};
    let contract = facts("channel.ts", "export interface Channel { send(): number }");
    let target = node(&contract, "send").id.clone();
    let caller = facts(
        "entry.ts",
        "import type { Channel as Port } from './channel'; function invoke(value: Port) { return value.send(); }",
    );
    let source = node(&caller, "invoke").id.clone();
    let directory = tempfile::tempdir().unwrap();
    let mut store = Store::create(&directory.path().join("graph.db")).unwrap();
    store
        .apply_native(
            "fixture",
            vec![contract, caller],
            vec![],
            Coverage::default(),
        )
        .unwrap();
    let snapshot = store.snapshot().unwrap();
    assert!(
        snapshot
            .edges
            .iter()
            .any(|e| e.source == source && e.target == target && e.relation == "declared_member")
    );
    assert!(
        !snapshot
            .edges
            .iter()
            .any(|e| e.source == source && e.relation == "calls")
    );
    store
        .apply_native(
            "fixture",
            vec![facts(
                "channel.ts",
                "export interface Channel { receive(): number }",
            )],
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
            .any(|e| e.source == source
                && matches!(e.relation.as_str(), "calls" | "declared_member"))
    );
}

#[test]
fn ambiguous_or_conditional_contract_signatures_stay_unresolved() {
    use graf::{model::Coverage, store::Store};
    for (path, source) in [
        (
            "duplicate.ts",
            "interface Channel { send(): void; send(x: number): void } function invoke(value: Channel) { value.send(); }",
        ),
        (
            "optional.ts",
            "interface Channel { send?(): void } function invoke(value: Channel) { value.send(); }",
        ),
        (
            "duplicate.go",
            "package wire\ntype Channel interface { Send(); Send(int) }\nfunc invoke(value Channel) { value.Send() }",
        ),
        (
            "src/lib.rs",
            "trait Channel { fn send(&self); fn send(&self, x: i32); } fn invoke(value: &dyn Channel) { value.send(); }",
        ),
        (
            "src/lib.rs",
            "#[cfg(feature = \"extra\")] trait Channel { fn send(&self); } fn invoke(value: &dyn Channel) { value.send(); }",
        ),
        (
            "src/lib.rs",
            "trait Channel { #[cfg(feature = \"extra\")] fn send(&self); } fn invoke(value: &dyn Channel) { value.send(); }",
        ),
        (
            "src/lib.rs",
            "trait Channel { fn send(); } fn invoke(value: &dyn Channel) { value.send(); }",
        ),
    ] {
        let file = facts(path, source);
        let caller = node(&file, "invoke").id.clone();
        let directory = tempfile::tempdir().unwrap();
        let mut store = Store::create(&directory.path().join("graph.db")).unwrap();
        store
            .apply_native("fixture", vec![file], vec![], Coverage::default())
            .unwrap();
        assert!(
            !store
                .snapshot()
                .unwrap()
                .edges
                .iter()
                .any(|e| e.source == caller
                    && matches!(e.relation.as_str(), "calls" | "declared_member")),
            "{path}: {source}"
        );
    }
}

#[test]
fn rust_simple_generic_impls_keep_exact_self_and_scoped_self_keys() {
    let provider = facts(
        "src/provider.rs",
        "use crate::model::Register; impl<Key, Value> Register<Key, Value> { pub fn inspect(&self) {} pub fn empty() {} }",
    );
    let caller = facts(
        "src/consumer.rs",
        "use crate::model::Register as Table; impl<Left, Right> Table<Left, Right> { pub fn visit(&self) { self.inspect(); Self::empty(); } }",
    );
    assert_eq!(node(&provider, "inspect").metadata["generic_impl_arity"], 2);
    assert_eq!(
        node(&caller, "visit").metadata["generic_impl_type"],
        "rust:.:model::Register"
    );
    assert_eq!(
        calls(&caller, "self.inspect")[0].candidate_keys,
        [key(node(&provider, "inspect"))]
    );
    assert_eq!(
        calls(&caller, "Self::empty")[0].candidate_keys,
        [key(node(&provider, "empty"))]
    );
    let model = facts("src/model.rs", "pub struct Register<A, B>(pub A, pub B);");
    assert_eq!(node(&model, "Register").metadata["generic_type_arity"], 2);
}

#[test]
fn rust_restricted_impl_headers_do_not_erase_type_constraints() {
    for header in [
        "impl<T: Copy> Register<T>",
        "impl<T> Register<T> where T: Copy",
        "impl<T> Inspect for Register<T>",
        "impl<'a, T> Register<'a, T>",
        "impl<const SIZE: usize> Register<SIZE>",
        "impl<T> Register<Option<T>>",
        "impl Register<u16>",
        "impl<T> Register<T, T>",
        "impl<A, B> Register<B, A>",
    ] {
        let source = format!(
            "fn helper() {{}} {header} {{ fn inspect(&self) {{}} fn visit(&self) {{ self.inspect(); Self::inspect(self); helper(); }} }}"
        );
        let file = facts("src/lib.rs", &source);
        assert!(node(&file, "inspect").binding_key.is_none(), "{header}");
        assert!(node(&file, "visit").binding_key.is_none(), "{header}");
        for spelling in ["self.inspect", "Self::inspect"] {
            assert!(
                calls(&file, spelling)[0].candidate_keys.is_empty(),
                "{header}: {spelling}"
            );
        }
        assert_eq!(
            calls(&file, "helper")[0].candidate_keys,
            [key(node(&file, "helper"))],
            "{header}"
        );
    }
}

#[test]
fn go_interface_requirements_keep_direct_members_and_embeds() {
    use graf::{model::Coverage, store::Store};
    use std::collections::BTreeSet;

    let file = facts(
        "poll/api.go",
        "package poll\ntype Packet struct {}\ntype Poller interface { Poll() Packet; Reset() }\ntype SealedPoller interface { Poller; Seal() }\ntype Numeric interface { ~int | ~int64 }\n",
    );
    let poller = node(&file, "Poller").id.clone();
    let sealed = node(&file, "SealedPoller").id.clone();
    let numeric = node(&file, "Numeric").id.clone();
    for (owner, expected) in [(&poller, vec!["Poll", "Reset"]), (&sealed, vec!["Seal"])] {
        let members: Vec<_> =
            file.nodes
                .iter()
                .filter(|n| {
                    n.kind == "method"
                        && file.edges.iter().any(|e| {
                            e.relation == "contains" && &e.source == owner && e.target == n.id
                        })
                })
                .collect();
        assert_eq!(members.len(), expected.len());
        assert_eq!(
            members
                .iter()
                .map(|n| n.label.as_str())
                .collect::<BTreeSet<_>>(),
            expected.into_iter().collect::<BTreeSet<_>>()
        );
        assert!(members.iter().all(|n| n.binding_key.is_none()));
    }
    assert!(file.references.iter().any(|r| {
        r.source == node(&file, "Poll").id
            && r.relation == "references_type"
            && r.candidate_keys == [key(node(&file, "Packet"))]
    }));
    assert!(
        !file
            .references
            .iter()
            .any(|r| r.source == numeric && r.relation == "embeds")
    );

    let directory = tempfile::tempdir().unwrap();
    let mut store = Store::create(&directory.path().join("graph.db")).unwrap();
    store
        .apply_native("fixture", vec![file], vec![], Coverage::default())
        .unwrap();
    let graph = store.snapshot().unwrap();
    let embeds: Vec<_> = graph
        .edges
        .iter()
        .filter(|e| e.relation == "embeds")
        .collect();
    assert_eq!(embeds.len(), 1);
    assert_eq!((&embeds[0].source, &embeds[0].target), (&sealed, &poller));
}

#[test]
fn go_receiver_methods_browse_incoming_types_without_duplicate_ownership() {
    use graf::{
        model::{Coverage, Direction, QueryOptions},
        store::Store,
    };
    use std::collections::BTreeSet;

    let types = facts(
        "poll/types.go",
        "package poll\ntype Packet struct {}\ntype Poller interface { Poll() Packet }\ntype Device struct {}\ntype Other struct {}\n",
    );
    let methods = facts(
        "poll/methods.go",
        r#"package poll
func (value Device) Poll() Packet { return Packet{} }
func (value *Device) Reset() {}
func (value Other) Poll() Packet { return Packet{} }
func invoke(value Poller) Packet { return value.Poll() }
func shadow(value Poller) { { value := struct { Poll func() Packet }{}; value.Poll() } }
func changed(value Poller, other Poller) { value = other; value.Poll() }
func unknown(value interface { Poll() Packet }) { value.Poll() }
"#,
    );
    let device = node(&types, "Device").id.clone();
    let contract = node(&types, "Poller").id.clone();
    let requirement = node(&types, "Poll").id.clone();
    let method_ids: BTreeSet<_> = ["go:poll:poll:Device.Poll", "go:poll:poll:Device.Reset"]
        .into_iter()
        .map(|binding| {
            methods
                .nodes
                .iter()
                .find(|n| n.binding_key.as_deref() == Some(binding))
                .unwrap()
                .id
                .clone()
        })
        .collect();
    assert!(!method_ids.contains(&requirement));
    assert!(node(&types, "Poll").binding_key.is_none());
    assert!(
        types.edges.iter().any(|e| {
            e.relation == "contains" && e.source == contract && e.target == requirement
        })
    );
    let file_owner = methods.nodes[0].id.clone();
    for method in &method_ids {
        let owners: Vec<_> = methods
            .edges
            .iter()
            .filter(|e| e.relation == "contains" && &e.target == method)
            .collect();
        assert_eq!(owners.len(), 1);
        assert_eq!(owners[0].source, file_owner);
        assert!(methods.references.iter().any(|r| {
            &r.source == method
                && r.relation == "receiver_type"
                && r.candidate_keys == [key(node(&types, "Device"))]
        }));
    }
    let invoke = node(&methods, "invoke").id.clone();
    let negatives: Vec<_> = ["shadow", "changed", "unknown"]
        .into_iter()
        .map(|name| node(&methods, name).id.clone())
        .collect();
    let directory = tempfile::tempdir().unwrap();
    let mut store = Store::create(&directory.path().join("graph.db")).unwrap();
    store
        .apply_native("fixture", vec![types, methods], vec![], Coverage::default())
        .unwrap();
    let incoming = QueryOptions {
        direction: Direction::Incoming,
        relation: Some("receiver_type".into()),
        ..Default::default()
    };
    let browse = store.neighbors(&device, &incoming).unwrap();
    assert!(!browse.truncated);
    assert_eq!(browse.edges.len(), method_ids.len());
    assert_eq!(
        browse
            .edges
            .iter()
            .map(|e| e.source.clone())
            .collect::<BTreeSet<_>>(),
        method_ids
    );
    assert!(
        browse
            .edges
            .iter()
            .all(|e| e.target == device && e.relation == "receiver_type")
    );
    let graph = store.snapshot().unwrap();
    for method in &method_ids {
        let owners: Vec<_> = graph
            .edges
            .iter()
            .filter(|e| e.relation == "contains" && &e.target == method)
            .collect();
        assert_eq!(owners.len(), 1);
        assert_eq!(owners[0].source, file_owner);
        assert!(
            !graph
                .edges
                .iter()
                .any(|e| e.source == device && &e.target == method)
        );
    }
    let navigation: Vec<_> = graph
        .edges
        .iter()
        .filter(|e| {
            e.source == invoke && matches!(e.relation.as_str(), "calls" | "declared_member")
        })
        .collect();
    assert_eq!(navigation.len(), 1);
    assert_eq!(navigation[0].relation, "declared_member");
    assert_eq!(navigation[0].target, requirement);
    for source in std::iter::once(&invoke).chain(negatives.iter()) {
        let unresolved = store
            .neighbors(
                source,
                &QueryOptions {
                    direction: Direction::Outgoing,
                    relation: Some("calls".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(unresolved.edges.is_empty());
        assert!(
            unresolved
                .unresolved
                .iter()
                .any(|r| &r.source == source && r.label == "value.Poll")
        );
    }
    assert!(!graph.edges.iter().any(|e| negatives.contains(&e.source)
        && matches!(e.relation.as_str(), "calls" | "declared_member")));

    // A second receiver declaration removes the browsing link; lexical owners survive.
    let duplicate = facts("poll/duplicate.go", "package poll\ntype Device struct {}\n");
    let duplicate_id = node(&duplicate, "Device").id.clone();
    store
        .apply_native("fixture", vec![duplicate], vec![], Coverage::default())
        .unwrap();
    for receiver in [&device, &duplicate_id] {
        assert!(
            store
                .neighbors(receiver, &incoming)
                .unwrap()
                .edges
                .is_empty()
        );
    }
    for method in &method_ids {
        let unresolved = store
            .neighbors(
                method,
                &QueryOptions {
                    direction: Direction::Outgoing,
                    relation: Some("receiver_type".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(unresolved.unresolved.len(), 1);
        assert!(unresolved.edges.is_empty());
        assert!(unresolved.unresolved[0].reason.contains("ambiguous"));
    }
    store
        .apply_native(
            "fixture",
            vec![],
            vec!["poll/duplicate.go".into()],
            Coverage::default(),
        )
        .unwrap();
    assert_eq!(
        store
            .neighbors(&device, &incoming)
            .unwrap()
            .edges
            .iter()
            .map(|e| e.source.clone())
            .collect::<BTreeSet<_>>(),
        method_ids
    );
}
