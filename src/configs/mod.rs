pub mod secrets;

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::core::task::InterfaceMode;
use crate::error::{JumabekError, JumabekResult};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub memory: MemorySection,
    pub llm: LlmSection,
    pub agent: AgentSection,
    #[serde(default)]
    pub preflight: PreflightSection,
    #[serde(default)]
    pub inbox: InboxSection,
    #[serde(default)]
    pub skills: std::collections::BTreeMap<String, std::collections::BTreeMap<String, String>>,

    #[serde(skip)]
    pub system_prompt: String,
    /// Where that prompt came from. Kept so the upgrade check has a file to
    /// look at rather than a setting to re-resolve.
    #[serde(skip)]
    pub system_prompt_file: PathBuf,
    #[serde(skip)]
    pub api_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemorySection {
    #[serde(default = "default_db_path")]
    pub db_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmSection {
    pub model: String,
    pub base_uri: String,
    #[serde(default = "default_prompt_path")]
    pub system_prompt_path: String,
    #[serde(default = "default_context_limit")]
    pub context_token_limit: u32,
    #[serde(default = "default_retry_max_retries")]
    pub retry_max_retries: u32,
    #[serde(default = "default_retry_initial_delay_ms")]
    pub retry_initial_delay_ms: u64,
    /// How long one request may take. The default suits a hosted endpoint; a
    /// model running on your own hardware can spend minutes on a single turn,
    /// and a timeout it cannot meet makes it unusable however well everything
    /// else is configured.
    #[serde(default = "default_request_timeout")]
    pub request_timeout_sec: u64,
    /// Sent as `reasoning_effort` when set, omitted when empty.
    ///
    /// `"none"` is what stops a hybrid model thinking out loud on Ollama —
    /// measured, unlike its own `think` field, which the OpenAI-compatible
    /// endpoint silently ignores. Left empty by default because an endpoint
    /// that does not know the field may reject the request outright.
    #[serde(default)]
    pub reasoning_effort: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSection {
    #[serde(default = "default_max_iterations")]
    pub max_iterations: u32,
    #[serde(default = "default_max_fix_iterations")]
    pub max_fix_iterations: u32,
    #[serde(default = "default_interface")]
    pub interface: String,
    #[serde(default = "default_skill_timeout")]
    pub skill_timeout_sec: u64,
    #[serde(default)]
    pub voice_name: Option<String>,
    #[serde(default = "default_language")]
    pub language: String,
    #[serde(default = "default_carry_over")]
    pub carry_over_messages: u32,
}

fn default_carry_over() -> u32 {
    30
}

/// The door skills and local programs knock on. Off unless switched on, and
/// bound to the loopback address only — a port that runs tasks on this machine
/// is a shell, and one reachable from the network is somebody else's shell.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InboxSection {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_inbox_port")]
    pub port: u16,
    #[serde(default = "default_inbox_timeout")]
    pub ask_timeout_sec: u64,
    #[serde(default)]
    pub grants: std::collections::BTreeMap<String, crate::core::task::Grant>,
}

impl Default for InboxSection {
    fn default() -> Self {
        InboxSection {
            enabled: false,
            port: default_inbox_port(),
            ask_timeout_sec: default_inbox_timeout(),
            grants: std::collections::BTreeMap::new(),
        }
    }
}

fn default_inbox_port() -> u16 {
    20129
}

fn default_inbox_timeout() -> u64 {
    300
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreflightSection {
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// The Rust image, kept under its old name so an existing config keeps
    /// working. Other languages are named in `images`.
    #[serde(default = "default_image")]
    pub image: String,
    /// Per-language overrides, keyed by language id: `python`, `node`, `rust`.
    /// Anything not listed falls back to the language's own pinned default.
    #[serde(default)]
    pub images: std::collections::BTreeMap<String, String>,
    #[serde(default = "default_build_cpu")]
    pub build_cpu: String,
    #[serde(default = "default_build_memory")]
    pub build_memory: String,
    #[serde(default = "default_run_cpu")]
    pub run_cpu: String,
    #[serde(default = "default_run_memory")]
    pub run_memory: String,
    #[serde(default = "default_build_timeout")]
    pub build_timeout_sec: u64,
    #[serde(default)]
    pub allow_without_docker: bool,
}

impl Default for PreflightSection {
    fn default() -> Self {
        PreflightSection {
            enabled: true,
            image: default_image(),
            images: std::collections::BTreeMap::new(),
            build_cpu: default_build_cpu(),
            build_memory: default_build_memory(),
            run_cpu: default_run_cpu(),
            run_memory: default_run_memory(),
            build_timeout_sec: default_build_timeout(),
            allow_without_docker: false,
        }
    }
}

impl PreflightSection {
    /// The image a skill in this language is built and checked in.
    ///
    /// `[preflight].image` predates there being more than one language, so it
    /// still means "the Rust image" — renaming it would break every config
    /// already on disk for no gain.
    pub fn image_for(&self, language: crate::core::languages::Language) -> &str {
        if let Some(named) = self.images.get(language.id()) {
            return named;
        }
        if language.needs_sdk() {
            return &self.image;
        }
        language.default_image()
    }
}

fn default_true() -> bool {
    true
}
fn default_image() -> String {
    "rust:1-slim".to_string()
}
fn default_build_cpu() -> String {
    "2".to_string()
}
fn default_build_memory() -> String {
    "2g".to_string()
}
fn default_run_cpu() -> String {
    "0.5".to_string()
}
fn default_run_memory() -> String {
    "256m".to_string()
}
fn default_build_timeout() -> u64 {
    600
}

fn default_db_path() -> String {
    "~/.jumabek/jumabek.db".to_string()
}
fn default_prompt_path() -> String {
    "./prompt.md".to_string()
}
fn default_context_limit() -> u32 {
    128_000
}
fn default_retry_max_retries() -> u32 {
    3
}
fn default_request_timeout() -> u64 {
    180
}
fn default_retry_initial_delay_ms() -> u64 {
    1000
}
fn default_max_iterations() -> u32 {
    10
}
fn default_max_fix_iterations() -> u32 {
    5
}
fn default_interface() -> String {
    "cli".to_string()
}
fn default_skill_timeout() -> u64 {
    360
}
fn default_language() -> String {
    "ru".to_string()
}

pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

pub fn jumabek_dir() -> Option<PathBuf> {
    home_dir().map(|home| home.join(".jumabek"))
}

pub fn expand_tilde(raw: &str) -> PathBuf {
    if let Some(rest) = raw.strip_prefix("~/").or_else(|| raw.strip_prefix("~\\"))
        && let Some(home) = home_dir()
    {
        return home.join(rest);
    }
    PathBuf::from(raw)
}

pub fn find_file(filename: &str) -> JumabekResult<PathBuf> {
    let mut checked = Vec::new();

    for dir in [Some(PathBuf::from(".")), jumabek_dir()]
        .into_iter()
        .flatten()
    {
        let path = dir.join(filename);
        if path.exists() {
            return Ok(path);
        }
        checked.push(path.display().to_string());
    }

    Err(JumabekError::ConfigError(format!(
        "{} not found. Checked: {}",
        filename,
        checked.join(", ")
    )))
}

impl Config {
    pub fn load() -> JumabekResult<(Self, PathBuf)> {
        let path = find_file("config.toml")?;

        let text = std::fs::read_to_string(&path).map_err(|e| {
            JumabekError::ConfigError(format!("cannot read {}: {}", path.display(), e))
        })?;

        let mut config: Config = toml::from_str(&text).map_err(|e| {
            JumabekError::ConfigError(format!("invalid config at {}: {}", path.display(), e))
        })?;

        let base = path.parent().unwrap_or(Path::new("."));
        config.system_prompt_file = resolve_prompt_path(base, &config.llm.system_prompt_path);
        config.system_prompt = load_system_prompt(&config.system_prompt_file)?;
        config.api_key = secrets::resolve_api_key()?;

        Ok((config, path))
    }

    pub fn settings_for_skill(&self, name: &str) -> std::collections::BTreeMap<String, String> {
        let mut merged = self.skills.get(name).cloned().unwrap_or_default();

        if let Ok(Some(secrets)) = secrets::load()
            && let Some(section) = secrets.skills.get(name)
        {
            for (key, value) in section {
                merged.insert(key.clone(), value.clone());
            }
        }

        merged
    }

    pub fn db_path(&self) -> PathBuf {
        expand_tilde(&self.memory.db_path)
    }

    pub fn interface_mode(&self) -> JumabekResult<InterfaceMode> {
        match self.agent.interface.to_lowercase().as_str() {
            "cli" => Ok(InterfaceMode::Cli),
            "voice" => Ok(InterfaceMode::Voice),
            other => Err(JumabekError::ConfigError(format!(
                "unknown interface '{}', expected 'cli' or 'voice'",
                other
            ))),
        }
    }
}

fn resolve_prompt_path(base: &Path, raw: &str) -> PathBuf {
    let candidate = expand_tilde(raw);
    if candidate.is_absolute() {
        candidate
    } else {
        base.join(candidate)
    }
}

fn load_system_prompt(path: &Path) -> JumabekResult<String> {
    let text = std::fs::read_to_string(path).map_err(|e| {
        JumabekError::ConfigError(format!(
            "cannot read system prompt at {}: {}",
            path.display(),
            e
        ))
    })?;

    if text.trim().is_empty() {
        return Err(JumabekError::ConfigError(format!(
            "system prompt at {} is empty",
            path.display()
        )));
    }

    Ok(text)
}
