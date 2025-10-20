/// ログパーサーモジュール - ワーカーログの構造化処理を提供
///
/// このモジュールはワーカーから出力される生ログを解析し、
/// ステップごとの構造化されたログエントリに変換する。

use crate::ui::types::{LogEntry, StepStatus};

/// ログパーサーの状態を保持する構造体
#[derive(Debug, Clone, Default)]
pub struct LogParser {
    // Parser state
    current_step_index: Option<usize>,
    current_step_name: Option<String>,
    current_prompt: Vec<String>,
    current_result: Vec<String>,
    current_thought: Vec<String>,
    in_prompt: bool,
    in_result: bool,
    in_thought: bool,
}

impl LogParser {
    /// 新しいログパーサーを作成
    pub fn new() -> Self {
        Self::default()
    }

    /// ログ行を解析して構造化する
    ///
    /// # 引数
    /// * `line` - 解析するログ行
    ///
    /// # 戻り値
    /// * `Some(LogEntry)` - ステップが完了してエントリが作成された場合
    /// * `None` - まだステップが進行中の場合
    pub fn parse_line(&mut self, line: &str) -> Option<LogEntry> {
        if line.starts_with("[STEP_START:") {
            self.handle_step_start(line);
            None
        } else if line == "─── Prompt ───" || line == "[PROMPT_START]" {
            self.start_prompt_section();
            None
        } else if line == "[PROMPT_END]" {
            self.end_prompt_section();
            None
        } else if line == "─── Result ───" || line == "[RESULT_START]" {
            self.start_result_section();
            None
        } else if line == "[RESULT_END]" {
            self.end_result_section();
            None
        } else if line == "[THOUGHT_START]" {
            self.start_thought_section();
            None
        } else if line == "[THOUGHT_END]" {
            self.end_thought_section();
            None
        } else if line.starts_with("[STEP_END:") {
            self.finalize_step(line)
        } else if line.starts_with("───") && line.ends_with("───") {
            // Other section markers end current sections
            self.end_all_sections();
            None
        } else if self.in_prompt && !line.starts_with("─") {
            self.append_prompt_line(line);
            None
        } else if self.in_result && !line.starts_with("─") {
            self.append_result_line(line);
            None
        } else if self.in_thought {
            self.append_thought_line(line);
            None
        } else {
            None
        }
    }

    /// ステップ開始マーカーを処理
    fn handle_step_start(&mut self, line: &str) {
        if let Some(content) = line
            .strip_prefix("[STEP_START:")
            .and_then(|s| s.strip_suffix("]"))
        {
            let parts: Vec<&str> = content.splitn(2, ':').collect();
            if parts.len() == 2 {
                if let Ok(idx) = parts[0].parse::<usize>() {
                    self.current_step_index = Some(idx);
                    self.current_step_name = Some(parts[1].to_string());
                    self.reset_buffers();
                }
            }
        }
    }

    /// プロンプトセクションを開始
    fn start_prompt_section(&mut self) {
        self.in_prompt = true;
        self.in_result = false;
        self.in_thought = false;
    }

    /// プロンプトセクションを終了
    fn end_prompt_section(&mut self) {
        self.in_prompt = false;
    }

    /// 結果セクションを開始
    fn start_result_section(&mut self) {
        self.in_prompt = false;
        self.in_result = true;
        self.in_thought = false;
    }

    /// 結果セクションを終了
    fn end_result_section(&mut self) {
        self.in_result = false;
    }

    /// 思考セクションを開始
    fn start_thought_section(&mut self) {
        self.in_prompt = false;
        self.in_result = false;
        self.in_thought = true;
        self.current_thought.clear();
    }

    /// 思考セクションを終了
    fn end_thought_section(&mut self) {
        self.in_thought = false;
    }

    /// 全てのセクションを終了
    fn end_all_sections(&mut self) {
        self.in_prompt = false;
        self.in_result = false;
        self.in_thought = false;
    }

    /// プロンプト行を追加
    fn append_prompt_line(&mut self, line: &str) {
        self.current_prompt.push(line.to_string());
    }

    /// 結果行を追加
    fn append_result_line(&mut self, line: &str) {
        self.current_result.push(line.to_string());
    }

    /// 思考行を追加
    fn append_thought_line(&mut self, line: &str) {
        self.current_thought.push(line.to_string());
    }

    /// バッファをリセット
    fn reset_buffers(&mut self) {
        self.current_prompt.clear();
        self.current_result.clear();
        self.current_thought.clear();
        self.in_prompt = false;
        self.in_result = false;
        self.in_thought = false;
    }

    /// ステップを完了してログエントリを作成
    fn finalize_step(&mut self, line: &str) -> Option<LogEntry> {
        let content = line
            .strip_prefix("[STEP_END:")
            .and_then(|s| s.strip_suffix("]"))?;

        let status = match content {
            "Success" => StepStatus::Success,
            "Failed" => StepStatus::Failed,
            _ => StepStatus::Running,
        };

        if let (Some(idx), Some(name)) = (self.current_step_index, &self.current_step_name) {
            let entry = LogEntry {
                step_index: idx,
                step_name: name.clone(),
                prompt_lines: self.current_prompt.clone(),
                result_lines: self.current_result.clone(),
                thought_lines: self.current_thought.clone(),
                status,
            };

            // Reset state after creating entry
            self.current_step_index = None;
            self.current_step_name = None;
            self.reset_buffers();

            Some(entry)
        } else {
            None
        }
    }

    /// 現在のパーサー状態を取得（テスト用）
    #[cfg(test)]
    pub fn current_state(&self) -> (bool, bool, bool) {
        (self.in_prompt, self.in_result, self.in_thought)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_complete_step() {
        let mut parser = LogParser::new();

        // ステップ開始
        assert!(parser.parse_line("[STEP_START:0:TestStep]").is_none());

        // プロンプトセクション
        assert!(parser.parse_line("[PROMPT_START]").is_none());
        assert!(parser.parse_line("This is a prompt").is_none());
        assert!(parser.parse_line("[PROMPT_END]").is_none());

        // 結果セクション
        assert!(parser.parse_line("[RESULT_START]").is_none());
        assert!(parser.parse_line("This is a result").is_none());
        assert!(parser.parse_line("[RESULT_END]").is_none());

        // ステップ終了
        let entry = parser.parse_line("[STEP_END:Success]");
        assert!(entry.is_some());

        let entry = entry.unwrap();
        assert_eq!(entry.step_index, 0);
        assert_eq!(entry.step_name, "TestStep");
        assert_eq!(entry.status, StepStatus::Success);
        assert_eq!(entry.prompt_lines, vec!["This is a prompt"]);
        assert_eq!(entry.result_lines, vec!["This is a result"]);
    }

    #[test]
    fn test_parse_with_thought() {
        let mut parser = LogParser::new();

        assert!(parser.parse_line("[STEP_START:1:ThinkingStep]").is_none());
        assert!(parser.parse_line("[THOUGHT_START]").is_none());
        assert!(parser.parse_line("Thinking about the problem").is_none());
        assert!(parser.parse_line("[THOUGHT_END]").is_none());

        let entry = parser.parse_line("[STEP_END:Success]");
        assert!(entry.is_some());

        let entry = entry.unwrap();
        assert_eq!(entry.thought_lines, vec!["Thinking about the problem"]);
    }

    #[test]
    fn test_alternate_section_markers() {
        let mut parser = LogParser::new();

        assert!(parser.parse_line("[STEP_START:2:AlternateStep]").is_none());
        assert!(parser.parse_line("─── Prompt ───").is_none());
        assert!(parser.parse_line("Alternate prompt marker").is_none());
        assert!(parser.parse_line("─── Result ───").is_none());
        assert!(parser.parse_line("Alternate result marker").is_none());

        let entry = parser.parse_line("[STEP_END:Failed]");
        assert!(entry.is_some());

        let entry = entry.unwrap();
        assert_eq!(entry.status, StepStatus::Failed);
        assert_eq!(entry.prompt_lines, vec!["Alternate prompt marker"]);
        assert_eq!(entry.result_lines, vec!["Alternate result marker"]);
    }
}
