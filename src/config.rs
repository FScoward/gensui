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
    pub command: String,
    #[serde(default)]
    pub description: Option<String>,
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
                        command: "echo 'Analyzing issue context'".to_string(),
                        description: Some("Issue内容の分析を実施".to_string()),
                    },
                    WorkflowStep {
                        name: "実装".to_string(),
                        command: "echo 'Implementing changes'".to_string(),
                        description: Some("コード変更を適用".to_string()),
                    },
                    WorkflowStep {
                        name: "テスト".to_string(),
                        command: "echo 'Running tests'".to_string(),
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
