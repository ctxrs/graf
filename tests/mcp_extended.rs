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
        Self::start_with_env(db, extra, &[])
    }
    fn start_with_env(db: &Path, extra: &[String], env: &[(&str, String)]) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_graf"))
            .args(["--db", db.to_str().unwrap(), "serve"])
            .args(extra)
            .current_dir(db.parent().unwrap())
            .envs(env.iter().map(|(name, value)| (*name, value)))
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

fn learning_fixture(dir: &Path) -> (std::path::PathBuf, graf::model::GraphSnapshot, String) {
    let root = dir.join("source");
    fs::create_dir(&root).unwrap();
    fs::write(
        root.join("policy.py"),
        "def cache_policy():\n    return 1\n",
    )
    .unwrap();
    fs::write(
        root.join("unrelated.py"),
        "def unrelated():\n    return 2\n",
    )
    .unwrap();
    let db = dir.join("learning.db");
    graf::index::run(&root, &db).unwrap();
    let graph = Store::open_read_only(&db).unwrap().snapshot().unwrap();
    let id = graph
        .nodes
        .iter()
        .find(|node| node.label == "cache_policy")
        .unwrap()
        .id
        .clone();
    (db, graph, id)
}

fn remember(dir: &Path, graph: &graf::model::GraphSnapshot, id: &str) {
    graf::memory::save_result(
        &graf::memory::SaveResultArgs {
            question: "Where is this policy defined?".into(),
            answer: Some("Saved answer text must not appear in selected-node annotations.".into()),
            answer_file: None,
            query_type: "query".into(),
            nodes: vec![id.into()],
            outcome: Some(graf::memory::Outcome::Useful),
            correction: None,
            memory_dir: dir.join("memory"),
        },
        Some(graph),
    )
    .unwrap();
}

fn without_learning(mut response: Value) -> Value {
    response.as_object_mut().unwrap().remove("learning");
    response.as_object_mut().unwrap().remove("learning_notice");
    response
}

#[test]
fn learning_annotations_require_opt_in_and_are_scoped_to_default_selected_nodes() {
    let dir = TempDir::new().unwrap();
    let (db, graph, id) = learning_fixture(dir.path());
    remember(dir.path(), &graph, &id);
    remember(dir.path(), &graph, &id);
    let unrelated = &graph
        .nodes
        .iter()
        .find(|node| node.label == "unrelated")
        .unwrap()
        .id;
    remember(dir.path(), &graph, unrelated);
    let before = fs::read(&db).unwrap();
    let mut plain = Mcp::start(&db, &[]);
    let mut enabled = Mcp::start(
        &db,
        &[
            "--memory-dir".into(),
            dir.path().join("memory").display().to_string(),
            "--project".into(),
            format!("other={}", db.display()),
        ],
    );
    for (tool, args) in [
        (
            "query_graph",
            json!({"question":"cache policy","depth":0,"context_filter":["calls"],"files":["policy.py"]}),
        ),
        ("get_node", json!({"label":id})),
        ("get_neighbors", json!({"label":id})),
    ] {
        let baseline = value(&plain.call(tool, args.clone())).clone();
        assert!(baseline.get("learning").is_none());
        let annotated = value(&enabled.call(tool, args.clone())).clone();
        assert_eq!(without_learning(annotated.clone()), baseline);
        assert_eq!(annotated["learning"]["nodes"][&id]["status"], "preferred");
        assert_eq!(annotated["learning"]["nodes"][&id]["verified_useful"], 2);
        assert!(annotated["learning"]["nodes"].get(unrelated).is_none());
        assert!(annotated["learning"].get("lessons").is_none());
        assert!(!annotated.to_string().contains("Saved answer text"));
        let hash = blake3::hash(&serde_json::to_vec(&graph).unwrap())
            .to_hex()
            .to_string();
        assert_eq!(annotated["learning"]["snapshot_hash"], hash);
        let mut routed = args.clone();
        routed["project"] = json!("other");
        assert_eq!(*value(&enabled.call(tool, routed)), baseline);
        for key in ["memory_dir", "project_path"] {
            let mut injected = args.clone();
            injected[key] = json!(dir.path());
            assert!(failed(&enabled.call(tool, injected)));
        }
    }
    // The shared show schema must not acquire CLI-only filesystem arguments.
    assert!(failed(
        &enabled.call("show", json!({"symbol":id,"memory_dir":dir.path()}))
    ));
    assert_eq!(fs::read(&db).unwrap(), before);
    assert_eq!(fs::read_dir(dir.path().join("memory")).unwrap().count(), 3);
    assert!(!dir.path().join("graf-out").exists());
}

#[test]
fn warm_learning_annotations_recheck_memory_source_and_updated_snapshot() {
    let dir = TempDir::new().unwrap();
    let (db, graph, id) = learning_fixture(dir.path());
    remember(dir.path(), &graph, &id);
    let mut client = Mcp::start(
        &db,
        &[
            "--memory-dir".into(),
            dir.path().join("memory").display().to_string(),
        ],
    );
    let first = value(&client.call("get_node", json!({"label":id}))).clone();
    assert_eq!(first["learning"]["nodes"][&id]["status"], "tentative");
    remember(dir.path(), &graph, &id);
    let preferred = value(&client.call("get_node", json!({"label":id}))).clone();
    assert_eq!(preferred["learning"]["nodes"][&id]["status"], "preferred");
    assert_eq!(preferred["graph"], first["graph"]);
    fs::write(
        dir.path().join("source/policy.py"),
        "def cache_policy():\n    return 9\n",
    )
    .unwrap();
    let stale = value(&client.call("get_node", json!({"label":id}))).clone();
    assert_eq!(stale["learning"]["nodes"][&id]["status"], "stale");
    assert_eq!(stale["learning"]["nodes"][&id]["verified_useful"], 0);
    assert_eq!(stale["graph"], first["graph"]);
    graf::index::run(&dir.path().join("source"), &db).unwrap();
    let updated = Store::open_read_only(&db).unwrap().snapshot().unwrap();
    let refreshed = value(&client.call("get_node", json!({"label":id}))).clone();
    assert_ne!(
        refreshed["graph"]["generation"],
        first["graph"]["generation"]
    );
    assert_eq!(refreshed["learning"]["nodes"][&id]["status"], "stale");
    remember(dir.path(), &updated, &id);
    remember(dir.path(), &updated, &id);
    let verified = value(&client.call("get_node", json!({"label":id}))).clone();
    assert_eq!(verified["learning"]["nodes"][&id]["status"], "preferred");
    assert_eq!(verified["learning"]["nodes"][&id]["verified_useful"], 2);
    assert_ne!(
        verified["learning"]["snapshot_hash"],
        first["learning"]["snapshot_hash"]
    );
}

#[test]
fn learning_limits_and_bad_memory_leave_sql_results_available() {
    let dir = TempDir::new().unwrap();
    let (db, graph, id) = learning_fixture(dir.path());
    remember(dir.path(), &graph, &id);
    let memory = dir.path().join("memory").display().to_string();
    let mut plain = Mcp::start(&db, &[]);
    let mut enabled = Mcp::start(&db, &["--memory-dir".into(), memory.clone()]);
    let baseline = value(&plain.call("get_neighbors", json!({"label":id}))).clone();
    let bytes = serde_json::to_vec(&baseline["graph"]).unwrap().len();
    for budget in [bytes.div_ceil(4) + 1, bytes.div_ceil(4) + 75, 2000] {
        let args = json!({"label":id,"token_budget":budget});
        let expected = value(&plain.call("get_neighbors", args.clone())).clone();
        let actual = value(&enabled.call("get_neighbors", args)).clone();
        assert_eq!(without_learning(actual.clone()), expected);
        if let Some(learning) = actual.get("learning") {
            assert!(
                serde_json::to_vec(&actual["graph"]).unwrap().len()
                    + serde_json::to_vec(&json!({"learning":learning}))
                        .unwrap()
                        .len()
                    <= budget * 4
            );
            if learning["status"] == "truncated" {
                assert!(
                    actual["learning_notice"]
                        .as_str()
                        .unwrap()
                        .contains("budget")
                );
            }
        } else {
            assert!(
                actual["learning_notice"]
                    .as_str()
                    .unwrap()
                    .contains("budget")
            );
        }
    }
    let mut bounded = Mcp::start(
        &db,
        &[
            "--memory-dir".into(),
            memory,
            "--snapshot-max-bytes".into(),
            "1".into(),
        ],
    );
    let omitted = value(&bounded.call("get_neighbors", json!({"label":id}))).clone();
    assert_eq!(without_learning(omitted.clone()), baseline);
    assert!(
        omitted["learning_notice"]
            .as_str()
            .unwrap()
            .contains("snapshot unavailable")
    );
    // Oversized records fail the reader's fixed cap even on an otherwise warm server.
    fs::write(
        dir.path().join("memory/oversized.md"),
        vec![b'x'; 2 * 1024 * 1024 + 1],
    )
    .unwrap();
    let invalid = value(&enabled.call("get_neighbors", json!({"label":id}))).clone();
    assert_eq!(without_learning(invalid.clone()), baseline);
    assert!(
        invalid["learning_notice"]
            .as_str()
            .unwrap()
            .contains("memory could not be read")
    );
}

#[test]
fn learning_on_large_snapshots_does_not_compute_analysis() {
    let dir = TempDir::new().unwrap();
    let db = fixture(dir.path(), "large", 25_001);
    let graph = Store::open_read_only(&db).unwrap().snapshot().unwrap();
    remember(dir.path(), &graph, "n0");
    remember(dir.path(), &graph, "n0");
    let mut client = Mcp::start(
        &db,
        &[
            "--memory-dir".into(),
            dir.path().join("memory").display().to_string(),
        ],
    );
    let response = client.call("get_node", json!({"label":"n0"}));
    assert_eq!(
        value(&response)["graph"]["nodes"].as_array().unwrap().len(),
        1
    );
    assert_eq!(
        value(&response)["learning"]["nodes"]["n0"]["status"],
        "tentative"
    );
    assert_eq!(
        value(&response)["learning"]["nodes"]["n0"]["verified_useful"],
        0
    );
    assert!(failed(&client.call("graph_stats", json!({}))));
}

#[test]
fn learning_cannot_turn_a_near_limit_sql_response_into_an_error() {
    let dir = TempDir::new().unwrap();
    let db = fixture(dir.path(), "wide", 1);
    let options = graf::query::SearchOptions {
        graph: graf::model::QueryOptions {
            depth: 0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut store = Store::create(&db).unwrap();
    let mut graph = store.snapshot().unwrap();
    graph.nodes[0].metadata["padding"] = json!("");
    let imported = |graph: &graf::model::GraphSnapshot| graf::model::ImportedGraph {
        nodes: graph.nodes.clone(),
        edges: graph.edges.clone(),
        metadata: graph.metadata.clone(),
    };
    store.refresh_import(imported(&graph)).unwrap();
    let size = serde_json::to_vec(&store.neighbors_resolved("n0", &options).unwrap())
        .unwrap()
        .len();
    graph.nodes[0].metadata["padding"] = json!("x".repeat(512 * 1024 - 8 - size));
    store.refresh_import(imported(&graph)).unwrap();
    let graph = store.snapshot().unwrap();
    drop(store);
    remember(dir.path(), &graph, "n0");
    let mut plain = Mcp::start(&db, &[]);
    let baseline = value(&plain.call("get_node", json!({"label":"n0"}))).clone();
    assert!((512 * 1024 - 12..=512 * 1024).contains(&serde_json::to_vec(&baseline).unwrap().len()));
    let mut enabled = Mcp::start(
        &db,
        &[
            "--memory-dir".into(),
            dir.path().join("memory").display().to_string(),
        ],
    );
    let response = enabled.call("get_node", json!({"label":"n0"}));
    assert_eq!(*value(&response), baseline);
    assert!(
        response["result"]["content"]
            .as_array()
            .unwrap()
            .iter()
            .any(|part| part["text"].as_str() == Some("Learning omitted: response size limit."))
    );
}

fn membership_fixture(dir: &Path, count: usize) -> std::path::PathBuf {
    let input = dir.join("memberships.json");
    fs::write(&input, json!({"directed":true,"multigraph":false,
        "nodes":(0..count).map(|i| json!({"id":format!("n{i:05}"),
            "label":format!("Item{i:05}"),"source_file":if i == 0 {"sample.py"} else {"bulk.rs"},
            "community":if i == 1 {json!("7")} else {json!(7)},"community_name":"Imported"})).collect::<Vec<_>>(),
        "links":[]}).to_string()).unwrap();
    let db = dir.join("memberships.db");
    Store::create(&db)
        .unwrap()
        .import_graph(read_graphify(&input).unwrap())
        .unwrap();
    db
}

#[test]
fn large_preserved_communities_skip_analysis_and_refresh_by_generation() {
    let dir = TempDir::new().unwrap();
    let db = membership_fixture(dir.path(), 25_001);
    let mut client = Mcp::start(&db, &[]);
    let resource = resource_value(client.resource("graf://communities"));
    assert_eq!(resource["community_source"], "preserved");
    assert_eq!(resource["computed_status"], "not_requested");
    assert!(resource["computed_communities"].is_null());
    let response = client.call("get_community", json!({"community_id":7,"limit":1}));
    assert_eq!(value(&response)["total_nodes"], 25_000);
    assert_eq!(value(&response)["nodes"][0]["id"], "n00000");
    assert_eq!(value(&response)["truncated"], true);
    let text = client.call("get_community", json!({"community_id":"7"}));
    assert_eq!(value(&text)["nodes"][0]["id"], "n00001");
    // Explicit analysis still has its original bound. Its cached failure must
    // neither initialize a partial report nor poison preserved/ordinary reads.
    assert!(failed(&client.call(
        "get_community",
        json!({"community_id":0,"community_source":"computed"})
    )));
    assert!(failed(&client.resource("graf://computed-communities")));
    assert_eq!(
        value(&client.call("get_node", json!({"label":"n00000"})))["seeds"][0],
        "n00000"
    );
    assert_eq!(
        value(&client.call("get_community", json!({"community_id":"7"})))["total_nodes"],
        1
    );
    let mut graph = read_graphify(&dir.path().join("memberships.json")).unwrap();
    graph.nodes[1].metadata["community_name"] = json!("Renamed");
    Store::open(&db).unwrap().refresh_import(graph).unwrap();
    let refreshed = client.call("get_community", json!({"community_id":"7"}));
    assert_eq!(value(&refreshed)["generation"], 2);
    assert_eq!(value(&refreshed)["community_names"], json!(["Renamed"]));
    let refreshed = resource_value(client.resource("graphify://communities"));
    assert_eq!(refreshed["generation"], 2);
    // Same IDs and counts can still have a different partition. Generation,
    // not total membership or a sidecar, is the cache authority.
    let mut graph = read_graphify(&dir.path().join("memberships.json")).unwrap();
    graph.nodes[0].metadata["community"] = json!("7");
    graph.nodes[1].metadata["community"] = json!(7);
    Store::open(&db).unwrap().refresh_import(graph).unwrap();
    let regrouped = client.call("get_community", json!({"community_id":"7"}));
    assert_eq!(value(&regrouped)["generation"], 3);
    assert_eq!(value(&regrouped)["total_nodes"], 1);
    assert_eq!(value(&regrouped)["nodes"][0]["id"], "n00000");
}

#[test]
fn snapshot_payload_budget_is_explicit_and_does_not_restrict_sql_queries() {
    let dir = TempDir::new().unwrap();
    let db = membership_fixture(dir.path(), 10);
    let mut small = Mcp::start(&db, &["--snapshot-max-bytes".into(), "1".into()]);
    assert!(failed(
        &small.call("get_community", json!({"community_id":7}))
    ));
    assert!(failed(&small.resource("graf://communities")));
    assert_eq!(
        value(&small.call("get_node", json!({"label":"n00000"})))["seeds"][0],
        "n00000"
    );
    let mut adequate = Mcp::start(&db, &["--snapshot-max-bytes".into(), "1048576".into()]);
    assert_eq!(
        value(&adequate.call("get_community", json!({"community_id":7})))["total_nodes"],
        9
    );
}

#[cfg(unix)]
#[test]
fn large_preserved_pr_impact_uses_changed_files_without_clustering() {
    let dir = TempDir::new().unwrap();
    let db = membership_fixture(dir.path(), 25_001);
    let env = fixture_pr_backend(dir.path());
    let mut client = Mcp::start_with_env(&db, &["--github-repo".into(), "acme/tools".into()], &env);
    let response = client.call("get_pr_impact", json!({"pr_number":7}));
    let impact = &value(&response)["entries"][0]["impact"];
    assert_eq!(impact["nodes"].as_array().unwrap().len(), 1);
    assert_eq!(impact["nodes"][0]["id"], "n00000");
    assert_eq!(impact["communities"][0]["source"], "stored");
    assert_eq!(impact["communities"][0]["id"], json!(7));
    assert_eq!(impact["communities"][0]["names"], json!(["Imported"]));
    assert_eq!(impact["generation"], 1);
}

#[test]
#[cfg(unix)]
fn large_unlabeled_pr_impact_keeps_all_changed_file_nodes_and_omission_notice() {
    let dir = TempDir::new().unwrap();
    let db = membership_fixture(dir.path(), 25_001);
    let mut graph = read_graphify(&dir.path().join("memberships.json")).unwrap();
    for node in &mut graph.nodes {
        let metadata = node.metadata.as_object_mut().unwrap();
        metadata.remove("community");
        metadata.remove("community_name");
    }
    // A second relevant node at the end proves impact is not an early sample.
    graph.nodes.last_mut().unwrap().file = "sample.py".into();
    let expected: Vec<_> = graph
        .nodes
        .iter()
        .filter(|node| node.file == "sample.py")
        .map(|node| serde_json::to_value(node).unwrap())
        .collect();
    Store::open(&db).unwrap().refresh_import(graph).unwrap();
    let before = fs::read(&db).unwrap();
    let env = fixture_pr_backend(dir.path());
    let mut client = Mcp::start_with_env(&db, &["--github-repo".into(), "acme/tools".into()], &env);
    for (tool, args) in [
        ("get_pr_impact", json!({"pr_number":7})),
        ("triage_prs", json!({})),
    ] {
        let response = client.call(tool, args);
        let report = value(&response);
        assert_eq!(report["graph_available"], true);
        for entry in report["entries"].as_array().unwrap() {
            let impact = &entry["impact"];
            assert_eq!(impact["nodes"], json!(expected), "{tool}");
            assert_eq!(impact["generation"], 2);
            assert_eq!(impact["nodes_without_community"], 2);
            assert!(impact["communities"].as_array().unwrap().is_empty());
        }
        assert!(
            report["notices"].as_array().unwrap().iter().any(|notice| {
                let text = notice.as_str().unwrap();
                text.contains("Computed communities omitted")
                    && text.contains("File/node impact remains available")
                    && text.contains("empty communities do not mean no community overlap")
            }),
            "{report}"
        );
    }
    assert_eq!(fs::read(&db).unwrap(), before);
}

#[test]
fn community_tools_preserve_typed_ids_composition_paths_and_computed_access() {
    let dir = TempDir::new().unwrap();
    let input = dir.path().join("communities.json");
    fs::write(&input, json!({"directed":true,"multigraph":false,"nodes":[
        {"id":"a","label":"First","source_file":"a.rs","community":7,"community_name":"Backend"},
        {"id":"b","label":"Second","source_file":"a.rs","community":7,"community_name":"Older name"},
        {"id":"s","label":"String","source_file":"s.rs","community":"7","community_name":"String identity"},
        {"id":"unassigned","label":"Unassigned","source_file":"u.rs"}
    ],"links":[]}).to_string()).unwrap();
    let mut imported = read_graphify(&input).unwrap();
    let snapshot = |nodes: Vec<graf::model::Node>| graf::model::GraphSnapshot {
        schema_version: graf::model::SCHEMA_VERSION,
        generation: 1,
        kind: "imported".into(),
        root: None,
        nodes,
        edges: vec![],
        metadata: Value::Null,
    };
    let one = snapshot(vec![imported.nodes[0].clone()]);
    let two = one.clone();
    let composed =
        graf::snapshot::merge(vec![("alpha".into(), one), ("beta".into(), two)]).unwrap();
    imported.nodes.extend(composed.nodes);
    let db = dir.path().join("communities.db");
    Store::create(&db).unwrap().import_graph(imported).unwrap();
    let before = fs::read(&db).unwrap();
    let mut client = Mcp::start(&db, &[]);
    let resource = resource_value(client.resource("graphify://communities"));
    assert_eq!(resource["community_source"], "preserved");
    let communities = resource["communities"].as_array().unwrap();
    assert_eq!(communities.len(), 4);
    assert!(
        communities
            .iter()
            .any(|c| c["id"] == json!(7) && c["project"] == json!([]) && c["nodes"] == 2)
    );
    assert!(
        communities
            .iter()
            .any(|c| c["id"] == json!("7") && c["nodes"] == 1)
    );
    let ambiguous = client.call("get_community", json!({"community_id":7}));
    assert!(failed(&ambiguous), "{ambiguous}");
    let direct = client.call(
        "get_community",
        json!({"community_id":7,"community_project":[]}),
    );
    assert_eq!(value(&direct)["community_id"], json!(7));
    assert_eq!(
        value(&direct)["community_names"],
        json!(["Backend", "Older name"])
    );
    assert_eq!(value(&direct)["total_nodes"], 2);
    let text = client.call("get_community", json!({"community_id":"7"}));
    assert_eq!(value(&text)["community_id"], json!("7"));
    assert_eq!(value(&text)["nodes"][0]["id"], "s");
    for project in ["alpha", "beta"] {
        let response = client.call(
            "get_community",
            json!({"community_id":7,"community_project":[project]}),
        );
        assert_eq!(value(&response)["community_project"], json!([project]));
        assert_eq!(value(&response)["nodes"][0]["metadata"]["project"], project);
    }
    assert_eq!(resource["computed_status"], "not_requested");
    assert!(resource["computed_communities"].is_null());
    let computed_resource = resource_value(client.resource("graf://computed-communities"));
    let computed_id = computed_resource["computed_communities"][0]["id"].clone();
    let computed = client.call(
        "get_community",
        json!({"community_id":computed_id,"community_source":"computed"}),
    );
    assert_eq!(value(&computed)["community_source"], "computed");
    assert!(value(&computed)["cohesion"].is_number());
    for args in [
        json!({"community_id":7,"community_project":["missing"]}),
        json!({"community_id":"7","community_source":"computed"}),
        json!({"community_id":7.5}),
        json!({"community_id":7,"community_project":[],"token_budget":1}),
    ] {
        assert!(failed(&client.call("get_community", args)));
    }
    let limited = client.call(
        "get_community",
        json!({"community_id":7,"community_project":[],"limit":1}),
    );
    assert_eq!(value(&limited)["truncated"], true);
    assert_eq!(fs::read(&db).unwrap(), before);
}

#[cfg(unix)]
fn fixture_pr_backend(dir: &Path) -> Vec<(&'static str, String)> {
    use std::os::unix::fs::PermissionsExt;
    let bin = dir.join("bin");
    fs::create_dir(&bin).unwrap();
    let log = dir.join("gh.log");
    let list_file = dir.join("list.json");
    let view_file = dir.join("view.json");
    let pull = |number, draft, conclusion| {
        json!({"number":number,"title":format!("Change {number}"),
        "state":"OPEN","headRefName":format!("branch-{number}"),"baseRefName":"main",
        "author":{"login":"contributor"},"isDraft":draft,"isCrossRepository":false,
        "reviewDecision":"APPROVED","statusCheckRollup":[{"status":"COMPLETED","conclusion":conclusion,"state":null}],
        "updatedAt":"2026-09-18T00:00:00Z","files":[{"path":"sample.py"}],"changedFiles":1})
    };
    let first = pull(7, false, "FAILURE");
    fs::write(
        &list_file,
        json!([first, pull(8, true, "SUCCESS")]).to_string(),
    )
    .unwrap();
    let mut merged = first.clone();
    merged["state"] = json!("MERGED");
    fs::write(&view_file, merged.to_string()).unwrap();
    let gh = bin.join("gh");
    fs::write(
        &gh,
        r#"#!/bin/sh
printf '%s\n' "$*" >> "$GRAF_TEST_PR_LOG"
case "$1 $2" in
  'repo view')
    [ "$3" = 'github.com/acme/tools' ] || exit 91
    printf '%s\n' '{"defaultBranchRef":{"name":"main"}}' ;;
  'pr list') /bin/cat "$GRAF_TEST_PR_LIST" ;;
  'pr view') [ "$3" = '7' ] || exit 92; /bin/cat "$GRAF_TEST_PR_VIEW" ;;
  *) exit 93 ;;
esac
"#,
    )
    .unwrap();
    fs::set_permissions(&gh, fs::Permissions::from_mode(0o755)).unwrap();
    vec![
        ("PATH", bin.to_string_lossy().into_owned()),
        ("GRAF_TEST_PR_LOG", log.to_string_lossy().into_owned()),
        (
            "GRAF_TEST_PR_LIST",
            list_file.to_string_lossy().into_owned(),
        ),
        (
            "GRAF_TEST_PR_VIEW",
            view_file.to_string_lossy().into_owned(),
        ),
    ]
}

#[cfg(unix)]
#[test]
fn pr_tools_require_opt_in_and_allowlisted_repo_and_use_fixture_backend() {
    let dir = TempDir::new().unwrap();
    let db = fixture(dir.path(), "primary", 3);
    let before = fs::read(&db).unwrap();
    let log = dir.path().join("gh.log");
    let env = fixture_pr_backend(dir.path());
    let mut disabled = Mcp::start_with_env(&db, &[], &env);
    let listing = disabled.request("tools/list", json!({}));
    for name in ["list_prs", "get_pr_impact", "triage_prs"] {
        assert!(
            !listing["result"]["tools"]
                .as_array()
                .unwrap()
                .iter()
                .any(|t| t["name"] == name)
        );
        assert!(failed(&disabled.call(name, json!({"pr_number":7}))));
    }
    assert!(!log.exists());
    let mut enabled =
        Mcp::start_with_env(&db, &["--github-repo".into(), "acme/tools".into()], &env);
    let listing = enabled.request("tools/list", json!({}));
    for name in ["list_prs", "get_pr_impact", "triage_prs"] {
        let tool = listing["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["name"] == name)
            .unwrap();
        assert_eq!(tool["annotations"]["readOnlyHint"], true);
        assert_eq!(tool["annotations"]["openWorldHint"], true);
        assert!(
            tool["inputSchema"]["properties"]
                .get("project_path")
                .is_none()
        );
    }
    assert_eq!(value(&enabled.call("stats", json!({})))["nodes"], 3);
    value(&enabled.call("query_graph", json!({"question":"primary0","depth":0})));
    for args in [
        json!({"repo":"other/repo"}),
        json!({"repo":"https://github.com/acme/tools"}),
        json!({"project_path":"/unregistered"}),
        json!({"project":"unregistered"}),
    ] {
        assert!(failed(&enabled.call("list_prs", args)));
    }
    assert!(failed(
        &enabled.call("get_pr_impact", json!({"pr_number":0}))
    ));
    assert!(
        !log.exists(),
        "startup, discovery, ordinary queries and rejected inputs must not invoke gh"
    );
    let list = enabled.call("list_prs", json!({}));
    assert_eq!(value(&list)["repo"], "acme/tools");
    assert_eq!(value(&list)["entries"].as_array().unwrap().len(), 2);
    assert_eq!(value(&list)["entries"][0]["ci"], "FAILURE");
    let impact = enabled.call("get_pr_impact", json!({"pr_number":7}));
    assert_eq!(value(&impact)["entries"][0]["pr"]["state"], "MERGED");
    assert_eq!(
        value(&impact)["entries"][0]["impact"]["nodes"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    let triage = enabled.call("triage_prs", json!({"base":"main","repo":"ACME/TOOLS"}));
    assert_eq!(value(&triage)["overlaps"][0]["prs"], json!([7, 8]));
    let calls = fs::read_to_string(&log).unwrap();
    assert_eq!(calls.lines().count(), 5);
    assert!(
        calls
            .lines()
            .all(|line| line.contains("github.com/acme/tools")),
        "{calls}"
    );
    assert_eq!(fs::read(&db).unwrap(), before);
}

#[test]
fn graphify_aliases_share_unicode_lookup_and_relation_ambiguity_over_stdio() {
    let dir = TempDir::new().unwrap();
    let input = dir.path().join("lookup.json");
    fs::write(
        &input,
        json!({"directed":true,"multigraph":true,
        "nodes":[
            {"id":"root","label":"ﬂow()","source_file":"a.rs"},
            {"id":"later","label":"FlowHistory","source_file":"a.rs"},
            {"id":"policy","label":"RetentionPolicy","source_file":"b.rs"},
            {"id":"twin-a","label":"Shared","source_file":"same.rs"},
            {"id":"twin-b","label":"Shared","source_file":"same.rs"},
            {"id":"literal","label":"Markup_%Code","source_file":"c.rs"},
            {"id":"ExactKey","label":"Unrelated","source_file":"c.rs"},
            {"id":"decoy","label":"ExactKey","source_file":"d.rs"}
        ],"links":[
            {"source":"root","target":"later","key":"one","relation":"calls"},
            {"source":"root","target":"policy","key":"two","relation":"calls_async"},
            {"source":"policy","target":"root","key":"three","relation":"references"}
        ]})
        .to_string(),
    )
    .unwrap();
    let db = dir.path().join("lookup.db");
    Store::create(&db)
        .unwrap()
        .import_graph(read_graphify(&input).unwrap())
        .unwrap();
    let before = fs::read(&db).unwrap();
    let mut client = Mcp::start(&db, &[]);
    let listing = client.request("tools/list", json!({}));
    for tool in ["get_node", "get_neighbors"] {
        let schema = &listing["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["name"] == tool)
            .unwrap()["inputSchema"];
        assert!(
            schema["required"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v == "label")
        );
        for (label, expected) in [
            ("flow", "root"),
            ("ＦＬＯＷ", "root"),
            ("retent", "policy"),
            ("olicy", "policy"),
            ("_%", "literal"),
            ("ExactKey", "ExactKey"),
        ] {
            let response = client.call(tool, json!({"label":label}));
            assert_eq!(value(&response)["seeds"][0], expected, "{tool}: {label}");
        }
        for label in ["Shared", "missing.rs::flow"] {
            let response = client.call(tool, json!({"label":label}));
            assert_eq!(response["result"]["isError"], true, "{response}");
        }
    }
    let node_error = client.call("get_node", json!({"label":"Shared"}));
    let neighbor_error = client.call("get_neighbors", json!({"label":"Shared", "limit":1}));
    assert_eq!(
        node_error["result"]["content"],
        neighbor_error["result"]["content"]
    );
    assert!(
        node_error["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("twin-a, twin-b")
    );
    for (filter, expected) in [
        ("calls", "calls"),
        ("ＣＡＬＬＳ", "calls"),
        ("async", "calls_async"),
        ("erenc", "references"),
    ] {
        let response = client.call(
            "get_neighbors",
            json!({"label":"flow","relation_filter":filter}),
        );
        let edges = value(&response)["graph"]["edges"].as_array().unwrap();
        assert_eq!(edges.len(), 1, "{filter}");
        assert_eq!(edges[0]["relation"], expected);
        if expected == "references" {
            assert_eq!(edges[0]["source"], "policy");
            assert_eq!(edges[0]["target"], "root");
        }
    }
    let ambiguous = client.call(
        "get_neighbors",
        json!({"label":"flow","relation_filter":"call"}),
    );
    assert_eq!(ambiguous["result"]["isError"], true);
    assert!(
        ambiguous["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("calls, calls_async")
    );
    let absent = client.call(
        "get_neighbors",
        json!({"label":"flow","relation_filter":"nonexistent"}),
    );
    assert!(
        value(&absent)["graph"]["edges"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let limited = client.call("get_neighbors", json!({"label":"flow","limit":1}));
    assert_eq!(value(&limited)["graph"]["truncated"], true);
    let tiny = client.call("get_neighbors", json!({"label":"flow","token_budget":1}));
    assert_eq!(tiny["result"]["isError"], true);
    assert_eq!(fs::read(&db).unwrap(), before);
}

#[test]
fn query_graph_ranks_late_records_stably_after_import_reordering() {
    let dir = TempDir::new().unwrap();
    let input = dir.path().join("rank.json");
    let mut nodes: Vec<_> = (0..100).flat_map(|i| [
        json!({"id":format!("a{i:03}"),"label":format!("CopperNoise{i:03}"),"source_file":"a.rs"}),
        json!({"id":format!("b{i:03}"),"label":format!("ZincNoise{i:03}"),"source_file":"a.rs"}),
    ]).collect();
    nodes.push(json!({"id":"winner","label":"Copper Zinc Planner","source_file":"b.rs"}));
    let write = |nodes: &[Value]| {
        fs::write(
            &input,
            json!({"directed":true,"multigraph":false,"nodes":nodes,"links":[]}).to_string(),
        )
        .unwrap()
    };
    write(&nodes);
    let db = dir.path().join("rank.db");
    Store::create(&db)
        .unwrap()
        .import_graph(read_graphify(&input).unwrap())
        .unwrap();
    let mut client = Mcp::start(&db, &[]);
    let first = client.call("query_graph", json!({"question":"Copper Zinc","depth":0}));
    assert_eq!(value(&first)["seeds"][0], "winner");
    nodes.reverse();
    write(&nodes);
    Store::open(&db)
        .unwrap()
        .refresh_import(read_graphify(&input).unwrap())
        .unwrap();
    let second = client.call("query_graph", json!({"question":"Copper Zinc","depth":0}));
    assert_eq!(value(&second)["seeds"], value(&first)["seeds"]);
    assert_eq!(
        value(&second)["graph"]["nodes"],
        value(&first)["graph"]["nodes"]
    );
    assert_eq!(value(&first)["graph"]["generation"], 1);
    assert_eq!(value(&second)["graph"]["generation"], 2);
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
