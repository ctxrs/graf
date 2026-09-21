use graf::{
    index::{self, IndexOptions},
    model::{Direction, GraphSnapshot, Node, QueryOptions},
    query::ImpactOptions,
    store::Store,
};
use std::{collections::BTreeSet, fs, path::Path};

fn indexed(root: &Path) -> Store {
    let db = root.join(".graf/index.db");
    index::run_with_options(
        root,
        &db,
        &IndexOptions {
            code_only: true,
            ..Default::default()
        },
    )
    .unwrap();
    Store::open_read_only(&db).unwrap()
}

fn node<'a>(graph: &'a GraphSnapshot, qualified: &str, kind: &str) -> &'a Node {
    let matches: Vec<_> = graph
        .nodes
        .iter()
        .filter(|n| n.qualified_name.as_deref() == Some(qualified) && n.kind == kind)
        .collect();
    assert_eq!(matches.len(), 1, "missing or ambiguous {kind} {qualified}");
    matches[0]
}

fn assert_impact(
    store: &Store,
    target: &Node,
    consumer: &Node,
    unrelated: &Node,
    relation: &str,
    call: (&str, Option<&Node>),
    linked: bool,
) {
    let snapshot = store.snapshot().unwrap();
    let recorded: Vec<_> = snapshot
        .edges
        .iter()
        .filter(|e| e.source == consumer.id && e.target == target.id && e.relation == relation)
        .collect();
    assert_eq!(recorded.len(), usize::from(linked));

    // Declaration navigation and proof of the runtime body are independent.
    let runtime = store
        .neighbors(
            &consumer.id,
            &QueryOptions {
                direction: Direction::Outgoing,
                relation: Some("calls".into()),
                ..Default::default()
            },
        )
        .unwrap();
    let (call_label, runtime_body) = call;
    let (call_file, call_line) = if let Some(body) = runtime_body {
        assert!(runtime.unresolved.is_empty());
        assert_eq!(runtime.edges.len(), 1);
        let call = &runtime.edges[0];
        assert_eq!(call.source, consumer.id);
        assert_eq!(call.target, body.id);
        assert_ne!(call.target, target.id);
        assert_eq!(call.relation, "calls");
        assert_eq!(call.file.as_deref(), Some(consumer.file.as_str()));
        assert_eq!(call.line, consumer.line);
        assert!(call.directed);
        (call.file.as_deref().unwrap(), call.line.unwrap())
    } else {
        assert!(runtime.edges.is_empty());
        assert_eq!(runtime.unresolved.len(), 1);
        let call = &runtime.unresolved[0];
        assert_eq!(call.source, consumer.id);
        assert_eq!(call.label, call_label);
        assert_eq!(call.relation, "calls");
        (call.file.as_str(), call.line)
    };

    let impact = store
        .impact_extended(&target.id, &ImpactOptions::default())
        .unwrap();
    assert_eq!(impact.seeds.as_slice(), std::slice::from_ref(&target.id));
    let expected = if linked {
        BTreeSet::from([target.id.as_str(), consumer.id.as_str()])
    } else {
        BTreeSet::from([target.id.as_str()])
    };
    assert_eq!(
        impact
            .graph
            .nodes
            .iter()
            .map(|n| n.id.as_str())
            .collect::<BTreeSet<_>>(),
        expected
    );
    assert!(!impact.graph.nodes.iter().any(|n| n.id == unrelated.id));
    assert!(!impact.graph.truncated);
    assert_eq!(impact.graph.edges.len(), recorded.len());
    if linked {
        let evidence = recorded[0];
        assert!(evidence.directed);
        assert_eq!(evidence.file.as_deref(), Some(call_file));
        assert_eq!(evidence.line, Some(call_line));
        assert!(
            evidence.metadata["reference_id"]
                .as_str()
                .unwrap()
                .ends_with(&format!(":{relation}"))
        );
        if runtime_body.is_some() {
            assert_eq!(
                evidence.metadata["reference_id"],
                format!(
                    "{}:{relation}",
                    runtime.edges[0].metadata["reference_id"].as_str().unwrap()
                )
            );
        }
        // Impact preserves the stored relation, orientation, confidence, and provenance.
        assert_eq!(
            serde_json::to_value(&impact.graph.edges[0]).unwrap(),
            serde_json::to_value(evidence).unwrap()
        );
    }

    let calls_only = store
        .impact_extended(
            &target.id,
            &ImpactOptions {
                relations: vec!["calls".into()],
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(
        calls_only.seeds.as_slice(),
        std::slice::from_ref(&target.id)
    );
    assert_eq!(calls_only.graph.nodes.len(), 1);
    assert_eq!(calls_only.graph.nodes[0].id, target.id);
    assert!(calls_only.graph.edges.is_empty());
    assert!(!calls_only.graph.truncated);
    assert_eq!(store.stats().unwrap().generation, snapshot.generation);
}

#[test]
fn impact_tracks_factory_const_evidence_and_loses_a_shadowed_consumer() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("factory.js");
    let source = "function factory() { return function runtime() {}; }\nexport const Product = factory();\nexport function consume() { return Product(); }\nfunction unrelated() { return 0; }\n";
    let mut target_id = None;
    for (source, linked) in [
        (source.to_owned(), true),
        (source.replace("consume()", "consume(Product)"), false),
    ] {
        fs::write(&path, &source).unwrap();
        let store = indexed(dir.path());
        let graph = store.snapshot().unwrap();
        let target = node(&graph, "Product", "constant");
        let bodies: Vec<_> = graph
            .nodes
            .iter()
            .filter(|n| n.file == "factory.js" && n.kind == "function" && n.label == "runtime")
            .collect();
        assert_eq!(bodies.len(), 1);
        let body = bodies[0];
        assert_eq!(body.line, Some(1));
        assert_eq!(
            body.metadata["start_byte"],
            source.find("function runtime").unwrap()
        );
        assert_eq!(target.metadata["declared_callee_binding"], true);
        assert_eq!(
            target_id.get_or_insert_with(|| target.id.clone()).as_str(),
            target.id.as_str()
        );
        assert_impact(
            &store,
            target,
            node(&graph, "consume", "function"),
            node(&graph, "unrelated", "function"),
            "declared_callee",
            ("Product", linked.then_some(body)),
            linked,
        );
    }
}

#[test]
fn impact_tracks_dynamic_member_evidence_and_removes_links_when_proof_is_lost() {
    let python = "class Reader:\n    def parse(self): return 1\n    def read(self): return self.parse()\nclass Other(Reader):\n    def parse(self): return 2\ndef unrelated(): return 0\n";
    let javascript = "class Reader {\n  parse() { return 1; }\n  read() { this.pending = 1; return this.parse(); }\n}\nfunction unrelated() { return 0; }\n";
    for (file, source, changed, changed_call) in [
        (
            "reader.py",
            python,
            python.replace(
                "class Other",
                "    def mutate(self, value): self.parse = value\nclass Other",
            ),
            "self.parse",
        ),
        (
            "reader.js",
            javascript,
            javascript
                .replace("read()", "read(other)")
                .replace("this.parse()", "other.parse()"),
            "other.parse",
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let mut target_id = None;
        for (source, linked, call) in [
            (
                source.to_owned(),
                true,
                if file.ends_with(".py") {
                    "self.parse"
                } else {
                    "this.parse"
                },
            ),
            (changed, false, changed_call),
        ] {
            fs::write(dir.path().join(file), source).unwrap();
            let store = indexed(dir.path());
            let graph = store.snapshot().unwrap();
            let target = node(&graph, "Reader.parse", "method");
            assert_eq!(
                target_id.get_or_insert_with(|| target.id.clone()).as_str(),
                target.id.as_str()
            );
            assert_impact(
                &store,
                target,
                node(&graph, "Reader.read", "method"),
                node(&graph, "unrelated", "function"),
                "declared_member",
                (call, None),
                linked,
            );
        }
    }
}
