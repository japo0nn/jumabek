pub const SCHEMA_VERSION: i64 = 2;

pub const SCHEMA: &str = r#"
PRAGMA journal_mode = WAL;
PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS sessions (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    started_at TEXT NOT NULL,
    ended_at   TEXT,
    interface  TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS messages (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id     INTEGER NOT NULL REFERENCES sessions(id),
    task_id        TEXT,
    parent_task_id TEXT,
    role           TEXT NOT NULL,
    content        TEXT NOT NULL,
    raw_json       TEXT,
    created_at     TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_messages_session ON messages(session_id, id);
CREATE INDEX IF NOT EXISTS idx_messages_task    ON messages(task_id);

CREATE TABLE IF NOT EXISTS facts (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    subject    TEXT NOT NULL,
    key        TEXT NOT NULL,
    value      TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE(subject, key, value)
);

CREATE INDEX IF NOT EXISTS idx_facts_subject ON facts(subject);

CREATE TABLE IF NOT EXISTS jobs (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    name        TEXT NOT NULL,
    task        TEXT NOT NULL,
    schedule    TEXT NOT NULL,
    grant_json  TEXT NOT NULL,
    state       TEXT NOT NULL,
    created_at  TEXT NOT NULL,
    next_run    TEXT,
    last_run    TEXT,
    last_result TEXT,
    runs        INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS idx_jobs_due ON jobs(state, next_run);

CREATE VIRTUAL TABLE IF NOT EXISTS messages_fts USING fts5(
    content,
    tokenize='unicode61 remove_diacritics 2'
);
"#;

pub const DROP_LEGACY_INDEX: &str = r#"
DROP TRIGGER IF EXISTS messages_ai;
DROP TABLE IF EXISTS messages_fts;
"#;
