//! Types for process capture and management.
//!
//! The wire-format DTOs (`ProcessConfig`, `ProcessState`, `ProcessStatus`,
//! `OutputLine`, `OutputStream`, and the `ParserType` that
//! `ProcessConfig.parser` references) live in
//! `qontinui_types::process_management`; this module re-exports them so the
//! rest of the runner can continue to `use crate::process_capture::types::*`
//! without change.
//!
//! Runtime-only state (`ProcessRuntime` with its `Instant` stamps and session
//! ids) and runner-side behavior (`ProcessConfig::backfill_build_command`,
//! plus a `Display`-style helper for `ProcessState`) stay here. Behavior on
//! the foreign DTOs is exposed through extension traits because Rust's orphan
//! rule forbids inherent `impl` blocks on types defined in another crate.
//! Import `ProcessConfigExt` / `ProcessStateExt` (or glob-import this module)
//! to call `config.backfill_build_command()` and `state.as_str()` exactly as
//! if they were inherent.

pub use qontinui_types::process_management::*;

use std::time::Instant;

// ============================================================================
// ProcessConfig behavior (extension trait)
// ============================================================================

/// Runner-side behavior for [`ProcessConfig`].
///
/// Exposed as a trait because `ProcessConfig` is defined in `qontinui-types`
/// and we cannot add inherent methods to it here. Import this trait (or use
/// `crate::process_capture::types::*`) to call these methods exactly as if
/// they were inherent.
pub trait ProcessConfigExt {
    /// If no build command is configured, infer one from the run command.
    /// This ensures existing configs automatically get rebuild support.
    fn backfill_build_command(&mut self);
}

impl ProcessConfigExt for ProcessConfig {
    fn backfill_build_command(&mut self) {
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

// ============================================================================
// ProcessState behavior (extension trait)
// ============================================================================

/// Runner-side helpers for [`ProcessState`].
///
/// Replaces the pre-extraction `impl std::fmt::Display for ProcessState`,
/// which is impossible here because of the orphan rule. Callers that need a
/// human-readable string should use [`ProcessStateExt::as_str`] — every wire
/// value uses the same spelling as the old `Display` impl (`"stopped"`,
/// `"starting"`, etc.), so log messages and error strings keep their previous
/// formatting.
pub trait ProcessStateExt {
    fn as_str(&self) -> &'static str;
}

impl ProcessStateExt for ProcessState {
    fn as_str(&self) -> &'static str {
        match self {
            ProcessState::Stopped => "stopped",
            ProcessState::Starting => "starting",
            ProcessState::Building => "building",
            ProcessState::Running => "running",
            ProcessState::Healthy => "healthy",
            ProcessState::Stopping => "stopping",
            ProcessState::Failed => "failed",
        }
    }
}

// ============================================================================
// Runtime state (runner-only)
// ============================================================================

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
