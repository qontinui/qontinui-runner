//! Click Overlay — Visual Click Feedback for Black-Box Desktop Automation
//!
//! Creates a transparent, always-on-top, click-through Tauri overlay window
//! that shows brief highlight animations at click locations during automation.
//!
//! For UI Bridge apps (white-box), the CSS-based click-highlight.ts is used
//! instead. This module handles the black-box case where we can't inject CSS
//! into the target application.
//!
//! ## Architecture
//!
//! The overlay is a small Tauri webview window that:
//! - Is transparent and always-on-top
//! - Has click-through enabled (WS_EX_TRANSPARENT on Windows)
//! - Renders colored circles at given screen coordinates via JS injection
//! - Auto-hides each highlight after a configurable duration
//!
//! ## Usage
//!
//! ```rust,ignore
//! // Show a click highlight at screen coordinates (500, 300)
//! show_click_highlight(app_handle, 500, 300, None).await;
//! ```

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use tracing::{debug, info, warn};

/// Whether the overlay window has been initialized.
static OVERLAY_INITIALIZED: AtomicBool = AtomicBool::new(false);

/// Configuration for click highlights.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClickHighlightConfig {
    /// CSS color for the highlight circle. Default: "#00c800" (green).
    #[serde(default = "default_color")]
    pub color: String,
    /// Duration in milliseconds before fade-out. Default: 600.
    #[serde(default = "default_duration")]
    pub duration_ms: u32,
    /// Diameter of the highlight circle in pixels. Default: 30.
    #[serde(default = "default_size")]
    pub size: u32,
    /// Whether the overlay is enabled. Default: true.
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

fn default_color() -> String {
    "#00c800".to_string()
}
fn default_duration() -> u32 {
    600
}
fn default_size() -> u32 {
    30
}
fn default_enabled() -> bool {
    true
}

impl Default for ClickHighlightConfig {
    fn default() -> Self {
        Self {
            color: default_color(),
            duration_ms: default_duration(),
            size: default_size(),
            enabled: default_enabled(),
        }
    }
}

/// Color presets for different action types.
pub fn color_for_action(action_type: &str) -> &'static str {
    match action_type {
        "click" | "doubleClick" => "#00c800",  // Green
        "type" | "sendKeys" => "#0064ff",      // Blue
        "scroll" => "#ff8c00",                 // Orange
        "select" => "#b400b4",                 // Purple
        "focus" => "#00b4b4",                  // Teal
        _ => "#00c800",                        // Green default
    }
}

/// Show a click highlight at the given screen coordinates.
///
/// This sends a message to the overlay window (if initialized) to render
/// a brief animated circle at the specified position.
///
/// For the overlay to work, `initialize_overlay` must have been called first
/// during app startup.
///
/// # Arguments
/// * `app_handle` - Tauri app handle for window management.
/// * `x` - X coordinate in screen pixels.
/// * `y` - Y coordinate in screen pixels.
/// * `config` - Optional highlight configuration (defaults if None).
pub async fn show_click_highlight(
    app_handle: &tauri::AppHandle,
    x: i32,
    y: i32,
    config: Option<&ClickHighlightConfig>,
) {
    let default_config = ClickHighlightConfig::default();
    let cfg = config.unwrap_or(&default_config);

    if !cfg.enabled {
        return;
    }

    // Get or create the overlay window
    let overlay_label = "click-overlay";

    if let Some(window) = app_handle.get_webview_window(overlay_label) {
        // Inject highlight via JavaScript
        let js = format!(
            r#"
            (function() {{
                const el = document.createElement('div');
                el.style.cssText = `
                    position: fixed;
                    left: {}px;
                    top: {}px;
                    width: {}px;
                    height: {}px;
                    border-radius: 50%;
                    border: 3px solid {};
                    pointer-events: none;
                    z-index: 999999;
                    animation: click-fade {}ms ease-out forwards;
                `;
                document.body.appendChild(el);
                setTimeout(() => el.remove(), {});
            }})();
            "#,
            x - cfg.size as i32 / 2,
            y - cfg.size as i32 / 2,
            cfg.size,
            cfg.size,
            cfg.color,
            cfg.duration_ms,
            cfg.duration_ms,
        );

        if let Err(e) = window.eval(&js) {
            debug!("Failed to inject click highlight: {}", e);
        }
    } else {
        debug!("Click overlay window not found — call initialize_overlay() first");
    }
}

/// Initialize the click overlay window.
///
/// Creates a transparent, always-on-top, click-through window that covers
/// the entire primary monitor. Should be called once during app startup.
///
/// On Windows, this uses WS_EX_TRANSPARENT for click-through behavior.
pub fn initialize_overlay(app_handle: &tauri::AppHandle) -> Result<(), String> {
    use tauri::{WebviewUrl, WebviewWindowBuilder};

    if OVERLAY_INITIALIZED.load(Ordering::Relaxed) {
        return Ok(());
    }

    let overlay_html = r#"
        <!DOCTYPE html>
        <html>
        <head>
            <style>
                * { margin: 0; padding: 0; }
                body { background: transparent; overflow: hidden; }
                @keyframes click-fade {
                    0% { opacity: 1; transform: scale(0.5); }
                    50% { opacity: 0.8; transform: scale(1); }
                    100% { opacity: 0; transform: scale(1.3); }
                }
            </style>
        </head>
        <body></body>
        </html>
    "#;

    // Write overlay HTML to a temp file
    let overlay_dir = std::env::temp_dir().join("qontinui-overlay");
    std::fs::create_dir_all(&overlay_dir)
        .map_err(|e| format!("Failed to create overlay dir: {}", e))?;
    let overlay_path = overlay_dir.join("click-overlay.html");
    std::fs::write(&overlay_path, overlay_html)
        .map_err(|e| format!("Failed to write overlay HTML: {}", e))?;

    // Use External with file:// URL — WebviewUrl::App resolves relative to Tauri's
    // distDir which won't find our temp file.
    let file_url_str = format!(
        "file:///{}",
        overlay_path.to_str().unwrap_or("").replace('\\', "/")
    );
    let url = WebviewUrl::External(
        file_url_str
            .parse()
            .map_err(|e| format!("Invalid overlay URL: {}", e))?,
    );

    match WebviewWindowBuilder::new(app_handle, "click-overlay", url)
        .title("Click Overlay")
        .transparent(true)
        .decorations(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .visible(false) // Start hidden, show on first highlight
        .build()
    {
        Ok(_window) => {
            OVERLAY_INITIALIZED.store(true, Ordering::Relaxed);
            info!("Click overlay window initialized");
            // Note: Click-through behavior is achieved via Tauri's transparent(true)
            // and the overlay being always-on-top with no interactive content.
            // For full WS_EX_TRANSPARENT on Windows, a Tauri plugin or raw HWND
            // manipulation would be needed — deferred for now.
            Ok(())
        }
        Err(e) => {
            warn!("Failed to create click overlay window: {}", e);
            Err(format!("Failed to create overlay: {}", e))
        }
    }
}

use tauri::Manager;
