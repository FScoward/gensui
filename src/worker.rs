use std::collections::{HashMap, VecDeque};
use std::env;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crate::config::{ClaudeStep, Config, Workflow, WorkflowStep};
use crate::state::{ManagerState, StateStore};
use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct WorkerId(pub usize);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkerStatus {
    Idle,
    Running,
    Paused,
    Failed,
    Archived,
}

impl WorkerStatus {
    pub fn label(&self) -> &'static str {
        match self {
            WorkerStatus::Idle => "Idle",
            WorkerStatus::Running => "Running",
            WorkerStatus::Paused => "Paused",
            WorkerStatus::Failed => "Failed",
            WorkerStatus::Archived => "Archived",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkerSnapshot {
    pub id: WorkerId,
    pub name: String,
    pub issue: Option<String>,
    pub agent: String,
    pub worktree: String,
    pub branch: String,
    pub status: WorkerStatus,
    pub last_event: String,
    pub workflow: String,
    pub total_steps: usize,
    pub current_step: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
}

#[derive(Default, Clone, Debug)]
pub struct CreateWorkerRequest {
    pub issue: Option<String>,
    pub agent: Option<String>,
    pub workflow: Option<String>,
    pub free_prompt: Option<String>,
    pub existing_worktree: Option<(PathBuf, String)>, // (worktree_path, branch_name)
    pub permission_mode: Option<String>,
}

#[derive(Clone, Debug)]
pub struct ExistingWorktree {
    pub path: PathBuf,
    pub branch: String,
}

pub enum WorkerCommand {
    Create(CreateWorkerRequest),
    Delete {
        id: WorkerId,
    },
    Restart {
        id: WorkerId,
    },
    Continue {
        id: WorkerId,
        prompt: String,
        permission_mode: Option<String>,
    },
    Persist {
        id: WorkerId,
    },
    PermissionPrompt {
        id: WorkerId,
        request: PermissionRequest,
        respond_to: Sender<PermissionDecision>,
    },
    PermissionResponse {
        id: WorkerId,
        request_id: u64,
        decision: PermissionDecision,
    },
}

#[derive(Clone, Debug)]
pub enum WorkerEvent {
    Created(WorkerSnapshot),
    Updated(WorkerSnapshot),
    Log {
        id: WorkerId,
        line: String,
    },
    Deleted {
        id: WorkerId,
        message: String,
    },
    Error {
        id: Option<WorkerId>,
        message: String,
    },
    PermissionRequested {
        id: WorkerId,
        request: PermissionRequest,
    },
    PermissionResolved {
        id: WorkerId,
        request_id: u64,
        decision: PermissionDecision,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum PermissionDecision {
    Allow {
        permission_mode: Option<String>,
        allowed_tools: Option<Vec<String>>,
    },
    Deny,
}

#[derive(Clone, Debug)]
pub struct PermissionRequest {
    pub request_id: u64,
    pub step_name: String,
    pub description: Option<String>,
    pub permission_mode: Option<String>,
    pub allowed_tools: Option<Vec<String>>,
}

static NEXT_PERMISSION_REQUEST_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone)]
pub struct WorkerHandle {
    cmd_tx: Sender<WorkerCommand>,
}

impl WorkerHandle {
    pub fn create_worker(&self, request: CreateWorkerRequest) -> Result<()> {
        self.cmd_tx
            .send(WorkerCommand::Create(request))
            .map_err(|err| anyhow!("failed to enqueue worker creation: {err}"))
    }

    pub fn delete_worker(&self, id: WorkerId) -> Result<()> {
        self.cmd_tx
            .send(WorkerCommand::Delete { id })
            .map_err(|err| anyhow!("failed to enqueue worker deletion: {err}"))
    }

    pub fn restart_worker(&self, id: WorkerId) -> Result<()> {
        self.cmd_tx
            .send(WorkerCommand::Restart { id })
            .map_err(|err| anyhow!("failed to enqueue worker restart: {err}"))
    }

    pub fn continue_worker(
        &self,
        id: WorkerId,
        prompt: String,
        permission_mode: Option<String>,
    ) -> Result<()> {
        self.cmd_tx
            .send(WorkerCommand::Continue {
                id,
                prompt,
                permission_mode,
            })
            .map_err(|err| anyhow!("failed to enqueue worker continuation: {err}"))
    }

    pub fn respond_permission(
        &self,
        id: WorkerId,
        request_id: u64,
        decision: PermissionDecision,
    ) -> Result<()> {
        self.cmd_tx
            .send(WorkerCommand::PermissionResponse {
                id,
                request_id,
                decision,
            })
            .map_err(|err| anyhow!("failed to enqueue permission response: {err}"))
    }
}

pub type WorkerEventReceiver = Receiver<WorkerEvent>;

pub fn spawn_worker_system(
    repo_root: PathBuf,
    config: Config,
) -> Result<(WorkerHandle, WorkerEventReceiver)> {
    let (cmd_tx, cmd_rx) = mpsc::channel();
    let (evt_tx, evt_rx) = mpsc::channel();
    let state_store = StateStore::new(repo_root.join(".gensui/state"))?;
    let initial_state = state_store
        .load_manager()?
        .unwrap_or_else(|| ManagerState { next_id: 1 });
    let next_id = initial_state.next_id.max(1);
    let manager = WorkerManager::new(
        repo_root,
        config,
        state_store,
        cmd_tx.clone(),
        cmd_rx,
        evt_tx.clone(),
        next_id,
    );

    thread::Builder::new()
        .name("gensui-worker-manager".into())
        .spawn(move || manager.run())
        .context("failed to spawn worker manager thread")?;

    Ok((WorkerHandle { cmd_tx }, evt_rx))
}

struct WorkerManager {
    repo_root: PathBuf,
    config: Config,
    state_store: StateStore,
    cmd_tx: Sender<WorkerCommand>,
    cmd_rx: Receiver<WorkerCommand>,
    evt_tx: Sender<WorkerEvent>,
    next_id: usize,
    workers: HashMap<WorkerId, WorkerRuntime>,
    pending_permissions: HashMap<u64, PendingPermission>,
}

struct PendingPermission {
    worker_id: WorkerId,
    respond_to: Sender<PermissionDecision>,
}

impl WorkerManager {
    fn new(
        repo_root: PathBuf,
        config: Config,
        state_store: StateStore,
        cmd_tx: Sender<WorkerCommand>,
        cmd_rx: Receiver<WorkerCommand>,
        evt_tx: Sender<WorkerEvent>,
        next_id: usize,
    ) -> Self {
        Self {
            repo_root,
            config,
            state_store,
            cmd_tx,
            cmd_rx,
            evt_tx,
            next_id,
            workers: HashMap::new(),
            pending_permissions: HashMap::new(),
        }
    }

    fn restore_workers(&mut self) {
        let records = match self.state_store.load_workers() {
            Ok(records) => records,
            Err(err) => {
                eprintln!("Failed to load worker states: {err}");
                return;
            }
        };

        for record in records {
            let worktree_path = self.repo_root.join(&record.snapshot.worktree);
            let worker_id = WorkerId(record.snapshot.id);

            // Update next_id to avoid conflicts
            if record.snapshot.id >= self.next_id {
                self.next_id = record.snapshot.id + 1;
                self.persist_manager_state();
            }

            let worktree_exists = worktree_path.exists();

            // Create snapshot from saved data
            let snapshot = WorkerSnapshot {
                id: worker_id,
                name: record.snapshot.name.clone(),
                issue: record.snapshot.issue.clone(),
                agent: record.snapshot.agent.clone(),
                worktree: record.snapshot.worktree.clone(),
                branch: record.snapshot.branch.clone(),
                status: if worktree_exists {
                    WorkerStatus::Idle
                } else {
                    WorkerStatus::Archived
                },
                last_event: if worktree_exists {
                    "Restored from saved state".to_string()
                } else {
                    "Archived (worktree removed)".to_string()
                },
                workflow: record.workflow.name.clone(),
                total_steps: record.workflow.steps.len(),
                current_step: None,
                session_id: record.snapshot.session_id.clone(),
            };

            if worktree_exists {
                // Create WorkerRuntime for active workers
                match WorkerRuntime::new(
                    snapshot.clone(),
                    worktree_path,
                    record.snapshot.branch.clone(),
                    record.workflow,
                    self.cmd_tx.clone(),
                ) {
                    Ok(runtime) => {
                        runtime
                            .completed_steps
                            .store(record.completed_steps, Ordering::SeqCst);

                        // Restore logs to runtime
                        for log_line in &record.logs {
                            runtime.add_log(log_line.clone());
                        }

                        self.workers.insert(worker_id, runtime);

                        // Notify UI
                        let _ = self.evt_tx.send(WorkerEvent::Created(snapshot.clone()));

                        // Send logs to UI
                        for log_line in record.logs {
                            let _ = self.evt_tx.send(WorkerEvent::Log {
                                id: worker_id,
                                line: log_line,
                            });
                        }
                    }
                    Err(err) => {
                        eprintln!("Failed to restore worker {}: {err}", record.snapshot.name);
                    }
                }
            } else {
                // For archived workers (no worktree), just send to UI for viewing
                let _ = self.evt_tx.send(WorkerEvent::Created(snapshot.clone()));

                // Send logs to UI
                for log_line in record.logs {
                    let _ = self.evt_tx.send(WorkerEvent::Log {
                        id: worker_id,
                        line: log_line,
                    });
                }
            }
        }
    }

    fn run(mut self) {
        // Give UI time to start polling events before restoring workers
        thread::sleep(Duration::from_millis(100));

        // Restore workers before entering command loop
        self.restore_workers();

        while let Ok(command) = self.cmd_rx.recv() {
            match command {
                WorkerCommand::Create(request) => {
                    if let Err(err) = self.handle_create(request) {
                        let _ = self.evt_tx.send(WorkerEvent::Error {
                            id: None,
                            message: err.to_string(),
                        });
                    }
                }
                WorkerCommand::Delete { id } => {
                    if let Err(err) = self.handle_delete(id) {
                        let _ = self.evt_tx.send(WorkerEvent::Error {
                            id: Some(id),
                            message: err.to_string(),
                        });
                    }
                }
                WorkerCommand::Restart { id } => {
                    if let Err(err) = self.handle_restart(id) {
                        let _ = self.evt_tx.send(WorkerEvent::Error {
                            id: Some(id),
                            message: err.to_string(),
                        });
                    }
                }
                WorkerCommand::Continue {
                    id,
                    prompt,
                    permission_mode,
                } => {
                    if let Err(err) = self.handle_continue(id, prompt, permission_mode) {
                        let _ = self.evt_tx.send(WorkerEvent::Error {
                            id: Some(id),
                            message: err.to_string(),
                        });
                    }
                }
                WorkerCommand::Persist { id } => {
                    self.persist_worker(id);
                }
                WorkerCommand::PermissionPrompt {
                    id,
                    request,
                    respond_to,
                } => {
                    self.handle_permission_prompt(id, request, respond_to);
                }
                WorkerCommand::PermissionResponse {
                    id,
                    request_id,
                    decision,
                } => {
                    self.handle_permission_response(id, request_id, decision);
                }
            }
        }

        self.shutdown_all();
    }

    fn handle_create(&mut self, request: CreateWorkerRequest) -> Result<()> {
        let worker_id = WorkerId(self.next_id);
        self.next_id += 1;
        self.persist_manager_state();

        let (worktree_path, branch, rel_worktree) =
            if let Some((existing_path, existing_branch)) = request.existing_worktree {
                // Use existing worktree
                let rel_path = existing_path
                    .strip_prefix(&self.repo_root)
                    .unwrap_or(&existing_path)
                    .to_string_lossy()
                    .to_string();
                (existing_path, existing_branch, rel_path)
            } else {
                // Create new worktree
                fs::create_dir_all(self.repo_root.join(".worktrees"))
                    .context("failed to create .worktrees directory")?;

                let timestamp = OffsetDateTime::now_utc().unix_timestamp();
                let worktree_name = format!("worker-{id:03}-{timestamp}", id = worker_id.0);
                let rel_worktree = format!(".worktrees/{worktree_name}");
                let worktree_path = self.repo_root.join(&rel_worktree);

                let branch = format!("gensui/{worktree_name}");
                let base_ref = determine_base_ref(&self.repo_root).unwrap_or_else(|| "HEAD".into());

                let output = Command::new("git")
                    .args([
                        "worktree",
                        "add",
                        &worktree_path.to_string_lossy(),
                        "-b",
                        &branch,
                        &base_ref,
                    ])
                    .current_dir(&self.repo_root)
                    .stderr(Stdio::piped())
                    .stdout(Stdio::piped())
                    .output()
                    .with_context(|| "failed to execute git worktree add")?;

                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    return Err(anyhow!(
                        "git worktree add failed: status={} stdout='{}' stderr='{}'",
                        output.status,
                        stdout.trim(),
                        stderr.trim()
                    ));
                }

                (worktree_path, branch, rel_worktree)
            };

        let workflow = if let Some(prompt) = request.free_prompt.clone() {
            let permission_mode = request.permission_mode.clone();

            Workflow {
                name: format!("free-form-{}", worker_id.0),
                description: Some("On-demand Claude execution".to_string()),
                steps: vec![WorkflowStep {
                    name: "Free Prompt".to_string(),
                    command: None,
                    claude: Some(ClaudeStep {
                        prompt,
                        model: None,
                        allowed_tools: None,
                        permission_mode,
                        extra_args: None,
                    }),
                    description: Some("User supplied prompt".to_string()),
                }],
            }
        } else {
            request
                .workflow
                .as_deref()
                .and_then(|name| self.config.workflow_by_name(name))
                .cloned()
                .unwrap_or_else(|| self.config.default_workflow().clone())
        };

        let agent = request
            .agent
            .unwrap_or_else(|| "Claude Code (simulated)".to_string());
        let issue = request.issue;
        let total_steps = workflow.steps().len();

        let snapshot = WorkerSnapshot {
            id: worker_id,
            name: format!("worker-{}", worker_id.0),
            issue,
            agent,
            worktree: rel_worktree.clone(),
            branch: branch.clone(),
            status: WorkerStatus::Running,
            last_event: "Worktree provisioned".into(),
            workflow: workflow.name.clone(),
            total_steps,
            current_step: None,
            session_id: None,
        };

        let runtime = WorkerRuntime::new(
            snapshot,
            worktree_path,
            branch,
            workflow,
            self.cmd_tx.clone(),
        );
        let mut runtime = match runtime {
            Ok(runtime) => runtime,
            Err(err) => {
                let _ = self.evt_tx.send(WorkerEvent::Error {
                    id: Some(worker_id),
                    message: err.to_string(),
                });
                return Err(err);
            }
        };

        let snapshot_for_event = runtime.snapshot();
        let _ = self.evt_tx.send(WorkerEvent::Created(snapshot_for_event));

        if let Some(requested) = request.workflow.as_ref() {
            if requested != &runtime.workflow.name {
                let _ = self.evt_tx.send(WorkerEvent::Log {
                    id: worker_id,
                    line: format!(
                        "Requested workflow '{}' not found. Using '{}' instead",
                        requested, runtime.workflow.name
                    ),
                });
            }
        }

        runtime.start_agent(&self.evt_tx);
        self.workers.insert(worker_id, runtime);

        self.persist_worker(worker_id);

        Ok(())
    }

    fn handle_delete(&mut self, id: WorkerId) -> Result<()> {
        let mut runtime = self
            .workers
            .remove(&id)
            .ok_or_else(|| anyhow!("worker {:?} not found", id))?;

        let snapshot = runtime.snapshot();

        runtime.stop_agent();

        let worktree_path = runtime.worktree_path.clone();
        let branch = runtime.branch.clone();

        let remove_output = Command::new("git")
            .args([
                "worktree",
                "remove",
                "--force",
                &worktree_path.to_string_lossy(),
            ])
            .current_dir(&self.repo_root)
            .stderr(Stdio::piped())
            .stdout(Stdio::piped())
            .output()
            .with_context(|| "failed to execute git worktree remove")?;

        if !remove_output.status.success() {
            let stderr = String::from_utf8_lossy(&remove_output.stderr);
            let stdout = String::from_utf8_lossy(&remove_output.stdout);
            let message = format!(
                "git worktree remove failed: stdout='{}' stderr='{}'",
                stdout.trim(),
                stderr.trim()
            );
            let _ = self.evt_tx.send(WorkerEvent::Error {
                id: Some(id),
                message: message.clone(),
            });
        }

        let branch_output = Command::new("git")
            .args(["branch", "-D", &branch])
            .current_dir(&self.repo_root)
            .stderr(Stdio::piped())
            .stdout(Stdio::piped())
            .output()
            .with_context(|| "failed to execute git branch -D")?;

        if !branch_output.status.success() {
            let stderr = String::from_utf8_lossy(&branch_output.stderr);
            let stdout = String::from_utf8_lossy(&branch_output.stdout);
            let message = format!(
                "git branch -D {} failed: stdout='{}' stderr='{}'",
                branch,
                stdout.trim(),
                stderr.trim()
            );
            let _ = self.evt_tx.send(WorkerEvent::Error {
                id: Some(id),
                message: message.clone(),
            });
        }

        let _ = self.evt_tx.send(WorkerEvent::Deleted {
            id,
            message: format!("Removed worktree {}", worktree_path.display()),
        });

        if let Err(err) = self.state_store.delete_worker(&snapshot.name) {
            eprintln!("Failed to delete worker state {}: {err}", snapshot.name);
        }

        self.cancel_pending_permissions_for_worker(id);

        Ok(())
    }

    fn handle_restart(&mut self, id: WorkerId) -> Result<()> {
        self.cancel_pending_permissions_for_worker(id);

        let runtime = self
            .workers
            .get_mut(&id)
            .ok_or_else(|| anyhow!("worker {:?} not found", id))?;

        runtime.stop_agent();

        // Reset completed steps to restart from beginning
        runtime.completed_steps.store(0, Ordering::SeqCst);

        {
            let mut snapshot = runtime.state.lock().expect("worker snapshot poisoned");
            snapshot.status = WorkerStatus::Running;
            snapshot.last_event = "Restart requested".into();
            snapshot.current_step = None;
            let _ = self.evt_tx.send(WorkerEvent::Updated(snapshot.clone()));
        }

        runtime.start_agent(&self.evt_tx);

        self.persist_worker(id);

        Ok(())
    }

    fn handle_continue(
        &mut self,
        id: WorkerId,
        prompt: String,
        permission_mode: Option<String>,
    ) -> Result<()> {
        self.cancel_pending_permissions_for_worker(id);

        let runtime = self
            .workers
            .get_mut(&id)
            .ok_or_else(|| anyhow!("worker {:?} not found", id))?;

        // Stop current agent execution
        runtime.stop_agent();

        // Mark all current steps as completed
        runtime
            .completed_steps
            .store(runtime.workflow.steps.len(), Ordering::SeqCst);

        // Create a new workflow step with the continuation prompt
        let continue_step = WorkflowStep {
            name: "Continue".to_string(),
            command: None,
            claude: Some(ClaudeStep {
                prompt,
                model: None,
                allowed_tools: None,
                permission_mode,
                extra_args: None,
            }),
            description: Some("User follow-up instruction".to_string()),
        };

        // Add the new step to the workflow
        runtime.workflow.steps.push(continue_step);

        {
            let mut snapshot = runtime.state.lock().expect("worker snapshot poisoned");
            snapshot.status = WorkerStatus::Running;
            snapshot.last_event = "Continue with new instruction".into();
            snapshot.total_steps = runtime.workflow.steps.len();
            let _ = self.evt_tx.send(WorkerEvent::Updated(snapshot.clone()));
        }

        let _ = self.evt_tx.send(WorkerEvent::Log {
            id,
            line: "追加指示を受信しました".to_string(),
        });

        // Restart agent with updated workflow
        runtime.start_agent(&self.evt_tx);

        self.persist_worker(id);

        Ok(())
    }

    fn handle_permission_prompt(
        &mut self,
        id: WorkerId,
        request: PermissionRequest,
        respond_to: Sender<PermissionDecision>,
    ) {
        self.pending_permissions.insert(
            request.request_id,
            PendingPermission {
                worker_id: id,
                respond_to,
            },
        );

        let _ = self
            .evt_tx
            .send(WorkerEvent::PermissionRequested { id, request });
    }

    fn handle_permission_response(
        &mut self,
        id: WorkerId,
        request_id: u64,
        decision: PermissionDecision,
    ) {
        if let Some(pending) = self.pending_permissions.remove(&request_id) {
            let _ = pending.respond_to.send(decision.clone());
            let _ = self.evt_tx.send(WorkerEvent::PermissionResolved {
                id,
                request_id,
                decision,
            });
        } else {
            let _ = self.evt_tx.send(WorkerEvent::Error {
                id: Some(id),
                message: format!("permission request {} not found", request_id),
            });
        }
    }

    fn cancel_pending_permissions_for_worker(&mut self, id: WorkerId) {
        let mut orphaned = Vec::new();
        for (request_id, pending) in self.pending_permissions.iter() {
            if pending.worker_id == id {
                orphaned.push(*request_id);
            }
        }

        for request_id in orphaned {
            if let Some(pending) = self.pending_permissions.remove(&request_id) {
                let _ = pending.respond_to.send(PermissionDecision::Deny);
            }
        }
    }

    fn shutdown_all(&mut self) {
        // Persist all workers before shutting down
        for id in self.workers.keys().copied().collect::<Vec<_>>() {
            self.persist_worker(id);
        }

        // Then stop all workers
        for (_, mut runtime) in self.workers.drain() {
            runtime.stop_agent();
        }

        for (_, pending) in self.pending_permissions.drain() {
            let _ = pending.respond_to.send(PermissionDecision::Deny);
        }
    }

    fn persist_manager_state(&self) {
        if let Err(err) = self.state_store.save_manager(&ManagerState {
            next_id: self.next_id,
        }) {
            eprintln!("Failed to persist manager state: {err}");
        }
    }

    fn persist_worker(&self, id: WorkerId) {
        if let Some(runtime) = self.workers.get(&id) {
            use crate::state::{WorkerRecord, WorkerSnapshotData};

            let snapshot = runtime.snapshot();
            let logs = runtime.get_logs();

            let record = WorkerRecord {
                snapshot: WorkerSnapshotData {
                    id: snapshot.id.0,
                    name: snapshot.name.clone(),
                    issue: snapshot.issue.clone(),
                    agent: snapshot.agent.clone(),
                    worktree: snapshot.worktree.clone(),
                    branch: snapshot.branch.clone(),
                    status: snapshot.status.label().to_string(),
                    last_event: snapshot.last_event.clone(),
                    workflow: snapshot.workflow.clone(),
                    total_steps: snapshot.total_steps,
                    current_step: snapshot.current_step.clone(),
                    session_id: snapshot.session_id.clone(),
                },
                logs,
                workflow: runtime.workflow.clone(),
                completed_steps: runtime.completed_steps.load(Ordering::SeqCst),
            };

            if let Err(err) = self.state_store.save_worker(&record) {
                eprintln!("Failed to persist worker {}: {err}", snapshot.name);
            }
        }
    }
}

struct WorkerRuntime {
    state: Arc<Mutex<WorkerSnapshot>>,
    cancel_flag: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
    worktree_path: PathBuf,
    branch: String,
    workflow: Workflow,
    completed_steps: Arc<AtomicUsize>,
    logs: Arc<Mutex<VecDeque<String>>>,
    cmd_tx: Sender<WorkerCommand>,
}

impl WorkerRuntime {
    fn new(
        snapshot: WorkerSnapshot,
        worktree_path: PathBuf,
        branch: String,
        workflow: Workflow,
        cmd_tx: Sender<WorkerCommand>,
    ) -> Result<Self> {
        Ok(Self {
            state: Arc::new(Mutex::new(snapshot)),
            cancel_flag: Arc::new(AtomicBool::new(false)),
            handle: None,
            worktree_path,
            branch,
            workflow,
            completed_steps: Arc::new(AtomicUsize::new(0)),
            logs: Arc::new(Mutex::new(VecDeque::new())),
            cmd_tx,
        })
    }

    fn snapshot(&self) -> WorkerSnapshot {
        self.state.lock().expect("worker snapshot poisoned").clone()
    }

    fn add_log(&self, line: String) {
        if let Ok(mut logs) = self.logs.lock() {
            const MAX_LOGS: usize = 1000;
            if logs.len() >= MAX_LOGS {
                logs.pop_front();
            }
            logs.push_back(line);
        }
    }

    fn get_logs(&self) -> Vec<String> {
        self.logs
            .lock()
            .map(|logs| logs.iter().cloned().collect())
            .unwrap_or_default()
    }

    fn start_agent(&mut self, evt_tx: &Sender<WorkerEvent>) {
        self.stop_agent();

        let state = Arc::clone(&self.state);
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel_flag = Arc::clone(&cancel);
        let evt_tx = evt_tx.clone();
        let worktree_path = self.worktree_path.clone();
        let workflow = self.workflow.clone();
        let completed_steps = Arc::clone(&self.completed_steps);
        let logs = Arc::clone(&self.logs);
        let cmd_tx = self.cmd_tx.clone();

        let handle = thread::Builder::new()
            .name(format!("gensui-agent-{}", self.snapshot().name))
            .spawn(move || {
                agent_simulation(
                    state,
                    cancel,
                    worktree_path,
                    evt_tx,
                    workflow,
                    completed_steps,
                    logs,
                    cmd_tx,
                )
            })
            .expect("failed to spawn agent simulation");

        self.handle = Some(handle);
    }

    fn stop_agent(&mut self) {
        self.cancel_flag.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn agent_simulation(
    state: Arc<Mutex<WorkerSnapshot>>,
    cancel: Arc<AtomicBool>,
    worktree_path: PathBuf,
    evt_tx: Sender<WorkerEvent>,
    workflow: Workflow,
    completed_steps: Arc<AtomicUsize>,
    logs: Arc<Mutex<VecDeque<String>>>,
    cmd_tx: Sender<WorkerCommand>,
) {
    // Helper function to save log and send event
    let send_log = |line: String, worker_id: WorkerId| {
        // Add to logs
        if let Ok(mut log_queue) = logs.lock() {
            const MAX_LOGS: usize = 1000;
            if log_queue.len() >= MAX_LOGS {
                log_queue.pop_front();
            }
            log_queue.push_back(line.clone());
        }

        // Send event to UI
        let _ = evt_tx.send(WorkerEvent::Log {
            id: worker_id,
            line,
        });
    };

    let worker_id = {
        state
            .lock()
            .map(|snapshot| snapshot.id)
            .unwrap_or(WorkerId(0))
    };

    let total_steps = workflow.steps().len();

    if total_steps == 0 {
        if let Ok(mut snapshot) = state.lock() {
            snapshot.status = WorkerStatus::Idle;
            snapshot.last_event = "No workflow steps defined".into();
            snapshot.current_step = None;
            let _ = evt_tx.send(WorkerEvent::Updated(snapshot.clone()));
        }
        return;
    }

    let start_step = completed_steps.load(Ordering::SeqCst);

    if start_step == 0 {
        if let Ok(mut snapshot) = state.lock() {
            snapshot.status = WorkerStatus::Running;
            snapshot.last_event = format!("ワークフロー '{}' を開始", workflow.name);
            snapshot.current_step = None;
            let _ = evt_tx.send(WorkerEvent::Updated(snapshot.clone()));
        }
    }

    if cancel.load(Ordering::SeqCst) {
        return;
    }

    for (idx, step) in workflow.steps().iter().enumerate() {
        // Skip already completed steps
        if idx < start_step {
            continue;
        }

        if cancel.load(Ordering::SeqCst) {
            if let Ok(mut snapshot) = state.lock() {
                snapshot.status = WorkerStatus::Paused;
                snapshot.last_event = "ワーカーがキャンセルされました".into();
                snapshot.current_step = None;
                let _ = evt_tx.send(WorkerEvent::Updated(snapshot.clone()));
            }
            return;
        }

        let step_desc = step
            .description
            .clone()
            .unwrap_or_else(|| "ステップを実行".to_string());

        let snapshot_info = if let Ok(mut snapshot) = state.lock() {
            snapshot.status = WorkerStatus::Running;
            snapshot.current_step = Some(format!("{}/{}: {}", idx + 1, total_steps, step.name));
            snapshot.last_event = step_desc.clone();
            let cloned = snapshot.clone();
            let _ = evt_tx.send(WorkerEvent::Updated(snapshot.clone()));
            cloned
        } else {
            WorkerSnapshot {
                id: worker_id,
                name: "unknown".into(),
                issue: None,
                agent: "Claude".into(),
                worktree: worktree_path.to_string_lossy().into_owned(),
                branch: "".into(),
                status: WorkerStatus::Running,
                last_event: step_desc.clone(),
                workflow: workflow.name.clone(),
                total_steps,
                current_step: Some(format!("{}/{}: {}", idx + 1, total_steps, step.name)),
                session_id: None,
            }
        };

        // Step start marker
        send_log(format!("[STEP_START:{}:{}]", idx, step.name), worker_id);
        send_log(format!("[{}] {}", step.name, step_desc), worker_id);

        let result = if let Some(claude_cfg) = &step.claude {
            let request_id = NEXT_PERMISSION_REQUEST_ID.fetch_add(1, Ordering::SeqCst);
            let permission_request = PermissionRequest {
                request_id,
                step_name: step.name.clone(),
                description: step.description.clone(),
                permission_mode: claude_cfg.permission_mode.clone(),
                allowed_tools: claude_cfg.allowed_tools.clone(),
            };

            let (perm_tx, perm_rx) = mpsc::channel();
            if let Err(err) = cmd_tx.send(WorkerCommand::PermissionPrompt {
                id: worker_id,
                request: permission_request.clone(),
                respond_to: perm_tx,
            }) {
                send_log(
                    format!("権限要求をエンキューできませんでした: {err}"),
                    worker_id,
                );
                if let Ok(mut snapshot) = state.lock() {
                    snapshot.status = WorkerStatus::Failed;
                    snapshot.last_event = "権限要求の送信に失敗しました".into();
                    snapshot.current_step = None;
                    let _ = evt_tx.send(WorkerEvent::Updated(snapshot.clone()));
                }
                return;
            }

            if let Ok(mut snapshot) = state.lock() {
                snapshot.last_event = format!("ステップ '{}' の権限確認を待機中", step.name);
                let _ = evt_tx.send(WorkerEvent::Updated(snapshot.clone()));
            }

            let tools_label = describe_allowed_tools(permission_request.allowed_tools.as_ref());

            send_log(
                format!(
                    "権限確認待ち (request #{}, tools: {})",
                    permission_request.request_id, tools_label
                ),
                worker_id,
            );

            let (effective_permission_mode, effective_allowed_tools) = match perm_rx.recv() {
                Ok(PermissionDecision::Allow {
                    permission_mode,
                    allowed_tools,
                }) => {
                    send_log("権限が承認されました".to_string(), worker_id);
                    (permission_mode, allowed_tools)
                }
                Ok(PermissionDecision::Deny) => {
                    send_log(
                        "権限が拒否されました。ステップを中断します".to_string(),
                        worker_id,
                    );
                    if let Ok(mut snapshot) = state.lock() {
                        snapshot.status = WorkerStatus::Paused;
                        snapshot.last_event =
                            format!("ステップ '{}' の権限が拒否されました", step.name);
                        snapshot.current_step = None;
                        let _ = evt_tx.send(WorkerEvent::Updated(snapshot.clone()));
                    }
                    return;
                }
                Err(_err) => {
                    send_log(
                        "権限確認中に内部エラーが発生したため中断します".to_string(),
                        worker_id,
                    );
                    if let Ok(mut snapshot) = state.lock() {
                        snapshot.status = WorkerStatus::Failed;
                        snapshot.last_event = "権限確認中にエラーが発生".into();
                        snapshot.current_step = None;
                        let _ = evt_tx.send(WorkerEvent::Updated(snapshot.clone()));
                    }
                    return;
                }
            };

            let prompt = render_prompt(&claude_cfg.prompt, &snapshot_info);

            // Prompt section
            send_log("[PROMPT_START]".to_string(), worker_id);
            send_log("─── Prompt ───".to_string(), worker_id);
            for line in prompt.lines() {
                send_log(line.to_string(), worker_id);
            }
            send_log("[PROMPT_END]".to_string(), worker_id);

            // Command details section
            send_log("─── Claude Code コマンド ───".to_string(), worker_id);

            // Permission Mode
            let effective_mode = claude_cfg
                .permission_mode
                .as_deref()
                .unwrap_or("bypassPermissions");
            let permission_mode_str = match effective_mode {
                "plan" => "プランモード (plan)".to_string(),
                "acceptEdits" => "編集承認モード (acceptEdits)".to_string(),
                "bypassPermissions" => "制限なしモード (bypassPermissions)".to_string(),
                other => format!("{}", other),
            };
            send_log(
                format!("Permission Mode: {}", permission_mode_str),
                worker_id,
            );

            // Override claude_cfg with user-selected permissions
            let mut claude_cfg_with_permissions = claude_cfg.clone();
            claude_cfg_with_permissions.permission_mode = effective_permission_mode.or_else(|| claude_cfg.permission_mode.clone());
            claude_cfg_with_permissions.allowed_tools = effective_allowed_tools.or_else(|| claude_cfg.allowed_tools.clone());

            // Model
            let model_str = claude_cfg_with_permissions.model.as_deref().unwrap_or("デフォルト");
            send_log(format!("Model: {}", model_str), worker_id);

            // Allowed Tools
            let tools_str = if let Some(tools) = &claude_cfg_with_permissions.allowed_tools {
                if tools.is_empty() {
                    "なし".to_string()
                } else {
                    tools.join(", ")
                }
            } else {
                "全て".to_string()
            };
            send_log(format!("Allowed Tools: {}", tools_str), worker_id);

            // Session
            let current_session_id = snapshot_info.session_id.as_deref();
            let session_str = if current_session_id.is_some() {
                "継続"
            } else {
                "新規"
            };
            send_log(format!("Session: {}", session_str), worker_id);

            // Extra Args
            if let Some(extra) = &claude_cfg_with_permissions.extra_args {
                if !extra.is_empty() {
                    send_log(format!("Extra Args: {}", extra.join(" ")), worker_id);
                }
            }

            // Pass current session_id to continue the session
            let result = run_claude_command(
                &claude_cfg_with_permissions,
                &prompt,
                &worktree_path,
                current_session_id,
                |line| send_log(line, worker_id),
            );

            // Store the session_id back into the snapshot
            if let Ok(Some(new_session_id)) = &result {
                if let Ok(mut snapshot) = state.lock() {
                    snapshot.session_id = Some(new_session_id.clone());
                    let _ = evt_tx.send(WorkerEvent::Updated(snapshot.clone()));
                }
            }

            result.map(|_| vec![])
        } else if let Some(command) = &step.command {
            send_log(format!("$ {}", command), worker_id);
            run_shell_command(command, &worktree_path)
        } else {
            Ok(vec!["(no-op step)".into()])
        };

        // Result section
        send_log("[RESULT_START]".to_string(), worker_id);

        match result {
            Ok(lines) => {
                for line in lines {
                    send_log(line, worker_id);
                }
                send_log("[RESULT_END]".to_string(), worker_id);
                // Step end marker (success)
                send_log("[STEP_END:Success]".to_string(), worker_id);

                // Increment completed steps
                completed_steps.fetch_add(1, Ordering::SeqCst);

                // Persist the worker state to disk
                let _ = cmd_tx.send(WorkerCommand::Persist { id: worker_id });
            }
            Err(err) => {
                send_log(format!("Error: {err}"), worker_id);
                send_log("[RESULT_END]".to_string(), worker_id);
                // Step end marker (failed)
                send_log("[STEP_END:Failed]".to_string(), worker_id);

                if let Ok(mut snapshot) = state.lock() {
                    snapshot.status = WorkerStatus::Failed;
                    snapshot.last_event = format!("Command failed: {err}");
                    snapshot.current_step =
                        Some(format!("{}/{}: {}", idx + 1, total_steps, step.name));
                    let _ = evt_tx.send(WorkerEvent::Updated(snapshot.clone()));
                }
                let _ = evt_tx.send(WorkerEvent::Error {
                    id: Some(worker_id),
                    message: err.to_string(),
                });
                return;
            }
        }

        thread::sleep(Duration::from_millis(400));
    }

    if let Ok(mut snapshot) = state.lock() {
        snapshot.status = WorkerStatus::Idle;
        snapshot.last_event = format!("ワークフロー '{}' が完了", workflow.name);
        snapshot.current_step = None;
        let _ = evt_tx.send(WorkerEvent::Updated(snapshot.clone()));
    }

    send_log(format!("Workflow '{}' completed", workflow.name), worker_id);
}

fn run_shell_command(command: &str, dir: &Path) -> Result<Vec<String>> {
    let output = Command::new("bash")
        .arg("-lc")
        .arg(command)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .with_context(|| format!("failed to execute shell command '{command}'"))?;

    let mut lines = Vec::new();

    // Process stdout
    let stdout_str = String::from_utf8_lossy(&output.stdout);
    for line in stdout_str.lines() {
        if !line.trim().is_empty() {
            lines.push(line.to_string());
        }
    }

    // Process stderr
    let stderr_str = String::from_utf8_lossy(&output.stderr);
    for line in stderr_str.lines() {
        if !line.trim().is_empty() {
            lines.push(line.to_string());
        }
    }

    if !output.status.success() {
        return Err(anyhow!(
            "command '{command}' exited with status {}",
            output.status
        ));
    }

    Ok(lines)
}

fn run_claude_command<F>(
    step: &ClaudeStep,
    prompt: &str,
    dir: &Path,
    session_id: Option<&str>,
    mut log_fn: F,
) -> Result<Option<String>>
where
    F: FnMut(String),
{
    let binary = env::var("GENSUI_CLAUDE_BIN").unwrap_or_else(|_| {
        // Try to find claude in common locations
        let home = env::var("HOME").unwrap_or_else(|_| ".".to_string());
        let local_claude = format!("{}/.claude/local/claude", home);
        if Path::new(&local_claude).exists() {
            local_claude
        } else {
            "claude".to_string()
        }
    });
    let mut cmd = Command::new(&binary);

    // Only set custom CLAUDE_CONFIG_DIR if GENSUI_CLAUDE_HOME is explicitly set
    // Otherwise, use the user's default Claude configuration (including API keys)
    if let Ok(custom_home) = env::var("GENSUI_CLAUDE_HOME") {
        let claude_home = PathBuf::from(custom_home);
        fs::create_dir_all(&claude_home).with_context(|| {
            format!(
                "failed to prepare Claude config directory at {}",
                claude_home.display()
            )
        })?;
        cmd.env("CLAUDE_CONFIG_DIR", &claude_home);
    }
    // Otherwise, don't set HOME or CLAUDE_CONFIG_DIR - use user's default settings

    cmd.arg("--print").arg(prompt);
    cmd.arg("--output-format").arg("stream-json");
    cmd.arg("--verbose");

    // Continue existing session if session_id is provided
    if session_id.is_some() {
        cmd.arg("--continue");
    }

    if let Some(model) = &step.model {
        cmd.arg("--model").arg(model);
    }

    // If permission_mode is not set, use bypassPermissions by default
    // This allows Claude to execute tools freely after user approves the step
    let effective_mode = step
        .permission_mode
        .as_deref()
        .unwrap_or("bypassPermissions");
    cmd.arg("--permission-mode").arg(effective_mode);

    if let Some(tools) = &step.allowed_tools {
        if !tools.is_empty() {
            cmd.arg("--allowedTools").arg(tools.join(","));
        }
    }

    if let Some(extra) = &step.extra_args {
        for arg in extra {
            let replaced = arg
                .replace("{{prompt}}", prompt)
                .replace("{{workdir}}", &dir.to_string_lossy());
            cmd.arg(replaced);
        }
    }

    cmd.current_dir(dir);

    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| "failed to spawn Claude Code process")?;

    let mut extracted_session_id: Option<String> = None;
    let mut stderr_lines = Vec::new();

    // Capture stderr in a separate thread
    let stderr_handle = child.stderr.take().map(|stderr| {
        thread::spawn(move || {
            let reader = BufReader::new(stderr);
            let mut lines = Vec::new();
            for line in reader.lines().flatten() {
                lines.push(line);
            }
            lines
        })
    });

    // Stream stdout in real-time
    if let Some(stdout) = child.stdout.take() {
        let reader = BufReader::new(stdout);
        for line in reader.lines() {
            let line = match line {
                Ok(l) => l,
                Err(e) => {
                    log_fn(format!("Error reading stdout: {}", e));
                    continue;
                }
            };

            if line.trim().is_empty() {
                continue;
            }

            // Try to parse as JSON
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&line) {
                // Extract thinking/analysis
                let thought_lines = extract_thinking_lines(&json);
                if !thought_lines.is_empty() {
                    log_fn("[THOUGHT_START]".to_string());
                    for thought_line in thought_lines {
                        log_fn(thought_line);
                    }
                    log_fn("[THOUGHT_END]".to_string());
                }

                match json.get("type").and_then(|v| v.as_str()) {
                    Some("system") => {
                        // Session initialization - extract session_id but don't log
                        if let Some(sid) = json.get("session_id").and_then(|v| v.as_str()) {
                            extracted_session_id = Some(sid.to_string());
                        }
                    }
                    Some("result") => {
                        // Check for errors first
                        if let Some(true) = json.get("is_error").and_then(|v| v.as_bool()) {
                            log_fn("⚠️  Claude encountered an error".to_string());
                        }

                        // Final result
                        if let Some(result_text) = json.get("result").and_then(|v| v.as_str()) {
                            if !result_text.is_empty() {
                                log_fn("─── Result ───".to_string());
                                for result_line in result_text.lines() {
                                    log_fn(result_line.to_string());
                                }
                            }
                        }
                    }
                    Some("error") => {
                        // API error response
                        log_fn("❌ API Error:".to_string());
                        if let Some(error_obj) = json.get("error") {
                            if let Some(message) = error_obj.get("message").and_then(|v| v.as_str()) {
                                log_fn(format!("  {}", message));
                            }
                        }
                    }
                    Some("assistant") => {
                        // Log assistant messages
                        if let Some(text) = json.get("text").and_then(|v| v.as_str()) {
                            if !text.trim().is_empty() {
                                log_fn(format!("💬 {}", text));
                            }
                        }
                    }
                    Some("tool_use") => {
                        // Log tool usage
                        if let Some(tool_name) = json.get("name").and_then(|v| v.as_str()) {
                            log_fn(format!("🔧 Using tool: {}", tool_name));
                        }
                    }
                    _ => {
                        // Log raw JSON for other event types for debugging
                        // log_fn(format!("🔍 JSON: {}", line));
                    }
                }
            } else {
                // Non-JSON output
                log_fn(line);
            }
        }
    }

    // Wait for process to complete
    let status = child.wait().with_context(|| "failed to wait for Claude Code process")?;

    // Collect stderr from thread
    if let Some(handle) = stderr_handle {
        if let Ok(lines) = handle.join() {
            stderr_lines = lines;
        }
    }

    // Log stderr output
    if !stderr_lines.is_empty() {
        log_fn("─── stderr ───".to_string());
        for line in &stderr_lines {
            if !line.trim().is_empty() {
                log_fn(line.clone());
            }
        }
    }

    if !status.success() {
        let mut error_msg = format!("Claude CLI exited with status {}", status);
        if !stderr_lines.is_empty() {
            error_msg.push_str("\nstderr:\n");
            error_msg.push_str(&stderr_lines.join("\n"));
        }
        return Err(anyhow!("{}", error_msg));
    }

    Ok(extracted_session_id)
}

fn extract_thinking_lines(json: &serde_json::Value) -> Vec<String> {
    fn walk(value: &serde_json::Value, acc: &mut Vec<String>, in_thinking: bool) {
        match value {
            serde_json::Value::String(text) => {
                if in_thinking {
                    for line in text.lines() {
                        let trimmed = line.trim_end();
                        if trimmed.is_empty() {
                            continue;
                        }
                        acc.push(trimmed.to_string());
                    }
                }
            }
            serde_json::Value::Array(items) => {
                for item in items {
                    walk(item, acc, in_thinking);
                }
            }
            serde_json::Value::Object(map) => {
                let mut next_flag = in_thinking;
                if let Some(ty) = map.get("type").and_then(|v| v.as_str()) {
                    let ty_lc = ty.to_ascii_lowercase();
                    if matches!(
                        ty_lc.as_str(),
                        "thinking" | "analysis" | "plan" | "reasoning"
                    ) {
                        next_flag = true;
                    }
                }

                if let Some(thinking) = map.get("thinking") {
                    walk(thinking, acc, true);
                }

                if let Some(text) = map.get("text") {
                    walk(text, acc, next_flag);
                }

                if let Some(content) = map.get("content") {
                    walk(content, acc, next_flag);
                }

                if let Some(message) = map.get("message") {
                    walk(message, acc, next_flag);
                }

                for (key, value) in map {
                    if matches!(key.as_str(), "thinking" | "analysis" | "reasoning" | "plan") {
                        walk(value, acc, true);
                    } else if !matches!(key.as_str(), "text" | "content" | "message") {
                        walk(value, acc, next_flag);
                    }
                }
            }
            _ => {}
        }
    }

    let mut acc = Vec::new();
    walk(json, &mut acc, false);
    acc
}

fn render_prompt(template: &str, snapshot: &WorkerSnapshot) -> String {
    template
        .replace(
            "{{issue}}",
            snapshot.issue.as_deref().unwrap_or("(no issue)"),
        )
        .replace("{{worker}}", snapshot.name.as_str())
        .replace("{{branch}}", snapshot.branch.as_str())
        .replace("{{worktree}}", snapshot.worktree.as_str())
}

fn describe_allowed_tools(tools: Option<&Vec<String>>) -> String {
    match tools {
        None => "制限なし".to_string(),
        Some(list) if list.is_empty() => "なし".to_string(),
        Some(list) => list.join(", "),
    }
}

fn determine_base_ref(repo_root: &Path) -> Option<String> {
    let output = Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(repo_root)
        .stderr(Stdio::null())
        .stdout(Stdio::piped())
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if branch.is_empty() || branch == "HEAD" {
        None
    } else {
        Some(branch)
    }
}

pub fn list_existing_worktrees(repo_root: &Path) -> Result<Vec<ExistingWorktree>> {
    let output = Command::new("git")
        .args(["worktree", "list", "--porcelain"])
        .current_dir(repo_root)
        .stderr(Stdio::piped())
        .stdout(Stdio::piped())
        .output()
        .context("failed to execute git worktree list")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!("git worktree list failed: {}", stderr.trim()));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut worktrees = Vec::new();
    let mut current_path: Option<PathBuf> = None;
    let mut current_branch: Option<String> = None;

    for line in stdout.lines() {
        if line.starts_with("worktree ") {
            // Save previous worktree if complete
            if let (Some(path), Some(branch)) = (current_path.take(), current_branch.take()) {
                worktrees.push(ExistingWorktree { path, branch });
            }
            // Start new worktree
            current_path = Some(PathBuf::from(line.trim_start_matches("worktree ").trim()));
        } else if line.starts_with("branch ") {
            let branch_ref = line.trim_start_matches("branch ").trim();
            // Extract branch name from refs/heads/branch-name
            if let Some(branch_name) = branch_ref.strip_prefix("refs/heads/") {
                current_branch = Some(branch_name.to_string());
            }
        } else if line.starts_with("detached") {
            // For detached HEAD, use a placeholder
            current_branch = Some("(detached HEAD)".to_string());
        }
    }

    // Save last worktree
    if let (Some(path), Some(branch)) = (current_path, current_branch) {
        worktrees.push(ExistingWorktree { path, branch });
    }

    Ok(worktrees)
}
