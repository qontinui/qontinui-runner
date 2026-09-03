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
    let raw = read_bounded(&git_dir.join(CUSTODY_FILE), 64 * 1024)?;
    serde_json::from_str::<CustodyRecord>(&raw).ok()
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
    /// resuming under the wrong root finds no transcript.
    pub config_dir: Option<String>,
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
#[derive(Debug, Clone, Default)]
pub struct SessionDirectory {
    names: HashMap<String, String>,
    /// id → the account root whose `projects/` holds its transcript.
    transcripts: HashMap<String, String>,
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
        let mut me = Self::default();
        me.load_names(&session_names_dir());
        for root in crate::terminal::transcript::find_claude_config_dirs() {
            if me.load_transcripts(&root) {
                me.roots_scanned += 1;
            }
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

    /// Index `<root>/projects/*/<session-id>.jsonl`. Returns whether the root
    /// was readable at all.
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
                if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                    self.transcripts
                        .entry(stem.to_ascii_lowercase())
                        .or_insert_with(|| norm(&root.to_string_lossy()));
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
        let config_dir = self.transcripts.get(&key).cloned();
        SessionIdentity {
            name: self.names.get(&key).cloned(),
            transcript: config_dir.is_some(),
            config_dir,
        }
    }

    /// Test seam.
    #[cfg(test)]
    pub fn from_parts(names: &[(&str, &str)], transcripts: &[&str], roots_scanned: usize) -> Self {
        Self {
            names: names
                .iter()
                .map(|(k, v)| (k.to_ascii_lowercase(), v.to_string()))
                .collect(),
            transcripts: transcripts
                .iter()
                .map(|t| (t.to_ascii_lowercase(), "C:/claude/.claude-test".to_string()))
                .collect(),
            roots_scanned,
        }
    }
}

fn session_names_dir() -> PathBuf {
    if let Ok(over) = std::env::var("QONTINUI_SESSION_NAMES_DIR") {
        if !over.trim().is_empty() {
            return PathBuf::from(over);
        }
    }
    dirs::home_dir()
        .unwrap_or_default()
        .join(".qontinui")
        .join("session-names")
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
}

impl AttributionSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CustodyRecord => "custody_record",
            Self::CoordAllocation => "coord_allocation",
            Self::CoordBranchAuthor => "coord_branch_author",
            Self::CommitTrailer => "commit_trailer",
            Self::None => "none",
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
    }

    #[derive(Debug, Clone, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct CoordSessionRow {
        pub session_id: String,
        /// `expected` | `active` | `pending_resolution` | `stale` | `closed`.
        #[serde(default)]
        pub owner_session_state: Option<String>,
        #[serde(default)]
        pub worktrees: Vec<CoordWorktreeRow>,
    }

    #[derive(Debug, Clone, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct CoordSessionWorktrees {
        #[serde(default)]
        pub sessions: Vec<CoordSessionRow>,
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
            }
        }

        pub fn len(&self) -> usize {
            self.rows.len()
        }

        pub fn is_empty(&self) -> bool {
            self.rows.is_empty()
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
    pub async fn fetch_ownership() -> Result<Option<CoordOwnership>, String> {
        let Some(base) = qontinui_runner_lib::profiles::connected_coord_base() else {
            return Ok(None);
        };
        let url = format!("{}/coord/sessions/worktrees", base.trim_end_matches('/'));
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .map_err(|e| format!("build custody http client: {e}"))?;
        let resp = crate::coord_http::coord_get(&client, &url)
            .send()
            .await
            .map_err(|e| format!("GET {url}: {e}"))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            let excerpt: String = body.chars().take(200).collect();
            return Err(format!("coord returned {status} for GET {url}: {excerpt}"));
        }
        let parsed: CoordSessionWorktrees = resp
            .json()
            .await
            .map_err(|e| format!("decode session-worktrees: {e}"))?;
        Ok(Some(CoordOwnership::from_response(parsed)))
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
}
