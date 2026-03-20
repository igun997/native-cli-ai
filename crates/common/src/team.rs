use crate::config::PermissionMode;
use crate::entity_id;
use crate::event::AgentEvent;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

entity_id!(TeamOrchestrationId);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TeamPhase {
    Decomposition,
    Execution,
    Merge,
    Validation,
    Complete,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkerStatus {
    Pending,
    Working,
    Paused,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TeamOutcome {
    Success,
    PartialSuccess,
    Failed,
    Cancelled,
}

// ── Role, plan, and assignment types ──────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoleDefinition {
    pub name: String,
    pub description: String,
    pub system_prompt: String,
    pub allowed_tools: Vec<String>,
    pub denied_tools: Vec<String>,
    pub model_override: Option<String>,
    pub permission_mode: PermissionMode,
    pub max_turns: u32,
    pub max_tool_calls: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentAssignment {
    pub name: String,
    pub role: RoleDefinition,
    pub task_brief: String,
    pub depends_on: Vec<String>,
    /// If false, agent runs as a pure LLM call without workspace/tools (for non-code tasks).
    #[serde(default = "default_true")]
    pub needs_workspace: bool,
    /// Optional custom system prompt that overrides the role's default system prompt.
    /// Allows the lead agent to create specialized agents on the fly.
    #[serde(default)]
    pub custom_system_prompt: Option<String>,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionStage {
    pub parallel: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionOrder {
    pub stages: Vec<ExecutionStage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamPlan {
    pub id: TeamOrchestrationId,
    pub task: String,
    pub agents: Vec<AgentAssignment>,
    pub execution_order: ExecutionOrder,
}

// ── Events, commands, cost, status, and result types ──────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamEvent {
    pub orchestration_id: TeamOrchestrationId,
    pub source_agent: String,
    pub source_session: String,
    pub event: AgentEvent,
    pub phase: TeamPhase,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TeamCommand {
    PauseAgent { name: String },
    ResumeAgent { name: String },
    RedirectAgent { name: String, message: String },
    CancelAgent { name: String },
    CancelAll,
    QueryStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WorkerCost {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub usd: f64,
    pub model: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TeamCost {
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub total_usd: f64,
    pub per_agent: HashMap<String, WorkerCost>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamStatus {
    pub orchestration_id: TeamOrchestrationId,
    pub phase: TeamPhase,
    pub workers: HashMap<String, WorkerStatus>,
    pub cost: TeamCost,
    pub started_at: DateTime<Utc>,
    pub elapsed_secs: u64,
}

/// Changed file entry (path + change type description).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamChangedFile {
    pub path: String,
    pub change_type: String, // "added", "modified", "deleted", "renamed"
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentReport {
    pub name: String,
    pub role: String,
    pub status: WorkerStatus,
    pub completion_report: Option<String>,
    pub changed_files: Vec<TeamChangedFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamOrchestrationResult {
    pub orchestration_id: TeamOrchestrationId,
    pub outcome: TeamOutcome,
    pub merge_branch: Option<String>,
    pub changed_files: Vec<TeamChangedFile>,
    pub validation_passed: bool,
    pub cost: TeamCost,
    pub agent_reports: Vec<AgentReport>,
}
