use graf::{index, ingest::IngestOptions, sources, store::Store};
use std::{fs, process::Command};
use tempfile::tempdir;

#[test]
fn managed_sources_survive_original_removal_and_update_without_conversion() {
    let root = tempdir().unwrap();
    let incoming = tempdir().unwrap();
    let source = incoming.path().join("notes.md");
    fs::write(&source, "# Architecture\nA durable imported document.\n").unwrap();
    let record = sources::add(
        root.path(),
        source.to_str().unwrap(),
        None,
        &IngestOptions::default(),
    )
    .unwrap();
    fs::remove_file(source).unwrap();
    let db = root.path().join(".graf/index.db");
    let first = index::run(root.path(), &db).unwrap();
    assert!(first.nodes > 0);
    assert!(
        Store::open(&db)
            .unwrap()
            .file_stamps()
            .unwrap()
            .iter()
            .any(|s| s.path == record.facts.path)
    );
    let before = fs::read(&db).unwrap();
    assert!(index::check_update(root.path(), &db).unwrap().fresh);
    assert_eq!(fs::read(&db).unwrap(), before);
    let second = index::run(root.path(), &db).unwrap();
    assert_eq!(second.generation, first.generation);
    assert_eq!(second.unchanged_files, 1);
    assert_eq!(fs::read(&db).unwrap(), before);
    let cache = fs::read_dir(root.path().join(".graf/sources"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    fs::write(cache, "{broken").unwrap();
    assert!(index::run(root.path(), &db).is_err());
    assert_eq!(
        Store::open(&db).unwrap().stats().unwrap().generation,
        first.generation
    );
}

fn cli(root: &std::path::Path, args: &[&str]) -> serde_json::Value {
    let out = Command::new(env!("CARGO_BIN_EXE_graf"))
        .current_dir(root)
        .args(args)
        .arg("--json")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

#[test]
fn cache_commands_work_without_a_database_and_do_not_print_cached_content() {
    let root = tempdir().unwrap();
    let cache = root.path().join("cache");
    fs::create_dir(&cache).unwrap();
    let valid = "a".repeat(64);
    let invalid = "b".repeat(64);
    fs::write(cache.join(format!("{valid}.json")), r#"{"split_at":3}"#).unwrap();
    let bad = cache.join(format!("{invalid}.json"));
    fs::write(&bad, "{broken SYNTHETIC_CACHED_TEXT").unwrap();
    let report = cli(root.path(), &["cache", "inspect", "cache"]);
    assert_eq!(report["entries"].as_array().unwrap().len(), 2);
    assert!(!report.to_string().contains("SYNTHETIC_CACHED_TEXT"));
    assert!(bad.exists());
    let removed = cli(root.path(), &["cache", "remove", "cache", &invalid]);
    assert_eq!(removed["removed"], true);
    assert!(!bad.exists());
    assert!(cache.join(format!("{valid}.json")).exists());
    assert!(!root.path().join(".graf").exists());
    let rejected = Command::new(env!("CARGO_BIN_EXE_graf"))
        .current_dir(root.path())
        .args(["cache", "remove", "cache", "../outside", "--json"])
        .output()
        .unwrap();
    assert!(!rejected.status.success());
}

#[test]
fn extraction_timings_are_explicit_measured_and_do_not_change_stored_options() {
    let root = tempdir().unwrap();
    fs::write(root.path().join("app.py"), "def work():\n    pass\n").unwrap();
    let first = cli(root.path(), &["index", "."]);
    assert!(first.get("timings").is_none());
    let measured = cli(root.path(), &["update", "--timing"]);
    let times = &measured["timings"];
    let total = times["total_ms"].as_f64().unwrap();
    assert!(total >= 0.0 && total.is_finite());
    for stage in ["detect_ms", "extract_ms", "commit_ms"] {
        let duration = times[stage].as_f64().unwrap();
        assert!(duration >= 0.0 && duration <= total);
    }
    assert_eq!(measured["generation"], first["generation"]);
    assert!(cli(root.path(), &["update"]).get("timings").is_none());
    let human = Command::new(env!("CARGO_BIN_EXE_graf"))
        .current_dir(root.path())
        .args(["index", ".", "--timing"])
        .output()
        .unwrap();
    assert!(human.status.success());
    assert!(String::from_utf8_lossy(&human.stderr).contains("Timing (ms): detect="));
    let incoming = tempdir().unwrap();
    let document = incoming.path().join("notes.md");
    fs::write(&document, "# Topic\n").unwrap();
    let added = cli(
        root.path(),
        &["add", document.to_str().unwrap(), "--timing"],
    );
    let times = &added["index"]["timings"];
    assert!(times["capture_ms"].as_f64().unwrap() <= times["total_ms"].as_f64().unwrap());
    assert!(
        !index::stored_options(&root.path().join(".graf/index.db"))
            .unwrap()
            .timing
    );
}

#[test]
fn whisper_presets_use_saved_graph_topics_only_when_explicitly_selected() {
    let root = tempdir().unwrap();
    fs::write(
        root.path().join("queue.py"),
        "def flush_messages():\n    pass\ndef durable_queue():\n    flush_messages()\n",
    )
    .unwrap();
    cli(root.path(), &["index", "."]);
    let db = root.path().join(".graf/index.db");
    cli(root.path(), &["index", ".", "--whisper", "tiny"]);
    let options = index::stored_options(&db).unwrap();
    let args = &options.ingest.converters["mp3"].args;
    let at = args
        .iter()
        .position(|arg| arg == "--initial_prompt")
        .unwrap();
    assert!(args[at + 1].contains("flush_messages"));
    assert!(args[at + 1].contains("durable_queue"));
    assert!(!args[at + 1].contains(root.path().to_str().unwrap()));
    let generation = cli(root.path(), &["update"])["generation"].clone();
    assert_eq!(generation, cli(root.path(), &["update"])["generation"]);
    cli(
        root.path(),
        &[
            "index",
            ".",
            "--whisper",
            "tiny",
            "--whisper-prompt",
            "Explicit Domain",
        ],
    );
    let options = index::stored_options(&db).unwrap();
    let args = &options.ingest.converters["mp3"].args;
    let at = args
        .iter()
        .position(|arg| arg == "--initial_prompt")
        .unwrap();
    assert_eq!(args[at + 1], "Explicit Domain");
    let empty = tempdir().unwrap();
    cli(empty.path(), &["index", ".", "--whisper", "tiny"]);
    assert!(
        !index::stored_options(&empty.path().join(".graf/index.db"))
            .unwrap()
            .ingest
            .converters["mp3"]
            .args
            .iter()
            .any(|a| a == "--initial_prompt")
    );
}

#[cfg(unix)]
fn semantic_fixture(directory: &std::path::Path) -> index::IndexOptions {
    use graf::ingest::{CommandAdapter, Provider, SemanticOptions};
    let script = directory.join("provider.py");
    fs::write(directory.join("count"), "2").unwrap();
    fs::write(&script, "import sys,json,pathlib\np=pathlib.Path(sys.argv[1])\ns=json.dumps(json.load(sys.stdin))\nwith (p/'calls').open('a') as f: f.write('call\\n')\nn=4 if 'Grow' in s else int((p/'count').read_text())\nwords=['Alpha','Beta','Gamma','Delta']\nprint(json.dumps({'nodes':[{'id':str(i),'label':w,'kind':'concept','evidence':w} for i,w in enumerate(words[:n])],'edges':[]}))\n").unwrap();
    index::IndexOptions {
        ingest: IngestOptions {
            semantic: Some(SemanticOptions {
                provider: Provider::Cli,
                command: Some(CommandAdapter {
                    program: "python3".into(),
                    args: vec![
                        script.to_string_lossy().into(),
                        directory.to_string_lossy().into(),
                    ],
                    output_file: false,
                }),
                ..Default::default()
            }),
            ..Default::default()
        },
        ..Default::default()
    }
}

#[cfg(unix)]
#[test]
fn semantic_shrink_is_per_source_and_requires_explicit_backup_acceptance() {
    let root = tempdir().unwrap();
    let fixture = tempdir().unwrap();
    let mut options = semantic_fixture(fixture.path());
    let db = root.path().join(".graf/index.db");
    fs::write(root.path().join("a.md"), "Alpha Beta Gamma Delta").unwrap();
    index::run_with_options(root.path(), &db, &options).unwrap();
    let before = Store::open_read_only(&db).unwrap().snapshot().unwrap();
    let stamps =
        serde_json::to_value(Store::open_read_only(&db).unwrap().file_stamps().unwrap()).unwrap();
    fs::write(fixture.path().join("count"), "1").unwrap();
    fs::write(root.path().join("a.md"), "Alpha Beta Gamma Delta revision").unwrap();
    fs::write(root.path().join("b.md"), "Grow Alpha Beta Gamma Delta").unwrap();
    let error = index::run_with_options(root.path(), &db, &options).unwrap_err();
    assert!(format!("{error:#}").contains("a.md (nodes 2->1"));
    assert_eq!(
        Store::open_read_only(&db)
            .unwrap()
            .snapshot()
            .unwrap()
            .generation,
        before.generation
    );
    assert_eq!(
        serde_json::to_value(Store::open_read_only(&db).unwrap().file_stamps().unwrap()).unwrap(),
        stamps
    );
    assert!(!root.path().join(".graf/backups").exists());
    options.allow_semantic_shrink = true;
    let accepted = index::run_with_options(root.path(), &db, &options).unwrap();
    assert_eq!(accepted.generation, before.generation + 1);
    let backup = fs::read_dir(root.path().join(".graf/backups"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let restored = graf::snapshot::read(&backup).unwrap();
    assert_eq!(
        serde_json::to_value(&restored.nodes).unwrap(),
        serde_json::to_value(&before.nodes).unwrap()
    );
    assert!(!index::stored_options(&db).unwrap().allow_semantic_shrink);
    options.allow_semantic_shrink = false;
    fs::remove_file(root.path().join("a.md")).unwrap();
    assert_eq!(
        index::run_with_options(root.path(), &db, &options)
            .unwrap()
            .deleted_files,
        1
    );
}

#[cfg(unix)]
#[test]
fn managed_semantic_shrink_retains_cache_until_explicit_acceptance() {
    let root = tempdir().unwrap();
    let fixture = tempdir().unwrap();
    let mut options = semantic_fixture(fixture.path());
    let db = root.path().join(".graf/index.db");
    let source = fixture.path().join("incoming.md");
    fs::write(&source, "Alpha Beta Gamma Delta").unwrap();
    let capture = Default::default();
    sources::add_and_index(
        root.path(),
        &db,
        source.to_str().unwrap(),
        None,
        &options,
        &capture,
    )
    .unwrap();
    let cache = fs::read_dir(root.path().join(".graf/sources"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let bytes = fs::read(&cache).unwrap();
    let generation = Store::open_read_only(&db)
        .unwrap()
        .stats()
        .unwrap()
        .generation;
    fs::write(fixture.path().join("count"), "0").unwrap();
    let error = sources::add_and_index(
        root.path(),
        &db,
        source.to_str().unwrap(),
        None,
        &options,
        &capture,
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains("previous source cache and graph retained"));
    assert_eq!(fs::read(&cache).unwrap(), bytes);
    assert_eq!(
        Store::open_read_only(&db)
            .unwrap()
            .stats()
            .unwrap()
            .generation,
        generation
    );
    options.allow_semantic_shrink = true;
    sources::add_and_index(
        root.path(),
        &db,
        source.to_str().unwrap(),
        None,
        &options,
        &capture,
    )
    .unwrap();
    assert_ne!(fs::read(&cache).unwrap(), bytes);
    assert_eq!(
        fs::read_dir(root.path().join(".graf/backups"))
            .unwrap()
            .count(),
        2
    );
}

#[cfg(unix)]
#[test]
fn renaming_a_managed_source_does_not_authorize_semantic_loss() {
    let root = tempdir().unwrap();
    let fixture = tempdir().unwrap();
    let mut options = semantic_fixture(fixture.path());
    let db = root.path().join(".graf/index.db");
    let source = fixture.path().join("incoming.md");
    fs::write(&source, "Alpha Beta Gamma Delta").unwrap();
    let source = source.to_str().unwrap();
    let capture = Default::default();
    sources::add_and_index(root.path(), &db, source, Some("old.md"), &options, &capture).unwrap();
    // Renaming without losing facts remains ordinary supported use.
    sources::add_and_index(
        root.path(),
        &db,
        source,
        Some("renamed.md"),
        &options,
        &capture,
    )
    .unwrap();
    assert!(!root.path().join(".graf/backups").exists());
    let cache = root.path().join(format!(
        ".graf/sources/{}.json",
        blake3::hash(source.as_bytes()).to_hex()
    ));
    let bytes = fs::read(&cache).unwrap();
    let before = Store::open_read_only(&db).unwrap().snapshot().unwrap();
    fs::write(fixture.path().join("count"), "0").unwrap();
    let error = sources::add_and_index(
        root.path(),
        &db,
        source,
        Some("smaller.md"),
        &options,
        &capture,
    )
    .unwrap_err();
    let receipt = error.downcast_ref::<index::FailedSemanticUsage>().unwrap();
    assert_eq!(receipt.semantic_usage.unwrap().calls, 1);
    assert_eq!(receipt.provider_usage.len(), 1);
    assert!(receipt.provider_usage[0].output_tokens.is_none());
    assert!(!receipt.usage_unavailable);
    assert_eq!(fs::read(&cache).unwrap(), bytes);
    assert_eq!(
        Store::open_read_only(&db)
            .unwrap()
            .stats()
            .unwrap()
            .generation,
        before.generation
    );
    // A durable capture can differ from the committed graph after a failed update.
    // Even if that capture contains no facts, the old indexed identity still protects them.
    sources::add(root.path(), source, Some("pending.md"), &options.ingest).unwrap();
    let pending = fs::read(&cache).unwrap();
    assert!(
        sources::add_and_index(
            root.path(),
            &db,
            source,
            Some("smaller.md"),
            &options,
            &capture
        )
        .is_err()
    );
    assert_eq!(fs::read(&cache).unwrap(), pending);
    options.allow_semantic_shrink = true;
    sources::add_and_index(
        root.path(),
        &db,
        source,
        Some("smaller.md"),
        &options,
        &capture,
    )
    .unwrap();
    let backups: Vec<_> = fs::read_dir(root.path().join(".graf/backups"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    let graph = backups
        .iter()
        .find(|p| {
            p.file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with("graph-")
        })
        .unwrap();
    assert_eq!(
        serde_json::to_value(graf::snapshot::read(graph).unwrap().nodes).unwrap(),
        serde_json::to_value(before.nodes).unwrap()
    );
    assert!(backups.iter().any(|p| fs::read(p).unwrap() == pending));
}

#[cfg(unix)]
#[test]
fn pending_managed_capture_is_protected_before_it_has_been_indexed() {
    let root = tempdir().unwrap();
    let fixture = tempdir().unwrap();
    let mut options = semantic_fixture(fixture.path());
    let db = root.path().join(".graf/index.db");
    let source = fixture.path().join("incoming.md");
    fs::write(&source, "Alpha Beta Gamma Delta").unwrap();
    fs::write(root.path().join("local.md"), "Local Alpha Beta Gamma Delta").unwrap();
    let source = source.to_str().unwrap();
    let capture = Default::default();
    options.max_semantic_calls = Some(1);
    assert!(sources::add_and_index(root.path(), &db, source, None, &options, &capture).is_err());
    assert_eq!(
        Store::open_read_only(&db)
            .unwrap()
            .stats()
            .unwrap()
            .generation,
        0
    );
    let cache = root.path().join(format!(
        ".graf/sources/{}.json",
        blake3::hash(source.as_bytes()).to_hex()
    ));
    let pending = fs::read(&cache).unwrap();
    let record: sources::SourceRecord = serde_json::from_slice(&pending).unwrap();
    assert_eq!(
        record
            .facts
            .nodes
            .iter()
            .filter(|n| n.metadata["provenance"] == "semantic")
            .count(),
        2
    );
    fs::write(fixture.path().join("count"), "0").unwrap();
    options.max_semantic_calls = Some(2);
    assert!(sources::add_and_index(root.path(), &db, source, None, &options, &capture).is_err());
    assert_eq!(fs::read(&cache).unwrap(), pending);
    assert_eq!(
        Store::open_read_only(&db)
            .unwrap()
            .stats()
            .unwrap()
            .generation,
        0
    );
    options.allow_semantic_shrink = true;
    sources::add_and_index(root.path(), &db, source, None, &options, &capture).unwrap();
    assert!(
        fs::read_dir(root.path().join(".graf/backups"))
            .unwrap()
            .any(|e| fs::read(e.unwrap().path()).unwrap() == pending)
    );
}

#[cfg(unix)]
#[test]
fn corpus_budget_is_shared_by_capture_and_local_files_and_cache_hits_are_free() {
    let root = tempdir().unwrap();
    let fixture = tempdir().unwrap();
    let mut options = semantic_fixture(fixture.path());
    let db = root.path().join(".graf/index.db");
    let source = fixture.path().join("incoming.md");
    fs::write(&source, "Alpha Beta Gamma Delta").unwrap();
    fs::write(root.path().join("local.md"), "Local Alpha Beta Gamma Delta").unwrap();
    options.max_semantic_calls = Some(1);
    options.ingest.semantic.as_mut().unwrap().cache_dir = Some(fixture.path().join("cache"));
    let capture = Default::default();
    let error = sources::add_and_index(
        root.path(),
        &db,
        source.to_str().unwrap(),
        None,
        &options,
        &capture,
    )
    .unwrap_err();
    let receipt = error.downcast_ref::<index::FailedSemanticUsage>().unwrap();
    assert_eq!(receipt.semantic_usage.unwrap().calls, 1);
    assert_eq!(receipt.provider_usage.len(), 1);
    assert!(receipt.provider_usage[0].output_tokens.is_none());
    assert!(!receipt.usage_unavailable);
    assert_eq!(
        fs::read_to_string(fixture.path().join("calls"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    assert_eq!(
        Store::open_read_only(&db)
            .unwrap()
            .stats()
            .unwrap()
            .generation,
        0
    );
    options.max_semantic_calls = Some(2);
    let (_, report) = sources::add_and_index(
        root.path(),
        &db,
        source.to_str().unwrap(),
        None,
        &options,
        &capture,
    )
    .unwrap();
    let usage = report.semantic_usage.unwrap();
    assert_eq!(usage.calls, 1); // Captured source is now a validated cache hit.
    assert_eq!(usage.reserved_output_tokens, 2048);
    let actual = report.provider_usage.unwrap();
    assert_eq!(actual.len(), 1);
    // A generic CLI response has no provider usage envelope. Unknown is not
    // zero and must not be confused with the 2048-token reservation above.
    assert!(actual[0].input_tokens.is_none());
    assert!(actual[0].output_tokens.is_none());
    options.max_semantic_calls = Some(0);
    options.max_semantic_output_tokens = Some(0);
    options.force = true;
    let report = index::run_with_options(root.path(), &db, &options).unwrap();
    assert_eq!(report.semantic_usage.unwrap().calls, 0);
    assert!(report.provider_usage.unwrap().is_empty());
    assert_eq!(
        fs::read_to_string(fixture.path().join("calls"))
            .unwrap()
            .lines()
            .count(),
        2
    );
}

#[cfg(unix)]
#[test]
fn failed_native_provider_usage_survives_index_capture_and_cli_errors() {
    use graf::ingest::{CommandAdapter, Provider, SemanticOptions};
    let root = tempdir().unwrap();
    let fixture = tempdir().unwrap();
    let db = root.path().join(".graf/index.db");
    fs::write(root.path().join("doc.md"), "Alpha Beta").unwrap();
    index::run(root.path(), &db).unwrap();
    let original =
        serde_json::to_value(Store::open_read_only(&db).unwrap().snapshot().unwrap()).unwrap();
    let script = fixture.path().join("provider.py");
    fs::write(
        &script,
        r#"import json, sys
if sys.argv[-1] == '--help':
    print('--print --output-format --json-schema')
    sys.exit(0)
sys.stdin.read()
print(json.dumps({'is_error':True,'subtype':'error_during_execution',
  'result':'SYNTHETIC_PROVIDER_PRIVATE_OUTPUT',
  'usage':{'input_tokens':11,'output_tokens':7},'total_cost_usd':0.125}))
sys.exit(1)
"#,
    )
    .unwrap();
    let mut command = CommandAdapter::claude_cli();
    command.program = "python3".into();
    command.args.insert(0, script.to_str().unwrap().into());
    let options = index::IndexOptions {
        ingest: IngestOptions {
            semantic: Some(SemanticOptions {
                provider: Provider::ClaudeCli,
                command: Some(command),
                ..Default::default()
            }),
            ..Default::default()
        },
        ..Default::default()
    };
    let source = fixture.path().join("capture.md");
    fs::write(&source, "Alpha Beta").unwrap();
    let errors = [
        index::run_with_options(root.path(), &db, &options).unwrap_err(),
        sources::add_and_index(
            root.path(),
            &db,
            source.to_str().unwrap(),
            None,
            &options,
            &Default::default(),
        )
        .unwrap_err(),
    ];
    for error in errors {
        let usage = error.downcast_ref::<index::FailedSemanticUsage>().unwrap();
        assert_eq!(usage.semantic_usage.unwrap().calls, 1);
        assert_eq!(usage.provider_usage.len(), 1);
        assert_eq!(usage.provider_usage[0].output_tokens, Some(7));
        assert_eq!(usage.provider_usage[0].cost_usd, Some(0.125));
        assert!(!usage.usage_unavailable);
        assert!(!format!("{error:#}").contains("SYNTHETIC_PROVIDER_PRIVATE_OUTPUT"));
    }
    let config = fixture.path().join("config.json");
    fs::write(&config, serde_json::to_vec(&options).unwrap()).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_graf"))
        .current_dir(root.path())
        .args(["index", ".", "--json", "--config", config.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let error = String::from_utf8(output.stderr).unwrap();
    assert!(error.contains(r#"\"cost_usd\":0.125"#), "{error}");
    assert!(!error.contains("SYNTHETIC_PROVIDER_PRIVATE_OUTPUT"));
    assert_eq!(
        serde_json::to_value(Store::open_read_only(&db).unwrap().snapshot().unwrap()).unwrap(),
        original
    );
}

#[cfg(unix)]
#[test]
#[ignore = "requires explicit GRAF_TEST_ROBOT_PYTHON with installed Robot Framework 7.5.x"]
fn official_robot_cli_uses_explicit_interpreter_only_for_changed_sources() {
    use std::os::unix::fs::PermissionsExt;
    let python = std::env::var("GRAF_TEST_ROBOT_PYTHON").expect("set GRAF_TEST_ROBOT_PYTHON");
    let root = tempdir().unwrap();
    let adapter = tempdir().unwrap();
    let calls = adapter.path().join("calls");
    let shim = adapter.path().join("python-shim");
    let quote = |text: &str| format!("'{}'", text.replace('\'', "'\\''"));
    fs::write(
        &shim,
        format!(
            "#!/bin/sh\nprintf 'call\\n' >> {}\nexec {} \"$@\"\n",
            quote(calls.to_str().unwrap()),
            quote(&python)
        ),
    )
    .unwrap();
    fs::set_permissions(&shim, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(
        root.path().join("suite.robot"),
        "*** Test Cases ***\nCase\n    Prepare\n*** Keywords ***\nPrepare\n    Log    ready\n",
    )
    .unwrap();
    let indexed = cli(
        root.path(),
        &["index", "--robot-python", shim.to_str().unwrap()],
    );
    assert_eq!(indexed["parsed_files"], 1);
    assert_eq!(fs::read_to_string(&calls).unwrap().lines().count(), 1);
    let db = root.path().join(".graf/index.db");
    let snapshot = Store::open_read_only(&db).unwrap().snapshot().unwrap();
    assert!(
        snapshot
            .nodes
            .iter()
            .any(|n| n.metadata["parser"] == "robotframework")
    );
    fs::remove_file(&shim).unwrap();
    cli(root.path(), &["query", "Prepare"]);
    assert_eq!(cli(root.path(), &["check-update"])["fresh"], true);
    assert_eq!(cli(root.path(), &["update"])["unchanged_files"], 1);
    let forced = Command::new(env!("CARGO_BIN_EXE_graf"))
        .current_dir(root.path())
        .args(["update", "--force"])
        .output()
        .unwrap();
    assert!(!forced.status.success());
    assert_eq!(
        Store::open_read_only(&db)
            .unwrap()
            .stats()
            .unwrap()
            .generation,
        snapshot.generation
    );
    assert_eq!(fs::read_to_string(calls).unwrap().lines().count(), 1);
}

#[test]
fn cli_persists_code_only_and_explicitly_changes_configuration() {
    let root = tempdir().unwrap();
    fs::write(root.path().join("app.py"), "def entry():\n    return 1\n").unwrap();
    fs::write(root.path().join("README.md"), "# Read me\nDocumentation\n").unwrap();
    assert_eq!(
        cli(root.path(), &["index", "--code-only"])["parsed_files"],
        1
    );
    assert!(
        cli(root.path(), &["check-update"])["fresh"]
            .as_bool()
            .unwrap()
    );
    assert_eq!(cli(root.path(), &["update"])["unchanged_files"], 1);
    assert_eq!(cli(root.path(), &["update", "--force"])["parsed_files"], 1);
    assert_eq!(cli(root.path(), &["update"])["unchanged_files"], 1);
    let configuration = tempdir().unwrap();
    let config = configuration.path().join("options.json");
    fs::write(&config, r#"{"code_only":false}"#).unwrap();
    assert_eq!(
        cli(
            root.path(),
            &["index", "--config", config.to_str().unwrap()]
        )["parsed_files"],
        1
    );
    fs::write(
        root.path().join("app.py"),
        "def new_entry():\n    return 2\n",
    )
    .unwrap();
    assert!(
        !cli(root.path(), &["check-update"])["fresh"]
            .as_bool()
            .unwrap()
    );
    let out = Command::new(env!("CARGO_BIN_EXE_graf"))
        .current_dir(root.path())
        .args(["watch", "--iterations", "1", "--json"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        cli(root.path(), &["check-update"])["fresh"]
            .as_bool()
            .unwrap()
    );
}

#[test]
fn cli_add_and_provider_registry_are_explicit_and_local() {
    let root = tempdir().unwrap();
    let incoming = tempdir().unwrap();
    let source = incoming.path().join("note.md");
    fs::write(&source, "# Topic\nImported content.\n").unwrap();
    let added = cli(root.path(), &["add", source.to_str().unwrap()]);
    assert!(added["index"]["nodes"].as_u64().unwrap() > 0);
    let settings = incoming.path().join("provider.json");
    fs::write(&settings,r#"{"provider":"open_ai","model":"gpt-6-astra","endpoint":"http://127.0.0.1:1/v1/chat/completions","key_env":"GRAF_TEST_KEY"}"#).unwrap();
    cli(
        root.path(),
        &[
            "provider",
            "--project",
            ".",
            "add",
            "local",
            settings.to_str().unwrap(),
        ],
    );
    let shown = cli(
        root.path(),
        &["provider", "--project", ".", "show", "local"],
    );
    assert_eq!(shown["key_env"], "GRAF_TEST_KEY");
    cli(
        root.path(),
        &["provider", "--project", ".", "remove", "local"],
    );
    assert_eq!(
        cli(root.path(), &["provider", "--project", ".", "list"])["custom"],
        serde_json::json!([])
    );
}

#[test]
fn query_logging_requires_explicit_destination_and_body_opt_in() {
    let root = tempdir().unwrap();
    fs::write(root.path().join("app.py"), "def entry():\n    return 1\n").unwrap();
    cli(root.path(), &["index"]);
    let log = root.path().join("queries.jsonl");
    cli(root.path(), &["query", "entry"]);
    assert!(!log.exists());
    cli(
        root.path(),
        &["query", "entry", "--query-log", log.to_str().unwrap()],
    );
    let records = fs::read_to_string(&log).unwrap();
    let value: serde_json::Value = serde_json::from_str(records.trim()).unwrap();
    assert_eq!(value["question"], "entry");
    assert!(value.get("response").is_none());
    cli(
        root.path(),
        &[
            "query",
            "entry",
            "--query-log",
            log.to_str().unwrap(),
            "--log-responses",
        ],
    );
    let records = fs::read_to_string(&log).unwrap();
    assert_eq!(records.lines().count(), 2);
    let value: serde_json::Value = serde_json::from_str(records.lines().nth(1).unwrap()).unwrap();
    assert!(value["response"]["nodes"].is_array());
    let db = root.path().join(".graf/index.db");
    let before = fs::read(&db).unwrap();
    cli(
        root.path(),
        &["query", "entry", "--query-log", db.to_str().unwrap()],
    );
    assert_eq!(fs::read(&db).unwrap(), before);
}

#[test]
fn extensionless_scripts_and_binary_forms_keep_real_paths() {
    let root = tempdir().unwrap();
    fs::create_dir(root.path().join("bin.v1")).unwrap();
    for (name, source) in [
        (
            "pytool",
            "#!/usr/bin/env python3\ndef python_entry():\n    return 1\n",
        ),
        (
            "jstool",
            "#!/usr/bin/env node\nfunction javascript_entry() { return 1; }\n",
        ),
        ("shtool", "#!/bin/sh\nshell_entry() { echo ok; }\n"),
        (
            "jltool",
            "#!/usr/bin/env julia\nfunction julia_entry()\n  return 1\nend\n",
        ),
        (
            "ordinary",
            "An ordinary extensionless document, not executable code.\n",
        ),
    ] {
        fs::write(root.path().join("bin.v1").join(name), source).unwrap();
    }
    fs::write(root.path().join("form.dfm"), b"TPF0\xff\0").unwrap();
    let db = root.path().join(".graf/index.db");
    let report = index::run(root.path(), &db).unwrap();
    let snapshot = Store::open_read_only(&db).unwrap().snapshot().unwrap();
    for (file, label) in [
        ("pytool", "python_entry"),
        ("jstool", "javascript_entry"),
        ("shtool", "shell_entry"),
        ("jltool", "julia_entry"),
    ] {
        assert!(
            snapshot
                .nodes
                .iter()
                .any(|n| n.file == format!("bin.v1/{file}") && n.label == label),
            "{file}"
        );
    }
    assert!(!snapshot.nodes.iter().any(|n| n.file == "bin.v1/ordinary"));
    assert!(
        report
            .diagnostics
            .iter()
            .any(|d| d.file == "form.dfm" && d.message.to_lowercase().contains("binary"))
    );
    assert!(!snapshot.nodes.iter().any(|n| n.file == "form.dfm"));
    assert!(index::check_update(root.path(), &db).unwrap().fresh);
    assert_eq!(
        index::run(root.path(), &db).unwrap().generation,
        report.generation
    );
}

#[test]
fn configured_python_roots_resolve_imports_and_invalidate_unchanged_sources() {
    let root = tempdir().unwrap();
    fs::create_dir_all(root.path().join("lib/pkg")).unwrap();
    fs::write(
        root.path().join("lib/pkg/api.py"),
        "def target():\n    return 1\n",
    )
    .unwrap();
    fs::write(
        root.path().join("lib/runner.py"),
        "from pkg.api import target\ndef entry():\n    return target()\n",
    )
    .unwrap();
    let db = root.path().join(".graf/index.db");
    index::run(root.path(), &db).unwrap();
    let before = Store::open_read_only(&db).unwrap().snapshot().unwrap();
    assert!(!before.edges.iter().any(|e| e.relation == "calls"));
    let changed = cli(root.path(), &["index", "--python-source-root", "lib"]);
    assert_eq!(changed["parsed_files"], 2);
    let after = Store::open_read_only(&db).unwrap().snapshot().unwrap();
    assert!(after.edges.iter().any(|e| e.relation == "calls"));
    assert!(index::check_update(root.path(), &db).unwrap().fresh);
    assert_eq!(cli(root.path(), &["update"])["unchanged_files"], 2);
    let bad = Command::new(env!("CARGO_BIN_EXE_graf"))
        .current_dir(root.path())
        .args(["index", "--python-source-root", "../outside"])
        .output()
        .unwrap();
    assert!(!bad.status.success());
    assert_eq!(
        Store::open_read_only(&db)
            .unwrap()
            .stats()
            .unwrap()
            .generation,
        after.generation
    );
}

#[test]
fn deep_cli_preserves_ast_and_never_refetches_during_query_or_unchanged_update() {
    use std::io::{Read, Write};
    let root = tempdir().unwrap();
    fs::write(
        root.path().join("app.py"),
        "def queue():\n    \"\"\"Queue provides durability\"\"\"\n    return 1\n",
    )
    .unwrap();
    cli(root.path(), &["index"]);
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!(
        "http://{}/v1/chat/completions",
        listener.local_addr().unwrap()
    );
    let server = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        let mut input = Vec::new();
        let mut buffer = [0; 4096];
        let end = loop {
            let n = socket.read(&mut buffer).unwrap();
            assert!(n > 0);
            input.extend_from_slice(&buffer[..n]);
            if let Some(i) = input.windows(4).position(|w| w == b"\r\n\r\n") {
                break i + 4;
            }
        };
        let length: usize = String::from_utf8_lossy(&input[..end])
            .lines()
            .find_map(|line| {
                line.to_ascii_lowercase()
                    .strip_prefix("content-length:")
                    .map(|n| n.trim().parse().unwrap())
            })
            .unwrap();
        while input.len() < end + length {
            let n = socket.read(&mut buffer).unwrap();
            assert!(n > 0);
            input.extend_from_slice(&buffer[..n]);
        }
        let graph = serde_json::json!({"nodes":[{"id":"q","label":"Queue","kind":"concept","evidence":"Queue"}],"edges":[]});
        let body = serde_json::json!({"choices":[{"finish_reason":"stop","message":{"content":graph.to_string()}}]}).to_string();
        write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
    });
    cli(
        root.path(),
        &[
            "index",
            "--deep",
            "--provider",
            "open_ai",
            "--endpoint",
            &endpoint,
        ],
    );
    server.join().unwrap(); // The provider is now offline.
    let db = root.path().join(".graf/index.db");
    let snapshot = Store::open_read_only(&db).unwrap().snapshot().unwrap();
    assert!(
        snapshot
            .nodes
            .iter()
            .any(|n| n.kind == "function" && n.label == "queue")
    );
    assert!(
        snapshot
            .nodes
            .iter()
            .any(|n| n.kind == "concept" && n.label == "Queue")
    );
    cli(root.path(), &["query", "queue"]);
    assert_eq!(cli(root.path(), &["update"])["unchanged_files"], 1);
    assert_eq!(
        cli(root.path(), &["index", "--no-semantic"])["parsed_files"],
        1
    );
    assert!(
        !Store::open_read_only(&db)
            .unwrap()
            .snapshot()
            .unwrap()
            .nodes
            .iter()
            .any(|n| n.kind == "concept")
    );
}

#[test]
fn navigation_filters_and_complete_path_budgets_reach_cli() {
    let root = tempdir().unwrap();
    fs::write(
        root.path().join("app.py"),
        "def target():\n    return 1\ndef entry():\n    return target()\n",
    )
    .unwrap();
    cli(root.path(), &["index"]);
    let path = cli(
        root.path(),
        &[
            "path", "entry", "target", "--kind", "function", "--file", "app.py", "--budget", "4096",
        ],
    );
    assert_eq!(path["found"], true);
    assert_eq!(
        path["result"]["graph"]["nodes"].as_array().unwrap().len(),
        2
    );
    assert!(path["result"]["estimated_tokens"].as_u64().unwrap() <= 4096);
    let callers = cli(
        root.path(),
        &[
            "callers", "target", "--kind", "function", "--budget", "4096",
        ],
    );
    assert!(
        callers["graph"]["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n["label"] == "entry")
    );
    let limited = Command::new(env!("CARGO_BIN_EXE_graf"))
        .current_dir(root.path())
        .args(["path", "entry", "target", "--budget", "1", "--json"])
        .output()
        .unwrap();
    assert!(!limited.status.success());
    assert!(limited.stdout.is_empty());
}

#[test]
fn explicit_google_add_is_case_insensitive_while_local_extraction_never_fetches() {
    let root = tempdir().unwrap();
    let input = tempdir().unwrap();
    let mut options = IngestOptions::default();
    let missing = input.path().join("converter-does-not-exist");
    for extension in ["gdoc", "gsheet", "gslides"] {
        options.converters.insert(
            extension.into(),
            graf::ingest::CommandAdapter {
                program: missing.to_str().unwrap().into(),
                args: vec![],
                output_file: false,
            },
        );
        for suffix in [extension.to_owned(), extension.to_uppercase()] {
            let name = format!("design.{suffix}");
            let path = input.path().join(&name);
            fs::write(&path, r#"{"doc_id":"abc_123"}"#).unwrap();
            let local = graf::ingest::extract(&path, &name, "fixture", &options).unwrap();
            assert!(!local.nodes.is_empty());
            // Explicit add must attempt the selected exporter in either casing.
            assert!(sources::add(root.path(), path.to_str().unwrap(), None, &options).is_err());
        }
    }
    assert!(!root.path().join(".graf/sources").exists());
}

#[test]
fn cli_freshness_preserves_committed_wal_pages() {
    const CHILD: &str = "GRAF_TEST_COMMITTED_WAL";
    if let Some(db) = std::env::var_os(CHILD) {
        let connection = rusqlite::Connection::open(db).unwrap();
        connection
            .pragma_update(None, "wal_autocheckpoint", 0)
            .unwrap();
        connection
            .execute("UPDATE metadata SET generation=generation+1", [])
            .unwrap();
        // Deliberately exit without running SQLite's closing checkpoint.
        std::process::exit(0);
    }
    let root = tempdir().unwrap();
    fs::write(root.path().join("app.py"), "def entry():\n    return 1\n").unwrap();
    let generation = cli(root.path(), &["index"])["generation"].as_u64().unwrap();
    let db = root.path().join(".graf/index.db");
    let child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "cli_freshness_preserves_committed_wal_pages"])
        .env(CHILD, &db)
        .output()
        .unwrap();
    assert!(child.status.success());
    let wal = root.path().join(".graf/index.db-wal");
    let before = fs::read(&db).unwrap();
    let wal_before = fs::read(&wal).unwrap();
    assert!(!wal_before.is_empty());
    for args in [&["check-update"][..], &["watch", "--iterations", "1"][..]] {
        let output = Command::new(env!("CARGO_BIN_EXE_graf"))
            .current_dir(root.path())
            .args(args)
            .arg("--json")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(fs::read(&db).unwrap(), before);
        assert_eq!(fs::read(&wal).unwrap(), wal_before);
    }
    let result = cli(root.path(), &["check-update"]);
    assert_eq!(result["generation"], generation + 1);
    assert_eq!(result["fresh"], true);
}

#[cfg(unix)]
#[test]
fn managed_add_and_local_index_share_semantic_file_budget() {
    use graf::ingest::{CommandAdapter, Provider, SemanticOptions};
    let root = tempdir().unwrap();
    let input = tempdir().unwrap();
    let source = input.path().join("remote.txt");
    let calls = input.path().join("calls.txt");
    let script = input.path().join("provider.py");
    fs::write(&source, "Queue provides durability").unwrap();
    fs::write(&script, "import sys,json\njson.load(sys.stdin)\nwith open(sys.argv[1], 'a') as f: f.write('called\\n')\nprint(json.dumps({'nodes':[], 'edges':[]}))\n").unwrap();
    let options = index::IndexOptions {
        ingest: IngestOptions {
            semantic: Some(SemanticOptions {
                provider: Provider::Cli,
                command: Some(CommandAdapter {
                    program: "python3".into(),
                    args: vec![
                        script.to_str().unwrap().into(),
                        calls.to_str().unwrap().into(),
                    ],
                    output_file: false,
                }),
                ..Default::default()
            }),
            ..Default::default()
        },
        ..Default::default()
    };
    let config = input.path().join("options.json");
    fs::write(&config, serde_json::to_vec(&options).unwrap()).unwrap();
    let run = |limit: &str| {
        Command::new(env!("CARGO_BIN_EXE_graf"))
            .current_dir(root.path())
            .args([
                "add",
                source.to_str().unwrap(),
                "--config",
                config.to_str().unwrap(),
                "--max-semantic-files",
                limit,
                "--json",
            ])
            .output()
            .unwrap()
    };
    let denied = run("0");
    assert!(!denied.status.success());
    assert!(String::from_utf8_lossy(&denied.stderr).contains("file budget"));
    assert!(!calls.exists());
    assert!(!root.path().join(".graf/sources").exists());
    let allowed = run("1");
    assert!(
        allowed.status.success(),
        "{}",
        String::from_utf8_lossy(&allowed.stderr)
    );
    assert_eq!(fs::read_to_string(&calls).unwrap().lines().count(), 1);
    let db = root.path().join(".graf/index.db");
    assert_eq!(index::stored_options(&db).unwrap().max_semantic_files, 1);
    let generation = Store::open_read_only(&db)
        .unwrap()
        .stats()
        .unwrap()
        .generation;
    fs::write(
        root.path().join("local.txt"),
        "Queue provides durability locally",
    )
    .unwrap();
    let denied = run("1");
    assert!(!denied.status.success());
    assert!(String::from_utf8_lossy(&denied.stderr).contains("file budget"));
    assert_eq!(fs::read_to_string(&calls).unwrap().lines().count(), 2);
    assert_eq!(
        Store::open_read_only(&db)
            .unwrap()
            .stats()
            .unwrap()
            .generation,
        generation
    );
    let allowed = run("2");
    assert!(
        allowed.status.success(),
        "{}",
        String::from_utf8_lossy(&allowed.stderr)
    );
    assert_eq!(fs::read_to_string(&calls).unwrap().lines().count(), 4);
    assert_eq!(index::stored_options(&db).unwrap().max_semantic_files, 2);
}

#[test]
fn watch_retries_writer_contention_and_combines_pending_file_changes() {
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;
    let root = tempdir().unwrap();
    fs::write(root.path().join("first.py"), "def old_first():\n    pass\n").unwrap();
    fs::write(
        root.path().join("second.py"),
        "def old_second():\n    pass\n",
    )
    .unwrap();
    let generation = cli(root.path(), &["index"])["generation"].as_u64().unwrap();
    let db = root.path().join(".graf/index.db");
    let writer = rusqlite::Connection::open(&db).unwrap();
    writer.execute_batch("BEGIN IMMEDIATE").unwrap();
    fs::write(root.path().join("first.py"), "def new_first():\n    pass\n").unwrap();
    fs::write(
        root.path().join("second.py"),
        "def new_second():\n    pass\n",
    )
    .unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_graf"))
        .current_dir(root.path())
        .args([
            "watch",
            "--interval-ms",
            "100",
            "--iterations",
            "2",
            "--json",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stderr = child.stderr.take().unwrap();
    let (send, receive) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines() {
            let _ = send.send(line.unwrap());
        }
    });
    let retry = receive.recv_timeout(std::time::Duration::from_secs(15));
    if retry.is_err() {
        let _ = child.kill();
        let _ = child.wait();
    }
    let retry = retry.expect("watch did not report bounded database contention");
    assert!(retry.contains("retrying"), "{retry}");
    writer.execute_batch("ROLLBACK").unwrap();
    let output = child.wait_with_output().unwrap();
    reader.join().unwrap();
    assert!(output.status.success());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["parsed_files"], 2);
    assert_eq!(report["generation"], generation + 1);
    assert!(index::check_update(root.path(), &db).unwrap().fresh);
}

#[cfg(unix)]
#[test]
fn clone_cache_reuses_offline_and_refreshes_without_resetting_user_changes() {
    let temp = tempdir().unwrap();
    let work = temp.path().join("upstream");
    let bare = temp.path().join("remote.git");
    let home = temp.path().join("home");
    fs::create_dir(&work).unwrap();
    fs::create_dir(&home).unwrap();
    let git = |directory: &std::path::Path, args: &[&str]| {
        let result = Command::new("git")
            .current_dir(directory)
            .args(args)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_AUTHOR_NAME", "Fixture")
            .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
            .env("GIT_COMMITTER_NAME", "Fixture")
            .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    };
    git(&work, &["init", "-b", "main"]);
    fs::write(work.join("app.py"), "def original():\n    pass\n").unwrap();
    git(&work, &["add", "app.py"]);
    git(&work, &["commit", "-m", "initial fixture"]);
    git(
        temp.path(),
        &[
            "clone",
            "--bare",
            work.to_str().unwrap(),
            bare.to_str().unwrap(),
        ],
    );
    git(&work, &["remote", "add", "origin", bare.to_str().unwrap()]);
    let url = "https://github.com/synthetic/project";
    let file_url = reqwest::Url::from_file_path(&bare).unwrap();
    let rewrite = format!("url.{file_url}.insteadOf");
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_graf"))
            .current_dir(temp.path())
            .env("HOME", &home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_COUNT", "1")
            .env("GIT_CONFIG_KEY_0", &rewrite)
            .env("GIT_CONFIG_VALUE_0", url)
            .arg("clone")
            .arg(url)
            .args(args)
            .arg("--json")
            .output()
            .unwrap()
    };
    let first = run(&["--index"]);
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let first: serde_json::Value = serde_json::from_slice(&first.stdout).unwrap();
    let checkout = home.join(".graf/repos/synthetic/project");
    assert_eq!(first["path"], checkout.to_str().unwrap());
    assert_eq!(first["status"], "cloned");
    fs::write(work.join("app.py"), "def replacement():\n    pass\n").unwrap();
    git(&work, &["commit", "-am", "replacement fixture"]);
    git(&work, &["push", "origin", "main"]);
    assert!(run(&[]).status.success());
    assert!(
        fs::read_to_string(checkout.join("app.py"))
            .unwrap()
            .contains("original")
    );
    let refreshed = run(&["--refresh", "--index"]);
    assert!(
        refreshed.status.success(),
        "{}",
        String::from_utf8_lossy(&refreshed.stderr)
    );
    assert!(
        fs::read_to_string(checkout.join("app.py"))
            .unwrap()
            .contains("replacement")
    );
    let db = checkout.join(".graf/index.db");
    let graph = Store::open_read_only(&db).unwrap().snapshot().unwrap();
    assert!(graph.nodes.iter().any(|n| n.label == "replacement"));
    assert!(!graph.nodes.iter().any(|n| n.label == "original"));
    let user_edit = "def local_edit():\n    pass\n";
    fs::write(checkout.join("app.py"), user_edit).unwrap();
    fs::write(work.join("app.py"), "def later():\n    pass\n").unwrap();
    git(&work, &["commit", "-am", "later fixture"]);
    git(&work, &["push", "origin", "main"]);
    assert!(!run(&["--refresh"]).status.success());
    assert_eq!(
        fs::read_to_string(checkout.join("app.py")).unwrap(),
        user_edit
    );
    assert_eq!(
        Store::open_read_only(&db)
            .unwrap()
            .stats()
            .unwrap()
            .generation,
        graph.generation
    );
    assert!(!run(&["--branch", "other"]).status.success());
    fs::rename(&bare, temp.path().join("offline.git")).unwrap();
    let reused = run(&[]);
    assert!(reused.status.success());
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&reused.stdout).unwrap()["status"],
        "reused"
    );
    assert!(!run(&["--refresh"]).status.success());
}
