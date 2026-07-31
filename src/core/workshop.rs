use std::path::{Path, PathBuf};

use crate::configs;
use crate::error::{JumabekError, JumabekResult};

const SDK_CARGO: &str = include_str!("../../jumabek_sdk/Cargo.toml");
const SDK_LIB: &str = include_str!("../../jumabek_sdk/src/lib.rs");
const SDK_PROTOCOL: &str = include_str!("../../jumabek_sdk/src/protocol.rs");
const SDK_RUNTIME: &str = include_str!("../../jumabek_sdk/src/runtime.rs");

pub fn root() -> JumabekResult<PathBuf> {
    configs::jumabek_dir()
        .ok_or_else(|| JumabekError::ConfigError("cannot resolve home directory".to_string()))
}

pub fn workshop_dir() -> JumabekResult<PathBuf> {
    Ok(root()?.join("workshop"))
}

pub fn sdk_dir() -> JumabekResult<PathBuf> {
    Ok(root()?.join("sdk"))
}

pub fn skills_dir() -> JumabekResult<PathBuf> {
    Ok(root()?.join("skills"))
}

pub fn ensure_sdk() -> JumabekResult<PathBuf> {
    let dir = sdk_dir()?;
    std::fs::create_dir_all(dir.join("src"))?;

    write_if_changed(&dir.join("Cargo.toml"), SDK_CARGO)?;
    write_if_changed(&dir.join("src/lib.rs"), SDK_LIB)?;
    write_if_changed(&dir.join("src/protocol.rs"), SDK_PROTOCOL)?;
    write_if_changed(&dir.join("src/runtime.rs"), SDK_RUNTIME)?;

    Ok(dir)
}

fn write_if_changed(path: &Path, contents: &str) -> JumabekResult<()> {
    if let Ok(existing) = std::fs::read_to_string(path)
        && existing == contents
    {
        return Ok(());
    }
    std::fs::write(path, contents)?;
    Ok(())
}

pub fn is_valid_module_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name.chars().next().is_some_and(|c| c.is_ascii_lowercase())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

pub fn cargo_manifest(module_name: &str, dependencies: &[String]) -> String {
    let mut extra = String::new();
    for dependency in dependencies {
        if let Some(line) = dependency_line(dependency) {
            extra.push_str(&line);
            extra.push('\n');
        }
    }

    format!(
        r#"[package]
name = "{name}"
version = "0.1.0"
edition = "2024"

[dependencies]
jumabek_sdk = {{ path = "../../sdk" }}
tokio = {{ version = "1", features = ["full"] }}
async-trait = "0.1"
serde_json = "1"
{extra}
[profile.release]
strip = true
"#,
        name = module_name,
        extra = extra
    )
}

/// A dependency as the model may write it. Plain `name@version` covers most
/// crates; features matter more than they look, because the default features of
/// an HTTP client usually drag in OpenSSL, which the build container does not
/// have. Without a way to say `rustls-tls`, such a skill simply cannot build.
fn dependency_line(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }

    if trimmed.starts_with('{') {
        return dependency_from_json(trimmed);
    }

    let (head, features) = match trimmed.split_once('+') {
        Some((head, features)) => (head.trim(), parse_features(features)),
        None => (trimmed, Vec::new()),
    };

    let (name, version) = match head.split_once('@') {
        Some((name, version)) => (name.trim(), version.trim()),
        None => (head, "*"),
    };

    build_line(name, version, &features, features.is_empty())
}

fn dependency_from_json(raw: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(raw).ok()?;

    let name = value.get("name")?.as_str()?;
    let version = value.get("version").and_then(|v| v.as_str()).unwrap_or("*");

    let features: Vec<String> = value
        .get("features")
        .and_then(|f| f.as_array())
        .map(|list| {
            list.iter()
                .filter_map(|f| f.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();

    let defaults = value
        .get("default_features")
        .or_else(|| value.get("default-features"))
        .and_then(|d| d.as_bool())
        .unwrap_or(features.is_empty());

    build_line(name, version, &features, defaults)
}

fn parse_features(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(|f| f.trim().to_string())
        .filter(|f| {
            !f.is_empty()
                && f.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        })
        .collect()
}

fn build_line(name: &str, version: &str, features: &[String], defaults: bool) -> Option<String> {
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return None;
    }

    if matches!(name, "jumabek_sdk" | "tokio" | "async-trait" | "serde_json") {
        return None;
    }

    if !version
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '*' | '^' | '~' | '='))
    {
        return None;
    }

    if features.is_empty() && defaults {
        return Some(format!("{} = \"{}\"", name, version));
    }

    let listed = features
        .iter()
        .map(|f| format!("\"{}\"", f))
        .collect::<Vec<_>>()
        .join(", ");

    Some(format!(
        "{} = {{ version = \"{}\", features = [{}], default-features = {} }}",
        name, version, listed, defaults
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_dependency_is_one_line() {
        assert_eq!(
            dependency_line("regex@1"),
            Some("regex = \"1\"".to_string())
        );
        assert_eq!(dependency_line("regex"), Some("regex = \"*\"".to_string()));
    }

    #[test]
    fn features_can_be_asked_for() {
        let line = dependency_line("reqwest@0.12+json,rustls-tls").unwrap();

        assert!(
            line.contains("features = [\"json\", \"rustls-tls\"]"),
            "{line}"
        );
        assert!(
            line.contains("default-features = false"),
            "asking for features has to drop the defaults, or openssl comes back: {line}"
        );
    }

    #[test]
    fn the_json_form_gives_full_control() {
        let line = dependency_line(
            r#"{"name":"reqwest","version":"0.12","features":["json"],"default_features":true}"#,
        )
        .unwrap();

        assert!(line.contains("default-features = true"), "{line}");
        assert!(line.contains("\"json\""), "{line}");
    }

    #[test]
    fn a_crate_that_is_already_there_is_never_added_twice() {
        for already in ["tokio@1", "serde_json@1", "async-trait@0.1", "jumabek_sdk"] {
            assert_eq!(dependency_line(already), None, "{already} was added again");
        }
    }

    #[test]
    fn an_injected_version_is_refused() {
        assert_eq!(dependency_line("evil@1\"\nsomething = \"else"), None);
        assert_eq!(dependency_line("../../etc@1"), None);
    }

    #[test]
    fn a_rubbish_feature_is_dropped_not_written_out() {
        let line = dependency_line("regex@1+good,\"bad\",also_good").unwrap();

        assert!(line.contains("\"good\""), "{line}");
        assert!(line.contains("\"also_good\""), "{line}");
        assert!(
            !line.contains("bad"),
            "an unchecked feature reached the manifest: {line}"
        );
    }

    #[test]
    fn a_manifest_with_features_still_parses_as_toml() {
        let manifest = cargo_manifest("bot", &["reqwest@0.12+json,rustls-tls".to_string()]);
        toml::from_str::<toml::Value>(&manifest).expect("the manifest is not valid TOML");
    }

    #[test]
    fn accepts_sane_module_names() {
        for name in ["file_ops", "youtube_downloader", "skill2"] {
            assert!(is_valid_module_name(name), "rejected {name}");
        }
    }

    #[test]
    fn rejects_names_that_could_escape_the_workshop() {
        for name in [
            "",
            "../evil",
            "C:/windows/system32",
            "with space",
            "Upper",
            "2leading",
            "semi;colon",
            "dash-name",
        ] {
            assert!(!is_valid_module_name(name), "accepted {name}");
        }
    }

    #[test]
    fn manifest_points_at_the_vendored_sdk() {
        let manifest = cargo_manifest("file_ops", &[]);
        assert!(manifest.contains(r#"name = "file_ops""#));
        assert!(manifest.contains(r#"jumabek_sdk = { path = "../../sdk" }"#));
    }

    #[test]
    fn extra_dependencies_are_added_with_versions() {
        let manifest = cargo_manifest("x", &["reqwest@0.12".to_string(), "regex".to_string()]);
        assert!(manifest.contains(r#"reqwest = "0.12""#), "{manifest}");
        assert!(manifest.contains(r#"regex = "*""#), "{manifest}");
    }

    #[test]
    fn duplicate_and_malformed_dependencies_are_dropped() {
        let manifest = cargo_manifest(
            "x",
            &[
                "tokio@1".to_string(),
                "jumabek_sdk".to_string(),
                "evil; rm -rf /".to_string(),
                "  ".to_string(),
            ],
        );

        assert_eq!(manifest.matches("tokio =").count(), 1, "{manifest}");
        assert_eq!(manifest.matches("jumabek_sdk =").count(), 1, "{manifest}");
        assert!(!manifest.contains("rm -rf"), "{manifest}");
    }

    #[test]
    fn the_embedded_sdk_is_the_real_one() {
        assert!(SDK_LIB.contains("pub trait SkillModule"));
        assert!(SDK_RUNTIME.contains("pub async fn run_skill"));
        assert!(SDK_PROTOCOL.contains("pub struct SkillRequest"));
        assert!(SDK_CARGO.contains("jumabek_sdk"));
    }

    #[test]
    fn the_embedded_sdk_stands_on_its_own() {
        assert!(
            !SDK_CARGO.contains(".workspace = true"),
            "the SDK is unpacked outside this repository, where a workspace to inherit from \
             does not exist — every skill build would fail to resolve the manifest:\n{}",
            SDK_CARGO
        );
        assert!(SDK_CARGO.contains("edition = \""), "{}", SDK_CARGO);
        assert!(SDK_CARGO.contains("version = \""), "{}", SDK_CARGO);
    }

    #[test]
    fn the_embedded_sdk_version_keeps_up_with_the_agent() {
        let expected = format!("version = \"{}\"", env!("CARGO_PKG_VERSION"));
        assert!(
            SDK_CARGO.contains(&expected),
            "the SDK says a different version from the agent that ships it: expected {}\n{}",
            expected,
            SDK_CARGO
        );
    }
}
