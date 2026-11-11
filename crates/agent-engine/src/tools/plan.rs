use super::Tool;
use agent_core::ToolDefinition;
use async_trait::async_trait;
use serde_json::json;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::RwLock;

static PLAN_MODE_ACTIVE: AtomicBool = AtomicBool::new(false);
static PLAN_MODE_REASON: RwLock<Option<String>> = RwLock::new(None);
static FINALIZED_PLAN: RwLock<Option<String>> = RwLock::new(None);

pub fn is_plan_mode_active() -> bool {
    PLAN_MODE_ACTIVE.load(Ordering::SeqCst)
}

pub fn set_plan_mode_active(active: bool) {
    PLAN_MODE_ACTIVE.store(active, Ordering::SeqCst);
}

pub fn get_plan_reason() -> Option<String> {
    PLAN_MODE_REASON.read().unwrap().clone()
}

pub fn get_finalized_plan() -> Option<String> {
    FINALIZED_PLAN.read().unwrap().clone()
}

pub fn reset_plan_mode() {
    PLAN_MODE_ACTIVE.store(false, Ordering::SeqCst);
    *PLAN_MODE_REASON.write().unwrap() = None;
    *FINALIZED_PLAN.write().unwrap() = None;
}

#[derive(Debug, Clone, Default)]
pub struct PlanModeState;

pub fn plan_mode_tools() -> (EnterPlanModeTool, ExitPlanModeTool, PlanModeState) {
    (EnterPlanModeTool, ExitPlanModeTool, PlanModeState)
}

// ---------------------------------------------------------------------------
// EnterPlanMode Tool (also aliased as PlanModeTool)
// ---------------------------------------------------------------------------

pub struct EnterPlanModeTool;
pub type PlanModeTool = EnterPlanModeTool;

impl Default for EnterPlanModeTool {
    fn default() -> Self {
        Self
    }
}

impl EnterPlanModeTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for EnterPlanModeTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "EnterPlanMode".into(),
            description: "Transition into plan mode. While in plan mode, mutating tools are restricted so you can safely inspect files and design an implementation plan."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "reason": {
                        "type": "string",
                        "description": "Optional explanation for entering plan mode"
                    }
                }
            }),
        }
    }

    async fn execute(
        &self,
        input: serde_json::Value,
        _workspace_root: &Path,
    ) -> Result<String, String> {
        let reason = input["reason"].as_str().map(|s| s.to_string());
        PLAN_MODE_ACTIVE.store(true, Ordering::SeqCst);
        *PLAN_MODE_REASON.write().unwrap() = reason.clone();

        let reason_msg = reason.map(|r| format!(" Reason: {r}.")).unwrap_or_default();

        Ok(format!(
            "Entered plan mode.{reason_msg} Mutating actions are now restricted. Explore workspace files, analyze requirements, and call ExitPlanMode when ready to execute."
        ))
    }
}

// ---------------------------------------------------------------------------
// ExitPlanMode Tool
// ---------------------------------------------------------------------------

pub struct ExitPlanModeTool;

impl Default for ExitPlanModeTool {
    fn default() -> Self {
        Self
    }
}

impl ExitPlanModeTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for ExitPlanModeTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "ExitPlanMode".into(),
            description: "Exit plan mode after planning is complete, restoring execution permissions so you can apply file modifications and run commands."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "plan": {
                        "type": "string",
                        "description": "The finalized implementation plan or summary of steps to execute"
                    }
                }
            }),
        }
    }

    async fn execute(
        &self,
        input: serde_json::Value,
        _workspace_root: &Path,
    ) -> Result<String, String> {
        let plan = input["plan"].as_str().map(|s| s.to_string());
        PLAN_MODE_ACTIVE.store(false, Ordering::SeqCst);
        *FINALIZED_PLAN.write().unwrap() = plan.clone();

        let plan_summary = plan
            .map(|p| format!("\nFinalized Plan:\n{p}"))
            .unwrap_or_default();

        Ok(format!(
            "Exited plan mode. Execution permissions restored. Ready to proceed with implementation.{plan_summary}"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_plan_mode_state_transitions() {
        reset_plan_mode();
        let dummy_root = Path::new(".");

        assert!(!is_plan_mode_active());

        // 1. Enter plan mode
        let enter_tool = EnterPlanModeTool;
        let enter_res = enter_tool
            .execute(
                json!({ "reason": "Analyze database schema changes" }),
                dummy_root,
            )
            .await
            .unwrap();
        assert!(enter_res.contains("Entered plan mode"));
        assert!(is_plan_mode_active());
        assert_eq!(
            get_plan_reason().as_deref(),
            Some("Analyze database schema changes")
        );

        // 2. Exit plan mode with finalized plan
        let exit_tool = ExitPlanModeTool;
        let exit_res = exit_tool
            .execute(
                json!({ "plan": "1. Add column\n2. Run migration" }),
                dummy_root,
            )
            .await
            .unwrap();
        assert!(exit_res.contains("Exited plan mode"));
        assert!(!is_plan_mode_active());
        assert_eq!(
            get_finalized_plan().as_deref(),
            Some("1. Add column\n2. Run migration")
        );
    }
}
