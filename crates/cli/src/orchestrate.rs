use nca_common::config::NcaConfig;
use nca_runtime::team_orchestrator::TeamOrchestrator;
use std::path::PathBuf;

pub async fn run_orchestrate(
    config: NcaConfig,
    workspace_root: PathBuf,
    prompt: String,
    agents: Option<String>,
) -> anyhow::Result<()> {
    println!("Starting multi-agent orchestration...\n");
    println!("Task: {prompt}");
    if let Some(hints) = &agents {
        println!("Agent hints: {hints}");
    }
    println!();

    let handle = TeamOrchestrator::start(config, workspace_root, prompt, agents)
        .await
        .map_err(anyhow::Error::msg)?;

    let mut event_rx = handle.subscribe();
    let event_task = tokio::spawn(async move {
        while let Ok(event) = event_rx.recv().await {
            let ts = event.timestamp.format("%H:%M:%S");
            println!("[{ts}] [{}] {:?}", event.source_agent, event.event);
        }
    });

    let result = handle.wait().await.map_err(anyhow::Error::msg)?;
    event_task.abort();

    println!("\n--- Orchestration Complete ---");
    println!("Outcome: {:?}", result.outcome);
    println!("Cost: ${:.4}", result.cost.total_usd);
    if let Some(branch) = &result.merge_branch {
        println!("Merge branch: {branch}");
    }
    println!("\nAgent Reports:");
    for report in &result.agent_reports {
        println!("  {} ({}): {:?}", report.name, report.role, report.status);
        if let Some(r) = &report.completion_report {
            let preview = if r.len() > 200 { &r[..200] } else { r };
            println!("    {preview}...");
        }
    }

    Ok(())
}
