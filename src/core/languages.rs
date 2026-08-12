//! What it takes to build a skill, per language.
//!
//! The protocol is one JSON line in, one JSON line out, and it has never cared
//! what wrote the other end — a skill is a process, not a Rust crate. The build
//! pipeline cared, though: it wrote a `Cargo.toml`, ran `cargo` in a `rust`
//! image and looked for a binary under `target/release`. Everything that made
//! that assumption lives here now, so a new language is a handful of match arms
//! rather than a search through the workshop.
//!
//! Three things stay language-independent on purpose: the protocol itself, the
//! validator (it talks to a process, not to a compiler), and the rule that
//! stdout belongs to the protocol.

use std::path::Path;

use crate::core::workshop;

/// The protocol helpers handed to skills that have no SDK crate to link
/// against. Rust gets the real `jumabek_sdk`; for the others the whole contract
/// fits in one file, and shipping it is cheaper than having the model
/// rediscover the wire format — and get it subtly wrong — every time.
pub const HELPER_PYTHON: &str = include_str!("helpers/jumabek.py");
pub const HELPER_NODE: &str = include_str!("helpers/jumabek.js");

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Language {
    #[default]
    Rust,
    Python,
    Node,
}

impl Language {
    pub const ALL: [Language; 3] = [Language::Rust, Language::Python, Language::Node];

    /// Accepts what a model is likely to write. An unknown name is `None`
    /// rather than a silent fall back to Rust: building Python source with
    /// `cargo` produces a compiler error that explains nothing.
    pub fn parse(raw: &str) -> Option<Language> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "" | "rust" | "rs" | "cargo" => Some(Language::Rust),
            "python" | "python3" | "py" => Some(Language::Python),
            "node" | "nodejs" | "js" | "javascript" => Some(Language::Node),
            _ => None,
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Language::Rust => "rust",
            Language::Python => "python",
            Language::Node => "node",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Language::Rust => "Rust",
            Language::Python => "Python",
            Language::Node => "Node",
        }
    }

    /// Where the generated code goes, relative to the skill directory.
    pub fn entry(self) -> &'static str {
        match self {
            Language::Rust => "src/main.rs",
            Language::Python => "main.py",
            Language::Node => "main.js",
        }
    }

    /// The protocol helper written next to the code, if the language needs one.
    pub fn helper(self) -> Option<(&'static str, &'static str)> {
        match self {
            Language::Rust => None,
            Language::Python => Some(("jumabek.py", HELPER_PYTHON)),
            Language::Node => Some(("jumabek.js", HELPER_NODE)),
        }
    }

    /// Only Rust links the vendored SDK, so only Rust needs it mounted.
    pub fn needs_sdk(self) -> bool {
        matches!(self, Language::Rust)
    }

    /// A single executable that can be copied anywhere, or a directory that has
    /// to be kept together and started through an interpreter.
    pub fn produces_binary(self) -> bool {
        matches!(self, Language::Rust)
    }

    /// The manifest file, if the language has one. Python with no dependencies
    /// gets none — an empty `requirements.txt` is a step that can only fail.
    pub fn manifest(self, module: &str, dependencies: &[String]) -> Option<(&'static str, String)> {
        match self {
            Language::Rust => Some(("Cargo.toml", workshop::cargo_manifest(module, dependencies))),
            Language::Python => {
                let lines = python_requirements(dependencies);
                (!lines.is_empty()).then(|| ("requirements.txt", lines.join("\n") + "\n"))
            }
            Language::Node => Some(("package.json", node_manifest(module, dependencies))),
        }
    }

    /// The image the preflight container is built from when the config names no
    /// other. Pinned to a major version, never to `latest`: a skill that built
    /// yesterday should build today.
    pub fn default_image(self) -> &'static str {
        match self {
            Language::Rust => "rust:1-slim",
            Language::Python => "python:3-slim",
            Language::Node => "node:22-slim",
        }
    }

    /// A named docker volume and where it mounts, so the second build of the
    /// day does not download the world again.
    pub fn cache(self) -> Option<(&'static str, &'static str)> {
        match self {
            Language::Rust => Some(("jumabek-cargo-cache", "/usr/local/cargo/registry")),
            Language::Python => Some(("jumabek-pip-cache", "/root/.cache/pip")),
            Language::Node => Some(("jumabek-npm-cache", "/root/.npm")),
        }
    }

    /// What to run, in order, inside the skill directory to turn source into
    /// something startable.
    ///
    /// Every language ends with a step that fails on broken source, not only on
    /// missing dependencies — otherwise a Python skill with a syntax error
    /// would sail through the build and only fall over at the handshake, where
    /// the error says nothing about which line is wrong.
    ///
    /// `runtime` is the interpreter to call: fixed inside the container, probed
    /// on the host, and ignored by Rust.
    pub fn build_steps(self, has_manifest: bool, runtime: &str) -> Vec<Vec<String>> {
        let argv = |parts: &[&str]| parts.iter().map(|p| p.to_string()).collect::<Vec<_>>();

        match self {
            Language::Rust => vec![argv(&["cargo", "build", "--release", "--quiet"])],

            Language::Python => {
                let mut steps = Vec::new();
                if has_manifest {
                    steps.push(vec![
                        runtime.to_string(),
                        "-m".into(),
                        "pip".into(),
                        "install".into(),
                        "--no-input".into(),
                        "--disable-pip-version-check".into(),
                        "--quiet".into(),
                        "--target".into(),
                        ".".into(),
                        "-r".into(),
                        "requirements.txt".into(),
                    ]);
                }
                steps.push(vec![
                    runtime.to_string(),
                    "-m".into(),
                    "compileall".into(),
                    "-q".into(),
                    "main.py".into(),
                    "jumabek.py".into(),
                ]);
                steps
            }

            Language::Node => {
                let mut steps = Vec::new();
                if has_manifest {
                    steps.push(argv(&[
                        "npm",
                        "install",
                        "--omit=dev",
                        "--no-audit",
                        "--no-fund",
                        "--loglevel=error",
                    ]));
                }
                steps.push(vec![
                    runtime.to_string(),
                    "--check".into(),
                    "main.js".into(),
                ]);
                steps
            }
        }
    }

    /// How the built skill is started from its own directory. `runtime` is
    /// ignored by Rust, which has a binary to run.
    pub fn start_argv(self, module: &str, runtime: &str, dir: &Path) -> Vec<String> {
        match self {
            Language::Rust => vec![
                dir.join("target")
                    .join("release")
                    .join(workshop::binary_name(module))
                    .display()
                    .to_string(),
            ],
            Language::Python => vec![
                runtime.to_string(),
                dir.join("main.py").display().to_string(),
            ],
            Language::Node => vec![
                runtime.to_string(),
                dir.join("main.js").display().to_string(),
            ],
        }
    }

    /// Command names to look for on this machine, in order of preference. The
    /// first one that answers `--version` is what the skill will be started
    /// with, and that name is written into the launcher so it cannot drift
    /// later.
    pub fn runtimes(self) -> &'static [&'static str] {
        match self {
            Language::Rust => &["cargo"],
            Language::Python => &["python3", "python"],
            Language::Node => &["node"],
        }
    }

    /// What else has to be on PATH besides the runtime. Named separately
    /// because "install Node" is not an actionable message when what is
    /// missing is npm.
    pub fn extra_tools(self) -> &'static [&'static str] {
        match self {
            Language::Rust | Language::Python => &[],
            Language::Node => &["npm"],
        }
    }

    /// What to tell someone who has none of it installed.
    pub fn install_hint(self) -> &'static str {
        match self {
            Language::Rust => "install the Rust toolchain from https://rustup.rs",
            Language::Python => "install Python 3 from https://python.org",
            Language::Node => "install Node (which ships npm) from https://nodejs.org",
        }
    }

    /// The runtime name used inside the container, where the image guarantees
    /// what is present and there is nothing to probe.
    pub fn container_runtime(self) -> &'static str {
        match self {
            Language::Rust => "cargo",
            Language::Python => "python3",
            Language::Node => "node",
        }
    }
}

impl std::fmt::Display for Language {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// A dependency as the model may write it, rendered for pip. `name@version`
/// becomes `name==version`; a bare name is left unpinned. A raw specifier the
/// model wrote itself (`httpx>=0.27`) is passed through once it survives the
/// character check — the whole point of that check is that a newline can never
/// reach the file, since one line is one dependency.
fn python_requirements(dependencies: &[String]) -> Vec<String> {
    let mut lines = Vec::new();

    for raw in dependencies {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }

        let trimmed = trimmed.split('+').next().unwrap_or(trimmed).trim();

        let line = match trimmed.split_once('@') {
            Some((name, version)) => {
                let (name, version) = (name.trim(), version.trim());
                if !is_python_package(name) || !is_version(version) {
                    continue;
                }
                if version == "*" {
                    name.to_string()
                } else {
                    format!("{}=={}", name, version)
                }
            }
            None => {
                if !is_python_requirement(trimmed) {
                    continue;
                }
                trimmed.to_string()
            }
        };

        if !lines.contains(&line) {
            lines.push(line);
        }
    }

    lines
}

fn node_manifest(module: &str, dependencies: &[String]) -> String {
    let mut deps = serde_json::Map::new();

    for raw in dependencies {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        let trimmed = trimmed.split('+').next().unwrap_or(trimmed).trim();

        let (name, version) = match trimmed[1..].find('@') {
            Some(at) => (&trimmed[..at + 1], trimmed[at + 2..].trim()),
            None => (trimmed, "*"),
        };

        if !is_node_package(name.trim()) || !is_version(version) {
            continue;
        }

        deps.insert(
            name.trim().to_string(),
            serde_json::Value::String(version.to_string()),
        );
    }

    let manifest = serde_json::json!({
        "name": module,
        "version": "0.1.0",
        "private": true,
        "description": format!("The {} JumaBek skill", module),
        "main": "main.js",
        "dependencies": deps,
    });

    serde_json::to_string_pretty(&manifest).unwrap_or_default() + "\n"
}

fn is_python_package(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '[' | ']'))
}

/// A whole requirement line, comparison operators included.
fn is_python_requirement(raw: &str) -> bool {
    !raw.is_empty()
        && raw.chars().all(|c| {
            c.is_ascii_alphanumeric()
                || matches!(
                    c,
                    '-' | '_' | '.' | '[' | ']' | '=' | '<' | '>' | '!' | '~' | ',' | '*' | '+'
                )
        })
}

fn is_node_package(name: &str) -> bool {
    if name.is_empty() || name.len() > 214 {
        return false;
    }

    let body = name.strip_prefix('@').unwrap_or(name);
    let (scope, rest) = match body.split_once('/') {
        Some((scope, rest)) if name.starts_with('@') => (Some(scope), rest),
        Some(_) => return false,
        None if name.starts_with('@') => return false,
        None => (None, body),
    };

    let sane = |part: &str| {
        !part.is_empty()
            && part.chars().all(|c| {
                c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_' | '.')
            })
    };

    sane(rest) && scope.map(sane).unwrap_or(true)
}

fn is_version(version: &str) -> bool {
    !version.is_empty()
        && version.chars().all(|c| {
            c.is_ascii_alphanumeric()
                || matches!(
                    c,
                    '.' | '-' | '*' | '^' | '~' | '=' | '<' | '>' | '+' | '|' | ' '
                )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_name_a_model_might_write_lands_somewhere() {
        for (raw, expected) in [
            ("rust", Language::Rust),
            ("RS", Language::Rust),
            ("", Language::Rust),
            ("python", Language::Python),
            ("Python3", Language::Python),
            ("py", Language::Python),
            ("node", Language::Node),
            ("JavaScript", Language::Node),
            ("js", Language::Node),
        ] {
            assert_eq!(Language::parse(raw), Some(expected), "on {raw:?}");
        }
    }

    #[test]
    fn an_unknown_language_is_refused_rather_than_guessed() {
        for raw in ["go", "ruby", "c++", "typescript"] {
            assert_eq!(
                Language::parse(raw),
                None,
                "{raw} was quietly turned into something else"
            );
        }
    }

    #[test]
    fn every_language_ships_a_way_to_speak_the_protocol() {
        for language in Language::ALL {
            let has_help = language.needs_sdk() || language.helper().is_some();
            assert!(
                has_help,
                "{} has neither the SDK nor a helper — the model would have to \
                 reinvent the wire format",
                language
            );
        }
    }

    #[test]
    fn the_embedded_helpers_are_the_real_ones() {
        assert!(HELPER_PYTHON.contains("def run("));
        assert!(HELPER_PYTHON.contains("get_metadata"));
        assert!(HELPER_NODE.contains("function run("));
        assert!(HELPER_NODE.contains("available_methods"));
    }

    #[test]
    fn the_helpers_keep_stdout_for_the_protocol() {
        assert!(
            HELPER_PYTHON.contains("sys.stdout = sys.stderr"),
            "a stray print() would corrupt the response line"
        );
        assert!(
            HELPER_NODE.contains("console.log = console.error"),
            "a stray console.log would corrupt the response line"
        );
    }

    /// Lines produced by the shipped Python helper, verbatim.
    ///
    /// The helpers write the wire format by hand — they have no serde to keep
    /// them honest — so the thing that can silently break them is a change to
    /// `SkillResponsePayload` on this side. Parsing their real output with the
    /// real types is what turns that into a failing test instead of a skill
    /// that builds, installs and then cannot be spoken to.
    const HELPER_REPLIES: &[&str] = &[
        r#"{"id": 1, "payload": {"Metadata": {"name": "greeter", "version": "0.1.0", "description": "Greets whoever is named"}}}"#,
        r#"{"id": 2, "payload": {"Methods": [{"method": "greet", "description": "Greet someone", "args_description": "a name"}]}}"#,
        r#"{"id": 3, "payload": {"Health": true}}"#,
        r#"{"id": 4, "payload": {"Output": {"Text": "hello"}}}"#,
        r#"{"id": 5, "payload": {"Output": {"Json": {"length": 3}}}}"#,
        r#"{"id": 6, "payload": {"Output": "Empty"}}"#,
        r#"{"id": 7, "payload": {"Error": {"NotFound": "unknown method"}}}"#,
        r#"{"id": 8, "payload": {"Error": {"InvalidArgs": "bad params"}}}"#,
    ];

    #[test]
    fn what_the_helpers_write_is_what_the_core_reads() {
        use jumabek_sdk::protocol::SkillResponse;

        for reply in HELPER_REPLIES {
            serde_json::from_str::<SkillResponse>(reply).unwrap_or_else(|e| {
                panic!("the core cannot read what the helpers write: {e}\n{reply}")
            });
        }
    }

    #[test]
    fn a_request_the_core_writes_is_what_the_helpers_expect() {
        use jumabek_sdk::protocol::SkillRequest;

        let request = SkillRequest {
            id: 1,
            method: "execute".to_string(),
            params: Some(r#"{"method":"greet","args":"aibar"}"#.to_string()),
        };

        let line = serde_json::to_string(&request).unwrap();
        assert!(
            line.contains(r#""params":"{\"method\":\"greet\""#),
            "params stopped being a string holding JSON, and both helpers parse it as one: {line}"
        );
    }

    #[test]
    fn every_language_checks_its_source_even_without_dependencies() {
        for language in Language::ALL {
            let steps = language.build_steps(false, language.container_runtime());
            assert!(
                !steps.is_empty(),
                "{} would accept source nobody ever looked at",
                language
            );
        }
    }

    #[test]
    fn dependencies_add_an_install_step() {
        for language in [Language::Python, Language::Node] {
            let without = language.build_steps(false, "x").len();
            let with = language.build_steps(true, "x").len();
            assert_eq!(with, without + 1, "{}", language);
        }
    }

    #[test]
    fn python_pins_what_it_was_given_and_leaves_the_rest_open() {
        let lines = python_requirements(&[
            "requests@2.31.0".to_string(),
            "httpx".to_string(),
            "beautifulsoup4@*".to_string(),
        ]);

        assert_eq!(lines, vec!["requests==2.31.0", "httpx", "beautifulsoup4"]);
    }

    #[test]
    fn python_accepts_a_specifier_written_by_hand() {
        assert_eq!(
            python_requirements(&["httpx>=0.27".to_string()]),
            vec!["httpx>=0.27"]
        );
        assert_eq!(
            python_requirements(&["uvicorn[standard]@0.30".to_string()]),
            vec!["uvicorn[standard]==0.30"]
        );
    }

    #[test]
    fn nothing_with_a_newline_reaches_requirements_txt() {
        let lines = python_requirements(&[
            "evil\n--extra-index-url http://attacker".to_string(),
            "also evil@1.0\nrequests".to_string(),
            "  ".to_string(),
        ]);

        assert!(
            lines.is_empty(),
            "pip was handed an extra directive: {lines:?}"
        );
    }

    #[test]
    fn a_duplicate_requirement_is_written_once() {
        let lines = python_requirements(&["requests@2".to_string(), "requests@2".to_string()]);
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn node_writes_a_manifest_that_parses() {
        let manifest = node_manifest("weather", &["axios@1.6".to_string(), "dayjs".to_string()]);
        let parsed: serde_json::Value = serde_json::from_str(&manifest).expect("not JSON");

        assert_eq!(parsed["name"], "weather");
        assert_eq!(parsed["dependencies"]["axios"], "1.6");
        assert_eq!(parsed["dependencies"]["dayjs"], "*");
    }

    #[test]
    fn node_keeps_the_scope_of_a_scoped_package() {
        let manifest = node_manifest("x", &["@octokit/rest@21".to_string()]);
        let parsed: serde_json::Value = serde_json::from_str(&manifest).unwrap();

        assert_eq!(parsed["dependencies"]["@octokit/rest"], "21");
    }

    #[test]
    fn a_rubbish_node_dependency_is_dropped_not_written_out() {
        let manifest = node_manifest("x", &["../../etc/passwd@1".to_string()]);
        let parsed: serde_json::Value = serde_json::from_str(&manifest).unwrap();

        assert!(
            parsed["dependencies"].as_object().unwrap().is_empty(),
            "{manifest}"
        );
    }

    #[test]
    fn python_has_nothing_to_install_when_nothing_was_asked_for() {
        assert!(
            Language::Python.manifest("x", &[]).is_none(),
            "an empty requirements.txt is a step that can only fail"
        );
    }

    #[test]
    fn every_language_names_a_pinned_image() {
        for language in Language::ALL {
            let image = language.default_image();
            assert!(!image.ends_with(":latest"), "{} floats on latest", language);
            assert!(image.contains(':'), "{} has no tag: {}", language, image);
        }
    }

    #[test]
    fn a_skill_is_started_from_its_own_directory() {
        let dir = Path::new("/skills/weather.d");

        assert_eq!(
            Language::Python.start_argv("weather", "python3", dir),
            vec![
                "python3".to_string(),
                "/skills/weather.d/main.py".to_string()
            ]
        );
        assert_eq!(
            Language::Node.start_argv("weather", "node", dir),
            vec!["node".to_string(), "/skills/weather.d/main.js".to_string()]
        );
    }

    #[test]
    fn only_rust_is_a_single_file_to_copy() {
        assert!(Language::Rust.produces_binary());
        assert!(!Language::Python.produces_binary());
        assert!(!Language::Node.produces_binary());
    }
}
