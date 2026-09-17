#![cfg(unix)]

use graf::{index, store::Store};
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::Path,
    process::{Command, Output},
    time::Instant,
};
use tempfile::{TempDir, tempdir};

const PASSWORD: &str = "fixture-password@;+";
const DSN: &str = "postgresql://fixture-user:fixture-password%40%3B%2B@db.invalid:5440/fixture%20database?sslmode=verify-full";
const SCHEMA: &str = r#"
CREATE TABLE public.parent (a integer, b integer, PRIMARY KEY (a,b));
CREATE TABLE public.child (a integer, b integer);
ALTER TABLE ONLY public.child ADD CONSTRAINT child_fk FOREIGN KEY (a,b) REFERENCES public.parent(a,b);
CREATE VIEW public.child_view AS SELECT a FROM public.child;
CREATE FUNCTION public.lookup() RETURNS integer LANGUAGE sql AS $$ SELECT a FROM public.parent; $$;
CREATE PROCEDURE public.refresh() LANGUAGE SQL AS $$ SELECT a FROM public.child; $$;
"#;

struct Fixture {
    root: TempDir,
    bin: TempDir,
    log: std::path::PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let root = tempdir().unwrap();
        let bin = tempdir().unwrap();
        let log = bin.path().join("calls.jsonl");
        let mock = r#"#!/usr/bin/python3
import os, sys, json, time
from pathlib import Path
name = Path(sys.argv[0]).name
log = Path(os.environ['MOCK_LOG'])
prior = log.read_text().count('\n') if log.exists() else 0
request = sys.stdin.read()
keys = ['PGHOST','PGPORT','PGDATABASE','PGUSER','PGPASSWORD','PGSSLMODE','PGSERVICE','PGHOSTADDR','NEO4J_ADDRESS','NEO4J_URI','NEO4J_USERNAME','NEO4J_PASSWORD','NEO4J_DATABASE','REDISCLI_AUTH']
with log.open('a') as f:
    f.write(json.dumps({'tool':name, 'args':sys.argv[1:], 'stdin':request, 'env':{k:os.environ.get(k) for k in keys}})+'\n')
mode = os.environ.get('MOCK_MODE','success')
if mode == 'sleep': time.sleep(30)
if mode == 'slow': time.sleep(0.6)
if mode == 'overflow':
    sys.stdout.write('x'*4096)
    sys.exit(0)
if mode == 'stderr-overflow':
    sys.stderr.write('x'*4096)
    sys.exit(0)
if mode == 'fail' or (mode == 'second-fails' and prior == 1):
    sys.stderr.write(os.environ.get('TEST_DSN','')+' '+os.environ.get('TEST_PASSWORD',''))
    sys.exit(7)
if name == 'pg_dump': sys.stdout.write(os.environ['MOCK_SCHEMA'])
else: print('acknowledged')
"#;
        for name in ["pg_dump", "cypher-shell", "redis-cli"] {
            let path = bin.path().join(name);
            fs::write(&path, mock).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
        }
        Self { root, bin, log }
    }
    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_graf"));
        cmd.env_clear()
            .current_dir(self.root.path())
            .args(args)
            .arg("--json")
            .env("PATH", self.bin.path())
            .env("MOCK_LOG", &self.log)
            .env("MOCK_SCHEMA", SCHEMA)
            .env("TEST_DSN", DSN)
            .env("TEST_USER", "fixture-user")
            .env("TEST_PASSWORD", PASSWORD)
            .env("NEO_URI", "bolt://neo.invalid:7687")
            .env("FALKOR_URI", "redis://redis.invalid:6379")
            .env_remove("PGSERVICE")
            .env_remove("PGHOSTADDR");
        cmd
    }
    fn output(&self, args: &[&str]) -> Output {
        self.command(args).output().unwrap()
    }
    fn calls(&self) -> Vec<Value> {
        fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .map(|s| serde_json::from_str(s).unwrap())
            .collect()
    }
    fn graph(&self) -> std::path::PathBuf {
        fs::write(
            self.root.path().join("main.py"),
            "def parent():\n    pass\ndef child():\n    parent()\n",
        )
        .unwrap();
        let db = self.root.path().join(".graf/index.db");
        index::run(self.root.path(), &db).unwrap();
        db
    }
}

fn success(out: Output) -> Value {
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}
fn failure(out: Output, text: &str) {
    assert!(!out.status.success());
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(combined.contains(text), "{combined}");
    assert!(
        !combined.contains(PASSWORD),
        "credential escaped into output"
    );
    assert!(!combined.contains(DSN), "DSN escaped into output");
}
fn cache(root: &Path) -> Vec<u8> {
    let path = fs::read_dir(root.join(".graf/sources"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    fs::read(path).unwrap()
}
fn postgres_args() -> Vec<&'static str> {
    vec![
        "introspect",
        "postgres",
        "--name",
        "catalog",
        "--dsn-env",
        "TEST_DSN",
    ]
}
fn neo_args() -> Vec<&'static str> {
    vec![
        "push",
        "neo4j",
        "--uri-env",
        "NEO_URI",
        "--user-env",
        "TEST_USER",
        "--password-env",
        "TEST_PASSWORD",
        "--database",
        "catalog",
    ]
}
fn falkor_args() -> Vec<&'static str> {
    vec![
        "push",
        "falkordb",
        "--uri-env",
        "FALKOR_URI",
        "--password-env",
        "TEST_PASSWORD",
        "--graph",
        "catalog",
    ]
}

#[test]
fn postgres_caches_schema_facts_with_credentials_only_in_environment() {
    let f = Fixture::new();
    let out = success(
        f.command(&postgres_args())
            .env("PGSERVICE", "must-not-route")
            .env("PGHOSTADDR", "192.0.2.2")
            .output()
            .unwrap(),
    );
    assert_eq!(out["source"], "postgres:catalog");
    assert_eq!(out["indexed"], false);
    assert!(!f.root.path().join(".graf/index.db").exists());
    let calls = f.calls();
    assert_eq!(calls.len(), 1);
    let args = calls[0]["args"].as_array().unwrap();
    for flag in [
        "--schema-only",
        "--serializable-deferrable",
        "--no-password",
        "--no-subscriptions",
    ] {
        assert!(args.contains(&json!(flag)));
    }
    assert!(!calls[0]["args"].to_string().contains("fixture-"));
    assert_eq!(calls[0]["env"]["PGDATABASE"], "fixture database");
    assert_eq!(calls[0]["env"]["PGPASSWORD"], PASSWORD);
    assert_eq!(calls[0]["env"]["PGHOST"], "db.invalid");
    assert_eq!(calls[0]["env"]["PGPORT"], "5440");
    assert_eq!(calls[0]["env"]["PGSSLMODE"], "verify-full");
    assert!(calls[0]["env"]["PGSERVICE"].is_null());
    assert!(calls[0]["env"]["PGHOSTADDR"].is_null());
    let bytes = cache(f.root.path());
    let saved = String::from_utf8(bytes.clone()).unwrap();
    for private in [PASSWORD, DSN, "fixture-user", "db.invalid"] {
        assert!(!saved.contains(private));
    }
    let record: Value = serde_json::from_slice(&bytes).unwrap();
    for (label, kind) in [
        ("public.parent", "table"),
        ("public.child_view", "view"),
        ("public.lookup", "function"),
        ("public.refresh", "procedure"),
    ] {
        assert!(
            record["facts"]["nodes"]
                .as_array()
                .unwrap()
                .iter()
                .any(|n| n["label"] == label && n["kind"] == kind),
            "missing {label}: {saved}"
        );
    }
    let db = f.root.path().join(".graf/index.db");
    index::run(f.root.path(), &db).unwrap();
    let snapshot = Store::open_read_only(&db).unwrap().snapshot().unwrap();
    assert!(snapshot.edges.iter().any(|e| e.relation == "references"));
    assert_eq!(f.calls().len(), 1, "index must reuse cache without network");
    success(f.output(&["stats"]));
    success(f.output(&["update"]));
    assert_eq!(f.calls().len(), 1, "reads/updates must not reconnect");
}

#[test]
fn postgres_failure_retains_cache_and_bounds_both_output_streams() {
    let f = Fixture::new();
    success(f.output(&postgres_args()));
    let before = cache(f.root.path());
    for (mode, text) in [
        ("fail", "pg_dump exited unsuccessfully"),
        ("overflow", "output exceeds byte limit"),
        ("stderr-overflow", "output exceeds byte limit"),
    ] {
        let mut args = postgres_args();
        args.extend(["--max-output-bytes", "1024"]);
        failure(
            f.command(&args).env("MOCK_MODE", mode).output().unwrap(),
            text,
        );
        assert_eq!(cache(f.root.path()), before);
    }
}

#[test]
fn postgres_standard_environment_and_invalid_dsn_are_explicit() {
    let f = Fixture::new();
    success(
        f.command(&["introspect", "postgres", "--name", "local"])
            .env("PGDATABASE", "environment_db")
            .env("PGUSER", "environment_user")
            .output()
            .unwrap(),
    );
    assert_eq!(f.calls()[0]["env"]["PGDATABASE"], "environment_db");
    for value in [
        "not-a-url",
        "postgresql://user:password@db.invalid/db?unsupported=secret",
        "postgresql://user:%xx@db.invalid/db",
    ] {
        failure(
            f.command(&postgres_args())
                .env("TEST_DSN", value)
                .output()
                .unwrap(),
            "URL",
        );
    }
    assert_eq!(f.calls().len(), 1);
    failure(
        f.command(&postgres_args())
            .env_remove("TEST_DSN")
            .output()
            .unwrap(),
        "environment variable",
    );
}

#[test]
fn neo4j_uses_one_transaction_and_preserves_source_database() {
    let f = Fixture::new();
    let db = f.graph();
    let before = fs::read(&db).unwrap();
    let report = success(f.output(&neo_args()));
    assert_eq!(report["atomic"], true);
    let calls = f.calls();
    assert_eq!(calls.len(), 1);
    assert!(
        calls[0]["args"]
            .as_array()
            .unwrap()
            .contains(&json!("--fail-fast"))
    );
    assert!(!calls[0]["args"].to_string().contains(PASSWORD));
    assert_eq!(calls[0]["env"]["NEO4J_PASSWORD"], PASSWORD);
    assert_eq!(calls[0]["env"]["NEO4J_USERNAME"], "fixture-user");
    assert_eq!(calls[0]["env"]["NEO4J_DATABASE"], "catalog");
    let script = calls[0]["stdin"].as_str().unwrap();
    assert!(script.starts_with(":begin\nMERGE"));
    assert!(script.ends_with(";\n:commit\n"));
    assert_eq!(script.matches(":commit").count(), 1);
    assert_eq!(fs::read(&db).unwrap(), before);
}

#[test]
fn neo4j_failure_does_not_claim_rollback_after_lost_commit_acknowledgment() {
    let f = Fixture::new();
    let db = f.graph();
    let before = fs::read(&db).unwrap();
    failure(
        f.command(&neo_args())
            .env("MOCK_MODE", "fail")
            .output()
            .unwrap(),
        "commit acknowledgment may be lost",
    );
    assert_eq!(f.calls().len(), 1, "must not retry an ambiguous commit");
    assert_eq!(fs::read(db).unwrap(), before);
}

#[test]
fn falkordb_preserves_semicolons_and_reports_partial_progress() {
    let f = Fixture::new();
    let db = f.root.path().join(".graf/index.db");
    let graph = graf::model::ImportedGraph {
        metadata: json!({}),
        edges: vec![],
        nodes: vec![graf::model::Node {
            id: "n;1".into(),
            label: "quote'; MATCH (x) DETACH DELETE x; //\n☃".into(),
            kind: "symbol".into(),
            file: "a;b.py".into(),
            line: None,
            end_line: None,
            qualified_name: None,
            binding_key: None,
            metadata: json!({"text":"value;another"}),
        }],
    };
    Store::create(&db).unwrap().import_graph(graph).unwrap();
    let before = fs::read(&db).unwrap();
    let report = success(f.output(&falkor_args()));
    assert_eq!(report["atomic"], false);
    let calls = f.calls();
    assert_eq!(calls.len(), 2, "one header and one intact node statement");
    assert_eq!(calls[1]["env"]["REDISCLI_AUTH"], PASSWORD);
    let argv = calls[1]["args"].as_array().unwrap();
    assert_eq!(
        &argv[argv.len() - 3..],
        &[json!("-x"), json!("GRAPH.QUERY"), json!("catalog")]
    );
    assert!(argv.contains(&json!("-e")));
    assert!(!calls[1]["args"].to_string().contains(PASSWORD));
    assert!(
        calls[1]["stdin"]
            .as_str()
            .unwrap()
            .contains("value;another")
    );
    assert!(!calls[1]["stdin"].as_str().unwrap().ends_with(';'));
    assert_eq!(fs::read(&db).unwrap(), before);
    fs::remove_file(&f.log).unwrap();
    failure(
        f.command(&falkor_args())
            .env("MOCK_MODE", "second-fails")
            .output()
            .unwrap(),
        "after 1 confirmed statements of 2",
    );
    assert_eq!(f.calls().len(), 2);
    assert_eq!(fs::read(db).unwrap(), before);
}

#[test]
fn falkordb_anonymous_mode_drops_inherited_password_and_supports_tls() {
    let f = Fixture::new();
    f.graph();
    let args = [
        "push",
        "falkordb",
        "--uri-env",
        "FALKOR_URI",
        "--graph",
        "catalog",
    ];
    success(
        f.command(&args)
            .env("REDISCLI_AUTH", "must-not-inherit")
            .env("FALKOR_URI", "rediss://redis.invalid:6380")
            .output()
            .unwrap(),
    );
    for call in f.calls() {
        assert!(call["env"]["REDISCLI_AUTH"].is_null());
        assert!(call["args"].as_array().unwrap().contains(&json!("--tls")));
    }
}

#[test]
fn connection_errors_do_not_echo_credentials_or_run_clients() {
    let f = Fixture::new();
    f.graph();
    failure(
        f.command(&neo_args())
            .env("NEO_URI", "bolt://user:fixture-password@db.invalid")
            .output()
            .unwrap(),
        "must not contain credentials",
    );
    failure(
        f.command(&falkor_args())
            .env("FALKOR_URI", "redis://user:fixture-password@db.invalid")
            .output()
            .unwrap(),
        "must not contain credentials",
    );
    use std::{ffi::OsString, os::unix::ffi::OsStringExt};
    failure(
        f.command(&neo_args())
            .env("TEST_PASSWORD", OsString::from_vec(vec![255, 254]))
            .output()
            .unwrap(),
        "environment variable",
    );
    assert!(f.calls().is_empty());
}

#[test]
fn deadline_bounds_hung_clients_and_the_entire_falkordb_push() {
    let f = Fixture::new();
    let mut args = postgres_args();
    args.extend(["--timeout-secs", "1"]);
    let start = Instant::now();
    failure(
        f.command(&args).env("MOCK_MODE", "sleep").output().unwrap(),
        "deadline exceeded",
    );
    assert!(start.elapsed().as_secs() < 5);
    assert!(!f.root.path().join(".graf/sources").exists());
    fs::remove_file(&f.log).unwrap();
    f.graph();
    let mut args = falkor_args();
    args.extend(["--timeout-secs", "1"]);
    let start = Instant::now();
    failure(
        f.command(&args).env("MOCK_MODE", "slow").output().unwrap(),
        "after 1 confirmed statements",
    );
    assert!(start.elapsed().as_secs() < 5);
    assert_eq!(f.calls().len(), 2);
}

#[test]
fn missing_client_is_actionable_and_does_not_modify_the_source_database() {
    let f = Fixture::new();
    let db = f.graph();
    let before = fs::read(&db).unwrap();
    fs::remove_file(f.bin.path().join("cypher-shell")).unwrap();
    failure(
        f.output(&neo_args()),
        "cannot start cypher-shell; install the client",
    );
    assert!(f.calls().is_empty());
    assert_eq!(fs::read(db).unwrap(), before);
}
