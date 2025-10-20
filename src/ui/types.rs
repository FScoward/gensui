/// UI関連の型定義

/// ログビューのモード
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogViewMode {
    Overview,
    Detail,
    Raw,
}

/// ステップの実行ステータス
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepStatus {
    Running,
    Success,
    Failed,
}

/// 構造化されたログエントリ
#[derive(Debug, Clone)]
pub struct LogEntry {
    pub step_index: usize,
    pub step_name: String,
    pub prompt_lines: Vec<String>,
    pub result_lines: Vec<String>,
    pub thought_lines: Vec<String>,
    pub status: StepStatus,
}

/// Claude Codeで利用可能なツールの定義
#[derive(Debug, Clone, Copy)]
pub struct ToolDef {
    pub name: &'static str,
    pub description: &'static str,
}

/// Claude Codeで利用可能なツール一覧
pub const AVAILABLE_TOOLS: &[ToolDef] = &[
    ToolDef {
        name: "Read",
        description: "ファイル読み取り",
    },
    ToolDef {
        name: "Write",
        description: "ファイル書き込み",
    },
    ToolDef {
        name: "Edit",
        description: "ファイル編集",
    },
    ToolDef {
        name: "Glob",
        description: "ファイル検索",
    },
    ToolDef {
        name: "Grep",
        description: "コード検索",
    },
    ToolDef {
        name: "Bash",
        description: "シェルコマンド実行",
    },
    ToolDef {
        name: "WebFetch",
        description: "Web取得",
    },
    ToolDef {
        name: "NotebookEdit",
        description: "Jupyter編集",
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_log_view_mode_equality() {
        assert_eq!(LogViewMode::Overview, LogViewMode::Overview);
        assert_ne!(LogViewMode::Overview, LogViewMode::Detail);
        assert_ne!(LogViewMode::Detail, LogViewMode::Raw);
    }

    #[test]
    fn test_step_status_equality() {
        assert_eq!(StepStatus::Running, StepStatus::Running);
        assert_ne!(StepStatus::Success, StepStatus::Failed);
    }

    #[test]
    fn test_available_tools_count() {
        assert_eq!(AVAILABLE_TOOLS.len(), 8);
    }

    #[test]
    fn test_available_tools_names() {
        let tool_names: Vec<&str> = AVAILABLE_TOOLS.iter().map(|t| t.name).collect();
        assert!(tool_names.contains(&"Read"));
        assert!(tool_names.contains(&"Write"));
        assert!(tool_names.contains(&"Edit"));
        assert!(tool_names.contains(&"Bash"));
    }

    #[test]
    fn test_log_entry_creation() {
        let entry = LogEntry {
            step_index: 1,
            step_name: "Test Step".to_string(),
            prompt_lines: vec!["prompt".to_string()],
            result_lines: vec!["result".to_string()],
            thought_lines: vec!["thought".to_string()],
            status: StepStatus::Success,
        };
        assert_eq!(entry.step_index, 1);
        assert_eq!(entry.step_name, "Test Step");
        assert_eq!(entry.status, StepStatus::Success);
    }
}
