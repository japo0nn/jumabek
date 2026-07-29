use chrono::Local;
use jumabek_sdk::{SkillError, SkillOutput};

use crate::configs::Config;
use crate::core::context::ContextBuilder;
use crate::core::llm::LlmClient;
use crate::core::planner;
use crate::core::safety;
use crate::core::self_improvement::{Chunk, Outcome, Progress, SelfImprovement};
use crate::core::task::{
    ActionType, AgentResponse, Constraints, InterfaceMode, SystemInfo, TaskObject, TaskObjectSkill,
    TaskObjectSkillMethod,
};
use crate::error::{JumabekError, JumabekResult};
use crate::interfaces::UserInterface;
use crate::memory::{Memory, NewMessage, Role};
use crate::skill_layer::SkillRegistry;
use crate::skill_layer::rpc_client::SkillRpcClient;
use jumabek_sdk::SkillModule;
use tokio::sync::RwLock;

const INDEXED_CONTENT_LIMIT: usize = 2_000;

const EXPAND_EVERYTHING_BELOW: usize = 4;

const PARSE_RETRIES: u32 = 2;

const PARSE_CORRECTION: &str = "Your previous answer could not be read as an agent response and \
     was discarded. Answer the same request again. Reply with one JSON object and nothing else: no \
     prose before or after it, no markdown fence. If the last answer was cut off, make this one \
     shorter.";

const CAPABILITIES: [&str; 6] = [
    "ExecuteModule",
    "PermissionRequest",
    "PromptToUser",
    "RequestData",
    "GenerateChunk",
    "RespondToUser",
];

pub struct Agent {
    config: Config,
    memory: Memory,
    registry: RwLock<SkillRegistry>,
    engine: SelfImprovement,
    expanded: RwLock<std::collections::HashSet<String>>,
    llm: LlmClient,
    context: ContextBuilder,
    mode: InterfaceMode,
}

enum StepOutcome {
    Continue(String),
    Finished,
    Aborted(String),
}

impl Agent {
    pub fn new(
        config: Config,
        memory: Memory,
        registry: SkillRegistry,
        mode: InterfaceMode,
    ) -> JumabekResult<Self> {
        let llm = LlmClient::new(&config)?;
        let context =
            ContextBuilder::new(config.system_prompt.clone(), config.llm.context_token_limit);

        Ok(Agent {
            config,
            memory,
            registry: RwLock::new(registry),
            engine: SelfImprovement::new(),
            expanded: RwLock::new(std::collections::HashSet::new()),
            llm,
            context,
            mode,
        })
    }

    pub fn set_mode(&mut self, mode: InterfaceMode) {
        self.mode = mode;
    }

    pub fn memory(&self) -> &Memory {
        &self.memory
    }

    pub async fn handle(&self, ui: &mut dyn UserInterface, request: String) -> JumabekResult<()> {
        let task_id = uuid::Uuid::new_v4().to_string();
        let mut task = self.new_task(&task_id, request).await;
        let step = self.config.agent.max_iterations;
        let mut budget = step;

        loop {
            let history = self.memory.current_session().await?;
            let built = self.context.build(&history, &task)?;

            if built.trimmed_messages > 0 {
                ui.show_status(&format!(
                    "context {} tokens, trimmed: {} older messages hidden",
                    built.total_tokens, built.trimmed_messages
                ))
                .await?;
            } else if built.total_tokens * 2 > self.config.llm.context_token_limit as usize {
                ui.show_status(&format!(
                    "context {} of {} tokens",
                    built.total_tokens, self.config.llm.context_token_limit
                ))
                .await?;
            }

            let reply = self.ask_until_readable(ui, &built.messages).await?;
            self.log_turn(&task, &reply.response, &reply.raw_content)
                .await?;

            if !reply.response.message.trim().is_empty() {
                ui.show_response(&reply.response.message).await?;
            }

            match self.run_actions(ui, &task, &reply.response).await? {
                StepOutcome::Finished => return Ok(()),
                StepOutcome::Aborted(reason) => {
                    ui.show_error(&reason).await?;
                    return Ok(());
                }
                StepOutcome::Continue(system_response) => {
                    task.iteration += 1;
                    if task.iteration >= budget {
                        if !self.ask_for_more_iterations(ui, &task, budget).await? {
                            return Ok(());
                        }
                        budget += step;
                        task.constraints.max_iterations = budget;
                    }
                    task.system_response = Some(system_response);
                }
            }
        }
    }

    /// Runs out of iterations by asking rather than by stopping. The agent has
    /// no way to tell a task that is stuck from one that is merely long, so the
    /// judgement belongs to whoever asked for it.
    async fn ask_for_more_iterations(
        &self,
        ui: &mut dyn UserInterface,
        task: &TaskObject,
        used: u32,
    ) -> JumabekResult<bool> {
        let carry_on = ui
            .ask_permission(
                "keep going",
                &format!(
                    "{} iterations have been used and the task is not finished. \
                     Allowing this grants {} more.",
                    used, self.config.agent.max_iterations
                ),
                "low",
            )
            .await?;

        let note = if carry_on {
            format!("user granted more iterations past {}", used)
        } else {
            format!("user stopped the task at {} iterations", used)
        };

        self.memory
            .log(NewMessage::new(Role::System, &note).task(&task.task_id))
            .await?;

        if !carry_on {
            ui.show_status(&format!("stopped at {} iterations", used))
                .await?;
        }

        Ok(carry_on)
    }

    /// An answer that cannot be parsed is thrown away rather than recorded, and
    /// the same request goes back with a note about what was wrong. Writing it
    /// to memory would leave the model reading its own broken output for the
    /// rest of the session.
    async fn ask_until_readable(
        &self,
        ui: &mut dyn UserInterface,
        messages: &[crate::core::task::LlmMessage],
    ) -> JumabekResult<crate::core::llm::LlmReply> {
        let mut attempt = 0;

        loop {
            let mut sent = messages.to_vec();
            if attempt > 0 {
                sent.push(crate::core::task::LlmMessage {
                    role: "system".to_string(),
                    content: PARSE_CORRECTION.to_string(),
                });
            }

            match self.llm.ask(&sent).await {
                Ok(reply) => return Ok(reply),
                Err(JumabekError::ParseError(detail)) if attempt < PARSE_RETRIES => {
                    attempt += 1;
                    ui.show_status(&format!(
                        "unreadable answer, asking again ({}/{}): {}",
                        attempt,
                        PARSE_RETRIES,
                        first_line(&detail)
                    ))
                    .await?;
                }
                Err(e) => return Err(e),
            }
        }
    }

    async fn new_task(&self, task_id: &str, request: String) -> TaskObject {
        TaskObject {
            task_id: task_id.to_string(),
            parent_task_id: None,
            message: request,
            system_info: system_info(),
            system_response: None,
            skills: self.skill_descriptions().await,
            capabilities: CAPABILITIES.iter().map(|c| c.to_string()).collect(),
            constraints: Constraints {
                max_iterations: self.config.agent.max_iterations,
                max_fix_iterations: self.config.agent.max_fix_iterations,
            },
            iteration: 0,
            fix_iteration: 0,
            interface_mode: self.mode,
        }
    }

    async fn skill_descriptions(&self) -> Vec<TaskObjectSkill> {
        let registry = self.registry.read().await;
        let expanded = self.expanded.read().await;

        let total = registry.list().len();
        let expand_all = total < EXPAND_EVERYTHING_BELOW;

        registry
            .all()
            .map(|skill| {
                let metadata = skill.get_metadata();
                let show_methods = expand_all || expanded.contains(&metadata.name);

                TaskObjectSkill {
                    name: metadata.name.clone(),
                    description: metadata.description.clone(),
                    available_methods: if show_methods {
                        skill
                            .available_methods()
                            .into_iter()
                            .map(|m| TaskObjectSkillMethod {
                                method: m.method,
                                description: m.description,
                                args_description: m.args_description,
                            })
                            .collect()
                    } else {
                        Vec::new()
                    },
                }
            })
            .collect()
    }

    async fn expand_skill(&self, name: &str) -> bool {
        if self.registry.read().await.get(name).is_none() {
            return false;
        }
        self.expanded.write().await.insert(name.to_string());
        true
    }

    async fn log_turn(
        &self,
        task: &TaskObject,
        response: &AgentResponse,
        raw_content: &str,
    ) -> JumabekResult<()> {
        let sent = match &task.system_response {
            Some(response) => truncate_for_index(response),
            None => task.message.clone(),
        };
        let task_json = serde_json::to_string(task)
            .map_err(|e| JumabekError::ParseError(format!("cannot encode task object: {}", e)))?;

        self.memory
            .log(
                NewMessage::new(Role::User, sent)
                    .task(&task.task_id)
                    .parent(task.parent_task_id.clone())
                    .raw(task_json),
            )
            .await?;

        self.memory
            .log(
                NewMessage::new(Role::Assistant, response.message.clone())
                    .task(&task.task_id)
                    .parent(task.parent_task_id.clone())
                    .raw(raw_content.to_string()),
            )
            .await?;

        Ok(())
    }

    async fn run_actions(
        &self,
        ui: &mut dyn UserInterface,
        task: &TaskObject,
        response: &AgentResponse,
    ) -> JumabekResult<StepOutcome> {
        let plan = planner::plan(&response.actions);

        if plan.parallel_groups() > 0 {
            ui.show_status(&format!("plan: {}", plan.describe()))
                .await?;
        }

        let mut results: Vec<String> = Vec::new();

        for stage in &plan.stages {
            for action in stage_actions(stage) {
                if let ActionType::ExecuteModule {
                    module,
                    method,
                    args,
                    ..
                } = action
                    && let Some(outcome) = self.check_safety(ui, task, module, method, args).await?
                {
                    return Ok(outcome);
                }
            }

            if let planner::Stage::Parallel(list) = stage {
                match self.run_parallel(ui, task, list).await? {
                    Ok(mut batch) => results.append(&mut batch),
                    Err(outcome) => return Ok(outcome),
                }
                continue;
            }

            let action = stage_actions(stage)[0];
            match action {
                ActionType::RespondToUser => {}

                ActionType::ExecuteModule {
                    module,
                    method,
                    args,
                    ..
                } => {
                    self.expand_skill(module).await;
                    ui.show_status(&format!("{} · {}", module, method)).await?;

                    let registry = self.registry.read().await;
                    let Some(skill) = registry.get(module) else {
                        drop(registry);
                        results.push(format!(
                            "[ERROR] unknown skill '{}'. Available: {}",
                            module,
                            self.available_names().await
                        ));
                        continue;
                    };

                    match skill.execute(method, args).await {
                        Ok(output) => {
                            let text = render_output(output);
                            self.memory
                                .log(NewMessage::new(Role::Skill, &text).task(&task.task_id))
                                .await?;
                            results.push(format!("[{}::{}] {}", module, method, text));
                        }
                        Err(err) => {
                            let wrapped = JumabekError::SkillError(err);
                            self.memory
                                .log(
                                    NewMessage::new(Role::Skill, wrapped.to_string())
                                        .task(&task.task_id),
                                )
                                .await?;

                            if !wrapped.is_recoverable() {
                                return Ok(StepOutcome::Aborted(wrapped.to_string()));
                            }
                            results.push(format!("[{}::{}] {}", module, method, wrapped));
                        }
                    }
                }

                ActionType::PermissionRequest {
                    action,
                    description,
                    risk_level,
                } => {
                    let allowed = ui.ask_permission(action, description, risk_level).await?;
                    let verdict = if allowed { "granted" } else { "denied" };

                    self.memory
                        .log(
                            NewMessage::new(
                                Role::System,
                                format!("permission {} for '{}'", verdict, action),
                            )
                            .task(&task.task_id),
                        )
                        .await?;

                    if !allowed {
                        return Ok(StepOutcome::Aborted(format!(
                            "[PERMISSION ERROR] denied: {}. The task cannot continue without it.",
                            action
                        )));
                    }

                    results.push(format!("[PERMISSION] granted: {}", action));
                }

                ActionType::PromptToUser { message, options } => {
                    let answer = if options.is_empty() {
                        ui.show_response(message).await?;
                        match ui.read_request().await? {
                            Some(text) => text,
                            None => {
                                return Ok(StepOutcome::Aborted(
                                    "No answer given, task stopped.".to_string(),
                                ));
                            }
                        }
                    } else {
                        ui.prompt_choice(message, options).await?
                    };

                    self.memory
                        .log(NewMessage::new(Role::User, &answer).task(&task.task_id))
                        .await?;

                    results.push(format!("[USER] {}", answer));
                }

                ActionType::RequestData {
                    source,
                    query,
                    limit,
                } => {
                    if source == "skill" {
                        let name = query.trim();
                        if self.expand_skill(name).await {
                            ui.show_status(&format!("skill · {}", name)).await?;
                            results.push(format!(
                                "[SKILL] the methods of '{}' are now listed in your skills field",
                                name
                            ));
                        } else {
                            results.push(format!(
                                "[ERROR] no skill called '{}'. Available: {}",
                                name,
                                self.available_names().await
                            ));
                        }
                        continue;
                    }

                    if source != "memory" {
                        results.push(format!(
                            "[ERROR] unknown data source '{}', only 'memory' and 'skill' are supported",
                            source
                        ));
                        continue;
                    }

                    ui.show_status(&format!("memory · {}", query)).await?;
                    let mut hits = self.memory.search(query, *limit).await?;

                    if hits.is_empty()
                        && let Some(widened) = self.widen_query(query).await
                    {
                        ui.show_status(&format!("memory · retry · {}", widened))
                            .await?;
                        hits = self.memory.search(&widened, *limit).await?;
                    }

                    if hits.is_empty() {
                        results.push(format!("[MEMORY] nothing found for '{}'", query));
                    } else {
                        let mut block = format!("[MEMORY] {} result(s):", hits.len());
                        for hit in hits {
                            block.push_str(&format!(
                                "\n  session {} {} [{}] {}",
                                hit.session_id, hit.created_at, hit.role, hit.content
                            ));
                        }
                        results.push(block);
                    }
                }

                ActionType::GenerateChunk {
                    module_name,
                    chunk_index,
                    total_chunks,
                    code_chunk,
                    dependencies,
                } => {
                    if !self.engine.is_approved(module_name).await {
                        let already_loaded = self.registry.read().await.get(module_name).is_some();

                        let (what, description, risk) = if already_loaded {
                            (
                                format!("rebuild the '{}' skill", module_name),
                                format!(
                                    "Replace the existing '{}' skill with newly written code. \
                                     The current binary is kept as .previous.",
                                    module_name
                                ),
                                "high",
                            )
                        } else {
                            (
                                format!("write a new skill '{}'", module_name),
                                format!(
                                    "Write, compile and install a new skill '{}'. \
                                     The code is written by the model and compiled on this \
                                     machine; once installed it loads in every future session.",
                                    module_name
                                ),
                                "medium",
                            )
                        };

                        let allowed = ui.ask_permission(&what, &description, risk).await?;

                        self.memory
                            .log(
                                NewMessage::new(
                                    Role::System,
                                    format!(
                                        "skill build {} for '{}'",
                                        if allowed { "approved" } else { "refused" },
                                        module_name
                                    ),
                                )
                                .task(&task.task_id),
                            )
                            .await?;

                        if !allowed {
                            return Ok(StepOutcome::Aborted(format!(
                                "[PERMISSION ERROR] refused to build '{}'. Use the skills you \
                                 already have, or explain what is missing.",
                                module_name
                            )));
                        }

                        self.engine.approve(module_name).await;
                    }

                    let progress = self
                        .engine
                        .accept_chunk(
                            &self.config.preflight,
                            self.config.agent.max_fix_iterations,
                            Chunk {
                                module: module_name,
                                index: *chunk_index,
                                total: *total_chunks,
                                code: code_chunk,
                                dependencies,
                            },
                        )
                        .await?;

                    match progress {
                        Progress::Buffered { received, total } => {
                            ui.show_status(&format!(
                                "{}: chunk {}/{} received",
                                module_name, received, total
                            ))
                            .await?;
                            results.push(format!(
                                "[BUILD] {} — {}/{} chunks buffered, send the rest",
                                module_name, received, total
                            ));
                        }

                        Progress::Rejected(reason) => {
                            results.push(format!("[BUILD ERROR] {}: {}", module_name, reason));
                        }

                        Progress::Built(outcome) => {
                            let text = self.finish_build(ui, task, module_name, outcome).await?;
                            results.push(text);
                        }
                    }
                }
            }
        }

        if results.is_empty() {
            return Ok(StepOutcome::Finished);
        }

        if response.is_done {
            return Ok(StepOutcome::Finished);
        }

        Ok(StepOutcome::Continue(results.join("\n")))
    }

    async fn widen_query(&self, query: &str) -> Option<String> {
        let system = "You expand search queries for a keyword index. Answer with 5 to 12 words only: synonyms and near-synonyms of the query, in the same language as the query plus their English equivalents. Separate them with spaces. No punctuation, no explanation, no quotes.";

        let widened = self.llm.complete(system, query).await.ok()?;
        let cleaned = clean_expansion(&widened);

        if cleaned.is_empty() {
            None
        } else {
            Some(cleaned)
        }
    }

    async fn check_safety(
        &self,
        ui: &mut dyn UserInterface,
        task: &TaskObject,
        module: &str,
        method: &str,
        args: &str,
    ) -> JumabekResult<Option<StepOutcome>> {
        let Some(verdict) = safety::classify(args) else {
            return Ok(None);
        };

        let allowed = ui
            .ask_permission(
                &format!("{}::{}", module, method),
                &format!(
                    "{}

Blocked by a safety rule: {}.",
                    args, verdict.reason
                ),
                verdict.risk.as_str(),
            )
            .await?;

        self.memory
            .log(
                NewMessage::new(
                    Role::System,
                    format!(
                        "safety gate ({}) {} for: {}",
                        verdict.reason,
                        if allowed { "granted" } else { "denied" },
                        args
                    ),
                )
                .task(&task.task_id),
            )
            .await?;

        if allowed {
            return Ok(None);
        }

        Ok(Some(StepOutcome::Aborted(format!(
            "[PERMISSION ERROR] denied: {} ({}). The task cannot continue without it.",
            args, verdict.reason
        ))))
    }

    async fn run_parallel(
        &self,
        ui: &mut dyn UserInterface,
        task: &TaskObject,
        actions: &[ActionType],
    ) -> JumabekResult<Result<Vec<String>, StepOutcome>> {
        let names: Vec<String> = actions
            .iter()
            .map(|a| match a {
                ActionType::ExecuteModule { module, method, .. } => {
                    format!("{}::{}", module, method)
                }
                other => format!("{:?}", other),
            })
            .collect();

        ui.show_status(&format!("running {} in parallel", names.join(", ")))
            .await?;

        let started = std::time::Instant::now();

        let calls = actions.iter().map(|action| async move {
            let ActionType::ExecuteModule {
                module,
                method,
                args,
                ..
            } = action
            else {
                return (
                    String::new(),
                    String::new(),
                    Err(SkillError::ExecutionFailed(
                        "only module calls can run in parallel".to_string(),
                    )),
                );
            };

            let registry = self.registry.read().await;
            let Some(skill) = registry.get(module) else {
                return (
                    module.clone(),
                    method.clone(),
                    Err(SkillError::NotFound(format!("unknown skill '{}'", module))),
                );
            };

            let outcome = skill.execute(method, args).await;
            (module.clone(), method.clone(), outcome)
        });

        let finished = futures::future::join_all(calls).await;

        ui.show_status(&format!(
            "parallel group done in {:.1}s",
            started.elapsed().as_secs_f64()
        ))
        .await?;

        let mut results = Vec::with_capacity(finished.len());

        for (module, method, outcome) in finished {
            match outcome {
                Ok(output) => {
                    let text = render_output(output);
                    self.memory
                        .log(NewMessage::new(Role::Skill, &text).task(&task.task_id))
                        .await?;
                    results.push(format!("[{}::{}] {}", module, method, text));
                }
                Err(err) => {
                    let wrapped = JumabekError::SkillError(err);
                    self.memory
                        .log(NewMessage::new(Role::Skill, wrapped.to_string()).task(&task.task_id))
                        .await?;

                    if !wrapped.is_recoverable() {
                        return Ok(Err(StepOutcome::Aborted(wrapped.to_string())));
                    }
                    results.push(format!("[{}::{}] {}", module, method, wrapped));
                }
            }
        }

        Ok(Ok(results))
    }

    async fn finish_build(
        &self,
        ui: &mut dyn UserInterface,
        task: &TaskObject,
        module: &str,
        outcome: Outcome,
    ) -> JumabekResult<String> {
        let budget = self.config.agent.max_fix_iterations;
        let used = self.engine.attempts_for(module).await;
        let left = budget.saturating_sub(used);

        match outcome {
            Outcome::GaveUp {
                attempts,
                last_error,
            } => {
                ui.show_error(&format!(
                    "{}: giving up after {} failed attempt(s)",
                    module, attempts
                ))
                .await?;

                self.memory
                    .log(
                        NewMessage::new(
                            Role::System,
                            format!(
                                "gave up building {} after {} attempts: {}",
                                module, attempts, last_error
                            ),
                        )
                        .task(&task.task_id),
                    )
                    .await?;

                Ok(format!(
                    "[GAVE UP] {} failed {} time(s), which is the limit. Do NOT send more chunks \
                     for it. Solve the task with the skills you already have, or tell the user \
                     what is missing. Last error:
{}",
                    module, attempts, last_error
                ))
            }

            Outcome::CompileFailed(stderr) => {
                ui.show_status(&format!("{}: does not compile", module))
                    .await?;
                self.memory
                    .log(
                        NewMessage::new(Role::System, format!("build failed for {}", module))
                            .task(&task.task_id),
                    )
                    .await?;
                Ok(format!(
                    "[BUILD FAILED] {} did not compile ({} attempt(s) left). Fix the code and \
                     resend every chunk.
{}",
                    module, left, stderr
                ))
            }

            Outcome::ValidationFailed(report) => {
                ui.show_status(&format!("{}: rejected by the validator", module))
                    .await?;
                self.memory
                    .log(
                        NewMessage::new(Role::System, format!("validator rejected {}", module))
                            .task(&task.task_id),
                    )
                    .await?;
                Ok(format!(
                    "[VALIDATOR REJECTED] {} compiled but failed its checks ({} attempt(s) left). \
                     Fix and resend.
{}",
                    module, left, report
                ))
            }

            Outcome::PreflightUnavailable(detail) => {
                ui.show_error(&format!(
                    "{}: cannot build without a preflight container — {}",
                    module, detail
                ))
                .await?;
                Ok(format!(
                    "[PREFLIGHT UNAVAILABLE] {} was not built: {}. Start Docker Desktop, or set \
                     allow_without_docker = true in [preflight] to build without the check.",
                    module, detail
                ))
            }

            Outcome::Deployed {
                path,
                report,
                preflight,
            } => {
                ui.show_status(&format!("{}: preflight {}", module, preflight))
                    .await?;
                ui.show_status(&format!("{}: built and validated", module))
                    .await?;

                let settings = self.config.settings_for_skill(module);
                let loaded = match SkillRpcClient::spawn_with_settings(&path, settings).await {
                    Ok(client) => {
                        let methods: Vec<String> = client
                            .methods_cached()
                            .iter()
                            .map(|m| m.method.clone())
                            .collect();
                        self.registry
                            .write()
                            .await
                            .register(Box::new(client) as Box<dyn SkillModule>);
                        ui.show_status(&format!("{} is live: {}", module, methods.join(", ")))
                            .await?;
                        Some(methods)
                    }
                    Err(e) => {
                        ui.show_error(&format!("{} built but could not be loaded: {}", module, e))
                            .await?;
                        None
                    }
                };

                self.memory
                    .log(
                        NewMessage::new(
                            Role::System,
                            format!("deployed skill {} to {}", module, path.display()),
                        )
                        .task(&task.task_id),
                    )
                    .await?;

                Ok(match loaded {
                    Some(methods) => format!(
                        "[BUILT] {} passed every check and is loaded right now. \
                         Methods: {}. You can call it immediately.
{}",
                        module,
                        methods.join(", "),
                        report
                    ),
                    None => format!(
                        "[BUILT] {} passed validation and was saved, but could not be loaded into \
                         this session. It will be available after a restart.",
                        module
                    ),
                })
            }
        }
    }

    async fn available_names(&self) -> String {
        let registry = self.registry.read().await;
        let names: Vec<String> = registry
            .list()
            .into_iter()
            .map(|m| m.name.clone())
            .collect();

        if names.is_empty() {
            "<none>".to_string()
        } else {
            names.join(", ")
        }
    }
}

fn stage_actions(stage: &planner::Stage) -> Vec<&ActionType> {
    match stage {
        planner::Stage::Single(action) => vec![action],
        planner::Stage::Parallel(list) => list.iter().collect(),
    }
}

fn first_line(text: &str) -> String {
    text.lines()
        .next()
        .unwrap_or_default()
        .chars()
        .take(120)
        .collect()
}

fn clean_expansion(raw: &str) -> String {
    raw.split_whitespace()
        .map(|word| word.trim_matches(|c: char| !c.is_alphanumeric()))
        .filter(|word| word.chars().count() >= 2)
        .take(16)
        .collect::<Vec<_>>()
        .join(" ")
}

fn truncate_for_index(text: &str) -> String {
    match text.char_indices().nth(INDEXED_CONTENT_LIMIT) {
        Some((idx, _)) => format!(
            "{}… [{} more characters kept in raw_json]",
            &text[..idx],
            text.chars().count() - INDEXED_CONTENT_LIMIT
        ),
        None => text.to_string(),
    }
}

fn render_output(output: SkillOutput) -> String {
    match output {
        SkillOutput::Text(text) => text,
        SkillOutput::Json(value) => value.to_string(),
        SkillOutput::Binary(bytes) => format!("<{} bytes of binary data>", bytes.len()),
        SkillOutput::Empty => "<no output>".to_string(),
    }
}

fn system_info() -> SystemInfo {
    SystemInfo {
        os: format!("{} ({})", std::env::consts::OS, std::env::consts::ARCH),
        shell: if cfg!(windows) {
            "powershell".to_string()
        } else {
            std::env::var("SHELL").unwrap_or_else(|_| "sh".to_string())
        },
        current_time: Local::now().to_rfc3339(),
    }
}

#[cfg(test)]
mod expansion_tests {
    use super::*;
    use jumabek_sdk::{MethodInfo, ModuleMetadata, SkillError, SkillOutput};

    struct Stub {
        metadata: ModuleMetadata,
    }

    impl Stub {
        fn new(name: &str) -> Self {
            Stub {
                metadata: ModuleMetadata {
                    name: name.to_string(),
                    version: "1.0.0".to_string(),
                    description: "a stub".to_string(),
                },
            }
        }
    }

    #[async_trait::async_trait]
    impl SkillModule for Stub {
        fn get_metadata(&self) -> &ModuleMetadata {
            &self.metadata
        }
        fn health_check(&self) -> bool {
            true
        }
        fn available_methods(&self) -> Vec<MethodInfo> {
            vec![MethodInfo {
                method: "run".to_string(),
                description: "runs".to_string(),
                args_description: "args".to_string(),
            }]
        }
        async fn execute(&self, _: &str, _: &str) -> Result<SkillOutput, SkillError> {
            Ok(SkillOutput::Empty)
        }
    }

    fn registry_of(count: usize) -> SkillRegistry {
        let mut registry = SkillRegistry::new();
        for i in 0..count {
            registry.register(Box::new(Stub::new(&format!("skill{}", i))) as Box<dyn SkillModule>);
        }
        registry
    }

    async fn describe(count: usize, expanded: &[&str]) -> Vec<TaskObjectSkill> {
        let registry = RwLock::new(registry_of(count));
        let expanded_set: std::collections::HashSet<String> =
            expanded.iter().map(|s| s.to_string()).collect();

        let registry = registry.read().await;
        let total = registry.list().len();
        let expand_all = total < EXPAND_EVERYTHING_BELOW;

        registry
            .all()
            .map(|skill| {
                let metadata = skill.get_metadata();
                let show = expand_all || expanded_set.contains(&metadata.name);
                TaskObjectSkill {
                    name: metadata.name.clone(),
                    description: metadata.description.clone(),
                    available_methods: if show {
                        skill
                            .available_methods()
                            .into_iter()
                            .map(|m| TaskObjectSkillMethod {
                                method: m.method,
                                description: m.description,
                                args_description: m.args_description,
                            })
                            .collect()
                    } else {
                        Vec::new()
                    },
                }
            })
            .collect()
    }

    #[test]
    fn a_parse_failure_is_summarised_to_one_line() {
        let detail = format!("cannot read the answer: {}\nsecond line", "x".repeat(300));
        let summary = first_line(&detail);

        assert!(!summary.contains('\n'), "status line spans lines");
        assert!(summary.chars().count() <= 120);
    }

    #[test]
    fn model_facing_text_has_no_collapsed_line_breaks() {
        // A run of spaces in one of these means a `\` continuation was lost and
        // the model is being handed mangled instructions.
        assert!(
            !PARSE_CORRECTION.contains("   "),
            "PARSE_CORRECTION lost a line continuation"
        );
    }

    #[tokio::test]
    async fn a_handful_of_skills_is_sent_in_full() {
        let described = describe(3, &[]).await;
        assert!(
            described.iter().all(|s| !s.available_methods.is_empty()),
            "methods were withheld when there was no reason to"
        );
    }

    #[tokio::test]
    async fn with_many_skills_only_the_ones_in_play_carry_methods() {
        let described = describe(10, &["skill4"]).await;

        let with_methods: Vec<&str> = described
            .iter()
            .filter(|s| !s.available_methods.is_empty())
            .map(|s| s.name.as_str())
            .collect();

        assert_eq!(with_methods, vec!["skill4"]);
        assert_eq!(described.len(), 10, "a skill disappeared from the list");
        assert!(
            described.iter().all(|s| !s.description.is_empty()),
            "a skill lost its summary and became unfindable"
        );
    }
}
