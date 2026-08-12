#[cfg(test)]
use std::path::Path;
use std::time::Duration;

use jumabek_sdk::protocol::SkillResponsePayload;

use crate::skill_layer::rpc_client::SkillRpcClient;

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(20);

/// A method name no skill would implement, used to learn what this particular skill's
/// dispatcher does with something it does not know.
const NONSENSE_METHOD: &str = "__jumabek_probe_no_such_method__";

/// Calling more than a handful would turn validation into a wait, and a skill that implements
/// the first four methods it declared and none of the rest is not the failure this is looking
/// for.
const SMOKE_LIMIT: usize = 4;

/// How far to go before believing a skill.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Depth {
    /// Does it start, name itself, and survive nonsense.
    Contract,
    /// Everything above, plus: does each method it declared actually exist.
    Smoke,
}

#[derive(Debug)]
pub struct Report {
    pub checks: Vec<(String, bool, String)>,
}

impl Report {
    pub fn passed(&self) -> bool {
        self.checks.iter().all(|(_, ok, _)| *ok)
    }

    pub fn failures(&self) -> Vec<String> {
        self.checks
            .iter()
            .filter(|(_, ok, _)| !ok)
            .map(|(name, _, detail)| format!("{}: {}", name, detail))
            .collect()
    }

    pub fn summary(&self) -> String {
        self.checks
            .iter()
            .map(|(name, ok, detail)| {
                let mark = if *ok { "ok" } else { "FAILED" };
                format!("  {} {} — {}", mark, name, detail)
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn add(&mut self, name: &str, ok: bool, detail: impl Into<String>) {
        self.checks.push((name.to_string(), ok, detail.into()));
    }
}

#[cfg(test)]
pub async fn validate(binary: &Path, expected_name: &str) -> Report {
    validate_command(
        tokio::process::Command::new(binary),
        &binary.display().to_string(),
        expected_name,
        Depth::Contract,
    )
    .await
}

pub async fn validate_command(
    command: tokio::process::Command,
    label: &str,
    expected_name: &str,
    depth: Depth,
) -> Report {
    let mut report = Report { checks: Vec::new() };

    let client = match tokio::time::timeout(
        HANDSHAKE_TIMEOUT,
        SkillRpcClient::spawn_command(command, label),
    )
    .await
    {
        Ok(Ok(client)) => {
            report.add(
                "starts and speaks the protocol",
                true,
                "handshake completed",
            );
            client
        }
        Ok(Err(e)) => {
            report.add("starts and speaks the protocol", false, e.to_string());
            return report;
        }
        Err(_) => {
            report.add(
                "starts and speaks the protocol",
                false,
                format!("no answer within {}s", HANDSHAKE_TIMEOUT.as_secs()),
            );
            return report;
        }
    };

    let metadata = client.get_metadata_cached();
    report.add(
        "reports the expected name",
        metadata.name == expected_name,
        format!("declared '{}', expected '{}'", metadata.name, expected_name),
    );

    report.add(
        "reports a version",
        !metadata.version.trim().is_empty(),
        format!("version '{}'", metadata.version),
    );

    report.add(
        "describes itself",
        metadata.description.trim().len() >= 10,
        format!("{} chars of description", metadata.description.trim().len()),
    );

    let methods = client.methods_cached();
    report.add(
        "exposes at least one method",
        !methods.is_empty(),
        format!("{} method(s)", methods.len()),
    );

    let documented = methods
        .iter()
        .all(|m| !m.method.trim().is_empty() && !m.description.trim().is_empty());
    report.add(
        "documents every method",
        documented,
        "each method needs a name and a description",
    );

    report.add("passes health_check", client.health_check_flag(), "alive");

    match tokio::time::timeout(HANDSHAKE_TIMEOUT, client.call("no_such_method", None)).await {
        Ok(Ok(response)) => report.add(
            "survives an unknown method",
            matches!(response.payload, SkillResponsePayload::Error(_)),
            "answered with an error instead of dying",
        ),
        Ok(Err(e)) => report.add("survives an unknown method", false, e.to_string()),
        Err(_) => report.add(
            "survives an unknown method",
            false,
            "stopped answering after a bad request",
        ),
    }

    let garbage = serde_json::json!({ "not": "execute params" }).to_string();
    match tokio::time::timeout(HANDSHAKE_TIMEOUT, client.call("execute", Some(garbage))).await {
        Ok(Ok(_)) => report.add(
            "survives malformed arguments",
            true,
            "answered instead of crashing",
        ),
        Ok(Err(e)) => report.add("survives malformed arguments", false, e.to_string()),
        Err(_) => report.add(
            "survives malformed arguments",
            false,
            "stopped answering after malformed arguments",
        ),
    }

    if depth == Depth::Smoke {
        let methods: Vec<String> = client
            .methods_cached()
            .iter()
            .take(SMOKE_LIMIT)
            .map(|m| m.method.clone())
            .collect();

        smoke(&client, &methods, &mut report).await;
    }

    let _ = client.shutdown().await;
    report
}

/// Does the skill implement what it said it implements?
async fn smoke(client: &SkillRpcClient, methods: &[String], report: &mut Report) {
    if methods.is_empty() {
        return;
    }

    let unknown = call_execute(client, NONSENSE_METHOD).await;
    let Some(Verdict::Unknown) = unknown else {
        report.add(
            "implements the methods it declares",
            true,
            "skipped: the skill does not report unknown methods separately",
        );
        return;
    };

    for method in methods {
        match call_execute(client, method).await {
            None => {
                report.add(
                    "implements the methods it declares",
                    false,
                    format!("'{}' stopped answering when it was called", method),
                );
                return;
            }
            Some(Verdict::Unknown) => {
                report.add(
                    "implements the methods it declares",
                    false,
                    format!(
                        "'{}' is declared but the skill answers it the same way it answers a \
                         method that does not exist — it was never wired into execute()",
                        method
                    ),
                );
                return;
            }
            Some(Verdict::Answered) => {}
        }
    }

    report.add(
        "implements the methods it declares",
        true,
        format!("{} method(s) answered for themselves", methods.len()),
    );
}

enum Verdict {
    /// The skill did not recognise the name.
    Unknown,
    /// Anything else — a result, or a failure that is about the work rather than about the
    /// name.
    Answered,
}

async fn call_execute(client: &SkillRpcClient, method: &str) -> Option<Verdict> {
    let params = serde_json::json!({ "method": method, "args": "" }).to_string();

    match tokio::time::timeout(HANDSHAKE_TIMEOUT, client.call("execute", Some(params))).await {
        Ok(Ok(response)) => Some(match response.payload {
            SkillResponsePayload::Error(jumabek_sdk::SkillError::NotFound(_)) => Verdict::Unknown,
            _ => Verdict::Answered,
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report_with(checks: Vec<(&str, bool)>) -> Report {
        Report {
            checks: checks
                .into_iter()
                .map(|(name, ok)| (name.to_string(), ok, "detail".to_string()))
                .collect(),
        }
    }

    #[test]
    fn a_report_passes_only_when_every_check_passed() {
        assert!(report_with(vec![("a", true), ("b", true)]).passed());
        assert!(!report_with(vec![("a", true), ("b", false)]).passed());
    }

    #[test]
    fn failures_are_listed_for_the_model_to_fix() {
        let report = report_with(vec![("good", true), ("bad", false)]);
        let failures = report.failures();
        assert_eq!(failures.len(), 1);
        assert!(failures[0].starts_with("bad:"));
    }

    /// The shipped skills land beside the test binary; `cargo test` builds them.
    fn built(name: &str) -> std::path::PathBuf {
        let mut dir = std::env::current_exe().expect("test executable has a path");
        dir.pop();
        if dir.ends_with("deps") {
            dir.pop();
        }

        dir.join(if cfg!(windows) {
            format!("{}.exe", name)
        } else {
            name.to_string()
        })
    }

    async fn report_for(name: &str, depth: Depth) -> Report {
        let binary = built(name);
        assert!(
            binary.is_file(),
            "{} must be built for this test — run cargo build --workspace first",
            binary.display()
        );

        validate_command(
            tokio::process::Command::new(&binary),
            &binary.display().to_string(),
            name,
            depth,
        )
        .await
    }

    #[tokio::test]
    async fn a_skill_that_implements_what_it_declares_passes() {
        let report = report_for("shell_executor", Depth::Smoke).await;

        assert!(
            report.passed(),
            "a working skill was rejected:\n{}",
            report.summary()
        );
    }

    #[test]
    fn a_missing_binary_fails_at_the_first_check() {
        let report = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(validate(Path::new("definitely/not/here.exe"), "ghost"));

        assert!(!report.passed());
        assert_eq!(report.checks.len(), 1, "kept probing a dead binary");
    }
}
