//! What a Claude Code transcript says happened AFTER a finish — the newest real
//! prompt, the newest record that opened a turn, and the newest assistant
//! line — read from the tail of the NEWEST copy of the session's transcript.
//!
//! Plan
//! `2026-11-16-finished-close-mints-its-close-authority-from-the-served-close-conditions`,
//! D6 and Phase 1. The close guard's revival arm
//! ([`super::finished_close_guard`]) judges these against coord's finish stamp,
//! by the one rule `finish_state` already applies in
//! `scripts/close-finished-sessions.py` (served `finished-criteria` condition
//! 4: a turn newer than the finish keeps the session open).
//!
//! ## Why not [`super::transcript_last_turn::last_turn_ms`]
//!
//! It returns the time of the last real `user`/`assistant` line. Against that
//! time the finish turn's own trailing reply reads as a revival, and a turn
//! opener that is not a prompt (a Session Bus delivery, a `/loop` or
//! ScheduleWakeup wake, a queued message) is missed. This reader keeps the
//! three times apart, mirroring `transcript_facts` / `turn_open` /
//! `opener_class` in that script.
//!
//! ## What counts
//!
//! * **A prompt** (`last_prompt_ms`): a `user` line that is not `isMeta`, not
//!   `isCompactSummary`, whose content carries no `tool_result`, whose text is
//!   not empty, and does not start with one of [`NON_PROMPT`]. A built-in
//!   command Claude Code answers itself (`<command-name>/model` followed by a
//!   `<local-command-stdout>` record) is taken back off.
//! * **A turn opener** (`last_turn_open_ms`): any `user` line that opens a turn
//!   outside a running one — a prompt, a Session Bus delivery (a non-meta
//!   `<system-reminder>Session Bus …` line), a peer message, a
//!   `<task-notification>`, a `/loop` or ScheduleWakeup wake (a META line) —
//!   and a `queue-operation` `enqueue` record, which is input queued for the
//!   session. A `tool_result`, a compaction summary, a line inside a running
//!   turn, and [`NOT_A_TURN`] lines (a meta `<system-reminder>`, a local
//!   command's caveat or output, an interrupt) open none. Counting the queue
//!   record is the conservative direction: it can only hold a close.
//! * **Assistant work** (`last_asst_ms`): the newest `assistant` line.
//!
//! A compaction summary is none of the three: Claude Code compacts an IDLE
//! session on its own, writing an `isCompactSummary` user line with no turn
//! around it (finding `2913808f`: one at +3284 s past a finish).
//!
//! ## Which copy, and how much of it
//!
//! An account migration copies a transcript into another account root and
//! resumes the same id there, so one id can have several copies — Phase 0
//! measured one with four, whose oldest ended 34 h before the finish stamp.
//! Reading a stale copy would miss the revival entirely, so every root is
//! searched FRESH (no index TTL) and the copies are ordered by
//! [`crate::agent_worktree::custody::order_by_newest_turn`], the one ordering
//! `--resume` itself relies on; the first is read.
//!
//! Only the tail is read: [`TAIL_WIDE`] first, and [`TAIL_DEEP`] once when that
//! window does not reach back to the finish stamp. Each window's answer is
//! cached per path on the file's [`FileStamp`] — the same `(len, mtime)` key
//! [`super::transcript_last_turn`] uses — so an unchanged file is never re-read.
//! [`TranscriptFacts::window_floor_ms`] says how far back a window reached; a
//! window that reached neither the start of the file nor the stamp cannot rule
//! a revival out, and the guard reads that as UNKNOWN.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use super::transcript_last_turn::{last_turn_ms, FileStamp, ReadBudget, TAIL_WIDE};
use crate::agent_worktree::custody::{order_by_newest_turn, TranscriptCopy};
use qontinui_runner_lib::file_tail::read_tail_lossy;

/// The second, deeper window, read once when [`TAIL_WIDE`] did not reach back
/// to the finish stamp.
pub const TAIL_DEEP: u64 = 16 * 1024 * 1024;

/// User lines that are not a new instruction (`NON_PROMPT` in the script).
pub const NON_PROMPT: [&str; 6] = [
    "<task-notification>",
    "This session is being continued",
    "<local-command",
    "Caveat:",
    "<system-reminder>",
    "[Request interrupted",
];

/// User lines that open no turn even outside one (`NOT_A_TURN` in the script);
/// a `<system-reminder>` one only when it is META — a non-meta one is a Session
/// Bus delivery, which does open a turn.
pub const NOT_A_TURN: [&str; 4] = [
    "<local-command",
    "Caveat:",
    "<system-reminder>",
    "[Request interrupted",
];

/// The record Claude Code writes after a built-in command it answered itself.
const LOCAL_OUTPUT: [&str; 2] = ["<local-command-stdout", "<local-command-stderr"];

/// What one tail window of one transcript established. Every time is unix ms.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TranscriptFacts {
    /// The newest real prompt.
    pub last_prompt_ms: Option<i64>,
    /// The newest record that opened a turn, of any kind.
    pub last_turn_open_ms: Option<i64>,
    /// The newest assistant line.
    pub last_asst_ms: Option<i64>,
    /// The oldest timestamp the window held — how far back it reached.
    pub window_floor_ms: Option<i64>,
    /// The window held the whole file, so nothing older exists.
    pub whole_file: bool,
}

impl TranscriptFacts {
    /// Did this window reach back to `stamp_ms - band_ms`? Only then can it
    /// rule a turn after the stamp in or out.
    pub fn reaches(&self, stamp_ms: i64, band_ms: i64) -> bool {
        self.whole_file
            || self
                .window_floor_ms
                .is_some_and(|floor| floor <= stamp_ms.saturating_sub(band_ms))
    }
}

fn starts_with_any(text: &str, prefixes: &[&str]) -> bool {
    prefixes.iter().any(|p| text.starts_with(p))
}

/// Strip one leading `<pasted_content …>` wrapper (`PASTED_RE` in the script).
fn unwrap_pasted(text: &str) -> &str {
    if let Some(rest) = text.strip_prefix("<pasted_content") {
        if let Some(end) = rest.find('>') {
            return rest[end + 1..].trim_start();
        }
    }
    text
}

/// Fold a transcript tail into [`TranscriptFacts`]. Pure. `whole_file` says
/// the tail is the entire file. Lines that are not JSON (a window's partial
/// first line) or carry no parseable `timestamp` are skipped.
pub fn facts_from_tail(tail: &str, whole_file: bool) -> TranscriptFacts {
    use serde_json::Value;
    let mut f = TranscriptFacts {
        whole_file,
        ..TranscriptFacts::default()
    };
    // A turn is running: records inside it (a tool result, a skill body
    // injected after a Skill call) open none.
    let mut turn_in = false;
    // What the newest opener / prompt replaced, so a `<command-name>` answered
    // locally can be taken back off.
    let mut turn_before: Option<(Option<i64>, String)> = None;
    let mut turn_text = String::new();
    let mut prompt_before: Option<(Option<i64>, String)> = None;
    let mut prompt_text = String::new();
    for line in tail.lines() {
        let Ok(e) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Some(at) = e
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(|ts| chrono::DateTime::parse_from_rfc3339(ts).ok())
            .map(|dt| dt.timestamp_millis())
        else {
            continue;
        };
        f.window_floor_ms = Some(f.window_floor_ms.map_or(at, |floor| floor.min(at)));
        let kind = e.get("type").and_then(Value::as_str).unwrap_or("");
        if kind == "queue-operation" {
            if e.get("operation").and_then(Value::as_str) == Some("enqueue") {
                f.last_turn_open_ms = Some(f.last_turn_open_ms.map_or(at, |t| t.max(at)));
            }
            continue;
        }
        let Some(message) = e.get("message").filter(|m| m.is_object()) else {
            continue;
        };
        let content = message.get("content");
        let flag = |key: &str| e.get(key).and_then(Value::as_bool) == Some(true);
        match kind {
            "assistant" => {
                prompt_before = None;
                turn_in = message.get("stop_reason").and_then(Value::as_str) == Some("tool_use");
                turn_before = None;
                f.last_asst_ms = Some(at);
            }
            "user" => {
                let has_tool_result = content
                    .and_then(Value::as_array)
                    .is_some_and(|blocks| {
                        blocks
                            .iter()
                            .any(|b| b.get("type").and_then(Value::as_str) == Some("tool_result"))
                    });
                let text: String = match content {
                    Some(Value::String(s)) => s.clone(),
                    Some(Value::Array(blocks)) => blocks
                        .iter()
                        .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
                        .filter_map(|b| b.get("text").and_then(Value::as_str))
                        .collect::<Vec<_>>()
                        .join(" "),
                    _ => String::new(),
                };
                let t = text.trim_start();
                let meta = flag("isMeta");
                let compact = flag("isCompactSummary");

                // Turn openers (`turn_open` in the script).
                if starts_with_any(t, &LOCAL_OUTPUT) {
                    if let Some((at_before, text_before)) = turn_before.take() {
                        if turn_text.starts_with("<command-name>") {
                            f.last_turn_open_ms = at_before;
                            turn_text = text_before;
                        }
                    }
                } else if t.starts_with("[Request interrupted") {
                    turn_in = false;
                } else {
                    let not_a_turn = starts_with_any(t, &NOT_A_TURN)
                        && (meta || !t.starts_with("<system-reminder>"));
                    if !(turn_in || t.is_empty() || compact || not_a_turn || has_tool_result) {
                        turn_before = Some((f.last_turn_open_ms, turn_text.clone()));
                        f.last_turn_open_ms = Some(at);
                        turn_text = unwrap_pasted(t).to_string();
                    }
                }

                // Prompts (`last_prompt` in the script, minus tool results).
                if meta || compact || has_tool_result {
                    continue;
                }
                if starts_with_any(t, &LOCAL_OUTPUT) {
                    if let Some((at_before, text_before)) = prompt_before.take() {
                        if prompt_text.starts_with("<command-name>") {
                            f.last_prompt_ms = at_before;
                            prompt_text = text_before;
                        }
                    }
                } else if !text.trim().is_empty() && !starts_with_any(t, &NON_PROMPT) {
                    prompt_before = Some((f.last_prompt_ms, prompt_text.clone()));
                    f.last_prompt_ms = Some(at);
                    prompt_text = text.trim().to_string();
                }
            }
            _ => {}
        }
    }
    f
}

type Cache = HashMap<(PathBuf, u64), (FileStamp, TranscriptFacts)>;

fn cache() -> &'static Mutex<Cache> {
    static CELL: OnceLock<Mutex<Cache>> = OnceLock::new();
    CELL.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Hard ceiling on cached windows; past it the cache is cleared.
const CACHE_CAP: usize = 4096;

/// The facts of the last `window` bytes of `path`, from the cache when the
/// file's `(len, mtime)` still matches `stamp`. `None` when the file could not
/// be read. An unstamped file is read and never cached.
pub fn read_facts(path: &Path, stamp: Option<FileStamp>, window: u64) -> Option<TranscriptFacts> {
    let key = (path.to_path_buf(), window);
    if let Some(stamp) = stamp {
        if let Ok(guard) = cache().lock() {
            if let Some((cached, facts)) = guard.get(&key) {
                if *cached == stamp {
                    return Some(*facts);
                }
            }
        }
    }
    let len = std::fs::metadata(path).ok()?.len();
    let tail = read_tail_lossy(path, window)?;
    let facts = facts_from_tail(&tail, len <= window);
    if let Some(stamp) = stamp {
        if let Ok(mut guard) = cache().lock() {
            if guard.len() >= CACHE_CAP {
                guard.clear();
            }
            guard.insert(key, (stamp, facts));
        }
    }
    Some(facts)
}

/// Every copy of `session_id`'s transcript under `roots`
/// (`<root>/projects/<encoded cwd>/<id>.jsonl`), stamped now. `Err` when a
/// root's `projects` dir exists but could not be listed: a copy could be hiding
/// there, so the set is not known. A root with no `projects` dir holds none.
pub fn find_copies(roots: &[PathBuf], session_id: &str) -> Result<Vec<TranscriptCopy>, String> {
    let file = format!("{}.jsonl", session_id.trim().to_ascii_lowercase());
    let mut copies = Vec::new();
    for root in roots {
        let projects = root.join("projects");
        let dirs = match std::fs::read_dir(&projects) {
            Ok(dirs) => dirs,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(format!("{} could not be listed: {e}", projects.display())),
        };
        for dir in dirs {
            let dir = dir.map_err(|e| format!("{} listing broke off: {e}", projects.display()))?;
            let path = dir.path().join(&file);
            if let Ok(meta) = std::fs::metadata(&path) {
                if meta.is_file() {
                    copies.push(TranscriptCopy {
                        config_dir: root.clone(),
                        path,
                        stamp: Some(FileStamp::of(&meta)),
                    });
                }
            }
        }
    }
    Ok(copies)
}

/// The copy holding the NEWEST turn, by the ordering `--resume` relies on.
pub fn newest_copy(mut copies: Vec<TranscriptCopy>) -> Option<TranscriptCopy> {
    let mut budget = ReadBudget::Unbounded;
    order_by_newest_turn(&mut copies, &mut |c| {
        last_turn_ms(&c.path, c.stamp, &mut budget)
    });
    copies.into_iter().next()
}

/// The facts of `session_id`'s newest transcript copy under `roots`, from a
/// window deep enough to reach `stamp_ms` when the file allows it. `Err` names
/// why no facts could be read: no copy at all, a root that could not be
/// listed, or a copy that could not be read.
pub fn facts_for_session_in(
    roots: &[PathBuf],
    session_id: &str,
    stamp_ms: Option<i64>,
    band_ms: i64,
) -> Result<TranscriptFacts, String> {
    let copies = find_copies(roots, session_id)?;
    let count = copies.len();
    let Some(copy) = newest_copy(copies) else {
        return Err(format!(
            "no transcript for session {session_id} under any of {} account root(s)",
            roots.len()
        ));
    };
    let unreadable = || {
        format!(
            "the newest of {count} transcript cop(ies), {}, could not be read",
            copy.path.display()
        )
    };
    let facts = read_facts(&copy.path, copy.stamp, TAIL_WIDE).ok_or_else(unreadable)?;
    match stamp_ms {
        Some(stamp) if !facts.reaches(stamp, band_ms) => {
            read_facts(&copy.path, copy.stamp, TAIL_DEEP).ok_or_else(unreadable)
        }
        _ => Ok(facts),
    }
}

/// [`facts_for_session_in`] over every Claude config root this machine has.
pub fn facts_for_session(
    session_id: &str,
    stamp_ms: Option<i64>,
    band_ms: i64,
) -> Result<TranscriptFacts, String> {
    facts_for_session_in(
        &crate::terminal::transcript::find_claude_config_dirs(),
        session_id,
        stamp_ms,
        band_ms,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    const T0: &str = "2026-11-15T10:00:00.000Z";

    fn ms(rfc: &str) -> i64 {
        chrono::DateTime::parse_from_rfc3339(rfc)
            .unwrap()
            .timestamp_millis()
    }

    /// `T0` plus `sec` seconds, as an RFC 3339 string.
    fn at(sec: i64) -> String {
        (chrono::DateTime::parse_from_rfc3339(T0).unwrap() + chrono::Duration::seconds(sec))
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
    }

    fn t(sec: i64) -> i64 {
        ms(&at(sec))
    }

    /// A `user` record (`rec` in the script's selftest), with optional flags.
    fn user(sec: i64, content: serde_json::Value, flags: &[&str]) -> String {
        let mut e = serde_json::json!({
            "type": "user", "timestamp": at(sec), "message": {"role": "user", "content": content},
        });
        for flag in flags {
            e[*flag] = serde_json::json!(true);
        }
        e.to_string()
    }

    fn said(sec: i64, stop: &str) -> String {
        serde_json::json!({
            "type": "assistant", "timestamp": at(sec),
            "message": {"role": "assistant", "stop_reason": stop,
                        "content": [{"type": "text", "text": "ok"}]},
        })
        .to_string()
    }

    fn tool_call(sec: i64) -> String {
        serde_json::json!({
            "type": "assistant", "timestamp": at(sec),
            "message": {"role": "assistant", "stop_reason": "tool_use",
                        "content": [{"type": "tool_use", "id": "u1", "name": "Bash", "input": {}}]},
        })
        .to_string()
    }

    fn tool_result(sec: i64) -> String {
        user(
            sec,
            serde_json::json!([{"type": "tool_result", "tool_use_id": "u1", "content": "x"}]),
            &[],
        )
    }

    fn facts(lines: &[String]) -> TranscriptFacts {
        facts_from_tail(&lines.join("\n"), true)
    }

    const WAKE: &str = "<task-notification>scheduled wake-up</task-notification>";
    const SCHEDULED_WAKE: &str = "Scheduled wake-up: re-run the deploy check.";

    /// The script's `turn_cases`, translated: which record opened the newest
    /// turn, and which is the newest prompt.
    #[test]
    fn turn_openers_and_prompts_match_the_scripts_turn_cases() {
        // A wake, a peer message (meta), then a turn that injects a skill body
        // mid-turn: the peer message opened the newest turn.
        let f = facts(&[
            user(0, serde_json::json!("fix the bug"), &[]),
            said(5, "end_turn"),
            user(60, serde_json::json!(WAKE), &[]),
            said(65, "end_turn"),
            user(
                120,
                serde_json::json!("<system-reminder>The user named this session x</system-reminder>"),
                &["isMeta"],
            ),
            user(
                300,
                serde_json::json!("Another Claude session sent a message: hi"),
                &["isMeta"],
            ),
            tool_call(301),
            tool_result(302),
            user(303, serde_json::json!("Base directory for this skill: /x"), &["isMeta"]),
            said(304, "end_turn"),
        ]);
        assert_eq!(f.last_turn_open_ms, Some(t(300)));
        assert_eq!(f.last_prompt_ms, Some(t(0)));
        assert_eq!(f.last_asst_ms, Some(t(304)));

        // Nothing after the prompt opens a turn: a meta reminder, a caveat, a
        // /model answered locally, a tool result, and a compaction summary.
        let f = facts(&[
            user(0, serde_json::json!("fix the bug"), &[]),
            tool_call(10),
            tool_result(11),
            said(12, "end_turn"),
            user(
                100,
                serde_json::json!("<system-reminder>The user named this session x</system-reminder>"),
                &["isMeta"],
            ),
            user(
                125,
                serde_json::json!("Caveat: The messages below were generated by the user while running local commands."),
                &[],
            ),
            user(130, serde_json::json!("<command-name>/model</command-name>"), &[]),
            user(131, serde_json::json!("<local-command-stdout>ok"), &[]),
            user(
                200,
                serde_json::json!("Summary of the earlier conversation"),
                &["isCompactSummary"],
            ),
        ]);
        assert_eq!(f.last_turn_open_ms, Some(t(0)));
        assert_eq!(f.last_prompt_ms, Some(t(0)));

        // An interrupted turn ends there, so the wake after it opens one.
        let f = facts(&[
            user(0, serde_json::json!("fix the bug"), &[]),
            tool_call(5),
            user(6, serde_json::json!("[Request interrupted by user for tool use]"), &[]),
            user(50, serde_json::json!(WAKE), &[]),
        ]);
        assert_eq!(f.last_turn_open_ms, Some(t(50)));
        assert_eq!(f.last_prompt_ms, Some(t(0)), "a wake is never a prompt");

        // coord's Session Bus delivery is a NON-meta system reminder, and it
        // opens a turn; it is not a prompt.
        let f = facts(&[
            user(0, serde_json::json!("fix the bug"), &[]),
            said(5, "end_turn"),
            user(
                20,
                serde_json::json!("<system-reminder>The user named this session x</system-reminder>"),
                &["isMeta"],
            ),
            user(
                60,
                serde_json::json!("<system-reminder>Session Bus ci_rework message (blocking priority). Message: check #3 went red</system-reminder>"),
                &[],
            ),
            said(65, "end_turn"),
        ]);
        assert_eq!(f.last_turn_open_ms, Some(t(60)));
        assert_eq!(f.last_prompt_ms, Some(t(0)));

        // A session's own scheduled wake is a META record: it opens a turn,
        // and is never a prompt.
        let f = facts(&[
            user(0, serde_json::json!("fix the bug"), &[]),
            said(5, "end_turn"),
            user(60, serde_json::json!(SCHEDULED_WAKE), &["isMeta"]),
            said(65, "end_turn"),
        ]);
        assert_eq!(f.last_turn_open_ms, Some(t(60)));
        assert_eq!(f.last_prompt_ms, Some(t(0)));
    }

    /// Phase 0 (finding `2913808f`): a compaction summary written while the
    /// session sat idle — 3284 s after its finish — is neither a prompt nor a
    /// turn opener nor assistant work.
    #[test]
    fn an_idle_compaction_summary_is_not_activity() {
        let f = facts(&[
            user(0, serde_json::json!("finish up"), &[]),
            said(30, "end_turn"),
            user(
                3284,
                serde_json::json!("This session is being continued from a previous conversation."),
                &["isCompactSummary"],
            ),
        ]);
        assert_eq!(f.last_prompt_ms, Some(t(0)));
        assert_eq!(f.last_turn_open_ms, Some(t(0)));
        assert_eq!(f.last_asst_ms, Some(t(30)));
    }

    /// A queued message is input for the session: its `enqueue` record counts
    /// as a turn opener (the conservative direction); other queue records and
    /// bookkeeping lines do not.
    #[test]
    fn a_queue_enqueue_opens_a_turn_and_bookkeeping_does_not() {
        let rec = |kind: &str, op: Option<&str>, sec: i64| {
            let mut e = serde_json::json!({"type": kind, "timestamp": at(sec)});
            if let Some(op) = op {
                e["operation"] = serde_json::json!(op);
            }
            e.to_string()
        };
        let f = facts(&[
            user(0, serde_json::json!("go"), &[]),
            said(5, "end_turn"),
            rec("last-prompt", None, 50),
            rec("queue-operation", Some("dequeue"), 60),
            rec("permission-mode", None, 70),
        ]);
        assert_eq!(f.last_turn_open_ms, Some(t(0)));
        let f = facts(&[
            user(0, serde_json::json!("go"), &[]),
            said(5, "end_turn"),
            rec("queue-operation", Some("enqueue"), 90),
        ]);
        assert_eq!(f.last_turn_open_ms, Some(t(90)));
        assert_eq!(f.last_prompt_ms, Some(t(0)));
    }

    #[test]
    fn a_partial_first_line_is_skipped_and_the_floor_is_the_oldest_time() {
        let tail = format!(
            "{}\n{}\n{}",
            "ial line with no start\"}",
            said(100, "end_turn"),
            user(200, serde_json::json!("again"), &[])
        );
        let f = facts_from_tail(&tail, false);
        assert_eq!(f.window_floor_ms, Some(t(100)));
        assert!(!f.whole_file);
        assert!(f.reaches(t(100) + 1000, 1000));
        assert!(!f.reaches(t(100) + 999, 1000));
        assert!(facts_from_tail("", true).reaches(0, 1000), "a whole file reaches everything");
    }

    fn write(path: &Path, lines: &[String]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut f = std::fs::File::create(path).unwrap();
        for l in lines {
            writeln!(f, "{l}").unwrap();
        }
    }

    /// Phase 0: a migrated session with several copies is read from the copy
    /// holding the NEWEST turn — even when an older copy was written later
    /// (a migration's fresh mtime) — so a revival in the live copy is seen.
    #[test]
    fn the_newest_transcript_copy_is_the_one_read() {
        let tmp = tempfile::tempdir().unwrap();
        let sid = "d30f7c5f-0000-4000-8000-000000000001";
        let live_root = tmp.path().join(".claude-a");
        let stale_root = tmp.path().join(".claude-b");
        // The live copy: finished at 600 s, then prompted again at 4000 s.
        write(
            &live_root.join("projects").join("p1").join(format!("{sid}.jsonl")),
            &[
                user(0, serde_json::json!("work"), &[]),
                said(600, "end_turn"),
                user(4000, serde_json::json!("one more thing"), &[]),
                said(4010, "end_turn"),
            ],
        );
        // The stale copy holds only old turns, ended 34 h before the finish,
        // but is written LAST (a fresh mtime).
        write(
            &stale_root.join("projects").join("p9").join(format!("{sid}.jsonl")),
            &[
                user(-130_000, serde_json::json!("work"), &[]),
                said(-122_400, "end_turn"),
            ],
        );
        let roots = vec![stale_root.clone(), live_root.clone()];
        let copies = find_copies(&roots, sid).unwrap();
        assert_eq!(copies.len(), 2);
        let newest = newest_copy(copies).unwrap();
        assert_eq!(newest.config_dir, live_root);
        let f = facts_for_session_in(&roots, sid, Some(t(600)), 1000).unwrap();
        assert_eq!(f.last_prompt_ms, Some(t(4000)));

        // No copy anywhere is an error, never empty facts.
        assert!(facts_for_session_in(&roots, "no-such-session", Some(0), 1000).is_err());
        // A root with no `projects` dir holds none — not an error.
        let bare = tmp.path().join(".claude-empty");
        std::fs::create_dir_all(&bare).unwrap();
        assert!(find_copies(&[bare], sid).unwrap().is_empty());
    }

    #[test]
    fn an_unchanged_file_is_served_from_the_cache_and_a_changed_one_is_reread() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("c.jsonl");
        write(&p, &[user(0, serde_json::json!("go"), &[])]);
        let stamp = std::fs::metadata(&p).ok().map(|m| FileStamp::of(&m));
        let first = read_facts(&p, stamp, TAIL_WIDE).unwrap();
        assert_eq!(first.last_prompt_ms, Some(t(0)));
        // Same stamp: the cached answer, even if the bytes were swapped under
        // an identical (len, mtime) — which is the cache's stated contract.
        assert_eq!(read_facts(&p, stamp, TAIL_WIDE), Some(first));
        write(
            &p,
            &[
                user(0, serde_json::json!("go"), &[]),
                user(500, serde_json::json!("and again"), &[]),
            ],
        );
        let restamped = std::fs::metadata(&p).ok().map(|m| FileStamp::of(&m));
        assert_eq!(
            read_facts(&p, restamped, TAIL_WIDE).unwrap().last_prompt_ms,
            Some(t(500))
        );
        assert!(read_facts(&tmp.path().join("gone.jsonl"), None, TAIL_WIDE).is_none());
    }
}
