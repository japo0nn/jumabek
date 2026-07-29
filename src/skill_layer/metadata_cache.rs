use std::path::{Path, PathBuf};

use jumabek_sdk::{MethodInfo, ModuleMetadata};
use serde::{Deserialize, Serialize};

use crate::configs;
use crate::error::JumabekResult;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedSkill {
    pub metadata: ModuleMetadata,
    pub methods: Vec<MethodInfo>,
    binary_len: u64,
    binary_mtime: i64,
}

impl CachedSkill {
    pub fn matches(&self, binary: &Path) -> bool {
        match fingerprint(binary) {
            Some((len, mtime)) => self.binary_len == len && self.binary_mtime == mtime,
            None => false,
        }
    }
}

pub fn cache_dir() -> Option<PathBuf> {
    configs::jumabek_dir().map(|dir| dir.join("cache"))
}

fn cache_path(binary: &Path) -> Option<PathBuf> {
    let name = binary.file_stem()?.to_str()?;
    Some(cache_dir()?.join(format!("{}.json", name)))
}

fn fingerprint(binary: &Path) -> Option<(u64, i64)> {
    let meta = std::fs::metadata(binary).ok()?;
    let mtime = meta
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs() as i64;
    Some((meta.len(), mtime))
}

pub fn load(binary: &Path) -> Option<CachedSkill> {
    let path = cache_path(binary)?;
    let text = std::fs::read_to_string(path).ok()?;
    let cached: CachedSkill = serde_json::from_str(&text).ok()?;

    if cached.matches(binary) {
        Some(cached)
    } else {
        None
    }
}

pub fn store(
    binary: &Path,
    metadata: &ModuleMetadata,
    methods: &[MethodInfo],
) -> JumabekResult<()> {
    let Some(path) = cache_path(binary) else {
        return Ok(());
    };
    let Some((binary_len, binary_mtime)) = fingerprint(binary) else {
        return Ok(());
    };

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let entry = CachedSkill {
        metadata: metadata.clone(),
        methods: methods.to_vec(),
        binary_len,
        binary_mtime,
    };

    if let Ok(text) = serde_json::to_string_pretty(&entry) {
        std::fs::write(path, text)?;
    }

    Ok(())
}

pub fn forget(binary: &Path) {
    if let Some(path) = cache_path(binary) {
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> (ModuleMetadata, Vec<MethodInfo>) {
        (
            ModuleMetadata {
                name: "demo".to_string(),
                version: "1.0.0".to_string(),
                description: "does something".to_string(),
            },
            vec![MethodInfo {
                method: "run".to_string(),
                description: "runs it".to_string(),
                args_description: "nothing".to_string(),
            }],
        )
    }

    fn temp_binary(name: &str, contents: &[u8]) -> PathBuf {
        let dir = std::env::temp_dir().join("jb_meta_cache");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, contents).unwrap();
        path
    }

    #[test]
    fn a_stored_entry_comes_back() {
        let binary = temp_binary("roundtrip.bin", b"v1");
        let (metadata, methods) = sample();

        store(&binary, &metadata, &methods).unwrap();
        let cached = load(&binary).expect("nothing cached");

        assert_eq!(cached.metadata.name, "demo");
        assert_eq!(cached.methods.len(), 1);
    }

    #[test]
    fn a_rebuilt_binary_invalidates_its_cache() {
        let binary = temp_binary("rebuilt.bin", b"v1");
        let (metadata, methods) = sample();
        store(&binary, &metadata, &methods).unwrap();
        assert!(load(&binary).is_some());

        std::thread::sleep(std::time::Duration::from_millis(1100));
        std::fs::write(&binary, b"v2-is-longer").unwrap();

        assert!(
            load(&binary).is_none(),
            "stale metadata was served for a rebuilt skill"
        );
    }

    #[test]
    fn nothing_cached_is_not_an_error() {
        let binary = temp_binary("never_stored.bin", b"x");
        forget(&binary);
        assert!(load(&binary).is_none());
    }
}
