//! While no Claude account on this runner is credential-valid, hold the
//! runner's own agent lane so coord stops placing work here.
//!
//! Plan
//! `2026-11-03-a-runner-with-no-logged-in-claude-account-keeps-an-open-agent-lane-so-coord-burns-spawns-on-it`,
//! Phase 3 (Design B). A runner with no logged-in account still reported an open
//! agent lane, so coord kept placing spawns on it: the emptiest lane in the
//! fleet, and one that refuses every spawn. coord has no capability token for
//! "holds an authenticated Claude account", and adding one would put a second
//! predicate at every coord door. The device drain is already the one lever
//! every door reads, and coord already serves a door a device may use on its
//! OWN drain: `POST /coord/devices/me/hold` and `POST /coord/devices/me/release`
//! (qontinui-coord `crates/coord/src/device_self_hold.rs`). This module is that
//! door's second caller.
//!
//! ## What it writes
//!
//! A hold with `prepare_id` [`PREPARE_ID`], reason [`HOLD_REASON`] and an
//! `until` [`HOLD_FOR`] ahead. coord stores the reason as
//! `restart-hold:<prepare_id>: <reason>`, so "own hold" here means a `me/drain`
//! reason starting [`OWN_REASON_PREFIX`]. That makes boot need no separate
//! path: a runner restarted (or logged in while down) reads its own hold back
//! and applies the same table.
//!
//! The door bounds what a hold can do: it writes only an empty lane or this
//! hold's own record (`409 lane_held_by_other` otherwise), and a release
//! removes only this hold's own record (`not_owner` otherwise). So an operator
//! drain is never overwritten and never cleared by this module.
//!
//! ## The decision table
//!
//! [`decide_hold_action`] is the table, as a pure function; [`account_hold_tick`]
//! gathers its inputs and carries out what it returns.
//!
//! | Case | Action |
//! |---|---|
//! | not the primary (`owns_shared_root_state()` false) | nothing — the drain is device-wide |
//! | not enrolled | nothing — no lever exists |
//! | drain unknown, or `dispatch_role` absent | nothing — an unread lane is never written |
//! | `dispatch_role` `bench` / `ci_node` | nothing — the role already closes the lane |
//! | not ready on 2 consecutive ticks, `clear`, no unexpired recorded `until` | hold |
//! | not ready, own hold, under [`RENEW_UNDER`] left | hold again with a new `until` (extends only) |
//! | not ready, own hold, more left | nothing — so a repeat writes nothing |
//! | not ready, drained by any other owner | nothing |
//! | not ready, `clear`, recorded `until` still ahead | nothing, logged once: an operator undrained (until that `until`, or until a ready tick reads the lane with no own hold) |
//! | ready, own hold | release, on the first ready reading |
//!
//! Then the answers: `released` / `not_owner` end it; `absent` while `me/drain`
//! still shows the own reason retries under each bound tenant, then leaves the
//! hold to its `until`; `unreadable`, a 5xx and a transport error retry next
//! tick; a 409 waits for the drain to change; a 401/403 (or another refusal)
//! warns once and retries every [`WRITE_BACKOFF`].
//!
//! One not-ready reading never holds, because a hold arms wind-down (a drain
//! closes finished sessions and idle looping agents), and a single bad read of
//! a credentials file must not do that.
//!
//! ## coord's door doc says "never on a timer"
//!
//! `device_self_hold.rs` (the module doc's "How the runner recognises its own
//! hold") says the runner re-holds "on an operator action of its own … and when
//! it boots again …, never on a timer". This module is a bounded timer caller,
//! and that is the doc's concern answered rather than ignored: it re-holds only
//! while no account is valid, never after an operator undrain until the
//! recorded `until` passes or a ready tick reads the lane with no own hold (an
//! account came back, ending the no-account spell the undrain answered), and a
//! renewal is about one
//! write per 18 h. coord's
//! doc is coord's to amend; this note is the runner half.
//!
//! ## Only the primary
//!
//! A temp or named runner shares the device with the primary, and the drain is
//! device-wide: a secondary with no login holding the lane would close the
//! primary's lane too. So only the runner that owns the shared root state acts
//! ([`crate::instance::owns_shared_root_state`]).

use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::Deserialize;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::coord_drain_state::{CoordDrainState, DrainObservation};

/// The hold's `prepare_id`: what coord keys the owner label, the stored reason
/// and the release on.
pub(crate) const PREPARE_ID: &str = "no-claude-account";

/// The caller reason sent with the hold.
pub(crate) const HOLD_REASON: &str = "no authenticated Claude account on this runner — run /login";

/// The prefix of the stored reason that marks the hold as this module's own:
/// coord writes `restart-hold:<prepare_id>: <reason>`.
pub(crate) const OWN_REASON_PREFIX: &str = "restart-hold:no-claude-account:";

/// How far ahead each hold's `until` is set. A dead runner, or a build rolled
/// back to one without this module, lapses within this.
pub(crate) const HOLD_FOR: chrono::Duration = chrono::Duration::hours(24);

/// An own hold with less than this left is renewed; with more, nothing is
/// written. 24 h minus 6 h is about one write per 18 h per held device.
pub(crate) const RENEW_UNDER: chrono::Duration = chrono::Duration::hours(6);

/// Consecutive not-ready readings before the first hold.
pub(crate) const NOT_READY_TICKS_TO_HOLD: u32 = 2;

/// After a 401/403 (or another refusal) the next write waits this long.
pub(crate) const WRITE_BACKOFF: chrono::Duration = chrono::Duration::minutes(10);

/// How long the loop waits for the boot drain read before its first tick.
const BOOT_READ_WAIT: Duration = Duration::from_secs(15);

const HOLD_PATH: &str = "/coord/devices/me/hold";
const RELEASE_PATH: &str = "/coord/devices/me/release";

/// The one `dispatch_role` whose lane is open to agent work.
const OPEN_ROLE: &str = "workhorse";

/// Whether a `me/drain` reason is this module's own hold.
pub(crate) fn is_own_reason(reason: Option<&str>) -> bool {
    reason.is_some_and(|r| r.starts_with(OWN_REASON_PREFIX))
}

// ---------------------------------------------------------------------------
// Inputs and memory
// ---------------------------------------------------------------------------

/// The agent lane as one `me/drain` read showed it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LaneView {
    Clear,
    Drained {
        until: Option<DateTime<Utc>>,
        reason: Option<String>,
    },
}

impl LaneView {
    /// The own hold's `until`, when the lane is held by this module.
    fn own_until(&self) -> Option<Option<DateTime<Utc>>> {
        match self {
            LaneView::Drained { until, reason } if is_own_reason(reason.as_deref()) => Some(*until),
            _ => None,
        }
    }
}

/// What the tick knows about coord's drain for this device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DrainView {
    /// This runner is not a coord device.
    NotEnrolled,
    /// The drain could not be read (or read `unreadable`).
    Unknown,
    /// A `me/drain` read, with the dispatch role coord served beside it.
    Read {
        lane: LaneView,
        dispatch_role: Option<String>,
    },
}

/// Everything [`decide_hold_action`] reads besides [`HoldMemory`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HoldInputs {
    pub owns_shared_root_state: bool,
    /// `Some(ready)` when the account predicate was read this tick; `None` when
    /// it was not (or its read did not finish), which is no evidence either way.
    pub ready: Option<bool>,
    pub drain: DrainView,
    pub now: DateTime<Utc>,
}

/// The hold this process last wrote and coord accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RecordedHold {
    pub until: DateTime<Utc>,
    /// The tenant whose drain map holds it — the one to release under.
    pub tenant_id: Option<Uuid>,
}

/// What the loop remembers between ticks. Process memory only: it resets at
/// boot, which coord's door doc allows (a hold is a record, not an intent).
///
/// Two accepted limits follow from that. An operator undrain made while the
/// runner is DOWN is invisible to the next process: it reads a clear lane and
/// holds again once two ticks read no account. And an operator drain that
/// REPLACED the own hold and then expired before the own `until` reads as an
/// undrain of the own hold, so the lane stays open until that `until`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct HoldMemory {
    /// Consecutive not-ready readings.
    pub not_ready_streak: u32,
    /// The own hold this process knows to be on the lane: one coord accepted
    /// from it, or one it read back from the lane (the previous run's, or one
    /// whose 200 was lost). Cleared once a ready tick reads the lane with no own
    /// hold on it.
    pub recorded: Option<RecordedHold>,
    /// A 409 answered a write decided against this lane; no write until the
    /// lane reads differently.
    pub conflict_on: Option<LaneView>,
    /// When that 409 was answered. The wait ends [`WRITE_BACKOFF`] after it even
    /// if the lane never reads differently (a `lane_unreadable` whose record the
    /// merged read never shows, or an operator drain whose `until` this clock
    /// has passed and coord's has not). `None` with `conflict_on` set waits
    /// until the lane changes.
    pub conflict_since: Option<DateTime<Utc>>,
    /// No write before this (after a 401/403 or another refusal).
    pub backoff_until: Option<DateTime<Utc>>,
    /// Whether the refusal that set the backoff was already warned about.
    pub refusal_warned: bool,
    /// An own hold whose release found nothing under any bound tenant: left to
    /// this `until` rather than retried every tick.
    pub release_left_to: Option<Option<DateTime<Utc>>>,
    /// The recorded `until` an operator undrain was last logged for.
    pub undrain_logged_for: Option<DateTime<Utc>>,
}

impl HoldMemory {
    /// Count one account reading into the debounce.
    pub(crate) fn observe_readiness(&mut self, ready: bool) {
        self.not_ready_streak = if ready {
            0
        } else {
            self.not_ready_streak.saturating_add(1)
        };
    }

    /// Fold one read lane. A lane that differs from the one a 409 answered ends
    /// the wait. An own hold the lane shows is recorded, so a hold this process
    /// did not see accepted — one left by the previous run, or one whose 200 was
    /// lost to a transport error or the tick timeout — still marks a later
    /// `Clear` as an operator undrain rather than as a lane to hold again.
    pub(crate) fn note_lane(&mut self, lane: &LaneView) {
        if self.conflict_on.as_ref().is_some_and(|c| c != lane) {
            self.conflict_on = None;
            self.conflict_since = None;
        }
        if let Some(Some(until)) = lane.own_until() {
            self.recorded = Some(match self.recorded {
                Some(rec) => RecordedHold {
                    until: rec.until.max(until),
                    tenant_id: rec.tenant_id,
                },
                None => RecordedHold {
                    until,
                    tenant_id: None,
                },
            });
        }
        if self.release_left_to.is_some() && lane.own_until() != self.release_left_to {
            self.release_left_to = None;
        }
    }

    fn backing_off(&self, now: DateTime<Utc>) -> bool {
        self.backoff_until.is_some_and(|b| now < b)
    }
}

// ---------------------------------------------------------------------------
// The decision
// ---------------------------------------------------------------------------

/// Why a tick wrote nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Idle {
    /// A temp or named runner: the drain is device-wide and the primary's.
    Secondary,
    NotEnrolled,
    DrainUnknown,
    /// The account predicate was not read this tick.
    ReadinessUnread,
    /// `dispatch_role` absent: coord could not read the lane.
    RoleUnread,
    /// `bench` / `ci_node` (or a role this build does not know).
    RoleClosesLane,
    /// An account is valid and there is no own hold to release.
    Ready,
    /// Not ready, but fewer than [`NOT_READY_TICKS_TO_HOLD`] readings in a row.
    Debouncing,
    /// Own hold with more than [`RENEW_UNDER`] left.
    HeldWithMargin,
    DrainedByOther,
    /// The lane reads clear while the hold this process recorded is unexpired:
    /// an operator undrained. Respected until that `until`, or until a ready
    /// tick whose lane read succeeds ends the no-account spell.
    OperatorUndrained {
        until: DateTime<Utc>,
    },
    /// A 409 answered the last write against this same lane.
    WaitingOnConflict,
    /// Inside [`WRITE_BACKOFF`] after a refusal.
    WriteBackoff,
    /// The release found nothing under any bound tenant; the hold lapses at its
    /// `until`.
    ReleaseLeftToDeadline,
}

/// What one tick does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HoldAction {
    Nothing(Idle),
    /// A fresh hold.
    Hold {
        until: DateTime<Utc>,
    },
    /// Extend the own hold's deadline.
    Renew {
        until: DateTime<Utc>,
    },
    /// Release the own hold, first under `tenant` when one was recorded.
    Release {
        tenant: Option<Uuid>,
    },
}

/// A deadline [`HOLD_FOR`] from `now`, whole seconds (coord's repeat tolerance
/// compares seconds, and a stable value keeps the wire readable).
pub(crate) fn hold_deadline(now: DateTime<Utc>) -> DateTime<Utc> {
    let at = now + HOLD_FOR;
    DateTime::from_timestamp(at.timestamp(), 0).unwrap_or(at)
}

/// PURE: Design B's table. See the module doc.
pub(crate) fn decide_hold_action(mem: &HoldMemory, inp: &HoldInputs) -> HoldAction {
    use HoldAction::Nothing;
    if !inp.owns_shared_root_state {
        return Nothing(Idle::Secondary);
    }
    let (lane, role) = match &inp.drain {
        DrainView::NotEnrolled => return Nothing(Idle::NotEnrolled),
        DrainView::Unknown => return Nothing(Idle::DrainUnknown),
        DrainView::Read {
            lane,
            dispatch_role,
        } => (lane, dispatch_role),
    };
    let Some(ready) = inp.ready else {
        return Nothing(Idle::ReadinessUnread);
    };
    let Some(role) = role.as_deref() else {
        return Nothing(Idle::RoleUnread);
    };
    let own_until = lane.own_until();

    if ready {
        // A release removes only this hold's own record, so it does not wait on
        // the role-closes-lane arm below. It does wait on an UNREAD role (above):
        // coord omits the role when it could not read the lane, and a lane read
        // coord could not make is no basis for a write.
        let Some(until) = own_until else {
            return Nothing(Idle::Ready);
        };
        if mem.release_left_to == Some(until) {
            return Nothing(Idle::ReleaseLeftToDeadline);
        }
        if mem.backing_off(inp.now) {
            return Nothing(Idle::WriteBackoff);
        }
        return HoldAction::Release {
            tenant: mem.recorded.and_then(|r| r.tenant_id),
        };
    }

    if role != OPEN_ROLE {
        return Nothing(Idle::RoleClosesLane);
    }
    if mem.conflict_on.as_ref() == Some(lane)
        && mem
            .conflict_since
            .is_none_or(|since| inp.now < since + WRITE_BACKOFF)
    {
        return Nothing(Idle::WaitingOnConflict);
    }

    match lane {
        LaneView::Drained { until, .. } if own_until.is_some() => {
            let Some(until) = until else {
                // coord always serves a held lane's deadline; one with none
                // cannot be running short.
                return Nothing(Idle::HeldWithMargin);
            };
            if *until - inp.now >= RENEW_UNDER {
                return Nothing(Idle::HeldWithMargin);
            }
            if mem.backing_off(inp.now) {
                return Nothing(Idle::WriteBackoff);
            }
            HoldAction::Renew {
                until: hold_deadline(inp.now),
            }
        }
        LaneView::Drained { .. } => Nothing(Idle::DrainedByOther),
        LaneView::Clear => {
            if let Some(rec) = mem.recorded {
                if rec.until > inp.now {
                    return Nothing(Idle::OperatorUndrained { until: rec.until });
                }
            }
            if mem.not_ready_streak < NOT_READY_TICKS_TO_HOLD {
                return Nothing(Idle::Debouncing);
            }
            if mem.backing_off(inp.now) {
                return Nothing(Idle::WriteBackoff);
            }
            HoldAction::Hold {
                until: hold_deadline(inp.now),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Wire answers
// ---------------------------------------------------------------------------

/// coord's answer to `POST /coord/devices/me/hold`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HoldReply {
    /// 200: the record that now holds the lane.
    Held {
        tenant_id: Option<Uuid>,
        until: DateTime<Utc>,
        /// `false` when the lane already held exactly this record.
        changed: bool,
    },
    /// 409 (`lane_held_by_other`, `lane_unreadable`, `hold_expired`).
    Conflict { code: String },
    /// 401 / 403: the credential is not accepted for this door.
    Forbidden { status: u16 },
    /// Another 4xx (a 400 or 422 means this build sent something coord refuses).
    Rejected { status: u16, detail: String },
    /// 5xx.
    ServerError { status: u16 },
    /// A 2xx this build cannot read.
    Unparseable { status: u16, detail: String },
}

#[derive(Deserialize)]
struct HoldWire {
    #[serde(default)]
    tenant_id: Option<Uuid>,
    until: DateTime<Utc>,
    #[serde(default = "default_true")]
    changed: bool,
}

fn default_true() -> bool {
    true
}

fn excerpt(body: &str) -> String {
    body.chars().take(200).collect()
}

fn error_code(body: &str) -> String {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_string))
        .unwrap_or_else(|| "unknown".to_string())
}

impl HoldReply {
    /// One line for the log, naming every field.
    pub(crate) fn describe(&self) -> String {
        match self {
            HoldReply::Held {
                tenant_id,
                until,
                changed,
            } => format!(
                "200 held until {until} in tenant {} ({})",
                tenant_id.map_or_else(|| "?".to_string(), |t| t.to_string()),
                if *changed {
                    "written"
                } else {
                    "a repeat, nothing written"
                }
            ),
            HoldReply::Conflict { code } => format!("409 {code}"),
            HoldReply::Forbidden { status } => format!("{status} refused the credential"),
            HoldReply::Rejected { status, detail } => format!("{status} rejected: {detail}"),
            HoldReply::ServerError { status } => format!("{status} server error"),
            HoldReply::Unparseable { status, detail } => format!("{status} unreadable: {detail}"),
        }
    }
}

/// PURE: a `me/hold` status and body.
pub(crate) fn parse_hold_reply(status: u16, body: &str) -> HoldReply {
    match status {
        200..=299 => match serde_json::from_str::<HoldWire>(body) {
            Ok(w) => HoldReply::Held {
                tenant_id: w.tenant_id,
                until: w.until,
                changed: w.changed,
            },
            Err(e) => HoldReply::Unparseable {
                status,
                detail: format!("{e}: {}", excerpt(body)),
            },
        },
        409 => HoldReply::Conflict {
            code: error_code(body),
        },
        401 | 403 => HoldReply::Forbidden { status },
        500..=599 => HoldReply::ServerError { status },
        _ => HoldReply::Rejected {
            status,
            detail: excerpt(body),
        },
    }
}

/// The agent-lane outcome of a release, as coord gave it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ReleaseOutcome {
    Released,
    NotOwner,
    Absent,
    Unreadable,
    /// An outcome this build does not know.
    Other(String),
}

/// coord's answer to `POST /coord/devices/me/release`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ReleaseReply {
    /// 200.
    Answered {
        outcome: ReleaseOutcome,
        tenant_id: Option<Uuid>,
        /// The device's merged drain after the call, when served.
        read_back: Option<DrainObservation>,
    },
    Forbidden {
        status: u16,
    },
    Rejected {
        status: u16,
        detail: String,
    },
    ServerError {
        status: u16,
    },
    Unparseable {
        status: u16,
        detail: String,
    },
}

#[derive(Deserialize)]
struct ReleaseWire {
    outcome: String,
    #[serde(default)]
    tenant_id: Option<Uuid>,
    #[serde(default)]
    read_back: Option<serde_json::Value>,
}

impl ReleaseReply {
    /// One line for the log, naming every field.
    pub(crate) fn describe(&self) -> String {
        match self {
            ReleaseReply::Answered {
                outcome,
                tenant_id,
                read_back,
            } => format!(
                "200 {outcome:?} in tenant {}, device now {}",
                tenant_id.map_or_else(|| "?".to_string(), |t| t.to_string()),
                match read_back {
                    Some(DrainObservation::Clear) => "clear".to_string(),
                    Some(DrainObservation::Drained { until, reason }) => format!(
                        "drained until {} ({})",
                        until.map_or_else(|| "?".to_string(), |u| u.to_string()),
                        reason.as_deref().unwrap_or("no reason")
                    ),
                    Some(other) => format!("{other:?}"),
                    None => "not served".to_string(),
                }
            ),
            ReleaseReply::Forbidden { status } => format!("{status} refused the credential"),
            ReleaseReply::Rejected { status, detail } => format!("{status} rejected: {detail}"),
            ReleaseReply::ServerError { status } => format!("{status} server error"),
            ReleaseReply::Unparseable { status, detail } => {
                format!("{status} unreadable: {detail}")
            }
        }
    }
}

/// PURE: a `me/release` status and body.
pub(crate) fn parse_release_reply(status: u16, body: &str) -> ReleaseReply {
    match status {
        200..=299 => match serde_json::from_str::<ReleaseWire>(body) {
            Ok(w) => ReleaseReply::Answered {
                outcome: match w.outcome.as_str() {
                    "released" => ReleaseOutcome::Released,
                    "not_owner" => ReleaseOutcome::NotOwner,
                    "absent" => ReleaseOutcome::Absent,
                    "unreadable" => ReleaseOutcome::Unreadable,
                    other => ReleaseOutcome::Other(other.to_string()),
                },
                tenant_id: w.tenant_id,
                read_back: w
                    .read_back
                    .as_ref()
                    .filter(|v| !v.is_null())
                    .map(crate::coord_drain_state::parse_drain_object),
            },
            Err(e) => ReleaseReply::Unparseable {
                status,
                detail: format!("{e}: {}", excerpt(body)),
            },
        },
        401 | 403 => ReleaseReply::Forbidden { status },
        500..=599 => ReleaseReply::ServerError { status },
        _ => ReleaseReply::Rejected {
            status,
            detail: excerpt(body),
        },
    }
}

/// What follows one release answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReleaseNext {
    /// `released`, `not_owner` (an operator drain written over the hold
    /// survives), or `absent` with the own reason gone from the read-back.
    Done,
    /// `absent` while the device's read-back still shows the own reason: the
    /// hold lives in another tenant's drain map.
    RetryUnderOtherTenants,
    /// `unreadable`, an unknown outcome, a 5xx or an unreadable answer.
    RetryNextTick,
    /// 401 / 403 or another refusal.
    Backoff,
}

/// PURE: the release-outcome rows of the table.
pub(crate) fn after_release(reply: &ReleaseReply) -> ReleaseNext {
    match reply {
        ReleaseReply::Answered {
            outcome, read_back, ..
        } => match outcome {
            ReleaseOutcome::Released | ReleaseOutcome::NotOwner => ReleaseNext::Done,
            ReleaseOutcome::Absent => {
                let still_own = matches!(
                    read_back,
                    Some(DrainObservation::Drained { reason, .. })
                        if is_own_reason(reason.as_deref())
                );
                if still_own {
                    ReleaseNext::RetryUnderOtherTenants
                } else {
                    ReleaseNext::Done
                }
            }
            ReleaseOutcome::Unreadable | ReleaseOutcome::Other(_) => ReleaseNext::RetryNextTick,
        },
        ReleaseReply::Forbidden { .. } | ReleaseReply::Rejected { .. } => ReleaseNext::Backoff,
        ReleaseReply::ServerError { .. } | ReleaseReply::Unparseable { .. } => {
            ReleaseNext::RetryNextTick
        }
    }
}

/// What applying a hold answer to the memory means for the log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HoldEffect {
    Held,
    Conflict,
    /// A refusal; `warn` is true the first time since the last success.
    Refused {
        warn: bool,
    },
    RetryNextTick,
}

impl HoldMemory {
    /// Fold a hold answer. `lane` is the lane the write was decided against.
    pub(crate) fn apply_hold_reply(
        &mut self,
        reply: &HoldReply,
        lane: &LaneView,
        now: DateTime<Utc>,
    ) -> HoldEffect {
        match reply {
            HoldReply::Held {
                tenant_id, until, ..
            } => {
                self.recorded = Some(RecordedHold {
                    until: *until,
                    tenant_id: *tenant_id,
                });
                self.conflict_on = None;
                self.conflict_since = None;
                self.backoff_until = None;
                self.refusal_warned = false;
                self.release_left_to = None;
                HoldEffect::Held
            }
            HoldReply::Conflict { .. } => {
                self.conflict_on = Some(lane.clone());
                self.conflict_since = Some(now);
                HoldEffect::Conflict
            }
            HoldReply::Forbidden { .. } | HoldReply::Rejected { .. } => self.refused(now),
            HoldReply::ServerError { .. } | HoldReply::Unparseable { .. } => {
                HoldEffect::RetryNextTick
            }
        }
    }

    fn refused(&mut self, now: DateTime<Utc>) -> HoldEffect {
        self.backoff_until = Some(now + WRITE_BACKOFF);
        let warn = !self.refusal_warned;
        self.refusal_warned = true;
        HoldEffect::Refused { warn }
    }

    /// The hold is gone (or no longer this process's to release).
    fn released(&mut self) {
        self.recorded = None;
        self.release_left_to = None;
        self.backoff_until = None;
        self.refusal_warned = false;
    }
}

// ---------------------------------------------------------------------------
// The tick
// ---------------------------------------------------------------------------

/// The tick's reads of this process, injectable so a test can drive the real
/// tick against a fake coord. [`HoldEnv::production`] is the real set.
pub(crate) struct HoldEnv {
    pub owns_shared_root_state: fn() -> bool,
    /// The spawn's own account predicate. Run on the blocking pool.
    pub account_ready: fn() -> bool,
    /// Every tenant this device is bound to, for the `absent` release retry.
    pub bound_tenants: fn() -> Vec<Uuid>,
}

impl HoldEnv {
    pub(crate) fn production() -> Self {
        Self {
            owns_shared_root_state: crate::instance::owns_shared_root_state,
            account_ready: crate::ai_provider::spawn_account_ready,
            bound_tenants: || {
                super::resolve_heartbeat_binding_set()
                    .map(|b| b.tenant_ids)
                    .unwrap_or_default()
            },
        }
    }
}

/// The account predicate on the blocking pool. A read that did not finish (a
/// panic) is no evidence about the accounts: `None`.
async fn read_readiness(env: &HoldEnv) -> Option<bool> {
    let f = env.account_ready;
    match qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked(f).await {
        Ok(ready) => Some(ready),
        Err(e) => {
            warn!(
                "fleet::claude_account_hold: account read did not finish ({e}); skipping this tick"
            );
            None
        }
    }
}

/// A `me/drain` read as the table sees it: an expired drain is clear, and an
/// `unreadable` or unparseable one is unknown.
fn lane_of(obs: &DrainObservation, now: DateTime<Utc>) -> Option<LaneView> {
    match obs {
        DrainObservation::Clear => Some(LaneView::Clear),
        DrainObservation::Drained { until, .. } if until.is_some_and(|u| u <= now) => {
            Some(LaneView::Clear)
        }
        DrainObservation::Drained { until, reason } => Some(LaneView::Drained {
            until: *until,
            reason: reason.clone(),
        }),
        DrainObservation::Unreadable
        | DrainObservation::Absent { .. }
        | DrainObservation::NotEnrolled { .. } => None,
    }
}

/// Re-read `me/drain` after a write and fold it into the cached drain state, so
/// the spawn gate and the banner follow the write at once.
async fn refold(base: &str) {
    match crate::coord_drain_state::read_me_drain_with_role(base).await {
        Ok(read) => crate::coord_drain_state::observe_me_drain(read.observation),
        Err(e) => debug!("fleet::claude_account_hold: me/drain re-read failed ({e}); the heartbeat will catch up"),
    }
}

fn hold_body(until: DateTime<Utc>) -> serde_json::Value {
    serde_json::json!({
        "reason": HOLD_REASON,
        "until": until,
        "prepare_id": PREPARE_ID,
    })
}

fn release_body() -> serde_json::Value {
    serde_json::json!({ "prepare_id": PREPARE_ID })
}

/// One tick: read the account predicate and the drain, decide, act. Returns
/// what was decided.
///
/// This is what [`spawn_claude_account_hold`] runs on every interval.
pub(crate) async fn account_hold_tick(mem: &mut HoldMemory, env: &HoldEnv) -> HoldAction {
    let now = Utc::now();
    let mut inputs = HoldInputs {
        owns_shared_root_state: (env.owns_shared_root_state)(),
        ready: None,
        drain: DrainView::Unknown,
        now,
    };
    if !inputs.owns_shared_root_state {
        return decide_hold_action(mem, &inputs);
    }
    let cached = crate::coord_drain_state::current();
    match &cached {
        CoordDrainState::NotEnrolled { .. } => {
            inputs.drain = DrainView::NotEnrolled;
            return decide_hold_action(mem, &inputs);
        }
        CoordDrainState::Unknown { .. } => return decide_hold_action(mem, &inputs),
        CoordDrainState::Clear | CoordDrainState::Drained { .. } => {}
    }

    if mem.recorded.is_some_and(|r| r.until <= now) {
        mem.recorded = None;
    }
    // The heartbeat's cached drain may already show the own hold (a hold whose
    // 200 was lost, read back by the heartbeat before this tick): record it too,
    // so an undrain that follows is not taken for a lane to hold. This relies on
    // a release folding its `read_back` into the cache (`write_release`), so a
    // released hold is not still cached here; a release with no `read_back`
    // re-reads the drain for the same reason.
    if let CoordDrainState::Drained { until, reason } = &cached {
        if is_own_reason(reason.as_deref()) {
            mem.note_lane(&LaneView::Drained {
                until: *until,
                reason: reason.clone(),
            });
        }
    }
    inputs.ready = read_readiness(env).await;
    let Some(ready) = inputs.ready else {
        return HoldAction::Nothing(Idle::ReadinessUnread);
    };
    mem.observe_readiness(ready);
    // Ready and the cached drain shows no own hold: there is nothing to
    // release, so the table can only say "nothing" — skip the read.
    // A cached drain whose reason was never read (the boot read and the
    // heartbeat's one refresh both failed) may be the own hold, and so may a
    // hold this process recorded: read in both cases rather than leave the
    // hold to its 24 h deadline.
    let cached_maybe_own = matches!(
        &cached,
        CoordDrainState::Drained { reason, .. }
            if reason.is_none() || is_own_reason(reason.as_deref())
    );
    if ready && !cached_maybe_own && mem.recorded.is_none() {
        return HoldAction::Nothing(Idle::Ready);
    }

    let Some(base) = crate::coord_drain_state::enrolled_coord_base() else {
        return decide_hold_action(mem, &inputs);
    };
    let lane = match crate::coord_drain_state::read_me_drain_with_role(&base).await {
        Ok(read) => {
            let lane = lane_of(&read.observation, now);
            crate::coord_drain_state::observe_me_drain(read.observation);
            if let Some(lane) = lane.clone() {
                inputs.drain = DrainView::Read {
                    lane,
                    dispatch_role: read.dispatch_role,
                };
            }
            lane
        }
        Err(e) => {
            debug!("fleet::claude_account_hold: me/drain read failed ({e}); nothing this tick");
            None
        }
    };
    if let Some(lane) = &lane {
        mem.note_lane(lane);
        // Ready, and the lane carries no own hold: nothing is left to release
        // (a release whose answer was lost landed anyway), and the next
        // not-ready spell is a new one, not an operator undrain.
        // `note_lane` has already cleared `release_left_to` for such a lane.
        if ready && lane.own_until().is_none() {
            mem.recorded = None;
        }
    }

    let action = decide_hold_action(mem, &inputs);
    match (&action, &lane) {
        (HoldAction::Hold { until } | HoldAction::Renew { until }, Some(lane)) => {
            let renew = matches!(action, HoldAction::Renew { .. });
            write_hold(mem, &base, *until, lane, renew, now).await;
        }
        (HoldAction::Release { tenant }, Some(lane)) => {
            write_release(mem, env, &base, *tenant, lane, now).await;
        }
        (HoldAction::Nothing(Idle::OperatorUndrained { until }), _) => {
            if mem.undrain_logged_for != Some(*until) {
                mem.undrain_logged_for = Some(*until);
                info!(
                    "fleet::claude_account_hold: the agent lane reads clear while this runner's \
                     no-account hold runs to {until}: an operator undrained it. Not re-holding \
                     before then; the pre-claim check still defers continuations meanwhile."
                );
            }
        }
        _ => {}
    }
    action
}

async fn write_hold(
    mem: &mut HoldMemory,
    base: &str,
    until: DateTime<Utc>,
    lane: &LaneView,
    renew: bool,
    now: DateTime<Utc>,
) {
    let verb = if renew { "renew" } else { "hold" };
    let reply = match crate::coord_drain_state::post_device_json(base, HOLD_PATH, &hold_body(until))
        .await
    {
        Ok((status, body)) => parse_hold_reply(status, &body),
        Err(e) => {
            debug!("fleet::claude_account_hold: {verb} POST failed ({e}); retrying next tick");
            return;
        }
    };
    let said = reply.describe();
    match mem.apply_hold_reply(&reply, lane, now) {
        HoldEffect::Held => {
            info!(
                "fleet::claude_account_hold: {verb}: no Claude account on this runner is \
                 credential-valid, so its agent lane is held until {until} (prepare_id={PREPARE_ID}, \
                 answer: {}). It is released on the first valid account.",
                reply.describe()
            );
            refold(base).await;
        }
        HoldEffect::Conflict => info!(
            "fleet::claude_account_hold: {verb} answered {said}; the lane is held by \
             another owner or unreadable. Waiting for it to change."
        ),
        HoldEffect::Refused { warn: true } => warn!(
            "fleet::claude_account_hold: {verb} refused ({said}); retrying every {} min",
            WRITE_BACKOFF.num_minutes()
        ),
        HoldEffect::Refused { warn: false } => {
            debug!("fleet::claude_account_hold: {verb} refused again ({said})")
        }
        HoldEffect::RetryNextTick => {
            debug!("fleet::claude_account_hold: {verb} answered {said}; retrying next tick")
        }
    }
}

async fn post_release(base: &str, tenant: Option<Uuid>) -> Option<ReleaseReply> {
    let body = release_body();
    let sent = match tenant {
        Some(t) => {
            crate::coord_drain_state::post_device_json_for_tenant(base, RELEASE_PATH, &body, t)
                .await
        }
        None => crate::coord_drain_state::post_device_json(base, RELEASE_PATH, &body).await,
    };
    match sent {
        Ok((status, body)) => Some(parse_release_reply(status, &body)),
        Err(e) => {
            debug!("fleet::claude_account_hold: release POST failed ({e}); retrying next tick");
            None
        }
    }
}

fn fold_read_back(reply: &ReleaseReply) {
    if let ReleaseReply::Answered {
        read_back: Some(obs),
        ..
    } = reply
    {
        crate::coord_drain_state::observe_me_drain(obs.clone());
    }
}

async fn write_release(
    mem: &mut HoldMemory,
    env: &HoldEnv,
    base: &str,
    tenant: Option<Uuid>,
    lane: &LaneView,
    now: DateTime<Utc>,
) {
    let Some(mut reply) = post_release(base, tenant).await else {
        return;
    };
    let mut next = after_release(&reply);
    if next == ReleaseNext::RetryUnderOtherTenants {
        let tried = tenant.or(match &reply {
            ReleaseReply::Answered { tenant_id, .. } => *tenant_id,
            _ => None,
        });
        for other in (env.bound_tenants)()
            .into_iter()
            .filter(|t| Some(*t) != tried)
        {
            let Some(r) = post_release(base, Some(other)).await else {
                return;
            };
            reply = r;
            next = after_release(&reply);
            if next != ReleaseNext::RetryUnderOtherTenants {
                break;
            }
        }
    }
    fold_read_back(&reply);
    let said = reply.describe();
    match next {
        ReleaseNext::Done => {
            mem.released();
            // With no read-back folded, the cache may still show the released
            // hold, and the tick would record it again from there.
            if !matches!(
                &reply,
                ReleaseReply::Answered {
                    read_back: Some(_),
                    ..
                }
            ) {
                refold(base).await;
            }
            info!(
                "fleet::claude_account_hold: a Claude account is valid again; released this \
                 runner's agent-lane hold ({said})"
            );
        }
        ReleaseNext::RetryUnderOtherTenants => {
            mem.release_left_to = lane.own_until();
            warn!(
                "fleet::claude_account_hold: the hold could not be released under any bound \
                 tenant ({said}); it lapses at its deadline {:?}",
                lane.own_until().flatten()
            );
        }
        ReleaseNext::RetryNextTick => {
            debug!("fleet::claude_account_hold: release answered {said}; retrying next tick")
        }
        ReleaseNext::Backoff => {
            if let HoldEffect::Refused { warn: true } = mem.refused(now) {
                warn!(
                    "fleet::claude_account_hold: release refused ({said}); retrying every {} min",
                    WRITE_BACKOFF.num_minutes()
                );
            }
        }
    }
}

/// The loop body: wait up to `boot_wait` for the boot drain read, then tick on
/// `interval`.
async fn run_account_hold_loop(
    interval: Duration,
    boot_wait: Duration,
    env: HoldEnv,
    on_tick: impl Fn(),
) {
    crate::coord_drain_state::await_boot_read(boot_wait).await;
    let limit = super::heartbeat_loop::tick_timeout(interval);
    let mut mem = HoldMemory::default();
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        ticker.tick().await;
        on_tick();
        if tokio::time::timeout(limit, account_hold_tick(&mut mem, &env))
            .await
            .is_err()
        {
            warn!(
                "fleet::claude_account_hold: tick did not finish within {}s; the next tick \
                 re-reads the drain",
                limit.as_secs()
            );
        }
    }
}

/// Start the account-hold loop: a supervised task beside
/// [`super::spawn_heartbeat`], at the heartbeat's interval, after the boot drain
/// read. See the module doc.
pub fn spawn_claude_account_hold() {
    let interval = super::heartbeat_loop::heartbeat_interval();
    info!(
        "fleet::claude_account_hold: starting, interval={}s",
        interval.as_secs()
    );
    crate::worker_supervisor::spawn_supervised_with_heartbeat(
        "fleet.claude_account_hold",
        move |hb| async move {
            run_account_hold_loop(interval, BOOT_READ_WAIT, HoldEnv::production(), move || {
                hb.tick()
            })
            .await;
        },
    );
}

#[cfg(test)]
mod tests;
