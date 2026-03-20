use nca_common::tool::{ToolCall, ToolDefinition, ToolResult};
use tokio::sync::{mpsc, oneshot};

use super::ToolExecutor;

/// Request sent from the tool to the runtime to start team orchestration.
#[derive(Debug)]
pub struct OrchestrationRequest {
    pub task: String,
    pub agent_hints: Option<String>,
    pub reply: oneshot::Sender<OrchestrationResponse>,
}

/// Response from the runtime after orchestration completes.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct OrchestrationResponse {
    pub orchestration_id: String,
    pub status: String,
    pub outcome: String,
    pub agent_reports: Vec<AgentReportSummary>,
    pub merge_branch: Option<String>,
    pub cost_usd: f64,
    pub event_log: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AgentReportSummary {
    pub name: String,
    pub role: String,
    pub status: String,
    pub report: Option<String>,
}

pub struct OrchestrateTeamTool {
    orch_tx: mpsc::Sender<OrchestrationRequest>,
}

impl OrchestrateTeamTool {
    pub fn new(orch_tx: mpsc::Sender<OrchestrationRequest>) -> Self {
        Self { orch_tx }
    }
}

#[async_trait::async_trait]
impl ToolExecutor for OrchestrateTeamTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "orchestrate_team".into(),
            description: "Spawn a team of AI agents to collaboratively solve a complex task. \
                A lead agent decomposes the task into subtasks, assigns them to specialized agents \
                (researcher, implementer, reviewer, tester, etc.), and coordinates their execution. \
                Use this for tasks that benefit from multiple perspectives or require distinct phases \
                (research → implement → test). Each agent can have custom instructions and identity. \
                For simple, single-step tasks, handle them directly instead."
                .into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "task": {
                        "type": "string",
                        "description": "A clear description of the complex task to be solved by the agent team."
                    },
                    "agent_hints": {
                        "type": "string",
                        "description": "Optional hints about which agents to use (e.g. 'researcher, implementer, tester' or 'a security auditor, a fixer'). If omitted, the lead agent decides."
                    }
                },
                "required": ["task"]
            }),
        }
    }

    async fn execute(&self, call: &ToolCall) -> ToolResult {
        let task = call.input["task"].as_str().unwrap_or("").to_string();
        if task.is_empty() {
            return ToolResult {
                call_id: call.id.clone(),
                success: false,
                output: String::new(),
                error: Some("task is required".into()),
            };
        }

        let agent_hints = call.input["agent_hints"].as_str().map(String::from);

        let (reply_tx, reply_rx) = oneshot::channel();
        let req = OrchestrationRequest {
            task,
            agent_hints,
            reply: reply_tx,
        };

        if self.orch_tx.send(req).await.is_err() {
            return ToolResult {
                call_id: call.id.clone(),
                success: false,
                output: String::new(),
                error: Some("Orchestration handler is not available".into()),
            };
        }

        // Wait up to 30 minutes for orchestration to complete
        match tokio::time::timeout(std::time::Duration::from_secs(1800), reply_rx).await {
            Ok(Ok(response)) => {
                let output = serde_json::to_string_pretty(&response).unwrap_or_default();
                let success = response.status == "completed";
                ToolResult {
                    call_id: call.id.clone(),
                    success,
                    output,
                    error: if success {
                        None
                    } else {
                        Some(format!(
                            "Orchestration finished with status: {}",
                            response.status
                        ))
                    },
                }
            }
            Ok(Err(_)) => ToolResult {
                call_id: call.id.clone(),
                success: false,
                output: String::new(),
                error: Some("Orchestration handler dropped the reply channel".into()),
            },
            Err(_) => ToolResult {
                call_id: call.id.clone(),
                success: false,
                output: String::new(),
                error: Some("Orchestration timed out after 30 minutes".into()),
            },
        }
    }
}
