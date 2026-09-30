//! Operator-input emission — the runner half of the *supply* side of the
//! operator-touch instrument (plan
//! `2026-09-20-agents-sustained-per-operator-hour-needs-an-operator-touch-record`,
//! Phase 2).
//!
//! [`crate::session::operator_touch`] records when an agent NEEDED a human
//! (the demand half). This module records when input actually arrived at a
//! session through a door a human uses — the moment an operator typed into,
//! answered, or redirected a session. That is what an "operator-hour" is made
//! of, and nothing else in the runner records it.
//!
//! ## One funnel, no per-producer call sites
//!
//! [`on_input`] is called from exactly two places:
//! [`TerminalSession::write`] and [`TerminalSession::submit_prompt`], beside
//! their existing `record_input`, and — for `write` — only AFTER the
//! `is_terminal_control_response` exclusion, so a focus report or a
//! device-attribute reply from the emulator never reads as a keystroke. Every
//! present and future PTY producer already passes through those two, tagged
//! with its [`PtyWriteCaller`], so no producer can bypass this.
//!
//! ## `actor_class` comes from the DOOR, never from the content
//!
//! [`classify`] is an exhaustive `match` on [`PtyWriteCaller`] with NO
//! wildcard arm: adding a producer variant fails to compile here until
//! someone decides what it is. Two doors only a human uses are `human`; four
//! doors a human UI and an agent can both reach are `unknown`; every
//! automated producer yields no event at all. No heuristic — typing cadence,
//! time of day, text shape — ever promotes `unknown` to `human`; shrinking
//! the `unknown` share is done by splitting a door.
//!
//! ## Never the bytes
//!
//! Nothing in this module receives the written data. [`on_input`] takes the
//! session (for its latch, coord id and screen), the caller tag and a
//! timestamp — pinned by `no_function_here_takes_the_written_bytes`. No byte
//! count leaves the process either: a byte count per minute is a typing-rate
//! biometric (plan D2).
//!
//! ## One event per (terminal, channel, 60-second bucket)
//!
//! Each [`TerminalSession`] owns an [`InputEpisodes`] latch — the
//! `context_watcher::mark_fired` claim-first pattern, keyed per channel and
//! re-armed by each new bucket — so a human typing a 400-character redirect
//! produces ONE row however many keystrokes it took. The bucket is
//! [`crate::session::operator_touch::epoch_bucket`] — the one bucket width
//! the demand store already uses; there is no second constant. coord
//! additionally stores `ON CONFLICT (idempotency_key) DO NOTHING`, so a
//! latch race costs at most a round trip, never a duplicate row.
//!
//! ## `session_state_at_input` — observed, not judged
//!
//! Only on the write that OPENS a latch: ONE un-debounced grid read through
//! [`TerminalSession::try_grid_text`] and
//! [`qontinui_runner_lib::looping_agent::idle::snapshot_looks_idle`].
//! `at_prompt` = the session looked idle at the first human input of the
//! bucket; `working` = it did not (the operator interrupted it); `unknown` =
//! the grid could not be read without blocking. The debounced
//! `looks_idle_quiescent` is async and sleeps, so it is never called here —
//! nothing may block a keystroke.
//!
//! ## Off the keystroke path
//!
//! The durable outbox's `record` fsyncs. The keystroke thread therefore only
//! `try_send`s onto a bounded queue ([`QUEUE_CAPACITY`]); a dedicated thread
//! appends to the outbox. A full queue DROPS and COUNTS — it never waits.
//! The outbox drains to `POST /coord/sessions/operator-input` under the
//! best-effort posture ([`crate::session::SessionEventKind::OperatorInput`]).
//!
//! ## No kill switch, no enable flag
//!
//! It records no content and gates nothing; a dial whose unset value
//! silently disables the fleet's central autonomy metric is the defect class
//! the plan refuses. `/health` `operatorInput` is the per-box observable.

use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender, TrySendError};
use std::sync::{Arc, OnceLock};

use serde_json::{json, Value};
use tracing::warn;
use uuid::Uuid;

use super::session::{PtyWriteCaller, TerminalSession};
use crate::session::operator_touch::epoch_bucket;

// ===========================================================================
// Vocabulary — the closed words coord's write boundary accepts
// ===========================================================================

/// Who the door says typed. `automated` is deliberately NOT a variant: an
/// automated producer yields no event, and coord refuses the word with a 422.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActorClass {
    /// A door only a human uses.
    Human,
    /// A door a human UI and an agent can both reach. Recorded and reported
    /// as its own share; counts toward neither "touched" nor "touch-free".
    Unknown,
}

impl ActorClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Human => "human",
            Self::Unknown => "unknown",
        }
    }
}

/// Which runner door the input came through. A subset of coord's
/// `ACCEPTED_CHANNELS`; the other three (`coord_answer`,
/// `coord_gate_clearance`, `admin_edit`) are emitted by coord itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    /// The pane's own keystrokes (the Tauri `terminal_write` command).
    LocalTerminal,
    /// A coord-relayed remote terminal's `terminal_input` frame.
    RemoteTerminal,
    /// The runner's HTTP / WebSocket / invoke-proxy surfaces — reachable by
    /// a human UI and an agent alike.
    RunnerHttp,
}

impl Channel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LocalTerminal => "local_terminal",
            Self::RemoteTerminal => "remote_terminal",
            Self::RunnerHttp => "runner_http",
        }
    }

    /// Slot in [`InputEpisodes`]' per-channel latch.
    fn index(self) -> usize {
        match self {
            Self::LocalTerminal => 0,
            Self::RemoteTerminal => 1,
            Self::RunnerHttp => 2,
        }
    }
}

const CHANNEL_COUNT: usize = 3;

/// What the screen looked like at the first human input of the bucket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionStateAtInput {
    /// The session looked idle at its prompt — the input answered a wait.
    AtPrompt,
    /// The session did not look idle — the input interrupted it.
    Working,
    /// The grid could not be read without blocking.
    Unknown,
}

impl SessionStateAtInput {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AtPrompt => "at_prompt",
            Self::Working => "working",
            Self::Unknown => "unknown",
        }
    }

    /// Fold one un-debounced grid snapshot (`None` = not readable without
    /// blocking) into a state. Pure.
    pub fn from_snapshot(snapshot: Option<(Vec<String>, u16)>) -> Self {
        use qontinui_runner_lib::looping_agent::idle::snapshot_looks_idle;
        match snapshot {
            None => Self::Unknown,
            Some((lines, cursor_row)) if snapshot_looks_idle(&lines, cursor_row) => Self::AtPrompt,
            Some(_) => Self::Working,
        }
    }
}

/// A classified door: the actor class and the channel it reports under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Door {
    pub actor_class: ActorClass,
    pub channel: Channel,
}

/// THE mapping from producer to door. Exhaustive, NO wildcard arm — a new
/// [`PtyWriteCaller`] variant fails to compile here until it is classified
/// (plan D3). `None` = an automated producer: seen, counted in `/health`
/// `by_caller_class.automated`, never emitted.
pub fn classify(caller: &PtyWriteCaller) -> Option<Door> {
    let door = |actor_class, channel| {
        Some(Door {
            actor_class,
            channel,
        })
    };
    match caller {
        // ---- human: doors only a person uses --------------------------
        PtyWriteCaller::TauriTerminalWrite => door(ActorClass::Human, Channel::LocalTerminal),
        PtyWriteCaller::RemoteTerminalInput => door(ActorClass::Human, Channel::RemoteTerminal),
        // ---- unknown: a human UI and an agent can both reach these ----
        PtyWriteCaller::TauriInvokeProxy
        | PtyWriteCaller::HttpWrite
        | PtyWriteCaller::WebSocketInput
        | PtyWriteCaller::HttpSubmitPrompt => door(ActorClass::Unknown, Channel::RunnerHttp),
        // ---- automated: no row --------------------------------------
        PtyWriteCaller::LoopingAgentNudge
        | PtyWriteCaller::AccountMigration
        | PtyWriteCaller::SessionMessagePoller
        | PtyWriteCaller::AutoResponse { .. }
        | PtyWriteCaller::WorkerSession
        | PtyWriteCaller::PtyTransport
        | PtyWriteCaller::ClaudeCliTransport
        | PtyWriteCaller::LaunchInitialCommand
        | PtyWriteCaller::HttpCreateInitialCommand
        | PtyWriteCaller::StewardLaunchCommand
        | PtyWriteCaller::GracefulExit => None,
        // Unit-test fixtures are not a door anyone steers through.
        #[cfg(test)]
        PtyWriteCaller::Test => None,
    }
}

/// The actor class a producer's door carries, or `None` for an automated
/// producer. A projection of [`classify`], so the two cannot disagree.
pub fn actor_class_of(caller: &PtyWriteCaller) -> Option<ActorClass> {
    classify(caller).map(|d| d.actor_class)
}

// ===========================================================================
// The per-terminal episode latch
// ===========================================================================

/// Sentinel for "this channel has never opened a bucket".
const NEVER: i64 = i64::MIN;

/// One terminal's episode latch: per channel, the bucket it last emitted for.
/// Owned by the [`TerminalSession`], so it lives and dies with the terminal
/// and needs no prune. Lock-free — one atomic swap per input write.
#[derive(Debug)]
pub struct InputEpisodes {
    last_bucket: [AtomicI64; CHANNEL_COUNT],
    /// Latches THIS terminal has opened — the per-terminal twin of the
    /// process-wide `latched` counter, readable without cross-test noise.
    opened: AtomicU64,
}

impl Default for InputEpisodes {
    fn default() -> Self {
        Self {
            last_bucket: std::array::from_fn(|_| AtomicI64::new(NEVER)),
            opened: AtomicU64::new(0),
        }
    }
}

/// A latch this write opened: the first input on `door.channel` in `bucket`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenedEpisode {
    pub door: Door,
    pub bucket: i64,
}

impl InputEpisodes {
    /// Observe one input write. Returns `Some` iff this write OPENED a new
    /// `(channel, bucket)` latch — claim-first, so exactly one of any number
    /// of concurrent writes in a bucket wins. Also tallies the write under
    /// its actor class for `/health`.
    pub fn observe(&self, caller: &PtyWriteCaller, now_unix_secs: i64) -> Option<OpenedEpisode> {
        let Some(door) = classify(caller) else {
            COUNTERS.seen_automated.fetch_add(1, Ordering::Relaxed);
            return None;
        };
        match door.actor_class {
            ActorClass::Human => COUNTERS.seen_human.fetch_add(1, Ordering::Relaxed),
            ActorClass::Unknown => COUNTERS.seen_unknown.fetch_add(1, Ordering::Relaxed),
        };
        let bucket = epoch_bucket(now_unix_secs);
        let previous = self.last_bucket[door.channel.index()].swap(bucket, Ordering::AcqRel);
        if previous == bucket {
            return None;
        }
        self.opened.fetch_add(1, Ordering::Relaxed);
        COUNTERS.latched.fetch_add(1, Ordering::Relaxed);
        Some(OpenedEpisode { door, bucket })
    }

    /// How many latches this terminal has opened.
    pub fn opened(&self) -> u64 {
        self.opened.load(Ordering::Relaxed)
    }
}

// ===========================================================================
// Payload
// ===========================================================================

/// The caller-formed idempotency key: `<session_id>:input:<channel>:<bucket>`.
/// coord tenant-prefixes it before storing (`stored_idempotency_key`).
pub fn idempotency_key(coord_session_id: Uuid, channel: Channel, bucket: i64) -> String {
    format!("{coord_session_id}:input:{}:{bucket}", channel.as_str())
}

/// Bucket start as RFC 3339 UTC, whole seconds.
fn bucket_start_rfc3339(bucket: i64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp(bucket, 0)
        .unwrap_or_default()
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// The `POST /coord/sessions/operator-input` body, forwarded verbatim by the
/// outbox drain. No tenant (coord takes it from the device JWT), no content,
/// no byte count.
pub fn input_payload(
    coord_session_id: Uuid,
    episode: OpenedEpisode,
    state: SessionStateAtInput,
) -> Value {
    json!({
        "session_id": coord_session_id.to_string(),
        "channel": episode.door.channel.as_str(),
        "actor_class": episode.door.actor_class.as_str(),
        "session_state_at_input": state.as_str(),
        "occurred_at": bucket_start_rfc3339(episode.bucket),
        "idempotency_key": idempotency_key(coord_session_id, episode.door.channel, episode.bucket),
    })
}

// ===========================================================================
// The funnel hook
// ===========================================================================

/// Called by [`TerminalSession::write`] (for non-control-response chunks) and
/// [`TerminalSession::submit_prompt`] once the input is on the wire. Never
/// fails, never blocks: an atomic swap per write; on the one write per
/// bucket that opens a latch, a coord-id read, one `try_lock` grid read and
/// a non-blocking `try_send`.
///
/// A terminal with no coord session id yet has nothing to attribute the input
/// to — the same skip `operator_touch_watch` makes — and is counted
/// `unattributed` rather than silently vanishing.
pub fn on_input(session: &TerminalSession, caller: &PtyWriteCaller, now_unix_secs: i64) {
    let Some(episode) = session
        .operator_input_episodes()
        .observe(caller, now_unix_secs)
    else {
        return;
    };
    let Some(coord_session_id) = session.coord_session_id() else {
        COUNTERS.unattributed.fetch_add(1, Ordering::Relaxed);
        return;
    };
    let state = SessionStateAtInput::from_snapshot(session.try_grid_text());
    enqueue(Pending {
        coord_session_id,
        payload: input_payload(coord_session_id, episode, state),
    });
}

// ===========================================================================
// Off-path queue → durable outbox
// ===========================================================================

/// Bound on events waiting for the outbox thread. At ≤1 event per terminal
/// per channel per minute this only fills if the outbox append itself is
/// wedged, and then dropping is the right answer.
pub const QUEUE_CAPACITY: usize = 256;

struct Pending {
    coord_session_id: Uuid,
    payload: Value,
}

/// `None` = the outbox thread could not be spawned; every event then drops
/// and counts.
static QUEUE: OnceLock<Option<SyncSender<Pending>>> = OnceLock::new();

fn queue() -> Option<&'static SyncSender<Pending>> {
    QUEUE
        .get_or_init(|| {
            let (tx, rx) = sync_channel::<Pending>(QUEUE_CAPACITY);
            match std::thread::Builder::new()
                .name("operator-input-outbox".to_string())
                .spawn(move || {
                    for pending in rx {
                        append_to_outbox(pending);
                    }
                }) {
                Ok(_) => Some(tx),
                Err(e) => {
                    warn!(error = %e, "operator_input: outbox thread did not spawn — every input event will drop");
                    None
                }
            }
        })
        .as_ref()
}

fn enqueue(pending: Pending) {
    let Some(tx) = queue() else {
        COUNTERS.dropped.fetch_add(1, Ordering::Relaxed);
        return;
    };
    match tx.try_send(pending) {
        Ok(()) => {}
        Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {
            COUNTERS.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// On the outbox thread: append one event. Best-effort, same posture as
/// `operator_touch::emit` — a failure is counted and logged, never retried
/// here (the outbox drain owns retries once a row is durable).
fn append_to_outbox(pending: Pending) {
    use tauri::Manager;
    let registry = crate::tauri_app_handle::current().and_then(|app| {
        app.try_state::<Arc<crate::session::SessionRegistry>>()
            .map(|s| s.inner().clone())
    });
    let Some(registry) = registry else {
        COUNTERS.dropped.fetch_add(1, Ordering::Relaxed);
        warn!(
            coord_session = %pending.coord_session_id,
            "operator_input: no session registry — input event dropped"
        );
        return;
    };
    match registry.coord_sync().outbox().record(
        registry.machine_id(),
        pending.coord_session_id,
        crate::session::SessionEventKind::OperatorInput,
        pending.payload,
    ) {
        Ok(_) => {
            COUNTERS.emitted.fetch_add(1, Ordering::Relaxed);
        }
        Err(e) => {
            COUNTERS.dropped.fetch_add(1, Ordering::Relaxed);
            warn!(
                coord_session = %pending.coord_session_id,
                error = %e,
                "operator_input: outbox append failed — input event dropped"
            );
        }
    }
}

// ===========================================================================
// /health
// ===========================================================================

struct Counters {
    /// Latches opened, process-wide.
    latched: AtomicU64,
    /// Appended to the durable outbox.
    emitted: AtomicU64,
    /// Opened but lost: queue full, no outbox thread, no registry, or an
    /// outbox append error.
    dropped: AtomicU64,
    /// Opened on a terminal with no coord session id to attribute it to.
    unattributed: AtomicU64,
    /// Input writes seen (after the control-response exclusion), by class.
    seen_human: AtomicU64,
    seen_unknown: AtomicU64,
    seen_automated: AtomicU64,
}

static COUNTERS: Counters = Counters {
    latched: AtomicU64::new(0),
    emitted: AtomicU64::new(0),
    dropped: AtomicU64::new(0),
    unattributed: AtomicU64::new(0),
    seen_human: AtomicU64::new(0),
    seen_unknown: AtomicU64::new(0),
    seen_automated: AtomicU64::new(0),
};

/// The `/health` `operatorInput` block. `emitter: true` is the per-box
/// "this build carries the emitter" signal coord's coverage block reads;
/// `by_caller_class.automated` climbing while nothing is emitted for it is
/// the proof automated writers are seen and deliberately not recorded.
pub fn health_json() -> Value {
    let n = |c: &AtomicU64| c.load(Ordering::Relaxed);
    json!({
        "emitter": true,
        "emitted": n(&COUNTERS.emitted),
        "latched": n(&COUNTERS.latched),
        "dropped": n(&COUNTERS.dropped),
        "unattributed": n(&COUNTERS.unattributed),
        "by_caller_class": {
            "human": n(&COUNTERS.seen_human),
            "unknown": n(&COUNTERS.seen_unknown),
            "automated": n(&COUNTERS.seen_automated),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every [`PtyWriteCaller`] variant, with its expected actor class. The
    /// `match` below has no wildcard, so a new variant fails to compile HERE
    /// as well as in [`classify`] — the table cannot silently fall behind.
    fn every_caller_with_expected_class() -> Vec<(PtyWriteCaller, Option<ActorClass>)> {
        let callers = vec![
            PtyWriteCaller::HttpSubmitPrompt,
            PtyWriteCaller::LoopingAgentNudge,
            PtyWriteCaller::AccountMigration,
            PtyWriteCaller::SessionMessagePoller,
            PtyWriteCaller::AutoResponse {
                rule_id: "r1".to_string(),
            },
            PtyWriteCaller::WorkerSession,
            PtyWriteCaller::TauriTerminalWrite,
            PtyWriteCaller::TauriInvokeProxy,
            PtyWriteCaller::RemoteTerminalInput,
            PtyWriteCaller::HttpWrite,
            PtyWriteCaller::WebSocketInput,
            PtyWriteCaller::PtyTransport,
            PtyWriteCaller::ClaudeCliTransport,
            PtyWriteCaller::LaunchInitialCommand,
            PtyWriteCaller::HttpCreateInitialCommand,
            PtyWriteCaller::StewardLaunchCommand,
            PtyWriteCaller::GracefulExit,
            PtyWriteCaller::Test,
        ];
        callers
            .into_iter()
            .map(|c| {
                let expected = match &c {
                    PtyWriteCaller::TauriTerminalWrite | PtyWriteCaller::RemoteTerminalInput => {
                        Some(ActorClass::Human)
                    }
                    PtyWriteCaller::TauriInvokeProxy
                    | PtyWriteCaller::HttpWrite
                    | PtyWriteCaller::WebSocketInput
                    | PtyWriteCaller::HttpSubmitPrompt => Some(ActorClass::Unknown),
                    PtyWriteCaller::LoopingAgentNudge
                    | PtyWriteCaller::AccountMigration
                    | PtyWriteCaller::SessionMessagePoller
                    | PtyWriteCaller::AutoResponse { .. }
                    | PtyWriteCaller::WorkerSession
                    | PtyWriteCaller::PtyTransport
                    | PtyWriteCaller::ClaudeCliTransport
                    | PtyWriteCaller::LaunchInitialCommand
                    | PtyWriteCaller::HttpCreateInitialCommand
                    | PtyWriteCaller::StewardLaunchCommand
                    | PtyWriteCaller::GracefulExit
                    | PtyWriteCaller::Test => None,
                };
                (c, expected)
            })
            .collect()
    }

    #[test]
    fn exactly_two_doors_are_human_four_are_unknown_and_the_rest_emit_nothing() {
        let table = every_caller_with_expected_class();
        assert_eq!(table.len(), 18, "17 production variants + Test");
        for (caller, expected) in &table {
            assert_eq!(actor_class_of(caller), *expected, "{caller}");
        }
        let count = |want: Option<ActorClass>| {
            table
                .iter()
                .filter(|(c, _)| actor_class_of(c) == want)
                .count()
        };
        assert_eq!(count(Some(ActorClass::Human)), 2);
        assert_eq!(count(Some(ActorClass::Unknown)), 4);
        assert_eq!(count(None), 12);
    }

    #[test]
    fn channels_follow_the_door() {
        let ch = |c: PtyWriteCaller| classify(&c).map(|d| d.channel);
        assert_eq!(
            ch(PtyWriteCaller::TauriTerminalWrite),
            Some(Channel::LocalTerminal)
        );
        assert_eq!(
            ch(PtyWriteCaller::RemoteTerminalInput),
            Some(Channel::RemoteTerminal)
        );
        for c in [
            PtyWriteCaller::TauriInvokeProxy,
            PtyWriteCaller::HttpWrite,
            PtyWriteCaller::WebSocketInput,
            PtyWriteCaller::HttpSubmitPrompt,
        ] {
            assert_eq!(ch(c), Some(Channel::RunnerHttp));
        }
    }

    /// Every channel word is one coord's write boundary accepts.
    #[test]
    fn channel_and_class_words_are_in_coords_accepted_vocabularies() {
        const ACCEPTED_CHANNELS: [&str; 6] = [
            "local_terminal",
            "remote_terminal",
            "runner_http",
            "coord_answer",
            "coord_gate_clearance",
            "admin_edit",
        ];
        for c in [
            Channel::LocalTerminal,
            Channel::RemoteTerminal,
            Channel::RunnerHttp,
        ] {
            assert!(ACCEPTED_CHANNELS.contains(&c.as_str()), "{c:?}");
        }
        for a in [ActorClass::Human, ActorClass::Unknown] {
            assert!(["human", "unknown"].contains(&a.as_str()));
        }
    }

    #[test]
    fn five_hundred_writes_in_one_bucket_open_exactly_one_episode() {
        let latch = InputEpisodes::default();
        let bucket_start = 1_726_000_020;
        let opened = (0..500)
            .filter_map(|i| {
                latch.observe(&PtyWriteCaller::TauriTerminalWrite, bucket_start + (i % 60))
            })
            .count();
        assert_eq!(opened, 1);
        assert_eq!(latch.opened(), 1);
        // The next bucket re-arms.
        assert!(latch
            .observe(&PtyWriteCaller::TauriTerminalWrite, bucket_start + 60)
            .is_some());
    }

    #[test]
    fn the_latch_is_per_channel() {
        let latch = InputEpisodes::default();
        let t = 1_726_000_020;
        assert!(latch
            .observe(&PtyWriteCaller::TauriTerminalWrite, t)
            .is_some());
        assert!(latch.observe(&PtyWriteCaller::HttpWrite, t).is_some());
        assert!(latch
            .observe(&PtyWriteCaller::RemoteTerminalInput, t)
            .is_some());
        // Two ambiguous doors share `runner_http` — one episode between them.
        assert!(latch.observe(&PtyWriteCaller::WebSocketInput, t).is_none());
        assert_eq!(latch.opened(), 3);
    }

    #[test]
    fn automated_writes_never_open_a_latch() {
        let latch = InputEpisodes::default();
        for (caller, expected) in every_caller_with_expected_class() {
            if expected.is_none() {
                assert!(latch.observe(&caller, 1_726_000_020).is_none(), "{caller}");
            }
        }
        assert_eq!(latch.opened(), 0);
    }

    #[test]
    fn payload_carries_the_contract_fields_and_nothing_else() {
        let sid = Uuid::parse_str("11111111-1111-4111-8111-111111111111").unwrap();
        let episode = OpenedEpisode {
            door: Door {
                actor_class: ActorClass::Human,
                channel: Channel::LocalTerminal,
            },
            bucket: 1_726_000_020,
        };
        let body = input_payload(sid, episode, SessionStateAtInput::Working);
        assert_eq!(body["session_id"], sid.to_string());
        assert_eq!(body["channel"], "local_terminal");
        assert_eq!(body["actor_class"], "human");
        assert_eq!(body["session_state_at_input"], "working");
        assert_eq!(body["occurred_at"], "2024-09-10T20:27:00Z");
        assert_eq!(
            body["idempotency_key"],
            "11111111-1111-4111-8111-111111111111:input:local_terminal:1726000020"
        );
        let mut keys: Vec<&str> = body
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "actor_class",
                "channel",
                "idempotency_key",
                "occurred_at",
                "session_id",
                "session_state_at_input",
            ],
            "no tenant, no content, no byte count"
        );
    }

    #[test]
    fn state_is_unknown_without_a_snapshot_and_observed_with_one() {
        assert_eq!(
            SessionStateAtInput::from_snapshot(None),
            SessionStateAtInput::Unknown
        );
        let idle = vec!["some output".to_string(), "❯ ".to_string()];
        assert_eq!(
            SessionStateAtInput::from_snapshot(Some((idle, 1))),
            SessionStateAtInput::AtPrompt
        );
        let busy = vec![
            "✻ Thinking… (esc to interrupt)".to_string(),
            "❯ ".to_string(),
        ];
        assert_eq!(
            SessionStateAtInput::from_snapshot(Some((busy, 1))),
            SessionStateAtInput::Working
        );
    }

    #[test]
    fn health_block_carries_the_declared_keys() {
        let h = health_json();
        assert_eq!(h["emitter"], true);
        for k in ["emitted", "latched", "dropped", "unattributed"] {
            assert!(h[k].is_u64(), "{k}");
        }
        for k in ["human", "unknown", "automated"] {
            assert!(h["by_caller_class"][k].is_u64(), "{k}");
        }
    }

    /// Privacy pin (plan Phase 2 acceptance): no function in this module's
    /// production code takes the written bytes. Scans every `fn` signature
    /// above the test module for a byte-slice or a `data` parameter.
    #[test]
    fn no_function_here_takes_the_written_bytes() {
        let src = include_str!("operator_input.rs");
        let production = src
            .split_once("#[cfg(test)]\nmod tests")
            .expect("test module marker")
            .0;
        let mut signatures = 0;
        let mut lines = production.lines();
        while let Some(line) = lines.next() {
            let t = line.trim_start();
            let is_fn = ["fn ", "pub fn ", "pub(crate) fn ", "pub(super) fn "]
                .iter()
                .any(|p| t.starts_with(p));
            if !is_fn {
                continue;
            }
            let mut sig = t.to_string();
            while !sig.contains('{') && !sig.contains(';') {
                match lines.next() {
                    Some(next) => sig.push_str(next.trim()),
                    None => break,
                }
            }
            let params = sig
                .split_once('(')
                .and_then(|(_, rest)| rest.split_once(')'))
                .map(|(p, _)| p.to_string())
                .unwrap_or_default();
            signatures += 1;
            assert!(!params.contains("u8"), "a fn takes bytes: {sig}");
            assert!(
                !params.contains("data"),
                "a fn takes a data parameter: {sig}"
            );
            assert!(!params.contains("str"), "a fn takes text: {sig}");
        }
        assert!(
            signatures >= 8,
            "the scan found the module's functions ({signatures})"
        );
    }
}
