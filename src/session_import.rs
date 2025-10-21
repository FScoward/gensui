/// セッション履歴のインポート機能
///
/// インタラクティブモード終了後にClaudeのセッションファイルから
/// 履歴を読み取ってSessionHistoryに変換する

use anyhow::{Context, Result};
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use time::OffsetDateTime;

use crate::state::{SessionEvent, SessionHistory};

/// Claudeのセッションディレクトリを取得
fn get_claude_sessions_dir() -> Result<PathBuf> {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .context("HOME or USERPROFILE environment variable not set")?;

    // Check custom CLAUDE_CONFIG_DIR first
    if let Ok(custom_dir) = std::env::var("CLAUDE_CONFIG_DIR") {
        return Ok(PathBuf::from(custom_dir).join("sessions"));
    }

    Ok(PathBuf::from(home).join(".claude").join("sessions"))
}

/// ディレクトリ内の最新のセッションファイルを取得
fn get_latest_session_file(sessions_dir: &Path, since: Option<OffsetDateTime>) -> Result<Option<PathBuf>> {
    if !sessions_dir.exists() {
        return Ok(None);
    }

    let mut latest_file: Option<(PathBuf, std::time::SystemTime)> = None;

    for entry in fs::read_dir(sessions_dir)? {
        let entry = entry?;
        let path = entry.path();

        // セッションファイルのみ（.jsonファイル）
        if !path.is_file() || path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }

        // ファイルの更新時刻を取得
        if let Ok(metadata) = fs::metadata(&path) {
            if let Ok(modified) = metadata.modified() {
                // since以降に更新されたファイルのみ
                if let Some(since_time) = since {
                    let since_sys = std::time::SystemTime::from(since_time);
                    if modified <= since_sys {
                        continue;
                    }
                }

                // 最新のファイルを記録
                if let Some((_, latest_time)) = &latest_file {
                    if modified > *latest_time {
                        latest_file = Some((path, modified));
                    }
                } else {
                    latest_file = Some((path, modified));
                }
            }
        }
    }

    Ok(latest_file.map(|(path, _)| path))
}

/// セッションファイルをパースしてSessionHistoryに変換
fn parse_session_file(file_path: &Path) -> Result<SessionHistory> {
    let content = fs::read_to_string(file_path)
        .with_context(|| format!("Failed to read session file: {}", file_path.display()))?;

    let session: Value = serde_json::from_str(&content)
        .with_context(|| format!("Failed to parse session file: {}", file_path.display()))?;

    // セッションIDを取得
    let session_id = session.get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();

    // 開始時刻を取得
    let started_at = session.get("created_at")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            OffsetDateTime::now_utc()
                .format(&time::format_description::well_known::Rfc3339)
                .unwrap_or_else(|_| "unknown".to_string())
        });

    // 終了時刻を取得
    let ended_at = session.get("updated_at")
        .or_else(|| session.get("ended_at"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    // メッセージ履歴からプロンプトとイベントを抽出
    let mut events = Vec::new();
    let mut prompt = String::new();
    let mut total_tool_uses = 0;
    let mut files_modified = Vec::new();

    if let Some(messages) = session.get("messages").and_then(|v| v.as_array()) {
        for (idx, message) in messages.iter().enumerate() {
            let role = message.get("role").and_then(|v| v.as_str()).unwrap_or("");
            let timestamp = message.get("timestamp")
                .and_then(|v| v.as_str())
                .unwrap_or(&started_at)
                .to_string();

            match role {
                "user" => {
                    // 最初のユーザーメッセージをプロンプトとする
                    if idx == 0 || prompt.is_empty() {
                        if let Some(content) = message.get("content") {
                            if let Some(text) = content.as_str() {
                                prompt = text.to_string();
                            } else if let Some(arr) = content.as_array() {
                                // contentが配列の場合、textブロックを探す
                                for block in arr {
                                    if let Some(text) = block.get("text").and_then(|v| v.as_str()) {
                                        prompt = text.to_string();
                                        break;
                                    }
                                }
                            }
                        }
                    }
                }
                "assistant" => {
                    if let Some(content) = message.get("content") {
                        if let Some(arr) = content.as_array() {
                            for block in arr {
                                let block_type = block.get("type").and_then(|v| v.as_str());

                                match block_type {
                                    Some("text") => {
                                        if let Some(text) = block.get("text").and_then(|v| v.as_str()) {
                                            if !text.trim().is_empty() {
                                                events.push(SessionEvent::AssistantMessage {
                                                    text: text.to_string(),
                                                    timestamp: timestamp.clone(),
                                                });
                                            }
                                        }
                                    }
                                    Some("tool_use") => {
                                        total_tool_uses += 1;

                                        let name = block.get("name")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("unknown")
                                            .to_string();

                                        // ファイル編集ツールの場合、ファイルパスを記録
                                        if matches!(name.as_str(), "Edit" | "Write") {
                                            if let Some(input) = block.get("input") {
                                                if let Some(file_path) = input.get("file_path").and_then(|v| v.as_str()) {
                                                    if !files_modified.contains(&file_path.to_string()) {
                                                        files_modified.push(file_path.to_string());
                                                    }
                                                }
                                            }
                                        }

                                        events.push(SessionEvent::ToolUse {
                                            name,
                                            timestamp: timestamp.clone(),
                                            input: block.get("input").cloned(),
                                        });
                                    }
                                    Some("thinking") => {
                                        if let Some(thinking) = block.get("thinking").and_then(|v| v.as_str()) {
                                            events.push(SessionEvent::ThinkingBlock {
                                                content: thinking.to_string(),
                                                timestamp: timestamp.clone(),
                                            });
                                        }
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }

    Ok(SessionHistory {
        session_id,
        started_at,
        ended_at,
        prompt: if prompt.is_empty() {
            "(Interactive session)".to_string()
        } else {
            prompt
        },
        events,
        total_tool_uses,
        files_modified,
    })
}

/// インタラクティブモード終了後に最新のセッションをインポート
pub fn import_latest_session(since: Option<OffsetDateTime>) -> Result<Option<SessionHistory>> {
    let sessions_dir = get_claude_sessions_dir()?;

    if let Some(latest_file) = get_latest_session_file(&sessions_dir, since)? {
        let history = parse_session_file(&latest_file)?;
        Ok(Some(history))
    } else {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_claude_sessions_dir() {
        let result = get_claude_sessions_dir();
        assert!(result.is_ok());
    }
}
