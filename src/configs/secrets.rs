use serde::{Deserialize, Serialize};

use crate::configs::find_file;
use crate::error::{JumabekError, JumabekResult};

pub const ENV_API_KEY: &str = "JUMABEK_API_KEY";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Secrets {
    /// Optional in full: a secrets file that only carries inbox tokens, or an
    /// endpoint that wants no key at all, has nothing to put here.
    #[serde(default)]
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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LlmSecrets {
    #[serde(default)]
    pub api_key: Option<String>,
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

/// The key to send, or an empty string when this endpoint does not want one.
///
/// A missing key used to be fatal at startup, which made "point it at a local
/// model" a two-step job: change `base_uri`, then invent a key the server will
/// throw away. Ollama, LM Studio and llama.cpp all ignore the header entirely.
///
/// So an absent key is now a statement rather than a mistake, and the check
/// moves rather than disappears: `jumabek doctor` says out loud that nothing is
/// configured, and an endpoint that did want a key answers 401 with a message
/// naming exactly where to put one.
pub fn resolve_api_key() -> JumabekResult<String> {
    if let Ok(key) = std::env::var(ENV_API_KEY) {
        let key = key.trim().to_string();
        if !key.is_empty() {
            return Ok(key);
        }
    }

    Ok(load()?
        .and_then(|secrets| secrets.llm.api_key)
        .map(|key| key.trim().to_string())
        .unwrap_or_default())
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

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Secrets {
        toml::from_str(text).expect("secrets should parse")
    }

    #[test]
    fn a_secrets_file_without_a_key_is_a_statement_not_a_syntax_error() {
        assert_eq!(parse("[llm]\n").llm.api_key, None);
        assert_eq!(parse("[inbox.tokens]\n").llm.api_key, None);
        assert_eq!(parse("").llm.api_key, None);
    }

    #[test]
    fn a_key_that_is_there_is_still_read() {
        assert_eq!(
            parse("[llm]\napi_key = \"sk-abc\"\n")
                .llm
                .api_key
                .as_deref(),
            Some("sk-abc")
        );
    }

    #[test]
    fn inbox_tokens_survive_a_file_with_no_llm_section() {
        let secrets = parse("[inbox.tokens]\ntelegram = \"0123456789012345678901234\"\n");
        assert!(secrets.inbox.unwrap().tokens.contains_key("telegram"));
    }
}
