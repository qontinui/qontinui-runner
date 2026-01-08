//! Task Recorder - Unified task execution tracking.
//!
//! This module provides the unified entry point for all task runs in Qontinui.
//! TaskRun is THE single concept for all execution:
//! - AI tasks (with prompt)
//! - Automation tasks (with config_id)
//! - Mixed tasks (AI with automation steps)
//!
//! # Architecture
//!
//! ```text
//! TaskRecorder (unified entry point)
//!     │
//!     ├─► Creates task_runs row
//!     │
//!     ├─► [Has automation?]
//!     │     └─► AutomationRecorder writes to task_run_automation
//!     │
//!     ├─► [Has AI sessions?]
//!     │     └─► Track session count, output logging
//!     │
//!     └─► TaskRecorder.complete_task()
//!           ├─► Update task_runs.status
//!           ├─► Generate summary (if configured)
//!           └─► Sync to qontinui-web
//! ```

mod recorder;

pub use recorder::{TaskConfig, TaskRecorder};
