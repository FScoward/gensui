use std::collections::HashMap;

use ratatui::layout::Rect;
use ratatui::text::Line;

use crate::ui::{
    centered_rect, help_lines, prepare_raw_log_data, render_create_selection_modal,
    render_detail_tab, render_footer, render_header, render_log_modal, render_modal,
    render_name_input_modal, render_overview_tab, render_permission_modal, render_prompt_modal,
    render_rename_worker_modal, render_session_history_modal, render_table, render_tool_selection_modal,
    render_worktree_selection_modal, LogViewMode,
};
use crate::worker::{ExistingWorktree, WorkerSnapshot};

use super::types::InputMode;
use super::App;

impl App {
    /// Main render function
    pub fn render(&self, frame: &mut ratatui::Frame<'_>) {
        let layout = ratatui::layout::Layout::default()
            .direction(ratatui::layout::Direction::Vertical)
            .constraints([
                ratatui::layout::Constraint::Length(3),
                ratatui::layout::Constraint::Min(8),
                ratatui::layout::Constraint::Length(5),
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

        if self.show_session_history {
            self.render_session_history_modal(frame);
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
                InputMode::NameInput { buffer, workflow_name, .. } => {
                    self.render_name_input_modal(frame, buffer, workflow_name);
                }
                InputMode::RenameWorker { buffer, worker_id } => {
                    if let Some(worker) = self.workers.iter().find(|w| w.snapshot.id == *worker_id) {
                        self.render_rename_worker_modal(frame, buffer, &worker.snapshot.name);
                    }
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
            .map(|&idx| (idx, &self.workers[idx].snapshot))
            .collect();

        render_table(
            frame,
            area,
            &workers_data,
            self.selected,
            self.animation_frame,
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
        prompt: &super::types::PermissionPromptState,
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

    pub fn help_lines(&self) -> Vec<Line<'static>> {
        help_lines()
    }

    fn log_modal_data(&self) -> (String, Vec<Line<'static>>) {
        let worker_logs = self.selected_worker_view().map(|view| &view.logs);
        let data = prepare_raw_log_data(
            worker_logs,
            &self.log_messages,
            self.log_scroll,
            self.auto_scroll_logs,
        );
        (data.title, data.lines)
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
        if let Some(view) = self.selected_worker_view() {
            render_overview_tab(
                frame,
                &view.structured_logs,
                self.selected_step,
                self.auto_scroll_logs,
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
            if let Some(entry) = view.structured_logs.get(self.selected_step) {
                render_detail_tab(frame, entry, self.log_scroll, self.auto_scroll_logs);
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

    fn render_session_history_modal(&self, frame: &mut ratatui::Frame<'_>) {
        let area = centered_rect(85, 80, frame.area());
        let sessions = self.get_selected_worker_session_histories();
        render_session_history_modal(
            frame,
            area,
            &sessions,
            self.selected_session,
            self.session_history_scroll,
        );
    }
}
