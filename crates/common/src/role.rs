use crate::config::PermissionMode;
use crate::team::RoleDefinition;
use serde::Deserialize;

/// TOML file structure for .nca/roles/*.toml
#[derive(Debug, Deserialize)]
pub struct RoleFile {
    pub role: RoleSection,
    pub tools: Option<ToolsSection>,
    pub model: Option<ModelSection>,
    pub constraints: Option<ConstraintsSection>,
}

#[derive(Debug, Deserialize)]
pub struct RoleSection {
    pub name: String,
    pub description: String,
    pub system_prompt: String,
}

#[derive(Debug, Deserialize)]
pub struct ToolsSection {
    pub allowed: Option<Vec<String>>,
    pub denied: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
pub struct ModelSection {
    pub preferred: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ConstraintsSection {
    pub max_turns: Option<u32>,
    pub max_tool_calls: Option<u32>,
    pub permission_mode: Option<PermissionMode>,
}

impl RoleFile {
    pub fn into_definition(self) -> RoleDefinition {
        RoleDefinition {
            name: self.role.name,
            description: self.role.description,
            system_prompt: self.role.system_prompt,
            allowed_tools: self
                .tools
                .as_ref()
                .and_then(|t| t.allowed.clone())
                .unwrap_or_default(),
            denied_tools: self
                .tools
                .as_ref()
                .and_then(|t| t.denied.clone())
                .unwrap_or_default(),
            model_override: self.model.and_then(|m| m.preferred),
            permission_mode: self
                .constraints
                .as_ref()
                .and_then(|c| c.permission_mode)
                .unwrap_or(PermissionMode::Default),
            max_turns: self
                .constraints
                .as_ref()
                .and_then(|c| c.max_turns)
                .unwrap_or(30),
            max_tool_calls: self
                .constraints
                .as_ref()
                .and_then(|c| c.max_tool_calls)
                .unwrap_or(100),
        }
    }
}
