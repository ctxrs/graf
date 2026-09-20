use graf::{index, model::GraphSnapshot, store::Store};
use std::{fs, path::Path};

fn write(root: &Path, path: &str, text: &str) {
    let path = root.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn snapshot(root: &Path, db: &Path) -> GraphSnapshot {
    index::run(root, db).unwrap();
    Store::open_read_only(db).unwrap().snapshot().unwrap()
}

fn imports(graph: &GraphSnapshot, file: &str, binding: &str) -> usize {
    graph
        .edges
        .iter()
        .filter(|edge| {
            edge.relation == "imports"
                && graph
                    .nodes
                    .iter()
                    .any(|n| n.id == edge.source && n.file == file)
                && graph
                    .nodes
                    .iter()
                    .any(|n| n.id == edge.target && n.binding_key.as_deref() == Some(binding))
        })
        .count()
}

#[test]
fn declared_npm_subpaths_share_scoped_package_evidence_without_symbol_guesses() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("repo");
    let db = temp.path().join("graph.db");
    write(
        &root,
        "package.json",
        r#"{"name":"sample","dependencies":{"prism":"^1","@sample/tools":"^2"}}"#,
    );
    write(
        &root,
        "bare.ts",
        "import 'prism'; import '@sample/tools';\n",
    );
    write(
        &root,
        "nested/sub.ts",
        "import {paint} from 'prism/colors'; import '@sample/tools/deep/widget'; export function run() { paint(); }\n",
    );
    write(&root, "colors.py", "def paint():\n    return 'unrelated'\n");
    write(&root, "isolated/package.json", r#"{"name":"isolated"}"#);
    write(&root, "isolated/main.ts", "import 'prism/colors';\n");
    let first = snapshot(&root, &db);
    for file in ["bare.ts", "nested/sub.ts"] {
        for package in ["prism", "@sample/tools"] {
            assert_eq!(
                imports(
                    &first,
                    file,
                    &format!("npm:dependency:package.json:{package}")
                ),
                1
            );
        }
    }
    assert_eq!(
        first
            .nodes
            .iter()
            .filter(|n| n.kind == "dependency")
            .count(),
        2
    );
    assert_eq!(
        imports(
            &first,
            "isolated/main.ts",
            "npm:dependency:package.json:prism"
        ),
        0
    );
    assert!(!first.edges.iter().any(|e| e.relation == "calls"));
    let dependency = first
        .nodes
        .iter()
        .find(|n| n.kind == "dependency" && n.label == "prism")
        .unwrap()
        .id
        .clone();
    write(
        &root,
        "package.json",
        r#"{"name":"sample","dependencies":{"@sample/tools":"^2"}}"#,
    );
    let removed = snapshot(&root, &db);
    assert!(!removed.nodes.iter().any(|n| n.id == dependency));
    assert_eq!(
        imports(
            &removed,
            "nested/sub.ts",
            "npm:dependency:package.json:prism"
        ),
        0
    );
    write(
        &root,
        "package.json",
        r#"{"name":"sample","dependencies":{"prism":"^3","@sample/tools":"^2"}}"#,
    );
    let restored = snapshot(&root, &db);
    assert_eq!(
        imports(
            &restored,
            "nested/sub.ts",
            "npm:dependency:package.json:prism"
        ),
        1
    );
}

#[test]
fn local_npm_sources_precede_declared_dependency_navigation() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("repo");
    let db = temp.path().join("graph.db");
    write(
        &root,
        "package.json",
        r#"{"name":"sample","dependencies":{"prism":"file:./library"}}"#,
    );
    write(
        &root,
        "library/package.json",
        r#"{"name":"prism","exports":{"./colors":"./colors.ts"}}"#,
    );
    write(&root, "library/colors.ts", "export function paint() {}\n");
    write(
        &root,
        "main.ts",
        "import {paint} from 'prism/colors'; export function run() { paint(); }\n",
    );
    let graph = snapshot(&root, &db);
    assert_eq!(
        imports(&graph, "main.ts", "npm:dependency:package.json:prism"),
        0
    );
    assert!(graph.edges.iter().any(|e| {
        e.relation == "calls"
            && graph
                .nodes
                .iter()
                .any(|n| n.id == e.target && n.file == "library/colors.ts" && n.label == "paint")
    }));
    assert!(graph.edges.iter().any(|e| {
        e.relation == "imports"
            && graph
                .nodes
                .iter()
                .any(|n| n.id == e.target && n.file == "library/colors.ts")
    }));
}

#[test]
fn npm_declarations_require_actual_top_level_string_valued_groups() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("repo");
    let db = temp.path().join("graph.db");
    write(
        &root,
        "package.json",
        r#"{"name":"sample",
        "require":{"custom-setting":"x"},"dependencies":{"invalid-version":false,"first":"1"},
        "devDependencies":{"second":"1","first":"1"},"peerDependencies":{"third":"1"},
        "optionalDependencies":{"fourth":"1"},"custom":{"dependencies":{"nested":"1"}}}"#,
    );
    write(
        &root,
        "main.ts",
        "import 'first/deep'; import 'invalid-version'; import 'custom-setting'; import 'nested';\n",
    );
    let graph = snapshot(&root, &db);
    let mut names: Vec<_> = graph
        .nodes
        .iter()
        .filter(|n| n.kind == "dependency")
        .map(|n| n.label.as_str())
        .collect();
    names.sort();
    assert_eq!(names, ["first", "fourth", "second", "third"]);
    assert_eq!(
        imports(&graph, "main.ts", "npm:dependency:package.json:first"),
        1
    );
    for name in ["invalid-version", "custom-setting", "nested"] {
        assert_eq!(
            imports(
                &graph,
                "main.ts",
                &format!("npm:dependency:package.json:{name}")
            ),
            0
        );
    }
}

#[test]
fn decoded_attribute_whitespace_remains_a_search_boundary_across_updates() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("repo");
    let db = temp.path().join("graph.db");
    write(
        &root,
        "main.tf",
        "resource \"queue\" \"jobs\" {\n label = \"north\\nsouthneedle\"\n tags = { note = \"left\\trightneedle\", chain = [\"before\\rafterneedle\"] }\n}\n",
    );
    snapshot(&root, &db);
    let query = |term: &str| {
        Store::open_read_only(&db)
            .unwrap()
            .query(
                term,
                &graf::model::QueryOptions {
                    depth: 0,
                    ..Default::default()
                },
            )
            .unwrap()
    };
    for term in ["southneedle", "rightneedle", "afterneedle"] {
        assert!(
            query(term).nodes.iter().any(|n| n.label == "queue.jobs"),
            "{term}"
        );
    }
    write(
        &root,
        "main.tf",
        "resource \"queue\" \"jobs\" { label = \"new replacementneedle\" }\n",
    );
    snapshot(&root, &db);
    for term in ["southneedle", "rightneedle", "afterneedle"] {
        assert!(query(term).nodes.is_empty(), "{term}");
    }
    assert!(
        query("replacementneedle")
            .nodes
            .iter()
            .any(|n| n.label == "queue.jobs")
    );
    fs::remove_file(root.join("main.tf")).unwrap();
    snapshot(&root, &db);
    assert!(query("replacementneedle").nodes.is_empty());
}

#[test]
fn all_markdown_variants_reconcile_links_and_failed_parse_preserves_published_graph() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("repo");
    let db = temp.path().join("graph.db");
    write(&root, "target.md", "# Target\n");
    write(&root, "other.md", "# Other\n");
    for ext in ["md", "mdx", "qmd", "skill"] {
        write(
            &root,
            &format!("docs/guide.{ext}"),
            "# Guide\n[Authored target](../target.md)\n",
        );
    }
    let first = snapshot(&root, &db);
    let count = |g: &GraphSnapshot, target: &str| {
        g.edges
            .iter()
            .filter(|e| {
                e.relation == "references"
                    && g.nodes
                        .iter()
                        .any(|n| n.id == e.source && n.file.starts_with("docs/guide."))
                    && g.nodes.iter().any(|n| n.id == e.target && n.file == target)
            })
            .count()
    };
    assert_eq!(count(&first, "target.md"), 4);
    for ext in ["md", "mdx", "qmd", "skill"] {
        write(
            &root,
            &format!("docs/guide.{ext}"),
            "# Updated\n[Authored other](../other.md)\n",
        );
    }
    let changed = snapshot(&root, &db);
    assert_eq!(count(&changed, "target.md"), 0);
    assert_eq!(count(&changed, "other.md"), 4);
    write(
        &root,
        "docs/guide.skill",
        "---\nbroken: [\n---\n[Target](../target.md)\n",
    );
    assert!(index::run(&root, &db).is_err());
    let after_failure = Store::open_read_only(&db).unwrap().snapshot().unwrap();
    assert_eq!(
        serde_json::to_value(&after_failure).unwrap(),
        serde_json::to_value(&changed).unwrap()
    );
    write(
        &root,
        "docs/guide.skill",
        "# Fixed\n[Target](../target.md)\n",
    );
    let recovered = snapshot(&root, &db);
    assert_eq!(count(&recovered, "target.md"), 1);
    assert_eq!(count(&recovered, "other.md"), 3);
}

#[test]
fn attribute_search_migrates_version_two_only_during_explicit_update() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("repo");
    let db = temp.path().join("graph.db");
    write(
        &root,
        "queue.tf",
        "resource \"queue\" \"jobs\" { region = \"literalregion\" }\n",
    );
    let before = snapshot(&root, &db);
    let connection = rusqlite::Connection::open(&db).unwrap();
    connection.execute_batch("UPDATE metadata SET search_version=2; UPDATE nodes SET search=label; DELETE FROM node_search; INSERT INTO node_search(rowid,text) SELECT rowid,search FROM nodes;").unwrap();
    drop(connection);
    let bytes = fs::read(&db).unwrap();
    let options = graf::model::QueryOptions::default();
    let store = Store::open_read_only(&db).unwrap();
    assert!(
        store
            .query("literalregion", &options)
            .unwrap()
            .nodes
            .is_empty()
    );
    assert_eq!(store.stats().unwrap().generation, before.generation);
    drop(store);
    assert_eq!(fs::read(&db).unwrap(), bytes);
    let report = index::run(&root, &db).unwrap();
    assert_eq!(report.parsed_files, 0);
    assert!(report.generation > before.generation);
    let store = Store::open_read_only(&db).unwrap();
    let result = store.query("literalregion", &options).unwrap();
    assert_eq!(
        result
            .nodes
            .iter()
            .find(|n| n.label == "queue.jobs")
            .unwrap()
            .metadata["attributes"]["region"],
        "literalregion"
    );
    drop(store);
    assert_eq!(
        index::run(&root, &db).unwrap().generation,
        report.generation
    );
}
