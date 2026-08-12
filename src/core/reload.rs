use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;

use crate::configs;
use crate::core::agent::Agent;
use crate::core::inbox::Inbox;
use crate::core::scheduler::Notifier;

const POLL: std::time::Duration = std::time::Duration::from_secs(3);

pub fn watched() -> Vec<PathBuf> {
    ["config.toml", "secrets.toml", "prompt.md"]
        .into_iter()
        .filter_map(|name| configs::find_file(name).ok())
        .collect()
}

fn stamps(paths: &[PathBuf]) -> Vec<Option<SystemTime>> {
    paths
        .iter()
        .map(|path| std::fs::metadata(path).ok().and_then(|m| m.modified().ok()))
        .collect()
}

pub fn watch(agent: Arc<Agent>, inbox: Option<Arc<Inbox>>, notifier: Arc<dyn Notifier>) {
    tokio::spawn(async move {
        let paths = watched();
        if paths.is_empty() {
            return;
        }

        let mut seen = stamps(&paths);

        loop {
            tokio::time::sleep(POLL).await;

            let now = stamps(&paths);
            if now == seen {
                continue;
            }

            let touched: Vec<String> = paths
                .iter()
                .zip(seen.iter().zip(now.iter()))
                .filter(|(_, (before, after))| before != after)
                .filter_map(|(path, _)| path.file_name().map(|n| n.to_string_lossy().to_string()))
                .collect();

            seen = now;

            tokio::time::sleep(std::time::Duration::from_millis(300)).await;

            match agent.reload().await {
                Ok(changes) => {
                    notifier.notify(format!("  · reloaded {}", touched.join(", ")));
                    for change in &changes {
                        notifier.notify(format!("  ·   {}", change));
                    }
                    if changes.is_empty() {
                        notifier.notify("  ·   nothing that matters changed".to_string());
                    }

                    if let Some(inbox) = &inbox {
                        match inbox.reload_keyring(&agent.inbox_grants().await) {
                            Ok(callers) if !callers.is_empty() => notifier
                                .notify(format!("  ·   inbox now admits {}", callers.join(", "))),
                            Ok(_) => notifier.notify("  ·   inbox admits nobody now".to_string()),
                            Err(problems) => {
                                for problem in problems {
                                    notifier.notify(format!("  x   {}", problem));
                                }
                            }
                        }
                    }
                }
                Err(e) => notifier.notify(format!(
                    "  x {} was saved but cannot be used: {}",
                    touched.join(", "),
                    e
                )),
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_file_has_no_stamp_and_does_not_panic() {
        let stamps = stamps(&[PathBuf::from("definitely-not-here.toml")]);
        assert_eq!(stamps, vec![None]);
    }

    #[test]
    fn a_touched_file_reads_as_different() {
        let dir = std::env::temp_dir().join("jb_reload_test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(&path, "a = 1").unwrap();

        let before = stamps(std::slice::from_ref(&path));
        std::thread::sleep(std::time::Duration::from_millis(1100));
        std::fs::write(&path, "a = 2").unwrap();
        let after = stamps(std::slice::from_ref(&path));

        assert_ne!(before, after, "a rewritten file looked unchanged");
    }
}
