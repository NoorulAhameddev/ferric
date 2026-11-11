use std::path::Path;

pub const DEFAULT_BASE_PERSONA: &str =
    "You are an expert AI software engineer. Analyze tasks carefully, read files before modifying, and verify changes.";

/// Information about the current git repository state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitContext {
    pub branch: String,
    pub status: String,
}

/// Discovers project context document in the workspace root (`FERRIC.md`, `AGENT.md`, `PROJECT.md`, etc.).
pub fn discover_project_instructions(workspace_root: &Path) -> Option<String> {
    let candidate_paths = [
        workspace_root.join("FERRIC.md"),
        workspace_root.join(".ferric").join("FERRIC.md"),
        workspace_root.join("AGENT.md"),
        workspace_root.join(".agent").join("AGENT.md"),
        workspace_root.join("agent.md"),
        workspace_root.join("PROJECT.md"),
        workspace_root.join("CLAUDE.md"),
        workspace_root.join(".claude").join("CLAUDE.md"),
        workspace_root.join("claude.md"),
    ];

    for path in &candidate_paths {
        if path.is_file() {
            if let Ok(content) = std::fs::read_to_string(path) {
                let trimmed = content.trim();
                if !trimmed.is_empty() {
                    return Some(trimmed.to_string());
                }
            }
        }
    }
    None
}

pub use discover_project_instructions as discover_claude_md;

/// Detects git repository context by running `git branch --show-current` (or `git rev-parse --abbrev-ref HEAD`)
/// and `git status --short`.
pub fn detect_git_context(workspace_root: &Path) -> Option<GitContext> {
    // 1. Verify that this directory is inside a git working tree
    let is_inside_work_tree = std::process::Command::new("git")
        .args(["--no-pager", "rev-parse", "--is-inside-work-tree"])
        .env("GIT_TERMINAL_PROMPT", "0")
        .current_dir(workspace_root)
        .output()
        .map(|out| out.status.success() && String::from_utf8_lossy(&out.stdout).trim() == "true")
        .unwrap_or(false);

    if !is_inside_work_tree {
        return None;
    }

    // 2. Query branch name: try `git branch --show-current`, fallback to `git rev-parse --abbrev-ref HEAD`
    let branch = std::process::Command::new("git")
        .args(["--no-pager", "branch", "--show-current"])
        .env("GIT_TERMINAL_PROMPT", "0")
        .current_dir(workspace_root)
        .output()
        .ok()
        .filter(|out| out.status.success())
        .and_then(|out| {
            let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if s.is_empty() {
                None
            } else {
                Some(s)
            }
        })
        .or_else(|| {
            std::process::Command::new("git")
                .args(["--no-pager", "rev-parse", "--abbrev-ref", "HEAD"])
                .env("GIT_TERMINAL_PROMPT", "0")
                .current_dir(workspace_root)
                .output()
                .ok()
                .filter(|out| out.status.success())
                .and_then(|out| {
                    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
                    if s.is_empty() {
                        None
                    } else {
                        Some(s)
                    }
                })
        })
        .unwrap_or_else(|| "HEAD".to_string());

    // 3. Query git status --short
    let status = std::process::Command::new("git")
        .args(["--no-pager", "status", "--short"])
        .env("GIT_TERMINAL_PROMPT", "0")
        .current_dir(workspace_root)
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| {
            let s = String::from_utf8_lossy(&out.stdout);
            let trimmed = s.trim_end();
            if trimmed.is_empty() {
                "clean".to_string()
            } else {
                trimmed.to_string()
            }
        })
        .unwrap_or_else(|| "clean".to_string());

    Some(GitContext { branch, status })
}

/// Assembles the structured system prompt from base persona, optional project instructions,
/// git context, and custom instructions.
pub fn assemble_system_prompt(
    base_persona: &str,
    project_instructions: Option<&str>,
    git_context: Option<&GitContext>,
    custom_instructions: Option<&str>,
) -> String {
    let mut prompt = String::from(base_persona);

    if let Some(content) = project_instructions {
        let trimmed = content.trim();
        if !trimmed.is_empty() {
            prompt.push_str(&format!("\n\n# Project Instructions:\n{trimmed}"));
        }
    }

    if let Some(git_ctx) = git_context {
        prompt.push_str(&format!(
            "\n\n# Git Context:\nBranch: {}\nStatus:\n{}",
            git_ctx.branch, git_ctx.status
        ));
    }

    if let Some(instructions) = custom_instructions {
        let trimmed = instructions.trim();
        if !trimmed.is_empty() {
            prompt.push_str("\n\n");
            prompt.push_str(trimmed);
        }
    }

    prompt
}

/// Discovers workspace context and builds a dynamic structured system prompt.
///
/// Format:
/// - Base persona: "You are an expert AI software engineer..."
/// - If project guidelines exist: "\n\n# Project Instructions:\n{content}"
/// - If git repository exists: "\n\n# Git Context:\nBranch: {branch}\nStatus:\n{status}"
/// - If custom_instructions exists: appended at the end.
pub fn build_system_prompt(workspace_root: &Path, custom_instructions: Option<&str>) -> String {
    let instructions = discover_project_instructions(workspace_root);
    let git_ctx = detect_git_context(workspace_root);
    assemble_system_prompt(
        DEFAULT_BASE_PERSONA,
        instructions.as_deref(),
        git_ctx.as_ref(),
        custom_instructions,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_build_system_prompt_base_only() {
        let temp_dir = tempfile::tempdir().unwrap();
        // In a fresh non-git directory without CLAUDE.md
        let prompt = build_system_prompt(temp_dir.path(), None);
        assert_eq!(prompt, DEFAULT_BASE_PERSONA);
    }

    #[test]
    fn test_assemble_system_prompt_with_mock_instructions() {
        let mock_instructions =
            "# Project Guidelines\n- Always run tests\n- Adhere to Rust 2021 conventions";
        let prompt =
            assemble_system_prompt(DEFAULT_BASE_PERSONA, Some(mock_instructions), None, None);

        assert!(prompt.starts_with(DEFAULT_BASE_PERSONA));
        assert!(prompt.contains("# Project Instructions:\n# Project Guidelines\n- Always run tests\n- Adhere to Rust 2021 conventions"));
        assert!(!prompt.contains("# Git Context:"));
    }

    #[test]
    fn test_assemble_system_prompt_with_mock_git_status() {
        let git_ctx = GitContext {
            branch: "feature/context-engine".to_string(),
            status: " M crates/agent-engine/src/context.rs\n?? new_file.txt".to_string(),
        };

        let prompt = assemble_system_prompt(DEFAULT_BASE_PERSONA, None, Some(&git_ctx), None);

        assert!(prompt.starts_with(DEFAULT_BASE_PERSONA));
        assert!(!prompt.contains("# Project Instructions"));
        assert!(prompt.contains("# Git Context:\nBranch: feature/context-engine\nStatus:\n M crates/agent-engine/src/context.rs\n?? new_file.txt"));
    }

    #[test]
    fn test_assemble_system_prompt_with_custom_instructions() {
        let custom = "Always respond in concise bullet points.";
        let prompt = assemble_system_prompt(DEFAULT_BASE_PERSONA, None, None, Some(custom));

        assert!(prompt.starts_with(DEFAULT_BASE_PERSONA));
        assert!(prompt.ends_with("\n\nAlways respond in concise bullet points."));
    }

    #[test]
    fn test_assemble_system_prompt_complete_stack() {
        let mock_instructions = "Rule 1: Never break production.";
        let git_ctx = GitContext {
            branch: "main".to_string(),
            status: "clean".to_string(),
        };
        let custom = "Priority: performance.";

        let prompt = assemble_system_prompt(
            DEFAULT_BASE_PERSONA,
            Some(mock_instructions),
            Some(&git_ctx),
            Some(custom),
        );

        let inst_idx = prompt.find("# Project Instructions:").unwrap();
        let git_idx = prompt.find("# Git Context:").unwrap();
        let custom_idx = prompt.find("Priority: performance.").unwrap();

        assert!(inst_idx < git_idx);
        assert!(git_idx < custom_idx);
        assert!(prompt.contains("Branch: main\nStatus:\nclean"));
    }

    #[test]
    fn test_discover_instructions_in_root_and_nested() {
        let temp_dir = tempfile::tempdir().unwrap();
        let ws_root = temp_dir.path();

        // Initially none
        assert_eq!(discover_project_instructions(ws_root), None);

        // Discovers in root FERRIC.md
        let root_doc = ws_root.join("FERRIC.md");
        fs::write(&root_doc, "Root instructions").unwrap();
        assert_eq!(
            discover_project_instructions(ws_root),
            Some("Root instructions".to_string())
        );

        // Empty file ignored
        fs::write(&root_doc, "   \n  \t  ").unwrap();
        assert_eq!(discover_project_instructions(ws_root), None);

        fs::remove_file(&root_doc).unwrap();

        // Discovers in .ferric/FERRIC.md
        let dot_ferric_dir = ws_root.join(".ferric");
        fs::create_dir_all(&dot_ferric_dir).unwrap();
        let dot_doc = dot_ferric_dir.join("FERRIC.md");
        fs::write(&dot_doc, "Nested instructions").unwrap();
        assert_eq!(
            discover_project_instructions(ws_root),
            Some("Nested instructions".to_string())
        );
    }

    #[test]
    fn test_build_system_prompt_in_temp_git_repo() {
        let temp_dir = tempfile::tempdir().unwrap();
        let ws_root = temp_dir.path();

        // Initialize a git repo in temp_dir
        let init_status = std::process::Command::new("git")
            .args(["init"])
            .current_dir(ws_root)
            .status();

        if let Ok(status) = init_status {
            if status.success() {
                // Add a file
                fs::write(ws_root.join("test.txt"), "hello").unwrap();
                fs::write(ws_root.join("FERRIC.md"), "Project guidelines here").unwrap();

                let prompt = build_system_prompt(ws_root, Some("Extra instruction"));

                assert!(prompt.contains(DEFAULT_BASE_PERSONA));
                assert!(prompt.contains("# Project Instructions:\nProject guidelines here"));
                assert!(prompt.contains("# Git Context:\nBranch: "));
                assert!(prompt.contains("Status:\n"));
                assert!(prompt.contains("?? test.txt") || prompt.contains("?? FERRIC.md"));
                assert!(prompt.ends_with("Extra instruction"));
            }
        }
    }
}
