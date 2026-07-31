use std::path::{Path, PathBuf};

use jumabek_sdk::SkillModule;

use crate::error::JumabekResult;
use crate::skill_layer::SkillRegistry;
use crate::skill_layer::lazy::LazySkill;
use crate::skill_layer::metadata_cache;

pub fn skills_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(|home| PathBuf::from(home).join(".jumabek").join("skills"))
}

/// Where an installed skill's binary sits, by name. Used when a skill has to be
/// started again with different settings.
pub fn binary_for(name: &str) -> Option<PathBuf> {
    let dir = skills_dir()?;
    let candidate = dir.join(if cfg!(windows) {
        format!("{}.exe", name)
    } else {
        name.to_string()
    });

    candidate.is_file().then_some(candidate)
}

fn is_executable(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }

    #[cfg(windows)]
    {
        matches!(
            path.extension().and_then(|e| e.to_str()),
            Some(ext) if ext.eq_ignore_ascii_case("exe")
        )
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        path.metadata()
            .map(|m| m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
}

pub fn discover(dir: &Path) -> JumabekResult<Vec<PathBuf>> {
    if !dir.is_dir() {
        return Ok(Vec::new());
    }

    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if is_executable(&path) {
            found.push(path);
        }
    }
    found.sort();
    Ok(found)
}

pub async fn load_into(
    registry: &mut SkillRegistry,
    dir: &Path,
    timeout: std::time::Duration,
    settings_for: &dyn Fn(&str) -> std::collections::BTreeMap<String, String>,
) -> JumabekResult<usize> {
    let mut loaded = 0;

    for path in discover(dir)? {
        let name = path
            .file_stem()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string();
        let settings = settings_for(&name);

        if let Some(cached) = metadata_cache::load(&path) {
            registry.register(Box::new(LazySkill::new(
                path.clone(),
                cached.metadata,
                cached.methods,
                settings,
                timeout,
            )) as Box<dyn SkillModule>);
            loaded += 1;
            continue;
        }

        match LazySkill::probe(path.clone(), settings, timeout).await {
            Ok(skill) => {
                registry.register(Box::new(skill) as Box<dyn SkillModule>);
                loaded += 1;
            }
            Err(e) => {
                eprintln!("[skill_layer] skipped '{}': {}", path.display(), e);
                metadata_cache::forget(&path);
            }
        }
    }

    Ok(loaded)
}

pub async fn load_default(
    registry: &mut SkillRegistry,
    timeout: std::time::Duration,
    settings_for: &dyn Fn(&str) -> std::collections::BTreeMap<String, String>,
) -> JumabekResult<usize> {
    match skills_dir() {
        Some(dir) => load_into(registry, &dir, timeout, settings_for).await,
        None => Ok(0),
    }
}
