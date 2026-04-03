//! Queue entry types (SQLite impl removed).

/// Status of a queued workflow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueueEntryStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Cancelled,
}

impl QueueEntryStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s {
            "pending" => Self::Pending,
            "running" => Self::Running,
            "completed" => Self::Completed,
            "failed" => Self::Failed,
            "cancelled" => Self::Cancelled,
            other => {
                tracing::warn!(
                    "Unknown queue entry status '{}', defaulting to Pending",
                    other
                );
                Self::Pending
            }
        }
    }
}

/// A persisted queue entry.
#[derive(Debug, Clone)]
pub struct PersistedQueueEntry {
    pub id: String,
    pub workflow_id: String,
    pub workflow_name: String,
    pub queued_at: String,
    pub priority: u8,
    pub status: QueueEntryStatus,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub error_message: Option<String>,
    pub task_run_id: Option<String>,
    pub retry_count: i32,
    pub max_retries: i32,
}
