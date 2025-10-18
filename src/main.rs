mod config;
mod state;
mod worker;

use std::collections::VecDeque;
use std::io::{self, Stdout};
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
    CreateWorkerRequest, WorkerEvent, WorkerEventReceiver, WorkerHandle, WorkerId, WorkerSnapshot,
    WorkerStatus, spawn_worker_system,
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
    let tick_rate = Duration::from_millis(200);

    loop {
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

        let (manager, event_rx) = spawn_worker_system(repo_root, config)?;

        Ok(Self {
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
        })
    }

    fn handle_key(&mut self, key_event: KeyEvent) -> bool {
        if let Some(mode) = self.input_mode.as_mut() {
            match mode {
                InputMode::FreePrompt { buffer, force_new } => match key_event.code {
                    KeyCode::Esc => {
                        self.input_mode = None;
                    }
                    KeyCode::Enter => {
                        let prompt = buffer.trim().to_string();
                        let is_force_new = *force_new;
                        self.input_mode = None;
                        if !prompt.is_empty() {
                            self.submit_free_prompt(prompt, is_force_new);
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
                        } else {
                            // Free input - always create new worker
                            self.input_mode = Some(InputMode::FreePrompt {
                                buffer: String::new(),
                                force_new: true,
                            });
                        }
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        *selected = selected.saturating_sub(1);
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        *selected = (*selected + 1).min(1);
                    }
                    _ => {}
                },
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
                if self.show_logs && (self.log_view_mode == LogViewMode::Detail || self.log_view_mode == LogViewMode::Raw) {
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
                            self.push_log(format!("アーカイブを削除しました: {}", worker.snapshot.name));
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
        }
    }

    fn scroll_log_up(&mut self) {
        self.log_scroll = self.log_scroll.saturating_sub(1);
    }

    fn scroll_log_down(&mut self) {
        if self.log_scroll < self.log_messages.len().saturating_sub(1) {
            self.log_scroll += 1;
        }
    }

    fn scroll_log_home(&mut self) {
        self.log_scroll = 0;
    }

    fn scroll_log_end(&mut self) {
        self.log_scroll = self.log_messages.len().saturating_sub(1);
    }

    fn scroll_log_page_up(&mut self) {
        self.log_scroll = self.log_scroll.saturating_sub(10);
    }

    fn scroll_log_page_down(&mut self) {
        let new_scroll = self.log_scroll.saturating_add(10);
        self.log_scroll = new_scroll.min(self.log_messages.len().saturating_sub(1));
    }

    fn compact_logs(&mut self) {
        while self.log_messages.len() > 4 {
            self.log_messages.pop_front();
        }
        self.push_log("アクションログを圧縮しました".into());
    }

    fn switch_log_tab_next(&mut self) {
        self.log_view_mode = match self.log_view_mode {
            LogViewMode::Overview => LogViewMode::Detail,
            LogViewMode::Detail => LogViewMode::Raw,
            LogViewMode::Raw => LogViewMode::Overview,
        };
        self.log_scroll = 0;
    }

    fn switch_log_tab_prev(&mut self) {
        self.log_view_mode = match self.log_view_mode {
            LogViewMode::Overview => LogViewMode::Raw,
            LogViewMode::Raw => LogViewMode::Detail,
            LogViewMode::Detail => LogViewMode::Overview,
        };
        self.log_scroll = 0;
    }

    fn select_step_up(&mut self) {
        self.selected_step = self.selected_step.saturating_sub(1);
    }

    fn select_step_down(&mut self) {
        if let Some(view) = self.selected_worker_view() {
            let max = view.structured_logs.len().saturating_sub(1);
            self.selected_step = (self.selected_step + 1).min(max);
        }
    }

    fn enter_detail_from_overview(&mut self) {
        self.log_view_mode = LogViewMode::Detail;
        self.log_scroll = 0;
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

    fn start_free_prompt(&mut self) {
        self.input_mode = Some(InputMode::FreePrompt {
            buffer: String::new(),
            force_new: false,
        });
    }

    fn submit_free_prompt(&mut self, prompt: String, force_new: bool) {
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
                        self.push_log("アーカイブされたワーカーには追加指示を送信できません".to_string());
                        return;
                    }

                    let worker_id = worker.snapshot.id;
                    match self.manager.continue_worker(worker_id, trimmed.to_string()) {
                        Ok(_) => {
                            self.push_log(format!("追加指示を送信しました (worker-{}): {}", worker_id.0, trimmed));
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

        match self.manager.create_worker(request) {
            Ok(_) => {
                self.push_log(format!("自由指示を送信しました: {}", trimmed));
            }
            Err(err) => {
                self.push_log(format!("自由指示の作成に失敗しました: {err}"));
            }
        }
    }

    fn select_next(&mut self) {
        let count = self.visible_indices().len();
        if count == 0 {
            self.selected = 0;
            return;
        }
        self.selected = (self.selected + 1).min(count.saturating_sub(1));
    }

    fn select_previous(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
        }
    }

    fn select_first(&mut self) {
        self.selected = 0;
    }

    fn select_last(&mut self) {
        let count = self.visible_indices().len();
        if count == 0 {
            self.selected = 0;
        } else {
            self.selected = count - 1;
        }
    }

    fn on_tick(&mut self) {
        self.poll_events();
        self.clamp_selection();
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

        if let Some(input_mode) = &self.input_mode {
            match input_mode {
                InputMode::FreePrompt { buffer, .. } => {
                    self.render_prompt_modal(frame, buffer);
                }
                InputMode::CreateWorkerSelection { selected } => {
                    self.render_create_selection_modal(frame, *selected);
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
            let mut style = Style::default().fg(status_color(worker.snapshot.status));
            if table_idx == self.selected {
                style = style.bg(Color::DarkGray).fg(Color::White);
            }

            Row::new(vec![
                Cell::from(worker.snapshot.name.clone()),
                Cell::from(
                    worker
                        .snapshot
                        .issue
                        .clone()
                        .unwrap_or_else(|| "Unassigned".into()),
                ),
                Cell::from(worker.snapshot.workflow.clone()),
                Cell::from(worker.snapshot.current_step.clone().unwrap_or_else(|| {
                    if worker.snapshot.total_steps > 0 {
                        format!("0/{} steps", worker.snapshot.total_steps)
                    } else {
                        "-".into()
                    }
                })),
                Cell::from(worker.snapshot.agent.clone()),
                Cell::from(worker.snapshot.worktree.clone()),
                Cell::from(worker.snapshot.branch.clone()),
                Cell::from(worker.snapshot.status.label()),
                Cell::from(worker.snapshot.last_event.clone()),
            ])
            .style(style)
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

    fn render_prompt_modal(&self, frame: &mut ratatui::Frame<'_>, buffer: &str) {
        let area = centered_rect(70, 30, frame.area());
        let lines = vec![
            Line::raw("自由指示を入力してください (Enterで送信 / Escでキャンセル)"),
            Line::raw(""),
            Line::from(Span::styled(
                buffer,
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::raw(""),
            Line::raw("Claude Codeがheadlessモードで実行されます"),
        ];
        let widget = Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(Block::default().borders(Borders::ALL).title("Free Prompt"));
        frame.render_widget(Clear, area);
        frame.render_widget(widget, area);
    }

    fn render_create_selection_modal(&self, frame: &mut ratatui::Frame<'_>, selected: usize) {
        let area = centered_rect(60, 40, frame.area());

        let workflow_name = self.current_workflow_name();
        let options = vec![
            format!("  ワークフローを実行 ({})", workflow_name),
            "  自由入力でワーカーを作成".to_string(),
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

        let widget = Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(Block::default().borders(Borders::ALL).title("Create Worker"));
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
            Line::raw("q – 終了"),
            Line::raw(""),
            Line::raw("ステータス: Running/Idle/Paused/Failed/Archived(青=履歴)"),
        ]
    }

    fn log_modal_data(&self) -> (String, Vec<Line<'static>>) {
        let (all_lines, base_title): (Vec<String>, &str) = if let Some(view) = self.selected_worker_view() {
            if view.logs.is_empty() {
                (vec!["このワーカーのログはまだありません。".to_string()], "Worker Logs")
            } else {
                (view.logs.iter().cloned().collect(), "Worker Logs")
            }
        } else if self.log_messages.is_empty() {
            (vec!["アクションログはまだありません。".to_string()], "Action Logs")
        } else {
            (self.log_messages.iter().cloned().collect(), "Action Logs")
        };

        let total_lines = all_lines.len();
        let visible_start = self.log_scroll;

        // Show lines from scroll position onwards
        let visible_lines: Vec<Line<'static>> = all_lines
            .iter()
            .skip(visible_start)
            .map(|s| Line::from(s.clone()))
            .collect();

        let title = if total_lines > 1 {
            format!(
                "{} (line {}/{}) [↑↓:scroll PgUp/PgDn:page Home/End:jump]",
                base_title,
                visible_start + 1,
                total_lines
            )
        } else {
            base_title.to_string()
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
                let title = "Overview [Tab:switch tabs]";
                let widget = Paragraph::new(lines)
                    .block(Block::default().borders(Borders::ALL).title(title));
                frame.render_widget(Clear, area);
                frame.render_widget(widget, area);
                return;
            }

            // Build table rows
            let header = Row::new(vec!["#", "Step", "Status", "Summary"])
                .style(Style::default().add_modifier(Modifier::BOLD))
                .bottom_margin(1);

            let rows: Vec<Row> = entries
                .iter()
                .map(|entry| {
                    let status_str = match entry.status {
                        StepStatus::Running => "Running",
                        StepStatus::Success => "✓ Success",
                        StepStatus::Failed => "✗ Failed",
                    };

                    let summary = entry
                        .result_lines
                        .first()
                        .map(|s| {
                            if s.len() > 60 {
                                format!("{}...", &s[..60])
                            } else {
                                s.clone()
                            }
                        })
                        .unwrap_or_else(|| "(no result)".to_string());

                    let style = if entry.step_index == self.selected_step {
                        Style::default().bg(Color::DarkGray)
                    } else {
                        Style::default()
                    };

                    Row::new(vec![
                        format!("{}", entry.step_index),
                        entry.step_name.clone(),
                        status_str.to_string(),
                        summary,
                    ])
                    .style(style)
                })
                .collect();

            let widths = [
                Constraint::Length(4),
                Constraint::Length(20),
                Constraint::Length(12),
                Constraint::Percentage(60),
            ];

            let table = Table::new(rows, widths)
                .header(header)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title("Overview [Tab:switch tabs | Enter:detail | j/k:select]"),
                );

            frame.render_widget(Clear, area);
            frame.render_widget(table, area);
        } else {
            // Show action logs (no structured logs available)
            let (title, lines) = self.log_modal_data();
            let widget = Paragraph::new(lines)
                .block(Block::default().borders(Borders::ALL).title(title));
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

                // Result section
                lines.push(Line::from(Span::styled(
                    "─── Result ───",
                    Style::default().add_modifier(Modifier::BOLD),
                )));
                for line in &entry.result_lines {
                    lines.push(Line::from(line.clone()));
                }

                let visible_lines: Vec<Line> = lines.iter().skip(self.log_scroll).cloned().collect();

                let title = format!(
                    "Detail - Step {} [Tab:switch tabs | Esc:back | ↑↓:scroll]",
                    entry.step_index
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
            let widget = Paragraph::new(lines)
                .block(Block::default().borders(Borders::ALL).title("Detail"));
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
        if count == 0 {
            self.selected = 0;
        } else if self.selected >= count {
            self.selected = count - 1;
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
    in_prompt: bool,
    in_result: bool,
}

#[derive(Debug, Clone)]
struct LogEntry {
    step_index: usize,
    step_name: String,
    prompt_lines: Vec<String>,
    result_lines: Vec<String>,
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
            in_prompt: false,
            in_result: false,
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
            if let Some(content) = line.strip_prefix("[STEP_START:").and_then(|s| s.strip_suffix("]")) {
                let parts: Vec<&str> = content.splitn(2, ':').collect();
                if parts.len() == 2 {
                    if let Ok(idx) = parts[0].parse::<usize>() {
                        self.current_step_index = Some(idx);
                        self.current_step_name = Some(parts[1].to_string());
                        self.current_prompt.clear();
                        self.current_result.clear();
                    }
                }
            }
        } else if line == "[PROMPT_START]" {
            self.in_prompt = true;
            self.in_result = false;
        } else if line == "[PROMPT_END]" {
            self.in_prompt = false;
        } else if line == "[RESULT_START]" {
            self.in_prompt = false;
            self.in_result = true;
        } else if line == "[RESULT_END]" {
            self.in_result = false;
        } else if line.starts_with("[STEP_END:") {
            // Finalize current step
            if let Some(content) = line.strip_prefix("[STEP_END:").and_then(|s| s.strip_suffix("]")) {
                let status = match content {
                    "Success" => StepStatus::Success,
                    "Failed" => StepStatus::Failed,
                    _ => StepStatus::Running,
                };

                if let (Some(idx), Some(name)) = (self.current_step_index, &self.current_step_name) {
                    let entry = LogEntry {
                        step_index: idx,
                        step_name: name.clone(),
                        prompt_lines: self.current_prompt.clone(),
                        result_lines: self.current_result.clone(),
                        status,
                    };
                    self.structured_logs.push(entry);

                    // Reset state
                    self.current_step_index = None;
                    self.current_step_name = None;
                    self.current_prompt.clear();
                    self.current_result.clear();
                }
            }
        } else if self.in_prompt && !line.starts_with("─") {
            // Collect prompt lines (skip separator lines)
            self.current_prompt.push(line.to_string());
        } else if self.in_result && !line.starts_with("─") {
            // Collect result lines (skip separator lines)
            self.current_result.push(line.to_string());
        }
    }
}

enum InputMode {
    FreePrompt { buffer: String, force_new: bool },
    CreateWorkerSelection { selected: usize },
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
