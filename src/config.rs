use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub workflows: Vec<Workflow>,
    #[serde(default)]
    pub default_workflow: Option<String>,
    /// Global default for sandbox mode.
    /// If true (default), Claude Code will run in sandbox mode unless explicitly disabled.
    /// Individual workflow steps can override this setting.
    #[serde(default = "default_sandbox_mode")]
    pub default_sandbox_mode: bool,
}

fn default_sandbox_mode() -> bool {
    true // Security-first: enable sandbox by default
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Workflow {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub steps: Vec<WorkflowStep>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WorkflowStep {
    pub name: String,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub claude: Option<ClaudeStep>,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct ClaudeStep {
    pub prompt: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub allowed_tools: Option<Vec<String>>,
    #[serde(default)]
    pub permission_mode: Option<String>,
    #[serde(default)]
    pub extra_args: Option<Vec<String>>,
    /// Enable sandbox mode to restrict file system access to worktree.
    /// Default: None (inherits from global config, which defaults to true)
    #[serde(default)]
    pub sandbox_mode: Option<bool>,
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        if path.exists() {
            let data = fs::read_to_string(path)
                .with_context(|| format!("failed to read config file {}", path.display()))?;
            let config: Self = serde_json::from_str(&data)
                .with_context(|| format!("failed to parse config file {}", path.display()))?;
            if config.workflows.is_empty() {
                Ok(Self::default())
            } else {
                Ok(config)
            }
        } else {
            Ok(Self::default())
        }
    }

    pub fn default_workflow<'a>(&'a self) -> &'a Workflow {
        if let Some(default_name) = &self.default_workflow {
            if let Some(workflow) = self.workflows.iter().find(|wf| &wf.name == default_name) {
                return workflow;
            }
        }
        self.workflows
            .get(0)
            .expect("config must always have at least one workflow")
    }

    pub fn workflow_by_name(&self, name: &str) -> Option<&Workflow> {
        self.workflows.iter().find(|wf| wf.name == name)
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            workflows: vec![Workflow {
                name: "default".to_string(),
                description: Some("標準的な分析→実装→テストの3ステップ".to_string()),
                steps: vec![
                    WorkflowStep {
                        name: "分析".to_string(),
                        command: Some("echo 'Analyzing issue context'".to_string()),
                        claude: None,
                        description: Some("Issue内容の分析を実施".to_string()),
                    },
                    WorkflowStep {
                        name: "実装".to_string(),
                        command: Some("echo 'Implementing changes'".to_string()),
                        claude: None,
                        description: Some("コード変更を適用".to_string()),
                    },
                    WorkflowStep {
                        name: "テスト".to_string(),
                        command: Some("echo 'Running tests'".to_string()),
                        claude: None,
                        description: Some("テストスイートを実行".to_string()),
                    },
                ],
            }],
            default_workflow: Some("default".to_string()),
            default_sandbox_mode: default_sandbox_mode(),
        }
    }
}

impl Workflow {
    pub fn steps(&self) -> &[WorkflowStep] {
        &self.steps
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_sandbox_mode_is_true() {
        assert_eq!(default_sandbox_mode(), true);
    }

    #[test]
    fn test_config_default_has_sandbox_enabled() {
        let config = Config::default();
        assert_eq!(config.default_sandbox_mode, true);
    }

    #[test]
    fn test_config_deserialize_with_sandbox_mode() {
        let json = r#"{
            "default_workflow": "test",
            "default_sandbox_mode": false,
            "workflows": [
                {
                    "name": "test",
                    "steps": []
                }
            ]
        }"#;

        let config: Config = serde_json::from_str(json).unwrap();
        assert_eq!(config.default_sandbox_mode, false);
    }

    #[test]
    fn test_config_deserialize_without_sandbox_mode_defaults_to_true() {
        let json = r#"{
            "default_workflow": "test",
            "workflows": [
                {
                    "name": "test",
                    "steps": []
                }
            ]
        }"#;

        let config: Config = serde_json::from_str(json).unwrap();
        assert_eq!(config.default_sandbox_mode, true);
    }

    #[test]
    fn test_claude_step_sandbox_mode_none() {
        let json = r#"{
            "prompt": "test prompt"
        }"#;

        let step: ClaudeStep = serde_json::from_str(json).unwrap();
        assert_eq!(step.sandbox_mode, None);
    }

    #[test]
    fn test_claude_step_sandbox_mode_true() {
        let json = r#"{
            "prompt": "test prompt",
            "sandbox_mode": true
        }"#;

        let step: ClaudeStep = serde_json::from_str(json).unwrap();
        assert_eq!(step.sandbox_mode, Some(true));
    }

    #[test]
    fn test_claude_step_sandbox_mode_false() {
        let json = r#"{
            "prompt": "test prompt",
            "sandbox_mode": false
        }"#;

        let step: ClaudeStep = serde_json::from_str(json).unwrap();
        assert_eq!(step.sandbox_mode, Some(false));
    }

    #[test]
    fn test_sandbox_mode_inheritance() {
        // Test the intended behavior: step-level overrides global default
        let global_default = true;

        // Case 1: Step has no sandbox_mode, should inherit global default
        let step_none: Option<bool> = None;
        assert_eq!(step_none.unwrap_or(global_default), true);

        // Case 2: Step explicitly sets sandbox_mode to false
        let step_false: Option<bool> = Some(false);
        assert_eq!(step_false.unwrap_or(global_default), false);

        // Case 3: Step explicitly sets sandbox_mode to true
        let step_true: Option<bool> = Some(true);
        assert_eq!(step_true.unwrap_or(global_default), true);
    }
}
