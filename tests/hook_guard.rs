use graf::{
    hook_guard::{self, Host, INPUT_LIMIT},
    model::{Coverage, FileFacts},
    store::Store,
};
use serde_json::{Value, json};
use std::{
    fs,
    io::{self, Read, Write},
    path::PathBuf,
    process::{Command, Stdio},
};
use tempfile::{TempDir, tempdir};

struct Fixture {
    _temp: TempDir,
    root: PathBuf,
    db: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempdir().unwrap();
        let root = temp.path().join(if cfg!(windows) {
            "project with spaces"
        } else {
            "project #with? spaces"
        });
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/app.py"), "def entry(): pass\n").unwrap();
        fs::write(root.join("notes.txt"), "notes\n").unwrap();
        let db = root.join(".graf/index.db");
        let mut store = Store::create(&db).unwrap();
        store
            .apply_native(
                root.canonicalize().unwrap().to_str().unwrap(),
                vec![facts("src/app.py")],
                vec![],
                Coverage::default(),
            )
            .unwrap();
        drop(store);
        Self {
            _temp: temp,
            root,
            db,
        }
    }
    fn run(&self, host: Host, tool: &str, input: Value) -> Option<Value> {
        let payload = json!({"tool_name":tool,"tool_input":input,"cwd":self.root});
        hook_guard::run(host, Some(&self.root), None, payload.to_string().as_bytes())
    }
}
fn facts(path: &str) -> FileFacts {
    FileFacts {
        path: path.into(),
        hash: "fixture".into(),
        module: path.into(),
        nodes: vec![],
        edges: vec![],
        references: vec![],
        diagnostics: vec![],
    }
}
fn guidance(value: Option<Value>) -> Value {
    let value = value.expect("relevant indexed source should receive context");
    let output = &value["hookSpecificOutput"];
    assert_eq!(output["hookEventName"], "PreToolUse");
    assert!(
        output["additionalContext"]
            .as_str()
            .unwrap()
            .contains("graf query")
    );
    assert!(output.get("permissionDecision").is_none());
    value
}

#[test]
fn ordinary_native_tools_offer_navigation_without_deciding_permission() {
    let f = Fixture::new();
    for host in [Host::Claude, Host::Codebuddy] {
        for (tool, input) in [
            ("Read", json!({"file_path":"src/app.py"})),
            ("Read", json!({"file_path":f.root.join("src/app.py")})),
            ("Glob", json!({"pattern":"**/*.py"})),
            ("Glob", json!({"pattern":"*.py","path":"src"})),
            ("Grep", json!({"pattern":"entry","path":"src"})),
            ("Grep", json!({"pattern":"entry","glob":"**/*.py"})),
        ] {
            guidance(f.run(host, tool, input));
        }
    }
    let gemini = f
        .run(Host::Gemini, "read_file", json!({"file_path":"src/app.py"}))
        .unwrap();
    assert_eq!(gemini["decision"], "allow");
    assert!(
        gemini["additionalContext"]
            .as_str()
            .unwrap()
            .contains("graf query")
    );
    assert!(
        f.run(Host::Gemini, "list_directory", json!({"path":"src"}))
            .unwrap()
            .get("additionalContext")
            .is_some()
    );
}

#[test]
fn shell_ast_recognizes_executables_instead_of_search_words_in_prose() {
    let f = Fixture::new();
    for command in [
        "rg entry",
        "grep -rn entry .",
        "/usr/bin/grep -n entry src/app.py",
        "'rg' 'entry' src",
        "FOO=1 rg entry src",
        "command rg entry src",
        "env FOO=bar rg entry src",
        "git grep entry",
        "git -C src grep entry",
        "find . -name '*.py'",
        "fd app src",
        "cat notes.txt | grep entry",
        "echo done; rg entry",
        "result=$(grep entry src/app.py)",
        "cat <<'EOF'\njust notes\nEOF\nrg entry src",
    ] {
        guidance(f.run(Host::Claude, "Bash", json!({"command":command})));
    }
    for command in [
        "git commit -m 'add flag support'",
        "gh pr create --body 'you can find it here'",
        "echo 'rg entry src'",
        "printf 'use grep here'",
        "pgrep -f entry",
        "cargo build",
        "git show grep",
        "python -c 'import os; print(\"rg x\")'",
        "cat <<'EOF'\nrg entry src\nfind . -name '*.py'\nEOF",
        "cat <<'EOF'\nrg entry src\n",
        "function f() { rg entry src; }",
        "rg 'unterminated",
        "echo rg",
        "echo '$(rg entry src)'",
        "rg",
        "git -C / grep entry",
        "rg entry /",
        "grep entry ../other.py",
        "cd /; rg entry",
        "rg entry $TARGET",
        "rg entry *.py",
        "eval 'rg entry'",
        "command -v rg entry",
        "env --help rg entry",
    ] {
        assert_eq!(
            f.run(
                Host::Claude,
                "Bash",
                json!({"command":command, "pattern":"stray"})
            ),
            None,
            "{command}"
        );
    }
}

#[test]
fn scope_requires_indexed_source_and_stays_inside_project() {
    let f = Fixture::new();
    fs::write(f.root.join("src/unindexed.py"), "pass").unwrap();
    fs::write(f._temp.path().join("outside.py"), "pass").unwrap();
    for (tool, input) in [
        ("Read", json!({"file_path":"notes.txt"})),
        ("Read", json!({"file_path":"src/unindexed.py"})),
        ("Read", json!({"file_path":"src/missing.py"})),
        ("Read", json!({"file_path":"../outside.py"})),
        (
            "Read",
            json!({"file_path":f._temp.path().join("outside.py")}),
        ),
        ("Read", json!({"file_path":".graf/index.db"})),
        ("Read", json!({"file_path":"C:outside.py"})),
        ("Glob", json!({"pattern":"**/*.rs"})),
        ("Glob", json!({"pattern":"../*.py"})),
        ("Grep", json!({"pattern":"entry","path":".."})),
        ("Grep", json!({"pattern":"entry","path":123})),
        (
            "Bash",
            json!({"command":"echo 'grep entry'","pattern":"entry"}),
        ),
        ("Write", json!({"file_path":"src/app.py"})),
    ] {
        assert_eq!(f.run(Host::Claude, tool, input), None, "{tool}");
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(f._temp.path().join("outside.py"), f.root.join("escape.py"))
            .unwrap();
        assert_eq!(
            f.run(Host::Claude, "Read", json!({"file_path":"escape.py"})),
            None
        );
    }
}

#[test]
fn malformed_or_failed_input_always_allows_gemini_and_never_creates_index() {
    let temp = tempdir().unwrap();
    for raw in [
        b"".as_slice(),
        b"{",
        b"[]",
        b"null",
        b"\xff",
        br#"{"tool_name":"Read","tool_input":[]} "#,
    ] {
        assert_eq!(
            hook_guard::run(Host::Claude, Some(temp.path()), None, raw),
            None
        );
        assert_eq!(
            hook_guard::run(Host::Gemini, Some(temp.path()), None, raw),
            Some(json!({"decision":"allow"}))
        );
    }
    struct Broken;
    impl Read for Broken {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::other("fixture failure"))
        }
    }
    assert_eq!(
        hook_guard::run(Host::Gemini, Some(temp.path()), None, Broken),
        Some(json!({"decision":"allow"}))
    );
    struct Counted(usize);
    impl Read for Counted {
        fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
            bytes.fill(b' ');
            self.0 += bytes.len();
            Ok(bytes.len())
        }
    }
    let mut input = Counted(0);
    assert_eq!(
        hook_guard::run(Host::Claude, Some(temp.path()), None, &mut input),
        None
    );
    assert_eq!(input.0, INPUT_LIMIT as usize + 1);
    assert!(!temp.path().join(".graf").exists());
}

#[test]
fn database_failures_and_foreign_roots_are_silent() {
    let f = Fixture::new();
    let event =
        json!({"tool_name":"read_file","tool_input":{"file_path":"src/app.py"},"cwd":f.root})
            .to_string();
    for data in [b"not SQLite".as_slice(), b""] {
        let db = f.root.join("bad.db");
        fs::write(&db, data).unwrap();
        assert_eq!(
            hook_guard::run(Host::Gemini, Some(&f.root), Some(&db), event.as_bytes()),
            Some(json!({"decision":"allow"}))
        );
    }
    let other = Fixture::new();
    assert_eq!(
        hook_guard::run(
            Host::Gemini,
            Some(&f.root),
            Some(&other.db),
            event.as_bytes()
        ),
        Some(json!({"decision":"allow"}))
    );
    let no_index = Fixture::new();
    fs::remove_file(&no_index.db).unwrap();
    assert_eq!(
        no_index.run(Host::Claude, "Read", json!({"file_path":"src/app.py"})),
        None
    );
    assert!(!no_index.db.exists());
    let conn = rusqlite::Connection::open(&f.db).unwrap();
    conn.pragma_update(None, "user_version", 999).unwrap();
    assert_eq!(
        f.run(Host::Claude, "Read", json!({"file_path":"src/app.py"})),
        None
    );
}

#[test]
fn reads_committed_wal_and_keeps_stale_source_access_unconditional() {
    let f = Fixture::new();
    let mut writer = Store::open(&f.db).unwrap();
    fs::write(f.root.join("src/new.py"), "def new(): pass\n").unwrap();
    writer
        .apply_native(
            f.root.canonicalize().unwrap().to_str().unwrap(),
            vec![facts("src/new.py")],
            vec![],
            Coverage::default(),
        )
        .unwrap();
    assert!(
        fs::metadata(f.db.with_file_name("index.db-wal"))
            .unwrap()
            .len()
            > 0
    );
    guidance(f.run(Host::Claude, "Read", json!({"file_path":"src/new.py"})));
    let before = writer.stats().unwrap().generation;
    // Deliberately change the source after indexing. Guidance cannot gate access
    // on graph freshness and must not create a once-per-session denial marker.
    fs::write(f.root.join("src/app.py"), "changed after indexing\n").unwrap();
    for _ in 0..2 {
        let value = guidance(f.run(Host::Claude, "Read", json!({"file_path":"src/app.py"})));
        assert!(
            value["hookSpecificOutput"]["additionalContext"]
                .as_str()
                .unwrap()
                .contains("freshness was not checked")
        );
        assert_eq!(
            fs::read_to_string(f.root.join("src/app.py")).unwrap(),
            "changed after indexing\n"
        );
    }
    assert_eq!(writer.stats().unwrap().generation, before);
    assert!(!f.root.join(".graf/cache").exists());
    assert!(!f.root.join(".graf/setup").exists());
}

#[test]
fn event_cwd_selects_project_without_recursive_discovery_and_cli_stays_protocol_only() {
    let f = Fixture::new();
    let event = json!({"tool_name":"Read","tool_input":{"file_path":"src/app.py"},"cwd":f.root})
        .to_string();
    guidance(hook_guard::run(Host::Claude, None, None, event.as_bytes()));
    let child_event =
        json!({"tool_name":"Read","tool_input":{"file_path":"app.py"},"cwd":f.root.join("src")})
            .to_string();
    assert_eq!(
        hook_guard::run(Host::Claude, None, None, child_event.as_bytes()),
        None
    );
    for (host, input) in [
        ("claude", event.as_bytes()),
        ("gemini", b"malformed".as_slice()),
    ] {
        let mut child = Command::new(env!("CARGO_BIN_EXE_graf"))
            .args(["hook-guard", "--platform", host, "--project"])
            .arg(&f.root)
            .current_dir(&f.root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(input).unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success());
        assert!(out.stderr.is_empty());
        let value: Value = serde_json::from_slice(&out.stdout).unwrap();
        if host == "gemini" {
            assert_eq!(value, json!({"decision":"allow"}));
        } else {
            guidance(Some(value));
        }
    }
}

#[test]
fn exclusive_database_contention_fails_open_promptly_then_guidance_resumes() {
    let f = Fixture::new();
    let blocker = rusqlite::Connection::open(&f.db).unwrap();
    // Use rollback-journal mode so EXCLUSIVE genuinely excludes readers. The
    // committed-WAL test separately protects the ordinary concurrent-writer case.
    let mode: String = blocker
        .query_row("PRAGMA journal_mode=DELETE", [], |r| r.get(0))
        .unwrap();
    assert_eq!(mode, "delete");
    blocker
        .execute_batch("PRAGMA locking_mode=EXCLUSIVE; BEGIN EXCLUSIVE")
        .unwrap();
    for (host, tool) in [
        (Host::Claude, "Read"),
        (Host::Codebuddy, "Read"),
        (Host::Gemini, "read_file"),
    ] {
        let started = std::time::Instant::now();
        let result = f.run(host, tool, json!({"file_path":"src/app.py"}));
        assert!(
            started.elapsed() < std::time::Duration::from_secs(1),
            "hook waited on an exclusive database lock"
        );
        let expected = (host == Host::Gemini).then(|| json!({"decision":"allow"}));
        assert_eq!(result, expected);
    }
    blocker.execute_batch("ROLLBACK").unwrap();
    drop(blocker); // exclusive locking mode retains the lock until disconnect
    guidance(f.run(Host::Claude, "Read", json!({"file_path":"src/app.py"})));
    let gemini = f
        .run(Host::Gemini, "read_file", json!({"file_path":"src/app.py"}))
        .unwrap();
    assert_eq!(gemini["decision"], "allow");
    assert!(gemini.get("additionalContext").is_some());
}
