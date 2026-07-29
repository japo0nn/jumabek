use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::process::Command;
use tokio::sync::Mutex;

use crate::configs::PreflightSection;
use crate::core::chunks::{ChunkBuffers, ChunkOutcome};
use crate::core::preflight;
use crate::core::validator;
use crate::core::workshop;
use crate::error::{JumabekError, JumabekResult};
use crate::supervisor::Supervisor;

const BUILD_TIMEOUT: Duration = Duration::from_secs(600);

pub struct Chunk<'a> {
    pub module: &'a str,
    pub index: u32,
    pub total: u32,
    pub code: &'a str,
    pub dependencies: &'a [String],
}

pub enum Progress {
    Buffered { received: u32, total: u32 },
    Rejected(String),
    Built(Outcome),
}

pub enum Outcome {
    GaveUp {
        attempts: u32,
        last_error: String,
    },
    Deployed {
        path: PathBuf,
        report: String,
        preflight: String,
    },
    CompileFailed(String),
    ValidationFailed(String),
    PreflightUnavailable(String),
}

pub struct SelfImprovement {
    buffers: Mutex<ChunkBuffers>,
    approved: Mutex<HashSet<String>>,
    attempts: Mutex<HashMap<String, u32>>,
}

impl SelfImprovement {
    pub fn new() -> Self {
        SelfImprovement {
            buffers: Mutex::new(ChunkBuffers::new()),
            approved: Mutex::new(HashSet::new()),
            attempts: Mutex::new(HashMap::new()),
        }
    }

    pub async fn attempts_for(&self, module: &str) -> u32 {
        self.attempts.lock().await.get(module).copied().unwrap_or(0)
    }

    async fn record_failure(&self, module: &str) -> u32 {
        let mut attempts = self.attempts.lock().await;
        let counter = attempts.entry(module.to_string()).or_insert(0);
        *counter += 1;
        *counter
    }

    async fn clear_failures(&self, module: &str) {
        self.attempts.lock().await.remove(module);
    }

    pub async fn is_approved(&self, module: &str) -> bool {
        self.approved.lock().await.contains(module)
    }

    pub async fn forget(&self, module: &str) {
        self.buffers.lock().await.forget(module);
    }

    pub async fn approve(&self, module: &str) {
        self.approved.lock().await.insert(module.to_string());
    }

    pub async fn accept_chunk(
        &self,
        config: &PreflightSection,
        max_fix_iterations: u32,
        chunk: Chunk<'_>,
    ) -> JumabekResult<Progress> {
        let module = chunk.module;

        if !workshop::is_valid_module_name(module) {
            return Ok(Progress::Rejected(format!(
                "'{}' is not a usable module name: use lowercase letters, digits and underscores, \
                 starting with a letter",
                module
            )));
        }

        let outcome = {
            let mut buffers = self.buffers.lock().await;
            buffers.push(
                module,
                chunk.index,
                chunk.total,
                chunk.code,
                chunk.dependencies,
            )
        };

        match outcome {
            ChunkOutcome::Buffered { received, total } => {
                Ok(Progress::Buffered { received, total })
            }
            ChunkOutcome::Rejected(reason) => Ok(Progress::Rejected(reason)),
            ChunkOutcome::Complete { code, dependencies } => {
                let outcome = self.build(config, module, &code, &dependencies).await?;

                let failure = match &outcome {
                    Outcome::CompileFailed(reason) | Outcome::ValidationFailed(reason) => {
                        Some(reason.clone())
                    }
                    _ => None,
                };

                let Some(reason) = failure else {
                    self.clear_failures(module).await;
                    return Ok(Progress::Built(outcome));
                };

                let attempts = self.record_failure(module).await;
                if attempts >= max_fix_iterations {
                    self.clear_failures(module).await;
                    self.forget(module).await;
                    return Ok(Progress::Built(Outcome::GaveUp {
                        attempts,
                        last_error: reason,
                    }));
                }

                Ok(Progress::Built(outcome))
            }
        }
    }

    async fn build(
        &self,
        config: &PreflightSection,
        module: &str,
        code: &str,
        dependencies: &[String],
    ) -> JumabekResult<Outcome> {
        let sdk_dir = workshop::ensure_sdk()?;

        let crate_dir = workshop::workshop_dir()?.join(module);
        std::fs::create_dir_all(crate_dir.join("src"))?;
        std::fs::write(
            crate_dir.join("Cargo.toml"),
            workshop::cargo_manifest(module, dependencies),
        )?;
        std::fs::write(crate_dir.join("src/main.rs"), code)?;

        let preflight_note = match self.preflight(config, module, &crate_dir, &sdk_dir).await? {
            Ok(note) => note,
            Err(outcome) => return Ok(outcome),
        };

        let compiled = match compile(&crate_dir).await? {
            Ok(path) => path,
            Err(stderr) => return Ok(Outcome::CompileFailed(stderr)),
        };

        let report = validator::validate(&compiled, module).await;
        if !report.passed() {
            return Ok(Outcome::ValidationFailed(format!(
                "{}\n\nFailed checks:\n  {}",
                report.summary(),
                report.failures().join("\n  ")
            )));
        }

        let destination = workshop::skills_dir()?.join(binary_name(module));
        std::fs::create_dir_all(destination.parent().unwrap())?;

        if let Ok(supervisor) = Supervisor::open() {
            let reason = if destination.exists() {
                format!("before-replacing-{}", module)
            } else {
                format!("before-adding-{}", module)
            };
            if let Err(e) = supervisor.snapshot(&reason) {
                eprintln!("[supervisor] could not snapshot before deploy: {}", e);
            }
        }

        replace_binary(&compiled, &destination)?;

        Ok(Outcome::Deployed {
            path: destination,
            report: report.summary(),
            preflight: preflight_note,
        })
    }

    async fn preflight(
        &self,
        config: &PreflightSection,
        module: &str,
        crate_dir: &Path,
        sdk_dir: &Path,
    ) -> JumabekResult<Result<String, Outcome>> {
        if !config.enabled {
            return Ok(Ok("skipped: disabled in config".to_string()));
        }

        let availability = preflight::availability().await;
        if !availability.usable {
            if config.allow_without_docker {
                return Ok(Ok(format!("skipped: {}", availability.detail)));
            }
            return Ok(Err(Outcome::PreflightUnavailable(
                availability.detail.clone(),
            )));
        }

        let build = tokio::time::timeout(
            Duration::from_secs(config.build_timeout_sec),
            preflight::build_command(config, crate_dir, sdk_dir, module).output(),
        )
        .await
        .map_err(|_| {
            JumabekError::InternalError(format!(
                "preflight build did not finish within {}s",
                config.build_timeout_sec
            ))
        })?
        .map_err(|e| JumabekError::InternalError(format!("cannot run docker: {}", e)))?;

        if !build.status.success() {
            let stderr = String::from_utf8_lossy(&build.stderr);
            return Ok(Err(Outcome::CompileFailed(format!(
                "the code did not compile in the preflight container\n{}",
                trim_compiler_output(&stderr)
            ))));
        }

        let command = preflight::validate_command(config, crate_dir, sdk_dir, module);
        let report =
            validator::validate_command(command, &format!("{} (container)", module), module).await;

        if !report.passed() {
            return Ok(Err(Outcome::ValidationFailed(format!(
                "the skill misbehaved inside the preflight container ({})\n{}\n\nFailed checks:\n  {}",
                preflight::describe_limits(config),
                report.summary(),
                report.failures().join("\n  ")
            ))));
        }

        Ok(Ok(format!(
            "passed in {} — {}",
            availability.detail,
            preflight::describe_limits(config)
        )))
    }
}

impl Default for SelfImprovement {
    fn default() -> Self {
        Self::new()
    }
}

async fn compile(crate_dir: &Path) -> JumabekResult<Result<PathBuf, String>> {
    let output = tokio::time::timeout(
        BUILD_TIMEOUT,
        Command::new("cargo")
            .args(["build", "--release", "--quiet"])
            .current_dir(crate_dir)
            .output(),
    )
    .await
    .map_err(|_| {
        JumabekError::InternalError(format!(
            "cargo build did not finish within {}s",
            BUILD_TIMEOUT.as_secs()
        ))
    })?
    .map_err(|e| {
        JumabekError::InternalError(format!(
            "cannot run cargo: {} — is the Rust toolchain installed?",
            e
        ))
    })?;

    if !output.status.success() {
        return Ok(Err(trim_compiler_output(&String::from_utf8_lossy(
            &output.stderr,
        ))));
    }

    let name = crate_dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();

    let built = crate_dir.join("target/release").join(binary_name(name));

    if !built.exists() {
        return Ok(Err(format!(
            "cargo reported success but {} is missing",
            built.display()
        )));
    }

    Ok(Ok(built))
}

fn binary_name(module: &str) -> String {
    if cfg!(windows) {
        format!("{}.exe", module)
    } else {
        module.to_string()
    }
}

fn replace_binary(source: &Path, destination: &Path) -> JumabekResult<()> {
    if destination.exists() {
        let backup = destination.with_extension("previous");
        let _ = std::fs::remove_file(&backup);
        std::fs::rename(destination, &backup)?;
    }

    std::fs::copy(source, destination)?;
    Ok(())
}

pub fn trim_compiler_output(stderr: &str) -> String {
    let errors: Vec<&str> = stderr
        .lines()
        .skip_while(|line| !line.starts_with("error"))
        .take(60)
        .collect();

    let text = if errors.is_empty() {
        let mut tail: Vec<&str> = stderr.lines().rev().take(30).collect();
        tail.reverse();
        tail.join("\n")
    } else {
        errors.join("\n")
    };

    text.trim().chars().take(4000).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compiler_output_keeps_the_errors_and_drops_the_noise() {
        let stderr = "\
   Compiling foo v0.1.0
   Compiling bar v0.2.0
warning: unused variable: `x`
error[E0425]: cannot find value `undefined_thing` in this scope
  --> src/main.rs:4:5
   |
 4 |     undefined_thing();
   |     ^^^^^^^^^^^^^^^ not found in this scope
error: aborting due to 1 previous error";

        let trimmed = trim_compiler_output(stderr);
        assert!(trimmed.starts_with("error[E0425]"), "got: {trimmed}");
        assert!(trimmed.contains("undefined_thing"));
        assert!(!trimmed.contains("Compiling foo"));
    }

    #[test]
    fn output_without_errors_still_says_something() {
        let trimmed = trim_compiler_output("linker failed\nsome detail");
        assert!(trimmed.contains("linker failed"), "got: {trimmed}");
    }

    #[test]
    fn compiler_output_is_capped() {
        let huge = (0..5000)
            .map(|i| format!("error: line {}", i))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(trim_compiler_output(&huge).chars().count() <= 4000);
    }

    #[test]
    fn binary_name_matches_the_platform() {
        let name = binary_name("file_ops");
        if cfg!(windows) {
            assert_eq!(name, "file_ops.exe");
        } else {
            assert_eq!(name, "file_ops");
        }
    }

    #[tokio::test]
    async fn a_bad_module_name_never_reaches_the_filesystem() {
        let engine = SelfImprovement::new();
        let progress = engine
            .accept_chunk(
                &PreflightSection::default(),
                5,
                Chunk {
                    module: "../escape",
                    index: 1,
                    total: 1,
                    code: "fn main() {}",
                    dependencies: &[],
                },
            )
            .await
            .unwrap();

        assert!(matches!(progress, Progress::Rejected(_)));
    }

    #[tokio::test]
    async fn chunks_are_buffered_until_the_last_one() {
        let engine = SelfImprovement::new();
        let progress = engine
            .accept_chunk(
                &PreflightSection::default(),
                5,
                Chunk {
                    module: "good_name",
                    index: 1,
                    total: 3,
                    code: "// part",
                    dependencies: &[],
                },
            )
            .await
            .unwrap();

        match progress {
            Progress::Buffered { received, total } => {
                assert_eq!((received, total), (1, 3));
            }
            _ => panic!("expected the chunk to be buffered"),
        }
    }
}

#[cfg(test)]
mod budget_tests {
    use super::*;

    fn broken_code() -> &'static str {
        "fn main() { this is not rust }"
    }

    fn chunk<'a>(module: &'a str, code: &'a str) -> Chunk<'a> {
        Chunk {
            module,
            index: 1,
            total: 1,
            code,
            dependencies: &[],
        }
    }

    fn no_preflight() -> PreflightSection {
        PreflightSection {
            enabled: false,
            ..PreflightSection::default()
        }
    }

    #[tokio::test]
    async fn a_module_gets_a_limited_number_of_tries() {
        let engine = SelfImprovement::new();
        let config = no_preflight();
        let budget = 3;

        let mut outcomes = Vec::new();
        for _ in 0..budget {
            let progress = engine
                .accept_chunk(&config, budget, chunk("doomed_module", broken_code()))
                .await
                .unwrap();

            match progress {
                Progress::Built(outcome) => outcomes.push(outcome),
                other => panic!(
                    "expected a build attempt, got something else: {}",
                    matches!(other, Progress::Buffered { .. })
                ),
            }
        }

        assert!(
            matches!(outcomes[0], Outcome::CompileFailed(_)),
            "the first failure should just be a failure"
        );

        match outcomes.last().unwrap() {
            Outcome::GaveUp { attempts, .. } => assert_eq!(*attempts, budget),
            other => panic!(
                "the budget was never enforced: {}",
                matches!(other, Outcome::CompileFailed(_))
            ),
        }
    }

    #[tokio::test]
    async fn giving_up_clears_the_counter_so_a_later_attempt_starts_fresh() {
        let engine = SelfImprovement::new();
        let config = no_preflight();

        for _ in 0..2 {
            let _ = engine
                .accept_chunk(&config, 2, chunk("second_chance", broken_code()))
                .await
                .unwrap();
        }

        assert_eq!(
            engine.attempts_for("second_chance").await,
            0,
            "the counter kept counting after the module was abandoned"
        );
    }

    #[tokio::test]
    async fn failures_are_counted_per_module() {
        let engine = SelfImprovement::new();
        let config = no_preflight();

        let _ = engine
            .accept_chunk(&config, 5, chunk("alpha_module", broken_code()))
            .await
            .unwrap();

        assert_eq!(engine.attempts_for("alpha_module").await, 1);
        assert_eq!(
            engine.attempts_for("beta_module").await,
            0,
            "one module's failures were charged to another"
        );
    }
}
