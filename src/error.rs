use std::fmt;

use jumabek_sdk::SkillError;

#[derive(Debug, Clone)]
pub enum JumabekError {
    LlmUnavailable(String),
    LlmTimeout(String),
    LlmInvalidResponse(String),

    ParseError(String),
    SkillError(jumabek_sdk::SkillError),
    DatabaseError(String),
    ConfigError(String),
    IoError(String),
    ReadlineError(String),
    InternalError(String),
}

impl JumabekError {
    pub fn is_recoverable(&self) -> bool {
        match self {
            JumabekError::SkillError(e) => !matches!(e, jumabek_sdk::SkillError::Fatal(_)),
            JumabekError::ParseError(_) => true,
            _ => false,
        }
    }
}

impl fmt::Display for JumabekError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            JumabekError::LlmUnavailable(msg) => write!(f, "[LLM ERROR] Unavailable: {}", msg),
            JumabekError::LlmTimeout(msg) => write!(f, "[LLM ERROR] Timeout: {}", msg),
            JumabekError::LlmInvalidResponse(msg) => write!(
                f,
                "[LLM ERROR] Invalid response format of JumaBek(should to remember message format from system prompt): {}",
                msg
            ),
            JumabekError::ParseError(msg) => write!(f, "[PARSE ERROR] {}", msg),
            JumabekError::SkillError(msg) => write!(f, "[SKILL ERROR] {}", msg),
            JumabekError::DatabaseError(msg) => write!(f, "[DB ERROR] {}", msg),
            JumabekError::ConfigError(msg) => write!(f, "[CONFIG ERROR] {}", msg),
            JumabekError::IoError(msg) => write!(f, "[IO ERROR] {}", msg),
            JumabekError::ReadlineError(msg) => write!(f, "[READLINE ERROR] {}", msg),
            JumabekError::InternalError(msg) => write!(f, "[INTERNAL ERROR] {}", msg),
        }
    }
}

impl std::error::Error for JumabekError {}

impl From<serde_json::Error> for JumabekError {
    fn from(value: serde_json::Error) -> Self {
        JumabekError::ParseError(value.to_string())
    }
}

impl From<toml::de::Error> for JumabekError {
    fn from(value: toml::de::Error) -> Self {
        JumabekError::ParseError(value.to_string())
    }
}

impl From<reqwest::Error> for JumabekError {
    fn from(value: reqwest::Error) -> Self {
        if value.is_timeout() {
            JumabekError::LlmTimeout(value.to_string())
        } else {
            JumabekError::LlmUnavailable(value.to_string())
        }
    }
}

impl From<std::io::Error> for JumabekError {
    fn from(value: std::io::Error) -> Self {
        JumabekError::IoError(value.to_string())
    }
}

impl From<rusqlite::Error> for JumabekError {
    fn from(value: rusqlite::Error) -> Self {
        JumabekError::DatabaseError(value.to_string())
    }
}

impl From<SkillError> for JumabekError {
    fn from(value: SkillError) -> Self {
        JumabekError::SkillError(value)
    }
}

impl From<rustyline::error::ReadlineError> for JumabekError {
    fn from(value: rustyline::error::ReadlineError) -> Self {
        JumabekError::ReadlineError(value.to_string())
    }
}

pub type JumabekResult<T> = Result<T, JumabekError>;
