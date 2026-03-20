use crate::role_catalog::RoleCatalog;
use crate::supervisor::{Supervisor, SupervisorConfig};
use crate::worktree::WorktreeManager;
use chrono::Utc;
use nca_common::config::NcaConfig;
use nca_common::event::{AgentEvent, EndReason};
use nca_common::message::Message;
use nca_common::session::OrchestrationContext;
use nca_common::team::*;
use nca_common::tool::ToolDefinition;
use nca_core::provider::factory::build_provider;
use nca_core::provider::StreamChunk;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc, RwLock};

// ── Worker state ────────────────────────────────────────────────────────

pub struct WorkerState {
    pub assignment: AgentAssignment,
    pub status: WorkerStatus,
    pub session_id: Option<String>,
    pub worktree_path: Option<PathBuf>,
    pub branch: Option<String>,
    pub completion_report: Option<String>,
    pub cost: WorkerCost,
}

// ── Driver (owned by spawned task) ──────────────────────────────────────

struct TeamOrchestratorDriver {
    id: TeamOrchestrationId,
    config: NcaConfig,
    workspace_root: PathBuf,
    plan: Option<TeamPlan>,
    phase: TeamPhase,
    workers: HashMap<String, WorkerState>,
    event_tx: broadcast::Sender<TeamEvent>,
    command_rx: mpsc::Receiver<TeamCommand>,
    status: Arc<RwLock<TeamStatus>>,
    result_tx: Option<tokio::sync::oneshot::Sender<TeamOrchestrationResult>>,
}

// ── Handle (cheaply cloneable, returned to callers) ─────────────────────

#[derive(Clone)]
pub struct TeamOrchestratorHandle {
    pub orchestration_id: TeamOrchestrationId,
    command_tx: mpsc::Sender<TeamCommand>,
    event_tx: broadcast::Sender<TeamEvent>,
    status: Arc<RwLock<TeamStatus>>,
    result_rx: Arc<tokio::sync::Mutex<Option<tokio::sync::oneshot::Receiver<TeamOrchestrationResult>>>>,
}

impl TeamOrchestratorHandle {
    /// Block until the orchestration completes and return the result.
    /// Can only be called once; subsequent calls return an error.
    pub async fn wait(&self) -> Result<TeamOrchestrationResult, String> {
        let mut rx_guard = self.result_rx.lock().await;
        let rx = rx_guard.take().ok_or("already consumed")?;
        rx.await.map_err(|e| format!("driver dropped: {e}"))
    }

    pub async fn pause_agent(&self, name: &str) -> Result<(), String> {
        self.command_tx
            .send(TeamCommand::PauseAgent {
                name: name.to_string(),
            })
            .await
            .map_err(|e| e.to_string())
    }

    pub async fn resume_agent(&self, name: &str) -> Result<(), String> {
        self.command_tx
            .send(TeamCommand::ResumeAgent {
                name: name.to_string(),
            })
            .await
            .map_err(|e| e.to_string())
    }

    pub async fn redirect_agent(&self, name: &str, message: &str) -> Result<(), String> {
        self.command_tx
            .send(TeamCommand::RedirectAgent {
                name: name.to_string(),
                message: message.to_string(),
            })
            .await
            .map_err(|e| e.to_string())
    }

    pub async fn cancel_agent(&self, name: &str) -> Result<(), String> {
        self.command_tx
            .send(TeamCommand::CancelAgent {
                name: name.to_string(),
            })
            .await
            .map_err(|e| e.to_string())
    }

    pub async fn cancel_all(&self) -> Result<(), String> {
        self.command_tx
            .send(TeamCommand::CancelAll)
            .await
            .map_err(|e| e.to_string())
    }

    pub async fn status(&self) -> TeamStatus {
        self.status.read().await.clone()
    }

    pub fn subscribe(&self) -> broadcast::Receiver<TeamEvent> {
        self.event_tx.subscribe()
    }
}

// ── Public entry point ──────────────────────────────────────────────────

pub struct TeamOrchestrator;

impl TeamOrchestrator {
    /// Spawn the orchestration driver as a tokio task and return a handle.
    pub async fn start(
        config: NcaConfig,
        workspace_root: PathBuf,
        prompt: String,
        agent_hints: Option<String>,
    ) -> Result<TeamOrchestratorHandle, String> {
        let id = TeamOrchestrationId::new(format!("team-{}", Utc::now().timestamp_micros()));
        let (event_tx, _) = broadcast::channel::<TeamEvent>(512);
        let (command_tx, command_rx) = mpsc::channel::<TeamCommand>(64);
        let (result_tx, result_rx) = tokio::sync::oneshot::channel();

        let status = Arc::new(RwLock::new(TeamStatus {
            orchestration_id: id.clone(),
            phase: TeamPhase::Decomposition,
            workers: HashMap::new(),
            cost: TeamCost::default(),
            started_at: Utc::now(),
            elapsed_secs: 0,
        }));

        let handle = TeamOrchestratorHandle {
            orchestration_id: id.clone(),
            command_tx,
            event_tx: event_tx.clone(),
            status: status.clone(),
            result_rx: Arc::new(tokio::sync::Mutex::new(Some(result_rx))),
        };

        let driver = TeamOrchestratorDriver {
            id,
            config,
            workspace_root,
            plan: None,
            phase: TeamPhase::Decomposition,
            workers: HashMap::new(),
            event_tx,
            command_rx,
            status,
            result_tx: Some(result_tx),
        };

        tokio::spawn(async move {
            driver.run(prompt, agent_hints).await;
        });

        Ok(handle)
    }
}

// ── Tool definition & system prompt for LLM decomposition ───────────────

fn team_plan_tool_definition() -> ToolDefinition {
    ToolDefinition {
        name: "create_team_plan".to_string(),
        description: "Create a multi-agent execution plan. Define which agents to spawn, their roles, task briefs, and execution order.".to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "required": ["agents", "stages"],
            "properties": {
                "agents": {
                    "type": "array",
                    "description": "List of agents to spawn",
                    "items": {
                        "type": "object",
                        "required": ["name", "role", "task_brief"],
                        "properties": {
                            "name": {
                                "type": "string",
                                "description": "Unique agent name (e.g. 'researcher-1', 'alpha')"
                            },
                            "role": {
                                "type": "string",
                                "description": "Role name from catalog: researcher, implementer, reviewer, tester, architect, debugger"
                            },
                            "task_brief": {
                                "type": "string",
                                "description": "Detailed instructions for this agent"
                            },
                            "depends_on": {
                                "type": "array",
                                "items": { "type": "string" },
                                "description": "Names of agents whose output this agent needs before starting"
                            },
                            "needs_workspace": {
                                "type": "boolean",
                                "description": "Whether this agent needs file/workspace access. Set to false for pure computation, math, analysis, or reasoning tasks that don't need to read/write files. Defaults to true."
                            },
                            "custom_system_prompt": {
                                "type": "string",
                                "description": "Custom system prompt for this agent, overriding the role's default. Use this to give agents specific identities, expertise, or behavioral instructions (e.g. 'You are Agent Alpha, a math specialist focused on x-variable statistics.')."
                            }
                        }
                    }
                },
                "stages": {
                    "type": "array",
                    "description": "Execution stages. Each stage is a list of agent names that run in parallel. Stages execute sequentially.",
                    "items": {
                        "type": "array",
                        "items": { "type": "string" }
                    }
                }
            }
        }),
    }
}

fn decomposition_system_prompt(available_roles: &[String]) -> String {
    format!(
        r#"You are a task decomposition agent. Your job is to analyze a user's request and break it down into a multi-agent execution plan.

Available roles: {}

Each role has different capabilities:
- researcher: Read-only. Analyzes code, searches the web, produces findings. Cannot modify files. Good for research, planning, and gathering information.
- implementer: Full access. Writes code, creates files, runs commands. Use for building and implementation tasks.
- reviewer: Read-only. Reviews code for quality, bugs, style. Cannot modify files.
- tester: Full access. Writes and runs tests. Use for validation and quality assurance.
- architect: Read-only. Designs approaches, produces plans. Cannot modify files.
- debugger: Full access. Investigates bugs, implements fixes.

PLANNING GUIDELINES:
- For complex tasks, create a FULL PIPELINE of agents across multiple stages.
- Think about the complete workflow: research/design → implement → review/test.
- Example: "create a landing page" should have at minimum:
  Stage 1: researcher (research best practices, gather inspiration, define structure)
  Stage 2: implementer (build the page based on research findings)
  Stage 3: reviewer + tester (review code quality, test the result)
- Do NOT use just one agent for tasks that naturally involve multiple phases.
- Each stage builds on the output of the previous one via depends_on.

Rules:
1. Use the create_team_plan tool to define your plan.
2. Each agent gets a unique name (e.g., "alpha", "beta", "researcher-1").
3. Stages define execution order — agents in the same stage run in parallel, stages run sequentially.
4. If agent B needs output from agent A, put them in different stages (A's stage first) and list A in B's depends_on.
5. For simple, single-step tasks (e.g., "what is 2+2"), one agent is fine. For anything involving research, implementation, or creation — use multiple agents in a pipeline.
6. Agent task briefs should be detailed and self-contained — each agent only sees its own brief plus context from dependencies.
7. Set needs_workspace to false for agents doing pure computation, math, analysis, writing, or reasoning that does NOT require reading/writing files. This makes them faster and lighter.
8. Use custom_system_prompt to give agents specific identities, expertise, or behavioral instructions. This overrides the role's default prompt. Use it when agents need specialized behavior beyond what the built-in roles provide.
9. You MUST call the create_team_plan tool. Do not respond with text only."#,
        available_roles.join(", ")
    )
}

// ── Driver implementation ───────────────────────────────────────────────

impl TeamOrchestratorDriver {
    async fn run(mut self, prompt: String, agent_hints: Option<String>) {
        let result = self.execute(prompt, agent_hints).await;
        let _ = self.result_tx.take().map(|tx| tx.send(result));
    }

    async fn execute(
        &mut self,
        prompt: String,
        agent_hints: Option<String>,
    ) -> TeamOrchestrationResult {
        // Phase 1: Decomposition — build a plan
        self.set_phase(TeamPhase::Decomposition).await;
        let catalog = RoleCatalog::load(&self.workspace_root);

        self.emit_event("orchestrator", AgentEvent::Checkpoint {
            phase: "Decomposing task into agent plan...".to_string(),
            detail: String::new(),
            turn: 0,
        }).await;

        let plan = match self.decompose_with_llm(&prompt, &agent_hints, &catalog).await {
            Some(mut p) => {
                p.task = prompt.clone();
                self.emit_event("orchestrator", AgentEvent::Checkpoint {
                    phase: format!("Plan created: {} agents in {} stages", p.agents.len(), p.execution_order.stages.len()),
                    detail: p.agents.iter().map(|a| format!("{} ({})", a.name, a.role.name)).collect::<Vec<_>>().join(", "),
                    turn: 0,
                }).await;
                p
            }
            None => {
                self.emit_event("orchestrator", AgentEvent::Checkpoint {
                    phase: "LLM decomposition failed, falling back to single implementer".to_string(),
                    detail: String::new(),
                    turn: 0,
                }).await;
                self.build_default_plan(&prompt, &catalog)
            }
        };
        self.plan = Some(plan.clone());

        for agent in &plan.agents {
            self.workers.insert(
                agent.name.clone(),
                WorkerState {
                    assignment: agent.clone(),
                    status: WorkerStatus::Pending,
                    session_id: None,
                    worktree_path: None,
                    branch: None,
                    completion_report: None,
                    cost: WorkerCost::default(),
                },
            );
        }
        self.update_status().await;

        // Phase 2: Execution — run stages sequentially, agents within each stage in parallel
        self.set_phase(TeamPhase::Execution).await;
        let mut stage_reports: HashMap<String, String> = HashMap::new();
        for stage in &plan.execution_order.stages {
            let reports = self.run_stage(stage, &stage_reports).await;
            stage_reports.extend(reports);

            // Drain any pending commands between stages
            while let Ok(cmd) = self.command_rx.try_recv() {
                self.process_command(cmd).await;
            }
        }

        // Phase 3: Merge — combine worktree branches
        self.set_phase(TeamPhase::Merge).await;
        let merge_result = self.merge_worktrees().await;

        let outcome = if self
            .workers
            .values()
            .all(|w| w.status == WorkerStatus::Completed)
        {
            TeamOutcome::Success
        } else if self
            .workers
            .values()
            .any(|w| w.status == WorkerStatus::Completed)
        {
            TeamOutcome::PartialSuccess
        } else {
            TeamOutcome::Failed
        };

        self.set_phase(if outcome == TeamOutcome::Failed {
            TeamPhase::Failed
        } else {
            TeamPhase::Complete
        })
        .await;

        TeamOrchestrationResult {
            orchestration_id: self.id.clone(),
            outcome,
            merge_branch: merge_result.ok(),
            changed_files: vec![],
            validation_passed: false,
            cost: self.aggregate_cost(),
            agent_reports: self.build_agent_reports(),
        }
    }

    fn build_default_plan(&self, prompt: &str, catalog: &RoleCatalog) -> TeamPlan {
        let role = catalog
            .get("implementer")
            .cloned()
            .expect("built-in implementer role must exist");
        TeamPlan {
            id: self.id.clone(),
            task: prompt.to_string(),
            agents: vec![AgentAssignment {
                name: "implementer-1".into(),
                role,
                task_brief: prompt.to_string(),
                depends_on: vec![],
                needs_workspace: true,
                custom_system_prompt: None,
            }],
            execution_order: ExecutionOrder {
                stages: vec![ExecutionStage {
                    parallel: vec!["implementer-1".into()],
                }],
            },
        }
    }

    // ── LLM decomposition ────────────────────────────────────────────────

    async fn decompose_with_llm(
        &self,
        prompt: &str,
        agent_hints: &Option<String>,
        catalog: &RoleCatalog,
    ) -> Option<TeamPlan> {
        let provider = match build_provider(&self.config) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("[orchestrator] Failed to build provider for decomposition: {e}");
                return None;
            }
        };

        let available_roles: Vec<String> = catalog.names().into_iter().map(|s| s.to_string()).collect();
        let system_prompt = decomposition_system_prompt(&available_roles);

        let mut user_prompt = format!("Decompose this task into a multi-agent plan:\n\n{prompt}");
        if let Some(hints) = agent_hints {
            user_prompt.push_str(&format!("\n\nAgent hints from user: {hints}"));
        }

        let messages = vec![
            Message::system(system_prompt),
            Message::user(user_prompt),
        ];

        let tool_def = team_plan_tool_definition();
        let model = self.config.model.default_model.clone();

        let mut rx = match provider.chat(&messages, &[tool_def], &model).await {
            Ok(rx) => rx,
            Err(e) => {
                eprintln!("[orchestrator] Provider call failed: {e}");
                return None;
            }
        };

        // Collect tool call from stream
        let mut tool_call_input: Option<serde_json::Value> = None;
        while let Some(chunk) = rx.recv().await {
            match chunk {
                StreamChunk::ToolUse(call) if call.name == "create_team_plan" => {
                    tool_call_input = Some(call.input);
                }
                StreamChunk::Done => break,
                _ => {}
            }
        }

        let input = tool_call_input?;
        self.parse_plan_from_tool_call(input, catalog)
    }

    fn parse_plan_from_tool_call(
        &self,
        input: serde_json::Value,
        catalog: &RoleCatalog,
    ) -> Option<TeamPlan> {
        let agents_arr = input.get("agents")?.as_array()?;
        let stages_arr = input.get("stages")?.as_array()?;

        let mut assignments = Vec::new();

        for agent_val in agents_arr {
            let name = agent_val.get("name")?.as_str()?.to_string();
            let role_name = agent_val.get("role")?.as_str()?;
            let task_brief = agent_val.get("task_brief")?.as_str()?.to_string();
            let depends_on: Vec<String> = agent_val
                .get("depends_on")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();

            let needs_workspace = agent_val
                .get("needs_workspace")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);

            let custom_system_prompt = agent_val
                .get("custom_system_prompt")
                .and_then(|v| v.as_str())
                .map(String::from);

            // Look up role from catalog, fall back to implementer
            let role = catalog
                .get(role_name)
                .or_else(|| catalog.get("implementer"))
                .cloned()?;

            assignments.push(AgentAssignment {
                name,
                role,
                task_brief,
                depends_on,
                needs_workspace,
                custom_system_prompt,
            });
        }

        if assignments.is_empty() {
            return None;
        }

        // Parse stages
        let mut stages = Vec::new();
        for stage_val in stages_arr {
            let parallel: Vec<String> = stage_val
                .as_array()?
                .iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect();
            if !parallel.is_empty() {
                stages.push(ExecutionStage { parallel });
            }
        }

        if stages.is_empty() {
            // If no stages defined, put all agents in one stage
            stages.push(ExecutionStage {
                parallel: assignments.iter().map(|a| a.name.clone()).collect(),
            });
        }

        // Validate: all agent names in stages must exist in assignments
        let agent_names: std::collections::HashSet<_> =
            assignments.iter().map(|a| a.name.as_str()).collect();
        for stage in &stages {
            for name in &stage.parallel {
                if !agent_names.contains(name.as_str()) {
                    eprintln!("[orchestrator] Stage references unknown agent: {name}");
                    return None;
                }
            }
        }

        Some(TeamPlan {
            id: self.id.clone(),
            task: String::new(), // will be set by caller
            agents: assignments,
            execution_order: ExecutionOrder { stages },
        })
    }

    // ── Stage execution ─────────────────────────────────────────────────

    async fn run_stage(
        &mut self,
        stage: &ExecutionStage,
        prior_reports: &HashMap<String, String>,
    ) -> HashMap<String, String> {
        let mut handles: Vec<(
            String,
            tokio::task::JoinHandle<(
                String,
                Option<String>,
                Option<PathBuf>,
                Option<String>,
            )>,
        )> = vec![];

        for agent_name in &stage.parallel {
            let worker = match self.workers.get_mut(agent_name) {
                Some(w) => w,
                None => continue,
            };
            worker.status = WorkerStatus::Working;

            let config = self.config.clone();
            let workspace = self.workspace_root.clone();
            let assignment = worker.assignment.clone();
            let orch_id = self.id.clone();
            let prior = prior_reports.clone();
            let event_tx = self.event_tx.clone();

            let handle = tokio::spawn(async move {
                run_worker_agent(config, workspace, assignment, orch_id, prior, event_tx).await
            });
            handles.push((agent_name.clone(), handle));
        }
        self.update_status().await;

        let mut reports = HashMap::new();
        for (name, handle) in handles {
            match handle.await {
                Ok((session_id, report, worktree_path, branch)) => {
                    if let Some(w) = self.workers.get_mut(&name) {
                        w.status = WorkerStatus::Completed;
                        w.session_id = Some(session_id);
                        w.worktree_path = worktree_path;
                        w.branch = branch;
                        w.completion_report = report.clone();
                    }
                    if let Some(r) = report {
                        reports.insert(name.clone(), r);
                    }
                }
                Err(e) => {
                    if let Some(w) = self.workers.get_mut(&name) {
                        w.status = WorkerStatus::Failed;
                    }
                    self.emit_event(
                        &name,
                        AgentEvent::Error {
                            message: format!("Worker panicked: {e}"),
                        },
                    )
                    .await;
                }
            }
        }
        self.update_status().await;
        reports
    }

    // ── Merge ───────────────────────────────────────────────────────────

    async fn merge_worktrees(&mut self) -> Result<String, String> {
        let agent_branches: Vec<(String, String)> = self
            .workers
            .iter()
            .filter(|(_, w)| w.status == WorkerStatus::Completed)
            .filter_map(|(name, w)| {
                w.branch
                    .as_ref()
                    .map(|b| (name.clone(), b.clone()))
            })
            .collect();

        let orch_id_str = self.id.to_string();
        let result = crate::team_merge::merge_agent_branches(
            &self.workspace_root,
            &orch_id_str,
            &agent_branches,
        )?;

        if !result.conflicts.is_empty() {
            self.emit_event(
                "orchestrator",
                AgentEvent::Checkpoint {
                    phase: "Merge conflicts detected".to_string(),
                    detail: result.conflicts.join(", "),
                    turn: 0,
                },
            )
            .await;
        }

        Ok(result.branch)
    }

    // ── Command processing ───────────────────────────────────────────────

    async fn process_command(&mut self, cmd: TeamCommand) {
        match cmd {
            TeamCommand::PauseAgent { name } => {
                if let Some(worker) = self.workers.get_mut(&name) {
                    if worker.status == WorkerStatus::Working {
                        worker.status = WorkerStatus::Paused;
                        self.emit_event("orchestrator", AgentEvent::Checkpoint {
                            phase: format!("Paused agent: {name}"),
                            detail: String::new(),
                            turn: 0,
                        }).await;
                    }
                }
            }
            TeamCommand::ResumeAgent { name } => {
                if let Some(worker) = self.workers.get_mut(&name) {
                    if worker.status == WorkerStatus::Paused {
                        worker.status = WorkerStatus::Working;
                        self.emit_event("orchestrator", AgentEvent::Checkpoint {
                            phase: format!("Resumed agent: {name}"),
                            detail: String::new(),
                            turn: 0,
                        }).await;
                    }
                }
            }
            TeamCommand::RedirectAgent { name, message } => {
                if let Some(worker) = self.workers.get_mut(&name) {
                    if worker.status == WorkerStatus::Paused {
                        worker.status = WorkerStatus::Working;
                        self.emit_event("orchestrator", AgentEvent::Checkpoint {
                            phase: format!("Redirected agent: {name}"),
                            detail: message,
                            turn: 0,
                        }).await;
                    }
                }
            }
            TeamCommand::CancelAgent { name } => {
                if let Some(worker) = self.workers.get_mut(&name) {
                    worker.status = WorkerStatus::Cancelled;
                    self.emit_event("orchestrator", AgentEvent::Checkpoint {
                        phase: format!("Cancelled agent: {name}"),
                        detail: String::new(),
                        turn: 0,
                    }).await;
                }
            }
            TeamCommand::CancelAll => {
                for (_, worker) in &mut self.workers {
                    if worker.status == WorkerStatus::Working || worker.status == WorkerStatus::Paused {
                        worker.status = WorkerStatus::Cancelled;
                    }
                }
                self.emit_event("orchestrator", AgentEvent::Checkpoint {
                    phase: "Cancelled all agents".to_string(),
                    detail: String::new(),
                    turn: 0,
                }).await;
            }
            TeamCommand::QueryStatus => { /* status readable via handle.status() */ }
        }
        self.update_status().await;
    }

    // ── Helpers ─────────────────────────────────────────────────────────

    async fn set_phase(&mut self, phase: TeamPhase) {
        self.phase = phase.clone();
        let mut status = self.status.write().await;
        status.phase = phase;
        status.elapsed_secs = Utc::now()
            .signed_duration_since(status.started_at)
            .num_seconds()
            .max(0) as u64;
    }

    async fn update_status(&self) {
        let mut status = self.status.write().await;
        status.workers = self
            .workers
            .iter()
            .map(|(name, w)| (name.clone(), w.status.clone()))
            .collect();
        status.cost = self.aggregate_cost();
        status.elapsed_secs = Utc::now()
            .signed_duration_since(status.started_at)
            .num_seconds()
            .max(0) as u64;
    }

    async fn emit_event(&self, source: &str, event: AgentEvent) {
        let team_event = TeamEvent {
            orchestration_id: self.id.clone(),
            source_agent: source.to_string(),
            source_session: String::new(),
            event,
            phase: self.phase.clone(),
            timestamp: Utc::now(),
        };
        self.persist_event(&team_event);
        let _ = self.event_tx.send(team_event);
    }

    fn persist_event(&self, event: &TeamEvent) {
        let dir = self.workspace_root.join(".nca").join("orchestrations");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(format!("{}.events.jsonl", self.id));
        if let Ok(json) = serde_json::to_string(event) {
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
                let _ = writeln!(f, "{json}");
            }
        }
    }

    fn aggregate_cost(&self) -> TeamCost {
        let mut cost = TeamCost::default();
        for (name, worker) in &self.workers {
            cost.total_input_tokens += worker.cost.input_tokens;
            cost.total_output_tokens += worker.cost.output_tokens;
            cost.total_usd += worker.cost.usd;
            cost.per_agent.insert(name.clone(), worker.cost.clone());
        }
        cost
    }

    fn build_agent_reports(&self) -> Vec<AgentReport> {
        self.workers
            .iter()
            .map(|(name, w)| AgentReport {
                name: name.clone(),
                role: w.assignment.role.name.clone(),
                status: w.status.clone(),
                completion_report: w.completion_report.clone(),
                changed_files: vec![],
            })
            .collect()
    }
}

// ── Worker agent runner ─────────────────────────────────────────────────

async fn run_worker_agent(
    config: NcaConfig,
    workspace_root: PathBuf,
    assignment: AgentAssignment,
    orch_id: TeamOrchestrationId,
    prior_reports: HashMap<String, String>,
    event_tx: broadcast::Sender<TeamEvent>,
) -> (String, Option<String>, Option<PathBuf>, Option<String>) {
    let session_id = format!(
        "team-{}-{}",
        assignment.name,
        Utc::now().timestamp_micros()
    );

    // For non-workspace tasks (math, analysis, reasoning), use a direct LLM call
    if !assignment.needs_workspace {
        return run_lightweight_agent(config, assignment, orch_id, prior_reports, event_tx, session_id).await;
    }

    let wt_manager = WorktreeManager::new(&workspace_root);

    let worktree_info = match wt_manager.create_worktree(&session_id) {
        Ok(info) => info,
        Err(e) => {
            return (
                session_id,
                Some(format!("Failed to create worktree: {e}")),
                None,
                None,
            )
        }
    };

    // Build context from prior stage reports
    let mut context_parts = vec![assignment.task_brief.clone()];
    for dep in &assignment.depends_on {
        if let Some(report) = prior_reports.get(dep) {
            context_parts.push(format!("[Context from prior stage: {dep}]\n{report}"));
        }
    }
    let full_prompt = context_parts.join("\n\n");

    // Configure supervisor with role-appropriate permissions
    let mut worker_config = config.clone();
    worker_config.permissions.mode = assignment.role.permission_mode;

    let orchestration_context = OrchestrationContext {
        orchestrator: Some("nca-team".to_string()),
        run_id: Some(orch_id.to_string()),
        task_id: Some(assignment.name.clone()),
        ..Default::default()
    };

    let sup_config = SupervisorConfig {
        config: worker_config,
        workspace_root: worktree_info.worktree_path.clone(),
        safe_mode: false,
        interactive_approvals: false,
        session_id: Some(session_id.clone()),
        approval_handler: None,
        orchestration_context: Some(orchestration_context),
    };

    let mut supervisor = match Supervisor::create(sup_config).await {
        Ok(s) => s,
        Err(e) => {
            return (
                session_id,
                Some(format!("Failed to create supervisor: {e}")),
                None,
                None,
            )
        }
    };

    supervisor
        .agent_mut()
        .set_system_prompt(
            assignment.custom_system_prompt.as_deref()
                .unwrap_or(&assignment.role.system_prompt),
        );
    supervisor.set_worktree_info(
        worktree_info.worktree_path.clone(),
        worktree_info.branch_name.clone(),
        worktree_info.base_branch.clone(),
    );

    // Take handle and forward worker agent events into the unified TeamEvent stream
    let mut sup_handle = supervisor.take_handle();

    let forward_task = if let Some(mut rx) = sup_handle.take_event_rx() {
        let agent_name = assignment.name.clone();
        let orch_id_clone = orch_id.clone();
        let event_tx_clone = event_tx.clone();
        Some(tokio::spawn(async move {
            while let Some(agent_event) = rx.recv().await {
                let team_event = TeamEvent {
                    orchestration_id: orch_id_clone.clone(),
                    source_agent: agent_name.clone(),
                    source_session: String::new(),
                    event: agent_event,
                    phase: TeamPhase::Execution,
                    timestamp: chrono::Utc::now(),
                };
                let _ = event_tx_clone.send(team_event);
            }
        }))
    } else {
        None
    };

    let result = supervisor.run_turn(&full_prompt).await;
    let report = match result {
        Ok(output) => Some(output),
        Err(e) => Some(format!("Agent error: {e}")),
    };

    supervisor.finish(EndReason::Completed).await;

    // Stop the event forwarding task
    if let Some(task) = forward_task {
        task.abort();
    }

    (
        session_id,
        report,
        Some(worktree_info.worktree_path),
        Some(worktree_info.branch_name),
    )
}

/// Lightweight agent that runs a pure LLM call — no workspace, no tools, no worktree.
/// Used for computation, math, analysis, reasoning tasks.
async fn run_lightweight_agent(
    config: NcaConfig,
    assignment: AgentAssignment,
    orch_id: TeamOrchestrationId,
    prior_reports: HashMap<String, String>,
    event_tx: broadcast::Sender<TeamEvent>,
    session_id: String,
) -> (String, Option<String>, Option<PathBuf>, Option<String>) {
    // Emit start event
    let _ = event_tx.send(TeamEvent {
        orchestration_id: orch_id.clone(),
        source_agent: assignment.name.clone(),
        source_session: session_id.clone(),
        event: AgentEvent::SessionStarted {
            session_id: session_id.clone(),
            workspace: PathBuf::new(),
            model: config.model.default_model.clone(),
        },
        phase: TeamPhase::Execution,
        timestamp: Utc::now(),
    });

    // Build provider
    let provider = match build_provider(&config) {
        Ok(p) => p,
        Err(e) => {
            return (session_id, Some(format!("Failed to build provider: {e}")), None, None);
        }
    };

    // Build prompt with context from prior stages
    let mut context_parts = vec![assignment.task_brief.clone()];
    for dep in &assignment.depends_on {
        if let Some(report) = prior_reports.get(dep) {
            context_parts.push(format!("[Context from prior stage: {dep}]\n{report}"));
        }
    }
    let user_prompt = context_parts.join("\n\n");

    let system_prompt = assignment.custom_system_prompt.as_deref()
        .unwrap_or(&assignment.role.system_prompt);
    let messages = vec![
        Message::system(system_prompt),
        Message::user(user_prompt),
    ];

    let model = config.model.default_model.clone();

    // Call provider directly — no tools
    let mut rx = match provider.chat(&messages, &[], &model).await {
        Ok(rx) => rx,
        Err(e) => {
            return (session_id, Some(format!("Provider call failed: {e}")), None, None);
        }
    };

    // Collect response
    let mut response_text = String::new();
    while let Some(chunk) = rx.recv().await {
        match chunk {
            StreamChunk::TextDelta(text) => {
                // Emit streaming event
                let _ = event_tx.send(TeamEvent {
                    orchestration_id: orch_id.clone(),
                    source_agent: assignment.name.clone(),
                    source_session: session_id.clone(),
                    event: AgentEvent::TokensStreamed { delta: text.clone() },
                    phase: TeamPhase::Execution,
                    timestamp: Utc::now(),
                });
                response_text.push_str(&text);
            }
            StreamChunk::Done => break,
            _ => {}
        }
    }

    // Emit completion event
    let _ = event_tx.send(TeamEvent {
        orchestration_id: orch_id.clone(),
        source_agent: assignment.name.clone(),
        source_session: session_id.clone(),
        event: AgentEvent::SessionEnded {
            reason: nca_common::event::EndReason::Completed,
        },
        phase: TeamPhase::Execution,
        timestamp: Utc::now(),
    });

    let report = if response_text.is_empty() {
        Some("Agent produced no output".to_string())
    } else {
        Some(response_text)
    };

    (session_id, report, None, None)
}
