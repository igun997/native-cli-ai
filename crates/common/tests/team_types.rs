use nca_common::config::PermissionMode;
use nca_common::role::RoleFile;
use nca_common::team::*;

#[test]
fn test_toml_role_parsing() {
    let toml_str = r#"
[role]
name = "custom-scanner"
description = "Scans for issues"
system_prompt = "You are a scanner."

[tools]
allowed = ["read_file", "search_code"]
denied = ["execute_bash"]

[model]
preferred = "minimax-m2.5"

[constraints]
max_turns = 10
max_tool_calls = 25
permission_mode = "plan"
"#;
    let role_file: RoleFile = toml::from_str(toml_str).unwrap();
    let def = role_file.into_definition();
    assert_eq!(def.name, "custom-scanner");
    assert_eq!(def.allowed_tools, vec!["read_file", "search_code"]);
    assert_eq!(def.denied_tools, vec!["execute_bash"]);
    assert_eq!(def.max_turns, 10);
}

#[test]
fn test_team_plan_serde_roundtrip() {
    let plan = TeamPlan {
        id: TeamOrchestrationId::new("team-test-123"),
        task: "Fix the bug".to_string(),
        agents: vec![AgentAssignment {
            name: "researcher-1".to_string(),
            role: RoleDefinition {
                name: "researcher".into(),
                description: "test".into(),
                system_prompt: "test".into(),
                allowed_tools: vec!["read_file".into()],
                denied_tools: vec![],
                model_override: None,
                permission_mode: PermissionMode::Plan,
                max_turns: 20,
                max_tool_calls: 50,
            },
            task_brief: "Analyze the auth module".to_string(),
            depends_on: vec![],
            needs_workspace: true,
            custom_system_prompt: None,
        }],
        execution_order: ExecutionOrder {
            stages: vec![ExecutionStage {
                parallel: vec!["researcher-1".into()],
            }],
        },
    };

    let json = serde_json::to_string(&plan).unwrap();
    let parsed: TeamPlan = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.id.to_string(), "team-test-123");
    assert_eq!(parsed.agents.len(), 1);
    assert_eq!(parsed.execution_order.stages.len(), 1);
}

#[test]
fn test_team_command_serde_all_variants() {
    let commands = vec![
        TeamCommand::PauseAgent {
            name: "researcher-1".into(),
        },
        TeamCommand::ResumeAgent {
            name: "researcher-1".into(),
        },
        TeamCommand::RedirectAgent {
            name: "impl-1".into(),
            message: "new task".into(),
        },
        TeamCommand::CancelAgent {
            name: "impl-1".into(),
        },
        TeamCommand::CancelAll,
        TeamCommand::QueryStatus,
    ];

    for cmd in &commands {
        let json = serde_json::to_string(cmd).unwrap();
        let parsed: TeamCommand = serde_json::from_str(&json).unwrap();
        // Verify the tag field exists
        assert!(json.contains("\"type\""), "Missing type tag in: {}", json);
        // Verify roundtrip produces valid JSON
        let _reparsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        drop(parsed);
    }
}

#[test]
fn test_enum_serde_snake_case() {
    let json = serde_json::to_string(&WorkerStatus::Working).unwrap();
    assert_eq!(json, "\"working\"");
    let json = serde_json::to_string(&WorkerStatus::Paused).unwrap();
    assert_eq!(json, "\"paused\"");

    let json = serde_json::to_string(&TeamPhase::Decomposition).unwrap();
    assert_eq!(json, "\"decomposition\"");

    let json = serde_json::to_string(&TeamOutcome::PartialSuccess).unwrap();
    assert_eq!(json, "\"partial_success\"");
}
