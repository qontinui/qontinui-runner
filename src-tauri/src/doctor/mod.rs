//! Doctor — Intelligent process health monitoring service.
//!
//! Replaces timeout-based process control with health-aware monitoring.
//! The Doctor inspects actual process health (CPU usage, process tree,
//! memory, stdout activity) and emits events when processes appear stuck,
//! rather than blindly killing them after arbitrary time limits.
//!
//! **Philosophy**: "Timeouts are dumb controls for a smart system."
//!
//! The Doctor NEVER kills processes. It observes and notifies.

pub mod commands;
pub mod config;
pub mod service;
pub mod strategies;
pub mod types;

// Re-exports for convenience
pub use config::DoctorConfig;
pub use service::{start_doctor_async, DoctorHandle};
pub use types::{DoctorEvent, ProcessRegistration, ProcessType};
