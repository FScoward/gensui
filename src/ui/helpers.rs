/// UI関連のヘルパー関数
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Color;
use crate::state::ActionLogEntry;
use crate::worker::WorkerStatus;

/// 画面中央に配置された矩形領域を計算する
///
/// # Arguments
/// * `percent_x` - 横幅のパーセンテージ（0-100）
/// * `percent_y` - 縦幅のパーセンテージ（0-100）
/// * `area` - 親となる描画領域
///
/// # Returns
/// 中央に配置された矩形領域
pub fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints(
            [
                Constraint::Percentage((100 - percent_y) / 2),
                Constraint::Percentage(percent_y),
                Constraint::Percentage((100 - percent_y) / 2),
            ]
            .as_ref(),
        )
        .split(area);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints(
            [
                Constraint::Percentage((100 - percent_x) / 2),
                Constraint::Percentage(percent_x),
                Constraint::Percentage((100 - percent_x) / 2),
            ]
            .as_ref(),
        )
        .split(vertical[1])[1]
}

/// アクションログエントリをフォーマットして文字列に変換
///
/// # Arguments
/// * `entry` - アクションログエントリ
///
/// # Returns
/// フォーマットされたログ文字列
pub fn format_action_log(entry: &ActionLogEntry) -> String {
    match &entry.worker {
        Some(worker) => format!("[{}][{}] {}", entry.timestamp, worker, entry.message),
        None => format!("[{}] {}", entry.timestamp, entry.message),
    }
}

/// ワーカーステータスに対応する色を返す
///
/// # Arguments
/// * `status` - ワーカーステータス
///
/// # Returns
/// ステータスに対応する色
pub fn status_color(status: WorkerStatus) -> Color {
    match status {
        WorkerStatus::Running => Color::Green,
        WorkerStatus::Paused => Color::Yellow,
        WorkerStatus::Failed => Color::Red,
        WorkerStatus::Idle => Color::Gray,
        WorkerStatus::Archived => Color::Blue,
    }
}

/// パーミッションモードに対応するラベルを返す
///
/// # Arguments
/// * `permission_mode` - パーミッションモード（オプショナル）
///
/// # Returns
/// モードに対応する日本語ラベル
pub fn permission_mode_label(permission_mode: &Option<String>) -> &str {
    match permission_mode.as_deref() {
        None => "制限なしモード",
        Some("plan") => "プランモード",
        Some("acceptEdits") => "編集承認モード",
        Some("bypassPermissions") => "制限なしモード",
        Some(other) => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_centered_rect() {
        let area = Rect::new(0, 0, 100, 100);
        let centered = centered_rect(50, 50, area);

        // 中央に配置されていることを確認
        assert!(centered.x >= area.x);
        assert!(centered.y >= area.y);
        assert!(centered.width <= area.width);
        assert!(centered.height <= area.height);
    }

    #[test]
    fn test_format_action_log_with_worker() {
        let entry = ActionLogEntry {
            timestamp: "2024-01-01T00:00:00Z".to_string(),
            message: "Test message".to_string(),
            worker: Some("worker-1".to_string()),
        };

        let formatted = format_action_log(&entry);
        assert_eq!(formatted, "[2024-01-01T00:00:00Z][worker-1] Test message");
    }

    #[test]
    fn test_format_action_log_without_worker() {
        let entry = ActionLogEntry {
            timestamp: "2024-01-01T00:00:00Z".to_string(),
            message: "Test message".to_string(),
            worker: None,
        };

        let formatted = format_action_log(&entry);
        assert_eq!(formatted, "[2024-01-01T00:00:00Z] Test message");
    }

    #[test]
    fn test_status_color() {
        assert_eq!(status_color(WorkerStatus::Running), Color::Green);
        assert_eq!(status_color(WorkerStatus::Paused), Color::Yellow);
        assert_eq!(status_color(WorkerStatus::Failed), Color::Red);
        assert_eq!(status_color(WorkerStatus::Idle), Color::Gray);
        assert_eq!(status_color(WorkerStatus::Archived), Color::Blue);
    }

    #[test]
    fn test_permission_mode_label() {
        assert_eq!(permission_mode_label(&None), "制限なしモード");
        assert_eq!(permission_mode_label(&Some("plan".to_string())), "プランモード");
        assert_eq!(permission_mode_label(&Some("acceptEdits".to_string())), "編集承認モード");
        assert_eq!(permission_mode_label(&Some("bypassPermissions".to_string())), "制限なしモード");
        assert_eq!(permission_mode_label(&Some("unknown".to_string())), "unknown");
    }
}
