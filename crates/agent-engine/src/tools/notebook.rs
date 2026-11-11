use super::resolve_workspace_path;
use super::Tool;
use agent_core::ToolDefinition;
use async_trait::async_trait;
use serde_json::json;
use std::path::Path;

pub struct NotebookEditTool;

impl Default for NotebookEditTool {
    fn default() -> Self {
        Self::new()
    }
}

impl NotebookEditTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for NotebookEditTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "NotebookEdit".into(),
            description:
                "Edit a Jupyter notebook (.ipynb) by replacing, inserting, or deleting cells."
                    .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "notebook_path": {
                        "type": "string",
                        "description": "Path to the Jupyter notebook (.ipynb) file relative to workspace root"
                    },
                    "cell_id": {
                        "type": "string",
                        "description": "Identifier or 0-based index of the target cell"
                    },
                    "edit_mode": {
                        "type": "string",
                        "enum": ["replace", "insert", "delete"],
                        "description": "The edit operation to perform: 'replace', 'insert', or 'delete'"
                    },
                    "new_source": {
                        "type": "string",
                        "description": "The new cell source code or markdown content (required for replace and insert)"
                    },
                    "cell_type": {
                        "type": "string",
                        "enum": ["code", "markdown", "raw"],
                        "description": "Cell type for inserted cell or when changing type in replace (defaults to 'code')"
                    }
                },
                "required": ["notebook_path", "edit_mode"]
            }),
        }
    }

    async fn execute(
        &self,
        input: serde_json::Value,
        workspace_root: &Path,
    ) -> Result<String, String> {
        let notebook_path_str = input["notebook_path"]
            .as_str()
            .ok_or("Missing 'notebook_path' argument")?;
        let edit_mode = input["edit_mode"]
            .as_str()
            .ok_or("Missing 'edit_mode' argument")?;

        let target_cell_id = match &input["cell_id"] {
            serde_json::Value::String(s) => Some(s.clone()),
            serde_json::Value::Number(n) => Some(n.to_string()),
            _ => None,
        };

        let new_source = input["new_source"].as_str();
        let cell_type_opt = input["cell_type"].as_str();

        let full_path = resolve_workspace_path(workspace_root, notebook_path_str)?;

        // Load or initialize notebook JSON
        let mut notebook: serde_json::Value = if full_path.exists() {
            let content = tokio::fs::read_to_string(&full_path)
                .await
                .map_err(|e| format!("Failed to read notebook '{}': {e}", full_path.display()))?;
            serde_json::from_str(&content)
                .map_err(|e| format!("Invalid JSON in notebook '{}': {e}", full_path.display()))?
        } else {
            if edit_mode != "insert" {
                return Err(format!(
                    "Notebook file '{}' does not exist. Use edit_mode='insert' to create a notebook.",
                    notebook_path_str
                ));
            }
            json!({
                "cells": [],
                "metadata": {
                    "language_info": { "name": "python" }
                },
                "nbformat": 4,
                "nbformat_minor": 5
            })
        };

        let cells = notebook
            .get_mut("cells")
            .and_then(|c| c.as_array_mut())
            .ok_or("Notebook missing 'cells' array")?;

        match edit_mode {
            "replace" => {
                let cell_id = target_cell_id
                    .as_deref()
                    .ok_or("Missing 'cell_id' for 'replace' mode")?;
                let source_content = new_source.ok_or("Missing 'new_source' for 'replace' mode")?;

                let cell_idx = find_cell_index(cells, cell_id)
                    .ok_or_else(|| format!("Cell '{cell_id}' not found in notebook"))?;

                let cell = &mut cells[cell_idx];

                if let Some(c_type) = cell_type_opt {
                    let old_type = cell.get("cell_type").and_then(|t| t.as_str()).unwrap_or("");
                    if old_type != c_type {
                        cell["cell_type"] = json!(c_type);
                        if c_type == "code" {
                            if cell.get("execution_count").is_none() {
                                cell["execution_count"] = json!(null);
                            }
                            if cell.get("outputs").is_none() {
                                cell["outputs"] = json!([]);
                            }
                        } else {
                            if let Some(obj) = cell.as_object_mut() {
                                obj.remove("execution_count");
                                obj.remove("outputs");
                            }
                        }
                    }
                }

                cell["source"] = source_to_json(source_content);

                save_notebook(&full_path, &notebook).await?;
                Ok(format!(
                    "Successfully replaced cell '{cell_id}' in notebook '{notebook_path_str}'"
                ))
            }
            "delete" => {
                let cell_id = target_cell_id
                    .as_deref()
                    .ok_or("Missing 'cell_id' for 'delete' mode")?;

                let cell_idx = find_cell_index(cells, cell_id)
                    .ok_or_else(|| format!("Cell '{cell_id}' not found in notebook"))?;

                cells.remove(cell_idx);
                let remaining_cells = cells.len();

                save_notebook(&full_path, &notebook).await?;
                Ok(format!(
                    "Successfully deleted cell '{cell_id}' from notebook '{notebook_path_str}' ({remaining_cells} cells remaining)"
                ))
            }
            "insert" => {
                let source_content = new_source.ok_or("Missing 'new_source' for 'insert' mode")?;
                let c_type = cell_type_opt.unwrap_or("code");
                let new_id = target_cell_id.as_deref().unwrap_or("");
                let final_id = if !new_id.is_empty() && find_cell_index(cells, new_id).is_none() {
                    new_id.to_string()
                } else {
                    let raw_id = uuid::Uuid::new_v4().to_string();
                    raw_id.replace('-', "")[..8].to_string()
                };

                let mut new_cell = json!({
                    "cell_type": c_type,
                    "id": final_id,
                    "metadata": {},
                    "source": source_to_json(source_content)
                });

                if c_type == "code" {
                    new_cell["execution_count"] = json!(null);
                    new_cell["outputs"] = json!([]);
                }

                // If cell_id is specified and exists, insert after that cell; otherwise append
                let insert_pos = if let Some(ref target_id) = target_cell_id {
                    if let Some(pos) = find_cell_index(cells, target_id) {
                        pos + 1
                    } else if let Ok(idx) = target_id.parse::<usize>() {
                        idx.min(cells.len())
                    } else {
                        cells.len()
                    }
                } else {
                    cells.len()
                };

                cells.insert(insert_pos, new_cell);
                let total_cells = cells.len();

                save_notebook(&full_path, &notebook).await?;
                Ok(format!(
                    "Successfully inserted {c_type} cell '{final_id}' at position {insert_pos} in notebook '{notebook_path_str}' ({total_cells} total cells)"
                ))
            }
            other => Err(format!(
                "Invalid edit_mode '{other}'. Expected 'replace', 'insert', or 'delete'"
            )),
        }
    }
}

fn find_cell_index(cells: &[serde_json::Value], cell_id: &str) -> Option<usize> {
    // 1. Direct ID match
    for (idx, cell) in cells.iter().enumerate() {
        if let Some(id) = cell.get("id").and_then(|v| v.as_str()) {
            if id == cell_id {
                return Some(idx);
            }
        }
    }

    // 2. 0-based integer index match
    if let Ok(idx) = cell_id.parse::<usize>() {
        if idx < cells.len() {
            return Some(idx);
        }
    }

    None
}

fn source_to_json(source: &str) -> serde_json::Value {
    let lines: Vec<String> = source
        .split_inclusive('\n')
        .map(|s| s.to_string())
        .collect();
    if lines.is_empty() {
        json!([source])
    } else {
        json!(lines)
    }
}

async fn save_notebook(path: &Path, notebook: &serde_json::Value) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| format!("Failed to create parent directory for notebook: {e}"))?;
    }

    let formatted = serde_json::to_string_pretty(notebook)
        .map_err(|e| format!("Failed to serialize notebook: {e}"))?;

    tokio::fs::write(path, formatted)
        .await
        .map_err(|e| format!("Failed to save notebook '{}': {e}", path.display()))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_notebook_insert_replace_delete_cycle() {
        let temp_dir = tempfile::tempdir().unwrap();
        let root = temp_dir.path();
        let tool = NotebookEditTool::new();

        // 1. Insert first code cell (creates notebook)
        let res_ins1 = tool
            .execute(
                json!({
                    "notebook_path": "analysis.ipynb",
                    "edit_mode": "insert",
                    "new_source": "import numpy as np\nx = np.array([1, 2, 3])\n",
                    "cell_type": "code"
                }),
                root,
            )
            .await;
        assert!(res_ins1.is_ok(), "Insert code cell failed: {:?}", res_ins1);

        // 2. Insert markdown cell
        let res_ins2 = tool
            .execute(
                json!({
                    "notebook_path": "analysis.ipynb",
                    "cell_id": "md-header",
                    "edit_mode": "insert",
                    "new_source": "# Data Analysis Notebook\n",
                    "cell_type": "markdown"
                }),
                root,
            )
            .await;
        assert!(res_ins2.is_ok());

        // Read notebook content to verify cells
        let file_path = root.join("analysis.ipynb");
        let content = tokio::fs::read_to_string(&file_path).await.unwrap();
        let nb: serde_json::Value = serde_json::from_str(&content).unwrap();
        let cells = nb["cells"].as_array().unwrap();
        assert_eq!(cells.len(), 2);
        assert_eq!(cells[0]["cell_type"], "code");
        assert_eq!(cells[1]["cell_type"], "markdown");

        // 3. Replace the code cell using index "0"
        let res_rep = tool
            .execute(
                json!({
                    "notebook_path": "analysis.ipynb",
                    "cell_id": "0",
                    "edit_mode": "replace",
                    "new_source": "import pandas as pd\ndf = pd.DataFrame({'a': [1, 2]})\n"
                }),
                root,
            )
            .await;
        assert!(res_rep.is_ok());

        // Verify updated source
        let content_after = tokio::fs::read_to_string(&file_path).await.unwrap();
        let nb_after: serde_json::Value = serde_json::from_str(&content_after).unwrap();
        let source_lines = nb_after["cells"][0]["source"].as_array().unwrap();
        assert!(source_lines[0].as_str().unwrap().contains("import pandas"));

        // 4. Delete the markdown cell using index "1"
        let res_del = tool
            .execute(
                json!({
                    "notebook_path": "analysis.ipynb",
                    "cell_id": "1",
                    "edit_mode": "delete"
                }),
                root,
            )
            .await;
        assert!(res_del.is_ok());

        let final_content = tokio::fs::read_to_string(&file_path).await.unwrap();
        let final_nb: serde_json::Value = serde_json::from_str(&final_content).unwrap();
        assert_eq!(final_nb["cells"].as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn test_notebook_boundary_escape() {
        let temp_dir = tempfile::tempdir().unwrap();
        let root = temp_dir.path();
        let tool = NotebookEditTool::new();

        let res = tool
            .execute(
                json!({
                    "notebook_path": "../escaped.ipynb",
                    "edit_mode": "insert",
                    "new_source": "print('evil')"
                }),
                root,
            )
            .await;

        assert_eq!(res.unwrap_err(), "Path escapes workspace boundary");
    }

    #[tokio::test]
    async fn test_notebook_invalid_cell_id() {
        let temp_dir = tempfile::tempdir().unwrap();
        let root = temp_dir.path();
        let tool = NotebookEditTool::new();

        // Create empty notebook
        let _ = tool
            .execute(
                json!({
                    "notebook_path": "nb.ipynb",
                    "edit_mode": "insert",
                    "new_source": "# Hello"
                }),
                root,
            )
            .await
            .unwrap();

        // Try replacing non-existent cell
        let res = tool
            .execute(
                json!({
                    "notebook_path": "nb.ipynb",
                    "cell_id": "nonexistent-id",
                    "edit_mode": "replace",
                    "new_source": "# New"
                }),
                root,
            )
            .await;

        assert!(res.is_err());
        assert!(res.unwrap_err().contains("not found in notebook"));
    }
}
