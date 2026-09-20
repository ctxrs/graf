use graf::{
    languages::parse,
    model::{Coverage, FileFacts, Node, Reference},
    store::Store,
};

fn facts(path: &str, source: &str) -> FileFacts {
    let facts = parse(path, source, "fixture").unwrap().unwrap();
    assert!(facts.diagnostics.is_empty(), "{:?}", facts.diagnostics);
    facts
}

fn node<'a>(facts: &'a FileFacts, qualified: &str) -> &'a Node {
    facts
        .nodes
        .iter()
        .find(|n| n.qualified_name.as_deref() == Some(qualified))
        .unwrap_or_else(|| panic!("missing {qualified}"))
}

fn callback_references(facts: &FileFacts) -> Vec<&Reference> {
    facts
        .references
        .iter()
        .filter(|r| r.reason.starts_with("callback argument;"))
        .inspect(|r| assert_eq!(r.relation, "references"))
        .collect()
}

fn assert_callback_site(source: &str, reference: &Reference, start: usize) {
    let end = start + reference.label.len();
    assert_eq!(&source[start..end], reference.label);
    assert_eq!(
        reference.id,
        format!("references:{}:{start}-{end}", reference.source)
    );
    assert_eq!(
        reference.line as usize,
        source[..start].bytes().filter(|b| *b == b'\n').count() + 1
    );
    assert!(reference.reason.contains("invocation is not implied"));
}

#[test]
fn callback_value_references_respect_block_loop_var_and_parameter_scopes() {
    let source = r#"export function transform() {}
function block(queue) {
  { const transform = 0; queue.offer(transform); }
  queue.offer(transform);
}
function forOf(queue, values) {
  for (const transform of values) { queue.offer(transform); }
  queue.offer(transform);
}
function cStyle(queue) {
  for (let transform = 0; transform < 1; transform++) { queue.offer(transform); }
  queue.offer(transform);
}
function varBlock(queue) {
  { var transform = 0; }
  queue.offer(transform);
}
function varLoop(queue, values) {
  for (var transform of values) { queue.offer(transform); }
  queue.offer(transform);
}
function parameter(queue, transform) { queue.offer(transform); }
function genuine(queue) { queue.offer(transform); }
"#;
    let expected = [
        ("block", false),
        ("block", true),
        ("forOf", false),
        ("forOf", true),
        ("cStyle", false),
        ("cStyle", true),
        ("varBlock", false),
        ("varLoop", false),
        ("varLoop", false),
        ("parameter", false),
        ("genuine", true),
    ];
    for path in ["scopes.js", "scopes.ts"] {
        let facts = facts(path, source);
        let references = callback_references(&facts);
        assert_eq!(references.len(), expected.len());
        let sites: Vec<_> = source.match_indices("queue.offer(transform)").collect();
        assert_eq!(sites.len(), expected.len());
        for ((offset, _), (owner, resolved)) in sites.into_iter().zip(expected) {
            let start = offset + "queue.offer(".len();
            let id = format!(
                "references:{}:{start}-{}",
                node(&facts, owner).id,
                start + "transform".len()
            );
            let reference = references.iter().find(|r| r.id == id).unwrap();
            assert_callback_site(source, reference, start);
            assert_eq!(reference.file, path);
            assert_eq!(reference.source, node(&facts, owner).id);
            assert_eq!(reference.label, "transform");
            let keys = if resolved {
                vec![format!("javascript:file:{path}:transform")]
            } else {
                vec![]
            };
            assert_eq!(reference.candidate_keys, keys, "{path}: {owner}:{start}");
        }
        assert!(!facts.references.iter().any(|r| {
            r.label == "transform" && matches!(r.relation.as_str(), "calls" | "declared_callee")
        }));
    }
}

#[test]
fn callback_value_references_use_final_write_and_dynamic_scope_checks() {
    for source in [
        "export function transform() {} function use() { queue.offer(transform); } transform = other;",
        "transform = other; export function transform() {} function use() { queue.offer(transform); }",
        "export function transform() {} function use() { queue.offer(transform); } function replace() { transform = other; }",
        "export function transform() {} function use() { queue.offer(transform); eval(code); }",
        "export function transform() {} with (context) { queue.offer(transform); }",
        "export function transform() {} export function transform() {} queue.offer(transform);",
    ] {
        for path in ["writes.js", "writes.ts"] {
            let facts = facts(path, source);
            let references: Vec<_> = callback_references(&facts)
                .into_iter()
                .filter(|r| r.label == "transform")
                .collect();
            assert_eq!(references.len(), 1, "{path}: {source}");
            assert!(references[0].candidate_keys.is_empty(), "{path}: {source}");
            assert_callback_site(
                source,
                references[0],
                source.find("queue.offer(transform)").unwrap() + "queue.offer(".len(),
            );
        }
    }
}

#[test]
fn callback_value_references_preserve_anonymous_owners_and_each_argument_site() {
    let source = r#"export function transform() {}
function make(queue, values) {
  for (const transform of values) { queue.wrap(() => queue.offer(transform)); }
  return () => { queue.offer(transform, transform); transform(); };
}
"#;
    for path in ["closures.js", "closures.ts"] {
        let facts = facts(path, source);
        let references = callback_references(&facts);
        assert_eq!(references.len(), 3);
        let arrows: Vec<_> = facts
            .nodes
            .iter()
            .filter(|n| n.kind == "function" && n.label.starts_with("<anonymous@"))
            .collect();
        assert_eq!(arrows.len(), 2);
        let sites = [
            ("() => queue.offer", "queue.offer(transform)", false),
            ("() => {", "queue.offer(transform, transform)", true),
        ];
        for (arrow, call, resolved) in sites {
            let arrow_start = source.find(arrow).unwrap();
            let owner = arrows
                .iter()
                .find(|n| n.metadata["start_byte"] == arrow_start)
                .unwrap();
            let owned: Vec<_> = references.iter().filter(|r| r.source == owner.id).collect();
            assert_eq!(owned.len(), if resolved { 2 } else { 1 });
            for (index, reference) in owned.iter().enumerate() {
                let start =
                    source.find(call).unwrap() + "queue.offer(".len() + index * "transform, ".len();
                assert_callback_site(source, reference, start);
                assert_ne!(reference.source, node(&facts, "make").id);
                assert_eq!(
                    reference.candidate_keys,
                    if resolved {
                        vec![format!("javascript:file:{path}:transform")]
                    } else {
                        vec![]
                    }
                );
            }
        }
        let calls: Vec<_> = facts
            .references
            .iter()
            .filter(|r| r.label == "transform" && r.relation == "calls")
            .collect();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].source, references[1].source);
        assert_eq!(
            calls[0].candidate_keys,
            [format!("javascript:file:{path}:transform")]
        );
    }
}

#[test]
fn callback_value_references_store_only_proved_functions_and_retract_stale_links() {
    let source = r#"import { remote } from './provider';
export function transform() {}
const local = () => {};
const data = 1;
const produced = factory();
class Shape {}
function use(queue) {
  queue.offer(transform, transform, local, data, produced, remote, unknown, Shape);
  queue.offer(object.transform, table[transform], ...values, () => {});
  transform();
}
"#;
    for path in ["values.js", "values.ts"] {
        let original = facts(path, source);
        let references = callback_references(&original);
        assert_eq!(references.len(), 8);
        for reference in &references {
            assert_eq!(reference.source, node(&original, "use").id);
            let expected = match reference.label.as_str() {
                "transform" => vec![format!("javascript:file:{path}:transform")],
                "local" => vec![node(&original, "local").binding_key.clone().unwrap()],
                "data" | "produced" | "remote" | "unknown" | "Shape" => vec![],
                other => panic!("unexpected callback: {other}"),
            };
            assert_eq!(reference.candidate_keys, expected);
        }
        assert!(
            !original
                .references
                .iter()
                .any(|r| r.relation == "declared_callee")
        );
        let provenance: Vec<_> = references
            .iter()
            .filter(|r| !r.candidate_keys.is_empty())
            .map(|r| (r.id.clone(), r.line, r.label.clone()))
            .collect();
        let directory = tempfile::tempdir().unwrap();
        let mut store = Store::create(&directory.path().join("graph.db")).unwrap();
        store
            .apply_native(
                "fixture",
                vec![
                    original.clone(),
                    facts("provider.js", "export function remote() {}"),
                ],
                vec![],
                Coverage::default(),
            )
            .unwrap();
        let graph = store.snapshot().unwrap();
        let edges: Vec<_> = graph
            .edges
            .iter()
            .filter(|e| e.relation == "references")
            .collect();
        assert_eq!(edges.len(), 3);
        for (id, line, label) in provenance {
            let edge = edges
                .iter()
                .find(|e| e.metadata["reference_id"] == id)
                .unwrap();
            assert_eq!(edge.id, format!("reference:{id}"));
            assert_eq!(edge.source, node(&original, "use").id);
            assert_eq!(edge.target, node(&original, &label).id);
            assert_eq!(edge.file.as_deref(), Some(path));
            assert_eq!(edge.line, Some(line));
        }
        let calls: Vec<_> = graph
            .edges
            .iter()
            .filter(|e| e.relation == "calls")
            .collect();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].target, node(&original, "transform").id);

        for (changed, count) in [
            (format!("{source}\ntransform = other; local = other;"), 0),
            (source.to_owned(), 3),
            (source.replace("export function transform() {}", ""), 1),
            (source.to_owned(), 3),
        ] {
            store
                .apply_native(
                    "fixture",
                    vec![facts(path, &changed)],
                    vec![],
                    Coverage::default(),
                )
                .unwrap();
            let graph = store.snapshot().unwrap();
            let edges: Vec<_> = graph
                .edges
                .iter()
                .filter(|e| e.relation == "references")
                .collect();
            assert_eq!(edges.len(), count);
            assert!(!edges.iter().any(|e| {
                graph
                    .nodes
                    .iter()
                    .any(|n| n.id == e.target && n.label == "remote")
            }));
            if count == 1 {
                assert_eq!(edges[0].target, node(&facts(path, &changed), "local").id);
            }
            assert_eq!(
                graph.edges.iter().filter(|e| e.relation == "calls").count(),
                usize::from(count == 3)
            );
        }
        store
            .apply_native("fixture", vec![], vec![path.into()], Coverage::default())
            .unwrap();
        let graph = store.snapshot().unwrap();
        assert!(!graph.nodes.iter().any(|n| n.file == path));
        assert!(
            !graph
                .edges
                .iter()
                .any(|e| matches!(e.relation.as_str(), "references" | "calls"))
        );
    }
}

#[test]
fn property_writes_preserve_method_declarations_without_runtime_dispatch() {
    let source = r#"
export class Assembly extends Base {
  make() { return 1; }
  queue() {
    const item = this.make();
    this.pending = item;
    return this.make();
  }
}
class Replacement extends Assembly { make() { return 2; } }
"#;
    let facts = facts("assembly.js", source);
    let caller = node(&facts, "Assembly.queue").id.clone();
    let target = node(&facts, "Assembly.make").id.clone();
    let calls: Vec<_> = facts
        .references
        .iter()
        .filter(|r| r.source == caller && r.relation == "calls" && r.label == "this.make")
        .collect();
    assert_eq!(calls.len(), 2);
    assert!(calls.iter().all(|r| r.candidate_keys.is_empty()));
    let declarations: Vec<_> = facts
        .references
        .iter()
        .filter(|r| r.source == caller && r.relation == "declared_member")
        .collect();
    assert_eq!(declarations.len(), 2);
    for r in declarations {
        assert_eq!(
            r.candidate_keys,
            ["javascript:file:assembly.js:Assembly#instance.make"]
        );
    }
    let directory = tempfile::tempdir().unwrap();
    let mut store = Store::create(&directory.path().join("graph.db")).unwrap();
    store
        .apply_native("fixture", vec![facts], vec![], Coverage::default())
        .unwrap();
    let snapshot = store.snapshot().unwrap();
    let edges: Vec<_> = snapshot
        .edges
        .iter()
        .filter(|e| {
            e.source == caller && matches!(e.relation.as_str(), "calls" | "declared_member")
        })
        .collect();
    assert_eq!(edges.len(), 2);
    assert!(
        edges
            .iter()
            .all(|e| e.relation == "declared_member" && e.target == target)
    );
}

#[test]
fn this_declaration_evidence_obeys_lexical_scope_and_member_kind() {
    for write in [
        "this.flag = 1;",
        "this.flag++;",
        "[this.flag] = values;",
        "this[key] = value;",
        "this.make = replacement;",
        "this.child.value = value;",
    ] {
        let source = format!(
            "class Assembly {{ make() {{}} run() {{ {write} return () => this.make(); }} }}"
        );
        let facts = facts("assembly.js", &source);
        assert_eq!(
            facts
                .references
                .iter()
                .filter(|r| r.relation == "declared_member")
                .count(),
            1,
            "{source}"
        );
        assert!(
            facts
                .references
                .iter()
                .filter(|r| r.label == "this.make" && r.relation == "calls")
                .all(|r| r.candidate_keys.is_empty())
        );
    }
    let static_facts = facts(
        "assembly.js",
        "class Assembly { make() {} static make() {} static run() { this.flag = 1; this.make(); } }",
    );
    let declaration = static_facts
        .references
        .iter()
        .find(|r| r.relation == "declared_member")
        .unwrap();
    assert_eq!(
        declaration.candidate_keys,
        ["javascript:local:assembly.js::Assembly#static.make"]
    );
    let static_method = static_facts
        .nodes
        .iter()
        .find(|n| n.label == "make" && n.metadata["static"] == true)
        .unwrap();
    assert_eq!(
        static_method.binding_key.as_deref(),
        Some("javascript:local:assembly.js::Assembly#static.make")
    );
    assert!(
        static_facts
            .references
            .iter()
            .filter(|r| r.label == "this.make" && r.relation == "calls")
            .all(|r| r.candidate_keys.is_empty())
    );

    for source in [
        "class Assembly { make() {} run() { this.flag = 1; return function () { this.make(); }; } }",
        "class Assembly { static make() {} run() { this.flag = 1; this.make(); } }",
        "class Assembly { make() {} make() {} run() { this.flag = 1; this.make(); } }",
        "class Assembly { get make() { return replacement; } run() { this.flag = 1; this.make(); } }",
        "class Assembly { make() {} [key]() {} run() { this.flag = 1; this.make(); } }",
        "class Assembly { @decorate make() {} run() { this.flag = 1; this.make(); } }",
        "class Assembly { make() {} run() { this.flag = 1; eval(code); this.make(); } }",
        "class Assembly { make() {} run() { this.flag = 1; this.missing(); } }",
        "class Assembly { make() {} run() { this.flag = 1; this[key](); } }",
        "class Service { make() {} } class Assembly { service: Service; run() { this.service = other; this.service.make(); } }",
    ] {
        let facts = facts("assembly.ts", source);
        assert!(
            !facts
                .references
                .iter()
                .any(|r| r.relation == "declared_member"),
            "{source}"
        );
        assert!(
            facts
                .references
                .iter()
                .filter(|r| r.relation == "calls" && r.label.starts_with("this"))
                .all(|r| r.candidate_keys.is_empty()),
            "{source}"
        );
    }
}

#[test]
fn variance_modifiers_recover_declarations_with_original_source_ranges() {
    let source = r#"// π keeps byte offsets distinct from character offsets
import type { Payload } from './types';
export interface Source<out Item = Payload> { read(): Item; }
export interface Sink<in Item> { write(value: Item): void; }
export interface Cell<in /* variance */ out Item extends Payload = Payload> { value: Item; }
export type Consumer<in Item> = (value: Item) => void;
export class Holder<out Item> { value!: Item; }
export abstract class Cache<out Item> { abstract read(): Item; }
export function after() { return 7; }
"#;
    for path in [
        "variance.ts",
        "variance.tsx",
        "variance.mts",
        "variance.cts",
    ] {
        let facts = facts(path, source);
        for name in [
            "Source", "Sink", "Cell", "Consumer", "Holder", "Cache", "after",
        ] {
            let n = node(&facts, name);
            assert!(n.binding_key.is_some(), "{path}: {name}");
            let start = n.metadata["start_byte"].as_u64().unwrap() as usize;
            let end = n.metadata["end_byte"].as_u64().unwrap() as usize;
            let original = &source[start..end];
            assert!(original.contains(name));
            assert_eq!(
                n.line.unwrap() as usize,
                source[..start].bytes().filter(|b| *b == b'\n').count() + 1
            );
        }
        let cell = node(&facts, "Cell");
        assert_eq!(cell.line, Some(5));
        let after = node(&facts, "after");
        assert_eq!(
            &source[after.metadata["start_byte"].as_u64().unwrap() as usize
                ..after.metadata["end_byte"].as_u64().unwrap() as usize],
            "function after() { return 7; }"
        );
        // An extensionless import retains both file and directory candidates;
        // the project resolver, not syntax extraction, chooses between them.
        for (owner, count) in [("Source", 1), ("Cell", 2)] {
            let references: Vec<_> = facts
                .references
                .iter()
                .filter(|r| {
                    r.source == node(&facts, owner).id
                        && r.relation == "references_type"
                        && r.label == "Payload"
                })
                .collect();
            assert_eq!(references.len(), count, "{path}: {owner}");
            for reference in references {
                assert_eq!(
                    reference.candidate_keys,
                    ["javascript:types:Payload", "javascript:types/index:Payload"],
                    "{path}: {owner}"
                );
            }
        }
        assert!(
            facts
                .references
                .iter()
                .filter(|r| r.label == "Item")
                .all(|r| r.candidate_keys.is_empty())
        );
    }
}

#[test]
fn variance_fallback_keeps_parameter_shadowing_comments_and_strings() {
    let source = r#"
import type { Value } from './types';
interface Box<out Value> { get(): Value; }
type Pair<out A extends Map<string, number>, in B = string> = (value: B) => A;
interface Names<out, input> { value: out; other: input; }
const text = "interface Fake<out T> { broken(: }";
// interface Comment<out T> { broken(: }
function finish() { return text; }
"#;
    let facts = facts("variance.ts", source);
    assert!(
        !facts
            .nodes
            .iter()
            .any(|n| matches!(n.label.as_str(), "Fake" | "Comment"))
    );
    assert!(
        facts
            .references
            .iter()
            .filter(|r| r.label == "Value")
            .all(|r| r.candidate_keys.is_empty())
    );
    assert!(node(&facts, "finish").binding_key.is_some());
    assert!(node(&facts, "Names").binding_key.is_some());
}

#[test]
fn variance_recovery_does_not_hide_invalid_or_unsupported_syntax() {
    for source in [
        "interface Box<out T> { broken(: T; }",
        "interface Box<out T> {} function broken( {",
        "interface Box<out T U> {}",
        "interface Box<out out T> {}",
        "interface Box<out in T> {}",
        "interface Box<in in T> {}",
        "interface Box<in out> {}",
        "interface Box<out T,> { value: ; }",
        "function make<out T>(value: T) { return value; }",
        "interface Box { make<out T>(value: T): T; }",
        "type Factory = <out T>(value: T) => T;",
        "const value = factory<out T>();",
        "const value = { out T: 1 };",
        "const text = 'interface Box<out T> {}'; function broken( {",
    ] {
        for path in ["invalid.ts", "invalid.tsx"] {
            let facts = parse(path, source, "fixture").unwrap().unwrap();
            assert!(!facts.diagnostics.is_empty(), "{path}: {source}");
            assert!(facts.nodes.is_empty(), "{path}: {source}");
            assert!(facts.edges.is_empty(), "{path}: {source}");
            assert!(facts.references.is_empty(), "{path}: {source}");
        }
    }
    let javascript = parse("invalid.js", "interface Box<out T> {}", "fixture")
        .unwrap()
        .unwrap();
    assert!(!javascript.diagnostics.is_empty());
    assert!(javascript.nodes.is_empty());
}

#[test]
fn factory_const_callees_keep_type_identity_and_callsite_provenance() {
    let source = r#"
export interface Shape {}
interface Maker<T> { (): T; new(): T; }
function manufacture() { return function implementation() {}; }
export const Shape: Maker<Shape> = manufacture();
export function use(): Shape {
  Shape();
  return new Shape();
}
"#;
    let original = facts("factory.ts", source);
    let interface = original
        .nodes
        .iter()
        .find(|n| n.label == "Shape" && n.kind == "interface")
        .unwrap();
    let value = original
        .nodes
        .iter()
        .find(|n| n.label == "Shape" && n.kind == "constant")
        .unwrap();
    let caller = node(&original, "use");
    assert_ne!(interface.id, value.id);
    assert_eq!(
        interface.binding_key.as_deref(),
        Some("javascript:factory:Shape")
    );
    assert_eq!(
        value.binding_key.as_deref(),
        Some("javascript:factory:Shape#declared_callee")
    );
    assert_eq!(value.metadata["declared_callee_binding"], true);
    assert_eq!(value.line, Some(5));
    assert_eq!(
        &source[value.metadata["start_byte"].as_u64().unwrap() as usize
            ..value.metadata["end_byte"].as_u64().unwrap() as usize],
        "Shape: Maker<Shape> = manufacture()"
    );
    assert_eq!(
        value.metadata["binding_aliases"],
        serde_json::json!(["javascript:file:factory.ts:Shape#declared_callee"])
    );
    let declarations: Vec<_> = original
        .references
        .iter()
        .filter(|r| r.relation == "declared_callee")
        .collect();
    assert_eq!(declarations.len(), 2);
    for declaration in &declarations {
        let call = original
            .references
            .iter()
            .find(|r| format!("{}:declared_callee", r.id) == declaration.id)
            .unwrap();
        assert_eq!(call.relation, "calls");
        assert!(call.candidate_keys.is_empty());
        assert_eq!(
            (
                &declaration.source,
                &declaration.label,
                &declaration.file,
                declaration.line
            ),
            (&caller.id, &call.label, &call.file, call.line)
        );
        assert_eq!(declaration.label, "Shape");
        assert_eq!(
            declaration.candidate_keys,
            ["javascript:file:factory.ts:Shape#declared_callee"]
        );
        assert!(
            declaration
                .reason
                .contains("factory result and runtime dispatch are unresolved")
        );
    }
    assert_eq!(
        declarations.iter().map(|r| r.line).collect::<Vec<_>>(),
        [7, 8]
    );
    assert!(original.references.iter().any(|r| r.source == caller.id
        && r.relation == "return_type"
        && r.candidate_keys == ["javascript:file:factory.ts:Shape"]));
    let factory_call = original
        .references
        .iter()
        .find(|r| r.label == "manufacture" && r.relation == "calls")
        .unwrap();
    assert_eq!(factory_call.source, original.nodes[0].id);
    assert!(!factory_call.candidate_keys.is_empty());
    assert!(!original.references.iter().any(|r| {
        r.label == "#declared_callee"
            || r.candidate_keys
                .iter()
                .any(|k| k.starts_with("javascript:receiver:"))
    }));

    let interface_id = interface.id.clone();
    let value_id = value.id.clone();
    let caller_id = caller.id.clone();
    let reference_ids: Vec<_> = declarations.iter().map(|r| r.id.clone()).collect();
    let directory = tempfile::tempdir().unwrap();
    let mut store = Store::create(&directory.path().join("graph.db")).unwrap();
    store
        .apply_native("fixture", vec![original], vec![], Coverage::default())
        .unwrap();
    let snapshot = store.snapshot().unwrap();
    let navigation: Vec<_> = snapshot
        .edges
        .iter()
        .filter(|e| e.source == caller_id && e.relation == "declared_callee")
        .collect();
    assert_eq!(navigation.len(), 2);
    for edge in navigation {
        assert_eq!(edge.target, value_id);
        assert_eq!(edge.file.as_deref(), Some("factory.ts"));
        assert!(matches!(edge.line, Some(7 | 8)));
        assert!(
            reference_ids
                .iter()
                .any(|id| edge.metadata["reference_id"] == *id)
        );
    }
    assert!(
        snapshot.edges.iter().any(|e| e.source == caller_id
            && e.target == interface_id
            && e.relation == "return_type")
    );
    assert!(
        !snapshot
            .edges
            .iter()
            .any(|e| e.source == caller_id && e.relation == "calls")
    );

    let changed = source.replace("= manufacture();", "= externalValue;");
    store
        .apply_native(
            "fixture",
            vec![facts("factory.ts", &changed)],
            vec![],
            Coverage::default(),
        )
        .unwrap();
    let updated = store.snapshot().unwrap();
    assert!(!updated.nodes.iter().any(|n| n.id == value_id));
    assert!(
        !updated
            .edges
            .iter()
            .any(|e| e.relation == "declared_callee")
    );
    assert!(updated.nodes.iter().any(|n| n.id == interface_id));
    assert!(
        !updated
            .edges
            .iter()
            .any(|e| e.relation == "calls" && e.target == interface_id)
    );
}

#[test]
fn local_factory_binding_does_not_replace_written_receiver_evidence() {
    let source = r#"
export class Service { run() {} }
const service: Service = acquire();
const Job = builders.make();
function use() { service.run(); service(); Job(); new Job(); }
"#;
    let facts = facts("receivers.ts", source);
    let member = facts
        .references
        .iter()
        .find(|r| r.label == "service.run" && r.relation == "calls")
        .unwrap();
    assert_eq!(
        member.candidate_keys,
        ["javascript:file:receivers.ts:Service#instance.run"]
    );
    for name in ["service", "Job"] {
        let declaration = facts
            .nodes
            .iter()
            .find(|n| n.label == name && n.kind == "constant")
            .unwrap();
        let expected = format!("javascript:local:receivers.ts::{name}#declared_callee");
        assert_eq!(declaration.binding_key.as_deref(), Some(expected.as_str()));
        assert_eq!(declaration.metadata["declared_callee_binding"], true);
        assert!(declaration.metadata["binding_aliases"].is_null());
        let navigation: Vec<_> = facts
            .references
            .iter()
            .filter(|r| r.label == name && r.relation == "declared_callee")
            .collect();
        assert_eq!(navigation.len(), if name == "service" { 1 } else { 2 });
        assert!(
            navigation
                .iter()
                .all(|r| r.candidate_keys == [expected.clone()])
        );
        assert!(
            facts
                .references
                .iter()
                .filter(|r| r.label == name && r.relation == "calls")
                .all(|r| r.candidate_keys.is_empty())
        );
    }
}

#[test]
fn factory_callee_navigation_obeys_binding_and_syntax_boundaries() {
    for source in [
        "const Item = factory(); function shadow(Item) { Item(); }",
        "const Item = factory(); { let Item = external; Item(); }",
        "const Item = factory(); Item[key]();",
        "const Item = factory(); Item.call();",
        "const Item = factory(); Item?.();",
        "let Item = factory(); Item();",
        "var Item = factory(); new Item();",
        "const Item = external; Item();",
        "function unknown(Item) { new Item(); }",
        "const Item = factories[key](); Item();",
        "const Item = factories?.make(); Item();",
        "const Item = factory?.(); Item();",
        "const Item = factory()(); Item();",
        "const Item = condition ? first() : second(); Item();",
        "const { Item } = factory(); Item();",
        "const Item = factory(); with (context) { Item(); }",
    ] {
        let facts = facts("negative.js", source);
        assert!(
            !facts
                .references
                .iter()
                .any(|r| r.relation == "declared_callee"),
            "{source}"
        );
        assert!(
            facts
                .references
                .iter()
                .filter(|r| r.relation == "calls" && r.label.starts_with("Item"))
                .all(|r| r.candidate_keys.is_empty()),
            "{source}"
        );
    }
    for source in [
        "export const Item = factory(); Item = other; Item();",
        "export const Item = factory(); Item(); Item = other;",
        "Item = other; export const Item = factory(); Item();",
        "export const Item = factory(); function replace() { Item = other; } Item();",
        "export const Item = factory(); Item.extra = other; Item();",
        "export const Item = factory(); const Item = factory(); Item();",
        "export const Item = factory(); eval(code); Item();",
    ] {
        let facts = facts("invalidated.js", source);
        let constants: Vec<_> = facts
            .nodes
            .iter()
            .filter(|n| n.kind == "constant")
            .collect();
        assert!(!constants.is_empty(), "{source}");
        assert!(
            constants.iter().all(|n| n.binding_key.is_none()
                && n.metadata["declared_callee_binding"] != true
                && n.metadata["binding_aliases"].is_null()),
            "{source}"
        );
        assert!(
            !facts
                .references
                .iter()
                .any(|r| r.relation == "declared_callee"),
            "{source}"
        );
        assert!(
            facts
                .references
                .iter()
                .filter(|r| r.label == "Item" && r.relation == "calls")
                .all(|r| r.candidate_keys.is_empty()),
            "{source}"
        );
    }
}
