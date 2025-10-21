use std::collections::HashMap;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::ui::{permission_mode_label, describe_allowed_tools, AVAILABLE_TOOLS, LogViewMode};
use crate::worker::{PermissionDecision, PermissionRequest, WorkerId, WorkerEvent, WorkerStatus};

use super::types::{InputMode, NameInputNextAction};
use super::App;

impl App {
    /// Handle keyboard input
    pub fn handle_key(&mut self, key_event: KeyEvent) -> bool {
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
                    worker_name,
                } => match key_event.code {
                    KeyCode::Esc => {
                        self.input_mode = None;
                    }
                    KeyCode::Enter => {
                        let prompt = buffer.trim().to_string();
                        let is_force_new = *force_new;
                        let mode = permission_mode.clone();
                        let name = worker_name.clone();
                        self.input_mode = None;
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
                            // Run workflow - show name input first
                            self.enqueue_create_worker();
                        } else if choice == 1 {
                            // Free input - show name input first, then free prompt
                            self.show_name_input_for_free_prompt();
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
                                    super::types::PermissionTrackerEntry {
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
                InputMode::NameInput { buffer, workflow_name, next_action } => match key_event.code {
                    KeyCode::Esc => {
                        // Cancel and use default name
                        let workflow = workflow_name.clone();
                        let action = next_action.clone();
                        self.input_mode = None;

                        match action {
                            NameInputNextAction::CreateWithWorkflow => {
                                self.create_worker_with_default_name(workflow);
                            }
                            NameInputNextAction::CreateWithFreePrompt => {
                                // Show free prompt modal with default name
                                self.input_mode = Some(InputMode::FreePrompt {
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
                        self.input_mode = None;

                        match action {
                            NameInputNextAction::CreateWithWorkflow => {
                                if name.is_empty() {
                                    // Use default name
                                    self.create_worker_with_default_name(workflow);
                                } else {
                                    // Use user-provided name
                                    self.create_worker_with_name(name, workflow);
                                }
                            }
                            NameInputNextAction::CreateWithFreePrompt => {
                                // Show free prompt modal
                                let worker_name = if name.is_empty() { None } else { Some(name) };
                                self.input_mode = Some(InputMode::FreePrompt {
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
                },
                InputMode::RenameWorker { buffer, worker_id } => match key_event.code {
                    KeyCode::Esc => {
                        self.input_mode = None;
                    }
                    KeyCode::Enter => {
                        let new_name = buffer.trim().to_string();
                        let wid = *worker_id;
                        self.input_mode = None;
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
                },
            }
            return false;
        }

        match key_event.code {
            KeyCode::Char('q') => return true,
            KeyCode::Char('c') => self.show_create_selection(),
            KeyCode::Char('d') => self.enqueue_delete_worker(),
            KeyCode::Char('r') => self.enqueue_restart_worker(),
            KeyCode::Char('n') => self.show_rename_modal(),
            KeyCode::Char('i') => self.start_free_prompt(),
            KeyCode::Char('h') => self.toggle_help(),
            KeyCode::Char('l') => self.toggle_logs(),
            KeyCode::Char('s') => self.toggle_session_history(),
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
                if self.show_session_history {
                    self.show_session_history = false;
                } else if self.show_logs
                    && (self.log_view_mode == LogViewMode::Detail
                        || self.log_view_mode == LogViewMode::Raw)
                {
                    self.back_to_overview();
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if self.show_session_history {
                    self.scroll_session_history_up();
                } else if self.show_logs {
                    match self.log_view_mode {
                        LogViewMode::Overview => self.select_step_up(),
                        LogViewMode::Detail | LogViewMode::Raw => self.scroll_log_up(),
                    }
                } else {
                    self.select_previous();
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.show_session_history {
                    self.scroll_session_history_down();
                } else if self.show_logs {
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
                    super::types::PermissionTrackerEntry {
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

        self.permission_prompt = Some(super::types::PermissionPromptState {
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
            super::types::PermissionTrackerEntry {
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

    pub fn worker_name_by_id(&self, id: WorkerId) -> Option<String> {
        self.workers
            .iter()
            .find(|view| view.snapshot.id == id)
            .map(|view| view.snapshot.name.clone())
    }

    /// Poll for worker events and update state
    pub fn poll_events(&mut self) {
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

    pub fn get_log_max_scroll(&self) -> usize {
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
}
