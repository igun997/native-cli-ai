# Wildcard Allow Command

**Issue:** [#18](https://github.com/madebyaris/native-cli-ai/issues/18)
**Date:** 2026-03-23
**Status:** Design approved

## Problem

Users must approve the same tool actions repeatedly (e.g., every `git status`, every `npm install`). Claude Code and OpenCode solve this with wildcard allow patterns like `git:*`. NCA currently uses simple substring matching (`key.contains(pattern)`) with no wildcard support and no way to add patterns during a session.

## Solution

Two features:

1. **Wildcard matching engine** — upgrade the `allow`/`deny` pattern matching from `contains` to support `*` wildcards.
2. **Interactive "always allow" (Ctrl+U)** — during an approval prompt, press Ctrl+U to auto-generate a smart prefix wildcard pattern and add it to a session-only allow list.

## Design

### 1. Wildcard Matching Engine

A `wildcard_matches(pattern: &str, text: &str) -> bool` function in `crates/core/src/approval.rs`.

**Algorithm:**
- If pattern contains no `*`, fall back to `text.contains(pattern)` (backward compatible).
- Split pattern on `*` into segments.
- If pattern starts with a non-`*` segment, text must start with it.
- If pattern ends with a non-`*` segment, text must end with it.
- Interior segments must appear in order within the text.

**Examples:**

| Pattern | Key | Match? |
|---------|-----|--------|
| `execute_bash:git *` | `execute_bash:git status` | yes |
| `execute_bash:git *` | `execute_bash:npm install` | no |
| `*:git *` | `execute_bash:git push` | yes |
| `git` | `execute_bash:git status` | yes (contains, backward compat) |

This function replaces `key.contains(pattern)` everywhere in `ApprovalPolicy::check()` — both `allow` and `deny` lists.

### 2. Session-Only Allow List

Add a `session_allow: Vec<String>` field to `ApprovalPolicy`:

```rust
pub struct ApprovalPolicy {
    config: PermissionConfig,
    handler: Option<Arc<dyn ApprovalHandler>>,
    fail_on_ask: bool,
    session_allow: Vec<String>,  // NEW
}
```

- Initialized empty in `ApprovalPolicy::new()`.
- Checked alongside `config.allow` in `check()` using `wildcard_matches`.
- New method: `pub fn add_session_allow(&mut self, pattern: String)` — pushes to the list, skips duplicates.
- Never persisted to disk. Lost when the session ends.

**Matching order (unchanged):** deny first, then allow (config + session), then tier-based defaults.

### 3. Smart Prefix Pattern Extraction

A `pub fn suggest_allow_pattern(tool_name: &str, tool_input: &serde_json::Value) -> String` function in `approval.rs`.

**Input data:** In the TUI, `ApprovalRequest.input` is a JSON string from `ToolCallStarted.input` (e.g., `{"command":"git status"}`). The function receives the parsed `serde_json::Value`.

**Logic:**
1. Extract the "meaningful text" from the tool input:
   - For objects: look for known keys in order: `command`, `path`, `file_path`, `url`. Take the first found string value.
   - For strings: use the string directly.
   - Otherwise: empty string.
2. Extract the first whitespace-delimited word from the meaningful text.
3. If a first word exists: return `"{tool_name}:{first_word} *"`.
4. If the meaningful text is empty: return `"{tool_name}:*"`.

**Note on matching:** The key built in `ApprovalPolicy::check()` is `"{tool_name}:{input.to_string()}"` where `input.to_string()` is the JSON serialization. The generated pattern `execute_bash:git *` must match against this JSON key. Since the key contains the JSON like `execute_bash:{"command":"git status"}`, the pattern needs to match against that. Therefore patterns should be constructed to match against the full JSON key: `execute_bash:*git *` (with leading `*` to skip JSON structure before the command text). Alternatively, `check()` can be updated to also build a "human-readable key" for matching. **Recommended:** update `check()` to extract the same meaningful text and build a secondary key `"{tool_name}:{meaningful_text}"` for pattern matching, keeping the JSON key for deny matching backward compatibility.

**Examples (with human-readable key):**

| Tool | Input JSON | Extracted Text | Pattern |
|------|-----------|----------------|---------|
| `execute_bash` | `{"command":"git status"}` | `git status` | `execute_bash:git *` |
| `execute_bash` | `{"command":"npm install express"}` | `npm install express` | `execute_bash:npm *` |
| `write_file` | `{"path":"src/main.rs","content":"..."}` | `src/main.rs` | `write_file:src/main.rs *` |
| `delete_path` | `{}` | *(empty)* | `delete_path:*` |

### 4. ApprovalVerdict Enum

Replace the `bool` return type of `ApprovalHandler::resolve()`:

```rust
pub enum ApprovalVerdict {
    Approved,
    Denied,
    AllowPattern(String),
}

#[async_trait::async_trait]
pub trait ApprovalHandler: Send + Sync {
    async fn resolve(&self, call: &ToolCall, description: &str) -> ApprovalVerdict;
}
```

All existing `ApprovalHandler` implementations are updated to return `ApprovalVerdict::Approved` / `ApprovalVerdict::Denied` instead of `true` / `false`.

### 5. TUI Keybinding — Ctrl+U

In `crates/cli/src/tui/app.rs`, add a handler next to the existing Ctrl+Y (~line 3151):

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

**Channel type change:** The `approval_answer_tx` channel changes from sending `(String, bool)` to sending `ApprovalAnswer`:

```rust
pub enum ApprovalAnswer {
    Verdict { call_id: String, approved: bool },
    AllowPattern { call_id: String, pattern: String },
}
```

**Hint text update:** The approval prompt hint becomes:
```
Reply: y/n · Ctrl+Y approve · Ctrl+N deny · Ctrl+U always allow
```

### 6. Data Flow — TUI to Agent Loop

The full data flow for `AllowPattern` through the TUI path:

1. **TUI** (app.rs): User presses Ctrl+U. TUI sends `ApprovalAnswer::AllowPattern { call_id, pattern }` via `approval_answer_tx`.

2. **REPL dispatch** (repl.rs ~line 1613): The approval dispatch task currently receives `(String, bool)` from `approval_rx` and calls `dispatch_tool_approval()`. This changes to receive `ApprovalAnswer` and dispatch accordingly:
   - `ApprovalAnswer::Verdict { call_id, approved }` -> sends `ApprovalVerdict::Approved` or `ApprovalVerdict::Denied` via oneshot
   - `ApprovalAnswer::AllowPattern { call_id, pattern }` -> sends `ApprovalVerdict::AllowPattern(pattern)` via oneshot

3. **Runner** (runner.rs): `dispatch_tool_approval()` signature changes from `(approvals, call_id, approved: bool)` to `(approvals, call_id, verdict: ApprovalVerdict)`. It resolves the oneshot with the verdict directly.

4. **IPC pending map** (ipc_pending.rs): `ApprovalPendingMap` type changes from `Arc<Mutex<HashMap<String, oneshot::Sender<bool>>>>` to `Arc<Mutex<HashMap<String, oneshot::Sender<ApprovalVerdict>>>>`.

5. **Agent loop** (agent.rs): `self.approval.resolve()` now returns `ApprovalVerdict`. The `IpcApprovalHandler` in supervisor.rs awaits the oneshot and returns the verdict:

```rust
let verdict = self.approval.resolve(call, &description).await;
self.emit(AgentEvent::ApprovalResolved {
    call_id: call.id.clone(),
    approved: matches!(verdict, ApprovalVerdict::Approved | ApprovalVerdict::AllowPattern(_)),
}).await;
match verdict {
    ApprovalVerdict::Approved => { /* existing approve logic */ }
    ApprovalVerdict::Denied => { /* existing deny logic */ }
    ApprovalVerdict::AllowPattern(pattern) => {
        self.approval.add_session_allow(pattern);
        // then proceed with existing approve logic
    }
}
```

**IPC handler changes in supervisor.rs:**
- `IpcApprovalHandler.pending` map type: `oneshot::Sender<bool>` -> `oneshot::Sender<ApprovalVerdict>`
- The `resolve` impl awaits the oneshot and returns `ApprovalVerdict` directly
- Fallback prompt (when IPC times out) returns `ApprovalVerdict::Approved` / `ApprovalVerdict::Denied`

**IPC socket path (supervisor `spawn_command_consumer`):**
- `AgentCommand::ApproveToolCall` / `DenyToolCall` also updated to send `ApprovalVerdict` through the pending map
- No new `AgentCommand` variant for AllowPattern via IPC socket (out of scope)

## Files Changed

| File | Change |
|------|--------|
| `crates/core/src/approval.rs` | `wildcard_matches`, `suggest_allow_pattern`, `extract_meaningful_text`, `session_allow` field, `ApprovalVerdict` enum, update `ApprovalHandler` trait, update `check()` to use human-readable key |
| `crates/core/src/agent.rs` | Handle `ApprovalVerdict::AllowPattern` in tool execution loop, update `ApprovalResolved` event |
| `crates/cli/src/tui/app.rs` | Ctrl+U handler, `ApprovalAnswer` enum, hint text, channel type change |
| `crates/cli/src/approval_prompts.rs` | Update all `ApprovalHandler` impls to return `ApprovalVerdict` |
| `crates/cli/src/ipc_pending.rs` | Update `ApprovalPendingMap` type alias to `oneshot<ApprovalVerdict>` |
| `crates/cli/src/runner.rs` | Update `dispatch_tool_approval` to accept `ApprovalVerdict` |
| `crates/cli/src/repl.rs` | Update approval dispatch task to handle `ApprovalAnswer` enum |
| `crates/runtime/src/supervisor.rs` | Update `IpcApprovalHandler` and `spawn_command_consumer` for `ApprovalVerdict` |

## Testing

- Unit tests for `wildcard_matches` — various patterns, edge cases (empty pattern, multiple `*`, no `*` fallback).
- Unit tests for `suggest_allow_pattern` — empty description, single word, multi-word.
- Integration test: add session allow pattern, verify subsequent matching tool call is auto-approved.
- Existing approval tests updated for `ApprovalVerdict` return type.

## Out of Scope

- Persisting session-allow patterns to workspace or global config.
- Full glob syntax (`?`, `[a-z]`, `**`).
- Interactive pattern picker (choosing between tool-level vs prefix wildcard).
