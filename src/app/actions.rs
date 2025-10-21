use crate::ui::{permission_mode_label, LogViewMode};
use crate::worker::{CreateWorkerRequest, WorkerId, WorkerStatus, list_existing_worktrees};

use super::types::{InputMode, NameInputNextAction, InteractiveRequest};
use super::App;

impl App {
    pub fn enqueue_create_worker(&mut self) {
        // Show name input modal
        let workflow_name = self
            .workflows
            .get(self.selected_workflow_idx)
            .map(|wf| wf.name.clone());

        self.input_mode = Some(InputMode::NameInput {
            buffer: String::new(),
            workflow_name,
            next_action: NameInputNextAction::CreateWithWorkflow,
        });
    }

    pub fn show_name_input_for_free_prompt(&mut self) {
        self.input_mode = Some(InputMode::NameInput {
            buffer: String::new(),
            workflow_name: None,
            next_action: NameInputNextAction::CreateWithFreePrompt,
        });
    }

    pub fn create_worker_with_default_name(&mut self, workflow_name: Option<String>) {
        let mut request = CreateWorkerRequest::default();
        request.workflow = workflow_name;
        request.name = None; // Use default name

        if let Err(err) = self.manager.create_worker(request) {
            self.push_log(format!("ワーカー作成に失敗しました: {err}"));
        } else {
            self.push_log("ワーカーを作成しました（デフォルト名）".into());
        }
    }

    pub fn create_worker_with_name(&mut self, name: String, workflow_name: Option<String>) {
        let mut request = CreateWorkerRequest::default();
        request.workflow = workflow_name;
        request.name = Some(name.clone());

        if let Err(err) = self.manager.create_worker(request) {
            self.push_log(format!("ワーカー作成に失敗しました: {err}"));
        } else {
            self.push_log(format!("ワーカーを作成しました: {}", name));
        }
    }

    pub fn show_rename_modal(&mut self) {
        if let Some(id) = self.selected_worker_id() {
            self.input_mode = Some(InputMode::RenameWorker {
                buffer: String::new(),
                worker_id: id,
            });
        } else {
            self.push_log("ワーカーが選択されていません".into());
        }
    }

    pub fn rename_worker(&mut self, worker_id: WorkerId, new_name: String) {
        if let Err(err) = self.manager.rename_worker(worker_id, new_name.clone()) {
            self.push_log(format!("ワーカー名の変更に失敗しました: {err}"));
        } else {
            self.push_log(format!("ワーカー名を変更しました: {}", new_name));
        }
    }

    pub fn enqueue_delete_worker(&mut self) {
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

    pub fn enqueue_restart_worker(&mut self) {
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

    pub fn toggle_help(&mut self) {
        self.show_help = !self.show_help;
    }

    pub fn toggle_logs(&mut self) {
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

    pub fn scroll_log_up(&mut self) {
        self.log_scroll = self.log_scroll.saturating_sub(1);
        // Disable auto-scroll when manually scrolling up
        self.auto_scroll_logs = false;
    }

    pub fn scroll_log_down(&mut self) {
        let max_scroll = self.get_log_max_scroll();
        if self.log_scroll < max_scroll {
            self.log_scroll += 1;
        }
        // Re-enable auto-scroll if we reached the bottom
        if self.log_scroll >= max_scroll {
            self.auto_scroll_logs = true;
        }
    }

    pub fn scroll_log_home(&mut self) {
        self.log_scroll = 0;
        // Disable auto-scroll when jumping to top
        self.auto_scroll_logs = false;
    }

    pub fn scroll_log_end(&mut self) {
        self.log_scroll = self.get_log_max_scroll();
        // Re-enable auto-scroll when jumping to bottom
        self.auto_scroll_logs = true;
    }

    pub fn scroll_log_page_up(&mut self) {
        self.log_scroll = self.log_scroll.saturating_sub(10);
        // Disable auto-scroll when paging up
        self.auto_scroll_logs = false;
    }

    pub fn scroll_log_page_down(&mut self) {
        let max_scroll = self.get_log_max_scroll();
        let new_scroll = self.log_scroll.saturating_add(10);
        self.log_scroll = new_scroll.min(max_scroll);
        // Re-enable auto-scroll if we reached the bottom
        if self.log_scroll >= max_scroll {
            self.auto_scroll_logs = true;
        }
    }

    pub fn compact_logs(&mut self) {
        while self.log_messages.len() > 4 {
            self.log_messages.pop_front();
        }
        self.push_log("アクションログを圧縮しました".into());
    }

    pub fn toggle_auto_scroll(&mut self) {
        self.auto_scroll_logs = !self.auto_scroll_logs;
        let status = if self.auto_scroll_logs {
            "ON"
        } else {
            "OFF"
        };
        self.push_log(format!("ログの自動スクロール: {}", status));
    }

    pub fn switch_log_tab_next(&mut self) {
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

    pub fn switch_log_tab_prev(&mut self) {
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

    pub fn select_step_up(&mut self) {
        self.selected_step = self.selected_step.saturating_sub(1);
    }

    pub fn select_step_down(&mut self) {
        if let Some(view) = self.selected_worker_view() {
            if !view.structured_logs.is_empty() {
                let max = view.structured_logs.len() - 1;
                self.selected_step = (self.selected_step + 1).min(max);
            }
        }
    }

    pub fn enter_detail_from_overview(&mut self) {
        // Only enter detail if a valid step is selected
        if let Some(view) = self.selected_worker_view() {
            if self.selected_step < view.structured_logs.len() {
                self.log_view_mode = LogViewMode::Detail;
                self.log_scroll = 0;
            }
        }
    }

    pub fn back_to_overview(&mut self) {
        self.log_view_mode = LogViewMode::Overview;
        self.log_scroll = 0;
    }

    pub fn cycle_workflow(&mut self) {
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

    pub fn show_create_selection(&mut self) {
        self.input_mode = Some(InputMode::CreateWorkerSelection { selected: 0 });
    }

    pub fn show_worktree_selection(&mut self) {
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

    pub fn enqueue_create_worker_with_worktree(
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

    pub fn start_free_prompt(&mut self) {
        self.input_mode = Some(InputMode::FreePrompt {
            buffer: String::new(),
            force_new: false,
            permission_mode: None,
            worker_name: None,
        });
    }

    pub fn start_interactive_prompt(&mut self) {
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

    pub fn submit_free_prompt(
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

    pub fn select_next(&mut self) {
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

    pub fn select_previous(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
            // Reset log view state when switching workers
            self.selected_step = 0;
            self.log_scroll = 0;
        }
    }

    pub fn select_first(&mut self) {
        self.selected = 0;
        // Reset log view state when switching workers
        self.selected_step = 0;
        self.log_scroll = 0;
    }

    pub fn select_last(&mut self) {
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

    /// Toggle session history view
    pub fn toggle_session_history(&mut self) {
        self.show_session_history = !self.show_session_history;
        // Reset scroll and selection when opening
        if self.show_session_history {
            self.session_history_scroll = 0;
            self.selected_session = 0;
        }
    }

    /// Scroll session history view up
    pub fn scroll_session_history_up(&mut self) {
        self.session_history_scroll = self.session_history_scroll.saturating_sub(1);
    }

    /// Scroll session history view down
    pub fn scroll_session_history_down(&mut self) {
        self.session_history_scroll += 1;
    }
}
