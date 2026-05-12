//! Auto-yield-on-idle policy for [`FileLockManager`].
//!
//! Implements §Open Q4 of the lock-yield-protocol-plan: if session A is
//! holding a file lock AND session A has been idle (no terminal stdout
//! activity) for at least `idle_threshold_secs` AND session B has been
//! waiting on the lock for at least `min_wait_secs`, the policy task
//! releases the lock on session A's behalf and emits a
//! `file-lock-auto-yielded` event (plus a matching `file-lock-released`
//! event so the existing `useFileLockTracking` listener clears state
//! without needing any frontend changes).
//!
//! Substrate:
//! - `FileLockManager::info_with_waiters` (this module, lock-yield Phase 1)
//! - `SessionManager::snapshot` (`claude_session::manager.rs:235`) —
//!   yields `(task_run_id, holder_name, Arc<AtomicU64>)` triples where
//!   the atomic stores epoch SECONDS of the last observed stdout line
//!   (`claude_session::session.rs:420`).
//! - `Settings::lock_yield_policy` — opt-in toggle + thresholds.
//!
//! The task runs at `POLL_INTERVAL_SECS` cadence forever; when the
//! policy is disabled the body short-circuits without scanning lock
//! state, so the cost is one settings load per tick.

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use tauri::{AppHandle, Emitter};
use tokio::sync::broadcast;
use tracing::info;

use crate::claude_session::manager::SessionManager;
use crate::executor::FileLockManager;
use crate::settings::LockYieldPolicySettings;

/// How often the policy task scans held locks. 5s is granular enough
/// that a user-visible auto-yield happens within a tick of the
/// (idle + wait) condition being met, while keeping the per-tick cost
/// trivial (one async read of the lock map + one settings load).
pub const POLL_INTERVAL_SECS: u64 = 5;

/// Pure predicate exposed for unit testing.
///
/// Returns `true` when the holder's idle time AND the oldest waiter's
/// wait time both meet the configured thresholds. The boundary check
/// is inclusive: `idle_secs >= threshold` and `waited_ms >= min * 1000`.
pub fn should_yield(
    holder_idle_secs: u64,
    waiter_waited_ms: u64,
    settings: &LockYieldPolicySettings,
    _now_ms: u64,
) -> bool {
    if !settings.enabled {
        return false;
    }
    if holder_idle_secs < settings.idle_threshold_secs {
        return false;
    }
    let min_wait_ms = settings.min_wait_secs.saturating_mul(1000);
    if waiter_waited_ms < min_wait_ms {
        return false;
    }
    true
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Spawn the auto-yield policy task. Returns immediately; the task
/// runs in the background for the lifetime of the runtime.
///
/// `app_handle` is used for the `app_handle.emit(...)` half of the
/// dual-emit (Tauri webview). `event_broadcast` is the SSE/headless
/// channel that mirrors the same payload.
///
/// Uses `tauri::async_runtime::spawn` (not bare `tokio::spawn`)
/// because this function is called from Tauri's synchronous `setup`
/// closure, before any user-side Tokio runtime is active.
/// `tauri::async_runtime::spawn` routes the future through Tauri's
/// own multi-threaded runtime, which is alive by the time `setup`
/// runs.
pub fn spawn(
    app_handle: AppHandle,
    file_lock_manager: Arc<FileLockManager>,
    session_manager: Arc<SessionManager>,
    event_broadcast: broadcast::Sender<serde_json::Value>,
) {
    tauri::async_runtime::spawn(async move {
        run_policy_loop(
            app_handle,
            file_lock_manager,
            session_manager,
            event_broadcast,
        )
        .await;
    });
}

async fn run_policy_loop(
    app_handle: AppHandle,
    file_lock_manager: Arc<FileLockManager>,
    session_manager: Arc<SessionManager>,
    event_broadcast: broadcast::Sender<serde_json::Value>,
) {
    let mut interval = tokio::time::interval(Duration::from_secs(POLL_INTERVAL_SECS));
    // Reset on every tick — `interval` defaults to "burst missed
    // ticks", which would spam yield events after a long PG migration
    // pause. We want strict one-tick-per-period regardless.
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        interval.tick().await;
        let settings = crate::settings::get_lock_yield_policy_settings();
        if !settings.enabled {
            continue;
        }

        let infos = file_lock_manager.info_with_waiters().await;
        if infos.is_empty() {
            continue;
        }

        let now_s = now_secs();
        let now_ms = now_millis();

        for entry in infos {
            // Skip uncontested locks — auto-yield is only relevant
            // when someone is actually blocked.
            let oldest = match entry.waiters.first() {
                Some(w) => w.clone(),
                None => continue,
            };

            // Look up the holder's last-activity tracker. PTY tabs
            // without a `task_run_id` in SessionManager are excluded
            // by `SessionManager::snapshot`, so we look up by
            // task_run_id directly.
            let holder_session = match session_manager.get(&entry.holder_task_run_id) {
                Some(s) => s,
                None => continue, // Holder is not a Claude session — can't measure idle.
            };
            let holder_last_activity_s = holder_session
                .last_activity_tracker()
                .load(Ordering::Relaxed);
            let holder_idle_secs = now_s.saturating_sub(holder_last_activity_s);
            let oldest_waiter_waited_ms = now_ms.saturating_sub(oldest.waiting_since_ms);

            if !should_yield(holder_idle_secs, oldest_waiter_waited_ms, &settings, now_ms) {
                continue;
            }

            // Release the lock on the holder's behalf, then emit BOTH
            // `file-lock-auto-yielded` (new event so observability /
            // banners can react specifically to auto-yield) and the
            // existing `file-lock-released` event (so the unchanged
            // `useFileLockTracking` listener clears state).
            file_lock_manager
                .release(&entry.file_path, &entry.holder_task_run_id)
                .await;

            let oldest_waiter_waited_secs = oldest_waiter_waited_ms / 1000;
            info!(
                "Auto-yielded file lock '{}' held by '{}' (idle {}s, waiter waited {}s)",
                entry.file_path, entry.holder_name, holder_idle_secs, oldest_waiter_waited_secs
            );

            let auto_yield_payload = json!({
                "type": "file-lock-auto-yielded",
                "file_path": &entry.file_path,
                "holder_task_run_id": &entry.holder_task_run_id,
                "holder_name": &entry.holder_name,
                "holder_idle_secs": holder_idle_secs,
                "oldest_waiter_task_run_id": &oldest.task_run_id,
                "oldest_waiter_waited_secs": oldest_waiter_waited_secs,
                "auto_yielded_at": now_ms,
            });
            let _ = app_handle.emit("file-lock-auto-yielded", &auto_yield_payload);
            let _ = event_broadcast.send(auto_yield_payload);

            // Mirror the released-event emit shape from
            // `mcp/file_registry.rs::yield_lock` (the manual yield
            // endpoint). Listener clears "X is holding Y" banners.
            let released_payload = json!({
                "type": "file-lock-released",
                "file_path": &entry.file_path,
                "task_run_id": &entry.holder_task_run_id,
                "holder_name": &entry.holder_name,
            });
            let _ = app_handle.emit("file-lock-released", &released_payload);
            let _ = event_broadcast.send(released_payload);
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(
        enabled: bool,
        idle_threshold_secs: u64,
        min_wait_secs: u64,
    ) -> LockYieldPolicySettings {
        LockYieldPolicySettings {
            enabled,
            idle_threshold_secs,
            min_wait_secs,
        }
    }

    #[test]
    fn yields_when_idle_and_wait_meet_thresholds() {
        let s = settings(true, 60, 30);
        // idle 90s, waited 31s.
        assert!(should_yield(90, 31_000, &s, 0));
    }

    #[test]
    fn does_not_yield_when_idle_below_threshold() {
        let s = settings(true, 60, 30);
        // idle 59s, waited well over the floor.
        assert!(!should_yield(59, 600_000, &s, 0));
    }

    #[test]
    fn does_not_yield_when_wait_below_threshold() {
        let s = settings(true, 60, 30);
        // idle 600s, waited only 29.999s.
        assert!(!should_yield(600, 29_999, &s, 0));
    }

    #[test]
    fn does_not_yield_when_disabled() {
        let s = settings(false, 60, 30);
        // Both thresholds met, but policy off.
        assert!(!should_yield(900, 900_000, &s, 0));
    }

    #[test]
    fn yields_at_inclusive_boundary_for_idle() {
        // idle == threshold should yield (inclusive).
        let s = settings(true, 60, 30);
        assert!(should_yield(60, 30_000, &s, 0));
    }

    #[test]
    fn yields_at_inclusive_boundary_for_wait() {
        // waited_ms == min_wait_secs * 1000 should yield.
        let s = settings(true, 60, 30);
        assert!(should_yield(120, 30_000, &s, 0));
    }

    #[test]
    fn does_not_yield_just_below_wait_boundary() {
        // 29_999 ms is one ms short of the 30s floor.
        let s = settings(true, 60, 30);
        assert!(!should_yield(120, 29_999, &s, 0));
    }

    #[test]
    fn zero_thresholds_yield_immediately_when_enabled() {
        // Edge case: with both thresholds at 0, any idle/wait satisfies.
        let s = settings(true, 0, 0);
        assert!(should_yield(0, 0, &s, 0));
        assert!(should_yield(1, 1, &s, 0));
    }

    #[test]
    fn saturating_arithmetic_on_min_wait_secs_max() {
        // u64::MAX * 1000 saturates; should not panic and should not yield
        // for any non-saturated waiter age.
        let s = LockYieldPolicySettings {
            enabled: true,
            idle_threshold_secs: 60,
            min_wait_secs: u64::MAX,
        };
        // Saturated min_wait_ms means any finite waited_ms is below the floor.
        assert!(!should_yield(600, 1_000_000_000, &s, 0));
    }
}
