use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PermissionMode {
    #[default]
    Default,
    Plan,
    AcceptEdits,
    BypassPermissions,
    DontAsk,
    Auto,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionRule {
    pub pattern: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PermissionPolicy {
    #[serde(default)]
    pub default_mode: PermissionMode,
    #[serde(default)]
    pub allow: Vec<String>,
    #[serde(default)]
    pub deny: Vec<String>,
    #[serde(default)]
    pub ask: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionDecision {
    Allow,
    Deny,
    Ask,
}

impl PermissionPolicy {
    pub fn new(mode: PermissionMode) -> Self {
        Self {
            default_mode: mode,
            allow: Vec::new(),
            deny: Vec::new(),
            ask: Vec::new(),
        }
    }

    pub fn permissive() -> Self {
        Self::new(PermissionMode::BypassPermissions)
    }

    /// Evaluates permission for a specific tool and action pattern (e.g., "Bash(git commit)", "FileWrite(src/main.rs)")
    pub fn evaluate(&self, tool_name: &str, target: &str) -> PermissionDecision {
        let pattern = format!("{tool_name}({target})");

        // 1. Check explicit deny rules first (highest priority)
        if self.matches_any(&pattern, &self.deny) || self.matches_any(tool_name, &self.deny) {
            return PermissionDecision::Deny;
        }

        // 2. Check explicit ask rules
        if self.matches_any(&pattern, &self.ask) || self.matches_any(tool_name, &self.ask) {
            return PermissionDecision::Ask;
        }

        // 3. Check explicit allow rules
        if self.matches_any(&pattern, &self.allow) || self.matches_any(tool_name, &self.allow) {
            return PermissionDecision::Allow;
        }

        // 4. Default mode fallback
        match self.default_mode {
            PermissionMode::BypassPermissions | PermissionMode::DontAsk => {
                PermissionDecision::Allow
            }
            PermissionMode::Plan => {
                if is_read_only_tool(tool_name) {
                    PermissionDecision::Allow
                } else {
                    PermissionDecision::Deny
                }
            }
            PermissionMode::AcceptEdits => {
                if is_read_only_tool(tool_name) || is_file_edit_tool(tool_name) {
                    PermissionDecision::Allow
                } else {
                    PermissionDecision::Ask
                }
            }
            PermissionMode::Default | PermissionMode::Auto => {
                if is_read_only_tool(tool_name) {
                    PermissionDecision::Allow
                } else {
                    PermissionDecision::Ask
                }
            }
        }
    }

    fn matches_any(&self, target: &str, rules: &[String]) -> bool {
        for rule in rules {
            if wildcard_match(rule, target) {
                return true;
            }
        }
        false
    }
}

pub fn wildcard_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0, 0);
    let (mut star_idx, mut match_idx) = (None, 0);

    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star_idx = Some(pi);
            pi += 1;
            match_idx = ti;
        } else if let Some(star) = star_idx {
            pi = star + 1;
            match_idx += 1;
            ti = match_idx;
        } else {
            return false;
        }
    }

    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }

    pi == p.len()
}

fn is_read_only_tool(tool_name: &str) -> bool {
    matches!(
        tool_name,
        "FileRead"
            | "Glob"
            | "Grep"
            | "WebSearch"
            | "WebFetch"
            | "ListMcpResources"
            | "ReadMcpResource"
            | "GitStatus"
            | "GitDiff"
    )
}

fn is_file_edit_tool(tool_name: &str) -> bool {
    matches!(
        tool_name,
        "FileEdit" | "FileWrite" | "NotebookEdit" | "PatchApply"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_read_only_allowed_by_default() {
        let policy = PermissionPolicy::default();
        assert_eq!(
            policy.evaluate("FileRead", "Cargo.toml"),
            PermissionDecision::Allow
        );
        assert_eq!(
            policy.evaluate("Glob", "**/*.rs"),
            PermissionDecision::Allow
        );
    }

    #[test]
    fn test_mutating_actions_ask_by_default() {
        let policy = PermissionPolicy::default();
        assert_eq!(policy.evaluate("Bash", "rm -rf ."), PermissionDecision::Ask);
        assert_eq!(
            policy.evaluate("FileWrite", "config.json"),
            PermissionDecision::Ask
        );
    }

    #[test]
    fn test_explicit_allow_and_deny() {
        let mut policy = PermissionPolicy::default();
        policy.allow.push("Bash(git status*)".into());
        policy.deny.push("Bash(rm *)".into());

        assert_eq!(
            policy.evaluate("Bash", "git status"),
            PermissionDecision::Allow
        );
        assert_eq!(
            policy.evaluate("Bash", "rm -rf /"),
            PermissionDecision::Deny
        );
    }
}
