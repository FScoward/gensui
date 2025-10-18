use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub workflows: Vec<Workflow>,
    #[serde(default)]
    pub default_workflow: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Workflow {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub steps: Vec<WorkflowStep>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WorkflowStep {
    pub name: String,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub claude: Option<ClaudeStep>,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
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
        }
    }
}

impl Workflow {
    pub fn steps(&self) -> &[WorkflowStep] {
        &self.steps
    }
}
