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

fn dependency_line(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }

    let (name, version) = match trimmed.split_once('@') {
        Some((name, version)) => (name.trim(), version.trim()),
        None => (trimmed, "*"),
    };

    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return None;
    }

    if matches!(name, "jumabek_sdk" | "tokio" | "async-trait" | "serde_json") {
        return None;
    }

    Some(format!("{} = \"{}\"", name, version))
}

#[cfg(test)]
mod tests {
    use super::*;

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
