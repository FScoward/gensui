use std::collections::VecDeque;

use crate::log_parser;
use crate::state::SessionHistory;
use crate::ui::LogEntry;
use crate::worker::WorkerSnapshot;

/// View model for a worker, including logs and structured data
pub struct WorkerView {
    pub snapshot: WorkerSnapshot,
    pub logs: VecDeque<String>,
    pub structured_logs: Vec<LogEntry>,
    pub session_histories: Vec<SessionHistory>,
    // Parser
    log_parser: log_parser::LogParser,
}

impl WorkerView {
    const LOG_CAPACITY: usize = 128;

    pub fn new(snapshot: WorkerSnapshot) -> Self {
        Self {
            snapshot,
            logs: VecDeque::with_capacity(Self::LOG_CAPACITY),
            structured_logs: Vec::new(),
            session_histories: Vec::new(),
            log_parser: log_parser::LogParser::new(),
        }
    }

    pub fn set_session_histories(&mut self, histories: Vec<SessionHistory>) {
        self.session_histories = histories;
    }

    pub fn update_snapshot(&mut self, snapshot: WorkerSnapshot) {
        self.snapshot = snapshot;
    }

    pub fn push_log(&mut self, line: String) {
        if self.logs.len() >= Self::LOG_CAPACITY {
            self.logs.pop_front();
        }
        self.logs.push_back(line.clone());

        // Parse structured log markers using log_parser
        if let Some(entry) = self.log_parser.parse_line(&line) {
            self.structured_logs.push(entry);
        }
    }
}
