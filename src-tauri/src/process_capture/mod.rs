//! Process capture module for spawning and managing child processes.
//!
//! This module enables the runner to directly spawn and manage child processes
//! (dev servers, build tools, etc.), capturing their stdout/stderr streams in
//! memory and feeding detected errors into the existing Error Monitor pipeline.
//!
//! # Architecture
//!
//! ```text
//! ProcessCaptureManager
//!   ├─ HashMap<id, ManagedProcess>
//!   │    ├─ tokio::process::Child
//!   │    ├─ stdout reader task → ring buffer + StreamErrorParser
//!   │    ├─ stderr reader task → ring buffer + StreamErrorParser
//!   │    └─ health check (port polling)
//!   │
//!   ├─ Ring Buffer (VecDeque<OutputLine>, configurable size)
//!   │    └─ Frontend fetches via get_process_output
//!   │
//!   └─ StreamErrorParser
//!        ├─ Reuses LogParser trait (parse_line, is_error_start, etc.)
//!        ├─ Multi-line error accumulation
//!        └─ Sends errors → ErrorMonitorHandle::ingest_stream_errors
//!             └─ Existing dedup + SQLite storage
//! ```

pub mod build_errors;
pub mod commands;
pub mod health;
pub mod manager;
pub mod primary_proxy;
pub(crate) mod process;
pub mod stream_parser;
pub mod types;

pub use manager::ProcessCaptureManager;
pub use types::*;
