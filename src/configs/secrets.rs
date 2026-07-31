use serde::{Deserialize, Serialize};

use crate::configs::find_file;
use crate::error::{JumabekError, JumabekResult};

pub const ENV_API_KEY: &str = "JUMABEK_API_KEY";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Secrets {
    pub llm: LlmSecrets,
    #[serde(default)]
    pub voice: Option<VoiceSecrets>,
    #[serde(default)]
    pub inbox: Option<InboxSecrets>,
    #[serde(default)]
    pub skills: std::collections::BTreeMap<String, std::collections::BTreeMap<String, String>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct InboxSecrets {
    #[serde(default)]
    pub tokens: std::collections::BTreeMap<String, String>,
}

pub fn inbox_tokens() -> std::collections::BTreeMap<String, String> {
    load()
        .ok()
        .flatten()
        .and_then(|s| s.inbox)
        .map(|inbox| inbox.tokens)
        .unwrap_or_default()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmSecrets {
    pub api_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoiceSecrets {
    pub groq_api_key: String,
}

pub fn load() -> JumabekResult<Option<Secrets>> {
    let Ok(path) = find_file("secrets.toml") else {
        return Ok(None);
    };

    warn_if_world_readable(&path);

    let text = std::fs::read_to_string(&path)
        .map_err(|e| JumabekError::ConfigError(format!("cannot read {}: {}", path.display(), e)))?;

    let secrets: Secrets = toml::from_str(&text).map_err(|e| {
        JumabekError::ConfigError(format!("invalid secrets at {}: {}", path.display(), e))
    })?;

    Ok(Some(secrets))
}

pub fn groq_api_key() -> JumabekResult<Option<String>> {
    if let Ok(key) = std::env::var("JUMABEK_GROQ_API_KEY") {
        let key = key.trim().to_string();
        if !key.is_empty() {
            return Ok(Some(key));
        }
    }

    Ok(load()?
        .and_then(|s| s.voice)
        .map(|v| v.groq_api_key.trim().to_string())
        .filter(|k| !k.is_empty()))
}

pub fn resolve_api_key() -> JumabekResult<String> {
    if let Ok(key) = std::env::var(ENV_API_KEY) {
        let key = key.trim().to_string();
        if !key.is_empty() {
            return Ok(key);
        }
    }

    if let Some(secrets) = load()? {
        let key = secrets.llm.api_key.trim();
        if !key.is_empty() {
            return Ok(key.to_string());
        }
    }

    Err(JumabekError::ConfigError(format!(
        "no API key: set {}, or copy secrets.toml.example to secrets.toml and fill [llm].api_key",
        ENV_API_KEY
    )))
}

#[cfg(unix)]
fn warn_if_world_readable(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;

    if let Ok(meta) = std::fs::metadata(path) {
        let mode = meta.permissions().mode();
        if mode & 0o077 != 0 {
            eprintln!(
                "[configs] warning: {} is readable by other users (mode {:o}); run: chmod 600 {}",
                path.display(),
                mode & 0o777,
                path.display()
            );
        }
    }
}

#[cfg(not(unix))]
fn warn_if_world_readable(_path: &std::path::Path) {}
