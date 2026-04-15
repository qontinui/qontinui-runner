//! Cloud registry poller — discovers devices registered with the qontinui.io relay backend.
//!
//! Polls `GET /api/v1/device-bridge/available-devices` at a configurable interval,
//! then sends the results through an async channel for consumption by the device registry.

use serde::{Deserialize, Serialize};
use std::time::Duration;
use tokio::sync::{mpsc, watch};
use tracing::{debug, error, info, warn};

// ============================================================================
// Types
// ============================================================================

/// Information about a cloud-registered device returned by the relay backend.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudDeviceInfo {
    pub device_id: String,
    pub display_name: String,
    pub platform: String,
    pub app_id: String,
    pub connected_since: i64,
    pub relay_session_id: String,
}

// ============================================================================
// CloudRegistryPoller
// ============================================================================

/// Polls the qontinui.io backend for cloud-registered devices.
pub struct CloudRegistryPoller {
    backend_url: String,
    poll_interval: Duration,
}

impl CloudRegistryPoller {
    /// Create a new poller.
    ///
    /// * `backend_url` — base URL of the qontinui.io backend (e.g. `https://api.qontinui.io`)
    /// * `poll_interval_secs` — how often to poll, in seconds
    pub fn new(backend_url: String, poll_interval_secs: u64) -> Self {
        Self {
            backend_url,
            poll_interval: Duration::from_secs(poll_interval_secs),
        }
    }

    /// Perform a single poll against the backend.
    ///
    /// Issues `GET {backend_url}/api/v1/device-bridge/available-devices` with a
    /// `Bearer {auth_token}` header, then parses the JSON response as
    /// `Vec<CloudDeviceInfo>`.
    pub async fn poll(&self, auth_token: &str) -> Result<Vec<CloudDeviceInfo>, reqwest::Error> {
        let url = format!(
            "{}/api/v1/device-bridge/available-devices",
            self.backend_url.trim_end_matches('/')
        );

        debug!("[cloud-registry] Polling {}", url);

        let client = reqwest::Client::new();
        let resp = client.get(&url).bearer_auth(auth_token).send().await?;

        // Treat non-2xx as an error via response error handling
        let resp = resp.error_for_status()?;

        // The backend wraps the list in `{ "devices": [...] }` or returns the
        // array directly. Try to unwrap either form.
        let json: serde_json::Value = resp.json().await?;

        let devices = if let Some(arr) = json.as_array() {
            serde_json::from_value(serde_json::Value::Array(arr.clone())).unwrap_or_default()
        } else if let Some(arr) = json.get("devices").and_then(|v| v.as_array()) {
            serde_json::from_value(serde_json::Value::Array(arr.clone())).unwrap_or_default()
        } else {
            Vec::new()
        };

        debug!("[cloud-registry] Found {} cloud device(s)", devices.len());

        Ok(devices)
    }

    /// Start a background polling task.
    ///
    /// * `auth_token_provider` — called before each poll to retrieve the current auth token.
    ///   If it returns `None` the poll is skipped for that cycle.
    /// * `event_tx` — channel where polled results are sent.
    /// * `shutdown` — watch receiver; task exits when the value becomes `true`.
    pub fn start_polling(
        self,
        auth_token_provider: impl Fn() -> Option<String> + Send + 'static,
        event_tx: mpsc::Sender<Vec<CloudDeviceInfo>>,
        mut shutdown: watch::Receiver<bool>,
    ) {
        tokio::spawn(async move {
            info!(
                "[cloud-registry] Background poller started (interval {:?})",
                self.poll_interval
            );

            loop {
                // Check for shutdown first
                if *shutdown.borrow() {
                    info!("[cloud-registry] Shutdown flag set — stopping poller");
                    break;
                }

                // Retrieve token
                if let Some(token) = auth_token_provider() {
                    match self.poll(&token).await {
                        Ok(devices) => {
                            if event_tx.send(devices).await.is_err() {
                                warn!("[cloud-registry] Event channel closed — stopping poller");
                                break;
                            }
                        }
                        Err(e) => {
                            warn!("[cloud-registry] Poll error: {e}");
                        }
                    }
                } else {
                    debug!("[cloud-registry] No auth token available, skipping poll");
                }

                // Wait for the next interval or a shutdown signal
                tokio::select! {
                    _ = tokio::time::sleep(self.poll_interval) => {}
                    _ = shutdown.changed() => {
                        if *shutdown.borrow() {
                            info!("[cloud-registry] Shutdown signal received — stopping poller");
                            break;
                        }
                    }
                }
            }

            info!("[cloud-registry] Background poller stopped");
        });
    }
}
