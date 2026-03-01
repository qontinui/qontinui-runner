//! Health check watcher: continuously monitors service URLs.
//!
//! Fires a trigger when a URL fails `consecutive_failures` times in a row.
//! Resets the failure counter when the URL recovers.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use super::super::types::{HealthCheckTarget, TriggerEvent};

/// Start a health check poller for a trigger.
///
/// Returns a JoinHandle for the spawned tokio task.
pub fn start_health_check(
    trigger_id: String,
    urls: Vec<HealthCheckTarget>,
    check_interval_seconds: u64,
    consecutive_failures_threshold: u32,
    tx: mpsc::Sender<TriggerEvent>,
    stop_signal: Arc<AtomicBool>,
) -> tokio::task::JoinHandle<()> {
    // Enforce minimum interval
    let interval = check_interval_seconds.max(10);

    tokio::spawn(async move {
        // Track consecutive failures per URL
        let mut failure_counts: HashMap<String, u32> = HashMap::new();
        // Track whether we've already fired for a URL (avoid re-firing on continued failure)
        let mut fired: HashMap<String, bool> = HashMap::new();

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .unwrap_or_default();

        loop {
            if stop_signal.load(Ordering::SeqCst) {
                break;
            }

            for target in &urls {
                let timeout = std::time::Duration::from_secs(target.timeout_seconds.max(1));

                let result = client.get(&target.url).timeout(timeout).send().await;

                let healthy = match result {
                    Ok(resp) => resp.status().as_u16() == target.expected_status,
                    Err(_) => false,
                };

                let count = failure_counts.entry(target.url.clone()).or_insert(0);
                let already_fired = fired.entry(target.url.clone()).or_insert(false);

                if healthy {
                    if *count > 0 {
                        debug!(
                            "Health check: {} recovered after {} failures",
                            target.url, count
                        );
                    }
                    *count = 0;
                    *already_fired = false;
                } else {
                    *count += 1;
                    debug!(
                        "Health check: {} failed ({}/{})",
                        target.url, count, consecutive_failures_threshold
                    );

                    if *count >= consecutive_failures_threshold && !*already_fired {
                        info!(
                            "Health check trigger '{}': {} failed {} consecutive times",
                            trigger_id, target.url, count
                        );

                        let mut variables = HashMap::new();
                        variables.insert("failed_url".to_string(), target.url.clone());
                        variables.insert("failure_count".to_string(), count.to_string());
                        variables.insert(
                            "expected_status".to_string(),
                            target.expected_status.to_string(),
                        );

                        let event = TriggerEvent {
                            trigger_id: trigger_id.clone(),
                            event_type: "health_check_failed".to_string(),
                            event_data: serde_json::json!({
                                "url": target.url,
                                "consecutive_failures": *count,
                                "expected_status": target.expected_status,
                            }),
                            variables,
                            chain_depth: 0,
                        };

                        if let Err(e) = tx.send(event).await {
                            warn!("Failed to send health check event: {}", e);
                        }

                        *already_fired = true;
                    }
                }
            }

            tokio::time::sleep(tokio::time::Duration::from_secs(interval)).await;
        }
    })
}
