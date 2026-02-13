//! Types for the Doctor health monitoring service.
//!
//! Defines process registration, health status, and event types
//! used by the Doctor to monitor AI processes.

use serde::Serialize;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;

/// Type of AI process being monitored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum ProcessType {
    /// Interactive streaming session (e.g., `claude --output-format stream-json`).
    /// Has `last_activity` Arc for stdout-based activity tracking.
    SessionStreaming,
    /// One-shot response mode (e.g., `claude --print`).
    /// Silent until done — relies on OS signals only.
    ResponseOneShot,
    /// HTTP API request via reqwest. Fast, minimal monitoring.
    ApiRequest,
}

/// Health status of a monitored process.
///
/// Escalation model: Healthy -> Indeterminate -> Suspicious -> Stuck
/// The Doctor NEVER kills processes — it only emits events.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum HealthStatus {
    /// Process is active: CPU delta > 0 OR recent stdout activity.
    Healthy,
    /// Single check with no CPU delta — normal during I/O waits.
    Indeterminate,
    /// Multiple consecutive checks with no activity — emit warning.
    Suspicious,
    /// Extended period of no activity across all signals — emit stuck event.
    Stuck,
}

/// Registration info for a monitored process.
pub struct ProcessRegistration {
    /// OS process ID.
    pub pid: u32,
    /// Type of process for strategy selection.
    pub process_type: ProcessType,
    /// Human-readable label (e.g., "Setup AI session", "Summary generation").
    pub label: String,
    /// Optional activity tracker for SessionStreaming processes.
    /// Updated by the stdout reader thread with Unix epoch seconds.
    pub last_activity: Option<Arc<AtomicU64>>,
}

/// Internal state for a monitored process within the Doctor service.
pub(crate) struct MonitoredProcess {
    /// OS process ID.
    pub pid: u32,
    /// Type of process.
    pub process_type: ProcessType,
    /// Human-readable label.
    pub label: String,
    /// Optional activity tracker (for SessionStreaming).
    pub last_activity: Option<Arc<AtomicU64>>,
    /// Current health status.
    pub status: HealthStatus,
    /// Number of consecutive "no activity" checks.
    pub inactive_checks: u32,
    /// Last sampled CPU time in 100-nanosecond intervals (kernel + user).
    pub last_cpu_time: Option<u64>,
    /// Last sampled working set size in bytes.
    pub last_memory: Option<u64>,
    /// Last sampled child process count.
    pub last_child_count: Option<u32>,
}

impl MonitoredProcess {
    /// Create a new monitored process from a registration.
    pub fn from_registration(reg: ProcessRegistration) -> Self {
        Self {
            pid: reg.pid,
            process_type: reg.process_type,
            label: reg.label,
            last_activity: reg.last_activity,
            status: HealthStatus::Healthy,
            inactive_checks: 0,
            last_cpu_time: None,
            last_memory: None,
            last_child_count: None,
        }
    }
}

/// Events emitted by the Doctor service to the frontend.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum DoctorEvent {
    /// Process appears suspicious — multiple checks with no activity.
    Warning {
        pid: u32,
        process_label: String,
        message: String,
        inactive_checks: u32,
    },
    /// Process appears stuck — extended period of no activity.
    Stuck {
        pid: u32,
        process_label: String,
        message: String,
        inactive_checks: u32,
    },
    /// Process has recovered and is healthy again.
    Healthy { pid: u32, process_label: String },
}
