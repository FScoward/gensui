use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
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
}

impl WorkerStatus {
    pub fn label(&self) -> &'static str {
        match self {
            WorkerStatus::Idle => "Idle",
            WorkerStatus::Running => "Running",
            WorkerStatus::Paused => "Paused",
            WorkerStatus::Failed => "Failed",
        }
    }

    pub fn from_label(label: &str) -> Self {
        match label {
            "Running" => WorkerStatus::Running,
            "Paused" => WorkerStatus::Paused,
            "Failed" => WorkerStatus::Failed,
            _ => WorkerStatus::Idle,
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
}

pub enum WorkerCommand {
    Create(CreateWorkerRequest),
    Delete { id: WorkerId },
    Restart { id: WorkerId },
    Continue { id: WorkerId, prompt: String },
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
}

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

    pub fn continue_worker(&self, id: WorkerId, prompt: String) -> Result<()> {
        self.cmd_tx
            .send(WorkerCommand::Continue { id, prompt })
            .map_err(|err| anyhow!("failed to enqueue worker continuation: {err}"))
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
    cmd_rx: Receiver<WorkerCommand>,
    evt_tx: Sender<WorkerEvent>,
    next_id: usize,
    workers: HashMap<WorkerId, WorkerRuntime>,
}

impl WorkerManager {
    fn new(
        repo_root: PathBuf,
        config: Config,
        state_store: StateStore,
        cmd_rx: Receiver<WorkerCommand>,
        evt_tx: Sender<WorkerEvent>,
        next_id: usize,
    ) -> Self {
        Self {
            repo_root,
            config,
            state_store,
            cmd_rx,
            evt_tx,
            next_id,
            workers: HashMap::new(),
        }
    }

    fn run(mut self) {
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
                WorkerCommand::Continue { id, prompt } => {
                    if let Err(err) = self.handle_continue(id, prompt) {
                        let _ = self.evt_tx.send(WorkerEvent::Error {
                            id: Some(id),
                            message: err.to_string(),
                        });
                    }
                }
            }
        }

        self.shutdown_all();
    }

    fn handle_create(&mut self, request: CreateWorkerRequest) -> Result<()> {
        let worker_id = WorkerId(self.next_id);
        self.next_id += 1;
        self.persist_manager_state();

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

        let workflow = if let Some(prompt) = request.free_prompt.clone() {
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
                        permission_mode: Some("plan".to_string()),
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

        let runtime = WorkerRuntime::new(snapshot, worktree_path, branch, workflow);
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

        Ok(())
    }

    fn handle_restart(&mut self, id: WorkerId) -> Result<()> {
        let runtime = self
            .workers
            .get_mut(&id)
            .ok_or_else(|| anyhow!("worker {:?} not found", id))?;

        runtime.stop_agent();

        {
            let mut snapshot = runtime.state.lock().expect("worker snapshot poisoned");
            snapshot.status = WorkerStatus::Running;
            snapshot.last_event = "Restart requested".into();
            snapshot.current_step = None;
            let _ = self.evt_tx.send(WorkerEvent::Updated(snapshot.clone()));
        }

        runtime.start_agent(&self.evt_tx);

        Ok(())
    }

    fn handle_continue(&mut self, id: WorkerId, prompt: String) -> Result<()> {
        let runtime = self
            .workers
            .get_mut(&id)
            .ok_or_else(|| anyhow!("worker {:?} not found", id))?;

        // Stop current agent execution
        runtime.stop_agent();

        // Create a new workflow step with the continuation prompt
        let continue_step = WorkflowStep {
            name: "Continue".to_string(),
            command: None,
            claude: Some(ClaudeStep {
                prompt,
                model: None,
                allowed_tools: None,
                permission_mode: Some("plan".to_string()),
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

        Ok(())
    }

    fn shutdown_all(&mut self) {
        for (_, mut runtime) in self.workers.drain() {
            runtime.stop_agent();
        }
    }

    fn persist_manager_state(&self) {
        if let Err(err) = self.state_store.save_manager(&ManagerState {
            next_id: self.next_id,
        }) {
            eprintln!("Failed to persist manager state: {err}");
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
}

impl WorkerRuntime {
    fn new(
        snapshot: WorkerSnapshot,
        worktree_path: PathBuf,
        branch: String,
        workflow: Workflow,
    ) -> Result<Self> {
        Ok(Self {
            state: Arc::new(Mutex::new(snapshot)),
            cancel_flag: Arc::new(AtomicBool::new(false)),
            handle: None,
            worktree_path,
            branch,
            workflow,
        })
    }

    fn snapshot(&self) -> WorkerSnapshot {
        self.state.lock().expect("worker snapshot poisoned").clone()
    }

    fn start_agent(&mut self, evt_tx: &Sender<WorkerEvent>) {
        self.stop_agent();

        let state = Arc::clone(&self.state);
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel_flag = Arc::clone(&cancel);
        let evt_tx = evt_tx.clone();
        let worktree_path = self.worktree_path.clone();
        let workflow = self.workflow.clone();

        let handle = thread::Builder::new()
            .name(format!("gensui-agent-{}", self.snapshot().name))
            .spawn(move || agent_simulation(state, cancel, worktree_path, evt_tx, workflow))
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
) {
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

    if let Ok(mut snapshot) = state.lock() {
        snapshot.status = WorkerStatus::Running;
        snapshot.last_event = format!("ワークフロー '{}' を開始", workflow.name);
        snapshot.current_step = None;
        let _ = evt_tx.send(WorkerEvent::Updated(snapshot.clone()));
    }

    if cancel.load(Ordering::SeqCst) {
        return;
    }

    for (idx, step) in workflow.steps().iter().enumerate() {
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

        let _ = evt_tx.send(WorkerEvent::Log {
            id: worker_id,
            line: format!("[{}] {}", step.name, step_desc),
        });

        let result = if let Some(claude_cfg) = &step.claude {
            let prompt = render_prompt(&claude_cfg.prompt, &snapshot_info);

            // Show the actual prompt being sent
            let _ = evt_tx.send(WorkerEvent::Log {
                id: worker_id,
                line: "─── Prompt ───".to_string(),
            });
            // Split prompt into lines and show each line
            for line in prompt.lines().take(10) {
                let _ = evt_tx.send(WorkerEvent::Log {
                    id: worker_id,
                    line: format!("  {}", line),
                });
            }
            if prompt.lines().count() > 10 {
                let _ = evt_tx.send(WorkerEvent::Log {
                    id: worker_id,
                    line: format!("  ... ({} more lines)", prompt.lines().count() - 10),
                });
            }
            let _ = evt_tx.send(WorkerEvent::Log {
                id: worker_id,
                line: "─── Executing ───".to_string(),
            });

            // Log command details
            let mut cmd_parts = vec!["claude", "--print", "--output-format", "stream-json", "--verbose"];
            if let Some(model) = &claude_cfg.model {
                cmd_parts.push("--model");
                cmd_parts.push(model);
            }
            if let Some(mode) = &claude_cfg.permission_mode {
                cmd_parts.push("--permission-mode");
                cmd_parts.push(mode);
            }
            let _ = evt_tx.send(WorkerEvent::Log {
                id: worker_id,
                line: format!("<Claude> {}", cmd_parts.join(" ")),
            });

            // Pass current session_id to continue the session
            let current_session_id = snapshot_info.session_id.as_deref();
            run_claude_command(claude_cfg, &prompt, &worktree_path, current_session_id)
                .map(|(lines, new_session_id)| {
                    // Store the session_id back into the snapshot
                    if let Some(sid) = new_session_id {
                        if let Ok(mut snapshot) = state.lock() {
                            snapshot.session_id = Some(sid);
                            let _ = evt_tx.send(WorkerEvent::Updated(snapshot.clone()));
                        }
                    }
                    lines
                })
        } else if let Some(command) = &step.command {
            let _ = evt_tx.send(WorkerEvent::Log {
                id: worker_id,
                line: format!("$ {}", command),
            });
            run_shell_command(command, &worktree_path)
        } else {
            Ok(vec!["(no-op step)".into()])
        };

        match result {
            Ok(lines) => {
                for line in lines {
                    let _ = evt_tx.send(WorkerEvent::Log {
                        id: worker_id,
                        line,
                    });
                }
            }
            Err(err) => {
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

    let _ = evt_tx.send(WorkerEvent::Log {
        id: worker_id,
        line: format!("Workflow '{}' completed", workflow.name),
    });
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
        return Err(anyhow!("command '{command}' exited with status {}", output.status));
    }

    Ok(lines)
}

fn run_claude_command(
    step: &ClaudeStep,
    prompt: &str,
    dir: &Path,
    session_id: Option<&str>,
) -> Result<(Vec<String>, Option<String>)> {
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

    if let Some(mode) = &step.permission_mode {
        cmd.arg("--permission-mode").arg(mode);
    }

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

    let output = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .with_context(|| "failed to execute Claude Code process")?;

    let mut lines = Vec::new();
    let mut extracted_session_id: Option<String> = None;

    // Process stdout - parse JSON stream
    let stdout_str = String::from_utf8_lossy(&output.stdout);
    for line in stdout_str.lines() {
        if line.trim().is_empty() {
            continue;
        }

        // Try to parse as JSON
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(line) {
            match json.get("type").and_then(|v| v.as_str()) {
                Some("system") => {
                    // Session initialization
                    if let Some(sid) = json.get("session_id").and_then(|v| v.as_str()) {
                        extracted_session_id = Some(sid.to_string());
                        lines.push(format!("Session: {}", &sid[..8.min(sid.len())]));
                    }
                }
                Some("assistant") => {
                    // Extract text content from assistant message
                    if let Some(message) = json.get("message") {
                        if let Some(content_array) = message.get("content").and_then(|v| v.as_array()) {
                            for content_item in content_array {
                                match content_item.get("type").and_then(|v| v.as_str()) {
                                    Some("text") => {
                                        if let Some(text) = content_item.get("text").and_then(|v| v.as_str()) {
                                            // Check if this is an error message
                                            if text.contains("API Error:") || text.contains("\"type\":\"error\"") {
                                                lines.push("❌ Error encountered:".to_string());
                                            }
                                            // Split long text into multiple lines for readability
                                            for text_line in text.lines() {
                                                lines.push(text_line.to_string());
                                            }
                                        }
                                    }
                                    Some("tool_use") => {
                                        if let Some(name) = content_item.get("name").and_then(|v| v.as_str()) {
                                            lines.push(format!("🔧 Using tool: {}", name));
                                        }
                                    }
                                    Some("thinking") => {
                                        lines.push("💭 Thinking...".to_string());
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                }
                Some("result") => {
                    // Check for errors first
                    if let Some(true) = json.get("is_error").and_then(|v| v.as_bool()) {
                        lines.push("⚠️  Claude encountered an error".to_string());
                    }

                    // Final result
                    if let Some(result_text) = json.get("result").and_then(|v| v.as_str()) {
                        if !result_text.is_empty() {
                            lines.push("─── Result ───".to_string());
                            for result_line in result_text.lines() {
                                lines.push(result_line.to_string());
                            }
                        }
                    }

                    // Show cost information if available
                    if let Some(cost) = json.get("total_cost_usd").and_then(|v| v.as_f64()) {
                        lines.push(format!("💰 Cost: ${:.4}", cost));
                    }
                }
                Some("error") => {
                    // API error response
                    lines.push("❌ API Error:".to_string());
                    if let Some(error_obj) = json.get("error") {
                        if let Some(message) = error_obj.get("message").and_then(|v| v.as_str()) {
                            lines.push(format!("  {}", message));
                        }
                        if let Some(error_type) = error_obj.get("type").and_then(|v| v.as_str()) {
                            lines.push(format!("  Type: {}", error_type));
                        }
                    }
                }
                Some("tool_result") => {
                    // Tool execution result - could show this for debugging
                    lines.push("✓ Tool completed".to_string());
                }
                _ => {
                    // Unknown type - log for debugging
                    lines.push(format!("DEBUG: {}", line));
                }
            }
        } else {
            // Not JSON, treat as plain text
            lines.push(line.to_string());
        }
    }

    // Process stderr
    let stderr_str = String::from_utf8_lossy(&output.stderr);
    for line in stderr_str.lines() {
        if !line.trim().is_empty() {
            lines.push(format!("stderr: {}", line));
        }
    }

    if !output.status.success() {
        let stderr_lines: Vec<_> = lines.iter()
            .filter(|l| l.starts_with("stderr:"))
            .cloned()
            .collect();
        let other_lines: Vec<_> = lines.iter()
            .filter(|l| !l.starts_with("stderr:"))
            .cloned()
            .collect();

        let mut error_parts = vec![format!("Claude CLI exited with status {}", output.status)];

        let has_other = !other_lines.is_empty();
        let has_stderr = !stderr_lines.is_empty();

        if has_other {
            error_parts.push("Output:".to_string());
            error_parts.extend(other_lines);
        }

        if has_stderr {
            error_parts.push("Errors:".to_string());
            error_parts.extend(stderr_lines);
        }

        if !has_other && !has_stderr {
            error_parts.push("No output captured".to_string());
        }

        return Err(anyhow!("{}", error_parts.join("\n")));
    }

    Ok((lines, extracted_session_id))
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
