use graf::{
    model::{Coverage, FileFacts, Node, Reference},
    parser::{PythonContext, parse_python},
    store::Store,
};
use serde_json::Value;

const DEFINITIONS: &str = "def destination():\n    return 1\n\ndef fallback():\n    return 2\n\n";

fn parse(source: &str) -> FileFacts {
    let facts = parse_python("sample.py", source, "fixture").unwrap();
    assert!(facts.diagnostics.is_empty(), "{:?}", facts.diagnostics);
    facts
}

fn node<'a>(facts: &'a FileFacts, name: &str) -> &'a Node {
    facts
        .nodes
        .iter()
        .find(|n| n.qualified_name.as_deref() == Some(name))
        .unwrap()
}

fn calls<'a>(facts: &'a FileFacts, owner: &str, label: &str) -> Vec<&'a Reference> {
    let owner = &node(facts, owner).id;
    facts
        .references
        .iter()
        .filter(|r| r.source == *owner && r.relation == "calls" && r.label == label)
        .collect()
}

fn targets(facts: &FileFacts, owner: &str, label: &str) -> Vec<Vec<String>> {
    calls(facts, owner, label)
        .into_iter()
        .map(|r| r.candidate_keys.clone())
        .collect()
}

#[test]
fn last_unconditional_value_resolves_fallback_with_exact_source_identity() -> anyhow::Result<()> {
    let source = format!(
        "{DEFINITIONS}def shadowed(destination):\n    return destination()\n\ndef mutated():\n    chosen = destination\n    chosen = fallback\n    return chosen()\n\nclass North:\n    def deliver(self):\n        return 3\n\nclass South:\n    def deliver(self):\n        return 4\n\ndef ambiguous(item):\n    return item.deliver()\n"
    );
    let mut facts = parse(&source);
    assert_eq!(
        targets(&facts, "shadowed", "destination"),
        [Vec::<String>::new()]
    );
    assert_eq!(
        targets(&facts, "mutated", "chosen"),
        [vec!["python:sample:fallback"]]
    );
    assert_eq!(
        targets(&facts, "ambiguous", "item.deliver"),
        [Vec::<String>::new()]
    );
    let context = PythonContext::from_facts(std::slice::from_ref(&facts));
    context.apply(&mut facts);
    let call = calls(&facts, "mutated", "chosen")[0].clone();
    let offset = source.find("chosen()").unwrap();
    let owner = node(&facts, "mutated").id.clone();
    let target = node(&facts, "fallback").id.clone();
    assert_eq!(
        call.id,
        format!("call:{owner}:{offset}-{}", offset + "chosen()".len())
    );
    assert_eq!(
        call.line as usize,
        source[..offset].bytes().filter(|b| *b == b'\n').count() + 1
    );
    assert_eq!(call.file, "sample.py");
    assert_eq!(call.source, owner);
    assert_eq!(call.candidate_keys, ["python:sample:fallback"]);
    let temp = tempfile::tempdir()?;
    let mut store = Store::create(&temp.path().join("graph.db"))?;
    store.apply_native("fixture", vec![facts], vec![], Coverage::default())?;
    let graph = store.snapshot()?;
    let edges: Vec<_> = graph
        .edges
        .iter()
        .filter(|e| e.source == owner && e.relation == "calls")
        .collect();
    assert_eq!(edges.len(), 1);
    assert_eq!(edges[0].id, format!("reference:{}", call.id));
    assert_eq!(edges[0].target, target);
    assert_eq!(edges[0].line, Some(call.line));
    Ok(())
}

#[test]
fn values_are_copied_at_each_write_and_calls_observe_their_own_position() {
    let facts = parse(&format!(
        "{DEFINITIONS}def run():\n    chosen()\n    chosen = destination\n    chosen()\n    copied = chosen\n    chosen = fallback\n    chosen()\n    copied()\n    chosen = unknown\n    chosen()\n    chosen = destination\n    chosen()\n"
    ));
    assert_eq!(
        targets(&facts, "run", "chosen"),
        [
            vec![],
            vec!["python:sample:destination"],
            vec!["python:sample:fallback"],
            vec![],
            vec!["python:sample:destination"],
        ]
    );
    assert_eq!(
        targets(&facts, "run", "copied"),
        [vec!["python:sample:destination"]]
    );
    let facts = parse(&format!(
        "{DEFINITIONS}def run():\n    chosen = destination\n    chosen = chosen()\n    chosen()\n"
    ));
    assert_eq!(
        targets(&facts, "run", "chosen"),
        [vec!["python:sample:destination"], vec![]]
    );
    let facts = parse(&format!(
        "{DEFINITIONS}def run():\n    ｃｈｏｓｅｎ = destination; chosen = fallback; ｃｈｏｓｅｎ()\n"
    ));
    assert_eq!(
        targets(&facts, "run", "ｃｈｏｓｅｎ"),
        [vec!["python:sample:fallback"]]
    );
}

#[test]
fn conditional_unsupported_and_parameter_writes_do_not_prove_values() {
    let bodies = [
        "    chosen = destination\n    if flag:\n        chosen = fallback\n    chosen()\n",
        "    chosen = destination\n    for chosen in values:\n        pass\n    chosen()\n",
        "    chosen = destination\n    while flag:\n        chosen = fallback\n    chosen()\n",
        "    chosen = destination\n    try:\n        chosen = fallback\n    except Exception:\n        pass\n    chosen()\n",
        "    chosen = destination\n    with manager() as chosen:\n        pass\n    chosen()\n",
        "    chosen = destination\n    match value:\n        case chosen:\n            pass\n    chosen()\n",
        "    chosen = destination\n    chosen += fallback\n    chosen()\n",
        "    chosen = destination\n    (chosen := fallback)\n    chosen()\n",
        "    chosen = destination\n    del chosen\n    chosen()\n",
        "    chosen = destination\n    chosen = other = fallback\n    chosen()\n",
        "    chosen = destination\n    chosen, other = values\n    chosen()\n",
        "    chosen = destination\n    chosen: Callable = fallback\n    chosen()\n",
        "    global chosen\n    chosen = destination\n    chosen()\n",
        "    chosen = destination\n    import module as chosen\n    chosen()\n",
        "    chosen = destination\n    def chosen():\n        pass\n    chosen()\n",
    ];
    for body in bodies {
        let facts = parse(&format!("{DEFINITIONS}def run():\n{body}"));
        let references = calls(&facts, "run", "chosen");
        assert!(!references.is_empty(), "{body}");
        assert!(
            references.iter().all(|r| r.candidate_keys.is_empty()),
            "{body}"
        );
    }
    for parameters in ["chosen", "destination", "*chosen", "**chosen"] {
        let facts = parse(&format!(
            "{DEFINITIONS}def run({parameters}):\n    chosen = destination\n    chosen()\n"
        ));
        assert_eq!(
            targets(&facts, "run", "chosen"),
            [Vec::<String>::new()],
            "{parameters}"
        );
    }
    let good = parse(&format!(
        "{DEFINITIONS}def run(flag):\n    chosen = destination\n    chosen = fallback\n    chosen()\n"
    ));
    assert_eq!(
        targets(&good, "run", "chosen"),
        [vec!["python:sample:fallback"]]
    );
}

#[test]
fn captured_reads_nonlocal_writes_and_value_escapes_veto_local_history() {
    for body in [
        "    chosen = destination\n    chosen()\n    def later():\n        return chosen()\n    chosen = fallback\n    chosen()\n    return later\n",
        "    chosen = destination\n    later = lambda: chosen()\n    chosen = fallback\n    chosen()\n    return later\n",
        "    chosen = destination\n    def change():\n        nonlocal chosen\n        chosen = fallback\n        chosen()\n    change()\n    chosen()\n",
        "    chosen = destination\n    register(chosen)\n    chosen = fallback\n    chosen()\n",
        "    chosen = destination\n    chosen()\n    return [chosen]\n",
        "    chosen = destination\n    chosen.__code__ = other.__code__\n    chosen()\n",
        "    chosen = destination\n    later = (chosen() for item in items)\n    chosen()\n",
    ] {
        let facts = parse(&format!("{DEFINITIONS}def run():\n{body}"));
        let references: Vec<_> = facts
            .references
            .iter()
            .filter(|r| r.relation == "calls" && r.label == "chosen")
            .collect();
        assert!(!references.is_empty());
        assert!(
            references.iter().all(|r| r.candidate_keys.is_empty()),
            "{body}"
        );
    }
    let captured = parse(
        "def outer():\n    def local():\n        return 1\n    def inner():\n        chosen = local\n        chosen()\n    return inner\n",
    );
    assert_eq!(
        targets(&captured, "outer.inner", "chosen"),
        [Vec::<String>::new()]
    );
    let local =
        parse("def run():\n    def local():\n        return 1\n    chosen = local\n    chosen()\n");
    assert_eq!(
        targets(&local, "run", "chosen"),
        [vec!["python:sample:run.local"]]
    );
}

#[test]
fn copied_value_escapes_veto_originals_but_plain_rebinding_keeps_each_value() {
    for body in [
        "    chosen = destination\n    copied = chosen\n    copied.__code__ = fallback.__code__\n    chosen()\n",
        "    chosen = destination\n    copied = chosen\n    register(copied)\n    chosen()\n",
        "    chosen = destination\n    copied = chosen\n    another = copied\n    another.__code__ = fallback.__code__\n    chosen()\n",
        "    chosen = destination\n    copied = chosen\n    def later():\n        return copied\n    register(later)\n    chosen()\n",
        "    chosen = destination\n    copied = chosen\n    chosen = fallback\n    register(copied)\n    chosen()\n",
        "    chosen = destination\n    copied = destination\n    register(copied)\n    chosen()\n",
    ] {
        let facts = parse(&format!("{DEFINITIONS}def run():\n{body}"));
        assert_eq!(
            targets(&facts, "run", "chosen"),
            [Vec::<String>::new()],
            "{body}"
        );
    }
    let reverse = parse(&format!(
        "{DEFINITIONS}def run():\n    chosen = destination\n    copied = chosen\n    register(chosen)\n    copied()\n"
    ));
    assert_eq!(targets(&reverse, "run", "copied"), [Vec::<String>::new()]);

    for (setup, expected) in [
        ("    copied = chosen\n", "python:sample:destination"),
        (
            "    intermediate = chosen\n    copied = intermediate\n",
            "python:sample:destination",
        ),
        (
            "    copied = chosen\n    copied = fallback\n",
            "python:sample:fallback",
        ),
    ] {
        let facts = parse(&format!(
            "{DEFINITIONS}def run():\n    chosen = destination\n{setup}    copied()\n    chosen()\n    chosen = fallback\n    copied()\n    chosen()\n"
        ));
        assert_eq!(
            targets(&facts, "run", "chosen"),
            [
                vec!["python:sample:destination"],
                vec!["python:sample:fallback"]
            ],
            "{setup}"
        );
        assert_eq!(
            targets(&facts, "run", "copied"),
            [vec![expected], vec![expected]],
            "{setup}"
        );
    }
}

#[test]
fn parenthesized_dynamic_calls_veto_values_but_ordinary_calls_do_not() {
    for callee in [
        "eval",
        "(eval)",
        "(((eval)))",
        "(exec)",
        "((globals))",
        "(locals)",
    ] {
        let arguments = if callee.contains("eval") || callee.contains("exec") {
            "\"setattr(chosen, '__code__', fallback.__code__)\""
        } else {
            ""
        };
        let facts = parse(&format!(
            "{DEFINITIONS}def run():\n    chosen = destination\n    {callee}({arguments})\n    chosen()\n"
        ));
        assert_eq!(
            targets(&facts, "run", "chosen"),
            [Vec::<String>::new()],
            "{callee}"
        );
    }
    for ordinary in ["", "    (fallback)()\n", "    \"eval('chosen')\"\n"] {
        let facts = parse(&format!(
            "{DEFINITIONS}def run():\n    chosen = destination\n{ordinary}    chosen()\n"
        ));
        assert_eq!(
            targets(&facts, "run", "chosen"),
            [vec!["python:sample:destination"]],
            "{ordinary}"
        );
    }
}

#[test]
fn unknown_dynamic_imported_and_future_targets_remain_unresolved() {
    let bodies = [
        "    chosen = unknown\n    chosen()\n",
        "    chosen = factory()\n    chosen()\n",
        "    chosen = obj.destination\n    chosen()\n",
        "    chosen = destination if flag else fallback\n    chosen()\n",
        "    chosen = lambda: destination()\n    chosen()\n",
        "    chosen = destination\n    eval(code)\n    chosen()\n",
        "    chosen = destination\n    exec(code)\n    chosen()\n",
        "    chosen = destination\n    locals()\n    chosen()\n",
        "    chosen = destination\n    yield 1\n    chosen()\n",
    ];
    for body in bodies {
        let facts = parse(&format!("{DEFINITIONS}def run():\n{body}"));
        assert_eq!(
            targets(&facts, "run", "chosen"),
            [Vec::<String>::new()],
            "{body}"
        );
    }
    for source in [
        "def run():\n    chosen = future\n    chosen()\ndef future():\n    pass\n",
        "def destination():\n    pass\ndef run():\n    chosen = destination\n    chosen()\ndestination = unknown\n",
        "def destination():\n    pass\nchosen = destination\ndef run():\n    chosen()\n",
        "def destination():\n    pass\ndef run():\n    chosen = destination\n    chosen()\ndef destination():\n    pass\n",
        "@decorate\ndef destination():\n    pass\ndef run():\n    chosen = destination\n    chosen()\n",
        "from provider import destination\ndef run():\n    chosen = destination\n    chosen()\n",
        "from provider import *\ndef destination():\n    pass\ndef run():\n    chosen = destination\n    chosen()\n",
        "class destination:\n    pass\ndef run():\n    chosen = destination\n    chosen()\n",
        "def destination():\n    pass\nasync def run():\n    chosen = destination\n    await pause()\n    chosen()\n",
    ] {
        let facts = parse(source);
        assert_eq!(
            targets(&facts, "run", "chosen"),
            [Vec::<String>::new()],
            "{source}"
        );
    }
}

#[test]
fn context_exports_and_fingerprints_do_not_publish_local_value_aliases() {
    let first = parse(&format!(
        "{DEFINITIONS}def run():\n    chosen = destination\n    chosen()\n"
    ));
    let second = parse(&format!(
        "{DEFINITIONS}def run():\n    chosen = fallback\n    chosen()\n"
    ));
    assert_eq!(first.nodes[0].metadata, second.nodes[0].metadata);
    assert_eq!(
        PythonContext::from_facts(std::slice::from_ref(&first)).fingerprint(),
        PythonContext::from_facts(std::slice::from_ref(&second)).fingerprint()
    );
    assert!(second.nodes.iter().all(|n| {
        !n.binding_key
            .as_deref()
            .is_some_and(|k| k.ends_with(":chosen"))
    }));
    let mut files = vec![
        second,
        parse_python(
            "public.py",
            "from sample import fallback as Public\n",
            "fixture",
        )
        .unwrap(),
        parse_python(
            "consumer.py",
            "from public import Public\ndef use():\n    Public()\n",
            "fixture",
        )
        .unwrap(),
    ];
    let context = PythonContext::from_facts(&files);
    for facts in &mut files {
        context.apply(facts);
    }
    assert_eq!(
        targets(&files[0], "run", "chosen"),
        [vec!["python:sample:fallback"]]
    );
    assert_eq!(
        targets(&files[2], "use", "Public"),
        [vec!["python:sample:fallback"]]
    );
}

#[test]
fn changing_a_value_retracts_stale_edges_through_existing_store_identity() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let mut store = Store::create(&temp.path().join("update.db"))?;
    let mut previous_id = None;
    for (value, expected) in [
        ("destination", Some("destination")),
        ("fallback   ", Some("fallback")),
        ("unknown    ", None),
    ] {
        let source = format!("{DEFINITIONS}def run():\n    chosen = {value}\n    chosen()\n");
        let mut facts = parse(&source);
        PythonContext::from_facts(std::slice::from_ref(&facts)).apply(&mut facts);
        let call_id = calls(&facts, "run", "chosen")[0].id.clone();
        if let Some(previous) = &previous_id {
            assert_eq!(&call_id, previous);
        }
        previous_id = Some(call_id.clone());
        let owner = node(&facts, "run").id.clone();
        let target = expected.map(|name| node(&facts, name).id.clone());
        store.apply_native("fixture", vec![facts], vec![], Coverage::default())?;
        let graph = store.snapshot()?;
        let edges: Vec<_> = graph
            .edges
            .iter()
            .filter(|e| e.source == owner && e.relation == "calls")
            .collect();
        match target {
            Some(target) => {
                assert_eq!(edges.len(), 1);
                assert_eq!(edges[0].target, target);
                assert_eq!(edges[0].id, format!("reference:{call_id}"));
            }
            None => {
                assert!(edges.is_empty());
                assert_eq!(
                    graph.metadata["graf_unresolved_references"][0]["id"],
                    Value::String(call_id)
                );
            }
        }
    }
    Ok(())
}
