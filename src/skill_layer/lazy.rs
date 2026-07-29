use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use jumabek_sdk::{MethodInfo, ModuleMetadata, SkillError, SkillModule, SkillOutput};
use tokio::sync::Mutex;

use crate::skill_layer::metadata_cache;
use crate::skill_layer::rpc_client::SkillRpcClient;

pub struct LazySkill {
    binary: PathBuf,
    metadata: ModuleMetadata,
    methods: Vec<MethodInfo>,
    settings: BTreeMap<String, String>,
    timeout: Duration,
    client: Mutex<Option<SkillRpcClient>>,
}

impl LazySkill {
    pub fn new(
        binary: PathBuf,
        metadata: ModuleMetadata,
        methods: Vec<MethodInfo>,
        settings: BTreeMap<String, String>,
        timeout: Duration,
    ) -> Self {
        LazySkill {
            binary,
            metadata,
            methods,
            settings,
            timeout,
            client: Mutex::new(None),
        }
    }

    pub async fn probe(
        binary: PathBuf,
        settings: BTreeMap<String, String>,
        timeout: Duration,
    ) -> crate::error::JumabekResult<Self> {
        let client = SkillRpcClient::spawn_with_settings(&binary, settings.clone())
            .await?
            .with_timeout(timeout);

        let metadata = client.get_metadata_cached().clone();
        let methods = client.methods_cached().to_vec();
        metadata_cache::store(&binary, &metadata, &methods)?;

        Ok(LazySkill {
            binary,
            metadata,
            methods,
            settings,
            timeout,
            client: Mutex::new(Some(client)),
        })
    }

    #[cfg(test)]
    pub fn is_running(&self) -> bool {
        self.client.try_lock().map(|c| c.is_some()).unwrap_or(true)
    }
}

#[async_trait::async_trait]
impl SkillModule for LazySkill {
    fn get_metadata(&self) -> &ModuleMetadata {
        &self.metadata
    }

    fn health_check(&self) -> bool {
        true
    }

    fn available_methods(&self) -> Vec<MethodInfo> {
        self.methods.clone()
    }

    async fn execute(&self, method: &str, args: &str) -> Result<SkillOutput, SkillError> {
        let mut slot = self.client.lock().await;

        if slot.is_none() {
            let started = SkillRpcClient::spawn_with_settings(&self.binary, self.settings.clone())
                .await
                .map_err(|e| {
                    SkillError::ExecutionFailed(format!(
                        "cannot start '{}': {}",
                        self.metadata.name, e
                    ))
                })?
                .with_timeout(self.timeout);

            let _ = metadata_cache::store(
                &self.binary,
                started.get_metadata_cached(),
                started.methods_cached(),
            );

            *slot = Some(started);
        }

        let client = slot.as_ref().expect("just started");
        client.execute(method, args).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn skill(binary: &str) -> LazySkill {
        LazySkill::new(
            PathBuf::from(binary),
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
            BTreeMap::new(),
            Duration::from_secs(5),
        )
    }

    #[test]
    fn metadata_is_available_without_starting_anything() {
        let lazy = skill("does/not/exist");

        assert_eq!(lazy.get_metadata().name, "demo");
        assert_eq!(lazy.available_methods().len(), 1);
        assert!(!lazy.is_running(), "the binary was started too early");
    }

    #[tokio::test]
    async fn a_missing_binary_only_fails_when_it_is_actually_called() {
        let lazy = skill("does/not/exist");
        assert!(!lazy.is_running());

        let result = lazy.execute("run", "").await;
        assert!(result.is_err(), "a missing binary answered a call");
    }
}
