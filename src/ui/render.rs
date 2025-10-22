/// メインUIのレンダリング機能
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table};

use crate::worker::{WorkerSnapshot, WorkerStatus};
use super::helpers::status_color;

/// ヘッダー部分をレンダリング
pub fn render_header(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    total_workers: usize,
    filter_label: &str,
    workflow_name: &str,
) {
    let line = Line::from(vec![
        Span::styled(
            "Gensui",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" – multi-worker dashboard  "),
        Span::raw(format!(
            "Workers: {}  Filter: {}  Workflow: {}",
            total_workers, filter_label, workflow_name
        )),
    ]);

    let header =
        Paragraph::new(line).block(Block::default().borders(Borders::ALL).title("Overview"));
    frame.render_widget(header, area);
}

/// ワーカーテーブルをレンダリング
pub fn render_table(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    workers: &[(usize, &WorkerSnapshot)],
    selected: usize,
    animation_frame: usize,
) {
    let rows = workers.iter().enumerate().map(|(table_idx, (_, worker))| {
        // For Running status, don't apply row-level color so cell colors show through
        let mut style = if worker.status == WorkerStatus::Running {
            Style::default()
        } else {
            Style::default().fg(status_color(worker.status))
        };

        if table_idx == selected {
            // For Running workers, only set background (not foreground) to preserve rainbow colors
            if worker.status == WorkerStatus::Running {
                style = style.bg(Color::DarkGray);
            } else {
                style = style.bg(Color::DarkGray).fg(Color::White);
            }
        }

        // Add spinner and rainbow gradient animation for Running status
        const SPINNER_CHARS: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
        const RAINBOW_COLORS: &[Color] = &[
            Color::Red,
            Color::LightRed,
            Color::Yellow,
            Color::LightYellow,
            Color::Green,
            Color::LightGreen,
            Color::Cyan,
            Color::LightCyan,
            Color::Blue,
            Color::LightBlue,
            Color::Magenta,
            Color::LightMagenta,
        ];

        // Animate per-character colors for Running status (left-to-right flow)
        let (name_cell, status_cell, last_event_cell) =
            if worker.status == WorkerStatus::Running {
                let spinner_idx = animation_frame % SPINNER_CHARS.len();
                let spinner = SPINNER_CHARS[spinner_idx];

                // Faster animation for smooth flow
                let slow_frame = animation_frame / 3;

                let is_selected = table_idx == selected;

                // Helper function to create rainbow text with per-character colors
                let create_rainbow_line = |text: &str| -> Vec<Span> {
                    if text.is_empty() {
                        return vec![Span::raw("")];
                    }

                    let mut spans = Vec::new();
                    let chars_vec: Vec<char> = text.chars().collect();
                    for (char_idx, ch) in chars_vec.iter().enumerate() {
                        let color_idx = (slow_frame + char_idx / 2) % RAINBOW_COLORS.len();
                        let color = RAINBOW_COLORS[color_idx];
                        let mut style = Style::default().fg(color).add_modifier(Modifier::BOLD);
                        if is_selected {
                            style = style.bg(Color::DarkGray);
                        }
                        spans.push(Span::styled(ch.to_string(), style));
                    }
                    spans
                };

                let status_text = format!("{} {}", spinner, worker.status.label());
                let sparkles = &["✨", "💫", "⭐", "🌟"];
                let sparkle_idx = (animation_frame / 10) % sparkles.len();
                let sparkle = sparkles[sparkle_idx];

                // For last_event, add sparkle as separate span to avoid emoji breakage
                let sparkle_style = if is_selected {
                    Style::default().bg(Color::DarkGray)
                } else {
                    Style::default()
                };
                let mut last_event_spans = vec![Span::styled(format!("{} ", sparkle), sparkle_style)];
                last_event_spans.extend(create_rainbow_line(&worker.last_event));

                (
                    Cell::from(Line::from(create_rainbow_line(&worker.name))),
                    Cell::from(Line::from(create_rainbow_line(&status_text))),
                    Cell::from(Line::from(last_event_spans)),
                )
            } else {
                (
                    Cell::from(worker.name.clone()),
                    Cell::from(worker.status.label()),
                    Cell::from(worker.last_event.clone()),
                )
            };

        // For Running workers that are selected, apply background to all cells
        let other_cell_style = if worker.status == WorkerStatus::Running && table_idx == selected {
            Style::default().bg(Color::DarkGray)
        } else {
            Style::default()
        };

        let row = Row::new(vec![
            name_cell,
            Cell::from(
                worker
                    .issue
                    .clone()
                    .unwrap_or_else(|| "Unassigned".into()),
            )
            .style(other_cell_style),
            Cell::from(worker.workflow.clone()).style(other_cell_style),
            Cell::from(worker.current_step.clone().unwrap_or_else(|| {
                if worker.total_steps > 0 {
                    format!("0/{} steps", worker.total_steps)
                } else {
                    "-".into()
                }
            }))
            .style(other_cell_style),
            Cell::from(worker.agent.clone()).style(other_cell_style),
            Cell::from(worker.worktree.clone()).style(other_cell_style),
            Cell::from(worker.branch.clone()).style(other_cell_style),
            status_cell,
            last_event_cell,
        ]);

        // Only apply row style for non-Running status (to preserve rainbow colors)
        if worker.status == WorkerStatus::Running {
            row
        } else {
            row.style(style)
        }
    });

    let header = Row::new(vec![
        Cell::from("NAME"),
        Cell::from("ISSUE"),
        Cell::from("WORKFLOW"),
        Cell::from("STEP"),
        Cell::from("AGENT"),
        Cell::from("WORKTREE"),
        Cell::from("BRANCH"),
        Cell::from("STATUS"),
        Cell::from("LAST EVENT"),
    ])
    .style(Style::default().add_modifier(Modifier::BOLD));

    let widths = [
        ratatui::layout::Constraint::Length(12),
        ratatui::layout::Constraint::Length(10),
        ratatui::layout::Constraint::Length(14),
        ratatui::layout::Constraint::Length(18),
        ratatui::layout::Constraint::Length(20),
        ratatui::layout::Constraint::Length(24),
        ratatui::layout::Constraint::Length(20),
        ratatui::layout::Constraint::Length(10),
        ratatui::layout::Constraint::Min(24),
    ];

    let table = Table::new(rows, widths)
        .header(header)
        .block(Block::default().borders(Borders::ALL).title("Workers"))
        .column_spacing(1)
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));

    frame.render_widget(table, area);
}

/// フッター部分をレンダリング
pub fn render_footer(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    workflow_name: &str,
) {
    let lines = vec![
        Line::from(vec![
            Span::styled("q", Style::default().fg(Color::Cyan)),
            Span::raw(" quit  "),
            Span::styled("c", Style::default().fg(Color::Cyan)),
            Span::raw(" create  "),
            Span::styled("d", Style::default().fg(Color::Cyan)),
            Span::raw(" delete  "),
            Span::styled("r", Style::default().fg(Color::Cyan)),
            Span::raw(" restart  "),
            Span::styled("n", Style::default().fg(Color::Cyan)),
            Span::raw(" rename  "),
            Span::styled("a", Style::default().fg(Color::Cyan)),
            Span::raw(" filter  "),
            Span::styled("w", Style::default().fg(Color::Cyan)),
            Span::raw(" workflow  "),
            Span::styled("h", Style::default().fg(Color::Cyan)),
            Span::raw(" help  "),
            Span::styled("l", Style::default().fg(Color::Cyan)),
            Span::raw(" logs"),
        ]),
        Line::from(vec![
            Span::styled("i", Style::default().fg(Color::Cyan)),
            Span::raw(": send prompt (or continue if worker selected) | "),
            Span::raw("Active workflow: "),
            Span::styled(workflow_name, Style::default().fg(Color::Magenta)),
        ]),
    ];

    let footer =
        Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title("Controls"));
    frame.render_widget(footer, area);
}

/// ヘルプテキストを生成
pub fn help_lines() -> Vec<Line<'static>> {
    vec![
        Line::raw("MVP ショートカット"),
        Line::raw(""),
        Line::raw("c – ワーカーを作成（ワークフロー or 自由入力を選択）"),
        Line::raw("d – ワーカー停止と worktree 削除（アーカイブは状態削除のみ）"),
        Line::raw("r – ワーカーを再起動（アーカイブは不可）"),
        Line::raw("n – ワーカー名を変更"),
        Line::raw("i – 自由指示を送信（ワーカー選択時は追加指示、アーカイブは不可）"),
        Line::raw("a – ステータスフィルタを切り替え"),
        Line::raw("w – 使用するワークフローを切り替え"),
        Line::raw("j/k または ↑/↓ – 選択移動 (ログ表示時はスクロール)"),
        Line::raw("PgUp/PgDn – ログを10行スクロール"),
        Line::raw("Home/End – ログの先頭/末尾へジャンプ"),
        Line::raw("l – 選択ワーカーのログを表示"),
        Line::raw("s – 選択ワーカーのセッション履歴を表示"),
        Line::raw("h – このヘルプを表示"),
        Line::raw("Shift+C – アクションログを圧縮"),
        Line::raw("Shift+I – インタラクティブClaude Code起動（権限を手動承認可能）"),
        Line::raw("Shift+A – ログの自動スクロールON/OFF切替"),
        Line::raw("q – 終了"),
        Line::raw(""),
        Line::raw("入力モーダル操作:"),
        Line::raw("  プロンプト入力: Enter で送信 / Ctrl+J で改行 / Esc でキャンセル"),
        Line::raw("  名前入力/変更: Enter で確定 / Esc でキャンセル"),
        Line::raw("  矢印キー/Home/End でカーソル移動、複数行入力可能"),
        Line::raw(""),
        Line::raw("ステータス: Running/Idle/Paused/Failed/Archived(青=履歴)"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_help_lines_count() {
        let lines = help_lines();
        assert!(lines.len() > 10);
    }

    #[test]
    fn test_help_lines_contains_shortcuts() {
        let lines = help_lines();
        let text = lines.iter()
            .map(|line| format!("{:?}", line))
            .collect::<Vec<_>>()
            .join("\n");

        assert!(text.contains("終了"));
        assert!(text.contains("ワーカーを作成"));
        assert!(text.contains("削除"));
    }
}
