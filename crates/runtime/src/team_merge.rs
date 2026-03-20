use crate::worktree::WorktreeManager;
use std::path::{Path, PathBuf};

pub struct MergeResult {
    pub branch: String,
    pub worktree_path: PathBuf,
    pub conflicts: Vec<String>,
    pub merged_agents: Vec<String>,
}

pub fn merge_agent_branches(
    workspace_root: &Path,
    orch_id: &str,
    agent_branches: &[(String, String)], // (agent_name, branch_name)
) -> Result<MergeResult, String> {
    let wt_manager = WorktreeManager::new(workspace_root);
    let merge_info = wt_manager
        .create_merge_worktree(orch_id)
        .map_err(|e| format!("Failed to create merge worktree: {e}"))?;

    let mut merged = vec![];
    let mut all_conflicts = vec![];

    for (agent_name, branch) in agent_branches {
        match wt_manager.merge_branch_into_worktree(&merge_info.worktree_path, branch) {
            Ok(true) => merged.push(agent_name.clone()),
            Ok(false) => {
                let conflicts = wt_manager.conflict_files(&merge_info.worktree_path);
                all_conflicts.extend(conflicts);
                let _ = std::process::Command::new("git")
                    .args(["merge", "--abort"])
                    .current_dir(&merge_info.worktree_path)
                    .output();
            }
            Err(e) => return Err(format!("Failed to merge {agent_name} ({branch}): {e}")),
        }
    }

    Ok(MergeResult {
        branch: merge_info.branch_name,
        worktree_path: merge_info.worktree_path,
        conflicts: all_conflicts,
        merged_agents: merged,
    })
}
