use std::time::Duration;

use reqwest::StatusCode;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};

use crate::configs::Config;
use crate::core::json_repair;
use crate::core::task::{AgentResponse, LlmMessage};
use crate::error::{JumabekError, JumabekResult};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(180);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

pub struct LlmClient {
    http: reqwest::Client,
    endpoint: String,
    model: String,
    api_key: String,
    max_retries: u32,
    initial_delay_ms: u64,
}

pub struct LlmReply {
    pub response: AgentResponse,
    pub raw_content: String,
}

impl LlmClient {
    pub fn new(config: &Config) -> JumabekResult<Self> {
        let http = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .connect_timeout(CONNECT_TIMEOUT)
            .build()
            .map_err(|e| JumabekError::InternalError(format!("cannot build http client: {}", e)))?;

        Ok(LlmClient {
            http,
            endpoint: format!(
                "{}/v1/chat/completions",
                config.llm.base_uri.trim_end_matches('/')
            ),
            model: config.llm.model.clone(),
            api_key: config.api_key.clone(),
            max_retries: config.llm.retry_max_retries.max(1),
            initial_delay_ms: config.llm.retry_initial_delay_ms,
        })
    }

    pub async fn ask(&self, messages: &[LlmMessage]) -> JumabekResult<LlmReply> {
        let content = self.request_content(messages).await?;
        let response = parse_agent_response(&content)?;
        Ok(LlmReply {
            response,
            raw_content: content,
        })
    }

    pub async fn complete(&self, system: &str, user: &str) -> JumabekResult<String> {
        let messages = vec![
            LlmMessage {
                role: "system".to_string(),
                content: system.to_string(),
            },
            LlmMessage {
                role: "user".to_string(),
                content: user.to_string(),
            },
        ];
        self.request_content(&messages).await
    }

    async fn request_content(&self, messages: &[LlmMessage]) -> JumabekResult<String> {
        let body = serde_json::json!({
            "model": self.model,
            "messages": messages,
            "stream": false,
            "thinking": { "type": "disabled" }
        });

        let mut last_error = JumabekError::LlmUnavailable("no attempt was made".to_string());

        for attempt in 0..self.max_retries {
            match self.attempt(&body).await {
                Ok(content) => return Ok(content),
                Err(AttemptError::Fatal(e)) => return Err(e),
                Err(AttemptError::Retryable(e)) => last_error = e,
            }

            if attempt + 1 < self.max_retries {
                let delay = Duration::from_millis(self.initial_delay_ms * (attempt as u64 + 1));
                tokio::time::sleep(delay).await;
            }
        }

        Err(last_error)
    }

    async fn attempt(&self, body: &serde_json::Value) -> Result<String, AttemptError> {
        let response = self
            .http
            .post(&self.endpoint)
            .header(AUTHORIZATION, format!("Bearer {}", self.api_key))
            .header(CONTENT_TYPE, "application/json")
            .json(body)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    AttemptError::Retryable(JumabekError::LlmTimeout(e.to_string()))
                } else {
                    AttemptError::Retryable(JumabekError::LlmUnavailable(format!(
                        "{} — is OmniRoute running at {}?",
                        e, self.endpoint
                    )))
                }
            })?;

        let status = response.status();
        let text = response.text().await.map_err(|e| {
            AttemptError::Retryable(JumabekError::LlmUnavailable(format!(
                "cannot read response body: {}",
                e
            )))
        })?;

        if !status.is_success() {
            return Err(classify_status(status, &text));
        }

        extract_content(&text).map_err(AttemptError::Fatal)
    }
}

enum AttemptError {
    Retryable(JumabekError),
    Fatal(JumabekError),
}

fn classify_status(status: StatusCode, body: &str) -> AttemptError {
    let detail = summarise_error_body(body);

    match status {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
            AttemptError::Fatal(JumabekError::LlmUnavailable(format!(
                "{} — check the API key (JUMABEK_API_KEY or secrets.toml): {}",
                status, detail
            )))
        }
        StatusCode::NOT_FOUND => AttemptError::Fatal(JumabekError::LlmUnavailable(format!(
            "{} — wrong base_uri or model: {}",
            status, detail
        ))),
        StatusCode::TOO_MANY_REQUESTS => AttemptError::Retryable(JumabekError::LlmUnavailable(
            format!("{} — rate limited: {}", status, detail),
        )),
        s if s.is_server_error() => AttemptError::Retryable(JumabekError::LlmUnavailable(format!(
            "{} — provider error: {}",
            status, detail
        ))),
        _ => AttemptError::Fatal(JumabekError::LlmUnavailable(format!(
            "{}: {}",
            status, detail
        ))),
    }
}

fn summarise_error_body(body: &str) -> String {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return "<empty body>".to_string();
    }

    if let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) {
        for path in [["error", "message"], ["message", ""], ["detail", ""]] {
            let found = if path[1].is_empty() {
                value.get(path[0]).and_then(|v| v.as_str())
            } else {
                value
                    .get(path[0])
                    .and_then(|v| v.get(path[1]))
                    .and_then(|v| v.as_str())
            };
            if let Some(text) = found {
                return text.to_string();
            }
        }
    }

    trimmed.chars().take(300).collect()
}

fn extract_content(body: &str) -> JumabekResult<String> {
    let raw: serde_json::Value = serde_json::from_str(body).map_err(|e| {
        JumabekError::LlmInvalidResponse(format!(
            "provider returned non-JSON: {} — body starts with: {}",
            e,
            body.chars().take(200).collect::<String>()
        ))
    })?;

    let choices = raw.get("choices").and_then(|c| c.as_array());
    let Some(choices) = choices else {
        return Err(JumabekError::LlmInvalidResponse(format!(
            "response has no 'choices' array: {}",
            body.chars().take(300).collect::<String>()
        )));
    };

    let Some(first) = choices.first() else {
        return Err(JumabekError::LlmInvalidResponse(
            "response contains an empty 'choices' array".to_string(),
        ));
    };

    let content = first
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .unwrap_or("");

    if content.trim().is_empty() {
        let finish_reason = first
            .get("finish_reason")
            .and_then(|f| f.as_str())
            .unwrap_or("unknown");
        return Err(JumabekError::LlmInvalidResponse(format!(
            "model returned empty content (finish_reason: {})",
            finish_reason
        )));
    }

    Ok(content.to_string())
}

pub fn parse_agent_response(content: &str) -> JumabekResult<AgentResponse> {
    let payload = json_repair::extract_json_payload(content);

    serde_json::from_str::<AgentResponse>(&payload).map_err(|e| {
        if json_repair::looks_truncated(content) {
            return JumabekError::ParseError(format!(
                "response looks truncated (unclosed JSON), the model probably hit its output limit: {}",
                e
            ));
        }

        JumabekError::ParseError(format!(
            "cannot read the answer as an agent response: {} — got: {}",
            e,
            payload.chars().take(400).collect::<String>()
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::task::ActionType;

    fn body_with(content: &str) -> String {
        serde_json::json!({
            "choices": [{ "message": { "role": "assistant", "content": content } }]
        })
        .to_string()
    }

    #[test]
    fn reads_content_out_of_envelope() {
        let body = body_with(r#"{"message":"ok","is_done":true,"actions":[]}"#);
        assert!(extract_content(&body).unwrap().contains("\"ok\""));
    }

    #[test]
    fn rejects_error_envelope_instead_of_returning_empty() {
        let body = r#"{"error":{"message":"invalid api key","type":"auth_error"}}"#;
        let err = extract_content(body).unwrap_err();
        assert!(matches!(err, JumabekError::LlmInvalidResponse(_)));
    }

    #[test]
    fn reports_empty_content_with_finish_reason() {
        let body = serde_json::json!({
            "choices": [{ "message": { "content": "" }, "finish_reason": "length" }]
        })
        .to_string();
        let err = extract_content(&body).unwrap_err().to_string();
        assert!(err.contains("length"), "got: {err}");
    }

    #[test]
    fn summarises_provider_error_message() {
        assert_eq!(
            summarise_error_body(r#"{"error":{"message":"model not found"}}"#),
            "model not found"
        );
        assert_eq!(summarise_error_body("   "), "<empty body>");
    }

    #[test]
    fn auth_failure_is_fatal_but_rate_limit_retries() {
        assert!(matches!(
            classify_status(StatusCode::UNAUTHORIZED, "{}"),
            AttemptError::Fatal(_)
        ));
        assert!(matches!(
            classify_status(StatusCode::TOO_MANY_REQUESTS, "{}"),
            AttemptError::Retryable(_)
        ));
        assert!(matches!(
            classify_status(StatusCode::INTERNAL_SERVER_ERROR, "{}"),
            AttemptError::Retryable(_)
        ));
    }

    #[test]
    fn parses_response_wrapped_in_markdown() {
        let content = "```json\n{\"message\":\"готово\",\"is_done\":true,\"actions\":[]}\n```";
        let parsed = parse_agent_response(content).unwrap();
        assert_eq!(parsed.message, "готово");
        assert!(parsed.is_done);
    }

    #[test]
    fn fills_missing_fields_with_defaults() {
        let parsed = parse_agent_response(r#"{"message":"hi"}"#).unwrap();
        assert!(!parsed.is_done);
        assert!(parsed.actions.is_empty());
    }

    #[test]
    fn accepts_action_aliases() {
        let content = r#"{"message":"","actions":[
            {"type":"PromptUser","message":"which one?","options":[]},
            {"type":"Respond"}
        ]}"#;
        let parsed = parse_agent_response(content).unwrap();
        assert!(matches!(parsed.actions[0], ActionType::PromptToUser { .. }));
        assert!(matches!(parsed.actions[1], ActionType::RespondToUser));
    }

    #[test]
    fn spawn_agent_is_recognised_under_its_aliases() {
        for name in ["SpawnAgent", "Spawn", "SubAgent", "SpawnSubAgent"] {
            let content = format!(
                r#"{{"actions":[{{"type":"{}","task":"read the logs","reason":"long"}}]}}"#,
                name
            );
            let parsed = parse_agent_response(&content).unwrap();
            match &parsed.actions[0] {
                ActionType::SpawnAgent { task, reason } => {
                    assert_eq!(task, "read the logs");
                    assert_eq!(reason, "long");
                }
                other => panic!("{} did not parse as a spawn: {:?}", name, other),
            }
        }
    }

    #[test]
    fn request_data_limit_defaults() {
        let content = r#"{"actions":[{"type":"RequestData","source":"memory","query":"doc"}]}"#;
        let parsed = parse_agent_response(content).unwrap();
        match &parsed.actions[0] {
            ActionType::RequestData { limit, .. } => assert_eq!(*limit, 5),
            other => panic!("unexpected action: {:?}", other),
        }
    }

    #[test]
    fn coerces_non_string_fields_the_model_gets_wrong() {
        let content = r#"{"message":"ok","actions":[
            {"type":"ExecuteModule","module":"slowpoke","method":"sleep","args":1},
            {"type":"ExecuteModule","module":"shell","method":"run","args":true},
            {"type":"ExecuteModule","module":"shell","method":"run","args":{"path":"/tmp"}}
        ]}"#;
        let parsed = parse_agent_response(content).unwrap();

        let args: Vec<&str> = parsed
            .actions
            .iter()
            .map(|a| match a {
                ActionType::ExecuteModule { args, .. } => args.as_str(),
                _ => panic!("unexpected action"),
            })
            .collect();

        assert_eq!(args, vec!["1", "true", r#"{"path":"/tmp"}"#]);
    }

    #[test]
    fn odd_shapes_in_lists_do_not_kill_the_turn() {
        let content = r#"{"actions":[
            {"type":"PermissionRequest","action":"x","description":"y","risk_level":"low",
             "options":[{"label":"Allow","value":"allow"}]},
            {"type":"GenerateChunk","module_name":"m","chunk_index":1,"total_chunks":1,
             "code_chunk":"fn main(){}","dependencies":[{"name":"regex","version":"1"}]}
        ]}"#;
        let parsed = parse_agent_response(content).unwrap();
        assert_eq!(parsed.actions.len(), 2);
    }

    #[test]
    fn a_numeric_message_does_not_kill_the_turn() {
        let parsed = parse_agent_response(r#"{"message":42,"is_done":true}"#).unwrap();
        assert_eq!(parsed.message, "42");
    }

    #[test]
    fn truncated_response_says_so() {
        let err = parse_agent_response(r#"{"message":"half a sen"#)
            .unwrap_err()
            .to_string();
        assert!(err.contains("truncated"), "got: {err}");
    }

    #[test]
    fn unknown_action_type_names_the_variants() {
        let err = parse_agent_response(r#"{"actions":[{"type":"Teleport"}]}"#)
            .unwrap_err()
            .to_string();
        assert!(err.contains("ExecuteModule"), "got: {err}");
    }
}
