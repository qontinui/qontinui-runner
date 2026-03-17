//! Doctor health monitoring service.
//!
//! Follows the error_monitor service pattern:
//! - `DoctorService` with `mpsc::Receiver<DoctorCommand>` for control
//! - `DoctorHandle` with `mpsc::Sender<DoctorCommand>` for external interaction
//! - Main loop: `tokio::select!` between command channel and check interval timer

use std::collections::HashMap;
use std::sync::atomic::Ordering;

use tokio::sync::mpsc::{self, Receiver, Sender};
use tracing::{debug, info};

use serde::Serialize;

use crate::doctor::config::DoctorConfig;
use crate::doctor::strategies;
use crate::doctor::types::{
    DoctorEvent, HealthStatus, MonitoredProcess, ProcessRegistration, ProcessType,
};

/// Snapshot of a monitored process's current status (for frontend queries).
#[derive(Debug, Clone, Serialize)]
pub struct ProcessStatus {
    pub pid: u32,
    pub label: String,
    pub process_type: ProcessType,
    pub status: HealthStatus,
    pub inactive_checks: u32,
}

/// Commands sent to the Doctor service via the handle.
#[derive(Debug)]
pub enum DoctorCommand {
    /// Register a new process for monitoring.
    Register(ProcessRegistration),
    /// Unregister a process by PID.
    Unregister { pid: u32 },
    /// Query current status of all monitored processes.
    QueryStatus {
        reply: tokio::sync::oneshot::Sender<Vec<ProcessStatus>>,
    },
    /// Shutdown the service.
    Shutdown,
}

// ProcessRegistration doesn't implement Debug automatically because of Arc<AtomicU64>
impl std::fmt::Debug for ProcessRegistration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProcessRegistration")
            .field("pid", &self.pid)
            .field("process_type", &self.process_type)
            .field("label", &self.label)
            .field("has_activity_tracker", &self.last_activity.is_some())
            .finish()
    }
}

/// Handle for controlling the Doctor service from other parts of the app.
#[derive(Clone)]
pub struct DoctorHandle {
    command_tx: Sender<DoctorCommand>,
}

impl DoctorHandle {
    /// Register a process for health monitoring.
    pub async fn register(&self, registration: ProcessRegistration) -> Result<(), String> {
        self.command_tx
            .send(DoctorCommand::Register(registration))
            .await
            .map_err(|e| format!("Failed to send register command: {}", e))
    }

    /// Register a process synchronously (for use from blocking/sync contexts).
    ///
    /// Uses `try_send()` instead of `blocking_send()` to avoid panicking when
    /// called inside `spawn_blocking()` (which already runs within a Tokio runtime).
    /// The channel is bounded (64 slots) so `try_send` can fail if full, but
    /// Doctor registration is best-effort — a missed registration just means
    /// the process won't be health-monitored.
    pub fn register_blocking(&self, registration: ProcessRegistration) -> Result<(), String> {
        self.command_tx
            .try_send(DoctorCommand::Register(registration))
            .map_err(|e| format!("Failed to send register command: {}", e))
    }

    /// Unregister a process by PID.
    pub async fn unregister(&self, pid: u32) -> Result<(), String> {
        self.command_tx
            .send(DoctorCommand::Unregister { pid })
            .await
            .map_err(|e| format!("Failed to send unregister command: {}", e))
    }

    /// Unregister a process synchronously (for use from blocking/sync contexts).
    ///
    /// Uses `try_send()` instead of `blocking_send()` to avoid panicking when
    /// called inside `spawn_blocking()` (which already runs within a Tokio runtime).
    pub fn unregister_blocking(&self, pid: u32) -> Result<(), String> {
        self.command_tx
            .try_send(DoctorCommand::Unregister { pid })
            .map_err(|e| format!("Failed to send unregister command: {}", e))
    }

    /// Query the current status of all monitored processes.
    pub async fn query_status(&self) -> Result<Vec<ProcessStatus>, String> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.command_tx
            .send(DoctorCommand::QueryStatus { reply: tx })
            .await
            .map_err(|e| format!("Failed to send query command: {}", e))?;
        rx.await
            .map_err(|e| format!("Failed to receive status: {}", e))
    }

    /// Shutdown the Doctor service.
    pub async fn shutdown(&self) -> Result<(), String> {
        self.command_tx
            .send(DoctorCommand::Shutdown)
            .await
            .map_err(|e| format!("Failed to send shutdown command: {}", e))
    }
}

/// The Doctor health monitoring service.
///
/// Periodically checks the health of registered AI processes using OS-level
/// signals (CPU time, memory, process tree) and emits events when processes
/// appear stuck. The Doctor NEVER kills processes — it only notifies.
pub struct DoctorService {
    config: DoctorConfig,
    /// Currently monitored processes keyed by PID.
    processes: HashMap<u32, MonitoredProcess>,
    /// Event sender for emitting Doctor events (consumed by the event bridge).
    event_tx: Sender<DoctorEvent>,
}

impl DoctorService {
    /// Create a new Doctor service, returning the service, handle, and command receiver.
    pub fn new(config: DoctorConfig) -> (Self, DoctorHandle, Receiver<DoctorCommand>) {
        let (command_tx, command_rx) = mpsc::channel(64);
        let (event_tx, _event_rx) = mpsc::channel(64);

        let service = Self {
            config,
            processes: HashMap::new(),
            event_tx,
        };

        let handle = DoctorHandle { command_tx };

        (service, handle, command_rx)
    }

    /// Create a new Doctor service with an external event receiver.
    /// The event receiver can be used to bridge Doctor events to Tauri.
    pub fn new_with_events(
        config: DoctorConfig,
    ) -> (
        Self,
        DoctorHandle,
        Receiver<DoctorCommand>,
        Receiver<DoctorEvent>,
    ) {
        let (command_tx, command_rx) = mpsc::channel(64);
        let (event_tx, event_rx) = mpsc::channel(64);

        let service = Self {
            config,
            processes: HashMap::new(),
            event_tx,
        };

        let handle = DoctorHandle { command_tx };

        (service, handle, command_rx, event_rx)
    }

    /// Run the Doctor service main loop.
    pub async fn run(mut self, mut command_rx: Receiver<DoctorCommand>) {
        info!(
            "Doctor service started (check_interval: {:?}, suspicious: {} checks, stuck: {} checks)",
            self.config.check_interval,
            self.config.suspicious_threshold,
            self.config.stuck_threshold
        );

        loop {
            tokio::select! {
                // Handle incoming commands
                Some(cmd) = command_rx.recv() => {
                    match cmd {
                        DoctorCommand::Register(reg) => {
                            info!(
                                "Doctor: Registering process PID {} ({:?}, label: {})",
                                reg.pid, reg.process_type, reg.label
                            );
                            let pid = reg.pid;
                            self.processes.insert(pid, MonitoredProcess::from_registration(reg));
                        }
                        DoctorCommand::Unregister { pid } => {
                            if self.processes.remove(&pid).is_some() {
                                info!("Doctor: Unregistered process PID {}", pid);
                            }
                        }
                        DoctorCommand::QueryStatus { reply } => {
                            let statuses: Vec<ProcessStatus> = self
                                .processes
                                .values()
                                .map(|p| ProcessStatus {
                                    pid: p.pid,
                                    label: p.label.clone(),
                                    process_type: p.process_type.clone(),
                                    status: p.status.clone(),
                                    inactive_checks: p.inactive_checks,
                                })
                                .collect();
                            let _ = reply.send(statuses);
                        }
                        DoctorCommand::Shutdown => {
                            info!("Doctor service shutting down");
                            break;
                        }
                    }
                }

                // Periodic health check
                _ = tokio::time::sleep(self.config.check_interval) => {
                    self.run_health_checks().await;
                }
            }
        }

        info!("Doctor service stopped");
    }

    /// Run health checks on all monitored processes.
    async fn run_health_checks(&mut self) {
        if self.processes.is_empty() {
            return;
        }

        // Collect PIDs to check (avoid borrowing issues)
        let pids: Vec<u32> = self.processes.keys().copied().collect();
        let mut to_remove = Vec::new();

        for pid in pids {
            let check_result = strategies::check_process_health(pid);

            if !check_result.process_alive {
                // Process is gone — auto-unregister
                if let Some(proc) = self.processes.get(&pid) {
                    debug!(
                        "Doctor: Process PID {} ({}) has exited, unregistering",
                        pid, proc.label
                    );
                }
                to_remove.push(pid);
                continue;
            }

            // Evaluate health based on available signals
            if let Some(proc) = self.processes.get_mut(&pid) {
                let (was_inactive, recovery_event) =
                    Self::evaluate_process_health(&self.config, proc, &check_result);

                // Emit recovery event if process returned to healthy
                if let Some(event) = recovery_event {
                    let _ = self.event_tx.try_send(event);
                }

                // Emit events based on status transitions
                match proc.status {
                    HealthStatus::Suspicious if was_inactive => {
                        let event = DoctorEvent::Warning {
                            pid: proc.pid,
                            process_label: proc.label.clone(),
                            message: format!(
                                "Process has been inactive for {} consecutive checks (~{}s)",
                                proc.inactive_checks,
                                proc.inactive_checks as u64 * self.config.check_interval.as_secs()
                            ),
                            inactive_checks: proc.inactive_checks,
                        };
                        let _ = self.event_tx.send(event).await;
                    }
                    HealthStatus::Stuck if was_inactive => {
                        let event = DoctorEvent::Stuck {
                            pid: proc.pid,
                            process_label: proc.label.clone(),
                            message: format!(
                                "Process appears stuck: no CPU activity, no stdout, no process tree changes for {} checks (~{}s)",
                                proc.inactive_checks,
                                proc.inactive_checks as u64
                                    * self.config.check_interval.as_secs()
                            ),
                            inactive_checks: proc.inactive_checks,
                        };
                        let _ = self.event_tx.send(event).await;
                    }
                    _ => {}
                }
            }
        }

        // Remove dead processes
        for pid in to_remove {
            self.processes.remove(&pid);
        }
    }

    /// Evaluate health of a single process based on check results.
    /// Returns `(is_inactive, optional_recovery_event)`.
    /// This is a pure function — event emission is handled by the caller.
    fn evaluate_process_health(
        config: &DoctorConfig,
        proc: &mut MonitoredProcess,
        check: &strategies::HealthCheckResult,
    ) -> (bool, Option<DoctorEvent>) {
        let mut signals_inactive = true;

        // Signal 1: CPU time delta
        if let Some(current_cpu) = check.cpu_time {
            if let Some(last_cpu) = proc.last_cpu_time {
                let cpu_delta = current_cpu.saturating_sub(last_cpu);
                // Non-zero CPU delta means the process is doing work.
                // Use a small threshold to filter noise (10ms = 100,000 * 100ns).
                if cpu_delta > 100_000 {
                    signals_inactive = false;
                }
            } else {
                // First check — can't determine delta, assume active
                signals_inactive = false;
            }
            proc.last_cpu_time = Some(current_cpu);
        }

        // Signal 2: Activity tracker (for SessionStreaming processes)
        if let Some(ref activity_arc) = proc.last_activity {
            let last_activity_secs = activity_arc.load(Ordering::Relaxed);
            let now_secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let gap = now_secs.saturating_sub(last_activity_secs);

            if gap < config.activity_gap_seconds {
                signals_inactive = false;
            }
        }

        // Signal 3: Process tree changes
        if let Some(current_children) = check.child_count {
            if let Some(last_children) = proc.last_child_count {
                if current_children != last_children {
                    // Tree changed — process is doing something
                    signals_inactive = false;
                }
            }
            proc.last_child_count = Some(current_children);
        }

        // Signal 4: Memory changes (rapid growth = active, flat + no CPU = stronger stuck signal)
        if let Some(current_mem) = check.memory_bytes {
            if let Some(last_mem) = proc.last_memory {
                let mem_delta = current_mem.abs_diff(last_mem);
                // Significant memory change (> 1MB) suggests activity
                if mem_delta > 1_048_576 {
                    signals_inactive = false;
                }
            }
            proc.last_memory = Some(current_mem);
        }

        // Update status based on signals
        let previous_status = proc.status.clone();
        let mut recovery_event = None;

        if signals_inactive {
            proc.inactive_checks += 1;

            if proc.inactive_checks >= config.stuck_threshold {
                proc.status = HealthStatus::Stuck;
            } else if proc.inactive_checks >= config.suspicious_threshold {
                proc.status = HealthStatus::Suspicious;
            } else {
                proc.status = HealthStatus::Indeterminate;
            }
        } else {
            // Activity detected — reset to healthy
            let was_unhealthy = matches!(
                previous_status,
                HealthStatus::Suspicious | HealthStatus::Stuck
            );
            proc.inactive_checks = 0;
            proc.status = HealthStatus::Healthy;

            // Return recovery event if we were previously unhealthy
            if was_unhealthy {
                recovery_event = Some(DoctorEvent::Healthy {
                    pid: proc.pid,
                    process_label: proc.label.clone(),
                });
            }
        }

        (signals_inactive, recovery_event)
    }
}

/// Start the Doctor service in a background task.
/// Returns the handle for registration/control.
pub async fn start_doctor_async(config: DoctorConfig) -> (DoctorHandle, Receiver<DoctorEvent>) {
    let (service, handle, command_rx, event_rx) = DoctorService::new_with_events(config);

    tokio::spawn(async move {
        service.run(command_rx).await;
    });

    (handle, event_rx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doctor::types::ProcessType;
    use std::sync::atomic::AtomicU64;
    use std::sync::Arc;

    #[tokio::test]
    async fn test_service_creation() {
        let config = DoctorConfig::default();
        let (_service, _handle, _command_rx) = DoctorService::new(config);
    }

    #[tokio::test]
    async fn test_register_and_unregister() {
        let config = DoctorConfig::default();
        let (handle, _event_rx) = start_doctor_async(config).await;

        // Register a fake process
        let reg = ProcessRegistration {
            pid: 99999999,
            process_type: ProcessType::ResponseOneShot,
            label: "Test process".to_string(),
            last_activity: None,
        };
        assert!(handle.register(reg).await.is_ok());

        // Unregister it
        assert!(handle.unregister(99999999).await.is_ok());

        // Shutdown
        assert!(handle.shutdown().await.is_ok());
    }

    #[tokio::test]
    async fn test_dead_process_auto_unregisters() {
        let mut config = DoctorConfig::default();
        config.check_interval = std::time::Duration::from_millis(50);

        let (handle, _event_rx) = start_doctor_async(config).await;

        // Register a nonexistent PID
        let reg = ProcessRegistration {
            pid: 99999999,
            process_type: ProcessType::ResponseOneShot,
            label: "Dead process".to_string(),
            last_activity: None,
        };
        handle.register(reg).await.unwrap();

        // Wait for a health check cycle
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // Shutdown
        handle.shutdown().await.unwrap();
    }

    #[test]
    fn test_evaluate_health_first_check_is_healthy() {
        let config = DoctorConfig::default();
        let (event_tx, _) = mpsc::channel(64);
        let service = DoctorService {
            config,
            processes: HashMap::new(),
            event_tx,
        };

        let mut proc = MonitoredProcess {
            pid: 1234,
            process_type: ProcessType::ResponseOneShot,
            label: "test".to_string(),
            last_activity: None,
            status: HealthStatus::Healthy,
            inactive_checks: 0,
            last_cpu_time: None,
            last_memory: None,
            last_child_count: None,
        };

        let check = strategies::HealthCheckResult {
            process_alive: true,
            cpu_time: Some(1_000_000),
            memory_bytes: Some(50_000_000),
            child_count: Some(2),
        };

        // First check — no previous data, should be considered active
        let (inactive, recovery) =
            DoctorService::evaluate_process_health(&service.config, &mut proc, &check);
        assert!(!inactive);
        assert!(recovery.is_none());
        assert_eq!(proc.status, HealthStatus::Healthy);
    }

    #[test]
    fn test_evaluate_health_escalation() {
        let mut config = DoctorConfig::default();
        config.suspicious_threshold = 2;
        config.stuck_threshold = 4;

        let (event_tx, _) = mpsc::channel(64);
        let service = DoctorService {
            config,
            processes: HashMap::new(),
            event_tx,
        };

        let mut proc = MonitoredProcess {
            pid: 1234,
            process_type: ProcessType::ResponseOneShot,
            label: "test".to_string(),
            last_activity: None,
            status: HealthStatus::Healthy,
            inactive_checks: 0,
            // Set initial CPU time so delta can be computed
            last_cpu_time: Some(1_000_000),
            last_memory: Some(50_000_000),
            last_child_count: Some(2),
        };

        // Simulate checks with zero CPU delta and no changes
        let check = strategies::HealthCheckResult {
            process_alive: true,
            cpu_time: Some(1_000_000), // Same as before = zero delta
            memory_bytes: Some(50_000_000),
            child_count: Some(2),
        };

        // Check 1: Indeterminate
        DoctorService::evaluate_process_health(&service.config, &mut proc, &check);
        assert_eq!(proc.inactive_checks, 1);
        assert_eq!(proc.status, HealthStatus::Indeterminate);

        // Check 2: Suspicious (threshold = 2)
        DoctorService::evaluate_process_health(&service.config, &mut proc, &check);
        assert_eq!(proc.inactive_checks, 2);
        assert_eq!(proc.status, HealthStatus::Suspicious);

        // Check 3: Still suspicious
        DoctorService::evaluate_process_health(&service.config, &mut proc, &check);
        assert_eq!(proc.inactive_checks, 3);
        assert_eq!(proc.status, HealthStatus::Suspicious);

        // Check 4: Stuck (threshold = 4)
        DoctorService::evaluate_process_health(&service.config, &mut proc, &check);
        assert_eq!(proc.inactive_checks, 4);
        assert_eq!(proc.status, HealthStatus::Stuck);
    }

    #[test]
    fn test_evaluate_health_recovery() {
        let config = DoctorConfig::default();
        let (event_tx, _) = mpsc::channel(64);
        let service = DoctorService {
            config,
            processes: HashMap::new(),
            event_tx,
        };

        let mut proc = MonitoredProcess {
            pid: 1234,
            process_type: ProcessType::ResponseOneShot,
            label: "test".to_string(),
            last_activity: None,
            status: HealthStatus::Suspicious,
            inactive_checks: 3,
            last_cpu_time: Some(1_000_000),
            last_memory: Some(50_000_000),
            last_child_count: Some(2),
        };

        // Now process shows CPU activity
        let check = strategies::HealthCheckResult {
            process_alive: true,
            cpu_time: Some(2_000_000), // Significant increase
            memory_bytes: Some(50_000_000),
            child_count: Some(2),
        };

        let (inactive, recovery) =
            DoctorService::evaluate_process_health(&service.config, &mut proc, &check);
        assert!(!inactive);
        assert!(recovery.is_some()); // Was Suspicious → Healthy, so recovery event emitted
        assert_eq!(proc.status, HealthStatus::Healthy);
        assert_eq!(proc.inactive_checks, 0);
    }

    #[test]
    fn test_activity_tracker_prevents_inactive() {
        let config = DoctorConfig::default();
        let (event_tx, _) = mpsc::channel(64);
        let service = DoctorService {
            config,
            processes: HashMap::new(),
            event_tx,
        };

        // Set activity tracker to current time
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let activity = Arc::new(AtomicU64::new(now_secs));

        let mut proc = MonitoredProcess {
            pid: 1234,
            process_type: ProcessType::SessionStreaming,
            label: "test".to_string(),
            last_activity: Some(activity),
            status: HealthStatus::Healthy,
            inactive_checks: 0,
            last_cpu_time: Some(1_000_000),
            last_memory: Some(50_000_000),
            last_child_count: Some(2),
        };

        // Even with zero CPU delta, activity tracker is recent
        let check = strategies::HealthCheckResult {
            process_alive: true,
            cpu_time: Some(1_000_000), // Same = no CPU delta
            memory_bytes: Some(50_000_000),
            child_count: Some(2),
        };

        let (inactive, _) =
            DoctorService::evaluate_process_health(&service.config, &mut proc, &check);
        assert!(!inactive);
        assert_eq!(proc.status, HealthStatus::Healthy);
    }
}
