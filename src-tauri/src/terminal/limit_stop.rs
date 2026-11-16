//! Did a session stop at a usage limit? Read from its transcript, not its screen.
//!
//! The screen-pattern scan (`usage_limit.rs`) is a proxy for this state, and it
//! has failed twice (plan
//! `2026-11-05-runner-terminal-pages-collide-crowd-and-overfill-and-limit-stalled-sessions-never-resume`,
//! §Why "Root cause of fix 5"): a branch build lacked the `hit your weekly limit`
//! pattern, and 43 of 103 restored parked panes never matched any pattern at
//! all. The Claude CLI records the stop itself as a structured transcript
//! entry — an assistant API-error entry carrying `isApiErrorMessage: true`,
//! `apiError: "usage_limit_reached"` and `quotaLimits.resetsAt` (seconds) —
//! followed only by non-conversational bookkeeping lines (`system`
//! `turn_duration`, `mode`, `permission-mode`, `last-prompt`, `cost-state`,
//! `tools-used`, …) and, when the CLI opens its `/rate-limit-options` menu,
//! by that local command's `user` entries, which never reach the model. That
//! entry is the state, so this module reads it.
//!
//! The read is bounded: the last [`TAIL_BYTES`] of the file, widened through
//! [`TAIL_WINDOWS`] while the window holds no conversational entry, and the
//! whole file only after that. A transcript that cannot
//! be read or parsed where the decision is made is [`LimitStopRead::Unreadable`]
//! — UNKNOWN — and never [`LimitStopRead::NotStopped`].
//!
//! Interim arm: plan `2026-10-28-one-session-lifecycle-authority` Phase 99 (the
//! Migrator) is to carry this classifier into its `decide()`.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::Mutex;

use serde_json::Value;

/// How much of the transcript's end is read first (the plan's bound).
pub(crate) const TAIL_BYTES: u64 = 256 * 1024;

/// The tail windows tried in order before the whole file. The bookkeeping that
/// follows the deciding entry is not always short: `api-request-blob`,
/// `attachment` and `api-request-shape` lines run to hundreds of KiB, and on
/// nilan in 2026-11 about one in ten transcripts over 256 KiB had more than
/// 256 KiB after their last conversational entry (two counts: 28 of 224 and
/// 25 of 266), every one under 1 MiB. Widening in steps
/// keeps those reads bounded instead of jumping to a 5–15 MB whole-file read.
const TAIL_WINDOWS: [u64; 3] = [TAIL_BYTES, 1024 * 1024, 4 * 1024 * 1024];

/// The `apiError` value the CLI writes when an account's quota is exhausted.
const USAGE_LIMIT_API_ERROR: &str = "usage_limit_reached";

/// A session whose last conversational entry is a usage-limit stop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LimitStop {
    /// The error entry's `uuid`. One stop is one entry, so this is the key a
    /// caller dedups a continue on.
    pub entry_uuid: String,
    /// `quotaLimits.resetsAt`, converted from seconds to milliseconds since the
    /// epoch. `None` when the entry carries no usable reset time.
    pub resets_at_ms: Option<i64>,
    /// `quotaLimits.rateLimitType` (`seven_day`, `five_hour`, …), as written.
    pub rate_limit_type: Option<String>,
    /// The error entry's own `timestamp` (RFC 3339), in milliseconds since
    /// the epoch: when the session stopped. `None` when absent or unparseable.
    /// A caller compares it with a later account migration, whose carried
    /// transcript still ends at this stop.
    pub stopped_at_ms: Option<i64>,
}

/// What the transcript says about a usage-limit stop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LimitStopRead {
    /// The last conversational entry is a usage-limit API error.
    Stopped(LimitStop),
    /// The last conversational entry is anything else, or the transcript holds
    /// no conversational entry at all.
    NotStopped,
    /// The transcript could not be read or parsed where the decision is made.
    /// UNKNOWN: a caller must not read it as either answer.
    Unreadable(String),
}

/// Which span of the file the decision was made on. Test-visible so the
/// bounded-read property is asserted structurally, not by wall-clock alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReadSpan {
    /// The last N bytes, N one of [`TAIL_WINDOWS`].
    Tail(u64),
    Whole,
}

/// Classify the session transcript at `path`. See the module docs.
pub(crate) fn transcript_limit_stop(path: &Path) -> LimitStopRead {
    read_limit_stop(path).0
}

fn read_limit_stop(path: &Path) -> (LimitStopRead, ReadSpan) {
    let mut file = match File::open(path) {
        Ok(f) => f,
        Err(e) => {
            return (
                LimitStopRead::Unreadable(format!("cannot open transcript: {e}")),
                ReadSpan::Whole,
            )
        }
    };
    let len = match file.metadata() {
        Ok(m) => m.len(),
        Err(e) => {
            return (
                LimitStopRead::Unreadable(format!("cannot stat transcript: {e}")),
                ReadSpan::Whole,
            )
        }
    };

    for window in TAIL_WINDOWS.into_iter().take_while(|&w| w < len) {
        // Read one byte before the tail too, to tell whether the tail starts
        // on a line boundary.
        match read_span(&mut file, len - window - 1, len) {
            Ok(bytes) => {
                // Skip through the first newline: when the byte before the
                // tail is `\n` that drops only that byte; otherwise it drops
                // the partial first line, which cannot be parsed.
                let start = bytes
                    .iter()
                    .position(|&b| b == b'\n')
                    .map_or(bytes.len(), |i| i + 1);
                match decide(&bytes[start..]) {
                    Decision::Decided(read) => return (read, ReadSpan::Tail(window)),
                    // No conversational entry in this window: widen it.
                    Decision::NoConversation => {}
                }
            }
            Err(e) => {
                return (
                    LimitStopRead::Unreadable(format!("cannot read transcript tail: {e}")),
                    ReadSpan::Tail(window),
                )
            }
        }
    }

    let bytes = match read_span(&mut file, 0, len) {
        Ok(b) => b,
        Err(e) => {
            return (
                LimitStopRead::Unreadable(format!("cannot read transcript: {e}")),
                ReadSpan::Whole,
            )
        }
    };
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return (
            LimitStopRead::Unreadable("transcript is empty".to_string()),
            ReadSpan::Whole,
        );
    }
    let read = match decide(&bytes) {
        Decision::Decided(read) => read,
        // Every line parsed and none is a conversational entry: the session
        // never exchanged a turn, so it did not stop at a limit.
        Decision::NoConversation => LimitStopRead::NotStopped,
    };
    (read, ReadSpan::Whole)
}

/// Bytes `offset..end` of `file`. Bounded at the `end` measured once, so a
/// transcript the CLI is still appending to is read as one snapshot: the
/// window stays its nominal size, and only a line already partial at that
/// moment can read as `Unreadable`.
fn read_span(file: &mut File, offset: u64, end: u64) -> std::io::Result<Vec<u8>> {
    file.seek(SeekFrom::Start(offset))?;
    let mut buf = Vec::new();
    file.take(end - offset).read_to_end(&mut buf)?;
    Ok(buf)
}

enum Decision {
    Decided(LimitStopRead),
    NoConversation,
}

/// Walk `bytes` from the end and decide on the last conversational entry.
fn decide(bytes: &[u8]) -> Decision {
    // Lines are decoded one at a time, as they are visited: a bad byte above
    // the deciding entry does not make the read UNKNOWN, one at or below it
    // does.
    // A final newline ends the last line; it does not start an empty one, so
    // line numbers in the reasons below count real lines from the end.
    let bytes = bytes.strip_suffix(b"\n").unwrap_or(bytes);
    // Set once a local command's output has been passed over, so the
    // `<command-name>` entry that ran it is passed over too (see
    // [`local_command_part`]).
    let mut in_local_command = false;
    for (from_end, raw) in bytes.split(|&b| b == b'\n').rev().enumerate() {
        let line = match std::str::from_utf8(raw) {
            Ok(l) => l.trim(),
            Err(e) => {
                return Decision::Decided(LimitStopRead::Unreadable(format!(
                    "transcript line {} from the end is not UTF-8: {e}",
                    from_end + 1
                )))
            }
        };
        if line.is_empty() {
            continue;
        }
        let entry: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            // A line that does not parse may be the entry that decides (a
            // truncated final write, a corrupt record): that is UNKNOWN.
            Err(e) => {
                return Decision::Decided(LimitStopRead::Unreadable(format!(
                    "transcript line {} from the end does not parse: {e}",
                    from_end + 1
                )))
            }
        };
        if !is_conversational(&entry) {
            continue;
        }
        match local_command_part(&entry) {
            Some(LocalCommandPart::Output) => {
                in_local_command = true;
                continue;
            }
            Some(LocalCommandPart::Caveat) => continue,
            Some(LocalCommandPart::Invocation) if in_local_command => {
                in_local_command = false;
                continue;
            }
            Some(LocalCommandPart::Invocation) | None => {}
        }
        return Decision::Decided(classify(&entry));
    }
    Decision::NoConversation
}

/// The pieces of a LOCAL slash command the CLI records as main-thread `user`
/// entries. A local command (`/rate-limit-options`, which the CLI opens by
/// itself on a limit stop; `/model`, …) runs in the CLI and never reaches the
/// model, so it neither answers nor clears the stop above it. Measured shape
/// (nilan, session `b9e123f1`, a seven-day stop parked on the menu): after
/// the error entry come an `isMeta` `<local-command-caveat>` entry, a
/// `<command-name>/rate-limit-options</command-name>` entry and a
/// `<local-command-stdout>Claude Code will continue automatically at …`
/// entry, then end of file.
enum LocalCommandPart {
    /// `<local-command-caveat>`: always local.
    Caveat,
    /// `<local-command-stdout>` / `<local-command-stderr>`: always local.
    Output,
    /// `<command-name>`: local only when its output follows it. A slash
    /// command that IS sent to the model writes no local output, so it stays
    /// conversational and decides the read.
    Invocation,
}

fn local_command_part(entry: &Value) -> Option<LocalCommandPart> {
    if entry.get("type").and_then(Value::as_str) != Some("user") {
        return None;
    }
    let content = entry
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(Value::as_str)?
        .trim_start();
    if content.starts_with("<local-command-caveat>") {
        Some(LocalCommandPart::Caveat)
    } else if content.starts_with("<local-command-stdout>")
        || content.starts_with("<local-command-stderr>")
    {
        Some(LocalCommandPart::Output)
    } else if content.starts_with("<command-name>") {
        Some(LocalCommandPart::Invocation)
    } else {
        None
    }
}

/// A main-thread `user` or `assistant` entry. `system`, `summary`,
/// `last-prompt`, `cost-state`, `tools-used` and the other bookkeeping types
/// are not, and neither is a subagent's (`isSidechain: true`) entry.
fn is_conversational(entry: &Value) -> bool {
    let kind = entry.get("type").and_then(Value::as_str);
    if !matches!(kind, Some("user") | Some("assistant")) {
        return false;
    }
    entry.get("isSidechain").and_then(Value::as_bool) != Some(true)
}

fn classify(entry: &Value) -> LimitStopRead {
    let is_limit_stop = entry.get("type").and_then(Value::as_str) == Some("assistant")
        && entry.get("isApiErrorMessage").and_then(Value::as_bool) == Some(true)
        && entry.get("apiError").and_then(Value::as_str) == Some(USAGE_LIMIT_API_ERROR);
    if !is_limit_stop {
        return LimitStopRead::NotStopped;
    }
    let Some(entry_uuid) = entry.get("uuid").and_then(Value::as_str) else {
        // A stop with no identity cannot be continued exactly once.
        return LimitStopRead::Unreadable("usage-limit stop entry carries no uuid".to_string());
    };
    let quota = entry.get("quotaLimits");
    let resets_at_ms = quota
        .and_then(|q| q.get("resetsAt"))
        .and_then(|r| r.as_i64().or_else(|| r.as_f64().map(|f| f as i64)))
        .and_then(|secs| secs.checked_mul(1000));
    let rate_limit_type = quota
        .and_then(|q| q.get("rateLimitType"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let stopped_at_ms = entry
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
        .map(|t| t.timestamp_millis());
    LimitStopRead::Stopped(LimitStop {
        entry_uuid: entry_uuid.to_string(),
        resets_at_ms,
        rate_limit_type,
        stopped_at_ms,
    })
}

// ── The park list (plan 2026-11-05 Phase 2) ──────────────────────────────────
//
// A session that stopped at a usage limit and cannot be continued or migrated
// yet is PARKED here until a due time, and the scan loop re-hands it to
// `account_migration::handle_usage_limit_hint` once that time has passed
// (Phase 3 drains it). In memory only, on purpose: it is not durable state. A
// restart loses it, and the next process re-derives a park from each
// transcript that still ends at its stop. What a restart does lose is the
// in-memory migration stamp the handler reads (`LAST_MIGRATED`), so a migrated
// session whose continue nudge never landed is decided from its carried stop
// again: parked until the OLD account's reset rather than continued at once.

/// One parked session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Park {
    pub terminal_id: String,
    pub claude_session_id: String,
    /// When the session is due to be handed back to the handler, in
    /// milliseconds since the epoch.
    pub due_at_ms: i64,
}

/// The parks, keyed by terminal: one pane has one pending re-entry.
#[derive(Debug, Default)]
pub(crate) struct ParkList {
    by_terminal: HashMap<String, Park>,
    /// The reason last REPORTED for each terminal's park. Kept when a due park
    /// is taken, so a pane re-parked for the same reason on every re-entry
    /// (a probe that keeps failing) reports once, not each time.
    reported: HashMap<String, &'static str>,
}

impl ParkList {
    /// Park `terminal_id` until `due_at_ms`. A second park of the same
    /// terminal replaces the first: the newest decision about a pane is the
    /// one that holds.
    /// Returns whether the park is worth reporting: the terminal was last
    /// reported under another reason, or never.
    pub(crate) fn park(
        &mut self,
        terminal_id: &str,
        claude_session_id: &str,
        due_at_ms: i64,
        reason: &'static str,
    ) -> bool {
        self.by_terminal.insert(
            terminal_id.to_string(),
            Park {
                terminal_id: terminal_id.to_string(),
                claude_session_id: claude_session_id.to_string(),
                due_at_ms,
            },
        );
        self.reported.insert(terminal_id.to_string(), reason) != Some(reason)
    }

    /// Remove and return every park due at `now_ms`, oldest due first.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "drained by the scan loop in plan 2026-11-05 Phase 3"
        )
    )]
    pub(crate) fn take_due(&mut self, now_ms: i64) -> Vec<Park> {
        let due: Vec<String> = self
            .by_terminal
            .iter()
            .filter(|(_, p)| p.due_at_ms <= now_ms)
            .map(|(id, _)| id.clone())
            .collect();
        let mut taken: Vec<Park> = due
            .iter()
            .filter_map(|id| self.by_terminal.remove(id))
            .collect();
        taken.sort_by(|a, b| {
            a.due_at_ms
                .cmp(&b.due_at_ms)
                .then_with(|| a.terminal_id.cmp(&b.terminal_id))
        });
        taken
    }

    /// Drop `terminal_id`'s pending park and its reported reason: its stop
    /// was answered (a continue reached the pane, or the session was already
    /// working), so a due re-entry would only re-read a session no longer
    /// stopped, and a later stop's park is news again.
    pub(crate) fn forget(&mut self, terminal_id: &str) {
        self.by_terminal.remove(terminal_id);
        self.reported.remove(terminal_id);
    }

    /// The park on `terminal_id`, if any.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "read by plan 2026-11-05 Phase 3 and by tests")
    )]
    pub(crate) fn get(&self, terminal_id: &str) -> Option<&Park> {
        self.by_terminal.get(terminal_id)
    }
}

/// The process's park list.
static PARKS: Mutex<Option<ParkList>> = Mutex::new(None);

/// Run `f` on the process's park list.
pub(crate) fn with_parks<T>(f: impl FnOnce(&mut ParkList) -> T) -> T {
    let mut guard = PARKS.lock().unwrap_or_else(|e| e.into_inner());
    f(guard.get_or_insert_with(ParkList::default))
}

/// Park `terminal_id` until `due_at_ms` on the process's park list; whether
/// to report it ([`ParkList::park`]).
pub(crate) fn park(
    terminal_id: &str,
    claude_session_id: &str,
    due_at_ms: i64,
    reason: &'static str,
) -> bool {
    with_parks(|parks| parks.park(terminal_id, claude_session_id, due_at_ms, reason))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::time::{Duration, Instant};

    /// A seven-day stop: a user prompt, an assistant reply, the usage-limit
    /// API-error entry, then only bookkeeping lines (`system` `turn_duration`
    /// first, as in all 49 jan transcripts the plan measured on merytshost). The error entry's field
    /// set and the trailing line types (`tools-used`, `file-history-snapshot`,
    /// `api-request-blob`, `api-request`, `attachment`, then the plan's
    /// `mode` … `cost-state` list) are copied from a real main-thread stop on
    /// nilan (`~/.claude-hotmail`, session `4ebed30f`, CLI 2.1.318,
    /// 2026-11-05); ids, text and times are edited.
    const SEVEN_DAY_STOP: &str = include_str!("fixtures/limit_stop/seven_day_stop.jsonl");
    /// The same shape on the five-hour window.
    const FIVE_HOUR_STOP: &str = include_str!("fixtures/limit_stop/five_hour_stop.jsonl");

    const SEVEN_DAY_UUID: &str = "7c1d2e3f-4a5b-4c6d-8e7f-90a1b2c3d4e5";
    /// 2026-11-03T20:00:00Z, jan's reset as the plan records it.
    const SEVEN_DAY_RESETS_AT_MS: i64 = 1_793_736_000_000;

    fn write_transcript(body: &[u8]) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().expect("tempfile");
        f.write_all(body).expect("write");
        f.flush().expect("flush");
        f
    }

    fn read(body: &str) -> LimitStopRead {
        let f = write_transcript(body.as_bytes());
        transcript_limit_stop(f.path())
    }

    fn assistant_text(uuid: &str, text: &str) -> String {
        serde_json::json!({
            "parentUuid": null, "isSidechain": false, "type": "assistant", "uuid": uuid,
            "timestamp": "2026-11-02T16:31:00.000Z",
            "message": {"role": "assistant", "model": "claude-opus-4-1",
                        "content": [{"type": "text", "text": text}]}
        })
        .to_string()
    }

    #[test]
    fn a_seven_day_stop_followed_by_bookkeeping_reads_stopped() {
        assert_eq!(
            read(SEVEN_DAY_STOP),
            LimitStopRead::Stopped(LimitStop {
                entry_uuid: SEVEN_DAY_UUID.to_string(),
                resets_at_ms: Some(SEVEN_DAY_RESETS_AT_MS),
                rate_limit_type: Some("seven_day".to_string()),
                // 2026-11-02T16:29:41Z, the error entry's own timestamp.
                stopped_at_ms: Some(1_793_636_981_000),
            })
        );
        // The fixture really does end in non-conversational lines: the last
        // line is `tools-used`, so a last-LINE reader cannot pass this test.
        let last = SEVEN_DAY_STOP.trim_end().lines().last().unwrap();
        assert!(last.contains("\"type\":\"tools-used\""), "{last}");
    }

    #[test]
    fn a_five_hour_stop_reads_stopped() {
        assert_eq!(
            read(FIVE_HOUR_STOP),
            LimitStopRead::Stopped(LimitStop {
                entry_uuid: "5f0e1d2c-3b4a-4958-8776-655443322110".to_string(),
                // 2026-11-02T21:00:00Z
                resets_at_ms: Some(1_793_653_200_000),
                rate_limit_type: Some("five_hour".to_string()),
                // 2026-11-02T18:37:12Z
                stopped_at_ms: Some(1_793_644_632_000),
            })
        );
    }

    #[test]
    fn a_stop_followed_by_a_user_prompt_is_not_stopped() {
        let user = serde_json::json!({
            "parentUuid": SEVEN_DAY_UUID, "isSidechain": false, "type": "user",
            "uuid": "aaaaaaaa-0000-4000-8000-000000000001",
            "message": {"role": "user", "content": "continue"}
        });
        let body = format!("{SEVEN_DAY_STOP}{user}\n");
        assert_eq!(read(&body), LimitStopRead::NotStopped);
    }

    #[test]
    fn an_assistant_reply_quoting_the_banner_is_not_stopped() {
        let quote = assistant_text(
            "bbbbbbbb-0000-4000-8000-000000000002",
            "The pane read \"You've hit your weekly limit · resets Nov 3, 9pm\".",
        );
        let body = format!(
            "{}\n{quote}\n{}\n",
            SEVEN_DAY_STOP.lines().next().unwrap(),
            r#"{"type":"system","subtype":"turn_duration","durationMs":4200,"isSidechain":false}"#
        );
        assert_eq!(read(&body), LimitStopRead::NotStopped);
    }

    #[test]
    fn a_sidechain_limit_error_under_a_main_thread_reply_is_not_stopped() {
        let error_line = SEVEN_DAY_STOP
            .lines()
            .find(|l| l.contains("usage_limit_reached"))
            .unwrap();
        let mut sidechain: Value = serde_json::from_str(error_line).unwrap();
        sidechain["isSidechain"] = Value::Bool(true);
        let main_reply = assistant_text("cccccccc-0000-4000-8000-000000000003", "All done.");
        let body = format!("{main_reply}\n{sidechain}\n");
        assert_eq!(read(&body), LimitStopRead::NotStopped);
    }

    /// The real menu-parked shape (nilan, session `b9e123f1`, ids edited): the
    /// CLI opens `/rate-limit-options` itself after the stop and records it as
    /// three main-thread `user` entries. They are local, so the stop decides.
    /// Mutation partner for "count every `user` entry".
    #[test]
    fn a_stop_parked_on_the_rate_limit_options_menu_reads_stopped() {
        let body = format!("{SEVEN_DAY_STOP}{RATE_LIMIT_OPTIONS_TAIL}");
        assert_eq!(
            read(&body),
            LimitStopRead::Stopped(LimitStop {
                entry_uuid: SEVEN_DAY_UUID.to_string(),
                resets_at_ms: Some(SEVEN_DAY_RESETS_AT_MS),
                rate_limit_type: Some("seven_day".to_string()),
                // 2026-11-02T16:29:41Z, the error entry's own timestamp.
                stopped_at_ms: Some(1_793_636_981_000),
            })
        );
    }

    /// A `<command-name>` entry with no local output after it was sent to the
    /// model: it is conversational, so the session moved on from the stop.
    #[test]
    fn a_model_bound_slash_command_after_a_stop_is_not_stopped() {
        let invocation = RATE_LIMIT_OPTIONS_TAIL.lines().nth(1).unwrap();
        assert!(
            invocation.contains("<command-name>/rate-limit-options"),
            "{invocation}"
        );
        let sent = invocation.replace("/rate-limit-options", "/implement-plan");
        let body = format!("{SEVEN_DAY_STOP}{sent}\n");
        assert_eq!(read(&body), LimitStopRead::NotStopped);
        // Local output ABOVE a later model-bound invocation does not hide it.
        let out = RATE_LIMIT_OPTIONS_TAIL.lines().nth(2).unwrap();
        let body = format!("{SEVEN_DAY_STOP}{out}\n{sent}\n");
        assert_eq!(read(&body), LimitStopRead::NotStopped);
    }

    /// The three `user` entries the CLI writes when it opens the
    /// `/rate-limit-options` menu, as measured on nilan (session `b9e123f1`,
    /// CLI-written texts verbatim; ids, times and the reset text edited).
    const RATE_LIMIT_OPTIONS_TAIL: &str = concat!(
        r#"{"parentUuid":"7c1d2e3f-4a5b-4c6d-8e7f-90a1b2c3d4e5","isSidechain":false,"promptId":"p-1","type":"user","message":{"role":"user","content":"<local-command-caveat>The command below was run directly in Claude Code, not sent to you as a request, and its output goes straight to the user. It's recorded here as context for later messages.</local-command-caveat>"},"isMeta":true,"uuid":"e1000000-0000-4000-8000-000000000001","timestamp":"2026-11-02T16:31:05.000Z","userType":"external","entrypoint":"cli","cwd":"/w","sessionId":"s","version":"2.1.318","gitBranch":"main"}"#,
        "\n",
        r#"{"parentUuid":"e1000000-0000-4000-8000-000000000001","isSidechain":false,"promptId":"p-1","type":"user","message":{"role":"user","content":"<command-name>/rate-limit-options</command-name>\n            <command-message>rate-limit-options</command-message>\n            <command-args></command-args>"},"uuid":"e1000000-0000-4000-8000-000000000002","timestamp":"2026-11-02T16:31:05.001Z","userType":"external","entrypoint":"cli","cwd":"/w","sessionId":"s","version":"2.1.318","gitBranch":"main"}"#,
        "\n",
        r#"{"parentUuid":"e1000000-0000-4000-8000-000000000002","isSidechain":false,"promptId":"p-1","type":"user","message":{"role":"user","content":"<local-command-stdout>Claude Code will continue automatically at Nov 3, 9pm. Keep this session open; it may still pause for permission prompts. Press esc to cancel the wait.</local-command-stdout>"},"uuid":"e1000000-0000-4000-8000-000000000003","timestamp":"2026-11-02T16:31:05.002Z","userType":"external","entrypoint":"cli","cwd":"/w","sessionId":"s","version":"2.1.318","gitBranch":"main"}"#,
        "\n",
    );

    /// Mutation partner for "accept any `isApiErrorMessage`".
    #[test]
    fn a_non_limit_api_error_is_not_stopped() {
        let error_line = SEVEN_DAY_STOP
            .lines()
            .find(|l| l.contains("usage_limit_reached"))
            .unwrap();
        let mut overloaded: Value = serde_json::from_str(error_line).unwrap();
        overloaded["apiError"] = Value::String("overloaded_error".to_string());
        overloaded["error"] = Value::String("server_error".to_string());
        let body = format!("{overloaded}\n");
        assert_eq!(read(&body), LimitStopRead::NotStopped);
    }

    /// The seven-day fixture's usage-limit error entry, for single-field edits.
    fn seven_day_error_entry() -> Value {
        let line = SEVEN_DAY_STOP
            .lines()
            .find(|l| l.contains("usage_limit_reached"))
            .unwrap();
        serde_json::from_str(line).unwrap()
    }

    #[test]
    fn a_limit_error_not_flagged_as_an_api_error_is_not_stopped() {
        let mut entry = seven_day_error_entry();
        entry["isApiErrorMessage"] = Value::Bool(false);
        assert_eq!(read(&format!("{entry}\n")), LimitStopRead::NotStopped);
        entry.as_object_mut().unwrap().remove("isApiErrorMessage");
        assert_eq!(read(&format!("{entry}\n")), LimitStopRead::NotStopped);
    }

    #[test]
    fn a_user_entry_carrying_the_limit_fields_is_not_stopped() {
        let mut entry = seven_day_error_entry();
        entry["type"] = Value::String("user".to_string());
        assert_eq!(read(&format!("{entry}\n")), LimitStopRead::NotStopped);
    }

    /// A stop with no `uuid` cannot be continued exactly once: UNKNOWN.
    #[test]
    fn a_limit_stop_without_a_uuid_is_unreadable() {
        let mut entry = seven_day_error_entry();
        entry.as_object_mut().unwrap().remove("uuid");
        assert_eq!(
            read(&format!("{entry}\n")),
            LimitStopRead::Unreadable("usage-limit stop entry carries no uuid".to_string())
        );
    }

    #[test]
    fn a_limit_stop_without_quota_limits_has_no_reset_time() {
        let mut entry = seven_day_error_entry();
        entry.as_object_mut().unwrap().remove("quotaLimits");
        assert_eq!(
            read(&format!("{entry}\n")),
            LimitStopRead::Stopped(LimitStop {
                entry_uuid: SEVEN_DAY_UUID.to_string(),
                resets_at_ms: None,
                rate_limit_type: None,
                stopped_at_ms: Some(1_793_636_981_000),
            })
        );
    }

    #[test]
    fn a_stop_with_no_timestamp_has_no_stop_time() {
        let mut entry = seven_day_error_entry();
        entry.as_object_mut().unwrap().remove("timestamp");
        assert!(matches!(
            read(&format!("{entry}\n")),
            LimitStopRead::Stopped(LimitStop {
                stopped_at_ms: None,
                ..
            })
        ));
    }

    #[test]
    fn a_fractional_reset_time_truncates_and_a_non_numeric_one_is_absent() {
        let mut entry = seven_day_error_entry();
        entry["quotaLimits"]["resetsAt"] = serde_json::json!(1_793_736_000.7);
        assert!(matches!(
            read(&format!("{entry}\n")),
            LimitStopRead::Stopped(LimitStop {
                resets_at_ms: Some(SEVEN_DAY_RESETS_AT_MS),
                ..
            })
        ));
        entry["quotaLimits"]["resetsAt"] = Value::String("soon".to_string());
        assert!(matches!(
            read(&format!("{entry}\n")),
            LimitStopRead::Stopped(LimitStop {
                resets_at_ms: None,
                ..
            })
        ));
    }

    /// A bad byte above the deciding entry does not make the read UNKNOWN.
    #[test]
    fn a_non_utf8_line_above_the_deciding_entry_is_not_read() {
        let mut body = b"{\"type\":\"user\",\"x\":\"\xff\"}\n".to_vec();
        body.extend_from_slice(SEVEN_DAY_STOP.as_bytes());
        let f = write_transcript(&body);
        assert!(matches!(
            transcript_limit_stop(f.path()),
            LimitStopRead::Stopped(ref s) if s.entry_uuid == SEVEN_DAY_UUID
        ));
    }

    #[test]
    fn an_empty_file_is_unreadable() {
        assert!(matches!(read(""), LimitStopRead::Unreadable(_)));
        assert!(matches!(read("\n\n  \n"), LimitStopRead::Unreadable(_)));
    }

    #[test]
    fn a_truncated_final_line_is_unreadable() {
        let mut body = SEVEN_DAY_STOP.to_string();
        body.push_str(r#"{"type":"assistant","isSidechain":false,"uuid":"dd"#);
        assert!(matches!(read(&body), LimitStopRead::Unreadable(_)));
        // The same partial line, newline-terminated, is still line 1 from the
        // end: the final newline does not count as an empty line.
        body.push('\n');
        assert!(matches!(
            read(&body),
            LimitStopRead::Unreadable(ref why) if why.starts_with("transcript line 1 from the end")
        ));
    }

    #[test]
    fn a_non_utf8_file_is_unreadable() {
        let mut body = SEVEN_DAY_STOP.as_bytes().to_vec();
        body.extend_from_slice(b"{\"type\":\"user\",\"x\":\"\xff\xfe\"}\n");
        let f = write_transcript(&body);
        assert!(matches!(
            transcript_limit_stop(f.path()),
            LimitStopRead::Unreadable(_)
        ));
    }

    #[test]
    fn a_missing_file_is_unreadable() {
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(
            transcript_limit_stop(&dir.path().join("absent.jsonl")),
            LimitStopRead::Unreadable(_)
        ));
    }

    #[test]
    fn a_transcript_with_no_conversation_is_not_stopped() {
        let body = SEVEN_DAY_STOP
            .lines()
            .filter(|l| !l.contains("\"type\":\"user\"") && !l.contains("\"type\":\"assistant\""))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(read(&body), LimitStopRead::NotStopped);
    }

    /// A 40 MB transcript is decided on its 256 KiB tail. The structural span
    /// assertion is the bound; the wall-clock one is a generous backstop.
    #[test]
    fn a_forty_megabyte_transcript_is_decided_on_its_tail() {
        let filler = assistant_text("eeeeeeee-0000-4000-8000-000000000004", &"x".repeat(4096));
        let mut body = String::with_capacity(41 * 1024 * 1024);
        while body.len() < 40 * 1024 * 1024 {
            body.push_str(&filler);
            body.push('\n');
        }
        body.push_str(SEVEN_DAY_STOP);
        let f = write_transcript(body.as_bytes());
        assert!(f.as_file().metadata().unwrap().len() >= 40 * 1024 * 1024);

        let started = Instant::now();
        let (read, span) = read_limit_stop(f.path());
        let elapsed = started.elapsed();
        assert_eq!(span, ReadSpan::Tail(TAIL_BYTES));
        assert!(matches!(read, LimitStopRead::Stopped(ref s) if s.entry_uuid == SEVEN_DAY_UUID));
        assert!(elapsed < Duration::from_secs(10), "took {elapsed:?}");
    }

    /// A tail that starts exactly on a line boundary keeps its first line.
    #[test]
    fn a_tail_starting_on_a_line_boundary_keeps_its_first_line() {
        let pad = format!(
            "{{\"type\":\"cost-state\",\"pad\":\"{}\"}}\n",
            "y".repeat(1024)
        );
        let mut prefix = String::new();
        while prefix.len() < 4096 {
            prefix.push_str(&pad);
        }
        // The deciding entry alone, padded with trailing spaces so the whole
        // tail is exactly TAIL_BYTES and starts at its first byte.
        let entry = seven_day_error_entry().to_string();
        let tail = format!(
            "{entry}{}\n",
            " ".repeat(TAIL_BYTES as usize - entry.len() - 1)
        );
        assert_eq!(tail.len() as u64, TAIL_BYTES);
        let f = write_transcript(format!("{prefix}{tail}").as_bytes());
        let (read, span) = read_limit_stop(f.path());
        assert_eq!(span, ReadSpan::Tail(TAIL_BYTES));
        assert!(matches!(read, LimitStopRead::Stopped(ref s) if s.entry_uuid == SEVEN_DAY_UUID));
    }

    /// `filler` repeated until it is at least `bytes` long.
    fn conversation_of(bytes: usize) -> String {
        let filler = assistant_text("eeeeeeee-0000-4000-8000-000000000005", &"z".repeat(4096));
        let mut body = String::with_capacity(bytes + filler.len() + 1);
        while body.len() < bytes {
            body.push_str(&filler);
            body.push('\n');
        }
        body
    }

    /// The measured shape: an `attachment` line (`isSidechain: false`) of more
    /// than 256 KiB after the stop. The first window holds no conversational
    /// entry, so the read widens to 1 MiB and still decides `Stopped`.
    #[test]
    fn a_stop_under_a_large_attachment_widens_the_window() {
        let big = format!("\"stdout\":\"{}\"", "x".repeat(300 * 1024));
        assert_eq!(SEVEN_DAY_STOP.matches("\"stdout\":\"\"").count(), 1);
        let stop = SEVEN_DAY_STOP.replace("\"stdout\":\"\"", &big);
        let body = format!("{}{stop}", conversation_of(2 * 1024 * 1024));
        let f = write_transcript(body.as_bytes());
        let (read, span) = read_limit_stop(f.path());
        assert_eq!(span, ReadSpan::Tail(1024 * 1024));
        assert!(matches!(read, LimitStopRead::Stopped(ref s) if s.entry_uuid == SEVEN_DAY_UUID));
    }

    /// A tail of nothing but bookkeeping falls back to the whole file.
    #[test]
    fn a_tail_with_no_conversation_falls_back_to_the_whole_file() {
        let mut body = String::from(SEVEN_DAY_STOP);
        let pad = format!(
            "{{\"type\":\"cost-state\",\"pad\":\"{}\"}}\n",
            "y".repeat(1024)
        );
        while (body.len() as u64) < TAIL_BYTES * 2 {
            body.push_str(&pad);
        }
        // The tail offset lands inside a pad line, so the partial-line drop
        // is exercised by construction, not by chance.
        let offset = body.len() - TAIL_BYTES as usize;
        assert_ne!(body.as_bytes()[offset - 1], b'\n');
        let f = write_transcript(body.as_bytes());
        let (read, span) = read_limit_stop(f.path());
        assert_eq!(span, ReadSpan::Whole);
        assert!(matches!(read, LimitStopRead::Stopped(ref s) if s.entry_uuid == SEVEN_DAY_UUID));
    }

    // ── The park list (plan 2026-11-05 Phase 2) ──────────────────────────────

    #[test]
    fn a_park_is_due_at_its_time_and_not_before() {
        let mut parks = ParkList::default();
        parks.park("t1", "s1", 1_000, "not-yet-reset");
        assert!(parks.take_due(999).is_empty());
        assert_eq!(
            parks.take_due(1_000),
            vec![Park {
                terminal_id: "t1".to_string(),
                claude_session_id: "s1".to_string(),
                due_at_ms: 1_000,
            }]
        );
        assert!(parks.get("t1").is_none());
    }

    #[test]
    fn due_parks_come_out_oldest_first_and_a_repark_replaces() {
        let mut parks = ParkList::default();
        parks.park("t2", "s2", 2_000, "not-yet-reset");
        parks.park("t1", "s1", 3_000, "not-yet-reset");
        parks.park("t1", "s1", 1_000, "not-yet-reset");
        parks.park("t3", "s3", 9_000, "not-yet-reset");
        let due: Vec<(String, i64)> = parks
            .take_due(5_000)
            .into_iter()
            .map(|p| (p.terminal_id, p.due_at_ms))
            .collect();
        assert_eq!(
            due,
            vec![("t1".to_string(), 1_000), ("t2".to_string(), 2_000)]
        );
        assert_eq!(parks.get("t3").map(|p| p.due_at_ms), Some(9_000));
    }

    #[test]
    fn a_park_reports_once_per_reason_even_across_a_drain() {
        let mut parks = ParkList::default();
        assert!(parks.park("t1", "s1", 1_000, "probe-unknown"));
        assert!(!parks.park("t1", "s1", 2_000, "probe-unknown"));
        // Taken by the re-entry and parked again for the same reason: quiet.
        assert_eq!(parks.take_due(5_000).len(), 1);
        assert!(!parks.park("t1", "s1", 6_000, "probe-unknown"));
        // A new reason is reported.
        assert!(parks.park("t1", "s1", 7_000, "not-yet-reset"));
        assert!(parks.park("t2", "s2", 7_000, "not-yet-reset"));
        // Once its stop is answered, the pane's park and reason are gone.
        parks.forget("t1");
        assert!(parks.get("t1").is_none());
        assert!(parks.park("t1", "s1", 9_000, "not-yet-reset"));
    }
}
