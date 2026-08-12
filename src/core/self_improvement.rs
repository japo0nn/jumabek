use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::process::Command;
use tokio::sync::Mutex;

use crate::configs::PreflightSection;
use crate::core::chunks::{ChunkBuffers, ChunkOutcome};
use crate::core::languages::Language;
use crate::core::preflight;
use crate::core::validator;
use crate::core::workshop;
use crate::error::{JumabekError, JumabekResult};
use crate::supervisor::Supervisor;

const BUILD_TIMEOUT: Duration = Duration::from_secs(600);
const TOOL_PROBE_TIMEOUT: Duration = Duration::from_secs(10);

pub struct Chunk<'a> {
    pub module: &'a str,
    pub index: u32,
    pub total: u32,
    pub code: &'a str,
    pub dependencies: &'a [String],
    pub language: Language,
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
    ToolchainMissing {
        language: Language,
        detail: String,
    },
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
                chunk.language,
            )
        };

        match outcome {
            ChunkOutcome::Buffered { received, total } => {
                Ok(Progress::Buffered { received, total })
            }
            ChunkOutcome::Rejected(reason) => Ok(Progress::Rejected(reason)),
            ChunkOutcome::Complete {
                code,
                dependencies,
                language,
            } => {
                let outcome = self
                    .build(config, module, language, &code, &dependencies)
                    .await?;

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
        language: Language,
        code: &str,
        dependencies: &[String],
    ) -> JumabekResult<Outcome> {
        let sdk_dir = if language.needs_sdk() {
            workshop::ensure_sdk()?
        } else {
            workshop::sdk_dir()?
        };

        let skill_dir = workshop::workshop_dir()?.join(module);
        let has_manifest = lay_out_sources(&skill_dir, module, language, code, dependencies)?;

        let preflight_note = match self
            .preflight(config, module, language, &skill_dir, &sdk_dir, has_manifest)
            .await?
        {
            Ok(note) => note,
            Err(outcome) => return Ok(outcome),
        };

        let runtime = match resolve_runtime(language).await {
            Ok(runtime) => runtime,
            Err(detail) => return Ok(Outcome::ToolchainMissing { language, detail }),
        };

        if let Err(output) = build_here(language, &skill_dir, has_manifest, &runtime).await? {
            return Ok(Outcome::CompileFailed(output));
        }

        let report = validator::validate_command(
            start_command(language, module, &runtime, &skill_dir),
            module,
            module,
            validator::Depth::Contract,
        )
        .await;

        if !report.passed() {
            return Ok(Outcome::ValidationFailed(format!(
                "{}\n\nFailed checks:\n  {}",
                report.summary(),
                report.failures().join("\n  ")
            )));
        }

        let skills_dir = workshop::skills_dir()?;
        std::fs::create_dir_all(&skills_dir)?;
        let launcher = skills_dir.join(workshop::binary_name(module));

        if let Ok(supervisor) = Supervisor::open() {
            let reason = if launcher.exists() {
                format!("before-replacing-{}", module)
            } else {
                format!("before-adding-{}", module)
            };
            if let Err(e) = supervisor.snapshot(&reason) {
                eprintln!("[supervisor] could not snapshot before deploy: {}", e);
            }
        }

        let installed = install(language, module, &skill_dir, &skills_dir, &runtime)?;

        Ok(Outcome::Deployed {
            path: installed,
            report: report.summary(),
            preflight: preflight_note,
        })
    }

    async fn preflight(
        &self,
        config: &PreflightSection,
        module: &str,
        language: Language,
        skill_dir: &Path,
        sdk_dir: &Path,
        has_manifest: bool,
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

        let steps =
            preflight::build_commands(config, language, skill_dir, sdk_dir, module, has_manifest);

        for mut step in steps {
            let result =
                tokio::time::timeout(Duration::from_secs(config.build_timeout_sec), step.output())
                    .await
                    .map_err(|_| {
                        JumabekError::InternalError(format!(
                            "preflight build did not finish within {}s",
                            config.build_timeout_sec
                        ))
                    })?
                    .map_err(|e| {
                        JumabekError::InternalError(format!("cannot run docker: {}", e))
                    })?;

            if !result.status.success() {
                return Ok(Err(Outcome::CompileFailed(format!(
                    "the code did not build in the {} preflight container\n{}",
                    language,
                    trim_compiler_output(&combined_output(&result))
                ))));
            }
        }

        let command = preflight::validate_command(config, language, skill_dir, sdk_dir, module);
        let report = validator::validate_command(
            command,
            &format!("{} (container)", module),
            module,
            validator::Depth::Smoke,
        )
        .await;

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

fn lay_out_sources(
    skill_dir: &Path,
    module: &str,
    language: Language,
    code: &str,
    dependencies: &[String],
) -> JumabekResult<bool> {
    if skill_dir.exists() && !previously_built_in(skill_dir, language) {
        let _ = std::fs::remove_dir_all(skill_dir);
    }
    std::fs::create_dir_all(skill_dir)?;

    let entry = skill_dir.join(language.entry());
    if let Some(parent) = entry.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(entry, code)?;

    if let Some((name, contents)) = language.helper() {
        std::fs::write(skill_dir.join(name), contents)?;
    }

    match language.manifest(module, dependencies) {
        Some((name, contents)) => {
            std::fs::write(skill_dir.join(name), contents)?;
            Ok(true)
        }
        None => Ok(false),
    }
}

fn previously_built_in(skill_dir: &Path, language: Language) -> bool {
    skill_dir.join(language.entry()).exists()
}

async fn resolve_runtime(language: Language) -> Result<String, String> {
    let mut runtime = None;
    for candidate in language.runtimes() {
        if answers(candidate).await {
            runtime = Some(candidate.to_string());
            break;
        }
    }

    let mut missing: Vec<String> = Vec::new();
    if runtime.is_none() {
        missing.push(language.runtimes().join(" or "));
    }
    for tool in language.extra_tools() {
        if !answers(tool).await {
            missing.push(tool.to_string());
        }
    }

    match runtime {
        Some(runtime) if missing.is_empty() => Ok(runtime),
        _ => Err(format!(
            "{} is not on PATH on this machine — {}",
            missing.join(", "),
            language.install_hint()
        )),
    }
}

async fn answers(program: &str) -> bool {
    let probe = Command::new(program).arg("--version").output();
    matches!(
        tokio::time::timeout(TOOL_PROBE_TIMEOUT, probe).await,
        Ok(Ok(output)) if output.status.success()
    )
}

async fn build_here(
    language: Language,
    skill_dir: &Path,
    has_manifest: bool,
    runtime: &str,
) -> JumabekResult<Result<(), String>> {
    for step in language.build_steps(has_manifest, runtime) {
        let (program, arguments) = step.split_first().expect("a build step with no command");

        let output = tokio::time::timeout(
            BUILD_TIMEOUT,
            Command::new(program)
                .args(arguments)
                .current_dir(skill_dir)
                .output(),
        )
        .await
        .map_err(|_| {
            JumabekError::InternalError(format!(
                "{} did not finish within {}s",
                program,
                BUILD_TIMEOUT.as_secs()
            ))
        })?
        .map_err(|e| {
            JumabekError::InternalError(format!(
                "cannot run {}: {} — {}",
                program,
                e,
                language.install_hint()
            ))
        })?;

        if !output.status.success() {
            return Ok(Err(trim_compiler_output(&combined_output(&output))));
        }
    }

    Ok(Ok(()))
}

fn start_command(language: Language, module: &str, runtime: &str, dir: &Path) -> Command {
    let argv = language.host_argv(module, runtime, dir);
    let (program, arguments) = argv.split_first().expect("a start command with no program");

    let mut command = Command::new(program);
    command.args(arguments);
    command
}

fn install(
    language: Language,
    module: &str,
    skill_dir: &Path,
    skills_dir: &Path,
    runtime: &str,
) -> JumabekResult<PathBuf> {
    let launcher = skills_dir.join(workshop::binary_name(module));

    if language.produces_binary() {
        let built = skill_dir
            .join("target")
            .join("release")
            .join(workshop::binary_name(module));

        if !built.exists() {
            return Err(JumabekError::InternalError(format!(
                "the build reported success but {} is missing",
                built.display()
            )));
        }

        replace_binary(&built, &launcher)?;
        return Ok(launcher);
    }

    let payload = skills_dir.join(format!("{}.d", module));
    if payload.exists() {
        std::fs::remove_dir_all(&payload)?;
    }
    copy_tree(skill_dir, &payload)?;

    let launcher = skills_dir.join(launcher_name(module));
    let entry = payload.join(
        Path::new(language.entry())
            .file_name()
            .expect("an entry file with no name"),
    );
    std::fs::write(&launcher, launcher_script(language, runtime, &entry))?;
    make_executable(&launcher)?;

    Ok(launcher)
}

fn launcher_name(module: &str) -> String {
    if cfg!(windows) {
        format!("{}.cmd", module)
    } else {
        module.to_string()
    }
}

fn launcher_script(language: Language, runtime: &str, entry: &Path) -> String {
    if cfg!(windows) {
        format!(
            "@echo off\r\n\
             rem Written by JumaBek — starts the {} skill.\r\n\
             \"{}\" \"{}\" %*\r\n",
            language,
            runtime,
            entry.display()
        )
    } else {
        format!(
            "#!/bin/sh\n\
             # Written by JumaBek — starts the {} skill.\n\
             exec \"{}\" \"{}\" \"$@\"\n",
            language,
            runtime,
            entry.display()
        )
    }
}

fn make_executable(path: &Path) -> JumabekResult<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

fn copy_tree(from: &Path, to: &Path) -> JumabekResult<()> {
    std::fs::create_dir_all(to)?;

    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let source = entry.path();
        let target = to.join(entry.file_name());

        if entry.file_type()?.is_dir() {
            copy_tree(&source, &target)?;
        } else {
            std::fs::copy(&source, &target)?;
        }
    }

    Ok(())
}

fn combined_output(output: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);

    match (stderr.trim().is_empty(), stdout.trim().is_empty()) {
        (true, true) => "the build failed without saying anything".to_string(),
        (true, false) => stdout.into_owned(),
        (false, true) => stderr.into_owned(),
        (false, false) => format!("{}\n{}", stderr, stdout),
    }
}

impl Default for SelfImprovement {
    fn default() -> Self {
        Self::new()
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
        let name = workshop::binary_name("file_ops");
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
                    language: Language::Rust,
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
                    language: Language::Rust,
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
mod interpreted_skill_tests {
    use super::*;
    use crate::skill_layer::loader;
    use crate::skill_layer::rpc_client::SkillRpcClient;

    const GREETER: &str = r#"
import jumabek

def execute(method, args):
    if method == "greet":
        print("a stray print must not corrupt the protocol")
        return "hello, " + args
    raise jumabek.SkillError("unknown method: " + method, kind="NotFound")

jumabek.run(
    name="greeter",
    version="0.1.0",
    description="Greets whoever is named in the arguments",
    methods=[{"method": "greet",
              "description": "Greet someone by name",
              "args_description": "the name to greet"}],
    execute=execute,
)
"#;

    const LIAR: &str = r#"
import jumabek

def execute(method, args):
    if method == "implemented":
        return "here"
    raise jumabek.SkillError("unknown method: " + method, kind="NotFound")

jumabek.run(
    name="liar",
    version="0.1.0",
    description="Declares a method it never implements",
    methods=[{"method": "implemented",
              "description": "A method that is actually handled",
              "args_description": "anything, it is ignored"},
             {"method": "forgotten",
              "description": "Declared here and handled nowhere",
              "args_description": "anything, it is never read"}],
    execute=execute,
)
"#;

    struct Sandbox {
        dir: PathBuf,
    }

    impl Sandbox {
        fn new(name: &str) -> Sandbox {
            let dir =
                std::env::temp_dir().join(format!("jumabek-py-{}-{}", name, std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(dir.join("skills")).expect("sandbox");
            Sandbox { dir }
        }
    }

    impl Drop for Sandbox {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    const GREETER_JS: &str = r#"
const jumabek = require("./jumabek");

jumabek.run({
  name: "greeter",
  version: "0.1.0",
  description: "Greets whoever is named in the arguments",
  methods: [{ method: "greet",
              description: "Greet someone by name",
              args_description: "the name to greet" }],
  async execute(method, args) {
    if (method === "greet") {
      console.log("a stray log must not corrupt the protocol");
      return `hello, ${args}`;
    }
    throw new jumabek.SkillError(`unknown method: ${method}`, "NotFound");
  },
});
"#;

    #[tokio::test]
    async fn a_python_skill_is_built_installed_and_answers_through_its_launcher() {
        greeter_survives_the_whole_pipeline(Language::Python, GREETER).await;
    }

    #[tokio::test]
    async fn a_node_skill_is_built_installed_and_answers_through_its_launcher() {
        greeter_survives_the_whole_pipeline(Language::Node, GREETER_JS).await;
    }

    async fn greeter_survives_the_whole_pipeline(language: Language, source: &str) {
        let Ok(runtime) = resolve_runtime(language).await else {
            eprintln!("skipped: no {} on this machine", language);
            return;
        };

        let sandbox = Sandbox::new(language.id());
        let skill_dir = sandbox.dir.join("workshop").join("greeter");
        let skills_dir = sandbox.dir.join("skills");

        let has_manifest = lay_out_sources(&skill_dir, "greeter", language, source, &[]).unwrap();
        let (helper, _) = language.helper().expect("no protocol helper to write");
        assert!(
            skill_dir.join(helper).exists(),
            "the protocol helper was not written next to the code"
        );

        build_here(language, &skill_dir, has_manifest, &runtime)
            .await
            .unwrap()
            .expect("the skill did not build");

        let report = validator::validate_command(
            start_command(language, "greeter", &runtime, &skill_dir),
            "greeter",
            "greeter",
            validator::Depth::Smoke,
        )
        .await;
        assert!(
            report.passed(),
            "a working {} skill was rejected:\n{}",
            language,
            report.summary()
        );

        let launcher = install(language, "greeter", &skill_dir, &skills_dir, &runtime).unwrap();

        assert!(
            skills_dir.join("greeter.d").join(helper).is_file(),
            "the skill's own files were not installed beside the launcher"
        );

        let found = loader::discover(&skills_dir).unwrap();
        assert_eq!(
            found,
            vec![launcher.clone()],
            "the loader does not see exactly one skill in {}",
            skills_dir.display()
        );

        let client = SkillRpcClient::spawn_with_settings(&launcher, Default::default())
            .await
            .expect("the installed skill does not start");

        assert_eq!(client.get_metadata_cached().name, "greeter");

        let params = serde_json::json!({ "method": "greet", "args": "aibar" }).to_string();
        let response = client.call("execute", Some(params)).await.unwrap();

        match response.payload {
            jumabek_sdk::protocol::SkillResponsePayload::Output(
                jumabek_sdk::SkillOutput::Text(text),
            ) => assert_eq!(text, "hello, aibar"),
            other => panic!("unexpected answer: {:?}", other),
        }

        let _ = client.shutdown().await;
    }

    async fn report_for_liar(depth: validator::Depth) -> Option<validator::Report> {
        let runtime = resolve_runtime(Language::Python).await.ok()?;

        let sandbox = Sandbox::new(&format!("liar-{:?}", depth));
        let skill_dir = sandbox.dir.join("workshop").join("liar");

        let has_manifest =
            lay_out_sources(&skill_dir, "liar", Language::Python, LIAR, &[]).unwrap();
        build_here(Language::Python, &skill_dir, has_manifest, &runtime)
            .await
            .unwrap()
            .expect("the fixture did not build");

        Some(
            validator::validate_command(
                start_command(Language::Python, "liar", &runtime, &skill_dir),
                "liar",
                "liar",
                depth,
            )
            .await,
        )
    }

    fn check<'a>(report: &'a validator::Report, name: &str) -> Option<&'a (String, bool, String)> {
        report.checks.iter().find(|(check, _, _)| check == name)
    }

    #[tokio::test]
    async fn a_method_that_was_never_wired_up_is_caught() {
        let Some(report) = report_for_liar(validator::Depth::Smoke).await else {
            eprintln!("skipped: no Python on this machine");
            return;
        };

        let smoke = check(&report, "implements the methods it declares")
            .expect("the smoke check never ran");

        assert!(!smoke.1, "a skill that declares a phantom method passed");
        assert!(
            smoke.2.contains("forgotten"),
            "the report does not say which method is missing: {}",
            smoke.2
        );
        assert!(!report.passed());
    }

    #[tokio::test]
    async fn the_contract_check_never_calls_the_skill_itself() {
        let Some(report) = report_for_liar(validator::Depth::Contract).await else {
            eprintln!("skipped: no Python on this machine");
            return;
        };

        assert!(
            check(&report, "implements the methods it declares").is_none(),
            "the contract depth called the skill's own methods"
        );
        assert!(
            report.passed(),
            "the same skill fails the contract checks it does keep: {}",
            report.summary()
        );
    }

    #[tokio::test]
    async fn a_python_skill_with_a_syntax_error_fails_the_build_not_the_handshake() {
        let Ok(runtime) = resolve_runtime(Language::Python).await else {
            eprintln!("skipped: no Python on this machine");
            return;
        };

        let sandbox = Sandbox::new("broken");
        let skill_dir = sandbox.dir.join("workshop").join("broken");

        let has_manifest =
            lay_out_sources(&skill_dir, "broken", Language::Python, "def (:", &[]).unwrap();

        let result = build_here(Language::Python, &skill_dir, has_manifest, &runtime)
            .await
            .unwrap();

        let report = result.expect_err("broken source built successfully");
        assert!(
            report.to_lowercase().contains("error"),
            "the failure does not say what is wrong: {report}"
        );
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
            language: Language::Rust,
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
