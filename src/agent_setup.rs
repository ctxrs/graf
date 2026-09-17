//! Explicit, reversible agent guidance, MCP setup, and foreground Git refresh hooks.
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{Read, Write},
    ops::Range,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail, ensure};
use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::switch_files;

#[derive(Debug, Args)]
pub struct SetupArgs {
    /// Agent host; agents installs the portable Agent Skills format.
    #[arg(long, default_value = "agents")]
    pub platform: String,
    /// Project directory (defaults to the current directory).
    #[arg(long, conflicts_with = "global")]
    pub project: Option<PathBuf>,
    /// Explicitly select the user-global installation.
    #[arg(long)]
    pub global: bool,
    /// Existing Claude, Codex or Hermes configuration root; also select it in the host.
    #[arg(long, requires = "global", conflicts_with = "profile")]
    pub config_root: Option<PathBuf>,
    /// Existing VS Code user-profile directory (locate via MCP: Open User Configuration).
    #[arg(long, requires = "global", conflicts_with = "config_root")]
    pub profile: Option<PathBuf>,
    /// Configure a native stdio MCP server, where supported.
    #[arg(long)]
    pub mcp: bool,
    /// Install graph guidance and CLI usage (the default when no component is selected).
    #[arg(long)]
    pub skill: bool,
}

#[derive(Debug, Args)]
pub struct HookArgs {
    #[command(subcommand)]
    pub command: HookCommand,
}

#[derive(Debug, Subcommand)]
pub enum HookCommand {
    /// Opt in to foreground graph refresh after commits, checkouts, and merges.
    Install {
        #[arg(long)]
        project: Option<PathBuf>,
    },
    /// Restore receipt-owned hooks; refuse later edits.
    Uninstall {
        #[arg(long)]
        project: Option<PathBuf>,
    },
    /// Inspect hook installation without modifying files.
    Status {
        #[arg(long)]
        project: Option<PathBuf>,
    },
}

#[derive(Debug, Serialize)]
pub struct SetupReport {
    pub status: String,
    pub platform: String,
    pub scope: PathBuf,
    pub files: Vec<PathBuf>,
    pub notes: Vec<String>,
}

const LIMIT: u64 = 8 * 1024 * 1024;
const BEGIN: &str = "<!-- graf:begin -->";
const GUIDANCE_VERSION: u32 = 1;
const HOSTS: &[&str] = &[
    "agents",
    "claude",
    "codex",
    "cursor",
    "gemini",
    "opencode",
    "kilo",
    "aider",
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
];
const GUIDANCE: &str = r#"# Graf code graph

Use `graf query "symbol or topic"`, `graf show SYMBOL`, `graf callers SYMBOL`,
`graf callees SYMBOL`, `graf impact SYMBOL`, and `graf path FROM TO` to navigate
the local SQLite snapshot. With MCP enabled, use the Graf tools for the same
reads. Use exact returned IDs for ambiguous names, `--json` for structured
results, and `--db PATH` to select a database. Check `truncated`, diagnostics,
and unresolved references. Read source files whenever useful to verify results.
Queries never rebuild, scan source freshness, fetch URLs, or call providers.

## Create and refresh

Run `graf index .` for static code and supported local documents; `--code-only`
limits discovery to code/configuration. `graf check-update` compares local
fingerprints without models or converters. `graf update` explicitly refreshes.
`graf watch --interval-ms 1000` is an optional foreground update loop, not a
service. Index/update/watch reuse stored extraction settings. Keep `.graf/`
databases, source caches, and setup receipts out of Git. No watcher is required.

## Add sources and select providers

`graf add FILE --project .` or `graf add URL --name page.html --project .`
imports once and saves extracted facts for later updates. Repeat add to fetch
again. Ordinary queries and updates do not refetch these saved sources.
Google pointers require explicit `--google` and configured gws; OCR uses
`--ocr` with installed Tesseract, media uses `--whisper MODEL` with Whisper/FFmpeg
or explicit `--download-media` with yt-dlp. Do not treat missing converters or
failed downloads as successful extraction; inspect errors and retain provenance.

Semantic extraction is opt-in: `graf provider --project . list` lists provider
choices; `graf provider --project . add NAME settings.json` registers settings.
`graf index . --provider NAME` selects one. Built-in HTTP providers need a full
`--endpoint URL`, a suitable `--model MODEL`, and `--key-env VARIABLE` when needed.
Use `--deep` to additionally enrich code, and `--vision` to allow image upload.
`--code-only --deep` can still send code to a model. Choose providers/endpoints
only for explicitly requested enrichment: calls may disclose source text and
incur costs. Bound work with `--max-semantic-files N` and provider call/token
settings; per-file limits are not a whole-corpus spending limit. Keys belong in
environment variables. Never invent an endpoint or silently enable a provider.
`graf index . --no-semantic` disables stored semantic settings. Check failures,
inferred confidence, evidence, and coverage rather than claiming complete facts.

## Analyze, export, and combine

`graf analyze`, `graf communities`, and `graf hubs --sort pagerank` explicitly
load the complete saved graph and can cost more than bounded navigation.
`--resolution` controls granularity; `--max-community-size` and `--min-cohesion`
are soft split targets. Inspect reported unsatisfied constraints.
`graf report --output report.md` and `graf export html --output graph.html`
create reports; `graf export snapshot-json --output graph.json` saves interchange
data. Other export formats include graphml, cypher, mermaid, svg, canvas,
callflow-html, tree-html, wiki, and obsidian. Wiki/Obsidian need an existing
output directory and create a fresh folder. `graf diagnose multigraph` reports
parallel/mixed edges and collapse risks without changing topology.
`graf label --output labels.json` saves deterministic membership-based labels;
`graf report --labels labels.json` applies only labels whose members still match.
`graf report --check-freshness` explicitly scans native source fingerprints;
without it, coverage describes stored inputs and freshness is not checked.
`graf benchmark --query SYMBOL --iterations 20` times local bounded SQL reads;
timings are machine/cache dependent and do not compare other graph tools.

`graf global add NAME PROJECT` and `graf global refresh` explicitly rebuild a
stored aggregate. `graf global list` and `graf global query TEXT` use saved data
without opening registered sources. Missing sources abort rebuilds and retain
the previous aggregate. `graf merge --project NAME=PATH --snapshot NAME=FILE
--output NEW_DB` combines named inputs without collapsing source identities.
Analysis, labels, and exports never refresh the original graph implicitly.

## Guidance updates

Compare the installed guidance version with a newer Graf installation's output.
Rerun `graf install` with the same platform, scope, and component flags to update
receipt-owned guidance. Later edits are refused, not overwritten. Uninstall with
the same selection restores the original pre-install bytes, including after a
guidance upgrade. No source-read restrictions or background services are needed.
"#;

fn guidance() -> String {
    format!(
        "<!-- graf guidance version: {GUIDANCE_VERSION}; executable: {} -->\n{GUIDANCE}",
        env!("CARGO_PKG_VERSION")
    )
}

#[derive(Clone, Copy)]
struct GuidanceStamp {
    revision: u32,
    package: (u64, u64, u64),
}

// Compare numeric release cores, not lexical text. Prerelease/build suffixes
// don't change guidance compatibility; the guidance revision remains explicit.
fn package_core(version: &str) -> Option<(u64, u64, u64)> {
    let mut parts = version.split(['-', '+']).next()?.split('.');
    let core = (
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
    );
    parts.next().is_none().then_some(core)
}

fn guidance_stamp(bytes: &[u8]) -> Option<GuidanceStamp> {
    let line = bytes
        .split(|b| *b == b'\n')
        .filter_map(|line| std::str::from_utf8(line).ok())
        .find_map(|line| {
            line.trim_end_matches('\r')
                .strip_prefix("<!-- graf guidance version: ")
        })?;
    let (revision, package) = line.strip_suffix(" -->")?.split_once("; executable: ")?;
    Some(GuidanceStamp {
        revision: revision.parse().ok()?,
        package: package_core(package)?,
    })
}

fn guidance_direction(stamp: GuidanceStamp) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let current =
        package_core(env!("CARGO_PKG_VERSION")).expect("Cargo package has a semantic version");
    // A newer component always wins: never advise a downgrade when package and
    // guidance revisions point in opposite directions.
    if stamp.package > current || stamp.revision > GUIDANCE_VERSION {
        Ordering::Greater
    } else if stamp.package < current || stamp.revision < GUIDANCE_VERSION {
        Ordering::Less
    } else {
        Ordering::Equal
    }
}

fn read_guidance_stamp(path: &Path) -> Option<GuidanceStamp> {
    check_parents(path).ok()?;
    let metadata = fs::symlink_metadata(path).ok()?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return None;
    }
    let mut options = File::options();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path).ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut prefix = Vec::new();
    file.take(4096).read_to_end(&mut prefix).ok()?;
    guidance_stamp(&prefix)
}

/// Read only bounded headers at known skill paths. The caller supplies an
/// already-selected project and prints notices to stderr, never protocol output.
/// Missing, unreadable, unrecognized and matching guidance is silent.
pub fn guidance_notices(project: Option<&Path>) -> Vec<String> {
    let mut candidates = Vec::new();
    if let Some(project) = project {
        for host in HOSTS {
            if let Ok(path) = skill_path(host, false) {
                candidates.push((*host, "project", project.join(path)));
            }
        }
    }
    if let Ok(home) = home() {
        for host in HOSTS {
            let path = match *host {
                "claude" => std::env::var_os("CLAUDE_CONFIG_DIR")
                    .filter(|v| !v.is_empty())
                    .map(PathBuf::from)
                    .unwrap_or_else(|| home.join(".claude"))
                    .join("skills/graf/SKILL.md"),
                "hermes" => {
                    let local = std::env::var_os("LOCALAPPDATA").filter(|v| !v.is_empty());
                    let base = std::env::var_os("HERMES_HOME")
                        .filter(|v| !v.is_empty())
                        .map(PathBuf::from)
                        .or_else(|| {
                            hermes_root(&home, cfg!(windows), local.as_deref().map(Path::new)).ok()
                        });
                    let Some(base) = base else {
                        continue;
                    };
                    base.join("skills/graf/SKILL.md")
                }
                _ => {
                    let Ok(path) = skill_path(host, true) else {
                        continue;
                    };
                    home.join(path)
                }
            };
            candidates.push((*host, "global", path));
        }
    }
    let mut seen = std::collections::BTreeSet::new();
    let mut notices = Vec::new();
    for (host, scope, path) in candidates {
        if !seen.insert(path.clone()) {
            continue;
        }
        let Some(stamp) = read_guidance_stamp(&path) else {
            continue;
        };
        let (major, minor, patch) = stamp.package;
        let summary = format!(
            "Graf {scope} guidance for {host} was installed by Graf {major}.{minor}.{patch} (guidance revision {}); this executable is {} (revision {GUIDANCE_VERSION}).",
            stamp.revision,
            env!("CARGO_PKG_VERSION")
        );
        match guidance_direction(stamp) {
            std::cmp::Ordering::Less => notices.push(format!("{summary} Installed guidance is older. Rerun `graf install` with the same platform, scope and component flags to update it; later edits will be refused.")),
            std::cmp::Ordering::Greater => notices.push(format!("{summary} Installed guidance is newer. Upgrade the Graf executable before updating guidance to avoid a downgrade.")),
            std::cmp::Ordering::Equal => {}
        }
    }
    notices
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Change {
    path: PathBuf,
    before: Option<Vec<u8>>,
    after: Vec<u8>,
    /// A hook backup retains the original access permissions, including ACLs.
    permission_source: Option<PathBuf>,
    executable: bool,
    /// Previous installed bytes accepted only while a guidance upgrade is pending.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    previous_after: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    version: u32,
    scope: PathBuf,
    changes: Vec<Change>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    guidance_version: Option<u32>,
}

fn root(path: Option<&Path>) -> Result<PathBuf> {
    let path = path.unwrap_or(Path::new(".")).canonicalize()?;
    ensure!(path.is_dir(), "scope must be a directory");
    Ok(path)
}

fn home() -> Result<PathBuf> {
    let value = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .context("cannot determine home directory")?;
    root(Some(Path::new(&value)))
}

fn platform(name: &str) -> Result<&str> {
    let name = match name {
        "skills" => "agents",
        "windows" => "claude",
        "antigravity-windows" => "antigravity",
        other => other,
    };
    ensure!(
        HOSTS.contains(&name),
        "unknown platform {name:?}; use agents for a host supporting Agent Skills"
    );
    Ok(name)
}

// Native skill locations follow the host's discovery contract, not its name.
fn skill_path(host: &str, global: bool) -> Result<&'static str> {
    Ok(match (host, global) {
        ("agents" | "codex" | "amp", false) | ("agents" | "codex", true) => {
            ".agents/skills/graf/SKILL.md"
        }
        ("claude", _) => ".claude/skills/graf/SKILL.md",
        ("cursor", false) => ".cursor/rules/graf.mdc",
        ("cursor", true) => ".cursor/skills/graf/SKILL.md",
        ("gemini", _) => ".gemini/skills/graf/SKILL.md",
        ("opencode", false) => ".opencode/skills/graf/SKILL.md",
        ("opencode", true) => ".config/opencode/skills/graf/SKILL.md",
        ("kilo", _) => ".kilo/skills/graf/SKILL.md",
        ("aider", _) => ".aider/graf.md",
        ("copilot" | "vscode", false) => ".github/skills/graf/SKILL.md",
        ("copilot" | "vscode", true) => ".copilot/skills/graf/SKILL.md",
        ("claw", false) => "skills/graf/SKILL.md",
        ("claw", true) => ".openclaw/skills/graf/SKILL.md",
        ("droid", _) => ".factory/skills/graf/SKILL.md",
        ("trae", _) => ".trae/skills/graf/SKILL.md",
        ("trae-cn", false) => ".trae/skills/graf/SKILL.md",
        ("trae-cn", true) => ".trae-cn/skills/graf/SKILL.md",
        ("hermes", _) => ".hermes/skills/graf/SKILL.md",
        ("kiro", _) => ".kiro/skills/graf/SKILL.md",
        ("pi", false) => ".pi/skills/graf/SKILL.md",
        ("pi", true) => ".pi/agent/skills/graf/SKILL.md",
        ("codebuddy", _) => ".codebuddy/skills/graf/SKILL.md",
        ("antigravity", false) => ".agents/skills/graf/SKILL.md",
        ("antigravity", true) => ".gemini/config/skills/graf/SKILL.md",
        ("kimi", _) => ".kimi/skills/graf/SKILL.md",
        ("amp", true) => ".config/agents/skills/graf/SKILL.md",
        ("devin", false) => ".devin/skills/graf/SKILL.md",
        ("devin", true) => ".config/devin/skills/graf/SKILL.md",
        _ => unreachable!(),
    })
}

fn guidance_path(host: &str, global: bool) -> Option<&'static str> {
    match (host, global) {
        ("aider", _) => Some(".aider.conf.yml"),
        ("claude", false) => Some("CLAUDE.md"),
        ("claude", true) => Some(".claude/CLAUDE.md"),
        ("gemini", false) => Some("GEMINI.md"),
        ("gemini", true) => Some(".gemini/GEMINI.md"),
        ("codex", true) => Some(".codex/AGENTS.md"),
        ("codex" | "agents" | "amp" | "opencode" | "droid", false) => Some("AGENTS.md"),
        _ => None,
    }
}

fn mcp_path(host: &str, global: bool) -> Result<(&'static str, &'static str)> {
    Ok(match (host, global) {
        ("claude", false) => (".mcp.json", "mcpServers"),
        ("claude", true) => (".claude.json", "mcpServers"),
        ("codex", _) => (".codex/config.toml", "mcp_servers"),
        ("cursor", _) => (".cursor/mcp.json", "mcpServers"),
        ("gemini", _) => (".gemini/settings.json", "mcpServers"),
        ("vscode", false) => (".vscode/mcp.json", "servers"),
        ("vscode", true) => ("mcp.json", "servers"),
        _ => bail!(
            "native MCP setup is not verified for {host} in this scope; install --skill and configure `graf serve` in the host manually"
        ),
    })
}

// Hermes' native Windows installer and runtime agree on this default. Keep the
// platform input separate so path selection can be checked without a Windows host.
fn hermes_root(home: &Path, windows: bool, local_appdata: Option<&Path>) -> Result<PathBuf> {
    if windows {
        let base = local_appdata
            .map(Path::to_path_buf)
            .unwrap_or_else(|| home.join("AppData/Local"));
        ensure!(base.is_absolute(), "LOCALAPPDATA must be an absolute path");
        Ok(base.join("hermes"))
    } else {
        Ok(home.join(".hermes"))
    }
}

fn check_parents(path: &Path) -> Result<()> {
    for parent in path.ancestors().skip(1) {
        match fs::symlink_metadata(parent) {
            Ok(m) => ensure!(
                m.is_dir() && !m.file_type().is_symlink(),
                "directory must not be a symlink: {}",
                parent.display()
            ),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

fn read(path: &Path) -> Result<Option<Vec<u8>>> {
    check_parents(path)?;
    match fs::symlink_metadata(path) {
        Ok(m) => ensure!(
            m.is_file() && !m.file_type().is_symlink(),
            "expected regular file: {}",
            path.display()
        ),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    }
    let mut bytes = Vec::new();
    File::open(path)?
        .take(LIMIT * 16 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= LIMIT * 16,
        "file exceeds size limit: {}",
        path.display()
    );
    Ok(Some(bytes))
}

fn atomic(
    path: &Path,
    bytes: &[u8],
    expected: Option<&[u8]>,
    source: Option<&Path>,
    executable: bool,
) -> Result<()> {
    ensure!(
        read(path)?.as_deref() == expected,
        "file changed; refusing to replace {}",
        path.display()
    );
    let parent = path.parent().context("file has no parent")?;
    fs::create_dir_all(parent)?;
    check_parents(path)?;
    let staging = tempfile::tempdir_in(parent)?;
    switch_files::protect(staging.path())?;
    let mut temp = tempfile::NamedTempFile::new_in(staging.path())?;
    switch_files::protect(temp.path())?;
    if let Some(source) = source.or_else(|| expected.map(|_| path)) {
        switch_files::preserve_permissions(temp.as_file(), source)?;
    }
    #[cfg(unix)]
    if executable {
        use std::os::unix::fs::PermissionsExt;
        let mode = temp.as_file().metadata()?.permissions().mode();
        temp.as_file()
            .set_permissions(fs::Permissions::from_mode(mode | 0o100))?;
    }
    #[cfg(not(unix))]
    let _ = executable;
    temp.write_all(bytes)?;
    temp.as_file().sync_all()?;
    ensure!(
        read(path)?.as_deref() == expected,
        "file changed during setup: {}",
        path.display()
    );
    switch_files::replace(temp, path, expected.is_some())
}

fn planned(path: PathBuf, after: Vec<u8>, dedicated: bool) -> Result<Change> {
    let before = read(&path)?;
    ensure!(
        !dedicated || before.is_none(),
        "unowned file already exists: {}",
        path.display()
    );
    ensure!(after.len() as u64 <= LIMIT, "configuration exceeds 8 MiB");
    Ok(Change {
        path,
        before,
        after,
        permission_source: None,
        executable: false,
        previous_after: None,
    })
}

fn edited(path: PathBuf, before: Option<Vec<u8>>, after: Vec<u8>) -> Result<Change> {
    let change = planned(path, after, false)?;
    ensure!(
        change.before == before,
        "file changed while planning installation"
    );
    Ok(change)
}

fn object_fields(bytes: &[u8], host: &str) -> Result<(BTreeMap<String, Range<usize>>, usize)> {
    use jsonc_parser::{CollectOptions, ParseOptions, common::Ranged};
    // VS Code's schema accepts JSONC; Gemini strips comments before JSON.parse.
    // Other hosts retain strict JSON until their contract establishes extensions.
    let parsed = jsonc_parser::parse_to_ast(
        std::str::from_utf8(bytes)?,
        &CollectOptions::default(),
        &ParseOptions {
            allow_comments: matches!(host, "vscode" | "gemini"),
            allow_trailing_commas: host == "vscode",
            allow_loose_object_property_names: false,
            allow_missing_commas: false,
            allow_single_quoted_strings: false,
            allow_hexadecimal_numbers: false,
            allow_unary_plus_numbers: false,
        },
    )
    .context("invalid MCP configuration for this host")?;
    let object = parsed
        .value
        .as_ref()
        .and_then(|v| v.as_object())
        .context("MCP configuration section must be an object")?;
    let mut fields = BTreeMap::new();
    for property in &object.properties {
        let range = property.value.range();
        ensure!(
            fields
                .insert(property.name.as_str().to_owned(), range.start..range.end)
                .is_none(),
            "duplicate JSON key; configuration unchanged"
        );
    }
    // Insert the new first member, before all existing content, avoiding any
    // need to move comments or interpret an existing trailing comma ourselves.
    Ok((fields, object.range.start + 1))
}

fn mcp_bytes(
    path: &Path,
    old: Option<&[u8]>,
    key: &str,
    scope: &Path,
    global: bool,
    host: &str,
) -> Result<Vec<u8>> {
    let exe = std::env::current_exe()?;
    let exe = exe.to_str().context("Graf executable path is not UTF-8")?;
    let db = scope.join(".graf/index.db");
    let args: Vec<&str> = if global {
        vec!["serve"]
    } else {
        vec![
            "--db",
            db.to_str().context("database path is not UTF-8")?,
            "serve",
        ]
    };
    if path.extension().is_some_and(|x| x == "toml") {
        let mut doc: toml_edit::DocumentMut = std::str::from_utf8(old.unwrap_or(b""))?.parse()?;
        if doc.get(key).is_none() {
            doc[key] = toml_edit::Item::Table(toml_edit::Table::new());
        }
        let table = doc[key]
            .as_table_like_mut()
            .context("mcp_servers must be a table")?;
        ensure!(
            !table.contains_key("graf"),
            "MCP server graf already exists and is not owned by this installation"
        );
        let mut server = toml_edit::Table::new();
        server["command"] = toml_edit::value(exe);
        let mut array = toml_edit::Array::new();
        for arg in args {
            array.push(arg);
        }
        server["args"] = toml_edit::value(array);
        table.insert("graf", toml_edit::Item::Table(server));
        return Ok(doc.to_string().into_bytes());
    }
    let entry = if key == "servers" {
        json!({"type":"stdio", "command":exe,"args":args})
    } else {
        json!({"command":exe,"args":args})
    };
    let old = old.unwrap_or(b"{}\n");
    ensure!(old.len() as u64 <= LIMIT, "MCP config exceeds 8 MiB");
    let (fields, root_start) = object_fields(old, host)?;
    let (position, comma, member) = if let Some(range) = fields.get(key) {
        let (servers, start) = object_fields(&old[range.clone()], host)?;
        ensure!(
            !servers.contains_key("graf"),
            "MCP server graf already exists and is not owned by this installation"
        );
        (
            range.start + start,
            !servers.is_empty(),
            format!("\"graf\": {entry}"),
        )
    } else {
        (
            root_start,
            !fields.is_empty(),
            format!("{}: {{\"graf\": {entry}}}", serde_json::to_string(key)?),
        )
    };
    let mut out = old[..position].to_vec();
    out.extend_from_slice(format!("\n  {member}{}\n", if comma { "," } else { "" }).as_bytes());
    out.extend_from_slice(&old[position..]);
    Ok(out)
}

fn receipt_path(scope: &Path, host: &str, component: &str) -> PathBuf {
    scope
        .join(".graf/setup")
        .join(format!("{host}-{component}.json"))
}

fn aider_config(old: Option<&[u8]>, guidance: &Path) -> Result<Vec<u8>> {
    use serde_yaml_ng::Value as Yaml;
    let old = old.unwrap_or(b"");
    ensure!(
        old.len() as u64 <= LIMIT,
        "Aider configuration exceeds 8 MiB"
    );
    let mut document: Yaml =
        serde_yaml_ng::from_slice(old).context("invalid Aider YAML configuration")?;
    if document.is_null() {
        document = Yaml::Mapping(Default::default());
    }
    document.apply_merge()?;
    let map = document
        .as_mapping_mut()
        .context("Aider configuration must be a YAML mapping")?;
    let key = Yaml::String("read".into());
    let previous = map.get(&key).cloned();
    let mut paths = match previous.clone() {
        None | Some(Yaml::Null) => vec![],
        Some(Yaml::String(path)) => vec![Yaml::String(path)],
        Some(Yaml::Sequence(paths)) if paths.iter().all(Yaml::is_string) => paths,
        _ => bail!("Aider read must be a filename or a list of filenames"),
    };
    let path = Yaml::String(
        guidance
            .to_str()
            .context("Aider guidance path is not UTF-8")?
            .into(),
    );
    if !paths.contains(&path) {
        paths.push(path);
    }
    map.insert(key, Yaml::Sequence(paths.clone()));
    // Most existing configs have no read key: appending preserves every byte.
    // Flow mappings, document terminators, and existing read keys use the YAML
    // parser's semantic merge; the receipt still restores the exact original.
    if previous.is_none() {
        let mut appended = old.to_vec();
        appended
            .extend_from_slice(format!("\nread: {}\n", serde_json::to_string(&paths)?).as_bytes());
        if serde_yaml_ng::from_slice::<Yaml>(&appended)
            .ok()
            .is_some_and(|mut value| value.apply_merge().is_ok() && value == document)
        {
            return Ok(appended);
        }
    }
    Ok(serde_yaml_ng::to_string(&document)?.into_bytes())
}

fn load(path: &Path, scope: &Path, allowed: &[PathBuf]) -> Result<Option<Receipt>> {
    let Some(bytes) = read(path)? else {
        return Ok(None);
    };
    let receipt: Receipt = serde_json::from_slice(&bytes).context("invalid setup receipt")?;
    ensure!(
        receipt.version == 1 && receipt.scope == scope && !receipt.changes.is_empty(),
        "setup receipt scope/version mismatch"
    );
    let mut paths = std::collections::BTreeSet::new();
    for change in &receipt.changes {
        ensure!(
            allowed.contains(&change.path) && paths.insert(&change.path),
            "receipt contains unexpected or duplicate destination"
        );
        ensure!(
            change
                .permission_source
                .as_ref()
                .is_none_or(|s| allowed.contains(s)),
            "receipt contains unexpected permissions source"
        );
    }
    Ok(Some(receipt))
}

fn recorded(change: &Change, current: &Option<Vec<u8>>) -> bool {
    current.as_deref() == Some(change.after.as_slice())
        || *current == change.before
        || change
            .previous_after
            .as_ref()
            .is_some_and(|old| current.as_deref() == Some(old.as_slice()))
}

fn verify(receipt: &Receipt) -> Result<()> {
    for change in &receipt.changes {
        let current = read(&change.path)?;
        ensure!(
            recorded(change, &current),
            "file changed after installation; refusing to modify {}",
            change.path.display()
        );
        if current.as_deref() == Some(change.after.as_slice()) {
            ensure!(
                executable_matches(change)?,
                "hook permissions changed after installation: {}",
                change.path.display()
            );
        }
    }
    Ok(())
}

fn executable_matches(change: &Change) -> Result<bool> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if change.executable {
            return Ok(fs::metadata(&change.path)?.permissions().mode() & 0o100 != 0);
        }
    }
    #[cfg(not(unix))]
    let _ = change;
    Ok(true)
}

// A persisted receipt precedes changes. Interrupted operations can be resumed or
// uninstalled; no unrecorded overwrite is needed to recover.
fn apply(path: &Path, receipt: &Receipt) -> Result<bool> {
    verify(receipt)?;
    let mut changed = false;
    let saved = read(path)?;
    let bytes = serde_json::to_vec(receipt)?;
    if saved.as_deref() != Some(bytes.as_slice()) {
        ensure!(
            bytes.len() as u64 <= LIMIT * 16,
            "setup receipt exceeds size limit"
        );
        atomic(path, &bytes, saved.as_deref(), None, false)?;
        changed = true;
    }
    for change in &receipt.changes {
        let current = read(&change.path)?;
        ensure!(
            recorded(change, &current),
            "file changed during setup: {}",
            change.path.display()
        );
        if current.as_deref() != Some(change.after.as_slice()) {
            atomic(
                &change.path,
                &change.after,
                current.as_deref(),
                change.permission_source.as_deref(),
                change.executable,
            )?;
            changed = true;
        }
    }
    if receipt.changes.iter().any(|c| c.previous_after.is_some()) {
        let mut completed = receipt.clone();
        for change in &mut completed.changes {
            change.previous_after = None;
        }
        atomic(
            path,
            &serde_json::to_vec(&completed)?,
            Some(&bytes),
            None,
            false,
        )?;
    }
    Ok(changed)
}

fn undo(path: &Path, receipt: &Receipt) -> Result<()> {
    verify(receipt)?;
    for change in receipt.changes.iter().rev() {
        let current = read(&change.path)?;
        ensure!(
            recorded(change, &current),
            "file changed during uninstall: {}",
            change.path.display()
        );
        if current == change.before {
            continue;
        }
        if let Some(before) = &change.before {
            // Hook originals remain in their sibling backup until the hook has
            // been restored. Use that backup to recover its exact access ACL.
            let backup = change.path.with_file_name(format!(
                "{}.graf-original",
                change.path.file_name().unwrap().to_string_lossy()
            ));
            let source = receipt
                .changes
                .iter()
                .any(|c| c.path == backup)
                .then_some(backup);
            atomic(
                &change.path,
                before,
                current.as_deref(),
                source.as_deref(),
                false,
            )?;
        } else {
            ensure!(
                read(&change.path)? == current,
                "file changed during uninstall"
            );
            fs::remove_file(&change.path)?;
        }
    }
    fs::remove_file(path)?;
    Ok(())
}

struct Lock(PathBuf);
impl Drop for Lock {
    fn drop(&mut self) {
        let _ = fs::remove_dir(&self.0);
    }
}

fn lock(scope: &Path, installing: bool) -> Result<Lock> {
    let directory = scope.join(".graf/setup");
    check_parents(&directory.join("record"))?;
    fs::create_dir_all(&directory)?;
    // Only this dedicated receipt directory is made private, never the project.
    switch_files::protect(&directory)?;
    let path = directory.join("lock");
    fs::create_dir(&path).context("another setup operation is active; if interrupted, remove .graf/setup/lock before retrying")?;
    let lock = Lock(path);
    if installing {
        let ignore = directory.join(".gitignore");
        if read(&ignore)?.is_none() {
            atomic(&ignore, b"*\n", None, None, false)?;
        }
        // Keep databases and receipts out of normal git add, even before indexing.
        let ignore = scope.join(".graf/.gitignore");
        if read(&ignore)?.is_none() {
            atomic(&ignore, b"*\n", None, None, false)?;
        }
    }
    Ok(lock)
}

fn skill_bytes(host: &str, path: &Path) -> Vec<u8> {
    let header = if host == "aider" {
        ""
    } else if host == "cursor" && path.extension().is_some_and(|x| x == "mdc") {
        "---\ndescription: Navigate and maintain the local Graf code graph\nalwaysApply: true\n---\n\n"
    } else {
        "---\nname: graf\ndescription: Navigate stored code and document graphs; explicitly add sources, select providers, analyze, export, and refresh with Graf.\n---\n\n"
    };
    format!("{header}{}", guidance()).into_bytes()
}

fn append_guidance(original: Option<&[u8]>) -> Vec<u8> {
    let mut bytes = original.unwrap_or_default().to_vec();
    bytes.extend_from_slice(format!("\n\n{BEGIN}\n{}<!-- graf:end -->\n", guidance()).as_bytes());
    bytes
}

fn upgrade_guidance(receipt: &mut Receipt, host: &str, primary: &Path) -> Result<()> {
    ensure!(
        receipt.guidance_version.unwrap_or(0) <= GUIDANCE_VERSION,
        "installed guidance is newer than this executable; use the newer Graf or uninstall first"
    );
    let stamp = receipt
        .changes
        .iter()
        .find(|change| change.path == primary)
        .and_then(|change| guidance_stamp(&change.after));
    ensure!(
        stamp.is_none_or(|stamp| guidance_direction(stamp) != std::cmp::Ordering::Greater),
        "installed guidance is newer than this executable; upgrade Graf to avoid a downgrade"
    );
    if receipt.guidance_version == Some(GUIDANCE_VERSION)
        && stamp.is_some_and(|stamp| guidance_direction(stamp) == std::cmp::Ordering::Equal)
    {
        return Ok(());
    }
    for change in &mut receipt.changes {
        let after = if change.path == primary {
            skill_bytes(host, primary)
        } else if host == "aider" {
            continue;
        } else {
            append_guidance(change.before.as_deref())
        };
        if change.after != after {
            // Preserve the original pre-install bytes for eventual uninstall;
            // the receipt also admits the exact current bytes during this upgrade.
            let current = read(&change.path)?;
            ensure!(
                recorded(change, &current),
                "file changed during guidance upgrade: {}",
                change.path.display()
            );
            change.previous_after = current.filter(|bytes| Some(bytes) != change.before.as_ref());
            change.after = after;
        }
    }
    receipt.guidance_version = Some(GUIDANCE_VERSION);
    Ok(())
}

fn setup(args: &SetupArgs, remove: bool) -> Result<SetupReport> {
    ensure!(
        !args.global || args.project.is_none(),
        "--global conflicts with --project"
    );
    let host = platform(&args.platform)?;
    ensure!(
        args.global || (args.config_root.is_none() && args.profile.is_none()),
        "--config-root and --profile require --global"
    );
    ensure!(
        args.config_root.is_none() || args.profile.is_none(),
        "--config-root conflicts with --profile"
    );
    ensure!(
        args.config_root.is_none() || matches!(host, "claude" | "codex" | "hermes"),
        "--config-root is supported only for Claude, Codex and Hermes"
    );
    ensure!(
        args.profile.is_none() || host == "vscode",
        "--profile selects a VS Code user-profile directory only"
    );
    ensure!(
        !(host == "vscode" && args.global && args.mcp && args.profile.is_none()),
        "VS Code global MCP requires --profile PATH; locate its directory with MCP: Open User Configuration"
    );
    let default_scope = if args.global {
        home()?
    } else {
        root(args.project.as_deref())?
    };
    let scope = if let Some(selected) = args.config_root.as_deref().or(args.profile.as_deref()) {
        // An explicit host root/profile is never created or resolved by name.
        let metadata = fs::symlink_metadata(selected)
            .context("selected root/profile must be an existing directory")?;
        ensure!(
            metadata.is_dir() && !metadata.file_type().is_symlink(),
            "selected root/profile must be a real directory, not a symlink"
        );
        root(Some(selected))?
    } else {
        default_scope.clone()
    };
    let mut report = SetupReport {
        status: "unchanged".into(),
        platform: host.into(),
        scope: scope.clone(),
        files: vec![],
        notes: vec![],
    };
    let skill = args.skill || !args.mcp;
    let mut groups = Vec::new();
    if skill {
        let primary = if host == "claude" && args.config_root.is_some() {
            scope.join("skills/graf/SKILL.md")
        } else if host == "hermes" && args.global {
            let base = if args.config_root.is_some() {
                scope.clone()
            } else {
                let local_appdata = std::env::var_os("LOCALAPPDATA").filter(|v| !v.is_empty());
                hermes_root(
                    &default_scope,
                    cfg!(windows),
                    local_appdata.as_deref().map(Path::new),
                )?
            };
            base.join("skills/graf/SKILL.md")
        } else {
            // Codex and VS Code discover personal skills outside their config/profile root.
            default_scope.join(skill_path(host, args.global)?)
        };
        let guidance = if host == "claude" && args.config_root.is_some() {
            Some(scope.join("CLAUDE.md"))
        } else if host == "codex" && args.config_root.is_some() {
            Some(scope.join("AGENTS.md"))
        } else {
            guidance_path(host, args.global).map(|p| default_scope.join(p))
        };
        let mut allowed = vec![primary];
        allowed.extend(guidance);
        groups.push(("skill", allowed));
    }
    if args.mcp {
        let (path, _) = mcp_path(host, args.global)?;
        let path = if host == "claude" && args.config_root.is_some() {
            // Claude retains its legacy config when present, otherwise using
            // .claude.json directly beneath CLAUDE_CONFIG_DIR.
            let legacy = scope.join(".config.json");
            if legacy.try_exists()? {
                legacy
            } else {
                scope.join(".claude.json")
            }
        } else if host == "codex" && args.config_root.is_some() {
            scope.join("config.toml")
        } else {
            scope.join(path)
        };
        groups.push(("mcp", vec![path]));
        if args.global {
            report.notes.push("Global MCP uses graf serve in the host's working directory; the host must launch it in an indexed project.".into());
        }
        if host == "codex" && !args.global {
            report
                .notes
                .push("Codex loads project MCP configuration only for trusted projects.".into());
        }
    }
    if skill && host == "cursor" && args.global {
        report.notes.push("Cursor global guidance is a local user skill in ~/.cursor/skills, loaded on demand. Always-on User Rules remain in Cursor's Customize > Rules UI; Graf does not edit them or sync cloud skills.".into());
    }
    if skill && args.global && matches!(host, "codex" | "vscode") {
        report.notes.push("Personal skills remain in the host's documented home-directory skill location and are shared across configurations/profiles; --config-root/--profile selects configuration and receipt storage only.".into());
    }
    if args.config_root.is_some() || args.profile.is_some() {
        report.notes.push("Use the same --global and root/profile selection for reinstall and uninstall. The selected directory holds the receipts; selecting it here does not change the host's active configuration.".into());
    }
    // No global environment overrides are silently ignored into a different home.
    if args.global && !remove && args.config_root.is_none() {
        let override_var = match host {
            "claude" => Some("CLAUDE_CONFIG_DIR"),
            "codex" => Some("CODEX_HOME"),
            "hermes" => Some("HERMES_HOME"),
            _ => None,
        };
        if let Some(var) = override_var {
            ensure!(
                std::env::var_os(var).is_none(),
                "{var} is set; select its existing directory explicitly with --config-root"
            );
        }
    }
    // Validate all groups before making changes; uninstalls never create state.
    if remove && !scope.join(".graf/setup").try_exists()? {
        return Ok(report);
    }
    let _lock = lock(&scope, !remove)?;
    let mut operations = Vec::new();
    for (component, allowed) in groups {
        let path = receipt_path(&scope, host, component);
        let receipt = if let Some(mut receipt) = load(&path, &scope, &allowed)? {
            verify(&receipt)?;
            if component == "skill" && !remove {
                upgrade_guidance(&mut receipt, host, &allowed[0])?;
            }
            receipt
        } else if remove {
            continue;
        } else {
            let mut changes = Vec::new();
            if component == "mcp" {
                let (_, key) = mcp_path(host, args.global)?;
                let old = read(&allowed[0])?;
                let after = mcp_bytes(&allowed[0], old.as_deref(), key, &scope, args.global, host)?;
                changes.push(edited(allowed[0].clone(), old, after)?);
            } else {
                changes.push(planned(
                    allowed[0].clone(),
                    skill_bytes(host, &allowed[0]),
                    true,
                )?);
                if let Some(path) = allowed.get(1) {
                    if host == "aider" {
                        let original = read(path)?;
                        let guidance = if args.global {
                            allowed[0].clone()
                        } else {
                            PathBuf::from(".aider/graf.md")
                        };
                        let after = aider_config(original.as_deref(), &guidance)?;
                        changes.push(edited(path.clone(), original, after)?);
                    } else {
                        let original = read(path)?;
                        ensure!(
                            !String::from_utf8_lossy(original.as_deref().unwrap_or_default())
                                .contains(BEGIN),
                            "unowned Graf guidance block already exists"
                        );
                        let after = append_guidance(original.as_deref());
                        changes.push(edited(path.clone(), original, after)?);
                    }
                }
            }
            Receipt {
                version: 1,
                scope: scope.clone(),
                changes,
                guidance_version: (component == "skill").then_some(GUIDANCE_VERSION),
            }
        };
        operations.push((path, receipt));
    }
    for (path, receipt) in operations {
        report
            .files
            .extend(receipt.changes.iter().map(|c| c.path.clone()));
        if remove {
            undo(&path, &receipt)?;
            report.status = "uninstalled".into();
        } else if apply(&path, &receipt)? {
            report.status = "installed".into();
        }
    }
    if skill {
        report.notes.push(format!("Guidance version {GUIDANCE_VERSION}; executable {}. Reinstall the same selection after upgrading Graf to refresh owned guidance.", env!("CARGO_PKG_VERSION")));
        report.notes.push("Guidance documents graf on PATH. No source-read restrictions, services, or background watchers were installed.".into());
    }
    Ok(report)
}

pub fn install(args: &SetupArgs) -> Result<SetupReport> {
    setup(args, false)
}
pub fn uninstall(args: &SetupArgs) -> Result<SetupReport> {
    setup(args, true)
}

fn git(project: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(project)
        .args(args)
        .output()?;
    ensure!(
        output.status.success(),
        "Git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(String::from_utf8(output.stdout)?
        .trim_end_matches(['\r', '\n'])
        .to_owned())
}

fn quote(path: &Path) -> Result<String> {
    let value = path
        .to_str()
        .context("hook executable path must be UTF-8")?;
    ensure!(
        !value.contains(['\n', '\r', '\0']),
        "hook executable path contains a line break or NUL"
    );
    Ok(format!("'{}'", value.replace('\'', "'\\''")))
}

const HOOKS: &[&str] = &["post-commit", "post-checkout", "post-merge"];

pub fn hook(args: &HookArgs) -> Result<SetupReport> {
    let (project, action) = match &args.command {
        HookCommand::Install { project } => (project, "install"),
        HookCommand::Uninstall { project } => (project, "uninstall"),
        HookCommand::Status { project } => (project, "status"),
    };
    let selected = root(project.as_deref())?;
    let scope = root(Some(Path::new(&git(
        &selected,
        &["rev-parse", "--show-toplevel"],
    )?)))?;
    let raw_hooks = PathBuf::from(git(
        &scope,
        &["rev-parse", "--path-format=absolute", "--git-path", "hooks"],
    )?);
    let mut hooks = PathBuf::new();
    for part in raw_hooks.components() {
        match part {
            std::path::Component::ParentDir => {
                hooks.pop();
            }
            std::path::Component::CurDir => {}
            part => hooks.push(part.as_os_str()),
        }
    }
    let common = PathBuf::from(git(
        &scope,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?)
    .canonicalize()?;
    check_parents(&hooks.join("post-commit"))?;
    // A project selection must not mutate a user-global hooks directory.
    ensure!(
        hooks.starts_with(&scope) || hooks.starts_with(&common),
        "hooks directory is outside this repository; global/shared core.hooksPath is not supported"
    );
    let allowed: Vec<_> = HOOKS
        .iter()
        .flat_map(|name| {
            [
                hooks.join(name),
                hooks.join(format!("{name}.graf-original")),
            ]
        })
        .collect();
    let path = receipt_path(&scope, "git", "hooks");
    let mut report = SetupReport {
        status: "not-installed".into(),
        platform: "git".into(),
        scope: scope.clone(),
        files: vec![],
        notes: vec![],
    };
    if action != "install" && read(&path)?.is_none() {
        return Ok(report);
    }
    let _lock = if action == "status" {
        None
    } else {
        Some(lock(&scope, action == "install")?)
    };
    let saved = load(&path, &scope, &allowed)?;
    if action == "status" {
        if let Some(receipt) = saved {
            report.files = receipt.changes.iter().map(|c| c.path.clone()).collect();
            report.status = if receipt.changes.iter().all(|c| {
                read(&c.path).ok().flatten().as_deref() == Some(&c.after)
                    && executable_matches(c).unwrap_or(false)
            }) {
                "installed"
            } else {
                "modified"
            }
            .into();
        }
        return Ok(report);
    }
    if action == "uninstall" {
        if let Some(receipt) = saved {
            undo(&path, &receipt)?;
            report.status = "uninstalled".into();
        }
        return Ok(report);
    }
    let receipt = if let Some(receipt) = saved {
        receipt
    } else {
        let exe = quote(&std::env::current_exe()?)?;
        let mut changes = Vec::new();
        for name in HOOKS {
            let path = hooks.join(name);
            let original = read(&path)?;
            ensure!(
                read(&hooks.join(format!("{name}.graf-original")))?.is_none(),
                "unowned hook backup already exists"
            );
            if let Some(original) = &original {
                let mut backup = planned(
                    hooks.join(format!("{name}.graf-original")),
                    original.clone(),
                    true,
                )?;
                backup.permission_source = Some(path.clone());
                changes.push(backup);
            }
            let checkout = if *name == "post-checkout" {
                "[ \"${3:-}\" = 0 ] && exit \"$graf_hook_status\"\n"
            } else {
                ""
            };
            let script = format!(
                "#!/bin/sh\n# graf managed refresh hook\ngraf_hook_status=0\nif [ -x \"$0.graf-original\" ]; then\n  \"$0.graf-original\" \"$@\" || graf_hook_status=$?\nfi\n{checkout}graf_root=$(git rev-parse --show-toplevel) || exit \"$graf_hook_status\"\nif [ -f \"$graf_root/.graf/index.db\" ]; then\n  {exe} --db \"$graf_root/.graf/index.db\" update || printf '%s\\n' 'graf: refresh failed; run graf update manually' >&2\nfi\nexit \"$graf_hook_status\"\n"
            );
            let mut change = edited(path, original, script.into_bytes())?;
            change.executable = true;
            changes.push(change);
        }
        Receipt {
            version: 1,
            scope: scope.clone(),
            changes,
            guidance_version: None,
        }
    };
    report.files = receipt.changes.iter().map(|c| c.path.clone()).collect();
    report.status = if apply(&path, &receipt)? {
        "installed"
    } else {
        "unchanged"
    }
    .into();
    report.notes.push("Refresh runs in the foreground only when .graf/index.db exists. Existing executable hooks are chained; their exit status is preserved. Hooks never stage or commit files.".into());
    Ok(report)
}

#[cfg(test)]
mod path_tests {
    use super::*;

    #[test]
    fn guidance_version_order_is_numeric_and_newer_revisions_win() {
        assert!(package_core("0.10.0").unwrap() > package_core("0.9.27").unwrap());
        assert_eq!(package_core("1.2.3-rc.1+build.2"), package_core("1.2.3"));
        assert!(package_core("not-a-version").is_none());
        assert!(package_core("1.2.3.4").is_none());
        assert_eq!(
            guidance_direction(GuidanceStamp {
                package: (0, 0, 0),
                revision: GUIDANCE_VERSION + 1
            }),
            std::cmp::Ordering::Greater
        );
    }

    #[test]
    fn notice_headers_skip_nonregular_files_and_never_read_past_limit() {
        let temp = tempfile::tempdir().unwrap();
        let skill = temp.path().join("SKILL.md");
        let header = "<!-- graf guidance version: 1; executable: 0.0.0 -->\n";
        fs::write(&skill, header).unwrap();
        assert!(read_guidance_stamp(&skill).is_some());
        fs::write(&skill, format!("{}\n{header}", "x".repeat(4096))).unwrap();
        assert!(read_guidance_stamp(&skill).is_none());
        assert!(read_guidance_stamp(temp.path()).is_none());
        #[cfg(unix)]
        {
            let link = temp.path().join("linked.md");
            std::os::unix::fs::symlink(&skill, &link).unwrap();
            assert!(read_guidance_stamp(&link).is_none());
        }
    }

    #[test]
    fn hermes_platform_roots_use_native_locations() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("synthetic home");
        let appdata = temp.path().join("custom local app data");
        for (windows, local, expected) in [
            (false, Some(appdata.as_path()), home.join(".hermes")),
            (true, Some(appdata.as_path()), appdata.join("hermes")),
            (true, None, home.join("AppData/Local/hermes")),
        ] {
            assert_eq!(hermes_root(&home, windows, local).unwrap(), expected);
        }
        assert!(hermes_root(&home, true, Some(Path::new("relative"))).is_err());
    }
}
