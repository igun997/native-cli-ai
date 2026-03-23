use nca_common::config::{PermissionConfig, PermissionMode};
use nca_common::tool::{PermissionTier, ToolCall};
use std::sync::Arc;

#[async_trait::async_trait]
pub trait ApprovalHandler: Send + Sync {
    async fn resolve(&self, call: &ToolCall, description: &str) -> bool;
}

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

/// Determines whether a tool call or command is allowed, needs approval, or is denied.
pub struct ApprovalPolicy {
    config: PermissionConfig,
    handler: Option<Arc<dyn ApprovalHandler>>,
    fail_on_ask: bool,
}

impl ApprovalPolicy {
    pub fn new(config: PermissionConfig) -> Self {
        Self {
            config,
            handler: None,
            fail_on_ask: false,
        }
    }

    pub fn with_handler(mut self, handler: Arc<dyn ApprovalHandler>) -> Self {
        self.handler = Some(handler);
        self
    }

    pub fn fail_on_ask(mut self) -> Self {
        self.fail_on_ask = true;
        self
    }

    /// Check the permission tier for a given tool name and input description.
    pub fn check(&self, tool_name: &str, description: &str) -> PermissionTier {
        let key = format!("{tool_name}:{description}");

        for pattern in &self.config.deny {
            if key.contains(pattern) {
                return PermissionTier::Denied;
            }
        }

        let explicitly_allowed = self
            .config
            .allow
            .iter()
            .any(|pattern| key.contains(pattern));

        let readonly = matches!(
            tool_name,
            "read_file"
                | "list_directory"
                | "search_code"
                | "git_status"
                | "git_diff"
                | "query_symbols"
                | "web_search"
                | "fetch_url"
                | "ask_question"
        );
        let file_edit = matches!(
            tool_name,
            "write_file"
                | "create_directory"
                | "apply_patch"
                | "edit_file"
                | "rename_path"
                | "move_path"
                | "copy_path"
                // Spawning a sub-agent is a coordination action equivalent to
                // delegating file-edit work; auto-approve at AcceptEdits and above.
                | "spawn_subagent"
        );
        let destructive = matches!(tool_name, "delete_path");
        match self.config.mode {
            PermissionMode::BypassPermissions => PermissionTier::Allowed,
            PermissionMode::Plan => {
                if readonly {
                    PermissionTier::Allowed
                } else {
                    PermissionTier::Denied
                }
            }
            PermissionMode::AcceptEdits => {
                if destructive {
                    PermissionTier::Ask
                } else if explicitly_allowed || readonly || file_edit {
                    PermissionTier::Allowed
                } else {
                    PermissionTier::Ask
                }
            }
            PermissionMode::DontAsk => {
                if readonly {
                    PermissionTier::Allowed
                } else {
                    PermissionTier::Denied
                }
            }
            PermissionMode::Default => {
                if explicitly_allowed || readonly {
                    PermissionTier::Allowed
                } else {
                    PermissionTier::Ask
                }
            }
        }
    }

    pub async fn resolve(&self, call: &ToolCall, description: &str) -> bool {
        match &self.handler {
            Some(handler) => handler.resolve(call, description).await,
            None => false,
        }
    }

    pub fn should_fail_on_ask(&self) -> bool {
        self.fail_on_ask
    }

    pub fn mode(&self) -> PermissionMode {
        self.config.mode
    }

    pub fn set_mode(&mut self, mode: PermissionMode) {
        self.config.mode = mode;
    }
}

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
}
