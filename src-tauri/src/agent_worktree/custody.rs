//! WIP custody — **who owns the uncommitted work in this worktree?**
//!
//! Plan `2026-08-22-wip-custody-rebuild-survivable-attribution`, **Phase 3**.
//! Phases 1–2 shipped in `qontinui-claude-config` (`scripts/wip-custody-record.sh`,
//! a `Stop` hook): every turn, the session writes
//! `$GIT_DIR/qontinui-custody.json` naming itself, and snapshots its dirty
//! tracked content to `refs/wip/<session-id>`. This module is the **consumer**
//! — it reads that record and turns it into an owner the operator can see at
//! the exact surface where the complaint bites (`GET
//! /agent-worktrees/reclaimable`).
//!
//! ## The problem, measured
//!
//! `capture_worktree` (`census.rs`) derived every census field from git and
//! filesystem metadata and read **no ownership file at all**; `SurveyItem`
//! carried 14 fields and **no owner field of any kind**. So the operator saw
//! `"Uncommitted work in this tree (G1) — commit or stash it first."` beside
//! `31.3 GB` with no way to learn whose it was. That is the whole complaint.
//!
//! ## THE ONE RULE THIS FILE IS ORGANISED AROUND
//!
//! **`unattributed` must never render as blank, and a guess must never render
//! as a fact.** Every value below carries the SOURCE it came from and a
//! CONFIDENCE, because the downstream consumer of "this worktree has no owner"
//! is a deletion engine. Concretely:
//!
//! * no source at all ⇒ [`AttributionSource::None`] and the literal label
//!   `unattributed` — matching coord's shipped precedent, `GET
//!   /coord/trees/wip-owners/:device_id`, described there as an *"ownership
//!   join (honest `unattributed`)"*;
//! * no source at all **while the coord ownership index was PARTIAL** ⇒
//!   [`AttributionSource::NonePartialCoordIndex`] and the label
//!   `unattributed-partial-coord-index` — the rows the cursor walk never
//!   reached are a coverage gap, not an established absence of owners;
//! * an id we have but cannot resolve to a human-readable session (**a
//!   ghost**: no name-file AND no transcript in ANY of the five
//!   `C:/claude/.claude-*` account roots — 16 of 111 measured 2026-08-22)
//!   renders `session <id> (unresolvable)`, never attributed and never blank;
//! * the `Session-Id:` commit trailer is admitted only as
//!   [`Confidence::Weak`], because it sits on the last **commit** and 35 of 37
//!   uncommitted-only worktrees have file mtimes newer than that commit
//!   (median +23.3 h, max +1,322 h) — it routinely names a DIFFERENT session
//!   than the one that made the edits.
//!
//! ## Liveness keys on `last_seen`, NEVER on `pid`
//!
//! The record carries `pid`, but it is `$PPID` in **bash's** pid namespace. On
//! Windows that is not a Win32 pid, so no Rust consumer can probe it — a fact
//! found during Phase 1 and called out in the plan. `pid` is a correlation
//! hint; [`Attribution::owner_live`] is derived from `last_seen` alone.
//!
//! ## Resolving `$GIT_DIR` without a subprocess
//!
//! A linked worktree's `.git` is a FILE holding one line `gitdir: <path>`.
//! [`git_dir_for`] reads it. At census scale (1,246 worktrees, and a walk that
//! already takes 33 minutes) that is 1,246 **file reads** instead of 1,246
//! `git` spawns. The admin-dir name is NEVER reconstructed from the worktree
//! path — git disambiguates collisions (`…/worktrees/qontinui-dev-notes1`), so
//! it must be read per worktree.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

/// File name the Phase-1 `Stop` hook writes inside `$GIT_DIR`.
pub const CUSTODY_FILE: &str = "qontinui-custody.json";

/// A custody record older than this, with nothing else contradicting it, is
/// STALE — evidence of a past owner rather than a live claim (plan Risk 3).
/// Two hours is ~15× the longest observed inter-turn gap for an active
/// session and well under the multi-day gaps that characterise abandonment.
pub const CUSTODY_STALE_AFTER_SECS: i64 = 2 * 60 * 60;

// ---------------------------------------------------------------------------
// The on-disk record — the contract shipped by `scripts/wip-custody-record.sh`
// ---------------------------------------------------------------------------

/// `$GIT_DIR/qontinui-custody.json`, exactly as the shipped hook emits it.
///
/// EVERY field is optional to us. This is a file written by a **shell script
/// in another repo**: a key added or removed there must degrade one field, not
/// fail the parse and lose custody for the whole worktree. `record_version`
/// is read but deliberately NOT gated on — an unknown future version still
/// yields whatever fields we recognise.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CustodyRecord {
    #[serde(default)]
    pub record_version: Option<u32>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub session_name: Option<String>,
    /// `$PPID` in bash's namespace. **NOT probeable on Windows** — never use
    /// this for liveness. See the module docs.
    #[serde(default)]
    pub pid: Option<i64>,
    #[serde(default)]
    pub repo: Option<String>,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub worktree_path: Option<String>,
    #[serde(default)]
    pub git_dir: Option<String>,
    #[serde(default)]
    pub work_unit_id: Option<String>,
    #[serde(default)]
    pub plan_slug: Option<String>,
    #[serde(default)]
    pub intent: Option<String>,
    /// `refs/wip/<safe-session-id>` — the Phase-2 snapshot ref.
    #[serde(default)]
    pub wip_ref: Option<String>,
    /// The stash-create commit the ref points at, when a capture landed.
    #[serde(default)]
    pub wip_commit: Option<String>,
    #[serde(default)]
    pub wip_captured_at: Option<String>,
    /// `captured` | `clean` | `unchanged` | `deferred` | `uncaptured` |
    /// `probe_failed` | `stash_create_failed` | `nothing_to_stash` |
    /// `ref_write_failed` | `git_unavailable` | `disabled`.
    ///
    /// The hook is deliberate that a FAILED probe is `probe_failed`, never
    /// `clean` — see [`wip_state_is_captured`] and [`wip_state_is_honest_clean`].
    #[serde(default)]
    pub wip_state: Option<String>,
    #[serde(default)]
    pub first_seen: Option<String>,
    #[serde(default)]
    pub last_seen: Option<String>,
    /// Unix seconds of `last_seen` — the liveness signal.
    #[serde(default)]
    pub last_seen_epoch: Option<i64>,
}

/// Does this `wip_state` mean the work is genuinely snapshotted to
/// `refs/wip/<id>`? ONLY `captured` does. Every other value — including the
/// reassuring-sounding `unchanged` — means the snapshot for THIS turn was not
/// taken, and a consumer must not report the work as safe.
pub fn wip_state_is_captured(state: Option<&str>) -> bool {
    state == Some("captured")
}

/// Does this `wip_state` positively assert the tree had nothing to capture?
/// ONLY `clean`. `probe_failed` is NOT clean — the hook names it separately
/// precisely so a failed git probe can never launder into "no work here".
pub fn wip_state_is_honest_clean(state: Option<&str>) -> bool {
    state == Some("clean")
}

// ---------------------------------------------------------------------------
// $GIT_DIR resolution — one file read, no subprocess
// ---------------------------------------------------------------------------

/// Resolve a worktree's `$GIT_DIR`.
///
/// * `.git` is a **directory** ⇒ a primary checkout, and the directory IS what
///   `git rev-parse --git-dir` would print, so the spawn is not taken.
/// * `.git` is a **file** ⇒ a linked worktree; read its single
///   `gitdir: <path>` line. git normally writes an absolute path but a
///   relative one is legal, so an unrooted value is resolved against the
///   worktree.
///
/// `None` when `.git` is absent or the file does not carry a `gitdir:` line.
pub fn git_dir_for(worktree: &Path) -> Option<PathBuf> {
    let dot_git = worktree.join(".git");
    let meta = std::fs::metadata(&dot_git).ok()?;
    if meta.is_dir() {
        return Some(dot_git);
    }
    // Bounded read: this is a one-line pointer file, and an unbounded read of
    // a path an attacker could replace with a FIFO would hang the census.
    let raw = read_bounded(&dot_git, 4096)?;
    parse_gitdir_line(&raw, worktree)
}

/// Pure half of [`git_dir_for`] — parse a `.git` FILE's contents.
pub fn parse_gitdir_line(raw: &str, worktree: &Path) -> Option<PathBuf> {
    let line = raw.lines().next()?.trim_end_matches('\r').trim();
    let rest = line.strip_prefix("gitdir:")?.trim();
    if rest.is_empty() {
        return None;
    }
    let p = PathBuf::from(rest);
    // `is_absolute` is false for `C:/x` on POSIX builds and for `/x` on
    // Windows, so spell both shapes rather than trusting the platform rule.
    let looks_absolute = p.is_absolute()
        || rest.starts_with('/')
        || rest.starts_with('\\')
        || rest
            .as_bytes()
            .get(1)
            .is_some_and(|c| *c == b':' && rest.as_bytes()[0].is_ascii_alphabetic());
    Some(if looks_absolute { p } else { worktree.join(p) })
}

/// Read at most `max` bytes of a small file. Returns `None` on any IO error
/// AND when the file is larger than `max` — a custody record that has grown
/// past a few KB is not a custody record.
fn read_bounded(path: &Path, max: u64) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > max {
        return None;
    }
    std::fs::read_to_string(path).ok()
}

/// Read the custody record for `worktree`, if one exists and parses.
///
/// Two file reads on the hit path (`.git`, then the record), one on the miss
/// path. No subprocess, ever.
pub fn read_custody(worktree: &Path) -> Option<CustodyRecord> {
    let git_dir = git_dir_for(worktree)?;
    let raw = read_bounded(&git_dir.join(CUSTODY_FILE), CUSTODY_MAX_BYTES)?;
    serde_json::from_str::<CustodyRecord>(&raw).ok()
}

/// Upper bound on one custody record, mirror or slot alike.
const CUSTODY_MAX_BYTES: u64 = 64 * 1024;

/// Directory inside `$GIT_DIR` holding ONE custody record per session
/// (`<safe-session-id>.json`), written by the `Stop` hook since 2026-09-04.
/// [`CUSTODY_FILE`] is only a byte-identical copy of the NEWEST slot.
pub const CUSTODY_SLOT_DIR: &str = "qontinui-custody.d";

/// At most this many slots are reported per worktree. Slots are never pruned
/// by the hook, so an old shared checkout accumulates one per session that
/// ever touched it; the newest are the ones a "who else is here?" consumer
/// needs, and the census body must not grow without bound.
pub const CUSTODY_MAX_SLOTS: usize = 64;

/// At most this many slot files are PARSED per worktree, pre-selected by file
/// mtime (the hook rewrites a slot every turn, so mtime tracks `last_seen`).
/// Keeps a directory holding thousands of dead slots from turning the census
/// walk into thousands of reads per worktree.
const CUSTODY_MAX_SLOT_SCAN: usize = 4 * CUSTODY_MAX_SLOTS;

/// Read EVERY per-session custody slot for `worktree`, newest first.
///
/// ## Why the mirror is not enough
///
/// [`read_custody`] reads `$GIT_DIR/qontinui-custody.json`, which the hook
/// keeps as a copy of whichever slot was written LAST. Two sessions cohabiting
/// a shared checkout therefore collapse on the mirror to whoever took the
/// latest turn — and "who ELSE is here?" is exactly the question shared-
/// checkout occupancy needs answered (plan
/// `2026-09-07-shared-checkout-occupancy-is-invisible-to-coord`, Phase 1c).
/// The slots under [`CUSTODY_SLOT_DIR`] are the authoritative per-session
/// store, so this reads them all.
///
/// ## What it does NOT do
///
/// No liveness filtering: the hook never prunes slots, so a consumer must age
/// each one by its own `last_seen`. No subprocess: `$GIT_DIR` comes from
/// [`git_dir_for`]. Every slot goes through the same bounded read as the
/// mirror; a non-file, an oversize file, or an unparseable one is skipped,
/// never fatal. Only `*.json` is considered, so the hook's in-flight
/// `*.json.tmp.<pid>` files are never read half-written.
///
/// Ordering: `last_seen_epoch` descending (a slot with no epoch sorts after
/// every slot with one), then `last_seen` descending, then `session_id`, so
/// the output is deterministic. Truncated to [`CUSTODY_MAX_SLOTS`].
pub fn read_custody_slots(worktree: &Path) -> Vec<CustodyRecord> {
    let Some(git_dir) = git_dir_for(worktree) else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(git_dir.join(CUSTODY_SLOT_DIR)) else {
        return Vec::new();
    };

    let mut candidates: Vec<(Option<std::time::SystemTime>, PathBuf)> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("json"))
        .map(|p| {
            let mtime = std::fs::metadata(&p).and_then(|m| m.modified()).ok();
            (mtime, p)
        })
        .collect();
    // Newest mtime first; `None` (unreadable metadata) sorts last.
    candidates.sort_by_key(|c| std::cmp::Reverse(c.0));
    candidates.truncate(CUSTODY_MAX_SLOT_SCAN);

    let mut slots: Vec<CustodyRecord> = candidates
        .iter()
        .filter_map(|(_, p)| read_bounded(p, CUSTODY_MAX_BYTES))
        .filter_map(|raw| serde_json::from_str::<CustodyRecord>(&raw).ok())
        .collect();
    slots.sort_by(|a, b| {
        b.last_seen_epoch
            .cmp(&a.last_seen_epoch)
            .then_with(|| b.last_seen.cmp(&a.last_seen))
            .then_with(|| a.session_id.cmp(&b.session_id))
    });
    slots.truncate(CUSTODY_MAX_SLOTS);
    slots
}

// ---------------------------------------------------------------------------
// The `Session-Id:` / `Session-Name:` commit trailers
// ---------------------------------------------------------------------------

/// Extract `Session-Id:` / `Session-Name:` trailers from a commit message body.
///
/// The LAST occurrence wins (a trailer block is appended, so a re-worded
/// message can carry both an old and a new one). Values are trimmed and an
/// empty value is dropped — "the trailer is present but blank" is not an id.
pub fn parse_session_trailers(body: &str) -> (Option<String>, Option<String>) {
    let mut id = None;
    let mut name = None;
    for line in body.lines() {
        let line = line.trim();
        if let Some(v) = strip_trailer(line, "Session-Id:") {
            id = Some(v);
        } else if let Some(v) = strip_trailer(line, "Session-Name:") {
            name = Some(v);
        }
    }
    (id, name)
}

#[expect(
    clippy::string_slice,
    reason = "legacy str byte slice — migrate to str::get / char_indices / str_utils::truncate_str; plan 2026-09-14-runner-str-byte-slice-class-has-no-lint-gate"
)]
fn strip_trailer(line: &str, key: &str) -> Option<String> {
    // BYTE comparison, not `line[..key.len()]`.
    //
    // Slicing by byte length panics the moment byte `key.len()` is not a UTF-8
    // char boundary — and it is not, for any commit line starting with CJK or
    // an emoji (`"日本語のコミット"`, `"🤖 rebuild"`). This runs inside
    // `capture_worktree` for EVERY worktree's HEAD message on a 33-minute walk
    // with no `catch_unwind` above it, so one such commit anywhere on the box
    // would abort the whole census and leave the survey serving a stale
    // snapshot forever.
    //
    // A matching ASCII prefix guarantees byte `key.len()` IS a boundary, so
    // the slice on the next line is safe only because of this check.
    let (k, l) = (key.as_bytes(), line.as_bytes());
    if l.len() < k.len() || !l[..k.len()].eq_ignore_ascii_case(k) {
        return None;
    }
    let v = line[key.len()..].trim();
    (!v.is_empty()).then(|| v.to_string())
}

// ---------------------------------------------------------------------------
// Session directory — name + ghost resolution across ALL account roots
// ---------------------------------------------------------------------------

/// What we could establish about one session id.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionIdentity {
    /// The operator-facing name, from `~/.qontinui/session-names/<id>` (the
    /// same source the `Session-Name:` commit trailer uses — 720 entries on
    /// this box today, nothing prunes it).
    pub name: Option<String>,
    /// A transcript for this id survives in at least one account root.
    pub transcript: bool,
    /// WHICH account root holds it — the `CLAUDE_CONFIG_DIR` a resume line
    /// must name. Resolution sweeps all five roots, so this is not decorative:
    /// resuming under the wrong root finds no transcript. When the id has a
    /// copy in several roots (an account migration copies the transcript and
    /// resumes the same id elsewhere), this is the root whose copy holds the
    /// NEWEST turn — the one `--resume` must use (see
    /// [`SessionDirectory::copies_of`]).
    pub config_dir: Option<String>,
}

/// One on-disk copy of a session's transcript:
/// `<config_dir>/projects/<encoded cwd>/<session id>.jsonl`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptCopy {
    /// The account root as discovered — NOT case-normalized, so its basename
    /// is the account label and the path can be handed to a shell as-is.
    pub config_dir: PathBuf,
    /// The transcript file itself.
    pub path: PathBuf,
    /// `(len, mtime)` as the walk saw them — the key the newest-turn cache
    /// validates against. `None` when the metadata could not be read.
    pub stamp: Option<crate::session::transcript_last_turn::FileStamp>,
}

/// Order a session's transcript copies so the one holding the NEWEST turn is
/// first: a readable turn beats a readable file with no turn, which beats a
/// copy that could not be read (or was not read).
///
/// The turn time decides first, never the mtime alone: a migration copy gets
/// a FRESH mtime while holding only the original's turns, so once the resumed
/// session speaks, its copy wins on the turn itself.
///
/// Ties on the turn — two copies holding the same newest turn — break on the
/// most recently WRITTEN copy. That is what a migration looks like when the
/// resumed session exits before its first turn: the migration wrote the copy
/// into the target root after the original's last write, and the target root
/// is the account `--resume` continued under. Ordering such a tie by name
/// instead would re-stamp a migrated session back onto its OLD account
/// (coord accepts an equal observation time). Only then the config dir, then
/// the path, so a tie that mtime cannot settle never flaps between walks.
pub(crate) fn order_by_newest_turn(
    copies: &mut [TranscriptCopy],
    read: &mut dyn FnMut(&TranscriptCopy) -> crate::session::transcript_last_turn::TurnRead,
) {
    use crate::session::transcript_last_turn::TurnRead;
    if copies.len() < 2 {
        return;
    }
    let rank = |r: TurnRead| match r {
        TurnRead::Read(Some(ms)) => (2u8, ms),
        TurnRead::Read(None) => (1, 0),
        TurnRead::Deferred | TurnRead::Unreadable => (0, 0),
    };
    let mut keyed: Vec<((u8, i64), TranscriptCopy)> =
        copies.iter().map(|c| (rank(read(c)), c.clone())).collect();
    let written = |c: &TranscriptCopy| c.stamp.and_then(|s| s.modified_ms);
    keyed.sort_by(|(ra, a), (rb, b)| {
        rb.cmp(ra)
            // Newest write first; an unknown mtime (`None`) sorts last.
            .then_with(|| written(b).cmp(&written(a)))
            .then_with(|| a.config_dir.cmp(&b.config_dir))
            .then_with(|| a.path.cmp(&b.path))
    });
    for (slot, (_, c)) in copies.iter_mut().zip(keyed) {
        *slot = c;
    }
}

impl SessionIdentity {
    /// A **ghost**: the id is stamped somewhere, but no name-file and no
    /// transcript survives in ANY root. 16 of 111 owning sessions measured
    /// 2026-08-22. Must render `session <id> (unresolvable)`.
    pub fn is_ghost(&self) -> bool {
        self.name.is_none() && !self.transcript
    }
}

/// A one-shot index of every session id this machine can still resolve.
///
/// ## Why it is an INDEX and not a per-lookup probe
///
/// Resolution must sweep **all five** `C:/claude/.claude-*` account roots plus
/// `~/.qontinui/session-names/`: only 9 of 111 sessions resolve inside
/// `.claude-gmail` alone, so a single-root lookup reports a 92% failure rate
/// that is an artifact of where it looked. But the survey resolves ~1,250
/// worktrees, and a per-worktree sweep of ~3,400 transcripts across ~5 roots
/// is quadratic. So the roots are walked ONCE per survey and every lookup is a
/// hash probe.
///
/// The transcript half is keyed on the id ALONE, deliberately — a session's
/// transcript lives under its *working directory*'s encoded project dir, which
/// is not necessarily the worktree we are attributing. Asking "does a
/// transcript for this id exist anywhere" is the question that decides ghost
/// vs resolvable; asking "does one exist under this path" would manufacture
/// ghosts out of sessions that simply moved.
///
/// ## Every root is kept, not the first
///
/// An account migration copies a session's transcript into another account
/// root and resumes the same id there, so one id can have a copy in several
/// roots. Keeping only the first root the walk met made the answer depend on
/// discovery order; every copy is kept instead and the copy holding the
/// NEWEST turn is put first ([`order_by_newest_turn`]). Only multi-root ids
/// pay a tail read for that, and the read is cached per `(len, mtime)`
/// ([`crate::session::transcript_last_turn`]).
#[derive(Debug, Clone, Default)]
pub struct SessionDirectory {
    names: HashMap<String, String>,
    /// id → every copy of its transcript, the newest-turn copy first.
    transcripts: HashMap<String, Vec<TranscriptCopy>>,
    /// Roots actually walked — reported so an empty index is never mistaken
    /// for "no sessions exist".
    roots_scanned: usize,
}

impl SessionDirectory {
    /// Walk `~/.qontinui/session-names/` and every discovered Claude config
    /// root. Never fails: an unreadable root contributes nothing and is
    /// counted out of `roots_scanned`, so [`Self::roots_scanned`] stays an
    /// honest statement of coverage.
    pub fn discover() -> Self {
        Self::discover_from(
            &session_names_dir(),
            &crate::terminal::transcript::find_claude_config_dirs(),
        )
    }

    /// [`Self::discover`] over an explicit names dir and root list — the
    /// filesystem-shaped test seam.
    pub(crate) fn discover_from(names_dir: &Path, roots: &[PathBuf]) -> Self {
        use crate::session::transcript_last_turn::{last_turn_ms, ReadBudget};
        let mut me = Self::default();
        me.load_names(names_dir);
        for root in roots {
            if me.load_transcripts(root) {
                me.roots_scanned += 1;
            }
        }
        // Unbounded: only ids with a copy in more than one root are read, and
        // each read is cached until that file changes.
        let mut budget = ReadBudget::Unbounded;
        for copies in me.transcripts.values_mut() {
            order_by_newest_turn(copies, &mut |c| last_turn_ms(&c.path, c.stamp, &mut budget));
        }
        me
    }

    /// Empty index — used on the cold path and in tests. Reports
    /// `roots_scanned == 0`, which callers read as "resolution was not
    /// attempted", never as "nothing resolves".
    pub fn empty() -> Self {
        Self::default()
    }

    /// [`Self::discover`] behind a short TTL.
    ///
    /// The walk touches ~3,400 transcripts across five account roots. The
    /// survey route is POLLED by the panel, so re-walking per request would
    /// put a multi-second directory scan on a hot path to answer a question
    /// whose answer changes on the timescale of a session starting — minutes,
    /// not milliseconds. 60 s is short enough that a session opened during
    /// triage resolves on the next poll and long enough that the scan is not
    /// the cost of the route.
    ///
    /// A poisoned lock degrades to a FRESH walk, never to
    /// [`Self::empty`]: an empty index would make every session look like a
    /// ghost, which is precisely the false report this module exists to
    /// prevent.
    pub fn cached() -> std::sync::Arc<Self> {
        use std::sync::{Arc, Mutex};
        use std::time::{Duration, Instant};
        const TTL: Duration = Duration::from_secs(60);
        static CACHE: OnceLock<Mutex<Option<(Instant, Arc<SessionDirectory>)>>> = OnceLock::new();

        let cell = CACHE.get_or_init(|| Mutex::new(None));
        let Ok(mut guard) = cell.lock() else {
            return Arc::new(Self::discover());
        };
        if let Some((at, dir)) = guard.as_ref() {
            if at.elapsed() < TTL {
                return Arc::clone(dir);
            }
        }
        let fresh = Arc::new(Self::discover());
        *guard = Some((Instant::now(), Arc::clone(&fresh)));
        fresh
    }

    pub fn roots_scanned(&self) -> usize {
        self.roots_scanned
    }

    pub fn names_indexed(&self) -> usize {
        self.names.len()
    }

    pub fn transcripts_indexed(&self) -> usize {
        self.transcripts.len()
    }

    /// The fleet tag name for `session_id` (`~/.qontinui/session-names/<id>`),
    /// if one is recorded.
    pub fn name_of(&self, session_id: &str) -> Option<&str> {
        self.names
            .get(&session_id.trim().to_ascii_lowercase())
            .map(String::as_str)
    }

    /// Every transcript copy of `session_id`, the copy holding the newest turn
    /// first. Empty when no root holds one.
    pub fn copies_of(&self, session_id: &str) -> &[TranscriptCopy] {
        self.transcripts
            .get(&session_id.trim().to_ascii_lowercase())
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// Every id that has at least one transcript copy, sorted — the ids whose
    /// account root this machine can name.
    pub fn transcribed_ids(&self) -> Vec<&str> {
        let mut ids: Vec<&str> = self.transcripts.keys().map(String::as_str).collect();
        ids.sort_unstable();
        ids
    }

    /// Every transcript file the walk saw, across all roots.
    pub fn transcript_paths(&self) -> std::collections::HashSet<PathBuf> {
        self.transcripts
            .values()
            .flatten()
            .map(|c| c.path.clone())
            .collect()
    }

    fn load_names(&mut self, dir: &Path) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let path = e.path();
            if !path.is_file() {
                continue;
            }
            let Some(id) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let Some(raw) = read_bounded(&path, 4096) else {
                continue;
            };
            let name = raw.trim();
            if !name.is_empty() {
                self.names.insert(id.to_ascii_lowercase(), name.to_string());
            }
        }
    }

    /// Index `<root>/projects/*/<session-id>.jsonl`, keeping EVERY copy (a
    /// second root's copy of an id is a migration, not a duplicate to drop).
    /// Returns whether the root was readable at all.
    fn load_transcripts(&mut self, root: &Path) -> bool {
        let projects = root.join("projects");
        let Ok(dirs) = std::fs::read_dir(&projects) else {
            return false;
        };
        for d in dirs.flatten() {
            let Ok(files) = std::fs::read_dir(d.path()) else {
                continue;
            };
            for f in files.flatten() {
                let p = f.path();
                if p.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                    continue;
                }
                let Some(stem) = p.file_stem().and_then(|s| s.to_str()) else {
                    continue;
                };
                let stamp = f
                    .metadata()
                    .ok()
                    .map(|m| crate::session::transcript_last_turn::FileStamp::of(&m));
                let copies = self
                    .transcripts
                    .entry(stem.to_ascii_lowercase())
                    .or_default();
                if copies.iter().all(|c| c.path != p) {
                    copies.push(TranscriptCopy {
                        config_dir: root.to_path_buf(),
                        path: p,
                        stamp,
                    });
                }
            }
        }
        true
    }

    /// Resolve one id. An id we have never seen yields
    /// `SessionIdentity::default()`, which [`SessionIdentity::is_ghost`] reads
    /// as a ghost — correct only because `discover()` swept every root; an
    /// [`Self::empty`] index must not be used to declare ghosts, which is what
    /// `roots_scanned` exists to let callers check.
    pub fn resolve(&self, session_id: &str) -> SessionIdentity {
        let key = session_id.trim().to_ascii_lowercase();
        let config_dir = self
            .transcripts
            .get(&key)
            .and_then(|copies| copies.first())
            .map(|c| norm(&c.config_dir.to_string_lossy()));
        SessionIdentity {
            name: self.names.get(&key).cloned(),
            transcript: config_dir.is_some(),
            config_dir,
        }
    }

    /// Test seam.
    #[cfg(test)]
    pub fn from_parts(names: &[(&str, &str)], transcripts: &[&str], roots_scanned: usize) -> Self {
        let root = PathBuf::from("C:/claude/.claude-test");
        Self {
            names: names
                .iter()
                .map(|(k, v)| (k.to_ascii_lowercase(), v.to_string()))
                .collect(),
            transcripts: transcripts
                .iter()
                .map(|t| {
                    let id = t.to_ascii_lowercase();
                    let copy = TranscriptCopy {
                        config_dir: root.clone(),
                        path: root.join("projects").join("p").join(format!("{id}.jsonl")),
                        stamp: None,
                    };
                    (id, vec![copy])
                })
                .collect(),
            roots_scanned,
        }
    }
}

/// Record `name` as the commit `Session-Name` for `session_id` by writing
/// `<session-names dir>/<session_id>` (what `prepare-commit-msg` and
/// [`SessionDirectory::discover`] read). `claude --name` writes neither, and the
/// runner was a reader only until now. Refuses an id that is not a plain
/// filename component. Returns the path written.
pub(crate) fn write_session_name(session_id: &str, name: &str) -> std::io::Result<PathBuf> {
    let id = session_id.trim();
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "session id is not a plain filename component",
        ));
    }
    let dir = session_names_dir();
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(id.to_ascii_lowercase());
    std::fs::write(&path, format!("{}\n", name.trim()))?;
    Ok(path)
}

fn session_names_dir() -> PathBuf {
    if let Ok(over) = std::env::var("QONTINUI_SESSION_NAMES_DIR") {
        if !over.trim().is_empty() {
            return PathBuf::from(over);
        }
    }
    qontinui_runner_lib::ambient::qontinui_dir_or_cwd().join("session-names")
}

// ---------------------------------------------------------------------------
// The resolution order — three sources, plus one explicitly-weak fallback
// ---------------------------------------------------------------------------

/// Where an attribution came from. Serialized as the snake_case token so the
/// operator (and any consumer) can always see WHICH source spoke.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AttributionSource {
    /// **Primary.** `$GIT_DIR/qontinui-custody.json`, written per turn by the
    /// session itself (Phase 1). Worktree-path-keyed, so it covers every
    /// creation path — hand-rolled `git worktree add`, `_wt-*`,
    /// `.agent-worktrees/` and coord-allocated worktrees identically. This is
    /// the only source that reaches the 168 dirty worktrees that were never
    /// allocated through coord.
    CustodyRecord,
    /// `coord.agent_worktrees` — written ONLY at allocation time, with no
    /// post-allocation repair door. Meaningful only now that Phase 0 landed:
    /// before it, `owner_live` was derived from a wrong-table join and read
    /// `Some(false)` for every allocated worktree.
    CoordAllocation,
    /// `coord.repo_branches.author_agent_session_id` — a durable
    /// *(repo, branch) → session* binding written stickily by coord's
    /// `claims.rs:1252`. Branch-keyed, never worktree-path-keyed. Consulted
    /// BEFORE declaring `unattributed`.
    CoordBranchAuthor,
    /// The `Session-Id:` commit trailer on HEAD. **Weak by construction** —
    /// it names whoever last COMMITTED, and the edits are usually newer (35 of
    /// 37 uncommitted-only worktrees, median +23.3 h). Admitted last, and
    /// always as [`Confidence::Weak`], so it can raise the surface's coverage
    /// without ever being mistaken for the custody record.
    CommitTrailer,
    /// Nothing spoke. Renders the literal `unattributed`.
    None,
    /// Nothing spoke, **but the coord ownership index was PARTIAL** (the
    /// cursor walk over `GET /coord/sessions/worktrees` stopped before coord's
    /// last page). Sources 2 and 3 were consulted over a PREFIX of the ledger,
    /// so the allocation row may sit on a page that was never read — "nobody
    /// owns this" is not established. Renders
    /// [`UNATTRIBUTED_PARTIAL_COORD_INDEX_LABEL`], never the confident
    /// `unattributed`. Never produced by [`resolve_attribution`] (which is
    /// index-agnostic); the survey applies it via
    /// [`Attribution::unattributed_in_partial_coord_index`].
    NonePartialCoordIndex,
}

/// The row label for [`AttributionSource::NonePartialCoordIndex`]. Spelled
/// the same as the wip-orphan report's `orphan_reason` arm for the same
/// condition, so the two surfaces cannot disagree about what they mean.
pub const UNATTRIBUTED_PARTIAL_COORD_INDEX_LABEL: &str = "unattributed-partial-coord-index";

impl AttributionSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CustodyRecord => "custody_record",
            Self::CoordAllocation => "coord_allocation",
            Self::CoordBranchAuthor => "coord_branch_author",
            Self::CommitTrailer => "commit_trailer",
            Self::None => "none",
            Self::NonePartialCoordIndex => "none_partial_coord_index",
        }
    }
}

/// How much weight the surface puts on the attribution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    /// A per-turn custody record whose `last_seen` is recent, or a coord
    /// allocation whose owner coord reports live.
    Strong,
    /// A custody record whose `last_seen` is stale, or a coord binding with no
    /// liveness statement — evidence of a PAST owner, never a live claim.
    Evidential,
    /// The commit trailer. Names the last committer, which is frequently not
    /// the session that made the edits.
    Weak,
    /// No attribution.
    NoneAtAll,
}

impl Confidence {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Strong => "strong",
            Self::Evidential => "evidential",
            Self::Weak => "weak",
            Self::NoneAtAll => "none",
        }
    }
}

/// What coord contributed for one worktree, if anything. Populated from
/// `GET /coord/sessions/worktrees` (source 2) and, once coord projects it,
/// the branch-author binding (source 3).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CoordOwner {
    /// `coord.agent_worktrees.agent_session_id`, path-keyed.
    pub allocation_session_id: Option<String>,
    /// coord's `owner_session_state` for that session, projected to a
    /// tri-state. `None` = UNKNOWABLE — never fabricated as `false`; this is
    /// the invariant Phase 0 restored.
    pub allocation_owner_live: Option<bool>,
    /// `coord.repo_branches.author_agent_session_id`, branch-keyed.
    pub branch_author_session_id: Option<String>,
    /// `coord.agent_worktrees.work_unit_id`, path-keyed — the plan this
    /// worktree was allocated FOR. Belongs to the SAME row as
    /// [`Self::allocation_session_id`], which is why the fill-in in
    /// [`resolve_attribution`] may only import it when that id agrees with the
    /// custody record's.
    pub allocation_work_unit_id: Option<String>,
}

/// Everything the resolver needs for ONE worktree. Pure input — no disk, no
/// clock, no network — so [`resolve_attribution`] is exhaustively testable.
#[derive(Debug, Clone, Default)]
pub struct AttributionInput<'a> {
    pub custody: Option<&'a CustodyRecord>,
    pub coord: Option<&'a CoordOwner>,
    /// `Session-Id:` trailer on HEAD.
    pub trailer_session_id: Option<&'a str>,
    /// `Session-Name:` trailer on HEAD — used only when the directory cannot
    /// resolve the trailer's id, and labelled as trailer-sourced.
    pub trailer_session_name: Option<&'a str>,
    /// Unix seconds, for the `last_seen` staleness comparison.
    pub now_epoch: i64,
}

/// The rendered attribution for one worktree. **Every field that could be
/// mistaken for "nobody owns this" is explicit rather than absent.**
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Attribution {
    /// The owning session id, when any source produced one.
    pub session_id: Option<String>,
    /// The resolved human-readable name, when one could be resolved.
    pub session_name: Option<String>,
    /// **The field to render. NEVER empty.** One of:
    /// `"<name>"`, `"session <id>"`, `"session <id> (unresolvable)"`, or the
    /// literal `"unattributed"`.
    pub session_label: String,
    /// Which source spoke.
    pub source: &'static str,
    pub confidence: &'static str,
    /// RFC3339-ish `last_seen` from the custody record.
    pub last_seen: Option<String>,
    /// Seconds since `last_seen`. `None` = the record carried no epoch, which
    /// is UNKNOWN — never rendered as "just seen".
    pub last_seen_age_secs: Option<i64>,
    /// Is the owning session live? `Some(true)` only when a per-turn custody
    /// record is fresh or coord positively says so; `Some(false)` only from an
    /// OBSERVATION (a stale record, or coord's own verdict); `None` =
    /// unknowable. Keyed on `last_seen`, never on `pid`.
    pub owner_live: Option<bool>,
    /// The id was stamped but no name-file and no transcript survives in any
    /// account root.
    pub unresolvable: bool,
    /// The `CLAUDE_CONFIG_DIR` whose `projects/` holds this session's
    /// transcript. `None` for a ghost — and a resume line MUST then be omitted
    /// rather than guessed, because a `--resume` under the wrong root fails
    /// with a message that looks like the session never existed.
    pub config_dir: Option<String>,
    pub work_unit_id: Option<String>,
    pub plan_slug: Option<String>,
    pub intent: Option<String>,
    /// `refs/wip/<id>` — where the Phase-2 snapshot of this tree's dirty
    /// content lives.
    pub wip_ref: Option<String>,
    pub wip_commit: Option<String>,
    /// Verbatim hook state. `captured` is the ONLY value meaning the work is
    /// snapshotted; `probe_failed` is NOT `clean`.
    pub wip_state: Option<String>,
    /// The custody record exists but its `last_seen` is older than
    /// [`CUSTODY_STALE_AFTER_SECS`].
    pub custody_stale: bool,
}

impl Attribution {
    /// The empty answer when the coord ownership index was PARTIAL: same
    /// shape as [`Self::unattributed`], but labelled
    /// [`UNATTRIBUTED_PARTIAL_COORD_INDEX_LABEL`] with source
    /// `none_partial_coord_index`, so no consumer reads a confident
    /// "unattributed" off an index that did not cover the whole ledger.
    pub fn unattributed_in_partial_coord_index() -> Self {
        Self {
            session_label: UNATTRIBUTED_PARTIAL_COORD_INDEX_LABEL.to_string(),
            source: AttributionSource::NonePartialCoordIndex.as_str(),
            ..Self::unattributed()
        }
    }

    /// The honest empty: `unattributed`, never a blank string.
    pub fn unattributed() -> Self {
        Self {
            session_id: None,
            session_name: None,
            session_label: "unattributed".to_string(),
            source: AttributionSource::None.as_str(),
            confidence: Confidence::NoneAtAll.as_str(),
            last_seen: None,
            last_seen_age_secs: None,
            owner_live: None,
            unresolvable: false,
            config_dir: None,
            work_unit_id: None,
            plan_slug: None,
            intent: None,
            wip_ref: None,
            wip_commit: None,
            wip_state: None,
            custody_stale: false,
        }
    }

    /// Did ANY source name an owner? The numerator of the attribution rate.
    pub fn is_attributed(&self) -> bool {
        self.session_id.is_some()
    }
}

/// Render the label for a session id. **This function is why nothing renders
/// blank.**
fn label_for(
    session_id: &str,
    identity: &SessionIdentity,
    fallback_name: Option<&str>,
    resolution_attempted: bool,
) -> String {
    if let Some(name) = identity.name.as_deref().or(fallback_name) {
        let name = name.trim();
        if !name.is_empty() {
            return format!("{name} (session {})", short_id(session_id));
        }
    }
    if identity.transcript {
        // Resolvable, just unnamed — the operator can `--resume` it.
        format!("session {session_id}")
    } else if resolution_attempted {
        // GHOST: id stamped, nothing survives that can resolve it.
        format!("session {session_id} (unresolvable)")
    } else {
        // NOT a ghost — we never looked. `roots_scanned == 0` means no account
        // root was readable, so "unresolvable" here would be a statement about
        // OUR failure dressed up as a fact about the session. The `unresolvable`
        // FIELD is already guarded this way at every call site; the LABEL is
        // what the operator actually reads, so it must be guarded identically.
        format!("session {session_id}")
    }
}

/// First uuid segment, or the whole id when it is shorter/not a uuid.
#[expect(
    clippy::string_slice,
    reason = "legacy str byte slice — migrate to str::get / char_indices / str_utils::truncate_str; plan 2026-09-14-runner-str-byte-slice-class-has-no-lint-gate"
)]
fn short_id(id: &str) -> &str {
    match id.find('-') {
        Some(i) if i >= 6 => &id[..i],
        _ => id,
    }
}

/// **The resolution order, as implemented.**
///
/// 1. custody record (worktree-path-keyed, every creation path);
/// 2. `coord.agent_worktrees` allocation (path-keyed, allocate-time only);
/// 3. `coord.repo_branches.author_agent_session_id` (branch-keyed, durable);
/// 4. the `Session-Id:` HEAD trailer — admitted last and only as
///    [`Confidence::Weak`], because it names the last committer;
/// 5. otherwise [`Attribution::unattributed`].
///
/// Later sources NEVER overwrite an earlier one's id. They do fill in fields
/// the earlier source left blank — today exactly one: a custody record with no
/// `work_unit_id` takes coord's allocation `work_unit_id`, because that is
/// additive information about the SAME owner. Two conditions gate it, and both
/// are load-bearing:
///
/// * the record's own `work_unit_id` must be blank (the record always wins on a
///   field it filled), and
/// * the record's session id must EQUAL coord's `allocation_session_id`.
///   A disagreement means coord's row describes a different session's
///   allocation, and importing its unit would attribute one session's work to
///   another session's plan.
///
/// **The fill-in lives INSIDE source 1, ahead of its early return.** It cannot
/// live in source 2: source 1 returns unconditionally once the record names a
/// session, so control never reaches source 2 for the population that needs
/// filling — a record that is PRESENT but blank. (Before this, the promise in
/// this paragraph was never kept for any worktree.) Sources 2 and 3 also spell
/// `work_unit_id` explicitly rather than inheriting `None` from
/// [`Attribution::unattributed`].
pub fn resolve_attribution(
    input: &AttributionInput<'_>,
    directory: &SessionDirectory,
) -> Attribution {
    // --- source 1: the custody record ---------------------------------------
    if let Some(rec) = input.custody {
        if let Some(id) = rec
            .session_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            let age = rec
                .last_seen_epoch
                .map(|e| input.now_epoch.saturating_sub(e));
            let stale = age.is_some_and(|a| a > CUSTODY_STALE_AFTER_SECS);
            let identity = directory.resolve(id);
            // The one additive fill-in, resolved BEFORE the return below —
            // see this function's docs. `input.coord` is read here rather
            // than in source 2 because source 2 is unreachable from here.
            let work_unit_id = non_empty(rec.work_unit_id.as_deref()).or_else(|| {
                input.coord.and_then(|c| {
                    // Ids must AGREE. A blank coord id is not agreement.
                    let coord_id = non_empty(c.allocation_session_id.as_deref())?;
                    if coord_id == id {
                        non_empty(c.allocation_work_unit_id.as_deref())
                    } else {
                        None
                    }
                })
            });
            return Attribution {
                session_id: Some(id.to_string()),
                session_name: identity.name.clone().or_else(|| {
                    rec.session_name
                        .as_deref()
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string)
                }),
                session_label: label_for(
                    id,
                    &identity,
                    rec.session_name.as_deref(),
                    directory.roots_scanned() > 0,
                ),
                source: AttributionSource::CustodyRecord.as_str(),
                confidence: if stale {
                    Confidence::Evidential.as_str()
                } else {
                    Confidence::Strong.as_str()
                },
                last_seen: rec.last_seen.clone(),
                last_seen_age_secs: age,
                // `last_seen` IS the liveness signal (never `pid`) — but only
                // in ONE direction. A FRESH record proves the session wrote a
                // turn minutes ago, so `Some(true)` is an observation. SILENCE
                // proves nothing: a session idle awaiting the operator, blocked
                // on a long build, or watching CI is alive and quiet, and this
                // fleet routinely has such sessions.
                //
                // So staleness stays `None` (unknowable), exactly as
                // `coord::owner_live_from_state` refuses to read coord's
                // `stale` / `pending_resolution` as death — a manufactured
                // `Some(false)` is what fed `SessionGone`'s destructive
                // `Remove` authority, the Phase 0 defect. The observation is
                // NOT lost: `custody_stale` and `last_seen_age_secs` carry it
                // losslessly, as evidence rather than as a verdict.
                owner_live: match age {
                    Some(a) if a <= CUSTODY_STALE_AFTER_SECS => Some(true),
                    _ => None,
                },
                unresolvable: identity.is_ghost() && directory.roots_scanned() > 0,
                config_dir: identity.config_dir.clone(),
                work_unit_id,
                plan_slug: non_empty(rec.plan_slug.as_deref()),
                intent: non_empty(rec.intent.as_deref()),
                wip_ref: non_empty(rec.wip_ref.as_deref()),
                wip_commit: non_empty(rec.wip_commit.as_deref()),
                wip_state: non_empty(rec.wip_state.as_deref()),
                custody_stale: stale,
            };
        }
    }

    // --- source 2: coord's allocation ---------------------------------------
    if let Some(c) = input.coord {
        if let Some(id) = non_empty(c.allocation_session_id.as_deref()) {
            let identity = directory.resolve(&id);
            return Attribution {
                session_label: label_for(&id, &identity, None, directory.roots_scanned() > 0),
                session_name: identity.name.clone(),
                source: AttributionSource::CoordAllocation.as_str(),
                confidence: if c.allocation_owner_live == Some(true) {
                    Confidence::Strong.as_str()
                } else {
                    Confidence::Evidential.as_str()
                },
                owner_live: c.allocation_owner_live,
                unresolvable: identity.is_ghost() && directory.roots_scanned() > 0,
                config_dir: identity.config_dir.clone(),
                session_id: Some(id),
                // Carried, not blanked. The id we just resolved IS
                // `allocation_session_id`, so it agrees with the row this unit
                // came from by construction.
                work_unit_id: non_empty(c.allocation_work_unit_id.as_deref()),
                ..Attribution::unattributed()
            };
        }

        // --- source 3: coord's durable branch-author binding ----------------
        if let Some(id) = non_empty(c.branch_author_session_id.as_deref()) {
            let identity = directory.resolve(&id);
            return Attribution {
                session_label: label_for(&id, &identity, None, directory.roots_scanned() > 0),
                session_name: identity.name.clone(),
                source: AttributionSource::CoordBranchAuthor.as_str(),
                // A durable (repo, branch) binding carries no liveness
                // statement at all, so it is evidence of an owner, never a
                // live claim.
                confidence: Confidence::Evidential.as_str(),
                owner_live: None,
                unresolvable: identity.is_ghost() && directory.roots_scanned() > 0,
                config_dir: identity.config_dir.clone(),
                session_id: Some(id),
                // Carried, not blanked — but only when there is no allocation
                // session id to DISAGREE with. Reaching this arm already means
                // `allocation_session_id` was blank, so this is belt-and-braces
                // against a future caller that populates one; and `owner_for`'s
                // (repo, branch) fallback never sets a unit at all.
                work_unit_id: if non_empty(c.allocation_session_id.as_deref()).is_none() {
                    non_empty(c.allocation_work_unit_id.as_deref())
                } else {
                    None
                },
                ..Attribution::unattributed()
            };
        }
    }

    // --- source 4: the HEAD commit trailer (WEAK) ---------------------------
    if let Some(id) = non_empty(input.trailer_session_id) {
        let identity = directory.resolve(&id);
        return Attribution {
            session_label: label_for(
                &id,
                &identity,
                input.trailer_session_name,
                directory.roots_scanned() > 0,
            ),
            session_name: identity
                .name
                .clone()
                .or_else(|| non_empty(input.trailer_session_name)),
            source: AttributionSource::CommitTrailer.as_str(),
            confidence: Confidence::Weak.as_str(),
            owner_live: None,
            unresolvable: identity.is_ghost() && directory.roots_scanned() > 0,
            config_dir: identity.config_dir.clone(),
            session_id: Some(id),
            ..Attribution::unattributed()
        };
    }

    Attribution::unattributed()
}

fn non_empty(s: Option<&str>) -> Option<String> {
    s.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

// ---------------------------------------------------------------------------
// Shell-safe rendering of the ready-to-run lines
// ---------------------------------------------------------------------------

/// Is this token safe to drop into a shell command UNQUOTED?
///
/// Session ids and commit shas come from `$GIT_DIR/qontinui-custody.json` —
/// written by a **shell script in another repo** — and from a `Session-Id:`
/// commit trailer, which any commit author controls. Both then land in a line
/// the operator COPY-PASTES INTO A SHELL. A `"` or a backtick in either would
/// break out of the quoting.
///
/// Every id this fleet issues is a UUID and every sha is hex, so the
/// conservative set costs nothing real; a token outside it makes the caller
/// emit NO command rather than a broken or dangerous one — the same
/// omit-never-guess rule the resume line already follows for an unknown
/// account root.
pub fn is_shell_safe_token(t: &str) -> bool {
    !t.is_empty()
        && t.len() <= 128
        && t.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// Render a path for inclusion inside a double-quoted shell word.
///
/// Returns `None` when the path contains a character that cannot be made safe
/// inside double quotes (`"`, `` ` ``, `$`, `\\`, or a control character) —
/// again: no line beats a broken one.
pub fn shell_quote_path(p: &str) -> Option<String> {
    if p.is_empty()
        || p.bytes()
            .any(|b| matches!(b, b'"' | b'`' | b'$' | b'\\') || b < 0x20)
    {
        return None;
    }
    Some(p.to_string())
}

// ---------------------------------------------------------------------------
// Path + repo joins (the two traps the plan calls out by name)
// ---------------------------------------------------------------------------

/// Normalize a path to the survey's `id` shape: forward slashes, lowercased,
/// no trailing slash. Mirrors `on_demand::norm_path` — kept here so this
/// module has no upward dependency on the survey.
pub fn norm(p: &str) -> String {
    p.replace('\\', "/").trim_end_matches('/').to_lowercase()
}

/// Does coord's ledger path name the same worktree as this absolute census
/// path?
///
/// **The join trap:** `coord.agent_worktrees.worktree_path` is coord's
/// *relative suggested path* for allocate-spawned worktrees, while the census
/// path is absolute. An equality-only join therefore drops every allocated
/// row silently. Matching is: exact after normalization, OR the census path
/// ends with `/<ledger path>`.
pub fn worktree_path_matches(census_path: &str, ledger_path: &str) -> bool {
    norm_path_matches(&norm(census_path), &norm(ledger_path))
}

/// [`worktree_path_matches`] over two ALREADY-normalized paths, and the ONE
/// place the rule lives — the hot path pre-normalizes coord's ledger paths at
/// construction and calls this directly, so a second spelling of the rule
/// would be a second thing to keep in sync.
///
/// The suffix arm requires a `/` boundary: without it `.../wt-abc` would match
/// a ledger path of `bc`.
pub fn norm_path_matches(census_norm: &str, ledger_norm: &str) -> bool {
    if ledger_norm.is_empty() {
        return false;
    }
    census_norm == ledger_norm
        || (census_norm.len() > ledger_norm.len()
            && census_norm.ends_with(ledger_norm)
            && census_norm.as_bytes()[census_norm.len() - ledger_norm.len() - 1] == b'/')
}

/// Does a `repo_branches`-style repo name the same repo as a bare checkout
/// name?
///
/// **The other join trap:** `agent_worktrees.repo` is the bare checkout name
/// while `repo_branches.repo` is `owner/name` (prod: 756/774 full-slug,
/// 18/774 bare). Coord spells BOTH arms —
/// `AND (b.repo = aw.repo OR split_part(b.repo,'/',2) = aw.repo)`
/// (`session_worktrees.rs:589`/`:598`) — and so must this, or the join drops
/// rows silently.
pub fn repo_matches(bare_or_slug: &str, bare: &str) -> bool {
    let a = bare_or_slug.trim().to_ascii_lowercase();
    let b = bare.trim().to_ascii_lowercase();
    if b.is_empty() {
        return false;
    }
    a == b || a.rsplit('/').next() == Some(b.as_str())
}

// ---------------------------------------------------------------------------
// Source 2 / 3 — coord's ownership doors
// ---------------------------------------------------------------------------

/// `GET /coord/sessions/worktrees` — the coord read that carries sources 2 and
/// 3, fetched ONCE per survey and joined path-wise onto the census.
///
/// ## Why this route and not `/coord/trees/wip-owners/:device_id`
///
/// `wip-owners` is the *rendering* precedent this module copies (its
/// "ownership join (honest `unattributed`)"), but it cannot be the source: its
/// key is `(device_id, repo, file)` over `coord.primary_trees.dirty_files`
/// — **there is no worktree path in the key at all**, so it structurally
/// cannot express a worktree. `/coord/sessions/worktrees` is the one door that
/// joins `coord.sessions ⋈ agent_worktrees ⋈ repo_branches`, and it is
/// `FleetPrincipal`-gated, so the runner's device JWT reads it.
///
/// ## What it does NOT carry today
///
/// `coord.repo_branches.author_agent_session_id` — attribution source 3 — is
/// written stickily by coord's `claims.rs:1252` and read by
/// `commit_lineage.rs:157` and `data/repo_branches.rs:3274`, but **no coord
/// HTTP route projects it**. [`CoordWorktreeRow::author_agent_session_id`] is
/// therefore declared `#[serde(default)]` and reads `None` against today's
/// coord. The runner-side consumption is complete; source 3 contributes
/// nothing until a coord door emits the column. That is stated here rather
/// than papered over, because a source that silently contributes zero looks
/// exactly like a source that found nothing.
pub mod coord {
    use super::{norm_path_matches, repo_matches, CoordOwner};
    use serde::Deserialize;
    use std::time::Duration;

    #[derive(Debug, Clone, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct CoordWorktreeRow {
        #[serde(default)]
        pub worktree_path: String,
        #[serde(default)]
        pub repo: String,
        #[serde(default)]
        pub branch: Option<String>,
        /// Forward-compatible: source 3. Absent on today's coord — see the
        /// module docs.
        #[serde(default)]
        pub author_agent_session_id: Option<String>,
        /// `coord.agent_worktrees.work_unit_id` — the plan this worktree was
        /// allocated FOR, as recorded at `POST /agents/allocate` time.
        ///
        /// Forward-compatible in exactly the same sense as
        /// [`Self::author_agent_session_id`]: the coord half that projects
        /// `workUnitId` onto this route ships as a SEPARATE PR, and the two may
        /// land in either order. `#[serde(default)]` is what makes an
        /// older coord (which omits the key entirely) deserialize to `None`
        /// instead of failing the whole survey.
        #[serde(default)]
        pub work_unit_id: Option<String>,
        /// `coord.agent_worktrees.status` — `allocated` | `active` | `merging`
        /// | `merged` | `abandoned`. `None` from a coord that omits it.
        #[serde(default)]
        pub ledger_status: Option<String>,
    }

    #[derive(Debug, Clone, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct CoordSessionRow {
        pub session_id: String,
        /// The Claude Code session id this row is filed under — the KEY of
        /// the keyed read, by which a keyed answer is mapped back to the id it
        /// was asked about. `None` from a coord that omits it.
        #[serde(default)]
        pub claude_code_session_id: Option<String>,
        /// `expected` | `active` | `pending_resolution` | `stale` | `closed`.
        #[serde(default)]
        pub owner_session_state: Option<String>,
        #[serde(default)]
        pub worktrees: Vec<CoordWorktreeRow>,
    }

    /// ONE PAGE of `GET /coord/sessions/worktrees`.
    ///
    /// Plan `2026-09-12-fleet-principal-session-routes-disagree-about-being-capped`
    /// Phase 1: the route is keyset-paged on the ledger primary key
    /// `(agent_id, repo)`. A page is capped at `limit` **worktree ledger rows**
    /// (not sessions), so a session may appear on MORE THAN ONE page and the
    /// client merges by `sessionId` — see [`merge_pages`]. `count` and
    /// `reapStatusCounts` are per-page and are deliberately not read here.
    #[derive(Debug, Clone, Default, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct CoordSessionWorktrees {
        #[serde(default)]
        pub sessions: Vec<CoordSessionRow>,
        /// The page size coord applied, in ledger rows. Absent on a coord that
        /// predates the cursor.
        #[serde(default)]
        pub limit: Option<u32>,
        /// Opaque cursor for the next page, with key PRESENCE kept:
        /// `Some(Some(c))` = continue from `c`; `Some(None)` = `nextCursor: null`,
        /// the walk is complete; `None` = the key is ABSENT, i.e. a coord that
        /// predates the cursor. That coord's one page is final (there is no
        /// way to ask it for more), but it is a possibly-CAPPED final page —
        /// [`walk_pages`] declares it partial when it came back full.
        #[serde(default, deserialize_with = "present_key")]
        pub next_cursor: Option<Option<String>>,
        /// Whether `coord.sessions.claude_code_session_id` — the bridge the
        /// ledger join runs through — exists on that database. Absent or
        /// `false` makes `sessions: []` UNKNOWN rather than "no worktrees";
        /// `None` from a coord that predates the field.
        #[serde(default)]
        pub session_bridge_column_present: Option<bool>,
    }

    /// Deserialize a present key (including `null`) as `Some(_)`, so an
    /// ABSENT key — `#[serde(default)]` — stays distinguishable as `None`.
    fn present_key<'de, D>(d: D) -> Result<Option<Option<String>>, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Option::<String>::deserialize(d).map(Some)
    }

    /// Page size requested per walk step. A pre-cursor coord honours
    /// `?limit=` too, so this is also the cap an old coord's single page is
    /// compared against.
    ///
    /// 100, not coord's 500-row clamp ceiling: a 500-row page measured 16-23 s
    /// on a large tenant (coord finding bc41a3df-4657-43be-a971-06b6b4813f27),
    /// past [`REQUEST_TIMEOUT`], and a page error fails the whole walk — so
    /// the survey got no index at all. A smaller page is meant to answer
    /// inside the deadline (unmeasured at 100 rows), so the walk returns at
    /// least a PARTIAL index when [`SURVEY_WALK_BUDGET`] runs out.
    pub const PAGE_LIMIT: u32 = 100;

    /// Hard bound on the walk: 100 pages x 100 rows = 10,000 ledger rows, ~7x
    /// the largest tenant measured (~1,360 live worktrees). Hitting it is
    /// never silent — the index comes back with [`CoordOwnership::partial`]
    /// set.
    pub const MAX_PAGES: usize = 100;

    /// Wall-clock budget for the operator-facing survey's whole walk. The
    /// per-request timeout ([`REQUEST_TIMEOUT`]) bounds ONE page; this bounds the sum,
    /// so a slow ledger yields a partial index rather than an N x 15 s hang.
    pub const SURVEY_WALK_BUDGET: Duration = Duration::from_secs(20);

    fn page_rows(page: &CoordSessionWorktrees) -> usize {
        page.sessions.iter().map(|s| s.worktrees.len()).sum()
    }

    /// Merge pages into one response, grouping by `sessionId` in first-seen
    /// order and concatenating each session's `worktrees` across pages.
    ///
    /// A session's worktrees are not contiguous under the route's
    /// primary-key order, so the same `sessionId` can arrive on several
    /// pages; keeping the first page's row and dropping the rest would lose
    /// exactly the worktrees past the page boundary.
    pub fn merge_pages(pages: Vec<CoordSessionWorktrees>) -> CoordSessionWorktrees {
        let mut order: Vec<CoordSessionRow> = Vec::new();
        let mut at: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        let mut limit = None;
        // The bridge column is reported per page: the merge carries `Some(true)`
        // only when EVERY page reported it present, `Some(false)` when any page
        // reported it absent, and `None` otherwise (a coord predating the field).
        let mut bridge_all_true = true;
        let mut bridge_any_false = false;
        for page in pages {
            limit = limit.or(page.limit);
            match page.session_bridge_column_present {
                Some(true) => {}
                Some(false) => {
                    bridge_any_false = true;
                    bridge_all_true = false;
                }
                None => bridge_all_true = false,
            }
            for s in page.sessions {
                match at.get(&s.session_id) {
                    Some(&i) => {
                        let row = &mut order[i];
                        if row.owner_session_state.is_none() {
                            row.owner_session_state = s.owner_session_state;
                        }
                        row.worktrees.extend(s.worktrees);
                    }
                    None => {
                        at.insert(s.session_id.clone(), order.len());
                        order.push(s);
                    }
                }
            }
        }
        CoordSessionWorktrees {
            sessions: order,
            limit,
            next_cursor: Some(None),
            session_bridge_column_present: if bridge_any_false {
                Some(false)
            } else if bridge_all_true {
                Some(true)
            } else {
                None
            },
        }
    }

    /// Walk the cursor from page one until `nextCursor` is null/absent, until
    /// `max_pages` pages have been read, or until `budget` has elapsed.
    ///
    /// Returns the merged response and, when the walk did NOT establish that
    /// it read everything, `Some(reason)`: the merged index is then a PREFIX
    /// and a worktree absent from it is UNKNOWN, not unowned. The arms that
    /// set it: the page bound, the time budget (once at least one page is
    /// in), a cursor coord already handed out (a cycle), and a pre-cursor
    /// coord's page that came back full.
    ///
    /// `fetch_page` receives `None` for page one and the previous page's
    /// cursor afterwards. A page ERROR fails the whole walk, and so does the
    /// budget expiring before page one — neither leaves anything to report.
    pub async fn walk_pages<F, Fut>(
        max_pages: usize,
        budget: Duration,
        mut fetch_page: F,
    ) -> Result<(CoordSessionWorktrees, Option<String>), String>
    where
        F: FnMut(Option<String>) -> Fut,
        Fut: std::future::Future<Output = Result<CoordSessionWorktrees, String>>,
    {
        const WHY: &str = "coord's session-worktree index was not fully read";
        let deadline = tokio::time::Instant::now() + budget;
        let mut pages = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let mut cursor: Option<String> = None;
        let mut partial = None;
        loop {
            if pages.len() >= max_pages {
                partial = Some(format!(
                    "{WHY}: stopped at the {max_pages}-page bound with a further page still offered"
                ));
                break;
            }
            let mut page = match tokio::time::timeout_at(deadline, fetch_page(cursor.clone())).await
            {
                Ok(r) => r?,
                Err(_) if pages.is_empty() => {
                    return Err(format!(
                        "coord's session-worktree index did not answer within {}s",
                        budget.as_secs_f32()
                    ));
                }
                Err(_) => {
                    partial = Some(format!(
                        "{WHY}: the {}s walk budget ran out after {} page(s)",
                        budget.as_secs_f32(),
                        pages.len()
                    ));
                    break;
                }
            };
            let next = page.next_cursor.take();
            let rows = page_rows(&page);
            pages.push(page);
            match next {
                // Key absent: a pre-cursor coord. Its page is final, but a
                // FULL one may be capped — say so rather than call it whole.
                None => {
                    if rows >= PAGE_LIMIT as usize {
                        partial = Some(format!(
                            "{WHY}: coord predates the cursor and returned {rows} rows, at the \
                             {PAGE_LIMIT}-row page cap, with no way to ask for more"
                        ));
                    }
                    break;
                }
                Some(None) => break,
                Some(Some(n)) if n.trim().is_empty() => break,
                Some(Some(n)) => {
                    // A cursor already handed out would re-read pages it has
                    // already served — a cycle; stop rather than spin to the
                    // bound duplicating rows.
                    if !seen.insert(n.clone()) {
                        partial = Some(format!(
                            "{WHY}: coord repeated a cursor after {} page(s)",
                            pages.len()
                        ));
                        break;
                    }
                    cursor = Some(n);
                }
            }
        }
        Ok((merge_pages(pages), partial))
    }

    /// Project coord's five-valued session state onto the tri-state
    /// `owner_live`.
    ///
    /// **`stale` and `pending_resolution` map to `None`, deliberately.** coord
    /// reports all five verbatim precisely because "owner probably gone but
    /// unconfirmed" is not a verdict, and Phase 0 of this very plan exists
    /// because a manufactured `Some(false)` on that population fed
    /// `SessionGone`, which carries destructive `Remove` authority. `None`
    /// (unknowable) is never treated as gone.
    pub fn owner_live_from_state(state: Option<&str>) -> Option<bool> {
        match state {
            Some("active") => Some(true),
            Some("closed") => Some(false),
            _ => None,
        }
    }

    /// The path-keyed ownership index the survey joins against.
    #[derive(Debug, Clone, Default)]
    pub struct CoordOwnership {
        /// `(session_id, owner_session_state, row, PRE-NORMALIZED ledger path)`.
        ///
        /// The path is normalized ONCE at construction rather than inside the
        /// lookup: the lookup runs per worktree, so normalizing there is
        /// `O(worktrees x rows)` string allocations — on this fleet's numbers
        /// (1,499 worktrees) that is millions of throwaway `String`s on a path
        /// the operator waits on.
        rows: Vec<(String, Option<String>, CoordWorktreeRow, String)>,
        /// Normalized ledger path → index into [`Self::rows`]. The exact-match
        /// arm is the overwhelmingly common one, and a linear scan there is
        /// `O(worktrees x coord_rows)` — ~2.4M comparisons at fleet scale. The
        /// suffix arm cannot be hashed, so it keeps the scan.
        by_path: std::collections::HashMap<String, usize>,
        /// coord answered at all. `false` ⇒ absence is UNKNOWN, not "no owner".
        pub reachable: bool,
        /// `Some(reason)` ⇒ the cursor walk stopped before coord's last page,
        /// so this index is a PREFIX of the ledger: a worktree that
        /// [`Self::owner_for`] does not find is UNKNOWN, never "unowned".
        /// `None` ⇒ the walk reached `nextCursor: null` (or an old coord's one
        /// and only page).
        pub partial: Option<String>,
        /// Claude session id (lowercased) → the `coord.sessions.id` coord's
        /// KEYED read answered it with (its own `sessions[]` entry, whatever
        /// that session's state). The ONLY way the custody gate resolves a
        /// Claude session to its ledger rows. Empty for an index built by
        /// [`Self::from_response`] alone.
        coord_row_by_claude: std::collections::HashMap<String, String>,
        /// Claude session id (lowercased) → why the keyed read did NOT answer
        /// it (`unknown`, `invalid`, cut by `inputTruncated`, …). Such a
        /// session's worktrees are UNKNOWN, never "none".
        unanswered_by_claude: std::collections::HashMap<String, String>,
    }

    impl CoordOwnership {
        pub fn from_response(resp: CoordSessionWorktrees) -> Self {
            let mut rows = Vec::new();
            for s in resp.sessions {
                for w in s.worktrees {
                    let norm_path = super::norm(&w.worktree_path);
                    rows.push((
                        s.session_id.clone(),
                        s.owner_session_state.clone(),
                        w,
                        norm_path,
                    ));
                }
            }
            let by_path = rows
                .iter()
                .enumerate()
                .map(|(i, (_, _, _, norm))| (norm.clone(), i))
                .collect();
            Self {
                rows,
                by_path,
                reachable: true,
                partial: None,
                coord_row_by_claude: std::collections::HashMap::new(),
                unanswered_by_claude: std::collections::HashMap::new(),
            }
        }

        /// Mark this index as a prefix of coord's ledger (see [`Self::partial`]).
        pub fn with_partial(mut self, partial: Option<String>) -> Self {
            self.partial = partial;
            self
        }

        /// Record which `coord.sessions.id` the keyed read answered each
        /// Claude session id with.
        pub fn record_coord_rows(&mut self, rows: impl IntoIterator<Item = (String, String)>) {
            self.coord_row_by_claude.extend(
                rows.into_iter()
                    .map(|(claude, coord)| (claude.to_ascii_lowercase(), coord)),
            );
        }

        /// Record why the keyed read did not answer each Claude session id.
        pub fn record_unanswered(&mut self, ids: impl IntoIterator<Item = (String, String)>) {
            self.unanswered_by_claude.extend(
                ids.into_iter()
                    .map(|(claude, why)| (claude.to_ascii_lowercase(), why)),
            );
        }

        /// The `coord.sessions.id` the keyed read answered
        /// `claude_session_id` with, if it did.
        pub fn coord_session_for_claude(&self, claude_session_id: &str) -> Option<&str> {
            self.coord_row_by_claude
                .get(&claude_session_id.to_ascii_lowercase())
                .map(String::as_str)
        }

        /// Why the keyed read did not answer `claude_session_id`, if it said.
        pub fn unanswered_reason(&self, claude_session_id: &str) -> Option<&str> {
            self.unanswered_by_claude
                .get(&claude_session_id.to_ascii_lowercase())
                .map(String::as_str)
        }

        pub fn len(&self) -> usize {
            self.rows.len()
        }

        pub fn is_empty(&self) -> bool {
            self.rows.is_empty()
        }

        /// Every ledger worktree row allocated to `session_id` — a
        /// `coord.sessions.id`, which is what this route keys its sessions on
        /// (NOT a Claude session id). Paths are verbatim, so a RELATIVE ledger
        /// path is the caller's to resolve against the workspace root. One row
        /// per path, the first in ledger order.
        pub fn worktrees_for_session(&self, session_id: &str) -> Vec<&CoordWorktreeRow> {
            let mut out: Vec<&CoordWorktreeRow> = Vec::new();
            for (sid, _, row, _) in &self.rows {
                if sid.eq_ignore_ascii_case(session_id)
                    && !row.worktree_path.is_empty()
                    && !out.iter().any(|r| r.worktree_path == row.worktree_path)
                {
                    out.push(row);
                }
            }
            out
        }

        /// Look one worktree up. `census_path` is absolute; coord's ledger path
        /// may be RELATIVE — see [`worktree_path_matches`]. `repo`/`branch` are
        /// the fallback join for source 3, and spell BOTH repo arms.
        pub fn owner_for(
            &self,
            census_path: &str,
            repo: &str,
            branch: Option<&str>,
        ) -> Option<CoordOwner> {
            let census_norm = super::norm(census_path);
            if let Some(&i) = self.by_path.get(&census_norm) {
                let (session_id, state, w, _) = &self.rows[i];
                return Some(CoordOwner {
                    allocation_session_id: Some(session_id.clone()),
                    allocation_owner_live: owner_live_from_state(state.as_deref()),
                    branch_author_session_id: w.author_agent_session_id.clone(),
                    allocation_work_unit_id: w.work_unit_id.clone(),
                });
            }
            let mut out: Option<CoordOwner> = None;
            for (session_id, state, w, ledger_norm) in &self.rows {
                if norm_path_matches(&census_norm, ledger_norm) {
                    return Some(CoordOwner {
                        allocation_session_id: Some(session_id.clone()),
                        allocation_owner_live: owner_live_from_state(state.as_deref()),
                        branch_author_session_id: w.author_agent_session_id.clone(),
                        allocation_work_unit_id: w.work_unit_id.clone(),
                    });
                }
                // Source 3 fallback: a durable (repo, branch) binding, with
                // BOTH repo arms spelled — `agent_worktrees.repo` is the bare
                // checkout name while `repo_branches.repo` is `owner/name`
                // (prod: 756/774 full-slug, 18/774 bare).
                if out.is_none()
                    && w.author_agent_session_id.is_some()
                    && branch.is_some()
                    && w.branch.as_deref() == branch
                    && repo_matches(&w.repo, repo)
                {
                    // `allocation_work_unit_id` is deliberately LEFT UNSET
                    // here. This arm matched on (repo, branch), not on path,
                    // so `w` is some OTHER worktree's allocation row — its
                    // work unit would attribute a different allocation's plan
                    // to this tree.
                    out = Some(CoordOwner {
                        branch_author_session_id: w.author_agent_session_id.clone(),
                        ..Default::default()
                    });
                }
            }
            out
        }
    }

    /// Fetch coord's ownership index. `Ok(None)` = cleanly not applicable (no
    /// coord base configured); `Err` = a real transport / non-2xx failure.
    /// **Never fatal to the survey** — the caller degrades to sources 1 and 4
    /// and says coord was unreachable rather than reporting `unattributed`.
    ///
    /// Walks the route's keyset cursor ([`walk_pages`]) rather than reading
    /// page one as the whole index: before the walk, every worktree past the
    /// first 200 ledger rows was reported `unattributed`. A walk that stops at
    /// [`MAX_PAGES`] returns an index with [`CoordOwnership::partial`] set.
    ///
    /// `budget` bounds the WHOLE walk (see [`walk_pages`]); the survey passes
    /// [`SURVEY_WALK_BUDGET`], the spawn-path trust gate its own shorter one.
    pub async fn fetch_ownership(budget: Duration) -> Result<Option<CoordOwnership>, String> {
        let Some(base) = qontinui_runner_lib::profiles::connected_coord_base() else {
            return Ok(None);
        };
        let base_url = format!("{}/coord/sessions/worktrees", base.trim_end_matches('/'));
        let Some(client) = crate::coord_http::coord_client() else {
            return Err("the shared coord HTTP client could not be built".to_string());
        };
        let (merged, partial) = walk_pages(MAX_PAGES, budget, |cursor| {
            let url = page_url(&base_url, cursor.as_deref());
            async move { get_json::<CoordSessionWorktrees>(client, &url, REQUEST_TIMEOUT).await }
        })
        .await?;
        Ok(Some(
            CoordOwnership::from_response(merged).with_partial(partial),
        ))
    }

    /// `…/coord/sessions/worktrees?limit=<PAGE_LIMIT>[&cursor=<urlencoded>]`.
    fn page_url(base_url: &str, cursor: Option<&str>) -> String {
        match cursor {
            Some(c) => format!(
                "{base_url}?limit={PAGE_LIMIT}&cursor={}",
                urlencoding::encode(c)
            ),
            None => format!("{base_url}?limit={PAGE_LIMIT}"),
        }
    }

    /// coord's cap on the keyed read's key — `MAX_IDS` (500) on the coord
    /// side (`qontinui-coord` `crates/coord/src/session_worktrees.rs`, via
    /// `session_work_status::parse_ids`). Ids past it are cut and reported as
    /// `inputTruncated`; the runner never sends more than
    /// [`KEYED_CHUNK_IDS`] per request, so it never relies on that report.
    pub const KEYED_MAX_IDS: usize = 500;

    /// Ids per keyed request. Sized by coord's measured cost, not by its
    /// [`KEYED_MAX_IDS`] cap: the keyed read's cost grows with the id count,
    /// measured 4.8 s at 25 ids, 10-13 s at 69 and 24-27 s at 100-200 (coord
    /// finding bc41a3df-4657-43be-a971-06b6b4813f27). A 200-id chunk therefore ran
    /// past the per-request deadline and left every id in it UNKNOWN; 25 keeps
    /// one chunk near 5 s, so a slow coord costs a few chunks rather than the
    /// whole read. (It also keeps the request line near 1 KB.)
    pub const KEYED_CHUNK_IDS: usize = 25;
    const _: () = assert!(KEYED_CHUNK_IDS <= KEYED_MAX_IDS);

    /// Keyed chunks in flight at once. coord's cost per keyed request is
    /// dominated by per-id ledger work, so concurrent chunks spread it across
    /// coord's workers instead of queueing it end to end: 69 ids (3 chunks)
    /// take about one chunk's time rather than three. Small on purpose — this
    /// read runs on every box every tick and must not become a load source.
    pub const KEYED_CONCURRENCY: usize = 3;

    /// Per-request deadline for one KEYED chunk: about six times the 4.8 s a
    /// 25-id chunk measured, so a loaded coord still answers inside it. The
    /// 15 s deadline the 200-id chunk ran under was below the 24.6 s coord
    /// took to answer it.
    pub const KEYED_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

    /// The keyed read's overall budget for `chunks` chunks: one
    /// [`KEYED_REQUEST_TIMEOUT`] per wave of [`KEYED_CONCURRENCY`] chunks, plus
    /// a little slack for scheduling. Sized to the chunk count rather than
    /// fixed, so a large candidate set is not cut short by a budget sized for
    /// a small one; a chunk still running when it expires is UNKNOWN.
    pub fn keyed_budget(chunks: usize) -> Duration {
        let waves = chunks.div_ceil(KEYED_CONCURRENCY).max(1);
        KEYED_REQUEST_TIMEOUT * waves as u32 + Duration::from_secs(5)
    }

    /// Per-request deadline for one survey page on the shared
    /// [`crate::coord_http::coord_client`], which carries no global timeout
    /// of its own. The walk as a whole is bounded by its caller's budget.
    pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

    /// `"<what>: <error>: <source>: …"` — the error with its whole
    /// `source()` chain. reqwest's own `Display` stops at `error sending
    /// request for url (…)`; the cause a reader needs (`operation timed out`,
    /// an OS error) is one or more `source()` hops down.
    fn request_error(what: &str, e: &(dyn std::error::Error + 'static)) -> String {
        format!("{what}: {}", crate::util::error_chain::error_chain(e))
    }

    /// GET `url` with the device bearer and `timeout`, and decode a 2xx body
    /// as `T`. Every failure is `Err` with its full cause chain.
    async fn get_json<T: serde::de::DeserializeOwned>(
        client: &reqwest::Client,
        url: &str,
        timeout: Duration,
    ) -> Result<T, String> {
        send_json(crate::coord_http::coord_get(client, url), url, timeout).await
    }

    /// [`get_json`]'s transport half, over an already-built request — the
    /// seam the tests drive without a device bearer.
    async fn send_json<T: serde::de::DeserializeOwned>(
        req: reqwest::RequestBuilder,
        url: &str,
        timeout: Duration,
    ) -> Result<T, String> {
        let resp = req
            .timeout(timeout)
            .send()
            .await
            .map_err(|e| request_error(&format!("GET {url}"), &e))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            let excerpt: String = body.chars().take(200).collect();
            return Err(format!("coord returned {status} for GET {url}: {excerpt}"));
        }
        resp.json::<T>()
            .await
            .map_err(|e| request_error(&format!("decode GET {url}"), &e))
    }

    /// One `unknown` entry of the keyed read: an id coord resolved to no
    /// `coord.sessions` row. Its `worktrees` (an allocate-only owner can have
    /// some) are deliberately not read — the id is UNKNOWN, whatever it holds.
    #[derive(Debug, Clone, Default, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct CoordKeyedUnknown {
        #[serde(default)]
        pub claude_code_session_id: String,
    }

    /// `GET /coord/sessions/worktrees?claude_code_session_ids=<csv>` — the
    /// KEYED one-snapshot read. Every key is optional here so that a response
    /// MISSING one is detectable rather than a decode error or a default.
    #[derive(Debug, Clone, Default, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct CoordKeyedSessionWorktrees {
        /// One entry per requested id that resolved to a `coord.sessions` row,
        /// WHATEVER its state, including `worktrees: []`.
        #[serde(default)]
        pub sessions: Vec<CoordSessionRow>,
        /// The accepted key, echoed. ABSENT ⇒ coord ignored the key (it
        /// predates the keyed mode and served page one of the tenant walk).
        #[serde(default)]
        pub claude_code_session_ids: Option<Vec<String>>,
        #[serde(default)]
        pub unknown: Vec<CoordKeyedUnknown>,
        #[serde(default)]
        pub invalid: Vec<String>,
        #[serde(default)]
        pub input_truncated: bool,
        #[serde(default)]
        pub session_bridge_column_present: Option<bool>,
    }

    /// `…/coord/sessions/worktrees?claude_code_session_ids=<id>,<id>,…`, each
    /// id URL-encoded.
    pub fn keyed_url(base_url: &str, ids: &[String]) -> String {
        let csv: Vec<String> = ids
            .iter()
            .map(|id| urlencoding::encode(id).into_owned())
            .collect();
        format!("{base_url}?claude_code_session_ids={}", csv.join(","))
    }

    /// The ids to ask about: trimmed, blanks dropped, deduplicated
    /// case-insensitively in first-seen order, then cut into requests of at
    /// most [`KEYED_CHUNK_IDS`].
    pub fn keyed_chunks(claude_session_ids: &[String]) -> Vec<Vec<String>> {
        let mut seen = std::collections::HashSet::new();
        let ids: Vec<String> = claude_session_ids
            .iter()
            .map(|id| id.trim())
            .filter(|id| !id.is_empty() && seen.insert(id.to_ascii_lowercase()))
            .map(str::to_string)
            .collect();
        ids.chunks(KEYED_CHUNK_IDS)
            .map(<[String]>::to_vec)
            .collect()
    }

    /// Why one keyed response cannot be trusted at all, if it cannot: no
    /// `claudeCodeSessionIds` echo (coord ignored the key), or a
    /// `sessionBridgeColumnPresent` that is not `true`.
    fn untrusted_keyed_response(resp: &CoordKeyedSessionWorktrees) -> Option<&'static str> {
        if resp.claude_code_session_ids.is_none() {
            return Some(
                "coord answered the keyed session-worktrees read without the \
                 `claudeCodeSessionIds` echo, so it ignored the key (a coord predating the keyed \
                 mode serves page one of the tenant-wide walk instead): the answer is not about \
                 these sessions",
            );
        }
        if resp.session_bridge_column_present != Some(true) {
            return Some(
                "coord's keyed session-worktrees read does not report the claude_code_session_id \
                 bridge column present, so no session in it can be resolved and an empty answer \
                 is UNKNOWN, not \"no worktrees\"",
            );
        }
        None
    }

    /// PURE: the custody index from the keyed reads — `(ids sent, coord's
    /// answer or why there is none)` per chunk.
    ///
    /// **A chunk fails alone.** A chunk whose request failed (transport,
    /// timeout, non-2xx, decode) or whose response cannot be trusted
    /// ([`untrusted_keyed_response`]) records exactly ITS ids UNKNOWN with
    /// that reason; every other chunk's answers still count. Only when there
    /// were chunks and EVERY one failed is the whole read `Err` (UNKNOWN for
    /// every session, carrying the first failure).
    ///
    /// Within a trusted response, an id is answered ONLY by its own
    /// `sessions[]` entry (`claudeCodeSessionId → sessionId`), whatever that
    /// session's state; an id that is in `invalid`, missing from the echo (cut
    /// by `inputTruncated`), in `unknown`, or without exactly one entry is
    /// recorded UNKNOWN with the reason — never "no worktrees".
    pub fn ownership_from_keyed(
        chunks: Vec<(Vec<String>, Result<CoordKeyedSessionWorktrees, String>)>,
    ) -> Result<CoordOwnership, String> {
        fn key(id: &str) -> String {
            id.trim().to_ascii_lowercase()
        }
        let total = chunks.len();
        let mut failures: Vec<String> = Vec::new();
        let mut sessions: Vec<CoordSessionRow> = Vec::new();
        let mut resolved: Vec<(String, String)> = Vec::new();
        let mut unanswered: Vec<(String, String)> = Vec::new();
        for (sent, answer) in chunks {
            let trusted = answer.and_then(|resp| match untrusted_keyed_response(&resp) {
                Some(why) => Err(why.to_string()),
                None => Ok(resp),
            });
            let resp = match trusted {
                Ok(resp) => resp,
                Err(e) => {
                    let why = format!("the keyed read of its {}-id chunk failed: {e}", sent.len());
                    unanswered.extend(sent.iter().map(|id| (key(id), why.clone())));
                    failures.push(e);
                    continue;
                }
            };
            let echo = resp.claude_code_session_ids.as_deref().unwrap_or_default();
            let echoed: std::collections::HashSet<String> = echo.iter().map(|s| key(s)).collect();
            let invalid: std::collections::HashSet<String> =
                resp.invalid.iter().map(|s| key(s)).collect();
            let unknown: std::collections::HashSet<String> = resp
                .unknown
                .iter()
                .map(|u| key(&u.claude_code_session_id))
                .collect();
            let mut entries: std::collections::HashMap<String, Vec<&CoordSessionRow>> =
                std::collections::HashMap::new();
            for s in &resp.sessions {
                if let Some(c) = s.claude_code_session_id.as_deref() {
                    entries.entry(key(c)).or_default().push(s);
                }
            }
            for id in &sent {
                let k = key(id);
                let why = if invalid.contains(&k) {
                    "coord rejected it as not a session id (`invalid`)"
                } else if !echoed.contains(&k) {
                    if resp.input_truncated {
                        "coord cut it from the request (`inputTruncated`)"
                    } else {
                        "coord did not echo it among the ids it answered for"
                    }
                } else if unknown.contains(&k) {
                    "coord resolved it to no session row (`unknown`), and an allocate-only \
                     owner can still hold worktrees under it"
                } else {
                    match entries.get(&k).map(Vec::as_slice) {
                        Some([one]) if !one.session_id.trim().is_empty() => {
                            resolved.push((k, one.session_id.clone()));
                            sessions.push((*one).clone());
                            continue;
                        }
                        Some([_, _, ..]) => "coord listed it under more than one session",
                        _ => {
                            "coord echoed it but answered it under neither `sessions` nor `unknown`"
                        }
                    }
                };
                unanswered.push((k, why.to_string()));
            }
        }
        if total > 0 && failures.len() == total {
            let first = failures.swap_remove(0);
            return Err(if total == 1 {
                first
            } else {
                format!("all {total} keyed session-worktrees chunks failed; the first: {first}")
            });
        }
        let mut index = CoordOwnership::from_response(CoordSessionWorktrees {
            sessions,
            session_bridge_column_present: Some(true),
            ..Default::default()
        });
        index.record_coord_rows(resolved);
        index.record_unanswered(unanswered);
        Ok(index)
    }

    /// Fetch every chunk through `fetch`, at most `concurrency` at once, all
    /// under ONE overall deadline `budget` from now. A chunk that has not
    /// answered by the deadline is `Err` for that chunk alone; answers are
    /// returned in chunk order, each paired with the ids it was asked about.
    pub async fn fetch_keyed_chunks<F, Fut>(
        chunks: Vec<Vec<String>>,
        concurrency: usize,
        budget: Duration,
        fetch: F,
    ) -> Vec<(Vec<String>, Result<CoordKeyedSessionWorktrees, String>)>
    where
        F: Fn(Vec<String>) -> Fut,
        Fut: std::future::Future<Output = Result<CoordKeyedSessionWorktrees, String>>,
    {
        use futures::stream::StreamExt;
        let deadline = tokio::time::Instant::now() + budget;
        // Unordered, so one slow chunk does not hold a slot past its own
        // answer the way an ordered buffer's head-of-line would; the order is
        // restored afterwards.
        let mut answers: Vec<(
            usize,
            Vec<String>,
            Result<CoordKeyedSessionWorktrees, String>,
        )> = futures::stream::iter(chunks.into_iter().enumerate())
            .map(|(at, chunk)| {
                let fut = fetch(chunk.clone());
                async move {
                    let answer =
                        tokio::time::timeout_at(deadline, fut)
                            .await
                            .unwrap_or_else(|_| {
                                Err(format!(
                                    "it did not answer within the keyed read's {}s overall budget",
                                    budget.as_secs()
                                ))
                            });
                    (at, chunk, answer)
                }
            })
            .buffer_unordered(concurrency.max(1))
            .collect()
            .await;
        answers.sort_by_key(|(at, _, _)| *at);
        answers
            .into_iter()
            .map(|(_, chunk, answer)| (chunk, answer))
            .collect()
    }

    /// The attributed-worktree index for exactly `claude_session_ids`, read
    /// through coord's KEYED door
    /// (`GET /coord/sessions/worktrees?claude_code_session_ids=<csv>`), one
    /// request — one SQL snapshot of the session pick and its rows — per
    /// [`KEYED_CHUNK_IDS`] ids, [`KEYED_CONCURRENCY`] in flight, each under
    /// [`KEYED_REQUEST_TIMEOUT`] and all under [`keyed_budget`].
    ///
    /// For a custody gate a missing row reads as "nothing attributed", i.e.
    /// clean, so this fails closed: `Err` (UNKNOWN at the gate) when no coord
    /// base is configured, when the shared coord client is unavailable, and
    /// when every chunk failed. A chunk that fails on its own — transport,
    /// timeout, non-2xx, decode, or a response [`ownership_from_keyed`]
    /// refuses — leaves exactly its ids UNKNOWN; per id, every answer but the
    /// id's own `sessions[]` entry is recorded UNKNOWN.
    ///
    /// This replaced a tenant-wide walk of four state-filtered pages bracketed
    /// by two work-status reads, which on a large tenant timed out on every
    /// pass (coord answered `state=active` in 17 to >60 s; the keyed read in
    /// 0.35 s — coord finding a7ab41b6-deb8-4977-b6db-fa742e1a6649) and so
    /// refused every finished close as `ownership_unreadable`.
    pub async fn fetch_live_ownership(
        claude_session_ids: &[String],
    ) -> Result<CoordOwnership, String> {
        let Some(base) = qontinui_runner_lib::profiles::connected_coord_base() else {
            return Err(
                "no coord base is configured, so the session's worktree allocations cannot be read"
                    .to_string(),
            );
        };
        // Not unit-tested: `coord_client()` is a process-wide `OnceLock`
        // whose `None` arm (a TLS backend that failed to initialise) a test
        // cannot provoke. It is UNKNOWN like every other arm here.
        let Some(client) = crate::coord_http::coord_client() else {
            return Err(
                "the shared coord HTTP client could not be built, so the session's worktree \
                 allocations cannot be read"
                    .to_string(),
            );
        };
        let base_url = format!("{}/coord/sessions/worktrees", base.trim_end_matches('/'));
        let chunks = keyed_chunks(claude_session_ids);
        let budget = keyed_budget(chunks.len());
        let answers = fetch_keyed_chunks(chunks, KEYED_CONCURRENCY, budget, |chunk| {
            let url = keyed_url(&base_url, &chunk);
            async move { get_json(client, &url, KEYED_REQUEST_TIMEOUT).await }
        })
        .await;
        ownership_from_keyed(answers)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn stale_and_pending_resolution_are_unknowable_never_dead() {
            assert_eq!(owner_live_from_state(Some("active")), Some(true));
            assert_eq!(owner_live_from_state(Some("closed")), Some(false));
            for s in ["stale", "pending_resolution", "expected"] {
                assert_eq!(
                    owner_live_from_state(Some(s)),
                    None,
                    "{s} must be UNKNOWABLE — a manufactured Some(false) here feeds \
                     SessionGone, which carries destructive Remove authority"
                );
            }
            assert_eq!(owner_live_from_state(None), None);
        }

        fn index(json: &str) -> CoordOwnership {
            CoordOwnership::from_response(serde_json::from_str(json).unwrap())
        }

        #[test]
        fn joins_a_relative_ledger_path_onto_an_absolute_census_path() {
            let idx = index(
                r#"{"sessions":[{"sessionId":"s1","ownerSessionState":"active",
                    "worktrees":[{"worktreePath":"agent-worktrees/abc/qontinui-runner",
                    "repo":"qontinui-runner","branch":"feat/x"}]}]}"#,
            );
            let got = idx
                .owner_for(
                    "D:/qontinui-root/agent-worktrees/abc/qontinui-runner",
                    "qontinui-runner",
                    Some("feat/x"),
                )
                .expect("relative ledger path must join");
            assert_eq!(got.allocation_session_id.as_deref(), Some("s1"));
            assert_eq!(got.allocation_owner_live, Some(true));
        }

        #[test]
        fn falls_back_to_the_branch_author_binding_across_both_repo_arms() {
            let idx = index(
                r#"{"sessions":[{"sessionId":"s1","ownerSessionState":"closed",
                    "worktrees":[{"worktreePath":"other/path",
                    "repo":"qontinui/qontinui-runner","branch":"feat/y",
                    "authorAgentSessionId":"author-1"}]}]}"#,
            );
            let got = idx
                .owner_for("D:/elsewhere/wt", "qontinui-runner", Some("feat/y"))
                .expect("full-slug repo must still match the bare checkout name");
            assert_eq!(got.allocation_session_id, None);
            assert_eq!(got.branch_author_session_id.as_deref(), Some("author-1"));
        }

        /// The wire key is `workUnitId` (the struct is `rename_all =
        /// "camelCase"`), it rides the PATH-keyed arms, and a coord that has
        /// not yet deployed the column must still deserialize — the two halves
        /// ship as separate PRs and may land in either order.
        #[test]
        fn the_allocation_work_unit_rides_the_path_keyed_arms_and_defaults_to_none() {
            let idx = index(
                r#"{"sessions":[{"sessionId":"s1","ownerSessionState":"active",
                    "worktrees":[{"worktreePath":"agent-worktrees/abc/qontinui-runner",
                    "repo":"qontinui-runner","branch":"feat/x",
                    "workUnitId":"11111111-2222-3333-4444-555555555555"}]}]}"#,
            );
            // exact-match arm
            let got = idx
                .owner_for(
                    "agent-worktrees/abc/qontinui-runner",
                    "qontinui-runner",
                    None,
                )
                .expect("exact path match");
            assert_eq!(
                got.allocation_work_unit_id.as_deref(),
                Some("11111111-2222-3333-4444-555555555555")
            );
            // suffix-join arm
            let got = idx
                .owner_for(
                    "D:/qontinui-root/agent-worktrees/abc/qontinui-runner",
                    "qontinui-runner",
                    Some("feat/x"),
                )
                .expect("relative ledger path must join");
            assert_eq!(
                got.allocation_work_unit_id.as_deref(),
                Some("11111111-2222-3333-4444-555555555555")
            );

            // A coord with no such column at all: absent key -> None, not a
            // decode failure that would blank the whole survey.
            let old = index(
                r#"{"sessions":[{"sessionId":"s1","ownerSessionState":"active",
                    "worktrees":[{"worktreePath":"wt","repo":"r"}]}]}"#,
            );
            let got = old.owner_for("wt", "r", None).expect("row present");
            assert_eq!(got.allocation_work_unit_id, None);
        }

        /// The (repo, branch) fallback matched a DIFFERENT worktree's row, so
        /// its work unit must not be imported onto this tree.
        #[test]
        fn the_branch_author_fallback_never_carries_another_rows_work_unit() {
            let idx = index(
                r#"{"sessions":[{"sessionId":"s1","ownerSessionState":"closed",
                    "worktrees":[{"worktreePath":"other/path",
                    "repo":"qontinui/qontinui-runner","branch":"feat/y",
                    "authorAgentSessionId":"author-1",
                    "workUnitId":"another-trees-plan"}]}]}"#,
            );
            let got = idx
                .owner_for("D:/elsewhere/wt", "qontinui-runner", Some("feat/y"))
                .expect("branch-author fallback");
            assert_eq!(got.branch_author_session_id.as_deref(), Some("author-1"));
            assert_eq!(
                got.allocation_work_unit_id, None,
                "that unit belongs to `other/path`, not to the tree we asked about"
            );
        }

        #[test]
        fn an_unknown_worktree_yields_none_not_a_fabricated_owner() {
            let idx = index(r#"{"sessions":[]}"#);
            assert!(idx
                .owner_for("D:/x", "qontinui-runner", Some("b"))
                .is_none());
            assert!(idx.reachable, "an EMPTY answer is still an answer");
            assert!(idx.partial.is_none());
        }

        // ---- cursor walk (plan 2026-09-12-fleet-principal-session-routes-…) --

        fn page(json: &str) -> CoordSessionWorktrees {
            serde_json::from_str(json).unwrap()
        }

        /// A fake coord serving `pages` in order, keyed by the cursor it was
        /// handed; records every cursor it saw.
        async fn walk_fake(
            pages: Vec<&str>,
            max_pages: usize,
        ) -> (CoordSessionWorktrees, Option<String>, Vec<Option<String>>) {
            let pages: Vec<CoordSessionWorktrees> = pages.into_iter().map(page).collect();
            let seen = std::sync::Mutex::new(Vec::new());
            let (merged, partial) = walk_pages(max_pages, Duration::from_secs(30), |cursor| {
                seen.lock().unwrap().push(cursor.clone());
                let i = match cursor.as_deref() {
                    None => 0,
                    Some(c) => c.trim_start_matches('p').parse::<usize>().unwrap(),
                };
                let p = pages[i].clone();
                async move { Ok(p) }
            })
            .await
            .unwrap();
            (merged, partial, seen.into_inner().unwrap())
        }

        #[tokio::test]
        async fn a_session_split_across_two_pages_keeps_every_worktree() {
            let (merged, partial, seen) = walk_fake(
                vec![
                    r#"{"sessions":[{"sessionId":"s1","ownerSessionState":"active",
                        "worktrees":[{"worktreePath":"wt/a","repo":"r"}]},
                        {"sessionId":"s2","worktrees":[{"worktreePath":"wt/b","repo":"r"}]}],
                        "count":2,"limit":2,"nextCursor":"p1"}"#,
                    r#"{"sessions":[{"sessionId":"s1","ownerSessionState":"active",
                        "worktrees":[{"worktreePath":"wt/c","repo":"r"}]}],
                        "count":1,"limit":2,"nextCursor":null}"#,
                ],
                MAX_PAGES,
            )
            .await;
            assert_eq!(seen, vec![None, Some("p1".to_string())]);
            assert!(partial.is_none(), "walk reached nextCursor: null");
            assert_eq!(merged.sessions.len(), 2, "s1 is ONE session, not two");
            let s1 = &merged.sessions[0];
            assert_eq!(s1.session_id, "s1");
            let paths: Vec<&str> = s1
                .worktrees
                .iter()
                .map(|w| w.worktree_path.as_str())
                .collect();
            assert_eq!(paths, vec!["wt/a", "wt/c"]);

            let idx = CoordOwnership::from_response(merged);
            assert_eq!(idx.len(), 3);
            let got = idx.owner_for("wt/c", "r", None).expect("page-2 row joins");
            assert_eq!(got.allocation_session_id.as_deref(), Some("s1"));
        }

        #[tokio::test]
        async fn hitting_the_page_bound_says_the_index_is_partial() {
            let (merged, partial, seen) = walk_fake(
                vec![
                    r#"{"sessions":[{"sessionId":"s1","worktrees":[{"worktreePath":"a","repo":"r"}]}],
                        "nextCursor":"p1"}"#,
                    r#"{"sessions":[{"sessionId":"s2","worktrees":[{"worktreePath":"b","repo":"r"}]}],
                        "nextCursor":"p2"}"#,
                    r#"{"sessions":[{"sessionId":"s3","worktrees":[{"worktreePath":"c","repo":"r"}]}],
                        "nextCursor":null}"#,
                ],
                2,
            )
            .await;
            assert_eq!(seen.len(), 2, "the bound caps the requests");
            assert_eq!(merged.sessions.len(), 2);
            let reason = partial.expect("a capped walk must SAY it is capped");
            assert!(reason.contains("2-page bound"), "{reason}");
            let idx = CoordOwnership::from_response(merged).with_partial(Some(reason));
            assert!(idx.partial.is_some());
            assert!(idx.owner_for("c", "r", None).is_none());
        }

        #[tokio::test]
        async fn a_coord_without_next_cursor_is_one_final_page() {
            // Pre-cursor coord: no `limit`, no `nextCursor` key at all.
            let (merged, partial, seen) = walk_fake(
                vec![
                    r#"{"tenantId":"t","sessions":[{"sessionId":"s1",
                        "worktrees":[{"worktreePath":"a","repo":"r"}]}],
                        "reapStatusCounts":{},"count":1}"#,
                ],
                MAX_PAGES,
            )
            .await;
            assert_eq!(seen, vec![None]);
            assert!(partial.is_none());
            assert_eq!(merged.sessions.len(), 1);
            assert_eq!(merged.limit, None);
        }

        #[tokio::test]
        async fn a_repeated_cursor_stops_the_walk_as_partial() {
            let (_, partial, seen) = walk_fake(
                vec![
                    r#"{"sessions":[],"nextCursor":"p1"}"#,
                    r#"{"sessions":[],"nextCursor":"p1"}"#,
                ],
                MAX_PAGES,
            )
            .await;
            assert_eq!(seen.len(), 2);
            assert!(partial
                .expect("must be partial")
                .contains("repeated a cursor"));
        }

        /// A->B->A: the cycle is caught at the first REPEAT, not only when the
        /// cursor fails to advance.
        #[tokio::test]
        async fn a_cursor_cycle_is_caught_not_walked_to_the_bound() {
            let (merged, partial, seen) = walk_fake(
                vec![
                    r#"{"sessions":[{"sessionId":"s0","worktrees":[{"worktreePath":"a","repo":"r"}]}],
                        "nextCursor":"p1"}"#,
                    r#"{"sessions":[{"sessionId":"s1","worktrees":[{"worktreePath":"b","repo":"r"}]}],
                        "nextCursor":"p2"}"#,
                    r#"{"sessions":[{"sessionId":"s2","worktrees":[{"worktreePath":"c","repo":"r"}]}],
                        "nextCursor":"p1"}"#,
                ],
                MAX_PAGES,
            )
            .await;
            assert_eq!(seen.len(), 3);
            assert_eq!(merged.sessions.len(), 3, "no page is merged twice");
            assert!(partial
                .expect("a cycle is partial")
                .contains("repeated a cursor"));
        }

        /// A pre-cursor coord honours `?limit=` but cannot be asked for more.
        /// A FULL page from it may be capped, so it is declared partial.
        #[tokio::test]
        async fn a_full_page_from_a_pre_cursor_coord_is_partial() {
            let wts: Vec<String> = (0..PAGE_LIMIT)
                .map(|i| format!(r#"{{"worktreePath":"wt/{i}","repo":"r"}}"#))
                .collect();
            let full = format!(
                r#"{{"sessions":[{{"sessionId":"s1","worktrees":[{}]}}],"count":1}}"#,
                wts.join(",")
            );
            let (merged, partial, seen) = walk_fake(vec![full.as_str()], MAX_PAGES).await;
            assert_eq!(seen, vec![None]);
            assert_eq!(merged.sessions[0].worktrees.len(), PAGE_LIMIT as usize);
            assert!(partial
                .expect("a full legacy page may be capped")
                .contains("predates"));

            // The SAME full page from a cursor-aware coord saying `null` is whole.
            let whole = full.replace(r#""count":1"#, r#""count":1,"nextCursor":null"#);
            let (_, partial, _) = walk_fake(vec![whole.as_str()], MAX_PAGES).await;
            assert!(
                partial.is_none(),
                "an explicit null ends the walk completely"
            );
        }

        #[tokio::test]
        async fn the_walk_budget_yields_a_partial_index_not_a_hang() {
            let (merged, partial) = walk_pages(
                MAX_PAGES,
                Duration::from_millis(300),
                |cursor| async move {
                    match cursor {
                        None => Ok(page(
                            r#"{"sessions":[{"sessionId":"s1","worktrees":[{"worktreePath":"a","repo":"r"}]}],
                                "nextCursor":"p1"}"#,
                        )),
                        Some(_) => {
                            tokio::time::sleep(Duration::from_secs(30)).await;
                            Ok(page(r#"{"sessions":[],"nextCursor":null}"#))
                        }
                    }
                },
            )
            .await
            .expect("page one arrived, so the walk reports what it has");
            assert_eq!(merged.sessions.len(), 1);
            assert!(partial.expect("budget ran out").contains("walk budget"));

            // No page at all inside the budget: nothing to report -> Err.
            let got = walk_pages(MAX_PAGES, Duration::from_millis(50), |_| async {
                tokio::time::sleep(Duration::from_secs(30)).await;
                Ok(page(r#"{"sessions":[]}"#))
            })
            .await;
            assert!(got.unwrap_err().contains("did not answer"));
        }

        #[tokio::test]
        async fn a_failed_page_fails_the_walk_rather_than_returning_a_prefix() {
            let got = walk_pages(MAX_PAGES, Duration::from_secs(30), |cursor| async move {
                match cursor {
                    None => Ok(page(r#"{"sessions":[],"nextCursor":"p1"}"#)),
                    Some(_) => Err("boom".to_string()),
                }
            })
            .await;
            assert_eq!(got.unwrap_err(), "boom");
        }

        #[test]
        fn the_cursor_is_url_encoded_and_the_limit_is_always_sent() {
            assert_eq!(
                page_url("https://c/coord/sessions/worktrees", None),
                format!("https://c/coord/sessions/worktrees?limit={PAGE_LIMIT}")
            );
            assert_eq!(
                page_url("https://c/coord/sessions/worktrees", Some("a+b/c=")),
                format!(
                    "https://c/coord/sessions/worktrees?limit={PAGE_LIMIT}&cursor=a%2Bb%2Fc%3D"
                )
            );
        }

        /// A walk's pages merge by `sessionId` with every row kept, and the
        /// merge keeps the bridge-column verdict honest: present only when
        /// every page said so.
        #[test]
        fn a_walked_index_keeps_every_row_and_the_bridge_survives_the_merge() {
            let p1 = page(
                r#"{"sessionBridgeColumnPresent":true,"limit":2,"nextCursor":"c1","sessions":[
                    {"sessionId":"a","worktrees":[{"worktreePath":"w1"},{"worktreePath":"w2"}]}]}"#,
            );
            let p2 = page(
                r#"{"sessionBridgeColumnPresent":true,"limit":2,"nextCursor":null,"sessions":[
                    {"sessionId":"a","worktrees":[{"worktreePath":"w3"}]}]}"#,
            );
            let merged = merge_pages(vec![p1.clone(), p2.clone()]);
            assert_eq!(merged.session_bridge_column_present, Some(true));
            let idx = CoordOwnership::from_response(merged);
            assert_eq!(idx.worktrees_for_session("a").len(), 3);

            let mut no_bridge = p2.clone();
            no_bridge.session_bridge_column_present = None;
            let merged = merge_pages(vec![p1.clone(), no_bridge]);
            assert_eq!(merged.session_bridge_column_present, None);

            let mut absent = p2;
            absent.session_bridge_column_present = Some(false);
            assert_eq!(
                merge_pages(vec![p1, absent]).session_bridge_column_present,
                Some(false)
            );
        }

        // ---- the keyed read (plan 2026-11-04-runner-finished-close-…, Phase 1) --

        fn keyed(json: &str) -> CoordKeyedSessionWorktrees {
            serde_json::from_str(json).unwrap()
        }

        fn ids(v: &[&str]) -> Vec<String> {
            v.iter().map(|s| s.to_string()).collect()
        }

        const A: &str = "aaaaaaaa-0000-0000-0000-000000000001";
        const B: &str = "bbbbbbbb-0000-0000-0000-000000000002";

        #[test]
        fn the_keyed_request_names_exactly_the_ids_asked_about() {
            assert_eq!(
                keyed_url("https://c/coord/sessions/worktrees", &ids(&[A, "x y"])),
                format!("https://c/coord/sessions/worktrees?claude_code_session_ids={A},x%20y")
            );
        }

        #[test]
        fn the_key_is_deduplicated_and_chunked_at_coords_cap() {
            let many: Vec<String> = (0..KEYED_CHUNK_IDS * 2 + 1)
                .map(|i| format!("{i:08x}-0000-0000-0000-000000000000"))
                .collect();
            let chunks = keyed_chunks(&many);
            assert_eq!(
                chunks.iter().map(Vec::len).collect::<Vec<_>>(),
                vec![KEYED_CHUNK_IDS, KEYED_CHUNK_IDS, 1]
            );
            assert_eq!(chunks.concat(), many, "every id once, in order");
            assert_eq!(
                keyed_chunks(&ids(&[A, " ", &A.to_ascii_uppercase(), B])),
                vec![ids(&[A, B])],
                "blanks dropped, duplicates (case-insensitive) sent once"
            );
            assert!(keyed_chunks(&[]).is_empty(), "no ids, no request");
        }

        #[test]
        fn a_2xx_without_the_echo_is_a_coord_that_ignored_the_key() {
            // What a coord predating the keyed mode serves: page one of the
            // tenant walk, bridge column and all — but no echo.
            let page_one = keyed(&format!(
                r#"{{"sessionBridgeColumnPresent":true,"nextCursor":null,"sessions":[
                    {{"sessionId":"c-1","claudeCodeSessionId":"{A}","worktrees":[]}}]}}"#
            ));
            let err = ownership_from_keyed(vec![(ids(&[A]), Ok(page_one))]).unwrap_err();
            assert!(err.contains("claudeCodeSessionIds"), "{err}");
        }

        #[test]
        fn a_missing_or_false_bridge_column_is_unknown() {
            for bridge in ["", r#""sessionBridgeColumnPresent":false,"#] {
                let resp = keyed(&format!(
                    r#"{{{bridge}"claudeCodeSessionIds":["{A}"],"sessions":[
                        {{"sessionId":"c-1","claudeCodeSessionId":"{A}","worktrees":[]}}]}}"#
                ));
                let err = ownership_from_keyed(vec![(ids(&[A]), Ok(resp))]).unwrap_err();
                assert!(err.contains("bridge column"), "{bridge}: {err}");
            }
        }

        #[test]
        fn unknown_invalid_truncated_and_unechoed_ids_are_unknown_never_empty() {
            let resp = keyed(&format!(
                r#"{{"sessionBridgeColumnPresent":true,"inputTruncated":true,
                    "claudeCodeSessionIds":["{A}","{B}"],
                    "unknown":[{{"claudeCodeSessionId":"{A}",
                        "worktrees":[{{"worktreePath":"/abs/allocate-only"}}]}}],
                    "invalid":["not-a-uuid"],
                    "sessions":[{{"sessionId":"c-b","claudeCodeSessionId":"{B}","worktrees":[]}}]}}"#
            ));
            let sent = ids(&[A, B, "not-a-uuid", "cccccccc-0000-0000-0000-000000000003"]);
            let idx = ownership_from_keyed(vec![(sent, Ok(resp))]).unwrap();
            assert_eq!(idx.coord_session_for_claude(B), Some("c-b"));
            for (id, why) in [
                (A, "`unknown`"),
                ("not-a-uuid", "`invalid`"),
                ("cccccccc-0000-0000-0000-000000000003", "`inputTruncated`"),
            ] {
                assert_eq!(idx.coord_session_for_claude(id), None, "{id}");
                let reason = idx.unanswered_reason(id).unwrap_or_default();
                assert!(reason.contains(why), "{id}: {reason}");
            }
            assert!(
                idx.worktrees_for_session("c-a").is_empty() && idx.is_empty(),
                "an `unknown` id's allocate-only rows are never attributed"
            );

            // Not echoed, no truncation flag: still unanswered.
            let resp = keyed(
                r#"{"sessionBridgeColumnPresent":true,"claudeCodeSessionIds":[],"sessions":[]}"#,
            );
            let idx = ownership_from_keyed(vec![(ids(&[A]), Ok(resp))]).unwrap();
            assert!(idx.unanswered_reason(A).unwrap().contains("did not echo"));
        }

        #[test]
        fn an_echoed_id_with_no_entry_or_two_entries_is_unknown() {
            let resp = keyed(&format!(
                r#"{{"sessionBridgeColumnPresent":true,"claudeCodeSessionIds":["{A}","{B}"],
                    "sessions":[
                    {{"sessionId":"c-1","claudeCodeSessionId":"{B}","worktrees":[]}},
                    {{"sessionId":"c-2","claudeCodeSessionId":"{B}","worktrees":[]}}]}}"#
            ));
            let idx = ownership_from_keyed(vec![(ids(&[A, B]), Ok(resp))]).unwrap();
            assert!(idx.unanswered_reason(A).unwrap().contains("neither"));
            assert!(idx.unanswered_reason(B).unwrap().contains("more than one"));
            assert_eq!(idx.coord_session_for_claude(B), None);
        }

        /// `worktrees: []` on the id's own entry is a POSITIVE "holds nothing",
        /// whatever the session's state; an entry's rows are attributed to it.
        #[test]
        fn an_entry_with_no_worktrees_is_positively_empty_whatever_its_state() {
            let resp = keyed(&format!(
                r#"{{"sessionBridgeColumnPresent":true,"inputTruncated":false,"invalid":[],
                    "unknown":[],"claudeCodeSessionIds":["{A}","{B}"],"sessions":[
                    {{"sessionId":"c-a","claudeCodeSessionId":"{A}","ownerSessionState":"closed",
                      "worktrees":[]}},
                    {{"sessionId":"c-b","claudeCodeSessionId":"{}","ownerSessionState":"stale",
                      "worktrees":[{{"worktreePath":"agent-worktrees/b/r","repo":"r"}}]}}]}}"#,
                B.to_ascii_uppercase()
            ));
            let idx = ownership_from_keyed(vec![(ids(&[A, B]), Ok(resp))]).unwrap();
            assert_eq!(idx.coord_session_for_claude(A), Some("c-a"));
            assert!(idx.unanswered_reason(A).is_none());
            assert!(idx.worktrees_for_session("c-a").is_empty());
            assert_eq!(idx.coord_session_for_claude(B), Some("c-b"));
            assert_eq!(
                idx.worktrees_for_session("c-b")[0].worktree_path,
                "agent-worktrees/b/r"
            );
        }

        fn good(id: &str, sid: &str) -> Result<CoordKeyedSessionWorktrees, String> {
            Ok(keyed(&format!(
                r#"{{"sessionBridgeColumnPresent":true,"claudeCodeSessionIds":["{id}"],
                    "sessions":[{{"sessionId":"{sid}","claudeCodeSessionId":"{id}","worktrees":[]}}]}}"#
            )))
        }

        const C: &str = "cccccccc-0000-0000-0000-000000000003";

        /// A failed or untrusted chunk leaves exactly ITS ids UNKNOWN; the
        /// other chunks' answers still count.
        #[test]
        fn a_bad_chunk_leaves_only_its_own_ids_unknown() {
            let idx = ownership_from_keyed(vec![
                (ids(&[A]), good(A, "c-a")),
                (ids(&[B]), good(B, "c-b")),
            ])
            .unwrap();
            assert_eq!(idx.coord_session_for_claude(A), Some("c-a"));
            assert_eq!(idx.coord_session_for_claude(B), Some("c-b"));

            let echo_less = Ok(keyed(
                r#"{"sessionBridgeColumnPresent":true,"sessions":[]}"#,
            ));
            let timed_out = Err("GET u: operation timed out".to_string());
            let idx = ownership_from_keyed(vec![
                (ids(&[A]), good(A, "c-a")),
                (ids(&[B]), echo_less),
                (ids(&[C]), timed_out),
            ])
            .unwrap();
            assert_eq!(idx.coord_session_for_claude(A), Some("c-a"));
            assert!(idx.unanswered_reason(A).is_none());
            for (id, why) in [(B, "claudeCodeSessionIds"), (C, "timed out")] {
                assert_eq!(idx.coord_session_for_claude(id), None, "{id}");
                let reason = idx.unanswered_reason(id).unwrap_or_default();
                assert!(
                    reason.contains("1-id chunk failed") && reason.contains(why),
                    "{id}: {reason}"
                );
            }
        }

        /// Every chunk failing is the whole read failing: `Err`, which the
        /// gate reads as `ownership_unreadable` for every session.
        #[test]
        fn every_chunk_failing_fails_the_read() {
            let err = ownership_from_keyed(vec![
                (ids(&[A]), Err("first".to_string())),
                (ids(&[B]), Err("second".to_string())),
            ])
            .unwrap_err();
            assert!(err.contains("all 2") && err.contains("first"), "{err}");
            assert!(
                ownership_from_keyed(vec![]).unwrap().is_empty(),
                "no ids asked about is no failure"
            );
        }

        #[test]
        fn the_keyed_read_is_tuned_to_coords_measured_cost() {
            assert_eq!(KEYED_CHUNK_IDS, 25);
            assert_eq!(KEYED_REQUEST_TIMEOUT, Duration::from_secs(30));
            assert_eq!(KEYED_CONCURRENCY, 3);
            assert_eq!(PAGE_LIMIT, 100);
            // 69 ids (the measured candidate set) is three chunks: one wave.
            let many: Vec<String> = (0..69)
                .map(|i| format!("{i:08x}-0000-0000-0000-000000000000"))
                .collect();
            let sizes: Vec<usize> = keyed_chunks(&many).iter().map(Vec::len).collect();
            assert_eq!(sizes, vec![25, 25, 19]);
            // The budget grows by one request deadline per wave of chunks.
            assert_eq!(keyed_budget(0), Duration::from_secs(35));
            assert_eq!(keyed_budget(3), Duration::from_secs(35));
            assert_eq!(keyed_budget(4), Duration::from_secs(65));
            assert_eq!(keyed_budget(9), Duration::from_secs(95));
            assert!(keyed_budget(3) > KEYED_REQUEST_TIMEOUT);
        }

        /// Chunks run at most `concurrency` at once, under one overall
        /// deadline; a chunk still running at the deadline is `Err` for that
        /// chunk alone, and answers come back in chunk order.
        #[tokio::test(start_paused = true)]
        async fn chunks_run_bounded_and_a_slow_chunk_times_out_alone() {
            use std::sync::atomic::{AtomicUsize, Ordering};
            use std::sync::Arc;
            /// Decrements on drop, so a chunk the deadline cancels leaves.
            struct Leave(Arc<AtomicUsize>);
            impl Drop for Leave {
                fn drop(&mut self) {
                    self.0.fetch_sub(1, Ordering::SeqCst);
                }
            }
            let in_flight = Arc::new(AtomicUsize::new(0));
            let peak = Arc::new(AtomicUsize::new(0));
            let chunks = vec![ids(&[A]), ids(&[B]), ids(&[C]), ids(&["d"]), ids(&["e"])];
            let started = tokio::time::Instant::now();
            let answers = fetch_keyed_chunks(chunks, 2, Duration::from_secs(25), |chunk| {
                let (in_flight, peak) = (in_flight.clone(), peak.clone());
                async move {
                    let now = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(now, Ordering::SeqCst);
                    let _leave = Leave(in_flight);
                    // C never answers in time; the rest take 10 s each.
                    let secs = if chunk[0] == C { 1_000 } else { 10 };
                    tokio::time::sleep(Duration::from_secs(secs)).await;
                    let id = chunk[0].clone();
                    good(&id, &format!("c-{id}"))
                }
            })
            .await;
            assert_eq!(peak.load(Ordering::SeqCst), 2, "bounded concurrency");
            assert!(
                started.elapsed() <= Duration::from_secs(25),
                "one overall deadline"
            );
            let order: Vec<&str> = answers.iter().map(|(c, _)| c[0].as_str()).collect();
            assert_eq!(order, vec![A, B, C, "d", "e"]);
            for (chunk, answer) in &answers {
                match chunk[0].as_str() {
                    C => assert!(
                        answer.as_ref().unwrap_err().contains("25s overall budget"),
                        "{answer:?}"
                    ),
                    // `e` starts at 20 s (when `d`'s slot frees up) and
                    // would finish at 30 s: past the deadline, so it too is
                    // UNKNOWN — the budget, not the request, bounds it.
                    "e" => assert!(answer.is_err(), "{answer:?}"),
                    _ => assert!(answer.is_ok(), "{chunk:?}: {answer:?}"),
                }
            }
            let idx = ownership_from_keyed(answers).unwrap();
            assert_eq!(
                idx.coord_session_for_claude(A),
                Some(format!("c-{A}").as_str())
            );
            assert!(idx.unanswered_reason(C).unwrap().contains("overall budget"));
        }

        /// A timeout reads as a timeout: the rendered error carries reqwest's
        /// `source()` chain, not only its generic `error sending request` head.
        #[tokio::test]
        async fn a_request_error_carries_its_source_cause() {
            // Bound and never accepted: the connect completes, no answer comes.
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let url = format!(
                "http://127.0.0.1:{}/coord/sessions/worktrees",
                listener.local_addr().unwrap().port()
            );
            let err = send_json::<CoordKeyedSessionWorktrees>(
                test_client().get(&url),
                &url,
                Duration::from_millis(200),
            )
            .await
            .expect_err("nothing answers");
            assert!(err.starts_with(&format!("GET {url}: ")), "{err}");
            assert!(err.contains("timed out"), "the cause is rendered: {err}");
            drop(listener);
        }

        /// A proxy-free client, so an ambient `HTTP_PROXY` cannot reroute
        /// the loopback request these tests make.
        fn test_client() -> reqwest::Client {
            reqwest::Client::builder().no_proxy().build().unwrap()
        }

        /// Serve one canned HTTP response on a loopback port; return its URL.
        async fn serve_once(response: &'static str) -> String {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!(
                "http://127.0.0.1:{}/coord/sessions/worktrees",
                listener.local_addr().unwrap().port()
            );
            tokio::spawn(async move {
                let (mut sock, _) = listener.accept().await.unwrap();
                let mut buf = [0u8; 4096];
                let _ = sock.read(&mut buf).await;
                let _ = sock.write_all(response.as_bytes()).await;
                let _ = sock.shutdown().await;
            });
            url
        }

        /// A non-2xx and an undecodable 2xx are both `Err` — UNKNOWN at the
        /// gate — never an empty index.
        #[tokio::test]
        async fn a_non_2xx_or_undecodable_answer_is_an_error() {
            let url = serve_once(
                "HTTP/1.1 503 Service Unavailable\r\ncontent-length: 4\r\nconnection: close\r\n\r\nbusy",
            )
            .await;
            let err = send_json::<CoordKeyedSessionWorktrees>(
                test_client().get(&url),
                &url,
                Duration::from_secs(5),
            )
            .await
            .unwrap_err();
            assert!(err.contains("503") && err.contains("busy"), "{err}");

            let url = serve_once(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 8\r\nconnection: close\r\n\r\nnot-json",
            )
            .await;
            let err = send_json::<CoordKeyedSessionWorktrees>(
                test_client().get(&url),
                &url,
                Duration::from_secs(5),
            )
            .await
            .unwrap_err();
            assert!(err.starts_with(&format!("decode GET {url}: ")), "{err}");
        }

        #[test]
        fn worktrees_for_session_lists_only_that_sessions_rows() {
            let idx = index(
                r#"{"sessions":[
                    {"sessionId":"AAAA-1","ownerSessionState":"active","worktrees":[
                        {"worktreePath":"agent-worktrees/a/qontinui-runner","repo":"qontinui-runner"},
                        {"worktreePath":"agent-worktrees/a/qontinui-schemas","repo":"qontinui-schemas"},
                        {"worktreePath":"agent-worktrees/a/qontinui-runner","repo":"qontinui-runner"}]},
                    {"sessionId":"bbbb-2","ownerSessionState":"active","worktrees":[
                        {"worktreePath":"agent-worktrees/b/qontinui-web","repo":"qontinui-web"}]}]}"#,
            );
            let paths: Vec<&str> = idx
                .worktrees_for_session("aaaa-1")
                .into_iter()
                .map(|r| r.worktree_path.as_str())
                .collect();
            assert_eq!(
                paths,
                vec![
                    "agent-worktrees/a/qontinui-runner",
                    "agent-worktrees/a/qontinui-schemas"
                ],
                "case-insensitive on the uuid, deduplicated, in ledger order"
            );
            assert!(idx.worktrees_for_session("cccc-3").is_empty());
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> SessionDirectory {
        SessionDirectory::from_parts(
            &[("aaaa1111-2222-3333-4444-555555555555", "amber-otter")],
            &[
                "aaaa1111-2222-3333-4444-555555555555",
                "bbbb1111-2222-3333-4444-555555555555",
            ],
            5,
        )
    }

    // ---- $GIT_DIR resolution ------------------------------------------------

    #[test]
    fn parses_an_absolute_gitdir_line() {
        let got = parse_gitdir_line(
            "gitdir: D:/qontinui-root/qontinui-runner/.git/worktrees/wt-a\n",
            Path::new("D:/x"),
        );
        assert_eq!(
            got.map(|p| norm(&p.to_string_lossy())),
            Some("d:/qontinui-root/qontinui-runner/.git/worktrees/wt-a".to_string())
        );
    }

    #[test]
    fn resolves_a_relative_gitdir_against_the_worktree() {
        let got = parse_gitdir_line("gitdir: ../.git/worktrees/wt-b", Path::new("D:/root/wt"));
        let got = norm(&got.unwrap().to_string_lossy());
        assert!(got.ends_with("../.git/worktrees/wt-b"), "got {got}");
        assert!(got.starts_with("d:/root/wt"), "got {got}");
    }

    #[test]
    fn tolerates_crlf_and_extra_lines() {
        assert!(parse_gitdir_line("gitdir: /a/b\r\nnoise\n", Path::new("/w")).is_some());
    }

    #[test]
    fn a_git_file_without_a_gitdir_line_is_none_not_a_guess() {
        // The admin dir name is NEVER reconstructed from the worktree path.
        assert!(parse_gitdir_line("something else\n", Path::new("/w")).is_none());
        assert!(parse_gitdir_line("gitdir:   \n", Path::new("/w")).is_none());
    }

    // ---- trailers -----------------------------------------------------------

    #[test]
    fn extracts_both_trailers_last_wins() {
        let body = "feat: x\n\nSession-Id: old\nSession-Name: n1\nSession-Id: new\n";
        let (id, name) = parse_session_trailers(body);
        assert_eq!(id.as_deref(), Some("new"));
        assert_eq!(name.as_deref(), Some("n1"));
    }

    #[test]
    fn a_blank_trailer_value_is_not_an_id() {
        let (id, _) = parse_session_trailers("Session-Id:   \n");
        assert_eq!(id, None);
    }

    // ---- wip_state honesty --------------------------------------------------

    #[test]
    fn only_captured_means_captured_and_probe_failed_is_not_clean() {
        assert!(wip_state_is_captured(Some("captured")));
        for s in [
            "unchanged",
            "deferred",
            "clean",
            "probe_failed",
            "uncaptured",
        ] {
            assert!(
                !wip_state_is_captured(Some(s)),
                "{s} must not read captured"
            );
        }
        assert!(wip_state_is_honest_clean(Some("clean")));
        assert!(!wip_state_is_honest_clean(Some("probe_failed")));
    }

    // ---- the rendering rule -------------------------------------------------

    #[test]
    fn nothing_at_all_renders_the_literal_unattributed_never_blank() {
        let a = resolve_attribution(&AttributionInput::default(), &dir());
        assert_eq!(a.session_label, "unattributed");
        assert!(!a.session_label.is_empty());
        assert_eq!(a.source, "none");
        assert!(!a.is_attributed());
    }

    #[test]
    fn a_ghost_renders_unresolvable_never_attributed_and_never_blank() {
        let coord = CoordOwner {
            allocation_session_id: Some("cccc9999-0000-0000-0000-000000000000".into()),
            ..Default::default()
        };
        let a = resolve_attribution(
            &AttributionInput {
                coord: Some(&coord),
                ..Default::default()
            },
            &dir(),
        );
        assert_eq!(
            a.session_label,
            "session cccc9999-0000-0000-0000-000000000000 (unresolvable)"
        );
        assert!(a.unresolvable);
        assert_eq!(a.session_name, None);
    }

    #[test]
    fn an_id_with_a_transcript_but_no_name_is_resolvable_not_a_ghost() {
        let coord = CoordOwner {
            allocation_session_id: Some("bbbb1111-2222-3333-4444-555555555555".into()),
            ..Default::default()
        };
        let a = resolve_attribution(
            &AttributionInput {
                coord: Some(&coord),
                ..Default::default()
            },
            &dir(),
        );
        assert_eq!(
            a.session_label,
            "session bbbb1111-2222-3333-4444-555555555555"
        );
        assert!(!a.unresolvable);
    }

    // ---- source order -------------------------------------------------------

    fn rec(id: &str, last_seen_epoch: Option<i64>) -> CustodyRecord {
        CustodyRecord {
            session_id: Some(id.into()),
            session_name: Some("from-record".into()),
            last_seen: Some("2026-08-24T00:00:00Z".into()),
            last_seen_epoch,
            wip_ref: Some(format!("refs/wip/{id}")),
            wip_state: Some("captured".into()),
            plan_slug: Some("2026-08-22-wip-custody".into()),
            ..Default::default()
        }
    }

    #[test]
    fn the_custody_record_outranks_coord_and_the_trailer() {
        let r = rec("aaaa1111-2222-3333-4444-555555555555", Some(1_000));
        let coord = CoordOwner {
            allocation_session_id: Some("other".into()),
            ..Default::default()
        };
        let a = resolve_attribution(
            &AttributionInput {
                custody: Some(&r),
                coord: Some(&coord),
                trailer_session_id: Some("trailer"),
                now_epoch: 1_060,
                ..Default::default()
            },
            &dir(),
        );
        assert_eq!(a.source, "custody_record");
        assert_eq!(
            a.session_id.as_deref(),
            Some("aaaa1111-2222-3333-4444-555555555555")
        );
        // The directory's name beats the record's own copy.
        assert_eq!(a.session_name.as_deref(), Some("amber-otter"));
        assert_eq!(a.confidence, "strong");
        assert_eq!(a.owner_live, Some(true));
        assert_eq!(a.wip_state.as_deref(), Some("captured"));
    }

    /// **The V-7 regression.** The fill-in USED to be specified for source 2 —
    /// which source 1's unconditional early return makes unreachable for
    /// exactly the population that needs it: a custody record that is PRESENT
    /// and names a session, but carries no `work_unit_id`. This test pins the
    /// source-1 path: `source == "custody_record"` (so we did NOT fall through
    /// to source 2) AND `work_unit_id` is coord's.
    #[test]
    fn a_present_record_with_no_work_unit_takes_coords_without_leaving_source_1() {
        let id = "aaaa1111-2222-3333-4444-555555555555";
        let r = rec(id, Some(1_000));
        assert!(
            r.work_unit_id.is_none(),
            "the fixture must be the blank-record population"
        );
        let coord = CoordOwner {
            allocation_session_id: Some(id.into()),
            allocation_work_unit_id: Some("11111111-2222-3333-4444-555555555555".into()),
            ..Default::default()
        };
        let a = resolve_attribution(
            &AttributionInput {
                custody: Some(&r),
                coord: Some(&coord),
                now_epoch: 1_060,
                ..Default::default()
            },
            &dir(),
        );
        assert_eq!(
            a.source, "custody_record",
            "source 1 must still win — the fill-in is additive, not a fallthrough"
        );
        assert_eq!(
            a.work_unit_id.as_deref(),
            Some("11111111-2222-3333-4444-555555555555"),
            "coord's unit fills the record's blank; before V-7 this was None for \
             100% of worktrees"
        );
        // The record's own fields are untouched by the fill.
        assert_eq!(a.plan_slug.as_deref(), Some("2026-08-22-wip-custody"));
        assert_eq!(a.session_id.as_deref(), Some(id));
    }

    /// The record always wins on a field it actually filled.
    #[test]
    fn coord_never_overwrites_a_work_unit_the_record_already_carries() {
        let id = "aaaa1111-2222-3333-4444-555555555555";
        let mut r = rec(id, Some(1_000));
        r.work_unit_id = Some("record-owns-this".into());
        let coord = CoordOwner {
            allocation_session_id: Some(id.into()),
            allocation_work_unit_id: Some("coord-would-have-said-this".into()),
            ..Default::default()
        };
        let a = resolve_attribution(
            &AttributionInput {
                custody: Some(&r),
                coord: Some(&coord),
                now_epoch: 1_060,
                ..Default::default()
            },
            &dir(),
        );
        assert_eq!(a.work_unit_id.as_deref(), Some("record-owns-this"));
    }

    /// **A disagreeing id must NEVER import coord's unit.** coord's row then
    /// describes a DIFFERENT session's allocation, and taking its unit would
    /// attribute one session's work to another session's plan.
    #[test]
    fn a_disagreeing_session_id_never_imports_coords_work_unit() {
        let r = rec("aaaa1111-2222-3333-4444-555555555555", Some(1_000));
        let coord = CoordOwner {
            allocation_session_id: Some("bbbb1111-2222-3333-4444-555555555555".into()),
            allocation_work_unit_id: Some("someone-elses-plan".into()),
            ..Default::default()
        };
        let a = resolve_attribution(
            &AttributionInput {
                custody: Some(&r),
                coord: Some(&coord),
                now_epoch: 1_060,
                ..Default::default()
            },
            &dir(),
        );
        assert_eq!(a.source, "custody_record");
        assert_eq!(
            a.work_unit_id, None,
            "a mismatched allocation is another session's plan — UNKNOWN beats wrong"
        );

        // A BLANK coord id is not agreement either.
        let blank = CoordOwner {
            allocation_session_id: Some("   ".into()),
            allocation_work_unit_id: Some("someone-elses-plan".into()),
            ..Default::default()
        };
        let a = resolve_attribution(
            &AttributionInput {
                custody: Some(&r),
                coord: Some(&blank),
                now_epoch: 1_060,
                ..Default::default()
            },
            &dir(),
        );
        assert_eq!(a.work_unit_id, None, "a blank coord id is not an agreement");
    }

    /// Source 2 carries the unit rather than blanking it via `unattributed()`:
    /// the id it resolves IS `allocation_session_id`, so agreement is
    /// structural.
    #[test]
    fn the_coord_allocation_source_carries_its_own_work_unit() {
        let coord = CoordOwner {
            allocation_session_id: Some("aaaa1111-2222-3333-4444-555555555555".into()),
            allocation_owner_live: Some(true),
            allocation_work_unit_id: Some("11111111-2222-3333-4444-555555555555".into()),
            ..Default::default()
        };
        let a = resolve_attribution(
            &AttributionInput {
                coord: Some(&coord),
                ..Default::default()
            },
            &dir(),
        );
        assert_eq!(a.source, "coord_allocation");
        assert_eq!(
            a.work_unit_id.as_deref(),
            Some("11111111-2222-3333-4444-555555555555")
        );
    }

    #[test]
    fn a_record_with_no_epoch_is_unknown_liveness_not_dead() {
        let r = rec("aaaa1111-2222-3333-4444-555555555555", None);
        let a = resolve_attribution(
            &AttributionInput {
                custody: Some(&r),
                now_epoch: 9_999_999,
                ..Default::default()
            },
            &dir(),
        );
        assert_eq!(a.owner_live, None, "absent last_seen is UNKNOWN, not dead");
        assert!(!a.custody_stale);
    }

    #[test]
    fn coord_allocation_outranks_the_branch_author_binding() {
        let coord = CoordOwner {
            allocation_session_id: Some("aaaa1111-2222-3333-4444-555555555555".into()),
            allocation_owner_live: Some(true),
            branch_author_session_id: Some("bbbb1111-2222-3333-4444-555555555555".into()),
            ..Default::default()
        };
        let a = resolve_attribution(
            &AttributionInput {
                coord: Some(&coord),
                ..Default::default()
            },
            &dir(),
        );
        assert_eq!(a.source, "coord_allocation");
        assert_eq!(a.confidence, "strong");
    }

    #[test]
    fn the_branch_author_binding_is_consulted_before_declaring_unattributed() {
        let coord = CoordOwner {
            branch_author_session_id: Some("aaaa1111-2222-3333-4444-555555555555".into()),
            ..Default::default()
        };
        let a = resolve_attribution(
            &AttributionInput {
                coord: Some(&coord),
                ..Default::default()
            },
            &dir(),
        );
        assert_eq!(a.source, "coord_branch_author");
        // A durable binding says nothing about liveness.
        assert_eq!(a.owner_live, None);
        assert_eq!(a.confidence, "evidential");
    }

    #[test]
    fn the_commit_trailer_is_last_and_always_weak() {
        let a = resolve_attribution(
            &AttributionInput {
                trailer_session_id: Some("aaaa1111-2222-3333-4444-555555555555"),
                trailer_session_name: Some("ignored-when-directory-resolves"),
                ..Default::default()
            },
            &dir(),
        );
        assert_eq!(a.source, "commit_trailer");
        assert_eq!(a.confidence, "weak");
        assert_eq!(a.owner_live, None);
        assert_eq!(a.session_name.as_deref(), Some("amber-otter"));
    }

    // ---- regressions the review caught --------------------------------------

    /// A commit message that starts with CJK or an emoji must NOT panic. This
    /// runs per worktree inside the census walk, which has no `catch_unwind`
    /// above it — one such commit anywhere on the box would abort the whole
    /// 33-minute walk and leave the survey serving a stale snapshot forever.
    #[test]
    fn a_non_ascii_commit_line_does_not_panic_the_census() {
        for body in [
            "日本語のコミットメッセージ\n\nSession-Id: abc\n",
            "🤖🚀🎉 rebuild the runner\nSession-Name: né\n",
            "Ω",
            "Session-Id",
        ] {
            let _ = parse_session_trailers(body);
        }
        let (id, _) = parse_session_trailers("日本語\nSession-Id: abc\n");
        assert_eq!(id.as_deref(), Some("abc"));
    }

    /// `roots_scanned == 0` means we never looked. Claiming `(unresolvable)`
    /// there would dress OUR failure up as a fact about the session — and the
    /// label is what the operator actually reads.
    #[test]
    fn an_unscanned_directory_never_labels_a_session_unresolvable() {
        let coord = CoordOwner {
            allocation_session_id: Some("zzzz-1111".into()),
            ..Default::default()
        };
        let a = resolve_attribution(
            &AttributionInput {
                coord: Some(&coord),
                ..Default::default()
            },
            &SessionDirectory::empty(),
        );
        assert_eq!(a.session_label, "session zzzz-1111");
        assert!(!a.unresolvable);
        assert!(!a.session_label.is_empty());
    }

    /// Silence is not death. A session idle awaiting the operator, blocked on a
    /// long build, or watching CI is alive and quiet — and a manufactured
    /// `Some(false)` here is what feeds `SessionGone`'s destructive `Remove`.
    #[test]
    fn a_stale_custody_record_is_unknowable_liveness_never_dead() {
        let r = rec("aaaa1111-2222-3333-4444-555555555555", Some(0));
        let a = resolve_attribution(
            &AttributionInput {
                custody: Some(&r),
                now_epoch: CUSTODY_STALE_AFTER_SECS + 10,
                ..Default::default()
            },
            &dir(),
        );
        assert_eq!(
            a.owner_live, None,
            "2h of custody silence is UNKNOWABLE, never a positive death claim"
        );
        // The observation is not lost — it is carried as evidence.
        assert!(a.custody_stale);
        assert_eq!(a.confidence, "evidential");
        assert!(a.last_seen_age_secs.unwrap() > CUSTODY_STALE_AFTER_SECS);
    }

    #[test]
    fn a_fresh_custody_record_is_still_positively_live() {
        let r = rec("aaaa1111-2222-3333-4444-555555555555", Some(1_000));
        let a = resolve_attribution(
            &AttributionInput {
                custody: Some(&r),
                now_epoch: 1_060,
                ..Default::default()
            },
            &dir(),
        );
        assert_eq!(a.owner_live, Some(true), "a written turn IS an observation");
    }

    /// The ready-to-run lines are copy-pasted into a shell, and their inputs
    /// come from a file another repo's shell script writes.
    #[test]
    fn shell_hostile_tokens_and_paths_are_refused_not_escaped_badly() {
        assert!(is_shell_safe_token("aaaa1111-2222-3333-4444-555555555555"));
        assert!(is_shell_safe_token("9f2cdeadbeef"));
        for bad in ["a\"; rm -rf /", "`whoami`", "$(id)", "a b", "", "a\nb"] {
            assert!(!is_shell_safe_token(bad), "{bad:?} must be refused");
        }
        assert_eq!(
            shell_quote_path("D:/qontinui-root/_wt/a"),
            Some("D:/qontinui-root/_wt/a".to_string())
        );
        for bad in ["D:/a\"b", "D:/`x`", "D:/$HOME", ""] {
            assert_eq!(shell_quote_path(bad), None, "{bad:?} must be refused");
        }
    }

    // ---- the two join traps -------------------------------------------------

    #[test]
    fn a_relative_coord_ledger_path_still_joins_the_absolute_census_path() {
        assert!(worktree_path_matches(
            "D:/qontinui-root/agent-worktrees/abc/qontinui-runner",
            "agent-worktrees/abc/qontinui-runner"
        ));
        assert!(worktree_path_matches(
            "D:\\qontinui-root\\_wt\\a",
            "D:/qontinui-root/_wt/a"
        ));
        assert!(!worktree_path_matches("D:/root/wt-abc", "wt-bc"));
        assert!(!worktree_path_matches("D:/root/a", ""));
    }

    #[test]
    fn the_repo_join_spells_both_arms() {
        // `repo_branches.repo` is `owner/name` for 756/774 prod rows and bare
        // for the other 18. A single-arm join drops rows silently.
        assert!(repo_matches("qontinui/qontinui-runner", "qontinui-runner"));
        assert!(repo_matches("qontinui-runner", "qontinui-runner"));
        assert!(!repo_matches("qontinui/qontinui-web", "qontinui-runner"));
        assert!(!repo_matches("qontinui/qontinui-runner", ""));
    }

    // ---- per-session custody slots (plan 2026-09-07 Phase 1c) --------------

    /// A primary-checkout-shaped worktree: `.git` is a directory.
    fn primary_with_slots(slots: &[(&str, &str)]) -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        let slot_dir = tmp.path().join(".git").join(CUSTODY_SLOT_DIR);
        std::fs::create_dir_all(&slot_dir).unwrap();
        for (name, body) in slots {
            std::fs::write(slot_dir.join(name), body).unwrap();
        }
        tmp
    }

    fn slot(id: &str, epoch: i64) -> String {
        serde_json::json!({
            "record_version": 2,
            "session_id": id,
            "session_name": format!("name-{id}"),
            "last_seen": format!("2026-09-26T00:00:{:02}Z", epoch % 60),
            "last_seen_epoch": epoch,
            "wip_state": "clean",
            "wip_ref": format!("refs/wip/{id}"),
        })
        .to_string()
    }

    #[test]
    fn two_slots_yield_two_occupants_newest_first() {
        let tmp = primary_with_slots(&[
            ("older.json", &slot("older", 1_000)),
            ("newer.json", &slot("newer", 2_000)),
        ]);
        let got = read_custody_slots(tmp.path());
        let ids: Vec<_> = got.iter().map(|r| r.session_id.as_deref()).collect();
        assert_eq!(ids, vec![Some("newer"), Some("older")]);
        assert_eq!(got[0].wip_ref.as_deref(), Some("refs/wip/newer"));
        assert_eq!(got[0].last_seen_epoch, Some(2_000));
    }

    #[test]
    fn a_malformed_slot_is_skipped_not_fatal() {
        let tmp = primary_with_slots(&[
            ("good.json", &slot("good", 5)),
            ("bad.json", "{ not json"),
            // The hook's in-flight temp file must never be read.
            ("good.json.tmp.123", &slot("tmp", 9)),
        ]);
        std::fs::create_dir(
            tmp.path()
                .join(".git")
                .join(CUSTODY_SLOT_DIR)
                .join("dir.json"),
        )
        .unwrap();
        let got = read_custody_slots(tmp.path());
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].session_id.as_deref(), Some("good"));
    }

    #[test]
    fn no_slot_dir_is_empty() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join(".git")).unwrap();
        assert!(read_custody_slots(tmp.path()).is_empty());
        // No `.git` at all is empty too, not a panic.
        let bare = tempfile::tempdir().unwrap();
        assert!(read_custody_slots(bare.path()).is_empty());
    }

    #[test]
    fn an_oversize_slot_is_skipped() {
        let mut big = slot("big", 7);
        big.pop(); // drop the closing brace, pad, restore it: still valid JSON
        big.push_str(&format!(",\"pad\":\"{}\"}}", "x".repeat(70 * 1024)));
        assert!(serde_json::from_str::<CustodyRecord>(&big).is_ok());
        let tmp = primary_with_slots(&[("big.json", &big), ("small.json", &slot("small", 1))]);
        let got = read_custody_slots(tmp.path());
        let ids: Vec<_> = got.iter().map(|r| r.session_id.as_deref()).collect();
        assert_eq!(ids, vec![Some("small")]);
    }

    #[test]
    fn a_linked_worktree_resolves_its_git_dir_through_the_git_file() {
        let tmp = tempfile::tempdir().unwrap();
        let admin = tmp.path().join("main/.git/worktrees/wt-a");
        std::fs::create_dir_all(admin.join(CUSTODY_SLOT_DIR)).unwrap();
        std::fs::write(
            admin.join(CUSTODY_SLOT_DIR).join("s.json"),
            slot("linked", 3),
        )
        .unwrap();
        let wt = tmp.path().join("wt-a");
        std::fs::create_dir(&wt).unwrap();
        std::fs::write(wt.join(".git"), format!("gitdir: {}\n", admin.display())).unwrap();
        let got = read_custody_slots(&wt);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].session_id.as_deref(), Some("linked"));
    }

    #[test]
    fn the_mtime_preselection_keeps_the_newest_files_not_the_oldest() {
        // More files than are ever parsed, so the pre-selection truncate
        // actually drops some. File `i` has BOTH mtime and epoch rising with
        // `i`, so a reversed mtime sort (or truncating the wrong end) parses
        // the oldest `CUSTODY_MAX_SLOT_SCAN` files and returns a top occupant
        // that is not the newest.
        let total = CUSTODY_MAX_SLOT_SCAN + 40;
        let tmp = primary_with_slots(&[]);
        let slot_dir = tmp.path().join(".git").join(CUSTODY_SLOT_DIR);
        let base = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        for i in 0..total {
            let path = slot_dir.join(format!("s{i}.json"));
            std::fs::write(&path, slot(&format!("s{i:04}"), 10_000 + i as i64)).unwrap();
            std::fs::File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_modified(base + std::time::Duration::from_secs(i as u64))
                .unwrap();
        }
        let got = read_custody_slots(tmp.path());
        assert_eq!(got.len(), CUSTODY_MAX_SLOTS);
        let expected: Vec<String> = (0..CUSTODY_MAX_SLOTS)
            .map(|k| format!("s{:04}", total - 1 - k))
            .collect();
        let ids: Vec<String> = got.iter().filter_map(|r| r.session_id.clone()).collect();
        assert_eq!(ids, expected);
    }

    #[test]
    fn slots_without_an_epoch_sort_after_slots_with_one_and_are_capped() {
        let mut files: Vec<(String, String)> = (0..(CUSTODY_MAX_SLOTS as i64 + 5))
            .map(|i| (format!("s{i}.json"), slot(&format!("s{i:03}"), 100 + i)))
            .collect();
        files.push((
            "noepoch.json".into(),
            serde_json::json!({"session_id": "noepoch"}).to_string(),
        ));
        let refs: Vec<(&str, &str)> = files
            .iter()
            .map(|(a, b)| (a.as_str(), b.as_str()))
            .collect();
        let tmp = primary_with_slots(&refs);
        let got = read_custody_slots(tmp.path());
        assert_eq!(got.len(), CUSTODY_MAX_SLOTS);
        let top = CUSTODY_MAX_SLOTS as i64 + 4;
        assert_eq!(
            got[0].session_id.as_deref(),
            Some(format!("s{top:03}").as_str())
        );
        assert!(got
            .iter()
            .all(|r| r.session_id.as_deref() != Some("noepoch")));
    }

    /// The spawn-time trailer file: `<QONTINUI_SESSION_NAMES_DIR>/<id>` holds the
    /// name, is readable back through `SessionDirectory`, and an id that is not
    /// a plain filename is refused.
    #[test]
    fn write_session_name_records_trailer_file_in_override_dir() {
        // The ONE shared env lock (a module-local mutex excludes nothing that
        // holds it), declared BEFORE the restore so the restore runs under it.
        let _g = crate::test_env::env_lock();
        let _restore = crate::test_env::EnvVarRestore::capture(&["QONTINUI_SESSION_NAMES_DIR"]);
        let tmp = tempfile::tempdir().unwrap();
        std::env::set_var("QONTINUI_SESSION_NAMES_DIR", tmp.path());
        let id = "AAAA1111-2222-3333-4444-555555555555";
        let path = write_session_name(id, "post-merge-runner#1863").unwrap();
        assert_eq!(path, tmp.path().join(id.to_ascii_lowercase()));
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "post-merge-runner#1863\n"
        );
        let mut d = SessionDirectory::default();
        d.load_names(tmp.path());
        assert_eq!(
            d.names.get(&id.to_ascii_lowercase()).map(String::as_str),
            Some("post-merge-runner#1863")
        );
        assert!(write_session_name("../evil", "x").is_err());
        assert!(write_session_name("", "x").is_err());
    }

    /// A migrated session's copies are ordered by their NEWEST TURN; equal
    /// turns break on the most recently written copy (the migration target),
    /// and only a tie mtime cannot settle falls to the config dir — so the
    /// order never depends on which root the walk met first.
    #[test]
    fn transcript_copies_order_by_newest_turn_then_newest_write_then_config_dir() {
        use crate::session::transcript_last_turn::{FileStamp, TurnRead};
        let copy = |root: &str, modified_ms: Option<i64>| TranscriptCopy {
            config_dir: PathBuf::from(root),
            path: PathBuf::from(format!("{root}/projects/p/s.jsonl")),
            stamp: Some(FileStamp {
                len: 1,
                modified_ms,
            }),
        };
        let turn_of = |c: &TranscriptCopy| match c.config_dir.to_str() {
            // `a` holds the newest turn but the OLDEST write: the turn wins.
            Some("/h/.claude-a") => TurnRead::Read(Some(500)),
            // b, c, f tie on the turn: the newest write wins (f), then the
            // two with the same mtime fall to the dir (b before c).
            Some("/h/.claude-b") => TurnRead::Read(Some(300)),
            Some("/h/.claude-c") => TurnRead::Read(Some(300)),
            Some("/h/.claude-f") => TurnRead::Read(Some(300)),
            Some("/h/.claude-d") => TurnRead::Read(None),
            _ => TurnRead::Unreadable,
        };
        let mut copies = vec![
            copy("/h/.claude-e", Some(9_000)),
            copy("/h/.claude-d", Some(8_000)),
            copy("/h/.claude-c", Some(2_000)),
            copy("/h/.claude-a", Some(1_000)),
            copy("/h/.claude-b", Some(2_000)),
            copy("/h/.claude-f", Some(7_000)),
        ];
        order_by_newest_turn(&mut copies, &mut |c| turn_of(c));
        let order: Vec<&str> = copies
            .iter()
            .map(|c| c.config_dir.to_str().unwrap())
            .collect();
        assert_eq!(
            order,
            vec![
                "/h/.claude-a", // newest turn, despite the oldest write
                "/h/.claude-f", // tied turn, newest write
                "/h/.claude-b", // tied turn and write: the dir decides
                "/h/.claude-c",
                "/h/.claude-d", // readable, no turn
                "/h/.claude-e", // unreadable, despite the newest write
            ]
        );
    }
}
