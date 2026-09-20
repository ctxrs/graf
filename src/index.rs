use std::{
    collections::{BTreeMap, HashMap},
    fs::OpenOptions,
    io::{Read, Write},
    path::{Path, PathBuf},
};

use crate::{
    ingest::{self, IngestOptions},
    languages,
    model::*,
    parser::{MAX_SOURCE_BYTES, PythonContext, empty_facts, parse_python_with_source_root},
    project_context::{ProjectContext, add_document_aliases},
    store::Store,
};
use anyhow::{Context, Result, bail, ensure};
use ignore::WalkBuilder;
use serde::{Deserialize, Serialize};

const EXTRACTOR_REVISION: u32 = 9;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct IndexOptions {
    pub code_only: bool,
    pub no_gitignore: bool,
    pub include_generated: bool,
    pub max_semantic_files: usize,
    /// Corpus-wide attempted-call and reserved-output limits, in addition to per-file limits.
    pub max_semantic_calls: Option<usize>,
    pub max_semantic_output_tokens: Option<u64>,
    pub semantic_code: bool,
    /// Repository-relative Python import roots; the most specific matching root wins.
    pub python_source_roots: Vec<String>,
    /// Explicit Swift module names mapped to repository-relative source roots.
    pub swift_modules: BTreeMap<String, String>,
    /// Explicit Python interpreter with the official Robot Framework parser installed.
    pub robot_python: Option<PathBuf>,
    /// Re-extract local files for this invocation; never persisted in the index.
    #[serde(skip)]
    pub force: bool,
    /// Accept a smaller semantic result for this invocation, after saving a snapshot.
    #[serde(skip)]
    pub allow_semantic_shrink: bool,
    /// Report wall-clock extraction phase timings for this invocation only.
    #[serde(skip)]
    pub timing: bool,
    pub ingest: IngestOptions,
}
impl Default for IndexOptions {
    fn default() -> Self {
        Self {
            code_only: false,
            no_gitignore: false,
            include_generated: false,
            max_semantic_files: 32,
            max_semantic_calls: None,
            max_semantic_output_tokens: None,
            semantic_code: false,
            python_source_roots: vec![],
            swift_modules: BTreeMap::new(),
            robot_python: None,
            force: false,
            allow_semantic_shrink: false,
            timing: false,
            ingest: IngestOptions::default(),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct Freshness {
    pub generation: u64,
    pub changed: Vec<String>,
    pub added: Vec<String>,
    pub deleted: Vec<String>,
    pub fresh: bool,
}

/// Run-local reservations and provider receipts survive a failed extraction or
/// commit. Available through anyhow downcasting; never written into the graph.
#[derive(Debug, Serialize)]
pub struct FailedSemanticUsage {
    pub semantic_usage: Option<ingest::SemanticUsage>,
    pub provider_usage: Vec<ingest::ProviderUsage>,
    pub usage_unavailable: bool,
    #[serde(skip)]
    message: String,
}
impl std::fmt::Display for FailedSemanticUsage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}; semantic usage before failure: {}",
            self.message,
            serde_json::to_string(self).map_err(|_| std::fmt::Error)?
        )
    }
}
impl std::error::Error for FailedSemanticUsage {}

pub(crate) fn retain_semantic_usage(error: anyhow::Error, options: &IndexOptions) -> anyhow::Error {
    if error.downcast_ref::<FailedSemanticUsage>().is_some() {
        return error;
    }
    let Some(semantic) = &options.ingest.semantic else {
        return error;
    };
    let reserved = semantic
        .runtime_budget
        .as_ref()
        .map(|b| b.usage())
        .transpose();
    let actual = semantic
        .runtime_usage
        .as_ref()
        .map(|r| r.snapshot())
        .transpose();
    let usage = FailedSemanticUsage {
        message: error.to_string(),
        usage_unavailable: reserved.is_err() || actual.is_err(),
        semantic_usage: reserved.ok().flatten(),
        provider_usage: actual.ok().flatten().unwrap_or_default(),
    };
    if usage.usage_unavailable
        || usage.semantic_usage.is_some_and(|u| u.calls > 0)
        || !usage.provider_usage.is_empty()
    {
        error.context(usage)
    } else {
        error
    }
}

pub fn stored_options(db: &Path) -> Result<IndexOptions> {
    if !db.try_exists()? {
        return Ok(IndexOptions::default());
    }
    let value = Store::open_read_only(db)?.graph_metadata()?;
    match value.get("graf_index_options") {
        Some(options) => {
            serde_json::from_value(options.clone()).context("invalid stored index options")
        }
        None => Ok(IndexOptions::default()),
    }
}

pub fn run(root: &Path, db: &Path) -> Result<IndexReport> {
    run_with_options(root, db, &stored_options(db)?)
}

pub fn run_with_options(root: &Path, db: &Path, options: &IndexOptions) -> Result<IndexReport> {
    run_with_reserved_semantic_files(root, db, options, 0)
}

pub(crate) fn run_with_reserved_semantic_files(
    root: &Path,
    db: &Path,
    options: &IndexOptions,
    reserved: usize,
) -> Result<IndexReport> {
    let prepared = prepare_semantic_budget(options);
    run_prepared(root, db, &prepared, reserved)
        .map_err(|error| retain_semantic_usage(error, &prepared))
}

fn run_prepared(
    root: &Path,
    db: &Path,
    options: &IndexOptions,
    reserved: usize,
) -> Result<IndexReport> {
    let started = std::time::Instant::now();
    for source_root in &options.python_source_roots {
        ensure!(
            source_root.is_empty()
                || (!source_root.contains(['\\', ':'])
                    && source_root
                        .split('/')
                        .all(|part| !matches!(part, "" | "." | ".."))),
            "Python source roots must be normalized repository-relative directories (empty means repository root)"
        );
    }
    ensure!(
        !options.semantic_code || options.ingest.semantic.is_some(),
        "deep code extraction requires an explicit semantic provider"
    );
    ensure!(
        options.max_semantic_files <= 100_000,
        "max_semantic_files must be at most 100000"
    );
    ensure!(
        reserved <= options.max_semantic_files,
        "semantic extraction exceeds configured file budget"
    );
    let root = root
        .canonicalize()
        .context("cannot canonicalize index root")?;
    ensure!(root.is_dir(), "index root must be a directory");
    let root_text = root.to_str().context("index root must be UTF-8")?;
    let mut store = Store::create(db)?;
    let stats = store.stats()?;
    ensure!(
        stats.kind != "imported",
        "cannot index native sources into an imported graph"
    );
    ensure!(
        stats.root.as_deref().is_none_or(|old| old == root_text),
        "index root differs from the stored root"
    );
    let mut old: HashMap<_, _> = store
        .file_stamps()?
        .into_iter()
        .map(|f| (f.path, f.hash))
        .collect();
    let (files, mut coverage) = discover(&root, &db.canonicalize()?, options)?;
    let context = ProjectContext::discover_with_swift_modules(
        &root,
        &files.iter().map(|f| f.1.clone()).collect::<Vec<_>>(),
        &options.swift_modules,
    )?;
    let ingest_fingerprint = ingest::config_fingerprint(&options.ingest)?;
    let mut python = python_inventory(&files, options, &old, &context, &ingest_fingerprint)?;
    let detect_ms = started.elapsed().as_secs_f64() * 1000.0;
    let extracting = std::time::Instant::now();
    let mut changed = vec![];
    let mut semantic_files = reserved;
    for (path, relative) in files {
        let code = is_code(&relative) || content_probe(&relative);
        let maximum = if code && !relative.ends_with(".dmi") {
            MAX_SOURCE_BYTES as u64
        } else {
            options.ingest.max_input_bytes
        };
        let (content_hash, content) = read_source(&path, maximum)?;
        context.validate_source(&relative, &content_hash)?;
        let hash = stamp(
            &relative,
            &content_hash,
            &context,
            &ingest_fingerprint,
            options,
            &python.fingerprint,
        );
        let unchanged = old
            .remove(&relative)
            .is_some_and(|previous| previous == hash);
        if unchanged && !options.force {
            coverage.unchanged_files += 1;
            continue;
        }
        let mut facts = if code {
            match content {
                Some(bytes) if relative.ends_with(".dmi") => {
                    languages::extended::parse_dmi(&relative, &bytes, &hash)?
                }
                Some(bytes) if relative.ends_with(".dfm") => {
                    languages::extended::parse_pascal_form_bytes(&relative, &bytes, &hash)?
                }
                Some(bytes) => match std::str::from_utf8(&bytes) {
                    Ok(source) => {
                        let mut facts = if relative.ends_with(".py")
                            || (Path::new(&relative).extension().is_none()
                                && languages::scripted::shebang_language(source) == Some("python"))
                        {
                            let mut facts = python
                                .facts
                                .remove(&relative)
                                .context("Python inventory changed during scan; retry indexing")?;
                            ensure!(
                                facts.hash == content_hash,
                                "Python source changed during scan; retry indexing"
                            );
                            facts.hash = hash.clone();
                            python
                                .context
                                .as_ref()
                                .context("Python inventory changed during scan; retry indexing")?
                                .apply(&mut facts);
                            facts
                        } else if is_robot(&relative) && options.robot_python.is_some() {
                            let python = options.robot_python.as_ref().unwrap();
                            let python = if python.is_relative() && python.components().count() > 1
                            {
                                root.join(python)
                            } else {
                                python.clone()
                            };
                            languages::templates::parse_robot_official(
                                &relative, source, &hash, &python,
                            )?
                        } else {
                            languages::parse(&relative, source, &hash)?.unwrap_or_else(|| {
                                diagnostic(
                                    &relative,
                                    &hash,
                                    "file content does not match a supported language",
                                )
                            })
                        };
                        if options.semantic_code && !facts.nodes.is_empty() {
                            semantic_files += 1;
                            ensure!(
                                semantic_files <= options.max_semantic_files,
                                "semantic extraction exceeds configured file budget"
                            );
                            ingest::enrich_facts(&mut facts, source, &options.ingest)?;
                        }
                        facts
                    }
                    Err(_) => diagnostic(&relative, &hash, "source is not UTF-8"),
                },
                None => diagnostic(
                    &relative,
                    &hash,
                    if relative.ends_with(".dmi") {
                        "source exceeds configured input byte limit"
                    } else {
                        "source exceeds the 4 MiB limit"
                    },
                ),
            }
        } else {
            if options.ingest.semantic.is_some() {
                semantic_files += 1;
                ensure!(
                    semantic_files <= options.max_semantic_files,
                    "semantic extraction exceeds the configured file budget; increase max_semantic_files explicitly"
                );
            }
            let bytes = content
                .context("document exceeds configured input byte limit; previous graph retained")?;
            let suffix = path
                .extension()
                .and_then(|s| s.to_str())
                .map(|s| format!(".{s}"))
                .unwrap_or_default();
            // Converters receive immutable task-owned bytes, not a source path
            // that could change after its fingerprint was computed.
            let mut staged = tempfile::Builder::new()
                .prefix("graf-ingest-")
                .suffix(&suffix)
                .tempfile()?;
            staged.write_all(&bytes)?;
            staged.flush()?;
            ingest::extract(staged.path(), &relative, &hash, &options.ingest)?
        };
        context.apply(&mut facts);
        add_document_aliases(&mut facts);
        changed.push(facts);
    }
    for mut facts in crate::sources::read(&root)? {
        coverage.supported_files += 1;
        if old
            .remove(&facts.path)
            .is_some_and(|previous| previous == facts.hash)
        {
            coverage.unchanged_files += 1;
        } else {
            add_document_aliases(&mut facts);
            changed.push(facts);
        }
    }
    changed.sort_by(|a, b| a.path.cmp(&b.path));
    let mut deleted: Vec<_> = old.into_keys().collect();
    deleted.sort();
    let extract_ms = extracting.elapsed().as_secs_f64() * 1000.0;
    let committing = std::time::Instant::now();
    let losses = store.semantic_losses(&changed)?;
    if !losses.is_empty() {
        ensure!(
            options.allow_semantic_shrink,
            "semantic extraction would remove recorded facts from {}; previous graph retained. Review the result and use --allow-semantic-shrink to accept it with a backup",
            losses.join(", ")
        );
        preserve_snapshot(&root, &store)?;
    }
    let mut report = store.apply_native_with_options(
        root_text,
        changed,
        deleted,
        coverage,
        serde_json::to_value(options)?,
    )?;
    report.semantic_usage = options
        .ingest
        .semantic
        .as_ref()
        .and_then(|semantic| semantic.runtime_budget.as_ref())
        .map(|budget| budget.usage())
        .transpose()?;
    report.provider_usage = options
        .ingest
        .semantic
        .as_ref()
        .and_then(|semantic| semantic.runtime_usage.as_ref())
        .map(|recorder| recorder.snapshot())
        .transpose()?;
    if options.timing {
        report.timings = Some(IndexTimings {
            detect_ms,
            extract_ms,
            commit_ms: committing.elapsed().as_secs_f64() * 1000.0,
            total_ms: started.elapsed().as_secs_f64() * 1000.0,
            capture_ms: None,
        });
    }
    Ok(report)
}

pub(crate) fn prepare_semantic_budget(options: &IndexOptions) -> IndexOptions {
    let mut prepared = options.clone();
    if let Some(semantic) = &mut prepared.ingest.semantic {
        if semantic.runtime_budget.is_none() {
            semantic.runtime_budget = Some(std::sync::Arc::new(ingest::SemanticBudget::new(
                options.max_semantic_calls,
                options.max_semantic_output_tokens,
            )));
        }
        if semantic.runtime_usage.is_none() {
            semantic.runtime_usage =
                Some(std::sync::Arc::new(ingest::SemanticUsageRecorder::default()));
        }
    }
    prepared
}

pub(crate) fn semantic_counts(facts: &FileFacts) -> (usize, usize) {
    (
        facts
            .nodes
            .iter()
            .filter(|n| semantic_provenance(&n.metadata))
            .count(),
        facts
            .edges
            .iter()
            .filter(|e| semantic_provenance(&e.metadata))
            .count(),
    )
}

pub(crate) fn semantic_provenance(metadata: &serde_json::Value) -> bool {
    metadata["inferred"] == true
        && matches!(
            metadata["provenance"].as_str(),
            Some("semantic" | "visual_inference")
        )
}

/// Portable graph evidence, written before an explicitly accepted semantic loss.
/// Native ownership/index state is not restored by importing this snapshot.
pub(crate) fn preserve_snapshot(root: &Path, store: &Store) -> Result<PathBuf> {
    let snapshot = store.snapshot()?;
    let bytes = serde_json::to_vec(&snapshot)?;
    preserve_backup(root, &format!("graph-{}", snapshot.generation), &bytes)
}

pub(crate) fn preserve_backup(root: &Path, prefix: &str, bytes: &[u8]) -> Result<PathBuf> {
    ensure!(
        !prefix.is_empty()
            && prefix
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-'),
        "invalid backup name"
    );
    ensure!(
        bytes.len() <= 256 * 1024 * 1024,
        "semantic backup exceeds snapshot size limit"
    );
    let directory = root.join(".graf/backups");
    for path in [root.join(".graf"), directory.clone()] {
        match std::fs::create_dir(&path) {
            Ok(()) => (),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                let metadata = std::fs::symlink_metadata(&path)?;
                ensure!(
                    metadata.is_dir() && !metadata.file_type().is_symlink(),
                    "backup path must be a regular directory"
                );
            }
            Err(error) => return Err(error).context("cannot create backup directory"),
        }
    }
    let name = format!("{prefix}-{}.json", blake3::hash(bytes).to_hex());
    let destination = directory.join(name);
    if destination.try_exists()? {
        let metadata = std::fs::symlink_metadata(&destination)?;
        ensure!(
            metadata.is_file() && !metadata.file_type().is_symlink(),
            "backup must be a regular file"
        );
        let existing = read_source(&destination, 256 * 1024 * 1024)?.1;
        ensure!(
            existing.as_deref() == Some(bytes),
            "existing backup content differs"
        );
        return Ok(destination);
    }
    let mut staged = tempfile::NamedTempFile::new_in(&directory)?;
    staged.write_all(bytes)?;
    staged.as_file().sync_all()?;
    staged
        .persist_noclobber(&destination)
        .map_err(|e| e.error)
        .context("cannot publish semantic backup")?;
    Ok(destination)
}

pub fn check_update(root: &Path, db: &Path) -> Result<Freshness> {
    let root = root.canonicalize()?;
    let store = Store::open_read_only(db)?;
    let stats = store.stats()?;
    ensure!(
        stats.kind == "native",
        "check-update requires a native graph"
    );
    ensure!(
        stats.root.as_deref() == root.to_str(),
        "index root differs from the stored root"
    );
    let options = stored_options(db)?;
    let (files, _) = discover(&root, &db.canonicalize()?, &options)?;
    let context = ProjectContext::discover_with_swift_modules(
        &root,
        &files.iter().map(|f| f.1.clone()).collect::<Vec<_>>(),
        &options.swift_modules,
    )?;
    let ingest_fingerprint = ingest::config_fingerprint(&options.ingest)?;
    let mut old: HashMap<_, _> = store
        .file_stamps()?
        .into_iter()
        .map(|f| (f.path, f.hash))
        .collect();
    let python = python_inventory(&files, &options, &old, &context, &ingest_fingerprint)?;
    let mut result = Freshness {
        generation: stats.generation,
        changed: vec![],
        added: vec![],
        deleted: vec![],
        fresh: false,
    };
    for (path, relative) in files {
        let maximum =
            if (is_code(&relative) || content_probe(&relative)) && !relative.ends_with(".dmi") {
                MAX_SOURCE_BYTES as u64
            } else {
                options.ingest.max_input_bytes
            };
        let (hash, _) = read_source(&path, maximum)?;
        context.validate_source(&relative, &hash)?;
        let hash = stamp(
            &relative,
            &hash,
            &context,
            &ingest_fingerprint,
            &options,
            &python.fingerprint,
        );
        match old.remove(&relative) {
            None => result.added.push(relative),
            Some(previous) if previous != hash => result.changed.push(relative),
            _ => (),
        }
    }
    for facts in crate::sources::read(&root)? {
        match old.remove(&facts.path) {
            None => result.added.push(facts.path),
            Some(previous) if previous != facts.hash => result.changed.push(facts.path),
            _ => (),
        }
    }
    result.added.sort();
    result.changed.sort();
    result.deleted = old.into_keys().collect();
    result.deleted.sort();
    result.fresh =
        result.added.is_empty() && result.changed.is_empty() && result.deleted.is_empty();
    Ok(result)
}

fn is_code(path: &str) -> bool {
    path.ends_with(".py") || languages::supports(path)
}
fn is_robot(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("robot") || extension.eq_ignore_ascii_case("resource")
        })
}
fn generic_json(path: &str) -> bool {
    path.ends_with(".json") || path.ends_with(".jsonc")
}
fn content_probe(path: &str) -> bool {
    generic_json(path) || Path::new(path).extension().is_none()
}
fn python_source_root<'a>(path: &str, options: &'a IndexOptions) -> Option<&'a str> {
    options
        .python_source_roots
        .iter()
        .filter(|root| {
            root.is_empty()
                || path
                    .strip_prefix(root.as_str())
                    .is_some_and(|tail| tail.starts_with('/'))
        })
        .max_by_key(|root| root.len())
        .map(String::as_str)
}

struct PythonInventory {
    context: Option<PythonContext>,
    fingerprint: String,
    facts: HashMap<String, FileFacts>,
}

fn python_inventory(
    files: &[(PathBuf, String)],
    options: &IndexOptions,
    old: &HashMap<String, String>,
    project: &ProjectContext,
    ingest_fingerprint: &str,
) -> Result<PythonInventory> {
    let possible_python = |path: &str| {
        !path.starts_with(".graf/sources/")
            && (path.ends_with(".py") || Path::new(path).extension().is_none())
    };
    let paths: std::collections::BTreeSet<_> = files
        .iter()
        .map(|(_, path)| path.as_str())
        .filter(|p| possible_python(p))
        .collect();
    let old_paths: std::collections::BTreeSet<_> = old
        .keys()
        .map(String::as_str)
        .filter(|p| possible_python(p))
        .collect();
    // Reuse only the stamp, never old parsed facts. If any possible Python file,
    // option, inventory member or extractor revision differs, rebuild context.
    // This still hashes source bytes, so equal timestamps cannot conceal edits.
    if !options.force && !paths.is_empty() && paths == old_paths {
        let first = paths.first().unwrap();
        let previous = &old[*first];
        let token = if first.ends_with(".py") {
            previous.split(':').nth(1)
        } else {
            previous.split(':').nth(2)
        };
        if let Some(token) =
            token.filter(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
        {
            let mut unchanged = true;
            for (path, relative) in files.iter().filter(|(_, path)| possible_python(path)) {
                let (hash, _) = read_source(path, MAX_SOURCE_BYTES as u64)?;
                if old.get(relative)
                    != Some(&stamp(
                        relative,
                        &hash,
                        project,
                        ingest_fingerprint,
                        options,
                        token,
                    ))
                {
                    unchanged = false;
                    break;
                }
            }
            if unchanged {
                return Ok(PythonInventory {
                    context: None,
                    fingerprint: token.to_owned(),
                    facts: HashMap::new(),
                });
            }
        }
    }
    let mut facts = Vec::new();
    for (path, relative) in files {
        if !relative.ends_with(".py") && Path::new(relative).extension().is_some() {
            continue;
        }
        let (hash, bytes) = read_source(path, MAX_SOURCE_BYTES as u64)?;
        let Some(bytes) = bytes else { continue };
        let Ok(source) = std::str::from_utf8(&bytes) else {
            continue;
        };
        if !relative.ends_with(".py")
            && languages::scripted::shebang_language(source) != Some("python")
        {
            continue;
        }
        facts.push(parse_python_with_source_root(
            relative,
            source,
            &hash,
            python_source_root(relative, options),
        )?);
    }
    let context = PythonContext::from_facts(&facts);
    Ok(PythonInventory {
        fingerprint: context.fingerprint().to_owned(),
        context: Some(context),
        facts: facts.into_iter().map(|f| (f.path.clone(), f)).collect(),
    })
}

/// Recover the raw local-source digest from formats produced by this indexer.
/// Managed captures hash a saved extraction record instead, so never qualify.
pub(crate) fn indexed_source_digest(stamp: &str) -> Option<&str> {
    let (family, _) = stamp.split_once(':')?;
    let known = if let Some(version) = family.strip_prefix("python-v") {
        version
            .parse::<u32>()
            .ok()
            .is_some_and(|v| (1..=EXTRACTOR_REVISION).contains(&v))
    } else if let Some(version) = family.strip_prefix("languages-native-languages-") {
        let current = languages::revision()
            .strip_prefix("native-languages-")?
            .parse::<u32>()
            .ok()?;
        version
            .parse::<u32>()
            .ok()
            .is_some_and(|v| (1..=current).contains(&v))
    } else {
        family == "ingest-v1"
    };
    if !known || stamp.split(':').count() < 3 || stamp.contains(":oversized:") {
        return None;
    }
    let digest = stamp.rsplit(':').next()?;
    (digest.len() == 64
        && digest
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
    .then_some(digest)
}

fn stamp(
    path: &str,
    hash: &str,
    context: &ProjectContext,
    ingest_fingerprint: &str,
    options: &IndexOptions,
    python_context: &str,
) -> String {
    let hash = if options.semantic_code {
        format!("deep:{ingest_fingerprint}:{hash}")
    } else {
        hash.to_owned()
    };
    let hash = if let Some(root) = python_source_root(path, options)
        .filter(|_| path.ends_with(".py") || Path::new(path).extension().is_none())
    {
        format!(
            "source-root:{}:{hash}",
            blake3::hash(root.as_bytes()).to_hex()
        )
    } else {
        hash
    };
    let hash = if let Some(python) = options.robot_python.as_ref().filter(|_| is_robot(path)) {
        format!(
            "robot-official-v1:{}:{hash}",
            blake3::hash(python.as_os_str().as_encoded_bytes()).to_hex()
        )
    } else {
        hash
    };
    if path.ends_with(".py") {
        format!("python-v{EXTRACTOR_REVISION}:{python_context}:{hash}")
    } else if languages::supports(path) || content_probe(path) {
        format!(
            "languages-{}:{}:{}:{hash}",
            languages::revision(),
            context.fingerprint(path),
            if Path::new(path).extension().is_none() {
                python_context
            } else {
                ""
            },
        )
    } else {
        format!("ingest-v1:{ingest_fingerprint}:{hash}")
    }
}

fn discover(
    root: &Path,
    db: &Path,
    options: &IndexOptions,
) -> Result<(Vec<(PathBuf, String)>, Coverage)> {
    let db = db.to_path_buf();
    let sidecars: Vec<_> = ["-wal", "-shm", "-journal"]
        .iter()
        .map(|suffix| {
            let mut path = db.as_os_str().to_owned();
            path.push(suffix);
            PathBuf::from(path)
        })
        .collect();
    let include_generated = options.include_generated;
    let mut walker = WalkBuilder::new(root);
    walker
        .hidden(false)
        .follow_links(false)
        .require_git(false)
        .git_global(false)
        .parents(false)
        .git_ignore(!options.no_gitignore)
        .git_exclude(!options.no_gitignore)
        .add_custom_ignore_filename(".graphifyignore")
        .add_custom_ignore_filename(".grafignore")
        .filter_entry(move |entry| {
            if entry.depth() == 0 {
                return true;
            }
            if entry.file_type().is_some_and(|t| t.is_symlink()) {
                return false;
            }
            if entry.path() == db || sidecars.iter().any(|p| p == entry.path()) {
                return false;
            }
            !entry.file_type().is_some_and(|t| {
                t.is_dir()
                    && match entry.file_name().to_str() {
                        Some(".git" | ".graf") => true,
                        Some(
                            ".venv" | "venv" | "env" | "__pycache__" | "node_modules"
                            | "site-packages" | "target" | "build" | "dist" | ".tox" | ".nox"
                            | ".mypy_cache" | ".pytest_cache" | ".ruff_cache",
                        ) => !include_generated,
                        _ => false,
                    }
            })
        });
    let mut coverage = Coverage::default();
    let mut files = vec![];
    for entry in walker.build() {
        let entry = entry.context("cannot walk index root")?;
        if let Some(error) = entry.error() {
            bail!("cannot apply ignore rules: {error}");
        }
        if entry.file_type().is_some_and(|t| t.is_dir()) {
            check_ignore_files(entry.path(), options.no_gitignore)?;
        }
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let relative = entry
            .path()
            .strip_prefix(root)?
            .to_str()
            .context("source path must be UTF-8")?
            .to_owned();
        #[cfg(windows)]
        let relative = relative.replace('\\', "/");
        let recognized_content = if content_probe(&relative) && !languages::supports(&relative) {
            let (_, content) = read_source(entry.path(), MAX_SOURCE_BYTES as u64)?;
            content
                .as_deref()
                .and_then(|b| std::str::from_utf8(b).ok())
                .is_some_and(|s| languages::recognizes(&relative, s))
        } else {
            false
        };
        if !is_code(&relative)
            && !recognized_content
            && (options.code_only || !ingest::supports(Path::new(&relative)))
        {
            coverage.unsupported_files += 1;
            continue;
        }
        coverage.supported_files += 1;
        files.push((entry.path().to_path_buf(), relative));
    }
    files.sort_by(|a, b| a.1.cmp(&b.1));
    Ok((files, coverage))
}

fn check_ignore_files(directory: &Path, no_gitignore: bool) -> Result<()> {
    // WalkBuilder suppresses ignore-file I/O errors, including invalid UTF-8.
    // Validate each visited directory so partial rules cannot publish a graph
    // that silently includes excluded files. Pruned subtrees need no validation.
    for name in [
        ".ignore",
        ".gitignore",
        ".git/info/exclude",
        ".grafignore",
        ".graphifyignore",
    ] {
        if no_gitignore && matches!(name, ".gitignore" | ".git/info/exclude") {
            continue;
        }
        let path = directory.join(name);
        match std::fs::metadata(&path) {
            Ok(metadata) => anyhow::ensure!(
                metadata.is_file(),
                "ignore rules must be a regular file: {}",
                path.display()
            ),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) =>
            {
                continue;
            }
            Err(error) => return Err(error).context("cannot inspect ignore rules"),
        }
        if let Some(error) = ignore::gitignore::GitignoreBuilder::new(directory).add(&path) {
            bail!("cannot apply ignore rules: {error}");
        }
    }
    Ok(())
}

fn diagnostic(path: &str, hash: &str, message: &str) -> FileFacts {
    let mut facts = empty_facts(path, hash);
    facts.diagnostics.push(Diagnostic {
        file: path.into(),
        line: None,
        message: message.into(),
    });
    facts
}

pub(crate) fn read_source(path: &Path, maximum: u64) -> Result<(String, Option<Vec<u8>>)> {
    for _ in 0..2 {
        if !std::fs::symlink_metadata(path)?.is_file() {
            bail!("source is no longer a regular file: {}", path.display());
        }
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.custom_flags(0x00200000); // FILE_FLAG_OPEN_REPARSE_POINT
        }
        let mut file = options
            .open(path)
            .with_context(|| format!("cannot read {}", path.display()))?;
        let before = file.metadata()?;
        ensure!(
            before.is_file() && !before.file_type().is_symlink(),
            "source is no longer a regular file: {}",
            path.display()
        );
        if before.len() > maximum {
            if file.metadata()?.len() > maximum {
                let size = if maximum == MAX_SOURCE_BYTES as u64 {
                    "4MiB".to_owned()
                } else {
                    maximum.to_string()
                };
                return Ok((format!("oversized:{size}"), None));
            }
            continue;
        }
        let modified = before.modified()?;
        let mut hasher = blake3::Hasher::new();
        let mut content = Some(Vec::new());
        let mut buffer = [0; 64 * 1024];
        let mut count = 0u64;
        // One extra byte detects growth without chasing an indefinitely growing file.
        let mut bounded = (&mut file).take(before.len().saturating_add(1));
        loop {
            let size = bounded
                .read(&mut buffer)
                .with_context(|| format!("cannot read {}", path.display()))?;
            if size == 0 {
                break;
            }
            count += size as u64;
            hasher.update(&buffer[..size]);
            if let Some(bytes) = &mut content {
                if bytes.len() + size <= maximum as usize {
                    bytes.extend_from_slice(&buffer[..size]);
                } else {
                    content = None;
                }
            }
        }
        let after = file.metadata()?;
        if count == before.len() && before.len() == after.len() && modified == after.modified()? {
            return Ok((hasher.finalize().to_hex().to_string(), content));
        }
    }
    bail!(
        "source changed during both read attempts: {}",
        path.display()
    )
}
