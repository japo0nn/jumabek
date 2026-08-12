use std::time::Duration;

use colored::Colorize;

use crate::configs::{self, Config};
use crate::core::languages::Language;
use crate::core::preflight;
use crate::core::prompt_version;
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
            checks.push(check_prompt(&config));
            checks.push(check_api_key(&config));
            Some(config)
        }
        Err(e) => {
            checks.push(Check::new(Level::Fail, "config", e.to_string()));
            None
        }
    };

    checks.push(check_llm(config.as_ref()).await);
    if let Some(config) = config.as_ref() {
        checks.push(check_intelligence(config));
    }
    for language in Language::ALL {
        checks.push(check_language(language).await);
    }
    checks.push(check_docker().await);
    checks.push(check_ffmpeg().await);
    checks.push(check_skills());

    Ok(checks)
}

/// A missing key no longer stops the agent starting, so this is the place that has to say it
/// out loud.
fn check_api_key(config: &Config) -> Check {
    if !config.api_key.is_empty() {
        return Check::new(Level::Ok, "API key", "found");
    }

    Check::new(
        Level::Ok,
        "API key",
        "none set — assuming the endpoint wants none",
    )
    .with_hint(
        "right for Ollama, LM Studio and llama.cpp, which ignore it.\n\
         For anything that does want one: set JUMABEK_API_KEY, or put it under\n\
         [llm].api_key in secrets.toml — otherwise the endpoint answers 401.",
    )
}

fn check_intelligence(config: &Config) -> Check {
    let levels = &config.llm.intelligence;
    let problems = levels.problems();

    if !problems.is_empty() {
        return Check::new(Level::Warn, "intelligence", problems.join("; ")).with_hint(
            "name all three of low, medium and high under [llm.intelligence],\n\
             or none of them",
        );
    }

    if !levels.enabled() {
        return Check::new(Level::Ok, "intelligence", "one model for everything").with_hint(
            "name low, medium and high under [llm.intelligence] to let JumaBek\n\
             pick a model to match the task",
        );
    }

    let named: Vec<String> = crate::core::intelligence::Level::ALL
        .iter()
        .map(|level| format!("{} {}", level, levels.model(*level)))
        .collect();

    Check::new(
        Level::Ok,
        "intelligence",
        format!(
            "{} · starts at {}",
            named.join(" · "),
            levels.starting_level()
        ),
    )
}

fn models_in_use(config: &Config) -> Vec<String> {
    let levels = &config.llm.intelligence;

    if !levels.enabled() {
        return vec![config.llm.model.clone()];
    }

    let mut names: Vec<String> = Vec::new();
    for level in crate::core::intelligence::Level::ALL {
        let model = levels.model(level).to_string();
        if !names.contains(&model) {
            names.push(model);
        }
    }
    names
}

async fn check_llm(config: Option<&Config>) -> Check {
    let Some(config) = config else {
        return Check::new(Level::Warn, "LLM", "not checked — no usable config")
            .with_hint("fix the config first, then run jumabek doctor again");
    };

    let endpoint = crate::core::llm::models_endpoint(&config.llm.base_uri);
    let hint = "point [llm].base_uri at any OpenAI-compatible endpoint: a local runner such\n\
                as Ollama or LM Studio, a router in front of several providers, or a\n\
                provider directly. An endpoint that wants no API key needs none.";

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
            let missing = models_in_use(config)
                .into_iter()
                .filter(|model| !body.contains(model))
                .collect::<Vec<_>>();

            if missing.is_empty() {
                Check::new(
                    Level::Ok,
                    "LLM",
                    format!(
                        "{} · {}",
                        config.llm.base_uri,
                        models_in_use(config).join(", ")
                    ),
                )
            } else {
                Check::new(
                    Level::Warn,
                    "LLM",
                    format!(
                        "{} is reachable but does not list {}",
                        config.llm.base_uri,
                        missing
                            .iter()
                            .map(|m| format!("'{}'", m))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                )
                .with_hint("check the model names against the endpoint's own list")
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

/// One line per language a skill could be written in.
async fn check_language(language: Language) -> Check {
    let mut found = None;
    for candidate in language.runtimes() {
        if let Some(version) = probe(candidate, &["--version"]).await {
            found = Some((candidate, version));
            break;
        }
    }

    let mut missing: Vec<&str> = Vec::new();
    if found.is_none() {
        missing.push(language.runtimes()[0]);
    }
    for tool in language.extra_tools() {
        if probe(tool, &["--version"]).await.is_none() {
            missing.push(tool);
        }
    }

    match (found, missing.is_empty()) {
        (Some((_, version)), true) => Check::new(
            Level::Ok,
            language.label(),
            format!("{} — skills can be written in {}", version, language),
        ),
        (_, _) => Check::new(
            Level::Warn,
            language.label(),
            format!("{} not found", missing.join(", ")),
        )
        .with_hint(format!(
            "JumaBek runs, and can still write skills in the other languages\n\
             {}",
            language.install_hint()
        )),
    }
}

fn check_prompt(config: &Config) -> Check {
    let characters = config.system_prompt.chars().count();
    let path = &config.system_prompt_file;

    match prompt_version::reconcile(path) {
        prompt_version::Status::InSync => Check::new(
            Level::Ok,
            "prompt",
            format!("{} characters, matches this build", characters),
        ),
        prompt_version::Status::Updated => Check::new(
            Level::Ok,
            "prompt",
            format!("updated to {}", prompt_version::VERSION),
        ),
        prompt_version::Status::LocalEdits => Check::new(
            Level::Ok,
            "prompt",
            format!("{} characters, with your edits", characters),
        ),
        prompt_version::Status::BaselineRecorded => Check::new(
            Level::Ok,
            "prompt",
            format!("{} characters, baseline recorded", characters),
        )
        .with_hint(
            "there was nothing to compare against, so this build's prompt was recorded\n\
             as the baseline; the next release will be able to tell what moved",
        ),
        prompt_version::Status::NeedsMerge { base } => Check::new(
            Level::Warn,
            "prompt",
            format!("older than this build ({})", prompt_version::VERSION),
        )
        .with_hint(format!(
            "your prompt.md has local edits and the shipped one has moved since.\n\
             Anything the new prompt describes and yours does not is invisible to\n\
             the model. Compare:\n  {}\n  {}",
            path.display(),
            base.display()
        )),
        prompt_version::Status::Unreadable(detail) => {
            Check::new(Level::Fail, "prompt", detail).with_hint("the agent cannot start without it")
        }
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
