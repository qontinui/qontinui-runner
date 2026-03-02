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

    // Re-trigger interval: after initial fire, allow re-firing every
    // (consecutive_failures_threshold * interval) seconds of continued failure.
    let re_trigger_secs = (consecutive_failures_threshold as u64) * interval;

    tokio::spawn(async move {
        // Track consecutive failures per URL
        let mut failure_counts: HashMap<String, u32> = HashMap::new();
        // Track when we last fired for each URL (for re-trigger interval)
        let mut last_fired: HashMap<String, std::time::Instant> = HashMap::new();

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

                if healthy {
                    if *count > 0 {
                        debug!(
                            "Health check: {} recovered after {} failures",
                            target.url, count
                        );
                    }
                    *count = 0;
                    last_fired.remove(&target.url);
                } else {
                    *count += 1;
                    debug!(
                        "Health check: {} failed ({}/{})",
                        target.url, count, consecutive_failures_threshold
                    );

                    if *count >= consecutive_failures_threshold {
                        // Fire if: never fired, or re-trigger interval has elapsed
                        let should_fire = match last_fired.get(&target.url) {
                            None => true,
                            Some(t) => t.elapsed().as_secs() >= re_trigger_secs,
                        };

                        if should_fire {
                            let is_retrigger = last_fired.contains_key(&target.url);
                            info!(
                                "Health check trigger '{}': {} failed {} consecutive times{}",
                                trigger_id,
                                target.url,
                                count,
                                if is_retrigger { " (re-trigger)" } else { "" }
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
                                event_type: if is_retrigger {
                                    "health_check_still_failing".to_string()
                                } else {
                                    "health_check_failed".to_string()
                                },
                                event_data: serde_json::json!({
                                    "url": target.url,
                                    "consecutive_failures": *count,
                                    "expected_status": target.expected_status,
                                    "is_retrigger": is_retrigger,
                                }),
                                variables,
                                chain_depth: 0,
                            };

                            if let Err(e) = tx.send(event).await {
                                warn!("Failed to send health check event: {}", e);
                            }

                            last_fired.insert(target.url.clone(), std::time::Instant::now());
                        }
                    }
                }
            }

            tokio::time::sleep(tokio::time::Duration::from_secs(interval)).await;
        }
    })
}
