//! The SDK message queue of a `ClaudeSession` — messages accepted while the
//! session was `Processing`, written to stdin when its turn ends — and the ONE
//! place that decides, under the queue lock, whether a message is written now
//! or queued.
//!
//! Each entry carries the door it came from ([`SdkMessageCaller`]) and the
//! session's barrier ids, because the turn-end drain is a WAKE in its own
//! right: a message queued before a `runner-restart` quiet barrier opened
//! would otherwise start a new turn after the barrier was up (plan
//! `2026-09-29-quiet-on-demand-…`, D4). So the drain re-runs the quiet-barrier
//! gate: an `Autonomous` entry the gate defers STAYS QUEUED (deferred, not
//! dropped), while `Operator` / session-internal entries drain normally — an
//! operator's message may pass a held autonomous one, by design.
//!
//! # One lock, one order
//!
//! Four paths write a turn: [`submit`] (`send_user_message`),
//! [`submit_initial`] (`send_initial_prompt`), and [`drain_one`] from the
//! stdout reader at turn end and from the heartbeat's retry of a barrier-held
//! entry. Every one of them takes the queue lock FIRST, then reads the state,
//! and holds the lock through the write and the `Ready → Processing`
//! transition. So exactly one of them can find the session
//! `Ready` and write — never two (no double send) — and a new message submitted
//! while anything is queued goes BEHIND it and the head drains instead (FIFO
//! among autonomous messages).
//!
//! ⚠ The stdin write runs under the queue lock. If the CLI stopped reading
//! stdin and the pipe filled, the write would block, and every other submit or
//! drain on this session waits behind it. Accepted: the alternative (writing
//! outside the lock) is exactly the double-send this lock exists to prevent.
//!
//! ⚠ **Acked, then lost.** The session-bus poller acks a message to coord as
//! soon as `send_user_message` returns `Ok` — including when it was QUEUED. If
//! the barrier then holds it at turn end and the session closes, exits or is
//! promoted before release, [`discard_all`] drops it while coord records it
//! delivered. A held message can now wait in a `Ready` session for as long as
//! the barrier (up to 1 h), which widens that window; every such drop is logged
//! at `warn!` with its caller and session.
//!
//! A queued autonomous message lives only in this process's memory, so a
//! restart drops it: while any is queued the session INSTANCE is registered in
//! [`crate::quiet_barrier::pending`] under [`PENDING_KEY`], which the `resume`
//! classification reports as a `pending_autonomous_prompt` straggler.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use tracing::{debug, warn};

use crate::quiet_barrier::{SdkMessageCaller, WakeClass};

/// The [`crate::quiet_barrier::pending`] key a session's queued autonomous
/// messages register under.
pub const PENDING_KEY: &str = "sdk_queue";

static NEXT_SEQ: AtomicU64 = AtomicU64::new(1);
static NEXT_INSTANCE: AtomicU64 = AtomicU64::new(1);

/// The registry key of ONE session instance: the session id plus a
/// per-process instance number. A promoted or rate-limit-restarted session
/// reuses its session id; keying by instance means a late-dropped OLD session
/// can neither clear nor be reported against the NEW one's entry.
pub fn new_instance_key(session_id: &str) -> String {
    format!(
        "{session_id}#{}",
        NEXT_INSTANCE.fetch_add(1, Ordering::Relaxed)
    )
}

/// One message waiting in (or passing through) a session's SDK queue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuedMessage {
    /// Submission order, unique per process (identity for "was it mine").
    pub seq: u64,
    pub text: String,
    /// The door it came from — re-checked against the barrier at drain time.
    pub caller: SdkMessageCaller,
    /// The session's id (the deferral string names it).
    pub session_id: String,
    /// The ids the requester exemption matches (session id + pinned Claude id).
    pub target_ids: Vec<String>,
    /// The session INSTANCE's pending-registry key ([`new_instance_key`]).
    pub registry_key: String,
}

impl QueuedMessage {
    pub fn new(
        text: &str,
        caller: SdkMessageCaller,
        session_id: &str,
        target_ids: Vec<String>,
        registry_key: &str,
    ) -> Self {
        Self {
            seq: NEXT_SEQ.fetch_add(1, Ordering::Relaxed),
            text: text.to_string(),
            caller,
            session_id: session_id.to_string(),
            target_ids,
            registry_key: registry_key.to_string(),
        }
    }

    fn is_autonomous(&self) -> bool {
        self.caller.wake_class() == WakeClass::Autonomous
    }
}

/// Remove and return the next message that may go out now: the FIRST entry
/// that is not `Autonomous`, or whose `gate` answers `Ok`. A deferred
/// autonomous entry stays where it is, so an operator's message queued behind
/// it is not held up by it, and it drains in order once the barrier lifts.
pub fn take_next_sendable(
    queue: &mut VecDeque<QueuedMessage>,
    gate: &dyn Fn(&QueuedMessage) -> Result<(), String>,
) -> Option<QueuedMessage> {
    let idx = queue
        .iter()
        .position(|m| !m.is_autonomous() || gate(m).is_ok())?;
    queue.remove(idx)
}

/// The live gate for a queued entry: the same
/// [`crate::quiet_barrier::sdk_wake_gate`] `send_user_message` runs.
pub fn live_gate(m: &QueuedMessage) -> Result<(), String> {
    crate::quiet_barrier::sdk_wake_gate(m.caller, &m.session_id, &m.target_ids)
}

/// Keep the pending-prompt registry in step with `queue` for the instance
/// `registry_key`: registered while any autonomous message is queued, cleared
/// once none is.
pub fn sync_pending(registry_key: &str, queue: &VecDeque<QueuedMessage>) {
    let autonomous = queue.iter().filter(|m| m.is_autonomous()).count();
    if autonomous == 0 {
        crate::quiet_barrier::pending::clear(registry_key, PENDING_KEY);
    } else {
        crate::quiet_barrier::pending::mark(
            registry_key,
            PENDING_KEY,
            &format!("{autonomous} autonomous SDK message(s) queued in memory"),
        );
    }
}

/// What [`submit`] did with the submitted message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Submitted {
    /// It was written as a new turn.
    Now,
    /// It is queued (behind earlier messages, or the session is mid-turn).
    Queued,
}

/// The session-side operations [`submit`] and [`drain_one`] perform UNDER the
/// queue lock. Production backs them with the session's state tracker and
/// stdin writer; tests with a recorder.
pub trait TurnSink {
    /// Can a message be accepted at all (not closed / not starting)?
    fn can_accept(&self) -> Result<(), String>;
    /// Is the session `Ready` for a new turn right now?
    fn is_ready(&self) -> bool;
    /// Write `m` as a new turn and move `Ready → Processing`.
    fn write_turn(&self, m: &QueuedMessage) -> Result<(), String>;
}

/// Submit `msg` (already past the entry gate). Under the queue lock:
/// - not `Ready` → queued (bounded by `max_len`);
/// - `Ready` with an empty queue → written now;
/// - `Ready` with anything queued → appended behind it, and the first
///   sendable entry is written instead (which may be this one — an operator
///   message passing held autonomous entries — or an earlier one).
pub fn submit(
    queue: &Mutex<VecDeque<QueuedMessage>>,
    msg: QueuedMessage,
    max_len: usize,
    sink: &dyn TurnSink,
    gate: &dyn Fn(&QueuedMessage) -> Result<(), String>,
) -> Result<Submitted, String> {
    let mut q = queue
        .lock()
        .map_err(|e| format!("Failed to lock message queue: {e}"))?;
    sink.can_accept()?;
    if sink.is_ready() && q.is_empty() {
        sink.write_turn(&msg)?;
        return Ok(Submitted::Now);
    }
    let mine = msg.seq;
    let key = msg.registry_key.clone();
    q.push_back(msg);
    let outcome = if sink.is_ready() {
        match take_next_sendable(&mut q, gate) {
            Some(next) => {
                let was_mine = next.seq == mine;
                match sink.write_turn(&next) {
                    Ok(()) if was_mine => Ok(Submitted::Now),
                    Ok(()) => Ok(Submitted::Queued),
                    Err(e) if was_mine => Err(e),
                    Err(e) => {
                        // An EARLIER message failed to write: dropped, exactly
                        // as the turn-end drain drops one; ours stays queued.
                        warn!("Failed to send queued user message: {e}");
                        Ok(Submitted::Queued)
                    }
                }
            }
            None => Ok(Submitted::Queued),
        }
    } else {
        Ok(Submitted::Queued)
    };
    // The cap applies only to a message that would STAY queued: an operator
    // message on a `Ready` session whose queue is full of barrier-held entries
    // is written above, never refused (D4: an operator path is never held).
    if q.len() > max_len {
        if let Some(pos) = q.iter().position(|m| m.seq == mine) {
            q.remove(pos);
            sync_pending(&key, &q);
            return Err(format!(
                "Message queue full ({max_len} messages). Wait for the current turn to complete."
            ));
        }
    }
    sync_pending(&key, &q);
    outcome
}

/// Write a session's INITIAL prompt — under the queue lock, in the same order
/// as [`submit`] — or refuse without writing. Every caller sends it BEFORE
/// registering the session, so nothing else can reach the session yet and
/// the refusal should never fire; it stays as a defence. If another message
/// already started a turn before the initial prompt, writing the prompt as
/// well would put two user turns in flight. So it is written only when the
/// session is `Ready` AND nothing is queued, else an error naming the race.
pub fn submit_initial(
    queue: &Mutex<VecDeque<QueuedMessage>>,
    msg: QueuedMessage,
    sink: &dyn TurnSink,
) -> Result<(), String> {
    let q = queue
        .lock()
        .map_err(|e| format!("Failed to lock message queue: {e}"))?;
    if !sink.is_ready() {
        return Err(
            "Cannot send initial prompt: the session is not Ready — another message already \
             started a turn before the initial prompt (initial-prompt race); the prompt was NOT \
             written"
                .to_string(),
        );
    }
    if !q.is_empty() {
        return Err(format!(
            "Cannot send initial prompt: {} message(s) are already queued for the session \
             (initial-prompt race); the prompt was NOT written",
            q.len()
        ));
    }
    sink.write_turn(&msg)
}

/// Drain one message if the session is `Ready`, under the queue lock. `None`
/// when nothing was written (not ready, empty, or every entry held); otherwise
/// the write's result. A failed write drops that message (logged by caller).
pub fn drain_one(
    queue: &Mutex<VecDeque<QueuedMessage>>,
    sink: &dyn TurnSink,
    gate: &dyn Fn(&QueuedMessage) -> Result<(), String>,
) -> Option<(QueuedMessage, Result<(), String>)> {
    let mut q = queue.lock().ok()?;
    if !sink.is_ready() || q.is_empty() {
        return None;
    }
    let next = take_next_sendable(&mut q, gate);
    let key = next
        .as_ref()
        .or(q.front())
        .map(|m| m.registry_key.clone());
    let out = match next {
        Some(m) => {
            let r = sink.write_turn(&m);
            Some((m, r))
        }
        None => {
            if let Some(held) = q.front() {
                debug!(
                    "{} queued message(s) held by the quiet barrier for session {}",
                    q.len(),
                    held.session_id
                );
            }
            None
        }
    };
    if let Some(key) = key {
        sync_pending(&key, &q);
    }
    out
}

/// Drop everything queued for a session instance that is closing or whose
/// process exited, and its registry entry — under the queue lock, so a drain
/// already running finishes first and none can re-register it afterwards.
///
/// Every dropped message is logged at `warn!` with its caller and session: an
/// autonomous one may already have been acked to coord (module docs, "Acked,
/// then lost").
pub fn discard_all(queue: &Mutex<VecDeque<QueuedMessage>>, registry_key: &str) {
    let mut q = queue.lock().unwrap_or_else(|e| e.into_inner());
    for m in q.drain(..) {
        warn!(
            caller = m.caller.tag(),
            session = %m.session_id,
            chars = m.text.len(),
            "SDK queue: dropping an undelivered queued message — the session is closing or \
             its process exited"
        );
    }
    crate::quiet_barrier::pending::clear(registry_key, PENDING_KEY);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    fn msg(text: &str, caller: SdkMessageCaller) -> QueuedMessage {
        QueuedMessage::new(text, caller, "sdk-sess", vec!["sdk-sess".to_string()], "sdk-sess#t")
    }

    /// A recorder sink: `Ready` until a turn is written, then `Processing`
    /// until `end_turn`. A write while not `Ready` is a double send — it is
    /// RECORDED (never panicked under the queue lock, which would poison it
    /// and turn a failure into a hang) and asserted by each test.
    #[derive(Default)]
    struct Recorder {
        processing: AtomicBool,
        double_send: AtomicBool,
        turns_started: std::sync::atomic::AtomicU64,
        written: Mutex<Vec<String>>,
    }

    impl Recorder {
        fn end_turn(&self) {
            self.processing.store(false, Ordering::SeqCst);
        }
        fn written(&self) -> Vec<String> {
            self.written.lock().unwrap().clone()
        }
        fn assert_no_double_send(&self) {
            assert!(
                !self.double_send.load(Ordering::SeqCst),
                "a turn was written while another was in progress"
            );
        }
    }

    impl TurnSink for Recorder {
        fn can_accept(&self) -> Result<(), String> {
            Ok(())
        }
        fn is_ready(&self) -> bool {
            !self.processing.load(Ordering::SeqCst)
        }
        fn write_turn(&self, m: &QueuedMessage) -> Result<(), String> {
            if self.processing.swap(true, Ordering::SeqCst) {
                self.double_send.store(true, Ordering::SeqCst);
            }
            self.written.lock().unwrap().push(m.text.clone());
            self.turns_started.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    fn defer(_: &QueuedMessage) -> Result<(), String> {
        Err("QUIET_BARRIER_DEFERRED: barrier=rr-1 x".to_string())
    }
    fn allow(_: &QueuedMessage) -> Result<(), String> {
        Ok(())
    }

    /// W2: under a deferring gate an autonomous entry stays queued, an
    /// operator entry behind it drains, and after release the autonomous one
    /// drains in order.
    #[test]
    fn a_deferred_autonomous_entry_stays_queued_and_operator_entries_drain() {
        let mut q: VecDeque<QueuedMessage> = VecDeque::new();
        q.push_back(msg("poller", SdkMessageCaller::SessionMessagePoller));
        q.push_back(msg("typed", SdkMessageCaller::TauriAiSessionMessage));
        q.push_back(msg("reprompt", SdkMessageCaller::ConductorReprompt));

        let first = take_next_sendable(&mut q, &defer).expect("the operator entry drains");
        assert_eq!(first.text, "typed");
        assert!(take_next_sendable(&mut q, &defer).is_none(), "autonomous entries are held");
        assert_eq!(q.len(), 2, "held, not dropped");

        assert_eq!(take_next_sendable(&mut q, &allow).unwrap().text, "poller");
        assert_eq!(take_next_sendable(&mut q, &allow).unwrap().text, "reprompt");
        assert!(q.is_empty());
    }

    /// Round-3 W1, deterministic interleave: a held autonomous message A sits
    /// in the queue of a `Ready` session (the barrier held it at turn end).
    /// The barrier lifts and a NEW autonomous message B is submitted before
    /// the heartbeat's drain runs: B goes BEHIND A and A is written (FIFO
    /// among autonomous) — B is not written directly. The heartbeat drain then
    /// finds the session mid-turn and writes nothing (no double send); the
    /// next turn end drains B.
    #[test]
    fn a_direct_send_queues_behind_a_held_message_and_never_double_sends() {
        let queue: Mutex<VecDeque<QueuedMessage>> = Mutex::new(VecDeque::new());
        let sink = Recorder::default();
        // A arrives mid-turn and is queued.
        sink.processing.store(true, Ordering::SeqCst);
        let a = msg("A", SdkMessageCaller::SessionMessagePoller);
        assert_eq!(submit(&queue, a, 10, &sink, &allow), Ok(Submitted::Queued));
        // Turn ends under the barrier: the drain holds A.
        sink.end_turn();
        assert!(drain_one(&queue, &sink, &defer).is_none());
        assert!(sink.written().is_empty());
        // Released. B is submitted before the heartbeat retries.
        let b = msg("B", SdkMessageCaller::ConductorReprompt);
        assert_eq!(submit(&queue, b, 10, &sink, &allow), Ok(Submitted::Queued));
        assert_eq!(sink.written(), vec!["A"], "the head went, not B");
        // The heartbeat's retry now: mid-turn, nothing written.
        assert!(drain_one(&queue, &sink, &allow).is_none());
        // Turn end drains B.
        sink.end_turn();
        let (m, r) = drain_one(&queue, &sink, &allow).expect("B drains");
        assert_eq!((m.text.as_str(), r), ("B", Ok(())));
        assert_eq!(sink.written(), vec!["A", "B"]);
        assert!(queue.lock().unwrap().is_empty());
        sink.assert_no_double_send();
    }

    /// An operator message submitted while an autonomous one is held passes
    /// it (by design) and is written now; with nothing queued, a submit on a
    /// `Ready` session writes directly.
    #[test]
    fn an_operator_submit_passes_a_held_entry_and_an_empty_queue_writes_directly() {
        let queue: Mutex<VecDeque<QueuedMessage>> = Mutex::new(VecDeque::new());
        let sink = Recorder::default();
        assert_eq!(
            submit(&queue, msg("first", SdkMessageCaller::HttpSessionMessage), 10, &sink, &allow),
            Ok(Submitted::Now)
        );
        let held = msg("held", SdkMessageCaller::SessionMessagePoller);
        assert_eq!(submit(&queue, held, 10, &sink, &defer), Ok(Submitted::Queued));
        sink.end_turn();
        let typed = msg("typed", SdkMessageCaller::TauriAiSessionMessage);
        assert_eq!(submit(&queue, typed, 10, &sink, &defer), Ok(Submitted::Now));
        assert_eq!(sink.written(), vec!["first", "typed"]);
        assert_eq!(queue.lock().unwrap().len(), 1, "the held entry is still queued");
        sink.assert_no_double_send();
    }

    /// W2-r4: a `Ready` session whose queue is FULL of barrier-held autonomous
    /// messages still writes an operator's message now (D4: an operator path
    /// is never held); a further autonomous message is refused as queue-full
    /// and is not left in the queue.
    #[test]
    fn a_full_queue_of_held_messages_never_refuses_an_operator_submit() {
        let queue: Mutex<VecDeque<QueuedMessage>> = Mutex::new(VecDeque::new());
        let sink = Recorder::default();
        sink.processing.store(true, Ordering::SeqCst);
        for i in 0..10 {
            let m = msg(&format!("held-{i}"), SdkMessageCaller::SessionMessagePoller);
            assert_eq!(submit(&queue, m, 10, &sink, &defer), Ok(Submitted::Queued));
        }
        sink.end_turn();
        assert!(drain_one(&queue, &sink, &defer).is_none(), "all 10 held");
        let typed = msg("typed", SdkMessageCaller::TauriAiSessionMessage);
        assert_eq!(submit(&queue, typed, 10, &sink, &defer), Ok(Submitted::Now));
        assert_eq!(sink.written(), vec!["typed"]);
        sink.end_turn();
        let extra = msg("extra", SdkMessageCaller::ConductorReprompt);
        let err = submit(&queue, extra, 10, &sink, &defer).expect_err("queue full");
        assert!(err.contains("queue full"), "{err}");
        let q = queue.lock().unwrap();
        assert_eq!(q.len(), 10);
        assert!(q.iter().all(|m| m.text != "extra"), "the refused message is not queued");
        sink.assert_no_double_send();
    }

    /// W1-r4: the initial prompt is written only on a `Ready` session with an
    /// empty queue, under the queue lock; if another message started a turn
    /// first (or is queued), it is refused naming the race and NOT written.
    #[test]
    fn the_initial_prompt_is_written_only_when_ready_and_nothing_is_queued() {
        let first = |t: &str| msg(t, SdkMessageCaller::ExecutorFirstMessage);
        let queue: Mutex<VecDeque<QueuedMessage>> = Mutex::new(VecDeque::new());
        let sink = Recorder::default();
        assert_eq!(submit_initial(&queue, first("brief"), &sink), Ok(()));
        assert_eq!(sink.written(), vec!["brief"]);

        // Another message won the race: mid-turn → refused, nothing written.
        let err = submit_initial(&queue, first("brief-2"), &sink).expect_err("not ready");
        assert!(err.contains("initial-prompt race") && err.contains("NOT written"), "{err}");

        // Ready but something is queued → refused, queue untouched.
        let queued = msg("poller", SdkMessageCaller::SessionMessagePoller);
        assert_eq!(submit(&queue, queued, 10, &sink, &allow), Ok(Submitted::Queued));
        sink.end_turn();
        let err = submit_initial(&queue, first("brief-3"), &sink).expect_err("queued");
        assert!(err.contains("already queued"), "{err}");
        assert_eq!(sink.written(), vec!["brief"]);
        assert_eq!(queue.lock().unwrap().len(), 1);
        sink.assert_no_double_send();
    }

    /// Round-5 W1: the ORDER is what makes the initial prompt unrefusable.
    /// Prompt-before-register: the brief is written while nothing can reach the
    /// session; a peer that reaches it the instant it is registered queues
    /// behind the brief — nothing is refused. Register-before-prompt (the old
    /// order): a peer that wins the race starts a turn first and the brief is
    /// refused, leaving a registered session with no brief.
    #[test]
    fn prompt_before_register_cannot_be_refused_by_a_race() {
        let first = |t: &str| msg(t, SdkMessageCaller::ExecutorFirstMessage);
        let peer = || msg("peer", SdkMessageCaller::SessionMessagePoller);

        // New order: prompt, then register (the peer arrives right after).
        let queue: Mutex<VecDeque<QueuedMessage>> = Mutex::new(VecDeque::new());
        let sink = Recorder::default();
        assert_eq!(submit_initial(&queue, first("brief"), &sink), Ok(()));
        // — registered here; a peer reaches it immediately —
        assert_eq!(submit(&queue, peer(), 10, &sink, &allow), Ok(Submitted::Queued));
        sink.end_turn();
        assert!(drain_one(&queue, &sink, &allow).is_some());
        assert_eq!(sink.written(), vec!["brief", "peer"]);
        sink.assert_no_double_send();

        // Old order: registered first, the peer wins, the brief is refused.
        let queue: Mutex<VecDeque<QueuedMessage>> = Mutex::new(VecDeque::new());
        let sink = Recorder::default();
        assert_eq!(submit(&queue, peer(), 10, &sink, &allow), Ok(Submitted::Now));
        assert!(submit_initial(&queue, first("brief"), &sink).is_err());
        assert_eq!(sink.written(), vec!["peer"], "the brief never went out");
    }

    /// Concurrent stress: a submitter and two drainers on one queue never
    /// write while a turn is in progress, and autonomous messages are written
    /// in submission order. The fake CLI ends a turn only after one STARTED
    /// (so `processing` is really held between turns and a second writer
    /// reading `Ready` would be caught), double sends are recorded rather than
    /// panicked under the lock, and every wait is bounded by one deadline — a
    /// regression fails the test instead of hanging it.
    #[test]
    fn concurrent_submits_and_drains_never_double_send_and_keep_fifo() {
        use std::sync::atomic::AtomicU64;
        const N: usize = 200;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        let queue: Arc<Mutex<VecDeque<QueuedMessage>>> = Arc::new(Mutex::new(VecDeque::new()));
        let sink = Arc::new(Recorder::default());
        let stop = Arc::new(AtomicBool::new(false));
        let cli = {
            let (sink, stop) = (sink.clone(), stop.clone());
            std::thread::spawn(move || {
                let ended = AtomicU64::new(0);
                while !stop.load(Ordering::SeqCst) && std::time::Instant::now() < deadline {
                    let started = sink.turns_started.load(Ordering::SeqCst);
                    let done = ended.load(Ordering::SeqCst);
                    if started > done
                        && ended
                            .compare_exchange(done, done + 1, Ordering::SeqCst, Ordering::SeqCst)
                            .is_ok()
                    {
                        sink.end_turn();
                    }
                    std::thread::yield_now();
                }
            })
        };
        let drainers: Vec<_> = (0..2)
            .map(|_| {
                let (queue, sink, stop) = (queue.clone(), sink.clone(), stop.clone());
                std::thread::spawn(move || {
                    while !stop.load(Ordering::SeqCst) && std::time::Instant::now() < deadline {
                        let _ = drain_one(&queue, sink.as_ref(), &allow);
                        std::thread::yield_now();
                    }
                })
            })
            .collect();
        for i in 0..N {
            let m = msg(&format!("{i:03}"), SdkMessageCaller::SessionMessagePoller);
            loop {
                match submit(&queue, m.clone(), 1_000, sink.as_ref(), &allow) {
                    Ok(_) => break,
                    // Retry ONLY on queue-full; any other error is a failure.
                    Err(e) if e.contains("queue full") => {
                        assert!(std::time::Instant::now() < deadline, "submit never got room");
                        std::thread::yield_now();
                    }
                    Err(e) => panic!("submit failed: {e}"),
                }
            }
        }
        while !queue.lock().map(|q| q.is_empty()).unwrap_or(true)
            && std::time::Instant::now() < deadline
        {
            std::thread::yield_now();
        }
        stop.store(true, Ordering::SeqCst);
        cli.join().unwrap();
        for d in drainers {
            d.join().unwrap();
        }
        assert!(!queue.is_poisoned(), "a drainer panicked under the queue lock");
        sink.assert_no_double_send();
        let written = sink.written();
        assert_eq!(written.len(), N, "everything was written before the deadline");
        let mut sorted = written.clone();
        sorted.sort();
        assert_eq!(written, sorted, "autonomous messages are written FIFO");
    }

    /// The live gate holds a queued autonomous entry under an open barrier and
    /// lets it out once the barrier is absent.
    #[test]
    fn the_live_gate_defers_a_queued_autonomous_entry_under_a_barrier() {
        use crate::quiet_barrier::{parse_record, with_test_state, BarrierState, FileRecord};
        let FileRecord::Recorded(b) = parse_record(
            include_bytes!("../quiet_barrier/fixtures/open-runner-restart.json"),
            None,
        ) else {
            panic!("fixture must parse");
        };
        let mut q: VecDeque<QueuedMessage> =
            [msg("poller", SdkMessageCaller::SessionMessagePoller)].into();
        let held = with_test_state(BarrierState::Open(b), || {
            take_next_sendable(&mut q, &live_gate)
        });
        assert!(held.is_none());
        assert_eq!(take_next_sendable(&mut q, &live_gate).unwrap().text, "poller");
    }

    /// The registry tracks queued AUTONOMOUS entries only, per instance key.
    #[test]
    fn sync_pending_registers_only_while_an_autonomous_entry_is_queued() {
        let key = "sdk-sync-pending-test#1";
        let mut q: VecDeque<QueuedMessage> =
            [msg("typed", SdkMessageCaller::TauriAiSessionMessage)].into();
        sync_pending(key, &q);
        assert!(crate::quiet_barrier::pending::for_session(key).is_empty());
        q.push_back(msg("poller", SdkMessageCaller::SessionMessagePoller));
        sync_pending(key, &q);
        let p = crate::quiet_barrier::pending::for_session(key);
        assert_eq!(p.len(), 1);
        assert!(p[0].starts_with("sdk_queue (1 autonomous"), "{p:?}");
        q.pop_back();
        sync_pending(key, &q);
        assert!(crate::quiet_barrier::pending::for_session(key).is_empty());
    }

    /// Note 2: instances of the same session id have distinct keys, so the
    /// OLD instance discarding its queue (a late drop after a restart reused
    /// the id) cannot clear the NEW instance's entry.
    #[test]
    fn a_late_old_instance_cannot_clear_the_new_instances_entry() {
        let old_key = new_instance_key("tr-reused");
        let new_key = new_instance_key("tr-reused");
        assert_ne!(old_key, new_key);
        assert!(old_key.starts_with("tr-reused#") && new_key.starts_with("tr-reused#"));
        let new_q: VecDeque<QueuedMessage> = [QueuedMessage::new(
            "poller",
            SdkMessageCaller::SessionMessagePoller,
            "tr-reused",
            vec![],
            &new_key,
        )]
        .into();
        sync_pending(&new_key, &new_q);
        let old_queue: Mutex<VecDeque<QueuedMessage>> = Mutex::new(VecDeque::new());
        discard_all(&old_queue, &old_key);
        assert_eq!(crate::quiet_barrier::pending::for_session(&new_key).len(), 1);
        let new_queue = Mutex::new(new_q);
        discard_all(&new_queue, &new_key);
        assert!(crate::quiet_barrier::pending::for_session(&new_key).is_empty());
        assert!(new_queue.lock().unwrap().is_empty());
    }
}
