//! Quiet barriers — the runner-side reader of the on-disk barrier store, and
//! the wake gate the PTY write funnel consults (plan
//! `2026-09-29-quiet-on-demand-a-safepoint-barrier-that-brings-in-flight-work-to-a-resumable-stop`,
//! Phase 4; D1, D4).
//!
//! ## The store (shared contract, schema 1)
//!
//! A barrier is a short-lived, mandatorily expiring request for quiet on one
//! scope, written by `qontinui-claude-config/scripts/quiet-barrier.sh` as one
//! JSON file per barrier under `<root>/barriers/<id>.json`, where `<root>` is
//! `$QONTINUI_QUIET_DIR` when set, else `<~/.qontinui>/quiet`. This module
//! implements the READER half of that contract, and the reader semantics are
//! identical in every reader:
//!
//! - OPEN iff the file parses, `schema == 1`, `state == "open"` and
//!   `now < until`.
//! - `until` defaults to `opened_at + 20 min`; `max_until` (the hard cap
//!   `extend` may not pass) defaults to `opened_at + 2 h` (`runner-restart`:
//!   1 h). The reader applies both caps itself, in this order: first
//!   `max_until = min(max_until, opened_at + cap(scope))`, then
//!   `until = min(until, max_until)` — so no file can hold a scope past
//!   `opened_at + cap(scope)`, whatever it says (a hand-edited `max_until`, or
//!   a clock jump at write time). The reference reader
//!   (qontinui-claude-config `scripts/lib/quiet_barrier.py` `evaluate()`) is
//!   the semantics this module matches, rule for rule — see [`parse_record`].
//! - Expired or `released` → ABSENT, never blocking: a crashed requester cannot
//!   wedge the box.
//! - Corrupt / unparseable / unknown schema → UNKNOWN for every scope (it may
//!   be any scope), BLOCKING — but only while the file's mtime is younger than
//!   2 h. Older corrupt files are ignored and reported. Readers never delete.
//! - The requester's own session is never held by its barrier.
//!
//! The canonical fixtures live in `fixtures/` beside this file (a byte copy of
//! qontinui-claude-config `scripts/tests/fixtures/quiet-barrier/`), and the
//! tests evaluate every one of them at the fixed instant `2026-09-29T10:20:00Z`.
//!
//! ## The store root honours `$QONTINUI_HOME` — deliberately
//!
//! The contract spells the default root `<home_dir>/.qontinui/quiet`; this
//! reader resolves it through [`qontinui_runner_lib::ambient::qontinui_dir`],
//! which honours `$QONTINUI_HOME`. On the primary runner the two are the same
//! directory. A temp/secondary runner launched with its own `QONTINUI_HOME`
//! reads its own store and is therefore isolated from the primary's barriers —
//! which is correct: a `runner-restart` barrier concerns the runner whose store
//! it is, and a temp runner must not hold its wakes because the primary is
//! being restarted. `$QONTINUI_QUIET_DIR` still overrides both.
//!
//! ## Why local files, not coord
//!
//! The enforcement points — here, the PTY write funnel — must answer in
//! microseconds and must work while coord is down. The directory scan is
//! therefore CACHED for [`SCAN_CACHE_TTL`]: the funnel sees keystrokes, and a
//! `readdir` per keystroke would be a regression the operator can feel. The
//! cache holds the PARSED files, not a verdict, so expiry is still judged
//! against the current clock on every call — a barrier stops blocking the
//! instant its `until` passes, not up to a TTL later.
//!
//! ## Only `runner-restart` gates wakes
//!
//! `repo:` and `target:` barriers are enforced by the session hooks and the
//! cargo wrappers (Phases 2-3). The runner reads them only to report them. A
//! corrupt file is UNKNOWN for every scope, so it also holds autonomous wakes
//! for as long as it is younger than 2 h — fail closed, bounded.

pub mod resume;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use chrono::{DateTime, Utc};

/// The one schema this reader understands.
pub const SCHEMA: u64 = 1;

/// Env override for the store root (see the module docs).
pub const QUIET_DIR_ENV: &str = "QONTINUI_QUIET_DIR";

/// Default lifetime when a record carries no `until`.
pub const DEFAULT_TTL: chrono::Duration = chrono::Duration::minutes(20);
/// Hard cap on any barrier's lifetime.
pub const CAP_GENERAL: chrono::Duration = chrono::Duration::hours(2);
/// Hard cap on a `runner-restart` barrier's lifetime.
pub const CAP_RUNNER_RESTART: chrono::Duration = chrono::Duration::hours(1);
/// How long a corrupt file blocks, measured from its mtime.
pub const CORRUPT_BLOCK_WINDOW: chrono::Duration = chrono::Duration::hours(2);

/// How long a directory scan is reused by the hot-path reader.
pub const SCAN_CACHE_TTL: Duration = Duration::from_millis(1500);

/// Machine-readable prefix of the refusal the PTY funnel returns for a wake the
/// barrier deferred. The full shape is
/// `QUIET_BARRIER_DEFERRED: barrier=<id> <free text>` — see
/// [`deferred_error`] / [`deferred_barrier_id`].
pub const QUIET_BARRIER_DEFERRED: &str = "QUIET_BARRIER_DEFERRED";

/// The `barrier_id` reported for a deferral caused by an UNKNOWN barrier state
/// (a corrupt file, an unreadable directory) rather than a named barrier.
pub const UNKNOWN_BARRIER_ID: &str = "unknown";

// ---------------------------------------------------------------------------
// Records
// ---------------------------------------------------------------------------

/// A barrier's scope, per the contract grammar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    /// `repo:<depth-1 checkout dir name>`
    Repo(String),
    /// `target:<absolute canonical dir, forward slashes>`
    Target(String),
    /// `runner-restart`
    RunnerRestart,
    /// Any other string. Not corrupt — the reference reader keeps it too — but
    /// it matches no reader's scope, so it holds nothing.
    Other(String),
}

impl Scope {
    /// Parse the wire form.
    pub fn parse(raw: &str) -> Self {
        if raw == "runner-restart" {
            return Self::RunnerRestart;
        }
        if let Some(name) = raw.strip_prefix("repo:").filter(|n| !n.is_empty()) {
            return Self::Repo(name.to_string());
        }
        if let Some(dir) = raw.strip_prefix("target:").filter(|d| !d.is_empty()) {
            return Self::Target(dir.to_string());
        }
        Self::Other(raw.to_string())
    }

    /// The wire form.
    pub fn as_wire(&self) -> String {
        match self {
            Self::Repo(n) => format!("repo:{n}"),
            Self::Target(d) => format!("target:{d}"),
            Self::RunnerRestart => "runner-restart".to_string(),
            Self::Other(raw) => raw.clone(),
        }
    }

    /// The lifetime cap `max_until` defaults to for a scope string: 1 h for
    /// `runner-restart`, 2 h for everything else (the reference's `cap_for`).
    pub fn cap_for(raw: &str) -> chrono::Duration {
        if raw == "runner-restart" {
            CAP_RUNNER_RESTART
        } else {
            CAP_GENERAL
        }
    }
}

/// One well-formed barrier record, with its EFFECTIVE `until` already capped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Barrier {
    pub id: String,
    pub scope: Scope,
    pub purpose: Option<String>,
    /// `requester.session_id` — the one session this barrier never holds.
    pub requester_session_id: Option<String>,
    pub opened_at: DateTime<Utc>,
    /// Effective deadline: `min(until, max_until)`, where an absent `until` is
    /// `opened_at + 20 min` and an absent `max_until` is `opened_at + cap`.
    pub until: DateTime<Utc>,
    pub straggler_policy: Option<String>,
}

impl Barrier {
    /// Whether `session_id` is this barrier's requester (exempt from it).
    pub fn exempts(&self, session_id: &str) -> bool {
        self.requester_session_id
            .as_deref()
            .is_some_and(|r| !r.is_empty() && r == session_id)
    }
}

/// What one file on disk says, independent of the clock (so a cached scan can
/// be re-judged against a fresh `now` on every read).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileRecord {
    /// Well-formed, `state: "open"`. Whether it is still open is a question
    /// for [`FileRecord::verdict`].
    Recorded(Barrier),
    /// Well-formed, `state: "released"` — tolerated and read as absent.
    Released { id: String },
    /// Unparseable, unknown schema, or outside the grammar. `mtime` bounds how
    /// long it blocks; `None` means the mtime could not be read.
    Corrupt {
        reason: String,
        mtime: Option<DateTime<Utc>>,
    },
}

/// The verdict on one file at one instant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileVerdict {
    Open(Barrier),
    /// Expired or released: never blocking.
    Absent,
    /// Corrupt and younger than [`CORRUPT_BLOCK_WINDOW`]: blocking, any scope.
    Unknown(String),
    /// Corrupt and older than the window: ignored, but reported.
    StaleCorrupt(String),
}

/// RFC 3339 UTC with a trailing `Z` (optionally fractional seconds) — exactly
/// the reference reader's `TS_RE`; an offset form is NOT accepted.
pub fn parse_ts(raw: &str) -> Option<DateTime<Utc>> {
    let rest = raw.strip_suffix('Z')?;
    if rest.len() < 19 || !rest.is_char_boundary(19) {
        return None;
    }
    let (base, frac) = rest.split_at(19);
    if !frac.is_empty() {
        let digits = frac.strip_prefix('.')?;
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
    }
    chrono::NaiveDateTime::parse_from_str(base, "%Y-%m-%dT%H:%M:%S")
        .ok()
        .map(|n| n.and_utc())
}

/// A present-but-unparseable timestamp field (including `null`) is corrupt;
/// an absent one takes `default`.
fn ts_field(
    rec: &serde_json::Map<String, serde_json::Value>,
    key: &str,
    default: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    match rec.get(key) {
        None => Some(default),
        Some(v) => v.as_str().and_then(parse_ts),
    }
}

/// The reference reader's `rec.get("schema") != SCHEMA` is a Python `==`, under
/// which `1`, `1.0` and `true` all equal `1`. Matched exactly, so the two
/// readers never disagree on whether a file is corrupt.
fn schema_is_one(v: &serde_json::Value) -> bool {
    match v {
        serde_json::Value::Number(n) => {
            n.as_u64() == Some(SCHEMA) || n.as_f64() == Some(SCHEMA as f64)
        }
        serde_json::Value::Bool(b) => *b,
        _ => false,
    }
}

/// Parse one barrier file's bytes. Never fails: anything unreadable is a
/// [`FileRecord::Corrupt`] carrying the reason. Mirrors the reference reader
/// (qontinui-claude-config `scripts/lib/quiet_barrier.py` `evaluate()`) rule
/// for rule:
///
/// - not a JSON object, `schema != 1`, a non-string `id`/`scope`, a `state`
///   outside `open|released`, an unparseable `opened_at` → corrupt;
/// - `until` absent → `opened_at + 20 min`; `max_until` absent →
///   `opened_at + cap(scope)` (1 h for `runner-restart`, else 2 h); either
///   present but unparseable → corrupt;
/// - `max_until` is first capped at `opened_at + cap(scope)` (a file's own
///   `max_until` is never trusted past the scope's lifetime cap), THEN the
///   effective deadline is `min(until, max_until)`;
/// - `requester.session_id` counts only as a non-empty string.
///
/// A scope string outside the grammar is NOT corrupt (it simply matches no
/// reader's scope), exactly as in the reference.
pub fn parse_record(bytes: &[u8], mtime: Option<DateTime<Utc>>) -> FileRecord {
    let corrupt = |reason: String| FileRecord::Corrupt { reason, mtime };
    let value: serde_json::Value = match serde_json::from_slice(bytes) {
        Ok(v) => v,
        Err(e) => return corrupt(format!("unparseable: {e}")),
    };
    let Some(rec) = value.as_object() else {
        return corrupt("not a JSON object".to_string());
    };
    let schema = rec.get("schema");
    if !schema.is_some_and(schema_is_one) {
        return corrupt(format!(
            "unknown schema {}",
            schema.map(|v| v.to_string()).unwrap_or_else(|| "None".into())
        ));
    }
    let scope_raw = rec.get("scope").and_then(|v| v.as_str());
    let id = rec.get("id").and_then(|v| v.as_str());
    let state = rec.get("state").and_then(|v| v.as_str());
    let opened_at = rec.get("opened_at").and_then(|v| v.as_str()).and_then(parse_ts);
    let (Some(scope_raw), Some(id), Some(state @ ("open" | "released")), Some(opened_at)) =
        (scope_raw, id, state, opened_at)
    else {
        return corrupt("missing or malformed id/scope/state/opened_at".to_string());
    };
    let scope = Scope::parse(scope_raw);
    let until = ts_field(rec, "until", opened_at + DEFAULT_TTL);
    let scope_cap = opened_at + Scope::cap_for(scope_raw);
    let max_until = ts_field(rec, "max_until", scope_cap);
    let (Some(until), Some(max_until)) = (until, max_until) else {
        return corrupt("malformed until/max_until".to_string());
    };
    // Contract order: cap `max_until` at the scope's lifetime FIRST, then clamp
    // `until` to it.
    let max_until = max_until.min(scope_cap);
    if state == "released" {
        return FileRecord::Released { id: id.to_string() };
    }
    let requester_session_id = rec
        .get("requester")
        .and_then(|r| r.as_object())
        .and_then(|r| r.get("session_id"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    FileRecord::Recorded(Barrier {
        id: id.to_string(),
        scope,
        purpose: rec.get("purpose").and_then(|v| v.as_str()).map(str::to_string),
        requester_session_id,
        opened_at,
        until: until.min(max_until),
        straggler_policy: rec
            .get("straggler_policy")
            .and_then(|v| v.as_str())
            .map(str::to_string),
    })
}

impl FileRecord {
    /// Judge this file at `now`.
    pub fn verdict(&self, now: DateTime<Utc>) -> FileVerdict {
        match self {
            Self::Recorded(b) if now < b.until => FileVerdict::Open(b.clone()),
            Self::Recorded(_) | Self::Released { .. } => FileVerdict::Absent,
            Self::Corrupt { reason, mtime } => match mtime {
                Some(m) if now - *m >= CORRUPT_BLOCK_WINDOW => {
                    FileVerdict::StaleCorrupt(reason.clone())
                }
                // A future mtime is a clock artifact; it still blocks (bounded
                // by the window once the clock passes it). An unreadable mtime
                // cannot be bounded — fail closed.
                _ => FileVerdict::Unknown(reason.clone()),
            },
        }
    }
}

// ---------------------------------------------------------------------------
// Directory scan
// ---------------------------------------------------------------------------

/// One pass over `<root>/barriers/`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Scan {
    /// `(file name, record)` for every `*.json` file, in name order.
    pub files: Vec<(String, FileRecord)>,
    /// Set when the directory itself could not be listed (other than
    /// "does not exist", which is simply an empty store). Blocking.
    pub dir_error: Option<String>,
}

fn system_time_to_utc(t: SystemTime) -> DateTime<Utc> {
    DateTime::<Utc>::from(t)
}

/// Scan `<root>/barriers/`. A missing directory is an empty store. Temp files
/// (the writer's atomic-rename staging) are skipped: only `*.json` names that
/// do not start with `.` are barrier files.
pub fn scan_dir(root: &Path) -> Scan {
    let dir = root.join("barriers");
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Scan::default(),
        Err(e) => {
            return Scan {
                files: Vec::new(),
                dir_error: Some(format!("barrier dir {} unreadable: {e}", dir.display())),
            }
        }
    };
    let mut files: Vec<(String, FileRecord)> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') || !name.ends_with(".json") {
            continue;
        }
        let path = entry.path();
        let mtime = std::fs::metadata(&path)
            .and_then(|m| m.modified())
            .ok()
            .map(system_time_to_utc);
        let record = match std::fs::read(&path) {
            Ok(bytes) => parse_record(&bytes, mtime),
            // Released between readdir and read: the writer deletes on release.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => FileRecord::Corrupt {
                reason: format!("unreadable: {e}"),
                mtime,
            },
        };
        files.push((name, record));
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));
    Scan {
        files,
        dir_error: None,
    }
}

/// The runner-restart question, answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BarrierState {
    /// No open runner-restart barrier and nothing unknown.
    Absent,
    /// An open runner-restart barrier.
    Open(Barrier),
    /// A corrupt barrier file younger than the window, or an unreadable store
    /// — it may be a runner-restart barrier, so it is treated as one.
    Unknown(String),
}

impl BarrierState {
    /// Whether autonomous wakes are held (Open or Unknown — fail closed).
    pub fn holds_wakes(&self) -> bool {
        !matches!(self, Self::Absent)
    }

    /// The id a deferral names: the barrier's, or [`UNKNOWN_BARRIER_ID`].
    pub fn barrier_id(&self) -> Option<&str> {
        match self {
            Self::Absent => None,
            Self::Open(b) => Some(&b.id),
            Self::Unknown(_) => Some(UNKNOWN_BARRIER_ID),
        }
    }
}

impl Scan {
    /// Every file's verdict at `now`, in name order.
    pub fn verdicts(&self, now: DateTime<Utc>) -> Vec<(String, FileVerdict)> {
        self.files
            .iter()
            .map(|(name, rec)| (name.clone(), rec.verdict(now)))
            .collect()
    }

    /// The runner-restart state at `now`. A fresh corrupt file (or an
    /// unreadable store) makes the answer `Unknown` EVEN BESIDE an open
    /// runner-restart barrier: the corrupt file may itself be a barrier of any
    /// scope with any requester, so nobody — not even the open barrier's own
    /// requester — is exempt while it is fresh. This matches the reference
    /// reader and the contract ("Corrupt … → state `unknown` for scope `*`,
    /// BLOCKING"). With no unknown, the earliest-opened open runner-restart
    /// barrier is reported when a hand-edit left two.
    pub fn runner_restart_state(&self, now: DateTime<Utc>) -> BarrierState {
        let mut open: Option<Barrier> = None;
        let mut unknown: Vec<String> = Vec::new();
        if let Some(e) = &self.dir_error {
            unknown.push(e.clone());
        }
        for (name, verdict) in self.verdicts(now) {
            match verdict {
                FileVerdict::Open(b) if b.scope == Scope::RunnerRestart => {
                    if open.as_ref().is_none_or(|o| b.opened_at < o.opened_at) {
                        open = Some(b);
                    }
                }
                FileVerdict::Unknown(reason) => {
                    unknown.push(format!("barrier file {name} is unreadable ({reason})"))
                }
                FileVerdict::Open(_) | FileVerdict::Absent | FileVerdict::StaleCorrupt(_) => {}
            }
        }
        match (open, unknown.is_empty()) {
            (Some(b), true) => BarrierState::Open(b),
            (None, true) => BarrierState::Absent,
            (Some(b), false) => BarrierState::Unknown(format!(
                "{} (beside open runner-restart barrier {})",
                unknown.join("; "),
                b.id
            )),
            (None, false) => BarrierState::Unknown(unknown.join("; ")),
        }
    }
}

/// A cached reader over one store root.
#[derive(Debug)]
pub struct BarrierReader {
    root: Option<PathBuf>,
    ttl: Duration,
    cache: Mutex<Option<(Instant, Arc<Scan>)>>,
}

impl BarrierReader {
    /// A reader over `root` (`None` = no resolvable store, read as empty).
    pub fn new(root: Option<PathBuf>, ttl: Duration) -> Self {
        Self {
            root,
            ttl,
            cache: Mutex::new(None),
        }
    }

    /// The (possibly cached) scan.
    pub fn scan(&self) -> Arc<Scan> {
        let Some(root) = &self.root else {
            return Arc::new(Scan::default());
        };
        let mut slot = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((at, scan)) = slot.as_ref() {
            if at.elapsed() < self.ttl {
                return scan.clone();
            }
        }
        let scan = Arc::new(scan_dir(root));
        *slot = Some((Instant::now(), scan.clone()));
        scan
    }

    /// The runner-restart state at `now`.
    pub fn runner_restart_state(&self, now: DateTime<Utc>) -> BarrierState {
        self.scan().runner_restart_state(now)
    }
}

/// The store root: `$QONTINUI_QUIET_DIR` when set and non-blank, else
/// `<~/.qontinui>/quiet` (through the crate's one ambient seam).
pub fn quiet_root() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var(QUIET_DIR_ENV) {
        if !dir.trim().is_empty() {
            return Some(PathBuf::from(dir));
        }
    }
    qontinui_runner_lib::ambient::qontinui_dir().map(|d| d.join("quiet"))
}

#[cfg(not(test))]
fn global_reader() -> &'static BarrierReader {
    static READER: std::sync::OnceLock<BarrierReader> = std::sync::OnceLock::new();
    READER.get_or_init(|| BarrierReader::new(quiet_root(), SCAN_CACHE_TTL))
}

#[cfg(test)]
thread_local! {
    /// Test seam: the state [`runner_restart_barrier_open`] answers on THIS
    /// thread. A unit-test process never reads the real store — a barrier an
    /// operator opened on the dev box must not change a test's outcome.
    static TEST_STATE: std::cell::RefCell<BarrierState> =
        const { std::cell::RefCell::new(BarrierState::Absent) };
}

/// Test seam: run `f` with [`runner_restart_barrier_open`] answering `state` on
/// this thread, restoring the previous answer afterwards — including when `f`
/// panics (a drop guard), so one failing test cannot leak a barrier into the
/// next test that reuses the thread.
#[cfg(test)]
pub fn with_test_state<R>(state: BarrierState, f: impl FnOnce() -> R) -> R {
    struct Restore(Option<BarrierState>);
    impl Drop for Restore {
        fn drop(&mut self) {
            if let Some(prev) = self.0.take() {
                TEST_STATE.with(|s| *s.borrow_mut() = prev);
            }
        }
    }
    let _restore = Restore(Some(TEST_STATE.with(|s| s.replace(state))));
    f()
}

/// Is a runner-restart barrier open right now? Cheap: the directory scan is
/// cached for [`SCAN_CACHE_TTL`], and expiry is judged against the live clock.
pub fn runner_restart_barrier_open() -> BarrierState {
    #[cfg(test)]
    {
        TEST_STATE.with(|s| s.borrow().clone())
    }
    #[cfg(not(test))]
    {
        global_reader().runner_restart_state(Utc::now())
    }
}

// ---------------------------------------------------------------------------
// Wake gating (D4)
// ---------------------------------------------------------------------------

/// Who a PTY write is on behalf of, for barrier purposes. Assigned per
/// `PtyWriteCaller` variant by an exhaustive match
/// (`PtyWriteCaller::wake_class`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WakeClass {
    /// A human's keystrokes — never held.
    Operator,
    /// A programmatic prompt into a live session — deferred under a
    /// runner-restart barrier.
    Autonomous,
    /// The session's own launch / transport / exit plumbing — never held.
    SessionInternal,
    /// Unit-test fixtures.
    Test,
}

/// The gate's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WakeDecision {
    Allow,
    /// Deferred, not dropped: the caller must keep the work and retry after
    /// the barrier is released or expires.
    Defer { barrier_id: String, reason: String },
}

/// PURE: the wake decision for `class` against `state`, for a write into a
/// session whose own ids are `target_session_ids` (requester exemption).
pub fn wake_decision(
    class: WakeClass,
    state: &BarrierState,
    target_session_ids: &[String],
) -> WakeDecision {
    match class {
        WakeClass::Operator | WakeClass::SessionInternal | WakeClass::Test => WakeDecision::Allow,
        WakeClass::Autonomous => match state {
            BarrierState::Absent => WakeDecision::Allow,
            BarrierState::Open(b) => {
                if target_session_ids.iter().any(|id| b.exempts(id)) {
                    WakeDecision::Allow
                } else {
                    WakeDecision::Defer {
                        barrier_id: b.id.clone(),
                        reason: format!(
                            "runner-restart barrier {} is open until {} ({})",
                            b.id,
                            b.until.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                            b.purpose.as_deref().unwrap_or("no purpose recorded"),
                        ),
                    }
                }
            }
            BarrierState::Unknown(reason) => WakeDecision::Defer {
                barrier_id: UNKNOWN_BARRIER_ID.to_string(),
                reason: format!(
                    "the quiet-barrier store is UNKNOWN ({reason}), so autonomous wakes are held \
                     (fail-closed, bounded by the 2 h corrupt-file window)"
                ),
            },
        },
    }
}

/// The wake decision for `caller` writing into a session whose own ids are
/// `target_session_ids`, against the live barrier state.
pub fn wake_allowed(
    caller: &crate::terminal::session::PtyWriteCaller,
    target_session_ids: &[String],
) -> WakeDecision {
    let class = caller.wake_class();
    // Operator / internal writes never pay for the (cached) scan.
    if class != WakeClass::Autonomous {
        return WakeDecision::Allow;
    }
    wake_decision(class, &runner_restart_barrier_open(), target_session_ids)
}

/// Is the wake gate holding autonomous wakes under `state`? The value of
/// `resume.wake_paths_gated`: the gate's decision function is asked about an
/// anonymous `Autonomous` wake. It reports the gate's decision, not that every
/// door consults the gate (that is pinned by tests).
pub fn autonomous_wakes_deferred_under(state: &BarrierState) -> bool {
    matches!(
        wake_decision(WakeClass::Autonomous, state, &[]),
        WakeDecision::Defer { .. }
    )
}

// ---------------------------------------------------------------------------
// SDK message wakes (the stream-json stdin of a `ClaudeSession`)
// ---------------------------------------------------------------------------

/// Who an SDK user message (`ClaudeSession::send_user_message`) is on behalf
/// of — the SDK twin of `PtyWriteCaller`. Classified by an exhaustive match
/// with no `_` arm ([`Self::wake_class`]), so a new door cannot compile
/// without being classified.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SdkMessageCaller {
    /// The Tauri `send_user_message` command: the operator typing into the
    /// runner's own session view.
    TauriAiSessionMessage,
    /// The backend chat relay (`backend_relay` `chat_message`): the operator
    /// typing into the web chat, relayed verbatim. A human's keystrokes, so it
    /// passes exactly like `RemoteTerminalInput` does on the PTY side.
    BackendChatRelay,
    /// The executor's first message to a session it just spawned (the brief):
    /// spawn plumbing, governed at spawn by the drain.
    ExecutorFirstMessage,
    /// `POST /sessions/{id}/message`: the agent door (the Coordinator,
    /// `/auto-review`, `/summarize-session`). `Autonomous`.
    HttpSessionMessage,
    /// `POST /task-runs/{id}/message`: the web dashboard's chat box
    /// (qontinui-web `AiConversationWidget`) and the mobile chat (qontinui-mobile
    /// `ChatClient`) — an operator typing. `Operator`: an operator's keystroke
    /// is never swallowed (UX). Agents message a session through
    /// `POST /sessions/{id}/message` ([`Self::HttpSessionMessage`]), which
    /// stays `Autonomous`.
    HttpTaskRunMessage,
    /// The conductor's step-5 re-prompt (`AiSessionDispatcher::reprompt`).
    ConductorReprompt,
    /// The coord session-bus poller's SDK delivery.
    SessionMessagePoller,
    #[cfg(test)]
    Test,
}

impl SdkMessageCaller {
    /// The D4 class. Exhaustive — no `_` arm.
    pub fn wake_class(self) -> WakeClass {
        match self {
            Self::TauriAiSessionMessage => WakeClass::Operator,
            Self::BackendChatRelay => WakeClass::Operator,
            Self::ExecutorFirstMessage => WakeClass::SessionInternal,
            Self::HttpSessionMessage => WakeClass::Autonomous,
            Self::HttpTaskRunMessage => WakeClass::Operator,
            Self::ConductorReprompt => WakeClass::Autonomous,
            Self::SessionMessagePoller => WakeClass::Autonomous,
            #[cfg(test)]
            Self::Test => WakeClass::Test,
        }
    }

    /// Stable tag for logs and the deferral string.
    pub fn tag(self) -> &'static str {
        match self {
            Self::TauriAiSessionMessage => "tauri_ai_session_message",
            Self::BackendChatRelay => "backend_chat_relay",
            Self::ExecutorFirstMessage => "executor_first_message",
            Self::HttpSessionMessage => "http_session_message",
            Self::HttpTaskRunMessage => "http_task_run_message",
            Self::ConductorReprompt => "conductor_reprompt",
            Self::SessionMessagePoller => "session_message_poller_sdk",
            #[cfg(test)]
            Self::Test => "test",
        }
    }
}

/// PURE: the deferral an SDK message from `caller` into a session whose own
/// ids are `target_session_ids` (the runner session id and the pinned Claude
/// session id) gets under `state` — the typed [`deferred_error`] — or `None`
/// when it may go out.
pub fn sdk_wake_refusal(
    caller: SdkMessageCaller,
    state: &BarrierState,
    session_id: &str,
    target_session_ids: &[String],
) -> Option<String> {
    match wake_decision(caller.wake_class(), state, target_session_ids) {
        WakeDecision::Allow => None,
        WakeDecision::Defer { barrier_id, reason } => Some(deferred_error(
            &barrier_id,
            caller.tag(),
            session_id,
            &reason,
        )),
    }
}

/// [`sdk_wake_refusal`] against the live barrier state. Operator and
/// session-internal callers never pay for the (cached) scan.
pub fn sdk_wake_gate(
    caller: SdkMessageCaller,
    session_id: &str,
    target_session_ids: &[String],
) -> Result<(), String> {
    if caller.wake_class() != WakeClass::Autonomous {
        return Ok(());
    }
    match sdk_wake_refusal(
        caller,
        &runner_restart_barrier_open(),
        session_id,
        target_session_ids,
    ) {
        None => Ok(()),
        Some(e) => Err(e),
    }
}

/// `Some((409, body))` when `err` is a quiet-barrier deferral — the mapping
/// every HTTP door that injects into a session uses, so a deferral answers
/// `409` carrying the `QUIET_BARRIER_DEFERRED` string (barrier id included)
/// instead of a 500 or a 200-with-`success:false`.
pub fn http_conflict(err: &str) -> Option<(axum::http::StatusCode, String)> {
    deferred_barrier_id(err)?;
    Some((axum::http::StatusCode::CONFLICT, err.to_string()))
}

/// Wrap an error with `context` UNLESS it is a quiet-barrier deferral, which
/// is returned verbatim so [`deferred_barrier_id`] still recognises it
/// upstream (it parses the `QUIET_BARRIER_DEFERRED:` prefix).
pub fn wrap_unless_deferred(err: String, context: impl FnOnce(&str) -> String) -> String {
    if deferred_barrier_id(&err).is_some() {
        err
    } else {
        context(&err)
    }
}

// ---------------------------------------------------------------------------
// In-memory deferred autonomous prompts (item: they die with the restart)
// ---------------------------------------------------------------------------

/// The registry of autonomous prompts the runner holds IN MEMORY for a
/// terminal session — work a restart would silently drop. The `resume`
/// classifier reads it: a session with an entry is a
/// `pending_autonomous_prompt` straggler, so a restart waits for the prompt to
/// be delivered (after release) or the operator sees it.
///
/// Producers: the account-migration prompt-when-idle watcher (for its whole
/// life, via [`pending::guard`]) and the auto-responder (from scheduling to
/// delivery or failure; kept across a barrier deferral and cleared when the
/// rule's text is no longer on screen).
pub mod pending {
    use std::collections::{BTreeMap, HashMap};
    use std::sync::Mutex;

    /// terminal id → (producer key → human description).
    static REGISTRY: Mutex<Option<HashMap<String, BTreeMap<String, String>>>> = Mutex::new(None);

    /// Record a pending prompt. Idempotent per `(terminal_id, key)`.
    pub fn mark(terminal_id: &str, key: &str, what: &str) {
        let mut g = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
        g.get_or_insert_with(HashMap::new)
            .entry(terminal_id.to_string())
            .or_default()
            .insert(key.to_string(), what.to_string());
    }

    /// Forget a pending prompt (delivered, failed, or no longer wanted).
    pub fn clear(terminal_id: &str, key: &str) {
        let mut g = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(map) = g.as_mut() {
            if let Some(entries) = map.get_mut(terminal_id) {
                entries.remove(key);
                if entries.is_empty() {
                    map.remove(terminal_id);
                }
            }
        }
    }

    /// Every `(session, key)` whose key starts with `prefix` — for a producer
    /// pruning its own entries (collected, so the caller decides outside the
    /// registry lock).
    pub fn keys_with_prefix(prefix: &str) -> Vec<(String, String)> {
        let g = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
        g.as_ref()
            .map(|m| {
                m.iter()
                    .flat_map(|(sid, entries)| {
                        entries
                            .keys()
                            .filter(|k| k.starts_with(prefix))
                            .map(move |k| (sid.clone(), k.clone()))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Whether `(terminal_id, key)` is pending.
    pub fn contains(terminal_id: &str, key: &str) -> bool {
        let g = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
        g.as_ref()
            .and_then(|m| m.get(terminal_id))
            .is_some_and(|e| e.contains_key(key))
    }

    /// The pending prompts for `terminal_id`, as `key (what)` strings, sorted.
    pub fn for_session(terminal_id: &str) -> Vec<String> {
        let g = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
        g.as_ref()
            .and_then(|m| m.get(terminal_id))
            .map(|e| e.iter().map(|(k, w)| format!("{k} ({w})")).collect())
            .unwrap_or_default()
    }

    /// Clears its entry when dropped — for a producer whose whole task IS the
    /// pending prompt (the watcher returns or unwinds → the entry goes).
    #[must_use = "the entry is cleared when the guard is dropped"]
    pub struct Guard {
        terminal_id: String,
        key: String,
    }

    impl Drop for Guard {
        fn drop(&mut self) {
            clear(&self.terminal_id, &self.key);
        }
    }

    /// [`mark`] now, [`clear`] when the returned guard drops.
    pub fn guard(terminal_id: &str, key: &str, what: &str) -> Guard {
        mark(terminal_id, key, what);
        Guard {
            terminal_id: terminal_id.to_string(),
            key: key.to_string(),
        }
    }
}

/// `Some(barrier_id)` while autonomous wakes are held by the live state —
/// for the producers that want to not even start work (the looping-agent
/// supervisor, the auto-responder's scan) rather than meet the funnel's
/// refusal. Not requester-aware: those producers have no single target.
pub fn autonomous_wakes_held() -> Option<String> {
    runner_restart_barrier_open().barrier_id().map(str::to_string)
}

/// The refusal string for a deferred write. Shape:
/// `QUIET_BARRIER_DEFERRED: barrier=<id> <caller> write to terminal <tid> deferred — <reason>`.
pub fn deferred_error(barrier_id: &str, caller: &str, terminal_id: &str, reason: &str) -> String {
    format!(
        "{QUIET_BARRIER_DEFERRED}: barrier={barrier_id} {caller} write to terminal {terminal_id} \
         deferred — {reason}. Nothing was written; retry after the barrier is released or expires."
    )
}

/// The barrier id out of a [`deferred_error`] string; `None` for any other
/// error (so callers branch on it with no string matching of their own).
pub fn deferred_barrier_id(err: &str) -> Option<&str> {
    let rest = err
        .strip_prefix(QUIET_BARRIER_DEFERRED)?
        .strip_prefix(": barrier=")?;
    let id = rest.split_whitespace().next()?;
    (!id.is_empty()).then_some(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fixed "now" every fixture is evaluated at (contract).
    fn fixed_now() -> DateTime<Utc> {
        parse_ts("2026-09-29T10:20:00Z").unwrap()
    }

    fn fixture(name: &str) -> &'static [u8] {
        match name {
            "open-runner-restart.json" => include_bytes!("fixtures/open-runner-restart.json"),
            "open-repo.json" => include_bytes!("fixtures/open-repo.json"),
            "open-target.json" => include_bytes!("fixtures/open-target.json"),
            "expired.json" => include_bytes!("fixtures/expired.json"),
            "released.json" => include_bytes!("fixtures/released.json"),
            "until-past-max.json" => include_bytes!("fixtures/until-past-max.json"),
            "max-until-past-cap.json" => include_bytes!("fixtures/max-until-past-cap.json"),
            "corrupt.json" => include_bytes!("fixtures/corrupt.json"),
            "schema2.json" => include_bytes!("fixtures/schema2.json"),
            other => panic!("no fixture {other}"),
        }
    }

    /// A fresh mtime: five minutes before the fixed now.
    fn fresh_mtime() -> Option<DateTime<Utc>> {
        Some(fixed_now() - chrono::Duration::minutes(5))
    }

    fn verdict_of(name: &str) -> FileVerdict {
        parse_record(fixture(name), fresh_mtime()).verdict(fixed_now())
    }

    #[test]
    fn fixture_open_runner_restart_is_open_and_fully_read() {
        let FileVerdict::Open(b) = verdict_of("open-runner-restart.json") else {
            panic!("expected open");
        };
        assert_eq!(b.id, "rr-20260929T101500Z-a1b2c3");
        assert_eq!(b.scope, Scope::RunnerRestart);
        assert_eq!(b.purpose.as_deref(), Some("runner rebuild+swap"));
        assert_eq!(
            b.requester_session_id.as_deref(),
            Some("aaaaaaaa-0000-4000-8000-000000000001")
        );
        assert_eq!(b.opened_at, parse_ts("2026-09-29T10:15:00Z").unwrap());
        assert_eq!(b.until, parse_ts("2026-09-29T10:35:00Z").unwrap());
        assert_eq!(b.straggler_policy.as_deref(), Some("wait"));
    }

    #[test]
    fn fixture_open_repo_is_open_with_repo_scope() {
        let FileVerdict::Open(b) = verdict_of("open-repo.json") else {
            panic!("expected open");
        };
        assert_eq!(b.scope, Scope::Repo("qontinui-runner".to_string()));
    }

    #[test]
    fn fixture_open_target_is_open_with_target_scope() {
        let FileVerdict::Open(b) = verdict_of("open-target.json") else {
            panic!("expected open");
        };
        assert_eq!(
            b.scope,
            Scope::Target("/home/fixture/qontinui-runner/target-agent".to_string())
        );
    }

    #[test]
    fn fixture_expired_is_absent() {
        assert_eq!(verdict_of("expired.json"), FileVerdict::Absent);
    }

    #[test]
    fn fixture_released_is_absent() {
        assert_eq!(verdict_of("released.json"), FileVerdict::Absent);
    }

    /// `until` past `max_until` reads as `max_until` (10:15), which is before
    /// the fixed now — so a hand-edited extension past the hard cap is absent.
    #[test]
    fn fixture_until_past_max_is_capped_to_max_until() {
        let FileRecord::Recorded(b) = parse_record(fixture("until-past-max.json"), fresh_mtime())
        else {
            panic!("expected a record");
        };
        assert_eq!(b.until, parse_ts("2026-09-29T10:15:00Z").unwrap());
        assert_eq!(
            FileRecord::Recorded(b).verdict(fixed_now()),
            FileVerdict::Absent
        );
    }

    /// A file whose `max_until` (and `until`) were hand-edited to 2099 is
    /// still held to `opened_at + cap(scope)` (08:00 + 2 h = 10:00): EXPIRED at
    /// the fixed now (10:20), OPEN at 09:00. A reader trusting the file's own
    /// `max_until` would read it open until 2099.
    #[test]
    fn fixture_max_until_past_cap_is_held_to_the_scope_cap() {
        let FileRecord::Recorded(b) =
            parse_record(fixture("max-until-past-cap.json"), fresh_mtime())
        else {
            panic!("expected a record");
        };
        assert_eq!(b.scope, Scope::Repo("qontinui-mcp".to_string()));
        assert_eq!(b.until, parse_ts("2026-09-29T10:00:00Z").unwrap());
        assert_eq!(verdict_of("max-until-past-cap.json"), FileVerdict::Absent);
        let at_nine = parse_ts("2026-09-29T09:00:00Z").unwrap();
        assert!(matches!(
            parse_record(fixture("max-until-past-cap.json"), fresh_mtime()).verdict(at_nine),
            FileVerdict::Open(_)
        ));
    }

    #[test]
    fn fixture_corrupt_is_unknown_while_young_and_ignored_when_old() {
        assert!(matches!(verdict_of("corrupt.json"), FileVerdict::Unknown(_)));
        let old = Some(fixed_now() - chrono::Duration::hours(3));
        assert!(matches!(
            parse_record(fixture("corrupt.json"), old).verdict(fixed_now()),
            FileVerdict::StaleCorrupt(_)
        ));
        // An unreadable mtime cannot be bounded: fail closed.
        assert!(matches!(
            parse_record(fixture("corrupt.json"), None).verdict(fixed_now()),
            FileVerdict::Unknown(_)
        ));
    }

    #[test]
    fn fixture_schema2_is_unknown_not_open() {
        let v = verdict_of("schema2.json");
        assert!(
            matches!(v, FileVerdict::Unknown(ref r) if r.contains("schema 2")),
            "{v:?}"
        );
    }

    #[test]
    fn until_defaults_to_twenty_minutes_and_is_capped_per_scope() {
        let rec = br#"{"schema":1,"id":"rr-x","scope":"runner-restart","opened_at":"2026-09-29T10:00:00Z","state":"open"}"#;
        let FileRecord::Recorded(b) = parse_record(rec, None) else {
            panic!()
        };
        assert_eq!(b.until, parse_ts("2026-09-29T10:20:00Z").unwrap());

        let rec = br#"{"schema":1,"id":"rr-y","scope":"runner-restart","opened_at":"2026-09-29T10:00:00Z","until":"2026-09-29T15:00:00Z","state":"open"}"#;
        let FileRecord::Recorded(b) = parse_record(rec, None) else {
            panic!()
        };
        assert_eq!(b.until, parse_ts("2026-09-29T11:00:00Z").unwrap(), "1 h cap");

        let rec = br#"{"schema":1,"id":"repo-z","scope":"repo:x","opened_at":"2026-09-29T10:00:00Z","until":"2026-09-29T15:00:00Z","state":"open"}"#;
        let FileRecord::Recorded(b) = parse_record(rec, None) else {
            panic!()
        };
        assert_eq!(b.until, parse_ts("2026-09-29T12:00:00Z").unwrap(), "2 h cap");
    }

    /// Mirrors the reference reader: a scope outside the grammar is kept (it
    /// holds nothing), while a bad state, a non-`Z` or unparseable timestamp,
    /// a `null` deadline and a non-object document are corrupt.
    #[test]
    fn corrupt_versus_merely_unknown_scope_matches_the_reference() {
        for rec in [
            &br#"{"schema":1,"id":"a","scope":"machine","opened_at":"2026-09-29T10:00:00Z","state":"open"}"#[..],
            &br#"{"schema":1,"id":"a","scope":"repo:","opened_at":"2026-09-29T10:00:00Z","state":"open"}"#[..],
        ] {
            let FileRecord::Recorded(b) = parse_record(rec, None) else {
                panic!("{}", String::from_utf8_lossy(rec));
            };
            assert!(matches!(b.scope, Scope::Other(_)), "{:?}", b.scope);
        }
        for rec in [
            &br#"{"schema":1,"id":"a","scope":"runner-restart","opened_at":"2026-09-29T10:00:00Z","state":"paused"}"#[..],
            &br#"{"schema":1,"id":"a","scope":"runner-restart","opened_at":"yesterday","state":"open"}"#[..],
            &br#"{"schema":1,"id":"a","scope":"runner-restart","opened_at":"2026-09-29T10:00:00+00:00","state":"open"}"#[..],
            &br#"{"schema":1,"id":"a","scope":"runner-restart","opened_at":"2026-09-29T10:00:00Z","until":null,"state":"open"}"#[..],
            &br#"{"schema":1,"id":7,"scope":"runner-restart","opened_at":"2026-09-29T10:00:00Z","state":"open"}"#[..],
            &br#"{"schema":1,"id":"a","scope":"runner-restart","opened_at":"2026-09-29T10:00:00Z","state":"released","max_until":"soon"}"#[..],
            &br#"[1,2]"#[..],
            &br#"{"schema":"1","id":"a","scope":"runner-restart","opened_at":"2026-09-29T10:00:00Z","state":"open"}"#[..],
        ] {
            assert!(
                matches!(parse_record(rec, None), FileRecord::Corrupt { .. }),
                "{}",
                String::from_utf8_lossy(rec)
            );
        }
        // Python `==`: `1.0` equals schema 1 in the reference, so it is not corrupt here either.
        assert!(matches!(
            parse_record(
                br#"{"schema":1.0,"id":"a","scope":"runner-restart","opened_at":"2026-09-29T10:00:00Z","state":"open"}"#,
                None
            ),
            FileRecord::Recorded(_)
        ));
        // Fractional seconds are accepted, as by the reference `TS_RE`.
        assert!(parse_ts("2026-09-29T10:00:00.250Z").is_some());
    }

    fn write_fixture(dir: &Path, name: &str) {
        std::fs::write(dir.join(name), fixture(name)).unwrap();
    }

    #[test]
    fn scan_of_the_whole_fixture_set_reads_open_runner_restart() {
        let tmp = tempfile::tempdir().unwrap();
        let barriers = tmp.path().join("barriers");
        std::fs::create_dir_all(&barriers).unwrap();
        for name in [
            "open-runner-restart.json",
            "open-repo.json",
            "open-target.json",
            "expired.json",
            "released.json",
            "until-past-max.json",
            "max-until-past-cap.json",
        ] {
            write_fixture(&barriers, name);
        }
        // Staging files from the writer's atomic rename are not barriers.
        std::fs::write(barriers.join(".rr-tmp.json"), b"{not json").unwrap();
        std::fs::write(barriers.join("rr.json.tmp"), b"{not json").unwrap();
        let scan = scan_dir(tmp.path());
        assert_eq!(scan.files.len(), 7);
        let BarrierState::Open(b) = scan.runner_restart_state(fixed_now()) else {
            panic!("expected open");
        };
        assert_eq!(b.id, "rr-20260929T101500Z-a1b2c3");
    }

    #[test]
    fn scan_with_only_a_fresh_corrupt_file_is_unknown_and_holds_wakes() {
        let tmp = tempfile::tempdir().unwrap();
        let barriers = tmp.path().join("barriers");
        std::fs::create_dir_all(&barriers).unwrap();
        write_fixture(&barriers, "corrupt.json");
        write_fixture(&barriers, "open-repo.json");
        // The file was just written, so its mtime is "now" in wall-clock
        // terms: judge it at the real clock.
        let state = scan_dir(tmp.path()).runner_restart_state(Utc::now());
        assert!(matches!(state, BarrierState::Unknown(_)), "{state:?}");
        assert!(state.holds_wakes());
        assert_eq!(state.barrier_id(), Some(UNKNOWN_BARRIER_ID));
    }

    /// Note 7: an open runner-restart barrier BESIDE a fresh corrupt file is
    /// `Unknown` (the reference reader and the contract hold everyone on the
    /// corrupt file) — so even the open barrier's own requester is held.
    #[test]
    fn an_open_barrier_beside_a_fresh_corrupt_file_is_unknown_for_everyone() {
        let tmp = tempfile::tempdir().unwrap();
        let barriers = tmp.path().join("barriers");
        std::fs::create_dir_all(&barriers).unwrap();
        write_fixture(&barriers, "open-runner-restart.json");
        write_fixture(&barriers, "corrupt.json");
        // Judged inside the open barrier's window AND inside the corrupt
        // file's freshness window: reparse the open fixture at a "now" just
        // after it opened, with the corrupt file judged as just written.
        let scan = scan_dir(tmp.path());
        let open_now = parse_ts("2026-09-29T10:20:00Z").unwrap();
        let mut files = scan.files.clone();
        for (_, rec) in files.iter_mut() {
            if let FileRecord::Corrupt { mtime, .. } = rec {
                *mtime = Some(open_now - chrono::Duration::minutes(1));
            }
        }
        let scan = Scan { files, dir_error: None };
        let state = scan.runner_restart_state(open_now);
        let BarrierState::Unknown(why) = &state else {
            panic!("expected unknown, got {state:?}");
        };
        assert!(why.contains("rr-20260929T101500Z-a1b2c3"), "{why}");
        // The requester named in the open fixture is NOT exempt now.
        assert!(matches!(
            wake_decision(
                WakeClass::Autonomous,
                &state,
                &["aaaaaaaa-0000-4000-8000-000000000001".to_string()],
            ),
            WakeDecision::Defer { .. }
        ));
    }

    /// Note 13: `with_test_state` restores the previous answer even when the
    /// closure panics.
    #[test]
    fn with_test_state_restores_on_panic() {
        let caught = std::panic::catch_unwind(|| {
            with_test_state(BarrierState::Unknown("x".into()), || panic!("boom"))
        });
        assert!(caught.is_err());
        assert_eq!(runner_restart_barrier_open(), BarrierState::Absent);
    }

    /// The SDK door table, spelled out literally per variant.
    #[test]
    fn sdk_message_caller_matches_the_d4_table() {
        use SdkMessageCaller::*;
        for (caller, class) in [
            (TauriAiSessionMessage, WakeClass::Operator),
            (BackendChatRelay, WakeClass::Operator),
            (ExecutorFirstMessage, WakeClass::SessionInternal),
            (HttpSessionMessage, WakeClass::Autonomous),
            (HttpTaskRunMessage, WakeClass::Operator),
            (ConductorReprompt, WakeClass::Autonomous),
            (SessionMessagePoller, WakeClass::Autonomous),
            (Test, WakeClass::Test),
        ] {
            assert_eq!(caller.wake_class(), class, "{caller:?}");
        }
    }

    fn open_rr_with_requester(requester: &str) -> BarrierState {
        let FileRecord::Recorded(mut b) = parse_record(fixture("open-runner-restart.json"), None)
        else {
            panic!("fixture must parse");
        };
        b.requester_session_id = Some(requester.to_string());
        BarrierState::Open(b)
    }

    /// Every autonomous SDK door is deferred under a barrier with a typed,
    /// recognisable refusal; operator doors pass; the requester is exempt by
    /// EITHER of its ids (runner session id or pinned Claude session id —
    /// note 9); and every deferral maps to 409 at the HTTP doors.
    #[test]
    fn sdk_wakes_are_deferred_by_door_and_the_requester_is_exempt_by_either_id() {
        use SdkMessageCaller::*;
        let open = open_rr_with_requester("claude-pinned-req");
        for caller in [HttpSessionMessage, ConductorReprompt, SessionMessagePoller] {
            let err = sdk_wake_refusal(caller, &open, "task-run-1", &["task-run-1".to_string()])
                .unwrap_or_else(|| panic!("{caller:?} must be deferred"));
            assert_eq!(deferred_barrier_id(&err), Some("rr-20260929T101500Z-a1b2c3"));
            assert!(err.contains(caller.tag()), "{err}");
            let (status, body) = http_conflict(&err).expect("409");
            assert_eq!(status, axum::http::StatusCode::CONFLICT);
            assert!(body.starts_with(QUIET_BARRIER_DEFERRED), "{body}");
            // Released: it goes out.
            assert_eq!(
                sdk_wake_refusal(caller, &BarrierState::Absent, "task-run-1", &[]),
                None
            );
            // The requester, by its pinned Claude id beside the runner id.
            assert_eq!(
                sdk_wake_refusal(
                    caller,
                    &open,
                    "task-run-1",
                    &["task-run-1".to_string(), "claude-pinned-req".to_string()]
                ),
                None
            );
        }
        for caller in [
            TauriAiSessionMessage,
            BackendChatRelay,
            HttpTaskRunMessage,
            ExecutorFirstMessage,
        ] {
            assert_eq!(sdk_wake_refusal(caller, &open, "t", &[]), None, "{caller:?}");
            assert_eq!(
                with_test_state(open.clone(), || sdk_wake_gate(caller, "t", &[])),
                Ok(())
            );
        }
        assert!(with_test_state(open, || sdk_wake_gate(
            ConductorReprompt,
            "t",
            &[]
        ))
        .is_err());
        assert!(http_conflict("TERMINAL_EXITED: gone").is_none());
    }

    #[test]
    fn wrap_unless_deferred_keeps_a_deferral_verbatim() {
        let d = deferred_error("rr-1", "conductor_reprompt", "t", "x");
        assert_eq!(wrap_unless_deferred(d.clone(), |e| format!("ctx: {e}")), d);
        assert_eq!(
            wrap_unless_deferred("boom".into(), |e| format!("ctx: {e}")),
            "ctx: boom"
        );
    }

    #[test]
    fn autonomous_wakes_deferred_under_reads_the_gate() {
        assert!(autonomous_wakes_deferred_under(&open_rr_with_requester("r")));
        assert!(autonomous_wakes_deferred_under(&BarrierState::Unknown("x".into())));
        assert!(!autonomous_wakes_deferred_under(&BarrierState::Absent));
    }

    #[test]
    fn pending_registry_marks_clears_and_guards() {
        let t = "term-pending-registry-test";
        assert!(pending::for_session(t).is_empty());
        pending::mark(t, "auto_response:r1", "scheduled");
        assert!(pending::contains(t, "auto_response:r1"));
        {
            let _g = pending::guard(t, "account_migration:nudge", "watching for idle");
            assert_eq!(pending::for_session(t).len(), 2);
        }
        assert_eq!(
            pending::for_session(t),
            vec!["auto_response:r1 (scheduled)".to_string()]
        );
        pending::clear(t, "auto_response:r1");
        assert!(pending::for_session(t).is_empty());
    }

    #[test]
    fn repo_and_target_barriers_do_not_hold_runner_wakes() {
        let tmp = tempfile::tempdir().unwrap();
        let barriers = tmp.path().join("barriers");
        std::fs::create_dir_all(&barriers).unwrap();
        write_fixture(&barriers, "open-repo.json");
        write_fixture(&barriers, "open-target.json");
        assert_eq!(
            scan_dir(tmp.path()).runner_restart_state(fixed_now()),
            BarrierState::Absent
        );
    }

    #[test]
    fn missing_store_is_absent() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(scan_dir(tmp.path()), Scan::default());
        assert_eq!(
            BarrierReader::new(None, SCAN_CACHE_TTL).runner_restart_state(fixed_now()),
            BarrierState::Absent
        );
    }

    /// The reader caches the scan, but judges expiry against the clock it is
    /// handed — so a cached open barrier stops holding the instant `until`
    /// passes, and a release is seen once the TTL lapses.
    #[test]
    fn reader_caches_the_scan_but_judges_expiry_live() {
        let tmp = tempfile::tempdir().unwrap();
        let barriers = tmp.path().join("barriers");
        std::fs::create_dir_all(&barriers).unwrap();
        write_fixture(&barriers, "open-runner-restart.json");
        let reader = BarrierReader::new(Some(tmp.path().to_path_buf()), Duration::from_secs(3600));
        assert!(matches!(
            reader.runner_restart_state(fixed_now()),
            BarrierState::Open(_)
        ));
        // Released on disk, but the long TTL keeps the cached scan…
        std::fs::remove_file(barriers.join("open-runner-restart.json")).unwrap();
        assert!(matches!(
            reader.runner_restart_state(fixed_now()),
            BarrierState::Open(_)
        ));
        // …while expiry is still judged live.
        assert_eq!(
            reader.runner_restart_state(parse_ts("2026-09-29T10:35:00Z").unwrap()),
            BarrierState::Absent
        );
        // A zero-TTL reader sees the release immediately.
        let fresh = BarrierReader::new(Some(tmp.path().to_path_buf()), Duration::ZERO);
        assert_eq!(fresh.runner_restart_state(fixed_now()), BarrierState::Absent);
    }

    fn open_barrier(requester: Option<&str>) -> BarrierState {
        let FileRecord::Recorded(mut b) =
            parse_record(fixture("open-runner-restart.json"), None)
        else {
            panic!()
        };
        b.requester_session_id = requester.map(str::to_string);
        BarrierState::Open(b)
    }

    #[test]
    fn autonomous_is_deferred_under_open_and_unknown_and_allowed_when_absent() {
        let open = open_barrier(None);
        assert_eq!(
            wake_decision(WakeClass::Autonomous, &open, &[]),
            WakeDecision::Defer {
                barrier_id: "rr-20260929T101500Z-a1b2c3".to_string(),
                reason: "runner-restart barrier rr-20260929T101500Z-a1b2c3 is open until \
                         2026-09-29T10:35:00Z (runner rebuild+swap)"
                    .to_string(),
            }
        );
        assert!(matches!(
            wake_decision(WakeClass::Autonomous, &BarrierState::Unknown("x".into()), &[]),
            WakeDecision::Defer { ref barrier_id, .. } if barrier_id == UNKNOWN_BARRIER_ID
        ));
        assert_eq!(
            wake_decision(WakeClass::Autonomous, &BarrierState::Absent, &[]),
            WakeDecision::Allow
        );
    }

    #[test]
    fn non_autonomous_classes_always_pass() {
        for state in [
            open_barrier(None),
            BarrierState::Unknown("x".into()),
            BarrierState::Absent,
        ] {
            for class in [
                WakeClass::Operator,
                WakeClass::SessionInternal,
                WakeClass::Test,
            ] {
                assert_eq!(wake_decision(class, &state, &[]), WakeDecision::Allow);
            }
        }
    }

    #[test]
    fn the_requester_session_is_exempt_from_its_own_barrier() {
        let sid = "0f6c1c7e-1111-4222-8333-944444444444";
        let open = open_barrier(Some(sid));
        assert_eq!(
            wake_decision(WakeClass::Autonomous, &open, &[sid.to_string()]),
            WakeDecision::Allow
        );
        // Exact match, as the reference reader's `is_exempt` (`==`).
        assert!(matches!(
            wake_decision(WakeClass::Autonomous, &open, &[sid.to_uppercase()]),
            WakeDecision::Defer { .. }
        ));
        assert!(matches!(
            wake_decision(WakeClass::Autonomous, &open, &["someone-else".into()]),
            WakeDecision::Defer { .. }
        ));
    }

    #[test]
    fn deferred_error_round_trips_its_barrier_id() {
        let e = deferred_error("rr-1", "http_write", "term-1", "because");
        assert!(e.starts_with(QUIET_BARRIER_DEFERRED));
        assert_eq!(deferred_barrier_id(&e), Some("rr-1"));
        assert_eq!(deferred_barrier_id("TERMINAL_EXITED: x"), None);
        assert_eq!(deferred_barrier_id("QUIET_BARRIER_DEFERRED: nope"), None);
    }

    #[test]
    fn test_seam_scopes_the_state_to_the_closure() {
        assert_eq!(runner_restart_barrier_open(), BarrierState::Absent);
        with_test_state(open_barrier(None), || {
            assert_eq!(
                autonomous_wakes_held().as_deref(),
                Some("rr-20260929T101500Z-a1b2c3")
            );
        });
        assert_eq!(autonomous_wakes_held(), None);
    }
}
