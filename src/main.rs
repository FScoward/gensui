mod config;
mod log_parser;
mod state;
mod ui;
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
use ratatui::text::Line;

use crate::config::{Config, Workflow};
use crate::state::{ActionLogEntry, StateStore};
use crate::ui::{
    centered_rect, describe_allowed_tools, format_action_log, help_lines, permission_mode_label,
    prepare_raw_log_data, render_create_selection_modal, render_detail_tab,
    render_footer, render_header, render_log_modal, render_modal,
    render_name_input_modal, render_overview_tab, render_permission_modal,
    render_prompt_modal, render_rename_worker_modal, render_table,
    render_tool_selection_modal, render_worktree_selection_modal,
    LogEntry, LogViewMode, AVAILABLE_TOOLS,
};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use worker::{
    CreateWorkerRequest, ExistingWorktree, PermissionDecision, PermissionRequest, WorkerEvent,
    WorkerEventReceiver, WorkerHandle, WorkerId, WorkerSnapshot, WorkerStatus,
    list_existing_worktrees, spawn_worker_system,
};

const GLOBAL_LOG_CAPACITY: usize = 64;

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
        if let Some(request) = app.ui_state.pending_interactive_mode.take() {
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

struct UIState {
    show_help: bool,
    show_logs: bool,
    log_view_mode: LogViewMode,
    selected_step: usize,
    selected: usize,
    animation_frame: usize,
    input_mode: Option<InputMode>,
    pending_interactive_mode: Option<InteractiveRequest>,
    status_filter: Option<WorkerStatus>,
}

impl UIState {
    fn new() -> Self {
        Self {
            show_help: false,
            show_logs: false,
            log_view_mode: LogViewMode::Overview,
            selected_step: 0,
            selected: 0,
            animation_frame: 0,
            input_mode: None,
            pending_interactive_mode: None,
            status_filter: None,
        }
    }
}

struct LogManager {
    log_messages: VecDeque<String>,
    log_scroll: usize,
    auto_scroll_logs: bool,
}

impl LogManager {
    fn new(capacity: usize) -> Self {
        Self {
            log_messages: VecDeque::with_capacity(capacity),
            log_scroll: 0,
            auto_scroll_logs: true,
        }
    }

    fn push(&mut self, message: String, capacity: usize) {
        if self.log_messages.len() >= capacity {
            self.log_messages.pop_front();
        }
        self.log_messages.push_back(message);
    }

    fn compact(&mut self, min_size: usize) {
        while self.log_messages.len() > min_size {
            self.log_messages.pop_front();
        }
    }
}

struct WorkerManager {
    workers: Vec<WorkerView>,
    permission_prompt: Option<PermissionPromptState>,
    permission_tracker: HashMap<u64, PermissionTrackerEntry>,
}

impl WorkerManager {
    fn new() -> Self {
        Self {
            workers: Vec::new(),
            permission_prompt: None,
            permission_tracker: HashMap::new(),
        }
    }

    fn add_or_update(&mut self, snapshot: WorkerSnapshot) {
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
    }

    fn remove(&mut self, id: WorkerId) -> Option<WorkerView> {
        if let Some(pos) = self.workers.iter().position(|view| view.snapshot.id == id) {
            Some(self.workers.remove(pos))
        } else {
            None
        }
    }

    fn add_log(&mut self, id: WorkerId, line: String) {
        if let Some(pos) = self.workers.iter().position(|view| view.snapshot.id == id) {
            let view = &mut self.workers[pos];
            view.push_log(line);
        }
    }

    fn worker_name_by_id(&self, id: WorkerId) -> Option<String> {
        self.workers
            .iter()
            .find(|view| view.snapshot.id == id)
            .map(|view| view.snapshot.name.clone())
    }
}

struct App {
    repo_root: std::path::PathBuf,
    manager: WorkerHandle,
    event_rx: WorkerEventReceiver,
    state_store: StateStore,
    workflows: Vec<Workflow>,
    selected_workflow_idx: usize,
    ui_state: UIState,
    log_manager: LogManager,
    worker_manager: WorkerManager,
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
        let mut log_manager = LogManager::new(GLOBAL_LOG_CAPACITY);
        let history = state_store.load_action_log(GLOBAL_LOG_CAPACITY)?;
        for entry in history {
            log_manager.push(format_action_log(&entry), GLOBAL_LOG_CAPACITY);
        }

        let (manager, event_rx) = spawn_worker_system(repo_root.clone(), config)?;

        Ok(Self {
            repo_root,
            manager,
            event_rx,
            state_store,
            workflows,
            selected_workflow_idx,
            ui_state: UIState::new(),
            log_manager,
            worker_manager: WorkerManager::new(),
        })
    }

    fn handle_key(&mut self, key_event: KeyEvent) -> bool {
        if self.worker_manager.permission_prompt.is_some() {
            return self.handle_permission_prompt_key(key_event);
        }

        if self.ui_state.input_mode.is_some() {
            return self.handle_input_mode_key(key_event);
        }

        self.handle_normal_key(key_event)
    }

    fn handle_permission_prompt_key(&mut self, key_event: KeyEvent) -> bool {
        let key_code = key_event.code;
        let mut should_submit = None;

        if let Some(prompt) = self.worker_manager.permission_prompt.as_mut() {
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
            match decision {
                PermissionDecision::Allow { .. } => {
                    let mut tools = HashMap::new();
                    for tool_def in AVAILABLE_TOOLS {
                        tools.insert(tool_def.name.to_string(), false);
                    }

                    if let Some(prompt_state) = &self.worker_manager.permission_prompt {
                        let worker_id = prompt_state.worker_id;
                        let request_id = prompt_state.request.request_id;
                        self.worker_manager.permission_prompt = None;

                        self.ui_state.input_mode = Some(InputMode::ToolSelection {
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

        false
    }

    fn handle_input_mode_key(&mut self, key_event: KeyEvent) -> bool {
        let Some(mode) = self.ui_state.input_mode.as_mut() else {
            return false;
        };

        match mode {
            InputMode::FreePrompt { .. } => self.handle_free_prompt_key(key_event),
            InputMode::CreateWorkerSelection { .. } => self.handle_create_selection_key(key_event),
            InputMode::WorktreeSelection { .. } => self.handle_worktree_selection_key(key_event),
            InputMode::ToolSelection { .. } => self.handle_tool_selection_key(key_event),
            InputMode::NameInput { .. } => self.handle_name_input_key(key_event),
            InputMode::RenameWorker { .. } => self.handle_rename_worker_key(key_event),
        }
    }

    fn handle_free_prompt_key(&mut self, key_event: KeyEvent) -> bool {
        let InputMode::FreePrompt {
            buffer,
            force_new,
            permission_mode,
            worker_name,
        } = self.ui_state.input_mode.as_mut().unwrap() else {
            return false;
        };

        match key_event.code {
            KeyCode::Esc => {
                self.ui_state.input_mode = None;
            }
            KeyCode::Enter => {
                let prompt = buffer.trim().to_string();
                let is_force_new = *force_new;
                let mode = permission_mode.clone();
                let name = worker_name.clone();
                self.ui_state.input_mode = None;
                if !prompt.is_empty() {
                    self.submit_free_prompt(prompt, is_force_new, mode, name);
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
        }
        false
    }

    fn handle_create_selection_key(&mut self, key_event: KeyEvent) -> bool {
        let InputMode::CreateWorkerSelection { selected } = self.ui_state.input_mode.as_mut().unwrap() else {
            return false;
        };

        match key_event.code {
            KeyCode::Esc => {
                self.ui_state.input_mode = None;
            }
            KeyCode::Enter => {
                let choice = *selected;
                self.ui_state.input_mode = None;
                if choice == 0 {
                    self.enqueue_create_worker();
                } else if choice == 1 {
                    self.show_name_input_for_free_prompt();
                } else {
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
        }
        false
    }

    fn handle_worktree_selection_key(&mut self, key_event: KeyEvent) -> bool {
        let InputMode::WorktreeSelection { worktrees, selected } = self.ui_state.input_mode.as_mut().unwrap() else {
            return false;
        };

        match key_event.code {
            KeyCode::Esc => {
                self.ui_state.input_mode = None;
            }
            KeyCode::Enter => {
                if let Some(worktree) = worktrees.get(*selected) {
                    let worktree_path = worktree.path.clone();
                    let branch = worktree.branch.clone();
                    self.ui_state.input_mode = None;
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
        }
        false
    }

    fn handle_tool_selection_key(&mut self, key_event: KeyEvent) -> bool {
        let InputMode::ToolSelection {
            tools,
            selected_idx,
            permission_mode,
            worker_id,
            request_id,
        } = self.ui_state.input_mode.as_mut().unwrap() else {
            return false;
        };

        match key_event.code {
            KeyCode::Esc => {
                if let Err(err) = self.manager.respond_permission(
                    *worker_id,
                    *request_id,
                    PermissionDecision::Deny,
                ) {
                    self.push_log(format!("権限応答の送信に失敗しました: {err}"));
                }
                self.ui_state.input_mode = None;
            }
            KeyCode::Enter => {
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
                self.ui_state.input_mode = None;

                if let Err(err) = self.manager.respond_permission(wid, rid, final_decision.clone()) {
                    self.push_log(format!("権限応答の送信に失敗しました: {err}"));
                } else {
                    let worker_name = self.worker_manager.worker_name_by_id(wid).unwrap_or_else(|| format!("worker-{}", wid.0));
                    self.worker_manager.permission_tracker.insert(
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
                let max_idx = tools.len();
                *selected_idx = (*selected_idx + 1).min(max_idx);
            }
            KeyCode::Char(' ') => {
                if *selected_idx < tools.len() {
                    let tool_name = AVAILABLE_TOOLS[*selected_idx].name;
                    if let Some(checked) = tools.get_mut(tool_name) {
                        *checked = !*checked;
                    }
                } else {
                    *permission_mode = match permission_mode.as_str() {
                        "acceptEdits" => "bypassPermissions".to_string(),
                        _ => "acceptEdits".to_string(),
                    };
                }
            }
            _ => {}
        }
        false
    }

    fn handle_name_input_key(&mut self, key_event: KeyEvent) -> bool {
        let InputMode::NameInput { buffer, workflow_name, next_action } = self.ui_state.input_mode.as_mut().unwrap() else {
            return false;
        };

        match key_event.code {
            KeyCode::Esc => {
                let workflow = workflow_name.clone();
                let action = next_action.clone();
                self.ui_state.input_mode = None;

                match action {
                    NameInputNextAction::CreateWithWorkflow => {
                        self.create_worker_with_default_name(workflow);
                    }
                    NameInputNextAction::CreateWithFreePrompt => {
                        self.ui_state.input_mode = Some(InputMode::FreePrompt {
                            buffer: String::new(),
                            force_new: true,
                            permission_mode: None,
                            worker_name: None,
                        });
                    }
                }
            }
            KeyCode::Enter => {
                let name = buffer.trim().to_string();
                let workflow = workflow_name.clone();
                let action = next_action.clone();
                self.ui_state.input_mode = None;

                match action {
                    NameInputNextAction::CreateWithWorkflow => {
                        if name.is_empty() {
                            self.create_worker_with_default_name(workflow);
                        } else {
                            self.create_worker_with_name(name, workflow);
                        }
                    }
                    NameInputNextAction::CreateWithFreePrompt => {
                        let worker_name = if name.is_empty() { None } else { Some(name) };
                        self.ui_state.input_mode = Some(InputMode::FreePrompt {
                            buffer: String::new(),
                            force_new: true,
                            permission_mode: None,
                            worker_name,
                        });
                    }
                }
            }
            KeyCode::Backspace => {
                buffer.pop();
            }
            KeyCode::Char(c) => {
                if !key_event.modifiers.contains(KeyModifiers::CONTROL)
                    && !key_event.modifiers.contains(KeyModifiers::ALT)
                {
                    buffer.push(c);
                }
            }
            _ => {}
        }
        false
    }

    fn handle_rename_worker_key(&mut self, key_event: KeyEvent) -> bool {
        let InputMode::RenameWorker { buffer, worker_id } = self.ui_state.input_mode.as_mut().unwrap() else {
            return false;
        };

        match key_event.code {
            KeyCode::Esc => {
                self.ui_state.input_mode = None;
            }
            KeyCode::Enter => {
                let new_name = buffer.trim().to_string();
                let wid = *worker_id;
                self.ui_state.input_mode = None;
                if !new_name.is_empty() {
                    self.rename_worker(wid, new_name);
                } else {
                    self.push_log("空の名前は無効です".into());
                }
            }
            KeyCode::Backspace => {
                buffer.pop();
            }
            KeyCode::Char(c) => {
                if !key_event.modifiers.contains(KeyModifiers::CONTROL)
                    && !key_event.modifiers.contains(KeyModifiers::ALT)
                {
                    buffer.push(c);
                }
            }
            _ => {}
        }
        false
    }

    fn handle_normal_key(&mut self, key_event: KeyEvent) -> bool {
        match key_event.code {
            KeyCode::Char('q') => return true,
            KeyCode::Char('c') => self.show_create_selection(),
            KeyCode::Char('d') => self.enqueue_delete_worker(),
            KeyCode::Char('r') => self.enqueue_restart_worker(),
            KeyCode::Char('n') => self.show_rename_modal(),
            KeyCode::Char('i') => self.start_free_prompt(),
            KeyCode::Char('h') => self.toggle_help(),
            KeyCode::Char('l') => self.toggle_logs(),
            KeyCode::Char('w') => self.cycle_workflow(),
            KeyCode::Char('a') => self.cycle_filter(),
            KeyCode::Tab => {
                if self.ui_state.show_logs {
                    self.switch_log_tab_next();
                }
            }
            KeyCode::BackTab => {
                if self.ui_state.show_logs {
                    self.switch_log_tab_prev();
                }
            }
            KeyCode::Enter => {
                if self.ui_state.show_logs && self.ui_state.log_view_mode == LogViewMode::Overview {
                    self.enter_detail_from_overview();
                }
            }
            KeyCode::Esc => {
                if self.ui_state.show_logs
                    && (self.ui_state.log_view_mode == LogViewMode::Detail
                        || self.ui_state.log_view_mode == LogViewMode::Raw)
                {
                    self.back_to_overview();
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if self.ui_state.show_logs {
                    match self.ui_state.log_view_mode {
                        LogViewMode::Overview => self.select_step_up(),
                        LogViewMode::Detail | LogViewMode::Raw => self.scroll_log_up(),
                    }
                } else {
                    self.select_previous();
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.ui_state.show_logs {
                    match self.ui_state.log_view_mode {
                        LogViewMode::Overview => self.select_step_down(),
                        LogViewMode::Detail | LogViewMode::Raw => self.scroll_log_down(),
                    }
                } else {
                    self.select_next();
                }
            }
            KeyCode::Home => {
                if self.ui_state.show_logs {
                    self.scroll_log_home();
                } else {
                    self.select_first();
                }
            }
            KeyCode::End => {
                if self.ui_state.show_logs {
                    self.scroll_log_end();
                } else {
                    self.select_last();
                }
            }
            KeyCode::PageUp => {
                if self.ui_state.show_logs {
                    self.scroll_log_page_up();
                }
            }
            KeyCode::PageDown => {
                if self.ui_state.show_logs {
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
        // Show name input modal
        let workflow_name = self
            .workflows
            .get(self.selected_workflow_idx)
            .map(|wf| wf.name.clone());

        self.ui_state.input_mode = Some(InputMode::NameInput {
            buffer: String::new(),
            workflow_name,
            next_action: NameInputNextAction::CreateWithWorkflow,
        });
    }

    fn show_name_input_for_free_prompt(&mut self) {
        self.ui_state.input_mode = Some(InputMode::NameInput {
            buffer: String::new(),
            workflow_name: None,
            next_action: NameInputNextAction::CreateWithFreePrompt,
        });
    }

    fn create_worker_with_default_name(&mut self, workflow_name: Option<String>) {
        let mut request = CreateWorkerRequest::default();
        request.workflow = workflow_name;
        request.name = None; // Use default name

        if let Err(err) = self.manager.create_worker(request) {
            self.push_log(format!("ワーカー作成に失敗しました: {err}"));
        } else {
            self.push_log("ワーカーを作成しました（デフォルト名）".into());
        }
    }

    fn create_worker_with_name(&mut self, name: String, workflow_name: Option<String>) {
        let mut request = CreateWorkerRequest::default();
        request.workflow = workflow_name;
        request.name = Some(name.clone());

        if let Err(err) = self.manager.create_worker(request) {
            self.push_log(format!("ワーカー作成に失敗しました: {err}"));
        } else {
            self.push_log(format!("ワーカーを作成しました: {}", name));
        }
    }

    fn show_rename_modal(&mut self) {
        if let Some(id) = self.selected_worker_id() {
            self.ui_state.input_mode = Some(InputMode::RenameWorker {
                buffer: String::new(),
                worker_id: id,
            });
        } else {
            self.push_log("ワーカーが選択されていません".into());
        }
    }

    fn rename_worker(&mut self, worker_id: WorkerId, new_name: String) {
        if let Err(err) = self.manager.rename_worker(worker_id, new_name.clone()) {
            self.push_log(format!("ワーカー名の変更に失敗しました: {err}"));
        } else {
            self.push_log(format!("ワーカー名を変更しました: {}", new_name));
        }
    }

    fn enqueue_delete_worker(&mut self) {
        if let Some(id) = self.selected_worker_id() {
            // Check if this is an archived worker
            if let Some(worker) = self.worker_manager.workers.iter().find(|w| w.snapshot.id == id) {
                if worker.snapshot.status == WorkerStatus::Archived {
                    // For archived workers, just delete the state file
                    if let Err(err) = self.state_store.delete_worker(&worker.snapshot.name) {
                        self.push_log(format!("アーカイブ削除に失敗しました: {err}"));
                    } else {
                        // Remove from UI
                        if let Some(pos) = self.worker_manager.workers.iter().position(|w| w.snapshot.id == id) {
                            let worker = self.worker_manager.workers.remove(pos);
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
            if let Some(worker) = self.worker_manager.workers.iter().find(|w| w.snapshot.id == id) {
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
        self.ui_state.show_help = !self.ui_state.show_help;
    }

    fn toggle_logs(&mut self) {
        self.ui_state.show_logs = !self.ui_state.show_logs;
        // Reset scroll and view mode when opening logs
        if self.ui_state.show_logs {
            self.log_manager.log_scroll = 0;
            self.ui_state.log_view_mode = LogViewMode::Overview;
            self.ui_state.selected_step = 0;
            // Clamp selected_step to valid range
            if let Some(view) = self.selected_worker_view() {
                if !view.structured_logs.is_empty() {
                    self.ui_state.selected_step = self.ui_state.selected_step.min(view.structured_logs.len() - 1);
                } else {
                    self.ui_state.selected_step = 0;
                }
            } else {
                self.ui_state.selected_step = 0;
            }
        }
    }

    fn scroll_log_up(&mut self) {
        self.log_manager.log_scroll = self.log_manager.log_scroll.saturating_sub(1);
        // Disable auto-scroll when manually scrolling up
        self.log_manager.auto_scroll_logs = false;
    }

    fn scroll_log_down(&mut self) {
        let max_scroll = self.get_log_max_scroll();
        if self.log_manager.log_scroll < max_scroll {
            self.log_manager.log_scroll += 1;
        }
        // Re-enable auto-scroll if we reached the bottom
        if self.log_manager.log_scroll >= max_scroll {
            self.log_manager.auto_scroll_logs = true;
        }
    }

    fn scroll_log_home(&mut self) {
        self.log_manager.log_scroll = 0;
        // Disable auto-scroll when jumping to top
        self.log_manager.auto_scroll_logs = false;
    }

    fn scroll_log_end(&mut self) {
        self.log_manager.log_scroll = self.get_log_max_scroll();
        // Re-enable auto-scroll when jumping to bottom
        self.log_manager.auto_scroll_logs = true;
    }

    fn scroll_log_page_up(&mut self) {
        self.log_manager.log_scroll = self.log_manager.log_scroll.saturating_sub(10);
        // Disable auto-scroll when paging up
        self.log_manager.auto_scroll_logs = false;
    }

    fn scroll_log_page_down(&mut self) {
        let max_scroll = self.get_log_max_scroll();
        let new_scroll = self.log_manager.log_scroll.saturating_add(10);
        self.log_manager.log_scroll = new_scroll.min(max_scroll);
        // Re-enable auto-scroll if we reached the bottom
        if self.log_manager.log_scroll >= max_scroll {
            self.log_manager.auto_scroll_logs = true;
        }
    }

    fn get_log_max_scroll(&self) -> usize {
        match self.ui_state.log_view_mode {
            LogViewMode::Raw => self.log_manager.log_messages.len().saturating_sub(1),
            _ => {
                if let Some(view) = self.selected_worker_view() {
                    view.logs.len().saturating_sub(1)
                } else {
                    self.log_manager.log_messages.len().saturating_sub(1)
                }
            }
        }
    }

    fn compact_logs(&mut self) {
        while self.log_manager.log_messages.len() > 4 {
            self.log_manager.log_messages.pop_front();
        }
        self.push_log("アクションログを圧縮しました".into());
    }

    fn toggle_auto_scroll(&mut self) {
        self.log_manager.auto_scroll_logs = !self.log_manager.auto_scroll_logs;
        let status = if self.log_manager.auto_scroll_logs {
            "ON"
        } else {
            "OFF"
        };
        self.push_log(format!("ログの自動スクロール: {}", status));
    }

    fn switch_log_tab_next(&mut self) {
        let next_mode = match self.ui_state.log_view_mode {
            LogViewMode::Overview => LogViewMode::Detail,
            LogViewMode::Detail => LogViewMode::Raw,
            LogViewMode::Raw => LogViewMode::Overview,
        };

        // Only switch to Detail if a valid step is available
        if matches!(next_mode, LogViewMode::Detail) {
            if let Some(view) = self.selected_worker_view() {
                if !view.structured_logs.is_empty() {
                    // Clamp selected_step to valid range
                    self.ui_state.selected_step = self.ui_state.selected_step.min(view.structured_logs.len() - 1);
                    self.ui_state.log_view_mode = next_mode;
                }
            }
        } else {
            self.ui_state.log_view_mode = next_mode;
        }
        self.log_manager.log_scroll = 0;
    }

    fn switch_log_tab_prev(&mut self) {
        let next_mode = match self.ui_state.log_view_mode {
            LogViewMode::Overview => LogViewMode::Raw,
            LogViewMode::Raw => LogViewMode::Detail,
            LogViewMode::Detail => LogViewMode::Overview,
        };

        // Only switch to Detail if a valid step is available
        if matches!(next_mode, LogViewMode::Detail) {
            if let Some(view) = self.selected_worker_view() {
                if !view.structured_logs.is_empty() {
                    // Clamp selected_step to valid range
                    self.ui_state.selected_step = self.ui_state.selected_step.min(view.structured_logs.len() - 1);
                    self.ui_state.log_view_mode = next_mode;
                }
            }
        } else {
            self.ui_state.log_view_mode = next_mode;
        }
        self.log_manager.log_scroll = 0;
    }

    fn select_step_up(&mut self) {
        self.ui_state.selected_step = self.ui_state.selected_step.saturating_sub(1);
    }

    fn select_step_down(&mut self) {
        if let Some(view) = self.selected_worker_view() {
            if !view.structured_logs.is_empty() {
                let max = view.structured_logs.len() - 1;
                self.ui_state.selected_step = (self.ui_state.selected_step + 1).min(max);
            }
        }
    }

    fn enter_detail_from_overview(&mut self) {
        // Only enter detail if a valid step is selected
        if let Some(view) = self.selected_worker_view() {
            if self.ui_state.selected_step < view.structured_logs.len() {
                self.ui_state.log_view_mode = LogViewMode::Detail;
                self.log_manager.log_scroll = 0;
            }
        }
    }

    fn back_to_overview(&mut self) {
        self.ui_state.log_view_mode = LogViewMode::Overview;
        self.log_manager.log_scroll = 0;
    }

    fn cycle_filter(&mut self) {
        self.ui_state.status_filter = match self.ui_state.status_filter {
            None => Some(WorkerStatus::Running),
            Some(WorkerStatus::Running) => Some(WorkerStatus::Paused),
            Some(WorkerStatus::Paused) => Some(WorkerStatus::Failed),
            Some(WorkerStatus::Failed) => Some(WorkerStatus::Idle),
            Some(WorkerStatus::Idle) => Some(WorkerStatus::Archived),
            Some(WorkerStatus::Archived) => None,
        };
        self.push_log("ステータスフィルタを更新しました".into());
        self.ui_state.selected = 0;
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
        self.ui_state.input_mode = Some(InputMode::CreateWorkerSelection { selected: 0 });
    }

    fn show_worktree_selection(&mut self) {
        match list_existing_worktrees(&self.repo_root) {
            Ok(worktrees) => {
                if worktrees.is_empty() {
                    self.push_log("既存のworktreeが見つかりませんでした".to_string());
                    return;
                }
                self.ui_state.input_mode = Some(InputMode::WorktreeSelection {
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
        self.ui_state.input_mode = Some(InputMode::FreePrompt {
            buffer: String::new(),
            force_new: false,
            permission_mode: None,
            worker_name: None,
        });
    }

    fn start_interactive_prompt(&mut self) {
        // Get selected worker
        if let Some(worker_id) = self.selected_worker_id() {
            if let Some(worker) = self.worker_manager.workers.iter().find(|w| w.snapshot.id == worker_id) {
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

                self.ui_state.pending_interactive_mode = Some(InteractiveRequest {
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
        worker_name: Option<String>,
    ) {
        let trimmed = prompt.trim();
        if trimmed.is_empty() {
            self.push_log("空の指示は送信されませんでした".into());
            return;
        }

        // Check if a worker is selected (only if not forcing new worker creation)
        if !force_new {
            let visible = self.visible_indices();
            if !visible.is_empty() && self.ui_state.selected < visible.len() {
                // Worker is selected - send continuation to existing worker
                let worker_index = visible[self.ui_state.selected];
                if let Some(worker) = self.worker_manager.workers.get(worker_index) {
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
        request.name = worker_name;

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
        if let Some(prompt) = self.worker_manager.permission_prompt.take() {
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
                self.worker_manager.permission_tracker.insert(
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
        let tools_text = describe_allowed_tools(&request.allowed_tools);
        let mode_text = permission_mode_label(&request.permission_mode).to_string();
        let step_name = request.step_name.clone();

        self.worker_manager.permission_prompt = Some(PermissionPromptState {
            worker_id: id,
            worker_name: worker_name.clone(),
            request,
            selection: PermissionDecision::Allow {
                permission_mode: None,
                allowed_tools: None,
            },
        });

        self.worker_manager.permission_tracker.insert(
            self.worker_manager.permission_prompt
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
        if let Some(current) = self.worker_manager.permission_prompt.as_ref() {
            if current.request.request_id == request_id {
                self.worker_manager.permission_prompt = None;
            }
        }

        let tracker = self.worker_manager.permission_tracker.remove(&request_id);
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


    fn select_next(&mut self) {
        let count = self.visible_indices().len();
        if count == 0 {
            self.ui_state.selected = 0;
            return;
        }
        self.ui_state.selected = (self.ui_state.selected + 1).min(count.saturating_sub(1));
        // Reset log view state when switching workers
        self.ui_state.selected_step = 0;
        self.log_manager.log_scroll = 0;
    }

    fn select_previous(&mut self) {
        if self.ui_state.selected > 0 {
            self.ui_state.selected -= 1;
            // Reset log view state when switching workers
            self.ui_state.selected_step = 0;
            self.log_manager.log_scroll = 0;
        }
    }

    fn select_first(&mut self) {
        self.ui_state.selected = 0;
        // Reset log view state when switching workers
        self.ui_state.selected_step = 0;
        self.log_manager.log_scroll = 0;
    }

    fn select_last(&mut self) {
        let count = self.visible_indices().len();
        if count == 0 {
            self.ui_state.selected = 0;
        } else {
            self.ui_state.selected = count - 1;
        }
        // Reset log view state when switching workers
        self.ui_state.selected_step = 0;
        self.log_manager.log_scroll = 0;
    }

    fn on_tick(&mut self) {
        self.poll_events();
        self.clamp_selection();
        self.ui_state.animation_frame = self.ui_state.animation_frame.wrapping_add(1);
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
                    if self.log_manager.auto_scroll_logs && self.ui_state.show_logs {
                        self.log_manager.log_scroll = self.get_log_max_scroll();
                    }
                }
                WorkerEvent::Deleted { id, message } => {
                    let worker_name = self
                        .worker_manager.workers
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
                WorkerEvent::Renamed { id: _, old_name, new_name } => {
                    self.push_log_with_worker(
                        Some(&new_name),
                        format!("Worker renamed: '{}' → '{}'", old_name, new_name),
                    );
                }
                WorkerEvent::Error { id, message } => {
                    if let Some(worker_id) = id {
                        self.add_worker_log(worker_id, format!("エラー: {message}"));
                        if let Some(name) = self
                            .worker_manager.workers
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

        if self.ui_state.show_logs {
            self.render_log_modal(frame);
        }

        if self.ui_state.show_help {
            self.render_modal(frame, 60, 50, "Help", self.help_lines());
        }

        if let Some(prompt) = &self.worker_manager.permission_prompt {
            self.render_permission_modal(frame, prompt);
        }

        if let Some(input_mode) = &self.ui_state.input_mode {
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
                InputMode::NameInput { buffer, workflow_name, .. } => {
                    self.render_name_input_modal(frame, buffer, workflow_name);
                }
                InputMode::RenameWorker { buffer, worker_id } => {
                    if let Some(worker) = self.worker_manager.workers.iter().find(|w| w.snapshot.id == *worker_id) {
                        self.render_rename_worker_modal(frame, buffer, &worker.snapshot.name);
                    }
                }
            }
        }
    }

    fn render_header(&self, frame: &mut ratatui::Frame<'_>, area: Rect) {
        let total = self.worker_manager.workers.len();
        let filter_label = self
            .ui_state.status_filter
            .map(|status| status.label().to_string())
            .unwrap_or_else(|| "All".into());

        render_header(
            frame,
            area,
            total,
            &filter_label,
            self.current_workflow_name(),
        );
    }

    fn render_table(&self, frame: &mut ratatui::Frame<'_>, area: Rect) {
        let visible = self.visible_indices();
        let workers_data: Vec<(usize, &WorkerSnapshot)> = visible
            .iter()
            .map(|&idx| (idx, &self.worker_manager.workers[idx].snapshot))
            .collect();

        render_table(
            frame,
            area,
            &workers_data,
            self.ui_state.selected,
            self.ui_state.animation_frame,
        );
    }

    fn render_footer(&self, frame: &mut ratatui::Frame<'_>, area: Rect) {
        render_footer(frame, area, self.current_workflow_name());
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
        render_modal(frame, area, title, lines);
    }

    fn render_prompt_modal(
        &self,
        frame: &mut ratatui::Frame<'_>,
        buffer: &str,
        permission_mode: &Option<String>,
    ) {
        let area = centered_rect(70, 30, frame.area());
        render_prompt_modal(frame, area, buffer, permission_mode);
    }

    fn render_permission_modal(
        &self,
        frame: &mut ratatui::Frame<'_>,
        prompt: &PermissionPromptState,
    ) {
        let area = centered_rect(70, 45, frame.area());
        render_permission_modal(
            frame,
            area,
            &prompt.worker_name,
            &prompt.request,
            &prompt.selection,
        );
    }

    fn render_create_selection_modal(&self, frame: &mut ratatui::Frame<'_>, selected: usize) {
        let area = centered_rect(60, 40, frame.area());
        let workflow_name = self.current_workflow_name();
        render_create_selection_modal(frame, area, selected, workflow_name);
    }

    fn render_tool_selection_modal(
        &self,
        frame: &mut ratatui::Frame<'_>,
        tools: &HashMap<String, bool>,
        selected_idx: usize,
        permission_mode: &str,
    ) {
        let area = centered_rect(70, 60, frame.area());
        render_tool_selection_modal(frame, area, tools, selected_idx, permission_mode);
    }

    fn render_worktree_selection_modal(
        &self,
        frame: &mut ratatui::Frame<'_>,
        worktrees: &[ExistingWorktree],
        selected: usize,
    ) {
        let area = centered_rect(70, 50, frame.area());
        render_worktree_selection_modal(frame, area, worktrees, selected);
    }

    fn render_name_input_modal(
        &self,
        frame: &mut ratatui::Frame<'_>,
        buffer: &str,
        workflow_name: &Option<String>,
    ) {
        let area = centered_rect(60, 40, frame.area());
        render_name_input_modal(frame, area, buffer, workflow_name);
    }

    fn render_rename_worker_modal(
        &self,
        frame: &mut ratatui::Frame<'_>,
        buffer: &str,
        current_name: &str,
    ) {
        let area = centered_rect(60, 40, frame.area());
        render_rename_worker_modal(frame, area, buffer, current_name);
    }

    fn help_lines(&self) -> Vec<Line<'static>> {
        help_lines()
    }

    fn log_modal_data(&self) -> (String, Vec<Line<'static>>) {
        let worker_logs = self.selected_worker_view().map(|view| &view.logs);
        let data = prepare_raw_log_data(
            worker_logs,
            &self.log_manager.log_messages,
            self.log_manager.log_scroll,
            self.log_manager.auto_scroll_logs,
        );
        (data.title, data.lines)
    }

    fn render_log_modal(&self, frame: &mut ratatui::Frame<'_>) {
        match self.ui_state.log_view_mode {
            LogViewMode::Overview => self.render_overview_tab(frame),
            LogViewMode::Detail => self.render_detail_tab(frame),
            LogViewMode::Raw => {
                let (title, lines) = self.log_modal_data();
                self.render_modal(frame, 70, 45, &title, lines);
            }
        }
    }

    fn render_overview_tab(&self, frame: &mut ratatui::Frame<'_>) {
        if let Some(view) = self.selected_worker_view() {
            render_overview_tab(
                frame,
                &view.structured_logs,
                self.ui_state.selected_step,
                self.log_manager.auto_scroll_logs,
            );
        } else {
            // Show action logs (no structured logs available)
            let area = centered_rect(80, 60, frame.area());
            let (title, lines) = self.log_modal_data();
            render_log_modal(frame, area, &title, lines);
        }
    }

    fn render_detail_tab(&self, frame: &mut ratatui::Frame<'_>) {
        if let Some(view) = self.selected_worker_view() {
            if let Some(entry) = view.structured_logs.get(self.ui_state.selected_step) {
                render_detail_tab(frame, entry, self.log_manager.log_scroll, self.log_manager.auto_scroll_logs);
            } else {
                let area = centered_rect(80, 60, frame.area());
                let lines = vec![Line::raw("選択されたステップが見つかりません。")];
                render_log_modal(frame, area, "Detail", lines);
            }
        } else {
            let area = centered_rect(80, 60, frame.area());
            let lines = vec![Line::raw("ワーカーが選択されていません。")];
            render_log_modal(frame, area, "Detail", lines);
        }
    }

    fn add_or_update_worker(&mut self, snapshot: WorkerSnapshot) {
        self.worker_manager.add_or_update(snapshot);
        self.clamp_selection();
    }

    fn remove_worker(&mut self, id: WorkerId) {
        if let Some(view) = self.worker_manager.remove(id) {
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
        self.worker_manager.add_log(id, line);
    }

    fn worker_name_by_id(&self, id: WorkerId) -> Option<String> {
        self.worker_manager.worker_name_by_id(id)
    }

    fn selected_worker_id(&self) -> Option<WorkerId> {
        let indices = self.visible_indices();
        indices
            .get(self.ui_state.selected)
            .and_then(|idx| self.worker_manager.workers.get(*idx))
            .map(|view| view.snapshot.id)
    }

    fn selected_worker_view(&self) -> Option<&WorkerView> {
        let indices = self.visible_indices();
        indices
            .get(self.ui_state.selected)
            .and_then(|idx| self.worker_manager.workers.get(*idx))
    }

    fn visible_indices(&self) -> Vec<usize> {
        self.worker_manager.workers
            .iter()
            .enumerate()
            .filter(|(_, view)| match self.ui_state.status_filter {
                None => true,
                Some(status) => view.snapshot.status == status,
            })
            .map(|(idx, _)| idx)
            .collect()
    }

    fn clamp_selection(&mut self) {
        let count = self.visible_indices().len();
        let old_selected = self.ui_state.selected;
        if count == 0 {
            self.ui_state.selected = 0;
        } else if self.ui_state.selected >= count {
            self.ui_state.selected = count - 1;
        }
        // Reset log view state if worker selection changed
        if old_selected != self.ui_state.selected {
            self.ui_state.selected_step = 0;
            self.log_manager.log_scroll = 0;
        }
    }

    fn push_log(&mut self, message: String) {
        self.push_log_with_worker(None, message);
    }

    fn push_log_with_worker(&mut self, worker: Option<&str>, message: String) {
        let entry = ActionLogEntry {
            timestamp: OffsetDateTime::now_utc()
                .format(&Rfc3339)
                .unwrap_or_else(|_| "unknown".to_string()),
            message,
            worker: worker.map(|w| w.to_string()),
        };
        self.log_manager.push(format_action_log(&entry), GLOBAL_LOG_CAPACITY);
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
    // Parser
    log_parser: log_parser::LogParser,
}


impl WorkerView {
    const LOG_CAPACITY: usize = 128;

    fn new(snapshot: WorkerSnapshot) -> Self {
        Self {
            snapshot,
            logs: VecDeque::with_capacity(Self::LOG_CAPACITY),
            structured_logs: Vec::new(),
            log_parser: log_parser::LogParser::new(),
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

        // Parse structured log markers using log_parser
        if let Some(entry) = self.log_parser.parse_line(&line) {
            self.structured_logs.push(entry);
        }
    }
}

enum InputMode {
    FreePrompt {
        buffer: String,
        force_new: bool,
        permission_mode: Option<String>,
        worker_name: Option<String>,
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
    NameInput {
        buffer: String,
        workflow_name: Option<String>,
        next_action: NameInputNextAction,
    },
    RenameWorker {
        buffer: String,
        worker_id: WorkerId,
    },
}

#[derive(Clone)]
enum NameInputNextAction {
    CreateWithWorkflow,
    CreateWithFreePrompt,
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

