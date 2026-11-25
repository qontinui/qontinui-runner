//! Transcript facts: W (declared work, with provenance), S (the schedule rules),
//! P (parked) and R (revived), from the session's transcript records
//! (architecture §2.1, §2.4).
//!
//! The records are the kernel's content-free distillation of a Claude Code
//! transcript (no prompt or reply text): a time, a scope, and a typed kind. The
//! runner's reader produces them; this module never sees a transcript line.

use crate::state::{
    AccountId, ApiErrorKind, AuthError, AutoContinue, CronKind, Declared, Dur, FinishKind,
    Instance, Parked, SelfCont, SelfConts, Timestamp, Tri, Unk, CRON_EXPIRY, KEEPALIVE,
};

/// Whose transcript a record is from. Only MAIN records declare a finish or
/// open a turn; schedules armed in a subagent transcript still count (S).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Main,
    Subagent,
}

/// A turn-opening record's origin (`turnOrigin` / `promptSource` /
/// `origin.kind`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnOrigin {
    Human,
    /// A peer message, including an own-child `selfSent`.
    Peer,
    /// A cron or wakeup fire.
    Scheduled,
    TaskNotification,
    AutoContinuation,
    Other(String),
}

/// The tools whose use declares a finish (§2.1 W).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeclareTool {
    FinishSession,
    Unattended,
    /// The finish-door call.
    FinishDoor,
    /// `coord_report_status` naming THIS session with status `finished`.
    CoordReportStatus,
    /// The session's own `work_completed` / `work_abandoned` report on its own gate.
    GateOutcome,
    /// `/next-move`'s E arm: NOT a declaration (T30). Present so a reader that
    /// sees it can say so; it never declares.
    NextMoveFinishE,
}

/// A tool use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolUse {
    Declare {
        tool: DeclareTool,
        kind: FinishKind,
        items: Vec<String>,
    },
    ScheduleWakeup {
        at: Timestamp,
        /// `stop: true` clears every loop entry.
        stop: bool,
    },
    CronCreate {
        id: String,
        next: Timestamp,
        interval: Option<Dur>,
        recurring: bool,
        durable: bool,
    },
    CronDelete {
        id: String,
    },
}

/// One kind of record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordKind {
    /// A user record that opens (or may open) a turn.
    TurnOpen {
        origin: TurnOrigin,
        /// `isCompactSummary`: excluded from R (7 false revivals otherwise).
        compact_summary: bool,
        /// A local-command record that starts no model turn (the lifecycle's own
        /// `/exit` command record and its output, T42).
        no_turn: bool,
    },
    ToolUse(ToolUse),
    /// `turn_end`: the end of a turn. `loop_turn`: `taskKind:"loop"` or a
    /// `wakeupSource`; `keepalive_fired`: the turn was itself a keepalive's fire.
    TurnEnd {
        loop_turn: bool,
        keepalive_fired: bool,
    },
    /// `scheduled_task_fire`.
    WakeupFired,
    CronFired {
        id: String,
    },
    /// An unbalanced `queue-operation` enqueue (`dequeue: true` balances it).
    Queue {
        id: String,
        dequeue: bool,
    },
    /// The "continuing automatically at <t>" notice.
    AutoContinueNotice {
        resets_at: Timestamp,
    },
    /// CC's "resurrected N session cron task(s)" record after a resume.
    CronsResurrected,
    /// CC's re-armed-wakeup record after a resume.
    WakeupRearmed {
        scheduled_for: Timestamp,
    },
    /// A final `isApiErrorMessage` record.
    ApiError(ApiClass),
    /// `quotaLimits.resetsAt` / `rateLimitType`.
    QuotaLimit {
        resets_at: Timestamp,
        limit_type: String,
        account: AccountId,
    },
}

/// How a final API error classifies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiClass {
    Auth(AuthError),
    Retryable(ApiErrorKind),
}

/// One distilled record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptRecord {
    pub at: Timestamp,
    pub scope: Scope,
    pub kind: RecordKind,
}

fn is_turn_open(r: &TranscriptRecord) -> bool {
    matches!(
        &r.kind,
        RecordKind::TurnOpen {
            compact_summary: false,
            no_turn: false,
            ..
        }
    ) && r.scope == Scope::Main
}

// ---------------------------------------------------------------------------
// W
// ---------------------------------------------------------------------------

/// A declaring tool use found in the MAIN transcript.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclaringUse {
    pub at: Timestamp,
    pub tool: DeclareTool,
    pub kind: FinishKind,
    pub items: Vec<String>,
}

/// What coord's row says about the work axis.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoordWork {
    Finished { since: Timestamp },
    Working,
    Stalled,
    Undeclared,
}

/// The relay's local finish for a gate outcome (runner#2425): reason
/// `relayed_gate_outcome:<outcome>`, matched with the gate's own outcome row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelayedFinish {
    pub outcome_row_present: bool,
}

/// What `declared_work` is matched against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FinishEvidence {
    pub coord: CoordWork,
    /// The runner's LOCAL finish record (`finishedAt`, written synchronously by
    /// the finish door): `None` when unreadable.
    pub local_finish_at: Option<Timestamp>,
    pub relay: Option<RelayedFinish>,
}

/// The window a declaration may precede the local finish record by (§2.1 W).
pub const DECLARATION_WINDOW: Dur = Dur::mins(10);

/// The declaring tool uses of the MAIN transcript, in time order.
pub fn declaring_uses(records: &[TranscriptRecord]) -> Vec<DeclaringUse> {
    let mut v: Vec<DeclaringUse> = records
        .iter()
        .filter(|r| r.scope == Scope::Main)
        .filter_map(|r| match &r.kind {
            RecordKind::ToolUse(ToolUse::Declare { tool, kind, items })
                if *tool != DeclareTool::NextMoveFinishE =>
            {
                Some(DeclaringUse {
                    at: r.at,
                    tool: *tool,
                    kind: *kind,
                    items: items.clone(),
                })
            }
            _ => None,
        })
        .collect();
    v.sort_by_key(|d| d.at);
    v
}

/// W with provenance. Coord's `finished` reads as `Finished` only when the
/// session's MAIN transcript holds the declaring tool use, timed before and
/// within 10 min of the runner's local finish record (coord's `since` is the
/// match only where the local record is unreadable or the declaration was
/// coord-only; a relayed gate outcome is matched by the relay's reason and the
/// gate's outcome row with no time bound). Otherwise `Unknown(declaration_
/// unverified)`: a subagent's or a peer's mis-resolved finish, or `/next-move`'s
/// E-arm finish (T28-T30, T44). Absence of a declaration is a fact, not an
/// unknown.
pub fn declared_work(records: &[TranscriptRecord], ev: &FinishEvidence) -> Declared {
    let since = match &ev.coord {
        CoordWork::Working => return Declared::Working,
        CoordWork::Stalled => return Declared::Stalled,
        CoordWork::Undeclared => return Declared::Undeclared,
        CoordWork::Finished { since } => *since,
    };
    let unverified = || {
        Declared::Unknown(Unk::new(
            "declaration",
            "declaration_unverified: no declaring tool use in the main transcript matches the finish",
        ))
    };
    let within = |d: &DeclaringUse, anchor: Timestamp| -> bool {
        d.at <= anchor && anchor.since(d.at) <= DECLARATION_WINDOW
    };
    for d in declaring_uses(records).into_iter().rev() {
        let matched = match d.tool {
            DeclareTool::GateOutcome => ev.relay.as_ref().is_some_and(|r| r.outcome_row_present),
            DeclareTool::CoordReportStatus => within(&d, since),
            DeclareTool::FinishSession | DeclareTool::Unattended | DeclareTool::FinishDoor => {
                match ev.local_finish_at {
                    Some(local) => within(&d, local),
                    None => within(&d, since),
                }
            }
            DeclareTool::NextMoveFinishE => false,
        };
        if matched {
            return Declared::Finished {
                at: d.at.min(since),
                kind: d.kind,
                items: d.items,
            };
        }
    }
    unverified()
}

// ---------------------------------------------------------------------------
// R
// ---------------------------------------------------------------------------

/// R (revived): a turn-opening record in the MAIN transcript newer than the
/// finish stamp — the EARLIER of the verified declaring use's time and coord's
/// `since`. Peer, own-child (`isMeta: true`), scheduled, task-notification and
/// auto-continuation turns all count; `isMeta` is NOT the exclusion — only a
/// compaction summary and a local-command record that starts no model turn are
/// excluded. `readable: false` (no transcript read) is UNKNOWN.
pub fn revived(records: &[TranscriptRecord], readable: bool, stamp: Option<Timestamp>) -> Tri {
    if !readable {
        return Tri::unknown("transcript", "main transcript unreadable");
    }
    let Some(stamp) = stamp else {
        return Tri::unknown("revived", "no finish stamp to measure from");
    };
    Tri::from_bool(records.iter().any(|r| is_turn_open(r) && r.at > stamp))
}

/// The finish stamp §2.1 R is measured from.
pub fn finish_stamp(w: &Declared, coord_since: Option<Timestamp>) -> Option<Timestamp> {
    match (w, coord_since) {
        (Declared::Finished { at, .. }, Some(s)) => Some((*at).min(s)),
        (Declared::Finished { at, .. }, None) => Some(*at),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// S
// ---------------------------------------------------------------------------

/// Undelivered coord messages (C2's count) and the Deliverer's in-flight set.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Arrivals {
    pub pending: Vec<PendingMessage>,
    pub in_flight: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingMessage {
    pub id: String,
    pub blocking: bool,
    pub delivered: bool,
    pub acked: bool,
}

/// Where the live instance began, so entries armed by an earlier instance are
/// lost unless the new instance's transcript shows a re-arm or a fire (§2.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InstanceBounds {
    pub instance: Instance,
    pub started_at: Timestamp,
}

/// S from the transcript rules: `ScheduleWakeup` / `CronCreate` / `CronDelete`
/// tool uses (main AND subagent transcripts), fires, the 7-day cron expiry, the
/// keepalive rule, the auto-continue notice, unbalanced queue enqueues newer
/// than the process start, CC's resurrect / re-arm records, and C2's arrivals.
/// `readable: false` is `Unknown` (gating: no close).
pub fn selfconts_from(
    records: &[TranscriptRecord],
    readable: bool,
    live: InstanceBounds,
    arrivals: &Arrivals,
    stop_crons: Option<&[SelfCont]>,
    now: Timestamp,
) -> SelfConts {
    if !readable {
        return SelfConts::Unknown(Unk::new("transcript", "unreadable"));
    }
    let mut recs: Vec<&TranscriptRecord> = records.iter().collect();
    recs.sort_by_key(|r| r.at);

    // (wakeup target, armed_at)
    let mut wakeup: Option<(Timestamp, Timestamp)> = None;
    let mut keepalive: Option<(Timestamp, Timestamp)> = None;
    struct Cron {
        id: String,
        next: Timestamp,
        interval: Option<Dur>,
        recurring: bool,
        durable: bool,
        created: Timestamp,
    }
    let mut crons: Vec<Cron> = Vec::new();
    let mut auto: Option<(Timestamp, Timestamp)> = None;
    let mut queued: Vec<(String, Timestamp)> = Vec::new();
    let mut scheduled_in_turn = false;
    let mut lost: Vec<Timestamp> = Vec::new();

    for r in recs {
        match &r.kind {
            RecordKind::TurnOpen {
                compact_summary: false,
                no_turn: false,
                ..
            } => {
                if r.scope == Scope::Main {
                    scheduled_in_turn = false;
                }
            }
            RecordKind::TurnOpen { .. } => {}
            RecordKind::ToolUse(ToolUse::ScheduleWakeup { at, stop }) => {
                if *stop {
                    wakeup = None;
                    keepalive = None;
                } else {
                    wakeup = Some((*at, r.at));
                    scheduled_in_turn = true;
                }
            }
            RecordKind::ToolUse(ToolUse::CronCreate {
                id,
                next,
                interval,
                recurring,
                durable,
            }) => {
                crons.retain(|c| &c.id != id);
                crons.push(Cron {
                    id: id.clone(),
                    next: *next,
                    interval: *interval,
                    recurring: *recurring,
                    durable: *durable,
                    created: r.at,
                });
            }
            RecordKind::ToolUse(ToolUse::CronDelete { id }) => crons.retain(|c| &c.id != id),
            RecordKind::ToolUse(ToolUse::Declare { .. }) => {}
            RecordKind::TurnEnd {
                loop_turn,
                keepalive_fired,
            } => {
                // The keepalive rule (§2.4): a loop turn that ended with no
                // ScheduleWakeup infers a keepalive at turn_end + 1,200 s. CC's
                // keepalive budget is 1, so a keepalive-FIRED tick that ends
                // without one arms nothing (the loop has ended).
                if *loop_turn && !scheduled_in_turn && !*keepalive_fired {
                    keepalive = Some((r.at.plus(KEEPALIVE), r.at));
                }
            }
            RecordKind::WakeupFired => {
                wakeup = None;
                keepalive = None;
            }
            RecordKind::CronFired { id } => {
                keepalive = None;
                if let Some(c) = crons.iter_mut().find(|c| &c.id == id) {
                    if c.recurring {
                        if let Some(iv) = c.interval {
                            while c.next <= r.at {
                                c.next = c.next.plus(iv);
                            }
                        }
                    }
                }
                crons.retain(|c| &c.id != id || c.recurring);
            }
            RecordKind::Queue { id, dequeue } => {
                if *dequeue {
                    queued.retain(|(q, _)| q != id);
                } else {
                    queued.push((id.clone(), r.at));
                }
            }
            RecordKind::AutoContinueNotice { resets_at } => auto = Some((*resets_at, r.at)),
            RecordKind::CronsResurrected | RecordKind::WakeupRearmed { .. } => {}
            RecordKind::ApiError(_) | RecordKind::QuotaLimit { .. } => {}
        }
    }

    // Re-arm records after the live instance began.
    let rearmed_crons = records
        .iter()
        .any(|r| r.at >= live.started_at && matches!(r.kind, RecordKind::CronsResurrected));
    let rearmed_wakeup: Option<Timestamp> = records.iter().find_map(|r| match r.kind {
        RecordKind::WakeupRearmed { scheduled_for } if r.at >= live.started_at => {
            Some(scheduled_for)
        }
        _ => None,
    });

    let by = live.instance;
    let mut out: Vec<SelfCont> = Vec::new();

    if let Some((target, armed_at)) = wakeup {
        if armed_at >= live.started_at || rearmed_wakeup == Some(target) {
            out.push(SelfCont::Wakeup {
                at: target,
                armed_by: by,
            });
        } else {
            lost.push(target);
        }
    }
    if let Some((at, armed_at)) = keepalive {
        if armed_at >= live.started_at {
            out.push(SelfCont::KeepaliveInferred { at, armed_by: by });
        } else {
            lost.push(at);
        }
    }
    for c in &crons {
        let expired = c.recurring && !c.durable && now > c.created.plus(CRON_EXPIRY);
        if expired {
            continue;
        }
        let one_shot_dead = !c.recurring && c.next <= now && !c.durable;
        if one_shot_dead && c.created < live.started_at {
            continue;
        }
        // A cron armed before this instance survives only if CC resurrected it
        // (recurring under 7 days old, a one-shot with a future fire) or it is
        // durable.
        let survives = c.created >= live.started_at
            || c.durable
            || (rearmed_crons && (c.recurring || c.next > now));
        if !survives {
            continue;
        }
        let kind = if c.durable {
            CronKind::Durable
        } else if c.recurring {
            CronKind::Recurring { created: c.created }
        } else {
            CronKind::OneShot { fire: c.next }
        };
        out.push(SelfCont::Cron {
            id: c.id.clone(),
            next: c.next,
            kind,
            armed_by: by,
        });
    }
    if let Some((resets_at, armed_at)) = auto {
        if armed_at >= live.started_at {
            out.push(SelfCont::AutoContinue {
                resets_at,
                armed_by: by,
            });
        }
        // else: the arming instance is gone; P reads `withdrawn` (§2.3).
    }
    // A Stop hook's `session_crons` (first-seen stamped) corroborate or add.
    if let Some(stop) = stop_crons {
        for e in stop {
            if !out.contains(e) {
                out.push(e.clone());
            }
        }
    }
    // An unbalanced enqueue newer than the process start.
    for (_, at) in queued {
        if at >= live.started_at {
            out.push(SelfCont::QueuedNotice { since: at });
        }
    }
    for at in lost {
        out.push(SelfCont::LostOnNewInstance { at });
    }
    for m in &arrivals.pending {
        out.push(SelfCont::PendingMessage {
            msg_id: m.id.clone(),
            blocking_unacked: m.blocking && m.delivered && !m.acked,
        });
    }
    for id in &arrivals.in_flight {
        out.push(SelfCont::InFlightDelivery { msg_id: id.clone() });
    }
    SelfConts::Known(out)
}

// ---------------------------------------------------------------------------
// P
// ---------------------------------------------------------------------------

/// How long past `resets_at` an unfired countdown reads `withdrawn` (§2.1 P).
pub const RESET_GRACE: Dur = Dur::mins(10);

/// P: the last turn ended on an API error and nothing has resumed it.
/// `armed` holds only while the instance that armed the countdown lives (it is
/// in memory: a restart, resume or migration drops it), and a reset passed by
/// more than 10 min with no fire reads `withdrawn`.
pub fn parked_from(
    records: &[TranscriptRecord],
    readable: bool,
    live: InstanceBounds,
    now: Timestamp,
) -> Parked {
    if !readable {
        return Parked::Unknown(Unk::new("transcript", "unreadable"));
    }
    let mut recs: Vec<&TranscriptRecord> = records.iter().collect();
    recs.sort_by_key(|r| r.at);
    let mut parked = Parked::None;
    let mut auto: Option<(Timestamp, Timestamp)> = None;
    let mut limit: Option<(Timestamp, String, AccountId)> = None;
    for r in recs {
        match &r.kind {
            // A real turn (any origin) resumes: parking is over.
            RecordKind::TurnOpen {
                compact_summary: false,
                no_turn: false,
                ..
            } if r.scope == Scope::Main => {
                parked = Parked::None;
                limit = None;
                auto = None;
            }
            RecordKind::QuotaLimit {
                resets_at,
                limit_type,
                account,
            } => {
                limit = Some((*resets_at, limit_type.clone(), account.clone()));
                parked = Parked::Limit {
                    limit_type: limit_type.clone(),
                    resets_at: *resets_at,
                    account: account.clone(),
                    auto_continue: AutoContinue::Withdrawn,
                };
            }
            RecordKind::ApiError(ApiClass::Auth(e)) => {
                parked = Parked::Auth {
                    error: *e,
                    account: String::new(),
                };
            }
            RecordKind::ApiError(ApiClass::Retryable(k)) => {
                parked = Parked::ApiError {
                    kind: *k,
                    retryable: true,
                };
            }
            RecordKind::AutoContinueNotice { resets_at } => auto = Some((*resets_at, r.at)),
            _ => {}
        }
    }
    if let (
        Parked::Limit {
            limit_type,
            resets_at,
            account,
            ..
        },
        Some((_, _, _)),
    ) = (&parked, &limit)
    {
        let armed = match auto {
            Some((_, armed_at)) => {
                armed_at >= live.started_at && now <= resets_at.plus(RESET_GRACE)
            }
            None => false,
        };
        return Parked::Limit {
            limit_type: limit_type.clone(),
            resets_at: *resets_at,
            account: account.clone(),
            auto_continue: if armed {
                AutoContinue::Armed {
                    armed_by: live.instance,
                }
            } else {
                AutoContinue::Withdrawn
            },
        };
    }
    parked
}

#[cfg(test)]
mod tests;
