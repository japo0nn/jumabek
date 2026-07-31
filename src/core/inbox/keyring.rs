use std::collections::BTreeMap;

use crate::core::task::Grant;

pub const MIN_TOKEN_LENGTH: usize = 24;

#[derive(Debug, Clone, PartialEq)]
pub struct Caller {
    pub name: String,
    pub grant: Grant,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Rejected {
    NoToken,
    UnknownToken,
}

impl std::fmt::Display for Rejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Rejected::NoToken => write!(f, "no token"),
            Rejected::UnknownToken => write!(f, "unknown token"),
        }
    }
}

/// Who may knock, and what they may do once inside. Tokens live in secrets.toml
/// and grants in config.toml: the secret and the permission are different kinds
/// of thing and are edited by different people at different times.
#[derive(Debug, Default)]
pub struct Keyring {
    by_token: BTreeMap<String, Caller>,
    weak: Vec<String>,
    ungranted: Vec<String>,
}

impl Keyring {
    pub fn build(tokens: &BTreeMap<String, String>, grants: &BTreeMap<String, Grant>) -> Keyring {
        let mut keyring = Keyring::default();

        for (name, token) in tokens {
            let token = token.trim();
            if token.is_empty() {
                continue;
            }

            if token.chars().count() < MIN_TOKEN_LENGTH {
                keyring.weak.push(name.clone());
                continue;
            }

            match grants.get(name) {
                Some(grant) => {
                    keyring.by_token.insert(
                        token.to_string(),
                        Caller {
                            name: name.clone(),
                            grant: grant.clone(),
                        },
                    );
                }
                None => keyring.ungranted.push(name.clone()),
            }
        }

        keyring
    }

    pub fn admit(&self, presented: Option<&str>) -> Result<&Caller, Rejected> {
        let Some(token) = presented.map(str::trim).filter(|t| !t.is_empty()) else {
            return Err(Rejected::NoToken);
        };

        // Constant-time-ish: every entry is compared, so a wrong token cannot be
        // narrowed down by how quickly it was refused.
        let mut found: Option<&Caller> = None;
        for (known, caller) in &self.by_token {
            if constant_time_eq(known.as_bytes(), token.as_bytes()) {
                found = Some(caller);
            }
        }

        found.ok_or(Rejected::UnknownToken)
    }

    pub fn is_empty(&self) -> bool {
        self.by_token.is_empty()
    }

    pub fn names(&self) -> Vec<&str> {
        self.by_token.values().map(|c| c.name.as_str()).collect()
    }

    /// Tokens that were configured but cannot be used, and why. Reported at
    /// startup: a token silently ignored is worse than one refused loudly.
    pub fn problems(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .weak
            .iter()
            .map(|name| {
                format!(
                    "inbox token '{}' is shorter than {} characters and was ignored",
                    name, MIN_TOKEN_LENGTH
                )
            })
            .collect();

        out.extend(self.ungranted.iter().map(|name| {
            format!(
                "inbox token '{}' has no [inbox.grants.{}] in config.toml and was ignored",
                name, name
            )
        }));

        out
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn grants(names: &[&str]) -> BTreeMap<String, Grant> {
        names
            .iter()
            .map(|name| {
                (
                    name.to_string(),
                    Grant {
                        skills: vec!["telegram".to_string()],
                        new_skills: false,
                        risky: false,
                    },
                )
            })
            .collect()
    }

    const GOOD: &str = "0123456789abcdef0123456789abcdef";
    const OTHER: &str = "fedcba9876543210fedcba9876543210";

    #[test]
    fn a_known_token_is_admitted_with_its_own_grant() {
        let keyring = Keyring::build(&tokens(&[("telegram", GOOD)]), &grants(&["telegram"]));
        let caller = keyring.admit(Some(GOOD)).unwrap();

        assert_eq!(caller.name, "telegram");
        assert_eq!(caller.grant.skills, vec!["telegram"]);
    }

    #[test]
    fn an_unknown_token_gets_nothing() {
        let keyring = Keyring::build(&tokens(&[("telegram", GOOD)]), &grants(&["telegram"]));
        assert_eq!(keyring.admit(Some(OTHER)), Err(Rejected::UnknownToken));
    }

    #[test]
    fn no_token_at_all_is_refused() {
        let keyring = Keyring::build(&tokens(&[("telegram", GOOD)]), &grants(&["telegram"]));
        assert_eq!(keyring.admit(None), Err(Rejected::NoToken));
        assert_eq!(keyring.admit(Some("  ")), Err(Rejected::NoToken));
    }

    #[test]
    fn a_token_without_a_grant_is_never_admitted() {
        let keyring = Keyring::build(&tokens(&[("telegram", GOOD)]), &grants(&[]));

        assert!(keyring.is_empty(), "a caller got in without a grant");
        assert_eq!(keyring.admit(Some(GOOD)), Err(Rejected::UnknownToken));
        assert!(keyring.problems()[0].contains("no [inbox.grants.telegram]"));
    }

    #[test]
    fn a_short_token_is_refused_and_reported() {
        let keyring = Keyring::build(&tokens(&[("telegram", "hunter2")]), &grants(&["telegram"]));

        assert!(keyring.is_empty());
        assert!(keyring.problems()[0].contains("shorter than"));
    }

    #[test]
    fn each_caller_keeps_its_own_rights() {
        let mut wide = grants(&["relay"]);
        wide.insert(
            "relay".to_string(),
            Grant {
                skills: vec!["*".to_string()],
                new_skills: true,
                risky: false,
            },
        );
        wide.extend(grants(&["telegram"]));

        let keyring = Keyring::build(&tokens(&[("telegram", GOOD), ("relay", OTHER)]), &wide);

        assert!(!keyring.admit(Some(GOOD)).unwrap().grant.new_skills);
        assert!(keyring.admit(Some(OTHER)).unwrap().grant.new_skills);
    }

    #[test]
    fn revoking_one_token_leaves_the_others_working() {
        let keyring = Keyring::build(
            &tokens(&[("telegram", GOOD)]),
            &grants(&["telegram", "relay"]),
        );

        assert_eq!(keyring.names(), vec!["telegram"]);
        assert_eq!(keyring.admit(Some(OTHER)), Err(Rejected::UnknownToken));
    }

    #[test]
    fn an_empty_keyring_admits_nobody() {
        let keyring = Keyring::build(&tokens(&[]), &grants(&[]));
        assert!(keyring.is_empty());
        assert_eq!(keyring.admit(Some(GOOD)), Err(Rejected::UnknownToken));
    }

    #[test]
    fn comparison_does_not_leak_the_length_of_the_match() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"abcd"));
    }
}
