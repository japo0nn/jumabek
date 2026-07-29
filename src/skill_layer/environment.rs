use std::collections::BTreeMap;

use tokio::process::Command;

pub const SKILL_PREFIX: &str = "JUMABEK_SKILL_";

#[cfg(windows)]
const PASSTHROUGH: &[&str] = &[
    "PATH",
    "SystemRoot",
    "SystemDrive",
    "windir",
    "COMSPEC",
    "PATHEXT",
    "TEMP",
    "TMP",
    "NUMBER_OF_PROCESSORS",
    "PROCESSOR_ARCHITECTURE",
    "USERPROFILE",
    "APPDATA",
    "LOCALAPPDATA",
    "PROGRAMFILES",
    "PROGRAMDATA",
];

#[cfg(not(windows))]
const PASSTHROUGH: &[&str] = &[
    "PATH", "HOME", "TMPDIR", "LANG", "LC_ALL", "SHELL", "USER", "TERM",
];

pub fn apply(command: &mut Command, settings: &BTreeMap<String, String>) {
    command.env_clear();

    for name in PASSTHROUGH {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }

    for (key, value) in settings {
        command.env(format!("{}{}", SKILL_PREFIX, normalise(key)), value);
    }
}

pub fn normalise(key: &str) -> String {
    key.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_of(command: &Command) -> BTreeMap<String, String> {
        command
            .as_std()
            .get_envs()
            .filter_map(|(k, v)| {
                Some((
                    k.to_string_lossy().to_string(),
                    v?.to_string_lossy().to_string(),
                ))
            })
            .collect()
    }

    fn settings() -> BTreeMap<String, String> {
        BTreeMap::from([
            ("api_key".to_string(), "abc123".to_string()),
            ("city".to_string(), "Almaty".to_string()),
        ])
    }

    #[test]
    fn a_skill_gets_its_own_settings_under_one_prefix() {
        let mut command = Command::new("noop");
        apply(&mut command, &settings());

        let env = env_of(&command);
        assert_eq!(env.get("JUMABEK_SKILL_API_KEY").unwrap(), "abc123");
        assert_eq!(env.get("JUMABEK_SKILL_CITY").unwrap(), "Almaty");
    }

    #[test]
    fn the_agent_credentials_are_withheld() {
        unsafe {
            std::env::set_var("JUMABEK_API_KEY", "llm-secret");
            std::env::set_var("JUMABEK_GROQ_API_KEY", "voice-secret");
        }

        let mut command = Command::new("noop");
        apply(&mut command, &settings());

        let env = env_of(&command);
        for leaked in ["JUMABEK_API_KEY", "JUMABEK_GROQ_API_KEY"] {
            assert!(
                !env.contains_key(leaked),
                "{} reached the skill: {:?}",
                leaked,
                env.keys().collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn unrelated_variables_do_not_leak_either() {
        unsafe {
            std::env::set_var("AWS_SECRET_ACCESS_KEY", "not yours");
        }

        let mut command = Command::new("noop");
        apply(&mut command, &BTreeMap::new());

        assert!(!env_of(&command).contains_key("AWS_SECRET_ACCESS_KEY"));
    }

    #[test]
    fn what_a_process_needs_to_start_is_kept() {
        let mut command = Command::new("noop");
        apply(&mut command, &BTreeMap::new());

        let env = env_of(&command);
        assert!(env.contains_key("PATH"), "PATH was dropped: {:?}", env);

        #[cfg(windows)]
        assert!(env.contains_key("SystemRoot"), "SystemRoot was dropped");
    }

    #[test]
    fn keys_become_valid_variable_names() {
        assert_eq!(normalise("api_key"), "API_KEY");
        assert_eq!(normalise("base-url"), "BASE_URL");
        assert_eq!(normalise("Refresh Rate"), "REFRESH_RATE");
    }
}
