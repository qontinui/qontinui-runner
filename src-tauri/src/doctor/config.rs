//! Configuration for the Doctor health monitoring service.

use std::time::Duration;

/// Configuration for the Doctor service.
#[derive(Debug, Clone)]
pub struct DoctorConfig {
    /// How often to run health checks on monitored processes.
    pub check_interval: Duration,
    /// Number of consecutive inactive checks before emitting a warning.
    pub suspicious_threshold: u32,
    /// Number of consecutive inactive checks before emitting a stuck event.
    pub stuck_threshold: u32,
    /// Minimum activity gap (in seconds) for SessionStreaming processes
    /// before considering the process potentially inactive.
    /// This prevents false alarms during normal I/O waits.
    pub activity_gap_seconds: u64,
}

impl Default for DoctorConfig {
    fn default() -> Self {
        Self {
            check_interval: Duration::from_secs(15),
            suspicious_threshold: 3,  // 3 checks = ~45 seconds
            stuck_threshold: 6,       // 6 checks = ~90 seconds
            activity_gap_seconds: 30, // 30 seconds of no stdout before counting as inactive
        }
    }
}
