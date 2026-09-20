use graf::{
    index::IndexOptions,
    ingest::{self, SemanticOptions},
};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};
use tempfile::{TempDir, tempdir};

// Exercise configuration without extracting documents or contacting a provider.
#[allow(dead_code)]
#[path = "../src/extraction.rs"]
mod extraction;

struct Fixture {
    dir: TempDir,
    home: PathBuf,
    bin: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempdir().unwrap();
        let home = dir.path().join("home");
        let bin = dir.path().join("bin");
        fs::create_dir(&home).unwrap();
        fs::create_dir(&bin).unwrap();
        Self { dir, home, bin }
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_graf"));
        cmd.current_dir(self.dir.path())
            .env_clear()
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .env("PATH", &self.bin)
            .arg("--json")
            .args(args);
        // Windows needs its system directory even with isolated provider state.
        if cfg!(windows)
            && let Some(root) = std::env::var_os("SystemRoot")
        {
            cmd.env("SystemRoot", root);
        }
        cmd
    }

    fn run(&self, args: &[&str]) -> Value {
        success(self.command(args).output().unwrap())
    }

    fn registry(&self) -> PathBuf {
        self.home.join(".graf/providers.json")
    }

    fn executable(&self, name: &str) -> PathBuf {
        let path = self.bin.join(name);
        // Any accidental execution leaves a marker in the command's cwd.
        fs::write(
            &path,
            if cfg!(windows) {
                "@echo off\necho invoked>INVOKED\nexit /b 97\n"
            } else {
                "#!/bin/sh\necho invoked > INVOKED\nexit 97\n"
            },
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        path
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

fn failure(output: Output) -> String {
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    String::from_utf8(output.stderr).unwrap()
}

fn detected<'a>(report: &'a Value, name: &str) -> &'a Value {
    report["providers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["preset"] == name)
        .unwrap()
}

#[test]
fn detect_reports_presence_only_without_running_commands_or_reading_registry() {
    let f = Fixture::new();
    let claude = f.executable(if cfg!(windows) {
        "claude.cmd"
    } else {
        "claude"
    });
    fs::create_dir_all(f.registry().parent().unwrap()).unwrap();
    fs::write(f.registry(), "invalid registry").unwrap();
    let report = success(
        f.command(&["provider", "detect"])
            .env("OPENAI_API_KEY", "synthetic-secret-never-print")
            .env("ANTHROPIC_API_KEY", "")
            .env("GOOGLE_API_KEY", "synthetic-alternate-key")
            .output()
            .unwrap(),
    );
    assert_eq!(
        detected(&report, "open_ai")["environment"]["OPENAI_API_KEY"],
        true
    );
    assert_eq!(
        detected(&report, "anthropic")["environment"]["ANTHROPIC_API_KEY"],
        true
    );
    assert_eq!(
        detected(&report, "gemini")["environment"]["GOOGLE_API_KEY"],
        true
    );
    assert_eq!(
        detected(&report, "gemini")["environment"]["GEMINI_API_KEY"],
        false
    );
    assert_eq!(
        detected(&report, "claude_cli")["command"]["path"],
        json!(claude)
    );
    assert!(detected(&report, "bedrock")["command"]["path"].is_null());
    assert!(!report.to_string().contains("synthetic-"));
    assert!(!f.dir.path().join("INVOKED").exists());
    assert_eq!(
        fs::read_to_string(f.registry()).unwrap(),
        "invalid registry"
    );

    let absent = success(
        f.command(&["provider", "detect"])
            .env_remove("HOME")
            .env_remove("USERPROFILE")
            .env_remove("PATH")
            .output()
            .unwrap(),
    );
    assert_eq!(
        detected(&absent, "open_ai")["environment"]["OPENAI_API_KEY"],
        false
    );
    assert!(detected(&absent, "claude_cli")["command"]["path"].is_null());
}

#[cfg(unix)]
#[test]
fn detect_skips_non_executable_files_and_directories() {
    let f = Fixture::new();
    fs::write(f.bin.join("claude"), "not executable").unwrap();
    fs::create_dir(f.bin.join("aws")).unwrap();
    let report = f.run(&["provider", "detect"]);
    assert!(detected(&report, "claude_cli")["command"]["path"].is_null());
    assert!(detected(&report, "bedrock")["command"]["path"].is_null());
    assert!(!f.registry().exists());
}

#[test]
fn templates_are_valid_portable_configs_with_explicit_models_and_native_routes() {
    let f = Fixture::new();
    for (name, endpoint, key) in [
        (
            "open_ai",
            "https://api.openai.com/v1/chat/completions",
            Some("OPENAI_API_KEY"),
        ),
        (
            "anthropic",
            "https://api.anthropic.com/v1/messages",
            Some("ANTHROPIC_API_KEY"),
        ),
        (
            "gemini",
            "https://generativelanguage.googleapis.com/v1beta/models/{model}:generateContent",
            Some("GEMINI_API_KEY"),
        ),
        ("ollama", "http://localhost:11434/api/chat", None),
        ("bedrock", "", None),
        ("claude_cli", "", None),
    ] {
        let output = f.run(&["provider", "template", name, "--model", "explicit-model"]);
        assert_eq!(output["endpoint"], endpoint);
        assert_eq!(output["key_env"], json!(key));
        assert_eq!(output["model"], "explicit-model");
        let settings: SemanticOptions = serde_json::from_value(output).unwrap();
        ingest::config_fingerprint(&ingest::IngestOptions {
            semantic: Some(settings),
            ..Default::default()
        })
        .unwrap();
    }
    let openai = success(
        f.command(&["provider", "template", "openai"])
            .env_remove("HOME")
            .env_remove("USERPROFILE")
            .output()
            .unwrap(),
    );
    assert_eq!(openai["model"], "gpt-6-astra");
    assert!(!f.registry().exists());
    let alternate = f.run(&[
        "provider",
        "template",
        "gemini",
        "--model",
        "gemini-example",
        "--key-env",
        "GOOGLE_API_KEY",
    ]);
    assert_eq!(alternate["key_env"], "GOOGLE_API_KEY");
    let azure = f.run(&["provider", "template", "azure", "--model", "deployment", "--endpoint", "https://example.openai.azure.com/openai/deployments/deployment/chat/completions?api-version=2024-10-21", "--key-env", "AZURE_OPENAI_API_KEY"]);
    assert_eq!(azure["model"], "deployment");
    assert_eq!(azure["key_env"], "AZURE_OPENAI_API_KEY");
}

#[test]
fn invalid_presets_do_not_create_a_registry_or_leak_credentials() {
    let f = Fixture::new();
    for args in [
        vec!["provider", "template", "anthropic"],
        vec![
            "provider",
            "template",
            "anthropic",
            "--endpoint",
            "http://localhost:1234/v1/messages",
        ],
        vec!["provider", "template", "open_ai", "--model", ""],
        vec!["provider", "template", "azure", "--model", "deployment"],
        vec!["provider", "template", "cli", "--model", "explicit-model"],
        vec!["provider", "template", "unknown"],
        vec![
            "provider",
            "setup",
            "bad",
            "open_ai",
            "--key-env",
            "sk-synthetic-secret",
        ],
        vec![
            "provider",
            "setup",
            "bad",
            "open_ai",
            "--model",
            "local",
            "--endpoint",
            "http://user:synthetic-secret@localhost/v1/chat/completions",
        ],
        vec![
            "provider",
            "setup",
            "bad",
            "open_ai",
            "--model",
            "local",
            "--endpoint",
            "http://localhost/v1/chat/completions?key=synthetic-secret",
        ],
        vec![
            "provider",
            "setup",
            "bad",
            "claude_cli",
            "--model",
            "sonnet",
            "--key-env",
            "ANTHROPIC_API_KEY",
        ],
    ] {
        let error = failure(f.command(&args).output().unwrap());
        assert!(!error.contains("synthetic-secret"));
        assert!(!f.registry().exists());
    }
}

#[test]
fn setup_works_with_existing_registry_commands_and_refuses_overwrites() {
    let f = Fixture::new();
    f.run(&["provider", "setup", "work", "openai"]);
    let saved = fs::read(f.registry()).unwrap();
    assert_eq!(f.run(&["provider", "show", "work"])["model"], "gpt-6-astra");
    assert_eq!(f.run(&["provider", "list"])["custom"], json!(["work"]));
    assert!(
        failure(
            f.command(&["provider", "setup", "work", "ollama", "--model", "other"])
                .output()
                .unwrap()
        )
        .contains("already exists")
    );
    assert_eq!(fs::read(f.registry()).unwrap(), saved);
    for name in ["open_ai", "openai", "bad/name", ""] {
        failure(
            f.command(&["provider", "setup", name, "open_ai"])
                .output()
                .unwrap(),
        );
        assert_eq!(fs::read(f.registry()).unwrap(), saved);
    }
    f.run(&["provider", "remove", "work"]);
    assert_eq!(f.run(&["provider", "list"])["custom"], json!([]));
}

#[test]
fn setup_pins_installed_cli_recipes_without_invoking_them() {
    let f = Fixture::new();
    for (preset, program) in [
        (
            "claude_cli",
            if cfg!(windows) {
                "claude.cmd"
            } else {
                "claude"
            },
        ),
        ("bedrock", if cfg!(windows) { "aws.exe" } else { "aws" }),
    ] {
        let args = [
            "provider",
            "setup",
            "native",
            preset,
            "--model",
            "explicit-model",
        ];
        assert!(failure(f.command(&args).output().unwrap()).contains("not found on PATH"));
        let executable = f.executable(program);
        let template = f.run(&["provider", "template", preset, "--model", "explicit-model"]);
        f.run(&args);
        let saved = f.run(&["provider", "show", "native"]);
        assert_eq!(saved["command"]["program"], json!(executable));
        assert_eq!(saved["command"]["args"], template["command"]["args"]);
        assert!(!f.dir.path().join("INVOKED").exists());
        f.run(&["provider", "remove", "native"]);
    }
}

#[test]
fn custom_endpoints_require_explicit_key_names_and_keep_local_http() {
    let f = Fixture::new();
    let endpoint = "http://127.0.0.1:1234/v1/chat/completions";
    let local = success(
        f.command(&["provider", "template", "open_ai", "--endpoint", endpoint])
            .env("OPENAI_API_KEY", "synthetic-key-never-send")
            .output()
            .unwrap(),
    );
    assert_eq!(local["endpoint"], endpoint);
    assert_eq!(local["model"], "gpt-6-astra");
    assert!(local["key_env"].is_null());
    assert!(!local.to_string().contains("synthetic-key"));
    let explicit = f.run(&[
        "provider",
        "template",
        "open_ai",
        "--model",
        "local-model",
        "--endpoint",
        endpoint,
        "--key-env",
        "LOCAL_API_KEY",
    ]);
    assert_eq!(explicit["key_env"], "LOCAL_API_KEY");
    assert_eq!(explicit["model"], "local-model");
    let selected = configure(
        extraction::ExtractionArgs {
            provider: Some("openai".into()),
            endpoint: Some(endpoint.into()),
            ..Default::default()
        },
        Default::default(),
        f.dir.path(),
    )
    .ingest
    .semantic
    .unwrap();
    assert_eq!(selected.model, "gpt-6-astra");
    assert_eq!(selected.endpoint, endpoint);
    assert!(selected.key_env.is_none());
}

fn configure(args: extraction::ExtractionArgs, options: IndexOptions, root: &Path) -> IndexOptions {
    args.configure(options, root, &root.join("index.db"))
        .unwrap()
}

#[test]
fn endpoint_changes_clear_saved_keys_but_preserve_same_endpoint_and_explicit_keys() {
    let f = Fixture::new();
    let original = "http://127.0.0.1:1234/v1/chat/completions";
    let changed = "http://127.0.0.1:5678/v1/chat/completions";
    let mut saved = IndexOptions::default();
    saved.ingest.semantic = Some(SemanticOptions {
        endpoint: original.into(),
        key_env: Some("ORIGINAL_API_KEY".into()),
        ..Default::default()
    });
    let config = f.dir.path().join("saved.json");
    let bytes = serde_json::to_vec(&saved).unwrap();
    fs::write(&config, &bytes).unwrap();
    for from_file in [false, true] {
        for (endpoint, key_env, expected) in [
            (None, None, Some("ORIGINAL_API_KEY")),
            (Some(original), None, Some("ORIGINAL_API_KEY")),
            (Some(changed), None, None),
            (Some(changed), Some("LOCAL_API_KEY"), Some("LOCAL_API_KEY")),
            (Some(original), Some("LOCAL_API_KEY"), Some("LOCAL_API_KEY")),
            (
                Some(changed),
                Some("ORIGINAL_API_KEY"),
                Some("ORIGINAL_API_KEY"),
            ),
        ] {
            let options = configure(
                extraction::ExtractionArgs {
                    config: from_file.then(|| config.clone()),
                    endpoint: endpoint.map(str::to_owned),
                    key_env: key_env.map(str::to_owned),
                    ..Default::default()
                },
                if from_file {
                    IndexOptions::default()
                } else {
                    saved.clone()
                },
                f.dir.path(),
            );
            let settings = options.ingest.semantic.unwrap();
            assert_eq!(settings.endpoint, endpoint.unwrap_or(original));
            assert_eq!(settings.key_env.as_deref(), expected);
        }
    }
    assert_eq!(fs::read(config).unwrap(), bytes);
    assert_eq!(
        saved.ingest.semantic.unwrap().key_env.as_deref(),
        Some("ORIGINAL_API_KEY")
    );
}

#[test]
fn cli_endpoint_overrides_send_only_authorized_keys_to_a_local_server() {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::time::{Duration, Instant};

    for (selection, explicit_key, expected) in [
        ("named_changed", None, None),
        (
            "named_changed",
            Some("LOCAL_API_KEY"),
            Some("Bearer synthetic-local-key"),
        ),
        ("named_same", None, Some("Bearer synthetic-original-key")),
        ("builtin", None, None),
    ] {
        let f = Fixture::new();
        let project = f.dir.path().join("project");
        fs::create_dir(&project).unwrap();
        fs::write(project.join("notes.md"), "Queue provides durability\n").unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!(
            "http://{}/v1/chat/completions",
            listener.local_addr().unwrap()
        );
        let server = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut socket = loop {
                match listener.accept() {
                    Ok((socket, _)) => break socket,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < deadline, "no provider request received");
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("accept: {error}"),
                }
            };
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            socket
                .set_write_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut reader = BufReader::new(&mut socket);
            let mut authorization = None;
            let mut length = 0;
            loop {
                let mut line = String::new();
                assert!(reader.read_line(&mut line).unwrap() > 0);
                if line == "\r\n" {
                    break;
                }
                if let Some((name, value)) = line.split_once(':') {
                    if name.eq_ignore_ascii_case("authorization") {
                        authorization = Some(value.trim().to_owned());
                    } else if name.eq_ignore_ascii_case("content-length") {
                        length = value.trim().parse::<usize>().unwrap();
                    }
                }
            }
            assert!(length <= 64 * 1024);
            reader.read_exact(&mut vec![0; length]).unwrap();
            drop(reader);
            let graph = json!({"nodes":[{"id":"q","label":"Queue","kind":"concept","evidence":"Queue"}],"edges":[]});
            let body = json!({"choices":[{"finish_reason":"stop","message":{"content":graph.to_string()}}]}).to_string();
            write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            authorization
        });
        if selection != "builtin" {
            let mut args = vec!["provider", "setup", "work", "openai"];
            if selection == "named_same" {
                args.extend(["--endpoint", &endpoint, "--key-env", "OPENAI_API_KEY"]);
            }
            f.run(&args);
        }
        let before = fs::read(f.registry()).ok();
        let mut args = vec![
            "index",
            project.to_str().unwrap(),
            "--provider",
            if selection == "builtin" {
                "openai"
            } else {
                "work"
            },
            "--endpoint",
            &endpoint,
        ];
        if let Some(key) = explicit_key {
            args.extend(["--key-env", key]);
        }
        let output = f
            .command(&args)
            .env("OPENAI_API_KEY", "synthetic-original-key")
            .env("LOCAL_API_KEY", "synthetic-local-key")
            .output()
            .unwrap();
        let authorization = server.join().unwrap();
        success(output);
        assert_eq!(authorization.as_deref(), expected, "selection: {selection}");
        assert_eq!(fs::read(f.registry()).ok(), before);
    }
}

#[test]
fn builtin_selection_is_explicit_and_saved_advanced_options_remain_authoritative() {
    let f = Fixture::new();
    let root = f.dir.path();
    assert!(
        configure(Default::default(), Default::default(), root)
            .ingest
            .semantic
            .is_none()
    );
    let selected = configure(
        extraction::ExtractionArgs {
            provider: Some("openai".into()),
            ..Default::default()
        },
        Default::default(),
        root,
    );
    assert_eq!(
        selected.ingest.semantic.unwrap().endpoint,
        "https://api.openai.com/v1/chat/completions"
    );

    let settings: SemanticOptions = serde_json::from_value(json!({
        "provider":"open_ai", "endpoint":"http://localhost:1234/custom", "model":"saved-model",
        "key_env":null, "max_calls":7, "temperature":0.4, "extra_body":{"seed":42}
    }))
    .unwrap();
    let mut saved = IndexOptions::default();
    saved.ingest.semantic = Some(settings.clone());
    let preserved = configure(Default::default(), saved.clone(), root);
    assert_eq!(
        serde_json::to_value(preserved.ingest.semantic).unwrap(),
        json!(settings)
    );
    let overridden = configure(
        extraction::ExtractionArgs {
            model: Some("override-model".into()),
            ..Default::default()
        },
        saved.clone(),
        root,
    );
    let mut expected = settings.clone();
    expected.model = "override-model".into();
    assert_eq!(
        serde_json::to_value(overridden.ingest.semantic).unwrap(),
        json!(expected)
    );
    let path = root.join("advanced.json");
    fs::write(&path, serde_json::to_vec(&saved).unwrap()).unwrap();
    let from_file = configure(
        extraction::ExtractionArgs {
            config: Some(path),
            ..Default::default()
        },
        Default::default(),
        root,
    );
    assert_eq!(
        serde_json::to_value(from_file.ingest.semantic).unwrap(),
        json!(settings)
    );
    assert!(
        configure(
            extraction::ExtractionArgs {
                no_semantic: true,
                ..Default::default()
            },
            saved,
            root
        )
        .ingest
        .semantic
        .is_none()
    );
}

#[test]
fn project_setup_and_advanced_json_add_round_trip_without_rewriting_fields() {
    let f = Fixture::new();
    let project = f.dir.path().join("project");
    fs::create_dir(&project).unwrap();
    let project_arg = project.to_str().unwrap();
    f.run(&[
        "provider",
        "--project",
        project_arg,
        "setup",
        "local",
        "ollama",
        "--model",
        "local-model",
    ]);
    assert!(!f.registry().exists());
    assert_eq!(
        f.run(&["provider", "--project", project_arg, "show", "local"])["endpoint"],
        "http://localhost:11434/api/chat"
    );
    let file = f.dir.path().join("adapter.json");
    let advanced = json!({"provider":"cli", "model":"custom-model", "command":{"program":"custom-adapter", "args":["--model", "{model}"], "output_file":false}, "max_calls":9, "extra_body":{"custom":true}});
    fs::write(&file, advanced.to_string()).unwrap();
    f.run(&["provider", "add", "advanced", file.to_str().unwrap()]);
    let expected: SemanticOptions = serde_json::from_value(advanced).unwrap();
    assert_eq!(f.run(&["provider", "show", "advanced"]), json!(expected));
    let before = fs::read(f.registry()).unwrap();
    f.run(&["provider", "template", "open_ai"]);
    assert_eq!(fs::read(f.registry()).unwrap(), before);
}
