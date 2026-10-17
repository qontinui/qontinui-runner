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

/// A bounded ring of command-fire observations.
///
/// The production surface owns exactly ONE instance ([`BUFFER`]); the public
/// [`record`] / [`query`] functions are one-line delegations to it. Tests
/// construct their own private ring, so a sibling test flooding the ring
/// (`ring_buffer_bound` records `MAX_ENTRIES + 50` entries) can never evict
/// another test's record. That eviction is what turned `since_filter` red in
/// the suite and green alone (plan
/// `2026-09-21-interleave-census-residue-five-more-suite-only-sites-a-tmpdir-substring-assertion-and-a-cross-process-class`
/// Phase 2) — the same per-test-handle shape as `outbound_trace`'s ring.
pub struct CommandAuditRing {
    entries: Mutex<VecDeque<CommandFireEvent>>,
}

impl CommandAuditRing {
    /// `const` so the one production instance needs no lazy init.
    pub const fn new() -> Self {
        Self {
            entries: Mutex::new(VecDeque::new()),
        }
    }

    /// Record a command-fire observation at the current wall-clock time.
    /// Best-effort: lock-poisoning silently drops the entry so a panicked
    /// test in another module never wedges the production IPC dispatch
    /// closure.
    pub fn record_in(&self, command: &str) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let event = CommandFireEvent {
            command: command.to_string(),
            fired_at_unix_ms: now,
        };
        if let Ok(mut buf) = self.entries.lock() {
            if buf.len() >= MAX_ENTRIES {
                buf.pop_front();
            }
            buf.push_back(event);
        }
        // Lock-poisoned: silently drop. Audit trail is best-effort.
    }

    /// Snapshot-query the ring, applying optional `since` (strict `>` on
    /// `fired_at_unix_ms`) and `command` (exact-equals on name) filters.
    /// Returns owned clones so the caller can release the mutex immediately.
    pub fn query_in(&self, since: Option<u64>, command: Option<&str>) -> Vec<CommandFireEvent> {
        let buf = match self.entries.lock() {
            Ok(b) => b,
            Err(_) => return Vec::new(),
        };
        buf.iter()
            .filter(|e| since.map(|s| e.fired_at_unix_ms > s).unwrap_or(true))
            .filter(|e| command.map(|c| e.command == c).unwrap_or(true))
            .cloned()
            .collect()
    }
}

impl Default for CommandAuditRing {
    fn default() -> Self {
        Self::new()
    }
}

/// The process-global ring the `invoke_handler` shim in `main.rs` feeds and
/// `GET /ui-bridge/control/tauri-command-history` reads. The ONLY instance
/// outside tests.
static BUFFER: CommandAuditRing = CommandAuditRing::new();

/// Record a command-fire observation. Called from the `invoke_handler` shim
/// in `main.rs` for every IPC dispatch.
pub fn record(command: &str) {
    BUFFER.record_in(command)
}

/// Query the process-global ring. Both filters are `Option<>`: omit either
/// to skip that filter axis.
pub fn query(since: Option<u64>, command: Option<&str>) -> Vec<CommandFireEvent> {
    BUFFER.query_in(since, command)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Every test owns a private `CommandAuditRing`, so no test can evict or
    // observe another's records. Only `the_public_api_only_delegates` reads
    // this file's source, to pin that the production functions stay one-line
    // delegations to the single static.

    #[test]
    fn record_and_query_basic() {
        let ring = CommandAuditRing::new();
        ring.record_in("cmd_a");
        ring.record_in("cmd_b");
        let results = ring.query_in(None, Some("cmd_a"));
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].command, "cmd_a");
    }

    #[test]
    fn ring_buffer_bound() {
        // Spray MAX_ENTRIES * 1.5 events; the ring keeps exactly the newest
        // MAX_ENTRIES.
        let ring = CommandAuditRing::new();
        for i in 0..(MAX_ENTRIES + 50) {
            ring.record_in(&format!("cmd_{i}"));
        }
        let results = ring.query_in(None, None);
        assert_eq!(results.len(), MAX_ENTRIES);
        assert_eq!(
            results[0].command, "cmd_50",
            "oldest entries are dropped first"
        );
        assert_eq!(
            results[MAX_ENTRIES - 1].command,
            format!("cmd_{}", MAX_ENTRIES + 49)
        );
    }

    #[test]
    fn since_filter() {
        let ring = CommandAuditRing::new();
        let cmd = "since_filter_cmd";
        let t_before = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        // 2ms sleeps put the record cleanly between the timestamp pair on
        // realistic clock resolutions (Windows ~15ms, Linux ~1ms).
        std::thread::sleep(std::time::Duration::from_millis(2));
        ring.record_in(cmd);
        std::thread::sleep(std::time::Duration::from_millis(2));
        let t_after = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        let results_before = ring.query_in(Some(t_before), Some(cmd));
        let results_after = ring.query_in(Some(t_after), Some(cmd));
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
    fn no_filters_returns_everything_recorded() {
        let ring = CommandAuditRing::new();
        ring.record_in("one");
        ring.record_in("two");
        let names: Vec<String> = ring
            .query_in(None, None)
            .into_iter()
            .map(|e| e.command)
            .collect();
        assert_eq!(names, vec!["one".to_string(), "two".to_string()]);
    }

    #[test]
    fn the_public_api_only_delegates_to_the_one_static() {
        // A per-test ring proves nothing if the production functions stop
        // routing through `BUFFER`, or if a second static ring appears beside
        // it. Pin both against this file's own source.
        let prod = crate::source_pin::ProdSource::of(include_str!("mod.rs"));
        let compact: String = prod.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(
            compact.contains("pub fn record(command: &str) { BUFFER.record_in(command) }"),
            "`record` must be a one-line delegation to BUFFER.record_in"
        );
        assert!(
            compact.contains(
                "pub fn query(since: Option<u64>, command: Option<&str>) -> Vec<CommandFireEvent> { BUFFER.query_in(since, command) }"
            ),
            "`query` must be a one-line delegation to BUFFER.query_in"
        );
        let statics = prod
            .lines()
            .filter(|l| {
                let t = l.trim_start();
                let t = t
                    .strip_prefix("pub(crate) ")
                    .or_else(|| t.strip_prefix("pub "))
                    .unwrap_or(t);
                t.starts_with("static ")
            })
            .count();
        assert_eq!(statics, 1, "exactly one production static ring (BUFFER)");
    }
}
