use std::fs;

use graf::{index, model::*, parser::parse_python, store::Store};
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
    assert!(
        call(&facts, "Service.run", "self.target")
            .candidate_keys
            .is_empty()
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
    assert!(
        call(&facts, "run", "h.object.method")
            .candidate_keys
            .is_empty()
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
    assert_eq!(report.parsed_files, 1);
    let store = Store::open(&db).unwrap();
    assert_eq!(store.file_stamps().unwrap()[0].path, "src/file.py");
    assert_eq!(store.stats().unwrap().coverage.unsupported_files, 2);
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
    for prefix in ["", "python-v0:"] {
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
        let revision = normal.strip_suffix(&content_hash).unwrap();
        assert!(!revision.is_empty());
        assert_eq!(oversized, &format!("{revision}oversized:4MiB"));
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
