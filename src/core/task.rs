use serde::{Deserialize, Deserializer, Serialize};

fn flexible_string_vec<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(match value {
        serde_json::Value::Array(items) => items
            .into_iter()
            .map(|item| match item {
                serde_json::Value::String(text) => text,
                other => other.to_string(),
            })
            .collect(),
        serde_json::Value::String(text) => vec![text],
        serde_json::Value::Null => Vec::new(),
        other => vec![other.to_string()],
    })
}

fn flexible_string<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(match value {
        serde_json::Value::String(text) => text,
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemInfo {
    pub os: String,
    pub shell: String,
    pub current_time: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskObjectSkillMethod {
    pub method: String,
    pub description: String,
    pub args_description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskObjectSkill {
    pub name: String,
    pub description: String,
    pub available_methods: Vec<TaskObjectSkillMethod>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Constraints {
    pub max_iterations: u32,
    pub max_fix_iterations: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Grant {
    #[serde(default, deserialize_with = "flexible_string_vec")]
    pub skills: Vec<String>,
    #[serde(default)]
    pub new_skills: bool,
    #[serde(default)]
    pub risky: bool,
}

impl Grant {
    pub fn allows(&self, skill: &str) -> bool {
        self.skills.iter().any(|s| s == skill || s == "*")
    }

    pub fn describe(&self) -> String {
        let skills = if self.skills.is_empty() {
            "no skills".to_string()
        } else {
            self.skills.join(", ")
        };

        let mut extras: Vec<&str> = Vec::new();
        if self.new_skills {
            extras.push("may write new skills");
        }
        if self.risky {
            extras.push("may run commands the safety rules stop");
        }

        if extras.is_empty() {
            skills
        } else {
            format!("{}; {}", skills, extras.join("; "))
        }
    }
}

/// Where a task came from, when it did not come from the person at the terminal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Origin {
    pub source: String,
    pub who: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskObject {
    pub task_id: String,
    pub parent_task_id: Option<String>,
    pub message: String,
    pub system_info: SystemInfo,
    pub system_response: Option<String>,
    pub skills: Vec<TaskObjectSkill>,
    pub capabilities: Vec<String>,
    pub constraints: Constraints,
    pub iteration: u32,
    pub fix_iteration: u32,
    pub depth: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grant: Option<Grant>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin: Option<Origin>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub intelligence: Option<crate::core::intelligence::Standing>,
    pub interface_mode: InterfaceMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InterfaceMode {
    Cli,
    Voice,
}

impl InterfaceMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            InterfaceMode::Cli => "cli",
            InterfaceMode::Voice => "voice",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Choice {
    #[serde(default, deserialize_with = "flexible_string")]
    pub label: String,
    #[serde(default, deserialize_with = "flexible_string")]
    pub value: String,
}

impl Choice {
    #[cfg(test)]
    pub fn new(label: impl Into<String>, value: impl Into<String>) -> Self {
        Choice {
            label: label.into(),
            value: value.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ActionType {
    ExecuteModule {
        module: String,
        method: String,
        #[serde(default, deserialize_with = "flexible_string")]
        args: String,
        #[serde(default)]
        parallel: bool,
    },
    #[serde(alias = "Respond", alias = "Answer")]
    RespondToUser,
    #[serde(alias = "RequestPermission", alias = "AskPermission")]
    PermissionRequest {
        #[serde(default, deserialize_with = "flexible_string")]
        action: String,
        #[serde(default, deserialize_with = "flexible_string")]
        description: String,
        #[serde(default, deserialize_with = "flexible_string")]
        risk_level: String,
    },
    #[serde(alias = "PromptUser", alias = "AskUser")]
    PromptToUser {
        #[serde(default, deserialize_with = "flexible_string")]
        message: String,
        #[serde(default)]
        options: Vec<Choice>,
    },
    RequestData {
        #[serde(default, deserialize_with = "flexible_string")]
        source: String,
        #[serde(default, deserialize_with = "flexible_string")]
        query: String,
        #[serde(default = "default_request_limit")]
        limit: u32,
    },
    #[serde(alias = "RequestInboxAccess", alias = "AskForInboxKey")]
    RequestInboxKey {
        #[serde(default, deserialize_with = "flexible_string")]
        module: String,
        #[serde(default, deserialize_with = "flexible_string")]
        why: String,
        #[serde(default, deserialize_with = "flexible_string_vec")]
        skills: Vec<String>,
    },
    #[serde(alias = "Memorise", alias = "Memorize", alias = "SaveFact")]
    Remember {
        #[serde(default, deserialize_with = "flexible_string")]
        subject: String,
        #[serde(default, deserialize_with = "flexible_string")]
        key: String,
        #[serde(default, deserialize_with = "flexible_string")]
        value: String,
        #[serde(default, deserialize_with = "flexible_string")]
        note: String,
    },
    #[serde(alias = "ForgetFact")]
    Forget {
        #[serde(default, deserialize_with = "flexible_string")]
        subject: String,
        #[serde(default, deserialize_with = "flexible_string")]
        key: String,
    },
    #[serde(alias = "CreateJob", alias = "Schedule", alias = "Remind")]
    ScheduleJob {
        #[serde(default, deserialize_with = "flexible_string")]
        name: String,
        #[serde(default, deserialize_with = "flexible_string")]
        task: String,
        #[serde(default, deserialize_with = "flexible_string")]
        schedule: String,
        #[serde(default)]
        grant: Grant,
    },
    #[serde(alias = "StopJob", alias = "ListJobs")]
    ManageJobs {
        #[serde(default, deserialize_with = "flexible_string")]
        operation: String,
        #[serde(default)]
        id: i64,
    },
    #[serde(
        alias = "SetIntelligence",
        alias = "SwitchLevel",
        alias = "SwitchModel"
    )]
    Switch {
        #[serde(default, deserialize_with = "flexible_string")]
        level: String,
        #[serde(default, deserialize_with = "flexible_string")]
        why: String,
    },
    #[serde(alias = "Spawn", alias = "SubAgent", alias = "SpawnSubAgent")]
    SpawnAgent {
        #[serde(default, deserialize_with = "flexible_string")]
        task: String,
        #[serde(default, deserialize_with = "flexible_string")]
        reason: String,
    },
    GenerateChunk {
        module_name: String,
        chunk_index: u32,
        total_chunks: u32,
        #[serde(default, deserialize_with = "flexible_string")]
        code_chunk: String,
        #[serde(default, deserialize_with = "flexible_string_vec")]
        dependencies: Vec<String>,
        /// `rust`, `python` or `node`.
        #[serde(default, deserialize_with = "flexible_string")]
        language: String,
    },
}

fn default_request_limit() -> u32 {
    5
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentResponse {
    #[serde(default, deserialize_with = "flexible_string")]
    pub message: String,
    #[serde(default)]
    pub is_done: bool,
    #[serde(default)]
    pub actions: Vec<ActionType>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmMessage {
    pub role: String,
    pub content: String,
}
