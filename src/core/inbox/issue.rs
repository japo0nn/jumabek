use std::path::Path;

use crate::error::{JumabekError, JumabekResult};

/// Config and secrets belong to the user: they carry their comments, their ordering and their
/// formatting.
pub fn put_entry(text: &str, table: &str, key: &str, value: &str) -> String {
    let line = format!("{} = \"{}\"", key, value);
    let header = format!("[{}]", table);

    let Some(start) = find_header(text, &header) else {
        let mut out = text.trim_end().to_string();
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str(&format!("{}\n{}\n", header, line));
        return out;
    };

    let body_start = start + header.len();
    let body_end = next_header(text, body_start).unwrap_or(text.len());
    let body = &text[body_start..body_end];

    let replaced: String = body
        .lines()
        .map(|existing| {
            if is_key(existing, key) {
                line.clone()
            } else {
                existing.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");

    if replaced != body.trim_end_matches('\n') && body.lines().any(|l| is_key(l, key)) {
        return format!("{}{}\n{}", &text[..body_start], replaced, &text[body_end..]);
    }

    format!(
        "{}\n{}{}",
        text[..body_start].trim_end(),
        line,
        &text[body_start..]
    )
}

/// A whole table at once, for something like a grant that is several keys and means nothing in
/// pieces.
pub fn put_table(text: &str, table: &str, body: &str) -> String {
    let header = format!("[{}]", table);
    let block = format!("{}\n{}\n", header, body.trim_end());

    let Some(start) = find_header(text, &header) else {
        let mut out = text.trim_end().to_string();
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str(&block);
        return out;
    };

    let end = next_header(text, start + header.len()).unwrap_or(text.len());
    format!("{}{}{}", &text[..start], block, &text[end..])
}

fn find_header(text: &str, header: &str) -> Option<usize> {
    text.lines()
        .scan(0usize, |at, line| {
            let here = *at;
            *at += line.len() + 1;
            Some((here, line))
        })
        .find(|(_, line)| line.trim() == header)
        .map(|(at, _)| at)
}

fn next_header(text: &str, from: usize) -> Option<usize> {
    text[from..]
        .lines()
        .scan(from, |at, line| {
            let here = *at;
            *at += line.len() + 1;
            Some((here, line))
        })
        .find(|(at, line)| *at > from && line.trim_start().starts_with('['))
        .map(|(at, _)| at)
}

fn is_key(line: &str, key: &str) -> bool {
    line.split('=')
        .next()
        .map(|left| left.trim().trim_matches('"') == key)
        .unwrap_or(false)
}

pub fn generate_token() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

pub fn grant_body(skills: &[String]) -> String {
    let listed = skills
        .iter()
        .map(|s| format!("\"{}\"", s))
        .collect::<Vec<_>>()
        .join(", ");

    format!("skills = [{}]\nnew_skills = false\nrisky = false", listed)
}

fn rewrite(path: &Path, change: impl FnOnce(&str) -> String) -> JumabekResult<()> {
    let existing = std::fs::read_to_string(path).unwrap_or_default();
    let updated = change(&existing);

    std::fs::write(path, updated)
        .map_err(|e| JumabekError::ConfigError(format!("cannot write {}: {}", path.display(), e)))
}

/// Issues a key for one caller: the token into secrets, the rights into config, and a copy of
/// the token into that skill's own settings so it can knock.
pub fn issue(
    config_path: &Path,
    secrets_path: &Path,
    module: &str,
    skills: &[String],
) -> JumabekResult<()> {
    let token = generate_token();

    rewrite(secrets_path, |text| {
        let with_token = put_entry(text, "inbox.tokens", module, &token);
        put_entry(
            &with_token,
            &format!("skills.{}", module),
            "inbox_token",
            &token,
        )
    })?;

    rewrite(config_path, |text| {
        put_table(
            text,
            &format!("inbox.grants.{}", module),
            &grant_body(skills),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_table_is_appended() {
        let out = put_entry("[llm]\nmodel = \"x\"", "inbox.tokens", "telegram", "abc");

        assert!(out.contains("[llm]"), "{out}");
        assert!(out.contains("[inbox.tokens]\ntelegram = \"abc\""), "{out}");
    }

    #[test]
    fn an_entry_joins_a_table_that_already_exists() {
        let text = "[inbox.tokens]\nrelay = \"old\"\n\n[llm]\nmodel = \"x\"\n";
        let out = put_entry(text, "inbox.tokens", "telegram", "abc");

        assert!(
            out.contains("relay = \"old\""),
            "the neighbour was lost: {out}"
        );
        assert!(out.contains("telegram = \"abc\""), "{out}");
        assert!(out.contains("[llm]"), "{out}");
    }

    #[test]
    fn rotating_a_key_replaces_it_rather_than_doubling_it() {
        let text = "[inbox.tokens]\ntelegram = \"old\"\n";
        let out = put_entry(text, "inbox.tokens", "telegram", "new");

        assert_eq!(out.matches("telegram =").count(), 1, "{out}");
        assert!(out.contains("\"new\""), "{out}");
        assert!(!out.contains("\"old\""), "{out}");
    }

    #[test]
    fn comments_and_ordering_survive() {
        let text = "# mine, hands off\n[llm]\n# the model\nmodel = \"x\"\n";
        let out = put_entry(text, "inbox.tokens", "telegram", "abc");

        assert!(out.contains("# mine, hands off"), "{out}");
        assert!(out.contains("# the model"), "{out}");
    }

    #[test]
    fn a_whole_table_replaces_the_old_one() {
        let text = "[inbox.grants.telegram]\nskills = []\nrisky = true\n\n[llm]\nmodel = \"x\"\n";
        let out = put_table(
            text,
            "inbox.grants.telegram",
            &grant_body(&["telegram".into()]),
        );

        assert!(out.contains("skills = [\"telegram\"]"), "{out}");
        assert!(
            !out.contains("risky = true"),
            "the old rights survived: {out}"
        );
        assert!(out.contains("[llm]"), "the next table was eaten: {out}");
    }

    #[test]
    fn a_grant_never_hands_out_more_than_asked() {
        let body = grant_body(&["telegram".to_string()]);

        assert!(body.contains("new_skills = false"));
        assert!(body.contains("risky = false"));
    }

    #[test]
    fn an_empty_file_gets_a_clean_table() {
        let out = put_entry("", "inbox.tokens", "telegram", "abc");
        assert_eq!(out, "[inbox.tokens]\ntelegram = \"abc\"\n");
    }

    #[test]
    fn a_generated_token_is_long_and_not_the_same_twice() {
        let one = generate_token();
        let two = generate_token();

        assert_eq!(one.chars().count(), 64);
        assert_ne!(one, two);
        assert!(one.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn issuing_writes_both_files_and_they_still_parse() {
        let dir = std::env::temp_dir().join("jb_issue_test");
        std::fs::create_dir_all(&dir).unwrap();
        let config = dir.join("config.toml");
        let secrets = dir.join("secrets.toml");
        std::fs::write(&config, "[agent]\nmax_iterations = 10\n").unwrap();
        std::fs::write(&secrets, "[llm]\napi_key = \"k\"\n").unwrap();

        issue(&config, &secrets, "telegram", &["telegram".to_string()]).unwrap();

        let config_text = std::fs::read_to_string(&config).unwrap();
        let secrets_text = std::fs::read_to_string(&secrets).unwrap();

        toml::from_str::<toml::Value>(&config_text).expect("config no longer parses");
        let parsed: toml::Value = toml::from_str(&secrets_text).expect("secrets no longer parse");

        assert!(
            config_text.contains("[inbox.grants.telegram]"),
            "{config_text}"
        );
        assert!(config_text.contains("max_iterations = 10"), "{config_text}");

        let token = parsed["inbox"]["tokens"]["telegram"].as_str().unwrap();
        let handed = parsed["skills"]["telegram"]["inbox_token"]
            .as_str()
            .unwrap();
        assert_eq!(
            token, handed,
            "the skill got a different token from the door"
        );
    }
}
