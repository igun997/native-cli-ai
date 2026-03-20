//! Transcript + status driven by `AgentEvent`.

use nca_common::event::{AgentEvent, InteractiveQuestionPayload};
use serde_json::Value;
use std::time::Instant;

#[derive(Debug, Clone)]
pub enum DisplayBlock {
    User(String),
    Assistant(String),
    ToolRunning {
        name: String,
        call_id: String,
        input: String,
    },
    ApprovalPending(ApprovalRequest),
    ApprovalResolved {
        tool: String,
        approved: bool,
    },
    ToolDone {
        name: String,
        ok: bool,
        detail: String,
    },
    /// Interactive `ask_question` prompt (options + suggested answer).
    Question(InteractiveQuestionPayload),
    System(String),
    ErrorLine(String),
}

#[derive(Debug, Clone)]
pub struct ApprovalRequest {
    pub call_id: String,
    pub tool: String,
    pub description: String,
    pub input: String,
}

pub struct TuiSessionState {
    pub blocks: Vec<DisplayBlock>,
    /// In-progress assistant text (shown below committed blocks until finalized).
    pub streaming_assistant: Option<String>,
    pub input_buffer: String,
    pub cursor_char_idx: usize,
    /// Scroll offset in *lines* (flattened transcript).
    pub scroll_lines: usize,
    /// When true, transcript stays pinned to the bottom as new output arrives.
    pub transcript_follow_tail: bool,
    pub session_id: String,
    pub model: String,
    pub agent_profile: String,
    pub permission_mode: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cost_usd: f64,
    pub started: Instant,
    pub busy: bool,
    pub should_exit: bool,
    /// Selected row in slash-command popup (↑↓ or click).
    pub slash_menu_index: usize,
    /// Centered command palette opened via Ctrl+P.
    pub command_palette_open: bool,
    /// Filter text for the command palette.
    pub command_palette_query: String,
    /// Approval request currently waiting for a local TUI answer.
    pub active_approval: Option<ApprovalRequest>,
    /// When set, the composer answers this question (see status hint).
    pub active_question: Option<InteractiveQuestionPayload>,
}

impl TuiSessionState {
    pub fn new(
        session_id: String,
        model: String,
        agent_profile: String,
        permission_mode: String,
    ) -> Self {
        Self {
            blocks: Vec::new(),
            streaming_assistant: None,
            input_buffer: String::new(),
            cursor_char_idx: 0,
            scroll_lines: 0,
            transcript_follow_tail: true,
            session_id,
            model,
            agent_profile,
            permission_mode,
            input_tokens: 0,
            output_tokens: 0,
            cost_usd: 0.0,
            started: Instant::now(),
            busy: false,
            should_exit: false,
            slash_menu_index: 0,
            command_palette_open: false,
            command_palette_query: String::new(),
            active_approval: None,
            active_question: None,
        }
    }

    pub fn set_busy(&mut self, busy: bool) {
        self.busy = busy;
    }

    pub fn push_error(&mut self, msg: String) {
        self.blocks.push(DisplayBlock::ErrorLine(msg));
    }

    pub fn set_agent_profile(&mut self, label: &str) {
        self.agent_profile = label.to_string();
    }

    pub fn set_permission_mode(&mut self, mode: &str) {
        self.permission_mode = mode.to_string();
    }

    fn flush_stream_before_tool(&mut self) {
        if let Some(s) = self.streaming_assistant.take() {
            if !s.trim().is_empty() {
                self.blocks.push(DisplayBlock::Assistant(s));
            }
        }
    }

    pub fn apply_event(&mut self, e: &AgentEvent) {
        match e {
            AgentEvent::SessionStarted {
                session_id, model, ..
            } => {
                self.session_id = session_id.clone();
                self.model = model.clone();
            }
            AgentEvent::MessageReceived { role, content } => {
                if role == "user" {
                    self.streaming_assistant = None;
                    self.blocks.push(DisplayBlock::User(content.clone()));
                } else if role == "assistant" {
                    self.streaming_assistant = None;
                    self.blocks.push(DisplayBlock::Assistant(content.clone()));
                }
            }
            AgentEvent::TokensStreamed { delta } => {
                self.streaming_assistant
                    .get_or_insert_with(String::new)
                    .push_str(delta);
            }
            AgentEvent::ToolCallStarted {
                call_id,
                tool,
                input,
            } => {
                self.flush_stream_before_tool();
                self.blocks.push(DisplayBlock::ToolRunning {
                    name: tool.clone(),
                    call_id: call_id.clone(),
                    input: format_tool_input(input),
                });
            }
            AgentEvent::ToolCallCompleted { call_id, output } => {
                let ok = output.success;
                self.active_approval = self
                    .active_approval
                    .take()
                    .filter(|req| req.call_id != *call_id);
                let detail = if ok {
                    truncate(&output.output, 120)
                } else {
                    output.error.clone().unwrap_or_else(|| "failed".into())
                };
                if let Some(idx) = self.blocks.iter().rposition(
                    |b| {
                        matches!(b, DisplayBlock::ToolRunning { call_id: id, .. } if id == call_id)
                            || matches!(b, DisplayBlock::ApprovalPending(req) if req.call_id == *call_id)
                    },
                ) {
                    let name = match &self.blocks[idx] {
                        DisplayBlock::ToolRunning { name, .. } => name.clone(),
                        DisplayBlock::ApprovalPending(req) => req.tool.clone(),
                        _ => "?".into(),
                    };
                    self.blocks[idx] = DisplayBlock::ToolDone { name, ok, detail };
                } else {
                    self.blocks.push(DisplayBlock::ToolDone {
                        name: "?".into(),
                        ok,
                        detail,
                    });
                }
            }
            AgentEvent::ApprovalRequested {
                call_id,
                tool,
                description,
            } => {
                let input = self
                    .blocks
                    .iter()
                    .rev()
                    .find_map(|block| match block {
                        DisplayBlock::ToolRunning {
                            call_id: id, input, ..
                        } if id == call_id => Some(input.clone()),
                        _ => None,
                    })
                    .unwrap_or_else(|| "{}".into());
                let req = ApprovalRequest {
                    call_id: call_id.clone(),
                    tool: tool.clone(),
                    description: description.clone(),
                    input,
                };
                self.active_approval = Some(req.clone());
                if let Some(idx) = self.blocks.iter().rposition(
                    |b| matches!(b, DisplayBlock::ToolRunning { call_id: id, .. } if id == call_id),
                ) {
                    self.blocks[idx] = DisplayBlock::ApprovalPending(req);
                } else {
                    self.blocks.push(DisplayBlock::ApprovalPending(req));
                }
            }
            AgentEvent::ApprovalResolved { call_id, approved } => {
                let tool = self
                    .active_approval
                    .as_ref()
                    .filter(|req| req.call_id == *call_id)
                    .map(|req| req.tool.clone())
                    .or_else(|| {
                        self.blocks.iter().rev().find_map(|block| match block {
                            DisplayBlock::ApprovalPending(req) if req.call_id == *call_id => {
                                Some(req.tool.clone())
                            }
                            _ => None,
                        })
                    })
                    .unwrap_or_else(|| "tool".into());
                self.active_approval = self
                    .active_approval
                    .take()
                    .filter(|req| req.call_id != *call_id);
                self.blocks.push(DisplayBlock::ApprovalResolved {
                    tool,
                    approved: *approved,
                });
            }
            AgentEvent::QuestionRequested { question } => {
                self.active_question = Some(question.clone());
                self.blocks.push(DisplayBlock::Question(question.clone()));
                // Bring the prompt into view when follow-tail is on (default).
                self.transcript_follow_tail = true;
            }
            AgentEvent::QuestionResolved {
                question_id,
                selection,
            } => {
                self.active_question = None;
                self.blocks.push(DisplayBlock::System(format!(
                    "Answered question {question_id}: {selection:?}"
                )));
            }
            AgentEvent::CostUpdated {
                input_tokens,
                output_tokens,
                estimated_cost_usd,
            } => {
                self.input_tokens = *input_tokens;
                self.output_tokens = *output_tokens;
                self.cost_usd = *estimated_cost_usd;
            }
            AgentEvent::Error { message } => {
                self.blocks.push(DisplayBlock::ErrorLine(message.clone()));
            }
            AgentEvent::Checkpoint { phase, detail, .. } => {
                let msg = if detail.is_empty() {
                    phase.clone()
                } else {
                    format!("{phase}: {}", truncate(detail, 120))
                };
                self.blocks.push(DisplayBlock::System(msg));
            }
            AgentEvent::ChildSessionSpawned {
                child_session_id,
                task,
                ..
            } => {
                let short = if child_session_id.len() > 8 {
                    &child_session_id[..8]
                } else {
                    child_session_id.as_str()
                };
                self.blocks.push(DisplayBlock::System(format!(
                    "Sub-agent {short}: {}",
                    truncate(task, 80)
                )));
            }
            AgentEvent::ChildSessionCompleted {
                child_session_id,
                status,
                ..
            } => {
                let short = if child_session_id.len() > 8 {
                    &child_session_id[..8]
                } else {
                    child_session_id.as_str()
                };
                self.blocks.push(DisplayBlock::System(format!(
                    "Sub-agent {short} done: {status}"
                )));
            }
            _ => {}
        }
    }
}

fn truncate(s: &str, max: usize) -> String {
    let t = s.trim();
    if t.chars().count() <= max {
        t.to_string()
    } else {
        format!(
            "{}…",
            t.chars().take(max.saturating_sub(1)).collect::<String>()
        )
    }
}

fn format_tool_input(value: &Value) -> String {
    if let Some(raw) = value.as_str() {
        if let Ok(parsed) = serde_json::from_str::<Value>(raw) {
            return serde_json::to_string_pretty(&parsed).unwrap_or_else(|_| raw.to_string());
        }
    }
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use nca_common::event::{InteractiveQuestionPayload, QuestionOption, QuestionSelection};

    #[test]
    fn question_requested_sets_active_question() {
        let mut st = TuiSessionState::new(
            "session-x".into(),
            "m".into(),
            "@build".into(),
            "default".into(),
        );
        let q = InteractiveQuestionPayload {
            question_id: "q-1".into(),
            call_id: "c1".into(),
            prompt: "Pick".into(),
            options: vec![QuestionOption {
                id: "a".into(),
                label: "A".into(),
            }],
            allow_custom: true,
            suggested_answer: "A".into(),
        };
        st.apply_event(&AgentEvent::QuestionRequested {
            question: q.clone(),
        });
        assert_eq!(
            st.active_question.as_ref().map(|x| x.question_id.as_str()),
            Some("q-1")
        );
        assert!(matches!(st.blocks.last(), Some(DisplayBlock::Question(_))));

        st.apply_event(&AgentEvent::QuestionResolved {
            question_id: "q-1".into(),
            selection: QuestionSelection::Suggested,
        });
        assert!(st.active_question.is_none());
    }

    #[test]
    fn approval_requested_promotes_running_tool_with_input() {
        let mut st = TuiSessionState::new(
            "session-x".into(),
            "m".into(),
            "@build".into(),
            "default".into(),
        );
        st.apply_event(&AgentEvent::ToolCallStarted {
            call_id: "call-1".into(),
            tool: "execute_bash".into(),
            input: serde_json::json!({"command":"ls -la"}),
        });
        st.apply_event(&AgentEvent::ApprovalRequested {
            call_id: "call-1".into(),
            tool: "execute_bash".into(),
            description: "Tool `execute_bash` requires approval".into(),
        });

        assert!(st.active_approval.is_some());
        match st.blocks.last() {
            Some(DisplayBlock::ApprovalPending(req)) => {
                assert_eq!(req.tool, "execute_bash");
                assert!(req.input.contains("command"));
                assert!(req.input.contains("ls -la"));
            }
            other => panic!("expected approval block, got {other:?}"),
        }
    }
}
