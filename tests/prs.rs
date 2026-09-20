use clap::Parser;
use graf::{
    model::{GraphSnapshot, Node, SCHEMA_VERSION},
    prs::{self, CiStatus, PrsArgs, PrsData, PrsRuntime},
};
use serde_json::{Value, json};

const NOW: u64 = 1_789_776_000; // 2026-09-19T00:00:00Z

#[derive(Parser)]
struct Cli {
    #[command(flatten)]
    args: PrsArgs,
}

fn pr(number: u64) -> Value {
    json!({"number":number,"title":"Unicode فارسی 🦀","state":"OPEN",
        "headRefName":format!("topic-{number}"),"baseRefName":"main","author":{"login":"contributor"},
        "isDraft":false,"isCrossRepository":false,"reviewDecision":"APPROVED",
        "statusCheckRollup":[{"status":"COMPLETED","conclusion":"SUCCESS"}],
        "updatedAt":"2026-09-18T00:00:00Z","files":[{"path":"src/auth.rs"}],"changedFiles":1})
}

fn data(prs: Vec<Value>) -> PrsData {
    serde_json::from_value(
        json!({"repo":"example/project","expected_base":"main","prs":prs,
        "list_may_be_truncated":false,"worktrees":[],"worktrees_available":false,"notices":[]}),
    )
    .unwrap()
}

fn args() -> PrsArgs {
    PrsArgs {
        repo: Some("example/project".into()),
        base: Some("main".into()),
        ..Default::default()
    }
}

fn node(id: &str, path: &str, metadata: Value) -> Node {
    Node {
        id: id.into(),
        label: id.into(),
        kind: "function".into(),
        file: path.into(),
        line: Some(1),
        end_line: Some(2),
        qualified_name: None,
        binding_key: None,
        metadata,
    }
}

fn graph(nodes: Vec<Node>) -> GraphSnapshot {
    GraphSnapshot {
        schema_version: SCHEMA_VERSION,
        generation: 7,
        kind: "imported".into(),
        root: Some("/synthetic/project".into()),
        nodes,
        edges: vec![],
        metadata: json!({"source":"synthetic"}),
    }
}

#[test]
fn clap_surface_accepts_hash_number_and_rejects_invalid_numbers() {
    let cli = Cli::try_parse_from([
        "prs",
        "#42",
        "-R",
        "example/project",
        "-b",
        "release/v1",
        "--triage",
        "--worktrees",
        "--conflicts",
        "--wrong-base",
    ])
    .unwrap();
    assert_eq!(cli.args.number, Some(42));
    assert!(cli.args.triage && cli.args.worktrees && cli.args.conflicts && cli.args.wrong_base);
    for value in ["0", "#0", "-1", "12x", "4294967296"] {
        assert!(Cli::try_parse_from(["prs", value]).is_err());
    }
}

#[test]
fn invalid_repositories_and_fixture_identity_fail_before_commands() {
    for repo in [
        "--help",
        "https://github.com/a/b",
        "a/b/c",
        "a/../b",
        "a/b;echo",
        "a/b\n",
        "a/-b",
        "a/{input}",
        "/b",
    ] {
        let a = PrsArgs {
            repo: Some(repo.into()),
            ..Default::default()
        };
        let runtime = PrsRuntime {
            gh_program: "missing-should-never-start".into(),
            ..Default::default()
        };
        let error = prs::load(&a, &runtime).unwrap_err().to_string();
        assert!(error.contains("repository"), "{repo}: {error}");
    }
    let mut fixture = data(vec![pr(1)]);
    fixture.repo = "other/project".into();
    assert!(prs::report(&args(), fixture, None, NOW).is_err());
}

#[test]
fn factual_status_queue_is_deterministic_and_keeps_all_reasons() {
    let mut wrong = pr(1);
    wrong["baseRefName"] = json!("release");
    wrong["isDraft"] = json!(true);
    let mut failure = pr(2);
    failure["statusCheckRollup"] = json!([{"conclusion":"FAILURE"}]);
    let mut changes = pr(3);
    changes["reviewDecision"] = json!("CHANGES_REQUESTED");
    let mut draft = pr(4);
    draft["isDraft"] = json!(true);
    draft["updatedAt"] = json!("2020-01-01T00:00:00Z");
    let mut stale = pr(5);
    stale["updatedAt"] = json!("2026-09-05T00:00:00Z");
    let mut pending = pr(6);
    pending["statusCheckRollup"] = json!([{"status":"IN_PROGRESS"}]);
    let mut none = pr(7);
    none["statusCheckRollup"] = json!([]);
    let input = data(vec![
        pr(8),
        none,
        pending,
        stale,
        draft,
        changes,
        failure,
        wrong,
    ]);
    let a = PrsArgs {
        wrong_base: true,
        triage: true,
        ..args()
    };
    let r = prs::report(&a, input.clone(), None, NOW).unwrap();
    assert_eq!(
        r.entries
            .iter()
            .map(|p| p.status.as_str())
            .collect::<Vec<_>>(),
        vec![
            "WRONG-BASE",
            "CI-FAIL",
            "CHANGES-REQ",
            "DRAFT",
            "STALE",
            "PENDING",
            "NO-CHECKS",
            "APPROVED"
        ]
    );
    assert!(r.entries[0].reasons.iter().any(|s| s == "Draft PR"));
    assert!(r.entries[5].pr.review_decision.as_deref() == Some("APPROVED"));
    let again = prs::report(&a, input, None, NOW).unwrap();
    assert_eq!(
        serde_json::to_value(r).unwrap(),
        serde_json::to_value(again).unwrap()
    );
}

#[test]
fn ci_handles_both_github_check_types_and_unknowns() {
    for (checks, expected) in [
        (json!([]), CiStatus::None),
        (Value::Null, CiStatus::Unknown),
        (json!([{"state":"SUCCESS"}]), CiStatus::Success),
        (
            json!([{"state":"ERROR"},{"conclusion":"SUCCESS"}]),
            CiStatus::Failure,
        ),
        (
            json!([{"state":"PENDING"},{"conclusion":"SUCCESS"}]),
            CiStatus::Pending,
        ),
        (
            json!([{"status":"COMPLETED","conclusion":"CANCELLED"}]),
            CiStatus::Failure,
        ),
        (
            json!([{"conclusion":"NEUTRAL"},{"conclusion":"SKIPPED"}]),
            CiStatus::Success,
        ),
        (json!([{"status":"COMPLETED"}]), CiStatus::Unknown),
    ] {
        let mut p = pr(1);
        p["statusCheckRollup"] = checks;
        let r = prs::report(&args(), data(vec![p]), None, NOW).unwrap();
        assert_eq!(r.entries[0].ci, expected);
    }
}

#[test]
fn wrong_base_filter_single_closed_detail_and_missing_graph_are_honest() {
    let mut p = pr(1);
    p["baseRefName"] = json!("release");
    let r = prs::report(&args(), data(vec![p.clone()]), None, NOW).unwrap();
    assert_eq!(r.hidden_wrong_base, 1);
    assert!(r.entries.is_empty());
    assert!(!r.graph_available);
    p["state"] = json!("MERGED");
    let a = PrsArgs {
        number: Some(1),
        ..args()
    };
    let r = prs::report(&a, data(vec![p]), None, NOW).unwrap();
    assert_eq!(r.entries[0].status, "MERGED");
    assert!(r.entries[0].impact.is_none());
    assert!(prs::format_text(&r).contains("Graph unavailable"));
    assert!(prs::report(&a, data(vec![]), None, NOW).is_err());
}

#[test]
fn age_validation_handles_calendar_boundary_and_future_dates() {
    for date in [
        "not-a-date",
        "2026-02-30T00:00:00Z",
        "2026-09-20T00:00:00Z",
        "2026-09-18T25:00:00Z",
    ] {
        let mut p = pr(1);
        p["updatedAt"] = json!(date);
        let r = prs::report(&args(), data(vec![p]), None, NOW).unwrap();
        assert_eq!(r.entries[0].age_days, None);
        assert_ne!(r.entries[0].status, "STALE");
    }
    let r = prs::report(&args(), data(vec![pr(1)]), None, NOW).unwrap();
    assert_eq!(r.entries[0].age_days, Some(1));
}

#[test]
fn impact_uses_exact_boundaries_deduplicates_and_retains_node_provenance() {
    let meta = json!({"source_file":"src/auth.rs","community":99,"origin":"fixture"});
    let g = graph(vec![
        node("auth", "src/auth.rs", meta.clone()),
        node("auth2", "src/auth.rs", json!({})),
        node("other", "src/other/auth.rs", json!({})),
        node("suffix", "src/myauth.rs", json!({})),
    ]);
    let mut p = pr(1);
    p["files"] = json!([{"path":"src/auth.rs"},{"path":"src/auth.rs"},{"path":"docs/文.md"}]);
    p["changedFiles"] = json!(2);
    let r = prs::report(&args(), data(vec![p]), Some(&g), NOW).unwrap();
    let impact = r.entries[0].impact.as_ref().unwrap();
    assert_eq!(
        impact
            .nodes
            .iter()
            .map(|n| n.id.as_str())
            .collect::<Vec<_>>(),
        ["auth", "auth2"]
    );
    assert_eq!(impact.nodes[0].metadata, meta);
    assert_eq!(impact.unmatched_files, ["docs/文.md"]);
    assert_eq!(impact.generation, 7);
    assert!(r.entries[0].files_complete);
}

#[test]
fn ambiguous_suffixes_and_composed_sources_are_not_guessed() {
    let g = graph(vec![
        node(
            "a",
            "src/auth.rs",
            json!({"project":"one","original_id":"auth","original_metadata":{"community":1}}),
        ),
        node(
            "b",
            "src/auth.rs",
            json!({"project":"two","original_id":"auth","original_metadata":{"community":1}}),
        ),
    ]);
    let r = prs::report(&args(), data(vec![pr(1)]), Some(&g), NOW).unwrap();
    let impact = r.entries[0].impact.as_ref().unwrap();
    assert!(impact.nodes.is_empty());
    assert_eq!(impact.ambiguous_files, ["src/auth.rs"]);
    let g = graph(vec![
        node("a", "src/auth.rs", json!({})),
        node("b", "tests/auth.rs", json!({})),
    ]);
    let mut p = pr(1);
    p["files"] = json!([{"path":"auth.rs"}]);
    let r = prs::report(&args(), data(vec![p]), Some(&g), NOW).unwrap();
    assert_eq!(
        r.entries[0].impact.as_ref().unwrap().ambiguous_files,
        ["auth.rs"]
    );
}

#[test]
fn unique_suffix_and_graph_root_paths_work_without_partial_filename_matches() {
    let g = graph(vec![
        node("a", "/synthetic/project/src/auth.rs", json!({})),
        node("b", "src/config.rs", json!({})),
    ]);
    let mut p = pr(1);
    p["files"] = json!([{"path":"auth.rs"},{"path":"g.rs"}]);
    p["changedFiles"] = json!(2);
    let r = prs::report(&args(), data(vec![p]), Some(&g), NOW).unwrap();
    let impact = r.entries[0].impact.as_ref().unwrap();
    assert_eq!(impact.nodes.len(), 1);
    assert_eq!(impact.nodes[0].id, "a");
    assert_eq!(impact.unmatched_files, ["g.rs"]);
}

#[test]
fn conflicts_are_exact_file_and_community_overlaps_with_partial_data_warning() {
    let g = graph(vec![node("a", "src/auth.rs", json!({}))]);
    let mut p = pr(2);
    p["changedFiles"] = json!(2);
    let mut wrong = pr(3);
    wrong["baseRefName"] = json!("release");
    let r = prs::report(
        &PrsArgs {
            conflicts: true,
            wrong_base: true,
            ..args()
        },
        data(vec![pr(1), p, wrong]),
        Some(&g),
        NOW,
    )
    .unwrap();
    assert_eq!(r.overlaps.len(), 1);
    assert_eq!(r.overlaps[0].prs, [1, 2]);
    assert_eq!(r.overlaps[0].files, ["src/auth.rs"]);
    assert_eq!(r.overlaps[0].communities.len(), 1);
    assert!(
        !r.entries
            .iter()
            .find(|e| e.pr.number == 2)
            .unwrap()
            .files_complete
    );
    assert!(
        r.notices
            .iter()
            .any(|n| n.contains("not textual conflicts"))
    );
}

#[test]
fn nul_worktrees_preserve_unicode_detached_records_and_fork_safety() {
    let raw = "worktree /synthetic/文 one\0HEAD abc\0branch refs/heads/topic-1\0\0worktree /synthetic/detached\0HEAD def\0detached\0\0worktree /synthetic/two\0HEAD ghi\0branch refs/heads/topic-2\0\0";
    let worktrees = prs::parse_worktrees(raw).unwrap();
    assert_eq!(worktrees.len(), 3);
    assert!(
        worktrees
            .iter()
            .find(|w| w.path.ends_with("detached"))
            .unwrap()
            .branch
            .is_none()
    );
    let mut fork = pr(2);
    fork["isCrossRepository"] = json!(true);
    let mut d = data(vec![pr(1), fork]);
    d.worktrees = worktrees;
    d.worktrees_available = true;
    let r = prs::report(
        &PrsArgs {
            worktrees: true,
            ..args()
        },
        d,
        None,
        NOW,
    )
    .unwrap();
    assert_eq!(r.entries[0].worktrees, ["/synthetic/文 one"]);
    assert!(r.entries[1].worktrees.is_empty());
}

#[test]
fn input_counts_and_paths_are_checked_and_terminal_controls_are_escaped() {
    for path in [
        "../auth.rs",
        "src/../auth.rs",
        "/auth.rs",
        "src//auth.rs",
        "",
    ] {
        let mut p = pr(1);
        p["files"] = json!([{"path":path}]);
        assert!(prs::report(&args(), data(vec![p]), None, NOW).is_err());
    }
    assert!(prs::report(&args(), data((1..=101).map(pr).collect()), None, NOW).is_err());
    let mut p = pr(1);
    p["title"] = json!("فارسی\u{001b}[2J\nforged");
    let r = prs::report(&args(), data(vec![p]), None, NOW).unwrap();
    let text = prs::format_text(&r);
    assert!(text.contains("فارسی"));
    assert!(!text.contains('\u{001b}'));
    assert!(!text.contains("\nforged"));
}

#[test]
fn stored_community_types_and_namespaces_do_not_collapse_in_conflicts() {
    let g = graph(vec![
        node(
            "a",
            "a.rs",
            json!({"project":"one","original_id":"n","original_metadata":{"community":1,"community_name":"integer"}}),
        ),
        node(
            "b",
            "b.rs",
            json!({"project":"one","original_id":"m","original_metadata":{"community":"1","community_name":"string"}}),
        ),
        node(
            "c",
            "c.rs",
            json!({"project":"two","original_id":"n","original_metadata":{"community":1}}),
        ),
        node(
            "d",
            "d.rs",
            json!({"project":"one","original_id":"q","original_metadata":{"community":1}}),
        ),
    ]);
    let input = (1..=4)
        .zip(["a.rs", "b.rs", "c.rs", "d.rs"])
        .map(|(number, path)| {
            let mut p = pr(number);
            p["files"] = json!([{"path":path}]);
            p
        })
        .collect();
    let r = prs::report(
        &PrsArgs {
            conflicts: true,
            ..args()
        },
        data(input),
        Some(&g),
        NOW,
    )
    .unwrap();
    assert_eq!(r.overlaps.len(), 1);
    assert_eq!(r.overlaps[0].prs, [1, 4]);
    let identity = &r.overlaps[0].communities[0];
    assert_eq!(identity.source, "stored");
    assert_eq!(identity.project, ["one"]);
    assert_eq!(identity.id, json!(1));
    assert!(identity.names.contains(&"integer".to_owned()));
    assert_eq!(
        r.entries[1].impact.as_ref().unwrap().communities[0].id,
        json!("1")
    );
}

#[test]
fn json_paths_preserve_newlines_and_absent_memberships_are_explicit() {
    let g = graph(vec![
        node("a", "line\nbreak.rs", json!({"community":"文"})),
        node("b", "plain.rs", json!({})),
    ]);
    let mut p = pr(1);
    p["files"] = json!([{"path":"line\nbreak.rs"},{"path":"plain.rs"}]);
    p["changedFiles"] = json!(2);
    let r = prs::report(&args(), data(vec![p]), Some(&g), NOW).unwrap();
    let impact = r.entries[0].impact.as_ref().unwrap();
    assert_eq!(impact.nodes.len(), 2);
    assert_eq!(impact.nodes_without_community, 1);
    assert_eq!(impact.communities[0].id, json!("文"));
}

#[test]
fn small_unlabeled_graph_still_computes_communities() {
    let g = graph(vec![node(
        "auth",
        "src/auth.rs",
        json!({"origin":"fixture"}),
    )]);
    let r = prs::report(&args(), data(vec![pr(1)]), Some(&g), NOW).unwrap();
    let impact = r.entries[0].impact.as_ref().unwrap();
    assert_eq!(impact.nodes.len(), 1);
    assert_eq!(impact.nodes_without_community, 0);
    assert_eq!(impact.communities.len(), 1);
    assert_eq!(impact.communities[0].source, "computed");
    assert!(
        !r.notices
            .iter()
            .any(|n| n.contains("Computed communities omitted"))
    );
}

#[test]
fn large_unlabeled_graph_retains_complete_node_provenance_and_file_overlaps() {
    let g = graph((0..5_001).map(|i| node(&format!("node-{i:05}"), "src/auth.rs",
        json!({"project":"fixture", "original_id":format!("original-{i}"), "original_metadata":{"origin":"fixture"}}))).collect());
    let r = prs::report(
        &PrsArgs {
            conflicts: true,
            ..args()
        },
        data(vec![pr(1), pr(2)]),
        Some(&g),
        NOW,
    )
    .unwrap();
    assert!(r.graph_available);
    for entry in &r.entries {
        let impact = entry.impact.as_ref().unwrap();
        assert_eq!(
            serde_json::to_value(&impact.nodes).unwrap(),
            serde_json::to_value(&g.nodes).unwrap()
        );
        assert_eq!(impact.graph_metadata, g.metadata);
        assert_eq!(impact.generation, g.generation);
        assert_eq!(impact.nodes_without_community, 5_001);
        assert!(impact.communities.is_empty());
        assert!(impact.unmatched_files.is_empty() && impact.ambiguous_files.is_empty());
        assert!(entry.files_complete);
    }
    assert_eq!(r.overlaps.len(), 1);
    assert_eq!(r.overlaps[0].files, ["src/auth.rs"]);
    assert!(r.overlaps[0].communities.is_empty());
    assert!(
        r.notices
            .iter()
            .any(|n| n.contains("Computed communities omitted") && n.contains("5000 nodes"))
    );
    assert!(prs::format_text(&r).contains("File/node impact remains available"));
}

#[test]
fn unlabeled_graph_edge_reference_and_byte_caps_skip_only_communities() {
    let base = graph(vec![node("auth", "src/auth.rs", json!({}))]);
    let mut edges = base.clone();
    edges.edges = (0..20_001)
        .map(|i| graf::model::Edge {
            id: format!("edge-{i}"),
            source: "auth".into(),
            target: "auth".into(),
            relation: "calls".into(),
            directed: true,
            file: None,
            line: None,
            confidence: "EXTRACTED".into(),
            metadata: json!({}),
        })
        .collect();
    let mut references = base.clone();
    references.metadata["graf_unresolved_references"] =
        json!(vec![json!({"source":"auth"}); 20_001]);
    let mut payload = base.clone();
    payload.metadata["fixture"] = json!("x".repeat(8 * 1024 * 1024));
    for (g, notice) in [
        (edges, "20000 edges"),
        (references, "20000 unresolved references"),
        (payload, "8 MiB"),
    ] {
        let r = prs::report(&args(), data(vec![pr(1)]), Some(&g), NOW).unwrap();
        let impact = r.entries[0].impact.as_ref().unwrap();
        assert_eq!(impact.nodes.len(), 1);
        assert_eq!(impact.nodes_without_community, 1);
        assert_eq!(impact.graph_metadata, g.metadata);
        assert!(impact.communities.is_empty());
        assert!(
            r.notices
                .iter()
                .any(|n| n.contains("Computed communities omitted") && n.contains(notice))
        );
    }
}

#[test]
fn large_preserved_graph_keeps_recorded_communities_without_omission() {
    let g = graph(
        (0..5_001)
            .map(|i| {
                node(
                    &format!("node-{i}"),
                    "src/auth.rs",
                    json!({"community":"retained"}),
                )
            })
            .collect(),
    );
    let r = prs::report(&args(), data(vec![pr(1)]), Some(&g), NOW).unwrap();
    let impact = r.entries[0].impact.as_ref().unwrap();
    assert_eq!(impact.nodes.len(), 5_001);
    assert_eq!(impact.nodes_without_community, 0);
    assert_eq!(impact.communities.len(), 1);
    assert_eq!(impact.communities[0].source, "stored");
    assert_eq!(impact.communities[0].id, json!("retained"));
    assert!(
        !r.notices
            .iter()
            .any(|n| n.contains("Computed communities omitted"))
    );
}

// Executable fixtures run through the production bounded runner. No live gh,
// authentication/config files, process environment mutation or repository writes.
#[cfg(unix)]
mod commands {
    use super::*;
    use std::{fs, os::unix::fs::PermissionsExt, time::Instant};

    fn script(body: &str) -> (tempfile::TempDir, PrsRuntime) {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("gh-fixture");
        fs::write(&file, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o700)).unwrap();
        let runtime = PrsRuntime {
            gh_program: file.to_str().unwrap().into(),
            cwd: temp.path().into(),
            ..Default::default()
        };
        (temp, runtime)
    }

    #[test]
    fn gh_argv_is_read_only_and_stdout_json_is_unicode() {
        let fixture = serde_json::to_string(&vec![pr(1)]).unwrap();
        let body = format!(
            "test \"$1\" = pr && test \"$2\" = list || exit 17\ncase \"$*\" in *'--repo github.com/example/project'*) ;; *) exit 18 ;; esac\nif read -r unexpected; then exit 19; fi\ncat <<'JSON'\n{fixture}\nJSON"
        );
        let (_temp, runtime) = script(&body);
        let loaded = prs::load(&args(), &runtime).unwrap();
        assert_eq!(loaded.prs[0].title, "Unicode فارسی 🦀");
    }

    #[test]
    fn single_pr_view_and_default_base_detection_use_explicit_repository() {
        let body = format!(
            "case \"$1 $2\" in\n'repo view') printf '%s' '{{\"defaultBranchRef\":{{\"name\":\"trunk\"}}}}' ;;\n'pr view') test \"$3\" = 1 || exit 12; cat <<'JSON'\n{}\nJSON\n;;\n*) exit 13 ;;\nesac",
            pr(1)
        );
        let (_temp, runtime) = script(&body);
        let a = PrsArgs {
            number: Some(1),
            base: None,
            ..args()
        };
        let loaded = prs::load(&a, &runtime).unwrap();
        assert_eq!(loaded.expected_base, "trunk");
        assert_eq!(loaded.prs.len(), 1);
    }

    #[test]
    fn authentication_failure_and_invalid_json_do_not_echo_child_output() {
        for body in [
            "printf 'secret-auth-fixture' >&2; exit 4",
            "printf 'secret-auth-fixture'",
            "printf '{\"secret-auth-fixture\":1}'",
        ] {
            let (_temp, runtime) = script(body);
            let error = format!("{:#}", prs::load(&args(), &runtime).unwrap_err());
            assert!(!error.contains("secret-auth-fixture"));
        }
        let runtime = PrsRuntime {
            gh_program: "/nonexistent/graf-gh-fixture".into(),
            ..Default::default()
        };
        assert!(prs::load(&args(), &runtime).is_err());
    }

    #[test]
    fn timeout_and_both_output_caps_are_enforced() {
        let (_temp, mut runtime) = script("sleep 5");
        runtime.timeout_secs = 1;
        let start = Instant::now();
        let error = format!("{:#}", prs::load(&args(), &runtime).unwrap_err());
        assert!(error.contains("timed out"));
        assert!(start.elapsed().as_secs() < 4);
        for body in ["printf '123456789'", "printf '123456789' >&2"] {
            let (_temp, mut runtime) = script(body);
            runtime.max_output_bytes = 8;
            let error = format!("{:#}", prs::load(&args(), &runtime).unwrap_err());
            assert!(error.contains("byte limit"), "{error}");
        }
    }

    #[test]
    fn truncated_lists_and_file_caps_are_not_silent() {
        let body = format!("cat <<'JSON'\n{}\nJSON", json!([pr(1)]));
        let (_temp, mut runtime) = script(&body);
        runtime.max_prs = 1;
        assert!(prs::load(&args(), &runtime).unwrap().list_may_be_truncated);
        let mut p = pr(1);
        p["files"] = json!([{"path":"a"},{"path":"b"}]);
        let (_temp, mut runtime) = script(&format!("cat <<'JSON'\n{}\nJSON", json!([p])));
        runtime.max_files = 1;
        assert!(prs::load(&args(), &runtime).is_err());
    }
    #[test]
    fn inferred_repo_and_worktrees_are_local_read_only_and_repo_scoped() {
        let body = format!("cat <<'JSON'\n{}\nJSON", json!([pr(1)]));
        let (temp, mut runtime) = script(&body);
        let git = temp.path().join("git-fixture");
        fs::write(&git, "#!/bin/sh\ntest \"$1\" = -C || exit 11\nshift 2\ncase \"$1 $2\" in\n'remote get-url') printf '%s' 'git@github.com:example/project.git' ;;\n'worktree list') printf 'worktree /synthetic/文\\0HEAD abc\\0branch refs/heads/topic-1\\0\\0' ;;\n*) exit 12 ;;\nesac\n").unwrap();
        fs::set_permissions(&git, fs::Permissions::from_mode(0o700)).unwrap();
        runtime.git_program = git.to_str().unwrap().into();
        let a = PrsArgs {
            repo: None,
            worktrees: true,
            ..args()
        };
        let d = prs::load(&a, &runtime).unwrap();
        assert_eq!(d.repo, "example/project");
        assert!(d.worktrees_available);
        let r = prs::report(&a, d, None, NOW).unwrap();
        assert_eq!(r.entries[0].worktrees, ["/synthetic/文"]);
        let a = PrsArgs {
            repo: Some("other/project".into()),
            worktrees: true,
            ..args()
        };
        let d = prs::load(&a, &runtime).unwrap();
        assert!(!d.worktrees_available);
        assert!(d.worktrees.is_empty());
        assert!(d.notices.iter().any(|n| n.contains("local origin")));
    }
}
