use std::{
    collections::BTreeMap,
    fs,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    sync::mpsc,
    time::Duration,
};

use serde_json::{Value, json};
use tempfile::{TempDir, tempdir};

fn write(path: &Path, bytes: impl AsRef<[u8]>) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}

fn snapshot() -> Value {
    json!({
        "directed": true, "multigraph": false,
        "nodes": [{"id":"entry", "label":"Entrée"}, {"id":"leaf", "label":"Leaf"}],
        "links": [{"source":"entry", "target":"leaf", "relation":"calls"}]
    })
}

fn project() -> TempDir {
    let dir = tempdir().unwrap();
    write(
        &dir.path().join("graphify-out/graph.json"),
        snapshot().to_string(),
    );
    dir
}

fn graphify(graph: &str) -> Value {
    // A nonexistent Python path also proves migration does not launch the old server.
    json!({"command":"/nonexistent/graf-test/python", "args":["-m", "graphify.serve", graph]})
}

fn config() -> Value {
    json!({
        "mcpServers": {
            "graphify": graphify("graphify-out/graph.json"),
            "other": {"command":"unrelated-tool", "args":["keep me"], "env":{"TEST_VALUE":"synthetic"}}
        },
        "hooks": {"generate":"graphify build"},
        "preferences": {"language":"日本語"}
    })
}

fn cli(cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_graf"))
        .current_dir(cwd)
        .args(args)
        .output()
        .unwrap()
}

fn success(output: Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("one JSON report on stdout")
}

fn failure(output: Output) {
    assert!(
        !output.status.success(),
        "unexpected success: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        output.stdout.is_empty(),
        "failed migration must not print a success report"
    );
    assert!(
        !output.stderr.is_empty(),
        "failure must explain the problem"
    );
}

fn read_json(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

fn files(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn visit(root: &Path, dir: &Path, result: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                visit(root, &entry.path(), result);
            } else {
                result.insert(
                    entry.path().strip_prefix(root).unwrap().to_owned(),
                    fs::read(entry.path()).unwrap(),
                );
            }
        }
    }
    let mut result = BTreeMap::new();
    visit(root, root, &mut result);
    result
}

fn fails_without_changes(root: &Path, args: &[&str]) {
    let before = files(root);
    failure(cli(root, args));
    assert_eq!(files(root), before, "rejected input changed project files");
    assert!(
        !root.join(".graf").exists(),
        "preflight must not create migration state"
    );
}

fn assert_report(report: &Value, root: &Path, config_path: &Path, status: &str) {
    assert_eq!(report["status"], status);
    assert_eq!(
        Path::new(report["database"].as_str().unwrap()),
        root.canonicalize().unwrap().join(".graf/index.db")
    );
    assert_eq!(
        Path::new(report["config"].as_str().unwrap()),
        config_path.canonicalize().unwrap()
    );
    assert_eq!(report["nodes"], 2);
    assert_eq!(report["edges"], 1);
}

#[test]
fn migration_repeat_undo_and_redo_preserve_config_and_imported_snapshot() {
    let dir = project();
    let root = dir.path();
    let path = root.join(".mcp.json");
    let original = format!("{}\n\n", serde_json::to_string_pretty(&config()).unwrap());
    write(&path, &original);
    write(
        &root.join(".claude/skills/graphify/SKILL.md"),
        "Generate the graph on request.\n",
    );
    let source = fs::read(root.join("graphify-out/graph.json")).unwrap();
    let report = success(cli(root, &["switch", "graphify", "--json"]));
    assert_report(&report, root, &path, "switched");
    let migrated = fs::read(&path).unwrap();
    let value = read_json(&path);
    assert!(value["mcpServers"].get("graphify").is_none());
    assert_eq!(
        value["mcpServers"]["other"],
        config()["mcpServers"]["other"]
    );
    assert_eq!(value["hooks"], config()["hooks"]);
    assert_eq!(value["preferences"], config()["preferences"]);
    assert_eq!(
        fs::read(root.join("graphify-out/graph.json")).unwrap(),
        source
    );
    assert_eq!(
        fs::read_to_string(root.join(".claude/skills/graphify/SKILL.md")).unwrap(),
        "Generate the graph on request.\n"
    );
    let db = root.join(".graf/index.db");
    let imported = fs::read(&db).unwrap();
    let callees = success(cli(root, &["callees", "entry", "--json"]));
    assert_eq!(callees["nodes"].as_array().unwrap().len(), 2);
    assert_eq!(callees["edges"][0]["relation"], "calls");

    // Repeating and redoing must use the saved snapshot, even after regeneration.
    write(
        &root.join("graphify-out/graph.json"),
        json!({"directed":true,"multigraph":false,"nodes":[],"links":[]}).to_string(),
    );
    let repeated = success(cli(root, &["switch", "graphify", "--json"]));
    assert_report(&repeated, root, &path, "already_switched");
    assert_eq!(fs::read(&path).unwrap(), migrated);
    assert_eq!(fs::read(&db).unwrap(), imported);
    for _ in 0..2 {
        let undone = success(cli(root, &["switch", "--undo", "--json"]));
        assert_report(&undone, root, &path, "undone");
        assert_eq!(fs::read(&path).unwrap(), original.as_bytes());
        assert_eq!(fs::read(&db).unwrap(), imported);
    }
    let redone = success(cli(root, &["switch", "graphify", "--json"]));
    assert_report(&redone, root, &path, "switched");
    assert_eq!(fs::read(&path).unwrap(), migrated);
    assert_eq!(fs::read(&db).unwrap(), imported);
    assert_eq!(success(cli(root, &["stats", "--json"]))["nodes"], 2);
}

#[test]
fn no_config_creates_graf_and_undo_removes_only_the_created_config() {
    let dir = project();
    let root = dir.path();
    let report = success(cli(root, &["switch", "graphify", "--json"]));
    assert_report(&report, root, &root.join(".mcp.json"), "switched");
    let config = read_json(&root.join(".mcp.json"));
    assert!(config["mcpServers"]["graf"].is_object());
    let db = fs::read(root.join(".graf/index.db")).unwrap();
    let undone = success(cli(root, &["switch", "--undo", "--json"]));
    assert_eq!(undone["status"], "undone");
    assert!(!root.join(".mcp.json").exists());
    assert_eq!(fs::read(root.join(".graf/index.db")).unwrap(), db);
    success(cli(root, &["switch", "graphify", "--json"]));
    assert_eq!(read_json(&root.join(".mcp.json")), config);
}

#[test]
fn autodetects_cursor_and_vscode_and_accepts_both_graph_argument_forms() {
    for (relative, key, args) in [
        (
            ".cursor/mcp.json",
            "mcpServers",
            json!(["-m", "graphify.serve"]),
        ),
        (
            ".vscode/mcp.json",
            "servers",
            json!(["-m", "graphify.serve", "--graph", "graphify-out/graph.json"]),
        ),
    ] {
        let dir = project();
        let path = dir.path().join(relative);
        let mut entry = graphify("graphify-out/graph.json");
        entry["args"] = args;
        let original =
            json!({key:{"graphify":entry,"other":{"command":"keep"}},"inputs":[{"id":"keep"}]});
        write(&path, original.to_string());
        let report = success(cli(dir.path(), &["switch", "graphify", "--json"]));
        assert_report(&report, dir.path(), &path, "switched");
        let migrated = read_json(&path);
        assert!(migrated[key].get("graphify").is_none());
        assert!(migrated[key]["graf"].is_object());
        assert_eq!(migrated[key]["other"], original[key]["other"]);
        assert_eq!(migrated["inputs"], original["inputs"]);
        if key == "servers" {
            assert_eq!(migrated[key]["graf"]["type"], "stdio");
        }
        assert!(!dir.path().join(".mcp.json").exists());
        success(cli(dir.path(), &["switch", "--undo", "--json"]));
        assert_eq!(fs::read(&path).unwrap(), original.to_string().as_bytes());
    }
}

#[test]
fn explicit_global_json_changes_only_selected_project_connection() {
    let dir = project();
    let settings = tempdir().unwrap();
    let path = settings.path().join("global.json");
    let mut original = config();
    original["mcpServers"]["graphify"] =
        graphify(dir.path().join("graphify-out/graph.json").to_str().unwrap());
    write(&path, original.to_string());
    let report = success(cli(
        dir.path(),
        &[
            "switch",
            "graphify",
            "--config",
            path.to_str().unwrap(),
            "--json",
        ],
    ));
    assert_report(&report, dir.path(), &path, "switched");
    assert_eq!(
        read_json(&path)["mcpServers"]["other"],
        original["mcpServers"]["other"]
    );
    assert!(!dir.path().join(".mcp.json").exists());
    success(cli(
        dir.path(),
        &[
            "switch",
            "--undo",
            "--config",
            path.to_str().unwrap(),
            "--json",
        ],
    ));
    assert_eq!(fs::read(&path).unwrap(), original.to_string().as_bytes());
}

#[test]
fn explicit_codex_toml_preserves_other_settings_and_restores_exact_bytes() {
    let dir = project();
    let path = dir.path().join("config.toml");
    let original = "# Personal settings\nmodel = 'example-model'\n\n[mcp_servers.graphify]\ncommand = 'python'\nargs = ['-m', 'graphify.serve', 'graphify-out/graph.json']\n\n# Keep this server\n[mcp_servers.other]\ncommand = 'keep'\nargs = ['one two']\n";
    write(&path, original);
    let report = success(cli(
        dir.path(),
        &["switch", "graphify", "--config", "config.toml", "--json"],
    ));
    assert_report(&report, dir.path(), &path, "switched");
    let text = fs::read_to_string(&path).unwrap();
    let doc: toml_edit::DocumentMut = text.parse().unwrap();
    assert_eq!(doc["model"].as_str(), Some("example-model"));
    assert_eq!(
        doc["mcp_servers"]["other"]["command"].as_str(),
        Some("keep")
    );
    assert_eq!(
        doc["mcp_servers"]["other"]["args"][0].as_str(),
        Some("one two")
    );
    assert!(doc["mcp_servers"].get("graphify").is_none());
    assert!(Path::new(doc["mcp_servers"]["graf"]["command"].as_str().unwrap()).is_absolute());
    assert!(text.contains("# Keep this server"));
    success(cli(dir.path(), &["switch", "--undo", "--json"]));
    assert_eq!(fs::read(&path).unwrap(), original.as_bytes());
}

#[test]
fn nested_discovery_and_explicit_project_handle_spaces_and_unicode() {
    let dir = tempdir().unwrap();
    let root = dir.path().join("projet café 東京");
    write(
        &root.join("graphify-out/graph.json"),
        snapshot().to_string(),
    );
    let nested = root.join("src/deep");
    fs::create_dir_all(&nested).unwrap();
    let report = success(cli(&nested, &["switch", "graphify", "--json"]));
    assert_report(&report, &root, &root.join(".mcp.json"), "switched");
    assert!(!nested.join(".graf").exists());
    success(cli(&nested, &["switch", "--undo", "--json"]));
    assert!(!root.join(".mcp.json").exists());

    let other = dir.path().join("other project");
    write(
        &other.join("données/graphe 東京.json"),
        snapshot().to_string(),
    );
    write(
        &other.join("settings.json"),
        json!({"mcpServers":{"graphify":graphify("données/graphe 東京.json")}}).to_string(),
    );
    let report = success(cli(
        &nested,
        &[
            "switch",
            "graphify",
            "--project",
            other.to_str().unwrap(),
            "--config",
            "settings.json",
            "--graph",
            "données/graphe 東京.json",
            "--json",
        ],
    ));
    assert_report(&report, &other, &other.join("settings.json"), "switched");
    assert!(!root.join(".mcp.json").exists());
}

#[test]
fn edges_alias_and_parallel_edges_survive_migration() {
    let dir = project();
    let mut graph = snapshot();
    graph["multigraph"] = json!(true);
    graph.as_object_mut().unwrap().remove("links");
    graph["edges"] = json!([
        {"source":"entry","target":"leaf","key":1,"relation":"calls"},
        {"source":"entry","target":"leaf","key":2,"relation":"imports"}
    ]);
    write(
        &dir.path().join("graphify-out/graph.json"),
        graph.to_string(),
    );
    let report = success(cli(dir.path(), &["switch", "graphify", "--json"]));
    assert_eq!(report["nodes"], 2);
    assert_eq!(report["edges"], 2);
    let shown = success(cli(dir.path(), &["show", "entry", "--json"]));
    let mut relations: Vec<_> = shown["edges"]
        .as_array()
        .unwrap()
        .iter()
        .map(|edge| edge["relation"].as_str().unwrap())
        .collect();
    relations.sort_unstable();
    assert_eq!(relations, ["calls", "imports"]);
}

#[test]
fn malformed_graphs_fail_before_config_or_database_changes() {
    let mut bad_direction = snapshot();
    bad_direction["directed"] = json!("true");
    let mut bad_multigraph = snapshot();
    bad_multigraph["multigraph"] = json!(0);
    let mut missing_endpoint = snapshot();
    missing_endpoint["links"][0]["target"] = json!("missing");
    let mut ambiguous_edges = snapshot();
    ambiguous_edges["edges"] = json!([]);
    for invalid in [
        "{".into(),
        "{}".into(),
        bad_direction.to_string(),
        bad_multigraph.to_string(),
        missing_endpoint.to_string(),
        ambiguous_edges.to_string(),
    ] {
        let dir = project();
        write(&dir.path().join(".mcp.json"), config().to_string());
        write(&dir.path().join("graphify-out/graph.json"), invalid);
        fails_without_changes(dir.path(), &["switch", "graphify", "--json"]);
    }
}

#[test]
fn malformed_and_duplicate_config_keys_fail_before_import() {
    for invalid in [
        "{",
        r#"{"mcpServers":[]}"#,
        r#"{"mcpServers":{},"servers":{}}"#,
        r#"{"mcpServers":{},"mcpServers":{}}"#,
        r#"{"mcpServers":{"graphify":{"command":"python","command":"python3","args":["-m","graphify.serve"]}}}"#,
        r#"{"mcpServers":{"graphify":{"command":"python","args":["-m","graphify.serve"]},"graphify":{"command":"python","args":["-m","graphify.serve"]}}}"#,
    ] {
        let dir = project();
        write(&dir.path().join(".mcp.json"), invalid);
        fails_without_changes(dir.path(), &["switch", "graphify", "--json"]);
    }
}

#[test]
fn missing_graph_and_explicit_graph_mismatch_leave_no_migration_state() {
    let dir = tempdir().unwrap();
    fails_without_changes(dir.path(), &["switch", "graphify", "--json"]);
    let dir = project();
    write(&dir.path().join(".mcp.json"), config().to_string());
    write(&dir.path().join("different.json"), snapshot().to_string());
    fails_without_changes(
        dir.path(),
        &["switch", "graphify", "--graph", "different.json", "--json"],
    );
}

#[test]
fn ambiguous_servers_require_selection_and_preserve_unselected_server() {
    let dir = project();
    let mut original = config();
    original["mcpServers"]["second"] = graphify("graphify-out/graph.json");
    write(&dir.path().join(".mcp.json"), original.to_string());
    fails_without_changes(dir.path(), &["switch", "graphify", "--json"]);
    success(cli(
        dir.path(),
        &["switch", "graphify", "--server", "second", "--json"],
    ));
    let changed = read_json(&dir.path().join(".mcp.json"));
    assert_eq!(
        changed["mcpServers"]["graphify"],
        original["mcpServers"]["graphify"]
    );
    assert!(changed["mcpServers"].get("second").is_none());
    assert!(changed["mcpServers"]["graf"].is_object());
}

#[test]
fn ambiguous_config_files_require_explicit_config() {
    let dir = project();
    for relative in [".mcp.json", ".cursor/mcp.json"] {
        write(&dir.path().join(relative), config().to_string());
    }
    fails_without_changes(dir.path(), &["switch", "graphify", "--json"]);
    success(cli(
        dir.path(),
        &[
            "switch",
            "graphify",
            "--config",
            ".cursor/mcp.json",
            "--json",
        ],
    ));
    assert_eq!(
        fs::read(dir.path().join(".mcp.json")).unwrap(),
        config().to_string().as_bytes()
    );
    assert!(read_json(&dir.path().join(".cursor/mcp.json"))["mcpServers"]["graf"].is_object());
}

#[test]
fn existing_database_is_never_overwritten() {
    let dir = project();
    write(&dir.path().join(".mcp.json"), config().to_string());
    write(
        &dir.path().join(".graf/index.db"),
        b"existing database sentinel",
    );
    let before = files(dir.path());
    failure(cli(dir.path(), &["switch", "graphify", "--json"]));
    assert_eq!(files(dir.path()), before);
}

#[test]
fn even_whitespace_edits_block_undo_and_repeat_without_losing_data() {
    let dir = project();
    success(cli(dir.path(), &["switch", "graphify", "--json"]));
    let path = dir.path().join(".mcp.json");
    let mut edited = fs::read(&path).unwrap();
    edited.push(b'\n');
    write(&path, &edited);
    let before = files(dir.path());
    for args in [
        ["switch", "--undo", "--json"],
        ["switch", "graphify", "--json"],
    ] {
        failure(cli(dir.path(), &args));
        assert_eq!(files(dir.path()), before);
    }
}

#[test]
fn http_connections_are_not_selected_or_modified() {
    let dir = project();
    let mut original = config();
    original["mcpServers"]["remote"] = json!({"type":"http","url":"http://127.0.0.1:1/mcp","command":"python","args":["-m","graphify.serve"]});
    write(&dir.path().join(".mcp.json"), original.to_string());
    fails_without_changes(
        dir.path(),
        &["switch", "graphify", "--server", "remote", "--json"],
    );
    success(cli(dir.path(), &["switch", "graphify", "--json"]));
    assert_eq!(
        read_json(&dir.path().join(".mcp.json"))["mcpServers"]["remote"],
        original["mcpServers"]["remote"]
    );
}

#[cfg(unix)]
#[test]
fn symlink_config_database_and_migration_directory_are_refused() {
    use std::os::unix::fs::symlink;
    for target in [".mcp.json", ".graf/index.db", ".graf"] {
        let dir = project();
        let external = tempdir().unwrap();
        let real = external.path().join("target");
        if target == ".graf" {
            fs::create_dir(&real).unwrap();
        } else {
            write(
                &real,
                if target == ".mcp.json" {
                    config().to_string()
                } else {
                    "database sentinel".into()
                },
            );
        }
        let link = dir.path().join(target);
        fs::create_dir_all(link.parent().unwrap()).unwrap();
        symlink(&real, &link).unwrap();
        let external_before = files(external.path());
        let graph = fs::read(dir.path().join("graphify-out/graph.json")).unwrap();
        failure(cli(dir.path(), &["switch", "graphify", "--json"]));
        assert_eq!(files(external.path()), external_before);
        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            fs::read(dir.path().join("graphify-out/graph.json")).unwrap(),
            graph
        );
        if target != ".mcp.json" {
            assert!(!dir.path().join(".mcp.json").exists());
        }
    }
}

struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn saved_mcp_command_serves_imported_graph_from_an_unrelated_directory() {
    let dir = project();
    success(cli(dir.path(), &["switch", "graphify", "--json"]));
    let config = read_json(&dir.path().join(".mcp.json"));
    let entry = &config["mcpServers"]["graf"];
    let command = entry["command"].as_str().unwrap();
    assert!(Path::new(command).is_absolute());
    let args: Vec<_> = entry["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|arg| arg.as_str().unwrap())
        .collect();
    let elsewhere = tempdir().unwrap();
    let db_before = fs::read(dir.path().join(".graf/index.db")).unwrap();
    let mut server = Server(
        Command::new(command)
            .args(args)
            .current_dir(elsewhere.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
    );
    let mut input = server.0.stdin.take().unwrap();
    let output = server.0.stdout.take().unwrap();
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(output).lines() {
            if sender.send(line.unwrap()).is_err() {
                break;
            }
        }
    });
    let mut send = |value: Value| {
        writeln!(input, "{value}").unwrap();
        input.flush().unwrap();
    };
    let receive = |id: u64| -> Value {
        let line = receiver
            .recv_timeout(Duration::from_secs(10))
            .expect("configured MCP server did not reply");
        let value: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(value["id"], id);
        assert!(value.get("error").is_none(), "{value}");
        value
    };
    send(
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"migration-test","version":"1"}}}),
    );
    assert_eq!(receive(1)["result"]["serverInfo"]["name"], "graf");
    send(json!({"jsonrpc":"2.0","method":"notifications/initialized"}));
    send(
        json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"stats","arguments":{}}}),
    );
    let stats = receive(2);
    assert_ne!(stats["result"]["isError"], true);
    assert_eq!(stats["result"]["structuredContent"]["nodes"], 2);
    assert_eq!(stats["result"]["structuredContent"]["edges"], 1);
    send(
        json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"callees","arguments":{"symbol":"entry"}}}),
    );
    let response = receive(3);
    assert_ne!(response["result"]["isError"], true);
    let graph = &response["result"]["structuredContent"];
    let mut ids: Vec<_> = graph["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|node| node["id"].as_str().unwrap())
        .collect();
    ids.sort_unstable();
    assert_eq!(ids, ["entry", "leaf"]);
    assert_eq!(graph["edges"][0]["relation"], "calls");
    drop(server);
    assert_eq!(
        fs::read(dir.path().join(".graf/index.db")).unwrap(),
        db_before
    );
    assert!(files(elsewhere.path()).is_empty());
}
