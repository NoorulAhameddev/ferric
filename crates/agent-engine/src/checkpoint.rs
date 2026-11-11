use chrono::{DateTime, Utc};
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone)]
pub struct Checkpoint {
    pub turn: u32,
    pub tree_sha: String,
    pub commit_sha: String,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug)]
pub struct RollbackResult {
    pub turn: u32,
    pub restored_files: Vec<String>,
    pub deleted_files: Vec<String>,
}

pub struct CheckpointManager {
    workspace_root: PathBuf,
    session_id: String,
    checkpoints: Vec<Checkpoint>,
    is_git_repo: bool,
}

impl CheckpointManager {
    pub fn new(workspace_root: impl Into<PathBuf>, session_id: impl Into<String>) -> Self {
        let workspace_root = workspace_root.into();
        let session_id = session_id.into();
        let is_git_repo = Self::detect_git_repo(&workspace_root);

        Self {
            workspace_root,
            session_id,
            checkpoints: Vec::new(),
            is_git_repo,
        }
    }

    /// Checks whether the workspace directory is inside a Git repository
    fn detect_git_repo(root: &Path) -> bool {
        if root.join(".git").exists() {
            return true;
        }
        let output = Command::new("git")
            .args(["rev-parse", "--is-inside-work-tree"])
            .current_dir(root)
            .output();

        match output {
            Ok(out) => {
                out.status.success() && String::from_utf8_lossy(&out.stdout).trim() == "true"
            }
            Err(_) => false,
        }
    }

    pub fn is_git_repo(&self) -> bool {
        self.is_git_repo
    }

    pub fn checkpoints(&self) -> &[Checkpoint] {
        &self.checkpoints
    }

    /// Captures a pre-turn snapshot of the workspace using a shadow Git index.
    /// Does not modify the user's actual index or working files.
    pub fn create_checkpoint(&mut self, turn: u32) -> Result<Option<Checkpoint>, String> {
        if !self.is_git_repo {
            return Ok(None);
        }

        let temp_idx = std::env::temp_dir().join(format!(
            "ferric_chkpt_{}_{}.idx",
            self.session_id,
            uuid::Uuid::new_v4()
        ));

        // 1. Run git add -A using the shadow index
        let add_status = Command::new("git")
            .arg("add")
            .arg("-A")
            .env("GIT_INDEX_FILE", &temp_idx)
            .current_dir(&self.workspace_root)
            .status()
            .map_err(|e| format!("Failed to invoke git add for shadow checkpoint: {e}"))?;

        if !add_status.success() {
            let _ = std::fs::remove_file(&temp_idx);
            return Err("git add failed with shadow index".into());
        }

        // 2. Write tree object
        let write_tree_output = Command::new("git")
            .arg("write-tree")
            .env("GIT_INDEX_FILE", &temp_idx)
            .current_dir(&self.workspace_root)
            .output()
            .map_err(|e| format!("Failed to invoke git write-tree: {e}"))?;

        let _ = std::fs::remove_file(&temp_idx);

        if !write_tree_output.status.success() {
            return Err(format!(
                "git write-tree failed: {}",
                String::from_utf8_lossy(&write_tree_output.stderr)
            ));
        }

        let tree_sha = String::from_utf8_lossy(&write_tree_output.stdout)
            .trim()
            .to_string();

        if tree_sha.is_empty() {
            return Err("Empty tree SHA returned by git write-tree".into());
        }

        // 3. Create commit object from tree
        let commit_msg = format!(
            "ferric checkpoint: session {} turn {}",
            self.session_id, turn
        );
        let commit_output = Command::new("git")
            .args(["commit-tree", &tree_sha, "-m", &commit_msg])
            .current_dir(&self.workspace_root)
            .output()
            .map_err(|e| format!("Failed to create commit tree: {e}"))?;

        if !commit_output.status.success() {
            return Err(format!(
                "git commit-tree failed: {}",
                String::from_utf8_lossy(&commit_output.stderr)
            ));
        }

        let commit_sha = String::from_utf8_lossy(&commit_output.stdout)
            .trim()
            .to_string();

        // 4. Update shadow ref
        let ref_name = format!("refs/ferric/checkpoints/{}/turn_{}", self.session_id, turn);
        let _ = Command::new("git")
            .args(["update-ref", &ref_name, &commit_sha])
            .current_dir(&self.workspace_root)
            .output();

        let checkpoint = Checkpoint {
            turn,
            tree_sha,
            commit_sha,
            timestamp: Utc::now(),
        };

        self.checkpoints.push(checkpoint.clone());
        Ok(Some(checkpoint))
    }

    /// Reverts the workspace to the state of the most recent checkpoint.
    /// Non-destructively removes newly created files from that turn, and restores modified/deleted files.
    pub fn rollback_latest(&mut self) -> Result<RollbackResult, String> {
        if !self.is_git_repo {
            return Err("Workspace is not a Git repository; checkpoints unavailable.".into());
        }

        let target_cp = self
            .checkpoints
            .pop()
            .ok_or_else(|| "No checkpoints available to undo.".to_string())?;

        // 1. Capture current workspace state into a shadow index to identify changes
        let temp_curr_idx = std::env::temp_dir().join(format!(
            "ferric_curr_{}_{}.idx",
            self.session_id,
            uuid::Uuid::new_v4()
        ));

        let add_out = Command::new("git")
            .arg("add")
            .arg("-A")
            .env("GIT_INDEX_FILE", &temp_curr_idx)
            .current_dir(&self.workspace_root)
            .status();

        let curr_tree_sha = if add_out.map(|s| s.success()).unwrap_or(false) {
            let tree_out = Command::new("git")
                .arg("write-tree")
                .env("GIT_INDEX_FILE", &temp_curr_idx)
                .current_dir(&self.workspace_root)
                .output();
            let _ = std::fs::remove_file(&temp_curr_idx);
            tree_out
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        } else {
            let _ = std::fs::remove_file(&temp_curr_idx);
            None
        };

        let curr_tree = match curr_tree_sha {
            Some(sha) if !sha.is_empty() => sha,
            _ => {
                // Fallback: diff against working tree directly
                return Err("Failed to capture current workspace tree for rollback diff".into());
            }
        };

        // 2. Diff target tree vs current tree to see what changed during that turn
        let diff_output = Command::new("git")
            .args([
                "diff-tree",
                "-r",
                "--name-status",
                &target_cp.tree_sha,
                &curr_tree,
            ])
            .current_dir(&self.workspace_root)
            .output()
            .map_err(|e| format!("Failed to compute diff-tree: {e}"))?;

        if !diff_output.status.success() {
            return Err(format!(
                "git diff-tree failed: {}",
                String::from_utf8_lossy(&diff_output.stderr)
            ));
        }

        let diff_text = String::from_utf8_lossy(&diff_output.stdout);
        let mut restored_files = Vec::new();
        let mut deleted_files = Vec::new();

        for line in diff_text.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            let mut parts = line.split('\t');
            let status = parts.next().unwrap_or("");
            let file_path = parts.next().unwrap_or("");

            if file_path.is_empty() {
                continue;
            }

            let full_path = self.workspace_root.join(file_path);

            if status.starts_with('A') {
                // Newly added in current: delete it
                if full_path.is_file() {
                    let _ = std::fs::remove_file(&full_path);
                    deleted_files.push(file_path.to_string());
                } else if full_path.is_dir() {
                    let _ = std::fs::remove_dir_all(&full_path);
                    deleted_files.push(file_path.to_string());
                }
            } else if status.starts_with('M') || status.starts_with('D') {
                // Modified or deleted in current: checkout file from target checkpoint commit
                let co_status = Command::new("git")
                    .args(["checkout", &target_cp.commit_sha, "--", file_path])
                    .current_dir(&self.workspace_root)
                    .status();

                if co_status.map(|s| s.success()).unwrap_or(false) {
                    restored_files.push(file_path.to_string());
                }
            }
        }

        Ok(RollbackResult {
            turn: target_cp.turn,
            restored_files,
            deleted_files,
        })
    }
}
