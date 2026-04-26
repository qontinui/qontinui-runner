//! Phase F.1 — Wake-from-web deep-link handler.
//!
//! qontinui-web emits `qontinui://wake?intent=<id>&task_id=<id>` URLs when a
//! scheduled task fires while no live runner is connected. The OS hands the URL
//! to the runner via the registered custom protocol; the deep-link plugin
//! delivers the URL into this handler.
//!
//! When a wake intent arrives we:
//!  1. Parse the URL and extract `intent` + optional `task_id` query params.
//!  2. Bring the runner window forward (`window.unminimize` + `set_focus`) so
//!     the user immediately sees the runner has woken.
//!  3. Fire an immediate `SchedulerService::tick()` so any task whose `next_run`
//!     fell into the past while the runner was offline runs now instead of
//!     waiting up to `check_interval_secs` (60s default) for the next loop.
//!
//! The Phase F.2 backend closes the loop separately: when the woken runner
//! re-registers on the qontinui-web WebSocket, the backend sees the matching
//! `wake_intent:{user_id}:{intent_id}` Redis key and clears it, emitting a
//! `runner.woke` event the frontend uses to advance the dispatch flow. We
//! deliberately do NOT POST a heartbeat from here — the WS reconnection that
//! happens automatically once the runner comes back online is the canonical
//! signal, and adding a parallel HTTP confirmation would just create two
//! sources of truth that can drift.

use std::collections::HashMap;
use std::sync::Arc;

use tauri::{AppHandle, Manager};
use tracing::{info, warn};
use url::Url;

use crate::scheduler_service::get_scheduler_service;

/// Deep-link scheme this runner answers to. Mirror of
/// `tauri.conf.json -> plugins.deep-link.desktop.schemes[0]`.
pub const SCHEME: &str = "qontinui";

/// Recognised wake-URL host. `qontinui://wake?intent=...` — anything else is
/// logged and ignored (forward-compat: future schemes like
/// `qontinui://open-task/<id>` won't accidentally trigger a tick).
pub const WAKE_HOST: &str = "wake";

/// Public entry point invoked by the deep-link plugin's `on_open_url` callback
/// for every URL the OS hands to the runner. May receive multiple URLs in one
/// burst (e.g. cold-start with queued URLs); we dispatch each independently.
pub fn handle_deep_link_urls<I, S>(app: AppHandle, urls: I)
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    for url_str in urls {
        let url_str = url_str.as_ref();
        match Url::parse(url_str) {
            Ok(url) => {
                if url.scheme() != SCHEME {
                    warn!(
                        "Deep-link URL with unknown scheme '{}' ignored: {}",
                        url.scheme(),
                        url_str
                    );
                    continue;
                }
                handle_url(app.clone(), &url, url_str);
            }
            Err(err) => {
                warn!("Failed to parse deep-link URL '{}': {}", url_str, err);
            }
        }
    }
}

/// Dispatch a single parsed `qontinui://...` URL.
fn handle_url(app: AppHandle, url: &Url, raw: &str) {
    // `Url::host_str()` returns the host segment for `qontinui://wake?...`. For
    // schemes the parser doesn't know to be hierarchical it can land in `path`
    // instead — handle both forms so we don't drop the URL on a parser quirk.
    let target = url
        .host_str()
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| url.path().trim_start_matches('/').to_string());

    if target.eq_ignore_ascii_case(WAKE_HOST) {
        let params: HashMap<String, String> = url
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        let intent = params.get("intent").cloned();
        let task_id = params.get("task_id").cloned();

        info!(
            "Received wake deep-link (intent={:?}, task_id={:?}, raw={})",
            intent, task_id, raw
        );

        focus_main_window(&app);

        // Fire scheduler tick on the Tauri async runtime so we return from the
        // deep-link callback immediately. The handler runs on Tauri's event
        // thread and we must not block it.
        tauri::async_runtime::spawn(async move {
            trigger_scheduler_tick(intent, task_id).await;
        });
    } else {
        warn!(
            "Deep-link URL with unknown host/path '{}' ignored: {}",
            target, raw
        );
    }
}

/// Bring the main runner window to the foreground. The wake URL implies the
/// user (or an automated dispatch from qontinui-web) wants the runner to act
/// *now*, so an unminimize + focus gives a visible signal that the wake landed.
fn focus_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(qontinui_runner_lib::get_main_window_label()) {
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

/// Trigger an immediate scheduler tick. Logs but does not propagate failures —
/// the regular scheduler heartbeat will pick the task up on its next cycle.
async fn trigger_scheduler_tick(intent: Option<String>, task_id: Option<String>) {
    let service: Option<Arc<crate::scheduler_service::SchedulerService>> =
        get_scheduler_service().await;
    let Some(service) = service else {
        warn!(
            "Wake deep-link received but scheduler service is not running \
             (intent={:?}, task_id={:?}); the runner will pick up due tasks on its \
             next regular heartbeat.",
            intent, task_id
        );
        return;
    };

    info!(
        "Wake deep-link triggering immediate scheduler tick (intent={:?}, task_id={:?})",
        intent, task_id
    );
    service.tick().await;
    info!(
        "Wake-triggered scheduler tick complete (intent={:?}, task_id={:?})",
        intent, task_id
    );
}

/// Helper invoked by the single-instance plugin closure when a *second* runner
/// process is launched with deep-link arguments. The single-instance plugin's
/// `deep-link` cargo feature scans `argv` for URLs matching the registered
/// scheme and forwards them here, so the running primary instance handles the
/// wake URL instead of starting a duplicate process.
pub fn handle_args_from_secondary_instance(app: AppHandle, argv: &[String]) {
    let urls: Vec<&str> = argv
        .iter()
        .filter(|arg| {
            let lower = arg.to_ascii_lowercase();
            lower.starts_with(&format!("{}://", SCHEME))
        })
        .map(|s| s.as_str())
        .collect();

    if urls.is_empty() {
        return;
    }

    info!(
        "Single-instance forwarded {} deep-link URL(s) from secondary launch",
        urls.len()
    );
    handle_deep_link_urls(app, urls);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_wake_url_with_query_params() {
        let url = Url::parse("qontinui://wake?intent=abc&task_id=xyz").unwrap();
        assert_eq!(url.scheme(), SCHEME);
        assert_eq!(url.host_str(), Some(WAKE_HOST));
        let params: HashMap<String, String> = url
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        assert_eq!(params.get("intent").map(String::as_str), Some("abc"));
        assert_eq!(params.get("task_id").map(String::as_str), Some("xyz"));
    }

    #[test]
    fn ignores_non_wake_host() {
        // Forward-compat: an unknown host like `qontinui://open-task` must not
        // get routed to the wake handler.
        let url = Url::parse("qontinui://open-task/abc").unwrap();
        assert_ne!(url.host_str(), Some(WAKE_HOST));
    }

    #[test]
    fn detects_deep_link_args_from_argv() {
        let argv = vec![
            "C:\\path\\to\\runner.exe".to_string(),
            "qontinui://wake?intent=abc".to_string(),
            "--unrelated-flag".to_string(),
        ];
        let urls: Vec<&str> = argv
            .iter()
            .filter(|arg| arg.to_ascii_lowercase().starts_with("qontinui://"))
            .map(|s| s.as_str())
            .collect();
        assert_eq!(urls.len(), 1);
        assert_eq!(urls[0], "qontinui://wake?intent=abc");
    }
}
