use std::fs;

use graf::{
    index,
    model::*,
    parser::{PythonContext, parse_python},
    sources,
    store::Store,
};
use tempfile::tempdir;

fn parse(source: &str) -> FileFacts {
    parse_python("src/pkg/code.py", source, "test-hash").unwrap()
}

fn call<'a>(facts: &'a FileFacts, owner: &str, label: &str) -> &'a Reference {
    let id = &facts
        .nodes
        .iter()
        .find(|n| n.qualified_name.as_deref() == Some(owner))
        .unwrap()
        .id;
    facts
        .references
        .iter()
        .find(|r| r.source == *id && r.relation == "calls" && r.label == label)
        .unwrap()
}

fn callees(store: &Store, name: &str) -> GraphResult {
    store
        .neighbors(
            name,
            &QueryOptions {
                direction: Direction::Outgoing,
                relation: Some("calls".into()),
                ..Default::default()
            },
        )
        .unwrap()
}

fn scan_manifest(db: &std::path::Path) -> std::path::PathBuf {
    let mut path = db.as_os_str().to_owned();
    path.push(".scan-manifest.json");
    path.into()
}

fn context_discoveries() -> usize {
    index::project_context_discoveries_for_tests()
}

#[test]
fn annotation_calls_are_conservatively_omitted_but_defaults_are_retained() {
    let source = "def annotation():\n    pass\ndef default():\n    pass\ndef runtime():\n    pass\nmodule_value: annotation()\nclass C:\n    class_value: annotation()\ndef run(value: annotation() = default()) -> annotation():\n    x: annotation()\n    y: annotation() = runtime()\n    def inner(value: annotation() = default()) -> annotation():\n        z: annotation()\n    runtime()\n";
    for postponed in [false, true] {
        let source = if postponed {
            format!("from __future__ import annotations\n{source}")
        } else {
            source.into()
        };
        let facts = parse(&source);
        assert!(facts.diagnostics.is_empty());
        let calls: Vec<_> = facts
            .references
            .iter()
            .filter(|r| r.relation == "calls")
            .collect();
        assert_eq!(calls.iter().filter(|r| r.label == "annotation").count(), 0);
        assert_eq!(calls.iter().filter(|r| r.label == "default").count(), 2);
        assert_eq!(calls.iter().filter(|r| r.label == "runtime").count(), 2);
        assert_eq!(
            call(&facts, "run", "default").candidate_keys,
            ["python:pkg.code:default"]
        );
        assert_eq!(
            call(&facts, "run", "runtime").candidate_keys,
            ["python:pkg.code:runtime"]
        );
    }
    let local_only = parse("def target():\n    pass\ndef run():\n    x: target()\n");
    assert!(!local_only.references.iter().any(|r| r.relation == "calls"));
}

#[test]
fn class_private_names_and_implicit_class_cells_do_not_fall_through() {
    let facts = parse(
        "def __target():\n    pass\ndef _C__target():\n    pass\ndef __class__():\n    pass\ndef ordinary():\n    pass\nclass C:\n    def run(self):\n        __target()\n        __class__()\n        ordinary()\n        def nested():\n            __target()\n            __class__()\n    def imported(self):\n        from other import __target as alias\n        alias()\ndef outside():\n    __target()\n    __class__()\n",
    );
    assert!(facts.diagnostics.is_empty());
    for owner in ["C.run", "C.run.nested"] {
        for name in ["__target", "__class__"] {
            assert!(call(&facts, owner, name).candidate_keys.is_empty());
        }
    }
    assert!(
        call(&facts, "C.imported", "alias")
            .candidate_keys
            .is_empty()
    );
    assert_eq!(
        call(&facts, "C.run", "ordinary").candidate_keys,
        ["python:pkg.code:ordinary"]
    );
    assert_eq!(
        call(&facts, "outside", "__target").candidate_keys,
        ["python:pkg.code:__target"]
    );
    assert_eq!(
        call(&facts, "outside", "__class__").candidate_keys,
        ["python:pkg.code:__class__"]
    );
}

#[test]
fn unicode_identifiers_normalize_bindings_without_changing_display_or_ids() {
    let facts = parse(
        "def K():\n    pass\ndef café():\n    pass\nclass ℂ:\n    def K(self):\n        pass\ndef run():\n    K()\n    K()\n    café()\n",
    );
    assert!(facts.diagnostics.is_empty());
    for name in ["K", "K"] {
        assert_eq!(
            call(&facts, "run", name).candidate_keys,
            ["python:pkg.code:K"]
        );
    }
    assert_eq!(
        call(&facts, "run", "café").candidate_keys,
        ["python:pkg.code:café"]
    );
    let target = facts
        .nodes
        .iter()
        .find(|n| n.qualified_name.as_deref() == Some("K"))
        .unwrap();
    assert_eq!(target.label, "K");
    assert_eq!(target.id, "python:src/pkg/code.py:K@0");
    let method = facts
        .nodes
        .iter()
        .find(|n| n.qualified_name.as_deref() == Some("C.K"))
        .unwrap();
    assert_eq!(method.label, "K");
    assert_eq!(method.binding_key.as_deref(), Some("python:pkg.code:C.K"));
    assert!(method.id.contains(":ℂ.K@"));

    for source in [
        "def K():\n    pass\nK = other\ndef run():\n    K()\n",
        "def K():\n    pass\ndef run(K):\n    K()\n",
        "def K():\n    pass\ndef run():\n    global K\n    K = other\n    K()\n",
        "def K():\n    pass\ndef K():\n    pass\ndef run():\n    K()\n",
    ] {
        let facts = parse(source);
        assert!(facts.diagnostics.is_empty());
        assert!(
            call(&facts, "run", "K").candidate_keys.is_empty(),
            "{source}"
        );
    }
    let supported = parse(
        "# K and café are ordinary text here\nlabel = 'K'\ndef target():\n    pass\ndef run():\n    target()\n",
    );
    assert!(supported.diagnostics.is_empty());
    assert_eq!(
        call(&supported, "run", "target").candidate_keys,
        ["python:pkg.code:target"]
    );
}

#[test]
fn unicode_imports_normalize_identifiers_but_keep_filesystem_module_identity() {
    let source = "from K import K as ｆ\nimport K as ｍ\nimport pkg.K\ndef caller():\n    f()\n    ｍ.K()\n    pkg.K.K()\n";
    let facts = parse_python("consumer.py", source, "h").unwrap();
    assert!(facts.diagnostics.is_empty());
    for name in ["f", "ｍ.K"] {
        assert_eq!(call(&facts, "caller", name).candidate_keys, ["python:K:K"]);
    }
    assert_eq!(
        call(&facts, "caller", "pkg.K.K").candidate_keys,
        ["python:pkg.K:K"]
    );
    assert!(facts.references.iter().any(|r| r.relation == "imports"
        && r.label == "K.K"
        && r.candidate_keys[0] == "python:K:K"));
    let relative = parse_python(
        "pkg/Kdir/consumer.py",
        "from .K import K as f\ndef caller():\n    f()\n",
        "h",
    )
    .unwrap();
    assert_eq!(relative.module, "pkg.Kdir.consumer");
    assert_eq!(
        call(&relative, "caller", "f").candidate_keys,
        ["python:pkg.Kdir.K:K"]
    );

    let root = tempdir().unwrap();
    let db = root.path().join(".graf/index.db");
    fs::create_dir(root.path().join("pkg")).unwrap();
    fs::write(root.path().join("consumer.py"), source).unwrap();
    for path in ["K.py", "K.py", "pkg/K.py", "pkg/K.py"] {
        fs::write(root.path().join(path), "def K():\n    pass\n").unwrap();
    }
    index::run(root.path(), &db).unwrap();
    let graph = callees(&Store::open(&db).unwrap(), "caller");
    assert_eq!(graph.edges.len(), 3);
    assert!(graph.unresolved.is_empty());
    for edge in &graph.edges {
        let target = graph.nodes.iter().find(|n| n.id == edge.target).unwrap();
        assert!(matches!(target.file.as_str(), "K.py" | "pkg/K.py"));
        assert_eq!(target.label, "K");
    }
    // Python source imports normalize K to K; they cannot reach the literal K.py file.
    fs::remove_file(root.path().join("K.py")).unwrap();
    index::run(root.path(), &db).unwrap();
    let graph = callees(&Store::open(&db).unwrap(), "caller");
    assert_eq!(graph.edges.len(), 1);
    assert_eq!(graph.unresolved.len(), 2);
}

#[test]
fn duplicate_parameters_diagnose_and_clear_the_entire_file() {
    for definition in [
        "def run(x, x):\n    target()\n",
        "async def run(x: int, x: str = ''):\n    target()\n",
        "def run(x, /, x):\n    target()\n",
        "def run(x, *, x=1):\n    target()\n",
        "def run(x, *x):\n    target()\n",
        "def run(*x, **x):\n    target()\n",
        "value = lambda x, x: target()\n",
        "def run(K, K):\n    target()\n",
        "value = lambda K, K: target()\n",
    ] {
        let facts = parse(&format!("def target():\n    pass\n{definition}"));
        assert!(
            facts.nodes.is_empty() && facts.edges.is_empty() && facts.references.is_empty(),
            "{definition}"
        );
        assert!(
            facts.diagnostics[0]
                .message
                .contains("Duplicate Python parameter"),
            "{definition}"
        );
        assert!(facts.diagnostics[0].line.is_some());
    }
    let valid = parse(
        "def target():\n    pass\ndef run(x: int, /, y: int = 1, *args, z: int = 1, **kwargs):\n    target()\n",
    );
    assert!(valid.diagnostics.is_empty());
    assert_eq!(
        call(&valid, "run", "target").candidate_keys,
        ["python:pkg.code:target"]
    );
}

#[test]
fn normalized_rebindings_and_duplicate_parameters_remove_old_targets_on_update() {
    let root = tempdir().unwrap();
    let db = root.path().join(".graf/index.db");
    let target = root.path().join("provider.py");
    fs::write(
        root.path().join("consumer.py"),
        "from provider import K\ndef caller():\n    K()\n",
    )
    .unwrap();
    for (source, diagnostics) in [
        ("def K():\n    pass\nK = other\n", 0),
        ("def K(x, x):\n    pass\n", 1),
    ] {
        fs::write(&target, "def K():\n    pass\n").unwrap();
        index::run(root.path(), &db).unwrap();
        assert_eq!(callees(&Store::open(&db).unwrap(), "caller").edges.len(), 1);
        fs::write(&target, source).unwrap();
        let updated = index::run(root.path(), &db).unwrap();
        assert_eq!(updated.parsed_files, 1);
        assert_eq!(updated.unchanged_files, 1);
        assert_eq!(updated.diagnostics.len(), diagnostics);
        let graph = callees(&Store::open(&db).unwrap(), "caller");
        assert!(graph.edges.is_empty());
        assert_eq!(graph.unresolved.len(), 1);
    }
}

#[test]
fn lexical_calls_have_one_owner_and_classes_are_not_closures() {
    let facts = parse(
        "def target():\n    pass\n\nclass Service:\n    def target(self):\n        pass\n    async def run(self):\n        target()\n        self.target()\n\ndef outer():\n    def inner():\n        target()\n    inner()\n",
    );
    assert!(facts.diagnostics.is_empty());
    assert_eq!(facts.nodes.len(), 7);
    assert_eq!(facts.edges.len(), 6);
    assert_eq!(
        call(&facts, "Service.run", "target").candidate_keys,
        ["python:pkg.code:target"]
    );
    assert_eq!(
        call(&facts, "Service.run", "self.target").candidate_keys,
        ["python-receiver:pkg.code:Service.target"]
    );
    assert_eq!(
        call(&facts, "outer.inner", "target").candidate_keys,
        ["python:pkg.code:target"]
    );
    assert_eq!(
        call(&facts, "outer", "inner").candidate_keys,
        ["python:pkg.code:outer.inner"]
    );
    assert_eq!(
        facts
            .references
            .iter()
            .filter(|r| r.relation == "calls")
            .count(),
        4
    );
    let run = facts.nodes.iter().find(|n| n.label == "run").unwrap();
    assert_eq!(run.kind, "method");
    assert_eq!(run.line, Some(7));
    assert_eq!(run.end_line, Some(9));
    assert!(facts.nodes.iter().all(|n| !n.id.starts_with('/')));
    let again = parse_python(&facts.path, "def target():\n    pass\n", "another-hash").unwrap();
    assert_eq!(facts.nodes[1].id, again.nodes[1].id);

    // Explicit receiver lookup and lexical lookup keep distinct declarations.
    let context = PythonContext::from_facts(std::slice::from_ref(&facts));
    let mut resolved = facts;
    context.apply(&mut resolved);
    assert_eq!(
        call(&resolved, "Service.run", "target").candidate_keys,
        ["python:pkg.code:target"]
    );
    assert_eq!(
        call(&resolved, "Service.run", "self.target").candidate_keys,
        ["python-member:pkg.code:Service.target"]
    );
}

#[test]
fn lexical_calls_do_not_infer_receivers_from_shadowed_or_static_parameters() {
    for (body, owner) in [
        (
            "    async def run(self):\n        self = other\n        target()\n        self.target()\n",
            "Service.run",
        ),
        (
            "    @staticmethod\n    async def run(self):\n        target()\n        self.target()\n",
            "Service.run",
        ),
        (
            "    async def run(self):\n        def inner(self):\n            target()\n            self.target()\n",
            "Service.run.inner",
        ),
    ] {
        let mut facts = parse(&format!(
            "def target():\n    pass\nclass Service:\n    def target(self):\n        pass\n{body}"
        ));
        assert!(facts.diagnostics.is_empty());
        assert_eq!(
            call(&facts, owner, "target").candidate_keys,
            ["python:pkg.code:target"],
            "{body}"
        );
        assert!(
            call(&facts, owner, "self.target").candidate_keys.is_empty(),
            "{body}"
        );
        PythonContext::from_facts(std::slice::from_ref(&facts)).apply(&mut facts);
        assert_eq!(
            call(&facts, owner, "target").candidate_keys,
            ["python:pkg.code:target"],
            "{body}"
        );
        assert!(
            call(&facts, owner, "self.target").candidate_keys.is_empty(),
            "{body}"
        );
    }
}

#[test]
fn explicit_and_relative_imports_produce_binding_candidates() {
    let facts = parse(
        "from .helpers import work as task\nimport pkg.helpers as h\nimport pkg.tools\nfrom ..shared import job\ndef run():\n    task()\n    h.work()\n    pkg.tools.tool()\n    job()\n    h.object.method()\n",
    );
    assert_eq!(
        call(&facts, "run", "task").candidate_keys,
        ["python:pkg.helpers:work"]
    );
    assert_eq!(
        call(&facts, "run", "h.work").candidate_keys,
        ["python:pkg.helpers:work"]
    );
    assert_eq!(
        call(&facts, "run", "pkg.tools.tool").candidate_keys,
        ["python:pkg.tools:tool"]
    );
    assert!(call(&facts, "run", "job").candidate_keys.is_empty());
    assert_eq!(
        call(&facts, "run", "h.object.method").candidate_keys,
        ["python-member:pkg.helpers:object.method"]
    );
    let package = parse_python(
        "src/pkg/sub/__init__.py",
        "from ..helpers import work\nwork()\n",
        "h",
    )
    .unwrap();
    assert_eq!(package.module, "pkg.sub");
    assert!(
        package
            .references
            .iter()
            .any(|r| r.relation == "calls" && r.candidate_keys == ["python:pkg.helpers:work"])
    );
}

#[test]
fn shadowing_and_dynamic_syntax_do_not_guess_targets() {
    let cases = [
        "def run(target):\n    target()\n",
        "def run(target: int = 0):\n    target()\n",
        "def run(*target):\n    target()\n",
        "def run():\n    target()\n    target = other\n",
        "def run():\n    target, other = things\n    target()\n",
        "def run():\n    for target in things:\n        target()\n",
        "def run():\n    with context() as target:\n        target()\n",
        "def run():\n    try:\n        pass\n    except Exception as target:\n        target()\n",
        "def run():\n    (target := other)\n    target()\n",
        "def run():\n    del target\n    target()\n",
        "def run():\n    global target\n    target()\n",
        "def run():\n    [target() for target in things]\n",
        "def run():\n    value = lambda target: target()\n",
        "def run():\n    match value:\n        case target:\n            target()\n",
        "def run():\n    exec(code)\n    target()\n",
        "def run():\n    if condition:\n        from other import target\n    target()\n",
    ];
    for body in cases {
        let facts = parse(&format!("def target():\n    pass\n{body}"));
        assert!(
            facts.diagnostics.is_empty(),
            "{body}: {:?}",
            facts.diagnostics
        );
        assert!(
            call(&facts, "run", "target").candidate_keys.is_empty(),
            "{body}"
        );
    }
    let facts = parse("import other as m\nm.target = value\ndef run():\n    m.target()\n");
    assert!(call(&facts, "run", "m.target").candidate_keys.is_empty());
}

#[test]
fn uncertain_exports_are_not_published_as_definite_bindings() {
    for source in [
        "def target():\n    pass\ntarget = other\n",
        "def target():\n    pass\ndef target():\n    pass\n",
        "@decorator\ndef target():\n    pass\n",
        "if condition:\n    def target():\n        pass\n",
        "def target():\n    pass\nfrom other import *\n",
    ] {
        let facts = parse(source);
        assert!(facts.diagnostics.is_empty());
        assert!(
            facts
                .nodes
                .iter()
                .filter(|n| n.label == "target")
                .all(|n| n.binding_key.is_none()),
            "{source}"
        );
    }
    let before = parse("target()\ndef target():\n    pass\n");
    assert!(before.references[0].candidate_keys.is_empty());
    let recursive = parse("def target():\n    target()\n");
    assert_eq!(
        call(&recursive, "target", "target").candidate_keys,
        ["python:pkg.code:target"]
    );
}

#[test]
fn parse_errors_discard_all_facts_and_keep_a_location() {
    let facts = parse("def valid():\n    pass\ndef broken(:\n");
    assert!(facts.nodes.is_empty() && facts.edges.is_empty() && facts.references.is_empty());
    assert_eq!(facts.diagnostics.len(), 1);
    assert!(facts.diagnostics[0].line.is_some());
    assert!(parse_python("../outside.py", "", "h").is_err());
    assert!(parse_python("/absolute.py", "", "h").is_err());
}

#[test]
fn updates_hash_content_remove_facts_and_restore_incoming_references() {
    let root = tempdir().unwrap();
    let db = root.path().join(".graf/index.db");
    let target = root.path().join("target.py");
    fs::write(&target, "def target():\n    pass\n").unwrap();
    fs::write(
        root.path().join("consumer.py"),
        "from target import target\ndef caller():\n    target()\n",
    )
    .unwrap();
    let first = index::run(root.path(), &db).unwrap();
    assert_eq!(first.parsed_files, 2);
    assert_eq!(callees(&Store::open(&db).unwrap(), "caller").edges.len(), 1);
    let unchanged = index::run(root.path(), &db).unwrap();
    assert_eq!(unchanged.generation, first.generation);
    assert_eq!((unchanged.parsed_files, unchanged.unchanged_files), (0, 2));

    let modified = fs::metadata(&target).unwrap().modified().unwrap();
    fs::write(&target, "def other_():\n    pass\n").unwrap();
    fs::File::options()
        .write(true)
        .open(&target)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(modified))
        .unwrap();
    let stale = index::check_update(root.path(), &db).unwrap();
    assert!(!stale.fresh);
    assert_eq!(stale.changed, ["target.py"]);
    let changed = index::run(root.path(), &db).unwrap();
    assert_eq!((changed.parsed_files, changed.unchanged_files), (1, 1));
    let graph = callees(&Store::open(&db).unwrap(), "caller");
    assert!(graph.edges.is_empty());
    assert_eq!(graph.unresolved.len(), 1);

    fs::remove_file(&target).unwrap();
    let deleted = index::run(root.path(), &db).unwrap();
    assert_eq!(deleted.deleted_files, 1);
    assert_eq!(
        callees(&Store::open(&db).unwrap(), "caller")
            .unresolved
            .len(),
        1
    );
    fs::write(&target, "def target():\n    pass\n").unwrap();
    let restored = index::run(root.path(), &db).unwrap();
    assert_eq!(restored.parsed_files, 1);
    assert_eq!(callees(&Store::open(&db).unwrap(), "caller").edges.len(), 1);

    fs::write(&target, "def target(:\n").unwrap();
    let invalid = index::run(root.path(), &db).unwrap();
    assert_eq!(invalid.diagnostics.len(), 1);
    assert_eq!(invalid.diagnostics[0].file, "target.py");
    let store = Store::open(&db).unwrap();
    assert_eq!(callees(&store, "caller").unresolved.len(), 1);
    assert!(
        store
            .query("target", &QueryOptions::default())
            .unwrap()
            .nodes
            .iter()
            .all(|n| n.file != "target.py")
    );
    assert_eq!(
        index::run(root.path(), &db).unwrap().generation,
        invalid.generation
    );
}

#[test]
fn python_terminal_add_delete_rebinds_without_reparsing_unchanged_files() {
    let root = tempdir().unwrap();
    let db = root.path().join(".graf/index.db");
    fs::write(root.path().join("target.py"), "def target():\n    pass\n").unwrap();
    fs::write(
        root.path().join("consumer.py"),
        "from target import target\ndef caller():\n    target()\n",
    )
    .unwrap();
    let first = index::run(root.path(), &db).unwrap();
    assert_eq!(first.parsed_files, 2);
    assert_eq!(callees(&Store::open(&db).unwrap(), "caller").edges.len(), 1);

    fs::write(root.path().join("added.py"), "def added():\n    pass\n").unwrap();
    let added = index::run(root.path(), &db).unwrap();
    assert_eq!((added.parsed_files, added.unchanged_files), (1, 2));
    assert_eq!(callees(&Store::open(&db).unwrap(), "caller").edges.len(), 1);

    fs::remove_file(root.path().join("added.py")).unwrap();
    let deleted = index::run(root.path(), &db).unwrap();
    assert_eq!((deleted.parsed_files, deleted.unchanged_files), (0, 2));
    assert_eq!(callees(&Store::open(&db).unwrap(), "caller").edges.len(), 1);
}

#[test]
fn python_terminal_target_deletion_and_restore_keep_consumer_facts() {
    let root = tempdir().unwrap();
    let db = root.path().join(".graf/index.db");
    let target = root.path().join("target.py");
    fs::write(&target, "def target(): pass\n").unwrap();
    fs::write(
        root.path().join("consumer.py"),
        "from target import target\ndef caller(): target()\n",
    )
    .unwrap();
    index::run(root.path(), &db).unwrap();
    let consumer_stamp = || {
        Store::open_read_only(&db)
            .unwrap()
            .file_stamps()
            .unwrap()
            .into_iter()
            .find(|stamp| stamp.path == "consumer.py")
            .unwrap()
            .hash
    };
    let original_stamp = consumer_stamp();
    for present in [false, true, false, true] {
        if present {
            fs::write(&target, "def target(): pass\n").unwrap();
        } else {
            fs::remove_file(&target).unwrap();
        }
        assert!(
            !index::check_update(root.path(), &db)
                .unwrap()
                .changed
                .contains(&"consumer.py".into())
        );
        let update = index::run(root.path(), &db).unwrap();
        assert_eq!(update.parsed_files, usize::from(present));
        assert_eq!(update.unchanged_files, 1);
        assert_eq!(consumer_stamp(), original_stamp);
        let graph = callees(&Store::open_read_only(&db).unwrap(), "caller");
        assert_eq!(graph.edges.len(), usize::from(present));
        assert_eq!(graph.unresolved.len(), usize::from(!present));
        assert_eq!(index::run(root.path(), &db).unwrap().parsed_files, 0);
    }
}

#[test]
fn native_initial_publish_restores_wal_without_retaining_publish_pages() {
    let root = tempdir().unwrap();
    let db = root.path().join("graph.db");
    fs::write(root.path().join("app.py"), "def entry():\n    return 1\n").unwrap();

    index::run(root.path(), &db).unwrap();

    let conn = rusqlite::Connection::open(&db).unwrap();
    let mode: String = conn
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
    drop(conn);
    assert!(!db.with_file_name("graph.db-wal").exists());
    assert!(Store::open_read_only(&db).unwrap().stats().unwrap().files > 0);
}

#[test]
fn python_star_import_keeps_context_invalidation_on_add() {
    let root = tempdir().unwrap();
    let db = root.path().join(".graf/index.db");
    fs::write(
        root.path().join("consumer.py"),
        "from target import *\ndef caller():\n    target()\n",
    )
    .unwrap();
    let first = index::run(root.path(), &db).unwrap();
    assert_eq!(first.parsed_files, 1);

    fs::write(root.path().join("target.py"), "def target():\n    pass\n").unwrap();
    let added = index::run(root.path(), &db).unwrap();
    assert_eq!((added.parsed_files, added.unchanged_files), (2, 0));
    assert_eq!(callees(&Store::open(&db).unwrap(), "caller").edges.len(), 1);
}

#[test]
fn ignores_apply_without_git_and_database_files_are_excluded() {
    let root = tempdir().unwrap();
    let db = root.path().join("graph.db");
    fs::write(root.path().join(".gitignore"), "ignored/\n*.skip.py\n").unwrap();
    for dir in [
        "ignored",
        ".git",
        ".graf",
        ".venv",
        "node_modules",
        "build",
        "src",
    ] {
        fs::create_dir(root.path().join(dir)).unwrap();
        fs::write(
            root.path().join(dir).join("file.py"),
            "def item():\n    pass\n",
        )
        .unwrap();
    }
    fs::write(root.path().join("x.skip.py"), "broken syntax\n").unwrap();
    fs::write(root.path().join("README.md"), "Project\n").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(root.path().join("src/file.py"), root.path().join("link.py"))
        .unwrap();
    let report = index::run(root.path(), &db).unwrap();
    assert_eq!(report.parsed_files, 2);
    let store = Store::open(&db).unwrap();
    assert_eq!(
        store
            .file_stamps()
            .unwrap()
            .iter()
            .map(|s| s.path.as_str())
            .collect::<Vec<_>>(),
        vec!["README.md", "src/file.py"]
    );
    assert_eq!(store.stats().unwrap().coverage.unsupported_files, 1);
    fs::write(
        root.path().join(".gitignore"),
        "ignored/\n*.skip.py\nsrc/\n",
    )
    .unwrap();
    assert_eq!(index::run(root.path(), &db).unwrap().deleted_files, 1);
}

#[test]
fn oversized_and_non_utf8_files_clear_old_definitions() {
    let root = tempdir().unwrap();
    let db = root.path().join(".graf/index.db");
    let source = root.path().join("module.py");
    fs::write(&source, "def old():\n    pass\n").unwrap();
    index::run(root.path(), &db).unwrap();
    fs::write(&source, vec![b' '; 4 * 1024 * 1024 + 1]).unwrap();
    let oversized = index::run(root.path(), &db).unwrap();
    assert_eq!(oversized.nodes, 0);
    assert!(oversized.diagnostics[0].message.contains("4 MiB"));
    // A huge sparse source needs no content scan and keeps the same diagnostic facts.
    fs::File::options()
        .write(true)
        .open(&source)
        .unwrap()
        .set_len(8 * 1024 * 1024 * 1024)
        .unwrap();
    let still_oversized = index::run(root.path(), &db).unwrap();
    assert_eq!(still_oversized.generation, oversized.generation);
    assert_eq!(still_oversized.unchanged_files, 1);
    fs::write(&source, "def recovered():\n    pass\n").unwrap();
    let recovered = index::run(root.path(), &db).unwrap();
    assert_eq!(recovered.parsed_files, 1);
    assert_eq!(recovered.nodes, 2);
    assert!(recovered.diagnostics.is_empty());
    fs::write(&source, [0xff, 0xfe]).unwrap();
    let invalid = index::run(root.path(), &db).unwrap();
    assert_eq!(invalid.nodes, 0);
    assert!(invalid.diagnostics[0].message.contains("UTF-8"));
    fs::remove_file(&source).unwrap();
    assert!(index::run(root.path(), &db).unwrap().diagnostics.is_empty());
}

#[test]
fn roots_and_imported_snapshots_cannot_be_replaced() {
    let root = tempdir().unwrap();
    let other = tempdir().unwrap();
    let db = root.path().join(".graf/index.db");
    fs::write(root.path().join("module.py"), "def item():\n    pass\n").unwrap();
    let first = index::run(root.path(), &db).unwrap();
    assert!(index::run(other.path(), &db).is_err());
    let stats = Store::open(&db).unwrap().stats().unwrap();
    assert_eq!(stats.generation, first.generation);
    assert_eq!(
        stats.root.unwrap(),
        root.path().canonicalize().unwrap().to_str().unwrap()
    );
    let imported = other.path().join("graph.db");
    Store::create(&imported)
        .unwrap()
        .import_graph(ImportedGraph {
            nodes: vec![],
            edges: vec![],
            metadata: serde_json::Value::Null,
        })
        .unwrap();
    assert!(index::run(root.path(), &imported).is_err());
    assert_eq!(
        Store::open(&imported).unwrap().stats().unwrap().kind,
        "imported"
    );
}

#[cfg(unix)]
#[test]
fn traversal_failure_does_not_commit_a_partial_update() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempdir().unwrap();
    let db = root.path().join(".graf/index.db");
    fs::write(root.path().join("a.py"), "def original():\n    pass\n").unwrap();
    let first = index::run(root.path(), &db).unwrap();
    fs::write(root.path().join("a.py"), "def changed():\n    pass\n").unwrap();
    let blocked = root.path().join("blocked");
    fs::create_dir(&blocked).unwrap();
    fs::set_permissions(&blocked, fs::Permissions::from_mode(0o000)).unwrap();
    let inaccessible = fs::read_dir(&blocked).is_err();
    let result = index::run(root.path(), &db);
    fs::set_permissions(&blocked, fs::Permissions::from_mode(0o700)).unwrap();
    if inaccessible {
        assert!(result.is_err());
        let store = Store::open(&db).unwrap();
        assert_eq!(store.stats().unwrap().generation, first.generation);
        assert!(
            store
                .neighbors("original", &QueryOptions::default())
                .is_ok()
        );
    }
}

#[test]
fn old_extractor_stamps_refresh_unchanged_and_oversized_files() {
    let root = tempdir().unwrap();
    let db = root.path().join(".graf/index.db");
    let source = "def current():\n    pass\n";
    fs::write(root.path().join("module.py"), source).unwrap();
    fs::File::create(root.path().join("large.py"))
        .unwrap()
        .set_len(4 * 1024 * 1024 + 1)
        .unwrap();
    let content_hash = blake3::hash(source.as_bytes()).to_hex().to_string();
    // Cover both pre-revision caches and caches written by an earlier extractor.
    for prefix in ["", "python-v0:", "python-v1:", "python-v2:"] {
        let outdated = vec![
            parse_python(
                "module.py",
                "def obsolete():\n    pass\n",
                &format!("{prefix}{content_hash}"),
            )
            .unwrap(),
            parse_python(
                "large.py",
                "def obsolete_large():\n    pass\n",
                &format!("{prefix}oversized:4MiB"),
            )
            .unwrap(),
        ];
        Store::create(&db)
            .unwrap()
            .apply_native(
                root.path().canonicalize().unwrap().to_str().unwrap(),
                outdated,
                vec![],
                Coverage {
                    supported_files: 2,
                    ..Default::default()
                },
            )
            .unwrap();
        let updated = index::run(root.path(), &db).unwrap();
        assert_eq!(updated.parsed_files, 2);
        assert_eq!(updated.unchanged_files, 0);
        assert_eq!(updated.nodes, 2);
        assert_eq!(updated.diagnostics.len(), 1);
        let store = Store::open(&db).unwrap();
        assert!(store.neighbors("current", &QueryOptions::default()).is_ok());
        assert!(
            store
                .query("obsolete", &QueryOptions::default())
                .unwrap()
                .nodes
                .is_empty()
        );
        let stamps = store.file_stamps().unwrap();
        let normal = &stamps.iter().find(|s| s.path == "module.py").unwrap().hash;
        let oversized = &stamps.iter().find(|s| s.path == "large.py").unwrap().hash;
        assert!(normal.ends_with(&content_hash));
        assert_eq!(oversized, "python-v12:terminal-v2:oversized:4MiB");
        let unchanged = index::run(root.path(), &db).unwrap();
        assert_eq!(unchanged.generation, updated.generation);
        assert_eq!(unchanged.unchanged_files, 2);
    }
}

#[test]
fn an_active_writer_cannot_commit_mixed_file_contents() {
    use std::{
        io::{Seek, SeekFrom, Write},
        sync::{
            Arc, Barrier,
            atomic::{AtomicBool, Ordering},
        },
        time::{Duration, UNIX_EPOCH},
    };

    let root = tempdir().unwrap();
    let db = root.path().join(".graf/index.db");
    let source = root.path().join("changing.py");
    fs::write(&source, "def original():\n    pass\n").unwrap();
    let first = index::run(root.path(), &db).unwrap();
    let mut writer = fs::File::options().write(true).open(&source).unwrap();
    // A sparse file keeps fixture setup cheap while allowing writes during each bounded read.
    writer.set_len(4 * 1024 * 1024).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let ready = Arc::new(Barrier::new(2));
    let writer_stop = stop.clone();
    let writer_ready = ready.clone();
    let handle = std::thread::spawn(move || {
        writer_ready.wait();
        let mut tick = 1;
        while !writer_stop.load(Ordering::Relaxed) {
            writer.seek(SeekFrom::Start(0)).unwrap();
            writer.write_all(b"#").unwrap();
            // Advance by whole seconds even on filesystems with coarse timestamps.
            writer
                .set_times(
                    fs::FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs(tick)),
                )
                .unwrap();
            tick += 1;
            std::thread::yield_now();
        }
    });
    ready.wait();
    let result = index::run(root.path(), &db);
    stop.store(true, Ordering::Relaxed);
    handle.join().unwrap();
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("changed during both read attempts")
    );
    let store = Store::open(&db).unwrap();
    assert_eq!(store.stats().unwrap().generation, first.generation);
    assert!(
        store
            .neighbors("original", &QueryOptions::default())
            .is_ok()
    );
    // Once the writer finishes, the ordinary update can replace the old snapshot.
    fs::write(&source, "def stable():\n    pass\n").unwrap();
    let stable = index::run(root.path(), &db).unwrap();
    assert_eq!(stable.parsed_files, 1);
    assert_eq!(stable.nodes, 2);
    assert!(stable.diagnostics.is_empty());
}

#[test]
fn unreadable_ignore_rules_preserve_generation_and_recover() {
    for name in [
        ".gitignore",
        ".ignore",
        ".git/info/exclude",
        "nested/.gitignore",
    ] {
        let root = tempdir().unwrap();
        let db = root.path().join(".graf/index.db");
        let rules = root.path().join(name);
        fs::create_dir_all(rules.parent().unwrap()).unwrap();
        let scope = if name.starts_with("nested/") {
            root.path().join("nested")
        } else {
            root.path().to_path_buf()
        };
        let source = scope.join("visible.py");
        fs::write(&source, "def before():\n    pass\n").unwrap();
        fs::create_dir_all(scope.join("privaté")).unwrap();
        fs::write(scope.join("privaté/hidden.py"), "def hidden():\n    pass\n").unwrap();
        // Invalid rules inside an excluded subtree must remain irrelevant.
        fs::write(scope.join("privaté/.gitignore"), [0xff]).unwrap();
        fs::write(&rules, "privaté/\n").unwrap();
        let before = index::run(root.path(), &db).unwrap();
        let original = Store::open(&db).unwrap().file_stamps().unwrap();
        assert_eq!(original.len(), 1);
        fs::write(&source, "def after():\n    pass\n").unwrap();
        fs::write(&rules, b"privat\xe9/\n").unwrap();
        let error = index::run(root.path(), &db).unwrap_err().to_string();
        assert!(error.contains("ignore"), "{error}");
        let store = Store::open(&db).unwrap();
        assert_eq!(store.stats().unwrap().generation, before.generation);
        assert_eq!(store.file_stamps().unwrap()[0].hash, original[0].hash);
        assert_eq!(store.stats().unwrap().files, 1);
        drop(store);
        fs::write(&rules, "privaté/\n").unwrap();
        let after = index::run(root.path(), &db).unwrap();
        assert_eq!(after.generation, before.generation + 1);
        assert_eq!(after.parsed_files, 1);
        assert_eq!(Store::open(&db).unwrap().stats().unwrap().files, 1);
    }
}

#[test]
fn failed_inventory_preserves_whole_graph_then_valid_shrink_keeps_unchanged_sources() {
    let root = tempdir().unwrap();
    let db = root.path().join(".graf/index.db");
    let active = root.path().join("active.py");
    let provider = root.path().join("provider.py");
    fs::write(&provider, "def retained():\n    pass\n").unwrap();
    fs::write(
        &active,
        "from provider import retained\ndef old():\n    retained()\ndef removed_one():\n    pass\ndef removed_two():\n    pass\n",
    )
    .unwrap();
    index::run(root.path(), &db).unwrap();
    let before = Store::open_read_only(&db).unwrap().snapshot().unwrap();
    let retained: Vec<_> = before
        .nodes
        .iter()
        .filter(|node| node.file == "provider.py")
        .map(|node| serde_json::to_value(node).unwrap())
        .collect();
    assert!(!retained.is_empty());

    fs::write(
        &active,
        "from provider import retained\ndef replacement():\n    retained()\n",
    )
    .unwrap();
    let rules = root.path().join(".grafignore");
    fs::write(&rules, [0xff]).unwrap();
    assert!(index::run(root.path(), &db).is_err());
    assert_eq!(
        serde_json::to_value(Store::open_read_only(&db).unwrap().snapshot().unwrap()).unwrap(),
        serde_json::to_value(&before).unwrap()
    );

    fs::write(&rules, "").unwrap();
    let report = index::run(root.path(), &db).unwrap();
    assert_eq!((report.parsed_files, report.unchanged_files), (1, 1));
    let store = Store::open_read_only(&db).unwrap();
    let after = store.snapshot().unwrap();
    assert_eq!(after.generation, before.generation + 1);
    assert!(after.nodes.len() < before.nodes.len());
    assert_eq!(
        after
            .nodes
            .iter()
            .filter(|node| node.file == "provider.py")
            .map(|node| serde_json::to_value(node).unwrap())
            .collect::<Vec<_>>(),
        retained
    );
    assert!(
        after
            .nodes
            .iter()
            .all(|node| !["old", "removed_one", "removed_two"].contains(&node.label.as_str()))
    );
    let graph = callees(&store, "replacement");
    assert_eq!(graph.edges.len(), 1);
    assert!(graph.nodes.iter().any(|node| node.label == "retained"));
    drop(store);
    let unchanged = index::run(root.path(), &db).unwrap();
    assert_eq!(
        (unchanged.parsed_files, unchanged.generation),
        (0, after.generation)
    );
}

#[test]
fn scan_manifest_skips_context_and_matches_index_and_check_update_outputs() {
    let root = tempdir().unwrap();
    let db = root.path().join(".graf/index.db");
    let source = root.path().join("app.py");
    fs::write(&source, "def original():\n    pass\n").unwrap();

    let first = index::run(root.path(), &db).unwrap();
    let initial_manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(scan_manifest(&db)).unwrap()).unwrap();
    assert!(initial_manifest["scan"]["sources"][0]["identity"].is_null());
    std::thread::sleep(std::time::Duration::from_millis(2_100));
    let graph =
        serde_json::to_value(Store::open_read_only(&db).unwrap().snapshot().unwrap()).unwrap();
    let before = context_discoveries();
    assert!(index::check_update(root.path(), &db).unwrap().fresh);
    assert_eq!(context_discoveries(), before);
    let unchanged = index::run(root.path(), &db).unwrap();
    assert_eq!(context_discoveries(), before);
    let promoted_manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(scan_manifest(&db)).unwrap()).unwrap();
    assert!(promoted_manifest["scan"]["sources"][0]["identity"].is_object());
    assert_eq!(
        (
            unchanged.generation,
            unchanged.parsed_files,
            unchanged.unchanged_files,
            unchanged.nodes,
            unchanged.edges,
        ),
        (first.generation, 0, 1, first.nodes, first.edges,)
    );
    assert_eq!(
        serde_json::to_value(&unchanged.diagnostics).unwrap(),
        serde_json::to_value(&first.diagnostics).unwrap()
    );
    assert_eq!(
        serde_json::to_value(Store::open_read_only(&db).unwrap().snapshot().unwrap()).unwrap(),
        graph
    );

    let fresh = index::check_update(root.path(), &db).unwrap();
    assert!(fresh.fresh);
    assert!(fresh.added.is_empty() && fresh.changed.is_empty() && fresh.deleted.is_empty());
    assert_eq!(context_discoveries(), before);

    fs::write(&source, "def updated():\n    pass\n").unwrap();
    let stale = index::check_update(root.path(), &db).unwrap();
    assert!(!stale.fresh);
    assert_eq!(stale.changed, ["app.py"]);
    assert_eq!(context_discoveries(), before + 1);
    let updated = index::run(root.path(), &db).unwrap();
    assert_eq!(updated.parsed_files, 1);
    assert!(
        !Store::open_read_only(&db)
            .unwrap()
            .query("updated", &QueryOptions::default())
            .unwrap()
            .nodes
            .is_empty()
    );
    let after_update = context_discoveries();
    assert!(index::check_update(root.path(), &db).unwrap().fresh);
    assert_eq!(context_discoveries(), after_update);
}

#[test]
fn oversized_supported_files_do_not_disable_the_scan_manifest() {
    let root = tempdir().unwrap();
    let db = root.path().join(".graf/index.db");
    fs::write(root.path().join("app.py"), "def app():\n    pass\n").unwrap();
    let oversized = root.path().join("generated.ts");
    let file = fs::File::create(&oversized).unwrap();
    file.set_len(4 * 1024 * 1024 + 1).unwrap();

    let first = index::run(root.path(), &db).unwrap();
    assert_eq!(first.parsed_files, 2);
    assert!(scan_manifest(&db).is_file());
    let discoveries = context_discoveries();
    let second = index::run(root.path(), &db).unwrap();
    assert_eq!(
        (second.parsed_files, second.generation),
        (0, first.generation)
    );
    assert_eq!(context_discoveries(), discoveries);
}

#[test]
fn scan_manifest_invalidates_inventory_rules_options_and_generation() {
    let root = tempdir().unwrap();
    let db = root.path().join(".graf/index.db");
    let app = root.path().join("app.py");
    fs::write(&app, "def app():\n    pass\n").unwrap();
    index::run(root.path(), &db).unwrap();

    let mut count = context_discoveries();
    fs::write(root.path().join("added.py"), "def added():\n    pass\n").unwrap();
    assert_eq!(index::run(root.path(), &db).unwrap().parsed_files, 1);
    count += 1;
    assert_eq!(context_discoveries(), count);

    fs::write(&app, "def changed():\n    pass\n").unwrap();
    assert_eq!(index::run(root.path(), &db).unwrap().parsed_files, 1);
    count += 1;
    assert_eq!(context_discoveries(), count);

    fs::remove_file(root.path().join("added.py")).unwrap();
    assert_eq!(index::run(root.path(), &db).unwrap().deleted_files, 1);
    count += 1;
    assert_eq!(context_discoveries(), count);

    let rules = root.path().join(".grafignore");
    fs::write(&rules, "# first\n").unwrap();
    index::run(root.path(), &db).unwrap();
    count += 1;
    fs::write(&rules, "# second\n").unwrap();
    index::run(root.path(), &db).unwrap();
    count += 1;
    assert_eq!(context_discoveries(), count);

    let cargo = root.path().join("Cargo.toml");
    fs::write(&cargo, "[package]\nname='first'\nversion='0.1.0'\n").unwrap();
    index::run(root.path(), &db).unwrap();
    count += 1;
    fs::write(&cargo, "[package]\nname='second'\nversion='0.1.0'\n").unwrap();
    index::run(root.path(), &db).unwrap();
    count += 1;
    assert_eq!(context_discoveries(), count);

    let mut options = index::stored_options(&db).unwrap();
    options.no_gitignore = true;
    index::run_with_options(root.path(), &db, &options).unwrap();
    count += 1;
    assert_eq!(context_discoveries(), count);

    let root_text = root.path().canonicalize().unwrap();
    let manual = parse_python("app.py", "def manual():\n    pass\n", "manual-stamp").unwrap();
    let coverage = Store::open_read_only(&db)
        .unwrap()
        .stats()
        .unwrap()
        .coverage;
    Store::open(&db)
        .unwrap()
        .apply_native(root_text.to_str().unwrap(), vec![manual], vec![], coverage)
        .unwrap();
    let repaired = index::run(root.path(), &db).unwrap();
    count += 1;
    assert_eq!(context_discoveries(), count);
    assert_eq!(repaired.parsed_files, 1);
    assert!(
        !Store::open_read_only(&db)
            .unwrap()
            .query("changed", &QueryOptions::default())
            .unwrap()
            .nodes
            .is_empty()
    );
}

#[test]
fn scan_manifest_observes_ignored_project_context_inputs() {
    let root = tempdir().unwrap();
    let db = root.path().join(".graf/index.db");
    fs::write(root.path().join(".grafignore"), "/tsconfig.json\n").unwrap();
    fs::write(
        root.path().join("tsconfig.json"),
        r#"{"compilerOptions":{"baseUrl":".","paths":{"target":["one.ts"]}}}"#,
    )
    .unwrap();
    fs::write(root.path().join("one.ts"), "export function work() {}\n").unwrap();
    fs::write(root.path().join("two.ts"), "export function work() {}\n").unwrap();
    fs::write(
        root.path().join("main.ts"),
        "import {work} from 'target'; export function Main(){work();}\n",
    )
    .unwrap();

    index::run(root.path(), &db).unwrap();
    let linked_to = |path: &str| {
        let graph = Store::open_read_only(&db).unwrap().snapshot().unwrap();
        graph.edges.iter().any(|edge| {
            edge.relation == "calls"
                && graph
                    .nodes
                    .iter()
                    .any(|node| node.id == edge.source && node.file == "main.ts")
                && graph
                    .nodes
                    .iter()
                    .any(|node| node.id == edge.target && node.file == path)
        })
    };
    assert!(linked_to("one.ts"));
    let discoveries = context_discoveries();
    assert_eq!(index::run(root.path(), &db).unwrap().parsed_files, 0);
    assert_eq!(context_discoveries(), discoveries);

    fs::write(
        root.path().join("tsconfig.json"),
        r#"{"compilerOptions":{"baseUrl":".","paths":{"target":["two.ts"]}}}"#,
    )
    .unwrap();
    assert!(index::run(root.path(), &db).unwrap().parsed_files > 0);
    assert_eq!(context_discoveries(), discoveries + 1);
    assert!(linked_to("two.ts"));
    assert!(!linked_to("one.ts"));
}

#[test]
fn scan_manifest_bad_files_and_failed_indexes_are_safe_misses() {
    let root = tempdir().unwrap();
    let db = root.path().join(".graf/index.db");
    let source = root.path().join("app.py");
    fs::write(&source, "def before():\n    pass\n").unwrap();
    index::run(root.path(), &db).unwrap();
    let manifest = scan_manifest(&db);
    assert!(manifest.is_file());

    let mut count = context_discoveries();
    fs::remove_file(&manifest).unwrap();
    index::run(root.path(), &db).unwrap();
    count += 1;
    fs::write(&manifest, b"{").unwrap();
    index::run(root.path(), &db).unwrap();
    count += 1;
    fs::File::create(&manifest)
        .unwrap()
        .set_len(16 * 1024 * 1024 + 1)
        .unwrap();
    index::run(root.path(), &db).unwrap();
    count += 1;
    assert_eq!(context_discoveries(), count);

    #[cfg(unix)]
    {
        let target = root.path().join("manifest-target");
        fs::write(&target, b"{}").unwrap();
        fs::remove_file(&manifest).unwrap();
        std::os::unix::fs::symlink(&target, &manifest).unwrap();
        index::run(root.path(), &db).unwrap();
        count += 1;
        assert_eq!(context_discoveries(), count);
        assert!(
            !fs::symlink_metadata(&manifest)
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    fs::write(&source, "def after():\n    pass\n").unwrap();
    fs::write(root.path().join(".grafignore"), [0xff]).unwrap();
    assert!(index::run(root.path(), &db).is_err());
    fs::remove_file(root.path().join(".grafignore")).unwrap();
    let recovered = index::run(root.path(), &db).unwrap();
    assert_eq!(recovered.parsed_files, 1);
    assert!(
        !Store::open_read_only(&db)
            .unwrap()
            .query("after", &QueryOptions::default())
            .unwrap()
            .nodes
            .is_empty()
    );
}

#[test]
fn scan_manifest_binds_managed_sources_and_database_identity() {
    let root = tempdir().unwrap();
    let db = root.path().join(".graf/index.db");
    let source = "fixture://managed";
    let relative = sources::relative_path(source, "managed.py").unwrap();
    sources::save(
        root.path(),
        source,
        parse_python(&relative, "def first():\n    pass\n", "first").unwrap(),
    )
    .unwrap();
    index::run(root.path(), &db).unwrap();
    let count = context_discoveries();
    assert_eq!(index::run(root.path(), &db).unwrap().parsed_files, 0);
    assert_eq!(context_discoveries(), count);

    sources::save(
        root.path(),
        source,
        parse_python(&relative, "def second():\n    pass\n", "second").unwrap(),
    )
    .unwrap();
    assert_eq!(index::run(root.path(), &db).unwrap().parsed_files, 1);
    assert_eq!(context_discoveries(), count + 1);
    assert!(
        !Store::open_read_only(&db)
            .unwrap()
            .query("second", &QueryOptions::default())
            .unwrap()
            .nodes
            .is_empty()
    );

    #[cfg(unix)]
    {
        let replacement = root.path().join(".graf/replacement.db");
        index::run(root.path(), &replacement).unwrap();
        let replacement_graph = serde_json::to_value(
            Store::open_read_only(&replacement)
                .unwrap()
                .snapshot()
                .unwrap(),
        )
        .unwrap();
        fs::rename(&replacement, &db).unwrap();
        let before = context_discoveries();
        let report = index::run(root.path(), &db).unwrap();
        assert_eq!(context_discoveries(), before + 1);
        assert_eq!(report.parsed_files, 0);
        assert_eq!(
            serde_json::to_value(Store::open_read_only(&db).unwrap().snapshot().unwrap()).unwrap(),
            replacement_graph
        );
    }
}
