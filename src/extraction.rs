use anyhow::{Context, Result, bail, ensure};
use clap::{Args, Subcommand};
use graf::{
    index::IndexOptions,
    ingest::{self, CommandAdapter, Provider, SemanticOptions},
};
use std::{
    collections::BTreeMap,
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Debug, Default, Args)]
pub struct ExtractionArgs {
    /// JSON IndexOptions. Overrides the stored configuration for this update.
    #[arg(long)]
    pub config: Option<PathBuf>,
    /// Re-extract local files even when their fingerprints are unchanged.
    #[arg(long)]
    pub force: bool,
    /// Include measured detection, extraction and commit times in the report.
    #[arg(long)]
    pub timing: bool,
    /// Bypass semantic/transcript cache reads and re-extract local files once.
    #[arg(long)]
    pub refresh_cache: bool,
    /// Accept fewer semantic facts after saving the prior graph in .graf/backups.
    #[arg(long)]
    pub allow_semantic_shrink: bool,
    #[arg(long)]
    pub code_only: bool,
    /// Also enrich code facts with the explicitly selected semantic provider.
    #[arg(long, conflicts_with = "no_semantic")]
    pub deep: bool,
    #[arg(long)]
    pub no_gitignore: bool,
    #[arg(long)]
    pub include_generated: bool,
    /// Repository-relative Python import root; repeat for multiple source trees.
    #[arg(long)]
    pub python_source_root: Vec<String>,
    /// Explicit Swift module and repository-relative source root, NAME=DIR; repeat as needed.
    #[arg(long)]
    pub swift_module: Vec<String>,
    /// Explicit interpreter with Robot Framework installed; native parsing otherwise stays local.
    #[arg(long)]
    pub robot_python: Option<PathBuf>,
    /// Explicitly enable semantic extraction using a built-in or registered provider.
    #[arg(long, conflicts_with = "no_semantic")]
    pub provider: Option<String>,
    #[arg(long)]
    pub model: Option<String>,
    /// Full provider request URL, including its API route.
    #[arg(long)]
    pub endpoint: Option<String>,
    /// Environment variable containing the API key; never a literal secret.
    #[arg(long)]
    pub key_env: Option<String>,
    #[arg(long)]
    pub no_semantic: bool,
    /// Explicitly allow uploading images to the configured semantic provider.
    #[arg(long, conflicts_with = "no_semantic")]
    pub vision: bool,
    #[arg(long)]
    pub max_semantic_files: Option<usize>,
    /// Maximum provider attempts across this invocation; cache hits are free.
    #[arg(long)]
    pub max_semantic_calls: Option<usize>,
    /// Maximum reserved output-token allowance across provider attempts.
    #[arg(long)]
    pub max_semantic_output_tokens: Option<u64>,
    /// Use installed Tesseract for local image OCR.
    #[arg(long)]
    pub ocr: bool,
    /// Use installed Whisper/FFmpeg with the named model for audio/video.
    #[arg(long)]
    pub whisper: Option<String>,
    /// Override the saved graph's topic hints for the selected Whisper adapter.
    #[arg(long, requires = "whisper")]
    pub whisper_prompt: Option<String>,
    /// Use installed gws when explicitly adding Google pointer documents.
    #[arg(long)]
    pub google: bool,
    /// Use installed yt-dlp when explicitly adding remote media.
    #[arg(long)]
    pub download_media: bool,
    /// Allow explicit URL imports from trusted private or localhost addresses.
    #[arg(long)]
    pub allow_private_urls: bool,
}

impl ExtractionArgs {
    pub fn configure(
        &self,
        mut options: IndexOptions,
        root: &Path,
        db: &Path,
    ) -> Result<IndexOptions> {
        if let Some(path) = &self.config {
            options =
                serde_json::from_slice(&bounded(path)?).context("invalid index configuration")?;
        }
        if self.code_only {
            options.code_only = true;
        }
        options.force = self.force || self.refresh_cache;
        options.timing = self.timing;
        options.ingest.force_cache_refresh = self.refresh_cache;
        options.allow_semantic_shrink = self.allow_semantic_shrink || self.no_semantic;
        if let Some(python) = &self.robot_python {
            options.robot_python = Some(python.clone());
        }
        if let Some(python) = &mut options.robot_python
            && python.is_relative()
            && python.components().count() > 1
        {
            *python = std::path::absolute(root)?.join(&*python);
        }
        if self.deep {
            options.semantic_code = true;
        }
        if self.no_gitignore {
            options.no_gitignore = true;
        }
        if self.include_generated {
            options.include_generated = true;
        }
        if !self.python_source_root.is_empty() {
            options.python_source_roots = self
                .python_source_root
                .iter()
                .map(|root| {
                    if root == "." {
                        String::new()
                    } else {
                        root.clone()
                    }
                })
                .collect();
        }
        if let Some(limit) = self.max_semantic_files {
            ensure!(
                limit <= 100_000,
                "max-semantic-files must be at most 100000"
            );
            options.max_semantic_files = limit;
        }
        if let Some(limit) = self.max_semantic_calls {
            options.max_semantic_calls = Some(limit);
        }
        if let Some(limit) = self.max_semantic_output_tokens {
            options.max_semantic_output_tokens = Some(limit);
        }
        if !self.swift_module.is_empty() {
            options.swift_modules.clear();
            for item in &self.swift_module {
                let (name, directory) = item
                    .split_once('=')
                    .context("swift-module requires NAME=DIR")?;
                ensure!(!name.is_empty(), "Swift module name must not be empty");
                let directory = if directory == "." { "" } else { directory };
                ensure!(
                    options
                        .swift_modules
                        .insert(name.to_owned(), directory.to_owned())
                        .is_none(),
                    "duplicate Swift module name"
                );
            }
        }
        if self.no_semantic {
            options.ingest.semantic = None;
            options.semantic_code = false;
        }
        if let Some(name) = &self.provider {
            let settings = if let Some(provider) = builtin(name) {
                ensure!(
                    provider == Provider::OpenAi || self.model.is_some(),
                    "name a model explicitly for this provider with --model"
                );
                SemanticOptions {
                    provider,
                    ..Default::default()
                }
            } else {
                let mut registry = read_registry(&global_registry()?)?;
                registry.extend(read_registry(&root.join(".graf/providers.json"))?);
                registry
                    .remove(name)
                    .with_context(|| format!("unknown provider {name}"))?
            };
            options.ingest.semantic = Some(settings);
        }
        if self.model.is_some() || self.endpoint.is_some() || self.key_env.is_some() || self.vision
        {
            let settings = options.ingest.semantic.as_mut().context(
                "provider settings require --provider or an existing semantic configuration",
            )?;
            if let Some(model) = &self.model {
                settings.model = model.clone();
            }
            if let Some(endpoint) = &self.endpoint {
                settings.endpoint = endpoint.clone();
            }
            if let Some(key) = &self.key_env {
                settings.key_env = Some(key.clone());
            }
            if self.vision {
                settings.vision = true;
            }
        }
        if self.ocr {
            for ext in ["png", "jpg", "jpeg", "gif", "bmp", "tif", "tiff", "webp"] {
                options
                    .ingest
                    .converters
                    .insert(ext.into(), CommandAdapter::tesseract());
            }
        }
        if let Some(model) = &self.whisper {
            let prompt = if let Some(prompt) = &self.whisper_prompt {
                ensure!(
                    prompt.len() <= 2048 && !prompt.contains('\0'),
                    "Whisper prompt must be at most 2048 bytes without NUL"
                );
                prompt.clone()
            } else if db.try_exists()? {
                graf::store::Store::open_read_only(db)?
                    .transcription_topics()?
                    .join(", ")
            } else {
                String::new()
            };
            let adapter = if prompt.is_empty() {
                CommandAdapter::whisper(model)
            } else {
                CommandAdapter::whisper_with_prompt(model, &prompt)
            };
            for ext in [
                "mp3", "wav", "m4a", "ogg", "flac", "aac", "opus", "mp4", "mkv", "mov", "webm",
                "avi", "m4v",
            ] {
                options
                    .ingest
                    .converters
                    .insert(ext.into(), adapter.clone());
            }
        }
        if self.google {
            for ext in ["gdoc", "gsheet", "gslides"] {
                options
                    .ingest
                    .converters
                    .insert(ext.into(), CommandAdapter::google_workspace());
            }
        }
        if self.download_media {
            options
                .ingest
                .converters
                .insert("url".into(), CommandAdapter::yt_dlp());
        }
        if self.allow_private_urls {
            options.ingest.allow_private_urls = true;
        }
        ingest::config_fingerprint(&options.ingest)?;
        Ok(options)
    }
}

#[derive(Debug, Args)]
pub struct ProviderArgs {
    /// Manage project providers instead of the global registry.
    #[arg(long)]
    pub project: Option<PathBuf>,
    #[command(subcommand)]
    pub command: ProviderCommand,
}
#[derive(Debug, Subcommand)]
pub enum ProviderCommand {
    List,
    Show {
        name: String,
    },
    /// Register a SemanticOptions JSON configuration. Credentials must be environment names.
    Add {
        name: String,
        file: PathBuf,
    },
    Remove {
        name: String,
    },
}

#[derive(Debug, Args)]
pub struct CacheArgs {
    #[command(subcommand)]
    pub command: CacheCommand,
}

#[derive(Debug, Subcommand)]
pub enum CacheCommand {
    /// Inspect bounded semantic cache records without contacting a provider.
    Inspect {
        directory: PathBuf,
        #[arg(long, default_value_t=1000, value_parser=clap::value_parser!(u32).range(1..=100000))]
        limit: u32,
        #[arg(long, default_value_t=8*1024*1024, value_parser=clap::value_parser!(u32).range(1..=16*1024*1024))]
        max_entry_bytes: u32,
    },
    /// Remove exactly one cache entry by its key so a later extraction can retry it.
    Remove { directory: PathBuf, key: String },
}

pub fn cache(args: &CacheArgs) -> Result<serde_json::Value> {
    match &args.command {
        CacheCommand::Inspect {
            directory,
            limit,
            max_entry_bytes,
        } => Ok(serde_json::json!({
            "directory": directory,
            "entries": ingest::inspect_semantic_cache(directory, *limit as usize, *max_entry_bytes as usize)?,
            "limit": limit,
        })),
        CacheCommand::Remove { directory, key } => Ok(serde_json::json!({
            "directory":directory,"key":key,"removed":ingest::remove_semantic_cache_entry(directory,key)?,
        })),
    }
}

const BUILTINS: [&str; 8] = [
    "open_ai",
    "anthropic",
    "gemini",
    "ollama",
    "azure",
    "cli",
    "bedrock",
    "claude_cli",
];
fn builtin(name: &str) -> Option<Provider> {
    match name {
        "open_ai" | "openai" => Some(Provider::OpenAi),
        "anthropic" => Some(Provider::Anthropic),
        "gemini" => Some(Provider::Gemini),
        "ollama" => Some(Provider::Ollama),
        "azure" => Some(Provider::Azure),
        "cli" => Some(Provider::Cli),
        "bedrock" => Some(Provider::Bedrock),
        "claude_cli" => Some(Provider::ClaudeCli),
        _ => None,
    }
}
fn global_registry() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(|h| PathBuf::from(h).join(".graf/providers.json"))
        .context("home directory unavailable; use --project")
}
fn bounded(path: &Path) -> Result<Vec<u8>> {
    use std::io::Read;
    let metadata = std::fs::symlink_metadata(path)?;
    ensure!(
        metadata.is_file() && metadata.len() <= 1024 * 1024,
        "configuration must be a regular file of at most 1 MiB"
    );
    let mut bytes = vec![];
    std::fs::File::open(path)?
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= 1024 * 1024, "configuration exceeds 1 MiB");
    Ok(bytes)
}
fn read_registry(path: &Path) -> Result<BTreeMap<String, SemanticOptions>> {
    if !path.try_exists()? {
        return Ok(BTreeMap::new());
    }
    serde_json::from_slice(&bounded(path)?).context("invalid provider registry")
}
pub fn provider(args: &ProviderArgs) -> Result<serde_json::Value> {
    let path = match &args.project {
        Some(root) => root.join(".graf/providers.json"),
        None => global_registry()?,
    };
    let mut registry = read_registry(&path)?;
    match &args.command {
        ProviderCommand::List => {
            return Ok(
                serde_json::json!({"builtins":BUILTINS,"custom":registry.keys().collect::<Vec<_>>()}),
            );
        }
        ProviderCommand::Show { name } => {
            if let Some(provider) = builtin(name) {
                return Ok(
                    serde_json::json!({"provider":provider,"configuration":"Set the full request endpoint and model explicitly; credentials use key_env."}),
                );
            }
            return serde_json::to_value(registry.get(name).context("provider not found")?)
                .map_err(Into::into);
        }
        ProviderCommand::Add { name, file } => {
            ensure!(
                builtin(name).is_none(),
                "built-in providers cannot be replaced"
            );
            ensure!(
                !name.is_empty()
                    && name.len() <= 64
                    && name
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || "_-".contains(c)),
                "invalid provider name"
            );
            let settings: SemanticOptions = serde_json::from_slice(&bounded(file)?)
                .context("invalid provider configuration")?;
            ingest::config_fingerprint(&ingest::IngestOptions {
                semantic: Some(settings.clone()),
                ..Default::default()
            })?;
            registry.insert(name.clone(), settings);
        }
        ProviderCommand::Remove { name } => {
            if builtin(name).is_some() {
                bail!("built-in providers cannot be removed");
            }
            ensure!(registry.remove(name).is_some(), "provider not found");
        }
    }
    let parent = path.parent().context("registry has no parent")?;
    std::fs::create_dir_all(parent)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer_pretty(&mut temporary, &registry)?;
    temporary.flush()?;
    temporary.as_file().sync_all()?;
    temporary.persist(&path).map_err(|e| e.error)?;
    Ok(serde_json::json!({"status":"updated","path":path}))
}
