use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};

use crate::config::Workflow;

#[derive(Debug, Clone)]
pub struct StateStore {
    base: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ManagerState {
    pub next_id: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerRecord {
    pub snapshot: WorkerSnapshotData,
    pub logs: Vec<String>,
    pub workflow: Workflow,
    pub completed_steps: usize,
    #[serde(default)]
    pub session_history: Vec<SessionHistory>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerSnapshotData {
    pub id: usize,
    pub name: String,
    pub issue: Option<String>,
    pub agent: String,
    pub worktree: String,
    pub branch: String,
    pub status: String,
    pub last_event: String,
    pub workflow: String,
    pub total_steps: usize,
    pub current_step: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionLogEntry {
    pub timestamp: String,
    pub message: String,
    pub worker: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionHistory {
    pub session_id: String,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub prompt: String,
    pub events: Vec<SessionEvent>,
    pub total_tool_uses: usize,
    pub files_modified: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SessionEvent {
    ToolUse {
        name: String,
        timestamp: String,
        input: Option<serde_json::Value>,
    },
    ToolResult {
        name: String,
        timestamp: String,
        output: Option<String>,
    },
    AssistantMessage {
        text: String,
        timestamp: String,
    },
    ThinkingBlock {
        content: String,
        timestamp: String,
    },
    Result {
        text: String,
        is_error: bool,
        timestamp: String,
    },
    Error {
        message: String,
        timestamp: String,
    },
}

impl StateStore {
    pub fn new(base: PathBuf) -> Result<Self> {
        fs::create_dir_all(&base)
            .with_context(|| format!("failed to create state dir {}", base.display()))?;
        fs::create_dir_all(base.join("workers")).with_context(|| {
            format!(
                "failed to create workers state dir under {}",
                base.display()
            )
        })?;
        Ok(Self { base })
    }

    pub fn load_manager(&self) -> Result<Option<ManagerState>> {
        let path = self.manager_state_path();
        if !path.exists() {
            return Ok(None);
        }
        let file = File::open(&path)
            .with_context(|| format!("failed to open manager state {}", path.display()))?;
        let state = serde_json::from_reader(file)
            .with_context(|| format!("failed to parse manager state {}", path.display()))?;
        Ok(Some(state))
    }

    pub fn save_manager(&self, state: &ManagerState) -> Result<()> {
        let path = self.manager_state_path();
        let tmp = path.with_extension("json.tmp");
        let file = File::create(&tmp)
            .with_context(|| format!("failed to open temp manager state {}", tmp.display()))?;
        serde_json::to_writer_pretty(file, state)
            .with_context(|| format!("failed to serialize manager state {}", tmp.display()))?;
        fs::rename(&tmp, &path)
            .with_context(|| format!("failed to persist manager state {}", path.display()))?;
        Ok(())
    }

    pub fn load_workers(&self) -> Result<Vec<WorkerRecord>> {
        let mut records = Vec::new();
        let dir = self.workers_dir();
        if !dir.exists() {
            return Ok(records);
        }
        for entry in fs::read_dir(&dir)
            .with_context(|| format!("failed to read workers dir {}", dir.display()))?
        {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            let file = File::open(entry.path()).with_context(|| {
                format!("failed to open worker state {}", entry.path().display())
            })?;
            if let Ok(record) = serde_json::from_reader(file) {
                records.push(record);
            }
        }
        records.sort_by(|a, b| a.snapshot.name.cmp(&b.snapshot.name));
        Ok(records)
    }

    pub fn save_worker(&self, record: &WorkerRecord) -> Result<()> {
        let path = self.worker_path(&record.snapshot.name);
        let tmp = path.with_extension("json.tmp");
        let file = File::create(&tmp)
            .with_context(|| format!("failed to create worker state {}", tmp.display()))?;
        serde_json::to_writer_pretty(file, record)
            .with_context(|| format!("failed to serialize worker state {}", tmp.display()))?;
        fs::rename(&tmp, &path)
            .with_context(|| format!("failed to persist worker state {}", path.display()))?;
        Ok(())
    }

    pub fn delete_worker(&self, worker_name: &str) -> Result<()> {
        let path = self.worker_path(worker_name);
        if path.exists() {
            fs::remove_file(&path)
                .with_context(|| format!("failed to remove worker state {}", path.display()))?;
        }
        Ok(())
    }

    pub fn rename_worker(&self, old_name: &str, new_name: &str) -> Result<()> {
        let old_path = self.worker_path(old_name);
        let new_path = self.worker_path(new_name);

        if !old_path.exists() {
            return Err(anyhow!("Worker state file {} not found", old_path.display()));
        }

        if new_path.exists() {
            return Err(anyhow!("Worker state file {} already exists", new_path.display()));
        }

        fs::rename(&old_path, &new_path)
            .with_context(|| format!(
                "Failed to rename worker state from {} to {}",
                old_path.display(),
                new_path.display()
            ))?;

        Ok(())
    }

    pub fn append_action_log(&self, entry: &ActionLogEntry) -> Result<()> {
        let path = self.action_log_path();
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("failed to open action log {}", path.display()))?;
        serde_json::to_writer(&mut file, entry).with_context(|| {
            format!(
                "failed to serialize action log entry for {}",
                entry.timestamp
            )
        })?;
        file.write_all(b"\n")
            .with_context(|| format!("failed to append newline to {}", path.display()))?;
        Ok(())
    }

    pub fn load_action_log(&self, limit: usize) -> Result<Vec<ActionLogEntry>> {
        let path = self.action_log_path();
        if !path.exists() {
            return Ok(Vec::new());
        }
        let file = File::open(&path)
            .with_context(|| format!("failed to open action log {}", path.display()))?;
        let reader = BufReader::new(file);
        let mut entries = Vec::new();
        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            if let Ok(entry) = serde_json::from_str::<ActionLogEntry>(&line) {
                entries.push(entry);
            }
        }
        if entries.len() > limit {
            entries.drain(0..entries.len() - limit);
        }
        Ok(entries)
    }

    fn manager_state_path(&self) -> PathBuf {
        self.base.join("manager.json")
    }

    fn action_log_path(&self) -> PathBuf {
        self.base.join("action_log.jsonl")
    }

    fn workers_dir(&self) -> PathBuf {
        self.base.join("workers")
    }

    fn worker_path(&self, worker_name: &str) -> PathBuf {
        self.workers_dir().join(format!("{}.json", worker_name))
    }
}
