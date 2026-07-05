//! Helper-answer poll loop (plan `2026-06-29-helper-task-queue`, Phase 1.3
//! Part D).
//!
//! Periodically GETs `GET /coord/helper-tasks/answers?since=<cursor>` with
//! the device JWT (same auth + coord-base resolution posture as
//! `mcp::session_message_poller`), folds returned answers into the persisted
//! store (`super::record_answers`), and persists the cursor to a small JSON
//! file next to `settings.json` so a restart resumes where it left off.
//!
//! Safety rails (mirroring the sibling pollers):
//! - **Owner-gated, with an outstanding-work carve-out.** The tick runs when
//!   `settings.helper_tasks.emit_enabled` is on OR the store already knows
//!   about tasks/answers — pausing emission must not stop collecting answers
//!   to tasks that are already outstanding. A runner that never emitted
//!   anything (empty store, emit off) still adds zero always-on coord
//!   traffic.
//! - **Device-JWT-gated.** Unpaired ⇒ skip quietly.
//! - **503-tolerant.** Coord maps un-migrated helper-task tables to 503 —
//!   treated as "feature not available yet", debug-logged, never an error.
//! - **Fail-open.** Any error is logged and the loop continues; spawned from
//!   `mcp_api.rs` as a plain background task.

use std::time::Duration;

use qontinui_types::helper_task::HelperAnswer;
use tracing::{debug, info, warn};

const POLL_INTERVAL: Duration = Duration::from_secs(60);

/// Cursor file co-located with `settings.json` (honors the per-instance
/// `QONTINUI_CONFIG_DIR` override, so temp runners don't share the primary's
/// cursor).
const CURSOR_FILE: &str = "helper-answers-cursor.json";

/// Overlap window subtracted from the persisted cursor when polling. Coord
/// filters strictly `created_at > since` while the cursor is the newest
/// `created_at` the poller has SEEN — an answer committed concurrently with a
/// poll and stamped ≤ cursor would otherwise be skipped forever. Re-reading a
/// 2s boundary window is idempotent: the store dedups by answer id and only
/// replaces on a strictly newer `created_at` (revision rule).
const CURSOR_OVERLAP_SECS: i64 = 2;

/// Run the poll loop forever. Spawned once at API boot; every failure mode is
/// contained inside the tick.
pub async fn run_forever() {
    info!(
        "helper_tasks poller started (interval={}s, active when settings.helper_tasks.emit_enabled OR the answer store is non-empty)",
        POLL_INTERVAL.as_secs()
    );
    loop {
        if let Err(e) = tick().await {
            warn!("helper_tasks poller: tick failed (will retry): {e}");
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

async fn tick() -> Result<(), String> {
    // Owner gate — default OFF — but only for a runner with nothing
    // outstanding: pausing emission must not stop collecting answers to tasks
    // already emitted, so a non-empty store keeps the poll alive. Only the
    // EMIT side keys on the toggle.
    let cfg = crate::settings::get_helper_tasks_settings();
    if !cfg.emit_enabled && !super::store_has_content() {
        return Ok(());
    }

    // Device JWT — unpaired ⇒ skip the tick quietly (no spam).
    let token = match crate::auth::AuthManager::new().get_access_token() {
        Ok(t) if !t.trim().is_empty() => t.trim().to_string(),
        _ => {
            debug!("helper_tasks poller: no device JWT yet (unpaired) — skipping tick");
            return Ok(());
        }
    };

    let base = crate::mcp::agent_worktrees::coord_http_base()
        .map_err(|e| format!("coord base unresolved: {e}"))?;
    let base = base.trim_end_matches('/');

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| format!("http client init: {e}"))?;

    // Build the query manually: this workspace's reqwest feature set does not
    // include `query`, and the cursor is a single RFC 3339 value anyway.
    let url = match load_cursor() {
        Some(cursor) => format!(
            "{base}/coord/helper-tasks/answers?since={}",
            percent_encode(&overlapped_since(&cursor))
        ),
        None => format!("{base}/coord/helper-tasks/answers"),
    };

    let resp = client
        .get(&url)
        .bearer_auth(&token)
        .send()
        .await
        .map_err(|e| format!("GET {url}: {e}"))?;
    let status = resp.status();
    if status == reqwest::StatusCode::SERVICE_UNAVAILABLE {
        // Helper-task tables not migrated coord-side — feature not available
        // yet. Non-fatal by design.
        debug!("helper_tasks poller: coord returned 503 (tables not migrated) — skipping tick");
        return Ok(());
    }
    if !status.is_success() {
        return Err(format!("GET {url} -> HTTP {status}"));
    }

    // Oldest-first per the coord contract; the LAST element carries the
    // newest `created_at`, which becomes the next cursor. NOTE: coord's
    // filter is strictly EXCLUSIVE (`created_at > since`) — the
    // CURSOR_OVERLAP_SECS window above is what protects the boundary.
    let answers: Vec<HelperAnswer> = resp
        .json()
        .await
        .map_err(|e| format!("answers decode: {e}"))?;
    if answers.is_empty() {
        return Ok(());
    }

    let next_cursor = answers
        .last()
        .map(|a| a.created_at.clone())
        .unwrap_or_default();
    let count = answers.len();
    super::record_answers(answers);
    if !next_cursor.is_empty() {
        save_cursor(&next_cursor);
    }
    info!("helper_tasks poller: collected {count} helper answer(s) (cursor -> {next_cursor})");
    Ok(())
}

/// Minimal RFC 3986 percent-encoding for a query VALUE: everything outside
/// the unreserved set (`A-Z a-z 0-9 - . _ ~`) is `%XX`-escaped. Covers the
/// `:` / `+` an RFC 3339 timestamp can carry without pulling in a crate.
fn percent_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for b in value.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Subtract [`CURSOR_OVERLAP_SECS`] from an RFC 3339 cursor. Falls back to
/// the raw cursor when it doesn't parse (coord then applies its strict `>`
/// against the value we stored — same behavior as before the overlap).
fn overlapped_since(cursor: &str) -> String {
    match chrono::DateTime::parse_from_rfc3339(cursor) {
        Ok(dt) => (dt - chrono::Duration::seconds(CURSOR_OVERLAP_SECS)).to_rfc3339(),
        Err(_) => cursor.to_string(),
    }
}

fn cursor_path() -> Option<std::path::PathBuf> {
    crate::settings::get_config_dir()
        .ok()
        .map(|d| d.join(CURSOR_FILE))
}

/// Load the persisted `since` cursor (RFC 3339). `None` on first run or any
/// read/parse failure — the first poll then fetches from the beginning, and
/// the store's id-dedup absorbs any overlap.
fn load_cursor() -> Option<String> {
    let path = cursor_path()?;
    let raw = std::fs::read_to_string(path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    v.get("since")
        .and_then(|s| s.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Persist the cursor atomically. Best-effort — a write failure only means
/// the next restart re-fetches a window the dedup already absorbs.
fn save_cursor(since: &str) {
    let Some(path) = cursor_path() else {
        return;
    };
    let body = serde_json::json!({ "since": since }).to_string();
    if let Err(e) = crate::fs_atomic::atomic_write(&path, body.as_bytes()) {
        warn!("helper_tasks poller: cursor persist failed (best-effort): {e}");
    }
}
