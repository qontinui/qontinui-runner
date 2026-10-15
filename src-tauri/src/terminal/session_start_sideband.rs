//! OSC 9999 `session_start` report — the provider session id a pane's
//! `SessionStart` hook writes to the PTY itself.
//!
//! Plan `2026-09-12-out-of-process-pty-owner-for-terminal-hosted-sessions`,
//! Phase 6, D11. Tier 1 is the PTY holder surviving the runner; tier 2 is what
//! happens when the HOLDER dies: the session in that pane is relaunched with
//! `--resume <id>` ([`crate::terminal::holder_relaunch`]). For that the runner
//! needs the id the provider is ACTUALLY running under in that pane, including
//! after a `/clear`. herdr and tty7 both converged on a `SessionStart` hook
//! reporting the id directly, and tty7's transport for it is the PTY — no
//! socket, port or file coordination. This runner already had the transport:
//! the OSC 9999 agent-status sideband ([`super::agent_status_sideband`]),
//! parsed by the grid on the reader thread. A report is a sub-tagged payload on
//! the same OSC number:
//!
//! ```text
//! ESC ] 9999 ; session_start ; {"session_id":"<uuid>","source":"startup"} BEL
//! ```
//!
//! The grid routes it into its own slot (`grid::OSC_SESSION_START_TAG`), so a
//! status arriving in the same chunk cannot displace it.
//!
//! # Only the id travels here — never the argv
//!
//! The PTY stream is relayed (SSE, the mobile relay), kept in the holder's and
//! the runner's rings, and shipped to coord's output store. So the report
//! carries the session id and source and nothing else (review r1, H1); the
//! launch argv a relaunch needs rides the hook's loopback
//! `POST /control/session-open` body instead
//! (`install_effects_producer::apply_launch_argv`). The report does not
//! confirm, create or re-key any record either — a report replayed out of a
//! holder's ring after a runner restart would otherwise resurrect what has
//! since moved on. The terminal only REMEMBERS the latest one: the id tier 2
//! relaunches.
//!
//! # Untrusted input
//!
//! Same posture as the status sideband: the payload came off a PTY, so it is
//! size-capped by the grid, never logged raw, and checked: `session_id` must be
//! a hyphenated UUID (what Claude Code mints, and what the hook checks before
//! writing), `source` one of the provider's four words, and a payload with a
//! DUPLICATE key is dropped whole (serde_json keeps the last duplicate, so a
//! spliced second `session_id` could otherwise re-key the report — review r1,
//! L4). Nothing here can panic or block the reader thread.

use std::sync::Mutex;

use serde::de::{Deserializer, MapAccess, Visitor};
use serde_json::Value as JsonValue;
use tracing::{debug, info};

/// One parsed `session_start` report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionStartReport {
    /// The provider session id the hook reported — a hyphenated UUID.
    pub session_id: String,
    /// `startup` | `resume` | `clear` | `compact`; `None` when absent or not
    /// one of those (never defaulted — an unreadable source is unknown).
    pub source: Option<String>,
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

/// Whether `id` is a hyphenated UUID: 36 ASCII characters, `-` at 8, 13, 18
/// and 23, hex everywhere else. Stricter than `Uuid::parse_str`, which also
/// takes braced, URN and simple forms.
pub fn is_hyphenated_uuid(id: &str) -> bool {
    id.len() == 36
        && id.bytes().enumerate().all(|(i, b)| match i {
            8 | 13 | 18 | 23 => b == b'-',
            _ => b.is_ascii_hexdigit(),
        })
}

/// A JSON object's members in order, or an error on a duplicate key.
struct UniqueKeys(Vec<(String, JsonValue)>);

impl<'de> serde::Deserialize<'de> for UniqueKeys {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = UniqueKeys;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a JSON object with unique keys")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<UniqueKeys, A::Error> {
                let mut members: Vec<(String, JsonValue)> = Vec::new();
                while let Some((k, v)) = map.next_entry::<String, JsonValue>()? {
                    if members.iter().any(|(seen, _)| *seen == k) {
                        return Err(serde::de::Error::custom("duplicate key"));
                    }
                    members.push((k, v));
                }
                Ok(UniqueKeys(members))
            }
        }
        d.deserialize_map(V)
    }
}

/// Parse one raw `session_start` payload. `None` — silently — for anything
/// that is not a JSON object with unique keys carrying a UUID `session_id`.
/// Never panics.
pub fn parse_session_start(raw: &str) -> Option<SessionStartReport> {
    if raw.len() > crate::terminal::grid::MAX_AGENT_STATUS_SIDEBAND_BYTES {
        return None;
    }
    let UniqueKeys(members) = serde_json::from_str(raw).ok()?;
    let get = |key: &str| members.iter().find(|(k, _)| k == key).map(|(_, v)| v);
    let session_id = get("session_id")?.as_str()?;
    if !is_hyphenated_uuid(session_id) {
        return None;
    }
    Some(SessionStartReport {
        session_id: session_id.to_string(),
        source: get("source")
            .and_then(JsonValue::as_str)
            .and_then(canonical_source)
            .map(str::to_string),
    })
}

/// Handle one drained `session_start` payload for terminal `terminal_id`:
/// remember it in `slot` (latest wins — the newest `SessionStart` is the
/// process now running in the pane). Returns the parsed report for tests.
pub fn dispatch(
    terminal_id: &str,
    slot: &Mutex<Option<SessionStartReport>>,
    raw: String,
) -> Option<SessionStartReport> {
    // NB: `raw` is untrusted PTY bytes and is never logged.
    let Some(report) = parse_session_start(&raw) else {
        debug!(
            terminal = %terminal_id,
            "session_start_sideband: OSC 9999 session_start payload dropped (unparseable, duplicate key or no UUID session id)"
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
        "session_start_sideband: SessionStart reported over the PTY"
    );
    Some(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SID: &str = "0b1c2d3e-4f50-6172-8394-a5b6c7d8e9f0";
    const SID2: &str = "11111111-2222-3333-4444-555555555555";

    #[test]
    fn session_start_sideband_parses_id_and_source() {
        let report = parse_session_start(&format!(r#"{{"session_id":"{SID}","source":"resume"}}"#))
            .expect("parses");
        assert_eq!(report.session_id, SID);
        assert_eq!(report.source.as_deref(), Some("resume"));
        // Anything else in the payload — an argv included — is ignored.
        let report = parse_session_start(&format!(
            r#"{{"session_id":"{SID}","argv":["--settings","/tmp/x"]}}"#
        ))
        .unwrap();
        assert_eq!(report.source, None);
    }

    #[test]
    fn session_start_sideband_a_bad_source_drops_only_that_field() {
        let report = parse_session_start(&format!(r#"{{"session_id":"{SID}","source":"reboot"}}"#))
            .expect("the id survives");
        assert_eq!(
            report.source, None,
            "an unknown source is unknown, not a default"
        );
    }

    #[test]
    fn session_start_sideband_without_a_uuid_id_is_dropped_whole() {
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
            r#"{"session_id":"second-id"}"#,
            r#"{"session_id":"0b1c2d3e4f5061728394a5b6c7d8e9f0"}"#,
            r#"{"session_id":"{0b1c2d3e-4f50-6172-8394-a5b6c7d8e9f0}"}"#,
            r#"{"session_id":"0b1c2d3e-4f50-6172-8394-a5b6c7d8e9fg"}"#,
        ] {
            assert!(parse_session_start(raw).is_none(), "must drop: {raw}");
        }
    }

    /// Review r1, L4: serde_json keeps the LAST duplicate key, so a spliced
    /// second `session_id` would re-key the report. A duplicate key drops it.
    #[test]
    fn session_start_sideband_a_duplicate_key_drops_the_report() {
        for raw in [
            format!(r#"{{"session_id":"{SID}","argv":[],"session_id":"{SID2}"}}"#),
            format!(r#"{{"session_id":"{SID}","source":"startup","source":"clear"}}"#),
        ] {
            assert!(parse_session_start(&raw).is_none(), "{raw}");
        }
    }

    #[test]
    fn session_start_sideband_dispatch_keeps_the_latest_report() {
        let slot = Mutex::new(None);
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
            format!(r#"{{"session_id":"{SID2}","source":"clear"}}"#),
        );
        let latest = slot.lock().unwrap().clone().unwrap();
        assert_eq!(
            (latest.session_id.as_str(), latest.source.as_deref()),
            (SID2, Some("clear"))
        );
    }

    /// End to end on the grid: the exact bytes the bundled hook writes reach
    /// the report parser through the grid's own slot.
    #[test]
    fn session_start_sideband_grid_to_report() {
        let mut grid = crate::terminal::grid::Grid::new(80, 24);
        let mut parser = vte::Parser::new();
        let bytes = format!(
            "\x1b]9999;session_start;{{\"session_id\":\"{SID}\",\"source\":\"startup\"}}\x07"
        );
        grid.feed(&mut parser, bytes.as_bytes());
        let raw = grid
            .take_session_start_sideband()
            .expect("routed to its slot");
        let report = parse_session_start(&raw).expect("parses");
        assert_eq!(report.session_id, SID);
        assert_eq!(report.source.as_deref(), Some("startup"));
    }
}
