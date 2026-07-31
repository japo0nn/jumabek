use std::collections::HashMap;
use std::sync::Arc;

use ferogram::{Client, InputMessage, PasswordToken, SendCodeOutcome, SignInError, TransportKind};
use jumabek_sdk::{MethodInfo, ModuleMetadata, SkillError, SkillModule, SkillOutput};
use tokio::sync::Mutex;

const INBOX_LIMIT: usize = 500;

type Names = Arc<Mutex<HashMap<i64, String>>>;

async fn name_of(client: &Client, names: &Names, id: i64) -> String {
    if let Some(known) = names.lock().await.get(&id) {
        return known.clone();
    }

    let resolved = match client.get_users_by_id(&[id]).await {
        Ok(users) => users.into_iter().flatten().next().map(|user| {
            let full = [user.first_name().unwrap_or(""), user.last_name().unwrap_or("")]
                .join(" ")
                .trim()
                .to_string();

            match (full.is_empty(), user.username()) {
                (false, Some(handle)) => format!("{} (@{})", full, handle),
                (false, None) => full,
                (true, Some(handle)) => format!("@{}", handle),
                (true, None) => format!("user {}", id),
            }
        }),
        Err(_) => None,
    }
    .unwrap_or_else(|| format!("user {}", id));

    names.lock().await.insert(id, resolved.clone());
    resolved
}

struct Settings {
    api_id: i32,
    api_hash: String,
    phone: String,
    session: String,
}

impl Settings {
    fn from_env() -> Result<Settings, String> {
        let api_id = env("API_ID")?;
        let api_id: i32 = api_id
            .parse()
            .map_err(|_| format!("api_id must be a number, got '{}'", api_id))?;

        Ok(Settings {
            api_id,
            api_hash: env("API_HASH")?,
            phone: env("PHONE_NUMBER")?,
            session: std::env::var("JUMABEK_SKILL_SESSION_PATH")
                .unwrap_or_else(|_| default_session()),
        })
    }
}

fn env(key: &str) -> Result<String, String> {
    let name = format!("JUMABEK_SKILL_{}", key);
    match std::env::var(&name) {
        Ok(value) if !value.trim().is_empty() => Ok(value.trim().to_string()),
        _ => Err(format!(
            "{} is not set. Put it under [skills.telegram] in secrets.toml",
            key.to_lowercase()
        )),
    }
}

fn default_session() -> String {
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .unwrap_or_else(|_| ".".to_string());
    format!("{}/.jumabek/telegram.session", home)
}

#[derive(Default)]
struct State {
    client: Option<Client>,
    shutdown: Option<ferogram::ShutdownToken>,
    login: Option<ferogram::LoginToken>,
    password: Option<PasswordToken>,
    watching: bool,
}

struct Telegram {
    metadata: ModuleMetadata,
    settings: Result<Settings, String>,
    state: Mutex<State>,
    inbox: Arc<Mutex<Vec<String>>>,
    dropped: Arc<Mutex<usize>>,
    names: Names,
}

impl Telegram {
    fn new() -> Self {
        Telegram {
            metadata: ModuleMetadata {
                name: "telegram".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                description: "Your own Telegram account over MTProto, not the Bot API: list \
                              chats, read history, send messages. Needs api_id, api_hash and \
                              phone_number under [skills.telegram] in secrets.toml, and a \
                              one-time login by code."
                    .to_string(),
            },
            settings: Settings::from_env(),
            state: Mutex::new(State::default()),
            inbox: Arc::new(Mutex::new(Vec::new())),
            dropped: Arc::new(Mutex::new(0)),
            names: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn settings(&self) -> Result<&Settings, SkillError> {
        self.settings
            .as_ref()
            .map_err(|e| SkillError::Fatal(e.clone()))
    }

    async fn connected<'a>(&self, state: &'a mut State) -> Result<&'a Client, SkillError> {
        if state.client.is_none() {
            let settings = self.settings()?;

            if let Some(parent) = std::path::Path::new(&settings.session).parent() {
                let _ = std::fs::create_dir_all(parent);
            }

            let (client, shutdown) = Client::builder()
                .api_id(settings.api_id)
                .api_hash(&settings.api_hash)
                .session(&settings.session)
                .transport(TransportKind::Abridged)
                .connect()
                .await
                .map_err(|e| SkillError::ExecutionFailed(format!("cannot reach Telegram: {}", e)))?;

            state.client = Some(client);
            state.shutdown = Some(shutdown);
        }

        Ok(state.client.as_ref().expect("just connected"))
    }

    async fn status(&self) -> Result<SkillOutput, SkillError> {
        let mut state = self.state.lock().await;
        let client = self.connected(&mut state).await?;

        let authorised = client
            .is_authorized()
            .await
            .map_err(|e| SkillError::ExecutionFailed(format!("cannot read auth state: {}", e)))?;

        if !authorised {
            return Ok(SkillOutput::Text(
                "Not signed in. Call login, then submit_code with the code Telegram sends."
                    .to_string(),
            ));
        }

        let me = client
            .get_me()
            .await
            .map_err(|e| SkillError::ExecutionFailed(format!("cannot read the account: {}", e)))?;

        Ok(SkillOutput::Text(format!(
            "Signed in as {} {} (id {})",
            me.first_name.as_deref().unwrap_or(""),
            me.last_name.as_deref().unwrap_or(""),
            me.id
        )))
    }

    async fn login(&self) -> Result<SkillOutput, SkillError> {
        let phone = self.settings()?.phone.clone();
        let mut state = self.state.lock().await;
        let client = self.connected(&mut state).await?;

        if client
            .is_authorized()
            .await
            .map_err(|e| SkillError::ExecutionFailed(format!("cannot read auth state: {}", e)))?
        {
            return Ok(SkillOutput::Text("Already signed in.".to_string()));
        }

        let outcome = client
            .request_login_code(&phone)
            .await
            .map_err(|e| SkillError::ExecutionFailed(format!("cannot request a code: {}", e)))?;

        match outcome {
            SendCodeOutcome::AlreadyAuthorized(name) => {
                Ok(SkillOutput::Text(format!("Already signed in as {}.", name)))
            }
            SendCodeOutcome::CodeRequired(token) => {
                state.login = Some(token);
                Ok(SkillOutput::Text(format!(
                    "A login code has been sent to {} in Telegram. Ask the user for it, then \
                     call submit_code with it. Do not guess the code.",
                    phone
                )))
            }
        }
    }

    async fn submit_code(&self, code: &str) -> Result<SkillOutput, SkillError> {
        let code = code.trim();
        if code.is_empty() {
            return Err(SkillError::InvalidArgs(
                "the login code is missing".to_string(),
            ));
        }

        let mut state = self.state.lock().await;
        let token = state.login.take().ok_or_else(|| {
            SkillError::InvalidArgs("no login is in progress — call login first".to_string())
        })?;
        let client = self.connected(&mut state).await?;

        match client.sign_in(&token, code).await {
            Ok(name) => {
                client.save_session().await.map_err(|e| {
                    SkillError::ExecutionFailed(format!("signed in but cannot save it: {}", e))
                })?;
                Ok(SkillOutput::Text(format!(
                    "Signed in as {}. The session is saved, so this will not be needed again.",
                    name
                )))
            }
            Err(SignInError::PasswordRequired(token)) => {
                state.password = Some(*token);
                Ok(SkillOutput::Text(
                    "The account has two-step verification. Ask the user for their Telegram \
                     password and call submit_password with it."
                        .to_string(),
                ))
            }
            Err(SignInError::SignUpRequired) => Err(SkillError::Fatal(
                "that phone number has no Telegram account".to_string(),
            )),
            Err(e) => Err(SkillError::ExecutionFailed(format!("sign-in failed: {}", e))),
        }
    }

    async fn submit_password(&self, password: &str) -> Result<SkillOutput, SkillError> {
        if password.trim().is_empty() {
            return Err(SkillError::InvalidArgs("the password is missing".to_string()));
        }

        let mut state = self.state.lock().await;
        let token = state.password.take().ok_or_else(|| {
            SkillError::InvalidArgs(
                "no password was asked for — call submit_code first".to_string(),
            )
        })?;
        let client = self.connected(&mut state).await?;

        let name = client
            .check_password(token, password.trim())
            .await
            .map_err(|e| SkillError::ExecutionFailed(format!("the password was refused: {}", e)))?;

        client.save_session().await.map_err(|e| {
            SkillError::ExecutionFailed(format!("signed in but cannot save it: {}", e))
        })?;

        Ok(SkillOutput::Text(format!(
            "Signed in as {}. The session is saved.",
            name
        )))
    }

    async fn list_dialogs(&self, args: &str) -> Result<SkillOutput, SkillError> {
        let limit: i32 = args.trim().parse().unwrap_or(20);

        let mut state = self.state.lock().await;
        let client = self.connected(&mut state).await?;

        let dialogs = client
            .get_dialogs(limit)
            .await
            .map_err(|e| SkillError::ExecutionFailed(format!("cannot list chats: {}", e)))?;

        if dialogs.is_empty() {
            return Ok(SkillOutput::Text("No chats.".to_string()));
        }

        let lines: Vec<String> = dialogs
            .iter()
            .map(|dialog| {
                format!(
                    "{} [id {}] unread {}",
                    dialog.title(),
                    peer_id(dialog),
                    dialog.unread_count()
                )
            })
            .collect();

        Ok(SkillOutput::Text(lines.join("\n")))
    }

    async fn read_chat(&self, args: &str) -> Result<SkillOutput, SkillError> {
        let (peer, rest) = split(args);
        if peer.is_empty() {
            return Err(SkillError::InvalidArgs(
                "expected: peer | limit — for example: me | 20".to_string(),
            ));
        }
        let limit: i32 = rest.parse().unwrap_or(20);

        let mut state = self.state.lock().await;
        let client = self.connected(&mut state).await?;

        let page = client
            .get_message_history(peer.as_str(), limit, 0, 0)
            .await
            .map_err(|e| SkillError::ExecutionFailed(format!("cannot read '{}': {}", peer, e)))?;

        if page.messages.is_empty() {
            return Ok(SkillOutput::Text(format!("No messages in '{}'.", peer)));
        }

        let mut lines: Vec<String> = Vec::with_capacity(page.messages.len());

        for message in &page.messages {
            let when = message
                .date_utc()
                .map(|d| d.format("%Y-%m-%d %H:%M").to_string())
                .unwrap_or_default();

            let who = if message.outgoing() {
                "you".to_string()
            } else {
                match message.sender_user_id() {
                    Some(id) => name_of(client, &self.names, id).await,
                    None => peer.clone(),
                }
            };

            lines.push(format!(
                "[{}] {} {}: {}",
                message.id(),
                when,
                who,
                message.text().unwrap_or("").trim()
            ));
        }

        Ok(SkillOutput::Text(lines.join("\n")))
    }

    async fn send_message(&self, args: &str) -> Result<SkillOutput, SkillError> {
        let (peer, text) = split(args);
        if peer.is_empty() || text.is_empty() {
            return Err(SkillError::InvalidArgs(
                "expected: peer | text — for example: me | on my way".to_string(),
            ));
        }

        let mut state = self.state.lock().await;
        let client = self.connected(&mut state).await?;

        client
            .send_message(peer.as_str(), InputMessage::markdown(text.as_str()))
            .await
            .map_err(|e| {
                SkillError::ExecutionFailed(format!("cannot send to '{}': {}", peer, e))
            })?;

        Ok(SkillOutput::Text(format!("Sent to {}.", peer)))
    }

    async fn watch(&self) -> Result<SkillOutput, SkillError> {
        let mut state = self.state.lock().await;

        if state.watching {
            let waiting = self.inbox.lock().await.len();
            return Ok(SkillOutput::Text(format!(
                "Already watching. {} message(s) waiting — call drain to read them.",
                waiting
            )));
        }

        let client = self.connected(&mut state).await?;

        if !client
            .is_authorized()
            .await
            .map_err(|e| SkillError::ExecutionFailed(format!("cannot read auth state: {}", e)))?
        {
            return Err(SkillError::InvalidArgs(
                "not signed in — call login first".to_string(),
            ));
        }

        let mut titles: HashMap<i64, String> = HashMap::new();
        if let Ok(dialogs) = client.get_dialogs(200).await {
            for dialog in &dialogs {
                titles.insert(peer_id(dialog), dialog.title().to_string());
            }
        }

        let mut stream = client.stream_updates();
        let inbox = Arc::clone(&self.inbox);
        let dropped = Arc::clone(&self.dropped);
        let names = Arc::clone(&self.names);
        let resolver = client.clone();

        tokio::spawn(async move {
            while let Some(update) = stream.next().await {
                let ferogram::update::Update::NewMessage(message) = update else {
                    continue;
                };
                if message.outgoing() {
                    continue;
                }

                let text = message.text().unwrap_or("").trim().to_string();
                if text.is_empty() {
                    continue;
                }

                let when = message
                    .date_utc()
                    .map(|d| d.format("%H:%M").to_string())
                    .unwrap_or_default();

                let from = match message.sender_user_id() {
                    Some(id) => name_of(&resolver, &names, id).await,
                    None => "unknown".to_string(),
                };

                let chat_id = message.chat_id();
                let where_ = match titles.get(&chat_id) {
                    Some(title) if *title != from => format!(" in {}", title),
                    _ => String::new(),
                };

                let line = format!("{} {}{}: {}", when, from, where_, text);

                let mut inbox = inbox.lock().await;
                if inbox.len() >= INBOX_LIMIT {
                    inbox.remove(0);
                    *dropped.lock().await += 1;
                }
                inbox.push(line);
            }
        });

        state.watching = true;

        Ok(SkillOutput::Text(
            "Watching. Incoming messages are collected as they arrive; call drain to read and \
             clear them. Nothing is sent anywhere on its own."
                .to_string(),
        ))
    }

    async fn drain(&self) -> Result<SkillOutput, SkillError> {
        let watching = self.state.lock().await.watching;
        if !watching {
            return Ok(SkillOutput::Text(
                "Not watching, so nothing has been collected. Call watch first.".to_string(),
            ));
        }

        let collected: Vec<String> = std::mem::take(&mut *self.inbox.lock().await);
        let lost = std::mem::take(&mut *self.dropped.lock().await);

        if collected.is_empty() {
            return Ok(SkillOutput::Text("No new messages.".to_string()));
        }

        let mut out = format!("{} new message(s):\n{}", collected.len(), collected.join("\n"));
        if lost > 0 {
            out.push_str(&format!(
                "\n\n{} older message(s) were dropped — they arrived faster than they were read.",
                lost
            ));
        }

        Ok(SkillOutput::Text(out))
    }

    fn config(&self) -> SkillOutput {
        match &self.settings {
            Ok(settings) => SkillOutput::Text(format!(
                "Configured for {}. Session file: {}",
                settings.phone, settings.session
            )),
            Err(problem) => SkillOutput::Text(format!(
                "Not configured: {}. Ask the user to add api_id, api_hash and phone_number \
                 under [skills.telegram] in secrets.toml, then restart JumaBek. The api_id and \
                 api_hash come from my.telegram.org.",
                problem
            )),
        }
    }
}

fn split(args: &str) -> (String, String) {
    match args.split_once('|') {
        Some((left, right)) => (left.trim().to_string(), right.trim().to_string()),
        None => (args.trim().to_string(), String::new()),
    }
}

fn peer_id(dialog: &ferogram::Dialog) -> i64 {
    match dialog.peer() {
        Some(ferogram::tl::enums::Peer::User(user)) => user.user_id,
        Some(ferogram::tl::enums::Peer::Chat(chat)) => -chat.chat_id,
        Some(ferogram::tl::enums::Peer::Channel(channel)) => {
            -1_000_000_000_000i64 - channel.channel_id
        }
        None => 0,
    }
}

#[async_trait::async_trait]
impl SkillModule for Telegram {
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
            "login" => self.login().await,
            "submit_code" => self.submit_code(args).await,
            "submit_password" => self.submit_password(args).await,
            "watch" => self.watch().await,
            "drain" => self.drain().await,
            "list_dialogs" => self.list_dialogs(args).await,
            "read_chat" => self.read_chat(args).await,
            "send_message" => self.send_message(args).await,
            other => Err(SkillError::NotFound(format!(
                "no method '{}'. Available: config, status, login, submit_code, \
                 submit_password, list_dialogs, read_chat, send_message",
                other
            ))),
        }
    }

    fn available_methods(&self) -> Vec<MethodInfo> {
        vec![
            MethodInfo {
                method: "config".to_string(),
                description: "Check whether the Telegram credentials are in place. Call this \
                              first if anything else complains about configuration."
                    .to_string(),
                args_description: "Nothing. Pass an empty string.".to_string(),
            },
            MethodInfo {
                method: "status".to_string(),
                description: "Report whether the account is signed in, and which account it is."
                    .to_string(),
                args_description: "Nothing. Pass an empty string.".to_string(),
            },
            MethodInfo {
                method: "login".to_string(),
                description: "Begin signing in. Telegram sends a code to the user's other \
                              devices. This does not complete the login on its own."
                    .to_string(),
                args_description: "Nothing. Pass an empty string.".to_string(),
            },
            MethodInfo {
                method: "submit_code".to_string(),
                description: "Finish signing in with the code the user received. Ask the user \
                              for it — it is never something you can work out."
                    .to_string(),
                args_description: "The code, digits only. Example: 12345".to_string(),
            },
            MethodInfo {
                method: "submit_password".to_string(),
                description: "Only needed when the account has two-step verification and \
                              submit_code asked for a password."
                    .to_string(),
                args_description: "The user's Telegram password.".to_string(),
            },
            MethodInfo {
                method: "watch".to_string(),
                description: "Start collecting incoming messages as they arrive. Telegram pushes \
                              them over the open connection, so nothing is polled and nothing is \
                              missed while this runs. Call it once; it keeps running until the \
                              skill stops."
                    .to_string(),
                args_description: "Nothing. Pass an empty string.".to_string(),
            },
            MethodInfo {
                method: "drain".to_string(),
                description: "Take everything collected since the last drain and clear it. Each \
                              line is who sent it, which chat it was in, and what they said. \
                              Messages the account owner sent are not collected. Pair this with \
                              a background job on a short interval to be told about messages \
                              without asking."
                    .to_string(),
                args_description: "Nothing. Pass an empty string.".to_string(),
            },
            MethodInfo {
                method: "list_dialogs".to_string(),
                description: "List recent chats, groups and channels with their ids and unread \
                              counts. Use it to find the id of a chat before reading it."
                    .to_string(),
                args_description: "How many, as a number. Empty means 20. Example: 30".to_string(),
            },
            MethodInfo {
                method: "read_chat".to_string(),
                description: "Read recent messages from one chat, newest first. Every line names \
                              who wrote it: 'you' for the account owner, otherwise the person's \
                              name and username. In a group this is how you tell them apart."
                    .to_string(),
                args_description: "peer | limit. The peer is a username, a numeric id, or 'me' \
                                   for saved messages. Example: durov | 20"
                    .to_string(),
            },
            MethodInfo {
                method: "send_message".to_string(),
                description: "Send a message as the user. This is visible to someone else and \
                              cannot be taken back — confirm the wording first."
                    .to_string(),
                args_description: "peer | text. Example: me | remember the tickets".to_string(),
            },
        ]
    }
}

#[tokio::main]
async fn main() {
    if let Err(e) = jumabek_sdk::runtime::run_skill(Telegram::new()).await {
        eprintln!("telegram skill stopped: {}", e);
        std::process::exit(1);
    }
}
