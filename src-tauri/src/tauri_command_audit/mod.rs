//! Bounded ring buffer of recently-fired Tauri command names + timestamps.
//!
//! **Purpose.** Provides an attestation surface for manual-test operators
//! verifying that React-frontend code paths invoked a non-allowlisted Tauri
//! command. The `2026-05-21` `/manual-test` session hit a verification gap on
//! `commands::auth::get_test_auto_login`: the command returns credentials so
//! it is intentionally **not** in the UI Bridge `invoke` allowlist, but
//! operators still needed a way to confirm that the React `AuthProvider`
//! mount-time invocation fired (so a tracing log gating Phase 8's
//! `test_auto_login_skipped` path could be reasoned about).
//!
//! This ring buffer records ONLY:
//!   - the command name (a `&'static str`-equivalent owned `String`)
//!   - the unix-millis timestamp at which the global Tauri
//!     `invoke_handler` shim saw the command dispatch
//!
//! **No args, no return values, no payload data.** Pure attestation. The
//! security argument that justified keeping `get_test_auto_login` off the
//! invoke allowlist (don't expose credentials) is preserved: we leak only
//! that a function with that name fired at some moment.
//!
//! The buffer is process-global, capped at [`MAX_ENTRIES`] (oldest dropped
//! on overflow), and exposed via the runner-only HTTP endpoint
//! `GET /ui-bridge/control/tauri-command-history` (see
//! `crate::mcp::ui_bridge::tauri_audit`).
//!
//! Lock-poisoning is treated as a best-effort drop: this is a debug /
//! attestation surface, not a correctness substrate, and we'd rather lose a
//! few entries than crash the IPC dispatch closure.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// Maximum number of command-fire events retained in the global ring buffer.
///
/// 100 chosen as a small constant: large enough to capture the bursty
/// mount-time IPC traffic of a fresh React shell load (~30-50 commands), but
/// small enough that an operator can `GET` the entire buffer without
/// pagination. Oldest entries are dropped on overflow.
const MAX_ENTRIES: usize = 100;

/// A single command-fire observation. Serialised verbatim into the
/// `/ui-bridge/control/tauri-command-history` response array.
#[derive(Clone, Debug, serde::Serialize)]
pub struct CommandFireEvent {
    /// The Tauri command name as seen by `Invoke.message.command()`.
    pub command: String,
    /// Wall-clock unix epoch millis at the time `record` was called.
    pub fired_at_unix_ms: u64,
}

/// Process-global ring buffer. `Mutex::new(VecDeque::new())` is `const`-stable
/// in modern Rust toolchains, so no `OnceLock`/lazy init is needed.
static BUFFER: Mutex<VecDeque<CommandFireEvent>> = Mutex::new(VecDeque::new());

/// Record a command-fire observation. Called from the `invoke_handler` shim
/// in `main.rs` for every IPC dispatch. Best-effort: lock-poisoning silently
/// drops the entry so a panicked test in another module never wedges the
/// production IPC dispatch closure.
pub fn record(command: &str) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let event = CommandFireEvent {
        command: command.to_string(),
        fired_at_unix_ms: now,
    };
    if let Ok(mut buf) = BUFFER.lock() {
        if buf.len() >= MAX_ENTRIES {
            buf.pop_front();
        }
        buf.push_back(event);
    }
    // Lock-poisoned: silently drop. Audit trail is best-effort.
}

/// Snapshot-query the buffer, applying optional `since` (strict `>` on
/// `fired_at_unix_ms`) and `command` (exact-equals on name) filters. Returns
/// owned clones so the caller can release the mutex immediately.
///
/// Both filters are `Option<>`: omit either to skip that filter axis.
pub fn query(since: Option<u64>, command: Option<&str>) -> Vec<CommandFireEvent> {
    let buf = match BUFFER.lock() {
        Ok(b) => b,
        Err(_) => return Vec::new(),
    };
    buf.iter()
        .filter(|e| since.map(|s| e.fired_at_unix_ms > s).unwrap_or(true))
        .filter(|e| command.map(|c| e.command == c).unwrap_or(true))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // BUFFER is process-global; the in-process unit tests below all use
    // unique command-name prefixes derived from the test fn name so concurrent
    // execution (cargo's default per-binary test harness) doesn't cross-
    // pollute results. Filtering by `command` in `query()` then isolates each
    // test's view.

    #[test]
    fn record_and_query_basic() {
        record("test_record_and_query_basic_cmd_a");
        record("test_record_and_query_basic_cmd_b");
        let results = query(None, Some("test_record_and_query_basic_cmd_a"));
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].command, "test_record_and_query_basic_cmd_a");
    }

    #[test]
    fn ring_buffer_bound() {
        // Spray MAX_ENTRIES * 1.5 events with a unique command name; the
        // buffer caps total entries at MAX_ENTRIES across all commands, so our
        // filtered view is bounded by MAX_ENTRIES.
        let cmd = "test_ring_buffer_bound_cmd";
        for _ in 0..(MAX_ENTRIES + 50) {
            record(cmd);
        }
        let results = query(None, Some(cmd));
        assert!(
            results.len() <= MAX_ENTRIES,
            "buffer over-cap: {} > {}",
            results.len(),
            MAX_ENTRIES
        );
    }

    #[test]
    fn since_filter() {
        let cmd = "test_since_filter_cmd";
        let t_before = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        // 2ms sleeps put the record cleanly between the timestamp pair on
        // realistic clock resolutions (Windows ~15ms, Linux ~1ms).
        std::thread::sleep(std::time::Duration::from_millis(2));
        record(cmd);
        std::thread::sleep(std::time::Duration::from_millis(2));
        let t_after = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        let results_before = query(Some(t_before), Some(cmd));
        let results_after = query(Some(t_after), Some(cmd));
        assert_eq!(
            results_before.len(),
            1,
            "expected since=t_before to include our record"
        );
        assert_eq!(
            results_after.len(),
            0,
            "expected since=t_after to exclude our record (strict >)"
        );
    }

    #[test]
    fn no_filters_returns_at_least_recent() {
        // Sanity: with both filters None, at least our just-recorded entry is
        // present (other tests may have added entries too — we just need our
        // one back).
        let cmd = "test_no_filters_returns_at_least_recent_cmd";
        record(cmd);
        let results = query(None, None);
        assert!(
            results.iter().any(|e| e.command == cmd),
            "no_filters query missed our just-recorded event"
        );
    }
}
