mod actions;
mod event_handler;
mod rendering;
mod types;
mod worker_view;

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;

use anyhow::{Context, Result};
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

use crate::config::{Config, Workflow};
use crate::state::{ActionLogEntry, StateStore};
use crate::ui::{format_action_log, LogViewMode};
use crate::worker::{
    spawn_worker_system, WorkerEventReceiver, WorkerHandle, WorkerId, WorkerSnapshot,
    WorkerStatus,
};

// Re-exported for use in App's public fields
#[allow(unused_imports)]
pub use types::{InteractiveRequest, PermissionPromptState};
pub use worker_view::WorkerView;

const GLOBAL_LOG_CAPACITY: usize = 64;

/// Main application state
pub struct App {
    pub repo_root: PathBuf,
    pub manager: WorkerHandle,
    pub event_rx: WorkerEventReceiver,
    pub state_store: StateStore,
    pub workflows: Vec<Workflow>,
    pub selected_workflow_idx: usize,
    pub workers: Vec<WorkerView>,
    pub selected: usize,
    pub show_help: bool,
    pub show_logs: bool,
    pub log_messages: VecDeque<String>,
    pub log_scroll: usize,
    pub status_filter: Option<WorkerStatus>,
    pub input_mode: Option<types::InputMode>,
    pub log_view_mode: LogViewMode,
    pub selected_step: usize,
    pub animation_frame: usize,
    pub permission_prompt: Option<types::PermissionPromptState>,
    pub permission_tracker: HashMap<u64, types::PermissionTrackerEntry>,
    pub pending_interactive_mode: Option<types::InteractiveRequest>,
    pub auto_scroll_logs: bool,
}

impl App {
    pub fn new() -> Result<Self> {
        let repo_root = std::env::current_dir().context("failed to determine repository root")?;
        let config_path = repo_root.join("workflows.json");
        let loaded =
            Config::load(&config_path).context("failed to load workflow configuration")?;
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

    pub fn on_tick(&mut self) {
        self.poll_events();
        self.clamp_selection();
        self.animation_frame = self.animation_frame.wrapping_add(1);
    }

    pub fn add_or_update_worker(&mut self, snapshot: WorkerSnapshot) {
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

    pub fn remove_worker(&mut self, id: WorkerId) {
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

    pub fn add_worker_log(&mut self, id: WorkerId, line: String) {
        if let Some(pos) = self.workers.iter().position(|view| view.snapshot.id == id) {
            let view = &mut self.workers[pos];
            view.push_log(line);
        }
    }

    pub fn selected_worker_id(&self) -> Option<WorkerId> {
        let indices = self.visible_indices();
        indices
            .get(self.selected)
            .and_then(|idx| self.workers.get(*idx))
            .map(|view| view.snapshot.id)
    }

    pub fn selected_worker_view(&self) -> Option<&WorkerView> {
        let indices = self.visible_indices();
        indices
            .get(self.selected)
            .and_then(|idx| self.workers.get(*idx))
    }

    pub fn visible_indices(&self) -> Vec<usize> {
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

    pub fn clamp_selection(&mut self) {
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

    pub fn push_log(&mut self, message: String) {
        self.push_log_with_worker(None, message);
    }

    pub fn push_log_with_worker(&mut self, worker: Option<&str>, message: String) {
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

    pub fn current_workflow_name(&self) -> &str {
        self.workflows
            .get(self.selected_workflow_idx)
            .map(|wf| wf.name.as_str())
            .unwrap_or("n/a")
    }
}
