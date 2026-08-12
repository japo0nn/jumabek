use std::path::Path;
use std::time::Duration;

use tokio::process::Command;
use tokio::sync::OnceCell;

use crate::configs::PreflightSection;
use crate::core::languages::Language;

const PROBE_TIMEOUT: Duration = Duration::from_secs(20);
const ROOT: &str = "/build";
const SDK_MOUNT: &str = "/build/sdk";

#[derive(Debug, Clone)]
pub struct Availability {
    pub usable: bool,
    pub detail: String,
}

static AVAILABILITY: OnceCell<Availability> = OnceCell::const_new();

pub async fn availability() -> &'static Availability {
    AVAILABILITY.get_or_init(probe).await
}

async fn probe() -> Availability {
    let output = tokio::time::timeout(
        PROBE_TIMEOUT,
        Command::new("docker")
            .args(["info", "--format", "{{.ServerVersion}}"])
            .output(),
    )
    .await;

    match output {
        Err(_) => Availability {
            usable: false,
            detail: "docker did not answer within 20s".to_string(),
        },
        Ok(Err(e)) => Availability {
            usable: false,
            detail: format!("docker command not found: {}", e),
        },
        Ok(Ok(result)) if result.status.success() => {
            let version = String::from_utf8_lossy(&result.stdout).trim().to_string();
            Availability {
                usable: true,
                detail: format!("docker engine {}", version),
            }
        }
        Ok(Ok(result)) => {
            let stderr = String::from_utf8_lossy(&result.stderr);
            let detail = if stderr.contains("pipe") || stderr.contains("daemon") {
                "docker is installed but the engine is not running — start Docker Desktop"
                    .to_string()
            } else {
                format!("docker is not usable: {}", stderr.trim())
            };
            Availability {
                usable: false,
                detail,
            }
        }
    }
}

/// One step of the build, in the language's own image. There may be more than
/// one — installing dependencies and checking the source are separate jobs, and
/// keeping them apart is what makes "it did not compile" distinguishable from
/// "that package does not exist".
pub fn build_commands(
    config: &PreflightSection,
    language: Language,
    skill_dir: &Path,
    sdk_dir: &Path,
    module: &str,
    has_manifest: bool,
) -> Vec<Command> {
    language
        .build_steps(has_manifest, language.container_runtime())
        .into_iter()
        .map(|step| {
            let mut command = Command::new("docker");
            command.args(["run", "--rm"]);

            command.args(["--cpus", &config.build_cpu]);
            command.args(["--memory", &config.build_memory]);
            command.args(["--pids-limit", "512"]);

            mount_sources(&mut command, language, skill_dir, sdk_dir, module);
            if let Some((volume, mount)) = language.cache() {
                command.args(["-v", &format!("{}:{}", volume, mount)]);
            }
            command.args(["-w", &skill_workdir(module)]);

            command.args(["-e", "CARGO_TERM_COLOR=never"]);
            command.arg(config.image_for(language));
            command.args(step);

            command
        })
        .collect()
}

/// The container the built skill is actually started in: no network, no
/// capabilities, read-only filesystem. It answers the protocol here or it does
/// not get installed.
pub fn validate_command(
    config: &PreflightSection,
    language: Language,
    skill_dir: &Path,
    sdk_dir: &Path,
    module: &str,
) -> Command {
    let mut command = Command::new("docker");
    command.args(["run", "--rm", "-i"]);

    command.args(["--network", "none"]);
    command.args(["--cpus", &config.run_cpu]);
    command.args(["--memory", &config.run_memory]);
    command.args(["--pids-limit", "64"]);
    command.args(["--cap-drop", "ALL"]);
    command.args(["--security-opt", "no-new-privileges"]);
    command.args(["--read-only"]);
    command.args(["--tmpfs", "/tmp:rw,size=64m"]);

    mount_sources(&mut command, language, skill_dir, sdk_dir, module);
    command.args(["-w", &skill_workdir(module)]);

    command.arg(config.image_for(language));
    command.args(language.start_argv(
        module,
        language.container_runtime(),
        Path::new(&skill_workdir(module)),
    ));

    command
}

fn skill_workdir(module: &str) -> String {
    format!("{}/workshop/{}", ROOT, module)
}

fn mount_sources(
    command: &mut Command,
    language: Language,
    skill_dir: &Path,
    sdk_dir: &Path,
    module: &str,
) {
    command.args([
        "-v",
        &format!("{}:{}", skill_dir.display(), skill_workdir(module)),
    ]);

    if language.needs_sdk() {
        command.args(["-v", &format!("{}:{}:ro", sdk_dir.display(), SDK_MOUNT)]);
    }
}

pub fn describe_limits(config: &PreflightSection) -> String {
    format!(
        "build: {} cpu / {} ram, network on · run: {} cpu / {} ram, network none, read-only",
        config.build_cpu, config.build_memory, config.run_cpu, config.run_memory
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn config() -> PreflightSection {
        PreflightSection::default()
    }

    fn build(config: &PreflightSection, language: Language) -> Vec<Command> {
        build_commands(
            config,
            language,
            &PathBuf::from("/host/workshop/file_ops"),
            &PathBuf::from("/host/sdk"),
            "file_ops",
            true,
        )
    }

    fn validate(config: &PreflightSection, language: Language, module: &str) -> Command {
        validate_command(
            config,
            language,
            &PathBuf::from("/host/workshop").join(module),
            &PathBuf::from("/host/sdk"),
            module,
        )
    }

    fn args_of(command: &Command) -> Vec<String> {
        command
            .as_std()
            .get_args()
            .map(|a| a.to_string_lossy().to_string())
            .collect()
    }

    #[test]
    fn the_build_container_may_reach_crates_io() {
        let commands = build(&config(), Language::Rust);
        let args = args_of(&commands[0]);

        assert!(!args.iter().any(|a| a == "none"), "build lost its network");
        assert!(args.contains(&"--rm".to_string()));
        assert!(args.windows(2).any(|w| w[0] == "--cpus" && w[1] == "2"));
    }

    #[test]
    fn every_language_reuses_its_package_cache() {
        for language in Language::ALL {
            let (volume, _) = language.cache().expect("no cache configured");
            let args = args_of(&build(&config(), language)[0]);

            assert!(
                args.iter().any(|a| a.starts_with(volume)),
                "{} downloads the world every time: {:?}",
                language,
                args
            );
        }
    }

    #[test]
    fn the_validation_container_is_locked_down_whatever_the_language() {
        for language in Language::ALL {
            let args = args_of(&validate(&config(), language, "file_ops"));

            for expected in [
                "--network",
                "none",
                "--cap-drop",
                "ALL",
                "--read-only",
                "no-new-privileges",
            ] {
                assert!(
                    args.iter().any(|a| a == expected),
                    "{} missing {}: {:?}",
                    language,
                    expected,
                    args
                );
            }
        }
    }

    #[test]
    fn the_validation_container_keeps_stdin_open_for_the_protocol() {
        for language in Language::ALL {
            let args = args_of(&validate(&config(), language, "x"));
            assert!(args.contains(&"-i".to_string()), "{}: {:?}", language, args);
        }
    }

    #[test]
    fn each_language_is_started_the_way_it_is_meant_to_be() {
        let rust = args_of(&validate(&config(), Language::Rust, "file_ops"));
        assert!(
            rust.last().unwrap().ends_with("/target/release/file_ops"),
            "got: {:?}",
            rust.last()
        );

        let python = args_of(&validate(&config(), Language::Python, "file_ops"));
        assert_eq!(
            &python[python.len() - 2..],
            &[
                "python3".to_string(),
                "/build/workshop/file_ops/main.py".to_string()
            ]
        );

        let node = args_of(&validate(&config(), Language::Node, "file_ops"));
        assert_eq!(
            &node[node.len() - 2..],
            &[
                "node".to_string(),
                "/build/workshop/file_ops/main.js".to_string()
            ]
        );
    }

    #[test]
    fn each_language_builds_in_its_own_image() {
        let config = config();

        for language in Language::ALL {
            let args = args_of(&build(&config, language)[0]);
            let image = config.image_for(language);

            assert!(
                args.contains(&image.to_string()),
                "{} was built in the wrong image, wanted {}: {:?}",
                language,
                image,
                args
            );
        }
    }

    #[test]
    fn only_rust_mounts_the_sdk() {
        for language in Language::ALL {
            let mounted = args_of(&build(&config(), language)[0])
                .iter()
                .any(|a| a.ends_with(":/build/sdk:ro"));

            assert_eq!(
                mounted,
                language.needs_sdk(),
                "{} mounts the Rust SDK it cannot use",
                language
            );
        }
    }

    #[test]
    fn the_sdk_is_mounted_where_the_manifest_expects_it() {
        for args in [
            args_of(&build(&config(), Language::Rust)[0]),
            args_of(&validate(&config(), Language::Rust, "file_ops")),
        ] {
            let workdir = args
                .windows(2)
                .find(|w| w[0] == "-w")
                .map(|w| w[1].clone())
                .expect("no workdir");

            assert_eq!(workdir, "/build/workshop/file_ops");
            assert!(
                args.iter().any(|a| a.ends_with(":/build/sdk:ro")),
                "sdk not mounted read-only: {:?}",
                args
            );
        }
    }

    #[test]
    fn a_language_with_dependencies_installs_before_it_checks() {
        let with = build_commands(
            &config(),
            Language::Python,
            Path::new("/host/workshop/x"),
            Path::new("/host/sdk"),
            "x",
            true,
        );
        let without = build_commands(
            &config(),
            Language::Python,
            Path::new("/host/workshop/x"),
            Path::new("/host/sdk"),
            "x",
            false,
        );

        assert_eq!(with.len(), 2, "no install step");
        assert_eq!(without.len(), 1, "installed dependencies nobody asked for");
        assert!(args_of(&with[0]).iter().any(|a| a == "pip"));
    }

    #[test]
    fn limits_are_reported_in_plain_words() {
        let text = describe_limits(&config());
        assert!(text.contains("network none"));
        assert!(text.contains("read-only"));
    }
}
