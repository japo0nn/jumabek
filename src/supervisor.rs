use std::path::{Path, PathBuf};

use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::configs;
use crate::error::{JumabekError, JumabekResult};

pub const KEEP_SNAPSHOTS: usize = 10;
const MANIFEST: &str = "snapshot.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub id: String,
    pub created_at: String,
    pub reason: String,
    pub files: Vec<String>,
}

pub struct Supervisor {
    root: PathBuf,
}

impl Supervisor {
    pub fn open() -> JumabekResult<Self> {
        let root = configs::jumabek_dir().ok_or_else(|| {
            JumabekError::ConfigError("cannot resolve home directory".to_string())
        })?;
        std::fs::create_dir_all(root.join("backups"))?;
        Ok(Supervisor { root })
    }

    #[cfg(test)]
    pub fn at(root: impl Into<PathBuf>) -> JumabekResult<Self> {
        let root = root.into();
        std::fs::create_dir_all(root.join("backups"))?;
        Ok(Supervisor { root })
    }

    fn backups_dir(&self) -> PathBuf {
        self.root.join("backups")
    }

    fn log_path(&self) -> PathBuf {
        self.root.join("supervisor.log")
    }

    pub fn log_event(&self, event: &str) {
        let line = format!("{} {}\n", Utc::now().to_rfc3339(), event);
        if let Err(e) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.log_path())
            .and_then(|mut file| std::io::Write::write_all(&mut file, line.as_bytes()))
        {
            eprintln!("[supervisor] cannot write the event log: {}", e);
        }
    }

    pub fn snapshot(&self, reason: &str) -> JumabekResult<Snapshot> {
        let id = format!(
            "{}_{}",
            Utc::now().format("%Y%m%dT%H%M%S"),
            sanitise(reason)
        );
        let target = self.backups_dir().join(&id);
        std::fs::create_dir_all(&target)?;

        let mut files = Vec::new();

        for name in ["config.toml", "prompt.md"] {
            let source = self.root.join(name);
            if source.is_file() {
                std::fs::copy(&source, target.join(name))?;
                files.push(name.to_string());
            }
        }

        let skills = self.root.join("skills");
        if skills.is_dir() {
            let copied = copy_dir(&skills, &target.join("skills"))?;
            for name in copied {
                files.push(format!("skills/{}", name));
            }
        }

        let snapshot = Snapshot {
            id: id.clone(),
            created_at: Utc::now().to_rfc3339(),
            reason: reason.to_string(),
            files,
        };

        std::fs::write(
            target.join(MANIFEST),
            serde_json::to_string_pretty(&snapshot).map_err(|e| {
                JumabekError::InternalError(format!("cannot write manifest: {}", e))
            })?,
        )?;

        self.log_event(&format!(
            "snapshot {} taken ({} file(s)): {}",
            snapshot.id,
            snapshot.files.len(),
            reason
        ));

        self.prune(KEEP_SNAPSHOTS)?;
        Ok(snapshot)
    }

    pub fn list(&self) -> JumabekResult<Vec<Snapshot>> {
        let dir = self.backups_dir();
        if !dir.is_dir() {
            return Ok(Vec::new());
        }

        let mut snapshots = Vec::new();
        for entry in std::fs::read_dir(&dir)? {
            let path = entry?.path();
            let manifest = path.join(MANIFEST);
            if !manifest.is_file() {
                continue;
            }
            let text = std::fs::read_to_string(&manifest)?;
            if let Ok(snapshot) = serde_json::from_str::<Snapshot>(&text) {
                snapshots.push(snapshot);
            }
        }

        snapshots.sort_by(|a, b| b.id.cmp(&a.id));
        Ok(snapshots)
    }

    pub fn restore(&self, id: &str) -> JumabekResult<Snapshot> {
        let source = self.backups_dir().join(id);
        let manifest = source.join(MANIFEST);

        if !manifest.is_file() {
            return Err(JumabekError::ConfigError(format!(
                "no snapshot called '{}'",
                id
            )));
        }

        let snapshot: Snapshot = serde_json::from_str(&std::fs::read_to_string(&manifest)?)
            .map_err(|e| JumabekError::InternalError(format!("broken manifest: {}", e)))?;

        self.snapshot("before-restore")?;

        for relative in &snapshot.files {
            let from = source.join(relative);
            let to = self.root.join(relative);

            if !from.is_file() {
                continue;
            }
            if let Some(parent) = to.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(&from, &to)?;
        }

        let removed = self.remove_skills_absent_from(&snapshot)?;

        self.log_event(&format!(
            "restored snapshot {} ({} file(s) put back, {} removed)",
            snapshot.id,
            snapshot.files.len(),
            removed
        ));

        Ok(snapshot)
    }

    fn remove_skills_absent_from(&self, snapshot: &Snapshot) -> JumabekResult<usize> {
        let skills = self.root.join("skills");
        if !skills.is_dir() {
            return Ok(0);
        }

        let kept: Vec<&str> = snapshot
            .files
            .iter()
            .filter_map(|f| f.strip_prefix("skills/"))
            .collect();

        let mut removed = 0;
        for entry in std::fs::read_dir(&skills)? {
            let path = entry?.path();
            if !path.is_file() {
                continue;
            }
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if kept.contains(&name) {
                continue;
            }
            if std::fs::remove_file(&path).is_ok() {
                removed += 1;
            }
        }

        Ok(removed)
    }

    pub fn prune(&self, keep: usize) -> JumabekResult<usize> {
        let snapshots = self.list()?;
        if snapshots.len() <= keep {
            return Ok(0);
        }

        let mut removed = 0;
        for snapshot in snapshots.into_iter().skip(keep) {
            let path = self.backups_dir().join(&snapshot.id);
            if std::fs::remove_dir_all(&path).is_ok() {
                removed += 1;
            }
        }

        if removed > 0 {
            self.log_event(&format!("pruned {} old snapshot(s)", removed));
        }

        Ok(removed)
    }
}

fn sanitise(text: &str) -> String {
    let cleaned: String = text
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();

    cleaned.trim_matches('-').chars().take(40).collect()
}

fn copy_dir(from: &Path, to: &Path) -> JumabekResult<Vec<String>> {
    std::fs::create_dir_all(to)?;

    let mut copied = Vec::new();
    for entry in std::fs::read_dir(from)? {
        let path = entry?.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        std::fs::copy(&path, to.join(name))?;
        copied.push(name.to_string());
    }

    copied.sort();
    Ok(copied)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("jb_supervisor_{}", name));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("skills")).unwrap();
        std::fs::write(dir.join("config.toml"), "original config").unwrap();
        std::fs::write(dir.join("prompt.md"), "original prompt").unwrap();
        std::fs::write(dir.join("skills/alpha.exe"), b"v1").unwrap();
        dir
    }

    #[test]
    fn a_snapshot_captures_skills_config_and_prompt() {
        let dir = scratch("capture");
        let supervisor = Supervisor::at(&dir).unwrap();

        let snapshot = supervisor.snapshot("before-build").unwrap();

        assert!(snapshot.files.contains(&"config.toml".to_string()));
        assert!(snapshot.files.contains(&"prompt.md".to_string()));
        assert!(snapshot.files.contains(&"skills/alpha.exe".to_string()));
        assert!(snapshot.reason.contains("before-build"));
    }

    #[test]
    fn secrets_are_never_copied_into_a_backup() {
        let dir = scratch("secrets");
        std::fs::write(dir.join("secrets.toml"), "api_key = \"leak me\"").unwrap();
        std::fs::write(dir.join("jumabek.db"), b"user history").unwrap();

        let supervisor = Supervisor::at(&dir).unwrap();
        let snapshot = supervisor.snapshot("check").unwrap();

        assert!(
            !snapshot.files.iter().any(|f| f.contains("secrets")),
            "secrets ended up in a backup: {:?}",
            snapshot.files
        );

        let copied = std::fs::read_dir(dir.join("backups").join(&snapshot.id))
            .unwrap()
            .filter_map(|e| e.ok())
            .any(|e| e.file_name().to_string_lossy().contains("secrets"));
        assert!(!copied, "a secrets file was written into the backup folder");
    }

    #[test]
    fn restore_puts_the_old_skill_back() {
        let dir = scratch("restore");
        let supervisor = Supervisor::at(&dir).unwrap();

        let snapshot = supervisor.snapshot("good-state").unwrap();

        std::fs::write(dir.join("skills/alpha.exe"), b"v2-broken").unwrap();
        std::fs::write(dir.join("config.toml"), "mangled").unwrap();

        supervisor.restore(&snapshot.id).unwrap();

        assert_eq!(
            std::fs::read(dir.join("skills/alpha.exe")).unwrap(),
            b"v1".to_vec()
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("config.toml")).unwrap(),
            "original config"
        );
    }

    #[test]
    fn restoring_first_saves_what_it_is_about_to_overwrite() {
        let dir = scratch("safety_net");
        let supervisor = Supervisor::at(&dir).unwrap();
        let good = supervisor.snapshot("good").unwrap();

        std::fs::write(dir.join("skills/alpha.exe"), b"v2").unwrap();
        supervisor.restore(&good.id).unwrap();

        let snapshots = supervisor.list().unwrap();
        assert!(
            snapshots.iter().any(|s| s.reason == "before-restore"),
            "no way back from the restore: {:?}",
            snapshots.iter().map(|s| &s.reason).collect::<Vec<_>>()
        );
    }

    #[test]
    fn restore_removes_a_skill_that_did_not_exist_back_then() {
        let dir = scratch("undo_add");
        let supervisor = Supervisor::at(&dir).unwrap();

        let before = supervisor.snapshot("before-adding").unwrap();

        std::fs::write(dir.join("skills/beta.exe"), b"new skill").unwrap();
        assert!(dir.join("skills/beta.exe").exists());

        supervisor.restore(&before.id).unwrap();

        assert!(
            !dir.join("skills/beta.exe").exists(),
            "the added skill survived the rollback"
        );
        assert!(
            dir.join("skills/alpha.exe").exists(),
            "the original skill was wiped too"
        );
    }

    #[test]
    fn restoring_something_that_does_not_exist_says_so() {
        let dir = scratch("missing");
        let supervisor = Supervisor::at(&dir).unwrap();
        let err = supervisor.restore("20990101T000000_nope").unwrap_err();
        assert!(err.to_string().contains("no snapshot"), "{}", err);
    }

    #[test]
    fn old_snapshots_are_pruned_newest_first() {
        let dir = scratch("prune");
        let supervisor = Supervisor::at(&dir).unwrap();

        for i in 0..5 {
            std::fs::create_dir_all(dir.join("backups").join(format!("2026010{}T000000_x", i)))
                .unwrap();
            let manifest = Snapshot {
                id: format!("2026010{}T000000_x", i),
                created_at: "2026-01-01T00:00:00Z".to_string(),
                reason: "x".to_string(),
                files: Vec::new(),
            };
            std::fs::write(
                dir.join("backups")
                    .join(format!("2026010{}T000000_x", i))
                    .join(MANIFEST),
                serde_json::to_string(&manifest).unwrap(),
            )
            .unwrap();
        }

        supervisor.prune(2).unwrap();
        let left = supervisor.list().unwrap();

        assert_eq!(left.len(), 2);
        assert_eq!(left[0].id, "20260104T000000_x", "kept the wrong ones");
    }

    #[test]
    fn events_survive_in_their_own_file() {
        let dir = scratch("log");
        let supervisor = Supervisor::at(&dir).unwrap();

        supervisor.log_event("startup");
        supervisor.log_event("shutdown");

        let log = std::fs::read_to_string(dir.join("supervisor.log")).unwrap();
        assert!(log.contains("startup"));
        assert!(log.contains("shutdown"));
        assert_eq!(log.lines().count(), 2);
    }

    #[test]
    fn a_reason_cannot_escape_the_backups_folder() {
        assert_eq!(sanitise("../../etc/passwd"), "etc-passwd");
        assert_eq!(sanitise("before build"), "before-build");
    }
}
