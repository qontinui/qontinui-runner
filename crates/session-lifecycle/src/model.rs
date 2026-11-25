//! The types `decide` takes and returns (architecture §3.1, §3.4, §2.8): the
//! dials, the host's capabilities, the work-item view, and the `Decision` with
//! its one `Action`.
//!
//! **S3 (operator-lane purity) by construction.** [`OperatorTask`] has private
//! fields and ONE constructor, [`OperatorTask::new`], whose first argument is a
//! [`Basis`]: a [`ClosedListItem`] or a [`ReservedAct`]. No constructor takes free
//! text as a basis, a symptom, a capacity condition or a "stuck" detection; those
//! are [`Detection`]s (`Report`), routed to agent lanes.

use std::fmt::Write as _;

use crate::state::{AccountId, ApiErrorKind, Digest, Dur, Timestamp, Tri, Unk};

/// The kernel's version, stamped on every `Decision` (§3.1: version-stamped).
pub const FN_VERSION: SemVer = SemVer {
    major: 0,
    minor: 1,
    patch: 0,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct SemVer {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

// ---------------------------------------------------------------------------
// Dials and host capabilities
// ---------------------------------------------------------------------------

/// A preference dial. A dial that cannot be read HOLDS ITS LAST KNOWN VALUE; one
/// never read at all is `Unknown` (G9; `capability-ships-enabled`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dial {
    On,
    Off,
    Unknown,
}

impl Dial {
    pub fn is_on(self) -> bool {
        self == Dial::On
    }
}

/// What coord's account roster says about budget (`spawn-routing-reads-account-
/// budget`). `Unknown` is never equated with "full" (D10u).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Budgets {
    /// Accounts other than the session's own that HAVE budget, in preference order.
    Known(Vec<AccountId>),
    Unknown(Unk),
}

/// The device's single wake budget (§3.2 "Budgets").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WakeBudget {
    /// The device's measured headroom (`session_target - active`) where fresh;
    /// `None` is UNKNOWN and never lowers `k`.
    pub headroom: Option<u32>,
    /// Lifecycle-initiated turns already issued this tick.
    pub used_this_tick: u32,
}

impl WakeBudget {
    /// `k = 4`, lowered to the measured headroom, and never below 1.
    pub fn k(&self) -> u32 {
        const K: u32 = 4;
        self.headroom.map_or(K, |h| h.clamp(1, K))
    }
    pub fn available(&self) -> bool {
        self.used_this_tick < self.k()
    }
}

/// The preference dials, the safety latch and the capacity reading `decide` is
/// parameterised by. Capacity enters ONLY as `grace_override` (S5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dials {
    /// `lifecycle.close`.
    pub close: Dial,
    /// `lifecycle.continue`.
    pub cont: Dial,
    /// The tenant's `resume_unfinished_enabled`.
    pub resume_unfinished: Dial,
    /// `lifecycle_close_latch` for THIS `fn_version`.
    pub latch: crate::state::Latch,
    /// How long the latch has been unreadable, as of `now` (past one poll
    /// interval an unreadable latch reads tripped).
    pub latch_poll_interval: Dur,
    /// Capacity-shortened grace (>= `GRACE_MIN`), set only when coord reports
    /// queued demand on this device's binding lane no other device can take.
    pub grace_override: Option<Dur>,
    /// Other accounts' budget for D9-D11.
    pub budgets: Budgets,
    /// The session's own account has budget for a resume (A3/D4).
    pub resume_account_has_budget: Tri,
    pub wake: WakeBudget,
    /// A fleet drain set by the operator or the restart executor's self-hold
    /// (interim narrowing I8: the CLI takes no act at all).
    pub drain_or_self_hold: bool,
    /// The per-tab "never prompt" switch exists on this host for attended
    /// sessions (build X only), D16's attended arm.
    pub per_tab_never_prompt: bool,
}

impl Dials {
    /// Dials for a box where everything is on and nothing is constrained.
    pub fn permissive() -> Dials {
        Dials {
            close: Dial::On,
            cont: Dial::On,
            resume_unfinished: Dial::On,
            latch: crate::state::Latch::Clear,
            latch_poll_interval: Dur::mins(1),
            grace_override: None,
            budgets: Budgets::Known(Vec::new()),
            resume_account_has_budget: Tri::True,
            wake: WakeBudget {
                headroom: None,
                used_this_tick: 0,
            },
            drain_or_self_hold: false,
            per_tab_never_prompt: false,
        }
    }
}

/// How a close is carried out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseCap {
    /// The interim CLI: `/exit` typed through `POST /terminals/{id}/write`.
    TypedExitRawWrite,
    /// Build X: through the repaired `graceful_exit` and the funnel.
    TypedExitFunnel,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContinueCap {
    /// The interim host: paste only.
    PasteOnly,
    /// Build X: Stop-hook block, then pump, then paste.
    StopBlockPumpPaste,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResumeCap {
    /// The interim host resumes only to remedy its own close.
    ResumeDoorAfterUnfinish,
    /// Build X's Resumer.
    Resumer,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FunnelCap {
    ScreenWitness,
    Exact,
}

/// What the host can do: a weaker host can only produce a more conservative
/// decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostCaps {
    pub close: CloseCap,
    pub cont: ContinueCap,
    pub resume: ResumeCap,
    pub funnel: FunnelCap,
    /// Hooks are delivered over HTTP (build X).
    pub hooks: bool,
    /// The running runner build is on the interim allow-list (I1); true on
    /// build X.
    pub build_allowlisted: bool,
    /// The cell (pane shape x Claude Code version x build) passed the E-CL1,
    /// E-DR1 and E-PT1 experiments (I1).
    pub cell_allowlisted: bool,
}

impl HostCaps {
    /// Build X.
    pub const BUILD_X: HostCaps = HostCaps {
        close: CloseCap::TypedExitFunnel,
        cont: ContinueCap::StopBlockPumpPaste,
        resume: ResumeCap::Resumer,
        funnel: FunnelCap::Exact,
        hooks: true,
        build_allowlisted: true,
        cell_allowlisted: true,
    };
    /// The interim CLI on the running build.
    pub const INTERIM: HostCaps = HostCaps {
        close: CloseCap::TypedExitRawWrite,
        cont: ContinueCap::PasteOnly,
        resume: ResumeCap::ResumeDoorAfterUnfinish,
        funnel: FunnelCap::ScreenWitness,
        hooks: false,
        build_allowlisted: true,
        cell_allowlisted: true,
    };
    pub fn is_interim(&self) -> bool {
        self.resume == ResumeCap::ResumeDoorAfterUnfinish
    }
}

// ---------------------------------------------------------------------------
// Work items (§2.8)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemKind {
    CiFirstVerdict,
    CiRed,
    PrConflict,
    EscalateHit,
    PrWatch,
    WaitGate,
    Question,
    Custody,
    WorkUnitScope,
    Finding,
    Resume,
}

/// Who must act.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Owner {
    /// The session this view is of.
    AuthorSession,
    ResumedAuthor,
    Fixer,
    MainFixLane,
    InfraLane,
    Ledger,
    Operator,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ItemState {
    Open { owner: Owner, deadline: Timestamp },
    Watched { lane: String, ack: bool },
    Pending { until: Timestamp },
    Impeded { refusal: String },
    Done,
    Deferred { trigger: String },
    Surfaced,
    Unknown(Unk),
}

/// One owned item as the ledger serves it (C2, <= tau_wl).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkItemView {
    pub id: String,
    pub kind: ItemKind,
    pub state: ItemState,
    /// A `ci_first_verdict` still inside its hold (§5.5): not terminal even
    /// though `Pending`.
    pub inside_first_verdict_hold: bool,
    /// A human-readable owed fact for a Continue (the clause named with it).
    pub fact: String,
}

impl WorkItemView {
    /// The session itself owes this item: `Open{owner: author session | resumed
    /// author}`, or an `Impeded` item (stays unfinished, `an-agent-clearable-
    /// escalate-hit-is-owed-not-watched`). Never `Unknown` (that is D0).
    pub fn owed_by_session(&self) -> bool {
        match &self.state {
            ItemState::Open { owner, .. } => {
                matches!(owner, Owner::AuthorSession | Owner::ResumedAuthor)
            }
            ItemState::Impeded { .. } => true,
            _ => false,
        }
    }
    /// Terminal for the owning session (the §2.8 column).
    pub fn terminal_for_session(&self) -> Tri {
        match &self.state {
            ItemState::Open { .. } | ItemState::Impeded { .. } => Tri::False,
            ItemState::Pending { .. } if self.inside_first_verdict_hold => Tri::False,
            ItemState::Pending { .. }
            | ItemState::Watched { .. }
            | ItemState::Done
            | ItemState::Deferred { .. }
            | ItemState::Surfaced => Tri::True,
            ItemState::Unknown(u) => Tri::Unknown(u.clone()),
        }
    }
    pub fn awaiting_first_verdict(&self) -> bool {
        self.kind == ItemKind::CiFirstVerdict
            && self.inside_first_verdict_hold
            && matches!(self.state, ItemState::Pending { .. })
    }
}

// ---------------------------------------------------------------------------
// The operator lane as types (§3.4)
// ---------------------------------------------------------------------------

pub type UiLocation = String;
pub type ResourceId = String;
pub type DoorId = String;
pub type ClauseRef = String;
pub type DeviceId = String;
pub type GitSha = String;
pub type EvidenceRef = String;

/// `escalation-bar` `escalation-closed-list`; each variant carries checkable
/// evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClosedListItem {
    /// Case 3: an act only a human can physically perform.
    CredentialOnlyHumanHolds {
        account_id: AccountId,
        ui_location: UiLocation,
    },
    ResourceSessionCannotObtain {
        resource: ResourceId,
        ui_location: UiLocation,
        cascade_tried: Vec<DoorId>,
    },
    ProductOrStrategyCall {
        fork: String,
        options: Vec<String>,
    },
    GenuinePriorityTie {
        a: String,
        b: String,
        ranking_applied: ClauseRef,
    },
}

/// Acts whose served tier is ask-first / operator-only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReservedAct {
    Reserved { document: String, clause: ClauseRef },
    RunningRunnerRestart { device: DeviceId, build: GitSha },
    OperatorAudienceGate { gate_id: String },
}

/// The only two bases an operator task can have.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Basis {
    Closed(ClosedListItem),
    Reserved(ReservedAct),
}

impl From<ClosedListItem> for Basis {
    fn from(c: ClosedListItem) -> Basis {
        Basis::Closed(c)
    }
}
impl From<ReservedAct> for Basis {
    fn from(r: ReservedAct) -> Basis {
        Basis::Reserved(r)
    }
}

/// An ask to the operator. Private fields, one constructor (S3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperatorTask {
    basis: Basis,
    recommendation: String,
    evidence: Vec<EvidenceRef>,
    documents_read: Vec<ClauseRef>,
    why_none_decides: String,
    policy_gap_finding_id: String,
    clear_predicate: String,
    /// Dedupe key (e.g. once per account, D11).
    dedupe_key: String,
}

impl OperatorTask {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        basis: impl Into<Basis>,
        recommendation: String,
        evidence: Vec<EvidenceRef>,
        documents_read: Vec<ClauseRef>,
        why_none_decides: String,
        policy_gap_finding_id: String,
        clear_predicate: String,
    ) -> OperatorTask {
        let basis = basis.into();
        let dedupe_key = match &basis {
            Basis::Closed(ClosedListItem::CredentialOnlyHumanHolds { account_id, .. }) => {
                format!("credential:{account_id}")
            }
            other => format!("{other:?}"),
        };
        OperatorTask {
            basis,
            recommendation,
            evidence,
            documents_read,
            why_none_decides,
            policy_gap_finding_id,
            clear_predicate,
            dedupe_key,
        }
    }
    pub fn basis(&self) -> &Basis {
        &self.basis
    }
    pub fn recommendation(&self) -> &str {
        &self.recommendation
    }
    pub fn evidence(&self) -> &[EvidenceRef] {
        &self.evidence
    }
    pub fn documents_read(&self) -> &[ClauseRef] {
        &self.documents_read
    }
    pub fn why_none_decides(&self) -> &str {
        &self.why_none_decides
    }
    pub fn policy_gap_finding_id(&self) -> &str {
        &self.policy_gap_finding_id
    }
    pub fn clear_predicate(&self) -> &str {
        &self.clear_predicate
    }
    pub fn dedupe_key(&self) -> &str {
        &self.dedupe_key
    }
}

// ---------------------------------------------------------------------------
// Decision
// ---------------------------------------------------------------------------

/// The row of §3.2 that fired.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RuleId {
    D0,
    D1,
    D2,
    D3,
    D4,
    D5,
    D6,
    D7,
    D8,
    D9,
    D10,
    D10u,
    D11,
    D12,
    D13,
    D14,
    D15,
    D16,
    D17,
    D18,
    D19,
    D19b,
    D20,
    D21,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Consumer {
    Gating,
    Detecting,
    GatingAndDetecting,
    /// A rule with no consumer column (`—`).
    Neither,
}

/// Why an `Observe` observes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ObserveWhy {
    Unknown(Unk),
    Working,
    Delegated,
    Spawning,
    Draft,
    Dialog,
    ClaudeContinuesItself,
    BudgetUnknown,
    Scheduled,
    AwaitingFirstVerdict,
    FinishedUnverifiedUnknown,
    GraceRunning { until: Timestamp },
    HeldAfterRelayedExit,
    BudgetExhausted,
    DialOff,
    HostCannot,
    Backoff { until: Timestamp },
    NothingOwed,
    Gated(String),
}

/// A Continue's message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContinueMsg {
    /// D15: each owed fact with its clause.
    OwedItems { facts: Vec<String> },
    /// D16: a closeout request; `reverify_first` for a session idle > 24 h.
    Closeout { reverify_first: bool },
    /// D12.
    RetryAfterError { kind: ApiErrorKind },
    /// D13: a wake lost on restart.
    LostWake { at: Timestamp },
    /// D13 build X: finished with live schedules.
    FinishedYetArmed,
    /// D18.
    DirtyTree { trees: Vec<String> },
    /// D19b: the stray wait loops that can never end.
    StrayWaits { shells: u32 },
    /// D1: "your last turn was cut by a close; continue it".
    CutByClose,
    /// D9's resubmit after a migration.
    ResubmitInterrupted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lane {
    StopBlock,
    Pump,
    Paste,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LanePlan {
    pub lanes: Vec<Lane>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResumeVia {
    /// A bare `--resume`, no prompt (a held session, D4).
    BareResume,
    /// Resumed with a first message.
    WithMessage,
    /// `ResumeAt` the limit's reset (D10).
    AtReset,
}

/// A `Report`'s subject.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DetectionKind {
    UnknownPersisted { probe: String },
    Closed,
    ClosedByOperator,
    ClosedByCreator,
    ExitedFinished,
    ExitedUnresumed,
    DraftStale,
    DialogStale,
    AllAccountsExhausted { resets_at: Timestamp },
    BudgetUnknown { probe: String },
    ScheduleOverdue,
    KeepaliveUnconfirmed,
    FinishedYetArmed,
    FinishedButPinned,
    ContinueExhausted,
    Stalled,
    DisputedByAuthor { items: Vec<String> },
    RetryRepeated,
    CloseRefused { guard: String },
    UnattendedOnly,
    ResumeItem,
    HeldAfterRelayedExit,
    FinishedUnverifiedUnknown,
    CustodyOwed,
    ParkedNotRetryable,
    ParkedAuthNoMigration,
    CloseNotCleared,
    ResumeRefused,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Detection {
    pub kind: DetectionKind,
    pub detail: String,
}

impl Detection {
    pub fn new(kind: DetectionKind, detail: impl Into<String>) -> Detection {
        Detection {
            kind,
            detail: detail.into(),
        }
    }
}

/// A capability the lifecycle verdict carries beside the one Action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cap {
    /// The session is held after a relayed exit (D4): no Continue of any rule,
    /// no queued message; coord reads it from the lifecycle verdict so its
    /// ledger, gate engine and legacy fixer dispatch leave the work where it is.
    Hold { resumed: bool },
    /// D21: unattended and NOT declared Finished: C4 `stalled`.
    MarkStalled,
    /// D21: re-queue disputed items for classification.
    RequeueDisputed { items: Vec<String> },
    /// The continuation's outcome, for the #2373 crash-continuation rule.
    ContinuationOutcome(ContinuationOutcome),
    /// A detection that rides beside a non-Report action (D10's
    /// `all_accounts_exhausted` beside `ResumeAt`).
    AlsoReport(Detection),
    /// An operator ask that rides beside a Migrate (D11's once-per-account
    /// login request). Still an [`OperatorTask`], so still S3-pure.
    AlsoNotify(OperatorTask),
}

/// What a restored continuation's exit means (#2373): a crashed runner's
/// `work_unreported` report names no session, and a shell-hosted restored
/// continuation's exit code is withheld, so the outcome is `Unknown` or
/// `Closed` — never `Crashed` or `Clean`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContinuationOutcome {
    Unknown,
    Closed,
}

/// An information-form notice (`POST /coord/agent-notifications`), NOT an
/// `OperatorTask`: it asks for no decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Notice {
    /// ONE at the hold's start.
    HoldStart {
        resumed: bool,
        exit_at: Timestamp,
        owes: Vec<String>,
    },
    /// Once more only when it newly owes something while held, at most once per
    /// session per 24 h.
    HoldNewlyOwes { owes: Vec<String> },
}

/// Private-constructor proof that the close guard cleared: the only way to hold
/// one is [`crate::guard::GuardReport::close_proof`] (S1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloseProof {
    pub(crate) digest: Digest,
    _sealed: (),
}

impl CloseProof {
    pub(crate) fn seal(digest: Digest) -> CloseProof {
        CloseProof {
            digest,
            _sealed: (),
        }
    }
    pub fn digest(&self) -> Digest {
        self.digest
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Observe {
        why: ObserveWhy,
    },
    Continue {
        message: ContinueMsg,
        lane: LanePlan,
    },
    /// The only close; ONE construction site (`guard::close_action`).
    Close {
        not_before: Timestamp,
        proof: CloseProof,
    },
    WithdrawFinish,
    Migrate {
        to_account: AccountId,
    },
    ResumeAt {
        at: Timestamp,
        via: ResumeVia,
    },
    NotifyOperator {
        task: OperatorTask,
    },
    Report {
        detection: Detection,
    },
}

impl Action {
    pub fn is_close(&self) -> bool {
        matches!(self, Action::Close { .. })
    }
    pub fn is_continue(&self) -> bool {
        matches!(self, Action::Continue { .. })
    }
    /// An action that moves the world (anything but Observe / Report).
    pub fn acts(&self) -> bool {
        !matches!(self, Action::Observe { .. } | Action::Report { .. })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuardId {
    G1,
    G2,
    G3,
    G4,
    G5,
    G6,
    G7,
    G8,
    G9,
    /// Interim narrowing I1 (cell and build allow-list).
    I1,
    /// I6: an attended session is closed only after 60 min idle.
    I6,
    /// I8: no drain or self-hold.
    I8,
    /// The close dial.
    Dial,
}

/// A conjunct evaluated: True | False | Unknown(probe, why).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Guard {
    pub id: GuardId,
    pub value: Tri,
    pub note: String,
}

/// Each reason names the axis value it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reason {
    pub axis: &'static str,
    pub value: String,
}

impl Reason {
    pub fn new(axis: &'static str, value: impl std::fmt::Debug) -> Reason {
        Reason {
            axis,
            value: format!("{value:?}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    pub action: Action,
    pub rule: RuleId,
    pub reasons: Vec<Reason>,
    pub guards: Vec<Guard>,
    pub consumer: Consumer,
    pub inputs_digest: Digest,
    pub fn_version: SemVer,
    /// Capabilities and verdict facts that ride beside the one Action.
    pub caps: Vec<Cap>,
    /// Information-form operator notices (never a decision request).
    pub notices: Vec<Notice>,
}

/// FNV-1a 64 over the `Debug` rendering of the inputs: deterministic because the
/// state holds only `Vec` and `BTreeMap` (no hash-ordered containers).
pub fn digest_of(parts: &[&dyn std::fmt::Debug]) -> Digest {
    let mut text = String::new();
    for p in parts {
        let _ = write!(text, "{p:?}\u{1f}");
    }
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in text.as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    Digest(h)
}

/// FNV-1a of a set of item ids, order-independent (ids are sorted first).
pub fn item_set_hash(ids: &[String]) -> u64 {
    let mut v: Vec<&String> = ids.iter().collect();
    v.sort();
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for id in v {
        for b in id.as_bytes() {
            h ^= u64::from(*b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        h ^= 0xff;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}
