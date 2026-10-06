//! Operator-touch CLOSE path — the runner closes the touches it opened (plan
//! `2026-10-05-operator-touch-close-path`, Phase 3; D1, D3, D4).
//!
//! [`crate::session::operator_touch`] OPENS a touch: it mints the idempotency
//! key `<coord_session_id>:<kind>:<bucket>` on EMIT time and enqueues the open
//! into the session outbox. Nothing on coord's side can ever resolve that row
//! by itself (the insert is `ON CONFLICT DO NOTHING`, and a repeat carrying a
//! `resolution` is discarded), so the opener — the only party that knows the
//! exact key without guessing — closes it, through coord's append-only close
//! sidecar `POST /coord/sessions/operator-touch/close`.
//!
//! ## What is remembered, and where
//!
//! Every open the runner may later close (`idle_at_prompt`,
//! `permission_prompt`) is remembered in an [`OpenTouchStore`]: a SET of open
//! touches per terminal and kind, keyed by the idempotency key, each carrying
//! the full open payload and the open's outbox `recorded_at`. The key is
//! bucketed on emit time, so it cannot be recomputed later — it has to be
//! kept.
//!
//! An open is remembered — and persisted — BEFORE its outbox append
//! (`operator_touch::emit` with a terminal to track), so an input landing
//! during the append's fsync still finds it and a crash after the append
//! still leaves it on disk; a failed append forgets it again (and persists
//! that). The append's own `recorded_at` then CONFIRMS the open.
//!
//! A crash BETWEEN that persist and the append leaves an open on disk that
//! was never appended. The restart closes it `abandoned` like any reloaded
//! open, and because the close carries its open (D1) coord inserts the open
//! with it. Acceptable: the touch was really observed — only its own open row
//! was lost — and the record is honest about how it ended. A touch a
//! close signal has taken stays in the persisted file until its close is
//! durably appended, so a crash between the two loses neither.
//!
//! The store is persisted BESIDE the session outbox, as
//! `<outbox>.operator-touch-open.json` — the outbox's own sidecar convention
//! (`<outbox>.quarantine.jsonl`, `<outbox>.cursor`) — rewritten atomically on
//! every change, so a runner restart does not orphan an open.
//!
//! `session_exit` is never remembered: it is terminal by nature and closed
//! only by coord's sweep (plan D4/D5).
//!
//! ## Close signals (plan D4)
//!
//! | signal | `resolution` | `close_actor_class` |
//! |---|---|---|
//! | first input after the open, from a `human` door | `answered` | `human` |
//! | …from an `unknown` door | `answered` | `unknown` |
//! | …from an automated door | `self_resolved` | `none` |
//! | the idle episode ends with no input (the agent resumed by itself — a task completion, a monitor, a wakeup) | `self_resolved` | `none` |
//! | the terminal dies (or is gone after a restart) | `abandoned` | — |
//!
//! The episode-end row applies to `idle_at_prompt` only and is observed by the
//! idle watcher (`operator_touch_watch`): its per-terminal latch sees the pane
//! leave the idle episode it fired for. As a backstop, a NEW idle episode on a
//! terminal still holding an older `idle_at_prompt` open closes the older one
//! the same way before opening its own — so a later keystroke never closes a
//! pile of stale opens as `answered` with inflated waits. ONE wait is ONE
//! touch: while a terminal holds a live `idle_at_prompt` open
//! ([`OpenTouchStore::holds_live_idle_open`]), the watcher emits no new idle
//! touch for it, even when a repaint restarts the idle tracker's `since_ms`.
//! A new idle touch fires only once that open was closed by some path (an
//! input, the episode end, the terminal's death). So the backstop never
//! reaches an open whose episode is live — such an open suppresses the new
//! episode before the backstop runs — and finds nothing in the repaint case.
//! What it DOES close is an older idle open put back after a failed close: it
//! retries that close with its decided words before the new episode opens. Episode end means
//! TWO consecutive `Busy` ticks: an idle pane that merely redraws (a resize, a
//! status-line refresh, hook output) restarts the idle tracker's `since_ms`
//! but is still waiting, and ends nothing.
//!
//! **A close that fails to be recorded keeps its words.** When a close's
//! enqueue or append fails, the open goes back into the store carrying the
//! resolution that close decided ([`OpenTouch::failed_close`]). It no longer
//! suppresses a new idle touch (its wait is over), and the next path to take
//! it — input, episode end, the backstop, death — retries those exact words:
//! a failed `answered` is never reworded `self_resolved`, and a failed
//! episode-end `self_resolved` or death `abandoned` is never reworded
//! `answered`. A close fails this way on a failed outbox append (a local disk
//! fault) or a failed enqueue (the close queue full, or the close thread
//! dead). Its retry carries NO `observed_wait_ms`: measured at the retry, the
//! wait would include the retry delay, which plan D3 counts as fabricated.
//!
//! **Episode end vs. input.** The two can race — an input at the moment the
//! tick reads its second busy frame. Both take opens under the store's ONE
//! lock, so whichever runs first takes the open and the other finds nothing:
//! a touch is never closed twice, and only the wording (`self_resolved` vs.
//! `answered`) depends on which won.
//!
//! **Accepted limit — unanswered permission prompts.** A `permission_prompt`
//! the operator never answers by keystroke (dismissed some other way, or the
//! agent moved on) has no episode-end signal: plan D4 defines its close as
//! the next input. It closes on that input, with a wait that includes the
//! time after the prompt stopped mattering, or `abandoned` when the terminal
//! dies; coord's sweep (plan D5) bounds whatever a dead runner leaves.
//!
//! The runner's own `/exit` (`PtyWriteCaller::GracefulExit`) is not an answer
//! but the session ending: it closes nothing on input, and the dying
//! terminal's opens are closed `abandoned`.
//!
//! The input class is qontinui-runner#1857's
//! [`crate::terminal::operator_input::actor_class_of`]; the signal is its
//! `on_input` seam, which runs on BOTH input funnels (`TerminalSession::write`
//! and `submit_prompt`) and — for `write` — only AFTER the
//! `is_terminal_control_response` exclusion, so a focus report from clicking
//! into a pane closes nothing. This module adds no second classifier.
//!
//! ## The close carries its open (plan D1)
//!
//! The outbox acks any 2xx and only retries transport failures, a dropped open
//! is never recreated, and a close could otherwise drain before its open. So
//! every close record carries the FULL open payload; coord inserts the open
//! (`ON CONFLICT DO NOTHING`) and the close in one transaction. There is no
//! retry on `touch_not_found`.
//!
//! ## The wait is measured here, on one clock (plan D3)
//!
//! coord's `emitted_at` and `closed_at` are both DRAIN times, so after any
//! backlog their difference reads ≈0. The close therefore carries
//! `observed_wait_ms` = the close record's outbox `recorded_at` − the open's,
//! both stamped by this device. It is computed at drain time from the close
//! row's own `recorded_at` (see [`close_body`]), so it is exactly that
//! difference rather than an estimate taken before the append.
//!
//! The open's side is the CONFIRMED outbox `recorded_at` of the open row. An
//! input can take a touch while its open is still in the append; the close
//! re-reads the confirmed value just before it is recorded, and omits the
//! wait if the open was never confirmed rather than measure from a
//! provisional stamp.
//!
//! An open RELOADED from disk after a restart has no observable end: the
//! process that could have seen it end is gone, and its terminal with it.
//! Such a touch is closed `abandoned` WITHOUT `observed_wait_ms` — a duration
//! that silently included the runner's downtime would be a fabricated wait.
//!
//! ## Kill switch
//!
//! `QONTINUI_OPERATOR_TOUCH_HOOK=0` ([`operator_touch::armed`]) disables the
//! close path exactly as it disables the opens: nothing is remembered, closed
//! or swept, and opens persisted by an earlier armed run stay on disk until
//! the switch is lifted.
//!
//! ## Off the keystroke path
//!
//! [`on_terminal_input`] runs inside the keystroke funnel. A process-wide
//! atomic count of closable opens short-circuits it before the env read, the
//! app-state lookup and the store's mutex — the common case, a keystroke on a
//! box with nothing open, costs one relaxed load. Otherwise it only takes a
//! short in-memory lock; recording the close (an fsync'd outbox append) and
//! re-persisting the store happen on a dedicated thread behind a bounded
//! queue. A full queue puts the touches back, so any close signal closes them.
//!
//! ## Same-bucket repeats
//!
//! A second touch of the same kind on the same session inside one 60-second
//! bucket mints the SAME key. If the first was already closed, the store
//! remembers the key again and a later signal closes it again; coord answers
//! the second close `already_closed` and the first close stands. Harmless by
//! construction, and rare: a new idle episode needs 60 s of quiet.
//!
//! ## What this path can still lose (best-effort, by design)
//!
//! - **The transport budget.** Closes ride the outbox's best-effort posture:
//!   a close that fails transport `BEST_EFFORT_MAX_ATTEMPTS` times, or that
//!   coord refuses with a 4xx, is Ack-dropped.
//! - **A dead close thread.** If the close thread cannot be spawned or dies,
//!   input closes are put back with their decided words and retried by every
//!   later signal on that terminal, finally by its death.
//!
//! coord's sweep (plan D5) bounds the open set in both cases: it closes every
//! open touch of a closed session `abandoned` past its grace window.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, OnceLock};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tracing::warn;
use uuid::Uuid;

use super::local_store::{OutboxEvent, OutboxWriter};
use super::operator_touch::{self, KIND_IDLE_AT_PROMPT, KIND_PERMISSION_PROMPT};
use super::{SessionEventKind, SessionRegistry};
use crate::terminal::operator_input::{actor_class_of, ActorClass};
use crate::terminal::session::PtyWriteCaller;

// ===========================================================================
// Vocabulary — coord's close route's closed words
// ===========================================================================

/// How a touch ended. coord validates the word (422 on an unknown one).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Resolution {
    /// Input arrived from a door a human (or an ambiguous door) uses.
    Answered,
    /// Input arrived from an automated producer — the agent's own machinery
    /// moved past the wait without a person.
    SelfResolved,
    /// The terminal died with the touch still open.
    Abandoned,
}

impl Resolution {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Answered => "answered",
            Self::SelfResolved => "self_resolved",
            Self::Abandoned => "abandoned",
        }
    }
}

/// Who the closing input's door says typed. `None` is the word coord takes
/// for an automated door; a close with no input behind it (`abandoned`)
/// carries no class at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloseActorClass {
    Human,
    Unknown,
    None,
}

impl CloseActorClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Human => "human",
            Self::Unknown => "unknown",
            Self::None => "none",
        }
    }
}

/// THE input-close mapping (plan D4), a projection of #1857's
/// [`actor_class_of`]: `unknown` stays `unknown` and is never rounded to
/// `human`. Pure.
pub fn input_close_words(class: Option<ActorClass>) -> (Resolution, CloseActorClass) {
    match class {
        Some(ActorClass::Human) => (Resolution::Answered, CloseActorClass::Human),
        Some(ActorClass::Unknown) => (Resolution::Answered, CloseActorClass::Unknown),
        None => (Resolution::SelfResolved, CloseActorClass::None),
    }
}

/// The kinds the runner closes. `session_exit` is terminal — never closed by
/// the runner (plan D4) — and `question` / `gate` are never opened here.
pub fn is_runner_closable_kind(kind: &str) -> bool {
    kind == KIND_IDLE_AT_PROMPT || kind == KIND_PERMISSION_PROMPT
}

// ===========================================================================
// The open set
// ===========================================================================

/// One open the runner remembers so it can close it later.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpenTouch {
    /// The runner terminal the touch was observed on.
    pub terminal_id: String,
    /// `idle_at_prompt` | `permission_prompt`.
    pub kind: String,
    /// The outbox lane the open rode — the close rides the same one, so it
    /// drains after its open in the per-session seq chain.
    pub coord_session_id: Uuid,
    /// `<coord_session_id>:<kind>:<bucket>` exactly as emitted.
    pub idempotency_key: String,
    /// The full `POST /coord/sessions/operator-touch` body the open sent —
    /// carried by the close (plan D1).
    pub open_payload: Value,
    /// The open's outbox `recorded_at` — the start of the measured wait.
    /// Provisional until [`Self::open_confirmed`].
    pub open_recorded_at: DateTime<Utc>,
    /// The open's append landed and `open_recorded_at` is that row's own
    /// stamp. An unconfirmed open's close carries no wait.
    #[serde(default)]
    pub open_confirmed: bool,
    /// A close signal already decided how this touch ended, but its close
    /// could not be enqueued or appended, so the open was put back carrying
    /// that decision. Whichever path takes it next (input, episode end, the
    /// backstop, death) RETRIES those exact words — an input-derived answer is
    /// never reworded `self_resolved`, and an episode end or a death is never
    /// reworded `answered`. Its wait has ended, so it never suppresses a new
    /// idle touch ([`OpenTouchStore::holds_live_idle_open`]).
    #[serde(default)]
    pub failed_close: Option<FailedClose>,
    /// The owning session's tenant, captured at open time. The close is
    /// usually drained after the session left the registry (always, after a
    /// restart), so the drain could no longer resolve it there; the close
    /// record carries it as top-level `tenant_id`, which the drain's
    /// `record_session_tenant` reads to pick the credential. Never sent in the
    /// wire body.
    #[serde(default)]
    pub tenant_id: Option<Uuid>,
    /// Loaded from disk by a later process: its end was not observed, so its
    /// close carries no wait. Never persisted.
    #[serde(skip)]
    pub reloaded: bool,
}

/// Closable opens across every store in the process — the keystroke path's
/// short-circuit ([`on_terminal_input`]). Each store publishes the change in
/// its own `open` set's size under its lock ([`Sets::published`]).
static OPEN_COUNT: AtomicUsize = AtomicUsize::new(0);

/// The words of a close that was decided but could not be recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailedClose {
    pub resolution: Resolution,
    pub actor_class: Option<CloseActorClass>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct StoreFile {
    version: u32,
    open: Vec<OpenTouch>,
}

const STORE_FILE_VERSION: u32 = 1;

/// The runner's remembered open touches, keyed by idempotency key (a SET:
/// re-emitting the same key keeps the FIRST open, which is the one coord's
/// `ON CONFLICT DO NOTHING` kept). Persisted beside the session outbox; see
/// the module docs.
///
/// A touch a close signal has TAKEN moves to `in_flight` until its close is
/// durably appended ([`Self::settle`]) or fails ([`Self::put_back`]). The file
/// is written from `open ∪ in_flight`, so a crash between the take and the
/// append loses neither the open nor its close — the restart closes it
/// `abandoned` instead.
#[derive(Debug)]
pub struct OpenTouchStore {
    /// `None` = in-memory only (no outbox path to sit beside).
    path: Option<PathBuf>,
    sets: Mutex<Sets>,
    /// Serializes file rewrites. A writer snapshots the sets while holding it,
    /// so the last rewrite to finish always carries every earlier change, and
    /// the keystroke path's lock on `sets` is never held across an fsync.
    persist_lock: Mutex<()>,
}

#[derive(Debug, Default)]
struct Sets {
    /// Closable: no close signal has taken these yet.
    open: BTreeMap<String, OpenTouch>,
    /// Taken by a close signal; the close is not yet durably appended.
    in_flight: BTreeMap<String, OpenTouch>,
    /// How many of `open` this store has added to [`OPEN_COUNT`].
    published: usize,
}

impl Sets {
    /// Bring [`OPEN_COUNT`] in line with `open.len()`. Called under the lock
    /// after every change to `open`.
    fn publish(&mut self) {
        let now = self.open.len();
        if now > self.published {
            OPEN_COUNT.fetch_add(now - self.published, Ordering::Relaxed);
        } else if now < self.published {
            OPEN_COUNT.fetch_sub(self.published - now, Ordering::Relaxed);
        }
        self.published = now;
    }
}

/// `<outbox>.operator-touch-open.json`.
pub fn store_path_beside(outbox_path: &Path) -> PathBuf {
    let name = outbox_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "session-outbox.jsonl".to_string());
    outbox_path.with_file_name(format!("{name}.operator-touch-open.json"))
}

impl Drop for OpenTouchStore {
    fn drop(&mut self) {
        let mut g = self.lock();
        g.open.clear();
        g.publish();
    }
}

impl OpenTouchStore {
    /// An in-memory store (nothing persisted).
    #[cfg(test)]
    pub fn in_memory() -> Self {
        Self {
            path: None,
            sets: Mutex::new(Sets::default()),
            persist_lock: Mutex::new(()),
        }
    }

    /// Open (or start) the store beside the session outbox, loading every
    /// open a previous process persisted. Each loaded open is marked
    /// `reloaded`. An unreadable file is warned about and treated as empty —
    /// coord's sweep (plan D5) still bounds whatever it held.
    pub fn open_beside(outbox_path: &Path) -> Self {
        let path = store_path_beside(outbox_path);
        let mut open = BTreeMap::new();
        match std::fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<StoreFile>(&bytes) {
                Ok(file) if file.version != STORE_FILE_VERSION => warn!(
                    path = %path.display(),
                    version = file.version,
                    dropped = file.open.len(),
                    "operator_touch_close: open-touch store has an unknown version — \
                     starting empty"
                ),
                Ok(file) => {
                    let mut unclosable = 0usize;
                    for mut touch in file.open {
                        if !is_runner_closable_kind(&touch.kind) {
                            unclosable += 1;
                            continue;
                        }
                        touch.reloaded = true;
                        open.insert(touch.idempotency_key.clone(), touch);
                    }
                    if unclosable > 0 {
                        warn!(
                            path = %path.display(),
                            dropped = unclosable,
                            "operator_touch_close: dropped reloaded opens of a kind the \
                             runner never closes"
                        );
                    }
                }
                Err(e) => warn!(
                    path = %path.display(),
                    error = %e,
                    "operator_touch_close: open-touch store unreadable — starting empty"
                ),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => warn!(
                path = %path.display(),
                error = %e,
                "operator_touch_close: open-touch store unreadable — starting empty"
            ),
        }
        let mut sets = Sets {
            open,
            in_flight: BTreeMap::new(),
            published: 0,
        };
        sets.publish();
        Self {
            path: Some(path),
            sets: Mutex::new(sets),
            persist_lock: Mutex::new(()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Sets> {
        self.sets.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Remember one open IN MEMORY ONLY. `false` (nothing changed) for a
    /// kind the runner never closes, or a key already held — the first open
    /// of a key is kept.
    pub fn remember_provisional(&self, touch: OpenTouch) -> bool {
        if !is_runner_closable_kind(&touch.kind) {
            return false;
        }
        let mut g = self.lock();
        if g.open.contains_key(&touch.idempotency_key)
            || g.in_flight.contains_key(&touch.idempotency_key)
        {
            return false;
        }
        g.open.insert(touch.idempotency_key.clone(), touch);
        g.publish();
        true
    }

    /// Remember one open and persist.
    #[cfg(test)]
    pub fn remember(&self, touch: OpenTouch) -> std::io::Result<()> {
        if self.remember_provisional(touch) {
            self.persist()
        } else {
            Ok(())
        }
    }

    /// The open's append landed: stamp its exact outbox `recorded_at` (wherever
    /// it now sits — an input may already have taken it) and persist.
    pub fn confirm(&self, key: &str, recorded_at: DateTime<Utc>) -> std::io::Result<()> {
        {
            let mut g = self.lock();
            let sets = &mut *g;
            if let Some(t) = sets
                .open
                .get_mut(key)
                .or_else(|| sets.in_flight.get_mut(key))
            {
                t.open_recorded_at = recorded_at;
                t.open_confirmed = true;
            }
        }
        self.persist()
    }

    /// The open's append failed: forget it unless a close signal already took
    /// it (that close carries the open, so coord still records both — D1).
    pub fn forget(&self, key: &str) -> std::io::Result<()> {
        {
            let mut g = self.lock();
            g.open.remove(key);
            g.publish();
        }
        self.persist()
    }

    /// Before recording closes: refresh each taken touch's open stamp from
    /// the store, where [`Self::confirm`] may have landed it after the take.
    pub fn refresh_taken(&self, closes: &mut [PendingClose]) {
        let g = self.lock();
        for c in closes {
            if let Some(t) = g.in_flight.get(&c.touch.idempotency_key) {
                c.touch.open_recorded_at = t.open_recorded_at;
                c.touch.open_confirmed = t.open_confirmed;
            }
        }
    }

    /// Take every open on `terminal_id` into flight. The caller records the
    /// closes, then [`Self::settle`]s them — or [`Self::put_back`]s on failure.
    pub fn take_for_terminal(&self, terminal_id: &str) -> Vec<OpenTouch> {
        self.take_where(|t| t.terminal_id == terminal_id)
    }

    /// Take every open of `kind` on `terminal_id` into flight.
    pub fn take_for_terminal_kind(&self, terminal_id: &str, kind: &str) -> Vec<OpenTouch> {
        self.take_where(|t| t.terminal_id == terminal_id && t.kind == kind)
    }

    fn take_where(&self, pred: impl Fn(&OpenTouch) -> bool) -> Vec<OpenTouch> {
        let mut g = self.lock();
        if g.open.is_empty() {
            return Vec::new();
        }
        let keys: Vec<String> = g
            .open
            .iter()
            .filter(|(_, t)| pred(t))
            .map(|(k, _)| k.clone())
            .collect();
        let mut taken = Vec::with_capacity(keys.len());
        for k in keys {
            if let Some(t) = g.open.remove(&k) {
                g.in_flight.insert(k, t.clone());
                taken.push(t);
            }
        }
        g.publish();
        taken
    }

    /// Their closes are durably appended: drop them, then persist.
    pub fn settle(&self, keys: &[String]) -> std::io::Result<()> {
        {
            let mut g = self.lock();
            for k in keys {
                g.in_flight.remove(k);
            }
        }
        self.persist()
    }

    /// Their closes could not be appended: back to closable, each carrying
    /// the words its close had decided ([`OpenTouch::failed_close`]) so the
    /// retry keeps them, then persist.
    pub fn put_back(&self, closes: Vec<PendingClose>) {
        {
            let mut g = self.lock();
            for c in closes {
                let key = c.touch.idempotency_key.clone();
                let mut t = g.in_flight.remove(&key).unwrap_or(c.touch);
                t.failed_close = Some(FailedClose {
                    resolution: c.resolution,
                    actor_class: c.actor_class,
                });
                g.open.entry(key).or_insert(t);
            }
            g.publish();
        }
        if let Err(e) = self.persist() {
            static LAST_WARN_SECS: AtomicI64 = AtomicI64::new(i64::MIN);
            if once_a_minute(&LAST_WARN_SECS) {
                warn!(
                    error = %e,
                    "operator_touch_close: open-touch store rewrite failed after put-back \
                     (warned at most once a minute)"
                );
            }
        }
    }

    /// Does `terminal_id` hold an `idle_at_prompt` open whose wait is still
    /// live — closable and with no failed close (a put-back open's wait has
    /// already ended, however it ended)? While it does, the idle watcher emits
    /// no new idle touch for that terminal: one wait is one touch.
    pub fn holds_live_idle_open(&self, terminal_id: &str) -> bool {
        self.lock().open.values().any(|t| {
            t.terminal_id == terminal_id
                && t.kind == KIND_IDLE_AT_PROMPT
                && t.failed_close.is_none()
        })
    }

    /// The terminals that currently hold a closable open. Read BEFORE
    /// snapshotting the live terminals (see [`close_orphaned`]).
    pub fn terminal_ids(&self) -> Vec<String> {
        let g = self.lock();
        let mut ids: Vec<String> = g.open.values().map(|t| t.terminal_id.clone()).collect();
        ids.sort();
        ids.dedup();
        ids
    }

    /// Every closable open, in key order (in-flight ones excluded).
    #[cfg(test)]
    pub fn snapshot(&self) -> Vec<OpenTouch> {
        self.lock().open.values().cloned().collect()
    }

    /// Rewrite the file from `open ∪ in_flight` (atomic temp + rename).
    pub fn persist(&self) -> std::io::Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let _p = self.persist_lock.lock().unwrap_or_else(|e| e.into_inner());
        let open = {
            let g = self.lock();
            g.open
                .values()
                .chain(g.in_flight.values())
                .cloned()
                .collect()
        };
        let file = StoreFile {
            version: STORE_FILE_VERSION,
            open,
        };
        let bytes = serde_json::to_vec(&file).map_err(std::io::Error::other)?;
        crate::fs_atomic::atomic_write(path, &bytes)
    }
}

// ===========================================================================
// The close record and its wire body
// ===========================================================================

/// One close to record.
#[derive(Debug, Clone, PartialEq)]
pub struct PendingClose {
    pub touch: OpenTouch,
    pub resolution: Resolution,
    pub actor_class: Option<CloseActorClass>,
}

impl PendingClose {
    /// The close of `touch` worded by THIS signal — unless an earlier close
    /// of it already decided its words and failed to be recorded, in which
    /// case the retry keeps those ([`OpenTouch::failed_close`]).
    pub fn worded(
        touch: OpenTouch,
        resolution: Resolution,
        actor_class: Option<CloseActorClass>,
    ) -> Self {
        let (resolution, actor_class) = match touch.failed_close {
            Some(f) => (f.resolution, f.actor_class),
            None => (resolution, actor_class),
        };
        Self {
            touch,
            resolution,
            actor_class,
        }
    }
}

/// The outbox payload of an `operator_touch_close` record. `open_recorded_at`
/// is present only when the wait is observable — a confirmed open that was
/// neither reloaded nor put back after a failed close (a retry's wait would
/// include the retry delay, a fabricated wait per plan D3); the
/// drain turns it into `observed_wait_ms` against the close row's own
/// `recorded_at`.
pub fn close_record_payload(close: &PendingClose) -> Value {
    let mut payload = json!({
        "touch": close.touch.open_payload,
        "resolution": close.resolution.as_str(),
    });
    if let Some(class) = close.actor_class {
        payload["close_actor_class"] = Value::String(class.as_str().to_string());
    }
    if let Some(tenant) = close.touch.tenant_id {
        payload["tenant_id"] = Value::String(tenant.to_string());
    }
    if !close.touch.reloaded && close.touch.open_confirmed && close.touch.failed_close.is_none() {
        payload["open_recorded_at"] = Value::String(
            close
                .touch
                .open_recorded_at
                .to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true),
        );
    }
    payload
}

/// The `POST /coord/sessions/operator-touch/close` body, built at drain time
/// from the outbox payload and the close row's own `recorded_at`. `None` for a
/// payload that carries no open (`touch` absent, or not an open of a kind the
/// runner closes) or no `resolution` — a body the queue cannot fix.
///
/// `observed_wait_ms` is omitted when the open's `recorded_at` is absent
/// (reloaded open), unparsable, or LATER than the close's (a backward clock
/// step) — never clamped to a fabricated zero.
pub fn close_body(payload: &Value, close_recorded_at: DateTime<Utc>) -> Option<Value> {
    let touch = payload.get("touch").filter(|t| {
        t.get("kind")
            .and_then(Value::as_str)
            .is_some_and(is_runner_closable_kind)
    })?;
    let resolution = payload.get("resolution").and_then(Value::as_str)?;
    let mut body = json!({
        "touch": touch,
        "resolution": resolution,
    });
    if let Some(class) = payload.get("close_actor_class").and_then(Value::as_str) {
        body["close_actor_class"] = Value::String(class.to_string());
    }
    let wait_ms = payload
        .get("open_recorded_at")
        .and_then(Value::as_str)
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|open| (close_recorded_at - open.with_timezone(&Utc)).num_milliseconds())
        .filter(|ms| *ms >= 0);
    if let Some(ms) = wait_ms {
        body["observed_wait_ms"] = json!(ms as u64);
    }
    Some(body)
}

/// Append every close in ONE outbox batch (one fsync), each on its touch's own
/// session lane.
pub fn record_closes(
    outbox: &OutboxWriter,
    machine_id: Uuid,
    closes: &[PendingClose],
) -> std::io::Result<()> {
    if closes.is_empty() {
        return Ok(());
    }
    let events = closes
        .iter()
        .map(|c| {
            OutboxEvent::new(
                machine_id,
                c.touch.coord_session_id,
                SessionEventKind::OperatorTouchClose,
                close_record_payload(c),
            )
        })
        .collect();
    outbox.record_batch(events).map(|_| ())
}

/// Record the closes, then persist the store without them. On a failed append
/// the touches go back into the store, so a later signal closes them.
pub fn record_and_persist(
    store: &OpenTouchStore,
    outbox: &OutboxWriter,
    machine_id: Uuid,
    mut closes: Vec<PendingClose>,
) {
    if closes.is_empty() {
        return;
    }
    store.refresh_taken(&mut closes);
    if let Err(e) = record_closes(outbox, machine_id, &closes) {
        static LAST_WARN_SECS: AtomicI64 = AtomicI64::new(i64::MIN);
        if once_a_minute(&LAST_WARN_SECS) {
            warn!(
                error = %e,
                count = closes.len(),
                "operator_touch_close: close append failed — touches kept open for any close \
                 signal (warned at most once a minute)"
            );
        }
        store.put_back(closes);
        return;
    }
    let keys: Vec<String> = closes
        .iter()
        .map(|c| c.touch.idempotency_key.clone())
        .collect();
    if let Err(e) = store.settle(&keys) {
        warn!(
            error = %e,
            "operator_touch_close: open-touch store rewrite failed — a restart may re-close \
             these as abandoned (coord answers already_closed)"
        );
    }
}

// ===========================================================================
// Opening: remember an open BEFORE it is appended
// ===========================================================================

/// Begin tracking an open `operator_touch::emit` is about to append for
/// `terminal_id`, so an input that lands DURING the append (an fsync) still
/// finds it, and persisted, so a crash right after the append still leaves it
/// on disk. [`confirm_tracking`] / [`abort_tracking`] finish it. `None` when
/// nothing was begun (an untracked kind, or a key already held — whose first
/// open stands).
pub fn begin_tracking(
    registry: &SessionRegistry,
    terminal_id: &str,
    coord_session_id: Uuid,
    kind: &str,
    idempotency_key: &str,
    open_payload: &Value,
) -> Option<String> {
    if !is_runner_closable_kind(kind) {
        return None;
    }
    let tenant_id = registry
        .describe_by_id(coord_session_id)
        .ok()
        .and_then(|d| d.intent.tenant_id);
    let store = registry.coord_sync().open_touches();
    let begun = store.remember_provisional(OpenTouch {
        terminal_id: terminal_id.to_string(),
        kind: kind.to_string(),
        coord_session_id,
        idempotency_key: idempotency_key.to_string(),
        open_payload: open_payload.clone(),
        // Provisional — replaced by the outbox row's own `recorded_at`.
        open_recorded_at: Utc::now(),
        open_confirmed: false,
        failed_close: None,
        tenant_id,
        reloaded: false,
    });
    if !begun {
        return None;
    }
    if let Err(e) = store.persist() {
        warn!(
            key = %idempotency_key,
            error = %e,
            "operator_touch_close: open-touch store rewrite failed — kept in memory only"
        );
    }
    Some(idempotency_key.to_string())
}

/// The open's append landed: stamp its exact `recorded_at` and persist.
/// Best-effort: a failed rewrite keeps it in memory for this process.
pub fn confirm_tracking(registry: &SessionRegistry, key: &str, recorded_at: DateTime<Utc>) {
    if let Err(e) = registry
        .coord_sync()
        .open_touches()
        .confirm(key, recorded_at)
    {
        warn!(
            key = %key,
            error = %e,
            "operator_touch_close: open-touch store rewrite failed — kept in memory only"
        );
    }
}

/// The open's append failed: stop tracking it.
pub fn abort_tracking(registry: &SessionRegistry, key: &str) {
    if let Err(e) = registry.coord_sync().open_touches().forget(key) {
        warn!(
            key = %key,
            error = %e,
            "operator_touch_close: open-touch store rewrite failed after a failed open append"
        );
    }
}

// ===========================================================================
// Close signal 1 — input (plan D4, via #1857's `on_input` seam)
// ===========================================================================

/// The input-close decision for one terminal: take every open on it and word
/// each close from the caller's door. Pure over the store.
///
/// The runner's own `/exit` ([`PtyWriteCaller::GracefulExit`]) is NOT an
/// answer: it ends the session, so it closes nothing here and the dying
/// terminal's opens are closed `abandoned` by [`close_orphaned`] (plan D4's
/// "the pane/terminal dies with the touch still open").
pub fn closes_for_input(
    store: &OpenTouchStore,
    terminal_id: &str,
    caller: &PtyWriteCaller,
) -> Vec<PendingClose> {
    if matches!(caller, PtyWriteCaller::GracefulExit) {
        return Vec::new();
    }
    let taken = store.take_for_terminal(terminal_id);
    if taken.is_empty() {
        return Vec::new();
    }
    let (resolution, actor_class) = input_close_words(actor_class_of(caller));
    taken
        .into_iter()
        .map(|touch| PendingClose::worded(touch, resolution, Some(actor_class)))
        .collect()
}

/// Called from `operator_input::on_input` — i.e. for every input write that
/// is not an emulator control response, and every `submit_prompt` — BEFORE
/// its latch, so automated input (which opens no input episode) still closes
/// a touch as `self_resolved`. Never blocks on I/O.
pub fn on_terminal_input(terminal_id: &str, caller: &PtyWriteCaller) {
    if OPEN_COUNT.load(Ordering::Relaxed) == 0 {
        return;
    }
    if !operator_touch::armed() {
        return;
    }
    if test_captured(terminal_id, caller) {
        return;
    }
    let Some(registry) = current_registry() else {
        return;
    };
    close_on_input(&registry, terminal_id, caller);
}

/// The input close against one registry: take the terminal's opens and hand
/// their closes to the close thread.
pub fn close_on_input(registry: &Arc<SessionRegistry>, terminal_id: &str, caller: &PtyWriteCaller) {
    let store = registry.coord_sync().open_touches();
    let closes = closes_for_input(&store, terminal_id, caller);
    if closes.is_empty() {
        return;
    }
    enqueue(Pending {
        registry: registry.clone(),
        store,
        closes,
    });
}

/// Under test, a thread's installed [`test_hook`] takes the signal; in a
/// build it never does.
#[cfg(test)]
fn test_captured(terminal_id: &str, caller: &PtyWriteCaller) -> bool {
    test_hook::capture(terminal_id, caller)
}

#[cfg(not(test))]
fn test_captured(_terminal_id: &str, _caller: &PtyWriteCaller) -> bool {
    false
}

fn current_registry() -> Option<Arc<SessionRegistry>> {
    use tauri::Manager;
    crate::tauri_app_handle::current().and_then(|app| {
        app.try_state::<Arc<SessionRegistry>>()
            .map(|s| s.inner().clone())
    })
}

/// Bound on input-close batches waiting for the close thread. At most one
/// batch per terminal per open, so this fills only if the outbox append is
/// wedged.
const QUEUE_CAPACITY: usize = 256;

struct Pending {
    registry: Arc<SessionRegistry>,
    store: Arc<OpenTouchStore>,
    closes: Vec<PendingClose>,
}

static QUEUE: OnceLock<Option<SyncSender<Pending>>> = OnceLock::new();

fn queue() -> Option<&'static SyncSender<Pending>> {
    QUEUE
        .get_or_init(|| {
            let (tx, rx) = sync_channel::<Pending>(QUEUE_CAPACITY);
            match std::thread::Builder::new()
                .name("operator-touch-close".to_string())
                .spawn(move || {
                    for p in rx {
                        record_and_persist(
                            &p.store,
                            p.registry.coord_sync().outbox(),
                            p.registry.machine_id(),
                            p.closes,
                        );
                    }
                }) {
                Ok(_) => Some(tx),
                Err(e) => {
                    warn!(error = %e, "operator_touch_close: close thread did not spawn");
                    None
                }
            }
        })
        .as_ref()
}

/// Hand a batch to the close thread; if it cannot take it, put the touches
/// back so any close signal closes them.
fn enqueue(pending: Pending) {
    let Some(tx) = queue() else {
        pending.store.put_back(pending.closes);
        return;
    };
    match tx.try_send(pending) {
        Ok(()) => {}
        Err(TrySendError::Full(p)) | Err(TrySendError::Disconnected(p)) => {
            warn_queue_unavailable();
            p.store.put_back(p.closes);
        }
    }
}

/// At most one warning a minute: a wedged close thread would otherwise warn
/// on every keystroke (each re-takes the touches it just put back).
fn warn_queue_unavailable() {
    static LAST_WARN_SECS: AtomicI64 = AtomicI64::new(i64::MIN);
    if once_a_minute(&LAST_WARN_SECS) {
        warn!(
            "operator_touch_close: close queue unavailable — touches kept open for any close signal"
        );
    }
}

// ===========================================================================
// Close signal 2 — the idle episode ended with no input (plan D4)
// ===========================================================================

/// The episode-end decision: take every `idle_at_prompt` open on
/// `terminal_id` as a `self_resolved` / `none` close. An input that answered
/// the episode has already taken its open, so whatever is left ended with no
/// input — except an open put back after a failed close, which is RETRIED
/// with the words that close decided (an input's `answered` stays
/// `answered`). Pure over the store.
pub fn idle_episode_end_closes(store: &OpenTouchStore, terminal_id: &str) -> Vec<PendingClose> {
    store
        .take_for_terminal_kind(terminal_id, KIND_IDLE_AT_PROMPT)
        .into_iter()
        .map(|touch| {
            PendingClose::worded(touch, Resolution::SelfResolved, Some(CloseActorClass::None))
        })
        .collect()
}

/// Close `terminal_id`'s remaining `idle_at_prompt` opens as `self_resolved`
/// — called by the idle watcher when the episode it fired for ends, and
/// before it opens a NEW episode (the backstop). An open carrying a failed
/// close's stored words ([`OpenTouch::failed_close`]) is closed with THOSE
/// words instead. Synchronous, on the tick's blocking thread.
pub fn close_idle_episode(registry: &SessionRegistry, terminal_id: &str) {
    if !operator_touch::armed() {
        return;
    }
    let store = registry.coord_sync().open_touches();
    let closes = idle_episode_end_closes(&store, terminal_id);
    record_and_persist(
        &store,
        registry.coord_sync().outbox(),
        registry.machine_id(),
        closes,
    );
}

// ===========================================================================
// Close signal 3 — the terminal is gone (death, or a restart)
// ===========================================================================

/// The orphan decision: take every open whose terminal is not live, as an
/// `abandoned` close — or with a failed close's stored words
/// ([`OpenTouch::failed_close`]) when it carries them, which override. `candidates` MUST be read ([`OpenTouchStore::terminal_ids`])
/// BEFORE the live set is snapshotted — an open remembered after the snapshot
/// was taken is then never mistaken for an orphan of a terminal the snapshot
/// simply predates. Pure over the store.
pub fn orphan_closes(
    store: &OpenTouchStore,
    candidates: &[String],
    is_live: impl Fn(&str) -> bool,
) -> Vec<PendingClose> {
    candidates
        .iter()
        .filter(|tid| !is_live(tid))
        .flat_map(|tid| store.take_for_terminal(tid))
        .map(|touch| PendingClose::worded(touch, Resolution::Abandoned, None))
        .collect()
}

/// Close every open whose terminal is not live, as `abandoned` (stored
/// failed-close words override, see [`orphan_closes`]). Runs on the
/// grid-scan tick (`operator_touch_watch::scan_idle_touches_once`), so it is
/// both the runtime death path (a pane that exited, or was removed) and the
/// startup recovery: terminal ids are minted fresh per process
/// (`TerminalManager::create`), so on the first tick after a restart no
/// remembered open has a live terminal, and each is closed `abandoned`
/// without a wait (see the module docs). Synchronous — the tick already runs
/// on a blocking thread.
pub fn close_orphaned(
    registry: &SessionRegistry,
    candidates: &[String],
    is_live: impl Fn(&str) -> bool,
) {
    if !operator_touch::armed() || candidates.is_empty() {
        return;
    }
    let store = registry.coord_sync().open_touches();
    let closes = orphan_closes(&store, candidates, is_live);
    record_and_persist(
        &store,
        registry.coord_sync().outbox(),
        registry.machine_id(),
        closes,
    );
}

// ===========================================================================
// coord's 200 outcomes that close nothing
// ===========================================================================

/// 200s from the close route that recorded NO close: `touch_not_found` (a
/// foreign or other-device key — never retried, plan D1) and
/// `token_carries_no_tenant` (the credential carried no tenant). Counted,
/// and warned at most once a minute, by the drain arm.
static UNRECORDED_CLOSES: AtomicU64 = AtomicU64::new(0);

/// The 200 `outcome` words that mean coord recorded no close.
pub fn is_unrecorded_close_outcome(outcome: &str) -> bool {
    matches!(outcome, "touch_not_found" | "token_carries_no_tenant")
}

/// Count one unrecorded close. Returns the running total and whether this one
/// should be logged.
pub fn note_unrecorded_close() -> (u64, bool) {
    static LAST_WARN_SECS: AtomicI64 = AtomicI64::new(i64::MIN);
    let total = UNRECORDED_CLOSES.fetch_add(1, Ordering::Relaxed) + 1;
    (total, once_a_minute(&LAST_WARN_SECS))
}

/// `true` at most once a minute per `slot` (a per-call-site
/// `static AtomicI64` initialised to `i64::MIN`) — the rate limit for every
/// warning here that a wedged disk or close thread would otherwise repeat on
/// each retry.
fn once_a_minute(slot: &AtomicI64) -> bool {
    let now = Utc::now().timestamp();
    let last = slot.load(Ordering::Relaxed);
    now.saturating_sub(last) >= 60
        && slot
            .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
}

// ===========================================================================
// Test hook — lets the `TerminalSession::write` / `submit_prompt` funnel be
// exercised end to end without a Tauri app
// ===========================================================================

#[cfg(test)]
pub(crate) mod test_hook {
    use super::*;
    use std::cell::RefCell;

    struct Hook {
        store: Arc<OpenTouchStore>,
        captured: Vec<PendingClose>,
    }

    thread_local! {
        static HOOK: RefCell<Option<Hook>> = const { RefCell::new(None) };
    }

    /// Route this thread's input-close signals to `store`, capturing the
    /// closes instead of enqueuing them. Returns what `f` closed.
    pub(crate) fn with_store<R>(
        store: Arc<OpenTouchStore>,
        f: impl FnOnce() -> R,
    ) -> (R, Vec<PendingClose>) {
        HOOK.with(|h| {
            *h.borrow_mut() = Some(Hook {
                store,
                captured: Vec::new(),
            })
        });
        let r = f();
        let captured = HOOK
            .with(|h| h.borrow_mut().take())
            .map(|h| h.captured)
            .unwrap_or_default();
        (r, captured)
    }

    /// `true` iff a hook is installed on this thread (and it handled the
    /// signal).
    pub(super) fn capture(terminal_id: &str, caller: &PtyWriteCaller) -> bool {
        HOOK.with(|h| {
            let mut slot = h.borrow_mut();
            let Some(hook) = slot.as_mut() else {
                return false;
            };
            let closes = closes_for_input(&hook.store, terminal_id, caller);
            hook.captured.extend(closes);
            true
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One coord session per terminal, as in production — so two terminals'
    /// opens never share an idempotency key.
    fn sid_for(terminal_id: &str) -> Uuid {
        let h = terminal_id.bytes().fold(0x5555_u128, |acc, b| {
            acc.wrapping_mul(131).wrapping_add(b as u128)
        });
        Uuid::from_u128(h)
    }

    fn open(terminal_id: &str, kind: &str, bucket: i64, at: DateTime<Utc>) -> OpenTouch {
        let sid = sid_for(terminal_id);
        let payload = operator_touch::touch_payload(kind, sid, Some("harness"), bucket);
        OpenTouch {
            terminal_id: terminal_id.to_string(),
            kind: kind.to_string(),
            coord_session_id: sid,
            idempotency_key: operator_touch::idempotency_key(sid, kind, bucket),
            open_payload: payload,
            open_recorded_at: at,
            open_confirmed: true,
            failed_close: None,
            tenant_id: None,
            reloaded: false,
        }
    }

    fn t0() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-05T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn outbox(dir: &Path) -> OutboxWriter {
        OutboxWriter::open(dir.join("outbox.jsonl")).unwrap()
    }

    // ---- D4: the words, row by row ---------------------------------------

    #[test]
    fn a_human_door_answers_as_human() {
        let store = OpenTouchStore::in_memory();
        store
            .remember(open("t1", KIND_IDLE_AT_PROMPT, 60, t0()))
            .unwrap();
        let closes = closes_for_input(&store, "t1", &PtyWriteCaller::TauriTerminalWrite);
        assert_eq!(closes.len(), 1);
        assert_eq!(closes[0].resolution, Resolution::Answered);
        assert_eq!(closes[0].actor_class, Some(CloseActorClass::Human));
    }

    #[test]
    fn an_unknown_door_answers_as_unknown_never_rounded_to_human() {
        for caller in [
            PtyWriteCaller::HttpWrite,
            PtyWriteCaller::WebSocketInput,
            PtyWriteCaller::TauriInvokeProxy,
            PtyWriteCaller::HttpSubmitPrompt,
        ] {
            let store = OpenTouchStore::in_memory();
            store
                .remember(open("t1", KIND_PERMISSION_PROMPT, 60, t0()))
                .unwrap();
            let closes = closes_for_input(&store, "t1", &caller);
            assert_eq!(closes.len(), 1, "{caller}");
            assert_eq!(closes[0].resolution, Resolution::Answered, "{caller}");
            assert_eq!(
                closes[0].actor_class,
                Some(CloseActorClass::Unknown),
                "{caller}"
            );
        }
    }

    #[test]
    fn an_automated_door_self_resolves_with_class_none() {
        for caller in [
            PtyWriteCaller::AutoResponse {
                rule_id: "r".to_string(),
            },
            PtyWriteCaller::WorkerSession,
            PtyWriteCaller::LoopingAgentNudge,
            PtyWriteCaller::SessionMessagePoller,
        ] {
            let store = OpenTouchStore::in_memory();
            store
                .remember(open("t1", KIND_PERMISSION_PROMPT, 60, t0()))
                .unwrap();
            let closes = closes_for_input(&store, "t1", &caller);
            assert_eq!(closes.len(), 1, "{caller}");
            assert_eq!(closes[0].resolution, Resolution::SelfResolved, "{caller}");
            assert_eq!(
                closes[0].actor_class,
                Some(CloseActorClass::None),
                "{caller}"
            );
        }
    }

    #[test]
    fn a_dead_terminal_abandons_every_open_it_held_with_no_class() {
        let store = OpenTouchStore::in_memory();
        store
            .remember(open("dead", KIND_IDLE_AT_PROMPT, 60, t0()))
            .unwrap();
        store
            .remember(open("dead", KIND_PERMISSION_PROMPT, 120, t0()))
            .unwrap();
        store
            .remember(open("live", KIND_IDLE_AT_PROMPT, 60, t0()))
            .unwrap();
        let candidates = store.terminal_ids();
        let closes = orphan_closes(&store, &candidates, |t| t == "live");
        assert_eq!(closes.len(), 2);
        assert!(closes
            .iter()
            .all(|c| c.resolution == Resolution::Abandoned && c.actor_class.is_none()));
        assert!(closes.iter().all(|c| c.touch.terminal_id == "dead"));
        assert_eq!(store.terminal_ids(), vec!["live".to_string()]);
    }

    #[test]
    fn session_exit_is_never_remembered_so_the_runner_never_closes_it() {
        let store = OpenTouchStore::in_memory();
        store
            .remember(open("t1", operator_touch::KIND_SESSION_EXIT, 60, t0()))
            .unwrap();
        assert!(store.snapshot().is_empty());
        assert!(closes_for_input(&store, "t1", &PtyWriteCaller::TauriTerminalWrite).is_empty());
        assert!(orphan_closes(&store, &["t1".to_string()], |_| false).is_empty());
    }

    #[test]
    fn the_word_table_is_exactly_d4() {
        assert_eq!(
            input_close_words(Some(ActorClass::Human)),
            (Resolution::Answered, CloseActorClass::Human)
        );
        assert_eq!(
            input_close_words(Some(ActorClass::Unknown)),
            (Resolution::Answered, CloseActorClass::Unknown)
        );
        assert_eq!(
            input_close_words(None),
            (Resolution::SelfResolved, CloseActorClass::None)
        );
        assert_eq!(Resolution::Abandoned.as_str(), "abandoned");
        assert_eq!(Resolution::SelfResolved.as_str(), "self_resolved");
        assert_eq!(CloseActorClass::None.as_str(), "none");
    }

    #[test]
    fn the_runners_own_exit_is_not_an_answer_and_leaves_the_touch_for_the_death_sweep() {
        let store = OpenTouchStore::in_memory();
        store
            .remember(open("t1", KIND_IDLE_AT_PROMPT, 60, t0()))
            .unwrap();
        assert!(closes_for_input(&store, "t1", &PtyWriteCaller::GracefulExit).is_empty());
        let closes = orphan_closes(&store, &store.terminal_ids(), |_| false);
        assert_eq!(closes.len(), 1);
        assert_eq!(closes[0].resolution, Resolution::Abandoned);
    }

    // ---- no close without an open key -----------------------------------

    #[test]
    fn input_on_a_terminal_with_no_open_key_closes_nothing() {
        let store = OpenTouchStore::in_memory();
        assert!(closes_for_input(&store, "t1", &PtyWriteCaller::TauriTerminalWrite).is_empty());
        store
            .remember(open("other", KIND_IDLE_AT_PROMPT, 60, t0()))
            .unwrap();
        assert!(closes_for_input(&store, "t1", &PtyWriteCaller::TauriTerminalWrite).is_empty());
        assert_eq!(
            store.snapshot().len(),
            1,
            "another terminal's open is untouched"
        );
    }

    #[test]
    fn a_touch_closes_once_the_second_input_finds_nothing() {
        let store = OpenTouchStore::in_memory();
        store
            .remember(open("t1", KIND_IDLE_AT_PROMPT, 60, t0()))
            .unwrap();
        assert_eq!(
            closes_for_input(&store, "t1", &PtyWriteCaller::TauriTerminalWrite).len(),
            1
        );
        assert!(closes_for_input(&store, "t1", &PtyWriteCaller::TauriTerminalWrite).is_empty());
    }

    #[test]
    fn the_open_set_is_a_set_keyed_by_the_emitted_key_and_keeps_the_first_open() {
        let store = OpenTouchStore::in_memory();
        let first = open("t1", KIND_IDLE_AT_PROMPT, 60, t0());
        let mut repeat = first.clone();
        repeat.open_recorded_at = t0() + chrono::Duration::seconds(30);
        store.remember(first.clone()).unwrap();
        store.remember(repeat).unwrap();
        store
            .remember(open("t1", KIND_IDLE_AT_PROMPT, 120, t0()))
            .unwrap();
        store
            .remember(open("t1", KIND_PERMISSION_PROMPT, 60, t0()))
            .unwrap();
        let snap = store.snapshot();
        assert_eq!(snap.len(), 3, "two idle buckets + one permission prompt");
        assert!(snap.contains(&first), "the first open of a key is kept");
        assert_eq!(
            closes_for_input(&store, "t1", &PtyWriteCaller::TauriTerminalWrite).len(),
            3,
            "one input closes every open on its terminal"
        );
    }

    // ---- the record and the wire body ------------------------------------

    #[test]
    fn the_close_carries_its_full_open_and_the_measured_wait() {
        let touch = open("t1", KIND_PERMISSION_PROMPT, 60, t0());
        let payload = close_record_payload(&PendingClose {
            touch: touch.clone(),
            resolution: Resolution::Answered,
            actor_class: Some(CloseActorClass::Human),
        });
        let close_at = t0() + chrono::Duration::milliseconds(83_250);
        let body = close_body(&payload, close_at).unwrap();
        assert_eq!(body["touch"], touch.open_payload, "the open rides verbatim");
        assert_eq!(body["resolution"], "answered");
        assert_eq!(body["close_actor_class"], "human");
        assert_eq!(body["observed_wait_ms"], 83_250);
        assert!(body.get("open_recorded_at").is_none(), "not a wire field");
        assert!(body.get("tenant_id").is_none());
        let keys: Vec<&String> = body.as_object().unwrap().keys().collect();
        assert_eq!(keys.len(), 4, "{keys:?}");
    }

    #[test]
    fn an_abandoned_close_carries_no_actor_class() {
        let payload = close_record_payload(&PendingClose {
            touch: open("t1", KIND_IDLE_AT_PROMPT, 60, t0()),
            resolution: Resolution::Abandoned,
            actor_class: None,
        });
        let body = close_body(&payload, t0() + chrono::Duration::seconds(5)).unwrap();
        assert_eq!(body["resolution"], "abandoned");
        assert!(body.get("close_actor_class").is_none());
        assert_eq!(body["observed_wait_ms"], 5_000);
    }

    #[test]
    fn a_reloaded_open_or_a_backward_clock_sends_no_wait_rather_than_a_fabricated_one() {
        let mut touch = open("t1", KIND_IDLE_AT_PROMPT, 60, t0());
        touch.reloaded = true;
        let payload = close_record_payload(&PendingClose {
            touch: touch.clone(),
            resolution: Resolution::Abandoned,
            actor_class: None,
        });
        assert!(payload.get("open_recorded_at").is_none());
        let body = close_body(&payload, t0() + chrono::Duration::hours(9)).unwrap();
        assert!(body.get("observed_wait_ms").is_none());

        touch.reloaded = false;
        let payload = close_record_payload(&PendingClose {
            touch,
            resolution: Resolution::Answered,
            actor_class: Some(CloseActorClass::Human),
        });
        let body = close_body(&payload, t0() - chrono::Duration::seconds(1)).unwrap();
        assert!(body.get("observed_wait_ms").is_none());
    }

    #[test]
    fn the_owning_tenant_rides_the_record_for_the_credential_but_never_the_body() {
        let tenant = Uuid::new_v4();
        let mut touch = open("t1", KIND_IDLE_AT_PROMPT, 60, t0());
        touch.tenant_id = Some(tenant);
        let payload = close_record_payload(&PendingClose {
            touch,
            resolution: Resolution::Abandoned,
            actor_class: None,
        });
        assert_eq!(payload["tenant_id"], tenant.to_string());
        let body = close_body(&payload, t0()).unwrap();
        assert!(body.get("tenant_id").is_none());
    }

    #[test]
    fn a_payload_without_its_open_has_no_body() {
        assert!(close_body(&json!({"resolution": "answered"}), t0()).is_none());
        assert!(close_body(&json!({"touch": "x", "resolution": "answered"}), t0()).is_none());
        assert!(close_body(&json!({"touch": {"kind": "idle_at_prompt"}}), t0()).is_none());
    }

    #[test]
    fn closes_ride_the_outbox_on_the_opens_session_lane() {
        let dir = tempfile::tempdir().unwrap();
        let ob = outbox(dir.path());
        let store = OpenTouchStore::open_beside(ob.path());
        store
            .remember(open("t1", KIND_IDLE_AT_PROMPT, 60, t0()))
            .unwrap();
        let closes = closes_for_input(&store, "t1", &PtyWriteCaller::TauriTerminalWrite);
        let machine = Uuid::new_v4();
        record_and_persist(&store, &ob, machine, closes);
        let pending = ob.pending().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].event_kind, "operator_touch_close");
        assert_eq!(pending[0].session_id, sid_for("t1"));
        assert_eq!(pending[0].machine_id, machine);
        assert_eq!(pending[0].payload["resolution"], "answered");
        // Persisted without it.
        assert!(OpenTouchStore::open_beside(ob.path()).snapshot().is_empty());
    }

    // ---- restart recovery ------------------------------------------------

    #[test]
    fn opens_survive_a_restart_and_are_closed_abandoned_without_a_wait() {
        let dir = tempfile::tempdir().unwrap();
        let ob = outbox(dir.path());
        {
            let before = OpenTouchStore::open_beside(ob.path());
            before
                .remember(open("old-1", KIND_IDLE_AT_PROMPT, 60, t0()))
                .unwrap();
            before
                .remember(open("old-1", KIND_PERMISSION_PROMPT, 60, t0()))
                .unwrap();
            before
                .remember(open("old-2", KIND_IDLE_AT_PROMPT, 120, t0()))
                .unwrap();
        } // the process "dies"

        let after = OpenTouchStore::open_beside(ob.path());
        let reloaded = after.snapshot();
        assert_eq!(reloaded.len(), 3, "nothing orphaned by the restart");
        assert!(reloaded.iter().all(|t| t.reloaded));

        // First tick of the new process: no remembered terminal is live.
        let candidates = after.terminal_ids();
        let closes = orphan_closes(&after, &candidates, |_| false);
        assert_eq!(closes.len(), 3);
        record_and_persist(&after, &ob, Uuid::new_v4(), closes);

        let rows = ob.pending().unwrap();
        assert_eq!(rows.len(), 3);
        for row in &rows {
            assert_eq!(row.event_kind, "operator_touch_close");
            let body = close_body(&row.payload, row.recorded_at).unwrap();
            assert_eq!(body["resolution"], "abandoned");
            assert!(body.get("observed_wait_ms").is_none());
            assert!(body["touch"]["idempotency_key"].is_string());
        }
        assert!(
            OpenTouchStore::open_beside(ob.path()).snapshot().is_empty(),
            "closed opens are gone from disk"
        );
    }

    #[test]
    fn a_taken_touch_stays_on_disk_until_its_close_is_appended() {
        let dir = tempfile::tempdir().unwrap();
        let ob_path = dir.path().join("outbox.jsonl");
        let store = OpenTouchStore::open_beside(&ob_path);
        store
            .remember(open("t1", KIND_IDLE_AT_PROMPT, 60, t0()))
            .unwrap();
        let taken = closes_for_input(&store, "t1", &PtyWriteCaller::TauriTerminalWrite);
        assert_eq!(taken.len(), 1);
        // Another terminal's open is persisted while t1's close is in flight…
        store
            .remember(open("t2", KIND_IDLE_AT_PROMPT, 60, t0()))
            .unwrap();
        // …and the process dies before the close is appended.
        let after = OpenTouchStore::open_beside(&ob_path);
        let mut terminals = after.terminal_ids();
        terminals.sort();
        assert_eq!(terminals, vec!["t1".to_string(), "t2".to_string()]);
    }

    #[test]
    fn tracking_before_the_append_survives_a_take_during_it() {
        let store = OpenTouchStore::in_memory();
        let touch = open("t1", KIND_PERMISSION_PROMPT, 60, t0());
        let key = touch.idempotency_key.clone();
        assert!(store.remember_provisional(touch.clone()));
        assert!(
            !store.remember_provisional(touch),
            "a held key keeps its first open"
        );
        // An input lands while the open's append is still in its fsync.
        let taken = closes_for_input(&store, "t1", &PtyWriteCaller::TauriTerminalWrite);
        assert_eq!(taken.len(), 1);
        // A failed append forgets only a touch nothing has taken: the close
        // already carries the open (D1).
        store.forget(&key).unwrap();
        store.put_back(taken);
        assert_eq!(store.snapshot().len(), 1);
        // A landed append stamps the exact recorded_at.
        let exact = t0() + chrono::Duration::milliseconds(7);
        store.confirm(&key, exact).unwrap();
        assert_eq!(store.snapshot()[0].open_recorded_at, exact);
        // An untracked kind is never begun.
        assert!(!store.remember_provisional(open(
            "t1",
            operator_touch::KIND_SESSION_EXIT,
            60,
            t0()
        )));
    }

    #[test]
    fn a_close_measures_from_the_opens_confirmed_stamp_even_if_taken_mid_append() {
        let dir = tempfile::tempdir().unwrap();
        let ob = outbox(dir.path());
        let store = OpenTouchStore::open_beside(ob.path());
        let mut touch = open("t1", KIND_PERMISSION_PROMPT, 60, t0());
        touch.open_confirmed = false; // provisional, as begin_tracking leaves it
        let key = touch.idempotency_key.clone();
        assert!(store.remember_provisional(touch));
        // The input lands while the open's append is still in its fsync…
        let closes = closes_for_input(&store, "t1", &PtyWriteCaller::TauriTerminalWrite);
        // …then the append lands with its real stamp.
        let exact = t0() + chrono::Duration::milliseconds(1_234);
        store.confirm(&key, exact).unwrap();
        record_and_persist(&store, &ob, Uuid::new_v4(), closes);
        let row = &ob.pending().unwrap()[0];
        let opened =
            DateTime::parse_from_rfc3339(row.payload["open_recorded_at"].as_str().unwrap())
                .unwrap()
                .with_timezone(&Utc);
        assert_eq!(opened, exact);
    }

    #[test]
    fn a_never_confirmed_open_closes_without_a_wait() {
        let mut touch = open("t1", KIND_IDLE_AT_PROMPT, 60, t0());
        touch.open_confirmed = false;
        let payload = close_record_payload(&PendingClose {
            touch,
            resolution: Resolution::Answered,
            actor_class: Some(CloseActorClass::Human),
        });
        assert!(payload.get("open_recorded_at").is_none());
        let body = close_body(&payload, t0() + chrono::Duration::seconds(3)).unwrap();
        assert!(body.get("observed_wait_ms").is_none());
    }

    #[test]
    fn a_failed_open_append_is_forgotten_on_disk_too() {
        let dir = tempfile::tempdir().unwrap();
        let ob_path = dir.path().join("outbox.jsonl");
        let store = OpenTouchStore::open_beside(&ob_path);
        let touch = open("t1", KIND_IDLE_AT_PROMPT, 60, t0());
        let key = touch.idempotency_key.clone();
        store.remember(touch).unwrap();
        assert_eq!(OpenTouchStore::open_beside(&ob_path).snapshot().len(), 1);
        store.forget(&key).unwrap();
        assert!(OpenTouchStore::open_beside(&ob_path).snapshot().is_empty());
    }

    #[test]
    fn reload_drops_unclosable_kinds_and_unknown_versions() {
        let dir = tempfile::tempdir().unwrap();
        let ob_path = dir.path().join("outbox.jsonl");
        let mut exit = open("t1", operator_touch::KIND_SESSION_EXIT, 60, t0());
        exit.idempotency_key = "x:session_exit:60".to_string();
        let file = json!({
            "version": STORE_FILE_VERSION,
            "open": [open("t1", KIND_IDLE_AT_PROMPT, 60, t0()), exit],
        });
        std::fs::write(store_path_beside(&ob_path), file.to_string()).unwrap();
        let kept = OpenTouchStore::open_beside(&ob_path).snapshot();
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].kind, KIND_IDLE_AT_PROMPT);

        let future = json!({
            "version": STORE_FILE_VERSION + 1,
            "open": [open("t1", KIND_IDLE_AT_PROMPT, 60, t0())],
        });
        std::fs::write(store_path_beside(&ob_path), future.to_string()).unwrap();
        assert!(OpenTouchStore::open_beside(&ob_path).snapshot().is_empty());
    }

    #[test]
    fn close_body_refuses_an_open_of_a_kind_the_runner_never_closes() {
        let payload = close_record_payload(&PendingClose {
            touch: open("t1", operator_touch::KIND_SESSION_EXIT, 60, t0()),
            resolution: Resolution::Abandoned,
            actor_class: None,
        });
        assert!(close_body(&payload, t0()).is_none());
    }

    #[test]
    fn a_store_publishes_exactly_its_closable_opens_to_the_keystroke_short_circuit() {
        let store = OpenTouchStore::in_memory();
        store
            .remember(open("t1", KIND_IDLE_AT_PROMPT, 60, t0()))
            .unwrap();
        store
            .remember(open("t2", KIND_IDLE_AT_PROMPT, 60, t0()))
            .unwrap();
        assert_eq!(store.lock().published, 2);
        let taken = closes_for_input(&store, "t1", &PtyWriteCaller::TauriTerminalWrite);
        assert_eq!(store.lock().published, 1, "in flight is not closable");
        store.put_back(taken);
        assert_eq!(store.lock().published, 2);
        assert!(OPEN_COUNT.load(Ordering::Relaxed) >= 2);
    }

    #[test]
    fn idle_episode_end_takes_only_idle_opens_as_self_resolved() {
        let store = OpenTouchStore::in_memory();
        store
            .remember(open("t1", KIND_IDLE_AT_PROMPT, 60, t0()))
            .unwrap();
        store
            .remember(open("t1", KIND_IDLE_AT_PROMPT, 120, t0()))
            .unwrap();
        store
            .remember(open("t1", KIND_PERMISSION_PROMPT, 60, t0()))
            .unwrap();
        let closes = idle_episode_end_closes(&store, "t1");
        assert_eq!(closes.len(), 2);
        assert!(closes.iter().all(|c| c.touch.kind == KIND_IDLE_AT_PROMPT
            && c.resolution == Resolution::SelfResolved
            && c.actor_class == Some(CloseActorClass::None)));
        assert_eq!(store.snapshot().len(), 1);
    }

    /// SOURCE GUARD: the keystroke entry point needs a Tauri app, so a test
    /// cannot drive it — pin its shape instead: the short-circuit comes
    /// first, and it hands off to `close_on_input` (whose enqueue
    /// `an_input_close_is_enqueued_and_recorded_by_the_close_thread` covers).
    #[test]
    fn the_keystroke_entry_short_circuits_then_hands_off_to_close_on_input() {
        let src = crate::terminal::operator_touch_watch::code_only(include_str!(
            "operator_touch_close.rs"
        ));
        let entry = src
            .split_once("pub fn on_terminal_input(")
            .expect("entry exists")
            .1
            .split_once("\n}\n")
            .expect("entry body")
            .0;
        let count = entry.find("OPEN_COUNT.load").expect("short-circuit");
        let armed = entry.find("operator_touch::armed()").expect("kill switch");
        let registry = entry.find("current_registry()").expect("registry lookup");
        assert!(count < armed && armed < registry);
        assert!(entry.contains("close_on_input(&registry, terminal_id, caller)"));
    }

    /// A failed INPUT close keeps its words: the open is put back carrying
    /// `answered`, no longer counts as a live wait, and whichever path retries
    /// it — episode end, backstop, input, death — closes it `answered`.
    #[test]
    fn a_failed_input_close_is_retried_as_answered_by_any_path() {
        let store = OpenTouchStore::in_memory();
        store
            .remember(open("t1", KIND_IDLE_AT_PROMPT, 60, t0()))
            .unwrap();
        assert!(store.holds_live_idle_open("t1"));
        let taken = closes_for_input(&store, "t1", &PtyWriteCaller::TauriTerminalWrite);
        store.put_back(taken);
        assert!(
            !store.holds_live_idle_open("t1"),
            "a put-back open's wait is over"
        );
        let retried = idle_episode_end_closes(&store, "t1");
        assert_eq!(retried.len(), 1);
        assert_eq!(retried[0].resolution, Resolution::Answered);
        assert_eq!(retried[0].actor_class, Some(CloseActorClass::Human));
        store.put_back(retried);
        let by_death = orphan_closes(&store, &store.terminal_ids(), |_| false);
        assert_eq!(by_death[0].resolution, Resolution::Answered);
    }

    /// A failed EPISODE-END close is retried `self_resolved` — by the next
    /// episode end / backstop, and even by a later keystroke, never `answered`.
    #[test]
    fn a_failed_episode_end_close_is_retried_self_resolved_not_answered() {
        let dir = tempfile::tempdir().unwrap();
        let ob_path = dir.path().join("outbox.jsonl");
        let ob = OutboxWriter::open(&ob_path).unwrap();
        let store = OpenTouchStore::open_beside(&ob_path);
        store
            .remember(open("t1", KIND_IDLE_AT_PROMPT, 60, t0()))
            .unwrap();
        // The episode ends, but the append fails.
        std::fs::remove_file(&ob_path).ok();
        std::fs::create_dir(&ob_path).unwrap();
        record_and_persist(
            &store,
            &ob,
            Uuid::new_v4(),
            idle_episode_end_closes(&store, "t1"),
        );
        let held = store.snapshot();
        assert_eq!(held.len(), 1, "kept for a retry");
        assert_eq!(
            held[0].failed_close,
            Some(FailedClose {
                resolution: Resolution::SelfResolved,
                actor_class: Some(CloseActorClass::None),
            })
        );
        assert!(!store.holds_live_idle_open("t1"), "a new episode may fire");
        // A keystroke now does not reword it.
        let by_input = closes_for_input(&store, "t1", &PtyWriteCaller::TauriTerminalWrite);
        assert_eq!(by_input[0].resolution, Resolution::SelfResolved);
        assert_eq!(by_input[0].actor_class, Some(CloseActorClass::None));
        store.put_back(by_input);
        // The disk heals; the backstop before the next episode retries it.
        std::fs::remove_dir(&ob_path).unwrap();
        record_and_persist(
            &store,
            &ob,
            Uuid::new_v4(),
            idle_episode_end_closes(&store, "t1"),
        );
        let rows = ob.pending().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].payload["resolution"], "self_resolved");
        assert_eq!(rows[0].payload["close_actor_class"], "none");
        assert!(store.snapshot().is_empty());
    }

    /// A retried close omits `observed_wait_ms` (plan D3): measured at the
    /// retry, the wait would include the retry delay.
    #[test]
    fn a_retried_close_carries_no_wait() {
        let store = OpenTouchStore::in_memory();
        store
            .remember(open("t1", KIND_PERMISSION_PROMPT, 60, t0()))
            .unwrap();
        let first = closes_for_input(&store, "t1", &PtyWriteCaller::TauriTerminalWrite);
        let measured = close_body(
            &close_record_payload(&first[0]),
            t0() + chrono::Duration::seconds(4),
        )
        .unwrap();
        assert_eq!(
            measured["observed_wait_ms"], 4_000,
            "the first attempt is measured"
        );
        store.put_back(first);
        let retry = closes_for_input(&store, "t1", &PtyWriteCaller::TauriTerminalWrite);
        assert_eq!(retry.len(), 1);
        let payload = close_record_payload(&retry[0]);
        assert!(payload.get("open_recorded_at").is_none());
        let body = close_body(&payload, t0() + chrono::Duration::hours(1)).unwrap();
        assert!(body.get("observed_wait_ms").is_none());
        assert_eq!(body["resolution"], "answered", "the words survive");
    }

    /// The failed words survive a restart too.
    #[test]
    fn a_failed_close_reloads_with_its_words() {
        let dir = tempfile::tempdir().unwrap();
        let ob_path = dir.path().join("outbox.jsonl");
        let store = OpenTouchStore::open_beside(&ob_path);
        store
            .remember(open("t1", KIND_IDLE_AT_PROMPT, 60, t0()))
            .unwrap();
        store.put_back(closes_for_input(&store, "t1", &PtyWriteCaller::HttpWrite));
        let after = OpenTouchStore::open_beside(&ob_path);
        let closes = orphan_closes(&after, &after.terminal_ids(), |_| false);
        assert_eq!(closes[0].resolution, Resolution::Answered);
        assert_eq!(closes[0].actor_class, Some(CloseActorClass::Unknown));
    }

    /// Persist-before-append: what `begin_tracking` remembers is on DISK,
    /// unconfirmed, before the open's append runs; a failed append's
    /// `forget` removes it from disk again.
    #[test]
    fn a_provisional_open_is_on_disk_before_its_append_and_gone_after_a_failed_one() {
        let dir = tempfile::tempdir().unwrap();
        let ob_path = dir.path().join("outbox.jsonl");
        let store = OpenTouchStore::open_beside(&ob_path);
        let mut touch = open("t1", KIND_PERMISSION_PROMPT, 60, t0());
        touch.open_confirmed = false;
        let key = touch.idempotency_key.clone();
        assert!(store.remember_provisional(touch));
        store.persist().unwrap();
        let on_disk = OpenTouchStore::open_beside(&ob_path).snapshot();
        assert_eq!(on_disk.len(), 1);
        assert!(!on_disk[0].open_confirmed, "persisted BEFORE confirm");
        store.forget(&key).unwrap();
        assert!(OpenTouchStore::open_beside(&ob_path).snapshot().is_empty());
    }

    #[test]
    fn a_failed_append_keeps_the_touches_open() {
        let dir = tempfile::tempdir().unwrap();
        let ob_path = dir.path().join("outbox.jsonl");
        let ob = OutboxWriter::open(&ob_path).unwrap();
        let store = OpenTouchStore::open_beside(&ob_path);
        store
            .remember(open("t1", KIND_IDLE_AT_PROMPT, 60, t0()))
            .unwrap();
        // Make the outbox path unwritable: replace the file with a directory.
        std::fs::remove_file(&ob_path).ok();
        std::fs::create_dir(&ob_path).unwrap();
        let closes = closes_for_input(&store, "t1", &PtyWriteCaller::TauriTerminalWrite);
        assert_eq!(closes.len(), 1);
        record_and_persist(&store, &ob, Uuid::new_v4(), closes);
        assert_eq!(store.snapshot().len(), 1, "put back for the next signal");
    }

    #[test]
    fn an_unreadable_store_starts_empty() {
        let dir = tempfile::tempdir().unwrap();
        let ob_path = dir.path().join("outbox.jsonl");
        std::fs::write(store_path_beside(&ob_path), b"{not json").unwrap();
        assert!(OpenTouchStore::open_beside(&ob_path).snapshot().is_empty());
    }

    #[test]
    fn the_store_sits_beside_the_outbox() {
        let p = store_path_beside(Path::new("/x/y/session-outbox.jsonl"));
        assert_eq!(
            p,
            Path::new("/x/y/session-outbox.jsonl.operator-touch-open.json")
        );
    }
}
