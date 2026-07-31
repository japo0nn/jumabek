pub mod facts;
pub mod query;
pub mod schema;

use std::path::Path;

use chrono::Utc;
use rusqlite::{Connection, params};
use tokio::sync::Mutex;

use crate::error::JumabekResult;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
    Skill,
    System,
}

impl Role {
    pub fn as_str(&self) -> &'static str {
        match self {
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Skill => "skill",
            Role::System => "system",
        }
    }
}

#[derive(Debug, Clone)]
pub struct NewMessage {
    pub role: Role,
    pub content: String,
    pub task_id: Option<String>,
    pub parent_task_id: Option<String>,
    pub raw_json: Option<String>,
}

impl NewMessage {
    pub fn new(role: Role, content: impl Into<String>) -> Self {
        NewMessage {
            role,
            content: content.into(),
            task_id: None,
            parent_task_id: None,
            raw_json: None,
        }
    }

    pub fn task(mut self, task_id: impl Into<String>) -> Self {
        self.task_id = Some(task_id.into());
        self
    }

    pub fn parent(mut self, parent_task_id: Option<String>) -> Self {
        self.parent_task_id = parent_task_id;
        self
    }

    pub fn raw(mut self, raw_json: impl Into<String>) -> Self {
        self.raw_json = Some(raw_json.into());
        self
    }
}

#[derive(Debug, Clone)]
pub struct StoredMessage {
    pub id: i64,
    pub task_id: Option<String>,
    pub role: String,
    pub content: String,
    pub raw_json: Option<String>,
}

impl StoredMessage {
    pub fn llm_content(&self) -> &str {
        self.raw_json.as_deref().unwrap_or(&self.content)
    }
}

#[derive(Debug, Clone)]
pub struct MemoryHit {
    pub session_id: i64,
    pub created_at: String,
    pub role: String,
    pub content: String,
}

pub struct Memory {
    conn: Mutex<Connection>,
    session_id: i64,
}

impl Memory {
    pub async fn open(path: &Path, interface: &str) -> JumabekResult<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let conn = Connection::open(path)?;
        migrate(&conn)?;
        conn.execute_batch(schema::SCHEMA)?;

        let started_at = Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO sessions (started_at, interface) VALUES (?1, ?2)",
            params![started_at, interface],
        )?;
        let session_id = conn.last_insert_rowid();

        Ok(Memory {
            conn: Mutex::new(conn),
            session_id,
        })
    }

    pub fn session_id(&self) -> i64 {
        self.session_id
    }

    pub async fn log(&self, message: NewMessage) -> JumabekResult<i64> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO messages
                 (session_id, task_id, parent_task_id, role, content, raw_json, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                self.session_id,
                message.task_id,
                message.parent_task_id,
                message.role.as_str(),
                message.content,
                message.raw_json,
                Utc::now().to_rfc3339(),
            ],
        )?;

        let id = conn.last_insert_rowid();

        if matches!(message.role, Role::User | Role::Assistant) {
            let searchable = query::to_search_text(&message.content);
            if !searchable.is_empty() {
                conn.execute(
                    "INSERT INTO messages_fts(rowid, content) VALUES (?1, ?2)",
                    params![id, searchable],
                )?;
            }
        }

        Ok(id)
    }

    pub async fn current_session(&self) -> JumabekResult<Vec<StoredMessage>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT id, task_id, role, content, raw_json
               FROM messages
              WHERE session_id = ?1
              ORDER BY id",
        )?;

        let rows = stmt
            .query_map(params![self.session_id], |row| {
                Ok(StoredMessage {
                    id: row.get(0)?,
                    task_id: row.get(1)?,
                    role: row.get(2)?,
                    content: row.get(3)?,
                    raw_json: row.get(4)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(rows)
    }

    /// The tail of the session before this one, so a restart does not read as
    /// amnesia. Only the root agent asks for it; a sub-agent starts clean on
    /// purpose.
    pub async fn previous_session_tail(&self, limit: u32) -> JumabekResult<Vec<StoredMessage>> {
        if limit == 0 {
            return Ok(Vec::new());
        }

        let conn = self.conn.lock().await;

        let previous: Option<i64> = conn
            .query_row(
                "SELECT MAX(id) FROM sessions WHERE id < ?1",
                params![self.session_id],
                |row| row.get(0),
            )
            .ok()
            .flatten();

        let Some(previous) = previous else {
            return Ok(Vec::new());
        };

        let mut stmt = conn.prepare(
            "SELECT id, task_id, role, content, raw_json
               FROM messages
              WHERE session_id = ?1
              ORDER BY id DESC
              LIMIT ?2",
        )?;

        let mut rows = stmt
            .query_map(params![previous, limit], |row| {
                Ok(StoredMessage {
                    id: row.get(0)?,
                    task_id: row.get(1)?,
                    role: row.get(2)?,
                    content: row.get(3)?,
                    raw_json: row.get(4)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        rows.reverse();
        Ok(rows)
    }

    pub async fn remember(&self, fact: &facts::Fact) -> JumabekResult<bool> {
        let conn = self.conn.lock().await;
        facts::remember(&conn, fact)
    }

    pub async fn forget(&self, subject: &str, key: Option<&str>) -> JumabekResult<usize> {
        let conn = self.conn.lock().await;
        facts::forget(&conn, subject, key)
    }

    pub async fn known_facts(&self) -> JumabekResult<Vec<facts::Fact>> {
        let conn = self.conn.lock().await;
        facts::all(&conn)
    }

    pub async fn search(&self, raw_query: &str, limit: u32) -> JumabekResult<Vec<MemoryHit>> {
        let Some(match_query) = query::build_match_query(raw_query) else {
            return Ok(Vec::new());
        };

        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT m.session_id, m.created_at, m.role, m.content
               FROM messages_fts f
               JOIN messages m ON m.id = f.rowid
              WHERE messages_fts MATCH ?1
                AND m.session_id != ?2
              ORDER BY bm25(messages_fts)
              LIMIT ?3",
        )?;

        let rows = stmt
            .query_map(params![match_query, self.session_id, limit], |row| {
                Ok(MemoryHit {
                    session_id: row.get(0)?,
                    created_at: row.get(1)?,
                    role: row.get(2)?,
                    content: row.get(3)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(rows)
    }

    pub async fn close(&self) -> JumabekResult<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE sessions SET ended_at = ?1 WHERE id = ?2",
            params![Utc::now().to_rfc3339(), self.session_id],
        )?;
        Ok(())
    }
}

fn migrate(conn: &Connection) -> JumabekResult<()> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;

    if version < schema::SCHEMA_VERSION {
        conn.execute_batch(schema::DROP_LEGACY_INDEX)?;
        conn.execute_batch(schema::SCHEMA)?;
        reindex(conn)?;
        conn.execute_batch(&format!("PRAGMA user_version = {}", schema::SCHEMA_VERSION))?;
    }

    Ok(())
}

fn reindex(conn: &Connection) -> JumabekResult<usize> {
    let mut stmt = conn.prepare(
        "SELECT id, content FROM messages WHERE role IN ('user', 'assistant') ORDER BY id",
    )?;

    let rows: Vec<(i64, String)> = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<Result<Vec<_>, _>>()?;

    let mut indexed = 0;
    for (id, content) in rows {
        let searchable = query::to_search_text(&content);
        if searchable.is_empty() {
            continue;
        }
        conn.execute(
            "INSERT INTO messages_fts(rowid, content) VALUES (?1, ?2)",
            params![id, searchable],
        )?;
        indexed += 1;
    }

    Ok(indexed)
}
