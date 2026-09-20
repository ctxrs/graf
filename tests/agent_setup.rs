//! Setup regression checks use only synthetic projects and homes.
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};
use tempfile::{TempDir, tempdir};

struct Sandbox {
    _temp: TempDir,
    project: PathBuf,
    home: PathBuf,
}
impl Sandbox {
    fn new() -> Self {
        let temp = tempdir().unwrap();
        let project = temp.path().join(if cfg!(windows) {
            "project with spaces"
        } else {
            "project with spaces '$()'"
        });
        let home = temp.path().join("synthetic home");
        fs::create_dir_all(&project).unwrap();
        fs::create_dir_all(&home).unwrap();
        Self {
            _temp: temp,
            project,
            home,
        }
    }
    fn command(&self, program: impl AsRef<std::ffi::OsStr>) -> Command {
        let mut command = Command::new(program);
        command
            .current_dir(&self.project)
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .env("XDG_CONFIG_HOME", self.home.join(".config"))
            .env("LOCALAPPDATA", self.home.join("AppData/Local"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", self.home.join("absent.gitconfig"))
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("CLAUDE_CONFIG_DIR")
            .env_remove("CODEX_HOME")
            .env_remove("HERMES_HOME");
        command
    }
    fn cli(&self, args: &[&str]) -> Output {
        self.command(env!("CARGO_BIN_EXE_graf"))
            .arg("--json")
            .args(args)
            .output()
            .unwrap()
    }
    fn ok(&self, args: &[&str]) -> Value {
        success(self.cli(args))
    }
    fn git(&self, args: &[&str]) -> Output {
        self.command("git").args(args).output().unwrap()
    }
    fn init(&self) {
        let output = self.git(&["init", "-q"]);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
fn success(output: Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}
fn fails(output: Output, message: &str) {
    assert!(!output.status.success(), "unexpected success");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains(message),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
fn write(path: &Path, data: impl AsRef<[u8]>) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, data).unwrap();
}

fn legacy_guidance(s: &Sandbox) -> (PathBuf, Vec<u8>, Value) {
    let original = b"# Existing rules\r\nUser spacing preserved.  ".to_vec();
    let mut after = original.clone();
    after.extend_from_slice(
        b"\n\n<!-- graf:begin -->\nOld navigation guidance\n<!-- graf:end -->\n",
    );
    let skill = s.project.join(".agents/skills/graf/SKILL.md");
    let rules = s.project.join("AGENTS.md");
    let old_skill = b"---\nname: graf\n---\nOld navigation guidance\n";
    write(&skill, old_skill);
    write(&rules, &after);
    let receipt = serde_json::json!({"version":1,"scope":s.project,"changes":[
        {"path":skill,"before":null,"after":old_skill.to_vec(),"permission_source":null,"executable":false},
        {"path":rules,"before":original,"after":after,"permission_source":null,"executable":false}
    ]});
    let path = s.project.join(".graf/setup/agents-skill.json");
    write(&path, serde_json::to_vec(&receipt).unwrap());
    (path, original, receipt)
}

#[test]
fn legacy_guidance_upgrades_without_losing_original_undo_or_edit_protection() {
    let s = Sandbox::new();
    let (path, original, _) = legacy_guidance(&s);
    let upgraded = s.ok(&["install"]);
    assert_eq!(upgraded["status"], "installed");
    assert!(upgraded["notes"].to_string().contains("Guidance version 1"));
    let receipt: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(receipt["guidance_version"], 1);
    assert_eq!(receipt["changes"][1]["before"], serde_json::json!(original));
    assert!(
        receipt["changes"]
            .as_array()
            .unwrap()
            .iter()
            .all(|c| c.get("previous_after").is_none())
    );
    assert_eq!(s.ok(&["install"])["status"], "unchanged");
    s.ok(&["uninstall"]);
    assert_eq!(fs::read(s.project.join("AGENTS.md")).unwrap(), original);
    assert!(!s.project.join(".agents/skills/graf/SKILL.md").exists());

    let (path, _, _) = legacy_guidance(&s);
    let before = fs::read(&path).unwrap();
    write(&s.project.join("AGENTS.md"), "Later user edit");
    fails(s.cli(&["install"]), "changed after installation");
    assert_eq!(fs::read(path).unwrap(), before);
    assert_eq!(
        fs::read_to_string(s.project.join("AGENTS.md")).unwrap(),
        "Later user edit"
    );
}

#[test]
fn interrupted_guidance_upgrade_resumes_or_uninstalls_from_recorded_bytes() {
    for action in ["install", "uninstall"] {
        let s = Sandbox::new();
        let (path, original, old) = legacy_guidance(&s);
        s.ok(&["install"]);
        let mut upgraded: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        for (index, change) in upgraded["changes"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .enumerate()
        {
            change["previous_after"] = old["changes"][index]["after"].clone();
        }
        // Model an interruption after updating the skill, before updating AGENTS.md.
        let old_rules: Vec<u8> =
            serde_json::from_value(old["changes"][1]["after"].clone()).unwrap();
        write(&s.project.join("AGENTS.md"), old_rules);
        write(&path, serde_json::to_vec(&upgraded).unwrap());
        s.ok(&[action]);
        if action == "install" {
            assert!(
                fs::read_to_string(s.project.join("AGENTS.md"))
                    .unwrap()
                    .contains("graf provider")
            );
            s.ok(&["uninstall"]);
        }
        assert_eq!(fs::read(s.project.join("AGENTS.md")).unwrap(), original);
        assert!(!path.exists());
    }
}

#[test]
fn normal_invocation_notices_are_directional_bounded_and_read_only() {
    let s = Sandbox::new();
    write(
        &s.project.join("example.py"),
        "def fixture_symbol():\n    return 1\n",
    );
    s.ok(&["index", "."]);
    s.ok(&["install"]);
    let skill = s.project.join(".agents/skills/graf/SKILL.md");
    let receipt = s.project.join(".graf/setup/agents-skill.json");
    // Notices must not parse receipts/configuration or expose their contents.
    write(&receipt, "synthetic-private-marker: not JSON");
    let db = s.project.join(".graf/index.db");
    let database = fs::read(&db).unwrap();
    fs::remove_file(s.project.join("example.py")).unwrap();
    let guidance = fs::read(s.project.join("AGENTS.md")).unwrap();
    for (package, revision, direction) in [
        ("0.0.0", 1, "older"),
        ("999999.0.0", 1, "newer"),
        (env!("CARGO_PKG_VERSION"), 2, "newer"),
        (env!("CARGO_PKG_VERSION"), 1, ""),
        ("not-a-version", 1, ""),
    ] {
        let content = format!(
            "<!-- graf guidance version: {revision}; executable: {package} -->\nsynthetic-private-marker\n"
        );
        write(&skill, &content);
        let result = s.cli(&["query", "fixture_symbol"]);
        let stderr = String::from_utf8_lossy(&result.stderr).into_owned();
        let value = success(result);
        assert!(!value["nodes"].as_array().unwrap().is_empty());
        if direction.is_empty() {
            assert!(stderr.is_empty(), "{stderr}");
        } else {
            assert_eq!(
                stderr.matches("Installed guidance is").count(),
                1,
                "{stderr}"
            );
            assert!(stderr.contains(direction), "{stderr}");
            if direction == "older" {
                assert!(stderr.contains("Rerun `graf install`"));
            } else {
                assert!(stderr.contains("Upgrade the Graf executable"));
                assert!(!stderr.contains("Rerun `graf install`"));
            }
        }
        assert!(!stderr.contains("synthetic-private-marker"));
        assert!(!stderr.contains(s.project.to_str().unwrap()));
        assert_eq!(fs::read_to_string(&skill).unwrap(), content);
        assert_eq!(fs::read(&db).unwrap(), database);
        assert_eq!(fs::read(s.project.join("AGENTS.md")).unwrap(), guidance);
        assert_eq!(
            fs::read_to_string(&receipt).unwrap(),
            "synthetic-private-marker: not JSON"
        );
    }
    // A valid-looking stamp beyond the bounded header isn't discovered.
    write(
        &skill,
        format!(
            "{}\n<!-- graf guidance version: 1; executable: 0.0.0 -->\n",
            "x".repeat(4096)
        ),
    );
    let result = s.cli(&["query", "fixture_symbol"]);
    assert!(result.stderr.is_empty());
    success(result);
    fs::remove_file(&skill).unwrap();
    let result = s.cli(&["query", "fixture_symbol"]);
    assert!(result.stderr.is_empty());
    success(result);
    assert_eq!(fs::read(db).unwrap(), database);
    write(
        &s.project.join("example.py"),
        "def fixture_symbol():\n    return 1\n",
    );
    for (package, direction) in [("0.0.0", "older"), ("999999.0.0", "newer")] {
        let content = format!("<!-- graf guidance version: 1; executable: {package} -->\n");
        write(&skill, &content);
        let result = s.cli(&["index", "."]);
        assert!(String::from_utf8_lossy(&result.stderr).contains(direction));
        success(result); // Normal index JSON also remains a single parseable value.
        assert_eq!(fs::read_to_string(&skill).unwrap(), content);
        assert_eq!(
            fs::read_to_string(&receipt).unwrap(),
            "synthetic-private-marker: not JSON"
        );
    }
}

#[test]
fn executable_stamp_refresh_preserves_undo_and_refuses_newer_guidance() {
    for package in ["0.0.0", "999999.0.0"] {
        let s = Sandbox::new();
        let original = b"Original rules\r\nKeep spacing.  ";
        write(&s.project.join("AGENTS.md"), original);
        s.ok(&["install"]);
        let receipt_path = s.project.join(".graf/setup/agents-skill.json");
        let mut receipt: Value = serde_json::from_slice(&fs::read(&receipt_path).unwrap()).unwrap();
        for change in receipt["changes"].as_array_mut().unwrap() {
            let bytes: Vec<u8> = serde_json::from_value(change["after"].clone()).unwrap();
            let changed = String::from_utf8(bytes).unwrap().replace(
                &format!("executable: {} -->", env!("CARGO_PKG_VERSION")),
                &format!("executable: {package} -->"),
            );
            write(Path::new(change["path"].as_str().unwrap()), &changed);
            change["after"] = serde_json::json!(changed.into_bytes());
        }
        let stamped_receipt = serde_json::to_vec(&receipt).unwrap();
        write(&receipt_path, &stamped_receipt);
        let skill = s.project.join(".agents/skills/graf/SKILL.md");
        let installed = fs::read(&skill).unwrap();
        if package == "0.0.0" {
            let mut edited = installed.clone();
            edited.extend_from_slice(b"User edit\n");
            write(&skill, edited);
            fails(s.cli(&["install"]), "changed after installation");
            assert_eq!(fs::read(&receipt_path).unwrap(), stamped_receipt);
            write(&skill, installed);
            assert_eq!(s.ok(&["install"])["status"], "installed");
            assert!(
                fs::read_to_string(&skill)
                    .unwrap()
                    .contains(&format!("executable: {} -->", env!("CARGO_PKG_VERSION")))
            );
            assert_eq!(s.ok(&["install"])["status"], "unchanged");
        } else {
            fails(s.cli(&["install"]), "upgrade Graf to avoid a downgrade");
            assert_eq!(fs::read(&receipt_path).unwrap(), stamped_receipt);
            assert_eq!(fs::read(&skill).unwrap(), installed);
        }
        s.ok(&["uninstall"]);
        assert_eq!(fs::read(s.project.join("AGENTS.md")).unwrap(), original);
        assert!(!skill.exists());
    }
}

#[test]
fn portable_guidance_preserves_bytes_and_exact_undo() {
    let s = Sandbox::new();
    let original = b"# User rules\r\n\r\nKeep this spacing.  \r\nNo final newline";
    let guidance = s.project.join("AGENTS.md");
    write(&guidance, original);
    let installed = s.ok(&["install"]);
    assert_eq!(installed["status"], "installed");
    let after = fs::read(&guidance).unwrap();
    assert!(after.starts_with(original));
    assert!(String::from_utf8_lossy(&after).contains("Read source files whenever useful"));
    assert!(s.project.join(".agents/skills/graf/SKILL.md").is_file());
    let skill = fs::read_to_string(s.project.join(".agents/skills/graf/SKILL.md")).unwrap();
    for workflow in [
        "graf add",
        "graf provider",
        "--deep",
        "--code-only",
        "--vision",
        "graf watch",
        "graf export",
        "graf global",
        "graf diagnose",
        "graf label",
        "graf benchmark",
        "--check-freshness",
        "--labels",
        "Queries never rebuild",
        "incur costs",
        "guidance version:",
    ] {
        assert!(
            skill.contains(workflow),
            "missing installed guidance: {workflow}"
        );
    }
    assert!(!s.home.join(".agents").exists());
    assert_eq!(s.ok(&["install"])["status"], "unchanged");
    assert_eq!(fs::read(&guidance).unwrap(), after);
    assert_eq!(s.ok(&["uninstall"])["status"], "uninstalled");
    assert_eq!(fs::read(&guidance).unwrap(), original);
    assert!(!s.project.join(".agents/skills/graf/SKILL.md").exists());
    assert_eq!(s.ok(&["uninstall"])["status"], "unchanged");
}

#[test]
fn later_edit_refuses_entire_uninstall_and_reinstall() {
    let s = Sandbox::new();
    s.ok(&["install", "--platform", "claude", "--skill", "--mcp"]);
    let skill = s.project.join(".claude/skills/graf/SKILL.md");
    let original = fs::read(&skill).unwrap();
    let config = s.project.join(".mcp.json");
    let mut config_value: Value = serde_json::from_slice(&fs::read(&config).unwrap()).unwrap();
    config_value["later"] = Value::Bool(true);
    write(&config, serde_json::to_vec(&config_value).unwrap());
    fails(
        s.cli(&["uninstall", "--platform", "claude", "--skill", "--mcp"]),
        "changed after installation",
    );
    assert_eq!(fs::read(&skill).unwrap(), original);
    assert!(s.project.join("CLAUDE.md").is_file());
    fails(
        s.cli(&["install", "--platform", "claude", "--mcp"]),
        "changed after installation",
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(config).unwrap()).unwrap(),
        config_value
    );
}

#[test]
fn json_servers_and_unrelated_bytes_survive_install_and_undo() {
    let s = Sandbox::new();
    let original = b"{\n  \"mcpServers\" : {\"graphify\":{\"command\":\"keep-graphify\"}, \"other\": {\"env\": {\"SECRET\": \"synthetic\"}}},\n  \"preferences\": [ 1,  2, 3 ]\n}\n";
    let path = s.project.join(".mcp.json");
    write(&path, original);
    s.ok(&["install", "--platform", "claude", "--mcp"]);
    let bytes = fs::read(&path).unwrap();
    assert!(String::from_utf8_lossy(&bytes).contains("\"preferences\": [ 1,  2, 3 ]"));
    assert!(
        String::from_utf8_lossy(&bytes)
            .contains("\"other\": {\"env\": {\"SECRET\": \"synthetic\"}}")
    );
    let config: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        config["mcpServers"]["graf"]["args"][1],
        s.project.join(".graf/index.db").to_str().unwrap()
    );
    assert_eq!(config["mcpServers"]["graphify"]["command"], "keep-graphify");
    assert!(!s.project.join("CLAUDE.md").exists());
    assert_eq!(
        s.ok(&["install", "--platform", "claude", "--mcp"])["status"],
        "unchanged"
    );
    s.ok(&["uninstall", "--platform", "claude", "--mcp"]);
    assert_eq!(fs::read(&path).unwrap(), original);
}

#[test]
fn toml_and_host_native_formats_round_trip() {
    let s = Sandbox::new();
    let path = s.project.join(".codex/config.toml");
    let original = b"# personal preferences\nmodel = 'example' # preserve\n\n[mcp_servers.other]\ncommand = 'other' # preserve too\n";
    write(&path, original);
    s.ok(&["install", "--platform", "codex", "--mcp"]);
    let result = fs::read_to_string(&path).unwrap();
    assert!(result.contains("model = 'example' # preserve"));
    assert!(result.contains("command = 'other' # preserve too"));
    let doc: toml_edit::DocumentMut = result.parse().unwrap();
    assert!(doc["mcp_servers"]["graf"]["command"].is_str());
    s.ok(&["uninstall", "--platform", "codex", "--mcp"]);
    assert_eq!(fs::read(path).unwrap(), original);
    for (host, file, key) in [
        ("cursor", ".cursor/mcp.json", "mcpServers"),
        ("gemini", ".gemini/settings.json", "mcpServers"),
        ("vscode", ".vscode/mcp.json", "servers"),
    ] {
        s.ok(&["install", "--platform", host, "--mcp"]);
        let value: Value =
            serde_json::from_slice(&fs::read(s.project.join(file)).unwrap()).unwrap();
        assert_eq!(value[key]["graf"]["args"][2], "serve");
        s.ok(&["uninstall", "--platform", host, "--mcp"]);
        assert!(!s.project.join(file).exists());
    }
}

#[test]
fn global_scope_is_explicit_and_uses_synthetic_home() {
    let s = Sandbox::new();
    s.ok(&[
        "install",
        "--platform",
        "codex",
        "--global",
        "--skill",
        "--mcp",
    ]);
    assert!(s.home.join(".agents/skills/graf/SKILL.md").exists());
    assert!(!s.project.join(".agents").exists());
    let doc: toml_edit::DocumentMut = fs::read_to_string(s.home.join(".codex/config.toml"))
        .unwrap()
        .parse()
        .unwrap();
    let args = doc["mcp_servers"]["graf"]["args"].as_array().unwrap();
    assert_eq!(args.len(), 1);
    assert_eq!(args.get(0).unwrap().as_str(), Some("serve"));
    s.ok(&[
        "uninstall",
        "--platform",
        "codex",
        "--global",
        "--skill",
        "--mcp",
    ]);
    assert!(!s.home.join(".codex/config.toml").exists());
    fails(
        s.cli(&["install", "--global", "--project", "."]),
        "cannot be used with",
    );
}

#[test]
fn jsonc_preserves_comments_byte_blocks_permissions_and_exact_undo() {
    for (host, file, key, original) in [
        (
            "vscode",
            ".vscode/mcp.json",
            "servers",
            "// café root comment\r\n{\r\n  \"servers\": { // leading\r\n    \"other\": {\"command\":\"keep // /* literal\",}, // trailing\r\n  },\r\n  \"inputs\": [ /* untouched */ ],\r\n} // final\r\n",
        ),
        (
            "vscode",
            ".vscode/mcp.json",
            "servers",
            "/* empty */ { // no members\n}\n",
        ),
        (
            "vscode",
            ".vscode/mcp.json",
            "servers",
            "{\"inputs\":[], /* last member */}\n",
        ),
        (
            "gemini",
            ".gemini/settings.json",
            "mcpServers",
            "/* café */ {\"mcpServers\": {/* empty */},\"theme\":\"dark\"} // end\n",
        ),
    ] {
        let s = Sandbox::new();
        let path = s.project.join(file);
        write(&path, original);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        }
        s.ok(&["install", "--platform", host, "--mcp"]);
        let installed = fs::read_to_string(&path).unwrap();
        let config = jsonc_parser::parse_to_serde_value(&installed, &Default::default())
            .unwrap()
            .unwrap();
        assert_eq!(config[key]["graf"]["args"][2], "serve");
        // The change is a single insertion: all original bytes, including CRLF,
        // comments, spacing, and trailing commas, remain in their original order.
        let prefix = original
            .bytes()
            .zip(installed.bytes())
            .take_while(|(a, b)| a == b)
            .count();
        assert!(installed.ends_with(&original[prefix..]));
        assert_eq!(
            s.ok(&["install", "--platform", host, "--mcp"])["status"],
            "unchanged"
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), installed);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o640
            );
        }
        write(&path, format!("{installed}\n// user edit\n"));
        fails(
            s.cli(&["uninstall", "--platform", host, "--mcp"]),
            "changed",
        );
        assert!(
            fs::read_to_string(&path)
                .unwrap()
                .ends_with("// user edit\n")
        );
        write(&path, installed);
        s.ok(&["uninstall", "--platform", host, "--mcp"]);
        assert_eq!(fs::read(&path).unwrap(), original.as_bytes());
    }
}

#[test]
fn jsonc_rejects_duplicates_and_extensions_outside_host_contract() {
    for (host, file, original) in [
        (
            "vscode",
            ".vscode/mcp.json",
            "{\"servers\":{}, /* duplicate */ \"servers\":{}}",
        ),
        (
            "vscode",
            ".vscode/mcp.json",
            "{\"servers\":{\"graf\":{}, /* duplicate */ \"gr\\u0061f\":{}}}",
        ),
        (
            "vscode",
            ".vscode/mcp.json",
            "{\"servers\":{\"x\":{} \"y\":{}}}",
        ),
        ("vscode", ".vscode/mcp.json", "{servers: {}}"),
        ("vscode", ".vscode/mcp.json", "{'servers': {}}"),
        (
            "gemini",
            ".gemini/settings.json",
            "{/* comments supported */ \"mcpServers\":{},}",
        ),
        (
            "cursor",
            ".cursor/mcp.json",
            "{/* JSONC not verified */ \"mcpServers\":{}}",
        ),
    ] {
        let s = Sandbox::new();
        let path = s.project.join(file);
        write(&path, original);
        assert!(
            !s.cli(&["install", "--platform", host, "--mcp"])
                .status
                .success()
        );
        assert_eq!(fs::read(&path).unwrap(), original.as_bytes());
        assert!(
            !s.project
                .join(format!(".graf/setup/{host}-mcp.json"))
                .exists()
        );
    }
}

#[test]
fn vscode_explicit_profile_is_isolated_and_reversible() {
    let s = Sandbox::new();
    let profile = s.home.join("Code/User/profiles/profile with spaces '$()'");
    let config = profile.join("mcp.json");
    let original =
        b"// profile-specific MCP\n{\"servers\": {\"other\": {\"command\": \"keep\"}},}\n";
    write(&config, original);
    let settings = profile.join("settings.json");
    write(&settings, "{\"editor.fontSize\": 15}");
    let other = s.home.join("Code/User/mcp.json");
    write(&other, "{\"servers\":{\"different\":{}}}");
    let args = [
        "--platform",
        "vscode",
        "--global",
        "--profile",
        profile.to_str().unwrap(),
        "--mcp",
        "--skill",
    ];
    let result = s.ok(&[&["install"][..], args.as_slice()].concat());
    assert_eq!(
        result["scope"],
        profile.canonicalize().unwrap().to_str().unwrap()
    );
    assert!(profile.join(".graf/setup/vscode-mcp.json").is_file());
    assert!(s.home.join(".copilot/skills/graf/SKILL.md").is_file());
    assert!(!profile.join(".copilot").exists());
    let value = jsonc_parser::parse_to_serde_value(
        &fs::read_to_string(&config).unwrap(),
        &Default::default(),
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        value["servers"]["graf"]["args"],
        serde_json::json!(["serve"])
    );
    assert_eq!(value["servers"]["graf"]["type"], "stdio");
    assert_eq!(
        s.ok(&[&["install"][..], args.as_slice()].concat())["status"],
        "unchanged"
    );
    s.ok(&[&["uninstall"][..], args.as_slice()].concat());
    assert_eq!(fs::read(config).unwrap(), original);
    assert_eq!(
        fs::read_to_string(settings).unwrap(),
        "{\"editor.fontSize\": 15}"
    );
    assert_eq!(
        fs::read_to_string(other).unwrap(),
        "{\"servers\":{\"different\":{}}}"
    );
    assert!(!s.home.join(".copilot/skills/graf/SKILL.md").exists());
    assert_eq!(
        s.ok(&[&["uninstall"][..], args.as_slice()].concat())["status"],
        "unchanged"
    );
}

#[test]
fn custom_native_roots_require_explicit_selection_and_preserve_originals() {
    for (host, variable) in [("codex", "CODEX_HOME"), ("hermes", "HERMES_HOME")] {
        let s = Sandbox::new();
        let native = s.home.join("custom root with spaces '$()'");
        fs::create_dir(&native).unwrap();
        let mut implicit = s.command(env!("CARGO_BIN_EXE_graf"));
        implicit
            .env(variable, &native)
            .args(["--json", "install", "--platform", host, "--global"]);
        fails(
            implicit.output().unwrap(),
            "select its existing directory explicitly",
        );
        assert!(!s.home.join(".graf").exists());
        let original = b"# User configuration\r\nUnrelated original bytes.  ";
        let config = native.join("config.toml");
        if host == "codex" {
            write(&native.join("AGENTS.md"), original);
            write(&config, "# Keep\nmodel = 'example'\n");
        }
        let mut args = vec![
            "--platform",
            host,
            "--global",
            "--config-root",
            native.to_str().unwrap(),
            "--skill",
        ];
        if host == "codex" {
            args.push("--mcp");
        }
        let mut install = s.command(env!("CARGO_BIN_EXE_graf"));
        install
            .env(variable, &native)
            .args(["--json", "install"])
            .args(&args);
        let result = success(install.output().unwrap());
        assert_eq!(
            result["scope"],
            native.canonicalize().unwrap().to_str().unwrap()
        );
        let skill = if host == "codex" {
            assert!(!s.home.join(".codex/config.toml").exists());
            assert!(!native.join(".agents").exists());
            s.home.join(".agents/skills/graf/SKILL.md")
        } else {
            assert!(!s.home.join(".hermes").exists());
            native.join("skills/graf/SKILL.md")
        };
        assert!(skill.is_file());
        assert!(
            native
                .join(format!(".graf/setup/{host}-skill.json"))
                .is_file()
        );
        assert_eq!(
            s.ok(&[&["install"][..], args.as_slice()].concat())["status"],
            "unchanged"
        );
        s.ok(&[&["uninstall"][..], args.as_slice()].concat());
        assert!(!skill.exists());
        if host == "codex" {
            assert_eq!(fs::read(native.join("AGENTS.md")).unwrap(), original);
            assert_eq!(
                fs::read_to_string(config).unwrap(),
                "# Keep\nmodel = 'example'\n"
            );
        }
    }
}

#[test]
fn claude_custom_root_preserves_defaults_and_uses_native_config_precedence() {
    for legacy in [false, true] {
        let s = Sandbox::new();
        s.ok(&[
            "install",
            "--platform",
            "claude",
            "--global",
            "--skill",
            "--mcp",
        ]);
        let defaults: Vec<_> = [
            ".claude/CLAUDE.md",
            ".claude/skills/graf/SKILL.md",
            ".claude.json",
            ".graf/setup/claude-skill.json",
            ".graf/setup/claude-mcp.json",
        ]
        .into_iter()
        .map(|name| {
            let path = s.home.join(name);
            let bytes = fs::read(&path).unwrap();
            (path, bytes)
        })
        .collect();
        let native = s.home.join("custom Claude with spaces '$()'");
        let rules = native.join("CLAUDE.md");
        let original_rules = b"# Existing user rules\r\nPreserve exactly.  ";
        write(&rules, original_rules);
        let modern = native.join(".claude.json");
        let original_config =
            b"{\"mcpServers\":{\"other\":{\"command\":\"preserve\"}},\"preferences\":[ 1,  2 ]}\n";
        write(&modern, original_config);
        let config = if legacy {
            let path = native.join(".config.json");
            write(&path, original_config);
            path
        } else {
            modern.clone()
        };
        let args = [
            "--platform",
            "claude",
            "--global",
            "--config-root",
            native.to_str().unwrap(),
            "--skill",
            "--mcp",
        ];
        let mut implicit = s.command(env!("CARGO_BIN_EXE_graf"));
        implicit.env("CLAUDE_CONFIG_DIR", &native).args([
            "--json",
            "install",
            "--platform",
            "claude",
            "--global",
        ]);
        fails(
            implicit.output().unwrap(),
            "select its existing directory explicitly",
        );
        let mut install = s.command(env!("CARGO_BIN_EXE_graf"));
        install
            .env("CLAUDE_CONFIG_DIR", &native)
            .args(["--json", "install"])
            .args(args);
        success(install.output().unwrap());
        let skill = native.join("skills/graf/SKILL.md");
        assert!(skill.is_file());
        assert!(!native.join(".claude").exists());
        assert!(native.join(".graf/setup/claude-skill.json").is_file());
        // Normal invocation inspects the active custom Claude skill, not an
        // unrelated default profile, and never upgrades the file implicitly.
        let skill_bytes = fs::read_to_string(&skill).unwrap();
        let older = skill_bytes.replace(
            &format!("executable: {} -->", env!("CARGO_PKG_VERSION")),
            "executable: 0.0.0 -->",
        );
        write(&skill, &older);
        write(
            &s.project.join("example.py"),
            "def fixture_symbol():\n    return 1\n",
        );
        let mut normal = s.command(env!("CARGO_BIN_EXE_graf"));
        normal
            .env("CLAUDE_CONFIG_DIR", &native)
            .args(["--json", "index", "."]);
        let result = normal.output().unwrap();
        assert!(String::from_utf8_lossy(&result.stderr).contains("global guidance for claude"));
        assert!(String::from_utf8_lossy(&result.stderr).contains("older"));
        success(result);
        assert_eq!(fs::read_to_string(&skill).unwrap(), older);
        write(&skill, skill_bytes);
        let value: Value = serde_json::from_slice(&fs::read(&config).unwrap()).unwrap();
        assert_eq!(
            value["mcpServers"]["graf"]["args"],
            serde_json::json!(["serve"])
        );
        assert_eq!(value["mcpServers"]["other"]["command"], "preserve");
        if legacy {
            assert_eq!(fs::read(&modern).unwrap(), original_config);
        }
        for (path, bytes) in &defaults {
            assert_eq!(&fs::read(path).unwrap(), bytes);
        }
        assert_eq!(
            s.ok(&[&["install"][..], args.as_slice()].concat())["status"],
            "unchanged"
        );
        let installed_rules = fs::read(&rules).unwrap();
        let installed_config = fs::read(&config).unwrap();
        write(&rules, "Later user rules");
        fails(
            s.cli(&[&["uninstall"][..], args.as_slice()].concat()),
            "changed after installation",
        );
        assert_eq!(fs::read(&config).unwrap(), installed_config);
        assert!(skill.exists());
        write(&rules, installed_rules);
        s.ok(&[&["uninstall"][..], args.as_slice()].concat());
        assert_eq!(fs::read(rules).unwrap(), original_rules);
        assert_eq!(fs::read(config).unwrap(), original_config);
        assert!(!skill.exists());
        for (path, bytes) in defaults {
            assert_eq!(fs::read(path).unwrap(), bytes);
        }
    }
}

#[test]
fn global_cursor_vscode_and_hermes_use_native_personal_skills() {
    for host in ["cursor", "vscode", "hermes"] {
        let s = Sandbox::new();
        let path = s.home.join(match host {
            "cursor" => ".cursor/skills/graf/SKILL.md",
            "vscode" => ".copilot/skills/graf/SKILL.md",
            _ if cfg!(windows) => "AppData/Local/hermes/skills/graf/SKILL.md",
            _ => ".hermes/skills/graf/SKILL.md",
        });
        let result = s.ok(&["install", "--platform", host, "--global"]);
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("---\nname: graf\n"));
        assert!(!text.contains("alwaysApply: true"));
        assert!(
            result["files"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v == path.to_str().unwrap())
        );
        if host == "cursor" {
            assert!(
                result["notes"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|v| v.as_str().unwrap().contains("User Rules"))
            );
            assert!(!s.home.join(".cursor/rules/graf.mdc").exists());
        }
        s.ok(&["uninstall", "--platform", host, "--global"]);
        assert!(!path.exists());
        assert!(!s.project.join(".graf").exists());
    }
}

#[test]
fn root_and_profile_selection_reject_unsupported_or_missing_directories() {
    let s = Sandbox::new();
    let directory = s.home.to_str().unwrap();
    fails(s.cli(&["install", "--config-root", directory]), "--global");
    fails(s.cli(&["install", "--profile", directory]), "--global");
    fails(
        s.cli(&[
            "install",
            "--global",
            "--profile",
            directory,
            "--config-root",
            directory,
        ]),
        "cannot be used with",
    );
    fails(
        s.cli(&[
            "install",
            "--platform",
            "cursor",
            "--global",
            "--config-root",
            directory,
        ]),
        "only for Claude, Codex and Hermes",
    );
    fails(
        s.cli(&[
            "install",
            "--platform",
            "codex",
            "--global",
            "--profile",
            directory,
        ]),
        "VS Code user-profile",
    );
    fails(
        s.cli(&["install", "--platform", "vscode", "--global", "--mcp"]),
        "requires --profile PATH",
    );
    for flag in ["--config-root", "--profile"] {
        let host = if flag == "--profile" {
            "vscode"
        } else {
            "hermes"
        };
        let absent = s.home.join("not an existing profile");
        assert!(
            !s.cli(&[
                "install",
                "--platform",
                host,
                "--global",
                flag,
                absent.to_str().unwrap()
            ])
            .status
            .success()
        );
        assert!(!absent.exists());
        let file = s.home.join("a file instead of a directory");
        write(&file, "untouched");
        fails(
            s.cli(&[
                "install",
                "--platform",
                host,
                "--global",
                flag,
                file.to_str().unwrap(),
            ]),
            "real directory",
        );
        assert_eq!(fs::read_to_string(file).unwrap(), "untouched");
    }
    #[cfg(unix)]
    {
        let linked = s.home.join("linked profile");
        std::os::unix::fs::symlink(&s.project, &linked).unwrap();
        fails(
            s.cli(&[
                "install",
                "--platform",
                "vscode",
                "--global",
                "--profile",
                linked.to_str().unwrap(),
                "--mcp",
            ]),
            "symlink",
        );
    }
    assert!(!s.home.join(".graf").exists());
    assert!(!s.project.join(".graf").exists());
}

#[test]
fn unowned_invalid_unsupported_and_duplicate_configs_are_unchanged() {
    let s = Sandbox::new();
    for original in [
        "{\"mcpServers\": {\"graf\": {\"command\":\"user\"}}}",
        "{\"mcpServers\": {}, \"mcpServers\": {}}",
        "{\"mcpServers\": {\"x\":{}, \"x\":{}}}",
        "{ /* comments */ \"mcpServers\": {} }",
        "{\"mcpServers\":null}",
    ] {
        let path = s.project.join(".mcp.json");
        write(&path, original);
        assert!(
            !s.cli(&["install", "--platform", "claude", "--mcp"])
                .status
                .success()
        );
        assert_eq!(fs::read_to_string(path).unwrap(), original);
    }
    write(
        &s.project.join(".agents/skills/graf/SKILL.md"),
        "user-owned",
    );
    fails(s.cli(&["install"]), "unowned file already exists");
    fails(
        s.cli(&["install", "--platform", "made-up"]),
        "unknown platform",
    );
    fails(
        s.cli(&["install", "--platform", "agents", "--mcp"]),
        "not verified",
    );
    assert!(!s.project.join("AGENTS.md").exists());
}

#[test]
fn skill_matrix_installs_only_named_scope_and_removes_owned_files() {
    for host in [
        "agents",
        "claude",
        "codex",
        "cursor",
        "gemini",
        "opencode",
        "kilo",
        "copilot",
        "claw",
        "droid",
        "trae",
        "trae-cn",
        "hermes",
        "kiro",
        "pi",
        "codebuddy",
        "antigravity",
        "kimi",
        "amp",
        "devin",
        "vscode",
    ] {
        let s = Sandbox::new();
        let result = s.ok(&["install", "--platform", host]);
        let paths: Vec<PathBuf> = result["files"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| PathBuf::from(v.as_str().unwrap()))
            .collect();
        assert!(!paths.is_empty());
        assert!(
            paths
                .iter()
                .all(|p| p.starts_with(&s.project) && p.is_file())
        );
        s.ok(&["uninstall", "--platform", host]);
        assert!(paths.iter().all(|p| !p.exists()));
    }
}

#[cfg(unix)]
#[test]
fn permissions_symlinks_and_hook_chaining() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let s = Sandbox::new();
    s.init();
    let config = s.project.join(".mcp.json");
    write(&config, "{}");
    fs::set_permissions(&config, fs::Permissions::from_mode(0o640)).unwrap();
    s.ok(&["install", "--platform", "claude", "--mcp"]);
    assert_eq!(
        fs::metadata(&config).unwrap().permissions().mode() & 0o777,
        0o640
    );
    let receipt = s.project.join(".graf/setup/claude-mcp.json");
    assert_eq!(
        fs::metadata(&receipt).unwrap().permissions().mode() & 0o777,
        0o600
    );
    s.ok(&["uninstall", "--platform", "claude", "--mcp"]);
    assert_eq!(
        fs::metadata(&config).unwrap().permissions().mode() & 0o777,
        0o640
    );
    fs::remove_file(&config).unwrap();
    let outside = s.home.join("config");
    write(&outside, "{}");
    symlink(&outside, &config).unwrap();
    fails(
        s.cli(&["install", "--platform", "claude", "--mcp"]),
        "regular file",
    );
    assert_eq!(fs::read_to_string(outside).unwrap(), "{}");

    let hook = s.project.join(".git/hooks/post-commit");
    let original = b"#!/bin/sh\nprintf '%s\\n' \"$1\" > original-ran\nexit 7\n";
    write(&hook, original);
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o751)).unwrap();
    s.ok(&["hook", "install"]);
    assert_eq!(s.ok(&["hook", "install"])["status"], "unchanged");
    assert_eq!(s.ok(&["hook", "status"])["status"], "installed");
    let output = s
        .command(&hook)
        .arg("space $() ' untouched")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(7));
    assert_eq!(
        fs::read_to_string(s.project.join("original-ran")).unwrap(),
        "space $() ' untouched\n"
    );
    assert!(!s.project.join(".graf/index.db").exists());
    s.ok(&["hook", "uninstall"]);
    assert_eq!(fs::read(&hook).unwrap(), original);
    assert_eq!(
        fs::metadata(&hook).unwrap().permissions().mode() & 0o777,
        0o751
    );
    assert!(
        !s.project
            .join(".git/hooks/post-commit.graf-original")
            .exists()
    );
    assert!(!s.project.join(".git/hooks/post-checkout").exists());
    assert_eq!(s.ok(&["hook", "status"])["status"], "not-installed");
}

#[test]
fn hook_status_is_read_only_and_modified_hook_prevents_partial_uninstall() {
    let s = Sandbox::new();
    s.init();
    assert_eq!(s.ok(&["hook", "status"])["status"], "not-installed");
    assert!(!s.project.join(".graf").exists());
    s.ok(&["hook", "install"]);
    let commit = s.project.join(".git/hooks/post-commit");
    let installed = fs::read(&commit).unwrap();
    let checkout = s.project.join(".git/hooks/post-checkout");
    write(&checkout, "#!/bin/sh\n# user's later edit\n");
    assert_eq!(s.ok(&["hook", "status"])["status"], "modified");
    fails(s.cli(&["hook", "uninstall"]), "changed after installation");
    assert_eq!(fs::read(&commit).unwrap(), installed);
    let ignore = s.git(&[
        "check-ignore",
        ".graf/index.db",
        ".graf/setup/git-hooks.json",
    ]);
    assert!(ignore.status.success());
}

#[test]
fn external_hooks_path_is_not_modified() {
    let s = Sandbox::new();
    s.init();
    let hooks = s.home.join("global hooks");
    fs::create_dir_all(&hooks).unwrap();
    assert!(
        s.git(&["config", "core.hooksPath", hooks.to_str().unwrap()])
            .status
            .success()
    );
    fails(s.cli(&["hook", "install"]), "outside this repository");
    assert_eq!(fs::read_dir(hooks).unwrap().count(), 0);
}

#[test]
fn aider_read_configuration_preserves_existing_files_and_exact_undo() {
    for original in [
        "# keep this comment\nmodel: example\n",
        "model: example\nread: CONVENTIONS.md\n",
        "read: [CONVENTIONS.md, 'file with spaces.md']\n",
        "{model: example, read: [CONVENTIONS.md]}\n",
    ] {
        let s = Sandbox::new();
        let config = s.project.join(".aider.conf.yml");
        write(&config, original);
        s.ok(&["install", "--platform", "aider"]);
        let after = fs::read(&config).unwrap();
        let value: serde_yaml_ng::Value = serde_yaml_ng::from_slice(&after).unwrap();
        let paths = value["read"].as_sequence().unwrap();
        assert!(paths.iter().any(|v| v.as_str() == Some(".aider/graf.md")));
        if original.contains("CONVENTIONS.md") {
            assert_eq!(paths[0].as_str(), Some("CONVENTIONS.md"));
        }
        if original.contains("# keep") {
            assert!(after.starts_with(original.as_bytes()));
        }
        assert!(!s.project.join("AGENTS.md").exists());
        assert_eq!(
            s.ok(&["install", "--platform", "aider"])["status"],
            "unchanged"
        );
        s.ok(&["uninstall", "--platform", "aider"]);
        assert_eq!(fs::read_to_string(config).unwrap(), original);
        assert!(!s.project.join(".aider/graf.md").exists());
    }
    let s = Sandbox::new();
    s.ok(&["install", "--platform", "aider", "--global"]);
    let value: serde_yaml_ng::Value =
        serde_yaml_ng::from_slice(&fs::read(s.home.join(".aider.conf.yml")).unwrap()).unwrap();
    assert_eq!(
        value["read"][0].as_str(),
        s.home.join(".aider/graf.md").to_str()
    );
    s.ok(&["uninstall", "--platform", "aider", "--global"]);
}

#[test]
fn hook_orphan_backups_and_parent_paths_are_not_adopted() {
    let s = Sandbox::new();
    s.init();
    let backup = s.project.join(".git/hooks/post-commit.graf-original");
    write(&backup, "unowned");
    fails(s.cli(&["hook", "install"]), "unowned hook backup");
    assert!(!s.project.join(".git/hooks/post-commit").exists());
    assert_eq!(fs::read_to_string(&backup).unwrap(), "unowned");
    let outside = s.project.join("../outside-hooks");
    fs::create_dir_all(&outside).unwrap();
    assert!(
        s.git(&["config", "core.hooksPath", outside.to_str().unwrap()])
            .status
            .success()
    );
    fails(s.cli(&["hook", "install"]), "outside this repository");
}

#[cfg(unix)]
#[test]
fn real_hook_refresh_handles_executable_metacharacters_and_preserves_staging() {
    use std::os::unix::fs::PermissionsExt;
    let s = Sandbox::new();
    s.init();
    write(
        &s.project.join("sample.py"),
        "def old_symbol():\n    pass\n",
    );
    s.ok(&["index", "."]);
    let exe = s.home.join("graf tool ' $(touch INJECTED)");
    fs::copy(env!("CARGO_BIN_EXE_graf"), &exe).unwrap();
    fs::set_permissions(&exe, fs::Permissions::from_mode(0o700)).unwrap();
    let mut command = s.command(&exe);
    success(
        command
            .args(["--json", "hook", "install"])
            .output()
            .unwrap(),
    );
    write(
        &s.project.join("sample.py"),
        "def new_symbol():\n    pass\n",
    );
    let hook = s.project.join(".git/hooks/post-commit");
    let output = s.command(&hook).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result = s.ok(&["show", "new_symbol"]);
    assert!(result.to_string().contains("new_symbol"));
    assert!(!s.project.join("INJECTED").exists());
    let staged = s.git(&["diff", "--cached", "--name-only"]);
    assert!(staged.stdout.is_empty());
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(s.ok(&["hook", "status"])["status"], "modified");
    fails(s.cli(&["hook", "uninstall"]), "permissions changed");
}

#[test]
fn tool_hooks_are_explicit_reversible_and_preserve_existing_settings() {
    for (host, event, matcher) in [
        ("claude", "PreToolUse", "Read|Glob|Grep|Bash"),
        ("codebuddy", "PreToolUse", "Read|Glob|Grep|Bash"),
        ("gemini", "BeforeTool", "read_file|list_directory"),
    ] {
        let s = Sandbox::new();
        let path = s.project.join(format!(".{host}/settings.json"));
        let original = format!(
            "{{\n  \"theme\": \"dark\",\n  \"hooks\": {{\"{event}\": [{{\"matcher\":\"Other\",\"hooks\":[{{\"type\":\"command\",\"command\":\"echo existing\"}}]}}]}}\n}}\n"
        );
        write(&path, &original);
        let flags = ["install", "--platform", host, "--tool-hooks"];
        assert_eq!(s.ok(&flags)["status"], "installed");
        let bytes = fs::read(&path).unwrap();
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["theme"], "dark");
        let entries = value["hooks"][event].as_array().unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0]["matcher"], matcher);
        assert_eq!(entries[1]["hooks"][0]["command"], "echo existing");
        let command = entries[0]["hooks"][0]["command"].as_str().unwrap();
        assert!(command.starts_with(&format!("graf hook-guard --platform {host} --project ")));
        assert!(entries[0]["hooks"][0]["timeout"].as_u64().unwrap() > 0);
        assert!(!command.contains("--strict"));
        assert_eq!(s.ok(&flags)["status"], "unchanged");
        assert_eq!(fs::read(&path).unwrap(), bytes);
        assert!(
            !s.project
                .join(format!(".{host}/skills/graf/SKILL.md"))
                .exists()
        );
        s.ok(&["uninstall", "--platform", host, "--tool-hooks"]);
        assert_eq!(fs::read(&path).unwrap(), original.as_bytes());
        s.ok(&flags);
        let later = format!("{} ", String::from_utf8(fs::read(&path).unwrap()).unwrap());
        write(&path, &later);
        fails(
            s.cli(&["uninstall", "--platform", host, "--tool-hooks"]),
            "changed after installation",
        );
        assert_eq!(fs::read(&path).unwrap(), later.as_bytes());
    }
}

#[test]
fn tool_hooks_reject_unsupported_scopes_and_malformed_settings_without_overwrite() {
    let s = Sandbox::new();
    fails(
        s.cli(&[
            "install",
            "--platform",
            "claude",
            "--tool-hooks",
            "--global",
        ]),
        "project installations",
    );
    fails(
        s.cli(&["install", "--platform", "codex", "--tool-hooks"]),
        "project installations",
    );
    assert!(!s.project.join(".graf/setup").exists());
    for invalid in [
        r#"{"hooks":{"PreToolUse":{}}}"#,
        r#"{"hooks":{},"hooks":{}}"#,
        r#"{"hooks":{"PreToolUse":[{"hooks":[{"command":"graf hook-guard --platform claude"}]}]}}"#,
    ] {
        let path = s.project.join(".claude/settings.json");
        write(&path, invalid);
        assert!(
            !s.cli(&["install", "--platform", "claude", "--tool-hooks"])
                .status
                .success()
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), invalid);
        assert!(
            !s.project
                .join(".graf/setup/claude-tool-hooks.json")
                .exists()
        );
    }
}

#[test]
fn gemini_mcp_and_tool_hooks_share_one_receipt_and_refuse_overlapping_ownership() {
    let s = Sandbox::new();
    let path = s.project.join(".gemini/settings.json");
    let original=b"{\n// user comment\n\"theme\":\"dark\",\"mcpServers\":{\"other\":{\"command\":\"other\"}}\n}\n";
    write(&path, original);
    s.ok(&["install", "--platform", "gemini", "--mcp", "--tool-hooks"]);
    let bytes = fs::read(&path).unwrap();
    let text = String::from_utf8(bytes.clone()).unwrap();
    assert!(text.contains("// user comment"));
    assert!(text.contains("hook-guard --platform gemini"));
    assert!(text.contains("\"graf\": {\"args\":"));
    assert!(
        s.project
            .join(".graf/setup/gemini-mcp-tool-hooks.json")
            .exists()
    );
    assert!(!s.project.join(".graf/setup/gemini-mcp.json").exists());
    fails(
        s.cli(&["uninstall", "--platform", "gemini", "--mcp"]),
        "already owned",
    );
    assert_eq!(fs::read(&path).unwrap(), bytes);
    s.ok(&["uninstall", "--platform", "gemini", "--mcp", "--tool-hooks"]);
    assert_eq!(fs::read(&path).unwrap(), original);
    s.ok(&["install", "--platform", "gemini", "--mcp"]);
    let before = fs::read(&path).unwrap();
    fails(
        s.cli(&["install", "--platform", "gemini", "--tool-hooks"]),
        "already owned",
    );
    assert_eq!(fs::read(&path).unwrap(), before);
    s.ok(&["uninstall", "--platform", "gemini", "--mcp"]);
    assert_eq!(fs::read(&path).unwrap(), original);
}

#[test]
fn claude_global_uninstall_preserves_first_run_metadata_and_added_servers() {
    for custom in [false, true] {
        let s = Sandbox::new();
        let selected = s.home.join("custom claude");
        fs::create_dir_all(&selected).unwrap();
        let scope = if custom { &selected } else { &s.home };
        let path = scope.join(".claude.json");
        let original =
            b"{\r\n \"mcpServers\": {\"alpha\":{\"command\":\"alpha\"}}, \"theme\":\"dark\"\r\n}  ";
        write(&path, original);
        let mut flags = vec!["--platform", "claude", "--global", "--mcp"];
        if custom {
            flags.extend(["--config-root", selected.to_str().unwrap()]);
        }
        let mut install = vec!["install"];
        install.extend(&flags);
        let mut uninstall = vec!["uninstall"];
        uninstall.extend(&flags);
        s.ok(&install);
        s.ok(&uninstall);
        assert_eq!(fs::read(&path).unwrap(), original);
        s.ok(&install);
        let installed: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        let graf = &installed["mcpServers"]["graf"];
        let current = format!(
            "{{\r\n  \"firstStartTime\": \"synthetic-time\",\r\n  \"theme\":\"dark\",\r\n  \"mcpServers\": {{\"alpha\":{{\"command\":\"alpha\"}}, \"graf\":{graf},  \"zulu\": {{\"command\":\"new-user-server\"}}}},\r\n  \"migrationVersion\": 4\r\n}}  "
        );
        write(&path, &current);
        fails(s.cli(&install), "changed after installation");
        assert_eq!(fs::read(&path).unwrap(), current.as_bytes());
        s.ok(&uninstall);
        let expected = current.replace(&format!("\"graf\":{graf},"), "");
        assert_eq!(fs::read_to_string(&path).unwrap(), expected);
        let value: Value = serde_json::from_str(&expected).unwrap();
        assert_eq!(value["firstStartTime"], "synthetic-time");
        assert_eq!(value["mcpServers"]["zulu"]["command"], "new-user-server");
        assert!(!scope.join(".graf/setup/claude-mcp.json").exists());
        assert_eq!(s.ok(&uninstall)["status"], "unchanged");
    }
}

#[test]
fn claude_global_cleanup_refuses_modified_owned_entry_and_ambiguous_json() {
    for mutation in [
        "owned",
        "duplicate",
        "nested-duplicate",
        "malformed",
        "map-type",
    ] {
        let s = Sandbox::new();
        let path = s.home.join(".claude.json");
        write(&path, b"{}");
        s.ok(&["install", "--platform", "claude", "--global", "--mcp"]);
        let installed: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        let graf = &installed["mcpServers"]["graf"];
        let current=match mutation {
            "owned" => r#"{"mcpServers":{"graf":{"command":"sensitive-fixture-must-not-be-printed","args":[]}},"firstStartTime":"time"}"#.to_owned(),
            "duplicate" => format!("{{\"mcpServers\":{{\"graf\":{graf},\"graf\":{graf}}}}}"),
            "nested-duplicate" => format!("{{\"mcpServers\":{{\"graf\":{graf}}},\"host\":{{\"userID\":1,\"userID\":2}}}}"),
            "malformed" => "{not JSON}".into(),
            _ => r#"{"mcpServers":[],"firstStartTime":"time"}"#.into(),
        };
        write(&path, &current);
        let receipt = s.home.join(".graf/setup/claude-mcp.json");
        let receipt_before = fs::read(&receipt).unwrap();
        let output = s.cli(&["uninstall", "--platform", "claude", "--global", "--mcp"]);
        assert!(!output.status.success(), "{mutation}");
        assert!(
            !String::from_utf8_lossy(&output.stderr)
                .contains("sensitive-fixture-must-not-be-printed")
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), current);
        assert_eq!(fs::read(&receipt).unwrap(), receipt_before);
    }
}

#[test]
fn claude_global_cleanup_handles_created_map_and_already_absent_entry() {
    for originally_present in [false, true] {
        for already_absent in [false, true] {
            let s = Sandbox::new();
            let path = s.home.join(".claude.json");
            write(
                &path,
                if originally_present {
                    b"{\"mcpServers\":{}}".as_slice()
                } else {
                    b"{}".as_slice()
                },
            );
            s.ok(&["install", "--platform", "claude", "--global", "--mcp"]);
            let installed: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            let graf = &installed["mcpServers"]["graf"];
            let current = if already_absent {
                "{\"firstStartVersion\":\"fixture\",\"mcpServers\":{}}".to_owned()
            } else {
                format!("{{\"firstStartVersion\":\"fixture\",\"mcpServers\":{{\"graf\":{graf}}}}}")
            };
            write(&path, &current);
            s.ok(&["uninstall", "--platform", "claude", "--global", "--mcp"]);
            let value: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            assert_eq!(value["firstStartVersion"], "fixture");
            assert!(value["mcpServers"].get("graf").is_none());
            assert_eq!(
                value.get("mcpServers").is_some(),
                originally_present || already_absent
            );
            if already_absent {
                assert_eq!(fs::read_to_string(&path).unwrap(), current);
            }
        }
    }
}

#[test]
fn claude_global_mcp_cleanup_does_not_bypass_guidance_edit_guards_or_project_receipts() {
    let s = Sandbox::new();
    let path = s.home.join(".claude.json");
    write(&path, b"{}");
    s.ok(&[
        "install",
        "--platform",
        "claude",
        "--global",
        "--mcp",
        "--skill",
    ]);
    let mut value: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    value["firstStartTime"] = Value::String("fixture".into());
    let current = serde_json::to_vec(&value).unwrap();
    write(&path, &current);
    write(&s.home.join(".claude/CLAUDE.md"), b"user guidance edit");
    fails(
        s.cli(&[
            "uninstall",
            "--platform",
            "claude",
            "--global",
            "--mcp",
            "--skill",
        ]),
        "changed after installation",
    );
    assert_eq!(fs::read(&path).unwrap(), current);
    let project = s.project.join(".mcp.json");
    s.ok(&["install", "--platform", "claude", "--mcp"]);
    let mut bytes = fs::read(&project).unwrap();
    bytes.push(b' ');
    write(&project, &bytes);
    fails(
        s.cli(&["uninstall", "--platform", "claude", "--mcp"]),
        "changed after installation",
    );
    assert_eq!(fs::read(&project).unwrap(), bytes);
}

#[cfg(windows)]
#[test]
fn windows_tool_hooks_refuse_unsupported_shell_path_without_changing_settings() {
    for host in ["claude", "codebuddy", "gemini"] {
        let mut s = Sandbox::new();
        s.project = s._temp.path().join("project with $ shell syntax");
        fs::create_dir_all(&s.project).unwrap();
        let path = s.project.join(format!(".{host}/settings.json"));
        let original = b"{\r\n \"theme\": \"dark\", \"hooks\": {}\r\n}  ";
        write(&path, original);
        fails(
            s.cli(&["install", "--platform", host, "--tool-hooks"]),
            "project path cannot be quoted safely for this host shell",
        );
        assert_eq!(fs::read(&path).unwrap(), original);
        assert!(
            !s.project
                .join(format!(".graf/setup/{host}-tool-hooks.json"))
                .exists()
        );
    }
}
