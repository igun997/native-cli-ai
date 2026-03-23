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

A `pub fn suggest_allow_pattern(tool_name: &str, description: &str) -> String` function in `approval.rs`.

**Logic:**
1. Extract the first whitespace-delimited word from `description`.
2. If a first word exists: return `"{tool_name}:{first_word} *"`.
3. If description is empty or a single word with no further content: return `"{tool_name}:*"`.

**Examples:**

| Tool | Description | Pattern |
|------|-------------|---------|
| `execute_bash` | `git status` | `execute_bash:git *` |
| `execute_bash` | `npm install express` | `execute_bash:npm *` |
| `write_file` | `src/main.rs` | `write_file:src/main.rs *` |
| `delete_path` | *(empty)* | `delete_path:*` |

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

```
(KeyCode::Char('u'), KeyModifiers::CONTROL) => {
    if let Some(req) = g.active_approval.clone() {
        let pattern = suggest_allow_pattern(&req.tool, &req.input);
        let call_id = req.call_id.clone();
        g.input_buffer.clear();
        g.cursor_char_idx = 0;
        g.blocks.push(DisplayBlock::System(
            format!("Always allowing: {pattern}"),
        ));
        drop(g);
        if let Some(ref tx) = approval_answer_tx {
            // send AllowPattern variant
        }
        continue;
    }
}
```

**Channel type change:** The `approval_answer_tx` channel changes from sending `(String, bool)` to sending `ApprovalAnswer`:

```rust
enum ApprovalAnswer {
    Verdict { call_id: String, approved: bool },
    AllowPattern { call_id: String, pattern: String },
}
```

**Hint text update:** The approval prompt hint becomes:
```
Reply: y/n · Ctrl+Y approve · Ctrl+N deny · Ctrl+U always allow
```

### 6. Agent Loop — Handling AllowPattern

In `crates/core/src/agent.rs`, where `self.approval.resolve()` is called:

```rust
match self.approval.resolve(call, &description).await {
    ApprovalVerdict::Approved => { /* existing approve logic */ }
    ApprovalVerdict::Denied => { /* existing deny logic */ }
    ApprovalVerdict::AllowPattern(pattern) => {
        self.approval.add_session_allow(pattern);
        // treat as approved
    }
}
```

## Files Changed

| File | Change |
|------|--------|
| `crates/core/src/approval.rs` | `wildcard_matches`, `suggest_allow_pattern`, `session_allow` field, `ApprovalVerdict` enum, update `ApprovalHandler` trait |
| `crates/core/src/agent.rs` | Handle `ApprovalVerdict::AllowPattern` in tool execution loop |
| `crates/cli/src/tui/app.rs` | Ctrl+U handler, `ApprovalAnswer` enum, hint text, channel type |
| `crates/cli/src/approval_prompts.rs` | Update all `ApprovalHandler` impls to return `ApprovalVerdict` |
| `crates/runtime/src/supervisor.rs` | Update IPC approval handler for `ApprovalVerdict` |

## Testing

- Unit tests for `wildcard_matches` — various patterns, edge cases (empty pattern, multiple `*`, no `*` fallback).
- Unit tests for `suggest_allow_pattern` — empty description, single word, multi-word.
- Integration test: add session allow pattern, verify subsequent matching tool call is auto-approved.
- Existing approval tests updated for `ApprovalVerdict` return type.

## Out of Scope

- Persisting session-allow patterns to workspace or global config.
- Full glob syntax (`?`, `[a-z]`, `**`).
- Interactive pattern picker (choosing between tool-level vs prefix wildcard).
