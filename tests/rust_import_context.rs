use graf::{
    index::{self, IndexOptions},
    model::{Direction, GraphSnapshot, QueryOptions},
    store::Store,
};
use std::{fs, path::Path};
use tempfile::tempdir;

fn write(root: &Path, path: &str, source: &str) {
    let path = root.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, source).unwrap();
}

fn index(root: &Path, force: bool) -> GraphSnapshot {
    let db = root.join(".graf/index.db");
    index::run_with_options(
        root,
        &db,
        &IndexOptions {
            code_only: true,
            force,
            ..Default::default()
        },
    )
    .unwrap();
    Store::open_read_only(&db).unwrap().snapshot().unwrap()
}

fn assert_callee(
    root: &Path,
    source_file: &str,
    source_label: &str,
    expected: Option<(&str, &str)>,
) {
    let store = Store::open_read_only(&root.join(".graf/index.db")).unwrap();
    let graph = store.snapshot().unwrap();
    let source = graph
        .nodes
        .iter()
        .find(|node| node.file == source_file && node.label == source_label)
        .unwrap();
    let result = store
        .neighbors(
            &source.id,
            &QueryOptions {
                direction: Direction::Outgoing,
                relation: Some("calls".into()),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(result.edges.len(), usize::from(expected.is_some()));
    assert_eq!(result.unresolved.len(), usize::from(expected.is_none()));
    if let Some((file, label)) = expected {
        let target = graph
            .nodes
            .iter()
            .find(|node| node.id == result.edges[0].target)
            .unwrap();
        assert_eq!((target.file.as_str(), target.label.as_str()), (file, label));
        assert!(matches!(target.kind.as_str(), "function" | "method"));
    }
}

#[test]
fn bare_and_self_reexports_follow_a_declared_module_chain() {
    for spelling in ["command::Command", "self::command::Command"] {
        let dir = tempdir().unwrap();
        let root = dir.path();
        write(
            root,
            "Cargo.toml",
            "[package]\nname='demo'\nversion='0.1.0'\nedition='2021'\n",
        );
        let library = "pub mod builder; pub use crate::builder::Command;\n";
        write(root, "src/lib.rs", library);
        write(
            root,
            "src/builder/mod.rs",
            &format!("mod command; pub use {spelling};\n"),
        );
        let provider = "pub struct Command; impl Command { pub fn new() -> Self { Self } }\n";
        write(root, "src/builder/command.rs", provider);
        index(root, false);

        write(root, "src/lib.rs", &format!("{library}mod probe;\n"));
        write(
            root,
            "src/probe.rs",
            "use crate::Command; pub fn graf_parity_probe() -> Command { Command::new() }\n",
        );
        let added = index(root, false);
        let target = Some(("src/builder/command.rs", "new"));
        assert_callee(root, "src/probe.rs", "graf_parity_probe", target);
        assert_eq!(index(root, false).generation, added.generation);
        assert_callee(root, "src/probe.rs", "graf_parity_probe", target);
        index(root, true);
        assert_callee(root, "src/probe.rs", "graf_parity_probe", target);

        fs::remove_file(root.join("src/builder/command.rs")).unwrap();
        index(root, false);
        assert_callee(root, "src/probe.rs", "graf_parity_probe", None);
        write(root, "src/builder/command.rs", provider);
        index(root, false);
        assert_callee(root, "src/probe.rs", "graf_parity_probe", target);

        fs::remove_file(root.join("src/probe.rs")).unwrap();
        write(root, "src/lib.rs", library);
        let deleted = index(root, false);
        assert!(!deleted.nodes.iter().any(|node| node.file == "src/probe.rs"));
        assert!(
            index::check_update(root, &root.join(".graf/index.db"))
                .unwrap()
                .fresh
        );
    }
}

#[test]
fn bare_import_does_not_choose_an_ambiguous_module_file() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "Cargo.toml",
        "[package]\nname='demo'\nversion='0.1.0'\nedition='2021'\n",
    );
    write(
        root,
        "src/lib.rs",
        "mod child; use child::work; pub fn caller() { work(); }\n",
    );
    write(root, "src/child.rs", "pub fn work() {}\n");
    // Both layouts exist, but only one supplies work. Name uniqueness cannot
    // substitute for an unambiguous declared module file.
    write(root, "src/child/mod.rs", "pub fn other() {}\n");
    index(root, false);
    assert_callee(root, "src/lib.rs", "caller", None);
    index(root, true);
    assert_callee(root, "src/lib.rs", "caller", None);

    fs::remove_file(root.join("src/child/mod.rs")).unwrap();
    index(root, false);
    assert_callee(root, "src/lib.rs", "caller", Some(("src/child.rs", "work")));
    write(root, "src/child/mod.rs", "pub fn other() {}\n");
    index(root, false);
    assert_callee(root, "src/lib.rs", "caller", None);
}

#[test]
fn a_blocked_local_module_does_not_fall_through_to_a_dependency() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "Cargo.toml",
        "[package]\nname='demo'\nversion='0.1.0'\nedition='2021'\n[dependencies]\nchild={path='dep'}\n",
    );
    write(
        root,
        "dep/Cargo.toml",
        "[package]\nname='child'\nversion='0.1.0'\nedition='2021'\n",
    );
    write(root, "dep/src/lib.rs", "pub fn work() {}\n");
    write(root, "src/child.rs", "pub fn work() {}\n");
    for (declaration, expected) in [
        ("", Some(("dep/src/lib.rs", "work"))),
        ("mod child;", Some(("src/child.rs", "work"))),
        ("#[cfg(feature = \"alternate\")] mod child;", None),
        ("mod child; mod child;", None),
        ("mod child;", Some(("src/child.rs", "work"))),
        ("", Some(("dep/src/lib.rs", "work"))),
    ] {
        write(
            root,
            "src/lib.rs",
            &format!("{declaration}\nuse child::work; pub fn caller() {{ work(); }}\n"),
        );
        index(root, false);
        assert_callee(root, "src/lib.rs", "caller", expected);
        index(root, true);
        assert_callee(root, "src/lib.rs", "caller", expected);
    }
}

#[test]
fn a_missing_parent_module_cannot_borrow_an_orphan_descendant() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "Cargo.toml",
        "[package]\nname='demo'\nversion='0.1.0'\nedition='2021'\n",
    );
    write(
        root,
        "src/lib.rs",
        "mod child; pub fn child() {} pub fn value() { child(); } pub fn caller() { crate::child::grand::work(); }\n",
    );
    write(
        root,
        "src/child/grand.rs",
        "pub fn work() {} pub fn local() { work(); }\n",
    );
    for present in [false, true, false] {
        if present {
            write(root, "src/child.rs", "pub mod grand;\n");
        } else if root.join("src/child.rs").exists() {
            fs::remove_file(root.join("src/child.rs")).unwrap();
        }
        for force in [false, true] {
            index(root, force);
            // A function occupies the value namespace, separately from mod child.
            assert_callee(root, "src/lib.rs", "value", Some(("src/lib.rs", "child")));
            assert_callee(
                root,
                "src/lib.rs",
                "caller",
                present.then_some(("src/child/grand.rs", "work")),
            );
            // Ordinary definitions and navigation inside the orphan survive.
            assert_callee(
                root,
                "src/child/grand.rs",
                "local",
                Some(("src/child/grand.rs", "work")),
            );
        }
    }

    let standalone = tempdir().unwrap();
    write(
        standalone.path(),
        "plain.rs",
        "fn work() {} fn caller() { work(); }\n",
    );
    index(standalone.path(), false);
    assert_callee(
        standalone.path(),
        "plain.rs",
        "caller",
        Some(("plain.rs", "work")),
    );
}
