//! Exploration engine — drives automated multi-page exploration of SDK apps.
//!
//! Runs as a background async task. Fetches snapshots from the target app,
//! generates fingerprints, clicks interactive elements, captures state changes,
//! and builds co-occurrence data for state discovery.

use std::collections::HashSet;
use std::sync::Arc;

use serde_json::Value;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use super::cooccurrence::CooccurrenceBuilder;
use super::discovery::{DiscoveryConfig, FingerprintStateDiscovery};
use super::fingerprint::{self, FingerprintConfig};
use super::types::*;
use crate::mcp::sdk_client::SdkConnectionManager;

// =============================================================================
// Engine
// =============================================================================

pub struct ExplorationEngine {
    cancel_token: CancellationToken,
    progress_tx: watch::Sender<ExplorationProgress>,
    progress_rx: watch::Receiver<ExplorationProgress>,
}

impl ExplorationEngine {
    pub fn new() -> Self {
        let (progress_tx, progress_rx) = watch::channel(ExplorationProgress {
            current: 0,
            total: 0,
            current_element: None,
            status: "idle".to_string(),
            captures: 0,
            unique_fingerprints: 0,
        });

        Self {
            cancel_token: CancellationToken::new(),
            progress_tx,
            progress_rx,
        }
    }

    /// Get a receiver for progress updates.
    pub fn progress_rx(&self) -> watch::Receiver<ExplorationProgress> {
        self.progress_rx.clone()
    }

    /// Get a clone of the cancel token (for external cancellation).
    pub fn cancel_token(&self) -> CancellationToken {
        self.cancel_token.clone()
    }

    /// Cancel the current exploration.
    pub fn cancel(&self) {
        self.cancel_token.cancel();
    }

    /// Run exploration against the connected SDK app.
    ///
    /// This is the main entry point. It:
    /// 1. Fetches initial snapshot from the target app
    /// 2. Generates fingerprints for all elements
    /// 3. BFS-explores interactive elements (clicks, captures, detects changes)
    /// 4. Builds co-occurrence data
    /// 5. Runs state discovery
    /// 6. Returns the discovery result
    pub async fn explore(
        &mut self,
        sdk_conn: &Arc<tokio::sync::Mutex<SdkConnectionManager>>,
        config: ExplorationConfig,
    ) -> Result<DiscoveryResult, String> {
        self.cancel_token = CancellationToken::new();
        let cancel = self.cancel_token.clone();

        let fp_config = FingerprintConfig {
            viewport_width: config.viewport_width,
            viewport_height: config.viewport_height,
        };

        let session_id = format!(
            "explore-{}-{}",
            chrono::Utc::now().timestamp_millis(),
            uuid::Uuid::new_v4()
                .to_string()
                .split('-')
                .next()
                .unwrap_or("0000")
        );

        let mut builder = CooccurrenceBuilder::new(session_id);
        let mut interacted_fingerprints: HashSet<String> = HashSet::new();
        let mut interacted_ids: HashSet<String> = HashSet::new();
        let mut interaction_count = 0usize;

        self.update_progress("running", 0, 0, None, &builder);

        // Step 1: Fetch initial snapshot
        let (current_elements, catalog, mut current_element_map) =
            self.fetch_and_fingerprint(sdk_conn, &fp_config).await?;

        let hashes = fingerprint::extract_hashes(&current_element_map);
        let cap_id = format!("cap-{}", chrono::Utc::now().timestamp_millis());
        builder.add_capture(
            cap_id,
            "initial".to_string(),
            "Initial".to_string(),
            hashes.clone(),
            &catalog,
        );

        let mut baseline_fingerprints: HashSet<String> = hashes.iter().cloned().collect();

        // Step 2: Build initial queue
        let mut queue = filter_explorable(
            &current_elements,
            &current_element_map,
            &interacted_fingerprints,
            &interacted_ids,
            &fp_config,
            &config.blocked_keywords,
        );

        self.update_progress("running", 0, queue.len(), None, &builder);

        info!(
            "Initial exploration queue: {} interactive elements (from {} total)",
            queue.len(),
            current_elements.len()
        );

        // Step 3: BFS exploration loop
        while !queue.is_empty() && interaction_count < config.max_interactions {
            if cancel.is_cancelled() {
                info!(
                    "Exploration cancelled after {} interactions",
                    interaction_count
                );
                break;
            }

            let (el_id, el_label, el_fp_hash) = queue.remove(0);

            // Mark as interacted
            if let Some(ref fp_hash) = el_fp_hash {
                interacted_fingerprints.insert(fp_hash.clone());
            }
            interacted_ids.insert(el_id.clone());
            interaction_count += 1;

            self.update_progress(
                "running",
                interaction_count,
                interaction_count + queue.len(),
                Some(el_label.clone()),
                &builder,
            );

            debug!(
                "Exploring [{}/{}]: click \"{}\" ({})",
                interaction_count,
                interaction_count + queue.len(),
                el_label,
                el_id
            );

            let before_hashes: HashSet<String> = fingerprint::extract_hashes(&current_element_map)
                .into_iter()
                .collect();
            let before_cap_id = builder.last_capture_id().unwrap_or("unknown").to_string();

            // Click the element
            if let Err(e) = execute_action(sdk_conn, &el_id, "click").await {
                debug!("Click failed for {}: {}", el_id, e);
                continue;
            }

            // Wait for UI to settle
            tokio::time::sleep(tokio::time::Duration::from_millis(config.settle_delay_ms)).await;

            // Capture new state
            let (new_elements, new_catalog, new_element_map) =
                match self.fetch_and_fingerprint(sdk_conn, &fp_config).await {
                    Ok(result) => result,
                    Err(e) => {
                        warn!("Failed to fetch elements after click: {}", e);
                        continue;
                    }
                };

            // Update current element map for next iteration's fingerprint comparison
            current_element_map = new_element_map.clone();

            let after_hashes_vec = fingerprint::extract_hashes(&new_element_map);
            let after_hashes: HashSet<String> = after_hashes_vec.iter().cloned().collect();

            let after_cap_id = format!("cap-{}", chrono::Utc::now().timestamp_millis());
            builder.add_capture(
                after_cap_id.clone(),
                el_label.clone(),
                format!("After click: {}", el_label),
                after_hashes_vec,
                &new_catalog,
            );

            // Record transition
            builder.add_transition(
                "click".to_string(),
                el_fp_hash,
                before_cap_id,
                after_cap_id,
                &before_hashes,
                &after_hashes,
            );

            // Detect significant change
            let appeared: Vec<&String> = after_hashes.difference(&before_hashes).collect();
            let disappeared: Vec<&String> = before_hashes.difference(&after_hashes).collect();
            let total_before = before_hashes.len();
            let change_ratio = if total_before > 0 {
                (appeared.len() + disappeared.len()) as f64 / total_before as f64
            } else {
                0.0
            };

            if !appeared.is_empty() || !disappeared.is_empty() {
                debug!(
                    "State change: +{} -{} fingerprints ({:.1}% change)",
                    appeared.len(),
                    disappeared.len(),
                    change_ratio * 100.0
                );
            }

            if change_ratio > config.change_threshold {
                info!("Significant state change — exploring new elements");

                // Add new explorable elements
                let new_explorable = filter_explorable(
                    &new_elements,
                    &new_element_map,
                    &interacted_fingerprints,
                    &interacted_ids,
                    &fp_config,
                    &config.blocked_keywords,
                );

                for item in new_explorable {
                    if !queue.iter().any(|(id, _, _)| *id == item.0) {
                        queue.push(item);
                    }
                }

                // Try to navigate back
                let back_element = find_back_navigation(&new_elements);
                if let Some((back_id, back_label)) = back_element {
                    debug!("Navigating back via \"{}\"", back_label);
                    let _ = execute_action(sdk_conn, &back_id, "click").await;
                    tokio::time::sleep(tokio::time::Duration::from_millis(config.settle_delay_ms))
                        .await;

                    // Re-fetch and check if we returned
                    if let Ok((ret_elements, ret_catalog, ret_map)) =
                        self.fetch_and_fingerprint(sdk_conn, &fp_config).await
                    {
                        let ret_hashes: HashSet<String> =
                            fingerprint::extract_hashes(&ret_map).into_iter().collect();
                        let overlap = baseline_fingerprints.intersection(&ret_hashes).count();
                        let return_ratio = if baseline_fingerprints.is_empty() {
                            0.0
                        } else {
                            overlap as f64 / baseline_fingerprints.len() as f64
                        };

                        let ret_cap_id = format!("cap-{}", chrono::Utc::now().timestamp_millis());
                        builder.add_capture(
                            ret_cap_id,
                            "return".to_string(),
                            "After back navigation".to_string(),
                            fingerprint::extract_hashes(&ret_map),
                            &ret_catalog,
                        );

                        if return_ratio > 0.7 {
                            debug!("Returned to baseline ({:.1}% match)", return_ratio * 100.0);
                        } else {
                            debug!(
                                "Different state after back ({:.1}% match)",
                                return_ratio * 100.0
                            );
                            baseline_fingerprints = ret_hashes;
                        }

                        // Refresh queue
                        queue = filter_explorable(
                            &ret_elements,
                            &ret_map,
                            &interacted_fingerprints,
                            &interacted_ids,
                            &fp_config,
                            &config.blocked_keywords,
                        );
                    }
                } else {
                    baseline_fingerprints = after_hashes;
                }
            }
        }

        if interaction_count >= config.max_interactions {
            info!(
                "Reached max interaction limit ({})",
                config.max_interactions
            );
        }

        info!(
            "Exploration complete: {} interactions, {} captures, {} unique fingerprints",
            interaction_count,
            builder.capture_count(),
            builder.unique_fingerprint_count()
        );

        // Step 4: Build co-occurrence export and run discovery
        let final_captures = builder.capture_count();
        let final_fingerprints = builder.unique_fingerprint_count();
        let export = builder.build();
        let mut discovery = FingerprintStateDiscovery::new(DiscoveryConfig::default());
        discovery.load_cooccurrence_export(&export);
        discovery.discover_states();

        let _ = self.progress_tx.send(ExplorationProgress {
            current: interaction_count,
            total: interaction_count,
            current_element: None,
            status: "complete".to_string(),
            captures: final_captures,
            unique_fingerprints: final_fingerprints,
        });

        Ok(discovery.into_result())
    }

    // =========================================================================
    // Helpers
    // =========================================================================

    async fn fetch_and_fingerprint(
        &self,
        sdk_conn: &Arc<tokio::sync::Mutex<SdkConnectionManager>>,
        fp_config: &FingerprintConfig,
    ) -> Result<
        (
            Vec<Value>,
            std::collections::HashMap<String, super::types::ElementFingerprint>,
            std::collections::HashMap<String, String>,
        ),
        String,
    > {
        let snapshot =
            sdk_request(sdk_conn, reqwest::Method::GET, "/control/snapshot", None).await?;

        let elements: Vec<Value> = snapshot
            .get("data")
            .and_then(|d| d.get("elements"))
            .and_then(|e| e.as_array())
            .cloned()
            .unwrap_or_default();

        let (catalog, element_map) = fingerprint::generate_fingerprints(&elements, fp_config);

        Ok((elements, catalog, element_map))
    }

    fn update_progress(
        &self,
        status: &str,
        current: usize,
        total: usize,
        current_element: Option<String>,
        builder: &CooccurrenceBuilder,
    ) {
        let _ = self.progress_tx.send(ExplorationProgress {
            current,
            total,
            current_element,
            status: status.to_string(),
            captures: builder.capture_count(),
            unique_fingerprints: builder.unique_fingerprint_count(),
        });
    }
}

// =============================================================================
// SDK Communication (uses existing sdk_request infrastructure)
// =============================================================================

async fn sdk_request(
    sdk_conn: &Arc<tokio::sync::Mutex<SdkConnectionManager>>,
    method: reqwest::Method,
    path: &str,
    body: Option<Value>,
) -> Result<Value, String> {
    let conn_guard = sdk_conn.lock().await;
    let conn = conn_guard
        .active_connection()
        .ok_or_else(|| "No active SDK app connection".to_string())?;

    let url = format!("{}{}{}", conn.app_url, conn.base_path, path);
    let mut request = conn.client.request(method, &url);

    if let Some(body) = body {
        request = request
            .header("Content-Type", "application/json")
            .json(&body);
    }

    let resp = request
        .send()
        .await
        .map_err(|e| format!("SDK request failed: {}", e))?;

    let status = resp.status();
    let json: Value = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse SDK response: {}", e))?;

    if !status.is_success() {
        return Err(format!("SDK app returned HTTP {}", status));
    }

    Ok(json)
}

async fn execute_action(
    sdk_conn: &Arc<tokio::sync::Mutex<SdkConnectionManager>>,
    element_id: &str,
    action: &str,
) -> Result<(), String> {
    let path = format!(
        "/control/element/{}/action",
        urlencoding::encode(element_id)
    );
    let body = serde_json::json!({ "action": action, "params": {} });
    sdk_request(sdk_conn, reqwest::Method::POST, &path, Some(body)).await?;
    Ok(())
}

// =============================================================================
// Element Filtering
// =============================================================================

/// Filter elements to those eligible for exploration clicks.
/// Returns (element_id, label, fingerprint_hash) tuples.
fn filter_explorable(
    elements: &[Value],
    element_map: &std::collections::HashMap<String, String>,
    interacted_fps: &HashSet<String>,
    interacted_ids: &HashSet<String>,
    fp_config: &FingerprintConfig,
    blocked_keywords: &[String],
) -> Vec<(String, String, Option<String>)> {
    let mut result = Vec::new();

    for el in elements {
        let id = el
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if id.is_empty() {
            continue;
        }

        // Must have actions
        let actions = el
            .get("actions")
            .and_then(|v| v.as_array())
            .map(|a| a.len())
            .unwrap_or(0);
        if actions == 0 {
            continue;
        }

        // Must be visible and enabled
        let visible = el
            .get("state")
            .and_then(|s| s.get("visible"))
            .and_then(|v| v.as_bool())
            .or_else(|| el.get("visible").and_then(|v| v.as_bool()))
            .unwrap_or(false);
        let enabled = el
            .get("state")
            .and_then(|s| s.get("enabled"))
            .and_then(|v| v.as_bool())
            .or_else(|| el.get("enabled").and_then(|v| v.as_bool()))
            .unwrap_or(true);

        if !visible || !enabled {
            continue;
        }

        // Get fingerprint hash
        let fp_hash = element_map.get(&id).cloned();

        // Skip global zones
        if let Some(ref hash) = fp_hash {
            // Compute zone from bounds
            let zone = {
                let bounds = el
                    .get("state")
                    .and_then(|s| s.get("rect"))
                    .or_else(|| el.get("bounds"));
                if let Some(b) = bounds {
                    let _x = b.get("x").and_then(|v| v.as_f64()).unwrap_or(0.0);
                    let y = b.get("y").and_then(|v| v.as_f64()).unwrap_or(0.0);
                    let _w = b.get("width").and_then(|v| v.as_f64()).unwrap_or(0.0);
                    let h = b.get("height").and_then(|v| v.as_f64()).unwrap_or(0.0);
                    let center_y = (y + h / 2.0) / fp_config.viewport_height;
                    if center_y < 0.12 {
                        "header"
                    } else if center_y > 0.88 {
                        "footer"
                    } else {
                        "main"
                    }
                } else {
                    "main"
                }
            };

            if EXPLORATION_SKIP_ZONES.contains(&zone) {
                continue;
            }

            // Skip already interacted by fingerprint
            if interacted_fps.contains(hash) {
                continue;
            }
        }

        // Skip already interacted by ID
        if interacted_ids.contains(&id) {
            continue;
        }

        let label = el
            .get("accessibleName")
            .or_else(|| el.get("label"))
            .and_then(|v| v.as_str())
            .or_else(|| {
                el.get("state")
                    .and_then(|s| s.get("textContent"))
                    .and_then(|v| v.as_str())
            })
            .unwrap_or(&id)
            .to_string();

        // Skip elements matching blocked keywords (safety: avoid "Delete", "Logout", etc.)
        if !blocked_keywords.is_empty() {
            let label_lower = label.to_lowercase();
            if blocked_keywords
                .iter()
                .any(|kw| label_lower.contains(&kw.to_lowercase()))
            {
                continue;
            }
        }

        result.push((id, label, fp_hash));
    }

    result
}

/// Find a "back" navigation element.
fn find_back_navigation(elements: &[Value]) -> Option<(String, String)> {
    let back_keywords = [
        "back", "close", "cancel", "return", "dismiss", "go back", "previous", "exit",
    ];

    for el in elements {
        let id = el.get("id").and_then(|v| v.as_str()).unwrap_or("");
        if id.is_empty() {
            continue;
        }

        let name = el
            .get("accessibleName")
            .or_else(|| el.get("label"))
            .and_then(|v| v.as_str())
            .or_else(|| {
                el.get("state")
                    .and_then(|s| s.get("textContent"))
                    .and_then(|v| v.as_str())
            })
            .unwrap_or("")
            .to_lowercase();

        let has_actions = el
            .get("actions")
            .and_then(|v| v.as_array())
            .map(|a| !a.is_empty())
            .unwrap_or(false);

        if !has_actions {
            continue;
        }

        for keyword in &back_keywords {
            if name.contains(keyword) {
                return Some((id.to_string(), name));
            }
        }
    }

    None
}
