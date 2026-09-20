use graf::{
    export::{ExportFormat, render, write_vault},
    model::{Edge, GraphSnapshot, Node},
};
use serde_json::{Value, json};
use std::{collections::BTreeSet, io::Write};

fn fixture() -> GraphSnapshot {
    GraphSnapshot{schema_version:1,generation:9,kind:"imported".into(),root:None,metadata:json!({"title":"Unicode 世界", "custom":{"a":3}}),
        nodes:["α","../notes/世界"].iter().map(|id|Node{id:(*id).into(),label:format!("{id} </script><script>alert('x')</script> [link](javascript:evil) & \"quote\" ` ```"),kind:"function".into(),file:"../secret [file].py".into(),line:Some(2),end_line:Some(8),qualified_name:Some("module.fn".into()),binding_key:Some("binding".into()),metadata:json!({"community":42,"nested":{"tags":["世界"]}})}).collect(),
        edges:[("α","../notes/世界",true),("α","../notes/世界",false),("α","../notes/世界",true),("α","α",false)].iter().enumerate().map(|(i,(a,b,d))|Edge{id:format!("edge-{i}'\\\n"),source:(*a).into(),target:(*b).into(),relation:"calls".into(),directed:*d,file:Some("src/α.py".into()),line:Some(4),confidence:"EXTRACTED".into(),metadata:json!({"weight":2,"source":"stale","_src":"old","from":"wrong","to":"wrong","context":"' ); MATCH (n) DETACH DELETE n; //"})}).collect()}
}

fn import(text: &str) -> graf::model::ImportedGraph {
    let mut file = tempfile::NamedTempFile::new().unwrap();
    file.write_all(text.as_bytes()).unwrap();
    graf::import::read_graphify(file.path()).unwrap()
}

#[test]
fn json_is_lossless_and_strict_graphify_retains_mixed_multiedges_and_provenance() {
    let snapshot = fixture();
    let json = render(&snapshot, ExportFormat::SnapshotJson).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&json).unwrap(),
        serde_json::to_value(&snapshot).unwrap()
    );
    let output = render(&snapshot, ExportFormat::GraphifyJson).unwrap();
    let value: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(value["multigraph"], true);
    assert_eq!(value["nodes"][0]["community"], 42);
    assert_eq!(
        value["nodes"][0]["_graf"],
        serde_json::to_value(&snapshot.nodes[0]).unwrap()
    );
    assert_eq!(
        value["links"][1]["_graf"],
        serde_json::to_value(&snapshot.edges[1]).unwrap()
    );
    let imported = import(&output);
    assert_eq!(imported.nodes.len(), 2);
    assert_eq!(imported.edges.len(), 4);
    for (actual, expected) in imported.edges.iter().zip(&snapshot.edges) {
        assert_eq!(
            (&actual.source, &actual.target, actual.directed),
            (&expected.source, &expected.target, expected.directed)
        );
        assert_eq!(actual.line, expected.line);
    }
}

#[test]
fn all_text_formats_escape_hostile_labels_without_losing_unicode() {
    let snapshot = fixture();
    let html = render(&snapshot, ExportFormat::Html).unwrap();
    assert!(!html.contains("</script><script>alert"));
    assert!(html.contains("\\u003c/script\\u003e"));
    assert!(html.contains("世界"));
    let data = html
        .split("<script id=\"graf-data\" type=\"application/json\">")
        .nth(1)
        .unwrap()
        .split("</script>")
        .next()
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(data).unwrap()["snapshot"],
        serde_json::to_value(&snapshot).unwrap()
    );
    assert!(!html.contains("innerHTML"));
    assert!(!html.contains("https://"));
    for format in [ExportFormat::GraphMl, ExportFormat::Svg] {
        let text = render(&snapshot, format).unwrap();
        assert!(!text.contains("<script>"));
        assert!(text.contains("&lt;/script&gt;"));
        assert!(text.contains("世界"));
        let mut reader = quick_xml::Reader::from_str(&text);
        let mut count = 0;
        loop {
            match reader.read_event().unwrap() {
                quick_xml::events::Event::Eof => break,
                _ => count += 1,
            }
        }
        assert!(count > 10);
    }
    let cypher = render(&snapshot, ExportFormat::Cypher).unwrap();
    assert!(cypher.contains("r.directed=false"));
    assert_eq!(cypher.matches("MERGE (a)-[r:GRAF_EDGE").count(), 4);
    assert!(cypher.contains("\\'"));
    assert!(cypher.contains("\\\\"));
    assert!(cypher.contains("世界"));
    let markdown = render(&snapshot, ExportFormat::Markdown).unwrap();
    assert!(!markdown.contains("<script>"));
    assert!(!markdown.contains("](javascript:"));
    assert!(!markdown.contains("```"));
    assert!(markdown.contains("世界"));
    assert!(markdown.contains("L4"));
    let mermaid = render(&snapshot, ExportFormat::Mermaid).unwrap();
    assert!(!mermaid.contains("<script>"));
    assert!(mermaid.contains("#34;"));
    assert!(mermaid.contains("世界"));
    assert_eq!(
        mermaid
            .lines()
            .filter(|l| l.contains("|\"calls\"|"))
            .count(),
        4
    );
}

#[test]
fn xml_control_characters_fail_explicitly_but_json_survives() {
    let mut snapshot = fixture();
    snapshot.nodes[0].label = "text\0after".into();
    assert!(render(&snapshot, ExportFormat::GraphMl).is_err());
    assert!(render(&snapshot, ExportFormat::Svg).is_err());
    assert!(
        render(&snapshot, ExportFormat::SnapshotJson)
            .unwrap()
            .contains("\\u0000")
    );
}

#[test]
fn wiki_uses_new_directories_and_safe_links_without_touching_user_content() {
    let destination = tempfile::tempdir().unwrap();
    std::fs::write(destination.path().join("index.md"), "user note").unwrap();
    std::fs::create_dir(destination.path().join(".obsidian")).unwrap();
    std::fs::write(destination.path().join(".obsidian/graph.json"), "settings").unwrap();
    let first = write_vault(&fixture(), destination.path()).unwrap();
    std::fs::write(first.directory.join("node-0.md"), "edited generated note").unwrap();
    let second = write_vault(&fixture(), destination.path()).unwrap();
    assert_ne!(first.directory, second.directory);
    assert_eq!(
        second.files,
        fixture().nodes.len()
            + 6
            + graf::analysis::analyze(&fixture(), &Default::default())
                .unwrap()
                .communities
                .len()
    );
    assert_eq!(
        std::fs::read_to_string(destination.path().join("index.md")).unwrap(),
        "user note"
    );
    assert_eq!(
        std::fs::read_to_string(destination.path().join(".obsidian/graph.json")).unwrap(),
        "settings"
    );
    assert_eq!(
        std::fs::read_to_string(first.directory.join("node-0.md")).unwrap(),
        "edited generated note"
    );
    assert!(
        std::fs::read_to_string(second.directory.join("node-0.md"))
            .unwrap()
            .contains("(node-1.md)")
    );
    let canvas: Value = serde_json::from_str(
        &std::fs::read_to_string(second.directory.join("graph.canvas")).unwrap(),
    )
    .unwrap();
    for card in canvas["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|n| n["type"] == "file")
    {
        assert!(
            destination
                .path()
                .join(card["file"].as_str().unwrap())
                .is_file()
        );
    }
    assert!(!second.directory.join(".obsidian").exists());
}

fn grouped() -> GraphSnapshot {
    let value = json!({"directed":false,"multigraph":false,"nodes":[{"id":"a"},{"id":"b"},{"id":"c"}],"links":[],"graph":{"title":"groups","hyperedges":[{"id":"group-one","nodes":["a","b","a"],"label":"Team 世界","source_file":"notes.md","source_location":"L3","custom":{"keep":true}},{"id":"group-two","members":["c"],"confidence":"INFERRED"}]}});
    let imported = import(&value.to_string());
    GraphSnapshot {
        schema_version: 1,
        generation: 1,
        kind: "imported".into(),
        root: None,
        nodes: imported.nodes,
        edges: imported.edges,
        metadata: imported.metadata,
    }
}

fn memberships(snapshot: &GraphSnapshot) -> Vec<BTreeSet<String>> {
    let mut groups: Vec<_> = snapshot
        .nodes
        .iter()
        .filter(|n| n.kind == "group")
        .map(|n| {
            snapshot
                .edges
                .iter()
                .filter(|e| e.target == n.id && e.relation == "member_of")
                .map(|e| e.source.clone())
                .collect()
        })
        .collect();
    groups.sort();
    groups
}

#[test]
fn graphify_groups_roundtrip_once_with_current_members_and_namespaces() {
    let mut snapshot = grouped();
    // Remove one member and add a new one while preserving stale original arrays.
    snapshot.edges.retain(|e| e.source != "b");
    snapshot.edges[0].source = "c".into();
    let output = render(&snapshot, ExportFormat::GraphifyJson).unwrap();
    let value: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(value["nodes"].as_array().unwrap().len(), 3);
    assert_eq!(value["links"].as_array().unwrap().len(), 0);
    assert_eq!(value["hyperedges"].as_array().unwrap().len(), 2);
    assert!(value["graph"].get("hyperedges").is_none());
    assert_eq!(value["hyperedges"][0]["custom"]["keep"], true);
    let imported = import(&output);
    let roundtrip = GraphSnapshot {
        nodes: imported.nodes,
        edges: imported.edges,
        metadata: imported.metadata,
        ..snapshot.clone()
    };
    assert_eq!(memberships(&snapshot), memberships(&roundtrip));
    assert_eq!(snapshot.nodes.len(), roundtrip.nodes.len());
    assert_eq!(snapshot.edges.len(), roundtrip.edges.len());
    let merged = graf::snapshot::merge(vec![
        ("one".into(), snapshot.clone()),
        ("two".into(), snapshot.clone()),
    ])
    .unwrap();
    let merged = GraphSnapshot {
        nodes: merged.nodes,
        edges: merged.edges,
        metadata: merged.metadata,
        ..snapshot
    };
    let text = render(&merged, ExportFormat::GraphifyJson).unwrap();
    let imported = import(&text);
    let roundtrip = GraphSnapshot {
        nodes: imported.nodes,
        edges: imported.edges,
        metadata: imported.metadata,
        ..merged.clone()
    };
    assert_eq!(memberships(&merged), memberships(&roundtrip));
    assert_eq!(merged.nodes.len(), roundtrip.nodes.len());
    assert_eq!(merged.edges.len(), roundtrip.edges.len());
}

#[test]
fn groups_with_ordinary_relations_keep_incidence_representation() {
    let mut snapshot = grouped();
    let group = snapshot
        .nodes
        .iter()
        .find(|n| n.kind == "group")
        .unwrap()
        .id
        .clone();
    snapshot.edges.push(Edge {
        id: "ordinary".into(),
        source: group,
        target: "c".into(),
        relation: "references".into(),
        directed: true,
        file: None,
        line: None,
        confidence: "UNKNOWN".into(),
        metadata: json!({}),
    });
    let imported = import(&render(&snapshot, ExportFormat::GraphifyJson).unwrap());
    assert_eq!(imported.nodes.len(), snapshot.nodes.len());
    assert_eq!(imported.edges.len(), snapshot.edges.len());
    assert!(imported.edges.iter().any(|e| e.relation == "references"));
}

#[test]
fn object_member_and_node_ids_alias_roundtrip_after_composition() {
    let input = json!({"directed":false,"multigraph":false,"nodes":[{"id":7},{"id":"7"}],"links":[],"groups":[{"id":"g","node_ids":[{"id":7,"role":"lead"},{"id":"7","role":"peer"}],"label":"Alias group"}]});
    let imported = import(&input.to_string());
    let snapshot = GraphSnapshot {
        schema_version: 1,
        generation: 1,
        kind: "imported".into(),
        root: None,
        nodes: imported.nodes,
        edges: imported.edges,
        metadata: imported.metadata,
    };
    let merged = graf::snapshot::merge(vec![
        ("项目".into(), snapshot.clone()),
        ("other".into(), snapshot.clone()),
    ])
    .unwrap();
    let merged = GraphSnapshot {
        nodes: merged.nodes,
        edges: merged.edges,
        metadata: merged.metadata,
        ..snapshot
    };
    let text = render(&merged, ExportFormat::GraphifyJson).unwrap();
    let value: Value = serde_json::from_str(&text).unwrap();
    for group in value["hyperedges"].as_array().unwrap() {
        assert!(group.get("node_ids").is_none());
        assert_eq!(group["nodes"].as_array().unwrap().len(), 2);
    }
    let imported = import(&text);
    let actual = GraphSnapshot {
        nodes: imported.nodes,
        edges: imported.edges,
        metadata: imported.metadata,
        ..merged.clone()
    };
    assert_eq!(memberships(&actual), memberships(&merged));
    let by_id = |mut records: Vec<Node>| {
        records.sort_by(|a, b| a.id.cmp(&b.id));
        serde_json::to_value(records).unwrap()
    };
    assert_eq!(by_id(actual.nodes), by_id(merged.nodes));
    assert_eq!(
        serde_json::to_value(actual.edges).unwrap(),
        serde_json::to_value(merged.edges).unwrap()
    );
}

#[test]
fn canvas_callflow_and_tree_are_standalone_and_escape_content() {
    let snapshot = fixture();
    let canvas: Value =
        serde_json::from_str(&render(&snapshot, ExportFormat::Canvas).unwrap()).unwrap();
    let cards: Vec<_> = canvas["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|n| n["type"] == "text")
        .collect();
    assert_eq!(cards.len(), 2);
    assert_eq!(canvas["edges"].as_array().unwrap().len(), 4);
    assert_eq!(canvas["edges"][1]["toEnd"], "none");
    assert_eq!(canvas["edges"][0]["toEnd"], "arrow");
    assert!(!cards[0]["text"].as_str().unwrap().contains("](javascript:"));
    assert_eq!(
        canvas["_graf_snapshot"],
        serde_json::to_value(&snapshot).unwrap()
    );
    for format in [ExportFormat::CallflowHtml, ExportFormat::TreeHtml] {
        let html = render(&snapshot, format).unwrap();
        assert!(html.starts_with("<!doctype html>"));
        assert!(!html.contains("</script><script>alert"));
        assert!(!html.contains("innerHTML"));
        assert!(html.contains("世界"));
        assert!(html.contains("data-search"));
        assert!(!html.contains("href=\"javascript:"));
    }
    let callflow = render(&snapshot, ExportFormat::CallflowHtml).unwrap();
    assert_eq!(callflow.matches("<tr data-search>").count(), 4);
    assert!(callflow.contains("L4"));
    assert!(callflow.contains("EXTRACTED"));
    let tree = render(&snapshot, ExportFormat::TreeHtml).unwrap();
    assert_eq!(tree.matches("<details data-search>").count(), 2);
}

#[test]
fn viewer_limits_retain_the_complete_large_snapshot_for_search_and_download() {
    use graf::export::{ExportOptions, render_with_options};
    let mut snapshot = fixture();
    let template = snapshot.nodes[0].clone();
    snapshot.edges.clear();
    snapshot.nodes = (0..1205)
        .map(|i| Node {
            id: format!("n{i}"),
            label: format!("Entity {i}"),
            ..template.clone()
        })
        .collect();
    let options = ExportOptions {
        node_limit: 200,
        edge_limit: 500,
        ..Default::default()
    };
    let html = render_with_options(&snapshot, ExportFormat::Html, &options).unwrap();
    let embedded = html
        .split("<script id=\"graf-data\" type=\"application/json\">")
        .nth(1)
        .unwrap()
        .split("</script>")
        .next()
        .unwrap();
    let data: Value = serde_json::from_str(embedded).unwrap();
    assert_eq!(data["snapshot"]["nodes"].as_array().unwrap().len(), 1205);
    assert_eq!(data["viewer"]["node_limit"], 200);
    assert_eq!(data["viewer"]["edge_limit"], 500);
    assert_eq!(
        data["analysis"]["communities"].as_array().unwrap().len(),
        1205
    );
    assert!(
        render_with_options(
            &snapshot,
            ExportFormat::Html,
            &ExportOptions {
                node_limit: 0,
                ..Default::default()
            }
        )
        .is_err()
    );
}

#[test]
fn cypher_statements_preserve_semicolons_quotes_and_newlines_as_string_data() {
    let mut snapshot = fixture();
    snapshot.nodes[0].label = "first; 'quoted'\n世界\\path\r\nlast;".into();
    let statements = graf::export::cypher_statements(&snapshot).unwrap();
    assert_eq!(
        statements.len(),
        1 + snapshot.nodes.len() + snapshot.edges.len()
    );
    assert!(statements[0].starts_with("MERGE (g:GrafSnapshot"));
    assert!(statements[1].contains(r#"n.label='first; \'quoted\'\n世界\\path\r\nlast;'"#));
    assert!(
        statements
            .iter()
            .all(|statement| !statement.ends_with(';') && !statement.contains(['\n', '\r']))
    );
    assert_eq!(
        statements
            .iter()
            .filter(|statement| statement.starts_with("MATCH "))
            .count(),
        snapshot.edges.len()
    );
    let rendered = render(&snapshot, ExportFormat::Cypher).unwrap();
    assert_eq!(
        rendered
            .lines()
            .filter(|line| !line.starts_with("//"))
            .collect::<Vec<_>>(),
        statements
            .iter()
            .map(|statement| format!("{statement};"))
            .collect::<Vec<_>>()
    );
    snapshot.edges[0].source = "unknown endpoint".into();
    assert!(graf::export::cypher_statements(&snapshot).is_err());
}

#[test]
fn report_insights_show_truncation_and_escape_labels_and_source_evidence() {
    let mut snapshot = fixture();
    snapshot.nodes[1].file = "docs/世界 </script>[x](javascript:evil).pdf".into();
    let template = snapshot.edges[0].clone();
    snapshot.edges = (0..10)
        .map(|i| Edge {
            id: format!("edge-{i:02}"),
            confidence: "AMBIGUOUS".into(),
            file: Some("evidence/'[x](javascript:evil)</script>.md".into()),
            directed: i % 2 == 0,
            ..template.clone()
        })
        .collect();
    let markdown = render(&snapshot, ExportFormat::Markdown).unwrap();
    assert!(markdown.contains("## Ranked structural connections"));
    assert!(markdown.contains("Showing 5 of 10 eligible connections (limit 5)"));
    assert!(markdown.contains("## Suggested questions"));
    assert!(markdown.contains("Showing 7 of 10 template candidates (limit 7)"));
    assert!(markdown.contains("source evidence:"));
    assert!(markdown.contains("L4"));
    assert!(markdown.contains("世界"));
    assert!(markdown.contains("&#60;&#47;script&#62;"));
    assert!(!markdown.contains("<script>"));
    assert!(!markdown.contains("</script>"));
    assert!(!markdown.contains("](javascript:"));
    assert!(!markdown.contains("```"));
    let html = render(&snapshot, ExportFormat::Html).unwrap();
    assert!(!html.contains("</script><script>alert"));
    let embedded = html
        .split("<script id=\"graf-data\" type=\"application/json\">")
        .nth(1)
        .unwrap()
        .split("</script>")
        .next()
        .unwrap();
    let data: Value = serde_json::from_str(embedded).unwrap();
    assert_eq!(data["analysis"]["surprise_candidates"], 10);
    assert_eq!(data["analysis"]["suggested_question_candidates"], 10);
    assert_eq!(
        data["analysis"]["surprises"][0]["edge"],
        serde_json::to_value(&snapshot.edges[0]).unwrap()
    );
    assert!(
        data["analysis"]["suggested_questions"][0]["question"]
            .as_str()
            .unwrap()
            .contains(&snapshot.nodes[0].label)
    );
}

#[test]
fn saved_membership_labels_override_report_titles_without_changing_graph_data() {
    use graf::export::{ExportOptions, render_with_options};
    let snapshot = fixture();
    let baseline = graf::analysis::analyze(&snapshot, &Default::default()).unwrap();
    let mut members = baseline.communities[0].nodes.clone();
    members.sort();
    let signature = blake3::hash(&serde_json::to_vec(&members).unwrap())
        .to_hex()
        .to_string();
    let label = "Curated 世界 </script><script>alert('label')</script> [x](javascript:evil)";
    let mut options = ExportOptions::default();
    options.community_labels.insert(signature, label.into());
    options
        .community_labels
        .insert("0".into(), "STALE-ID-LABEL".into());
    let mut stale_members = members.clone();
    stale_members.push("missing-node".into());
    stale_members.sort();
    options.community_labels.insert(
        blake3::hash(&serde_json::to_vec(&stale_members).unwrap())
            .to_hex()
            .to_string(),
        "STALE-MEMBERSHIP-LABEL".into(),
    );
    let markdown = render_with_options(&snapshot, ExportFormat::Markdown, &options).unwrap();
    assert!(markdown.contains("Curated 世界 &#60;&#47;script&#62;"));
    assert!(!markdown.contains("<script>"));
    assert!(!markdown.contains("](javascript:"));
    assert!(!markdown.contains("STALE-"));
    let html = render_with_options(&snapshot, ExportFormat::Html, &options).unwrap();
    assert!(!html.contains("</script><script>alert"));
    let embedded = html
        .split("<script id=\"graf-data\" type=\"application/json\">")
        .nth(1)
        .unwrap()
        .split("</script>")
        .next()
        .unwrap();
    let data: Value = serde_json::from_str(embedded).unwrap();
    assert_eq!(data["analysis"]["communities"][0]["label"], label);
    assert_eq!(
        data["analysis"]["nodes"],
        serde_json::to_value(&baseline.nodes).unwrap()
    );
    assert_eq!(data["snapshot"], serde_json::to_value(&snapshot).unwrap());
    let canvas: Value = serde_json::from_str(
        &render_with_options(&snapshot, ExportFormat::Canvas, &options).unwrap(),
    )
    .unwrap();
    assert_eq!(canvas["nodes"][0]["label"], format!("Community 0: {label}"));
    assert_eq!(
        render_with_options(&snapshot, ExportFormat::SnapshotJson, &options).unwrap(),
        render(&snapshot, ExportFormat::SnapshotJson).unwrap()
    );
    options
        .community_labels
        .retain(|_, value| value.starts_with("STALE-"));
    assert_eq!(
        render_with_options(&snapshot, ExportFormat::Markdown, &options).unwrap(),
        render(&snapshot, ExportFormat::Markdown).unwrap()
    );
}

#[test]
fn vault_graph_color_snippets_match_actual_tags_and_preserve_user_configuration() {
    use graf::export::{ExportOptions, write_vault_with_options};
    let snapshot = fixture();
    let baseline = graf::analysis::analyze(&snapshot, &Default::default()).unwrap();
    let mut members = baseline.communities[0].nodes.clone();
    members.sort();
    let mut options = ExportOptions::default();
    options.community_labels.insert(
        blake3::hash(&serde_json::to_vec(&members).unwrap())
            .to_hex()
            .to_string(),
        "Saved 世界 </script>".into(),
    );
    let vault = tempfile::tempdir().unwrap();
    std::fs::create_dir(vault.path().join(".obsidian")).unwrap();
    let user_config = r##"{"colorGroups":[{"query":"tag:#personal","color":{"a":1,"rgb":123}}],"existing":true}"##;
    std::fs::write(vault.path().join(".obsidian/graph.json"), user_config).unwrap();
    std::fs::write(vault.path().join("personal.md"), "# User-owned note").unwrap();
    let exported = write_vault_with_options(&snapshot, vault.path(), &options).unwrap();
    let config: Value = serde_json::from_str(
        &std::fs::read_to_string(exported.directory.join("graph-colors.json")).unwrap(),
    )
    .unwrap();
    let groups = config["colorGroups"].as_array().unwrap();
    assert_eq!(groups.len(), baseline.communities.len());
    let query = groups[0]["query"].as_str().unwrap();
    let tag = query.strip_prefix("tag:#").unwrap();
    assert!(tag.starts_with(&format!(
        "graf/{}/",
        exported.directory.file_name().unwrap().to_str().unwrap()
    )));
    assert!(
        tag.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '/'))
    );
    assert_eq!(groups[0]["color"]["a"], 1);
    assert!(groups[0]["color"]["rgb"].as_u64().unwrap() <= 0xffffff);
    for name in ["node-0.md", "node-1.md", "community-0.md"] {
        let note = std::fs::read_to_string(exported.directory.join(name)).unwrap();
        assert!(note.starts_with(&format!("---\ntags:\n  - {tag}\n---\n")));
        assert!(!note.contains("</script>"));
    }
    let community = std::fs::read_to_string(exported.directory.join("community-0.md")).unwrap();
    assert!(community.contains("Saved 世界 &#60;&#47;script&#62;"));
    let instructions = std::fs::read_to_string(exported.directory.join("graph-colors.md")).unwrap();
    assert!(instructions.contains(query));
    assert!(instructions.contains("manually merge"));
    assert!(!exported.directory.join(".obsidian").exists());
    assert_eq!(
        std::fs::read_to_string(vault.path().join(".obsidian/graph.json")).unwrap(),
        user_config
    );
    assert_eq!(
        std::fs::read_to_string(vault.path().join("personal.md")).unwrap(),
        "# User-owned note"
    );
    let next = write_vault_with_options(&snapshot, vault.path(), &options).unwrap();
    let next_config: Value = serde_json::from_str(
        &std::fs::read_to_string(next.directory.join("graph-colors.json")).unwrap(),
    )
    .unwrap();
    assert_ne!(next_config["colorGroups"][0]["query"], groups[0]["query"]);
}

#[test]
fn viewer_traversal_keeps_off_page_mixed_relations_and_source_evidence() {
    use graf::export::{ExportOptions, render_with_options};
    let mut snapshot = fixture();
    let template = snapshot.nodes[0].clone();
    let hostile = "neighbor\" </script><img src=x onerror=alert(1)> 世界";
    snapshot.nodes = ["origin", "same-label", "off-page", hostile]
        .iter()
        .map(|id| Node {
            id: (*id).into(),
            label: "Shared label".into(),
            file: format!("docs/{id}.md"),
            ..template.clone()
        })
        .collect();
    let edge = snapshot.edges[0].clone();
    snapshot.edges = [
        ("origin", hostile, true),
        (hostile, "origin", true),
        ("origin", hostile, false),
        ("origin", "origin", false),
        ("origin", "off-page", true),
        ("origin", hostile, true),
    ]
    .iter()
    .enumerate()
    .map(|(i, (source, target, directed))| Edge {
        id: format!("record-{i}"),
        source: (*source).into(),
        target: (*target).into(),
        directed: *directed,
        relation: format!("relation-{i} </script>"),
        file: Some(format!("evidence/{i}.md")),
        line: Some(i as u32 + 1),
        ..edge.clone()
    })
    .collect();
    let options = ExportOptions {
        node_limit: 1,
        edge_limit: 1,
        ..Default::default()
    };
    let html = render_with_options(&snapshot, ExportFormat::Html, &options).unwrap();
    let embedded = html
        .split("<script id=\"graf-data\" type=\"application/json\">")
        .nth(1)
        .unwrap()
        .split("</script>")
        .next()
        .unwrap();
    let data: Value = serde_json::from_str(embedded).unwrap();
    assert_eq!(data["snapshot"], serde_json::to_value(&snapshot).unwrap());
    assert_eq!(data["viewer"]["node_limit"], 1);
    assert_eq!(data["viewer"]["edge_limit"], 1);
    assert!(!html.contains("<img src=x"));
    assert!(!html.contains("innerHTML"));
    assert!(html.contains("connect-src 'none'"));
    // Export contract only: browser execution separately checks traversal,
    // focus restoration, geometry, and the inert rendering of these records.
    for control in [
        "back",
        "focus-node",
        "neighbors",
        "neighbor-direction",
        "neighbor-search",
        "neighbor-previous",
        "neighbor-next",
        "fit",
        "zoom-in",
        "zoom-out",
    ] {
        assert!(html.contains(&format!("id=\"{control}\"")), "{control}");
    }
}

#[test]
fn viewer_empty_snapshot_stays_exportable_and_edge_limit_must_be_positive() {
    use graf::export::{ExportOptions, render_with_options};
    let mut snapshot = fixture();
    snapshot.nodes.clear();
    snapshot.edges.clear();
    let html = render(&snapshot, ExportFormat::Html).unwrap();
    let embedded = html
        .split("<script id=\"graf-data\" type=\"application/json\">")
        .nth(1)
        .unwrap()
        .split("</script>")
        .next()
        .unwrap();
    let data: Value = serde_json::from_str(embedded).unwrap();
    assert_eq!(data["snapshot"], serde_json::to_value(&snapshot).unwrap());
    assert_eq!(data["analysis"]["nodes"], json!([]));
    assert_eq!(data["analysis"]["communities"], json!([]));
    assert!(html.contains("id=\"download\""));
    assert!(
        render_with_options(
            &snapshot,
            ExportFormat::Html,
            &ExportOptions {
                edge_limit: 0,
                ..Default::default()
            }
        )
        .is_err()
    );
}

fn viewer_data(html: &str) -> Value {
    serde_json::from_str(
        html.split("<script id=\"graf-data\" type=\"application/json\">")
            .nth(1)
            .unwrap()
            .split("</script>")
            .next()
            .unwrap(),
    )
    .unwrap()
}

fn learning_fixture(snapshot: &GraphSnapshot) -> graf::memory::LearningOverlay {
    graf::memory::LearningOverlay {
        schema_version: 1,
        generated_unix_secs: 1234,
        snapshot_hash: Some(blake3::hash(&serde_json::to_vec(snapshot).unwrap()).to_hex().to_string()),
        nodes: [(snapshot.nodes[0].id.clone(), graf::memory::LearningNode {
            status: "tentative".into(), score: 1.5, useful: 2, negative: 0,
            verified_useful: 0, unverified: 2,
            reason: "unverified </script><img id=learning-injection src=x onerror=alert(1)> 世界".into(),
        })].into(),
        lessons: "# Lessons\n## preferred\n## tentative\n    α | useful=2 | verified_useful=0 | unverified=2\nQuestion: <script>alert(1)</script>\nDead end: [link](javascript:evil)\n```\nOriginal answer 世界\nCorrection: preserved\r<img src=x onerror=alert(1)>".into(),
    }
}

#[test]
fn learning_exports_bind_exact_snapshot_escape_observations_and_keep_topology() {
    use graf::export::{ExportOptions, render_with_options, write_vault_with_options};
    let snapshot = fixture();
    let before = serde_json::to_value(&snapshot).unwrap();
    let options = ExportOptions {
        learning: Some(learning_fixture(&snapshot)),
        ..Default::default()
    };
    let plain = render(&snapshot, ExportFormat::Html).unwrap();
    let html = render_with_options(&snapshot, ExportFormat::Html, &options).unwrap();
    let embedded = viewer_data(&html);
    assert_eq!(embedded["snapshot"], before);
    assert_eq!(embedded["analysis"], viewer_data(&plain)["analysis"]);
    assert_eq!(
        embedded["learning"],
        serde_json::to_value(options.learning.as_ref().unwrap()).unwrap()
    );
    assert_eq!(embedded["learning"]["nodes"]["α"]["status"], "tentative");
    assert!(embedded["learning"]["nodes"].get("../notes/世界").is_none());
    assert!(!html.contains("<img id=learning-injection"));
    assert!(!html.contains("<script>alert(1)</script>"));
    assert!(!html.contains("innerHTML"));
    assert!(html.contains("connect-src 'none'"));
    assert!(viewer_data(&plain)["learning"].is_null());
    let report = render_with_options(&snapshot, ExportFormat::Markdown, &options).unwrap();
    assert!(report.contains("## Work-memory lessons"));
    assert!(report.contains("Status: tentative"));
    assert!(report.contains("verified useful 0"));
    assert!(report.contains("Original answer 世界"));
    assert!(report.contains("    Correction: preserved\n"));
    assert!(report.contains("\n## preferred\n\n"));
    assert!(
        report.contains(
            "\n## tentative\n\n        α | useful=2 | verified_useful=0 | unverified=2\n"
        )
    );
    assert!(report.contains("    Question: <script>alert(1)</script>\n"));
    assert!(report.contains("    Dead end: [link](javascript:evil)\n"));
    assert!(report.contains("    ```\n"));
    assert!(report.contains("\n    <img src=x onerror=alert(1)>\n"));
    let lessons = report
        .split("### Recorded lessons and event provenance\n\n")
        .nth(1)
        .unwrap();
    assert!(lessons.lines().all(|line| line.is_empty()
        || line.starts_with("    ")
        || matches!(line, "## preferred" | "## tentative")));
    assert!(
        !render(&snapshot, ExportFormat::Markdown)
            .unwrap()
            .contains("## Work-memory lessons")
    );
    let vault = tempfile::tempdir().unwrap();
    let output = write_vault_with_options(&snapshot, vault.path(), &options).unwrap();
    assert_eq!(
        std::fs::read_to_string(output.directory.join("report.md")).unwrap(),
        report
    );
    let first = std::fs::read_to_string(output.directory.join("node-0.md")).unwrap();
    assert!(first.contains("## Work-memory observation"));
    assert!(first.contains("Status: tentative"));
    assert!(!first.contains("<img"));
    assert!(
        !std::fs::read_to_string(output.directory.join("node-1.md"))
            .unwrap()
            .contains("## Work-memory observation")
    );
    assert_eq!(
        serde_json::from_str::<Value>(
            &std::fs::read_to_string(output.directory.join("snapshot.json")).unwrap()
        )
        .unwrap(),
        before
    );
    assert_eq!(serde_json::to_value(&snapshot).unwrap(), before);
}

#[test]
fn learning_mismatch_rejected_before_render_or_vault_creation() {
    use graf::export::{ExportOptions, render_with_options, write_vault_with_options};
    let snapshot = fixture();
    let options = ExportOptions {
        learning: Some(learning_fixture(&snapshot)),
        ..Default::default()
    };
    for change in 0..5 {
        let mut other = snapshot.clone();
        match change {
            0 => other.generation += 1,
            1 => other.nodes[0].label.push_str(" changed"),
            2 => other.edges[0].relation = "references".into(),
            3 => other.metadata["source_proof"] = json!("different"),
            _ => other.root = Some("another-root".into()),
        }
        for format in [ExportFormat::Html, ExportFormat::Markdown] {
            let error = render_with_options(&other, format, &options).unwrap_err();
            assert!(error.to_string().contains("exact snapshot"), "{error}");
        }
        let vault = tempfile::tempdir().unwrap();
        let error = write_vault_with_options(&other, vault.path(), &options).unwrap_err();
        assert!(error.to_string().contains("exact snapshot"));
        assert_eq!(std::fs::read_dir(vault.path()).unwrap().count(), 0);
    }
    for invalid in 0..3 {
        let mut options = options.clone();
        let learning = options.learning.as_mut().unwrap();
        match invalid {
            0 => learning.snapshot_hash = None,
            1 => learning.schema_version = 999,
            _ => learning.nodes.values_mut().next().unwrap().score = f64::NAN,
        }
        assert!(render_with_options(&snapshot, ExportFormat::Html, &options).is_err());
    }
}

#[test]
fn learning_rejects_other_formats_and_empty_memory_still_matches_snapshot() {
    use graf::export::{ExportOptions, render_with_options};
    let snapshot = fixture();
    let options = ExportOptions {
        learning: Some(learning_fixture(&snapshot)),
        ..Default::default()
    };
    for format in [
        ExportFormat::SnapshotJson,
        ExportFormat::GraphifyJson,
        ExportFormat::GraphMl,
        ExportFormat::Cypher,
        ExportFormat::Mermaid,
        ExportFormat::Svg,
        ExportFormat::Canvas,
        ExportFormat::CallflowHtml,
        ExportFormat::TreeHtml,
    ] {
        let error = render_with_options(&snapshot, format, &options).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("only by HTML, Markdown and wiki")
        );
    }
    let memory = tempfile::tempdir().unwrap();
    let overlay = graf::memory::learning_overlay_at(
        &graf::memory::ReflectArgs {
            memory_dir: memory.path().into(),
            out: memory.path().join("must-not-exist.md"),
            half_life_days: 30.0,
            min_corroboration: 2,
            if_stale: false,
        },
        Some(&snapshot),
        42,
    )
    .unwrap();
    assert_eq!(
        overlay.snapshot_hash,
        options.learning.unwrap().snapshot_hash
    );
    let options = ExportOptions {
        learning: Some(overlay),
        ..Default::default()
    };
    assert!(viewer_data(&render_with_options(&snapshot, ExportFormat::Html, &options).unwrap())["learning"]["nodes"].as_object().unwrap().is_empty());
    assert_eq!(std::fs::read_dir(memory.path()).unwrap().count(), 0);
}

#[test]
fn viewer_groups_use_current_incidence_edges_without_cliques_or_metadata_guesses() {
    use graf::export::{ExportOptions, render_with_options};
    let imported = import(&json!({"directed":false,"multigraph":false,"nodes":[{"id":"a"},{"id":"b"},{"id":"c"}],"links":[],
        "hyperedges":[{"id":"recorded","label":"Group 世界 </script><img src=x>","nodes":["a","b","c"]}]}).to_string());
    let mut graph = GraphSnapshot {
        schema_version: 1,
        generation: 1,
        kind: "imported".into(),
        root: None,
        nodes: imported.nodes,
        edges: imported.edges,
        metadata: imported.metadata,
    };
    let group_id = graph
        .nodes
        .iter()
        .find(|node| node.kind == "group")
        .unwrap()
        .id
        .clone();
    // Old embedded group metadata still names c. Only current edges can establish membership.
    graph.edges.retain(|edge| edge.source != "c");
    let mut ordinary = graph.edges[0].clone();
    ordinary.id = "ordinary-group-relation".into();
    ordinary.source = "c".into();
    ordinary.relation = "references".into();
    graph.edges.push(ordinary);
    // A second incidence is evidence, not a duplicate node or an inferred clique edge.
    let mut parallel = graph.edges[0].clone();
    parallel.id = "corroborating-membership".into();
    std::mem::swap(&mut parallel.source, &mut parallel.target);
    graph.edges.push(parallel);
    let before = serde_json::to_value(&graph).unwrap();
    let options = ExportOptions {
        node_limit: 1,
        edge_limit: 1,
        ..Default::default()
    };
    let html = render_with_options(&graph, ExportFormat::Html, &options).unwrap();
    let data = viewer_data(&html);
    assert_eq!(data["groups"].as_array().unwrap().len(), 1);
    assert_eq!(data["groups"][0]["id"], group_id);
    assert_eq!(data["groups"][0]["members"], json!(["a", "b"]));
    let incidences: BTreeSet<_> = data["groups"][0]["incidences"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(incidences.len(), 3);
    assert!(incidences.contains("corroborating-membership"));
    assert!(!incidences.contains("ordinary-group-relation"));
    assert_eq!(data["snapshot"], before);
    assert_eq!(data["viewer"], json!({"node_limit":1,"edge_limit":1}));
    assert_eq!(serde_json::to_value(&graph).unwrap(), before);
    assert!(!html.contains("<img src=x>"));
    assert!(html.contains("id=\"groups-panel\""));
}
