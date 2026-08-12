use serde::{Deserialize, Serialize};

use crate::core::task::{Grant, Origin};

pub const TEXT_LIMIT: usize = 8_000;
pub const SOURCE_LIMIT: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Notify,
    Ask,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Incoming {
    pub source: String,
    pub kind: Kind,
    pub text: String,
    #[serde(default)]
    pub who: Option<String>,
    #[serde(default)]
    pub meta: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Accepted {
    pub source: String,
    pub kind: Kind,
    pub text: String,
    pub who: String,
    pub grant: Grant,
}

impl Accepted {
    pub fn origin(&self) -> Origin {
        Origin {
            source: self.source.clone(),
            who: self.who.clone(),
        }
    }

    /// What the agent is actually asked to do.
    pub fn as_task(&self) -> String {
        let mut task = format!("[from {} · {}]\n{}", self.source, self.who, self.text);

        if self.kind == Kind::Notify {
            task.push_str(
                "\n\nThis arrived on its own; nobody is waiting on a reply. Decide whether it \
                 is worth telling the user about, and say so in one or two lines if it is.",
            );
        }

        task
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Refusal {
    Malformed(String),
    NoText,
    NoSource,
    TooLong,
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refusal::Malformed(why) => write!(f, "cannot read the request: {}", why),
            Refusal::NoText => write!(f, "'text' is required and cannot be empty"),
            Refusal::NoSource => write!(f, "'source' is required and cannot be empty"),
            Refusal::TooLong => write!(f, "'text' is longer than {} characters", TEXT_LIMIT),
        }
    }
}

/// Everything that reaches the agent from outside passes through here.
pub fn accept(body: &str, grant: Grant) -> Result<Accepted, Refusal> {
    let incoming: Incoming =
        serde_json::from_str(body).map_err(|e| Refusal::Malformed(e.to_string()))?;

    let source = clean(&incoming.source, SOURCE_LIMIT);
    if source.is_empty() {
        return Err(Refusal::NoSource);
    }

    let text = incoming.text.trim();
    if text.is_empty() {
        return Err(Refusal::NoText);
    }
    if text.chars().count() > TEXT_LIMIT {
        return Err(Refusal::TooLong);
    }

    let who = incoming
        .who
        .map(|who| clean(&who, SOURCE_LIMIT))
        .filter(|who| !who.is_empty())
        .unwrap_or_else(|| "unknown".to_string());

    Ok(Accepted {
        source,
        kind: incoming.kind,
        text: text.to_string(),
        who,
        grant,
    })
}

fn clean(raw: &str, limit: usize) -> String {
    raw.trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(limit)
        .collect::<String>()
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grant() -> Grant {
        Grant {
            skills: vec!["shell_executor".to_string()],
            new_skills: false,
            risky: false,
        }
    }

    #[test]
    fn a_well_formed_notify_is_accepted() {
        let body = r#"{"source":"telegram","kind":"notify","text":"Асия: буду через час"}"#;
        let accepted = accept(body, grant()).unwrap();

        assert_eq!(accepted.source, "telegram");
        assert_eq!(accepted.kind, Kind::Notify);
        assert_eq!(accepted.who, "unknown");
    }

    #[test]
    fn who_is_carried_through_when_it_is_given() {
        let body = r#"{"source":"phone","kind":"ask","text":"что по задачам","who":"aibar"}"#;
        assert_eq!(accept(body, grant()).unwrap().who, "aibar");
    }

    #[test]
    fn the_grant_comes_from_the_token_not_the_caller() {
        let body = r#"{"source":"x","kind":"ask","text":"hi",
                       "grant":{"skills":["*"],"new_skills":true,"risky":true}}"#;
        let accepted = accept(body, grant()).unwrap();

        assert_eq!(accepted.grant, grant(), "a caller widened its own rights");
        assert!(!accepted.grant.risky);
    }

    #[test]
    fn a_request_without_text_is_refused() {
        let body = r#"{"source":"telegram","kind":"notify","text":"   "}"#;
        assert_eq!(accept(body, grant()), Err(Refusal::NoText));
    }

    #[test]
    fn a_request_without_a_source_is_refused() {
        let body = r#"{"source":"","kind":"notify","text":"что-то"}"#;
        assert_eq!(accept(body, grant()), Err(Refusal::NoSource));
    }

    #[test]
    fn an_unknown_kind_is_refused_rather_than_guessed() {
        let body = r#"{"source":"x","kind":"delete_everything","text":"hi"}"#;
        assert!(matches!(accept(body, grant()), Err(Refusal::Malformed(_))));
    }

    #[test]
    fn rubbish_is_refused_with_a_reason() {
        assert!(matches!(
            accept("not json", grant()),
            Err(Refusal::Malformed(_))
        ));
        assert!(matches!(accept("", grant()), Err(Refusal::Malformed(_))));
    }

    #[test]
    fn an_enormous_text_is_refused_rather_than_truncated() {
        let body = serde_json::json!({
            "source": "x", "kind": "ask", "text": "a".repeat(TEXT_LIMIT + 1)
        })
        .to_string();
        assert_eq!(accept(&body, grant()), Err(Refusal::TooLong));
    }

    #[test]
    fn control_characters_are_stripped_from_the_source() {
        let body = "{\"source\":\"tele\\u0000gram\\n\",\"kind\":\"ask\",\"text\":\"hi\"}";
        assert_eq!(accept(body, grant()).unwrap().source, "telegram");
    }

    #[test]
    fn the_task_names_where_it_came_from() {
        let body = r#"{"source":"telegram","kind":"notify","text":"Асия: буду","who":"asiya"}"#;
        let task = accept(body, grant()).unwrap().as_task();

        assert!(task.contains("[from telegram · asiya]"), "{task}");
        assert!(task.contains("nobody is waiting on a reply"), "{task}");
    }

    #[test]
    fn an_ask_does_not_carry_the_notify_wording() {
        let body = r#"{"source":"phone","kind":"ask","text":"сколько задач"}"#;
        let task = accept(body, grant()).unwrap().as_task();

        assert!(!task.contains("nobody is waiting"), "{task}");
        assert!(task.ends_with("сколько задач"), "{task}");
    }
}
