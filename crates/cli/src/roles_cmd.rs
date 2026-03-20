use nca_runtime::role_catalog::RoleCatalog;
use std::path::Path;

pub fn run_roles_list(workspace_root: &Path) {
    let catalog = RoleCatalog::load(workspace_root);
    println!("Available roles:\n");
    for role in catalog.list() {
        println!("  {} — {}", role.name, role.description);
        println!(
            "    Mode: {:?} | Max turns: {} | Max tools: {}",
            role.permission_mode, role.max_turns, role.max_tool_calls
        );
        if let Some(model) = &role.model_override {
            println!("    Model: {model}");
        }
        println!();
    }
}

pub fn run_roles_show(workspace_root: &Path, name: &str) {
    let catalog = RoleCatalog::load(workspace_root);
    match catalog.get(name) {
        Some(role) => {
            println!("Role: {}", role.name);
            println!("Description: {}", role.description);
            println!("Permission mode: {:?}", role.permission_mode);
            println!("Max turns: {}", role.max_turns);
            println!("Max tool calls: {}", role.max_tool_calls);
            if let Some(model) = &role.model_override {
                println!("Preferred model: {model}");
            }
            if !role.allowed_tools.is_empty() {
                println!("Allowed tools: {}", role.allowed_tools.join(", "));
            }
            if !role.denied_tools.is_empty() {
                println!("Denied tools: {}", role.denied_tools.join(", "));
            }
            println!("\nSystem prompt:\n{}", role.system_prompt);
        }
        None => {
            eprintln!("Role '{name}' not found. Use 'nca roles list' to see available roles.");
        }
    }
}
