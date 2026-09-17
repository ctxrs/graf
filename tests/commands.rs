//! CLI acceptance checks with explicit synthetic graph stores and homes.
use graf::{
    model::{Edge, ImportedGraph, Node},
    store::Store,
};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};
use tempfile::{TempDir, tempdir};

struct Sandbox {
    _temp: TempDir,
    root: PathBuf,
    home: PathBuf,
}
impl Sandbox {
    fn new() -> Self {
        let temp = tempdir().unwrap();
        let root = temp.path().join("project paths with spaces");
        let home = temp.path().join("synthetic home");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&home).unwrap();
        Self {
            _temp: temp,
            root,
            home,
        }
    }
    fn cli(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_graf"))
            .current_dir(&self.root)
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .env("XDG_CONFIG_HOME", self.home.join(".config"))
            .args(args)
            .output()
            .unwrap()
    }
    fn ok(&self, args: &[&str]) -> Value {
        success(self.cli(args))
    }
    fn db(&self, name: &str, label: &str) -> PathBuf {
        let path = self.root.join(name).join(".graf/index.db");
        Store::create(&path)
            .unwrap()
            .import_graph(graph(label))
            .unwrap();
        path
    }
}
fn success(output: Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn snapshot_exports_guard_shrink_and_preserve_previous_bytes() {
    let s = Sandbox::new();
    let db = s.db("source", "example");
    let output = s.root.join("graph.json");
    s.ok(&[
        "--db",
        string(&db),
        "export",
        "snapshot-json",
        "--output",
        string(&output),
    ]);
    let original = fs::read(&output).unwrap();
    let small = s.root.join("small.json");
    let mut graph = Store::open_read_only(&db).unwrap().snapshot().unwrap();
    graph.nodes.retain(|n| n.id == "a");
    graph.edges.clear();
    fs::write(&small, serde_json::to_vec(&graph).unwrap()).unwrap();
    fails(
        s.cli(&[
            "export",
            "snapshot-json",
            "--snapshot",
            string(&small),
            "--output",
            string(&output),
        ]),
        "would shrink",
    );
    assert_eq!(fs::read(&output).unwrap(), original);
    s.ok(&[
        "export",
        "snapshot-json",
        "--snapshot",
        string(&small),
        "--output",
        string(&output),
        "--allow-shrink",
    ]);
    let directory = s.root.join(".graf/export-backups");
    let backup = directory.join(format!("{}.bak", blake3::hash(&original).to_hex()));
    assert_eq!(fs::read(&backup).unwrap(), original);
    assert_eq!(graf::snapshot::read(&backup).unwrap().nodes.len(), 3);
    let changed = fs::metadata(&output).unwrap().modified().unwrap();
    s.ok(&[
        "export",
        "snapshot-json",
        "--snapshot",
        string(&small),
        "--output",
        string(&output),
    ]);
    assert_eq!(fs::metadata(&output).unwrap().modified().unwrap(), changed);
    assert_eq!(fs::read_dir(&directory).unwrap().count(), 1);
    fs::write(&output, "{broken").unwrap();
    fails(
        s.cli(&[
            "export",
            "snapshot-json",
            "--snapshot",
            string(&small),
            "--output",
            string(&output),
            "--allow-shrink",
        ]),
        "existing graph output is invalid",
    );
    assert_eq!(fs::read_to_string(&output).unwrap(), "{broken");
    fs::write(&output, "").unwrap();
    s.ok(&[
        "export",
        "snapshot-json",
        "--snapshot",
        string(&small),
        "--output",
        string(&output),
    ]);
}

#[test]
fn ordinary_export_backup_failure_preserves_user_content() {
    let s = Sandbox::new();
    let db = s.db("source", "example");
    let output = s.root.join("report.md");
    fs::write(&output, "Human notes worth keeping").unwrap();
    fs::write(s.root.join(".graf"), "not a directory").unwrap();
    fails(
        s.cli(&["--db", string(&db), "report", "--output", string(&output)]),
        "backup path must be a regular directory",
    );
    assert_eq!(
        fs::read_to_string(&output).unwrap(),
        "Human notes worth keeping"
    );
    fs::remove_file(s.root.join(".graf")).unwrap();
    s.ok(&["--db", string(&db), "report", "--output", string(&output)]);
    let backup = s.root.join(".graf/export-backups").join(format!(
        "{}.bak",
        blake3::hash(b"Human notes worth keeping").to_hex()
    ));
    assert_eq!(
        fs::read_to_string(backup).unwrap(),
        "Human notes worth keeping"
    );
}

#[test]
fn global_links_public_namespace_calls_and_refresh_removes_inaccessible_targets() {
    let s = Sandbox::new();
    let caller = s.root.join("caller");
    let provider = s.root.join("provider");
    fs::create_dir(&caller).unwrap();
    fs::create_dir(&provider).unwrap();
    fs::write(caller.join("Main.java"), "package app; import api.Tools; public class Main { public static void run() { Tools.work(); } }\n").unwrap();
    fs::write(
        provider.join("Tools.java"),
        "package api; public class Tools { public static void work() {} }\n",
    )
    .unwrap();
    s.ok(&["index", string(&caller), "--json"]);
    s.ok(&["index", string(&provider), "--json"]);
    let registry = s.root.join("registry.db");
    s.ok(&[
        "--db",
        string(&registry),
        "global",
        "add",
        "caller",
        string(&caller),
    ]);
    let linked = s.ok(&[
        "--db",
        string(&registry),
        "global",
        "add",
        "provider",
        string(&provider),
    ]);
    assert!(linked["reference_links"].as_u64().unwrap() >= 1);
    let graph = Store::open_read_only(&registry)
        .unwrap()
        .snapshot()
        .unwrap();
    let calls: Vec<_> = graph
        .edges
        .iter()
        .filter(|e| e.relation == "calls")
        .collect();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].confidence, "INFERRED");
    assert_eq!(
        calls[0].metadata["graf_composition"]["source_project"],
        "caller"
    );
    assert_eq!(
        calls[0].metadata["graf_composition"]["target_project"],
        "provider"
    );
    assert!(
        s.ok(&["--db", string(&registry), "global", "refresh"])["unchanged"]
            .as_bool()
            .unwrap()
    );
    let merged = s.root.join("merged.db");
    let selected = s.root.join("linked.db");
    let a = format!("caller={}", caller.display());
    let b = format!("provider={}", provider.display());
    s.ok(&[
        "merge",
        "--project",
        &a,
        "--project",
        &b,
        "--output",
        string(&merged),
    ]);
    assert!(
        !Store::open_read_only(&merged)
            .unwrap()
            .snapshot()
            .unwrap()
            .edges
            .iter()
            .any(|e| e.relation == "calls")
    );
    s.ok(&[
        "merge",
        "--project",
        &a,
        "--project",
        &b,
        "--output",
        string(&selected),
        "--link-references",
    ]);
    assert_eq!(
        Store::open_read_only(&selected)
            .unwrap()
            .snapshot()
            .unwrap()
            .edges
            .iter()
            .filter(|e| e.relation == "calls")
            .count(),
        1
    );
    fs::write(
        provider.join("Tools.java"),
        "package api; public class Tools { private static void work() {} }\n",
    )
    .unwrap();
    s.ok(&["index", string(&provider), "--json"]);
    s.ok(&["--db", string(&registry), "global", "refresh"]);
    assert!(
        !Store::open_read_only(&registry)
            .unwrap()
            .snapshot()
            .unwrap()
            .edges
            .iter()
            .any(|e| e.relation == "calls")
    );
}
fn fails(output: Output, message: &str) {
    assert!(!output.status.success(), "unexpected success");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains(message),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
fn graph(label: &str) -> ImportedGraph {
    let nodes = ["a", "b", "isolated"]
        .into_iter()
        .map(|id| Node {
            id: id.into(),
            label: format!("{label}_{id}"),
            kind: "function".into(),
            file: format!("{id}.py"),
            line: Some(1),
            end_line: Some(3),
            qualified_name: None,
            binding_key: None,
            metadata: json!({"user":"kept"}),
        })
        .collect();
    let edges = [true, false, true]
        .into_iter()
        .enumerate()
        .map(|(i, directed)| Edge {
            id: format!("edge{i}"),
            source: "a".into(),
            target: "b".into(),
            relation: "calls".into(),
            directed,
            file: Some("a.py".into()),
            line: Some(i as u32 + 1),
            confidence: "EXTRACTED".into(),
            metadata: json!({"weight":1}),
        })
        .collect();
    ImportedGraph {
        nodes,
        edges,
        metadata: json!({"fixture":label}),
    }
}
fn string(path: &Path) -> &str {
    path.to_str().unwrap()
}

#[test]
fn analysis_communities_and_hubs_use_existing_snapshot() {
    let s = Sandbox::new();
    let db = s.db("local", "alpha");
    let generation = Store::open(&db).unwrap().stats().unwrap().generation;
    let report = s.ok(&["--json", "--db", string(&db), "analyze"]);
    assert_eq!(report["nodes"].as_array().unwrap().len(), 3);
    assert_eq!(report["isolates"], json!(["isolated"]));
    assert_eq!(report["call_edges"].as_array().unwrap().len(), 3);
    let communities = s.ok(&["--json", "--db", string(&db), "communities"]);
    assert_eq!(communities["communities"].as_array().unwrap().len(), 2);
    let one = s.ok(&["--json", "--db", string(&db), "communities", "--id", "0"]);
    assert_eq!(one["communities"].as_array().unwrap().len(), 1);
    fails(
        s.cli(&["--db", string(&db), "communities", "--id", "999"]),
        "does not exist",
    );
    let hubs = s.ok(&["--json", "--db", string(&db), "hubs", "--top", "1"]);
    assert_eq!(hubs["hubs"][0]["node"]["id"], "a");
    assert_eq!(hubs["hubs"][0]["metrics"]["degree"], 3);
    assert_eq!(hubs["truncated"], true);
    let ranks = s.ok(&[
        "--json",
        "--db",
        string(&db),
        "hubs",
        "--sort",
        "pagerank",
        "--top",
        "3",
    ]);
    // All of a's outgoing weight goes to b, and b's undirected return arc
    // sends all of its outgoing weight to a. Parallel arcs do not change
    // these normalized probabilities: both rank equally, then IDs break ties.
    assert_eq!(ranks["hubs"][0]["node"]["id"], "a");
    assert_eq!(ranks["hubs"][1]["node"]["id"], "b");
    let a_rank = ranks["hubs"][0]["metrics"]["pagerank"].as_f64().unwrap();
    let b_rank = ranks["hubs"][1]["metrics"]["pagerank"].as_f64().unwrap();
    let isolated_rank = ranks["hubs"][2]["metrics"]["pagerank"].as_f64().unwrap();
    assert!((a_rank - b_rank).abs() < 1e-12);
    assert!(a_rank > isolated_rank);
    let output = s.root.join("analysis report.json");
    s.ok(&["--db", string(&db), "analyze", "--output", string(&output)]);
    let saved: Value = serde_json::from_slice(&fs::read(output).unwrap()).unwrap();
    assert_eq!(saved, report);
    assert_eq!(
        Store::open(&db).unwrap().stats().unwrap().generation,
        generation
    );
}

#[test]
fn every_single_file_export_and_report_are_callable() {
    let s = Sandbox::new();
    let db = s.db("local", "alpha");
    for (format, marker) in [
        ("snapshot-json", "schema_version"),
        ("graphify-json", "multigraph"),
        ("graphml", "<graphml"),
        ("cypher", "MERGE"),
        ("mermaid", "flowchart"),
        ("svg", "<svg"),
        ("html", "<html"),
        ("markdown", "#"),
        ("canvas", "nodes"),
        ("callflow-html", "<html"),
        ("tree-html", "<html"),
    ] {
        let output = s.root.join(format!("{format} output"));
        if !matches!(format, "snapshot-json" | "graphify-json") {
            fs::write(&output, b"replace this existing export").unwrap();
        }
        let result = s.ok(&[
            "--json",
            "--db",
            string(&db),
            "export",
            format,
            "--output",
            string(&output),
        ]);
        assert_eq!(result["format"], format);
        let bytes = fs::read_to_string(output).unwrap();
        assert!(
            bytes.contains(marker),
            "{format}: {}",
            &bytes[..bytes.len().min(200)]
        );
        assert!(!bytes.contains("replace this existing export"));
    }
    let output = s.cli(&["--db", string(&db), "report"]);
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).starts_with('#'));
    let json = s.ok(&["--json", "--db", string(&db), "report", "--format", "html"]);
    assert_eq!(json["format"], "html");
    assert!(json["content"].as_str().unwrap().contains("<html"));
}

#[test]
fn wiki_and_obsidian_leave_existing_notes_untouched() {
    let s = Sandbox::new();
    let db = s.db("local", "alpha");
    let vault = s.root.join("vault with notes");
    fs::create_dir(&vault).unwrap();
    fs::write(vault.join("index.md"), "user note").unwrap();
    let first = s.ok(&[
        "--json",
        "--db",
        string(&db),
        "report",
        "--format",
        "wiki",
        "--output",
        string(&vault),
    ]);
    let generated = PathBuf::from(first["directory"].as_str().unwrap());
    assert!(generated.join("index.md").is_file());
    assert!(generated.join("graph.canvas").is_file());
    let second = s.ok(&[
        "--json",
        "--db",
        string(&db),
        "export",
        "obsidian",
        "--output",
        string(&vault),
    ]);
    assert_ne!(first["directory"], second["directory"]);
    assert_eq!(
        fs::read_to_string(vault.join("index.md")).unwrap(),
        "user note"
    );
    fails(
        s.cli(&["--db", string(&db), "export", "wiki"]),
        "require --output",
    );
}

#[test]
fn merge_namespaces_collisions_and_snapshot_provenance() {
    let s = Sandbox::new();
    let one = s.db("one", "one");
    let two = s.db("two", "two");
    let snapshot = s.root.join("input snapshot.json");
    s.ok(&[
        "--db",
        string(&two),
        "export",
        "snapshot-json",
        "--output",
        string(&snapshot),
    ]);
    let from_json = s.ok(&["--json", "analyze", "--snapshot", string(&snapshot)]);
    assert_eq!(from_json["nodes"].as_array().unwrap().len(), 3);
    let output = s.root.join("merged.db");
    s.ok(&[
        "--json",
        "merge",
        "--project",
        &format!(
            "first={}",
            one.parent().unwrap().parent().unwrap().display()
        ),
        "--snapshot",
        &format!("second={}", snapshot.display()),
        "--output",
        string(&output),
    ]);
    let combined = Store::open(&output).unwrap().snapshot().unwrap();
    assert_eq!((combined.nodes.len(), combined.edges.len()), (6, 6));
    let ids: std::collections::BTreeSet<_> = combined.nodes.iter().map(|n| &n.id).collect();
    assert_eq!(ids.len(), 6);
    assert!(
        combined
            .edges
            .iter()
            .all(|e| ids.contains(&e.source) && ids.contains(&e.target))
    );
    assert_eq!(
        combined
            .nodes
            .iter()
            .filter(|n| n.metadata["project"] == "first")
            .count(),
        3
    );
    assert_eq!(combined.metadata["projects"][1]["generation"], 1);
    assert_eq!(
        combined.metadata["projects"][1]["metadata"]["fixture"],
        "two"
    );
    assert_eq!(Store::open(&one).unwrap().stats().unwrap().nodes, 3);
    fails(
        s.cli(&[
            "merge",
            "--project",
            &format!("x={}", one.display()),
            "--project",
            &format!("x={}", two.display()),
            "--output",
            string(&s.root.join("duplicate.db")),
        ]),
        "duplicate",
    );
    assert!(!s.root.join("duplicate.db").exists());
    fails(
        s.cli(&[
            "merge",
            "--project",
            &format!("x={}", one.display()),
            "--output",
            string(&s.root.join("one.db")),
        ]),
        "at least two",
    );
}

#[test]
fn global_registry_reads_survive_missing_sources_and_failed_refresh_is_atomic() {
    let s = Sandbox::new();
    let one = s.db("one", "before");
    let two = s.db("two", "other");
    let aggregate = s.root.join("registry aggregate.db");
    s.ok(&[
        "--json",
        "--db",
        string(&aggregate),
        "global",
        "add",
        "one",
        string(&one),
    ]);
    s.ok(&[
        "--json",
        "--db",
        string(&aggregate),
        "global",
        "add",
        "two",
        string(&two),
    ]);
    let original = Store::open(&aggregate).unwrap().snapshot().unwrap();
    let query = s.ok(&[
        "--json",
        "--db",
        string(&aggregate),
        "global",
        "query",
        "before_a",
        "--depth",
        "0",
        "--limit",
        "1",
    ]);
    assert_eq!(query["nodes"].as_array().unwrap().len(), 1);
    Store::open(&one)
        .unwrap()
        .refresh_import(graph("after"))
        .unwrap();
    let stale = s.ok(&[
        "--json",
        "--db",
        string(&aggregate),
        "global",
        "query",
        "before_a",
    ]);
    assert!(!stale["nodes"].as_array().unwrap().is_empty());
    assert_eq!(
        Store::open(&aggregate).unwrap().stats().unwrap().generation,
        original.generation
    );
    s.ok(&["--json", "--db", string(&aggregate), "global", "refresh"]);
    let fresh = s.ok(&[
        "--json",
        "--db",
        string(&aggregate),
        "global",
        "query",
        "after_a",
    ]);
    assert!(!fresh["nodes"].as_array().unwrap().is_empty());
    fs::remove_file(&two).unwrap();
    let before_failure = Store::open(&aggregate).unwrap().snapshot().unwrap();
    let listed = s.ok(&["--json", "--db", string(&aggregate), "global", "list"]);
    assert_eq!(listed["entries"].as_array().unwrap().len(), 2);
    s.ok(&[
        "--json",
        "--db",
        string(&aggregate),
        "global",
        "query",
        "other_a",
    ]);
    fails(
        s.cli(&["--db", string(&aggregate), "global", "refresh"]),
        "aggregate unchanged",
    );
    fails(
        s.cli(&[
            "--db",
            string(&aggregate),
            "global",
            "add",
            "new",
            string(&one),
        ]),
        "aggregate unchanged",
    );
    let after_failure = Store::open(&aggregate).unwrap().snapshot().unwrap();
    assert_eq!(
        serde_json::to_value(&before_failure).unwrap(),
        serde_json::to_value(&after_failure).unwrap()
    );
    s.ok(&[
        "--json",
        "--db",
        string(&aggregate),
        "global",
        "remove",
        "two",
    ]);
    s.ok(&[
        "--json",
        "--db",
        string(&aggregate),
        "global",
        "remove",
        "one",
    ]);
    let empty = Store::open(&aggregate).unwrap().snapshot().unwrap();
    assert!(empty.nodes.is_empty());
    assert_eq!(empty.metadata["graf_registry"]["entries"], json!([]));
    fails(
        s.cli(&["--db", string(&aggregate), "global", "remove", "missing"]),
        "not registered",
    );
}

#[test]
fn default_global_home_is_explicit_and_list_is_nonmutating() {
    let s = Sandbox::new();
    let source = s.db("one", "source");
    let listing = s.ok(&["--json", "global", "list"]);
    assert_eq!(listing["entries"], json!([]));
    assert!(!s.home.join(".graf").exists());
    s.ok(&["--json", "global", "path"]);
    assert!(!s.home.join(".graf").exists());
    fails(
        s.cli(&["global", "query", "anything"]),
        "no global aggregate",
    );
    let snapshot = s.root.join("global input.json");
    s.ok(&[
        "--db",
        string(&source),
        "export",
        "snapshot-json",
        "--output",
        string(&snapshot),
    ]);
    s.ok(&[
        "--json",
        "global",
        "add",
        "snapshot",
        string(&snapshot),
        "--snapshot",
    ]);
    assert!(s.home.join(".graf/global.db").is_file());
    let result = s.ok(&["--json", "global", "query", "source_a"]);
    assert!(!result["nodes"].as_array().unwrap().is_empty());
}

#[test]
fn outputs_never_replace_source_databases_or_snapshots() {
    let s = Sandbox::new();
    let db = s.db("one", "source");
    let other = s.db("two", "other");
    let before = Store::open(&db).unwrap().snapshot().unwrap();
    fails(
        s.cli(&[
            "--db",
            string(&db),
            "export",
            "markdown",
            "--output",
            string(&db),
        ]),
        "source database",
    );
    fails(
        s.cli(&[
            "--db",
            string(&db),
            "export",
            "markdown",
            "--output",
            string(&other),
        ]),
        "SQLite database",
    );
    fails(
        s.cli(&[
            "--db",
            string(&db),
            "global",
            "add",
            "source",
            string(&other),
        ]),
        "not a Graf global registry",
    );
    fails(
        s.cli(&[
            "merge",
            "--project",
            &format!("one={}", db.display()),
            "--project",
            &format!("two={}", other.display()),
            "--output",
            string(&db),
        ]),
        "one of its sources",
    );
    let snapshot = s.root.join("input.json");
    s.ok(&[
        "--db",
        string(&db),
        "export",
        "snapshot-json",
        "--output",
        string(&snapshot),
    ]);
    let bytes = fs::read(&snapshot).unwrap();
    fails(
        s.cli(&[
            "export",
            "markdown",
            "--snapshot",
            string(&snapshot),
            "--output",
            string(&snapshot),
        ]),
        "source database or snapshot",
    );
    assert_eq!(fs::read(snapshot).unwrap(), bytes);
    assert_eq!(
        serde_json::to_value(before).unwrap(),
        serde_json::to_value(Store::open(&db).unwrap().snapshot().unwrap()).unwrap()
    );
}

#[cfg(unix)]
#[test]
fn output_permissions_aliases_and_failed_render_preserve_existing_data() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let s = Sandbox::new();
    let db = s.db("one", "source");
    let output = s.root.join("report.md");
    fs::write(&output, "old report").unwrap();
    fs::set_permissions(&output, fs::Permissions::from_mode(0o640)).unwrap();
    s.ok(&["--db", string(&db), "report", "--output", string(&output)]);
    assert_eq!(
        fs::metadata(&output).unwrap().permissions().mode() & 0o777,
        0o640
    );
    let alias = s.root.join("source alias");
    fs::hard_link(&db, &alias).unwrap();
    fails(
        s.cli(&["--db", string(&db), "analyze", "--output", string(&alias)]),
        "source database",
    );
    let link = s.root.join("output symlink");
    symlink(&output, &link).unwrap();
    fails(
        s.cli(&["--db", string(&db), "report", "--output", string(&link)]),
        "not a symlink",
    );
    let broken = s.root.join("invalid.json");
    fs::write(&broken, "{invalid}").unwrap();
    let before = fs::read(&output).unwrap();
    assert!(
        !s.cli(&[
            "report",
            "--snapshot",
            string(&broken),
            "--output",
            string(&output)
        ])
        .status
        .success()
    );
    assert_eq!(fs::read(output).unwrap(), before);
}

#[test]
fn analysis_filters_and_visualization_limits_reach_library_options() {
    let s = Sandbox::new();
    let db = s.db("one", "source");
    let mut data = graph("source");
    let mut noise = data.nodes[0].clone();
    noise.id = "noise".into();
    noise.label = "print".into();
    noise.file.clear();
    noise.kind = "external".into();
    data.nodes.push(noise);
    Store::open(&db).unwrap().refresh_import(data).unwrap();
    let filtered = s.ok(&["--json", "--db", string(&db), "hubs", "--top", "10"]);
    assert!(
        filtered["noise_filtered_hubs"]
            .as_array()
            .unwrap()
            .contains(&json!("noise"))
    );
    assert!(
        !filtered["hubs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["node"]["id"] == "noise")
    );
    let unfiltered = s.ok(&[
        "--json",
        "--db",
        string(&db),
        "hubs",
        "--top",
        "10",
        "--include-noise",
    ]);
    assert!(
        unfiltered["hubs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["node"]["id"] == "noise")
    );
    let analysis = s.ok(&[
        "--json",
        "--db",
        string(&db),
        "analyze",
        "--exclude-hubs",
        "0",
    ]);
    assert_eq!(analysis["excluded_hubs"], json!(["a", "b"]));
    assert!(
        analysis["communities"]
            .as_array()
            .unwrap()
            .iter()
            .all(|c| c["label"].is_string())
    );
    let html = s.ok(&[
        "--json",
        "--db",
        string(&db),
        "export",
        "html",
        "--node-limit",
        "2",
        "--edge-limit",
        "1",
    ]);
    let html = html["content"].as_str().unwrap();
    assert!(html.contains("\"node_limit\":2"));
    assert!(html.contains("\"edge_limit\":1"));
    fails(
        s.cli(&["--db", string(&db), "analyze", "--exclude-hubs", "101"]),
        "between 0 and 100",
    );
    fails(
        s.cli(&["--db", string(&db), "report", "--node-limit", "0"]),
        "invalid value",
    );
}

#[test]
fn merge_and_export_reject_source_sidecars_before_writing() {
    let s = Sandbox::new();
    let one = s.db("one", "one");
    let two = s.db("two", "two");
    let sidecar = PathBuf::from(format!("{}-wal", one.display()));
    fails(
        s.cli(&[
            "--db",
            string(&one),
            "export",
            "markdown",
            "--output",
            string(&sidecar),
        ]),
        "sidecar",
    );
    fails(
        s.cli(&[
            "merge",
            "--project",
            &format!("one={}", one.display()),
            "--project",
            &format!("two={}", two.display()),
            "--output",
            string(&sidecar),
        ]),
        "sidecar",
    );
    assert_eq!(Store::open(&one).unwrap().stats().unwrap().nodes, 3);
}

#[test]
fn multigraph_diagnostics_count_direction_and_parallel_records_without_changes() {
    let s = Sandbox::new();
    let db = s.db("diagnostics", "source");
    let mut data = graph("source");
    let mut reverse = data.edges[0].clone();
    reverse.id = "reverse".into();
    reverse.source = "b".into();
    reverse.target = "a".into();
    reverse.relation = "uses".into();
    data.edges.push(reverse);
    let mut duplicate = data.edges[1].clone();
    duplicate.id = "reverse-undirected".into();
    std::mem::swap(&mut duplicate.source, &mut duplicate.target);
    data.edges.push(duplicate);
    for relation in ["calls", "references"] {
        let mut edge = data.edges[0].clone();
        edge.id = format!("loop-{relation}");
        edge.source = "isolated".into();
        edge.target = "isolated".into();
        edge.relation = relation.into();
        data.edges.push(edge);
    }
    Store::open(&db).unwrap().refresh_import(data).unwrap();
    let before = fs::read(&db).unwrap();
    let result = s.ok(&["--json", "--db", string(&db), "diagnose", "multigraph"]);
    for (field, count) in [
        ("node_count", 3),
        ("edge_count", 7),
        ("directed_edges", 5),
        ("undirected_edges", 2),
        ("self_loop_edges", 2),
        ("parallel_edge_groups", 3),
        ("parallel_extra_edges", 3),
        ("mixed_endpoint_groups", 1),
        ("relation_variant_groups", 2),
        ("duplicate_record_edges", 1),
        ("ordered_unique_endpoint_pairs", 3),
        ("ordered_same_endpoint_collapse_loss", 4),
        ("undirected_unique_endpoint_pairs", 2),
        ("undirected_same_endpoint_collapse_loss", 5),
        ("collapse_risk_groups", 2),
    ] {
        assert_eq!(result[field], count, "{field}");
    }
    assert_eq!(result["examples"][0]["endpoints"], json!(["a", "b"]));
    assert_eq!(result["examples"][0]["edge_count"], 5);
    assert_eq!(result["examples"].as_array().unwrap().len(), 2);
    let one = s.ok(&[
        "--json",
        "--db",
        string(&db),
        "diagnose",
        "--max-examples",
        "1",
    ]);
    assert_eq!(one["examples"].as_array().unwrap().len(), 1);
    assert_eq!(one["examples_truncated"], true);
    let none = s.ok(&[
        "--json",
        "--db",
        string(&db),
        "diagnose",
        "--max-examples",
        "0",
    ]);
    assert_eq!(none["examples"], json!([]));
    fails(
        s.cli(&["--db", string(&db), "diagnose", "--max-examples", "101"]),
        "invalid value",
    );
    let snapshot = s.root.join("saved snapshot.json");
    s.ok(&[
        "--db",
        string(&db),
        "export",
        "snapshot-json",
        "--output",
        string(&snapshot),
    ]);
    let saved = s.ok(&["--json", "diagnose", "--snapshot", string(&snapshot)]);
    assert_eq!(saved, result);
    assert_eq!(fs::read(db).unwrap(), before);
}

fn read_json(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

#[test]
fn labels_reuse_exact_membership_across_order_and_id_changes() {
    let s = Sandbox::new();
    let db = s.db("labels", "source");
    let output = s.root.join("community labels.json");
    let before = fs::read(&db).unwrap();
    let first = s.ok(&[
        "--json",
        "--db",
        string(&db),
        "label",
        "--output",
        string(&output),
    ]);
    assert_eq!(first["generated"], 2);
    assert_eq!(first["reused"], 0);
    let original = read_json(&output);
    let (signature, record) = original["labels"]
        .as_object()
        .unwrap()
        .iter()
        .find(|(_, item)| item["members"] == json!(["a", "b"]))
        .unwrap();
    let original_id = record["community_id"].clone();
    let mut custom = original.clone();
    custom["labels"][signature]["label"] = json!("My reviewed subsystem");
    custom["labels"][signature]["members"] = json!(["b", "a"]);
    custom["labels"][signature]["community_id"] = json!(999);
    fs::write(&output, serde_json::to_vec(&custom).unwrap()).unwrap();
    let second = s.ok(&[
        "--json",
        "--db",
        string(&db),
        "label",
        "--output",
        string(&output),
    ]);
    assert_eq!(second["reused"], 2);
    let saved = read_json(&output);
    assert_eq!(saved["labels"][signature]["label"], "My reviewed subsystem");
    assert_eq!(saved["labels"][signature]["members"], json!(["a", "b"]));
    assert_eq!(saved["labels"][signature]["community_id"], original_id);
    assert_eq!(fs::read(&db).unwrap(), before);

    // An added disconnected node changes numeric community IDs, but not membership.
    let mut data = graph("source");
    let mut node = data.nodes[2].clone();
    node.id = "00".into();
    node.label = "new isolated node".into();
    data.nodes.push(node);
    data.nodes.reverse();
    data.edges.reverse();
    Store::open(&db)
        .unwrap()
        .refresh_import(data.clone())
        .unwrap();
    let next = s.root.join("next labels.json");
    let result = s.ok(&[
        "--json",
        "--db",
        string(&db),
        "label",
        "--input",
        string(&output),
        "--output",
        string(&next),
    ]);
    assert_eq!(result["reused"], 2);
    assert_eq!(result["generated"], 1);
    let saved = read_json(&next);
    assert_eq!(saved["labels"][signature]["label"], "My reviewed subsystem");
    assert_ne!(saved["labels"][signature]["community_id"], original_id);

    // Replacing a member must not carry a custom label to the different community.
    data.nodes.iter_mut().find(|n| n.id == "b").unwrap().id = "c".into();
    for edge in &mut data.edges {
        edge.target = "c".into();
    }
    Store::open(&db).unwrap().refresh_import(data).unwrap();
    let changed = s.ok(&[
        "--json",
        "--db",
        string(&db),
        "label",
        "--output",
        string(&next),
    ]);
    assert_eq!(changed["reused"], 2);
    assert_eq!(changed["generated"], 1);
    let changed = read_json(&next);
    assert!(changed["labels"].get(signature).is_none());
    assert!(
        changed["labels"]
            .as_object()
            .unwrap()
            .values()
            .all(|entry| entry["label"] != "My reviewed subsystem")
    );
}

#[test]
fn label_validation_and_source_protection_leave_files_unchanged() {
    let s = Sandbox::new();
    let db = s.db("labels", "source");
    let output = s.root.join("labels.json");
    s.ok(&["--db", string(&db), "label", "--output", string(&output)]);
    let before = fs::read(&output).unwrap();
    let source = fs::read(&db).unwrap();
    fails(s.cli(&["--db", string(&db), "label"]), "--output");
    fails(
        s.cli(&["--db", string(&db), "label", "--output", string(&db)]),
        "source database",
    );
    let invalid = s.root.join("invalid labels.json");
    let mut labels = read_json(&output);
    labels["labels"]
        .as_object_mut()
        .unwrap()
        .values_mut()
        .next()
        .unwrap()["members"] = json!(["different"]);
    fs::write(&invalid, serde_json::to_vec(&labels).unwrap()).unwrap();
    fails(
        s.cli(&[
            "--db",
            string(&db),
            "label",
            "--input",
            string(&invalid),
            "--output",
            string(&output),
        ]),
        "signature does not match",
    );
    assert_eq!(fs::read(&output).unwrap(), before);
    fs::File::create(&invalid)
        .unwrap()
        .set_len(8 * 1024 * 1024 + 1)
        .unwrap();
    fails(
        s.cli(&[
            "--db",
            string(&db),
            "label",
            "--input",
            string(&invalid),
            "--output",
            string(&output),
        ]),
        "exceeds 8 MiB",
    );
    assert_eq!(fs::read(&output).unwrap(), before);
    assert_eq!(fs::read(&db).unwrap(), source);
    let snapshot = s.root.join("source.json");
    s.ok(&[
        "--db",
        string(&db),
        "export",
        "snapshot-json",
        "--output",
        string(&snapshot),
    ]);
    let before = fs::read(&snapshot).unwrap();
    fails(
        s.cli(&[
            "label",
            "--snapshot",
            string(&snapshot),
            "--output",
            string(&snapshot),
        ]),
        "source database or snapshot",
    );
    assert_eq!(fs::read(snapshot).unwrap(), before);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&output, fs::Permissions::from_mode(0o640)).unwrap();
        s.ok(&["--db", string(&db), "label", "--output", string(&output)]);
        assert_eq!(
            fs::metadata(&output).unwrap().permissions().mode() & 0o777,
            0o640
        );
    }
}

#[test]
fn analysis_aliases_and_tree_share_read_only_workflows() {
    let s = Sandbox::new();
    let db = s.db("aliases", "source");
    let before = fs::read(&db).unwrap();
    let analyze = s.ok(&["--json", "--db", string(&db), "analyze"]);
    assert_eq!(
        s.ok(&["--json", "--db", string(&db), "cluster-only"]),
        analyze
    );
    let hubs = s.ok(&["--json", "--db", string(&db), "hubs"]);
    assert_eq!(s.ok(&["--json", "--db", string(&db), "god-nodes"]), hubs);
    let export = s.ok(&[
        "--json",
        "--db",
        string(&db),
        "export",
        "tree-html",
        "--node-limit",
        "2",
    ]);
    let tree = s.ok(&["--json", "--db", string(&db), "tree", "--node-limit", "2"]);
    assert_eq!(tree, export);
    assert_eq!(fs::read(db).unwrap(), before);
    assert_eq!(fs::read_dir(&s.root).unwrap().count(), 1);
    assert_eq!(fs::read_dir(&s.home).unwrap().count(), 0);
}

#[test]
fn global_unchanged_content_skips_writes_and_reads_live_wal_snapshots() {
    let s = Sandbox::new();
    let source = s.db("source", "before");
    let aggregate = s.root.join("aggregate.db");
    let first = s.ok(&[
        "--json",
        "--db",
        string(&aggregate),
        "global",
        "add",
        "one",
        string(&source),
    ]);
    assert_eq!(first["unchanged"], false);
    let bytes = fs::read(&aggregate).unwrap();
    let modified = fs::metadata(&aggregate).unwrap().modified().unwrap();
    let generation = first["stats"]["generation"].clone();
    for command in [vec!["add", "one", string(&source)], vec!["refresh"]] {
        let mut args = vec!["--json", "--db", string(&aggregate), "global"];
        args.extend(command);
        let result = s.ok(&args);
        assert_eq!(result["unchanged"], true);
        assert_eq!(result["stats"]["generation"], generation);
        assert_eq!(fs::read(&aggregate).unwrap(), bytes);
        assert_eq!(
            fs::metadata(&aggregate).unwrap().modified().unwrap(),
            modified
        );
    }
    let old_fingerprint = Store::open_read_only(&aggregate)
        .unwrap()
        .graph_metadata()
        .unwrap()["graf_registry_fingerprint"]
        .clone();
    assert!(old_fingerprint.as_str().unwrap().starts_with("blake3:"));
    // Keep the writer alive so committed source changes remain visible through WAL.
    let mut writer = Store::open(&source).unwrap();
    writer.refresh_import(graph("after")).unwrap();
    assert!(
        fs::metadata(format!("{}-wal", source.display()))
            .unwrap()
            .len()
            > 0
    );
    let updated = s.ok(&["--json", "--db", string(&aggregate), "global", "refresh"]);
    assert_eq!(updated["unchanged"], false);
    assert_eq!(
        updated["stats"]["generation"].as_u64().unwrap(),
        generation.as_u64().unwrap() + 1
    );
    let store = Store::open_read_only(&aggregate).unwrap();
    assert_ne!(
        store.graph_metadata().unwrap()["graf_registry_fingerprint"],
        old_fingerprint
    );
    assert!(
        store
            .snapshot()
            .unwrap()
            .nodes
            .iter()
            .any(|node| node.label == "after_a")
    );
    drop(store);
    drop(writer);
    fs::remove_file(&source).unwrap();
    let saved = fs::read(&aggregate).unwrap();
    s.ok(&["--json", "--db", string(&aggregate), "global", "list"]);
    s.ok(&[
        "--json",
        "--db",
        string(&aggregate),
        "global",
        "query",
        "after_a",
    ]);
    assert_eq!(fs::read(&aggregate).unwrap(), saved);
    fails(
        s.cli(&["--db", string(&aggregate), "global", "refresh"]),
        "aggregate unchanged",
    );
    assert_eq!(fs::read(aggregate).unwrap(), saved);
}

#[test]
fn global_fingerprint_ignores_snapshot_record_and_object_key_order() {
    let s = Sandbox::new();
    let db = s.db("source", "source");
    let snapshot = s.root.join("source.json");
    let aggregate = s.root.join("aggregate.db");
    s.ok(&[
        "--db",
        string(&db),
        "export",
        "snapshot-json",
        "--output",
        string(&snapshot),
    ]);
    s.ok(&[
        "--db",
        string(&aggregate),
        "global",
        "add",
        "one",
        string(&snapshot),
        "--snapshot",
    ]);
    let bytes = fs::read(&aggregate).unwrap();
    let mut graph = read_json(&snapshot);
    graph["nodes"].as_array_mut().unwrap().reverse();
    graph["edges"].as_array_mut().unwrap().reverse();
    // Emit top-level fields in reverse order, independently of serializer map order.
    let entries: Vec<_> = graph
        .as_object()
        .unwrap()
        .iter()
        .rev()
        .map(|(k, v)| format!("{}:{}", serde_json::to_string(k).unwrap(), v))
        .collect();
    fs::write(&snapshot, format!("{{{}}}", entries.join(","))).unwrap();
    let result = s.ok(&["--json", "--db", string(&aggregate), "global", "refresh"]);
    assert_eq!(result["unchanged"], true);
    assert_eq!(fs::read(aggregate).unwrap(), bytes);
}

fn packages(version: &str, other_ecosystem: &str) -> ImportedGraph {
    let mut nodes = Vec::new();
    for (id, kind, key) in [
        ("shared", "package", Some("package:npm:@demo/shared")),
        ("empty-name", "package", Some("package:npm:")),
        ("empty-ecosystem", "package", Some("package::shared")),
        ("function", "function", Some("package:npm:@demo/shared")),
        ("basename", "package", None),
        ("other", "package", Some(other_ecosystem)),
    ] {
        let mut node = graph("package").nodes.remove(0);
        node.id = id.into();
        node.kind = kind.into();
        node.label = "same basename".into();
        node.binding_key = key.map(str::to_owned);
        node.metadata = json!({"version":version});
        nodes.push(node);
    }
    ImportedGraph {
        nodes,
        edges: vec![],
        metadata: json!({}),
    }
}

#[test]
fn global_package_links_keep_versions_and_ids_distinct_merge_is_opt_in() {
    let s = Sandbox::new();
    let one = s.db("one", "one");
    let two = s.db("two", "two");
    Store::open(&one)
        .unwrap()
        .refresh_import(packages("1.0", "package:python:common"))
        .unwrap();
    Store::open(&two)
        .unwrap()
        .refresh_import(packages("2.0", "package:npm:common"))
        .unwrap();
    let aggregate = s.root.join("aggregate.db");
    s.ok(&[
        "--db",
        string(&aggregate),
        "global",
        "add",
        "one",
        string(&one),
    ]);
    let added = s.ok(&[
        "--json",
        "--db",
        string(&aggregate),
        "global",
        "add",
        "two",
        string(&two),
    ]);
    assert_eq!(added["package_links"], 1);
    let graph = Store::open_read_only(&aggregate)
        .unwrap()
        .snapshot()
        .unwrap();
    assert_eq!(graph.nodes.len(), 12);
    assert_eq!(graph.edges.len(), 1);
    let edge = &graph.edges[0];
    assert_eq!(edge.relation, "same_package");
    assert!(!edge.directed);
    assert_ne!(edge.source, edge.target);
    assert_eq!(edge.metadata["package_key"], "package:npm:@demo/shared");
    assert_eq!(edge.metadata["sources"][0]["project"], "one");
    assert_eq!(edge.metadata["sources"][1]["project"], "two");
    assert_eq!(edge.metadata["sources"][0]["version"], "1.0");
    assert_eq!(edge.metadata["sources"][1]["version"], "2.0");
    for node in graph
        .nodes
        .iter()
        .filter(|n| n.metadata["original_id"] == "shared")
    {
        assert_eq!(
            node.binding_key.as_deref(),
            Some("package:npm:@demo/shared")
        );
        assert_eq!(
            node.metadata["original_metadata"]["version"],
            if node.metadata["project"] == "one" {
                "1.0"
            } else {
                "2.0"
            }
        );
    }
    let query = s.ok(&[
        "--json",
        "--db",
        string(&aggregate),
        "global",
        "query",
        &edge.source,
        "--relation",
        "same_package",
    ]);
    assert_eq!(query["nodes"].as_array().unwrap().len(), 2);
    assert_eq!(query["edges"].as_array().unwrap().len(), 1);
    for link in [false, true] {
        let output = s.root.join(format!("merge-{link}.db"));
        let project_one = format!("one={}", one.display());
        let project_two = format!("two={}", two.display());
        let mut args = vec![
            "merge",
            "--project",
            &project_one,
            "--project",
            &project_two,
            "--output",
            string(&output),
        ];
        if link {
            args.push("--link-packages");
        }
        s.ok(&args);
        let graph = Store::open_read_only(&output).unwrap().snapshot().unwrap();
        assert_eq!(graph.nodes.len(), 12);
        assert_eq!(graph.edges.len(), usize::from(link));
    }
}

#[test]
fn benchmark_measures_bounded_real_results_without_opening_native_sources() {
    let s = Sandbox::new();
    let source = s.root.join("native source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("module.py"), "def known_symbol():\n    pass\n").unwrap();
    let db = s.root.join("native.db");
    s.ok(&[
        "--json",
        "--db",
        string(&db),
        "index",
        string(&source),
        "--code-only",
    ]);
    fs::remove_dir_all(&source).unwrap();
    let before = fs::read(&db).unwrap();
    let modified = fs::metadata(&db).unwrap().modified().unwrap();
    let measured = s.ok(&[
        "--json",
        "--db",
        string(&db),
        "benchmark",
        "--query",
        "known_symbol",
        "--query",
        "does_not_exist",
        "--iterations",
        "3",
        "--depth",
        "0",
    ]);
    assert_eq!(measured["measured_calls"], 6);
    assert_eq!(measured["warmup_calls_per_query"], 1);
    assert_eq!(measured["queries"][0]["result"]["nodes"], 1);
    assert_eq!(measured["queries"][0]["measured_totals"]["nodes"], 3);
    assert_eq!(measured["queries"][1]["result"]["nodes"], 0);
    assert_eq!(measured["queries"][1]["measured_totals"]["edges"], 0);
    for query in measured["queries"].as_array().unwrap() {
        let median = query["median_ms"].as_f64().unwrap();
        let p95 = query["p95_ms"].as_f64().unwrap();
        assert!(median.is_finite() && median >= 0.0 && p95 >= median);
        assert!(query["max_ms"].as_f64().unwrap() >= p95);
    }
    fails(s.cli(&["--db", string(&db), "benchmark"]), "--query");
    fails(
        s.cli(&[
            "--db",
            string(&db),
            "benchmark",
            "--query",
            "known",
            "--iterations",
            "1001",
        ]),
        "invalid value",
    );
    fails(
        s.cli(&["--db", string(&db), "benchmark", "--query", " "]),
        "nonempty",
    );
    assert_eq!(fs::read(&db).unwrap(), before);
    assert_eq!(fs::metadata(&db).unwrap().modified().unwrap(), modified);
}

#[test]
fn reports_show_stored_coverage_and_scan_freshness_only_when_requested() {
    let s = Sandbox::new();
    let source = s.root.join("corpus");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("module.py"), "def old_symbol():\n    pass\n").unwrap();
    fs::write(source.join("opaque.unknown"), "opaque bytes").unwrap();
    let db = s.root.join("native.db");
    s.ok(&[
        "--json",
        "--db",
        string(&db),
        "index",
        string(&source),
        "--code-only",
    ]);
    let before = fs::read(&db).unwrap();
    let stored = s.ok(&["--json", "--db", string(&db), "report"]);
    assert_eq!(stored["source_context"]["indexed_files"], 1);
    assert_eq!(stored["source_context"]["coverage"]["supported_files"], 1);
    assert_eq!(stored["source_context"]["coverage"]["unsupported_files"], 1);
    assert_eq!(
        stored["source_context"]["freshness"]["status"],
        "not_checked"
    );
    let fresh = s.ok(&["--json", "--db", string(&db), "report", "--check-freshness"]);
    assert_eq!(fresh["source_context"]["freshness"]["status"], "fresh");
    fs::write(source.join("module.py"), "def new_symbol():\n    pass\n").unwrap();
    let stale = s.ok(&[
        "--json",
        "--db",
        string(&db),
        "report",
        "--check-freshness",
        "--format",
        "html",
    ]);
    assert_eq!(stale["source_context"]["freshness"]["status"], "stale");
    assert_eq!(
        stale["source_context"]["freshness"]["details"]["changed"],
        json!(["module.py"])
    );
    assert!(
        stale["content"]
            .as_str()
            .unwrap()
            .contains("Source coverage and freshness")
    );
    let exported = s.ok(&["--json", "--db", string(&db), "export", "markdown"]);
    assert!(exported.get("source_context").is_none());
    fs::remove_dir_all(source).unwrap();
    let stored_again = s.ok(&["--json", "--db", string(&db), "report"]);
    assert_eq!(stored_again, stored);
    let unavailable = s.ok(&["--json", "--db", string(&db), "report", "--check-freshness"]);
    assert_eq!(
        unavailable["source_context"]["freshness"]["status"],
        "unavailable"
    );
    assert_eq!(fs::read(&db).unwrap(), before);
    let imported = s.db("imported", "source");
    fails(
        s.cli(&["--db", string(&imported), "report", "--check-freshness"]),
        "requires a native database",
    );
}

#[test]
fn display_labels_and_community_granularity_are_explicit_and_source_preserving() {
    let s = Sandbox::new();
    let db = s.db("labels", "source");
    let labels = s.root.join("labels.json");
    s.ok(&["--db", string(&db), "label", "--output", string(&labels)]);
    let mut file = read_json(&labels);
    let entry = file["labels"]
        .as_object_mut()
        .unwrap()
        .values_mut()
        .find(|c| c["members"] == json!(["a", "b"]))
        .unwrap();
    entry["label"] = json!("Reviewed component");
    fs::write(&labels, serde_json::to_vec(&file).unwrap()).unwrap();
    let before = fs::read(&db).unwrap();
    let report = s.ok(&[
        "--json",
        "--db",
        string(&db),
        "report",
        "--labels",
        string(&labels),
    ]);
    assert!(
        report["content"]
            .as_str()
            .unwrap()
            .contains("Reviewed component")
    );
    let unchanged = s.ok(&["--json", "--db", string(&db), "report"]);
    assert!(
        !unchanged["content"]
            .as_str()
            .unwrap()
            .contains("Reviewed component")
    );
    let html = s.ok(&[
        "--json",
        "--db",
        string(&db),
        "export",
        "html",
        "--labels",
        string(&labels),
    ]);
    assert!(
        html["content"]
            .as_str()
            .unwrap()
            .contains("Reviewed component")
    );
    let split = s.ok(&[
        "--json",
        "--db",
        string(&db),
        "analyze",
        "--resolution",
        "100",
        "--max-community-size",
        "1",
        "--min-cohesion",
        "1",
    ]);
    assert_eq!(split["communities"].as_array().unwrap().len(), 3);
    assert!(split["community_split_attempts"].is_number());
    assert!(split["unsatisfied_community_constraints"].is_array());
    let stale = s.ok(&[
        "--json",
        "--db",
        string(&db),
        "report",
        "--resolution",
        "100",
        "--labels",
        string(&labels),
    ]);
    assert!(
        !stale["content"]
            .as_str()
            .unwrap()
            .contains("Reviewed component")
    );
    for (flag, value, error) in [
        ("--resolution", "0", "positive"),
        ("--resolution", "NaN", "positive"),
        ("--min-cohesion", "1.1", "between 0 and 1"),
        ("--max-community-size", "0", "invalid value"),
    ] {
        fails(s.cli(&["--db", string(&db), "analyze", flag, value]), error);
    }
    assert_eq!(fs::read(db).unwrap(), before);
}
