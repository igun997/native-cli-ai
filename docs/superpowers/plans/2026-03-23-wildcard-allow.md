# Wildcard Allow Command Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add wildcard pattern matching to NCA's permission system and a Ctrl+U "always allow" shortcut in the TUI for session-scoped auto-approval.

**Architecture:** Extend `ApprovalPolicy` with a `wildcard_matches` function and session-scoped allow list. Replace the `bool` return type of `ApprovalHandler::resolve()` with an `ApprovalVerdict` enum that supports `AllowPattern`. Wire a Ctrl+U keybinding in the TUI through `repl.rs` → `runner.rs` → oneshot → agent loop.

**Tech Stack:** Rust, async-trait, serde_json, tokio oneshot channels, ratatui TUI

**Spec:** `docs/superpowers/specs/2026-03-23-wildcard-allow-design.md`

---

### Task 1: Wildcard Matching Function

**Files:**
- Modify: `crates/core/src/approval.rs`

- [ ] **Step 1: Write failing tests for `wildcard_matches`**

Add a `#[cfg(test)]` module at the bottom of `crates/core/src/approval.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcard_matches_no_star_falls_back_to_contains() {
        assert!(wildcard_matches("git", "execute_bash:git status"));
        assert!(!wildcard_matches("npm", "execute_bash:git status"));
    }

    #[test]
    fn wildcard_matches_trailing_star() {
        assert!(wildcard_matches("execute_bash:git *", "execute_bash:git status"));
        assert!(wildcard_matches("execute_bash:git *", "execute_bash:git push --force"));
        assert!(!wildcard_matches("execute_bash:git *", "execute_bash:npm install"));
    }

    #[test]
    fn wildcard_matches_leading_star() {
        assert!(wildcard_matches("*:git push", "execute_bash:git push"));
        assert!(!wildcard_matches("*:git push", "execute_bash:npm install"));
    }

    #[test]
    fn wildcard_matches_both_stars() {
        assert!(wildcard_matches("*:git *", "execute_bash:git push"));
        assert!(wildcard_matches("*git*", "execute_bash:git status"));
    }

    #[test]
    fn wildcard_matches_exact() {
        assert!(wildcard_matches("execute_bash:git status", "execute_bash:git status"));
        assert!(!wildcard_matches("execute_bash:git status", "execute_bash:git push"));
    }

    #[test]
    fn wildcard_matches_star_only() {
        assert!(wildcard_matches("*", "anything at all"));
    }

    #[test]
    fn wildcard_matches_empty_pattern() {
        assert!(wildcard_matches("", "anything"));
    }

    #[test]
    fn wildcard_matches_tool_level() {
        assert!(wildcard_matches("execute_bash:*", "execute_bash:git status"));
        assert!(!wildcard_matches("execute_bash:*", "write_file:src/main.rs"));
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p nca-core -- tests::wildcard_matches`
Expected: FAIL — `wildcard_matches` not defined

- [ ] **Step 3: Implement `wildcard_matches`**

Add this function above the `ApprovalPolicy` struct in `crates/core/src/approval.rs`:

```rust
/// Match `text` against `pattern` where `*` matches any substring.
/// If `pattern` contains no `*`, falls back to `text.contains(pattern)`.
pub fn wildcard_matches(pattern: &str, text: &str) -> bool {
    if !pattern.contains('*') {
        return text.contains(pattern);
    }
    let parts: Vec<&str> = pattern.split('*').collect();
    let mut pos = 0;
    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }
        if i == 0 {
            // First segment: text must start with it
            if !text.starts_with(part) {
                return false;
            }
            pos = part.len();
        } else if i == parts.len() - 1 {
            // Last segment: text must end with it
            if !text[pos..].ends_with(part) {
                return false;
            }
        } else {
            // Interior segment: must appear after current position
            match text[pos..].find(part) {
                Some(idx) => pos += idx + part.len(),
                None => return false,
            }
        }
    }
    true
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p nca-core -- tests::wildcard_matches`
Expected: All 8 tests PASS

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/approval.rs
git commit -m "feat: add wildcard_matches function for permission patterns"
```

---

### Task 2: Meaningful Text Extraction & Pattern Suggestion

**Files:**
- Modify: `crates/core/src/approval.rs`

- [ ] **Step 1: Write failing tests for `extract_meaningful_text` and `suggest_allow_pattern`**

Add to the `tests` module in `crates/core/src/approval.rs`:

```rust
    #[test]
    fn extract_meaningful_text_command_key() {
        let input = serde_json::json!({"command": "git status"});
        assert_eq!(extract_meaningful_text(&input), "git status");
    }

    #[test]
    fn extract_meaningful_text_path_key() {
        let input = serde_json::json!({"path": "src/main.rs", "content": "fn main() {}"});
        assert_eq!(extract_meaningful_text(&input), "src/main.rs");
    }

    #[test]
    fn extract_meaningful_text_empty_object() {
        let input = serde_json::json!({});
        assert_eq!(extract_meaningful_text(&input), "");
    }

    #[test]
    fn extract_meaningful_text_string_value() {
        let input = serde_json::json!("hello world");
        assert_eq!(extract_meaningful_text(&input), "hello world");
    }

    #[test]
    fn suggest_pattern_bash_git() {
        let input = serde_json::json!({"command": "git status"});
        assert_eq!(
            suggest_allow_pattern("execute_bash", &input),
            "execute_bash:git *"
        );
    }

    #[test]
    fn suggest_pattern_bash_npm() {
        let input = serde_json::json!({"command": "npm install express"});
        assert_eq!(
            suggest_allow_pattern("execute_bash", &input),
            "execute_bash:npm *"
        );
    }

    #[test]
    fn suggest_pattern_empty_input() {
        let input = serde_json::json!({});
        assert_eq!(
            suggest_allow_pattern("delete_path", &input),
            "delete_path:*"
        );
    }

    #[test]
    fn suggest_pattern_single_word_command() {
        let input = serde_json::json!({"command": "ls"});
        assert_eq!(
            suggest_allow_pattern("execute_bash", &input),
            "execute_bash:ls *"
        );
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p nca-core -- tests::extract_meaningful_text tests::suggest_pattern`
Expected: FAIL — functions not defined

- [ ] **Step 3: Implement `extract_meaningful_text` and `suggest_allow_pattern`**

Add these functions in `crates/core/src/approval.rs` after `wildcard_matches`:

```rust
/// Extract the human-readable text from a tool's JSON input.
/// Looks for known keys: command, path, file_path, url.
pub fn extract_meaningful_text(input: &serde_json::Value) -> String {
    match input {
        serde_json::Value::Object(map) => {
            for key in &["command", "path", "file_path", "url"] {
                if let Some(serde_json::Value::String(s)) = map.get(*key) {
                    return s.clone();
                }
            }
            String::new()
        }
        serde_json::Value::String(s) => s.clone(),
        _ => String::new(),
    }
}

/// Generate a smart wildcard allow pattern from a tool name and its JSON input.
/// E.g. ("execute_bash", {"command":"git status"}) -> "execute_bash:git *"
pub fn suggest_allow_pattern(tool_name: &str, tool_input: &serde_json::Value) -> String {
    let text = extract_meaningful_text(tool_input);
    let first_word = text.split_whitespace().next().unwrap_or("");
    if first_word.is_empty() {
        format!("{tool_name}:*")
    } else {
        format!("{tool_name}:{first_word} *")
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p nca-core -- tests::extract_meaningful_text tests::suggest_pattern`
Expected: All 8 tests PASS

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/approval.rs
git commit -m "feat: add extract_meaningful_text and suggest_allow_pattern"
```

---

### Task 3: Session Allow List & Human-Readable Key in `check()`

**Files:**
- Modify: `crates/core/src/approval.rs`

- [ ] **Step 1: Write failing tests for session allow and human-readable key matching**

Add to the `tests` module in `crates/core/src/approval.rs`:

```rust
    use nca_common::config::PermissionConfig;

    #[test]
    fn session_allow_wildcard_approves_matching_tool() {
        let config = PermissionConfig::default();
        let mut policy = ApprovalPolicy::new(config);
        policy.add_session_allow("execute_bash:git *".into());

        // The key in check() is "{tool_name}:{description}" where description = input JSON string
        // With human-readable key, "execute_bash:git status" should match "execute_bash:git *"
        let tier = policy.check("execute_bash", &serde_json::json!({"command": "git status"}).to_string());
        assert_eq!(tier, PermissionTier::Allowed);
    }

    #[test]
    fn session_allow_does_not_match_different_prefix() {
        let config = PermissionConfig::default();
        let mut policy = ApprovalPolicy::new(config);
        policy.add_session_allow("execute_bash:git *".into());

        let tier = policy.check("execute_bash", &serde_json::json!({"command": "npm install"}).to_string());
        // Should NOT be allowed by session pattern — falls through to default behavior
        assert_ne!(tier, PermissionTier::Allowed);
    }

    #[test]
    fn session_allow_deduplicates() {
        let config = PermissionConfig::default();
        let mut policy = ApprovalPolicy::new(config);
        policy.add_session_allow("execute_bash:git *".into());
        policy.add_session_allow("execute_bash:git *".into());
        assert_eq!(policy.session_allow.len(), 1);
    }

    #[test]
    fn config_allow_wildcard_works() {
        let config = PermissionConfig {
            allow: vec!["execute_bash:git *".into()],
            ..Default::default()
        };
        let policy = ApprovalPolicy::new(config);
        let tier = policy.check("execute_bash", &serde_json::json!({"command": "git status"}).to_string());
        assert_eq!(tier, PermissionTier::Allowed);
    }

    #[test]
    fn deny_wildcard_works() {
        let config = PermissionConfig {
            deny: vec!["execute_bash:rm *".into()],
            ..Default::default()
        };
        let policy = ApprovalPolicy::new(config);
        let tier = policy.check("execute_bash", &serde_json::json!({"command": "rm -rf /"}).to_string());
        assert_eq!(tier, PermissionTier::Denied);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p nca-core -- tests::session_allow tests::config_allow_wildcard tests::deny_wildcard`
Expected: FAIL — `session_allow` field and `add_session_allow` not defined

- [ ] **Step 3: Add `session_allow` field and `add_session_allow` method**

Modify `ApprovalPolicy` struct in `crates/core/src/approval.rs`:

```rust
pub struct ApprovalPolicy {
    config: PermissionConfig,
    handler: Option<Arc<dyn ApprovalHandler>>,
    fail_on_ask: bool,
    session_allow: Vec<String>,
}
```

Update `ApprovalPolicy::new()`:

```rust
    pub fn new(config: PermissionConfig) -> Self {
        Self {
            config,
            handler: None,
            fail_on_ask: false,
            session_allow: Vec::new(),
        }
    }
```

Add method:

```rust
    /// Add a pattern to the session-scoped allow list. Skips duplicates.
    pub fn add_session_allow(&mut self, pattern: String) {
        if !self.session_allow.contains(&pattern) {
            self.session_allow.push(pattern);
        }
    }
```

- [ ] **Step 4: Update `check()` to use wildcard matching and human-readable key**

Replace the `check()` method body. Key changes:
1. Build both `json_key` (existing) and `readable_key` (human-readable) from the description
2. Use `wildcard_matches` instead of `contains` for all pattern checks
3. Check `session_allow` alongside `config.allow`

```rust
    pub fn check(&self, tool_name: &str, description: &str) -> PermissionTier {
        let json_key = format!("{tool_name}:{description}");

        // Build a human-readable key by extracting meaningful text from JSON input
        let readable_key = if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(description) {
            let text = extract_meaningful_text(&parsed);
            if text.is_empty() {
                json_key.clone()
            } else {
                format!("{tool_name}:{text}")
            }
        } else {
            json_key.clone()
        };

        // Deny check: match against both keys
        for pattern in &self.config.deny {
            if wildcard_matches(pattern, &json_key) || wildcard_matches(pattern, &readable_key) {
                return PermissionTier::Denied;
            }
        }

        // Allow check: config.allow + session_allow, match against both keys
        let explicitly_allowed = self
            .config
            .allow
            .iter()
            .chain(self.session_allow.iter())
            .any(|pattern| wildcard_matches(pattern, &json_key) || wildcard_matches(pattern, &readable_key));

        // ... rest of the method stays the same (readonly/file_edit/destructive/mode match) ...
```

The rest of the method (from `let readonly = matches!(...` onward) stays exactly the same.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p nca-core -- tests`
Expected: All tests PASS (wildcard, extract, suggest, session_allow, config_allow, deny)

- [ ] **Step 6: Commit**

```bash
git add crates/core/src/approval.rs
git commit -m "feat: add session_allow list and human-readable key matching in check()"
```

---

### Task 4: ApprovalVerdict Enum & Trait Update

**Files:**
- Modify: `crates/core/src/approval.rs`
- Modify: `crates/core/src/agent.rs:305-340`
- Modify: `crates/runtime/src/supervisor.rs:1047-1074`
- Modify: `crates/cli/src/approval_prompts.rs:117-121,177-200,223-248`

- [ ] **Step 1: Define `ApprovalVerdict` and update `ApprovalHandler` trait**

In `crates/core/src/approval.rs`, add the enum before the trait:

```rust
/// Result of an approval prompt.
#[derive(Debug, Clone)]
pub enum ApprovalVerdict {
    Approved,
    Denied,
    /// User chose "always allow" — pattern should be added to session allow list.
    AllowPattern(String),
}

impl ApprovalVerdict {
    pub fn is_approved(&self) -> bool {
        matches!(self, ApprovalVerdict::Approved | ApprovalVerdict::AllowPattern(_))
    }
}
```

Update the trait:

```rust
#[async_trait::async_trait]
pub trait ApprovalHandler: Send + Sync {
    async fn resolve(&self, call: &ToolCall, description: &str) -> ApprovalVerdict;
}
```

Update `ApprovalPolicy::resolve()`:

```rust
    pub async fn resolve(&self, call: &ToolCall, description: &str) -> ApprovalVerdict {
        match &self.handler {
            Some(handler) => handler.resolve(call, description).await,
            None => ApprovalVerdict::Denied,
        }
    }
```

- [ ] **Step 2: Update `IpcApprovalHandler` in supervisor.rs**

In `crates/runtime/src/supervisor.rs`, update the `IpcApprovalHandler` impl (line 1048):

```rust
#[async_trait::async_trait]
impl ApprovalHandler for IpcApprovalHandler {
    async fn resolve(&self, call: &nca_common::tool::ToolCall, _description: &str) -> ApprovalVerdict {
        let (tx, rx) = oneshot::channel();
        {
            let mut m = self.pending.lock().unwrap();
            m.insert(call.id.clone(), tx);
        }
        match tokio::time::timeout(std::time::Duration::from_secs(300), rx).await {
            Ok(Ok(verdict)) => verdict,
            _ => {
                let mut m = self.pending.lock().unwrap();
                m.remove(&call.id);
                ApprovalVerdict::Denied
            }
        }
    }
}
```

Update `AutoDenyHandler` (line 1070):

```rust
#[async_trait::async_trait]
impl ApprovalHandler for AutoDenyHandler {
    async fn resolve(&self, _call: &nca_common::tool::ToolCall, _description: &str) -> ApprovalVerdict {
        ApprovalVerdict::Denied
    }
}
```

Update `spawn_command_consumer` (lines 956-970) to send `ApprovalVerdict`:

```rust
AgentCommand::ApproveToolCall { call_id } => {
    if let Some(ref p) = approval_pending
        && let Ok(mut m) = p.lock()
        && let Some(tx) = m.remove(&call_id)
    {
        let _ = tx.send(ApprovalVerdict::Approved);
    }
}
AgentCommand::DenyToolCall { call_id } => {
    if let Some(ref p) = approval_pending
        && let Ok(mut m) = p.lock()
        && let Some(tx) = m.remove(&call_id)
    {
        let _ = tx.send(ApprovalVerdict::Denied);
    }
}
```

Add the import at the top of `supervisor.rs`:

```rust
use nca_core::approval::{ApprovalHandler, ApprovalPolicy, ApprovalVerdict};
```

- [ ] **Step 3: Update `ApprovalHandler` impls in approval_prompts.rs**

In `crates/cli/src/approval_prompts.rs`, update the `InteractiveApprovalHandler` impl (line 117):

```rust
#[async_trait::async_trait]
impl ApprovalHandler for InteractiveApprovalHandler {
    async fn resolve(&self, call: &ToolCall, description: &str) -> ApprovalVerdict {
        let _guard = self.prompt_lock.lock().await;
        match self.prompt_approval(call, description) {
            Some(true) => ApprovalVerdict::Approved,
            _ => ApprovalVerdict::Denied,
        }
    }
}
```

Update `InteractiveIpcApprovalHandler` impl (line 177):

```rust
#[async_trait::async_trait]
impl ApprovalHandler for InteractiveIpcApprovalHandler {
    async fn resolve(&self, call: &ToolCall, description: &str) -> ApprovalVerdict {
        let (tx, rx) = tokio::sync::oneshot::channel();
        {
            let mut m = self.pending.lock().await;
            m.insert(call.id.clone(), tx);
        }

        match tokio::time::timeout(std::time::Duration::from_secs(5), rx).await {
            Ok(Ok(approved)) => {
                let mut m = self.pending.lock().await;
                m.remove(&call.id);
                if approved { ApprovalVerdict::Approved } else { ApprovalVerdict::Denied }
            }
            _ => {
                let mut m = self.pending.lock().await;
                m.remove(&call.id);
                drop(m);

                let _guard = self.prompt_lock.lock().await;
                match self.prompt_approval(call, description) {
                    Some(true) => ApprovalVerdict::Approved,
                    _ => ApprovalVerdict::Denied,
                }
            }
        }
    }
}
```

Update `StdioApprovalHandler` impl in `legacy` module (line 223):

```rust
#[async_trait::async_trait]
impl ApprovalHandler for StdioApprovalHandler {
    async fn resolve(&self, call: &ToolCall, description: &str) -> ApprovalVerdict {
        let _guard = self.prompt_lock.lock().await;
        // ... existing stdin prompt logic ...
        if matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
            ApprovalVerdict::Approved
        } else {
            ApprovalVerdict::Denied
        }
    }
}
```

Add import to `approval_prompts.rs`:

```rust
use nca_core::approval::{ApprovalHandler, ApprovalVerdict};
```

- [ ] **Step 4: Update agent loop in agent.rs**

In `crates/core/src/agent.rs` (line 305), replace the approval resolution block:

```rust
                        let verdict = self.approval.resolve(call, &description).await;
                        let approved = verdict.is_approved();
                        self.emit(AgentEvent::ApprovalResolved {
                            call_id: call.id.clone(),
                            approved,
                        })
                        .await;

                        if let ApprovalVerdict::AllowPattern(pattern) = &verdict {
                            self.approval.add_session_allow(pattern.clone());
                        }

                        if approved {
```

The rest of the approved/denied logic stays the same. The `use` import at the top of `agent.rs` needs to add `ApprovalVerdict`:

```rust
use crate::approval::{ApprovalPolicy, ApprovalVerdict};
```

- [ ] **Step 5: Update `ApprovalPendingMap` type alias**

In `crates/cli/src/ipc_pending.rs`, update:

```rust
use nca_core::approval::ApprovalVerdict;

pub type ApprovalPendingMap = Arc<Mutex<HashMap<String, oneshot::Sender<ApprovalVerdict>>>>;
```

- [ ] **Step 6: Update `dispatch_tool_approval` in runner.rs**

In `crates/cli/src/runner.rs` (line 34), update the function:

```rust
use nca_core::approval::ApprovalVerdict;

pub fn dispatch_tool_approval(
    approvals: &Option<ApprovalPendingMap>,
    call_id: &str,
    verdict: ApprovalVerdict,
) -> bool {
    let Some(approvals) = approvals else {
        return false;
    };
    let Ok(mut map) = approvals.lock() else {
        return false;
    };
    let Some(tx) = map.remove(call_id) else {
        return false;
    };
    tx.send(verdict).is_ok()
}
```

- [ ] **Step 7: Verify it compiles**

Run: `cargo build -p nca-cli`
Expected: Compilation errors in `repl.rs` (channel type mismatch) — this is expected and will be fixed in Task 5.

- [ ] **Step 8: Commit**

```bash
git add crates/core/src/approval.rs crates/core/src/agent.rs crates/runtime/src/supervisor.rs crates/cli/src/approval_prompts.rs crates/cli/src/ipc_pending.rs crates/cli/src/runner.rs
git commit -m "feat: replace bool approval with ApprovalVerdict enum across all handlers"
```

---

### Task 5: TUI Wiring — Ctrl+U, ApprovalAnswer, Channel Update

**Files:**
- Modify: `crates/cli/src/tui/app.rs:1108,3151-3174`
- Modify: `crates/cli/src/repl.rs:1612-1627`

- [ ] **Step 1: Define `ApprovalAnswer` enum in `tui/app.rs`**

Add near the top of `crates/cli/src/tui/app.rs` (after the imports):

```rust
use nca_core::approval::{ApprovalVerdict, suggest_allow_pattern};

/// Message from TUI to the approval dispatch task.
#[derive(Debug)]
pub enum ApprovalAnswer {
    Verdict { call_id: String, approved: bool },
    AllowPattern { call_id: String, pattern: String },
}
```

- [ ] **Step 2: Update channel type in repl.rs**

In `crates/cli/src/repl.rs` (line 1612), change the channel:

```rust
use crate::tui::app::ApprovalAnswer;
```

```rust
        let (approval_tx, mut approval_rx) =
            tokio::sync::mpsc::unbounded_channel::<ApprovalAnswer>();
        let approval_dispatch = approval.clone();
        let approval_state = tui_state.clone();
        tokio::spawn(async move {
            while let Some(answer) = approval_rx.recv().await {
                let (call_id, verdict) = match answer {
                    ApprovalAnswer::Verdict { call_id, approved } => (
                        call_id,
                        if approved { ApprovalVerdict::Approved } else { ApprovalVerdict::Denied },
                    ),
                    ApprovalAnswer::AllowPattern { call_id, pattern } => (
                        call_id,
                        ApprovalVerdict::AllowPattern(pattern),
                    ),
                };
                if !dispatch_tool_approval(&approval_dispatch, &call_id, verdict)
                    && let Ok(mut g) = approval_state.lock()
                {
                    g.clear_active_approval_if_matches(&call_id);
                    g.push_error(
                        "approval was no longer pending; cleared stale approval state".into(),
                    );
                }
            }
        });
```

Add import at the top of `repl.rs`:

```rust
use nca_core::approval::ApprovalVerdict;
```

- [ ] **Step 3: Update existing Ctrl+Y and Ctrl+N to send `ApprovalAnswer::Verdict`**

In `crates/cli/src/tui/app.rs` (line 3151), update the Ctrl+Y handler:

```rust
(KeyCode::Char('y'), KeyModifiers::CONTROL) => {
    if let Some(req) = g.active_approval.clone() {
        let call_id = req.call_id.clone();
        g.input_buffer.clear();
        g.cursor_char_idx = 0;
        drop(g);
        if let Some(ref tx) = approval_answer_tx {
            let _ = tx.send(ApprovalAnswer::Verdict { call_id, approved: true });
        }
        continue;
    }
}
```

Similarly update Ctrl+N (line 3163):

```rust
(KeyCode::Char('n'), KeyModifiers::CONTROL) => {
    if let Some(req) = g.active_approval.clone() {
        let call_id = req.call_id.clone();
        g.input_buffer.clear();
        g.cursor_char_idx = 0;
        drop(g);
        if let Some(ref tx) = approval_answer_tx {
            let _ = tx.send(ApprovalAnswer::Verdict { call_id, approved: false });
        }
        continue;
    }
}
```

Also update all other places that send through `approval_answer_tx` (the Enter-key approval path around line 3197-3218) to use `ApprovalAnswer::Verdict`.

- [ ] **Step 4: Add Ctrl+U handler**

In `crates/cli/src/tui/app.rs`, add right after the Ctrl+N handler (after line 3174):

```rust
(KeyCode::Char('u'), KeyModifiers::CONTROL) => {
    if let Some(req) = g.active_approval.clone() {
        let input_json: serde_json::Value =
            serde_json::from_str(&req.input).unwrap_or_default();
        let pattern = suggest_allow_pattern(&req.tool, &input_json);
        let call_id = req.call_id.clone();
        g.input_buffer.clear();
        g.cursor_char_idx = 0;
        g.blocks.push(DisplayBlock::System(
            format!("Always allowing: {pattern}"),
        ));
        drop(g);
        if let Some(ref tx) = approval_answer_tx {
            let _ = tx.send(ApprovalAnswer::AllowPattern { call_id, pattern });
        }
        continue;
    }
}
```

- [ ] **Step 5: Update hint text**

In `crates/cli/src/tui/app.rs` (line 1108), update the approval hint:

```rust
        Line::from(Span::styled(
            " Reply: y/n · Ctrl+Y approve · Ctrl+N deny · Ctrl+U always allow",
            Style::default().fg(theme::MUTED),
        )),
```

- [ ] **Step 6: Update channel type in `run_blocking` signature and callers**

Find where `approval_answer_tx` is declared with type `UnboundedSender<(String, bool)>` in `app.rs` and update to `UnboundedSender<ApprovalAnswer>`. The function signature for `run_blocking` in `crates/cli/src/tui/mod.rs` or `app.rs` needs to accept `Option<UnboundedSender<ApprovalAnswer>>` instead of `Option<UnboundedSender<(String, bool)>>`.

- [ ] **Step 7: Build and verify**

Run: `cargo build -p nca-cli`
Expected: Clean compilation with no errors

- [ ] **Step 8: Commit**

```bash
git add crates/cli/src/tui/app.rs crates/cli/src/repl.rs
git commit -m "feat: add Ctrl+U always-allow shortcut with session-scoped wildcard pattern"
```

---

### Task 6: Final Verification & Cleanup

**Files:**
- All modified files

- [ ] **Step 1: Run full test suite**

Run: `cargo test --workspace`
Expected: All tests pass

- [ ] **Step 2: Run clippy**

Run: `cargo clippy --workspace -- -D warnings`
Expected: No warnings

- [ ] **Step 3: Build release binary**

Run: `cargo build --release`
Expected: Clean build

- [ ] **Step 4: Manual smoke test**

1. Install: `cp target/release/nca /usr/local/bin/`
2. Start a session in a test directory
3. Trigger a tool that requires approval (e.g., bash command)
4. Press Ctrl+U — verify "Always allowing: ..." message appears
5. Trigger same category of tool — verify it auto-approves without prompting
6. Verify Ctrl+Y and Ctrl+N still work as before

- [ ] **Step 5: Commit any cleanup**

```bash
git add -A
git commit -m "chore: cleanup after wildcard allow implementation"
```
