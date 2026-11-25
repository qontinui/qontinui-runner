//! The state vector (architecture §2.1), three-valued logic, `fold`, and the
//! input-facts vocabulary the kernel owns.
//!
//! Everything here is plain data. A session's state is eight orthogonal axes,
//! each wrapped in [`Observed`] (value + `observed_at`) and each with an explicit
//! `Unknown(probe, reason)` arm: nothing defaults (§2.1). [`fold`] is the pure
//! reducer that turns events into that vector; [`phase_of`] is §2.2's priority
//! table as an exhaustive match.
//!
//! ## Vocabulary the other phases conform to
//!
//! [`CloseDoor`], [`Writer`] / [`PaneWrite`], [`CloseAuthority`] and the
//! [`ExitFacts`] they ride in are the kernel's definition of what the runner
//! stores in a pane's exit facts (Phase 15), what the input funnel stamps on each
//! write (Phases 80 and 83) and what a minted close authority is (Phase 90). The
//! runner conforms to these types; the kernel never reads a field a device
//! credential can set (D3, §3.2).
//!
//! Collections are `Vec`/`BTreeMap` only, so `Debug` output (the `inputs_digest`
//! preimage) is deterministic.

use std::collections::BTreeMap;

// ---------------------------------------------------------------------------
// Time
// ---------------------------------------------------------------------------

/// Milliseconds since the Unix epoch. The kernel never reads a clock: `now` is
/// an argument (§3.1 "no clock reads").
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Timestamp(pub u64);

/// A length of time in milliseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Dur(pub u64);

impl Dur {
    pub const fn ms(n: u64) -> Dur {
        Dur(n)
    }
    pub const fn secs(n: u64) -> Dur {
        Dur(n * 1_000)
    }
    pub const fn mins(n: u64) -> Dur {
        Dur(n * 60_000)
    }
    pub const fn hours(n: u64) -> Dur {
        Dur(n * 3_600_000)
    }
    pub const fn days(n: u64) -> Dur {
        Dur(n * 86_400_000)
    }
}

impl Timestamp {
    pub const fn plus(self, d: Dur) -> Timestamp {
        Timestamp(self.0.saturating_add(d.0))
    }
    pub const fn minus(self, d: Dur) -> Timestamp {
        Timestamp(self.0.saturating_sub(d.0))
    }
    /// `self - earlier`, saturating at zero (an `earlier` in the future reads 0).
    pub const fn since(self, earlier: Timestamp) -> Dur {
        Dur(self.0.saturating_sub(earlier.0))
    }
}

/// The idle-grace window g (§2.5, D20).
pub const GRACE: Dur = Dur::mins(10);
/// The grace floor capacity may shorten it to (D20).
pub const GRACE_MIN: Dur = Dur::mins(2);
/// `T_mig`: a limit reset closer than this is left to Claude Code's own countdown (D8).
pub const T_MIG: Dur = Dur::mins(60);
/// An UNKNOWN or detection persisting this long is reported (D0, D9, D19).
pub const UNKNOWN_REPORT_AFTER: Dur = Dur::mins(30);
/// Overdue bound for a wakeup, cron, queued notice or lost wake (D13).
pub const OVERDUE: Dur = Dur::mins(10);
/// Keepalive length (§2.4: 1,200 s).
pub const KEEPALIVE: Dur = Dur::secs(1_200);
/// A recurring session cron expires this long after creation (§2.1 S).
pub const CRON_EXPIRY: Dur = Dur::days(7);

/// Freshness bounds, §2.5.
pub mod tau {
    use super::Dur;
    pub const H: Dur = Dur::secs(10);
    pub const A: Dur = Dur::secs(60);
    pub const I: Dur = Dur::secs(60);
    pub const S: Dur = Dur::secs(60);
    pub const P: Dur = Dur::secs(60);
    /// `τ_wl`.
    pub const O: Dur = Dur::mins(10);
    /// K is read fresh for every close; the actuator re-reads it itself, so a
    /// Decision accepts a read this recent.
    pub const K: Dur = Dur::secs(60);
}

// ---------------------------------------------------------------------------
// Three-valued logic
// ---------------------------------------------------------------------------

/// What could not be read, and why (`Unknown(probe, reason)`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Unk {
    pub probe: String,
    pub reason: String,
}

impl Unk {
    pub fn new(probe: impl Into<String>, reason: impl Into<String>) -> Self {
        Unk {
            probe: probe.into(),
            reason: reason.into(),
        }
    }
}

/// Kleene's three values. `Unknown` is never `True`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tri {
    True,
    False,
    Unknown(Unk),
}

impl Tri {
    pub fn from_bool(b: bool) -> Tri {
        if b {
            Tri::True
        } else {
            Tri::False
        }
    }
    pub fn unknown(probe: impl Into<String>, reason: impl Into<String>) -> Tri {
        Tri::Unknown(Unk::new(probe, reason))
    }
    pub fn is_true(&self) -> bool {
        matches!(self, Tri::True)
    }
    pub fn is_false(&self) -> bool {
        matches!(self, Tri::False)
    }
    pub fn is_unknown(&self) -> bool {
        matches!(self, Tri::Unknown(_))
    }
    /// Kleene conjunction: `False` dominates, then `Unknown` (the first one met),
    /// else `True`.
    pub fn and(self, other: Tri) -> Tri {
        match (self, other) {
            (Tri::False, _) | (_, Tri::False) => Tri::False,
            (Tri::Unknown(u), _) | (_, Tri::Unknown(u)) => Tri::Unknown(u),
            (Tri::True, Tri::True) => Tri::True,
        }
    }
    /// Kleene disjunction: `True` dominates, then `Unknown`, else `False`.
    pub fn or(self, other: Tri) -> Tri {
        match (self, other) {
            (Tri::True, _) | (_, Tri::True) => Tri::True,
            (Tri::Unknown(u), _) | (_, Tri::Unknown(u)) => Tri::Unknown(u),
            (Tri::False, Tri::False) => Tri::False,
        }
    }
    pub fn not(self) -> Tri {
        match self {
            Tri::True => Tri::False,
            Tri::False => Tri::True,
            u @ Tri::Unknown(_) => u,
        }
    }
    /// Conjunction of any number of values; empty is `True`.
    pub fn all<I: IntoIterator<Item = Tri>>(it: I) -> Tri {
        it.into_iter().fold(Tri::True, Tri::and)
    }
}

// ---------------------------------------------------------------------------
// Identity and close-authority vocabulary (input facts the kernel owns)
// ---------------------------------------------------------------------------

/// A session's harness id (the same id across its instances).
pub type SessionId = String;
/// A coord account id.
pub type AccountId = String;

/// Who a runner close or pane write resolved to (the loopback peer's `claude`
/// session, R3-2). `Unresolvable` is UNKNOWN, never a match.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Principal {
    Session(SessionId),
    Unresolvable,
}

/// The close-site roster class a runner close passes at runtime (R0a/R0b-1),
/// stored in the pane's exit facts before the kill. Only the door a runner's
/// OWN code set is evidence; nothing a device credential can put on a frame is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CloseDoor {
    /// The lifecycle's own Closer (`PtyWriteCaller::LifecycleClose` bytes, or on
    /// the interim host the journal entry written before its first byte). D2.
    LifecycleClose,
    /// The operator twin `operator_terminal_close` (`CloseDoor::OperatorTab`):
    /// only the webview's explicit per-pane and per-page close gestures call it.
    OperatorTab,
    /// An end or close the relay stamped `origin: user_session` (P6f-1;
    /// `CloseDoor::RelayUserSession`). `remote_block` is whether the frame also
    /// carried a relay `remote` block: a device-relayed frame always does, so a
    /// stamp counts only with NO block (NC6-2).
    RelayUserSession { remote_block: bool },
    /// A device-socket end, or an unstamped relay end (an older source or relay).
    RelayDevice,
    /// `DELETE /terminals/{id}`, the proxy's `terminal_close`, the steward
    /// stop's kill: `closer` is the loopback peer's resolved `claude` session.
    Agent { closer: Principal },
    /// Any of the webview's programmatic closes that reaches a live `claude`
    /// (Keep AI's prune, the reorganize's and the project move's before R0c).
    Webview,
    /// `close_all` at runner exit.
    RunnerExit,
}

/// Who wrote bytes into a pane (R2-1's per-write writer, resolved by R3-2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Writer {
    /// A write that came through an agent door; the resolved session.
    AgentDoor(Principal),
    /// The webview's own keystrokes (`TauriTerminalWrite`).
    OperatorKeystroke,
    /// Input relayed from a remote tab (`RemoteTerminalInput`), stamped or not.
    RelayedInput,
    /// A lifecycle delivery (`PtyWriteCaller` of the Deliverer).
    LifecycleDelivery,
    /// The HTTP create's deferred initial command (`HttpCreateInitialCommand`):
    /// counts as the creator's own write (m6-3, NC7-3).
    HttpCreateInitialCommand,
    /// The funnel's `ControlResponse` chunks (the webview's xterm answering
    /// Claude Code's terminal queries): not input, counts for no one (NC7-3).
    ControlResponse,
}

/// One write into a pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneWrite {
    pub writer: Writer,
    pub at: Timestamp,
}

/// How a typed exit reached the PTY, when CC ended on a typed exit
/// (SessionEnd `prompt_input_exit` / `logout`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypedExit {
    /// The bytes came from the webview's own keystrokes (`TauriTerminalWrite`).
    OperatorKeystroke,
    /// Relayed input the relay stamped `origin: user_session`; `remote_block` as
    /// on [`CloseDoor::RelayUserSession`].
    RelayedUserSession { remote_block: bool },
    /// Relayed input with no stamp (the operator's `/exit` in a REMOTE tab reads
    /// here — the product cannot tell it from an agent's, §12.1) or a device
    /// socket's.
    RelayedUnstamped,
    /// The lifecycle's own typed `/exit`.
    Lifecycle,
    /// Any other writer (an agent door, an unresolved writer).
    Other,
}

/// Why the process ended, as the PTY wait status and the runner saw it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitInitiator {
    Runner,
    SelfExit,
    Crash,
    External,
    Operator,
}

/// What the runner stored when the pane's process ended (R0b-1: the `door`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExitFacts {
    pub code: Option<i32>,
    pub initiator: ExitInitiator,
    /// The `CloseDoor` a runner close stored before its kill; `None` for an exit
    /// no runner close caused, and for a runner build before R0b-1.
    pub door: Option<CloseDoor>,
    /// Set when the exit was a typed exit rather than a runner close.
    pub typed: Option<TypedExit>,
    pub at: Timestamp,
    /// The session that created the pane, recorded at its HTTP or proxy spawn.
    pub creator: Principal,
    /// Every write into the pane since the spawn (R2-1).
    pub writes: Vec<PaneWrite>,
    /// The exit was read back gone after a lifecycle Close (D2).
    pub read_back_gone: bool,
}

/// What a minted close authority attests, R0a/R6 (`CloseAuthority`). The kernel
/// names the variants; only [`crate::guard`] can build `Product` (it holds the
/// evidence that every conjunct read True).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CloseAuthority {
    /// Minted from a `decide()` Close whose guard the Closer re-read.
    Product { decision_digest: Digest },
    /// The operator's end of ONE session they selected, on an operator
    /// credential; attributed D3 and not subject to G1-G9 (R6).
    Operator,
}

/// A replayable digest of a Decision's inputs (FNV-1a, 64 bit; see
/// [`crate::decide::digest_of`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Digest(pub u64);

/// How a pane is launched; a steward or looping-agent pane's launcher owns its
/// relaunch, so D4 never resumes it (§3.2 D4, RV5-3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneKind {
    Terminal,
    Steward,
    Looping,
    /// `GET /restart-readiness` `windDown.kind` unreadable: treated as looping.
    Unreadable,
}

/// `attended` or `unattended` (§2.1 Mode). Unknown is treated as attended: it is
/// never nudged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Attended,
    Unattended,
    Unknown,
}

impl Mode {
    /// Only a positively derived `unattended` may be continued.
    pub fn may_continue(self) -> bool {
        self == Mode::Unattended
    }
}

// ---------------------------------------------------------------------------
// Axes
// ---------------------------------------------------------------------------

/// An axis value with its observation time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observed<T> {
    pub value: T,
    pub observed_at: Timestamp,
}

impl<T> Observed<T> {
    pub fn new(value: T, observed_at: Timestamp) -> Self {
        Observed { value, observed_at }
    }
    pub fn age(&self, now: Timestamp) -> Dur {
        now.since(self.observed_at)
    }
    pub fn fresh(&self, now: Timestamp, tau: Dur) -> bool {
        self.observed_at <= now && self.age(now) <= tau
    }
}

/// The Claude Code process instance an arming entry belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Instance {
    pub pid: u32,
    pub proc_start: u64,
}

/// Pane shape, for the interim allow-list (I1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneShape {
    Holder,
    InProcess,
    BashHosted,
}

/// **H** hosting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Hosting {
    Spawning,
    Live {
        instance: Instance,
        config_dir: String,
        cc_version: String,
        pane_shape: PaneShape,
        /// Registry status has appeared (§2.2 phase 1: Live with none is Spawning).
        registry_seen: bool,
    },
    Exited(ExitFacts),
    Unknown(Unk),
}

/// **A** activity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Activity {
    Turn,
    /// `since_turn_end` carries the idle clock across the compaction (§2.3).
    Compacting { since_turn_end: Option<Timestamp> },
    Delegated {
        agents: u32,
        workflows: u32,
    },
    /// Background shells, monitors and MCP tasks the main loop is not running.
    /// `stray_shells` of the `shells` are proven stray waits (§4.1); a shell that
    /// is not proven stray is work.
    ShellPinned {
        shells: u32,
        stray_shells: u32,
        monitors: u32,
        mcp_tasks: u32,
    },
    /// `since_turn_end` is the idle clock (last turn end); `grace_from` is the
    /// start of the continuous-idle window (restarted by a compaction).
    Idle {
        since_turn_end: Timestamp,
        grace_from: Timestamp,
    },
    Unknown(Unk),
}

impl Activity {
    /// §2.2 phase 3's exception: a `ShellPinned` whose every live shell is a
    /// proven stray wait and which has no monitor or MCP task (the Delegated
    /// arm is a different variant, so no agent or workflow is in flight).
    pub fn pinned_by_strays(&self) -> bool {
        matches!(self, Activity::ShellPinned { shells, stray_shells, monitors, mcp_tasks }
            if *shells > 0 && stray_shells == shells && *monitors == 0 && *mcp_tasks == 0)
    }
}

/// How a cron was created, for the resume rule (D4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CronKind {
    /// Recurring session cron; resurrected on resume while under 7 days old.
    Recurring { created: Timestamp },
    /// One-shot session cron; resurrected while its fire is in the future.
    OneShot { fire: Timestamp },
    /// `durable: true`, in `.claude/scheduled_tasks.json`: survives restarts.
    Durable,
}

/// One element of the S set. Every arming entry carries the instance that armed
/// it and holds only while that instance is the live one (§2.1 S).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelfCont {
    Wakeup {
        at: Timestamp,
        armed_by: Instance,
    },
    Cron {
        id: String,
        next: Timestamp,
        kind: CronKind,
        armed_by: Instance,
    },
    KeepaliveInferred {
        at: Timestamp,
        armed_by: Instance,
    },
    AutoContinue {
        resets_at: Timestamp,
        armed_by: Instance,
    },
    QueuedNotice {
        since: Timestamp,
    },
    InFlightDelivery {
        msg_id: String,
    },
    /// `blocking_unacked`: a delivered-but-unacked `blocking` message (G4).
    PendingMessage {
        msg_id: String,
        blocking_unacked: bool,
    },
    /// A wake lost when a new instance replaced the one that armed it.
    LostOnNewInstance {
        at: Timestamp,
    },
}

impl SelfCont {
    /// Whether this entry still holds under `live` (the live instance, if any).
    /// Arming entries hold only for the instance that armed them; the rest
    /// (a notice, a delivery, a message, a lost wake) are not instance-scoped.
    pub fn holds(&self, live: Option<Instance>) -> bool {
        match self {
            SelfCont::Wakeup { armed_by, .. }
            | SelfCont::Cron { armed_by, .. }
            | SelfCont::KeepaliveInferred { armed_by, .. }
            | SelfCont::AutoContinue { armed_by, .. } => live == Some(*armed_by),
            SelfCont::QueuedNotice { .. }
            | SelfCont::InFlightDelivery { .. }
            | SelfCont::PendingMessage { .. }
            | SelfCont::LostOnNewInstance { .. } => true,
        }
    }
}

/// **S**.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelfConts {
    Known(Vec<SelfCont>),
    Unknown(Unk),
}

/// Auto-continue state of a parked limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoContinue {
    Armed { armed_by: Instance },
    Withdrawn,
}

/// The auth errors of D11.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthError {
    AuthenticationFailed,
    OauthOrgNotAllowed,
    AccountOnHold,
    BillingError,
    CloudCredentialError,
}

/// The retryable and request errors of D12.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiErrorKind {
    Overloaded,
    ServerError,
    InvalidRequest,
    MaxOutputTokens,
    Unknown,
}

/// **P** parked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Parked {
    None,
    Limit {
        limit_type: String,
        resets_at: Timestamp,
        account: AccountId,
        auto_continue: AutoContinue,
    },
    Auth {
        error: AuthError,
        account: AccountId,
    },
    ApiError {
        kind: ApiErrorKind,
        retryable: bool,
    },
    Unknown(Unk),
}

/// What an open dialog is (D7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DialogKind {
    LimitMenu,
    /// A prompt for a credential only the operator holds.
    CredentialPrompt {
        account: AccountId,
        ui_location: String,
    },
    Permission,
    Elicitation,
    Sandbox,
    Security,
    AskUserQuestion,
    Other,
}

/// **I** input surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Input {
    Clear,
    Dialog {
        kind: DialogKind,
        since: Timestamp,
    },
    Draft {
        bytes: u32,
        writer: Writer,
        since: Timestamp,
    },
    Unknown(Unk),
}

/// What kind of finish was declared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinishKind {
    Done,
    Blocked,
    WaitingHuman,
}

/// **W** declared work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Declared {
    Undeclared,
    Working,
    Finished {
        at: Timestamp,
        kind: FinishKind,
        items: Vec<String>,
    },
    Stalled,
    /// `Unknown(declaration_unverified)` or an unreadable axis.
    Unknown(Unk),
}

/// **O**: the owned-work-item axis. The items themselves arrive as
/// [`crate::decide::WorkItemView`]s; this records whether they were read fresh.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OwnedAxis {
    Known,
    /// `Unknown(unattributed_push)` and friends.
    Unknown(Unk),
}

/// One touched tree's custody (K).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Custody {
    Clean { evidence: String },
    Dirty { reason: String },
    Absent { evidence: String },
    Unknown { reason: String },
}

/// K: custody per touched tree, read fresh at the moment of any close.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CustodyAxis {
    Known(Vec<(String, Custody)>),
    Unknown(Unk),
}

/// The safety latch `lifecycle_close_latch` (G9).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Latch {
    Clear,
    Tripped,
    /// Unreadable; `since` lets a poll interval pass before it reads tripped.
    Unreadable {
        since: Timestamp,
    },
}

/// A hold after a relayed exit (D4, m7-2): the operator's possible stop outranks
/// every product-started turn until a turn the product did not deliver opens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hold {
    pub since: Timestamp,
    /// The session was resumed (and is live) rather than left down.
    pub resumed: bool,
    /// When the operator was last told (once at the start, then at most once
    /// per 24 h and only when something new is owed).
    pub last_notice: Option<Timestamp>,
}

/// One Continue the lifecycle delivered, for the budgets (§3.2) and D21.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContinueRecord {
    pub at: Timestamp,
    /// Digest of the owed-item set the Continue carried.
    pub items: u64,
    /// Which door started it (`/next-move` nudges count against the same budget).
    pub source: ContinueSource,
    /// Whether a new fact arrived after it (a tool call alone is not one).
    pub new_fact_since: bool,
}

/// Who sent a Continue-shaped turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContinueSource {
    Lifecycle,
    /// `/next-move`'s nudge or redirect, read from `coord_overlord_interventions`.
    NextMove,
}

/// A Stop-hook block (budget: ≤ 2 consecutive, ≤ 3 per hour per owed-item set,
/// counted independently of tool calls).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockRecord {
    pub at: Timestamp,
    pub items: u64,
}

/// A `/next-move` intervention read from `coord_overlord_interventions`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NextMoveKind {
    Nudge,
    Redirect,
    /// The E arm's finish: NOT a declaration (T30).
    FinishE,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NextMoveIntervention {
    pub at: Timestamp,
    pub kind: NextMoveKind,
}

/// Budget and delivery history of one session.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Ledger {
    pub continues: Vec<ContinueRecord>,
    pub blocks: Vec<BlockRecord>,
    pub next_move: Vec<NextMoveIntervention>,
    /// `[lifecycle-dispute] <item_id>: <reason>` footers found in the session's
    /// last turn.
    pub disputes: Vec<String>,
    /// D4 resume attempts in the last 24 h.
    pub resume_attempts: Vec<Timestamp>,
    /// Migrations in the last 24 h (`MIGRATION_CAP`).
    pub migrations: Vec<Timestamp>,
    /// Detections already reported, with when (dedupe by key).
    pub reported: BTreeMap<String, Timestamp>,
    /// When the current UNKNOWN reading began, by probe.
    pub unknown_since: BTreeMap<String, Timestamp>,
    /// Operator notices already sent for this session, by key.
    pub notices: BTreeMap<String, Timestamp>,
}

/// The whole vector, plus the facts decide needs that are not axes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionState {
    pub session_id: SessionId,
    pub hosting: Observed<Hosting>,
    pub activity: Observed<Activity>,
    pub selfcont: Observed<SelfConts>,
    pub parked: Observed<Parked>,
    pub input: Observed<Input>,
    pub work: Observed<Declared>,
    pub owned: Observed<OwnedAxis>,
    pub custody: Observed<CustodyAxis>,
    pub mode: Mode,
    pub pane_kind: PaneKind,
    /// **R**, derived by [`crate::evaluate::revived`] from the main transcript
    /// before `decide` runs (a field because `decide` is pure over its input).
    pub revived: Tri,
    /// G1: the coord row reads `session_status = finished`, read fresh.
    pub coord_finished: Tri,
    /// The close-time schedules a resume would bring back, read from the
    /// transcript at the exit (D4's hold test); `Unknown` keeps it down.
    pub exit_schedules: Option<SelfConts>,
    /// The session is on the operator's R8-3 per-row hold, or is a §9.4 restore
    /// placeholder awaiting the operator's Restore.
    pub operator_hold: bool,
    /// A gate-continuation session: the gate that spawned it (the #2373 rule).
    pub continuation_gate: Option<String>,
    /// The pane's PTY child is a shell with `claude --resume` typed into it, not
    /// the agent itself (a restored continuation): the exit code describes the
    /// shell (#2373).
    pub shell_hosted: bool,
    /// Any hold after a relayed exit currently standing.
    pub hold: Option<Hold>,
    pub ledger: Ledger,
    /// An event of a kind this build does not know was folded (§3.7): it can
    /// only withhold.
    pub schema_skew: Option<Unk>,
}

impl SessionState {
    /// A state with every axis `Unknown` at `at`: the right start for `fold`.
    pub fn unread(session_id: impl Into<String>, at: Timestamp) -> SessionState {
        let u = |probe: &str| Unk::new(probe, "not yet read");
        SessionState {
            session_id: session_id.into(),
            hosting: Observed::new(Hosting::Unknown(u("hosting")), at),
            activity: Observed::new(Activity::Unknown(u("activity")), at),
            selfcont: Observed::new(SelfConts::Unknown(u("selfcont")), at),
            parked: Observed::new(Parked::Unknown(u("parked")), at),
            input: Observed::new(Input::Unknown(u("input")), at),
            work: Observed::new(Declared::Unknown(u("work")), at),
            owned: Observed::new(OwnedAxis::Unknown(u("owned")), at),
            custody: Observed::new(CustodyAxis::Unknown(u("custody")), at),
            mode: Mode::Unknown,
            pane_kind: PaneKind::Unreadable,
            revived: Tri::unknown("revived", "not yet read"),
            coord_finished: Tri::unknown("coord_row", "not yet read"),
            exit_schedules: None,
            operator_hold: false,
            continuation_gate: None,
            shell_hosted: false,
            hold: None,
            ledger: Ledger::default(),
            schema_skew: None,
        }
    }

    /// The live instance, when H is a fresh `Live`.
    pub fn live_instance(&self) -> Option<Instance> {
        match &self.hosting.value {
            Hosting::Live { instance, .. } => Some(*instance),
            _ => None,
        }
    }

    /// The S entries that currently hold (empty when S is unknown).
    pub fn holding_selfcont(&self) -> Vec<&SelfCont> {
        let live = self.live_instance();
        match &self.selfcont.value {
            SelfConts::Known(v) => v.iter().filter(|e| e.holds(live)).collect(),
            SelfConts::Unknown(_) => Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Events and fold
// ---------------------------------------------------------------------------

/// What an event says. Each variant updates exactly the axes named in its doc.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventKind {
    /// H: a new live instance (spawn seam / resume / restore / migration).
    HostLive {
        instance: Instance,
        config_dir: String,
        cc_version: String,
        pane_shape: PaneShape,
    },
    /// H: the registry first reported a status for this instance.
    RegistrySeen,
    /// H: the PTY wait status and exit facts.
    HostExit(ExitFacts),
    /// A: UserPromptSubmit.
    TurnStart,
    /// A: Stop. Clears a limit's `Parked` only via its own events.
    TurnEnd,
    /// A: PreCompact / PostCompact.
    CompactStart,
    CompactEnd,
    /// A: registry or Stop `background_tasks` counts.
    Background {
        agents: u32,
        workflows: u32,
        shells: u32,
        stray_shells: u32,
        monitors: u32,
        mcp_tasks: u32,
    },
    /// P: StopFailure with its classified error.
    StopFailure(Parked),
    /// P: Notification `quota_auto_resume_*`.
    QuotaNotice(QuotaNotice),
    /// P: migration completed, or a real prompt: parking over.
    ParkedCleared,
    /// I: the funnel, hook or witness read.
    InputRead(Input),
    /// S: replace the armed set from a transcript or Stop read.
    SelfContRead(Vec<SelfCont>),
    /// S: unreadable.
    SelfContUnknown(Unk),
    /// W: the verified declaration, or its absence.
    WorkRead(Declared),
    /// O: the owned-item axis.
    OwnedRead(OwnedAxis),
    /// K: custody per tree.
    CustodyRead(CustodyAxis),
    /// G1.
    CoordRow {
        finished: Tri,
    },
    /// R.
    RevivedRead(Tri),
    /// Mode and pane kind.
    ModeRead {
        mode: Mode,
        pane_kind: PaneKind,
    },
    /// An event kind this build does not know (§3.7).
    UnknownKind(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuotaNotice {
    Fired,
    Stale,
    Disabled,
}

/// One pushed event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub at: Timestamp,
    /// Tie-break for equal timestamps (the producer's sequence).
    pub seq: u64,
    pub kind: EventKind,
}

/// Fold `events` into `state`, in `(at, seq)` order. Pure and deterministic.
/// An unknown event kind sets [`SessionState::schema_skew`], which can only
/// withhold (§3.7).
pub fn fold(mut state: SessionState, events: &[Event]) -> SessionState {
    let mut sorted: Vec<&Event> = events.iter().collect();
    sorted.sort_by_key(|e| (e.at, e.seq));
    for e in sorted {
        apply(&mut state, e);
    }
    state
}

fn apply(s: &mut SessionState, e: &Event) {
    let at = e.at;
    match &e.kind {
        EventKind::HostLive {
            instance,
            config_dir,
            cc_version,
            pane_shape,
        } => {
            let previous = s.live_instance();
            if previous != Some(*instance) {
                lose_instance_state(s, previous, at);
            }
            s.hosting = Observed::new(
                Hosting::Live {
                    instance: *instance,
                    config_dir: config_dir.clone(),
                    cc_version: cc_version.clone(),
                    pane_shape: *pane_shape,
                    registry_seen: false,
                },
                at,
            );
        }
        EventKind::RegistrySeen => {
            if let Hosting::Live {
                instance,
                config_dir,
                cc_version,
                pane_shape,
                ..
            } = &s.hosting.value
            {
                s.hosting = Observed::new(
                    Hosting::Live {
                        instance: *instance,
                        config_dir: config_dir.clone(),
                        cc_version: cc_version.clone(),
                        pane_shape: *pane_shape,
                        registry_seen: true,
                    },
                    at,
                );
            }
        }
        EventKind::HostExit(facts) => {
            s.hosting = Observed::new(Hosting::Exited(facts.clone()), at);
        }
        EventKind::TurnStart => {
            s.activity = Observed::new(Activity::Turn, at);
            // A real prompt ends a hold and any parking.
            if matches!(
                s.parked.value,
                Parked::Limit { .. } | Parked::ApiError { .. }
            ) {
                s.parked = Observed::new(Parked::None, at);
            }
        }
        EventKind::TurnEnd => {
            s.activity = Observed::new(
                Activity::Idle {
                    since_turn_end: at,
                    grace_from: at,
                },
                at,
            );
        }
        EventKind::CompactStart => {
            let since = match &s.activity.value {
                Activity::Idle { since_turn_end, .. } => Some(*since_turn_end),
                Activity::Compacting { since_turn_end } => *since_turn_end,
                _ => None,
            };
            s.activity = Observed::new(Activity::Compacting { since_turn_end: since }, at);
        }
        EventKind::CompactEnd => {
            // The idle clock is unchanged, the grace window restarts (§2.3).
            let since = match &s.activity.value {
                Activity::Idle { since_turn_end, .. } => *since_turn_end,
                Activity::Compacting { since_turn_end } => since_turn_end.unwrap_or(at),
                _ => at,
            };
            s.activity = Observed::new(
                Activity::Idle {
                    since_turn_end: since,
                    grace_from: at,
                },
                at,
            );
        }
        EventKind::Background {
            agents,
            workflows,
            shells,
            stray_shells,
            monitors,
            mcp_tasks,
        } => {
            if matches!(s.activity.value, Activity::Turn | Activity::Compacting { .. }) {
                // A turn outranks background work; only the age moves.
                s.activity.observed_at = at;
            } else if *agents > 0 || *workflows > 0 {
                s.activity = Observed::new(
                    Activity::Delegated {
                        agents: *agents,
                        workflows: *workflows,
                    },
                    at,
                );
            } else if *shells > 0 || *monitors > 0 || *mcp_tasks > 0 {
                s.activity = Observed::new(
                    Activity::ShellPinned {
                        shells: *shells,
                        stray_shells: (*stray_shells).min(*shells),
                        monitors: *monitors,
                        mcp_tasks: *mcp_tasks,
                    },
                    at,
                );
            } else {
                let (since, grace) = match &s.activity.value {
                    Activity::Idle {
                        since_turn_end,
                        grace_from,
                    } => (*since_turn_end, *grace_from),
                    _ => (at, at),
                };
                s.activity = Observed::new(
                    Activity::Idle {
                        since_turn_end: since,
                        grace_from: grace,
                    },
                    at,
                );
            }
        }
        EventKind::StopFailure(p) => {
            s.parked = Observed::new(p.clone(), at);
            s.activity = Observed::new(
                Activity::Idle {
                    since_turn_end: at,
                    grace_from: at,
                },
                at,
            );
        }
        EventKind::QuotaNotice(n) => {
            if let Parked::Limit {
                limit_type,
                resets_at,
                account,
                auto_continue,
            } = &s.parked.value
            {
                let ac = match n {
                    QuotaNotice::Fired => *auto_continue,
                    QuotaNotice::Stale | QuotaNotice::Disabled => AutoContinue::Withdrawn,
                };
                s.parked = Observed::new(
                    Parked::Limit {
                        limit_type: limit_type.clone(),
                        resets_at: *resets_at,
                        account: account.clone(),
                        auto_continue: ac,
                    },
                    at,
                );
            }
        }
        EventKind::ParkedCleared => s.parked = Observed::new(Parked::None, at),
        EventKind::InputRead(i) => s.input = Observed::new(i.clone(), at),
        EventKind::SelfContRead(v) => s.selfcont = Observed::new(SelfConts::Known(v.clone()), at),
        EventKind::SelfContUnknown(u) => {
            s.selfcont = Observed::new(SelfConts::Unknown(u.clone()), at)
        }
        EventKind::WorkRead(w) => s.work = Observed::new(w.clone(), at),
        EventKind::OwnedRead(o) => s.owned = Observed::new(o.clone(), at),
        EventKind::CustodyRead(c) => s.custody = Observed::new(c.clone(), at),
        EventKind::CoordRow { finished } => s.coord_finished = finished.clone(),
        EventKind::RevivedRead(r) => s.revived = r.clone(),
        EventKind::ModeRead { mode, pane_kind } => {
            s.mode = *mode;
            s.pane_kind = *pane_kind;
        }
        EventKind::UnknownKind(k) => {
            s.schema_skew = Some(Unk::new("event_schema", format!("unknown event kind {k}")));
        }
    }
}

/// A new instance replaces `previous`: its arming entries become
/// `LostOnNewInstance` / `withdrawn` until the new instance's transcript shows a
/// re-arm or a fire (§2.3 "any → new instance"; T33, T34).
fn lose_instance_state(s: &mut SessionState, previous: Option<Instance>, at: Timestamp) {
    let Some(old) = previous else { return };
    if let SelfConts::Known(entries) = &s.selfcont.value {
        let mut kept = Vec::new();
        for e in entries {
            match e {
                SelfCont::Wakeup { at: wake, armed_by } if *armed_by == old => {
                    kept.push(SelfCont::LostOnNewInstance { at: *wake });
                }
                SelfCont::KeepaliveInferred { at: k, armed_by } if *armed_by == old => {
                    kept.push(SelfCont::LostOnNewInstance { at: *k });
                }
                SelfCont::AutoContinue { armed_by, .. } if *armed_by == old => {}
                // A cron is resurrected by Claude Code on resume per the
                // transcript rule; the evaluator re-emits it for the new instance.
                SelfCont::Cron { armed_by, .. } if *armed_by == old => {}
                other => kept.push(other.clone()),
            }
        }
        s.selfcont = Observed::new(SelfConts::Known(kept), at);
    }
    if let Parked::Limit {
        limit_type,
        resets_at,
        account,
        auto_continue: AutoContinue::Armed { armed_by },
    } = &s.parked.value
    {
        if *armed_by == old {
            s.parked = Observed::new(
                Parked::Limit {
                    limit_type: limit_type.clone(),
                    resets_at: *resets_at,
                    account: account.clone(),
                    auto_continue: AutoContinue::Withdrawn,
                },
                at,
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Phases (§2.2)
// ---------------------------------------------------------------------------

/// `Stopped`'s sub-phases, in order (§2.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stopped {
    Revived,
    Owing,
    Undeclared,
    AwaitingFirstVerdict,
    FinishedUnverified,
    FinishedPinned,
    Finished,
}

/// §2.2's phase, first match wins.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Phase {
    Unknown(Unk),
    Spawning,
    Working,
    Delegated,
    Interacting,
    Parked,
    Scheduled,
    Stopped(Stopped),
    Closing,
    Closed { finished: bool },
}

/// The items-derived facts the phase needs from §2.8, so the table stays pure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ItemFacts {
    /// Some item is positively owed by this session (OwedBySession).
    pub owed: bool,
    /// Every pushed head is waiting inside the first-verdict hold (§5.5).
    pub awaiting_first_verdict: bool,
    /// Some item (or tree) is Unknown or Dirty.
    pub unverified: bool,
}

/// Compute §2.2's phase.
///
/// Rule 0: an axis the next phases need that is `Unknown` or stale (§2.5) makes
/// the phase `Unknown`. For an `Exited` host only H (and W, which an exit rule
/// reads) is needed: a dead process has no activity, input or schedule to read,
/// and demanding them would make every exit `Unknown` forever.
pub fn phase_of(s: &SessionState, items: ItemFacts, closing: bool, now: Timestamp) -> Phase {
    if let Some(u) = &s.schema_skew {
        return Phase::Unknown(u.clone());
    }
    match &s.hosting.value {
        Hosting::Unknown(u) => return Phase::Unknown(u.clone()),
        _ if !s.hosting.fresh(now, tau::H) && !matches!(s.hosting.value, Hosting::Exited(_)) => {
            return Phase::Unknown(Unk::new("hosting", "stale"));
        }
        _ => {}
    }
    if let Hosting::Exited(_) = &s.hosting.value {
        return Phase::Closed {
            finished: matches!(s.work.value, Declared::Finished { .. }),
        };
    }
    // Needed from here on: A, I, P, S, fresh.
    if let Activity::Unknown(u) = &s.activity.value {
        return Phase::Unknown(u.clone());
    }
    if !s.activity.fresh(now, tau::A) {
        return Phase::Unknown(Unk::new("activity", "stale"));
    }
    if let Input::Unknown(u) = &s.input.value {
        return Phase::Unknown(u.clone());
    }
    if !s.input.fresh(now, tau::I) {
        return Phase::Unknown(Unk::new("input", "stale"));
    }
    if let Parked::Unknown(u) = &s.parked.value {
        return Phase::Unknown(u.clone());
    }
    if !s.parked.fresh(now, tau::P) {
        return Phase::Unknown(Unk::new("parked", "stale"));
    }
    if let SelfConts::Unknown(u) = &s.selfcont.value {
        return Phase::Unknown(u.clone());
    }
    if !s.selfcont.fresh(now, tau::S) {
        return Phase::Unknown(Unk::new("selfcont", "stale"));
    }
    // 1 Spawning
    match &s.hosting.value {
        Hosting::Spawning => return Phase::Spawning,
        Hosting::Live {
            registry_seen: false,
            ..
        } => return Phase::Spawning,
        Hosting::Live { .. } | Hosting::Exited(_) | Hosting::Unknown(_) => {}
    }
    // 2 Working, 3 Delegated
    match &s.activity.value {
        Activity::Turn | Activity::Compacting { .. } => return Phase::Working,
        Activity::Delegated { .. } => return Phase::Delegated,
        Activity::ShellPinned { .. } if !s.activity.value.pinned_by_strays() => {
            return Phase::Delegated
        }
        Activity::ShellPinned { .. } | Activity::Idle { .. } | Activity::Unknown(_) => {}
    }
    // 4 Interacting
    match &s.input.value {
        Input::Dialog { .. } | Input::Draft { .. } => return Phase::Interacting,
        Input::Clear | Input::Unknown(_) => {}
    }
    // 5 Parked
    match &s.parked.value {
        Parked::None => {}
        Parked::Limit { .. } | Parked::Auth { .. } | Parked::ApiError { .. } => {
            return Phase::Parked
        }
        Parked::Unknown(_) => {}
    }
    // 6 Scheduled
    if !s.holding_selfcont().is_empty() {
        return Phase::Scheduled;
    }
    // 8 Closing precedes the Stopped decision only when a close is in flight.
    if closing {
        return Phase::Closing;
    }
    // 7 Stopped and its sub-phases.
    if s.revived.is_true() {
        return Phase::Stopped(Stopped::Revived);
    }
    if s.revived.is_unknown() && matches!(s.work.value, Declared::Finished { .. }) {
        return Phase::Unknown(match &s.revived {
            Tri::Unknown(u) => u.clone(),
            _ => Unk::new("revived", "unread"),
        });
    }
    if items.owed {
        return Phase::Stopped(Stopped::Owing);
    }
    match &s.work.value {
        Declared::Undeclared | Declared::Working | Declared::Stalled => {
            Phase::Stopped(Stopped::Undeclared)
        }
        Declared::Unknown(u) => Phase::Unknown(u.clone()),
        Declared::Finished { .. } => {
            if items.awaiting_first_verdict {
                Phase::Stopped(Stopped::AwaitingFirstVerdict)
            } else if items.unverified || custody_unverified(&s.custody.value) {
                Phase::Stopped(Stopped::FinishedUnverified)
            } else if s.activity.value.pinned_by_strays() {
                Phase::Stopped(Stopped::FinishedPinned)
            } else {
                Phase::Stopped(Stopped::Finished)
            }
        }
    }
}

fn custody_unverified(k: &CustodyAxis) -> bool {
    match k {
        CustodyAxis::Unknown(_) => true,
        CustodyAxis::Known(trees) => trees
            .iter()
            .any(|(_, c)| !matches!(c, Custody::Clean { .. })),
    }
}

#[cfg(test)]
mod tests;
