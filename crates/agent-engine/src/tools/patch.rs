use super::Tool;
use agent_core::ToolDefinition;
use async_trait::async_trait;
use serde_json::json;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LineOp {
    Context(String),
    Delete(String),
    Add(String),
}

#[derive(Debug, Clone)]
pub struct PatchHunk {
    pub old_start: usize,
    pub old_count: usize,
    pub new_start: usize,
    pub new_count: usize,
    pub lines: Vec<LineOp>,
}

#[derive(Debug, Clone)]
pub struct FilePatch {
    pub old_file: Option<String>,
    pub new_file: Option<String>,
    pub is_new: bool,
    pub is_deleted: bool,
    pub hunks: Vec<PatchHunk>,
}

pub struct PatchApplyTool;

impl PatchApplyTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for PatchApplyTool {
    fn default() -> Self {
        Self::new()
    }
}

/// Parses a unified diff string into structured FilePatch blocks
pub fn parse_unified_diff(diff_str: &str) -> Result<Vec<FilePatch>, String> {
    let mut patches = Vec::new();
    let lines: Vec<&str> = diff_str.lines().collect();
    let mut i = 0;

    while i < lines.len() {
        let line = lines[i];

        // Match file header (diff --git a/... b/... or --- a/... +++ b/...)
        if line.starts_with("diff --git ") || line.starts_with("--- ") {
            let mut old_file = None;
            let mut new_file = None;

            if line.starts_with("diff --git ") {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 4 {
                    old_file = Some(clean_diff_path(parts[2]));
                    new_file = Some(clean_diff_path(parts[3]));
                }
                i += 1;
            }

            // Consume extended header lines until --- and +++
            while i < lines.len() && !lines[i].starts_with("--- ") && !lines[i].starts_with("@@ ") {
                i += 1;
            }

            if i < lines.len() && lines[i].starts_with("--- ") {
                let path = lines[i]
                    .trim_start_matches("--- ")
                    .split_whitespace()
                    .next()
                    .unwrap_or("");
                old_file = Some(clean_diff_path(path));
                i += 1;
            }

            if i < lines.len() && lines[i].starts_with("+++ ") {
                let path = lines[i]
                    .trim_start_matches("+++ ")
                    .split_whitespace()
                    .next()
                    .unwrap_or("");
                new_file = Some(clean_diff_path(path));
                i += 1;
            }

            let is_new = old_file.as_deref() == Some("/dev/null");
            let is_deleted = new_file.as_deref() == Some("/dev/null");

            let mut hunks = Vec::new();

            // Parse hunks
            while i < lines.len() && lines[i].starts_with("@@ ") {
                let hunk_header = lines[i];
                i += 1;

                let (old_start, old_count, new_start, new_count) = parse_hunk_header(hunk_header)?;
                let mut hunk_lines = Vec::new();

                while i < lines.len() {
                    let cur = lines[i];
                    if cur.starts_with("diff --git ")
                        || cur.starts_with("--- ")
                        || cur.starts_with("@@ ")
                    {
                        break;
                    }

                    if let Some(rest) = cur.strip_prefix(' ') {
                        hunk_lines.push(LineOp::Context(rest.to_string()));
                    } else if let Some(rest) = cur.strip_prefix('-') {
                        hunk_lines.push(LineOp::Delete(rest.to_string()));
                    } else if let Some(rest) = cur.strip_prefix('+') {
                        hunk_lines.push(LineOp::Add(rest.to_string()));
                    } else if cur.starts_with('\\') {
                        // "\ No newline at end of file"
                    } else if cur.is_empty() {
                        // Empty line treated as empty context
                        hunk_lines.push(LineOp::Context(String::new()));
                    } else {
                        break;
                    }
                    i += 1;
                }

                hunks.push(PatchHunk {
                    old_start,
                    old_count,
                    new_start,
                    new_count,
                    lines: hunk_lines,
                });
            }

            if old_file.is_some() || new_file.is_some() {
                patches.push(FilePatch {
                    old_file,
                    new_file,
                    is_new,
                    is_deleted,
                    hunks,
                });
            }
        } else {
            i += 1;
        }
    }

    if patches.is_empty() {
        return Err("No valid unified diff hunks found in input patch.".into());
    }

    Ok(patches)
}

fn clean_diff_path(path: &str) -> String {
    let p = path.trim().trim_matches('"');
    if let Some(stripped) = p.strip_prefix("a/") {
        stripped.to_string()
    } else if let Some(stripped) = p.strip_prefix("b/") {
        stripped.to_string()
    } else {
        p.to_string()
    }
}

fn parse_hunk_header(header: &str) -> Result<(usize, usize, usize, usize), String> {
    // Format: @@ -old_start,old_count +new_start,new_count @@
    let parts: Vec<&str> = header.split("@@").collect();
    if parts.len() < 3 {
        return Err(format!("Malformed hunk header: {header}"));
    }

    let coords = parts[1].trim();
    let sub: Vec<&str> = coords.split_whitespace().collect();
    if sub.len() < 2 {
        return Err(format!("Malformed coords in hunk header: {coords}"));
    }

    let parse_coord = |s: &str, prefix: char| -> Result<(usize, usize), String> {
        let s = s
            .strip_prefix(prefix)
            .ok_or_else(|| format!("Expected prefix {prefix}"))?;
        if let Some((start, count)) = s.split_once(',') {
            let st = start.parse::<usize>().map_err(|e| e.to_string())?;
            let ct = count.parse::<usize>().map_err(|e| e.to_string())?;
            Ok((st, ct))
        } else {
            let st = s.parse::<usize>().map_err(|e| e.to_string())?;
            Ok((st, 1))
        }
    };

    let (old_start, old_count) = parse_coord(sub[0], '-')?;
    let (new_start, new_count) = parse_coord(sub[1], '+')?;

    Ok((old_start, old_count, new_start, new_count))
}

/// Applies a single file's hunks in-memory. Returns transformed lines if successful.
fn apply_file_hunks(
    original_lines: &[String],
    hunks: &[PatchHunk],
    file_name: &str,
) -> Result<Vec<String>, String> {
    let mut result = original_lines.to_vec();
    let mut line_offset: isize = 0;

    for (h_idx, hunk) in hunks.iter().enumerate() {
        // Calculate expected target index
        let target_idx = (hunk.old_start as isize - 1 + line_offset).max(0) as usize;

        // Find actual match pos (exact or within small fuzzy window ±3)
        let mut match_idx = None;
        let search_start = target_idx.saturating_sub(3);
        let search_end = (target_idx + 3).min(result.len());

        for candidate in search_start..=search_end {
            if test_hunk_match(&result, candidate, &hunk.lines) {
                match_idx = Some(candidate);
                break;
            }
        }

        let actual_pos = match match_idx {
            Some(p) => p,
            None => {
                return Err(format!(
                    "Hunk #{} failed to apply to {file_name}: context lines at line {} do not match file content.",
                    h_idx + 1,
                    hunk.old_start
                ));
            }
        };

        // Apply hunk: remove deleted/context lines, insert added/context lines
        let mut new_slice = Vec::new();
        let mut old_consumed = 0;

        for op in &hunk.lines {
            match op {
                LineOp::Context(c) => {
                    new_slice.push(c.clone());
                    old_consumed += 1;
                }
                LineOp::Delete(_) => {
                    old_consumed += 1;
                }
                LineOp::Add(a) => {
                    new_slice.push(a.clone());
                }
            }
        }

        let replace_end = (actual_pos + old_consumed).min(result.len());
        result.splice(actual_pos..replace_end, new_slice.clone());

        let lines_diff = (new_slice.len() as isize) - (old_consumed as isize);
        line_offset += lines_diff;
    }

    Ok(result)
}

fn test_hunk_match(lines: &[String], start: usize, ops: &[LineOp]) -> bool {
    let mut cur = start;
    for op in ops {
        match op {
            LineOp::Context(expected) | LineOp::Delete(expected) => {
                if cur >= lines.len() {
                    return false;
                }
                if lines[cur].trim_end() != expected.trim_end() {
                    return false;
                }
                cur += 1;
            }
            LineOp::Add(_) => {}
        }
    }
    true
}

#[async_trait]
impl Tool for PatchApplyTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "PatchApply".into(),
            description: "Apply a multi-file unified diff transactionally. Verifies all hunks across all target files before modifying anything. If any hunk fails, rolls back completely with zero disk changes.".into(),
            input_schema: json!({
                "type": "object",
                "required": ["patch"],
                "properties": {
                    "patch": {
                        "type": "string",
                        "description": "Unified diff content (including diff --git, ---, +++, @@ markers)"
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
        let patch_text = input
            .get("patch")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "Missing required parameter 'patch'".to_string())?;

        let file_patches = parse_unified_diff(patch_text)?;

        // Phase 1: Dry run validation across all files
        struct PlannedChange {
            target_path: PathBuf,
            relative_display: String,
            is_deletion: bool,
            content: Option<String>,
        }

        let mut planned_changes = Vec::new();

        for fp in &file_patches {
            let rel_name = fp
                .new_file
                .as_ref()
                .or(fp.old_file.as_ref())
                .ok_or_else(|| "Patch has neither old nor new file name".to_string())?;

            if rel_name == "/dev/null" {
                continue;
            }

            let target_path = workspace_root.join(rel_name);

            // Path containment safety
            let canonical_root = dunce::canonicalize(workspace_root)
                .unwrap_or_else(|_| workspace_root.to_path_buf());
            let target_parent = target_path.parent().unwrap_or(workspace_root);
            if let Ok(canon_parent) = dunce::canonicalize(target_parent) {
                if !canon_parent.starts_with(&canonical_root) {
                    return Err(format!(
                        "Security violation: Patch target '{rel_name}' escapes workspace root."
                    ));
                }
            }

            if fp.is_deleted {
                if !target_path.exists() {
                    return Err(format!("File to delete does not exist: {rel_name}"));
                }
                planned_changes.push(PlannedChange {
                    target_path,
                    relative_display: rel_name.clone(),
                    is_deletion: true,
                    content: None,
                });
            } else if fp.is_new {
                // File creation
                let mut new_lines = Vec::new();
                for h in &fp.hunks {
                    for op in &h.lines {
                        if let LineOp::Add(a) = op {
                            new_lines.push(a.clone());
                        }
                    }
                }
                let body = new_lines.join("\n") + "\n";
                planned_changes.push(PlannedChange {
                    target_path,
                    relative_display: rel_name.clone(),
                    is_deletion: false,
                    content: Some(body),
                });
            } else {
                // Modification of existing file
                if !target_path.exists() {
                    return Err(format!("Target file does not exist to patch: {rel_name}"));
                }

                let original_raw = std::fs::read_to_string(&target_path)
                    .map_err(|e| format!("Failed to read {rel_name}: {e}"))?;
                let original_lines: Vec<String> = original_raw.lines().map(String::from).collect();

                let transformed_lines = apply_file_hunks(&original_lines, &fp.hunks, rel_name)?;
                let body = transformed_lines.join("\n") + "\n";

                planned_changes.push(PlannedChange {
                    target_path,
                    relative_display: rel_name.clone(),
                    is_deletion: false,
                    content: Some(body),
                });
            }
        }

        // Phase 2: Atomic commit to disk (all validations passed!)
        let mut modified_summary = Vec::new();

        for change in planned_changes {
            if change.is_deletion {
                std::fs::remove_file(&change.target_path)
                    .map_err(|e| format!("Failed to delete {}: {e}", change.relative_display))?;
                modified_summary.push(format!("deleted {}", change.relative_display));
            } else if let Some(content) = change.content {
                if let Some(parent) = change.target_path.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| {
                        format!(
                            "Failed to create directories for {}: {e}",
                            change.relative_display
                        )
                    })?;
                }
                std::fs::write(&change.target_path, content)
                    .map_err(|e| format!("Failed to write {}: {e}", change.relative_display))?;
                modified_summary.push(format!("modified {}", change.relative_display));
            }
        }

        Ok(format!(
            "Successfully applied transactional patch across {} file(s):\n  • {}",
            modified_summary.len(),
            modified_summary.join("\n  • ")
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_parse_unified_diff_basic() {
        let diff = r#"diff --git a/src/lib.rs b/src/lib.rs
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1,3 +1,4 @@
 fn foo() {
-    let x = 1;
+    let x = 2;
+    let y = 3;
 }
"#;
        let patches = parse_unified_diff(diff).expect("diff should parse");
        assert_eq!(patches.len(), 1);
        assert_eq!(patches[0].old_file.as_deref(), Some("src/lib.rs"));
        assert_eq!(patches[0].new_file.as_deref(), Some("src/lib.rs"));
        assert_eq!(patches[0].hunks.len(), 1);
        assert_eq!(patches[0].hunks[0].old_start, 1);
        assert_eq!(patches[0].hunks[0].old_count, 3);
    }

    #[tokio::test]
    async fn test_patch_apply_transactional_success() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("main.rs");
        std::fs::write(&file_path, "fn main() {\n    let a = 1;\n}\n").unwrap();

        let patch = r#"--- a/main.rs
+++ b/main.rs
@@ -1,3 +1,3 @@
 fn main() {
-    let a = 1;
+    let a = 42;
 }
"#;
        let tool = PatchApplyTool::new();
        let res = tool
            .execute(json!({ "patch": patch }), dir.path())
            .await
            .expect("patch should apply");

        assert!(res.contains("modified main.rs"));
        let updated = std::fs::read_to_string(&file_path).unwrap();
        assert!(updated.contains("let a = 42;"));
    }

    #[tokio::test]
    async fn test_patch_apply_atomicity_on_failure() {
        let dir = tempdir().unwrap();
        let file1 = dir.path().join("file1.rs");
        let file2 = dir.path().join("file2.rs");
        std::fs::write(&file1, "line 1\nline 2\n").unwrap();
        std::fs::write(&file2, "target 1\ntarget 2\n").unwrap();

        // Diff where file1 matches, but file2 context does not match
        let patch = r#"diff --git a/file1.rs b/file1.rs
--- a/file1.rs
+++ b/file1.rs
@@ -1,2 +1,2 @@
 line 1
-line 2
+line 2 modified
diff --git a/file2.rs b/file2.rs
--- a/file2.rs
+++ b/file2.rs
@@ -1,2 +1,2 @@
 non_existent_context
-target 2
+target 2 modified
"#;
        let tool = PatchApplyTool::new();
        let res = tool.execute(json!({ "patch": patch }), dir.path()).await;
        assert!(
            res.is_err(),
            "patch must fail because file2 context is mismatched"
        );

        // Verify file1 was NOT touched (atomic zero-write rollback!)
        let content1 = std::fs::read_to_string(&file1).unwrap();
        assert_eq!(content1, "line 1\nline 2\n");
    }
}
