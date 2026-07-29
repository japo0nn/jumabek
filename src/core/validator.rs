use std::path::Path;
use std::time::Duration;

use jumabek_sdk::protocol::SkillResponsePayload;

use crate::skill_layer::rpc_client::SkillRpcClient;

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(20);

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

pub async fn validate(binary: &Path, expected_name: &str) -> Report {
    validate_command(
        tokio::process::Command::new(binary),
        &binary.display().to_string(),
        expected_name,
    )
    .await
}

pub async fn validate_command(
    command: tokio::process::Command,
    label: &str,
    expected_name: &str,
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

    let _ = client.shutdown().await;
    report
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

    #[test]
    fn a_missing_binary_fails_at_the_first_check() {
        let report = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(validate(Path::new("definitely/not/here.exe"), "ghost"));

        assert!(!report.passed());
        assert_eq!(report.checks.len(), 1, "kept probing a dead binary");
    }
}
