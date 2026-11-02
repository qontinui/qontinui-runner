//! Session-identity reporter — the runner half of plan
//! `2026-10-19-session-lookup-by-name-shows-claude-account-in-web`, Phase 2.
//!
//! ## What it answers
//!
//! "Which Claude account did session `spaceship-latency` run on?" Coord's live
//! census only knows sessions whose process is running, and Claude Code deletes
//! its registry file when the process exits — so for an ENDED session the only
//! durable account fact is which account root still holds its transcript, and
//! only this machine can see that. This reporter tells coord, which stamps it
//! onto `coord.agent_sessions` (Phase 1's `POST /coord/session-identity/:device_id`).
//!
//! ## What it reports
//!
//! On the runner's session-lifecycle tick ([`on_lifecycle_tick`], paced to
//! [`REPORT_INTERVAL`]) the PRIMARY runner posts one row per session id this
//! machine can resolve:
//!
//! - every id [`SessionDirectory`] indexes with a transcript in some account
//!   root — `live: false`;
//! - every live Claude Code registry row
//!   ([`super::claude_session_registry::read_live_sessions`], read one account
//!   home at a time so each row keeps its config dir) — `live: true`. A live id
//!   is reported once, as live.
//!
//! | field | source |
//! |---|---|
//! | `account` | config-dir basename minus `.claude-`; `default` for the bare `.claude` home ([`account_label`]) — the census label, deliberately NOT a roster id |
//! | `config_dir` | the account root as discovered (live: the registry file's home; ended: the root whose copy holds the newest turn) |
//! | `registry_name` | the live registry `name`, only when its `nameSource` is not `derived` |
//! | `tag_name` | `~/.qontinui/session-names/<id>` (`SessionIdentity.name`, the `Session-Name` trailer source) |
//! | `last_acted_at` | the newest `user`/`assistant` line's own `timestamp` in that root's copy ([`super::transcript_last_turn`]); a live row with no turn line yet falls back to the registry `updatedAt` |
//!
//! A migrated session (its transcript copied into another account root and
//! resumed there) is reported under the root holding its NEWEST turn, with that
//! turn's time, so an older root reported later can never displace it — coord
//! keeps the stamp with the newest `account_observed_at`. When both copies hold
//! the same newest turn (the resumed session exited before speaking), the most
//! recently WRITTEN copy — the migration target — wins, because coord accepts
//! an equal observation time and a name-ordered tie would re-stamp the session
//! back onto its old account.
//!
//! ## What it never carries
//!
//! No transcript content: only the tail is read, and only to find the newest
//! turn's timestamp. Names and account labels only. A name longer than
//! [`MAX_NAME_CHARS`] is omitted from the row, never truncated.
//!
//! ## Protocol
//!
//! - **Bounded window.** At most [`MAX_ROWS_PER_POST`] rows per post — coord
//!   answers `422 too_many_rows` above that and never truncates. The runner has
//!   no read telling it which ids coord lacks, so it rotates: rows are sent
//!   oldest-reported first (never-reported first of all, most recently acted
//!   first among those).
//! - **Watermark.** A local per-id record of the facts last ACCEPTED by coord
//!   (`config_dir`, `tag_name`, `last_acted_at`, and `registry_name` when one
//!   is known). A row whose facts are unchanged is not re-sent; a changed fact
//!   is. An absent `registry_name` (the session ended) does not count as a
//!   change — it means "no live registry fact", not "the name was removed".
//!   Persisted beside the session ledger
//!   ([`watermark_path`]), at most every [`PERSIST_EVERY`] or after a full
//!   window of first reports: losing the tail of it only re-sends rows coord
//!   upserts idempotently.
//! - **Responses.** `2xx` advances the watermark. `503
//!   session_identity_unavailable` (the columns are not deployed) is retried
//!   next pass and NEVER marks a row reported. `404` means the door itself is
//!   not deployed yet: nothing is marked, the reporter backs off
//!   ([`NOT_DEPLOYED_BACKOFF`], doubling to [`BACKOFF_MAX`]), and the state is
//!   logged once, not per pass. `403` (an unpaired device or a path/token
//!   device mismatch) backs off the same way. A `422` halves the window; a
//!   one-row window that still answers `422` quarantines that row in memory
//!   until its facts change, so one malformed row can never stall the rest.
//! - **Tail-read budget.** At most [`TAIL_READS_PER_PASS`] uncached transcript
//!   tails are read per pass; a row whose newest turn is not read yet is
//!   deferred to a later pass, never sent with a guessed time.
//!
//! ## Limits
//!
//! The account roots are whatever
//! [`crate::terminal::transcript::find_claude_config_dirs`] discovers. On
//! POSIX that is `CLAUDE_CONFIG_DIR`, the runner's configured roster and
//! `~/.claude` — not an unlisted `~/.claude-*` home, whose scan is a separate
//! change (qontinui-runner#1904's account-home discovery). Windows boxes see
//! every `C:/claude/.claude-*` root.
//!
//! ## Failure posture
//!
//! Best-effort throughout: the filesystem work runs on the blocking pool, an
//! indeterminate process snapshot skips the pass (a live session would
//! otherwise be reported as ended), and every failure is logged once per state
//! change.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use super::claude_session_registry::{read_live_sessions, LiveClaudeSession};
use super::transcript_last_turn::TurnRead;
use crate::agent_worktree::custody::{SessionDirectory, TranscriptCopy};

/// How often a pass runs. The lifecycle tick that drives it is 45 s; identity
/// facts are durable (liveness is the census's job), so a pass every few ticks
/// is enough, and it bounds the extra process snapshot each pass takes.
pub const REPORT_INTERVAL: Duration = Duration::from_secs(180);

/// coord's per-post row ceiling (`422 too_many_rows` above it).
pub const MAX_ROWS_PER_POST: usize = 500;

/// Uncached transcript tails one pass may read. A box holds thousands of
/// transcripts (8,432 session ids across 17 account homes on one Linux fleet
/// box, counted 2026-11-02); after a runner start the cache is cold, so the
/// first passes spread the reads instead of tailing every file at once.
pub const TAIL_READS_PER_PASS: usize = 1_000;

/// coord refuses a name longer than this; such a name is omitted from the row.
pub const MAX_NAME_CHARS: usize = 1024;

/// First back-off after a `404` (the door is not deployed) or a `403`.
pub const NOT_DEPLOYED_BACKOFF: Duration = Duration::from_secs(15 * 60);

/// Ceiling for the doubling back-off.
pub const BACKOFF_MAX: Duration = Duration::from_secs(60 * 60);

/// The longest a dirty watermark waits before it is written.
pub const PERSIST_EVERY: Duration = Duration::from_secs(30 * 60);

const POST_TIMEOUT: Duration = Duration::from_secs(30);

const WATERMARK_VERSION: u32 = 1;

const WATERMARK_FILE: &str = "session-identity-watermark.json";

// ===========================================================================
// Rows
// ===========================================================================

/// One session's identity facts. Field names are coord's ingest contract
/// (`POST /coord/session-identity/:device_id`) — do not rename. Absent values
/// serialize as explicit `null`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IdentityRow {
    pub session_id: String,
    pub live: bool,
    pub account: String,
    pub config_dir: String,
    pub registry_name: Option<String>,
    pub tag_name: Option<String>,
    pub last_acted_at: Option<String>,
}

impl IdentityRow {
    fn facts(&self) -> ReportedFacts {
        ReportedFacts {
            config_dir: self.config_dir.clone(),
            tag_name: self.tag_name.clone(),
            last_acted_at: self.last_acted_at.clone(),
            registry_name: self.registry_name.clone(),
        }
    }
}

/// The post body.
#[derive(Debug, Serialize)]
pub struct IdentityPost<'a> {
    pub observed_at: String,
    pub rows: &'a [IdentityRow],
}

/// The census contract's account label for a config dir: the basename minus a
/// leading `.claude-` (`.claude-qontinui` → `qontinui`), `default` for the
/// bare `.claude` home, and the bare basename for anything else (a
/// `CLAUDE_CONFIG_DIR` pinned somewhere unconventional).
///
/// Deliberately NOT [`super::past_sessions::account_from_config_dir`], which
/// labels the default home `unknown` because the session-repository identity
/// key depends on it; the census and this stamp name it `default`. Never a
/// roster id: the roster maps the bare home differently per OS.
pub fn account_label(config_dir: &Path) -> Option<String> {
    let base = config_dir.file_name()?.to_str()?;
    if base == ".claude" {
        return Some("default".to_string());
    }
    let label = base
        .strip_prefix(".claude-")
        .filter(|s| !s.is_empty())
        .unwrap_or(base);
    (!label.trim().is_empty()).then(|| label.to_string())
}

/// A hyphenated 36-character UUID — the only session-id shape coord accepts.
pub fn is_uuid_shaped(s: &str) -> bool {
    s.len() == 36 && uuid::Uuid::try_parse(s).is_ok()
}

/// Unix ms → RFC 3339 with millisecond precision and a `Z` suffix; `None` for
/// a non-positive or out-of-range value. One fixed format, so the watermark's
/// string comparison is a comparison of instants.
fn ms_to_rfc3339(ms: i64) -> Option<String> {
    if ms <= 0 {
        return None;
    }
    DateTime::<Utc>::from_timestamp_millis(ms)
        .map(|t| t.to_rfc3339_opts(SecondsFormat::Millis, true))
}

/// A name coord will accept, or `None`. Over-long names are omitted, never cut:
/// a truncated name would be a different, wrong name.
fn bounded_name(raw: &str) -> Option<String> {
    let t = raw.trim();
    (!t.is_empty() && t.chars().count() <= MAX_NAME_CHARS).then(|| t.to_string())
}

/// A registry name the operator chose (a `/rename` or a launch-time name), as
/// opposed to Claude Code's own `nameSource: "derived"` auto-name.
fn operator_named(s: &LiveClaudeSession) -> bool {
    !s.name.trim().is_empty() && s.name_source.as_deref() != Some("derived")
}

/// One live registry row with the account home its file was read from.
#[derive(Debug, Clone)]
pub struct LiveRow {
    pub config_dir: PathBuf,
    pub session: LiveClaudeSession,
}

/// Read every live registry row, one account home at a time so each row keeps
/// the config dir it came from (`read_live_sessions` reports the account label
/// but not the path).
pub fn read_live_rows(homes: &[PathBuf], live_pids: &HashSet<u32>) -> Vec<LiveRow> {
    homes
        .iter()
        .flat_map(|home| {
            read_live_sessions(std::slice::from_ref(home), live_pids)
                .into_iter()
                .map(move |session| LiveRow {
                    config_dir: home.clone(),
                    session,
                })
        })
        .collect()
}

/// What one build produced.
#[derive(Debug, Default)]
pub struct Candidates {
    /// One row per reportable id, sorted by `session_id`.
    pub rows: Vec<IdentityRow>,
    /// Ids whose newest turn was not read this pass (budget spent) — retried
    /// next pass, never sent with a guessed time.
    pub deferred: usize,
    /// Every id the index or the registry knew this pass, reportable or not —
    /// the set the watermark is pruned to.
    pub known_ids: HashSet<String>,
    /// How many account roots the directory walk actually read. A pass that
    /// read fewer roots than an earlier one saw LESS, not fewer sessions, so
    /// it must not prune ([`Reporter::prune_watermark`]).
    pub roots_scanned: usize,
}

/// Build every reportable row. `read` is the newest-turn probe
/// ([`super::transcript_last_turn::last_turn_ms`] with a budget in
/// production); injected so the build is a pure function of the index.
///
/// An id yields no row when it has no account root this machine can name: a
/// ghost (no name, no transcript), or a name with no transcript anywhere.
pub fn build_candidates(
    dir: &SessionDirectory,
    live: &[LiveRow],
    read: &mut dyn FnMut(&TranscriptCopy) -> TurnRead,
) -> Candidates {
    // Live rows by id. Several processes may report one id; the representative
    // is an operator-named row before a derived one, then the lowest config
    // dir and pid, so the choice is stable between passes.
    let mut live_by_id: HashMap<String, &LiveRow> = HashMap::new();
    for row in live {
        let id = row.session.session_id.trim().to_ascii_lowercase();
        if !is_uuid_shaped(&id) {
            continue;
        }
        let key = |r: &LiveRow| {
            (
                !operator_named(&r.session),
                r.config_dir.clone(),
                r.session.pid,
            )
        };
        let better = match live_by_id.get(&id).copied() {
            None => true,
            Some(cur) => key(row) < key(cur),
        };
        if better {
            live_by_id.insert(id, row);
        }
    }

    let ids: BTreeSet<String> = dir
        .transcribed_ids()
        .into_iter()
        .map(str::to_string)
        .chain(live_by_id.keys().cloned())
        .filter(|id| is_uuid_shaped(id))
        .collect();

    let mut out = Candidates {
        roots_scanned: dir.roots_scanned(),
        ..Candidates::default()
    };
    for id in ids {
        out.known_ids.insert(id.clone());
        let tag_name = dir.name_of(&id).and_then(bounded_name);
        let copies = dir.copies_of(&id);

        let (config_dir, live, registry_name, last_acted_ms) = if let Some(lr) = live_by_id.get(&id)
        {
            // The live process's own home: its transcript copy there is the
            // one it is writing.
            let mut newest: Option<i64> = None;
            let mut deferred = false;
            for c in copies.iter().filter(|c| c.config_dir == lr.config_dir) {
                match read(c) {
                    TurnRead::Read(Some(ms)) => newest = newest.max(Some(ms)),
                    TurnRead::Read(None) | TurnRead::Unreadable => {}
                    TurnRead::Deferred => deferred = true,
                }
            }
            if deferred {
                out.deferred += 1;
                continue;
            }
            // No turn line yet (a session that has not spoken, or whose
            // transcript the cached index has not seen): the registry's own
            // update time is the best observation of it.
            let fallback = [lr.session.updated_at, lr.session.started_at]
                .into_iter()
                .find(|ms| *ms > 0);
            (
                lr.config_dir.clone(),
                true,
                operator_named(&lr.session)
                    .then(|| bounded_name(&lr.session.name))
                    .flatten(),
                newest.or(fallback),
            )
        } else {
            // Ended: the directory already ordered the copies newest-turn
            // first; take the first one that can be read.
            let mut chosen: Option<(&TranscriptCopy, Option<i64>)> = None;
            let mut deferred = false;
            for c in copies {
                match read(c) {
                    TurnRead::Read(t) => {
                        chosen = Some((c, t));
                        break;
                    }
                    TurnRead::Deferred => {
                        deferred = true;
                        break;
                    }
                    TurnRead::Unreadable => {}
                }
            }
            if deferred {
                out.deferred += 1;
                continue;
            }
            let Some((copy, turn)) = chosen else {
                continue; // no readable copy this pass
            };
            (copy.config_dir.clone(), false, None, turn)
        };

        let Some(account) = account_label(&config_dir) else {
            continue;
        };
        out.rows.push(IdentityRow {
            session_id: id,
            live,
            account,
            config_dir: config_dir.to_string_lossy().into_owned(),
            registry_name,
            tag_name,
            last_acted_at: last_acted_ms.and_then(ms_to_rfc3339),
        });
    }
    out
}

// ===========================================================================
// The watermark
// ===========================================================================

/// The facts coord last accepted for one id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportedFacts {
    pub config_dir: String,
    pub tag_name: Option<String>,
    pub last_acted_at: Option<String>,
    pub registry_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct WatermarkEntry {
    facts: ReportedFacts,
    reported_at_ms: i64,
}

/// Per-id record of what coord has accepted, and when.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Watermark {
    version: u32,
    entries: BTreeMap<String, WatermarkEntry>,
}

impl Default for Watermark {
    fn default() -> Self {
        Self {
            version: WATERMARK_VERSION,
            entries: BTreeMap::new(),
        }
    }
}

impl Watermark {
    /// Does `row` carry a fact coord has not accepted yet?
    pub fn needs_send(&self, row: &IdentityRow) -> bool {
        let Some(e) = self.entries.get(&row.session_id) else {
            return true;
        };
        e.facts.config_dir != row.config_dir
            || e.facts.tag_name != row.tag_name
            || e.facts.last_acted_at != row.last_acted_at
            || (row.registry_name.is_some() && e.facts.registry_name != row.registry_name)
    }

    /// The next window: rows with an unaccepted fact, oldest-reported first —
    /// never-reported first of all, most recently acted first among those —
    /// at most `window` of them. A row quarantined with these exact facts is
    /// skipped.
    pub fn select(
        &self,
        rows: &[IdentityRow],
        window: usize,
        quarantine: &HashMap<String, ReportedFacts>,
    ) -> Vec<IdentityRow> {
        let mut pending: Vec<(&IdentityRow, Option<i64>)> = rows
            .iter()
            .filter(|r| self.needs_send(r))
            .filter(|r| quarantine.get(&r.session_id) != Some(&r.facts()))
            .map(|r| (r, self.entries.get(&r.session_id).map(|e| e.reported_at_ms)))
            .collect();
        pending.sort_by(|(a, ra), (b, rb)| {
            ra.cmp(rb)
                .then_with(|| b.last_acted_at.cmp(&a.last_acted_at))
                .then_with(|| a.session_id.cmp(&b.session_id))
        });
        pending
            .into_iter()
            .take(window)
            .map(|(r, _)| r.clone())
            .collect()
    }

    /// Record `rows` as accepted at `now_ms`. Returns how many were reported
    /// for the first time. A row without a `registry_name` keeps the one coord
    /// already holds (absent means "not live", not "renamed to nothing").
    pub fn record_sent(&mut self, rows: &[IdentityRow], now_ms: i64) -> usize {
        let mut first = 0;
        for r in rows {
            let mut facts = r.facts();
            match self.entries.get(&r.session_id) {
                Some(prev) => {
                    if facts.registry_name.is_none() {
                        facts.registry_name = prev.facts.registry_name.clone();
                    }
                }
                None => first += 1,
            }
            self.entries.insert(
                r.session_id.clone(),
                WatermarkEntry {
                    facts,
                    reported_at_ms: now_ms,
                },
            );
        }
        first
    }

    /// Drop entries for ids neither the index nor the registry knows any more
    /// (their transcripts were deleted). Returns how many were dropped.
    pub fn retain_ids(&mut self, known: &HashSet<String>) -> usize {
        let before = self.entries.len();
        self.entries.retain(|id, _| known.contains(id));
        before - self.entries.len()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Where the watermark lives: beside the session ledger, instance-scoped, so a
/// temp runner can never read or clobber the primary's.
pub fn watermark_path() -> PathBuf {
    crate::instance::scope_path(&qontinui_runner_lib::ambient::runner_dir_or_cwd())
        .join(WATERMARK_FILE)
}

/// Load a watermark. Absent, unreadable, unparseable or another version is an
/// EMPTY watermark: every row is re-sent once, which coord upserts idempotently.
pub fn load_watermark(path: &Path) -> Watermark {
    std::fs::read(path)
        .ok()
        .and_then(|raw| serde_json::from_slice::<Watermark>(&raw).ok())
        .filter(|w| w.version == WATERMARK_VERSION)
        .unwrap_or_default()
}

/// Write the watermark atomically.
pub fn write_watermark(path: &Path, wm: &Watermark) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec(wm).map_err(std::io::Error::other)?;
    crate::fs_atomic::atomic_write(path, &bytes)
}

// ===========================================================================
// Responses
// ===========================================================================

/// What coord's answer to one post means for the reporter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PostOutcome {
    /// `2xx` — the rows are stamped (or deliberately skipped by coord).
    Accepted,
    /// `503 session_identity_unavailable` — the stamp columns are not deployed.
    Unavailable,
    /// `404` — the door itself is not deployed yet.
    NotDeployed,
    /// `422` — the body was refused (`too_many_rows`, or a row coord rejects).
    Rejected {
        code: Option<String>,
    },
    /// `401` — the credential was not accepted; the refresher may fix it.
    Unauthorized,
    /// `403` — an unpaired device, or a path device that is not the token's.
    Refused {
        code: Option<String>,
    },
    Http(u16),
    Transport,
}

/// The coord error code in a refusal body (`{"error": "..."}` or
/// `{"code": "..."}`), bounded, for the log line only.
fn error_code(body: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    let code = v
        .get("error")
        .and_then(|e| e.as_str())
        .or_else(|| v.get("code").and_then(|c| c.as_str()))?;
    let code: String = code.chars().take(120).collect();
    (!code.is_empty()).then_some(code)
}

/// Classify a coord answer.
pub fn classify(status: u16, body: &str) -> PostOutcome {
    match status {
        200..=299 => PostOutcome::Accepted,
        401 => PostOutcome::Unauthorized,
        403 => PostOutcome::Refused {
            code: error_code(body),
        },
        404 => PostOutcome::NotDeployed,
        422 => PostOutcome::Rejected {
            code: error_code(body),
        },
        503 => PostOutcome::Unavailable,
        s => PostOutcome::Http(s),
    }
}

// ===========================================================================
// The reporter
// ===========================================================================

/// What one pass came to — the edge the once-per-state-change log fires on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PassState {
    Posted,
    /// Nothing changed since coord last accepted every row.
    Idle,
    NoDeviceId,
    /// The runner is not connected to a coord (standalone).
    NoCoord,
    Unpaired,
    /// The process snapshot was indeterminate.
    Snapshot,
    /// The blocking build task failed.
    Build,
    Unavailable,
    NotDeployed,
    Refused(Option<String>),
    Rejected(Option<String>),
    Unauthorized,
    Http(u16),
    Transport,
}

/// The reporter's state across passes.
#[derive(Debug)]
pub struct Reporter {
    watermark: Option<Watermark>,
    window: usize,
    quarantine: HashMap<String, ReportedFacts>,
    next_due: Option<Instant>,
    backoff: Option<Duration>,
    last_state: Option<PassState>,
    dirty: bool,
    first_reports_since_persist: usize,
    last_persist: Option<Instant>,
    /// The most account roots any pass in this process has read — the bar a
    /// pass must meet before its absence of an id counts as the id being gone.
    roots_high_water: usize,
}

impl Default for Reporter {
    fn default() -> Self {
        Self {
            watermark: None,
            window: MAX_ROWS_PER_POST,
            quarantine: HashMap::new(),
            next_due: None,
            backoff: None,
            last_state: None,
            dirty: false,
            first_reports_since_persist: 0,
            last_persist: None,
            roots_high_water: 0,
        }
    }
}

impl Reporter {
    fn is_due(&self, now: Instant) -> bool {
        self.next_due.is_none_or(|due| now >= due)
    }

    /// Fold one post's outcome into the state. The watermark advances ONLY on
    /// `Accepted`; every other answer leaves the rows to be sent again.
    fn apply(
        &mut self,
        outcome: PostOutcome,
        batch: &[IdentityRow],
        now: Instant,
        now_ms: i64,
    ) -> PassState {
        match outcome {
            PostOutcome::Accepted => {
                let wm = self.watermark.get_or_insert_with(Watermark::default);
                self.first_reports_since_persist += wm.record_sent(batch, now_ms);
                self.dirty = true;
                self.window = (self.window.saturating_mul(2)).min(MAX_ROWS_PER_POST);
                self.backoff = None;
                for r in batch {
                    self.quarantine.remove(&r.session_id);
                }
                PassState::Posted
            }
            PostOutcome::Unavailable => PassState::Unavailable,
            PostOutcome::NotDeployed => {
                self.back_off(now);
                PassState::NotDeployed
            }
            PostOutcome::Refused { code } => {
                self.back_off(now);
                PassState::Refused(code)
            }
            PostOutcome::Rejected { code } => {
                if batch.len() > 1 {
                    self.window = (batch.len() / 2).max(1);
                } else if let Some(r) = batch.first() {
                    warn!(
                        session_id = %r.session_id,
                        code = code.as_deref().unwrap_or("none"),
                        "session identity: coord refused this one row — held back until its facts change"
                    );
                    self.quarantine.insert(r.session_id.clone(), r.facts());
                }
                PassState::Rejected(code)
            }
            PostOutcome::Unauthorized => PassState::Unauthorized,
            PostOutcome::Http(s) => PassState::Http(s),
            PostOutcome::Transport => PassState::Transport,
        }
    }

    /// Drop watermark entries for ids this pass no longer saw — but ONLY when
    /// the walk read at least as many account roots as any earlier pass in
    /// this process. An empty or partial read (a root unreadable this pass, an
    /// unmounted home) is a smaller view, not proof the sessions are gone;
    /// pruning on it would re-send every row of that root once it reappears.
    /// The bar is in memory, so a root removed for good stops pruning only
    /// until the next runner start. Returns how many entries were dropped.
    fn prune_watermark(&mut self, known: &HashSet<String>, roots_scanned: usize) -> usize {
        if roots_scanned == 0 || roots_scanned < self.roots_high_water {
            return 0;
        }
        self.roots_high_water = roots_scanned;
        let dropped = self
            .watermark
            .get_or_insert_with(Watermark::default)
            .retain_ids(known);
        if dropped > 0 {
            self.dirty = true;
        }
        dropped
    }

    fn back_off(&mut self, now: Instant) {
        let next = match self.backoff {
            None => NOT_DEPLOYED_BACKOFF,
            Some(b) => (b.saturating_mul(2)).min(BACKOFF_MAX),
        };
        self.backoff = Some(next);
        self.next_due = Some(now + next);
    }

    fn should_persist(&self, now: Instant) -> bool {
        self.dirty
            && (self
                .last_persist
                .is_none_or(|at| now.duration_since(at) >= PERSIST_EVERY)
                || self.first_reports_since_persist >= MAX_ROWS_PER_POST)
    }

    async fn maybe_persist(&mut self) {
        let now = Instant::now();
        if !self.should_persist(now) {
            return;
        }
        let Some(snapshot) = self.watermark.clone() else {
            return;
        };
        let written = qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked(move || {
            write_watermark(&watermark_path(), &snapshot)
        })
        .await;
        match written {
            Ok(Ok(())) => {
                self.dirty = false;
                self.first_reports_since_persist = 0;
                self.last_persist = Some(now);
            }
            Ok(Err(e)) => {
                debug!(error = %e, "session identity: watermark write failed — kept in memory")
            }
            Err(e) => debug!(error = %e, "session identity: watermark write task failed"),
        }
    }

    async fn run_pass(&mut self) {
        let state = self.pass().await;
        if self.last_state.as_ref() != Some(&state) {
            log_transition(&state);
            self.last_state = Some(state);
        }
    }

    async fn pass(&mut self) -> PassState {
        // The default pace; a back-off below pushes it further out.
        self.next_due = Some(Instant::now() + REPORT_INTERVAL);

        let Some(device_id) = crate::agent_runtime::load_local_device_id() else {
            return PassState::NoDeviceId;
        };
        let Some(base) = qontinui_runner_lib::profiles::connected_coord_base() else {
            return PassState::NoCoord;
        };
        // The PAIRED-DEVICE JWT, resolved once and presented verbatim: the door
        // refuses an agent or bootstrap token and a path device that differs
        // from the token's. No credential ⇒ no post, never anonymous. The row
        // is keyed by device, so the default binding is correct by
        // construction (`TenantScope::Device`).
        let token = qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked(|| {
            crate::auth::device_bearer_scoped(crate::auth::TenantScope::Device)
        })
        .await
        .ok()
        .flatten();
        let Some(token) = token else {
            return PassState::Unpaired;
        };

        let snap = crate::process_capture::process_tree::snapshot_process_table_public().await;
        let live_pids = match super::claude_session_registry::live_pids_from_snapshot(&snap) {
            Ok(p) => p,
            Err(e) => {
                debug!(error = %e, "session identity: process snapshot indeterminate");
                return PassState::Snapshot;
            }
        };

        if self.watermark.is_none() {
            let loaded = qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked(|| {
                load_watermark(&watermark_path())
            })
            .await
            .unwrap_or_default();
            self.watermark = Some(loaded);
        }

        let built = qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked(move || {
            use super::transcript_last_turn::{last_turn_ms, retain_paths, ReadBudget};
            let dir = SessionDirectory::cached();
            let homes = crate::terminal::transcript::find_claude_config_dirs();
            let live = read_live_rows(&homes, &live_pids);
            let mut budget = ReadBudget::Limited(TAIL_READS_PER_PASS);
            let candidates = build_candidates(&dir, &live, &mut |c| {
                last_turn_ms(&c.path, c.stamp, &mut budget)
            });
            retain_paths(&dir.transcript_paths());
            candidates
        })
        .await;
        let Ok(candidates) = built else {
            return PassState::Build;
        };

        self.prune_watermark(&candidates.known_ids, candidates.roots_scanned);
        let wm = self.watermark.get_or_insert_with(Watermark::default);
        let batch = wm.select(&candidates.rows, self.window, &self.quarantine);
        debug!(
            rows = candidates.rows.len(),
            deferred = candidates.deferred,
            batch = batch.len(),
            "session identity: pass built"
        );
        if batch.is_empty() {
            self.maybe_persist().await;
            return PassState::Idle;
        }

        let outcome = post(&base, device_id, &token, &batch).await;
        let state = self.apply(
            outcome,
            &batch,
            Instant::now(),
            Utc::now().timestamp_millis(),
        );
        self.maybe_persist().await;
        state
    }
}

/// POST one window. Only the coord error code is kept from a refusal body.
async fn post(base: &str, device_id: uuid::Uuid, token: &str, rows: &[IdentityRow]) -> PostOutcome {
    let url = format!(
        "{}/coord/session-identity/{device_id}",
        base.trim_end_matches('/')
    );
    let body = IdentityPost {
        observed_at: Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
        rows,
    };
    let Ok(client) = reqwest::Client::builder().timeout(POST_TIMEOUT).build() else {
        return PostOutcome::Transport;
    };
    // coord-auth-exempt(device-jwt-required): `token` is the paired-device JWT
    // the pass resolved via `device_bearer_scoped(TenantScope::Device)`; the
    // pass returns `Unpaired` before reaching here when there is none, so this
    // never posts anonymously. The session-identity door accepts only that
    // credential, for the path device id.
    match client
        .post(&url)
        .bearer_auth(token)
        .json(&body)
        .send()
        .await
    {
        Ok(resp) => {
            let status = resp.status().as_u16();
            let text = if resp.status().is_success() {
                String::new()
            } else {
                resp.text().await.unwrap_or_default()
            };
            classify(status, &text)
        }
        Err(_) => PostOutcome::Transport,
    }
}

fn log_transition(state: &PassState) {
    match state {
        PassState::Posted => info!("session identity: posted to coord"),
        PassState::Idle => debug!("session identity: nothing changed since coord last accepted"),
        PassState::NoDeviceId => info!("session identity: no local device id — not posting"),
        PassState::NoCoord => info!("session identity: no coord connected — not posting"),
        PassState::Unpaired => {
            info!("session identity: no device credential (unpaired) — not posting")
        }
        PassState::Snapshot => warn!(
            "session identity: process snapshot indeterminate — pass skipped (a live session \
             would otherwise be reported as ended)"
        ),
        PassState::Build => warn!("session identity: the build task failed — retrying next pass"),
        PassState::Unavailable => info!(
            "session identity: coord answered 503 (stamp columns not deployed yet) — retrying \
             next pass, nothing marked reported"
        ),
        PassState::NotDeployed => info!(
            "session identity: coord answered 404 (the session-identity door is not deployed \
             yet) — backing off, nothing marked reported"
        ),
        PassState::Refused(code) => warn!(
            code = code.as_deref().unwrap_or("none"),
            "session identity: coord answered 403 — backing off"
        ),
        PassState::Rejected(code) => warn!(
            code = code.as_deref().unwrap_or("none"),
            "session identity: coord answered 422 — narrowing the window"
        ),
        PassState::Unauthorized => {
            warn!("session identity: coord answered 401 — retrying next pass")
        }
        PassState::Http(code) => {
            warn!("session identity: coord answered HTTP {code} — retrying next pass")
        }
        PassState::Transport => warn!("session identity: coord unreachable — retrying next pass"),
    }
}

fn reporter() -> &'static Arc<tokio::sync::Mutex<Reporter>> {
    static CELL: OnceLock<Arc<tokio::sync::Mutex<Reporter>>> = OnceLock::new();
    CELL.get_or_init(|| Arc::new(tokio::sync::Mutex::new(Reporter::default())))
}

/// Called on every session-lifecycle tick. Starts a pass when one is due and
/// none is in flight, and returns at once — the pass runs on its own task, so
/// the tick's latency is unaffected.
///
/// PRIMARY runner only: a secondary or temp runner on the same device would
/// post the same rows under the same device id from a separate watermark.
pub fn on_lifecycle_tick() {
    if !crate::instance::owns_shared_root_state() {
        return;
    }
    let Ok(mut guard) = Arc::clone(reporter()).try_lock_owned() else {
        return; // a pass is still running
    };
    if !guard.is_due(Instant::now()) {
        return;
    }
    tauri::async_runtime::spawn(async move {
        guard.run_pass().await;
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::transcript_last_turn::{last_turn_ms, ReadBudget};
    use std::fs;

    const SID_A: &str = "86e2b721-8270-49fe-bb15-6c8b33b000c8";
    const SID_B: &str = "11111111-1111-4111-8111-111111111111";
    const SID_C: &str = "22222222-2222-4222-8222-222222222222";
    const SID_D: &str = "33333333-3333-4333-8333-333333333333";

    fn home(root: &Path, name: &str) -> PathBuf {
        let h = root.join(name);
        fs::create_dir_all(h.join("projects")).unwrap();
        h
    }

    fn transcript(home: &Path, project: &str, sid: &str, turns: &[&str]) {
        let dir = home.join("projects").join(project);
        fs::create_dir_all(&dir).unwrap();
        let body: String = turns
            .iter()
            .map(|ts| {
                format!(r#"{{"type":"assistant","timestamp":"{ts}","message":{{"content":"x"}}}}"#)
                    + "\n"
            })
            .collect();
        fs::write(dir.join(format!("{sid}.jsonl")), body).unwrap();
    }

    fn name_file(names: &Path, sid: &str, name: &str) {
        fs::create_dir_all(names).unwrap();
        fs::write(names.join(sid), format!("{name}\n")).unwrap();
    }

    fn unbounded() -> impl FnMut(&TranscriptCopy) -> TurnRead {
        let mut budget = ReadBudget::Unbounded;
        move |c: &TranscriptCopy| last_turn_ms(&c.path, c.stamp, &mut budget)
    }

    fn row(id: &str, acted: &str) -> IdentityRow {
        IdentityRow {
            session_id: id.to_string(),
            live: false,
            account: "qontinui".to_string(),
            config_dir: "/h/.claude-qontinui".to_string(),
            registry_name: None,
            tag_name: Some(format!("name-{id}")),
            last_acted_at: Some(acted.to_string()),
        }
    }

    fn many_rows(n: usize) -> Vec<IdentityRow> {
        (0..n)
            .map(|i| {
                row(
                    &format!("00000000-0000-4000-8000-{i:012}"),
                    &format!("2026-10-01T00:00:00.{:03}Z", i % 1000),
                )
            })
            .collect()
    }

    #[test]
    fn account_labels_follow_the_census_contract() {
        assert_eq!(
            account_label(Path::new("/home/x/.claude")).as_deref(),
            Some("default")
        );
        assert_eq!(
            account_label(Path::new("/home/x/.claude-qontinui")).as_deref(),
            Some("qontinui")
        );
        assert_eq!(
            account_label(Path::new("C:/claude/.claude-tiohorst/")).as_deref(),
            Some("tiohorst")
        );
        assert_eq!(
            account_label(Path::new("/srv/custom-home")).as_deref(),
            Some("custom-home")
        );
        assert_eq!(
            account_label(Path::new("/home/x/.claude-")).as_deref(),
            Some(".claude-")
        );
        assert_eq!(account_label(Path::new("/")), None);
    }

    #[test]
    fn an_ended_session_with_a_name_and_an_account_root_yields_a_row() {
        let tmp = tempfile::tempdir().unwrap();
        let q = home(tmp.path(), ".claude-qontinui");
        transcript(
            &q,
            "-home-x-proj",
            SID_A,
            &["2026-10-19T08:00:00.000Z", "2026-10-19T09:15:30.250Z"],
        );
        let names = tmp.path().join("session-names");
        name_file(&names, SID_A, "spaceship-latency");

        let dir = SessionDirectory::discover_from(&names, std::slice::from_ref(&q));
        let got = build_candidates(&dir, &[], &mut unbounded());

        assert_eq!(got.deferred, 0);
        assert_eq!(
            got.rows,
            vec![IdentityRow {
                session_id: SID_A.to_string(),
                live: false,
                account: "qontinui".to_string(),
                config_dir: q.to_string_lossy().into_owned(),
                registry_name: None,
                tag_name: Some("spaceship-latency".to_string()),
                last_acted_at: Some("2026-10-19T09:15:30.250Z".to_string()),
            }]
        );
    }

    #[test]
    fn a_ghost_a_name_without_a_transcript_or_a_non_session_stem_yields_no_row() {
        let tmp = tempfile::tempdir().unwrap();
        let q = home(tmp.path(), ".claude-qontinui");
        let names = tmp.path().join("session-names");
        // B has a name but no transcript in any root: no account to name, so
        // no row — never one under an invented account.
        name_file(&names, SID_B, "named-only");
        // A transcript whose stem is not a session id is not a session.
        transcript(&q, "-p", "agent-a1b2c3", &["2026-10-19T08:00:00.000Z"]);
        // C is a ghost that DOES reach an input the reporter reads: a registry
        // file left by a crashed process (its pid is not live), with no name
        // and no transcript anywhere.
        fs::create_dir_all(q.join("sessions")).unwrap();
        fs::write(
            q.join("sessions").join("404.json"),
            format!(r#"{{"pid":404,"sessionId":"{SID_C}","cwd":"/p","updatedAt":1790000000000}}"#),
        )
        .unwrap();

        let homes = vec![q.clone()];
        let live = read_live_rows(&homes, &HashSet::from([1_u32]));
        let dir = SessionDirectory::discover_from(&names, &homes);
        let got = build_candidates(&dir, &live, &mut unbounded());
        assert!(got.rows.is_empty(), "rows: {:?}", got.rows);
        assert!(
            got.known_ids.is_empty(),
            "neither the stale ghost, the name-only id nor the non-session stem is known: {:?}",
            got.known_ids
        );
    }

    #[test]
    fn a_migrated_session_reports_the_root_holding_its_newest_turn() {
        let tmp = tempfile::tempdir().unwrap();
        let gmail = home(tmp.path(), ".claude-gmail");
        let qontinui = home(tmp.path(), ".claude-qontinui");
        // The resumed copy (qontinui) holds the newest turn. It is written
        // FIRST, so the stale gmail copy has the newer mtime, and gmail also
        // sorts first by name — neither may decide.
        transcript(
            &qontinui,
            "-p",
            SID_D,
            &["2026-10-18T10:00:00.000Z", "2026-10-19T12:00:00.000Z"],
        );
        std::thread::sleep(Duration::from_millis(20));
        transcript(&gmail, "-p", SID_D, &["2026-10-18T10:00:00.000Z"]);

        let names = tmp.path().join("session-names");
        // Walk gmail first, so "the first root" would be the wrong one.
        let dir = SessionDirectory::discover_from(&names, &[gmail.clone(), qontinui.clone()]);
        assert_eq!(dir.copies_of(SID_D).len(), 2, "every root's copy is kept");
        assert_eq!(
            dir.resolve(SID_D).config_dir.as_deref(),
            Some(crate::agent_worktree::custody::norm(&qontinui.to_string_lossy()).as_str()),
            "existing callers resolve the newest-turn root too"
        );

        let got = build_candidates(&dir, &[], &mut unbounded());
        assert_eq!(got.rows.len(), 1);
        assert_eq!(got.rows[0].account, "qontinui");
        assert_eq!(got.rows[0].config_dir, qontinui.to_string_lossy());
        assert_eq!(
            got.rows[0].last_acted_at.as_deref(),
            Some("2026-10-19T12:00:00.000Z")
        );
    }

    #[test]
    fn a_live_row_is_reported_live_with_only_an_operator_chosen_registry_name() {
        let tmp = tempfile::tempdir().unwrap();
        let q = home(tmp.path(), ".claude-qontinui");
        let d = home(tmp.path(), ".claude");
        transcript(&q, "-p", SID_A, &["2026-10-19T09:00:00.000Z"]);
        fs::create_dir_all(q.join("sessions")).unwrap();
        fs::create_dir_all(d.join("sessions")).unwrap();
        fs::write(
            q.join("sessions").join("101.json"),
            format!(r#"{{"pid":101,"sessionId":"{SID_A}","cwd":"/p","name":"spaceship-latency","status":"idle","updatedAt":1790000000000}}"#),
        )
        .unwrap();
        fs::write(
            d.join("sessions").join("202.json"),
            format!(r#"{{"pid":202,"sessionId":"{SID_B}","cwd":"/p","name":"qontinui-root-ec","nameSource":"derived","status":"busy","updatedAt":1790000000000}}"#),
        )
        .unwrap();
        // A stale file from a crashed process: its pid is not live.
        fs::write(
            q.join("sessions").join("303.json"),
            format!(r#"{{"pid":303,"sessionId":"{SID_C}","cwd":"/p","name":"crashed","updatedAt":1790000000000}}"#),
        )
        .unwrap();

        let homes = vec![q.clone(), d.clone()];
        let live_pids: HashSet<u32> = [101, 202].into_iter().collect();
        let live = read_live_rows(&homes, &live_pids);
        let dir = SessionDirectory::discover_from(&tmp.path().join("none"), &homes);
        let got = build_candidates(&dir, &live, &mut unbounded());

        assert_eq!(got.rows.len(), 2, "rows: {:?}", got.rows);
        let a = got.rows.iter().find(|r| r.session_id == SID_A).unwrap();
        assert!(a.live);
        assert_eq!(a.registry_name.as_deref(), Some("spaceship-latency"));
        assert_eq!(a.account, "qontinui");
        assert_eq!(a.last_acted_at.as_deref(), Some("2026-10-19T09:00:00.000Z"));
        let b = got.rows.iter().find(|r| r.session_id == SID_B).unwrap();
        assert!(b.live);
        assert_eq!(b.registry_name, None, "a derived auto-name is not reported");
        assert_eq!(b.account, "default", "the bare home is `default`");
        // No turn line yet: the registry's own update time stands in.
        assert_eq!(b.last_acted_at, ms_to_rfc3339(1_790_000_000_000));
        assert!(got.rows.iter().all(|r| r.session_id != SID_C));
    }

    #[test]
    fn an_unread_tail_defers_the_row_rather_than_guessing_a_time() {
        let tmp = tempfile::tempdir().unwrap();
        let q = home(tmp.path(), ".claude-qontinui");
        transcript(&q, "-p", SID_B, &["2026-10-19T09:00:00.000Z"]);
        let dir =
            SessionDirectory::discover_from(&tmp.path().join("none"), std::slice::from_ref(&q));
        let mut none_left = ReadBudget::Limited(0);
        let got = build_candidates(&dir, &[], &mut |c| {
            last_turn_ms(&c.path, c.stamp, &mut none_left)
        });
        assert!(got.rows.is_empty());
        assert_eq!(got.deferred, 1);
        assert!(
            got.known_ids.contains(SID_B),
            "a deferred id is still known"
        );
    }

    #[test]
    fn an_over_long_name_is_omitted_never_truncated() {
        let tmp = tempfile::tempdir().unwrap();
        let q = home(tmp.path(), ".claude-qontinui");
        transcript(&q, "-p", SID_A, &["2026-10-19T09:00:00.000Z"]);
        let names = tmp.path().join("session-names");
        name_file(&names, SID_A, &"n".repeat(MAX_NAME_CHARS + 1));
        let dir = SessionDirectory::discover_from(&names, std::slice::from_ref(&q));
        let got = build_candidates(&dir, &[], &mut unbounded());
        assert_eq!(got.rows.len(), 1);
        assert_eq!(got.rows[0].tag_name, None);
        assert_eq!(
            bounded_name(&"n".repeat(MAX_NAME_CHARS)).map(|s| s.len()),
            Some(MAX_NAME_CHARS)
        );
    }

    #[test]
    fn an_unchanged_row_is_not_resent_and_a_changed_fact_is() {
        let mut wm = Watermark::default();
        let r = row(SID_A, "2026-10-19T09:00:00.000Z");
        let q = HashMap::new();
        assert_eq!(
            wm.select(std::slice::from_ref(&r), 500, &q),
            vec![r.clone()]
        );
        assert_eq!(wm.record_sent(std::slice::from_ref(&r), 1), 1);
        assert!(
            wm.select(std::slice::from_ref(&r), 500, &q).is_empty(),
            "an unchanged row is not re-sent"
        );

        let mut newer = r.clone();
        newer.last_acted_at = Some("2026-10-19T10:00:00.000Z".to_string());
        assert_eq!(wm.select(std::slice::from_ref(&newer), 500, &q).len(), 1);

        let mut renamed = r.clone();
        renamed.tag_name = Some("renamed".to_string());
        assert_eq!(wm.select(std::slice::from_ref(&renamed), 500, &q).len(), 1);

        let mut moved = r.clone();
        moved.config_dir = "/h/.claude-gmail".to_string();
        assert_eq!(wm.select(std::slice::from_ref(&moved), 500, &q).len(), 1);

        // A live registry name is a fact; its later absence (the session
        // ended) is not a change.
        let mut live = r.clone();
        live.live = true;
        live.registry_name = Some("spaceship-latency".to_string());
        assert_eq!(wm.select(std::slice::from_ref(&live), 500, &q).len(), 1);
        assert_eq!(wm.record_sent(std::slice::from_ref(&live), 2), 0);
        assert!(wm.select(std::slice::from_ref(&r), 500, &q).is_empty());
    }

    #[test]
    fn a_window_holds_at_most_500_rows_and_rotates_oldest_reported_first() {
        let rows = many_rows(1_200);
        let q = HashMap::new();
        let mut wm = Watermark::default();

        let first = wm.select(&rows, MAX_ROWS_PER_POST, &q);
        assert_eq!(first.len(), MAX_ROWS_PER_POST);
        wm.record_sent(&first, 100);
        let second = wm.select(&rows, MAX_ROWS_PER_POST, &q);
        assert_eq!(second.len(), MAX_ROWS_PER_POST);
        assert!(
            second.iter().all(|r| !first.contains(r)),
            "never-reported rows go next"
        );
        wm.record_sent(&second, 200);
        let third = wm.select(&rows, MAX_ROWS_PER_POST, &q);
        assert_eq!(third.len(), 200);
        wm.record_sent(&third, 300);
        assert!(wm.select(&rows, MAX_ROWS_PER_POST, &q).is_empty());

        // Every fact changes: the OLDEST-reported window goes first.
        let changed: Vec<IdentityRow> = rows
            .iter()
            .map(|r| IdentityRow {
                last_acted_at: Some("2026-11-01T00:00:00.000Z".to_string()),
                ..r.clone()
            })
            .collect();
        let next = wm.select(&changed, MAX_ROWS_PER_POST, &q);
        let first_ids: HashSet<&str> = first.iter().map(|r| r.session_id.as_str()).collect();
        assert_eq!(next.len(), MAX_ROWS_PER_POST);
        assert!(next
            .iter()
            .all(|r| first_ids.contains(r.session_id.as_str())));
    }

    #[test]
    fn never_reported_rows_go_most_recently_acted_first() {
        let old = row(SID_A, "2026-01-01T00:00:00.000Z");
        let new = row(SID_B, "2026-10-01T00:00:00.000Z");
        let wm = Watermark::default();
        let got = wm.select(&[old.clone(), new.clone()], 1, &HashMap::new());
        assert_eq!(got, vec![new]);
    }

    #[test]
    fn a_503_or_404_never_advances_the_watermark() {
        let rows = many_rows(3);
        let now = Instant::now();
        let mut r = Reporter {
            watermark: Some(Watermark::default()),
            ..Reporter::default()
        };
        let batch = r
            .watermark
            .as_ref()
            .unwrap()
            .select(&rows, r.window, &r.quarantine);
        assert_eq!(batch.len(), 3);

        assert_eq!(
            r.apply(PostOutcome::Unavailable, &batch, now, 1),
            PassState::Unavailable
        );
        assert!(r.watermark.as_ref().unwrap().is_empty());
        assert!(!r.dirty);

        assert_eq!(
            r.apply(PostOutcome::NotDeployed, &batch, now, 2),
            PassState::NotDeployed
        );
        assert!(r.watermark.as_ref().unwrap().is_empty());
        assert_eq!(r.next_due, Some(now + NOT_DEPLOYED_BACKOFF));
        assert!(!r.is_due(now + NOT_DEPLOYED_BACKOFF - Duration::from_secs(1)));
        // A second 404 doubles the back-off.
        r.apply(PostOutcome::NotDeployed, &batch, now, 3);
        assert_eq!(r.next_due, Some(now + NOT_DEPLOYED_BACKOFF * 2));

        // The same rows are still pending.
        assert_eq!(
            r.watermark
                .as_ref()
                .unwrap()
                .select(&rows, r.window, &r.quarantine),
            batch
        );

        assert_eq!(
            r.apply(PostOutcome::Accepted, &batch, now, 4),
            PassState::Posted
        );
        assert_eq!(r.watermark.as_ref().unwrap().len(), 3);
        assert!(r.dirty);
        assert_eq!(r.backoff, None, "an accepted post clears the back-off");
        assert!(r
            .watermark
            .as_ref()
            .unwrap()
            .select(&rows, r.window, &r.quarantine)
            .is_empty());
    }

    #[test]
    fn a_422_narrows_the_window_and_quarantines_a_single_refused_row() {
        let rows = many_rows(400);
        let now = Instant::now();
        let mut r = Reporter {
            watermark: Some(Watermark::default()),
            ..Reporter::default()
        };
        let rejected = || PostOutcome::Rejected {
            code: Some("too_many_rows".to_string()),
        };
        r.apply(rejected(), &rows, now, 1);
        assert_eq!(r.window, 200);
        assert!(r.watermark.as_ref().unwrap().is_empty());

        let one = vec![rows[0].clone()];
        r.apply(rejected(), &one, now, 2);
        let next = r
            .watermark
            .as_ref()
            .unwrap()
            .select(&one, 500, &r.quarantine);
        assert!(next.is_empty(), "the refused row is held back");

        // Its facts change: it is offered again.
        let mut changed = rows[0].clone();
        changed.tag_name = Some("fixed".to_string());
        assert_eq!(
            r.watermark
                .as_ref()
                .unwrap()
                .select(std::slice::from_ref(&changed), 500, &r.quarantine)
                .len(),
            1
        );

        // A success grows the window back toward the cap: 200 doubles to 400.
        r.apply(
            PostOutcome::Accepted,
            std::slice::from_ref(&changed),
            now,
            3,
        );
        assert_eq!(r.window, 400);
        r.apply(
            PostOutcome::Accepted,
            std::slice::from_ref(&changed),
            now,
            4,
        );
        assert_eq!(r.window, MAX_ROWS_PER_POST, "never past the cap");
    }

    #[test]
    fn coord_answers_are_classified_per_the_contract() {
        assert_eq!(classify(200, ""), PostOutcome::Accepted);
        assert_eq!(classify(201, ""), PostOutcome::Accepted);
        assert_eq!(
            classify(404, r#"{"error":"no_such_route"}"#),
            PostOutcome::NotDeployed
        );
        assert_eq!(
            classify(503, r#"{"error":"session_identity_unavailable"}"#),
            PostOutcome::Unavailable
        );
        assert_eq!(
            classify(422, r#"{"error":"too_many_rows"}"#),
            PostOutcome::Rejected {
                code: Some("too_many_rows".to_string())
            }
        );
        assert_eq!(
            classify(403, r#"{"code":"paired_device_required"}"#),
            PostOutcome::Refused {
                code: Some("paired_device_required".to_string())
            }
        );
        assert_eq!(classify(401, "not json"), PostOutcome::Unauthorized);
        assert_eq!(classify(500, ""), PostOutcome::Http(500));
    }

    #[test]
    fn the_post_body_carries_the_contract_field_names() {
        let r = row(SID_A, "2026-10-19T09:00:00.000Z");
        let body = IdentityPost {
            observed_at: "2026-10-19T09:01:00.000Z".to_string(),
            rows: std::slice::from_ref(&r),
        };
        let v = serde_json::to_value(&body).unwrap();
        assert_eq!(v["observed_at"], "2026-10-19T09:01:00.000Z");
        let obj = v["rows"][0].as_object().unwrap();
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec![
                "account",
                "config_dir",
                "last_acted_at",
                "live",
                "registry_name",
                "session_id",
                "tag_name"
            ]
        );
        assert!(obj["registry_name"].is_null());
    }

    #[test]
    fn the_watermark_round_trips_and_an_unreadable_one_is_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("sub").join(WATERMARK_FILE);
        let mut wm = Watermark::default();
        wm.record_sent(&many_rows(3), 42);
        write_watermark(&path, &wm).unwrap();
        assert_eq!(load_watermark(&path), wm);

        fs::write(&path, b"{not json").unwrap();
        assert!(load_watermark(&path).is_empty());
        fs::write(&path, br#"{"version":99,"entries":{}}"#).unwrap();
        assert_eq!(load_watermark(&path), Watermark::default());
    }

    #[test]
    fn a_walk_that_read_fewer_roots_never_prunes_the_watermark() {
        let mut r = Reporter {
            watermark: Some(Watermark::default()),
            ..Reporter::default()
        };
        r.watermark.as_mut().unwrap().record_sent(
            &[
                row(SID_A, "2026-10-19T09:00:00.000Z"),
                row(SID_B, "2026-10-19T09:00:00.000Z"),
            ],
            1,
        );
        let both: HashSet<String> = [SID_A.to_string(), SID_B.to_string()].into();
        let only_a: HashSet<String> = [SID_A.to_string()].into();

        // A full read of three roots sets the bar; nothing is missing yet.
        assert_eq!(r.prune_watermark(&both, 3), 0);
        // An empty walk saw nothing: absence is not evidence.
        assert_eq!(r.prune_watermark(&HashSet::new(), 0), 0);
        // A partial walk (one root unreadable) does not prune B either.
        assert_eq!(r.prune_watermark(&only_a, 2), 0);
        assert_eq!(r.watermark.as_ref().unwrap().len(), 2);
        assert!(!r.dirty);
        // A walk as wide as the bar that still lacks B: B is gone.
        assert_eq!(r.prune_watermark(&only_a, 3), 1);
        assert_eq!(r.watermark.as_ref().unwrap().len(), 1);
        assert!(r.dirty);
    }

    #[test]
    fn a_migrated_session_that_exited_before_its_first_turn_stays_on_the_new_account() {
        let tmp = tempfile::tempdir().unwrap();
        let gmail = home(tmp.path(), ".claude-gmail");
        let qontinui = home(tmp.path(), ".claude-qontinui");
        let turns = ["2026-10-18T10:00:00.000Z", "2026-10-19T12:00:00.000Z"];
        // The original (gmail) is written first; the migration then copies the
        // transcript into qontinui. The resumed session exits without a turn,
        // so both copies hold the same newest turn, and gmail sorts first by
        // name — only the newer write can name the account it continued under.
        transcript(&gmail, "-p", SID_D, &turns);
        std::thread::sleep(Duration::from_millis(20));
        transcript(&qontinui, "-p", SID_D, &turns);

        let dir = SessionDirectory::discover_from(
            &tmp.path().join("none"),
            &[gmail.clone(), qontinui.clone()],
        );
        let got = build_candidates(&dir, &[], &mut unbounded());
        assert_eq!(got.rows.len(), 1);
        assert_eq!(got.rows[0].account, "qontinui");
        assert_eq!(
            got.rows[0].last_acted_at.as_deref(),
            Some("2026-10-19T12:00:00.000Z")
        );
    }

    #[test]
    fn pruning_drops_ids_no_longer_known() {
        let mut wm = Watermark::default();
        wm.record_sent(
            &[
                row(SID_A, "2026-10-19T09:00:00.000Z"),
                row(SID_B, "2026-10-19T09:00:00.000Z"),
            ],
            1,
        );
        let known: HashSet<String> = [SID_A.to_string()].into_iter().collect();
        assert_eq!(wm.retain_ids(&known), 1);
        assert_eq!(wm.len(), 1);
    }
}
