use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crate::config::{Config, Workflow};
use anyhow::{Context, Result, anyhow};
use time::OffsetDateTime;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct WorkerId(pub usize);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
}

#[derive(Clone, Debug)]
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
}

#[derive(Default, Clone, Debug)]
pub struct CreateWorkerRequest {
    pub issue: Option<String>,
    pub agent: Option<String>,
    pub workflow: Option<String>,
}

pub enum WorkerCommand {
    Create(CreateWorkerRequest),
    Delete { id: WorkerId },
    Restart { id: WorkerId },
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
}

pub type WorkerEventReceiver = Receiver<WorkerEvent>;

pub fn spawn_worker_system(
    repo_root: PathBuf,
    config: Config,
) -> Result<(WorkerHandle, WorkerEventReceiver)> {
    let (cmd_tx, cmd_rx) = mpsc::channel();
    let (evt_tx, evt_rx) = mpsc::channel();
    let manager = WorkerManager::new(repo_root, config, cmd_rx, evt_tx.clone());

    thread::Builder::new()
        .name("gensui-worker-manager".into())
        .spawn(move || manager.run())
        .context("failed to spawn worker manager thread")?;

    Ok((WorkerHandle { cmd_tx }, evt_rx))
}

struct WorkerManager {
    repo_root: PathBuf,
    config: Config,
    cmd_rx: Receiver<WorkerCommand>,
    evt_tx: Sender<WorkerEvent>,
    next_id: usize,
    workers: HashMap<WorkerId, WorkerRuntime>,
}

impl WorkerManager {
    fn new(
        repo_root: PathBuf,
        config: Config,
        cmd_rx: Receiver<WorkerCommand>,
        evt_tx: Sender<WorkerEvent>,
    ) -> Self {
        Self {
            repo_root,
            config,
            cmd_rx,
            evt_tx,
            next_id: 1,
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
            }
        }

        self.shutdown_all();
    }

    fn handle_create(&mut self, request: CreateWorkerRequest) -> Result<()> {
        let worker_id = WorkerId(self.next_id);
        self.next_id += 1;

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

        let workflow = request
            .workflow
            .as_deref()
            .and_then(|name| self.config.workflow_by_name(name))
            .cloned()
            .unwrap_or_else(|| self.config.default_workflow().clone());

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

    fn shutdown_all(&mut self) {
        for (_, mut runtime) in self.workers.drain() {
            runtime.stop_agent();
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

        if let Ok(mut snapshot) = state.lock() {
            snapshot.status = WorkerStatus::Running;
            snapshot.current_step = Some(format!("{}/{}: {}", idx + 1, total_steps, step.name));
            snapshot.last_event = step_desc.clone();
            let _ = evt_tx.send(WorkerEvent::Updated(snapshot.clone()));
        }

        let _ = evt_tx.send(WorkerEvent::Log {
            id: worker_id,
            line: format!("[{}] {}", step.name, step_desc),
        });

        let _ = evt_tx.send(WorkerEvent::Log {
            id: worker_id,
            line: format!("$ {}", step.command),
        });

        match run_shell_command(&step.command, &worktree_path) {
            Ok(lines) => {
                for line in lines {
                    let _ = evt_tx.send(WorkerEvent::Log {
                        id: worker_id,
                        line: format!("$ {}", line),
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
    let mut child = Command::new("bash")
        .arg("-lc")
        .arg(command)
        .current_dir(dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to spawn shell command '{command}'"))?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("failed to capture stdout"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow!("failed to capture stderr"))?;

    let mut lines = Vec::new();

    let mut read_pipe = |reader: &mut dyn BufRead| -> Result<()> {
        for line in reader.lines() {
            let line = line?;
            if !line.trim().is_empty() {
                lines.push(line);
            }
        }
        Ok(())
    };

    let mut stdout_reader = BufReader::new(stdout);
    read_pipe(&mut stdout_reader)?;

    let mut stderr_reader = BufReader::new(stderr);
    read_pipe(&mut stderr_reader)?;

    let status = child.wait()?;
    if !status.success() {
        return Err(anyhow!("command '{command}' exited with status {status}"));
    }

    Ok(lines)
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
