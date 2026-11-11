use crate::permissions::{PermissionMode, PermissionPolicy};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum McpServerEntry {
    Stdio {
        command: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        env: HashMap<String, String>,
    },
    Http {
        url: String,
        #[serde(default)]
        headers: HashMap<String, String>,
    },
}

#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ProviderConfig {
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
}

impl std::fmt::Debug for ProviderConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderConfig")
            .field("api_key", &self.api_key.as_ref().map(|_| "[REDACTED]"))
            .field("base_url", &self.base_url)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentConfig {
    #[serde(default = "default_provider")]
    pub default_provider: String,
    #[serde(default = "default_model")]
    pub default_model: String,
    #[serde(default)]
    pub providers: HashMap<String, ProviderConfig>,
    #[serde(default)]
    pub permissions: PermissionPolicy,
    #[serde(default)]
    pub mcp_servers: HashMap<String, McpServerEntry>,
}

fn default_provider() -> String {
    "anthropic".to_string()
}

fn default_model() -> String {
    "ferric-pro".to_string()
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            default_provider: default_provider(),
            default_model: default_model(),
            providers: HashMap::new(),
            permissions: PermissionPolicy::default(),
            mcp_servers: HashMap::new(),
        }
    }
}

impl AgentConfig {
    /// Loads configuration merging:
    /// Global config (~/.agent.json) -> Project config (.agent/settings.json) -> Environment variables
    pub fn load(workspace_root: &Path) -> Self {
        let mut config = Self::default();

        // 1. Try global user config (~/.agent.json, ~/.ferric.json, or fallback ~/.claude.json)
        if let Some(user_home) = home_dir() {
            let candidates = [
                user_home.join(".agent.json"),
                user_home.join(".ferric.json"),
                user_home.join(".claude.json"),
            ];
            for path in &candidates {
                if let Ok(content) = std::fs::read_to_string(path) {
                    if let Ok(global_cfg) = serde_json::from_str::<AgentConfig>(&content) {
                        config.merge_from(global_cfg);
                        break;
                    }
                }
            }
        }

        // 2. Try project workspace config (.agent/settings.json, .ferric/settings.json, or fallback .claude/settings.json)
        let project_candidates = [
            workspace_root.join(".agent").join("settings.json"),
            workspace_root.join(".ferric").join("settings.json"),
            workspace_root.join(".claude").join("settings.json"),
        ];
        for path in &project_candidates {
            if let Ok(content) = std::fs::read_to_string(path) {
                if let Ok(project_cfg) = serde_json::from_str::<AgentConfig>(&content) {
                    config.merge_from(project_cfg);
                    break;
                }
            }
        }

        // 3. Environment variable overrides (highest precedence)
        config.apply_environment_overrides();

        config
    }

    /// Merges another config over this one (values in `other` take precedence)
    pub fn merge_from(&mut self, other: AgentConfig) {
        if other.default_provider != default_provider() {
            self.default_provider = other.default_provider;
        }
        if other.default_model != default_model() {
            self.default_model = other.default_model;
        }

        for (k, v) in other.providers {
            let entry = self.providers.entry(k).or_default();
            if v.api_key.is_some() {
                entry.api_key = v.api_key;
            }
            if v.base_url.is_some() {
                entry.base_url = v.base_url;
            }
        }

        if other.permissions.default_mode != PermissionMode::Default {
            let can_override = match (
                self.permissions.default_mode,
                other.permissions.default_mode,
            ) {
                // If user config is already BypassPermissions, other can set BypassPermissions
                (PermissionMode::BypassPermissions, PermissionMode::BypassPermissions) => true,
                // Disallow escalating to BypassPermissions if user config is not already BypassPermissions
                (_, PermissionMode::BypassPermissions) => {
                    tracing::warn!("Ignoring attempt to escalate permissions to BypassPermissions");
                    false
                }
                // Disallow escalating to DontAsk if user config is not already DontAsk or BypassPermissions
                (PermissionMode::DontAsk, PermissionMode::DontAsk) => true,
                (PermissionMode::BypassPermissions, PermissionMode::DontAsk) => true,
                (_, PermissionMode::DontAsk) => {
                    tracing::warn!("Ignoring attempt to escalate permissions to DontAsk");
                    false
                }
                // If user is in Plan mode (read-only), do not allow weakening to more permissive mode
                (PermissionMode::Plan, mode) if mode != PermissionMode::Plan => {
                    tracing::warn!("Ignoring attempt to weaken restrictive Plan mode");
                    false
                }
                _ => true,
            };
            if can_override {
                self.permissions.default_mode = other.permissions.default_mode;
            }
        }
        for allow_rule in other.permissions.allow {
            if !self.permissions.allow.contains(&allow_rule) {
                self.permissions.allow.push(allow_rule);
            }
        }
        // Security constraint: Untrusted workspace/imported rules cannot remove existing deny rules
        for deny_rule in other.permissions.deny {
            if !self.permissions.deny.contains(&deny_rule) {
                self.permissions.deny.push(deny_rule);
            }
        }
        for ask_rule in other.permissions.ask {
            if !self.permissions.ask.contains(&ask_rule) {
                self.permissions.ask.push(ask_rule);
            }
        }

        for (k, v) in other.mcp_servers {
            self.mcp_servers.insert(k, v);
        }
    }

    /// Overrides configuration from well-known environment variables
    pub fn apply_environment_overrides(&mut self) {
        // Anthropic
        if let Ok(val) = std::env::var("ANTHROPIC_API_KEY") {
            let p = self.providers.entry("anthropic".into()).or_default();
            p.api_key = Some(val);
        }
        if let Ok(val) = std::env::var("ANTHROPIC_BASE_URL") {
            let p = self.providers.entry("anthropic".into()).or_default();
            p.base_url = Some(val);
        }

        // OpenAI
        if let Ok(val) = std::env::var("OPENAI_API_KEY") {
            let p = self.providers.entry("openai".into()).or_default();
            p.api_key = Some(val);
        }
        if let Ok(val) = std::env::var("OPENAI_BASE_URL") {
            let p = self.providers.entry("openai".into()).or_default();
            p.base_url = Some(val);
        }

        // Gemini
        if let Ok(val) = std::env::var("GEMINI_API_KEY") {
            let p = self.providers.entry("gemini".into()).or_default();
            p.api_key = Some(val);
        }
        if let Ok(val) = std::env::var("GEMINI_BASE_URL") {
            let p = self.providers.entry("gemini".into()).or_default();
            p.base_url = Some(val);
        }

        // Ollama
        if let Ok(val) = std::env::var("OLLAMA_BASE_URL") {
            let p = self.providers.entry("ollama".into()).or_default();
            p.base_url = Some(val);
        }

        // Global model and provider overrides
        if let Ok(val) = std::env::var("AGENT_DEFAULT_MODEL")
            .or_else(|_| std::env::var("FERRIC_DEFAULT_MODEL"))
            .or_else(|_| std::env::var("CLAUDE_DEFAULT_MODEL"))
        {
            self.default_model = val;
        }
        if let Ok(val) = std::env::var("AGENT_DEFAULT_PROVIDER")
            .or_else(|_| std::env::var("FERRIC_DEFAULT_PROVIDER"))
            .or_else(|_| std::env::var("CLAUDE_DEFAULT_PROVIDER"))
        {
            self.default_provider = val;
        }
    }

    /// Resolves an API key for a provider, falling back to empty string if unset
    pub fn get_api_key(&self, provider: &str) -> String {
        self.providers
            .get(provider)
            .and_then(|p| p.api_key.clone())
            .unwrap_or_default()
    }

    /// Resolves custom base URL for a provider if configured
    pub fn get_base_url(&self, provider: &str) -> Option<String> {
        self.providers
            .get(provider)
            .and_then(|p| p.base_url.clone())
    }

    /// Saves the current configuration to a JSON file
    pub fn save_to(&self, path: &Path) -> Result<(), std::io::Error> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let serialized = serde_json::to_string_pretty(self)?;
        std::fs::write(path, serialized)?;
        Ok(())
    }
}

fn home_dir() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        std::env::var("USERPROFILE")
            .ok()
            .map(PathBuf::from)
            .or_else(|| {
                let drive = std::env::var("HOMEDRIVE").ok()?;
                let path = std::env::var("HOMEPATH").ok()?;
                Some(PathBuf::from(format!("{drive}{path}")))
            })
    }
    #[cfg(not(target_os = "windows"))]
    {
        std::env::var("HOME").ok().map(PathBuf::from)
    }
}

/// Redacts known API keys and sensitive tokens from error messages, diagnostic logs, and traces.
pub fn redact_secrets(input: &str, secrets: &[&str]) -> String {
    let mut sanitized = input.to_string();
    for secret in secrets {
        if !secret.is_empty() && secret.len() >= 6 {
            sanitized = sanitized.replace(secret, "[REDACTED]");
        }
    }
    sanitized
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_secret_redaction_in_debug_and_logs() {
        let p = ProviderConfig {
            api_key: Some("sk-ant-api03-verysecretkey123456789".into()),
            base_url: Some("https://api.anthropic.com".into()),
        };

        let debug_str = format!("{p:?}");
        assert!(!debug_str.contains("verysecretkey"));
        assert!(debug_str.contains("[REDACTED]"));

        let log_msg = "Error connecting with key sk-ant-api03-verysecretkey123456789 to endpoint";
        let clean = redact_secrets(log_msg, &[p.api_key.as_ref().unwrap()]);
        assert!(!clean.contains("verysecretkey"));
        assert!(clean.contains("[REDACTED]"));
    }

    #[test]
    fn test_config_serialization_roundtrip() {
        let mut config = AgentConfig {
            default_provider: "openai".into(),
            default_model: "gpt-4o".into(),
            ..Default::default()
        };
        config.providers.insert(
            "openai".into(),
            ProviderConfig {
                api_key: Some("sk-test123".into()),
                base_url: Some("https://api.openai.com/v1".into()),
            },
        );
        config.mcp_servers.insert(
            "local_git".into(),
            McpServerEntry::Stdio {
                command: "git-mcp".into(),
                args: vec!["--verbose".into()],
                env: HashMap::new(),
            },
        );

        let serialized = serde_json::to_string(&config).unwrap();
        let parsed: AgentConfig = serde_json::from_str(&serialized).unwrap();

        assert_eq!(parsed.default_provider, "openai");
        assert_eq!(parsed.default_model, "gpt-4o");
        assert_eq!(parsed.get_api_key("openai"), "sk-test123");
        assert!(parsed.mcp_servers.contains_key("local_git"));
    }

    #[test]
    fn test_config_merge_and_security_deny_retention() {
        let mut base = AgentConfig::default();
        base.permissions.deny.push("Bash(rm -rf *)".into());

        let mut workspace_cfg = AgentConfig {
            default_model: "ferric-opus".into(),
            ..Default::default()
        };
        workspace_cfg
            .permissions
            .allow
            .push("Bash(git status)".into());

        base.merge_from(workspace_cfg);
        assert_eq!(base.default_model, "ferric-opus");
        assert!(base
            .permissions
            .allow
            .contains(&"Bash(git status)".to_string()));
        // Security guarantee: base deny rule must still be strictly enforced
        assert!(base
            .permissions
            .deny
            .contains(&"Bash(rm -rf *)".to_string()));
    }

    #[test]
    fn test_prevent_workspace_permission_escalation() {
        // 1. Base config has Default mode; workspace attempts to set BypassPermissions
        let mut base_default = AgentConfig::default();
        let mut malicious_workspace = AgentConfig::default();
        malicious_workspace.permissions.default_mode = PermissionMode::BypassPermissions;

        base_default.merge_from(malicious_workspace.clone());
        assert_eq!(
            base_default.permissions.default_mode,
            PermissionMode::Default,
            "Workspace config must NOT escalate default_mode from Default to BypassPermissions"
        );

        // 2. Base config has Plan mode; workspace attempts to set BypassPermissions
        let mut base_plan = AgentConfig {
            permissions: PermissionPolicy {
                default_mode: PermissionMode::Plan,
                ..Default::default()
            },
            ..Default::default()
        };
        base_plan.merge_from(malicious_workspace.clone());
        assert_eq!(
            base_plan.permissions.default_mode,
            PermissionMode::Plan,
            "Workspace config must NOT escalate default_mode from Plan to BypassPermissions"
        );

        // 3. Base config has Plan mode; workspace attempts to weaken to AcceptEdits
        let mut workspace_accept_edits = AgentConfig::default();
        workspace_accept_edits.permissions.default_mode = PermissionMode::AcceptEdits;
        base_plan.merge_from(workspace_accept_edits);
        assert_eq!(
            base_plan.permissions.default_mode,
            PermissionMode::Plan,
            "Workspace config must NOT weaken Plan mode to AcceptEdits"
        );

        // 4. Base config has Default; workspace tightens to Plan
        let mut workspace_plan = AgentConfig::default();
        workspace_plan.permissions.default_mode = PermissionMode::Plan;
        base_default.merge_from(workspace_plan);
        assert_eq!(
            base_default.permissions.default_mode,
            PermissionMode::Plan,
            "Workspace config CAN tighten Default to Plan"
        );

        // 5. User explicitly configured BypassPermissions; merging retains BypassPermissions
        let mut base_bypass = AgentConfig {
            permissions: PermissionPolicy {
                default_mode: PermissionMode::BypassPermissions,
                ..Default::default()
            },
            ..Default::default()
        };
        base_bypass.merge_from(malicious_workspace);
        assert_eq!(
            base_bypass.permissions.default_mode,
            PermissionMode::BypassPermissions,
            "User-level BypassPermissions is retained when workspace also has BypassPermissions"
        );
    }
}
