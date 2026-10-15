//! OSC 9999 `session_start` report — the provider session id (and launch
//! argv) a pane's `SessionStart` hook writes to the PTY itself.
//!
//! Plan `2026-09-12-out-of-process-pty-owner-for-terminal-hosted-sessions`,
//! Phase 6, D11. Tier 1 is the PTY holder surviving the runner; tier 2 is what
//! happens when the HOLDER dies: the session in that pane is relaunched with
//! `--resume <id>` ([`crate::terminal::holder_relaunch`]). For that the runner
//! needs the id the provider is ACTUALLY running under, and the argv it was
//! launched with. herdr and tty7 both converged on a `SessionStart` hook
//! reporting the id directly, and tty7's transport for it is the PTY — no
//! socket, port or file coordination. This runner already had the transport:
//! the OSC 9999 agent-status sideband ([`super::agent_status_sideband`]),
//! parsed by the grid on the reader thread. A report is a sub-tagged payload on
//! the same OSC number:
//!
//! ```text
//! ESC ] 9999 ; session_start ; {"session_id":"<id>","source":"startup",
//!                               "argv":["--model","opus"]} BEL
//! ```
//!
//! The grid routes it into its own slot (`grid::OSC_SESSION_START_TAG`), so a
//! status arriving in the same chunk cannot displace it.
//!
//! The bundled hook (`resources/session-restore/claude_session_hook.sh`) writes
//! it to `/dev/tty` AFTER its `POST /control/session-open`, so the record the
//! report's argv lands on normally exists already. The POST stays the channel
//! that CONFIRMS a session; this report does not confirm, create or re-key any
//! record (a report replayed out of a holder's ring after a runner restart
//! would otherwise resurrect what has since moved on). It does two things: the
//! terminal remembers the latest report in memory (the id tier 2 relaunches),
//! and the argv is persisted onto the session's open record on this terminal
//! (`SessionLifecycleStore::set_launch_argv`).
//!
//! ## Untrusted input
//!
//! Same posture as the status sideband: the payload came off a PTY, so it is
//! size-capped by the grid, never logged raw, and every field is checked —
//! `session_id` by the one session-id predicate
//! ([`crate::session::session_id::is_valid_session_id`]), `source` against the
//! provider's four words, and `argv` by
//! [`crate::session::resume_argv::parse_argv_value`]. A report without a valid
//! id is dropped whole; a bad `argv` drops the argv only (UNKNOWN, never a
//! partial argv). Nothing here can panic or block the reader thread.

use std::sync::Mutex;
use std::time::Duration;

use serde_json::Value as JsonValue;
use tracing::{debug, info};

use crate::session::resume_argv::parse_argv_value;
use crate::session::session_id::is_valid_session_id;

/// Longest session id a report may carry. Provider ids are UUIDs (36 chars);
/// the cap only bounds what an untrusted payload can make the runner store.
pub const MAX_SESSION_ID_CHARS: usize = 128;

/// When the report's record is not there yet (the hook's POST and its OSC race
/// only when the POST failed or is slow), the argv write is retried once after
/// this long.
pub const ARGV_PERSIST_RETRY_AFTER: Duration = Duration::from_secs(5);

/// One parsed `session_start` report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionStartReport {
    /// The provider session id the hook reported — validated shell-safe.
    pub session_id: String,
    /// `startup` | `resume` | `clear` | `compact`; `None` when absent or not
    /// one of those (never defaulted — an unreadable source is unknown).
    pub source: Option<String>,
    /// The launch argv, when the report carried a well-formed one.
    pub argv: Option<Vec<String>>,
}

fn canonical_source(raw: &str) -> Option<&'static str> {
    match raw {
        "startup" => Some("startup"),
        "resume" => Some("resume"),
        "clear" => Some("clear"),
        "compact" => Some("compact"),
        _ => None,
    }
}

/// Parse one raw `session_start` payload. `None` — silently — for anything
/// that is not a JSON object carrying a valid `session_id`. Never panics.
pub fn parse_session_start(raw: &str) -> Option<SessionStartReport> {
    if raw.len() > crate::terminal::grid::MAX_AGENT_STATUS_SIDEBAND_BYTES {
        return None;
    }
    let value: JsonValue = serde_json::from_str(raw).ok()?;
    let obj = value.as_object()?;
    let session_id = obj.get("session_id")?.as_str()?;
    if session_id.chars().count() > MAX_SESSION_ID_CHARS || !is_valid_session_id(session_id) {
        return None;
    }
    Some(SessionStartReport {
        session_id: session_id.to_string(),
        source: obj
            .get("source")
            .and_then(JsonValue::as_str)
            .and_then(canonical_source)
            .map(str::to_string),
        argv: obj.get("argv").and_then(parse_argv_value),
    })
}

/// Handle one drained `session_start` payload for terminal `terminal_id`:
/// remember it in `slot` (latest wins — the newest `SessionStart` is the
/// process now running in the pane), then persist its argv off the reader
/// thread. Returns the parsed report for the caller's tests.
pub fn dispatch(
    terminal_id: &str,
    slot: &Mutex<Option<SessionStartReport>>,
    raw: String,
) -> Option<SessionStartReport> {
    // NB: `raw` is untrusted PTY bytes and is never logged.
    let Some(report) = parse_session_start(&raw) else {
        debug!(
            terminal = %terminal_id,
            "session_start_sideband: OSC 9999 session_start payload dropped (unparseable or no valid session id)"
        );
        return None;
    };
    if let Ok(mut guard) = slot.lock() {
        *guard = Some(report.clone());
    }
    info!(
        terminal = %terminal_id,
        session_id = %report.session_id,
        source = report.source.as_deref().unwrap_or("?"),
        argv_reported = report.argv.is_some(),
        "session_start_sideband: SessionStart reported over the PTY"
    );
    if let Some(argv) = report.argv.clone() {
        let terminal_id = terminal_id.to_string();
        let session_id = report.session_id.clone();
        tauri::async_runtime::spawn(async move {
            persist_argv(&terminal_id, &session_id, &argv).await;
        });
    }
    Some(report)
}

/// Write `argv` onto `session_id`'s open record on `terminal_id`, retrying
/// once when the record is not there yet. Best-effort: a sideband must never
/// disturb the live session.
async fn persist_argv(terminal_id: &str, session_id: &str, argv: &[String]) {
    use crate::session::session_lifecycle_store::{LaunchArgvWrite, SessionLifecycleStore};
    use tauri::Manager;

    let Some(app) = crate::tauri_app_handle::current() else {
        return;
    };
    let Some(store) = app
        .try_state::<std::sync::Arc<SessionLifecycleStore>>()
        .map(|s| s.inner().clone())
    else {
        return;
    };
    for attempt in 0..2 {
        if attempt > 0 {
            tokio::time::sleep(ARGV_PERSIST_RETRY_AFTER).await;
        }
        match store.set_launch_argv(session_id, terminal_id, argv) {
            LaunchArgvWrite::Written | LaunchArgvWrite::Unchanged => return,
            LaunchArgvWrite::NoOpenRecordOnTerminal => {}
        }
    }
    info!(
        terminal = %terminal_id,
        session_id = %session_id,
        "session_start_sideband: no open record for the reported session on this terminal — launch argv not persisted (a relaunch would use the default resume command)"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    const SID: &str = "0b1c2d3e-4f50-6172-8394-a5b6c7d8e9f0";

    #[test]
    fn session_start_sideband_parses_id_source_and_argv() {
        let report = parse_session_start(&format!(
            r#"{{"session_id":"{SID}","source":"resume","argv":["--model","opus"]}}"#
        ))
        .expect("parses");
        assert_eq!(report.session_id, SID);
        assert_eq!(report.source.as_deref(), Some("resume"));
        assert_eq!(
            report.argv,
            Some(vec!["--model".to_string(), "opus".to_string()])
        );
    }

    #[test]
    fn session_start_sideband_a_bad_argv_or_source_drops_only_that_field() {
        let report = parse_session_start(&format!(
            r#"{{"session_id":"{SID}","source":"reboot","argv":["ok",7]}}"#
        ))
        .expect("the id survives");
        assert_eq!(
            report.source, None,
            "an unknown source is unknown, not a default"
        );
        assert_eq!(report.argv, None, "a partly-bad argv is no argv");
        let report = parse_session_start(&format!(r#"{{"session_id":"{SID}"}}"#)).unwrap();
        assert_eq!((report.source, report.argv), (None, None));
    }

    #[test]
    fn session_start_sideband_without_a_valid_id_is_dropped_whole() {
        for raw in [
            "",
            "not json",
            "[]",
            "{}",
            r#"{"source":"startup"}"#,
            r#"{"session_id":""}"#,
            r#"{"session_id":7}"#,
            r#"{"session_id":"a b"}"#,
            r#"{"session_id":"$(id)"}"#,
            r#"{"session_id":"x;rm"}"#,
        ] {
            assert!(parse_session_start(raw).is_none(), "must drop: {raw}");
        }
        let long = "a".repeat(MAX_SESSION_ID_CHARS + 1);
        assert!(parse_session_start(&format!(r#"{{"session_id":"{long}"}}"#)).is_none());
    }

    #[test]
    fn session_start_sideband_dispatch_keeps_the_latest_report() {
        let slot = Mutex::new(None);
        // No argv: nothing is spawned onto the async runtime.
        let first = dispatch("t-1", &slot, format!(r#"{{"session_id":"{SID}"}}"#));
        assert_eq!(first.map(|r| r.session_id).as_deref(), Some(SID));
        assert!(dispatch("t-1", &slot, "garbage".into()).is_none());
        assert_eq!(
            slot.lock().unwrap().as_ref().map(|r| r.session_id.as_str()),
            Some(SID),
            "a dropped payload leaves the last good report in place"
        );
        dispatch(
            "t-1",
            &slot,
            r#"{"session_id":"second-id","source":"clear"}"#.into(),
        );
        let latest = slot.lock().unwrap().clone().unwrap();
        assert_eq!(
            (latest.session_id.as_str(), latest.source.as_deref()),
            ("second-id", Some("clear"))
        );
    }

    /// End to end on the grid: the exact bytes the bundled hook writes reach
    /// the report parser through the grid's own slot.
    #[test]
    fn session_start_sideband_grid_to_report() {
        let mut grid = crate::terminal::grid::Grid::new(80, 24);
        let mut parser = vte::Parser::new();
        let bytes = format!(
            "\x1b]9999;session_start;{{\"session_id\":\"{SID}\",\"source\":\"startup\",\"argv\":[\"--add-dir\",\"/srv/a;b\"]}}\x07"
        );
        grid.feed(&mut parser, bytes.as_bytes());
        let raw = grid
            .take_session_start_sideband()
            .expect("routed to its slot");
        let report = parse_session_start(&raw).expect("parses");
        assert_eq!(report.session_id, SID);
        assert_eq!(
            report.argv,
            Some(vec!["--add-dir".to_string(), "/srv/a;b".to_string()])
        );
    }
}
