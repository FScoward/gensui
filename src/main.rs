mod config;
mod worker;

use std::collections::VecDeque;
use std::io::{self, Stdout};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Clear, Paragraph, Row, Table, Wrap};

use crate::config::{Config, Workflow};
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
                    match key_event.code {
                        KeyCode::Char('q') => break,
                        KeyCode::Char('c') => app.enqueue_create_worker(),
                        KeyCode::Char('d') => app.enqueue_delete_worker(),
                        KeyCode::Char('r') => app.enqueue_restart_worker(),
                        KeyCode::Char('h') => app.toggle_help(),
                        KeyCode::Char('l') => app.toggle_logs(),
                        KeyCode::Char('w') => app.cycle_workflow(),
                        KeyCode::Char('a') => app.cycle_filter(),
                        KeyCode::Up | KeyCode::Char('k') => app.select_previous(),
                        KeyCode::Down | KeyCode::Char('j') => app.select_next(),
                        KeyCode::Home => app.select_first(),
                        KeyCode::End => app.select_last(),
                        KeyCode::Char('C') if key_event.modifiers.contains(KeyModifiers::SHIFT) => {
                            app.compact_logs()
                        }
                        _ => {}
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
    workflows: Vec<Workflow>,
    selected_workflow_idx: usize,
    workers: Vec<WorkerView>,
    selected: usize,
    show_help: bool,
    show_logs: bool,
    log_messages: VecDeque<String>,
    status_filter: Option<WorkerStatus>,
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

        let workflows = config.workflows.clone();
        let default_idx = config
            .default_workflow
            .as_ref()
            .and_then(|name| workflows.iter().position(|wf| &wf.name == name))
            .unwrap_or(0);
        let selected_workflow_idx = if workflows.is_empty() {
            0
        } else {
            default_idx.min(workflows.len() - 1)
        };

        let (manager, event_rx) = spawn_worker_system(repo_root, config)?;

        Ok(Self {
            manager,
            event_rx,
            workflows,
            selected_workflow_idx,
            workers: Vec::new(),
            selected: 0,
            show_help: false,
            show_logs: false,
            log_messages: VecDeque::with_capacity(GLOBAL_LOG_CAPACITY),
            status_filter: None,
        })
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
            if let Err(err) = self.manager.delete_worker(id) {
                self.push_log(format!("ワーカー削除に失敗しました ({:?}): {err}", id));
            }
        }
    }

    fn enqueue_restart_worker(&mut self) {
        if let Some(id) = self.selected_worker_id() {
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
    }

    fn compact_logs(&mut self) {
        while self.log_messages.len() > 4 {
            self.log_messages.pop_front();
        }
        self.push_log("アクションログを圧縮しました".into());
    }

    fn cycle_filter(&mut self) {
        self.status_filter = match self.status_filter {
            None => Some(WorkerStatus::Running),
            Some(WorkerStatus::Running) => Some(WorkerStatus::Paused),
            Some(WorkerStatus::Paused) => Some(WorkerStatus::Failed),
            Some(WorkerStatus::Failed) => Some(WorkerStatus::Idle),
            Some(WorkerStatus::Idle) => None,
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
                    self.push_log(format!(
                        "{} をプロビジョニングしました (workflow: {}, steps: {})",
                        snapshot.name, snapshot.workflow, snapshot.total_steps
                    ));
                }
                WorkerEvent::Updated(snapshot) => {
                    self.add_or_update_worker(snapshot.clone());
                    self.push_log(format!("{}: {}", snapshot.name, snapshot.last_event));
                }
                WorkerEvent::Log { id, line } => {
                    self.add_worker_log(id, line);
                }
                WorkerEvent::Deleted { id, message } => {
                    self.remove_worker(id);
                    self.push_log(message);
                }
                WorkerEvent::Error { id, message } => {
                    if let Some(worker_id) = id {
                        self.add_worker_log(worker_id, format!("エラー: {message}"));
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
            self.render_modal(frame, 70, 45, "Logs", self.log_lines_for_modal());
        }

        if self.show_help {
            self.render_modal(frame, 60, 50, "Help", self.help_lines());
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
                Span::raw(
                    "Shift+C: compact logs | 選択ワーカーのログは 'l' で確認 | Active workflow: ",
                ),
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

    fn help_lines(&self) -> Vec<Line<'static>> {
        vec![
            Line::raw("MVP ショートカット"),
            Line::raw(""),
            Line::raw("c – git worktree を作成しワーカーを起動"),
            Line::raw("d – ワーカー停止と worktree 削除"),
            Line::raw("r – ワーカーを再起動"),
            Line::raw("a – ステータスフィルタを切り替え"),
            Line::raw("w – 使用するワークフローを切り替え"),
            Line::raw("j/k または ↑/↓ – 選択移動"),
            Line::raw("l – 選択ワーカーのログを表示"),
            Line::raw("h – このヘルプを表示"),
            Line::raw("Shift+C – アクションログを圧縮"),
            Line::raw("q – 終了"),
        ]
    }

    fn log_lines_for_modal(&self) -> Vec<Line<'static>> {
        if let Some(view) = self.selected_worker_view() {
            if view.logs.is_empty() {
                vec![Line::raw("このワーカーのログはまだありません。")]
            } else {
                view.logs.iter().cloned().map(Line::from).collect()
            }
        } else if self.log_messages.is_empty() {
            vec![Line::raw("アクションログはまだありません。")]
        } else {
            self.log_messages.iter().cloned().map(Line::from).collect()
        }
    }

    fn add_or_update_worker(&mut self, snapshot: WorkerSnapshot) {
        if let Some(view) = self
            .workers
            .iter_mut()
            .find(|view| view.snapshot.id == snapshot.id)
        {
            view.update_snapshot(snapshot);
        } else {
            self.workers.push(WorkerView::new(snapshot));
        }
        self.clamp_selection();
    }

    fn remove_worker(&mut self, id: WorkerId) {
        if let Some(pos) = self.workers.iter().position(|view| view.snapshot.id == id) {
            self.workers.remove(pos);
        }
        self.clamp_selection();
    }

    fn add_worker_log(&mut self, id: WorkerId, line: String) {
        if let Some(view) = self.workers.iter_mut().find(|view| view.snapshot.id == id) {
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
        if self.log_messages.len() >= GLOBAL_LOG_CAPACITY {
            self.log_messages.pop_front();
        }
        self.log_messages.push_back(message);
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
}

impl WorkerView {
    const LOG_CAPACITY: usize = 128;

    fn new(snapshot: WorkerSnapshot) -> Self {
        Self {
            snapshot,
            logs: VecDeque::with_capacity(Self::LOG_CAPACITY),
        }
    }

    fn update_snapshot(&mut self, snapshot: WorkerSnapshot) {
        self.snapshot = snapshot;
    }

    fn push_log(&mut self, line: String) {
        if self.logs.len() >= Self::LOG_CAPACITY {
            self.logs.pop_front();
        }
        self.logs.push_back(line);
    }
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

fn status_color(status: WorkerStatus) -> Color {
    match status {
        WorkerStatus::Running => Color::Green,
        WorkerStatus::Paused => Color::Yellow,
        WorkerStatus::Failed => Color::Red,
        WorkerStatus::Idle => Color::Gray,
    }
}
