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
    assert_eq!(ids(&impact), ["a", "b", "c"]);
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
        assert_eq!(
            names,
            [
                "callees", "callers", "impact", "path", "query", "show", "stats"
            ]
        );
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
