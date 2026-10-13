//! Cross-machine session handoff — trigger + receiver.
//!
//! Plan: `D:/qontinui-root/qontinui-dev-notes/plans/
//! 2026-05-23-coord-native-sessions-phase-7-10.md` §Phase 7. One-way move
//! ("Continue elsewhere"): an operator moves an active session from one
//! machine to another, with cwd + held claims + recent PTY scrollback
//! following.
//!
//! ## Transport: WebSocket-relay push + on-connect catch-up (NOT polling)
//!
//! The plan text says the handoff request travels via JetStream. The
//! runner has **no NATS client** — but it already maintains a persistent
//! WebSocket relay to coord's `/ws` Redis-pub/sub fan-out (the same
//! channel `agent_runtime.rs` uses to receive `events.agent.spawn_*`).
//! Coord's `post_handoff` handler dual-publishes the
//! `handoff_request` payload on subject
//! `qontinui.sessions.<tenant>.<target-machine>.handoff_request` over BOTH
//! JetStream AND Redis pub/sub (see coord `build_events::dual_publish_body`).
//! The Redis arm is exactly what coord's `/ws` fan-out relays.
//!
//! So the receiver is **push-driven, not poll-driven**:
//!
//! 1. **Real-time push.** The receiver opens a coord `/ws` subscription
//!    under the closed-set name `sessions`
//!    (`qontinui_runner_lib::coord_ws::Subscription::Sessions`), which coord
//!    resolves server-side to `qontinui.sessions.<tenant>.<self-device>.*`
//!    from the upgrade credential's claims, and filters inbound envelopes
//!    for `…<self-device>.handoff_request`. Server-side fan-out
//!    (coord PUBLISHes to the target machine's subject) means no
//!    N-runners-polling — the target machine sees the request the instant
//!    coord records it. This reuses the existing runner↔coord relay
//!    transport rather than adding a second poll loop or a fleet-wide NATS
//!    client.
//! 2. **On-(re)connect catch-up.** Every time the relay WS connects (or
//!    reconnects after a drop), the receiver does ONE
//!    `GET /sessions/handoff-requests?device_id=<self>` and materializes
//!    anything that arrived while it was offline. Coord's durable
//!    `handoff_request` event row is the source of truth for this
//!    catch-up, so a request published while the runner was disconnected
//!    is never lost. This is the robustness backstop — steady-state
//!    delivery is the push.
//! 3. **Periodic catch-up tick.** The HANDOFF catch-up GET also runs every
//!    [`CATCHUP_TICK`] while the socket is up, so the window is bounded even
//!    when the socket is healthy but DELIVERS nothing. That is exactly the
//!    shape against a coord predating the closed subscription set: such a
//!    coord ignores `?subscribe=` and PSUBSCRIBEs its default `events.*`,
//!    which never matches `qontinui.sessions.*` — so with the socket held
//!    open, no `handoff_request` frame ever arrives on it. Against that coord
//!    this lane degrades to POLL cadence (one tick), not to silence. The
//!    other three lanes are harmless against the old coord because their
//!    subjects sit under `events.*`; this one is not, which is why the tick
//!    exists. A repeat sighting of a handoff this process already
//!    materialized never starts a second child: [`MaterializedSources`]
//!    turns it into a close-only retry, and a source whose close coord
//!    REFUSED for good (see [`SourceClose`]) into no request at all.
//!
//!    The close RETRY itself is not driven by sightings. Coord drops a
//!    handoff from its pending list as soon as this device's child row
//!    exists, and that row registers asynchronously — often after the first
//!    close was already refused as `handoff_child_not_materialized` — so the
//!    source may never be sighted again. Every handoff catch-up pass (the
//!    tick included) therefore re-sends the close for each source in
//!    [`MaterializedSources`] that has not settled, under the tenant scope it
//!    recorded, whether or not coord still lists it: exponential backoff from
//!    one tick up to [`CLOSE_RETRY_CEIL`], abandoned (logged once) after
//!    [`CLOSE_RETRY_LIFETIME`] of failures. The backoff is measured from the
//!    moment each close request STARTED. Because these closes run inline on
//!    the socket pump, each one carries its own [`CLOSE_REQUEST_TIMEOUT`], and
//!    one [`retry_due_closes`] pass sends at most [`CLOSE_RETRY_PASS_CAP`] of
//!    them (most overdue first), leaving the rest due for the next pass. That
//!    cap bounds the retry pass only: a handoff coord still LISTS that comes
//!    back as close-only is retried in the catch-up loop itself, before the
//!    capped pass, so a whole catch-up pass also grows with coord's list.
//!
//!    The tick drives only the arms with no OTHER periodic owner — handoff
//!    and respawn. The remote-attach and remote-create arms that share this
//!    socket's on-connect replay each already have their own poll task in
//!    `main.rs` (`attach::POLL_INTERVAL`, `create::POLL_INTERVAL`), each at
//!    least as frequent as this tick, so putting them on this tick as well
//!    would add GETs and deliver nothing sooner.
//!    [`catchups_for`] is where that split lives.
//!
//! ## Receiver flow (one handoff)
//!
//! 1. A push frame (or the on-connect catch-up GET) yields a
//!    [`PendingHandoff`] addressed to this device.
//! 2. Fetch `GET /sessions/:id/handoff-state` → [`HandoffState`].
//! 3. Build an [`Intent`] from the source intent (cwd via `repo` /
//!    `declared_paths`), materialize a child session via
//!    [`SessionRegistry::start_with_parent`] so coord stamps
//!    `parent_session_id`.
//! 4. Re-acquire each held claim under this device (`POST /claims/acquire`
//!    — idempotent by resource_key, so this is the "claim transfer").
//! 5. Replay warm-tier scrollback into the new PTY.
//! 5b. Materialize a local restore-registry record from the newest
//!    `restore-record` session event (plan
//!    `2026-07-09-runner-session-history-cloud-sync` §3.4, Phase 4).
//!    Coord's `HandoffState` bundle does NOT carry session events (verified
//!    against coord `sessions.rs::HandoffState` 2026-07-09), so the
//!    receiver fetches them from the durable-replay portion of
//!    `GET /sessions/:id/events` (bounded read; the endpoint replays the
//!    last rows immediately, then live-tails). Tier honesty carries over:
//!    a `"full"` record materializes AUTHORITATIVE + CONFIRMED (the
//!    existing restore classifier auto-resumes it by authoritative id); a
//!    `"terminal_only"` record materializes AUTHORITATIVE + PROVISIONAL
//!    (the classifier restores terminal+cwd with a fresh conversation —
//!    exactly the existing phantom-shell branch, no new restore path).
//!    Best-effort: any failure here never fails the handoff.
//! 6. Close the source session through coord's device-authed handoff
//!    completion door (`POST /sessions/:id/handoff/complete`, presenting the
//!    SOURCE session's tenant slot) so it transitions to `closed`
//!    (`closed_at = now()`) and coord runs its close side effects exactly
//!    once. Coord authorizes it on the handoff request itself: the caller
//!    must be the request's target device AND already hold the materialized
//!    child (`parent_session_id = :id`) — so a close sent before the child's
//!    row has registered is refused as `handoff_child_not_materialized`. That
//!    refusal, a 5xx, a transport error and any answer outside the contract
//!    are retried from [`MaterializedSources`] on the catch-up passes with
//!    per-source backoff (module doc, point 3), not by waiting for coord to
//!    list the source again. A `not_handoff_target` refusal is settled, but a
//!    FRESH `handoff_request` push frame for the same source addressed to
//!    this device (a re-handoff back here) revives it and the close is sent
//!    again; a `session_not_found` refusal and a successful close stay
//!    settled. (`DELETE /sessions/:id`, which this step
//!    used to send, is coord's operator-admin route and 401s a device; plan
//!    `2026-10-10-remote-create-residuals-followups` Phase 4.) The child's
//!    `started` event carries `parent_session_id`, which is the durable
//!    `handoff_to` link (parent → child by `parent_session_id` index).
//!
//! ## This loop also carries the RESPAWN arm
//!
//! Coord publishes `respawn_request` on the SAME
//! `qontinui.sessions.<tenant>.<device>.<kind>` family (plan
//! `2026-08-26-sessions-console-consolidation` §6 Phase 5), so
//! [`connect_and_pump`] forwards every frame to [`super::respawn`] as well and
//! [`connect_and_pump`]'s on-connect catch-up runs
//! [`super::respawn::run_catchup`] beside this module's own. The two arms are
//! disambiguated ONLY by the channel's trailing segment —
//! [`parse_handoff_push`] requires `.handoff_request`,
//! [`super::respawn::parse_respawn_push`] requires `.respawn_request` — so
//! neither swallows the other's frames and nothing is materialized twice.
//! A respawn deliberately does NOT run step 6 of the receiver flow above:
//! its source is already closed, which is the premise of the feature.
//!
//! Step 3 happening before step 6 is deliberate: the source is only torn
//! down once the child exists, so a failed materialization leaves the
//! source intact and the next push/catch-up retries. Idempotency: coord's
//! `get_handoff_requests` filters out any source that already has a
//! materialized child on this device, so a push + catch-up double-delivery
//! never materializes twice.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::time::Instant;
use uuid::Uuid;

use super::intent::Intent;
use super::provider_adapter::RestoreTier;
use super::restore_record_emitter::{RESTORE_RECORD_EVENT, TIER_FULL, TIER_TERMINAL_ONLY};
use super::session_id::is_valid_session_id;
use super::session_lifecycle_store::{
    SessionLifecycleStore, TerminalSessionRecord, DEFAULT_PROVIDER, ORIGIN_AUTHORITATIVE,
};
use crate::auth::TenantScope;

use super::{SessionKind, SessionRegistry};

/// Reconnect backoff floor for the push-subscriber WS loop. Matches the
/// `agent_runtime.rs` reconnect posture (2s → 60s capped).
const RECONNECT_BACKOFF_FLOOR: Duration = Duration::from_secs(2);
/// Reconnect backoff ceiling.
const RECONNECT_BACKOFF_CEIL: Duration = Duration::from_secs(60);
/// How often the catch-up GETs re-run on a LIVE socket (module doc, point 3).
/// Same interval class as `agent_runtime`'s poll backstop. Against a coord
/// predating `?subscribe=` this is the lane's whole delivery cadence.
const CATCHUP_TICK: Duration = Duration::from_secs(60);
/// Ceiling of a failing source close's retry backoff (module doc, point 3).
/// The backoff doubles from one [`CATCHUP_TICK`] per failure; ten minutes
/// keeps a close stuck behind a coord outage at six requests an hour instead
/// of sixty, while a child row that registered late is still picked up within
/// a tick or two of its first refusal (the early steps are 1 and 2 ticks).
const CLOSE_RETRY_CEIL: Duration = Duration::from_secs(10 * 60);
/// How long a source close keeps being retried after its FIRST failure before
/// this process abandons it (logged once at warn). A day comfortably covers a
/// coord outage, a late child registration, or a credential gap a re-pairing
/// fixes; a close still failing after that is failing for a reason a retry
/// will not change, and coord's staleness reaper is the backstop for the
/// source. At the ceiling that is ~150 requests per stuck source in total,
/// and a fresh handoff request for the source revives it.
const CLOSE_RETRY_LIFETIME: Duration = Duration::from_secs(24 * 60 * 60);
/// Slack when deciding whether a scheduled close retry is due. The retry is
/// scheduled from the moment its attempt STARTED, and the passes that drive it
/// run on [`CATCHUP_TICK`] — a few seconds of jitter between ticks must not
/// push a one-tick backoff out to two.
const CLOSE_RETRY_DUE_SLACK: Duration = Duration::from_secs(5);
/// Per-request timeout of one source close (`POST …/handoff/complete`),
/// tighter than the shared client's. Every close — the first one and each
/// retry — runs inline on the socket pump's task, so against a coord that
/// accepts the connection and never answers, a pass with N due retries would
/// otherwise hold the pump (push frames, pings, the drain arm) for N times the
/// client's 30 s. The close is a tiny idempotent POST; ten seconds is ample
/// for a healthy coord, and a timeout is just another retryable answer.
const CLOSE_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// The most close retries one [`retry_due_closes`] pass sends. With
/// [`CLOSE_REQUEST_TIMEOUT`] this bounds one pass's stall on an unresponsive
/// coord at ~80 s, whatever the backlog. Any further due closes are left
/// pending and due, so the next pass (one [`CATCHUP_TICK`] later) sends them;
/// the most overdue go first, so none is starved.
const CLOSE_RETRY_PASS_CAP: usize = 8;

/// The periodic catch-up ticker (module doc, point 3): fires immediately
/// once (the caller consumes that tick, since the on-connect catch-up just
/// ran), then every [`CATCHUP_TICK`]; a tick missed while a catch-up was
/// still running is delayed, not bunched. Factored out so the schedule is
/// unit-testable on a paused clock without a socket.
fn catchup_interval() -> tokio::time::Interval {
    let mut tick = tokio::time::interval(CATCHUP_TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    tick
}

/// What a sighting of a pending handoff for a given source should do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Sighting {
    /// First sighting this run: start the child, then close the source.
    Materialize,
    /// A child for this source was already started by THIS process: do not
    /// start another; only retry the `close_source` that must have failed,
    /// presenting the SOURCE's tenant scope captured when the child started.
    CloseOnly(TenantScope),
    /// This process already settled the source's close — coord closed it,
    /// refused it ([`SourceClose::Refused`]), or the retries were abandoned
    /// after [`CLOSE_RETRY_LIFETIME`]. Nothing to send on a sighting: only a
    /// fresh handoff request can reopen a `not_handoff_target` refusal or an
    /// abandoned close (`revive_on_fresh_request`).
    Settled,
}

/// Where one materialized source's close stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CloseState {
    /// Not settled: the close is (re-)sent once `next_at` comes due.
    Pending {
        /// Failed attempts so far (0 until the first close is answered).
        failures: u32,
        /// When the next attempt is due.
        next_at: Instant,
        /// When the FIRST failure happened — the start of the lifetime bound.
        first_failure_at: Option<Instant>,
    },
    /// Coord closed the source (200). Settled for good: nothing revives it.
    Closed,
    /// Coord refused the close for good ([`SourceClose::Refused`]).
    /// `revivable` is `true` for `403 not_handoff_target`, which a later
    /// re-handoff of the same source BACK to this device makes stale; `false`
    /// for `404 session_not_found`, which nothing changes.
    Refused { revivable: bool },
    /// Retried for [`CLOSE_RETRY_LIFETIME`] without a terminal answer; left to
    /// coord's staleness reaper. A fresh handoff request revives it.
    Abandoned,
}

impl CloseState {
    fn pending_now(now: Instant) -> Self {
        CloseState::Pending {
            failures: 0,
            next_at: now,
            first_failure_at: None,
        }
    }

    fn settled(self) -> bool {
        !matches!(self, CloseState::Pending { .. })
    }
}

/// What this process knows about one source it started a child for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct MaterializedSource {
    /// The SOURCE session's tenant scope, taken from the child intent exactly
    /// as [`reacquire_claim`] takes it, so a close retry presents the same
    /// credential the first close did.
    tenant: TenantScope,
    /// The close's progress, and the retry schedule while it is pending.
    close: CloseState,
}

/// How [`MaterializedSources::record`] filed one close outcome — the input to
/// [`settle_close`]'s log line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Recorded {
    /// The source was never marked here; nothing was recorded.
    Untracked,
    /// Settled: closed.
    Closed,
    /// Settled: refused. `revivable` mirrors [`CloseState::Refused`]: `true`
    /// for `not_handoff_target` (a fresh handoff request back here revives
    /// it), `false` for `session_not_found` (terminal).
    Refused { revivable: bool },
    /// A retryable answer to a close whose entry had ALREADY settled (a racing
    /// or late attempt). Ignored: a settled entry is never reopened by a
    /// retryable answer — only a fresh handoff request revives one.
    AlreadySettled,
    /// A retryable failure, rescheduled `retry_in` from the attempt.
    /// `first` is `true` for the source's first failure (the one logged at
    /// warn; repeats log at debug).
    Retry {
        failures: u32,
        retry_in: Duration,
        first: bool,
    },
    /// A retryable failure past [`CLOSE_RETRY_LIFETIME`]: abandoned.
    Abandoned { failures: u32 },
}

/// The backoff after `failures` consecutive failed closes (`failures >= 1`):
/// one [`CATCHUP_TICK`], doubling per failure, capped at [`CLOSE_RETRY_CEIL`].
pub(super) fn close_retry_backoff(failures: u32) -> Duration {
    let doublings = failures.saturating_sub(1).min(16);
    CATCHUP_TICK
        .saturating_mul(1u32 << doublings)
        .min(CLOSE_RETRY_CEIL)
}

/// Source sessions this process has already started a child for, and how
/// each one's close stands.
///
/// The only "ack" of a handoff is `close_source`, which runs AFTER the child
/// is started (`materialize`). A close that keeps failing — 403 in a
/// credential gap, coord 5xx — leaves the handoff in coord's pending list, and
/// with the [`CATCHUP_TICK`] every tick would otherwise start ANOTHER child
/// terminal for the same source: an unbounded duplicate-spawn loop, one per
/// minute. This set turns a repeat sighting into a close-only retry, and a
/// sighting after a terminal answer into nothing at all.
///
/// It is also what DRIVES the close retries ([`retry_due_closes`]): a source
/// coord has stopped listing — its child row registered after the first
/// close was refused — is still re-sent from here, on a per-source backoff.
///
/// Per-process on purpose: it is the process that started the child, so it is
/// the process that knows. A restart forgets it, and the next sighting after
/// a restart materializes again — one duplicate per restart, bounded, versus
/// one per tick. Marked at the moment the child is STARTED, not when the
/// close succeeds, because the child is what must not be duplicated.
#[derive(Default)]
pub(super) struct MaterializedSources(Mutex<HashMap<Uuid, MaterializedSource>>);

impl MaterializedSources {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<Uuid, MaterializedSource>> {
        self.0.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// The pure decision for one sighting of `source`.
    pub(super) fn sighting(&self, source: Uuid) -> Sighting {
        sighting_for(&self.lock(), source)
    }

    /// Record that a child for `source` has been started under `tenant` (the
    /// SOURCE session's scope). Returns `true` when this is the first record
    /// (the set changed); a repeat keeps the original record untouched.
    pub(super) fn mark(&self, source: Uuid, tenant: TenantScope) -> bool {
        let mut seen = self.lock();
        if seen.contains_key(&source) {
            return false;
        }
        seen.insert(
            source,
            MaterializedSource {
                tenant,
                close: CloseState::pending_now(Instant::now()),
            },
        );
        true
    }

    /// A FRESH `handoff_request` push frame for `source` addressed to this
    /// device: coord has just recorded a new handoff of it to here. That makes
    /// a `not_handoff_target` refusal (an earlier request had moved it
    /// elsewhere) or an abandoned retry stale, so either is revived and the
    /// close becomes due now; a pending close's backoff is reset for the same
    /// reason. A closed source, and one coord said does not exist, stay
    /// settled. Returns `true` when the state changed.
    pub(super) fn revive_on_fresh_request(&self, source: Uuid, now: Instant) -> bool {
        let mut seen = self.lock();
        let Some(entry) = seen.get_mut(&source) else {
            return false;
        };
        match entry.close {
            CloseState::Closed | CloseState::Refused { revivable: false } => false,
            CloseState::Refused { revivable: true }
            | CloseState::Abandoned
            | CloseState::Pending { .. } => {
                entry.close = CloseState::pending_now(now);
                true
            }
        }
    }

    /// The tenant scope to close `source` under, when its close is pending
    /// and due at `now`; `None` when it is settled, backing off, or unknown.
    pub(super) fn due(&self, source: Uuid, now: Instant) -> Option<TenantScope> {
        self.lock()
            .get(&source)
            .and_then(|entry| close_due(entry, now).then_some(entry.tenant))
    }

    /// Every pending close that is due at `now`, with its tenant scope —
    /// independent of whether coord still lists the source as pending.
    /// Ordered most overdue first, so a pass that sends only a capped prefix
    /// ([`CLOSE_RETRY_PASS_CAP`]) never starves the rest.
    pub(super) fn due_closes(&self, now: Instant) -> Vec<(Uuid, TenantScope)> {
        let mut due: Vec<(Instant, Uuid, TenantScope)> = self
            .lock()
            .iter()
            .filter_map(|(source, entry)| match entry.close {
                CloseState::Pending { next_at, .. } if close_due(entry, now) => {
                    Some((next_at, *source, entry.tenant))
                }
                _ => None,
            })
            .collect();
        due.sort_by_key(|(next_at, source, _)| (*next_at, *source));
        due.into_iter()
            .map(|(_, source, tenant)| (source, tenant))
            .collect()
    }

    /// File one close outcome for `source`, attempted at `attempted_at`. A
    /// source this process never marked is left unrecorded: it has no child
    /// here, and settling it would suppress a materialization this process
    /// has not done.
    pub(super) fn record(
        &self,
        source: Uuid,
        close: &SourceClose,
        attempted_at: Instant,
    ) -> Recorded {
        let mut seen = self.lock();
        let Some(entry) = seen.get_mut(&source) else {
            return Recorded::Untracked;
        };
        match close {
            SourceClose::Closed { .. } => {
                entry.close = CloseState::Closed;
                Recorded::Closed
            }
            SourceClose::Refused { status, error } => {
                let revivable = (*status, error.as_str()) == (403, "not_handoff_target");
                entry.close = CloseState::Refused { revivable };
                Recorded::Refused { revivable }
            }
            SourceClose::Retry(_) => {
                let (failures, first_failure_at) = match entry.close {
                    CloseState::Pending {
                        failures,
                        first_failure_at,
                        ..
                    } => (failures + 1, first_failure_at.unwrap_or(attempted_at)),
                    // A retryable answer arriving after the entry settled (a
                    // racing or late attempt) must not reopen it: a Closed
                    // source is done, a Refused or Abandoned one is reopened
                    // only by a fresh handoff request.
                    CloseState::Closed | CloseState::Refused { .. } | CloseState::Abandoned => {
                        return Recorded::AlreadySettled;
                    }
                };
                if attempted_at.saturating_duration_since(first_failure_at) >= CLOSE_RETRY_LIFETIME
                {
                    entry.close = CloseState::Abandoned;
                    return Recorded::Abandoned { failures };
                }
                let retry_in = close_retry_backoff(failures);
                entry.close = CloseState::Pending {
                    failures,
                    next_at: attempted_at + retry_in,
                    first_failure_at: Some(first_failure_at),
                };
                Recorded::Retry {
                    failures,
                    retry_in,
                    first: failures == 1,
                }
            }
        }
    }
}

/// Whether `entry`'s close is pending and due at `now` (within
/// [`CLOSE_RETRY_DUE_SLACK`]).
fn close_due(entry: &MaterializedSource, now: Instant) -> bool {
    match entry.close {
        CloseState::Pending { next_at, .. } => now + CLOSE_RETRY_DUE_SLACK >= next_at,
        _ => false,
    }
}

/// [`Sighting`] for `source` against the sources already materialized.
pub(super) fn sighting_for(seen: &HashMap<Uuid, MaterializedSource>, source: Uuid) -> Sighting {
    match seen.get(&source) {
        None => Sighting::Materialize,
        Some(entry) if entry.close.settled() => Sighting::Settled,
        Some(entry) => Sighting::CloseOnly(entry.tenant),
    }
}

/// Purpose suffix a HANDOFF stamps on the child intent. Parameterised (rather
/// than inlined) because the respawn receiver reuses
/// [`build_child_intent`] and must say what it actually did — a respawn is not
/// a continuation of a live session.
pub(super) const HANDOFF_CONTINUATION_NOTE: &str = "continued here";

// ---------------------------------------------------------------------------
// Wire types — mirror `qontinui-coord/src/sessions.rs` Phase 7 shapes.
// ---------------------------------------------------------------------------

/// One pending handoff, as returned by
/// `GET /sessions/handoff-requests`. Mirrors coord's `PendingHandoff`.
#[derive(Debug, Clone, Deserialize)]
pub struct PendingHandoff {
    pub source_session_id: Uuid,
    pub target_device_id: Uuid,
    pub tenant_id: Uuid,
    pub session_kind: String,
}

/// Envelope coord returns from `GET /sessions/handoff-requests`.
#[derive(Debug, Clone, Deserialize)]
struct HandoffListResponse {
    #[serde(default)]
    handoffs: Vec<PendingHandoff>,
}

/// State-transfer bundle from `GET /sessions/:id/handoff-state`. Mirrors
/// coord's `HandoffState`.
#[derive(Debug, Clone, Deserialize)]
pub struct HandoffState {
    pub source_session_id: Uuid,
    #[allow(dead_code)]
    pub tenant_id: Uuid,
    #[allow(dead_code)]
    pub source_device_id: Uuid,
    pub session_kind: String,
    pub intent: serde_json::Value,
    pub repo: Option<String>,
    pub branch: Option<String>,
    #[serde(default)]
    pub held_claims: Vec<HeldClaim>,
    #[serde(default)]
    pub output_chunks: Vec<OutputChunk>,
}

/// One held claim to re-acquire under the new device.
#[derive(Debug, Clone, Deserialize)]
pub struct HeldClaim {
    pub kind: String,
    pub resource_key: String,
}

/// One warm-tier output chunk (base64) for scrollback replay.
#[derive(Debug, Clone, Deserialize)]
pub struct OutputChunk {
    #[allow(dead_code)]
    pub chunk_offset: i64,
    pub payload_b64: String,
}

/// Errors raised by the handoff trigger + receiver.
#[derive(Debug, thiserror::Error)]
pub enum HandoffError {
    #[error("coord HTTP error: {0}")]
    Http(String),
    #[error("coord returned status {0}: {1}")]
    Status(u16, String),
    #[error("response parse failed: {0}")]
    Parse(String),
    #[error("session error: {0}")]
    Session(String),
}

// ---------------------------------------------------------------------------
// Trigger
// ---------------------------------------------------------------------------

/// Body of `POST /sessions/:id/handoff`.
#[derive(Debug, Serialize)]
struct TriggerBody {
    target_device_id: Uuid,
}

/// Publish a handoff request for `source_session_id` to
/// `target_device_id`. POSTs `/sessions/:id/handoff`; coord records the
/// durable event + publishes the JetStream subject. Plan §Phase 7.
///
/// This is the runner-side trigger surface (also reachable from the
/// dashboard's "Continue elsewhere" button via the web backend proxy —
/// the dashboard hits coord directly through the proxy, this exists for
/// a runner-initiated handoff and is exercised by the unit test).
///
/// `tenant` is the SOURCE session's scope. `/sessions/{id}/…` is a
/// per-session route, so it presents that session's device-JWT slot rather than
/// the device default — the same rule the drain loop and the output pipe
/// follow. A caller that cannot resolve the session passes
/// [`TenantScope::Unresolved`], which keeps the default binding on a
/// single-bound device and degrades to unauthenticated on a multi-bound one.
pub async fn trigger_handoff(
    http: &reqwest::Client,
    coord_url: &str,
    source_session_id: Uuid,
    target_device_id: Uuid,
    tenant: TenantScope,
) -> Result<(), HandoffError> {
    let url = format!(
        "{}/sessions/{}/handoff",
        coord_url.trim_end_matches('/'),
        source_session_id
    );
    let resp = crate::auth::attach_device_auth_for(
        http.post(&url).json(&TriggerBody { target_device_id }),
        tenant,
    )
    .send()
    .await
    .map_err(|e| HandoffError::Http(format!("POST {url}: {e}")))?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(HandoffError::Status(
            status.as_u16(),
            body.chars().take(500).collect(),
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Receiver
// ---------------------------------------------------------------------------

/// Start the handoff-receiver task. Returns the [`JoinHandle`] so
/// `main.rs` can keep it alive for the lifetime of the process.
///
/// The task is **push-driven**: it opens a coord `/ws` subscription under
/// the `sessions` name (this tenant's subjects for this device) and
/// materializes each `handoff_request` addressed to this device the
/// instant coord fans it out. On every
/// (re)connect it also runs a single catch-up GET so anything published
/// while the runner was offline is replayed. Plan §Phase 7.
///
/// `lifecycle_store` is the durable restore registry: an accepted handoff
/// for a session with a mirrored `restore-record` event materializes a
/// local registry record so the EXISTING restore flow can resurrect the
/// session here (plan `2026-07-09-runner-session-history-cloud-sync` §3.4).
pub fn start_receiver_task(
    registry: Arc<SessionRegistry>,
    lifecycle_store: Arc<SessionLifecycleStore>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(run_receiver_loop(registry, lifecycle_store))
}

/// Derive coord's `/ws` URL (with the `sessions` subscription) from the
/// resolved coord HTTP base in `CoordSync`. The runner's `CoordSync` stores
/// the coord base in HTTP(S) form; the shared builder swaps the scheme and
/// appends `/ws` idempotently. Coord narrows the subscription to
/// `qontinui.sessions.<tenant>.<device>.*` from the upgrade credential — the
/// receiver's own filter ([`parse_handoff_push`]) already matches on
/// `.<self-device>.handoff_request`, so the tighter server-side scope
/// removes only frames it discarded anyway.
fn coord_ws_url(coord_http_base: &str) -> String {
    qontinui_runner_lib::coord_ws::build_ws_url(
        coord_http_base,
        qontinui_runner_lib::coord_ws::Subscription::Sessions,
    )
}

/// The receiver loop. Reconnects the coord `/ws` push subscription with
/// capped exponential backoff; on each successful connect it fires the
/// catch-up GET, then pumps inbound frames until the socket drops.
async fn run_receiver_loop(
    registry: Arc<SessionRegistry>,
    lifecycle_store: Arc<SessionLifecycleStore>,
) {
    let http = registry.coord_sync().http_client();
    let coord_url = registry.coord_sync().coord_url().to_string();
    let device_id = registry.machine_id();
    let ws_url = coord_ws_url(&coord_url);

    tracing::info!(
        coord_url = %coord_url,
        ws_url = %ws_url,
        device = %device_id,
        "session handoff: push receiver starting"
    );

    // Lives for the whole receiver (every reconnect): the duplicate-spawn
    // guard has to outlive the socket, or a reconnect would forget it.
    let sources = MaterializedSources::default();

    let mut backoff = RECONNECT_BACKOFF_FLOOR;
    loop {
        match connect_and_pump(
            &registry,
            &lifecycle_store,
            &http,
            &coord_url,
            &ws_url,
            device_id,
            &sources,
        )
        .await
        {
            Ok(()) => {
                tracing::debug!("session handoff: push WS closed cleanly; reconnecting");
                backoff = RECONNECT_BACKOFF_FLOOR;
            }
            Err(e) => {
                tracing::debug!(
                    error = %e,
                    backoff_secs = backoff.as_secs(),
                    "session handoff: push WS error; reconnecting after backoff"
                );
            }
        }
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(RECONNECT_BACKOFF_CEIL);
    }
}

/// One connect-and-pump iteration: open the coord `/ws` subscription, run
/// the on-connect catch-up, then forward each inbound `handoff_request`
/// frame addressed to this device to [`materialize`]. Returns on
/// disconnect (Ok = clean close, Err = transport error).
async fn connect_and_pump(
    registry: &Arc<SessionRegistry>,
    lifecycle_store: &Arc<SessionLifecycleStore>,
    http: &reqwest::Client,
    coord_url: &str,
    ws_url: &str,
    device_id: Uuid,
    sources: &MaterializedSources,
) -> Result<(), HandoffError> {
    let mut ws = qontinui_runner_lib::coord_ws::connect(ws_url, "session handoff")
        .await
        .map_err(|e| HandoffError::Http(format!("connect coord /ws {ws_url}: {e}")))?;

    tracing::info!(device = %device_id, "session handoff: push WS connected");

    // On-connect catch-up: replay anything that landed while we were
    // offline. Best-effort — a failure here doesn't abort the pump (the push
    // path still works, and the next tick or reconnect retries it).
    //
    // Coord's device drain (plan `2026-09-13-drained-runner-never-reaches-idle`):
    // a handoff or respawn materialized here is autonomous, so the catch-up
    // decides against a real drain read, not the not-yet-read boot state.
    // Rows it defers stay pending on coord and are replayed by the
    // paused->allowed arm of the pump below.
    crate::coord_drain_state::await_boot_read(std::time::Duration::from_secs(15)).await;
    run_all_catchups(
        CatchupPass::OnConnect,
        registry,
        lifecycle_store,
        http,
        coord_url,
        device_id,
        sources,
    )
    .await;

    // Periodic catch-up (module doc, point 3): bounds the delivery window on a
    // socket that is up but delivers nothing — the pre-`subscribe=` coord.
    let mut catchup_tick = catchup_interval();
    // The first tick fires immediately; the on-connect catch-up just ran.
    catchup_tick.tick().await;

    // A row the drain deferred is not pushed a second time, so without this arm
    // it would wait for the next reconnect or tick.
    let mut drain_rx = crate::coord_drain_state::subscribe();
    let mut autonomous_allowed = crate::coord_drain_state::current().allows_autonomous_spawns();
    let mut drain_rx_open = true;

    loop {
        tokio::select! {
            _ = catchup_tick.tick() => {
                run_all_catchups(CatchupPass::Tick, registry, lifecycle_store, http, coord_url, device_id, sources).await;
            }
            changed = drain_rx.changed(), if drain_rx_open => {
                if changed.is_err() {
                    // The sender lives in a static and is never dropped; stop
                    // polling a closed channel rather than spin on it.
                    drain_rx_open = false;
                    continue;
                }
                let now_allowed = drain_rx.borrow_and_update().allows_autonomous_spawns();
                if resumed_after_drain(autonomous_allowed, now_allowed) {
                    tracing::info!(
                        "session handoff: autonomous spawns allowed again — replaying pending \
                         handoffs and respawns the drain deferred"
                    );
                    run_all_catchups(CatchupPass::DrainResumed, registry, lifecycle_store, http, coord_url, device_id, sources).await;
                }
                autonomous_allowed = now_allowed;
            }
            maybe_msg = ws.next() => {
                let Some(msg) = maybe_msg else {
                    // Stream ended (peer hung up without a Close frame).
                    return Ok(());
                };
                let msg = msg.map_err(|e| HandoffError::Http(format!("coord /ws recv: {e}")))?;
                match msg {
                    tokio_tungstenite::tungstenite::Message::Text(t) => {
                        handle_push_frame(
                            registry,
                            lifecycle_store,
                            http,
                            coord_url,
                            device_id,
                            sources,
                            t.as_str(),
                        )
                        .await;
                    }
                    tokio_tungstenite::tungstenite::Message::Binary(b) => {
                        let s = String::from_utf8_lossy(&b);
                        handle_push_frame(
                            registry,
                            lifecycle_store,
                            http,
                            coord_url,
                            device_id,
                            sources,
                            &s,
                        )
                        .await;
                    }
                    tokio_tungstenite::tungstenite::Message::Ping(p) => {
                        // Keep the socket alive — coord's `/ws` answers our pings,
                        // but reply to server pings too.
                        let _ = ws
                            .send(tokio_tungstenite::tungstenite::Message::Pong(p))
                            .await;
                    }
                    tokio_tungstenite::tungstenite::Message::Close(_) => {
                        tracing::debug!("session handoff: push WS closed by peer");
                        return Ok(());
                    }
                    _ => {}
                }
            }
        }
    }
}

/// Which pass is asking for a catch-up — the input to [`catchups_for`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CatchupPass {
    /// A (re)connect of this receiver's socket. Everything that landed while
    /// the socket was down has to be replayed, so this runs EVERY arm.
    OnConnect,
    /// A [`CATCHUP_TICK`] on a live socket.
    Tick,
    /// Coord's device drain just lifted (paused -> allowed). Only the arms the
    /// drain actually DEFERS need replaying: a row it deferred stays pending on
    /// coord and is never pushed again, so nothing else would deliver it before
    /// the next reconnect. Plan `2026-09-13-drained-runner-never-reaches-idle`.
    DrainResumed,
}

/// The catch-up arms this one socket's (re)connect drives, each on its own
/// coord route. They ride together because they share a socket, not because
/// they share a schedule — see [`catchups_for`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CatchupKind {
    /// The durable pending `handoff_request` list.
    Handoff,
    /// The pending respawn requests (`session::respawn`).
    Respawn,
    /// The remote-ATTACH grants (`session::attach`).
    Attach,
    /// The remote-CREATE grants (`session::create`).
    Create,
}

/// Which arms a pass drives, and the whole of why the two passes differ.
///
/// `OnConnect` runs all four: a socket that was down missed every push, so
/// every arm needs its replay.
///
/// `Tick` runs only the two arms that have **no other periodic owner**:
///
/// - `Handoff` and `Respawn` are driven by nothing else in the process. The
///   on-connect replay was their only backstop, which is the gap
///   [`CATCHUP_TICK`] exists to close.
/// - `Attach` and `Create` each already have a process-lifetime poll task —
///   `session::attach::start_poll_task` and `session::create::start_poll_task`,
///   both spawned in `main.rs`, each on its own `POLL_INTERVAL`. Create's
///   equals [`CATCHUP_TICK`]; attach's is SHORTER (15 s, sized against the
///   source's `ATTACH_TIMEOUT` — see `session::attach::POLL_INTERVAL`).
///   Driving either from here as well issues a second GET to a route its own
///   task already polls at least as often, and delivers nothing that task
///   would not have delivered within the same tick.
///
/// Expressed as a function over the pass rather than as "which helper the
/// select arm happens to call", because the asymmetry IS the decision and at
/// the call site it is invisible.
pub(super) fn catchups_for(pass: CatchupPass) -> &'static [CatchupKind] {
    match pass {
        CatchupPass::OnConnect => &[
            CatchupKind::Handoff,
            CatchupKind::Respawn,
            CatchupKind::Attach,
            CatchupKind::Create,
        ],
        CatchupPass::Tick => &[CatchupKind::Handoff, CatchupKind::Respawn],
        // The same two arms as `Tick`, for a DIFFERENT reason, so it is spelled
        // separately rather than aliased: these are the spawning arms the drain
        // gate defers. `Attach` and `Create` mint grants rather than spawning,
        // are never deferred, and so need no replay here.
        CatchupPass::DrainResumed => &[CatchupKind::Handoff, CatchupKind::Respawn],
    }
}

/// Run the catch-up GETs [`catchups_for`] selects for `pass`, in order. Each
/// one is best-effort and self-logging; none aborts the pump.
async fn run_all_catchups(
    pass: CatchupPass,
    registry: &Arc<SessionRegistry>,
    lifecycle_store: &Arc<SessionLifecycleStore>,
    http: &reqwest::Client,
    coord_url: &str,
    device_id: Uuid,
    sources: &MaterializedSources,
) {
    for kind in catchups_for(pass) {
        match kind {
            // The durable `handoff_request` event row in coord is the source
            // of truth; this GET drains it.
            CatchupKind::Handoff => {
                run_catchup(
                    registry,
                    lifecycle_store,
                    http,
                    coord_url,
                    device_id,
                    sources,
                )
                .await
            }
            // The RESPAWN catch-up, on its own coord route. Separate on
            // purpose: the handoff read filters `s.state <> 'closed'` (fatal
            // for a respawn, whose source is closed by construction) and
            // `PendingHandoff` carries neither the account pin nor the Claude
            // session id. Plan `2026-08-26-sessions-console-consolidation` §6
            // Phase 5. Safe to repeat on the tick without a
            // [`MaterializedSources`]-style guard: its dedup is coord's
            // server-side materialized-child filter (keyed on the
            // `parent_session_id` the resume stamps), and the per-session
            // `MIGRATION_CAP` bounds any residue at 3 per 24 h rather than
            // one per tick.
            CatchupKind::Respawn => {
                super::respawn::run_catchup(registry, lifecycle_store, http, coord_url, device_id)
                    .await
            }
            // The remote-ATTACH grants coord minted for this device while the
            // socket was down (plan
            // `2026-08-31-remote-session-tabs-in-runner-terminal`, Phase 3c),
            // into the table the backend relay's terminal handlers enforce
            // against.
            CatchupKind::Attach => {
                super::attach::run_catchup(
                    http,
                    coord_url,
                    device_id,
                    super::attach::CATCHUP_TIMEOUT,
                )
                .await
            }
            // The remote-CREATE grants beside them (plan
            // `2026-09-11-headless-runner-parity-from-a-headed-runner`, Phase
            // 3b). Without this the target has no source for the grants coord
            // minted while it was down, and a `terminal_create` arriving under
            // one of them is refused — correct, but avoidable.
            CatchupKind::Create => {
                super::create::run_catchup(
                    http,
                    coord_url,
                    device_id,
                    super::create::CATCHUP_TIMEOUT,
                )
                .await
            }
        }
    }
}

/// PURE: whether a drain-state change should replay the deferred catch-up —
/// only a transition from paused (drained or unknown) to allowed.
fn resumed_after_drain(was_allowed: bool, now_allowed: bool) -> bool {
    !was_allowed && now_allowed
}

/// Run the one-shot handoff catch-up: GET the durable pending list and
/// materialize each. Used on every (re)connect and on every [`CATCHUP_TICK`].
async fn run_catchup(
    registry: &Arc<SessionRegistry>,
    lifecycle_store: &Arc<SessionLifecycleStore>,
    http: &reqwest::Client,
    coord_url: &str,
    device_id: Uuid,
    sources: &MaterializedSources,
) {
    match fetch_pending(http, coord_url, device_id).await {
        Ok(pending) => {
            if !pending.is_empty() {
                tracing::info!(
                    count = pending.len(),
                    "session handoff: catch-up replaying pending handoffs"
                );
            }
            for handoff in pending {
                materialize_logged(
                    registry,
                    lifecycle_store,
                    http,
                    coord_url,
                    sources,
                    &handoff,
                )
                .await;
            }
        }
        Err(HandoffError::Status(401 | 403, _)) => {
            // Pre-pairing / early-reconnect window: coord (once it gates the
            // handoff readers with FleetPrincipal) rejects the anonymous GET
            // until the device-JWT lands. Not fatal — the catch-up re-runs on
            // the next (re)connect, and the push path stays active. One line.
            tracing::warn!(
                "session handoff: catch-up GET unauthorized (401/403) — retrying after device pairing/auth"
            );
        }
        Err(e) => {
            tracing::debug!(error = %e, "session handoff: catch-up GET failed (push path still active)");
        }
    }
    // Whatever the GET answered — including a failure — re-send every close
    // that is due. Coord stops listing a source once this device's child row
    // exists, so the pending list above cannot be what drives these retries.
    retry_due_closes(http, coord_url, sources).await;
}

/// Re-send the close of every materialized source whose close is pending and
/// due (module doc, point 3), each under the tenant scope recorded when its
/// child started, and file each outcome. Driven by [`MaterializedSources`],
/// never by coord's pending list.
///
/// Sends at most [`CLOSE_RETRY_PASS_CAP`] closes, most overdue first: this
/// runs inline on the socket pump, so a large backlog against an unresponsive
/// coord must not hold the pump for the whole backlog. The rest stay pending
/// and due, and the next pass picks them up.
async fn retry_due_closes(http: &reqwest::Client, coord_url: &str, sources: &MaterializedSources) {
    let due = sources.due_closes(Instant::now());
    if due.len() > CLOSE_RETRY_PASS_CAP {
        tracing::debug!(
            due = due.len(),
            cap = CLOSE_RETRY_PASS_CAP,
            "session handoff: more source closes due than one pass sends; the rest wait for the next pass"
        );
    }
    for (source, tenant) in due.into_iter().take(CLOSE_RETRY_PASS_CAP) {
        retry_close(http, coord_url, sources, source, tenant).await;
    }
}

/// One close-only retry of `source`, filed against the moment it started.
async fn retry_close(
    http: &reqwest::Client,
    coord_url: &str,
    sources: &MaterializedSources,
    source: Uuid,
    tenant: TenantScope,
) {
    let attempt = attempt_close(http, coord_url, source, tenant).await;
    settle_close(sources, source, attempt.close, true, attempt.started_at);
}

/// One close of a source, with the instant its request STARTED — what the
/// retry schedule is measured from ([`MaterializedSources::record`]).
#[derive(Debug)]
pub(super) struct CloseAttempt {
    /// Taken immediately before the request, so nothing that ran before it
    /// (in `materialize`: the drain gate, the state fetch, the claim
    /// re-acquires, the scrollback replay) is counted against the backoff.
    started_at: Instant,
    close: SourceClose,
}

/// [`close_source`], timed from the start of its request.
async fn attempt_close(
    http: &reqwest::Client,
    coord_url: &str,
    source_session_id: Uuid,
    tenant: TenantScope,
) -> CloseAttempt {
    let started_at = Instant::now();
    let close = close_source(http, coord_url, source_session_id, tenant).await;
    CloseAttempt { started_at, close }
}

/// Parse one inbound coord `/ws` envelope. Coord wraps each pub/sub
/// message as `{"channel": "<subject>", "payload": "<json-string>"}`.
/// We accept handoff frames whose channel is
/// `qontinui.sessions.<tenant>.<self-device>.handoff_request` (the
/// machine-scoped subject coord publishes on for the TARGET device), then
/// materialize. Frames for other devices / other subjects are ignored.
async fn handle_push_frame(
    registry: &Arc<SessionRegistry>,
    lifecycle_store: &Arc<SessionLifecycleStore>,
    http: &reqwest::Client,
    coord_url: &str,
    device_id: Uuid,
    sources: &MaterializedSources,
    text: &str,
) {
    // The RESPAWN arm shares this one socket (coord publishes respawns on the
    // same `qontinui.sessions.<tenant>.<device>.<kind>` family). The two arms
    // are disambiguated ONLY by the channel's trailing segment — this parser
    // requires `.handoff_request`, `parse_respawn_push` requires
    // `.respawn_request` — so neither can swallow the other's frames and no
    // frame is materialized twice. Both directions are asserted in
    // `respawn`'s tests.
    super::respawn::handle_push_frame(registry, lifecycle_store, http, coord_url, device_id, text)
        .await;
    // The ATTACH arm — third suffix on the same socket (`.attach_request`),
    // same disambiguation. Records a grant; materializes nothing.
    super::attach::handle_push_frame(device_id, text);
    // The CREATE arm — fourth suffix on the same socket (`.create_request`),
    // same disambiguation. Records a grant; materializes nothing. The two
    // grant arms are disjoint in both directions (asserted in `create`'s
    // tests), so an attach grant can never land in the create table.
    super::create::handle_push_frame(device_id, text);

    let Some(handoff) = parse_handoff_push(text, device_id) else {
        return;
    };
    tracing::info!(
        source = %handoff.source_session_id,
        "session handoff: push received; materializing"
    );
    // A push frame is a FRESH handoff request for this source addressed to
    // this device — unlike a catch-up sighting, which can repeat an old one.
    // It revives a `not_handoff_target` refusal (or an abandoned retry) that a
    // re-handoff back here has made stale, and makes a backing-off close due.
    if sources.revive_on_fresh_request(handoff.source_session_id, Instant::now()) {
        tracing::info!(
            source = %handoff.source_session_id,
            "session handoff: fresh handoff request for a source this process already \
             materialized; its close is due again"
        );
    }
    materialize_logged(
        registry,
        lifecycle_store,
        http,
        coord_url,
        sources,
        &handoff,
    )
    .await;
}

/// Pure parse+filter of a coord `/ws` envelope into a [`PendingHandoff`]
/// addressed to `device_id`. Returns `None` when the frame isn't a
/// handoff for this device (so the pump can ignore it). Factored out so
/// the unit tests can exercise the channel-matching + payload-decode
/// without a live WS.
pub(super) fn parse_handoff_push(text: &str, device_id: Uuid) -> Option<PendingHandoff> {
    let envelope: serde_json::Value = serde_json::from_str(text).ok()?;

    // Coord `/ws` envelope: {"channel": "<subject>", "payload": "<json>"}.
    // The payload is itself a JSON string (Redis pub/sub carries strings).
    let channel = envelope.get("channel").and_then(|c| c.as_str())?;

    // Match `qontinui.sessions.<tenant>.<device>.handoff_request` and
    // require the device segment == this device. We match on the
    // device + kind segments rather than reconstructing the full subject
    // (we don't carry the tenant here) — the device segment is the
    // address, the trailing segment is the event kind.
    let suffix = format!(".{device_id}.handoff_request");
    if !channel.starts_with("qontinui.sessions.") || !channel.ends_with(&suffix) {
        return None;
    }

    // Payload may be a JSON string (Redis arm) or an inlined object
    // (defensive — some envelopes inline). Handle both.
    let payload_val = match envelope.get("payload") {
        Some(serde_json::Value::String(s)) => serde_json::from_str::<serde_json::Value>(s).ok()?,
        Some(other) => other.clone(),
        None => return None,
    };

    let source_session_id = payload_val
        .get("source_session_id")
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok())?;
    let target_device_id = payload_val
        .get("target_device_id")
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok())
        .unwrap_or(device_id);
    // Defense-in-depth: the channel already filtered by device, but if a
    // payload's target disagrees, trust the address, not the body.
    if target_device_id != device_id {
        return None;
    }
    let tenant_id = payload_val
        .get("tenant_id")
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok())
        .unwrap_or_else(Uuid::nil);
    let session_kind = payload_val
        .get("session_kind")
        .and_then(|v| v.as_str())
        .unwrap_or("terminal_shell")
        .to_string();

    Some(PendingHandoff {
        source_session_id,
        target_device_id,
        tenant_id,
        session_kind,
    })
}

/// Materialize a handoff and log (but swallow) any error. The source is
/// left intact on failure so the next push/catch-up retries.
///
/// A source this process has ALREADY started a child for (`sources`) is not
/// materialized again: the repeat sighting means the earlier `close_source`
/// failed and the handoff is still pending, so only the close is retried —
/// never a second child (module doc, point 3). A source whose close already
/// reached a terminal answer is not touched at all.
async fn materialize_logged(
    registry: &Arc<SessionRegistry>,
    lifecycle_store: &Arc<SessionLifecycleStore>,
    http: &reqwest::Client,
    coord_url: &str,
    sources: &MaterializedSources,
    handoff: &PendingHandoff,
) {
    let source = handoff.source_session_id;
    match sources.sighting(source) {
        Sighting::Materialize => {
            match materialize(registry, lifecycle_store, http, coord_url, sources, handoff).await {
                // Scheduled from the close's OWN start, not from the start of
                // `materialize`: everything before the close (drain gate, state
                // fetch, claim re-acquires, scrollback) can take seconds, and
                // counting it would make a retryable first close due again in
                // this very pass's `retry_due_closes`.
                Ok(attempt) => {
                    settle_close(sources, source, attempt.close, false, attempt.started_at)
                }
                Err(e) => tracing::warn!(
                    source = %source,
                    error = %e,
                    "session handoff: materialize failed; source left intact, will retry on next push/catch-up"
                ),
            }
        }
        Sighting::CloseOnly(_) => match sources.due(source, Instant::now()) {
            Some(tenant) => {
                tracing::info!(
                    source = %source,
                    "session handoff: source already materialized by this process; retrying close only (no second child)"
                );
                retry_close(http, coord_url, sources, source, tenant).await;
            }
            None => tracing::debug!(
                source = %source,
                "session handoff: source already materialized by this process; its close is backing off"
            ),
        },
        Sighting::Settled => tracing::debug!(
            source = %source,
            "session handoff: source's close already settled by this process; nothing to send"
        ),
    }
}

/// Record one close outcome in `sources` and log it.
///
/// A terminal answer — closed, or refused for good — settles the source so
/// the next sighting and the next retry pass send nothing; a refusal is logged
/// ONCE at warn. A retryable answer is rescheduled on the source's backoff:
/// its first failure logs at warn, repeats at debug (each one names the next
/// delay), and abandoning it after [`CLOSE_RETRY_LIFETIME`] logs once at warn.
fn settle_close(
    sources: &MaterializedSources,
    source: Uuid,
    close: SourceClose,
    deferred: bool,
    attempted_at: Instant,
) {
    let attempt = if deferred { "deferred close" } else { "close" };
    match (sources.record(source, &close, attempted_at), &close) {
        (_, SourceClose::Closed { already_closed }) => tracing::info!(
            source = %source,
            already_closed,
            "session handoff: {attempt} of the source succeeded"
        ),
        (Recorded::Refused { revivable: true }, SourceClose::Refused { status, error }) => {
            tracing::warn!(
                source = %source,
                status,
                error = %error,
                "session handoff: coord refused the {attempt} of the source (this device is no \
                 longer its handoff target); not retrying unless a fresh handoff request \
                 re-targets it here (coord's staleness reaper remains the backstop)"
            )
        }
        (_, SourceClose::Refused { status, error }) => tracing::warn!(
            source = %source,
            status,
            error = %error,
            "session handoff: coord refused the {attempt} of the source for good; not retrying \
             (coord's staleness reaper remains the backstop)"
        ),
        (
            Recorded::Retry {
                failures,
                retry_in,
                first: true,
            },
            SourceClose::Retry(e),
        ) => tracing::warn!(
            source = %source,
            error = %e,
            failures,
            retry_in_secs = retry_in.as_secs(),
            "session handoff: {attempt} of the source failed; retrying with backoff"
        ),
        (
            Recorded::Retry {
                failures, retry_in, ..
            },
            SourceClose::Retry(e),
        ) => tracing::debug!(
            source = %source,
            error = %e,
            failures,
            retry_in_secs = retry_in.as_secs(),
            "session handoff: {attempt} of the source failed again; still backing off"
        ),
        (Recorded::AlreadySettled, SourceClose::Retry(e)) => tracing::debug!(
            source = %source,
            error = %e,
            "session handoff: late retryable answer to the {attempt} of a source whose close \
             already settled; ignored"
        ),
        (Recorded::Abandoned { failures }, SourceClose::Retry(e)) => tracing::warn!(
            source = %source,
            error = %e,
            failures,
            "session handoff: giving up on closing the source after {} hours of retries \
             (coord's staleness reaper remains the backstop)",
            CLOSE_RETRY_LIFETIME.as_secs() / 3600
        ),
        (_, SourceClose::Retry(e)) => tracing::warn!(
            source = %source,
            error = %e,
            "session handoff: {attempt} of the source failed for a source this process never \
             materialized; not scheduling a retry"
        ),
    }
}

/// Fetch the durable pending-handoff list for this device. Used by the
/// on-(re)connect catch-up. (Same coord endpoint the previous poll loop
/// used — now invoked once per connect rather than every 5s.)
async fn fetch_pending(
    http: &reqwest::Client,
    coord_url: &str,
    device_id: Uuid,
) -> Result<Vec<PendingHandoff>, HandoffError> {
    let url = format!(
        "{}/sessions/handoff-requests?device_id={}",
        coord_url.trim_end_matches('/'),
        device_id
    );
    let resp = crate::coord_http::coord_get(http, &url)
        .send()
        .await
        .map_err(|e| HandoffError::Http(format!("GET {url}: {e}")))?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(HandoffError::Status(
            status.as_u16(),
            body.chars().take(300).collect(),
        ));
    }
    let parsed: HandoffListResponse = resp
        .json()
        .await
        .map_err(|e| HandoffError::Parse(format!("decode handoff list: {e}")))?;
    Ok(parsed.handoffs)
}

/// Fetch the state-transfer bundle for a source session.
pub(super) async fn fetch_state(
    http: &reqwest::Client,
    coord_url: &str,
    source_session_id: Uuid,
) -> Result<HandoffState, HandoffError> {
    let url = format!(
        "{}/sessions/{}/handoff-state",
        coord_url.trim_end_matches('/'),
        source_session_id
    );
    let resp = crate::coord_http::coord_get(http, &url)
        .send()
        .await
        .map_err(|e| HandoffError::Http(format!("GET {url}: {e}")))?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(HandoffError::Status(
            status.as_u16(),
            body.chars().take(300).collect(),
        ));
    }
    resp.json()
        .await
        .map_err(|e| HandoffError::Parse(format!("decode handoff state: {e}")))
}

/// Materialize one handoff: build the child intent, start the child
/// session with `parent_session_id`, re-acquire claims, replay
/// scrollback, then close the source. Plan §Phase 7.
///
/// `Err` is a failure BEFORE the child started (the source is left intact and
/// the next sighting materializes again); once the child exists the result is
/// `Ok` carrying the close outcome and the instant the close request started,
/// which the caller records.
async fn materialize(
    registry: &Arc<SessionRegistry>,
    lifecycle_store: &Arc<SessionLifecycleStore>,
    http: &reqwest::Client,
    coord_url: &str,
    sources: &MaterializedSources,
    handoff: &PendingHandoff,
) -> Result<CloseAttempt, HandoffError> {
    // Coord's device drain (plan `2026-09-13-drained-runner-never-reaches-idle`,
    // D3): materializing a handoff starts a session on this device on coord's
    // say-so, so it is a coord dispatch and is deferred while the drain holds.
    // Before any fetch, so the source stays intact and the pending handoff
    // replays on the next push/catch-up.
    if let crate::coord_drain_state::DrainGate::Defer { reason, .. } =
        crate::coord_drain_state::drain_gate_for_work(
            crate::coord_drain_state::SpawnOrigin::CoordDispatch,
            &format!("handoff:{}", handoff.source_session_id),
        )
    {
        return Err(HandoffError::Session(format!(
            "deferred by the coord device drain: {reason}"
        )));
    }
    let state = fetch_state(http, coord_url, handoff.source_session_id).await?;

    let intent = build_child_intent(&state, HANDOFF_CONTINUATION_NOTE)?;
    // Captured before `intent` moves into the registry: the child inherits the
    // SOURCE session's tenant (`build_child_intent` carries `tenant_id` across),
    // and the claims re-acquired below belong to that same tenant.
    let tenant = TenantScope::for_session(intent.tenant_id);

    // Start the child session locally with lineage back to the source.
    let child = registry
        .start_with_parent(intent, handoff.source_session_id)
        .map_err(|e| HandoffError::Session(e.to_string()))?;
    let child_id = child.id();
    // Marked the moment the child EXISTS — before the close below, whose
    // failure is exactly what makes this source get sighted again.
    sources.mark(handoff.source_session_id, tenant);

    tracing::info!(
        source = %handoff.source_session_id,
        child = %child_id,
        "session handoff: materialized child session"
    );

    // Re-acquire held claims under this device. Idempotent by
    // resource_key; failures are logged but don't abort the handoff —
    // the session row + scrollback are the load-bearing artifacts.
    let device_id = registry.machine_id();
    for claim in &state.held_claims {
        if let Err(e) = reacquire_claim(http, coord_url, claim, device_id, tenant).await {
            tracing::warn!(
                kind = %claim.kind,
                resource_key = %claim.resource_key,
                error = %e,
                "session handoff: claim re-acquire failed (best-effort)"
            );
        }
    }

    // Replay warm-tier scrollback into the new PTY, in order.
    for chunk in &state.output_chunks {
        match base64::engine::general_purpose::STANDARD.decode(&chunk.payload_b64) {
            Ok(bytes) => {
                if let Err(e) = registry.write_input(child_id, &bytes) {
                    tracing::warn!(
                        child = %child_id,
                        error = %e,
                        "session handoff: scrollback replay write failed"
                    );
                    break;
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "session handoff: scrollback chunk base64 decode failed");
            }
        }
    }

    // Tear down the source FIRST — one-way move. coord's handoff-completion
    // door sets state='closed', closed_at=now(), and runs the close side
    // effects (claim release, `closed` publish) exactly once. This
    // deliberately runs before the restore-registry materialization below
    // (F6): the materialization's bounded SSE read pays an idle wait (up to
    // RESTORE_RECORD_FETCH_DEADLINE) even when the source mirrored nothing,
    // and the load-bearing teardown must not queue behind it. Ordering is
    // safe: coord's close is a soft close (the row and its
    // coord.session_events rows survive), so the events replay still serves
    // the mirror afterwards.
    let close_attempt = attempt_close(http, coord_url, handoff.source_session_id, tenant).await;

    // Phase 4 (session-history cloud sync §3.4) — materialize a local
    // restore-registry record from the source's newest mirrored
    // `restore-record` event, so the EXISTING restore flow can resurrect
    // the session on THIS machine. Best-effort: a session with no mirror
    // (cloud sync off at the source, no linked terminal) simply has no
    // record to materialize, and no failure here aborts the handoff. Runs
    // regardless of the close outcome so a failed close doesn't also cost
    // the registry record.
    materialize_restore_registry(
        registry,
        lifecycle_store,
        http,
        coord_url,
        handoff.source_session_id,
        child_id,
    )
    .await;

    Ok(close_attempt)
}

// ---------------------------------------------------------------------------
// Phase 4 — restore-registry materialization (plan
// `2026-07-09-runner-session-history-cloud-sync` §3.4)
// ---------------------------------------------------------------------------

/// Overall deadline for the bounded `GET /sessions/:id/events` read.
const RESTORE_RECORD_FETCH_DEADLINE: Duration = Duration::from_secs(8);
/// Per-chunk idle timeout: the SSE endpoint replays the durable rows
/// immediately on connect and then live-tails (silence until the next
/// published event, keep-alive pings every 15s) — a short idle gap after
/// the replay burst means the durable window is fully read.
const RESTORE_RECORD_FETCH_IDLE: Duration = Duration::from_secs(2);

/// Fixed UUIDv5 namespace for provisional (terminal-only) handoff record
/// keys: the key is `Uuid::new_v5(&NS, source_session_id)`, so retrying a
/// materialization for the same source upserts the SAME registry record
/// instead of minting a fresh `Uuid::new_v4` duplicate each attempt.
/// Randomly generated once for this purpose — never change it, or retries
/// stop being idempotent across builds.
const HANDOFF_PROVISIONAL_KEY_NS: Uuid = Uuid::from_u128(0x8f1d_5b0e_43c2_4a7a_9f6e_2d81_c4b3_7a59);

/// Fetch + materialize, logging (but swallowing) every failure.
async fn materialize_restore_registry(
    registry: &Arc<SessionRegistry>,
    lifecycle_store: &Arc<SessionLifecycleStore>,
    http: &reqwest::Client,
    coord_url: &str,
    source_session_id: Uuid,
    child_id: Uuid,
) {
    let Some(payload) = fetch_latest_restore_record(http, coord_url, source_session_id).await
    else {
        tracing::debug!(
            source = %source_session_id,
            "session handoff: no restore-record event for source — skipping registry materialization"
        );
        return;
    };

    // Attach the record to the child's REAL PTY terminal when it has one,
    // so the registry's liveness poll matches a live terminal instead of
    // orphan-closing a synthetic id. Non-PTY child kinds fall back to a
    // deterministic placeholder.
    let terminal_id = registry
        .pty_terminal_id(child_id)
        .unwrap_or_else(|| format!("handoff-{source_session_id}"));

    let record = registry_record_from_restore_payload(&payload, &terminal_id, source_session_id);
    let session_key = record.claude_session_id.clone();
    // The tier this record was MATERIALIZED at: `confirmed_at` is `Some` iff
    // the payload parsed as wire `full` AND carried a non-blank, shell-safe id
    // (see `registry_record_from_restore_payload`). Logged in the wire
    // vocabulary via the shared constants (never a re-typed literal), beside the
    // payload's own `restore_tier` verbatim — the two differ exactly when this
    // machine downgraded the payload (a `full` with an absent, blank or unsafe
    // id, or an unknown or absent tier), which is the case worth seeing.
    let materialized_tier = if record.confirmed_at.is_some() {
        TIER_FULL
    } else {
        TIER_TERMINAL_ONLY
    };
    let mirrored_tier = payload
        .get("restore_tier")
        .and_then(|v| v.as_str())
        .unwrap_or("<absent>");
    lifecycle_store.record_open(record);
    tracing::info!(
        source = %source_session_id,
        child = %child_id,
        session = %session_key,
        tier = materialized_tier,
        mirrored_tier = %mirrored_tier,
        "session handoff: materialized restore-registry record"
    );
}

/// Read the durable-replay window of `GET /sessions/:id/events` (SSE) and
/// return the payload of the NEWEST (highest-seq) `restore-record` event,
/// if any. Coord's `HandoffState` bundle does not carry session events, so
/// this is the read path for the mirrored registry record. Bounded: the
/// stream live-tails after the replay, so reading stops on a short idle
/// gap, the overall deadline, or stream end — whichever comes first.
async fn fetch_latest_restore_record(
    http: &reqwest::Client,
    coord_url: &str,
    session_id: Uuid,
) -> Option<serde_json::Value> {
    let url = format!(
        "{}/sessions/{}/events",
        coord_url.trim_end_matches('/'),
        session_id
    );
    let resp = match crate::coord_http::coord_get(http, &url).send().await {
        Ok(r) => r,
        Err(e) => {
            tracing::debug!(error = %e, "session handoff: restore-record fetch failed (GET events)");
            return None;
        }
    };
    if !resp.status().is_success() {
        tracing::debug!(
            status = %resp.status(),
            session = %session_id,
            "session handoff: restore-record fetch rejected — proceeding without registry record"
        );
        return None;
    }

    let mut resp = resp;
    let mut buf = String::new();
    let deadline = tokio::time::Instant::now() + RESTORE_RECORD_FETCH_DEADLINE;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(RESTORE_RECORD_FETCH_IDLE.min(remaining), resp.chunk()).await {
            Ok(Ok(Some(bytes))) => buf.push_str(&String::from_utf8_lossy(&bytes)),
            Ok(Ok(None)) => break, // stream ended (e.g. no live tail configured)
            Ok(Err(e)) => {
                tracing::debug!(error = %e, "session handoff: restore-record fetch stream error");
                break;
            }
            Err(_) => break, // idle — the replay burst is fully read
        }
    }
    latest_restore_record_from_sse(&buf)
}

/// Pure parse of an accumulated SSE buffer into the newest `restore-record`
/// payload. Frames are `\n\n`-separated; each carries `data: <json>` lines
/// (the replay frames serialize coord's `SessionEventRow`:
/// `{id, session_id, seq, event_kind, payload, occurred_at}`). "Newest" is
/// max `seq` with last-wins on ties, so re-emitted (debounce-reset) rows
/// resolve to the latest state.
fn latest_restore_record_from_sse(buf: &str) -> Option<serde_json::Value> {
    let mut best: Option<(i64, serde_json::Value)> = None;
    for frame in buf.split("\n\n") {
        let mut data = String::new();
        for line in frame.lines() {
            if let Some(rest) = line.strip_prefix("data:") {
                data.push_str(rest.trim_start());
            }
        }
        if data.is_empty() {
            continue;
        }
        let Ok(row) = serde_json::from_str::<serde_json::Value>(&data) else {
            continue;
        };
        if row.get("event_kind").and_then(|v| v.as_str()) != Some(RESTORE_RECORD_EVENT) {
            continue;
        }
        let Some(payload) = row.get("payload").filter(|p| p.is_object()).cloned() else {
            continue;
        };
        let seq = row.get("seq").and_then(|v| v.as_i64()).unwrap_or(0);
        if best.as_ref().map(|(s, _)| seq >= *s).unwrap_or(true) {
            best = Some((seq, payload));
        }
    }
    best.map(|(_, p)| p)
}

/// Pure mapping: a mirrored `restore-record` payload → a local
/// [`TerminalSessionRecord`] that FEEDS THE EXISTING restore flow (the
/// frontend `classifyRestoreAction` gate), honoring tiers honestly:
///
/// - `restore_tier == "full"` with an authoritative id → the record is
///   keyed by that id, origin `authoritative`, CONFIRMED (the emitter only
///   claims `full` for source-confirmed records) — the classifier
///   auto-resumes the conversation via the provider's `--resume <id>`.
/// - anything else (`terminal_only`, an unknown tier, or a `full` whose id is
///   absent or fails [`is_valid_session_id`]) →
///   a deterministic per-source key (UUIDv5 of `source_session_id` under
///   [`HANDOFF_PROVISIONAL_KEY_NS`], so a materialization retry upserts the
///   SAME record instead of piling up duplicates), origin `authoritative`,
///   PROVISIONAL (`confirmed_at` unset) — the classifier's phantom-shell
///   branch restores terminal+cwd with an honest fresh conversation and
///   never types a resume. (NOT `reconciled`: that origin quarantines
///   behind a resume-confirm banner, which would be dishonest for a record
///   that has no conversation to resume.)
///
/// `record_open` stamps the timestamps; placeholders here are overwritten.
fn registry_record_from_restore_payload(
    payload: &serde_json::Value,
    terminal_id: &str,
    source_session_id: Uuid,
) -> TerminalSessionRecord {
    let provider = payload
        .get("provider")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or(DEFAULT_PROVIDER)
        .to_string();
    let cwd = payload
        .get("cwd")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    // The peer's id is an INGRESS into this machine's registry: a peer on a
    // build predating the emitter's shell-safety gate can still mirror `full`
    // for an unsafe id. Such an id is treated exactly like no id — it never
    // becomes a confirmed authoritative record here (plan
    // `2026-08-23-single-source-derived-facts` item 1).
    let authoritative_id = payload
        .get("authoritative_session_id")
        .and_then(|v| v.as_str())
        // Validated RAW, not trimmed: trimming would turn `"abc\n"` into a
        // "safe" `"abc"`, and the emitter never pads an id it sends.
        .filter(|s| is_valid_session_id(s));
    // Parsed through the one wire-vocabulary parser; an absent or unknown
    // spelling (including the frontend's hyphenated `terminal-only`) is not
    // `full`, so it degrades to terminal-only.
    let tier = payload
        .get("restore_tier")
        .and_then(|v| v.as_str())
        .and_then(RestoreTier::from_wire_str);

    let (claude_session_id, confirmed_at) = match (tier, authoritative_id) {
        (Some(RestoreTier::Full), Some(id)) => {
            (id.to_string(), Some(chrono::Utc::now().timestamp_millis()))
        }
        // Honest degrade: no resumable id (absent, blank, or shell-unsafe) ⇒
        // terminal-only semantics under a DETERMINISTIC per-source key (a real
        // UUID, so shell-safety validation and future confirmations behave
        // normally; v5 of the source session id, so a materialization retry is
        // idempotent — record_open upserts by this key instead of minting a
        // duplicate).
        _ => (
            Uuid::new_v5(&HANDOFF_PROVISIONAL_KEY_NS, source_session_id.as_bytes()).to_string(),
            None,
        ),
    };

    TerminalSessionRecord {
        claude_session_id,
        config_dir: None,
        working_dir: cwd,
        page_id: "default".to_string(),
        zone_index: 0,
        title: Some(format!("{provider} (handed off)")),
        terminal_id: terminal_id.to_string(),
        // record_open seeds these from `now`; values here are placeholders.
        opened_at: 0,
        last_seen_at: 0,
        state: "open".to_string(),
        closed_at: None,
        close_reason: None,
        provider,
        origin: Some(ORIGIN_AUTHORITATIVE.to_string()),
        restore_pending_at: None,
        confirmed_at,
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
        finish_reason: None,
        finish_synced: false,
        spawn_device_default: None,
        adopted_from: None,
    }
}

/// Build the child session [`Intent`] from the source state. cwd comes
/// from `repo` (the PTY transport uses `intent.repo` as the working
/// dir); `declared_paths` + branch carry over verbatim. The purpose is
/// annotated so the dashboard shows the lineage at a glance.
pub(super) fn build_child_intent(
    state: &HandoffState,
    continuation_note: &str,
) -> Result<Intent, HandoffError> {
    let kind = SessionKind::parse(&state.session_kind).ok_or_else(|| {
        HandoffError::Parse(format!("unknown session_kind: {}", state.session_kind))
    })?;

    // Pull purpose + declared_paths + share_output from the source intent
    // JSON. Default sensibly on any missing field so a sparse source
    // intent still materializes.
    let src = &state.intent;
    let source_purpose = src
        .get("purpose")
        .and_then(|v| v.as_str())
        .unwrap_or("handoff session");
    let purpose = format!("{source_purpose} ({continuation_note})");
    let declared_paths = src
        .get("declared_paths")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|p| p.as_str())
                .map(std::path::PathBuf::from)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    // This reads the raw persisted JSON directly rather than going through
    // `Intent`'s `Deserialize` impl, so `Intent::share_output`'s own
    // `#[serde(default = "default_true")]` (plan
    // `2026-09-22-transcript-sync-default-on-with-tenant-and-user-controls`
    // §3.5) has no effect here — this fallback is a SECOND, independent
    // default-resolution point that must be kept in sync by hand. A sparse
    // source intent (predating this field, or from any other reason the key
    // is absent) must resolve the same way a missing key resolves everywhere
    // else: `true`, ship-on-by-default per `engineering-priorities`
    // `capability-ships-enabled`.
    let share_output = src
        .get("share_output")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let redact_secrets = src.get("redact_secrets").and_then(|v| v.as_bool());
    // Dual-read: coord renamed the wire key `plan_slug` → `work_unit_slug`.
    // Read the new name first and fall back to the legacy one so a source
    // intent written by EITHER coord release materializes.
    // (The writer leg has since switched: `Intent` now carries the canonical
    // `work_unit_slug` and emits ONLY that key — plan
    // `2026-07-30-coord-web-plan-slug-wire-key-retirement` Phase 1 step 6.
    // That makes this fallback the ONLY thing standing between a pre-rename
    // blob and a lost slug.)
    //
    // ** THE `plan_slug` FALLBACK IS PERMANENT. DO NOT "CLEAN IT UP". **
    // This is not a migration window that eventually closes. `src` is a
    // persisted `coord.sessions.intent` JSONB blob, and Phase 5 of plan
    // `2026-07-30-coord-web-plan-slug-wire-key-retirement` decided (a) LEAVE
    // the historical blobs unmigrated: every session row written before the
    // rename carries only `plan_slug`, and those rows never expire. Deleting
    // the fallback would silently hand `None` to every handoff off a
    // pre-rename session — a quiet degradation, not an error. The three-line
    // fallback is the entire cost of never having to rewrite that table.
    // `.as_str()` must be applied BEFORE the fallback, not after. `.get()`
    // returns `Some(Value::Null)` for a present-but-null key, so an
    // `.or_else(...).and_then(as_str)` chain would treat an explicit
    // `{"work_unit_slug": null, "plan_slug": "x"}` as "new key present" and
    // drop the slug entirely instead of falling back. Coord emits explicit
    // nulls on exactly this shape — its autonomous-dispatch metadata is built
    // with `json!({"plan_slug": slug, "work_unit_slug": slug})`, which
    // serializes `None` as `null` rather than omitting the key.
    let work_unit_slug = src
        .get("work_unit_slug")
        .and_then(|v| v.as_str())
        .or_else(|| src.get("plan_slug").and_then(|v| v.as_str()))
        .map(str::to_string);
    let correlation_topic = src
        .get("correlation_topic")
        .and_then(|v| v.as_str())
        .map(str::to_string);

    // Phase 8b — carry the SOURCE session's tenant binding across the
    // handoff (session tenancy is immutable; the child continues the same
    // tenant's work). Absent on legacy source intents → None, and the
    // registry stamps this machine's default at materialization.
    let tenant_id = src
        .get("tenant_id")
        .and_then(|v| v.as_str())
        .and_then(|s| uuid::Uuid::parse_str(s.trim()).ok());

    Ok(Intent {
        kind,
        purpose,
        repo: state.repo.clone(),
        branch: state.branch.clone(),
        // The child intent is re-serialized under the canonical key alone —
        // the dual-read above is what folds a legacy blob into it.
        work_unit_slug,
        plan_slug: None,
        correlation_topic,
        page_id: None,
        declared_paths,
        share_output,
        redact_secrets,
        tenant_id,
    })
}

/// Re-acquire one claim under `device_id` via `POST /claims/acquire`.
/// The kind string maps to coord's `ClaimKind` snake_case wire form.
/// `tenant` is the SOURCE session's scope, carried down from the child intent
/// `build_child_intent` just derived (Phase 5, plan
/// `2026-08-29-runner-work-scoped-writes-default-tenant-credential` §D1). Passed
/// rather than looked up because the caller is already holding it: the claim
/// being re-acquired belongs to the session being moved, and coord stamps
/// `metadata.tenant_id` on the acquire audit row from the body it is given.
///
/// `pub(super)` because `session::respawn` re-acquires the same claims for the
/// same reason; it derives its `tenant` from its own `child_intent`, so the two
/// callers agree by construction rather than by convention.
pub(super) async fn reacquire_claim(
    http: &reqwest::Client,
    coord_url: &str,
    claim: &HeldClaim,
    device_id: Uuid,
    tenant: TenantScope,
) -> Result<(), HandoffError> {
    let url = format!("{}/claims/acquire", coord_url.trim_end_matches('/'));
    let mut body = json!({
        "kind": claim.kind,
        "resource_key": claim.resource_key,
        "machine_id": device_id.to_string(),
    });
    if let Some(t) = tenant.declared_tenant() {
        body["tenant_id"] = json!(t);
    }
    let resp = crate::auth::attach_device_auth_for(http.post(&url).json(&body), tenant)
        .send()
        .await
        .map_err(|e| HandoffError::Http(format!("POST {url}: {e}")))?;
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(HandoffError::Status(
            status.as_u16(),
            text.chars().take(200).collect(),
        ));
    }
    Ok(())
}

/// Coord's answer to a handoff-completion request, classified for the retry
/// machinery in [`settle_close`].
#[derive(Debug)]
pub(super) enum SourceClose {
    /// `200 {"closed": true, "already_closed": bool}` — done either way:
    /// `already_closed` only says whether THIS request was the one that closed
    /// it.
    Closed { already_closed: bool },
    /// Coord refused the close for a reason a retry from this device cannot
    /// change (`404 session_not_found`, `403 not_handoff_target`). Not retried
    /// on the catch-up cadence. `session_not_found` is terminal; a
    /// `not_handoff_target` refusal is re-sent only when a fresh handoff
    /// request re-targets this device (`revive_on_fresh_request`).
    Refused { status: u16, error: String },
    /// Worth sending again on the catch-up cadence: a transport error, a 5xx,
    /// `403 handoff_child_not_materialized` (the child's row may simply not
    /// have registered with coord yet), or any answer outside the contract —
    /// a 401 in a credential gap, or a 404 from a coord predating the door.
    Retry(HandoffError),
}

/// Classify a handoff-completion response by status and body.
///
/// Only the two refusals coord names as terminal are terminal; an error code
/// is read from the body's `error` field so a bare 404 (a coord without the
/// route yet) stays retryable rather than abandoning the source for good.
pub(super) fn classify_close_response(status: u16, body: &str) -> SourceClose {
    let parsed: Option<serde_json::Value> = serde_json::from_str(body).ok();
    if (200..300).contains(&status) {
        let closed = parsed
            .as_ref()
            .and_then(|v| v.get("closed"))
            .and_then(|v| v.as_bool());
        return match closed {
            Some(true) => SourceClose::Closed {
                already_closed: parsed
                    .as_ref()
                    .and_then(|v| v.get("already_closed"))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
            },
            _ => SourceClose::Retry(HandoffError::Parse(format!(
                "handoff completion answered {status} without `closed: true`: {}",
                body.chars().take(200).collect::<String>()
            ))),
        };
    }
    let error = parsed
        .as_ref()
        .and_then(|v| v.get("error"))
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    match (status, error.as_str()) {
        (404, "session_not_found") | (403, "not_handoff_target") => {
            SourceClose::Refused { status, error }
        }
        _ => SourceClose::Retry(HandoffError::Status(
            status,
            body.chars().take(200).collect(),
        )),
    }
}

/// Close the source session through coord's handoff-completion door,
/// `POST /sessions/:id/handoff/complete` with an empty JSON body.
///
/// `tenant` is the SOURCE session's scope, taken from the child intent exactly
/// as [`reacquire_claim`] takes it: coord authorizes the close against the
/// source's tenant, so this presents THAT binding's device-JWT slot, never the
/// device's default.
///
/// Bounded by its own [`CLOSE_REQUEST_TIMEOUT`] rather than the shared
/// client's: it runs inline on the socket pump. A timeout is a transport error,
/// so it classifies as [`SourceClose::Retry`].
async fn close_source(
    http: &reqwest::Client,
    coord_url: &str,
    source_session_id: Uuid,
    tenant: TenantScope,
) -> SourceClose {
    let url = format!(
        "{}/sessions/{}/handoff/complete",
        coord_url.trim_end_matches('/'),
        source_session_id
    );
    let request = crate::auth::attach_device_auth_for(http.post(&url).json(&json!({})), tenant)
        .timeout(CLOSE_REQUEST_TIMEOUT);
    let resp = match request.send().await {
        Ok(resp) => resp,
        Err(e) => return SourceClose::Retry(HandoffError::Http(format!("POST {url}: {e}"))),
    };
    let status = resp.status().as_u16();
    let body = resp.text().await.unwrap_or_default();
    classify_close_response(status, &body)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // =======================================================================
    // Catch-up dedupe (module doc, point 3): a source already materialized by
    // this process is sighted again only because its close failed, so the
    // second sighting is close-only — never a second child.
    // =======================================================================

    #[test]
    fn handoff_dedupe_first_sighting_materializes_second_is_close_only() {
        let sources = MaterializedSources::default();
        let src = Uuid::new_v4();
        let other = Uuid::new_v4();
        let tenant = TenantScope::Owned(Uuid::new_v4());

        assert_eq!(sources.sighting(src), Sighting::Materialize);
        // The child is started: mark. First record changes the set.
        assert!(sources.mark(src, tenant));
        // Every later sighting of the SAME source (the next tick, the next
        // reconnect's catch-up, a replayed push frame) is close-only, under
        // the SOURCE's tenant captured at materialization.
        assert_eq!(sources.sighting(src), Sighting::CloseOnly(tenant));
        assert_eq!(sources.sighting(src), Sighting::CloseOnly(tenant));
        // Re-marking is idempotent and does not flip the decision or the scope.
        assert!(!sources.mark(src, TenantScope::Device));
        assert_eq!(sources.sighting(src), Sighting::CloseOnly(tenant));
        // A different source is unaffected.
        assert_eq!(sources.sighting(other), Sighting::Materialize);
    }

    #[test]
    fn handoff_dedupe_pure_decision_is_membership() {
        let src = Uuid::new_v4();
        let tenant = TenantScope::Owned(Uuid::new_v4());
        let mut seen = HashMap::new();
        assert_eq!(sighting_for(&seen, src), Sighting::Materialize);
        seen.insert(
            src,
            MaterializedSource {
                tenant,
                close: CloseState::pending_now(Instant::now()),
            },
        );
        assert_eq!(sighting_for(&seen, src), Sighting::CloseOnly(tenant));
        assert_eq!(sighting_for(&seen, Uuid::new_v4()), Sighting::Materialize);
        for settled in [
            CloseState::Closed,
            CloseState::Refused { revivable: true },
            CloseState::Refused { revivable: false },
            CloseState::Abandoned,
        ] {
            seen.get_mut(&src).unwrap().close = settled;
            assert_eq!(sighting_for(&seen, src), Sighting::Settled, "{settled:?}");
        }
    }

    /// A terminal close answer settles the source: the next sighting sends
    /// nothing. A retryable one leaves it close-only. Settling a source this
    /// process never materialized records nothing.
    #[test]
    fn settle_close_settles_only_terminal_answers() {
        let sources = MaterializedSources::default();
        let tenant = TenantScope::Owned(Uuid::new_v4());
        let (closed, refused, retry) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        for src in [closed, refused, retry] {
            sources.mark(src, tenant);
        }
        let now = Instant::now();
        settle_close(
            &sources,
            closed,
            SourceClose::Closed {
                already_closed: false,
            },
            false,
            now,
        );
        settle_close(
            &sources,
            refused,
            SourceClose::Refused {
                status: 403,
                error: "not_handoff_target".into(),
            },
            true,
            now,
        );
        settle_close(
            &sources,
            retry,
            SourceClose::Retry(HandoffError::Status(503, String::new())),
            true,
            now,
        );
        assert_eq!(sources.sighting(closed), Sighting::Settled);
        assert_eq!(sources.sighting(refused), Sighting::Settled);
        assert_eq!(sources.sighting(retry), Sighting::CloseOnly(tenant));

        let never = Uuid::new_v4();
        assert_eq!(
            sources.record(
                never,
                &SourceClose::Closed {
                    already_closed: false
                },
                now
            ),
            Recorded::Untracked
        );
        assert_eq!(sources.sighting(never), Sighting::Materialize);
    }

    // =======================================================================
    // Phase 4 (plan `2026-10-10-remote-create-residuals-followups`): the source
    // is closed through coord's handoff-completion door, never the
    // operator-admin `DELETE /sessions/:id` that 401s a device.
    // =======================================================================

    #[test]
    fn close_response_200_is_closed_either_way() {
        assert!(matches!(
            classify_close_response(200, r#"{"closed":true,"already_closed":false}"#),
            SourceClose::Closed {
                already_closed: false
            }
        ));
        assert!(matches!(
            classify_close_response(200, r#"{"closed":true,"already_closed":true}"#),
            SourceClose::Closed {
                already_closed: true
            }
        ));
    }

    #[test]
    fn close_response_2xx_without_closed_true_is_retried() {
        assert!(matches!(
            classify_close_response(200, r#"{"closed":false}"#),
            SourceClose::Retry(HandoffError::Parse(_))
        ));
        assert!(matches!(
            classify_close_response(200, "not json"),
            SourceClose::Retry(HandoffError::Parse(_))
        ));
    }

    #[test]
    fn close_response_named_refusals_are_terminal() {
        match classify_close_response(404, r#"{"error":"session_not_found"}"#) {
            SourceClose::Refused { status, error } => {
                assert_eq!((status, error.as_str()), (404, "session_not_found"));
            }
            other => panic!("404 session_not_found must be terminal, got {other:?}"),
        }
        match classify_close_response(403, r#"{"error":"not_handoff_target"}"#) {
            SourceClose::Refused { status, error } => {
                assert_eq!((status, error.as_str()), (403, "not_handoff_target"));
            }
            other => panic!("403 not_handoff_target must be terminal, got {other:?}"),
        }
    }

    #[test]
    fn close_response_child_not_materialized_is_retried() {
        assert!(matches!(
            classify_close_response(403, r#"{"error":"handoff_child_not_materialized"}"#),
            SourceClose::Retry(HandoffError::Status(403, _))
        ));
    }

    /// Everything outside the named refusals retries: a 5xx, a 401 in a
    /// credential gap, and — load-bearing for rollout order — a bare 404 from a
    /// coord that predates the door, which must not abandon the source.
    #[test]
    fn close_response_outside_the_contract_is_retried() {
        for (status, body) in [
            (500, r#"{"error":"internal"}"#),
            (503, ""),
            (401, r#"{"error":"unauthorized"}"#),
            (404, ""),
            (404, r#"{"error":"not_found"}"#),
            (403, r#"{"error":"forbidden"}"#),
        ] {
            assert!(
                matches!(
                    classify_close_response(status, body),
                    SourceClose::Retry(HandoffError::Status(s, _)) if s == status
                ),
                "{status} {body:?} must be retryable"
            );
        }
    }

    /// One request recorded by the fake coord below.
    #[derive(Debug, Clone)]
    struct RecordedRequest {
        method: String,
        path: String,
        auth: Option<String>,
        body: String,
    }

    /// A fake coord that records every request and answers each with
    /// `(status, body)`.
    async fn spawn_fake_coord(
        status: u16,
        body: &'static str,
    ) -> (String, Arc<Mutex<Vec<RecordedRequest>>>) {
        use axum::{body::Bytes, http::HeaderMap, http::Method, http::Uri};
        let seen: Arc<Mutex<Vec<RecordedRequest>>> = Arc::default();
        let rec = seen.clone();
        let app = axum::Router::new().fallback(
            move |method: Method, uri: Uri, headers: HeaderMap, bytes: Bytes| {
                let rec = rec.clone();
                async move {
                    rec.lock().unwrap().push(RecordedRequest {
                        method: method.to_string(),
                        path: uri.path().to_string(),
                        auth: headers
                            .get("authorization")
                            .and_then(|v| v.to_str().ok())
                            .map(str::to_string),
                        body: String::from_utf8_lossy(&bytes).into_owned(),
                    });
                    (
                        axum::http::StatusCode::from_u16(status).unwrap(),
                        [("content-type", "application/json")],
                        body,
                    )
                }
            },
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (format!("http://{addr}"), seen)
    }

    /// An unexpired, unsigned device JWT claiming `tenant` — enough for the
    /// slot reader, which checks `exp` and never verifies a signature.
    fn device_jwt_for(tenant: &Uuid) -> String {
        let header = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(br#"{"alg":"none","typ":"JWT"}"#);
        let exp = chrono::Utc::now().timestamp() + 3 * 60 * 60;
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(format!(r#"{{"tenant_id":"{tenant}","exp":{exp}}}"#).as_bytes());
        format!("{header}.{payload}.sig")
    }

    /// `close_source` POSTs `{}` to `/sessions/:id/handoff/complete` with the
    /// SOURCE tenant's credential — on a device whose DEFAULT binding is a
    /// different tenant, so presenting the default slot would fail here — and
    /// never sends `DELETE /sessions/:id`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn close_source_posts_handoff_complete_with_the_source_tenant_slot() {
        let amb = crate::test_env::isolated_ambient();
        std::env::set_var("QONTINUI_DISABLE_KEYCHAIN", "1");
        amb.write_machine_json("{\"device_id\":\"fixture-device\"}");
        let storage = std::path::PathBuf::from(
            std::env::var("QONTINUI_SECURE_STORAGE_DIR")
                .expect("the ambient fixture pins the secure-storage dir"),
        );
        std::fs::create_dir_all(&storage).unwrap();
        let (default_tenant, source_tenant) = (Uuid::now_v7(), Uuid::now_v7());
        std::fs::write(
            storage.join("paired_user.json"),
            json!({
                "default_tenant_id": default_tenant,
                "bindings": [{ "tenant_id": default_tenant }, { "tenant_id": source_tenant }],
            })
            .to_string(),
        )
        .unwrap();
        let am = crate::auth::AuthManager::new();
        let default_jwt = device_jwt_for(&default_tenant);
        let source_jwt = device_jwt_for(&source_tenant);
        am.store_tenant_device_jwt(&default_tenant, &default_jwt)
            .expect("the default tenant's slot");
        am.store_tenant_device_jwt(&source_tenant, &source_jwt)
            .expect("the source tenant's slot");

        let (base, seen) = spawn_fake_coord(200, r#"{"closed":true,"already_closed":false}"#).await;
        let source = Uuid::new_v4();
        let outcome = close_source(
            &reqwest::Client::new(),
            &base,
            source,
            TenantScope::Owned(source_tenant),
        )
        .await;
        assert!(
            matches!(
                outcome,
                SourceClose::Closed {
                    already_closed: false
                }
            ),
            "{outcome:?}"
        );

        let seen = seen.lock().unwrap().clone();
        assert_eq!(seen.len(), 1, "exactly one request: {seen:?}");
        let req = &seen[0];
        assert_eq!(req.method, "POST");
        assert_eq!(req.path, format!("/sessions/{source}/handoff/complete"));
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&req.body).unwrap(),
            json!({}),
            "an empty JSON body"
        );
        assert_eq!(
            req.auth,
            Some(format!("Bearer {source_jwt}")),
            "the SOURCE tenant's slot, not the default binding's"
        );
        assert!(
            seen.iter().all(|r| r.method != "DELETE"),
            "the operator-admin DELETE /sessions/:id is never sent"
        );
    }

    /// The close outcome is classified off the wire, not just off a 2xx: a
    /// terminal refusal and a retryable one both come back as such.
    #[tokio::test]
    async fn close_source_classifies_coords_refusals() {
        let (base, _) = spawn_fake_coord(403, r#"{"error":"not_handoff_target"}"#).await;
        assert!(matches!(
            close_source(
                &reqwest::Client::new(),
                &base,
                Uuid::new_v4(),
                TenantScope::Device
            )
            .await,
            SourceClose::Refused { status: 403, .. }
        ));
        let (base, _) =
            spawn_fake_coord(403, r#"{"error":"handoff_child_not_materialized"}"#).await;
        assert!(matches!(
            close_source(
                &reqwest::Client::new(),
                &base,
                Uuid::new_v4(),
                TenantScope::Device
            )
            .await,
            SourceClose::Retry(HandoffError::Status(403, _))
        ));
        // Transport failure: nothing listens on a bound-then-dropped port.
        let port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        assert!(matches!(
            close_source(
                &reqwest::Client::new(),
                &format!("http://127.0.0.1:{port}"),
                Uuid::new_v4(),
                TenantScope::Device
            )
            .await,
            SourceClose::Retry(HandoffError::Http(_))
        ));
    }

    /// Source-level guard: the module's executable code never sends a DELETE.
    #[test]
    fn the_handoff_module_never_sends_a_delete() {
        let src = include_str!("handoff.rs");
        let production = src
            .split("#[cfg(test)]")
            .next()
            .expect("the module has a production half");
        let code: String = production
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        for forbidden in [".delete(", "DELETE"] {
            assert!(
                !code.contains(forbidden),
                "close_source must not DELETE, but the module's code contains {forbidden:?}"
            );
        }
        assert!(code.contains("/handoff/complete"));
    }

    // =======================================================================
    // Close retries are driven by `MaterializedSources`, not by re-sightings
    // (module doc, point 3).
    // =======================================================================

    /// A fake coord answering each request with the next scripted
    /// `(status, body)`; the last one repeats once the script runs out.
    async fn spawn_scripted_fake_coord(
        script: Vec<(u16, &'static str)>,
    ) -> (String, Arc<Mutex<Vec<RecordedRequest>>>) {
        use axum::{body::Bytes, http::HeaderMap, http::Method, http::Uri};
        assert!(!script.is_empty());
        let seen: Arc<Mutex<Vec<RecordedRequest>>> = Arc::default();
        let rec = seen.clone();
        let script = Arc::new(script);
        let app = axum::Router::new().fallback(
            move |method: Method, uri: Uri, headers: HeaderMap, bytes: Bytes| {
                let rec = rec.clone();
                let script = script.clone();
                async move {
                    let n = {
                        let mut rec = rec.lock().unwrap();
                        rec.push(RecordedRequest {
                            method: method.to_string(),
                            path: uri.path().to_string(),
                            auth: headers
                                .get("authorization")
                                .and_then(|v| v.to_str().ok())
                                .map(str::to_string),
                            body: String::from_utf8_lossy(&bytes).into_owned(),
                        });
                        rec.len() - 1
                    };
                    let (status, body) = script[n.min(script.len() - 1)];
                    (
                        axum::http::StatusCode::from_u16(status).unwrap(),
                        [("content-type", "application/json")],
                        body,
                    )
                }
            },
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (format!("http://{addr}"), seen)
    }

    /// The first close is refused `handoff_child_not_materialized` (the child
    /// row has not registered yet). Coord then drops the source from its
    /// pending list — nothing will ever sight it again — yet a later catch-up
    /// pass still re-sends the close from `MaterializedSources` and settles on
    /// coord's 200. Before the fix the retry was driven only by a re-sighting,
    /// so this close was never sent a second time.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_close_refused_before_the_child_registered_is_retried_without_a_resighting() {
        let (base, seen) = spawn_scripted_fake_coord(vec![
            (403, r#"{"error":"handoff_child_not_materialized"}"#),
            (200, r#"{"closed":true,"already_closed":false}"#),
        ])
        .await;
        let http = reqwest::Client::new();
        let sources = MaterializedSources::default();
        let source = Uuid::new_v4();
        let tenant = TenantScope::Device;
        sources.mark(source, tenant);

        // The first close (materialize's own): refused, rescheduled one tick out.
        let t0 = Instant::now();
        let close = close_source(&http, &base, source, tenant).await;
        settle_close(&sources, source, close, false, t0);
        assert_eq!(sources.sighting(source), Sighting::CloseOnly(tenant));

        // A pass before the backoff is up sends nothing.
        assert!(sources.due_closes(t0 + CATCHUP_TICK / 2).is_empty());
        // The next tick's pass — with coord no longer listing the source, so
        // the only driver is the set itself — re-sends and settles.
        let due = sources.due_closes(t0 + CATCHUP_TICK);
        assert_eq!(due, vec![(source, tenant)]);
        for (src, scope) in due {
            let close = close_source(&http, &base, src, scope).await;
            settle_close(&sources, src, close, true, t0 + CATCHUP_TICK);
        }
        assert_eq!(sources.sighting(source), Sighting::Settled);
        assert!(sources.due_closes(t0 + CLOSE_RETRY_LIFETIME * 2).is_empty());

        // And the production pass the catch-up runs sends nothing further.
        retry_due_closes(&http, &base, &sources).await;
        let seen = seen.lock().unwrap().clone();
        assert_eq!(seen.len(), 2, "one refused close, one retry: {seen:?}");
        for req in &seen {
            assert_eq!(
                (req.method.as_str(), req.path.clone()),
                ("POST", format!("/sessions/{source}/handoff/complete"))
            );
        }
    }

    /// `retry_due_closes` itself (the function every handoff catch-up pass
    /// ends with) re-sends a due close with no pending-list input at all.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_catchup_retry_pass_resends_a_due_close_it_was_never_shown() {
        let (base, seen) =
            spawn_scripted_fake_coord(vec![(200, r#"{"closed":true,"already_closed":true}"#)])
                .await;
        let sources = MaterializedSources::default();
        let source = Uuid::new_v4();
        sources.mark(source, TenantScope::Device);
        retry_due_closes(&reqwest::Client::new(), &base, &sources).await;
        assert_eq!(sources.sighting(source), Sighting::Settled);
        assert_eq!(seen.lock().unwrap().len(), 1);
    }

    /// Repeated retryable failures space attempts out — one tick, doubling,
    /// capped — and the source is abandoned once its failures span the
    /// lifetime bound, after which no pass sends anything.
    #[test]
    fn repeated_retryable_failures_back_off_and_are_eventually_abandoned() {
        assert_eq!(close_retry_backoff(1), CATCHUP_TICK);
        assert_eq!(close_retry_backoff(2), CATCHUP_TICK * 2);
        assert_eq!(close_retry_backoff(3), CATCHUP_TICK * 4);
        assert_eq!(close_retry_backoff(4), CATCHUP_TICK * 8);
        assert_eq!(close_retry_backoff(5), CLOSE_RETRY_CEIL);
        assert_eq!(close_retry_backoff(u32::MAX), CLOSE_RETRY_CEIL);

        let sources = MaterializedSources::default();
        let source = Uuid::new_v4();
        sources.mark(source, TenantScope::Device);
        let retry = || SourceClose::Retry(HandoffError::Status(503, String::new()));

        let t0 = Instant::now();
        let mut at = t0;
        let mut gaps = Vec::new();
        for n in 1..=6u32 {
            assert_eq!(sources.due_closes(at), vec![(source, TenantScope::Device)]);
            let recorded = sources.record(source, &retry(), at);
            let Recorded::Retry {
                failures,
                retry_in,
                first,
            } = recorded
            else {
                panic!("attempt {n}: {recorded:?}");
            };
            assert_eq!((failures, first), (n, n == 1));
            // Not due a tick before the backoff is up (outside the slack)…
            assert!(
                sources
                    .due_closes(at + retry_in - CLOSE_RETRY_DUE_SLACK - Duration::from_secs(1))
                    .is_empty(),
                "attempt {n} came due early"
            );
            gaps.push(retry_in);
            at += retry_in;
        }
        assert_eq!(
            gaps,
            vec![
                CATCHUP_TICK,
                CATCHUP_TICK * 2,
                CATCHUP_TICK * 4,
                CATCHUP_TICK * 8,
                CLOSE_RETRY_CEIL,
                CLOSE_RETRY_CEIL,
            ]
        );

        // Past the lifetime bound measured from the FIRST failure: abandoned.
        let late = t0 + CLOSE_RETRY_LIFETIME;
        assert!(matches!(
            sources.record(source, &retry(), late),
            Recorded::Abandoned { failures: 7 }
        ));
        assert_eq!(sources.sighting(source), Sighting::Settled);
        assert!(sources.due_closes(late + CLOSE_RETRY_CEIL * 10).is_empty());
    }

    /// `403 not_handoff_target` settles the source — until a FRESH push frame
    /// for that source addressed to this device (a re-handoff back here)
    /// revives it, after which the close is sent again and settles on 200. A
    /// 200-closed source stays settled on a later push frame, and so does a
    /// `404 session_not_found` refusal.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_fresh_handoff_request_revives_a_not_handoff_target_refusal() {
        let (base, seen) = spawn_scripted_fake_coord(vec![
            (403, r#"{"error":"not_handoff_target"}"#),
            (200, r#"{"closed":true,"already_closed":false}"#),
        ])
        .await;
        let http = reqwest::Client::new();
        let sources = MaterializedSources::default();
        let device = Uuid::new_v4();
        let tenant_id = Uuid::new_v4();
        let source = Uuid::new_v4();
        let tenant = TenantScope::Owned(tenant_id);
        sources.mark(source, tenant);

        let t0 = Instant::now();
        let close = close_source(&http, &base, source, tenant).await;
        settle_close(&sources, source, close, false, t0);
        assert_eq!(sources.sighting(source), Sighting::Settled);
        // A catch-up pass sends nothing for a refused source, however late.
        assert!(sources.due_closes(t0 + CLOSE_RETRY_CEIL).is_empty());

        // The re-handoff back here arrives as a push frame for this device.
        let frame = ws_envelope(
            &format!("qontinui.sessions.{tenant_id}.{device}.handoff_request"),
            handoff_payload(source, device, tenant_id, "terminal_shell"),
        );
        let fresh = parse_handoff_push(&frame, device).expect("a frame for this device");
        let t1 = Instant::now();
        assert!(sources.revive_on_fresh_request(fresh.source_session_id, t1));
        assert_eq!(sources.sighting(source), Sighting::CloseOnly(tenant));
        assert_eq!(sources.due(source, t1), Some(tenant));
        retry_due_closes(&http, &base, &sources).await;
        assert_eq!(sources.sighting(source), Sighting::Settled);
        assert_eq!(seen.lock().unwrap().len(), 2, "the close was re-attempted");

        // Now 200-closed: a further fresh frame leaves it settled.
        assert!(!sources.revive_on_fresh_request(source, Instant::now()));
        assert_eq!(sources.sighting(source), Sighting::Settled);
        retry_due_closes(&http, &base, &sources).await;
        assert_eq!(seen.lock().unwrap().len(), 2, "nothing more was sent");

        // `session_not_found` is terminal even against a fresh frame.
        let gone = Uuid::new_v4();
        sources.mark(gone, tenant);
        sources.record(
            gone,
            &SourceClose::Refused {
                status: 404,
                error: "session_not_found".into(),
            },
            t1,
        );
        assert!(!sources.revive_on_fresh_request(gone, Instant::now()));
        assert_eq!(sources.sighting(gone), Sighting::Settled);
    }

    /// A registry and lifecycle store that the close-only paths below accept
    /// but never use: no transport starts anything, and the coord-sync loops
    /// are never spawned. The returned dir must outlive both.
    fn inert_registry(
        coord_url: &str,
    ) -> (
        Arc<SessionRegistry>,
        Arc<SessionLifecycleStore>,
        tempfile::TempDir,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let outbox = Arc::new(
            crate::session::local_store::OutboxWriter::open(dir.path().join("outbox.jsonl"))
                .unwrap(),
        );
        let coord = crate::session::coord_sync::CoordSync::new_for_test(
            outbox,
            coord_url.to_string(),
            Duration::from_secs(3600),
            Duration::from_secs(3600),
        );
        let external: crate::session::DynTransport = Arc::new(crate::session::ExternalTransport);
        let registry = SessionRegistry::new(
            Uuid::new_v4(),
            crate::session::SessionTransports {
                pty: external.clone(),
                claude_cli: external.clone(),
                workflow: external,
            },
            coord,
        );
        let store = Arc::new(
            SessionLifecycleStore::open(dir.path().join("terminal-sessions.json")).unwrap(),
        );
        (registry, store, dir)
    }

    /// The handoff catch-up pass, through its real entry `run_catchup`: coord's
    /// pending-list GET fails (500), yet the pass still re-sends the due close
    /// of a source this process materialized — exactly once — and settles it
    /// on coord's 200. Fails if `run_catchup` stops ending with
    /// `retry_due_closes`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn run_catchup_resends_a_due_close_even_when_the_pending_list_get_fails() {
        let _amb = crate::test_env::isolated_ambient();
        std::env::set_var("QONTINUI_DISABLE_KEYCHAIN", "1");
        let (base, seen) = spawn_scripted_fake_coord(vec![
            (500, r#"{"error":"boom"}"#),
            (200, r#"{"closed":true,"already_closed":false}"#),
        ])
        .await;
        let (registry, store, _dir) = inert_registry(&base);
        let sources = MaterializedSources::default();
        let source = Uuid::new_v4();
        sources.mark(source, TenantScope::Device);

        run_catchup(
            &registry,
            &store,
            &reqwest::Client::new(),
            &base,
            Uuid::new_v4(),
            &sources,
        )
        .await;

        let seen = seen.lock().unwrap().clone();
        let closes: Vec<_> = seen
            .iter()
            .filter(|r| r.path == format!("/sessions/{source}/handoff/complete"))
            .collect();
        assert_eq!(closes.len(), 1, "exactly one close: {seen:?}");
        assert_eq!(closes[0].method, "POST");
        assert_eq!(
            seen[0].path, "/sessions/handoff-requests",
            "the pending-list GET ran first and failed: {seen:?}"
        );
        assert_eq!(sources.sighting(source), Sighting::Settled);
        assert!(sources
            .due_closes(Instant::now() + CLOSE_RETRY_CEIL)
            .is_empty());
    }

    /// A push frame, through its real entry `handle_push_frame`: a source whose
    /// close was refused `403 not_handoff_target` is settled, and a FRESH
    /// `handoff_request` frame for it addressed to this device revives it — the
    /// close is POSTed and settles Closed on coord's 200, with no second child.
    /// Fails if `handle_push_frame` stops calling `revive_on_fresh_request`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_push_frame_revives_a_not_handoff_target_refusal_and_closes_the_source() {
        let _amb = crate::test_env::isolated_ambient();
        std::env::set_var("QONTINUI_DISABLE_KEYCHAIN", "1");
        let (base, seen) = spawn_fake_coord(200, r#"{"closed":true,"already_closed":false}"#).await;
        let (registry, store, _dir) = inert_registry(&base);
        let sources = MaterializedSources::default();
        let (device, tenant_id, source) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let tenant = TenantScope::Owned(tenant_id);
        sources.mark(source, tenant);
        assert_eq!(
            sources.record(
                source,
                &SourceClose::Refused {
                    status: 403,
                    error: "not_handoff_target".into(),
                },
                Instant::now(),
            ),
            Recorded::Refused { revivable: true }
        );
        assert_eq!(sources.sighting(source), Sighting::Settled);

        let frame = ws_envelope(
            &format!("qontinui.sessions.{tenant_id}.{device}.handoff_request"),
            handoff_payload(source, device, tenant_id, "terminal_shell"),
        );
        handle_push_frame(
            &registry,
            &store,
            &reqwest::Client::new(),
            &base,
            device,
            &sources,
            &frame,
        )
        .await;

        let seen = seen.lock().unwrap().clone();
        assert_eq!(seen.len(), 1, "exactly one request, the close: {seen:?}");
        assert_eq!(
            (seen[0].method.as_str(), seen[0].path.clone()),
            ("POST", format!("/sessions/{source}/handoff/complete"))
        );
        assert_eq!(sources.sighting(source), Sighting::Settled);
        assert!(
            registry.snapshot().is_empty(),
            "a revived close never starts a second child"
        );
    }

    // -----------------------------------------------------------------------
    // Entry-point-only arming tests. Each one enters ONLY through its
    // population's entry point (`run_catchup` / `handle_push_frame`): state is
    // seeded by building `MaterializedSources` as a struct literal (no method of
    // it is called), and every assertion reads the fake coord's requests — a
    // second call of the same entry point that sends no further POST is what
    // "settled" means. Neither these tests nor the helpers below name any other
    // function this change touched, so they observe the change from outside.
    // -----------------------------------------------------------------------

    /// A fake coord answering each request with the next scripted
    /// `(status, body)` (the last repeats), recording into an `RwLock` — so
    /// reading what it saw needs no `Mutex` guard at all.
    async fn spawn_rw_fake_coord(
        script: Vec<(u16, &'static str)>,
    ) -> (String, Arc<std::sync::RwLock<Vec<RecordedRequest>>>) {
        use axum::{body::Bytes, http::HeaderMap, http::Method, http::Uri};
        assert!(!script.is_empty());
        let seen: Arc<std::sync::RwLock<Vec<RecordedRequest>>> = Arc::default();
        let rec = seen.clone();
        let script = Arc::new(script);
        let app = axum::Router::new().fallback(
            move |method: Method, uri: Uri, headers: HeaderMap, bytes: Bytes| {
                let rec = rec.clone();
                let script = script.clone();
                async move {
                    let n = {
                        let mut rec = rec.write().unwrap();
                        rec.push(RecordedRequest {
                            method: method.to_string(),
                            path: uri.path().to_string(),
                            auth: headers
                                .get("authorization")
                                .and_then(|v| v.to_str().ok())
                                .map(str::to_string),
                            body: String::from_utf8_lossy(&bytes).into_owned(),
                        });
                        rec.len() - 1
                    };
                    let (status, body) = script[n.min(script.len() - 1)];
                    (
                        axum::http::StatusCode::from_u16(status).unwrap(),
                        [("content-type", "application/json")],
                        body,
                    )
                }
            },
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (format!("http://{addr}"), seen)
    }

    /// Pair this (isolated) device with ONE tenant and store that tenant's
    /// device JWT in its slot; returns the tenant and the JWT a close under
    /// `TenantScope::Owned(tenant)` must present as its bearer.
    fn pair_one_tenant_slot(amb: &crate::test_env::IsolatedAmbient) -> (Uuid, String) {
        std::env::set_var("QONTINUI_DISABLE_KEYCHAIN", "1");
        amb.write_machine_json("{\"device_id\":\"fixture-device\"}");
        let storage = std::path::PathBuf::from(
            std::env::var("QONTINUI_SECURE_STORAGE_DIR")
                .expect("the ambient fixture pins the secure-storage dir"),
        );
        std::fs::create_dir_all(&storage).unwrap();
        let tenant_id = Uuid::now_v7();
        std::fs::write(
            storage.join("paired_user.json"),
            json!({
                "default_tenant_id": tenant_id,
                "bindings": [{ "tenant_id": tenant_id }],
            })
            .to_string(),
        )
        .unwrap();
        let jwt = device_jwt_for(&tenant_id);
        crate::auth::AuthManager::new()
            .store_tenant_device_jwt(&tenant_id, &jwt)
            .expect("the tenant's slot");
        (tenant_id, jwt)
    }

    /// The requests the fake coord saw that are closes of `source`.
    fn handoff_complete_posts(seen: &[RecordedRequest], source: Uuid) -> Vec<RecordedRequest> {
        let path = format!("/sessions/{source}/handoff/complete");
        seen.iter()
            .filter(|r| r.method == "POST" && r.path == path)
            .cloned()
            .collect()
    }

    /// `run_catchup` ALONE: one source this process started a child for, its
    /// close pending and due now (seeded as a literal). Coord answers the
    /// pending-list GET 500 and the close 200 `closed:true` — the pass still
    /// POSTs the close exactly once, with the source tenant's bearer and an
    /// empty JSON body; a second pass sends no further POST (settled). Fails if
    /// `run_catchup` stops ending with its retry pass.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn run_catchup_alone_retries_a_pending_close_and_settles_it() {
        let amb = crate::test_env::isolated_ambient();
        let (tenant_id, jwt) = pair_one_tenant_slot(&amb);
        let (base, seen) = spawn_rw_fake_coord(vec![
            (500, r#"{"error":"boom"}"#),
            (200, r#"{"closed":true,"already_closed":false}"#),
            (500, r#"{"error":"boom"}"#),
        ])
        .await;
        let (registry, store, _dir) = inert_registry(&base);
        let source = Uuid::new_v4();
        let sources = MaterializedSources(Mutex::new(HashMap::from([(
            source,
            MaterializedSource {
                tenant: TenantScope::Owned(tenant_id),
                close: CloseState::Pending {
                    failures: 0,
                    next_at: Instant::now(),
                    first_failure_at: None,
                },
            },
        )])));
        let http = reqwest::Client::new();
        let device = Uuid::new_v4();

        run_catchup(&registry, &store, &http, &base, device, &sources).await;

        let first = seen.read().unwrap().clone();
        let posts = handoff_complete_posts(&first, source);
        assert_eq!(posts.len(), 1, "exactly one close POST: {first:?}");
        assert_eq!(
            posts[0].auth,
            Some(format!("Bearer {jwt}")),
            "the source tenant's slot"
        );
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&posts[0].body).unwrap(),
            json!({}),
            "an empty JSON body"
        );
        assert_eq!(
            (first[0].method.as_str(), first[0].path.as_str()),
            ("GET", "/sessions/handoff-requests"),
            "the pending-list GET ran first (and failed): {first:?}"
        );
        assert!(first.iter().all(|r| r.method != "DELETE"), "{first:?}");

        run_catchup(&registry, &store, &http, &base, device, &sources).await;

        let second = seen.read().unwrap().clone();
        assert_eq!(
            handoff_complete_posts(&second, source).len(),
            1,
            "a settled close is never re-sent: {second:?}"
        );
        assert_eq!(
            second.len(),
            first.len() + 1,
            "the second pass sent only its pending-list GET: {second:?}"
        );
    }

    /// `handle_push_frame` ALONE: one source whose close was refused
    /// `403 not_handoff_target` (revivable; seeded as a literal). A FRESH
    /// `handoff_request` frame for it addressed to this device revives the
    /// close — exactly one POST, with the source tenant's bearer, and no child
    /// started. The same frame again, once coord answered 200, sends no POST
    /// (settled). Fails if `handle_push_frame` stops reviving on a fresh request.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_push_frame_alone_revives_a_refused_close_and_settles_it() {
        let amb = crate::test_env::isolated_ambient();
        let (tenant_id, jwt) = pair_one_tenant_slot(&amb);
        let (base, seen) =
            spawn_rw_fake_coord(vec![(200, r#"{"closed":true,"already_closed":false}"#)]).await;
        let (registry, store, _dir) = inert_registry(&base);
        let (device, source) = (Uuid::new_v4(), Uuid::new_v4());
        let sources = MaterializedSources(Mutex::new(HashMap::from([(
            source,
            MaterializedSource {
                tenant: TenantScope::Owned(tenant_id),
                close: CloseState::Refused { revivable: true },
            },
        )])));
        let http = reqwest::Client::new();
        let frame = ws_envelope(
            &format!("qontinui.sessions.{tenant_id}.{device}.handoff_request"),
            handoff_payload(source, device, tenant_id, "terminal_shell"),
        );

        handle_push_frame(&registry, &store, &http, &base, device, &sources, &frame).await;

        let first = seen.read().unwrap().clone();
        assert_eq!(first.len(), 1, "exactly one request, the close: {first:?}");
        let posts = handoff_complete_posts(&first, source);
        assert_eq!(posts.len(), 1, "{first:?}");
        assert_eq!(posts[0].auth, Some(format!("Bearer {jwt}")));
        assert!(
            registry.snapshot().is_empty(),
            "a revived close never starts a second child"
        );

        handle_push_frame(&registry, &store, &http, &base, device, &sources, &frame).await;

        let second = seen.read().unwrap().clone();
        assert_eq!(
            second.len(),
            1,
            "a closed source sends nothing on a repeat frame: {second:?}"
        );
        assert!(registry.snapshot().is_empty(), "still no child");
    }

    /// One retry pass sends at most `CLOSE_RETRY_PASS_CAP` closes, most overdue
    /// first; the rest stay due and the next pass sends them.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_retry_pass_sends_at_most_the_cap_and_leaves_the_rest_due() {
        let (base, seen) = spawn_fake_coord(200, r#"{"closed":true,"already_closed":false}"#).await;
        let sources = MaterializedSources::default();
        let total = CLOSE_RETRY_PASS_CAP + 3;
        for _ in 0..total {
            sources.mark(Uuid::new_v4(), TenantScope::Device);
        }
        let http = reqwest::Client::new();
        retry_due_closes(&http, &base, &sources).await;
        assert_eq!(seen.lock().unwrap().len(), CLOSE_RETRY_PASS_CAP);
        assert_eq!(sources.due_closes(Instant::now()).len(), 3);
        retry_due_closes(&http, &base, &sources).await;
        assert_eq!(seen.lock().unwrap().len(), total);
        assert!(sources.due_closes(Instant::now()).is_empty());
    }

    /// Due closes come back most overdue first.
    #[test]
    fn due_closes_are_ordered_most_overdue_first() {
        let sources = MaterializedSources::default();
        let (early, late) = (Uuid::new_v4(), Uuid::new_v4());
        let t0 = Instant::now();
        sources.mark(late, TenantScope::Device);
        sources.mark(early, TenantScope::Device);
        let retry = || SourceClose::Retry(HandoffError::Status(503, String::new()));
        sources.record(early, &retry(), t0);
        sources.record(late, &retry(), t0 + Duration::from_secs(30));
        let at = t0 + CATCHUP_TICK + Duration::from_secs(30);
        assert_eq!(
            sources.due_closes(at),
            vec![(early, TenantScope::Device), (late, TenantScope::Device)]
        );
    }

    /// A retryable answer that arrives after the entry settled — Closed,
    /// either kind of Refused, Abandoned — is ignored: it never turns a settled
    /// entry back into a pending one.
    #[test]
    fn a_late_retryable_answer_never_reopens_a_settled_close() {
        let sources = MaterializedSources::default();
        let now = Instant::now();
        let retry = || SourceClose::Retry(HandoffError::Status(503, String::new()));
        let settle: [(SourceClose, Recorded); 3] = [
            (
                SourceClose::Closed {
                    already_closed: false,
                },
                Recorded::Closed,
            ),
            (
                SourceClose::Refused {
                    status: 403,
                    error: "not_handoff_target".into(),
                },
                Recorded::Refused { revivable: true },
            ),
            (
                SourceClose::Refused {
                    status: 404,
                    error: "session_not_found".into(),
                },
                Recorded::Refused { revivable: false },
            ),
        ];
        for (answer, expected) in settle {
            let source = Uuid::new_v4();
            sources.mark(source, TenantScope::Device);
            assert_eq!(sources.record(source, &answer, now), expected);
            assert_eq!(
                sources.record(source, &retry(), now),
                Recorded::AlreadySettled
            );
            assert_eq!(sources.sighting(source), Sighting::Settled);
            assert!(sources.due(source, now + CLOSE_RETRY_CEIL).is_none());
        }
        // Abandoned, too.
        let source = Uuid::new_v4();
        sources.mark(source, TenantScope::Device);
        sources.record(source, &retry(), now);
        assert!(matches!(
            sources.record(source, &retry(), now + CLOSE_RETRY_LIFETIME),
            Recorded::Abandoned { .. }
        ));
        assert_eq!(
            sources.record(source, &retry(), now + CLOSE_RETRY_LIFETIME),
            Recorded::AlreadySettled
        );
        assert_eq!(sources.sighting(source), Sighting::Settled);
    }

    /// A (re)connect missed every push, so it replays EVERY arm.
    #[test]
    fn on_connect_replays_every_catchup_arm() {
        assert_eq!(
            catchups_for(CatchupPass::OnConnect),
            &[
                CatchupKind::Handoff,
                CatchupKind::Respawn,
                CatchupKind::Attach,
                CatchupKind::Create,
            ]
        );
    }

    /// The tick drives ONLY the arms nothing else drives. `attach` and
    /// `create` each have their own poll task in `main.rs`, each at least as
    /// frequent as this tick, so adding them here is a doubled GET, not a
    /// second backstop. This test fails the moment someone "restores symmetry"
    /// between the two passes.
    #[test]
    fn the_tick_drives_only_the_arms_with_no_other_periodic_owner() {
        let ticked = catchups_for(CatchupPass::Tick);
        assert_eq!(ticked, &[CatchupKind::Handoff, CatchupKind::Respawn]);
        for owned_elsewhere in [CatchupKind::Attach, CatchupKind::Create] {
            assert!(
                !ticked.contains(&owned_elsewhere),
                "{owned_elsewhere:?} already has its own poll task; the tick must not double it"
            );
        }
    }

    /// The doubling this split removes needs only that each polled arm runs at
    /// least as often as this tick — NOT that the periods coincide, which is
    /// what this test used to assert. Attach's no longer does: it is sized
    /// against the source's `ATTACH_TIMEOUT`, because a grant a dropped push
    /// lost must be recorded inside the source's own attach budget and every
    /// source retry mints a fresh jti. Pinned so a change to either period is
    /// a decision taken here rather than a silent re-divergence.
    #[test]
    fn every_polled_arm_polls_at_least_as_often_as_the_tick() {
        use crate::mcp::remote_terminal::ATTACH_TIMEOUT;
        assert!(crate::session::attach::POLL_INTERVAL <= CATCHUP_TICK);
        assert!(crate::session::attach::POLL_INTERVAL < ATTACH_TIMEOUT);
        assert_eq!(crate::session::create::POLL_INTERVAL, CATCHUP_TICK);
    }

    /// Every arm the tick drives must be one `OnConnect` drives too —
    /// otherwise a reconnect would SKIP a replay the tick was covering.
    #[test]
    fn the_tick_arms_are_a_subset_of_the_on_connect_arms() {
        let on_connect = catchups_for(CatchupPass::OnConnect);
        for arm in catchups_for(CatchupPass::Tick) {
            assert!(on_connect.contains(arm), "{arm:?} missing from OnConnect");
        }
    }

    /// The tick arm's schedule, on a paused clock: the first tick is
    /// immediate (the caller consumes it because the on-connect catch-up
    /// just ran), the next does not fire before `CATCHUP_TICK`, and does
    /// fire at it.
    #[tokio::test(start_paused = true)]
    async fn catchup_tick_fires_immediately_once_then_every_catchup_tick() {
        let mut tick = catchup_interval();
        let start = tokio::time::Instant::now();

        // First tick: immediate.
        tick.tick().await;
        assert_eq!(tokio::time::Instant::now() - start, Duration::ZERO);

        // Not before the period.
        let early = tokio::time::timeout(CATCHUP_TICK - Duration::from_secs(1), tick.tick()).await;
        assert!(early.is_err(), "tick fired before CATCHUP_TICK");

        // At the period.
        tick.tick().await;
        assert_eq!(tokio::time::Instant::now() - start, CATCHUP_TICK);

        // And again one period later.
        tick.tick().await;
        assert_eq!(tokio::time::Instant::now() - start, CATCHUP_TICK * 2);
    }

    fn make_state(kind: &str) -> HandoffState {
        HandoffState {
            source_session_id: Uuid::nil(),
            tenant_id: Uuid::nil(),
            source_device_id: Uuid::nil(),
            session_kind: kind.to_string(),
            intent: json!({
                "kind": kind,
                "purpose": "fix the auth bug",
                "declared_paths": ["/repo/a", "/repo/b"],
                "share_output": true,
                "redact_secrets": false,
            }),
            repo: Some("qontinui-runner".to_string()),
            branch: Some("main".to_string()),
            held_claims: vec![HeldClaim {
                kind: "session".to_string(),
                resource_key: "session:t:m:s".to_string(),
            }],
            output_chunks: vec![OutputChunk {
                chunk_offset: 0,
                payload_b64: base64::engine::general_purpose::STANDARD.encode(b"$ ls\n"),
            }],
        }
    }

    #[test]
    fn build_child_intent_threads_cwd_and_purpose() {
        let state = make_state("terminal_shell");
        let intent = build_child_intent(&state, HANDOFF_CONTINUATION_NOTE).unwrap();
        assert_eq!(intent.kind, SessionKind::TerminalShell);
        assert_eq!(intent.repo.as_deref(), Some("qontinui-runner"));
        assert_eq!(intent.branch.as_deref(), Some("main"));
        assert_eq!(intent.declared_paths.len(), 2);
        assert!(intent.purpose.contains("fix the auth bug"));
        assert!(intent.purpose.contains("continued here"));
        assert!(intent.share_output);
        assert_eq!(intent.redact_secrets, Some(false));
        // Built intent must pass validation so start_with_parent accepts it.
        intent.validate().unwrap();
    }

    #[test]
    fn build_child_intent_defaults_sparse_source() {
        let state = HandoffState {
            source_session_id: Uuid::nil(),
            tenant_id: Uuid::nil(),
            source_device_id: Uuid::nil(),
            session_kind: "terminal_claude".to_string(),
            intent: json!({}),
            repo: None,
            branch: None,
            held_claims: vec![],
            output_chunks: vec![],
        };
        let intent = build_child_intent(&state, HANDOFF_CONTINUATION_NOTE).unwrap();
        assert_eq!(intent.kind, SessionKind::TerminalClaude);
        assert!(intent.purpose.contains("handoff session"));
        assert!(intent.declared_paths.is_empty());
        // Ship-on-by-default (plan
        // 2026-09-22-transcript-sync-default-on-with-tenant-and-user-controls
        // §3.5): a source intent that omits `share_output` entirely must
        // resolve to `true` here too, matching `Intent::share_output`'s own
        // serde default — this call site reads the raw JSON directly and so
        // needed its own fallback fixed in step with that one.
        assert!(intent.share_output);
        intent.validate().unwrap();
    }

    /// `make_state` with an extra key spliced into the source intent JSON,
    /// so the dual-read tests below differ ONLY in which slug key they carry.
    fn state_with_intent_key(key: &str, value: &str) -> HandoffState {
        let mut state = make_state("terminal_shell");
        state
            .intent
            .as_object_mut()
            .unwrap()
            .insert(key.to_string(), json!(value));
        state
    }

    #[test]
    fn build_child_intent_reads_legacy_plan_slug_key() {
        // Un-renamed coord (every coord shipping today) writes `plan_slug`.
        let state = state_with_intent_key("plan_slug", "2026-07-28-some-unit");
        let intent = build_child_intent(&state, HANDOFF_CONTINUATION_NOTE).unwrap();
        assert_eq!(
            intent.work_unit_slug.as_deref(),
            Some("2026-07-28-some-unit")
        );
        // …and the child re-emits it under the CANONICAL key alone: the
        // legacy blob is folded forward, never propagated.
        assert_eq!(
            intent.plan_slug, None,
            "the deprecated field is accept-only; the child must not re-emit it"
        );
        let json = serde_json::to_value(&intent).unwrap();
        assert_eq!(
            json.get("work_unit_slug").and_then(|v| v.as_str()),
            Some("2026-07-28-some-unit")
        );
        assert!(json.get("plan_slug").is_none(), "{json}");
    }

    #[test]
    fn build_child_intent_reads_new_work_unit_slug_key() {
        // Post-rename coord writes `work_unit_slug`.
        let state = state_with_intent_key("work_unit_slug", "2026-07-28-some-unit");
        let intent = build_child_intent(&state, HANDOFF_CONTINUATION_NOTE).unwrap();
        assert_eq!(
            intent.work_unit_slug.as_deref(),
            Some("2026-07-28-some-unit")
        );
    }

    #[test]
    fn build_child_intent_prefers_work_unit_slug_over_plan_slug() {
        // Both present (a coord mid-rename): the NEW key wins.
        let mut state = state_with_intent_key("plan_slug", "old-name");
        state
            .intent
            .as_object_mut()
            .unwrap()
            .insert("work_unit_slug".to_string(), json!("new-name"));
        let intent = build_child_intent(&state, HANDOFF_CONTINUATION_NOTE).unwrap();
        assert_eq!(intent.work_unit_slug.as_deref(), Some("new-name"));
    }

    #[test]
    fn build_child_intent_absent_slug_is_none() {
        let intent =
            build_child_intent(&make_state("terminal_shell"), HANDOFF_CONTINUATION_NOTE).unwrap();
        assert_eq!(intent.work_unit_slug, None);
    }

    #[test]
    fn build_child_intent_falls_back_when_new_key_is_explicitly_null() {
        // Present-but-null is NOT the same as absent: `.get()` yields
        // `Some(Value::Null)`, so a naive `.or_else(get).and_then(as_str)`
        // chain would see "new key present", get `None` from `as_str`, and
        // silently drop the slug. Coord emits this exact shape — its
        // autonomous-dispatch metadata uses
        // `json!({"plan_slug": slug, "work_unit_slug": slug})`, which writes
        // `null` for a `None` rather than omitting the key.
        let mut state = make_state("terminal_shell");
        let obj = state.intent.as_object_mut().unwrap();
        obj.insert("work_unit_slug".to_string(), serde_json::Value::Null);
        obj.insert("plan_slug".to_string(), json!("2026-07-28-some-unit"));
        let intent = build_child_intent(&state, HANDOFF_CONTINUATION_NOTE).unwrap();
        assert_eq!(
            intent.work_unit_slug.as_deref(),
            Some("2026-07-28-some-unit"),
            "an explicit null on the new key must fall back to the legacy key"
        );
    }

    #[test]
    fn build_child_intent_both_keys_null_is_none() {
        let mut state = make_state("terminal_shell");
        let obj = state.intent.as_object_mut().unwrap();
        obj.insert("work_unit_slug".to_string(), serde_json::Value::Null);
        obj.insert("plan_slug".to_string(), serde_json::Value::Null);
        let intent = build_child_intent(&state, HANDOFF_CONTINUATION_NOTE).unwrap();
        assert_eq!(intent.work_unit_slug, None);
    }

    #[test]
    fn build_child_intent_rejects_unknown_kind() {
        let state = make_state("nonsense_kind");
        let err = build_child_intent(&state, HANDOFF_CONTINUATION_NOTE).unwrap_err();
        assert!(matches!(err, HandoffError::Parse(_)));
    }

    #[test]
    fn pending_handoff_deserializes() {
        let v = json!({
            "source_session_id": Uuid::nil(),
            "target_device_id": Uuid::nil(),
            "tenant_id": Uuid::nil(),
            "session_kind": "agentic",
        });
        let p: PendingHandoff = serde_json::from_value(v).unwrap();
        assert_eq!(p.session_kind, "agentic");
    }

    #[test]
    fn handoff_list_response_defaults_empty() {
        // Coord may return only {count: 0} on an empty poll; the
        // default-empty serde attr keeps that from being a parse error.
        let v = json!({ "count": 0 });
        let r: HandoffListResponse = serde_json::from_value(v).unwrap();
        assert!(r.handoffs.is_empty());
    }

    #[test]
    fn output_chunk_round_trips_base64() {
        let encoded = base64::engine::general_purpose::STANDARD.encode(b"hello world");
        let v = json!({ "chunk_offset": 3, "payload_b64": encoded });
        let c: OutputChunk = serde_json::from_value(v).unwrap();
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(&c.payload_b64)
            .unwrap();
        assert_eq!(decoded, b"hello world");
    }

    // -----------------------------------------------------------------------
    // Push-transport tests (WebSocket-relay path — Phase 7 rework)
    // -----------------------------------------------------------------------

    /// Build a coord `/ws` envelope as the Redis pub/sub arm produces it:
    /// `{"channel": "<subject>", "payload": "<json-string>"}`.
    fn ws_envelope(channel: &str, payload: serde_json::Value) -> String {
        json!({
            "channel": channel,
            "payload": payload.to_string(),
        })
        .to_string()
    }

    fn handoff_payload(source: Uuid, target: Uuid, tenant: Uuid, kind: &str) -> serde_json::Value {
        json!({
            "event_kind": "handoff_request",
            "source_session_id": source,
            "target_device_id": target,
            "tenant_id": tenant,
            "session_kind": kind,
        })
    }

    #[test]
    fn coord_ws_url_swaps_scheme_and_subscribes_as_sessions() {
        assert_eq!(
            coord_ws_url("http://localhost:9870"),
            "ws://localhost:9870/ws?subscribe=sessions"
        );
        assert_eq!(
            coord_ws_url("https://coord.qontinui.io/"),
            "wss://coord.qontinui.io/ws?subscribe=sessions"
        );
    }

    #[test]
    fn parse_handoff_push_accepts_frame_for_this_device() {
        let device = Uuid::new_v4();
        let tenant = Uuid::new_v4();
        let source = Uuid::new_v4();
        let channel = format!("qontinui.sessions.{tenant}.{device}.handoff_request");
        let frame = ws_envelope(&channel, handoff_payload(source, device, tenant, "agentic"));

        let parsed = parse_handoff_push(&frame, device).expect("frame for this device parses");
        assert_eq!(parsed.source_session_id, source);
        assert_eq!(parsed.target_device_id, device);
        assert_eq!(parsed.tenant_id, tenant);
        assert_eq!(parsed.session_kind, "agentic");
    }

    #[test]
    fn parse_handoff_push_ignores_other_devices() {
        let device = Uuid::new_v4();
        let other = Uuid::new_v4();
        let tenant = Uuid::new_v4();
        let source = Uuid::new_v4();
        // Subject addressed to `other`, not us.
        let channel = format!("qontinui.sessions.{tenant}.{other}.handoff_request");
        let frame = ws_envelope(&channel, handoff_payload(source, other, tenant, "agentic"));
        assert!(parse_handoff_push(&frame, device).is_none());
    }

    #[test]
    fn parse_handoff_push_ignores_other_event_kinds() {
        let device = Uuid::new_v4();
        let tenant = Uuid::new_v4();
        // A `started`/`heartbeat` subject for this device must not trigger
        // a handoff materialization.
        let channel = format!("qontinui.sessions.{tenant}.{device}.started");
        let frame = ws_envelope(&channel, json!({"event_kind": "started"}));
        assert!(parse_handoff_push(&frame, device).is_none());
    }

    #[test]
    fn parse_handoff_push_ignores_non_session_subjects() {
        let device = Uuid::new_v4();
        // The broader `events.*` family `agent_runtime` consumes must not
        // be mistaken for a handoff even if it somehow reaches this socket.
        let channel = format!("events.agent.spawn_requested.{device}");
        let frame = ws_envelope(&channel, json!({"agent_id": Uuid::nil()}));
        assert!(parse_handoff_push(&frame, device).is_none());
    }

    #[test]
    fn parse_handoff_push_accepts_inlined_payload_object() {
        // Defensive: some envelopes inline the payload as an object rather
        // than a JSON string. The parser must handle both.
        let device = Uuid::new_v4();
        let tenant = Uuid::new_v4();
        let source = Uuid::new_v4();
        let channel = format!("qontinui.sessions.{tenant}.{device}.handoff_request");
        let envelope = json!({
            "channel": channel,
            "payload": handoff_payload(source, device, tenant, "workflow"),
        })
        .to_string();
        let parsed = parse_handoff_push(&envelope, device).expect("inlined payload parses");
        assert_eq!(parsed.source_session_id, source);
        assert_eq!(parsed.session_kind, "workflow");
    }

    #[test]
    fn parse_handoff_push_rejects_payload_target_mismatch() {
        // Channel says us, but the payload's target_device_id disagrees —
        // trust the address (channel), reject the frame.
        let device = Uuid::new_v4();
        let other = Uuid::new_v4();
        let tenant = Uuid::new_v4();
        let source = Uuid::new_v4();
        let channel = format!("qontinui.sessions.{tenant}.{device}.handoff_request");
        let frame = ws_envelope(&channel, handoff_payload(source, other, tenant, "agentic"));
        assert!(parse_handoff_push(&frame, device).is_none());
    }

    #[test]
    fn parse_handoff_push_ignores_garbage() {
        let device = Uuid::new_v4();
        assert!(parse_handoff_push("not json", device).is_none());
        assert!(parse_handoff_push("{}", device).is_none());
    }

    // -----------------------------------------------------------------------
    // Phase 4 — restore-registry materialization (plan
    // `2026-07-09-runner-session-history-cloud-sync` §3.4)
    // -----------------------------------------------------------------------

    fn restore_payload(tier: &str, authoritative: Option<&str>) -> serde_json::Value {
        json!({
            "provider": "claude",
            "authoritative_session_id": authoritative,
            "cwd": "C:/repo",
            "launch_command": match authoritative {
                Some(id) => format!("claude --resume {id}"),
                None => "claude".to_string(),
            },
            "restore_tier": tier,
            "machine_id": Uuid::new_v4(),
        })
    }

    /// A `full`-tier payload materializes an AUTHORITATIVE + CONFIRMED
    /// record keyed by the authoritative id — exactly the shape the
    /// existing restore classifier auto-resumes.
    #[test]
    fn restore_payload_full_tier_materializes_confirmed_authoritative_record() {
        let rec = registry_record_from_restore_payload(
            &restore_payload(TIER_FULL, Some("11111111-2222-3333-4444-555555555555")),
            "term-child",
            Uuid::new_v4(),
        );
        assert_eq!(
            rec.claude_session_id,
            "11111111-2222-3333-4444-555555555555"
        );
        assert_eq!(rec.origin.as_deref(), Some(ORIGIN_AUTHORITATIVE));
        assert!(
            rec.confirmed_at.is_some(),
            "full tier ⇒ confirmed ⇒ classifyRestoreAction auto-resumes"
        );
        assert_eq!(rec.provider, "claude");
        assert_eq!(rec.working_dir.as_deref(), Some("C:/repo"));
        assert_eq!(rec.terminal_id, "term-child");
        assert_eq!(rec.state, "open");
    }

    /// A peer's `full` payload whose `authoritative_session_id` fails the
    /// shell-safety gate materializes EXACTLY like a `terminal_only` payload:
    /// provisional (no `confirmed_at`), under the per-source key — never a
    /// confirmed authoritative record carrying the unsafe id. A peer on a build
    /// predating the emitter's gate can still send this shape.
    #[test]
    fn restore_payload_full_with_unsafe_id_materializes_like_terminal_only() {
        let source = Uuid::new_v4();
        let terminal_only = registry_record_from_restore_payload(
            &restore_payload(TIER_TERMINAL_ONLY, None),
            "term-child",
            source,
        );
        for bad in ["abc; rm -rf /", "$(id)", "abc\n", "a b", "abc|tee x"] {
            let rec = registry_record_from_restore_payload(
                &restore_payload(TIER_FULL, Some(bad)),
                "term-child",
                source,
            );
            assert!(
                rec.confirmed_at.is_none(),
                "unsafe id {bad:?} was confirmed"
            );
            assert_ne!(
                rec.claude_session_id,
                bad.trim(),
                "unsafe id {bad:?} became the key"
            );
            assert_eq!(
                rec.claude_session_id, terminal_only.claude_session_id,
                "unsafe id {bad:?} must take the terminal_only key"
            );
        }
        // Positive control: the same payload with a safe id IS confirmed under it.
        let ok = registry_record_from_restore_payload(
            &restore_payload(TIER_FULL, Some("11111111-2222-3333-4444-555555555555")),
            "term-child",
            source,
        );
        assert_eq!(ok.claude_session_id, "11111111-2222-3333-4444-555555555555");
        assert!(ok.confirmed_at.is_some());
    }

    /// The tier is parsed as the WIRE vocabulary: the frontend's hyphenated
    /// `terminal-only`, or any unknown spelling, is not `full`.
    #[test]
    fn restore_payload_tier_parses_only_the_wire_vocabulary() {
        let source = Uuid::new_v4();
        for tier in ["terminal-only", "FULL", "garbage"] {
            let rec = registry_record_from_restore_payload(
                &restore_payload(tier, Some("sess-full-1")),
                "term-child",
                source,
            );
            assert!(rec.confirmed_at.is_none(), "tier {tier:?} was read as full");
        }
    }

    /// A `terminal_only` payload materializes a PROVISIONAL authoritative
    /// record under a deterministic per-source key — the classifier's
    /// phantom-shell branch restores terminal+cwd with an honest fresh
    /// conversation, never typing a resume against a null id, and a
    /// materialization retry upserts the SAME record (F7 idempotency).
    #[test]
    fn restore_payload_terminal_only_materializes_provisional_record() {
        let source = Uuid::new_v4();
        let rec = registry_record_from_restore_payload(
            &restore_payload("terminal_only", None),
            "term-child",
            source,
        );
        assert!(
            Uuid::parse_str(&rec.claude_session_id).is_ok(),
            "terminal_only key is a real uuid, got {}",
            rec.claude_session_id
        );
        assert_eq!(rec.origin.as_deref(), Some(ORIGIN_AUTHORITATIVE));
        assert!(
            rec.confirmed_at.is_none(),
            "terminal_only ⇒ provisional ⇒ classifyRestoreAction restores terminal-only"
        );
        assert_eq!(rec.working_dir.as_deref(), Some("C:/repo"));

        // Deterministic: same source ⇒ same key (retry idempotency);
        // different source ⇒ different key (no cross-session collision).
        let retry = registry_record_from_restore_payload(
            &restore_payload("terminal_only", None),
            "term-child",
            source,
        );
        assert_eq!(rec.claude_session_id, retry.claude_session_id);
        let other = registry_record_from_restore_payload(
            &restore_payload("terminal_only", None),
            "term-child",
            Uuid::new_v4(),
        );
        assert_ne!(rec.claude_session_id, other.claude_session_id);
    }

    /// Honest degrade: a `full` claim WITHOUT an authoritative id cannot
    /// resume anything — it materializes as terminal-only (provisional,
    /// minted key), never a confirmed record with a fabricated id.
    #[test]
    fn restore_payload_full_without_id_degrades_to_terminal_only() {
        let rec = registry_record_from_restore_payload(
            &restore_payload(TIER_FULL, None),
            "term-child",
            Uuid::new_v4(),
        );
        assert!(Uuid::parse_str(&rec.claude_session_id).is_ok());
        assert!(rec.confirmed_at.is_none());
    }

    /// Missing/blank provider defaults to claude (the pre-provider-aware
    /// registry default), so a sparse mirror still restores.
    #[test]
    fn restore_payload_defaults_provider() {
        let rec = registry_record_from_restore_payload(
            &json!({"restore_tier": "terminal_only"}),
            "term-child",
            Uuid::new_v4(),
        );
        assert_eq!(rec.provider, DEFAULT_PROVIDER);
        assert!(rec.working_dir.is_none());
    }

    /// End-to-end into the real registry: `record_open` on the mapped
    /// record lands a RESTORABLE row with the tier-honest fields intact.
    #[test]
    fn materialized_record_feeds_the_existing_registry_restorably() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionLifecycleStore::open(dir.path().join("terminal-sessions.json")).unwrap();
        let rec = registry_record_from_restore_payload(
            &restore_payload(TIER_FULL, Some("sess-full-1")),
            "term-child",
            Uuid::new_v4(),
        );
        store.record_open(rec);

        let restorable =
            store.restorable_records(chrono::Utc::now().timestamp_millis(), None, true, None);
        assert_eq!(restorable.len(), 1, "materialized record is restorable");
        let r = &restorable[0];
        assert_eq!(r.claude_session_id, "sess-full-1");
        assert_eq!(r.origin.as_deref(), Some(ORIGIN_AUTHORITATIVE));
        assert!(r.confirmed_at.is_some());
        assert!(r.opened_at > 0, "record_open stamped real timestamps");
    }

    /// The SSE-buffer parser picks the NEWEST (highest-seq) restore-record
    /// row out of the replay window, ignoring other event kinds, non-JSON
    /// noise, and payload-less rows.
    #[test]
    fn latest_restore_record_from_sse_picks_newest_and_ignores_noise() {
        let old = json!({
            "id": 1, "session_id": Uuid::nil(), "seq": 3,
            "event_kind": "restore-record",
            "payload": {"restore_tier": "terminal_only", "provider": "claude"},
        });
        let newest = json!({
            "id": 2, "session_id": Uuid::nil(), "seq": 7,
            "event_kind": "restore-record",
            "payload": {"restore_tier": "full", "provider": "claude",
                         "authoritative_session_id": "sess-9"},
        });
        let other_kind = json!({
            "id": 3, "session_id": Uuid::nil(), "seq": 9,
            "event_kind": "handoff_request",
            "payload": {"target_device_id": Uuid::nil()},
        });
        let buf = format!(
            "event: replay\ndata: {old}\n\nevent: replay\ndata: {newest}\n\n\
             event: replay\ndata: {other_kind}\n\nevent: live\ndata: not json\n\n"
        );
        let payload = latest_restore_record_from_sse(&buf).expect("newest restore-record");
        assert_eq!(payload["restore_tier"], "full");
        assert_eq!(payload["authoritative_session_id"], "sess-9");

        // Empty / noise-only buffers yield nothing.
        assert!(latest_restore_record_from_sse("").is_none());
        assert!(latest_restore_record_from_sse("event: replay\ndata: {}\n\n").is_none());
    }

    #[test]
    fn only_a_paused_to_allowed_transition_replays_the_deferred_catch_up() {
        assert!(super::resumed_after_drain(false, true));
        assert!(!super::resumed_after_drain(true, true));
        assert!(!super::resumed_after_drain(true, false));
        assert!(!super::resumed_after_drain(false, false));
    }
}
