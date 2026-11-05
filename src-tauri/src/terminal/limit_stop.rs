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
//! `tools-used`, …). That entry is the state, so this module reads it.
//!
//! The read is bounded: the last [`TAIL_BYTES`] of the file, and the whole file
//! only when that tail holds no conversational entry. A transcript that cannot
//! be read or parsed where the decision is made is [`LimitStopRead::Unreadable`]
//! — UNKNOWN — and never [`LimitStopRead::NotStopped`].
//!
//! Interim arm: plan `2026-10-28-one-session-lifecycle-authority` Phase 99 (the
//! Migrator) is to carry this classifier into its `decide()`.

#![cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "no caller until plan 2026-11-05 Phase 2 splits handle_usage_limit_hint"
    )
)]

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use serde_json::Value;

/// How much of the transcript's end is read first. A limit stop is followed by
/// a handful of short bookkeeping lines, so the deciding entry sits well inside
/// this window in every measured transcript; a longer read happens only when
/// the tail holds no `user`/`assistant` entry at all.
pub(crate) const TAIL_BYTES: u64 = 256 * 1024;

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
    Tail,
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
                unreadable(format!("cannot open transcript: {e}")),
                ReadSpan::Whole,
            )
        }
    };
    let len = match file.metadata() {
        Ok(m) => m.len(),
        Err(e) => {
            return (
                unreadable(format!("cannot stat transcript: {e}")),
                ReadSpan::Whole,
            )
        }
    };

    if len > TAIL_BYTES {
        match read_from(&mut file, len - TAIL_BYTES) {
            Ok(bytes) => {
                // The tail almost always starts mid-line: drop the partial
                // first line, which cannot be parsed and is not the end.
                let start = bytes
                    .iter()
                    .position(|&b| b == b'\n')
                    .map_or(bytes.len(), |i| i + 1);
                match decide(&bytes[start..]) {
                    Decision::Decided(read) => return (read, ReadSpan::Tail),
                    // No conversational entry in the tail: read it all.
                    Decision::NoConversation => {}
                }
            }
            Err(e) => {
                return (
                    unreadable(format!("cannot read transcript tail: {e}")),
                    ReadSpan::Tail,
                )
            }
        }
    }

    let bytes = match read_from(&mut file, 0) {
        Ok(b) => b,
        Err(e) => {
            return (
                unreadable(format!("cannot read transcript: {e}")),
                ReadSpan::Whole,
            )
        }
    };
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return (
            unreadable("transcript is empty".to_string()),
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

fn unreadable(why: String) -> LimitStopRead {
    LimitStopRead::Unreadable(why)
}

fn read_from(file: &mut File, offset: u64) -> std::io::Result<Vec<u8>> {
    file.seek(SeekFrom::Start(offset))?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf)?;
    Ok(buf)
}

enum Decision {
    Decided(LimitStopRead),
    NoConversation,
}

/// Walk `bytes` from the end and decide on the last conversational entry.
fn decide(bytes: &[u8]) -> Decision {
    let text = match std::str::from_utf8(bytes) {
        Ok(t) => t,
        Err(e) => return Decision::Decided(unreadable(format!("transcript is not UTF-8: {e}"))),
    };
    for (from_end, line) in text.lines().rev().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let entry: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            // A line that does not parse may be the entry that decides (a
            // truncated final write, a corrupt record): that is UNKNOWN.
            Err(e) => {
                return Decision::Decided(unreadable(format!(
                    "transcript line {} from the end does not parse: {e}",
                    from_end + 1
                )))
            }
        };
        if !is_conversational(&entry) {
            continue;
        }
        return Decision::Decided(classify(&entry));
    }
    Decision::NoConversation
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
        return unreadable("usage-limit stop entry carries no uuid".to_string());
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
    LimitStopRead::Stopped(LimitStop {
        entry_uuid: entry_uuid.to_string(),
        resets_at_ms,
        rate_limit_type,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::time::{Duration, Instant};

    /// A seven-day stop: a user prompt, an assistant reply, the usage-limit
    /// API-error entry, then only bookkeeping lines (`system` `turn_duration`
    /// first, as in all 49 measured jan transcripts). The field set is the one
    /// the plan measured on merytshost (`apiError`, `apiErrorParams`,
    /// `apiErrorStatus`, `error: "rate_limit"`, `isApiErrorMessage`,
    /// `quotaLimits{status, resetsAt, rateLimitType, …}`); the values are
    /// reconstructed, since the authoring box held no limit-stop transcript.
    const SEVEN_DAY_STOP: &str = include_str!("fixtures/limit_stop/seven_day_stop.jsonl");
    /// The same shape on the five-hour window.
    const FIVE_HOUR_STOP: &str = include_str!("fixtures/limit_stop/five_hour_stop.jsonl");

    const SEVEN_DAY_UUID: &str = "7c1d2e3f-4a5b-4c6d-8e7f-90a1b2c3d4e5";
    /// 2026-11-03T20:00:00Z, jan's measured reset.
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

    /// A 40 MB transcript is decided on its 256 KiB tail, quickly.
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
        assert_eq!(span, ReadSpan::Tail);
        assert!(matches!(read, LimitStopRead::Stopped(ref s) if s.entry_uuid == SEVEN_DAY_UUID));
        assert!(elapsed < Duration::from_secs(2), "took {elapsed:?}");
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
        let f = write_transcript(body.as_bytes());
        let (read, span) = read_limit_stop(f.path());
        assert_eq!(span, ReadSpan::Whole);
        assert!(matches!(read, LimitStopRead::Stopped(ref s) if s.entry_uuid == SEVEN_DAY_UUID));
    }
}
