//! Approval gate system for human-in-the-loop workflow pauses.
//!
//! Provides the `ApprovalRegistry` which manages pending approval requests.
//! Workflow execution pauses on an approval gate, waiting for a human response
//! via the runner API or frontend UI.
//!
//! ## Flow
//!
//! 1. During the agentic phase, the AI outputs `[APPROVAL_GATE]` marker or
//!    the workflow config specifies an approval point.
//! 2. The loop controller creates an `ApprovalRequest` and registers it.
//! 3. An `ApprovalRequired` event is emitted to the frontend.
//! 4. Execution pauses (the loop controller awaits the oneshot receiver).
//! 5. The human responds via `POST /task-runs/:id/approvals/:aid/respond`.
//! 6. The response is sent through the oneshot channel, unblocking execution.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{oneshot, Mutex};
use tracing::{info, warn};

/// Context provided to the human reviewer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalContext {
    /// Summary of what the AI did in the agentic phase
    pub summary: String,
    /// Files modified (from structured output or git diff)
    #[serde(default)]
    pub files_modified: Vec<String>,
    /// Git diff stat (one-line summary)
    #[serde(default)]
    pub git_diff_stat: Option<String>,
    /// Git diff content (truncated)
    #[serde(default)]
    pub git_diff: Option<String>,
}

/// A request for human approval.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalRequest {
    /// Unique ID for this approval request
    pub id: String,
    /// Task execution ID
    pub execution_id: String,
    /// Current iteration
    pub iteration: u32,
    /// Prompt/question for the reviewer
    pub prompt: String,
    /// Context about what happened
    pub context: ApprovalContext,
    /// Available options (e.g., ["Approve", "Reject"])
    pub options: Vec<String>,
    /// Timestamp when the request was created
    pub created_at: String,
}

/// Response from the human reviewer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalResponse {
    /// Whether the action was approved
    pub approved: bool,
    /// The action taken (e.g., "approve", "reject", "abort")
    pub action: String,
    /// Optional comment from the reviewer
    #[serde(default)]
    pub comment: Option<String>,
}

/// Internal state for a pending approval.
struct PendingApproval {
    request: ApprovalRequest,
    sender: Option<oneshot::Sender<ApprovalResponse>>,
}

/// Registry that manages pending approval requests.
///
/// Thread-safe and shared across the application. Approval requests are
/// registered by the loop controller and resolved by API endpoint handlers.
pub struct ApprovalRegistry {
    pending: Mutex<HashMap<String, PendingApproval>>,
}

impl ApprovalRegistry {
    pub fn new() -> Self {
        Self {
            pending: Mutex::new(HashMap::new()),
        }
    }

    /// Register a new approval request.
    ///
    /// Returns a `oneshot::Receiver` that the caller can await to get the response.
    /// The request is stored and can be retrieved via `get_pending()` for the API/UI.
    pub async fn register(&self, request: ApprovalRequest) -> oneshot::Receiver<ApprovalResponse> {
        let (tx, rx) = oneshot::channel();
        let id = request.id.clone();

        let mut pending = self.pending.lock().await;
        pending.insert(
            id.clone(),
            PendingApproval {
                request,
                sender: Some(tx),
            },
        );
        info!("Registered approval request: {}", id);

        rx
    }

    /// Resolve a pending approval request with a response.
    ///
    /// Returns `Ok(())` if the approval was found and resolved,
    /// or `Err(message)` if not found or already resolved.
    pub async fn resolve(
        &self,
        approval_id: &str,
        response: ApprovalResponse,
    ) -> Result<(), String> {
        let mut pending = self.pending.lock().await;

        if let Some(mut approval) = pending.remove(approval_id) {
            if let Some(sender) = approval.sender.take() {
                info!(
                    "Resolving approval '{}': action={}, approved={}",
                    approval_id, response.action, response.approved
                );
                sender.send(response).map_err(|_| {
                    "Failed to send approval response — receiver was dropped".to_string()
                })
            } else {
                Err(format!("Approval '{}' was already resolved", approval_id))
            }
        } else {
            Err(format!(
                "No pending approval found with ID '{}'",
                approval_id
            ))
        }
    }

    /// Get all pending approval requests for a given execution.
    pub async fn get_pending_for_execution(&self, execution_id: &str) -> Vec<ApprovalRequest> {
        let pending = self.pending.lock().await;
        pending
            .values()
            .filter(|a| a.request.execution_id == execution_id)
            .map(|a| a.request.clone())
            .collect()
    }

    /// Get a specific pending approval request.
    pub async fn get_pending(&self, approval_id: &str) -> Option<ApprovalRequest> {
        let pending = self.pending.lock().await;
        pending.get(approval_id).map(|a| a.request.clone())
    }

    /// Remove all pending approvals for an execution (cleanup on stop/abort).
    pub async fn cancel_all_for_execution(&self, execution_id: &str) {
        let mut pending = self.pending.lock().await;
        let ids_to_remove: Vec<String> = pending
            .iter()
            .filter(|(_, a)| a.request.execution_id == execution_id)
            .map(|(id, _)| id.clone())
            .collect();

        for id in &ids_to_remove {
            if let Some(mut approval) = pending.remove(id) {
                // Send a reject response to unblock any waiting code
                if let Some(sender) = approval.sender.take() {
                    let _ = sender.send(ApprovalResponse {
                        approved: false,
                        action: "abort".to_string(),
                        comment: Some("Execution was stopped".to_string()),
                    });
                }
            }
        }

        if !ids_to_remove.is_empty() {
            warn!(
                "Cancelled {} pending approval(s) for execution {}",
                ids_to_remove.len(),
                execution_id
            );
        }
    }
}

/// Global approval registry instance.
static APPROVAL_REGISTRY: std::sync::OnceLock<Arc<ApprovalRegistry>> = std::sync::OnceLock::new();

/// Get or initialize the global approval registry.
pub fn get_approval_registry() -> Arc<ApprovalRegistry> {
    APPROVAL_REGISTRY
        .get_or_init(|| Arc::new(ApprovalRegistry::new()))
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_register_and_resolve() {
        let registry = ApprovalRegistry::new();

        let request = ApprovalRequest {
            id: "test-1".to_string(),
            execution_id: "exec-1".to_string(),
            iteration: 2,
            prompt: "Review these changes?".to_string(),
            context: ApprovalContext {
                summary: "Fixed auth bug".to_string(),
                files_modified: vec!["src/auth.rs".to_string()],
                git_diff_stat: Some("1 file changed".to_string()),
                git_diff: None,
            },
            options: vec!["Approve".to_string(), "Reject".to_string()],
            created_at: "2026-01-01T00:00:00Z".to_string(),
        };

        let rx = registry.register(request).await;

        // Resolve in a separate task
        let response = ApprovalResponse {
            approved: true,
            action: "approve".to_string(),
            comment: Some("Looks good".to_string()),
        };
        registry.resolve("test-1", response).await.unwrap();

        let result = rx.await.unwrap();
        assert!(result.approved);
        assert_eq!(result.action, "approve");
    }

    #[tokio::test]
    async fn test_get_pending() {
        let registry = ApprovalRegistry::new();

        let request = ApprovalRequest {
            id: "test-2".to_string(),
            execution_id: "exec-2".to_string(),
            iteration: 1,
            prompt: "Approve?".to_string(),
            context: ApprovalContext {
                summary: "Changes".to_string(),
                files_modified: vec![],
                git_diff_stat: None,
                git_diff: None,
            },
            options: vec!["Approve".to_string(), "Reject".to_string()],
            created_at: "2026-01-01T00:00:00Z".to_string(),
        };

        let _rx = registry.register(request).await;

        let pending = registry.get_pending_for_execution("exec-2").await;
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, "test-2");

        let specific = registry.get_pending("test-2").await;
        assert!(specific.is_some());
    }

    #[tokio::test]
    async fn test_cancel_all() {
        let registry = ApprovalRegistry::new();

        let request = ApprovalRequest {
            id: "test-3".to_string(),
            execution_id: "exec-3".to_string(),
            iteration: 1,
            prompt: "Approve?".to_string(),
            context: ApprovalContext {
                summary: "Changes".to_string(),
                files_modified: vec![],
                git_diff_stat: None,
                git_diff: None,
            },
            options: vec!["Approve".to_string(), "Reject".to_string()],
            created_at: "2026-01-01T00:00:00Z".to_string(),
        };

        let rx = registry.register(request).await;
        registry.cancel_all_for_execution("exec-3").await;

        // The receiver should get an abort response
        let result = rx.await.unwrap();
        assert!(!result.approved);
        assert_eq!(result.action, "abort");

        // No more pending
        let pending = registry.get_pending_for_execution("exec-3").await;
        assert!(pending.is_empty());
    }
}
