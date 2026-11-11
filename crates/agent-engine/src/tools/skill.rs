use super::resolve_workspace_path;
use super::Tool;
use agent_core::ToolDefinition;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkillInfo {
    pub name: String,
    pub description: String,
    pub directory_name: String,
    pub path: PathBuf,
    pub content: String,
}

pub struct SkillTool;

impl Default for SkillTool {
    fn default() -> Self {
        Self::new()
    }
}

impl SkillTool {
    pub fn new() -> Self {
        Self
    }

    /// Discovers all available custom skills within `.agent/skills/*/SKILL.md` or `.ferric/skills/*/SKILL.md`
    pub fn discover_skills(workspace_root: &Path) -> Vec<SkillInfo> {
        let mut skills = Vec::new();
        let search_bases = [
            workspace_root.join(".ferric").join("skills"),
            workspace_root.join(".agent").join("skills"),
            workspace_root.join(".claude").join("skills"),
        ];

        for base in search_bases {
            if !base.is_dir() {
                continue;
            }

            let entries = match std::fs::read_dir(&base) {
                Ok(e) => e,
                Err(_) => continue,
            };

            for entry in entries.filter_map(|e| e.ok()) {
                let skill_dir = entry.path();
                if !skill_dir.is_dir() {
                    continue;
                }

                let dir_name = entry.file_name().to_string_lossy().to_string();

                // Look for SKILL.md or skill.md
                let candidate_file = if skill_dir.join("SKILL.md").is_file() {
                    Some(skill_dir.join("SKILL.md"))
                } else if skill_dir.join("skill.md").is_file() {
                    Some(skill_dir.join("skill.md"))
                } else {
                    None
                };

                if let Some(skill_file) = candidate_file {
                    if let Ok(content) = std::fs::read_to_string(&skill_file) {
                        let (name, description, body) = parse_skill_markdown(&content, &dir_name);
                        skills.push(SkillInfo {
                            name,
                            description,
                            directory_name: dir_name,
                            path: skill_file,
                            content: body,
                        });
                    }
                }
            }
        }

        skills.sort_by(|a, b| a.name.cmp(&b.name));
        skills
    }
}

#[async_trait]
impl Tool for SkillTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "Skill".into(),
            description: "Discover and load custom skills from `.agent/skills/*/SKILL.md` or `.ferric/skills/*/SKILL.md`. Call with skill_name to load skill instructions, or leave empty to list available skills."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "skill_name": {
                        "type": "string",
                        "description": "Optional name or directory name of the skill to load"
                    },
                    "action": {
                        "type": "string",
                        "enum": ["load", "list"],
                        "description": "Action to perform: 'load' (default when skill_name is provided) or 'list'"
                    }
                }
            }),
        }
    }

    async fn execute(
        &self,
        input: serde_json::Value,
        workspace_root: &Path,
    ) -> Result<String, String> {
        let skills = Self::discover_skills(workspace_root);

        let skill_name_opt = input["skill_name"]
            .as_str()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty());
        let action = input["action"]
            .as_str()
            .unwrap_or(if skill_name_opt.is_some() {
                "load"
            } else {
                "list"
            });

        match action {
            "list" => {
                if skills.is_empty() {
                    return Ok("No custom skills found in '.agent/skills/*/SKILL.md' or '.ferric/skills/*/SKILL.md'.".to_string());
                }

                let mut out = format!("Discovered {} custom skill(s):\n\n", skills.len());
                for skill in &skills {
                    out.push_str(&format!(
                        "- **{}**: {} (dir: `{}`)\n",
                        skill.name, skill.description, skill.directory_name
                    ));
                }
                out.push_str("\nTo load instructions for a skill, use `Skill` with skill_name.");
                Ok(out)
            }
            "load" => {
                let target_name = skill_name_opt.ok_or(
                    "Missing 'skill_name' to load. Use action='list' to view available skills.",
                )?;

                // Match against skill name or directory name
                let found = skills.iter().find(|s| {
                    s.name.eq_ignore_ascii_case(target_name)
                        || s.directory_name.eq_ignore_ascii_case(target_name)
                });

                if let Some(skill) = found {
                    // Check workspace boundary
                    let _ =
                        resolve_workspace_path(workspace_root, skill.path.to_str().unwrap_or(""))?;
                    Ok(format!(
                        "# Skill: {}\nDescription: {}\n\n{}",
                        skill.name, skill.description, skill.content
                    ))
                } else {
                    let available = if skills.is_empty() {
                        "none".to_string()
                    } else {
                        skills
                            .iter()
                            .map(|s| format!("'{}'", s.name))
                            .collect::<Vec<_>>()
                            .join(", ")
                    };
                    Err(format!(
                        "Skill '{target_name}' not found. Available skills: {available}"
                    ))
                }
            }
            other => Err(format!(
                "Invalid action '{other}'. Expected 'load' or 'list'"
            )),
        }
    }
}

/// Parses frontmatter and body from a SKILL.md file
fn parse_skill_markdown(content: &str, fallback_name: &str) -> (String, String, String) {
    let trimmed = content.trim();

    if let Some(rest) = trimmed.strip_prefix("---") {
        if let Some(end_idx) = rest.find("---") {
            let frontmatter = &rest[..end_idx];
            let body = rest[end_idx + 3..].trim().to_string();

            let mut name = String::new();
            let mut description = String::new();

            for line in frontmatter.lines() {
                let line_trimmed = line.trim();
                if let Some(val) = line_trimmed.strip_prefix("name:") {
                    name = val.trim().trim_matches('"').trim_matches('\'').to_string();
                } else if let Some(val) = line_trimmed.strip_prefix("description:") {
                    description = val.trim().trim_matches('"').trim_matches('\'').to_string();
                }
            }

            let final_name = if name.is_empty() {
                fallback_name.to_string()
            } else {
                name
            };

            let final_desc = if description.is_empty() {
                extract_first_paragraph(&body)
            } else {
                description
            };

            return (final_name, final_desc, body);
        }
    }

    // No frontmatter present
    let description = extract_first_paragraph(trimmed);
    (fallback_name.to_string(), description, trimmed.to_string())
}

fn extract_first_paragraph(body: &str) -> String {
    for line in body.lines() {
        let t = line.trim().trim_start_matches('#').trim();
        if !t.is_empty() {
            return t.chars().take(120).collect();
        }
    }
    "Custom skill instructions".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_skill_discovery_and_loading() {
        let temp_dir = tempfile::tempdir().unwrap();
        let root = temp_dir.path();

        // Create .agent/skills/git-helper/SKILL.md with frontmatter
        let skill1_dir = root.join(".agent").join("skills").join("git-helper");
        tokio::fs::create_dir_all(&skill1_dir).await.unwrap();
        let skill1_content = r#"---
name: Git Workflow Helper
description: Provides best practices for git commits and rebasing
---
## Git Instructions
Always write atomic commits.
"#;
        tokio::fs::write(skill1_dir.join("SKILL.md"), skill1_content)
            .await
            .unwrap();

        // Create .agent/skills/clean-code/SKILL.md without frontmatter
        let skill2_dir = root.join(".agent").join("skills").join("clean-code");
        tokio::fs::create_dir_all(&skill2_dir).await.unwrap();
        let skill2_content = "# Clean Code\nWrite clean, readable functions with comments.";
        tokio::fs::write(skill2_dir.join("SKILL.md"), skill2_content)
            .await
            .unwrap();

        let tool = SkillTool::new();

        // 1. List skills
        let list_res = tool
            .execute(json!({ "action": "list" }), root)
            .await
            .unwrap();
        assert!(list_res.contains("Discovered 2 custom skill(s)"));
        assert!(list_res.contains("Git Workflow Helper"));
        assert!(list_res.contains("clean-code"));

        // 2. Load skill by frontmatter name
        let load_res1 = tool
            .execute(
                json!({
                    "skill_name": "Git Workflow Helper"
                }),
                root,
            )
            .await
            .unwrap();
        assert!(load_res1.contains("Always write atomic commits."));

        // 3. Load skill by directory name
        let load_res2 = tool
            .execute(
                json!({
                    "skill_name": "clean-code"
                }),
                root,
            )
            .await
            .unwrap();
        assert!(load_res2.contains("Write clean, readable functions with comments."));

        // 4. Load nonexistent skill
        let err_res = tool
            .execute(
                json!({
                    "skill_name": "nonexistent"
                }),
                root,
            )
            .await;
        assert!(err_res.is_err());
        assert!(err_res.unwrap_err().contains("not found"));
    }

    #[tokio::test]
    async fn test_empty_workspace_skills() {
        let temp_dir = tempfile::tempdir().unwrap();
        let root = temp_dir.path();
        let tool = SkillTool::new();

        let list_res = tool
            .execute(json!({ "action": "list" }), root)
            .await
            .unwrap();
        assert!(list_res.contains("No custom skills found"));
    }
}
