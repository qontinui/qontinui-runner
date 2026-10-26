//! The prepare action, its cancel, the rebuilt runner's release, the account
//! freeze and the runner-local spawn hold (plan
//! `2026-10-21-operator-prepare-for-restart-and-progressive-restore`, Phase 3;
//! D1 and D3).
//!
//! ## The hold (D1)
//!
//! * **Primary of an enrolled device**: `POST /coord/devices/me/hold` under the
//!   device JWT (the same client `coord_drain_state` reads `me/drain` with),
//!   then read the device's own drain back. The hold is `confirmed` only when
//!   that read says `state: drained` AND its `reason` starts with
//!   `restart-hold:<prepare_id>:` — coord's read-back cannot show `drained_by`,
//!   so the reason carries the prepare id. Anything else is `unconfirmed(<cause>)`:
//!   coord unreachable, a refusal, a `404` from a coord that predates the route
//!   (`route_absent`, never success), a read-back that does not name this
//!   prepare. The plan expected the runner's drain state to fail closed by
//!   itself there; that holds only when coord stays unreachable for three
//!   heartbeats, so an unconfirmed hold is also BACKSTOPPED by the runner-local
//!   hold ([`holds_locally`]) — otherwise a refused hold, or a coord without
//!   the route, would leave `me/drain` reading `clear` and autonomous spawns
//!   running through the prepare.
//! * **`409 lane_held_by_other`** — the agent lane is already drained by
//!   another owner (an operator, or an earlier prepare). The device IS held,
//!   and `coord_drain_state` sees that drain and defers autonomous spawns by
//!   itself; but the hold is not THIS prepare's, D1 defines "confirmed" as the
//!   read-back naming this prepare, and this prepare's release will (rightly)
//!   answer `not_owner` and leave it standing. So it is recorded as
//!   `unconfirmed(lane_held_by_other)` — the honest reading — and the verdict
//!   phase decides what an other-owner hold means for "ready". The same holds
//!   for coord's other `409` codes (`lane_unreadable`, `hold_expired`), each
//!   recorded under its own code.
//! * **Not a coord device** (`NotEnrolled`): there is no coord lever to split,
//!   so the hold is runner-local and gates `drain_gate` — `local`.
//! * **A secondary instance** never writes the coord hold: the drain is keyed
//!   on the machine, which the primary shares, so a secondary's prepare would
//!   drain the primary. It takes the runner-local hold over its OWN
//!   `drain_gate` instead — `local (secondary)`.
//!
//! The hold has an `until` (default 6 h; coord's answer wins once it gives
//! one): the backstop for a rebuild that never happens. Past it, the record
//! reads "hold expired at …" and neither the local hold nor the freeze applies.
//!
//! ## Cancel and the rebuilt runner's boot
//!
//! Cancel sends `POST /coord/devices/me/release {prepare_id}` for a coord hold
//! and records coord's per-lane outcome VERBATIM (`released | not_owner |
//! absent | unreadable`), then marks the record `cancelled`, which lifts the
//! freeze and any local hold. A release coord did not answer with a lane
//! outcome (unreachable, refused) does NOT settle: the hold still stands, so
//! the prepare stays in force, the attempt is recorded (`release_failed` in the
//! view), and Release hold retries it. A coord without the self-hold route
//! (`route_absent`) is the exception while it never confirmed a hold for this
//! prepare: no coord hold of ours exists, so the release settles — with no
//! coord write at all when the hold itself answered `route_absent`
//! ([`release_settled`]).
//!
//! At boot, after the drain read and before any restore ([`on_boot`] runs
//! inside `coord_drain_state::boot_read`), a record whose `prepared_build` is
//! not this binary's `RUNNER_BUILD_ID` is the rebuild the hold waited for: the
//! hold is released the same way and the record moves to `restoring`. On the
//! SAME build the hold stays and [`same_build_restart`] goes true, which the
//! page shows as "Restarted on the same build. Spawns still held" with Release
//! hold (the cancel path).
//!
//! ## The account freeze (D3)
//!
//! While a prepare is active, a CONFIRMED usage-limit hit (and the chat path's
//! rate-limit restart) does not move the session to another account:
//! [`freeze_hit`] marks the cohort row `limit_hit` with the account's reset
//! time where the usage probe had one, and the caller returns. Keyed on the
//! local prepare record, never on the drain in general — an ordinary drain
//! leaves the migration's deliberate `exempt` classing alone.
//!
//! Everything that talks to coord goes through [`HoldDoor`], so the controller
//! is tested against an injected door and never against real coord: this box's
//! device holding its own agent lane would drain the primary runner.

use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use super::cohort::{Cohort, StateAtPrepare};
use super::{
    load_from, persist_to, record_path, rfc3339, running_build, HoldStatus, PrepState,
    PrepareRecord, ReleaseReport, ReleaseTrigger,
};
use crate::coord_drain_state::DrainObservation;

/// Tauri event emitted on every prepare state change. Payload: [`PrepChanged`].
pub const RESTART_PREP_CHANGED_EVENT: &str = "restart-prep-changed";

/// The runner's default hold length when coord gives none: 6 h (D1).
pub const DEFAULT_HOLD_MS: i64 = 6 * 60 * 60 * 1000;

/// The furthest out an Extend may push the hold's `until`, from now: under
/// coord's own 30-day drain ceiling (`MAX_DRAIN_DAYS`, which refuses a
/// later `until` with a 400), with a day of margin for clock skew.
pub const MAX_HOLD_MS: i64 = 29 * 24 * 60 * 60 * 1000;

/// The prefix coord writes before the prepare id in the hold's `reason`.
pub const HOLD_REASON_PREFIX: &str = "restart-hold:";

/// The longest operator reason sent, in bytes. Coord refuses more than 512;
/// the cut leaves room and lands on a char boundary.
pub const MAX_REASON_BYTES: usize = 400;

/// The reason sent when the operator gave none (coord refuses a blank one).
pub const DEFAULT_REASON: &str = "operator prepare for restart";

// ---------------------------------------------------------------------------
// The door to coord
// ---------------------------------------------------------------------------

/// Where this runner instance stands toward coord's device drain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HoldRole {
    /// The instance that owns the device heartbeat on an enrolled device: it
    /// takes the coord hold.
    Primary,
    /// Not a coord device: the runner-local hold.
    NotEnrolled { why: String },
    /// A secondary instance: the runner-local hold over its own spawns only.
    Secondary,
    /// The local files could not say whether this is a coord device. The hold
    /// is unconfirmed; the drain state already fails closed on the same read.
    Indeterminate { cause: String },
}

/// `POST /coord/devices/me/hold` body. Exactly the coord contract's fields
/// (it is `deny_unknown_fields`); the device comes from the JWT.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HoldRequest {
    pub reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub until: Option<String>,
    pub prepare_id: String,
}

/// What a hold POST answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HoldCall {
    /// `200`: the agent lane holds this prepare's record. `until_ms` is the
    /// record's deadline, when coord served one.
    Held { until_ms: Option<i64> },
    /// `409 {"error": <code>}`: the lane was left as it is (held by another
    /// owner, unreadable, or the hold had already expired).
    Conflict { code: String },
    /// `404`: this coord has no such route (Phase 2 not deployed).
    RouteAbsent,
    /// Any other answer.
    Refused { status: u16, detail: String },
    /// No answer at all.
    Unreachable { cause: String },
}

/// What a release POST answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReleaseCall {
    /// `200`: coord's per-lane outcome, verbatim.
    Answered {
        outcome: String,
    },
    RouteAbsent,
    Refused {
        status: u16,
        detail: String,
    },
    Unreachable {
        cause: String,
    },
    /// No release could be sent because this instance has no coord lever: a
    /// secondary, or a runner that is not a coord device. Settles.
    NotSent {
        why: String,
    },
    /// The local files could not say whether this is a coord device, so no
    /// release was sent — and the hold may well stand. Does NOT settle.
    Indeterminate {
        cause: String,
    },
}

/// Coord, as the prepare needs it. The live door is [`LiveDoor`]; tests
/// inject their own.
#[async_trait]
pub trait HoldDoor: Send + Sync {
    fn role(&self) -> HoldRole;
    async fn hold(&self, req: &HoldRequest) -> HoldCall;
    async fn release(&self, prepare_id: &str) -> ReleaseCall;
    /// `GET /coord/devices/me/drain`, the read-back for both. Reads only:
    /// what the read means for the runner's own drain state is folded in by
    /// [`Self::publish_read`], which the controller calls once the record says
    /// what the read means for the cohort.
    async fn read_drain(&self) -> Result<DrainObservation, String>;
    /// Fold a [`Self::read_drain`] answer into the runner's drain state, so the
    /// banner and the gate see it now rather than on the next heartbeat. The
    /// controller calls it AFTER recording what the read means: a release's
    /// read-back lifts the drain, and a lifted drain re-runs every deferred
    /// boot restore, so the record must already say `restoring` by then
    /// (otherwise the cohort's rows reach the auto-restore all at once).
    fn publish_read(&self, _read: &Result<DrainObservation, String>) {}
}

fn excerpt(body: &str) -> String {
    body.chars().take(200).collect()
}

/// PURE: a hold POST's status and body → [`HoldCall`].
pub fn parse_hold_reply(status: u16, body: &str) -> HoldCall {
    let json = serde_json::from_str::<serde_json::Value>(body).ok();
    match status {
        200..=299 => {
            let until_ms = json
                .as_ref()
                .and_then(|j| j.get("until"))
                .and_then(|u| u.as_str())
                .and_then(|u| chrono::DateTime::parse_from_rfc3339(u).ok())
                .map(|u| u.timestamp_millis());
            HoldCall::Held { until_ms }
        }
        404 => HoldCall::RouteAbsent,
        409 => match json
            .as_ref()
            .and_then(|j| j.get("error"))
            .and_then(|e| e.as_str())
        {
            Some(code) if !code.trim().is_empty() => HoldCall::Conflict {
                code: code.to_string(),
            },
            _ => HoldCall::Refused {
                status,
                detail: excerpt(body),
            },
        },
        _ => HoldCall::Refused {
            status,
            detail: excerpt(body),
        },
    }
}

/// PURE: a release POST's status and body → [`ReleaseCall`]. A `200` with no
/// readable `outcome` is a refusal naming that, never a guessed "released".
pub fn parse_release_reply(status: u16, body: &str) -> ReleaseCall {
    match status {
        200..=299 => match serde_json::from_str::<serde_json::Value>(body)
            .ok()
            .and_then(|j| {
                j.get("outcome")
                    .and_then(|o| o.as_str())
                    .map(str::to_string)
            }) {
            Some(outcome) if !outcome.trim().is_empty() => ReleaseCall::Answered { outcome },
            _ => ReleaseCall::Refused {
                status,
                detail: format!("no `outcome` in the release answer: {}", excerpt(body)),
            },
        },
        404 => ReleaseCall::RouteAbsent,
        _ => ReleaseCall::Refused {
            status,
            detail: excerpt(body),
        },
    }
}

/// PURE: whether a `me/drain` read confirms THIS prepare's hold. `Err` names
/// why not.
pub fn confirms_hold(
    read: &Result<DrainObservation, String>,
    prepare_id: &str,
) -> Result<(), String> {
    let marker = format!("{HOLD_REASON_PREFIX}{prepare_id}:");
    match read {
        Ok(DrainObservation::Drained {
            reason: Some(reason),
            ..
        }) if reason.starts_with(&marker) => Ok(()),
        Ok(DrainObservation::Drained { reason, .. }) => Err(format!(
            "read_back_not_this_hold: the device reads drained but its reason {:?} does not \
             name this prepare",
            reason.as_deref().unwrap_or("")
        )),
        Ok(other) => Err(format!(
            "read_back_not_drained: {}",
            observation_label(other)
        )),
        Err(e) => Err(format!("read_back_failed: {e}")),
    }
}

fn observation_label(obs: &DrainObservation) -> String {
    match obs {
        DrainObservation::Clear => "clear".to_string(),
        DrainObservation::Drained { reason, .. } => {
            format!("drained ({})", reason.as_deref().unwrap_or("no reason"))
        }
        DrainObservation::Unreadable => "unreadable".to_string(),
        DrainObservation::Absent { detail } => format!("absent: {detail}"),
        DrainObservation::NotEnrolled { why } => format!("not enrolled: {why}"),
    }
}

fn read_back_label(read: &Result<DrainObservation, String>) -> String {
    match read {
        Ok(obs) => observation_label(obs),
        Err(e) => format!("unreadable: {e}"),
    }
}

/// The live door: the device JWT, through `coord_drain_state`'s client.
pub struct LiveDoor;

const HOLD_PATH: &str = "/coord/devices/me/hold";
const RELEASE_PATH: &str = "/coord/devices/me/release";

#[async_trait]
impl HoldDoor for LiveDoor {
    fn role(&self) -> HoldRole {
        use crate::coord_drain_state::Enrollment;
        // The same predicate the device heartbeat uses: only the instance that
        // owns the machine payload is the device's primary.
        if !crate::fleet::machine_state_publish_allowed(crate::instance::owns_shared_root_state()) {
            return HoldRole::Secondary;
        }
        match crate::coord_drain_state::enrollment() {
            Enrollment::Enrolled(_) => HoldRole::Primary,
            Enrollment::NotEnrolled(why) => HoldRole::NotEnrolled {
                why: why.to_string(),
            },
            Enrollment::Indeterminate(cause) => HoldRole::Indeterminate { cause },
        }
    }

    async fn hold(&self, req: &HoldRequest) -> HoldCall {
        let crate::coord_drain_state::Enrollment::Enrolled(base) =
            crate::coord_drain_state::enrollment()
        else {
            return HoldCall::Unreachable {
                cause: "no coord base resolved at hold time".to_string(),
            };
        };
        let body = match serde_json::to_value(req) {
            Ok(b) => b,
            Err(e) => {
                return HoldCall::Unreachable {
                    cause: format!("hold body did not serialize: {e}"),
                }
            }
        };
        match crate::coord_drain_state::post_device_json(&base, HOLD_PATH, &body).await {
            Ok((status, text)) => parse_hold_reply(status, &text),
            Err(cause) => HoldCall::Unreachable { cause },
        }
    }

    async fn release(&self, prepare_id: &str) -> ReleaseCall {
        if !crate::fleet::machine_state_publish_allowed(crate::instance::owns_shared_root_state()) {
            return ReleaseCall::NotSent {
                why: "a secondary instance never writes the device's drain".to_string(),
            };
        }
        let base = match crate::coord_drain_state::enrollment() {
            crate::coord_drain_state::Enrollment::Enrolled(base) => base,
            crate::coord_drain_state::Enrollment::NotEnrolled(why) => {
                return ReleaseCall::NotSent {
                    why: why.to_string(),
                }
            }
            crate::coord_drain_state::Enrollment::Indeterminate(cause) => {
                return ReleaseCall::Indeterminate { cause }
            }
        };
        let body = serde_json::json!({ "prepare_id": prepare_id });
        match crate::coord_drain_state::post_device_json(&base, RELEASE_PATH, &body).await {
            Ok((status, text)) => parse_release_reply(status, &text),
            Err(cause) => ReleaseCall::Unreachable { cause },
        }
    }

    async fn read_drain(&self) -> Result<DrainObservation, String> {
        let read = match crate::coord_drain_state::enrollment() {
            crate::coord_drain_state::Enrollment::Enrolled(base) => {
                crate::coord_drain_state::read_me_drain(&base).await
            }
            crate::coord_drain_state::Enrollment::NotEnrolled(why) => {
                Err(format!("not a coord device: {why}"))
            }
            crate::coord_drain_state::Enrollment::Indeterminate(cause) => Err(cause),
        };
        read
    }

    fn publish_read(&self, read: &Result<DrainObservation, String>) {
        // A fresh read is a fresh read: the banner and the gate see the hold
        // (or its release) now, not on the next heartbeat.
        crate::coord_drain_state::fold_me_drain_read(read);
    }
}

// ---------------------------------------------------------------------------
// The view
// ---------------------------------------------------------------------------

/// What a record means at a moment — the panel's and the banner's reading.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrepView {
    /// The hold as the panel spells it: `confirmed`, `unconfirmed: <cause>`,
    /// `local`, `local (secondary)`, `release: <outcome>`, or
    /// `hold expired at <time>` — an expired hold never reads as held.
    pub hold: String,
    /// In its window and not past its `until`.
    pub active: bool,
    pub accounts_frozen: bool,
    /// The runner-local hold is gating `drain_gate` — the `local` holds, and
    /// the backstop under an `unconfirmed` one (see [`holds_locally`]).
    pub local_hold: bool,
    /// The runner restarted on the build that prepared, so spawns are still
    /// held: the "same build — spawns still held" banner, whose Release hold
    /// is the cancel path.
    pub same_build_hold: bool,
    pub hold_expired_at: Option<String>,
    /// A release was sent and did not settle (coord unreachable, refused,
    /// route absent): the prepare is still in force and the hold still
    /// stands. The outcome, verbatim; Release hold retries it.
    pub release_failed: Option<String>,
    /// The verdict in the record was evaluated by THIS runner process. False
    /// after a restart until the notice loop's first tick here: the persisted
    /// verdict and row states then describe the previous process's sessions,
    /// and the panel reads them as the last reading, never as current.
    pub verdict_current: bool,
}

/// Whether `hold` puts the runner-local hold on `drain_gate`.
///
/// The `local` holds, by definition (D1). And an `unconfirmed` one: the plan
/// expected the runner's drain state to fail closed by itself there, but that
/// is true only when coord is unreachable for three heartbeats — a refused
/// hold, a `404` from a coord without the route, or a hold POST that alone
/// timed out all leave `me/drain` reading `clear`, and autonomous spawns would
/// run through a prepare. So an unconfirmed hold is backstopped locally. When
/// the device IS drained by another owner (`lane_held_by_other`) the local
/// hold is redundant and harmless.
pub fn holds_locally(hold: &HoldStatus) -> bool {
    matches!(
        hold,
        HoldStatus::Local | HoldStatus::LocalSecondary | HoldStatus::Unconfirmed { .. }
    )
}

/// Whether anything holds spawns under `hold`: coord's confirmed hold, or the
/// runner-local one.
pub fn spawns_held(hold: &HoldStatus) -> bool {
    matches!(hold, HoldStatus::Confirmed) || holds_locally(hold)
}

/// Whether a release outcome SETTLES the hold: coord gave a lane outcome
/// (`released | not_owner | absent | unreadable`), the hold was runner-local,
/// or there is no coord lever to send one through. Anything else — coord
/// unreachable, refused, an outcome this build does not know — leaves the
/// hold standing, so the prepare stays in force.
///
/// `route_absent` (a coord with no self-hold route) settles only when coord
/// never CONFIRMED a hold for this prepare (`coord_hold_confirmed` false): a
/// coord without the route never held anything for us, so there is nothing
/// to let go — and refusing to settle would leave the prepare (and its
/// runner-local backstop) in force with no way to end it, before or after a
/// rebuild. A coord that DID confirm a hold and then answers `404` to its
/// release has not let it go: owner-conditional semantics are kept.
pub fn release_settled(outcome: &str, coord_hold_confirmed: bool) -> bool {
    matches!(
        outcome,
        "released" | "not_owner" | "absent" | "unreadable" | "local"
    ) || (outcome == "route_absent" && !coord_hold_confirmed)
        || outcome.starts_with("not_sent: ")
        || outcome.starts_with("already released: ")
}

/// The release of `rec` SETTLED BY EXPIRY, or `None`: a release that did not
/// settle (`outcome`, [`release_settled`] false) on a runner that `restarted`
/// since the prepare, once the hold's `until` has passed at `now_ms`. Coord's
/// drain expires on its own at that `until`, so there is nothing left to
/// release — and refusing to settle would leave the record in its window,
/// where it refuses every new prepare ([`RestartPrep::prepare`]) and its
/// cohort is never restored, for as long as coord stays unreachable or keeps
/// answering `route_absent` to a hold it once confirmed. A runner that has
/// NOT restarted keeps its sessions running and the prepare in force: its
/// expired hold is renewed by Extend or ended by a release coord answers.
///
/// Coord's word beats the local clock: when the release's `read` back says
/// coord still holds a drain for THIS prepare ([`confirms_hold`]) — coord's
/// `until` ran past the recorded one, or the clocks disagree — the release
/// does not settle, and Release hold stays the door to retry it. An
/// unreadable or absent read keeps the escape, which is the coord-unreachable
/// case it exists for. A hold with no recorded `until` never expires here.
fn expired_release(
    rec: &PrepareRecord,
    restarted: bool,
    now_ms: i64,
    outcome: &str,
    read: Option<&Result<DrainObservation, String>>,
) -> Option<String> {
    let coord_still_holds = read.is_some_and(|r| confirms_hold(r, &rec.prepare_id).is_ok());
    (restarted && rec.hold_expired(now_ms) && !coord_still_holds).then(|| {
        let until = rec
            .hold_until
            .clone()
            .or_else(|| rec.hold_until_ms.map(rfc3339))
            .unwrap_or_default();
        format!("expired: {until} (the release answered {outcome})")
    })
}

/// Whether coord ever confirmed a hold for `rec` (see
/// [`PrepareRecord::coord_hold_confirmed`]); a record whose hold reads
/// `confirmed` now has, whatever the flag says.
fn coord_hold_was_confirmed(rec: &PrepareRecord) -> bool {
    rec.coord_hold_confirmed || rec.hold == HoldStatus::Confirmed
}

/// A record just moved to `restoring` with NO row waiting — every session
/// was closed on purpose during the prepare, so none is a restore candidate
/// — has nothing to restore: it is `done` at once, rather than a cohort that
/// shows an empty strip and blocks the next prepare until it is dismissed.
fn settle_empty_restore(rec: &mut PrepareRecord) {
    if super::restore::RestoringCohort::of(rec).is_some_and(|c| c.ids().is_empty()) {
        rec.state = PrepState::Done;
    }
}

/// Why `action` is refused on a runner rebuilt since `rec` was prepared, or
/// `None`. The hold waits for a rebuild (D1); a rebuilt runner whose boot
/// release did not settle owes a release — never a longer drain, and never a
/// re-sent hold for the old prepare.
fn rebuilt_refusal(rec: &PrepareRecord, action: &str) -> Option<String> {
    (rec.prepared_build != running_build()).then(|| {
        format!(
            "this runner was rebuilt since the prepare — release the hold rather than {action} it"
        )
    })
}

/// PURE: the view of `rec` at `now_ms`.
pub fn view_of(rec: &PrepareRecord, same_build_restart: bool, now_ms: i64) -> PrepView {
    let active = rec.is_active(now_ms);
    let expired_at = (rec.in_window() && rec.hold_expired(now_ms))
        .then(|| rec.hold_until.clone().unwrap_or_default());
    let hold = match &expired_at {
        Some(at) => format!("hold expired at {at}"),
        None => rec.hold.label(),
    };
    PrepView {
        hold,
        active,
        accounts_frozen: active,
        local_hold: active && holds_locally(&rec.hold),
        same_build_hold: same_build_restart && active && spawns_held(&rec.hold),
        hold_expired_at: expired_at,
        verdict_current: rec.verdict.as_ref().and_then(|v| v.evaluated_by.as_deref())
            == Some(super::running_process()),
        release_failed: rec
            .release
            .as_ref()
            .filter(|_| rec.in_window())
            .map(|r| r.outcome.clone()),
    }
}

/// The `restart-prep-changed` payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PrepChanged {
    pub record: PrepareRecord,
    pub view: PrepView,
}

// ---------------------------------------------------------------------------
// The controller
// ---------------------------------------------------------------------------

/// What a usage-limit (or rate-limit) hit met.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FreezeHit {
    /// No active prepare: the caller proceeds as it always did.
    NotFrozen,
    /// An active prepare froze the account move. `row_marked` says whether a
    /// cohort row carried the session (a chat session may have none).
    Frozen { row_marked: bool },
}

/// What the boot step did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BootStep {
    NoRecord,
    /// The record is restoring, done or cancelled: nothing held.
    NotInWindow(PrepState),
    /// A different build: the hold was released (outcome verbatim) and the
    /// record moved to `restoring`.
    ReleasedForRebuild {
        outcome: String,
    },
    /// A different build, but the release did not settle: the hold stands and
    /// the record stays in its window (Release hold, or the next boot, retries).
    ReleaseFailed {
        outcome: String,
    },
    /// The same build: the hold stays (unless it has expired).
    SameBuildHeld {
        expired: bool,
    },
    /// Another runner process on the same instance root holds the record
    /// (its ownership lock): this process changed nothing.
    NotOwner,
}

type ChangeHook = Box<dyn Fn(&PrepChanged) + Send + Sync>;

/// The prepare record and the operations on it. One per process in the live
/// runner ([`install_live`]); tests build their own over a temp path.
pub struct RestartPrep {
    path: PathBuf,
    record: Mutex<Option<PrepareRecord>>,
    same_build_restart: AtomicBool,
    /// A notice-loop change that is in memory but not yet on disk (its write
    /// failed); [`Self::update_quiet`] retries the write on every call.
    persist_pending: AtomicBool,
    /// Serializes prepare / cancel / boot / the notice tick so two clicks
    /// (or a click and a tick) cannot race.
    pub(super) ops: tokio::sync::Mutex<()>,
    on_change: Option<ChangeHook>,
    /// The notice loop's told-but-busy diagnostic memory: per session, the key
    /// of the line last logged (`notice::log_told_busy`). In memory only, and
    /// cleared when a new prepare starts.
    pub(super) told_busy_logged: Mutex<HashMap<String, String>>,
    /// This process's exclusive lock on the record (`<record>.lock`), once it
    /// won it ([`Self::claim_ownership`]). Held for the process's life; the OS
    /// releases it at exit.
    owner_lock: Mutex<Option<std::fs::File>>,
    /// The lock could not be TAKEN at all (no lock file, a filesystem without
    /// locks) — not "another process holds it". Ownership then falls back to
    /// the pre-lock behaviour: this process acts on the record, as every
    /// process did before the lock existed, and says so once.
    owner_lock_unavailable: AtomicBool,
}

fn sanitize_reason(reason: &str) -> String {
    let cleaned: String = reason
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        return DEFAULT_REASON.to_string();
    }
    let mut end = trimmed.len().min(MAX_REASON_BYTES);
    while !trimmed.is_char_boundary(end) {
        end -= 1;
    }
    trimmed
        .get(..end)
        .unwrap_or(DEFAULT_REASON)
        .trim_end()
        .to_string()
}

impl RestartPrep {
    /// A controller over the record at `path` (loaded now, if readable).
    pub fn load(path: PathBuf, on_change: Option<ChangeHook>) -> Self {
        let record = load_from(&path);
        Self {
            path,
            record: Mutex::new(record),
            same_build_restart: AtomicBool::new(false),
            persist_pending: AtomicBool::new(false),
            ops: tokio::sync::Mutex::new(()),
            on_change,
            told_busy_logged: Mutex::new(HashMap::new()),
            owner_lock: Mutex::new(None),
            owner_lock_unavailable: AtomicBool::new(false),
        }
    }

    pub(super) fn lock(&self) -> std::sync::MutexGuard<'_, Option<PrepareRecord>> {
        match self.record.lock() {
            Ok(g) => g,
            Err(e) => e.into_inner(),
        }
    }

    pub fn record(&self) -> Option<PrepareRecord> {
        self.lock().clone()
    }

    pub fn same_build_restart(&self) -> bool {
        self.same_build_restart.load(Ordering::SeqCst)
    }

    pub fn view(&self, now_ms: i64) -> Option<PrepView> {
        self.record()
            .map(|r| view_of(&r, self.same_build_restart(), now_ms))
    }

    /// The runner-local hold's description while it gates `drain_gate`.
    pub fn local_hold_reason(&self, now_ms: i64) -> Option<String> {
        let guard = self.lock();
        let rec = guard.as_ref()?;
        (rec.is_active(now_ms) && holds_locally(&rec.hold)).then(|| {
            format!(
                "prepare {} — hold {}, until {}",
                rec.prepare_id,
                rec.hold.label(),
                rec.hold_until.as_deref().unwrap_or("?")
            )
        })
    }

    pub fn accounts_frozen(&self, now_ms: i64) -> bool {
        self.lock().as_ref().is_some_and(|r| r.is_active(now_ms))
    }

    pub(super) fn emit(&self, rec: &PrepareRecord) {
        if let Some(hook) = &self.on_change {
            hook(&PrepChanged {
                record: rec.clone(),
                view: view_of(rec, self.same_build_restart(), now_ms()),
            });
        }
    }

    /// Persist, cache, then announce — the announcement outside the lock.
    fn store(&self, rec: PrepareRecord) -> Result<(), String> {
        persist_to(&self.path, &rec)
            .map_err(|e| format!("restart_prep: could not write {}: {e}", self.path.display()))?;
        *self.lock() = Some(rec.clone());
        self.emit(&rec);
        Ok(())
    }

    /// Apply `f` to the CURRENT record — under the lock, so a `limit_hit` the
    /// freeze wrote while an operation awaited coord is kept — then persist
    /// and announce. Errs when the record is no longer `prepare_id`'s.
    pub(super) fn update(
        &self,
        prepare_id: &str,
        f: impl FnOnce(&mut PrepareRecord),
    ) -> Result<PrepareRecord, String> {
        let rec = {
            let mut guard = self.lock();
            let Some(mut next) = guard
                .as_ref()
                .filter(|r| r.prepare_id == prepare_id)
                .cloned()
            else {
                return Err(format!(
                    "restart_prep: the record is no longer prepare {prepare_id}"
                ));
            };
            f(&mut next);
            // Persist first: a failed write leaves memory and disk agreeing
            // on the record as it was.
            persist_to(&self.path, &next).map_err(|e| {
                format!("restart_prep: could not write {}: {e}", self.path.display())
            })?;
            *guard = Some(next.clone());
            next
        };
        self.emit(&rec);
        Ok(rec)
    }

    /// [`Self::update`] for the notice loop, which re-reads the census every
    /// tick: apply `f` to the current record, keep the result in memory, and
    /// persist and announce `restart-prep-changed` ONLY when something an
    /// observer can see changed ([`material`]) — a row state, a stamp, the
    /// verdict, the prepare state. A tick that merely re-read the same world
    /// moves the re-capture and evaluation times in memory and writes nothing.
    ///
    /// Unlike [`Self::update`], a failed write does NOT roll memory back: the
    /// tick's delivery stamps record text that already reached a pane, and
    /// dropping them would re-send the notice on the next tick. The change is
    /// kept and announced, the write failure is logged, and every later call
    /// retries the write until it lands (the same posture as
    /// [`Self::freeze_hit`]).
    pub(super) fn update_quiet(
        &self,
        prepare_id: &str,
        f: impl FnOnce(&mut PrepareRecord),
    ) -> Result<(PrepareRecord, bool), String> {
        let (rec, changed) = {
            let mut guard = self.lock();
            let Some(current) = guard
                .as_ref()
                .filter(|r| r.prepare_id == prepare_id)
                .cloned()
            else {
                return Err(format!(
                    "restart_prep: the record is no longer prepare {prepare_id}"
                ));
            };
            let mut next = current.clone();
            f(&mut next);
            let changed = material(&current) != material(&next);
            if changed || self.persist_pending.load(Ordering::SeqCst) {
                let written = persist_to(&self.path, &next);
                self.persist_pending
                    .store(written.is_err(), Ordering::SeqCst);
                if let Err(e) = written {
                    warn!(
                        error = %e,
                        path = %self.path.display(),
                        "restart_prep: the notice loop's change is kept in memory but the record \
                         write failed"
                    );
                }
            }
            *guard = Some(next.clone());
            (next, changed)
        };
        if changed {
            self.emit(&rec);
        }
        Ok((rec, changed))
    }

    /// While a prepare is active, a confirmed usage-limit hit freezes the
    /// account: mark the session's cohort row `limit_hit` (with the reset
    /// time, when known) and tell the caller not to move it.
    pub fn freeze_hit(
        &self,
        claude_session_id: &str,
        limit_reset_at_ms: Option<i64>,
        now_ms: i64,
    ) -> FreezeHit {
        let marked = {
            let mut guard = self.lock();
            let Some(rec) = guard.as_mut() else {
                return FreezeHit::NotFrozen;
            };
            if !rec.is_active(now_ms) {
                return FreezeHit::NotFrozen;
            }
            match rec
                .cohort
                .rows
                .iter_mut()
                .find(|r| r.session.claude_session_id == claude_session_id)
            {
                Some(row) => {
                    row.state_at_prepare = StateAtPrepare::LimitHit;
                    if limit_reset_at_ms.is_some() {
                        row.limit_reset_at = limit_reset_at_ms;
                    }
                    // A row change the cohort's last re-capture did not see:
                    // the verdict waits for the next one (Phase 4, condition 3).
                    rec.last_change_at_ms = Some(now_ms);
                    if let Err(e) = persist_to(&self.path, rec) {
                        warn!(
                            error = %e,
                            session = claude_session_id,
                            "restart_prep: limit_hit marked in memory but the record write failed"
                        );
                    }
                    Some(rec.clone())
                }
                None => None,
            }
        };
        match marked {
            Some(rec) => {
                self.emit(&rec);
                FreezeHit::Frozen { row_marked: true }
            }
            None => FreezeHit::Frozen { row_marked: false },
        }
    }

    /// Take (or retry) the hold for `rec`: the status it reached and coord's
    /// `until`, when coord gave one. Never writes the coord hold from a
    /// secondary or an un-enrolled runner. Touches no state — the caller
    /// applies the answer through [`Self::update`].
    async fn hold_outcome(
        &self,
        door: &dyn HoldDoor,
        rec: &PrepareRecord,
    ) -> (HoldStatus, Option<i64>) {
        let outcome = match door.role() {
            HoldRole::Secondary => (HoldStatus::LocalSecondary, None),
            HoldRole::NotEnrolled { .. } => (HoldStatus::Local, None),
            HoldRole::Indeterminate { cause } => (
                HoldStatus::Unconfirmed {
                    cause: format!("enrollment_indeterminate: {cause}"),
                },
                None,
            ),
            HoldRole::Primary => {
                let req = HoldRequest {
                    reason: rec.reason.clone(),
                    until: rec.hold_until.clone(),
                    prepare_id: rec.prepare_id.clone(),
                };
                let unconfirmed = |cause: String| (HoldStatus::Unconfirmed { cause }, None);
                match door.hold(&req).await {
                    HoldCall::Held { until_ms } => {
                        let read = door.read_drain().await;
                        // A hold read-back only ever tightens the gate: fold
                        // it in at once.
                        door.publish_read(&read);
                        match confirms_hold(&read, &rec.prepare_id) {
                            Ok(()) => (HoldStatus::Confirmed, until_ms),
                            Err(cause) => (HoldStatus::Unconfirmed { cause }, until_ms),
                        }
                    }
                    HoldCall::Conflict { code } => unconfirmed(code),
                    HoldCall::RouteAbsent => unconfirmed("route_absent".to_string()),
                    HoldCall::Refused { status, detail } => {
                        unconfirmed(format!("refused: HTTP {status}: {detail}"))
                    }
                    HoldCall::Unreachable { cause } => {
                        unconfirmed(format!("coord_unreachable: {cause}"))
                    }
                }
            }
        };
        info!(
            prepare = %rec.prepare_id,
            hold = %outcome.0.label(),
            "restart_prep: hold taken"
        );
        outcome
    }

    /// [`Self::hold_outcome`], applied to the current record.
    async fn take_hold(
        &self,
        door: &dyn HoldDoor,
        rec: &PrepareRecord,
    ) -> Result<PrepareRecord, String> {
        let (hold, until_ms) = self.hold_outcome(door, rec).await;
        self.update(&rec.prepare_id, |r| {
            // A hold confirmed after a failed release supersedes that report.
            if hold == HoldStatus::Confirmed {
                r.release = None;
                r.coord_hold_confirmed = true;
            }
            r.hold = hold;
            if let Some(u) = until_ms {
                r.hold_until_ms = Some(u);
                r.hold_until = Some(rfc3339(u));
            }
        })
    }

    /// Let the hold go and say what that answered, with the read-back the
    /// release took (if any). The read-back is NOT folded into the drain state
    /// here: the caller records what the release means first and then
    /// publishes it ([`HoldDoor::publish_read`]).
    async fn release_hold(
        &self,
        door: &dyn HoldDoor,
        rec: &PrepareRecord,
        trigger: ReleaseTrigger,
        now_ms: i64,
        build: &str,
    ) -> (ReleaseReport, Option<Result<DrainObservation, String>>) {
        let (outcome, read) = match &rec.hold {
            HoldStatus::Local | HoldStatus::LocalSecondary => ("local".to_string(), None),
            HoldStatus::ReleaseSent { outcome } => (format!("already released: {outcome}"), None),
            // The hold POST answered `404` and coord never confirmed a hold:
            // this coord has no self-hold route, so no coord hold of ours
            // exists and releasing needs no coord write.
            HoldStatus::Unconfirmed { cause }
                if cause == "route_absent" && !coord_hold_was_confirmed(rec) =>
            {
                (
                    "not_sent: no coord hold was taken (coord has no self-hold route)".to_string(),
                    None,
                )
            }
            // `NotAttempted` too: a crash between coord's answer and the write
            // that records it leaves a hold the record does not know about.
            // The release is owner-conditional, so the worst answer is `absent`.
            HoldStatus::NotAttempted | HoldStatus::Confirmed | HoldStatus::Unconfirmed { .. } => {
                let outcome = match door.release(&rec.prepare_id).await {
                    ReleaseCall::Answered { outcome } => outcome,
                    ReleaseCall::RouteAbsent => "route_absent".to_string(),
                    ReleaseCall::Refused { status, detail } => {
                        format!("refused: HTTP {status}: {detail}")
                    }
                    ReleaseCall::Unreachable { cause } => format!("coord_unreachable: {cause}"),
                    ReleaseCall::NotSent { why } => format!("not_sent: {why}"),
                    ReleaseCall::Indeterminate { cause } => {
                        format!("enrollment_indeterminate: {cause}")
                    }
                };
                let read = if outcome.starts_with("not_sent: ")
                    || outcome.starts_with("enrollment_indeterminate: ")
                {
                    None
                } else {
                    Some(door.read_drain().await)
                };
                (outcome, read)
            }
        };
        let read_back = read.as_ref().map(read_back_label);
        info!(
            prepare = %rec.prepare_id,
            trigger = ?trigger,
            outcome = %outcome,
            read_back = read_back.as_deref().unwrap_or("-"),
            "restart_prep: hold released"
        );
        (
            ReleaseReport {
                at_ms: now_ms,
                at: rfc3339(now_ms),
                trigger,
                on_build: build.to_string(),
                outcome,
                read_back,
            },
            read,
        )
    }

    /// Prepare for a restart. Idempotent while a prepare is active: a second
    /// call returns the active record, and re-tries only a hold that is not
    /// yet held (same `prepare_id`, so coord sees one hold).
    pub async fn prepare<F, Fut>(
        &self,
        door: &dyn HoldDoor,
        reason: &str,
        now_ms: i64,
        capture: F,
    ) -> Result<PrepareRecord, String>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<Cohort, String>>,
    {
        let _op = self.ops.lock().await;
        // A prepare still in its window on a runner that restarted since —
        // rebuilt, or the same build — is owed a release, which moves its
        // cohort to `restoring`. Never a re-sent hold for it, and never a new
        // record over it, whether or not its hold has expired: the old
        // cohort's rows are withheld from the auto-restore and shielded from
        // the orphan close only while this record names them, so replacing it
        // after the boot restore has run strands them — never restored, never
        // shown as placeholders.
        if let Some(rec) = self
            .record()
            .filter(|r| r.in_window() && (r.restarted_since() || self.same_build_restart()))
        {
            return Err(format!(
                "this runner was rebuilt or restarted since prepare {} — release its hold (its \
                 sessions are then restored by hand) rather than preparing again over it",
                rec.prepare_id
            ));
        }
        if let Some(rec) = self.record().filter(|r| r.is_active(now_ms)) {
            if matches!(
                rec.hold,
                HoldStatus::Confirmed | HoldStatus::Local | HoldStatus::LocalSecondary
            ) {
                return Ok(rec);
            }
            return self.take_hold(door, &rec).await;
        }
        // A rebuilt runner's cohort still waiting to be restored is the
        // operator's restore in progress: its placeholders, the shield that
        // keeps them from the orphan close, the held marks and the restore
        // verdicts all live in this record. A new prepare would overwrite it.
        if let Some(rec) = self.record().filter(|r| r.state == PrepState::Restoring) {
            // The rows NOT yet back — restored ones are no longer waiting.
            let waiting = super::restore::RestoringCohort::of(&rec)
                .map(|c| c.ids().len())
                .unwrap_or(0);
            return Err(format!(
                "the last prepare's sessions ({waiting}) are still waiting to be restored — \
                 restore or dismiss them before preparing again"
            ));
        }
        // A fresh prepare is not the one a past same-build boot saw: the flag
        // (the banner, and Release hold's restart test) belongs to that record.
        self.same_build_restart.store(false, Ordering::SeqCst);
        let cohort = capture().await?;
        // A new prepare's diagnostics start empty.
        self.told_busy_logged
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        let mut rec = PrepareRecord::new(uuid::Uuid::new_v4().to_string(), now_ms, cohort);
        rec.reason = sanitize_reason(reason);
        let until = now_ms + DEFAULT_HOLD_MS;
        rec.hold_until_ms = Some(until);
        rec.hold_until = Some(rfc3339(until));
        // The record first: a hold the next boot could not find is a hold
        // nothing releases before its `until`.
        self.store(rec.clone())?;
        self.take_hold(door, &rec).await
    }

    /// Extend the hold (the panel's Extend, D1): push its `until` out by
    /// [`DEFAULT_HOLD_MS`] from the later of now and the current `until` —
    /// so an expired hold is renewed from now — capped at [`MAX_HOLD_MS`]
    /// from now. The extended `until` is then sent through the same hold
    /// call as the prepare (same `prepare_id`, so coord treats it as a repeat
    /// of the caller's own hold and moves only its deadline), and the record
    /// takes coord's answer: its `until` when one came back, and the hold
    /// status the read-back supports. A runner-local hold just takes the new
    /// `until`. Errs when no prepare is in its window or the hold is already
    /// at the ceiling.
    pub async fn extend(&self, door: &dyn HoldDoor, now_ms: i64) -> Result<PrepareRecord, String> {
        let _op = self.ops.lock().await;
        let Some(rec) = self.record().filter(PrepareRecord::in_window) else {
            return Err("no prepare is in progress — nothing to extend".to_string());
        };
        // The hold waits for a rebuild (D1). A rebuilt runner whose boot
        // release did not settle owes a release, never a longer drain.
        if let Some(refusal) = rebuilt_refusal(&rec, "extending") {
            return Err(refusal);
        }
        let from = rec.hold_until_ms.unwrap_or(now_ms).max(now_ms);
        let until = (from + DEFAULT_HOLD_MS).min(now_ms + MAX_HOLD_MS);
        if rec.hold_until_ms.is_some_and(|u| until <= u) {
            return Err(format!(
                "the hold already runs to {} — the longest it may be held from now",
                rec.hold_until.as_deref().unwrap_or("?")
            ));
        }
        // The requested `until` goes to coord; the RECORD takes only what was
        // answered, so the panel never shows an expiry nobody granted.
        let mut asked = rec.clone();
        asked.hold_until_ms = Some(until);
        asked.hold_until = Some(rfc3339(until));
        let (hold, coord_until) = self.hold_outcome(door, &asked).await;
        let granted = match (&hold, coord_until) {
            // Coord named its own deadline.
            (_, Some(u)) => Some(u),
            // Held with no deadline in the answer (the one that was sent), or
            // held by the runner-local hold — the `local` ones, and the backstop
            // under an unconfirmed one ([`holds_locally`]) — whose deadline is
            // ours to set: spawns stay held to the new `until` either way.
            (h, None) if spawns_held(h) => Some(until),
            _ => rec.hold_until_ms,
        };
        info!(
            prepare = %rec.prepare_id,
            requested = %rfc3339(until),
            granted = granted.map(rfc3339).as_deref().unwrap_or("-"),
            hold = %hold.label(),
            "restart_prep: hold extension answered"
        );
        self.update(&rec.prepare_id, |r| {
            if hold == HoldStatus::Confirmed {
                r.release = None;
                r.coord_hold_confirmed = true;
            }
            r.hold = hold;
            r.hold_until_ms = granted;
            r.hold_until = granted.map(rfc3339);
        })
    }

    /// Cancel the prepare (and the same-build banner's Release hold): release
    /// the hold and record `cancelled`, which lifts the freeze and any local
    /// hold — or, once the runner has restarted since the prepare (rebuilt
    /// with a boot release that did not settle, or on the same build),
    /// `restoring`: the cohort is restored by hand, never auto-restored at once. A release that does not SETTLE ([`release_settled`]) leaves the
    /// prepare in force — the hold still stands — records the attempt, and
    /// errs, so the operator can try again. Except on a restarted runner
    /// whose hold has expired: coord's drain ended by itself, so the release
    /// settles as `expired: <until>` ([`expired_release`]).
    pub async fn cancel(&self, door: &dyn HoldDoor, now_ms: i64) -> Result<PrepareRecord, String> {
        let _op = self.ops.lock().await;
        let Some(rec) = self.record().filter(PrepareRecord::in_window) else {
            return Err("no prepare is in progress — nothing to cancel".to_string());
        };
        let (mut report, read) = self
            .release_hold(door, &rec, ReleaseTrigger::Cancel, now_ms, running_build())
            .await;
        let rebuilt = rec.restarted_since() || self.same_build_restart();
        let mut outcome = report.outcome.clone();
        let mut settled = release_settled(&outcome, coord_hold_was_confirmed(&rec));
        if !settled {
            if let Some(expired) = expired_release(&rec, rebuilt, now_ms, &outcome, read.as_ref()) {
                outcome = expired;
                report.outcome = outcome.clone();
                settled = true;
            }
        }
        // On a REBUILT runner — the boot step could not settle its release, so
        // the record stayed in its window — the release the operator pressed
        // is the one the boot step owed: the cohort moves to `restoring`, as
        // the boot step would have moved it, instead of `cancelled`, which
        // would hand every cohort row to the auto-restore at once (D6).
        // The same holds after a restart on the SAME build: the sessions died
        // with that restart exactly as with a rebuild, and D6 covers every
        // prepared restart. Only a cancel before any restart — the sessions
        // still running — is a true cancel.
        // Answered by the record itself (`restarted_since`), so a Release
        // pressed before the boot step has run is judged the same way
        // (`rebuilt`, above — which also lets an expired hold settle).
        let rec = self.update(&rec.prepare_id, |r| {
            if settled {
                r.hold = HoldStatus::ReleaseSent {
                    outcome: outcome.clone(),
                };
                r.state = if rebuilt {
                    PrepState::Restoring
                } else {
                    PrepState::Cancelled
                };
                settle_empty_restore(r);
            }
            r.release = Some(report);
        });
        // Only now, with the record saying `restoring` (or `cancelled`), may the
        // release's read-back lift the drain: a lifted drain re-runs every
        // deferred boot restore, which must find the cohort restoring.
        if let Some(read) = read.as_ref() {
            door.publish_read(read);
        }
        let rec = rec?;
        if settled {
            // The record left its window: the same-build flag was its, not the
            // next prepare's.
            self.same_build_restart.store(false, Ordering::SeqCst);
            Ok(rec)
        } else {
            Err(format!(
                "the hold was not released ({outcome}) — the prepare is still in force; try \
                 Release hold again"
            ))
        }
    }

    /// Win (or confirm) this process's exclusive ownership of the record: an
    /// OS file lock on `<record>.lock`, held for the process's life.
    ///
    /// Why: a second runner process started on the same instance root (it
    /// later fails to bind the port and exits) reads the same record before it
    /// gets that far. Without the lock it ran the boot step against the shared
    /// record — releasing the hold and moving the cohort to `restoring` on
    /// behalf of the runner that is actually serving. Only the process that
    /// owns the record may mutate it at boot or tick it. `false` when another
    /// process holds the lock, or the lock file cannot be opened (UNKNOWN is
    /// never ownership).
    ///
    /// A lock that cannot be taken at all — the lock file cannot be opened
    /// (a read-only directory), or the filesystem refuses locks — is NOT
    /// another process: it is logged as such, once, and ownership falls back
    /// to the pre-lock behaviour (this process acts) rather than leaving the
    /// record's hold unreleasable for its whole life.
    pub fn claim_ownership(&self) -> bool {
        let mut held = self.owner_lock.lock().unwrap_or_else(|e| e.into_inner());
        if held.is_some() || self.owner_lock_unavailable.load(Ordering::SeqCst) {
            return true;
        }
        let path = self.path.with_extension("lock");
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let file = match std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
        {
            Ok(f) => f,
            Err(e) => return self.lock_unavailable(&path, &e.to_string()),
        };
        match file.try_lock() {
            Ok(()) => {
                *held = Some(file);
                true
            }
            Err(std::fs::TryLockError::WouldBlock) => false,
            Err(std::fs::TryLockError::Error(e)) => self.lock_unavailable(&path, &e.to_string()),
        }
    }

    /// The ownership lock could not be taken for a reason other than another
    /// holder: log it distinctly, once, and fall back to acting.
    fn lock_unavailable(&self, path: &std::path::Path, why: &str) -> bool {
        if !self.owner_lock_unavailable.swap(true, Ordering::SeqCst) {
            warn!(
                path = %path.display(),
                error = why,
                "restart_prep: the record's ownership lock cannot be taken (NOT another runner \
                 process — the lock file or the filesystem refuses it); acting on the record \
                 without it, as before the lock existed"
            );
        }
        true
    }

    /// Whether this process owns the record (see [`Self::claim_ownership`]).
    pub fn owns_record(&self) -> bool {
        self.owner_lock_unavailable.load(Ordering::SeqCst)
            || self
                .owner_lock
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_some()
    }

    /// The late claim (review 5a): a process whose boot step found the record
    /// owned by another process (`NotOwner`) — typically the previous runner
    /// of a quick restart, which freed the port but took longer than the boot
    /// wait to exit — keeps trying from its notice loop. When it finally wins
    /// the lock it runs the boot step it skipped, so the hold is released and
    /// the cohort moves to `restoring` instead of spawns staying deferred until
    /// the hold expires. `None` when this process already owned the record
    /// (nothing to do) or still cannot claim it.
    pub async fn claim_late(
        &self,
        door: &dyn HoldDoor,
        build: &str,
        now_ms: i64,
    ) -> Option<BootStep> {
        if self.owns_record() || !self.claim_ownership() {
            return None;
        }
        Some(self.on_boot(door, build, now_ms).await)
    }

    /// The boot step: a rebuilt runner releases the hold and starts restoring;
    /// the same build keeps it (re-trying a hold that never took). Only in the
    /// process that owns the record ([`Self::claim_ownership`]); any other
    /// returns [`BootStep::NotOwner`] having changed nothing.
    pub async fn on_boot(&self, door: &dyn HoldDoor, build: &str, now_ms: i64) -> BootStep {
        if !self.claim_ownership() {
            return BootStep::NotOwner;
        }
        let _op = self.ops.lock().await;
        let Some(rec) = self.record() else {
            return BootStep::NoRecord;
        };
        if !rec.in_window() {
            return BootStep::NotInWindow(rec.state);
        }
        if rec.prepared_build != build {
            let (mut report, read) = self
                .release_hold(door, &rec, ReleaseTrigger::Rebuild, now_ms, build)
                .await;
            let mut outcome = report.outcome.clone();
            let mut settled = release_settled(&outcome, coord_hold_was_confirmed(&rec));
            // A rebuilt runner has restarted by definition: an expired hold
            // settles here as it does on Release hold ([`expired_release`]).
            if !settled {
                if let Some(expired) = expired_release(&rec, true, now_ms, &outcome, read.as_ref())
                {
                    outcome = expired;
                    report.outcome = outcome.clone();
                    settled = true;
                }
            }
            let applied = self.update(&rec.prepare_id, |r| {
                if settled {
                    r.hold = HoldStatus::ReleaseSent {
                        outcome: outcome.clone(),
                    };
                    r.state = PrepState::Restoring;
                    settle_empty_restore(r);
                }
                r.release = Some(report);
            });
            if let Err(e) = applied {
                warn!("{e}");
            }
            // The record says `restoring` before the read-back lifts the drain
            // (see `cancel`).
            if let Some(read) = read.as_ref() {
                door.publish_read(read);
            }
            return if settled {
                BootStep::ReleasedForRebuild { outcome }
            } else {
                warn!(
                    prepare = %rec.prepare_id,
                    outcome = %outcome,
                    "restart_prep: the rebuilt runner could not release the restart hold — \
                     the prepare stays in force until Release hold or the next boot succeeds"
                );
                BootStep::ReleaseFailed { outcome }
            };
        }
        self.same_build_restart.store(true, Ordering::SeqCst);
        let expired = rec.hold_expired(now_ms);
        let rec = if !expired
            && matches!(
                rec.hold,
                HoldStatus::NotAttempted | HoldStatus::Unconfirmed { .. }
            ) {
            match self.take_hold(door, &rec).await {
                Ok(r) => r,
                Err(e) => {
                    warn!("{e}");
                    rec
                }
            }
        } else {
            rec
        };
        warn!(
            prepare = %rec.prepare_id,
            hold = %view_of(&rec, true, now_ms).hold,
            "restart_prep: restarted on the build that prepared — spawns still held until \
             the operator releases the hold or the runner is rebuilt"
        );
        self.emit(&rec);
        BootStep::SameBuildHeld { expired }
    }
}

// ---------------------------------------------------------------------------
// The live controller
// ---------------------------------------------------------------------------

static LIVE: OnceLock<RestartPrep> = OnceLock::new();

pub(super) fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn live_hook() -> ChangeHook {
    Box::new(|changed: &PrepChanged| {
        if let Some(handle) = crate::tauri_app_handle::current() {
            use tauri::Emitter;
            if let Err(e) = handle.emit(RESTART_PREP_CHANGED_EVENT, changed) {
                warn!("restart_prep: emit {RESTART_PREP_CHANGED_EVENT} failed: {e}");
            }
        }
        // A local hold that started or lifted is not in the cached coord
        // state, so wake the drain waiters to re-check the gate now.
        crate::coord_drain_state::nudge_waiters();
    })
}

/// The process's controller, created on first use over [`record_path`].
/// Never in a unit-test build: a test must not read this box's record or
/// hold its spawns — tests build their own [`RestartPrep`].
pub fn install_live() -> Option<&'static RestartPrep> {
    if cfg!(test) {
        return LIVE.get();
    }
    Some(LIVE.get_or_init(|| RestartPrep::load(record_path(), Some(live_hook()))))
}

/// Whether the runner restarted on the build that prepared (see [`on_boot`]).
pub fn same_build_restart() -> bool {
    LIVE.get().is_some_and(RestartPrep::same_build_restart)
}

/// The runner-local hold's description while it gates `drain_gate`. Cheap;
/// `None` before the controller is installed (the boot drain read installs
/// it, and until then the drain state is `Unknown`, which defers anyway).
pub fn local_hold_reason() -> Option<String> {
    #[cfg(test)]
    if let Some(hold) = TEST_LOCAL_HOLD.with(|h| h.borrow().clone()) {
        return Some(hold);
    }
    LIVE.get()?.local_hold_reason(now_ms())
}

#[cfg(test)]
thread_local! {
    /// A unit test's stand-in for an active runner-local hold, on its own
    /// thread only — a test never installs the live controller.
    static TEST_LOCAL_HOLD: std::cell::RefCell<Option<String>> =
        const { std::cell::RefCell::new(None) };
}

/// Make [`local_hold_reason`] answer `hold` on this test thread.
#[cfg(test)]
pub(crate) fn set_test_local_hold(hold: Option<&str>) {
    TEST_LOCAL_HOLD.with(|h| *h.borrow_mut() = hold.map(str::to_string));
}

/// Whether an active prepare freezes account moves.
pub fn accounts_frozen() -> bool {
    install_live().is_some_and(|c| c.accounts_frozen(now_ms()))
}

/// [`RestartPrep::freeze_hit`] on the live record.
pub fn freeze_hit(claude_session_id: &str, limit_reset_at_ms: Option<i64>) -> FreezeHit {
    match install_live() {
        Some(c) => c.freeze_hit(claude_session_id, limit_reset_at_ms, now_ms()),
        None => FreezeHit::NotFrozen,
    }
}

/// The boot step on the live record. Runs inside
/// `coord_drain_state::boot_read`, after the drain read.
pub async fn on_boot() {
    let Some(ctl) = install_live() else {
        return;
    };
    // The previous process of a quick restart can still hold the lock for a
    // moment while it exits: wait a bounded time for it. Bounded BELOW the
    // 15 s every `await_boot_read` caller waits (this runs inside
    // `coord_drain_state::boot_read`), so the boot read still completes before
    // they give up on it; a lock won later is claimed by the notice loop
    // (`RestartPrep::claim_late`), which then runs this boot step.
    let deadline = std::time::Instant::now() + BOOT_OWNERSHIP_WAIT;
    while !ctl.claim_ownership() && std::time::Instant::now() < deadline {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    let step = ctl.on_boot(&LiveDoor, running_build(), now_ms()).await;
    if step == BootStep::NotOwner {
        warn!(
            "restart_prep: another runner process owns this instance's restart record — boot \
             step skipped for now; the notice loop claims the record and runs it if that \
             process exits"
        );
    }
    info!(step = ?step, "restart_prep: boot step");
}

/// How long the live boot step waits for the record's ownership lock — below
/// the 15 s `await_boot_read` bound its callers use (handoff, startup resume,
/// agent runtime, the MCP API), since it runs inside the boot read.
pub(crate) const BOOT_OWNERSHIP_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

/// The late claim on the live controller ([`RestartPrep::claim_late`]), run by
/// each notice-loop tick until this process owns the record.
pub async fn claim_late_live(ctl: &RestartPrep) {
    if let Some(step) = ctl.claim_late(&LiveDoor, running_build(), now_ms()).await {
        info!(step = ?step, "restart_prep: record claimed late — boot step run now");
    }
}

/// What [`RestartPrep::update_quiet`] compares: the record minus the fields a
/// tick moves every time it looks — the re-capture time and the verdict's
/// evaluation time.
fn material(rec: &PrepareRecord) -> PrepareRecord {
    let mut m = rec.clone();
    m.cohort.captured_at_ms = 0;
    m.cohort.captured_at.clear();
    if let Some(v) = m.verdict.as_mut() {
        v.evaluated_at_ms = 0;
        v.evaluated_at.clear();
        v.cohort_captured_at_ms = 0;
    }
    m
}

fn changed_payload(ctl: &RestartPrep, rec: PrepareRecord) -> PrepChanged {
    PrepChanged {
        view: view_of(&rec, ctl.same_build_restart(), now_ms()),
        record: rec,
    }
}

/// The Tauri `restart_prep_prepare` body.
pub async fn prepare_from_app(
    app: tauri::AppHandle,
    reason: String,
) -> Result<PrepChanged, String> {
    let ctl = install_live().ok_or("restart prepare is not available in this build")?;
    let rec = ctl
        .prepare(&LiveDoor, &reason, now_ms(), || async move {
            qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked(move || {
                super::live_cohort_from_app(&app)
            })
            .await
            .map_err(|e| format!("restart_prep cohort task failed: {e}"))?
        })
        .await?;
    // The first notice pass now rather than on the loop's next tick.
    tauri::async_runtime::spawn(super::notice::tick_live());
    Ok(changed_payload(ctl, rec))
}

/// The Tauri `restart_prep_current` body: the record and its view as this
/// process holds them — the same state every `restart-prep-changed` carries,
/// read from memory (which the notice loop keeps ahead of a failed disk
/// write), with no live cohort capture. `None`: no readable record.
pub fn current_from_app() -> Result<Option<PrepChanged>, String> {
    let ctl = install_live().ok_or("restart prepare is not available in this build")?;
    Ok(ctl.record().map(|rec| changed_payload(ctl, rec)))
}

/// The Tauri `restart_prep_extend` body.
pub async fn extend_from_app() -> Result<PrepChanged, String> {
    let ctl = install_live().ok_or("restart prepare is not available in this build")?;
    let rec = ctl.extend(&LiveDoor, now_ms()).await?;
    // The hold may have changed (a refused extension is unconfirmed): the
    // verdict is re-evaluated now rather than on the loop's next tick.
    tauri::async_runtime::spawn(super::notice::tick_live());
    Ok(changed_payload(ctl, rec))
}

/// The Tauri `restart_prep_cancel` body.
pub async fn cancel_from_app() -> Result<PrepChanged, String> {
    let ctl = install_live().ok_or("restart prepare is not available in this build")?;
    let rec = ctl.cancel(&LiveDoor, now_ms()).await?;
    // The sessions that were told to pause get one "resume your work" line
    // now, through the same two input arms; the notice loop retries any it
    // could not reach yet.
    tauri::async_runtime::spawn(super::notice::tick_live());
    Ok(changed_payload(ctl, rec))
}

#[cfg(test)]
impl RestartPrep {
    /// [`Self::update`] for tests outside this module; panics on a mismatch.
    pub(crate) fn update_for_test(&self, prepare_id: &str, f: impl FnOnce(&mut PrepareRecord)) {
        self.update(prepare_id, f).expect("update_for_test");
    }
}

/// A controller over a temp record holding an ACTIVE prepare whose cohort
/// carries `session_ids`, for the freeze tests of the callers
/// (`account_migration`, `claude_session`). The prepare started at `now_ms`.
#[cfg(test)]
pub(crate) fn active_controller_for_test(
    dir: &std::path::Path,
    session_ids: &[&str],
    now_ms: i64,
) -> RestartPrep {
    use crate::restart_prep::cohort::{build_row, RowFacts};
    use crate::session::session_ledger::LedgerEntry;
    let rows = session_ids
        .iter()
        .map(|id| {
            build_row(
                LedgerEntry {
                    claude_session_id: (*id).to_string(),
                    terminal_id: format!("term-{id}"),
                    page_id: "page-a".to_string(),
                    zone_index: 0,
                    title: None,
                    session_name: None,
                    account_label: None,
                    config_dir: None,
                    working_dir: None,
                    restorable: None,
                    worktree_path: None,
                    plan_slug: None,
                    work_unit_id: None,
                    wip_state: None,
                    wip_ref: None,
                    custody_session_mismatch: false,
                    name_source: None,
                    resume_config_dir: None,
                    resume_command: None,
                    provider: None,
                    last_seen_at: None,
                    finished: false,
                },
                RowFacts::default(),
            )
        })
        .collect();
    let path = dir.join(super::RECORD_FILE);
    let mut rec = PrepareRecord::new(
        "prep-test".to_string(),
        now_ms,
        Cohort {
            captured_at_ms: now_ms,
            captured_at: rfc3339(now_ms),
            reason: super::CAPTURE_REASON.to_string(),
            rows,
        },
    );
    rec.hold = HoldStatus::Confirmed;
    rec.hold_until_ms = Some(now_ms + DEFAULT_HOLD_MS);
    rec.hold_until = Some(rfc3339(now_ms + DEFAULT_HOLD_MS));
    persist_to(&path, &rec).expect("seed the record");
    RestartPrep::load(path, None)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coord_drain_state::{gate_with_local_hold, CoordDrainState, DrainGate, SpawnOrigin};
    use crate::restart_prep::cohort::{build_row, CohortRow, RowFacts};
    use crate::session::session_ledger::LedgerEntry;
    use std::sync::Arc;

    const T0: i64 = 1_790_000_000_000;

    /// A coord stand-in: the hold writes a drained record whose reason names
    /// the prepare, the release clears it, and every call is counted.
    struct MockDoor {
        role: HoldRole,
        hold_reply: Mutex<Option<HoldCall>>,
        release_reply: Mutex<Option<ReleaseCall>>,
        drain: Mutex<Result<DrainObservation, String>>,
        holds: Mutex<Vec<HoldRequest>>,
        releases: Mutex<Vec<String>>,
        /// Coord's `until` on a successful hold.
        until_reply: Mutex<Option<i64>>,
        /// Runs inside the hold call — stands for work that lands while an
        /// operation awaits coord.
        on_hold: Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
        /// Every read folded into the drain state, with the record's state ON
        /// DISK at that moment (when `record_at` names the record).
        published: Mutex<Vec<(String, Option<PrepState>)>>,
        record_at: Mutex<Option<PathBuf>>,
    }

    impl MockDoor {
        fn new(role: HoldRole) -> Arc<Self> {
            Arc::new(Self {
                role,
                hold_reply: Mutex::new(None),
                release_reply: Mutex::new(None),
                drain: Mutex::new(Ok(DrainObservation::Clear)),
                holds: Mutex::new(vec![]),
                releases: Mutex::new(vec![]),
                until_reply: Mutex::new(None),
                on_hold: Mutex::new(None),
                published: Mutex::new(vec![]),
                record_at: Mutex::new(None),
            })
        }
        fn primary() -> Arc<Self> {
            Self::new(HoldRole::Primary)
        }
        fn hold_count(&self) -> usize {
            self.holds.lock().unwrap().len()
        }
        fn release_count(&self) -> usize {
            self.releases.lock().unwrap().len()
        }
    }

    #[async_trait]
    impl HoldDoor for MockDoor {
        fn role(&self) -> HoldRole {
            self.role.clone()
        }
        async fn hold(&self, req: &HoldRequest) -> HoldCall {
            self.holds.lock().unwrap().push(req.clone());
            if let Some(hook) = self.on_hold.lock().unwrap().as_ref() {
                hook();
            }
            if let Some(reply) = self.hold_reply.lock().unwrap().clone() {
                return reply;
            }
            *self.drain.lock().unwrap() = Ok(DrainObservation::Drained {
                until: None,
                reason: Some(format!(
                    "{HOLD_REASON_PREFIX}{}: {}",
                    req.prepare_id, req.reason
                )),
            });
            HoldCall::Held {
                until_ms: *self.until_reply.lock().unwrap(),
            }
        }
        async fn release(&self, prepare_id: &str) -> ReleaseCall {
            self.releases.lock().unwrap().push(prepare_id.to_string());
            if let Some(reply) = self.release_reply.lock().unwrap().clone() {
                return reply;
            }
            *self.drain.lock().unwrap() = Ok(DrainObservation::Clear);
            ReleaseCall::Answered {
                outcome: "released".to_string(),
            }
        }
        async fn read_drain(&self) -> Result<DrainObservation, String> {
            self.drain.lock().unwrap().clone()
        }
        fn publish_read(&self, read: &Result<DrainObservation, String>) {
            let state = self
                .record_at
                .lock()
                .unwrap()
                .as_deref()
                .and_then(load_from)
                .map(|r| r.state);
            self.published
                .lock()
                .unwrap()
                .push((read_back_label(read), state));
        }
    }

    fn entry(id: &str) -> LedgerEntry {
        LedgerEntry {
            claude_session_id: id.to_string(),
            terminal_id: format!("term-{id}"),
            page_id: "page-a".to_string(),
            zone_index: 0,
            title: None,
            session_name: Some(format!("name-{id}")),
            account_label: Some("iris".to_string()),
            config_dir: Some("/home/u/.claude-iris".to_string()),
            working_dir: Some("/home/u/repo".to_string()),
            restorable: Some(true),
            worktree_path: None,
            plan_slug: None,
            work_unit_id: None,
            wip_state: None,
            wip_ref: None,
            custody_session_mismatch: false,
            name_source: None,
            resume_config_dir: None,
            resume_command: None,
            provider: None,
            last_seen_at: None,
            finished: false,
        }
    }

    fn cohort(ids: &[&str]) -> Cohort {
        Cohort {
            captured_at_ms: T0,
            captured_at: rfc3339(T0),
            reason: super::super::CAPTURE_REASON.to_string(),
            rows: ids
                .iter()
                .map(|id| build_row(entry(id), RowFacts::default()))
                .collect::<Vec<CohortRow>>(),
        }
    }

    fn controller(dir: &tempfile::TempDir) -> RestartPrep {
        RestartPrep::load(dir.path().join(super::super::RECORD_FILE), None)
    }

    async fn prepare(ctl: &RestartPrep, door: &MockDoor) -> PrepareRecord {
        ctl.prepare(door, "rebuild", T0, || async { Ok(cohort(&["s1", "s2"])) })
            .await
            .expect("prepare")
    }

    fn gate(
        ctl: &RestartPrep,
        state: &CoordDrainState,
        origin: SpawnOrigin,
        now: i64,
    ) -> DrainGate {
        let hold = ctl.local_hold_reason(now);
        gate_with_local_hold(
            state,
            origin,
            hold.as_deref(),
            chrono::DateTime::from_timestamp_millis(now).unwrap(),
        )
    }

    #[tokio::test]
    async fn restart_prep_prepare_twice_takes_one_hold() {
        let dir = tempfile::tempdir().unwrap();
        let ctl = controller(&dir);
        let door = MockDoor::primary();
        let first = prepare(&ctl, &door).await;
        assert_eq!(first.hold, HoldStatus::Confirmed, "{:?}", first.hold);
        let mut captures = 0;
        let second = ctl
            .prepare(&*door, "again", T0 + 1_000, || {
                captures += 1;
                async { Ok(cohort(&["other"])) }
            })
            .await
            .unwrap();
        assert_eq!(door.hold_count(), 1, "one hold, not two");
        assert_eq!(captures, 0, "an active prepare is not re-captured");
        assert_eq!(second.prepare_id, first.prepare_id);
        assert_eq!(second.cohort, first.cohort);
        assert_eq!(load_from(&ctl.path), Some(second));
    }

    #[tokio::test]
    async fn restart_prep_a_retried_unconfirmed_hold_reuses_the_prepare_id() {
        let dir = tempfile::tempdir().unwrap();
        let ctl = controller(&dir);
        let door = MockDoor::primary();
        *door.hold_reply.lock().unwrap() = Some(HoldCall::Unreachable {
            cause: "timed out".to_string(),
        });
        let first = prepare(&ctl, &door).await;
        assert!(matches!(first.hold, HoldStatus::Unconfirmed { .. }));
        *door.hold_reply.lock().unwrap() = None;
        let second = prepare(&ctl, &door).await;
        assert_eq!(second.hold, HoldStatus::Confirmed);
        let holds = door.holds.lock().unwrap();
        assert_eq!(holds.len(), 2);
        assert_eq!(
            holds[0].prepare_id, holds[1].prepare_id,
            "coord sees ONE hold"
        );
    }

    #[tokio::test]
    async fn restart_prep_coord_unreachable_reads_unconfirmed() {
        let dir = tempfile::tempdir().unwrap();
        let ctl = controller(&dir);
        let door = MockDoor::primary();
        *door.hold_reply.lock().unwrap() = Some(HoldCall::Unreachable {
            cause: "POST https://coord/coord/devices/me/hold: connection refused".to_string(),
        });
        let rec = prepare(&ctl, &door).await;
        match &rec.hold {
            HoldStatus::Unconfirmed { cause } => {
                assert!(cause.starts_with("coord_unreachable: "), "{cause}")
            }
            other => panic!("expected unconfirmed, got {other:?}"),
        }
        assert!(
            ctl.local_hold_reason(T0).is_some(),
            "an unconfirmed hold is backstopped locally: coord may still read clear"
        );
        assert!(!gate(&ctl, &CoordDrainState::Clear, SpawnOrigin::Steward, T0).allows());
        assert!(
            ctl.accounts_frozen(T0),
            "the freeze is keyed on the record, not the hold"
        );
    }

    #[tokio::test]
    async fn restart_prep_a_404_is_route_absent_never_success() {
        let dir = tempfile::tempdir().unwrap();
        let ctl = controller(&dir);
        let door = MockDoor::primary();
        *door.hold_reply.lock().unwrap() = Some(parse_hold_reply(404, "Not Found"));
        let rec = prepare(&ctl, &door).await;
        assert_eq!(
            rec.hold,
            HoldStatus::Unconfirmed {
                cause: "route_absent".to_string()
            }
        );
        assert_eq!(
            parse_release_reply(404, ""),
            ReleaseCall::RouteAbsent,
            "the release's 404 is route_absent too"
        );
    }

    #[tokio::test]
    async fn restart_prep_a_lane_held_by_another_owner_is_unconfirmed_by_its_code() {
        let dir = tempfile::tempdir().unwrap();
        let ctl = controller(&dir);
        let door = MockDoor::primary();
        let body = r#"{"error":"lane_held_by_other","detail":"…","lane":"agent","lane_outcome":"not_owner","read_back":{"state":"drained"}}"#;
        *door.hold_reply.lock().unwrap() = Some(parse_hold_reply(409, body));
        let rec = prepare(&ctl, &door).await;
        assert_eq!(rec.hold.label(), "unconfirmed: lane_held_by_other");
    }

    #[tokio::test]
    async fn restart_prep_a_read_back_naming_another_prepare_is_not_confirmed() {
        let dir = tempfile::tempdir().unwrap();
        let ctl = controller(&dir);
        let door = MockDoor::primary();
        *door.hold_reply.lock().unwrap() = Some(HoldCall::Held { until_ms: None });
        // An id that PREFIXES nothing: `restart-hold:<id>1:` must not pass for
        // `restart-hold:<id>:`.
        *door.drain.lock().unwrap() = Ok(DrainObservation::Drained {
            until: None,
            reason: Some("operator maintenance".to_string()),
        });
        let rec = prepare(&ctl, &door).await;
        match &rec.hold {
            HoldStatus::Unconfirmed { cause } => {
                assert!(cause.starts_with("read_back_not_this_hold"), "{cause}")
            }
            other => panic!("{other:?}"),
        }
        let near = Ok(DrainObservation::Drained {
            until: None,
            reason: Some(format!("{HOLD_REASON_PREFIX}{}1: x", rec.prepare_id)),
        });
        assert!(confirms_hold(&near, &rec.prepare_id).is_err());
        let exact = Ok(DrainObservation::Drained {
            until: None,
            reason: Some(format!("{HOLD_REASON_PREFIX}{}: x", rec.prepare_id)),
        });
        assert!(confirms_hold(&exact, &rec.prepare_id).is_ok());
        assert!(confirms_hold(&Ok(DrainObservation::Clear), &rec.prepare_id).is_err());
    }

    #[tokio::test]
    async fn restart_prep_coord_until_replaces_the_default() {
        let dir = tempfile::tempdir().unwrap();
        let ctl = controller(&dir);
        let door = MockDoor::primary();
        let rec = prepare(&ctl, &door).await;
        assert_eq!(rec.hold_until_ms, Some(T0 + DEFAULT_HOLD_MS));
        let sent = door.holds.lock().unwrap()[0].clone();
        assert_eq!(sent.until, rec.hold_until, "the default is sent explicitly");
        assert_eq!(sent.reason, "rebuild");
        let body = serde_json::to_value(&sent).unwrap();
        let keys: Vec<&String> = body.as_object().unwrap().keys().collect();
        assert_eq!(
            keys,
            ["prepare_id", "reason", "until"],
            "coord is deny_unknown_fields"
        );

        // Coord's own `until` wins once it answers one.
        let dir = tempfile::tempdir().unwrap();
        let ctl = controller(&dir);
        let door = MockDoor::primary();
        *door.until_reply.lock().unwrap() = Some(T0 + 3_600_000);
        let rec = prepare(&ctl, &door).await;
        assert_eq!(rec.hold_until_ms, Some(T0 + 3_600_000));
        assert_eq!(rec.hold_until, Some(rfc3339(T0 + 3_600_000)));

        let reply = r#"{"outcome":"restored","changed":true,"until":"2026-10-21T18:00:00+00:00"}"#;
        assert_eq!(
            parse_hold_reply(200, reply),
            HoldCall::Held {
                until_ms: Some(
                    chrono::DateTime::parse_from_rfc3339("2026-10-21T18:00:00+00:00")
                        .unwrap()
                        .timestamp_millis()
                )
            }
        );
    }

    /// Phase 6: on a runner REBUILT since the prepare whose boot step could
    /// not settle the release, the operator's Release hold moves the cohort to
    /// `restoring` — placeholders, one at a time — not `cancelled`, which
    /// would hand every cohort row to the auto-restore at once (D6).
    #[tokio::test]
    async fn release_hold_on_a_rebuilt_runner_starts_the_restore_instead_of_cancelling() {
        let dir = tempfile::tempdir().unwrap();
        let ctl = controller(&dir);
        let door = MockDoor::primary();
        let rec = prepare(&ctl, &door).await;
        ctl.update(&rec.prepare_id, |r| {
            r.prepared_build = "an-older-build".to_string()
        })
        .unwrap();
        let released = ctl.cancel(&*door, T0 + 5_000).await.unwrap();
        assert_eq!(released.state, PrepState::Restoring);
        assert_eq!(released.hold.label(), "release: released");
        assert!(!ctl.accounts_frozen(T0 + 5_000), "the freeze is lifted");
        assert!(
            crate::restart_prep::restore::RestoringCohort::of(&released).is_some(),
            "the cohort is restored by hand"
        );
    }

    /// Review round 4: the same-build flag belongs to the record the boot step
    /// saw. A later prepare in the same process, cancelled before any restart,
    /// is a true cancel — its told sessions get the resume line.
    #[tokio::test]
    async fn a_prepare_after_a_same_build_restart_cancels_as_a_true_cancel() {
        let dir = tempfile::tempdir().unwrap();
        let ctl = controller(&dir);
        let door = MockDoor::primary();
        prepare(&ctl, &door).await;
        ctl.on_boot(&*door, running_build(), T0 + 1_000).await;
        assert!(ctl.same_build_restart());
        let first = ctl.cancel(&*door, T0 + 2_000).await.unwrap();
        assert_eq!(first.state, PrepState::Restoring);
        assert!(!ctl.same_build_restart(), "the flag left with its record");
        // A restoring cohort is the operator's to restore or dismiss before a
        // new prepare may replace it (Phase 5 review round 2).
        crate::restart_prep::restore::dismiss_on(&ctl, &first.prepare_id, &|_| {}).unwrap();
        let second = ctl
            .prepare(&*door, "again", T0 + 3_000, || async {
                Ok(cohort(&["s1"]))
            })
            .await
            .unwrap();
        assert_ne!(second.prepare_id, first.prepare_id);
        assert!(!ctl.view(T0 + 3_000).unwrap().same_build_hold);
        let cancelled = ctl.cancel(&*door, T0 + 4_000).await.unwrap();
        assert_eq!(cancelled.state, PrepState::Cancelled);
    }

    /// A same-build Release hold still owes the resume line to the told
    /// sessions that survived the restart; a release on a rebuilt runner owes
    /// nothing (every session ended with the old build).
    #[tokio::test]
    async fn a_same_build_release_owes_the_resume_line_and_a_rebuilt_one_does_not() {
        let dir = tempfile::tempdir().unwrap();
        let ctl = controller(&dir);
        let door = MockDoor::primary();
        let rec = prepare(&ctl, &door).await;
        ctl.update(&rec.prepare_id, |r| {
            r.cohort.rows[0].notified_at = Some(T0 + 10)
        })
        .unwrap();
        ctl.on_boot(&*door, running_build(), T0 + 1_000).await;
        let released = ctl.cancel(&*door, T0 + 2_000).await.unwrap();
        assert_eq!(released.state, PrepState::Restoring);
        assert!(RestartPrep::owes_resume(&released, T0 + 2_001));

        let dir = tempfile::tempdir().unwrap();
        let ctl = controller(&dir);
        let rec = prepare(&ctl, &door).await;
        ctl.update(&rec.prepare_id, |r| {
            r.cohort.rows[0].notified_at = Some(T0 + 10);
            r.prepared_build = "an-older-build".to_string();
        })
        .unwrap();
        let rebuilt = ctl.cancel(&*door, T0 + 2_000).await.unwrap();
        assert_eq!(rebuilt.state, PrepState::Restoring);
        assert!(!RestartPrep::owes_resume(&rebuilt, T0 + 2_001));
    }

    /// Review round 5: Release hold pressed after a same-build restart but
    /// BEFORE the boot step ran still starts the hand restore — the record
    /// names the process that prepared, and this is another one.
    #[tokio::test]
    async fn a_release_before_the_boot_step_after_a_restart_starts_the_restore() {
        let dir = tempfile::tempdir().unwrap();
        let ctl = controller(&dir);
        let door = MockDoor::primary();
        let rec = prepare(&ctl, &door).await;
        assert!(!rec.restarted_since(), "the process that prepared");
        ctl.update(&rec.prepare_id, |r| {
            r.prepared_process = Some("a-previous-runner-process".to_string())
        })
        .unwrap();
        assert!(!ctl.same_build_restart(), "no boot step has run");
        let released = ctl.cancel(&*door, T0 + 5_000).await.unwrap();
        assert_eq!(released.state, PrepState::Restoring);
    }

    #[tokio::test]
    async fn restart_prep_cancel_releases_and_reports_the_outcome_verbatim() {
        let dir = tempfile::tempdir().unwrap();
        let ctl = controller(&dir);
        let door = MockDoor::primary();
        let rec = prepare(&ctl, &door).await;
        let cancelled = ctl.cancel(&*door, T0 + 5_000).await.unwrap();
        assert_eq!(
            door.releases.lock().unwrap().as_slice(),
            [rec.prepare_id.clone()]
        );
        assert_eq!(cancelled.state, PrepState::Cancelled);
        assert_eq!(cancelled.hold.label(), "release: released");
        let report = cancelled.release.as_ref().unwrap();
        assert_eq!(report.trigger, ReleaseTrigger::Cancel);
        assert_eq!(report.read_back.as_deref(), Some("clear"));
        assert!(!ctl.accounts_frozen(T0 + 5_000), "cancel lifts the freeze");
        assert!(
            ctl.cancel(&*door, T0 + 6_000).await.is_err(),
            "nothing left to cancel"
        );

        // Every coord outcome is carried as given — never collapsed to ok.
        for outcome in ["not_owner", "absent", "unreadable"] {
            let dir = tempfile::tempdir().unwrap();
            let ctl = controller(&dir);
            let door = MockDoor::primary();
            prepare(&ctl, &door).await;
            *door.release_reply.lock().unwrap() = Some(parse_release_reply(
                200,
                &format!(r#"{{"outcome":"{outcome}","changed":false}}"#),
            ));
            let rec = ctl.cancel(&*door, T0 + 1).await.unwrap();
            assert_eq!(rec.hold.label(), format!("release: {outcome}"));
            assert_eq!(rec.release.unwrap().outcome, outcome);
        }
        assert!(matches!(
            parse_release_reply(200, r#"{"changed":true}"#),
            ReleaseCall::Refused { .. }
        ));
    }

    fn seeded(
        dir: &tempfile::TempDir,
        build: &str,
        hold: HoldStatus,
        until_ms: i64,
    ) -> RestartPrep {
        let path = dir.path().join(super::super::RECORD_FILE);
        let mut rec = PrepareRecord::new("prep-boot".to_string(), T0, cohort(&["s1"]));
        rec.prepared_build = build.to_string();
        rec.hold = hold;
        rec.hold_until_ms = Some(until_ms);
        rec.hold_until = Some(rfc3339(until_ms));
        persist_to(&path, &rec).unwrap();
        RestartPrep::load(path, None)
    }

    #[tokio::test]
    async fn restart_prep_a_different_build_boot_releases_the_hold() {
        let dir = tempfile::tempdir().unwrap();
        let ctl = seeded(
            &dir,
            "an-older-build",
            HoldStatus::Confirmed,
            T0 + DEFAULT_HOLD_MS,
        );
        let door = MockDoor::primary();
        let step = ctl.on_boot(&*door, running_build(), T0 + 60_000).await;
        assert_eq!(
            step,
            BootStep::ReleasedForRebuild {
                outcome: "released".to_string()
            }
        );
        assert_eq!(door.releases.lock().unwrap().as_slice(), ["prep-boot"]);
        let rec = load_from(&ctl.path).unwrap();
        assert_eq!(rec.state, PrepState::Restoring);
        assert_eq!(
            rec.release.as_ref().unwrap().trigger,
            ReleaseTrigger::Rebuild
        );
        assert_eq!(rec.release.as_ref().unwrap().on_build, running_build());
        assert!(!ctl.same_build_restart());
        assert!(!ctl.accounts_frozen(T0 + 60_000));
    }

    #[tokio::test]
    async fn restart_prep_a_same_build_boot_keeps_the_hold() {
        let dir = tempfile::tempdir().unwrap();
        let ctl = seeded(
            &dir,
            running_build(),
            HoldStatus::Confirmed,
            T0 + DEFAULT_HOLD_MS,
        );
        let door = MockDoor::primary();
        let step = ctl.on_boot(&*door, running_build(), T0 + 60_000).await;
        assert_eq!(step, BootStep::SameBuildHeld { expired: false });
        assert_eq!(door.release_count(), 0, "the same build releases nothing");
        assert_eq!(load_from(&ctl.path).unwrap().state, PrepState::Preparing);
        let view = ctl.view(T0 + 60_000).unwrap();
        assert!(view.same_build_hold, "the banner flag is raised");
        assert_eq!(view.hold, "confirmed");
        // Release hold IS the cancel path — but after a restart the sessions
        // died with it, so it starts the hand restore (Phase 6, D6), never the
        // auto-restore of every row at once that `cancelled` would hand them to.
        let rec = ctl.cancel(&*door, T0 + 70_000).await.unwrap();
        assert_eq!(rec.state, PrepState::Restoring);
        assert_eq!(door.release_count(), 1);
        assert!(!ctl.view(T0 + 70_000).unwrap().same_build_hold);
    }

    #[tokio::test]
    async fn restart_prep_a_hold_past_its_until_reads_expired_never_held() {
        let dir = tempfile::tempdir().unwrap();
        let until = T0 + 1_000;
        let ctl = seeded(&dir, running_build(), HoldStatus::Local, until);
        let step = ctl
            .on_boot(&*MockDoor::primary(), running_build(), until + 1)
            .await;
        assert_eq!(step, BootStep::SameBuildHeld { expired: true });
        let view = ctl.view(until + 1).unwrap();
        assert_eq!(view.hold, format!("hold expired at {}", rfc3339(until)));
        assert_eq!(view.hold_expired_at, Some(rfc3339(until)));
        assert!(!view.active && !view.local_hold && !view.same_build_hold && !view.accounts_frozen);
        assert!(ctl.local_hold_reason(until + 1).is_none());
        assert!(
            ctl.local_hold_reason(until - 1).is_some(),
            "held until then"
        );
    }

    #[tokio::test]
    async fn restart_prep_not_enrolled_holds_locally_and_defers_autonomous_spawns() {
        let dir = tempfile::tempdir().unwrap();
        let ctl = controller(&dir);
        let door = MockDoor::new(HoldRole::NotEnrolled {
            why: "~/.qontinui/machine.json is missing".to_string(),
        });
        let not_enrolled = CoordDrainState::NotEnrolled { why: "no coord" };
        assert!(gate(&ctl, &not_enrolled, SpawnOrigin::LoopingAgent, T0).allows());

        let rec = prepare(&ctl, &door).await;
        assert_eq!(rec.hold.label(), "local");
        assert_eq!(door.hold_count(), 0, "no coord hold without coord");
        match gate(&ctl, &not_enrolled, SpawnOrigin::LoopingAgent, T0 + 1) {
            DrainGate::Defer { reason, .. } => {
                assert!(reason.contains("restart prepare"), "{reason}")
            }
            DrainGate::Allow => panic!("an autonomous spawn must be deferred under the local hold"),
        }
        assert!(
            gate(&ctl, &not_enrolled, SpawnOrigin::OperatorTerminal, T0 + 1).allows(),
            "the operator is never held"
        );
        ctl.cancel(&*door, T0 + 2).await.unwrap();
        assert_eq!(door.release_count(), 0);
        assert!(gate(&ctl, &not_enrolled, SpawnOrigin::LoopingAgent, T0 + 3).allows());
    }

    #[tokio::test]
    async fn restart_prep_a_secondary_writes_no_coord_hold_and_holds_its_own_spawns() {
        let dir = tempfile::tempdir().unwrap();
        let ctl = controller(&dir);
        let door = MockDoor::new(HoldRole::Secondary);
        let rec = prepare(&ctl, &door).await;
        assert_eq!(rec.hold.label(), "local (secondary)");
        assert_eq!(
            door.hold_count(),
            0,
            "a secondary never drains the shared device"
        );
        // A secondary reads the device's drain, which the primary keeps clear.
        assert!(!gate(
            &ctl,
            &CoordDrainState::Clear,
            SpawnOrigin::Scheduler,
            T0 + 1
        )
        .allows());
        // Its boot on a new build lifts the local hold without a coord release.
        let step = ctl.on_boot(&*door, "a-newer-build", T0 + 2).await;
        assert_eq!(
            step,
            BootStep::ReleasedForRebuild {
                outcome: "local".to_string()
            }
        );
        assert_eq!(door.release_count(), 0);
        assert!(gate(
            &ctl,
            &CoordDrainState::Clear,
            SpawnOrigin::Scheduler,
            T0 + 3
        )
        .allows());
    }

    /// E2E: a second runner process on the same instance root (it later
    /// failed to bind the port) ran the boot step against the shared record.
    /// Only the process that owns the record's lock may: the other changes
    /// nothing and reports `NotOwner`.
    #[tokio::test]
    async fn restart_prep_only_the_record_owner_runs_the_boot_step() {
        let dir = tempfile::tempdir().unwrap();
        let serving = controller(&dir);
        let door = MockDoor::new(HoldRole::Secondary);
        prepare(&serving, &door).await;
        assert!(
            serving.claim_ownership(),
            "the serving process owns the record"
        );
        let before = serving.record().unwrap();

        // The duplicate process loads the same record file.
        let duplicate = RestartPrep::load(dir.path().join(super::super::RECORD_FILE), None);
        let step = duplicate.on_boot(&*door, "a-newer-build", T0 + 2).await;
        assert_eq!(step, BootStep::NotOwner);
        assert!(!duplicate.owns_record());
        assert_eq!(
            crate::restart_prep::load_from(&dir.path().join(super::super::RECORD_FILE)).unwrap(),
            before,
            "the shared record is untouched"
        );
        // The owner's own boot step still runs.
        assert_eq!(
            serving.on_boot(&*door, "a-newer-build", T0 + 3).await,
            BootStep::ReleasedForRebuild {
                outcome: "local".to_string()
            }
        );
        // Once the owner is gone, the next process can own it.
        drop(serving);
        assert!(duplicate.claim_ownership());
    }

    /// Review 5a: the previous process freed the port but held the record's
    /// lock past the boot wait, so the serving runner's boot step was skipped.
    /// Once that process is gone the serving runner claims the record late and
    /// runs the skipped boot step — the hold is released, the cohort restores.
    #[tokio::test]
    async fn restart_prep_a_late_claim_runs_the_skipped_boot_step() {
        let dir = tempfile::tempdir().unwrap();
        let old = controller(&dir);
        let door = MockDoor::new(HoldRole::Secondary);
        prepare(&old, &door).await;
        assert!(old.claim_ownership());
        let serving = RestartPrep::load(dir.path().join(super::super::RECORD_FILE), None);
        assert_eq!(
            serving.on_boot(&*door, "a-newer-build", T0 + 2).await,
            BootStep::NotOwner
        );
        // Still held by the old process: nothing yet.
        assert_eq!(
            serving.claim_late(&*door, "a-newer-build", T0 + 3).await,
            None
        );
        assert_eq!(serving.record().unwrap().state, PrepState::Preparing);
        drop(old);
        assert_eq!(
            serving.claim_late(&*door, "a-newer-build", T0 + 4).await,
            Some(BootStep::ReleasedForRebuild {
                outcome: "local".to_string()
            })
        );
        assert_eq!(serving.record().unwrap().state, PrepState::Restoring);
        // Owned now: later ticks do nothing.
        assert_eq!(
            serving.claim_late(&*door, "a-newer-build", T0 + 5).await,
            None
        );
    }

    /// Nit 6: a lock that cannot be taken at all (here: its directory is a
    /// FILE, so the lock file cannot be opened) is not another process — the
    /// controller falls back to acting, as before the lock existed.
    #[test]
    fn restart_prep_an_untakeable_lock_is_not_a_duplicate_process() {
        let dir = tempfile::tempdir().unwrap();
        let not_a_dir = dir.path().join("a-file");
        std::fs::write(&not_a_dir, b"x").unwrap();
        let ctl = RestartPrep::load(not_a_dir.join(super::super::RECORD_FILE), None);
        assert!(ctl.claim_ownership());
        assert!(ctl.owns_record());
    }

    /// Review 5b: the boot wait stays under the 15 s `await_boot_read` bound.
    #[test]
    fn restart_prep_the_boot_ownership_wait_is_below_the_boot_read_bound() {
        assert!(super::BOOT_OWNERSHIP_WAIT < std::time::Duration::from_secs(15));
    }

    #[tokio::test]
    async fn restart_prep_freeze_marks_limit_hit_only_while_active() {
        let dir = tempfile::tempdir().unwrap();
        let ctl = controller(&dir);
        assert_eq!(
            ctl.freeze_hit("s1", Some(9), T0),
            FreezeHit::NotFrozen,
            "no prepare"
        );
        let door = MockDoor::primary();
        prepare(&ctl, &door).await;
        assert_eq!(
            ctl.freeze_hit("s2", Some(T0 + 3_600_000), T0 + 1),
            FreezeHit::Frozen { row_marked: true }
        );
        assert_eq!(
            ctl.freeze_hit("chat-session", None, T0 + 1),
            FreezeHit::Frozen { row_marked: false }
        );
        let on_disk = load_from(&ctl.path).unwrap();
        let s2 = &on_disk.cohort.rows[1];
        assert_eq!(s2.session.claude_session_id, "s2");
        assert_eq!(s2.state_at_prepare, StateAtPrepare::LimitHit);
        assert_eq!(s2.limit_reset_at, Some(T0 + 3_600_000));
        assert_ne!(
            on_disk.cohort.rows[0].state_at_prepare,
            StateAtPrepare::LimitHit
        );
        ctl.cancel(&*door, T0 + 2).await.unwrap();
        assert_eq!(ctl.freeze_hit("s1", None, T0 + 3), FreezeHit::NotFrozen);
    }

    #[tokio::test]
    async fn restart_prep_a_release_coord_did_not_answer_keeps_the_prepare_in_force() {
        let dir = tempfile::tempdir().unwrap();
        let ctl = controller(&dir);
        let door = MockDoor::primary();
        prepare(&ctl, &door).await;
        *door.release_reply.lock().unwrap() = Some(ReleaseCall::Unreachable {
            cause: "timed out".to_string(),
        });
        let err = ctl.cancel(&*door, T0 + 1).await.unwrap_err();
        assert!(err.contains("coord_unreachable"), "{err}");
        let rec = load_from(&ctl.path).unwrap();
        assert_eq!(rec.state, PrepState::Preparing, "the hold still stands");
        assert_eq!(rec.hold, HoldStatus::Confirmed);
        let view = ctl.view(T0 + 1).unwrap();
        assert_eq!(
            view.release_failed.as_deref(),
            Some("coord_unreachable: timed out")
        );
        assert!(view.accounts_frozen);
        // Release hold again, now answered.
        *door.release_reply.lock().unwrap() = None;
        let rec = ctl.cancel(&*door, T0 + 2).await.unwrap();
        assert_eq!(rec.state, PrepState::Cancelled);
        assert_eq!(door.release_count(), 2);
        assert_eq!(ctl.view(T0 + 2).unwrap().release_failed, None);

        // The rebuilt runner's boot, the same way.
        let dir = tempfile::tempdir().unwrap();
        let ctl = seeded(
            &dir,
            "an-older-build",
            HoldStatus::Confirmed,
            T0 + DEFAULT_HOLD_MS,
        );
        let door = MockDoor::primary();
        *door.release_reply.lock().unwrap() = Some(parse_release_reply(404, ""));
        assert_eq!(
            ctl.on_boot(&*door, running_build(), T0 + 1).await,
            BootStep::ReleaseFailed {
                outcome: "route_absent".to_string()
            }
        );
        assert_eq!(load_from(&ctl.path).unwrap().state, PrepState::Preparing);
        assert!(!release_settled("refused: HTTP 500: boom", false));
        assert!(!release_settled(
            "enrollment_indeterminate: machine.json unreadable",
            false
        ));
        assert!(release_settled(
            "not_sent: a secondary instance never writes the device's drain",
            false
        ));
        assert!(release_settled("not_owner", true) && release_settled("local", true));
        assert!(
            !release_settled("route_absent", true),
            "a confirmed coord hold is not let go by a 404"
        );
    }

    #[tokio::test]
    async fn restart_prep_a_hold_the_record_never_learned_of_is_still_released() {
        // A crash between coord's 200 and the write that records it.
        let dir = tempfile::tempdir().unwrap();
        let ctl = seeded(
            &dir,
            "an-older-build",
            HoldStatus::NotAttempted,
            T0 + DEFAULT_HOLD_MS,
        );
        let door = MockDoor::primary();
        let step = ctl.on_boot(&*door, running_build(), T0 + 1).await;
        assert_eq!(door.releases.lock().unwrap().as_slice(), ["prep-boot"]);
        assert_eq!(
            step,
            BootStep::ReleasedForRebuild {
                outcome: "released".to_string()
            }
        );
    }

    #[tokio::test]
    async fn restart_prep_a_same_build_boot_retries_a_hold_that_never_took() {
        let dir = tempfile::tempdir().unwrap();
        let ctl = seeded(
            &dir,
            running_build(),
            HoldStatus::Unconfirmed {
                cause: "coord_unreachable: x".to_string(),
            },
            T0 + DEFAULT_HOLD_MS,
        );
        let door = MockDoor::primary();
        let step = ctl.on_boot(&*door, running_build(), T0 + 1).await;
        assert_eq!(step, BootStep::SameBuildHeld { expired: false });
        assert_eq!(door.hold_count(), 1);
        assert_eq!(door.holds.lock().unwrap()[0].prepare_id, "prep-boot");
        assert_eq!(load_from(&ctl.path).unwrap().hold, HoldStatus::Confirmed);
        assert!(ctl.view(T0 + 1).unwrap().same_build_hold);
    }

    #[tokio::test]
    async fn restart_prep_a_confirmed_retry_clears_a_stale_failed_release() {
        let dir = tempfile::tempdir().unwrap();
        let ctl = controller(&dir);
        let door = MockDoor::primary();
        *door.hold_reply.lock().unwrap() = Some(HoldCall::Unreachable {
            cause: "x".to_string(),
        });
        prepare(&ctl, &door).await;
        *door.release_reply.lock().unwrap() = Some(ReleaseCall::Unreachable {
            cause: "y".to_string(),
        });
        assert!(ctl.cancel(&*door, T0 + 1).await.is_err());
        assert!(ctl.view(T0 + 1).unwrap().release_failed.is_some());
        *door.hold_reply.lock().unwrap() = None;
        let rec = prepare(&ctl, &door).await;
        assert_eq!(rec.hold, HoldStatus::Confirmed);
        assert_eq!(rec.release, None);
        assert_eq!(ctl.view(T0 + 2).unwrap().release_failed, None);
    }

    #[tokio::test]
    async fn restart_prep_a_limit_hit_marked_while_the_hold_awaits_coord_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let ctl = Arc::new(controller(&dir));
        let door = MockDoor::primary();
        *door.hold_reply.lock().unwrap() = Some(HoldCall::Unreachable {
            cause: "x".to_string(),
        });
        prepare(&ctl, &door).await;
        *door.hold_reply.lock().unwrap() = None;
        let peer = Arc::clone(&ctl);
        *door.on_hold.lock().unwrap() = Some(Box::new(move || {
            assert_eq!(
                peer.freeze_hit("s1", Some(T0 + 99), T0 + 1),
                FreezeHit::Frozen { row_marked: true }
            );
        }));
        let rec = prepare(&ctl, &door).await;
        assert_eq!(rec.hold, HoldStatus::Confirmed);
        let s1 = &load_from(&ctl.path).unwrap().cohort.rows[0];
        assert_eq!(
            s1.state_at_prepare,
            StateAtPrepare::LimitHit,
            "not overwritten"
        );
        assert_eq!(s1.limit_reset_at, Some(T0 + 99));
    }

    #[test]
    fn restart_prep_reason_is_bounded_printable_and_never_blank() {
        assert_eq!(sanitize_reason("   "), DEFAULT_REASON);
        assert_eq!(sanitize_reason("a\nb\tc"), "a b c");
        let long = "é".repeat(400);
        let cut = sanitize_reason(&long);
        assert!(cut.len() <= MAX_REASON_BYTES && !cut.is_empty());
    }

    #[test]
    fn restart_prep_the_view_spells_each_hold_for_the_panel() {
        let mut rec = PrepareRecord::new("p".to_string(), T0, cohort(&[]));
        rec.hold_until_ms = Some(T0 + 10);
        rec.hold_until = Some(rfc3339(T0 + 10));
        rec.hold = HoldStatus::Unconfirmed {
            cause: "route_absent".to_string(),
        };
        let v = view_of(&rec, false, T0);
        assert_eq!(v.hold, "unconfirmed: route_absent");
        assert!(v.active && v.accounts_frozen && v.local_hold, "{v:?}");
        assert!(
            !view_of(&rec, true, T0 + 10).same_build_hold,
            "expired: nothing holds"
        );
        rec.hold = HoldStatus::NotAttempted;
        assert!(
            !view_of(&rec, true, T0).same_build_hold,
            "no hold taken: not 'held'"
        );
        rec.state = PrepState::Cancelled;
        let v = view_of(&rec, true, T0 + 100);
        assert!(!v.active && !v.same_build_hold);
        assert_eq!(
            v.hold_expired_at, None,
            "a cancelled record is not 'expired'"
        );
    }

    #[tokio::test]
    async fn restart_prep_extend_moves_the_coord_hold_s_until_and_reads_it_back() {
        let dir = tempfile::tempdir().unwrap();
        let ctl = controller(&dir);
        let door = MockDoor::primary();
        let rec = prepare(&ctl, &door).await;
        assert_eq!(rec.hold, HoldStatus::Confirmed);
        let now = T0 + 60_000;
        let ext = ctl.extend(&*door, now).await.unwrap();
        let want = T0 + 2 * DEFAULT_HOLD_MS;
        assert_eq!(ext.hold_until_ms, Some(want), "6 h past the current until");
        assert_eq!(ext.hold, HoldStatus::Confirmed);
        let holds = door.holds.lock().unwrap().clone();
        assert_eq!(holds.len(), 2, "the extension is one more hold call");
        assert_eq!(
            holds[1].prepare_id, rec.prepare_id,
            "a repeat of the same hold"
        );
        assert_eq!(holds[1].until, Some(rfc3339(want)));
        assert_eq!(
            super::super::load_from(&dir.path().join(super::super::RECORD_FILE))
                .unwrap()
                .hold_until_ms,
            Some(want),
            "persisted"
        );
        // Coord's own answer wins over the requested until.
        *door.until_reply.lock().unwrap() = Some(T0 + DEFAULT_HOLD_MS + 1_000);
        let ext = ctl.extend(&*door, now).await.unwrap();
        assert_eq!(ext.hold_until_ms, Some(T0 + DEFAULT_HOLD_MS + 1_000));
        // A refused extension is not reported as confirmed.
        *door.hold_reply.lock().unwrap() = Some(HoldCall::Unreachable {
            cause: "timeout".to_string(),
        });
        let before = ctl.record().unwrap().hold_until_ms;
        let ext = ctl.extend(&*door, now).await.unwrap();
        assert!(
            matches!(ext.hold, HoldStatus::Unconfirmed { .. }),
            "{:?}",
            ext.hold
        );
        assert!(
            ext.hold_until_ms > before,
            "the runner-local backstop under an unconfirmed hold is extended"
        );
        assert!(
            ctl.local_hold_reason(T0 + 12 * 60 * 60 * 1000).is_some(),
            "and spawns stay held past the old expiry"
        );
    }

    #[tokio::test]
    async fn restart_prep_extend_renews_an_expired_local_hold_from_now_and_stops_at_the_ceiling() {
        let dir = tempfile::tempdir().unwrap();
        let until = T0 + 1_000;
        let ctl = seeded(&dir, running_build(), HoldStatus::Local, until);
        let door = MockDoor::new(HoldRole::NotEnrolled {
            why: "test".to_string(),
        });
        let now = T0 + 10_000;
        assert!(!ctl.record().unwrap().is_active(now), "expired");
        let ext = ctl.extend(&*door, now).await.unwrap();
        assert_eq!(
            ext.hold_until_ms,
            Some(now + DEFAULT_HOLD_MS),
            "renewed from now"
        );
        assert_eq!(ext.hold, HoldStatus::Local);
        assert!(ext.is_active(now), "active again");
        assert_eq!(door.hold_count(), 0, "a local hold calls no coord");
        // Repeated extensions stop at the ceiling, then refuse.
        let mut last = ext;
        for _ in 0..200 {
            match ctl.extend(&*door, now).await {
                Ok(r) => last = r,
                Err(e) => {
                    assert!(e.contains("longest"), "{e}");
                    break;
                }
            }
        }
        assert_eq!(last.hold_until_ms, Some(now + MAX_HOLD_MS));
        assert!(ctl.extend(&*door, now).await.is_err());
    }

    #[tokio::test]
    async fn restart_prep_extend_refuses_on_a_rebuilt_runner() {
        let dir = tempfile::tempdir().unwrap();
        let ctl = seeded(
            &dir,
            "an-older-build",
            HoldStatus::Confirmed,
            T0 + DEFAULT_HOLD_MS,
        );
        let door = MockDoor::primary();
        let err = ctl.extend(&*door, T0).await.unwrap_err();
        assert!(err.contains("rebuilt"), "{err}");
        assert_eq!(door.hold_count(), 0, "no longer drain is requested");
        assert_eq!(
            ctl.record().unwrap().hold_until_ms,
            Some(T0 + DEFAULT_HOLD_MS)
        );
    }

    /// Finding 5 (review): with coord's self-hold route not deployed, the hold
    /// POST answers `404` and the hold reads `unconfirmed: route_absent`. No
    /// coord hold of ours exists, so Release hold must settle without a coord
    /// write — the runner-local backstop lifts — instead of erring forever.
    #[tokio::test]
    async fn restart_prep_a_route_absent_hold_cancels_without_a_coord_write() {
        let dir = tempfile::tempdir().unwrap();
        let ctl = controller(&dir);
        let door = MockDoor::primary();
        *door.hold_reply.lock().unwrap() = Some(HoldCall::RouteAbsent);
        *door.release_reply.lock().unwrap() = Some(ReleaseCall::RouteAbsent);
        let rec = prepare(&ctl, &door).await;
        assert_eq!(rec.hold.label(), "unconfirmed: route_absent");
        assert!(
            ctl.local_hold_reason(T0 + 1).is_some(),
            "the backstop holds"
        );
        let rec = ctl
            .cancel(&*door, T0 + 2)
            .await
            .expect("the release settles");
        assert_eq!(rec.state, PrepState::Cancelled);
        assert_eq!(
            door.release_count(),
            0,
            "no coord write for a hold never taken"
        );
        assert!(
            ctl.local_hold_reason(T0 + 3).is_none(),
            "the backstop lifts"
        );
        assert!(rec.release.unwrap().outcome.starts_with("not_sent: "));
    }

    /// Finding 5: a rebuilt runner whose prepare's hold coord never confirmed,
    /// and whose release coord answers `404`, settles and starts the restore.
    #[tokio::test]
    async fn restart_prep_a_rebuild_settles_a_route_absent_release_of_an_unconfirmed_hold() {
        let dir = tempfile::tempdir().unwrap();
        let ctl = seeded(
            &dir,
            "an-older-build",
            HoldStatus::Unconfirmed {
                cause: "coord_unreachable: timed out".to_string(),
            },
            T0 + DEFAULT_HOLD_MS,
        );
        let door = MockDoor::primary();
        *door.release_reply.lock().unwrap() = Some(ReleaseCall::RouteAbsent);
        assert_eq!(
            ctl.on_boot(&*door, running_build(), T0 + 1).await,
            BootStep::ReleasedForRebuild {
                outcome: "route_absent".to_string()
            }
        );
        assert_eq!(door.release_count(), 1, "the release was still asked");
        assert_eq!(load_from(&ctl.path).unwrap().state, PrepState::Restoring);
    }

    /// Finding 5: `prepare` on a rebuilt runner refuses exactly as `extend`
    /// does — it never re-sends the OLD prepare's coord hold.
    #[tokio::test]
    async fn restart_prep_prepare_on_a_rebuilt_runner_refuses_like_extend() {
        let dir = tempfile::tempdir().unwrap();
        let ctl = seeded(
            &dir,
            "an-older-build",
            HoldStatus::Unconfirmed {
                cause: "coord_unreachable: x".to_string(),
            },
            T0 + DEFAULT_HOLD_MS,
        );
        let door = MockDoor::primary();
        let err = ctl
            .prepare(&*door, "again", T0 + 1, || async { Ok(cohort(&["n"])) })
            .await
            .unwrap_err();
        assert!(err.contains("rebuilt"), "{err}");
        assert_eq!(
            door.hold_count(),
            0,
            "the old prepare's hold is not re-sent"
        );
        let extend = ctl.extend(&*door, T0 + 1).await.unwrap_err();
        assert!(extend.contains("rebuilt"), "{extend}");
    }

    /// Finding 3 (review): the release's read-back lifts the drain, and a lifted
    /// drain re-runs every deferred boot restore — so it is folded in only
    /// once the record already says `restoring`, on the boot step and on
    /// Release hold alike.
    #[tokio::test]
    async fn restart_prep_a_release_lifts_the_drain_only_once_the_record_restores() {
        let dir = tempfile::tempdir().unwrap();
        let ctl = seeded(
            &dir,
            "an-older-build",
            HoldStatus::Confirmed,
            T0 + DEFAULT_HOLD_MS,
        );
        let door = MockDoor::primary();
        *door.record_at.lock().unwrap() = Some(ctl.path.clone());
        ctl.on_boot(&*door, running_build(), T0 + 1).await;
        assert_eq!(
            door.published.lock().unwrap().as_slice(),
            [("clear".to_string(), Some(PrepState::Restoring))]
        );

        // Release hold after a same-build restart.
        let dir = tempfile::tempdir().unwrap();
        let ctl = seeded(
            &dir,
            running_build(),
            HoldStatus::Confirmed,
            T0 + DEFAULT_HOLD_MS,
        );
        let door = MockDoor::primary();
        *door.record_at.lock().unwrap() = Some(ctl.path.clone());
        ctl.on_boot(&*door, running_build(), T0 + 1).await;
        ctl.cancel(&*door, T0 + 2).await.unwrap();
        assert_eq!(
            door.published.lock().unwrap().last(),
            Some(&("clear".to_string(), Some(PrepState::Restoring)))
        );
    }

    /// Review round 2, A: a restarted runner's prepare whose hold has EXPIRED
    /// is still in its window, and still owed a release that restores its
    /// cohort by hand. A prepare (a stale webview's) must not store a new
    /// record over it — on a rebuilt runner and after a same-build restart.
    #[tokio::test]
    async fn restart_prep_a_restarted_runners_expired_prepare_is_never_replaced() {
        let until = T0 + 1_000;
        let dir = tempfile::tempdir().unwrap();
        let ctl = seeded(
            &dir,
            "an-older-build",
            HoldStatus::Unconfirmed {
                cause: "coord_unreachable: x".to_string(),
            },
            until,
        );
        let door = MockDoor::primary();
        let err = ctl
            .prepare(&*door, "again", until + 1, || async { Ok(cohort(&["n"])) })
            .await
            .unwrap_err();
        assert!(err.contains("rebuilt or restarted"), "{err}");
        assert_eq!(door.hold_count(), 0);
        assert_eq!(load_from(&ctl.path).unwrap().prepare_id, "prep-boot");

        let dir = tempfile::tempdir().unwrap();
        let ctl = seeded(&dir, running_build(), HoldStatus::Confirmed, until);
        let door = MockDoor::primary();
        ctl.on_boot(&*door, running_build(), until + 1).await;
        assert!(
            !ctl.record().unwrap().is_active(until + 2),
            "the hold expired"
        );
        let err = ctl
            .prepare(&*door, "again", until + 2, || async { Ok(cohort(&["n"])) })
            .await
            .unwrap_err();
        assert!(err.contains("rebuilt or restarted"), "{err}");
        assert_eq!(load_from(&ctl.path).unwrap().prepare_id, "prep-boot");
    }

    /// Review round 2, D: a cohort with no restore candidate left (every
    /// session closed on purpose during the prepare) is `done` at once on the
    /// rebuilt runner's boot and on Release hold — never an empty `restoring`
    /// that blocks the next prepare — and the drain still lifts only after the
    /// record moved.
    #[tokio::test]
    async fn restart_prep_a_cohort_with_nothing_to_restore_is_done_at_once() {
        let closed_on_purpose = |ctl: &RestartPrep| {
            ctl.update("prep-boot", |r| {
                for row in &mut r.cohort.rows {
                    row.left_capture_at = Some(T0 + 10);
                    row.left_capture_reason = Some("explicit".to_string());
                }
            })
            .unwrap();
        };
        let dir = tempfile::tempdir().unwrap();
        let ctl = seeded(
            &dir,
            "an-older-build",
            HoldStatus::Confirmed,
            T0 + DEFAULT_HOLD_MS,
        );
        closed_on_purpose(&ctl);
        let door = MockDoor::primary();
        *door.record_at.lock().unwrap() = Some(ctl.path.clone());
        ctl.on_boot(&*door, running_build(), T0 + 1).await;
        assert_eq!(load_from(&ctl.path).unwrap().state, PrepState::Done);
        assert_eq!(
            door.published.lock().unwrap().as_slice(),
            [("clear".to_string(), Some(PrepState::Done))]
        );

        let dir = tempfile::tempdir().unwrap();
        let ctl = seeded(
            &dir,
            running_build(),
            HoldStatus::Confirmed,
            T0 + DEFAULT_HOLD_MS,
        );
        closed_on_purpose(&ctl);
        let door = MockDoor::primary();
        ctl.on_boot(&*door, running_build(), T0 + 1).await;
        let rec = ctl.cancel(&*door, T0 + 2).await.unwrap();
        assert_eq!(rec.state, PrepState::Done);
        // Nothing waits, so a new prepare may start.
        ctl.prepare(&*door, "next", T0 + 3, || async { Ok(cohort(&["n"])) })
            .await
            .expect("a new prepare");
    }

    /// Review round 3: a restarted runner's release that coord cannot settle
    /// — unreachable, or `route_absent` for a hold it once confirmed — settles
    /// as `expired: <until>` once the hold's `until` has passed, because
    /// coord's drain expired by itself. The record restores (or is `done` with
    /// nothing to restore, so a new prepare may start), and the drain lifts
    /// only after the record moved. A runner that has NOT restarted keeps the
    /// prepare in force.
    #[tokio::test]
    async fn restart_prep_an_expired_hold_on_a_restarted_runner_settles_its_release() {
        let until = T0 + 1_000;
        let unreachable = || ReleaseCall::Unreachable {
            cause: "timed out".to_string(),
        };

        // Same-build restart, coord unreachable: Release hold restores.
        let dir = tempfile::tempdir().unwrap();
        let ctl = seeded(
            &dir,
            running_build(),
            HoldStatus::Unconfirmed {
                cause: "coord_unreachable: x".to_string(),
            },
            until,
        );
        let door = MockDoor::primary();
        *door.record_at.lock().unwrap() = Some(ctl.path.clone());
        *door.release_reply.lock().unwrap() = Some(unreachable());
        *door.drain.lock().unwrap() = Err("coord unreachable".to_string());
        ctl.on_boot(&*door, running_build(), until + 1).await;
        let rec = ctl
            .cancel(&*door, until + 2)
            .await
            .expect("settled by expiry");
        assert_eq!(rec.state, PrepState::Restoring);
        let outcome = &rec.release.as_ref().unwrap().outcome;
        assert!(
            outcome.starts_with(&format!("expired: {}", rfc3339(until))),
            "{outcome}"
        );
        assert!(outcome.contains("coord_unreachable"), "{outcome}");
        assert_eq!(
            door.published.lock().unwrap().last().map(|p| p.1.clone()),
            Some(Some(PrepState::Restoring)),
            "the read is published only after the record restores"
        );
        // No longer refused as a restarted runner's prepare — only by the
        // cohort waiting to be restored.
        let err = ctl
            .prepare(&*door, "next", until + 3, || async { Ok(cohort(&["n"])) })
            .await
            .unwrap_err();
        assert!(err.contains("waiting to be restored"), "{err}");

        // Nothing to restore: `done`, and the next prepare is allowed.
        let dir = tempfile::tempdir().unwrap();
        let ctl = seeded(
            &dir,
            running_build(),
            HoldStatus::Unconfirmed {
                cause: "coord_unreachable: x".to_string(),
            },
            until,
        );
        ctl.update("prep-boot", |r| {
            for row in &mut r.cohort.rows {
                row.left_capture_at = Some(T0 + 10);
                row.left_capture_reason = Some("explicit".to_string());
            }
        })
        .unwrap();
        let door = MockDoor::primary();
        *door.release_reply.lock().unwrap() = Some(unreachable());
        ctl.on_boot(&*door, running_build(), until + 1).await;
        assert_eq!(
            ctl.cancel(&*door, until + 2).await.unwrap().state,
            PrepState::Done
        );
        *door.release_reply.lock().unwrap() = None;
        ctl.prepare(&*door, "next", until + 3, || async { Ok(cohort(&["n"])) })
            .await
            .expect("a new prepare");

        // Rebuilt runner, a CONFIRMED hold whose release answers 404: the boot
        // step settles it by expiry.
        let dir = tempfile::tempdir().unwrap();
        let ctl = seeded(&dir, "an-older-build", HoldStatus::Confirmed, until);
        let door = MockDoor::primary();
        *door.release_reply.lock().unwrap() = Some(parse_release_reply(404, ""));
        let step = ctl.on_boot(&*door, running_build(), until + 1).await;
        assert!(
            matches!(&step, BootStep::ReleasedForRebuild { outcome }
                if outcome.starts_with("expired: ") && outcome.contains("route_absent")),
            "{step:?}"
        );
        assert_eq!(load_from(&ctl.path).unwrap().state, PrepState::Restoring);

        // Before the hold expires, the same release does not settle.
        let dir = tempfile::tempdir().unwrap();
        let ctl = seeded(&dir, "an-older-build", HoldStatus::Confirmed, until);
        let door = MockDoor::primary();
        *door.release_reply.lock().unwrap() = Some(unreachable());
        assert!(matches!(
            ctl.on_boot(&*door, running_build(), until - 1).await,
            BootStep::ReleaseFailed { .. }
        ));
        assert!(ctl.cancel(&*door, until - 1).await.is_err());
        // Once it has, Release hold settles it.
        assert_eq!(
            ctl.cancel(&*door, until).await.unwrap().state,
            PrepState::Restoring
        );

        // A runner that has NOT restarted keeps the prepare in force.
        let dir = tempfile::tempdir().unwrap();
        let ctl = controller(&dir);
        let door = MockDoor::primary();
        prepare(&ctl, &door).await;
        *door.release_reply.lock().unwrap() = Some(unreachable());
        let err = ctl
            .cancel(&*door, T0 + DEFAULT_HOLD_MS + 1)
            .await
            .unwrap_err();
        assert!(err.contains("coord_unreachable"), "{err}");
        assert_eq!(load_from(&ctl.path).unwrap().state, PrepState::Preparing);

        // Coord's read-back still holds a drain for THIS prepare (its `until`
        // ran past the recorded one): the local clock does not settle it.
        let dir = tempfile::tempdir().unwrap();
        let ctl = seeded(&dir, "an-older-build", HoldStatus::Confirmed, until);
        let door = MockDoor::primary();
        *door.release_reply.lock().unwrap() = Some(parse_release_reply(404, ""));
        *door.drain.lock().unwrap() = Ok(DrainObservation::Drained {
            until: None,
            reason: Some(format!("{HOLD_REASON_PREFIX}prep-boot: operator restart")),
        });
        assert!(
            matches!(
                ctl.on_boot(&*door, running_build(), until + 1).await,
                BootStep::ReleaseFailed { .. }
            ),
            "coord still holds: no settle by expiry on boot"
        );
        assert!(
            ctl.cancel(&*door, until + 2).await.is_err(),
            "coord still holds: no settle by expiry on Release hold"
        );
        assert!(load_from(&ctl.path).unwrap().in_window());

        // A hold with NO recorded `until` never counts as expired.
        let dir = tempfile::tempdir().unwrap();
        let ctl = seeded(&dir, "an-older-build", HoldStatus::Confirmed, until);
        ctl.update("prep-boot", |r| {
            r.hold_until_ms = None;
            r.hold_until = None;
        })
        .unwrap();
        let door = MockDoor::primary();
        *door.release_reply.lock().unwrap() = Some(unreachable());
        assert!(matches!(
            ctl.on_boot(&*door, running_build(), i64::MAX / 2).await,
            BootStep::ReleaseFailed { .. }
        ));
        assert!(ctl.cancel(&*door, i64::MAX / 2).await.is_err());
    }

    #[tokio::test]
    async fn restart_prep_extend_refuses_outside_the_window() {
        let dir = tempfile::tempdir().unwrap();
        let ctl = controller(&dir);
        let door = MockDoor::primary();
        assert!(ctl.extend(&*door, T0).await.is_err(), "no record");
        prepare(&ctl, &door).await;
        ctl.cancel(&*door, T0).await.unwrap();
        let err = ctl.extend(&*door, T0).await.unwrap_err();
        assert!(err.contains("nothing to extend"), "{err}");
        assert_eq!(door.hold_count(), 1, "only the prepare's hold");
    }

    #[tokio::test]
    async fn restart_prep_a_prepare_never_overwrites_a_cohort_waiting_to_be_restored() {
        let dir = tempfile::tempdir().unwrap();
        let ctl = controller(&dir);
        let door = MockDoor::primary();
        let rec = prepare(&ctl, &door).await;
        ctl.update(&rec.prepare_id, |r| r.state = PrepState::Restoring)
            .unwrap();
        let err = ctl
            .prepare(&*door, "again", T0 + 1, || async { Ok(cohort(&["s9"])) })
            .await
            .unwrap_err();
        assert!(err.contains("still waiting to be restored"), "{err}");
        let kept = ctl.record().unwrap();
        assert_eq!(
            kept.prepare_id, rec.prepare_id,
            "the restoring record is kept"
        );
        assert_eq!(kept.state, PrepState::Restoring);
        assert_eq!(door.hold_count(), 1, "no second hold");
        // Once dismissed (done), a new prepare starts.
        ctl.update(&rec.prepare_id, |r| r.state = PrepState::Done)
            .unwrap();
        let fresh = ctl
            .prepare(&*door, "again", T0 + 2, || async { Ok(cohort(&["s9"])) })
            .await
            .unwrap();
        assert_ne!(fresh.prepare_id, rec.prepare_id);
    }

    /// A verdict persisted by another runner process (the one before a
    /// restart) is never current; the one this process evaluates is.
    #[test]
    fn restart_prep_a_verdict_from_another_process_is_not_current() {
        let mut rec = PrepareRecord::new("prep-v".to_string(), T0, cohort(&["s1"]));
        assert!(!view_of(&rec, false, T0).verdict_current, "no verdict yet");
        let ev = crate::restart_prep::notice::CensusEvidence {
            census_error: None,
            by_session: Default::default(),
            live_terminals: Default::default(),
            unattributed_live: 0,
            device_drain: None,
            close_reasons: std::collections::HashMap::new(),
            activity_detail: std::collections::HashMap::new(),
            activity_key: std::collections::HashMap::new(),
        };
        let mut v = crate::restart_prep::notice::verdict_of(&rec, &ev, None, T0);
        rec.verdict = Some(v.clone());
        assert!(view_of(&rec, false, T0).verdict_current, "evaluated here");
        v.evaluated_by = Some("a-process-before-the-restart".to_string());
        rec.verdict = Some(v.clone());
        assert!(!view_of(&rec, false, T0).verdict_current);
        v.evaluated_by = None;
        rec.verdict = Some(v);
        assert!(
            !view_of(&rec, false, T0).verdict_current,
            "unstamped is not current"
        );
    }
}
