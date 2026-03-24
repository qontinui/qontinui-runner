//! Types for process capture and management.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Instant;

use crate::error_monitor::types::ParserType;

/// Configuration for a managed process.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessConfig {
    /// Unique identifier (UUID)
    pub id: String,
    /// Human-readable name (e.g., "FastAPI Backend")
    pub name: String,
    /// Command to execute (e.g., "python", "npm", "cargo")
    pub command: String,
    /// Command arguments (e.g., ["run", "dev"])
    #[serde(default)]
    pub args: Vec<String>,
    /// Working directory (absolute path)
    pub cwd: String,
    /// Extra environment variables
    #[serde(default)]
    pub env: HashMap<String, String>,
    /// Port to check for health (optional)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub health_port: Option<u16>,
    /// Parser type for error detection
    #[serde(default)]
    pub parser: ParserType,
    /// Start when runner launches
    #[serde(default)]
    pub auto_start: bool,
    /// Category (e.g., "backend", "frontend")
    #[serde(default = "default_category")]
    pub category: String,
    /// Ring buffer max lines (default 2000)
    #[serde(default = "default_buffer_size")]
    pub buffer_size: usize,
    /// Whether this config is enabled
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Regex patterns for errors to ignore (matched against error message and raw entry)
    #[serde(default)]
    pub ignore_patterns: Vec<String>,
    /// Startup group for ordered startup (lower groups start first, default 0).
    /// Processes in the same group start together. The runner waits for health ports
    /// in each group to be ready before starting the next group.
    #[serde(default)]
    pub start_group: u32,
    /// Whether this is a dev-mode-only service (not started in production builds)
    #[serde(default)]
    pub dev_only: bool,
    /// Whether rebuild and AI fix features are enabled for this process
    #[serde(default = "default_true")]
    pub rebuild_enabled: bool,
    /// Build command to run before restarting (e.g., "cargo", "npm")
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_command: Option<String>,
    /// Build command arguments (e.g., ["build"], ["run", "build"])
    #[serde(default)]
    pub build_args: Vec<String>,
}

impl ProcessConfig {
    /// If no build command is configured, infer one from the run command.
    /// This ensures existing configs automatically get rebuild support.
    pub fn backfill_build_command(&mut self) {
        if self.build_command.is_some() {
            return;
        }
        let (cmd, args) = match self.command.as_str() {
            "cargo" => ("cargo", vec!["build"]),
            "npm" | "npx" => ("npm", vec!["run", "build"]),
            "poetry" => ("poetry", vec!["install"]),
            "python" => {
                // Check for build system indicators in the cwd
                let cwd = std::path::Path::new(&self.cwd);
                if cwd.join("poetry.lock").exists() {
                    ("poetry", vec!["install"])
                } else if cwd.join("setup.py").exists()
                    || cwd.join("setup.cfg").exists()
                    || cwd.join("pyproject.toml").exists()
                {
                    ("pip", vec!["install", "-e", "."])
                } else {
                    return;
                }
            }
            "docker" => return,
            _ => return,
        };
        self.build_command = Some(cmd.to_string());
        self.build_args = args.into_iter().map(|s| s.to_string()).collect();
    }
}

fn default_category() -> String {
    "general".to_string()
}

fn default_buffer_size() -> usize {
    2000
}

fn default_true() -> bool {
    true
}

/// State of a managed process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessState {
    Stopped,
    Starting,
    Building,
    Running,
    Healthy,
    Stopping,
    Failed,
}

impl std::fmt::Display for ProcessState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProcessState::Stopped => write!(f, "stopped"),
            ProcessState::Starting => write!(f, "starting"),
            ProcessState::Building => write!(f, "building"),
            ProcessState::Running => write!(f, "running"),
            ProcessState::Healthy => write!(f, "healthy"),
            ProcessState::Stopping => write!(f, "stopping"),
            ProcessState::Failed => write!(f, "failed"),
        }
    }
}

/// Which output stream a line came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputStream {
    Stdout,
    Stderr,
}

/// A single line of output from a managed process.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputLine {
    /// ISO 8601 timestamp
    pub timestamp: String,
    /// Which stream this came from
    pub stream: OutputStream,
    /// The line content
    pub line: String,
}

/// Status summary of a managed process.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessStatus {
    pub id: String,
    pub name: String,
    pub state: ProcessState,
    pub pid: Option<u32>,
    /// Uptime in seconds (None if not running)
    pub uptime_secs: Option<u64>,
    /// Whether the health port is responding
    pub port_healthy: Option<bool>,
    /// Number of times this process has been restarted
    pub restart_count: u32,
    /// Number of errors detected from this process
    pub error_count: u32,
    pub category: String,
    /// Whether this process has a build command configured
    pub has_build_command: bool,
}

/// Internal runtime state for a managed process.
pub(crate) struct ProcessRuntime {
    /// When the process was started
    pub started_at: Option<Instant>,
    /// Current state
    pub state: ProcessState,
    /// PID of the child process
    pub pid: Option<u32>,
    /// Number of restarts
    pub restart_count: u32,
    /// Number of errors detected
    pub error_count: u32,
    /// Whether health port is healthy
    pub port_healthy: Option<bool>,
    /// Active database session ID for output persistence
    pub session_id: Option<String>,
    /// Whether specs have been auto-discovered for this process start
    pub specs_discovered: bool,
}

impl Default for ProcessRuntime {
    fn default() -> Self {
        Self {
            started_at: None,
            state: ProcessState::Stopped,
            pid: None,
            restart_count: 0,
            error_count: 0,
            port_healthy: None,
            session_id: None,
            specs_discovered: false,
        }
    }
}
