use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

use serde_json::{Value, json};
use tempfile::{TempDir, tempdir};

fn write(path: &Path, bytes: impl AsRef<[u8]>) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}

fn graph(id: &str) -> String {
    json!({
        "directed": true, "multigraph": false,
        "nodes": [{"id": id, "label": id}], "links": []
    })
    .to_string()
}

fn config(graph: &str) -> Value {
    json!({"mcpServers": {"graphify": {
        "command": "python", "args": ["-m", "graphify.serve", graph]
    }}})
}

fn project() -> TempDir {
    let dir = tempdir().unwrap();
    write(
        &dir.path().join("graphify-out/graph.json"),
        graph("original"),
    );
    dir
}

fn cli(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_graf"))
        .current_dir(root)
        .args(args)
        .arg("--json")
        .output()
        .unwrap()
}

fn success(output: Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn failure(output: Output) {
    assert!(
        !output.status.success(),
        "unexpected success: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(output.stdout.is_empty());
    assert!(!output.stderr.is_empty());
}

#[cfg(unix)]
#[test]
fn symlink_parent_component_imports_the_actual_configured_graph() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join("actual/subdir")).unwrap();
    std::os::unix::fs::symlink("actual/subdir", root.join("alias")).unwrap();
    let expected = graph("configured");
    let decoy = graph("decoy");
    write(&root.join("actual/graph.json"), &expected);
    write(&root.join("graph.json"), &decoy);
    let before = config("alias/../graph.json").to_string();
    write(&root.join(".mcp.json"), &before);

    success(cli(root, &["switch", "graphify"]));
    let shown = success(cli(root, &["show", "configured"]));
    assert_eq!(shown["nodes"][0]["id"], "configured");
    assert_eq!(
        fs::read_to_string(root.join("actual/graph.json")).unwrap(),
        expected
    );
    assert_eq!(fs::read_to_string(root.join("graph.json")).unwrap(), decoy);
    success(cli(root, &["switch", "--undo"]));
    assert_eq!(fs::read_to_string(root.join(".mcp.json")).unwrap(), before);
}

#[test]
fn ordinary_parent_component_remains_supported() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    fs::create_dir(root.join("subdir")).unwrap();
    write(&root.join("graph.json"), graph("configured"));
    let before = config("subdir/../graph.json").to_string();
    write(&root.join(".mcp.json"), &before);

    success(cli(root, &["switch", "graphify"]));
    let shown = success(cli(root, &["show", "configured"]));
    assert_eq!(shown["nodes"][0]["id"], "configured");
    success(cli(root, &["switch", "--undo"]));
    assert_eq!(fs::read_to_string(root.join(".mcp.json")).unwrap(), before);
}

#[test]
fn equal_count_database_substitution_blocks_repeat_and_redo() {
    for undo_first in [false, true] {
        let dir = project();
        let root = dir.path();
        success(cli(root, &["switch", "graphify"]));
        let database = root.join(".graf/index.db");
        let original = fs::read(&database).unwrap();
        if undo_first {
            success(cli(root, &["switch", "--undo"]));
        }
        let config_before = fs::read(root.join(".mcp.json")).ok();
        let receipt = root.join(".graf/switch-graphify.json");
        let receipt_before = fs::read(&receipt).unwrap();
        write(&root.join("replacement.json"), graph("replacement"));
        let imported = success(cli(
            root,
            &[
                "--db",
                "replacement.db",
                "import",
                "graphify",
                "replacement.json",
            ],
        ));
        assert_eq!(imported["nodes"], 1);
        assert_eq!(imported["edges"], 0);
        let replacement = fs::read(root.join("replacement.db")).unwrap();
        assert_ne!(replacement, original);
        fs::write(&database, &replacement).unwrap();

        failure(cli(root, &["switch", "graphify"]));
        assert_eq!(fs::read(root.join(".mcp.json")).ok(), config_before);
        assert_eq!(fs::read(&receipt).unwrap(), receipt_before);
        assert_eq!(fs::read(&database).unwrap(), replacement);

        // Restoring the recorded snapshot must make the same operation work.
        fs::write(&database, &original).unwrap();
        let report = success(cli(root, &["switch", "graphify"]));
        assert_eq!(
            report["status"],
            if undo_first {
                "switched"
            } else {
                "already_switched"
            }
        );
        assert_eq!(fs::read(&database).unwrap(), original);
    }
}

#[test]
fn rewritten_config_limit_is_checked_before_publishing_migration_state() {
    for entries in [100_000, 1_500_000] {
        let dir = project();
        let root = dir.path();
        let before = format!(
            "{{\"mcpServers\":{},\"preferences\":[{}0]}}",
            config("graphify-out/graph.json")["mcpServers"],
            "0,".repeat(entries - 1)
        );
        assert!(before.len() < 8 * 1024 * 1024);
        write(&root.join(".mcp.json"), &before);
        let source = fs::read(root.join("graphify-out/graph.json")).unwrap();

        let output = cli(root, &["switch", "graphify"]);
        if entries == 1_500_000 {
            failure(output);
            assert!(!root.join(".graf").exists());
        } else {
            success(output);
            success(cli(root, &["switch", "graphify"]));
            success(cli(root, &["switch", "--undo"]));
        }
        assert_eq!(fs::read(root.join(".mcp.json")).unwrap(), before.as_bytes());
        assert_eq!(
            fs::read(root.join("graphify-out/graph.json")).unwrap(),
            source
        );
    }
}

#[test]
fn receipt_from_another_project_and_conflicting_config_are_rejected() {
    let source = project();
    success(cli(source.path(), &["switch", "graphify"]));
    let receipt_bytes = fs::read(source.path().join(".graf/switch-graphify.json")).unwrap();
    let other = project();
    let root = other.path();
    let original_config = config("graphify-out/graph.json").to_string();
    write(&root.join(".mcp.json"), &original_config);
    write(&root.join(".graf/switch-graphify.json"), &receipt_bytes);
    for args in [["switch", "graphify"], ["switch", "--undo"]] {
        failure(cli(root, &args));
        assert_eq!(
            fs::read(root.join(".mcp.json")).unwrap(),
            original_config.as_bytes()
        );
        assert_eq!(
            fs::read(root.join(".graf/switch-graphify.json")).unwrap(),
            receipt_bytes
        );
        assert!(!root.join(".graf/index.db").exists());
    }

    let root = source.path();
    write(&root.join("other.json"), &original_config);
    let switched_config = fs::read(root.join(".mcp.json")).unwrap();
    let database = fs::read(root.join(".graf/index.db")).unwrap();
    for action in ["graphify", "--undo"] {
        failure(cli(root, &["switch", action, "--config", "other.json"]));
        assert_eq!(fs::read(root.join(".mcp.json")).unwrap(), switched_config);
        assert_eq!(
            fs::read(root.join("other.json")).unwrap(),
            original_config.as_bytes()
        );
        assert_eq!(fs::read(root.join(".graf/index.db")).unwrap(), database);
        assert_eq!(
            fs::read(root.join(".graf/switch-graphify.json")).unwrap(),
            receipt_bytes
        );
    }
}

#[test]
fn edits_after_switch_or_undo_are_not_overwritten() {
    for undo_first in [false, true] {
        let dir = project();
        let root = dir.path();
        write(
            &root.join(".mcp.json"),
            config("graphify-out/graph.json").to_string(),
        );
        success(cli(root, &["switch", "graphify"]));
        if undo_first {
            success(cli(root, &["switch", "--undo"]));
        }
        let mut edited = fs::read(root.join(".mcp.json")).unwrap();
        edited.push(b'\n');
        fs::write(root.join(".mcp.json"), &edited).unwrap();
        let receipt = fs::read(root.join(".graf/switch-graphify.json")).unwrap();
        let database = fs::read(root.join(".graf/index.db")).unwrap();
        for args in [["switch", "graphify"], ["switch", "--undo"]] {
            failure(cli(root, &args));
            assert_eq!(fs::read(root.join(".mcp.json")).unwrap(), edited);
            assert_eq!(
                fs::read(root.join(".graf/switch-graphify.json")).unwrap(),
                receipt
            );
            assert_eq!(fs::read(root.join(".graf/index.db")).unwrap(), database);
        }
    }
}

#[test]
fn ordinary_receipt_repeat_preserves_config_database_and_receipt() {
    let dir = project();
    let root = dir.path();
    write(
        &root.join(".mcp.json"),
        config("graphify-out/graph.json").to_string(),
    );
    success(cli(root, &["switch", "graphify"]));
    let paths = [".mcp.json", ".graf/index.db", ".graf/switch-graphify.json"];
    let before = paths.map(|path| fs::read(root.join(path)).unwrap());

    for args in [
        &["switch", "graphify"][..],
        &["switch", "graphify", "--config", ".mcp.json"][..],
    ] {
        assert_eq!(success(cli(root, args))["status"], "already_switched");
        assert_eq!(paths.map(|path| fs::read(root.join(path)).unwrap()), before);
    }
}

#[cfg(unix)]
#[test]
fn receipt_parent_redirect_requires_explicit_resolved_global_config() {
    let dir = project();
    let root = dir.path();
    let external = tempdir().unwrap();
    let original = config("graphify-out/graph.json").to_string();
    write(&root.join(".cursor/mcp.json"), &original);
    success(cli(root, &["switch", "graphify"]));
    let migrated = fs::read(root.join(".cursor/mcp.json")).unwrap();
    let receipt = fs::read(root.join(".graf/switch-graphify.json")).unwrap();
    let database = fs::read(root.join(".graf/index.db")).unwrap();

    let moved = external.path().join("cursor");
    fs::rename(root.join(".cursor"), &moved).unwrap();
    std::os::unix::fs::symlink(&moved, root.join(".cursor")).unwrap();
    let global = moved.join("mcp.json").canonicalize().unwrap();

    for action in ["graphify", "--undo"] {
        failure(cli(root, &["switch", action]));
        assert_eq!(fs::read(&global).unwrap(), migrated);
        assert_eq!(
            fs::read(root.join(".graf/switch-graphify.json")).unwrap(),
            receipt
        );
        assert_eq!(fs::read(root.join(".graf/index.db")).unwrap(), database);
    }

    // Selecting the resolved global path explicitly authorizes the same receipt.
    for (action, status, expected) in [
        ("graphify", "already_switched", migrated.as_slice()),
        ("--undo", "undone", original.as_bytes()),
        ("graphify", "switched", migrated.as_slice()),
    ] {
        let report = success(cli(
            root,
            &["switch", action, "--config", global.to_str().unwrap()],
        ));
        assert_eq!(report["status"], status);
        assert_eq!(Path::new(report["config"].as_str().unwrap()), global);
        assert_eq!(fs::read(&global).unwrap(), expected);
        assert_eq!(
            fs::read(root.join(".graf/switch-graphify.json")).unwrap(),
            receipt
        );
        assert_eq!(fs::read(root.join(".graf/index.db")).unwrap(), database);
        // The opt-in applies to this invocation, not future repeat/redo calls.
        failure(cli(root, &["switch", "graphify"]));
        assert_eq!(fs::read(&global).unwrap(), expected);
    }
}

#[test]
fn receipt_parent_traversal_is_rejected_even_when_config_bytes_match() {
    let dir = tempdir().unwrap();
    let root = dir.path().join("project");
    write(&root.join("graphify-out/graph.json"), graph("original"));
    let original = config("graphify-out/graph.json").to_string();
    write(&root.join(".mcp.json"), &original);
    success(cli(&root, &["switch", "graphify"]));
    let migrated = fs::read(root.join(".mcp.json")).unwrap();
    let database = fs::read(root.join(".graf/index.db")).unwrap();
    let receipt_path = root.join(".graf/switch-graphify.json");
    let mut receipt: Value = serde_json::from_slice(&fs::read(&receipt_path).unwrap()).unwrap();
    receipt["config"] = json!(root.canonicalize().unwrap().join("../global.json"));
    let forged = serde_json::to_vec(&receipt).unwrap();
    fs::write(&receipt_path, &forged).unwrap();
    let global = dir.path().join("global.json");

    // A valid database and exact before/after config bytes must not authorize
    // an external destination hidden behind a project-prefixed path.
    for matching in [original.as_bytes(), migrated.as_slice()] {
        write(&global, matching);
        for action in ["graphify", "--undo"] {
            failure(cli(&root, &["switch", action]));
            assert_eq!(fs::read(&global).unwrap(), matching);
            assert_eq!(fs::read(root.join(".mcp.json")).unwrap(), migrated);
            assert_eq!(fs::read(&receipt_path).unwrap(), forged);
            assert_eq!(fs::read(root.join(".graf/index.db")).unwrap(), database);
        }
    }
}
