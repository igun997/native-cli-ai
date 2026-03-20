use nca_common::config::PermissionMode;
use nca_common::role::RoleFile;
use nca_common::team::RoleDefinition;
use std::collections::HashMap;
use std::path::Path;

pub struct RoleCatalog {
    roles: HashMap<String, RoleDefinition>,
}

impl RoleCatalog {
    /// Load roles: project-level > global > built-in
    pub fn load(workspace_root: &Path) -> Self {
        let mut roles = HashMap::new();

        // 1. Built-in roles (lowest priority)
        for role in built_in_roles() {
            roles.insert(role.name.clone(), role);
        }

        // 2. Global roles (~/.nca/roles/*.toml)
        if let Some(home) = dirs::home_dir() {
            let global_dir = home.join(".nca").join("roles");
            load_toml_roles(&global_dir, &mut roles);
        }

        // 3. Project-level roles (highest priority)
        let project_dir = workspace_root.join(".nca").join("roles");
        load_toml_roles(&project_dir, &mut roles);

        Self { roles }
    }

    pub fn get(&self, name: &str) -> Option<&RoleDefinition> {
        self.roles.get(name)
    }

    pub fn list(&self) -> Vec<&RoleDefinition> {
        self.roles.values().collect()
    }

    pub fn names(&self) -> Vec<&str> {
        self.roles.keys().map(|s| s.as_str()).collect()
    }
}

fn load_toml_roles(dir: &Path, roles: &mut HashMap<String, RoleDefinition>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().map_or(false, |e| e == "toml") {
            if let Ok(content) = std::fs::read_to_string(&path) {
                if let Ok(role_file) = toml::from_str::<RoleFile>(&content) {
                    let def = role_file.into_definition();
                    roles.insert(def.name.clone(), def);
                }
            }
        }
    }
}

fn built_in_roles() -> Vec<RoleDefinition> {
    vec![
        RoleDefinition {
            name: "researcher".into(),
            description: "Analyzes codebase, finds root causes, produces findings reports".into(),
            system_prompt: "You are a code researcher. Your job is to analyze, search, and \
                            understand the codebase. You produce detailed findings reports, not \
                            code changes. Be thorough and cite file:line references."
                .into(),
            allowed_tools: vec![
                "read_file".into(),
                "search_code".into(),
                "list_directory".into(),
                "git_status".into(),
                "git_diff".into(),
                "web_search".into(),
                "fetch_url".into(),
            ],
            denied_tools: vec![
                "write_file".into(),
                "edit_file".into(),
                "execute_bash".into(),
                "delete_path".into(),
            ],
            model_override: None,
            permission_mode: PermissionMode::Plan,
            max_turns: 20,
            max_tool_calls: 50,
        },
        RoleDefinition {
            name: "implementer".into(),
            description: "Writes code that solves the assigned task".into(),
            system_prompt: "You are a code implementer. Write clean, correct code that solves \
                            the assigned task. Follow existing patterns and conventions in the \
                            codebase. Make minimal, focused changes."
                .into(),
            allowed_tools: vec![],
            denied_tools: vec![],
            model_override: None,
            permission_mode: PermissionMode::AcceptEdits,
            max_turns: 30,
            max_tool_calls: 100,
        },
        RoleDefinition {
            name: "reviewer".into(),
            description: "Reviews code for quality, bugs, and style issues".into(),
            system_prompt: "You are a code reviewer. Review the code changes for correctness, \
                            quality, security issues, and style. Produce a structured review \
                            with specific file:line references. Do not make changes yourself."
                .into(),
            allowed_tools: vec![
                "read_file".into(),
                "search_code".into(),
                "list_directory".into(),
                "git_status".into(),
                "git_diff".into(),
            ],
            denied_tools: vec![
                "write_file".into(),
                "edit_file".into(),
                "execute_bash".into(),
                "delete_path".into(),
            ],
            model_override: None,
            permission_mode: PermissionMode::Plan,
            max_turns: 15,
            max_tool_calls: 40,
        },
        RoleDefinition {
            name: "tester".into(),
            description: "Writes and runs tests for the assigned code".into(),
            system_prompt: "You are a test engineer. Write comprehensive tests (unit, \
                            integration) for the assigned code. Run tests to verify they pass. \
                            Follow the project's existing test patterns."
                .into(),
            allowed_tools: vec![],
            denied_tools: vec![],
            model_override: None,
            permission_mode: PermissionMode::AcceptEdits,
            max_turns: 25,
            max_tool_calls: 80,
        },
        RoleDefinition {
            name: "architect".into(),
            description: "Designs approach and structure for the task".into(),
            system_prompt: "You are a software architect. Analyze the codebase and design an \
                            approach for the assigned task. Produce a detailed plan with file \
                            paths, interfaces, and data flow. Do not write implementation code."
                .into(),
            allowed_tools: vec![
                "read_file".into(),
                "search_code".into(),
                "list_directory".into(),
                "git_status".into(),
                "git_diff".into(),
                "web_search".into(),
                "fetch_url".into(),
            ],
            denied_tools: vec![
                "write_file".into(),
                "edit_file".into(),
                "execute_bash".into(),
                "delete_path".into(),
            ],
            model_override: None,
            permission_mode: PermissionMode::Plan,
            max_turns: 15,
            max_tool_calls: 40,
        },
        RoleDefinition {
            name: "debugger".into(),
            description: "Finds root causes and fixes bugs".into(),
            system_prompt: "You are a debugger. Systematically investigate the reported issue: \
                            reproduce, isolate root cause, and implement a targeted fix. Explain \
                            your reasoning at each step."
                .into(),
            allowed_tools: vec![],
            denied_tools: vec![],
            model_override: None,
            permission_mode: PermissionMode::AcceptEdits,
            max_turns: 25,
            max_tool_calls: 80,
        },
    ]
}
