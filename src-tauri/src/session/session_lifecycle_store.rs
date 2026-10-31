//! Durable, backend-owned terminal-session lifecycle registry, keyed by
//! `claudeSessionId`.
//!
//! ## Problem
//!
//! The frontend grid previously tracked "which Claude sessions exist and
//! which zone each belongs to" in a `localStorage` snapshot. That snapshot
//! is fragile: it duplicates sessions across restarts (a fresh terminal id
//! per launch keys a new row), and it can't observe a session that died
//! while the UI wasn't looking. The result is ghost / duplicate session
//! tiles in the grid.
//!
//! ## Fix
//!
//! Make the runner backend the source of truth. Each terminal that hosts a
//! Claude session records a [`TerminalSessionRecord`] keyed by the *stable*
//! `claude_session_id`. Re-opening the same Claude session (same id) updates
//! the existing record in place rather than appending a duplicate — the
//! structural dedup-by-key is what kills the duplicate-session bug. A slow
//! liveness poll ([`classify`]) lazily flips dead sessions to `closed`,
//! never closing on uncertainty.
//!
//! ## Storage
//!
//! Mirrors [`crate::session::pane_store::PaneSessionStore`]: a single JSON
//! file at `<.qontinui>/runner/terminal-sessions.json` (an object map
//! `claude_session_id -> record`), rewritten atomically via temp-file +
//! rename. Missing / corrupt file → empty map (a fresh registry is the safe
//! default; the poll re-discovers live sessions).
//!
//! Durability is a **write-ahead log of per-record deltas** compacted into that
//! snapshot — see [`LifecycleDelta`] and [`SessionLifecycleStore::compact`].
//!
//! ## Durability scope — PROCESS crash, not power loss
//!
//! The WAL survives the runner process dying: an appended line is one
//! `write_all` into an append-mode handle, so a crash can only truncate the
//! tail (which [`replay_wal`] drops), and compaction writes the snapshot before
//! truncating the log, so a crash in between replays deltas the snapshot
//! already holds — idempotent.
//!
//! It is deliberately NOT power-loss durable. Appends are **not `fsync`ed**:
//! they sit in the OS page cache, so a host power cut or kernel panic can lose
//! recently appended deltas even though the `write_all` returned. That is the
//! accepted trade — an `fsync` per mutation would put a disk flush back on the
//! terminal spawn path, which is exactly the cost this design removed, and the
//! failure mode is bounded (a session record loses at most its latest state and
//! the liveness poll re-discovers live sessions). Do not read the crash-safety
//! ordering above as a power-loss guarantee.

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use crate::session::observation_epoch::{ObservationEpoch, Observed};
use crate::session::restore_record_emitter::RestoreRecordEmitter;
use crate::session::snapshot_history::{SnapshotHistory, SnapshotSession, TranscriptProbe};

/// The runner's own app-data root, `~/.qontinui/runner`, keeping the existing
/// "." fallback for a home-less environment. Base for the instance-scoped
/// path helpers below.
fn runner_dir() -> PathBuf {
    qontinui_runner_lib::ambient::runner_dir_or_cwd()
}

/// Canonical, instance-scoped lifecycle-store path — the single source of
/// truth every consumer must resolve through (boot writer, fleet publishers,
/// message poller, session bus, the test-fixture control routes).
///
/// - Primary (`instance::data_subdir() == None`) → the legacy UNSCOPED
///   `~/.qontinui/runner/terminal-sessions.json`, byte-for-byte identical to
///   the pre-fix primary path, so its crash-recovery reattach is unchanged.
/// - Named/temp secondary → `~/.qontinui/runner/instance-<name>/terminal-sessions.json`.
///
/// This replaces the previous API-PORT keying. Temp-runner ports (9877-9899)
/// are RECYCLED across unrelated spawns, so a fresh runner on a reused port
/// used to `open()` the previous occupant's `terminal-sessions-<port>.json`
/// and auto-restore its foreign PTYs. The durable instance identity is unique
/// per spawn and stable per named runner, so no such inheritance is possible.
/// Mirrors the #788 `window-assignments` convention (scope the DIRECTORY via
/// `instance::scope_path`, keep the filename plain) and reuses its fail-closed
/// `instance-unnamed-<port>` quarantine for free.
pub fn store_path() -> PathBuf {
    crate::instance::scope_path(&runner_dir()).join("terminal-sessions.json")
}

/// Canonical, instance-scoped snapshot-history path, sibling to
/// [`store_path`]: primary →
/// `~/.qontinui/runner/session-restore/session-snapshots.jsonl`; secondary →
/// `~/.qontinui/runner/instance-<name>/session-restore/session-snapshots.jsonl`.
///
/// DELIBERATELY re-derives the `session-restore/` segment under the scoped
/// base rather than calling `crate::session::claude_hook::session_restore_dir()`:
/// that dir is ALSO the Claude SessionStart-hook materialization dir and must
/// stay UNSCOPED (scoping it globally would move hook delivery — out of scope).
/// Do NOT "consolidate" the two into one call.
pub fn snapshot_history_path() -> PathBuf {
    crate::instance::scope_path(&runner_dir())
        .join("session-restore")
        .join("session-snapshots.jsonl")
}

/// Closed records older than this are pruned (24h in millis).
const CLOSED_RETENTION_MS: i64 = 86_400_000;
/// Open records not seen for this long are pruned (7d in millis).
const OPEN_STALE_MS: i64 = 604_800_000;
/// A record closed with reason `"pty-exit"` (its PTY died — e.g. a graceful
/// runner restart firing `handleExit` on every live PTY) is still restorable
/// when it closed within this long of the registry's LAST MOMENT OF LIFE (the
/// restore anchor — see [`restore_admissible`]), NOT of wall-clock now: a
/// graceful stop stamps every close at the shutdown instant, so a rebuild of
/// any length still restores them. A close further back than the window, or
/// any other close reason (explicit user close), is NOT restorable. 10 minutes
/// in millis.
const RESTORABLE_PTY_EXIT_MS: i64 = 600_000;
/// A record closed with reason `"poll-dead"` (the liveness poll saw a live
/// shell with zero descendants for several consecutive idle ticks) is still
/// restorable when it closed within this long of the registry's LAST MOMENT OF
/// LIFE (the restore anchor — see [`restore_admissible`]), not of wall-clock
/// now. A `poll-dead` close is far less
/// certain than a `pty-exit` (the shell pty is, by definition, still alive —
/// the session was merely idle between tool calls), so an immediate restart
/// should bring it back rather than silently drop it. Beyond the window it is
/// NOT restorable (a genuinely abandoned session should not resurrect days
/// later). 10 minutes in millis.
const RESTORABLE_POLL_DEAD_MS: i64 = 600_000;
/// Anchored-recency grace for `state == "open"` rows in
/// [`SessionLifecycleStore::restorable_records`]. An open row is restorable
/// iff `anchor - last_seen_at <= grace`, where `anchor` is the EARLIER of
/// the registry's LAST MOMENT OF LIFE (max of every row's `last_seen_at` /
/// `closed_at`, plus the prior shutdown marker's `at`) and the OBSERVATION
/// EPOCH — the last poll tick whose process snapshot succeeded
/// ([`crate::session::observation_epoch`]; see [`RestoreAnchors`]).
///
/// A row's `last_seen_at` only advances when a SUCCESSFUL poll tick touches
/// it, so its staleness is evidence of death only while the poll could see.
/// A stretch of failed snapshots freezes every stamp; measuring against the
/// last observation keeps that unobservable stretch from reading as "dead"
/// (the 2026-10-27 force-exit stranded 64 of 65 sessions that way: no touch
/// landed for 77 minutes before the clean marker). Recency relative to the
/// anchor — not wall-clock now — survives arbitrary downtime: a crash
/// followed by an hours-later boot still restores the rows that were fresh
/// when the previous process last looked, while a multi-day ghost row (one
/// successful ticks kept NOT finding alive) stays excluded on EVERY boot
/// kind. 10 minutes in millis (matches the close-grace windows).
const RESTORABLE_OPEN_ANCHOR_GRACE_MS: i64 = 600_000;
/// Number of consecutive claude-absent ticks a *live* shell (`Some(true)` with
/// no Claude in its inclusive subtree) must accumulate before the poll closes
/// it `poll-dead`. A live Claude in the subtree is KeepAlive immediately and
/// never reaches this debounce. The debounce only governs the claude-absent
/// case: it absorbs a transient process-snapshot miss and gives an in-flight
/// relaunch a window, so a momentary blip doesn't drop a session. Requiring
/// several ticks (≈ N × 45s poll interval) only closes shells that are
/// claude-absent for an extended, unambiguous stretch (operator quit claude;
/// bare shell lingers). A `Some(false)` (pty actually dead) shell still closes
/// immediately — that path is unambiguous and bypasses this debounce entirely.
const LIVE_SHELL_DEAD_TICKS: u32 = 3;
/// Number of consecutive NO-MATCHING-TERMINAL ticks an open record must
/// accumulate before the poll closes it `"no-terminal"`. A record whose
/// terminal id (and fallback triple) matches nothing in THIS instance for
/// many consecutive ticks is not "uncertainty" — it is an orphan (e.g. a
/// stale ghost row inherited from a previous process) that would otherwise
/// stay `open` for up to 7 days and re-qualify for restore at every boot.
/// The close fires on tick N+1 (≈ 4 ticks × 45s ≈ 3 min), debounced so a
/// just-created record whose terminal is still registering never closes.
/// `"no-terminal"` is automatically NON-restorable: the restore grace match
/// only covers `"pty-exit"` / `"poll-dead"`.
const NO_TERMINAL_ORPHAN_TICKS: u32 = 3;

/// The provider every pre-provider-aware record is assumed to belong to. Used
/// as the `#[serde(default)]` for [`TerminalSessionRecord::provider`]: existing
/// on-disk records have no `provider` field and are all Claude today.
pub const DEFAULT_PROVIDER: &str = "claude";

/// `#[serde(default)]` source for [`TerminalSessionRecord::provider`].
fn default_provider() -> String {
    DEFAULT_PROVIDER.to_string()
}

/// Authoritative origin: the runner KNOWS the session id exactly (pre-pinned
/// `--session-id`, lifted from a typed flag, or a provider hook POSTed it).
pub const ORIGIN_AUTHORITATIVE: &str = "authoritative";
/// Reconciled origin: the id was recovered by a backstop and may name a foreign
/// session — restore treats it conservatively.
pub const ORIGIN_RECONCILED: &str = "reconciled";
/// Observed origin: a claude-process-start-anchored, uniquely-correlated
/// transcript bind — the transcript proves the session exists, so restore
/// treats a CONFIRMED observed row the same as confirmed authoritative
/// (auto-resume-eligible), unlike the conservative `reconciled`/mtime guess.
pub const ORIGIN_OBSERVED: &str = "observed";

/// The `zone_index` sentinel for "this session sits in no zone".
///
/// It carries TWO meanings and the writer decides which: the frontend writes it
/// to ASSERT that a tab was dragged out of every zone (past the 9-zone ceiling a
/// live tab is genuinely unassigned), while a backstop that simply has no view
/// of the grid means "unknown". Only the asserting writer may persist it —
/// [`crate::session::reconcile`] carries the terminal's existing zone forward
/// instead, because an unknown that overwrites a known value is not a default,
/// it is data loss. Mirrors `UNZONED_INDEX` in `src/components/terminal/sessionRecordArgs.ts`.
pub const UNZONED_ZONE_INDEX: i32 = -1;

/// Normalize a possibly-legacy `origin` value to the current vocabulary. Maps
/// the pre-migration `bind_origin` values (`"pinned"`→`"authoritative"`,
/// `"guessed"`→`"reconciled"`) and passes the new values through unchanged —
/// including `"observed"`, which has no legacy alias and falls through the `_`
/// arm verbatim. Any other string is left verbatim (forward-compat: a value
/// this build doesn't know is not silently rewritten). Returns `None` for
/// `None`.
fn normalize_origin(origin: Option<String>) -> Option<String> {
    origin.map(|o| match o.as_str() {
        "pinned" => ORIGIN_AUTHORITATIVE.to_string(),
        "guessed" => ORIGIN_RECONCILED.to_string(),
        _ => o,
    })
}

/// [`TerminalSessionRecord::wind_down_outcome`] — the graceful exit landed and
/// the pane was closed.
pub const WIND_DOWN_CLOSED: &str = "closed";
/// [`TerminalSessionRecord::wind_down_outcome`] — `claude` outlived the
/// graceful-exit deadline. Nothing was killed; the session is STILL RUNNING.
pub const WIND_DOWN_EXIT_STUCK: &str = "exit_stuck";
/// [`TerminalSessionRecord::wind_down_outcome`] — `claude` left, but the pane
/// could not be proven clear at the moment of the close, so the tab was left
/// open (see `terminal::graceful_exit::CloseTabResult`).
pub const WIND_DOWN_CLOSE_REFUSED: &str = "close_refused";
/// [`TerminalSessionRecord::wind_down_outcome`] — the close was attempted and
/// what it achieved is NOT known. Distinct from
/// [`WIND_DOWN_CLOSE_REFUSED`], which is the positive statement that the pane
/// was left untouched.
pub const WIND_DOWN_CLOSE_UNKNOWN: &str = "close_unknown";
/// [`TerminalSessionRecord::wind_down_outcome`] — the pane was not in a state
/// where `/exit` was safe to submit, or the write failed. Nothing was closed,
/// but the pane MAY have been written to (see
/// `terminal::graceful_exit::GracefulExitOutcome::Refused`), which is why this
/// is recorded rather than treated as a non-event.
pub const WIND_DOWN_NOT_ATTEMPTED: &str = "exit_not_submitted";

/// [`TerminalSessionRecord::wind_down_arm`] — the close was made by the
/// executor's DRAIN arm: coord held this device drained.
pub const WIND_DOWN_ARM_DRAIN: &str = "drain";
/// [`TerminalSessionRecord::wind_down_arm`] — the close was made by the
/// executor's `finished_close` arm: a finished, idle, custody-clean terminal
/// session past grace, with no drain involved (plan
/// `2026-10-03-finished-runner-sessions-close-their-window-without-a-drain`).
pub const WIND_DOWN_ARM_FINISHED_CLOSE: &str = "finished_close";

/// Restore tier: the session was brought back with `claude --resume` and the
/// provider handshake landed — the transcript continues.
pub const RESTORE_TIER_RESUMED: &str = "resumed";
/// Restore tier: only the PTY/terminal came back (no transcript to resume, or
/// resume was deliberately not attempted). The tile exists; the conversation
/// does not.
pub const RESTORE_TIER_TERMINAL_ONLY: &str = "terminal-only";
/// Restore tier: a resume was attempted for this record and did not land.
pub const RESTORE_TIER_FAILED: &str = "failed";

/// How long a `restore_pending_at` marker may stand before the rendered verdict
/// stops calling the restore "in flight" and starts calling it timed out.
///
/// Derived from the frontend's own resume-verification budget, which is the
/// only thing that can legitimately hold the marker open. `typeResumeAndVerify`
/// (`src/components/terminal/resumeVerification.ts`) spends at most
/// `attempts` (2) x `waitForClaudeHandshake`'s `timeoutMs` (15 s) plus the
/// inter-attempt `settleMs` pauses — the file's own header calls it the
/// "~31 s scrollback poll" budget. The backend's liveness poll, whose
/// `PollAction::KeepAlive` arm self-heals a stale marker via
/// [`SessionLifecycleStore::clear_restore_pending`], runs on a 3 s cadence on
/// top of that.
///
/// Five minutes is roughly an order of magnitude above that ~31 s worst case,
/// which is deliberate: the cost of being too GENEROUS is a marker that reads
/// "pending" for a few extra minutes, while the cost of being too tight is
/// calling a slow-but-live restore a failure. A loaded box has been measured
/// taking 10 s for a single local `/health` probe, so the margin is not
/// theoretical.
pub const RESTORE_PENDING_TTL_MS: i64 = 300_000;

/// The rendered restore verdict for one record — what a reader should be TOLD,
/// as opposed to the raw `restore_tier` byte that is stored.
///
/// `mark_restore_pending` deliberately stamps [`RESTORE_TIER_FAILED`] the
/// instant a resume is ATTEMPTED, because the census must never claim a
/// restore landed when it might not have. That pessimism is right for the
/// stored value and wrong for the rendered one: every reader — the session-info
/// projection, the restore-health report — printed the bare word `failed` while
/// the resume was still in flight, which reads as a terminal verdict on a
/// restore that is merely unfinished. An operator seeing `failed` reaches for
/// recovery; the honest statement is "not yet confirmed".
///
/// `restore_pending_at` is the discriminator, and it is already durable and
/// backend-owned. `failed` survives as the verdict only once the pending
/// marker is CLEARED without the tier being upgraded to
/// [`RESTORE_TIER_RESUMED`] — i.e. the attempt really is over and really did
/// not land.
///
/// ## The marker AGES OUT ([`RESTORE_PENDING_TTL_MS`])
///
/// "Pending" is only honest while the resume verification could still be
/// running. Nothing guarantees it finishes: the frontend can be killed
/// mid-handshake, and the backend self-heal that would clear the marker fires
/// only on `PollAction::KeepAlive` — i.e. only for a session observed
/// confidently ALIVE. A record whose verification silently died therefore held
/// its marker forever and was reported as a restore in flight forever, which is
/// a worse lie than the pessimistic `failed` this rendering exists to soften.
///
/// So a marker older than [`RESTORE_PENDING_TTL_MS`] renders
/// `"failed (verification timed out)"` — a statement of what is actually known
/// (a resume was attempted, and nothing ever confirmed it) rather than a claim
/// that something is still happening. `now_ms` is passed in rather than read
/// here so one projection renders every row against a single instant, and so
/// the rule is testable without sleeping.
pub fn describe_restore_status(
    restore_tier: Option<&str>,
    restore_pending_at: Option<i64>,
    now_ms: i64,
) -> String {
    // `saturating_sub` so a marker stamped in the FUTURE (clock skew between
    // the stamping and the rendering) reads as age 0 — in flight — rather than
    // wrapping into a huge age and reporting a timeout that has not happened.
    let stale = |at: i64| now_ms.saturating_sub(at) > RESTORE_PENDING_TTL_MS;
    match (restore_tier, restore_pending_at) {
        // A restore whose verification never came back. The attempt is over in
        // every sense that matters; say so.
        (Some(RESTORE_TIER_FAILED), Some(at)) if stale(at) => {
            "failed (verification timed out)".to_string()
        }
        // A restore in flight. `failed` here means "attempt recorded, landing
        // unproven" — say exactly that.
        (Some(RESTORE_TIER_FAILED), Some(_)) => "pending (not yet confirmed)".to_string(),
        (Some(tier), _) => tier.to_string(),
        // Never restored is NOT a failed restore — but an ancient marker on
        // such a record is just as stale as one on a `failed`-tier row.
        (None, Some(at)) if stale(at) => "failed (verification timed out)".to_string(),
        (None, Some(_)) => "pending (not yet confirmed)".to_string(),
        (None, None) => "not-restored".to_string(),
    }
}

/// Reap the restore-pending marker on a record that is about to become
/// `closed`, demoting a still-pessimistic `failed` tier to
/// [`RESTORE_TIER_TERMINAL_ONLY`].
///
/// ## Why a close is the right place to do this
///
/// [`SessionLifecycleStore::mark_restore_pending`] stamps `restore_pending_at`
/// + [`RESTORE_TIER_FAILED`] the instant a resume is ATTEMPTED, and exactly one
/// thing ever retires that pair: [`SessionLifecycleStore::clear_restore_pending`],
/// reached from the frontend's verified handshake or from the liveness poll's
/// `PollAction::KeepAlive` self-heal. `KeepAlive` requires the session to be
/// observed confidently ALIVE, which a closed record can never be. So once a
/// record goes `closed` the marker is unreachable: it is being held for a
/// retry that cannot occur, and the restore-health report keeps announcing a
/// restore in flight on a session that ended — forever.
///
/// ## Why BOTH fields have to move
///
/// Clearing `restore_pending_at` alone is not enough. `?include=failed` selects
/// on the stored TIER (`RestoreHealthFilter::matches`), so a closed row that
/// kept `restore_tier: "failed"` would still be pulled into the failed bucket
/// and still be reported as a broken restore needing attention. The demotion
/// target is [`RESTORE_TIER_TERMINAL_ONLY`] because that is the honest residual
/// verdict — a terminal was restored, a conversation was not — and because
/// `failed` must not survive as a permanent accusation against a session that
/// simply ended before its resume could be confirmed.
///
/// Every other tier is left verbatim: [`RESTORE_TIER_RESUMED`] is a landed
/// restore whose truth a close does not change, [`RESTORE_TIER_TERMINAL_ONLY`]
/// is already the value we would write, and `None` means no restore was ever
/// recorded — a close must not invent one.
fn reap_restore_marker_on_close(rec: &mut TerminalSessionRecord) {
    // A close also ends any wait for an account choice. Cleared here, the one
    // helper every close path calls (`record_close_checked`, both `record_open`
    // supersede arms, the boot collision repair), so no close can leave a
    // stamp behind for a later re-open to mistake for a running hold.
    rec.awaiting_account_since = None;
    rec.restore_pending_at = None;
    if rec.restore_tier.as_deref() == Some(RESTORE_TIER_FAILED) {
        rec.restore_tier = Some(RESTORE_TIER_TERMINAL_ONLY.to_string());
    }
}

/// [`TerminalSessionRecord::name_source`] value meaning "an operator chose this
/// name" (a `/rename`, or a launch-time name).
pub const NAME_SOURCE_OPERATOR: &str = "operator";
/// [`TerminalSessionRecord::name_source`] value meaning "Claude Code invented
/// this name itself" (its `<dir>-<2hex>` auto-derivation).
pub const NAME_SOURCE_DERIVED: &str = "derived";

/// Map Claude Code's live-registry `nameSource` into the durable vocabulary.
///
/// The registry writes the key ONLY for its own auto-derivation; an operator
/// `/rename` is spelled by the key's ABSENCE. Storing that absence as `None`
/// would be a genuine ambiguity here, because `None` on the durable record
/// already means "never observed" — and under the stickiness rule a `None`
/// never overwrites, so a session that was auto-named and then `/rename`d
/// would keep its stale `"derived"` marker forever.
///
/// Resolving absence to an explicit [`NAME_SOURCE_OPERATOR`] at the moment of
/// observation removes both problems: the rename overwrites `"derived"`, and a
/// stored `None` keeps its one honest meaning, "not observed".
///
/// A present-but-unrecognized value is passed through VERBATIM rather than
/// collapsed — a future CLI variant is not this function's to interpret, and
/// consumers already apply the registry's own rule (anything but `"derived"` is
/// operator-chosen).
pub fn durable_name_source(registry_value: Option<&str>) -> String {
    match registry_value.map(str::trim).filter(|s| !s.is_empty()) {
        Some(v) => v.to_string(),
        None => NAME_SOURCE_OPERATOR.to_string(),
    }
}

/// Trim a possibly-empty incoming string to `None`.
///
/// The stickiness rule below reads `None` as "the writer reported nothing —
/// keep what is stored". An empty/whitespace-only string is the same statement
/// spelled differently, so it must normalize to `None` BEFORE the merge, or a
/// blank would clobber a known-good value (exactly the trap `config_dir`
/// already normalizes away at the top of [`SessionLifecycleStore::record_open`]).
fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|s| !s.trim().is_empty())
}

/// The `finished_at` a RESTAMP gets ([`SessionLifecycleStore::set_finished`]
/// re-finishing an already-finished mark): `now`, unless that EQUALS the old
/// stamp (two finishes inside one millisecond), in which case one past it.
///
/// The ACK matcher ([`SessionLifecycleStore::mark_finish_synced`]) needs the
/// two stamps only to DIFFER, so a late ACK of the old row can never sync the
/// new mark. Taking `now` otherwise — rather than `max(now, prev + 1)` — keeps
/// a clock stepped backwards from dating the new finish in the future. Pure.
fn restamp_at(prev: i64, now: i64) -> i64 {
    if now == prev {
        prev.saturating_add(1)
    } else {
        now
    }
}

/// Best-effort identity overlay for an existing record — the D1 "who is this
/// session" fields that live outside the spawn-time record.
///
/// Every field is `Option` and every `None` means **"not reported"**, never
/// "clear it": [`SessionLifecycleStore::update_identity`] merges stickily, so a
/// live-registry read that misses a session can never blank the account or name
/// the runner already knows. That is the same rule `config_dir` has always
/// followed, and it is what makes the stored identity survive the process that
/// produced it (Claude Code deletes its per-process registry file on exit).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionIdentityUpdate {
    /// Claude account label (`account.label`, e.g. `"gmail"`).
    pub account_label: Option<String>,
    /// CLI wrapper for that account (`account.wrapper`, e.g. `"clp"`).
    pub account_wrapper: Option<String>,
    /// The name the operator actually sees in the session window / `/resume`.
    pub session_name: Option<String>,
    /// Provenance of `session_name` (`"derived"` = Claude Code's own
    /// `<dir>-<2hex>` auto-name; anything else, including absent, = operator).
    pub name_source: Option<String>,
    /// Coord tenant this session was spawned under.
    pub tenant_id: Option<String>,
    /// Orchestration task-run id — set only for worker sessions.
    pub task_run_id: Option<String>,
    /// Whether this session runs with provider permissions bypassed.
    pub bypass_permissions: Option<bool>,
}

impl SessionIdentityUpdate {
    /// True when the overlay reports nothing at all — the caller can skip the
    /// store round-trip entirely.
    fn is_empty(&self) -> bool {
        self.account_label.is_none()
            && self.account_wrapper.is_none()
            && self.session_name.is_none()
            && self.name_source.is_none()
            && self.tenant_id.is_none()
            && self.task_run_id.is_none()
            && self.bypass_permissions.is_none()
    }

    /// Normalize every empty/whitespace-only string to `None` so the sticky
    /// merge treats "reported blank" and "reported nothing" identically.
    fn normalized(self) -> Self {
        Self {
            account_label: non_empty(self.account_label),
            account_wrapper: non_empty(self.account_wrapper),
            session_name: non_empty(self.session_name),
            name_source: non_empty(self.name_source),
            tenant_id: non_empty(self.tenant_id),
            task_run_id: non_empty(self.task_run_id),
            bypass_permissions: self.bypass_permissions,
        }
    }

    /// Apply the overlay to `rec` in place, sticky. Returns true iff anything
    /// actually changed (so callers can skip a durable write on a no-op tick).
    fn apply(&self, rec: &mut TerminalSessionRecord) -> bool {
        let mut changed = false;
        macro_rules! sticky {
            ($field:ident) => {
                if self.$field.is_some() && self.$field != rec.$field {
                    rec.$field = self.$field.clone();
                    changed = true;
                }
            };
        }
        sticky!(account_label);
        sticky!(account_wrapper);
        sticky!(session_name);
        sticky!(name_source);
        sticky!(tenant_id);
        sticky!(task_run_id);
        sticky!(bypass_permissions);
        changed
    }
}

/// One persisted terminal-session lifecycle record, keyed by
/// `claude_session_id`. Timestamps are unix epoch millis.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalSessionRecord {
    /// Stable Claude session id — the map key and the source of truth for
    /// session identity across restarts.
    pub claude_session_id: String,
    /// `CLAUDE_CONFIG_DIR` / config dir the session was launched under, if
    /// known.
    pub config_dir: Option<String>,
    /// Working directory of the session, if known.
    pub working_dir: Option<String>,
    /// The grid page this session's tile belongs to.
    pub page_id: String,
    /// The grid zone index this session's tile belongs to.
    pub zone_index: i32,
    /// Human-readable title shown in the grid tile, if known.
    pub title: Option<String>,
    /// The runner terminal id currently hosting this session.
    pub terminal_id: String,
    /// Unix millis when this session was first opened.
    pub opened_at: i64,
    /// Unix millis of the most recent liveness/touch.
    pub last_seen_at: i64,
    /// `"open"` | `"closed"`.
    pub state: String,
    /// Unix millis when this session was closed, if closed.
    pub closed_at: Option<i64>,
    /// Why the session was closed (e.g. `"poll-dead"`, an explicit reason).
    pub close_reason: Option<String>,
    /// Which AI-CLI provider owns this session (`"claude"`, `"gemini"`, …).
    /// `#[serde(default)]` makes every existing record — written before the
    /// runner was provider-aware — deserialize as `"claude"` (see
    /// [`default_provider`]); they are all Claude today.
    #[serde(default = "default_provider")]
    pub provider: String,
    /// How `claude_session_id` was bound:
    ///
    /// - `"authoritative"` — the runner KNOWS the id exactly (it pre-pinned
    ///   `--session-id`, lifted it from a typed `--resume`/`--session-id`, or
    ///   a provider hook POSTed it). Safe to auto-resume unattended.
    /// - `"reconciled"` — the id was recovered by a backstop (freshest-
    ///   transcript mtime / process-start-anchored reconcile) and may name a
    ///   foreign session. Restore treats it conservatively.
    ///
    /// Migration: on-disk records written by the previous schema carried this
    /// field as `bindOrigin` with the values `"pinned"`/`"guessed"`. The
    /// `alias = "bindOrigin"` reads the old JSON key, and [`load_map`]
    /// normalizes the legacy values (`pinned`→`authoritative`,
    /// `guessed`→`reconciled`) at load. Records predating the field entirely
    /// deserialize as `None`, which consumers read as `"reconciled"`.
    #[serde(default, alias = "bindOrigin", skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    /// Unix millis when a boot-restore began re-typing this session's
    /// `claude --resume` (set via [`SessionLifecycleStore::mark_restore_pending`],
    /// cleared via [`SessionLifecycleStore::clear_restore_pending`] once the
    /// resume handshake is verified). While set, the liveness poll must NEVER
    /// flip the record `poll-dead` — a restore whose resume command silently
    /// failed to land needs its durable `open` state intact for the next
    /// attempt (operator retry or next boot), not destroyed mid-restore. The
    /// marker is backend-owned and durable so a frontend crash mid-restore
    /// can't lose it. Self-heals: the poll clears a stale marker the moment it
    /// observes the session confidently alive (KeepAlive).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restore_pending_at: Option<i64>,
    /// The row's `last_seen_at` when it FIRST started waiting for an account
    /// choice ([`SessionLifecycleStore::mark_awaiting_account`]) — its last
    /// moment of genuine life. While set on an open, unfinished row and no
    /// older than the open-row prune age ([`awaiting_account_hold`]), it IS the
    /// hold: the row stays on the roster and in the restore set, and the poll
    /// never closes it `no-terminal`. The hold never refreshes `last_seen_at`,
    /// so a held row can never move the restore anchor. Durable, so the hold
    /// survives any number of restarts without a re-mark. Cleared by
    /// [`SessionLifecycleStore::record_open`] (a verified resume is new life),
    /// a close, Finish, and the poll once the hold has expired.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub awaiting_account_since: Option<i64>,
    /// Unix millis when a provider's SessionStart hook CONFIRMED that a real
    /// session actually started in this terminal (session-restore-redesign
    /// Phase 2 coordinator refinement). `None`/unset = PROVISIONAL.
    ///
    /// ## Why a provisional→confirmed distinction
    ///
    /// The spawn-time authoritative record ([`apply_identity_seam`]) is written
    /// for EVERY terminal — including a plain shell that never runs a provider
    /// (the runner can't know at spawn whether the user will type `claude`, run
    /// `ls`, or just sit at a prompt). Restoring those phantom shell "sessions"
    /// as `claude --resume <unused-uuid>` would manufacture failed resumes. The
    /// provider's SessionStart hook firing — POSTed to `/control/session-open`
    /// with `source:"startup"|"resume"` — is the observable proof that a REAL
    /// provider started here. The hook write sets this; the spawn-time write
    /// leaves it unset. Phase 4's restore classifier uses `confirmed_at`
    /// (OR a real transcript on disk) to gate auto-resume vs treat-as-plain-shell
    /// — this phase only PROVIDES the signal; it does NOT change the classifier.
    ///
    /// `#[serde(default)]`: every pre-Phase-2 on-disk record deserializes as
    /// `None` (provisional) — a confirming hook (or Phase 4's transcript check)
    /// re-establishes confirmation; nothing is lost, the field is purely
    /// additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirmed_at: Option<i64>,
    /// Coord-minted stable fleet session handle (`fsh_…`) for this session
    /// (session-identity fabric Phase 1, plan
    /// `2026-07-05-session-identity-messaging-restore-fabric.md` §4). Persisted
    /// next to `claude_session_id` so a restart re-presents the SAME handle on
    /// restore-rebind instead of re-minting. The registry is
    /// SERVER-authoritative — rebind is keyed on the durable
    /// `claude_session_id`, so a divergent local value is overwritten (server
    /// wins; see [`SessionLifecycleStore::set_handle`]).
    ///
    /// `#[serde(default)]`: every pre-fabric on-disk record deserializes as
    /// `None` (no handle acquired yet) — purely additive; legacy rows load
    /// cleanly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handle: Option<String>,
    /// Claude account label this session runs under (`account.label` from the
    /// live registry, e.g. `"gmail"`; falls back to the account derived from
    /// `config_dir` at spawn).
    ///
    /// ## Why it is stored at all
    ///
    /// The name and account an operator sees live ONLY in Claude Code's own
    /// per-process registry (`<config_dir>/sessions/<pid>.json`), which Claude
    /// Code DELETES when the process exits. After a restart the operator
    /// therefore cannot tell which account a restored session belongs to. The
    /// D1 fields below (`account_label` … `name_source`) copy that
    /// process-lifetime data into the durable record so it survives the process
    /// that produced it.
    ///
    /// ## Stickiness (the load-bearing rule)
    ///
    /// Written at three points — spawn, `POST /control/session-open`
    /// confirmation, and every liveness-poll tick that finds the session in the
    /// live registry. Last write wins, but a read that does NOT see the session
    /// must never blank a stored value: **absent ⇒ keep**. Enforced in
    /// [`SessionLifecycleStore::record_open`] and
    /// [`SessionLifecycleStore::update_identity`], both of which take an
    /// incoming value only when it is `Some` and non-blank. This is exactly the
    /// `config_dir` rule.
    ///
    /// `#[serde(default)]`: every record written before this field existed
    /// deserializes as `None` — purely additive, no on-disk migration. There is
    /// no legacy VALUE vocabulary to normalize at load (unlike `origin`), and
    /// no writer can persist a blank, because every write path normalizes
    /// empty→`None` first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_label: Option<String>,
    /// CLI wrapper for [`Self::account_label`] (`account.wrapper`, e.g.
    /// `"clp"`) — the half of the resume command that names the account, so a
    /// restored session can be reproduced verbatim. Sticky, see
    /// [`Self::account_label`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_wrapper: Option<String>,
    /// The session name the operator actually sees (the live registry's `name`
    /// — verbatim what the session window and `/resume` show). Sticky, see
    /// [`Self::account_label`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_name: Option<String>,
    /// Provenance of [`Self::session_name`], verbatim from the live registry's
    /// `nameSource`. `Some("derived")` marks Claude Code's own `<dir>-<2hex>`
    /// auto-name; anything else — INCLUDING absent — means the name is
    /// operator-chosen (`/rename`). Sticky, see [`Self::account_label`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_source: Option<String>,
    /// Coord tenant this session was spawned under (`Intent.tenant_id`, stamped
    /// from `terminal_create`). Already carried on the frontend `TerminalTab`;
    /// durable here so a restart still knows it. Sticky.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant_id: Option<String>,
    /// Orchestration task-run id — present only for WORKER sessions (the
    /// in-process FSM worker records itself keyed by its task run). Marks the
    /// record as machine-spawned rather than operator-opened. Sticky.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_run_id: Option<String>,
    /// Whether the session runs with provider permissions bypassed
    /// (`--dangerously-skip-permissions` / `--permission-mode
    /// bypassPermissions`) — the same signal the `terminal-bypass-permissions`
    /// Tauri event carries to the frontend. Safety-relevant, so it belongs on
    /// the durable identity record rather than only in process-lifetime UI
    /// state. Sticky.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bypass_permissions: Option<bool>,
    /// Unix millis when a BOOT-RESTORE re-materialized this record, as opposed
    /// to the session being freshly created. Set by the boot-restore path via
    /// [`SessionLifecycleStore::mark_restored_from_boot`].
    ///
    /// Load-bearing for the restore census: it is the only marker that
    /// distinguishes "this session came back" from "this session was opened
    /// again", and the census's `restored` set is exactly the records carrying
    /// it. Backend-owned and durable (like `restore_pending_at`) so a frontend
    /// crash mid-restore cannot lose it. Sticky — a later re-record must not
    /// erase the fact that the record was restored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restored_from_boot_at: Option<i64>,
    /// The honest tier the restore achieved: [`RESTORE_TIER_RESUMED`],
    /// [`RESTORE_TIER_TERMINAL_ONLY`] or [`RESTORE_TIER_FAILED`]. Mirrors the
    /// frontend `TerminalTab`'s `resumeFailed` / `restoreTerminalOnly` flags,
    /// which are process-lifetime only. `None` = this record was never restored
    /// (or predates the field) — which is a DIFFERENT statement from
    /// `Some("failed")` and must not be collapsed into it. Sticky.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restore_tier: Option<String>,
    /// Unix millis when this session was marked **FINISHED** — the operator (or
    /// `/unattended`) declaring its work complete. `None` = not finished.
    ///
    /// ## What it is, and what it is NOT
    ///
    /// This is the WORK axis, and it is **orthogonal to `state`**. A live
    /// `open` session can be finished (its work is done; the terminal is still
    /// there), and a `closed` session is usually NOT finished (it crashed, or
    /// the PTY exited mid-task). Collapsing the two is the mistake this field
    /// exists to avoid — `state`/`close_reason` answer *is it running*, this
    /// answers *is there anything left to do*.
    ///
    /// Marking is **pure metadata**: it never touches the process. A finished
    /// session keeps running until it exits on its own, and unmarking restores
    /// it exactly.
    ///
    /// ## Why it changes restore
    ///
    /// [`SessionLifecycleStore::restorable_records`] excludes it, so a rebuilt
    /// runner brings back only the UNFINISHED sessions. That is the entire point
    /// of plan `2026-09-01-session-finished-marker-and-unfinished-resume`.
    ///
    /// Sticky, like every field above it: absent ⇒ keep. Clearing it requires an
    /// explicit unmark ([`SessionLifecycleStore::set_finished`] with `None`),
    /// never a report that merely omits it.
    ///
    /// `#[serde(default)]` is load-bearing rather than decorative: every record
    /// already on disk predates this field, and without it the WHOLE registry
    /// fails to deserialize on the first boot after the upgrade — losing exactly
    /// the sessions this feature exists to preserve.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<i64>,
    /// What the drained-runner wind-down executor did to this session (plan
    /// `2026-09-13-drained-runner-never-reaches-idle`, Phase 4). One of the
    /// FIVE `WIND_DOWN_*` words, and they say different things:
    ///
    /// * [`WIND_DOWN_CLOSED`] — `claude` left and the pane was closed;
    /// * [`WIND_DOWN_EXIT_STUCK`] — `claude` outlived the deadline, nothing was
    ///   killed, the session is STILL RUNNING;
    /// * [`WIND_DOWN_CLOSE_REFUSED`] — `claude` left, but the pane could not be
    ///   proven clear at the close, so the tab was left open;
    /// * [`WIND_DOWN_CLOSE_UNKNOWN`] — `claude` left and the close was
    ///   attempted, and what it achieved is NOT known. This one asserts nothing
    ///   about the pane either way and must not be read as either of the two
    ///   above;
    /// * [`WIND_DOWN_NOT_ATTEMPTED`] — `/exit` was never submitted (the pane was
    ///   not in a safe state, or the write failed). Nothing was closed, but the
    ///   pane MAY have been written to.
    ///
    /// It is the OUTCOME axis of an automatic close, and it is orthogonal to
    /// both `state` and `finished_at`: a session can be `closed` for a dozen
    /// other reasons, and an `exit_stuck` one is still `open` and still
    /// running. `None` = wind-down never acted on this record, which is a
    /// different statement from "it tried and failed" and must not be collapsed
    /// into it.
    ///
    /// Sticky, like every field around it. `#[serde(default)]`: every record
    /// already on disk predates the field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wind_down_outcome: Option<String>,
    /// Unix millis when [`Self::wind_down_outcome`] was recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wind_down_at: Option<i64>,
    /// WHICH executor arm produced [`Self::wind_down_outcome`]:
    /// [`WIND_DOWN_ARM_DRAIN`] or [`WIND_DOWN_ARM_FINISHED_CLOSE`]. Written
    /// together with the outcome, so the two always describe the same act.
    /// `None` on a record no arm acted on — and on every outcome recorded
    /// before the field existed, all of which were the drain arm's.
    ///
    /// Not [`Self::origin`]: that is session-id bind provenance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wind_down_arm: Option<String>,
    /// Free-text reason recorded alongside [`Self::finished_at`] (e.g.
    /// `"unattended: 6 units, all landed"`). Sticky; searchable in the
    /// Previous-sessions view.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<String>,
    /// `true` once coord has ACKed this session's finished marker.
    ///
    /// The local mark is authoritative for RESUME the instant it is written —
    /// the runner must never wait on the network to know what to bring back —
    /// but coord is authoritative for the FACT. This flag is what lets the two
    /// disagree safely: the boot reconcile may clear a local mark that coord
    /// contradicts **only when it is synced**, because an unsynced mark is a
    /// write coord has not seen yet and reverting it would silently undo the
    /// operator's action.
    ///
    /// `false` on a finished record therefore means "coord write still owed".
    /// The write is enqueued when the mark is made, again when a re-finish
    /// RESTAMPS it (`set_finished` with a reason, which clears this flag), again
    /// on any no-op re-finish of the still-unsynced mark (`set_finished`'s retry
    /// door), AND
    /// again when the session next gets a coord row in this process — a FRESH
    /// registration (`AiCoordRegistrar::register_inner`) or, for the terminal
    /// plane, its terminal being bound to its coord row
    /// (`AiCoordRegistrar::deliver_owed_finish`). That late delivery covers
    /// only the IN-PROCESS window between `record_open` and the coord bind: a
    /// finished session is not restored on boot, so no terminal re-binds for
    /// it after a runner restart and a mark still unsynced then is carried
    /// only by an outbox row already queued (or a re-run). A write coord
    /// REFUSES (4xx) is ACK-dropped and not retried until the next
    /// registration or re-run — the flag stays `false`, which is the honest
    /// reading.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub finish_synced: bool,
    /// The machine's default tenant for new sessions AS READ WHEN THIS SESSION
    /// WAS SPAWNED (plan
    /// `2026-09-10-spawn-tenant-never-reaches-the-session-coord-credential`,
    /// review follow-up W-B).
    ///
    /// It is the expected tenant of a spawn that chose none, and the session-info
    /// tenancy report compares a tenant-less session against it. It used to
    /// live only in a process-lifetime map, so after a runner restart a restored
    /// session was compared against the RESTORE-time default — a default switch
    /// between the two made a correct session read diverged, or a wrong one read
    /// agreeing. Persisting it here, beside the record restore already reads,
    /// keeps the value of the spawn.
    ///
    /// `None` = NOT RECORDED (a record written before the field, or a session
    /// whose spawn this runner did not see) — reported as UNKNOWN, never as
    /// agreement. `Some({tenantId: None})` = recorded, and the machine named no
    /// default then. SET ONCE: [`SessionLifecycleStore::record_spawn_device_default`]
    /// writes it only onto a record the spawn itself created, and neither a
    /// re-record nor a restore re-spawn overwrites it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spawn_device_default: Option<SpawnDeviceDefault>,
    /// The harness id this session was ADOPTED FROM — the session that most
    /// recently started on the same terminal
    /// ([`SessionLifecycleStore::latest_open_on_terminal`]) when the provider's
    /// SessionStart hook reported this id with `source: "clear"` (`/clear` inside a running
    /// provider mints a new harness id inside the SAME process).
    /// [`SessionLifecycleStore::mark_adopted_from`] is the one writer.
    ///
    /// It records WHOSE session was cleared, not merely which terminal it
    /// ran on. Every restored session is a typed `claude --resume X` in a
    /// pane pinned to some OTHER id P, so the terminal alone cannot say
    /// whether a `/clear` continued P (the pane's own coord row) or X (X's
    /// own row, registered by `AiCoordRegistrar::register_sniffed_session`).
    /// A finished marker therefore resolves its coord row as its predecessor
    /// does, walking this chain
    /// ([`crate::claude_session::coord_register::resolve_adoption_chain`]).
    ///
    /// Not recorded — so the session resolves by its own id alone, and its
    /// mark stays local-only until that id has a coord row — when the id
    /// changed without a `clear` hook: the in-process `/resume` picker, or an
    /// adoption whose hook posted `startup` (the Windows `.cmd` shim).
    ///
    /// Sticky: written only by a `clear` hook for this id, never rewritten by
    /// `record_open`. `None` = not adopted, or the record predates the
    /// field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adopted_from: Option<String>,
    /// Unix millis of the most recent provider SessionStart hook that reported
    /// this id ([`SessionLifecycleStore::note_session_start_hook`]) — when its
    /// process last started (or resumed into) its terminal. The durable half of
    /// [`SessionLifecycleStore::latest_open_on_terminal`]'s ranking, so a pane
    /// whose operator ran `/resume` still resolves to the resumed session after
    /// a restart. `None` = no hook seen, or the record predates the field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_session_start_at: Option<i64>,
}

/// See [`TerminalSessionRecord::spawn_device_default`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpawnDeviceDefault {
    /// The default tenant read at spawn, or `None` when the machine named none.
    pub tenant_id: Option<String>,
    /// The machine's pin could NOT be read at spawn (`machine.json` unresolvable).
    /// Kept so the report says UNKNOWN (`unresolvable`) rather than reading the
    /// absent tenant as "no default". Set once like the rest of the value.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub unresolvable: bool,
}

/// One durable, self-contained mutation appended to the write-ahead log.
///
/// The registry used to durably record a mutation by rewriting the WHOLE map
/// (`to_vec_pretty` → `.json.tmp` → `rename`), making every terminal spawn
/// O(total sessions) and the aggregate O(N²). A delta is O(1) in the number
/// of sessions: one `write_all` of one line.
///
/// Both variants are IDEMPOTENT and last-writer-wins per key, so replaying a
/// suffix that the snapshot already contains (the crash-between-snapshot-and-
/// truncate window) is harmless.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "camelCase")]
enum LifecycleDelta {
    /// Insert-or-replace the record under its own `claude_session_id`.
    /// `Box`ed so the enum stays small (the record is ~20 fields).
    Upsert { rec: Box<TerminalSessionRecord> },
    /// Delete the record stored under `id`.
    Remove { id: String },
}

/// Append this many deltas before forcing a compaction, regardless of whether
/// the idle/heartbeat compaction has had a chance to run. Bounds both the WAL
/// file size and the boot replay cost under a spawn burst.
const WAL_COMPACT_APPENDS: usize = 512;

/// Open-in-append WAL handle plus its compaction bookkeeping.
///
/// The handle is kept open across appends (re-opening per mutation would put a
/// directory lookup + create back on the spawn path) and is dropped on
/// compaction, which truncates the file.
#[derive(Debug, Default)]
struct WalWriter {
    file: Option<std::fs::File>,
    /// Deltas appended since the last successful compaction.
    appends: usize,
    /// A mutation landed in the in-memory map but did NOT reach the WAL
    /// (serialize failure, open failure, `write_all` failure, poisoned lock).
    ///
    /// Without this the store would be silently dirty-but-uncounted: the map is
    /// already mutated, `appends` never moved, and `compact_if_dirty` gates on
    /// `appends > 0` — so if that mutation were the LAST one before a crash it
    /// would be lost outright. (The whole-map rewrite this replaced had no such
    /// hole: the next mutation's rewrite carried the earlier one along.) Cleared
    /// by a successful [`SessionLifecycleStore::compact`], which writes the
    /// whole map and therefore captures it.
    dirty: bool,
}

/// Durable map of `claude_session_id -> TerminalSessionRecord`.
///
/// Cheap to clone-share via `Arc`. Durability is a **write-ahead log of
/// per-record deltas** (`terminal-sessions.wal.jsonl`) compacted into the JSON
/// snapshot (`terminal-sessions.json`); a mutation costs one appended line, not
/// a whole-map rewrite. See [`LifecycleDelta`] and [`SessionLifecycleStore::compact`].
#[derive(Debug)]
pub struct SessionLifecycleStore {
    path: PathBuf,
    /// Sibling write-ahead log — `<path>` with the extension replaced by
    /// `wal.jsonl`. Derived once at [`Self::open`].
    wal_path: PathBuf,
    /// WAL append handle.
    ///
    /// LOCK ORDER — `map` then `wal`, never the reverse. Every delta is written
    /// while its mutation still holds the map lock, so the WAL order is exactly
    /// the mutation order and replay is last-writer-wins with no sequence
    /// numbers. (The old whole-map `persist` ran OUTSIDE the lock and could
    /// therefore let a stale snapshot land after a fresher one, silently losing
    /// a mutation; deltas under the lock remove that race outright.)
    /// [`Self::compact`] takes both in the same order, so an appender is either
    /// entirely inside the snapshot it truncates or entirely after it.
    wal: Mutex<WalWriter>,
    map: Mutex<HashMap<String, TerminalSessionRecord>>,
    /// Terminal ids whose session has MOVED to another terminal while the
    /// old PTY was still alive — the account-migration order is spawn the
    /// replacement first, then close the source, so for a moment two PTYs
    /// carry one session. [`Self::record_open`] re-keys the row onto the new
    /// terminal; the old PTY's later exit-close names `(session id, old
    /// terminal)`, which no open row owns any more, and would otherwise read
    /// as a MIS-BOUND close (`NotFound`, a `warn!`) on the fleet's most
    /// routine interruption. [`Self::mark_terminal_superseded`] records the
    /// hand-off and [`Self::record_close_checked`] answers that close with
    /// [`CloseOutcome::TerminalSuperseded`] — no mutation, no observer, no nonce
    /// release, because the session lives on. Process-local by design: a
    /// superseded PTY cannot outlive the runner that spawned it, so there is
    /// nothing to persist. Consumed on first match.
    superseded_terminals: Mutex<HashSet<String>>,
    /// Process-local ORDER of provider SessionStart hooks: a monotonic counter
    /// and, per session id, the counter value of the most recent hook that
    /// reported it ([`Self::note_session_start_hook`]). It answers "which of the
    /// open sessions on this terminal STARTED most recently", which
    /// [`Self::latest_open_on_terminal`] needs: `confirmed_at` is monotonic (a
    /// typed `claude --resume X` of a session confirmed earlier keeps X's OLD
    /// stamp) and `last_seen_at` is bumped by every liveness touch of every
    /// open record. The durable twin is
    /// [`TerminalSessionRecord::last_session_start_at`], which carries the
    /// order across a restart — a holder-adopted pane fires no new hook. This
    /// counter only breaks that stamp's ties and wall-clock steps.
    /// LOCK ORDER — `map` then this, never the reverse.
    session_start_hooks: Mutex<(u64, HashMap<String, u64>)>,
    /// Optional write-only sink for the append-only snapshot HISTORY
    /// ([`crate::session::snapshot_history`], Phase 4 of the session-restore
    /// shim-fix plan). When attached, every layout-meaningful mutation
    /// (open/close/rename/rekey/remove/confirm) appends one complete
    /// snapshot of the registry to the durable JSONL audit trail. The
    /// history is DERIVED from this store — never read back by it or by the
    /// restore path.
    snapshot_history: OnceLock<Arc<SnapshotHistory>>,
    /// Optional observation epoch ([`crate::session::observation_epoch`]): the
    /// last instant the liveness poll could see the process table, for the
    /// prior process (latched at boot) and for this one. Restore admission
    /// bounds the open-row anchor by it ([`RestoreAnchors`]). Unattached →
    /// no prior epoch, and this process has never observed.
    observation_epoch: OnceLock<Arc<ObservationEpoch>>,
    /// Whether the prior shutdown marker attests a FINISHED teardown
    /// ([`crate::session::shutdown_marker::BootClassification::teardown_complete`]),
    /// attached at boot beside the epoch. It decides whether the marker may
    /// anchor open rows ([`RestoreAnchors`]). Unattached → the marker the
    /// caller passes is used as it always was.
    prior_teardown_complete: OnceLock<bool>,
    /// Optional restore-registry → coord mirror (plan
    /// `2026-07-09-runner-session-history-cloud-sync` §3.4, Phase 4). When
    /// attached, every open-record write/refresh ([`record_open`](Self::record_open))
    /// and every confirmation flip ([`confirm_session`](Self::confirm_session)
    /// — confirmation changes the record's honest `restore_tier`) hands the
    /// merged record to the emitter, which gates (`session_metadata_sync_enabled`),
    /// debounces on the material wire fields, and enqueues a
    /// `restore-record` session event via the outbox. Best-effort by
    /// construction: the emitter swallows every failure and never blocks the
    /// registry write on the network (the drain loop does the pushing).
    /// Unattached (tests, ephemeral fallbacks) → no-op.
    restore_emitter: OnceLock<Arc<RestoreRecordEmitter>>,
    /// Optional transcript-existence probe. Read at two points: write time, to
    /// stamp every snapshot-history entry ([`SnapshotSession::restorable`] /
    /// `transcript_exists`), and list time, by callers going through
    /// [`Self::probe_transcript_exists`].
    ///
    /// The registry's own SELECTION logic never consults it: `restorable_records`
    /// and the reconcile paths are unchanged by an attached probe, so it cannot
    /// add or remove restore candidates. What it does feed is the honest
    /// restore TIER the frontend classifier picks — a candidate with no
    /// transcript is still restored as a terminal, just never `--resume`d.
    /// Unattached → every read is `None` ("not probed"), never `false`.
    transcript_probe: OnceLock<Arc<dyn TranscriptProbe>>,
    /// Optional close observer (session-identity fabric Phase 3, review W2):
    /// invoked with the `claude_session_id` AFTER a record actually flips
    /// open→closed (never on a repeat/absent close). main.rs attaches a
    /// closure that calls `AiCoordRegistrar::close_session` (through a Weak,
    /// avoiding the Arc cycle with the registrar's attached store), so
    /// sniffed-registered sessions don't accrue as never-closing
    /// `coord.sessions` rows when their terminal dies. A csid the registrar
    /// never registered no-ops inside `close_session` (index miss).
    /// Unattached (tests, ephemeral fallbacks) → no-op.
    close_observer: OnceLock<CloseObserver>,
    /// Optional finish observer (plan
    /// `2026-09-01-session-finished-marker-and-unfinished-resume` §5.2):
    /// invoked with the resulting record AFTER [`Self::set_finished`] durably
    /// writes a change coord must hear about — a mark coord has not ACKed yet
    /// (`finished_at: Some`), or an unmark (`finished_at: None`). main.rs
    /// attaches a closure that enqueues the matching outbox row through
    /// `AiCoordRegistrar::finish_session` / `unfinish_session`, which is what
    /// makes coord authoritative for the marker rather than blind to it — and
    /// keeps an unmark from leaving coord saying `finished`. Unattached
    /// (tests, ephemeral fallbacks) → every change stays local-only.
    finish_observer: OnceLock<FinishObserver>,
    /// Unix millis at which THIS runner process booted — the line between the
    /// PRIOR process's rows and this one's own activity. The restore anchor
    /// ([`RestoreAnchors`]) judges a row stamped before it against the prior
    /// process's last moment of life, so a restore this process performs (a
    /// `record_open` re-assert stamps `last_seen_at = now`) can never move the
    /// window the rest of that cohort is measured against. Production:
    /// [`crate::session::session_ledger::process_boot_ms`]. Tests:
    /// `i64::MAX` (every fixture row is the prior life) unless a test sets it
    /// with [`Self::set_boot_ms_for_test`].
    boot_ms: AtomicI64,
    /// Serializes [`Self::set_finished`] END TO END — the map mutation AND the
    /// finish-observer hand-off — so the order coord's outbox receives
    /// finished / working rows in is the order the local marker changed in.
    ///
    /// Without it the observer runs after the map guard is released, so a
    /// finish (or a no-op retry carrying a snapshot) and a concurrent unmark
    /// could enqueue in the opposite order to their local writes, leaving
    /// coord `finished` while the local record is unmarked. Taken BEFORE the
    /// map lock, and never taken by the observer or anything it calls, so it
    /// adds no lock cycle.
    ///
    /// The registrar's late-delivery paths (`AiCoordRegistrar::register_inner`'s
    /// re-offer and `deliver_owed_finish`) read and enqueue through
    /// [`SessionLifecycleStore::with_unsynced_finished_records`], which holds
    /// this same lock, so a late delivery cannot land its `Finished` after a
    /// concurrent unmark's `working` either.
    finish_forward: Mutex<()>,
}

/// Boxed close-observer callback (see `SessionLifecycleStore::close_observer`).
/// Newtype so the store can keep `#[derive(Debug)]`.
struct CloseObserver(Box<dyn Fn(&str) + Send + Sync>);

/// Boxed finish-observer callback (see `SessionLifecycleStore::finish_observer`).
/// Returns whether the change it was told about reached the coord outbox.
struct FinishObserver(Box<dyn Fn(&TerminalSessionRecord) -> FinishSync + Send + Sync>);

/// Whether a finished-marker change was handed to coord — the verdict
/// `POST /sessions/{id}/finish` reports, so a local-only mark can never read
/// as a coord write.
///
/// Decided at ENQUEUE time: `Queued` means the outbox holds a row addressed to
/// `coord_session_id`, not that coord has applied it. `finishSynced` on the
/// record (stamped by the drain's ACK) is the later, stronger answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinishSync {
    /// An outbox row addressed to this coord session is queued.
    Queued { coord_session_id: uuid::Uuid },
    /// Nothing was queued; the change is local-only, for this reason.
    LocalOnly(LocalOnlyReason),
}

/// Why a finished-marker change stayed local-only. See [`FinishSync`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalOnlyReason {
    /// No coord session id resolves for this session on any runner plane.
    NoCoordSession,
    /// A coord id resolved, but the outbox write failed.
    OutboxWriteFailed,
    /// Nothing forwarded the change: no finish observer is attached (tests,
    /// ephemeral stores), or the registrar it forwards to has been dropped.
    NoForwarder,
    /// The mark is already synced (coord ACKed it), so no coord write was
    /// owed and none was queued: a no-op re-run on a synced mark, or a no-op
    /// unmark.
    NotRequeued,
}

impl LocalOnlyReason {
    /// The wire spelling (`coord.reason` in the finish route's response).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NoCoordSession => "no_coord_session",
            Self::OutboxWriteFailed => "outbox_write_failed",
            Self::NoForwarder => "no_forwarder",
            Self::NotRequeued => "not_requeued",
        }
    }
}

/// What a [`SessionLifecycleStore::set_finished`] call actually changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinishChange {
    /// The marker itself was written: a fresh mark, a RESTAMP of an existing
    /// one (a re-finish that carried a reason — `finished_at` moved to now and
    /// coord is owed the write again), or an unmark.
    Marker,
    /// Nothing was written — the marker was already in the requested state and
    /// no reason was sent (a read-back, or the retry door). The timestamp did
    /// not move.
    None,
}

impl FinishChange {
    /// The wire spelling (`changed` in the finish route's response).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Marker => "marker",
            Self::None => "none",
        }
    }
}

/// Why [`SessionLifecycleStore::set_finished`] produced no [`FinishOutcome`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetFinishedError {
    /// The id is unknown to this lifecycle registry — and ONLY that. The
    /// finish route answers `404`.
    UnknownSession,
    /// The registry could not be read (its lock is poisoned), so whether the
    /// id is known is itself unknown. Never collapsed into
    /// [`Self::UnknownSession`]: the finish route answers `500`.
    Unavailable,
}

/// The result of a [`SessionLifecycleStore::set_finished`] call on a KNOWN
/// session: the record as it now stands, what changed, and whether coord was
/// told.
#[derive(Debug, Clone)]
pub struct FinishOutcome {
    pub record: TerminalSessionRecord,
    pub changed: FinishChange,
    pub coord: FinishSync,
}

impl FinishOutcome {
    /// The wire body both finish doors answer with — `POST
    /// /sessions/{id}/finish` and the `terminal_session_set_finished` Tauri
    /// command:
    ///
    /// ```json
    /// {"success": true, "session": <record>,
    ///  "changed": "marker" | "none",
    ///  "coord": {"queued": bool, "coordSessionId": <uuid|null>,
    ///            "reason": null | "no_coord_session" | "outbox_write_failed"
    ///                      | "no_forwarder" | "not_requeued"}}
    /// ```
    ///
    /// `coord.queued` is decided at enqueue time; `session.finishSynced` is
    /// the later verdict the drain's ACK stamps. A caller that must know coord
    /// APPLIED the mark re-reads `finishSynced`.
    ///
    /// `changed: "marker"` on an already-finished session is a RESTAMP: the
    /// mark's `finishedAt` moved to now and `finishSynced` is `false` again
    /// until coord ACKs the re-sent write. That write carries the new time
    /// (as `last_progress_at`), never the reason text: the `Finished` row's
    /// payload carries no reason, so a reason lives in the local record only.
    pub fn response_json(&self) -> serde_json::Value {
        let coord = match self.coord {
            FinishSync::Queued { coord_session_id } => serde_json::json!({
                "queued": true,
                "coordSessionId": coord_session_id.to_string(),
                "reason": null,
            }),
            FinishSync::LocalOnly(reason) => serde_json::json!({
                "queued": false,
                "coordSessionId": null,
                "reason": reason.as_str(),
            }),
        };
        let mut session = serde_json::to_value(&self.record).unwrap_or_default();
        // The record omits `finishSynced` when false (compact on disk); the
        // response ALWAYS carries it, so a caller never has to read an absent
        // key as `false` — absence would be indistinguishable from a build
        // that predates the field.
        if let Some(obj) = session.as_object_mut() {
            obj.insert(
                "finishSynced".to_string(),
                serde_json::Value::Bool(self.record.finish_synced),
            );
        }
        serde_json::json!({
            "success": true,
            "session": session,
            "changed": self.changed.as_str(),
            "coord": coord,
        })
    }
}

impl std::fmt::Debug for FinishObserver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("FinishObserver")
    }
}

impl std::fmt::Debug for CloseObserver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CloseObserver")
    }
}

impl SessionLifecycleStore {
    /// Open (or initialize) the store at `path`: load the JSON snapshot, then
    /// replay the sibling write-ahead log over it.
    ///
    /// A TORN TAIL (the process died mid-append) is dropped, never fatal — see
    /// [`replay_wal`]. When the replay found anything it could not use (a torn
    /// tail or an unparsable line), the store is compacted immediately so the
    /// next append can never be concatenated onto a partial line.
    pub fn open(path: impl AsRef<Path>) -> std::io::Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let wal_path = wal_path_for(&path);
        let mut map = load_map(&path);
        let replay = replay_wal(&wal_path, &mut map);
        // A held row is unbound by rule; a file written before the rule (or by
        // hand) may still carry its old terminal id.
        let detached = detach_held_rows(&mut map, Utc::now().timestamp_millis());
        let store = Self {
            path,
            wal_path,
            wal: Mutex::new(WalWriter::default()),
            map: Mutex::new(map),
            superseded_terminals: Mutex::new(HashSet::new()),
            session_start_hooks: Mutex::new((0, HashMap::new())),
            snapshot_history: OnceLock::new(),
            observation_epoch: OnceLock::new(),
            prior_teardown_complete: OnceLock::new(),
            restore_emitter: OnceLock::new(),
            transcript_probe: OnceLock::new(),
            close_observer: OnceLock::new(),
            finish_observer: OnceLock::new(),
            boot_ms: AtomicI64::new(default_boot_ms()),
            finish_forward: Mutex::new(()),
        };
        if replay.applied > 0 || replay.damaged {
            info!(
                applied = replay.applied,
                damaged = replay.damaged,
                path = %store.wal_path.display(),
                "session_lifecycle_store: replayed write-ahead log — folding into the snapshot"
            );
        }
        if detached > 0 {
            info!(
                detached,
                "session_lifecycle_store: detached held rows from their old terminals at load"
            );
        }
        if replay.applied > 0 || replay.damaged || detached > 0 {
            store.compact();
        }
        Ok(store)
    }

    /// Attach the close observer (once, at startup) — invoked with the
    /// `claude_session_id` after every real open→closed transition
    /// ([`Self::record_close`]). See the field doc for the production wiring
    /// (coord session-row closure, fabric Phase 3 / review W2). Without it
    /// every close is local-only, the pre-Phase-3 behavior.
    pub fn attach_close_observer(&self, f: impl Fn(&str) + Send + Sync + 'static) {
        if self.close_observer.set(CloseObserver(Box::new(f))).is_err() {
            warn!("session_lifecycle_store: close observer already attached — ignoring");
        }
    }

    /// Attach the finish observer (once, at startup) — invoked with the
    /// resulting record after every [`Self::set_finished`] write that owes
    /// coord a write: an unsynced mark, or an unmark. See the field doc for
    /// the production wiring. Without it every change is local-only.
    pub fn attach_finish_observer(
        &self,
        f: impl Fn(&TerminalSessionRecord) -> FinishSync + Send + Sync + 'static,
    ) {
        if self
            .finish_observer
            .set(FinishObserver(Box::new(f)))
            .is_err()
        {
            warn!("session_lifecycle_store: finish observer already attached — ignoring");
        }
    }

    /// Attach the append-only snapshot-history sink (once, at startup).
    /// Without an attached sink every history hook is a no-op — the store
    /// works standalone (tests, ephemeral fallbacks).
    pub fn attach_snapshot_history(&self, history: Arc<SnapshotHistory>) {
        if self.snapshot_history.set(history).is_err() {
            warn!("session_lifecycle_store: snapshot history already attached — ignoring");
        }
    }

    /// Attach the observation epoch (once, at startup, BEFORE the restore
    /// census latches — it is an admission input).
    pub fn attach_observation_epoch(&self, epoch: Arc<ObservationEpoch>) {
        if self.observation_epoch.set(epoch).is_err() {
            warn!("session_lifecycle_store: observation epoch already attached — ignoring");
        }
    }

    /// The attached observation epoch — the liveness poll records its
    /// successful ticks through it.
    pub fn observation_epoch(&self) -> Option<&Arc<ObservationEpoch>> {
        self.observation_epoch.get()
    }

    /// Attach whether the prior shutdown's teardown finished (once, at
    /// startup, BEFORE the restore census latches — it is an admission input).
    pub fn attach_prior_teardown_complete(&self, complete: bool) {
        if self.prior_teardown_complete.set(complete).is_err() {
            warn!("session_lifecycle_store: prior teardown bit already attached — ignoring");
        }
    }

    /// Attach the restore-record cloud-mirror emitter (once, at startup).
    /// Without an attached emitter the mirror hooks are no-ops — the store
    /// works standalone (tests, ephemeral fallbacks).
    pub fn attach_restore_record_emitter(&self, emitter: Arc<RestoreRecordEmitter>) {
        if self.restore_emitter.set(emitter).is_err() {
            warn!("session_lifecycle_store: restore-record emitter already attached — ignoring");
        }
    }

    /// Attach the transcript-existence probe (once, at startup). It stamps the
    /// restorability tuple onto snapshot-history entries AND answers
    /// [`Self::probe_transcript_exists`] for the boot-restore projection. Without
    /// it both read "not probed" — the store works standalone.
    pub fn attach_transcript_probe(&self, probe: Arc<dyn TranscriptProbe>) {
        if self.transcript_probe.set(probe).is_err() {
            warn!("session_lifecycle_store: transcript probe already attached — ignoring");
        }
    }

    /// Ask the attached transcript probe whether `session_id` has a provider
    /// transcript on disk.
    ///
    /// `None` means NOT PROBED (no probe attached) and callers MUST treat it as
    /// UNKNOWN — never as "absent".
    ///
    /// This is what lets the boot-restore projection tell an id whose
    /// conversation exists from one whose does not. `confirmed_at` cannot: it
    /// attests only that a provider session STARTED in the terminal (its
    /// SessionStart hook fired), while the transcript is written once the session
    /// carries messages — so a session launched and never used is confirmed
    /// forever with no transcript, and `claude --resume <id>` on it fails with
    /// "No conversation found" on every boot.
    ///
    /// Also `None` when `working_dir` is absent OR BLANK. A transcript path is
    /// DERIVED from the project dir, so without one the probe has nothing to
    /// stat — [`crate::session::reconcile::DiskTranscriptIndex`] answers a bare
    /// `false` there, which is "could not determine", NOT "does not exist".
    /// Gating on that raw `false` would downgrade a working-dir-less record to
    /// terminal-only and silently lose a resumable conversation, so the
    /// unanswerable case is mapped back to UNKNOWN here.
    ///
    /// Blank matters as much as absent because `""` is reachable: the
    /// hook-confirm merge writes `prior.working_dir.clone().unwrap_or_default()`
    /// (`install_effects_producer`), and unlike `config_dir` the registry does
    /// not normalize an empty `working_dir` away. An empty project path encodes
    /// to `""`, which stats a path that can never exist — a `false` that says
    /// nothing about the session.
    ///
    /// ## Known residual false-negative paths
    ///
    /// A `Some(false)` here is "the probe looked and found nothing at the path it
    /// derived", which is not quite "no transcript exists anywhere". Two ways to
    /// derive the wrong path, both fail-CLOSED (demote to terminal-only) rather
    /// than fail-open:
    ///
    /// 1. The transcript's location follows the PROVIDER PROCESS's cwd, while
    ///    this record carries the PTY's spawn cwd. `cd` then launch, and they
    ///    diverge permanently — the hook-confirm merge deliberately preserves
    ///    `prior.working_dir` (see the 2026-07-13 stranding incident), so it
    ///    never self-heals.
    /// 2. [`crate::session::reconcile::DiskTranscriptIndex::discover`] snapshots
    ///    the config-dir set ONCE at startup and ignores the record's own
    ///    `config_dir`, so a transcript under an account dir added later is
    ///    invisible. Contrast `past_sessions::resolve_transcript_path`, which
    ///    tries the record's `config_dir` first and re-discovers per call.
    ///
    /// Both degrade a restore to terminal-only (right cwd, fresh conversation)
    /// and neither loses data — the session stays resumable by hand from the
    /// Past Sessions surface, which uses the better resolver. Answering by
    /// SESSION ID across all project dirs (a walk of every project dir under
    /// every config dir) would remove the class outright and is the follow-up
    /// worth doing before anything DESTRUCTIVE (e.g. pruning transcript-less
    /// rows) is gated on this.
    pub fn probe_transcript_exists(
        &self,
        session_id: &str,
        working_dir: Option<&str>,
    ) -> Option<bool> {
        let working_dir = working_dir.filter(|s| !s.trim().is_empty())?;
        self.transcript_probe
            .get()
            .map(|p| p.transcript_exists(session_id, Some(working_dir)))
    }

    /// WHICH config dirs hold `session_id`'s transcript — the account
    /// evidence the boot restore uses when a record carries no `config_dir`.
    ///
    /// Guarded exactly like [`Self::probe_transcript_exists`]: `None` (UNKNOWN)
    /// for a missing/blank `working_dir`, for no attached probe, and for a
    /// probe that answers existence only. `Some(vec![])` is a real "no dir
    /// holds it". Shares that method's documented blind spots (the provider
    /// cwd vs the PTY cwd, and a config-dir set snapshotted at startup).
    pub fn probe_transcript_config_dirs(
        &self,
        session_id: &str,
        working_dir: Option<&str>,
    ) -> Option<Vec<std::path::PathBuf>> {
        let working_dir = working_dir.filter(|s| !s.trim().is_empty())?;
        self.transcript_probe
            .get()?
            .transcript_config_dirs(session_id, Some(working_dir))
    }

    /// Project the registry into snapshot entries, stamping restorability from
    /// the attached probe (if any). Shared by the change + heartbeat sinks so
    /// both stamp identically.
    fn snapshot_sessions(
        &self,
        snapshot: impl IntoIterator<Item = TerminalSessionRecord>,
    ) -> Vec<SnapshotSession> {
        let probe = self.transcript_probe.get();
        snapshot
            .into_iter()
            .map(|rec| SnapshotSession::from_record(&rec, probe.map(|p| p.as_ref())))
            .collect()
    }

    /// Mirror one just-written OPEN record to coord via the attached
    /// emitter, if any. Called AFTER the registry persist; best-effort by
    /// construction (the emitter gates, debounces, and swallows failures —
    /// it never fails the registry path).
    ///
    /// The transcript probe result is threaded through so the MIRRORED restore
    /// tier matches what this machine's own restore path will do. Without it the
    /// mirror advertises `restore_tier: "full"` (which `handoff` turns back into
    /// `origin: authoritative` + `confirmed_at`) for exactly the ids the local
    /// classifier now refuses to `--resume`.
    fn mirror_restore_record(&self, rec: &TerminalSessionRecord) {
        if rec.state != "open" {
            return;
        }
        if let Some(emitter) = self.restore_emitter.get() {
            emitter.emit(
                rec,
                self.probe_transcript_exists(&rec.claude_session_id, rec.working_dir.as_deref()),
            );
        }
    }

    /// Append a CHANGE entry for the records this mutation actually touched,
    /// if a history sink is attached. Called by every layout-meaningful
    /// mutation AFTER the registry write; best-effort by construction (the sink
    /// never fails the registry path).
    ///
    /// PER-RECORD, not the full registry: an append used to serialize every
    /// session in the store on every mutation, making the audit trail O(N) per
    /// spawn and the aggregate O(N²) — the same cost class the WAL removed from
    /// the registry itself. The only reader,
    /// [`crate::session::snapshot_history::read_all_snapshot_sessions`], already
    /// merges newest-`ts`-per-session-id across lines, so a delta line and a
    /// whole-registry line reconstruct identically. The 5-minute HEARTBEAT
    /// ([`Self::snapshot_heartbeat`]) still writes the FULL registry and remains
    /// the recovery anchor.
    fn snapshot_change(&self, changed: impl IntoIterator<Item = TerminalSessionRecord>) {
        if let Some(history) = self.snapshot_history.get() {
            let sessions = self.snapshot_sessions(changed);
            if sessions.is_empty() {
                return;
            }
            history.record_change(sessions);
        }
    }

    /// Append a HEARTBEAT snapshot of the full registry to the history sink,
    /// if attached. Called periodically by the liveness poll; the sink
    /// itself enforces the minimum heartbeat spacing, so calling this every
    /// poll tick is cheap.
    pub fn snapshot_heartbeat(&self) {
        // Idle compaction: the liveness poll is the store's periodic tick, so
        // fold any accumulated WAL into the snapshot here. Runs whether or not
        // a history sink is attached, and does nothing when no mutation has
        // landed since the last fold — an idle runner converges on a compact
        // snapshot without any mutation ever paying an O(N) rewrite.
        self.compact_if_dirty();
        let Some(history) = self.snapshot_history.get() else {
            return;
        };
        // Clone the records OUT of the lock before projecting: the probe stats
        // the disk, and holding the registry mutex across filesystem I/O would
        // let a slow/hung volume block every session write in the runner.
        let records: Vec<TerminalSessionRecord> = match self.map.lock() {
            Ok(m) => m.values().cloned().collect(),
            Err(e) => {
                warn!(error = %e, "session_lifecycle_store: lock poisoned on snapshot_heartbeat");
                return;
            }
        };
        history.record_heartbeat(self.snapshot_sessions(records));
    }

    /// Insert or refresh a session by `claude_session_id`. If a record
    /// already exists, its `opened_at` is preserved; the record is always
    /// (re-)marked `open` (clearing `closed_at` / `close_reason`), its
    /// mutable fields are refreshed, and `last_seen_at` is bumped to now.
    /// A brand-new record sets `opened_at == last_seen_at == now`. Atomic-
    /// writes after mutating.
    pub fn record_open(&self, rec: TerminalSessionRecord) {
        // Normalize a stray empty/whitespace-only config_dir to None: it must
        // never become a bogus `CLAUDE_CONFIG_DIR=""` on resume, and the sticky
        // guard below must treat "no account reported" uniformly as None (so an
        // empty incoming does not clobber a known-good binding).
        // Same normalization for the D1 identity fields: the sticky guards
        // below read `None` as "not reported", so a blank must become `None`
        // here or it would clobber a known-good account/name.
        let rec = TerminalSessionRecord {
            config_dir: non_empty(rec.config_dir),
            account_label: non_empty(rec.account_label),
            account_wrapper: non_empty(rec.account_wrapper),
            session_name: non_empty(rec.session_name),
            name_source: non_empty(rec.name_source),
            tenant_id: non_empty(rec.tenant_id),
            task_run_id: non_empty(rec.task_run_id),
            restore_tier: non_empty(rec.restore_tier),
            ..rec
        };
        let now = Utc::now().timestamp_millis();
        // Captured before `rec`'s fields are moved into the entry below — used
        // after the entry mutation to evict any phantom sibling on the same
        // terminal (see the supersede scan further down).
        let new_id = rec.claude_session_id.clone();
        let new_terminal_id = rec.terminal_id.clone();
        let new_is_authoritative =
            normalize_origin(rec.origin.clone()).as_deref() == Some(ORIGIN_AUTHORITATIVE);
        // A bind that carries confirmation proves a real session owns the
        // terminal, regardless of origin (authoritative OR the launch-agnostic
        // `observed` bind). `confirmed_at` is `Option<i64>` (Copy) — safe to
        // read here before `rec`'s fields are moved into the entry below.
        let new_is_confirmed = rec.confirmed_at.is_some();
        let (merged, superseded) = {
            let mut m = match self.map.lock() {
                Ok(m) => m,
                Err(e) => {
                    warn!(error = %e, "session_lifecycle_store: lock poisoned on record_open");
                    return;
                }
            };
            // W-B: a BRAND-NEW record binding a terminal inherits the spawn-time
            // device default another open record on that terminal already
            // carries. The seam records a provisional row at spawn; an account
            // launcher then types `claude --session-id <its own id>`, whose row
            // is the one the tenancy report reads — without this it would lose
            // the value the spawn recorded. A record that already EXISTED (a
            // resume/restore) never inherits: its spawn was not this terminal's.
            let is_new_record = !m.contains_key(&rec.claude_session_id);
            let inherited_spawn_default = if is_new_record
                && rec.spawn_device_default.is_none()
                && !new_terminal_id.trim().is_empty()
            {
                m.values()
                    .filter(|o| o.state == "open" && o.terminal_id == new_terminal_id)
                    .find_map(|o| o.spawn_device_default.clone())
            } else {
                None
            };
            let entry =
                m.entry(rec.claude_session_id.clone())
                    .or_insert_with(|| TerminalSessionRecord {
                        opened_at: now,
                        ..rec.clone()
                    });
            // Preserve opened_at on an existing record; refresh everything
            // else and re-open.
            // config_dir is STICKY (like the `confirmed_at` monotonic guard
            // below): a later write that omits it — a provider that doesn't
            // report an account, a zone-move backstop, Gemini — must NOT clobber
            // a known-good account binding. Take the incoming value only when it
            // is Some (empty already normalized to None at the top of the fn).
            if rec.config_dir.is_some() {
                entry.config_dir = rec.config_dir;
            }
            entry.working_dir = rec.working_dir;
            entry.page_id = rec.page_id;
            entry.zone_index = rec.zone_index;
            entry.title = rec.title;
            // terminal_id is STICKY against an empty/placeholder incoming (like
            // the `config_dir` guard above): a write that omits the terminal —
            // an empty/whitespace-only id (a provider that didn't report it)
            // — must NOT clobber a real binding. Clobbering it
            // would orphan the row AND dodge the single-tenant-terminal invariant
            // below (which is gated on a non-empty terminal_id). Take the incoming
            // value only when it names a terminal.
            if !rec.terminal_id.trim().is_empty() {
                entry.terminal_id = rec.terminal_id;
            }
            entry.provider = rec.provider;
            // Unasserted origin (zone-move backstop / boot re-assert) must not
            // degrade an authoritative binding to reconciled — preserve on
            // None. The incoming value is normalized so a legacy caller still
            // passing "pinned"/"guessed" lands as authoritative/reconciled.
            if let Some(origin) = normalize_origin(rec.origin) {
                entry.origin = Some(origin);
            }
            // Confirmation is monotonic: a provisional re-record (the spawn-time
            // writer, a zone-move backstop) must NEVER clear a confirmation a
            // provider hook already set. An incoming `Some` (a hook-sourced write
            // carrying confirmation) DOES set it. So: take the incoming value when
            // present, else preserve the existing one.
            if rec.confirmed_at.is_some() {
                entry.confirmed_at = rec.confirmed_at;
            }
            // The fleet session handle is STICKY like `config_dir`: handle
            // acquisition is a separate best-effort write
            // ([`Self::set_handle`]) and virtually no `record_open` caller
            // knows the handle — a re-record without one (zone-move backstop,
            // boot re-assert, provider hook) must never clear it. Take the
            // incoming value only when present.
            if rec.handle.is_some() {
                entry.handle = rec.handle;
            }
            // The D1 identity fields are STICKY for the same reason
            // `config_dir` is: they come from sources that are absent far more
            // often than they are wrong (the live registry only exists while
            // the process does; the spawn writer knows the account but not the
            // name; the hook knows neither). Absent ⇒ keep. A caller that
            // genuinely wants to CHANGE one passes it — last write wins — but
            // no caller can blank one by omission.
            if rec.account_label.is_some() {
                entry.account_label = rec.account_label;
            }
            if rec.account_wrapper.is_some() {
                entry.account_wrapper = rec.account_wrapper;
            }
            if rec.session_name.is_some() {
                entry.session_name = rec.session_name;
            }
            if rec.name_source.is_some() {
                entry.name_source = rec.name_source;
            }
            if rec.tenant_id.is_some() {
                entry.tenant_id = rec.tenant_id;
            }
            if rec.task_run_id.is_some() {
                entry.task_run_id = rec.task_run_id;
            }
            if rec.bypass_permissions.is_some() {
                entry.bypass_permissions = rec.bypass_permissions;
            }
            // The restore markers are sticky too: a re-record (the frontend's
            // post-restore re-assert, a zone move, the provider hook) must not
            // erase the fact that this record was re-materialized by a boot
            // restore — that fact is the census's only evidence.
            if rec.restored_from_boot_at.is_some() {
                entry.restored_from_boot_at = rec.restored_from_boot_at;
            }
            if rec.restore_tier.is_some() {
                entry.restore_tier = rec.restore_tier;
            }
            // W-B: set ONCE — the default a session was spawned under never
            // changes, so a re-record can fill an absent value but never replace
            // a recorded one.
            if entry.spawn_device_default.is_none() {
                entry.spawn_device_default = rec.spawn_device_default.or(inherited_spawn_default);
            }
            entry.state = "open".to_string();
            entry.closed_at = None;
            entry.close_reason = None;
            // A (re-)opened session BOUND to a terminal is no longer waiting
            // for an account choice — the verified resume that re-asserts it
            // ends the wait. A re-record that leaves the row unbound (a blank
            // incoming terminal over a held, detached row) is not a bind, so
            // the hold stays — the same rule as [`Self::rebind_terminal`].
            if !entry.terminal_id.trim().is_empty() {
                entry.awaiting_account_since = None;
            }
            entry.last_seen_at = now;
            let merged = entry.clone();

            // Single-tenant-terminal invariant: a PTY terminal hosts at most
            // ONE live provider session. When a new record that PROVES a real
            // session owns the terminal binds it, any OTHER still-open record on
            // the SAME terminal that no provider hook ever confirmed is a
            // superseded phantom — evict it so restore never resurrects a
            // session that never ran.
            //
            // "Proves a real session" = either AUTHORITATIVE (the runner knows
            // the id exactly — a pinned/typed `--session-id`) OR CONFIRMED of
            // ANY origin. The confirmed arm is what closes the launch-agnostic
            // `observed`-orphan class: a process-anchored, uniquely-correlated
            // `observed` bind is confirmed the moment its transcript is found,
            // and that confirmation proves the terminal's real session — so its
            // unconfirmed siblings are phantoms, evicted here while the terminal
            // is still alive.
            //
            // The split this fixes: the always-on identity seam records a fresh
            // pinned id at SHELL spawn, but an account/CLI launcher then TYPES
            // `claude --session-id <its own id>` into that shell, so the seam's
            // row and the launcher's row bind the same terminal under two ids —
            // the seam row is the orphan (unconfirmed, no transcript).
            //
            // Two eviction arms, distinguished so the registry + logs stay
            // readable:
            //   * UNCONFIRMED sibling → `"superseded"` — the phantom seam row
            //     above. Retired by ANY authoritative-or-confirmed incoming.
            //   * CONFIRMED sibling → `"superseded-terminal-reuse"` — a PRIOR
            //     real run whose exit-close never fired (crash / missed poll) on
            //     a PTY that a long-lived shell then reused for the next `claude`
            //     run. A terminal hosts ONE live session at a time, so when a NEW
            //     CONFIRMED session binds it the prior confirmed run is a
            //     superseded previous tenant and must be closed — otherwise these
            //     accumulate as stale `open` confirmed rows and collapse onto one
            //     terminal at restore (the P4 mass-strand). Gated on
            //     `new_is_confirmed`: only a NEW CONFIRMED binding retires a prior
            //     confirmed sibling — an authoritative-but-UNCONFIRMED incoming
            //     (itself not yet proven) must NOT evict a confirmed row.
            let mut superseded: Vec<TerminalSessionRecord> = Vec::new();
            if (new_is_authoritative || new_is_confirmed) && !new_terminal_id.is_empty() {
                for other in m.values_mut() {
                    // A row held for an account choice is never a sibling: the
                    // hold detached its terminal, so it cannot share this one.
                    if other.claude_session_id == new_id
                        || other.terminal_id != new_terminal_id
                        || other.state != "open"
                    {
                        continue;
                    }
                    if other.confirmed_at.is_none() {
                        other.state = "closed".to_string();
                        other.closed_at = Some(now);
                        other.close_reason = Some("superseded".to_string());
                        // A closed record's restore can never be retried — see
                        // `reap_restore_marker_on_close`.
                        reap_restore_marker_on_close(other);
                        superseded.push(other.clone());
                        info!(
                            terminal_id = %new_terminal_id,
                            superseded = %other.claude_session_id,
                            by = %new_id,
                            "session-restore: evicted unconfirmed phantom sibling — new authoritative session bound the terminal"
                        );
                    } else if new_is_confirmed {
                        other.state = "closed".to_string();
                        other.closed_at = Some(now);
                        other.close_reason = Some("superseded-terminal-reuse".to_string());
                        // A closed record's restore can never be retried — see
                        // `reap_restore_marker_on_close`.
                        reap_restore_marker_on_close(other);
                        superseded.push(other.clone());
                        info!(
                            terminal_id = %new_terminal_id,
                            superseded = %other.claude_session_id,
                            by = %new_id,
                            "session-restore: retired prior confirmed session on a reused terminal — new confirmed session bound it"
                        );
                    }
                }
            }
            // O(1) durable write: the merged record plus any sibling this open
            // superseded — never the whole map.
            let mut deltas = Vec::with_capacity(1 + superseded.len());
            deltas.push(LifecycleDelta::Upsert {
                rec: Box::new(merged.clone()),
            });
            deltas.extend(superseded.iter().map(|r| LifecycleDelta::Upsert {
                rec: Box::new(r.clone()),
            }));
            self.persist(m, &deltas);
            (merged, superseded)
        };
        self.snapshot_change(std::iter::once(merged.clone()).chain(superseded));
        // Phase 4 cloud mirror — emit AFTER the durable local write, from
        // the MERGED record (origin/confirmation preservation applied).
        self.mirror_restore_record(&merged);
    }

    /// Mark an open session closed. No-op (no error) if the session is
    /// absent or already closed.
    ///
    /// Thin wrapper over [`Self::record_close_checked`] with no terminal
    /// expectation — byte-identical behaviour to the pre-`CloseOutcome` close.
    /// Kept for the internal closers that legitimately have no live terminal in
    /// hand (`poll-dead`, `never-started`, `no-terminal`): those
    /// records' terminals are gone by definition, which is the entire meaning of
    /// their close reasons.
    ///
    /// Credential hygiene (Task 5): when this close ends the LAST open session
    /// for its workdir, the workdir's coord-mcp proxy nonce is revoked and its
    /// app-data session-restore config is reaped
    /// ([`crate::coord_mcp::release_workdir_on_session_close`]) — a closed
    /// session's credential must not stay live. Skipped for account-migration
    /// closes (the session continues under a new record with the same workdir)
    /// and when a sibling open session still shares the workdir.
    pub fn record_close(&self, claude_session_id: &str, reason: &str) {
        let _ = self.record_close_checked(claude_session_id, None, reason);
    }

    /// Record that `old_terminal_id`'s session now lives on `new_terminal_id`
    /// and the old PTY is about to be closed. The next close naming
    /// `old_terminal_id` is answered with [`CloseOutcome::TerminalSuperseded`] instead
    /// of being resolved against the map (where it would land as a
    /// mis-bound `NotFound`). Call it AFTER the replacement's
    /// [`Self::record_open`] and BEFORE tearing the old PTY down — the
    /// account migration's step 4.
    pub fn mark_terminal_superseded(&self, old_terminal_id: &str, new_terminal_id: &str) {
        let old = old_terminal_id.trim();
        if old.is_empty() {
            return;
        }
        match self.superseded_terminals.lock() {
            Ok(mut set) => {
                set.insert(old.to_string());
                debug!(
                    old_terminal_id = %old,
                    new_terminal_id = %new_terminal_id,
                    "session_lifecycle_store: terminal superseded — its session moved to a new PTY"
                );
            }
            Err(e) => warn!(
                error = %e,
                "session_lifecycle_store: lock poisoned on mark_terminal_superseded"
            ),
        }
    }

    /// Withdraw a [`Self::mark_terminal_superseded`] that will never be
    /// consumed — the old PTY turned out to be gone already, so its exit-close
    /// either arrived before the mark or never will. Keeps the register equal
    /// to "closes still expected". Harmless to call for an id never marked.
    pub fn unmark_terminal_superseded(&self, old_terminal_id: &str) {
        if let Ok(mut set) = self.superseded_terminals.lock() {
            set.remove(old_terminal_id.trim());
        }
    }

    #[cfg(test)]
    fn superseded_terminal_count(&self) -> usize {
        self.superseded_terminals
            .lock()
            .map(|s| s.len())
            .unwrap_or(0)
    }

    /// Close the record the caller's TERMINAL actually owns, not merely the one
    /// its (possibly stale) `claude_session_id` names.
    ///
    /// ## Why this exists
    ///
    /// The durable binding is established as a `(terminal_id,
    /// claude_session_id)` PAIR by [`Self::record_open`], and was historically
    /// dissolved by `claude_session_id` ALONE. A frontend tab can legitimately
    /// carry a `claudeSessionId` that no longer keys the record for *its own*
    /// terminal — a provisional spawn-seam id, a restored id whose PTY was
    /// respawned under a fresh `--session-id`, or a freshest-mtime `reconciled`
    /// bind that "may be foreign". Every one of those is a well-typed, genuinely
    /// minted id, so nothing rejects it: the close landed on a foreign record
    /// (or on nothing), the live terminal's record stayed `open` forever, and
    /// all three cases reported success identically.
    ///
    /// ## Semantics
    ///
    /// * `Some(t)` and `t` is in the superseded-terminal register
    ///   ([`Self::mark_terminal_superseded`]) — the session moved to another
    ///   PTY and this is the old pane leaving. Consume the entry and answer
    ///   [`CloseOutcome::TerminalSuperseded`] before anything below runs:
    ///   nothing mutated, no observer, no nonce release.
    /// * `expect_terminal_id == None` — today's behaviour exactly: close the
    ///   record at `claude_session_id` if it is open.
    /// * `Some(t)` and the record at `claude_session_id` is open ON `t` —
    ///   close it. [`CloseOutcome::Closed`]. This is the healthy path.
    /// * `Some(t)` and that record belongs to a DIFFERENT terminal (or does not
    ///   exist) — **do not close it.** Resolve the open rows owning `t` and
    ///   close that record instead, reporting [`CloseOutcome::Redirected`] so
    ///   the mis-binding becomes observable rather than silent.
    /// * `Some(t)` with MORE THAN ONE open row on `t` — the per-terminal
    ///   single-open-row invariant is violated (it is enforced at open and at
    ///   boot, never in between). Rank with [`open_authority_key`], breaking
    ///   ties on `claude_session_id` so the choice is DETERMINISTIC, close the
    ///   winner, and report [`CloseOutcome::RedirectedAmbiguous`] naming every
    ///   candidate. A nondeterministic close would be a worse failure than the
    ///   silent no-op it replaces, because it is not reproducible — which is
    ///   why this must not be a first-match `.find()` over the map.
    /// * Nothing resolves — [`CloseOutcome::NotFound`], `warn!` with both ids,
    ///   and **no row is mutated and no observer fires**.
    ///
    /// [`CloseOutcome::AlreadyClosed`] separates a benign repeat close (the
    /// record for this very terminal is already `closed` — an explicit close
    /// followed by the pty-exit close, or a `never-started` retirement of a
    /// provider-less shell) from a genuine resolution failure, so the caller can
    /// report the latter without crying wolf on the former.
    pub fn record_close_checked(
        &self,
        claude_session_id: &str,
        expect_terminal_id: Option<&str>,
        reason: &str,
    ) -> CloseOutcome {
        let now = Utc::now().timestamp_millis();
        // An empty / whitespace terminal id is "no expectation": empty ids
        // are uncorrelatable and already excluded from every terminal-keyed
        // resolver in this file.
        let expect = expect_terminal_id.map(str::trim).filter(|t| !t.is_empty());

        // A close naming a terminal whose session already MOVED to a new PTY
        // (account migration: spawn the replacement, then close the source) is
        // the old pane leaving, not a session ending. Answered BEFORE the map
        // is locked — the register never reads the map, and keeping the two
        // locks un-nested means there is no lock order to get wrong — and
        // before the map resolver, which would otherwise see a session id
        // keyed to a different terminal and report a mis-bound `NotFound`.
        // Consumed on first match so a genuinely stale close of the same id
        // later still surfaces.
        if let Some(t) = expect {
            let superseded = match self.superseded_terminals.lock() {
                Ok(mut set) => set.remove(t),
                Err(e) => {
                    warn!(
                        error = %e,
                        "session_lifecycle_store: lock poisoned on the superseded-terminal register — treating the close as not superseded"
                    );
                    false
                }
            };
            if superseded {
                debug!(
                    claude_session_id = %claude_session_id,
                    terminal_id = %t,
                    reason = %reason,
                    "session-close: terminal was superseded by a migration respawn — the session lives on, no-op"
                );
                return CloseOutcome::TerminalSuperseded {
                    requested: claude_session_id.to_string(),
                    terminal_id: t.to_string(),
                };
            }
        }
        let (outcome, closed_id, closed, closed_workdir, workdir_still_in_use) = {
            let mut m = match self.map.lock() {
                Ok(m) => m,
                Err(e) => {
                    warn!(error = %e, "session_lifecycle_store: lock poisoned on record_close");
                    return CloseOutcome::NotFound {
                        requested: claude_session_id.to_string(),
                        terminal_id: expect.map(str::to_string),
                    };
                }
            };

            let (target, outcome) = resolve_close_target(&m, claude_session_id, expect);

            let Some(target) = target else {
                // Nothing to mutate. Report before dropping the lock.
                match &outcome {
                    CloseOutcome::NotFound { .. } => warn!(
                        claude_session_id = %claude_session_id,
                        terminal_id = %expect.unwrap_or("<none>"),
                        reason = %reason,
                        "session-close: close resolved to no open record — neither the requested session id nor the caller's terminal owns one"
                    ),
                    CloseOutcome::AlreadyClosed { .. } => debug!(
                        claude_session_id = %claude_session_id,
                        reason = %reason,
                        "session-close: record already closed — no-op"
                    ),
                    _ => {}
                }
                return outcome;
            };

            match &outcome {
                CloseOutcome::Redirected { closed, .. } => warn!(
                    requested = %claude_session_id,
                    closed = %closed,
                    terminal_id = %expect.unwrap_or("<none>"),
                    reason = %reason,
                    "session-close: MIS-BOUND close redirected — the tab's claudeSessionId keys a different terminal's record; closing the record this terminal actually owns"
                ),
                CloseOutcome::RedirectedAmbiguous {
                    closed, candidates, ..
                } => warn!(
                    requested = %claude_session_id,
                    closed = %closed,
                    candidates = %candidates.join(","),
                    terminal_id = %expect.unwrap_or("<none>"),
                    reason = %reason,
                    "session-close: MIS-BOUND close redirected onto a terminal carrying MULTIPLE open rows — the per-terminal single-open-row invariant is violated; closed the highest-authority row"
                ),
                _ => {}
            }

            let closed = match m.get_mut(target.as_str()) {
                Some(rec) if rec.state == "open" => {
                    rec.state = "closed".to_string();
                    rec.closed_at = Some(now);
                    rec.close_reason = Some(reason.to_string());
                    // A closed record's restore can never be retried — see
                    // `reap_restore_marker_on_close`.
                    reap_restore_marker_on_close(rec);
                    rec.clone()
                }
                // Unreachable: `resolve_close_target` only names OPEN rows.
                _ => return outcome,
            };
            let workdir = closed.working_dir.clone();
            // Answered under the lock rather than off a full-map clone — the
            // clone existed only to serve this one query.
            let still_in_use = workdir.as_deref().is_some_and(|wd| {
                m.values()
                    .any(|r| r.state == "open" && r.working_dir.as_deref() == Some(wd))
            });
            self.persist(
                m,
                &[LifecycleDelta::Upsert {
                    rec: Box::new(closed.clone()),
                }],
            );
            (outcome, target, closed, workdir, still_in_use)
        };
        self.snapshot_change(std::iter::once(closed));
        // Fabric Phase 3 (review W2): notify AFTER the durable local write,
        // outside the map lock, and only on a REAL open→closed transition
        // (the early return above skips repeat/absent closes). Best-effort:
        // the production observer enqueues a coord `Closed` outbox row.
        //
        // Fired with the id ACTUALLY closed, never the id requested: under a
        // redirect, notifying for the requested id would evict coord's row for
        // the live session while the dead one leaked.
        if let Some(obs) = self.close_observer.get() {
            (obs.0)(closed_id.as_str());
        }

        // Credential hygiene: drop the coord-mcp device nonce bound to this
        // workdir once no OPEN record still uses it. An account migration
        // never reaches here — its row is never closed, only re-keyed — so
        // the nonce the resumed pane still uses is never released under it.
        if let Some(wd) = closed_workdir {
            if !workdir_still_in_use {
                crate::coord_mcp::release_workdir_on_session_close(&wd);
            }
        }
        outcome
    }

    /// Note that `claude_session_id` was NOT resumed because its account is
    /// unknown, and is waiting for the operator to choose one: stamps
    /// [`TerminalSessionRecord::awaiting_account_since`] (the row's current
    /// `last_seen_at`) on the first mark, which starts the hold
    /// ([`awaiting_account_hold`]). A re-mark while the hold is running keeps
    /// the original stamp, so a restart cannot reset the hold's clock. A re-mark
    /// after the hold EXPIRED is re-stamped from the current `last_seen_at`; the
    /// hold never refreshes that, so a new hold starts only if the row was seen
    /// since.
    ///
    /// A running hold DETACHES the row's terminal (`terminal_id` cleared): the
    /// row has no pane by design, and its old terminal id may already host
    /// another session — see [`awaiting_account_hold`]. `page_id` and
    /// `zone_index` stay, for placement. Only a bind ends the hold and re-binds
    /// the row ([`Self::record_open`] on a verified resume).
    ///
    /// No write when nothing changes. Absent, closed or finished record (none
    /// of which the hold can hold) → no-op.
    ///
    /// Never holds a LIVE session: when the row is still bound to a terminal
    /// that `terminal_is_live` reports as a live PTY of this process, the mark
    /// is DECLINED (no stamp, no detach) — that pane is the session, and
    /// detaching it would cut a running session off its own terminal. The
    /// liveness is passed in so the store stays free of the `TerminalManager`;
    /// the Tauri command supplies the manager's live set.
    pub fn mark_awaiting_account(
        &self,
        claude_session_id: &str,
        now_ms: i64,
        terminal_is_live: &dyn Fn(&str) -> bool,
    ) {
        let changed = {
            let mut m = match self.map.lock() {
                Ok(m) => m,
                Err(e) => {
                    warn!(error = %e, "session_lifecycle_store: lock poisoned on mark_awaiting_account");
                    return;
                }
            };
            let Some(rec) = m
                .get_mut(claude_session_id)
                .filter(|r| r.state == "open" && r.finished_at.is_none())
            else {
                debug!(
                    claude_session_id,
                    "session_lifecycle_store: no open, unfinished record — awaiting-account mark dropped"
                );
                return;
            };
            if !rec.terminal_id.trim().is_empty() && terminal_is_live(&rec.terminal_id) {
                warn!(
                    claude_session_id,
                    terminal_id = %rec.terminal_id,
                    "session_lifecycle_store: awaiting-account mark DECLINED — the row's \
                     terminal is a live PTY (a live session is never held or detached)"
                );
                return;
            }
            let before = (rec.awaiting_account_since, rec.terminal_id.clone());
            // A stamp past the prune age is an ENDED hold, not a running one:
            // it is treated as absent, so the mark re-stamps.
            if !awaiting_account_hold(rec, now_ms) {
                rec.awaiting_account_since = Some(rec.last_seen_at);
            }
            if awaiting_account_hold(rec, now_ms) && !rec.terminal_id.is_empty() {
                info!(
                    claude_session_id,
                    terminal_id = %rec.terminal_id,
                    "session_lifecycle_store: held for an account choice — detached from its old terminal"
                );
                rec.terminal_id.clear();
            }
            if (rec.awaiting_account_since, rec.terminal_id.clone()) == before {
                return; // already stamped and unbound — nothing to flush
            }
            let changed = rec.clone();
            self.persist(
                m,
                &[LifecycleDelta::Upsert {
                    rec: Box::new(changed.clone()),
                }],
            );
            changed
        };
        self.snapshot_change(std::iter::once(changed));
    }

    /// The liveness poll's hold for a row waiting for an account choice:
    /// `true` when [`awaiting_account_hold`] holds the row at `now_ms`, so the
    /// poll must neither close it `no-terminal` nor count toward closing it.
    /// `false` → classify the row as usual.
    ///
    /// The hold is read from the durable stamp alone and NEVER refreshes
    /// `last_seen_at`: a refresh fed the restore anchor, so an intermediate
    /// boot that held a row for one poll tick moved the next crash boot's
    /// anchor to that tick and stranded every un-held open row of the cohort.
    /// The row stays on the roster and in the restore set by the hold itself
    /// ([`restore_admissible`]).
    ///
    /// Bounded: once the stamp is more than the open-row prune age
    /// ([`OPEN_STALE_MS`], 7 days) old, the wait ends — the stamp is cleared
    /// and the row is classified as usual (closed `no-terminal` like any
    /// orphan).
    pub fn poll_hold_awaiting_account(&self, claude_session_id: &str, now_ms: i64) -> bool {
        let mut m = match self.map.lock() {
            Ok(m) => m,
            Err(e) => {
                warn!(error = %e, "session_lifecycle_store: lock poisoned on poll_hold_awaiting_account");
                return false;
            }
        };
        let Some(rec) = m.get_mut(claude_session_id) else {
            return false;
        };
        if awaiting_account_hold(rec, now_ms) {
            return true;
        }
        if rec.state != "open" || rec.awaiting_account_since.is_none() {
            return false;
        }
        info!(
            claude_session_id,
            "session_lifecycle_store: account choice unanswered past the open-row \
             prune age — no longer held"
        );
        rec.awaiting_account_since = None;
        let changed = rec.clone();
        self.persist(
            m,
            &[LifecycleDelta::Upsert {
                rec: Box::new(changed),
            }],
        );
        false
    }

    /// Hold, at BOOT, every restore candidate the frontend's boot restore will
    /// leave `needs-account` because its account cannot be named from evidence.
    ///
    /// Restore is lazy per page: the frontend marks a row only when ITS page
    /// restores, and the poll closes an unmatched open row `no-terminal` after
    /// ~3 minutes, so a page opened later would have found its needs-account
    /// rows already gone.
    ///
    /// The predicate is [`boot_restore_needs_account`] — the frontend's
    /// `classifyRestoreAction` gates (valid id, authoritative or observed
    /// origin, confirmed, transcript not probed absent, a full-tier Claude
    /// provider) followed by `resolveRestoreAccount`'s evidence half
    /// ([`crate::session::session_ledger::resolve_config_dir`] over `probe`'s
    /// transcript holders). A row the frontend would restore terminal-only or
    /// skip is never held. The frontend's account rule also refuses a
    /// resolvable dir that is shell-unsafe or may be an unreadable default
    /// home — this set is narrower by exactly those rows, which the frontend
    /// marks when their page opens. Returns the ids held.
    pub fn hold_unresolved_account_candidates(
        &self,
        prior_marker_at: Option<i64>,
        boot_was_clean: bool,
        probe: &dyn TranscriptProbe,
    ) -> Vec<String> {
        let now_ms = Utc::now().timestamp_millis();
        let held: Vec<String> = self
            .restorable_records(prior_marker_at, boot_was_clean)
            .into_iter()
            .filter(|r| r.state == "open" && r.task_run_id.is_none())
            .filter(|r| boot_restore_needs_account(r, probe))
            .map(|r| r.claude_session_id)
            .collect();
        // At boot no PTY of THIS process exists yet, and terminal ids are
        // per-process, so no held row's recorded terminal can be live here.
        for id in &held {
            self.mark_awaiting_account(id, now_ms, &|_| false);
        }
        held
    }

    /// Bump `last_seen_at` on a present session. No-op (no write) if absent.
    pub fn touch(&self, claude_session_id: &str) {
        let now = Utc::now().timestamp_millis();
        let mut m = match self.map.lock() {
            Ok(m) => m,
            Err(e) => {
                warn!(error = %e, "session_lifecycle_store: lock poisoned on touch");
                return;
            }
        };
        let touched = match m.get_mut(claude_session_id) {
            Some(rec) => {
                rec.last_seen_at = now;
                rec.clone()
            }
            None => return,
        };
        self.persist(
            m,
            &[LifecycleDelta::Upsert {
                rec: Box::new(touched),
            }],
        );
    }

    /// Update the persisted `title` of the OPEN record hosted by
    /// `terminal_id` (plan
    /// `2026-07-03-runner-session-tracking-drift-and-guardrails` Phase 3
    /// item 4). Title renames (`terminal_set_title` → OSC 0 echo, the `/name`
    /// skill, an operator rename) previously mutated only the in-memory
    /// `TerminalSession` title, so the durable registry stayed frozen at the
    /// spawn-time title and a restart restored the pane under a stale name.
    /// Keyed by terminal id because the rename call sites hold only that —
    /// the same row [`Self::find_open_by_terminal`] resolves (the session
    /// running there). No-op (no write) if no open record references the
    /// terminal, or the title is already current.
    pub fn update_title_by_terminal(&self, terminal_id: &str, title: &str) {
        let changed = {
            let mut m = match self.map.lock() {
                Ok(m) => m,
                Err(e) => {
                    warn!(error = %e, "session_lifecycle_store: lock poisoned on update_title_by_terminal");
                    return;
                }
            };
            let Some(id) = self.ranked_open_id(&m, terminal_id, |_| true) else {
                return; // no open record hosts this terminal — nothing to flush
            };
            let Some(rec) = m.get_mut(&id) else {
                return;
            };
            if rec.title.as_deref() == Some(title) {
                return; // already current — nothing to flush
            }
            rec.title = Some(title.to_string());
            let changed = rec.clone();
            self.persist(
                m,
                &[LifecycleDelta::Upsert {
                    rec: Box::new(changed.clone()),
                }],
            );
            changed
        };
        self.snapshot_change(std::iter::once(changed));
    }

    /// Re-point an OPEN record at the terminal that now hosts it, without
    /// touching any liveness or provenance field.
    ///
    /// A cold restore recreates the PTY under a fresh ephemeral terminal id, so
    /// the record's `terminal_id` goes stale the moment the old process is
    /// gone. Only the VERIFIED-resume path used to re-assert it (via
    /// `record_open`), which left every `terminal-only` / `quarantine` record
    /// permanently pointing at a dead id — so the next restore pass could not
    /// recognise its own work and cold-created ANOTHER terminal for the same
    /// record, every pass, forever. (Observed 2026-07-22: 48 stale records on
    /// one page had leaked ~330 orphan PTYs across ~7 restore passes.)
    ///
    /// `record_open` is the wrong tool for that rebind: it refreshes
    /// `last_seen_at`, which is exactly what made ghost rows immortal. This
    /// updates the binding ONLY — `last_seen_at`, `state`, `origin`,
    /// `confirmed_at` and `restore_pending_at` are all left untouched, so a
    /// stale row still ages out of the restorable set on schedule.
    ///
    /// No-op (no write, no flush) when the session is absent, not `open`, or
    /// already bound to `terminal_id`.
    ///
    /// Debug-asserts that `terminal_id` is not itself a KEY of the record map —
    /// the free tell that the two id arguments were passed the wrong way round
    /// (the map is keyed by `claude_session_id`, so a swapped call puts a
    /// session id in the terminal slot and would silently rebind a record onto
    /// a "terminal" that is really another session).
    pub fn rebind_terminal(&self, claude_session_id: &str, terminal_id: &str, zone_index: i32) {
        let changed = {
            let mut m = match self.map.lock() {
                Ok(m) => m,
                Err(e) => {
                    warn!(error = %e, "session_lifecycle_store: lock poisoned on rebind_terminal");
                    return;
                }
            };
            // Checked WHILE the guard is already held — taking the lock a
            // second time to answer this would deadlock on a non-reentrant
            // Mutex. `debug_assert!` compiles out of release entirely, so this
            // costs a release build nothing.
            debug_assert!(
                !m.contains_key(terminal_id),
                "rebind_terminal: terminal_id {terminal_id:?} is a claude_session_id key in the \
                 lifecycle store — the two id arguments look swapped (expected \
                 rebind_terminal(claude_session_id, terminal_id, ..), got \
                 claude_session_id={claude_session_id:?})"
            );
            let Some(rec) = m.get_mut(claude_session_id) else {
                return; // unknown session — nothing to rebind
            };
            if rec.state != "open" {
                return; // closed/exited records are not restore targets
            }
            if rec.terminal_id == terminal_id && rec.zone_index == zone_index {
                return; // already current — no write, no flush
            }
            rec.terminal_id = terminal_id.to_string();
            rec.zone_index = zone_index;
            // A row bound to a terminal is no longer held: a held row is
            // unbound by rule ([`awaiting_account_hold`]), so a bind ends the
            // hold rather than leaving a held row contesting a terminal.
            if !rec.terminal_id.trim().is_empty() {
                rec.awaiting_account_since = None;
            }
            let changed = rec.clone();
            self.persist(
                m,
                &[LifecycleDelta::Upsert {
                    rec: Box::new(changed.clone()),
                }],
            );
            changed
        };
        self.snapshot_change(std::iter::once(changed));
    }

    /// Mirror a terminal-page move into the durable registry, resolved by
    /// `terminal_id`. The `POST /terminals/{id}/move` surface mutates the
    /// in-memory `TerminalSession.page_id`; without this flush the durable
    /// record stays frozen at the spawn-time page, so a restart would restore
    /// the pane on its original page rather than where the operator moved it.
    /// Sibling of [`Self::update_title_by_terminal`], but applied to EVERY
    /// open record on the terminal: the page is where the pane sits, not a
    /// property of whichever session runs in it, and a terminal can carry
    /// several open rows. No-op (no write) if no open record references the
    /// terminal, or every page is already current.
    pub fn update_page_by_terminal(&self, terminal_id: &str, page_id: &str) {
        let changed = {
            let mut m = match self.map.lock() {
                Ok(m) => m,
                Err(e) => {
                    warn!(error = %e, "session_lifecycle_store: lock poisoned on update_page_by_terminal");
                    return;
                }
            };
            let changed: Vec<TerminalSessionRecord> = m
                .values_mut()
                .filter(|r| open_on_terminal(r, terminal_id) && r.page_id != page_id)
                .map(|rec| {
                    rec.page_id = page_id.to_string();
                    rec.clone()
                })
                .collect();
            if changed.is_empty() {
                return; // nothing hosts this terminal, or already current
            }
            let deltas: Vec<LifecycleDelta> = changed
                .iter()
                .map(|r| LifecycleDelta::Upsert {
                    rec: Box::new(r.clone()),
                })
                .collect();
            self.persist(m, &deltas);
            changed
        };
        self.snapshot_change(changed);
    }

    /// Mark a present session as restore-pending (a boot-restore is about to
    /// type / has typed `claude --resume` and the handshake is not yet
    /// verified). While the marker is set the liveness poll skips the record
    /// entirely except for a confident-alive observation — see [`classify`].
    /// No-op (no write) if the session is absent.
    ///
    /// ## Also stamps the BOOT-RESTORE marker (restore census, plan D6/P5)
    ///
    /// This is the moment the backend learns "a boot-restore is
    /// re-materializing this record", so it is where `restored_from_boot_at` is
    /// set — durable and backend-owned exactly like `restore_pending_at`, so a
    /// frontend crash mid-restore cannot lose the evidence.
    ///
    /// The tier stamped here is [`RESTORE_TIER_FAILED`], and that is the HONEST
    /// statement at this instant: a resume has been attempted for this record
    /// and has NOT landed. It is upgraded to [`RESTORE_TIER_RESUMED`] by
    /// [`Self::clear_restore_pending`] the moment the handshake verifies (or
    /// the poll observes the session confidently alive). Stamping optimistically
    /// instead would let a restore that silently died report `resumed`, which is
    /// the one direction the census must never be wrong in.
    ///
    /// ## `boot = false`: an operator-initiated resume
    ///
    /// A zone-profile resume (`resumeProfileSession`) needs the poll guard but
    /// is NOT a boot restore, so it sets `restore_pending_at` ONLY — stamping
    /// the boot markers would count it in the restore census
    /// (`restore_census::observe_restored`) and the ledger's came-back set. It
    /// also skips a non-open row: an operator resume of a closed row has
    /// nothing for the poll to guard, and a pending marker on it would never
    /// be cleared. Clearing via [`Self::clear_restore_pending`] is unchanged —
    /// it promotes the tier only on a row carrying `restored_from_boot_at`.
    pub fn mark_restore_pending(&self, claude_session_id: &str, boot: bool) {
        let now = Utc::now().timestamp_millis();
        let mut m = match self.map.lock() {
            Ok(m) => m,
            Err(e) => {
                warn!(error = %e, "session_lifecycle_store: lock poisoned on mark_restore_pending");
                return;
            }
        };
        let changed = match m.get_mut(claude_session_id) {
            Some(rec) if boot => {
                rec.restore_pending_at = Some(now);
                rec.restored_from_boot_at = Some(now);
                rec.restore_tier = Some(RESTORE_TIER_FAILED.to_string());
                rec.clone()
            }
            Some(rec) if rec.state == "open" => {
                rec.restore_pending_at = Some(now);
                rec.clone()
            }
            _ => return,
        };
        self.persist(
            m,
            &[LifecycleDelta::Upsert {
                rec: Box::new(changed),
            }],
        );
    }

    /// Mark a session FINISHED, or unmark it — the WORK axis, orthogonal to
    /// `state`.
    ///
    /// `reason` is recorded only when finishing. Passing `finished = false`
    /// clears the marker, the reason, and the sync flag together, so an unmark
    /// leaves no residue that a later reconcile could act on.
    ///
    /// This is deliberately an EXPLICIT setter rather than a field on
    /// [`SessionIdentityUpdate`]: that overlay carries what the live provider
    /// registry reports about a session, and the registry has no opinion about
    /// whether an operator considers the work done. Routing it through the
    /// sticky merge would also make the marker unclearable by construction,
    /// since `None` there means "not reported".
    ///
    /// **Metadata only — never touches the process.** A finished session keeps
    /// running until it exits on its own.
    ///
    /// For a known session it returns a [`FinishOutcome`] naming what changed
    /// — the marker, or nothing — and whether coord was told ([`FinishSync`]),
    /// so a caller can report what actually landed rather than what it asked
    /// for, and a re-run can read the current sync verdict instead of an
    /// error.
    ///
    /// **A re-finish that carries a reason RESTAMPS** (plan
    /// `2026-10-23-coord-records-a-session-finished-but-not-when-so-a-revival-cannot-be-told-from-a-re-finish`,
    /// Phase 3): `finished_at` moves to now (always a stamp DIFFERENT from the
    /// old one — see `restamp_at`), `finish_synced` is cleared and the finish
    /// is re-sent to coord carrying the new time as its write instant. So a
    /// session revived after its first finish and finished again carries the
    /// SECOND finish time locally, and coord is told it.
    ///
    /// Whether coord's own clock follows depends on the COORD build. A coord
    /// carrying that plan's Phase 2 (in flight at this writing) moves its
    /// transition clock (`state_started_at`, served as `since`) on every
    /// finish, finished → finished included. An older coord moves it only when
    /// the status actually CHANGES: a revival coord saw (the row moved off
    /// `finished`) re-dates it on either build, but a re-finish of a row coord
    /// still reads `finished` does not, so there `sessionStatusSince` can LAG
    /// this `finished_at`. The re-sent row
    /// carries the new stamp, so only coord's ACK of THAT write pairs with the
    /// new mark ([`Self::mark_finish_synced`]); a late ACK of the old row is a
    /// stale one and is ignored. Reported as [`FinishChange::Marker`], since
    /// the mark — not only its reason — was rewritten.
    ///
    /// The REASON is the discriminator because it is the one the finish
    /// protocol already draws: every declaring caller sends one
    /// (`/finish-session`'s Step 1, `/unattended`, `/rewind-session`), while the
    /// read-back after a finish is a reason-less re-POST whose whole job is to
    /// read `finishSynced`. Restamping that read-back would clear the very flag
    /// it reads, on every read, so it could never observe coord's ACK. A
    /// reason equal to the stored one still restamps: it is a new declaration
    /// that happens to give the same why.
    ///
    /// [`SetFinishedError::UnknownSession`] means the id is unknown, and
    /// nothing else; a poisoned registry lock is
    /// [`SetFinishedError::Unavailable`], so an unreadable registry can never
    /// read as "no such session".
    ///
    /// A re-finish with NO reason (or a blank one) is a no-op. On a mark coord
    /// has NOT ACKed (`finish_synced: false`) it is the retry door: nothing
    /// local is written (`changed: None`), but the owed coord write is
    /// re-offered to the finish observer and its verdict returned — so a mark
    /// that stayed local-only (no coord row yet, an outbox write failure) can
    /// be pushed again by a reason-less re-run. A duplicate enqueue is harmless
    /// (idempotent path-addressed PATCH).
    ///
    /// Each re-run while the mark stays unsynced appends one more `Finished`
    /// row, including while an earlier one is still pending: the outbox has
    /// no side-effect-free "is a row for this session pending" probe (its
    /// `pending()` is the drain loop's cursor-bearing scan), so the count is
    /// bounded by operator re-runs rather than deduplicated, and each row is
    /// the same idempotent PATCH.
    ///
    /// Calls are serialized end to end (`finish_forward`), so the retry's
    /// snapshot cannot be overtaken by a concurrent unmark: the order coord's
    /// outbox sees is the order the marker changed in.
    pub fn set_finished(
        &self,
        claude_session_id: &str,
        finished: bool,
        reason: Option<String>,
    ) -> Result<FinishOutcome, SetFinishedError> {
        // Held to the end of this call: see `finish_forward`. It guards no
        // data, so a poisoned guard is simply taken over.
        let _forward = self
            .finish_forward
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = Utc::now().timestamp_millis();
        let mut m = match self.map.lock() {
            Ok(m) => m,
            Err(e) => {
                warn!(error = %e, "session_lifecycle_store: lock poisoned on set_finished");
                return Err(SetFinishedError::Unavailable);
            }
        };
        let rec = m
            .get_mut(claude_session_id)
            .ok_or(SetFinishedError::UnknownSession)?;

        // A blank reason is no reason.
        let reason = non_empty(reason);
        // A reason-less re-finish is a READ (the finish protocol's read-back,
        // or the retry door) and must not move the timestamp: restamping it
        // would clear `finish_synced` on every read-back, so the read-back
        // could never see coord's ACK. A re-finish WITH a reason is a new
        // declaration and falls through to the restamp below. A repeated
        // unmark is a no-op whatever it carries.
        if finished == rec.finished_at.is_some() && (!finished || reason.is_none()) {
            let current = rec.clone();
            // Release the map lock BEFORE the observer runs (it may read the
            // store, and it enqueues to the outbox). `finish_forward` stays
            // held, so no concurrent unmark can change the marker between
            // this snapshot and its enqueue.
            drop(m);
            let coord = if current.finished_at.is_some() && !current.finish_synced {
                self.forward_finish_change(&current)
            } else {
                FinishSync::LocalOnly(LocalOnlyReason::NotRequeued)
            };
            return Ok(FinishOutcome {
                record: current,
                changed: FinishChange::None,
                coord,
            });
        }

        if finished {
            rec.finished_at = Some(match rec.finished_at {
                None => now,
                // A RESTAMP (a re-finish that carried a reason).
                Some(prev) => restamp_at(prev, now),
            });
            // A fresh or restamped mark is unsynced by definition — the outbox
            // owes coord a write, and the boot reconcile must not revert it
            // meanwhile.
            rec.finish_synced = false;
            if let Some(r) = reason {
                rec.finish_reason = Some(r);
            }
            // A session marked finished is not coming back — nothing to wait
            // for ([`awaiting_account_hold`] also refuses a finished row).
            rec.awaiting_account_since = None;
        } else {
            rec.finished_at = None;
            rec.finish_reason = None;
            rec.finish_synced = false;
        }

        let changed = rec.clone();
        self.persist(
            m,
            &[LifecycleDelta::Upsert {
                rec: Box::new(changed.clone()),
            }],
        );
        // Notify AFTER the durable local write and outside the map lock (the
        // guard was consumed by `persist`). Every write that reaches here owes
        // coord a write — a fresh mark, a restamp, or an unmark (reaching here
        // means it really changed). A duplicate enqueue is harmless: both coord
        // writes are idempotent path-addressed PATCHes.
        let coord = self.forward_finish_change(&changed);
        Ok(FinishOutcome {
            record: changed,
            changed: FinishChange::Marker,
            coord,
        })
    }

    /// Hand `rec`'s finished-marker state to the finish observer and return
    /// its verdict. MUST be called with the map lock released.
    fn forward_finish_change(&self, rec: &TerminalSessionRecord) -> FinishSync {
        match self.finish_observer.get() {
            Some(obs) => (obs.0)(rec),
            None => FinishSync::LocalOnly(LocalOnlyReason::NoForwarder),
        }
    }

    /// Record that coord has ACKed this session's finished marker.
    ///
    /// Kept separate from [`Self::set_finished`] because the two have different
    /// authorities: the local mark is authoritative for RESUME immediately,
    /// while this flag is what later permits the boot reconcile to defer to
    /// coord. Only a still-finished record is stamped — an ACK arriving after
    /// the operator unmarked the session must not resurrect the marker.
    ///
    /// `acked_finished_at` is the `finished_at` the ACKed write carried. When
    /// `Some`, only the SAME mark is stamped: after mark → unmark → re-mark, or
    /// a restamp, a late ACK for the first mark must not report the second as
    /// synced before its own write has drained. `None` stamps whatever mark is
    /// current (a row written without the field).
    pub fn mark_finish_synced(&self, claude_session_id: &str, acked_finished_at: Option<i64>) {
        let mut m = match self.map.lock() {
            Ok(m) => m,
            Err(e) => {
                warn!(error = %e, "session_lifecycle_store: lock poisoned on mark_finish_synced");
                return;
            }
        };
        let changed = match m.get_mut(claude_session_id) {
            Some(rec)
                if rec.finished_at.is_some()
                    && !rec.finish_synced
                    && acked_finished_at.is_none_or(|at| rec.finished_at == Some(at)) =>
            {
                rec.finish_synced = true;
                rec.clone()
            }
            _ => return,
        };
        self.persist(
            m,
            &[LifecycleDelta::Upsert {
                rec: Box::new(changed),
            }],
        );
    }

    /// Record what the wind-down executor did to this session — one of the
    /// `WIND_DOWN_*` outcome words above — and which arm did it (one of the
    /// `WIND_DOWN_ARM_*` words). No-op when the record is absent.
    ///
    /// Written whether or not the record is still `open`: the `closed` outcome
    /// is recorded on a record the close itself has usually just flipped, and
    /// the two statements are orthogonal (see
    /// [`TerminalSessionRecord::wind_down_outcome`]).
    pub fn set_wind_down_outcome(
        &self,
        claude_session_id: &str,
        outcome: &str,
        arm: &str,
        at_ms: i64,
    ) {
        let mut m = match self.map.lock() {
            Ok(m) => m,
            Err(e) => {
                warn!(error = %e, "session_lifecycle_store: lock poisoned on set_wind_down_outcome");
                return;
            }
        };
        let changed = match m.get_mut(claude_session_id) {
            Some(rec) => {
                rec.wind_down_outcome = Some(outcome.to_string());
                rec.wind_down_at = Some(at_ms);
                rec.wind_down_arm = Some(arm.to_string());
                rec.clone()
            }
            None => return,
        };
        self.persist(
            m,
            &[LifecycleDelta::Upsert {
                rec: Box::new(changed),
            }],
        );
    }

    /// Clear a session's restore-pending marker (resume handshake verified —
    /// the session is live again). No-op (no write) if the session is absent
    /// or the marker is already clear.
    ///
    /// ## Also UPGRADES the restore tier (restore census, plan D6/P5)
    ///
    /// Both callers mean the same thing — the verified-handshake path in
    /// `runVerifiedResume`, and the liveness poll's self-heal when it observes
    /// the session confidently alive — so this is the one place where "the
    /// resume actually landed" is known. A record that
    /// [`Self::mark_restore_pending`] stamped [`RESTORE_TIER_FAILED`] (attempt
    /// recorded, landing unproven) is therefore promoted to
    /// [`RESTORE_TIER_RESUMED`] here.
    ///
    /// Only a record already carrying `restored_from_boot_at` is touched: a
    /// session that was never boot-restored must not acquire a restore tier
    /// just because some other path cleared a marker on it.
    pub fn clear_restore_pending(&self, claude_session_id: &str) {
        let mut m = match self.map.lock() {
            Ok(m) => m,
            Err(e) => {
                warn!(error = %e, "session_lifecycle_store: lock poisoned on clear_restore_pending");
                return;
            }
        };
        let changed = match m.get_mut(claude_session_id) {
            Some(rec) if rec.restore_pending_at.is_some() => {
                rec.restore_pending_at = None;
                if rec.restored_from_boot_at.is_some()
                    && rec.restore_tier.as_deref() == Some(RESTORE_TIER_FAILED)
                {
                    rec.restore_tier = Some(RESTORE_TIER_RESUMED.to_string());
                }
                rec.clone()
            }
            _ => return, // absent or already clear — nothing to flush
        };
        self.persist(
            m,
            &[LifecycleDelta::Upsert {
                rec: Box::new(changed),
            }],
        );
    }

    /// Flip a present session from PROVISIONAL to CONFIRMED (a provider's
    /// SessionStart hook fired for this session id — session-restore-redesign
    /// Phase 2). Stamps `confirmed_at` with now iff it is not already set
    /// (confirmation is monotonic — the first hook wins; a later resume hook is
    /// a no-op). No-op (no write) if the session is absent or already confirmed.
    ///
    /// Phase 4 reads `confirmed_at` (OR a real transcript on disk) to decide
    /// auto-resume vs treat-as-plain-shell; this method just records the signal.
    pub fn confirm_session(&self, claude_session_id: &str) {
        let now = Utc::now().timestamp_millis();
        let confirmed = {
            let mut m = match self.map.lock() {
                Ok(m) => m,
                Err(e) => {
                    warn!(error = %e, "session_lifecycle_store: lock poisoned on confirm_session");
                    return;
                }
            };
            let confirmed = match m.get_mut(claude_session_id) {
                Some(rec) if rec.confirmed_at.is_none() => {
                    rec.confirmed_at = Some(now);
                    rec.clone()
                }
                _ => return, // absent or already confirmed — nothing to flush
            };
            self.persist(
                m,
                &[LifecycleDelta::Upsert {
                    rec: Box::new(confirmed.clone()),
                }],
            );
            confirmed
        };
        self.snapshot_change(std::iter::once(confirmed.clone()));
        // Phase 4 cloud mirror — a confirmation flip changes the record's
        // honest restore tier (terminal_only → full for a Full-tier
        // provider), which is a material wire-field change.
        self.mirror_restore_record(&confirmed);
    }

    /// Record that the provider ADOPTED `claude_session_id` from
    /// `predecessor` — its SessionStart hook reported the id with `source:
    /// "clear"` while `predecessor` was the session open on that terminal
    /// (see [`TerminalSessionRecord::adopted_from`]). Writes only on change;
    /// no-op when the session is absent, `predecessor` is empty, or names the
    /// session itself. Never creates a record.
    pub fn mark_adopted_from(&self, claude_session_id: &str, predecessor: &str) {
        if predecessor.trim().is_empty() || predecessor == claude_session_id {
            return;
        }
        let mut m = match self.map.lock() {
            Ok(m) => m,
            Err(e) => {
                warn!(error = %e, "session_lifecycle_store: lock poisoned on mark_adopted_from");
                return;
            }
        };
        let changed = match m.get_mut(claude_session_id) {
            Some(rec) if rec.adopted_from.as_deref() != Some(predecessor) => {
                rec.adopted_from = Some(predecessor.to_string());
                rec.clone()
            }
            _ => return,
        };
        self.persist(
            m,
            &[LifecycleDelta::Upsert {
                rec: Box::new(changed),
            }],
        );
    }

    /// Persist the coord-minted stable fleet session handle (`fsh_…`) onto the
    /// record for `claude_session_id` (session-identity fabric Phase 1).
    ///
    /// SERVER WINS: coord's `coord.session_handles` registry rebinds on the
    /// durable `claude_session_id` (UNIQUE anchor), so when the local value
    /// diverges from what the server returned, the local file drifted —
    /// overwrite it and warn. Writes ONLY on change; a matching handle is a
    /// no-op, and an ABSENT record (e.g. an AI subprocess session that has no
    /// terminal-grid row) is a debug-logged no-op — this method never creates
    /// records (a handle-only row would be a phantom restore candidate).
    pub fn set_handle(&self, claude_session_id: &str, handle: &str) {
        {
            let mut m = match self.map.lock() {
                Ok(m) => m,
                Err(e) => {
                    warn!(error = %e, "session_lifecycle_store: lock poisoned on set_handle");
                    return;
                }
            };
            let Some(rec) = m.get_mut(claude_session_id) else {
                debug!(
                    claude_session_id,
                    handle,
                    "session_lifecycle_store: no record for session — handle not persisted locally (registry is server-authoritative)"
                );
                return;
            };
            match rec.handle.as_deref() {
                Some(existing) if existing == handle => return, // unchanged — no write
                Some(existing) => warn!(
                    claude_session_id,
                    local = existing,
                    server = handle,
                    "session_lifecycle_store: local session handle diverged from coord registry — server wins"
                ),
                None => info!(
                    claude_session_id,
                    handle, "session_lifecycle_store: bound fleet session handle"
                ),
            }
            rec.handle = Some(handle.to_string());
            let changed = rec.clone();
            self.persist(
                m,
                &[LifecycleDelta::Upsert {
                    rec: Box::new(changed),
                }],
            );
        }
    }

    /// Merge a best-effort identity overlay onto an existing record — the D1
    /// refresh path used by the liveness poll, the `POST /control/session-open`
    /// confirmation and the spawn writers.
    ///
    /// STICKY, and that is the point: every `None` in `update` means "this
    /// source reported nothing", never "clear it". A live-registry read that
    /// misses the session (Claude Code had not written its per-process file
    /// yet, or the process already exited) therefore leaves the stored account
    /// and name intact instead of blanking the only durable copy. Blank strings
    /// are normalized to `None` first, so "reported empty" degrades to
    /// "reported nothing" rather than to a destructive write.
    ///
    /// Writes ONLY on a real change (an unchanged overlay is the common case on
    /// a 45s poll tick and must not cost a WAL append), and NEVER creates a
    /// record — an identity-only row would be a phantom restore candidate.
    /// Returns true iff the record was found and changed.
    pub fn update_identity(&self, claude_session_id: &str, update: &SessionIdentityUpdate) -> bool {
        let update = update.clone().normalized();
        if update.is_empty() {
            return false;
        }
        let mut m = match self.map.lock() {
            Ok(m) => m,
            Err(e) => {
                warn!(error = %e, "session_lifecycle_store: lock poisoned on update_identity");
                return false;
            }
        };
        let Some(rec) = m.get_mut(claude_session_id) else {
            debug!(
                claude_session_id,
                "session_lifecycle_store: no record for session — identity overlay dropped"
            );
            return false;
        };
        if !update.apply(rec) {
            return false; // nothing new — no durable write
        }
        let changed = rec.clone();
        debug!(
            claude_session_id,
            account = changed.account_label.as_deref().unwrap_or("?"),
            name = changed.session_name.as_deref().unwrap_or("?"),
            "session_lifecycle_store: session identity refreshed"
        );
        self.persist(
            m,
            &[LifecycleDelta::Upsert {
                rec: Box::new(changed),
            }],
        );
        true
    }

    /// [`Self::update_identity`] addressed by TERMINAL id instead of session
    /// id, for the writers that only ever hold a terminal (the bypass-
    /// permissions emit sites, the spawn-time tenant stamp). Resolves through
    /// the same open-record lookup the reconnect path uses; a terminal with no
    /// open record is a debug-logged no-op.
    pub fn update_identity_by_terminal(
        &self,
        terminal_id: &str,
        update: &SessionIdentityUpdate,
    ) -> bool {
        let Some(rec) = self.find_open_by_terminal(terminal_id) else {
            debug!(
                terminal_id,
                "session_lifecycle_store: no open record for terminal — identity overlay dropped"
            );
            return false;
        };
        self.update_identity(&rec.claude_session_id, update)
    }

    /// Record the device default tenant a session was SPAWNED under (W-B of plan
    /// `2026-09-10-spawn-tenant-never-reaches-the-session-coord-credential`) on
    /// the open record for `terminal_id` — but only when that record was CREATED
    /// by this spawn (`opened_at >= spawned_since_ms`) and carries none yet.
    ///
    /// A record that pre-dates the spawn is a resume or boot restore of an older
    /// session: the default read now is the RESTORE-time default, which is
    /// exactly the value W-B exists not to report as the spawn's. Such a record
    /// keeps whatever it had, and one that predates the field stays unrecorded
    /// (UNKNOWN). Returns whether anything was written.
    pub fn record_spawn_device_default(
        &self,
        terminal_id: &str,
        spawn_default: SpawnDeviceDefault,
        spawned_since_ms: i64,
    ) -> bool {
        let mut m = match self.map.lock() {
            Ok(m) => m,
            Err(e) => {
                warn!(error = %e, "session_lifecycle_store: lock poisoned on record_spawn_device_default");
                return false;
            }
        };
        // The open record ranked as the session running on the terminal. A
        // terminal can carry several open rows (`record_open` supersedes only
        // unconfirmed siblings), so this is the ranked pick, not a first match;
        // the `opened_at` gate below still refuses a row older than the spawn.
        let rec = self
            .ranked_open_id(&m, terminal_id, |_| true)
            .and_then(|id| m.get_mut(&id));
        let Some(rec) = rec else {
            debug!(
                terminal_id,
                "session_lifecycle_store: no open record for terminal — spawn default dropped"
            );
            return false;
        };
        if rec.spawn_device_default.is_some() || rec.opened_at < spawned_since_ms {
            return false;
        }
        rec.spawn_device_default = Some(SpawnDeviceDefault {
            tenant_id: non_empty(spawn_default.tenant_id),
            unresolvable: spawn_default.unresolvable,
        });
        let changed = rec.clone();
        self.persist(
            m,
            &[LifecycleDelta::Upsert {
                rec: Box::new(changed),
            }],
        );
        true
    }

    /// Stamp the boot-restore markers on a present record: `restored_from_boot_at`
    /// = now and `restore_tier` = `tier` (one of [`RESTORE_TIER_RESUMED`],
    /// [`RESTORE_TIER_TERMINAL_ONLY`], [`RESTORE_TIER_FAILED`]).
    ///
    /// Backend-owned and durable, exactly like `restore_pending_at`, so a
    /// frontend crash mid-restore cannot lose the evidence that this record was
    /// re-materialized rather than freshly created — the restore census has no
    /// other source for that claim.
    ///
    /// NOT monotonic-once: a restore that starts `terminal-only` and then lands
    /// its resume must be able to say so, and a retry re-stamps its own attempt
    /// time. Absent record → debug-logged no-op (never creates a row).
    ///
    /// ALWAYS re-stamps `restored_from_boot_at`, even when the tier is
    /// unchanged. The timestamp is the restore census's only way to tell a
    /// restore that happened THIS boot from a sticky stamp left by a previous
    /// one (`restored_from_boot_at` survives restarts by design), so skipping
    /// the write on a repeat tier would make a genuinely-restored session read
    /// as `missing` on every boot after its first. The cost is one WAL append
    /// per restore pass per record, which is bounded and rare.
    pub fn mark_restored_from_boot(&self, claude_session_id: &str, tier: &str) {
        let now = Utc::now().timestamp_millis();
        let changed = {
            let mut m = match self.map.lock() {
                Ok(m) => m,
                Err(e) => {
                    warn!(error = %e, "session_lifecycle_store: lock poisoned on mark_restored_from_boot");
                    return;
                }
            };
            let Some(rec) = m.get_mut(claude_session_id) else {
                debug!(
                    claude_session_id,
                    tier, "session_lifecycle_store: no record for session — restore marker dropped"
                );
                return;
            };
            rec.restored_from_boot_at = Some(now);
            rec.restore_tier = Some(tier.to_string());
            let changed = rec.clone();
            self.persist(
                m,
                &[LifecycleDelta::Upsert {
                    rec: Box::new(changed.clone()),
                }],
            );
            changed
        };
        info!(
            claude_session_id,
            tier, "session-restore: record marked as restored from boot"
        );
        self.snapshot_change(std::iter::once(changed));
    }

    /// Re-key a record from `old_id` to `new_id`: remove the entry stored under
    /// `old_id` and re-insert it under `new_id`, updating the record's own
    /// `claude_session_id` to match. Atomic under the same lock + persisted via
    /// the existing temp-file write.
    ///
    /// ## Why a re-key, not a field edit
    ///
    /// `claude_session_id` is the MAP KEY ([`record_open`] does
    /// `m.entry(rec.claude_session_id.clone())`, and the in-place refresh block
    /// only touches non-key fields). An adapter that reports a CORRECTED id can
    /// therefore not change the key by re-recording — it must re-key. This helper
    /// is the defensive primitive for that.
    ///
    /// No-op (no write) when: `old_id == new_id`; `old_id` is absent; or `new_id`
    /// is ALREADY present (re-keying onto a live row would clobber it — refuse
    /// rather than lose data). NOTE: no shipped adapter triggers this today (the
    /// audit refuted Claude id-rotation); it exists so a future adapter that does
    /// report a corrected id has a correct primitive and never leaves a
    /// permanently-stale row.
    pub fn rekey_session(&self, old_id: &str, new_id: &str) {
        if old_id == new_id {
            return;
        }
        let rekeyed = {
            let mut m = match self.map.lock() {
                Ok(m) => m,
                Err(e) => {
                    warn!(error = %e, "session_lifecycle_store: lock poisoned on rekey_session");
                    return;
                }
            };
            if m.contains_key(new_id) {
                warn!(
                    old_id,
                    new_id,
                    "session_lifecycle_store: rekey target already present — refusing to clobber"
                );
                return;
            }
            let Some(mut rec) = m.remove(old_id) else {
                return; // old id absent — nothing to re-key
            };
            rec.claude_session_id = new_id.to_string();
            m.insert(new_id.to_string(), rec.clone());
            self.persist(
                m,
                &[
                    LifecycleDelta::Remove {
                        id: old_id.to_string(),
                    },
                    LifecycleDelta::Upsert {
                        rec: Box::new(rec.clone()),
                    },
                ],
            );
            rec
        };
        self.snapshot_change(std::iter::once(rekeyed));
    }

    /// Remove a record outright (session-restore-redesign Phase 4 reconcile
    /// phantom-prune). Unlike [`record_close`], which leaves a `closed` row that
    /// the restore-grace logic might still resurrect, this DELETES the entry so a
    /// phantom provisional record (authoritative-but-unconfirmed, no live process
    /// and no transcript) can never auto-resume on any future boot. No-op (no
    /// write) if the id is absent.
    /// A removal appends only a `Remove` delta and NO snapshot-history entry: a
    /// `SnapshotSession` can only express a record that exists, and the history
    /// reader ([`crate::session::snapshot_history::read_all_snapshot_sessions`])
    /// merges the newest line per id — so even under the old full-registry
    /// append, a removed id kept showing up from the older lines that still
    /// carried it. Omitting the entry is therefore behavior-preserving.
    pub fn remove_session(&self, claude_session_id: &str) {
        let mut m = match self.map.lock() {
            Ok(m) => m,
            Err(e) => {
                warn!(error = %e, "session_lifecycle_store: lock poisoned on remove_session");
                return;
            }
        };
        if m.remove(claude_session_id).is_none() {
            return; // absent — nothing to flush
        }
        self.persist(
            m,
            &[LifecycleDelta::Remove {
                id: claude_session_id.to_string(),
            }],
        );
    }

    /// Clone of the open record whose session is RUNNING on `terminal_id`, if
    /// any. A blank id resolves to nothing — it is how an unbound row (a held
    /// one included) is represented.
    ///
    /// A terminal can carry several open rows at once — a pane whose operator
    /// ran `/resume` (or typed `claude --resume X`) keeps the pane's original
    /// confirmed row open beside the resumed one, because the hook bind evicts
    /// only UNCONFIRMED siblings. So this is NOT a first match over the
    /// `HashMap`: it picks the same row [`Self::latest_open_on_terminal`]
    /// ranks highest. A first match once made the token-exhaustion migration
    /// resume a 7-day-stale pane session and close the pane running the real
    /// one (2026-10-29, terminal `5b27af82`).
    pub fn find_open_by_terminal(&self, terminal_id: &str) -> Option<TerminalSessionRecord> {
        self.ranked_open_on_terminal(terminal_id, |_| true)
    }

    /// Like [`Self::find_open_by_terminal`], but only returns a CONFIRMED
    /// record — one whose `confirmed_at` is set (a provider SessionStart hook
    /// fired, or a start-anchored `observed`/`reconciled` bind proved a real
    /// session exists here).
    ///
    /// This is what `terminal_list`'s `sessionIdsByTerminal` map uses to
    /// re-attach `claudeSessionId` to reconnected/other tabs. The confirmed
    /// gate is load-bearing: the spawn-time identity seam (`apply_identity_seam`)
    /// mints a fresh session id and records an AUTHORITATIVE-but-PROVISIONAL
    /// `open` row for EVERY terminal — including a plain interactive shell that
    /// never runs a provider (a phantom), and a non-pinned launch whose minted
    /// uuid is NOT the id the process actually runs under. Surfacing an
    /// unconfirmed id would bind a phantom / foreign / never-used id onto the
    /// tab: the per-session PR dropdown would poll coord for a non-existent
    /// session every minute, session-scoped UI-bridge selectors would key off a
    /// dead id, and — because the transcript-poll capture stops as soon as a tab
    /// carries ANY id (`useTabSessionIdCapture`) — the tab could be PERMANENTLY
    /// mis-bound to the seam's uuid instead of the real run id. Gating on
    /// `confirmed_at` mirrors the reconcile phantom gate
    /// ([`crate::session::reconcile::is_phantom_record`]) and the restore
    /// classifier, so only a real, correctly-identified session lights up
    /// session-scoped UI.
    pub fn find_confirmed_open_by_terminal(
        &self,
        terminal_id: &str,
    ) -> Option<TerminalSessionRecord> {
        self.ranked_open_on_terminal(terminal_id, |r| r.confirmed_at.is_some())
    }

    /// Record that a provider's SessionStart hook just reported
    /// `claude_session_id` — the process that hook came from is now the one
    /// running in its terminal. Feeds [`Self::latest_open_on_terminal`]: the
    /// process-local order, and the durable
    /// [`TerminalSessionRecord::last_session_start_at`] stamp that carries the
    /// answer across a restart. Never creates a record.
    pub fn note_session_start_hook(&self, claude_session_id: &str) {
        let now = Utc::now().timestamp_millis();
        match self.map.lock() {
            Ok(mut m) => {
                if let Some(rec) = m.get_mut(claude_session_id) {
                    rec.last_session_start_at = Some(now);
                    let changed = rec.clone();
                    // Not snapshotted or mirrored: the stamp is a ranking
                    // input for terminal lookups, not layout or wire state.
                    self.persist(
                        m,
                        &[LifecycleDelta::Upsert {
                            rec: Box::new(changed),
                        }],
                    );
                }
            }
            Err(e) => {
                warn!(error = %e, "session_lifecycle_store: lock poisoned on note_session_start_hook");
            }
        }
        let mut g = self.hook_order();
        g.0 += 1;
        let seq = g.0;
        g.1.insert(claude_session_id.to_string(), seq);
    }

    /// The process-local hook order. A poisoned lock is recovered rather than
    /// refused: the guarded data is a counter and a map with no invariant a
    /// panic can break, and refusing would blank every terminal lookup.
    fn hook_order(&self) -> std::sync::MutexGuard<'_, (u64, HashMap<String, u64>)> {
        self.session_start_hooks
            .lock()
            .unwrap_or_else(|p| p.into_inner())
    }

    /// The MOST RECENT open session on `terminal_id`, other than `exclude` —
    /// the session a `/clear` hook on that terminal continued.
    ///
    /// A terminal routinely carries several open, confirmed rows (a pane
    /// pinned to P whose operator then typed `claude --resume X`: the hook
    /// bind evicts only UNCONFIRMED siblings, confirming runs no supersede
    /// scan, and liveness keeps both alive while any provider runs in the
    /// subtree), so a first match over the `HashMap` would pick one by
    /// iteration order. This ranks instead, greatest wins:
    ///
    /// 1. the latest SessionStart hook seen for the id in this process
    ///    ([`Self::note_session_start_hook`]) — a row with one outranks a row
    ///    without;
    /// 2. then the durable [`TerminalSessionRecord::last_session_start_at`] —
    ///    the same fact, surviving a restart or a holder handoff, whose adopted
    ///    panes fire no new hook;
    /// 3. then `confirmed_at` (a confirmed row outranks an unconfirmed one);
    /// 4. then `last_seen_at`, then `opened_at`;
    /// 5. then `claude_session_id`, so the choice is deterministic even on a
    ///    full tie.
    ///
    /// Hook order comes before `confirmed_at` because `confirmed_at` is
    /// monotonic: a typed resume of a session confirmed BEFORE the pane's own
    /// session keeps the older stamp and would lose to the pane on
    /// `confirmed_at` alone. The process-local order leads the durable stamp
    /// because it cannot be misordered by a wall-clock step.
    pub fn latest_open_on_terminal(
        &self,
        terminal_id: &str,
        exclude: &str,
    ) -> Option<TerminalSessionRecord> {
        self.ranked_open_on_terminal(terminal_id, |r| r.claude_session_id != exclude)
    }

    /// The open row on `terminal_id` that `admit` accepts and that ranks
    /// highest by [`Self::latest_open_on_terminal`]'s order — the selector
    /// behind the "which session is running on this terminal" reads and the
    /// terminal-keyed writes. (Closing by terminal ranks differently, by
    /// [`open_authority_key`]: it settles which row a dead PTY's exit closes.)
    fn ranked_open_on_terminal(
        &self,
        terminal_id: &str,
        admit: impl Fn(&TerminalSessionRecord) -> bool,
    ) -> Option<TerminalSessionRecord> {
        let m = match self.map.lock() {
            Ok(m) => m,
            Err(e) => {
                warn!(error = %e, "session_lifecycle_store: lock poisoned on ranked_open_on_terminal");
                return None;
            }
        };
        let id = self.ranked_open_id(&m, terminal_id, admit)?;
        m.get(&id).cloned()
    }

    /// Id of the row [`Self::ranked_open_on_terminal`] picks, for a caller
    /// already holding `map`. Takes the hook-order lock, so it keeps the
    /// store's LOCK ORDER — `map` then hooks.
    fn ranked_open_id(
        &self,
        m: &HashMap<String, TerminalSessionRecord>,
        terminal_id: &str,
        admit: impl Fn(&TerminalSessionRecord) -> bool,
    ) -> Option<String> {
        if terminal_id.trim().is_empty() {
            return None;
        }
        let hooks = self.hook_order();
        m.values()
            .filter(|r| open_on_terminal(r, terminal_id) && admit(r))
            .max_by(|a, b| {
                let key = |r: &TerminalSessionRecord| {
                    (
                        hooks.1.get(&r.claude_session_id).copied(),
                        r.last_session_start_at,
                        r.confirmed_at,
                        r.last_seen_at,
                        r.opened_at,
                    )
                };
                key(a)
                    .cmp(&key(b))
                    .then_with(|| a.claude_session_id.cmp(&b.claude_session_id))
            })
            .map(|r| r.claude_session_id.clone())
    }

    /// Clone of the record for `claude_session_id`, open or closed.
    pub fn get(&self, claude_session_id: &str) -> Option<TerminalSessionRecord> {
        match self.map.lock() {
            Ok(m) => m.get(claude_session_id).cloned(),
            Err(e) => {
                warn!(error = %e, "session_lifecycle_store: lock poisoned on get");
                None
            }
        }
    }

    /// Clone of every record whose `state == "open"`.
    pub fn open_records(&self) -> Vec<TerminalSessionRecord> {
        match self.map.lock() {
            Ok(m) => m.values().filter(|r| r.state == "open").cloned().collect(),
            Err(e) => {
                warn!(error = %e, "session_lifecycle_store: lock poisoned on open_records");
                Vec::new()
            }
        }
    }

    /// Clone of EVERY record — OPEN and CLOSED alike (closed rows are retained
    /// until the 24 h prune). Unlike [`open_records`](Self::open_records) this
    /// applies no state filter. Used by the DISPLAY-only "previous sessions"
    /// listing ([`crate::session::past_sessions`]), which merges these with the
    /// snapshot-history reader; it is NOT a restore surface.
    pub fn all_records(&self) -> Vec<TerminalSessionRecord> {
        match self.map.lock() {
            Ok(m) => m.values().cloned().collect(),
            Err(e) => {
                warn!(error = %e, "session_lifecycle_store: lock poisoned on all_records");
                Vec::new()
            }
        }
    }

    /// Run `f` over [`Self::unsynced_finished_records`] with `finish_forward`
    /// held for the read AND whatever `f` enqueues — the serialized door for
    /// the registrar's late-delivery paths. Without it a delivery could read a
    /// still-finished record, lose the race to an unmark, and enqueue its
    /// `Finished` after the unmark's `working`, leaving coord `finished` for a
    /// session the operator unmarked, with nothing left to retry. `f` must not
    /// call [`Self::set_finished`] (the lock is not re-entrant).
    pub fn with_unsynced_finished_records<R>(
        &self,
        f: impl FnOnce(Vec<TerminalSessionRecord>) -> R,
    ) -> R {
        let _forward = self
            .finish_forward
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        f(self.unsynced_finished_records())
    }

    /// Test seam: whether `finish_forward` is held right now (by anyone).
    #[cfg(test)]
    pub(crate) fn finish_forward_held(&self) -> bool {
        self.finish_forward.try_lock().is_err()
    }

    /// Clone of every record carrying a finished marker coord has not ACKed
    /// (`finished_at` set, `finish_synced` false) — the marks whose coord
    /// write is still owed. Filtered under the lock so a late-delivery pass
    /// clones only those, not the whole registry.
    pub fn unsynced_finished_records(&self) -> Vec<TerminalSessionRecord> {
        match self.map.lock() {
            Ok(m) => m
                .values()
                .filter(|r| r.finished_at.is_some() && !r.finish_synced)
                .cloned()
                .collect(),
            Err(e) => {
                warn!(error = %e, "session_lifecycle_store: lock poisoned on unsynced_finished_records");
                Vec::new()
            }
        }
    }

    /// Clone of every record that should be RESTORED on boot. This is the
    /// subset of records the restore path resurrects:
    ///
    /// - every `state == "open"` record that is RECENT relative to the
    ///   registry's anchor (see below) — a hard crash AND a clean shutdown
    ///   whose `pty-exit` close never flushed both leave fresh open rows,
    ///   while a stale ghost row (terminal long gone) carries an old
    ///   `last_seen_at` and is excluded on every boot kind, PLUS
    /// - every `state == "closed"` record whose `close_reason == "pty-exit"`
    ///   (its PTY died — the case a GRACEFUL restart produces by firing
    ///   `handleExit` on every live PTY) that closed within the
    ///   [`RESTORABLE_PTY_EXIT_MS`] grace window OF THE ANCHOR, PLUS
    /// - every `state == "closed"` record whose `close_reason == "poll-dead"`
    ///   (the liveness poll closed an idle-but-live shell) that closed within
    ///   the [`RESTORABLE_POLL_DEAD_MS`] grace window of the anchor — a poll-dead close is
    ///   uncertain (the shell pty was still alive), so an immediate restart
    ///   should bring the session back rather than silently drop it.
    ///
    /// Records closed for any other reason (explicit user close, the poll's
    /// `"no-terminal"` orphan close), or `pty-exit` / `poll-dead` closes older
    /// than their grace windows, are excluded — a user who closes a tab does
    /// not want it resurrected, and a long-dead close is stale.
    ///
    /// The liveness arms live in [`restore_admissible`] (shared with the
    /// ledger roster, [`Self::roster_records`]); this method adds only the
    /// finished filter and the terminal dedupe.
    ///
    /// ## Anchored recency (open AND closed rows)
    ///
    /// An open row is admitted iff
    /// `anchor - last_seen_at <= RESTORABLE_OPEN_ANCHOR_GRACE_MS`, and a
    /// `pty-exit` / `poll-dead` closed row iff `anchor - closed_at <= grace` —
    /// the closed arms were wall-clock (`now - closed_at`) until plan
    /// `2026-10-04-runner-session-roster-restore-picker` Phase 1, so a graceful
    /// rebuild taking longer than 10 minutes restored nothing. The
    /// anchor is the registry's LAST moment of genuine session life, NOT
    /// wall-clock now — and a row from the PRIOR process (stamped before this
    /// process booted) is measured against the prior process's last moment
    /// only, never against this process's own restores and touches (see
    /// [`RestoreAnchors`], which also covers the shutdown marker and the crash
    /// boot's heartbeat).
    ///
    /// A wall-clock rule (`now - last_seen <= grace`) would restore NOTHING
    /// after any downtime longer than the grace (a crash, an hours-later boot);
    /// the row-relative anchor is downtime-proof — when the whole registry dies
    /// together the anchor equals the crash instant, so `anchor - last_seen ≈ 0`
    /// and the cohort survives regardless of how long the runner was down. A
    /// genuinely-newer prior row (one that really was alive later) legitimately
    /// advances the anchor; a crash cohort more than `grace` older than that is
    /// stale and excluded from boot restore. Nothing else re-offers it on
    /// restore: `terminal_session_list_open` returns this set and nothing more.
    /// Its row stays in the registry, so it remains resumable by hand from the
    /// Past Sessions surface (`session::past_sessions`).
    ///
    /// ## A late restore admits the same cohort (drain-deferred boot)
    ///
    /// The boot partition above is also what keeps a WITHHELD restore honest:
    /// when a coord device drain defers `BootResume` for hours, every session
    /// this process hosts is touched each poll tick, and a single anchor over
    /// every row would walk forward to "now" and strand the prior boot's crash
    /// cohort (plan `2026-10-01-drain-deferred-restore-is-swept-as-orphans`).
    /// Because a row stamped before the boot is judged against the PRIOR anchor
    /// only, a restore run hours late admits exactly the cohort a restore at
    /// boot would have admitted. The boot line is the store's own (`boot_ms`,
    /// the process-boot latch), so the restore set, the roster and the restore
    /// census can never disagree about which side of it a row is on.
    ///
    /// ## One-live-session-per-terminal (open rows)
    ///
    /// A PTY hosts at most ONE live provider session, but the durable registry
    /// can hold several `open` rows on one `terminal_id` (P4: a reused terminal
    /// whose prior runs' exit-closes never fired — see
    /// [`Self::repair_terminal_id_collisions`]). Restore maps a terminal to a
    /// single session, so returning N open rows on one terminal collapses them.
    /// The persistent boot repair fixes the durable store, but it must not RACE
    /// the frontend's on-mount restore read, so the read is deduped here too:
    /// among admitted `open` rows sharing a non-empty `terminal_id` we keep the
    /// single most-authoritative one (CONFIRMED over unconfirmed, then newest
    /// `last_seen_at`, then newest `opened_at`) and drop the rest — a row held
    /// for an account choice has no terminal, so it is kept but never contests
    /// one (see [`dedupe_open_by_terminal`]). This is
    /// idempotent with the boot repair and immunizes the read regardless of when
    /// the repair persists.
    pub fn restorable_records(
        &self,
        prior_marker_at: Option<i64>,
        boot_was_clean: bool,
    ) -> Vec<TerminalSessionRecord> {
        self.admitted_records(prior_marker_at, boot_was_clean).0
    }

    /// The session ROSTER the rebuild-safe ledger captures
    /// ([`crate::session::session_ledger::capture`]): EXACTLY the set
    /// [`Self::restorable_records`] returns — the same [`restore_admissible`]
    /// admission against the same anchors, and the same one-row-per-terminal
    /// dedupe — followed by the admitted FINISHED rows (the ledger tags them),
    /// so a mistaken Finish stays undoable from the roster.
    ///
    /// Why not [`Self::open_records`]: a graceful stop closes every PTY
    /// `pty-exit`, so a poll tick between those exits and process exit would
    /// otherwise write a shrunken (or empty) ledger while the next boot still
    /// restores the closed rows. And why not `open ∪ admissible`: a stale
    /// `open` ghost (or a second `open` row on one terminal) is in no restore
    /// set, so listing it made the "will come back" preview overcount the next
    /// boot's restore census. With this set the ledger's UNFINISHED entries ARE
    /// the restore set (plan `2026-10-04-runner-session-roster-restore-picker`,
    /// Phases 2 and 5).
    pub fn roster_records(
        &self,
        prior_marker_at: Option<i64>,
        boot_was_clean: bool,
    ) -> Vec<TerminalSessionRecord> {
        let (mut roster, finished) = self.admitted_records(prior_marker_at, boot_was_clean);
        roster.extend(finished);
        roster
    }

    /// Every row [`restore_admissible`] admits, split on the WORK axis:
    /// `(unfinished — deduped to one open row per terminal, finished)`.
    ///
    /// FINISHED is terminal for restore and outranks every liveness arm: a
    /// finished session is not restorable no matter how recently it was alive,
    /// how it closed, or whether it is still `open`. That is the line that
    /// makes "rebuild the runner, get only the UNFINISHED sessions back" true
    /// (plan `2026-09-01-session-finished-marker-and-unfinished-resume`), and it
    /// keeps a finished session out of the restore census's expected set, so it
    /// is never counted as a strand. It is applied BEFORE the terminal dedupe,
    /// so a finished row can never win a terminal from an unfinished one.
    fn admitted_records(
        &self,
        prior_marker_at: Option<i64>,
        boot_was_clean: bool,
    ) -> (Vec<TerminalSessionRecord>, Vec<TerminalSessionRecord>) {
        // Read before the map lock: the heartbeat is the history's own latch.
        let prior_heartbeat_ms = self
            .snapshot_history
            .get()
            .and_then(|h| h.prior_last_append_ms());
        let observed = self
            .observation_epoch
            .get()
            .map(|e| e.observed())
            .unwrap_or_default();
        let prior_held: HashSet<&str> = self
            .observation_epoch
            .get()
            .map(|e| e.prior_held_unjudged().iter().map(String::as_str).collect())
            .unwrap_or_default();
        let marker_teardown_complete = self.prior_teardown_complete.get().copied();
        let boot_ms = self.boot_ms.load(Ordering::Relaxed);
        match self.map.lock() {
            Ok(m) => {
                let now_ms = Utc::now().timestamp_millis();
                let held = |r: &TerminalSessionRecord| {
                    awaiting_prior_boot_restore(r, &prior_held, boot_ms, now_ms)
                };
                let anchors = restore_anchors(
                    m.values().filter(|r| !held(r)),
                    prior_marker_at,
                    boot_was_clean,
                    marker_teardown_complete,
                    prior_heartbeat_ms,
                    observed,
                    boot_ms,
                    now_ms,
                );
                let (finished, unfinished): (Vec<_>, Vec<_>) = m
                    .values()
                    .filter(|r| held(r) || restore_admissible(r, &anchors, now_ms))
                    .cloned()
                    .partition(|r| r.finished_at.is_some());
                (dedupe_open_by_terminal(unfinished), finished)
            }
            Err(e) => {
                warn!(error = %e, "session_lifecycle_store: lock poisoned on admitted_records");
                (Vec::new(), Vec::new())
            }
        }
    }

    /// Move this store's boot line (see the `boot_ms` field) — tests model "a
    /// row this process wrote after it booted" with it.
    #[cfg(test)]
    pub(crate) fn set_boot_ms_for_test(&self, boot_ms: i64) {
        self.boot_ms.store(boot_ms, Ordering::Relaxed);
    }

    /// Drop `closed` records closed > 24h ago and `open` records not seen
    /// for > 7d — EXCEPT any record whose terminal is still live.
    ///
    /// `live_terminal_ids` is the caller's current live-terminal view (the
    /// liveness poll's own `TerminalManager::list()` for this tick — the same
    /// source `classify` runs against, so prune and close-detection can never
    /// disagree about what "live" means). A closed row whose terminal still
    /// lives is the registry's only proof that terminal was ever recorded:
    /// deleting it on wall-clock retention while the terminal survives
    /// converts a "recorded then closed" terminal into an apparent "never
    /// recorded" one (measured live 2026-07: 234 of 255 live terminals looked
    /// never-recorded because their closed rows had been retention-pruned).
    /// The same gate covers the open-stale path: a not-seen-in-7d row whose
    /// terminal is still alive is a liveness-tracking gap to surface, not
    /// garbage to silently delete. Retention clocks start mattering only once
    /// the terminal itself is gone.
    ///
    /// The "recorded then closed" proof needs only the NEWEST closed row per
    /// live terminal, so only that row is exempt from retention; older closed
    /// rows under the same live terminal age out normally. Without this bound
    /// a long-lived terminal hosting many sequential claude runs would retain
    /// (and rewrite) every closed row for the primary's whole uptime.
    /// Atomic-writes only if something changed.
    pub fn prune(&self, now: i64, live_terminal_ids: &HashSet<String>) {
        {
            let mut m = match self.map.lock() {
                Ok(m) => m,
                Err(e) => {
                    warn!(error = %e, "session_lifecycle_store: lock poisoned on prune");
                    return;
                }
            };
            // Newest closed_at per live terminal — the one closed row per
            // terminal the liveness gate keeps unconditionally.
            let mut newest_closed: HashMap<&str, i64> = HashMap::new();
            for rec in m.values() {
                if rec.state == "closed" && live_terminal_ids.contains(&rec.terminal_id) {
                    if let Some(closed_at) = rec.closed_at {
                        let e = newest_closed
                            .entry(rec.terminal_id.as_str())
                            .or_insert(closed_at);
                        *e = (*e).max(closed_at);
                    }
                }
            }
            let newest_closed: HashMap<String, i64> = newest_closed
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect();
            let before = m.len();
            m.retain(|_, rec| {
                let terminal_live = live_terminal_ids.contains(&rec.terminal_id);
                // A non-closed record whose terminal is STILL LIVE is never
                // pruned — it must outlive its terminal, whatever its age.
                if terminal_live && rec.state != "closed" {
                    return true;
                }
                if rec.state == "closed" {
                    match rec.closed_at {
                        Some(closed_at) => {
                            // Live terminal: its newest closed row is the
                            // recorded-then-closed proof — exempt from
                            // retention. Older siblings age out normally.
                            if terminal_live
                                && newest_closed.get(&rec.terminal_id) == Some(&closed_at)
                            {
                                return true;
                            }
                            now - closed_at <= CLOSED_RETENTION_MS
                        }
                        // Closed without a timestamp — keep (shouldn't happen).
                        None => true,
                    }
                } else {
                    now - rec.last_seen_at <= OPEN_STALE_MS
                }
            });
            if m.len() == before {
                return; // nothing pruned — skip the write
            }
        }
        // A prune drops an unbounded number of rows and already scanned the
        // whole map, so it is O(N) by construction — fold straight into a fresh
        // snapshot rather than appending N `Remove` deltas. Runs on the slow
        // liveness poll, never on the spawn path.
        self.compact();
    }

    /// One-time boot repair for the P4 registry corruption (plan
    /// `2026-07-19-runner-session-restore-mass-strand-and-git-popup`, Phase 3):
    /// many CONFIRMED `open` rows accumulated onto a SINGLE reused `terminal_id`
    /// (a long-lived PTY hosting sequential `claude` runs, each prior run's
    /// exit-close never firing), so restore mapped N rows onto one terminal and
    /// they collapsed.
    ///
    /// For each group of `open` rows sharing a non-empty `terminal_id`, KEEP the
    /// newest live tenant — max `last_seen_at`, tie-broken by `opened_at` — and
    /// CLOSE the older ones with `close_reason = "superseded-terminal-reuse"`
    /// (the same reason `record_open`'s confirmed-reuse eviction stamps, so the
    /// boot repair and the ongoing invariant are indistinguishable in the
    /// registry). A terminal hosts ONE live session, so this collapses to the
    /// live tenant rather than fanning out synthetic ids — 53 of the 54 observed
    /// rows are DEAD prior runs and preserving them as distinct restorable rows
    /// would resurrect 53 phantoms.
    ///
    /// Idempotent: a healthy registry (≤1 open row per terminal) closes nothing.
    /// Returns the number of rows closed (INFO-logged by the boot caller — no
    /// silent cap).
    ///
    /// ## Why this stays BOOT-ONLY (resolved decision, session-identity
    /// disambiguation R1)
    ///
    /// It is called from [`crate::session::reconcile::run_at_boot`] and nowhere
    /// else, and a runtime/periodic second repair was considered and REJECTED.
    /// Two facts close it:
    ///
    ///  1. A post-boot collision is already OBSERVABLE without mutating
    ///     anything — the restore-health read path reports
    ///     `terminalIdsWithMultipleOpenRows`
    ///     (`install_effects_producer::RestoreHealthResponse`), which names
    ///     every terminal carrying more than one `open` row at the moment of
    ///     the probe.
    ///  2. The restore READ is already collision-immune: the
    ///     one-live-session-per-terminal dedupe inside
    ///     [`Self::restorable_records`] keeps the single most-authoritative open
    ///     row per `terminal_id` and drops the rest, using the same ranking key
    ///     this repair uses. So a collision that survives boot changes what
    ///     restore does: nothing.
    ///
    /// A runtime repair would therefore write to durable state — closing rows,
    /// reaping restore markers, appending WAL deltas, firing close observers —
    /// to fix something no reader reads wrong. That is strictly more failure
    /// modes (a race with `record_open`'s deliberate
    /// `(new_is_authoritative || new_is_confirmed)` supersede gate, which
    /// legitimately leaves an observed-unconfirmed row coexisting with the row
    /// it did not earn the right to evict) for zero capability gained. Observe
    /// it on the read path; repair it once, at boot, where the P4 corruption
    /// actually accumulates.
    pub fn repair_terminal_id_collisions(&self) -> usize {
        let now = Utc::now().timestamp_millis();
        let (closed_recs, closed) = {
            let mut m = match self.map.lock() {
                Ok(m) => m,
                Err(e) => {
                    warn!(error = %e, "session_lifecycle_store: lock poisoned on repair_terminal_id_collisions");
                    return 0;
                }
            };
            // Group the OPEN rows by their (non-empty) terminal_id.
            let mut by_terminal: HashMap<String, Vec<String>> = HashMap::new();
            for rec in m.values() {
                if rec.state == "open" && !rec.terminal_id.trim().is_empty() {
                    by_terminal
                        .entry(rec.terminal_id.clone())
                        .or_default()
                        .push(rec.claude_session_id.clone());
                }
            }
            let mut closed = 0usize;
            let mut closed_recs: Vec<TerminalSessionRecord> = Vec::new();
            for (_terminal, ids) in by_terminal {
                if ids.len() < 2 {
                    continue; // no collision on this terminal
                }
                // Rank the colliding rows by restore authority — keep [0]
                // (the row most likely to BE the live session), close the rest.
                // A CONFIRMED row must always outrank an unconfirmed one: the
                // record_open invariant is that an unconfirmed row never evicts
                // a confirmed one, and a naive newest-`last_seen` sort would
                // violate it (a later zone-move / boot re-assert can give an
                // unconfirmed phantom a marginally newer `last_seen_at` than the
                // real confirmed session, so we'd keep the phantom and close
                // the real one). Same key, over the same contenders (open rows
                // bound to a terminal), as the read-time dedupe in
                // `restorable_records`. A row held for an account choice is in
                // neither: the hold detached its terminal, so it never joins a
                // group here and is never closed as a reuse loser.
                let mut ranked = ids;
                ranked.sort_by(|a, b| {
                    let ka = m.get(a.as_str()).map(open_authority_key).unwrap_or((
                        false,
                        i64::MIN,
                        i64::MIN,
                    ));
                    let kb = m.get(b.as_str()).map(open_authority_key).unwrap_or((
                        false,
                        i64::MIN,
                        i64::MIN,
                    ));
                    kb.cmp(&ka)
                });
                for id in ranked.into_iter().skip(1) {
                    if let Some(rec) = m.get_mut(id.as_str()) {
                        rec.state = "closed".to_string();
                        rec.closed_at = Some(now);
                        rec.close_reason = Some("superseded-terminal-reuse".to_string());
                        // A closed record's restore can never be retried — see
                        // `reap_restore_marker_on_close`.
                        reap_restore_marker_on_close(rec);
                        closed_recs.push(rec.clone());
                        closed += 1;
                    }
                }
            }
            if closed == 0 {
                return 0; // nothing collapsed — skip the write
            }
            (closed_recs, closed)
        };
        // Boot-time bulk repair — one compaction rather than N deltas.
        self.compact();
        self.snapshot_change(closed_recs);
        closed
    }

    /// Durably record one mutation as O(1) appended WAL lines.
    ///
    /// MUST be called while the caller still holds the `map` lock (the guard is
    /// taken by reference purely to make that a compile-time requirement) — see
    /// the `wal` field doc for why the ordering matters. Returns whether the WAL
    /// has grown past [`WAL_COMPACT_APPENDS`], which the caller folds into the
    /// snapshot AFTER releasing the map lock (see [`Self::persist`]).
    ///
    /// Best-effort exactly like the whole-map rewrite it replaces: a write
    /// failure is logged, not propagated — the in-memory map still reflects the
    /// mutation for this process. EVERY such failure sets
    /// [`WalWriter::dirty`], so the idle compaction still folds the map to disk
    /// rather than leaving the mutation dirty-but-uncounted.
    #[must_use = "a full WAL must be compacted once the map lock is released"]
    fn wal_append(
        &self,
        _guard: &std::sync::MutexGuard<'_, HashMap<String, TerminalSessionRecord>>,
        deltas: &[LifecycleDelta],
    ) -> bool {
        if deltas.is_empty() {
            return false;
        }
        let mut w = match self.wal.lock() {
            Ok(w) => w,
            Err(e) => {
                // A poisoned WAL lock is unrecoverable for BOTH the append and
                // the compaction that would rescue it, so there is no dirty flag
                // to usefully set here — the map stays authoritative in memory.
                warn!(error = %e, "session_lifecycle_store: WAL lock poisoned — mutation kept in memory only");
                return false;
            }
        };
        // One buffer, one `write_all`: a delta line must reach the file whole or
        // not at all as far as the OS is concerned, so a crash can only ever
        // truncate the tail (which `replay_wal` drops).
        let mut buf = Vec::with_capacity(deltas.len() * 512);
        for d in deltas {
            match serde_json::to_writer(&mut buf, d) {
                Ok(()) => buf.push(b'\n'),
                Err(e) => {
                    warn!(error = %e, "session_lifecycle_store: WAL serialize failed — delta dropped");
                    w.dirty = true;
                    return false;
                }
            }
        }
        if w.file.is_none() {
            match std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.wal_path)
            {
                Ok(f) => w.file = Some(f),
                Err(e) => {
                    warn!(
                        error = %e,
                        path = %self.wal_path.display(),
                        "session_lifecycle_store: WAL open failed — mutation kept in memory only"
                    );
                    w.dirty = true;
                    return false;
                }
            }
        }
        if w.file.is_none() {
            // Unreachable in practice (the block above either opened the file or
            // returned), but the dirty flag makes the impossible case honest
            // rather than silently uncounted.
            w.dirty = true;
            return false;
        }
        let file = w.file.as_mut().expect("WAL handle present (checked above)");
        if let Err(e) = file.write_all(&buf) {
            warn!(
                error = %e,
                path = %self.wal_path.display(),
                "session_lifecycle_store: WAL append failed — mutation kept in memory only"
            );
            // Drop the handle so the next append re-opens rather than keeping
            // writing through a broken descriptor.
            w.file = None;
            w.dirty = true;
            return false;
        }
        w.appends += deltas.len();
        w.appends >= WAL_COMPACT_APPENDS
    }

    /// Re-read this store's on-disk snapshot (+ WAL replay) and REPLACE the
    /// in-memory map with it. Returns the record count now held.
    ///
    /// THE DEFECT this exists for: the `seed-lifecycle-store` test seam writes
    /// the snapshot file directly, but a RUNNING runner holds the authoritative
    /// copy in memory and rewrites the whole file on its next persist. Any
    /// lifecycle write after the seed — a poll tick's `touch`, a close, a
    /// compaction — therefore silently overwrote the seeded rows with the
    /// pre-seed in-memory state, and the seam reported `success: true` while
    /// the seed had already been lost. Reloading closes the window: the running
    /// store adopts the seeded file as its own state, so the next persist
    /// writes the SEED back rather than clobbering it.
    ///
    /// Deliberately whole-map REPLACE, not merge: the seam's contract is
    /// clear-then-seed (the file it wrote is the complete intended store), and
    /// a merge would resurrect exactly the pre-seed rows the caller cleared.
    ///
    /// Takes `map` then `wal` (the store-wide lock order) so no append can
    /// straddle the swap, and truncates the WAL — its deltas describe the state
    /// that was just discarded, and replaying them over the reload would undo
    /// it on the next `open()`.
    pub fn reload_from_disk(&self) -> std::io::Result<usize> {
        let mut m = self
            .map
            .lock()
            .map_err(|_| std::io::Error::other("session lifecycle map lock poisoned"))?;
        let mut w = self
            .wal
            .lock()
            .map_err(|_| std::io::Error::other("session lifecycle WAL lock poisoned"))?;
        let mut fresh = load_map(&self.path);
        let replay = replay_wal(&self.wal_path, &mut fresh);
        // Same rule as `open`: a held row is unbound. The rewrite below
        // persists it.
        detach_held_rows(&mut fresh, Utc::now().timestamp_millis());
        if replay.applied > 0 || replay.damaged {
            // The WAL on disk belongs to the state we are replacing. Fold what
            // it had into the reload (so a concurrent append is not lost) and
            // then rewrite both files from the reloaded map below.
            debug!(
                applied = replay.applied,
                damaged = replay.damaged,
                "session_lifecycle_store: reload folded a non-empty WAL"
            );
        }
        *m = fresh;
        let count = m.len();
        if let Err(e) = write_map(&self.path, &m) {
            warn!(error = %e, "session_lifecycle_store: reload snapshot rewrite failed");
        }
        // Same ending as [`Self::compact`], and for the same reason: the WAL
        // handle is kept OPEN across appends, so truncating the file without
        // dropping it would leave the next append writing at the old offset —
        // NUL-padding the log into a torn line.
        w.file = None;
        w.dirty = false;
        match std::fs::File::create(&self.wal_path) {
            Ok(_) => w.appends = 0,
            Err(e) => warn!(
                error = %e,
                path = %self.wal_path.display(),
                "session_lifecycle_store: reload WAL truncate failed — replay may re-apply discarded deltas"
            ),
        }
        Ok(count)
    }

    /// Fold the WAL into the JSON snapshot and truncate it.
    ///
    /// Ordering is what makes this crash-safe: the snapshot is written and
    /// renamed into place FIRST, and only then is the WAL truncated. A crash in
    /// between replays deltas the snapshot already contains — idempotent — while
    /// the reverse order would lose them outright.
    ///
    /// Takes `map` then `wal` (the store-wide lock order), so no append can
    /// straddle the truncation.
    pub fn compact(&self) {
        let m = match self.map.lock() {
            Ok(m) => m,
            Err(e) => {
                warn!(error = %e, "session_lifecycle_store: lock poisoned on compact");
                return;
            }
        };
        let mut w = match self.wal.lock() {
            Ok(w) => w,
            Err(e) => {
                warn!(error = %e, "session_lifecycle_store: WAL lock poisoned on compact");
                return;
            }
        };
        if let Err(e) = write_map(&self.path, &m) {
            warn!(
                error = %e,
                path = %self.path.display(),
                "session_lifecycle_store: snapshot rewrite failed — WAL kept, nothing lost"
            );
            return; // keep the WAL: it is still the durable record
        }
        // Snapshot is on disk and renamed into place — the WAL is now redundant.
        // The rewrite serialized the WHOLE map, so it also captured any mutation
        // that failed to reach the WAL (see [`WalWriter::dirty`]).
        w.file = None;
        w.dirty = false;
        match std::fs::File::create(&self.wal_path) {
            Ok(_) => w.appends = 0,
            Err(e) => warn!(
                error = %e,
                path = %self.wal_path.display(),
                "session_lifecycle_store: WAL truncate failed — replay will re-apply committed deltas (idempotent)"
            ),
        }
    }

    /// Fold the WAL into the snapshot when there is anything to fold. Called
    /// from the liveness poll's heartbeat so an idle runner converges on a
    /// compact snapshot without any mutation ever paying an O(N) rewrite.
    ///
    /// "Anything to fold" is appended deltas OR a mutation that never reached
    /// the WAL at all ([`WalWriter::dirty`]) — gating on `appends` alone would
    /// silently drop the latter.
    fn compact_if_dirty(&self) {
        let dirty = matches!(self.wal.lock(), Ok(w) if w.appends > 0 || w.dirty);
        if dirty {
            self.compact();
        }
    }

    /// Durably record one mutation, compacting when the WAL has grown past its
    /// bound. Call with the map guard still held; it is consumed so the
    /// compaction (which re-takes the map lock) can only run after release.
    fn persist(
        &self,
        guard: std::sync::MutexGuard<'_, HashMap<String, TerminalSessionRecord>>,
        deltas: &[LifecycleDelta],
    ) {
        let full = self.wal_append(&guard, deltas);
        drop(guard);
        if full {
            self.compact();
        }
    }
}

/// Typed outcome of [`SessionLifecycleStore::record_close_checked`].
///
/// Serialized into `CommandResponse.data` (never into `message`): the outcome
/// is a fact a caller acts on, not prose a human reads, and an unresolvable
/// close must not be reported as a success.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "camelCase")]
pub enum CloseOutcome {
    /// The record named by the caller owned the caller's terminal (or the
    /// caller named no terminal) and was closed. The healthy path.
    #[serde(rename_all = "camelCase")]
    Closed { claude_session_id: String },
    /// The named record belongs to a DIFFERENT terminal. It was left open and
    /// the single open record owning the caller's terminal was closed instead.
    /// A `Redirected` in normal operation is itself a bug report.
    #[serde(rename_all = "camelCase")]
    Redirected {
        requested: String,
        closed: String,
        terminal_id: String,
    },
    /// As [`CloseOutcome::Redirected`], but the caller's terminal carried more
    /// than one open row. The winner is `open_authority_key`-ranked with a
    /// `claude_session_id` tiebreak so the choice is deterministic; every
    /// candidate is named so the invariant violation is observable.
    #[serde(rename_all = "camelCase")]
    RedirectedAmbiguous {
        requested: String,
        closed: String,
        terminal_id: String,
        candidates: Vec<String>,
    },
    /// The record for this very terminal is already `closed` — a benign repeat
    /// (explicit close then pty-exit close) or a `never-started` retirement of a
    /// provider-less shell. Nothing mutated; not a failure.
    #[serde(rename_all = "camelCase")]
    AlreadyClosed { claude_session_id: String },
    /// The named terminal's session had already MOVED to another PTY (an
    /// account-migration respawn) before this close arrived; the old pane is
    /// leaving, the session is not. Benign no-op — nothing mutated, no
    /// observer, the workdir nonce kept. See
    /// [`SessionLifecycleStore::mark_terminal_superseded`].
    ///
    /// Distinct from the `close_reason: "superseded"` that [`SessionLifecycleStore::record_open`]
    /// writes when another session binds a ROW's terminal — that closes a row;
    /// this retires a terminal and closes nothing.
    #[serde(rename_all = "camelCase")]
    TerminalSuperseded {
        requested: String,
        terminal_id: String,
    },
    /// Neither the requested `claude_session_id` nor the caller's terminal
    /// resolves to an open record. Nothing mutated, no observer fired.
    #[serde(rename_all = "camelCase")]
    NotFound {
        requested: String,
        terminal_id: Option<String>,
    },
}

/// Resolve WHICH record a close is actually about, under the map lock.
///
/// Returns `(target claude_session_id to close, typed outcome)`. A `None`
/// target means nothing is mutated. Pure over the map — every mutation, log and
/// side effect belongs to the caller.
fn resolve_close_target(
    m: &HashMap<String, TerminalSessionRecord>,
    claude_session_id: &str,
    expect_terminal_id: Option<&str>,
) -> (Option<String>, CloseOutcome) {
    let existing = m.get(claude_session_id);
    match (expect_terminal_id, existing) {
        // No terminal named — the pre-existing csid-only contract, unchanged.
        (None, Some(rec)) if rec.state == "open" => (
            Some(claude_session_id.to_string()),
            CloseOutcome::Closed {
                claude_session_id: claude_session_id.to_string(),
            },
        ),
        (None, Some(_)) => (
            None,
            CloseOutcome::AlreadyClosed {
                claude_session_id: claude_session_id.to_string(),
            },
        ),
        (None, None) => (
            None,
            CloseOutcome::NotFound {
                requested: claude_session_id.to_string(),
                terminal_id: None,
            },
        ),
        // The named record owns the caller's terminal — pair intact.
        (Some(t), Some(rec)) if rec.terminal_id == t && rec.state == "open" => (
            Some(claude_session_id.to_string()),
            CloseOutcome::Closed {
                claude_session_id: claude_session_id.to_string(),
            },
        ),
        (Some(t), Some(rec)) if rec.terminal_id == t => (
            None,
            CloseOutcome::AlreadyClosed {
                claude_session_id: claude_session_id.to_string(),
            },
        ),
        // Mismatch, or the requested id is unknown: the caller's TERMINAL is
        // the half of the key that is trustworthy here, so resolve from it.
        (Some(t), _) => {
            let mut candidates: Vec<&TerminalSessionRecord> = m
                .values()
                .filter(|r| r.state == "open" && r.terminal_id == t)
                .collect();
            // Deterministic order even when `open_authority_key` ties: the
            // HashMap's own iteration order must never decide which live
            // session dies.
            candidates.sort_by(|a, b| {
                open_authority_key(a)
                    .cmp(&open_authority_key(b))
                    .then_with(|| a.claude_session_id.cmp(&b.claude_session_id))
            });
            match candidates.len() {
                0 => (
                    None,
                    CloseOutcome::NotFound {
                        requested: claude_session_id.to_string(),
                        terminal_id: Some(t.to_string()),
                    },
                ),
                1 => {
                    let closed = candidates[0].claude_session_id.clone();
                    (
                        Some(closed.clone()),
                        CloseOutcome::Redirected {
                            requested: claude_session_id.to_string(),
                            closed,
                            terminal_id: t.to_string(),
                        },
                    )
                }
                _ => {
                    // Greatest authority key wins — same comparator the boot
                    // repair uses, so both agree on which colliding row is live.
                    let closed = candidates
                        .last()
                        .expect("len > 1")
                        .claude_session_id
                        .clone();
                    let names: Vec<String> = candidates
                        .iter()
                        .map(|r| r.claude_session_id.clone())
                        .collect();
                    (
                        Some(closed.clone()),
                        CloseOutcome::RedirectedAmbiguous {
                            requested: claude_session_id.to_string(),
                            closed,
                            terminal_id: t.to_string(),
                            candidates: names,
                        },
                    )
                }
            }
        }
    }
}

/// Restore-authority key for an `open` row on a contested terminal, greatest =
/// most likely to be the LIVE session. A CONFIRMED row (a provider hook proved
/// a real session ran) outranks any unconfirmed one regardless of timestamps;
/// among equal confirmation, the newest `last_seen_at` then `opened_at` wins.
/// Shared by the read-time dedupe ([`dedupe_open_by_terminal`]) and the boot
/// repair ([`SessionLifecycleStore::repair_terminal_id_collisions`]) so both
/// agree on which colliding row to keep.
fn open_authority_key(rec: &TerminalSessionRecord) -> (bool, i64, i64) {
    (rec.confirmed_at.is_some(), rec.last_seen_at, rec.opened_at)
}

/// This process's boot instant for a freshly opened store: the shared
/// process-boot latch in production; `i64::MAX` under `cfg(test)`, so every
/// fixture row reads as the PRIOR life (the boot those fixtures model) unless a
/// test moves the line with [`SessionLifecycleStore::set_boot_ms_for_test`].
fn default_boot_ms() -> i64 {
    if cfg!(test) {
        i64::MAX
    } else {
        crate::session::session_ledger::process_boot_ms()
    }
}

/// The instants every restore-admission window is measured from (see
/// [`SessionLifecycleStore::restorable_records`]).
///
/// ## Why two
///
/// `prior` is the PRIOR process's LAST MOMENT OF LIFE: the max over every row
/// stamp (`last_seen_at`, `closed_at`) strictly EARLIER than this process's
/// boot, plus the prior shutdown marker on a clean boot. A row whose own stamp
/// predates the boot is judged against it — an `open` row against `prior`
/// exactly, a closed row against `prior_closed`, which on a crash boot also
/// takes the snapshot history's last heartbeat (see below).
///
/// The boot line is a WALL-CLOCK comparison: no monotonic stamp survives a
/// process restart, so rows carry `Utc::now()` millis. A wall-clock step
/// BACKWARDS inside this process (an NTP correction, a manual clock change)
/// stamps this process's own re-asserts BEFORE `boot_ms`, where they read as
/// prior-process rows and re-feed `prior` — the window a restore's own
/// activity is supposed to be unable to move. Nothing here detects it; the
/// exposure is one skew-sized window for the rest of the process's life.
///
/// It must exclude this process's own activity. A restore re-asserts the rows
/// it brings back (`record_open` stamps `last_seen_at = now`), so an anchor over
/// EVERY row moved with the restore itself: a second page restored 30 minutes
/// into a rebuild measured its own `pty-exit` cohort (stamped at shutdown)
/// against a re-assert 30 minutes later, restored nothing, and the ledger poll
/// dropped that cohort from the roster.
///
/// `live` is the anchor over every row, this process's included — the old
/// single anchor. A row stamped during THIS process's life is judged against
/// it, so a tab closed `pty-exit` an hour ago while the runner kept running is
/// stale, not restorable forever.
///
/// ## The marker and the heartbeat
///
/// `prior_marker_at` is the shutdown instant on a clean exit, but the crashed
/// process's OWN boot instant on a crash (see
/// [`crate::session::shutdown_marker`]), and an INTERMEDIATE boot during the
/// downtime rewrites it to its own later boot time — the 2026-07-19 anchor that
/// stranded 81 sessions. So it counts only on a CLEAN boot.
///
/// On a crash boot the rows alone can under-state the last moment of life: a
/// SPARSE registry whose newest row is a session the operator exited hours
/// before the crash would anchor at that exit and resurrect it. The snapshot
/// history's heartbeat ([`SnapshotHistory::prior_last_append_ms`], appended at
/// most ~5 minutes apart by the liveness poll, even for an empty registry)
/// bounds the anchor from below on a crash boot — for the CLOSED arms
/// (`pty-exit` / `poll-dead`) ONLY (`prior_closed`). It is a timestamp, never
/// session content — the history is still not read as a restore source.
///
/// It must not bound the `open` arm. The heartbeat is the history file's last
/// append by ANY process, so an intermediate short-lived boot during the
/// downtime (one that crashed or was killed before restoring anything) stamps
/// it with ITS liveness, not the cohort's: an open cohort last seen at T, an
/// intermediate boot heartbeating at T+1h46m, then a crash boot would measure
/// every open row against T+1h46m and strand the whole cohort — the shape of
/// the 2026-07-19 81-session stranding. Open rows are therefore anchored by
/// the prior process's row stamps (plus the clean-boot marker) alone
/// (`prior`): a whole cohort dying together self-anchors at its own instant.
/// The closed arms keep the heartbeat because an exit is a deliberate,
/// already-observed end — resurrecting a long-exited session is the worse
/// error there, and an intermediate boot can only make that arm stricter.
///
/// ## The observation epoch (open rows only)
///
/// An open row's `last_seen_at` advances only when a poll tick whose process
/// snapshot SUCCEEDED touches it — nothing else refreshes a live session. So
/// "stale relative to the anchor" is evidence of death only for the stretch
/// the poll could actually see. When snapshots fail, every stamp freezes
/// while later stamps (a spawn, the clean marker written before teardown)
/// still move `prior` — the 2026-10-27 force-exit stranded 64 of 65 open
/// sessions that way. The open arm is therefore bounded by the observation
/// epoch ([`crate::session::observation_epoch`]):
///
/// - a PRIOR-process row is measured against `min(prior, prior_observed)`
///   when the prior process left an epoch, else `prior` (the first boot after
///   the upgrade — logged once at boot). The epoch was latched before this
///   process wrote one, so nothing this process does moves it. A ghost stays
///   excluded: successful ticks ran after it went stale, so the epoch is past
///   it.
/// - a row written by THIS process is measured against
///   `min(live, current_observed)`, or `min(live, boot_ms)` when this process
///   has never observed successfully — if it never looked, nothing it wrote
///   is stale. This is what keeps the ledger roster (captured every poll tick)
///   from shedding a starved cohort while the process is still running.
///
/// `min` only ever moves the open anchor EARLIER, so the epoch can admit more
/// rows than `prior` alone, never fewer. The 2026-07-19 intermediate-boot
/// strand is untouched: a short-lived intermediate boot that never observed
/// writes no epoch, and one that did can only lower the bound.
///
/// ## The marker and the teardown (open rows only)
///
/// Both clean writers stamp the marker BEFORE the terminal teardown, so a
/// clean marker says a shutdown STARTED, not that it finished. An interrupted
/// teardown is the shape of the 2026-10-27 strand: a runner too loaded to
/// finish closing its terminals had also stopped touching its rows, and its
/// marker sat 38 minutes past every one of them. So the marker joins the open
/// anchor `prior` only when the store's attached
/// [`crate::session::shutdown_marker::BootClassification::teardown_complete`]
/// says the teardown finished, or when the prior process left an observation
/// epoch. With an epoch the open anchor is capped at it, so the marker can
/// lift the anchor no further than the last successful observation, and an
/// interrupted teardown does not weaken that evidence. Keeping the marker
/// there is also what keeps out a lone ghost that the poll observed dead.
/// With neither, an interrupted teardown's open rows are judged by their own
/// stamps alone. A lone ghost then self-anchors and restores, accepted
/// deliberately: a ghost pane can be closed, a dropped session cannot be
/// rebuilt. Teardown evidence that was never attached keeps the marker.
///
/// The closed arms (`prior_closed`) keep the clean marker whatever its
/// teardown bit. A close stamp is an observed end, not a poll touch, and the
/// marker instant is still a moment the prior process was alive.
///
/// ## Held rows
///
/// A row held for an account choice ([`awaiting_account_hold`] at the same
/// `now_ms` admission uses) feeds NO anchor: it is admitted by its hold, not by
/// recency, so nothing a hold does to the row can move the window its un-held
/// cohort is measured against. Only a LIVE hold is exempt: once the stamp has
/// expired the row is an ordinary open row again — it is judged by recency, so
/// its stamps feed the anchor like every other row's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RestoreAnchors {
    boot_ms: i64,
    /// Prior-process row stamps, plus the shutdown marker on a clean boot
    /// when the marker may anchor open rows (see "The marker and the
    /// teardown").
    prior: Option<i64>,
    /// Prior-process row stamps plus the clean-boot marker whatever its
    /// teardown bit, plus the snapshot-history heartbeat on a crash boot.
    prior_closed: Option<i64>,
    live: Option<i64>,
    /// The PRIOR process's observation epoch (before `boot_ms`), if it left one.
    prior_observed: Option<i64>,
    /// THIS process's last successful observation, if any.
    current_observed: Option<i64>,
}

impl RestoreAnchors {
    /// The anchor an `open` row last seen at `stamp` is measured against —
    /// bounded by the observation epoch (see "The observation epoch" above).
    fn for_open(&self, stamp: i64) -> Option<i64> {
        if stamp < self.boot_ms {
            self.prior
                .map(|p| self.prior_observed.map_or(p, |o| p.min(o)))
        } else {
            self.live
                .map(|l| l.min(self.current_observed.unwrap_or(self.boot_ms)))
        }
    }

    /// The anchor a `pty-exit` / `poll-dead` row closed at `stamp` is
    /// measured against.
    fn for_closed(&self, stamp: i64) -> Option<i64> {
        if stamp < self.boot_ms {
            self.prior_closed
        } else {
            self.live
        }
    }
}

fn restore_anchors<'a>(
    records: impl Iterator<Item = &'a TerminalSessionRecord> + Clone,
    prior_marker_at: Option<i64>,
    boot_was_clean: bool,
    marker_teardown_complete: Option<bool>,
    prior_heartbeat_ms: Option<i64>,
    observed: Observed,
    boot_ms: i64,
    now_ms: i64,
) -> RestoreAnchors {
    let stamps = records
        .filter(move |r| !awaiting_account_hold(r, now_ms))
        .flat_map(|r| [Some(r.last_seen_at), r.closed_at])
        .flatten();
    let clean_marker = prior_marker_at
        .filter(|_| boot_was_clean)
        .filter(|at| *at < boot_ms);
    // Like the marker, an epoch counts only when it predates this boot.
    let prior_observed = observed.prior.filter(|at| *at < boot_ms);
    // See "The marker and the teardown": the open anchor takes the marker
    // only when the teardown finished, or an epoch caps the anchor anyway.
    let open_marker = clean_marker
        .filter(|_| marker_teardown_complete.unwrap_or(true) || prior_observed.is_some());
    let crash_heartbeat = prior_heartbeat_ms
        .filter(|_| !boot_was_clean)
        .filter(|at| *at < boot_ms);
    let prior_rows = stamps.clone().filter(|at| *at < boot_ms).max();
    let prior = prior_rows.into_iter().chain(open_marker).max();
    let prior_closed = prior_rows
        .into_iter()
        .chain(clean_marker)
        .chain(crash_heartbeat)
        .max();
    let live = stamps.chain(prior_closed).max();
    RestoreAnchors {
        boot_ms,
        prior,
        prior_closed,
        live,
        prior_observed,
        current_observed: observed.current,
    }
}

/// Detach the terminal of every row [`awaiting_account_hold`] holds at
/// `now_ms`, so a store opened from disk keeps the rule that a held row is
/// unbound (see [`awaiting_account_hold`]). Returns how many rows changed.
fn detach_held_rows(map: &mut HashMap<String, TerminalSessionRecord>, now_ms: i64) -> usize {
    let mut detached = 0;
    for rec in map.values_mut() {
        if awaiting_account_hold(rec, now_ms) && !rec.terminal_id.is_empty() {
            rec.terminal_id.clear();
            detached += 1;
        }
    }
    detached
}

/// Is `r` an open row bound to `terminal_id`? A blank id names no terminal, so
/// it never resolves to an unbound row (a held one, or any uncorrelated row).
fn open_on_terminal(r: &TerminalSessionRecord, terminal_id: &str) -> bool {
    r.state == "open" && !terminal_id.trim().is_empty() && r.terminal_id == terminal_id
}

/// Is `r` held open for an account choice at `now_ms`? An `open`, unfinished
/// row whose [`TerminalSessionRecord::awaiting_account_since`] is no more than
/// the open-row prune age ([`OPEN_STALE_MS`]) old. The ONE predicate behind the
/// poll's hold ([`SessionLifecycleStore::poll_hold_awaiting_account`]), the
/// re-mark rule and the terminal detach
/// ([`SessionLifecycleStore::mark_awaiting_account`], [`detach_held_rows`]),
/// restore/roster admission ([`restore_admissible`]) and the anchor exclusion
/// ([`restore_anchors`]).
///
/// A held row has no pane by design, so it holds no terminal: the hold
/// detaches `terminal_id` (blank = unbound, as for any uncorrelated row) and
/// only a bind ends it ([`SessionLifecycleStore::record_open`],
/// [`SessionLifecycleStore::rebind_terminal`]). That one rule keeps it out of
/// every terminal contest — the restore dedupe, the boot collision repair,
/// `record_open`'s supersede arms, the poll's direct match and the frontend's
/// reconnect — without an exemption in any of them.
fn awaiting_account_hold(r: &TerminalSessionRecord, now_ms: i64) -> bool {
    r.state == "open"
        && r.finished_at.is_none()
        && r.awaiting_account_since
            .is_some_and(|since| now_ms - since <= OPEN_STALE_MS)
}

/// Was `r` held UNJUDGED by the prior process's liveness poll — a row of that
/// process's boot-latched restorable set still waiting for its page's restore
/// on the poll's last successful tick
/// ([`crate::session::observation_epoch::ObservationEpoch::prior_held_unjudged`])?
/// Such a row is admitted by that hold, like [`awaiting_account_hold`]: the
/// prior process admitted it at its boot and never looked at it again, so its
/// observation epoch is no evidence against it (plan
/// `2026-10-28-force-exited-runner-strands-open-sessions-out-of-the-restore-set`,
/// Phase 4). It feeds no anchor, and the hold lasts while the row stays open,
/// unfinished and untouched since before this boot, up to the open-row prune
/// age ([`OPEN_STALE_MS`]) after its own stamp.
fn awaiting_prior_boot_restore(
    r: &TerminalSessionRecord,
    prior_held: &HashSet<&str>,
    boot_ms: i64,
    now_ms: i64,
) -> bool {
    r.state == "open"
        && r.finished_at.is_none()
        && r.last_seen_at < boot_ms
        && now_ms - r.last_seen_at <= OPEN_STALE_MS
        && prior_held.contains(r.claude_session_id.as_str())
}

/// Will the frontend's boot restore leave `r` `needs-account` because no
/// account can be named from evidence? Mirrors `classifyRestoreAction`'s gates
/// (`useTerminalInitialization.ts`) and the evidence half of
/// `resolveRestoreAccount` — NOT the whole frontend decision: the frontend
/// also refuses a resolvable dir that is shell-unsafe or may be an unreadable
/// default home, so this predicate is NARROWER by exactly those rows (the
/// frontend marks them itself when their page opens). The mirrored gates:
///
/// 1. the id is a valid session id (else `skip-invalid`);
/// 2. origin `authoritative` or `observed` (else `terminal-only`);
/// 3. confirmed (else `terminal-only` — a phantom shell);
/// 4. the transcript is not probed ABSENT — a missing/blank `working_dir` is
///    unprobed, which never downgrades (`transcriptExists !== false`);
/// 5. the provider resolves to the Claude adapter with a `full` restore tier
///    ([`adapter_for`], the twin of `providerDescriptorFor`, which falls back
///    to Claude for an unknown provider exactly as this does);
/// 6. the account cannot be named from evidence: `resolveRestoreAccount`'s
///    recorded-dir-then-unique-holder order
///    ([`crate::session::session_ledger::resolve_config_dir`]), where an
///    unprobed holder set is unknown.
fn boot_restore_needs_account(r: &TerminalSessionRecord, probe: &dyn TranscriptProbe) -> bool {
    use crate::session::provider_adapter::{adapter_for, RestoreTier};
    if !crate::session::session_id::is_valid_session_id(&r.claude_session_id) {
        return false;
    }
    if !matches!(r.origin.as_deref(), Some("authoritative" | "observed")) {
        return false;
    }
    if r.confirmed_at.is_none() {
        return false;
    }
    let working_dir = r.working_dir.as_deref().filter(|d| !d.trim().is_empty());
    if working_dir.is_some_and(|wd| !probe.transcript_exists(&r.claude_session_id, Some(wd))) {
        return false;
    }
    let adapter = adapter_for(&r.provider);
    if adapter.provider() != DEFAULT_PROVIDER || adapter.restore_tier() != RestoreTier::Full {
        return false;
    }
    let has_recorded = r
        .config_dir
        .as_deref()
        .is_some_and(|d| !d.trim().is_empty());
    let holders = if has_recorded {
        None
    } else {
        working_dir.and_then(|wd| probe.transcript_config_dirs(&r.claude_session_id, Some(wd)))
    };
    crate::session::session_ledger::resolve_config_dir(r.config_dir.as_deref(), holders.as_deref())
        .is_none()
}

/// The LIVENESS half of restore admission — *was this session alive at the
/// last moment of life?* — with the finished (work-axis) filter deliberately
/// NOT applied, so the ledger roster and the restore set share one predicate.
///
/// Every arm is measured against the anchor for the row's own stamp
/// ([`RestoreAnchors`]), never wall-clock now, so the length of the downtime
/// never matters:
///
/// - `open`: `anchor - last_seen_at <= RESTORABLE_OPEN_ANCHOR_GRACE_MS`, the
///   anchor bounded by the observation epoch ([`RestoreAnchors::for_open`]);
/// - closed `pty-exit`: `anchor - closed_at <= RESTORABLE_PTY_EXIT_MS` — a
///   graceful stop stamps every PTY at the shutdown instant, which on a clean
///   boot IS the prior anchor, so a rebuild of any length restores them while a
///   close long before shutdown stays excluded;
/// - closed `poll-dead`: `anchor - closed_at <= RESTORABLE_POLL_DEAD_MS`;
/// - every other close reason (explicit user close, the poll's `no-terminal`
///   orphan close): never.
///
/// A row held for an account choice ([`awaiting_account_hold`] at `now_ms`)
/// is admitted by its hold rather than its recency: it was a restore
/// candidate when it was held, and it stays one until the operator answers or
/// the hold expires — at most the open-row prune age ([`OPEN_STALE_MS`]) after
/// its stamp. The exemption from the recency arms lasts only that long: an
/// expired stamp falls through to the recency arm like any other open row.
/// The prior poll's unjudged holds ([`awaiting_prior_boot_restore`]) are
/// admitted the same way, by the caller, before this predicate runs.
fn restore_admissible(r: &TerminalSessionRecord, anchors: &RestoreAnchors, now_ms: i64) -> bool {
    if awaiting_account_hold(r, now_ms) {
        return true;
    }
    if r.state == "open" {
        return match anchors.for_open(r.last_seen_at) {
            Some(anchor) => anchor - r.last_seen_at <= RESTORABLE_OPEN_ANCHOR_GRACE_MS,
            // Unreachable: every open row not held at `now_ms` feeds its own
            // last_seen_at into this anchor (`restore_anchors` excludes
            // exactly the rows `awaiting_account_hold` holds at the same
            // `now_ms`, and those returned above). An expired stamp is not a
            // hold, so it no longer reaches here. Admit defensively.
            None => true,
        };
    }
    if r.state == "closed" {
        let grace = match r.close_reason.as_deref() {
            Some("pty-exit") => RESTORABLE_PTY_EXIT_MS,
            Some("poll-dead") => RESTORABLE_POLL_DEAD_MS,
            _ => return false,
        };
        return match r.closed_at {
            Some(closed_at) => anchors
                .for_closed(closed_at)
                .is_some_and(|anchor| anchor - closed_at <= grace),
            None => false,
        };
    }
    false
}

/// Collapse admitted `open` rows that share a non-empty `terminal_id` down to
/// the single most-authoritative one (see [`open_authority_key`]). A PTY hosts
/// at most one live session, so the restore read must never return N open rows
/// for one terminal (P4 collision) — that would collapse them onto one terminal
/// at restore. This is the read-side guard that makes the restore immune to a
/// registry collision REGARDLESS of whether the persistent boot repair has run
/// yet (it is spawn-delayed and can race the frontend's on-mount read). Closed
/// rows and open rows with an empty `terminal_id` pass through untouched and
/// never win a terminal; input order is otherwise preserved. A row held for an
/// account choice is one of the latter: holding it detaches its terminal
/// ([`SessionLifecycleStore::mark_awaiting_account`]), so it is kept in the set
/// without contesting one.
fn dedupe_open_by_terminal(records: Vec<TerminalSessionRecord>) -> Vec<TerminalSessionRecord> {
    let contests =
        |r: &TerminalSessionRecord| r.state == "open" && !r.terminal_id.trim().is_empty();
    // Winner id per contested terminal_id.
    let mut winner: HashMap<&str, &TerminalSessionRecord> = HashMap::new();
    for r in &records {
        if !contests(r) {
            continue;
        }
        winner
            .entry(r.terminal_id.as_str())
            .and_modify(|best| {
                if open_authority_key(r) > open_authority_key(best) {
                    *best = r;
                }
            })
            .or_insert(r);
    }
    records
        .iter()
        .filter(|r| {
            if !contests(r) {
                return true; // closed / unbound rows always pass
            }
            // Keep only the winning open row for this terminal.
            winner
                .get(r.terminal_id.as_str())
                .is_some_and(|best| best.claude_session_id == r.claude_session_id)
        })
        .cloned()
        .collect()
}

fn load_map(path: &Path) -> HashMap<String, TerminalSessionRecord> {
    if !path.exists() {
        return HashMap::new();
    }
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            warn!(error = %e, "session_lifecycle_store: read failed — starting empty");
            return HashMap::new();
        }
    };
    // Two-stage parse so ONE malformed record can't discard the whole store.
    // A real boot loads a registry that may hold a row written by an older /
    // newer schema or a partially-corrupted entry; the original
    // `from_slice::<HashMap<_, Record>>` failed the entire deserialize on the
    // first bad value, silently wiping every restorable on-screen session.
    // Instead: parse to a generic `HashMap<String, Value>` (only the top-level
    // object shape must be intact), then `from_value` each entry individually,
    // KEEPING the good rows and warn+dropping the bad ones.
    let raw: HashMap<String, serde_json::Value> = match serde_json::from_slice(&bytes) {
        Ok(raw) => raw,
        Err(e) => {
            // The file isn't even a JSON object map — unrecoverable; start empty.
            warn!(
                error = %e,
                path = %path.display(),
                "session_lifecycle_store: corrupt map file (not a JSON object) — starting empty"
            );
            return HashMap::new();
        }
    };

    let mut map = HashMap::with_capacity(raw.len());
    let mut dropped: Vec<(String, serde_json::Value)> = Vec::new();
    for (key, value) in raw {
        match serde_json::from_value::<TerminalSessionRecord>(value.clone()) {
            Ok(mut rec) => {
                // Migrate legacy `bind_origin` values to the current vocabulary
                // at load (`pinned`→`authoritative`, `guessed`→`reconciled`).
                // The field-name read is handled by `alias = "bindOrigin"`; this
                // maps the VALUES, which also changed.
                rec.origin = normalize_origin(rec.origin);
                map.insert(key, rec);
            }
            Err(e) => {
                warn!(
                    error = %e,
                    key = %key,
                    path = %path.display(),
                    "session_lifecycle_store: malformed record — dropped, keeping the rest"
                );
                dropped.push((key, value));
            }
        }
    }

    if !dropped.is_empty() {
        warn!(
            dropped = dropped.len(),
            kept = map.len(),
            path = %path.display(),
            "session_lifecycle_store: dropped malformed record(s); preserved the valid rows"
        );
        // Best-effort `.corrupt` side-car with the raw dropped entries so the
        // dropped data is inspectable after the fact. Never fails the load.
        let sidecar: HashMap<String, serde_json::Value> = dropped.into_iter().collect();
        let sidecar_path = path.with_extension("json.corrupt");
        match serde_json::to_vec_pretty(&sidecar) {
            Ok(side_bytes) => {
                if let Err(e) = std::fs::write(&sidecar_path, &side_bytes) {
                    warn!(
                        error = %e,
                        path = %sidecar_path.display(),
                        "session_lifecycle_store: failed to write .corrupt side-car for dropped records"
                    );
                }
            }
            Err(e) => {
                warn!(error = %e, "session_lifecycle_store: failed to serialize dropped records side-car");
            }
        }
    }

    map
}

fn write_map(path: &Path, map: &HashMap<String, TerminalSessionRecord>) -> std::io::Result<()> {
    let tmp = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(map).map_err(std::io::Error::other)?;
    // fsync the temp file BEFORE the rename: the rename is what publishes the
    // snapshot, and publishing a name that points at unflushed bytes is exactly
    // how a compaction can lose the deltas it is about to truncate.
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(&bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(tmp, path)?;
    Ok(())
}

/// Write-ahead-log path for a snapshot path: `terminal-sessions.json` →
/// `terminal-sessions.wal.jsonl`, always a sibling so both live under the same
/// instance-scoped directory.
pub(crate) fn wal_path_for(path: &Path) -> PathBuf {
    path.with_extension("wal.jsonl")
}

/// Outcome of a WAL replay — enough to decide whether the log needs folding
/// into the snapshot before the next append.
#[derive(Debug, Default, Clone, Copy)]
struct WalReplay {
    /// Deltas successfully applied to the map.
    applied: usize,
    /// The log ended mid-line (a crash during an append) or held a line that
    /// could not be parsed. Either way the file must be rewritten before any
    /// further append, or the next line would be concatenated onto the partial
    /// one and corrupt an otherwise-good record.
    damaged: bool,
}

/// Replay `wal_path` over `map`, in file order, last-writer-wins per key.
///
/// CRASH SAFETY. Appends are whole-line `write_all`s, so the only damage a
/// crash can do is truncate the final line. A trailing segment with no `\n`
/// terminator is therefore treated as NEVER COMMITTED and dropped — it is the
/// mutation that was in flight when the process died, which the old whole-map
/// rewrite would equally have lost (its `.json.tmp` would simply not have been
/// renamed). Every complete line before it is committed and is applied.
///
/// A complete-but-unparsable line (schema drift, media corruption) is skipped
/// with a warning rather than aborting the replay: one bad delta must not cost
/// the operator every restorable session, the same fail-soft rule
/// [`load_map`] applies to the snapshot.
fn replay_wal(wal_path: &Path, map: &mut HashMap<String, TerminalSessionRecord>) -> WalReplay {
    let bytes = match std::fs::read(wal_path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return WalReplay::default(),
        Err(e) => {
            warn!(
                error = %e,
                path = %wal_path.display(),
                "session_lifecycle_store: WAL read failed — snapshot used as-is"
            );
            return WalReplay::default();
        }
    };
    if bytes.is_empty() {
        return WalReplay::default();
    }
    let mut out = WalReplay::default();
    let mut malformed = 0usize;
    // Only `\n`-terminated segments are committed; `split` yields a trailing
    // empty slice for a well-terminated file and the torn partial otherwise.
    let mut segments: Vec<&[u8]> = bytes.split(|b| *b == b'\n').collect();
    match segments.pop() {
        Some(tail) if !tail.is_empty() => {
            out.damaged = true;
            warn!(
                bytes = tail.len(),
                path = %wal_path.display(),
                "session_lifecycle_store: torn WAL tail (crash mid-append) — dropping the uncommitted line"
            );
        }
        _ => {}
    }
    for seg in segments {
        if seg.iter().all(|b| b.is_ascii_whitespace()) {
            continue;
        }
        match serde_json::from_slice::<LifecycleDelta>(seg) {
            Ok(LifecycleDelta::Upsert { rec }) => {
                let mut rec = *rec;
                rec.origin = normalize_origin(rec.origin);
                map.insert(rec.claude_session_id.clone(), rec);
                out.applied += 1;
            }
            Ok(LifecycleDelta::Remove { id }) => {
                map.remove(&id);
                out.applied += 1;
            }
            Err(e) => {
                malformed += 1;
                out.damaged = true;
                warn!(
                    error = %e,
                    path = %wal_path.display(),
                    "session_lifecycle_store: malformed WAL delta — skipped, keeping the rest"
                );
            }
        }
    }
    if malformed > 0 {
        warn!(
            malformed,
            applied = out.applied,
            path = %wal_path.display(),
            "session_lifecycle_store: WAL replay dropped malformed delta(s)"
        );
    }
    out
}

// ---------------------------------------------------------------------------
// Boot-restore phase latch (plan 2026-10-01-drain-deferred-restore-is-swept-as-orphans)
// ---------------------------------------------------------------------------

/// Where THIS process's boot restore stands, as recorded by the restore path
/// itself (`commands::terminal::terminal_session_list_open`).
///
/// The lifecycle poll reads it so it cannot sweep, as `no-terminal` orphans,
/// the very records a drain-deferred restore promised to bring back. On
/// 2026-10-01 a crash-restarted primary under a coord drain had all 44 of its
/// crash victims closed `no-terminal` ~3½ minutes after boot while
/// `terminal_session_list_open` was still answering "restore deferred" — so
/// when the drain lifted the restore set was empty.
///
/// Monotonic: `NotYetRun → Withheld → Ran`, or `NotYetRun → Ran`. Once the
/// restore has run, normal orphan handling resumes for good; a later drain does
/// not re-arm the hold. The restore runs per PAGE (the active one first), so
/// this latch alone cannot keep the rows of a page not yet opened. The
/// boot-census arm of [`withheld_boot_restore_candidate`] closes that gap (the
/// plan's recorded follow-up "Non-active pages at an UNDRAINED boot") for the
/// life of THIS process: a row in the boot-latched restorable set is held in
/// every phase (plan
/// `2026-10-28-force-exited-runner-strands-open-sessions-out-of-the-restore-set`,
/// Phase 4). A held row is never touched, so across a further restart it is
/// carried by the observation epoch instead: each successful tick records the
/// ids it held unjudged, and the next boot admits those rows by that hold
/// ([`awaiting_prior_boot_restore`]) rather than measuring them against an
/// epoch that never looked at them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootRestorePhase {
    /// The restore path has not been asked yet (the frontend has not mounted)
    /// — or this process has no restore consumer at all.
    NotYetRun,
    /// The restore path was asked and WITHHELD the restore set (the coord drain
    /// gate deferred `BootResume`, drained or state unknown). Nothing has been
    /// restored, and the withheld records must stay restorable.
    Withheld,
    /// The restore path returned a restore set. The boot restore has run.
    Ran,
}

const BOOT_RESTORE_NOT_YET_RUN: u8 = 0;
const BOOT_RESTORE_WITHHELD: u8 = 1;
const BOOT_RESTORE_RAN: u8 = 2;

static BOOT_RESTORE_PHASE: std::sync::atomic::AtomicU8 =
    std::sync::atomic::AtomicU8::new(BOOT_RESTORE_NOT_YET_RUN);

/// This process's [`BootRestorePhase`].
pub fn boot_restore_phase() -> BootRestorePhase {
    match BOOT_RESTORE_PHASE.load(std::sync::atomic::Ordering::Acquire) {
        BOOT_RESTORE_WITHHELD => BootRestorePhase::Withheld,
        BOOT_RESTORE_RAN => BootRestorePhase::Ran,
        _ => BootRestorePhase::NotYetRun,
    }
}

/// Record that the boot restore was WITHHELD (drain-deferred). Only moves
/// `NotYetRun → Withheld`: idempotent under the frontend's repeated asks, and
/// a no-op once the restore has run.
pub fn note_boot_restore_withheld() {
    let moved = BOOT_RESTORE_PHASE
        .compare_exchange(
            BOOT_RESTORE_NOT_YET_RUN,
            BOOT_RESTORE_WITHHELD,
            std::sync::atomic::Ordering::AcqRel,
            std::sync::atomic::Ordering::Acquire,
        )
        .is_ok();
    if moved {
        info!(
            "session_lifecycle_store: boot restore WITHHELD by the coord drain — the lifecycle poll will hold prior-boot records open until it runs"
        );
    }
}

/// Record that the boot restore has RUN (a restore set was returned).
/// Terminal.
pub fn note_boot_restore_ran() {
    let prior = BOOT_RESTORE_PHASE.swap(BOOT_RESTORE_RAN, std::sync::atomic::Ordering::AcqRel);
    if prior == BOOT_RESTORE_WITHHELD {
        info!(
            "session_lifecycle_store: withheld boot restore has now run — normal orphan handling resumes"
        );
    }
}

/// What one `terminal_session_list_open` call may do to [`BootRestorePhase`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootRestoreLatchEvent {
    /// Not the boot restore's own read — the latch is left alone.
    Untouched,
    /// The boot restore asked and the drain gate withheld its set.
    Withheld,
    /// The boot restore asked and was handed its set.
    Ran,
}

/// Which latch event a `terminal_session_list_open` call produces. Pure, so the
/// rule is unit-testable without a Tauri `State`.
///
/// Only the boot restore's own read (`purpose: "restore"`) may move the latch,
/// in EITHER direction (plan `2026-10-01-drain-deferred-restore-is-swept-as-orphans`,
/// vet correction 1). The tree-reset report, the worker-adoption probe and the
/// page reconcile read the same set; if their first non-deferred call after a
/// drain lifted counted as `Ran`, the poll's hold would be released before
/// anything had been restored.
pub fn boot_restore_latch_event(is_boot_restore: bool, deferred: bool) -> BootRestoreLatchEvent {
    match (is_boot_restore, deferred) {
        (false, _) => BootRestoreLatchEvent::Untouched,
        (true, true) => BootRestoreLatchEvent::Withheld,
        (true, false) => BootRestoreLatchEvent::Ran,
    }
}

/// Apply a [`BootRestoreLatchEvent`] to the process-global latch.
pub fn apply_boot_restore_latch_event(event: BootRestoreLatchEvent) {
    match event {
        BootRestoreLatchEvent::Untouched => {}
        BootRestoreLatchEvent::Withheld => note_boot_restore_withheld(),
        BootRestoreLatchEvent::Ran => note_boot_restore_ran(),
    }
}

/// Test-only reset of the process-global latch.
#[cfg(test)]
pub(crate) fn reset_boot_restore_phase_for_test() {
    BOOT_RESTORE_PHASE.store(
        BOOT_RESTORE_NOT_YET_RUN,
        std::sync::atomic::Ordering::Release,
    );
}

/// Is this open record a boot-restore candidate whose restore is being
/// WITHHELD — i.e. must the lifecycle poll leave it alone this tick?
///
/// - Only PRIOR-BOOT records qualify: `last_seen_at < boot_at_ms`. A record
///   this process has touched carries this-boot evidence and is judged
///   normally. With no boot instant (unclassified — tests only) nothing
///   qualifies, which is the pre-fix behaviour.
/// - `Ran` ⇒ never: the restore has run, so a prior-boot record matching no
///   terminal now is a genuine orphan (or is `restore_pending`, already exempt).
/// - `Withheld` ⇒ hold, even after the drain lifts, until the restore actually
///   runs. If the frontend's re-run never comes the records simply stay `open`
///   — the never-close-on-uncertainty direction; the 7-day open-stale prune
///   still bounds them.
/// - `NotYetRun` ⇒ hold iff the drain gate for `BootResume` defers right now
///   (drained, or drain state unknown). This covers a poll tick that lands
///   before the frontend's first ask; when the gate allows, today's handling
///   applies (the restore is about to run, or there is no consumer for it).
/// - `awaiting_boot_restore` ⇒ hold in EVERY phase, `Ran` included (plan
///   `2026-10-28-force-exited-runner-strands-open-sessions-out-of-the-restore-set`,
///   Phase 4). The caller passes
///   [`crate::session::restore_census::BootCensus::awaits_restore`]: the row is
///   in the boot-latched restorable set and is not finished. Restore is lazy
///   per PAGE, so after `Ran` a row on a page not opened yet still matches no
///   terminal by design, and closing it `no-terminal` after
///   `NO_TERMINAL_ORPHAN_TICKS` strands it for good. Its own restore makes the
///   hold moot: the restore re-asserts the row (`record_open`) or re-points its
///   terminal (a rebind), so the poll matches it to that live terminal — by
///   id, or by the `(page_id, title, working_dir)` fallback, which this arm
///   never disables ([`boot_restore_hold`]) — and judges it there, which the
///   hold never overrides; its first touch moves it past the boot line. A
///   close or a finish by the operator ends the hold, and so does the
///   `OPEN_STALE_MS` prune. A row outside the latched set is a genuine orphan
///   and closes as before. The poll records the rows this arm held so the
///   hold survives a further restart (see [`BootRestorePhase`]).
pub fn withheld_boot_restore_candidate(
    last_seen_at: i64,
    boot_at_ms: Option<i64>,
    phase: BootRestorePhase,
    drain_defers_boot_resume: bool,
    awaiting_boot_restore: bool,
) -> bool {
    let Some(boot_at) = boot_at_ms else {
        return false;
    };
    if last_seen_at >= boot_at {
        return false;
    }
    if awaiting_boot_restore {
        return true;
    }
    match phase {
        BootRestorePhase::Ran => false,
        BootRestorePhase::Withheld => true,
        BootRestorePhase::NotYetRun => drain_defers_boot_resume,
    }
}

/// The two flags the liveness poll derives per row from
/// [`withheld_boot_restore_candidate`]. They differ in one input, and on
/// purpose:
///
/// - `id_match_only` — the drain/phase hold alone (no boot-census arm). It is
///   what [`match_live_terminal`] takes: while the boot restore is withheld, a
///   prior-boot row may match only its OWN terminal id, never another tab
///   through the `(page_id, title, working_dir)` fallback.
/// - `hold` — that hold OR the boot-census arm. It is what
///   [`hold_for_withheld_boot_restore`] takes: a row it covers is never closed
///   `no-terminal` this tick.
///
/// The census arm widens only `hold`. Before it existed, the `Ran` phase kept
/// the triple fallback, and that is how an auto-resume row whose re-assert
/// was lost still finds its live tab and is touched; narrowing the match for
/// every latched row would leave such a row untouched until the next boot
/// dropped it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BootRestoreHold {
    pub id_match_only: bool,
    pub hold: bool,
}

/// Should the poll carry `rec`, which it holds this tick, into the observation
/// epoch's `held_unjudged` set, so that the next boot admits it by that hold
/// ([`awaiting_prior_boot_restore`])? Only a boot-census row
/// (`awaiting_boot_restore`) whose restore this process has not attempted, or
/// attempted less than [`RESTORE_PENDING_TTL_MS`] before `now_ms`. A
/// `restore_pending_at` at or after this boot means the restore ran here:
/// while it is inside the TTL the resume may still be verifying on a loaded
/// box, so it is still carried; past the TTL it is a resume that did not
/// verify, and carrying it would re-offer that failed resume on every
/// restart (for example, after the operator closed the failed tab). Left
/// uncarried, the next boot judges it by the epoch and does not re-offer it,
/// which is what it did before the hold existed.
pub fn carries_boot_restore_hold(
    rec: &TerminalSessionRecord,
    boot_at_ms: Option<i64>,
    awaiting_boot_restore: bool,
    now_ms: i64,
) -> bool {
    awaiting_boot_restore
        && !rec
            .restore_pending_at
            .zip(boot_at_ms)
            .is_some_and(|(pending_at, boot_at)| {
                pending_at >= boot_at && now_ms - pending_at > RESTORE_PENDING_TTL_MS
            })
}

/// See [`BootRestoreHold`].
pub fn boot_restore_hold(
    last_seen_at: i64,
    boot_at_ms: Option<i64>,
    phase: BootRestorePhase,
    drain_defers_boot_resume: bool,
    awaiting_boot_restore: bool,
) -> BootRestoreHold {
    BootRestoreHold {
        id_match_only: withheld_boot_restore_candidate(
            last_seen_at,
            boot_at_ms,
            phase,
            drain_defers_boot_resume,
            false,
        ),
        hold: withheld_boot_restore_candidate(
            last_seen_at,
            boot_at_ms,
            phase,
            drain_defers_boot_resume,
            awaiting_boot_restore,
        ),
    }
}

/// Match an open record to a live terminal the way the lifecycle poll does: by
/// `terminal_id` first, then by the `(page_id, title, working_dir)` triple —
/// EXCEPT for a record whose boot restore is withheld (`held`), which gets the
/// id match only.
///
/// A held record is from the PRIOR process, whose terminals all died with it,
/// and terminal ids are fresh UUIDs per spawn, so nothing live can carry its
/// id. A triple match is therefore necessarily a DIFFERENT, this-boot terminal
/// that happens to share a page, a default title ("Claude 1") and a working
/// directory — and accepting it would either close the record (`poll-dead`,
/// whose grace has long expired after an hours-long drain, or
/// `never-started`) or `touch` it into this boot, hijacking a session that is
/// still waiting to be restored.
pub fn match_live_terminal<'a, T: ?Sized>(
    terminal_id: &str,
    triple: Option<(&str, &str, &str)>,
    by_id: &HashMap<&str, &'a T>,
    by_triple: &HashMap<(&str, &str, &str), &'a T>,
    held: bool,
) -> Option<&'a T> {
    by_id.get(terminal_id).copied().or_else(|| {
        if held {
            return None;
        }
        by_triple.get(&triple?).copied()
    })
}

/// Rewrite a [`classify`] verdict for a record whose boot restore is withheld
/// (see [`withheld_boot_restore_candidate`]). Modelled on the
/// `restore_pending` guard inside `classify`: the no-match arms
/// (`NoMatchWait`, `CloseNoTerminal`) become `Skip`, so a withheld record is
/// never closed `"no-terminal"` (non-restorable) and accumulates no no-match
/// ticks — the orphan debounce starts from zero once the hold lifts. Arms
/// that saw a live terminal are this-tick evidence and pass through.
pub fn hold_for_withheld_boot_restore(action: PollAction, held: bool) -> PollAction {
    if held
        && matches!(
            action,
            PollAction::NoMatchWait | PollAction::CloseNoTerminal
        )
    {
        PollAction::Skip
    } else {
        action
    }
}

// ---------------------------------------------------------------------------
// Liveness-poll decision core (pure)
// ---------------------------------------------------------------------------

/// What the liveness poll should do with one open session this tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PollAction {
    /// The session is alive and busy — refresh its `last_seen_at`.
    KeepAlive,
    /// The session's shell pty is alive but no live Claude is present in its
    /// inclusive subtree, and it has not yet reached [`LIVE_SHELL_DEAD_TICKS`]
    /// consecutive claude-absent ticks — wait more ticks before closing
    /// (debounce a transient snapshot miss / an in-flight relaunch).
    NeedsConfirm,
    /// The session is dead — flip it to `closed` (reason `"poll-dead"`).
    Close,
    /// The record's shell is dead (or claude never appeared) AND the record was
    /// never confirmed — no provider session ever started in this terminal, so
    /// there is nothing that died. Close it `"never-started"`, which is
    /// non-restorable (the restore grace match covers only `"pty-exit"` /
    /// `"poll-dead"`), instead of libelling a bare shell as a dead session.
    CloseNeverStarted,
    /// No live terminal matched this record this tick, and it has not yet
    /// reached [`NO_TERMINAL_ORPHAN_TICKS`] consecutive no-match ticks —
    /// increment the no-match counter and wait (debounce registration races).
    NoMatchWait,
    /// No live terminal matched for > [`NO_TERMINAL_ORPHAN_TICKS`]
    /// consecutive ticks — the record is an orphan; close it with reason
    /// `"no-terminal"` (non-restorable).
    CloseNoTerminal,
    /// Uncertain — do nothing (NEVER close on uncertainty).
    Skip,
}

/// Pure liveness-classification core. Asymmetric by design: we only ever
/// close on a *confident* dead signal (a dead pty, or a no-match streak long
/// enough to be an orphan rather than a race), and `Skip` (never close) on
/// genuine uncertainty (snapshot failure, mid-restore records).
///
/// - `live_is_alive`: `Some(true)` shell pty alive, `Some(false)` dead,
///   `None` no matching pty for this session.
/// - `claude_present`: whether a live Claude process is present in the tracked
///   PID's *inclusive* subtree (the tracked PID's own image is `claude*`, or any
///   descendant's is). This replaces the old `descendant_count > 0` heuristic,
///   which mis-closed an idle **agent** session whose tracked PID *is* `claude`
///   (it spawns zero children while parked on a gate). The invariant: the
///   `Some(true)` arm never reaches `Close` while `claude_present` — a live
///   Claude here is the normal state, never a dead signal. `Close` is reachable
///   from `Some(true)` only when claude is ABSENT for `LIVE_SHELL_DEAD_TICKS`
///   (the preserved bare-shell cleanup: operator quit claude, shell PID lingers).
/// - `consecutive_dead`: count of prior consecutive claude-absent ticks.
/// - `consecutive_no_match`: count of prior consecutive ticks where NO live
///   terminal matched this record. "No matching terminal in THIS instance
///   for many consecutive ticks" is not uncertainty — it is an orphan that
///   would otherwise stay `open` for up to 7 days and re-qualify for restore
///   at every boot. Debounced via [`NO_TERMINAL_ORPHAN_TICKS`].
/// - `snapshot_ok`: whether the system-wide process snapshot succeeded.
/// - `restore_pending`: whether the record carries a restore-pending marker
///   (a boot-restore typed/queued `claude --resume` whose handshake has not
///   been verified yet — or the restore FAILED and awaits an operator retry).
///   A mid-restore record is uncertainty by definition: the restored pane may
///   be a plain shell whose resume never landed, and flipping it closed
///   would destroy the durable `open` state the next attempt needs. So while
///   pending, only a confident "alive and busy" (KeepAlive) observation
///   passes through — which also lets the caller self-heal a stale marker —
///   and every other outcome becomes Skip. In particular a mid-restore row
///   (which keeps its OLD `terminal_id` until the deferred re-assert) must
///   never accumulate no-match ticks: `None` + pending ⇒ Skip, not
///   `NoMatchWait`.
/// - `confirmed`: whether the record carries a `confirmed_at` — i.e. a provider
///   session actually started in this terminal (`POST /control/session-open`,
///   or a transcript-anchored reconcile bind). Every PTY gets a PROVISIONAL
///   record at spawn by design (`apply_identity_seam`, `confirmed_at: None`) so
///   restore has a pre-minted identity; a bare PowerShell pane that never runs
///   a provider therefore holds an unconfirmed record forever. Such a record
///   has no session to lose, so it must never be closed `"poll-dead"` — that
///   reason means "a live session died" and buys a
///   [`RESTORABLE_POLL_DEAD_MS`] restore grace, which is exactly wrong here.
///   Handled symmetrically to `restore_pending`: a parameter that rewrites the
///   base outcome rather than a new lifecycle state (the record already carries
///   the fact).
/// Which session PLANE's evidence governs a lifecycle record's liveness.
///
/// The lifecycle store holds records from two disjoint planes. Terminal-plane
/// records are bound to a `TerminalManager` PTY and are correctly judged by
/// [`classify`]'s `live_is_alive` / `claude_present` inputs. **Phase-2
/// orchestration workers are not**: `dispatch_subtask`
/// (`orchestration_loop/ai_session_executor.rs`) records a worker with
/// `terminal_id == task_run_id` — a value that is not a terminal id — so
/// neither the live-terminal index nor the `(page_id, title, working_dir)`
/// fallback can ever match it. `live_is_alive` is therefore `None` on EVERY
/// tick by construction, and the terminal-plane orphan debounce closed a
/// perfectly live worker's coord session row after
/// [`NO_TERMINAL_ORPHAN_TICKS`].
///
/// A worker's liveness lives in the `SessionManager`, so the poll reads it
/// there and hands the answer in as this parameter. Modelled on the
/// `restore_pending` precedent: a parameter that rewrites the base outcome,
/// not a new lifecycle state (the record already carries the fact, in
/// `task_run_id`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerPlane {
    /// Not a worker record (`task_run_id` is `None`) — classify from
    /// `live_is_alive` exactly as before.
    NotWorker,
    /// A worker record still registered in the `SessionManager`. Alive.
    Registered,
    /// A worker record no longer registered — the worker is genuinely gone,
    /// so the record must still be retired (nothing else in the
    /// orchestration loop closes it).
    Gone,
    /// A worker record whose plane could not be consulted (no
    /// `SessionManager` in app state). Uncertainty: never close.
    Unknown,
}

pub fn classify(
    live_is_alive: Option<bool>,
    claude_present: bool,
    consecutive_dead: u32,
    consecutive_no_match: u32,
    snapshot_ok: bool,
    restore_pending: bool,
    confirmed: bool,
    worker_plane: WorkerPlane,
) -> PollAction {
    // Worker-plane arm — HIGHEST precedence, ahead of the snapshot guard.
    // A Phase-2 orchestration worker has no `TerminalManager` PTY by design,
    // so every terminal-plane input below is meaningless for it: the process
    // snapshot is irrelevant (its liveness is an in-process `SessionManager`
    // read, not a PID walk), and `live_is_alive` is `None` on every tick, which
    // the orphan debounce would turn into a close of a live session. Judge it
    // by its own plane or not at all.
    match worker_plane {
        // Registered in the SessionManager — alive, and idle is normal for a
        // worker parked between turns. Never a close.
        WorkerPlane::Registered => return PollAction::KeepAlive,
        // Gone from the SessionManager. The record must still be retired:
        // nothing in `orchestration_loop` ever closes a worker row, so this is
        // the only path that does. Reuse the existing non-restorable orphan
        // variant rather than minting a new `PollAction` and a new
        // `close_reason` — "no terminal, and the worker is gone" is exactly
        // what `CloseNoTerminal` already means.
        WorkerPlane::Gone => return PollAction::CloseNoTerminal,
        // The plane could not be consulted. Uncertainty dominates, same as a
        // failed process snapshot below.
        WorkerPlane::Unknown => return PollAction::Skip,
        WorkerPlane::NotWorker => {}
    }
    if !snapshot_ok {
        return PollAction::Skip;
    }
    let base = match live_is_alive {
        None => {
            if consecutive_no_match < NO_TERMINAL_ORPHAN_TICKS {
                PollAction::NoMatchWait
            } else {
                PollAction::CloseNoTerminal
            }
        }
        Some(false) => PollAction::Close,
        Some(true) => {
            if claude_present {
                // A live Claude in the inclusive subtree — KeepAlive
                // unconditionally, no matter how long it has idled. This is the
                // floor that fixes the false poll-dead close of idle agents.
                PollAction::KeepAlive
            } else if consecutive_dead < LIVE_SHELL_DEAD_TICKS {
                // No Claude present, but debounce several ticks before closing:
                // absorbs a transient snapshot miss, and a restart in this
                // window does not drop a still-live session.
                PollAction::NeedsConfirm
            } else {
                // Claude absent for the full debounce — genuine poll-dead (e.g.
                // operator quit claude and only the bare shell PID lingers).
                PollAction::Close
            }
        }
    };
    // Restore-pending guard arm (never-close-on-uncertainty): a record mid-
    // restore (or whose restore failed) must keep its `open` state intact —
    // never Close, and don't even accumulate NeedsConfirm or no-match ticks
    // against it.
    if restore_pending && base != PollAction::KeepAlive {
        return PollAction::Skip;
    }
    // Never-confirmed guard: a provisional record whose provider never started
    // cannot have "died". Rewrite the poll-dead close to the non-restorable
    // `never-started` close so a bare shell is not preserved as a restore
    // candidate — and so `poll-dead` keeps meaning what it says.
    //
    // It stays NARROW deliberately, and widening it to `CloseNoTerminal` is NOT
    // the worker fix (a 2026-07-26 plan proposed exactly that; the 2026-09-04
    // vet refuted it). `CloseNeverStarted` is *also* a close — `main.rs` handles
    // it with `record_close(.., "never-started")`, which fires the same close
    // observer — so the rewrite would change a `close_reason` string and leave a
    // live worker's coord row closed all the same. The plane arm at the top of
    // this function is the fix; `classify_never_confirmed_leaves_other_arms_intact`
    // pins this guard's narrowness.
    if !confirmed && base == PollAction::Close {
        return PollAction::CloseNeverStarted;
    }
    base
}

/// How the liveness poll matched an open row to a live terminal this tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PollTerminalMatch {
    /// The row's own `terminal_id` is a live terminal.
    Direct,
    /// Matched only by the `(page_id, title, working_dir)` fallback — some
    /// live terminal carries the row's page, title and directory, which does
    /// not make it the row's terminal.
    Fallback,
    /// No live terminal matched.
    Unmatched,
}

impl PollTerminalMatch {
    /// How `rec` matched this tick: `Direct` when its own (non-blank)
    /// `terminal_id` is live per `is_live_terminal`, else `Fallback` when the
    /// `(page_id, title, working_dir)` lookup found a terminal, else
    /// `Unmatched`. A held row is unbound
    /// ([`SessionLifecycleStore::mark_awaiting_account`]), so it can never match
    /// `Direct` — in particular not through its old terminal id once that id
    /// hosts another session.
    pub fn for_record(
        rec: &TerminalSessionRecord,
        is_live_terminal: impl Fn(&str) -> bool,
        fallback_matched: bool,
    ) -> Self {
        if !rec.terminal_id.trim().is_empty() && is_live_terminal(&rec.terminal_id) {
            Self::Direct
        } else if fallback_matched {
            Self::Fallback
        } else {
            Self::Unmatched
        }
    }
}

/// The liveness poll's awaiting-account branch, for one open row this tick:
/// `true` = the poll must SKIP the row (no classification, no close).
///
/// A `needs-account` row the boot restore left for the operator has no
/// terminal BY DESIGN. Closing it `no-terminal` would drop it from the roster
/// and from the next boot's restore set while the operator is still being
/// asked about it, so a row that is awaiting an account is held
/// ([`SessionLifecycleStore::poll_hold_awaiting_account`] — never refreshed,
/// bounded by the prune age) and its close-detection streaks are reset, so a later
/// end of the hold starts counting from zero. Only a [`PollTerminalMatch::Direct`]
/// match releases the hold — the row's own terminal is live, so it is classified
/// as usual. A held row is unbound, so it reaches `Direct` only once a bind
/// (a verified resume) has already ended the hold; never through its OLD
/// terminal id, which may now host another session. A
/// [`PollTerminalMatch::Fallback`] match does NOT release it: another tab on the
/// same page, title and directory is not this row's resume, and classifying the
/// row against it would let that tab's shell close the held row.
pub fn poll_holds_unmatched_row(
    store: &SessionLifecycleStore,
    claude_session_id: &str,
    terminal_match: PollTerminalMatch,
    now_ms: i64,
    consecutive_dead: &mut HashMap<String, u32>,
    consecutive_no_match: &mut HashMap<String, u32>,
) -> bool {
    if terminal_match == PollTerminalMatch::Direct
        || !store.poll_hold_awaiting_account(claude_session_id, now_ms)
    {
        return false;
    }
    consecutive_dead.remove(claude_session_id);
    consecutive_no_match.remove(claude_session_id);
    true
}

/// The `close_reason` a [`PollAction::Close`] verdict should actually be
/// recorded under, given the record's last restore outcome.
///
/// ## Why `Close` alone is not enough to pick `"poll-dead"`
///
/// `classify`'s `Close` verdict means "a live shell went idle with no Claude
/// present for the full debounce" — true both for a real conversation that
/// ended AND for a bare shell that [`RESTORE_TIER_TERMINAL_ONLY`] already
/// established can never host one (a confirmed session with no transcript, or
/// a terminal-only-tier provider — see `classifyRestoreAction` on the
/// frontend, `useTerminalInitialization.ts`). The two cases must not share a
/// close reason: `"poll-dead"` is what [`SessionLifecycleStore::
/// restorable_records`] grants grace to (`RESTORABLE_POLL_DEAD_MS`), which is
/// exactly right for a session that might come back and exactly wrong for a
/// tile that was already restored honestly empty. Recording the latter as
/// `"poll-dead"` hands the *next* restore pass the same unresumable record,
/// which recreates the same blank terminal, which idles out the same way —
/// an unbounded cycle of empty panes, each one costing a `terminal_create`
/// call (and its resource-guard check) on every first activation of the page,
/// with nothing ever actually restored.
///
/// ## Why this is a separate function rather than a `classify` arm
///
/// `classify` is deliberately kept narrow (see the `never-confirmed` guard's
/// own comment above, and `classify_never_confirmed_leaves_other_arms_intact`
/// — a 2026-09-04 vet already refused widening `classify` itself for a
/// related worker-retirement case). This reads `restore_tier`, a field
/// `classify` never touches and has no other reason to accept; keeping the
/// decision here, downstream of the verdict, leaves `classify`'s contract
/// untouched. Pure + exported so the mapping is unit-testable without the
/// poll loop.
///
/// Any reason other than `"poll-dead"` / `"pty-exit"` is automatically
/// excluded from `restorable_records`'s grace window (it matches those two
/// strings by name and falls through to "not restorable" otherwise), so
/// picking a new honest reason here needs no matching change there.
pub fn close_reason_for_dead_shell(restore_tier: Option<&str>) -> &'static str {
    if restore_tier == Some(RESTORE_TIER_TERMINAL_ONLY) {
        "terminal-only-idle"
    } else {
        "poll-dead"
    }
}

/// A minimal open record for `id`, for tests OUTSIDE this module that need a
/// session the store knows (the finish route, the registrar).
#[cfg(test)]
pub(crate) fn test_open_record(id: &str) -> TerminalSessionRecord {
    TerminalSessionRecord {
        claude_session_id: id.to_string(),
        config_dir: None,
        working_dir: None,
        page_id: "default".to_string(),
        zone_index: 0,
        title: None,
        terminal_id: format!("term-{id}"),
        opened_at: 0,
        last_seen_at: 0,
        state: "open".to_string(),
        closed_at: None,
        close_reason: None,
        provider: DEFAULT_PROVIDER.to_string(),
        origin: None,
        restore_pending_at: None,
        awaiting_account_since: None,
        confirmed_at: None,
        handle: None,
        account_label: None,
        account_wrapper: None,
        session_name: None,
        name_source: None,
        tenant_id: None,
        task_run_id: None,
        bypass_permissions: None,
        restored_from_boot_at: None,
        restore_tier: None,
        finished_at: None,
        wind_down_outcome: None,
        wind_down_at: None,
        wind_down_arm: None,
        finish_reason: None,
        finish_synced: false,
        spawn_device_default: None,
        adopted_from: None,
        last_session_start_at: None,
    }
}

#[cfg(test)]
mod tests;
