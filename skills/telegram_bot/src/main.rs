use std::sync::Arc;

use jumabek_sdk::{MethodInfo, ModuleMetadata, SkillError, SkillModule, SkillOutput};
use tokio::sync::Mutex;

const API: &str = "https://api.telegram.org";
const POLL_SECONDS: u64 = 25;
const REPLY_LIMIT: usize = 4000;

struct Settings {
    token: String,
    allowed: Vec<i64>,
}

impl Settings {
    fn from_env() -> Result<Settings, String> {
        let token = std::env::var("JUMABEK_SKILL_BOT_TOKEN")
            .ok()
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .ok_or_else(|| {
                "bot_token is not set. Get one from @BotFather and put it under \
                 [skills.telegram_bot] in secrets.toml"
                    .to_string()
            })?;

        let allowed: Vec<i64> = std::env::var("JUMABEK_SKILL_ALLOWED_CHATS")
            .unwrap_or_default()
            .split(',')
            .filter_map(|id| id.trim().parse::<i64>().ok())
            .collect();

        Ok(Settings { token, allowed })
    }
}

/// The inbox is how an answer is produced: the bot does not think, it carries.
struct Door {
    url: String,
    token: String,
}

impl Door {
    fn from_env() -> Option<Door> {
        let token = std::env::var("JUMABEK_SKILL_INBOX_TOKEN").ok()?;
        let token = token.trim().to_string();
        if token.is_empty() {
            return None;
        }

        let port = std::env::var("JUMABEK_SKILL_INBOX_PORT")
            .ok()
            .and_then(|p| p.trim().parse::<u16>().ok())
            .unwrap_or(20129);

        Some(Door {
            url: format!("http://127.0.0.1:{}/ask", port),
            token,
        })
    }

    async fn ask(&self, who: &str, text: &str) -> Result<String, String> {
        let body = serde_json::json!({
            "source": "telegram_bot",
            "kind": "ask",
            "who": who,
            "text": text,
        });

        let response = reqwest::Client::new()
            .post(&self.url)
            .bearer_auth(&self.token)
            .json(&body)
            .timeout(std::time::Duration::from_secs(600))
            .send()
            .await
            .map_err(|e| e.to_string())?;

        let status = response.status();
        let payload: serde_json::Value = response
            .json()
            .await
            .map_err(|e| format!("{} — unreadable answer: {}", status, e))?;

        if !status.is_success() {
            return Err(payload
                .get("error")
                .and_then(|e| e.as_str())
                .unwrap_or("the inbox refused it")
                .to_string());
        }

        Ok(payload
            .get("reply")
            .and_then(|r| r.as_str())
            .unwrap_or("")
            .to_string())
    }
}

#[derive(Default)]
struct State {
    offset: i64,
    serving: bool,
}

struct TelegramBot {
    metadata: ModuleMetadata,
    settings: Result<Settings, String>,
    state: Arc<Mutex<State>>,
    http: reqwest::Client,
}

impl TelegramBot {
    fn new() -> Self {
        TelegramBot {
            metadata: ModuleMetadata {
                name: "telegram_bot".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                description: "A Telegram bot the user talks to instead of sitting at the \
                              terminal. Messages are relayed to JumaBek and the answer is sent \
                              back to the chat. Needs bot_token and allowed_chats under \
                              [skills.telegram_bot] in secrets.toml."
                    .to_string(),
            },
            settings: Settings::from_env(),
            state: Arc::new(Mutex::new(State::default())),
            http: reqwest::Client::new(),
        }
    }

    fn settings(&self) -> Result<&Settings, SkillError> {
        self.settings
            .as_ref()
            .map_err(|e| SkillError::Fatal(e.clone()))
    }

    async fn call(
        client: &reqwest::Client,
        token: &str,
        method: &str,
        body: serde_json::Value,
        timeout: u64,
    ) -> Result<serde_json::Value, String> {
        let response = client
            .post(format!("{}/bot{}/{}", API, token, method))
            .json(&body)
            .timeout(std::time::Duration::from_secs(timeout))
            .send()
            .await
            .map_err(|e| e.to_string())?;

        let payload: serde_json::Value = response.json().await.map_err(|e| e.to_string())?;

        if payload.get("ok").and_then(|o| o.as_bool()) == Some(true) {
            Ok(payload.get("result").cloned().unwrap_or_default())
        } else {
            Err(payload
                .get("description")
                .and_then(|d| d.as_str())
                .unwrap_or("telegram refused the call")
                .to_string())
        }
    }

    async fn send(&self, chat: i64, text: &str) -> Result<(), SkillError> {
        let token = &self.settings()?.token;

        for part in split_for_telegram(text) {
            Self::call(
                &self.http,
                token,
                "sendMessage",
                serde_json::json!({ "chat_id": chat, "text": part }),
                30,
            )
            .await
            .map_err(SkillError::ExecutionFailed)?;
        }

        Ok(())
    }

    async fn status(&self) -> Result<SkillOutput, SkillError> {
        let settings = self.settings()?;
        let me = Self::call(
            &self.http,
            &settings.token,
            "getMe",
            serde_json::json!({}),
            30,
        )
        .await
        .map_err(SkillError::ExecutionFailed)?;

        let serving = self.state.lock().await.serving;

        Ok(SkillOutput::Text(format!(
            "Connected as @{} ({}). Allowed chats: {}. {}",
            me.get("username").and_then(|u| u.as_str()).unwrap_or("?"),
            me.get("id").and_then(|i| i.as_i64()).unwrap_or(0),
            if settings.allowed.is_empty() {
                "none — nobody can talk to it yet".to_string()
            } else {
                settings
                    .allowed
                    .iter()
                    .map(|id| id.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            },
            if serving {
                "Serving: messages reach JumaBek and answers go back."
            } else {
                "Not serving yet — call serve."
            }
        )))
    }

    async fn serve(&self) -> Result<SkillOutput, SkillError> {
        let settings = self.settings()?;

        if settings.allowed.is_empty() {
            return Err(SkillError::InvalidArgs(
                "allowed_chats is empty, so the bot would answer strangers. Add your own chat \
                 id under [skills.telegram_bot] in secrets.toml: allowed_chats = \"123456789\". \
                 Send the bot a message and call whoami to find it."
                    .to_string(),
            ));
        }

        let Some(door) = Door::from_env() else {
            return Err(SkillError::InvalidArgs(
                "no inbox token, so there is nothing to relay to. Ask the user for an inbox key \
                 for this skill."
                    .to_string(),
            ));
        };

        {
            let mut state = self.state.lock().await;
            if state.serving {
                return Ok(SkillOutput::Text("Already serving.".to_string()));
            }
            state.serving = true;
        }

        let token = settings.token.clone();
        let allowed = settings.allowed.clone();
        let state = Arc::clone(&self.state);
        let http = self.http.clone();

        tokio::spawn(async move {
            loop {
                let offset = state.lock().await.offset;

                let updates = match Self::call(
                    &http,
                    &token,
                    "getUpdates",
                    serde_json::json!({
                        "offset": offset,
                        "timeout": POLL_SECONDS,
                        "allowed_updates": ["message"],
                    }),
                    POLL_SECONDS + 15,
                )
                .await
                {
                    Ok(updates) => updates,
                    Err(e) => {
                        eprintln!("[telegram_bot] getUpdates: {}", e);
                        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                        continue;
                    }
                };

                for update in updates.as_array().unwrap_or(&Vec::new()) {
                    if let Some(id) = update.get("update_id").and_then(|i| i.as_i64()) {
                        state.lock().await.offset = id + 1;
                    }

                    let Some(message) = update.get("message") else {
                        continue;
                    };
                    let Some(chat) = message
                        .get("chat")
                        .and_then(|c| c.get("id"))
                        .and_then(|i| i.as_i64())
                    else {
                        continue;
                    };
                    let Some(text) = message.get("text").and_then(|t| t.as_str()) else {
                        continue;
                    };

                    if !allowed.contains(&chat) {
                        eprintln!("[telegram_bot] ignored a message from chat {}", chat);
                        continue;
                    }

                    let who = message
                        .get("from")
                        .and_then(|f| f.get("username"))
                        .and_then(|u| u.as_str())
                        .unwrap_or("owner")
                        .to_string();

                    let _ = Self::call(
                        &http,
                        &token,
                        "sendChatAction",
                        serde_json::json!({ "chat_id": chat, "action": "typing" }),
                        15,
                    )
                    .await;

                    let reply = match door.ask(&who, text).await {
                        Ok(reply) if !reply.trim().is_empty() => reply,
                        Ok(_) => "Готово.".to_string(),
                        Err(e) => {
                            eprintln!("[telegram_bot] the inbox: {}", e);
                            format!("Не смог обработать: {}", e)
                        }
                    };

                    for part in split_for_telegram(&reply) {
                        let _ = Self::call(
                            &http,
                            &token,
                            "sendMessage",
                            serde_json::json!({ "chat_id": chat, "text": part }),
                            30,
                        )
                        .await;
                    }
                }
            }
        });

        Ok(SkillOutput::Text(
            "Serving. The user can now write to the bot from anywhere and the answer comes \
             back in that chat. Messages from any chat that is not allowed are ignored."
                .to_string(),
        ))
    }

    async fn whoami(&self) -> Result<SkillOutput, SkillError> {
        let settings = self.settings()?;

        let updates = Self::call(
            &self.http,
            &settings.token,
            "getUpdates",
            serde_json::json!({ "timeout": 0, "limit": 10 }),
            30,
        )
        .await
        .map_err(SkillError::ExecutionFailed)?;

        let mut seen: Vec<String> = Vec::new();
        for update in updates.as_array().unwrap_or(&Vec::new()) {
            let Some(chat) = update.get("message").and_then(|m| m.get("chat")) else {
                continue;
            };
            let id = chat.get("id").and_then(|i| i.as_i64()).unwrap_or(0);
            let name = chat
                .get("username")
                .or_else(|| chat.get("first_name"))
                .and_then(|n| n.as_str())
                .unwrap_or("?");

            let line = format!("{} — chat id {}", name, id);
            if !seen.contains(&line) {
                seen.push(line);
            }
        }

        if seen.is_empty() {
            return Ok(SkillOutput::Text(
                "Nobody has written to the bot yet. Ask the user to send it any message, then \
                 call whoami again."
                    .to_string(),
            ));
        }

        Ok(SkillOutput::Text(format!(
            "Recent chats:\n{}\n\nPut the right one in allowed_chats under \
             [skills.telegram_bot] in secrets.toml.",
            seen.join("\n")
        )))
    }

    fn config(&self) -> SkillOutput {
        match &self.settings {
            Ok(settings) => SkillOutput::Text(format!(
                "Token present. Allowed chats: {}. Inbox token: {}.",
                if settings.allowed.is_empty() {
                    "none — serve will refuse until one is set".to_string()
                } else {
                    settings.allowed.len().to_string()
                },
                if Door::from_env().is_some() {
                    "present"
                } else {
                    "missing — nothing to relay to"
                }
            )),
            Err(problem) => SkillOutput::Text(format!("Not configured: {}", problem)),
        }
    }
}

fn split_for_telegram(text: &str) -> Vec<String> {
    let text = text.trim();
    if text.is_empty() {
        return vec!["(пусто)".to_string()];
    }

    let mut parts = Vec::new();
    let mut current = String::new();

    for line in text.lines() {
        if current.chars().count() + line.chars().count() + 1 > REPLY_LIMIT && !current.is_empty() {
            parts.push(std::mem::take(&mut current));
        }

        if line.chars().count() > REPLY_LIMIT {
            let mut rest: Vec<char> = line.chars().collect();
            while rest.len() > REPLY_LIMIT {
                let tail = rest.split_off(REPLY_LIMIT);
                parts.push(rest.into_iter().collect());
                rest = tail;
            }
            current = rest.into_iter().collect();
            continue;
        }

        if !current.is_empty() {
            current.push('\n');
        }
        current.push_str(line);
    }

    if !current.is_empty() {
        parts.push(current);
    }

    parts
}

fn split(args: &str) -> (String, String) {
    match args.split_once('|') {
        Some((left, right)) => (left.trim().to_string(), right.trim().to_string()),
        None => (args.trim().to_string(), String::new()),
    }
}

#[async_trait::async_trait]
impl SkillModule for TelegramBot {
    fn get_metadata(&self) -> &ModuleMetadata {
        &self.metadata
    }

    fn health_check(&self) -> bool {
        self.settings.is_ok()
    }

    async fn execute(&self, method: &str, args: &str) -> Result<SkillOutput, SkillError> {
        match method {
            "config" => Ok(self.config()),
            "status" => self.status().await,
            "whoami" => self.whoami().await,
            "serve" => self.serve().await,
            "send" => {
                let (chat, text) = split(args);
                let chat: i64 = chat
                    .parse()
                    .map_err(|_| SkillError::InvalidArgs("expected: chat_id | text".to_string()))?;
                if text.is_empty() {
                    return Err(SkillError::InvalidArgs("nothing to send".to_string()));
                }
                self.send(chat, &text).await?;
                Ok(SkillOutput::Text(format!("Sent to {}.", chat)))
            }
            other => Err(SkillError::NotFound(format!(
                "no method '{}'. Available: config, status, whoami, serve, send",
                other
            ))),
        }
    }

    fn available_methods(&self) -> Vec<MethodInfo> {
        vec![
            MethodInfo {
                method: "config".to_string(),
                description: "Check whether the bot token, the allowed chats and the inbox key \
                              are in place. Call this first if anything complains."
                    .to_string(),
                args_description: "Nothing. Pass an empty string.".to_string(),
            },
            MethodInfo {
                method: "status".to_string(),
                description: "Report which bot this is, who may talk to it, and whether it is \
                              already relaying messages."
                    .to_string(),
                args_description: "Nothing. Pass an empty string.".to_string(),
            },
            MethodInfo {
                method: "whoami".to_string(),
                description: "List the chats that recently wrote to the bot, with their ids. \
                              Use it to find the user's own chat id for allowed_chats."
                    .to_string(),
                args_description: "Nothing. Pass an empty string.".to_string(),
            },
            MethodInfo {
                method: "serve".to_string(),
                description: "Start relaying: a message to the bot becomes a request to you, \
                              and your answer is sent back to that chat. Call it once; it \
                              keeps running until the skill stops. Refuses if allowed_chats is \
                              empty, because a public bot would otherwise answer strangers."
                    .to_string(),
                args_description: "Nothing. Pass an empty string.".to_string(),
            },
            MethodInfo {
                method: "send".to_string(),
                description: "Send a message to a chat without being asked — a reminder, or \
                              the result of something that finished."
                    .to_string(),
                args_description: "chat_id | text. Example: 123456789 | сборка готова".to_string(),
            },
        ]
    }
}

#[tokio::main]
async fn main() {
    if let Err(e) = jumabek_sdk::runtime::run_skill(TelegramBot::new()).await {
        eprintln!("telegram_bot stopped: {}", e);
        std::process::exit(1);
    }
}
