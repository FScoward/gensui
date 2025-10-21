use std::collections::HashMap;
use std::path::PathBuf;

use crate::worker::{ExistingWorktree, PermissionDecision, PermissionRequest, WorkerId};

/// Input modes for the TUI
pub enum InputMode {
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

/// Next action after name input
#[derive(Clone)]
pub enum NameInputNextAction {
    CreateWithWorkflow,
    CreateWithFreePrompt,
}

/// Permission prompt state
pub struct PermissionPromptState {
    pub worker_id: WorkerId,
    pub worker_name: String,
    pub request: PermissionRequest,
    pub selection: PermissionDecision,
}

impl PermissionPromptState {
    pub fn toggle(&mut self) {
        self.selection = match &self.selection {
            PermissionDecision::Allow { .. } => PermissionDecision::Deny,
            PermissionDecision::Deny => PermissionDecision::Allow {
                permission_mode: None,
                allowed_tools: None,
            },
        };
    }
}

/// Permission tracker entry
pub struct PermissionTrackerEntry {
    pub worker_name: String,
    pub step_name: String,
}

/// Interactive mode request
pub struct InteractiveRequest {
    pub worker_name: String,
    pub worktree_path: PathBuf,
}
