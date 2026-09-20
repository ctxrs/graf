use std::{fs, path::Path, sync::Arc, thread};

use clap::Parser;
use graf::{
    index,
    memory::{
        LearningOverlay, MAX_TEXT_BYTES, Outcome, ReflectArgs, SaveResultArgs, learning_overlay_at,
        load_records, reflect_at, save_result_at,
    },
    model::{GraphSnapshot, Node},
    store::Store,
};
use serde_json::json;

const DAY: u64 = 86400;
const NOW: u64 = 20000 * DAY;

fn save_args(dir: &Path) -> SaveResultArgs {
    SaveResultArgs {
        question: "Where is parsing?".into(),
        answer: Some("Use parse.".into()),
        answer_file: None,
        query_type: "query".into(),
        nodes: Vec::new(),
        outcome: Some(Outcome::Useful),
        correction: None,
        memory_dir: dir.join("memory"),
    }
}

fn reflect_args(dir: &Path) -> ReflectArgs {
    ReflectArgs {
        memory_dir: dir.join("memory"),
        out: dir.join("lessons/LESSONS.md"),
        half_life_days: 30.0,
        min_corroboration: 2,
        if_stale: false,
    }
}

fn graph(root: &Path) -> GraphSnapshot {
    fs::write(root.join("source.rs"), "fn parse() {}\n").unwrap();
    GraphSnapshot {
        schema_version: 1,
        generation: 7,
        kind: "native".into(),
        root: Some(root.to_string_lossy().into_owned()),
        nodes: vec![Node {
            id: "node:parse".into(),
            label: "parse".into(),
            kind: "function".into(),
            file: "source.rs".into(),
            line: Some(1),
            end_line: Some(1),
            qualified_name: None,
            binding_key: None,
            metadata: json!({"community":"parser"}),
        }],
        edges: Vec::new(),
        metadata: json!({}),
    }
}

fn indexed_graph(dir: &Path) -> GraphSnapshot {
    let root = dir.join("project");
    fs::create_dir_all(&root).unwrap();
    for (file, bytes) in [
        ("source.py", "def parse():\n    return 1\n"),
        ("source.rs", "fn parse() {}\n"),
        ("notes.md", "# Parsing\n\nParser notes.\n"),
    ] {
        fs::write(root.join(file), bytes).unwrap();
    }
    reindex(dir)
}

fn reindex(dir: &Path) -> GraphSnapshot {
    let db = dir.join("graph.db");
    index::run(&dir.join("project"), &db).unwrap();
    Store::open_read_only(&db).unwrap().snapshot().unwrap()
}

fn source_node(graph: &GraphSnapshot, file: &str) -> String {
    graph
        .nodes
        .iter()
        .find(|n| n.file == file && (n.label == "parse" || n.kind == "document"))
        .unwrap_or_else(|| panic!("no expected indexed node for {file}"))
        .id
        .clone()
}

fn section<'a>(text: &'a str, heading: &str) -> &'a str {
    text.split_once(&format!("## {heading}\n"))
        .unwrap()
        .1
        .split("\n## ")
        .next()
        .unwrap()
}

#[test]
fn memory_round_trips_escaping_unicode_and_answer_file() {
    let dir = tempfile::tempdir().unwrap();
    let mut args = save_args(dir.path());
    args.question = "\"quoted\" \\ path\n---\n日本語 🦀\twhy?".into();
    args.answer = None;
    let answer = dir.path().join("answer.txt");
    fs::write(&answer, "  <script>x</script>\n## corrected\nUnicode λ  ").unwrap();
    args.answer_file = Some(answer);
    let path = save_result_at(&args, None, NOW).unwrap();
    assert!(path.is_file());
    let records = load_records(&args.memory_dir).unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].question, args.question);
    assert_eq!(
        records[0].answer,
        "<script>x</script>\n## corrected\nUnicode λ"
    );
    let reflection = reflect_args(dir.path());
    reflect_at(&reflection, None, NOW).unwrap();
    let text = fs::read_to_string(reflection.out).unwrap();
    assert!(text.contains("    <script>") || text.contains("Answer: <script>"));
    assert!(!text.contains("\n## corrected\nUnicode"));
}

#[test]
fn memory_exact_node_ids_reject_labels_missing_and_ambiguous_ids() {
    let dir = tempfile::tempdir().unwrap();
    let mut graph = graph(dir.path());
    let mut args = save_args(dir.path());
    args.nodes = vec!["parse".into()];
    assert!(save_result_at(&args, Some(&graph), NOW).is_err());
    args.nodes = vec!["missing".into()];
    assert!(save_result_at(&args, Some(&graph), NOW).is_err());
    args.nodes = vec!["node:parse".into(), "node:parse".into()];
    assert!(save_result_at(&args, None, NOW).is_err());
    save_result_at(&args, Some(&graph), NOW).unwrap();
    assert_eq!(
        load_records(&args.memory_dir).unwrap()[0].source_nodes,
        vec!["node:parse"]
    );
    graph.nodes.push(graph.nodes[0].clone());
    assert!(save_result_at(&args, Some(&graph), NOW).is_err());
}

#[test]
fn memory_atomic_repeated_and_concurrent_saves_preserve_every_event() {
    let dir = tempfile::tempdir().unwrap();
    let args = Arc::new(save_args(dir.path()));
    let handles: Vec<_> = (0..16)
        .map(|_| {
            let args = Arc::clone(&args);
            thread::spawn(move || save_result_at(&args, None, NOW).unwrap())
        })
        .collect();
    let paths: std::collections::BTreeSet<_> =
        handles.into_iter().map(|h| h.join().unwrap()).collect();
    assert_eq!(paths.len(), 16);
    save_result_at(&args, None, NOW).unwrap();
    assert_eq!(load_records(&args.memory_dir).unwrap().len(), 17);
    assert_eq!(fs::read_dir(&args.memory_dir).unwrap().count(), 17);
}

#[test]
fn reflection_distinct_events_corroborate_but_copied_files_do_not() {
    let dir = tempfile::tempdir().unwrap();
    let graph = graph(dir.path());
    let mut args = save_args(dir.path());
    args.nodes.push("node:parse".into());
    let path = save_result_at(&args, Some(&graph), NOW).unwrap();
    fs::copy(path, args.memory_dir.join("copy.md")).unwrap();
    let reflection = reflect_args(dir.path());
    reflect_at(&reflection, Some(&graph), NOW).unwrap();
    let text = fs::read_to_string(&reflection.out).unwrap();
    assert!(!section(&text, "preferred").contains("node:parse"));
    assert!(section(&text, "tentative").contains("useful=1"));
    save_result_at(&args, Some(&graph), NOW + 1).unwrap();
    reflect_at(&reflection, Some(&graph), NOW + 1).unwrap();
    let text = fs::read_to_string(&reflection.out).unwrap();
    assert!(!section(&text, "preferred").contains("node:parse"));
    assert!(section(&text, "tentative").contains("useful=2"));
    assert!(section(&text, "tentative").contains("community=parser"));
    assert!(section(&text, "tentative").contains("verification=unverified"));
}

#[test]
fn reflection_correction_and_decay_preserve_original_provenance() {
    let dir = tempfile::tempdir().unwrap();
    let graph = graph(dir.path());
    let mut args = save_args(dir.path());
    args.nodes.push("node:parse".into());
    save_result_at(&args, Some(&graph), NOW - 30 * DAY).unwrap();
    args.outcome = Some(Outcome::Corrected);
    args.correction = Some("Use parse_v2 instead.".into());
    let correction = save_result_at(&args, Some(&graph), NOW).unwrap();
    let mut reflection = reflect_args(dir.path());
    reflect_at(&reflection, Some(&graph), NOW).unwrap();
    let text = fs::read_to_string(&reflection.out).unwrap();
    assert!(section(&text, "contested").contains("score=-0.500000000"));
    assert!(section(&text, "corrected").contains("Original answer: Use parse."));
    assert!(section(&text, "corrected").contains("Use parse_v2 instead."));
    assert!(
        fs::read_to_string(correction)
            .unwrap()
            .contains("Use parse_v2 instead.")
    );
    reflection.half_life_days = 0.0;
    reflect_at(&reflection, Some(&graph), NOW).unwrap();
    assert!(
        section(&fs::read_to_string(&reflection.out).unwrap(), "contested")
            .contains("score=0.000000000")
    );
}

#[test]
fn reflection_dead_end_only_is_not_a_recommended_source() {
    let dir = tempfile::tempdir().unwrap();
    let graph = graph(dir.path());
    let mut args = save_args(dir.path());
    args.nodes.push("node:parse".into());
    args.outcome = Some(Outcome::DeadEnd);
    save_result_at(&args, Some(&graph), NOW).unwrap();
    let reflection = reflect_args(dir.path());
    reflect_at(&reflection, Some(&graph), NOW).unwrap();
    let text = fs::read_to_string(reflection.out).unwrap();
    for name in ["preferred", "tentative", "contested"] {
        assert!(!section(&text, name).contains("node:parse"));
    }
    assert!(section(&text, "dead_end").contains("Where is parsing?"));
}

#[test]
fn reflection_stale_source_node_and_graph_changes_are_not_guessed() {
    let dir = tempfile::tempdir().unwrap();
    let graph = graph(dir.path());
    let mut args = save_args(dir.path());
    args.nodes.push("node:parse".into());
    save_result_at(&args, Some(&graph), NOW).unwrap();
    let reflection = reflect_args(dir.path());
    reflect_at(&reflection, Some(&graph), NOW).unwrap();
    assert!(
        fs::read_to_string(&reflection.out)
            .unwrap()
            .contains("unverified (unchanged since save; snapshot/source correspondence unknown)")
    );
    fs::write(dir.path().join("source.rs"), "fn changed() {}\n").unwrap();
    reflect_at(&reflection, Some(&graph), NOW).unwrap();
    let text = fs::read_to_string(&reflection.out).unwrap();
    assert!(text.contains("stale (source changed)"));
    assert!(!section(&text, "tentative").contains("node:parse"));
    fs::remove_file(dir.path().join("source.rs")).unwrap();
    reflect_at(&reflection, Some(&graph), NOW).unwrap();
    assert!(
        fs::read_to_string(&reflection.out)
            .unwrap()
            .contains("stale (source unavailable)")
    );
    let mut changed = graph.clone();
    changed.nodes[0].label = "changed".into();
    reflect_at(&reflection, Some(&changed), NOW).unwrap();
    assert!(
        fs::read_to_string(&reflection.out)
            .unwrap()
            .contains("stale (node changed)")
    );
    changed = graph.clone();
    changed.root = Some("different-project".into());
    reflect_at(&reflection, Some(&changed), NOW).unwrap();
    assert!(
        fs::read_to_string(&reflection.out)
            .unwrap()
            .contains("stale (graph identity changed)")
    );
    changed.nodes.clear();
    reflect_at(&reflection, Some(&changed), NOW).unwrap();
    assert!(
        fs::read_to_string(&reflection.out)
            .unwrap()
            .contains("stale (node missing)")
    );
    reflect_at(&reflection, None, NOW).unwrap();
    assert!(
        fs::read_to_string(&reflection.out)
            .unwrap()
            .contains("unverified (no graph supplied)")
    );
}

#[test]
fn reflection_source_changed_before_save_never_becomes_current_or_preferred() {
    let dir = tempfile::tempdir().unwrap();
    let graph = graph(dir.path());
    let snapshot_bytes = serde_json::to_vec(&graph).unwrap();
    // The snapshot still describes parse, but disk no longer contains it at save time.
    let source = dir.path().join("source.rs");
    fs::write(&source, "fn replacement() {}\n").unwrap();
    let mut args = save_args(dir.path());
    args.nodes.push("node:parse".into());
    save_result_at(&args, Some(&graph), NOW).unwrap();
    save_result_at(&args, Some(&graph), NOW + 1).unwrap();

    // Both independently captured baselines match at reflection time. That cannot
    // establish that either saved event refers to a node in the current source.
    let mut reflection = reflect_args(dir.path());
    reflection.if_stale = true;
    reflect_at(&reflection, Some(&graph), NOW + 1).unwrap();
    let text = fs::read_to_string(&reflection.out).unwrap();
    assert!(
        text.contains("unverified (unchanged since save; snapshot/source correspondence unknown)")
    );
    assert!(!text.contains("| current"));
    assert!(!section(&text, "preferred").contains("node:parse"));
    assert!(section(&text, "tentative").contains("useful=2"));
    assert!(section(&text, "tentative").contains("verification=unverified"));
    assert!(
        reflect_at(&reflection, Some(&graph), NOW + 1)
            .unwrap()
            .skipped
    );
    reflection.min_corroboration = 1;
    reflect_at(&reflection, Some(&graph), NOW + 1).unwrap();
    assert!(
        section(&fs::read_to_string(&reflection.out).unwrap(), "preferred")
            .trim()
            .is_empty()
    );
    assert_eq!(fs::read_to_string(source).unwrap(), "fn replacement() {}\n");
    assert_eq!(serde_json::to_vec(&graph).unwrap(), snapshot_bytes);
}

#[test]
fn reflection_matching_source_is_unchanged_since_save_then_stale_after_edit() {
    let dir = tempfile::tempdir().unwrap();
    let graph = graph(dir.path());
    let source = dir.path().join("source.rs");
    let mut args = save_args(dir.path());
    args.nodes.push("node:parse".into());
    save_result_at(&args, Some(&graph), NOW).unwrap();
    save_result_at(&args, Some(&graph), NOW + 1).unwrap();
    let mut reflection = reflect_args(dir.path());
    reflection.if_stale = true;
    reflect_at(&reflection, Some(&graph), NOW + 1).unwrap();
    let text = fs::read_to_string(&reflection.out).unwrap();
    // Even a matching fixture is not an indexed-digest proof carried by GraphSnapshot.
    assert!(
        text.contains("unverified (unchanged since save; snapshot/source correspondence unknown)")
    );
    assert!(section(&text, "tentative").contains("useful=2"));
    assert!(!section(&text, "preferred").contains("node:parse"));
    assert!(!text.contains("| stale"));
    assert!(!text.contains("| current"));
    assert_eq!(fs::read_to_string(&source).unwrap(), "fn parse() {}\n");

    fs::write(&source, "fn replacement() {}\n").unwrap();
    assert!(
        !reflect_at(&reflection, Some(&graph), NOW + 1)
            .unwrap()
            .skipped
    );
    let text = fs::read_to_string(&reflection.out).unwrap();
    assert!(text.contains("stale (source changed)"));
    for name in ["preferred", "tentative", "contested"] {
        assert!(!section(&text, name).contains("node:parse"));
    }
    assert!(section(&text, "Event provenance").contains("Where is parsing?"));
}

#[test]
fn reflection_unverified_tentative_sources_rank_by_decayed_score_then_id() {
    let dir = tempfile::tempdir().unwrap();
    let mut graph = graph(dir.path());
    let template = graph.nodes.remove(0);
    for id in ["a:old", "z:recent", "b:recent"] {
        let mut node = template.clone();
        node.id = id.into();
        graph.nodes.push(node);
    }
    let mut args = save_args(dir.path());
    for (id, time) in [
        ("a:old", NOW - 30 * DAY),
        ("z:recent", NOW),
        ("b:recent", NOW),
    ] {
        args.nodes = vec![id.into()];
        save_result_at(&args, Some(&graph), time).unwrap();
    }
    let reflection = reflect_args(dir.path());
    reflect_at(&reflection, Some(&graph), NOW).unwrap();
    let text = fs::read_to_string(&reflection.out).unwrap();
    let tentative = section(&text, "tentative");
    assert!(tentative.find("b:recent").unwrap() < tentative.find("z:recent").unwrap());
    assert!(tentative.find("z:recent").unwrap() < tentative.find("a:old").unwrap());
    assert!(tentative.contains("score=0.500000000"));
    assert!(
        tentative.contains("verification=unverified (snapshot/source correspondence unproven)")
    );
    assert!(section(&text, "preferred").trim().is_empty());
    let first = fs::read(&reflection.out).unwrap();
    reflect_at(&reflection, Some(&graph), NOW).unwrap();
    assert_eq!(fs::read(reflection.out).unwrap(), first);
}

#[test]
fn reflection_is_deterministic_and_if_stale_checks_content_and_clock() {
    let dir = tempfile::tempdir().unwrap();
    let mut args = save_args(dir.path());
    save_result_at(&args, None, NOW).unwrap();
    let mut reflection = reflect_args(dir.path());
    reflect_at(&reflection, None, NOW).unwrap();
    let before = fs::read(&reflection.out).unwrap();
    reflect_at(&reflection, None, NOW).unwrap();
    assert_eq!(before, fs::read(&reflection.out).unwrap());
    reflection.if_stale = true;
    assert!(reflect_at(&reflection, None, NOW).unwrap().skipped);
    args.answer = Some("An updated answer.".into());
    save_result_at(&args, None, NOW).unwrap();
    assert!(!reflect_at(&reflection, None, NOW).unwrap().skipped);
    assert!(!reflect_at(&reflection, None, NOW + DAY).unwrap().skipped);
    assert!(reflect_at(&reflection, None, NOW + DAY).unwrap().skipped);
}

#[test]
fn memory_graphify_utc_documents_resolve_only_unique_labels_without_inventing_identity() {
    let dir = tempfile::tempdir().unwrap();
    let graph = graph(dir.path());
    let args = save_args(dir.path());
    fs::create_dir_all(&args.memory_dir).unwrap();
    let text = "---\ntype: \"query\"\ndate: \"2024-10-04T00:00:00.123456+00:00\"\nquestion: \"Where?\"\ncontributor: \"graphify\"\noutcome: \"useful\"\nsource_nodes: [\"node:parse\", \"parse\"]\n---\n\n# Q: Where?\n\n## Answer\n\nUse parse.\n\n## Outcome\n\n- Signal: useful\n";
    fs::write(
        args.memory_dir.join("legacy.md"),
        text.replace('\n', "\r\n"),
    )
    .unwrap();
    let records = load_records(&args.memory_dir).unwrap();
    assert_eq!(records[0].created_unix_secs, NOW);
    assert_eq!(records[0].answer, "Use parse.");
    assert_eq!(records[0].contributor, "graphify");
    let reflection = reflect_args(dir.path());
    reflect_at(&reflection, Some(&graph), NOW).unwrap();
    let text = fs::read_to_string(reflection.out).unwrap();
    assert!(text.contains("unverified (no saved source fingerprint)"));
    assert!(section(&text, "tentative").contains("useful=1"));
    assert!(!text.contains("parse | stale (node missing)"));
}

#[test]
fn memory_rejects_oversize_invalid_inputs_and_preserves_existing_files() {
    let dir = tempfile::tempdir().unwrap();
    let mut args = save_args(dir.path());
    args.answer = Some("x".repeat(MAX_TEXT_BYTES + 1));
    assert!(save_result_at(&args, None, NOW).is_err());
    assert!(!args.memory_dir.exists());
    args.answer = None;
    assert!(save_result_at(&args, None, NOW).is_err());
    args.answer = Some("valid".into());
    args.correction = Some("not a corrected outcome".into());
    assert!(save_result_at(&args, None, NOW).is_err());
    args.correction = None;
    let path = save_result_at(&args, None, NOW).unwrap();
    let bytes = fs::read(&path).unwrap();
    let mut reflection = reflect_args(dir.path());
    reflection.out = path.clone();
    assert!(reflect_at(&reflection, None, NOW).is_err());
    assert_eq!(bytes, fs::read(path).unwrap());
    reflection = reflect_args(dir.path());
    reflection.half_life_days = f64::NAN;
    assert!(reflect_at(&reflection, None, NOW).is_err());
    reflection.half_life_days = 30.0;
    reflection.min_corroboration = 0;
    assert!(reflect_at(&reflection, None, NOW).is_err());
}

#[cfg(unix)]
#[test]
fn memory_rejects_symlinks_devices_and_outside_root_source_evidence() {
    use std::os::unix::fs::symlink;
    let dir = tempfile::tempdir().unwrap();
    let mut args = save_args(dir.path());
    let text = dir.path().join("text");
    let link = dir.path().join("link");
    fs::write(&text, "answer").unwrap();
    symlink(&text, &link).unwrap();
    args.answer = None;
    args.answer_file = Some(link);
    assert!(save_result_at(&args, None, NOW).is_err());
    args.answer_file = Some("/dev/zero".into());
    assert!(save_result_at(&args, None, NOW).is_err());
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("outside.rs"), "private").unwrap();
    let mut graph = graph(dir.path());
    graph.nodes[0].file = outside
        .path()
        .join("outside.rs")
        .to_string_lossy()
        .into_owned();
    args = save_args(dir.path());
    args.nodes.push("node:parse".into());
    save_result_at(&args, Some(&graph), NOW).unwrap();
    assert!(
        load_records(&args.memory_dir).unwrap()[0].evidence[0]
            .source_hash
            .is_none()
    );
    let reflection = reflect_args(dir.path());
    symlink(
        outside.path().join("outside.rs"),
        args.memory_dir.join("linked.md"),
    )
    .unwrap();
    assert!(reflect_at(&reflection, Some(&graph), NOW).is_err());
}

#[derive(Parser)]
struct SaveCli {
    #[command(flatten)]
    args: SaveResultArgs,
}

#[test]
fn memory_clap_requires_exactly_one_answer_and_accepts_upstream_outcomes() {
    assert!(SaveCli::try_parse_from(["test", "--question", "Q"]).is_err());
    assert!(
        SaveCli::try_parse_from([
            "test",
            "--question",
            "Q",
            "--answer",
            "A",
            "--answer-file",
            "file"
        ])
        .is_err()
    );
    for outcome in ["useful", "dead_end", "corrected"] {
        assert!(
            SaveCli::try_parse_from([
                "test",
                "--question",
                "Q",
                "--answer",
                "A",
                "--outcome",
                outcome
            ])
            .is_ok()
        );
    }
    assert!(
        SaveCli::try_parse_from([
            "test",
            "--question",
            "Q",
            "--answer",
            "A",
            "--outcome",
            "wrong"
        ])
        .is_err()
    );
}

#[test]
fn reflection_empty_corpus_foreign_docs_and_future_events_are_explicit() {
    let dir = tempfile::tempdir().unwrap();
    let mut reflection = reflect_args(dir.path());
    assert_eq!(reflect_at(&reflection, None, NOW).unwrap().records, 0);
    let graph = graph(dir.path());
    let mut args = save_args(dir.path());
    args.nodes.push("node:parse".into());
    save_result_at(&args, Some(&graph), NOW + DAY).unwrap();
    fs::write(args.memory_dir.join("foreign.md"), "# unrelated notes\n").unwrap();
    reflection.if_stale = true;
    assert_eq!(
        reflect_at(&reflection, Some(&graph), NOW).unwrap().records,
        1
    );
    let text = fs::read_to_string(&reflection.out).unwrap();
    assert!(!section(&text, "tentative").contains("node:parse"));
    assert!(section(&text, "Event provenance").contains("Where is parsing?"));
    reflect_at(&reflection, Some(&graph), NOW + DAY).unwrap();
    assert!(
        section(&fs::read_to_string(&reflection.out).unwrap(), "tentative").contains("node:parse")
    );
    fs::write(
        args.memory_dir.join("broken.md"),
        "---\ncontributor: graf\n",
    )
    .unwrap();
    assert!(reflect_at(&reflection, Some(&graph), NOW).is_err());
}

#[test]
fn memory_bounded_file_reads_reject_large_or_non_utf8_answers() {
    let dir = tempfile::tempdir().unwrap();
    let mut args = save_args(dir.path());
    let answer = dir.path().join("answer.txt");
    args.answer = None;
    args.answer_file = Some(answer.clone());
    fs::write(&answer, vec![b'x'; MAX_TEXT_BYTES + 1]).unwrap();
    assert!(save_result_at(&args, None, NOW).is_err());
    fs::write(&answer, [0xff, 0xfe]).unwrap();
    assert!(save_result_at(&args, None, NOW).is_err());
    fs::write(&answer, "the ordinary case").unwrap();
    save_result_at(&args, None, NOW).unwrap();
    assert_eq!(
        load_records(&args.memory_dir).unwrap()[0].answer,
        "the ordinary case"
    );
}

#[test]
fn reflection_if_stale_detects_source_changes_and_memory_deletions() {
    let dir = tempfile::tempdir().unwrap();
    let graph = graph(dir.path());
    let mut args = save_args(dir.path());
    args.nodes.push("node:parse".into());
    let path = save_result_at(&args, Some(&graph), NOW).unwrap();
    let mut reflection = reflect_args(dir.path());
    reflection.if_stale = true;
    reflect_at(&reflection, Some(&graph), NOW).unwrap();
    assert!(reflect_at(&reflection, Some(&graph), NOW).unwrap().skipped);
    fs::write(dir.path().join("source.rs"), "changed bytes\n").unwrap();
    assert!(!reflect_at(&reflection, Some(&graph), NOW).unwrap().skipped);
    fs::remove_file(path).unwrap();
    assert!(!reflect_at(&reflection, Some(&graph), NOW).unwrap().skipped);
    assert_eq!(
        reflect_at(&reflection, Some(&graph), NOW).unwrap().records,
        0
    );
}

#[test]
fn memory_conflicting_event_copies_are_rejected_without_overwriting_lessons() {
    let dir = tempfile::tempdir().unwrap();
    let args = save_args(dir.path());
    save_result_at(&args, None, NOW).unwrap();
    let reflection = reflect_args(dir.path());
    reflect_at(&reflection, None, NOW).unwrap();
    let original = fs::read(&reflection.out).unwrap();
    let mut record = load_records(&args.memory_dir).unwrap().remove(0);
    record.answer = "different event content with the same ID".into();
    fs::write(
        args.memory_dir.join("conflict.md"),
        format!("---\n{}---\n", serde_yaml_ng::to_string(&record).unwrap()),
    )
    .unwrap();
    assert!(reflect_at(&reflection, None, NOW).is_err());
    assert_eq!(original, fs::read(reflection.out).unwrap());
}

#[test]
fn memory_actual_index_proof_promotes_python_rust_and_document_events() {
    let dir = tempfile::tempdir().unwrap();
    let graph = indexed_graph(dir.path());
    let mut args = save_args(dir.path());
    for file in ["source.py", "source.rs", "notes.md"] {
        let raw = fs::read(dir.path().join("project").join(file)).unwrap();
        let digest = blake3::hash(&raw).to_hex().to_string();
        assert_eq!(graph.metadata["graf_source_digests"]["files"][file], digest);
        args.nodes.push(source_node(&graph, file));
    }
    let first = save_result_at(&args, Some(&graph), NOW).unwrap();
    save_result_at(&args, Some(&graph), NOW + 1).unwrap();
    let saved = load_records(&args.memory_dir).unwrap();
    for evidence in &saved[0].evidence {
        assert!(evidence.indexed_source_hash.is_some());
        assert_eq!(evidence.indexed_source_hash, evidence.source_hash);
    }
    let reflection = reflect_args(dir.path());
    let overlay = learning_overlay_at(&reflection, Some(&graph), NOW + 1).unwrap();
    for id in &args.nodes {
        let node = &overlay.nodes[id];
        assert_eq!(node.status, "preferred");
        assert_eq!(
            (node.useful, node.verified_useful, node.unverified),
            (2, 2, 0)
        );
        assert!(node.reason.contains("2x current"));
    }
    assert!(overlay.lessons.contains("verified_useful=2"));
    assert!(section(&overlay.lessons, "preferred").contains(&args.nodes[0]));
    assert!(first.is_file());
    // An unrelated reindex changes generation, not the identity of unchanged evidence.
    fs::write(dir.path().join("project/unrelated.rs"), "fn other() {}\n").unwrap();
    let later = reindex(dir.path());
    assert!(later.generation > graph.generation);
    let later_overlay = learning_overlay_at(&reflection, Some(&later), NOW + 1).unwrap();
    for id in &args.nodes {
        assert_eq!(later_overlay.nodes[id].status, "preferred");
    }
    assert_ne!(overlay.snapshot_hash, later_overlay.snapshot_hash);
}

#[test]
fn memory_native_proof_before_save_mismatch_stays_stale_without_graph_or_after_restore() {
    let dir = tempfile::tempdir().unwrap();
    let graph = indexed_graph(dir.path());
    let path = dir.path().join("project/source.py");
    let original = fs::read(&path).unwrap();
    let mut args = save_args(dir.path());
    let id = source_node(&graph, "source.py");
    args.nodes.push(id.clone());
    fs::write(&path, "def replacement():\n    return 2\n").unwrap();
    save_result_at(&args, Some(&graph), NOW).unwrap();
    save_result_at(&args, Some(&graph), NOW + 1).unwrap();
    let record = load_records(&args.memory_dir).unwrap().remove(0);
    assert!(record.evidence[0].indexed_source_hash.is_some());
    assert_ne!(
        record.evidence[0].indexed_source_hash,
        record.evidence[0].source_hash
    );
    let reflection = reflect_args(dir.path());
    let mut no_proof = graph.clone();
    no_proof
        .metadata
        .as_object_mut()
        .unwrap()
        .remove("graf_source_digests");
    for current in [Some(&graph), Some(&no_proof), None] {
        let overlay = learning_overlay_at(&reflection, current, NOW + 1).unwrap();
        let node = &overlay.nodes[&id];
        assert_eq!(node.status, "stale");
        assert_eq!((node.useful, node.verified_useful, node.score), (0, 0, 0.0));
        assert!(
            node.reason
                .contains("source differed from indexed bytes when saved")
        );
        assert!(!section(&overlay.lessons, "preferred").contains(&id));
        assert!(!section(&overlay.lessons, "tentative").contains(&id));
    }
    fs::write(&path, original).unwrap();
    let overlay = learning_overlay_at(&reflection, Some(&graph), NOW + 1).unwrap();
    assert_eq!(overlay.nodes[&id].status, "stale");
    assert!(
        overlay.nodes[&id]
            .reason
            .contains("source differed from indexed bytes when saved")
    );
    // New genuinely matching events can corroborate without rehabilitating old ones.
    save_result_at(&args, Some(&graph), NOW + 2).unwrap();
    save_result_at(&args, Some(&graph), NOW + 3).unwrap();
    let overlay = learning_overlay_at(&reflection, Some(&graph), NOW + 3).unwrap();
    assert_eq!(overlay.nodes[&id].status, "preferred");
    assert_eq!(
        (
            overlay.nodes[&id].useful,
            overlay.nodes[&id].verified_useful
        ),
        (2, 2)
    );
    assert!(overlay.nodes[&id].reason.contains("excluded stale=2"));
}

#[test]
fn memory_native_proof_after_save_edits_deletion_node_root_and_index_changes_stay_stale() {
    let dir = tempfile::tempdir().unwrap();
    let graph = indexed_graph(dir.path());
    let path = dir.path().join("project/source.py");
    let original = fs::read(&path).unwrap();
    let mut args = save_args(dir.path());
    let id = source_node(&graph, "source.py");
    args.nodes.push(id.clone());
    save_result_at(&args, Some(&graph), NOW).unwrap();
    let reflection = reflect_args(dir.path());
    let overlay = learning_overlay_at(&reflection, Some(&graph), NOW).unwrap();
    assert_eq!(overlay.nodes[&id].verified_useful, 1);
    assert_eq!(overlay.nodes[&id].status, "tentative");
    fs::write(&path, "def replacement():\n    return 2\n").unwrap();
    let overlay = learning_overlay_at(&reflection, Some(&graph), NOW).unwrap();
    assert_eq!(overlay.nodes[&id].status, "stale");
    assert!(overlay.nodes[&id].reason.contains("source changed"));
    fs::remove_file(&path).unwrap();
    let overlay = learning_overlay_at(&reflection, Some(&graph), NOW).unwrap();
    assert!(overlay.nodes[&id].reason.contains("source unavailable"));
    fs::write(&path, &original).unwrap();
    let mut changed = graph.clone();
    changed.nodes.iter_mut().find(|n| n.id == id).unwrap().label = "other".into();
    let overlay = learning_overlay_at(&reflection, Some(&changed), NOW).unwrap();
    assert!(overlay.nodes[&id].reason.contains("node changed"));
    changed = graph.clone();
    changed.root = Some(dir.path().join("other").to_string_lossy().into_owned());
    let overlay = learning_overlay_at(&reflection, Some(&changed), NOW).unwrap();
    assert!(overlay.nodes[&id].reason.contains("graph identity changed"));
    // Keep the cited node's shape identical, while changing bytes elsewhere in its file.
    let mut edited = original.clone();
    edited.extend_from_slice(b"\n# changed file revision\n");
    fs::write(&path, edited).unwrap();
    let changed = reindex(dir.path());
    fs::write(&path, &original).unwrap();
    let overlay = learning_overlay_at(&reflection, Some(&changed), NOW).unwrap();
    assert_eq!(overlay.nodes[&id].status, "stale");
    assert!(
        overlay.nodes[&id].reason.contains("indexed source changed")
            || overlay.nodes[&id].reason.contains("node changed")
    );
}

#[test]
fn memory_old_yaml_neither_supplies_nor_vetoes_verified_corroboration() {
    let dir = tempfile::tempdir().unwrap();
    let graph = indexed_graph(dir.path());
    let mut args = save_args(dir.path());
    let id = source_node(&graph, "source.py");
    args.nodes.push(id.clone());
    let old_path = save_result_at(&args, Some(&graph), NOW).unwrap();
    // Old YAML predates indexed_source_hash. Remove the actual key, not just its value.
    let text = fs::read_to_string(&old_path).unwrap();
    let old_text = text
        .lines()
        .filter(|line| !line.trim_start().starts_with("indexed_source_hash:"))
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    fs::write(&old_path, old_text).unwrap();
    assert!(
        load_records(&args.memory_dir).unwrap()[0].evidence[0]
            .indexed_source_hash
            .is_none()
    );
    save_result_at(&args, Some(&graph), NOW + 1).unwrap();
    let reflection = reflect_args(dir.path());
    let overlay = learning_overlay_at(&reflection, Some(&graph), NOW + 2).unwrap();
    let node = &overlay.nodes[&id];
    assert_eq!(
        (node.useful, node.verified_useful, node.unverified),
        (2, 1, 1)
    );
    assert_eq!(node.status, "tentative");
    save_result_at(&args, Some(&graph), NOW + 2).unwrap();
    let overlay = learning_overlay_at(&reflection, Some(&graph), NOW + 2).unwrap();
    let node = &overlay.nodes[&id];
    assert_eq!(
        (node.useful, node.verified_useful, node.unverified),
        (3, 2, 1)
    );
    assert_eq!(node.status, "preferred");
    assert!(node.reason.contains("1x unverified"));
    assert!(
        overlay
            .lessons
            .contains("mixed current and unverified observations")
    );
    let mut without_proof = graph.clone();
    without_proof
        .metadata
        .as_object_mut()
        .unwrap()
        .remove("graf_source_digests");
    args.outcome = Some(Outcome::Corrected);
    args.correction = Some("The result only handles this input.".into());
    save_result_at(&args, Some(&without_proof), NOW + 3).unwrap();
    let overlay = learning_overlay_at(&reflection, Some(&graph), NOW + 3).unwrap();
    let node = &overlay.nodes[&id];
    assert_eq!(node.status, "contested");
    assert_eq!(
        (node.verified_useful, node.negative, node.unverified),
        (2, 1, 2)
    );
    assert!(section(&overlay.lessons, "corrected").contains("Original answer: Use parse."));
    assert!(section(&overlay.lessons, "corrected").contains("The result only handles this input."));
}

#[test]
fn memory_learning_overlay_is_read_only_serializable_and_matches_reflection() {
    let dir = tempfile::tempdir().unwrap();
    let reflection = reflect_args(dir.path());
    let empty = learning_overlay_at(&reflection, None, NOW).unwrap();
    assert!(empty.nodes.is_empty());
    assert!(empty.snapshot_hash.is_none());
    assert!(!reflection.memory_dir.exists());
    assert!(!reflection.out.parent().unwrap().exists());
    let graph = indexed_graph(dir.path());
    let graph_bytes = serde_json::to_vec(&graph).unwrap();
    let mut args = save_args(dir.path());
    args.nodes.push(source_node(&graph, "source.py"));
    let saved = save_result_at(&args, Some(&graph), NOW).unwrap();
    let record_bytes = fs::read(&saved).unwrap();
    let overlay = learning_overlay_at(&reflection, Some(&graph), NOW).unwrap();
    assert_eq!(overlay.schema_version, 1);
    assert_eq!(overlay.generated_unix_secs, NOW);
    assert_eq!(
        overlay.snapshot_hash,
        Some(blake3::hash(&graph_bytes).to_hex().to_string())
    );
    assert!(!reflection.out.parent().unwrap().exists());
    assert_eq!(fs::read(&saved).unwrap(), record_bytes);
    assert_eq!(fs::read_dir(&args.memory_dir).unwrap().count(), 1);
    assert_eq!(serde_json::to_vec(&graph).unwrap(), graph_bytes);
    let serialized = serde_json::to_vec(&overlay).unwrap();
    let decoded: LearningOverlay = serde_json::from_slice(&serialized).unwrap();
    assert_eq!(serde_json::to_vec(&decoded.clone()).unwrap(), serialized);
    assert_eq!(
        serde_json::to_vec(&learning_overlay_at(&reflection, Some(&graph), NOW).unwrap()).unwrap(),
        serialized
    );
    reflect_at(&reflection, Some(&graph), NOW).unwrap();
    assert_eq!(
        fs::read_to_string(&reflection.out).unwrap(),
        overlay.lessons
    );
    // Read projection must not read or attempt publication to its ignored output path.
    let mut ignored_output = reflection.clone();
    ignored_output.out = saved.clone();
    ignored_output.if_stale = true;
    assert_eq!(
        serde_json::to_vec(&learning_overlay_at(&ignored_output, Some(&graph), NOW).unwrap())
            .unwrap(),
        serialized
    );
    assert_eq!(fs::read(saved).unwrap(), record_bytes);
}

#[test]
fn memory_overlay_keeps_stale_dead_end_and_corrected_nodes_without_graph_mutation() {
    let dir = tempfile::tempdir().unwrap();
    let graph = indexed_graph(dir.path());
    let original_graph = serde_json::to_vec(&graph).unwrap();
    let mut args = save_args(dir.path());
    let dead_end = source_node(&graph, "source.py");
    let corrected = source_node(&graph, "source.rs");
    let stale = source_node(&graph, "notes.md");
    args.nodes = vec![dead_end.clone()];
    args.outcome = Some(Outcome::DeadEnd);
    save_result_at(&args, Some(&graph), NOW).unwrap();
    args.nodes = vec![corrected.clone()];
    args.outcome = Some(Outcome::Corrected);
    args.correction = Some("Check the caller too.".into());
    save_result_at(&args, Some(&graph), NOW).unwrap();
    args.nodes = vec![stale.clone()];
    args.outcome = Some(Outcome::Useful);
    args.correction = None;
    save_result_at(&args, Some(&graph), NOW).unwrap();
    fs::write(dir.path().join("project/notes.md"), "# New notes\n").unwrap();
    let overlay = learning_overlay_at(&reflect_args(dir.path()), Some(&graph), NOW).unwrap();
    assert_eq!(overlay.nodes[&dead_end].status, "dead_end");
    assert_eq!(overlay.nodes[&dead_end].negative, 1);
    assert_eq!(overlay.nodes[&dead_end].score, -1.0);
    assert_eq!(overlay.nodes[&corrected].status, "corrected");
    assert_eq!(overlay.nodes[&stale].status, "stale");
    assert_eq!(overlay.nodes[&stale].score, 0.0);
    assert!(overlay.nodes[&stale].reason.contains("excluded stale=1"));
    assert!(section(&overlay.lessons, "dead_end").contains("Where is parsing?"));
    assert!(section(&overlay.lessons, "corrected").contains("Check the caller too."));
    assert_eq!(serde_json::to_vec(&graph).unwrap(), original_graph);
}

#[test]
fn memory_native_proof_malformed_missing_imported_and_nonexact_paths_remain_unverified() {
    let dir = tempfile::tempdir().unwrap();
    let graph = indexed_graph(dir.path());
    let id = source_node(&graph, "source.py");
    let proof = graph.metadata["graf_source_digests"].clone();
    let digest = proof["files"]["source.py"].as_str().unwrap();
    let malformed = [
        json!(null),
        json!({"algorithm":"sha256", "files":{"source.py":digest}}),
        json!({"algorithm":"blake3", "files":[]}),
        json!({"algorithm":"blake3", "files":{"source.py":"not-a-digest"}}),
        json!({"algorithm":"blake3", "files":{"source.py":"F".repeat(64)}}),
        json!({"algorithm":"blake3", "files":{"other/source.py":digest}}),
        json!({"algorithm":"blake3", "files":{"./source.py":digest}}),
        json!({"algorithm":"blake3", "files":{"SOURCE.py":digest}}),
    ];
    for value in malformed {
        let case = tempfile::tempdir().unwrap();
        let mut changed = graph.clone();
        changed.metadata["graf_source_digests"] = value;
        let mut args = save_args(case.path());
        args.nodes.push(id.clone());
        save_result_at(&args, Some(&changed), NOW).unwrap();
        save_result_at(&args, Some(&changed), NOW).unwrap();
        assert!(
            load_records(&args.memory_dir).unwrap()[0].evidence[0]
                .indexed_source_hash
                .is_none()
        );
        // Restoring good current metadata cannot backfill the original missing proof.
        let overlay = learning_overlay_at(&reflect_args(case.path()), Some(&graph), NOW).unwrap();
        assert_eq!(overlay.nodes[&id].status, "tentative");
        assert_eq!(
            (
                overlay.nodes[&id].verified_useful,
                overlay.nodes[&id].unverified
            ),
            (0, 2)
        );
    }
    for kind in ["imported", "composed"] {
        let case = tempfile::tempdir().unwrap();
        let mut changed = graph.clone();
        changed.kind = kind.into();
        let mut args = save_args(case.path());
        args.nodes.push(id.clone());
        save_result_at(&args, Some(&changed), NOW).unwrap();
        assert!(
            load_records(&args.memory_dir).unwrap()[0].evidence[0]
                .indexed_source_hash
                .is_none()
        );
        let overlay = learning_overlay_at(&reflect_args(case.path()), Some(&changed), NOW).unwrap();
        assert_eq!(overlay.nodes[&id].verified_useful, 0);
        assert_eq!(overlay.nodes[&id].unverified, 1);
    }
    // Good saved proof cannot stay verified if the current metadata is removed.
    let mut args = save_args(dir.path());
    args.nodes.push(id.clone());
    save_result_at(&args, Some(&graph), NOW).unwrap();
    let mut missing = graph.clone();
    missing
        .metadata
        .as_object_mut()
        .unwrap()
        .remove("graf_source_digests");
    let overlay = learning_overlay_at(&reflect_args(dir.path()), Some(&missing), NOW).unwrap();
    assert_eq!(overlay.nodes[&id].status, "tentative");
    assert_eq!(overlay.nodes[&id].verified_useful, 0);
}
