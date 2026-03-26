//! Background Capture Service — continuous screen observation for the activity timeline.
//!
//! Screenpipe-inspired event-driven capture for black-box desktop apps. Runs as a
//! background tokio task that captures screen state on meaningful events (focus change,
//! idle timer) and writes results to the activity timeline via PgDb.
//!
//! For white-box UI Bridge apps, use `BackgroundObserver` (TypeScript) instead.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{info, warn};

use crate::database::pg::PgDb;
use crate::database::types::ActivityTimelineInput;

/// Configuration for the background capture service.
#[derive(Debug, Clone)]
pub struct BackgroundCaptureConfig {
    /// Capture at least every N seconds (idle fallback timer).
    pub capture_interval_secs: u64,
    /// Capture when window focus changes (detected by Python bridge).
    pub capture_on_focus_change: bool,
    /// Monitor index for capture (None = primary).
    pub monitor_index: Option<i32>,
}

impl Default for BackgroundCaptureConfig {
    fn default() -> Self {
        Self {
            capture_interval_secs: 30,
            capture_on_focus_change: true,
            monitor_index: None,
        }
    }
}

/// Result from a single capture cycle (returned from Python bridge).
#[derive(Debug, Clone, serde::Deserialize)]
pub struct CaptureResult {
    pub text_content: String,
    pub content_hash: String,
    pub source_type: String,
    pub element_count: i32,
    pub confidence: f64,
    pub app_name: Option<String>,
    pub window_title: Option<String>,
    pub url: Option<String>,
    pub screenshot_path: Option<String>,
}

/// Background capture service that runs in a tokio task.
///
/// The service coordinates with the Python bridge (PairedCaptureService + EventMonitor)
/// to perform actual captures, then stores results in the activity timeline.
pub struct BackgroundCaptureService {
    pg: Arc<PgDb>,
    config: BackgroundCaptureConfig,
    stop_signal: Arc<AtomicBool>,
    last_content_hash: Arc<Mutex<Option<String>>>,
}

impl BackgroundCaptureService {
    /// Create a new background capture service.
    pub fn new(pg: Arc<PgDb>, config: BackgroundCaptureConfig) -> Self {
        Self {
            pg,
            config,
            stop_signal: Arc::new(AtomicBool::new(false)),
            last_content_hash: Arc::new(Mutex::new(None)),
        }
    }

    /// Signal the service to stop.
    pub fn stop(&self) {
        self.stop_signal.store(true, Ordering::SeqCst);
    }

    /// Whether the service has been signaled to stop.
    pub fn is_stopped(&self) -> bool {
        self.stop_signal.load(Ordering::SeqCst)
    }

    /// Run the background capture loop.
    ///
    /// This method runs until `stop()` is called. It should be spawned in a tokio task.
    /// The `capture_fn` is called on each tick to perform the actual capture via the
    /// Python bridge. This indirection allows testing without the Python bridge.
    pub async fn run<F, Fut>(&self, capture_fn: F)
    where
        F: Fn() -> Fut,
        Fut: std::future::Future<Output = Option<CaptureResult>>,
    {
        info!(
            "BackgroundCaptureService started (interval={}s, focus_change={})",
            self.config.capture_interval_secs, self.config.capture_on_focus_change
        );

        let interval = tokio::time::Duration::from_secs(self.config.capture_interval_secs);

        loop {
            if self.stop_signal.load(Ordering::SeqCst) {
                info!("BackgroundCaptureService stopping");
                break;
            }

            tokio::time::sleep(interval).await;

            if self.stop_signal.load(Ordering::SeqCst) {
                break;
            }

            // Request capture from Python bridge
            let result = capture_fn().await;

            if let Some(capture) = result {
                self.store_capture(capture).await;
            }
        }

        info!("BackgroundCaptureService stopped");
    }

    /// Store a capture result in the activity timeline, with content-hash dedup.
    async fn store_capture(&self, capture: CaptureResult) {
        // Skip if content unchanged
        {
            let mut last_hash = self.last_content_hash.lock().await;
            if let Some(ref prev) = *last_hash {
                if prev == &capture.content_hash {
                    return;
                }
            }
            *last_hash = Some(capture.content_hash.clone());
        }

        let input = ActivityTimelineInput {
            text_content: capture.text_content,
            source_type: capture.source_type,
            capture_mode: "black_box".to_string(),
            app_name: capture.app_name,
            window_title: capture.window_title,
            url: capture.url,
            task_run_id: None,
            screenshot_path: capture.screenshot_path,
            element_count: Some(capture.element_count),
            confidence: Some(capture.confidence),
            metadata_json: None,
        };

        match self.pg.insert_activity_entry(&input).await {
            Ok(id) => {
                info!("Background capture stored: timeline entry {}", id);
            }
            Err(e) => {
                warn!("Background capture failed to store: {}", e);
            }
        }
    }
}
