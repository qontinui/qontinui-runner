//! When a Claude Code transcript last had a TURN — the `timestamp` of its last
//! real `user`/`assistant` line — behind one process-wide cache keyed on the
//! file's `(len, mtime)`.
//!
//! Plan `2026-10-19-session-lookup-by-name-shows-claude-account-in-web`,
//! Phase 2. Two consumers share it, so a file is tailed once per change rather
//! than once per consumer:
//!
//! - [`crate::agent_worktree::custody::SessionDirectory`] orders a MIGRATED
//!   session's transcript copies (account migration copies the transcript into
//!   another account root and resumes the same id there) so the copy holding
//!   the newest turn — the one `claude --resume` must use — comes first;
//! - [`super::identity_stamp`] reports that turn's time as `last_acted_at`.
//!
//! ## Why the turn and not the file's mtime
//!
//! A file's mtime records the last WRITE, and not every write is a turn:
//! Claude Code appends many non-turn lines to a transcript — `last-prompt`,
//! `attachment`, `permission-mode`, `queue-operation`, `ai-title` and
//! `custom-title` records, and harness-injected `isMeta` user lines (each seen
//! in the 400 most recently written transcripts on one Linux fleet box,
//! 2026-11-02) — and an account migration COPIES the whole
//! file into another root, giving the copy a fresh mtime while it holds only
//! old turns. Either would make an idle or superseded copy look newest. The
//! mtime is used here only as half of the cache key — a changed key costs one
//! re-read, never a wrong answer. (Ordering two copies that hold the SAME
//! newest turn is a separate question, settled by the newest write in
//! [`crate::agent_worktree::custody::SessionDirectory`].)
//!
//! ## What is read
//!
//! Only the tail ([`TAIL_FIRST`], then once [`TAIL_WIDE`] when one tool result
//! filled the first window), decoded lossily by
//! [`super::claude_activity::read_tail_lossy`] and scanned from the end by
//! [`super::claude_activity::last_message_ms`], which reads `type`, `isMeta`
//! and `timestamp` and stops at the first real message. Nothing from a line is
//! kept beyond the parsed instant.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use super::claude_activity::{last_message_ms, read_tail_lossy};

/// The first tail window read for a transcript.
pub const TAIL_FIRST: u64 = 64 * 1024;

/// The one wider retry, for a transcript whose last line alone is larger than
/// [`TAIL_FIRST`] (a single tool result routinely is).
pub const TAIL_WIDE: u64 = 1024 * 1024;

/// Hard ceiling on cached paths. The cache is pruned to the live transcript set
/// every reporter pass ([`retain_paths`]); this only stops a runaway if nothing
/// ever prunes it.
const CACHE_CAP: usize = 200_000;

/// The cache key half that says whether a cached answer still describes the
/// file: its length and its modification time, as the directory walk saw them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FileStamp {
    pub len: u64,
    pub modified_ms: Option<i64>,
}

impl FileStamp {
    /// The stamp of a directory entry's metadata, `None` when it cannot be
    /// read — an unstamped file is still tailed, just never cached.
    pub fn of(meta: &std::fs::Metadata) -> Self {
        let modified_ms = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .and_then(|d| i64::try_from(d.as_millis()).ok());
        Self {
            len: meta.len(),
            modified_ms,
        }
    }
}

/// What one probe of one transcript established.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnRead {
    /// The file was read. `Some(ms)` is the newest turn's instant (unix ms);
    /// `None` means the tail held no real `user`/`assistant` line at all.
    Read(Option<i64>),
    /// Not read this time: the caller's [`ReadBudget`] was spent. UNKNOWN —
    /// never "no turn".
    Deferred,
    /// The file could not be opened or read (it vanished, a permission blip).
    /// Never cached, so the next probe tries again.
    Unreadable,
}

/// How many uncached tail reads one caller may still spend. A cache hit is
/// free; a miss costs one. `Unbounded` is for callers whose work is already
/// bounded by construction (ordering the copies of migrated sessions only).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadBudget {
    Unbounded,
    Limited(usize),
}

impl ReadBudget {
    fn take(&mut self) -> bool {
        match self {
            Self::Unbounded => true,
            Self::Limited(0) => false,
            Self::Limited(n) => {
                *n -= 1;
                true
            }
        }
    }
}

type Cache = HashMap<PathBuf, (FileStamp, Option<i64>)>;

fn cache() -> &'static Mutex<Cache> {
    static CELL: OnceLock<Mutex<Cache>> = OnceLock::new();
    CELL.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The newest turn in `path`, from the cache when `stamp` still matches, else
/// from a tail read charged to `budget`.
///
/// A `None` stamp (metadata was unreadable) is never cached: there is no key
/// that could say the cached answer still holds.
pub fn last_turn_ms(path: &Path, stamp: Option<FileStamp>, budget: &mut ReadBudget) -> TurnRead {
    if let Some(stamp) = stamp {
        if let Ok(guard) = cache().lock() {
            if let Some((cached, turn)) = guard.get(path) {
                if *cached == stamp {
                    return TurnRead::Read(*turn);
                }
            }
        }
    }
    if !budget.take() {
        return TurnRead::Deferred;
    }
    let Some(turn) = read_last_turn(path) else {
        return TurnRead::Unreadable;
    };
    if let Some(stamp) = stamp {
        if let Ok(mut guard) = cache().lock() {
            if guard.len() >= CACHE_CAP {
                guard.clear();
            }
            guard.insert(path.to_path_buf(), (stamp, turn));
        }
    }
    TurnRead::Read(turn)
}

/// Drop every cached path not in `keep` — called with the transcript set the
/// latest directory walk saw, so deleted transcripts do not accumulate.
pub fn retain_paths(keep: &HashSet<PathBuf>) {
    if let Ok(mut guard) = cache().lock() {
        guard.retain(|p, _| keep.contains(p));
    }
}

/// Read the tail and find the newest turn. `None` only on an I/O failure;
/// `Some(None)` is a readable file with no turn line in reach.
fn read_last_turn(path: &Path) -> Option<Option<i64>> {
    let len = std::fs::metadata(path).ok()?.len();
    let tail = read_tail_lossy(path, TAIL_FIRST)?;
    if let Some(ms) = last_message_ms(&tail) {
        return Some(Some(ms));
    }
    if len <= TAIL_FIRST {
        return Some(None); // the whole file was read
    }
    let wide = read_tail_lossy(path, TAIL_WIDE)?;
    Some(last_message_ms(&wide))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn line(kind: &str, ts: &str) -> String {
        format!(r#"{{"type":"{kind}","timestamp":"{ts}","message":{{"content":"x"}}}}"#)
    }

    fn write(path: &Path, lines: &[String]) {
        let mut f = std::fs::File::create(path).unwrap();
        for l in lines {
            writeln!(f, "{l}").unwrap();
        }
    }

    fn stamp_of(path: &Path) -> Option<FileStamp> {
        std::fs::metadata(path).ok().map(|m| FileStamp::of(&m))
    }

    fn ms(rfc: &str) -> i64 {
        chrono::DateTime::parse_from_rfc3339(rfc)
            .unwrap()
            .timestamp_millis()
    }

    #[test]
    fn the_newest_real_turn_wins_and_meta_and_record_lines_do_not_count() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("a.jsonl");
        write(
            &p,
            &[
                line("user", "2026-10-01T10:00:00.000Z"),
                line("assistant", "2026-10-01T10:05:00.000Z"),
                // A harness-injected meta line is not a turn.
                r#"{"type":"user","isMeta":true,"timestamp":"2026-10-01T11:00:00.000Z"}"#
                    .to_string(),
                // Nor is a bookkeeping record appended after the last turn.
                r#"{"type":"last-prompt","timestamp":"2026-10-01T12:00:00.000Z"}"#.to_string(),
                r#"{"type":"queue-operation","timestamp":"2026-10-01T12:30:00.000Z"}"#.to_string(),
            ],
        );
        let got = last_turn_ms(&p, stamp_of(&p), &mut ReadBudget::Unbounded);
        assert_eq!(got, TurnRead::Read(Some(ms("2026-10-01T10:05:00.000Z"))));
    }

    #[test]
    fn a_file_with_no_turn_line_reads_as_none_not_as_unreadable() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("b.jsonl");
        write(&p, &[r#"{"type":"permission-mode"}"#.to_string()]);
        assert_eq!(
            last_turn_ms(&p, stamp_of(&p), &mut ReadBudget::Unbounded),
            TurnRead::Read(None)
        );
        let missing = tmp.path().join("gone.jsonl");
        assert_eq!(
            last_turn_ms(&missing, None, &mut ReadBudget::Unbounded),
            TurnRead::Unreadable
        );
    }

    #[test]
    fn a_turn_beyond_the_first_window_is_found_by_the_wide_retry() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("c.jsonl");
        // One non-turn record larger than TAIL_FIRST after the last turn.
        let huge = format!(
            r#"{{"type":"attachment","data":"{}"}}"#,
            "y".repeat(usize::try_from(TAIL_FIRST).unwrap() + 1024)
        );
        write(&p, &[line("assistant", "2026-10-02T09:00:00.000Z"), huge]);
        assert_eq!(
            last_turn_ms(&p, stamp_of(&p), &mut ReadBudget::Unbounded),
            TurnRead::Read(Some(ms("2026-10-02T09:00:00.000Z")))
        );
    }

    #[test]
    fn a_cache_hit_costs_no_budget_and_a_changed_stamp_rereads() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("d.jsonl");
        write(&p, &[line("user", "2026-10-03T08:00:00.000Z")]);
        let first = stamp_of(&p);
        assert_eq!(
            last_turn_ms(&p, first, &mut ReadBudget::Limited(1)),
            TurnRead::Read(Some(ms("2026-10-03T08:00:00.000Z")))
        );
        // Same stamp, zero budget: served from the cache.
        assert_eq!(
            last_turn_ms(&p, first, &mut ReadBudget::Limited(0)),
            TurnRead::Read(Some(ms("2026-10-03T08:00:00.000Z")))
        );
        // The file grows: the stamp changes, and with no budget the answer is
        // UNKNOWN rather than the stale cached instant.
        write(
            &p,
            &[
                line("user", "2026-10-03T08:00:00.000Z"),
                line("assistant", "2026-10-03T09:30:00.000Z"),
            ],
        );
        let second = stamp_of(&p);
        assert_ne!(first, second);
        assert_eq!(
            last_turn_ms(&p, second, &mut ReadBudget::Limited(0)),
            TurnRead::Deferred
        );
        assert_eq!(
            last_turn_ms(&p, second, &mut ReadBudget::Limited(1)),
            TurnRead::Read(Some(ms("2026-10-03T09:30:00.000Z")))
        );
    }

    #[test]
    fn an_unstamped_file_is_read_every_time_and_never_cached() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("e.jsonl");
        write(&p, &[line("user", "2026-10-04T08:00:00.000Z")]);
        assert_eq!(
            last_turn_ms(&p, None, &mut ReadBudget::Limited(1)),
            TurnRead::Read(Some(ms("2026-10-04T08:00:00.000Z")))
        );
        // No stamp means no key that could vouch for a cached answer.
        assert_eq!(
            last_turn_ms(&p, None, &mut ReadBudget::Limited(0)),
            TurnRead::Deferred
        );
    }
}
