/// セッション履歴のインポート機能
///
/// インタラクティブモード終了後にClaudeのセッションファイルから
/// 履歴を読み取ってSessionHistoryに変換する

use anyhow::{Context, Result};
use serde_json::Value;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use time::OffsetDateTime;

use crate::state::{SessionEvent, SessionHistory};

/// プロジェクトパスを正規化（/home/user/gensui → -home-user-gensui）
fn normalize_project_path(path: &Path) -> String {
    path.to_string_lossy()
        .replace('/', "-")
        .replace('\\', "-")
}

/// Claudeのプロジェクトディレクトリを取得
fn get_claude_projects_dir(project_path: &Path) -> Result<PathBuf> {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .context("HOME or USERPROFILE environment variable not set")?;

    // Check custom CLAUDE_CONFIG_DIR first
    let claude_dir = if let Ok(custom_dir) = std::env::var("CLAUDE_CONFIG_DIR") {
        PathBuf::from(custom_dir)
    } else {
        PathBuf::from(home).join(".claude")
    };

    let normalized = normalize_project_path(project_path);
    Ok(claude_dir.join("projects").join(normalized))
}

/// ディレクトリ内の最新のセッションファイルを取得
fn get_latest_session_file(sessions_dir: &Path, since: Option<OffsetDateTime>) -> Result<Option<PathBuf>> {
    if !sessions_dir.exists() {
        return Ok(None);
    }

    let mut latest_file: Option<(PathBuf, std::time::SystemTime)> = None;
    let mut file_count = 0;

    for entry in fs::read_dir(sessions_dir)? {
        let entry = entry?;
        let path = entry.path();

        // セッションファイルのみ（.jsonlファイル）
        if !path.is_file() || path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
            continue;
        }

        file_count += 1;
        eprintln!("Debug: Found .jsonl file: {}", path.display());

        // ファイルの更新時刻を取得
        if let Ok(metadata) = fs::metadata(&path) {
            if let Ok(modified) = metadata.modified() {
                eprintln!("Debug:   Modified: {:?}", modified);

                // since以降に更新されたファイルのみ
                if let Some(since_time) = since {
                    let since_sys = std::time::SystemTime::from(since_time);
                    eprintln!("Debug:   Since: {:?}", since_sys);
                    if modified <= since_sys {
                        eprintln!("Debug:   Skipped (too old)");
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

    eprintln!("Debug: Total .jsonl files found: {}", file_count);
    Ok(latest_file.map(|(path, _)| path))
}

/// JSONLセッションファイルをパースしてSessionHistoryに変換
fn parse_session_file(file_path: &Path) -> Result<SessionHistory> {
    let file = fs::File::open(file_path)
        .with_context(|| format!("Failed to open session file: {}", file_path.display()))?;
    let reader = BufReader::new(file);

    let mut session_id = String::from("unknown");
    let mut started_at = String::new();
    let mut ended_at: Option<String> = None;
    let mut events = Vec::new();
    let mut prompt = String::new();
    let mut total_tool_uses = 0;
    let mut files_modified = Vec::new();
    let mut first_timestamp = None;
    let mut last_timestamp = None;

    // JSONL形式（1行1イベント）をパース
    for (line_num, line) in reader.lines().enumerate() {
        let line = line.with_context(|| {
            format!("Failed to read line {} from {}", line_num + 1, file_path.display())
        })?;

        if line.trim().is_empty() {
            continue;
        }

        let event: Value = serde_json::from_str(&line).with_context(|| {
            format!(
                "Failed to parse JSON on line {} in {}",
                line_num + 1,
                file_path.display()
            )
        })?;

        // セッションIDを取得（最初の行から）
        if session_id == "unknown" {
            if let Some(id) = event.get("sessionId").and_then(|v| v.as_str()) {
                session_id = id.to_string();
            }
        }

        // タイムスタンプを記録
        if let Some(timestamp) = event.get("timestamp").and_then(|v| v.as_str()) {
            if first_timestamp.is_none() {
                first_timestamp = Some(timestamp.to_string());
            }
            last_timestamp = Some(timestamp.to_string());
        }

        // イベントタイプを取得
        let event_type = event.get("type").and_then(|v| v.as_str());
        let timestamp = event
            .get("timestamp")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        match event_type {
            Some("user") => {
                // ユーザーメッセージからプロンプトを抽出
                if let Some(message) = event.get("message") {
                    if let Some(content) = message.get("content") {
                        if let Some(text) = content.as_str() {
                            // 最初の実質的なユーザーメッセージをプロンプトとする（"Warmup"は除外）
                            if prompt.is_empty() && text != "Warmup" {
                                prompt = text.to_string();
                            }
                        }
                    }
                }
            }
            Some("assistant") => {
                if let Some(message) = event.get("message") {
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
            }
            Some("tool_use") => {
                total_tool_uses += 1;

                let name = event
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
                    .to_string();

                // ファイル編集ツールの場合、ファイルパスを記録
                if matches!(name.as_str(), "Edit" | "Write") {
                    if let Some(input) = event.get("input") {
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
                    input: event.get("input").cloned(),
                });
            }
            Some("tool_result") => {
                if let Some(name) = event.get("name").and_then(|v| v.as_str()) {
                    events.push(SessionEvent::ToolResult {
                        name: name.to_string(),
                        timestamp: timestamp.clone(),
                        output: event
                            .get("output")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string()),
                    });
                }
            }
            _ => {}
        }
    }

    // 開始・終了時刻を設定
    started_at = first_timestamp.unwrap_or_else(|| {
        OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_else(|_| "unknown".to_string())
    });
    ended_at = last_timestamp;

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
pub fn import_latest_session(
    project_path: &Path,
    since: Option<OffsetDateTime>,
) -> Result<Option<SessionHistory>> {
    let sessions_dir = get_claude_projects_dir(project_path)?;

    eprintln!("Debug: Looking for sessions in: {}", sessions_dir.display());
    eprintln!("Debug: Sessions dir exists: {}", sessions_dir.exists());

    if let Some(latest_file) = get_latest_session_file(&sessions_dir, since)? {
        eprintln!("Debug: Found session file: {}", latest_file.display());
        let history = parse_session_file(&latest_file)?;
        Ok(Some(history))
    } else {
        eprintln!("Debug: No session file found");
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_project_path() {
        let path = Path::new("/home/user/gensui");
        assert_eq!(normalize_project_path(path), "-home-user-gensui");
    }

    #[test]
    fn test_get_claude_projects_dir() {
        let project_path = Path::new("/home/user/gensui");
        let result = get_claude_projects_dir(project_path);
        assert!(result.is_ok());
    }
}
