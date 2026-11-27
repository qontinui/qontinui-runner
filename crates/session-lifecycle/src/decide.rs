//! `decide`: the one pure function that turns a session's state vector, its
//! work items, the dials and the host's capabilities into exactly one
//! [`Action`] (architecture §3.1, rules §3.2, first match wins).
//!
//! Properties by construction and test: **total** (an exhaustive match over
//! [`Phase`], no wildcard arm), **deterministic** (no clock, no I/O, no
//! randomness), **monotone in safety** (an axis turning `Unknown` can only move
//! the action toward Observe/Report) and **version-stamped**.
//!
//! Decisions recorded where the architecture leaves the reading open (each
//! cites its section):
//!
//! - **D0 first.** The phase is computed by [`phase_of`], whose first act is the
//!   freshness / Unknown gate (§2.5): a needed axis `Unknown` or stale is
//!   `Phase::Unknown` and the answer is D0 (Observe, then Report after 30 min of
//!   the same Unknown). An `Exited` host skips the live-axis requirements.
//! - **D21 is "any phase"**: it is applied as an interception on every Continue
//!   a rule would emit (D12, D13, D15, D16, D18, D19b) plus the disputed arm in
//!   `Owing`, because a Continue is the only thing it stops.
//! - **The 10-minute Continue budget** counts a `/next-move` Nudge or Redirect
//!   as a Continue (§3.2 "Budgets", coexistence); its E-arm finish is NOT a
//!   declaration and counts nothing. A lifecycle Continue is "absent a new fact"
//!   when the owed item set is unchanged from the last one sent.
//! - **The Stop-hook cap** (3 blocks per item set per hour) is read from the
//!   ledger's block records; "2 consecutive" is approximated by the 10-minute
//!   rule, since a tool call (which resets Claude Code's own counter, SIG-4)
//!   is not part of the ledger.
//! - **An owed dirty tree.** A custody `Dirty` tree is owed (D15) only while W is
//!   not `Finished`; once W is `Finished` the same tree is D18's (a Continue
//!   without a WithdrawFinish).
//! - **D4's resume is allowed when W is Unknown** (an unreadable declaration is
//!   not a finish), and the interim host never resumes for D4: it only reports.
//! - **The held-after-relayed-exit hold** (D4, m6-4..N8-1): the session carries a
//!   `hold` cap on every verdict; no Continue of any rule and no queued message;
//!   one operator notice at the hold's start, then at most once per 24 h and only
//!   when it newly owes something.

use crate::guard::{self, GuardReport};
use crate::model::{
    digest_of, item_set_hash, Action, Budgets, Cap, ClosedListItem, Consumer, ContinuationOutcome,
    ContinueCap, ContinueMsg, Decision, Detection, DetectionKind, Dials, Guard, HostCaps, Lane,
    LanePlan, Notice, ObserveWhy, OperatorTask, Reason, ResumeCap, ResumeVia, RuleId, WorkItemView,
    FN_VERSION,
};
use crate::state::{
    AccountId, Activity, AutoContinue, CloseDoor, CronKind, Custody, CustodyAxis, Declared, Dur,
    ExitFacts, Hosting, Input, ItemFacts, Mode, NextMoveKind, OwnedAxis, PaneKind, Parked, Phase,
    Principal, SelfCont, SelfConts, SessionState, Stopped, Timestamp, Tri, TypedExit, Unk, Writer,
    CLOSE_ACK_TIMEOUT, CLOSE_BACKOFF, OVERDUE, T_MIG, UNKNOWN_REPORT_AFTER,
};

/// `MIGRATION_CAP`: account migrations per session per 24 h
/// (`terminal/account_migration.rs`).
pub const MIGRATION_CAP: usize = 3;
/// D4: resume attempts per session per 24 h.
pub const RESUME_ATTEMPTS_CAP: usize = 3;
/// The Continue budget window.
pub const CONTINUE_WINDOW: Dur = Dur::mins(10);
/// Stop-hook blocks per item set per hour.
pub const BLOCKS_PER_HOUR: usize = 3;
/// D16's settle when no Stop hook is present.
pub const SETTLE: Dur = Dur::mins(15);
/// D6: a measured draft is reported after this long.
pub const DRAFT_STALE: Dur = Dur::hours(24);
/// D7: a dialog in an unattended session is reported after this long.
pub const DIALOG_STALE: Dur = Dur::mins(30);

/// D12: how long a request error waits before its one Continue.
///
/// An IMPLEMENTATION CHOICE: §3.2 D12 says "Continue once after backoff" and
/// names no duration. Two minutes lets a transient overload clear and is well
/// inside the 10-minute Continue budget.
pub const ERROR_BACKOFF: Dur = Dur::mins(2);

const RETRY_SET: &str = "__retry__";
const CLOSEOUT_SET: &str = "__closeout__";
const STRAY_SET: &str = "__stray__";
const CUT_SET: &str = "__cut__";

// ---------------------------------------------------------------------------
// Item facts
// ---------------------------------------------------------------------------

/// One owed fact: a stable id and the sentence a Continue carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Owed {
    pub id: String,
    pub fact: String,
}

fn dirty_trees(s: &SessionState) -> Vec<String> {
    match &s.custody.value {
        CustodyAxis::Known(trees) => trees
            .iter()
            .filter(|(_, c)| matches!(c, Custody::Dirty { .. }))
            .map(|(p, _)| p.clone())
            .collect(),
        CustodyAxis::Unknown(_) => Vec::new(),
    }
}

fn finished(s: &SessionState) -> bool {
    matches!(s.work.value, Declared::Finished { .. })
}

/// Every fact the session positively owes: its own open items, an `Impeded`
/// item, a delivered-but-unacked `blocking` message, and (while not Finished) a
/// dirty tree.
pub fn owed_facts(s: &SessionState, items: &[WorkItemView]) -> Vec<Owed> {
    let mut v: Vec<Owed> = items
        .iter()
        .filter(|i| i.owed_by_session())
        .map(|i| Owed {
            id: i.id.clone(),
            fact: i.fact.clone(),
        })
        .collect();
    if let SelfConts::Known(entries) = &s.selfcont.value {
        for e in entries {
            if let SelfCont::PendingMessage {
                msg_id,
                blocking_unacked: true,
            } = e
            {
                v.push(Owed {
                    id: format!("message:{msg_id}"),
                    fact: format!("blocking message {msg_id} was delivered and is unacknowledged"),
                });
            }
        }
    }
    if !finished(s) {
        for t in dirty_trees(s) {
            v.push(Owed {
                id: format!("custody:{t}"),
                fact: format!(
                    "tree {t} is dirty or ahead of upstream: commit and push, or record why"
                ),
            });
        }
    }
    v.sort_by(|a, b| a.id.cmp(&b.id));
    v
}

/// The three booleans [`phase_of`] needs, from the items.
pub fn item_facts(s: &SessionState, items: &[WorkItemView]) -> ItemFacts {
    let owed = !owed_facts(s, items).is_empty();
    let awaiting_first_verdict = items.iter().any(WorkItemView::awaiting_first_verdict);
    let owned_unknown = matches!(s.owned.value, OwnedAxis::Unknown(_));
    let unverified = owned_unknown
        || items.iter().any(|i| {
            !i.owed_by_session()
                && !i.awaiting_first_verdict()
                && !i.terminal_for_session().is_true()
        });
    ItemFacts {
        owed,
        awaiting_first_verdict,
        unverified,
    }
}

// ---------------------------------------------------------------------------
// The function
// ---------------------------------------------------------------------------

struct Ctx<'a> {
    s: &'a SessionState,
    items: &'a [WorkItemView],
    dials: &'a Dials,
    host: HostCaps,
    now: Timestamp,
    report: GuardReport,
    digest: crate::state::Digest,
}

/// A decision under construction.
struct Dec {
    rule: RuleId,
    consumer: Consumer,
    action: Action,
    reasons: Vec<Reason>,
    caps: Vec<Cap>,
    notices: Vec<Notice>,
}

impl Dec {
    fn new(rule: RuleId, consumer: Consumer, action: Action) -> Dec {
        Dec {
            rule,
            consumer,
            action,
            reasons: Vec::new(),
            caps: Vec::new(),
            notices: Vec::new(),
        }
    }
    fn why(mut self, axis: &'static str, v: impl std::fmt::Debug) -> Dec {
        self.reasons.push(Reason::new(axis, v));
        self
    }
    fn cap(mut self, c: Cap) -> Dec {
        self.caps.push(c);
        self
    }
    fn notice(mut self, n: Notice) -> Dec {
        self.notices.push(n);
        self
    }
}

fn observe(rule: RuleId, why: ObserveWhy) -> Dec {
    Dec::new(rule, Consumer::Neither, Action::Observe { why })
}

fn report(rule: RuleId, consumer: Consumer, kind: DetectionKind, detail: impl Into<String>) -> Dec {
    Dec::new(
        rule,
        consumer,
        Action::Report {
            detection: Detection::new(kind, detail),
        },
    )
}

/// The decision function (§3.1).
pub fn decide(
    s: &SessionState,
    items: &[WorkItemView],
    dials: &Dials,
    host: HostCaps,
    now: Timestamp,
) -> Decision {
    let report = guard::evaluate(s, items, dials, host, now);
    let digest = digest_of(&[&s, &items, &dials, &host, &now]);
    let ctx = Ctx {
        s,
        items,
        dials,
        host,
        now,
        report,
        digest,
    };
    let facts = item_facts(s, items);
    let phase = crate::state::phase_of(s, facts, close_in_flight(s, now), now);
    let mut d = match &phase {
        Phase::Unknown(u) => d0(&ctx, u),
        Phase::Spawning => observe(RuleId::D5, ObserveWhy::Spawning).why("phase", &phase),
        Phase::Working => observe(RuleId::D5, ObserveWhy::Working).why("phase", &phase),
        Phase::Delegated => observe(RuleId::D5, ObserveWhy::Delegated).why("phase", &phase),
        Phase::Interacting => interacting(&ctx),
        Phase::Parked => parked(&ctx),
        Phase::Scheduled => scheduled(&ctx),
        Phase::Stopped(st) => stopped(&ctx, st),
        Phase::Closing => closing(&ctx),
        Phase::Closed { finished } => closed(&ctx, *finished),
    };
    hold_riders(&ctx, &mut d);
    if let Some(c) = unconfirmed_rider(&ctx) {
        d.caps.push(c);
    }
    finish(&ctx, d, &phase)
}

fn finish(ctx: &Ctx, d: Dec, phase: &Phase) -> Decision {
    let mut reasons = d.reasons;
    reasons.insert(0, Reason::new("phase", phase));
    Decision {
        action: d.action,
        rule: d.rule,
        reasons,
        guards: ctx.report.guards.clone(),
        consumer: d.consumer,
        inputs_digest: ctx.digest,
        fn_version: FN_VERSION,
        caps: d.caps,
        notices: d.notices,
    }
}

// ---------------------------------------------------------------------------
// D0
// ---------------------------------------------------------------------------

/// Phase 8: a Close was issued and its acknowledgement is pending, inside
/// [`CLOSE_ACK_TIMEOUT`]. The inbox is frozen and NO second Close is emitted
/// (S6: no blind retry). Past the timeout the session is no longer `Closing`:
/// `decide` treats it as Stopped again, reports `close_unconfirmed` ONCE as a
/// rider ([`unconfirmed_rider`]) and refuses a new Close until the backoff ends
/// ([`close_backoff_until`]); a turn opening clears the fact outright (§3.5).
fn closing(_ctx: &Ctx) -> Dec {
    observe(RuleId::D20, ObserveWhy::CloseInFlight)
}

/// The close in flight, if it is still inside its ack window.
fn close_in_flight(s: &SessionState, now: Timestamp) -> bool {
    s.closing.is_some_and(|t| now.since(t) <= CLOSE_ACK_TIMEOUT)
}

/// When a new Close is next allowed: after an unconfirmed one, its timeout plus
/// the backoff; or the recorded `close_backoff_until` once the fold saw it.
fn close_backoff_until(s: &SessionState) -> Option<Timestamp> {
    let stale = s
        .closing
        .map(|t| t.plus(CLOSE_ACK_TIMEOUT).plus(CLOSE_BACKOFF));
    match (stale, s.close_backoff_until) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, b) => a.or(b),
    }
}

/// `close_unconfirmed`, reported once per issued close: an ack timeout takes
/// the session back to Stopped (§3.5 S6).
fn unconfirmed_rider(ctx: &Ctx) -> Option<Cap> {
    let t = ctx.s.closing?;
    if ctx.now.since(t) <= CLOSE_ACK_TIMEOUT {
        return None;
    }
    let key = format!("close_unconfirmed:{}", t.0);
    if ctx.s.ledger.reported.contains_key(&key) {
        return None;
    }
    Some(Cap::AlsoReport(Detection::new(
        DetectionKind::CloseUnconfirmed,
        format!("close_unconfirmed: a Close issued at {} was not acknowledged inside its timeout; ledger key {key}", t.0),
    )))
}

fn d0(ctx: &Ctx, u: &Unk) -> Dec {
    let persisted = ctx
        .s
        .ledger
        .unknown_since
        .get(&u.probe)
        .is_some_and(|since| ctx.now.since(*since) >= UNKNOWN_REPORT_AFTER);
    if persisted {
        report(
            RuleId::D0,
            Consumer::Detecting,
            DetectionKind::UnknownPersisted {
                probe: u.probe.clone(),
            },
            u.reason.clone(),
        )
        .why("unknown", u)
    } else {
        observe(RuleId::D0, ObserveWhy::Unknown(u.clone())).why("unknown", u)
    }
}

// ---------------------------------------------------------------------------
// Continue plumbing: budgets, D21, the hold
// ---------------------------------------------------------------------------

fn lane_plan(ctx: &Ctx) -> Option<LanePlan> {
    match ctx.host.cont {
        ContinueCap::None => None,
        ContinueCap::PasteOnly => Some(LanePlan {
            lanes: vec![Lane::Paste],
        }),
        ContinueCap::StopBlockPumpPaste => Some(LanePlan {
            lanes: vec![Lane::StopBlock, Lane::Pump, Lane::Paste],
        }),
    }
}

/// Why a Continue of this item set must not go out now, if so.
fn continue_blocked(ctx: &Ctx, set: u64, ids: &[String]) -> Option<Dec> {
    let s = ctx.s;
    if s.hold.is_some() {
        return Some(observe(RuleId::D4, ObserveWhy::HeldAfterRelayedExit));
    }
    if !ctx.dials.cont.is_on() {
        return Some(observe(RuleId::D15, ObserveWhy::DialOff));
    }
    if lane_plan(ctx).is_none() {
        return Some(observe(RuleId::D15, ObserveWhy::HostCannot));
    }
    // D21: the same set continued twice with no new fact between, three blocks
    // in an hour, or disputed.
    let own: Vec<&crate::state::ContinueRecord> = s
        .ledger
        .continues
        .iter()
        .filter(|c| c.source == crate::state::ContinueSource::Lifecycle)
        .collect();
    let n = own.len();
    let twice = n >= 2
        && own[n - 1].items == set
        && own[n - 2].items == set
        && !own[n - 1].new_fact_since
        && !own[n - 2].new_fact_since;
    let blocks = s
        .ledger
        .blocks
        .iter()
        .filter(|b| b.items == set && ctx.now.since(b.at) <= Dur::hours(1))
        .count();
    let disputed: Vec<String> = ids
        .iter()
        .filter(|i| s.ledger.disputes.contains(i))
        .cloned()
        .collect();
    if twice || blocks >= BLOCKS_PER_HOUR || !disputed.is_empty() {
        return Some(exhausted(ctx, disputed));
    }
    // The 10-minute budget: an unchanged set within the window, or any
    // /next-move nudge / redirect within it.
    if let Some(last) = own.last() {
        if last.items == set && ctx.now.since(last.at) < CONTINUE_WINDOW {
            return Some(observe(
                RuleId::D15,
                ObserveWhy::Backoff {
                    until: last.at.plus(CONTINUE_WINDOW),
                },
            ));
        }
    }
    if let Some(nm) = s.ledger.next_move.iter().rev().find(|n| {
        matches!(n.kind, NextMoveKind::Nudge | NextMoveKind::Redirect)
            && ctx.now.since(n.at) < CONTINUE_WINDOW
    }) {
        return Some(observe(
            RuleId::D15,
            ObserveWhy::Backoff {
                until: nm.at.plus(CONTINUE_WINDOW),
            },
        ));
    }
    if !ctx.dials.wake.available() {
        return Some(observe(RuleId::D15, ObserveWhy::BudgetExhausted));
    }
    None
}

/// D21: stop continuing, hand the items to the ledger's next route.
fn exhausted(ctx: &Ctx, disputed: Vec<String>) -> Dec {
    let s = ctx.s;
    let mut d = if disputed.is_empty() {
        report(
            RuleId::D21,
            Consumer::GatingAndDetecting,
            DetectionKind::ContinueExhausted,
            "the same item set was continued twice with no new fact, or blocked 3 times in an hour",
        )
    } else {
        report(
            RuleId::D21,
            Consumer::GatingAndDetecting,
            DetectionKind::DisputedByAuthor {
                items: disputed.clone(),
            },
            "the session disputed these items with a [lifecycle-dispute] footer",
        )
        .cap(Cap::RequeueDisputed { items: disputed })
    };
    // An unattended session NOT declared Finished is `stalled`; a Finished one
    // is never written stalled (Report only).
    if s.mode == Mode::Unattended && !finished(s) && s.hold.is_none() {
        d = d.cap(Cap::MarkStalled);
    }
    d
}

/// Build a Continue, or the reason it is withheld.
fn cont(
    ctx: &Ctx,
    rule: RuleId,
    consumer: Consumer,
    message: ContinueMsg,
    set_ids: &[String],
) -> Dec {
    let set = item_set_hash(set_ids);
    if let Some(blocked) = continue_blocked(ctx, set, set_ids) {
        return blocked;
    }
    let lane = lane_plan(ctx).unwrap_or(LanePlan { lanes: Vec::new() });
    Dec::new(rule, consumer, Action::Continue { message, lane })
}

// ---------------------------------------------------------------------------
// D6 / D7
// ---------------------------------------------------------------------------

fn interacting(ctx: &Ctx) -> Dec {
    match &ctx.s.input.value {
        Input::Draft { since, .. } => {
            if ctx.now.since(*since) >= DRAFT_STALE {
                report(
                    RuleId::D6,
                    Consumer::Detecting,
                    DetectionKind::DraftStale,
                    "a measured draft has sat for 24 h; never typed into, never closed over",
                )
                .why("input", &ctx.s.input.value)
            } else {
                observe(RuleId::D6, ObserveWhy::Draft).why("input", &ctx.s.input.value)
            }
        }
        Input::Dialog { kind, since } => match kind {
            crate::state::DialogKind::LimitMenu => match &ctx.s.parked.value {
                Parked::Limit { .. } => parked(ctx),
                _ => observe(RuleId::D7, ObserveWhy::Dialog).why("input", &ctx.s.input.value),
            },
            crate::state::DialogKind::CredentialPrompt {
                account,
                ui_location,
            } => credential_ask(ctx, RuleId::D7, account, ui_location, None),
            _ => {
                if ctx.s.mode == Mode::Unattended && ctx.now.since(*since) >= DIALOG_STALE {
                    report(
                        RuleId::D7,
                        Consumer::Detecting,
                        DetectionKind::DialogStale,
                        "a dialog has waited 30 min in an unattended session; never auto-answered",
                    )
                    .why("input", &ctx.s.input.value)
                } else {
                    observe(RuleId::D7, ObserveWhy::Dialog).why("input", &ctx.s.input.value)
                }
            }
        },
        // phase_of never yields Interacting for Clear / Unknown.
        Input::Clear | Input::Unknown(_) => observe(
            RuleId::D7,
            ObserveWhy::Gated("input not interacting".into()),
        ),
    }
}

fn credential_task(account: &AccountId, ui_location: &str, parked_sessions: usize) -> OperatorTask {
    OperatorTask::new(
        ClosedListItem::CredentialOnlyHumanHolds {
            account_id: account.clone(),
            ui_location: ui_location.to_string(),
        },
        format!(
            "log in account {account}: run `/login` in a `claude` terminal started under that \
             account's CLAUDE_CONFIG_DIR ({parked_sessions} session(s) parked)"
        ),
        vec![format!("account:{account}")],
        vec!["escalation-bar policy-answers-before-the-operator case 3".to_string()],
        "no served clause lets an agent hold or type an account's credential".to_string(),
        "policy-gap:credential-only-human-holds".to_string(),
        format!("account {account} authenticates"),
    )
}

/// NotifyOperator once per account (deduped through the ledger's notices).
fn credential_ask(
    ctx: &Ctx,
    rule: RuleId,
    account: &AccountId,
    ui_location: &str,
    rider_for: Option<Dec>,
) -> Dec {
    let key = format!("credential:{account}");
    let already = ctx.s.ledger.notices.contains_key(&key);
    match (rider_for, already) {
        (Some(d), true) => d,
        (Some(d), false) => d.cap(Cap::AlsoNotify(credential_task(account, ui_location, 1))),
        (None, true) => observe(rule, ObserveWhy::Gated(format!("{key} already asked"))),
        (None, false) => Dec::new(
            rule,
            Consumer::GatingAndDetecting,
            Action::NotifyOperator {
                task: credential_task(account, ui_location, 1),
            },
        ),
    }
}

// ---------------------------------------------------------------------------
// D8 - D12
// ---------------------------------------------------------------------------

fn recent(ts: &[Timestamp], now: Timestamp, within: Dur) -> usize {
    ts.iter().filter(|t| now.since(**t) <= within).count()
}

/// R or the coord row Unknown: a Migrate resubmits a turn and moves an account,
/// so (S2) it never acts while a finish-related axis is unreadable.
fn finish_axes_unknown(s: &SessionState) -> Option<Unk> {
    match (&s.revived.value, &s.coord_finished.value) {
        (Tri::Unknown(u), _) | (_, Tri::Unknown(u)) => Some(u.clone()),
        _ => None,
    }
}

fn migration_available(ctx: &Ctx) -> bool {
    recent(&ctx.s.ledger.migrations, ctx.now, Dur::hours(24)) < MIGRATION_CAP
}

fn parked(ctx: &Ctx) -> Dec {
    let s = ctx.s;
    match &s.parked.value {
        Parked::Limit {
            resets_at,
            auto_continue,
            account,
            ..
        } => {
            let left = resets_at.since(ctx.now);
            let before_reset = *resets_at > ctx.now;
            let armed = matches!(auto_continue, AutoContinue::Armed { .. });
            // D8: Claude Code continues itself.
            if armed && before_reset && left <= T_MIG {
                return observe(RuleId::D8, ObserveWhy::ClaudeContinuesItself)
                    .why("parked", &s.parked.value);
            }
            match &ctx.dials.budgets {
                // D10u: never equated with "full"; never closes.
                Budgets::Unknown(u) => {
                    let persisted = s
                        .ledger
                        .unknown_since
                        .get("budget")
                        .is_some_and(|t| ctx.now.since(*t) >= UNKNOWN_REPORT_AFTER);
                    if persisted {
                        report(
                            RuleId::D10u,
                            Consumer::Detecting,
                            DetectionKind::BudgetUnknown {
                                probe: u.probe.clone(),
                            },
                            u.reason.clone(),
                        )
                    } else {
                        observe(RuleId::D10u, ObserveWhy::BudgetUnknown)
                    }
                }
                Budgets::Known(others) => {
                    if before_reset && left > T_MIG && migration_available(ctx) {
                        if let Some(to) = others.iter().find(|a| *a != account) {
                            if s.hold.is_some() {
                                return observe(RuleId::D9, ObserveWhy::HeldAfterRelayedExit);
                            }
                            if let Some(u) = finish_axes_unknown(s) {
                                return observe(RuleId::D9, ObserveWhy::Unknown(u));
                            }
                            if !ctx.dials.wake.available() {
                                return observe(RuleId::D9, ObserveWhy::BudgetExhausted);
                            }
                            return Dec::new(
                                RuleId::D9,
                                Consumer::Gating,
                                Action::Migrate {
                                    to_account: to.clone(),
                                },
                            )
                            .why("parked", &s.parked.value);
                        }
                    }
                    // D10: nothing to migrate to (or too close, or the cap is
                    // spent): schedule against the stated reset, never close.
                    let exhausted = others.iter().all(|a| a == account);
                    let mut d = Dec::new(
                        RuleId::D10,
                        Consumer::GatingAndDetecting,
                        Action::ResumeAt {
                            at: *resets_at,
                            via: ResumeVia::AtReset,
                        },
                    )
                    .why("parked", &s.parked.value);
                    if exhausted {
                        d = d.cap(Cap::AlsoReport(Detection::new(
                            DetectionKind::AllAccountsExhausted {
                                resets_at: *resets_at,
                            },
                            "every account is out of budget; scheduled against the stated reset",
                        )));
                    }
                    d
                }
            }
        }
        Parked::Auth { account, .. } => {
            // D11: Migrate when another account has budget, and ask the operator
            // once per account.
            let can_migrate = match &ctx.dials.budgets {
                Budgets::Known(others) => others.iter().find(|a| *a != account).cloned(),
                Budgets::Unknown(_) => None,
            };
            let migrate = can_migrate
                .filter(|_| {
                    migration_available(ctx)
                        && s.hold.is_none()
                        && finish_axes_unknown(s).is_none()
                        && ctx.dials.wake.available()
                })
                .map(|to| {
                    Dec::new(
                        RuleId::D11,
                        Consumer::GatingAndDetecting,
                        Action::Migrate { to_account: to },
                    )
                    .why("parked", &s.parked.value)
                });
            credential_ask(
                ctx,
                RuleId::D11,
                account,
                "claude /login under that account",
                migrate,
            )
        }
        // D12 (§3.2): every request-error kind — `overloaded`, `server_error`,
        // `invalid_request`, `max_output_tokens`, `unknown` — gets ONE Continue
        // after a backoff ("the last turn failed with <kind>; continue it"); a
        // repeat is a Report. There is no `retryable` split: the table names no
        // kind that is only reported, so the earlier `retryable` field of
        // `Parked::ApiError` is dropped in favour of `since` (when the turn
        // ended on the error), which the backoff needs.
        Parked::ApiError { kind, since } => {
            let ready = since.plus(ERROR_BACKOFF);
            if ctx.now < ready {
                return observe(RuleId::D12, ObserveWhy::Backoff { until: ready });
            }
            let retry = [RETRY_SET.to_string()];
            let set = item_set_hash(&retry);
            let repeated = s
                .ledger
                .continues
                .iter()
                .any(|c| c.items == set && ctx.now.since(c.at) <= Dur::hours(1));
            if repeated {
                return report(
                    RuleId::D12,
                    Consumer::Gating,
                    DetectionKind::RetryRepeated,
                    "a retry continue was already sent and the session is parked again",
                );
            }
            if !s.mode.may_continue() {
                return report(
                    RuleId::D12,
                    Consumer::Gating,
                    DetectionKind::UnattendedOnly,
                    "an attended or unknown-mode session is never nudged",
                );
            }
            cont(
                ctx,
                RuleId::D12,
                Consumer::Gating,
                ContinueMsg::RetryAfterError { kind: *kind },
                &retry,
            )
        }
        // phase_of yields Parked only for the three arms above.
        Parked::None | Parked::Unknown(_) => {
            observe(RuleId::D8, ObserveWhy::Gated("not parked".into()))
        }
    }
}

// ---------------------------------------------------------------------------
// D13
// ---------------------------------------------------------------------------

fn scheduled(ctx: &Ctx) -> Dec {
    let s = ctx.s;
    let holding = s.holding_selfcont();
    let idle = matches!(s.activity.value, Activity::Idle { .. });
    let unattended_x = s.mode.may_continue() && !ctx.host.is_interim() && s.hold.is_none();

    // A wake lost on the resume.
    if let Some(at) = holding.iter().find_map(|e| match e {
        SelfCont::LostOnNewInstance { at } if ctx.now.since(*at) >= OVERDUE && *at <= ctx.now => {
            Some(*at)
        }
        _ => None,
    }) {
        if unattended_x {
            return cont(
                ctx,
                RuleId::D13,
                Consumer::Detecting,
                ContinueMsg::LostWake { at },
                &["__lostwake__".to_string()],
            );
        }
        return report(
            RuleId::D13,
            Consumer::Detecting,
            DetectionKind::ScheduleOverdue,
            "a pending wake was lost on restart; reported, not continued",
        );
    }

    // Declared finished while still armed.
    let armed_schedule = holding.iter().any(|e| {
        matches!(
            e,
            SelfCont::Wakeup { .. } | SelfCont::Cron { .. } | SelfCont::KeepaliveInferred { .. }
        )
    });
    if finished(s) && armed_schedule {
        if unattended_x {
            return cont(
                ctx,
                RuleId::D13,
                Consumer::Detecting,
                ContinueMsg::FinishedYetArmed,
                &["__finished_yet_armed__".to_string()],
            );
        }
        return report(
            RuleId::D13,
            Consumer::Detecting,
            DetectionKind::FinishedYetArmed,
            "W = Finished while a cron or wakeup is armed",
        );
    }

    // Measured overdue, and the UNKNOWN keepalive.
    if idle {
        let overdue = holding.iter().any(|e| match e {
            SelfCont::Wakeup { at, .. } => ctx.now.since(*at) >= OVERDUE && *at <= ctx.now,
            SelfCont::Cron { next, .. } => ctx.now.since(*next) >= OVERDUE && *next <= ctx.now,
            SelfCont::QueuedNotice { since } => ctx.now.since(*since) >= OVERDUE,
            _ => false,
        });
        if overdue {
            return report(
                RuleId::D13,
                Consumer::Detecting,
                DetectionKind::ScheduleOverdue,
                "a measured wakeup, cron or queued notice is 10 min overdue",
            );
        }
        let keepalive_overdue = holding.iter().any(|e| {
            matches!(e, SelfCont::KeepaliveInferred { at, .. }
                if *at <= ctx.now && ctx.now.since(*at) >= OVERDUE)
        });
        if keepalive_overdue {
            return report(
                RuleId::D13,
                Consumer::Detecting,
                DetectionKind::KeepaliveUnconfirmed,
                "an inferred keepalive is overdue; its firing is UNKNOWN",
            );
        }
    }
    observe(RuleId::D13, ObserveWhy::Scheduled)
}

// ---------------------------------------------------------------------------
// Stopped (D1/D14, D15 - D20)
// ---------------------------------------------------------------------------

fn stopped(ctx: &Ctx, st: &Stopped) -> Dec {
    let s = ctx.s;
    match st {
        // D14: as D1.
        Stopped::Revived => Dec::new(RuleId::D14, Consumer::Gating, Action::WithdrawFinish)
            .why("revived", &s.revived.value)
            .why("work", &s.work.value),
        Stopped::Owing => owing(ctx),
        Stopped::Undeclared => undeclared(ctx),
        Stopped::AwaitingFirstVerdict => {
            observe(RuleId::D17, ObserveWhy::AwaitingFirstVerdict).why("items", ctx.items.len())
        }
        Stopped::FinishedUnverified => finished_unverified(ctx),
        Stopped::FinishedPinned => finished_pinned(ctx),
        Stopped::Finished => d20(ctx),
    }
}

fn owing(ctx: &Ctx) -> Dec {
    let s = ctx.s;
    let owed = owed_facts(s, ctx.items);
    let ids: Vec<String> = owed.iter().map(|o| o.id.clone()).collect();
    let facts: Vec<String> = owed.iter().map(|o| o.fact.clone()).collect();
    // A held session receives none: its owed items stay its own, reported.
    if s.hold.is_some() {
        return report(
            RuleId::D15,
            Consumer::Gating,
            DetectionKind::HeldAfterRelayedExit,
            "owed items stay the held session's own; no Continue, no route step takes them",
        )
        .why("owed", &ids);
    }
    // A disputed set goes back to the ledger even when the session is Finished.
    if ids.iter().any(|i| s.ledger.disputes.contains(i)) {
        let disputed: Vec<String> = ids
            .iter()
            .filter(|i| s.ledger.disputes.contains(i))
            .cloned()
            .collect();
        return exhausted(ctx, disputed).why("owed", &ids);
    }
    // WithdrawFinish first on a Finished session (its read-back is the next
    // Decision's input).
    if finished(s) {
        return Dec::new(RuleId::D15, Consumer::Gating, Action::WithdrawFinish)
            .why("work", &s.work.value)
            .why("owed", &ids);
    }
    match s.mode {
        Mode::Unattended => cont(
            ctx,
            RuleId::D15,
            Consumer::Gating,
            ContinueMsg::OwedItems { facts },
            &ids,
        )
        .why("owed", &ids),
        Mode::Attended => {
            // One informational Continue, only when the owed fact is new and
            // about the session's own PRs, never on the interim host.
            let set = item_set_hash(&ids);
            let new_fact = !s.ledger.continues.iter().any(|c| c.items == set);
            let own_prs = ctx.items.iter().any(|i| {
                i.owed_by_session()
                    && matches!(
                        i.kind,
                        crate::model::ItemKind::CiRed
                            | crate::model::ItemKind::PrConflict
                            | crate::model::ItemKind::PrWatch
                    )
            });
            if new_fact && own_prs && !ctx.host.is_interim() {
                cont(
                    ctx,
                    RuleId::D15,
                    Consumer::Gating,
                    ContinueMsg::OwedItems { facts },
                    &ids,
                )
                .why("owed", &ids)
            } else {
                observe(
                    RuleId::D15,
                    ObserveWhy::Gated("attended: no new owed fact about its own PRs".into()),
                )
                .why("owed", &ids)
            }
        }
        // Unknown mode is treated as attended and never nudged.
        Mode::Unknown => observe(
            RuleId::D15,
            ObserveWhy::Gated("mode unknown: never nudged".into()),
        )
        .why("owed", &ids),
    }
}

fn undeclared(ctx: &Ctx) -> Dec {
    let s = ctx.s;
    if s.hold.is_some() {
        return observe(RuleId::D16, ObserveWhy::HeldAfterRelayedExit);
    }
    if matches!(s.work.value, Declared::Stalled) {
        return observe(
            RuleId::D16,
            ObserveWhy::Gated("already marked stalled".into()),
        );
    }
    let since_end = match &s.activity.value {
        Activity::Idle { since_turn_end, .. } => ctx.now.since(*since_turn_end),
        _ => Dur::ms(0),
    };
    let closeout = [CLOSEOUT_SET.to_string()];
    match s.mode {
        Mode::Unattended => {
            // Through the Stop-hook block at turn end when present, else after a
            // 15-minute settle.
            if !ctx.host.hooks && since_end < SETTLE {
                let until = match &s.activity.value {
                    Activity::Idle { since_turn_end, .. } => since_turn_end.plus(SETTLE),
                    _ => ctx.now,
                };
                return observe(RuleId::D16, ObserveWhy::Backoff { until });
            }
            cont(
                ctx,
                RuleId::D16,
                Consumer::Gating,
                ContinueMsg::Closeout {
                    reverify_first: since_end > Dur::hours(24),
                },
                &closeout,
            )
            .why("work", &s.work.value)
        }
        Mode::Attended | Mode::Unknown => {
            // After 24 h idle, once, only where the per-tab never-prompt switch
            // exists (build X) — and never to an unknown-mode session.
            let set = item_set_hash(&closeout);
            let sent = s.ledger.continues.iter().any(|c| c.items == set);
            if s.mode == Mode::Attended
                && since_end > Dur::hours(24)
                && ctx.dials.per_tab_never_prompt
                && !ctx.host.is_interim()
                && !sent
            {
                cont(
                    ctx,
                    RuleId::D16,
                    Consumer::Gating,
                    ContinueMsg::Closeout {
                        reverify_first: true,
                    },
                    &closeout,
                )
            } else {
                observe(RuleId::D16, ObserveWhy::Gated("attended: no prompt".into()))
            }
        }
    }
}

fn finished_unverified(ctx: &Ctx) -> Dec {
    let s = ctx.s;
    let dirty = dirty_trees(s);
    if !dirty.is_empty() {
        if !s.mode.may_continue() {
            return report(
                RuleId::D18,
                Consumer::Gating,
                DetectionKind::CustodyOwed,
                "a finished attended session owes custody; never nudged",
            );
        }
        let ids: Vec<String> = dirty.iter().map(|t| format!("custody:{t}")).collect();
        return cont(
            ctx,
            RuleId::D18,
            Consumer::Gating,
            ContinueMsg::DirtyTree { trees: dirty },
            &ids,
        );
    }
    // D19: Observe; Report after 30 min.
    let persisted = s
        .ledger
        .unknown_since
        .get("finished_unverified")
        .is_some_and(|t| ctx.now.since(*t) >= UNKNOWN_REPORT_AFTER);
    if persisted {
        report(
            RuleId::D19,
            Consumer::Gating,
            DetectionKind::FinishedUnverifiedUnknown,
            "a finished session has an unverified condition (Dirty or Unknown never closes)",
        )
    } else {
        observe(RuleId::D19, ObserveWhy::FinishedUnverifiedUnknown)
    }
}

fn finished_pinned(ctx: &Ctx) -> Dec {
    let s = ctx.s;
    let shells = match &s.activity.value {
        Activity::ShellPinned { shells, .. } => *shells,
        _ => 0,
    };
    if s.mode.may_continue() && !ctx.host.is_interim() && s.hold.is_none() {
        cont(
            ctx,
            RuleId::D19b,
            Consumer::GatingAndDetecting,
            ContinueMsg::StrayWaits { shells },
            &[STRAY_SET.to_string()],
        )
    } else {
        report(
            RuleId::D19b,
            Consumer::GatingAndDetecting,
            DetectionKind::FinishedButPinned,
            format!("{shells} stray wait loop(s) can never end; never closed while one runs (G3)"),
        )
    }
}

fn d20(ctx: &Ctx) -> Dec {
    let r = &ctx.report;
    if let Some(until) = close_backoff_until(ctx.s) {
        if ctx.now < until {
            return observe(RuleId::D20, ObserveWhy::Backoff { until });
        }
    }
    if let Some(a) = guard::close_action(r, ctx.now) {
        return Dec::new(RuleId::D20, Consumer::Gating, a);
    }
    if r.cleared_but_for_grace() {
        let until = r.grace_until.unwrap_or(ctx.now);
        return observe(RuleId::D20, ObserveWhy::GraceRunning { until });
    }
    let first: Option<&Guard> = r.first_not_true();
    let note = first.map_or_else(
        || "no conjunct".to_string(),
        |g| format!("{:?}: {}", g.id, g.note),
    );
    let persisted = ctx
        .s
        .ledger
        .unknown_since
        .get("close_not_cleared")
        .is_some_and(|t| ctx.now.since(*t) >= UNKNOWN_REPORT_AFTER);
    if persisted {
        report(
            RuleId::D20,
            Consumer::Gating,
            DetectionKind::CloseNotCleared,
            note,
        )
    } else {
        observe(RuleId::D20, ObserveWhy::Gated(note))
    }
}

// ---------------------------------------------------------------------------
// Closed: D1 (cut by a close), D2, D3, D4
// ---------------------------------------------------------------------------

/// Positive operator provenance (D3): ONLY an operator door of the close-site
/// roster, a user-session path with no relay `remote` block, or a typed exit the
/// funnel attributes to the webview's own keystrokes. Nothing a device credential
/// can produce counts; an unreadable or absent door is never D3.
pub fn operator_provenance(f: &ExitFacts) -> bool {
    match &f.door {
        Some(CloseDoor::OperatorTab) => return true,
        Some(CloseDoor::RelayUserSession {
            remote_block: false,
        }) => return true,
        Some(
            CloseDoor::RelayUserSession { remote_block: true }
            | CloseDoor::RelayDevice
            | CloseDoor::Agent { .. }
            | CloseDoor::Webview
            | CloseDoor::RunnerExit
            | CloseDoor::LifecycleClose,
        ) => return false,
        None => {}
    }
    matches!(
        f.typed,
        Some(
            TypedExit::OperatorKeystroke
                | TypedExit::RelayedUserSession {
                    remote_block: false
                }
        )
    )
}

/// `closed_by_creator` (N5-3): an exit through an `agent` door whose resolved
/// closer is positively the session that created the pane, of a pane no one but
/// that creator has written into since the spawn. An unresolvable closer or
/// creator is UNKNOWN and stays D4.
pub fn closed_by_creator(f: &ExitFacts) -> Tri {
    let Some(CloseDoor::Agent { closer }) = &f.door else {
        return Tri::False;
    };
    let (Principal::Session(closer), Principal::Session(creator)) = (closer, &f.creator) else {
        return Tri::unknown("closer", "closer or creator unresolvable");
    };
    if closer != creator {
        return Tri::False;
    }
    let only_creator = f.writes.iter().all(|w| match &w.writer {
        Writer::AgentDoor(Principal::Session(w)) => w == creator,
        Writer::AgentDoor(Principal::Unresolvable) => false,
        Writer::HttpCreateInitialCommand | Writer::ControlResponse => true,
        Writer::OperatorKeystroke | Writer::RelayedInput | Writer::LifecycleDelivery => false,
    });
    if f.writes
        .iter()
        .any(|w| matches!(&w.writer, Writer::AgentDoor(Principal::Unresolvable)))
    {
        return Tri::unknown("writer", "a write's author is unresolvable");
    }
    Tri::from_bool(only_creator)
}

/// A relayed exit: the operator's `/exit` in a REMOTE tab, which no rule can
/// tell from an agent's.
pub fn relayed_exit(f: &ExitFacts) -> bool {
    matches!(
        f.typed,
        Some(TypedExit::RelayedUnstamped | TypedExit::RelayedUserSession { remote_block: true })
    )
}

/// Whether a bare `--resume` brings back a schedule the session's transcript
/// shows at the exit (m9-1, PRES-9): a recurring session cron under 7 days old,
/// a one-shot with a future fire, a wakeup still ahead, a durable cron, an
/// inferred keepalive (its re-arming UNKNOWN), or an S axis unreadable at the
/// exit.
pub fn resume_would_restore_schedule(exit: Option<&SelfConts>, at: Timestamp) -> Tri {
    match exit {
        None => Tri::unknown("selfcont", "no S read at the exit"),
        Some(SelfConts::Unknown(u)) => Tri::Unknown(u.clone()),
        Some(SelfConts::Known(v)) => Tri::from_bool(v.iter().any(|e| match e {
            SelfCont::Wakeup { at: w, .. } => *w > at,
            SelfCont::Cron { kind, .. } => match kind {
                CronKind::Durable => true,
                CronKind::Recurring { created } => at.since(*created) < crate::state::CRON_EXPIRY,
                CronKind::OneShot { fire } => *fire > at,
            },
            SelfCont::KeepaliveInferred { .. } => true,
            SelfCont::AutoContinue { .. }
            | SelfCont::QueuedNotice { .. }
            | SelfCont::InFlightDelivery { .. }
            | SelfCont::PendingMessage { .. }
            | SelfCont::LostOnNewInstance { .. } => false,
        })),
    }
}

fn outcome_cap(ctx: &Ctx, f: &ExitFacts) -> Option<Cap> {
    // #2373: a crashed runner's `work_unreported` report names no session, and a
    // shell-hosted restored continuation's exit code is withheld, so the outcome
    // is `unknown` or `closed` — never `crashed` or `clean`.
    ctx.s.continuation_gate.as_ref()?;
    let positively_closed = f.door.is_some() || f.typed.is_some();
    let o = if ctx.s.shell_hosted || f.code.is_none() {
        if positively_closed {
            ContinuationOutcome::Closed
        } else {
            ContinuationOutcome::Unknown
        }
    } else if positively_closed {
        ContinuationOutcome::Closed
    } else {
        return None;
    };
    Some(Cap::ContinuationOutcome(o))
}

fn closed(ctx: &Ctx, is_finished: bool) -> Dec {
    let s = ctx.s;
    let Hosting::Exited(f) = &s.hosting.value else {
        return observe(RuleId::D2, ObserveWhy::Gated("no exit facts".into()));
    };
    let mut d = closed_inner(ctx, f, is_finished);
    if let Some(c) = outcome_cap(ctx, f) {
        d = d.cap(c);
    }
    d.why("exit", f)
}

fn closed_inner(ctx: &Ctx, f: &ExitFacts, is_finished: bool) -> Dec {
    let s = ctx.s;
    // D2: the lifecycle's own close, read back.
    if matches!(f.door, Some(CloseDoor::LifecycleClose))
        || matches!(f.typed, Some(TypedExit::Lifecycle))
    {
        if !f.read_back_gone {
            return observe(RuleId::D2, ObserveWhy::Gated("close not read back".into()));
        }
        // D1: a close that cut a revival.
        if s.revived.value.is_true() {
            let cut = [CUT_SET.to_string()];
            let _ = cut;
            return Dec::new(
                RuleId::D1,
                Consumer::Gating,
                Action::ResumeAt {
                    at: ctx.now,
                    via: ResumeVia::WithMessage,
                },
            )
            .why("revived", &s.revived.value);
        }
        return report(
            RuleId::D2,
            Consumer::Neither,
            DetectionKind::Closed,
            "the lifecycle's own close, read back",
        );
    }
    // D3: positive operator provenance only.
    if operator_provenance(f) {
        return report(
            RuleId::D3,
            Consumer::Neither,
            DetectionKind::ClosedByOperator,
            format!("work axis as it stood: {:?}; never resumed", s.work.value),
        );
    }
    // D4 applies only while W is not Finished.
    if is_finished {
        return report(
            RuleId::D4,
            Consumer::Gating,
            DetectionKind::ExitedFinished,
            "exited with W = Finished: nothing to resume",
        );
    }
    match closed_by_creator(f) {
        Tri::True => {
            return report(
                RuleId::D4,
                Consumer::Neither,
                DetectionKind::ClosedByCreator,
                "the creator cleaned up what it opened; the audit trail, not a product close",
            )
        }
        Tri::False | Tri::Unknown(_) => {}
    }
    // The held-after-relayed-exit hold.
    if relayed_exit(f) {
        return relayed_hold(ctx, f);
    }
    d4_resume(ctx, f)
}

fn hold_notice_start(ctx: &Ctx, f: &ExitFacts, resumed: bool) -> Option<Notice> {
    let first = ctx.s.hold.as_ref().is_none_or(|h| h.last_notice.is_none());
    first.then(|| Notice::HoldStart {
        resumed,
        exit_at: f.at,
        owes: owed_facts(ctx.s, ctx.items)
            .into_iter()
            .map(|o| o.fact)
            .collect(),
    })
}

fn relayed_hold(ctx: &Ctx, f: &ExitFacts) -> Dec {
    let s = ctx.s;
    // Resumed with no prompt only when its transcript shows no schedule a resume
    // would bring back, and only where a resumer exists (the interim host never
    // resumes for D4).
    let schedules = resume_would_restore_schedule(s.exit_schedules.as_ref(), f.at);
    let can_resume =
        ctx.host.resume == ResumeCap::Resumer && schedules == Tri::False && resume_allowed(ctx);
    let mut d = if can_resume {
        Dec::new(
            RuleId::D4,
            Consumer::Gating,
            Action::ResumeAt {
                at: ctx.now,
                via: ResumeVia::BareResume,
            },
        )
    } else {
        report(
            RuleId::D4,
            Consumer::Gating,
            DetectionKind::HeldAfterRelayedExit,
            "exited through relayed input; not resumed (schedules a resume would restore, no resumer, or a budget)",
        )
    }
    .cap(Cap::Hold { resumed: can_resume })
    .why("schedules", &schedules);
    if let Some(n) = hold_notice_start(ctx, f, can_resume) {
        d = d.notice(n);
    }
    d
}

/// The standing conditions a D4 resume needs.
fn resume_allowed(ctx: &Ctx) -> bool {
    let s = ctx.s;
    ctx.dials.resume_unfinished.is_on()
        && ctx.dials.resume_account_has_budget == Tri::True
        && ctx.dials.wake.available()
        && !s.operator_hold
        && !matches!(s.pane_kind, PaneKind::Steward | PaneKind::Looping)
        && recent(&s.ledger.resume_attempts, ctx.now, Dur::hours(24)) < RESUME_ATTEMPTS_CAP
}

fn d4_resume(ctx: &Ctx, f: &ExitFacts) -> Dec {
    let _ = f;
    let s = ctx.s;
    // A steward or looping-agent pane: its launcher owns the relaunch.
    if matches!(s.pane_kind, PaneKind::Steward | PaneKind::Looping) {
        return report(
            RuleId::D4,
            Consumer::Gating,
            DetectionKind::ExitedUnresumed,
            "a steward or looping pane: its launcher owns its relaunch",
        );
    }
    // The interim host never resumes for D4: it only reports.
    if ctx.host.resume != ResumeCap::Resumer {
        return report(
            RuleId::D4,
            Consumer::Gating,
            DetectionKind::ExitedUnresumed,
            "the interim host never resumes for D4 (its resume door remedies only its own close)",
        );
    }
    if s.operator_hold {
        return report(
            RuleId::D4,
            Consumer::Gating,
            DetectionKind::ExitedUnresumed,
            "held for the operator's Restore (r2)",
        );
    }
    if recent(&s.ledger.resume_attempts, ctx.now, Dur::hours(24)) >= RESUME_ATTEMPTS_CAP {
        return report(
            RuleId::D4,
            Consumer::Gating,
            DetectionKind::ResumeItem,
            "3 resume attempts in 24 h: opened a `resume` work item",
        );
    }
    if !ctx.dials.resume_unfinished.is_on() {
        return report(
            RuleId::D4,
            Consumer::Gating,
            DetectionKind::ResumeRefused,
            "the tenant's resume_unfinished_enabled is not on",
        );
    }
    if ctx.dials.resume_account_has_budget != Tri::True {
        return report(
            RuleId::D4,
            Consumer::Gating,
            DetectionKind::ResumeRefused,
            "no account with budget is proven for the resume",
        );
    }
    if !ctx.dials.wake.available() {
        return observe(RuleId::D4, ObserveWhy::BudgetExhausted);
    }
    Dec::new(
        RuleId::D4,
        Consumer::Gating,
        Action::ResumeAt {
            at: ctx.now,
            via: ResumeVia::WithMessage,
        },
    )
}

// ---------------------------------------------------------------------------
// The hold's riders
// ---------------------------------------------------------------------------

/// Every verdict of a held session carries the `hold` cap (coord learns the hold
/// from the lifecycle verdict), plus the once-per-24 h "newly owes" notice.
fn hold_riders(ctx: &Ctx, d: &mut Dec) {
    let Some(h) = &ctx.s.hold else { return };
    if !d.caps.iter().any(|c| matches!(c, Cap::Hold { .. })) {
        d.caps.push(Cap::Hold { resumed: h.resumed });
    }
    if matches!(ctx.s.hosting.value, Hosting::Exited(_)) {
        return;
    }
    let owed = owed_facts(ctx.s, ctx.items);
    if owed.is_empty() {
        return;
    }
    let ids: Vec<String> = owed.iter().map(|o| o.id.clone()).collect();
    let key = format!("hold-owed:{}", item_set_hash(&ids));
    let newly = !ctx.s.ledger.notices.contains_key(&key);
    let spaced = h
        .last_notice
        .is_none_or(|t| ctx.now.since(t) >= Dur::hours(24));
    if newly && spaced {
        d.notices.push(Notice::HoldNewlyOwes {
            owes: owed.into_iter().map(|o| o.fact).collect(),
        });
    }
}

#[cfg(test)]
mod tests;
