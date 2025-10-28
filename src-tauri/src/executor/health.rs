use crate::error::AppError;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tokio::time::{interval, Instant};
use tracing::{debug, error, info, warn};

/// Health check interval (how often to ping Python process)
const HEALTH_CHECK_INTERVAL: Duration = Duration::from_secs(5);

/// Maximum time to wait for pong response
const PONG_TIMEOUT: Duration = Duration::from_secs(3);

/// Health status of the Python executor
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HealthStatus {
    /// Health monitoring not started yet
    NotStarted,

    /// Executor is healthy (responding to pings)
    Healthy {
        /// Last successful ping time
        last_ping: Instant,
    },

    /// Executor is unhealthy (not responding to pings)
    Unhealthy {
        /// Time when executor became unhealthy
        since: Instant,
        /// Number of consecutive failed pings
        consecutive_failures: u32,
    },

    /// Health monitoring stopped
    Stopped,
}

/// Manages health monitoring of the Python executor
pub struct HealthMonitor {
    /// Current health status
    status: Arc<RwLock<HealthStatus>>,

    /// Whether monitoring is active
    active: Arc<RwLock<bool>>,

    /// Last pong timestamp we received
    last_pong: Arc<RwLock<Option<Instant>>>,

    /// Pending ping timestamp (waiting for pong)
    pending_ping: Arc<RwLock<Option<Instant>>>,
}

impl HealthMonitor {
    /// Creates a new health monitor
    pub fn new() -> Self {
        Self {
            status: Arc::new(RwLock::new(HealthStatus::NotStarted)),
            active: Arc::new(RwLock::new(false)),
            last_pong: Arc::new(RwLock::new(None)),
            pending_ping: Arc::new(RwLock::new(None)),
        }
    }

    /// Starts health monitoring
    pub async fn start(&self) {
        let mut active = self.active.write().await;
        *active = true;

        let mut status = self.status.write().await;
        *status = HealthStatus::Healthy {
            last_ping: Instant::now(),
        };

        info!("Health monitoring started");
    }

    /// Stops health monitoring
    pub async fn stop(&self) {
        let mut active = self.active.write().await;
        *active = false;

        let mut status = self.status.write().await;
        *status = HealthStatus::Stopped;

        info!("Health monitoring stopped");
    }

    /// Returns current health status
    #[allow(dead_code)]
    pub async fn get_status(&self) -> HealthStatus {
        self.status.read().await.clone()
    }

    /// Returns whether monitoring is active
    pub async fn is_active(&self) -> bool {
        *self.active.read().await
    }

    /// Records a pong response received from executor
    pub async fn record_pong(&self) {
        let now = Instant::now();

        // Update last pong time
        let mut last_pong = self.last_pong.write().await;
        *last_pong = Some(now);

        // Clear pending ping
        let mut pending_ping = self.pending_ping.write().await;
        if let Some(ping_time) = *pending_ping {
            let latency = now.duration_since(ping_time);
            debug!("Received pong, latency: {:?}", latency);
        }
        *pending_ping = None;

        // Update status to healthy
        let mut status = self.status.write().await;
        *status = HealthStatus::Healthy { last_ping: now };
    }

    /// Records that we sent a ping
    pub async fn record_ping(&self) {
        let now = Instant::now();
        let mut pending_ping = self.pending_ping.write().await;
        *pending_ping = Some(now);
        debug!("Sent health check ping");
    }

    /// Checks if we're waiting too long for a pong
    pub async fn check_pong_timeout(&self) -> bool {
        let pending = self.pending_ping.read().await;

        if let Some(ping_time) = *pending {
            let elapsed = Instant::now().duration_since(ping_time);

            if elapsed > PONG_TIMEOUT {
                warn!("Pong timeout: no response for {:?}", elapsed);
                return true;
            }
        }

        false
    }

    /// Marks the executor as unhealthy
    pub async fn mark_unhealthy(&self, consecutive_failures: u32) {
        let mut status = self.status.write().await;

        if let HealthStatus::Unhealthy { since, .. } = &*status {
            // Already unhealthy, just update failure count
            *status = HealthStatus::Unhealthy {
                since: *since,
                consecutive_failures,
            };
        } else {
            // Transitioning to unhealthy
            warn!(
                "Executor marked as unhealthy after {} consecutive failures",
                consecutive_failures
            );
            *status = HealthStatus::Unhealthy {
                since: Instant::now(),
                consecutive_failures,
            };
        }
    }

    /// Returns whether the executor is healthy
    #[allow(dead_code)]
    pub async fn is_healthy(&self) -> bool {
        matches!(*self.status.read().await, HealthStatus::Healthy { .. })
    }
}

impl Default for HealthMonitor {
    fn default() -> Self {
        Self::new()
    }
}

/// Health check task that runs in the background
pub struct HealthCheckTask {
    monitor: Arc<HealthMonitor>,
    ping_sender: tokio::sync::mpsc::Sender<()>,
}

impl HealthCheckTask {
    /// Creates a new health check task
    pub fn new(monitor: Arc<HealthMonitor>, ping_sender: tokio::sync::mpsc::Sender<()>) -> Self {
        Self {
            monitor,
            ping_sender,
        }
    }

    /// Runs the health check loop
    pub async fn run(self) -> Result<(), AppError> {
        info!("Starting health check task");

        let mut tick = interval(HEALTH_CHECK_INTERVAL);
        let mut consecutive_failures = 0u32;

        loop {
            tick.tick().await;

            // Check if monitoring is still active
            if !self.monitor.is_active().await {
                info!("Health monitoring stopped, exiting task");
                break;
            }

            // Send ping command to Python executor
            if let Err(e) = self.ping_sender.send(()).await {
                error!("Failed to send ping: {}", e);
                break;
            }

            self.monitor.record_ping().await;

            // Wait a bit and check for pong timeout
            tokio::time::sleep(PONG_TIMEOUT).await;

            if self.monitor.check_pong_timeout().await {
                consecutive_failures += 1;
                self.monitor.mark_unhealthy(consecutive_failures).await;

                if consecutive_failures >= 3 {
                    error!(
                        "Executor unresponsive after {} consecutive health check failures",
                        consecutive_failures
                    );
                    // Could trigger a restart or other recovery action here
                }
            } else {
                // Reset failure counter on success
                if consecutive_failures > 0 {
                    info!("Executor recovered, resetting failure counter");
                    consecutive_failures = 0;
                }
            }
        }

        info!("Health check task exiting");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_health_monitor_initialization() {
        let monitor = HealthMonitor::new();
        let status = monitor.get_status().await;
        assert_eq!(status, HealthStatus::NotStarted);
    }

    #[tokio::test]
    async fn test_start_monitoring() {
        let monitor = HealthMonitor::new();
        monitor.start().await;

        assert!(monitor.is_active().await);
        assert!(monitor.is_healthy().await);
    }

    #[tokio::test]
    async fn test_pong_recording() {
        let monitor = HealthMonitor::new();
        monitor.start().await;

        monitor.record_ping().await;
        monitor.record_pong().await;

        assert!(monitor.is_healthy().await);
    }

    #[tokio::test]
    async fn test_unhealthy_marking() {
        let monitor = HealthMonitor::new();
        monitor.start().await;

        monitor.mark_unhealthy(1).await;

        assert!(!monitor.is_healthy().await);

        let status = monitor.get_status().await;
        if let HealthStatus::Unhealthy {
            consecutive_failures,
            ..
        } = status
        {
            assert_eq!(consecutive_failures, 1);
        } else {
            panic!("Expected Unhealthy status");
        }
    }

    #[tokio::test]
    async fn test_stop_monitoring() {
        let monitor = HealthMonitor::new();
        monitor.start().await;
        monitor.stop().await;

        assert!(!monitor.is_active().await);

        let status = monitor.get_status().await;
        assert_eq!(status, HealthStatus::Stopped);
    }
}
