use std::path::{Path, PathBuf};
use std::str::FromStr;

use chrono::{DateTime, Duration, Utc};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::core::task::Grant;
use crate::error::{JumabekError, JumabekResult};

const DEFAULT_POLL_SECONDS: u64 = 15;

const BUSY_TIMEOUT_MS: u32 = 5_000;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Schedule {
    Once { at: DateTime<Utc> },
    Every { seconds: u64 },
    Cron { expr: String },
    Watch { path: PathBuf, seconds: u64 },
}

impl Schedule {
    pub fn parse(text: &str) -> JumabekResult<Schedule> {
        let text = text.trim();
        let (head, rest) = match text.split_once(char::is_whitespace) {
            Some((head, rest)) => (head.to_lowercase(), rest.trim()),
            None => (text.to_lowercase(), ""),
        };

        match head.as_str() {
            "in" => Ok(Schedule::Once {
                at: Utc::now() + parse_duration(rest)?,
            }),

            "at" => {
                let at = DateTime::parse_from_rfc3339(rest)
                    .map_err(|e| {
                        bad(format!(
                            "'at' needs an RFC3339 timestamp like 2026-07-30T09:00:00Z: {}",
                            e
                        ))
                    })?
                    .with_timezone(&Utc);
                Ok(Schedule::Once { at })
            }

            "every" => {
                let every = parse_duration(rest)?;
                let seconds = every.num_seconds();
                if seconds < 10 {
                    return Err(bad(
                        "'every' must be at least 10s, anything shorter is a busy loop".to_string(),
                    ));
                }
                Ok(Schedule::Every {
                    seconds: seconds as u64,
                })
            }

            "cron" => {
                let expr = normalise_cron(rest)?;
                cron::Schedule::from_str(&expr)
                    .map_err(|e| bad(format!("cannot read the cron expression: {}", e)))?;
                Ok(Schedule::Cron { expr })
            }

            "watch" => {
                if rest.is_empty() {
                    return Err(bad("'watch' needs a directory to watch".to_string()));
                }
                Ok(Schedule::Watch {
                    path: PathBuf::from(rest),
                    seconds: DEFAULT_POLL_SECONDS,
                })
            }

            other => Err(bad(format!(
                "unknown schedule '{}'. Use: in <duration>, at <rfc3339>, \
                 every <duration>, cron <expression>, watch <directory>",
                other
            ))),
        }
    }

    pub fn describe(&self) -> String {
        match self {
            Schedule::Once { at } => format!("once at {}", at.to_rfc3339()),
            Schedule::Every { seconds } => format!("every {}", humanise(*seconds)),
            Schedule::Cron { expr } => format!("cron {}", expr),
            Schedule::Watch { path, seconds } => {
                format!("watching {} every {}", path.display(), humanise(*seconds))
            }
        }
    }

    pub fn next_after(&self, after: DateTime<Utc>) -> Option<DateTime<Utc>> {
        match self {
            Schedule::Once { at } => {
                if *at > after {
                    Some(*at)
                } else {
                    None
                }
            }
            Schedule::Every { seconds } => Some(after + Duration::seconds(*seconds as i64)),
            Schedule::Cron { expr } => cron::Schedule::from_str(expr)
                .ok()?
                .after(&after)
                .next()
                .map(|t| t.with_timezone(&Utc)),
            Schedule::Watch { seconds, .. } => Some(after + Duration::seconds(*seconds as i64)),
        }
    }

    pub fn first_run(&self) -> Option<DateTime<Utc>> {
        match self {
            Schedule::Every { seconds } => Some(Utc::now() + Duration::seconds(*seconds as i64)),
            other => other.next_after(Utc::now()),
        }
    }
}

fn bad(message: String) -> JumabekError {
    JumabekError::ConfigError(message)
}

fn parse_duration(text: &str) -> JumabekResult<Duration> {
    let text = text.trim();
    let digits: String = text.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return Err(bad(format!(
            "'{}' is not a duration — write it like 45s, 30m, 2h or 1d",
            text
        )));
    }

    let amount: i64 = digits
        .parse()
        .map_err(|_| bad(format!("'{}' is too large to be a duration", digits)))?;

    match text[digits.len()..].trim() {
        "s" | "sec" | "secs" | "second" | "seconds" => Ok(Duration::seconds(amount)),
        "m" | "min" | "mins" | "minute" | "minutes" => Ok(Duration::minutes(amount)),
        "h" | "hr" | "hrs" | "hour" | "hours" => Ok(Duration::hours(amount)),
        "d" | "day" | "days" => Ok(Duration::days(amount)),
        other => Err(bad(format!("unknown unit '{}' — use s, m, h or d", other))),
    }
}

fn normalise_cron(expr: &str) -> JumabekResult<String> {
    let fields: Vec<&str> = expr.split_whitespace().collect();
    match fields.len() {
        5 => Ok(format!("0 {}", fields.join(" "))),
        6 | 7 => Ok(fields.join(" ")),
        n => Err(bad(format!(
            "a cron expression has 5 fields (minute hour day month weekday), got {}",
            n
        ))),
    }
}

fn humanise(seconds: u64) -> String {
    match seconds {
        s if s % 86_400 == 0 && s >= 86_400 => format!("{}d", s / 86_400),
        s if s % 3_600 == 0 && s >= 3_600 => format!("{}h", s / 3_600),
        s if s % 60 == 0 && s >= 60 => format!("{}m", s / 60),
        s => format!("{}s", s),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Running,
    Paused,
    Done,
}

impl State {
    pub fn as_str(&self) -> &'static str {
        match self {
            State::Running => "running",
            State::Paused => "paused",
            State::Done => "done",
        }
    }

    fn parse(text: &str) -> State {
        match text {
            "paused" => State::Paused,
            "done" => State::Done,
            _ => State::Running,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Job {
    pub id: i64,
    pub name: String,
    pub task: String,
    pub schedule: Schedule,
    pub grant: Grant,
    pub state: State,
    pub next_run: Option<DateTime<Utc>>,
    pub last_run: Option<String>,
    pub last_result: Option<String>,
    pub runs: i64,
}

pub struct NewJob {
    pub name: String,
    pub task: String,
    pub schedule: Schedule,
    pub grant: Grant,
}

pub struct JobStore {
    conn: Mutex<Connection>,
}

impl JobStore {
    pub fn open(path: &Path) -> JumabekResult<Self> {
        let conn = Connection::open(path)?;
        conn.busy_timeout(std::time::Duration::from_millis(BUSY_TIMEOUT_MS as u64))?;
        Ok(JobStore {
            conn: Mutex::new(conn),
        })
    }

    pub async fn add(&self, job: NewJob) -> JumabekResult<i64> {
        let conn = self.conn.lock().await;
        let next = job.schedule.first_run();

        conn.execute(
            "INSERT INTO jobs (name, task, schedule, grant_json, state, created_at, next_run)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                job.name,
                job.task,
                serde_json::to_string(&job.schedule).unwrap_or_default(),
                serde_json::to_string(&job.grant).unwrap_or_default(),
                State::Running.as_str(),
                Utc::now().to_rfc3339(),
                next.map(|t| t.to_rfc3339()),
            ],
        )?;

        Ok(conn.last_insert_rowid())
    }

    pub async fn all(&self) -> JumabekResult<Vec<Job>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT id, name, task, schedule, grant_json, state, next_run, last_run,
                    last_result, runs
               FROM jobs
              ORDER BY id",
        )?;

        let rows = stmt
            .query_map([], |row| Ok(read_job(row)))?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(rows.into_iter().flatten().collect())
    }

    pub async fn due(&self, now: DateTime<Utc>) -> JumabekResult<Vec<Job>> {
        Ok(self
            .all()
            .await?
            .into_iter()
            .filter(|job| {
                job.state == State::Running && job.next_run.is_some_and(|next| next <= now)
            })
            .collect())
    }

    pub async fn get(&self, id: i64) -> JumabekResult<Option<Job>> {
        Ok(self.all().await?.into_iter().find(|job| job.id == id))
    }

    pub async fn set_state(&self, id: i64, state: State) -> JumabekResult<bool> {
        let conn = self.conn.lock().await;
        let changed = conn.execute(
            "UPDATE jobs SET state = ?1 WHERE id = ?2",
            params![state.as_str(), id],
        )?;
        Ok(changed > 0)
    }

    pub async fn remove(&self, id: i64) -> JumabekResult<bool> {
        let conn = self.conn.lock().await;
        let changed = conn.execute("DELETE FROM jobs WHERE id = ?1", params![id])?;
        Ok(changed > 0)
    }

    pub async fn finish_run(&self, id: i64, result: &str) -> JumabekResult<()> {
        let Some(job) = self.get(id).await? else {
            return Ok(());
        };

        let now = Utc::now();
        let next = job.schedule.next_after(now);
        let state = if next.is_some() {
            job.state
        } else {
            State::Done
        };

        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE jobs
                SET last_run = ?1, last_result = ?2, runs = runs + 1, next_run = ?3, state = ?4
              WHERE id = ?5",
            params![
                now.to_rfc3339(),
                truncate(result),
                next.map(|t| t.to_rfc3339()),
                state.as_str(),
                id,
            ],
        )?;

        Ok(())
    }
}

fn read_job(row: &rusqlite::Row) -> Option<Job> {
    let schedule: String = row.get(3).ok()?;
    let grant: String = row.get(4).ok()?;
    let state: String = row.get(5).ok()?;
    let next: Option<String> = row.get(6).ok()?;

    Some(Job {
        id: row.get(0).ok()?,
        name: row.get(1).ok()?,
        task: row.get(2).ok()?,
        schedule: serde_json::from_str(&schedule).ok()?,
        grant: serde_json::from_str(&grant).ok()?,
        state: State::parse(&state),
        next_run: next
            .and_then(|t| DateTime::parse_from_rfc3339(&t).ok())
            .map(|t| t.with_timezone(&Utc)),
        last_run: row.get(7).ok()?,
        last_result: row.get(8).ok()?,
        runs: row.get(9).ok()?,
    })
}

fn truncate(text: &str) -> String {
    const LIMIT: usize = 1_000;
    match text.char_indices().nth(LIMIT) {
        Some((idx, _)) => format!("{}…", &text[..idx]),
        None => text.to_string(),
    }
}

pub fn snapshot(path: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(path) else {
        return Vec::new();
    };

    let mut seen: Vec<String> = entries
        .flatten()
        .map(|entry| {
            let meta = entry.metadata().ok();
            let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
            let stamp = meta
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            format!("{}|{}|{}", entry.file_name().to_string_lossy(), size, stamp)
        })
        .collect();

    seen.sort();
    seen
}

pub fn changes(before: &[String], after: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();

    for entry in after {
        if !before.contains(entry) {
            let name = entry.split('|').next().unwrap_or(entry);
            let existed = before.iter().any(|b| b.starts_with(&format!("{}|", name)));
            out.push(format!(
                "{} {}",
                if existed { "changed" } else { "added" },
                name
            ));
        }
    }

    for entry in before {
        let name = entry.split('|').next().unwrap_or(entry);
        if !after.iter().any(|a| a.starts_with(&format!("{}|", name))) {
            out.push(format!("removed {}", name));
        }
    }

    out.sort();
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_one_off_delay() {
        let Schedule::Once { at } = Schedule::parse("in 2h").unwrap() else {
            panic!("not a one-off");
        };
        let ahead = (at - Utc::now()).num_minutes();
        assert!((115..=120).contains(&ahead), "{} minutes ahead", ahead);
    }

    #[test]
    fn reads_an_absolute_moment() {
        let parsed = Schedule::parse("at 2026-07-30T09:00:00Z").unwrap();
        assert!(matches!(parsed, Schedule::Once { .. }));
    }

    #[test]
    fn reads_an_interval() {
        assert_eq!(
            Schedule::parse("every 30m").unwrap(),
            Schedule::Every { seconds: 1_800 }
        );
    }

    #[test]
    fn refuses_a_busy_loop() {
        let err = Schedule::parse("every 1s").unwrap_err().to_string();
        assert!(err.contains("at least 10s"), "got: {err}");
    }

    #[test]
    fn a_five_field_cron_gets_its_seconds_column() {
        let Schedule::Cron { expr } = Schedule::parse("cron 0 9 * * 1-5").unwrap() else {
            panic!("not a cron");
        };
        assert_eq!(expr, "0 0 9 * * 1-5");
    }

    #[test]
    fn a_broken_cron_is_refused_at_creation() {
        assert!(Schedule::parse("cron not a cron line").is_err());
        assert!(Schedule::parse("cron 99 99 * * *").is_err());
    }

    #[test]
    fn cron_finds_its_next_occurrence() {
        let schedule = Schedule::parse("cron 0 9 * * *").unwrap();
        let from = DateTime::parse_from_rfc3339("2026-07-29T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let next = schedule.next_after(from).unwrap();
        assert_eq!(next.to_rfc3339(), "2026-07-30T09:00:00+00:00");
    }

    #[test]
    fn a_one_off_does_not_come_round_again() {
        let schedule = Schedule::parse("in 1h").unwrap();
        assert!(
            schedule
                .next_after(Utc::now() + Duration::hours(2))
                .is_none()
        );
    }

    #[test]
    fn an_interval_waits_before_its_first_run() {
        let schedule = Schedule::parse("every 30m").unwrap();
        let first = schedule.first_run().unwrap();
        assert!(first > Utc::now() + Duration::minutes(25));
    }

    #[test]
    fn unknown_schedules_say_what_is_accepted() {
        let err = Schedule::parse("sometimes maybe").unwrap_err().to_string();
        assert!(err.contains("cron"), "got: {err}");
        assert!(err.contains("watch"), "got: {err}");
    }

    #[test]
    fn durations_cover_the_usual_units() {
        assert_eq!(parse_duration("45s").unwrap(), Duration::seconds(45));
        assert_eq!(parse_duration("30 minutes").unwrap(), Duration::minutes(30));
        assert_eq!(parse_duration("2h").unwrap(), Duration::hours(2));
        assert_eq!(parse_duration("1d").unwrap(), Duration::days(1));
        assert!(parse_duration("soon").is_err());
        assert!(parse_duration("5 fortnights").is_err());
    }

    #[test]
    fn a_watch_reports_what_moved() {
        let before = vec!["a.txt|10|100".to_string(), "b.txt|20|200".to_string()];
        let after = vec!["a.txt|10|100".to_string(), "c.txt|30|300".to_string()];

        assert_eq!(
            changes(&before, &after),
            vec!["added c.txt", "removed b.txt"]
        );
    }

    #[test]
    fn a_rewritten_file_reads_as_changed_not_added() {
        let before = vec!["a.txt|10|100".to_string()];
        let after = vec!["a.txt|11|180".to_string()];
        assert_eq!(changes(&before, &after), vec!["changed a.txt"]);
    }

    #[test]
    fn an_unchanged_directory_reports_nothing() {
        let same = vec!["a.txt|10|100".to_string()];
        assert!(changes(&same, &same).is_empty());
    }

    #[test]
    fn schedules_describe_themselves_for_a_listing() {
        assert_eq!(Schedule::parse("every 2h").unwrap().describe(), "every 2h");
        assert!(
            Schedule::parse("watch C:/tmp")
                .unwrap()
                .describe()
                .contains("watching")
        );
    }
}
