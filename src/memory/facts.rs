use chrono::Utc;
use rusqlite::{Connection, params};

use crate::error::JumabekResult;

pub const SUBJECT_LIMIT: usize = 64;
pub const VALUE_LIMIT: usize = 400;
pub const RENDER_LIMIT: usize = 120;

#[derive(Debug, Clone, PartialEq)]
pub struct Fact {
    pub subject: String,
    pub key: String,
    pub value: String,
}

pub fn remember(conn: &Connection, fact: &Fact) -> JumabekResult<bool> {
    let now = Utc::now().to_rfc3339();

    let changed = conn.execute(
        "INSERT INTO facts (subject, key, value, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?4)
         ON CONFLICT(subject, key, value) DO UPDATE SET updated_at = ?4",
        params![
            normalise(&fact.subject),
            normalise(&fact.key),
            trim(&fact.value),
            now
        ],
    )?;

    Ok(changed > 0)
}

pub fn forget(conn: &Connection, subject: &str, key: Option<&str>) -> JumabekResult<usize> {
    let subject = normalise(subject);

    let removed = match key {
        Some(key) => conn.execute(
            "DELETE FROM facts WHERE subject = ?1 AND key = ?2",
            params![subject, normalise(key)],
        )?,
        None => conn.execute("DELETE FROM facts WHERE subject = ?1", params![subject])?,
    };

    Ok(removed)
}

pub fn all(conn: &Connection) -> JumabekResult<Vec<Fact>> {
    let mut stmt = conn.prepare(
        "SELECT subject, key, value
           FROM facts
          ORDER BY subject = 'me' DESC, subject, key, id",
    )?;

    let rows = stmt
        .query_map([], |row| {
            Ok(Fact {
                subject: row.get(0)?,
                key: row.get(1)?,
                value: row.get(2)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(rows)
}

/// One line per subject, so a hundred facts about a dozen people stay readable
/// at the top of every request instead of becoming a wall.
pub fn render(facts: &[Fact]) -> String {
    if facts.is_empty() {
        return String::new();
    }

    let mut lines: Vec<String> = Vec::new();
    let mut index = 0;

    while index < facts.len() && lines.len() < RENDER_LIMIT {
        let subject = facts[index].subject.clone();
        let mut parts: Vec<String> = Vec::new();

        while index < facts.len() && facts[index].subject == subject {
            parts.push(format!("{}: {}", facts[index].key, facts[index].value));
            index += 1;
        }

        lines.push(format!("{} — {}", subject, parts.join("; ")));
    }

    if index < facts.len() {
        lines.push(format!(
            "[{} more subjects are stored but not shown here]",
            facts[index..]
                .iter()
                .map(|f| f.subject.as_str())
                .collect::<std::collections::BTreeSet<_>>()
                .len()
        ));
    }

    lines.join("\n")
}

fn normalise(text: &str) -> String {
    let cleaned: String = text
        .trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(SUBJECT_LIMIT)
        .collect();
    cleaned.trim().to_lowercase()
}

fn trim(text: &str) -> String {
    text.trim().chars().take(VALUE_LIMIT).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(crate::memory::schema::SCHEMA).unwrap();
        conn
    }

    fn fact(subject: &str, key: &str, value: &str) -> Fact {
        Fact {
            subject: subject.to_string(),
            key: key.to_string(),
            value: value.to_string(),
        }
    }

    #[test]
    fn a_fact_survives_being_written_twice() {
        let conn = db();
        remember(&conn, &fact("Олжас", "telegram", "@olzhas")).unwrap();
        remember(&conn, &fact("олжас", "TELEGRAM", "@olzhas")).unwrap();

        let stored = all(&conn).unwrap();
        assert_eq!(
            stored.len(),
            1,
            "the same fact was stored twice: {stored:?}"
        );
    }

    #[test]
    fn a_person_can_have_several_names() {
        let conn = db();
        remember(&conn, &fact("Олжас", "alias", "Балык")).unwrap();
        remember(&conn, &fact("Олжас", "alias", "Олжик")).unwrap();
        remember(&conn, &fact("Олжас", "telegram", "@olzhas")).unwrap();

        let rendered = render(&all(&conn).unwrap());
        assert!(rendered.contains("Балык"), "{rendered}");
        assert!(rendered.contains("Олжик"), "{rendered}");
        assert!(rendered.contains("@olzhas"), "{rendered}");
        assert_eq!(
            rendered.lines().count(),
            1,
            "one subject, one line: {rendered}"
        );
    }

    #[test]
    fn what_is_known_about_the_user_comes_first() {
        let conn = db();
        remember(&conn, &fact("Олжас", "alias", "Балык")).unwrap();
        remember(&conn, &fact("me", "city", "Алматы")).unwrap();

        let rendered = render(&all(&conn).unwrap());
        assert!(rendered.starts_with("me —"), "{rendered}");
    }

    #[test]
    fn forgetting_takes_one_key_or_the_whole_subject() {
        let conn = db();
        remember(&conn, &fact("олжас", "alias", "Балык")).unwrap();
        remember(&conn, &fact("олжас", "telegram", "@olzhas")).unwrap();

        assert_eq!(forget(&conn, "олжас", Some("alias")).unwrap(), 1);
        assert_eq!(all(&conn).unwrap().len(), 1);

        assert_eq!(forget(&conn, "олжас", None).unwrap(), 1);
        assert!(all(&conn).unwrap().is_empty());
    }

    #[test]
    fn forgetting_something_unknown_is_not_an_error() {
        let conn = db();
        assert_eq!(forget(&conn, "nobody", None).unwrap(), 0);
    }

    #[test]
    fn nothing_known_renders_to_nothing() {
        let conn = db();
        assert!(render(&all(&conn).unwrap()).is_empty());
    }

    #[test]
    fn an_overlong_value_is_cut_rather_than_stored_whole() {
        let conn = db();
        remember(&conn, &fact("me", "note", &"x".repeat(1000))).unwrap();

        let stored = all(&conn).unwrap();
        assert_eq!(stored[0].value.chars().count(), VALUE_LIMIT);
    }

    #[test]
    fn a_crowded_memory_stops_short_and_says_so() {
        let conn = db();
        for i in 0..(RENDER_LIMIT + 20) {
            remember(&conn, &fact(&format!("person{:03}", i), "note", "x")).unwrap();
        }

        let rendered = render(&all(&conn).unwrap());
        assert_eq!(rendered.lines().count(), RENDER_LIMIT + 1);
        assert!(rendered.ends_with("not shown here]"), "{rendered}");
    }
}
