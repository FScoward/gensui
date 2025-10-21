use std::collections::VecDeque;

use crate::log_parser;
use crate::state::{SessionEvent, SessionHistory};
use crate::ui::{types::StepStatus, LogEntry};
use crate::worker::WorkerSnapshot;

/// View model for a worker, including logs and structured data
pub struct WorkerView {
    pub snapshot: WorkerSnapshot,
    pub logs: VecDeque<String>,
    pub structured_logs: Vec<LogEntry>,
    #[allow(dead_code)]
    pub session_histories: Vec<SessionHistory>,
    // Parser
    log_parser: log_parser::LogParser,
}

impl WorkerView {
    const LOG_CAPACITY: usize = 128;

    pub fn new(snapshot: WorkerSnapshot) -> Self {
        Self {
            snapshot,
            logs: VecDeque::with_capacity(Self::LOG_CAPACITY),
            structured_logs: Vec::new(),
            session_histories: Vec::new(),
            log_parser: log_parser::LogParser::new(),
        }
    }

    #[allow(dead_code)]
    pub fn set_session_histories(&mut self, histories: Vec<SessionHistory>) {
        self.session_histories = histories;
    }

    pub fn update_snapshot(&mut self, snapshot: WorkerSnapshot) {
        self.snapshot = snapshot;
    }

    pub fn push_log(&mut self, line: String) {
        if self.logs.len() >= Self::LOG_CAPACITY {
            self.logs.pop_front();
        }
        self.logs.push_back(line.clone());

        // Parse structured log markers using log_parser
        if let Some(entry) = self.log_parser.parse_line(&line) {
            self.structured_logs.push(entry);
        }
    }

    /// SessionHistoryをLogEntryに変換してstructured_logsに追加
    pub fn add_session_history_logs(&mut self, history: &SessionHistory) {
        let entries = Self::convert_session_to_log_entries(history);
        self.structured_logs.extend(entries);
    }

    /// SessionHistoryをLogEntry列に変換
    fn convert_session_to_log_entries(history: &SessionHistory) -> Vec<LogEntry> {
        let mut entries = Vec::new();
        let mut step_index = 0;
        let events = &history.events;

        let mut i = 0;
        while i < events.len() {
            if let SessionEvent::ToolUse { name, .. } = &events[i] {
                let step_name = name.clone();
                let mut prompt_lines = Vec::new();
                let mut thought_lines = Vec::new();
                let mut result_lines = Vec::new();

                // 直前のAssistantMessage/ThinkingBlockを収集
                if i > 0 {
                    let mut j = i - 1;
                    loop {
                        match &events[j] {
                            SessionEvent::AssistantMessage { text, .. } => {
                                // 複数行のテキストを分割
                                for line in text.lines().rev() {
                                    prompt_lines.insert(0, line.to_string());
                                }
                            }
                            SessionEvent::ThinkingBlock { content, .. } => {
                                // 複数行のテキストを分割
                                for line in content.lines().rev() {
                                    thought_lines.insert(0, line.to_string());
                                }
                            }
                            SessionEvent::ToolUse { .. } => {
                                // 前のToolUseに到達したら終了
                                break;
                            }
                            _ => {}
                        }

                        if j == 0 {
                            break;
                        }
                        j -= 1;
                    }
                }

                // 次のToolResultを探す
                let mut k = i + 1;
                while k < events.len() {
                    match &events[k] {
                        SessionEvent::ToolResult {
                            name: result_name,
                            output,
                            ..
                        } => {
                            if result_name == &step_name {
                                if let Some(output_text) = output {
                                    result_lines = output_text
                                        .lines()
                                        .map(|s| s.to_string())
                                        .collect();
                                }
                                break;
                            }
                        }
                        SessionEvent::ToolUse { .. } => {
                            // 次のToolUseに到達したら終了
                            break;
                        }
                        _ => {}
                    }
                    k += 1;
                }

                // LogEntryを作成
                entries.push(LogEntry {
                    step_index,
                    step_name,
                    prompt_lines,
                    result_lines,
                    thought_lines,
                    status: StepStatus::Success, // ToolResultがあればSuccess
                });

                step_index += 1;
            }
            i += 1;
        }

        // ToolUseが1つもない場合は、セッション全体を1つのLogEntryとして作成
        if entries.is_empty() && !events.is_empty() {
            let mut prompt_lines = Vec::new();
            let mut thought_lines = Vec::new();
            let result_lines = Vec::new();

            // すべてのイベントからAssistantMessageとThinkingBlockを収集
            for event in events {
                match event {
                    SessionEvent::AssistantMessage { text, .. } => {
                        for line in text.lines() {
                            prompt_lines.push(line.to_string());
                        }
                    }
                    SessionEvent::ThinkingBlock { content, .. } => {
                        for line in content.lines() {
                            thought_lines.push(line.to_string());
                        }
                    }
                    _ => {}
                }
            }

            // プロンプトまたは思考内容がある場合のみLogEntryを作成
            if !prompt_lines.is_empty() || !thought_lines.is_empty() {
                entries.push(LogEntry {
                    step_index: 0,
                    step_name: "Session".to_string(),
                    prompt_lines,
                    result_lines,
                    thought_lines,
                    status: StepStatus::Success,
                });
            }
        }

        // Debug: created entries.len() LogEntry(ies) from session history.session_id

        entries
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::SessionEvent;

    #[test]
    fn test_convert_session_to_log_entries() {
        // テスト用のSessionHistoryを作成
        let history = SessionHistory {
            session_id: "test-session".to_string(),
            started_at: "2024-01-01T00:00:00Z".to_string(),
            ended_at: Some("2024-01-01T00:10:00Z".to_string()),
            prompt: "Test prompt".to_string(),
            events: vec![
                SessionEvent::AssistantMessage {
                    text: "I'll read the file".to_string(),
                    timestamp: "2024-01-01T00:01:00Z".to_string(),
                },
                SessionEvent::ToolUse {
                    name: "Read".to_string(),
                    timestamp: "2024-01-01T00:02:00Z".to_string(),
                    input: None,
                },
                SessionEvent::ToolResult {
                    name: "Read".to_string(),
                    timestamp: "2024-01-01T00:03:00Z".to_string(),
                    output: Some("File contents here\nLine 2".to_string()),
                },
                SessionEvent::ThinkingBlock {
                    content: "Thinking about the file".to_string(),
                    timestamp: "2024-01-01T00:04:00Z".to_string(),
                },
                SessionEvent::ToolUse {
                    name: "Write".to_string(),
                    timestamp: "2024-01-01T00:05:00Z".to_string(),
                    input: None,
                },
                SessionEvent::ToolResult {
                    name: "Write".to_string(),
                    timestamp: "2024-01-01T00:06:00Z".to_string(),
                    output: Some("File written successfully".to_string()),
                },
            ],
            total_tool_uses: 2,
            files_modified: vec!["test.txt".to_string()],
        };

        let entries = WorkerView::convert_session_to_log_entries(&history);

        // 2つのツール使用があるので2つのエントリが作成されるはず
        assert_eq!(entries.len(), 2);

        // 最初のエントリ（Read）
        assert_eq!(entries[0].step_index, 0);
        assert_eq!(entries[0].step_name, "Read");
        assert_eq!(entries[0].prompt_lines, vec!["I'll read the file"]);
        assert_eq!(
            entries[0].result_lines,
            vec!["File contents here", "Line 2"]
        );
        assert_eq!(entries[0].status, StepStatus::Success);

        // 2番目のエントリ（Write）
        assert_eq!(entries[1].step_index, 1);
        assert_eq!(entries[1].step_name, "Write");
        // Writeの前にThinkingBlockがあるので、それがthought_linesに含まれる
        assert_eq!(entries[1].thought_lines, vec!["Thinking about the file"]);
        assert_eq!(entries[1].result_lines, vec!["File written successfully"]);
        assert_eq!(entries[1].status, StepStatus::Success);
    }

    #[test]
    fn test_convert_session_with_multiline_text() {
        let history = SessionHistory {
            session_id: "test-session".to_string(),
            started_at: "2024-01-01T00:00:00Z".to_string(),
            ended_at: Some("2024-01-01T00:10:00Z".to_string()),
            prompt: "Test prompt".to_string(),
            events: vec![
                SessionEvent::AssistantMessage {
                    text: "Line 1\nLine 2\nLine 3".to_string(),
                    timestamp: "2024-01-01T00:01:00Z".to_string(),
                },
                SessionEvent::ToolUse {
                    name: "Bash".to_string(),
                    timestamp: "2024-01-01T00:02:00Z".to_string(),
                    input: None,
                },
                SessionEvent::ToolResult {
                    name: "Bash".to_string(),
                    timestamp: "2024-01-01T00:03:00Z".to_string(),
                    output: Some("Output line 1\nOutput line 2".to_string()),
                },
            ],
            total_tool_uses: 1,
            files_modified: vec![],
        };

        let entries = WorkerView::convert_session_to_log_entries(&history);

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].step_name, "Bash");
        assert_eq!(entries[0].prompt_lines, vec!["Line 1", "Line 2", "Line 3"]);
        assert_eq!(
            entries[0].result_lines,
            vec!["Output line 1", "Output line 2"]
        );
    }

    #[test]
    fn test_convert_session_without_tool_uses() {
        // ツール使用がないセッション（単なる対話のみ）
        let history = SessionHistory {
            session_id: "test-session-no-tools".to_string(),
            started_at: "2024-01-01T00:00:00Z".to_string(),
            ended_at: Some("2024-01-01T00:05:00Z".to_string()),
            prompt: "Just a conversation".to_string(),
            events: vec![
                SessionEvent::AssistantMessage {
                    text: "I understand your question.".to_string(),
                    timestamp: "2024-01-01T00:01:00Z".to_string(),
                },
                SessionEvent::ThinkingBlock {
                    content: "Let me think about this...".to_string(),
                    timestamp: "2024-01-01T00:02:00Z".to_string(),
                },
                SessionEvent::AssistantMessage {
                    text: "Here is my answer.".to_string(),
                    timestamp: "2024-01-01T00:03:00Z".to_string(),
                },
            ],
            total_tool_uses: 0,
            files_modified: vec![],
        };

        let entries = WorkerView::convert_session_to_log_entries(&history);

        // ツール使用がない場合、セッション全体が1つのLogEntryとして作成される
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].step_index, 0);
        assert_eq!(entries[0].step_name, "Session");
        assert_eq!(
            entries[0].prompt_lines,
            vec!["I understand your question.", "Here is my answer."]
        );
        assert_eq!(
            entries[0].thought_lines,
            vec!["Let me think about this..."]
        );
        assert!(entries[0].result_lines.is_empty());
        assert_eq!(entries[0].status, StepStatus::Success);
    }

    #[test]
    fn test_convert_empty_session() {
        // イベントが空のセッション
        let history = SessionHistory {
            session_id: "empty-session".to_string(),
            started_at: "2024-01-01T00:00:00Z".to_string(),
            ended_at: Some("2024-01-01T00:01:00Z".to_string()),
            prompt: "Empty".to_string(),
            events: vec![],
            total_tool_uses: 0,
            files_modified: vec![],
        };

        let entries = WorkerView::convert_session_to_log_entries(&history);

        // イベントが空の場合はLogEntryも作成されない
        assert_eq!(entries.len(), 0);
    }

    #[test]
    fn test_convert_session_with_only_system_events() {
        // AssistantMessageやThinkingBlockがないセッション
        let history = SessionHistory {
            session_id: "system-only-session".to_string(),
            started_at: "2024-01-01T00:00:00Z".to_string(),
            ended_at: Some("2024-01-01T00:01:00Z".to_string()),
            prompt: "System events only".to_string(),
            events: vec![
                // 他のイベントタイプがあっても、AssistantMessageやThinkingBlockがなければ何も作成されない
                SessionEvent::ToolResult {
                    name: "SomeResult".to_string(),
                    timestamp: "2024-01-01T00:01:00Z".to_string(),
                    output: Some("Result without tool use".to_string()),
                },
            ],
            total_tool_uses: 0,
            files_modified: vec![],
        };

        let entries = WorkerView::convert_session_to_log_entries(&history);

        // AssistantMessageやThinkingBlockがない場合はLogEntryは作成されない
        assert_eq!(entries.len(), 0);
    }
}
