//! Workflow execution queue with concurrency control and rate limiting.
//!
//! Provides infrastructure for managing concurrent workflow executions:
//!
//! - **WorkflowQueue**: A bounded queue with semaphore-based concurrency control.
//!   Workflows are submitted and dequeued in priority order. A tokio `Semaphore`
//!   limits how many workflows can execute concurrently.
//!
//! - **ApiRateLimiter**: A token-bucket rate limiter for AI API calls. Tracks
//!   available tokens and refills them over time, providing backpressure when
//!   the API rate limit is approached.
//!
//! This module provides the queue infrastructure only. Integration with the
//! actual workflow execution path (routing starts through the queue) is
//! handled separately.

use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::Arc;
use tokio::sync::{Mutex, Notify, Semaphore};
use tracing::{debug, info, warn};

// =============================================================================
// Queue Configuration
// =============================================================================

/// Configuration for the workflow queue.
#[derive(Debug, Clone)]
pub struct QueueConfig {
    /// Maximum number of workflows executing concurrently.
    pub max_concurrent: usize,
    /// Maximum queue depth — new submissions are rejected when full.
    pub max_queue_depth: usize,
}

impl Default for QueueConfig {
    fn default() -> Self {
        Self {
            max_concurrent: 3,
            max_queue_depth: 50,
        }
    }
}

// =============================================================================
// Queued Workflow Entry
// =============================================================================

/// A workflow waiting in the queue to be executed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueuedWorkflow {
    /// Unique identifier for this queue entry (typically a UUID).
    pub id: String,
    /// The workflow definition ID to execute.
    pub workflow_id: String,
    /// Human-readable workflow name (for logging and display).
    pub workflow_name: String,
    /// ISO-8601 timestamp of when the workflow was queued.
    pub queued_at: String,
    /// Priority: 0 = normal, 1 = high. High-priority items are dequeued first.
    pub priority: u8,
}

// =============================================================================
// Queue Status
// =============================================================================

/// Snapshot of queue state, suitable for API responses.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueueStatus {
    /// Number of workflows waiting to execute.
    pub pending: usize,
    /// Number of workflows currently executing (semaphore permits in use).
    pub running: usize,
    /// Maximum concurrent executions allowed.
    pub max_concurrent: usize,
    /// Maximum queue depth before submissions are rejected.
    pub max_queue_depth: usize,
}

// =============================================================================
// Workflow Queue
// =============================================================================

/// A bounded workflow execution queue with semaphore-based concurrency control.
///
/// The queue supports priority ordering: high-priority workflows (priority > 0)
/// are inserted at the front of the queue, while normal-priority workflows are
/// appended to the back.
///
/// The `next()` method blocks until both a queued workflow is available AND a
/// semaphore permit is acquired, ensuring the concurrency limit is respected.
pub struct WorkflowQueue {
    queue: Mutex<VecDeque<QueuedWorkflow>>,
    semaphore: Arc<Semaphore>,
    config: QueueConfig,
    notify: Notify,
}

impl WorkflowQueue {
    /// Create a new workflow queue with the given configuration.
    pub fn new(config: QueueConfig) -> Self {
        info!(
            "Workflow queue initialized: max_concurrent={}, max_queue_depth={}",
            config.max_concurrent, config.max_queue_depth
        );
        Self {
            queue: Mutex::new(VecDeque::new()),
            semaphore: Arc::new(Semaphore::new(config.max_concurrent)),
            config,
            notify: Notify::new(),
        }
    }

    /// Submit a workflow for execution.
    ///
    /// Returns the queue position (0-indexed) on success, or an error string
    /// if the queue is full.
    ///
    /// High-priority workflows (priority > 0) are inserted at the front of the
    /// queue ahead of normal-priority items. Normal-priority workflows are
    /// appended to the back.
    pub async fn submit(&self, workflow: QueuedWorkflow) -> Result<usize, String> {
        let mut queue = self.queue.lock().await;

        if queue.len() >= self.config.max_queue_depth {
            warn!(
                "Workflow queue is full ({}/{}), rejecting workflow '{}' (id: {})",
                queue.len(),
                self.config.max_queue_depth,
                workflow.workflow_name,
                workflow.id
            );
            return Err(format!(
                "Queue is full ({}/{}). Try again later.",
                queue.len(),
                self.config.max_queue_depth
            ));
        }

        let priority = workflow.priority;
        let name = workflow.workflow_name.clone();
        let id = workflow.id.clone();

        if priority > 0 {
            // High-priority: insert at the front, but after any other high-priority items
            let insert_pos = queue
                .iter()
                .position(|w| w.priority == 0)
                .unwrap_or(queue.len());
            queue.insert(insert_pos, workflow);
            info!(
                "Workflow '{}' (id: {}) submitted with HIGH priority at position {}",
                name, id, insert_pos
            );
        } else {
            queue.push_back(workflow);
            info!(
                "Workflow '{}' (id: {}) submitted at position {}",
                name,
                id,
                queue.len() - 1
            );
        }

        let position = queue
            .iter()
            .position(|w| w.id == id)
            .unwrap_or(queue.len() - 1);

        // Wake up any consumer waiting in `next()`
        self.notify.notify_one();

        Ok(position)
    }

    /// Get the next workflow to execute.
    ///
    /// This method blocks until both:
    /// 1. A workflow is available in the queue
    /// 2. A concurrency permit is available from the semaphore
    ///
    /// Returns `None` only if the semaphore is closed (shutdown scenario).
    /// The caller receives ownership of both the `QueuedWorkflow` and an
    /// `OwnedSemaphorePermit`. The permit is held for the duration of
    /// execution and released when dropped.
    pub async fn next(&self) -> Option<(QueuedWorkflow, tokio::sync::OwnedSemaphorePermit)> {
        loop {
            // First, acquire a semaphore permit (blocks until a slot is free)
            let permit = match self.semaphore.clone().acquire_owned().await {
                Ok(permit) => permit,
                Err(_) => {
                    // Semaphore closed — shutting down
                    warn!("Workflow queue semaphore closed, returning None");
                    return None;
                }
            };

            // Now try to pop from the queue
            {
                let mut queue = self.queue.lock().await;
                if let Some(workflow) = queue.pop_front() {
                    info!(
                        "Dequeued workflow '{}' (id: {}), {} remaining in queue",
                        workflow.workflow_name,
                        workflow.id,
                        queue.len()
                    );
                    return Some((workflow, permit));
                }
            }

            // Queue was empty — release the permit and wait for a notification
            drop(permit);
            debug!("Queue empty, waiting for notification");
            self.notify.notified().await;
        }
    }

    /// Get a snapshot of the current queue status.
    pub async fn status(&self) -> QueueStatus {
        let queue = self.queue.lock().await;
        let pending = queue.len();
        let running = self.config.max_concurrent - self.semaphore.available_permits();

        QueueStatus {
            pending,
            running,
            max_concurrent: self.config.max_concurrent,
            max_queue_depth: self.config.max_queue_depth,
        }
    }

    /// Remove a queued workflow by its ID.
    ///
    /// Returns `true` if the workflow was found and removed, `false` otherwise.
    /// Only pending (not yet executing) workflows can be cancelled.
    pub async fn cancel(&self, id: &str) -> bool {
        let mut queue = self.queue.lock().await;
        let initial_len = queue.len();
        queue.retain(|w| w.id != id);
        let removed = queue.len() < initial_len;
        if removed {
            info!("Cancelled queued workflow with id: {}", id);
        } else {
            debug!(
                "No queued workflow found with id: {} (may already be executing)",
                id
            );
        }
        removed
    }

    /// List all workflows currently waiting in the queue.
    ///
    /// Does not include workflows that are currently executing.
    pub async fn list(&self) -> Vec<QueuedWorkflow> {
        let queue = self.queue.lock().await;
        queue.iter().cloned().collect()
    }
}

// =============================================================================
// Global Singleton
// =============================================================================

/// Global workflow queue instance with default configuration.
///
/// Access via `get_workflow_queue()` from anywhere in the application.
static WORKFLOW_QUEUE: Lazy<WorkflowQueue> =
    Lazy::new(|| WorkflowQueue::new(QueueConfig::default()));

/// Get a reference to the global workflow queue.
pub fn get_workflow_queue() -> &'static WorkflowQueue {
    &WORKFLOW_QUEUE
}

// =============================================================================
// API Rate Limiter
// =============================================================================

/// A token-bucket rate limiter for AI API calls.
///
/// Each API call consumes one token. Tokens are refilled continuously based on
/// a configurable rate (derived from max calls per minute). When no tokens are
/// available, `acquire()` blocks until a token becomes available.
///
/// This limiter is designed to be conservative and prevent 429 responses from
/// upstream AI providers.
pub struct ApiRateLimiter {
    /// Current number of available tokens.
    tokens: Mutex<f64>,
    /// Maximum token capacity.
    max_tokens: f64,
    /// Tokens added per second.
    refill_rate: f64,
    /// Timestamp of the last refill calculation.
    last_refill: Mutex<std::time::Instant>,
}

impl ApiRateLimiter {
    /// Create a new rate limiter with the given maximum calls per minute.
    ///
    /// For example, `ApiRateLimiter::new(30.0)` allows 30 API calls per minute,
    /// refilling at 0.5 tokens per second.
    pub fn new(max_calls_per_minute: f64) -> Self {
        let refill_rate = max_calls_per_minute / 60.0;
        let max_tokens = max_calls_per_minute;
        info!(
            "API rate limiter initialized: {:.0} calls/min, refill_rate={:.2}/sec",
            max_calls_per_minute, refill_rate
        );
        Self {
            tokens: Mutex::new(max_tokens),
            max_tokens,
            refill_rate,
            last_refill: Mutex::new(std::time::Instant::now()),
        }
    }

    /// Refill tokens based on elapsed time since the last refill.
    async fn refill(&self) {
        let mut last = self.last_refill.lock().await;
        let mut tokens = self.tokens.lock().await;

        let elapsed = last.elapsed().as_secs_f64();
        let new_tokens = elapsed * self.refill_rate;
        *tokens = (*tokens + new_tokens).min(self.max_tokens);
        *last = std::time::Instant::now();
    }

    /// Wait until a token is available, then consume it.
    ///
    /// If no tokens are available, this method sleeps in a tight loop with
    /// 100ms intervals, refilling tokens on each iteration until one becomes
    /// available.
    pub async fn acquire(&self) {
        loop {
            self.refill().await;

            let mut tokens = self.tokens.lock().await;
            if *tokens >= 1.0 {
                *tokens -= 1.0;
                debug!(
                    "Rate limiter: token acquired, {:.1} tokens remaining",
                    *tokens
                );
                return;
            }

            // Calculate how long to wait for the next token
            let deficit = 1.0 - *tokens;
            let wait_secs = deficit / self.refill_rate;
            drop(tokens); // Release the lock while sleeping

            debug!(
                "Rate limiter: no tokens available, waiting {:.0}ms",
                wait_secs * 1000.0
            );
            tokio::time::sleep(tokio::time::Duration::from_millis(
                (wait_secs * 1000.0).ceil() as u64,
            ))
            .await;
        }
    }

    /// Record a rate limit response (HTTP 429) from the upstream API.
    ///
    /// This immediately drains all available tokens, forcing subsequent
    /// callers to wait for a full refill cycle. This is a conservative
    /// response to prevent hammering the API.
    pub async fn record_rate_limit(&self) {
        warn!("Rate limit response received (429) — draining all tokens to back off");
        let mut tokens = self.tokens.lock().await;
        *tokens = 0.0;
    }

    /// Get the current number of available tokens (for monitoring).
    pub async fn available_tokens(&self) -> f64 {
        self.refill().await;
        let tokens = self.tokens.lock().await;
        *tokens
    }
}

/// Global API rate limiter instance.
///
/// Default: 30 calls per minute (conservative).
static API_RATE_LIMITER: Lazy<ApiRateLimiter> = Lazy::new(|| ApiRateLimiter::new(30.0));

/// Get a reference to the global API rate limiter.
pub fn get_api_rate_limiter() -> &'static ApiRateLimiter {
    &API_RATE_LIMITER
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_queue_submit_and_list() {
        let queue = WorkflowQueue::new(QueueConfig {
            max_concurrent: 2,
            max_queue_depth: 10,
        });

        let wf = QueuedWorkflow {
            id: "test-1".to_string(),
            workflow_id: "wf-abc".to_string(),
            workflow_name: "Test Workflow".to_string(),
            queued_at: chrono::Utc::now().to_rfc3339(),
            priority: 0,
        };

        let pos = queue.submit(wf.clone()).await.unwrap();
        assert_eq!(pos, 0);

        let items = queue.list().await;
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, "test-1");
    }

    #[tokio::test]
    async fn test_queue_priority_ordering() {
        let queue = WorkflowQueue::new(QueueConfig {
            max_concurrent: 1,
            max_queue_depth: 10,
        });

        // Submit a normal-priority workflow
        let normal = QueuedWorkflow {
            id: "normal-1".to_string(),
            workflow_id: "wf-1".to_string(),
            workflow_name: "Normal".to_string(),
            queued_at: chrono::Utc::now().to_rfc3339(),
            priority: 0,
        };
        queue.submit(normal).await.unwrap();

        // Submit a high-priority workflow
        let high = QueuedWorkflow {
            id: "high-1".to_string(),
            workflow_id: "wf-2".to_string(),
            workflow_name: "High Priority".to_string(),
            queued_at: chrono::Utc::now().to_rfc3339(),
            priority: 1,
        };
        queue.submit(high).await.unwrap();

        // High priority should be first
        let items = queue.list().await;
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].id, "high-1");
        assert_eq!(items[1].id, "normal-1");
    }

    #[tokio::test]
    async fn test_queue_cancel() {
        let queue = WorkflowQueue::new(QueueConfig {
            max_concurrent: 2,
            max_queue_depth: 10,
        });

        let wf = QueuedWorkflow {
            id: "cancel-me".to_string(),
            workflow_id: "wf-x".to_string(),
            workflow_name: "To Cancel".to_string(),
            queued_at: chrono::Utc::now().to_rfc3339(),
            priority: 0,
        };
        queue.submit(wf).await.unwrap();
        assert_eq!(queue.list().await.len(), 1);

        let cancelled = queue.cancel("cancel-me").await;
        assert!(cancelled);
        assert_eq!(queue.list().await.len(), 0);

        // Cancelling again should return false
        let cancelled_again = queue.cancel("cancel-me").await;
        assert!(!cancelled_again);
    }

    #[tokio::test]
    async fn test_queue_full_rejection() {
        let queue = WorkflowQueue::new(QueueConfig {
            max_concurrent: 1,
            max_queue_depth: 2,
        });

        for i in 0..2 {
            let wf = QueuedWorkflow {
                id: format!("wf-{}", i),
                workflow_id: format!("wf-{}", i),
                workflow_name: format!("Workflow {}", i),
                queued_at: chrono::Utc::now().to_rfc3339(),
                priority: 0,
            };
            queue.submit(wf).await.unwrap();
        }

        // Third submission should be rejected
        let wf = QueuedWorkflow {
            id: "wf-overflow".to_string(),
            workflow_id: "wf-overflow".to_string(),
            workflow_name: "Overflow".to_string(),
            queued_at: chrono::Utc::now().to_rfc3339(),
            priority: 0,
        };
        let result = queue.submit(wf).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("full"));
    }

    #[tokio::test]
    async fn test_queue_status() {
        let queue = WorkflowQueue::new(QueueConfig {
            max_concurrent: 3,
            max_queue_depth: 50,
        });

        let status = queue.status().await;
        assert_eq!(status.pending, 0);
        assert_eq!(status.running, 0);
        assert_eq!(status.max_concurrent, 3);
        assert_eq!(status.max_queue_depth, 50);

        // Submit a workflow to see pending count increase
        let wf = QueuedWorkflow {
            id: "status-test".to_string(),
            workflow_id: "wf-s".to_string(),
            workflow_name: "Status Test".to_string(),
            queued_at: chrono::Utc::now().to_rfc3339(),
            priority: 0,
        };
        queue.submit(wf).await.unwrap();

        let status = queue.status().await;
        assert_eq!(status.pending, 1);
    }

    #[tokio::test]
    async fn test_queue_next_dequeues() {
        let queue = Arc::new(WorkflowQueue::new(QueueConfig {
            max_concurrent: 2,
            max_queue_depth: 10,
        }));

        let wf = QueuedWorkflow {
            id: "dequeue-test".to_string(),
            workflow_id: "wf-d".to_string(),
            workflow_name: "Dequeue Test".to_string(),
            queued_at: chrono::Utc::now().to_rfc3339(),
            priority: 0,
        };
        queue.submit(wf).await.unwrap();

        // next() should return immediately since there's a queued item and permits available
        let result =
            tokio::time::timeout(tokio::time::Duration::from_millis(500), queue.next()).await;

        assert!(result.is_ok(), "next() should not have timed out");
        let (workflow, _permit) = result.unwrap().unwrap();
        assert_eq!(workflow.id, "dequeue-test");

        // Queue should now be empty
        assert_eq!(queue.list().await.len(), 0);
    }

    #[tokio::test]
    async fn test_rate_limiter_acquire() {
        let limiter = ApiRateLimiter::new(600.0); // 10/sec = generous for testing

        // Should acquire immediately since we start full
        let start = std::time::Instant::now();
        limiter.acquire().await;
        let elapsed = start.elapsed();
        assert!(elapsed.as_millis() < 100, "First acquire should be instant");
    }

    #[tokio::test]
    async fn test_rate_limiter_drain_on_429() {
        let limiter = ApiRateLimiter::new(60.0); // 1/sec

        // Record a rate limit — should drain tokens
        limiter.record_rate_limit().await;

        let available = limiter.available_tokens().await;
        assert!(
            available < 1.0,
            "Tokens should be near-zero after rate limit, got {}",
            available
        );
    }
}
