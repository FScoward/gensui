mod config;
mod state;
mod worker;

use std::collections::{HashMap, VecDeque};
use std::io::{self, Stdout};
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Clear, Paragraph, Row, Table, Wrap};

use crate::config::{Config, Workflow};
use crate::state::{ActionLogEntry, StateStore};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use worker::{
    CreateWorkerRequest, ExistingWorktree, PermissionDecision, PermissionRequest, WorkerEvent,
    WorkerEventReceiver, WorkerHandle, WorkerId, WorkerSnapshot, WorkerStatus,
    list_existing_worktrees, spawn_worker_system,
};

const GLOBAL_LOG_CAPACITY: usize = 64;

// Available tools for Claude Code
struct ToolDef {
    name: &'static str,
    description: &'static str,
}

const AVAILABLE_TOOLS: &[ToolDef] = &[
    ToolDef {
        name: "Read",
        description: "ファイル読み取り",
    },
    ToolDef {
        name: "Write",
        description: "ファイル書き込み",
    },
    ToolDef {
        name: "Edit",
        description: "ファイル編集",
    },
    ToolDef {
        name: "Glob",
        description: "ファイル検索",
    },
    ToolDef {
        name: "Grep",
        description: "コード検索",
    },
    ToolDef {
        name: "Bash",
        description: "シェルコマンド実行",
    },
    ToolDef {
        name: "WebFetch",
        description: "Web取得",
    },
    ToolDef {
        name: "NotebookEdit",
        description: "Jupyter編集",
    },
];

fn main() -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    crossterm::execute!(stdout, crossterm::terminal::EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let result = run_app(&mut terminal);
    disable_raw_mode()?;
    crossterm::execute!(
        terminal.backend_mut(),
        crossterm::terminal::LeaveAlternateScreen,
        crossterm::cursor::Show
    )?;
    result
}

fn run_app(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<()> {
    let mut app = App::new()?;
    let mut last_tick = Instant::now();
    let tick_rate = Duration::from_millis(50);

    loop {
        // Check for interactive mode request
        if let Some(request) = app.pending_interactive_mode.take() {
            // Suspend TUI
            disable_raw_mode()?;
            crossterm::execute!(
                terminal.backend_mut(),
                crossterm::terminal::LeaveAlternateScreen,
                crossterm::cursor::Show
            )?;

            // Display info and wait for user
            println!("\n=== Interactive Claude Code Session ===");
            println!("Worker: {}", request.worker_name);
            println!("Worktree: {}", request.worktree_path.display());
            println!("\nPress Enter to start Claude Code CLI...");

            let mut input = String::new();
            io::stdin().read_line(&mut input)?;

            // Launch Claude Code CLI (non-headless mode)
            let status = Command::new("claude")
                .current_dir(&request.worktree_path)
                .status();

            match status {
                Ok(exit_status) => {
                    println!("\nClaude Code exited with: {:?}", exit_status);
                }
                Err(err) => {
                    println!("\nFailed to launch Claude Code: {}", err);
                    println!("Make sure 'claude' command is available in your PATH");
                }
            }

            println!("\nPress Enter to return to TUI...");
            input.clear();
            io::stdin().read_line(&mut input)?;

            // Resume TUI
            enable_raw_mode()?;
            crossterm::execute!(
                terminal.backend_mut(),
                crossterm::terminal::EnterAlternateScreen
            )?;
            terminal.clear()?;

            app.push_log(format!(
                "インタラクティブセッションから復帰しました ({})",
                request.worker_name
            ));

            continue;
        }

        terminal.draw(|frame| app.render(frame))?;

        let timeout = tick_rate
            .checked_sub(last_tick.elapsed())
            .unwrap_or_else(|| Duration::from_secs(0));

        if event::poll(timeout)? {
            if let Event::Key(key_event) = event::read()? {
                if key_event.kind == KeyEventKind::Press {
                    if app.handle_key(key_event) {
                        break;
                    }
                }
            }
        }

        if last_tick.elapsed() >= tick_rate {
            app.on_tick();
            last_tick = Instant::now();
        }
    }

    Ok(())
}

struct App {
    repo_root: std::path::PathBuf,
    manager: WorkerHandle,
    event_rx: WorkerEventReceiver,
    state_store: StateStore,
    workflows: Vec<Workflow>,
    selected_workflow_idx: usize,
    workers: Vec<WorkerView>,
    selected: usize,
    show_help: bool,
    show_logs: bool,
    log_messages: VecDeque<String>,
    log_scroll: usize,
    status_filter: Option<WorkerStatus>,
    input_mode: Option<InputMode>,
    log_view_mode: LogViewMode,
    selected_step: usize,
    animation_frame: usize,
    permission_prompt: Option<PermissionPromptState>,
    permission_tracker: HashMap<u64, PermissionTrackerEntry>,
    pending_interactive_mode: Option<InteractiveRequest>,
    auto_scroll_logs: bool,
}

struct InteractiveRequest {
    worker_name: String,
    worktree_path: PathBuf,
}

impl App {
    fn new() -> Result<Self> {
        let repo_root = std::env::current_dir().context("failed to determine repository root")?;
        let config_path = repo_root.join("workflows.json");
        let loaded = Config::load(&config_path).context("failed to load workflow configuration")?;
        let config = if loaded.workflows.is_empty() {
            Config::default()
        } else {
            loaded
        };

        let state_store = StateStore::new(repo_root.join(".gensui/state"))?;

        let mut workflows = config.workflows.clone();
        let default_idx = config
            .default_workflow
            .as_ref()
            .and_then(|name| workflows.iter().position(|wf| &wf.name == name))
            .unwrap_or(0);
        if workflows.is_empty() {
            workflows = Config::default().workflows;
        }
        let selected_workflow_idx = if workflows.is_empty() {
            0
        } else {
            default_idx.min(workflows.len() - 1)
        };

        // Don't restore workers to UI - they are not running in WorkerManager
        // Only restore action logs for history
        let mut log_messages = VecDeque::with_capacity(GLOBAL_LOG_CAPACITY);
        let history = state_store.load_action_log(GLOBAL_LOG_CAPACITY)?;
        for entry in history {
            log_messages.push_back(format_action_log(&entry));
        }

        let (manager, event_rx) = spawn_worker_system(repo_root.clone(), config)?;

        Ok(Self {
            repo_root,
            manager,
            event_rx,
            state_store,
            workflows,
            selected_workflow_idx,
            workers: Vec::new(),
            selected: 0,
            show_help: false,
            show_logs: false,
            log_messages,
            log_scroll: 0,
            status_filter: None,
            input_mode: None,
            log_view_mode: LogViewMode::Overview,
            selected_step: 0,
            animation_frame: 0,
            permission_prompt: None,
            permission_tracker: HashMap::new(),
            pending_interactive_mode: None,
            auto_scroll_logs: true,
        })
    }

    fn handle_key(&mut self, key_event: KeyEvent) -> bool {
        if self.permission_prompt.is_some() {
            let key_code = key_event.code;

            let mut should_submit = None;

            if let Some(prompt) = self.permission_prompt.as_mut() {
                match key_code {
                    KeyCode::Left | KeyCode::Right | KeyCode::Char('h') | KeyCode::Char('l') => {
                        if !key_event.modifiers.contains(KeyModifiers::CONTROL)
                            && !key_event.modifiers.contains(KeyModifiers::ALT)
                        {
                            prompt.toggle();
                        }
                    }
                    KeyCode::Char('y') if key_event.modifiers.is_empty() => {
                        should_submit = Some(PermissionDecision::Allow {
                            permission_mode: None,
                            allowed_tools: None,
                        });
                    }
                    KeyCode::Char('n') if key_event.modifiers.is_empty() => {
                        should_submit = Some(PermissionDecision::Deny);
                    }
                    KeyCode::Enter => {
                        should_submit = Some(prompt.selection.clone());
                    }
                    KeyCode::Esc => {
                        should_submit = Some(PermissionDecision::Deny);
                    }
                    _ => {}
                }
            }

            if let Some(decision) = should_submit {
                // If Allow, open tool selection modal
                match decision {
                    PermissionDecision::Allow { .. } => {
                        // Initialize tool selection with all tools unchecked
                        let mut tools = HashMap::new();
                        for tool_def in AVAILABLE_TOOLS {
                            tools.insert(tool_def.name.to_string(), false);
                        }

                        // Save worker_id and request_id before clearing permission_prompt
                        if let Some(prompt_state) = &self.permission_prompt {
                            let worker_id = prompt_state.worker_id;
                            let request_id = prompt_state.request.request_id;

                            // Clear permission_prompt to allow tool selection modal to receive key input
                            self.permission_prompt = None;

                            self.input_mode = Some(InputMode::ToolSelection {
                                tools,
                                selected_idx: 0,
                                permission_mode: "acceptEdits".to_string(),
                                worker_id,
                                request_id,
                            });
                        }
                    }
                    PermissionDecision::Deny => {
                        self.submit_permission_decision(decision);
                    }
                }
            }

            return false;
        }

        if let Some(mode) = self.input_mode.as_mut() {
            match mode {
                InputMode::FreePrompt {
                    buffer,
                    force_new,
                    permission_mode,
                } => match key_event.code {
                    KeyCode::Esc => {
                        self.input_mode = None;
                    }
                    KeyCode::Enter => {
                        let prompt = buffer.trim().to_string();
                        let is_force_new = *force_new;
                        let mode = permission_mode.clone();
                        self.input_mode = None;
                        if !prompt.is_empty() {
                            self.submit_free_prompt(prompt, is_force_new, mode);
                        } else {
                            self.push_log("空の指示は送信されませんでした".into());
                        }
                    }
                    KeyCode::Backspace => {
                        buffer.pop();
                    }
                    KeyCode::Tab => {
                        buffer.push('\t');
                    }
                    KeyCode::Char('p') if key_event.modifiers.contains(KeyModifiers::CONTROL) => {
                        // Cycle through: None -> "plan" -> "acceptEdits" -> None
                        *permission_mode = match permission_mode.as_deref() {
                            None => Some("plan".to_string()),
                            Some("plan") => Some("acceptEdits".to_string()),
                            Some("acceptEdits") => None,
                            _ => None,
                        };
                    }
                    KeyCode::Char(c) => {
                        if !key_event.modifiers.contains(KeyModifiers::CONTROL)
                            && !key_event.modifiers.contains(KeyModifiers::ALT)
                        {
                            buffer.push(c);
                        }
                    }
                    _ => {}
                },
                InputMode::CreateWorkerSelection { selected } => match key_event.code {
                    KeyCode::Esc => {
                        self.input_mode = None;
                    }
                    KeyCode::Enter => {
                        let choice = *selected;
                        self.input_mode = None;
                        if choice == 0 {
                            // Run workflow
                            self.enqueue_create_worker();
                        } else if choice == 1 {
                            // Free input - always create new worker
                            self.input_mode = Some(InputMode::FreePrompt {
                                buffer: String::new(),
                                force_new: true,
                                permission_mode: None,
                            });
                        } else {
                            // Use existing worktree
                            self.show_worktree_selection();
                        }
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        *selected = selected.saturating_sub(1);
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        *selected = (*selected + 1).min(2);
                    }
                    _ => {}
                },
                InputMode::WorktreeSelection {
                    worktrees,
                    selected,
                } => match key_event.code {
                    KeyCode::Esc => {
                        self.input_mode = None;
                    }
                    KeyCode::Enter => {
                        if let Some(worktree) = worktrees.get(*selected) {
                            let worktree_path = worktree.path.clone();
                            let branch = worktree.branch.clone();
                            self.input_mode = None;
                            self.enqueue_create_worker_with_worktree(worktree_path, branch);
                        }
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        *selected = selected.saturating_sub(1);
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        let max_idx = worktrees.len().saturating_sub(1);
                        *selected = (*selected + 1).min(max_idx);
                    }
                    _ => {}
                },
                InputMode::ToolSelection {
                    tools,
                    selected_idx,
                    permission_mode,
                    worker_id,
                    request_id,
                } => {
                    // Will implement key handling here
                    match key_event.code {
                        KeyCode::Esc => {
                            // Cancel - send Deny
                            if let Err(err) = self.manager.respond_permission(
                                *worker_id,
                                *request_id,
                                PermissionDecision::Deny,
                            ) {
                                self.push_log(format!("権限応答の送信に失敗しました: {err}"));
                            }
                            self.input_mode = None;
                        }
                        KeyCode::Enter => {
                            // Submit with selected tools and permission_mode
                            let selected_tools: Vec<String> = tools
                                .iter()
                                .filter_map(|(name, &checked)| if checked { Some(name.clone()) } else { None })
                                .collect();

                            let final_decision = PermissionDecision::Allow {
                                permission_mode: Some(permission_mode.clone()),
                                allowed_tools: if selected_tools.is_empty() { None } else { Some(selected_tools) },
                            };

                            let wid = *worker_id;
                            let rid = *request_id;

                            // Clear input mode first to release borrow
                            self.input_mode = None;

                            if let Err(err) = self.manager.respond_permission(wid, rid, final_decision.clone()) {
                                self.push_log(format!("権限応答の送信に失敗しました: {err}"));
                            } else {
                                let worker_name = self.worker_name_by_id(wid).unwrap_or_else(|| format!("worker-{}", wid.0));
                                self.permission_tracker.insert(
                                    rid,
                                    PermissionTrackerEntry {
                                        worker_name,
                                        step_name: format!("Tool selection"),
                                    },
                                );
                            }
                        }
                        KeyCode::Up | KeyCode::Char('k') => {
                            *selected_idx = selected_idx.saturating_sub(1);
                        }
                        KeyCode::Down | KeyCode::Char('j') => {
                            let max_idx = tools.len(); // tools.len() is for permission_mode toggle
                            *selected_idx = (*selected_idx + 1).min(max_idx);
                        }
                        KeyCode::Char(' ') => {
                            // Toggle tool selection
                            if *selected_idx < tools.len() {
                                let tool_name = AVAILABLE_TOOLS[*selected_idx].name;
                                if let Some(checked) = tools.get_mut(tool_name) {
                                    *checked = !*checked;
                                }
                            } else {
                                // Toggle permission_mode
                                *permission_mode = match permission_mode.as_str() {
                                    "acceptEdits" => "bypassPermissions".to_string(),
                                    _ => "acceptEdits".to_string(),
                                };
                            }
                        }
                        _ => {}
                    }
                }
            }
            return false;
        }

        match key_event.code {
            KeyCode::Char('q') => return true,
            KeyCode::Char('c') => self.show_create_selection(),
            KeyCode::Char('d') => self.enqueue_delete_worker(),
            KeyCode::Char('r') => self.enqueue_restart_worker(),
            KeyCode::Char('i') => self.start_free_prompt(),
            KeyCode::Char('h') => self.toggle_help(),
            KeyCode::Char('l') => self.toggle_logs(),
            KeyCode::Char('w') => self.cycle_workflow(),
            KeyCode::Char('a') => self.cycle_filter(),
            KeyCode::Tab => {
                if self.show_logs {
                    self.switch_log_tab_next();
                }
            }
            KeyCode::BackTab => {
                if self.show_logs {
                    self.switch_log_tab_prev();
                }
            }
            KeyCode::Enter => {
                if self.show_logs && self.log_view_mode == LogViewMode::Overview {
                    self.enter_detail_from_overview();
                }
            }
            KeyCode::Esc => {
                if self.show_logs
                    && (self.log_view_mode == LogViewMode::Detail
                        || self.log_view_mode == LogViewMode::Raw)
                {
                    self.back_to_overview();
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if self.show_logs {
                    match self.log_view_mode {
                        LogViewMode::Overview => self.select_step_up(),
                        LogViewMode::Detail | LogViewMode::Raw => self.scroll_log_up(),
                    }
                } else {
                    self.select_previous();
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.show_logs {
                    match self.log_view_mode {
                        LogViewMode::Overview => self.select_step_down(),
                        LogViewMode::Detail | LogViewMode::Raw => self.scroll_log_down(),
                    }
                } else {
                    self.select_next();
                }
            }
            KeyCode::Home => {
                if self.show_logs {
                    self.scroll_log_home();
                } else {
                    self.select_first();
                }
            }
            KeyCode::End => {
                if self.show_logs {
                    self.scroll_log_end();
                } else {
                    self.select_last();
                }
            }
            KeyCode::PageUp => {
                if self.show_logs {
                    self.scroll_log_page_up();
                }
            }
            KeyCode::PageDown => {
                if self.show_logs {
                    self.scroll_log_page_down();
                }
            }
            KeyCode::Char('C') if key_event.modifiers.contains(KeyModifiers::SHIFT) => {
                self.compact_logs()
            }
            KeyCode::Char('I') if key_event.modifiers.contains(KeyModifiers::SHIFT) => {
                self.start_interactive_prompt()
            }
            KeyCode::Char('A') if key_event.modifiers.contains(KeyModifiers::SHIFT) => {
                self.toggle_auto_scroll()
            }
            _ => {}
        }

        false
    }

    fn enqueue_create_worker(&mut self) {
        let mut request = CreateWorkerRequest::default();
        request.workflow = self
            .workflows
            .get(self.selected_workflow_idx)
            .map(|wf| wf.name.clone());

        if let Err(err) = self.manager.create_worker(request) {
            self.push_log(format!("ワーカー作成に失敗しました: {err}"));
        }
    }

    fn enqueue_delete_worker(&mut self) {
        if let Some(id) = self.selected_worker_id() {
            // Check if this is an archived worker
            if let Some(worker) = self.workers.iter().find(|w| w.snapshot.id == id) {
                if worker.snapshot.status == WorkerStatus::Archived {
                    // For archived workers, just delete the state file
                    if let Err(err) = self.state_store.delete_worker(&worker.snapshot.name) {
                        self.push_log(format!("アーカイブ削除に失敗しました: {err}"));
                    } else {
                        // Remove from UI
                        if let Some(pos) = self.workers.iter().position(|w| w.snapshot.id == id) {
                            let worker = self.workers.remove(pos);
                            self.push_log(format!(
                                "アーカイブを削除しました: {}",
                                worker.snapshot.name
                            ));
                            self.clamp_selection();
                        }
                    }
                    return;
                }
            }

            if let Err(err) = self.manager.delete_worker(id) {
                self.push_log(format!("ワーカー削除に失敗しました ({:?}): {err}", id));
            }
        }
    }

    fn enqueue_restart_worker(&mut self) {
        if let Some(id) = self.selected_worker_id() {
            // Check if this is an archived worker
            if let Some(worker) = self.workers.iter().find(|w| w.snapshot.id == id) {
                if worker.snapshot.status == WorkerStatus::Archived {
                    self.push_log("アーカイブされたワーカーは再起動できません".to_string());
                    return;
                }
            }

            if let Err(err) = self.manager.restart_worker(id) {
                self.push_log(format!("ワーカー再起動に失敗しました ({:?}): {err}", id));
            }
        }
    }

    fn toggle_help(&mut self) {
        self.show_help = !self.show_help;
    }

    fn toggle_logs(&mut self) {
        self.show_logs = !self.show_logs;
        // Reset scroll and view mode when opening logs
        if self.show_logs {
            self.log_scroll = 0;
            self.log_view_mode = LogViewMode::Overview;
            self.selected_step = 0;
            // Clamp selected_step to valid range
            if let Some(view) = self.selected_worker_view() {
                if !view.structured_logs.is_empty() {
                    self.selected_step = self.selected_step.min(view.structured_logs.len() - 1);
                } else {
                    self.selected_step = 0;
                }
            } else {
                self.selected_step = 0;
            }
        }
    }

    fn scroll_log_up(&mut self) {
        self.log_scroll = self.log_scroll.saturating_sub(1);
        // Disable auto-scroll when manually scrolling up
        self.auto_scroll_logs = false;
    }

    fn scroll_log_down(&mut self) {
        let max_scroll = self.get_log_max_scroll();
        if self.log_scroll < max_scroll {
            self.log_scroll += 1;
        }
        // Re-enable auto-scroll if we reached the bottom
        if self.log_scroll >= max_scroll {
            self.auto_scroll_logs = true;
        }
    }

    fn scroll_log_home(&mut self) {
        self.log_scroll = 0;
        // Disable auto-scroll when jumping to top
        self.auto_scroll_logs = false;
    }

    fn scroll_log_end(&mut self) {
        self.log_scroll = self.get_log_max_scroll();
        // Re-enable auto-scroll when jumping to bottom
        self.auto_scroll_logs = true;
    }

    fn scroll_log_page_up(&mut self) {
        self.log_scroll = self.log_scroll.saturating_sub(10);
        // Disable auto-scroll when paging up
        self.auto_scroll_logs = false;
    }

    fn scroll_log_page_down(&mut self) {
        let max_scroll = self.get_log_max_scroll();
        let new_scroll = self.log_scroll.saturating_add(10);
        self.log_scroll = new_scroll.min(max_scroll);
        // Re-enable auto-scroll if we reached the bottom
        if self.log_scroll >= max_scroll {
            self.auto_scroll_logs = true;
        }
    }

    fn get_log_max_scroll(&self) -> usize {
        match self.log_view_mode {
            LogViewMode::Raw => self.log_messages.len().saturating_sub(1),
            _ => {
                if let Some(view) = self.selected_worker_view() {
                    view.logs.len().saturating_sub(1)
                } else {
                    self.log_messages.len().saturating_sub(1)
                }
            }
        }
    }

    fn compact_logs(&mut self) {
        while self.log_messages.len() > 4 {
            self.log_messages.pop_front();
        }
        self.push_log("アクションログを圧縮しました".into());
    }

    fn toggle_auto_scroll(&mut self) {
        self.auto_scroll_logs = !self.auto_scroll_logs;
        let status = if self.auto_scroll_logs {
            "ON"
        } else {
            "OFF"
        };
        self.push_log(format!("ログの自動スクロール: {}", status));
    }

    fn switch_log_tab_next(&mut self) {
        let next_mode = match self.log_view_mode {
            LogViewMode::Overview => LogViewMode::Detail,
            LogViewMode::Detail => LogViewMode::Raw,
            LogViewMode::Raw => LogViewMode::Overview,
        };

        // Only switch to Detail if a valid step is available
        if matches!(next_mode, LogViewMode::Detail) {
            if let Some(view) = self.selected_worker_view() {
                if !view.structured_logs.is_empty() {
                    // Clamp selected_step to valid range
                    self.selected_step = self.selected_step.min(view.structured_logs.len() - 1);
                    self.log_view_mode = next_mode;
                }
            }
        } else {
            self.log_view_mode = next_mode;
        }
        self.log_scroll = 0;
    }

    fn switch_log_tab_prev(&mut self) {
        let next_mode = match self.log_view_mode {
            LogViewMode::Overview => LogViewMode::Raw,
            LogViewMode::Raw => LogViewMode::Detail,
            LogViewMode::Detail => LogViewMode::Overview,
        };

        // Only switch to Detail if a valid step is available
        if matches!(next_mode, LogViewMode::Detail) {
            if let Some(view) = self.selected_worker_view() {
                if !view.structured_logs.is_empty() {
                    // Clamp selected_step to valid range
                    self.selected_step = self.selected_step.min(view.structured_logs.len() - 1);
                    self.log_view_mode = next_mode;
                }
            }
        } else {
            self.log_view_mode = next_mode;
        }
        self.log_scroll = 0;
    }

    fn select_step_up(&mut self) {
        self.selected_step = self.selected_step.saturating_sub(1);
    }

    fn select_step_down(&mut self) {
        if let Some(view) = self.selected_worker_view() {
            if !view.structured_logs.is_empty() {
                let max = view.structured_logs.len() - 1;
                self.selected_step = (self.selected_step + 1).min(max);
            }
        }
    }

    fn enter_detail_from_overview(&mut self) {
        // Only enter detail if a valid step is selected
        if let Some(view) = self.selected_worker_view() {
            if self.selected_step < view.structured_logs.len() {
                self.log_view_mode = LogViewMode::Detail;
                self.log_scroll = 0;
            }
        }
    }

    fn back_to_overview(&mut self) {
        self.log_view_mode = LogViewMode::Overview;
        self.log_scroll = 0;
    }

    fn cycle_filter(&mut self) {
        self.status_filter = match self.status_filter {
            None => Some(WorkerStatus::Running),
            Some(WorkerStatus::Running) => Some(WorkerStatus::Paused),
            Some(WorkerStatus::Paused) => Some(WorkerStatus::Failed),
            Some(WorkerStatus::Failed) => Some(WorkerStatus::Idle),
            Some(WorkerStatus::Idle) => Some(WorkerStatus::Archived),
            Some(WorkerStatus::Archived) => None,
        };
        self.push_log("ステータスフィルタを更新しました".into());
        self.selected = 0;
        self.clamp_selection();
    }

    fn cycle_workflow(&mut self) {
        if self.workflows.is_empty() {
            return;
        }
        self.selected_workflow_idx = (self.selected_workflow_idx + 1) % self.workflows.len();
        let name = self.current_workflow_name().to_string();
        let desc = self
            .workflows
            .get(self.selected_workflow_idx)
            .and_then(|wf| wf.description.as_deref())
            .unwrap_or("説明なし");
        self.push_log(format!(
            "使用するワークフローを '{}' に切り替えました ({})",
            name, desc
        ));
    }

    fn show_create_selection(&mut self) {
        self.input_mode = Some(InputMode::CreateWorkerSelection { selected: 0 });
    }

    fn show_worktree_selection(&mut self) {
        match list_existing_worktrees(&self.repo_root) {
            Ok(worktrees) => {
                if worktrees.is_empty() {
                    self.push_log("既存のworktreeが見つかりませんでした".to_string());
                    return;
                }
                self.input_mode = Some(InputMode::WorktreeSelection {
                    worktrees,
                    selected: 0,
                });
            }
            Err(err) => {
                self.push_log(format!("worktreeの一覧取得に失敗しました: {err}"));
            }
        }
    }

    fn enqueue_create_worker_with_worktree(
        &mut self,
        worktree_path: std::path::PathBuf,
        branch: String,
    ) {
        let mut request = CreateWorkerRequest::default();
        request.workflow = self
            .workflows
            .get(self.selected_workflow_idx)
            .map(|wf| wf.name.clone());
        request.existing_worktree = Some((worktree_path.clone(), branch.clone()));

        if let Err(err) = self.manager.create_worker(request) {
            self.push_log(format!("ワーカー作成に失敗しました: {err}"));
        } else {
            self.push_log(format!(
                "既存worktree '{}'でワーカーを作成しました",
                worktree_path.display()
            ));
        }
    }

    fn start_free_prompt(&mut self) {
        self.input_mode = Some(InputMode::FreePrompt {
            buffer: String::new(),
            force_new: false,
            permission_mode: None,
        });
    }

    fn start_interactive_prompt(&mut self) {
        // Get selected worker
        if let Some(worker_id) = self.selected_worker_id() {
            if let Some(worker) = self.workers.iter().find(|w| w.snapshot.id == worker_id) {
                // Check if archived
                if worker.snapshot.status == WorkerStatus::Archived {
                    self.push_log(
                        "アーカイブされたワーカーではインタラクティブモードを使用できません"
                            .to_string(),
                    );
                    return;
                }

                let worktree_path = self.repo_root.join(&worker.snapshot.worktree);
                let worker_name = worker.snapshot.name.clone();

                self.pending_interactive_mode = Some(InteractiveRequest {
                    worker_name,
                    worktree_path,
                });
            }
        } else {
            self.push_log("ワーカーを選択してください".to_string());
        }
    }

    fn submit_free_prompt(
        &mut self,
        prompt: String,
        force_new: bool,
        permission_mode: Option<String>,
    ) {
        let trimmed = prompt.trim();
        if trimmed.is_empty() {
            self.push_log("空の指示は送信されませんでした".into());
            return;
        }

        // Check if a worker is selected (only if not forcing new worker creation)
        if !force_new {
            let visible = self.visible_indices();
            if !visible.is_empty() && self.selected < visible.len() {
                // Worker is selected - send continuation to existing worker
                let worker_index = visible[self.selected];
                if let Some(worker) = self.workers.get(worker_index) {
                    // Check if archived
                    if worker.snapshot.status == WorkerStatus::Archived {
                        self.push_log(
                            "アーカイブされたワーカーには追加指示を送信できません".to_string(),
                        );
                        return;
                    }

                    let worker_id = worker.snapshot.id;
                    match self.manager.continue_worker(
                        worker_id,
                        trimmed.to_string(),
                        permission_mode.clone(),
                    ) {
                        Ok(_) => {
                            let mode_str = permission_mode_label(&permission_mode);
                            self.push_log(format!(
                                "追加指示を送信しました (worker-{}, {}): {}",
                                worker_id.0, mode_str, trimmed
                            ));
                        }
                        Err(err) => {
                            self.push_log(format!("追加指示の送信に失敗しました: {err}"));
                        }
                    }
                    return;
                }
            }
        }

        // No worker selected or force_new is true - create new worker
        let mut request = CreateWorkerRequest::default();
        request.free_prompt = Some(trimmed.to_string());
        request.permission_mode = permission_mode.clone();

        match self.manager.create_worker(request) {
            Ok(_) => {
                let mode_str = permission_mode_label(&permission_mode);
                self.push_log(format!(
                    "自由指示を送信しました ({}): {}",
                    mode_str, trimmed
                ));
            }
            Err(err) => {
                self.push_log(format!("自由指示の作成に失敗しました: {err}"));
            }
        }
    }

    fn submit_permission_decision(&mut self, decision: PermissionDecision) {
        if let Some(prompt) = self.permission_prompt.take() {
            if let Err(err) = self.manager.respond_permission(
                prompt.worker_id,
                prompt.request.request_id,
                decision,
            ) {
                self.push_log_with_worker(
                    Some(&prompt.worker_name),
                    format!("権限応答の送信に失敗しました: {err}"),
                );
            } else {
                self.permission_tracker.insert(
                    prompt.request.request_id,
                    PermissionTrackerEntry {
                        worker_name: prompt.worker_name,
                        step_name: prompt.request.step_name.clone(),
                    },
                );
            }
        }
    }

    fn handle_permission_requested(&mut self, id: WorkerId, request: PermissionRequest) {
        let worker_name = self
            .worker_name_by_id(id)
            .unwrap_or_else(|| format!("worker-{}", id.0));
        let tools_text = Self::describe_allowed_tools(&request.allowed_tools);
        let mode_text = permission_mode_label(&request.permission_mode).to_string();
        let step_name = request.step_name.clone();

        self.permission_prompt = Some(PermissionPromptState {
            worker_id: id,
            worker_name: worker_name.clone(),
            request,
            selection: PermissionDecision::Allow {
                permission_mode: None,
                allowed_tools: None,
            },
        });

        self.permission_tracker.insert(
            self.permission_prompt
                .as_ref()
                .map(|prompt| prompt.request.request_id)
                .unwrap(),
            PermissionTrackerEntry {
                worker_name: worker_name.clone(),
                step_name: step_name.clone(),
            },
        );

        self.add_worker_log(
            id,
            format!(
                "権限確認待ち: ステップ='{}', ツール={}, モード={}",
                step_name, tools_text, mode_text
            ),
        );

        self.push_log_with_worker(
            Some(&worker_name),
            format!(
                "ステップ '{}' の権限確認を受信しました (ツール: {}, モード: {})",
                step_name, tools_text, mode_text
            ),
        );
    }

    fn handle_permission_resolved(
        &mut self,
        id: WorkerId,
        request_id: u64,
        decision: PermissionDecision,
    ) {
        if let Some(current) = self.permission_prompt.as_ref() {
            if current.request.request_id == request_id {
                self.permission_prompt = None;
            }
        }

        let tracker = self.permission_tracker.remove(&request_id);
        let worker_name = tracker
            .as_ref()
            .map(|entry| entry.worker_name.clone())
            .or_else(|| self.worker_name_by_id(id))
            .unwrap_or_else(|| format!("worker-{}", id.0));
        let step_name = tracker.as_ref().map(|entry| entry.step_name.clone());

        let action_text = match decision {
            PermissionDecision::Allow { .. } => "許可",
            PermissionDecision::Deny => "拒否",
        };

        let message = if let Some(step) = step_name.clone() {
            format!("ステップ '{}' の権限を{}しました", step, action_text)
        } else {
            format!("権限リクエスト (#{}) を{}しました", request_id, action_text)
        };

        self.add_worker_log(id, message.clone());
        self.push_log_with_worker(Some(&worker_name), message);
    }

    fn worker_name_by_id(&self, id: WorkerId) -> Option<String> {
        self.workers
            .iter()
            .find(|view| view.snapshot.id == id)
            .map(|view| view.snapshot.name.clone())
    }

    fn describe_allowed_tools(tools: &Option<Vec<String>>) -> String {
        match tools {
            None => "制限なし".to_string(),
            Some(list) if list.is_empty() => "なし".to_string(),
            Some(list) => list.join(", "),
        }
    }

    fn select_next(&mut self) {
        let count = self.visible_indices().len();
        if count == 0 {
            self.selected = 0;
            return;
        }
        self.selected = (self.selected + 1).min(count.saturating_sub(1));
        // Reset log view state when switching workers
        self.selected_step = 0;
        self.log_scroll = 0;
    }

    fn select_previous(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
            // Reset log view state when switching workers
            self.selected_step = 0;
            self.log_scroll = 0;
        }
    }

    fn select_first(&mut self) {
        self.selected = 0;
        // Reset log view state when switching workers
        self.selected_step = 0;
        self.log_scroll = 0;
    }

    fn select_last(&mut self) {
        let count = self.visible_indices().len();
        if count == 0 {
            self.selected = 0;
        } else {
            self.selected = count - 1;
        }
        // Reset log view state when switching workers
        self.selected_step = 0;
        self.log_scroll = 0;
    }

    fn on_tick(&mut self) {
        self.poll_events();
        self.clamp_selection();
        self.animation_frame = self.animation_frame.wrapping_add(1);
    }

    fn poll_events(&mut self) {
        while let Ok(event) = self.event_rx.try_recv() {
            match event {
                WorkerEvent::Created(snapshot) => {
                    self.add_or_update_worker(snapshot.clone());
                    self.push_log_with_worker(
                        Some(&snapshot.name),
                        format!(
                            "{} をプロビジョニングしました (workflow: {}, steps: {})",
                            snapshot.name, snapshot.workflow, snapshot.total_steps
                        ),
                    );
                }
                WorkerEvent::Updated(snapshot) => {
                    self.add_or_update_worker(snapshot.clone());
                    self.push_log_with_worker(
                        Some(&snapshot.name),
                        format!("{}: {}", snapshot.name, snapshot.last_event),
                    );
                }
                WorkerEvent::Log { id, line } => {
                    self.add_worker_log(id, line);
                    // Auto-scroll to bottom when new log is added
                    if self.auto_scroll_logs && self.show_logs {
                        self.log_scroll = self.get_log_max_scroll();
                    }
                }
                WorkerEvent::Deleted { id, message } => {
                    let worker_name = self
                        .workers
                        .iter()
                        .find(|view| view.snapshot.id == id)
                        .map(|view| view.snapshot.name.clone());
                    self.remove_worker(id);
                    if let Some(name) = worker_name {
                        self.push_log_with_worker(Some(&name), message);
                    } else {
                        self.push_log(message);
                    }
                }
                WorkerEvent::Error { id, message } => {
                    if let Some(worker_id) = id {
                        self.add_worker_log(worker_id, format!("エラー: {message}"));
                        if let Some(name) = self
                            .workers
                            .iter()
                            .find(|view| view.snapshot.id == worker_id)
                            .map(|view| view.snapshot.name.clone())
                        {
                            self.push_log_with_worker(Some(&name), format!("エラー: {message}"));
                            continue;
                        }
                    }
                    self.push_log(format!("エラー: {message}"));
                }
                WorkerEvent::PermissionRequested { id, request } => {
                    self.handle_permission_requested(id, request);
                }
                WorkerEvent::PermissionResolved {
                    id,
                    request_id,
                    decision,
                } => {
                    self.handle_permission_resolved(id, request_id, decision);
                }
            }
        }
    }

    fn render(&self, frame: &mut ratatui::Frame<'_>) {
        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Min(8),
                Constraint::Length(5),
            ])
            .split(frame.area());

        self.render_header(frame, layout[0]);
        self.render_table(frame, layout[1]);
        self.render_footer(frame, layout[2]);

        if self.show_logs {
            self.render_log_modal(frame);
        }

        if self.show_help {
            self.render_modal(frame, 60, 50, "Help", self.help_lines());
        }

        if let Some(prompt) = &self.permission_prompt {
            self.render_permission_modal(frame, prompt);
        }

        if let Some(input_mode) = &self.input_mode {
            match input_mode {
                InputMode::FreePrompt {
                    buffer,
                    permission_mode,
                    ..
                } => {
                    self.render_prompt_modal(frame, buffer, permission_mode);
                }
                InputMode::CreateWorkerSelection { selected } => {
                    self.render_create_selection_modal(frame, *selected);
                }
                InputMode::WorktreeSelection {
                    worktrees,
                    selected,
                } => {
                    self.render_worktree_selection_modal(frame, worktrees, *selected);
                }
                InputMode::ToolSelection {
                    tools,
                    selected_idx,
                    permission_mode,
                    ..
                } => {
                    self.render_tool_selection_modal(frame, tools, *selected_idx, permission_mode);
                }
            }
        }
    }

    fn render_header(&self, frame: &mut ratatui::Frame<'_>, area: Rect) {
        let total = self.workers.len();
        let filter_label = self
            .status_filter
            .map(|status| status.label().to_string())
            .unwrap_or_else(|| "All".into());

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
                total,
                filter_label,
                self.current_workflow_name()
            )),
        ]);

        let header =
            Paragraph::new(line).block(Block::default().borders(Borders::ALL).title("Overview"));
        frame.render_widget(header, area);
    }

    fn render_table(&self, frame: &mut ratatui::Frame<'_>, area: Rect) {
        let visible = self.visible_indices();
        let rows = visible.iter().enumerate().map(|(table_idx, worker_idx)| {
            let worker = &self.workers[*worker_idx];

            // For Running status, don't apply row-level color so cell colors show through
            let mut style = if worker.snapshot.status == WorkerStatus::Running {
                Style::default()
            } else {
                Style::default().fg(status_color(worker.snapshot.status))
            };

            if table_idx == self.selected {
                // For Running workers, only set background (not foreground) to preserve rainbow colors
                if worker.snapshot.status == WorkerStatus::Running {
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
                if worker.snapshot.status == WorkerStatus::Running {
                    let spinner_idx = self.animation_frame % SPINNER_CHARS.len();
                    let spinner = SPINNER_CHARS[spinner_idx];

                    // Faster animation for smooth flow
                    let slow_frame = self.animation_frame / 3;

                    let is_selected = table_idx == self.selected;

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

                    let status_text = format!("{} {}", spinner, worker.snapshot.status.label());
                    let sparkles = &["✨", "💫", "⭐", "🌟"];
                    let sparkle_idx = (self.animation_frame / 10) % sparkles.len();
                    let sparkle = sparkles[sparkle_idx];

                    // For last_event, add sparkle as separate span to avoid emoji breakage
                    let sparkle_style = if is_selected {
                        Style::default().bg(Color::DarkGray)
                    } else {
                        Style::default()
                    };
                    let mut last_event_spans =
                        vec![Span::styled(format!("{} ", sparkle), sparkle_style)];
                    last_event_spans.extend(create_rainbow_line(&worker.snapshot.last_event));

                    (
                        Cell::from(Line::from(create_rainbow_line(&worker.snapshot.name))),
                        Cell::from(Line::from(create_rainbow_line(&status_text))),
                        Cell::from(Line::from(last_event_spans)),
                    )
                } else {
                    (
                        Cell::from(worker.snapshot.name.clone()),
                        Cell::from(worker.snapshot.status.label()),
                        Cell::from(worker.snapshot.last_event.clone()),
                    )
                };

            // For Running workers that are selected, apply background to all cells
            let other_cell_style =
                if worker.snapshot.status == WorkerStatus::Running && table_idx == self.selected {
                    Style::default().bg(Color::DarkGray)
                } else {
                    Style::default()
                };

            let row = Row::new(vec![
                name_cell,
                Cell::from(
                    worker
                        .snapshot
                        .issue
                        .clone()
                        .unwrap_or_else(|| "Unassigned".into()),
                )
                .style(other_cell_style),
                Cell::from(worker.snapshot.workflow.clone()).style(other_cell_style),
                Cell::from(worker.snapshot.current_step.clone().unwrap_or_else(|| {
                    if worker.snapshot.total_steps > 0 {
                        format!("0/{} steps", worker.snapshot.total_steps)
                    } else {
                        "-".into()
                    }
                }))
                .style(other_cell_style),
                Cell::from(worker.snapshot.agent.clone()).style(other_cell_style),
                Cell::from(worker.snapshot.worktree.clone()).style(other_cell_style),
                Cell::from(worker.snapshot.branch.clone()).style(other_cell_style),
                status_cell,
                last_event_cell,
            ]);

            // Only apply row style for non-Running status (to preserve rainbow colors)
            if worker.snapshot.status == WorkerStatus::Running {
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
            Constraint::Length(12),
            Constraint::Length(10),
            Constraint::Length(14),
            Constraint::Length(18),
            Constraint::Length(20),
            Constraint::Length(24),
            Constraint::Length(20),
            Constraint::Length(10),
            Constraint::Min(24),
        ];

        let table = Table::new(rows, widths)
            .header(header)
            .block(Block::default().borders(Borders::ALL).title("Workers"))
            .column_spacing(1)
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED));

        frame.render_widget(table, area);
    }

    fn render_footer(&self, frame: &mut ratatui::Frame<'_>, area: Rect) {
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
                Span::styled(
                    self.current_workflow_name(),
                    Style::default().fg(Color::Magenta),
                ),
            ]),
        ];

        let footer =
            Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title("Controls"));
        frame.render_widget(footer, area);
    }

    fn render_modal(
        &self,
        frame: &mut ratatui::Frame<'_>,
        percent_x: u16,
        percent_y: u16,
        title: &str,
        lines: Vec<Line>,
    ) {
        let area = centered_rect(percent_x, percent_y, frame.area());
        let widget = Paragraph::new(lines)
            .wrap(Wrap { trim: true })
            .block(Block::default().borders(Borders::ALL).title(title));
        frame.render_widget(Clear, area);
        frame.render_widget(widget, area);
    }

    fn render_prompt_modal(
        &self,
        frame: &mut ratatui::Frame<'_>,
        buffer: &str,
        permission_mode: &Option<String>,
    ) {
        let area = centered_rect(70, 30, frame.area());
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

    fn render_permission_modal(
        &self,
        frame: &mut ratatui::Frame<'_>,
        prompt: &PermissionPromptState,
    ) {
        let area = centered_rect(70, 45, frame.area());
        let mode_label = permission_mode_label(&prompt.request.permission_mode).to_string();
        let tools_text = Self::describe_allowed_tools(&prompt.request.allowed_tools);
        let description = prompt
            .request
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
            Span::styled(&prompt.worker_name, Style::default().fg(Color::Cyan)),
        ]));
        lines.push(Line::from(vec![
            Span::raw("ステップ: "),
            Span::styled(&prompt.request.step_name, Style::default().fg(Color::Green)),
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
            let is_selected = decision == &prompt.selection;
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

    fn render_create_selection_modal(&self, frame: &mut ratatui::Frame<'_>, selected: usize) {
        let area = centered_rect(60, 40, frame.area());

        let workflow_name = self.current_workflow_name();
        let options = vec![
            format!("  ワークフローを実行 ({})", workflow_name),
            "  自由入力でワーカーを作成".to_string(),
            "  既存worktreeを使用".to_string(),
        ];

        let lines: Vec<Line> = vec![
            Line::raw("ワーカーの作成方法を選択してください"),
            Line::raw(""),
        ]
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

    fn render_tool_selection_modal(
        &self,
        frame: &mut ratatui::Frame<'_>,
        tools: &HashMap<String, bool>,
        selected_idx: usize,
        permission_mode: &str,
    ) {
        let area = centered_rect(70, 60, frame.area());

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

    fn render_worktree_selection_modal(
        &self,
        frame: &mut ratatui::Frame<'_>,
        worktrees: &[ExistingWorktree],
        selected: usize,
    ) {
        let area = centered_rect(70, 50, frame.area());

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

    fn help_lines(&self) -> Vec<Line<'static>> {
        vec![
            Line::raw("MVP ショートカット"),
            Line::raw(""),
            Line::raw("c – ワーカーを作成（ワークフロー or 自由入力を選択）"),
            Line::raw("d – ワーカー停止と worktree 削除（アーカイブは状態削除のみ）"),
            Line::raw("r – ワーカーを再起動（アーカイブは不可）"),
            Line::raw("i – 自由指示を送信（ワーカー選択時は追加指示、アーカイブは不可）"),
            Line::raw("a – ステータスフィルタを切り替え"),
            Line::raw("w – 使用するワークフローを切り替え"),
            Line::raw("j/k または ↑/↓ – 選択移動 (ログ表示時はスクロール)"),
            Line::raw("PgUp/PgDn – ログを10行スクロール"),
            Line::raw("Home/End – ログの先頭/末尾へジャンプ"),
            Line::raw("l – 選択ワーカーのログを表示"),
            Line::raw("h – このヘルプを表示"),
            Line::raw("Shift+C – アクションログを圧縮"),
            Line::raw("Shift+I – インタラクティブClaude Code起動（権限を手動承認可能）"),
            Line::raw("Shift+A – ログの自動スクロールON/OFF切替"),
            Line::raw("q – 終了"),
            Line::raw(""),
            Line::raw("ステータス: Running/Idle/Paused/Failed/Archived(青=履歴)"),
        ]
    }

    fn log_modal_data(&self) -> (String, Vec<Line<'static>>) {
        let (all_lines, base_title): (Vec<String>, &str) =
            if let Some(view) = self.selected_worker_view() {
                if view.logs.is_empty() {
                    (
                        vec!["このワーカーのログはまだありません。".to_string()],
                        "Worker Logs",
                    )
                } else {
                    (view.logs.iter().cloned().collect(), "Worker Logs")
                }
            } else if self.log_messages.is_empty() {
                (
                    vec!["アクションログはまだありません。".to_string()],
                    "Action Logs",
                )
            } else {
                (self.log_messages.iter().cloned().collect(), "Action Logs")
            };

        let total_lines = all_lines.len();
        let visible_start = self.log_scroll.min(total_lines.saturating_sub(1));

        // Show lines from scroll position onwards
        let visible_lines: Vec<Line<'static>> = all_lines
            .iter()
            .skip(visible_start)
            .map(|s| Line::from(s.clone()))
            .collect();

        let auto_scroll_status = if self.auto_scroll_logs {
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

        (title, visible_lines)
    }

    fn render_log_modal(&self, frame: &mut ratatui::Frame<'_>) {
        match self.log_view_mode {
            LogViewMode::Overview => self.render_overview_tab(frame),
            LogViewMode::Detail => self.render_detail_tab(frame),
            LogViewMode::Raw => {
                let (title, lines) = self.log_modal_data();
                self.render_modal(frame, 70, 45, &title, lines);
            }
        }
    }

    fn render_overview_tab(&self, frame: &mut ratatui::Frame<'_>) {
        let area = centered_rect(80, 60, frame.area());

        if let Some(view) = self.selected_worker_view() {
            let entries = &view.structured_logs;

            if entries.is_empty() {
                let lines = vec![Line::raw("このワーカーにはまだステップログがありません。")];
                let auto_scroll_status = if self.auto_scroll_logs {
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
            let safe_selected_step = self.selected_step.min(entries.len().saturating_sub(1));

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
                Constraint::Length(4),
                Constraint::Length(20),
                Constraint::Length(12),
                Constraint::Min(30),
            ];

            let auto_scroll_status = if self.auto_scroll_logs {
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
        } else {
            // Show action logs (no structured logs available)
            let (title, lines) = self.log_modal_data();
            let widget =
                Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(title));
            frame.render_widget(Clear, area);
            frame.render_widget(widget, area);
        }
    }

    fn render_detail_tab(&self, frame: &mut ratatui::Frame<'_>) {
        let area = centered_rect(80, 60, frame.area());

        if let Some(view) = self.selected_worker_view() {
            if let Some(entry) = view.structured_logs.get(self.selected_step) {
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

                let visible_lines: Vec<Line> =
                    lines.iter().skip(self.log_scroll).cloned().collect();

                let auto_scroll_status = if self.auto_scroll_logs {
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
            } else {
                let lines = vec![Line::raw("選択されたステップが見つかりません。")];
                let widget = Paragraph::new(lines)
                    .block(Block::default().borders(Borders::ALL).title("Detail"));
                frame.render_widget(Clear, area);
                frame.render_widget(widget, area);
            }
        } else {
            let lines = vec![Line::raw("ワーカーが選択されていません。")];
            let widget =
                Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title("Detail"));
            frame.render_widget(Clear, area);
            frame.render_widget(widget, area);
        }
    }

    fn add_or_update_worker(&mut self, snapshot: WorkerSnapshot) {
        if let Some(pos) = self
            .workers
            .iter()
            .position(|view| view.snapshot.id == snapshot.id)
        {
            let view = &mut self.workers[pos];
            view.update_snapshot(snapshot);
        } else {
            let view = WorkerView::new(snapshot);
            self.workers.push(view);
        }
        self.clamp_selection();
    }

    fn remove_worker(&mut self, id: WorkerId) {
        if let Some(pos) = self.workers.iter().position(|view| view.snapshot.id == id) {
            let view = self.workers.remove(pos);
            if let Err(err) = self.state_store.delete_worker(&view.snapshot.name) {
                eprintln!(
                    "Failed to delete worker state {}: {err}",
                    view.snapshot.name
                );
            }
        }
        self.clamp_selection();
    }

    fn add_worker_log(&mut self, id: WorkerId, line: String) {
        if let Some(pos) = self.workers.iter().position(|view| view.snapshot.id == id) {
            let view = &mut self.workers[pos];
            view.push_log(line);
        }
    }

    fn selected_worker_id(&self) -> Option<WorkerId> {
        let indices = self.visible_indices();
        indices
            .get(self.selected)
            .and_then(|idx| self.workers.get(*idx))
            .map(|view| view.snapshot.id)
    }

    fn selected_worker_view(&self) -> Option<&WorkerView> {
        let indices = self.visible_indices();
        indices
            .get(self.selected)
            .and_then(|idx| self.workers.get(*idx))
    }

    fn visible_indices(&self) -> Vec<usize> {
        self.workers
            .iter()
            .enumerate()
            .filter(|(_, view)| match self.status_filter {
                None => true,
                Some(status) => view.snapshot.status == status,
            })
            .map(|(idx, _)| idx)
            .collect()
    }

    fn clamp_selection(&mut self) {
        let count = self.visible_indices().len();
        let old_selected = self.selected;
        if count == 0 {
            self.selected = 0;
        } else if self.selected >= count {
            self.selected = count - 1;
        }
        // Reset log view state if worker selection changed
        if old_selected != self.selected {
            self.selected_step = 0;
            self.log_scroll = 0;
        }
    }

    fn push_log(&mut self, message: String) {
        self.push_log_with_worker(None, message);
    }

    fn push_log_with_worker(&mut self, worker: Option<&str>, message: String) {
        if self.log_messages.len() >= GLOBAL_LOG_CAPACITY {
            self.log_messages.pop_front();
        }
        let entry = ActionLogEntry {
            timestamp: OffsetDateTime::now_utc()
                .format(&Rfc3339)
                .unwrap_or_else(|_| "unknown".to_string()),
            message,
            worker: worker.map(|w| w.to_string()),
        };
        self.log_messages.push_back(format_action_log(&entry));
        if let Err(err) = self.state_store.append_action_log(&entry) {
            eprintln!("Failed to persist action log: {err}");
        }
    }

    fn current_workflow_name(&self) -> &str {
        self.workflows
            .get(self.selected_workflow_idx)
            .map(|wf| wf.name.as_str())
            .unwrap_or("n/a")
    }
}

struct WorkerView {
    snapshot: WorkerSnapshot,
    logs: VecDeque<String>,
    structured_logs: Vec<LogEntry>,
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

#[derive(Debug, Clone)]
struct LogEntry {
    step_index: usize,
    step_name: String,
    prompt_lines: Vec<String>,
    result_lines: Vec<String>,
    thought_lines: Vec<String>,
    status: StepStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StepStatus {
    Running,
    Success,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LogViewMode {
    Overview,
    Detail,
    Raw,
}

impl WorkerView {
    const LOG_CAPACITY: usize = 128;

    fn new(snapshot: WorkerSnapshot) -> Self {
        Self {
            snapshot,
            logs: VecDeque::with_capacity(Self::LOG_CAPACITY),
            structured_logs: Vec::new(),
            current_step_index: None,
            current_step_name: None,
            current_prompt: Vec::new(),
            current_result: Vec::new(),
            current_thought: Vec::new(),
            in_prompt: false,
            in_result: false,
            in_thought: false,
        }
    }

    fn update_snapshot(&mut self, snapshot: WorkerSnapshot) {
        self.snapshot = snapshot;
    }

    fn push_log(&mut self, line: String) {
        if self.logs.len() >= Self::LOG_CAPACITY {
            self.logs.pop_front();
        }
        self.logs.push_back(line.clone());

        // Parse structured log markers
        self.parse_log_line(&line);
    }

    fn parse_log_line(&mut self, line: &str) {
        if line.starts_with("[STEP_START:") {
            // Extract step index and name
            if let Some(content) = line
                .strip_prefix("[STEP_START:")
                .and_then(|s| s.strip_suffix("]"))
            {
                let parts: Vec<&str> = content.splitn(2, ':').collect();
                if parts.len() == 2 {
                    if let Ok(idx) = parts[0].parse::<usize>() {
                        self.current_step_index = Some(idx);
                        self.current_step_name = Some(parts[1].to_string());
                        self.current_prompt.clear();
                        self.current_result.clear();
                        self.current_thought.clear();
                        self.in_prompt = false;
                        self.in_result = false;
                        self.in_thought = false;
                    }
                }
            }
        } else if line == "─── Prompt ───" {
            // Start of prompt section (alternative to [PROMPT_START])
            self.in_prompt = true;
            self.in_result = false;
            self.in_thought = false;
        } else if line == "[PROMPT_START]" {
            self.in_prompt = true;
            self.in_result = false;
            self.in_thought = false;
        } else if line == "[PROMPT_END]" {
            self.in_prompt = false;
        } else if line == "─── Result ───" {
            // Start of result section (alternative to [RESULT_START])
            self.in_prompt = false;
            self.in_result = true;
            self.in_thought = false;
        } else if line.starts_with("───") && line.ends_with("───") {
            // Other section markers (Claude Code コマンド, stderr, etc.) end current sections
            self.in_prompt = false;
            self.in_result = false;
            self.in_thought = false;
        } else if line == "[RESULT_START]" {
            self.in_prompt = false;
            self.in_result = true;
            self.in_thought = false;
        } else if line == "[RESULT_END]" {
            self.in_result = false;
        } else if line == "[THOUGHT_START]" {
            self.in_prompt = false;
            self.in_result = false;
            self.in_thought = true;
            self.current_thought.clear();
        } else if line == "[THOUGHT_END]" {
            self.in_thought = false;
        } else if line.starts_with("[STEP_END:") {
            // Finalize current step
            if let Some(content) = line
                .strip_prefix("[STEP_END:")
                .and_then(|s| s.strip_suffix("]"))
            {
                let status = match content {
                    "Success" => StepStatus::Success,
                    "Failed" => StepStatus::Failed,
                    _ => StepStatus::Running,
                };

                if let (Some(idx), Some(name)) = (self.current_step_index, &self.current_step_name)
                {
                    let entry = LogEntry {
                        step_index: idx,
                        step_name: name.clone(),
                        prompt_lines: self.current_prompt.clone(),
                        result_lines: self.current_result.clone(),
                        thought_lines: self.current_thought.clone(),
                        status,
                    };
                    self.structured_logs.push(entry);

                    // Reset state
                    self.current_step_index = None;
                    self.current_step_name = None;
                    self.current_prompt.clear();
                    self.current_result.clear();
                    self.current_thought.clear();
                    self.in_thought = false;
                }
            }
        } else if self.in_prompt && !line.starts_with("─") {
            // Collect prompt lines (skip separator lines)
            self.current_prompt.push(line.to_string());
        } else if self.in_result && !line.starts_with("─") {
            // Collect result lines (skip separator lines)
            self.current_result.push(line.to_string());
        } else if self.in_thought {
            self.current_thought.push(line.to_string());
        }
    }
}

enum InputMode {
    FreePrompt {
        buffer: String,
        force_new: bool,
        permission_mode: Option<String>,
    },
    CreateWorkerSelection {
        selected: usize,
    },
    WorktreeSelection {
        worktrees: Vec<ExistingWorktree>,
        selected: usize,
    },
    ToolSelection {
        tools: HashMap<String, bool>,  // tool name -> checked state
        selected_idx: usize,            // cursor position (0..tools.len() + 1, last item is permission_mode)
        permission_mode: String,        // "acceptEdits" or "bypassPermissions"
        worker_id: WorkerId,            // worker requesting permission
        request_id: u64,                // permission request ID
    },
}

struct PermissionPromptState {
    worker_id: WorkerId,
    worker_name: String,
    request: PermissionRequest,
    selection: PermissionDecision,
}

impl PermissionPromptState {
    fn toggle(&mut self) {
        self.selection = match &self.selection {
            PermissionDecision::Allow { .. } => PermissionDecision::Deny,
            PermissionDecision::Deny => PermissionDecision::Allow {
                permission_mode: None,
                allowed_tools: None,
            },
        };
    }
}

struct PermissionTrackerEntry {
    worker_name: String,
    step_name: String,
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
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

fn format_action_log(entry: &ActionLogEntry) -> String {
    match &entry.worker {
        Some(worker) => format!("[{}][{}] {}", entry.timestamp, worker, entry.message),
        None => format!("[{}] {}", entry.timestamp, entry.message),
    }
}

fn status_color(status: WorkerStatus) -> Color {
    match status {
        WorkerStatus::Running => Color::Green,
        WorkerStatus::Paused => Color::Yellow,
        WorkerStatus::Failed => Color::Red,
        WorkerStatus::Idle => Color::Gray,
        WorkerStatus::Archived => Color::Blue,
    }
}

fn permission_mode_label(permission_mode: &Option<String>) -> &str {
    match permission_mode.as_deref() {
        None => "制限なしモード",
        Some("plan") => "プランモード",
        Some("acceptEdits") => "編集承認モード",
        Some("bypassPermissions") => "制限なしモード",
        Some(other) => other,
    }
}
