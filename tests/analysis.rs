use graf::{
    analysis::{AnalysisOptions, analyze},
    model::{Edge, GraphSnapshot, Node},
};
use serde_json::json;
use std::collections::BTreeSet;

fn graph(count: usize, records: &[(usize, usize, f64, bool)]) -> GraphSnapshot {
    GraphSnapshot {
        schema_version: 1,
        generation: 7,
        kind: "imported".into(),
        root: None,
        metadata: json!({}),
        nodes: (0..count)
            .map(|i| Node {
                id: format!("n{i:02}"),
                label: format!("Node {i}"),
                kind: "function".into(),
                file: format!("file{i}.py"),
                line: Some(1),
                end_line: Some(3),
                qualified_name: None,
                binding_key: None,
                metadata: json!({}),
            })
            .collect(),
        edges: records
            .iter()
            .enumerate()
            .map(|(i, &(a, b, w, directed))| Edge {
                id: format!("e{i:03}"),
                source: format!("n{a:02}"),
                target: format!("n{b:02}"),
                relation: "calls".into(),
                directed,
                file: Some(format!("file{a}.py")),
                line: Some(2),
                confidence: "EXTRACTED".into(),
                metadata: json!({"weight":w}),
            })
            .collect(),
    }
}

#[test]
fn pagerank_has_known_stationary_solution_and_weighted_branching() {
    let report = analyze(&graph(2, &[(0, 1, 1.0, true)]), &AnalysisOptions::default()).unwrap();
    assert!(report.pagerank_converged);
    assert!((report.nodes[0].pagerank - 20.0 / 57.0).abs() < 1e-9);
    assert!((report.nodes[1].pagerank - 37.0 / 57.0).abs() < 1e-9);
    let report = analyze(
        &graph(3, &[(0, 1, 3.0, true), (0, 2, 1.0, true)]),
        &AnalysisOptions::default(),
    )
    .unwrap();
    assert!(report.nodes[1].pagerank > report.nodes[2].pagerank);
    assert!((report.nodes.iter().map(|n| n.pagerank).sum::<f64>() - 1.0).abs() < 1e-10);
    assert_eq!(report.file_dependencies.len(), 2);
    assert_eq!(report.call_edges[0].line, Some(2));
}

#[test]
fn degree_counts_mixed_parallel_and_self_edges() {
    let snapshot = graph(
        3,
        &[
            (0, 1, 1.0, true),
            (0, 1, 2.0, false),
            (0, 0, 3.0, false),
            (1, 1, 4.0, true),
        ],
    );
    let report = analyze(&snapshot, &AnalysisOptions::default()).unwrap();
    let a = &report.nodes[0];
    let b = &report.nodes[1];
    assert_eq!((a.degree, a.in_degree, a.out_degree), (4, 3, 4));
    assert_eq!((b.degree, b.in_degree, b.out_degree), (4, 3, 2));
    assert_eq!(a.weighted_degree, 9.0);
    assert_eq!(b.weighted_degree, 11.0);
    assert_eq!(report.nodes[2].degree, 0);
    assert_eq!(report.hubs, vec!["n00", "n01", "n02"]);
    let mut reordered = snapshot.clone();
    reordered.nodes.reverse();
    reordered.edges.reverse();
    assert_eq!(
        serde_json::to_value(&report).unwrap(),
        serde_json::to_value(analyze(&reordered, &AnalysisOptions::default()).unwrap()).unwrap()
    );
}

#[test]
fn communities_separate_dense_cliques_joined_by_a_bridge() {
    let mut records = Vec::new();
    for base in [0, 5] {
        for a in base..base + 5 {
            for b in a + 1..base + 5 {
                records.push((a, b, 1.0, false));
            }
        }
    }
    records.push((4, 5, 1.0, false));
    let snapshot = graph(11, &records);
    let report = analyze(&snapshot, &AnalysisOptions::default()).unwrap();
    assert!(report.community_converged);
    assert_eq!(
        report
            .communities
            .iter()
            .map(|c| c.nodes.len())
            .collect::<Vec<_>>(),
        vec![5, 5, 1]
    );
    // Q = 2 * (10/21 - (21/42)^2) = 19/42.
    assert!((report.community_modularity - 19.0 / 42.0).abs() < 1e-10);
    assert_connected(&snapshot, &report);
}

fn assert_connected(snapshot: &GraphSnapshot, report: &graf::analysis::AnalysisReport) {
    for community in &report.communities {
        let members: BTreeSet<_> = community.nodes.iter().collect();
        let mut visited = BTreeSet::from([&community.nodes[0]]);
        let mut pending = vec![&community.nodes[0]];
        while let Some(id) = pending.pop() {
            for e in &snapshot.edges {
                if e.metadata["weight"].as_f64().unwrap_or(1.0) <= 0.0 {
                    continue;
                }
                let other = if &e.source == id {
                    &e.target
                } else if &e.target == id {
                    &e.source
                } else {
                    continue;
                };
                if members.contains(other) && visited.insert(other) {
                    pending.push(other);
                }
            }
        }
        assert_eq!(visited, members, "disconnected community");
    }
}

#[test]
fn empty_isolates_limits_and_invalid_weights_are_explicit() {
    let empty = analyze(&graph(0, &[]), &AnalysisOptions::default()).unwrap();
    assert!(empty.nodes.is_empty());
    assert!(empty.pagerank_converged);
    let isolated = analyze(&graph(3, &[]), &AnalysisOptions::default()).unwrap();
    assert_eq!(isolated.communities.len(), 3);
    assert_eq!(isolated.nodes[0].pagerank, 1.0 / 3.0);
    let options = AnalysisOptions {
        max_iterations: 1,
        community_max_passes: 1,
        ..AnalysisOptions::default()
    };
    let report = analyze(&graph(2, &[(0, 1, 1.0, true)]), &options).unwrap();
    assert!(!report.pagerank_converged);
    assert!(!report.community_converged);
    assert!(
        analyze(
            &graph(2, &[(0, 1, -1.0, true)]),
            &AnalysisOptions::default()
        )
        .is_err()
    );
    let mut snapshot = graph(2, &[(0, 1, 1.0, true)]);
    snapshot.edges[0].metadata = json!({"weight":"heavy"});
    assert!(analyze(&snapshot, &AnalysisOptions::default()).is_err());
    snapshot.edges[0].target = "missing".into();
    assert!(analyze(&snapshot, &AnalysisOptions::default()).is_err());
}

#[test]
fn aggregation_improves_ring_of_cliques_beyond_one_clique_per_community() {
    // Thirty triangles in a ring have Q=3/4-1/30 when each triangle is
    // separate. Coarsening must improve that partition by joining adjacent
    // triangles; this is also the documented modularity resolution limit.
    let mut records = Vec::new();
    for c in 0..30 {
        let a = c * 3;
        records.extend([
            (a, a + 1, 1.0, false),
            (a + 1, a + 2, 1.0, false),
            (a, a + 2, 1.0, false),
            (a + 2, ((c + 1) % 30) * 3, 1.0, false),
        ]);
    }
    let snapshot = graph(90, &records);
    let report = analyze(&snapshot, &AnalysisOptions::default()).unwrap();
    assert!(report.community_modularity > 0.75 - 1.0 / 30.0 + 0.01);
    assert!(report.communities.len() < 30);
    assert_connected(&snapshot, &report);
}

#[test]
fn report_audit_and_import_cycles_use_only_recorded_evidence() {
    let mut snapshot = graph(
        4,
        &[(0, 1, 1.0, true), (1, 0, 1.0, true), (1, 2, 1.0, true)],
    );
    snapshot.edges[0].relation = "imports".into();
    snapshot.edges[1].relation = "imports_from".into();
    snapshot.edges[2].confidence = "AMBIGUOUS".into();
    let report = analyze(&snapshot, &AnalysisOptions::default()).unwrap();
    assert_eq!(report.import_cycles, vec![vec!["file0.py", "file1.py"]]);
    assert_eq!(report.confidence_counts["AMBIGUOUS"], 1);
    assert_eq!(report.isolates, vec!["n03"]);
    assert_eq!(report.call_edges.len(), 1);
    assert_eq!(report.call_edges[0].source, "n01");
    for community in &report.communities {
        assert!((0.0..=1.0).contains(&community.cohesion));
    }
}

#[test]
fn percentile_exclusion_reattaches_hub_without_merging_dense_subsystems() {
    let mut records = Vec::new();
    for base in [0, 5] {
        for a in base..base + 5 {
            for b in a + 1..base + 5 {
                records.push((a, b, 1.0, false));
            }
        }
    }
    for node in 0..10 {
        records.push((10, node, 1.0, false));
    }
    let snapshot = graph(11, &records);
    let options = AnalysisOptions {
        exclude_hubs_percentile: Some(80.0),
        ..Default::default()
    };
    let report = analyze(&snapshot, &options).unwrap();
    assert_eq!(report.excluded_hubs, vec!["n10"]);
    assert!(!report.hubs.iter().any(|id| id == "n10"));
    assert_eq!(report.nodes.len(), 11);
    assert_eq!(
        report
            .communities
            .iter()
            .map(|c| c.nodes.len())
            .collect::<Vec<_>>(),
        vec![6, 5]
    );
    assert!((report.community_modularity - 23.0 / 72.0).abs() < 1e-10);
    assert_eq!(report.communities[0].label, "Node 0");
    assert_eq!(report.communities[1].label, "Node 5");
    assert_connected(&snapshot, &report);
    let mut reordered = snapshot.clone();
    reordered.nodes.reverse();
    reordered.edges.reverse();
    assert_eq!(
        serde_json::to_value(report).unwrap(),
        serde_json::to_value(analyze(&reordered, &options).unwrap()).unwrap()
    );
    let invalid = AnalysisOptions {
        exclude_hubs_percentile: Some(101.0),
        ..Default::default()
    };
    assert!(analyze(&snapshot, &invalid).is_err());
    let all = AnalysisOptions {
        exclude_hubs_percentile: Some(100.0),
        ..Default::default()
    };
    assert!(analyze(&snapshot, &all).unwrap().excluded_hubs.is_empty());
}

#[test]
fn noise_filter_changes_rankings_and_labels_without_deleting_graph_data() {
    let mut snapshot = graph(
        4,
        &[(0, 2, 1.0, true), (1, 2, 1.0, true), (3, 2, 1.0, true)],
    );
    snapshot.nodes[0].label = "str".into();
    snapshot.nodes[1].kind = "module".into();
    snapshot.nodes[3].file.clear();
    let filtered = analyze(&snapshot, &Default::default()).unwrap();
    assert_eq!(filtered.hubs, vec!["n02"]);
    assert_eq!(filtered.noise_filtered_hubs, vec!["n00", "n01", "n03"]);
    assert_eq!(filtered.nodes.len(), 4);
    assert_eq!(filtered.call_edges.len(), 3);
    assert_eq!(filtered.communities[0].label, "Node 2");
    let unfiltered = analyze(
        &snapshot,
        &AnalysisOptions {
            filter_noise: false,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(unfiltered.hubs.len(), 4);
    assert!(unfiltered.noise_filtered_hubs.is_empty());
    assert_eq!(
        filtered
            .nodes
            .iter()
            .map(|n| (&n.id, n.community, n.pagerank))
            .collect::<Vec<_>>(),
        unfiltered
            .nodes
            .iter()
            .map(|n| (&n.id, n.community, n.pagerank))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_surprising_connections_cross_source_multi_file() {
    let records: Vec<_> = (1..8).map(|i| (0, i, 1.0, false)).collect();
    let mut snapshot = graph(8, &records);
    for node in &mut snapshot.nodes {
        node.file = "src/core.py".into();
    }
    snapshot.nodes[1].file = "papers/世界.pdf".into();
    snapshot.nodes[7].kind = "file".into();
    snapshot.nodes[7].file = "other/module.py".into();
    for edge in &mut snapshot.edges[1..6] {
        edge.relation = "imports".into();
    }
    snapshot.edges[0].confidence = "INFERRED".into();
    snapshot.edges[0].file = Some("evidence/notes.md".into());
    snapshot.edges[0].line = Some(37);
    snapshot.edges[0].metadata = json!({"weight":1,"evidence":{"quote":"Unverified link; 世界"}});
    let report = analyze(&snapshot, &Default::default()).unwrap();
    assert_eq!(report.surprise_candidates, 1);
    let surprise = &report.surprises[0];
    assert_eq!(
        serde_json::to_value(&surprise.edge).unwrap(),
        serde_json::to_value(&snapshot.edges[0]).unwrap()
    );
    assert_eq!(surprise.source_file, "src/core.py");
    assert_eq!(surprise.target_file, "papers/世界.pdf");
    for code in [
        "confidence",
        "cross_source",
        "cross_directory",
        "cross_category",
        "peripheral_hub",
    ] {
        assert!(surprise.signals.iter().any(|signal| signal.code == code));
    }
    assert_eq!(
        surprise.score,
        surprise.signals.iter().map(|s| s.points).sum::<u32>()
    );
    assert_eq!(
        report.call_edges.len(),
        2,
        "ranking filters retain the original evidence"
    );
}

#[test]
fn test_surprising_connections_single_file_uses_community_bridges() {
    let mut records = Vec::new();
    for base in [0, 5] {
        for a in base..base + 5 {
            for b in a + 1..base + 5 {
                records.push((a, b, 1.0, false));
            }
        }
    }
    records.push((4, 5, 1.0, false));
    let mut snapshot = graph(10, &records);
    for node in &mut snapshot.nodes {
        node.file = "single.py".into();
    }
    let report = analyze(&snapshot, &Default::default()).unwrap();
    assert_eq!(report.communities.len(), 2);
    assert_eq!(report.surprise_candidates, 1);
    let surprise = &report.surprises[0];
    assert_eq!(surprise.edge.id, "e020");
    assert_ne!(surprise.source_community, surprise.target_community);
    assert_eq!(surprise.source_file, surprise.target_file);
    assert!(surprise.signals.iter().any(|s| s.code == "cross_community"));
    assert!(!surprise.signals.iter().any(|s| s.code == "cross_source"));
    assert_eq!(report.suggested_question_candidates, 2);
    for question in &report.suggested_questions {
        assert_eq!(question.kind, "bridge_node");
        assert_eq!(question.evidence_count, 1);
        assert_eq!(question.edge_evidence[0].id, "e020");
        assert!(!question.edge_evidence[0].directed);
    }
}

#[test]
fn test_surprising_connections_ambiguous_scores_higher_than_extracted() {
    let mut snapshot = graph(
        2,
        &[(0, 1, 1.0, true), (0, 1, 1.0, false), (0, 1, 1.0, true)],
    );
    snapshot.edges[1].confidence = "INFERRED".into();
    snapshot.edges[2].confidence = "AMBIGUOUS".into();
    let report = analyze(&snapshot, &Default::default()).unwrap();
    assert_eq!(report.surprise_candidates, 3);
    assert_eq!(
        report
            .surprises
            .iter()
            .map(|s| s.edge.id.as_str())
            .collect::<Vec<_>>(),
        ["e002", "e001", "e000"]
    );
    assert_eq!(report.surprises[0].score, report.surprises[2].score + 2);
    assert_eq!(report.surprises[1].score, report.surprises[2].score + 1);
    assert!(!report.surprises[1].edge.directed);
}

#[test]
fn test_suggest_questions_excludes_rationale_nodes_from_isolated_count() {
    let mut snapshot = graph(5, &[]);
    snapshot.nodes[1].kind = "rationale".into();
    snapshot.nodes[2].metadata = json!({"project":"merged","original_id":"before","original_metadata":{"file_type":"rationale"}});
    snapshot.nodes[3].kind = "module".into();
    snapshot.nodes[4].file.clear();
    let report = analyze(
        &snapshot,
        &AnalysisOptions {
            filter_noise: false,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(report.isolates.len(), 5);
    assert_eq!(report.suggested_question_candidates, 1);
    let question = &report.suggested_questions[0];
    assert_eq!(question.kind, "isolated_nodes");
    assert_eq!(question.evidence_count, 1);
    assert_eq!(question.node_ids, ["n00"]);
    snapshot.nodes[0].kind = "rationale".into();
    let report = analyze(&snapshot, &Default::default()).unwrap();
    assert!(report.suggested_questions.is_empty());
    assert!(report.surprises.is_empty());
}

#[test]
fn question_templates_cover_ambiguity_inferred_hubs_weak_nodes_and_low_cohesion() {
    let records: Vec<_> = (1..16).map(|i| (0, i, 1.0, true)).collect();
    let mut snapshot = graph(16, &records);
    snapshot.edges[0].confidence = "AMBIGUOUS".into();
    for edge in &mut snapshot.edges[1..5] {
        edge.confidence = "INFERRED".into();
    }
    let report = analyze(&snapshot, &Default::default()).unwrap();
    assert_eq!(report.communities.len(), 1);
    assert!(report.communities[0].cohesion < 0.15);
    assert_eq!(report.suggested_question_candidates, 4);
    let kinds: Vec<_> = report
        .suggested_questions
        .iter()
        .map(|q| q.kind.as_str())
        .collect();
    assert_eq!(
        kinds,
        [
            "ambiguous_edge",
            "verify_inferred",
            "isolated_nodes",
            "low_cohesion"
        ]
    );
    let inferred = &report.suggested_questions[1];
    assert_eq!(inferred.evidence_count, 4);
    assert_eq!(inferred.edge_evidence.len(), 3);
    assert!(
        inferred
            .edge_evidence
            .iter()
            .all(|e| e.confidence == "INFERRED")
    );
    assert_eq!(report.suggested_questions[2].evidence_count, 15);
    assert_eq!(report.suggested_questions[2].node_ids.len(), 3);
    assert_eq!(
        report.suggested_questions[3].community_ids,
        [report.communities[0].id]
    );
}

#[test]
fn insights_have_explicit_limits_complete_candidate_counts_and_stable_order() {
    let records: Vec<_> = (0..12).map(|i| (0, 1, 1.0, i % 2 == 0)).collect();
    let mut snapshot = graph(2, &records);
    for edge in &mut snapshot.edges {
        edge.confidence = "AMBIGUOUS".into();
    }
    let report = analyze(&snapshot, &Default::default()).unwrap();
    assert_eq!(report.surprise_candidates, 12);
    assert_eq!(report.surprises.len(), 5);
    assert_eq!(report.suggested_question_candidates, 12);
    assert_eq!(report.suggested_questions.len(), 7);
    assert_eq!(report.surprises.last().unwrap().edge.id, "e004");
    assert_eq!(
        report.suggested_questions.last().unwrap().edge_evidence[0].id,
        "e006"
    );
    snapshot.edges.reverse();
    snapshot.nodes.reverse();
    let reversed = analyze(&snapshot, &Default::default()).unwrap();
    assert_eq!(
        serde_json::to_value(report).unwrap(),
        serde_json::to_value(reversed).unwrap()
    );
}

fn joined_cliques() -> GraphSnapshot {
    let mut records = Vec::new();
    for base in [0, 5] {
        for a in base..base + 5 {
            for b in a + 1..base + 5 {
                records.push((a, b, 1.0, false));
            }
        }
    }
    records.push((4, 5, 1.0, true));
    graph(11, &records)
}

#[test]
fn resolution_changes_granularity_with_unchanged_default_and_isolates() {
    let snapshot = joined_cliques();
    let defaults = AnalysisOptions::default();
    assert_eq!(defaults.resolution, 1.0);
    assert_eq!(defaults.max_community_size, None);
    assert_eq!(defaults.min_cohesion, None);
    let normal = analyze(&snapshot, &defaults).unwrap();
    assert_eq!(
        normal
            .communities
            .iter()
            .map(|c| c.nodes.len())
            .collect::<Vec<_>>(),
        [5, 5, 1]
    );
    assert!((normal.community_modularity - 19.0 / 42.0).abs() < 1e-10);
    assert_eq!(normal.community_split_attempts, 0);
    assert!(normal.unsatisfied_community_constraints.is_empty());
    // Older serialized option objects inherit the unchanged defaults.
    let decoded: AnalysisOptions = serde_json::from_str("{}").unwrap();
    assert_eq!(
        serde_json::to_value(&normal).unwrap(),
        serde_json::to_value(analyze(&snapshot, &decoded).unwrap()).unwrap()
    );
    let coarse = analyze(
        &snapshot,
        &AnalysisOptions {
            resolution: 0.05,
            ..defaults.clone()
        },
    )
    .unwrap();
    assert_eq!(
        coarse
            .communities
            .iter()
            .map(|c| c.nodes.len())
            .collect::<Vec<_>>(),
        [10, 1]
    );
    assert!((coarse.community_modularity - 0.95).abs() < 1e-10);
    assert_eq!(coarse.community_resolution, 0.05);
    let fine = analyze(
        &snapshot,
        &AnalysisOptions {
            resolution: 50.0,
            ..defaults
        },
    )
    .unwrap();
    assert_eq!(fine.communities.len(), 11);
    assert_connected(&snapshot, &coarse);
    assert_connected(&snapshot, &fine);
}

#[test]
fn optional_size_and_cohesion_repartition_recover_connected_cliques() {
    let snapshot = joined_cliques();
    for options in [
        AnalysisOptions {
            resolution: 0.05,
            max_community_size: Some(5),
            ..Default::default()
        },
        AnalysisOptions {
            resolution: 0.05,
            min_cohesion: Some(0.8),
            ..Default::default()
        },
    ] {
        let report = analyze(&snapshot, &options).unwrap();
        assert_eq!(
            report
                .communities
                .iter()
                .map(|c| c.nodes.len())
                .collect::<Vec<_>>(),
            [5, 5, 1]
        );
        assert!(report.community_split_attempts > 0);
        assert!(report.community_passes <= options.community_max_passes);
        assert!(report.unsatisfied_community_constraints.is_empty());
        // Final score always uses the requested full-graph resolution, even
        // though the induced retry uses its documented floor of one.
        assert!((report.community_modularity - (20.0 / 21.0 - 0.025)).abs() < 1e-10);
        assert_connected(&snapshot, &report);
        let mut reordered = snapshot.clone();
        reordered.nodes.reverse();
        reordered.edges.reverse();
        for edge in &mut reordered.edges {
            if !edge.directed {
                std::mem::swap(&mut edge.source, &mut edge.target);
            }
        }
        let other = analyze(&reordered, &options).unwrap();
        assert_eq!(
            serde_json::to_value(&report.communities).unwrap(),
            serde_json::to_value(other.communities).unwrap()
        );
        assert_eq!(
            report.community_split_attempts,
            other.community_split_attempts
        );
    }
}

#[test]
fn split_thresholds_report_unsplittable_groups_and_respect_total_pass_budget() {
    let mut records = Vec::new();
    for a in 0..6 {
        for b in a + 1..6 {
            records.push((a, b, 1.0, false));
        }
    }
    let snapshot = graph(6, &records);
    let options = AnalysisOptions {
        max_community_size: Some(3),
        ..Default::default()
    };
    let report = analyze(&snapshot, &options).unwrap();
    assert_eq!(
        report.communities.len(),
        1,
        "a complete clique is not arbitrarily sliced to satisfy a size target"
    );
    assert_eq!(report.community_split_attempts, 1);
    assert_eq!(report.unsatisfied_community_constraints, [0]);
    let limited = analyze(
        &snapshot,
        &AnalysisOptions {
            community_max_passes: 1,
            ..options
        },
    )
    .unwrap();
    assert_eq!(limited.community_passes, 1);
    assert_eq!(limited.community_split_attempts, 0);
    assert!(!limited.community_converged);
    assert!(!limited.unsatisfied_community_constraints.is_empty());
    let isolated = analyze(
        &graph(3, &[]),
        &AnalysisOptions {
            min_cohesion: Some(1.0),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(isolated.communities.len(), 3);
    assert_eq!(isolated.community_split_attempts, 0);
    assert!(isolated.unsatisfied_community_constraints.is_empty());
}

#[test]
fn invalid_resolution_and_community_thresholds_fail_explicitly() {
    let snapshot = graph(0, &[]);
    for resolution in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        assert!(
            analyze(
                &snapshot,
                &AnalysisOptions {
                    resolution,
                    ..Default::default()
                }
            )
            .is_err()
        );
    }
    for cohesion in [-0.01, 1.01, f64::NAN, f64::INFINITY] {
        assert!(
            analyze(
                &snapshot,
                &AnalysisOptions {
                    min_cohesion: Some(cohesion),
                    ..Default::default()
                }
            )
            .is_err()
        );
    }
    assert!(
        analyze(
            &snapshot,
            &AnalysisOptions {
                max_community_size: Some(0),
                ..Default::default()
            }
        )
        .is_err()
    );
    assert!(
        analyze(
            &snapshot,
            &AnalysisOptions {
                resolution: f64::MIN_POSITIVE,
                min_cohesion: Some(0.0),
                ..Default::default()
            }
        )
        .is_ok()
    );
    let extreme = analyze(
        &graph(2, &[(0, 1, 1.0, false)]),
        &AnalysisOptions {
            resolution: f64::MAX,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(extreme.community_modularity.is_finite());
    assert_eq!(extreme.communities.len(), 2);
}
