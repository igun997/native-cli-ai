use crate::prompt::NcaPrompt;
use crate::runner::{SessionRuntime, dispatch_question_answer, dispatch_tool_approval};
use crate::slash_commands::SLASH_COMMANDS;
use crate::tui::{DisplayBlock, TuiCmd, TuiSessionState, run_blocking, spawn_tui_bridge};
use nca_common::config::PermissionMode;
use nca_common::event::{EndReason, QuestionSelection};
use nca_core::skills::SkillCatalog;
use nca_runtime::memory_store::MemoryStore;
use reedline::{Completer, Emacs, FileBackedHistory, Reedline, Signal, Suggestion, Vi};
use std::io::Write;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use tokio::process::Command;

/// Where slash-command and preset output goes (TTY transcript vs full-screen TUI).
pub(crate) enum ReplOutput<'a> {
    Stdio,
    Tui(&'a Arc<Mutex<TuiSessionState>>),
}

impl ReplOutput<'_> {
    fn print(&self, s: &str) {
        match self {
            ReplOutput::Stdio => {
                print!("{s}");
                let _ = std::io::stdout().flush();
            }
            ReplOutput::Tui(st) => {
                if let Ok(mut g) = st.lock() {
                    for line in s.split('\n') {
                        g.blocks.push(DisplayBlock::System(line.to_string()));
                    }
                }
            }
        }
    }

    fn println(&self, s: &str) {
        self.print(&format!("{s}\n"));
    }

    fn eprintln(&self, s: &str) {
        match self {
            ReplOutput::Stdio => eprintln!("{s}"),
            ReplOutput::Tui(st) => {
                if let Ok(mut g) = st.lock() {
                    g.blocks.push(DisplayBlock::System(format!("[!] {s}")));
                }
            }
        }
    }

    fn clear_screen(&self) {
        match self {
            ReplOutput::Stdio => {
                print!("\x1B[2J\x1B[H");
                std::io::stdout().flush().ok();
            }
            ReplOutput::Tui(st) => {
                if let Ok(mut g) = st.lock() {
                    g.blocks.clear();
                    g.streaming_assistant = None;
                    g.scroll_lines = 0;
                }
            }
        }
    }
}

/// Special input prefixes
const INPUT_PREFIXES: &[&str] = &[
    "!",  // Bash mode - run shell command directly
    "@",  // File reference - fuzzy file search
    "\\", // Multiline continuation
];

/// Agent profiles inspired by OpenCode's multi-agent system.
/// Each profile modifies behavior and system prompt emphasis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AgentProfile {
    /// Default full-access agent for development work
    #[default]
    Build,
    /// Read-only agent for analysis and planning - denies edits
    Plan,
    /// Focused code review agent
    Review,
    /// Bug diagnosis and fix agent
    Fix,
    /// Testing and validation agent
    Test,
}

impl AgentProfile {
    /// Get the display name for this profile (shown in prompt)
    pub fn label(&self) -> &'static str {
        match self {
            AgentProfile::Build => "build",
            AgentProfile::Plan => "plan",
            AgentProfile::Review => "review",
            AgentProfile::Fix => "fix",
            AgentProfile::Test => "test",
        }
    }

    /// Get system prompt modifier for this profile
    pub fn system_modifier(&self) -> &'static str {
        match self {
            AgentProfile::Build => "",
            AgentProfile::Plan => {
                "Profile: PLAN MODE (read-only)\n- You must not modify files or run shell commands.\n\
                 - Inspect, search, read, research the web, and propose the next steps only.\n\
                 - If asked to change code, explain what would change instead of claiming it was done."
            }
            AgentProfile::Review => {
                "Profile: REVIEW MODE\n- Focus on identifying bugs, regressions, security issues, and code quality problems.\n\
                 - Check for missing tests, edge cases, and error handling.\n\
                 - Be specific about severity: critical, major, minor, or suggestion."
            }
            AgentProfile::Fix => {
                "Profile: FIX MODE\n- Diagnose the issue thoroughly before making changes.\n\
                 - Prefer minimal, verified fixes over broad rewrites.\n\
                 - Always explain the root cause and the fix."
            }
            AgentProfile::Test => {
                "Profile: TEST MODE\n- Focus on validating code correctness and edge cases.\n\
                 - Run tests, checks, or lints when tools allow.\n\
                 - Report clearly what passed, what failed, and any issues found."
            }
        }
    }

    /// Get reedline suggestion color for this profile
    pub fn style(&self) -> &'static str {
        match self {
            AgentProfile::Build => "",
            AgentProfile::Plan => "cyan",
            AgentProfile::Review => "yellow",
            AgentProfile::Fix => "red",
            AgentProfile::Test => "green",
        }
    }

    /// Cycle to the next profile (for Tab switching)
    pub fn next(self) -> Self {
        match self {
            AgentProfile::Build => AgentProfile::Plan,
            AgentProfile::Plan => AgentProfile::Review,
            AgentProfile::Review => AgentProfile::Fix,
            AgentProfile::Fix => AgentProfile::Test,
            AgentProfile::Test => AgentProfile::Build,
        }
    }

    /// All profiles in cycle order
    pub const ALL: [Self; 5] = [Self::Build, Self::Plan, Self::Review, Self::Fix, Self::Test];
}

impl std::fmt::Display for AgentProfile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.label())
    }
}

/// Session state for REPL
pub struct Repl {
    runtime: SessionRuntime,
    prompt: NcaPrompt,
    run_mode: bool,
    history_path: std::path::PathBuf,
    agent_profile: AgentProfile,
    current_agent_label: String,
}

impl Repl {
    pub fn new(runtime: SessionRuntime, safe_mode: bool, run_mode: bool) -> Self {
        let history_path = runtime.workspace_root().join(".nca/.history");
        let agent_profile = AgentProfile::default();
        let current_agent_label = format!("@{}", agent_profile.label());
        Self {
            runtime,
            prompt: NcaPrompt::new(safe_mode, run_mode),
            run_mode,
            history_path,
            agent_profile,
            current_agent_label,
        }
    }

    /// Run the interactive REPL until the user exits.
    pub async fn run(&mut self) -> anyhow::Result<()> {
        let mut editor = self.build_editor()?;

        // Start orchestration consumer so orchestrate_team tool works in REPL
        let _orch_task = if let Some(orch_rx) = self.runtime.take_orch_rx() {
            Some(nca_runtime::supervisor::orchestration_consumer(
                orch_rx,
                self.runtime.config().clone(),
                self.runtime.workspace_root().to_path_buf(),
                None,
            ))
        } else {
            None
        };

        if self.run_mode {
            self.print_banner();
        }

        loop {
            // Update prompt with current agent profile
            self.prompt.set_agent(&self.current_agent_label);
            let sig = editor.read_line(&self.prompt);
            match sig {
                Ok(Signal::Success(input)) => {
                    if input.is_empty() {
                        continue;
                    }

                    // Tab switches agent profile (OpenCode-style)
                    if input == "\t" {
                        self.switch_agent();
                        continue;
                    }

                    // Bash mode: ! prefix runs shell command directly
                    if input.starts_with('!') {
                        let cmd = input.trim_start_matches('!');
                        self.run_bash_command(cmd).await;
                        continue;
                    }

                    // File reference: @ prefix for fuzzy file search
                    if input.starts_with('@') {
                        let query = input.trim_start_matches('@');
                        self.handle_file_reference(query).await;
                        continue;
                    }

                    // Slash commands
                    if input.starts_with('/') {
                        if !self.handle_command(&input, ReplOutput::Stdio).await? {
                            break;
                        }
                        continue;
                    }

                    // Regular input to agent
                    match self.runtime.run_turn(&input).await {
                        Ok(output) => {
                            println!("{output}");
                        }
                        Err(err) => {
                            eprintln!("error: {err}");
                        }
                    }
                }
                Ok(Signal::CtrlD) => {
                    // Ctrl+D - exit
                    eprintln!("\n[exit]");
                    break;
                }
                Ok(Signal::CtrlC) => {
                    // Ctrl+C - cancel current or exit
                    eprintln!(
                        "\n[cancel] Press Ctrl+D to exit, or wait for current operation to complete"
                    );
                }
                Err(err) => {
                    eprintln!("read error: {err}");
                    break;
                }
            }
        }

        self.runtime.finish(EndReason::UserExit).await;
        Ok(())
    }

    fn print_banner(&self) {
        eprintln!(
            r#"
╔══════════════════════════════════════════════════════════════╗
║  nca - Native CLI AI                                          ║
║  Interactive terminal mode                                     ║
╠══════════════════════════════════════════════════════════════╣
║  Shortcuts:                                                   ║
║    ! <cmd>   Run shell command (bash mode)                    ║
║    @ <file>  Reference a file                                 ║
║    / <cmd>   Slash commands                                  ║
║    Tab       Switch agent profile (@build/@plan/@review...)   ║
║    Ctrl+D    Exit                                            ║
║    Ctrl+C    Cancel current request                           ║
║    Ctrl+L    Clear screen                                     ║
║    Ctrl+R    Search command history                           ║
╚══════════════════════════════════════════════════════════════╝
"#
        );
    }

    /// Switch to the next agent profile (called on Tab press)
    fn switch_agent(&mut self) {
        let next = self.agent_profile.next();
        self.agent_profile = next;
        self.current_agent_label = format!("@{}", next.label());
        self.prompt.set_agent(&self.current_agent_label);

        // Update runtime permission mode based on profile
        if next == AgentProfile::Plan {
            self.runtime.set_permission_mode(PermissionMode::Plan);
        }

        eprintln!("\n[agent] Switched to @{} mode", next.label());
        if next == AgentProfile::Plan {
            eprintln!("[agent] Plan mode: file edits and shell commands are disabled");
        }
    }

    /// Run a shell command directly (bash mode) - Claude Code style
    /// Output is returned to the conversation context
    async fn run_bash_command(&self, cmd: &str) {
        let cmd = cmd.trim();
        if cmd.is_empty() {
            eprintln!("! usage: !<command> [args]");
            return;
        }

        eprintln!("[bash] {cmd}");

        let output = Command::new("sh")
            .arg("-c")
            .arg(cmd)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await;

        match output {
            Ok(out) => {
                let stdout = String::from_utf8_lossy(&out.stdout);
                let stderr = String::from_utf8_lossy(&out.stderr);

                if !stdout.is_empty() {
                    println!("{stdout}");
                }
                if !stderr.is_empty() {
                    eprintln!("[stderr] {stderr}");
                }
                if out.status.success() {
                    eprintln!("[bash] completed (exit 0)");
                } else {
                    eprintln!("[bash] failed (exit {})", out.status.code().unwrap_or(-1));
                }
            }
            Err(e) => {
                eprintln!("[bash] failed to execute: {e}");
            }
        }
    }

    /// Handle file reference (@ prefix) - OpenCode style
    /// Performs fuzzy file search and shows matching files
    async fn handle_file_reference(&self, query: &str) {
        let query = query.trim();
        let workspace = self.runtime.workspace_root();

        eprintln!("[file] Searching for: {query}");

        // Build find command for fuzzy search
        let find_cmd = if query.is_empty() {
            format!(
                "find . -type f -name '*.rs' -o -name '*.ts' -o -name '*.js' -o -name '*.py' -o -name '*.json' 2>/dev/null | head -20"
            )
        } else {
            // Escape special characters for grep
            let escaped = query.replace(
                |c: char| !c.is_alphanumeric() && c != '.' && c != '-' && c != '_',
                "\\",
            );
            format!(
                "find . -type f \\( -name '*{escaped}*' -o -path '*{escaped}*' \\) 2>/dev/null | head -20"
            )
        };

        let output = Command::new("sh")
            .arg("-c")
            .arg(&find_cmd)
            .current_dir(workspace)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await;

        match output {
            Ok(out) => {
                let files = String::from_utf8_lossy(&out.stdout);
                if files.is_empty() {
                    eprintln!("[file] No files found matching: {query}");
                } else {
                    eprintln!("[file] Matches:");
                    for (i, line) in files.lines().enumerate() {
                        if !line.is_empty() {
                            println!("  {}: {}", i + 1, line);
                        }
                    }
                    eprintln!("\n[file] Reference files in your prompt using @<number> or @<path>");
                }
            }
            Err(e) => {
                eprintln!("[file] Search failed: {e}");
            }
        }
    }

    /// Open external editor for long prompts (Ctrl+G style)
    async fn open_external_editor(&self) -> Option<String> {
        let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vim".to_string());

        // Create a temp file
        let temp_path = std::env::temp_dir().join("nca-prompt-XXXXXX");
        let temp_path_str = temp_path.to_string_lossy().to_string();

        // Use mktemp-like approach
        let temp_file = format!("{}.txt", std::process::id());
        let temp_path = std::env::temp_dir().join(&temp_file);

        // Write current buffer if any
        std::fs::write(&temp_path, "").ok()?;

        // Spawn editor
        let output = Command::new("sh")
            .arg("-c")
            .arg(format!("{} '{}'", editor, temp_path.display()))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await;

        match output {
            Ok(_) => {
                let content = std::fs::read_to_string(&temp_path).ok()?;
                let _ = std::fs::remove_file(&temp_path);
                let content = content.trim().to_string();
                if content.is_empty() {
                    None
                } else {
                    Some(content)
                }
            }
            Err(e) => {
                eprintln!("[editor] Failed to open: {e}");
                None
            }
        }
    }

    fn build_editor(&self) -> anyhow::Result<Reedline> {
        let mut builder = Reedline::create()
            .with_quick_completions(true)
            .with_partial_completions(true)
            .with_ansi_colors(true);

        // Try to load history from disk
        if let Some(parent) = self.history_path.parent() {
            std::fs::create_dir_all(parent).ok();
            if let Ok(history) = FileBackedHistory::with_file(100, self.history_path.clone()) {
                builder = builder.with_history(Box::new(history));
            }
        }

        // Support vim mode if enabled via env
        if std::env::var("NCA_EDITOR_MODE")
            .map(|v| v.eq_ignore_ascii_case("vi") || v.eq_ignore_ascii_case("vim"))
            .unwrap_or(false)
        {
            builder = builder.with_edit_mode(Box::new(Vi::default()));
        } else {
            builder = builder.with_edit_mode(Box::new(Emacs::default()));
        }

        Ok(builder)
    }

    async fn handle_command(&mut self, input: &str, out: ReplOutput<'_>) -> anyhow::Result<bool> {
        let mut parts = input.split_whitespace();
        let command = parts.next().unwrap_or_default();
        let rest = input
            .strip_prefix(command)
            .map(str::trim)
            .unwrap_or_default();

        match command {
            "/q" | "/quit" | "/exit" => return Ok(false),
            "/help" => {
                out.print(
                    "nca Interactive Mode - Claude Code inspired shortcuts:\n\n\
                     INPUT MODES:\n\
                       ! <cmd>     Run shell command directly (output feeds back to context)\n\
                       @ <query>   Search and reference files\n\
                       / <cmd>     Slash commands\n\
                       \\          Multiline input (end line with \\ to continue)\n\n\
                     SLASH COMMANDS:\n\
                       /help                       Show this help\n\
                       /status                     Show current session status\n\
                       /agent [profile]           Show or switch agent profile\n\
                       /plan <task>               Run a planning-oriented turn\n\
                       /review <task>             Review code or changes\n\
                       /fix <task>                Run a bug-fix oriented turn\n\
                       /test <task>               Ask the agent to validate/test\n\
                       /clear                     Clear the screen\n\
                       /compact                   Save a compact session summary\n\
                       /undo                      Undo last agent response\n\
                       /redo                      Redo undone response\n\
                       /diff                      Show recent file changes\n\
                       /cost                      Show token usage and cost\n\
                       /stats                     Show session statistics\n\
                       /auto-answer               Accept suggested answer for pending ask_question\n\
                       /skills                    List discovered skills\n\
                       /memory [text]             Show or store workspace memory\n\
                       /models                    Show available models\n\
                       /mcp                       List configured MCP servers\n\
                       /agents                    Show child sessions\n\
                       /logs                      Print the current event log\n\
                       /attach                    Show current attach target\n\
                       /config                    Show effective runtime config\n\
                       /doctor                    Run MiniMax config checks\n\
                       /orchestrate <task>        Run multi-agent orchestration\n\
                       /roles [list|show <name>]  Manage agent roles\n\
                       /sessions                  List local session IDs\n\
                       /permissions [mode]        Show or set permission mode\n\
                       /permission-bypass [on|off|toggle]  Quick bypass toggle (default: toggle)\n\
                       /exit                      Exit repl\n\n\
                     KEYBOARD SHORTCUTS:\n\
                       Tab                         Switch agent profile (@build -> @plan -> @review)\n\
                       Ctrl+D                     Exit repl\n\
                       Ctrl+C                     Cancel current request\n\
                       Ctrl+L                     Clear screen\n\
                       Ctrl+R                     Search command history\n",
                );
            }
            "/status" => {
                let snapshot = self.runtime.snapshot();
                out.println(&format!(
                    "session={} model={} agent={} permission_mode={:?} children={} memory={}",
                    snapshot.id,
                    self.runtime.model(),
                    self.agent_profile.label(),
                    self.runtime.permission_mode(),
                    snapshot.child_session_ids.len(),
                    self.runtime.memory_store_path().display()
                ));
                if let Some(summary) = snapshot.session_summary {
                    out.println(&format!("summary: {}", summary.replace('\n', " ")));
                }
            }
            "/agent" => {
                if let Some(target) = parts.next() {
                    let target_clean = target.trim_start_matches('@').to_lowercase();
                    let matched = AgentProfile::ALL.iter().find(|p| {
                        p.label() == target_clean
                    });
                    if let Some(profile) = matched {
                        self.agent_profile = *profile;
                        self.current_agent_label = format!("@{}", profile.label());
                        self.prompt.set_agent(&self.current_agent_label);
                        if *profile == AgentProfile::Plan {
                            self.runtime.set_permission_mode(PermissionMode::Plan);
                        } else {
                            self.runtime.set_permission_mode(PermissionMode::Default);
                        }
                        if let ReplOutput::Tui(st) = &out {
                            if let Ok(mut g) = st.lock() {
                                g.set_agent_profile(&self.current_agent_label);
                                g.set_permission_mode(&format!(
                                    "{:?}",
                                    self.runtime.permission_mode()
                                ));
                            }
                        }
                        out.println(&format!("Switched to @{} mode", profile.label()));
                    } else {
                        out.println(&format!("Unknown agent profile: {}", target));
                        out.println(&format!(
                            "Available: {}",
                            AgentProfile::ALL
                                .iter()
                                .map(|p| p.label())
                                .collect::<Vec<_>>()
                                .join(", ")
                        ));
                    }
                } else {
                    out.println(&format!("Current agent: @{}", self.agent_profile.label()));
                    out.println("Available profiles:");
                    for profile in AgentProfile::ALL {
                        let marker = if profile == self.agent_profile { " *" } else { "" };
                        out.println(&format!("  @{}{}", profile.label(), marker));
                    }
                }
            }
            "/plan" => {
                self.run_preset(
                    "Create a short implementation plan before coding. Focus on steps, risks, and validation.\n\nTask:\n",
                    rest,
                    out,
                )
                .await?
            }
            "/review" => {
                self.run_preset(
                    "Review the requested code or changes. Prioritize bugs, regressions, risks, and missing tests.\n\nReview target:\n",
                    rest,
                    out,
                )
                .await?
            }
            "/fix" => {
                self.run_preset(
                    "Diagnose and fix the issue below. Prefer a minimal verified change.\n\nIssue:\n",
                    rest,
                    out,
                )
                .await?
            }
            "/test" => {
                self.run_preset(
                    "Validate the requested area. Run tests or checks if tools allow, and report what passed or failed.\n\nTarget:\n",
                    rest,
                    out,
                )
                .await?
            }
            "/model" => {
                if let Some(model) = parts.next() {
                    let resolved = self.runtime.config().model.resolve_alias(model);
                    self.runtime.set_model(resolved.clone());
                    if let ReplOutput::Tui(st) = out {
                        if let Ok(mut g) = st.lock() {
                            g.model = resolved.clone();
                        }
                    }
                    out.println(&format!("model set to {resolved}"));
                } else {
                    out.println(&format!("model: {}", self.runtime.model()));
                }
            }
            "/clear" => {
                out.clear_screen();
                out.println("[screen cleared]");
            }
            "/undo" => {
                out.eprintln("[undo] Not yet implemented - use /compact to save session state");
            }
            "/redo" => {
                out.eprintln("[redo] Not yet implemented");
            }
            "/diff" => {
                // Show recent file changes via git
                let output = Command::new("sh")
                    .arg("-c")
                    .arg("git diff --stat HEAD~5..HEAD 2>/dev/null || git diff --stat 2>/dev/null || echo 'No git changes'")
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .output()
                    .await;
                match output {
                    Ok(cmd_out) => {
                        let diff = String::from_utf8_lossy(&cmd_out.stdout);
                        if diff.is_empty() {
                            out.println("[diff] No recent changes");
                        } else {
                            out.print(&diff);
                        }
                    }
                    Err(e) => out.eprintln(&format!("[diff] Failed: {e}")),
                }
            }
            "/cost" => {
                let snapshot = self.runtime.snapshot();
                out.eprintln(&format!("[cost] Session: {}", snapshot.id));
                out.eprintln("[cost] Use 'nca logs --follow' to see real-time token usage");
            }
            "/stats" => {
                let snapshot = self.runtime.snapshot();
                out.println(&format!("session_id: {}", snapshot.id));
                out.println(&format!("model: {}", self.runtime.model()));
                out.println(&format!("agent: @{}", self.agent_profile.label()));
                out.println(&format!(
                    "permission_mode: {:?}",
                    self.runtime.permission_mode()
                ));
                out.println(&format!(
                    "child_sessions: {}",
                    snapshot.child_session_ids.len()
                ));
                out.println(&format!(
                    "memory_path: {}",
                    self.runtime.memory_store_path().display()
                ));
            }
            "/permissions" => {
                if let Some(mode) = parts.next() {
                    if let Some(parsed_mode) = parse_permission_mode(mode) {
                        self.runtime.set_permission_mode(parsed_mode);
                        if let ReplOutput::Tui(st) = out {
                            if let Ok(mut g) = st.lock() {
                                g.set_permission_mode(&format!("{parsed_mode:?}"));
                            }
                        }
                        out.println(&format!("permission mode set to {parsed_mode:?}"));
                    } else {
                        out.println(
                            "invalid mode; expected one of: default, plan, accept-edits, dont-ask, bypass-permissions",
                        );
                    }
                } else {
                    out.println(&format!(
                        "permission_mode: {:?}",
                        self.runtime.permission_mode()
                    ));
                }
            }
            "/permission-bypass" => {
                let sub = parts.next().unwrap_or("").trim();
                let target = match sub.to_ascii_lowercase().as_str() {
                    "" | "toggle" => {
                        if self.runtime.permission_mode() == PermissionMode::BypassPermissions {
                            PermissionMode::Default
                        } else {
                            PermissionMode::BypassPermissions
                        }
                    }
                    "on" | "enable" | "yes" | "1" => PermissionMode::BypassPermissions,
                    "off" | "disable" | "no" | "0" => PermissionMode::Default,
                    _ => {
                        out.println(
                            "usage: /permission-bypass [on|off|toggle] — default toggles bypass ↔ default",
                        );
                        return Ok(true);
                    }
                };
                self.runtime.set_permission_mode(target);
                if let ReplOutput::Tui(st) = out {
                    if let Ok(mut g) = st.lock() {
                        g.set_permission_mode(&format!("{target:?}"));
                    }
                }
                out.println(&format!("permission mode set to {target:?}"));
            }
            "/skills" => {
                let skills = SkillCatalog::discover(
                    self.runtime.workspace_root(),
                    &self.runtime.config().harness.skill_directories,
                )
                .map_err(anyhow::Error::msg)?;
                if skills.is_empty() {
                    out.println("no skills discovered");
                } else {
                    for skill in skills {
                        out.println(&skill.summary_line());
                    }
                }
            }
            "/memory" => {
                if rest.is_empty() {
                    let store = MemoryStore::new(self.runtime.memory_store_path());
                    let mem = store.load().await.map_err(anyhow::Error::msg)?;
                    if mem.notes.is_empty() {
                        out.println("no memory notes stored");
                    } else {
                        for note in mem.notes.iter().rev().take(5) {
                            out.println(&format!(
                                "{} {} {}",
                                note.id,
                                note.kind,
                                note.content.replace('\n', " ")
                            ));
                        }
                    }
                } else {
                    self.runtime
                        .append_memory_note("note", Some(rest.to_string()))
                        .await
                        .map_err(anyhow::Error::msg)?;
                    out.println("memory note saved");
                }
            }
            "/compact" => {
                let summary = self.runtime.compact_summary();
                self.runtime.set_session_summary(Some(summary.clone()));
                self.runtime
                    .append_memory_note("session-summary", Some(summary.clone()))
                    .await
                    .map_err(anyhow::Error::msg)?;
                self.runtime.save().await.map_err(anyhow::Error::msg)?;
                out.println(&format!("saved session summary:\n{}", summary));
            }
            "/models" => {
                let provider = self.runtime.config().provider.default;
                out.println(&format!(
                    "default_provider={} default_model={} thinking={} budget={}",
                    provider.display_name(),
                    self.runtime.config().model.default_model,
                    self.runtime.config().model.enable_thinking,
                    self.runtime.config().model.thinking_budget
                ));
                for provider in nca_common::config::ProviderKind::ALL {
                    out.println(&format!(
                        "  {} -> {} ({})",
                        provider.display_name(),
                        self.runtime.config().provider.model_for(provider),
                        self.runtime.config().provider.base_url_for(provider)
                    ));
                }
                for (alias, target) in &self.runtime.config().model.aliases {
                    out.println(&format!("  {alias} -> {target}"));
                }
            }
            "/mcp" => {
                if self.runtime.config().mcp.servers.is_empty() {
                    out.println("no MCP servers configured");
                } else {
                    for server in self
                        .runtime
                        .config()
                        .mcp
                        .servers
                        .iter()
                        .filter(|server| server.enabled)
                    {
                        out.println(&format!(
                            "{} command={} {}",
                            server.name,
                            server.command,
                            server.args.join(" ")
                        ));
                    }
                }
            }
            "/agents" => {
                let snapshot = self.runtime.snapshot();
                if snapshot.child_session_ids.is_empty() {
                    out.println("no child sessions yet");
                } else {
                    for child in snapshot.child_session_ids {
                        out.println(&child);
                    }
                }
            }
            "/logs" => {
                match tokio::fs::read_to_string(self.runtime.event_log_path()).await {
                    Ok(data) => out.print(&data),
                    Err(err) => {
                        out.eprintln(&format!("failed to read log: {err}"))
                    }
                }
            }
            "/attach" => {
                let snapshot = self.runtime.snapshot();
                out.println(&format!(
                    "session={} socket={}",
                    snapshot.id,
                    snapshot
                        .socket_path
                        .as_ref()
                        .map(|path| path.display().to_string())
                        .unwrap_or_else(|| "<none>".into())
                ));
            }
            "/config" => {
                let config = self.runtime.config();
                out.println(&format!(
                    "provider={:?} model={} permission_mode={:?} memory={}",
                    config.provider.default,
                    self.runtime.model(),
                    self.runtime.permission_mode(),
                    self.runtime.memory_store_path().display()
                ));
            }
            "/doctor" => {
                for provider in nca_common::config::ProviderKind::ALL {
                    let configured = self
                        .runtime
                        .config()
                        .provider
                        .api_key_present_for(provider);
                    out.println(&format!(
                        "{}{} API key {} ({})",
                        provider.display_name(),
                        if provider == self.runtime.config().provider.default {
                            " [selected]"
                        } else {
                            ""
                        },
                        if configured { "configured" } else { "missing" },
                        self.runtime.config().provider.api_key_env_for(provider)
                    ));
                }
            }
            "/auto-answer" => {
                let from_tui = if let ReplOutput::Tui(st) = &out {
                    st.lock()
                        .ok()
                        .and_then(|g| g.active_question.as_ref().map(|q| q.question_id.clone()))
                } else {
                    None
                };
                let ok = if let Some(qid) = from_tui {
                    self.runtime
                        .submit_question_answer(&qid, QuestionSelection::Suggested)
                } else {
                    self.runtime.submit_suggested_answer()
                };
                if ok {
                    out.println("accepted suggested answer for pending question");
                } else {
                    out.eprintln(
                        "no pending interactive question to auto-answer (use when ask_question is waiting)",
                    );
                }
            }
            "/sessions" => match self.runtime.list_session_ids().await {
                Ok(mut ids) => {
                    ids.sort();
                    if ids.is_empty() {
                        out.println("no saved sessions");
                    } else {
                        for id in ids {
                            out.println(&id);
                        }
                    }
                }
                Err(error) => {
                    out.eprintln(&format!("failed to list sessions: {error}"));
                }
            },
            "/orchestrate" => {
                if rest.is_empty() {
                    out.eprintln("Usage: /orchestrate <task description> [--agents \"hints\"]");
                } else {
                    // Parse optional --agents flag
                    let (prompt, agents) = if let Some(idx) = rest.find("--agents") {
                        let prompt = rest[..idx].trim().to_string();
                        let agents_str = rest[idx + 8..].trim().trim_matches('"').to_string();
                        (prompt, Some(agents_str))
                    } else {
                        (rest.to_string(), None)
                    };

                    out.println(&format!("Starting multi-agent orchestration..."));
                    out.println(&format!("Task: {prompt}"));
                    if let Some(ref hints) = agents {
                        out.println(&format!("Agents: {hints}"));
                    }

                    let config = self.runtime.config().clone();
                    let workspace = self.runtime.workspace_root().to_path_buf();

                    match nca_runtime::team_orchestrator::TeamOrchestrator::start(
                        config, workspace, prompt, agents,
                    ).await {
                        Ok(handle) => {
                            // Stream events live into TUI or stdout
                            let mut event_rx = handle.subscribe();
                            let tui_state_for_events: Option<Arc<Mutex<TuiSessionState>>> =
                                if let ReplOutput::Tui(st) = &out {
                                    Some((*st).clone())
                                } else {
                                    None
                                };
                            let event_task = tokio::spawn(async move {
                                while let Ok(event) = event_rx.recv().await {
                                    let ts = event.timestamp.format("%H:%M:%S");
                                    let line = format!("[{ts}] [{}] {:?}", event.source_agent, event.event);
                                    if let Some(ref st) = tui_state_for_events {
                                        if let Ok(mut g) = st.lock() {
                                            g.blocks.push(DisplayBlock::System(line));
                                        }
                                    } else {
                                        println!("{line}");
                                    }
                                }
                            });

                            match handle.wait().await {
                                Ok(result) => {
                                    event_task.abort();
                                    out.println("\n--- Orchestration Complete ---");
                                    out.println(&format!("Outcome: {:?}", result.outcome));
                                    out.println(&format!("Cost: ${:.4}", result.cost.total_usd));
                                    if let Some(branch) = &result.merge_branch {
                                        out.println(&format!("Merge branch: {branch}"));
                                    }
                                    out.println("\nAgent Reports:");
                                    for report in &result.agent_reports {
                                        out.println(&format!(
                                            "  {} ({}): {:?}",
                                            report.name, report.role, report.status
                                        ));
                                        if let Some(r) = &report.completion_report {
                                            let preview = if r.len() > 500 { &r[..500] } else { r };
                                            out.println(&format!("    {}", preview.replace('\n', "\n    ")));
                                        }
                                    }
                                }
                                Err(e) => {
                                    event_task.abort();
                                    out.eprintln(&format!("Orchestration failed: {e}"));
                                }
                            }
                        }
                        Err(e) => out.eprintln(&format!("Failed to start orchestration: {e}")),
                    }
                }
            }
            "/roles" => {
                let sub = parts.next().unwrap_or("list");
                match sub {
                    "list" | "" => {
                        let catalog = nca_runtime::role_catalog::RoleCatalog::load(
                            self.runtime.workspace_root(),
                        );
                        out.println("Available roles:\n");
                        for role in catalog.list() {
                            out.println(&format!(
                                "  {} — {} (mode: {:?}, turns: {}, tools: {})",
                                role.name, role.description,
                                role.permission_mode, role.max_turns, role.max_tool_calls
                            ));
                        }
                    }
                    "show" => {
                        let name = parts.next().unwrap_or("");
                        if name.is_empty() {
                            out.eprintln("Usage: /roles show <name>");
                        } else {
                            let catalog = nca_runtime::role_catalog::RoleCatalog::load(
                                self.runtime.workspace_root(),
                            );
                            match catalog.get(name) {
                                Some(role) => {
                                    out.println(&format!("Role: {}", role.name));
                                    out.println(&format!("Description: {}", role.description));
                                    out.println(&format!("Mode: {:?}", role.permission_mode));
                                    out.println(&format!("Max turns: {}", role.max_turns));
                                    out.println(&format!("Max tool calls: {}", role.max_tool_calls));
                                    if !role.allowed_tools.is_empty() {
                                        out.println(&format!("Allowed: {}", role.allowed_tools.join(", ")));
                                    }
                                    if !role.denied_tools.is_empty() {
                                        out.println(&format!("Denied: {}", role.denied_tools.join(", ")));
                                    }
                                }
                                None => out.eprintln(&format!("Role '{name}' not found")),
                            }
                        }
                    }
                    other => out.eprintln(&format!("Unknown roles subcommand: {other}. Use: list, show")),
                }
            }
            _ => {
                if command.starts_with('/') {
                    if self
                        .try_run_skill(command.trim_start_matches('/'), rest, &out)
                        .await?
                    {
                        return Ok(true);
                    }
                }
                out.eprintln(&format!("unknown command: {command}"));
            }
        }

        Ok(true)
    }

    async fn run_preset(
        &mut self,
        prefix: &str,
        task: &str,
        out: ReplOutput<'_>,
    ) -> anyhow::Result<()> {
        if task.trim().is_empty() {
            out.println("usage: /<command> <task description>");
            return Ok(());
        }
        let prompt = format!("{prefix}{}", task.trim());
        match self.runtime.run_turn(&prompt).await {
            Ok(output) => {
                if matches!(out, ReplOutput::Stdio) {
                    out.println(&output);
                }
            }
            Err(err) => {
                out.eprintln(&format!("error: {err}"));
            }
        }
        Ok(())
    }

    async fn try_run_skill(
        &mut self,
        skill_name: &str,
        task: &str,
        out: &ReplOutput<'_>,
    ) -> anyhow::Result<bool> {
        let skills = SkillCatalog::discover(
            self.runtime.workspace_root(),
            &self.runtime.config().harness.skill_directories,
        )
        .map_err(anyhow::Error::msg)?;
        let Some(skill) = skills.into_iter().find(|skill| skill.command == skill_name) else {
            return Ok(false);
        };

        if let Some(model) = &skill.model {
            self.runtime
                .set_model(self.runtime.config().model.resolve_alias(model));
        }
        if let Some(mode) = skill.permission_mode {
            self.runtime.set_permission_mode(mode);
        }

        let prompt = skill.prompt_for_task(task);
        match self.runtime.run_turn(&prompt).await {
            Ok(output) => {
                if matches!(out, ReplOutput::Stdio) {
                    out.println(&output);
                }
            }
            Err(err) => {
                out.eprintln(&format!("error: {err}"));
            }
        }
        Ok(true)
    }

    /// Full-screen TUI: transcript + streaming + composer (default on TTY).
    pub async fn run_with_tui(&mut self) -> anyhow::Result<()> {
        // Start orchestration consumer so orchestrate_team tool works in TUI
        let _orch_task = if let Some(orch_rx) = self.runtime.take_orch_rx() {
            Some(nca_runtime::supervisor::orchestration_consumer(
                orch_rx,
                self.runtime.config().clone(),
                self.runtime.workspace_root().to_path_buf(),
                None,
            ))
        } else {
            None
        };

        let session_id = self.runtime.session_id().to_string();
        let model = self.runtime.model().to_string();
        let perm = format!("{:?}", self.runtime.permission_mode());
        let tui_state: Arc<Mutex<TuiSessionState>> = Arc::new(Mutex::new(TuiSessionState::new(
            session_id,
            model,
            self.current_agent_label.clone(),
            perm,
        )));

        let rx = self
            .runtime
            .take_event_rx()
            .ok_or_else(|| anyhow::anyhow!("internal: event channel already taken"))?;
        let log_path = self.runtime.event_log_path();
        let ipc = self.runtime.take_ipc_handle();
        let approval = self.runtime.take_ipc_approval_pending();
        let question = self.runtime.question_pending();
        let _bridge = spawn_tui_bridge(
            rx,
            log_path,
            ipc,
            approval.clone(),
            question.clone(),
            tui_state.clone(),
        );

        // Answers must bypass the main `cmd_rx` loop: while `run_turn` is blocked inside
        // `ask_question`, that task never receives `TuiCmd::Submit` or `QuestionAnswer`.
        let (answer_tx, mut answer_rx) =
            tokio::sync::mpsc::unbounded_channel::<(String, QuestionSelection)>();
        let qp_dispatch = question.clone();
        tokio::spawn(async move {
            while let Some((qid, sel)) = answer_rx.recv().await {
                let _ = dispatch_question_answer(&qp_dispatch, &qid, sel);
            }
        });
        let answer_for_tui = answer_tx.clone();
        drop(answer_tx);

        let (approval_tx, mut approval_rx) =
            tokio::sync::mpsc::unbounded_channel::<(String, bool)>();
        let approval_dispatch = approval.clone();
        let approval_state = tui_state.clone();
        tokio::spawn(async move {
            while let Some((call_id, approved)) = approval_rx.recv().await {
                if !dispatch_tool_approval(&approval_dispatch, &call_id, approved) {
                    if let Ok(mut g) = approval_state.lock() {
                        g.push_error(
                            "failed to resolve approval (expired or already handled)".into(),
                        );
                    }
                }
            }
        });
        let approval_for_tui = approval_tx.clone();
        drop(approval_tx);

        let (cmd_tx, mut cmd_rx) = tokio::sync::mpsc::unbounded_channel::<TuiCmd>();
        let st = tui_state.clone();
        let banner = self.run_mode;
        let ui = tokio::task::spawn_blocking(move || {
            run_blocking(
                st,
                cmd_tx,
                Some(answer_for_tui),
                Some(approval_for_tui),
                banner,
            )
        });

        loop {
            let cmd = cmd_rx.recv().await;
            let Some(cmd) = cmd else { break };
            match cmd {
                TuiCmd::Exit => {
                    if let Ok(mut g) = tui_state.lock() {
                        g.should_exit = true;
                    }
                    break;
                }
                TuiCmd::CycleAgent => {
                    let next = self.agent_profile.next();
                    self.agent_profile = next;
                    self.current_agent_label = format!("@{}", next.label());
                    if next == AgentProfile::Plan {
                        self.runtime.set_permission_mode(PermissionMode::Plan);
                    } else {
                        self.runtime.set_permission_mode(PermissionMode::Default);
                    }
                    if let Ok(mut g) = tui_state.lock() {
                        g.set_agent_profile(&self.current_agent_label);
                        g.set_permission_mode(&format!("{:?}", self.runtime.permission_mode()));
                    }
                }
                TuiCmd::CancelTurn => {
                    self.runtime.request_cancel();
                }
                TuiCmd::QuestionAnswer(selection) => {
                    let qid = if let Ok(g) = tui_state.lock() {
                        g.active_question.as_ref().map(|q| q.question_id.clone())
                    } else {
                        None
                    };
                    if let Some(qid) = qid {
                        if !self.runtime.submit_question_answer(&qid, selection) {
                            if let Ok(mut g) = tui_state.lock() {
                                g.push_error(
                                    "failed to submit answer (expired or already answered)".into(),
                                );
                            }
                        }
                    }
                }
                TuiCmd::Submit(line) => {
                    let line = line.trim().to_string();
                    if line.is_empty() {
                        continue;
                    }
                    if line.starts_with('!') {
                        let shell_cmd = line.trim_start_matches('!').trim();
                        self.run_bash_tui(shell_cmd, &tui_state).await;
                        continue;
                    }
                    if line.starts_with('@') {
                        let q = line.trim_start_matches('@');
                        self.file_ref_tui(q, &tui_state).await;
                        continue;
                    }
                    if line.starts_with('/') {
                        if !self
                            .handle_command(&line, ReplOutput::Tui(&tui_state))
                            .await?
                        {
                            if let Ok(mut g) = tui_state.lock() {
                                g.should_exit = true;
                            }
                            break;
                        }
                        continue;
                    }
                    if let Ok(mut g) = tui_state.lock() {
                        g.set_busy(true);
                    }
                    if let Err(e) = self.runtime.run_turn(&line).await {
                        if let Ok(mut g) = tui_state.lock() {
                            g.push_error(e.to_string());
                        }
                    }
                    if let Ok(mut g) = tui_state.lock() {
                        g.set_busy(false);
                    }
                }
            }
        }

        let _ = ui.await;
        self.runtime.finish(EndReason::UserExit).await;
        Ok(())
    }

    async fn run_bash_tui(&self, cmd: &str, st: &Arc<Mutex<TuiSessionState>>) {
        fn log(st: &Arc<Mutex<TuiSessionState>>, s: &str) {
            if let Ok(mut g) = st.lock() {
                g.blocks.push(DisplayBlock::System(s.to_string()));
            }
        }
        if cmd.is_empty() {
            log(st, "! usage: !<command>");
            return;
        }
        log(st, &format!("[bash] {cmd}"));
        let output = Command::new("sh")
            .arg("-c")
            .arg(cmd)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await;
        match output {
            Ok(out) => {
                let stdout = String::from_utf8_lossy(&out.stdout);
                let stderr = String::from_utf8_lossy(&out.stderr);
                if !stdout.is_empty() {
                    if let Ok(mut g) = st.lock() {
                        for line in stdout.lines() {
                            g.blocks.push(DisplayBlock::System(line.to_string()));
                        }
                    }
                }
                if !stderr.is_empty() {
                    log(st, &format!("[stderr] {stderr}"));
                }
                log(
                    st,
                    &if out.status.success() {
                        "[bash] exit 0".into()
                    } else {
                        format!("[bash] exit {}", out.status.code().unwrap_or(-1))
                    },
                );
            }
            Err(e) => log(st, &format!("[bash] {e}")),
        }
    }

    async fn file_ref_tui(&self, query: &str, st: &Arc<Mutex<TuiSessionState>>) {
        fn log(st: &Arc<Mutex<TuiSessionState>>, s: &str) {
            if let Ok(mut g) = st.lock() {
                g.blocks.push(DisplayBlock::System(s.to_string()));
            }
        }
        let query = query.trim();
        let workspace = self.runtime.workspace_root();
        log(st, &format!("[file] search: {query}"));
        let find_cmd = if query.is_empty() {
            "find . -type f \\( -name '*.rs' -o -name '*.ts' -o -name '*.toml' \\) 2>/dev/null | head -20"
                .to_string()
        } else {
            let escaped = query.replace(
                |c: char| !c.is_alphanumeric() && c != '.' && c != '-' && c != '_',
                "\\",
            );
            format!(
                "find . -type f \\( -name '*{escaped}*' -o -path '*{escaped}*' \\) 2>/dev/null | head -20"
            )
        };
        let output = Command::new("sh")
            .arg("-c")
            .arg(&find_cmd)
            .current_dir(workspace)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await;
        match output {
            Ok(out) => {
                let files = String::from_utf8_lossy(&out.stdout);
                if files.trim().is_empty() {
                    log(st, "[file] no matches");
                } else {
                    for (i, line) in files.lines().enumerate() {
                        if !line.is_empty() {
                            log(st, &format!("  {}: {}", i + 1, line));
                        }
                    }
                    log(st, "[file] reference with @<path> in your next message");
                }
            }
            Err(e) => log(st, &format!("[file] {e}")),
        }
    }
}

/// Tab completion for REPL commands and skills
impl Completer for Repl {
    fn complete(&mut self, line: &str, _pos: usize) -> Vec<Suggestion> {
        let mut suggestions = Vec::new();

        // Complete REPL commands starting with /
        if line.starts_with('/') {
            for cmd in SLASH_COMMANDS {
                if cmd.starts_with(line) {
                    suggestions.push(Suggestion {
                        value: cmd.to_string(),
                        description: Some("REPL command".to_string()),
                        extra: None,
                        span: reedline::Span { start: 0, end: 0 },
                        append_whitespace: true,
                        style: None,
                    });
                }
            }
        }

        // Complete bash mode commands (starting with !)
        if line.starts_with('!') {
            // Common shell commands
            let bash_commands = [
                "git", "ls", "cat", "find", "grep", "npm", "cargo", "make", "docker", "curl",
            ];
            let prefix = line.trim_start_matches('!');
            for cmd in bash_commands {
                let full = format!("!{}", cmd);
                if full.starts_with(line) {
                    suggestions.push(Suggestion {
                        value: full,
                        description: Some("Shell command".to_string()),
                        extra: None,
                        span: reedline::Span { start: 0, end: 0 },
                        append_whitespace: true,
                        style: None,
                    });
                }
            }
        }

        // Complete file references (starting with @)
        if line.starts_with('@') {
            let prefix = line.trim_start_matches('@');
            // Suggest some common file patterns
            let patterns = [
                "src/",
                "lib/",
                "tests/",
                "docs/",
                "Cargo.toml",
                "package.json",
                "README.md",
            ];
            for pat in patterns {
                if pat.starts_with(prefix) {
                    suggestions.push(Suggestion {
                        value: format!("@{}", pat),
                        description: Some("File reference".to_string()),
                        extra: None,
                        span: reedline::Span { start: 0, end: 0 },
                        append_whitespace: true,
                        style: None,
                    });
                }
            }
        }

        // Load skills for completion
        if let Ok(skills) = SkillCatalog::discover(
            self.runtime.workspace_root(),
            &self.runtime.config().harness.skill_directories,
        ) {
            for skill in skills {
                let skill_cmd = format!("/{}", skill.command);
                if skill_cmd.starts_with(line) {
                    suggestions.push(Suggestion {
                        value: skill_cmd,
                        description: skill.description,
                        extra: None,
                        span: reedline::Span { start: 0, end: 0 },
                        append_whitespace: true,
                        style: None,
                    });
                }
            }
        }

        suggestions
    }
}

fn parse_permission_mode(raw: &str) -> Option<PermissionMode> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "default" => Some(PermissionMode::Default),
        "plan" => Some(PermissionMode::Plan),
        "accept-edits" | "accept_edits" | "acceptedits" => Some(PermissionMode::AcceptEdits),
        "dont-ask" | "dont_ask" | "dontask" => Some(PermissionMode::DontAsk),
        "bypass-permissions" | "bypass_permissions" | "bypasspermissions" => {
            Some(PermissionMode::BypassPermissions)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_permission_aliases() {
        assert_eq!(
            parse_permission_mode("accept-edits"),
            Some(PermissionMode::AcceptEdits)
        );
        assert_eq!(
            parse_permission_mode("dontask"),
            Some(PermissionMode::DontAsk)
        );
        assert_eq!(
            parse_permission_mode("bypass_permissions"),
            Some(PermissionMode::BypassPermissions)
        );
        assert_eq!(parse_permission_mode("invalid"), None);
    }
}
