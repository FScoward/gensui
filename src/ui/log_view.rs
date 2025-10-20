/// ログビューのレンダリング機能
use std::collections::VecDeque;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Clear, Paragraph, Row, Table, Wrap};

use super::types::{LogEntry, StepStatus};
use super::helpers::centered_rect;

/// ログモーダルのデータ
pub struct LogModalData {
    pub title: String,
    pub lines: Vec<Line<'static>>,
}

/// Rawログビュー用のデータを生成
pub fn prepare_raw_log_data(
    worker_logs: Option<&VecDeque<String>>,
    global_logs: &VecDeque<String>,
    log_scroll: usize,
    auto_scroll: bool,
) -> LogModalData {
    let (all_lines, base_title): (Vec<String>, &str) = if let Some(logs) = worker_logs {
        if logs.is_empty() {
            (
                vec!["このワーカーのログはまだありません。".to_string()],
                "Worker Logs",
            )
        } else {
            (logs.iter().cloned().collect(), "Worker Logs")
        }
    } else if global_logs.is_empty() {
        (
            vec!["アクションログはまだありません。".to_string()],
            "Action Logs",
        )
    } else {
        (global_logs.iter().cloned().collect(), "Action Logs")
    };

    let total_lines = all_lines.len();
    let visible_start = log_scroll.min(total_lines.saturating_sub(1));

    // Show lines from scroll position onwards
    let visible_lines: Vec<Line<'static>> = all_lines
        .iter()
        .skip(visible_start)
        .map(|s| Line::from(s.clone()))
        .collect();

    let auto_scroll_status = if auto_scroll {
        "[Auto-scroll: ON]"
    } else {
        "[Auto-scroll: OFF]"
    };

    let title = if total_lines > 1 {
        format!(
            "{} (line {}/{}) {} [↑↓:scroll PgUp/PgDn:page Home/End:jump Shift+A:toggle]",
            base_title,
            visible_start + 1,
            total_lines,
            auto_scroll_status
        )
    } else {
        format!("{} {}", base_title, auto_scroll_status)
    };

    LogModalData {
        title,
        lines: visible_lines,
    }
}

/// Overviewタブをレンダリング
pub fn render_overview_tab(
    frame: &mut ratatui::Frame<'_>,
    entries: &[LogEntry],
    selected_step: usize,
    auto_scroll: bool,
) {
    let area = centered_rect(80, 60, frame.area());

    if entries.is_empty() {
        let lines = vec![Line::raw("このワーカーにはまだステップログがありません。")];
        let auto_scroll_status = if auto_scroll {
            "[Auto-scroll: ON]"
        } else {
            "[Auto-scroll: OFF]"
        };
        let title = format!("Overview {} [Tab:switch tabs Shift+A:toggle]", auto_scroll_status);
        let widget = Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL).title(title));
        frame.render_widget(Clear, area);
        frame.render_widget(widget, area);
        return;
    }

    // Clamp selected_step to valid range to prevent panic
    let safe_selected_step = selected_step.min(entries.len().saturating_sub(1));

    // Build table rows
    let header = Row::new(vec![
        Cell::from("#"),
        Cell::from("Step"),
        Cell::from("Status"),
        Cell::from("Summary"),
    ])
    .style(Style::default().add_modifier(Modifier::BOLD))
    .bottom_margin(1);

    // Add status and summary processing with safe string slicing
    let rows: Vec<Row> = entries
        .iter()
        .enumerate()
        .map(|(idx, entry)| {
            let status_str = match entry.status {
                StepStatus::Running => "Running",
                StepStatus::Success => "✓ Success",
                StepStatus::Failed => "✗ Failed",
            };

            // Safe string truncation using chars instead of byte slicing
            let (summary_source, prefix) =
                if let Some(first_thought) = entry.thought_lines.first() {
                    (first_thought.clone(), "🤔 ")
                } else if let Some(first_result) = entry.result_lines.first() {
                    (first_result.clone(), "")
                } else {
                    ("(no result)".to_string(), "")
                };

            let summary_body = {
                let chars: Vec<char> = summary_source.chars().collect();
                if chars.len() > 60 {
                    let truncated: String = chars.iter().take(60).collect();
                    format!("{}...", truncated)
                } else {
                    summary_source
                }
            };
            let summary = format!("{}{}", prefix, summary_body);

            let style = if idx == safe_selected_step {
                Style::default().bg(Color::DarkGray)
            } else {
                Style::default()
            };

            Row::new(vec![
                Cell::from(format!("{}", entry.step_index)),
                Cell::from(entry.step_name.clone()),
                Cell::from(status_str),
                Cell::from(summary),
            ])
            .style(style)
        })
        .collect();

    let widths = [
        ratatui::layout::Constraint::Length(4),
        ratatui::layout::Constraint::Length(20),
        ratatui::layout::Constraint::Length(12),
        ratatui::layout::Constraint::Min(30),
    ];

    let auto_scroll_status = if auto_scroll {
        "[Auto-scroll: ON]"
    } else {
        "[Auto-scroll: OFF]"
    };
    let title = format!(
        "Overview {} [Tab:switch tabs | Enter:detail | j/k:select | Shift+A:toggle]",
        auto_scroll_status
    );

    let table = Table::new(rows, widths).header(header).block(
        Block::default()
            .borders(Borders::ALL)
            .title(title),
    );

    frame.render_widget(Clear, area);
    frame.render_widget(table, area);
}

/// Detailタブをレンダリング
pub fn render_detail_tab(
    frame: &mut ratatui::Frame<'_>,
    entry: &LogEntry,
    log_scroll: usize,
    auto_scroll: bool,
) {
    let area = centered_rect(80, 60, frame.area());

    let mut lines = Vec::new();

    // Title
    lines.push(Line::from(format!(
        "Step #{}: {}",
        entry.step_index, entry.step_name
    )));
    lines.push(Line::raw(""));

    // Prompt section
    lines.push(Line::from(Span::styled(
        "─── Prompt ───",
        Style::default().add_modifier(Modifier::BOLD),
    )));
    for line in &entry.prompt_lines {
        lines.push(Line::from(line.clone()));
    }
    lines.push(Line::raw(""));

    if !entry.thought_lines.is_empty() {
        lines.push(Line::from(Span::styled(
            "─── Thought ───",
            Style::default().add_modifier(Modifier::BOLD),
        )));
        for line in &entry.thought_lines {
            lines.push(Line::from(line.clone()));
        }
        lines.push(Line::raw(""));
    }

    // Result section
    lines.push(Line::from(Span::styled(
        "─── Result ───",
        Style::default().add_modifier(Modifier::BOLD),
    )));
    for line in &entry.result_lines {
        lines.push(Line::from(line.clone()));
    }

    let visible_lines: Vec<Line> = lines.iter().skip(log_scroll).cloned().collect();

    let auto_scroll_status = if auto_scroll {
        "[Auto-scroll: ON]"
    } else {
        "[Auto-scroll: OFF]"
    };
    let title = format!(
        "Detail - Step {} {} [Tab:switch tabs | Esc:back | ↑↓:scroll | Shift+A:toggle]",
        entry.step_index,
        auto_scroll_status
    );

    let widget = Paragraph::new(visible_lines)
        .block(Block::default().borders(Borders::ALL).title(title))
        .wrap(Wrap { trim: false });

    frame.render_widget(Clear, area);
    frame.render_widget(widget, area);
}

/// ログモーダルをレンダリング
pub fn render_log_modal(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    title: &str,
    lines: Vec<Line>,
) {
    let widget = Paragraph::new(lines)
        .block(Block::default().borders(Borders::ALL).title(title));
    frame.render_widget(Clear, area);
    frame.render_widget(widget, area);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_prepare_raw_log_data_empty() {
        let global_logs = VecDeque::new();
        let data = prepare_raw_log_data(None, &global_logs, 0, true);

        assert!(data.title.contains("Action Logs"));
        assert!(data.title.contains("[Auto-scroll: ON]"));
        assert_eq!(data.lines.len(), 1);
    }

    #[test]
    fn test_prepare_raw_log_data_with_logs() {
        let mut global_logs = VecDeque::new();
        global_logs.push_back("Log line 1".to_string());
        global_logs.push_back("Log line 2".to_string());

        let data = prepare_raw_log_data(None, &global_logs, 0, false);

        assert!(data.title.contains("Action Logs"));
        assert!(data.title.contains("[Auto-scroll: OFF]"));
        assert_eq!(data.lines.len(), 2);
    }

    #[test]
    fn test_prepare_raw_log_data_with_scroll() {
        let mut global_logs = VecDeque::new();
        for i in 0..10 {
            global_logs.push_back(format!("Log line {}", i));
        }

        let data = prepare_raw_log_data(None, &global_logs, 5, true);

        // Should show lines from index 5 onwards
        assert!(data.lines.len() <= 5);
        assert!(data.title.contains("line 6/10"));
    }
}
