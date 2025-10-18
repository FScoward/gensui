# Gensui（元帥）

Gensuiは、複数のAIコーディングエージェントを同時に扱うためのTUI（Text User Interface）ベースのマルチワーカー管理ツールです。Issue対応のばらつきやコンテキストスイッチの負担を減らし、定義済みワークフローに沿った自動処理で品質と速度を両立します。

## 背景と課題
- 複数Issueを並行していると進捗が散逸しやすい
- ブラウザやターミナルのタブが増え、現在地が分からなくなる
- コマンド操作を毎回手で行うのは煩雑
- 担当者によって対応手順が異なり品質が揺らぐ

## ソリューション概要
- k9s風キーバインドを備えたTUIダッシュボード
- 各ワーカーを独立した`git worktree`上で稼働させてコンフリクトを回避
- Claude Code / Codex / Cursor / Aiderなど、複数エージェントを並列実行
- ワークフロー定義に従い、分析→実装→テストといったステップを順番に遂行
- リアルタイムでステータス・編集中ファイル・トークン使用量・ログを可視化

## 想定ユースケース
1. **複数Issueの同時進行**: Issueごとにワーカーを割り当て、進捗を一覧で管理
2. **アプローチ比較実験**: 同一Issueを複数エージェントで試行しアウトプットを比較
3. **チーム開発の標準化**: 担当者ごとの作業をワークフローで統一し品質を平準化

対応エージェント例: Claude Code、OpenAI Codex、Cursor、Aider など。

## TUIデザイン
```
┌─ Gensui [workers] ─────────────────────────────────────────┐
│ NAME      STATUS   ISSUE   AGENT        WORKTREE     BRANCH │
│ worker-1  Running  #123    Claude Code  .wt/wt-1     fix123 │
│ worker-2  Paused   #456    Codex        .wt/wt-2     feat   │
│ ...                                                    ... │
├────────────────────────────────────────────────────────────┤
│ <0> all <1> running <2> paused <3> failed <4> idle          │
│ <enter> view <d> delete <l> logs <p> pause <r> restart <:>  │
└────────────────────────────────────────────────────────────┘
```

- **メインビュー**: ワーカーの一覧・フィルタ・ソート・カラーコード（緑=Running, 黄=Paused, 赤=Failed, 灰=Idle）
- **詳細ビュー**: Issue情報、ブランチ、実行中ステップ、セッションIDなどを表示
- **ログビュー**: `follow`/`wrap`を備えたリアルタイムログモニタ
- **キーバインド**: `Enter` 詳細、`n` 新規、`p` 一時停止、`l` ログ、`:` コマンドモード、`/` フィルタ、`q` 終了

## ワーカーライフサイクル
1. Issue番号を指定し、GitHub等からメタ情報を取得
2. `git worktree add .worktrees/wt-{id} -b feature/issue-{number}`で専用環境を生成
3. ベースブランチから新規ブランチを作成してチェックアウト
4. 選択したエージェントをworktree内でHeadless起動し、Issueテンプレートをプロンプトとして投入
5. 定義済みワークフローを順次実行し、各ステップの結果をTUIへストリーミング
6. 正常終了時はworktreeの削除/保持を選択、異常終了時はworktreeを保持してデバッグに備える

## エージェント制御アプローチ
| アプローチ | 概要 | 長所 | 短所 | 用途 |
|-------------|------|------|------|------|
| Headless CLI (推奨) | `npx @anthropic-ai/claude-code@latest --output-format stream-json`などを外部プロセスとして起動 | 並行実行とリアルタイム監視に最適、言語自由度が高い | プロセス管理とエラーハンドリングが複雑 | Gensui本体実装
| Agent SDK | `@anthropic-ai/claude-agent-sdk`等でAsyncIteratorを利用 | 型安全・公式サポート | Node.js依存、カスタマイズ性やや低い | 追加オプションとして
| MCP Server | Claude Code内のプラグインとして提供 | セットアップ簡単、エディタ統合が容易 | 独立TUIには不向き、並列性が低い | Claude Code拡張用途

GensuiではHeadless CLI方式を標準とし、`command-group`でプロセスグルーピング、JSON Lines出力のパース、ANSI除去等でTUIに連携します。

## パーミッションと安全性
- 基本ポリシー: `--allowedTools "Read,Write,Edit,Bash,Grep" --permission-mode acceptEdits`
- `.claude/settings.json`でプロジェクト固有ルールをホワイトリスト/ブラックリスト管理
- 機密ファイル (`.env`, `secrets/**`) へのアクセスは明示的に拒否
- 危険コマンド (`rm`, `curl`など) は許可制。実行前に差分やコマンドを確認するガードを実装
- Enterprise → プロジェクト共有 → ローカル → ユーザー設定の優先順位で適用

## セッション / コンテキスト管理
- Headlessモードはステートレスなので、`session_id`を取得・保存して`--resume`で継続
- worktreeごとに`.session`と`CLAUDE.md`を配置し、永続コンテキストとターン数を追跡
- ターン数やトークン使用量が閾値を超えたら警告し、`/compact`などの圧縮コマンドを推奨
- Agent SDK利用時は`forkSession`で別アプローチを生成し比較検証を容易に

## 技術スタック候補
- 言語: RustまたはGo（並行処理とパフォーマンス重視）
- TUI: Rustなら`ratatui`、Goなら`tview`/`bubbletea`
- Git操作: `git2`クレート／`git` CLIラッパ
- 非同期I/O: Rust `tokio`、Go `goroutine`
- 設定: YAML/TOMLでワークフロー定義、`.claude`ディレクトリにセッション情報を保存

## 実装ロードマップ
- **Phase 1 (MVP)**: メインTUI、ワーカー作成/削除、worktree自動化、Claude Code対応
- **Phase 2**: 詳細/ログビュー、複数エージェント対応、ワークフロー定義、検索・フィルタ機能
- **Phase 3**: CPU/メモリ/トークン監視、自動リトライ、メトリクス分析、プラグイン拡張

## 関連プロジェクトからの学び
- **Vibe Kanban** (Rust + React): `command-group`によるプロセス制御、非同期ストリーム処理、エージェントプロファイル管理、git自動化
- **Claude Task Master** (TypeScript): Agent SDK + MCP統合、PRD解析によるタスク自動生成、マルチプロバイダー抽象化

これらのベストプラクティスを取り込みつつ、GensuiはTUI特化・git worktree連携・リアルタイム監視で差別化を図ります。

## 次のステップ
- [x] Rust + `ratatui`でのプロトタイプ実装
- [x] worktree管理ライブラリ/ラッパの整備
- [x] Claude Code Headless制御用モジュール作成
- [ ] 基本ワークフロー（分析→実装→テスト）をテンプレート化
- [ ] 早期ユーザーフィードバックの収集と設計改善

Gensuiによって、AIエージェントを活用したマルチIssue処理を安全かつ効率的に進めましょう。

## MVPプロトタイプの起動方法

開発用の最小構成TUIを用意しました。`cargo run`で起動すると、実際に`git worktree`を作成／削除しながら、簡易的なエージェント処理（シミュレーション）とログストリームを確認できます。

### 実行

```
cargo run
```

### キーバインド

- `q`: アプリケーション終了
- `c`: 新しいワーカーのプロビジョンを実行（git worktree追加＋エージェント起動）
- `d`: 選択中のワーカーを削除
- `r`: 選択中ワーカーを再起動（ワークフローを再実行）
- `a`: ステータスフィルタを循環（All → Running → Paused → Failed → Idle → All）
- `w`: 利用するワークフローを切り替え（`workflows.json`で定義）
- `i`: 自由指示を入力し、そのままClaudeに送信
- `j` / `k` または `↑` / `↓`: 行の移動
- `l`: アクションログのモーダル表示切り替え
- `h`: ヘルプモーダル表示切り替え
- `Shift+C`: ログを圧縮（古いログを上限4件まで削除）

### ワークフロー設定

リポジトリ直下の`workflows.json`からワークフローを読み込みます。ファイルが存在しない・空の場合はデフォルトの3ステップ（分析→実装→テスト）が自動挿入されます。

```json
{
  "default_workflow": "default",
  "workflows": [
    {
      "name": "default",
      "description": "分析→実装→テストの標準フロー",
      "steps": [
        { "name": "分析", "command": "echo '[分析] Issueを解析しています'" },
        { "name": "実装", "command": "echo '[実装] 変更を適用中'" },
        { "name": "テスト", "command": "echo '[テスト] テストを実行中'" }
      ]
    }
  ]
}
```

ヘッダ／フッタに現在選択中のワークフロー名が表示され、`w`キーで順次切り替え可能です。ワーカー作成時には選択中のワークフローが適用され、各ステップのコマンド実行ログが`Logs`モーダルから確認できます。

#### Claude Code連携

ステップに`claude`ブロックを定義すると、Claude Code CLIをheadless実行します。CLIへのパスは環境変数`GENSUI_CLAUDE_BIN`（デフォルト: `claude`）で指定します。例:

```json
{
  "name": "Claude分析",
  "description": "Claude CodeにIssueの影響範囲をまとめさせる",
  "claude": {
    "prompt": "Issue {{issue}} について、影響範囲と懸念点を3点以内でまとめてください。",
    "model": "sonnet",
    "permission_mode": "plan",
    "allowed_tools": [],
    "extra_args": ["--max-output-tokens", "800"]
  }
}
```

テンプレートでは`{{issue}}`、`{{branch}}`、`{{worktree}}`、`{{worker}}`が利用できます。`extra_args`はCLI引数をそのまま追加し、`{{prompt}}`や`{{workdir}}`プレースホルダを埋め込みます。

> ⚠️ Claude CLIのバージョンによりフラグ名が異なる場合があります。必要に応じて`extra_args`側でフル引数を指定してください。非ゼロ終了の場合はステップが`Failed`となり、stderr/stdoutをログに記録します。

> ℹ️ `.gensui/state/` 以下にワーカー状態とアクションログをJSONで保存します。再起動すると直近64件のアクションログと各ワーカーのステップ履歴が復元されます。

### 今後の発展余地

- `App::on_tick`内で実ワーカーのポーリングと状態更新を実装
- 実エージェント（Claude Code等）の出力をストリーミングし、ログビューに反映
- Action Logを永続化し、セッション再開時の履歴を復元
- ワークフロー定義に応じた前後処理（例: Issueメタ情報の取得、テストカバレッジ収集）
