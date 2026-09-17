use std::{
    fs,
    io::{BufRead, BufReader, Write},
    path::Path,
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc::{self, Receiver},
    time::Duration,
};

use graf::{import::read_graphify, store::Store};
use serde_json::{Value, json};
use tempfile::TempDir;

fn fixture(dir: &Path, name: &str, count: usize) -> std::path::PathBuf {
    let input = dir.join(format!("{name}.json"));
    fs::write(&input, json!({"directed": true, "multigraph": false, "nodes": (0..count).map(|i|
        json!({"id": format!("n{i}"), "label": format!("{name}{i}"), "source_file":"sample.py"})).collect::<Vec<_>>(),
        "links": (1..count).map(|i| json!({"source":"n0", "target":format!("n{i}"),
            "relation":"calls", "confidence":"EXTRACTED"})).collect::<Vec<_>>() }).to_string()).unwrap();
    let db = dir.join(format!("{name}.db"));
    Store::create(&db)
        .unwrap()
        .import_graph(read_graphify(&input).unwrap())
        .unwrap();
    db
}

struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct Mcp {
    process: Process,
    input: ChildStdin,
    output: Receiver<String>,
}
impl Mcp {
    fn start(db: &Path, extra: &[String]) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_graf"))
            .args(["--db", db.to_str().unwrap(), "serve"])
            .args(extra)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let input = child.stdin.take().unwrap();
        let output = lines(child.stdout.take().unwrap());
        let mut client = Self {
            process: Process(child),
            input,
            output,
        };
        assert_eq!(
            client.request("initialize", initialize())["result"]["serverInfo"]["name"],
            "graf"
        );
        writeln!(
            client.input,
            "{}",
            json!({"jsonrpc":"2.0","method":"notifications/initialized"})
        )
        .unwrap();
        client.input.flush().unwrap();
        client
    }
    fn request(&mut self, method: &str, params: Value) -> Value {
        writeln!(
            self.input,
            "{}",
            json!({"jsonrpc":"2.0","id":7,"method":method,"params":params})
        )
        .unwrap();
        self.input.flush().unwrap();
        serde_json::from_str(
            &self
                .output
                .recv_timeout(Duration::from_secs(10))
                .expect("MCP response timeout"),
        )
        .unwrap()
    }
    fn call(&mut self, name: &str, arguments: Value) -> Value {
        self.request("tools/call", json!({"name":name,"arguments":arguments}))
    }
    fn resource(&mut self, uri: &str) -> Value {
        self.request("resources/read", json!({"uri":uri}))
    }
}
fn lines(reader: impl std::io::Read + Send + 'static) -> Receiver<String> {
    let (send, receive) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(reader).lines() {
            if send.send(line.unwrap()).is_err() {
                break;
            }
        }
    });
    receive
}
fn initialize() -> Value {
    json!({"protocolVersion":"2025-11-25", "capabilities":{}, "clientInfo":{"name":"test","version":"1"}})
}
fn value(response: &Value) -> &Value {
    assert_eq!(response["result"]["isError"], false, "{response}");
    &response["result"]["structuredContent"]
}
fn failed(response: &Value) -> bool {
    response["error"].is_object() || response["result"]["isError"] == true
}
fn resource_value(response: Value) -> Value {
    serde_json::from_str(response["result"]["contents"][0]["text"].as_str().unwrap()).unwrap()
}

#[test]
fn stdio_registered_projects_resources_and_graphify_tools_are_read_only() {
    let dir = TempDir::new().unwrap();
    let db = fixture(dir.path(), "primary", 3);
    let other = fixture(dir.path(), "secondary", 2);
    let before = (fs::read(&db).unwrap(), fs::read(&other).unwrap());
    let mut mcp = Mcp::start(
        &db,
        &["--project".into(), format!("other={}", other.display())],
    );
    let listing = mcp.request("tools/list", json!({}));
    let tools = listing["result"]["tools"].as_array().unwrap();
    for name in [
        "query",
        "show",
        "callers",
        "callees",
        "impact",
        "path",
        "stats",
        "query_graph",
        "get_node",
        "get_neighbors",
        "shortest_path",
        "graph_stats",
        "god_nodes",
        "get_community",
    ] {
        let tool = tools.iter().find(|t| t["name"] == name).unwrap();
        assert_eq!(tool["annotations"]["readOnlyHint"], true);
        assert!(
            tool["inputSchema"]["properties"]["project"].is_object(),
            "{tool}"
        );
    }
    assert_eq!(value(&mcp.call("stats", json!({})))["nodes"], 3);
    assert_eq!(
        value(&mcp.call("stats", json!({"project":"other"})))["nodes"],
        2
    );
    assert_eq!(
        value(&mcp.call(
            "query",
            json!({"project":"other","text":"secondary0","depth":0})
        ))["nodes"][0]["label"],
        "secondary0"
    );
    let query = mcp.call(
        "query_graph",
        json!({"project":"other", "question":"secondary0", "mode":"dfs"}),
    );
    assert_eq!(value(&query)["graph"]["nodes"].as_array().unwrap().len(), 2);
    let node = mcp.call(
        "get_node",
        json!({"project":"other","label":"sample.py::secondary0"}),
    );
    assert_eq!(value(&node)["graph"]["nodes"][0]["id"], "n0");
    let neighbors = mcp.call(
        "get_neighbors",
        json!({"label":"n0","relation_filter":"calls"}),
    );
    assert_eq!(
        value(&neighbors)["graph"]["nodes"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    assert_eq!(
        value(&mcp.call("shortest_path", json!({"source":"n1","target":"n0"})))["found"],
        false
    );
    assert_eq!(
        value(&mcp.call(
            "shortest_path",
            json!({"source":"n1","target":"n0","undirected":true})
        ))["found"],
        true
    );
    let stats = mcp.call("graph_stats", json!({}));
    assert_eq!(value(&stats)["confidence_counts"]["EXTRACTED"], 2);
    assert_eq!(
        value(&mcp.call("god_nodes", json!({"top_n":1})))["nodes"][0]["id"],
        "n0"
    );
    let hubs = mcp.call("god_nodes", json!({"exclude_hubs_percentile":0}));
    assert!(
        value(&hubs)["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .all(|n| n["id"] != "n0")
    );
    let resources = mcp.request("resources/list", json!({}));
    assert!(
        resources["result"]["resources"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["uri"] == "graf://projects/other/graph")
    );
    let communities = resource_value(mcp.resource("graf://communities"));
    let community = mcp.call(
        "get_community",
        json!({"community_id":communities["communities"][0]["id"],"limit":1}),
    );
    assert_eq!(value(&community)["nodes"].as_array().unwrap().len(), 1);
    assert_eq!(
        resource_value(mcp.resource("graf://projects/other/graph"))["nodes"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    for name in ["stats", "god-nodes", "surprises", "audit", "questions"] {
        assert_eq!(
            resource_value(mcp.resource(&format!("graf://{name}"))),
            resource_value(mcp.resource(&format!("graphify://{name}")))
        );
    }
    assert!(
        mcp.resource("graphify://report")["result"]["contents"][0]["text"]
            .as_str()
            .unwrap()
            .contains("# Graf graph report")
    );
    for args in [
        json!({"project":"../secondary.db"}),
        json!({"project_path":dir.path()}),
        json!({"db":other}),
    ] {
        assert!(failed(&mcp.call("stats", args)));
    }
    assert!(failed(
        &mcp.call("query", json!({"text":"n0","project_path":dir.path()}))
    ));
    assert!(failed(
        &mcp.call("get_community", json!({"community_id":9999}))
    ));
    assert!(failed(&mcp.call("god_nodes", json!({"top_n":0}))));
    assert!(failed(
        &mcp.call("god_nodes", json!({"exclude_hubs_percentile":101}))
    ));
    assert!(failed(
        &mcp.call("query_graph", json!({"question":"n0","token_budget":0}))
    ));
    assert!(failed(&mcp.resource("graf://projects/unknown/graph")));
    assert!(failed(&mcp.resource("file:///etc/passwd")));
    assert!(failed(&mcp.call("update", json!({}))));
    assert_eq!(value(&mcp.call("stats", json!({})))["nodes"], 3);
    assert_eq!((fs::read(&db).unwrap(), fs::read(&other).unwrap()), before);
}

#[test]
fn insight_resources_preserve_ranked_evidence_and_candidate_counts() {
    let dir = TempDir::new().unwrap();
    let input = dir.path().join("insights.json");
    fs::write(&input, json!({"directed":true,"multigraph":false,
        "nodes": (0..10).map(|i| json!({"id":format!("n{i}"), "label":format!("handler{i}"),
            "file_type":"function", "source_file":format!("module{i}/handler.py")})).collect::<Vec<_>>(),
        "links": (1..10).map(|i| json!({"source":"n0", "target":format!("n{i}"),
            "relation":"calls", "confidence":"AMBIGUOUS", "source_file":"module0/handler.py",
            "source_location":"L12", "evidence_tag":format!("record-{i}")})).collect::<Vec<_>>()
    }).to_string()).unwrap();
    let db = dir.path().join("insights.db");
    Store::create(&db)
        .unwrap()
        .import_graph(read_graphify(&input).unwrap())
        .unwrap();
    let snapshot = Store::open_read_only(&db).unwrap().snapshot().unwrap();
    let evidence: Vec<_> = snapshot
        .edges
        .iter()
        .map(|edge| serde_json::to_value(edge).unwrap())
        .collect();
    let before = fs::read(&db).unwrap();
    let mut mcp = Mcp::start(&db, &[]);
    let surprises = resource_value(mcp.resource("graf://surprises"));
    assert_eq!(surprises["generation"], snapshot.generation);
    assert_eq!(surprises["surprise_candidates"], 9);
    assert_eq!(surprises["truncated"], true);
    let rows = surprises["surprises"].as_array().unwrap();
    assert_eq!(rows.len(), 5);
    for row in rows {
        assert!(row["score"].as_u64().unwrap() > 0);
        assert!(!row["signals"].as_array().unwrap().is_empty());
        assert!(evidence.contains(&row["edge"]), "{row}");
        assert_eq!(row["source_file"], "module0/handler.py");
        assert!(
            row["target_file"]
                .as_str()
                .unwrap()
                .ends_with("/handler.py")
        );
        assert!(row["source_community"].is_u64() && row["target_community"].is_u64());
    }
    assert!(
        rows.windows(2)
            .all(|pair| pair[0]["score"].as_u64() >= pair[1]["score"].as_u64())
    );
    let questions = resource_value(mcp.resource("graf://questions"));
    assert_eq!(questions["generation"], snapshot.generation);
    assert!(questions["suggested_question_candidates"].as_u64().unwrap() >= 9);
    assert_eq!(questions["truncated"], true);
    let rows = questions["questions"].as_array().unwrap();
    assert_eq!(rows.len(), 7);
    assert!(rows.iter().any(|row| row["kind"] == "ambiguous_edge"));
    for row in rows {
        assert!(!row["kind"].as_str().unwrap().is_empty());
        assert!(!row["question"].as_str().unwrap().is_empty());
        assert!(!row["why"].as_str().unwrap().is_empty());
        assert!(row["node_ids"].is_array() && row["community_ids"].is_array());
        let edges = row["edge_evidence"].as_array().unwrap();
        assert!(edges.len() <= 3);
        assert!(row["evidence_count"].as_u64().unwrap() >= edges.len() as u64);
        assert!(edges.iter().all(|edge| evidence.contains(edge)));
    }
    assert_eq!(
        surprises,
        resource_value(mcp.resource("graphify://surprises"))
    );
    assert_eq!(
        questions,
        resource_value(mcp.resource("graphify://questions"))
    );
    assert_eq!(fs::read(&db).unwrap(), before);
}

#[test]
fn cached_analysis_tracks_explicit_database_updates() {
    let dir = TempDir::new().unwrap();
    let db = fixture(dir.path(), "primary", 3);
    let replacement = fixture(dir.path(), "replacement", 2);
    let mut mcp = Mcp::start(&db, &[]);
    assert_eq!(value(&mcp.call("graph_stats", json!({})))["nodes"], 3);
    let old = value(&mcp.call("graph_stats", json!({})))["generation"]
        .as_u64()
        .unwrap();
    drop(replacement);
    Store::create(&db)
        .unwrap()
        .refresh_import(read_graphify(&dir.path().join("replacement.json")).unwrap())
        .unwrap();
    let response = mcp.call("graph_stats", json!({}));
    assert_eq!(value(&response)["nodes"], 2);
    assert!(value(&response)["generation"].as_u64().unwrap() > old);
}

fn http_server(db: &Path) -> (Process, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_graf"))
        .args([
            "--db",
            db.to_str().unwrap(),
            "serve",
            "--transport",
            "http",
            "--port",
            "0",
            "--path",
            "/test-mcp",
            "--bearer-token-env",
            "GRAF_TEST_BEARER",
        ])
        .env("GRAF_TEST_BEARER", "synthetic-token")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stderr = lines(child.stderr.take().unwrap());
    let process = Process(child);
    let line = stderr
        .recv_timeout(Duration::from_secs(10))
        .expect("HTTP readiness timeout");
    let url = line
        .strip_prefix("Graf MCP listening on ")
        .expect(&line)
        .to_owned();
    (process, url)
}

#[test]
fn http_uses_streamable_protocol_auth_host_origin_and_body_bounds() {
    let dir = TempDir::new().unwrap();
    let db = fixture(dir.path(), "primary", 3);
    let before = fs::read(&db).unwrap();
    let (_process, url) = http_server(&db);
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let post = || {
        client
            .post(&url)
            .header("Accept", "application/json, text/event-stream")
            .header("MCP-Protocol-Version", "2025-11-25")
    };
    let init = json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":initialize()});
    assert_eq!(post().json(&init).send().unwrap().status(), 401);
    assert_eq!(
        post()
            .bearer_auth("incorrect")
            .json(&init)
            .send()
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        post()
            .bearer_auth("synthetic-token")
            .header("Host", "attacker.example")
            .json(&init)
            .send()
            .unwrap()
            .status(),
        403
    );
    assert_eq!(
        post()
            .bearer_auth("synthetic-token")
            .header("Origin", "https://attacker.example")
            .json(&init)
            .send()
            .unwrap()
            .status(),
        403
    );
    let response: Value = post()
        .header("Authorization", "bEaReR synthetic-token")
        .json(&init)
        .send()
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(response["result"]["serverInfo"]["name"], "graf");
    let request = |method: &str, params: Value| -> Value {
        post()
            .bearer_auth("synthetic-token")
            .json(&json!({"jsonrpc":"2.0","id":2,"method":method,"params":params}))
            .send()
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .unwrap()
    };
    assert!(
        request("tools/list", json!({}))["result"]["tools"]
            .as_array()
            .unwrap()
            .len()
            >= 14
    );
    let stats = request("tools/call", json!({"name":"graph_stats","arguments":{}}));
    assert_eq!(value(&stats)["nodes"], 3);
    assert_eq!(
        resource_value(request("resources/read", json!({"uri":"graf://graph"})))["nodes"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    assert_eq!(
        post()
            .bearer_auth("synthetic-token")
            .header("Content-Type", "application/json")
            .body("x".repeat(1024 * 1024 + 1))
            .send()
            .unwrap()
            .status(),
        413
    );
    assert_eq!(
        post()
            .bearer_auth("synthetic-token")
            .header("Content-Type", "application/json")
            .body("{")
            .send()
            .unwrap()
            .status(),
        // rmcp classifies malformed JSON as unsupported media.
        415
    );
    assert_eq!(
        value(&request(
            "tools/call",
            json!({"name":"stats","arguments":{}})
        ))["nodes"],
        3
    );
    assert_eq!(fs::read(&db).unwrap(), before);
}

#[test]
fn startup_rejects_unauthenticated_nonloopback_and_unregistered_databases() {
    let dir = TempDir::new().unwrap();
    let db = fixture(dir.path(), "primary", 2);
    for args in [
        vec!["--transport", "http", "--host", "0.0.0.0"],
        vec!["--project", "default=missing.db"],
        vec!["--project", "../bad=missing.db"],
        vec!["--project", "missing=missing.db"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_graf"))
            .current_dir(dir.path())
            .args(["--db", db.to_str().unwrap(), "serve"])
            .args(args)
            .output()
            .unwrap();
        assert!(!output.status.success());
    }
    assert!(!dir.path().join("missing.db").exists());
}

#[cfg(unix)]
#[test]
fn invalid_bearer_environment_does_not_echo_its_value() {
    use std::os::unix::ffi::OsStringExt;
    let dir = TempDir::new().unwrap();
    let db = fixture(dir.path(), "primary", 2);
    let mut token = b"synthetic-sensitive-value".to_vec();
    token.push(0xff);
    let output = Command::new(env!("CARGO_BIN_EXE_graf"))
        .args([
            "--db",
            db.to_str().unwrap(),
            "serve",
            "--transport",
            "http",
            "--bearer-token-env",
            "GRAF_TEST_INVALID_BEARER",
        ])
        .env(
            "GRAF_TEST_INVALID_BEARER",
            std::ffi::OsString::from_vec(token),
        )
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("bearer environment variable"), "{stderr}");
    assert!(!stderr.contains("synthetic-sensitive-value"));
}

#[test]
fn oversized_stdio_message_ends_transport() {
    let dir = TempDir::new().unwrap();
    let db = fixture(dir.path(), "primary", 2);
    let mut mcp = Mcp::start(&db, &[]);
    let _ = writeln!(mcp.input, "{}", "x".repeat(1024 * 1024 + 1));
    let _ = mcp.input.flush();
    assert!(mcp.output.recv_timeout(Duration::from_secs(5)).is_err());
    for _ in 0..100 {
        if mcp.process.0.try_wait().unwrap().is_some() {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("oversized input did not close the server");
}
