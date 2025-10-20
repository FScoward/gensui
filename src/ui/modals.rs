/// モーダルウィンドウのレンダリング機能
use std::collections::HashMap;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

use crate::worker::{ExistingWorktree, PermissionDecision, PermissionRequest};
use super::helpers::permission_mode_label;
use super::types::AVAILABLE_TOOLS;

/// 汎用的なモーダルウィンドウをレンダリング
///
/// # Arguments
/// * `frame` - 描画フレーム
/// * `area` - 描画領域
/// * `title` - モーダルのタイトル
/// * `lines` - 表示する行のリスト
pub fn render_modal(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    title: &str,
    lines: Vec<Line>,
) {
    let widget = Paragraph::new(lines)
        .wrap(Wrap { trim: true })
        .block(Block::default().borders(Borders::ALL).title(title));
    frame.render_widget(Clear, area);
    frame.render_widget(widget, area);
}

/// 自由入力プロンプトモーダルをレンダリング
///
/// # Arguments
/// * `frame` - 描画フレーム
/// * `area` - 描画領域
/// * `buffer` - 入力バッファ
/// * `permission_mode` - パーミッションモード
pub fn render_prompt_modal(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    buffer: &str,
    permission_mode: &Option<String>,
) {
    let mode_str = permission_mode_label(permission_mode);
    let mode_color = match permission_mode.as_deref() {
        Some("plan") => Color::Cyan,
        Some("acceptEdits") => Color::Yellow,
        _ => Color::Green,
    };

    let lines = vec![
        Line::raw(
            "自由指示を入力してください (Enterで送信 / Escでキャンセル / Ctrl+Pでモード切替)",
        ),
        Line::raw(""),
        Line::from(Span::styled(
            buffer,
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )),
        Line::raw(""),
        Line::from(vec![
            Span::raw("モード: "),
            Span::styled(
                mode_str,
                Style::default().fg(mode_color).add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::raw("Claude Codeがheadlessモードで実行されます"),
    ];
    let widget = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .block(Block::default().borders(Borders::ALL).title("Free Prompt"));
    frame.render_widget(Clear, area);
    frame.render_widget(widget, area);
}

/// パーミッション確認モーダルをレンダリング
pub fn render_permission_modal(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    worker_name: &str,
    request: &PermissionRequest,
    selection: &PermissionDecision,
) {
    let mode_label = permission_mode_label(&request.permission_mode).to_string();
    let tools_text = describe_allowed_tools(&request.allowed_tools);
    let description = request
        .description
        .as_deref()
        .unwrap_or("このステップに進む前に権限が必要です");

    let mut lines = Vec::new();
    lines.push(Line::from(vec![Span::styled(
        "権限確認",
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    )]));
    lines.push(Line::raw(""));
    lines.push(Line::from(vec![
        Span::raw("ワーカー: "),
        Span::styled(worker_name, Style::default().fg(Color::Cyan)),
    ]));
    lines.push(Line::from(vec![
        Span::raw("ステップ: "),
        Span::styled(&request.step_name, Style::default().fg(Color::Green)),
    ]));
    lines.push(Line::from(vec![
        Span::raw("説明: "),
        Span::raw(description),
    ]));
    lines.push(Line::from(vec![
        Span::raw("権限モード: "),
        Span::styled(mode_label, Style::default().fg(Color::Yellow)),
    ]));
    lines.push(Line::from(vec![
        Span::raw("許可ツール: "),
        Span::styled(tools_text, Style::default().fg(Color::Cyan)),
    ]));
    lines.push(Line::raw(""));

    let options = [
        (
            PermissionDecision::Allow {
                permission_mode: None,
                allowed_tools: None,
            },
            "許可する",
        ),
        (PermissionDecision::Deny, "拒否する"),
    ];

    let mut option_spans = Vec::new();
    for (idx, (decision, label)) in options.iter().enumerate() {
        if idx > 0 {
            option_spans.push(Span::raw("    "));
        }
        let is_selected = decision == selection;
        let style = if is_selected {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray)
        };
        option_spans.push(Span::styled(*label, style));
    }
    lines.push(Line::from(option_spans));
    lines.push(Line::raw(""));
    lines.push(Line::raw("←/→ で切替 • Enter/ Y = 許可 • Esc/ N = 拒否"));

    let widget = Paragraph::new(lines)
        .wrap(Wrap { trim: true })
        .block(Block::default().borders(Borders::ALL).title("Permission"));

    frame.render_widget(Clear, area);
    frame.render_widget(widget, area);
}

/// ワーカー作成方法選択モーダルをレンダリング
pub fn render_create_selection_modal(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    selected: usize,
    workflow_name: &str,
) {
    let options = vec![
        format!("  ワークフローを実行 ({})", workflow_name),
        "  自由入力でワーカーを作成".to_string(),
        "  既存worktreeを使用".to_string(),
    ];

    let lines: Vec<Line> = vec![Line::raw("ワーカーの作成方法を選択してください"), Line::raw("")]
        .into_iter()
        .chain(options.iter().enumerate().map(|(i, opt)| {
            if i == selected {
                Line::from(Span::styled(
                    format!("> {}", opt),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ))
            } else {
                Line::from(opt.clone())
            }
        }))
        .chain(vec![
            Line::raw(""),
            Line::raw("↑↓: 選択移動  Enter: 決定  Esc: キャンセル"),
        ])
        .collect();

    let widget = Paragraph::new(lines).wrap(Wrap { trim: false }).block(
        Block::default()
            .borders(Borders::ALL)
            .title("Create Worker"),
    );
    frame.render_widget(Clear, area);
    frame.render_widget(widget, area);
}

/// ツール選択モーダルをレンダリング
pub fn render_tool_selection_modal(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    tools: &HashMap<String, bool>,
    selected_idx: usize,
    permission_mode: &str,
) {
    let mut lines = vec![
        Line::from(Span::styled(
            "ツール選択",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )),
        Line::raw(""),
        Line::raw("↑/↓: 移動  Space: 切替  Enter: 決定  Esc: キャンセル"),
        Line::raw(""),
    ];

    // Render tool checkboxes
    for (idx, tool_def) in AVAILABLE_TOOLS.iter().enumerate() {
        let checked = tools.get(tool_def.name).copied().unwrap_or(false);
        let checkbox = if checked { "[✓]" } else { "[ ]" };
        let text = format!("{} {}  - {}", checkbox, tool_def.name, tool_def.description);

        let line = if idx == selected_idx {
            Line::from(Span::styled(
                format!("> {}", text),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ))
        } else {
            Line::from(text)
        };
        lines.push(line);
    }

    lines.push(Line::raw(""));

    // Render permission_mode selector
    let mode_text = format!(
        "Permission Mode: {}",
        match permission_mode {
            "acceptEdits" => "acceptEdits (編集承認)",
            "bypassPermissions" => "bypassPermissions (制限なし)",
            _ => permission_mode,
        }
    );
    let mode_line = if selected_idx == AVAILABLE_TOOLS.len() {
        Line::from(Span::styled(
            format!("> {} (Space で切替)", mode_text),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ))
    } else {
        Line::from(mode_text)
    };
    lines.push(mode_line);

    let widget = Paragraph::new(lines).wrap(Wrap { trim: false }).block(
        Block::default()
            .borders(Borders::ALL)
            .title("Tool Selection"),
    );

    frame.render_widget(Clear, area);
    frame.render_widget(widget, area);
}

/// worktree選択モーダルをレンダリング
pub fn render_worktree_selection_modal(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    worktrees: &[ExistingWorktree],
    selected: usize,
) {
    let lines: Vec<Line> = vec![Line::raw("既存のworktreeを選択してください"), Line::raw("")]
        .into_iter()
        .chain(worktrees.iter().enumerate().map(|(i, wt)| {
            let display_text = format!("  {} (branch: {})", wt.path.display(), wt.branch);
            if i == selected {
                Line::from(Span::styled(
                    format!("> {}", display_text),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ))
            } else {
                Line::from(display_text)
            }
        }))
        .chain(vec![
            Line::raw(""),
            Line::raw("↑↓: 選択移動  Enter: 決定  Esc: キャンセル"),
        ])
        .collect();

    let widget = Paragraph::new(lines).wrap(Wrap { trim: false }).block(
        Block::default()
            .borders(Borders::ALL)
            .title("Select Worktree"),
    );
    frame.render_widget(Clear, area);
    frame.render_widget(widget, area);
}

/// 許可されたツールの説明テキストを生成
pub fn describe_allowed_tools(tools: &Option<Vec<String>>) -> String {
    match tools {
        None => "制限なし".to_string(),
        Some(list) if list.is_empty() => "なし".to_string(),
        Some(list) => list.join(", "),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_describe_allowed_tools_none() {
        assert_eq!(describe_allowed_tools(&None), "制限なし");
    }

    #[test]
    fn test_describe_allowed_tools_empty() {
        assert_eq!(describe_allowed_tools(&Some(vec![])), "なし");
    }

    #[test]
    fn test_describe_allowed_tools_with_tools() {
        let tools = vec!["Read".to_string(), "Write".to_string()];
        assert_eq!(describe_allowed_tools(&Some(tools)), "Read, Write");
    }
}
