use std::time::Duration;

use reqwest::multipart::{Form, Part};

use crate::error::{JumabekError, JumabekResult};
use crate::voice::wav;

const GROQ_ENDPOINT: &str = "https://api.groq.com/openai/v1/audio/transcriptions";
const MODEL: &str = "whisper-large-v3";
const TIMEOUT: Duration = Duration::from_secs(60);

pub struct Stt {
    http: reqwest::Client,
    api_key: String,
    language: Option<String>,
}

impl Stt {
    pub fn new(api_key: impl Into<String>, language: Option<String>) -> JumabekResult<Self> {
        let http = reqwest::Client::builder()
            .timeout(TIMEOUT)
            .build()
            .map_err(|e| JumabekError::InternalError(format!("cannot build http client: {}", e)))?;

        Ok(Stt {
            http,
            api_key: api_key.into(),
            language,
        })
    }

    pub async fn transcribe(&self, samples: &[i16]) -> JumabekResult<String> {
        let audio = wav::pcm_to_wav(samples);

        let part = Part::bytes(audio)
            .file_name("speech.wav")
            .mime_str("audio/wav")
            .map_err(|e| JumabekError::InternalError(format!("bad audio part: {}", e)))?;

        let mut form = Form::new()
            .part("file", part)
            .text("model", MODEL)
            .text("response_format", "json");

        if let Some(language) = &self.language {
            form = form.text("language", language.clone());
        }

        let response = self
            .http
            .post(GROQ_ENDPOINT)
            .bearer_auth(&self.api_key)
            .multipart(form)
            .send()
            .await
            .map_err(|e| {
                JumabekError::InternalError(format!("speech recognition request failed: {}", e))
            })?;

        let status = response.status();
        let body = response.text().await.map_err(|e| {
            JumabekError::InternalError(format!("cannot read recognition response: {}", e))
        })?;

        parse_transcription(status.as_u16(), &body)
    }
}

fn parse_transcription(status: u16, body: &str) -> JumabekResult<String> {
    let value: serde_json::Value = serde_json::from_str(body).map_err(|e| {
        JumabekError::InternalError(format!(
            "speech recognition returned non-JSON ({}): {} — {}",
            status,
            e,
            body.chars().take(200).collect::<String>()
        ))
    })?;

    if let Some(message) = value
        .get("error")
        .and_then(|e| e.get("message"))
        .and_then(|m| m.as_str())
    {
        return Err(JumabekError::InternalError(format!(
            "speech recognition failed ({}): {}",
            status, message
        )));
    }

    if status == 401 || status == 403 {
        return Err(JumabekError::ConfigError(
            "speech recognition rejected the Groq key — check [voice].groq_api_key".to_string(),
        ));
    }

    match value.get("text").and_then(|t| t.as_str()) {
        Some(text) => {
            let text = text.trim();
            if is_hallucination(text) {
                return Ok(String::new());
            }
            Ok(text.to_string())
        }
        None => Err(JumabekError::InternalError(format!(
            "speech recognition response has no 'text' field: {}",
            body.chars().take(200).collect::<String>()
        ))),
    }
}

const HALLUCINATIONS_EXACT: &[&str] = &[
    "bye",
    "bye bye",
    "you",
    "thank you",
    "thanks for watching",
    "subscribe",
    "спасибо",
    "спасибо за просмотр",
    "спасибо за внимание",
    "продолжение следует",
];

const HALLUCINATIONS_PREFIX: &[&str] = &[
    "субтитры сделал",
    "субтитры делал",
    "редактор субтитров",
    "подписывайтесь на канал",
    "продолжение следует",
];

fn is_hallucination(text: &str) -> bool {
    let cleaned: String = text
        .to_lowercase()
        .chars()
        .filter(|c| !matches!(c, '.' | ',' | '!' | '?' | '…' | '"' | '\'' | '-'))
        .collect();
    let cleaned = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");

    if cleaned.is_empty() {
        return true;
    }

    HALLUCINATIONS_EXACT.iter().any(|p| cleaned == *p)
        || HALLUCINATIONS_PREFIX.iter().any(|p| cleaned.starts_with(p))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_transcript() {
        let body = r#"{"text":"  открой файл  "}"#;
        assert_eq!(parse_transcription(200, body).unwrap(), "открой файл");
    }

    #[test]
    fn surfaces_the_provider_error() {
        let body = r#"{"error":{"message":"Invalid API Key","type":"invalid_request_error"}}"#;
        let err = parse_transcription(401, body).unwrap_err().to_string();
        assert!(err.contains("Invalid API Key"), "got: {err}");
    }

    #[test]
    fn names_the_config_key_on_bare_auth_failure() {
        let err = parse_transcription(401, "{}").unwrap_err().to_string();
        assert!(err.contains("groq_api_key"), "got: {err}");
    }

    #[test]
    fn rejects_html_error_pages() {
        let err = parse_transcription(502, "<html>bad gateway</html>")
            .unwrap_err()
            .to_string();
        assert!(err.contains("non-JSON"), "got: {err}");
    }

    #[test]
    fn drops_whisper_hallucinations_from_silence() {
        for text in [
            "Bye",
            "bye.",
            "Thank you.",
            "you",
            "Спасибо за просмотр!",
            "Продолжение следует...",
            "Субтитры сделал DimaTorzok",
            "   ",
            ".",
        ] {
            let body = serde_json::json!({ "text": text }).to_string();
            assert_eq!(
                parse_transcription(200, &body).unwrap(),
                "",
                "did not filter: {text}"
            );
        }
    }

    #[test]
    fn keeps_real_speech_that_merely_looks_short() {
        for text in [
            "да",
            "открой файл",
            "bye bye baby, найди песню",
            "спасибо, теперь удали его",
        ] {
            let body = serde_json::json!({ "text": text }).to_string();
            assert_eq!(
                parse_transcription(200, &body).unwrap(),
                text,
                "wrongly filtered: {text}"
            );
        }
    }

    #[test]
    fn rejects_a_response_without_text() {
        let err = parse_transcription(200, r#"{"duration":1.5}"#)
            .unwrap_err()
            .to_string();
        assert!(err.contains("no 'text' field"), "got: {err}");
    }
}
