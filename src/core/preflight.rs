use std::path::Path;
use std::time::Duration;

use tokio::process::Command;
use tokio::sync::OnceCell;

use crate::configs::PreflightSection;

const PROBE_TIMEOUT: Duration = Duration::from_secs(20);
const CARGO_CACHE_VOLUME: &str = "jumabek-cargo-cache";
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

pub fn build_command(
    config: &PreflightSection,
    crate_dir: &Path,
    sdk_dir: &Path,
    module: &str,
) -> Command {
    let mut command = Command::new("docker");
    command.args(["run", "--rm"]);

    command.args(["--cpus", &config.build_cpu]);
    command.args(["--memory", &config.build_memory]);
    command.args(["--pids-limit", "512"]);

    mount_sources(&mut command, crate_dir, sdk_dir, module);
    command.args([
        "-v",
        &format!("{}:/usr/local/cargo/registry", CARGO_CACHE_VOLUME),
    ]);
    command.args(["-w", &crate_workdir(module)]);

    command.args(["-e", "CARGO_TERM_COLOR=never"]);
    command.arg(&config.image);
    command.args(["cargo", "build", "--release", "--quiet"]);

    command
}

pub fn validate_command(
    config: &PreflightSection,
    crate_dir: &Path,
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

    mount_sources(&mut command, crate_dir, sdk_dir, module);
    command.args(["-w", &crate_workdir(module)]);

    command.arg(&config.image);
    command.arg(format!(
        "{}/target/release/{}",
        crate_workdir(module),
        module
    ));

    command
}

fn crate_workdir(module: &str) -> String {
    format!("{}/workshop/{}", ROOT, module)
}

fn mount_sources(command: &mut Command, crate_dir: &Path, sdk_dir: &Path, module: &str) {
    command.args([
        "-v",
        &format!("{}:{}", crate_dir.display(), crate_workdir(module)),
    ]);
    command.args(["-v", &format!("{}:{}:ro", sdk_dir.display(), SDK_MOUNT)]);
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
        PreflightSection {
            enabled: true,
            image: "rust:1-slim".to_string(),
            build_cpu: "2".to_string(),
            build_memory: "2g".to_string(),
            run_cpu: "0.5".to_string(),
            run_memory: "256m".to_string(),
            build_timeout_sec: 600,
            allow_without_docker: false,
        }
    }

    fn build(config: &PreflightSection) -> Command {
        build_command(
            config,
            &PathBuf::from("/host/workshop/file_ops"),
            &PathBuf::from("/host/sdk"),
            "file_ops",
        )
    }

    fn validate(config: &PreflightSection, module: &str) -> Command {
        validate_command(
            config,
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
        let args = args_of(&build(&config()));
        assert!(!args.iter().any(|a| a == "none"), "build lost its network");
        assert!(args.contains(&"--rm".to_string()));
        assert!(args.windows(2).any(|w| w[0] == "--cpus" && w[1] == "2"));
    }

    #[test]
    fn the_build_container_reuses_the_cargo_cache() {
        let args = args_of(&build(&config()));
        assert!(
            args.iter().any(|a| a.starts_with(CARGO_CACHE_VOLUME)),
            "no cargo cache volume: {:?}",
            args
        );
    }

    #[test]
    fn the_validation_container_is_locked_down() {
        let args = args_of(&validate(&config(), "file_ops"));

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
                "missing {}: {:?}",
                expected,
                args
            );
        }
    }

    #[test]
    fn the_validation_container_keeps_stdin_open_for_the_protocol() {
        let args = args_of(&validate(&config(), "x"));
        assert!(args.contains(&"-i".to_string()), "no stdin: {:?}", args);
    }

    #[test]
    fn the_validation_container_runs_the_built_binary() {
        let args = args_of(&validate(&config(), "file_ops"));
        assert!(
            args.last().unwrap().ends_with("/target/release/file_ops"),
            "got: {:?}",
            args.last()
        );
    }

    #[test]
    fn the_sdk_is_mounted_where_the_manifest_expects_it() {
        for args in [
            args_of(&build(&config())),
            args_of(&validate(&config(), "file_ops")),
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
    fn limits_are_reported_in_plain_words() {
        let text = describe_limits(&config());
        assert!(text.contains("network none"));
        assert!(text.contains("read-only"));
    }
}
