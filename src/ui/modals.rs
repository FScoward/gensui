/// モーダルウィンドウのレンダリング機能
use std::collections::HashMap;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

use crate::state::{SessionEvent, SessionHistory};
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

/// Worker名前入力モーダルをレンダリング
pub fn render_name_input_modal(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    buffer: &str,
    workflow_name: &Option<String>,
) {
    let workflow_text = workflow_name
        .as_ref()
        .map(|name| format!("ワークフロー: {}", name))
        .unwrap_or_else(|| "新規ワーカー".to_string());

    let lines = vec![
        Line::from(Span::styled(
            "Worker名を入力してください",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )),
        Line::raw(""),
        Line::raw(&workflow_text),
        Line::raw(""),
        Line::from(vec![
            Span::raw("名前: "),
            Span::styled(
                buffer,
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::raw(""),
        Line::raw("Enter: 確定 / Esc: スキップ（デフォルト名を使用）"),
        Line::raw(""),
        Line::from(vec![
            Span::styled(
                "※",
                Style::default().fg(Color::Gray),
            ),
            Span::styled(
                " 使用可能: 英数字、ハイフン、アンダースコア、日本語 (1-64文字)",
                Style::default().fg(Color::Gray),
            ),
        ]),
    ];

    let widget = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .block(Block::default().borders(Borders::ALL).title("Worker Name"));
    frame.render_widget(Clear, area);
    frame.render_widget(widget, area);
}

/// Worker名前変更モーダルをレンダリング
pub fn render_rename_worker_modal(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    buffer: &str,
    current_name: &str,
) {
    let lines = vec![
        Line::from(Span::styled(
            "Worker名を変更",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )),
        Line::raw(""),
        Line::from(vec![
            Span::raw("現在の名前: "),
            Span::styled(
                current_name,
                Style::default().fg(Color::Gray),
            ),
        ]),
        Line::raw(""),
        Line::from(vec![
            Span::raw("新しい名前: "),
            Span::styled(
                buffer,
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::raw(""),
        Line::raw("Enter: 確定 / Esc: キャンセル"),
        Line::raw(""),
        Line::from(vec![
            Span::styled(
                "※",
                Style::default().fg(Color::Gray),
            ),
            Span::styled(
                " 使用可能: 英数字、ハイフン、アンダースコア、日本語 (1-64文字)",
                Style::default().fg(Color::Gray),
            ),
        ]),
    ];

    let widget = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .block(Block::default().borders(Borders::ALL).title("Rename Worker"));
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

/// セッション履歴モーダルをレンダリング
pub fn render_session_history_modal(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    sessions: &[SessionHistory],
    selected_session: usize,
    scroll: usize,
) {
    let mut lines = vec![
        Line::from(Span::styled(
            "Session History",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )),
        Line::raw(""),
        Line::raw("↑/↓: スクロール  j/k: セッション選択  q/Esc: 閉じる"),
        Line::raw("━".repeat(area.width as usize)),
    ];

    if sessions.is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::from(Span::styled(
            "セッション履歴がありません",
            Style::default().fg(Color::Gray),
        )));
    } else {
        for (idx, session) in sessions.iter().enumerate() {
            let is_selected = idx == selected_session;

            // Session header
            let session_header = format!(
                "Session #{} [{}]",
                idx + 1,
                session.session_id.chars().take(8).collect::<String>()
            );

            lines.push(Line::raw(""));
            if is_selected {
                lines.push(Line::from(Span::styled(
                    format!("> {}", session_header),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                )));
            } else {
                lines.push(Line::from(Span::styled(
                    session_header,
                    Style::default().fg(Color::Cyan),
                )));
            }

            // Session metadata
            lines.push(Line::from(vec![
                Span::raw("  開始: "),
                Span::styled(&session.started_at, Style::default().fg(Color::Green)),
            ]));

            if let Some(ended_at) = &session.ended_at {
                lines.push(Line::from(vec![
                    Span::raw("  終了: "),
                    Span::styled(ended_at, Style::default().fg(Color::Green)),
                ]));
            }

            lines.push(Line::from(vec![
                Span::raw("  プロンプト: "),
                Span::styled(
                    truncate_string(&session.prompt, 60),
                    Style::default().fg(Color::White),
                ),
            ]));

            lines.push(Line::from(vec![
                Span::raw("  ツール使用: "),
                Span::styled(
                    format!("{} 回", session.total_tool_uses),
                    Style::default().fg(Color::Magenta),
                ),
            ]));

            if !session.files_modified.is_empty() {
                lines.push(Line::from(vec![
                    Span::raw("  編集ファイル: "),
                    Span::styled(
                        format!("{} 件", session.files_modified.len()),
                        Style::default().fg(Color::Yellow),
                    ),
                ]));

                // Show first few files
                for (file_idx, file) in session.files_modified.iter().take(3).enumerate() {
                    lines.push(Line::from(vec![
                        Span::raw("    "),
                        Span::styled(
                            truncate_string(file, 50),
                            Style::default().fg(Color::Gray),
                        ),
                    ]));

                    if file_idx == 2 && session.files_modified.len() > 3 {
                        lines.push(Line::from(vec![
                            Span::raw("    "),
                            Span::styled(
                                format!("... あと {} 件", session.files_modified.len() - 3),
                                Style::default().fg(Color::Gray),
                            ),
                        ]));
                    }
                }
            }

            // Show event summary if selected
            if is_selected {
                lines.push(Line::raw(""));
                lines.push(Line::from(Span::styled(
                    "  イベント:",
                    Style::default().fg(Color::Cyan),
                )));

                let event_count = session.events.len().min(10);
                for event in session.events.iter().take(event_count) {
                    let event_line = match event {
                        SessionEvent::ToolUse { name, timestamp, .. } => {
                            Line::from(vec![
                                Span::raw("    🔧 "),
                                Span::styled(name, Style::default().fg(Color::Blue)),
                                Span::raw(" @ "),
                                Span::styled(
                                    format_timestamp(timestamp),
                                    Style::default().fg(Color::Gray),
                                ),
                            ])
                        }
                        SessionEvent::AssistantMessage { text, timestamp } => {
                            Line::from(vec![
                                Span::raw("    💬 "),
                                Span::styled(
                                    truncate_string(text, 50),
                                    Style::default().fg(Color::White),
                                ),
                                Span::raw(" @ "),
                                Span::styled(
                                    format_timestamp(timestamp),
                                    Style::default().fg(Color::Gray),
                                ),
                            ])
                        }
                        SessionEvent::ThinkingBlock { timestamp, .. } => {
                            Line::from(vec![
                                Span::raw("    💭 "),
                                Span::styled("Thinking...", Style::default().fg(Color::Magenta)),
                                Span::raw(" @ "),
                                Span::styled(
                                    format_timestamp(timestamp),
                                    Style::default().fg(Color::Gray),
                                ),
                            ])
                        }
                        SessionEvent::Result { text, is_error, timestamp } => {
                            let icon = if *is_error { "❌" } else { "✅" };
                            let color = if *is_error { Color::Red } else { Color::Green };
                            Line::from(vec![
                                Span::raw(format!("    {} ", icon)),
                                Span::styled(
                                    truncate_string(text, 50),
                                    Style::default().fg(color),
                                ),
                                Span::raw(" @ "),
                                Span::styled(
                                    format_timestamp(timestamp),
                                    Style::default().fg(Color::Gray),
                                ),
                            ])
                        }
                        SessionEvent::Error { message, timestamp } => {
                            Line::from(vec![
                                Span::raw("    ⚠️  "),
                                Span::styled(
                                    truncate_string(message, 50),
                                    Style::default().fg(Color::Red),
                                ),
                                Span::raw(" @ "),
                                Span::styled(
                                    format_timestamp(timestamp),
                                    Style::default().fg(Color::Gray),
                                ),
                            ])
                        }
                        SessionEvent::ToolResult { name, timestamp, .. } => {
                            Line::from(vec![
                                Span::raw("    ✓  "),
                                Span::styled(
                                    format!("{} result", name),
                                    Style::default().fg(Color::Green),
                                ),
                                Span::raw(" @ "),
                                Span::styled(
                                    format_timestamp(timestamp),
                                    Style::default().fg(Color::Gray),
                                ),
                            ])
                        }
                    };
                    lines.push(event_line);
                }

                if session.events.len() > event_count {
                    lines.push(Line::from(vec![
                        Span::raw("    "),
                        Span::styled(
                            format!("... あと {} イベント", session.events.len() - event_count),
                            Style::default().fg(Color::Gray),
                        ),
                    ]));
                }
            }
        }
    }

    // Apply scroll offset
    let display_lines: Vec<Line> = lines.into_iter().skip(scroll).collect();

    let widget = Paragraph::new(display_lines)
        .wrap(Wrap { trim: false })
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Session History"),
        );

    frame.render_widget(Clear, area);
    frame.render_widget(widget, area);
}

/// 文字列を指定長で切り詰める（文字数ベース、マルチバイト文字対応）
fn truncate_string(s: &str, max_len: usize) -> String {
    let char_count = s.chars().count();
    if char_count <= max_len {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_len.saturating_sub(3)).collect();
        format!("{}...", truncated)
    }
}

/// タイムスタンプをフォーマット
fn format_timestamp(timestamp: &str) -> String {
    // RFC3339形式のタイムスタンプから時刻部分のみを抽出
    if let Some(time_part) = timestamp.split('T').nth(1) {
        if let Some(time_only) = time_part.split('.').next() {
            return time_only.to_string();
        }
    }
    timestamp.to_string()
}
