use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use ferogram::{Client, InputMessage, PasswordToken, SendCodeOutcome, SignInError, TransportKind};
use jumabek_sdk::{MethodInfo, ModuleMetadata, SkillError, SkillModule, SkillOutput};
use tokio::sync::Mutex;

const INBOX_LIMIT: usize = 500;

type Names = Arc<Mutex<HashMap<i64, String>>>;

/// Chat titles by id, refreshed whenever the watch list changes.
type Titles = Arc<Mutex<HashMap<i64, String>>>;

/// Which chats are worth waking the agent for.
///
/// `None` means every chat, which is what `watch` with no arguments has always
/// done. `Some` names them, and an empty `Some` means none at all — messages
/// still arrive and still pile up for `drain`, nothing just interrupts anybody.
type Watchlist = Arc<Mutex<Option<HashSet<i64>>>>;

async fn name_of(client: &Client, names: &Names, id: i64) -> String {
    if let Some(known) = names.lock().await.get(&id) {
        return known.clone();
    }

    let resolved = match client.get_users_by_id(&[id]).await {
        Ok(users) => users.into_iter().flatten().next().map(|user| {
            let full = [
                user.first_name().unwrap_or(""),
                user.last_name().unwrap_or(""),
            ]
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

/// Where to knock when a message arrives. Without it the skill still works —
/// messages pile up for drain — but nothing wakes the agent on its own.
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
            url: format!("http://127.0.0.1:{}/notify", port),
            token,
        })
    }

    async fn knock(&self, who: &str, text: &str) -> Result<(), String> {
        let body = serde_json::json!({
            "source": "telegram",
            "kind": "notify",
            "who": who,
            "text": text,
        });

        let response = reqwest::Client::new()
            .post(&self.url)
            .bearer_auth(&self.token)
            .json(&body)
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await
            .map_err(|e| e.to_string())?;

        if response.status().is_success() {
            Ok(())
        } else {
            Err(format!("{} from the inbox", response.status()))
        }
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
    titles: Titles,
    watchlist: Watchlist,
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
            titles: Arc::new(Mutex::new(HashMap::new())),
            watchlist: Arc::new(Mutex::new(None)),
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
                .map_err(|e| {
                    SkillError::ExecutionFailed(format!("cannot reach Telegram: {}", e))
                })?;

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
            Err(e) => Err(SkillError::ExecutionFailed(format!(
                "sign-in failed: {}",
                e
            ))),
        }
    }

    async fn submit_password(&self, password: &str) -> Result<SkillOutput, SkillError> {
        if password.trim().is_empty() {
            return Err(SkillError::InvalidArgs(
                "the password is missing".to_string(),
            ));
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
            let when = message.date_utc().map(local_time).unwrap_or_default();

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

    /// Turns what the model wrote into chat ids.
    ///
    /// A number is taken as an id, because that is what `list_dialogs` prints.
    /// Anything else is matched, case-insensitively, against the chat titles
    /// from the same listing. Whatever did not match is handed back rather than
    /// dropped: a name that quietly matches nothing leaves someone certain they
    /// are watching a chat they are not, which is worse than an error.
    async fn resolve_chats(&self, args: &str) -> Result<(Vec<(i64, String)>, Vec<String>), String> {
        let wanted: Vec<&str> = args
            .split(',')
            .map(str::trim)
            .filter(|token| !token.is_empty())
            .collect();

        let mut found = Vec::new();
        let mut missing = Vec::new();

        let titles = self.titles.lock().await;

        for token in wanted {
            if let Ok(id) = token.parse::<i64>() {
                let title = titles
                    .get(&id)
                    .cloned()
                    .unwrap_or_else(|| format!("chat {}", id));
                found.push((id, title));
                continue;
            }

            let needle = token.trim_start_matches('@').to_lowercase();
            let hit = titles
                .iter()
                .find(|(_, title)| title.to_lowercase().contains(&needle));

            match hit {
                Some((id, title)) => found.push((*id, title.clone())),
                None => missing.push(token.to_string()),
            }
        }

        Ok((found, missing))
    }

    async fn refresh_titles(&self, client: &Client) {
        if let Ok(dialogs) = client.get_dialogs(200).await {
            let mut titles = self.titles.lock().await;
            for dialog in &dialogs {
                titles.insert(peer_id(dialog), dialog.title().to_string());
            }
        }
    }

    /// Describes what will and will not interrupt the user right now.
    async fn watchlist_summary(&self) -> String {
        let watchlist = self.watchlist.lock().await;
        let titles = self.titles.lock().await;

        match &*watchlist {
            None => "Waking on every chat.".to_string(),
            Some(chats) if chats.is_empty() => {
                "Waking on nothing. Messages still arrive and wait for drain.".to_string()
            }
            Some(chats) => {
                let named: Vec<String> = chats
                    .iter()
                    .map(|id| match titles.get(id) {
                        Some(title) => format!("{} [id {}]", title, id),
                        None => format!("chat {}", id),
                    })
                    .collect();
                format!("Waking on {} chat(s): {}", named.len(), named.join(", "))
            }
        }
    }

    async fn watch(&self, args: &str) -> Result<SkillOutput, SkillError> {
        let mut state = self.state.lock().await;

        let client = self.connected(&mut state).await?.clone();

        if !client
            .is_authorized()
            .await
            .map_err(|e| SkillError::ExecutionFailed(format!("cannot read auth state: {}", e)))?
        {
            return Err(SkillError::InvalidArgs(
                "not signed in — call login first".to_string(),
            ));
        }

        self.refresh_titles(&client).await;

        let mut unresolved: Vec<String> = Vec::new();

        if args.trim().is_empty() {
            *self.watchlist.lock().await = None;
        } else {
            let (found, missing) = self
                .resolve_chats(args)
                .await
                .map_err(SkillError::ExecutionFailed)?;

            if found.is_empty() {
                return Err(SkillError::InvalidArgs(format!(
                    "none of those name a chat: {}. Call list_dialogs to see the exact titles \
                     and ids, then pass one of those.",
                    missing.join(", ")
                )));
            }

            // Adding rather than replacing is what makes "watch this one too"
            // a single call instead of restating the whole list every time.
            let mut watchlist = self.watchlist.lock().await;
            let chats = watchlist.get_or_insert_with(HashSet::new);
            for (id, _) in found {
                chats.insert(id);
            }
            unresolved = missing;
        }

        if state.watching {
            let waiting = self.inbox.lock().await.len();
            let mut out = format!(
                "{} {} message(s) waiting — call drain to read them.",
                self.watchlist_summary().await,
                waiting
            );
            if !unresolved.is_empty() {
                out.push_str(&format!(
                    "\n\nNot found, so not being watched: {}.",
                    unresolved.join(", ")
                ));
            }
            return Ok(SkillOutput::Text(out));
        }

        let mut stream = client.stream_updates();
        let inbox = Arc::clone(&self.inbox);
        let dropped = Arc::clone(&self.dropped);
        let names = Arc::clone(&self.names);
        let titles = Arc::clone(&self.titles);
        let watchlist = Arc::clone(&self.watchlist);
        let resolver = client.clone();
        let door = Door::from_env();

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

                let when = message.date_utc().map(local_clock).unwrap_or_default();

                let from = match message.sender_user_id() {
                    Some(id) => name_of(&resolver, &names, id).await,
                    None => "unknown".to_string(),
                };

                let chat_id = message_chat_id(&message);
                let where_ = match titles.lock().await.get(&chat_id) {
                    Some(title) if *title != from => format!(" in {}", title),
                    _ => String::new(),
                };

                let line = format!("{} {}{}: {}", when, from, where_, text);

                // Read every time, never captured: this is what lets the list
                // change while the stream stays open.
                let wanted = match &*watchlist.lock().await {
                    None => true,
                    Some(chats) => chats.contains(&chat_id),
                };

                // The agent is told first, and the buffer is the fallback: if
                // the door is shut or refuses, nothing is lost, drain still has
                // it. A chat nobody asked to be woken for goes straight to the
                // buffer — seen, kept, not interrupting anyone.
                let delivered = match (wanted, &door) {
                    (true, Some(door)) => match door.knock(&from, &line).await {
                        Ok(()) => true,
                        Err(e) => {
                            eprintln!("[telegram] cannot reach the inbox: {}", e);
                            false
                        }
                    },
                    _ => false,
                };

                if delivered {
                    continue;
                }

                let mut inbox = inbox.lock().await;
                if inbox.len() >= INBOX_LIMIT {
                    inbox.remove(0);
                    *dropped.lock().await += 1;
                }
                inbox.push(line);
            }
        });

        state.watching = true;
        drop(state);

        let mut out = format!("Watching. {}", self.watchlist_summary().await);

        out.push_str(if Door::from_env().is_some() {
            " Messages from a watched chat are pushed to JumaBek the moment they arrive — no \
             polling and no drain needed. Everything else, and anything that cannot be \
             delivered, waits for drain instead, so nothing is lost."
        } else {
            " Messages are collected as they arrive; call drain to read and clear them. Set \
             inbox_token under [skills.telegram] to have watched chats pushed through the \
             moment they land instead."
        });

        if !unresolved.is_empty() {
            out.push_str(&format!(
                "\n\nNot found, so not being watched: {}.",
                unresolved.join(", ")
            ));
        }

        Ok(SkillOutput::Text(out))
    }

    /// Narrows the list, or empties it. Never stops the stream: messages keep
    /// arriving and keep piling up for `drain`, they simply stop interrupting.
    /// Stopping the stream outright would mean losing everything sent while it
    /// was off, and there is no way to ask Telegram for it afterwards.
    async fn unwatch(&self, args: &str) -> Result<SkillOutput, SkillError> {
        if !self.state.lock().await.watching {
            return Ok(SkillOutput::Text(
                "Not watching, so there is nothing to narrow.".to_string(),
            ));
        }

        if args.trim().is_empty() {
            *self.watchlist.lock().await = Some(HashSet::new());
            return Ok(SkillOutput::Text(
                "Waking on nothing from now on. Messages still arrive and wait for drain; call \
                 watch with a chat to start being interrupted again."
                    .to_string(),
            ));
        }

        let (found, missing) = self
            .resolve_chats(args)
            .await
            .map_err(SkillError::ExecutionFailed)?;

        {
            let mut watchlist = self.watchlist.lock().await;
            match &mut *watchlist {
                // Removing one chat out of "everything" only means anything
                // once "everything" is written out as a list.
                None => {
                    let titles = self.titles.lock().await;
                    let removed: HashSet<i64> = found.iter().map(|(id, _)| *id).collect();
                    *watchlist = Some(
                        titles
                            .keys()
                            .copied()
                            .filter(|id| !removed.contains(id))
                            .collect(),
                    );
                }
                Some(chats) => {
                    for (id, _) in &found {
                        chats.remove(id);
                    }
                }
            }
        }

        let mut out = self.watchlist_summary().await;
        if !missing.is_empty() {
            out.push_str(&format!("\n\nNot found: {}.", missing.join(", ")));
        }

        Ok(SkillOutput::Text(out))
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

        let mut out = format!(
            "{} new message(s):\n{}",
            collected.len(),
            collected.join("\n")
        );
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

/// Telegram stamps messages in UTC. Shown as-is they are wrong by whatever the
/// timezone is, and the agent reasons about them — "an hour ago" has to mean an
/// hour ago.
fn local_time(when: chrono::DateTime<chrono::Utc>) -> String {
    when.with_timezone(&chrono::Local)
        .format("%Y-%m-%d %H:%M")
        .to_string()
}

fn local_clock(when: chrono::DateTime<chrono::Utc>) -> String {
    when.with_timezone(&chrono::Local)
        .format("%H:%M")
        .to_string()
}

fn split(args: &str) -> (String, String) {
    match args.split_once('|') {
        Some((left, right)) => (left.trim().to_string(), right.trim().to_string()),
        None => (args.trim().to_string(), String::new()),
    }
}

/// One id space for chats, whichever end of the library it came from.
///
/// Telegram numbers a group and a user separately, so a group and a user can
/// both be 12345 and mean different chats. The Bot API convention settles it by
/// pushing groups and channels negative, and that is the form `list_dialogs`
/// prints — so it is the form the model has seen and the only sane thing to
/// match on.
///
/// `Message::chat_id()` does **not** use that convention: it hands back the
/// bare number. Comparing the two directly is a comparison that silently never
/// matches for groups and channels, which is why both callers go through here.
fn canonical_peer(peer: &ferogram::tl::enums::Peer) -> i64 {
    match peer {
        ferogram::tl::enums::Peer::User(user) => user.user_id,
        ferogram::tl::enums::Peer::Chat(chat) => -chat.chat_id,
        ferogram::tl::enums::Peer::Channel(channel) => -1_000_000_000_000i64 - channel.channel_id,
    }
}

fn peer_id(dialog: &ferogram::Dialog) -> i64 {
    dialog.peer().map(canonical_peer).unwrap_or(0)
}

fn message_chat_id(message: &ferogram::update::IncomingMessage) -> i64 {
    message.peer_id().map(canonical_peer).unwrap_or(0)
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
            "watch" => self.watch(args).await,
            "unwatch" => self.unwatch(args).await,
            "drain" => self.drain().await,
            "list_dialogs" => self.list_dialogs(args).await,
            "read_chat" => self.read_chat(args).await,
            "send_message" => self.send_message(args).await,
            other => Err(SkillError::NotFound(format!(
                "no method '{}'. Available: {}",
                other,
                self.available_methods()
                    .iter()
                    .map(|m| m.method.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
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
                description: "Start collecting incoming messages as they arrive, and choose \
                              which chats are worth interrupting the user for. Telegram pushes \
                              them over the open connection, so nothing is polled and nothing \
                              is missed while this runs. Safe to call again at any time: it \
                              ADDS to the list rather than replacing it, so watching one more \
                              chat is one call. Messages from every other chat are still \
                              collected and still readable with drain — they just do not wake \
                              anyone. Prefer naming chats: with no arguments every message \
                              becomes a task, which on a busy account means constant \
                              interruption."
                    .to_string(),
                args_description: "Chats to wake on, comma separated: an id from list_dialogs, \
                                   or part of a chat title, case-insensitive. Example: \
                                   'Mum, Upwork, -1001234567890'. Empty string means every \
                                   chat."
                    .to_string(),
            },
            MethodInfo {
                method: "unwatch".to_string(),
                description: "Stop waking on some chats, or on all of them. Never stops \
                              collecting: messages keep arriving and stay readable with drain, \
                              they simply stop interrupting. With an empty string it stops \
                              waking on everything, which is the quiet setting rather than an \
                              off switch."
                    .to_string(),
                args_description: "Chats to stop waking on, comma separated, named the same way \
                                   as in watch. Empty string means all of them."
                    .to_string(),
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
