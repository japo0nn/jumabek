use std::time::Duration;

use colored::Colorize;

use crate::configs::{self, Config};
use crate::core::preflight;
use crate::error::JumabekResult;
use crate::skill_layer::loader;

const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Ok,
    Warn,
    Fail,
}

impl Level {
    fn label(&self) -> colored::ColoredString {
        match self {
            Level::Ok => "ok  ".green(),
            Level::Warn => "WARN".yellow(),
            Level::Fail => "FAIL".red().bold(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Check {
    pub level: Level,
    pub name: String,
    pub detail: String,
    pub hint: Option<String>,
}

impl Check {
    fn new(level: Level, name: &str, detail: impl Into<String>) -> Self {
        Check {
            level,
            name: name.to_string(),
            detail: detail.into(),
            hint: None,
        }
    }

    fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }
}

pub fn summarise(checks: &[Check]) -> (usize, usize, usize) {
    let count = |level: Level| checks.iter().filter(|c| c.level == level).count();
    (count(Level::Ok), count(Level::Warn), count(Level::Fail))
}

pub fn print(checks: &[Check]) {
    println!();
    for check in checks {
        println!(
            "  {} {:<12} {}",
            check.level.label(),
            check.name,
            check.detail
        );
        if let Some(hint) = &check.hint {
            for line in hint.lines() {
                println!("       {}", line.bright_black());
            }
        }
    }

    let (ok, warn, fail) = summarise(checks);
    println!();
    println!("  {} ok, {} warning(s), {} failure(s)", ok, warn, fail);

    if fail == 0 && warn == 0 {
        println!("  {}", "everything is in place".green());
    } else if fail == 0 {
        println!(
            "  {}",
            "JumaBek will run; the warnings above disable parts of it".yellow()
        );
    }
    println!();
}

pub async fn run() -> JumabekResult<Vec<Check>> {
    let mut checks = Vec::new();

    let home = configs::jumabek_dir();
    checks.push(match &home {
        Some(dir) => Check::new(Level::Ok, "home", dir.display().to_string()),
        None => Check::new(Level::Fail, "home", "cannot resolve the home directory")
            .with_hint("set HOME (unix) or USERPROFILE (windows)"),
    });

    let config = match Config::load() {
        Ok((config, path)) => {
            checks.push(Check::new(Level::Ok, "config", path.display().to_string()));
            checks.push(Check::new(
                Level::Ok,
                "prompt",
                format!("{} characters", config.system_prompt.chars().count()),
            ));
            checks.push(Check::new(Level::Ok, "API key", "found"));
            Some(config)
        }
        Err(e) => {
            let text = e.to_string();
            let missing_key = text.contains("no API key");

            checks.push(
                Check::new(
                    if missing_key {
                        Level::Warn
                    } else {
                        Level::Fail
                    },
                    if missing_key { "API key" } else { "config" },
                    text,
                )
                .with_hint(
                    "set JUMABEK_API_KEY, or copy secrets.toml.example to secrets.toml\n\
                     in your home directory and fill in [llm].api_key",
                ),
            );
            None
        }
    };

    checks.push(check_llm(config.as_ref()).await);
    checks.push(check_cargo().await);
    checks.push(check_docker().await);
    checks.push(check_ffmpeg().await);
    checks.push(check_skills());

    Ok(checks)
}

async fn check_llm(config: Option<&Config>) -> Check {
    let Some(config) = config else {
        return Check::new(Level::Warn, "LLM", "not checked — no usable config")
            .with_hint("fix the config first, then run jumabek doctor again");
    };

    let endpoint = format!("{}/v1/models", config.llm.base_uri.trim_end_matches('/'));
    let hint = "tested against OmniRoute; other OpenAI-compatible endpoints should work but \
                are untested\n\
                start one with:  npm i -g omniroute && omniroute serve\n\
                or point [llm].base_uri at your own endpoint";

    let client = match reqwest::Client::builder().timeout(PROBE_TIMEOUT).build() {
        Ok(client) => client,
        Err(e) => return Check::new(Level::Warn, "LLM", format!("cannot probe: {}", e)),
    };

    match client
        .get(&endpoint)
        .bearer_auth(&config.api_key)
        .send()
        .await
    {
        Ok(response) if response.status().is_success() => {
            let body = response.text().await.unwrap_or_default();
            let known = body.contains(&config.llm.model);
            if known {
                Check::new(
                    Level::Ok,
                    "LLM",
                    format!("{} · {}", config.llm.base_uri, config.llm.model),
                )
            } else {
                Check::new(
                    Level::Warn,
                    "LLM",
                    format!(
                        "{} is reachable but does not list '{}'",
                        config.llm.base_uri, config.llm.model
                    ),
                )
                .with_hint("check [llm].model against the endpoint's model list")
            }
        }
        Ok(response) => Check::new(
            Level::Warn,
            "LLM",
            format!("{} answered {}", config.llm.base_uri, response.status()),
        )
        .with_hint(hint),
        Err(_) => Check::new(
            Level::Warn,
            "LLM",
            format!("{} is not reachable", config.llm.base_uri),
        )
        .with_hint(hint),
    }
}

async fn check_cargo() -> Check {
    match probe("cargo", &["--version"]).await {
        Some(version) => Check::new(
            Level::Ok,
            "Rust",
            format!("{} — skills can be built", version),
        ),
        None => Check::new(Level::Warn, "Rust", "cargo not found").with_hint(
            "JumaBek runs, but cannot write itself new skills\n\
                 install from https://rustup.rs",
        ),
    }
}

async fn check_docker() -> Check {
    let availability = preflight::availability().await;
    if availability.usable {
        Check::new(Level::Ok, "Docker", &availability.detail)
    } else {
        Check::new(Level::Warn, "Docker", &availability.detail).with_hint(
            "new skills are checked in a container before they touch your machine;\n\
             without it building them is refused (or set allow_without_docker = true)",
        )
    }
}

async fn check_ffmpeg() -> Check {
    match probe("ffmpeg", &["-version"]).await {
        Some(version) => Check::new(Level::Ok, "ffmpeg", version),
        None => Check::new(Level::Warn, "ffmpeg", "not found")
            .with_hint("voice mode needs it for microphone capture; cli mode is unaffected"),
    }
}

fn check_skills() -> Check {
    let Some(dir) = loader::skills_dir() else {
        return Check::new(Level::Warn, "skills", "cannot resolve the skills directory");
    };

    match loader::discover(&dir) {
        Ok(found) if found.is_empty() => {
            Check::new(Level::Warn, "skills", format!("none in {}", dir.display())).with_hint(
                "JumaBek cannot do anything without at least one skill;\n\
                 the installer normally puts shell_executor there",
            )
        }
        Ok(found) => {
            let names: Vec<String> = found
                .iter()
                .filter_map(|p| p.file_stem()?.to_str().map(|s| s.to_string()))
                .collect();
            Check::new(
                Level::Ok,
                "skills",
                format!("{} installed: {}", names.len(), names.join(", ")),
            )
        }
        Err(e) => Check::new(Level::Warn, "skills", e.to_string()),
    }
}

async fn probe(program: &str, args: &[&str]) -> Option<String> {
    let output = tokio::time::timeout(
        PROBE_TIMEOUT,
        tokio::process::Command::new(program).args(args).output(),
    )
    .await
    .ok()?
    .ok()?;

    if !output.status.success() {
        return None;
    }

    let text = String::from_utf8_lossy(&output.stdout);
    Some(
        text.lines()
            .next()
            .unwrap_or_default()
            .trim()
            .chars()
            .take(60)
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_clean_report_has_no_warnings_or_failures() {
        let checks = vec![
            Check::new(Level::Ok, "a", "fine"),
            Check::new(Level::Ok, "b", "fine"),
        ];
        assert_eq!(summarise(&checks), (2, 0, 0));
    }

    #[test]
    fn levels_are_counted_separately() {
        let checks = vec![
            Check::new(Level::Ok, "a", "fine"),
            Check::new(Level::Warn, "b", "meh"),
            Check::new(Level::Warn, "c", "meh"),
            Check::new(Level::Fail, "d", "broken"),
        ];
        assert_eq!(summarise(&checks), (1, 2, 1));
    }

    #[tokio::test]
    async fn a_missing_program_is_reported_as_missing() {
        assert!(
            probe("definitely-not-a-real-program-xyz", &["--version"])
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn a_present_program_reports_its_version() {
        let version = probe("cargo", &["--version"]).await;
        assert!(version.is_some(), "cargo should be on PATH in this repo");
        assert!(version.unwrap().contains("cargo"));
    }
}
