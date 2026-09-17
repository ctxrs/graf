use std::{
    fs,
    io::{BufRead, BufReader, Write},
    path::Path,
    process::{Child, ChildStdin, Command, Output, Stdio},
    sync::mpsc::{self, Receiver},
    time::Duration,
};

use serde_json::{Value, json};
use tempfile::{TempDir, tempdir};

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
    serde_json::from_slice(&output.stdout).expect("stdout must contain exactly one JSON value")
}

fn failure(output: Output) -> String {
    assert!(!output.status.success());
    assert!(
        output.stdout.is_empty(),
        "failure must not write prose to stdout"
    );
    String::from_utf8(output.stderr).unwrap()
}

fn imported() -> TempDir {
    let dir = tempdir().unwrap();
    fs::write(
        dir.path().join("graph.json"),
        json!({
            "directed": true, "multigraph": false,
            "nodes": [
                {"id":"a", "label":"entry"},
                {"id":"b", "label":"middle"},
                {"id":"c", "label":"leaf"},
                {"id":"x", "label":"member"},
                {"id":"d1", "label":"duplicate"},
                {"id":"d2", "label":"duplicate"}
            ],
            "links": [
                {"source":"a", "target":"b", "relation":"calls"},
                {"source":"b", "target":"c", "relation":"calls"},
                {"source":"c", "target":"x", "relation":"contains"}
            ]
        })
        .to_string(),
    )
    .unwrap();
    let stats = success(cli(
        dir.path(),
        &["import", "graphify", "graph.json", "--json"],
    ));
    assert_eq!(stats["kind"], "imported");
    dir
}

fn ids(graph: &Value) -> Vec<&str> {
    let mut ids: Vec<_> = graph["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["id"].as_str().unwrap())
        .collect();
    ids.sort_unstable();
    ids
}

#[test]
fn imported_cli_routes_direction_relations_and_preserves_snapshot() {
    let dir = imported();
    let db = dir.path().join(".graf/index.db");
    let before = fs::read(&db).unwrap();
    let callers = success(cli(dir.path(), &["--json", "callers", "b"]));
    assert_eq!(ids(&callers), ["a", "b"]);
    let callees = success(cli(dir.path(), &["callees", "b", "--json"]));
    assert_eq!(ids(&callees), ["b", "c"]);
    let impact = success(cli(dir.path(), &["impact", "c", "--json"]));
    assert_eq!(ids(&impact["graph"]), ["a", "b", "c", "x"]);
    assert_eq!(impact["seeds"], json!(["c", "x"]));
    let show = success(cli(dir.path(), &["show", "c", "--json"]));
    assert_eq!(ids(&show), ["b", "c", "x"]);
    let query = success(cli(
        dir.path(),
        &[
            "query",
            "middle",
            "--direction",
            "in",
            "--relation",
            "calls",
            "--json",
        ],
    ));
    assert_eq!(ids(&query), ["a", "b"]);
    let path = success(cli(dir.path(), &["path", "a", "c", "--json"]));
    assert_eq!(path["found"], true);
    assert_eq!(path["graph"]["schema_version"], 1);
    let reverse = success(cli(dir.path(), &["path", "c", "a", "--json"]));
    assert_eq!(reverse["found"], false);
    let incoming = success(cli(
        dir.path(),
        &["path", "c", "a", "--direction", "in", "--json"],
    ));
    assert_eq!(incoming["found"], true);
    let bounded = success(cli(dir.path(), &["show", "b", "--limit", "1", "--json"]));
    assert_eq!(bounded["truncated"], true);
    assert!(bounded["nodes"].as_array().unwrap().len() <= 1);
    let error = failure(cli(dir.path(), &["show", "duplicate", "--json"]));
    assert!(error.contains("d1") && error.contains("d2"), "{error}");
    failure(cli(dir.path(), &["update", "--json"]));
    failure(cli(
        dir.path(),
        &["import", "graphify", "graph.json", "--json"],
    ));
    assert_eq!(fs::read(db).unwrap(), before);
    let human = cli(dir.path(), &["stats"]);
    assert!(human.status.success());
    assert!(String::from_utf8(human.stdout).unwrap().contains("6 nodes"));
}

#[test]
fn native_index_discovers_ancestors_and_update_uses_recorded_root() {
    let dir = tempdir().unwrap();
    let root = dir.path().join("project");
    fs::create_dir_all(root.join("nested")).unwrap();
    fs::write(
        root.join("demo.py"),
        "def leaf():\n    pass\n\ndef entry():\n    leaf()\n",
    )
    .unwrap();
    let initial = success(cli(dir.path(), &["index", "project", "--json"]));
    assert_eq!(initial["parsed_files"], 1);
    assert!(root.join(".graf/index.db").is_file());
    assert!(!dir.path().join(".graf").exists());
    let nested = root.join("nested");
    let initial_stats = success(cli(&nested, &["stats", "--json"]));
    assert_eq!(initial_stats["generation"], initial["generation"]);
    fs::write(root.join("demo.py"), "def replacement():\n    pass\n").unwrap();
    // Reads still see the indexed snapshot until update is explicitly requested.
    success(cli(&nested, &["show", "entry", "--json"]));
    let updated = success(cli(&nested, &["update", "--json"]));
    assert!(updated["generation"].as_u64().unwrap() > initial["generation"].as_u64().unwrap());
    success(cli(&nested, &["show", "replacement", "--json"]));
    failure(cli(&nested, &["show", "entry", "--json"]));
    let unchanged = success(cli(&nested, &["update", "--json"]));
    assert_eq!(unchanged["generation"], updated["generation"]);
}

#[test]
fn explicit_database_errors_and_limits_leave_stdout_and_disk_clean() {
    let dir = tempdir().unwrap();
    for command in ["stats", "update", "serve"] {
        failure(cli(dir.path(), &["--db", "missing.db", "--json", command]));
        assert!(!dir.path().join("missing.db").exists());
    }
    fs::write(dir.path().join("bad.json"), "{}").unwrap();
    failure(cli(
        dir.path(),
        &["import", "graphify", "bad.json", "--json"],
    ));
    assert!(!dir.path().join(".graf").exists());
    fs::write(
        dir.path().join("ok.json"),
        r#"{"directed":true,"multigraph":false,"nodes":[],"links":[]}"#,
    )
    .unwrap();
    success(cli(
        dir.path(),
        &[
            "--db",
            "custom.db",
            "import",
            "graphify",
            "ok.json",
            "--json",
        ],
    ));
    success(cli(dir.path(), &["stats", "--db", "custom.db", "--json"]));
    assert!(!dir.path().join(".graf").exists());
    for flags in [
        ["--depth", "7"],
        ["--limit", "0"],
        ["--limit", "501"],
        ["--direction", "sideways"],
    ] {
        failure(cli(
            dir.path(),
            &[
                "--db",
                "custom.db",
                "--json",
                "query",
                "x",
                flags[0],
                flags[1],
            ],
        ));
    }
}

fn assert_terminal_safe(text: &str) {
    assert!(text.chars().all(|c| !c.is_control() || c == '\n'));
    for c in ['\u{200b}', '\u{2028}', '\u{202e}', '\u{2066}', '\u{feff}'] {
        assert!(!text.contains(c));
    }
}

#[test]
fn human_output_escapes_controls_while_json_and_mcp_preserve_values() {
    let dir = tempdir().unwrap();
    let hostile = "normal\x1b[2J\x1b[H\n\r\t\u{85}\u{200b}\u{2028}\u{202e}\u{2066}\u{feff}";
    let escaped = r"normal\u{1b}[2J\u{1b}[H\n\r\t\u{85}\u{200b}\u{2028}\u{202e}\u{2066}\u{feff}";
    fs::write(
        dir.path().join("graph.json"),
        json!({
            "directed": true, "multigraph": false,
            "nodes": [
                {"id":"a", "label":hostile, "source_file":hostile, "file_type":hostile},
                {"id":hostile, "label":"café_東京"}
            ],
            "links": [{"source":"a", "target":hostile, "relation":hostile}]
        })
        .to_string(),
    )
    .unwrap();
    success(cli(
        dir.path(),
        &["import", "graphify", "graph.json", "--json"],
    ));
    let output = cli(dir.path(), &["show", "a"]);
    assert!(output.status.success());
    let human = String::from_utf8(output.stdout).unwrap();
    assert_terminal_safe(&human);
    assert!(human.contains(escaped));
    assert!(human.contains("café_東京"));
    let machine = success(cli(dir.path(), &["show", "a", "--json"]));
    let node = machine["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["id"] == "a")
        .unwrap();
    assert_eq!(node["label"], hostile);
    assert_eq!(node["file"], hostile);
    assert_eq!(machine["edges"][0]["relation"], hostile);
    let mut client = Mcp::start(dir.path(), "2025-11-25");
    let response = client.call("show", json!({"symbol":"a"}));
    assert_eq!(response["result"]["structuredContent"], machine);

    for args in [
        vec!["show", "missing\x1b[2J\n\u{202e}"],
        vec!["query", "a", "--direction", "bad\x1b[2J\n\u{202e}"],
    ] {
        let error = failure(cli(dir.path(), &args));
        assert_terminal_safe(&error);
        assert!(error.contains(r"\u{1b}[2J"), "{error:?}");
    }
    let help = cli(dir.path(), &["--help"]);
    assert!(help.status.success());
    assert!(
        String::from_utf8(help.stdout)
            .unwrap()
            .contains("Usage: graf")
    );
}

#[cfg(unix)]
#[test]
fn human_diagnostics_and_root_escape_filename_controls() {
    let dir = tempdir().unwrap();
    let root = dir.path().join("project\x1b[2J\n");
    fs::create_dir(&root).unwrap();
    let filename = "bad\x1b[2J\r\t\u{202e}.py";
    fs::write(root.join(filename), "def broken(:\n").unwrap();
    let output = cli(&root, &["index", "--json"]);
    let diagnostics = String::from_utf8(output.stderr.clone()).unwrap();
    assert_terminal_safe(&diagnostics);
    assert!(diagnostics.contains(r"bad\u{1b}[2J\r\t\u{202e}.py"));
    let report = success(output);
    assert_eq!(report["diagnostics"][0]["file"], filename);
    let stats = cli(&root, &["stats"]);
    assert!(stats.status.success());
    let human = String::from_utf8(stats.stdout).unwrap();
    assert_terminal_safe(&human);
    assert!(human.contains(r"project\u{1b}[2J\n"));
}

// A small stdio client fixture: the SDK owns all server protocol handling.
struct Mcp {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
}

impl Mcp {
    fn start(cwd: &Path, version: &str) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_graf"))
            .current_dir(cwd)
            .arg("serve")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (sender, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                if sender.send(line.unwrap()).is_err() {
                    break;
                }
            }
        });
        let mut client = Self {
            child,
            stdin,
            lines,
        };
        let init = client.request(json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
            "protocolVersion":version,"capabilities":{},"clientInfo":{"name":"graf-test","version":"1"}
        }}));
        assert_eq!(init["result"]["serverInfo"]["name"], "graf");
        // The SDK negotiates legacy initialize; 2026-07-28 uses discovery.
        let negotiated = if version == "2026-07-28" {
            "2025-11-25"
        } else {
            version
        };
        assert_eq!(init["result"]["protocolVersion"], negotiated);
        client.send(json!({"jsonrpc":"2.0","method":"notifications/initialized"}));
        client
    }

    fn send(&mut self, request: Value) {
        writeln!(self.stdin, "{request}").unwrap();
        self.stdin.flush().unwrap();
    }

    fn request(&mut self, request: Value) -> Value {
        let id = request["id"].clone();
        self.send(request);
        let line = self
            .lines
            .recv_timeout(Duration::from_secs(10))
            .expect("MCP response timeout");
        let response: Value =
            serde_json::from_str(&line).expect("MCP stdout must contain only protocol JSON");
        assert_eq!(response["id"], id);
        response
    }

    fn call(&mut self, name: &str, arguments: Value) -> Value {
        self.request(json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":name,"arguments":arguments}}))
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn mcp_stdio_has_typed_read_only_tools_and_honest_errors() {
    let dir = imported();
    let before = fs::read(dir.path().join(".graf/index.db")).unwrap();
    for version in ["2025-11-25", "2026-07-28"] {
        let mut client = Mcp::start(dir.path(), version);
        let listing = client.request(json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}));
        let tools = listing["result"]["tools"].as_array().unwrap();
        let mut names: Vec<_> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        names.sort_unstable();
        for required in [
            "callees",
            "callers",
            "impact",
            "path",
            "query",
            "show",
            "stats",
            "graph_stats",
            "god_nodes",
            "get_community",
        ] {
            assert!(names.contains(&required), "missing MCP tool {required}");
        }
        for tool in tools {
            assert_eq!(tool["annotations"]["readOnlyHint"], true);
        }
        let query = tools.iter().find(|t| t["name"] == "query").unwrap();
        assert_eq!(query["inputSchema"]["properties"]["limit"]["maximum"], 500);
        assert_eq!(query["inputSchema"]["properties"]["depth"]["maximum"], 6);
        let result = client.call("callers", json!({"symbol":"b"}));
        assert_eq!(result["result"]["isError"], false);
        assert_eq!(ids(&result["result"]["structuredContent"]), ["a", "b"]);
        assert_eq!(result["result"]["structuredContent"]["schema_version"], 1);
        let error = client.call("show", json!({"symbol":"duplicate"}));
        assert_eq!(error["result"]["isError"], true);
        for args in [
            json!({"text":"a","depth":7}),
            json!({"text":"a","limit":0}),
            json!({"text":"a","limit":501}),
            json!({"text":" "}),
        ] {
            let error = client.call("query", args);
            assert!(
                error["error"].is_object() || error["result"]["isError"] == true,
                "{error}"
            );
        }
        let error = client.call("query", json!({"text":"a", "db":"other.db"}));
        assert!(error["error"].is_object() || error["result"]["isError"] == true);
        let error = client.call("update", json!({}));
        assert!(error["error"].is_object() || error["result"]["isError"] == true);
    }
    assert_eq!(fs::read(dir.path().join(".graf/index.db")).unwrap(), before);
    assert!(!dir.path().join("other.db").exists());
}
