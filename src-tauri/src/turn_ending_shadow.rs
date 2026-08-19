//! Shadow mode for the premature-bailout detector — classify and RECORD, act
//! on nothing.
//!
//! This is the impure half of the design. The pure classifier lives in the lib
//! ([`qontinui_runner_lib::looping_agent::turn_ending`]); this module resolves a
//! looping agent's Claude Code transcript, pulls the turn-final assistant text
//! out of it, calls the classifier, and appends the verdict to the agent's
//! durable journal. **It changes no behaviour.** The false-positive rate on real
//! fleet traffic is unknown, and a control that wrongly flagged completed work
//! would be worse than the gap it fills, so nothing consumes these verdicts
//! until the shadow corpus has been reviewed by hand.
//!
//! ## Why the transcript and not the terminal grid
//!
//! The looping agent's only other observation surface is the rendered VT grid,
//! which cannot carry this input:
//!
//! - [`crate::terminal::output_scan::normalize`] — which every grid scanner runs
//!   — collapses each whitespace run **including newlines** to a single space,
//!   deliberately, so TUI wrapping cannot break a match. "Last non-empty
//!   paragraph" has nothing left to split on.
//! - The grid is a hard-wrapped viewport, so a long final paragraph is re-wrapped
//!   at the terminal width and may have scrolled off entirely.
//!
//! The transcript JSONL has the text as the model wrote it. The chain from a
//! looping agent to its own transcript is closed by
//! `LoopingAgentRuntime.claude_session_id` — the pinned `--session-id` of the
//! current spawn.
//!
//! ## Why the cap is 256 KB and not 4 KB
//!
//! [`crate::terminal::transcript::session_digest`] reads the last 4096 bytes
//! ("enough for 2-3 messages"), which is right for a preview and wrong here. A
//! bailing-out turn is characteristically a LONG prose turn — it summarizes real
//! work before declining the rest — so it is precisely the turn that overruns a
//! small window, and [`read_tail_bytes`] drops the leading partial line, meaning
//! the record we most want to read is the one most likely to be discarded.
//!
//! Returning `Complete` in that case would be the worst available failure: the
//! shadow corpus would look clean, and the Phase 3 review would greenlight on a
//! result that was never read. So a truncated or absent record is reported as
//! [`TurnEnding::Unknown`] with a reason, and counted in its own bucket.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use qontinui_runner_lib::looping_agent::turn_ending::{
    classify_turn_ending, TurnEnding, UnreadReason,
};

use crate::terminal::transcript::{
    find_claude_config_dirs, parse_assistant_record, read_tail_bytes, session_transcript_path,
};

/// How much of the transcript tail to read when classifying a turn ending.
///
/// Sized for a whole final assistant turn rather than a preview. `read_tail_bytes`
/// returns the entire file when it is smaller, so this only costs anything on
/// long-running sessions.
pub const DEFAULT_READ_CAP_BYTES: u64 = 256 * 1024;

/// One shadow-mode observation, appended to the looping agent's journal.
///
/// Field names are stable on purpose — the Phase 3 hand review reads this
/// corpus, and a rename would orphan everything recorded before it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ShadowRecord {
    /// Always `"turn_ending_shadow"`, so a mixed journal can be filtered.
    pub kind: String,
    pub agent_id: String,
    pub claude_session_id: String,
    pub observed_at_ms: i64,
    /// `complete` | `waiting_on_signal` | `user_deflection` | `bailout` | `unknown`.
    pub verdict: String,
    /// The `PATTERN_*` label, when a stop pattern matched.
    pub pattern: Option<String>,
    /// The observable-signal phrase, when one was found.
    pub signal: Option<String>,
    /// The `UnreadReason` label, when the text could not be classified.
    pub unread_reason: Option<String>,
    /// The byte cap this read used — so a corpus reviewed later can tell
    /// whether a re-read at a larger cap might change the verdict.
    pub read_cap_bytes: u64,
    /// Whether the transcript was larger than the cap (i.e. the read was
    /// bounded at all).
    pub truncated_read: bool,
    /// The classified paragraph, capped for the journal. This is what makes the
    /// Phase 3 review possible without re-deriving anything.
    pub paragraph: String,
}

/// Longest paragraph excerpt written into the journal.
const PARAGRAPH_JOURNAL_CAP: usize = 2000;

fn truncate_on_char_boundary(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

/// The turn-final assistant text for a session, or why it could not be read.
///
/// Never returns `Ok("")` — empty content is an [`UnreadReason`], because the
/// caller must not be able to confuse "nothing was said" with "nothing was
/// read".
pub fn read_turn_final_text(
    config_dir: &Path,
    project_path: &str,
    session_id: &str,
    cap_bytes: u64,
) -> Result<FinalText, UnreadReason> {
    let path = session_transcript_path(config_dir, project_path, session_id);
    read_turn_final_text_at(&path, cap_bytes)
}

/// The turn-final assistant text plus what the caller needs to judge it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FinalText {
    /// The assistant record's text.
    pub text: String,
    /// Whether the transcript was larger than the read cap.
    pub truncated_read: bool,
    /// The record's own uuid — the identity the observer dedupes on, so a
    /// 5-second tick does not re-record one turn ending a hundred times.
    pub uuid: String,
}

/// [`read_turn_final_text`] over an already-resolved transcript path.
pub fn read_turn_final_text_at(path: &Path, cap_bytes: u64) -> Result<FinalText, UnreadReason> {
    if !path.exists() {
        return Err(UnreadReason::TranscriptMissing);
    }

    // Did we bound the read at all? Needed to tell TruncatedAtCap from a
    // genuinely absent record below.
    let truncated_read = std::fs::metadata(path)
        .map(|m| m.len() > cap_bytes)
        .unwrap_or(false);

    let content = read_tail_bytes(path, cap_bytes).ok_or(UnreadReason::ReadError)?;

    let mut last_assistant: Option<(String, String)> = None;
    let mut saw_any_record = false;

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(record) = serde_json::from_str::<serde_json::Value>(line) else {
            // A malformed line inside the window is noise, not a verdict — the
            // leading PARTIAL line was already dropped by read_tail_bytes, so
            // this is a genuinely bad record. Skip it, as session_digest does.
            continue;
        };
        saw_any_record = true;
        if record.get("type").and_then(|t| t.as_str()) == Some("assistant") {
            if let Some(msg) = parse_assistant_record(&record) {
                last_assistant = Some((msg.text, msg.uuid));
            }
        }
    }

    match last_assistant {
        Some((text, uuid)) if !text.trim().is_empty() => Ok(FinalText {
            text,
            truncated_read,
            uuid,
        }),
        // Distinguish "we cut the file and the record was in the cut part" from
        // "the record genuinely is not there". Only the second is a fact about
        // the session; the first is a fact about our read.
        _ if truncated_read => Err(UnreadReason::TruncatedAtCap),
        _ if saw_any_record => Err(UnreadReason::NoAssistantRecord),
        _ => Err(UnreadReason::MalformedRecord),
    }
}

/// Classify a session's turn ending. Pure classification over an impure read.
///
/// Any read failure becomes [`TurnEnding::Unknown`] — never `Complete`.
pub fn classify_session_turn_ending(
    config_dir: &Path,
    project_path: &str,
    session_id: &str,
    cap_bytes: u64,
) -> (TurnEnding, bool) {
    match read_turn_final_text(config_dir, project_path, session_id, cap_bytes) {
        Ok(ft) => (classify_turn_ending(&ft.text), ft.truncated_read),
        Err(reason) => (TurnEnding::Unknown { reason }, false),
    }
}

/// Per-agent identity of the LAST turn ending recorded, so a repeated idle tick
/// is not repeatedly recorded.
///
/// The supervisor ticks every ~5s and `idle` stays true for as long as the agent
/// sits at its prompt, so without this one turn ending would be journalled
/// dozens or hundreds of times — inflating every verdict by however long that
/// agent happened to idle and making the Phase 3 false-positive rate
/// uncountable, which is the one number the shadow corpus exists to produce.
static LAST_OBSERVED: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);

/// Record `key` as the newest observation for `agent_id`; return whether it is
/// NEW (i.e. whether the caller should journal it).
fn claim_observation(agent_id: &str, key: &str) -> bool {
    let mut guard = match LAST_OBSERVED.lock() {
        Ok(g) => g,
        // A poisoned lock must not silence observation — record and move on.
        Err(p) => p.into_inner(),
    };
    let map = guard.get_or_insert_with(HashMap::new);
    match map.get(agent_id) {
        Some(prev) if prev == key => false,
        _ => {
            map.insert(agent_id.to_string(), key.to_string());
            true
        }
    }
}

/// Build the journal record for a verdict.
pub fn shadow_record(
    agent_id: &str,
    claude_session_id: &str,
    observed_at_ms: i64,
    ending: &TurnEnding,
    paragraph: &str,
    cap_bytes: u64,
    truncated_read: bool,
) -> ShadowRecord {
    let (pattern, signal, unread_reason) = match ending {
        TurnEnding::Complete => (None, None, None),
        TurnEnding::WaitingOnSignal { signal } => (None, Some(signal.clone()), None),
        TurnEnding::UserDeflection { pattern } => (Some(pattern.label().to_string()), None, None),
        TurnEnding::Bailout { pattern } => (Some(pattern.label().to_string()), None, None),
        TurnEnding::Unknown { reason } => (None, None, Some(reason.label().to_string())),
    };
    ShadowRecord {
        kind: "turn_ending_shadow".to_string(),
        agent_id: agent_id.to_string(),
        claude_session_id: claude_session_id.to_string(),
        observed_at_ms,
        verdict: ending.kind_label().to_string(),
        pattern,
        signal,
        unread_reason,
        read_cap_bytes: cap_bytes,
        truncated_read,
        paragraph: truncate_on_char_boundary(paragraph, PARAGRAPH_JOURNAL_CAP),
    }
}

/// Append one shadow record to the agent's durable journal (JSONL).
///
/// Best-effort: a failure here must never disturb the supervisor tick — shadow
/// mode is observation, and losing an observation is strictly better than
/// perturbing the loop it is observing.
pub fn append_shadow_record(journal_path: &str, record: &ShadowRecord) {
    let Ok(line) = serde_json::to_string(record) else {
        warn!("turn_ending_shadow: record failed to serialize; dropping");
        return;
    };
    if let Some(parent) = Path::new(journal_path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    use std::io::Write;
    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(journal_path)
    {
        Ok(mut f) => {
            if let Err(e) = writeln!(f, "{line}") {
                warn!(error = %e, journal = %journal_path, "turn_ending_shadow: journal append failed");
            }
        }
        Err(e) => {
            warn!(error = %e, journal = %journal_path, "turn_ending_shadow: journal open failed");
        }
    }
}

/// Locate the on-disk transcript for `session_id` WITHOUT knowing its project
/// path.
///
/// `session_transcript_path` needs `(config_dir, project_path, session_id)`, but
/// the looping-agent registry records only the pinned `claude_session_id`. The
/// session id is unique, so sweep every Claude config dir on this machine
/// (`find_claude_config_dirs` is MACHINE-scoped — it enumerates every account's
/// tree, not just this session's) and look for `<session_id>.jsonl` under any
/// `projects/<encoded>/`. Returns the config dir, the encoded project directory
/// name, and the transcript path.
pub fn resolve_transcript(session_id: &str) -> Option<(PathBuf, String)> {
    let file = format!("{session_id}.jsonl");
    for config_dir in find_claude_config_dirs() {
        let projects = config_dir.join("projects");
        let Ok(entries) = std::fs::read_dir(&projects) else {
            continue;
        };
        for entry in entries.flatten() {
            if !entry.path().is_dir() {
                continue;
            }
            if entry.path().join(&file).is_file() {
                let encoded = entry.file_name().to_string_lossy().to_string();
                return Some((config_dir, encoded));
            }
        }
    }
    None
}

/// The whole shadow observation for one idle tick: read, classify, record.
///
/// Returns the verdict when one was newly recorded, or `None` when this tick
/// saw the SAME turn ending already journalled for this agent (the common case
/// — the supervisor ticks every ~5s and `idle` stays true while the agent waits
/// at its prompt).
///
/// **NOTHING acts on the verdict.** Shadow mode records and returns; the
/// caller logs.
///
/// Fail-soft at every step: an unresolvable transcript is RECORDED as
/// `Unknown { TranscriptMissing }` rather than skipped, so the Phase 3 review
/// can see how often the read itself failed. Silently skipping would make a
/// broken reader indistinguishable from a healthy fleet.
pub fn observe_turn_ending(
    agent_id: &str,
    journal_path: &str,
    claude_session_id: &str,
    observed_at_ms: i64,
) -> Option<TurnEnding> {
    let Some((config_dir, encoded_project)) = resolve_transcript(claude_session_id) else {
        return record_if_new(
            agent_id,
            journal_path,
            claude_session_id,
            observed_at_ms,
            TurnEnding::Unknown {
                reason: UnreadReason::TranscriptMissing,
            },
            String::new(),
            false,
            &format!("unread:{}", UnreadReason::TranscriptMissing.label()),
        );
    };

    // `resolve_transcript` returns the ALREADY-ENCODED project directory name,
    // and `session_transcript_path` encodes what it is given — so re-encoding
    // would mangle it. Build the path directly here instead.
    let path = config_dir
        .join("projects")
        .join(&encoded_project)
        .join(format!("{claude_session_id}.jsonl"));

    match read_turn_final_text_at(&path, DEFAULT_READ_CAP_BYTES) {
        Ok(ft) => {
            let paragraph =
                qontinui_runner_lib::looping_agent::turn_ending::last_non_empty_paragraph(&ft.text)
                    .unwrap_or_default()
                    .to_string();
            let ending = classify_turn_ending(&ft.text);
            let key = format!("uuid:{}", ft.uuid);
            record_if_new(
                agent_id,
                journal_path,
                claude_session_id,
                observed_at_ms,
                ending,
                paragraph,
                ft.truncated_read,
                &key,
            )
        }
        Err(reason) => record_if_new(
            agent_id,
            journal_path,
            claude_session_id,
            observed_at_ms,
            TurnEnding::Unknown { reason },
            String::new(),
            false,
            &format!("unread:{}", reason.label()),
        ),
    }
}

/// Journal `ending` unless `key` matches what was already recorded for this
/// agent. Returns the ending when it was recorded, `None` when deduped.
#[allow(clippy::too_many_arguments)]
fn record_if_new(
    agent_id: &str,
    journal_path: &str,
    claude_session_id: &str,
    observed_at_ms: i64,
    ending: TurnEnding,
    paragraph: String,
    truncated_read: bool,
    key: &str,
) -> Option<TurnEnding> {
    if !claim_observation(agent_id, key) {
        return None;
    }
    let record = shadow_record(
        agent_id,
        claude_session_id,
        observed_at_ms,
        &ending,
        &paragraph,
        DEFAULT_READ_CAP_BYTES,
        truncated_read,
    );
    append_shadow_record(journal_path, &record);
    debug!(
        agent = %agent_id,
        verdict = %record.verdict,
        pattern = ?record.pattern,
        unread_reason = ?record.unread_reason,
        "turn_ending_shadow: observed (SHADOW MODE - no action taken)"
    );
    Some(ending)
}

#[cfg(test)]
mod tests {
    use super::*;
    use qontinui_runner_lib::looping_agent::turn_ending::PatternId;
    use std::fs;

    /// Build a throwaway Claude-Code-shaped config dir with one transcript.
    fn fixture(project_path: &str, session_id: &str, lines: &[String]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = session_transcript_path(dir.path(), project_path, session_id);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, lines.join("\n")).unwrap();
        dir
    }

    fn assistant_line(text: &str) -> String {
        serde_json::json!({
            "type": "assistant",
            "uuid": "u1",
            "timestamp": "2026-08-19T00:00:00.000Z",
            "message": { "role": "assistant", "content": [{ "type": "text", "text": text }] }
        })
        .to_string()
    }

    #[test]
    fn reads_the_last_assistant_text_and_classifies_it() {
        let d = fixture(
            "D:/proj",
            "s1",
            &[
                assistant_line("first turn, all fine"),
                assistant_line("Did the work.\n\nI'll stop here."),
            ],
        );
        let (ending, truncated) =
            classify_session_turn_ending(d.path(), "D:/proj", "s1", DEFAULT_READ_CAP_BYTES);
        assert!(!truncated);
        assert_eq!(
            ending,
            TurnEnding::Bailout {
                pattern: PatternId::StoppingHere
            }
        );
    }

    #[test]
    fn a_missing_transcript_is_unknown_not_complete() {
        let d = tempfile::tempdir().unwrap();
        let (ending, _) =
            classify_session_turn_ending(d.path(), "D:/proj", "nope", DEFAULT_READ_CAP_BYTES);
        assert_eq!(
            ending,
            TurnEnding::Unknown {
                reason: UnreadReason::TranscriptMissing
            }
        );
    }

    /// The §3.1.2 case: the final assistant record is longer than the read cap,
    /// so `read_tail_bytes` drops it as a partial line. This MUST report
    /// `TruncatedAtCap`, never `Complete` — otherwise the shadow corpus looks
    /// clean because it was never read.
    #[test]
    fn a_record_larger_than_the_cap_is_truncated_not_complete() {
        let long = "x".repeat(8000);
        let d = fixture(
            "D:/proj",
            "s1",
            &[assistant_line(&long), assistant_line(&long)],
        );
        let (ending, _) = classify_session_turn_ending(d.path(), "D:/proj", "s1", 4096);
        assert_eq!(
            ending,
            TurnEnding::Unknown {
                reason: UnreadReason::TruncatedAtCap
            },
            "a turn that overruns the cap must NOT read as Complete"
        );
    }

    #[test]
    fn the_same_transcript_classifies_fine_at_the_real_cap() {
        // Same fixture, default cap: proves the 4 KB failure above is about the
        // CAP, not the content — and that 256 KB is the fix.
        let long = "x".repeat(8000);
        let d = fixture(
            "D:/proj",
            "s1",
            &[
                assistant_line(&long),
                assistant_line(&format!("{long}\n\nI am unable to proceed.")),
            ],
        );
        let (ending, truncated) =
            classify_session_turn_ending(d.path(), "D:/proj", "s1", DEFAULT_READ_CAP_BYTES);
        assert!(!truncated);
        assert_eq!(
            ending,
            TurnEnding::Bailout {
                pattern: PatternId::UnableToProceed
            }
        );
    }

    #[test]
    fn a_transcript_with_no_assistant_record_is_unknown() {
        let user = serde_json::json!({
            "type": "user", "uuid": "u", "timestamp": "2026-08-19T00:00:00.000Z",
            "message": {"role": "user", "content": "hi"}
        })
        .to_string();
        let d = fixture("D:/proj", "s1", &[user]);
        let (ending, _) =
            classify_session_turn_ending(d.path(), "D:/proj", "s1", DEFAULT_READ_CAP_BYTES);
        assert!(matches!(ending, TurnEnding::Unknown { .. }));
    }

    #[test]
    fn journal_records_round_trip_and_keep_stable_keys() {
        let ending = TurnEnding::Bailout {
            pattern: PatternId::GivingUp,
        };
        let r = shadow_record("agent-1", "s1", 1234, &ending, "I'm giving up.", 4096, true);
        assert_eq!(r.kind, "turn_ending_shadow");
        assert_eq!(r.verdict, "bailout");
        assert_eq!(r.pattern.as_deref(), Some("PATTERN_GIVING_UP"));
        assert!(r.truncated_read);
        let json = serde_json::to_string(&r).unwrap();
        let back: ShadowRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(back, r);
        // The reviewer must be able to filter on these exact keys.
        for key in [
            "\"kind\"",
            "\"verdict\"",
            "\"pattern\"",
            "\"unread_reason\"",
            "\"read_cap_bytes\"",
            "\"truncated_read\"",
            "\"paragraph\"",
        ] {
            assert!(json.contains(key), "journal key {key} missing");
        }
    }

    #[test]
    fn unknown_records_carry_their_reason_for_the_corpus_tally() {
        let ending = TurnEnding::Unknown {
            reason: UnreadReason::TruncatedAtCap,
        };
        let r = shadow_record("a", "s", 0, &ending, "", 4096, true);
        assert_eq!(r.verdict, "unknown");
        assert_eq!(r.unread_reason.as_deref(), Some("truncated_at_cap"));
        assert_eq!(r.pattern, None);
    }

    #[test]
    fn appending_to_the_journal_is_line_delimited_and_creates_parents() {
        let d = tempfile::tempdir().unwrap();
        let jp = d
            .path()
            .join("nested")
            .join("journal.jsonl")
            .to_string_lossy()
            .to_string();
        for i in 0..3 {
            let r = shadow_record("a", "s", i, &TurnEnding::Complete, "done", 1024, false);
            append_shadow_record(&jp, &r);
        }
        let body = fs::read_to_string(&jp).unwrap();
        assert_eq!(body.lines().count(), 3);
        for line in body.lines() {
            let _: ShadowRecord = serde_json::from_str(line).expect("each line parses");
        }
    }

    #[test]
    fn the_same_turn_ending_is_recorded_once_not_once_per_tick() {
        // The supervisor ticks every ~5s and `idle` stays true while the agent
        // waits at its prompt, so without dedupe one ending would be journalled
        // dozens of times and every verdict would scale with idle time.
        //
        // Driven through `record_if_new` rather than `observe_turn_ending` on
        // purpose: pointing the resolver at a fixture needs $CLAUDE_CONFIG_DIR,
        // and this crate has no serial-test guard, so an env-mutating test
        // would race every other test that reads it.
        let jd = tempfile::tempdir().unwrap();
        let jp = jd.path().join("j.jsonl").to_string_lossy().to_string();
        let agent = "agent-tick-dedupe";
        let ending = TurnEnding::Bailout {
            pattern: PatternId::StoppingHere,
        };

        let mut recorded = 0;
        for tick in 0..25 {
            // Same turn, 25 consecutive idle ticks, same record uuid.
            if record_if_new(
                agent,
                &jp,
                "s1",
                tick,
                ending.clone(),
                "I'll stop here.".to_string(),
                false,
                "uuid:abc",
            )
            .is_some()
            {
                recorded += 1;
            }
        }
        assert_eq!(recorded, 1, "25 idle ticks must record ONE observation");
        assert_eq!(
            std::fs::read_to_string(&jp).unwrap().lines().count(),
            1,
            "one turn ending must produce exactly one journal row"
        );

        // A genuinely new turn re-arms and is recorded.
        assert!(record_if_new(
            agent,
            &jp,
            "s1",
            99,
            TurnEnding::Complete,
            "all done".to_string(),
            false,
            "uuid:def",
        )
        .is_some());
        assert_eq!(std::fs::read_to_string(&jp).unwrap().lines().count(), 2);
    }

    #[test]
    fn dedupe_is_per_agent_not_global() {
        assert!(claim_observation("agent-a", "uuid:1"));
        assert!(!claim_observation("agent-a", "uuid:1"));
        // A different agent with the same key is a genuinely different
        // observation and must still be recorded.
        assert!(claim_observation("agent-b", "uuid:1"));
        // A new ending for agent-a re-arms.
        assert!(claim_observation("agent-a", "uuid:2"));
    }

    #[test]
    fn paragraph_excerpt_is_capped_on_a_char_boundary() {
        let r = shadow_record(
            "a",
            "s",
            0,
            &TurnEnding::Complete,
            &"é".repeat(4000),
            1024,
            false,
        );
        assert!(r.paragraph.len() <= PARAGRAPH_JOURNAL_CAP + 4);
        assert!(r.paragraph.ends_with('…'));
    }
}
