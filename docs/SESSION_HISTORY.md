# セッション履歴機能

## 概要

Gensuiはクlaude Codeの各実行セッションの詳細な履歴を自動的に記録・保存します。これにより、「Interactive Claude Code Sessionで何をしたか」が常に追跡可能になります。

## 実装された機能

### 1. セッション履歴データ構造 (`src/state.rs`)

```rust
pub struct SessionHistory {
    pub session_id: String,           // セッションID
    pub started_at: String,            // 開始時刻 (RFC3339)
    pub ended_at: Option<String>,      // 終了時刻 (RFC3339)
    pub prompt: String,                // ユーザーが入力したプロンプト
    pub events: Vec<SessionEvent>,     // セッション内のイベント
    pub total_tool_uses: usize,        // ツール使用回数
    pub files_modified: Vec<String>,   // 編集されたファイル一覧
}

pub enum SessionEvent {
    ToolUse { name, timestamp, input },        // ツール使用
    ToolResult { name, timestamp, output },    // ツール結果
    AssistantMessage { text, timestamp },      // Claudeのメッセージ
    ThinkingBlock { content, timestamp },      // 思考プロセス
    Result { text, is_error, timestamp },      // 最終結果
    Error { message, timestamp },              // エラー
}
```

### 2. 自動記録 (`src/worker/mod.rs`)

Claude Codeの`--output-format stream-json`出力をリアルタイムでパースし、以下の情報を記録：

- **ツール使用**: Read, Write, Edit, Bash, Grep などのツール呼び出し
- **ファイル編集**: WriteやEditツールで変更されたファイルパス
- **思考プロセス**: Claudeの推論・分析ブロック
- **メッセージ**: Claudeからの応答メッセージ
- **結果とエラー**: 実行結果とエラー情報

### 3. 永続化と復元

- **保存先**: `.gensui/state/workers/{worker_name}.json`
- **自動保存**: 各ステップ完了時、アプリ終了時
- **自動復元**: アプリ起動時に過去のセッション履歴を自動的にロード

### 4. セッション継続

- セッションIDを保存し、次回実行時に`--continue`フラグで同一セッションを継続
- 複数回の`i`キー実行でも、同じセッションコンテキストを維持

## 使用例

### セッション履歴の確認

現在のところ、セッション履歴は`.gensui/state/workers/*.json`ファイルで確認できます：

```bash
# ワーカーのセッション履歴を表示
cat .gensui/state/workers/worker-name.json | jq '.session_history'
```

### JSONフォーマット例

```json
{
  "session_history": [
    {
      "session_id": "abc123...",
      "started_at": "2025-01-15T10:30:00Z",
      "ended_at": "2025-01-15T10:32:15Z",
      "prompt": "Add a new feature to handle user authentication",
      "events": [
        {
          "ThinkingBlock": {
            "content": "I'll need to create an auth module...",
            "timestamp": "2025-01-15T10:30:05Z"
          }
        },
        {
          "ToolUse": {
            "name": "Write",
            "timestamp": "2025-01-15T10:30:10Z",
            "input": {
              "file_path": "src/auth.rs",
              "content": "..."
            }
          }
        },
        {
          "Result": {
            "text": "I've created the authentication module.",
            "is_error": false,
            "timestamp": "2025-01-15T10:32:15Z"
          }
        }
      ],
      "total_tool_uses": 3,
      "files_modified": ["src/auth.rs", "src/lib.rs"]
    }
  ]
}
```

## 今後の拡張

- **UIでのセッション履歴表示**: TUI上でセッション履歴を閲覧できる専用ビューの追加
- **セッション検索**: 特定のファイル編集やツール使用を検索
- **セッション比較**: 複数セッション間の変更を比較
- **エクスポート機能**: セッション履歴をMarkdownやHTMLで出力

## 技術詳細

### パース処理

`run_claude_command`関数 (src/worker/mod.rs:1482-1778) でJSON出力をパース：

1. `stream-json`形式の各行をリアルタイムでパース
2. `type`フィールドに基づいてイベントを分類
3. `SessionEvent`として構造化
4. `SessionHistory`にイベントを蓄積

### セッションID管理

- Claude Codeが出力する`session_id`を抽出
- `WorkerSnapshot`に保存
- 次回実行時に`--continue`フラグで継続

### ファイル変更追跡

- `Write`および`Edit`ツールの`input.file_path`を抽出
- 重複を除いて`files_modified`リストに追加
