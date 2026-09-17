use std::{collections::HashMap, fs::File, io::Read, path::Path};

use anyhow::{Context, Result, bail};
use ignore::WalkBuilder;

use crate::{
    model::*,
    parser::{MAX_SOURCE_BYTES, empty_facts, parse_python},
    store::Store,
};

// Bump when parser, binding-resolution, grammar, or extraction settings change
// stored facts. This invalidates every file stamp, including skipped sources.
const EXTRACTOR_REVISION: u32 = 3;

/// Collect file facts before applying any graph changes.
///
/// Each file read checks descriptor size and modification time and retries once
/// if either changes. The directory is not a simultaneous filesystem snapshot:
/// files may change between reads, and edits preserving both metadata values
/// cannot be detected by this check. Run another update after writers finish.
/// Files over 4 MiB are skipped with diagnostics and a stable over-limit stamp;
/// their contents are neither read nor hashed until they shrink below the limit.
pub fn run(root: &Path, db: &Path) -> Result<IndexReport> {
    let root = root
        .canonicalize()
        .context("cannot canonicalize index root")?;
    if !root.is_dir() {
        bail!("index root must be a directory");
    }
    let root_text = root.to_str().context("index root must be UTF-8")?;
    let mut store = Store::create(db)?;
    let stats = store.stats()?;
    if stats.kind == "imported" {
        bail!("cannot index Python into an imported graph");
    }
    if stats.root.as_deref().is_some_and(|old| old != root_text) {
        bail!("index root differs from the stored root");
    }
    let mut old: HashMap<_, _> = store
        .file_stamps()?
        .into_iter()
        .map(|f| (f.path, f.hash))
        .collect();
    let mut coverage = Coverage::default();
    let mut changed = vec![];
    let db = db
        .canonicalize()
        .context("cannot canonicalize index database")?;
    let sidecars: Vec<_> = ["-wal", "-shm", "-journal"]
        .iter()
        .map(|suffix| {
            let mut path = db.as_os_str().to_owned();
            path.push(suffix);
            std::path::PathBuf::from(path)
        })
        .collect();
    let mut walker = WalkBuilder::new(&root);
    walker
        .hidden(false)
        .follow_links(false)
        .require_git(false)
        .git_global(false)
        .parents(false)
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
                    && matches!(
                        entry.file_name().to_str(),
                        Some(
                            ".git"
                                | ".graf"
                                | ".venv"
                                | "venv"
                                | "env"
                                | "__pycache__"
                                | "node_modules"
                                | "site-packages"
                                | "target"
                                | "build"
                                | "dist"
                                | ".tox"
                                | ".nox"
                                | ".mypy_cache"
                                | ".pytest_cache"
                                | ".ruff_cache"
                        )
                    )
            })
        });
    for entry in walker.build() {
        let entry = entry.context("cannot walk index root")?;
        if let Some(error) = entry.error() {
            bail!("cannot apply ignore rules: {error}");
        }
        if entry.file_type().is_some_and(|t| t.is_dir()) {
            check_ignore_files(entry.path())?;
        }
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        if entry.path().extension().is_none_or(|ext| ext != "py") {
            coverage.unsupported_files += 1;
            continue;
        }
        let relative = entry
            .path()
            .strip_prefix(&root)?
            .to_str()
            .context("Python file path must be UTF-8")?;
        #[cfg(windows)]
        let relative = relative.replace('\\', "/");
        #[cfg(windows)]
        let relative = relative.as_str();
        coverage.supported_files += 1;
        let (hash, content) = read_source(entry.path())?;
        let hash = format!("python-v{EXTRACTOR_REVISION}:{hash}");
        if old
            .remove(relative)
            .is_some_and(|previous| previous == hash)
        {
            coverage.unchanged_files += 1;
            continue;
        }
        let facts = match content {
            Some(bytes) => match std::str::from_utf8(&bytes) {
                Ok(source) => parse_python(relative, source, &hash)?,
                Err(_) => diagnostic(relative, &hash, "Python source is not UTF-8"),
            },
            None => diagnostic(relative, &hash, "Python source exceeds the 4 MiB limit"),
        };
        changed.push(facts);
    }
    changed.sort_by(|a, b| a.path.cmp(&b.path));
    let mut deleted: Vec<_> = old.into_keys().collect();
    deleted.sort();
    store.apply_native(root_text, changed, deleted, coverage)
}

fn check_ignore_files(directory: &Path) -> Result<()> {
    // WalkBuilder suppresses ignore-file I/O errors, including invalid UTF-8.
    // Validate each visited directory so partial rules cannot publish a graph
    // that silently includes excluded files. Pruned subtrees need no validation.
    for name in [".ignore", ".gitignore", ".git/info/exclude"] {
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

fn read_source(path: &Path) -> Result<(String, Option<Vec<u8>>)> {
    for _ in 0..2 {
        if !std::fs::symlink_metadata(path)?.is_file() {
            bail!(
                "Python source is no longer a regular file: {}",
                path.display()
            );
        }
        let mut file =
            File::open(path).with_context(|| format!("cannot read {}", path.display()))?;
        let before = file.metadata()?;
        if before.len() > MAX_SOURCE_BYTES as u64 {
            if file.metadata()?.len() > MAX_SOURCE_BYTES as u64 {
                return Ok(("oversized:4MiB".into(), None));
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
                if bytes.len() + size <= MAX_SOURCE_BYTES {
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
        "Python source changed during both read attempts: {}",
        path.display()
    )
}
